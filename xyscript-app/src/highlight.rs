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

/// One rule of a [`RegexGrammar`]: spans matching `re` are classified as `kind`.
/// `source` keeps the original regex text for config emission.
#[derive(Debug, Clone)]
pub struct GrammarRule {
    pub kind: String,
    pub source: String,
    pub re: regex::Regex,
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
                let base = TokenKind::from_name(&rule.kind);
                for caps in rule.re.captures_iter(line) {
                    let whole = caps.get(0).expect("capture group 0 is the whole match");
                    let (a, b) = (whole.start(), whole.end());
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
                    let mut segs: Vec<(usize, usize, TokenKind)> = Vec::new();
                    for name in rule.re.capture_names().flatten() {
                        if let Some(g) = caps.name(name) {
                            segs.push((g.start(), g.end(), TokenKind::from_name(name)));
                        }
                    }
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

    /// `HLRULE <kind> <regex>`: append a rule to the current grammar. Errors if no
    /// grammar was declared, or the regex does not compile (both trappable).
    pub fn add_rule(&mut self, kind: &str, regex: &str) -> Result<(), String> {
        let i = self
            .current
            .ok_or_else(|| "HLRULE: no current grammar — use HLLANG first".to_string())?;
        let re = regex::Regex::new(regex).map_err(|e| format!("HLRULE: invalid regex: {e}"))?;
        self.grammars[i].rules.push(GrammarRule {
            kind: kind.trim().to_ascii_lowercase(),
            source: regex.to_string(),
            re,
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
            reg.add_rule(kind, re).unwrap();
        }
        reg.for_ext(exts[0]).unwrap().clone()
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
