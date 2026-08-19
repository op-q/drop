//! Turning a path the user named into a byte stream of known exact length.
//!
//! Drop commits to `file_size` when the session is created, so every payload
//! must know its length before the first byte moves.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use tokio::sync::mpsc;

use crate::tar::TarPlan;

pub const CHUNK_BYTES: usize = 1024 * 1024;

pub const TAR_MIME: &str = "application/x-tar";
pub const TAR_GZIP_MIME: &str = "application/x-tar+gzip";
pub const GZIP_MIME: &str = "application/gzip";
pub const BINARY_MIME: &str = "application/octet-stream";

pub enum Source {
    /// Streamed straight off disk.
    File(PathBuf),
    /// Streamed as a tar archive built on the fly.
    Directory(Box<TarPlan>),
}

pub struct Payload {
    pub filename: String,
    pub mime_type: String,
    pub size: u64,
    pub source: Source,
    /// Present only for a compressed payload: the spool file is deleted when
    /// the payload is dropped, whether the transfer succeeded or not.
    spool: Option<SpoolFile>,
    pub summary: String,
    pub warnings: Vec<String>,
}

/// Spool files that must not outlive this process.
///
/// [`SpoolFile`] deletes itself on drop, which covers every path the program
/// returns through. A signal does not unwind, so the handler installed by
/// [`remove_spool_files`] needs somewhere to look the paths up.
static SPOOL_PATHS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Deletes every spool file this process still owns.
///
/// Safe to call from a signal handler path and safe to call twice: a file that
/// is already gone is not an error.
pub fn remove_spool_files() {
    let paths = match SPOOL_PATHS.lock() {
        Ok(mut registry) => std::mem::take(&mut *registry),
        Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
    };

    for path in paths {
        let _ = fs::remove_file(path);
    }
}

/// Resolves when the process is asked to terminate.
///
/// A payload compressed to a spool file is the one thing here that outlives a
/// killed process, so `send` waits on this to delete it before exiting.
pub async fn wait_for_termination() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let Ok(mut terminate) = signal(SignalKind::terminate()) else {
            let _ = tokio::signal::ctrl_c().await;
            return;
        };

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// A temporary file that removes itself on drop.
struct SpoolFile {
    path: PathBuf,
}

impl SpoolFile {
    /// Creates a spool file that only this user can read.
    ///
    /// `create_new` means the open fails rather than following something that
    /// is already at the path, so a symlink planted in a shared temporary
    /// directory cannot redirect the write onto a file of the attacker's
    /// choosing. The mode keeps the payload's bytes off other users' reads.
    fn create(filename: &str) -> io::Result<(Self, fs::File)> {
        let directory = std::env::temp_dir();
        let sanitized = filename.replace(['/', '\\'], "_");

        for attempt in 0..64_u32 {
            let path = directory.join(format!(
                "drop-{}-{:016x}-{sanitized}.tmp",
                std::process::id(),
                nonce(attempt)
            ));

            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);

            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }

            match options.open(&path) {
                Ok(file) => {
                    if let Ok(mut registry) = SPOOL_PATHS.lock() {
                        registry.push(path.clone());
                    }

                    return Ok((Self { path }, file));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }

        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create a temporary file for the compressed payload",
        ))
    }
}

impl Drop for SpoolFile {
    fn drop(&mut self) {
        if let Ok(mut registry) = SPOOL_PATHS.lock() {
            registry.retain(|path| path != &self.path);
        }

        let _ = fs::remove_file(&self.path);
    }
}

/// Distinguishes concurrent spool files without pulling in a random-number
/// dependency. Uniqueness is what this is for; `create_new` is what makes the
/// path safe regardless of how guessable it is.
fn nonce(attempt: u32) -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or_default();

    nanos ^ (u64::from(attempt) << 48) ^ (&attempt as *const u32 as u64)
}

