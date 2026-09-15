//! Asking the receiver before a byte is written.
//!
//! The receiver has opened the sealed metadata, so it knows the name, the type
//! and the size of what is on offer, and where it would land. Nothing has been
//! created on disk. This module shows that and gets a yes or a no, while the
//! transfer waits.
//!
//! Two things make that harder than printing a question:
//!
//! - **The connection has to keep being read while a person thinks.** A relay
//!   pings its sockets and drops one that never answers, and tungstenite only
//!   answers a ping while the socket is being read. So the question races a
//!   read of the transport, and a read can be abandoned when the person
//!   answers. That is why [`crate::transport::Transport::receive`] must be
//!   cancel-safe. Reading also lets a sender's `cancel` end the question.
//! - **A person may never answer.** The question has a deadline, and stdin is
//!   read on a thread of its own rather than on the runtime's blocking pool: a
//!   runtime shuts down by waiting for its blocking tasks, so an unanswered
//!   `read_line` there would hold the process open after the transfer had
//!   already been declined.
//!
//! See `docs/plans/receiver-consent-and-status-plan-2026-09-14.md`.

use std::{error::Error, future::Future, io::IsTerminal, time::Duration};

use serde_json::json;

use crate::{
    display,
    transport::{Frame, Transport},
};

/// How long the receiver's question stays open.
///
/// Long enough to read a preview and think, short enough that an abandoned
/// prompt does not hold a relay session. It must stay under the relay's
/// five-minute session lifetime, and under the sender's own wait for an
/// answer, `send::CONSENT_TIMEOUT`, or the sender gives up on a receiver that
/// is still deciding. `the_three_clocks_are_in_order` holds them in order.
pub const ACCEPT_DEADLINE: Duration = Duration::from_secs(120);

/// Whether a receiver asks before accepting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Acceptance {
    /// Show the preview and ask. Needs a terminal on stdin.
    Ask,
    /// Accept whatever is offered without asking: `--yes`.
    Yes,
}

impl Acceptance {
    /// Refuses to start a receive that would need to ask and cannot.
    ///
    /// Called before anything is contacted. Discovering it after the key
    /// exchange would burn a code the sender cannot use again, for a question
    /// that was never going to be answerable.
    pub fn check_answerable(self) -> Result<(), Box<dyn Error + Send + Sync>> {
        if self == Self::Ask && !std::io::stdin().is_terminal() {
            return Err(
                "there is no terminal to ask whether to accept this transfer. \
                        Pass --yes to accept whatever the sender sends without asking"
                    .into(),
            );
        }

        Ok(())
    }
}

/// What the receiver is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    /// The name as the sender sent it. Rendered only through [`display`].
    pub name: String,
    pub size: u64,
    pub landing: Landing,
    /// The sender's claim for a folder, and only a claim: the extractor's
    /// limits still bound what is actually written.
    pub entry_count: Option<u64>,
    pub unpacked_size: Option<u64>,
}

/// Where accepting would put it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Landing {
    /// A single file at this path.
    File {
        path: String,
        /// The name the sender asked for, when it had to change: taken
        /// already, or rewritten for this platform.
        instead_of: Option<String>,
        replacing: bool,
    },
    /// An archive unpacked into this directory.
    Folder { into: String },
}

/// The receiver's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Consent {
    Accept,
    Decline,
}

/// How a receive ends without the transfer happening, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Accepted,
    Declined,
    TimedOut,
}

/// Asks for a decision about one preview.
///
/// A trait so the policy can be tested without a terminal, for the same reason
/// [`crate::send::AnotherAttempt`] is one.
pub trait ConsentPrompt {
    fn decide(&mut self, preview: &Preview) -> impl Future<Output = Consent> + Send;
}

/// Asks the person at the terminal.
pub struct AskTheReceiver;

