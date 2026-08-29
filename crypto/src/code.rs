//! Transfer codes: a public nameplate that routes, and secret words that
//! authenticate.
//!
//! ```text
//! 7F2A91-crossover-clockwork-ridge
//! ^^^^^^ nameplate — the relay sees this and routes on it
//!        ^^^^^^^^^^^^^^^^^^^^^^^^^ words — never transmitted, PAKE password
//! ```
//!
//! **The split is not cosmetic.** SPAKE2 protects a transfer only while the
//! attacker does not know the password, and the relay is an attacker in our
//! threat model. A relay handed the password can run the exchange against both
//! peers at once and sit in the middle reading and rewriting everything. So
//! the half the relay needs for routing and the half that authenticates have
//! to be different bytes, and only the routing half ever leaves the client.
//!
//! The same reasoning applies to the peer-to-peer transport: a DHT record
//! published under a key derived from the secret would let anyone grind the
//! 33-bit word space offline and recover it. The record is keyed on the
//! nameplate, which is public and carries nothing.
//!
//! The words are only 33 bits, which would be indefensible if an attacker
//! could guess offline. They cannot: a wrong guess produces a wrong key, fails
//! the first sealed frame, and burns the session. One guess, online, per code.
//! See `docs/decisions.md` entry 7.

use std::fmt;

use crate::wordlist::WORDS;

/// Secret words per code. Three gives 33 bits.
///
/// That is enough only because of the PAKE. An attacker gets one online guess
/// before the session is consumed, so the useful comparison is not against an
/// offline cracking rate but against a single try — and a single try against
/// 2^33 is hopeless. Adding words past this point costs speakability and buys
/// almost nothing.
pub const SECRET_WORDS: usize = 3;

/// Bits each word contributes. 2048 words, so exactly 11.
const BITS_PER_WORD: u32 = 11;

/// Bytes of nameplate a self-allocated code draws, rendered as two hex
/// characters each. Three gives the same six characters the relay issues.
const NAMEPLATE_BYTES: usize = 3;

/// A validated transfer code: a routing nameplate plus the secret words.
#[derive(Clone, PartialEq, Eq)]
pub struct TransferCode {
    /// Uppercased. The relay allocates this and sees it.
    nameplate: String,
    /// Lowercased, dash-joined. Never leaves the client.
    words: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CodeError {
    WrongWordCount { found: usize },
    UnknownWord { word: String },
    MissingNameplate,
    MalformedNameplate { nameplate: String },
}

impl fmt::Display for CodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongWordCount { found } => write!(
                formatter,
                "a transfer code ends with {SECRET_WORDS} words separated by dashes; \
                 this one has {found}"
            ),
            Self::UnknownWord { word } => write!(
                formatter,
                "'{word}' is not a word used in transfer codes — check for a typo"
            ),
            Self::MissingNameplate => write!(
                formatter,
                "a transfer code starts with a short code from the sender, \
                 then {SECRET_WORDS} words"
            ),
            Self::MalformedNameplate { nameplate } => write!(
                formatter,
                "'{nameplate}' is not a valid session code — it should be letters and digits"
            ),
        }
    }
}

impl std::error::Error for CodeError {}

impl TransferCode {
    /// Draws fresh secret words for a nameplate the relay just allocated.
    ///
    /// Indices are masked to 11 bits rather than reduced modulo 2048, so the
    /// distribution stays uniform: 2^11 divides 2^16 exactly, and a modulo
    /// would not have been uniform for a list of any other size.
    pub fn generate_for(nameplate: &str) -> Result<Self, CodeError> {
        let mut entropy = [0u8; SECRET_WORDS * 2];
        rand::fill(&mut entropy);

        let mask = (1u16 << BITS_PER_WORD) - 1;
        let words: Vec<&str> = entropy
            .chunks_exact(2)
            .map(|pair| {
                let raw = u16::from_be_bytes([pair[0], pair[1]]);
                WORDS[usize::from(raw & mask)]
            })
            .collect();

        Ok(Self {
            nameplate: normalise_nameplate(nameplate)?,
            words: words.join("-"),
        })
    }

    /// Draws a whole code, nameplate included, for a send with no server to
    /// allocate one.
    ///
    /// The relay path calls [`Self::generate_for`] because the relay hands out
    /// the nameplate and needs it to match a session it created. A direct
    /// transfer has nobody to ask, so it draws its own — six hex characters,
    /// the same shape the relay issues, so one code format serves both paths
    /// and a person cannot tell from a code which way it will travel.
    ///
    /// **The nameplate is not a secret and this does not try to make it one.**
    /// It is 24 bits, it is published to a public DHT, and it is meant to be
    /// enumerable — see `rendezvous.rs`. What keeps a transfer safe is the
    /// words, which are drawn separately and never leave this process.
    ///
    /// Collision is possible and is the caller's problem, not this function's:
    /// two live transfers that draw the same nameplate meet at the same DHT
    /// record. A caller that publishes should resolve first, which costs one
    /// round trip and also proves the DHT is reachable before a code is shown
    /// to anybody.
    pub fn generate() -> Result<Self, CodeError> {
        let mut entropy = [0u8; NAMEPLATE_BYTES];
        rand::fill(&mut entropy);

        let nameplate: String = entropy.iter().map(|byte| format!("{byte:02X}")).collect();

        Self::generate_for(&nameplate)
    }

