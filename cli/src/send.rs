//! The sending half of a terminal-to-terminal transfer.

use std::{error::Error, fmt, future::Future, io::IsTerminal, path::Path, time::Duration};

use serde_json::{Value, json};

use crate::{
    cancel::{Cancel, Cancellable, Ended},
    client, crypto, direct,
    payload::Payload,
    progress::Progress,
    transport::{Frame, Transport, relay},
};

/// In-flight bytes the sender allows before waiting for acknowledgements.
///
/// This is the window that decides throughput on a high-latency link: the
/// sender may keep this much data unacknowledged, so the ceiling is roughly
/// `WINDOW_BYTES / round-trip time` regardless of available bandwidth.
const WINDOW_BYTES: u64 = 16 * 1024 * 1024;

/// How long the sender waits to hear whether the peer opened the metadata.
///
/// Only the direct path waits at all. The frame being waited for is a few
/// bytes and the peer sends it the moment it has decrypted something it
/// already holds, so this is generous rather than tight — it bounds a peer
/// that has stopped talking, not a slow one.
const META_CHECKPOINT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the sender waits for the receiver to accept.
///
/// The receiver's own question closes after
/// [`crate::consent::ACCEPT_DEADLINE`] and answers `decline` then, so this only
/// bounds a receiver that has stopped talking altogether. Longer than that
/// deadline, so a slow link cannot make the sender give up on a receiver that
/// is still deciding.
pub const CONSENT_TIMEOUT: Duration = Duration::from_secs(150);

/// How one attempt at a transfer ended.
///
/// Two outcomes rather than a success and an error, because a peer that could
/// not open the metadata is not a broken transfer — it is one consumed guess,
/// and `docs/decisions.md` entry 13 has the sender ask a human whether to allow
/// another. The caller cannot make that decision from an error message, and
/// matching on message text would rest a security decision on a sentence
/// somebody might one day reword.
///
/// The payload comes back with the failure, and that is the type saying
/// something true rather than a convenience: the checkpoint fires before a
/// single chunk is produced, so nothing has been read, compressed or spooled
/// twice. A retry costs a connection, not the file.
pub enum Attempt {
    Done,
    FailedTheCode {
        /// What the peer actually did. An explicit refusal, a timeout, a
        /// disconnect and an unexpected frame all reach here, and entry 13
        /// counts them the same: from this side the honest mistyper and the
        /// silent attacker are indistinguishable, and should be.
        what_happened: &'static str,
        payload: Payload,
    },
}

/// Written by hand because [`Payload`] is a file on its way somewhere, not
/// something to render into a test failure or a log line.
impl fmt::Debug for Attempt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Done => formatter.write_str("Done"),
            Self::FailedTheCode { what_happened, .. } => {
                write!(formatter, "FailedTheCode({what_happened})")
            }
        }
    }
}

pub struct SendOptions {
    /// The relay to forward through, if one is configured. `None` is the
    /// ordinary case: there is no hosted relay and no compiled-in default, so
    /// a transfer that has not been given one stays on the direct path.
    pub origin: Option<String>,
    pub compress: Option<u32>,
    /// Which carrier to use. See [`crate::direct::Path`].
    pub path: crate::direct::Path,
    /// Print [`crate::direct::status_line`] beside the prose, for a caller
    /// that is a program rather than a person.
    pub status: bool,
    /// Which rendezvous infrastructure the direct path uses. Default is n0's
    /// relays and the public DHT; see [`crate::direct::Rendezvous`].
    pub rendezvous: crate::direct::Rendezvous,
    /// Called once with the session code, as soon as the relay issues it.
    ///
    /// The code is what the other terminal needs, so it is handed to the caller
    /// rather than only printed: that keeps presentation in the binary and lets
    /// callers that are not a terminal observe it.
    pub on_code: Box<dyn FnMut(&str) + Send>,
}

impl SendOptions {
    /// Options that print the code to stdout, one line, nothing else, so it
    /// survives being piped into another command.
    pub fn printing(
        origin: Option<String>,
        compress: Option<u32>,
        path: crate::direct::Path,
        status: bool,
        rendezvous: crate::direct::Rendezvous,
    ) -> Self {
        Self {
            origin,
            compress,
            path,
            status,
            rendezvous,
            on_code: Box::new(|code| println!("{code}")),
        }
    }
}

pub async fn run(path: &Path, options: SendOptions) -> Result<(), Box<dyn Error + Send + Sync>> {
    // The first Ctrl-C tells the receiver and stops; a second, or a transfer
    // that will not stop, exits at once. Exiting that way does not unwind, so
    // it restores the terminal and deletes spool files itself. See
    // `crate::cancel`.
    let cancel = Cancel::default();
    crate::cancel::exit_on_second_interrupt(cancel.clone());

    run_cancellable(path, options, cancel).await
}

/// [`run`], stopped by `cancel` rather than by a signal handler it installs.
/// For programs and tests that decide when to stop.
pub async fn run_cancellable(
    path: &Path,
    mut options: SendOptions,
    cancel: Cancel,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    if options.status {
        direct::enable_state_lines();
    }

    // Before the payload is read, compressed and spooled: being told the
    // relay is missing is worth nothing after a wait to compress a directory.
    if options.path == direct::Path::Relay && options.origin.is_none() {
        return Err(format!(
            "--transport relay forwards through a relay, and none is configured: {}.",
            client::NAME_A_RELAY
        )
        .into());
    }

    let payload = Payload::prepare(path, options.compress)?;

    for warning in &payload.warnings {
        // Names from the tree being sent. They are local, but a hostile name
        // can already be sitting on a shared disk.
        eprintln!("warning: {}", crate::display::for_terminal(warning));
    }

    if payload.size == 0 {
        return Err("there is nothing to send: the payload is empty".into());
    }

    eprintln!("Sending {}", crate::display::for_terminal(&payload.summary));
    eprintln!("Size    {}", crate::progress::format_bytes(payload.size));

    // The relay bounds and accounts for what actually crosses it, which is
    // ciphertext, so that is what the session reserves.
    let sealed_size = crypto::ciphertext_len(payload.size);

    // Every fallback decision happens here, before a code exists. Once one is
    // printed the path is fixed, because the two paths name their nameplates
    // differently and the code carries one of them.
    if options.path != direct::Path::Relay {
        match try_direct(&mut options, payload, sealed_size, &cancel).await {
            Ok(outcome) => return outcome,
            Err(failed) => {
                direct::may_fall_back(
                    options.path,
                    options.origin.as_deref(),
                    failed.error.as_ref(),
                )?;
                eprintln!("No peer-to-peer path: {}", failed.error);
                eprintln!("Falling back to the relay.");

                return send_over_relay(
                    &mut options,
                    *failed.payload,
                    sealed_size,
                    direct::Fallback::Rendezvous,
                    &cancel,
                )
                .await;
            }
        }
    }

    send_over_relay(
        &mut options,
        payload,
        sealed_size,
        direct::Fallback::None,
        &cancel,
    )
    .await
}

