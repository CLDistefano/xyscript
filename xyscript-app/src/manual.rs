//! MAN — search the user manual (`manual.typ`) by section: the prose companion to `HELP`'s
//! reference search. The manual is narrative, not entry-structured, so the unit of search is a
//! SECTION (a heading and the text under it). `search` greps section titles + bodies with the same
//! substring/regex rules as [`crate::help::search`]; the app lists each match as its heading plus the
//! full section body (no truncated excerpt / ellipsis).
//!
//! Like the help index, this parses the embedded `manual.typ` at runtime (cached), so it can't drift
//! from the shipped manual and needs no build step.

use regex::{Regex, RegexBuilder};
use std::sync::OnceLock;

const MANUAL_TYP: &str = include_str!("../../docs/manual.typ");

/// One manual section: a heading breadcrumb and its prose reduced to plain text.
pub struct Section {
    /// Heading path, e.g. `"Editing ▸ Search and replace"`.
    pub title: String,
    /// The section's prose (until the next heading), Typst markup reduced to readable plain text.
    pub body: String,
}

/// Reduce a run of manual Typst prose to plain, searchable text. Best-effort: it strips the markup
/// that would otherwise hide a match or clutter a snippet, and collapses whitespace — it does not aim
/// to render the manual.
fn plainify(src: &str) -> String {
    // Reduce `#raw("…")` FIRST, on the raw source — before `clean` unescapes `\"`→`"` and hides the
    // string boundaries. This pattern allows escaped chars inside, so raws that contain an escaped
    // quote (e.g. the `MENU "f"` example, written `#raw("MENU \"f\"")`) reduce correctly; the capture
    // is then unescaped. `clean`'s own simpler `#raw` pass runs later over what remains (nothing).
    static RAW: OnceLock<Regex> = OnceLock::new();
    let s = RAW
        .get_or_init(|| Regex::new(r#"#raw\("((?:\\.|[^"\\])*)"\)"#).unwrap())
        .replace_all(src, |c: &regex::Captures| c[1].replace("\\\"", "\"").replace("\\\\", "\\"));
    // Then the shared inline reductions (`#cmd`/`#sv`/`#func`/`#lib`, backticks, backslash-unescaping).
    let s = crate::help::clean(&s);
    // `#kbd("X")` → X; `#link("url", "label")` → label; `#link("url")` → url.
    static KBD: OnceLock<Regex> = OnceLock::new();
    let s = KBD.get_or_init(|| Regex::new(r#"#kbd\("([^"]*)"\)"#).unwrap()).replace_all(&s, "$1");
    static LINK2: OnceLock<Regex> = OnceLock::new();
    let s = LINK2
        .get_or_init(|| Regex::new(r#"#link\("[^"]*",\s*"([^"]*)"\)"#).unwrap())
        .replace_all(&s, "$1");
    static LINK1: OnceLock<Regex> = OnceLock::new();
    let s = LINK1.get_or_init(|| Regex::new(r#"#link\("([^"]*)"\)"#).unwrap()).replace_all(&s, "$1");
    // Callout wrappers `#tip("Title")[ … ]` / `#note[ … ]` etc.: keep the title + body, drop the shell.
    static CALLOUT: OnceLock<Regex> = OnceLock::new();
    let s = CALLOUT
        .get_or_init(|| Regex::new(r#"#(?:tip|note|warning|caution)\(?"?([^")\[]*)"?\)?\["#).unwrap())
        .replace_all(&s, "$1 ");
    // Whole-line block directives that carry no prose (layout/figure/table scaffolding).
    static DIRECTIVE: OnceLock<Regex> = OnceLock::new();
    let dir = DIRECTIVE.get_or_init(|| {
        Regex::new(r"(?m)^\s*#(?:v|pagebreak|figure|image|set|show|let|table|grid|columns|align|par|block|line|par|place|counter)\b.*$").unwrap()
    });
    let s = dir.replace_all(&s, " ");
    // Drop line comments.
    static COMMENT: OnceLock<Regex> = OnceLock::new();
    let s = COMMENT.get_or_init(|| Regex::new(r"(?m)^\s*//.*$").unwrap()).replace_all(&s, " ");
    // Strip the term-list leading `/ `, emphasis `*`/`_`, stray content brackets, and collapse space.
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '*' | '_' | '[' | ']' => {} // emphasis markers / content-block brackets
            _ => out.push(ch),
        }
    }
    // `/ term` list markers at a line start → just the term text.
    static TERM: OnceLock<Regex> = OnceLock::new();
    let out = TERM.get_or_init(|| Regex::new(r"(?m)^\s*/\s+").unwrap()).replace_all(&out, "");
    // Collapse all runs of whitespace (incl. the newlines we joined across) to single spaces.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn sections() -> &'static Vec<Section> {
    static SECS: OnceLock<Vec<Section>> = OnceLock::new();
    SECS.get_or_init(build)
}

fn heading_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(=+)\s+(.*\S)\s*$").unwrap())
}

fn build() -> Vec<Section> {
    let mut secs: Vec<Section> = Vec::new();
    let mut stack: Vec<String> = Vec::new(); // heading titles by (level-1)
    let mut cur_title: Option<String> = None;
    let mut buf = String::new();
    let flush = |secs: &mut Vec<Section>, title: &Option<String>, buf: &str| {
        if let Some(t) = title {
            secs.push(Section { title: t.clone(), body: plainify(buf) });
        }
    };
    for line in MANUAL_TYP.lines() {
        if let Some(c) = heading_re().captures(line) {
            // Close the section that just ended.
            flush(&mut secs, &cur_title, &buf);
            buf.clear();
            let level = c[1].len();
            // `#raw`/emphasis can appear in a heading too — reduce it for the breadcrumb.
            let title = plainify(&c[2]);
            stack.truncate(level.saturating_sub(1));
            while stack.len() < level.saturating_sub(1) {
                stack.push(String::new()); // (defensive: a skipped level)
            }
            stack.push(title);
            let crumb: Vec<&str> = stack.iter().map(String::as_str).filter(|s| !s.is_empty()).collect();
            cur_title = Some(crumb.join(" \u{25b8} "));
        } else if cur_title.is_some() {
            buf.push_str(line);
            buf.push('\n');
        }
        // (Lines before the first heading — the title page / TOC scaffolding — are ignored.)
    }
    flush(&mut secs, &cur_title, &buf);
    secs
}

/// Regex metacharacters that switch [`search`] to regex mode — the same set as [`crate::help`].
const REGEX_META: &[char] = &['\\', '.', '^', '$', '|', '[', '(', '{', '*', '+', '?', '#'];

/// Manual sections matching `query`, in manual order — regex when the query holds a metacharacter
/// (case-insensitive), else a case-insensitive substring. A metachar query that fails to compile
/// falls back to substring. Title matches come first, then body-only matches.
pub fn search(query: &str) -> Vec<&'static Section> {
    let raw = query.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    let re = raw
        .contains(|c| REGEX_META.contains(&c))
        .then(|| RegexBuilder::new(raw).case_insensitive(true).build().ok())
        .flatten();
    let (mut title_hits, mut body_hits): (Vec<&Section>, Vec<&Section>) = (Vec::new(), Vec::new());
    match &re {
        Some(re) => {
            for s in sections() {
                if re.is_match(&s.title) {
                    title_hits.push(s);
                } else if re.is_match(&s.body) {
                    body_hits.push(s);
                }
            }
        }
        None => {
            let q = raw.to_ascii_lowercase();
            for s in sections() {
                if s.title.to_ascii_lowercase().contains(&q) {
                    title_hits.push(s);
                } else if s.body.to_ascii_lowercase().contains(&q) {
                    body_hits.push(s);
                }
            }
        }
    }
    title_hits.append(&mut body_hits);
    title_hits
}

/// A short overview for bare-argument callers that want text (the GUI opens the manual instead).
pub fn overview() -> String {
    format!(
        "MAN <topic> — search the user manual (a {} sections).\n\
         e.g.  MAN scratchpad   MAN \"page setup\"   MAN ^Editing\n\n\
         A topic with a regex metacharacter (\\ . ^ $ | [ ( {{ * + ? #) is matched as a regular \
         expression, else as a substring. Bare MAN opens the manual.",
        sections().len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_parse_with_breadcrumbs() {
        let secs = sections();
        assert!(secs.len() > 40, "expected many sections, got {}", secs.len());
        // A known nested section carries its heading path.
        assert!(
            secs.iter().any(|s| s.title.contains('\u{25b8}')),
            "expected at least one nested breadcrumb"
        );
        // Prose is reduced (no leftover #raw(...) / heading markers).
        assert!(secs.iter().all(|s| !s.body.contains("#raw(")), "raw() should be reduced");
    }

    #[test]
    fn search_finds_sections_by_substring_and_regex() {
        // Substring: the Scratchpad chapter.
        let hits = search("scratchpad");
        assert!(hits.iter().any(|s| s.title.to_lowercase().contains("scratchpad")), "scratchpad section");
        // Regex: `^Editing` anchors to a section titled Editing (title starts with it).
        let hits = search("^Editing");
        assert!(hits.iter().any(|s| s.title.starts_with("Editing")), "regex ^Editing");
        // A metachar query that can't compile falls back to substring (no panic, may be empty).
        let _ = search("op(en");
        // Nonsense → nothing.
        assert!(search("zzzznotawordxyz").is_empty());
    }
}
