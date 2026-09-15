//! Archive round-trip and extraction-safety tests.
//!
//! The size agreement test is the important one: Drop declares the payload
//! length before sending a byte, so a plan whose computed length disagreed with
//! its own output would fail every directory transfer at the last chunk.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use drop_cli::{
    names::Naming,
    tar::{TarPlan, resolve_archive_path, safe_relative_path, symlink_target_stays_inside},
    untar::TarExtractor,
};
// Only the symlink tests use this, and planting a symlink needs Unix.
#[cfg(unix)]
use drop_cli::tar::traverses_only_real_dirs;

fn scratch(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "drop-cli-test-{}-{}-{name}",
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
    fs::write(path, contents).expect("write fixture file");
}

fn sample_tree(root: &Path) {
    write_file(&root.join("readme.md"), b"# sample\n");
    write_file(&root.join("data/numbers.bin"), &vec![7_u8; 4096]);
    write_file(&root.join("data/nested/deep.txt"), b"deep contents");
    // Exactly one block, to catch an off-by-one in the padding maths.
    write_file(&root.join("data/aligned.bin"), &vec![3_u8; 512]);
    // One byte over a block boundary.
    write_file(&root.join("data/unaligned.bin"), &vec![9_u8; 513]);
    write_file(&root.join("empty.txt"), b"");
    fs::create_dir_all(root.join("empty-dir")).expect("empty directory");
}

#[test]
fn declares_exactly_the_number_of_bytes_it_writes() {
    let base = scratch("size");
    let root = base.join("payload");
    sample_tree(&root);

    let plan = TarPlan::scan(&root).expect("scan");
    let mut buffer = Vec::new();
    let written = plan.write_to(&mut buffer, |_| {}).expect("write archive");

    assert_eq!(
        plan.total_bytes(),
        written,
        "the declared length must match the streamed length"
    );
    assert_eq!(
        plan.total_bytes(),
        buffer.len() as u64,
        "the declared length must match the produced bytes"
    );
    assert_eq!(
        buffer.len() % 512,
        0,
        "an archive is a whole number of blocks"
    );

    fs::remove_dir_all(&base).ok();
}

#[test]
fn declares_the_right_length_for_paths_too_long_for_a_ustar_header() {
    let base = scratch("longpath");
    let root = base.join("payload");

    // Deeper than the 100-byte name field and past the 155-byte prefix split,
    // which forces the GNU long-name record path.
    let mut deep = root.clone();
    for index in 0..14 {
        deep = deep.join(format!("directory-segment-number-{index:03}"));
    }
    write_file(&deep.join("file-with-a-fairly-long-name.txt"), b"payload");

    let plan = TarPlan::scan(&root).expect("scan");
    let mut buffer = Vec::new();
    let written = plan.write_to(&mut buffer, |_| {}).expect("write archive");

    assert_eq!(plan.total_bytes(), written);
    assert_eq!(plan.total_bytes(), buffer.len() as u64);

    fs::remove_dir_all(&base).ok();
}

#[test]
fn round_trips_a_tree_through_its_own_extractor() {
    let base = scratch("roundtrip");
    let root = base.join("payload");
    sample_tree(&root);

    let plan = TarPlan::scan(&root).expect("scan");
    let mut buffer = Vec::new();
    plan.write_to(&mut buffer, |_| {}).expect("write archive");

    let destination = base.join("extracted");
    fs::create_dir_all(&destination).expect("destination");

    // Feed the archive in awkward slices to exercise the streaming parser
    // across block and entry boundaries.
    let mut extractor = TarExtractor::new(&destination);
    for slice in buffer.chunks(377) {
        extractor.write(slice).expect("extract slice");
    }

    let extracted = destination.join("payload");
    assert_eq!(
        fs::read_to_string(extracted.join("readme.md")).expect("readme"),
        "# sample\n"
    );
    assert_eq!(
        fs::read(extracted.join("data/numbers.bin")).expect("numbers"),
        vec![7_u8; 4096]
    );
    assert_eq!(
        fs::read_to_string(extracted.join("data/nested/deep.txt")).expect("deep"),
        "deep contents"
    );
    assert_eq!(
        fs::read(extracted.join("data/unaligned.bin")).expect("unaligned"),
        vec![9_u8; 513]
    );
    assert_eq!(
        fs::read(extracted.join("empty.txt")).expect("empty").len(),
        0
    );
    assert!(extracted.join("empty-dir").is_dir());
    assert!(
        extractor.warnings().is_empty(),
        "{:?}",
        extractor.warnings()
    );

    fs::remove_dir_all(&base).ok();
}

