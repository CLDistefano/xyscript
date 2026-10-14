//! Syntax-highlighting **seam** (`gfm-track-plan.md` step 8a).
//!
//! Highlighting is not a Markdown-only feature: it extends to many languages via
//! user-authored **regex grammars** (Phase 2). So this module is the **abstraction**
//! — a general [`TokenKind`] (a fixed set of common names plus an open-ended
//! [`TokenKind::Other`] for grammar-defined kinds), a [`theme`] mapping kinds to
//! [`Style`]s, and a [`Highlighter`] that turns source text into byte-ranged
//! [`Token`]s. Markdown is the first implementation (via `comrak`); a
//! [`RegexGrammar`] is the second (flat per-line `pattern → kind` rules).
//!
//! Highlighting is **off by default** (a quiet screen); the editor enables it per
//! buffer (`HIGHLIGHT on`). Tokens are **byte offsets into the document's UTF-8
//! source** — highlighting assumes a UTF-8/ASCII view (the case for Markdown and
//! code; legacy single-byte files are simply not highlighted).

use std::collections::HashMap;

use comrak::nodes::{NodeValue, Sourcepos};
use comrak::{parse_document, Arena, Options};

use crate::style::{Color, Span, Style, StyledLine};

/// A classified span of source, as a half-open **byte range** `[start, end)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub start: usize,
    pub end: usize,
    pub kind: TokenKind,
}

/// The kind a highlighter can emit. The named variants cover Markdown/prose and the
/// common code constructs (so the built-in theme is general from the start);
/// [`TokenKind::Other`] carries any grammar-defined kind name verbatim (decision
/// 2026-06-18: arbitrary string kinds). An `Other` kind that the active theme does
/// not colour simply renders in the editor's `FGCOLOR`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TokenKind {
    // Markdown / prose
    Heading,
    Emphasis,
    Strong,
    Code,
    Link,
    Quote,
    Rule,
    // Code
    Keyword,
    StringLit,
    Comment,
    Number,
    /// A callable name — built-in expression functions in a code grammar (e.g. XPL's `LEN`, `UPPER`).
    Function,
    /// A grammar-defined kind named by the user (lowercased). Not one of the built-ins.
    Other(String),
}

impl TokenKind {
    /// The built-in (named) kinds, in a stable order (config emission, listing).
    pub const ALL: [TokenKind; 12] = [
        TokenKind::Heading,
        TokenKind::Emphasis,
        TokenKind::Strong,
        TokenKind::Code,
        TokenKind::Link,
        TokenKind::Quote,
        TokenKind::Rule,
        TokenKind::Keyword,
        TokenKind::StringLit,
        TokenKind::Comment,
        TokenKind::Number,
        TokenKind::Function,
    ];

    /// The lowercase name used by `HLCOLOR`/`HLRULE` and config emission.
    pub fn name(&self) -> &str {
        match self {
            TokenKind::Heading => "heading",
            TokenKind::Emphasis => "emphasis",
            TokenKind::Strong => "strong",
            TokenKind::Code => "code",
            TokenKind::Link => "link",
            TokenKind::Quote => "quote",
            TokenKind::Rule => "rule",
            TokenKind::Keyword => "keyword",
            TokenKind::StringLit => "string",
            TokenKind::Comment => "comment",
            TokenKind::Number => "number",
            TokenKind::Function => "function",
            TokenKind::Other(s) => s.as_str(),
        }
    }

    /// Map a kind name (case-insensitive) to a `TokenKind`. A known name maps to its
    /// built-in variant (`string`/`stringlit` both → `StringLit`); ANY other name is
    /// accepted as [`TokenKind::Other`] (grammars may name arbitrary kinds).
    pub fn from_name(s: &str) -> TokenKind {
        let k = s.trim().to_ascii_lowercase();
        if k == "stringlit" {
            return TokenKind::StringLit;
        }
        TokenKind::ALL
            .into_iter()
            .find(|t| t.name() == k)
            .unwrap_or(TokenKind::Other(k))
    }
}

/// The built-in default highlight colours (RGB), one per built-in [`TokenKind`].
/// These match the original fixed palette, so first-run appearance is unchanged;
/// `HLCOLOR` overrides them at runtime and `HLRESET` restores them. A kind absent
/// from the active theme renders in the editor's `FGCOLOR`.
pub fn default_highlight_theme() -> HashMap<TokenKind, (u8, u8, u8)> {
    use TokenKind::*;
    [
        (Heading, (30, 150, 160)),
        (Emphasis, (30, 30, 30)),
        (Strong, (30, 30, 30)),
        (Code, (40, 150, 70)),
        (Link, (50, 90, 200)),
        (Quote, (170, 40, 180)),
        (Rule, (170, 130, 20)),
        (Keyword, (170, 40, 180)),
        (StringLit, (40, 150, 70)),
        (Comment, (30, 150, 160)),
        (Number, (200, 60, 60)),
        (Function, (150, 150, 50)),
    ]
    .into_iter()
    .collect()
}

/// A highlight palette tuned to be legible on a DARK canvas — used by `THEME dark`. Same kinds as
/// [`default_highlight_theme`] (the light palette), recoloured brighter/warmer for a dark background.
pub fn dark_highlight_theme() -> HashMap<TokenKind, (u8, u8, u8)> {
    use TokenKind::*;
    [
        (Heading, (95, 200, 215)),
        (Emphasis, (200, 200, 200)),
        (Strong, (235, 235, 235)),
        (Code, (140, 210, 150)),
        (Link, (110, 165, 250)),
        (Quote, (200, 140, 225)),
        (Rule, (220, 190, 100)),
        (Keyword, (215, 130, 220)),
        (StringLit, (225, 175, 120)),
        (Comment, (120, 140, 150)),
        (Number, (240, 150, 135)),
        (Function, (205, 200, 120)),
    ]
    .into_iter()
    .collect()
}

