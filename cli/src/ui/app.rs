//! The screens a person sees, and nothing else.
//!
//! # What this module is allowed to do
//!
//! Collect choices and hand them back. It runs *before* a transfer, decides
//! nothing about how one works, and holds no opinion about paths, codes or
//! carriers beyond presenting them. Every screen here returns a plan; the
//! caller runs it through the same [`crate::send::run`] and
//! [`crate::recv::run`] that a flag-driven invocation uses.
//!
//! That boundary is why the interface can be added without touching the
//! transfer core: the archive rules in [`crate::untar`], the one-guess policy
//! in [`crate::send`] and the carrier choice in [`crate::direct`] are unmoved
//! and unaware of it.
//!
//! # Styling
//!
//! Deliberately almost none. Two colours, no borders, no boxes: a header, a
//! body, and a line of key hints. The interface should read as part of the
//! terminal rather than as an application that has taken it over.

use std::{
    fs,
    io::{self, Stderr},
    path::{Path as FsPath, PathBuf},
};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{
    direct::Path as Carrier,
    progress::format_bytes,
    ui::terminal::{self, Guard},
};

/// What a bare `drop` asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Send,
    Receive,
}

/// Everything a send needs, as chosen on screen.
#[derive(Clone, Debug)]
pub struct SendPlan {
    pub path: PathBuf,
    pub compress: bool,
    pub carrier: Carrier,
    pub server: Option<String>,
}

/// Everything a receive needs, as chosen on screen.
#[derive(Clone, Debug)]
pub struct ReceivePlan {
    pub code: String,
    pub out_dir: PathBuf,
    pub force: bool,
    pub carrier: Carrier,
    pub server: Option<String>,
}

/// Why a screen returned without an answer.
///
/// Two reasons, because they were one reason until somebody selected the wrong
/// folder and found that the only way back was to start the program again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Leave {
    /// Done with the whole interface. Not an error — the person chose to go,
    /// and the caller exits quietly.
    Quit,
    /// Back one screen.
    Back,
}

type Outcome<T> = Result<Result<T, Leave>, io::Error>;

/// The terminal, for as long as the interface is on screen.
struct Screen {
    terminal: Terminal<CrosstermBackend<Stderr>>,
    _guard: Guard,
}

impl Screen {
    fn open() -> io::Result<Self> {
        // The activation rule in [`crate::ui::Invocation`] has already decided
        // there is a person here, so this should never fire. It is checked
        // anyway because the cost of being wrong is not an error message: raw
        // mode on a pipe succeeds on some platforms and produces a program
        // that cannot be quit.
        if !terminal::is_available() {
            return Err(io::Error::other(
                "the interface needs a terminal on stdin and stderr",
            ));
        }

        terminal::install_panic_hook();

        let guard = Guard::enter()?;
        let terminal = Terminal::new(CrosstermBackend::new(io::stderr()))?;

        Ok(Self {
            terminal,
            _guard: guard,
        })
    }

    /// Draws a header, a body, and a hint line.
    fn draw(&mut self, header: &str, body: Vec<Line<'_>>, hints: &str) -> io::Result<()> {
        self.terminal.draw(|frame| {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(2),
                    Constraint::Min(1),
                    Constraint::Length(1),
                ])
                .split(frame.area());

            let title = Line::from(vec![
                Span::styled("drop", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(format!("  {header}"), Style::default().fg(Color::DarkGray)),
            ]);

            frame.render_widget(Paragraph::new(title), rows[0]);
            frame.render_widget(Paragraph::new(body), rows[1]);
            frame.render_widget(
                Paragraph::new(Line::styled(
                    hints.to_string(),
                    Style::default().fg(Color::DarkGray),
                )),
                rows[2],
            );
        })?;

        Ok(())
    }

    /// The next key press.
    ///
    /// Release and repeat events are dropped: Windows reports both edges of a
    /// key, and counting them moves a selection two rows per press.
    fn key(&mut self) -> io::Result<KeyEvent> {
        loop {
            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                return Ok(key);
            }
        }
    }
}