    /// Accepts a code as a person is likely to have retyped it.
    ///
    /// Case is folded and dashes, spaces, and underscores are all accepted as
    /// separators, because a code that was read aloud gets written down in
    /// whichever of those the listener prefers. Rejecting a correct code over
    /// punctuation would be a poor trade when a rejected attempt costs the
    /// whole session.
    pub fn parse(input: &str) -> Result<Self, CodeError> {
        let mut parts: Vec<&str> = input
            .split(|character: char| {
                character == '-' || character == '_' || character.is_whitespace()
            })
            .filter(|part| !part.is_empty())
            .collect();

        if parts.is_empty() {
            return Err(CodeError::MissingNameplate);
        }

        let nameplate = normalise_nameplate(parts.remove(0))?;

        if parts.len() != SECRET_WORDS {
            return Err(CodeError::WrongWordCount { found: parts.len() });
        }

        let words: Vec<String> = parts
            .iter()
            .map(|part| part.trim().to_ascii_lowercase())
            .collect();

        for word in &words {
            if !WORDS.contains(&word.as_str()) {
                return Err(CodeError::UnknownWord { word: word.clone() });
            }
        }

        Ok(Self {
            nameplate,
            words: words.join("-"),
        })
    }

    /// The routing half. This is the only part that may be sent to a relay or
    /// published to a DHT.
    pub fn nameplate(&self) -> &str {
        &self.nameplate
    }

    /// The SPAKE2 password.
    ///
    /// **The words only.** Including the nameplate would add no secrecy — the
    /// relay already has it — and would invite the mistake of treating the
    /// whole code as safe to transmit.
    pub fn as_password(&self) -> &[u8] {
        self.words.as_bytes()
    }

    /// The full code, as shown to a person and typed on the other end.
    pub fn to_shareable(&self) -> String {
        format!("{}-{}", self.nameplate, self.words)
    }
}

/// Nameplates come from the relay, which allocates uppercase alphanumerics.
///
/// Folding case here is what fixes the long-standing papercut where
/// `drop recv 4607f9` was refused while `4607F9` worked.
fn normalise_nameplate(nameplate: &str) -> Result<String, CodeError> {
    let trimmed = nameplate.trim();

    if trimmed.is_empty() {
        return Err(CodeError::MissingNameplate);
    }

    if !trimmed
        .chars()
        .all(|character| character.is_ascii_alphanumeric())
    {
        return Err(CodeError::MalformedNameplate {
            nameplate: trimmed.to_string(),
        });
    }

    Ok(trimmed.to_ascii_uppercase())
}

impl fmt::Display for TransferCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_shareable())
    }
}