/// A direct path that could not be set up, carrying the payload back.
///
/// Boxed because a [`Payload`] is large and this rides in an `Err`, which
/// otherwise makes every `Result` in this file the size of the failure case.
pub(crate) struct SetupFailed {
    error: Box<dyn Error + Send + Sync>,
    payload: Box<Payload>,
}

/// Sets up a transfer nobody operates, and runs it.
///
/// The payload comes back with a setup failure so `run` can still fall back
/// without re-reading, compressing and spooling the file a second time — the
/// same reason [`Attempt::FailedTheCode`] carries it.
async fn try_direct(
    options: &mut SendOptions,
    payload: Payload,
    sealed_size: u64,
    cancel: &Cancel,
) -> Result<Result<(), Box<dyn Error + Send + Sync>>, Box<SetupFailed>> {
    eprintln!("Looking for a peer-to-peer path...");

    let directory = match options.rendezvous.directory() {
        Ok(directory) => directory,
        Err(error) => return Err(SetupFailed::new(error, payload)),
    };

    let published = match direct::publish_sender(&directory, &options.rendezvous).await {
        Ok(published) => published,
        Err(error) => return Err(SetupFailed::new(error, payload)),
    };

    let code = published.code.clone();
    announce(options, &code);
    direct::report(
        direct::Carrier::Direct,
        direct::Fallback::None,
        options.status,
    );
    eprintln!("Waiting for the receiver to connect...");

    // Only from here does a failure stop being a fallback: the code is out, a
    // receiver may already be dialling, and starting again elsewhere would send
    // them to a nameplate nobody is listening on.
    let endpoint = published.endpoint;
    let result = send_policing_guesses(
        || {
            let cancel = cancel.clone();
            let accepting = endpoint.accept_transfer();
            async move {
                // Nobody has connected yet, so there is nobody to tell.
                tokio::select! {
                    biased;
                    () = cancel.fired() => Err(crate::transport::TransportError::Cancelled),
                    accepted = accepting => {
                        accepted.map(|transport| Cancellable::new(transport, cancel))
                    }
                }
            }
        },
        &code,
        payload,
        sealed_size,
        &mut AskTheTerminal::new(),
    )
    .await;

    endpoint.shutdown().await;

    Ok(result)
}

impl SetupFailed {
    fn new(error: impl Into<Box<dyn Error + Send + Sync>>, payload: Payload) -> Box<Self> {
        Box::new(Self {
            error: error.into(),
            payload: Box::new(payload),
        })
    }
}

/// The path with a Drop server in it, unchanged.
///
/// `fallback` is why this is the relay rather than what the relay did, so it
/// is passed in rather than decided here: this function cannot tell being
/// asked for from being fallen back to.
async fn send_over_relay(
    options: &mut SendOptions,
    payload: Payload,
    sealed_size: u64,
    fallback: direct::Fallback,
    cancel: &Cancel,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    // Session creation is a blocking HTTP call, so it runs on the blocking
    // pool rather than stalling a runtime worker.
    // Unreachable in practice — `run` refuses `--transport relay` without one
    // and `may_fall_back` refuses to arrive here without one — but stated as an
    // error rather than an unwrap, because the alternative to a sentence here
    // is a panic in front of somebody sending a file.
    let origin = options
        .origin
        .clone()
        .ok_or_else(|| format!("this transfer needs a relay: {}", client::NAME_A_RELAY))?;

    let nameplate = {
        let origin = origin.clone();

        tokio::task::spawn_blocking(move || client::create_session(&origin, sealed_size)).await??
    };

    // The relay allocated the nameplate; the words are drawn here and never
    // sent anywhere. Together they are what the receiver types.
    let code = crypto::TransferCode::generate_for(&nameplate)?;
    announce(options, &code);
    direct::report(direct::Carrier::Relay, fallback, options.status);
    eprintln!("Waiting for the receiver to connect...");

    let mut transport = Cancellable::new(
        relay::connect_sender(&origin, code.nameplate()).await?,
        cancel.clone(),
    );

    match send_transfer(&mut transport, &code, payload, sealed_size).await? {
        Attempt::Done => Ok(()),
        // A wrong code over the relay. The relay has already burned the
        // session, so there is no second attempt to offer and nobody to ask:
        // the transfer ends here and says why.
        Attempt::FailedTheCode { what_happened, .. } => {
            Err(format!("the receiver could not open this transfer: {what_happened}").into())
        }
    }
}

/// Shows the code, once, however the caller asked for it to be shown.
fn announce(options: &mut SendOptions, code: &crypto::TransferCode) {
    let shareable = code.to_shareable();

    // The bare code goes to stdout, alone on its line, so it survives a pipe.
    // Everything below is stderr and is for the person watching.
    (options.on_code)(&shareable);

    eprintln!();
    eprintln!("  Give that code to whoever is receiving. They can run \"drop recv\"");
    eprintln!("  and enter it when asked, or skip the prompt with:");
    eprintln!();
    eprintln!("      drop recv {shareable}");
    eprintln!();
}

