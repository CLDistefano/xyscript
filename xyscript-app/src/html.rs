//! HTML → typst markup — the *structural* conversion.
//!
//! Same contract as [`crate::gfm`]: [`to_typst`] turns a document into a typst **body** string, which
//! [`crate::pdf`] compiles to PDF or rasters for the live preview. Sharing that intermediate form is
//! what keeps HTML support from becoming an HTML/browser pivot — we translate the document's
//! *structure* into typst and let the existing native renderer lay it out. We do **not** interpret
//! presentation: `class`, `style`, and stylesheets are ignored. HTML-as-document converts well;
//! HTML-as-page-layout (a table-based marketing email, say) converts to readable content, not to a
//! visual replica of the page.
//!
//! Parsing is html5ever, so malformed real-world markup (unclosed tags, stray `<`, implied `tbody`)
//! recovers the way a browser would rather than failing.
//!
//! The two correctness concerns from `gfm.rs` apply unchanged — text goes through
//! [`escape_text`] so it can never inject typst, and we emit *function-form* markup
//! (`#strong[…]`, `#table(…)`) rather than line-start-sensitive shorthand. Two more are specific
//! to HTML:
//! - **Non-content subtrees are dropped.** `<script>`/`<style>`/`<head>` text is code, CSS, and
//!   metadata; emitting it would dump source into the document.
//! - **Whitespace is collapsed.** HTML treats any run of whitespace as one space (the newlines and
//!   indentation between tags are not content). `<pre>` opts out.
//!
//! Known first-cut gaps: images render as a link to their src (same conservative choice `gfm.rs`
//! documents — an unloadable `#image` path fails the whole compile, and most real HTML points at
//! remote images); `colspan`/`rowspan` are ignored; definition lists render as plain blocks.

use crate::typst_esc::{escape_text, typst_str};
use html5ever::driver::ParseOpts;
use html5ever::parse_document;
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};

/// Convert an HTML document to a typst markup **body** (no page/font preamble — the renderer's
/// concern, exactly as for GFM).
pub fn to_typst(source: &str) -> String {
    let dom = parse_document(RcDom::default(), ParseOpts::default()).one(source);
    let mut out = String::new();
    emit(&dom.document, &mut out, false);
    tidy(&out)
}

/// Elements whose *content* is not document text — dropped subtree and all.
fn is_dropped(tag: &str) -> bool {
    matches!(tag, "script" | "style" | "head" | "meta" | "link" | "title" | "base" | "noscript")
}

fn emit(node: &Handle, out: &mut String, in_link: bool) {
    match &node.data {
        NodeData::Document => emit_children(node, out, in_link),

        // Collapse whitespace runs to a single space: the newlines/indentation between tags are
        // layout in the source, not content. `<pre>` never reaches here (see the `pre` arm).
        NodeData::Text { contents } => {
            let text = contents.borrow();
            out.push_str(&escape_text(&collapse_ws(&text)));
        }

        NodeData::Element { name, attrs, .. } => {
            let tag = name.local.as_ref();
            let attrs = attrs.borrow();
            let attr = |k: &str| -> Option<String> {
                attrs.iter().find(|a| a.name.local.as_ref() == k).map(|a| a.value.to_string())
            };
            emit_element(node, out, tag, &attr, in_link);
        }

        // Comments, doctype, and processing instructions carry no document content.
        NodeData::Comment { .. } | NodeData::Doctype { .. } | NodeData::ProcessingInstruction { .. } => {}
    }
}