/// Whether a key means "stop".
///
/// Ctrl-C is included because the interface holds the terminal in raw mode,
/// where the terminal driver no longer turns it into SIGINT: without this, the
/// one key everybody reaches for would do nothing at all.
fn leaving(key: KeyEvent) -> Option<Leave> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(Leave::Quit);
    }

    match key.code {
        KeyCode::Char('q') => Some(Leave::Quit),
        KeyCode::Esc => Some(Leave::Back),
        _ => None,
    }
}

/// A highlighted row, or a plain one.
fn row(selected: bool, text: String) -> Line<'static> {
    if selected {
        Line::styled(
            format!("> {text}"),
            Style::default().add_modifier(Modifier::BOLD),
        )
    } else {
        Line::from(format!("  {text}"))
    }
}

/// Moves a selection without running off either end.
fn step(selected: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }

    let last = len - 1;

    match delta {
        d if d < 0 => selected.saturating_sub(1),
        _ => (selected + 1).min(last),
    }
}

// ---------------------------------------------------------------- the chooser

/// What bare `drop` opens: two choices and nothing else.
fn choose(screen: &mut Screen) -> Outcome<Choice> {
    let mut selected = 0usize;

    let options = [
        (Choice::Send, "send a file or folder"),
        (Choice::Receive, "receive a file"),
    ];

    loop {
        let body = options
            .iter()
            .enumerate()
            .map(|(index, (_, label))| row(index == selected, (*label).to_string()))
            .collect();

        screen.draw("", body, "↑↓ move    enter choose    q quit")?;

        let key = screen.key()?;

        // The first screen: there is nothing behind it, so `esc` leaves too.
        if leaving(key).is_some() {
            return Ok(Err(Leave::Quit));
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => selected = step(selected, options.len(), -1),
            KeyCode::Down | KeyCode::Char('j') => selected = step(selected, options.len(), 1),
            KeyCode::Enter => return Ok(Ok(options[selected].0)),
            _ => {}
        }
    }
}

// ----------------------------------------------------------- the file browser

/// One row in the browser.
struct Entry {
    name: String,
    path: PathBuf,
    is_dir: bool,
    size: Option<u64>,
}

/// What the browser is being used to pick.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Picking {
    /// `send`: a file or a folder, either is a payload.
    Anything,
    /// `recv`: somewhere to write, so files are not offered.
    DirectoryOnly,
}

/// Reads one directory into rows.
///
/// Directory sizes are left blank rather than walked. Computing them eagerly
/// turns opening a folder into a recursive stat of everything beneath it, which
/// on a home directory is a visible pause every time a selection moves. Open
/// question 2 in the plan is whether they appear at all.
fn list(dir: &FsPath, picking: Picking, show_hidden: bool) -> io::Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();

            if !show_hidden && name.starts_with('.') {
                return None;
            }

            // A symlink's own metadata says "symlink"; what matters for
            // navigation is what it points at, so this follows it. A broken
            // link has no target and is skipped rather than shown as a file
            // that cannot be sent.
            let metadata = fs::metadata(entry.path()).ok()?;
            let is_dir = metadata.is_dir();

            if picking == Picking::DirectoryOnly && !is_dir {
                return None;
            }

            Some(Entry {
                name,
                path: entry.path(),
                is_dir,
                size: (!is_dir).then_some(metadata.len()),
            })
        })
        .collect();

    // Directories first, then by name, case-insensitively. The ordering the
    // filesystem hands back is arbitrary and looks broken to a person.
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(entries)
}