#[test]
fn writes_an_archive_gnu_tar_can_read() {
    let Some(tar_binary) = system_tar() else {
        eprintln!("skipping: no system tar available");
        return;
    };

    let base = scratch("gnutar");
    let root = base.join("payload");
    sample_tree(&root);

    let plan = TarPlan::scan(&root).expect("scan");
    let mut archive = fs::File::create(base.join("payload.tar")).expect("archive file");
    plan.write_to(&mut archive, |_| {}).expect("write archive");
    archive.flush().expect("flush archive");
    drop(archive);

    let destination = base.join("extracted");
    fs::create_dir_all(&destination).expect("destination");

    let output = std::process::Command::new(&tar_binary)
        .arg("-xf")
        .arg(base.join("payload.tar"))
        .arg("-C")
        .arg(&destination)
        .output()
        .expect("run system tar");

    assert!(
        output.status.success(),
        "system tar rejected the archive: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        fs::read(destination.join("payload/data/unaligned.bin")).expect("unaligned"),
        vec![9_u8; 513]
    );
    assert_eq!(
        fs::read_to_string(destination.join("payload/data/nested/deep.txt")).expect("deep"),
        "deep contents"
    );

    fs::remove_dir_all(&base).ok();
}

fn system_tar() -> Option<PathBuf> {
    ["/usr/bin/tar", "/bin/tar"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
}

#[test]
fn refuses_archive_paths_that_escape_the_destination() {
    let destination = Path::new("/tmp/destination");

    for hostile in [
        "../escape.txt",
        "nested/../../escape.txt",
        "/etc/passwd",
        "..",
        "./",
        "",
    ] {
        assert!(
            safe_relative_path(destination, hostile).is_none(),
            "{hostile} must be refused"
        );
    }

    assert_eq!(
        safe_relative_path(destination, "payload/data/file.txt"),
        Some(destination.join("payload/data/file.txt"))
    );
    assert_eq!(
        safe_relative_path(destination, "payload/./file.txt"),
        Some(destination.join("payload/file.txt"))
    );
}

#[test]
fn refuses_symlinks_whose_target_leaves_the_destination() {
    let destination = Path::new("/tmp/destination");

    assert!(!symlink_target_stays_inside(
        destination,
        &destination.join("payload"),
        "/etc/passwd"
    ));
    assert!(!symlink_target_stays_inside(
        destination,
        &destination.join("payload"),
        "../../elsewhere"
    ));
    assert!(symlink_target_stays_inside(
        destination,
        &destination.join("payload/data"),
        "../readme.md"
    ));
    assert!(symlink_target_stays_inside(
        destination,
        &destination.join("payload"),
        "data/file.txt"
    ));
}

#[test]
fn does_not_write_outside_the_destination_when_extracting_a_hostile_archive() {
    let base = scratch("hostile");
    let destination = base.join("extracted");
    fs::create_dir_all(&destination).expect("destination");

    let archive = hostile_archive("../escaped.txt", b"owned");

    let mut extractor = TarExtractor::new(&destination);
    extractor
        .write(&archive)
        .expect("extraction must not fail hard");

    assert!(
        !base.join("escaped.txt").exists(),
        "a `..` entry must not write outside the destination"
    );
    assert_eq!(extractor.files_written(), 0);
    assert!(
        extractor
            .warnings()
            .iter()
            .any(|warning| warning.contains("outside the destination")),
        "the refusal must be reported: {:?}",
        extractor.warnings()
    );

    fs::remove_dir_all(&base).ok();
}

/// Builds a single-entry archive with an arbitrary, possibly hostile, path.
fn hostile_archive(name: &str, contents: &[u8]) -> Vec<u8> {
    let mut block = [0_u8; 512];

    block[..name.len()].copy_from_slice(name.as_bytes());
    block[100..108].copy_from_slice(b"0000644\0");
    block[108..116].copy_from_slice(b"0000000\0");
    block[116..124].copy_from_slice(b"0000000\0");

    let size = format!("{:011o}\0", contents.len());
    block[124..136].copy_from_slice(size.as_bytes());
    block[136..148].copy_from_slice(b"00000000000\0");
    block[156] = b'0';
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");

    block[148..156].fill(b' ');
    let checksum: u32 = block.iter().map(|byte| u32::from(*byte)).sum();
    let encoded = format!("{checksum:06o}");
    block[148..148 + encoded.len()].copy_from_slice(encoded.as_bytes());
    block[154] = 0;
    block[155] = b' ';

    let mut archive = block.to_vec();
    let mut padded = contents.to_vec();
    padded.resize(contents.len().div_ceil(512) * 512, 0);
    archive.extend_from_slice(&padded);
    archive.extend_from_slice(&[0_u8; 1024]);
    archive
}

/// Builds a multi-entry archive from `(name, typeflag, link_target, contents)`.
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

/// A chain of symlinks must not be able to walk the extractor out of the
/// destination.
///
/// Every entry name here is lexically clean — no `..`, nothing absolute — so
/// name validation alone accepts all of them. The escape comes from the kernel
/// applying `..` *after* following a link: with `a -> .` on disk, `a/..` is one
/// level above where the path text says it is, and each further link adds
/// another level. This is the case that a purely lexical check misses.
#[test]
#[cfg(unix)]
fn does_not_follow_a_symlink_chain_out_of_the_destination() {
    let base = scratch("symlink-chain");
    let destination = base.join("extracted");
    fs::create_dir_all(&destination).expect("destination");

    let archive = archive_of(&[
        ("arch/", b'5', "", b"".as_slice()),
        // Lexically inside, but on disk this points back at `arch` itself.
        ("arch/a", b'2', ".", b"".as_slice()),
        // Lexically pops to the destination root; really lands outside it.
        ("arch/a/b", b'2', "../..", b"".as_slice()),
        // No `..` anywhere in this name.
        ("arch/a/b/pwned.txt", b'0', "", b"OWNED".as_slice()),
    ]);

    let mut extractor = TarExtractor::new(&destination);
    extractor
        .write(&archive)
        .expect("extraction must not fail hard");

    assert!(
        !base.join("pwned.txt").exists(),
        "a symlink chain must not write outside the destination"
    );
    assert_eq!(
        extractor.files_written(),
        0,
        "nothing should have been written: {:?}",
        extractor.warnings()
    );
    assert!(
        extractor
            .warnings()
            .iter()
            .any(|warning| warning.contains("symbolic link")),
        "the refusal must be reported: {:?}",
        extractor.warnings()
    );

    fs::remove_dir_all(&base).ok();
}

/// The same escape, but reaching two levels up rather than one.
///
/// Depth is chosen by the attacker: every extra link in the chain buys another
/// level, so a fix that only handles a single hop is not a fix.
#[test]
#[cfg(unix)]
fn does_not_follow_a_longer_symlink_chain_out_of_the_destination() {
    let base = scratch("symlink-chain-deep");
    let destination = base.join("extracted");
    fs::create_dir_all(&destination).expect("destination");

    let outside = base
        .parent()
        .expect("scratch parent")
        .join("pwned-deep.txt");
    fs::remove_file(&outside).ok();

    let archive = archive_of(&[
        ("arch/", b'5', "", b"".as_slice()),
        ("arch/a", b'2', ".", b"".as_slice()),
        ("arch/a/b", b'2', ".", b"".as_slice()),
        ("arch/a/b/c", b'2', "../../..", b"".as_slice()),
        ("arch/a/b/c/pwned-deep.txt", b'0', "", b"OWNED".as_slice()),
    ]);

    let mut extractor = TarExtractor::new(&destination);
    extractor
        .write(&archive)
        .expect("extraction must not fail hard");

    assert!(
        !outside.exists(),
        "a longer symlink chain must not write outside the destination"
    );
    assert_eq!(extractor.files_written(), 0);

    fs::remove_dir_all(&base).ok();
}

/// A symlink target must not reach outside by traversing another symlink.
///
/// `a -> .` followed by a target of `a/../..` resolves inside on paper and
/// outside on disk, so the target check has to look at what is on the
/// filesystem, not only at the text of the target.
#[test]
#[cfg(unix)]
fn refuses_a_symlink_target_that_escapes_through_another_symlink() {
    let base = scratch("symlink-target-chain");
    let destination = base.join("extracted");
    fs::create_dir_all(destination.join("arch")).expect("destination");
    std::os::unix::fs::symlink(".", destination.join("arch/a")).expect("planted link");

    assert!(
        !symlink_target_stays_inside(&destination, &destination.join("arch"), "a/../.."),
        "a target that walks through a symlink must be refused"
    );

    fs::remove_dir_all(&base).ok();
}

#[test]
#[cfg(unix)]
fn refuses_to_descend_through_a_symlinked_parent() {
    let base = scratch("real-dirs");
    let destination = base.join("extracted");
    fs::create_dir_all(destination.join("real")).expect("destination");
    std::os::unix::fs::symlink(".", destination.join("linked")).expect("planted link");

    assert!(traverses_only_real_dirs(
        &destination,
        &destination.join("real/file.txt")
    ));
    assert!(traverses_only_real_dirs(
        &destination,
        &destination.join("absent/deeper/file.txt")
    ));
    assert!(
        !traverses_only_real_dirs(&destination, &destination.join("linked/file.txt")),
        "a symlinked parent must not be descended into"
    );
    assert!(
        !traverses_only_real_dirs(Path::new("/tmp/elsewhere"), &destination.join("file.txt")),
        "a path outside the destination must be refused"
    );

    fs::remove_dir_all(&base).ok();
}

/// Extraction must not quietly replace files the receiver already has.
#[test]
fn keeps_existing_files_unless_overwriting_is_requested() {
    let base = scratch("overwrite");
    let destination = base.join("extracted");
    write_file(&destination.join("keep.txt"), b"original");

    let archive = archive_of(&[("keep.txt", b'0', "", b"replaced".as_slice())]);

    let mut extractor = TarExtractor::new(&destination);
    extractor.write(&archive).expect("extraction");

    assert_eq!(
        fs::read_to_string(destination.join("keep.txt")).expect("read"),
        "original",
        "an existing file must survive extraction by default"
    );
    assert_eq!(extractor.files_written(), 0);
    assert!(
        extractor
            .warnings()
            .iter()
            .any(|warning| warning.contains("already exists")),
        "the skip must be reported: {:?}",
        extractor.warnings()
    );

    let mut forced = TarExtractor::new(&destination).overwriting(true);
    forced.write(&archive).expect("forced extraction");

    assert_eq!(
        fs::read_to_string(destination.join("keep.txt")).expect("read"),
        "replaced",
        "--force must replace the file"
    );
    assert_eq!(forced.files_written(), 1);

    fs::remove_dir_all(&base).ok();
}

/// A file entry must never be written through a symlink already sitting at its
/// path, even one whose target is inside the destination.
#[test]
#[cfg(unix)]
fn replaces_a_symlink_rather_than_writing_through_it() {
    let base = scratch("write-through");
    let destination = base.join("extracted");
    fs::create_dir_all(&destination).expect("destination");
    write_file(&destination.join("secret.txt"), b"original");
    std::os::unix::fs::symlink("secret.txt", destination.join("entry.txt")).expect("planted link");

    let archive = archive_of(&[("entry.txt", b'0', "", b"replaced".as_slice())]);

    let mut extractor = TarExtractor::new(&destination).overwriting(true);
    extractor.write(&archive).expect("extraction");

    assert_eq!(
        fs::read_to_string(destination.join("secret.txt")).expect("read"),
        "original",
        "the link's target must not be written through"
    );
    assert_eq!(
        fs::read_to_string(destination.join("entry.txt")).expect("read"),
        "replaced"
    );

    fs::remove_dir_all(&base).ok();
}

/// A directory entry that declares content must not desynchronize the parser.
#[test]
fn consumes_declared_content_on_entry_types_that_create_no_file() {
    let base = scratch("desync");
    let destination = base.join("extracted");
    fs::create_dir_all(&destination).expect("destination");

    // A directory header claiming 512 bytes of content, followed by a block of
    // junk. A parser that ignored the size would read that junk as a header.
    let archive = archive_of(&[
        ("bogus/", b'5', "", vec![0xAB_u8; 512].as_slice()),
        ("after.txt", b'0', "", b"real".as_slice()),
    ]);

    let mut extractor = TarExtractor::new(&destination);
    extractor.write(&archive).expect("extraction");

    assert_eq!(
        fs::read_to_string(destination.join("after.txt")).expect("read"),
        "real",
        "the entry after a sized directory header must still parse"
    );

    fs::remove_dir_all(&base).ok();
}

/// Builds a one-entry archive whose name travels in a GNU long-name record,
/// for names longer than a ustar header can hold.
fn long_name_archive(name: &str, contents: &[u8]) -> Vec<u8> {
    let mut archive = archive_of(&[("././@LongLink", b'L', "", name.as_bytes())]);
    // `archive_of` ends every archive with its trailer; this one continues.
    archive.truncate(archive.len() - 1024);
    archive.extend(archive_of(&[("placeholder", b'0', "", contents)]));
    archive
}

/// Every archive path is split on `/`, the only separator the format has,
/// so every platform judges the same components. A backslash inside one is
/// refused everywhere, because on Windows it would be a separator.
#[test]
fn archive_paths_are_judged_the_same_way_on_every_platform() {
    let destination = Path::new("destination");

    for naming in [Naming::AsSent, Naming::Windows] {
        for hostile in [
            "a\\..\\b",
            "..\\escape.txt",
            "\\\\server\\share\\x",
            "\\\\?\\C:\\x",
            "safe/..\\..\\x",
        ] {
            assert!(
                resolve_archive_path(destination, hostile, naming).is_none(),
                "{hostile} must be refused under {naming:?}"
            );
        }
    }

    // A colon is an ordinary character on Unix and a stream or a drive on
    // Windows. Neither reading lets it leave the destination.
    let as_sent = resolve_archive_path(destination, "C:x/notes", Naming::AsSent);
    if cfg!(windows) {
        // Stored as sent on Windows, `C:x` is a drive-relative path, and
        // pushing it replaces the destination rather than extending it. Names
        // are never stored as sent on Windows, but if they were, the final
        // check that the result is still inside the destination catches it.
        // Found by the first Windows run of this test.
        assert!(as_sent.is_none(), "a drive prefix escaped: {as_sent:?}");
    } else {
        let as_sent = as_sent.expect("an ordinary name on this platform");
        assert_eq!(as_sent.stored_as, "C:x/notes");
        assert!(!as_sent.renamed);
    }

    let windows =
        resolve_archive_path(destination, "C:x/notes", Naming::Windows).expect("accepted");
    assert_eq!(windows.stored_as, "C_x/notes");
    assert!(windows.renamed);
    assert_eq!(windows.path, destination.join("C_x").join("notes"));
}

/// The rewriting that protects a Windows receiver, run through the real
/// extractor on whatever platform the tests are on. The names below are ones
/// Windows would read as a stream, a device, or a name that loses its last
/// character. Here they must arrive as ordinary files with their new names
/// reported.
#[test]
fn windows_naming_stores_every_entry_as_an_ordinary_file_and_reports_it() {
    let base = scratch("windows-naming");
    let destination = base.join("extracted");
    fs::create_dir_all(&destination).expect("destination");

    let archive = archive_of(&[
        ("meeting/", b'5', "", b"".as_slice()),
        ("meeting/10:30 standup.md", b'0', "", b"agenda".as_slice()),
        ("meeting/CON", b'0', "", b"not a device".as_slice()),
        (
            "meeting/nul.txt",
            b'0',
            "",
            b"not a device either".as_slice(),
        ),
        ("meeting/report.", b'0', "", b"trailing dot".as_slice()),
        (
            "meeting/notes.txt:hidden",
            b'0',
            "",
            b"not a stream".as_slice(),
        ),
    ]);

    let mut extractor = TarExtractor::new(&destination).naming(Naming::Windows);
    extractor.write(&archive).expect("extraction");

    for (stored, contents) in [
        ("10_30 standup.md", "agenda"),
        ("CON_", "not a device"),
        ("nul_.txt", "not a device either"),
        ("report_", "trailing dot"),
        ("notes.txt_hidden", "not a stream"),
    ] {
        assert_eq!(
            fs::read_to_string(destination.join("meeting").join(stored))
                .unwrap_or_else(|error| panic!("{stored}: {error}")),
            contents
        );
    }

    assert_eq!(extractor.files_written(), 5);
    let renames = extractor
        .warnings()
        .iter()
        .filter(|warning| warning.starts_with("renamed "))
        .count();
    assert_eq!(
        renames,
        5,
        "every rewrite must be reported: {:?}",
        extractor.warnings()
    );

    fs::remove_dir_all(&base).ok();
}

/// Two names that rewrite to the same name do not overwrite each other. The
/// second is skipped like any other existing file, and says so.
#[test]
fn a_rewrite_that_collides_keeps_the_first_file() {
    let base = scratch("windows-collision");
    let destination = base.join("extracted");
    fs::create_dir_all(&destination).expect("destination");

    let archive = archive_of(&[
        ("a_b", b'0', "", b"first".as_slice()),
        ("a:b", b'0', "", b"second".as_slice()),
    ]);

    let mut extractor = TarExtractor::new(&destination).naming(Naming::Windows);
    extractor.write(&archive).expect("extraction");

    assert_eq!(
        fs::read_to_string(destination.join("a_b")).expect("read"),
        "first"
    );
    assert!(
        extractor
            .warnings()
            .iter()
            .any(|warning| warning.contains("already exists")),
        "{:?}",
        extractor.warnings()
    );

    fs::remove_dir_all(&base).ok();
}

/// A name the filesystem refuses skips that entry and nothing else.
///
/// A 300-byte component is refused by every mainstream filesystem, which is
/// what makes this runnable everywhere. It stands in for an exFAT drive
/// refusing `a:b` on Linux, or a Windows name the rewriting does not cover.
#[test]
fn a_name_the_filesystem_refuses_skips_only_that_entry() {
    let base = scratch("refused-name");
    let destination = base.join("extracted");
    fs::create_dir_all(&destination).expect("destination");

    let mut archive = long_name_archive(&"x".repeat(300), b"unstorable");
    archive.truncate(archive.len() - 1024);
    archive.extend(archive_of(&[(
        "after.txt",
        b'0',
        "",
        b"still arrives".as_slice(),
    )]));

    let mut extractor = TarExtractor::new(&destination);
    extractor
        .write(&archive)
        .expect("one unstorable name must not end the extraction");

    assert_eq!(
        fs::read_to_string(destination.join("after.txt")).expect("the entry after it"),
        "still arrives"
    );
    assert_eq!(extractor.files_written(), 1);
    assert!(
        extractor
            .warnings()
            .iter()
            .any(|warning| warning.contains("refused the name")),
        "{:?}",
        extractor.warnings()
    );

    fs::remove_dir_all(&base).ok();
}

/// A symlink in a folder sent from Linux or macOS must not stop a Windows
/// receiver from getting everything else. Before this, `create_symlink`'s
/// error ended the extraction at the first link.
#[test]
#[cfg(not(unix))]
fn a_symlink_the_receiver_cannot_create_is_skipped_not_fatal() {
    let base = scratch("symlink-unsupported");
    let destination = base.join("extracted");
    fs::create_dir_all(&destination).expect("destination");

    let archive = archive_of(&[
        ("project/", b'5', "", b"".as_slice()),
        ("project/.bin/", b'5', "", b"".as_slice()),
        ("project/.bin/tool", b'2', "../tool.js", b"".as_slice()),
        ("project/tool.js", b'0', "", b"console.log(1)".as_slice()),
    ]);

    let mut extractor = TarExtractor::new(&destination);
    extractor
        .write(&archive)
        .expect("a link this system cannot create must not end the extraction");

    assert_eq!(
        fs::read_to_string(destination.join("project").join("tool.js")).expect("read"),
        "console.log(1)"
    );
    assert!(
        extractor
            .warnings()
            .iter()
            .any(|warning| warning.contains("cannot create symbolic links")),
        "{:?}",
        extractor.warnings()
    );

    fs::remove_dir_all(&base).ok();
}

/// On a case-insensitive filesystem `makefile` is `Makefile`, so the second
/// entry is an existing file and is kept rather than overwritten.
#[test]
#[cfg(any(windows, target_os = "macos"))]
fn a_name_differing_only_in_case_does_not_overwrite_on_a_case_insensitive_disk() {
    let base = scratch("case-insensitive");
    let destination = base.join("extracted");
    fs::create_dir_all(&destination).expect("destination");

    let archive = archive_of(&[
        ("Makefile", b'0', "", b"first".as_slice()),
        ("makefile", b'0', "", b"second".as_slice()),
    ]);

    let mut extractor = TarExtractor::new(&destination);
    extractor.write(&archive).expect("extraction");

    assert_eq!(
        fs::read_to_string(destination.join("Makefile")).expect("read"),
        "first"
    );
    assert_eq!(extractor.files_written(), 1);

    fs::remove_dir_all(&base).ok();
}

/// The real thing, on Windows: names that Windows would read as a stream on
/// an existing file, a device, a drive or a shortened name. Nothing outside
/// the destination changes, and the receiver's own file keeps its contents.
#[test]
#[cfg(windows)]
fn hostile_windows_names_cannot_reach_a_stream_a_device_or_an_existing_file() {
    let base = scratch("windows-hostile");
    let destination = base.join("extracted");
    fs::create_dir_all(&destination).expect("destination");
    write_file(&destination.join("mine.txt"), b"the receiver's own file");

    let archive = archive_of(&[
        ("mine.txt::$DATA", b'0', "", b"overwritten".as_slice()),
        ("mine.txt:hidden", b'0', "", b"smuggled".as_slice()),
        ("CON", b'0', "", b"device".as_slice()),
        ("nul.txt", b'0', "", b"device".as_slice()),
        ("report.", b'0', "", b"dot".as_slice()),
        ("notes ", b'0', "", b"space".as_slice()),
        ("C:x", b'0', "", b"drive".as_slice()),
        ("a\\..\\..\\escape.txt", b'0', "", b"escape".as_slice()),
    ]);

    let mut extractor = TarExtractor::new(&destination);
    extractor.write(&archive).expect("extraction");

    assert_eq!(
        fs::read_to_string(destination.join("mine.txt")).expect("read"),
        "the receiver's own file",
        "an entry reached the receiver's existing file"
    );
    assert!(
        fs::read(destination.join("mine.txt:hidden")).is_err(),
        "an entry created an alternate data stream on the receiver's file"
    );

    for stored in [
        "mine.txt__$DATA",
        "mine.txt_hidden",
        "CON_",
        "nul_.txt",
        "report_",
        "notes_",
        "C_x",
    ] {
        assert!(
            destination.join(stored).is_file(),
            "{stored} was not written"
        );
    }

    assert!(!base.join("escape.txt").exists());
    let outside: Vec<_> = fs::read_dir(&base)
        .expect("list")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(outside, vec![std::ffi::OsString::from("extracted")]);

    fs::remove_dir_all(&base).ok();
}

/// A Windows sender's link targets use backslashes, and every receiver reads
/// tar targets with slashes. Relative ones are rewritten; absolute ones would
/// dangle anywhere else and are left out.
#[test]
fn a_windows_link_target_is_recorded_portably() {
    use drop_cli::tar::portable_link_target;

    assert_eq!(
        portable_link_target("..\\shared\\config", true).as_deref(),
        Some("../shared/config")
    );
    assert_eq!(
        portable_link_target("sibling", true).as_deref(),
        Some("sibling")
    );

    for absolute in [
        "C:\\Users\\me\\file",
        "c:relative-to-drive",
        "\\\\server\\share",
        "\\rooted",
        "/rooted",
    ] {
        assert_eq!(portable_link_target(absolute, true), None, "{absolute}");
    }

    // Anywhere else a backslash is part of a name, and is kept.
    assert_eq!(portable_link_target("a\\b", false).as_deref(), Some("a\\b"));
}
