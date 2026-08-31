//! The receiving half of a terminal-to-terminal transfer.

use std::{
    error::Error,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde_json::json;

use crate::{
    crypto, direct,
    payload::{GZIP_MIME, TAR_GZIP_MIME, TAR_MIME},
    progress::Progress,
    transport::{Frame, Transport, relay},
    untar::TarExtractor,
};

/// Acknowledge in batches rather than per chunk.
///
/// The acknowledgement is what releases the sender's window, so this must stay
/// well below the sender's window or the transfer would stall waiting for an
/// acknowledgement that only a later chunk would trigger.
const ACK_INTERVAL_BYTES: u64 = 4 * 1024 * 1024;

/// How much a compressed payload may expand before extraction is abandoned.
///
/// A gzip stream can expand roughly a thousandfold, so a sender that the
/// receiver has no reason to trust could turn a modest transfer into an
/// arbitrarily large write. Bounding the *ratio* rather than an absolute size
/// keeps the cost of an attack proportional to what the attacker has to push
/// through the relay, while leaving real archives room: source trees and
/// documents compress well under ten to one.
const MAX_EXPANSION_RATIO: u64 = 100;

/// Expansion allowance for a payload small enough that the ratio would be
/// needlessly strict.
const MIN_EXPANSION_ALLOWANCE: u64 = 1024 * 1024 * 1024;

/// How many numbered alternatives to try before refusing a name.
///
/// One collision is ordinary. A thousand means something is producing files
/// faster than anyone is consuming them, and counting upwards forever would
/// hide that rather than report it.
const MAX_NAME_ATTEMPTS: u32 = 1000;

/// How much compressed input is handed to the decompressor at a time.
///
/// `write_all` does not return until the whole slice is consumed, and every
/// byte it produces is buffered until it can be drained, so feeding a whole
/// chunk at once would let the expansion ratio decide a single allocation.
const DECOMPRESSION_SLICE_BYTES: usize = 32 * 1024;

/// Bounds how far a compressed payload may expand while it is being written.
struct ExpansionGuard {
    declared: u64,
    produced: u64,
    limit: u64,
}

impl ExpansionGuard {
    fn new(declared: u64) -> Self {
        Self {
            declared,
            produced: 0,
            limit: declared
                .saturating_mul(MAX_EXPANSION_RATIO)
                .max(MIN_EXPANSION_ALLOWANCE),
        }
    }

    fn record(&mut self, bytes: u64) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.produced = self.produced.saturating_add(bytes);

        if self.produced > self.limit {
            return Err(format!(
                "the sender declared a {} transfer but it has already expanded past {}; \
                 refusing to keep unpacking it",
                crate::progress::format_bytes(self.declared),
                crate::progress::format_bytes(self.limit)
            )
            .into());
        }

        Ok(())
    }
}

pub struct ReceiveOptions {
    pub origin: String,
    /// Which carrier to use. See [`crate::direct::Path`].
    pub path: crate::direct::Path,
    pub out_dir: PathBuf,
    pub extract: bool,
    pub force: bool,
}

/// Where received bytes are written: either straight to a file, or through the
/// decompressor and archive extractor into a directory.
enum Target {
    File {
        path: PathBuf,
        file: fs::File,
    },
    Archive {
        root: PathBuf,
        extractor: Box<TarExtractor>,
    },
}

pub async fn run(code: &str, options: ReceiveOptions) -> Result<(), Box<dyn Error + Send + Sync>> {
    let code = crypto::TransferCode::parse(code)?;

    // The nameplate says where to look, and looking is what decides the path:
    // a record under it means the sender went direct, and its absence means
    // the sender fell back. A missing record is not a wrong code — a wrong code
    // is not detectable here at all, and surfaces at the sealed metadata.
    if options.path != direct::Path::Relay {
        match try_direct(&code, &options).await {
            Ok(Some(outcome)) => return outcome,
            Ok(None) => {
                direct::may_fall_back(options.path, &*missing_record())?;
                eprintln!("No sender published for this code; trying the relay.");
            }
            Err(error) => {
                direct::may_fall_back(options.path, error.as_ref())?;
                eprintln!("No peer-to-peer path: {error}");
                eprintln!("Falling back to the relay.");
            }
        }
    }

    eprintln!("Connecting to {}...", options.origin);
    direct::report("relay (encrypted; the relay cannot read it)");

    let mut transport = relay::connect_receiver(&options.origin, code.nameplate()).await?;

    receive_transfer(&mut transport, &code, &options).await
}

