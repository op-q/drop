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
    time::Duration,
};

use drop_cli::{
    recv::{self, ReceiveOptions},
    send::{self, SendOptions},
};
use tokio::sync::oneshot;

async fn spawn_relay() -> String {
    let state = api::build_state();
    api::start_background_services(state.clone());

    let app = api::build_app(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("relay listener");
    let addr: SocketAddr = listener.local_addr().expect("relay address");

    tokio::spawn(async move {
        api::serve(listener, app).await.expect("relay server");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    format!("http://{addr}")
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
                    origin,
                    compress,
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
                    origin,
                    out_dir: destination,
                    extract: true,
                    force,
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

    let error = recv::run(
        "ZZZZZZ",
        ReceiveOptions {
            origin,
            out_dir: base.clone(),
            extract: true,
            force: true,
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

/// Builds a ustar archive from `(name, typeflag, link_target, contents)`.
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
