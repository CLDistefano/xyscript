//! Language checking (harper-core) — a thin wrapper over the pure-Rust offline grammar/spell checker.
//! This module knows ONLY about text: it takes a string and returns issues as **byte spans into that
//! string** plus a kind and message. Mapping those onto the editor's rope/cursor offsets is the
//! host's job (see `AppHost`), because only the host has the `Buffer` and its encoding.
//!
//! [`LintEngine`] holds a reusable harper `LintGroup` (building it is non-trivial), so the background
//! lint worker can lint job after job without rebuilding the linter each time.

/// Classify a harper `LintKind` (rendered name) as a SPELLING issue vs a GRAMMAR/other issue.
/// `Spelling` and `Typo` are both spelling-class (a typo is a misspelling); everything else —
/// agreement, capitalization, word-choice, style, repetition, … — is grammar-class.
pub fn is_spelling_kind(kind: &str) -> bool {
    matches!(kind, "Spelling" | "Typo")
}

/// A located issue in editor (**rope**) coordinates — produced by the host after mapping a harper
/// byte span through the active buffer. The GUI draws these as underlines.
#[derive(Debug, Clone)]
pub struct LintMark {
    pub start: usize,
    pub end: usize,
    pub kind: String,
    pub message: String,
}

impl LintMark {
    /// Whether this is a SPELLING issue (vs grammar/other) — drives the `SPELLING`/`GRAMMAR` toggles,
    /// the underline colour, and `SPELLNEXT`/`GRAMMARNEXT`.
    pub fn is_spelling(&self) -> bool {
        is_spelling_kind(&self.kind)
    }
}

/// Which markup the document is, so we pick the parser that skips the right syntax (checking only
/// prose). Mirrors the editor's `RenderKind` (Gfm / Typst) — the host maps one to the other.
/// (Always compiled so the background-worker plumbing needs no feature gate.)
#[derive(Clone, Copy, Debug)]
pub enum Flavor {
    /// CommonMark / GFM — harper's `Markdown` parser.
    Markdown,
    /// Typst — harper-typst's prose-aware `Typst` parser (rides on `typst-syntax`, already a dep).
    Typst,
}

/// One issue as a BYTE span `[start, end)` into the linted string, with its kind and message. The
/// host adds the region's base offset and converts to rope coordinates.
pub type LintSpan = (usize, usize, String, String);

/// Which checker produces the SPELLING underlines. Harper is the built-in (English grammar+spelling,
/// behind the `lint` feature); Hunspell shells out to the `hunspell` CLI for multilingual spelling.
/// Always compiled so the host can store/parse it without the feature. (`SPELLENGINE`.)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SpellEngine {
    #[default]
    Harper,
    Hunspell,
}

impl SpellEngine {
    pub fn parse(s: &str) -> Option<SpellEngine> {
        match s.trim().to_ascii_lowercase().as_str() {
            "harper" => Some(SpellEngine::Harper),
            "hunspell" => Some(SpellEngine::Hunspell),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            SpellEngine::Harper => "harper",
            SpellEngine::Hunspell => "hunspell",
        }
    }
}

/// A word token is a maximal run of alphabetic characters plus internal apostrophes (contractions).
/// Unicode-aware `is_alphabetic` so Cyrillic/Greek/accented scripts tokenize too. Approximates
/// Hunspell's word notion closely enough for span mapping; hyphenated/alphanumeric edges are a
/// known simplification for this spike.
fn is_word_char(c: char) -> bool {
    c.is_alphabetic() || c == '\''
}

/// SPELLING spans for every whole-word occurrence in `text` whose (apostrophe-trimmed) form is in
/// `misspelled`. This is the "translate the `hunspell -l` list into underlines" step — treating the
/// word list as a set-membership oracle and locating the occurrences ourselves, which is correct
/// because spelling is context-free (every occurrence of a flagged word IS misspelled). PURE +
/// unit-tested; needs no `hunspell` binary.
pub fn spans_for_words(text: &str, misspelled: &std::collections::HashSet<String>) -> Vec<LintSpan> {
    if misspelled.is_empty() {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut start: Option<usize> = None;
    // A trailing sentinel flushes the last word without duplicating the emit logic.
    for (i, c) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
        if is_word_char(c) {
            start.get_or_insert(i);
        } else if let Some(s) = start.take() {
            let word = &text[s..i];
            let trimmed = word.trim_matches('\'');
            if !trimmed.is_empty() && misspelled.contains(trimmed) {
                let lead = word.len() - word.trim_start_matches('\'').len();
                let ws = s + lead;
                spans.push((ws, ws + trimmed.len(), "Spelling".to_string(), String::new()));
            }
        }
    }
    spans
}