impl ConsentPrompt for AskTheReceiver {
    async fn decide(&mut self, preview: &Preview) -> Consent {
        eprint!("{}", render(preview));
        eprint!("Accept? [y/N] ");

        let (answer_tx, answer_rx) = tokio::sync::oneshot::channel();

        // A thread of its own, not `spawn_blocking`: see the module comment.
        // If nobody answers, this thread is simply left behind when the
        // process exits.
        std::thread::spawn(move || {
            let mut line = String::new();
            let answered = std::io::stdin().read_line(&mut line).map(|_| line);
            let _ = answer_tx.send(answered);
        });

        match answer_rx.await {
            Ok(Ok(line)) if matches!(line.trim(), "y" | "Y" | "yes" | "Yes" | "YES") => {
                Consent::Accept
            }
            // Anything else, including an unreadable stdin, is not a yes.
            _ => Consent::Decline,
        }
    }
}

/// Accepts without asking, and says so.
pub struct AcceptWithoutAsking;

impl ConsentPrompt for AcceptWithoutAsking {
    async fn decide(&mut self, preview: &Preview) -> Consent {
        eprint!("{}", render(preview));
        eprintln!("Accepting without asking (--yes).");
        Consent::Accept
    }
}

/// Shows the preview and waits for an answer, while still reading the
/// connection.
///
/// Returns once the receiver has decided or the deadline passed. The decision
/// has not been sent to the sender yet; the caller does that, because what to
/// send depends on what it then manages to create.
///
/// While the question is open:
/// - the relay's own narration is read past;
/// - a sender's `cancel` or `error` ends the receive with the sender's reason;
/// - a chunk is a sender streaming before anyone agreed, so the receiver
///   cancels and stops.
pub async fn ask<T, P>(
    transport: &mut T,
    prompt: &mut P,
    preview: &Preview,
    deadline: Duration,
) -> Result<Outcome, Box<dyn Error + Send + Sync>>
where
    T: Transport,
    P: ConsentPrompt,
{
    let decision = prompt.decide(preview);
    tokio::pin!(decision);

    let expiry = tokio::time::sleep(deadline);
    tokio::pin!(expiry);

    loop {
        tokio::select! {
            consent = &mut decision => {
                return Ok(match consent {
                    Consent::Accept => Outcome::Accepted,
                    Consent::Decline => Outcome::Declined,
                });
            }
            () = &mut expiry => {
                eprintln!();
                return Ok(Outcome::TimedOut);
            }
            frame = transport.receive() => match frame? {
                None => {
                    eprintln!();
                    return Err("the sender went away before you answered".into());
                }
                Some(Frame::Chunk(_)) => {
                    eprintln!();
                    let _ = transport
                        .send_control(json!({ "type": "cancel", "reason": "integrity" }))
                        .await;
                    return Err("the sender started sending before you accepted, so the \
                                transfer was stopped"
                        .into());
                }
                Some(Frame::Control(payload)) => match payload["type"].as_str() {
                    Some("cancel") => {
                        eprintln!();
                        crate::direct::state("cancelled");
                        return Err(crate::cancel::Ended::PeerCancelled(peer_cancelled(
                            "sender",
                            payload["reason"].as_str(),
                        ))
                        .into());
                    }
                    Some("error") => {
                        eprintln!();
                        return Err(display::peer_message(
                            payload["message"].as_str().unwrap_or("the transfer failed"),
                        )
                        .into());
                    }
                    _ => {}
                },
            },
        }
    }
}

/// A sentence for a peer's `cancel`, from the reasons peers agree on.
///
/// The reason is matched, never printed, so a peer cannot put its own words on
/// this terminal through it.
pub fn peer_cancelled(peer: &str, reason: Option<&str>) -> String {
    match reason {
        Some("write_failed") => format!("the {peer} could not write the file and stopped"),
        Some("read_failed") => format!("the {peer} could not read what it was sending and stopped"),
        Some("integrity") => {
            format!("the {peer} stopped because the transfer did not arrive intact")
        }
        Some("too_large") => {
            format!("the {peer} stopped because the transfer grew past a safe size")
        }
        _ => format!("the {peer} cancelled the transfer"),
    }
}

