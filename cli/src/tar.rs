//! A minimal, deterministic ustar archive writer.
//!
//! Drop's protocol commits to an exact byte count before the first byte is
//! sent: the session records `file_size`, the relay rejects any transfer whose
//! relayed total disagrees with it, and the receiver only confirms completion
//! on an exact match. A directory therefore cannot be streamed through a
//! general-purpose archiver whose output length is only known once it finishes.
//!
//! [`TarPlan`] resolves that by scanning the tree once and computing the exact
//! archive length from the same entry list that is later streamed, so the
//! declared size and the produced bytes come from one source of truth rather
//! than from two implementations that must agree.

use std::{
    fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

const BLOCK: usize = 512;
const NAME_LEN: usize = 100;
const PREFIX_LEN: usize = 155;

/// Trailing end-of-archive marker: two zero blocks.
const TRAILER_BLOCKS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    File,
    Symlink,
}

#[derive(Debug, Clone)]
pub struct TarEntry {
    /// Path inside the archive, always `/`-separated and relative.
    pub archive_path: String,
    /// Path on disk to read the contents from.
    pub source: PathBuf,
    pub kind: EntryKind,
    /// Size recorded at scan time. The stream writes exactly this many content
    /// bytes even if the file changes underneath us.
    pub size: u64,
    pub mode: u32,
    pub mtime: u64,
    pub link_target: String,
}

/// A scanned directory tree with a known exact archive length.
#[derive(Debug, Clone)]
pub struct TarPlan {
    entries: Vec<TarEntry>,
    total_bytes: u64,
    skipped: Vec<String>,
}

impl TarPlan {
    /// Scans `root`, recording every entry that can be represented.
    ///
    /// Symbolic links are stored as links rather than followed, so a link
    /// pointing outside the tree cannot pull unrelated files into the archive
    /// and a link cycle cannot make the scan diverge.
    pub fn scan(root: &Path) -> io::Result<Self> {
        let mut entries = Vec::new();
        let mut skipped = Vec::new();

        let root_name = root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty() && name != "." && name != "..")
            .unwrap_or_else(|| "archive".to_string());

        collect(root, &root_name, &mut entries, &mut skipped)?;

        // A stable order keeps the archive reproducible and makes the streamed
        // bytes match the scanned plan regardless of directory iteration order.
        entries.sort_by(|left, right| left.archive_path.cmp(&right.archive_path));

        let total_bytes =
            entries.iter().map(entry_blocks).sum::<u64>() + (TRAILER_BLOCKS * BLOCK) as u64;

        Ok(Self {
            entries,
            total_bytes,
            skipped,
        })
    }

    /// The exact number of bytes [`TarPlan::write_to`] will produce.
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    pub fn file_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.kind == EntryKind::File)
            .count()
    }

    /// Entries that could not be represented in a ustar archive, such as
    /// sockets, FIFOs, and device nodes.
    pub fn skipped(&self) -> &[String] {
        &self.skipped
    }

    pub fn entries(&self) -> &[TarEntry] {
        &self.entries
    }

    /// Streams the archive, writing exactly [`TarPlan::total_bytes`] bytes.
    ///
    /// `on_warning` is called when a file no longer matches the size recorded
    /// during the scan. Such an entry is padded or truncated to the recorded
    /// size rather than allowed to change the archive length, because the
    /// declared length is already committed to the relay.
    pub fn write_to<W: Write>(
        &self,
        writer: &mut W,
        mut on_warning: impl FnMut(&str),
    ) -> io::Result<u64> {
        let mut written = 0_u64;

        for entry in &self.entries {
            written += write_entry(writer, entry, &mut on_warning)?;
        }

        let trailer = [0_u8; BLOCK * TRAILER_BLOCKS];
        writer.write_all(&trailer)?;
        written += trailer.len() as u64;

        Ok(written)
    }
}

