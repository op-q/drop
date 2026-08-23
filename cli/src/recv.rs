//! The receiving half of a terminal-to-terminal transfer.

use std::{
    error::Error,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

use crate::{
    client::{self, Socket},
    payload::{GZIP_MIME, TAR_GZIP_MIME, TAR_MIME},
    progress::Progress,
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
    eprintln!("Connecting to {}...", options.origin);

    let mut socket = client::open_download(&options.origin, code).await?;
    let meta = wait_for_meta(&mut socket).await?;

    let (filename, size, mime_type) = meta;
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

    let mut target = open_target(&options, &filename, is_archive, decompress)?;
    let mut decoder = if decompress {
        Some(flate2::write::GzDecoder::new(Vec::new()))
    } else {
        None
    };
    let mut expansion = ExpansionGuard::new(size);

    let mut progress = Progress::new("Receiving", size);
    let mut received = 0_u64;
    let mut unacknowledged = 0_u64;

    while let Some(message) = socket.next().await {
        match message? {
            Message::Binary(data) => {
                if received + data.len() as u64 > size {
                    let _ = socket
                        .send(Message::Text(json!({ "type": "error" }).to_string().into()))
                        .await;
                    return Err("the relay sent more bytes than the file declared".into());
                }

                received += data.len() as u64;
                unacknowledged += data.len() as u64;

                write_bytes(&mut target, decoder.as_mut(), &mut expansion, &data)?;
                progress.update(received);

                if unacknowledged >= ACK_INTERVAL_BYTES || received == size {
                    unacknowledged = 0;
                    socket
                        .send(Message::Text(
                            json!({ "type": "chunk_ack", "bytes_received": received })
                                .to_string()
                                .into(),
                        ))
                        .await?;
                }
            }
            Message::Text(text) => {
                let payload: Value = serde_json::from_str(&text)?;

                match payload["type"].as_str() {
                    Some("complete") => {
                        finish(&mut target, decoder, &mut expansion)?;
                        progress.finish(received);

                        socket
                            .send(Message::Text(
                                json!({ "type": "complete", "bytes_received": received })
                                    .to_string()
                                    .into(),
                            ))
                            .await?;

                        report(&target, received);
                        let _ = socket.close(None).await;
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
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    Err("the transfer connection closed before the file was complete".into())
}

async fn wait_for_meta(
    socket: &mut Socket,
) -> Result<(String, u64, String), Box<dyn Error + Send + Sync>> {
    while let Some(message) = socket.next().await {
        let Message::Text(text) = message? else {
            continue;
        };

        let payload: Value = serde_json::from_str(&text)?;

        match payload["type"].as_str() {
            Some("meta") => {
                let filename = payload["filename"].as_str().unwrap_or("download.bin");
                let size = payload["file_size"].as_u64().unwrap_or(0);
                let mime_type = payload["mime_type"].as_str().unwrap_or("");

                return Ok((filename.to_string(), size, mime_type.to_string()));
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