fn missing_record() -> Box<dyn Error + Send + Sync> {
    "nobody has published a peer-to-peer address for this code".into()
}

/// Looks the sender up and, if it is there, receives from it directly.
///
/// `Ok(None)` distinguishes "the sender is not on this path" from "this path is
/// broken", because only the first is an ordinary outcome worth falling back
/// from quietly.
#[allow(clippy::type_complexity)]
async fn try_direct(
    code: &crypto::TransferCode,
    options: &ReceiveOptions,
) -> Result<Option<Result<(), Box<dyn Error + Send + Sync>>>, Box<dyn Error + Send + Sync>> {
    eprintln!("Looking for the sender...");

    let directory = direct::Directory::new()?;

    let Some(mut dialled) = direct::dial_sender(&directory, code).await? else {
        return Ok(None);
    };

    direct::report("peer-to-peer (no Drop server)");

    // The endpoint has to outlive the transfer. It owns the connection's
    // driver, so dropping it early kills a transfer that had just started —
    // which is exactly what happened the first time this ran over a real
    // network, and what the loopback tests could not see.
    let outcome = receive_transfer(&mut dialled.transport, code, options).await;

    dialled.endpoint.shutdown().await;

    Ok(Some(outcome))
}

/// The receiver's half of a transfer, over whatever is carrying it.
///
/// Written against the conversation rather than against a socket, so a second
/// carrier is a different `T` and not a second copy of this function.
pub(crate) async fn receive_transfer<T: Transport>(
    transport: &mut T,
    code: &crypto::TransferCode,
    options: &ReceiveOptions,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let keys = exchange_keys(transport, code).await?;
    let (version, ciphertext_size, sealed_metadata) = wait_for_meta(transport).await?;

    if version != crypto::ENVELOPE_VERSION {
        return Err(crypto::CryptoError::UnsupportedVersion { found: version }.into());
    }

    // Opening the metadata is where a mistyped code is caught: it happens
    // before any destination is created and before a byte is written. The
    // session is consumed either way, which is what holds an attacker to one
    // guess.
    //
    // Who consumes it differs by carrier. Over the relay a server refuses the
    // next claim and this side says nothing. Over a direct connection there is
    // no server, so the sender is waiting to hear how this went and cannot
    // proceed until it does — `docs/decisions.md` entry 13.
    let sealed_metadata = crypto::from_hex(&sealed_metadata)?;
    let meta = match crypto::open_metadata(&keys, ciphertext_size, &sealed_metadata) {
        Ok(meta) => meta,
        Err(error) => {
            // Saying so discloses nothing. A peer that reaches this branch
            // already knows it failed, and staying silent would only make the
            // sender wait out its timeout before reaching the same conclusion.
            if transport.peers_enforce_one_guess() {
                let _ = transport
                    .send_control(json!({
                        "type": "error",
                        "message": "the code did not open this transfer",
                    }))
                    .await;
            }

            return Err(error.into());
        }
    };

    // The checkpoint the direct path adds, and the reason it is here rather
    // than after the destination is opened: what it attests is that this peer
    // knew the code, which opening the metadata has just proved. A receiver
    // that then fails to create a file has still guessed correctly, and
    // charging it an attempt would punish the wrong failure.
    if transport.peers_enforce_one_guess() {
        transport.send_control(json!({ "type": "meta_ok" })).await?;
    }

    let size = meta.plaintext_size;
    let filename = meta.filename;
    let mime_type = meta.mime_type;

    let mut opener = crypto::Opener::new(&keys, size);

    eprintln!(
        "Receiving {} ({})",
        filename,
        crate::progress::format_bytes(size)
    );

    let decompress = options.extract
        && (mime_type == GZIP_MIME || mime_type == TAR_GZIP_MIME || filename.ends_with(".gz"));
    let is_archive = options.extract
        && (mime_type == TAR_MIME
            || mime_type == TAR_GZIP_MIME
            || strip_gz(&filename).ends_with(".tar"));

    let mut target = open_target(options, &filename, is_archive, decompress)?;
    let mut decoder = if decompress {
        Some(flate2::write::GzDecoder::new(Vec::new()))
    } else {
        None
    };
    let mut expansion = ExpansionGuard::new(size);

    let mut progress = Progress::new("Receiving", size);
    // Two scales, deliberately kept apart: the relay meters sealed bytes and
    // acknowledgements are counted in them, while progress, the expansion
    // guard, and the file on disk are all plaintext.
    let mut received = 0_u64;
    let mut written = 0_u64;
    let mut unacknowledged = 0_u64;

    while let Some(frame) = transport.receive().await? {
        match frame {
            Frame::Chunk(data) => {
                if received + data.len() as u64 > ciphertext_size {
                    let _ = transport.send_control(json!({ "type": "error" })).await;
                    return Err("the relay sent more bytes than the transfer declared".into());
                }

                received += data.len() as u64;
                unacknowledged += data.len() as u64;

                // A chunk that fails here is not written. The alternative is
                // putting bytes on disk that nobody vouched for.
                let plaintext = match opener.open_chunk(&data) {
                    Ok(plaintext) => plaintext,
                    Err(error) => {
                        let _ = transport.send_control(json!({ "type": "error" })).await;
                        discard_partial(target);
                        return Err(error.into());
                    }
                };

                written += plaintext.len() as u64;
                write_bytes(&mut target, decoder.as_mut(), &mut expansion, &plaintext)?;
                progress.update(written);

                if unacknowledged >= ACK_INTERVAL_BYTES || received == ciphertext_size {
                    unacknowledged = 0;
                    transport
                        .send_control(json!({ "type": "chunk_ack", "bytes_received": received }))
                        .await?;
                }
            }
            Frame::Control(payload) => match payload["type"].as_str() {
                Some("complete") => {
                    // Every chunk that arrived was authentic; only the count
                    // against the sealed total shows a stream that simply
                    // stopped early.
                    if let Err(error) = opener.finish() {
                        let _ = transport.send_control(json!({ "type": "error" })).await;
                        discard_partial(target);
                        return Err(error.into());
                    }

                    finish(&mut target, decoder, &mut expansion)?;
                    progress.finish(written);

                    transport
                        .send_control(json!({ "type": "complete", "bytes_received": received }))
                        .await?;

                    report(&target, written);
                    transport.close().await;
                    return Ok(());
                }
                Some("error") => {
                    return Err(payload["message"]
                        .as_str()
                        .unwrap_or("the relay reported an error")
                        .to_string()
                        .into());
                }
                _ => {}
            },
        }
    }

    Err("the transfer connection closed before the file was complete".into())
}

