//! Turning a name somebody else chose into a name this disk can store.
//!
//! On Linux and macOS a file name may contain anything except `/` and NUL, so a
//! name arrives and is stored as it was sent. Windows reads meaning into
//! ordinary-looking names, and not all of it is harmless:
//!
//! | Name | What Windows does with it |
//! | --- | --- |
//! | `notes.txt:hidden` | writes an NTFS alternate data stream on `notes.txt` |
//! | `notes.txt::$DATA` | opens the main stream of `notes.txt` itself |
//! | `CON`, `nul.txt`, `COM1` | opens a device, not a file |
//! | `report.`, `report ` | silently drops the trailing dot or space |
//! | `a<b`, `a?b`, a control character | refuses to create it at all |
//!
//! Every one of those can arrive from an honest sender. `10:30 standup.md` is
//! an ordinary name on a Mac. So names are **rewritten, not refused**: each
//! character Windows would interpret becomes `_`, and a reserved device name
//! gets `_` after its stem. A rewrite changes one component's spelling and
//! never introduces a separator, so it cannot change which directory an entry
//! lands in. That is the difference from the rule in [`crate::tar`], which
//! refuses `..` rather than normalising it, because normalising `..` would.
//!
//! The rewriting is written as pure functions compiled on every platform, so
//! the policy is tested everywhere. It is *applied* only where
//! [`Naming::for_this_platform`] says so.

use std::borrow::Cow;

/// How names from a peer are stored on this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Naming {
    /// Stored as sent. Correct wherever only `/` and NUL are special.
    AsSent,
    /// Rewritten so Windows stores a file under the name it was given, rather
    /// than a stream, a device, or a name with its last character dropped.
    Windows,
}

impl Naming {
    /// The naming this build's platform needs.
    pub const fn for_this_platform() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::AsSent
        }
    }

    /// One path component under this naming. Never contains a separator.
    ///
    /// `component` must already be a single normal component: not empty, not
    /// `.` or `..`, and free of `/` and `\`. Callers split and validate first.
    pub fn component<'a>(&self, component: &'a str) -> Cow<'a, str> {
        match self {
            Self::AsSent => Cow::Borrowed(component),
            Self::Windows => windows_component(component),
        }
    }
}

/// Device names Windows opens instead of a file, compared case-insensitively
/// against a name's stem.
///
/// The superscript digits are not a curiosity: Windows treats `COM¹` as a
/// device too.
const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM0", "COM1", "COM2", "COM3", "COM4",
    "COM5", "COM6", "COM7", "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3",
    "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
];

/// Characters Windows either refuses in a name or interprets. `:` is the one
/// that matters most, because it names an alternate data stream.
fn windows_forbids(character: char) -> bool {
    matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*') || (character as u32) < 0x20
}

/// What Windows needs `component` to be, so that the name it stores is the
/// name it was given.
///
/// Borrowed when nothing had to change, so a caller can tell a rewrite from a
/// pass-through without comparing strings.
pub fn windows_component(component: &str) -> Cow<'_, str> {
    let mut rewritten: String = component
        .chars()
        .map(|character| {
            if windows_forbids(character) {
                '_'
            } else {
                character
            }
        })
        .collect();

    // Windows strips trailing dots and spaces when it creates a file, so the
    // name written would not be the name checked. Replace every one of them,
    // not only the last, or `report..` would still lose a dot.
    let kept = rewritten.trim_end_matches(['.', ' ']).len();
    if kept < rewritten.len() {
        let trailing = rewritten.len() - kept;
        rewritten.truncate(kept);
        rewritten.extend(std::iter::repeat_n('_', trailing));
    }

    // A device is named by the stem alone, with any trailing spaces ignored:
    // `nul.txt` and `NUL .txt` are both the null device.
    let stem_end = rewritten.find('.').unwrap_or(rewritten.len());
    let stem = rewritten[..stem_end].trim_end_matches(' ');
    if WINDOWS_RESERVED
        .iter()
        .any(|reserved| stem.to_uppercase() == *reserved)
    {
        rewritten.insert(stem_end, '_');
    }

    if rewritten == component {
        Cow::Borrowed(component)
    } else {
        Cow::Owned(rewritten)
    }
}