/// Navigate a tree and pick something out of it.
///
/// **`enter` picks whatever is highlighted, folder or file.** It descends into
/// nothing, because "go into this" and "I mean this one" cannot both be the
/// key everybody presses first — and the first person to use this pressed
/// `enter` on a folder they meant to send and was taken inside it instead.
/// Opening a folder is `→`, which is the key that already means "further in"
/// on the row above it.
///
/// The first row is the folder currently being looked at, so choosing it needs
/// no special key either: it is just another row to press `enter` on.
fn browse(screen: &mut Screen, start: PathBuf, picking: Picking) -> Outcome<PathBuf> {
    let mut directory = start.canonicalize().unwrap_or(start);
    let mut show_hidden = false;
    let mut selected = 0usize;
    let mut entries = list(&directory, picking, show_hidden).unwrap_or_default();

    let header = match picking {
        Picking::Anything => "send — choose a file or folder",
        Picking::DirectoryOnly => "receive — choose where to save",
    };

    loop {
        // Row 0 is this folder; entries start at 1.
        let rows = entries.len() + 1;

        let mut body = vec![
            Line::styled(
                directory.display().to_string(),
                Style::default().fg(Color::Cyan),
            ),
            Line::from(""),
            row(selected == 0, ".   (this folder)".to_string()),
        ];

        for (index, entry) in entries.iter().enumerate() {
            let detail = match (entry.is_dir, entry.size) {
                (true, _) => "/".to_string(),
                (false, Some(size)) => format!("   {}", format_bytes(size)),
                (false, None) => String::new(),
            };

            body.push(row(
                selected == index + 1,
                format!("{}{detail}", entry.name),
            ));
        }

        screen.draw(
            header,
            body,
            "↑↓ move    enter choose    → open folder    ← up    . hidden    q quit",
        )?;

        let key = screen.key()?;

        if let Some(leave) = leaving(key) {
            return Ok(Err(leave));
        }

        let mut reload = false;

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => selected = step(selected, rows, -1),
            KeyCode::Down | KeyCode::Char('j') => selected = step(selected, rows, 1),

            // The answer, whatever is highlighted.
            KeyCode::Enter => {
                let chosen = match selected.checked_sub(1).and_then(|index| entries.get(index)) {
                    Some(entry) => entry.path.clone(),
                    None => directory.clone(),
                };

                return Ok(Ok(chosen));
            }

            // Navigation, and only navigation.
            KeyCode::Right | KeyCode::Char('l') => {
                if let Some(entry) = selected.checked_sub(1).and_then(|index| entries.get(index))
                    && entry.is_dir
                {
                    directory = entry.path.clone();
                    reload = true;
                }
            }

            KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => {
                if let Some(parent) = directory.parent() {
                    directory = parent.to_path_buf();
                    reload = true;
                }
            }

            KeyCode::Char('.') => {
                show_hidden = !show_hidden;
                reload = true;
            }

            _ => {}
        }

        if reload {
            entries = list(&directory, picking, show_hidden).unwrap_or_default();
            selected = 0;
        }
    }
}

// ------------------------------------------------------------- a text field

/// One line of typed input, refused until it is valid.
///
/// `validate` runs on every `enter` and its message is shown under the field.
/// Checking here rather than after the interface has closed is the whole point
/// of the parameter: a transfer code that is wrong is wrong the moment it is
/// typed, and finding that out two screens later — from a line of terminal
/// output, with the interface already gone — is the same mistake as not
/// checking at all.
fn ask(
    screen: &mut Screen,
    header: &str,
    label: &str,
    initial: &str,
    validate: impl Fn(&str) -> Result<(), String>,
) -> Outcome<String> {
    let mut value = initial.to_string();
    let mut complaint: Option<String> = None;

    loop {
        let mut body = vec![
            Line::styled(label, Style::default().fg(Color::DarkGray)),
            Line::from(""),
            Line::styled(
                format!("  {value}_"),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ];

        if let Some(message) = &complaint {
            body.push(Line::from(""));
            body.push(Line::styled(
                format!("  {message}"),
                Style::default().fg(Color::Yellow),
            ));
        }

        screen.draw(header, body, "enter continue    esc back")?;

        let key = screen.key()?;

        // `q` is a character here, not a command: this is a text field. So
        // `esc` goes back and ctrl-c leaves, and neither can be typed.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(Err(Leave::Quit));
        }

        if key.code == KeyCode::Esc {
            return Ok(Err(Leave::Back));
        }

        match key.code {
            KeyCode::Enter => {
                let trimmed = value.trim().to_string();

                if trimmed.is_empty() {
                    continue;
                }

                match validate(&trimmed) {
                    Ok(()) => return Ok(Ok(trimmed)),
                    Err(message) => complaint = Some(message),
                }
            }

            // Any edit clears the complaint: it described the old value, and
            // leaving it up makes a corrected field look still-broken.
            KeyCode::Backspace => {
                value.pop();
                complaint = None;
            }
            KeyCode::Char(character) => {
                value.push(character);
                complaint = None;
            }

            _ => {}
        }
    }
}

// --------------------------------------------------------- the options screen