/// The sender's half of a transfer, over whatever is carrying it.
///
/// Everything below this line is written against the conversation rather than
/// against a socket, so a second carrier is a different `T` and not a second
/// copy of this function.
pub(crate) async fn send_transfer<T: Transport>(
    transport: &mut T,
    code: &crypto::TransferCode,
    payload: Payload,
    sealed_size: u64,
) -> Result<Attempt, Box<dyn Error + Send + Sync>> {
    transport.await_peer().await?;
    eprintln!("Receiver connected.");
    direct::state("connected");

    let keys = exchange_keys(transport, code).await?;
    let mut sealer = crypto::Sealer::new(&keys, payload.size);

    let metadata = crypto::seal_metadata(
        &keys,
        sealed_size,
        &crypto::Metadata {
            filename: payload.filename.clone(),
            mime_type: payload.mime_type.clone(),
            plaintext_size: payload.size,
            entry_count: payload.entry_count,
            unpacked_size: payload.unpacked_size,
        },
    )?;

    transport
        .send_control(json!({
            "type": "meta",
            "version": crypto::ENVELOPE_VERSION,
            "ciphertext_size": sealed_size,
            "metadata": crypto::to_hex(&metadata),
        }))
        .await?;

    // Nothing has been streamed yet, and nothing will be until the peer proves
    // it opened what was just sent.
    if let Some(what_happened) = await_meta_checkpoint(transport, &keys).await? {
        transport.close().await;

        return Ok(Attempt::FailedTheCode {
            what_happened,
            payload,
        });
    }

    // Nor until the receiver has seen what this is and agreed to it.
    direct::state("code-ok");
    eprintln!("The receiver entered the code and is deciding whether to accept...");
    if let Err(ended) = await_consent(transport).await {
        transport.close().await;
        return Err(ended);
    }
    eprintln!("Accepted.");
    direct::state("accepted");

    let total = payload.size;
    let result = stream_payload(transport, payload, &mut sealer, sealed_size).await;

    if let Err(error) = result {
        // A failure on this side, reading or sealing the payload, is news to
        // the receiver, so tell it rather than leave it waiting on a session
        // that will never finish. A failure that came from the other side, or
        // from a person cancelling here, has already been said.
        let already_said = error.downcast_ref::<Ended>().is_some()
            || error
                .downcast_ref::<crate::transport::TransportError>()
                .is_some();
        if !already_said {
            let _ = transport
                .send_control(json!({ "type": "cancel", "reason": "read_failed" }))
                .await;
        }
        transport.close().await;
        return Err(error);
    }

    await_completion(transport, sealed_size).await?;
    transport.close().await;

    eprintln!("Sent {}.", crate::progress::format_bytes(total));
    direct::state("done");
    Ok(Attempt::Done)
}

/// Asked before a peer that failed the code is allowed to be followed by
/// another.
///
/// A trait rather than a direct read of stdin so the policy can be tested
/// without a terminal, which matters more than usual here: the decision this
/// gates is the one holding a 33-bit password together.
pub trait AnotherAttempt {
    /// `attempt` counts the guesses already consumed, starting at one.
    fn allow(&mut self, attempt: u32, what_happened: &str) -> impl Future<Output = bool> + Send;
}

/// Asks the human sitting in front of the sender, in `docs/decisions.md`
/// entry 13's words.
///
/// The count is shown on purpose. Entry 13 chose this over a fixed retry limit
/// because an attacker grinding the code then needs an approval per guess,
/// which caps the attack at human speed *and makes it visible* — and a counter
/// nobody is shown is not visible.
pub struct AskTheTerminal {
    interactive: bool,
}

impl Default for AskTheTerminal {
    fn default() -> Self {
        Self::new()
    }
}

impl AskTheTerminal {
    pub fn new() -> Self {
        Self {
            interactive: std::io::stdin().is_terminal(),
        }
    }

    /// A sender with nobody in front of it.
    ///
    /// Tests construct this rather than reading the real stdin, because
    /// `cargo test` inherits whatever terminal it was started from: asking the
    /// process would make the strict-when-unattended case pass or block
    /// depending on how the suite happened to be launched.
    #[cfg(test)]
    fn unattended() -> Self {
        Self { interactive: false }
    }
}

impl AnotherAttempt for AskTheTerminal {
    async fn allow(&mut self, attempt: u32, what_happened: &str) -> bool {
        eprintln!();
        eprintln!("A peer connected and failed the code ({what_happened}).");
        eprintln!("This may be a mistype, or someone guessing.");

        if attempt > 1 {
            eprintln!(
                "That is {attempt} failed attempts on this transfer. Repeated \
                 failures are what being probed looks like."
            );
        }

        // Not a terminal means nobody is there to answer, and entry 13 makes
        // that strict: one attempt, exactly as the relay would have enforced.
        // Failing closed is the safe direction.
        if !self.interactive {
            eprintln!("Not running in a terminal, so this transfer ends here.");
            return false;
        }

        eprint!("Allow another attempt? [y/N] ");

        // Reading stdin blocks, and blocking a runtime worker while a human
        // thinks would stall every other task on it.
        let answer = tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).map(|_| line)
        })
        .await;

        match answer {
            Ok(Ok(line)) => matches!(line.trim(), "y" | "Y" | "yes" | "Yes"),
            // A stdin that cannot be read is a stdin nobody answered.
            _ => false,
        }
    }
}

