//! `drop` — terminal client for the Drop ephemeral file-transfer relay.

use std::{path::PathBuf, process::ExitCode};

use drop_cli::{client, direct, recv, send, ui};

const USAGE: &str = "\
drop — send a file or folder between two terminals

USAGE
    drop send <PATH> [OPTIONS]
    drop recv <CODE> [OPTIONS]

COMMANDS
    send <PATH>     Share a file or folder and print a one-time code.
                    A folder is streamed as a tar archive.
    recv <CODE>     Receive using the code shown by the sender.
    help            Show this help. Also -h, --help.
    version         Show the version. Also -V, --version.

    Typed on their own in a terminal — `drop`, `drop send`, `drop recv` —
    these open an interface that asks for what they need. Given anything at
    all, or run where the output is not a terminal, they behave as below and
    the options apply.

    Everything below is an option to send or recv and belongs after one of
    them: `drop send notes.pdf --compress`, never `drop --compress`.

OPTIONS (send and recv)
    -s, --server <URL>   Relay to forward through [env: DROP_SERVER]
                         No default: there is no hosted Drop relay. Name one
                         you run, or leave it unset and stay on the direct
                         path, which needs no Drop server at all.
    -t, --transport <T>  p2p, relay, or auto [default: auto]
                         p2p connects the two terminals directly and involves
                         no Drop server at all. relay forwards through the one
                         --server names, and fails without it. auto tries p2p
                         and falls back only if a relay is configured; with
                         none it is p2p, and says so rather than falling back
                         to nowhere.

                         A browser on the other end can only meet you at a
                         relay, because it cannot speak QUIC to a peer. That
                         transfer needs --server naming a relay you run.
        --status         Print one machine-readable line naming the carrier
                         that moved the bytes [env: DROP_STATUS]

                             drop-status: path=p2p fallback=none

                         For scripts and test harnesses. The ordinary output
                         above it says the same thing in words, and that is
                         what it is there for.

OPTIONS (send)
    -c, --compress       Compress before sending. Useful for source trees and
                         documents; skip it for media that is already
                         compressed.
        --level <N>      Compression level, 1-9 [default: 6]

OPTIONS (recv)
    -o, --out <DIR>      Where to write [default: current directory]
        --no-extract     Write the archive as a file instead of unpacking it
    -f, --force          Overwrite an existing file

