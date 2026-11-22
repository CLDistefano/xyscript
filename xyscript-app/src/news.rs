//! Release notes — the "New in {ver}" text.
//!
//! The curated [`docs/NEWS.md`](../../docs/NEWS.md) is baked into the binary at compile time (the
//! same `include_str!` pattern as the manual and reference), so the build always carries exactly its
//! own release notes — no download, no shipped asset, no overwrite-guard interaction. At runtime we
//! pull out the `## v{version}` section for the running version to show in a one-time read-only tab
//! after an update, and on demand from `Help ▸ What's new`.
//!
//! When a release is cut, `packaging/gen-news.py` scaffolds a fresh `## v{ver}` section from the
//! merged pull requests since the last tag; the author trims it before tagging, and the build bakes
//! whatever was committed.

/// The curated release notes, baked in at compile time.
pub const NEWS_MD: &str = include_str!("../../docs/NEWS.md");

/// The body of the `## v{version}` section of `md`, if present: every line between that heading and
/// the next `## ` heading (or end of file), trimmed of surrounding blank lines. A `None` means the
/// file carries no notes for that version (a section may be absent, or present but empty).
///
/// The match tolerates a leading `v`/`V` on either side, so `"0.1.17"` and `"v0.1.17"` both find a
/// `## v0.1.17` heading. Sub-headings (`### …`) and body text stay in the section; only a top-level
/// `## ` heading ends it.
pub fn section_for(md: &str, version: &str) -> Option<String> {
    let want = normalize(version);
    let mut body: Vec<&str> = Vec::new();
    let mut in_section = false;
    for line in md.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            if in_section {
                break; // the next top-level section heading ends ours
            }
            if normalize(rest) == want {
                in_section = true;
            }
            continue; // never keep a `## ` heading line in the body
        }
        if in_section {
            body.push(line);
        }
    }
    if !in_section {
        return None;
    }
    let text = body.join("\n");
    let text = text.trim_matches('\n');
    if text.trim().is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// The release notes for the currently-running version, if the baked `NEWS.md` carries a section.
pub fn current() -> Option<String> {
    section_for(NEWS_MD, crate::update::CURRENT_VERSION)
}

/// Lower-case, trim, drop one leading `v`/`V` — so version strings and `## ` headings compare equal
/// however they spell the `v` prefix.
fn normalize(s: &str) -> String {
    s.trim().trim_start_matches(['v', 'V']).to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# Title\n\nintro\n\n## v2.0.0\n- new thing\n- another\n\n## v1.0.0\n- old thing\n";

    #[test]
    fn extracts_the_requested_section_only() {
        let s = section_for(SAMPLE, "2.0.0").unwrap();
        assert_eq!(s, "- new thing\n- another");
        // Stops at the next `## ` heading — the older section is not bled in.
        assert!(!s.contains("old thing"));
    }

    #[test]
    fn tolerates_a_v_prefix_on_either_side() {
        assert!(section_for(SAMPLE, "v2.0.0").is_some());
        assert!(section_for(SAMPLE, "2.0.0").is_some());
        assert_eq!(section_for(SAMPLE, "v1.0.0").unwrap(), "- old thing");
    }

    #[test]
    fn missing_version_is_none() {
        assert!(section_for(SAMPLE, "9.9.9").is_none());
    }

    #[test]
    fn an_empty_section_is_none() {
        let md = "## v3.0.0\n\n## v2.0.0\n- x\n";
        assert!(section_for(md, "3.0.0").is_none(), "a heading with no body yields None");
    }

    #[test]
    fn subheadings_stay_in_the_body() {
        let md = "## v1.2.3\n- top\n### Details\n- nested\n## v1.0.0\n- old\n";
        let s = section_for(md, "1.2.3").unwrap();
        assert!(s.contains("### Details") && s.contains("- nested") && !s.contains("old"));
    }

    #[test]
    fn the_baked_notes_carry_the_upcoming_section() {
        // The real NEWS.md must stay parseable and carry the section this release documents.
        assert!(section_for(NEWS_MD, "0.1.17").is_some(), "NEWS.md must document v0.1.17");
    }
}
