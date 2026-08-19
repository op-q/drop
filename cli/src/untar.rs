//! Streaming ustar extraction.
//!
//! The receiver never holds a whole archive: bytes are parsed and written out
//! as they arrive off the socket, so extracting a 4 GiB directory needs no
//! staging file and no more memory than one chunk.
//!
//! Extraction is the point where a remote peer chooses filesystem paths, so
//! every entry is validated against the destination root before anything is
//! created. See [`crate::tar::safe_relative_path`].

use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

use crate::tar::{safe_relative_path, symlink_target_stays_inside, traverses_only_real_dirs};

const BLOCK: usize = 512;

/// A sane ceiling for a GNU long-name record. The record is buffered in memory
/// because it *is* a path, so a hostile archive must not be able to declare a
/// gigabyte-long one.
const MAX_LONG_RECORD_BYTES: u64 = 64 * 1024;

enum Sink {
    File(File),
    /// Content of an entry type we do not create, consumed and dropped.
    Discard,
    /// A GNU long-name or long-link record, whose content *is* the path.
    Collect {
        typeflag: u8,
        value: Vec<u8>,
    },
}

enum State {
    Header,
    Content {
        sink: Sink,
        remaining: u64,
        padding: usize,
    },
    Padding {
        remaining: usize,
    },
    Finished,
}

pub struct TarExtractor {
    destination: PathBuf,
    state: State,
    header: Vec<u8>,
    long_name: Option<String>,
    long_link: Option<String>,
    files_written: u64,
    /// Whether an entry may replace a file that is already on disk.
    overwrite: bool,
    warnings: Vec<String>,
}

impl TarExtractor {
    pub fn new(destination: &Path) -> Self {
        Self {
            destination: destination.to_path_buf(),
            state: State::Header,
            header: Vec::with_capacity(BLOCK),
            long_name: None,
            long_link: None,
            files_written: 0,
            overwrite: false,
            warnings: Vec::new(),
        }
    }

    /// Allows entries to replace files that already exist in the destination.
    ///
    /// Off by default: extraction writes into a directory the receiver chose,
    /// which defaults to the working directory, so silently replacing what is
    /// already there is destructive in a way a peer should not get to decide.
    pub fn overwriting(mut self, overwrite: bool) -> Self {
        self.overwrite = overwrite;
        self
    }

