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
    tar::{TarPlan, safe_relative_path, symlink_target_stays_inside, traverses_only_real_dirs},
    untar::TarExtractor,
};

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
