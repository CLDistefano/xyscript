//! GFM → typst markup (`gfm-track-plan.md` step 8b).
//!
//! The single intermediate representation for rendered output is **typst markup**
//! (decision G3): [`to_typst`] turns a GFM document into a typst *body* string,
//! which 8c compiles to PDF (and 8d will render to SVG/raster for the GUI). We
//! drive the transform from the comrak AST already used for highlighting (G1).
//!
//! Two correctness concerns dominate:
//! - **Escaping.** Text content must never let typst metacharacters through —
//!   especially `#` (code) and `$` (math), which would otherwise *inject* typst.
//!   [`escape_text`] backslash-escapes the markup-significant set; string literals
//!   (URLs, code) go through [`typst_str`].
//! - **Robust constructs.** We emit *function-form* markup (`#emph[…]`,
//!   `#strong[…]`, `#raw(…)`, `#list(…)`, `#table(…)`) rather than the
//!   indentation/line-start-sensitive shorthand, so generated output is
//!   well-formed regardless of content.
//!
//! Scope is GFM's element set. A raw HTML **block** is handed to [`crate::html`],
//! the same structural converter `DOCTYPE html` uses, so an HTML table or figure
//! embedded in Markdown renders. Known first-cut gaps (refine later): task-list
//! checkboxes are rendered as plain items; images render as a link to their src
//! (typst `#image` needs a readable path); INLINE raw HTML is still escaped text,
//! because comrak delivers it as separate open/close fragments.

use crate::typst_esc::{escape_text, typst_str};
use comrak::nodes::{AstNode, ListType, NodeValue};
use comrak::{parse_document, Arena, Options};
use typst::syntax::{SyntaxKind, SyntaxNode};

type ColorSpan = (usize, usize, (u8, u8, u8));
type AttrSpan = (usize, usize, u8);

/// Overlay of the document's `MDA` styling (foreground/background colours + text attributes) for the
/// preview/PDF pre-pass: as `to_typst` walks the comrak AST, each prose TEXT node's source byte range
/// is intersected with these spans and the covered sub-runs are wrapped in the matching Typst markup
/// (`#strong` / `#emph` / `#underline` / `#text(fill:)` / `#highlight(fill:)` / reverse). This lets the
/// on-screen `MDA` styling print, without it being part of the Markdown source. Offsets are the same
/// rope-byte offsets the editor stores (== source byte offsets for a UTF-8 document).
pub struct Styler {
    line_starts: Vec<usize>,
    fg: Vec<ColorSpan>,
    bg: Vec<ColorSpan>,
    attr: Vec<AttrSpan>,
    def_fg: (u8, u8, u8), // the document's FGCOLOR/BGCOLOR — used to render `RV` reverse video
    def_bg: (u8, u8, u8),
    ul_spaces: bool, // ULSPACES: whether an underline runs through inter-word blanks (else it breaks)
}

impl Styler {
    pub fn new(
        source: &str,
        fg: Vec<ColorSpan>,
        bg: Vec<ColorSpan>,
        attr: Vec<AttrSpan>,
        def_fg: (u8, u8, u8),
        def_bg: (u8, u8, u8),
        ul_spaces: bool,
    ) -> Self {
        Styler {
            line_starts: crate::highlight::line_start_offsets(source),
            fg,
            bg,
            attr,
            def_fg,
            def_bg,
            ul_spaces,
        }
    }

    /// Whether there is any styling at all (empty → the walk behaves exactly like plain `to_typst`).
    fn is_empty(&self) -> bool {
        self.fg.is_empty() && self.bg.is_empty() && self.attr.is_empty()
    }

    /// The payload of the span covering byte offset `at` in a sorted, non-overlapping list.
    fn cover<T: Copy>(list: &[(usize, usize, T)], at: usize) -> Option<T> {
        list.iter().find(|&&(s, e, _)| s <= at && at < e).map(|&(_, _, v)| v)
    }