/// Map a token kind to a display style for the **TUI** renderer (the GUI reads RGB
/// from the runtime theme instead). A single built-in theme; grammar-defined
/// (`Other`) kinds have no TUI style and render plain.
pub fn theme(kind: &TokenKind) -> Style {
    let s = Style::default();
    match kind {
        TokenKind::Heading => Style { fg: Color::Cyan, bold: true, ..s },
        TokenKind::Strong => Style { bold: true, ..s },
        TokenKind::Emphasis => Style { italic: true, ..s },
        TokenKind::Code => Style { fg: Color::Green, ..s },
        TokenKind::Link => Style { fg: Color::Blue, underline: true, ..s },
        TokenKind::Quote => Style { fg: Color::Magenta, ..s },
        TokenKind::Rule => Style { fg: Color::Yellow, ..s },
        TokenKind::Keyword => Style { fg: Color::Magenta, bold: true, ..s },
        TokenKind::StringLit => Style { fg: Color::Green, ..s },
        TokenKind::Comment => Style { fg: Color::Cyan, italic: true, ..s },
        TokenKind::Number => Style { fg: Color::Red, ..s },
        TokenKind::Function => Style { fg: Color::Yellow, ..s },
        TokenKind::Other(_) => s, // no built-in TUI style for grammar kinds
    }
}

// ---- regex grammars (Phase 2) ----------------------------------------------

/// The compiled matcher behind one grammar rule. `Fast` is the linear-time `regex`
/// engine (the default); `Perl` is `fancy-regex` (the `/P` switch), which adds
/// look-around, backreferences, and atomic groups — but backtracks, so it is built
/// with a step limit ([`HL_BACKTRACK_LIMIT`]) and a match that trips it is skipped.
#[derive(Debug, Clone)]
pub enum RuleMatcher {
    Fast(regex::Regex),
    Perl(fancy_regex::Regex),
    /// A `/P` rule of the shape `(?<=X)BODY` (positive look-behind, no other look-around or
    /// backreference) rewritten to run on the LINEAR `regex` engine as `(?:X)(BODY)`: the look-behind
    /// content is matched as a discarded prefix, and only the BODY capture group (`body`) is emitted —
    /// so `X` is left unclaimed, exactly as the zero-width look-behind did. This escapes `fancy-regex`'s
    /// backtracking + lack of prefiltering (the heavy cost on a dense `«MD…»` grammar), while matching
    /// the identical spans. See [`transform_lookbehind`]. BODY is a NAMED group ([`LB_BODY_GROUP`]) so
    /// its index survives any capturing groups inside `X` (e.g. an alternation `(IT|RV)`).
    FastLookbehind(regex::Regex),
}

/// Reserved capture-group name for the BODY of a rewritten look-behind rule (see
/// [`RuleMatcher::FastLookbehind`]). Excluded from the named-group → token-kind mapping.
const LB_BODY_GROUP: &str = "xylbbody";

/// One match of a rule in a single line: the whole span plus its named-group
/// sub-spans already resolved to token kinds, so the highlight loop is engine-agnostic.
struct RuleMatch {
    whole: (usize, usize),
    groups: Vec<(usize, usize, TokenKind)>,
}

/// Per-match backtracking budget for a `/P` grammar rule. The highlighter runs on
/// every render, so a catastrophic pattern must fail fast rather than freeze the UI;
/// a `/P` match that exceeds this is dropped (that span just isn't highlighted). Well
/// under `fancy-regex`'s 1,000,000 default, but ample for line-scoped look-around.
const HL_BACKTRACK_LIMIT: usize = 100_000;

impl RuleMatcher {
    /// Every non-overlapping match of this rule in one line. A `/P` match that trips
    /// the backtrack limit (or otherwise errors) is silently skipped via
    /// `filter_map(Result::ok)`, so a pathological rule degrades to "no highlight".
    fn matches(&self, line: &str) -> Vec<RuleMatch> {
        match self {
            RuleMatcher::Fast(re) => re
                .captures_iter(line)
                .map(|caps| {
                    let w = caps.get(0).expect("capture group 0 is the whole match");
                    let groups = re
                        .capture_names()
                        .flatten()
                        .filter_map(|n| {
                            caps.name(n).map(|g| (g.start(), g.end(), TokenKind::from_name(n)))
                        })
                        .collect();
                    RuleMatch { whole: (w.start(), w.end()), groups }
                })
                .collect(),
            RuleMatcher::Perl(re) => re
                .captures_iter(line)
                .filter_map(Result::ok)
                .map(|caps| {
                    let w = caps.get(0).expect("capture group 0 is the whole match");
                    let groups = re
                        .capture_names()
                        .flatten()
                        .filter_map(|n| {
                            caps.name(n).map(|g| (g.start(), g.end(), TokenKind::from_name(n)))
                        })
                        .collect();
                    RuleMatch { whole: (w.start(), w.end()), groups }
                })
                .collect(),
            // The whole MATCH is `X BODY`, but the rule's span is only BODY (the reserved named group) —
            // the `X` prefix stays unclaimed, like the original look-behind. Any USER named groups
            // inside BODY carry absolute offsets, so they resolve exactly as on the other engines; the
            // reserved BODY group is excluded from the kind mapping.
            RuleMatcher::FastLookbehind(re) => re
                .captures_iter(line)
                .filter_map(|caps| {
                    let w = caps.name(LB_BODY_GROUP)?;
                    let groups = re
                        .capture_names()
                        .flatten()
                        .filter(|n| *n != LB_BODY_GROUP)
                        .filter_map(|n| {
                            caps.name(n).map(|g| (g.start(), g.end(), TokenKind::from_name(n)))
                        })
                        .collect();
                    Some(RuleMatch { whole: (w.start(), w.end()), groups })
                })
                .collect(),
        }
    }
}

/// One rule of a [`RegexGrammar`]: spans matching `matcher` are classified as `kind`.
/// `source` keeps the original regex text, and `perl` whether it used `/P`, for config
/// emission.
#[derive(Debug, Clone)]
pub struct GrammarRule {
    pub kind: String,
    pub source: String,
    pub perl: bool,
    /// Required literals: a line can match this rule ONLY if it contains at least one of these
    /// (a cheap `str::contains` guard run before the regex). Empty = no filter (always scan). This is
    /// what makes a lookbehind-heavy `/P` grammar fast: `fancy-regex` has no literal prefiltering of
    /// its own, so without this every `(?<=«MDxx»)…` rule scans every line even when `«MDxx»` is
    /// absent. Extracted conservatively by [`required_literals`] — only ever a genuinely-required
    /// literal, so pruning can never change which spans match.
    pub prefilter: Vec<String>,
    pub matcher: RuleMatcher,
}