/// Sends a payload over a carrier the peers themselves have to police,
/// allowing another attempt only when a human says so.
///
/// Public, and nothing in the binary calls it yet. That is the same shape
/// [`crate::transport::quic`] has and for the same reason: the direct path is
/// built and deliberately not reachable, and Phase 4 is what connects the two.
///
/// This is the whole of `docs/decisions.md` entry 13's mechanism. The relay
/// path does not come through here and does not need to: a wrong guess burns
/// the session server-side, so there is nothing for a human to decide.
///
/// `accept` produces one fresh connection per attempt. It has to be fresh —
/// the failed one was closed, and reusing a connection an unknown peer already
/// spoke on would be handing the same peer its second guess for free.
pub async fn send_policing_guesses<T, A, F, Fut>(
    mut accept: F,
    code: &crypto::TransferCode,
    mut payload: Payload,
    sealed_size: u64,
    approver: &mut A,
) -> Result<(), Box<dyn Error + Send + Sync>>
where
    T: Transport,
    A: AnotherAttempt,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, crate::transport::TransportError>>,
{
    let mut attempt = 1_u32;

    loop {
        let mut transport = accept().await?;

        match send_transfer(&mut transport, code, payload, sealed_size).await? {
            Attempt::Done => return Ok(()),
            Attempt::FailedTheCode {
                what_happened,
                payload: returned,
            } => {
                // Handed back rather than re-read: nothing was streamed, so a
                // retry costs a connection and not the file.
                payload = returned;

                if !approver.allow(attempt, what_happened).await {
                    return Err(format!(
                        "the transfer ended after {attempt} failed attempt{}",
                        if attempt == 1 { "" } else { "s" }
                    )
                    .into());
                }

                attempt += 1;
                eprintln!("Waiting for another receiver...");
            }
        }
    }
}

/// Waits for the peer to prove it opened the metadata.
///
/// This is the moment the protocol puts between a code and a payload. On the
/// direct path it is the whole of the one-guess enforcement: without it the
/// sender streams an entire payload before learning anything about the peer, so
/// a wrong guess costs an attacker one connection and reveals one bit, which is
/// an unlimited online oracle against a 33-bit password. Over the relay the
/// relay enforces one guess itself, and this is what lets the sender know the
/// code was right before a byte moves.
///
/// The peer's `meta_ok` must carry a key confirmation these keys accept. A bare
/// `meta_ok` is a claim, and the party making it is the party being
/// rate-limited: a wrong guesser could send it anyway, the attempt counter
/// would never climb, and nobody would be asked. See
/// `docs/plans/meta-ok-key-confirmation-plan-2026-08-31.md`.
///
/// Every way of not hearing a valid `meta_ok` is the same outcome. See
/// [`Attempt::FailedTheCode`].
///
/// # What is skipped, and only where
///
/// Over the relay, the relay narrates between `meta` and the peer's answer: it
/// reports `sending` once it accepts `meta`, and throttled `progress`. Those
/// are the relay's words, not the peer's, so they are read past. On the direct
/// path there is no narrator, and any frame but `meta_ok` is the peer not
/// answering the question. The deadline covers the whole wait either way, so
/// nothing a peer sends can hold it open.
///
/// # On cancelling a read
///
/// `iroh`'s stream reads are not cancel-safe: a `read_exact` dropped partway
/// through leaves the framing mid-frame, and the survey in
/// `docs/validation/iroh-pkarr-api-survey-2026-08-24.md` warns against putting
/// a timeout around a transfer's reads for exactly that reason. It does not
/// bite here, and the reason is worth stating rather than rediscovering: every
/// path out of this function that involves the timeout firing also abandons
/// this connection. A stream nobody reads again cannot be desynchronised by
/// having been left mid-frame.
async fn await_meta_checkpoint<T: Transport>(
    transport: &mut T,
    keys: &crypto::SessionKeys,
) -> Result<Option<&'static str>, Box<dyn Error + Send + Sync>> {
    let relay_narrates = !transport.peers_enforce_one_guess();

    let answer = tokio::time::timeout(META_CHECKPOINT_TIMEOUT, async {
        loop {
            match transport.receive().await? {
                Some(Frame::Control(payload))
                    if relay_narrates
                        && matches!(payload["type"].as_str(), Some("status" | "progress")) =>
                {
                    continue;
                }
                other => return Ok::<_, crate::transport::TransportError>(other),
            }
        }
    })
    .await;

    let outcome: Result<Option<&'static str>, Box<dyn Error + Send + Sync>> = match answer {
        Err(_) => Ok(Some("it stopped responding")),
        Ok(Err(error)) => Err(error.into()),
        Ok(Ok(None)) => Ok(Some("it disconnected without answering")),
        // A peer that sends payload here is not answering the question.
        Ok(Ok(Some(Frame::Chunk(_)))) => Ok(Some("it sent data instead of answering")),
        Ok(Ok(Some(Frame::Control(payload)))) => Ok(match payload["type"].as_str() {
            Some("meta_ok") => {
                let proven = payload["confirmation"]
                    .as_str()
                    .and_then(|hex| crypto::from_hex(hex).ok())
                    .is_some_and(|offered| keys.confirms(&offered));

                // Counted exactly like every other failure: one attempt. The
                // words say what was observed, as the other outcomes' do, and
                // not whether the peer was honest, which cannot be known.
                if proven {
                    None
                } else {
                    Some("it could not prove it opened the transfer")
                }
            }
            Some("error") => Some("it reported that the code did not open it"),
            // Anything else is a peer not following the protocol, which is what
            // a peer probing the code looks like.
            Some(_) | None => Some("it answered with something else"),
        }),
    };

    outcome
}

/// Waits for the receiver to accept the transfer it has been shown.
///
/// Anything other than `accept` ends the transfer, and says how in the
/// receiver's terms: declined, did not answer in time, or cancelled. The
/// relay's narration is read past; a peer never sends it.
async fn await_consent<T: Transport>(
    transport: &mut T,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let answer = tokio::time::timeout(CONSENT_TIMEOUT, async {
        loop {
            let Some(frame) = transport.receive().await? else {
                return Err::<(), Box<dyn Error + Send + Sync>>(
                    "the receiver went away without answering".into(),
                );
            };

            let Frame::Control(payload) = frame else {
                return Err("the receiver sent data instead of answering".into());
            };

            match payload["type"].as_str() {
                Some("accept") => return Ok(()),
                Some("decline") => {
                    direct::state("declined");
                    let why = match payload["reason"].as_str() {
                        Some("timed_out") => {
                            "the receiver did not answer within two minutes, so the transfer \
                             was declined"
                        }
                        _ => "the receiver declined the transfer",
                    };
                    return Err(Ended::Declined(why.to_string()).into());
                }
                Some("cancel") => {
                    direct::state("cancelled");
                    return Err(Ended::PeerCancelled(crate::consent::peer_cancelled(
                        "receiver",
                        payload["reason"].as_str(),
                    ))
                    .into());
                }
                Some("error") => return Err(relay_error(&payload).into()),
                _ => {}
            }
        }
    })
    .await;

    match answer {
        Ok(result) => result,
        Err(_) => Err("the receiver stopped responding before accepting".into()),
    }
}