/// The preview as the terminal shows it.
pub fn render(preview: &Preview) -> String {
    let mut out = String::from("\nIncoming transfer\n");

    match &preview.landing {
        Landing::Folder { into } => {
            let folder = display::name(
                preview
                    .name
                    .strip_suffix(".gz")
                    .unwrap_or(&preview.name)
                    .strip_suffix(".tar")
                    .unwrap_or(&preview.name),
            );
            let mut detail = Vec::new();
            if let Some(count) = preview.entry_count {
                detail.push(format!("{count} file{}", if count == 1 { "" } else { "s" }));
            }
            if let Some(unpacked) = preview.unpacked_size {
                detail.push(format!(
                    "{} unpacked",
                    crate::progress::format_bytes(unpacked)
                ));
            }

            if detail.is_empty() {
                out.push_str(&format!("  Folder   {folder}\n"));
            } else {
                out.push_str(&format!("  Folder   {folder}   {}\n", detail.join(", ")));
            }
            out.push_str(&format!(
                "  Size     {} to download\n",
                crate::progress::format_bytes(preview.size)
            ));
            out.push_str(&format!("  Into     {}\n", display::for_terminal(into)));
        }
        Landing::File {
            path,
            instead_of,
            replacing,
        } => {
            out.push_str(&format!("  Name     {}\n", display::name(&preview.name)));
            out.push_str(&format!(
                "  Type     {}\n",
                display::type_label(&preview.name)
            ));
            out.push_str(&format!(
                "  Size     {}\n",
                crate::progress::format_bytes(preview.size)
            ));

            let path = display::for_terminal(path);
            if *replacing {
                out.push_str(&format!(
                    "  Save as  {path}   (replacing the file already there)\n"
                ));
            } else if let Some(requested) = instead_of {
                out.push_str(&format!(
                    "  Save as  {path}   (instead of {})\n",
                    display::name(requested)
                ));
            } else {
                out.push_str(&format!("  Save as  {path}\n"));
            }

            if display::is_program(&preview.name) {
                out.push_str("  This is a program. Only open it if you trust the sender.\n");
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::{ACCEPT_DEADLINE, Consent, ConsentPrompt, Landing, Outcome, Preview, ask, render};
    use crate::transport::scripted::ScriptedTransport;
    use serde_json::json;
    use std::time::Duration;

    struct Answering(Consent);

    impl ConsentPrompt for Answering {
        async fn decide(&mut self, _preview: &Preview) -> Consent {
            self.0
        }
    }

    /// Never answers, like a person who walked away.
    struct Silent;

    impl ConsentPrompt for Silent {
        async fn decide(&mut self, _preview: &Preview) -> Consent {
            std::future::pending().await
        }
    }

    fn a_file() -> Preview {
        Preview {
            name: "report.pdf".into(),
            size: 2_500_000,
            landing: Landing::File {
                path: "./report.pdf".into(),
                instead_of: None,
                replacing: false,
            },
            entry_count: None,
            unpacked_size: None,
        }
    }

    #[tokio::test]
    async fn an_answer_is_the_outcome() {
        for (answer, outcome) in [
            (Consent::Accept, Outcome::Accepted),
            (Consent::Decline, Outcome::Declined),
        ] {
            let mut transport = ScriptedTransport::saying(vec![]).held_open();
            let decided = ask(
                &mut transport,
                &mut Answering(answer),
                &a_file(),
                ACCEPT_DEADLINE,
            )
            .await
            .expect("decided");
            assert_eq!(decided, outcome);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn nobody_answering_is_a_timeout_not_a_hang() {
        // A peer that stays connected and says nothing, so only the deadline
        // can end this.
        let mut transport = ScriptedTransport::saying(vec![]).held_open();
        let preview = a_file();
        let mut silent = Silent;
        let pending = ask(
            &mut transport,
            &mut silent,
            &preview,
            Duration::from_secs(120),
        );

        let decided = tokio::time::timeout(Duration::from_secs(121), pending)
            .await
            .expect("the deadline must end the question")
            .expect("a timeout is not an error");

        assert_eq!(decided, Outcome::TimedOut);
    }

    #[tokio::test]
    async fn a_sender_cancelling_ends_the_question_with_its_reason() {
        let mut transport =
            ScriptedTransport::saying(vec![json!({ "type": "cancel", "reason": "user" })]);

        let error = ask(&mut transport, &mut Silent, &a_file(), ACCEPT_DEADLINE)
            .await
            .expect_err("a cancelled transfer cannot be accepted");

        assert!(error.to_string().contains("sender cancelled"), "{error}");
    }

    #[tokio::test]
    async fn the_relays_narration_does_not_end_the_question() {
        let mut transport = ScriptedTransport::saying(vec![
            json!({ "type": "status", "status": "sending" }),
            json!({ "type": "progress", "bytes_transferred": 0, "total_bytes": 10 }),
        ])
        .held_open();

        let decided = ask(
            &mut transport,
            &mut Answering(Consent::Accept),
            &a_file(),
            ACCEPT_DEADLINE,
        )
        .await
        .expect("narration is not an error");

        assert_eq!(decided, Outcome::Accepted);
    }

    #[tokio::test]
    async fn a_chunk_before_consent_stops_the_transfer_and_says_why() {
        let mut transport =
            ScriptedTransport::new(vec![crate::transport::Frame::Chunk(vec![0; 16])]);

        let error = ask(&mut transport, &mut Silent, &a_file(), ACCEPT_DEADLINE)
            .await
            .expect_err("bytes nobody agreed to are not a transfer");

        assert!(error.to_string().contains("before you accepted"), "{error}");
        let sent = transport.sent_control();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["type"], "cancel");
        assert_eq!(sent[0]["reason"], "integrity");
    }

    #[test]
    fn a_cancel_reason_is_matched_and_never_printed() {
        let sentence = super::peer_cancelled("sender", Some("\u{1b}[2Jgotcha"));
        assert_eq!(sentence, "the sender cancelled the transfer");
    }

    #[test]
    fn the_preview_names_what_is_coming_and_where_it_goes() {
        let shown = render(&Preview {
            name: "report.pdf".into(),
            size: 2_500_000,
            landing: Landing::File {
                path: "./report-1.pdf".into(),
                instead_of: Some("report.pdf".into()),
                replacing: false,
            },
            entry_count: None,
            unpacked_size: None,
        });

        assert!(shown.contains("report.pdf"), "{shown}");
        assert!(shown.contains("PDF"), "{shown}");
        assert!(shown.contains("2.4 MiB"), "{shown}");
        assert!(shown.contains("./report-1.pdf"), "{shown}");
        assert!(shown.contains("instead of report.pdf"), "{shown}");
        assert!(!shown.contains("program"), "{shown}");
    }

    #[test]
    fn a_program_is_called_one_whatever_its_name_pretends() {
        let shown = render(&Preview {
            name: "invoice.pdf                         .exe".into(),
            ..a_file()
        });

        assert!(shown.contains("This is a program"), "{shown}");
        assert!(
            !shown.contains("PDF"),
            "the type must come from the real extension: {shown}"
        );
    }

    #[test]
    fn a_folder_preview_gives_the_senders_counts() {
        let shown = render(&Preview {
            name: "holiday-photos.tar.gz".into(),
            size: 1_100_000_000,
            landing: Landing::Folder {
                into: "/home/me/Downloads".into(),
            },
            entry_count: Some(128),
            unpacked_size: Some(1_200_000_000),
        });

        assert!(shown.contains("holiday-photos "), "{shown}");
        assert!(shown.contains("128 files"), "{shown}");
        assert!(shown.contains("unpacked"), "{shown}");
        assert!(shown.contains("/home/me/Downloads"), "{shown}");
    }

    /// Three clocks bound an unanswered question, and they only work in one
    /// order: the receiver's deadline, then the sender's wait for an answer,
    /// then the relay's session lifetime. Inverted, the sender or the relay
    /// gives up on a receiver that is still deciding and reports it as a
    /// broken connection.
    #[test]
    fn the_three_clocks_are_in_order() {
        let relay_session = Duration::from_secs(api::config::SESSION_TTL_SECS);

        assert!(ACCEPT_DEADLINE < crate::send::CONSENT_TIMEOUT);
        assert!(crate::send::CONSENT_TIMEOUT < relay_session);
    }
}
