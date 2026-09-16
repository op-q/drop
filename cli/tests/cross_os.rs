//! Folders archived on one operating system and received on another.
//!
//! The wire is the same everywhere: JSON, big-endian integers, sealed bytes.
//! What differs by platform is how a sender reads a tree and how a receiver
//! writes one, so that is where a cross-OS transfer can go wrong. Two runners
//! cannot swap a transfer code mid-run, so these tests split a transfer in
//! two. One test, on each OS, builds a tree using every kind of name and file
//! that OS can hold and archives it exactly as `drop send` would. The other,
//! on each OS, receives every OS's archive through the real receive path over
//! an in-process relay and checks what landed.
//!
//! Both are `#[ignore]`d. They run in `.github/workflows/cross-os.yml`, which
//! moves the archives between runners:
//!
//! ```text
//! DROP_FIXTURE_OUT=dir cargo test -p drop-cli --test cross_os -- --ignored produce
//! DROP_FIXTURE_IN=dir  cargo test -p drop-cli --test cross_os -- --ignored consume
//! ```
//!
//! See `docs/plans/cross-platform-plan-2026-09-14.md`, phase 4.

use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use drop_cli::{
    cancel::Cancel,
    consent::Acceptance,
    names::Naming,
    recv::{self, ReceiveOptions},
    send::{self, SendOptions},
    tar::TarPlan,
};
use serde_json::{Value, json};
use tokio::sync::oneshot;

/// The OS the tests are running on, as it names fixtures.
fn this_os() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// Contents for a fixture file, derived from its path so the consumer can
/// regenerate them without the producer shipping a copy or a hash.
fn contents_for(path: &str, len: usize) -> Vec<u8> {
    let mut state = path.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    }) | 1;

    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

struct Tree {
    root: PathBuf,
    entries: Vec<Value>,
}

impl Tree {
    fn file(&mut self, relative: &str, len: usize, executable: bool) {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().expect("a parent")).expect("parent directories");
        fs::write(&path, contents_for(relative, len)).expect("fixture file");

        #[cfg(unix)]
        if executable {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("mode");
        }

        self.entries.push(json!({
            "kind": "file",
            "path": relative,
            "len": len,
            "executable": executable && cfg!(unix),
        }));
    }

    fn directory(&mut self, relative: &str) {
        fs::create_dir_all(self.root.join(relative)).expect("fixture directory");
        self.entries
            .push(json!({ "kind": "dir", "path": relative }));
    }

    #[cfg(unix)]
    fn symlink(&mut self, relative: &str, target: &str) {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().expect("a parent")).expect("parent directories");
        std::os::unix::fs::symlink(target, &path).expect("fixture symlink");
        self.entries
            .push(json!({ "kind": "symlink", "path": relative, "target": target }));
    }
}

fn scratch(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "drop-cross-os-{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&base).expect("scratch directory");
    base
}

/// Builds this OS's fixture tree and archives it the way `drop send` does.
#[test]
#[ignore = "run by .github/workflows/cross-os.yml, which moves archives between runners"]
fn produce() {
    let out = PathBuf::from(std::env::var("DROP_FIXTURE_OUT").expect("DROP_FIXTURE_OUT"));
    fs::create_dir_all(&out).expect("output directory");

    let base = scratch("produce");
    let mut tree = Tree {
        root: base.join("tree"),
        entries: Vec::new(),
    };

    tree.file("readme.md", 200, false);
    tree.file("empty.txt", 0, false);
    tree.file("nested/deeper/data.bin", 3 * 1024 * 1024 + 7, false);
    tree.directory("empty-dir");
    tree.file("Łódź 東京 🎉.txt", 64, false);
    // Longer than a ustar header holds, so it travels in a long-name record.
    tree.file(&format!("{}.txt", "long-name-".repeat(12)), 32, false);
    // Deeper than Windows' historic 260-character path limit, once the
    // runner's temporary directory is in front of it.
    let deep: Vec<String> = (0..12)
        .map(|level| format!("level-{level:02}-padding"))
        .collect();
    tree.file(&format!("{}/bottom.txt", deep.join("/")), 16, false);
    tree.file("scripts/run.sh", 40, true);

    // What only a Unix sender can put in a folder.
    #[cfg(unix)]
    {
        tree.symlink("links/to-readme", "../readme.md");
        tree.file("10:30 standup.md", 48, false);
        tree.file("nul.txt", 8, false);
        tree.file("trailing dot.", 8, false);
    }

    let plan = TarPlan::scan(&tree.root).expect("scan");
    let name = format!("from-{}", this_os());

    let mut archive = fs::File::create(out.join(format!("{name}.tar"))).expect("archive");
    plan.write_to(&mut archive, |warning| {
        panic!("fixture changed while archiving: {warning}")
    })
    .expect("archived");

    let tar_bytes = fs::read(out.join(format!("{name}.tar"))).expect("read back");
    let mut gzip = flate2::write::GzEncoder::new(
        fs::File::create(out.join(format!("{name}.tar.gz"))).expect("gzip archive"),
        flate2::Compression::new(6),
    );
    std::io::Write::write_all(&mut gzip, &tar_bytes).expect("compressed");
    gzip.finish().expect("gzip finished");

    fs::write(
        out.join(format!("{name}.json")),
        serde_json::to_vec_pretty(&json!({ "producer": this_os(), "entries": tree.entries }))
            .expect("manifest"),
    )
    .expect("manifest written");

    fs::remove_dir_all(&base).ok();
}

