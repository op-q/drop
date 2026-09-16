//! Terminal progress reporting.

use std::{
    io::{IsTerminal, Write},
    time::{Duration, Instant},
};

const REFRESH: Duration = Duration::from_millis(120);

pub struct Progress {
    label: String,
    total: u64,
    started: Instant,
    last_drawn: Instant,
    interactive: bool,
    /// Whether the terminal understands escape sequences. See [`escapes_work`].
    escapes: bool,
    /// How wide the last line drawn was, so a line drawn without escapes can
    /// cover it.
    last_width: usize,
    finished: bool,
}

/// Whether this terminal interprets `\x1b[2K`.
///
/// Every Unix terminal and Windows Terminal do. The older console that
/// `cmd.exe` and PowerShell 5 still open on many machines does only once a
/// program turns on its VT processing, and until then prints `←[2K` before
/// every update. crossterm turns it on here and says whether that worked.
#[cfg(windows)]
fn escapes_work() -> bool {
    crossterm::ansi_support::supports_ansi()
}

#[cfg(not(windows))]
fn escapes_work() -> bool {
    true
}

impl Progress {
    pub fn new(label: impl Into<String>, total: u64) -> Self {
        let now = Instant::now();

        Self {
            label: label.into(),
            total,
            started: now,
            // Draw the first update immediately rather than after one refresh.
            last_drawn: now - REFRESH,
            interactive: std::io::stderr().is_terminal(),
            escapes: escapes_work(),
            last_width: 0,
            finished: false,
        }
    }

    pub fn update(&mut self, transferred: u64) {
        if !self.interactive || self.last_drawn.elapsed() < REFRESH {
            return;
        }

        self.last_drawn = Instant::now();
        self.draw(transferred);
    }

    pub fn finish(&mut self, transferred: u64) {
        if self.finished {
            return;
        }

        self.finished = true;

        if self.interactive {
            self.draw(transferred);
            eprintln!();
        } else {
            eprintln!(
                "{} {} in {}",
                self.label,
                format_bytes(transferred),
                format_duration(self.started.elapsed())
            );
        }
    }

    fn draw(&mut self, transferred: u64) {
        let elapsed = self.started.elapsed().as_secs_f64();
        let rate = if elapsed > 0.0 {
            transferred as f64 / elapsed
        } else {
            0.0
        };

        let percent = if self.total > 0 {
            (transferred as f64 / self.total as f64 * 100.0).min(100.0)
        } else {
            0.0
        };

        let eta = if rate > 0.0 && self.total > transferred {
            format_duration(Duration::from_secs_f64(
                (self.total - transferred) as f64 / rate,
            ))
        } else {
            "--".to_string()
        };

        let line = format!(
            "{} {:>5.1}%  {} / {}  {}/s  ETA {}",
            self.label,
            percent,
            format_bytes(transferred),
            format_bytes(self.total),
            format_bytes(rate as u64),
            eta
        );

        let mut stderr = std::io::stderr();
        let _ = write!(stderr, "{}", redraw(&line, self.escapes, self.last_width));
        let _ = stderr.flush();
        self.last_width = line.chars().count();
    }
}

/// What to write to replace the previous progress line with `line`.
///
/// With escapes, clear the line and write. Without them, return to the start
/// and pad with spaces over whatever the previous, possibly longer, line left.
fn redraw(line: &str, escapes: bool, previous_width: usize) -> String {
    if escapes {
        format!("\r\x1b[2K{line}")
    } else {
        let width = line.chars().count();
        format!(
            "\r{line}{}",
            " ".repeat(previous_width.saturating_sub(width))
        )
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

    let mut value = bytes as f64;
    let mut unit = 0;

    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();

    if seconds >= 3600 {
        format!("{}h{:02}m", seconds / 3600, (seconds % 3600) / 60)
    } else if seconds >= 60 {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn without_escapes_a_shorter_line_covers_a_longer_one() {
        let drawn = super::redraw("short", false, 12);
        assert_eq!(drawn, "\rshort       ");
        assert!(!drawn.contains('\x1b'));
    }

    #[test]
    fn with_escapes_the_line_is_cleared_first() {
        assert_eq!(super::redraw("line", true, 40), "\r\x1b[2Kline");
    }

    use std::time::Duration;

    use super::{format_bytes, format_duration};

    #[test]
    fn formats_byte_counts_at_a_readable_scale() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
    }

    #[test]
    fn formats_durations_across_scales() {
        assert_eq!(format_duration(Duration::from_secs(9)), "9s");
        assert_eq!(format_duration(Duration::from_secs(95)), "1m35s");
        assert_eq!(format_duration(Duration::from_secs(7325)), "2h02m");
    }
}
