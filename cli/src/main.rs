//! `drop` — terminal client for the Drop ephemeral file-transfer relay.

use std::{path::PathBuf, process::ExitCode};

use drop_cli::{client, direct, recv, send};

const USAGE: &str = "\
drop — send a file or folder between two terminals

USAGE
    drop send <PATH> [OPTIONS]
    drop recv <CODE> [OPTIONS]

COMMANDS
    send <PATH>     Share a file or folder and print a one-time code.
                    A folder is streamed as a tar archive.
    recv <CODE>     Receive using the code shown by the sender.

OPTIONS
    -s, --server <URL>   Relay to use [env: DROP_SERVER]
                         [default: https://api.drop.lifbom.com]
    -t, --transport <T>  p2p, relay, or auto [default: auto]
                         p2p connects the two terminals directly and involves
                         no Drop server at all; relay forwards through one,
                         which is what a browser peer needs. auto tries p2p
                         and falls back. p2p fails rather than falling back.
    -c, --compress       (send) Compress before sending. Useful for source
                         trees and documents; skip it for media that is already
                         compressed.
        --level <N>      (send) Compression level, 1-9 [default: 6]
    -o, --out <DIR>      (recv) Where to write [default: current directory]
        --no-extract     (recv) Write the archive as a file instead of
                         unpacking it
    -f, --force          (recv) Overwrite an existing file
    -h, --help           Show this help
    -V, --version        Show the version

NOTES
    Both peers must be online at the same time: Drop never stores the file.
    A code is single use and expires after five idle minutes.

    Every transfer is encrypted end to end with a key derived from the code,
    on either transport. What the transport changes is who carries the bytes,
    not who can read them.
";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    match run(arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: Vec<String>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Some(command) = arguments.first().map(String::as_str) else {
        eprint!("{USAGE}");
        return Err("no command given".into());
    };

    match command {
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(())
        }
        "-V" | "--version" | "version" => {
            println!("drop {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "send" => {
            let options = parse(&arguments[1..])?;
            let path = options
                .positional
                .clone()
                .ok_or("send needs a file or folder path: drop send <PATH>")?;

            let compress = options
                .compress
                .then(|| options.level.unwrap_or(6))
                .map(|level| level.clamp(1, 9));

            runtime()?.block_on(send::run(
                &PathBuf::from(path),
                send::SendOptions::printing(options.origin(), compress, options.path()?),
            ))
        }
        "recv" | "receive" | "get" => {
            let options = parse(&arguments[1..])?;
            let code = options
                .positional
                .clone()
                .ok_or("recv needs the sender's code: drop recv <CODE>")?;

            runtime()?.block_on(recv::run(
                &code,
                recv::ReceiveOptions {
                    origin: options.origin(),
                    path: options.path()?,
                    out_dir: options
                        .out
                        .clone()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from(".")),
                    extract: !options.no_extract,
                    force: options.force,
                },
            ))
        }
        other => {
            eprint!("{USAGE}");
            Err(format!("unknown command `{other}`").into())
        }
    }
}

fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
}

#[derive(Default)]
struct Options {
    positional: Option<String>,
    server: Option<String>,
    transport: Option<String>,
    out: Option<String>,
    level: Option<u32>,
    compress: bool,
    no_extract: bool,
    force: bool,
}

impl Options {
    fn origin(&self) -> String {
        let configured = self
            .server
            .clone()
            .or_else(|| std::env::var("DROP_SERVER").ok())
            .unwrap_or_else(|| client::DEFAULT_SERVER.to_string());

        client::normalize_origin(&configured)
    }

    /// Which carrier to use, from the flag or the environment.
    ///
    /// Defaults to `auto`, which is the only value most people should ever
    /// need: a person sending a file should not have to know what a DHT is.
    fn path(&self) -> Result<direct::Path, Box<dyn std::error::Error + Send + Sync>> {
        let configured = self
            .transport
            .clone()
            .or_else(|| std::env::var("DROP_TRANSPORT").ok())
            .unwrap_or_else(|| "auto".to_string());

        direct::Path::parse(configured.trim()).map_err(Into::into)
    }
}

fn parse(arguments: &[String]) -> Result<Options, Box<dyn std::error::Error + Send + Sync>> {
    let mut options = Options::default();
    let mut index = 0;

    while index < arguments.len() {
        let argument = arguments[index].as_str();

        let mut value = |name: &str| -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            index += 1;
            arguments
                .get(index)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value").into())
        };

        match argument {
            "-s" | "--server" => options.server = Some(value("--server")?),
            "-t" | "--transport" => options.transport = Some(value("--transport")?),
            "-o" | "--out" => options.out = Some(value("--out")?),
            "--level" => options.level = Some(value("--level")?.parse()?),
            "-c" | "--compress" => options.compress = true,
            "--no-extract" => options.no_extract = true,
            "-f" | "--force" => options.force = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option `{other}`").into());
            }
            other => {
                if options.positional.is_some() {
                    return Err(format!("unexpected extra argument `{other}`").into());
                }
                options.positional = Some(other.to_string());
            }
        }

        index += 1;
    }

    Ok(options)
}
