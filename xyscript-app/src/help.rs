//! Help index — Phase 0 of "Help v2".
//!
//! `HELP <name>` looks up the reference entry for a command, `$special`, or `function`. The text
//! comes from the SAME `reference.typ` the GUI opens for Help ▸ Reference and the no-omission tests
//! check, so the in-app help can't drift from the printed reference. There's no `typst` CLI here
//! (XyScript embeds typst as a library), so instead of a build-time `typst query` we parse the
//! embedded reference at runtime — the dictionary entries are a strictly regular Typst terms list
//! (`/ #cmd("CV", "CVA"): …`), which line-parses cleanly. Parsed once, lazily.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

/// The reference, embedded so `HELP` works on any install.
const REFERENCE_TYP: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../docs/reference.typ"));

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    Command,
    Special,
    Function,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Command => "command",
            Kind::Special => "special variable",
            Kind::Function => "function",
        }
    }
    fn from_macro(name: &str) -> Kind {
        match name {
            "sv" => Kind::Special,
            "func" => Kind::Function,
            _ => Kind::Command,
        }
    }
}

#[derive(Clone)]
pub struct HelpEntry {
    /// Headword(s) as written in the reference (e.g. `["CV", "CVA"]`).
    pub names: Vec<String>,
    pub kind: Kind,
    /// One-paragraph description, Typst markup reduced to plain text.
    pub synopsis: String,
}

struct Index {
    entries: Vec<HelpEntry>,
    /// `(kind, UPPER-CASED name)` → entry. Keyed by kind too, so `$SE` (the special) and `SE` (the
    /// search command) — same name, different kinds — stay distinct.
    by_name: HashMap<(Kind, String), usize>,
    /// Distinct headword names per kind (Command, Special, Function) — for the `HELP` overview.
    counts: [usize; 3],
}

fn index() -> &'static Index {
    static IDX: OnceLock<Index> = OnceLock::new();
    IDX.get_or_init(build)
}

/// The compiled `#cmd("A"[, "B"]…)` / `#sv("X")` / `#func("X"[, "sig"])` matcher (args have no
/// nested `)` in the reference). Capture 1 = macro name, capture 2 = the quoted-args blob.
fn entry_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"#(cmd|sv|func)\(([^)]*)\)"#).unwrap())
}

/// The quoted strings inside a macro's arg blob, in order.
fn quoted_args(blob: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r#""([^"]*)""#).unwrap());
    re.captures_iter(blob).map(|c| c[1].to_string()).collect()
}

/// Normalize a query / headword to a lookup key: strip a leading `$` and a trailing `()`, upper-case.
fn key(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix('$').unwrap_or(s);
    let s = s.strip_suffix("()").unwrap_or(s);
    s.to_ascii_uppercase()
}

/// The names declared by one macro: `#cmd` lists every quoted arg (synonyms); `#sv`/`#func` take
/// only the first (a `#func`'s later args are the signature, not a name).
fn macro_names(kind: &str, blob: &str) -> Vec<String> {
    let args = quoted_args(blob);
    match kind {
        "cmd" => args,
        _ => args.into_iter().take(1).collect(),
    }
}

/// Split a terms-list entry body into `(term, description)` at the first top-level `:` — the one
/// that isn't inside a `"…"` string or `(…)`. The term part (the `#macro(…)` run) has no such
/// colon, so the first top-level colon is always the separator.
fn split_term(rest: &str) -> Option<(&str, &str)> {
    let bytes = rest.as_bytes();
    let (mut depth, mut in_str) = (0i32, false);
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'"' => in_str = !in_str,
            b'(' if !in_str => depth += 1,
            b')' if !in_str => depth -= 1,
            b':' if !in_str && depth == 0 => return Some((&rest[..i], rest[i + 1..].trim_start())),
            _ => {}
        }
    }
    None
}