/// Runs the key exchange and returns the derived session keys.
///
/// The relay carries both messages without being able to use either: the
/// password they authenticate against never leaves this process.
async fn exchange_keys<T: Transport>(
    transport: &mut T,
    code: &crypto::TransferCode,
) -> Result<crypto::SessionKeys, Box<dyn Error + Send + Sync>> {
    let (handshake, outbound) = crypto::Handshake::start(code);

    transport
        .send_control(json!({
            "type": "key_exchange",
            "message": crypto::to_hex(&outbound),
        }))
        .await?;

    while let Some(frame) = transport.receive().await? {
        let Frame::Control(payload) = frame else {
            continue;
        };

        match payload["type"].as_str() {
            Some("key_exchange") => {
                let peer = payload["message"]
                    .as_str()
                    .ok_or("the receiver sent a malformed key exchange message")?;

                return Ok(handshake.finish(&crypto::from_hex(peer)?)?);
            }
            Some("error") => return Err(relay_error(&payload).into()),
            _ => {}
        }
    }

    Err("the connection closed before the receiver completed the key exchange".into())
}

/// Streams the payload, keeping at most [`WINDOW_BYTES`] unacknowledged.
///
/// The window and the acknowledgements are counted in sealed bytes, because
/// that is what the relay sees and meters. Progress is reported against the
/// same scale so the two never disagree on screen.
async fn stream_payload<T: Transport>(
    transport: &mut T,
    payload: Payload,
    sealer: &mut crypto::Sealer,
    total: u64,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut chunks = payload.into_chunks();

    let mut progress = Progress::new("Sending ", total);
    let mut sent = 0_u64;
    let mut acknowledged = 0_u64;

    loop {
        // Drain acknowledgements that have already arrived, then block only if
        // the window is actually full.
        while sent - acknowledged >= WINDOW_BYTES {
            acknowledged = next_acknowledgement(transport, acknowledged).await?;
            progress.update(acknowledged);
        }

        let Some(chunk) = chunks.recv().await else {
            break;
        };

        let sealed = sealer.seal_chunk(&chunk?)?;
        sent += sealed.len() as u64;
        if let Err(error) = transport.send_chunk(sealed).await {
            return Err(explain_write_failure(transport, error).await);
        }
        progress.update(acknowledged);
    }

    if sent != total {
        return Err(format!(
            "the payload produced {sent} bytes but {total} were declared; \
             the source changed while it was being sent"
        )
        .into());
    }

    while acknowledged < total {
        acknowledged = next_acknowledgement(transport, acknowledged).await?;
        progress.update(acknowledged);
    }

    progress.finish(total);

    transport
        .send_control(json!({ "type": "complete" }))
        .await?;

    Ok(())
}

/// Turns a failed write into the reason the peer gave, if it gave one.
///
/// A peer that stops a transfer says so and the connection closes behind it.
/// The sender is usually mid-write when that happens, so the write fails
/// first, as a broken pipe or a reset, and the peer's explanation is still
/// sitting unread. Reporting the broken pipe would describe the symptom and
/// hide the cause. So before giving up, read what already arrived, briefly,
/// and prefer the peer's words.
async fn explain_write_failure<T: Transport>(
    transport: &mut T,
    error: crate::transport::TransportError,
) -> Box<dyn Error + Send + Sync> {
    if !matches!(error, crate::transport::TransportError::Io(_)) {
        return error.into();
    }

    let explanation = tokio::time::timeout(Duration::from_millis(500), async {
        while let Ok(Some(frame)) = transport.receive().await {
            let Frame::Control(payload) = frame else {
                continue;
            };

            match payload["type"].as_str() {
                Some("cancel") => {
                    direct::state("cancelled");
                    return Some(
                        Ended::PeerCancelled(crate::consent::peer_cancelled(
                            "receiver",
                            payload["reason"].as_str(),
                        ))
                        .into(),
                    );
                }
                Some("error") => return Some(relay_error(&payload).into()),
                // Acknowledgements and narration that were already on their
                // way; the explanation, if there is one, is behind them.
                _ => {}
            }
        }

        None
    })
    .await;

    explanation.ok().flatten().unwrap_or_else(|| error.into())
}

async fn next_acknowledgement<T: Transport>(
    transport: &mut T,
    current: u64,
) -> Result<u64, Box<dyn Error + Send + Sync>> {
    while let Some(frame) = transport.receive().await? {
        let Frame::Control(payload) = frame else {
            continue;
        };

        match payload["type"].as_str() {
            // `chunk_ack` is what the receiver actually sends. The relay
            // renames it to `ack` on the way through, so both are the same
            // sentence and which one arrives says only what carried it.
            Some("ack") | Some("chunk_ack") => {
                if let Some(bytes) = payload["bytes_received"].as_u64() {
                    return Ok(bytes.max(current));
                }
            }
            Some("cancel") => {
                direct::state("cancelled");
                return Err(Ended::PeerCancelled(crate::consent::peer_cancelled(
                    "receiver",
                    payload["reason"].as_str(),
                ))
                .into());
            }
            Some("error") => return Err(relay_error(&payload).into()),
            _ => {}
        }
    }

    Err("the transfer connection closed before the receiver acknowledged the file".into())
}

