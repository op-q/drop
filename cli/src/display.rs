//! Putting text somebody else chose on this terminal.
//!
//! A filename is chosen by the sender, an error message by the peer or the
//! relay, and none of them is trusted. Printed as-is, a name can carry escape
//! sequences that move the cursor, clear the line, retitle the window, or plant
//! a hyperlink. It can also carry bidirectional controls that make `exe.pdf`
//! read as `fdp.exe`. That matters most exactly where Drop asks a person to
//! decide something about the name they are looking at, and it matters already
//! for the "Receiving" line, which is printed before any decision exists.
//!
//! Everything here replaces rather than removes. A `\u{FFFD}` where a control
//! character was tells the reader the name was odd; silently deleting it would
//! show them a cleaner name than the one that exists.
//!
//! Legitimate names in any script pass through unchanged: only control and
//! formatting characters are touched, never letters, marks or emoji.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// What stands in for anything that should not reach a terminal.
const REPLACEMENT: char = '\u{FFFD}';

/// Columns a name may take before its middle is elided.
///
/// Wide enough for ordinary names, narrow enough that a hostile one cannot push
/// a question onto a line of its own and off the screen.
pub const NAME_COLUMNS: usize = 80;

/// Columns kept from the end of an elided name, so the extension — the part
/// that says what kind of file this is — always survives.
const TAIL_COLUMNS: usize = 24;

/// Columns a message from a peer or a relay may take.
const MESSAGE_COLUMNS: usize = 200;

/// Characters that are not controls but still change how a terminal lays text
/// out: bidirectional overrides, embeddings, isolates and marks; line and
/// paragraph separators; and invisible characters that let two different names
/// look identical.
///
/// Zero-width joiner and non-joiner (U+200D, U+200C) are deliberately absent.
/// Emoji sequences and several scripts need them, and they cannot reorder or
/// hide text.
fn is_layout_control(character: char) -> bool {
    matches!(
        character,
        '\u{061C}'              // arabic letter mark
        | '\u{200B}'            // zero width space
        | '\u{200E}' | '\u{200F}' // left-to-right and right-to-left marks
        | '\u{2028}' | '\u{2029}' // line and paragraph separators
        | '\u{202A}'..='\u{202E}' // embeddings and overrides
        | '\u{2060}'            // word joiner
        | '\u{2066}'..='\u{2069}' // isolates
        | '\u{FEFF}'            // byte order mark
    )
}

/// Makes untrusted text safe to print, without shortening it.
///
/// Control characters (C0, DEL, C1, which includes the bytes that start every
/// escape sequence) and layout controls become `\u{FFFD}`. Runs of whitespace
/// collapse to one space, so padding cannot push the end of a name out of
/// sight.
pub fn for_terminal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_whitespace = false;

    for character in text.chars() {
        if character.is_control() || is_layout_control(character) {
            out.push(REPLACEMENT);
            in_whitespace = false;
        } else if character.is_whitespace() {
            if !in_whitespace {
                out.push(' ');
            }
            in_whitespace = true;
        } else {
            out.push(character);
            in_whitespace = false;
        }
    }

    out
}

/// A name a peer chose, ready to print on one line.
///
/// Sanitised by [`for_terminal`], then elided in the middle if it is wider than
/// [`NAME_COLUMNS`]. The middle rather than the end, because the end holds the
/// extension and the extension is what says whether this is a document or a
/// program.
pub fn name(text: &str) -> String {
    elide_middle(&for_terminal(text), NAME_COLUMNS)
}

/// A message a peer or a relay sent, ready to print after `error: `.
///
/// Neither is trusted, so this is sanitised too, and capped so a hostile one
/// cannot fill the screen.
pub fn peer_message(text: &str) -> String {
    elide_middle(&for_terminal(text), MESSAGE_COLUMNS)
}