/// The set of misspelled word-types in `text` per `hunspell -d <dicts> -l` (fed via stdin). Returns
/// EMPTY on any failure — the binary missing, a spawn/IO error, a non-zero exit — so the feature
/// degrades to "no spell underlines" rather than erroring. `bin` is the executable (name on `PATH`
/// or a full path). Always compiled (only `std::process`); the viewport-sized input keeps the
/// single write→read exchange well under the OS pipe buffer, so no writer thread is needed here.
pub fn hunspell_misspelled(text: &str, dicts: &str, bin: &str) -> std::collections::HashSet<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(bin);
    cmd.arg("-d")
        .arg(dicts)
        .arg("-l")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Windows: the dist exe is GUI-subsystem, so a console child (hunspell.exe) would flash a
    // throwaway console window on EVERY check (once per scroll/edit). CREATE_NO_WINDOW suppresses it;
    // stdout is piped regardless, so nothing is lost. (Same flag the shell-out paths use.)
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return std::collections::HashSet::new(),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes()); // dropping stdin at scope end signals EOF
    }
    match child.wait_with_output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        Err(_) => std::collections::HashSet::new(),
    }
}

/// Spell-check `text` with Hunspell and return SPELLING [`LintSpan`]s — the same shape harper's
/// `lint_region` returns, so the host maps/renders them identically.
pub fn hunspell_check(text: &str, dicts: &str, bin: &str) -> Vec<LintSpan> {
    spans_for_words(text, &hunspell_misspelled(text, dicts, bin))
}

/// A fixable issue for `LINTFIX`: byte span `[start, end)`, whether it's a SPELLING issue, and the
/// ordered fix choices as `(label, replacement)` (`replacement` = what the span text becomes).
pub type LintFix = (usize, usize, bool, Vec<(String, String)>);

/// The English dialect harper checks against (`DIALECT`, `$DIALECT`). Always compiled so the host can
/// parse/store it without the feature; mapped to harper's `Dialect` inside [`LintEngine`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Dialect {
    #[default]
    American,
    British,
    Canadian,
    Australian,
    Indian,
}

impl Dialect {
    /// Parse a dialect name (case-insensitive; a couple of common aliases). `None` = unrecognized.
    pub fn parse(s: &str) -> Option<Dialect> {
        match s.trim().to_ascii_lowercase().as_str() {
            "american" | "us" => Some(Dialect::American),
            "british" | "uk" | "gb" => Some(Dialect::British),
            "canadian" | "ca" => Some(Dialect::Canadian),
            "australian" | "au" => Some(Dialect::Australian),
            "indian" | "in" => Some(Dialect::Indian),
            _ => None,
        }
    }
    /// The canonical lowercase name (for `$DIALECT` / SaveCfg).
    pub fn name(self) -> &'static str {
        match self {
            Dialect::American => "american",
            Dialect::British => "british",
            Dialect::Canadian => "canadian",
            Dialect::Australian => "australian",
            Dialect::Indian => "indian",
        }
    }
    #[cfg(feature = "lint")]
    fn to_harper(self) -> harper_core::Dialect {
        match self {
            Dialect::American => harper_core::Dialect::American,
            Dialect::British => harper_core::Dialect::British,
            Dialect::Canadian => harper_core::Dialect::Canadian,
            Dialect::Australian => harper_core::Dialect::Australian,
            Dialect::Indian => harper_core::Dialect::Indian,
        }
    }
}

/// A reusable harper linter (curated dictionary + American dialect). Build once, lint many times.
#[cfg(feature = "lint")]
pub struct LintEngine {
    group: harper_core::linting::LintGroup,
}

#[cfg(feature = "lint")]
impl Default for LintEngine {
    fn default() -> Self {
        Self::new(Dialect::default())
    }
}

#[cfg(feature = "lint")]
impl LintEngine {
    /// Build a linter for `dialect` over the curated dictionary. (User "learned" words are honoured
    /// by FILTERING them out of the results in the host — harper's mutable-dictionary path proved
    /// unreliable, and result-level filtering also drops the "Did you mean…" typo marks.)
    pub fn new(dialect: Dialect) -> Self {
        use harper_core::linting::LintGroup;
        use harper_core::spell::FstDictionary;
        LintEngine {
            group: LintGroup::new_curated(FstDictionary::curated(), dialect.to_harper()),
        }
    }