/// Waits for the receiver to confirm it has the whole payload.
///
/// Two frames mean this, and which one arrives depends only on what carried
/// the transfer. The receiver's own word is `complete`, carrying the count it
/// wrote; a relay consumes that, checks the count itself, and reports
/// `transfer_complete` instead. Over a direct connection there is nobody in
/// between to do the checking, so this does it here — otherwise the direct
/// path would report success on a receiver's word alone, which is a weaker
/// promise than the relayed path already makes.
async fn await_completion<T: Transport>(
    transport: &mut T,
    declared: u64,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    eprintln!("Waiting for the receiver to finish writing...");

    while let Some(frame) = transport.receive().await? {
        let Frame::Control(payload) = frame else {
            continue;
        };

        match payload["type"].as_str() {
            Some("complete") => {
                let confirmed = payload["bytes_received"].as_u64().unwrap_or(0);

                if confirmed != declared {
                    return Err(format!(
                        "the receiver confirmed {confirmed} bytes but {declared} were sent"
                    )
                    .into());
                }

                return Ok(());
            }
            Some("status") => match payload["status"].as_str() {
                Some("transfer_complete") => return Ok(()),
                Some("cancelled") => {
                    return Err("the transfer was cancelled".into());
                }
                _ => {}
            },
            Some("finishing") => {
                eprintln!("The receiver has everything and is finishing up...");
                direct::state("finishing");
            }
            Some("cancel") => {
                direct::state("cancelled");
                return Err(Ended::PeerCancelled(crate::consent::peer_cancelled(
                    "receiver",
                    payload["reason"].as_str(),
                ))
                .into());
            }
            Some("error") => return Err(relay_error(&payload).into()),
            _ => {}
        }
    }

    Err("the transfer connection closed before the receiver confirmed the file".into())
}