    pub fn files_written(&self) -> u64 {
        self.files_written
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Feeds the next slice of archive bytes.
    pub fn write(&mut self, mut input: &[u8]) -> io::Result<()> {
        while !input.is_empty() {
            match &mut self.state {
                State::Finished => return Ok(()),

                State::Header => {
                    let want = BLOCK - self.header.len();
                    let take = want.min(input.len());
                    self.header.extend_from_slice(&input[..take]);
                    input = &input[take..];

                    if self.header.len() == BLOCK {
                        let block: [u8; BLOCK] = self
                            .header
                            .as_slice()
                            .try_into()
                            .expect("header buffer is exactly one block");
                        self.header.clear();
                        self.begin_entry(&block)?;
                    }
                }

                State::Content {
                    sink,
                    remaining,
                    padding,
                } => {
                    let take = (*remaining).min(input.len() as u64) as usize;

                    match sink {
                        Sink::File(file) => file.write_all(&input[..take])?,
                        Sink::Collect { value, .. } => value.extend_from_slice(&input[..take]),
                        Sink::Discard => {}
                    }

                    input = &input[take..];
                    *remaining -= take as u64;

                    if *remaining == 0 {
                        let padding = *padding;
                        let finished = std::mem::replace(&mut self.state, State::Header);

                        if let State::Content {
                            sink: Sink::Collect { typeflag, value },
                            ..
                        } = finished
                        {
                            // The record's content is the real path for the
                            // header that follows it.
                            let text = String::from_utf8_lossy(
                                value.split(|byte| *byte == 0).next().unwrap_or(&value),
                            )
                            .into_owned();

                            if typeflag == b'L' {
                                self.long_name = Some(text);
                            } else {
                                self.long_link = Some(text);
                            }
                        }

                        if padding > 0 {
                            self.state = State::Padding { remaining: padding };
                        }
                    }
                }

                State::Padding { remaining } => {
                    let take = (*remaining).min(input.len());
                    input = &input[take..];
                    *remaining -= take;

                    if *remaining == 0 {
                        self.state = State::Header;
                    }
                }
            }
        }

        Ok(())
    }

    fn begin_entry(&mut self, block: &[u8; BLOCK]) -> io::Result<()> {
        if block.iter().all(|byte| *byte == 0) {
            self.state = State::Finished;
            return Ok(());
        }

        if !checksum_matches(block) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "archive header failed its checksum; the transfer is not a valid tar stream",
            ));
        }

        let size = parse_octal(&block[124..136]).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "archive header has an invalid size",
            )
        })?;
        let typeflag = block[156];
        let padding = padding_for(size);

        // GNU long-name records carry the real path as their content.
        if typeflag == b'L' || typeflag == b'K' {
            if size > MAX_LONG_RECORD_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "archive declares an implausibly long path",
                ));
            }

            self.state = State::Content {
                sink: Sink::Collect {
                    typeflag,
                    value: Vec::with_capacity(size as usize),
                },
                remaining: size,
                padding,
            };
            return Ok(());
        }

        let name = self.long_name.take().unwrap_or_else(|| joined_name(block));
        let link_target = self
            .long_link
            .take()
            .unwrap_or_else(|| read_str(&block[157..257]));

        let Some(path) = safe_relative_path(&self.destination, &name) else {
            self.warnings.push(format!(
                "refused {name}: the archive entry points outside the destination"
            ));
            self.state = State::Content {
                sink: Sink::Discard,
                remaining: size,
                padding,
            };
            return Ok(());
        };

        // A lexically safe path can still resolve outside once an earlier entry
        // has planted a symlink among its parents, so the path is checked
        // against what is actually on disk, not only against its own text.
        if !traverses_only_real_dirs(&self.destination, &path) {
            self.warnings.push(format!(
                "refused {name}: the archive entry resolves through a symbolic link"
            ));
            self.state = State::Content {
                sink: Sink::Discard,
                remaining: size,
                padding,
            };
            return Ok(());
        }

        let mode = parse_octal(&block[100..108]).unwrap_or(0o644) as u32;

        match typeflag {
            b'5' => {
                fs::create_dir_all(&path)?;
                self.state = self.skip_content(size, padding);
            }
            b'2' => {
                let parent = path.parent().unwrap_or(&self.destination);

                if !symlink_target_stays_inside(&self.destination, parent, &link_target) {
                    self.warnings.push(format!(
                        "refused symlink {name}: its target escapes the destination"
                    ));
                } else if !self.overwrite && path.symlink_metadata().is_ok() {
                    self.warnings.push(format!(
                        "skipped symlink {name}: it already exists; pass --force to replace it"
                    ));
                } else {
                    fs::create_dir_all(parent)?;
                    let _ = fs::remove_file(&path);
                    create_symlink(&link_target, &path)?;
                }

                self.state = self.skip_content(size, padding);
            }
            b'0' | 0 => {
                if !self.overwrite && path.symlink_metadata().is_ok() {
                    self.warnings.push(format!(
                        "skipped {name}: it already exists; pass --force to replace it"
                    ));
                    self.state = self.skip_content(size, padding);
                    return Ok(());
                }

                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }

                // Remove first rather than truncating in place: an existing
                // entry here may be a symlink, and `File::create` would follow
                // it and write through to wherever it points.
                let _ = fs::remove_file(&path);

                let file = File::create(&path)?;
                apply_mode(&file, mode)?;
                self.files_written += 1;

                self.state = if size == 0 {
                    State::Header
                } else {
                    State::Content {
                        sink: Sink::File(file),
                        remaining: size,
                        padding,
                    }
                };
            }
            other => {
                self.warnings.push(format!(
                    "skipped {name}: unsupported archive entry type {}",
                    other as char
                ));
                self.state = State::Content {
                    sink: Sink::Discard,
                    remaining: size,
                    padding,
                };
            }
        }

        Ok(())
    }
}

impl TarExtractor {
    /// State that consumes `size` content bytes without storing them.
    ///
    /// An entry type that creates no file can still declare a size, and the
    /// bytes must be read past or the next header would be parsed out of the
    /// entry's own content.
    fn skip_content(&self, size: u64, padding: usize) -> State {
        if size == 0 && padding == 0 {
            State::Header
        } else {
            State::Content {
                sink: Sink::Discard,
                remaining: size,
                padding,
            }
        }
    }
}

fn padding_for(size: u64) -> usize {
    let remainder = (size % BLOCK as u64) as usize;
    if remainder == 0 { 0 } else { BLOCK - remainder }
}

fn joined_name(block: &[u8; BLOCK]) -> String {
    let name = read_str(&block[0..100]);
    let prefix = read_str(&block[345..500]);

    if prefix.is_empty() {
        name
    } else {
        format!("{prefix}/{name}")
    }
}

fn read_str(field: &[u8]) -> String {
    let end = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

fn parse_octal(field: &[u8]) -> Option<u64> {
    let text = read_str(field);
    let trimmed = text.trim();

    if trimmed.is_empty() {
        return Some(0);
    }

    u64::from_str_radix(trimmed, 8).ok()
}

fn checksum_matches(block: &[u8; BLOCK]) -> bool {
    let Some(recorded) = parse_octal(&block[148..156]) else {
        return false;
    };

    let computed: u64 = block
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            if (148..156).contains(&index) {
                u64::from(b' ')
            } else {
                u64::from(*byte)
            }
        })
        .sum();

    computed == recorded
}

#[cfg(unix)]
fn apply_mode(file: &File, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(mode & 0o777))
}

#[cfg(not(unix))]
fn apply_mode(_file: &File, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &str, path: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, path)
}

#[cfg(not(unix))]
fn create_symlink(_target: &str, _path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "symbolic links in archives are not extracted on this platform",
    ))
}