/// A flat, line-oriented regex grammar: an ordered list of `pattern → kind` rules,
/// associated with one or more file extensions. Each source line is highlighted
/// independently (NO multi-line constructs). Rules apply in order with
/// **first-match-wins**: once a span is claimed by an earlier rule, a later rule
/// cannot recolour it.
#[derive(Debug, Clone)]
pub struct RegexGrammar {
    pub name: String,
    /// Lowercased file extensions WITHOUT the leading dot (e.g. `["rs"]`).
    pub exts: Vec<String>,
    pub rules: Vec<GrammarRule>,
}

impl RegexGrammar {
    /// Classify `source` into byte-ranged tokens, one line at a time. No tokens
    /// overlap (first-match-wins resolves overlaps as matches are claimed).
    pub fn highlight(&self, source: &str) -> Vec<Token> {
        let mut tokens = Vec::new();
        if self.rules.is_empty() {
            return tokens;
        }
        for (line_start, line) in lines_with_offsets(source) {
            if line.is_empty() {
                continue;
            }
            // Per-line claim map: byte i is true once some rule has taken it.
            let mut claimed = vec![false; line.len()];
            for rule in &self.rules {
                // Literal prefilter: skip the (possibly expensive `/P`) scan when none of the rule's
                // required literals is present — a cheap necessary-condition check. This is the fast
                // path for a lookbehind grammar: a rule whose code isn't on this line costs an O(1)-ish
                // `contains` instead of a full fancy-regex sweep.
                if !rule.prefilter.is_empty() && !rule.prefilter.iter().any(|p| line.contains(p.as_str())) {
                    continue;
                }
                let base = TokenKind::from_name(&rule.kind);
                for m in rule.matcher.matches(line) {
                    let (a, b) = m.whole;
                    if a >= b || claimed[a..b].iter().any(|&c| c) {
                        continue; // empty, or overlaps an earlier (winning) rule
                    }
                    // NAMED capture groups recolour their own sub-span: the group's NAME is the token
                    // kind (`(?P<kind>…)`), coloured by that kind's `HLCOLOR`. Ungrouped parts of the
                    // match keep the rule's own `kind`. A rule with NO named groups colours the whole
                    // match as `kind` — the original behaviour, byte-for-byte. This lets one rule paint
                    // (say) the `«MDBO»`/`«MDNM»` delimiters one colour and the text between another,
                    // with no look-behind/ahead. Groups must not overlap; a group starting inside an
                    // earlier one is skipped so the emitted tokens stay flat and non-overlapping.
                    let mut segs = m.groups;
                    segs.sort_by_key(|s| s.0);
                    let mut push = |s: usize, e: usize, k: TokenKind| {
                        if s < e {
                            tokens.push(Token { start: line_start + s, end: line_start + e, kind: k });
                        }
                    };
                    let mut pos = a;
                    for (gs, ge, gk) in segs {
                        if gs < pos {
                            continue; // overlapping / nested group — keep the token stream flat
                        }
                        push(pos, gs, base.clone()); // ungrouped gap → the rule's own kind
                        push(gs, ge, gk); // the group → its named kind
                        pos = ge;
                    }
                    push(pos, b, base.clone()); // trailing ungrouped tail
                    for c in &mut claimed[a..b] {
                        *c = true;
                    }
                }
            }
        }
        tokens
    }
}

/// The set of user-defined regex grammars, plus the one currently being defined
/// (so `HLRULE` knows where to append). Owned by the host; a document gets a
/// CLONE (snapshot) of the matching grammar when highlighting is (re)enabled.
#[derive(Debug, Clone, Default)]
pub struct GrammarRegistry {
    grammars: Vec<RegexGrammar>,
    /// Index of the grammar `HLLANG` last declared — the target for `HLRULE`.
    current: Option<usize>,
}

impl GrammarRegistry {
    /// `HLLANG <name> <exts>`: create (or RESET, so re-running a grammar script is
    /// idempotent) the named grammar with these extensions, and select it as the
    /// target for subsequent `HLRULE`s. `exts` are lowercased, dots stripped.
    pub fn define(&mut self, name: &str, exts: Vec<String>) {
        let exts: Vec<String> = exts
            .iter()
            .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|e| !e.is_empty())
            .collect();
        if let Some(i) = self.grammars.iter().position(|g| g.name == name) {
            self.grammars[i].exts = exts;
            self.grammars[i].rules.clear();
            self.current = Some(i);
        } else {
            self.grammars.push(RegexGrammar { name: name.to_string(), exts, rules: Vec::new() });
            self.current = Some(self.grammars.len() - 1);
        }
    }

    /// `HLRULE[/P] <kind> <regex>`: append a rule to the current grammar. `perl` (the `/P` switch)
    /// compiles the pattern with the `fancy-regex` engine (look-around, backreferences, atomic groups)
    /// under a backtrack limit; otherwise the linear-time `regex` engine. Errors (naming the `kind`) if
    /// no grammar was declared, or the regex does not compile — the caller raises these as FATAL so a
    /// grammar loaded with `RUN` stops and reports the broken rule rather than dropping it silently.
    pub fn add_rule(&mut self, kind: &str, regex: &str, perl: bool) -> Result<(), String> {
        let i = self
            .current
            .ok_or_else(|| format!("HLRULE {kind}: no current grammar — use HLLANG first"))?;
        // Name the offending rule's kind so a grammar that fails to load says WHICH rule broke. NB a
        // regex capture-group NAME must be `[A-Za-z_][A-Za-z0-9_]*` (no hyphens) — a common trip-up.
        let label = if perl { "HLRULE/P" } else { "HLRULE" };
        let matcher = if perl {
            // A positive look-behind `(?<=X)BODY` with no other look-around/backref can run on the
            // LINEAR engine (rewritten `(?:X)(BODY)`, emitting only BODY) — far faster than fancy-regex
            // on a dense grammar, and identical spans. Fall back to the fancy engine when it doesn't fit
            // (other look-around, a backreference, or the rewrite doesn't compile).
            match transform_lookbehind(regex) {
                Some(fast) => RuleMatcher::FastLookbehind(fast),
                None => fancy_regex::RegexBuilder::new(regex)
                    .backtrack_limit(HL_BACKTRACK_LIMIT)
                    .build()
                    .map(RuleMatcher::Perl)
                    .map_err(|e| format!("{label} {kind}: invalid regular expression: {e}"))?,
            }
        } else {
            regex::Regex::new(regex)
                .map(RuleMatcher::Fast)
                .map_err(|e| format!("{label} {kind}: invalid regular expression: {e}"))?
        };
        self.grammars[i].rules.push(GrammarRule {
            kind: kind.trim().to_ascii_lowercase(),
            source: regex.to_string(),
            perl,
            prefilter: required_literals(regex),
            matcher,
        });
        Ok(())
    }

    /// The grammar whose extension list contains `ext` (lowercased, no dot), if any.
    pub fn for_ext(&self, ext: &str) -> Option<&RegexGrammar> {
        let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
        self.grammars.iter().find(|g| g.exts.iter().any(|e| *e == ext))
    }

    pub fn grammars(&self) -> &[RegexGrammar] {
        &self.grammars
    }
}