fn collect(
    path: &Path,
    archive_path: &str,
    entries: &mut Vec<TarEntry>,
    skipped: &mut Vec<String>,
) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let file_type = metadata.file_type();

    if file_type.is_symlink() {
        let target = fs::read_link(path)?;
        entries.push(TarEntry {
            archive_path: archive_path.to_string(),
            source: path.to_path_buf(),
            kind: EntryKind::Symlink,
            size: 0,
            mode: 0o777,
            mtime: modified_seconds(&metadata),
            link_target: target.to_string_lossy().into_owned(),
        });
        return Ok(());
    }

    if file_type.is_dir() {
        entries.push(TarEntry {
            archive_path: format!("{archive_path}/"),
            source: path.to_path_buf(),
            kind: EntryKind::Directory,
            size: 0,
            mode: permission_mode(&metadata, 0o755),
            mtime: modified_seconds(&metadata),
            link_target: String::new(),
        });

        for child in fs::read_dir(path)? {
            let child = child?;
            let name = child.file_name().to_string_lossy().into_owned();
            collect(
                &child.path(),
                &format!("{archive_path}/{name}"),
                entries,
                skipped,
            )?;
        }

        return Ok(());
    }

    if file_type.is_file() {
        entries.push(TarEntry {
            archive_path: archive_path.to_string(),
            source: path.to_path_buf(),
            kind: EntryKind::File,
            size: metadata.len(),
            mode: permission_mode(&metadata, 0o644),
            mtime: modified_seconds(&metadata),
            link_target: String::new(),
        });
        return Ok(());
    }

    skipped.push(archive_path.to_string());
    Ok(())
}

#[cfg(unix)]
fn permission_mode(metadata: &fs::Metadata, _default: u32) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn permission_mode(metadata: &fs::Metadata, default: u32) -> u32 {
    if metadata.permissions().readonly() {
        default & 0o555
    } else {
        default
    }
}