/// Which single checkbox the options screen shows, and what it is called.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Send,
    Receive,
}

/// The settings both screens collect.
struct Settings {
    /// `compress` when sending, `overwrite` when receiving. One checkbox
    /// either way, so one field.
    toggle: bool,
    carrier: Carrier,
    server: Option<String>,
}

fn checkbox(on: bool) -> &'static str {
    if on { "[x]" } else { "[ ]" }
}

/// Cycles `auto → p2p → relay → auto`.
fn next_carrier(carrier: Carrier) -> Carrier {
    match carrier {
        Carrier::Auto => Carrier::Direct,
        Carrier::Direct => Carrier::Relay,
        Carrier::Relay => Carrier::Auto,
    }
}

/// The options screen, and the last thing before a transfer starts.
fn options(
    screen: &mut Screen,
    mode: Mode,
    subject: &str,
    mut settings: Settings,
) -> Outcome<Settings> {
    let (header, toggle_label) = match mode {
        Mode::Send => ("send — options", "compress before sending"),
        Mode::Receive => ("receive — options", "overwrite files that already exist"),
    };

    let mut selected = 0usize;
    let rows = 4;

    loop {
        let relay_label = match &settings.server {
            Some(server) => format!("custom relay   {server}"),
            None => "add a custom relay".to_string(),
        };

        let mut body = vec![
            Line::styled(subject.to_string(), Style::default().fg(Color::Cyan)),
            Line::from(""),
            row(
                selected == 0,
                format!("{} {toggle_label}", checkbox(settings.toggle)),
            ),
            row(
                selected == 1,
                format!("    carrier        {}", settings.carrier),
            ),
            row(
                selected == 2,
                format!("{} {relay_label}", checkbox(settings.server.is_some())),
            ),
            Line::from(""),
            row(selected == 3, "start".to_string()),
        ];

        // Entry 16: there is no hosted relay, so asking for one without naming
        // one cannot work. Said here rather than left for the transfer to fail
        // on, because this screen is where it can still be fixed.
        if settings.carrier == Carrier::Relay && settings.server.is_none() {
            body.push(Line::from(""));
            body.push(Line::styled(
                "  the relay carrier needs a relay — add one above",
                Style::default().fg(Color::Yellow),
            ));
        }

        screen.draw(
            header,
            body,
            "↑↓ move    space change    enter select    esc back    q quit",
        )?;

        let key = screen.key()?;

        if let Some(leave) = leaving(key) {
            return Ok(Err(leave));
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => selected = step(selected, rows, -1),
            KeyCode::Down | KeyCode::Char('j') => selected = step(selected, rows, 1),

            KeyCode::Char(' ') | KeyCode::Enter => match selected {
                0 => settings.toggle = !settings.toggle,
                1 => settings.carrier = next_carrier(settings.carrier),

                // Space clears a relay; enter types a new one. Clearing needs
                // to be one key, because the way back from "I added this by
                // mistake" should not be another prompt.
                2 => {
                    if key.code == KeyCode::Char(' ') && settings.server.is_some() {
                        settings.server = None;
                    } else {
                        let current = settings.server.clone().unwrap_or_default();

                        let relay_header = match mode {
                            Mode::Send => "send — custom relay",
                            Mode::Receive => "receive — custom relay",
                        };

                        if let Ok(entered) = ask(
                            screen,
                            relay_header,
                            "The relay to forward through. There is no hosted one; \
                             this is a relay you or somebody you trust runs.",
                            &current,
                            |_| Ok(()),
                        )? {
                            settings.server = Some(crate::client::normalize_origin(&entered));
                        }
                    }
                }

                _ => return Ok(Ok(settings)),
            },

            _ => {}
        }
    }
}

// ------------------------------------------------------------- entry points

/// What the interface collected, whichever command it was opened for.
#[derive(Debug)]
pub enum Plan {
    Send(SendPlan),
    Receive(ReceivePlan),
}

/// Reads `DROP_SERVER` so a relay already exported shows as in effect rather
/// than appearing unset on a screen that says it is unset.
fn configured_server() -> Option<String> {
    std::env::var("DROP_SERVER")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| crate::client::normalize_origin(&value))
}