/// Rewrite a positive-look-behind `/P` rule `(?<=X)BODY` to run on the LINEAR `regex` engine as
/// `(?:X)(BODY)` (emitting only the BODY capture), when that is provably EQUIVALENT and compiles.
/// Returns the compiled fast regex (BODY = capture group 1) or `None` to keep the fancy engine.
///
/// EQUIVALENCE: `(?<=X)BODY` is zero-width in `X`, so consecutive matches can have an `X` that
/// overlaps the previous match; `(?:X)(BODY)` *consumes* `X`, so it would miss such overlaps (e.g.
/// `(?<=aa)a` on `aaaa` matches twice, but `(?:aa)(a)` once). The transform is only applied when that
/// can't happen: **X's first character is a literal that BODY can never match**, so every `X` begins
/// exactly at the delimiter where the previous BODY stopped — never inside it. (The whole `«MD…»`
/// grammar fits: `X` starts with `«`, and BODY is `[^…«»]+`.) Anything else keeps the fancy engine.
fn transform_lookbehind(pattern: &str) -> Option<regex::Regex> {
    let (x, rest) = split_lookbehind(pattern)?;
    if x.is_empty() || rest.is_empty() {
        return None;
    }
    let first = first_literal_char(x)?; // X's leading char, as a concrete literal
    let excluded = negated_class_only(rest)?; // BODY must be exactly `[^set]+`/`*`
    if !excluded.contains(&first) {
        return None; // BODY could match X's first char → overlap possible → not safe
    }
    // BODY is a NAMED group so its index survives capturing groups inside X (e.g. `(IT|RV)`). The
    // compile also GATES correctness: a look-around or backreference in X/BODY fails here (the `regex`
    // engine rejects them) → `None` → fancy fallback.
    regex::Regex::new(&format!("(?:{x})(?P<{LB_BODY_GROUP}>{rest})")).ok()
}

/// If `pattern` begins with a positive look-behind `(?<=…)`, split it into `(content, rest)`. Finds
/// the `)` that closes the assertion, honoring escapes and character classes (so a `)` inside `[...]`
/// or after `\` doesn't close it early).
fn split_lookbehind(pattern: &str) -> Option<(&str, &str)> {
    let after = pattern.strip_prefix("(?<=")?;
    let bytes = after.as_bytes();
    let (mut i, mut depth, mut in_class) = (0usize, 0usize, false);
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                i += 2;
                continue;
            }
            b'[' if !in_class => in_class = true,
            b']' if in_class => in_class = false,
            b'(' if !in_class => depth += 1,
            b')' if !in_class => {
                if depth == 0 {
                    return Some((&after[..i], &after[i + 1..]));
                }
                depth -= 1;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The first LITERAL character of a regex fragment, or `None` if it starts with a metacharacter /
/// group / class (so we can't name a single concrete char). Decodes a leading escape.
fn first_literal_char(s: &str) -> Option<char> {
    let mut it = s.chars();
    match it.next()? {
        '\\' => match it.next()? {
            'r' => Some('\r'),
            'n' => Some('\n'),
            't' => Some('\t'),
            // An escaped metacharacter is that literal character; a class shorthand (\d …) is not a
            // single literal, so bail.
            c @ ('+' | '-' | '.' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$'
                | '\\' | '/') => Some(c),
            _ => None,
        },
        c @ ('(' | '[' | '.' | '*' | '+' | '?' | '^' | '$' | '|' | '{' | ')' | ']' | '}') => {
            let _ = c;
            None
        }
        c => Some(c),
    }
}

/// If `rest` is EXACTLY a single negated character class with a `+`/`*` quantifier (`[^abc]+`), return
/// the set of excluded characters; otherwise `None`. Requiring the whole BODY to be this shape keeps
/// the equivalence argument simple: every character BODY matches is outside the returned set. Escapes
/// `\r`/`\n`/`\t` and escaped metacharacters are decoded; a range (`a-z`) makes us bail (`None`).
fn negated_class_only(rest: &str) -> Option<std::collections::HashSet<char>> {
    let inner = rest.strip_prefix("[^")?;
    let close = {
        // Find the `]` that ends the class (honoring escapes); there must be exactly one `+`/`*` after.
        let b = inner.as_bytes();
        let (mut i, mut end) = (0usize, None);
        while i < b.len() {
            match b[i] {
                b'\\' => i += 2,
                b']' => {
                    end = Some(i);
                    break;
                }
                _ => i += 1,
            }
        }
        end?
    };
    let quant = &inner[close + 1..];
    if quant != "+" && quant != "*" {
        return None;
    }
    let body = &inner[..close];
    if body.contains('-') {
        return None; // a range — don't try to enumerate; keep the fancy engine
    }
    let mut set = std::collections::HashSet::new();
    let mut it = body.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next()? {
                'r' => set.insert('\r'),
                'n' => set.insert('\n'),
                't' => set.insert('\t'),
                e => set.insert(e),
            };
        } else {
            set.insert(c);
        }
    }
    Some(set)
}

/// Shortest literal worth prefiltering on — below this a literal is too common to prune usefully.
const PREFILTER_MIN: usize = 3;