/// The message in an `error` frame, made safe to print.
///
/// The relay wrote it, or on the direct path the receiver did. Neither is
/// trusted to put bytes on this terminal.
fn relay_error(payload: &Value) -> String {
    crate::display::peer_message(
        payload["message"]
            .as_str()
            .unwrap_or("the relay reported an error"),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        AnotherAttempt, AskTheTerminal, Payload, await_completion, await_meta_checkpoint,
        next_acknowledgement, send_policing_guesses,
    };
    use crate::transport::scripted::ScriptedTransport;
    use serde_json::{Value, json};
    use std::{collections::VecDeque, path::PathBuf};

    use crate::{crypto, transport::Frame};

    /// Keys two peers agree on when both typed `code`, sender's first.
    fn agreed_keys(
        sender_code: &str,
        receiver_code: &str,
    ) -> (crypto::SessionKeys, crypto::SessionKeys) {
        let sender_code = crypto::TransferCode::parse(sender_code).expect("a code");
        let receiver_code = crypto::TransferCode::parse(receiver_code).expect("a code");
        let (sender, sender_half) = crypto::Handshake::start(&sender_code);
        let (receiver, receiver_half) = crypto::Handshake::start(&receiver_code);

        (
            sender.finish(&receiver_half).expect("well formed"),
            receiver.finish(&sender_half).expect("well formed"),
        )
    }

    const CODE: &str = "A1B2C3-abandon-ability-able";

    fn meta_ok_from(keys: &crypto::SessionKeys) -> Value {
        json!({ "type": "meta_ok", "confirmation": crypto::to_hex(&keys.confirmation()) })
    }

    /// The happy path of the checkpoint: the peer proved it opened the
    /// metadata, so the payload may follow.
    #[tokio::test]
    async fn a_peer_that_proves_it_opened_the_metadata_passes_the_checkpoint() {
        let (sender, receiver) = agreed_keys(CODE, CODE);
        let mut transport = ScriptedTransport::saying(vec![meta_ok_from(&receiver)]).direct();

        let outcome = await_meta_checkpoint(&mut transport, &sender)
            .await
            .expect("the peer answered");

        assert!(outcome.is_none(), "unexpected failure: {outcome:?}");
    }

    /// The gap the confirmation closes. A peer that guessed wrong cannot open
    /// the metadata, but nothing stopped it saying it had, and before version 2
    /// that was enough: the attempt counter never climbed and nobody was asked.
    #[tokio::test]
    async fn claiming_success_without_proving_it_is_a_failed_guess() {
        let (sender, _) = agreed_keys(CODE, CODE);
        let (_, wrong) = agreed_keys(CODE, "A1B2C3-abandon-ability-above");

        let claims = [
            (
                "a bare meta_ok, as version 1 sent",
                json!({ "type": "meta_ok" }),
            ),
            ("a confirmation from the wrong keys", meta_ok_from(&wrong)),
            (
                "a confirmation that is not hex",
                json!({ "type": "meta_ok", "confirmation": "not hex at all" }),
            ),
            (
                "a confirmation of the wrong length",
                json!({ "type": "meta_ok", "confirmation": "00ff" }),
            ),
            (
                "a confirmation that is not a string",
                json!({ "type": "meta_ok", "confirmation": 42 }),
            ),
        ];

        for (what, claim) in claims {
            for direct in [true, false] {
                let transport = ScriptedTransport::saying(vec![claim.clone()]);
                let mut transport = if direct {
                    transport.direct()
                } else {
                    transport
                };

                let outcome = await_meta_checkpoint(&mut transport, &sender)
                    .await
                    .expect("an unproven claim is not a transport failure");

                assert!(
                    outcome.is_some(),
                    "{what} passed the checkpoint (direct: {direct})"
                );
            }
        }
    }

    /// Every way of not hearing a proven `meta_ok` is one consumed attempt,
    /// and the caller has to be able to tell that apart from a broken network
    /// without reading the message. `docs/decisions.md` entry 13.
    #[tokio::test]
    async fn every_way_of_not_answering_is_the_same_failed_guess() {
        let (sender, _) = agreed_keys(CODE, CODE);
        let scripts = [
            ("an explicit refusal", vec![json!({ "type": "error" })]),
            ("silence, then a hang-up", vec![]),
            (
                "an unrelated frame",
                vec![json!({ "type": "chunk_ack", "bytes_received": 1 })],
            ),
        ];

        for (what, script) in scripts {
            let mut transport = ScriptedTransport::saying(script).direct();

            let outcome = await_meta_checkpoint(&mut transport, &sender)
                .await
                .expect("a peer that will not answer is not a transport failure");

            assert!(
                outcome.is_some(),
                "{what} should count as a consumed guess, not as a pass"
            );
        }
    }

    /// Over the relay, the relay speaks between `meta` and the peer's answer,
    /// and those are its words rather than the peer's. On the direct path
    /// nobody narrates, so the same frame is a peer not answering.
    #[tokio::test]
    async fn the_relays_narration_is_read_past_and_a_direct_peers_is_not() {
        let (sender, receiver) = agreed_keys(CODE, CODE);
        let script = || {
            vec![
                json!({ "type": "status", "status": "sending" }),
                json!({ "type": "progress", "bytes_transferred": 0, "total_bytes": 10 }),
                meta_ok_from(&receiver),
            ]
        };

        let mut relayed = ScriptedTransport::saying(script());
        let outcome = await_meta_checkpoint(&mut relayed, &sender)
            .await
            .expect("answered");
        assert!(
            outcome.is_none(),
            "the relay's narration was taken for an answer"
        );

        let mut direct = ScriptedTransport::saying(script()).direct();
        let outcome = await_meta_checkpoint(&mut direct, &sender)
            .await
            .expect("answered");
        assert!(
            outcome.is_some(),
            "a direct peer sent something other than meta_ok first and passed anyway"
        );
    }

    /// A receiver that knows the code, and runs a real handshake to prove it.
    ///
    /// It has to react rather than replay. Its confirmation depends on the
    /// sender's handshake half, which is fresh every run, so no fixed script
    /// could hold it.
    fn a_peer_that_completes(code: &crypto::TransferCode, sealed_size: u64) -> ScriptedTransport {
        let (handshake, half) = crypto::Handshake::start(code);
        let mut handshake = Some(handshake);
        let mut keys = None;
        let mut received = 0_u64;

        ScriptedTransport::responding(move |frame| match frame {
            Frame::Control(sent) if sent["type"] == "key_exchange" => {
                let sender_half =
                    crypto::from_hex(sent["message"].as_str().expect("a half")).expect("hex");
                keys = Some(
                    handshake
                        .take()
                        .expect("one key exchange")
                        .finish(&sender_half)
                        .expect("well formed"),
                );
                vec![Frame::Control(
                    json!({ "type": "key_exchange", "message": crypto::to_hex(&half) }),
                )]
            }
            Frame::Control(sent) if sent["type"] == "meta" => vec![
                Frame::Control(meta_ok_from(keys.as_ref().expect("keys first"))),
                Frame::Control(json!({ "type": "accept" })),
            ],
            Frame::Chunk(chunk) => {
                received += chunk.len() as u64;
                if received == sealed_size {
                    vec![Frame::Control(
                        json!({ "type": "chunk_ack", "bytes_received": received }),
                    )]
                } else {
                    vec![]
                }
            }
            Frame::Control(sent) if sent["type"] == "complete" => vec![Frame::Control(
                json!({ "type": "complete", "bytes_received": received }),
            )],
            Frame::Control(_) => vec![],
        })
        .direct()
    }

    /// A peer that gets through the handshake and then cannot open what it was
    /// sent — a mistype, or a guess. They look identical from here.
    fn a_peer_that_fails_the_code(code: &crypto::TransferCode) -> Vec<Value> {
        let (_, half) = crypto::Handshake::start(code);

        vec![
            json!({ "type": "key_exchange", "message": crypto::to_hex(&half) }),
            json!({ "type": "error", "message": "the code did not open this transfer" }),
        ]
    }

    struct Answers {
        replies: VecDeque<bool>,
        asked: Vec<u32>,
    }

    impl Answers {
        fn of(replies: Vec<bool>) -> Self {
            Self {
                replies: replies.into(),
                asked: Vec::new(),
            }
        }
    }

    impl AnotherAttempt for Answers {
        async fn allow(&mut self, attempt: u32, _what_happened: &str) -> bool {
            self.asked.push(attempt);
            self.replies.pop_front().unwrap_or(false)
        }
    }

    fn a_small_payload(name: &str) -> (PathBuf, Payload) {
        let base =
            std::env::temp_dir().join(format!("drop-attempts-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&base).expect("a scratch directory");
        let file = base.join("payload.bin");
        std::fs::write(&file, vec![3u8; 64 * 1024]).expect("fixture written");

        let payload = Payload::prepare(&file, None).expect("payload prepared");
        (base, payload)
    }

    /// The mechanism entry 13 chose, end to end: a failed guess does not end
    /// the transfer, but it does not continue on its own either. A human says
    /// yes, and the next peer gets its one attempt.
    #[tokio::test]
    async fn another_attempt_happens_only_because_a_human_allowed_it() {
        let code =
            crate::crypto::TransferCode::parse("A1B2C3-abandon-ability-able").expect("a code");
        let (base, payload) = a_small_payload("allowed");
        let sealed_size = crate::crypto::ciphertext_len(payload.size);

        let mut attempts = VecDeque::from(vec![
            ScriptedTransport::saying(a_peer_that_fails_the_code(&code)).direct(),
            a_peer_that_completes(&code, sealed_size),
        ]);
        let accept = move || {
            let next = attempts.pop_front().expect("an attempt was prepared");
            async move { Ok(next) }
        };

        let mut answers = Answers::of(vec![true]);

        send_policing_guesses(accept, &code, payload, sealed_size, &mut answers)
            .await
            .expect("the second peer knew the code");

        assert_eq!(
            answers.asked,
            vec![1],
            "the human should have been asked exactly once, after the first \
             failed guess and before the second attempt existed"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    /// Declining is the whole point of asking. It has to actually stop the
    /// transfer rather than merely slow it down.
    #[tokio::test]
    async fn a_declined_attempt_ends_the_transfer() {
        let code =
            crate::crypto::TransferCode::parse("A1B2C3-abandon-ability-able").expect("a code");
        let (base, payload) = a_small_payload("declined");
        let sealed_size = crate::crypto::ciphertext_len(payload.size);

        // Two are prepared and only one may be taken: an accept that ran twice
        // would be a second guess nobody approved.
        let mut attempts = VecDeque::from(vec![
            ScriptedTransport::saying(a_peer_that_fails_the_code(&code)).direct(),
            a_peer_that_completes(&code, sealed_size),
        ]);
        let mut accepted = 0;
        let accept = move || {
            accepted += 1;
            assert!(accepted <= 1, "a declined transfer must not accept again");
            let next = attempts.pop_front().expect("an attempt was prepared");
            async move { Ok(next) }
        };

        let mut answers = Answers::of(vec![false]);

        let error = send_policing_guesses(accept, &code, payload, sealed_size, &mut answers)
            .await
            .expect_err("a declined attempt is not a transfer");

        assert!(
            error.to_string().contains("1 failed attempt"),
            "the sender should say how many guesses it saw: {error}"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    /// Grinding the code costs one human approval per guess. That is the
    /// property entry 13 picked over a retry limit, so it is worth holding
    /// open rather than inferring from the single-failure case.
    #[tokio::test]
    async fn each_further_guess_needs_its_own_approval() {
        let code =
            crate::crypto::TransferCode::parse("A1B2C3-abandon-ability-able").expect("a code");
        let (base, payload) = a_small_payload("grinding");
        let sealed_size = crate::crypto::ciphertext_len(payload.size);

        let mut attempts = VecDeque::from(vec![
            ScriptedTransport::saying(a_peer_that_fails_the_code(&code)).direct(),
            ScriptedTransport::saying(a_peer_that_fails_the_code(&code)).direct(),
            ScriptedTransport::saying(a_peer_that_fails_the_code(&code)).direct(),
        ]);
        let accept = move || {
            let next = attempts.pop_front().expect("an attempt was prepared");
            async move { Ok(next) }
        };

        let mut answers = Answers::of(vec![true, true, false]);

        send_policing_guesses(accept, &code, payload, sealed_size, &mut answers)
            .await
            .expect_err("three failed guesses are not a transfer");

        assert_eq!(
            answers.asked,
            vec![1, 2, 3],
            "every guess after the first must be approved on its own, and the \
             count must climb where the human can see it"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    /// Entry 13's safe direction: with nobody there to notice a counter
    /// climbing, the prompt cannot be the protection, so the sender falls back
    /// to exactly what the relay would have done — one attempt.
    #[tokio::test]
    async fn an_unattended_sender_allows_nothing() {
        let mut approver = AskTheTerminal::unattended();

        assert!(
            !approver.allow(1, "it stopped responding").await,
            "a sender with no terminal must be strict"
        );
    }

    /// The sender's paths run over a transport that is not a socket, which is
    /// the whole claim the trait makes.
    ///
    /// `chunk_ack` is the receiver's own word and arrives unchanged over a
    /// direct connection; `ack` is the relay's rewording of the same thing.
    /// The sender must not care which one it got.
    #[tokio::test]
    async fn an_acknowledgement_counts_under_either_name() {
        for name in ["ack", "chunk_ack"] {
            let mut transport =
                ScriptedTransport::saying(vec![json!({ "type": name, "bytes_received": 90 })]);

            let acknowledged = next_acknowledgement(&mut transport, 40)
                .await
                .expect("an acknowledgement arrived");
            assert_eq!(acknowledged, 90, "under the name {name}");
        }
    }

    /// Acknowledgements only ever move forward. One that is behind what the
    /// sender already counted would otherwise reopen the window.
    #[tokio::test]
    async fn an_acknowledgement_never_moves_backwards() {
        let mut transport =
            ScriptedTransport::saying(vec![json!({ "type": "ack", "bytes_received": 10 })]);

        let acknowledged = next_acknowledgement(&mut transport, 40)
            .await
            .expect("an acknowledgement arrived");
        assert_eq!(acknowledged, 40);
    }

    /// The relay checks the receiver's count before rewording it. Directly,
    /// there is nobody in between, so the sender checks it.
    #[tokio::test]
    async fn a_receiver_confirming_the_whole_payload_completes_the_transfer() {
        let mut transport =
            ScriptedTransport::saying(vec![json!({ "type": "complete", "bytes_received": 4096 })]);

        await_completion(&mut transport, 4096)
            .await
            .expect("the receiver confirmed everything that was sent");
    }

    #[tokio::test]
    async fn a_receiver_confirming_less_than_was_sent_is_not_a_completion() {
        let mut transport =
            ScriptedTransport::saying(vec![json!({ "type": "complete", "bytes_received": 4000 })]);

        let error = await_completion(&mut transport, 4096)
            .await
            .expect_err("a short confirmation is not a success");
        assert!(error.to_string().contains("4000"), "unexpected: {error}");
    }

    /// The relay's rewording still means the same thing, and it carries no
    /// count of its own because the relay already checked it.
    #[tokio::test]
    async fn the_relays_rewording_completes_the_transfer_too() {
        let mut transport = ScriptedTransport::saying(vec![
            json!({ "type": "status", "status": "transfer_complete" }),
        ]);

        await_completion(&mut transport, 4096)
            .await
            .expect("the relay confirmed the transfer");
    }

    #[tokio::test]
    async fn an_error_frame_ends_the_wait_with_its_message() {
        let mut transport = ScriptedTransport::saying(vec![
            json!({ "type": "error", "message": "receiver disconnected" }),
        ]);

        let error = await_completion(&mut transport, 4096)
            .await
            .expect_err("an error frame is not a completion");
        assert!(error.to_string().contains("receiver disconnected"));
    }

    #[tokio::test]
    async fn a_cancelled_transfer_is_distinct_from_a_dropped_connection() {
        let mut transport =
            ScriptedTransport::saying(vec![json!({ "type": "status", "status": "cancelled" })]);

        let error = await_completion(&mut transport, 4096)
            .await
            .expect_err("a cancellation is not a completion");
        assert!(error.to_string().contains("cancelled"));
    }
}