    /// Emit `literal` (prose whose content starts at source byte `src_start`) with any MDA styling that
    /// covers it spliced in as Typst wrappers. `escape` runs each sub-run through [`escape_text`] (the
    /// GFM path, whose text is Markdown prose); the authored-Typst path passes `false` because the text
    /// is already Typst markup (its own [`escape_text`] would double-escape). Falls back to plain text
    /// where nothing covers it.
    fn emit_styled(&self, src_start: usize, literal: &str, escape: bool, out: &mut String) {
        let end = src_start + literal.len();
        // Boundaries inside [src_start, end): every span edge from all three layers, plus the ends.
        let mut cuts = vec![src_start, end];
        for &(s, e, _) in self.fg.iter().chain(self.bg.iter()) {
            if s > src_start && s < end { cuts.push(s); }
            if e > src_start && e < end { cuts.push(e); }
        }
        for &(s, e, _) in &self.attr {
            if s > src_start && s < end { cuts.push(s); }
            if e > src_start && e < end { cuts.push(e); }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let raw = &literal[a - src_start..b - src_start];
            let piece = if escape { escape_text(raw) } else { raw.to_string() };
            let attr = Self::cover(&self.attr, a).unwrap_or(0);
            // Underline is INNERMOST so `ULSPACES off` can break it at inter-word blanks while the
            // weight / colour wrappers still cover the whole piece (blanks included). `ULSPACES on`
            // underlines the piece whole.
            let mut s = if attr & xyscript_xpl::ATTR_UNDERLINE != 0 {
                if self.ul_spaces {
                    format!("#underline[{piece}]")
                } else {
                    underline_words(&piece)
                }
            } else {
                piece
            };
            if attr & xyscript_xpl::ATTR_BOLD != 0 {
                s = format!("#strong[{s}]");
            }
            if attr & xyscript_xpl::ATTR_ITALIC != 0 {
                s = format!("#emph[{s}]");
            }
            // Reverse video overrides the colour layers (it defines its own fg/bg from the doc colours).
            if attr & xyscript_xpl::ATTR_REVERSE != 0 {
                s = format!("#highlight(fill: {})[#text(fill: {})[{s}]]", rgb(self.def_fg), rgb(self.def_bg));
            } else {
                if let Some(c) = Self::cover(&self.fg, a) {
                    s = format!("#text(fill: {})[{s}]", rgb(c));
                }
                if let Some(c) = Self::cover(&self.bg, a) {
                    s = format!("#highlight(fill: {})[{s}]", rgb(c));
                }
            }
            out.push_str(&s);
        }
    }

    /// Splice this document's MDA styling into an **authored-Typst** source. Parse it into Typst's
    /// lossless syntax tree, reconstruct it verbatim, and wrap each maximal run of markup prose
    /// (`Text`/`Space` leaves) that MDA covers in the matching Typst markup — the authored-Typst
    /// analogue of [`to_typst_styled`]. Only prose leaves are wrapped, so headings, emphasis/strong
    /// markers, code, math and every other construct pass through untouched; a source that fails to
    /// parse cleanly still round-trips (its error nodes are leaves emitted verbatim), so the compiler
    /// reports the same diagnostics it would without styling. No newlines are added, so compile-error
    /// LINE numbers still map to the user's buffer (columns on a styled line may shift). An empty styler
    /// returns the source unchanged.
    pub fn splice_typst_source(&self, source: &str) -> String {
        if self.is_empty() {
            return source.to_string();
        }
        let root = typst::syntax::parse(source);
        let mut out = String::new();
        let mut pos = 0usize;
        let mut run: Option<(usize, usize)> = None; // byte range of the prose run being accumulated
        self.walk_typst(&root, source, &mut pos, &mut run, &mut out);
        if let Some((s, e)) = run {
            self.emit_styled(s, &source[s..e], false, &mut out);
        }
        out
    }

