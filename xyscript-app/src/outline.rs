//! Document outline for `OUTLINE`/`OL`: extract a navigable list of a document's structure — headings
//! for Markdown / Typst / HTML, or `SUB <name>` routine headers for XPL (plain-text) files. Pure and
//! line-oriented; the host turns each entry's line into a rope offset and feeds the chooser.

use crate::document::RenderKind;

/// One outline entry: a display `label`, the 0-based `line` it lives on, and a `level` (1 = top) used
/// to indent the label in the chooser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineEntry {
    pub label: String,
    pub line: usize,
    pub level: u8,
}

/// Extract the outline of `text` for a document of `kind`. Empty when there's no detectable structure.
pub fn outline(text: &str, kind: RenderKind) -> Vec<OutlineEntry> {
    match kind {
        RenderKind::Gfm => headings_atx(text),
        RenderKind::Typst => headings_marker(text, '='),
        RenderKind::Html => headings_html(text),
        RenderKind::Text => xpl_routines(text),
    }
}

/// Is a (trimmed) line a fenced-code-block delimiter (` ``` ` or `~~~`)? Toggles the skip state so a
/// `#`/`=` inside a code block isn't mistaken for a heading.
fn is_fence(trimmed: &str) -> bool {
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

/// Markdown ATX headings: `#`..`######` at the line start (up to 3 leading spaces), outside code fences.
fn headings_atx(text: &str) -> Vec<OutlineEntry> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for (line, raw) in text.lines().enumerate() {
        let t = raw.trim_start();
        if is_fence(t) {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || raw.len() - t.len() > 3 {
            continue; // inside a fence, or indented >3 (a code block, not a heading)
        }
        let hashes = t.bytes().take_while(|&b| b == b'#').count();
        if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            let label = t[hashes..].trim().trim_end_matches('#').trim_end().to_string();
            if !label.is_empty() {
                out.push(OutlineEntry { label, line, level: hashes as u8 });
            }
        }
    }
    out
}

/// Marker-prefix headings (Typst `=`..`======`) at the line start, outside code fences.
fn headings_marker(text: &str, marker: char) -> Vec<OutlineEntry> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for (line, raw) in text.lines().enumerate() {
        let t = raw.trim_start();
        if is_fence(t) {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let marks = t.chars().take_while(|&c| c == marker).count();
        if (1..=6).contains(&marks) && t[marks..].starts_with(' ') {
            let label = t[marks..].trim().to_string();
            if !label.is_empty() {
                out.push(OutlineEntry { label, line, level: marks as u8 });
            }
        }
    }
    out
}

/// HTML `<h1>`..`<h6>` headings (one per line, the common case): level from the tag digit, label from
/// the line with all tags stripped.
fn headings_html(text: &str) -> Vec<OutlineEntry> {
    let mut out = Vec::new();
    for (line, raw) in text.lines().enumerate() {
        let lower = raw.to_ascii_lowercase();
        if let Some(pos) = (1..=6).filter_map(|n| lower.find(&format!("<h{n}")).map(|p| (p, n))).min() {
            let level = pos.1 as u8;
            let label = strip_tags(raw);
            if !label.is_empty() {
                out.push(OutlineEntry { label, line, level });
            }
        }
    }
    out
}

/// Remove `<...>` tags from `s` and collapse whitespace — the visible text of an HTML line.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// XPL routine headers: a `SUB <name>` line (case-insensitive), flat (level 1).
fn xpl_routines(text: &str) -> Vec<OutlineEntry> {
    let mut out = Vec::new();
    for (line, raw) in text.lines().enumerate() {
        let mut it = raw.split_whitespace();
        if it.next().is_some_and(|w| w.eq_ignore_ascii_case("SUB")) {
            if let Some(name) = it.next() {
                out.push(OutlineEntry { label: name.to_string(), line, level: 1 });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(v: &[OutlineEntry]) -> Vec<&str> {
        v.iter().map(|e| e.label.as_str()).collect()
    }

    #[test]
    fn markdown_atx_headings_with_levels_skipping_code_fences() {
        let md = "# Title\n\nintro\n\n## Section A\ntext\n\n```\n# not a heading (in a fence)\n```\n\n### Sub A.1\n#### Deep\n";
        let o = outline(md, RenderKind::Gfm);
        assert_eq!(labels(&o), vec!["Title", "Section A", "Sub A.1", "Deep"]);
        assert_eq!(o[0].level, 1);
        assert_eq!(o[1].level, 2);
        assert_eq!(o[3].level, 4);
        // The fenced `# not a heading` is skipped.
        assert!(!labels(&o).iter().any(|l| l.contains("not a heading")));
    }

    #[test]
    fn atx_ignores_hash_without_space_and_trailing_hashes() {
        let md = "#notaheading\n\n## Closed ##\n";
        let o = outline(md, RenderKind::Gfm);
        assert_eq!(labels(&o), vec!["Closed"]); // trailing #'s trimmed, no-space form ignored
    }

    #[test]
    fn typst_equals_headings() {
        let typ = "= Chapter 1\n\nbody\n\n== Part A\n=== Detail\n";
        let o = outline(typ, RenderKind::Typst);
        assert_eq!(labels(&o), vec!["Chapter 1", "Part A", "Detail"]);
        assert_eq!(o[2].level, 3);
    }

    #[test]
    fn html_h_tags() {
        let html = "<h1>Intro</h1>\n<p>text</p>\n<h2 class=\"x\">Details</h2>\n";
        let o = outline(html, RenderKind::Html);
        assert_eq!(labels(&o), vec!["Intro", "Details"]);
        assert_eq!(o[1].level, 2);
    }

    #[test]
    fn xpl_sub_routines() {
        let xpl = "SUB greet\n  PR \"hi\"\nENDSUB\n\nSUB Compute\n  RETURN 1\nES\n";
        let o = outline(xpl, RenderKind::Text);
        assert_eq!(labels(&o), vec!["greet", "Compute"]);
        assert!(o.iter().all(|e| e.level == 1));
    }

    #[test]
    fn empty_when_no_structure() {
        assert!(outline("just some prose\nwith no headings\n", RenderKind::Text).is_empty());
        assert!(outline("plain text\nno hashes\n", RenderKind::Gfm).is_empty());
    }
}