    /// Lint `text` with the markup-aware parser for `flavor`, returning issues as BYTE spans into
    /// `text` (end-exclusive) with kind + message.
    pub fn lint_region(&mut self, text: &str, flavor: Flavor) -> Vec<LintSpan> {
        use harper_core::linting::Linter;
        use harper_core::parsers::Markdown;
        use harper_core::Document;
        let document = match flavor {
            Flavor::Typst => Document::new_curated(text, &harper_typst::Typst),
            Flavor::Markdown => Document::new_curated(text, &Markdown::default()),
        };
        // harper reports CHAR indices; map to byte offsets in `text` (one extra entry = end-of-text).
        let byte_of_char: Vec<usize> = text
            .char_indices()
            .map(|(b, _)| b)
            .chain(std::iter::once(text.len()))
            .collect();
        let at = |ci: usize| byte_of_char[ci.min(byte_of_char.len().saturating_sub(1))];
        self.group
            .lint(&document)
            .into_iter()
            .filter_map(|l| {
                // Trim whitespace off the span's ends so the underline never covers leading/trailing
                // spaces, tabs, or newlines (harper — especially via the Markdown/Typst parser — can
                // report a span padded with whitespace; a whole-whitespace span is dropped entirely).
                let (bs, be) = (at(l.span.start), at(l.span.end));
                let slice = text.get(bs..be)?;
                let bs = bs + (slice.len() - slice.trim_start().len());
                let be = be - (slice.len() - slice.trim_end().len());
                (bs < be).then(|| (bs, be, format!("{:?}", l.lint_kind), l.message))
            })
            .collect()
    }

    /// Lint `text` and return, for each issue that HAS suggestions, its byte span plus the ordered
    /// fix choices as `(label, replacement)` — where `replacement` is what the span text should BECOME
    /// (accounting for harper's `ReplaceWith` / `InsertAfter` / `Remove`). Used by `LINTFIX`.
    pub fn lint_region_fixes(&mut self, text: &str, flavor: Flavor) -> Vec<LintFix> {
        use harper_core::linting::Linter;
        use harper_core::parsers::Markdown;
        use harper_core::Document;
        let document = match flavor {
            Flavor::Typst => Document::new_curated(text, &harper_typst::Typst),
            Flavor::Markdown => Document::new_curated(text, &Markdown::default()),
        };
        let byte_of_char: Vec<usize> = text
            .char_indices()
            .map(|(b, _)| b)
            .chain(std::iter::once(text.len()))
            .collect();
        let at = |ci: usize| byte_of_char[ci.min(byte_of_char.len().saturating_sub(1))];
        self.group
            .lint(&document)
            .into_iter()
            .filter_map(|l| {
                if l.suggestions.is_empty() {
                    return None;
                }
                let (bs, be) = (at(l.span.start), at(l.span.end));
                let is_spelling = is_spelling_kind(&format!("{:?}", l.lint_kind));
                let original = &text[bs..be];
                let choices: Vec<(String, String)> = l
                    .suggestions
                    .iter()
                    .map(|sug| {
                        if let Some(chars) = sug.as_replace_with() {
                            let rep: String = chars.iter().collect();
                            (rep.clone(), rep) // "quick" → replace with "quick"
                        } else if let Some(chars) = sug.as_insert_after() {
                            let ins: String = chars.iter().collect();
                            (format!("{original}{ins}  (insert)"), format!("{original}{ins}"))
                        } else {
                            ("(delete)".to_string(), String::new()) // Remove
                        }
                    })
                    .collect();
                Some((bs, be, is_spelling, choices))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn set(words: &[&str]) -> HashSet<String> {
        words.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn spans_for_words_underlines_every_whole_word_occurrence() {
        let text = "teh cat sat on teh mat";
        let spans = spans_for_words(text, &set(&["teh"]));
        // Both occurrences of "teh" (offsets 0 and 15), nothing else.
        assert_eq!(spans.iter().map(|(s, e, ..)| (*s, *e)).collect::<Vec<_>>(), [(0, 3), (15, 18)]);
        assert!(spans.iter().all(|(_, _, k, _)| k == "Spelling"));
    }

    #[test]
    fn spans_for_words_matches_whole_words_not_substrings() {
        // "cat" is flagged, but "category" must NOT be underlined (whole-word match only).
        let spans = spans_for_words("cat category cats", &set(&["cat"]));
        assert_eq!(spans.iter().map(|(s, e, ..)| (*s, *e)).collect::<Vec<_>>(), [(0, 3)]);
    }

    #[test]
    fn spans_for_words_handles_apostrophes_and_unicode() {
        // Contraction kept as one token; a Cyrillic misspelling located by byte offset.
        let text = "I dont know — цетер";
        let spans = spans_for_words(text, &set(&["dont", "цетер"]));
        let got: Vec<&str> = spans.iter().map(|(s, e, ..)| &text[*s..*e]).collect();
        assert_eq!(got, ["dont", "цетер"]);
    }

    #[test]
    fn spans_for_words_empty_set_is_empty() {
        assert!(spans_for_words("anything here", &HashSet::new()).is_empty());
    }
}