fn here() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn defaults() -> Settings {
    Settings {
        toggle: false,
        carrier: Carrier::Auto,
        server: configured_server(),
    }
}

/// Opens the interface and returns what the person chose.
///
/// `chosen` is the command they already named — `drop send` or `drop recv` —
/// and `None` is bare `drop`, which asks first.
///
/// One [`Screen`] covers the whole interaction rather than one per step. That
/// is not tidiness: each screen enters and leaves the alternate screen, so a
/// screen per step makes bare `drop` visibly flicker back to the shell between
/// choosing "send" and seeing the file browser.
///
/// # Why this is a loop and not a sequence of calls
///
/// **The next screen is whichever answer is still missing**, and going back is
/// throwing the last answer away. Written as a straight sequence, `esc` on the
/// options screen could only mean "give up", which is what it used to mean and
/// what made choosing the wrong folder unrecoverable. Written this way, back
/// costs one `None` and the settings already toggled survive it.
pub fn run(chosen: Option<Choice>) -> Outcome<Plan> {
    let mut screen = Screen::open()?;

    let mut choice = chosen;
    let mut path: Option<PathBuf> = None;
    let mut code: Option<String> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut settings = defaults();

    loop {
        // Stepping back out of the first screen leaves. When the command was
        // named on the command line there is no chooser behind it, so that is
        // the first screen and the same applies.
        let Some(current) = choice else {
            match choose(&mut screen)? {
                Ok(picked) => choice = Some(picked),
                Err(_) => return Ok(Err(Leave::Quit)),
            }

            continue;
        };

        match current {
            Choice::Send => {
                let Some(chosen_path) = path.clone() else {
                    match browse(&mut screen, here(), Picking::Anything)? {
                        Ok(picked) => path = Some(picked),
                        // Behind the first screen is the chooser, or the
                        // shell if the command was named on the command line.
                        Err(Leave::Back) if chosen.is_some() => {
                            return Ok(Err(Leave::Quit));
                        }
                        Err(Leave::Back) => choice = None,
                        Err(leave) => return Ok(Err(leave)),
                    }

                    continue;
                };

                let subject = chosen_path.display().to_string();

                match options(&mut screen, Mode::Send, &subject, settings)? {
                    Ok(answered) => {
                        return Ok(Ok(Plan::Send(SendPlan {
                            path: chosen_path,
                            compress: answered.toggle,
                            carrier: answered.carrier,
                            server: answered.server,
                        })));
                    }
                    Err(Leave::Back) => {
                        settings = defaults();
                        path = None;
                    }
                    Err(leave) => return Ok(Err(leave)),
                }
            }

            Choice::Receive => {
                let Some(entered) = code.clone() else {
                    match ask(
                        &mut screen,
                        "receive — code",
                        "The code the sender is showing you.",
                        "",
                        |value| {
                            crate::crypto::TransferCode::parse(value)
                                .map(|_| ())
                                .map_err(|error| error.to_string())
                        },
                    )? {
                        Ok(answered) => code = Some(answered),
                        // Behind the first screen is the chooser, or the
                        // shell if the command was named on the command line.
                        Err(Leave::Back) if chosen.is_some() => {
                            return Ok(Err(Leave::Quit));
                        }
                        Err(Leave::Back) => choice = None,
                        Err(leave) => return Ok(Err(leave)),
                    }

                    continue;
                };

                let Some(destination) = out_dir.clone() else {
                    match browse(&mut screen, here(), Picking::DirectoryOnly)? {
                        Ok(picked) => out_dir = Some(picked),
                        Err(Leave::Back) => code = None,
                        Err(leave) => return Ok(Err(leave)),
                    }

                    continue;
                };

                let subject = format!("{entered}  →  {}", destination.display());

                match options(&mut screen, Mode::Receive, &subject, settings)? {
                    Ok(answered) => {
                        return Ok(Ok(Plan::Receive(ReceivePlan {
                            code: entered,
                            out_dir: destination,
                            force: answered.toggle,
                            carrier: answered.carrier,
                            server: answered.server,
                        })));
                    }
                    Err(Leave::Back) => {
                        settings = defaults();
                        out_dir = None;
                    }
                    Err(leave) => return Ok(Err(leave)),
                }
            }
        }
    }
}