fn modified_seconds(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn blocks_for(bytes: u64) -> u64 {
    bytes.div_ceil(BLOCK as u64) * BLOCK as u64
}

/// Total archive bytes an entry contributes, matching what [`write_entry`]
/// produces byte for byte.
fn entry_blocks(entry: &TarEntry) -> u64 {
    let mut total = 0;

    if split_name(&entry.archive_path).is_none() {
        // GNU long-name record: one header plus the padded path.
        total += BLOCK as u64 + blocks_for(entry.archive_path.len() as u64 + 1);
    }

    if entry.link_target.len() > NAME_LEN {
        total += BLOCK as u64 + blocks_for(entry.link_target.len() as u64 + 1);
    }

    total += BLOCK as u64;

    if entry.kind == EntryKind::File {
        total += blocks_for(entry.size);
    }

    total
}

fn write_entry<W: Write>(
    writer: &mut W,
    entry: &TarEntry,
    on_warning: &mut impl FnMut(&str),
) -> io::Result<u64> {
    let mut written = 0_u64;

    if split_name(&entry.archive_path).is_none() {
        written += write_gnu_long_record(writer, b'L', &entry.archive_path)?;
    }

    if entry.link_target.len() > NAME_LEN {
        written += write_gnu_long_record(writer, b'K', &entry.link_target)?;
    }

    writer.write_all(&build_header(entry))?;
    written += BLOCK as u64;

    if entry.kind == EntryKind::File {
        written += write_file_contents(writer, entry, on_warning)?;
    }

    Ok(written)
}

/// Writes exactly `entry.size` content bytes plus block padding.
///
/// A file that shrank, grew, or vanished between the scan and the stream is
/// zero-padded or truncated to the recorded size. The declared transfer length
/// is already committed, so changing the output length here would fail the
/// whole transfer instead of producing one imperfect entry.
fn write_file_contents<W: Write>(
    writer: &mut W,
    entry: &TarEntry,
    on_warning: &mut impl FnMut(&str),
) -> io::Result<u64> {
    let mut remaining = entry.size;
    let mut buffer = vec![0_u8; 128 * 1024];
    let mut short = false;

    match fs::File::open(&entry.source) {
        Ok(mut file) => {
            while remaining > 0 {
                let want = buffer.len().min(remaining as usize);
                let read = file.read(&mut buffer[..want])?;

                if read == 0 {
                    short = true;
                    break;
                }

                writer.write_all(&buffer[..read])?;
                remaining -= read as u64;
            }

            if !short && file.read(&mut buffer[..1])? > 0 {
                on_warning(&format!(
                    "{} grew while being read; sending the first {} bytes",
                    entry.archive_path, entry.size
                ));
            }
        }
        Err(error) => {
            on_warning(&format!(
                "{} could not be read ({error}); sending zero bytes in its place",
                entry.archive_path
            ));
            short = true;
        }
    }

    if short && remaining > 0 {
        on_warning(&format!(
            "{} shrank while being read; padding {} bytes",
            entry.archive_path, remaining
        ));

        let filler = vec![0_u8; buffer.len()];
        while remaining > 0 {
            let take = filler.len().min(remaining as usize);
            writer.write_all(&filler[..take])?;
            remaining -= take as u64;
        }
    }

    let padding = blocks_for(entry.size) - entry.size;
    if padding > 0 {
        writer.write_all(&vec![0_u8; padding as usize])?;
    }

    Ok(blocks_for(entry.size))
}

fn write_gnu_long_record<W: Write>(writer: &mut W, typeflag: u8, value: &str) -> io::Result<u64> {
    let payload_len = value.len() as u64 + 1;

    let header = TarEntry {
        archive_path: "././@LongLink".to_string(),
        source: PathBuf::new(),
        kind: EntryKind::File,
        size: payload_len,
        mode: 0o644,
        mtime: 0,
        link_target: String::new(),
    };

    let mut block = build_header(&header);
    block[156] = typeflag;
    recompute_checksum(&mut block);
    writer.write_all(&block)?;

    let padded = blocks_for(payload_len);
    let mut payload = vec![0_u8; padded as usize];
    payload[..value.len()].copy_from_slice(value.as_bytes());
    writer.write_all(&payload)?;

    Ok(BLOCK as u64 + padded)
}

/// Splits an archive path into the ustar `prefix` and `name` fields, or returns
/// `None` when it does not fit and a GNU long-name record is needed.
fn split_name(path: &str) -> Option<(String, String)> {
    if path.len() <= NAME_LEN {
        return Some((String::new(), path.to_string()));
    }

    // The split must fall on a separator: ustar rejoins the halves with a `/`.
    let split_at = path
        .char_indices()
        .filter(|(index, character)| {
            *character == '/'
                && *index <= PREFIX_LEN
                && path.len() - index - 1 <= NAME_LEN
                && *index > 0
        })
        .map(|(index, _)| index)
        .next_back()?;

    Some((
        path[..split_at].to_string(),
        path[split_at + 1..].to_string(),
    ))
}

fn build_header(entry: &TarEntry) -> [u8; BLOCK] {
    let mut block = [0_u8; BLOCK];

    let (prefix, name) = split_name(&entry.archive_path).unwrap_or_else(|| {
        // A long-name record already carries the real path; the header keeps a
        // truncated copy for readers that ignore the extension.
        (
            String::new(),
            entry.archive_path.chars().take(NAME_LEN).collect(),
        )
    });

    write_str(&mut block[0..NAME_LEN], &name);
    write_octal(&mut block[100..108], entry.mode as u64, 7);
    write_octal(&mut block[108..116], 0, 7);
    write_octal(&mut block[116..124], 0, 7);
    write_octal(&mut block[124..136], entry.size, 11);
    write_octal(&mut block[136..148], entry.mtime, 11);

    block[156] = match entry.kind {
        EntryKind::Directory => b'5',
        EntryKind::File => b'0',
        EntryKind::Symlink => b'2',
    };

    let link = if entry.link_target.len() > NAME_LEN {
        &entry.link_target[..NAME_LEN]
    } else {
        &entry.link_target
    };
    write_str(&mut block[157..257], link);

    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    write_str(&mut block[265..297], "drop");
    write_str(&mut block[297..329], "drop");
    write_str(&mut block[345..500], &prefix);

    recompute_checksum(&mut block);
    block
}

fn recompute_checksum(block: &mut [u8; BLOCK]) {
    block[148..156].fill(b' ');

    let checksum: u32 = block.iter().map(|byte| u32::from(*byte)).sum();

    let encoded = format!("{checksum:06o}");
    block[148..148 + encoded.len()].copy_from_slice(encoded.as_bytes());
    block[154] = 0;
    block[155] = b' ';
}

fn write_str(field: &mut [u8], value: &str) {
    let bytes = value.as_bytes();
    let take = bytes.len().min(field.len());
    field[..take].copy_from_slice(&bytes[..take]);
}

fn write_octal(field: &mut [u8], value: u64, digits: usize) {
    let encoded = format!("{value:0width$o}", width = digits);
    let bytes = encoded.as_bytes();
    let take = bytes.len().min(digits);
    field[..take].copy_from_slice(&bytes[..take]);
    field[digits] = 0;
}

/// Rejects an archive path that would write outside the extraction root.
///
/// Absolute paths, `..` components, and Windows drive prefixes are all refused
/// rather than normalized away, because a rewritten path silently changes where
/// a hostile archive lands instead of refusing it.
pub fn safe_relative_path(destination: &Path, archive_path: &str) -> Option<PathBuf> {
    let trimmed = archive_path.trim_end_matches('/');

    if trimmed.is_empty() || trimmed.starts_with('/') || trimmed.starts_with('\\') {
        return None;
    }

    let candidate = Path::new(trimmed);
    let mut resolved = destination.to_path_buf();

    for component in candidate.components() {
        match component {
            Component::Normal(part) => {
                let text = part.to_string_lossy();
                if text.contains('\\') {
                    return None;
                }
                resolved.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }

    if resolved == destination {
        return None;
    }

    Some(resolved)
}

/// Whether every directory `path` traverses below `destination` is a real
/// directory rather than a symbolic link.
///
/// Lexical validation alone is not sound. The kernel applies `..` *after*
/// following a symlink, so an entry whose components are all lexically inside
/// the destination still lands outside once an earlier archive entry has
/// planted a link along the way: `a -> .` makes `a/..` escape one level, and a
/// chain of them escapes arbitrarily far. Rather than trying to model that
/// resolution, refuse to descend through a symlink at all.
///
/// Fails closed: an entry that cannot be checked is treated as unsafe.
pub fn traverses_only_real_dirs(destination: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(destination) else {
        return false;
    };

    let components: Vec<Component<'_>> = relative.components().collect();
    let mut current = destination.to_path_buf();

    // The final component is the entry itself, not a directory we descend into.
    for component in components.iter().take(components.len().saturating_sub(1)) {
        current.push(component);

        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return false,
            // Nothing exists at this depth, so nothing deeper can either. The
            // extractor creates real directories the rest of the way down.
            Err(error) if error.kind() == io::ErrorKind::NotFound => return true,
            Err(_) => return false,
        }
    }

    true
}

/// Whether a symlink target stays inside `destination` when resolved from
/// `link_parent`.
///
/// `link_parent` must already have passed [`traverses_only_real_dirs`], so the
/// lexical position of the link matches its real one. The target is then walked
/// component by component, refusing any step into an existing symlink: that is
/// what keeps this lexical `..` accounting equal to the kernel's.
///
/// Fails closed: a target that cannot be checked is treated as unsafe.
pub fn symlink_target_stays_inside(destination: &Path, link_parent: &Path, target: &str) -> bool {
    let target_path = Path::new(target);

    if target_path.is_absolute() {
        return false;
    }

    let base = match link_parent.strip_prefix(destination) {
        Ok(relative) => relative,
        Err(_) => return false,
    };

    let mut resolved: Vec<std::ffi::OsString> = Vec::new();

    for component in base.components() {
        match component {
            Component::Normal(part) => resolved.push(part.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                if resolved.pop().is_none() {
                    return false;
                }
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }

    for component in target_path.components() {
        match component {
            Component::Normal(part) => {
                resolved.push(part.to_os_string());

                // Stepping into a symlink here would make the kernel resolve
                // the rest of this target from somewhere other than where the
                // stack says we are, which is exactly the divergence that lets
                // a later `..` escape.
                let mut here = destination.to_path_buf();
                here.extend(resolved.iter());

                match fs::symlink_metadata(&here) {
                    Ok(metadata) if metadata.file_type().is_symlink() => return false,
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(_) => return false,
                }
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if resolved.pop().is_none() {
                    return false;
                }
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }

    true
}