    /// Depth-first walk of the Typst CST. Consecutive `Text`/`Space` leaves accumulate into one prose
    /// `run` (so a styled multi-word phrase is wrapped once, preserving inter-word spacing and line
    /// breaking); any other leaf flushes the pending run (styled) and is emitted verbatim.
    fn walk_typst(
        &self,
        node: &SyntaxNode,
        source: &str,
        pos: &mut usize,
        run: &mut Option<(usize, usize)>,
        out: &mut String,
    ) {
        if node.children().len() == 0 {
            let text = node.text();
            let start = *pos;
            *pos += text.len();
            if matches!(node.kind(), SyntaxKind::Text | SyntaxKind::Space) {
                let run_start = run.map(|(s, _)| s).unwrap_or(start);
                *run = Some((run_start, start + text.len()));
            } else {
                if let Some((s, e)) = run.take() {
                    self.emit_styled(s, &source[s..e], false, out);
                }
                out.push_str(text.as_str());
            }
        } else {
            for child in node.children() {
                self.walk_typst(child, source, pos, run, out);
            }
        }
    }
}

/// Wrap each maximal run of non-blank characters in `#underline[…]`, leaving ASCII spaces/tabs bare —
/// the `ULSPACES off` rendering (the underline breaks at inter-word blanks). Matches the editor, which
/// splits its underline rects on `' '`/`'\t'`.
fn underline_words(s: &str) -> String {
    let mut out = String::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if !word.is_empty() {
            out.push_str("#underline[");
            out.push_str(word);
            out.push(']');
            word.clear();
        }
    };
    for ch in s.chars() {
        if ch == ' ' || ch == '\t' {
            flush(&mut word, &mut out);
            out.push(ch);
        } else {
            word.push(ch);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// A Typst `rgb(r, g, b)` colour literal.
fn rgb((r, g, b): (u8, u8, u8)) -> String {
    format!("rgb({r}, {g}, {b})")
}

/// Convert a GFM document to a typst markup **body** (no page/font preamble —
/// that is the renderer's concern in 8c).
pub fn to_typst(source: &str) -> String {
    to_typst_impl(source, None)
}

/// Like [`to_typst`], but splice the document's `MDA` styling (colours + attributes) into the output
/// so it prints — the preview/PDF pre-pass. An empty styler is identical to plain [`to_typst`].
pub fn to_typst_styled(source: &str, styler: &Styler) -> String {
    to_typst_impl(source, (!styler.is_empty()).then_some(styler))
}

fn to_typst_impl(source: &str, styler: Option<&Styler>) -> String {
    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.render.sourcepos = styler.is_some(); // only pay for source positions when styling
    let root = parse_document(&arena, source, &options);

    let em = Emitter { styler };
    let mut out = String::new();
    em.emit(root, &mut out);
    out.truncate(out.trim_end().len()); // drop the trailing block separator
    out
}

/// Carries the optional MDA [`Styler`] through the recursive walk; without it, output is byte-for-byte
/// what the original free-function walk produced.
struct Emitter<'s> {
    styler: Option<&'s Styler>,
}

impl<'s> Emitter<'s> {
    fn emit<'a>(&self, node: &'a AstNode<'a>, out: &mut String) {
        let ast = node.data.borrow();
        match &ast.value {
            NodeValue::Document => self.emit_children(node, out),

            NodeValue::Heading(h) => {
                for _ in 0..h.level {
                    out.push('=');
                }
                out.push(' ');
                self.emit_children(node, out);
                out.push_str("\n\n");
            }
            NodeValue::Paragraph => {
                self.emit_children(node, out);
                out.push_str("\n\n");
            }

            NodeValue::Text(t) => match self
                .styler
                .and_then(|s| crate::highlight::sourcepos_to_bytes(ast.sourcepos, &s.line_starts, usize::MAX).map(|r| (s, r)))
            {
                // Style only when the node's source range matches its literal length (plain prose);
                // otherwise the byte mapping is unreliable, so emit escaped text unchanged.
                Some((s, (start, end))) if end - start == t.len() => s.emit_styled(start, t, true, out),
                _ => out.push_str(&escape_text(t)),
            },
            NodeValue::SoftBreak => out.push(' '),
            NodeValue::LineBreak => out.push_str("#linebreak()"),

            NodeValue::Emph => self.wrap(node, out, "#emph[", "]"),
            NodeValue::Strong => self.wrap(node, out, "#strong[", "]"),
            NodeValue::Strikethrough => self.wrap(node, out, "#strike[", "]"),

            NodeValue::Code(c) => {
                out.push_str("#raw(");
                out.push_str(&typst_str(&c.literal));
                out.push(')');
            }
            NodeValue::CodeBlock(cb) => {
                out.push_str("#raw(");
                out.push_str(&typst_str(&cb.literal));
                out.push_str(", block: true");
                let lang = cb.info.split_whitespace().next().unwrap_or("");
                if !lang.is_empty() {
                    out.push_str(", lang: ");
                    out.push_str(&typst_str(lang));
                }
                out.push_str(")\n\n");
            }

            NodeValue::BlockQuote => {
                out.push_str("#quote(block: true)[");
                out.push_str(self.block_children(node).trim_end());
                out.push_str("]\n\n");
            }

            NodeValue::List(nl) => {
                let ordered = matches!(nl.list_type, ListType::Ordered);
                out.push_str(if ordered { "#enum(" } else { "#list(" });
                if ordered && nl.start != 1 {
                    out.push_str(&format!("start: {}, ", nl.start));
                }
                for item in node.children() {
                    let content = self.block_children(item);
                    let content = content.trim_end();
                    out.push('[');
                    // In a TIGHT list (no blank lines in the source), an item's paragraph and a nested
                    // list/block must not be separated by a blank line — a paragraph break makes Typst
                    // insert paragraph spacing above the nested content (extra gap over the first nested
                    // item). Collapse blank-line runs to a single newline so nesting stays tight. A LOOSE
                    // list keeps its blank lines (its items are meant to be spaced).
                    if nl.tight {
                        out.push_str(&collapse_blank_lines(content));
                    } else {
                        out.push_str(content);
                    }
                    out.push_str("], ");
                }
                out.push_str(")\n\n");
            }

            NodeValue::ThematicBreak => out.push_str("#line(length: 100%)\n\n"),

            NodeValue::Link(nl) => {
                out.push_str("#link(");
                out.push_str(&typst_str(&nl.url));
                out.push(')');
                self.wrap(node, out, "[", "]");
            }
            // Render images as a clickable link to their source (typst `#image`
            // needs a readable path; remote/relative srcs would fail to load).
            NodeValue::Image(nl) => {
                out.push_str("#link(");
                out.push_str(&typst_str(&nl.url));
                out.push(')');
                self.wrap(node, out, "[", "]");
            }

            NodeValue::Table(nt) => {
                let ncols = nt.num_columns.max(1);
                out.push_str(&format!("#table(columns: {ncols}, "));
                for row in node.children() {
                    let is_header = matches!(row.data.borrow().value, NodeValue::TableRow(true));
                    for cell in row.children() {
                        out.push('[');
                        let content = self.block_children(cell);
                        let content = content.trim();
                        if is_header {
                            out.push_str("#strong[");
                            out.push_str(content);
                            out.push(']');
                        } else {
                            out.push_str(content);
                        }
                        out.push_str("], ");
                    }
                }
                out.push_str(")\n\n");
            }

        // A raw HTML BLOCK is a self-contained run of markup, so hand it to the same structural
        // converter `DOCTYPE html` uses: a table or figure written in HTML inside a Markdown file
        // RENDERS instead of showing up as escaped source. An empty result — the block was a comment,
        // or a `<script>`/`<style>` the converter drops — contributes nothing rather than a blank gap.
        NodeValue::HtmlBlock(h) => {
            let converted = crate::html::to_typst(&h.literal);
            if !converted.trim().is_empty() {
                out.push_str(&converted);
                out.push_str("\n\n");
            }
        }
        // INLINE raw HTML can't get the same treatment as a block: comrak delivers it as separate
        // fragment nodes (`<b>`, the text, and `</b>` are three siblings), so there is no self-contained
        // element to convert — feeding `<b>` alone to an HTML parser yields an empty `#strong[]` that
        // swallows the text that follows. EXCEPT `<br>`, which is a VOID element (no closing tag), so it
        // IS self-contained: translate it to a hard line break (GitHub does), matching the block path.
        // Everything else stays escaped text until there's a fragment accumulator.
        NodeValue::HtmlInline(h) => {
            let compact: String =
                h.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
            if compact == "<br>" || compact == "<br/>" {
                out.push_str("#linebreak()");
            } else {
                out.push_str(&escape_text(h));
            }
        }

            // TableRow/TableCell/Item are handled by their parents; anything else
            // (footnotes, etc.) degrades to rendering its children.
            _ => self.emit_children(node, out),
        }
    }

    fn emit_children<'a>(&self, node: &'a AstNode<'a>, out: &mut String) {
        for child in node.children() {
            self.emit(child, out);
        }
    }

    /// Emit `open`, the node's children, then `close` (for inline wrappers).
    fn wrap<'a>(&self, node: &'a AstNode<'a>, out: &mut String, open: &str, close: &str) {
        out.push_str(open);
        self.emit_children(node, out);
        out.push_str(close);
    }

    /// Render a node's children into a fresh string (for block content placed inside
    /// a `[…]` content block, e.g. list items / table cells / quotes).
    fn block_children<'a>(&self, node: &'a AstNode<'a>) -> String {
        let mut s = String::new();
        self.emit_children(node, &mut s);
        s
    }
}

