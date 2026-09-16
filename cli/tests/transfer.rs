//! End-to-end transfers through a real relay.
//!
//! These drive the same code paths the shipped binary uses — session creation,
//! the upload and download sockets, the acknowledgement window, and extraction
//! — against an actual Axum server, so a protocol mistake surfaces here rather
//! than in a user's terminal.

use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use drop_cli::{
    recv::{self, ReceiveOptions},
    send::{self, SendOptions},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::oneshot,
};

async fn spawn_relay() -> String {
    spawn_relay_with_state().await.0
}

/// As [`spawn_relay`], but handing back the relay's state so a test can watch
/// a session rather than guess at its timing.
async fn spawn_relay_with_state() -> (String, api::app_state::AppState) {
    let state = api::build_state();
    api::start_background_services(state.clone());

    let app = api::build_app(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("relay listener");
    let addr: SocketAddr = listener.local_addr().expect("relay address");

    tokio::spawn(async move {
        api::serve(listener, app).await.expect("relay server");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    (format!("http://{addr}"), state)
}

fn scratch(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "drop-transfer-{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&base).expect("scratch directory");
    base
}

fn write_file(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent directory");
    }
    fs::write(path, contents).expect("write fixture");
}

/// Sends `source` and receives it into `destination`, exactly as two terminals
/// would.
async fn transfer(
    origin: &str,
    source: &Path,
    destination: &Path,
    compress: Option<u32>,
) -> Result<(), String> {
    transfer_forcing(origin, source, destination, compress, true).await
}

/// As [`transfer`], but choosing whether the receiver may replace files it
/// already has.
async fn transfer_forcing(
    origin: &str,
    source: &Path,
    destination: &Path,
    compress: Option<u32>,
    force: bool,
) -> Result<(), String> {
    let (code_tx, code_rx) = oneshot::channel();
    let mut code_tx = Some(code_tx);

    let sender = tokio::spawn({
        let origin = origin.to_string();
        let source = source.to_path_buf();

        async move {
            send::run(
                &source,
                SendOptions {
                    origin: Some(origin),
                    compress,
                    // These drive a real relay in-process, so they pin the
                    // path rather than letting `auto` reach for a DHT.
                    path: drop_cli::direct::Path::Relay,
                    status: false,
                    rendezvous: drop_cli::direct::Rendezvous::default(),
                    on_code: Box::new(move |code| {
                        if let Some(sender) = code_tx.take() {
                            let _ = sender.send(code.to_string());
                        }
                    }),
                },
            )
            .await
            .map_err(|error| error.to_string())
        }
    });

    let code = tokio::time::timeout(Duration::from_secs(10), code_rx)
        .await
        .map_err(|_| "the sender never produced a session code".to_string())?
        .map_err(|_| "the sender failed before producing a session code".to_string())?;

    let receiver = tokio::spawn({
        let origin = origin.to_string();
        let destination = destination.to_path_buf();

        async move {
            recv::run(
                &code,
                ReceiveOptions {
                    path: drop_cli::direct::Path::Relay,
                    status: false,
                    rendezvous: drop_cli::direct::Rendezvous::default(),
                    origin: Some(origin),
                    out_dir: destination,
                    extract: true,
                    force,
                    acceptance: drop_cli::consent::Acceptance::Yes,
                },
            )
            .await
            .map_err(|error| error.to_string())
        }
    });

    let (sent, received) = tokio::time::timeout(
        Duration::from_secs(60),
        futures_util::future::join(sender, receiver),
    )
    .await
    .map_err(|_| "the transfer did not finish in time".to_string())?;

    sent.map_err(|error| error.to_string())??;
    received.map_err(|error| error.to_string())??;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transfers_a_single_file_between_two_terminals() {
    let origin = spawn_relay().await;
    let base = scratch("file");

    // Larger than one chunk, and deliberately not a chunk multiple.
    let contents: Vec<u8> = (0..(3 * 1024 * 1024 + 12_345))
        .map(|index| (index % 251) as u8)
        .collect();
    let source = base.join("source/report.bin");
    write_file(&source, &contents);

    let destination = base.join("destination");

    transfer(&origin, &source, &destination, None)
        .await
        .expect("transfer should succeed");

    assert_eq!(
        fs::read(destination.join("report.bin")).expect("received file"),
        contents,
        "the received bytes must match the sent bytes exactly"
    );

    fs::remove_dir_all(&base).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transfers_a_folder_and_extracts_it_on_the_other_side() {
    let origin = spawn_relay().await;
    let base = scratch("folder");
    let source = base.join("project");

    write_file(&source.join("README.md"), b"# project\n");
    write_file(&source.join("src/main.rs"), b"fn main() {}\n");
    write_file(&source.join("assets/blob.bin"), &vec![42_u8; 700_000]);
    write_file(&source.join("src/nested/deep/value.txt"), b"nested value");
    fs::create_dir_all(source.join("empty")).expect("empty directory");

    let destination = base.join("destination");

    transfer(&origin, &source, &destination, None)
        .await
        .expect("folder transfer should succeed");

    let extracted = destination.join("project");
    assert_eq!(
        fs::read_to_string(extracted.join("README.md")).expect("readme"),
        "# project\n"
    );
    assert_eq!(
        fs::read_to_string(extracted.join("src/main.rs")).expect("main.rs"),
        "fn main() {}\n"
    );
    assert_eq!(
        fs::read(extracted.join("assets/blob.bin")).expect("blob"),
        vec![42_u8; 700_000]
    );
    assert_eq!(
        fs::read_to_string(extracted.join("src/nested/deep/value.txt")).expect("nested"),
        "nested value"
    );
    assert!(extracted.join("empty").is_dir());

    fs::remove_dir_all(&base).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transfers_a_compressed_folder_and_restores_it() {
    let origin = spawn_relay().await;
    let base = scratch("compressed");
    let source = base.join("docs");

    // Highly compressible, so the spooled payload is meaningfully smaller than
    // the archive it was built from.
    write_file(
        &source.join("notes.txt"),
        "drop ".repeat(200_000).as_bytes(),
    );
    write_file(&source.join("more/notes.txt"), b"short");

    let destination = base.join("destination");

    transfer(&origin, &source, &destination, Some(6))
        .await
        .expect("compressed transfer should succeed");

    let extracted = destination.join("docs");
    assert_eq!(
        fs::read_to_string(extracted.join("notes.txt")).expect("notes"),
        "drop ".repeat(200_000)
    );
    assert_eq!(
        fs::read_to_string(extracted.join("more/notes.txt")).expect("nested notes"),
        "short"
    );

    fs::remove_dir_all(&base).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reports_a_clear_error_for_an_unknown_code() {
    let origin = spawn_relay().await;
    let base = scratch("badcode");

    // Well formed, so it gets past the client-side parse and is actually put
    // to the relay — which is the path this test exists to cover.
    let error = recv::run(
        "ZZZZZZ-abandon-ability-able",
        ReceiveOptions {
            path: drop_cli::direct::Path::Relay,
            status: false,
            rendezvous: drop_cli::direct::Rendezvous::default(),
            origin: Some(origin.clone()),
            out_dir: base.clone(),
            extract: true,
            force: true,
            acceptance: drop_cli::consent::Acceptance::Yes,
        },
    )
    .await
    .expect_err("an unknown code must fail");

    assert!(
        error.to_string().contains("invalid session code"),
        "unexpected error: {error}"
    );

    fs::remove_dir_all(&base).ok();
}

/// A code that cannot be a code is rejected here rather than spent against the
/// relay. It matters because a session is consumed by the first receiver to
/// claim it: a typo that reached the relay would burn the transfer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_malformed_code_fails_before_the_relay_is_contacted() {
    let base = scratch("malformedcode");

    let error = recv::run(
        "7F2A91-abandon-ability-frobnicate",
        ReceiveOptions {
            path: drop_cli::direct::Path::Relay,
            status: false,
            rendezvous: drop_cli::direct::Rendezvous::default(),
            // Nothing is listening here. Reaching it at all is the failure.
            origin: Some("http://127.0.0.1:1".to_string()),
            out_dir: base.clone(),
            extract: true,
            force: true,
            acceptance: drop_cli::consent::Acceptance::Yes,
        },
    )
    .await
    .expect_err("a malformed code must fail");

    assert!(
        error.to_string().contains("frobnicate"),
        "unexpected error: {error}"
    );

    fs::remove_dir_all(&base).ok();
}

/// Builds a ustar archive from `(name, typeflag, link_target, contents)`.
///
/// Unix only because its one caller is: the hostile archive it builds plants
/// symlinks, and the receiver cannot create those on Windows.
#[cfg(unix)]
fn archive_of(entries: &[(&str, u8, &str, &[u8])]) -> Vec<u8> {
    let mut archive = Vec::new();

    for (name, typeflag, link_target, contents) in entries {
        let mut block = [0_u8; 512];

        block[..name.len()].copy_from_slice(name.as_bytes());
        block[100..108].copy_from_slice(b"0000755\0");
        block[108..116].copy_from_slice(b"0000000\0");
        block[116..124].copy_from_slice(b"0000000\0");

        let size = format!("{:011o}\0", contents.len());
        block[124..136].copy_from_slice(size.as_bytes());
        block[136..148].copy_from_slice(b"00000000000\0");
        block[156] = *typeflag;
        block[157..157 + link_target.len()].copy_from_slice(link_target.as_bytes());
        block[257..263].copy_from_slice(b"ustar\0");
        block[263..265].copy_from_slice(b"00");

        block[148..156].fill(b' ');
        let checksum: u32 = block.iter().map(|byte| u32::from(*byte)).sum();
        let encoded = format!("{checksum:06o}");
        block[148..148 + encoded.len()].copy_from_slice(encoded.as_bytes());
        block[154] = 0;
        block[155] = b' ';

        archive.extend_from_slice(&block);

        if !contents.is_empty() {
            let mut padded = contents.to_vec();
            padded.resize(contents.len().div_ceil(512) * 512, 0);
            archive.extend_from_slice(&padded);
        }
    }

    archive.extend_from_slice(&[0_u8; 1024]);
    archive
}

/// The whole attack, over the wire: a hostile `.tar` is an ordinary payload as
/// far as the sender is concerned, and the receiver unpacks it on arrival.
///
/// Every entry name is lexically clean, so this only fails to escape because
/// the extractor checks the paths against what is on disk.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[cfg(unix)]
async fn a_hostile_archive_cannot_write_outside_the_receivers_directory() {
    let origin = spawn_relay().await;
    let base = scratch("hostile-e2e");

    let source = base.join("payload.tar");
    write_file(
        &source,
        &archive_of(&[
            ("arch/", b'5', "", b"".as_slice()),
            // A benign entry, so a test that passes because nothing was
            // extracted at all is distinguishable from one that passes because
            // the hostile entries were refused.
            ("arch/safe.txt", b'0', "", b"fine".as_slice()),
            ("arch/a", b'2', ".", b"".as_slice()),
            ("arch/a/b", b'2', "../..", b"".as_slice()),
            ("arch/a/b/pwned.txt", b'0', "", b"OWNED".as_slice()),
        ]),
    );

    let destination = base.join("destination");
    fs::create_dir_all(&destination).expect("destination");

    // As above: the subject here is where the bytes landed, asserted below
    // against the filesystem. The completion handshake is covered by the
    // transfers above and is not re-asserted, so a socket reset during relay
    // teardown cannot turn a passing security check into a red build.
    let outcome = transfer(&origin, &source, &destination, None).await;

    assert!(
        !base.join("pwned.txt").exists(),
        "a hostile archive must not write outside the receiver's directory: {outcome:?}"
    );
    assert!(
        !destination
            .parent()
            .is_some_and(|parent| parent.join("pwned.txt").exists()),
        "nothing may be written above the destination"
    );
    assert_eq!(
        fs::read_to_string(destination.join("arch/safe.txt")).expect("the safe entry"),
        "fine",
        "the harmless entry must still be extracted: {outcome:?}"
    );

    fs::remove_dir_all(&base).ok();
}

/// A second copy of the same file lands beside the first rather than replacing
/// it or failing the transfer outright.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_colliding_file_is_numbered_rather_than_refused() {
    let base = scratch("collide-e2e");

    let source = base.join("outgoing");
    write_file(&source.join("report.pdf"), b"from the sender");
    let file = source.join("report.pdf");

    let destination = base.join("destination");

    let origin = spawn_relay().await;
    transfer_forcing(&origin, &file, &destination, None, false)
        .await
        .expect("the first transfer should succeed");

    // This used to abort the receiver before a byte moved, which dropped the
    // socket and told the sender its peer had disconnected.
    let origin = spawn_relay().await;
    transfer_forcing(&origin, &file, &destination, None, false)
        .await
        .expect("a name collision must not fail the transfer");

    assert_eq!(
        fs::read_to_string(destination.join("report.pdf")).expect("the first copy"),
        "from the sender",
        "the file already on disk must be left alone"
    );
    assert_eq!(
        fs::read_to_string(destination.join("report-1.pdf")).expect("the numbered copy"),
        "from the sender",
        "the second copy must land beside it"
    );

    fs::remove_dir_all(&base).ok();
}

/// The receiver's existing files survive a transfer that did not ask to
/// replace them, and are replaced when it did.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extraction_replaces_existing_files_only_when_forced() {
    let base = scratch("overwrite-e2e");

    let source = base.join("project");
    write_file(&source.join("notes.txt"), b"from the sender");

    let destination = base.join("destination");
    write_file(
        &destination.join("project/notes.txt"),
        b"mine, already here",
    );

    // What this test owns is the overwrite decision, which is checked below
    // against what is actually on disk. Whether the sender gets a clean
    // completion handshake is owned by the transfers above, and is deliberately
    // not re-asserted here: the relay can reset a socket during teardown under
    // load, and failing this test for that would be testing the wrong thing.
    // Each transfer gets its own relay so they do not contend for one.
    let origin = spawn_relay().await;
    let unforced = transfer_forcing(&origin, &source, &destination, None, false).await;

    assert_eq!(
        fs::read_to_string(destination.join("project/notes.txt")).expect("read"),
        "mine, already here",
        "the receiver's own file must survive a transfer that did not force: {unforced:?}"
    );

    let origin = spawn_relay().await;
    let forced = transfer_forcing(&origin, &source, &destination, None, true).await;

    assert_eq!(
        fs::read_to_string(destination.join("project/notes.txt")).expect("read"),
        "from the sender",
        "--force must replace the file: {forced:?}"
    );

    fs::remove_dir_all(&base).ok();
}

/// The property the short code depends on: a receiver who mistypes a word
/// derives a different key, cannot open the transfer details, and writes
/// nothing. It also does not get to try again — the relay consumed the session
/// when this receiver claimed it — which is what holds an attacker to a single
/// online guess against the words.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wrong_code_is_refused_and_leaves_nothing_on_disk() {
    let origin = spawn_relay().await;
    let base = scratch("wrongcode");

    let source = base.join("source/secret.bin");
    write_file(&source, &vec![7_u8; 64 * 1024]);
    let destination = base.join("destination");

    let (code_tx, code_rx) = oneshot::channel();
    let mut code_tx = Some(code_tx);

    let sender = tokio::spawn({
        let origin = origin.clone();
        let source = source.clone();

        async move {
            send::run(
                &source,
                SendOptions {
                    origin: Some(origin),
                    compress: None,
                    path: drop_cli::direct::Path::Relay,
                    status: false,
                    rendezvous: drop_cli::direct::Rendezvous::default(),
                    on_code: Box::new(move |code| {
                        if let Some(sender) = code_tx.take() {
                            let _ = sender.send(code.to_string());
                        }
                    }),
                },
            )
            .await
            .map_err(|error| error.to_string())
        }
    });

    let code = tokio::time::timeout(Duration::from_secs(10), code_rx)
        .await
        .expect("the sender never produced a session code")
        .expect("the sender failed before producing a session code");

    // Keep the nameplate, so the receiver reaches the right session, and
    // change one word so only the password is wrong.
    let mut parts: Vec<String> = code.split('-').map(str::to_string).collect();
    let last = parts.len() - 1;
    parts[last] = if parts[last] == "zoo" { "zebra" } else { "zoo" }.to_string();
    let wrong_code = parts.join("-");
    assert_ne!(wrong_code, code, "the mangled code must actually differ");

    let error = recv::run(
        &wrong_code,
        ReceiveOptions {
            path: drop_cli::direct::Path::Relay,
            status: false,
            rendezvous: drop_cli::direct::Rendezvous::default(),
            origin: Some(origin.clone()),
            out_dir: destination.clone(),
            extract: true,
            force: true,
            acceptance: drop_cli::consent::Acceptance::Yes,
        },
    )
    .await
    .expect_err("a wrong code must fail");

    assert!(
        error.to_string().contains("check the code"),
        "unexpected error: {error}"
    );

    // Nothing was written, because the failure happens on the sealed transfer
    // details — before a destination file is opened.
    let leftovers: Vec<_> = fs::read_dir(&destination)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "a refused transfer left files behind: {leftovers:?}"
    );

    let _ = sender.await;
    fs::remove_dir_all(&base).ok();
}

/// A transfer where the receiver claims its socket before the sender claims
/// its own.
///
/// Both orders are allowed, and this one was broken. The receiver sent its half
/// of the key exchange on connect; the relay had no sender to give it to and
/// dropped it, since a key exchange is forwarded and never held; and the sender
/// then waited for a half that no longer existed, with no error on either side.
/// The other transfer tests race the two connections and the sender almost
/// always wins, which is why this survived. Here the sender is held at its code
/// callback until the relay has actually recorded the receiver, so the order is
/// pinned rather than hoped for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_receiver_that_connects_first_still_completes_the_transfer() {
    let (origin, state) = spawn_relay_with_state().await;
    let base = scratch("receiver-first");

    let contents: Vec<u8> = (0..(64 * 1024 + 7))
        .map(|index| (index % 251) as u8)
        .collect();
    let source = base.join("source/early.bin");
    write_file(&source, &contents);
    let destination = base.join("destination");

    let (code_tx, code_rx) = oneshot::channel();
    let mut code_tx = Some(code_tx);
    // Blocking, not async, because `on_code` is a synchronous callback. It
    // parks the sender's worker thread, which the runtime has three more of.
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();

    let sender = tokio::spawn({
        let origin = origin.clone();
        let source = source.clone();

        async move {
            send::run(
                &source,
                SendOptions {
                    origin: Some(origin),
                    compress: None,
                    path: drop_cli::direct::Path::Relay,
                    status: false,
                    rendezvous: drop_cli::direct::Rendezvous::default(),
                    on_code: Box::new(move |code| {
                        if let Some(sender) = code_tx.take() {
                            let _ = sender.send(code.to_string());
                        }

                        let _ = release_rx.recv();
                    }),
                },
            )
            .await
            .map_err(|error| error.to_string())
        }
    });

    let code = tokio::time::timeout(Duration::from_secs(10), code_rx)
        .await
        .expect("the sender never produced a session code")
        .expect("the sender failed before producing a session code");

    let nameplate = code
        .split('-')
        .next()
        .expect("a code always has a nameplate")
        .to_string();

    let receiver = tokio::spawn({
        let origin = origin.clone();
        let destination = destination.clone();

        async move {
            recv::run(
                &code,
                ReceiveOptions {
                    path: drop_cli::direct::Path::Relay,
                    status: false,
                    rendezvous: drop_cli::direct::Rendezvous::default(),
                    origin: Some(origin),
                    out_dir: destination,
                    extract: true,
                    force: true,
                    acceptance: drop_cli::consent::Acceptance::Yes,
                },
            )
            .await
            .map_err(|error| error.to_string())
        }
    });

    let mut connected = false;
    for _ in 0..500 {
        if state
            .sessions
            .get(&nameplate)
            .await
            .is_some_and(|session| session.receiver_connected)
        {
            connected = true;
            break;
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(connected, "the receiver never claimed its socket");

    release_tx.send(()).expect("the sender is still waiting");

    let (sent, received) = tokio::time::timeout(
        Duration::from_secs(60),
        futures_util::future::join(sender, receiver),
    )
    .await
    .expect("the transfer did not finish in time");

    sent.expect("the sender task panicked")
        .expect("send should succeed");
    received
        .expect("the receiver task panicked")
        .expect("receive should succeed");

    assert_eq!(
        fs::read(destination.join("early.bin")).expect("received file"),
        contents,
        "the received bytes must match the sent bytes exactly"
    );

    fs::remove_dir_all(&base).ok();
}

/// The machine-readable carrier line, asserted against the real binary.
///
/// This is the one test here that spawns `drop` as a process rather than
/// calling into the library, and it is deliberate. What the network lab in
/// `netlab/` consumes is a line on the **stderr of a subprocess**, so an
/// in-process assertion would pin the string while leaving every step between
/// it and a shell — the flag parsing, the option plumbing, the stream it is
/// written to — unchecked. The unit tests in `direct.rs` pin the wording; this
/// pins that the wording reaches a program that spawned the binary.
///
/// `--transport relay` for the same reason the fixtures above use it: this
/// drives a real relay in-process and must not reach for a DHT.
///
/// The sender is asked with the flag and the receiver with `DROP_STATUS`, so
/// one transfer covers both ways of asking. A harness exporting the variable
/// once and spawning many `drop` processes is the case the variable exists
/// for, and it would be embarrassing for it to be the untested one.
#[tokio::test]
async fn the_carrier_line_reaches_a_program_that_spawned_the_binary() {
    let origin = spawn_relay().await;
    let base = scratch("status-line");
    let source = base.join("payload.bin");
    let destination = base.join("received");
    fs::create_dir_all(&destination).expect("destination directory");

    let contents = b"a synthetic payload, carried by a relay that cannot read it";
    write_file(&source, contents);

    let mut sender = tokio::process::Command::new(env!("CARGO_BIN_EXE_drop"))
        .args([
            "send",
            source.to_str().expect("a printable source path"),
            "--server",
            &origin,
            "--transport",
            "relay",
            "--status",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the sender");

    // The code is the one thing on stdout, so it can be read without waiting
    // for the process — which has not started transferring yet and will not
    // until a receiver arrives with this code.
    let mut announced = BufReader::new(sender.stdout.take().expect("the sender's stdout")).lines();
    let code = tokio::time::timeout(Duration::from_secs(30), announced.next_line())
        .await
        .expect("the sender announced a code in time")
        .expect("reading the sender's stdout")
        .expect("the sender announced a code at all");

    let receiver = tokio::process::Command::new(env!("CARGO_BIN_EXE_drop"))
        .args([
            "recv",
            &code,
            "--yes",
            "--server",
            &origin,
            "--transport",
            "relay",
            "--out",
            destination.to_str().expect("a printable destination path"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("DROP_STATUS", "1")
        .spawn()
        .expect("spawn the receiver")
        .wait_with_output();

    let receiver = tokio::time::timeout(Duration::from_secs(60), receiver)
        .await
        .expect("the receiver finished in time")
        .expect("the receiver ran");

    let sender = tokio::time::timeout(Duration::from_secs(60), sender.wait_with_output())
        .await
        .expect("the sender finished in time")
        .expect("the sender ran");

    let sent = String::from_utf8_lossy(&sender.stderr);
    let received = String::from_utf8_lossy(&receiver.stderr);

    assert!(sender.status.success(), "the sender failed:\n{sent}");
    assert!(
        receiver.status.success(),
        "the receiver failed:\n{received}"
    );

    // Nothing was fallen back from: the relay is what `--transport relay`
    // asked for, and saying `rendezvous` here would be the line reporting a
    // failure that did not happen.
    let expected = "drop-status: path=relay fallback=none";
    assert!(
        sent.lines().any(|line| line == expected),
        "the sender did not report its carrier:\n{sent}"
    );
    assert!(
        received.lines().any(|line| line == expected),
        "the receiver did not report its carrier:\n{received}"
    );

    assert_eq!(
        fs::read(destination.join("payload.bin")).expect("the received file"),
        contents,
        "the payload did not survive the transfer this line describes"
    );
}

/// Off unless asked for, because the ordinary output is the product.
///
/// Paired with the test above rather than folded into it: together they say
/// the flag is what turns the line on, which neither says alone.
#[tokio::test]
async fn the_carrier_line_stays_out_of_an_ordinary_transfer() {
    let origin = spawn_relay().await;
    let base = scratch("no-status-line");
    let source = base.join("payload.bin");
    let destination = base.join("received");
    fs::create_dir_all(&destination).expect("destination directory");
    write_file(&source, b"a synthetic payload");

    let mut sender = tokio::process::Command::new(env!("CARGO_BIN_EXE_drop"))
        .args([
            "send",
            source.to_str().expect("a printable source path"),
            "--server",
            &origin,
            "--transport",
            "relay",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("DROP_STATUS")
        .spawn()
        .expect("spawn the sender");

    let mut announced = BufReader::new(sender.stdout.take().expect("the sender's stdout")).lines();
    let code = tokio::time::timeout(Duration::from_secs(30), announced.next_line())
        .await
        .expect("the sender announced a code in time")
        .expect("reading the sender's stdout")
        .expect("the sender announced a code at all");

    let receiver = tokio::process::Command::new(env!("CARGO_BIN_EXE_drop"))
        .args([
            "recv",
            &code,
            "--yes",
            "--server",
            &origin,
            "--transport",
            "relay",
            "--out",
            destination.to_str().expect("a printable destination path"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("DROP_STATUS")
        .spawn()
        .expect("spawn the receiver")
        .wait_with_output();

    let receiver = tokio::time::timeout(Duration::from_secs(60), receiver)
        .await
        .expect("the receiver finished in time")
        .expect("the receiver ran");

    let sender = tokio::time::timeout(Duration::from_secs(60), sender.wait_with_output())
        .await
        .expect("the sender finished in time")
        .expect("the sender ran");

    let sent = String::from_utf8_lossy(&sender.stderr);
    let received = String::from_utf8_lossy(&receiver.stderr);

    assert!(sender.status.success(), "the sender failed:\n{sent}");
    assert!(
        receiver.status.success(),
        "the receiver failed:\n{received}"
    );

    assert!(
        !sent.contains("drop-status:"),
        "the sender printed a machine line nobody asked for:\n{sent}"
    );
    assert!(
        !received.contains("drop-status:"),
        "the receiver printed a machine line nobody asked for:\n{received}"
    );

    // The prose is what a person gets, and it must still be there.
    assert!(
        sent.contains("Path    relay"),
        "the sender stopped saying which path it took:\n{sent}"
    );
}

/// A name the sender chose cannot run on the receiver's terminal.
///
/// The name carries a clear-line-and-move-up sequence and a right-to-left
/// override. Before `display` existed, the receiver's "Receiving" line printed
/// both verbatim, before the receiver had agreed to anything.
///
/// Through the real binary and a pipe, because a pipe is what makes the
/// assertion meaningful: the progress line only writes escape sequences when
/// stderr is a terminal, so any escape found here came from the name.
///
/// Unix only. Windows refuses control characters in file names, so there is
/// no way to create the fixture there, and a Windows sender cannot produce
/// this name either.
#[cfg(unix)]
#[tokio::test]
async fn a_hostile_file_name_cannot_write_escape_sequences_to_the_receiver() {
    let origin = spawn_relay().await;
    let base = scratch("hostile-name");
    let hostile_name = "report\x1b[2K\x1b[1A\u{202E}fdp.exe";
    let source = base.join(hostile_name);
    let destination = base.join("received");
    fs::create_dir_all(&destination).expect("destination directory");
    write_file(&source, b"not what the name says");

    let mut sender = tokio::process::Command::new(env!("CARGO_BIN_EXE_drop"))
        .arg("send")
        .arg(&source)
        .args(["--server", &origin, "--transport", "relay"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the sender");

    let mut announced = BufReader::new(sender.stdout.take().expect("the sender's stdout")).lines();
    let code = tokio::time::timeout(Duration::from_secs(30), announced.next_line())
        .await
        .expect("the sender announced a code in time")
        .expect("reading the sender's stdout")
        .expect("the sender announced a code at all");

    let receiver = tokio::process::Command::new(env!("CARGO_BIN_EXE_drop"))
        .args([
            "recv",
            &code,
            "--yes",
            "--server",
            &origin,
            "--transport",
            "relay",
        ])
        .arg("--out")
        .arg(&destination)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the receiver")
        .wait_with_output();

    let receiver = tokio::time::timeout(Duration::from_secs(60), receiver)
        .await
        .expect("the receiver finished in time")
        .expect("the receiver ran");
    let sender = tokio::time::timeout(Duration::from_secs(60), sender.wait_with_output())
        .await
        .expect("the sender finished in time")
        .expect("the sender ran");

    let sent = String::from_utf8_lossy(&sender.stderr);
    let received = String::from_utf8_lossy(&receiver.stderr);

    assert!(sender.status.success(), "the sender failed:\n{sent}");
    assert!(
        receiver.status.success(),
        "the receiver failed:\n{received}"
    );

    for (who, output) in [("receiver", &received), ("sender", &sent)] {
        assert!(
            !output.contains('\x1b'),
            "an escape sequence from the name reached the {who}'s terminal:\n{output:?}"
        );
        assert!(
            !output.contains('\u{202E}'),
            "a right-to-left override from the name reached the {who}'s terminal:\n{output:?}"
        );
    }

    assert!(
        received.contains("fdp.exe"),
        "the name should still be shown, only neutralised:\n{received}"
    );

    // The file itself keeps the name the sender gave it. Displaying a name
    // safely is not the same as renaming a file, and this test is about the
    // first.
    assert_eq!(
        fs::read(destination.join(hostile_name)).expect("the received file"),
        b"not what the name says"
    );
}

/// Says no to whatever it is shown, and remembers what that was.
struct Declines {
    shown: std::sync::Arc<std::sync::Mutex<Option<drop_cli::consent::Preview>>>,
}

impl drop_cli::consent::ConsentPrompt for Declines {
    async fn decide(&mut self, preview: &drop_cli::consent::Preview) -> drop_cli::consent::Consent {
        *self.shown.lock().expect("not poisoned") = Some(preview.clone());
        drop_cli::consent::Consent::Decline
    }
}

/// The whole point of consent, over a real relay: the receiver is shown what
/// is coming, says no, and nothing about its directory changes. The sender
/// hears a decline, not a broken connection, and the relay does not count it
/// as a failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn declining_writes_nothing_and_tells_the_sender() {
    let (origin, relay) = spawn_relay_with_state().await;
    let base = scratch("decline");
    let source = base.join("quarterly-report.pdf");
    let destination = base.join("received");
    write_file(&source, &vec![5_u8; 70_000]);
    // Already there, so a careless decline path that numbered or truncated a
    // file would show up.
    write_file(
        &destination.join("quarterly-report.pdf"),
        b"the receiver's own",
    );

    let before: Vec<_> = fs::read_dir(&destination)
        .expect("list")
        .map(|entry| entry.expect("entry").path())
        .collect();

    let (code_tx, code_rx) = oneshot::channel();
    let mut code_tx = Some(code_tx);
    let sender = tokio::spawn({
        let origin = origin.clone();
        let source = source.clone();
        async move {
            send::run(
                &source,
                SendOptions {
                    origin: Some(origin),
                    compress: None,
                    path: drop_cli::direct::Path::Relay,
                    status: false,
                    rendezvous: drop_cli::direct::Rendezvous::default(),
                    on_code: Box::new(move |code| {
                        if let Some(sender) = code_tx.take() {
                            let _ = sender.send(code.to_string());
                        }
                    }),
                },
            )
            .await
            .map_err(|error| error.to_string())
        }
    });

    let code = tokio::time::timeout(Duration::from_secs(10), code_rx)
        .await
        .expect("a code in time")
        .expect("a code at all");

    let shown = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut prompt = Declines {
        shown: shown.clone(),
    };
    let received = recv::run_deciding(
        &code,
        &ReceiveOptions {
            path: drop_cli::direct::Path::Relay,
            status: false,
            rendezvous: drop_cli::direct::Rendezvous::default(),
            origin: Some(origin),
            out_dir: destination.clone(),
            extract: true,
            force: false,
            // Not consulted: `run_deciding` puts the question to the prompt
            // passed below instead.
            acceptance: drop_cli::consent::Acceptance::Ask,
        },
        &mut prompt,
        drop_cli::cancel::Cancel::default(),
    )
    .await;

    assert!(received.is_ok(), "declining is not a failure: {received:?}");

    let sent = tokio::time::timeout(Duration::from_secs(30), sender)
        .await
        .expect("the sender finished")
        .expect("the sender task ran");
    let error = sent.expect_err("a declined transfer did not happen");
    assert!(
        error.contains("declined"),
        "the sender should say so: {error}"
    );

    let preview = shown
        .lock()
        .expect("not poisoned")
        .clone()
        .expect("a preview was shown");
    assert_eq!(preview.name, "quarterly-report.pdf");
    assert_eq!(preview.size, 70_000);
    let drop_cli::consent::Landing::File {
        path, instead_of, ..
    } = preview.landing
    else {
        panic!("a single file should land as a file");
    };
    assert!(
        path.ends_with("quarterly-report-1.pdf"),
        "the preview should show the numbered name it would have used: {path}"
    );
    assert_eq!(instead_of.as_deref(), Some("quarterly-report.pdf"));

    let after: Vec<_> = fs::read_dir(&destination)
        .expect("list")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert_eq!(
        before, after,
        "declining must leave the destination as it was"
    );
    assert_eq!(
        fs::read(destination.join("quarterly-report.pdf")).expect("read"),
        b"the receiver's own"
    );

    for _ in 0..100 {
        if relay.metrics.snapshot().total_transfers_declined == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let metrics = relay.metrics.snapshot();
    assert_eq!(metrics.total_transfers_declined, 1);
    assert_eq!(metrics.total_transfer_failures, 0);
}

/// With nobody at a terminal to ask and no `--yes`, the receiver refuses
/// before contacting anything, so the sender's code is not spent on a question
/// nobody could answer.
#[tokio::test]
async fn a_receiver_with_no_terminal_and_no_yes_refuses_before_connecting() {
    // A listener standing in for a relay, so a connection attempt would be
    // seen rather than inferred from an error message.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let origin = format!("http://{}", listener.local_addr().expect("address"));

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_drop"))
        .args([
            "recv",
            "A1B2C3-abandon-ability-able",
            "--server",
            &origin,
            "--transport",
            "relay",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    let output = tokio::time::timeout(Duration::from_secs(30), output)
        .await
        .expect("the receiver finished in time")
        .expect("the receiver ran");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "it should refuse:\n{stderr}");
    assert!(
        stderr.contains("--yes"),
        "it should name the way out:\n{stderr}"
    );

    let contacted = tokio::time::timeout(Duration::from_millis(200), listener.accept()).await;
    assert!(
        contacted.is_err(),
        "the receiver contacted the relay before refusing"
    );
}

/// Starts a relayed send of `size` bytes that can be cancelled, and returns
/// the code with the sender's task.
async fn cancellable_send(
    origin: &str,
    source: &Path,
    cancel: drop_cli::cancel::Cancel,
) -> (
    String,
    tokio::task::JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>>,
) {
    let (code_tx, code_rx) = oneshot::channel();
    let mut code_tx = Some(code_tx);
    let sender = tokio::spawn({
        let origin = origin.to_string();
        let source = source.to_path_buf();
        async move {
            send::run_cancellable(
                &source,
                SendOptions {
                    origin: Some(origin),
                    compress: None,
                    path: drop_cli::direct::Path::Relay,
                    status: false,
                    rendezvous: drop_cli::direct::Rendezvous::default(),
                    on_code: Box::new(move |code| {
                        if let Some(sender) = code_tx.take() {
                            let _ = sender.send(code.to_string());
                        }
                    }),
                },
                cancel,
            )
            .await
        }
    });

    let code = tokio::time::timeout(Duration::from_secs(10), code_rx)
        .await
        .expect("a code in time")
        .expect("a code at all");

    (code, sender)
}

fn relayed_receive(origin: &str, destination: &Path) -> ReceiveOptions {
    ReceiveOptions {
        path: drop_cli::direct::Path::Relay,
        status: false,
        rendezvous: drop_cli::direct::Rendezvous::default(),
        origin: Some(origin.to_string()),
        out_dir: destination.to_path_buf(),
        extract: true,
        force: false,
        acceptance: drop_cli::consent::Acceptance::Yes,
    }
}

/// A file of `len` zero bytes that takes no disk space.
///
/// The cancel tests need a transfer that is certainly still running when the
/// cancel lands. A real 48 MiB file moves in a few hundred milliseconds here,
/// which once let the last byte arrive before the cancel did. A gibibyte cannot,
/// and a sparse one costs nothing to create.
fn sparse_file(path: &Path, len: u64) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent directory");
    }
    fs::File::create(path)
        .and_then(|file| file.set_len(len))
        .expect("sparse fixture");
}

/// Waits until a file has started arriving, so a cancel lands mid-transfer
/// rather than before it or after it.
async fn wait_until_partly_written(path: &Path) {
    for _ in 0..500 {
        if fs::metadata(path).is_ok_and(|metadata| metadata.len() > 0) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{} never started arriving", path.display());
}

/// A receiver that stops half-way tells the sender, which ends saying the
/// receiver cancelled rather than that a connection dropped. The half-written
/// file does not survive to be mistaken for the whole one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_receiver_cancelling_mid_transfer_tells_the_sender_and_keeps_nothing() {
    let (origin, relay) = spawn_relay_with_state().await;
    let base = scratch("receiver-cancels");
    let source = base.join("large.bin");
    let destination = base.join("received");
    fs::create_dir_all(&destination).expect("destination");
    sparse_file(&source, 1024 * 1024 * 1024);

    let (code, sender) =
        cancellable_send(&origin, &source, drop_cli::cancel::Cancel::default()).await;

    let receiver_cancel = drop_cli::cancel::Cancel::default();
    let receiver = tokio::spawn({
        let origin = origin.clone();
        let destination = destination.clone();
        let cancel = receiver_cancel.clone();
        async move {
            recv::run_deciding(
                &code,
                &relayed_receive(&origin, &destination),
                &mut drop_cli::consent::Acceptance::Yes,
                cancel,
            )
            .await
            .map_err(|error| drop_cli::cancel::exit_code_for(error.as_ref()))
        }
    });

    wait_until_partly_written(&destination.join("large.bin")).await;
    receiver_cancel.fire();

    let received = tokio::time::timeout(Duration::from_secs(10), receiver)
        .await
        .expect("the receiver stopped promptly")
        .expect("the receiver task ran");
    assert_eq!(received, Err(130), "cancelled here");

    let sent = tokio::time::timeout(Duration::from_secs(10), sender)
        .await
        .expect("the sender stopped promptly")
        .expect("the sender task ran");
    let error = sent.expect_err("a cancelled transfer did not complete");
    assert_eq!(
        drop_cli::cancel::exit_code_for(error.as_ref()),
        4,
        "{error}"
    );
    assert!(
        error.to_string().contains("receiver cancelled"),
        "the sender should say who stopped it: {error}"
    );

    assert!(
        !destination.join("large.bin").exists(),
        "a half-written file was left behind"
    );

    for _ in 0..100 {
        if relay.metrics.snapshot().total_transfers_cancelled == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(relay.metrics.snapshot().total_transfers_cancelled, 1);
    assert_eq!(relay.metrics.snapshot().total_transfer_failures, 0);
}

/// The same from the other side: a sender that stops half-way is reported to
/// the receiver as the sender cancelling, and the receiver keeps nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sender_cancelling_mid_transfer_tells_the_receiver_and_it_keeps_nothing() {
    let origin = spawn_relay().await;
    let base = scratch("sender-cancels");
    let source = base.join("large.bin");
    let destination = base.join("received");
    fs::create_dir_all(&destination).expect("destination");
    sparse_file(&source, 1024 * 1024 * 1024);

    let sender_cancel = drop_cli::cancel::Cancel::default();
    let (code, sender) = cancellable_send(&origin, &source, sender_cancel.clone()).await;

    let receiver = tokio::spawn({
        let origin = origin.clone();
        let destination = destination.clone();
        async move {
            recv::run_deciding(
                &code,
                &relayed_receive(&origin, &destination),
                &mut drop_cli::consent::Acceptance::Yes,
                drop_cli::cancel::Cancel::default(),
            )
            .await
        }
    });

    wait_until_partly_written(&destination.join("large.bin")).await;
    sender_cancel.fire();

    let sent = tokio::time::timeout(Duration::from_secs(10), sender)
        .await
        .expect("the sender stopped promptly")
        .expect("the sender task ran");
    let error = sent.expect_err("cancelled");
    assert_eq!(drop_cli::cancel::exit_code_for(error.as_ref()), 130);

    let received = tokio::time::timeout(Duration::from_secs(10), receiver)
        .await
        .expect("the receiver stopped promptly")
        .expect("the receiver task ran");
    let error = received.expect_err("a cancelled transfer did not complete");
    assert_eq!(
        drop_cli::cancel::exit_code_for(error.as_ref()),
        4,
        "{error}"
    );
    assert!(error.to_string().contains("sender cancelled"), "{error}");

    assert!(
        !destination.join("large.bin").exists(),
        "a half-written file was left behind"
    );
}

/// Ctrl-C, for real: a signal to a running `drop recv` makes it tell the
/// sender and exit 130, and the sender exits 4 saying the receiver cancelled.
/// Before this, the receiver died without a word and the sender reported a
/// dropped connection.
///
/// Unix only, because it sends SIGINT with `kill`. On Windows the same code
/// path is reached by `tokio::signal::ctrl_c`, which cannot be delivered to one
/// child process from a test.
#[cfg(unix)]
#[tokio::test]
async fn an_interrupt_cancels_politely_and_both_sides_say_so() {
    let origin = spawn_relay().await;
    let base = scratch("interrupt");
    let source = base.join("large.bin");
    let destination = base.join("received");
    fs::create_dir_all(&destination).expect("destination");
    sparse_file(&source, 1024 * 1024 * 1024);

    let mut sender = tokio::process::Command::new(env!("CARGO_BIN_EXE_drop"))
        .arg("send")
        .arg(&source)
        .args(["--server", &origin, "--transport", "relay", "--status"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the sender");

    let mut announced = BufReader::new(sender.stdout.take().expect("stdout")).lines();
    let code = tokio::time::timeout(Duration::from_secs(30), announced.next_line())
        .await
        .expect("a code in time")
        .expect("stdout readable")
        .expect("a code at all");

    let mut receiver = tokio::process::Command::new(env!("CARGO_BIN_EXE_drop"))
        .args([
            "recv",
            &code,
            "--yes",
            "--status",
            "--server",
            &origin,
            "--transport",
            "relay",
        ])
        .arg("--out")
        .arg(&destination)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the receiver");

    // Collected as it arrives, so the signal goes in once the transfer is
    // genuinely under way and the full output is still there to assert on.
    let mut receiver_lines = BufReader::new(receiver.stderr.take().expect("stderr")).lines();
    let mut received_text = String::new();
    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(line) = receiver_lines.next_line().await.expect("stderr readable") {
            received_text.push_str(&line);
            received_text.push('\n');
            if line.starts_with("drop-status: state=accepted") {
                break;
            }
        }
    })
    .await
    .expect("the receiver accepted in time");
    wait_until_partly_written(&destination.join("large.bin")).await;

    let pid = receiver.id().expect("a running receiver").to_string();
    let killed = std::process::Command::new("kill")
        .args(["-INT", &pid])
        .status()
        .expect("kill ran");
    assert!(killed.success());

    let receiver_status = tokio::time::timeout(Duration::from_secs(10), receiver.wait())
        .await
        .expect("the receiver exited promptly")
        .expect("the receiver ran");
    while let Ok(Some(line)) = receiver_lines.next_line().await {
        received_text.push_str(&line);
        received_text.push('\n');
    }

    let sender = tokio::time::timeout(Duration::from_secs(10), sender.wait_with_output())
        .await
        .expect("the sender exited promptly")
        .expect("the sender ran");
    let sent_text = String::from_utf8_lossy(&sender.stderr);

    assert_eq!(receiver_status.code(), Some(130), "{received_text}");
    assert!(received_text.contains("Cancelled."), "{received_text}");

    assert_eq!(sender.status.code(), Some(4), "{sent_text}");
    assert!(
        sent_text.contains("the receiver cancelled the transfer"),
        "{sent_text}"
    );

    let states: Vec<&str> = sent_text
        .lines()
        .filter_map(|line| line.strip_prefix("drop-status: state="))
        .collect();
    assert_eq!(
        states,
        vec!["connected", "code-ok", "accepted", "cancelled"],
        "the sender's states, in order:\n{sent_text}"
    );

    assert!(
        !destination.join("large.bin").exists(),
        "a half-written file was left behind"
    );
}