/// Runs the receiver's half of the key exchange.
///
/// The half is sent in reply to the sender's, not on connect. The relay
/// forwards a key exchange to a peer that is connected and drops one that is
/// not, so a receiver that arrives first and sends immediately has its half
/// discarded — and the sender then waits for a message that no longer exists,
/// with neither side seeing an error. The sender's half arriving is the proof
/// that there is somebody to reply to.
async fn exchange_keys<T: Transport>(
    transport: &mut T,
    code: &crypto::TransferCode,
) -> Result<crypto::SessionKeys, Box<dyn Error + Send + Sync>> {
    let (handshake, outbound) = crypto::Handshake::start(code);

    while let Some(frame) = transport.receive().await? {
        let Frame::Control(payload) = frame else {
            continue;
        };

        match payload["type"].as_str() {
            Some("key_exchange") => {
                let peer = payload["message"]
                    .as_str()
                    .ok_or("the sender sent a malformed key exchange message")?;

                transport
                    .send_control(json!({
                        "type": "key_exchange",
                        "message": crypto::to_hex(&outbound),
                    }))
                    .await?;

                return Ok(handshake.finish(&crypto::from_hex(peer)?)?);
            }
            Some("status") => {
                if payload["status"].as_str() == Some("waiting_for_sender") {
                    eprintln!("Waiting for the sender...");
                }
            }
            Some("error") => {
                return Err(payload["message"]
                    .as_str()
                    .unwrap_or("the relay reported an error")
                    .to_string()
                    .into());
            }
            _ => {}
        }
    }

    Err("the connection closed before the sender completed the key exchange".into())
}