impl Payload {
    /// Describes `path` as a transferable payload.
    ///
    /// Compression cannot be streamed: the relay is told the exact length up
    /// front, and a compressed length is only known once compression finishes.
    /// A compressed payload is therefore spooled to a temporary file first,
    /// which costs local disk but keeps the protocol's exact-length guarantee.
    pub fn prepare(path: &Path, compress: Option<u32>) -> io::Result<Self> {
        let metadata = fs::metadata(path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("cannot read {}: {error}", path.display()),
            )
        })?;

        let base_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "drop-payload".to_string());

        let mut payload = if metadata.is_dir() {
            let plan = TarPlan::scan(path)?;
            let warnings = plan
                .skipped()
                .iter()
                .map(|entry| format!("skipped {entry}: unsupported file type"))
                .collect();

            Self {
                filename: format!("{base_name}.tar"),
                mime_type: TAR_MIME.to_string(),
                size: plan.total_bytes(),
                summary: format!(
                    "{} ({} files, archived as {}.tar)",
                    path.display(),
                    plan.file_count(),
                    base_name
                ),
                source: Source::Directory(Box::new(plan)),
                spool: None,
                warnings,
            }
        } else {
            Self {
                filename: base_name.clone(),
                mime_type: guess_mime(&base_name).to_string(),
                size: metadata.len(),
                summary: path.display().to_string(),
                source: Source::File(path.to_path_buf()),
                spool: None,
                warnings: Vec::new(),
            }
        };

        if let Some(level) = compress {
            payload.compress_into_spool(level)?;
        }

        Ok(payload)
    }

    fn compress_into_spool(&mut self, level: u32) -> io::Result<()> {
        let (spool, file) = SpoolFile::create(&self.filename)?;

        let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::new(level));
        let mut warnings = Vec::new();

        match &self.source {
            Source::File(path) => {
                let mut input = fs::File::open(path)?;
                io::copy(&mut input, &mut encoder)?;
            }
            Source::Directory(plan) => {
                plan.write_to(&mut encoder, |warning| warnings.push(warning.to_string()))?;
            }
        }

        let mut file = encoder.finish()?;
        file.flush()?;
        let compressed_size = file.metadata()?.len();
        drop(file);

        self.warnings.extend(warnings);
        self.summary = format!(
            "{} ({} compressed to {})",
            self.summary,
            crate::progress::format_bytes(self.size),
            crate::progress::format_bytes(compressed_size)
        );
        self.filename = format!("{}.gz", self.filename);
        self.mime_type = if self.mime_type == TAR_MIME {
            TAR_GZIP_MIME.to_string()
        } else {
            GZIP_MIME.to_string()
        };
        self.size = compressed_size;
        self.source = Source::File(spool.path.clone());
        self.spool = Some(spool);

        Ok(())
    }

    /// Starts producing the payload, returning a channel of chunks.
    ///
    /// Reading and archiving happen on the blocking pool so a slow disk never
    /// stalls the socket task, and the bounded channel means the producer waits
    /// for the network instead of buffering the whole payload.
    pub fn into_chunks(self) -> mpsc::Receiver<io::Result<Vec<u8>>> {
        let (sender, receiver) = mpsc::channel::<io::Result<Vec<u8>>>(4);
        let source = self.source;
        // Held until production finishes so the spool file outlives its reader.
        let spool = self.spool;

        std::thread::spawn(move || {
            let mut writer = ChunkWriter::new(sender.clone());

            let result = match &source {
                Source::File(path) => fs::File::open(path)
                    .and_then(|mut file| io::copy(&mut file, &mut writer).map(|_| ())),
                Source::Directory(plan) => plan.write_to(&mut writer, |_| {}).map(|_| ()),
            };

            let result = result.and_then(|()| writer.flush());

            if let Err(error) = result {
                let _ = sender.blocking_send(Err(error));
            }

            drop(spool);
        });

        receiver
    }
}

/// Buffers writes into fixed-size chunks and hands them to an async consumer.
struct ChunkWriter {
    sender: mpsc::Sender<io::Result<Vec<u8>>>,
    buffer: Vec<u8>,
}