/// Shortens `text` to at most `columns` display columns by replacing its middle
/// with an ellipsis, measuring wide characters as two columns.
fn elide_middle(text: &str, columns: usize) -> String {
    if text.width() <= columns {
        return text.to_string();
    }

    let tail_budget = TAIL_COLUMNS.min(columns / 2);
    // One column for the ellipsis itself.
    let head_budget = columns - tail_budget - 1;

    let mut head = String::new();
    let mut used = 0;
    for character in text.chars() {
        let width = character.width().unwrap_or(0);
        if used + width > head_budget {
            break;
        }
        head.push(character);
        used += width;
    }

    let mut tail: Vec<char> = Vec::new();
    let mut used = 0;
    for character in text.chars().rev() {
        let width = character.width().unwrap_or(0);
        if used + width > tail_budget {
            break;
        }
        tail.push(character);
        used += width;
    }
    tail.reverse();

    format!("{head}\u{2026}{}", tail.into_iter().collect::<String>())
}

/// The extension of a name, as this receiver will store it: after the last
/// dot, lowercased, and only if there is something before the dot.
fn extension(name: &str) -> Option<String> {
    let last = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let (stem, extension) = last.rsplit_once('.')?;

    if stem.is_empty() || extension.is_empty() {
        return None;
    }

    Some(extension.trim().to_ascii_lowercase())
}

/// Extensions a system will run rather than open, on at least one of the three
/// platforms Drop runs on.
const PROGRAM_EXTENSIONS: &[&str] = &[
    "exe", "msi", "bat", "cmd", "com", "scr", "ps1", "vbs", "vbe", "js", "jse", "wsf", "wsh",
    "hta", "cpl", "msc", "lnk", "jar", "app", "dmg", "pkg", "command", "sh", "run", "appimage",
    "desktop", "deb", "rpm",
];

/// Whether a name says it is a program, going by what the receiving system
/// will look at: its real, final extension.
///
/// This is why it takes the name and not the sender's MIME type. The sender
/// chooses both, but the extension of the file that lands is what decides
/// what happens when it is opened.
pub fn is_program(name: &str) -> bool {
    extension(name).is_some_and(|extension| PROGRAM_EXTENSIONS.contains(&extension.as_str()))
}

/// A short description of a file's type, from its final extension.
///
/// Never from the sender's MIME type: see [`is_program`]. The extension shown
/// is sanitised like every other peer-chosen string.
pub fn type_label(name: &str) -> String {
    let Some(extension) = extension(name) else {
        return "File (no extension)".to_string();
    };

    let kind = match extension.as_str() {
        _ if is_program(name) => "Program",
        "pdf" => "PDF document",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "bmp" | "tif" | "tiff" | "svg" => {
            "Image"
        }
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" => "Video",
        "mp3" | "wav" | "flac" | "m4a" | "ogg" | "opus" | "aac" => "Audio",
        "zip" | "tar" | "gz" | "tgz" | "7z" | "rar" | "xz" | "bz2" | "zst" => "Archive",
        "txt" | "md" | "log" | "csv" | "json" | "yaml" | "yml" | "toml" | "xml" => "Text",
        "doc" | "docx" | "odt" | "rtf" | "pages" => "Document",
        "xls" | "xlsx" | "ods" | "numbers" => "Spreadsheet",
        "ppt" | "pptx" | "odp" | "key" => "Presentation",
        "html" | "htm" => "Web page",
        _ => "File",
    };

    format!("{kind} (.{})", for_terminal(&extension))
}

#[cfg(test)]
mod tests {
    use super::{NAME_COLUMNS, for_terminal, is_program, name, peer_message, type_label};
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn a_csi_sequence_cannot_reach_the_terminal() {
        // Clear the line, move up, and print something else.
        let hostile = "report.pdf\x1b[2K\x1b[1Ainvoice.pdf";
        let shown = for_terminal(hostile);

        assert!(!shown.contains('\x1b'), "escape survived: {shown:?}");
        assert!(
            shown.contains('\u{FFFD}'),
            "the tampering should be visible"
        );
        assert!(shown.contains("report.pdf") && shown.contains("invoice.pdf"));
    }

    #[test]
    fn an_osc_hyperlink_and_a_window_title_are_neutralised() {
        let hyperlink = "\x1b]8;;https://example.invalid\x07click.txt\x1b]8;;\x07";
        let title = "\x1b]0;owned\x07name.txt";

        for hostile in [hyperlink, title] {
            let shown = for_terminal(hostile);
            assert!(
                !shown.contains('\x1b') && !shown.contains('\x07'),
                "{shown:?}"
            );
        }
    }