/// Removes a partially written file after a failure.
///
/// A decryption failure at chunk N leaves N-1 chunks already on disk, and they
/// look exactly like a real file. Leaving one behind after telling the user
/// the transfer failed is how a truncated payload gets mistaken for a whole
/// one. Extraction into a directory is left alone: the entries written are
/// individually authentic, and deleting a tree the receiver may already have
/// had files in is a worse failure than reporting the stop.
fn discard_partial(target: Target) {
    if let Target::File { path, file } = target {
        drop(file);
        let _ = fs::remove_file(&path);
    }
}

async fn wait_for_meta<T: Transport>(
    transport: &mut T,
) -> Result<(u8, u64, String), Box<dyn Error + Send + Sync>> {
    while let Some(frame) = transport.receive().await? {
        let Frame::Control(payload) = frame else {
            continue;
        };

        match payload["type"].as_str() {
            Some("meta") => {
                let version = payload["version"].as_u64().unwrap_or(0) as u8;
                let ciphertext_size = payload["ciphertext_size"].as_u64().unwrap_or(0);
                let metadata = payload["metadata"]
                    .as_str()
                    .ok_or("the sender sent transfer details this build cannot read")?;

                return Ok((version, ciphertext_size, metadata.to_string()));
            }
            Some("status") => {
                if payload["status"].as_str() == Some("waiting_for_sender") {
                    eprintln!("Waiting for the sender...");
                }
            }
            Some("error") => {
                return Err(payload["message"]
                    .as_str()
                    .unwrap_or("the relay reported an error")
                    .to_string()
                    .into());
            }
            _ => {}
        }
    }

    Err("the transfer connection closed before the sender described the file".into())
}

fn open_target(
    options: &ReceiveOptions,
    filename: &str,
    is_archive: bool,
    decompress: bool,
) -> Result<Target, Box<dyn Error + Send + Sync>> {
    fs::create_dir_all(&options.out_dir)?;

    if is_archive {
        return Ok(Target::Archive {
            root: options.out_dir.clone(),
            extractor: Box::new(TarExtractor::new(&options.out_dir).overwriting(options.force)),
        });
    }

    // A remote peer chooses this name, so keep only the final component: an
    // archive-style path in `filename` must not decide where the file lands.
    let mut safe_name = Path::new(filename)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty() && name != "." && name != "..")
        .unwrap_or_else(|| "download.bin".to_string());

    if decompress {
        safe_name = strip_gz(&safe_name).to_string();
    }

    let requested = options.out_dir.join(&safe_name);

    if options.force {
        let file = fs::File::create(&requested)?;
        return Ok(Target::File {
            path: requested,
            file,
        });
    }

    let (path, file) = create_new_file(&options.out_dir, &safe_name)?;

    if path != requested {
        eprintln!(
            "{} already exists; saving as {} instead",
            safe_name,
            path.file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
        );
    }

    Ok(Target::File { path, file })
}

/// Creates the destination file, adding `-1`, `-2`, and so on to the name when
/// it is already taken.
///
/// The sender chooses this name and the receiver is often not watching the
/// terminal, so a collision must neither replace what is already on disk nor
/// abandon a transfer that is otherwise fine. Refusing used to do the latter:
/// the receiver gave up before a byte moved, dropped the socket, and the sender
/// was told its peer had disconnected.
///
/// `create_new` makes the test and the creation one atomic step, so two
/// receives running side by side cannot settle on the same path — which the
/// previous `exists()` check could not guarantee.
fn create_new_file(dir: &Path, name: &str) -> io::Result<(PathBuf, fs::File)> {
    for attempt in 0..=MAX_NAME_ATTEMPTS {
        let candidate = if attempt == 0 {
            name.to_string()
        } else {
            numbered_name(name, attempt)
        };
        let path = dir.join(candidate);

        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("{name} and its first {MAX_NAME_ATTEMPTS} numbered alternatives all exist"),
    ))
}