/// Extract a conservative set of REQUIRED literals from a rule's regex `pattern`: a line can match
/// the rule only if it contains at least ONE of them, so they gate the (sometimes expensive, `/P`
/// look-behind) scan with a cheap `str::contains`. Empty = nothing provably required → always scan
/// (today's behaviour). CORRECTNESS: every returned literal must appear in the line of ANY match, so
/// we only ever emit a genuinely-required literal and bail (empty) on anything we can't prove —
/// pruning can therefore never drop a span that should have matched.
fn required_literals(pattern: &str) -> Vec<String> {
    // The look-behind / look-ahead whose content is required gates most of these rules; peel one
    // positive assertion and read its literal prefix. A negative assertion (content must be ABSENT)
    // or any other leading group: don't try.
    let body = if let Some(r) = pattern.strip_prefix("(?<=") {
        r
    } else if let Some(r) = pattern.strip_prefix("(?=") {
        r
    } else if pattern.starts_with('(') {
        return Vec::new();
    } else {
        pattern
    };
    let mut prefix = String::new();
    let mut chars = body.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            '\\' => {
                chars.next();
                match chars.next() {
                    Some('d') => return expand_suffixes(&prefix, "0123456789".chars()),
                    // An escaped metacharacter is a literal character.
                    Some(e @ ('+' | '-' | '.' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|'
                        | '^' | '$' | '\\' | '/')) => prefix.push(e),
                    _ => break, // \w \s \r \n \xHH … — stop; the prefix so far is required
                }
            }
            '(' => return expand_alternation(&prefix, &mut chars),
            '|' => return Vec::new(), // a top-level alternation — the prefix isn't required
            '.' | '*' | '+' | '?' | '[' | '{' | '^' | '$' | ')' => break,
            _ => {
                prefix.push(c);
                chars.next();
            }
        }
    }
    one_literal(prefix)
}

/// A single-literal prefilter, kept only when it's long enough to be selective.
fn one_literal(prefix: String) -> Vec<String> {
    if prefix.chars().count() >= PREFILTER_MIN {
        vec![prefix]
    } else {
        Vec::new()
    }
}

/// `prefix × {each suffix}` as a required-literal set (for a `\d`-style single class right after a
/// literal prefix). Dropped unless every literal clears [`PREFILTER_MIN`].
fn expand_suffixes(prefix: &str, suffixes: impl Iterator<Item = char>) -> Vec<String> {
    let set: Vec<String> = suffixes.map(|s| format!("{prefix}{s}")).collect();
    if !set.is_empty() && set.iter().all(|l| l.chars().count() >= PREFILTER_MIN) {
        set
    } else {
        Vec::new()
    }
}

/// Try to expand a pure-literal alternation group — `(a|b|c)` or `(?:a|b)` — immediately after
/// `prefix` into `prefix × {a,b,c}`. Any non-literal branch (or a nested construct) falls back to the
/// prefix alone (still a required literal).
fn expand_alternation(prefix: &str, chars: &mut std::iter::Peekable<std::str::Chars>) -> Vec<String> {
    chars.next(); // consume '('
    if chars.peek() == Some(&'?') {
        chars.next();
        if chars.next() != Some(':') {
            return one_literal(prefix.to_string()); // (?<= / (?= / named group — don't try
        }
    }
    let mut branches = vec![String::new()];
    for c in chars.by_ref() {
        match c {
            ')' => {
                let set: Vec<String> = branches.iter().map(|b| format!("{prefix}{b}")).collect();
                if set.iter().all(|l| l.chars().count() >= PREFILTER_MIN) {
                    return set;
                }
                return one_literal(prefix.to_string());
            }
            '|' => branches.push(String::new()),
            // A metacharacter inside a branch → not a pure-literal alternation; use the prefix alone.
            '\\' | '(' | '[' | '.' | '*' | '+' | '?' | '{' | '^' | '$' => {
                return one_literal(prefix.to_string())
            }
            _ => branches.last_mut().unwrap().push(c),
        }
    }
    one_literal(prefix.to_string()) // unterminated (shouldn't happen for a valid regex)
}

/// Iterate `(byte_offset_of_line_start, line_text_without_eol)` over `source`.
fn lines_with_offsets(source: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = source.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            // Trim a trailing CR so `$` anchors and matches exclude the EOL.
            let mut end = i;
            if end > start && bytes[end - 1] == b'\r' {
                end -= 1;
            }
            out.push((start, &source[start..end]));
            start = i + 1;
        }
    }
    if start <= source.len() {
        out.push((start, &source[start..]));
    }
    out
}

/// The active highlighter for a buffer. A closed enum (no trait objects): Markdown
/// (comrak) or a user-defined regex grammar (a self-contained snapshot).
#[derive(Debug, Clone)]
pub enum Highlighter {
    Markdown,
    Regex(RegexGrammar),
}

impl Highlighter {
    /// The BUILT-IN highlighter for a file path's extension, if any. (Grammar
    /// resolution is done by the host, which owns the registry; this is the
    /// fallback for types no grammar claims.)
    pub fn for_path(path: Option<&std::path::Path>) -> Option<Highlighter> {
        let ext = path?.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "md" | "markdown" | "mkd" | "mdown" => Some(Highlighter::Markdown),
            _ => None,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Highlighter::Markdown => "markdown",
            Highlighter::Regex(g) => &g.name,
        }
    }

    /// Classify `source` into byte-ranged tokens.
    pub fn highlight(&self, source: &str) -> Vec<Token> {
        match self {
            Highlighter::Markdown => highlight_markdown(source),
            Highlighter::Regex(g) => g.highlight(source),
        }
    }

}

// ---- Markdown (comrak) -----------------------------------------------------

fn highlight_markdown(source: &str) -> Vec<Token> {
    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    let root = parse_document(&arena, source, &options);

    let line_starts = line_start_offsets(source);
    let mut tokens = Vec::new();
    for node in root.descendants() {
        let data = node.data.borrow();
        let kind = match &data.value {
            NodeValue::Heading(_) => TokenKind::Heading,
            NodeValue::Strong => TokenKind::Strong,
            NodeValue::Emph => TokenKind::Emphasis,
            NodeValue::Code(_) | NodeValue::CodeBlock(_) => TokenKind::Code,
            NodeValue::Link(_) | NodeValue::Image(_) => TokenKind::Link,
            NodeValue::BlockQuote => TokenKind::Quote,
            NodeValue::ThematicBreak => TokenKind::Rule,
            _ => continue,
        };
        if let Some((start, end)) = sourcepos_to_bytes(data.sourcepos, &line_starts, source.len()) {
            if start < end {
                tokens.push(Token { start, end, kind });
            }
        }
    }
    tokens
}

/// Byte offset of the start of each (0-based) source line.
pub(crate) fn line_start_offsets(s: &str) -> Vec<usize> {
    let mut v = vec![0];
    for (i, b) in s.bytes().enumerate() {
        if b == b'\n' {
            v.push(i + 1);
        }
    }
    v
}