/// Collapse runs of two or more newlines down to a single newline (single newlines pass through). Used
/// to keep a TIGHT list item tight: a paragraph followed by a nested list would otherwise be separated
/// by a blank line, which Typst reads as a paragraph break and spaces the nested content away.
fn collapse_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_nl = false;
    for ch in s.chars() {
        if ch == '\n' {
            if !prev_nl {
                out.push('\n');
            }
            prev_nl = true;
        } else {
            out.push(ch);
            prev_nl = false;
        }
    }
    out
}

// Text/string escaping lives in `typst_esc` — shared with the HTML converter, which faces the same
// typst-injection boundary.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_levels() {
        assert_eq!(to_typst("# One"), "= One");
        assert_eq!(to_typst("### Three"), "=== Three");
    }

    #[test]
    fn inline_br_becomes_a_linebreak() {
        // `<br>` is a void element, so it's self-contained even inline → a hard line break, in any
        // spelling. (Block `<br>` on its own line already worked via the HTML-block path.)
        assert_eq!(to_typst("a<br>b"), "a#linebreak()b");
        assert_eq!(to_typst("a<br/>b"), "a#linebreak()b");
        assert_eq!(to_typst("a<br />b"), "a#linebreak()b");
        assert_eq!(to_typst("a<BR>b"), "a#linebreak()b");
        // Other inline HTML (paired tags, not convertible in isolation) is still left as escaped text.
        assert!(
            !to_typst("a<span>x</span>b").contains("#linebreak()"),
            "non-<br> inline HTML must not be turned into a break"
        );
    }

    #[test]
    fn inline_emphasis_strong_strike_code() {
        assert_eq!(to_typst("a *b* **c**"), "a #emph[b] #strong[c]");
        assert_eq!(to_typst("~~gone~~"), "#strike[gone]");
        assert_eq!(to_typst("use `x` here"), "use #raw(\"x\") here");
    }

    #[test]
    fn mda_styling_splices_typst_into_the_body() {
        use xyscript_xpl::{ATTR_BOLD, ATTR_REVERSE, ATTR_UNDERLINE};
        let src = "hello world";
        // Bold over "hello" (attr layer), red foreground over "world" (fg colour layer).
        let styler = Styler::new(
            src,
            vec![(6, 11, (255, 0, 0))], // fg: "world"
            vec![],
            vec![(0, 5, ATTR_BOLD)], // attr: "hello" bold
            (0, 0, 0),
            (255, 255, 255),
            true,
        );
        assert_eq!(
            to_typst_styled(src, &styler),
            "#strong[hello] #text(fill: rgb(255, 0, 0))[world]"
        );

        // Underline + reverse over the whole word; reverse defines its own fg/bg from the doc colours.
        let styler2 = Styler::new(
            "boo",
            vec![],
            vec![],
            vec![(0, 3, ATTR_UNDERLINE | ATTR_REVERSE)],
            (10, 20, 30),
            (200, 210, 220),
            true,
        );
        assert_eq!(
            to_typst_styled("boo", &styler2),
            "#highlight(fill: rgb(10, 20, 30))[#text(fill: rgb(200, 210, 220))[#underline[boo]]]"
        );

        // An empty styler is identical to plain to_typst (no regression when nothing is styled).
        let empty = Styler::new(src, vec![], vec![], vec![], (0, 0, 0), (255, 255, 255), true);
        assert_eq!(to_typst_styled(src, &empty), to_typst(src));
    }

    #[test]
    fn ulspaces_off_breaks_the_underline_at_blanks() {
        use xyscript_xpl::ATTR_UNDERLINE;
        let src = "a b\tc"; // space then tab between the letters
        let attr = || vec![(0, src.len(), ATTR_UNDERLINE)];
        let mk = |ul_spaces| {
            Styler::new(src, vec![], vec![], attr(), (0, 0, 0), (255, 255, 255), ul_spaces)
        };
        // ULSPACES on → the whole run is underlined, blanks included.
        assert_eq!(to_typst_styled(src, &mk(true)), "#underline[a b\tc]");
        // ULSPACES off → each non-blank run is underlined; the space and tab stay bare.
        assert_eq!(to_typst_styled(src, &mk(false)), "#underline[a] #underline[b]\t#underline[c]");
    }

    #[test]
    fn authored_typst_splice_styles_prose_only() {
        use xyscript_xpl::{ATTR_BOLD, ATTR_UNDERLINE};
        // "= Fancy" heading + a paragraph. Byte offsets: '=' 0, ' ' 1, "Fancy" [2,7), "\n\n" [7,9),
        // "hello world" [9,20).
        let src = "= Fancy\n\nhello world";
        let styler = Styler::new(
            src,
            vec![],
            vec![],
            vec![(2, 7, ATTR_BOLD), (9, 20, ATTR_UNDERLINE)], // bold "Fancy", underline "hello world"
            (0, 0, 0),
            (255, 255, 255),
            true,
        );
        let out = styler.splice_typst_source(src);
        // The heading marker passes through; only its prose is wrapped.
        assert!(out.starts_with("= #strong[Fancy]"), "heading prose bold, marker intact: {out}");
        // The multi-word underline is a SINGLE wrapper (spacing preserved), NOT per-word.
        assert!(out.contains("#underline[hello world]"), "one wrapper over the phrase: {out}");
        assert!(!out.contains("#underline[hello] #underline[world]"), "not per-word: {out}");
        // An empty styler returns the source byte-for-byte.
        let empty = Styler::new(src, vec![], vec![], vec![], (0, 0, 0), (255, 255, 255), true);
        assert_eq!(empty.splice_typst_source(src), src);
    }

    #[test]
    fn authored_typst_splice_leaves_code_and_markers_untouched() {
        use xyscript_xpl::ATTR_UNDERLINE;
        // A whole-document underline must NOT wrap the `#strong[…]` call or the math — only the plain
        // prose leaves. (Wrapping a function call or math would produce broken Typst.)
        let src = "see #strong[bold] then $x^2$ done";
        let styler = Styler::new(
            src,
            vec![],
            vec![],
            vec![(0, src.len(), ATTR_UNDERLINE)],
            (0, 0, 0),
            (255, 255, 255),
            true,
        );
        let out = styler.splice_typst_source(src);
        // The `#strong[…]` call and the math survive verbatim as STRUCTURE; the call's inner prose
        // ("bold") is itself styled (correct — it's content the user underlined), but math content is
        // not markup text, so it's left alone.
        assert!(out.contains("#strong[#underline[bold]]"), "call intact, inner prose styled: {out}");
        assert!(out.contains("$x^2$"), "math must stay intact: {out}");
        assert!(out.starts_with("#underline[see"), "leading prose underlined: {out}");
        // The call itself and the math were NOT wrapped in an underline (that would break Typst).
        assert!(!out.contains("#underline[#strong"), "the call is not wrapped: {out}");
        assert!(!out.contains("#underline[$"), "math is not wrapped: {out}");
    }

    #[test]
    fn links_carry_targets() {
        assert_eq!(to_typst("see [docs](http://x)"), "see #link(\"http://x\")[docs]");
    }

    #[test]
    fn escaping_prevents_typst_injection() {
        // `#` and `$` must be neutralized, or they'd inject typst code/math.
        assert_eq!(to_typst(r"a #b $c d"), r"a \#b \$c d");
        // GFM consumes `\[`/`\]` as escapes (→ literal brackets); we then escape
        // those brackets for typst.
        assert_eq!(to_typst(r"x\[y\]"), r"x\[y\]");
    }

    #[test]
    fn fenced_code_block_with_lang() {
        let md = "```rust\nlet x = 1;\n```\n";
        assert_eq!(to_typst(md), "#raw(\"let x = 1;\\n\", block: true, lang: \"rust\")");
    }

    #[test]
    fn bullet_and_ordered_lists() {
        assert_eq!(to_typst("- a\n- b"), "#list([a], [b], )");
        assert_eq!(to_typst("1. a\n2. b"), "#enum([a], [b], )");
        assert_eq!(to_typst("3. a\n4. b"), "#enum(start: 3, [a], [b], )");
    }

    #[test]
    fn tight_nested_list_stays_tight() {
        // A nested list in a TIGHT list (no blank lines in the source) must be joined to its parent
        // item's text by a single newline, not a blank line — the blank line made Typst insert
        // paragraph spacing above the first nested item.
        assert_eq!(to_typst("- a\n  - b\n  - c\n- d"), "#list([a\n#list([b], [c], )], [d], )");
        // A LOOSE list (blank lines between items) is NOT collapsed — its spacing is intentional.
        assert!(
            to_typst("- a\n\n  - b\n\n- d").contains("a\n\n"),
            "loose list keeps its blank-line paragraph spacing"
        );
    }

    #[test]
    fn blockquote_and_rule() {
        assert_eq!(to_typst("> quoted"), "#quote(block: true)[quoted]");
        assert_eq!(to_typst("---"), "#line(length: 100%)");
    }

    #[test]
    fn table_with_header() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |\n";
        let out = to_typst(md);
        assert!(out.starts_with("#table(columns: 2, "), "got: {out}");
        assert!(out.contains("[#strong[A]], [#strong[B]], "), "got: {out}");
        assert!(out.contains("[1], [2], "), "got: {out}");
    }

    #[test]
    fn paragraphs_separated_by_blank_lines() {
        assert_eq!(to_typst("one\n\ntwo"), "one\n\ntwo");
        // A soft break within a paragraph becomes a space.
        assert_eq!(to_typst("one\ntwo"), "one two");
    }
}