async fn spawn_relay() -> String {
    let state = api::build_state();
    api::start_background_services(state.clone());
    let app = api::build_app(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("relay listener");
    let addr: SocketAddr = listener.local_addr().expect("relay address");
    tokio::spawn(async move { api::serve(listener, app).await.expect("relay") });
    tokio::time::sleep(Duration::from_millis(50)).await;
    format!("http://{addr}")
}

/// Sends `archive` and receives it, extracting, into `destination`.
async fn transfer(origin: &str, archive: &Path, destination: &Path) {
    let (code_tx, code_rx) = oneshot::channel();
    let mut code_tx = Some(code_tx);

    let sender = tokio::spawn({
        let origin = origin.to_string();
        let archive = archive.to_path_buf();
        async move {
            send::run_cancellable(
                &archive,
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
                Cancel::default(),
            )
            .await
            .map_err(|error| error.to_string())
        }
    });

    let code = tokio::time::timeout(Duration::from_secs(30), code_rx)
        .await
        .expect("a code in time")
        .expect("a code");

    recv::run_deciding(
        &code,
        &ReceiveOptions {
            path: drop_cli::direct::Path::Relay,
            status: false,
            rendezvous: drop_cli::direct::Rendezvous::default(),
            origin: Some(origin.to_string()),
            out_dir: destination.to_path_buf(),
            extract: true,
            force: false,
            acceptance: Acceptance::Yes,
        },
        &mut Acceptance::Yes,
        Cancel::default(),
    )
    .await
    .unwrap_or_else(|error| panic!("receiving {} failed: {error}", archive.display()));

    sender
        .await
        .expect("sender task")
        .unwrap_or_else(|error| panic!("sending {} failed: {error}", archive.display()));
}

/// Where an entry from the archive is expected to land on this OS.
fn landed(destination: &Path, relative: &str) -> PathBuf {
    let naming = Naming::for_this_platform();
    let mut path = destination.join("tree");
    for component in relative.split('/') {
        path.push(naming.component(component).as_ref());
    }
    path
}

/// Receives every OS's archive, tar and gzipped tar, and checks what landed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run by .github/workflows/cross-os.yml, which moves archives between runners"]
async fn consume() {
    let input = PathBuf::from(std::env::var("DROP_FIXTURE_IN").expect("DROP_FIXTURE_IN"));
    let origin = spawn_relay().await;

    let mut manifests: Vec<PathBuf> = fs::read_dir(&input)
        .expect("fixture directory")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    manifests.sort();
    assert!(!manifests.is_empty(), "no fixtures in {}", input.display());

    let mut report = Vec::new();

    for manifest_path in manifests {
        let manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).expect("manifest")).expect("json");
        let producer = manifest["producer"].as_str().expect("producer").to_string();
        let stem = manifest_path
            .file_stem()
            .expect("stem")
            .to_string_lossy()
            .into_owned();

        for suffix in ["tar", "tar.gz"] {
            let archive = input.join(format!("{stem}.{suffix}"));
            let destination = scratch(&format!("{stem}-{}", suffix.replace('.', "-")));
            transfer(&origin, &archive, &destination).await;

            for entry in manifest["entries"].as_array().expect("entries") {
                let relative = entry["path"].as_str().expect("path");
                let path = landed(&destination, relative);
                let what = format!("{producer} {suffix} -> {}: {relative}", this_os());

                match entry["kind"].as_str().expect("kind") {
                    "file" => {
                        let len = entry["len"].as_u64().expect("len") as usize;
                        let arrived = fs::read(&path)
                            .unwrap_or_else(|error| panic!("{what}: missing ({error})"));
                        assert!(
                            arrived == contents_for(relative, len),
                            "{what}: contents differ"
                        );

                        #[cfg(unix)]
                        if entry["executable"].as_bool() == Some(true) {
                            use std::os::unix::fs::PermissionsExt;
                            let mode = fs::metadata(&path).expect("metadata").permissions().mode();
                            assert!(mode & 0o111 != 0, "{what}: lost its executable bit");
                        }
                    }
                    "dir" => assert!(path.is_dir(), "{what}: directory missing"),
                    "symlink" => {
                        if cfg!(unix) {
                            let target = fs::read_link(&path)
                                .unwrap_or_else(|error| panic!("{what}: link missing ({error})"));
                            assert_eq!(
                                target.to_string_lossy(),
                                entry["target"].as_str().expect("target"),
                                "{what}: link target changed"
                            );
                        } else {
                            // Skipped with a warning on a platform that
                            // cannot create links, never half-made.
                            assert!(
                                fs::symlink_metadata(&path).is_err(),
                                "{what}: something was created where a link was skipped"
                            );
                        }
                    }
                    other => panic!("{what}: unknown entry kind {other}"),
                }
            }

            report.push(format!("{producer} {suffix} -> {}: ok", this_os()));
            fs::remove_dir_all(&destination).ok();
        }
    }

    for line in report {
        println!("{line}");
    }
}