/// Convert a comrak [`Sourcepos`] (1-based line/byte-column, inclusive end) to a
/// half-open byte range `[start, end)` into the source.
pub(crate) fn sourcepos_to_bytes(sp: Sourcepos, line_starts: &[usize], len: usize) -> Option<(usize, usize)> {
    if sp.start.line == 0 || sp.start.column == 0 || sp.end.line == 0 || sp.end.column == 0 {
        return None; // synthetic / unknown position
    }
    let start = line_starts.get(sp.start.line - 1)? + (sp.start.column - 1);
    let end = line_starts.get(sp.end.line - 1)? + sp.end.column; // inclusive col -> exclusive
    Some((start.min(len), end.min(len)))
}

// ---- turning tokens into styled spans (pure; used by the viewport) ----------

/// Build the styled spans for one line: `text` is the decoded line content (UTF-8),
/// `line_start` its byte offset in the document, `tokens` the document's tokens.
/// Overlapping tokens are resolved last-wins (Markdown emits pre-order, so the
/// innermost construct wins; regex grammars never emit overlaps). A run with no
/// token uses the plain (default) style.
pub fn styled_line(text: &str, line_start: usize, tokens: &[Token]) -> StyledLine {
    if text.is_empty() {
        return Vec::new();
    }
    let n = text.len();
    let mut kinds: Vec<Option<&TokenKind>> = vec![None; n];
    for t in tokens {
        let a = t.start.max(line_start);
        let b = t.end.min(line_start + n);
        if a < b {
            for slot in &mut kinds[(a - line_start)..(b - line_start)] {
                *slot = Some(&t.kind);
            }
        }
    }
    let mut spans: StyledLine = Vec::new();
    let mut cur_kind: Option<&TokenKind> = kinds[0];
    let mut cur = String::new();
    for (bi, ch) in text.char_indices() {
        let k = kinds[bi];
        if k != cur_kind && !cur.is_empty() {
            spans.push(make_span(std::mem::take(&mut cur), cur_kind));
        }
        cur_kind = k;
        cur.push(ch);
    }
    if !cur.is_empty() {
        spans.push(make_span(cur, cur_kind));
    }
    spans
}

fn make_span(text: String, kind: Option<&TokenKind>) -> Span {
    match kind {
        Some(k) => Span::styled(text, theme(k)),
        None => Span::plain(text),
    }
}