impl ChunkWriter {
    fn new(sender: mpsc::Sender<io::Result<Vec<u8>>>) -> Self {
        Self {
            sender,
            buffer: Vec::with_capacity(CHUNK_BYTES),
        }
    }

    fn dispatch(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        let chunk = std::mem::replace(&mut self.buffer, Vec::with_capacity(CHUNK_BYTES));

        self.sender.blocking_send(Ok(chunk)).map_err(|_| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the transfer ended before the payload was fully read",
            )
        })
    }
}

impl Write for ChunkWriter {
    fn write(&mut self, mut input: &[u8]) -> io::Result<usize> {
        let total = input.len();

        while !input.is_empty() {
            let space = CHUNK_BYTES - self.buffer.len();
            let take = space.min(input.len());
            self.buffer.extend_from_slice(&input[..take]);
            input = &input[take..];

            if self.buffer.len() == CHUNK_BYTES {
                self.dispatch()?;
            }
        }

        Ok(total)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.dispatch()
    }
}

fn guess_mime(filename: &str) -> &'static str {
    let extension = filename
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();

    match extension.as_str() {
        "txt" | "md" | "log" => "text/plain",
        "json" => "application/json",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "mp4" => "video/mp4",
        "zip" => "application/zip",
        "gz" => GZIP_MIME,
        "tar" => TAR_MIME,
        _ => BINARY_MIME,
    }
}

#[cfg(test)]
mod spool_tests {
    use super::{Payload, SPOOL_PATHS, Source, remove_spool_files};
    use std::{fs, path::PathBuf, sync::Mutex};

    /// `remove_spool_files` deletes every spool in the process, which is what a
    /// signal handler needs and what makes these two tests unable to share a
    /// process concurrently.
    static EXCLUSIVE: Mutex<()> = Mutex::new(());

    fn fixture(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "drop-spool-test-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after the epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&directory).expect("fixture directory");
        fs::write(directory.join("payload.txt"), vec![b'a'; 4096]).expect("fixture file");
        directory
    }

    /// The spool holds the user's file bytes on a directory other local users
    /// can usually write to, so it must not be readable by any of them.
    #[test]
    #[cfg(unix)]
    fn creates_the_spool_file_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = EXCLUSIVE.lock().unwrap_or_else(|error| error.into_inner());

        let directory = fixture("permissions");
        let payload = Payload::prepare(&directory, Some(6)).expect("compressed payload");

        let Source::File(path) = &payload.source else {
            panic!("a compressed payload streams from its spool file");
        };

        let mode = fs::metadata(path)
            .expect("spool metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "the spool must not be group or world accessible, got {:o}",
            mode & 0o777
        );

        drop(payload);
        fs::remove_dir_all(&directory).ok();
    }

    /// A signal does not unwind, so the destructor never runs and this is the
    /// only thing standing between a cancelled send and a leftover copy of the
    /// user's data in the temporary directory.
    #[test]
    fn removes_the_spool_file_without_unwinding() {
        let _guard = EXCLUSIVE.lock().unwrap_or_else(|error| error.into_inner());

        let directory = fixture("signal");
        let payload = Payload::prepare(&directory, Some(6)).expect("compressed payload");

        let Source::File(path) = &payload.source else {
            panic!("a compressed payload streams from its spool file");
        };
        let path = path.clone();
        assert!(path.exists(), "the spool file should exist while sending");

        // Stand in for the signal handler, which cannot unwind into `Drop`.
        remove_spool_files();

        assert!(
            !path.exists(),
            "the spool file must not survive a terminating signal"
        );
        assert!(
            !SPOOL_PATHS
                .lock()
                .expect("registry")
                .iter()
                .any(|registered| registered == &path),
            "the registry must not keep a path it has already deleted"
        );

        drop(payload);
        fs::remove_dir_all(&directory).ok();
    }
}