/// The name a single received file is saved under, and whether it had to be
/// changed for this platform.
///
/// Only the last component of what the sender named is kept, so a name shaped
/// like a path cannot decide where the file lands. On Windows both `/` and `\`
/// separate, since both do on that disk. A name with nothing usable in it
/// becomes `download.bin`.
pub fn received_file_name(sent: &str, naming: Naming) -> (String, bool) {
    let separators: &[char] = match naming {
        Naming::AsSent => &['/'],
        Naming::Windows => &['/', '\\'],
    };

    let last = sent
        .split(separators)
        .rev()
        .find(|segment| !segment.is_empty() && *segment != ".");

    match last {
        None | Some("..") => ("download.bin".to_string(), false),
        Some(segment) => match naming.component(segment) {
            Cow::Borrowed(same) => (same.to_string(), false),
            Cow::Owned(changed) => (changed, true),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{Naming, received_file_name, windows_component};
    use std::borrow::Cow;

    fn rewritten(name: &str) -> String {
        windows_component(name).into_owned()
    }

    #[test]
    fn an_alternate_data_stream_becomes_an_ordinary_name() {
        assert_eq!(rewritten("notes.txt:hidden"), "notes.txt_hidden");
        assert_eq!(rewritten("notes.txt::$DATA"), "notes.txt__$DATA");
    }

    #[test]
    fn a_drive_prefix_cannot_survive() {
        assert_eq!(rewritten("C:"), "C_");
        assert_eq!(rewritten("C:x"), "C_x");
    }

    #[test]
    fn device_names_get_a_suffix_on_their_stem() {
        assert_eq!(rewritten("CON"), "CON_");
        assert_eq!(rewritten("con"), "con_");
        assert_eq!(rewritten("nul.txt"), "nul_.txt");
        assert_eq!(rewritten("NUL .txt"), "NUL _.txt");
        assert_eq!(rewritten("com1.tar.gz"), "com1_.tar.gz");
        assert_eq!(rewritten("LPT¹"), "LPT¹_");
        assert_eq!(rewritten("conout$"), "conout$_");
    }

    #[test]
    fn a_name_that_only_starts_like_a_device_is_left_alone() {
        for name in [
            "CONTRACT.pdf",
            "console.log",
            "nullable.rs",
            "COM10",
            "auxiliary",
        ] {
            assert!(
                matches!(windows_component(name), Cow::Borrowed(_)),
                "{name} is not a device name"
            );
        }
    }

    #[test]
    fn trailing_dots_and_spaces_are_kept_visible() {
        assert_eq!(rewritten("report."), "report_");
        assert_eq!(rewritten("report "), "report_");
        assert_eq!(rewritten("report.. "), "report___");
        assert_eq!(rewritten("..."), "___");
    }

    #[test]
    fn forbidden_characters_and_controls_become_underscores() {
        assert_eq!(rewritten("a<b>c\"d|e?f*g"), "a_b_c_d_e_f_g");
        assert_eq!(rewritten("tab\there"), "tab_here");
    }

    #[test]
    fn a_rewrite_never_introduces_a_separator_or_a_parent() {
        // Separators are split off by every caller before a component gets
        // here, so none appear in these inputs; see `Naming::component`.
        for hostile in ["C:", "..:", ":..", "a::$DATA", "CON", "...", ". .", "?:"] {
            let result = rewritten(hostile);
            assert!(!result.contains(['/', '\\', ':']), "{hostile} -> {result}");
            assert_ne!(result, "..", "{hostile}");
            assert_ne!(result, ".", "{hostile}");
        }
    }

    #[test]
    fn honest_names_pass_through_unchanged() {
        for name in [
            "report.pdf",
            "Łódź 東京 🎉.txt",
            ".bashrc",
            "archive.tar.gz",
            "a b c",
        ] {
            assert!(
                matches!(windows_component(name), Cow::Borrowed(_)),
                "{name} should not be rewritten"
            );
        }
    }

    #[test]
    fn a_colon_from_a_mac_is_rewritten_but_kept_readable() {
        assert_eq!(rewritten("10:30 standup.md"), "10_30 standup.md");
    }

    #[test]
    fn a_received_file_keeps_only_its_last_component() {
        assert_eq!(
            received_file_name("docs/report.pdf", Naming::AsSent),
            ("report.pdf".to_string(), false)
        );
        assert_eq!(
            received_file_name("folder/", Naming::AsSent),
            ("folder".to_string(), false)
        );
        assert_eq!(
            received_file_name("folder/.", Naming::AsSent),
            ("folder".to_string(), false)
        );
    }

    #[test]
    fn a_received_file_with_no_usable_name_gets_a_neutral_one() {
        for sent in ["", "/", ".", "..", "a/..", "//"] {
            assert_eq!(
                received_file_name(sent, Naming::AsSent).0,
                "download.bin",
                "{sent:?}"
            );
        }
    }

    #[test]
    fn a_backslash_separates_only_where_it_does_on_disk() {
        assert_eq!(
            received_file_name("C:\\Users\\x\\report.pdf", Naming::Windows),
            ("report.pdf".to_string(), false)
        );
        assert_eq!(
            received_file_name("a\\b.pdf", Naming::AsSent),
            ("a\\b.pdf".to_string(), false)
        );
        assert_eq!(
            received_file_name("..\\..\\evil.exe", Naming::Windows),
            ("evil.exe".to_string(), false)
        );
    }

    #[test]
    fn a_received_file_is_rewritten_for_windows_and_says_so() {
        assert_eq!(
            received_file_name("report:v2.pdf", Naming::Windows),
            ("report_v2.pdf".to_string(), true)
        );
        assert_eq!(
            received_file_name("report:v2.pdf", Naming::AsSent),
            ("report:v2.pdf".to_string(), false)
        );
    }
}