/// Horizontally clip a styled line to the character window `[left, left + width)`,
/// preserving each surviving span's style.
pub fn clip_styled(line: &StyledLine, left: usize, width: usize) -> StyledLine {
    let mut out = StyledLine::new();
    let vis_end = left.saturating_add(width);
    let mut col = 0; // characters consumed so far
    for span in line {
        let chars: Vec<char> = span.text.chars().collect();
        let span_start = col;
        let span_end = col + chars.len();
        col = span_end;
        let a = span_start.max(left);
        let b = span_end.min(vis_end);
        if a < b {
            let s: String = chars[(a - span_start)..(b - span_start)].iter().collect();
            out.push(Span::styled(s, span.style));
        }
        if col >= vis_end {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The source slice a token covers (for offset-exact assertions).
    fn slice<'a>(src: &'a str, t: &Token) -> &'a str {
        &src[t.start..t.end]
    }
    fn first(tokens: &[Token], kind: TokenKind) -> Option<Token> {
        tokens.iter().find(|t| t.kind == kind).cloned()
    }

    #[test]
    fn markdown_tokens_have_exact_byte_ranges() {
        let src = "# Title\n\nA **bold** and `code` and *em* word.\n";
        let toks = Highlighter::Markdown.highlight(src);
        assert_eq!(slice(src, &first(&toks, TokenKind::Heading).unwrap()), "# Title");
        assert_eq!(slice(src, &first(&toks, TokenKind::Strong).unwrap()), "**bold**");
        assert_eq!(slice(src, &first(&toks, TokenKind::Code).unwrap()), "`code`");
        assert_eq!(slice(src, &first(&toks, TokenKind::Emphasis).unwrap()), "*em*");
    }

    #[test]
    fn fenced_code_block_and_link_and_rule() {
        let src = "see [docs](http://x)\n\n```\nlet x = 1;\n```\n\n---\n";
        let toks = Highlighter::Markdown.highlight(src);
        assert_eq!(slice(src, &first(&toks, TokenKind::Link).unwrap()), "[docs](http://x)");
        let code = first(&toks, TokenKind::Code).unwrap();
        assert!(slice(src, &code).starts_with("```"));
        assert!(slice(src, &code).contains("let x = 1;"));
        assert_eq!(slice(src, &first(&toks, TokenKind::Rule).unwrap()), "---");
    }

    #[test]
    fn for_path_selects_markdown_by_extension() {
        use std::path::Path;
        assert_eq!(Highlighter::for_path(Some(Path::new("a.md"))).map(|h| h.name().to_string()), Some("markdown".into()));
        assert_eq!(Highlighter::for_path(Some(Path::new("a.MARKDOWN"))).map(|h| h.name().to_string()), Some("markdown".into()));
        assert!(Highlighter::for_path(Some(Path::new("a.rs"))).is_none());
        assert!(Highlighter::for_path(None).is_none());
    }

    #[test]
    fn styled_line_splits_at_token_boundaries() {
        let line = "A **bold** x";
        let toks = vec![Token { start: 2, end: 10, kind: TokenKind::Strong }];
        let spans = styled_line(line, 0, &toks);
        let texts: Vec<&str> = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["A ", "**bold**", " x"]);
        assert!(spans[0].style.is_plain());
        assert!(spans[1].style.bold);
        assert!(spans[2].style.is_plain());
    }

    #[test]
    fn styled_line_handles_multibyte_runs() {
        let line = "café!";
        let toks = vec![Token { start: 0, end: 5, kind: TokenKind::Code }];
        let spans = styled_line(line, 0, &toks);
        assert_eq!(spans.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), vec!["café", "!"]);
        assert_eq!(spans[0].style.fg, Color::Green);
    }

    #[test]
    fn clip_styled_preserves_styles_across_the_window() {
        let line = vec![
            Span::plain("A "),
            Span::styled("**bold**", theme(&TokenKind::Strong)),
            Span::plain(" x"),
        ];
        let clipped = clip_styled(&line, 2, 8);
        assert_eq!(clipped.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), vec!["**bold**"]);
        assert!(clipped[0].style.bold);
        let clipped2 = clip_styled(&line, 0, 4); // "A **"
        assert_eq!(clipped2.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), vec!["A ", "**"]);
    }

    // ---- regex grammars ----

    fn grammar(rules: &[(&str, &str)], exts: &[&str]) -> RegexGrammar {
        let mut reg = GrammarRegistry::default();
        reg.define("test", exts.iter().map(|e| e.to_string()).collect());
        for (kind, re) in rules {
            reg.add_rule(kind, re, false).unwrap();
        }
        reg.for_ext(exts[0]).unwrap().clone()
    }

    /// Build a single-rule grammar with the `/P` (fancy-regex) engine.
    fn perl_grammar(kind: &str, re: &str, ext: &str) -> RegexGrammar {
        let mut reg = GrammarRegistry::default();
        reg.define("test", vec![ext.to_string()]);
        reg.add_rule(kind, re, true).unwrap();
        reg.for_ext(ext).unwrap().clone()
    }

    #[test]
    fn required_literals_are_conservative_and_selective() {
        // Look-behind literal → required (read through to the metachar, so the full «MDBO»).
        assert_eq!(required_literals(r"(?<=«MDBO»)[^\r\n«»]+"), vec!["«MDBO»".to_string()]);
        // Pure-literal alternation → expanded.
        assert_eq!(
            required_literals(r"(?<=«MD(IT|RV)»)[^\r\n«»]+"),
            vec!["«MDIT".to_string(), "«MDRV".to_string()]
        );
        // Wildcard after a literal → the literal prefix (still selective).
        assert_eq!(required_literals(r"(?<=«MDI.»)[^\r\n«»]+"), vec!["«MDI".to_string()]);
        // Escaped metachar is literal.
        assert_eq!(required_literals(r"(?<=«MD\+..»)[^\r\n«»]+"), vec!["«MD+".to_string()]);
        // `\d` after a literal → the ten digit literals.
        let d = required_literals(r"(?<=«MD\d»)[^\r\n«»]+");
        assert_eq!(d.len(), 10);
        assert!(d.contains(&"«MD0".to_string()) && d.contains(&"«MD9".to_string()));
        // A plain (non-lookaround) literal prefix works too.
        assert_eq!(required_literals(r"«LB[^«»]+»"), vec!["«LB".to_string()]);
        // CONSERVATIVE bail-outs → empty (always scan): a too-short prefix, a leading group, a
        // leading negative assertion, a top-level alternation.
        assert!(required_literals(r"«\w[^«»]+»").is_empty(), "only «, too short");
        assert!(required_literals(r"(^|\s);\*;").is_empty(), "leading group");
        assert!(required_literals(r"(?<!x)foo").is_empty(), "negative look-behind");
        assert!(required_literals(r"foo|bar").is_empty(), "top-level alternation");
    }

    #[test]
    fn lookbehind_transform_matches_the_fancy_engine_exactly() {
        // Every real xywrite /P rule must transform, and the linear matcher must produce the IDENTICAL
        // spans as the fancy engine — including tricky adjacency (empty body between codes, repeated
        // codes back-to-back, a code at end of line).
        let pats = [
            r"(?<=«MDBO»)[^\r\n«»]+",
            r"(?<=«MD(IT|RV)»)[^\r\n«»]+",
            r"(?<=«MDNM»)[^\r\n«»]+",
            r"(?<=«MD\d»)[^\r\n«»]+",
            r"(?<=«MDI.»)[^\r\n«»]+",
            r"(?<=«MD\+..»)[^\r\n«»]+",
        ];
        let lines = [
            "«MDBO»bold«MDNM» plain «MDIT»ital«MDNM»",
            "«MDBO»«MDNM»empty-between",
            "«MDBO»a«MDBO»b«MDBO»c",
            "no codes here at all",
            "«MD3»sized «MD+01»up «MDI9»idx",
            "trailing«MDBO»",
            "",
        ];
        for p in pats {
            let fast = transform_lookbehind(p).unwrap_or_else(|| panic!("should transform: {p}"));
            let fancy = fancy_regex::RegexBuilder::new(p)
                .backtrack_limit(HL_BACKTRACK_LIMIT)
                .build()
                .unwrap();
            let fm = RuleMatcher::FastLookbehind(fast);
            let pm = RuleMatcher::Perl(fancy);
            for line in lines {
                let a: Vec<_> = fm.matches(line).into_iter().map(|m| m.whole).collect();
                let b: Vec<_> = pm.matches(line).into_iter().map(|m| m.whole).collect();
                assert_eq!(a, b, "transformed != fancy for {p:?} on {line:?}");
            }
        }
    }

    #[test]
    fn lookbehind_transform_bails_when_unsafe() {
        // X's first char CAN be matched by BODY → consuming X would miss overlaps → must NOT transform.
        assert!(transform_lookbehind(r"(?<=aa)a").is_none(), "overlap-unsafe");
        // BODY isn't a single negated class → can't prove the no-overlap property → bail.
        assert!(transform_lookbehind(r"(?<=«MDBO»)\w+").is_none(), "non-class body");
        // A nested look-around / backreference → the linear engine can't compile it → bail.
        assert!(transform_lookbehind(r"(?<=«MD»(?=x))[^«»]+").is_none(), "nested lookaround");
        assert!(transform_lookbehind(r"(?<=(\w)\1)[^«»]+").is_none(), "backreference");
        // Not a look-behind at all.
        assert!(transform_lookbehind(r"«MD[^«»]+»").is_none(), "no lookbehind");
    }

    #[test]
    fn prefilter_never_changes_highlighting() {
        // The literal prefilter is an optimization only: highlighting with it must equal highlighting
        // with it disabled, for lines that do and don't contain each code.
        let mut reg = GrammarRegistry::default();
        reg.define("xy", vec!["xy".into()]);
        for (k, re) in [
            ("bo", r"(?<=«MDBO»)[^\r\n«»]+"),
            ("it", r"(?<=«MD(IT|RV)»)[^\r\n«»]+"),
            ("nm", r"(?<=«MDNM»)[^\r\n«»]+"),
            ("num", r"(?<=«MD\d»)[^\r\n«»]+"),
            ("ix", r"(?<=«MDI.»)[^\r\n«»]+"),
        ] {
            reg.add_rule(k, re, true).unwrap();
        }
        let g = reg.for_ext("xy").unwrap().clone();
        assert!(!g.rules[0].prefilter.is_empty(), "a /P rule got a prefilter");
        // A copy with prefilters disabled = the pre-optimization behaviour.
        let mut naive = g.clone();
        for r in &mut naive.rules {
            r.prefilter.clear();
        }
        for src in [
            "«MDBO»bold«MDNM» plain «MDIT»ital«MDNM»\n",
            "«MD3»sized«MDNM»\n",
            "«MDRV»rev«MDNM» «MDI5»idx«MDNM»\n",
            "no codes here at all\n",
            "\n",
        ] {
            assert_eq!(g.highlight(src), naive.highlight(src), "prefilter changed result for {src:?}");
        }
    }

    #[test]
    fn perl_rule_highlights_with_lookaround_and_named_groups() {
        // Look-ahead: colour "foo" only where "bar" follows — impossible with the linear engine.
        let g = perl_grammar("keyword", r"foo(?=bar)", "t");
        let src = "foobar foobaz\n";
        let toks = g.highlight(src);
        let kw: Vec<_> = toks.iter().filter(|t| t.kind == TokenKind::Keyword).collect();
        assert_eq!(kw.len(), 1);
        assert_eq!(slice(src, kw[0]), "foo"); // the one before "bar", not "foobaz"

        // Named groups still recolour sub-spans under /P (delimiters vs enclosed text), and a
        // backreference in the pattern (fancy-regex only) drives the match.
        let g = perl_grammar("delim", r"(?P<delim>\W)(?P<inner>\w+)\k<delim>", "t");
        let src = "|word| next\n";
        let toks = g.highlight(src);
        let inner = toks.iter().find(|t| t.kind == TokenKind::Other("inner".into())).unwrap();
        assert_eq!(slice(src, inner), "word");
        // The matched delimiters keep the rule's own "delim" kind.
        assert!(toks.iter().any(|t| t.kind == TokenKind::Other("delim".into())));
    }

    #[test]
    fn perl_rule_is_flagged_for_config_emission() {
        let g = perl_grammar("k", r"a(?=b)", "t");
        assert!(g.rules[0].perl, "a /P rule must remember it, so SaveCfg re-emits HLRULE/P");
        let g = grammar(&[("k", "a")], &["t"]);
        assert!(!g.rules[0].perl);
    }

    #[test]
    fn regex_grammar_tokenizes_per_line_with_byte_ranges() {
        let g = grammar(
            &[("keyword", r"\b(let|fn)\b"), ("number", r"\b\d+\b")],
            &["rs"],
        );
        let src = "let x = 12;\nfn g() {}\n";
        let toks = g.highlight(src);
        // keyword "let" at 0..3, number "12" at 8..10, keyword "fn" on line 2.
        assert_eq!(slice(src, &first(&toks, TokenKind::Keyword).unwrap()), "let");
        assert_eq!(slice(src, &first(&toks, TokenKind::Number).unwrap()), "12");
        let fns: Vec<_> = toks.iter().filter(|t| t.kind == TokenKind::Keyword).collect();
        assert_eq!(fns.len(), 2);
        assert_eq!(slice(src, fns[1]), "fn");
    }

    #[test]
    fn regex_grammar_supports_arbitrary_other_kinds() {
        let g = grammar(&[("macro", r"\w+!")], &["rs"]);
        let toks = g.highlight("println!\n");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].kind, TokenKind::Other("macro".into()));
        assert_eq!(toks[0].kind.name(), "macro");
    }

    #[test]
    fn regex_grammar_first_rule_wins_on_overlap() {
        // "comment" claims the whole "// let x" first; the later keyword rule must
        // NOT recolour "let" inside it.
        let g = grammar(
            &[("comment", r"//.*$"), ("keyword", r"\blet\b")],
            &["rs"],
        );
        let toks = g.highlight("// let x\nlet y\n");
        // Line 1: only the comment token (keyword inside is suppressed).
        let line1: Vec<_> = toks.iter().filter(|t| t.start < 9).collect();
        assert_eq!(line1.len(), 1);
        assert_eq!(line1[0].kind, TokenKind::Comment); // "comment" is a built-in kind
        // Line 2: the keyword fires (no comment claims it).
        assert_eq!(slice("// let x\nlet y\n", &first(&toks, TokenKind::Keyword).unwrap()), "let");
    }

    #[test]
    fn from_name_maps_known_and_other() {
        assert_eq!(TokenKind::from_name("Keyword"), TokenKind::Keyword);
        assert_eq!(TokenKind::from_name("stringlit"), TokenKind::StringLit);
        assert_eq!(TokenKind::from_name("string"), TokenKind::StringLit);
        assert_eq!(TokenKind::from_name("TYPE"), TokenKind::Other("type".into()));
    }

    #[test]
    fn named_capture_groups_recolour_their_span() {
        // The XyWrite case: `«MDBO»This is boldface text.«MDNM»` — the two `«MD…»` codes take the
        // rule's own kind (xymode), the text BETWEEN them takes the group's kind (xytext). One rule,
        // no look-behind/ahead.
        let g = grammar(&[("xymode", "«MD[^«»]+»(?P<xytext>[^«]*)«MDNM»")], &["xy"]);
        let src = "«MDBO»This is boldface text.«MDNM»\n";
        let toks = g.highlight(src);
        // Three flat tokens: code, text, code.
        assert_eq!(toks.len(), 3, "expected code/text/code, got {toks:?}");
        assert_eq!((slice(src, &toks[0]), &toks[0].kind), ("«MDBO»", &TokenKind::Other("xymode".into())));
        assert_eq!(
            (slice(src, &toks[1]), &toks[1].kind),
            ("This is boldface text.", &TokenKind::Other("xytext".into()))
        );
        assert_eq!((slice(src, &toks[2]), &toks[2].kind), ("«MDNM»", &TokenKind::Other("xymode".into())));
        // Tokens are flat (non-overlapping) and cover the whole span contiguously.
        assert!(toks[0].end == toks[1].start && toks[1].end == toks[2].start);
    }

    #[test]
    fn a_rule_without_groups_is_unchanged() {
        // No named groups → the whole match takes the rule's kind, exactly as before.
        let g = grammar(&[("xymode", "«MD[^«»]+»")], &["xy"]);
        let toks = g.highlight("«MDBO»x«MDNM»\n");
        assert_eq!(toks.len(), 2);
        assert!(toks.iter().all(|t| t.kind == TokenKind::Other("xymode".into())));
    }
}