    #[test]
    fn c1_controls_are_replaced_too() {
        // U+009B is a single-character CSI on terminals that honour C1.
        let shown = for_terminal("a\u{009B}2Kb");
        assert_eq!(shown, "a\u{FFFD}2Kb");
    }

    #[test]
    fn newlines_and_tabs_cannot_break_a_line() {
        let shown = for_terminal("first\nsecond\rthird\tfourth");
        assert!(!shown.contains(['\n', '\r', '\t']), "{shown:?}");
    }

    #[test]
    fn every_bidirectional_control_is_replaced() {
        let controls = [
            '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}',
            '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
        ];

        for control in controls {
            let shown = for_terminal(&format!("invoice{control}fdp.exe"));
            assert!(
                !shown.contains(control),
                "U+{:04X} survived: {shown:?}",
                control as u32
            );
        }
    }

    #[test]
    fn the_classic_right_to_left_override_shows_its_real_extension() {
        // Rendered naively this reads "invoice_exe.pdf".
        let shown = name("invoice_\u{202E}fdp.exe");
        assert!(shown.ends_with("fdp.exe"), "{shown:?}");
    }

    #[test]
    fn whitespace_padding_collapses() {
        let shown = for_terminal("report.pdf                                        .exe");
        assert_eq!(shown, "report.pdf .exe");
    }

    #[test]
    fn a_long_name_keeps_its_extension() {
        let long = format!("{}.exe", "a".repeat(300));
        let shown = name(&long);

        assert!(shown.width() <= NAME_COLUMNS, "{} columns", shown.width());
        assert!(shown.ends_with(".exe"), "{shown:?}");
        assert!(shown.contains('\u{2026}'));
    }

    #[test]
    fn wide_characters_count_as_two_columns() {
        let long = format!("{}.txt", "東".repeat(100));
        let shown = name(&long);

        assert!(shown.width() <= NAME_COLUMNS, "{} columns", shown.width());
        assert!(shown.ends_with(".txt"));
    }

    #[test]
    fn honest_names_in_any_script_are_untouched() {
        for honest in [
            "Łódź 東京 🎉.txt",
            "résumé-final (2).pdf",
            "مرحبا.docx",
            "👩‍👩‍👧 family.jpg", // zero-width joiners inside the emoji
            "10:30 standup.md",
        ] {
            assert_eq!(name(honest), honest);
        }
    }

    #[test]
    fn a_short_name_is_not_elided() {
        assert_eq!(name("report.pdf"), "report.pdf");
    }

    #[test]
    fn a_peer_message_is_sanitised_and_capped() {
        let hostile = format!("\x1b[2J{}", "x".repeat(10_000));
        let shown = peer_message(&hostile);

        assert!(!shown.contains('\x1b'));
        assert!(shown.width() <= 200);
    }

    #[test]
    fn a_type_comes_from_the_real_extension() {
        assert_eq!(type_label("report.pdf"), "PDF document (.pdf)");
        assert_eq!(type_label("Holiday.JPG"), "Image (.jpg)");
        assert_eq!(type_label("notes"), "File (no extension)");
        assert_eq!(type_label(".bashrc"), "File (no extension)");
        assert_eq!(type_label("archive.tar.gz"), "Archive (.gz)");
    }

    #[test]
    fn padding_cannot_hide_a_programs_extension() {
        let disguised = "invoice.pdf                                        .exe";
        assert!(is_program(disguised));
        assert_eq!(type_label(disguised), "Program (.exe)");
    }

    #[test]
    fn programs_on_every_platform_are_recognised() {
        for name in [
            "setup.exe",
            "run.BAT",
            "tool.ps1",
            "App.app",
            "installer.dmg",
            "go.sh",
        ] {
            assert!(is_program(name), "{name}");
        }
        for name in ["report.pdf", "photo.jpg", "exe", "notes.txt"] {
            assert!(!is_program(name), "{name}");
        }
    }
}