NOTES
    Both peers must be online at the same time: Drop never stores the file.
    A code is single use and expires after five idle minutes.

    Every transfer is encrypted end to end with a key derived from the code,
    on either carrier. What the carrier changes is who moves the bytes, not
    who can read them.

    The direct path involves no Drop server, which is not the same as no
    infrastructure. It finds the other terminal through the public DHT and a
    relay operated by n0, and when two peers cannot hole-punch, that relay
    carries the encrypted connection. DROP_RENDEZVOUS_RELAY and
    DROP_RENDEZVOUS_BOOTSTRAP point both at infrastructure you run instead.
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
        // Bare `drop`. On a terminal it asks which of the two commands the
        // person wants; anywhere else it stays exactly as loud as it was,
        // because a script that ran `drop` with no arguments made a mistake.
        if ui::Invocation::from_process(false, false, status_requested()).surface()
            == ui::Surface::Interface
        {
            return interactive(None);
        }

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

            if options.surface() == ui::Surface::Interface {
                return interactive(Some(ui::app::Choice::Send));
            }

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
                send::SendOptions::printing(
                    options.origin(),
                    compress,
                    options.path()?,
                    options.status(),
                    options.rendezvous()?,
                ),
            ))
        }
        "recv" | "receive" | "get" => {
            let options = parse(&arguments[1..])?;

            if options.surface() == ui::Surface::Interface {
                return interactive(Some(ui::app::Choice::Receive));
            }

            let code = options
                .positional
                .clone()
                .ok_or("recv needs the sender's code: drop recv <CODE>")?;

            runtime()?.block_on(recv::run(
                &code,
                recv::ReceiveOptions {
                    origin: options.origin(),
                    path: options.path()?,
                    status: options.status(),
                    rendezvous: options.rendezvous()?,
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

/// `--status` cannot have been parsed yet when `drop` is typed bare, so only
/// the environment can be asked.
fn status_requested() -> bool {
    std::env::var_os("DROP_STATUS").is_some()
}

/// Runs whatever the interface collected.
///
/// `chosen` is the command already named on the command line, or `None` for
/// bare `drop`, which asks first.
///
/// The interface has closed by the time a transfer starts: it collects the
/// plan, gives the terminal back, and the transfer then prints exactly what it
/// prints for a flag-driven run. Putting progress on the interface is phase 3
/// of the plan. Quitting is not an error — the person chose to leave, so this
/// returns success and says nothing.
fn interactive(
    chosen: Option<ui::app::Choice>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Ok(plan) = ui::app::run(chosen)? else {
        return Ok(());
    };

    let rendezvous = direct::Rendezvous::from_env()?;

    match plan {
        ui::app::Plan::Send(plan) => runtime()?.block_on(send::run(
            &plan.path,
            send::SendOptions::printing(
                plan.server,
                plan.compress.then_some(6),
                plan.carrier,
                false,
                rendezvous,
            ),
        )),

        ui::app::Plan::Receive(plan) => runtime()?.block_on(recv::run(
            &plan.code,
            recv::ReceiveOptions {
                origin: plan.server,
                path: plan.carrier,
                status: false,
                rendezvous,
                out_dir: plan.out_dir,
                extract: true,
                force: plan.force,
            },
        )),
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
    status: bool,
    /// Whether any flag at all was given. Flags are the program-facing
    /// surface, so using one is how an invocation says which audience it is.
    /// See [`drop_cli::ui::Invocation`].
    flagged: bool,
}

impl Options {
    /// The relay to use, if anybody named one.
    ///
    /// `None` is the ordinary answer now. There is no hosted relay and no
    /// compiled-in default, so an unconfigured `drop` takes the direct path
    /// rather than opening a connection to a host that no longer exists.
    ///
    /// An empty value means unset, the same way the rendezvous variables treat
    /// one: `DROP_SERVER=` in a script is somebody clearing it, and reading
    /// that as an origin would produce `https://` and a baffling failure.
    fn origin(&self) -> Option<String> {
        self.server
            .clone()
            .or_else(|| std::env::var("DROP_SERVER").ok())
            .filter(|configured| !configured.trim().is_empty())
            .map(|configured| client::normalize_origin(&configured))
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

    /// Which rendezvous infrastructure to use, from the environment only.
    ///
    /// No flag, unlike `--server`. That one is per transfer — a user may
    /// reasonably send one file through a different relay — while rendezvous
    /// infrastructure is a property of the network the machine is on, set once
    /// in a profile or a unit file and never changed between two transfers. A
    /// flag for it would charge every reader of `--help` for something almost
    /// nobody sets and nobody sets twice.
    fn rendezvous(&self) -> Result<direct::Rendezvous, Box<dyn std::error::Error + Send + Sync>> {
        direct::Rendezvous::from_env().map_err(Into::into)
    }

    /// Whether to add the machine-readable line.
    ///
    /// The environment can only turn this on, never off, so a harness that
    /// exports `DROP_STATUS` once gets the line from every `drop` it spawns
    /// without having to thread a flag through each invocation. Any value
    /// counts, including an empty one: this is a switch, and inventing a
    /// vocabulary of truthy strings for it would be a second thing to get
    /// wrong.
    fn status(&self) -> bool {
        self.status || std::env::var_os("DROP_STATUS").is_some()
    }

    /// Whether this invocation gets the interface or the command.
    fn surface(&self) -> ui::Surface {
        ui::Invocation::from_process(self.positional.is_some(), self.flagged, self.status())
            .surface()
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

        if argument.starts_with('-') {
            options.flagged = true;
        }

        match argument {
            "-s" | "--server" => options.server = Some(value("--server")?),
            "-t" | "--transport" => options.transport = Some(value("--transport")?),
            "-o" | "--out" => options.out = Some(value("--out")?),
            "--level" => options.level = Some(value("--level")?.parse()?),
            "-c" | "--compress" => options.compress = true,
            "--no-extract" => options.no_extract = true,
            "-f" | "--force" => options.force = true,
            "--status" => options.status = true,
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