/// Turns `report.pdf` into `report-1.pdf`.
///
/// The number goes before the extension so the file still opens with the
/// application the receiver expects.
fn numbered_name(name: &str, attempt: u32) -> String {
    let path = Path::new(name);

    match (
        path.file_stem().and_then(|stem| stem.to_str()),
        path.extension().and_then(|extension| extension.to_str()),
    ) {
        (Some(stem), Some(extension)) => format!("{stem}-{attempt}.{extension}"),
        (Some(stem), None) => format!("{stem}-{attempt}"),
        _ => format!("{name}-{attempt}"),
    }
}

/// Feeds received bytes through the optional decompressor into the target.
fn write_bytes(
    target: &mut Target,
    decoder: Option<&mut flate2::write::GzDecoder<Vec<u8>>>,
    expansion: &mut ExpansionGuard,
    data: &[u8],
) -> Result<(), Box<dyn Error + Send + Sync>> {
    match decoder {
        Some(decoder) => {
            // Drain after every small slice so the decompressor never holds
            // more than one slice's worth of expansion in memory.
            for slice in data.chunks(DECOMPRESSION_SLICE_BYTES) {
                decoder.write_all(slice)?;
                let produced = std::mem::take(decoder.get_mut());
                expansion.record(produced.len() as u64)?;
                write_plain(target, &produced)?;
            }

            Ok(())
        }
        None => write_plain(target, data),
    }
}

fn write_plain(target: &mut Target, data: &[u8]) -> Result<(), Box<dyn Error + Send + Sync>> {
    if data.is_empty() {
        return Ok(());
    }

    match target {
        Target::File { file, .. } => file.write_all(data)?,
        Target::Archive { extractor, .. } => extractor.write(data)?,
    }

    Ok(())
}

fn finish(
    target: &mut Target,
    decoder: Option<flate2::write::GzDecoder<Vec<u8>>>,
    expansion: &mut ExpansionGuard,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    if let Some(mut decoder) = decoder {
        // finish() flushes the deflate stream and validates the gzip trailer,
        // so a truncated or corrupted archive is caught here rather than
        // silently producing a short file.
        decoder.try_finish()?;
        let produced = std::mem::take(decoder.get_mut());
        expansion.record(produced.len() as u64)?;
        write_plain(target, &produced)?;
    }

    if let Target::File { file, .. } = target {
        file.flush()?;
    }

    Ok(())
}

fn report(target: &Target, received: u64) {
    match target {
        Target::File { path, .. } => {
            eprintln!(
                "Saved {} ({}).",
                path.display(),
                crate::progress::format_bytes(received)
            );
        }
        Target::Archive { root, extractor } => {
            for warning in extractor.warnings() {
                eprintln!("warning: {warning}");
            }

            eprintln!(
                "Extracted {} files into {}.",
                extractor.files_written(),
                root.display()
            );
        }
    }
}

fn strip_gz(filename: &str) -> &str {
    filename.strip_suffix(".gz").unwrap_or(filename)
}

#[cfg(test)]
mod name_tests {
    use super::{create_new_file, numbered_name};

    #[test]
    fn puts_the_number_before_the_extension() {
        assert_eq!(numbered_name("report.pdf", 1), "report-1.pdf");
        assert_eq!(numbered_name("archive.tar.gz", 2), "archive.tar-2.gz");
    }

    #[test]
    fn numbers_a_name_that_has_no_extension() {
        assert_eq!(numbered_name("notes", 3), "notes-3");
    }

    #[test]
    fn treats_a_leading_dot_as_part_of_the_name() {
        assert_eq!(numbered_name(".bashrc", 1), ".bashrc-1");
    }

