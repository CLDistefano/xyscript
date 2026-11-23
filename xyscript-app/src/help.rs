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

use regex::{Regex, RegexBuilder};

/// The reference, embedded so `HELP` works on any install.
const REFERENCE_TYP: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../docs/reference.typ"));

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    Command,
    Special,
    Function,
    Library,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Command => "command",
            Kind::Special => "special variable",
            Kind::Function => "function",
            Kind::Library => "library routine",
        }
    }
    fn from_macro(name: &str) -> Kind {
        match name {
            "sv" => Kind::Special,
            "func" => Kind::Function,
            "lib" => Kind::Library,
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
    /// Distinct headword names per kind (Command, Special, Function, Library) — for the `HELP` overview.
    counts: [usize; 4],
}

fn index() -> &'static Index {
    static IDX: OnceLock<Index> = OnceLock::new();
    IDX.get_or_init(build)
}

/// The compiled `#cmd("A"[, "B"]…)` / `#sv("X")` / `#func("X"[, "sig"])` matcher (args have no
/// nested `)` in the reference). Capture 1 = macro name, capture 2 = the quoted-args blob.
fn entry_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"#(cmd|sv|func|lib)\(([^)]*)\)"#).unwrap())
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
        // `#cmd`/`#lib` list every quoted arg (command synonyms / library-routine aliases); `#sv`/`#func`
        // take only the first (a `#func`'s later args are the signature, not a name).
        "cmd" | "lib" => args,
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
    // The "… by topic" sections are scannable indexes whose glosses are deliberately terser than the
    // alphabetical A–Z listings (which are the authoritative, detailed source). Skip them here so an
    // item's help comes from its full A–Z entry, not its brief topical one (both sit in the doc, and the
    // topical section comes first — so without this it would win the `or_insert` below). This covers the
    // built-in "Commands by topic" AND the "Library routines by topic" listings.
    let mut in_topical = false;
    while i < lines.len() {
        let line = lines[i].trim_end();
        if line == "= Commands by topic" || line == "= Library routines by topic" {
            in_topical = true;
            i += 1;
            continue;
        }
        if line == "= Commands" || line == "= Library routines" {
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
            if !(rest.starts_with("#cmd(")
                || rest.starts_with("#sv(")
                || rest.starts_with("#func(")
                || rest.starts_with("#lib("))
            {
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
    let mut counts = [0usize; 4];
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
        Kind::Library => 3,
    }
}

/// Reduce a Typst description to plain text: entry macros become their readable forms, code
/// backticks are dropped, and the common backslash escapes are unescaped.
pub(crate) fn clean(s: &str) -> String {
    let out = entry_re().replace_all(s, |c: &regex::Captures| {
        let names = quoted_args(&c[2]);
        match &c[1] {
            "sv" => format!("${}", names.first().cloned().unwrap_or_default()),
            "func" => format!("{}()", names.first().cloned().unwrap_or_default()),
            _ => names.join(" / "),
        }
    });
    // Inline `#raw("code")` → its literal text. The library-routine entries use it for inline code
    // (e.g. `#raw("$PDFreader")`) where the rest of the reference uses backticks; reduce both.
    static RAW_RE: OnceLock<Regex> = OnceLock::new();
    let raw_re = RAW_RE.get_or_init(|| Regex::new(r#"#raw\("([^"]*)"\)"#).unwrap());
    let out = raw_re.replace_all(&out, |c: &regex::Captures| c[1].to_string());
    // Inline content wrappers — `#emph[x]`, `#strong[x]`, `#underline[x]`, `#highlight[x]`, and
    // `#text(…)[x]` — reduce to their inner text `x` (so the reference's `#emph[not]` etc. don't show
    // their markup). The bracketed body is non-nested in entries, so a single pass suffices.
    static WRAP_RE: OnceLock<Regex> = OnceLock::new();
    let wrap_re = WRAP_RE
        .get_or_init(|| Regex::new(r#"#(?:emph|strong|underline|highlight|text)(?:\([^)]*\))?\[([^\]]*)\]"#).unwrap());
    let out = wrap_re.replace_all(&out, |c: &regex::Captures| c[1].to_string());
    let out = out.replace('`', "");
    // Strip Typst emphasis markers `*bold*` / `_italic_` (all `*`/`_` in an entry are emphasis).
    // A `\`-escaped pair is carried through verbatim so a literal `\*`/`\_` survives to be unescaped
    // below rather than losing its `*`/`_`. Mirrors the manual reducer, which also drops emphasis.
    let mut stripped = String::with_capacity(out.len());
    let mut it = out.chars();
    while let Some(c) = it.next() {
        match c {
            '\\' => {
                stripped.push('\\');
                if let Some(n) = it.next() {
                    stripped.push(n);
                }
            }
            '*' | '_' => {} // emphasis marker
            _ => stripped.push(c),
        }
    }
    unescape(&stripped).trim().to_string()
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
    // A `$` prefers a special, a trailing `()` a function; otherwise try a built-in command first, then
    // a special, a library routine, and finally a function. Library routines can't shadow a built-in
    // command (the loader rejects that), so the only bare-name overlap is with a like-named FUNCTION —
    // e.g. the `log` routine vs the `LOG()` function; a bare `HELP log` favours the routine you'd type on
    // the command line, and the function stays reachable as `HELP log()`.
    let order = if raw.starts_with('$') {
        [Kind::Special, Kind::Command, Kind::Function, Kind::Library]
    } else if raw.ends_with("()") {
        [Kind::Function, Kind::Command, Kind::Special, Kind::Library]
    } else {
        [Kind::Command, Kind::Special, Kind::Library, Kind::Function]
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

/// Regex metacharacters that switch [`search`] from substring to regex mode when the query contains
/// one. (`#` is here per the documented set even though the `regex` crate treats it as a literal
/// outside verbose mode — matching on it then behaves like a substring, which is harmless.)
const REGEX_META: &[char] = &['\\', '.', '^', '$', '|', '[', '(', '{', '*', '+', '?', '#'];

/// The `(phrase, regex)` used to EMPHasize a search term in the HELP/MAN results dialog. Always regex
/// mode (`EMPH/R` semantics): a query that is itself a regex (holds a metacharacter) highlights its own
/// matches; a plain term is escaped and wrapped in `\w*…\w*` so the WHOLE word it sits in lights up —
/// so `push` highlights `pushed`, `pushes`, `pushing`, and so on. `None` for an empty term (a leading
/// `$` / trailing `()` is ignored first, matching how `search` normalizes a substring query).
pub(crate) fn result_emphasis(query: &str, substring: bool) -> Option<(String, bool)> {
    let raw = query.trim();
    let raw = raw.strip_prefix('$').unwrap_or(raw);
    let raw = raw.strip_suffix("()").unwrap_or(raw).trim();
    if raw.is_empty() {
        return None;
    }
    let phrase = if raw.contains(|c| REGEX_META.contains(&c)) {
        raw.to_string()
    } else if substring {
        format!(r"\w*{}\w*", regex::escape(raw))
    } else {
        // Word-start (the default): highlight from the START of the word the term begins, so `mark`
        // lights up `marks`/`marker` but not `benchmark` — matching the search's word-start rule.
        format!(r"\b{}\w*", regex::escape(raw))
    };
    Some((phrase, true))
}

/// Every help entry whose name(s) OR synopsis match `query` — the grep-like `HELP` fallback. If the
/// query contains a regex metacharacter (`REGEX_META`) it is treated as a case-insensitive **regular
/// expression**; otherwise it matches whole words: by default a word that STARTS with the term
/// (word-start — so `table` finds `table`/`tables` but not `portable`), or, when `substring` is set
/// (`/A`), the term anywhere in a word (the old forgiving behaviour). A leading `$` / trailing `()`
/// is ignored, case-insensitive. A query that looks like a regex but fails to compile falls back to
/// a word search. NAME matches come first (more relevant), then description-only matches; each group
/// keeps reference order. Returns all matches (the caller caps the display).
pub fn search(query: &str, substring: bool) -> Vec<HelpEntry> {
    let raw = query.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    // Regex mode when a metacharacter is present AND the pattern compiles; else substring.
    let re = raw
        .contains(|c| REGEX_META.contains(&c))
        .then(|| RegexBuilder::new(raw).case_insensitive(true).build().ok())
        .flatten();
    let entries = &index().entries;
    let (mut name_hits, mut desc_hits) = (Vec::new(), Vec::new());
    match &re {
        // Regex: match names / synopsis directly (case-insensitive built in).
        Some(re) => {
            for e in entries {
                if e.names.iter().any(|n| re.is_match(n)) {
                    name_hits.push(e.clone());
                } else if re.is_match(&e.synopsis) {
                    desc_hits.push(e.clone());
                }
            }
        }
        // Substring: a leading `$` / trailing `()` is ignored, case-insensitive.
        None => {
            let q = raw.trim_start_matches('$').trim_end_matches("()").to_ascii_lowercase();
            if q.is_empty() {
                return Vec::new(); // a `$`/`()`-only query has nothing left to match
            }
            for e in entries {
                if e.names.iter().any(|n| haystack_hit(n, &q, substring)) {
                    name_hits.push(e.clone());
                } else if haystack_hit(&e.synopsis, &q, substring) {
                    desc_hits.push(e.clone());
                }
            }
        }
    }
    name_hits.append(&mut desc_hits);
    name_hits
}

// ---- boolean / proximity query (AND / OR / NOT / "phrase" / NEAR[/n]) ---------------------------

/// A parsed HELP query. `Word` matches a token by SUBSTRING (forgiving, like plain `HELP`); a quoted
/// `Phrase` matches consecutive words; `Near` is true when its two operands occur within `n` words;
/// `And`/`Or`/`Not` compose. Leaves and operators are all lowercased/normalized at lex time.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Query {
    Word(String),
    Phrase(Vec<String>),
    Not(Box<Query>),
    And(Vec<Query>),
    Or(Vec<Query>),
    Near(Box<Query>, Box<Query>, usize),
}

/// Proximity when `NEAR` is written without `/n`.
const NEAR_DEFAULT: usize = 5;

enum Tok {
    And,
    Or,
    Not,
    Near(usize),
    LParen,
    RParen,
    Phrase(Vec<String>),
    Word(String),
}

/// Lowercased alphanumeric words of `s` (the tokenization both the query and an entry's text use).
pub(crate) fn words_of(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect()
}

/// Whether one tokenized `text_word` matches a `query_word` under the match mode: by SUBSTRING
/// (`/A` — the term anywhere in the word) or, by default, by PREFIX (word-start, which also admits
/// inflections like `mark`→`marks`). Both inputs are already lowercased.
pub(crate) fn word_hit(text_word: &str, query_word: &str, substring: bool) -> bool {
    if substring {
        text_word.contains(query_word)
    } else {
        text_word.starts_with(query_word)
    }
}

/// Whether any whole word of `haystack` matches the (lowercased) `needle` under the mode — the
/// name/synopsis/title test for the plain (non-boolean) search.
pub(crate) fn haystack_hit(haystack: &str, needle: &str, substring: bool) -> bool {
    words_of(haystack).iter().any(|w| word_hit(w, needle, substring))
}

/// Whether a tokenized `text_word` matches one word of a QUOTED PHRASE. A phrase is an explicit
/// "exactly this" request, so by default each of its words must match a WHOLE word exactly (not by
/// prefix the way a bare term does) — this keeps `"status line"` from matching `statusbar lineup` or
/// `$STATUSLINE`. `/A` (`substring`) still relaxes it to match the word anywhere. Both lowercased.
fn phrase_word_hit(text_word: &str, phrase_word: &str, substring: bool) -> bool {
    if substring {
        text_word.contains(phrase_word)
    } else {
        text_word == phrase_word
    }
}

/// Whether `q` should be read as a boolean/proximity query rather than the plain substring/regex
/// `search`. True when it has an explicit operator (`AND`/`OR`/`NOT`/`NEAR`), a quoted phrase, or is
/// plain multi-word (implicit AND) — but NOT a single regex-ish term, so `HELP ^PV` stays a regex.
pub fn is_boolean_query(q: &str) -> bool {
    if q.contains('"') {
        return true;
    }
    let words: Vec<&str> = q.split_whitespace().collect();
    let has_op = words.iter().any(|w| {
        let u = w.to_ascii_uppercase();
        u == "AND" || u == "OR" || u == "NOT" || u == "NEAR" || u.starts_with("NEAR/")
    });
    has_op || (words.len() >= 2 && !q.contains(|c| REGEX_META.contains(&c)))
}

fn lex_query(q: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut chars = q.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if c == '(' || c == ')' {
            chars.next();
            toks.push(if c == '(' { Tok::LParen } else { Tok::RParen });
            continue;
        }
        if c == '"' {
            chars.next();
            let mut s = String::new();
            for c in chars.by_ref() {
                if c == '"' {
                    break;
                }
                s.push(c);
            }
            let ws = words_of(&s);
            if !ws.is_empty() {
                toks.push(Tok::Phrase(ws));
            }
            continue;
        }
        // A bare run until whitespace / paren / quote.
        let mut s = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() || c == '(' || c == ')' || c == '"' {
                break;
            }
            s.push(c);
            chars.next();
        }
        let up = s.to_ascii_uppercase();
        if up == "AND" {
            toks.push(Tok::And);
        } else if up == "OR" {
            toks.push(Tok::Or);
        } else if up == "NOT" {
            toks.push(Tok::Not);
        } else if up == "NEAR" {
            toks.push(Tok::Near(NEAR_DEFAULT));
        } else if let Some(n) = up.strip_prefix("NEAR/").and_then(|r| r.parse::<usize>().ok()).filter(|&n| n > 0) {
            toks.push(Tok::Near(n));
        } else {
            // A punctuated word (`wrap-indent`, `$cwd`, `len()`) splits into its alnum pieces: one piece
            // is a Word, several become an implicit Phrase.
            match words_of(&s).as_slice() {
                [] => {}
                [one] => toks.push(Tok::Word(one.clone())),
                many => toks.push(Tok::Phrase(many.to_vec())),
            }
        }
    }
    toks
}

// Recursive descent: or := and (OR and)* ; and := unary (AND? unary)* (implicit AND) ;
// unary := NOT unary | near ; near := atom (NEAR atom)* ; atom := (or) | phrase | word.
fn parse(toks: &[Tok]) -> Option<Query> {
    let mut i = 0;
    parse_or(toks, &mut i)
}
fn parse_or(toks: &[Tok], i: &mut usize) -> Option<Query> {
    let mut parts = vec![parse_and(toks, i)?];
    while matches!(toks.get(*i), Some(Tok::Or)) {
        *i += 1;
        parts.push(parse_and(toks, i)?);
    }
    Some(if parts.len() == 1 { parts.pop().unwrap() } else { Query::Or(parts) })
}
fn parse_and(toks: &[Tok], i: &mut usize) -> Option<Query> {
    let mut parts = vec![parse_unary(toks, i)?];
    loop {
        match toks.get(*i) {
            Some(Tok::And) => {
                *i += 1;
                parts.push(parse_unary(toks, i)?);
            }
            // Adjacent operands with no explicit operator = implicit AND.
            Some(Tok::Not | Tok::Word(_) | Tok::Phrase(_) | Tok::LParen) => parts.push(parse_unary(toks, i)?),
            _ => break,
        }
    }
    Some(if parts.len() == 1 { parts.pop().unwrap() } else { Query::And(parts) })
}
fn parse_unary(toks: &[Tok], i: &mut usize) -> Option<Query> {
    if matches!(toks.get(*i), Some(Tok::Not)) {
        *i += 1;
        return Some(Query::Not(Box::new(parse_unary(toks, i)?)));
    }
    parse_near(toks, i)
}
fn parse_near(toks: &[Tok], i: &mut usize) -> Option<Query> {
    let first = parse_atom(toks, i)?;
    if !matches!(toks.get(*i), Some(Tok::Near(_))) {
        return Some(first);
    }
    // `a NEAR/3 b NEAR/5 c` → AND of the adjacent pairs.
    let (mut pairs, mut prev) = (Vec::new(), first);
    while let Some(&Tok::Near(n)) = toks.get(*i) {
        *i += 1;
        let next = parse_atom(toks, i)?;
        pairs.push(Query::Near(Box::new(prev), Box::new(next.clone()), n));
        prev = next;
    }
    Some(if pairs.len() == 1 { pairs.pop().unwrap() } else { Query::And(pairs) })
}
fn parse_atom(toks: &[Tok], i: &mut usize) -> Option<Query> {
    match toks.get(*i) {
        Some(Tok::LParen) => {
            *i += 1;
            let q = parse_or(toks, i)?;
            if matches!(toks.get(*i), Some(Tok::RParen)) {
                *i += 1;
            }
            Some(q)
        }
        Some(Tok::Word(w)) => {
            let w = w.clone();
            *i += 1;
            Some(Query::Word(w))
        }
        Some(Tok::Phrase(ws)) => {
            let ws = ws.clone();
            *i += 1;
            Some(Query::Phrase(ws))
        }
        _ => None, // a dangling/misplaced operator → whole parse fails → caller falls back to `search`
    }
}

/// Word index positions of a `Word`/`Phrase` operand in `words` (for `Near`); empty for other nodes.
fn positions(q: &Query, words: &[String], substring: bool) -> Vec<usize> {
    match q {
        Query::Word(w) => words
            .iter()
            .enumerate()
            .filter(|(_, t)| word_hit(t, w, substring))
            .map(|(i, _)| i)
            .collect(),
        Query::Phrase(ws) if !ws.is_empty() && words.len() >= ws.len() => (0..=words.len() - ws.len())
            .filter(|&i| ws.iter().enumerate().all(|(k, w)| phrase_word_hit(&words[i + k], w, substring)))
            .collect(),
        _ => Vec::new(),
    }
}
fn eval(q: &Query, words: &[String], substring: bool) -> bool {
    match q {
        Query::Word(w) => words.iter().any(|t| word_hit(t, w, substring)),
        // A phrase matches a window of consecutive words, each an EXACT whole word of the phrase
        // (word-start would make `"status line"` match `statusbar lineup`); `/A` relaxes to substring.
        Query::Phrase(ws) => {
            !ws.is_empty()
                && words.len() >= ws.len()
                && (0..=words.len() - ws.len())
                    .any(|i| ws.iter().enumerate().all(|(k, w)| phrase_word_hit(&words[i + k], w, substring)))
        }
        Query::Not(x) => !eval(x, words, substring),
        Query::And(xs) => xs.iter().all(|x| eval(x, words, substring)),
        Query::Or(xs) => xs.iter().any(|x| eval(x, words, substring)),
        Query::Near(a, b, n) => {
            let (pa, pb) = (positions(a, words, substring), positions(b, words, substring));
            pa.iter().any(|&x| pb.iter().any(|&y| x.abs_diff(y) <= *n))
        }
    }
}
/// Parse a raw query into an AST (lex + recursive descent), or `None` if malformed. Shared by the
/// reference ([`search_query`]) and the manual (`manual::search_query`).
pub(crate) fn compile_query(q: &str) -> Option<Query> {
    parse(&lex_query(q))
}

/// Whether the parsed query matches a document given its lowercased alphanumeric `words`. `substring`
/// picks the per-word mode: word-start (default) or anywhere-in-word (`/A`).
pub(crate) fn query_matches(q: &Query, words: &[String], substring: bool) -> bool {
    eval(q, words, substring)
}

/// Positive (not negated) leaf words — for name-match ranking and the results highlight.
pub(crate) fn positive_leaf_words(q: &Query) -> Vec<String> {
    fn go(q: &Query, pos: bool, out: &mut Vec<String>) {
        match q {
            Query::Word(w) if pos => out.push(w.clone()),
            Query::Phrase(ws) if pos => out.extend(ws.iter().cloned()),
            Query::Word(_) | Query::Phrase(_) => {}
            Query::Not(x) => go(x, !pos, out),
            Query::And(xs) | Query::Or(xs) => xs.iter().for_each(|x| go(x, pos, out)),
            Query::Near(a, b, _) => {
                go(a, pos, out);
                go(b, pos, out);
            }
        }
    }
    let mut out = Vec::new();
    go(q, true, &mut out);
    out
}

/// Highlight regex for a single term under the mode: word-start (`\bw\w*`) by default, or the whole
/// surrounding word under `/A` (`\w*w\w*`).
fn leaf_word_pattern(w: &str, substring: bool) -> String {
    if substring {
        format!(r"\w*{}\w*", regex::escape(w))
    } else {
        format!(r"\b{}\w*", regex::escape(w))
    }
}

/// Highlight regex for a QUOTED PHRASE: its words in sequence, separated by non-word runs, so it
/// lights up the phrase itself (`status line`) and not every stray `status`/`line`. Exact words with
/// end boundaries by default; `/A` relaxes each to a substring.
fn leaf_phrase_pattern(ws: &[String], substring: bool) -> String {
    let mut s = String::new();
    for (i, w) in ws.iter().enumerate() {
        if i > 0 {
            s.push_str(r"\W+");
        }
        let e = regex::escape(w);
        if substring {
            s.push_str(&format!(r"\w*{e}\w*"));
        } else if i == 0 {
            s.push_str(&format!(r"\b{e}"));
        } else {
            s.push_str(&e);
        }
    }
    if !substring {
        s.push_str(r"\b"); // exact end — don't bleed into `lineup`
    }
    s
}

/// Positive (not negated) leaf HIGHLIGHT patterns, preserving phrase adjacency — the results-highlight
/// counterpart to [`positive_leaf_words`] (which flattens phrases for name-match ranking).
fn positive_leaf_patterns(q: &Query, substring: bool) -> Vec<String> {
    fn go(q: &Query, pos: bool, substring: bool, out: &mut Vec<String>) {
        match q {
            Query::Word(w) if pos => out.push(leaf_word_pattern(w, substring)),
            Query::Phrase(ws) if pos && !ws.is_empty() => out.push(leaf_phrase_pattern(ws, substring)),
            Query::Word(_) | Query::Phrase(_) => {}
            Query::Not(x) => go(x, !pos, substring, out),
            Query::And(xs) | Query::Or(xs) => xs.iter().for_each(|x| go(x, pos, substring, out)),
            Query::Near(a, b, _) => {
                go(a, pos, substring, out);
                go(b, pos, substring, out);
            }
        }
    }
    let mut out = Vec::new();
    go(q, true, substring, &mut out);
    out
}

fn entry_words(e: &HelpEntry) -> Vec<String> {
    words_of(&format!("{} {}", e.names.join(" "), e.synopsis))
}

/// The query entry point `HELP` uses: a boolean/proximity search when the query reads that way
/// ([`is_boolean_query`]), else the plain substring/regex [`search`]. Matching entries are ranked
/// name-hits first (any positive term appears in a headword), then reference order.
pub fn search_query(query: &str, substring: bool) -> Vec<HelpEntry> {
    if !is_boolean_query(query) {
        return search(query, substring);
    }
    let Some(q) = parse(&lex_query(query)) else {
        return search(query, substring); // malformed → degrade to the plain search
    };
    let leaves = positive_leaf_words(&q);
    let (mut name_hits, mut desc_hits) = (Vec::new(), Vec::new());
    for e in &index().entries {
        let words = entry_words(e);
        if eval(&q, &words, substring) {
            let name_hit = e
                .names
                .iter()
                .any(|n| leaves.iter().any(|w| haystack_hit(n, w, substring)));
            if name_hit {
                name_hits.push(e.clone());
            } else {
                desc_hits.push(e.clone());
            }
        }
    }
    name_hits.append(&mut desc_hits);
    name_hits
}

/// Results-highlight pattern for a query: for a boolean query, an alternation of its POSITIVE leaf
/// patterns — bare terms as whole words, a `"quoted phrase"` as the adjacent phrase itself (so it
/// lights up `status line`, not every stray `status`/`line`); otherwise the plain [`result_emphasis`].
/// `None` when there's nothing to light.
pub(crate) fn query_emphasis(query: &str, substring: bool) -> Option<(String, bool)> {
    if !is_boolean_query(query) {
        return result_emphasis(query, substring);
    }
    let q = parse(&lex_query(query))?;
    let mut pats = positive_leaf_patterns(&q, substring);
    pats.sort();
    pats.dedup();
    pats.retain(|p| !p.is_empty());
    if pats.is_empty() {
        return None;
    }
    // Each fragment is already anchored to the mode; alternate them at the top level.
    Some((pats.join("|"), true))
}

/// A short overview shown by a bare `HELP` (no argument).
pub fn overview() -> String {
    let [c, s, f, l] = index().counts;
    format!(
        "HELP <name> — look up a command, $special, function, or library routine.\n\
         e.g.  HELP CV   HELP $CWD   HELP UPPER   HELP num2words\n\n\
         {c} commands, {s} special variables, {f} functions, and {l} library routines are documented.\n\
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
    fn clean_reduces_emphasis_and_content_wrappers() {
        // Emphasis markers and inline content wrappers reduce to their text (like the manual reducer),
        // so `REF` output carries no leftover Typst markup.
        assert_eq!(clean("run it #emph[now] and *stop*"), "run it now and stop");
        assert_eq!(clean("#strong[bold] _soft_ #underline[u]"), "bold soft u");
        assert_eq!(clean("a #text(weight: \"bold\")[big] one"), "a big one");
        // Escaped literals survive (the `\*` is kept, not treated as emphasis).
        assert_eq!(clean(r"a literal \* char"), "a literal * char");
    }

    #[test]
    fn a_real_entry_has_no_leftover_markup() {
        // AUTOREOPEN's description uses `#emph[unsaved]` / `#emph[independent of]`; the synopsis must
        // carry neither the wrapper nor a bare `*`/`_` emphasis marker.
        let s = lookup("AUTOREOPEN").unwrap().synopsis;
        assert!(!s.contains("#emph"), "emph reduced: {s}");
        assert!(s.contains("unsaved") && s.contains("independent of"), "text kept: {s}");
        assert!(!s.contains('#'), "no leftover directives: {s}");
    }

    #[test]
    fn inline_secondary_entries_are_reachable() {
        // `DFA` is documented inline on the `DF` line; it should still resolve.
        assert!(lookup("DFA").is_some());
    }

    #[test]
    fn resolves_library_routines() {
        let n2w = lookup("num2words").expect("num2words documented");
        assert_eq!(n2w.kind, Kind::Library);
        assert_eq!(n2w.kind.label(), "library routine");
        assert!(n2w.synopsis.to_lowercase().contains("number"), "synopsis: {}", n2w.synopsis);
        // Case-insensitive, incl. mixed-case and hyphenated routine names.
        assert_eq!(lookup("GETIP").unwrap().kind, Kind::Library);
        assert_eq!(lookup("getHolidays-ca").unwrap().kind, Kind::Library);
        // A bare name shared with a function favours the routine; the function needs its `()`.
        assert_eq!(lookup("log").unwrap().kind, Kind::Library);
        assert_eq!(lookup("log()").unwrap().kind, Kind::Function);
        // Inline `#raw("…")` in a routine gloss is reduced to plain text in the synopsis.
        let ty = lookup("typst").unwrap().synopsis;
        assert!(!ty.contains("#raw("), "raw not reduced: {ty}");
        assert!(ty.contains("$PDFreader"), "synopsis: {ty}");
    }

    #[test]
    fn help_resolves_every_library_routine() {
        // Same completeness guaranty as the reference: every user-facing bundled routine (a SUB with a
        // `# DOC:` line) resolves via HELP to its library entry. No-DOC helpers are exempt.
        const LIB: &str = include_str!("../../packaging/runtime/xyscript.lib");
        let mut required: Vec<String> = Vec::new();
        let (mut cur, mut has_doc): (Option<String>, bool) = (None, false);
        for line in LIB.lines() {
            let t = line.trim_start();
            if let Some(rest) = t.strip_prefix("SUB ") {
                if let Some(n) = cur.take() {
                    if has_doc {
                        required.push(n);
                    }
                }
                has_doc = false;
                cur = rest.split_whitespace().next().map(str::to_string);
            } else if t.starts_with("# DOC:") {
                has_doc = true;
            }
        }
        if let Some(n) = cur.take() {
            if has_doc {
                required.push(n);
            }
        }
        assert!(required.len() >= 70, "sanity: only {} documented routines parsed", required.len());
        let missing: Vec<&String> =
            required.iter().filter(|n| lookup(n).map(|e| e.kind) != Some(Kind::Library)).collect();
        assert!(missing.is_empty(), "HELP can't resolve {} library routine(s): {missing:?}", missing.len());
    }

    #[test]
    fn result_emphasis_word_start_vs_substring() {
        // Default (word-start): the regex anchors at the start of a word, lighting up the term and
        // its inflections but NOT a word that merely ends with it.
        let (p, rx) = result_emphasis("push", false).unwrap();
        assert!(rx, "emphasis is always regex mode");
        assert_eq!(p, r"\bpush\w*");
        let re = Regex::new(&p).unwrap();
        for w in ["push", "pushed", "pushes", "pushing"] {
            assert_eq!(re.find(w).map(|m| m.as_str()), Some(w), "{w} should word-start match");
        }
        assert!(re.find("repush").is_none(), "word-start must not light a trailing occurrence");
        // `/A` (substring): the forgiving form that lights up the whole surrounding word.
        let (ps, _) = result_emphasis("push", true).unwrap();
        assert_eq!(ps, r"\w*push\w*");
        assert_eq!(Regex::new(&ps).unwrap().find("repush").map(|m| m.as_str()), Some("repush"));
        // A regex term passes through unchanged (it highlights its own matches), in either mode.
        assert_eq!(result_emphasis("^PV", false).unwrap().0, "^PV");
        // A leading `$` / trailing `()` is ignored (matches how the search normalizes); empty → None.
        assert_eq!(result_emphasis("$cwd", false).unwrap().0, r"\bcwd\w*");
        assert_eq!(result_emphasis("UPPER()", true).unwrap().0, r"\w*UPPER\w*");
        assert!(result_emphasis("   ", false).is_none());
        assert!(result_emphasis("$", false).is_none());
    }

    #[test]
    fn suggest_finds_near_misses_but_not_nonsense() {
        let near = suggest("CVV", 5); // one edit from CV / CVA
        assert!(near.iter().any(|s| s == "CV" || s == "CVA"), "got {near:?}");
        assert!(suggest("zzzzzzzz", 5).is_empty(), "no wild suggestions");
    }

    // ---- boolean / proximity query ----------------------------------------------------------

    /// Evaluate a parsed query against a plain-text haystack (synthetic, deterministic). Uses the
    /// word-start default; [`word_start_is_the_default_substring_is_opt_in`] covers the `/A` mode.
    fn matches(query: &str, text: &str) -> bool {
        let q = parse(&lex_query(query)).expect("parses");
        let words = words_of(text);
        eval(&q, &words, false)
    }

    #[test]
    fn word_start_is_the_default_substring_is_opt_in() {
        let table = parse(&lex_query("table")).unwrap();
        // Word-start (default): `table` matches the real word and its plural, but NOT `portable`.
        assert!(eval(&table, &words_of("a plain table"), false));
        assert!(eval(&table, &words_of("two tables here"), false), "admits an inflection");
        assert!(!eval(&table, &words_of("strictly portable"), false), "rejects a trailing occurrence");
        // `/A` (substring) matches the term anywhere in a word.
        assert!(eval(&table, &words_of("strictly portable"), true), "/A finds it inside `portable`");
        // A bare word is word-start, but a quoted PHRASE is exact (see
        // `a_quoted_phrase_is_exact_not_word_start`): `"mark set"` matches `mark set`, not `mark setpoint`.
        let phrase = parse(&lex_query("\"mark set\"")).unwrap();
        assert!(eval(&phrase, &words_of("the mark set here"), false), "exact phrase words");
        assert!(!eval(&phrase, &words_of("the mark setpoint"), false), "phrase words are not prefixes");
        assert!(!eval(&parse(&lex_query("able")).unwrap(), &words_of("a portable table"), false));
    }

    #[test]
    fn boolean_operators_and_phrase_and_near_evaluate() {
        assert!(matches("foo AND bar", "foo bar baz"));
        assert!(!matches("foo AND bar", "foo baz"));
        assert!(matches("foo OR bar", "only bar here"));
        assert!(!matches("foo OR bar", "nothing relevant"));
        assert!(matches("foo NOT bar", "foo baz")); // foo present, bar absent
        assert!(!matches("foo NOT bar", "foo bar"));
        assert!(matches("tab indent", "a tab and some indent")); // implicit AND
        assert!(!matches("tab indent", "only a tab"));
        // Quoted phrase = consecutive words.
        assert!(matches("\"wrap indent\"", "soft wrap indent here"));
        assert!(!matches("\"wrap indent\"", "wrap soft indent"));
        // NEAR/n = within n words (color..background is 2 apart).
        assert!(matches("color NEAR/2 background", "color of background"));
        assert!(!matches("color NEAR/1 background", "color of background"));
        // Grouping + precedence.
        assert!(matches("(a OR z) AND b", "a b c"));
        assert!(!matches("(a OR z) AND b", "a c"));
    }

    #[test]
    fn parens_override_default_precedence_and_nest() {
        // Default precedence: AND binds tighter than OR, so `a OR b AND c` == `a OR (b AND c)`.
        assert!(matches("a OR b AND c", "a only")); // `a` alone satisfies the OR
        assert!(!matches("a OR b AND c", "b only")); // b without c, and no a → no match
        assert!(matches("a OR b AND c", "b c")); // the AND branch
        // Parens regroup it to `(a OR b) AND c`: now `c` is mandatory.
        assert!(!matches("(a OR b) AND c", "a only")); // missing the mandatory c
        assert!(matches("(a OR b) AND c", "a c"));
        assert!(matches("(a OR b) AND c", "b c"));
        // Nested parens: `(a OR (b AND c)) AND d`.
        assert!(matches("(a OR (b AND c)) AND d", "a d")); // left branch of the inner OR + d
        assert!(matches("(a OR (b AND c)) AND d", "b c d")); // right branch (b AND c) + d
        assert!(!matches("(a OR (b AND c)) AND d", "b d")); // inner OR fails (no a, and b-without-c)
        assert!(!matches("(a OR (b AND c)) AND d", "a")); // mandatory outer d missing
    }

    #[test]
    fn a_quoted_phrase_is_exact_not_word_start() {
        // A quoted phrase restricts to the EXACT consecutive words — the bug fix: it must NOT match a
        // window that merely starts with the phrase's words (`statusbar lineup`) or a fused word.
        let p = parse(&lex_query("\"status line\"")).unwrap();
        assert!(eval(&p, &words_of("shown on the status line now"), false), "exact phrase matches");
        assert!(!eval(&p, &words_of("a statusbar lineup"), false), "no word-start prefixes");
        assert!(!eval(&p, &words_of("the statusline widget"), false), "no fused word");
        assert!(!eval(&p, &words_of("status of the line"), false), "words must be consecutive");
        assert!(!eval(&p, &words_of("two status lines"), false), "an inflected last word is not exact");
        // `/A` relaxes a phrase's words to substrings again.
        assert!(eval(&p, &words_of("a statusbar lineup"), true), "/A matches within words");
    }

    #[test]
    fn phrase_highlight_targets_the_phrase_not_stray_words() {
        let (pat, rx) = query_emphasis("\"status line\"", false).unwrap();
        assert!(rx);
        let re = Regex::new(&pat).unwrap();
        assert!(re.is_match("shown on the status line"), "lights up the phrase");
        assert!(!re.is_match("statuspath"), "does not light a lone status-word");
        assert!(!re.is_match("the statusline special"), "does not light a fused word");
        assert!(!re.is_match("a lineup of items"), "does not light a lone line-word");
    }

    #[test]
    fn is_boolean_query_distinguishes_from_plain_and_regex() {
        assert!(!is_boolean_query("indent")); // single word → plain substring
        assert!(!is_boolean_query("^PV")); // regex → stays regex
        assert!(!is_boolean_query("col(ou)?r")); // single regex-ish term
        assert!(is_boolean_query("tab AND indent"));
        assert!(is_boolean_query("tab indent")); // plain multi-word → implicit AND
        assert!(is_boolean_query("\"wrap indent\""));
        assert!(is_boolean_query("color NEAR/3 bg"));
        assert!(is_boolean_query("a OR b"));
    }

    #[test]
    fn query_emphasis_lists_positive_leaves_only() {
        let (p, rx) = query_emphasis("tab AND indent", false).unwrap();
        assert!(rx);
        assert!(p.contains("tab") && p.contains("indent"), "{p}");
        let (p2, _) = query_emphasis("tab NOT indent", false).unwrap();
        assert!(p2.contains("tab") && !p2.contains("indent"), "NOT term excluded: {p2}");
        // A plain single term still uses the whole-word emphasis path.
        assert!(query_emphasis("indent", false).is_some());
    }

    #[test]
    fn search_query_boolean_over_the_real_reference() {
        // WRAPINDENT (name holds wrap+indent, synopsis is about indentation) matches `wrap AND indent`
        // but is excluded by `wrap NOT indent` — a clean differential that doesn't hard-code corpus text.
        let has_wi = |hits: &[HelpEntry]| hits.iter().any(|e| e.names.iter().any(|n| n.eq_ignore_ascii_case("WRAPINDENT")));
        assert!(has_wi(&search_query("wrap AND indent", false)), "wrap AND indent finds WRAPINDENT");
        assert!(!has_wi(&search_query("wrap NOT indent", false)), "wrap NOT indent excludes WRAPINDENT");
    }
}