fn build() -> Index {
    let mut entries = Vec::new();
    // Names collected in two priorities: a name in an entry's HEADWORD position is its authoritative
    // definition; a name only mentioned inline in some description (e.g. a `$special` referenced by a
    // command) is a fallback. Headwords are indexed first so a pervasive inline mention can't steal a
    // name from the entry that actually defines it.
    let mut headwords: Vec<(Kind, String, usize)> = Vec::new();
    let mut inline: Vec<(Kind, String, usize)> = Vec::new();
    let lines: Vec<&str> = REFERENCE_TYP.lines().collect();
    let mut i = 0;
    // The "Commands by topic" section is a scannable index whose glosses are deliberately terser than
    // the alphabetical "Commands" A–Z (which is the authoritative, detailed source). Skip it here so a
    // command's help comes from its full A–Z entry, not its brief topical one (both sit in the doc, and
    // the topical section comes first — so without this it would win the `or_insert` below).
    let mut in_topical = false;
    while i < lines.len() {
        let line = lines[i].trim_end();
        if line == "= Commands by topic" {
            in_topical = true;
            i += 1;
            continue;
        }
        if line == "= Commands" {
            in_topical = false;
            i += 1;
            continue;
        }
        if in_topical {
            i += 1;
            continue;
        }
        // (1) Dictionary entry — a terms-list item `/ #cmd("X"[, "Y"]…): description`.
        if let Some(rest) = line.strip_prefix("/ ") {
            i += 1;
            if !(rest.starts_with("#cmd(") || rest.starts_with("#sv(") || rest.starts_with("#func(")) {
                continue;
            }
            let Some((term, desc)) = split_term(rest) else { continue };
            push_entry(&mut entries, &mut headwords, &mut inline, term_names(term), desc);
            continue;
        }
        // (2) Tour entry — a run of macro-only header lines (`#cmd("NW")`) followed by a prose
        // description, ending at a blank line or the next header.
        if let Some(first) = macro_only_line(line.trim_start()) {
            let mut names = first;
            i += 1;
            while i < lines.len() {
                let Some(more) = macro_only_line(lines[i].trim()) else { break };
                names.extend(more);
                i += 1;
            }
            let start = i;
            while i < lines.len()
                && !lines[i].trim().is_empty()
                && macro_only_line(lines[i].trim()).is_none()
            {
                i += 1;
            }
            let desc = lines[start..i].join(" ");
            push_entry(&mut entries, &mut headwords, &mut inline, names, &desc);
            continue;
        }
        i += 1;
    }
    // Count distinct headword names per kind for the overview (before merging in inline mentions).
    let mut counts = [0usize; 3];
    let mut seen = std::collections::HashSet::new();
    for &(k, ref name, _) in &headwords {
        if seen.insert((k, name.clone())) {
            counts[kind_slot(k)] += 1;
        }
    }
    let mut by_name = HashMap::new();
    for (k, name, i) in headwords.into_iter().chain(inline) {
        by_name.entry((k, name)).or_insert(i);
    }
    Index { entries, by_name, counts }
}

/// The `(kind, name)` headwords declared by a term/header — the macros in document order. The kind
/// of the FIRST macro stamps the entry.
fn term_names(term: &str) -> Vec<(Kind, String)> {
    entry_re()
        .captures_iter(term)
        .flat_map(|c| {
            let k = Kind::from_macro(&c[1]);
            macro_names(&c[1], &c[2]).into_iter().map(move |n| (k, n))
        })
        .collect()
}

/// Whether `trimmed` (already whitespace-trimmed) is a *macro-only* line — nothing but
/// `#cmd(…)`/`#sv(…)`/`#func(…)` tokens (and `/` separators / spaces). Returns the declared
/// headwords if so. Distinguishes a tour ENTRY header from prose that merely mentions a `#cmd(…)`.
fn macro_only_line(trimmed: &str) -> Option<Vec<(Kind, String)>> {
    if trimmed.is_empty() || !trimmed.starts_with('#') {
        return None;
    }
    let leftover = entry_re().replace_all(trimmed, "");
    if leftover.chars().any(|c| !c.is_whitespace() && c != '/') {
        return None; // prose remained → not a pure header line
    }
    Some(term_names(trimmed))
}

/// Record one parsed entry: its headword names + kind, the inline secondary names in its
/// description, and the cleaned synopsis.
fn push_entry(
    entries: &mut Vec<HelpEntry>,
    headwords: &mut Vec<(Kind, String, usize)>,
    inline: &mut Vec<(Kind, String, usize)>,
    names: Vec<(Kind, String)>,
    desc: &str,
) {
    let Some(&(kind, _)) = names.first() else { return };
    let idx = entries.len();
    for (k, n) in &names {
        headwords.push((*k, key(n), idx));
    }
    for cap in entry_re().captures_iter(desc) {
        let k = Kind::from_macro(&cap[1]);
        for n in macro_names(&cap[1], &cap[2]) {
            inline.push((k, key(&n), idx));
        }
    }
    let display: Vec<String> = names.into_iter().map(|(_, n)| n).collect();
    entries.push(HelpEntry { names: display, kind, synopsis: clean(desc) });
}

fn kind_slot(k: Kind) -> usize {
    match k {
        Kind::Command => 0,
        Kind::Special => 1,
        Kind::Function => 2,
    }
}