fn emit_element(
    node: &Handle,
    out: &mut String,
    tag: &str,
    attr: &dyn Fn(&str) -> Option<String>,
    in_link: bool,
) {
    match tag {
        _ if is_dropped(tag) => {}

        // Headings: `h1`..`h6` → `=`..`======`.
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = tag.as_bytes()[1] - b'0';
            let content = inline_content(node, in_link);
            if !content.is_empty() {
                for _ in 0..level {
                    out.push('=');
                }
                out.push(' ');
                out.push_str(&content);
                out.push_str("\n\n");
            }
        }

        "p" => {
            let content = inline_content(node, in_link);
            if !content.is_empty() {
                out.push_str(&content);
                out.push_str("\n\n");
            }
        }

        "br" => out.push_str("#linebreak()"),
        "hr" => out.push_str("#line(length: 100%)\n\n"),

        "strong" | "b" => wrap(node, out, "#strong[", "]", in_link),
        "em" | "i" | "cite" | "var" => wrap(node, out, "#emph[", "]", in_link),
        "s" | "del" | "strike" => wrap(node, out, "#strike[", "]", in_link),
        "u" | "ins" => wrap(node, out, "#underline[", "]", in_link),
        "sub" => wrap(node, out, "#sub[", "]", in_link),
        "sup" => wrap(node, out, "#super[", "]", in_link),

        // Inline code: the literal text, unescaped-but-quoted via `#raw`.
        "code" | "kbd" | "samp" | "tt" => {
            out.push_str("#raw(");
            out.push_str(&typst_str(&raw_text(node)));
            out.push(')');
        }

        // Preformatted: whitespace is significant, so take the subtree's raw text. A nested
        // `<code class="language-x">` names the language, the usual HTML idiom for code blocks.
        "pre" => {
            let text = raw_text(node);
            out.push_str("#raw(");
            out.push_str(&typst_str(&text));
            out.push_str(", block: true");
            if let Some(lang) = code_language(node) {
                out.push_str(", lang: ");
                out.push_str(&typst_str(&lang));
            }
            out.push_str(")\n\n");
        }

        "ul" | "menu" => emit_list(node, out, false, 1, in_link),
        "ol" => {
            let start = attr("start").and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or(1);
            emit_list(node, out, true, start, in_link);
        }

        "blockquote" => {
            let content = block_children(node, in_link);
            let content = content.trim();
            if !content.is_empty() {
                out.push_str("#quote(block: true)[");
                out.push_str(content);
                out.push_str("]\n\n");
            }
        }

        "a" => {
            // An anchor with no href is just a link target — render its text, not a link.
            match attr("href") {
                Some(href) if !href.trim().is_empty() => {
                    out.push_str("#link(");
                    out.push_str(&typst_str(href.trim()));
                    out.push(')');
                    wrap(node, out, "[", "]", true);
                }
                _ => emit_children(node, out, in_link),
            }
        }

        // Images render as a link to their src (see the module note), labelled by their alt text so
        // the document still reads sensibly; the src is the fallback label.
        //
        // INSIDE an anchor (`<a href="X"><img src="Y">`, the linked-banner idiom that fills real
        // HTML) we emit only the label: nesting `#link(Y)` inside `#link(X)` would point the click
        // at the image FILE instead of the anchor's destination.
        "img" => {
            let src = attr("src").unwrap_or_default();
            if !src.trim().is_empty() {
                let alt = attr("alt").unwrap_or_default();
                let label = if alt.trim().is_empty() { src.clone() } else { alt };
                let label = escape_text(&collapse_ws(&label));
                if in_link {
                    out.push_str(&label);
                } else {
                    out.push_str("#link(");
                    out.push_str(&typst_str(src.trim()));
                    out.push_str(")[");
                    out.push_str(&label);
                    out.push(']');
                }
            }
        }

        "table" => emit_table(node, out, in_link),

        // Everything else — `div`, `span`, `section`, `body`, `html`, `tbody`, … — is transparent:
        // it contributes structure we don't model, so render its children. This is also the
        // recovery path for unknown//custom elements.
        _ => emit_children(node, out, in_link),
    }
}

/// `#list(…)` / `#enum(…)` from the element's `<li>` children (non-`li` children are whitespace).
fn emit_list(node: &Handle, out: &mut String, ordered: bool, start: i64, in_link: bool) {
    let items: Vec<String> = children(node)
        .iter()
        .filter(|c| element_tag(c).as_deref() == Some("li"))
        .map(|li| block_children(li, in_link).trim().to_string())
        .collect();
    if items.is_empty() {
        return;
    }
    out.push_str(if ordered { "#enum(" } else { "#list(" });
    if ordered && start != 1 {
        out.push_str(&format!("start: {start}, "));
    }
    for item in items {
        out.push('[');
        out.push_str(&item);
        out.push_str("], ");
    }
    out.push_str(")\n\n");
}

/// `#table(columns: n, …)`. Rows are gathered through any `thead`/`tbody`/`tfoot` wrappers; `th`
/// cells are bolded like the GFM header row. Ragged rows are padded so columns stay aligned.
/// `colspan`/`rowspan` are ignored (a first-cut gap).
fn emit_table(node: &Handle, out: &mut String, in_link: bool) {
    let mut rows: Vec<Handle> = Vec::new();
    collect_rows(node, &mut rows);
    let cells: Vec<Vec<(bool, String)>> = rows
        .iter()
        .map(|row| {
            children(row)
                .iter()
                .filter_map(|c| {
                    let tag = element_tag(c)?;
                    matches!(tag.as_str(), "td" | "th")
                        .then(|| (tag == "th", block_children(c, in_link).trim().to_string()))
                })
                .collect()
        })
        .filter(|r: &Vec<(bool, String)>| !r.is_empty())
        .collect();
    let ncols = cells.iter().map(|r| r.len()).max().unwrap_or(0);
    if ncols == 0 {
        return;
    }
    out.push_str(&format!("#table(columns: {ncols}, "));
    for row in &cells {
        for (is_header, content) in row {
            out.push('[');
            if *is_header && !content.is_empty() {
                out.push_str("#strong[");
                out.push_str(content);
                out.push(']');
            } else {
                out.push_str(content);
            }
            out.push_str("], ");
        }
        for _ in row.len()..ncols {
            out.push_str("[], "); // pad a short row so the grid stays aligned
        }
    }
    out.push_str(")\n\n");
}