#[cfg(test)]
mod html_block_tests {
    use super::*;

    #[test]
    fn raw_html_blocks_render_instead_of_showing_as_source() {
        // The payoff: a table written in HTML inside a Markdown file becomes a real table.
        let md = "before\n\n<table><tr><th>H</th></tr><tr><td>1</td></tr></table>\n\nafter";
        assert_eq!(to_typst(md), "before\n\n#table(columns: 1, [#strong[H]], [1], )\n\nafter");
        // Structure survives; presentation-only wrappers are transparent.
        assert_eq!(
            to_typst("a\n\n<div class=\"x\"><p>Hi <em>there</em></p></div>\n\nb"),
            "a\n\nHi #emph[there]\n\nb"
        );
    }

    #[test]
    fn html_blocks_that_carry_no_content_leave_no_gap() {
        // A comment or a dropped script/style contributes nothing — not a blank block.
        assert_eq!(to_typst("a\n\n<!-- note -->\n\nb"), "a\n\nb");
        assert_eq!(to_typst("a\n\n<script>var x=1;</script>\n\nb"), "a\n\nb");
    }

    #[test]
    fn inline_raw_html_is_still_escaped() {
        // Documented gap: comrak splits inline HTML into open/close fragments, so there is no
        // self-contained element to convert. It stays visible as text rather than silently
        // swallowing the content between the tags.
        assert_eq!(to_typst("text with <b>inline</b> html"), r"text with \<b\>inline\</b\> html");
    }

    #[test]
    fn native_markdown_constructs_are_unaffected() {
        // Guard against the HTML path capturing things it shouldn't.
        assert_eq!(
            to_typst("| md | table |\n|---|---|\n| 1 | 2 |"),
            "#table(columns: 2, [#strong[md]], [#strong[table]], [1], [2], )"
        );
        assert_eq!(to_typst("# Heading\n\ntext"), "= Heading\n\ntext");
    }
}
