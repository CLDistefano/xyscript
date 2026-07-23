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

/// Convert a GFM document to a typst markup **body** (no page/font preamble —
/// that is the renderer's concern in 8c).
pub fn to_typst(source: &str) -> String {
    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    let root = parse_document(&arena, source, &options);

    let mut out = String::new();
    emit(root, &mut out);
    out.truncate(out.trim_end().len()); // drop the trailing block separator
    out
}

fn emit<'a>(node: &'a AstNode<'a>, out: &mut String) {
    let ast = node.data.borrow();
    match &ast.value {
        NodeValue::Document => emit_children(node, out),

        NodeValue::Heading(h) => {
            for _ in 0..h.level {
                out.push('=');
            }
            out.push(' ');
            emit_children(node, out);
            out.push_str("\n\n");
        }
        NodeValue::Paragraph => {
            emit_children(node, out);
            out.push_str("\n\n");
        }

        NodeValue::Text(t) => out.push_str(&escape_text(t)),
        NodeValue::SoftBreak => out.push(' '),
        NodeValue::LineBreak => out.push_str("#linebreak()"),

        NodeValue::Emph => wrap(node, out, "#emph[", "]"),
        NodeValue::Strong => wrap(node, out, "#strong[", "]"),
        NodeValue::Strikethrough => wrap(node, out, "#strike[", "]"),

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
            out.push_str(block_children(node).trim_end());
            out.push_str("]\n\n");
        }

        NodeValue::List(nl) => {
            let ordered = matches!(nl.list_type, ListType::Ordered);
            out.push_str(if ordered { "#enum(" } else { "#list(" });
            if ordered && nl.start != 1 {
                out.push_str(&format!("start: {}, ", nl.start));
            }
            for item in node.children() {
                out.push('[');
                out.push_str(block_children(item).trim_end());
                out.push_str("], ");
            }
            out.push_str(")\n\n");
        }

        NodeValue::ThematicBreak => out.push_str("#line(length: 100%)\n\n"),

        NodeValue::Link(nl) => {
            out.push_str("#link(");
            out.push_str(&typst_str(&nl.url));
            out.push(')');
            wrap(node, out, "[", "]");
        }
        // Render images as a clickable link to their source (typst `#image`
        // needs a readable path; remote/relative srcs would fail to load).
        NodeValue::Image(nl) => {
            out.push_str("#link(");
            out.push_str(&typst_str(&nl.url));
            out.push(')');
            wrap(node, out, "[", "]");
        }

        NodeValue::Table(nt) => {
            let ncols = nt.num_columns.max(1);
            out.push_str(&format!("#table(columns: {ncols}, "));
            for row in node.children() {
                let is_header = matches!(row.data.borrow().value, NodeValue::TableRow(true));
                for cell in row.children() {
                    out.push('[');
                    let content = block_children(cell);
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
        // INLINE raw HTML can't get the same treatment: comrak delivers it as separate fragment nodes
        // (`<b>`, the text, and `</b>` are three siblings), so there is no self-contained element to
        // convert — feeding `<b>` alone to an HTML parser yields an empty `#strong[]` that swallows
        // the text that follows. Left as escaped text until there's a fragment accumulator.
        NodeValue::HtmlInline(h) => out.push_str(&escape_text(h)),

        // TableRow/TableCell/Item are handled by their parents; anything else
        // (footnotes, etc.) degrades to rendering its children.
        _ => emit_children(node, out),
    }
}

fn emit_children<'a>(node: &'a AstNode<'a>, out: &mut String) {
    for child in node.children() {
        emit(child, out);
    }
}

/// Emit `open`, the node's children, then `close` (for inline wrappers).
fn wrap<'a>(node: &'a AstNode<'a>, out: &mut String, open: &str, close: &str) {
    out.push_str(open);
    emit_children(node, out);
    out.push_str(close);
}

/// Render a node's children into a fresh string (for block content placed inside
/// a `[…]` content block, e.g. list items / table cells / quotes).
fn block_children<'a>(node: &'a AstNode<'a>) -> String {
    let mut s = String::new();
    emit_children(node, &mut s);
    s
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
    fn inline_emphasis_strong_strike_code() {
        assert_eq!(to_typst("a *b* **c**"), "a #emph[b] #strong[c]");
        assert_eq!(to_typst("~~gone~~"), "#strike[gone]");
        assert_eq!(to_typst("use `x` here"), "use #raw(\"x\") here");
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