/// Reduce a Typst description to plain text: entry macros become their readable forms, code
/// backticks are dropped, and the common backslash escapes are unescaped.
fn clean(s: &str) -> String {
    let out = entry_re().replace_all(s, |c: &regex::Captures| {
        let names = quoted_args(&c[2]);
        match &c[1] {
            "sv" => format!("${}", names.first().cloned().unwrap_or_default()),
            "func" => format!("{}()", names.first().cloned().unwrap_or_default()),
            _ => names.join(" / "),
        }
    });
    let out = out.replace('`', "");
    unescape(&out).trim().to_string()
}

/// Unescape the backslash escapes the reference uses to keep markup characters literal.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(n @ ('$' | '|' | '#' | '@' | '*' | '_' | '\\' | '"' | '<' | '>')) => out.push(n),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Look up help for a command, `$special`, or `function` name (any casing). A leading `$` prefers a
/// special and a trailing `()` prefers a function — disambiguating names that exist as more than one
/// kind (e.g. `$SE` the special vs `SE` the command); otherwise the order is command, special,
/// function. Falls through to the other kinds if the preferred one has no entry.
pub fn lookup(name: &str) -> Option<HelpEntry> {
    let idx = index();
    let raw = name.trim();
    let order = if raw.starts_with('$') {
        [Kind::Special, Kind::Command, Kind::Function]
    } else if raw.ends_with("()") {
        [Kind::Function, Kind::Command, Kind::Special]
    } else {
        [Kind::Command, Kind::Special, Kind::Function]
    };
    let k = key(raw);
    order
        .into_iter()
        .find_map(|kind| idx.by_name.get(&(kind, k.clone())).map(|&i| idx.entries[i].clone()))
}

/// The closest known names to `name` (for "did you mean"), nearest first, up to `max`. Only
/// reasonably-close matches (edit distance ≤ 3 and less than the longer length) are offered.
pub fn suggest(name: &str, max: usize) -> Vec<String> {
    let want = key(name);
    if want.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(usize, &str)> = index()
        .by_name
        .keys()
        .map(|(_, k)| (levenshtein(&want, k), k.as_str()))
        .filter(|&(d, k)| d > 0 && d <= 3 && d < k.len().max(want.len()))
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    scored.dedup_by(|a, b| a.1 == b.1); // a name in two kinds shouldn't appear twice
    scored.into_iter().take(max).map(|(_, k)| k.to_string()).collect()
}

/// A short overview shown by a bare `HELP` (no argument).
pub fn overview() -> String {
    let [c, s, f] = index().counts;
    format!(
        "HELP <name> — look up a command, $special, or function.\n\
         e.g.  HELP CV   HELP $CWD   HELP UPPER\n\n\
         {c} commands, {s} special variables, and {f} functions are documented.\n\
         Open the full reference with  Help \u{25b8} Reference."
    )
}

/// Classic Levenshtein edit distance (two-row DP), for `suggest`.
fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_up_commands_specials_and_functions() {
        let cv = lookup("cv").expect("CV documented");
        assert_eq!(cv.kind.label(), "command");
        assert!(cv.names.iter().any(|n| n == "CV") && cv.names.iter().any(|n| n == "CVA"));
        assert!(cv.synopsis.contains("change-with-verify"), "synopsis: {}", cv.synopsis);
        // `$`-decoration and casing are tolerated for specials…
        assert_eq!(lookup("$cwd").unwrap().kind, Kind::Special);
        // …and `()` for functions.
        assert_eq!(lookup("UPPER()").unwrap().kind, Kind::Function);
    }

    #[test]
    fn markup_is_reduced_to_plain_text() {
        let s = lookup("CMPLACE").unwrap().synopsis;
        assert!(!s.contains('`'), "backticks stripped: {s}");
        assert!(!s.contains("#cmd("), "macros expanded: {s}");
        assert!(s.contains("$CMPLACE"), "an #sv became $-form: {s}");
    }

    #[test]
    fn inline_secondary_entries_are_reachable() {
        // `DFA` is documented inline on the `DF` line; it should still resolve.
        assert!(lookup("DFA").is_some());
    }

    #[test]
    fn suggest_finds_near_misses_but_not_nonsense() {
        let near = suggest("CVV", 5); // one edit from CV / CVA
        assert!(near.iter().any(|s| s == "CV" || s == "CVA"), "got {near:?}");
        assert!(suggest("zzzzzzzz", 5).is_empty(), "no wild suggestions");
    }
}