/// Codes are secrets. Printing one into a log or an error by accident is the
/// exact failure this type exists to make hard, so `Debug` redacts.
impl fmt::Debug for TransferCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TransferCode(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A self-allocated code has to be indistinguishable from a relay-issued
    /// one, or the code itself would tell a stranger which path a transfer is
    /// taking before they even try it.
    #[test]
    fn a_self_drawn_code_has_the_shape_the_relay_issues() {
        let drawn = TransferCode::generate().expect("a code");
        let issued = TransferCode::generate_for("7F2A91").expect("a code");

        assert_eq!(drawn.nameplate().len(), issued.nameplate().len());
        assert!(
            drawn.nameplate().chars().all(|c| c.is_ascii_hexdigit()),
            "not hex: {}",
            drawn.nameplate()
        );
        assert!(
            drawn.nameplate().chars().all(|c| !c.is_lowercase()),
            "nameplates are printed uppercase: {}",
            drawn.nameplate()
        );
        assert_eq!(
            drawn.to_shareable().split('-').count(),
            issued.to_shareable().split('-').count()
        );
    }

    /// It must survive the trip through a person retyping it.
    #[test]
    fn a_self_drawn_code_parses_back() {
        let drawn = TransferCode::generate().expect("a code");
        let reparsed = TransferCode::parse(&drawn.to_shareable()).expect("parses");

        assert_eq!(reparsed.nameplate(), drawn.nameplate());
        assert_eq!(reparsed.to_shareable(), drawn.to_shareable());
    }

    /// Two codes drawn in a row must not collide, which would mean the
    /// nameplate carried no entropy at all.
    #[test]
    fn two_drawn_codes_differ() {
        let first = TransferCode::generate().expect("a code");
        let second = TransferCode::generate().expect("a code");

        assert_ne!(first.to_shareable(), second.to_shareable());
    }

    const NAMEPLATE: &str = "7F2A91";

    #[test]
    fn generated_codes_round_trip_through_parse() {
        for _ in 0..64 {
            let code = TransferCode::generate_for(NAMEPLATE).unwrap();
            let parsed =
                TransferCode::parse(&code.to_shareable()).expect("generated code should parse");
            assert_eq!(parsed, code);
        }
    }

    #[test]
    fn generated_codes_have_the_declared_word_count() {
        let code = TransferCode::generate_for(NAMEPLATE).unwrap();
        assert_eq!(code.to_shareable().split('-').count(), SECRET_WORDS + 1);
    }

    /// The property the whole split exists for. If the password ever contains
    /// the nameplate, a relay that is handed the nameplate is handed a piece
    /// of the password too.
    #[test]
    fn the_password_is_the_words_and_never_the_nameplate() {
        let code = TransferCode::parse("7F2A91-abandon-ability-able").unwrap();

        assert_eq!(code.as_password(), b"abandon-ability-able");
        assert!(
            !code
                .as_password()
                .windows(6)
                .any(|window| window == b"7F2A91")
        );
        assert_eq!(code.nameplate(), "7F2A91");
    }

    /// Two transfers that share a nameplate but not the words must not agree
    /// on a password, or a relay could replay one session's handshake into
    /// another.
    #[test]
    fn the_same_nameplate_with_different_words_gives_different_passwords() {
        let first = TransferCode::parse("7F2A91-abandon-ability-able").unwrap();
        let second = TransferCode::parse("7F2A91-abandon-ability-about").unwrap();

        assert_ne!(first.as_password(), second.as_password());
    }

    #[test]
    fn parse_folds_case_and_accepts_any_separator() {
        let canonical = TransferCode::parse("7F2A91-abandon-ability-able").unwrap();

        for variant in [
            "7F2A91-ABANDON-ABILITY-ABLE",
            "7f2a91-Abandon ability Able",
            "7F2A91_abandon_ability_able",
            "  7F2A91 - abandon - ability - able  ",
        ] {
            assert_eq!(
                TransferCode::parse(variant).unwrap(),
                canonical,
                "variant {variant:?} should normalise to the canonical code"
            );
        }
    }

    /// The papercut recorded in the implementation checklist: the relay stores
    /// nameplates uppercase, and a lowercase retype used to be refused.
    #[test]
    fn a_lowercase_nameplate_is_accepted_and_normalised() {
        let code = TransferCode::parse("4607f9-abandon-ability-able").unwrap();
        assert_eq!(code.nameplate(), "4607F9");
    }

    #[test]
    fn parse_rejects_a_wrong_word_count() {
        assert_eq!(
            TransferCode::parse("7F2A91-abandon-ability"),
            Err(CodeError::WrongWordCount { found: 2 })
        );
        assert_eq!(
            TransferCode::parse("7F2A91-abandon-ability-able-about"),
            Err(CodeError::WrongWordCount { found: 4 })
        );
    }

    #[test]
    fn parse_rejects_a_word_outside_the_list() {
        assert_eq!(
            TransferCode::parse("7F2A91-abandon-ability-frobnicate"),
            Err(CodeError::UnknownWord {
                word: "frobnicate".to_string()
            })
        );
    }

    #[test]
    fn parse_rejects_an_empty_or_malformed_nameplate() {
        assert_eq!(TransferCode::parse("   "), Err(CodeError::MissingNameplate));
        assert_eq!(
            TransferCode::parse("7F:2A-abandon-ability-able"),
            Err(CodeError::MalformedNameplate {
                nameplate: "7F:2A".to_string()
            })
        );
    }

    #[test]
    fn debug_does_not_leak_the_words() {
        let code = TransferCode::parse("7F2A91-abandon-ability-able").unwrap();
        let rendered = format!("{code:?}");

        assert!(!rendered.contains("abandon"));
        assert!(!rendered.contains("ability"));
    }

    /// The wordlist module claims this, and prefix completion on code entry
    /// would depend on it. Hold the list to it.
    #[test]
    fn wordlist_prefixes_are_unique() {
        let mut prefixes: Vec<&str> = WORDS
            .iter()
            .map(|word| &word[..4.min(word.len())])
            .collect();
        prefixes.sort_unstable();
        let before = prefixes.len();
        prefixes.dedup();
        assert_eq!(before, prefixes.len(), "four-letter prefixes collide");
    }

    #[test]
    fn wordlist_is_exactly_the_size_the_bit_maths_assumes() {
        assert_eq!(WORDS.len(), 1usize << BITS_PER_WORD);
    }
}