    #[test]
    fn walks_past_every_taken_name() {
        let dir = std::env::temp_dir().join(format!("drop-name-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch directory");

        let (first, _) = create_new_file(&dir, "report.pdf").expect("first");
        let (second, _) = create_new_file(&dir, "report.pdf").expect("second");
        let (third, _) = create_new_file(&dir, "report.pdf").expect("third");

        assert_eq!(first.file_name().unwrap(), "report.pdf");
        assert_eq!(second.file_name().unwrap(), "report-1.pdf");
        assert_eq!(third.file_name().unwrap(), "report-2.pdf");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn never_replaces_an_existing_file() {
        let dir = std::env::temp_dir().join(format!("drop-keep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch directory");
        std::fs::write(dir.join("report.pdf"), b"mine, already here").expect("existing file");

        let (path, _) = create_new_file(&dir, "report.pdf").expect("a free name");

        assert_ne!(path.file_name().unwrap(), "report.pdf");
        assert_eq!(
            std::fs::read_to_string(dir.join("report.pdf")).expect("read"),
            "mine, already here",
            "the receiver's own file must survive"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod expansion_tests {
    use super::ExpansionGuard;

    #[test]
    fn allows_a_small_payload_its_full_floor() {
        let mut guard = ExpansionGuard::new(1024);

        assert!(guard.record(512 * 1024 * 1024).is_ok());
        assert!(guard.record(512 * 1024 * 1024).is_ok());
    }

    #[test]
    fn refuses_a_payload_that_expands_past_the_ratio() {
        // Large enough that the ratio, not the floor, sets the limit.
        let declared = 1024 * 1024 * 1024;
        let mut guard = ExpansionGuard::new(declared);

        assert!(guard.record(declared * 100).is_ok());
        assert!(
            guard.record(1).is_err(),
            "expansion beyond the ratio must be refused"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{exchange_keys, wait_for_meta};
    use crate::crypto;
    use crate::transport::scripted::ScriptedTransport;
    use serde_json::json;

    #[tokio::test]
    async fn the_receivers_half_is_written_only_after_the_senders_arrives() {
        let code = crypto::TransferCode::generate_for("A1B2C3").expect("a well-formed code");
        let (_, sender_half) = crypto::Handshake::start(&code);

        // The status arrives first and must not draw a reply: the sender is
        // not connected yet, and a half sent now would be dropped.
        let mut transport = ScriptedTransport::saying(vec![
            json!({ "type": "status", "status": "waiting_for_sender" }),
            json!({ "type": "key_exchange", "message": crypto::to_hex(&sender_half) }),
        ]);

        exchange_keys(&mut transport, &code)
            .await
            .expect("the halves agree");

        let sent = transport.sent_control();
        assert_eq!(sent.len(), 1, "exactly one half, and only in reply");
        assert_eq!(sent[0]["type"], "key_exchange");
    }

    #[tokio::test]
    async fn a_sender_that_stops_before_the_key_exchange_is_an_error() {
        let code = crypto::TransferCode::generate_for("A1B2C3").expect("a well-formed code");
        let mut transport = ScriptedTransport::silent();

        // `expect_err` is not available here: `SessionKeys` deliberately does
        // not implement `Debug`, so a key can never be printed by accident.
        let Err(error) = exchange_keys(&mut transport, &code).await else {
            panic!("there was no half to agree with");
        };
        assert!(error.to_string().contains("before the sender completed"));

        assert!(
            transport.sent_control().is_empty(),
            "nothing to reply to means nothing sent"
        );
    }

    #[tokio::test]
    async fn transfer_details_are_read_past_the_frames_that_precede_them() {
        let mut transport = ScriptedTransport::saying(vec![
            json!({ "type": "status", "status": "waiting_for_sender" }),
            json!({
                "type": "meta",
                "version": crypto::ENVELOPE_VERSION,
                "ciphertext_size": 4096,
                "metadata": "abcdef",
            }),
        ]);

        let (version, ciphertext_size, metadata) = wait_for_meta(&mut transport)
            .await
            .expect("the sender described the payload");

        assert_eq!(version, crypto::ENVELOPE_VERSION);
        assert_eq!(ciphertext_size, 4096);
        assert_eq!(metadata, "abcdef");
    }
}