/// Push every `<tr>` in the subtree onto `rows` (descending through `thead`/`tbody`/`tfoot`, but not
/// into a nested `<table>` — its rows belong to that table).
fn collect_rows(node: &Handle, rows: &mut Vec<Handle>) {
    for child in children(node).iter() {
        match element_tag(child).as_deref() {
            Some("tr") => rows.push(child.clone()),
            Some("table") => {}
            Some(_) => collect_rows(child, rows),
            None => {}
        }
    }
}

/// The `lang` for a `<pre>`, from a descendant `<code class="language-x">` / `class="lang-x"`.
fn code_language(node: &Handle) -> Option<String> {
    for child in children(node).iter() {
        if element_tag(child).as_deref() == Some("code") {
            if let NodeData::Element { attrs, .. } = &child.data {
                for a in attrs.borrow().iter() {
                    if a.name.local.as_ref() == "class" {
                        for cls in a.value.split_whitespace() {
                            if let Some(l) = cls.strip_prefix("language-").or(cls.strip_prefix("lang-")) {
                                if !l.is_empty() {
                                    return Some(l.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some(found) = code_language(child) {
            return Some(found);
        }
    }
    None
}

fn emit_children(node: &Handle, out: &mut String, in_link: bool) {
    for child in children(node).iter() {
        emit(child, out, in_link);
    }
}

/// Emit `open`, the node's children, then `close` (for inline wrappers).
fn wrap(node: &Handle, out: &mut String, open: &str, close: &str, in_link: bool) {
    out.push_str(open);
    emit_children(node, out, in_link);
    out.push_str(close);
}

/// A node's children rendered into a fresh string (for content placed inside a `[…]` block).
fn block_children(node: &Handle, in_link: bool) -> String {
    let mut s = String::new();
    emit_children(node, &mut s, in_link);
    s
}

/// Like [`block_children`], but trimmed and with any block separators flattened — for places that
/// must stay on one line (a heading, a paragraph).
fn inline_content(node: &Handle, in_link: bool) -> String {
    let s = block_children(node, in_link);
    collapse_ws(&s).trim().to_string()
}

/// Every descendant text node concatenated verbatim — no escaping, no whitespace collapsing. Used
/// for `<pre>`/`<code>`, whose content goes into a typst string literal.
fn raw_text(node: &Handle) -> String {
    let mut s = String::new();
    fn walk(node: &Handle, s: &mut String) {
        match &node.data {
            NodeData::Text { contents } => s.push_str(&contents.borrow()),
            NodeData::Element { name, .. } if is_dropped(name.local.as_ref()) => {}
            _ => {
                for child in children(node).iter() {
                    walk(child, s);
                }
            }
        }
    }
    walk(node, &mut s);
    s
}

fn children(node: &Handle) -> std::cell::Ref<'_, Vec<Handle>> {
    node.children.borrow()
}

/// The lowercase tag name if `node` is an element.
fn element_tag(node: &Handle) -> Option<String> {
    match &node.data {
        NodeData::Element { name, .. } => Some(name.local.as_ref().to_ascii_lowercase()),
        _ => None,
    }
}

/// Collapse every run of whitespace to a single space (HTML's own rule for non-`pre` content).
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(ch);
            in_ws = false;
        }
    }
    out
}

/// Final clean-up of the emitted body: trim each line, squeeze runs of blank lines down to one, and
/// trim the ends. Collapsing whitespace per text node leaves stray spaces around block boundaries
/// (the newlines between tags become a space that lands at a line start or end). Trimming whole
/// lines is safe here because every block we emit occupies its own line and typst markup is not
/// indentation-sensitive in function form — and `#raw` content is inside a string literal, where the
/// newlines are already escaped, so it is never split across lines for this to touch.
fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank_run = 0usize;
    for line in s.lines() {
        let line = line.trim();
        if line.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.truncate(out.trim_end().len());
    out
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_paragraphs_and_inline_marks() {
        assert_eq!(to_typst("<h1>One</h1>"), "= One");
        assert_eq!(to_typst("<h3>Three</h3>"), "=== Three");
        assert_eq!(
            to_typst("<h1>Title</h1><p>Hello <b>bold</b> and <i>it</i>.</p>"),
            "= Title\n\nHello #strong[bold] and #emph[it]."
        );
        assert_eq!(to_typst("<p><strong>a</strong> <em>b</em></p>"), "#strong[a] #emph[b]");
        assert_eq!(to_typst("<p><del>gone</del> <u>up</u></p>"), "#strike[gone] #underline[up]");
        assert_eq!(to_typst("<p>H<sub>2</sub>O e<sup>x</sup></p>"), "H#sub[2]O e#super[x]");
        assert_eq!(to_typst("<p>a<br>b</p>"), "a#linebreak()b");
        assert_eq!(to_typst("<hr>"), "#line(length: 100%)");
    }

    #[test]
    fn code_and_preformatted_keep_their_text_verbatim() {
        assert_eq!(to_typst("<p>use <code>x &amp; y</code></p>"), "use #raw(\"x & y\")");
        // Raw strings below: the `\n` inside the emitted typst string literal is a real backslash-n,
        // because `typst_str` escapes the newline rather than embedding it.
        assert_eq!(
            to_typst("<pre><code class=\"language-rust\">let x = 1;\n</code></pre>"),
            r#"#raw("let x = 1;\n", block: true, lang: "rust")"#
        );
        // Without a language class, no `lang:` argument.
        assert_eq!(to_typst("<pre>a b\n c</pre>"), r#"#raw("a b\n c", block: true)"#);
    }

    #[test]
    fn lists_quotes_links_and_images() {
        assert_eq!(to_typst("<ul><li>one</li><li>two</li></ul>"), "#list([one], [two], )");
        assert_eq!(to_typst("<ol><li>a</li></ol>"), "#enum([a], )");
        assert_eq!(to_typst("<ol start=\"3\"><li>a</li></ol>"), "#enum(start: 3, [a], )");
        assert_eq!(to_typst("<blockquote><p>quoted</p></blockquote>"), "#quote(block: true)[quoted]");
        assert_eq!(to_typst("<p>a <a href=\"http://x\">link</a> b</p>"), "a #link(\"http://x\")[link] b");
        // An anchor with no href is a target, not a link — render just its text.
        assert_eq!(to_typst("<p><a name=\"top\">here</a></p>"), "here");
        // Images render as a link to their src, labelled by alt (see the module note).
        assert_eq!(to_typst("<p><img src=\"a.png\" alt=\"Alt\"></p>"), "#link(\"a.png\")[Alt]");
        assert_eq!(to_typst("<p><img src=\"a.png\"></p>"), "#link(\"a.png\")[a.png]");
    }

    /// `<a href="X"><img src="Y"></a>` — the linked-banner idiom that fills real-world HTML. The
    /// image must NOT emit its own nested `#link`, or the click lands on the image file (Y) instead
    /// of the anchor's destination (X).
    #[test]
    fn a_linked_image_keeps_the_anchors_destination() {
        assert_eq!(
            to_typst(r#"<a href="http://dest"><img src="http://img.png" alt="Banner"></a>"#),
            r#"#link("http://dest")[Banner]"#
        );
        // Nested deeper inside the anchor, and with no alt, the same rule holds.
        assert_eq!(
            to_typst(r#"<a href="http://dest"><span><img src="http://img.png"></span></a>"#),
            r#"#link("http://dest")[http://img.png]"#
        );
        // Outside an anchor the image still links to its own src.
        assert_eq!(to_typst(r#"<img src="x.png" alt="A">"#), r#"#link("x.png")[A]"#);
    }

    #[test]
    fn tables_gather_rows_through_wrappers_and_pad_ragged_rows() {
        assert_eq!(
            to_typst("<table><tr><th>H</th><th>I</th></tr><tr><td>1</td><td>2</td></tr></table>"),
            "#table(columns: 2, [#strong[H]], [#strong[I]], [1], [2], )"
        );
        // thead/tbody wrappers are descended through, and a short row is padded to the column count.
        assert_eq!(
            to_typst("<table><thead><tr><th>A</th><th>B</th></tr></thead><tbody><tr><td>1</td></tr></tbody></table>"),
            "#table(columns: 2, [#strong[A]], [#strong[B]], [1], [], )"
        );
    }

    #[test]
    fn text_content_can_never_inject_typst() {
        // The whole point of routing text through `escape_text`: `#` starts code, `$` starts math.
        assert_eq!(to_typst("<p>inject #hash $math</p>"), r"inject \#hash \$math");
        assert_eq!(to_typst("<p>x[y]</p>"), r"x\[y\]");
        // A `#` arriving via an entity is still escaped — it is text by the time we see it. (`-` is
        // in the escaped set too; over-escaping a line-start marker is harmless, per `typst_esc`.)
        assert_eq!(to_typst("<p>&#35;not-code</p>"), r"\#not\-code");
    }

    #[test]
    fn non_content_subtrees_are_dropped() {
        // Script/style text is code and CSS — emitting it would dump source into the document.
        assert_eq!(to_typst("<script>var x = 1;</script><style>p{color:red}</style><p>after</p>"), "after");
        assert_eq!(to_typst("<head><title>T</title></head><body><p>b</p></body>"), "b");
        // Comments carry no content.
        assert_eq!(to_typst("<p>a<!-- note -->b</p>"), "ab");
    }

    #[test]
    fn whitespace_between_tags_is_not_content() {
        // The newlines and indentation between tags collapse away; no leading/trailing artifacts.
        assert_eq!(to_typst("<div>\n  <p>spread\n  over lines</p>\n</div>"), "spread over lines");
        assert_eq!(to_typst("<p>a</p>\n\n<p>b</p>"), "a\n\nb");
    }

    #[test]
    fn presentation_is_ignored_structure_is_kept() {
        // `class`/`style` carry no structure — the element is transparent, its text survives.
        assert_eq!(
            to_typst("<div class=\"fancy\" style=\"color:red\"><p>text</p></div>"),
            "text"
        );
        assert_eq!(to_typst("<p><span style=\"font-weight:bold\">not bold</span></p>"), "not bold");
    }

    #[test]
    fn malformed_markup_recovers_like_a_browser() {
        assert_eq!(to_typst("<p>unclosed <b>bold"), "unclosed #strong[bold]");
        assert_eq!(to_typst("<ul><li>a<li>b</ul>"), "#list([a], [b], )"); // implied </li>
        assert_eq!(to_typst("plain text, no tags"), "plain text, no tags");
        assert_eq!(to_typst(""), "");
    }

    #[test]
    fn entities_are_decoded_before_escaping() {
        assert_eq!(to_typst("<p>a &amp; b &lt; c</p>"), r"a & b \< c");
        assert_eq!(to_typst("<p>&copy; 2026</p>"), "© 2026");
    }

    /// The emitted body must be WELL-FORMED typst, not merely the string we expected — so compile a
    /// document exercising every construct through the real engine. This is what catches an unbalanced
    /// `[`, a stray `#`, or a bad argument list that a string comparison would happily accept.
    #[test]
    fn emitted_markup_compiles_as_typst() {
        // `r##"…"##`: the HTML below contains `"#`, which would close a plain `r#"…"#`.
        let html = r##"
            <!DOCTYPE html>
            <html><head><title>T</title><style>p{color:red}</style></head>
            <body>
              <h1>Report &amp; Notes</h1>
              <p>Text with <b>bold</b>, <i>italic</i>, <code>x[0] # $</code>, and a
                 <a href="http://example.com/a?b=1&amp;c=2">link</a>.</p>
              <ul><li>first</li><li>second <em>item</em></li></ul>
              <ol start="4"><li>four</li></ol>
              <blockquote><p>A quotation.</p></blockquote>
              <pre><code class="language-rust">fn main() { let x = "#"; }
</code></pre>
              <table>
                <thead><tr><th>Name</th><th>Value</th></tr></thead>
                <tbody><tr><td>alpha</td><td>1</td></tr><tr><td>ragged</td></tr></tbody>
              </table>
              <p><img src="logo.png" alt="Logo"> H<sub>2</sub>O e<sup>x</sup></p>
              <hr>
              <p>Trailing paragraph with a stray &lt;angle&gt; and 100% width.</p>
              <script>if (a < b) { danger(); }</script>
            </body></html>
        "##;
        let body = to_typst(html);
        // Sanity: the dangerous subtrees really are gone before we hand this to typst.
        assert!(!body.contains("danger()"), "script text leaked into the body: {body}");
        assert!(!body.contains("color:red"), "style text leaked into the body: {body}");
        let pdf = crate::pdf::typst_to_pdf(&body, std::path::Path::new("."))
            .unwrap_or_else(|e| panic!("emitted markup failed to compile:\n{e}\n--- body ---\n{body}"));
        assert!(pdf.starts_with(b"%PDF-"));
    }
}
