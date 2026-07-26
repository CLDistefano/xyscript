//! Built-in `GREP` — a ripgrep-style recursive text search over `$CWD`, built on the ripgrep
//! libraries (`grep-regex`/`grep-searcher`/`ignore`) rather than reinvented by hand. Results land in
//! a read-only tab as `path:line:col: text` lines — one hit per line, so **line N → `hits[N]`**, the
//! same mapping a `DIR` listing uses — and the `GJ` verb opens the hit under the caret and jumps to
//! it. See host.rs (`EditorOp::Grep` / `EditorOp::GrepJump`).

use std::path::{Path, PathBuf};

use grep_matcher::Matcher;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, SearcherBuilder, Sink, SinkMatch};
use ignore::WalkBuilder;

/// One match: the absolute file path plus its 1-based line and (byte) column, and the matched line's
/// text (trailing newline stripped) for display.
#[derive(Debug, Clone)]
pub struct GrepHit {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
    pub text: String,
}

/// Per-tab state for a `GREP` results listing (analogous to [`crate::document::DirState`]): the search
/// that produced it plus the hits, where the rendered buffer keeps **line N → `hits[N]`** so the `GJ`
/// verb resolves the hit under the caret by its line.
#[derive(Debug, Clone)]
pub struct GrepState {
    /// The search root (`$CWD` at run time) — display paths are relative to it; `GJ` uses each hit's
    /// own absolute path.
    pub base: PathBuf,
    /// The pattern searched (for the tab label / a later re-run).
    pub pattern: String,
    /// Whether `pattern` was a regular expression (`/R`).
    pub regex: bool,
    /// Whether the hit cap was reached (results truncated).
    pub capped: bool,
    /// One hit per buffer line, in display order (line N → `hits[N]`).
    pub hits: Vec<GrepHit>,
}

/// The outcome of a `GREP` run.
pub struct GrepResult {
    pub hits: Vec<GrepHit>,
    /// True if the hit cap was reached (results are truncated).
    pub capped: bool,
    /// How many files were actually searched (for the status line).
    pub files_searched: usize,
}

/// Run `GREP` under `base`: recursively walk (respecting `.gitignore` and skipping hidden files via
/// `ignore`), optionally filter file NAMES by `mask` (a DOS-style glob, e.g. `*.rs`), and collect up
/// to `cap` matches. The pattern is a regular expression when `regex`, else a literal (escaped);
/// `case`/`whole` map to the matcher's case-insensitive / word-boundary options — the same knobs as
/// the FIND family. Phase 1 is synchronous and capped; streaming comes later.
pub fn run_grep(
    base: &Path,
    pattern: &str,
    regex: bool,
    case: bool,
    whole: bool,
    mask: Option<&str>,
    cap: usize,
) -> Result<GrepResult, String> {
    let body = if regex { pattern.to_string() } else { regex::escape(pattern) };
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(case)
        .word(whole)
        .build(&body)
        .map_err(|e| format!("invalid pattern: {e}"))?;
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(0)) // skip binary files (stop at the first NUL byte)
        .build();

    let mut hits: Vec<GrepHit> = Vec::new();
    let mut files_searched = 0usize;

    for result in WalkBuilder::new(base).build() {
        if hits.len() >= cap {
            break;
        }
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue, // unreadable dir/file: skip it, don't abort the whole search
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue; // directories, symlinks-to-dirs, etc.
        }
        let path = entry.path();
        if let Some(m) = mask {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !crate::dir::glob_match(m, name) {
                continue;
            }
        }
        files_searched += 1;
        let mut sink = HitSink { path, matcher: &matcher, hits: &mut hits, cap };
        // A read error on one file (permissions, vanished mid-walk) isn't fatal — skip it.
        let _ = searcher.search_path(&matcher, path, &mut sink);
    }
    let capped = hits.len() >= cap;
    Ok(GrepResult { hits, capped, files_searched })
}

/// Collects hits from one file's search into the shared `hits` vec, stopping at `cap`.
struct HitSink<'a> {
    path: &'a Path,
    matcher: &'a grep_regex::RegexMatcher,
    hits: &'a mut Vec<GrepHit>,
    cap: usize,
}

impl Sink for HitSink<'_> {
    type Error = std::io::Error;
    fn matched(
        &mut self,
        _searcher: &grep_searcher::Searcher,
        mat: &SinkMatch<'_>,
    ) -> Result<bool, std::io::Error> {
        if self.hits.len() >= self.cap {
            return Ok(false); // stop this file's search — the cap is reached
        }
        let line = mat.line_number().unwrap_or(0) as usize;
        let bytes = mat.bytes();
        // Column = byte offset of the first match within the line (1-based). `find` re-runs the matcher
        // on the line; a miss (shouldn't happen on a matched line) falls back to column 1.
        let col = self.matcher.find(bytes).ok().flatten().map(|m| m.start()).unwrap_or(0) + 1;
        let text = String::from_utf8_lossy(bytes).trim_end_matches(['\r', '\n']).to_string();
        self.hits.push(GrepHit { path: self.path.to_path_buf(), line, col, text });
        Ok(self.hits.len() < self.cap)
    }
}

/// Render hits as `relpath:line:col: text` lines (one per hit, so **line N → `hits[N]`**). Paths are
/// shown relative to `base` (the search root, = `$CWD`) for readability; the hit keeps the absolute
/// path for `GJ`.
pub fn render_hits(hits: &[GrepHit], base: &Path) -> String {
    let mut out = String::new();
    for h in hits {
        let rel = h.path.strip_prefix(base).unwrap_or(h.path.as_path());
        out.push_str(&format!("{}:{}:{}: {}\n", rel.display(), h.line, h.col, h.text));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fresh empty temp directory (unique per call, no `rand`/clock needed).
    fn tmpdir() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("xyscript-grep-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn grep_literal_mask_regex_case_and_whole_word() {
        let d = tmpdir();
        fs::write(d.join("a.txt"), "hello world\nfoo BAR baz\ncat\n").unwrap();
        fs::write(d.join("b.rs"), "let x = 42;\nfn cat() {}\n").unwrap();
        fs::write(d.join("c.txt"), "concatenate\ncat\n").unwrap();

        // Literal, case-sensitive: "cat" as a substring → a.txt:3, b.rs:2, c.txt:1 (concatenate), c.txt:2.
        let r = run_grep(&d, "cat", false, false, false, None, 100).unwrap();
        assert_eq!(r.hits.len(), 4, "literal substring matches");

        // Whole-word drops the "cat" inside "concatenate".
        let r = run_grep(&d, "cat", false, false, true, None, 100).unwrap();
        assert_eq!(r.hits.len(), 3, "whole-word excludes concatenate");
        assert!(r.hits.iter().all(|h| h.text != "concatenate"));

        // File-name mask.
        let r = run_grep(&d, "cat", false, false, false, Some("*.rs"), 100).unwrap();
        assert_eq!(r.hits.len(), 1);
        assert!(r.hits[0].path.ends_with("b.rs"));
        assert_eq!(r.hits[0].line, 2);
        assert_eq!(r.hits[0].col, 4, "col is the 1-based byte column of the match (fn |cat)");

        // Regex.
        let r = run_grep(&d, r"\d+", true, false, false, None, 100).unwrap();
        assert_eq!(r.hits.len(), 1);
        assert!(r.hits[0].text.contains("42"));

        // Case-insensitive literal.
        let r = run_grep(&d, "bar", false, true, false, None, 100).unwrap();
        assert_eq!(r.hits.len(), 1);
        assert!(r.hits[0].text.contains("BAR"));

        // Cap truncates.
        let r = run_grep(&d, "cat", false, false, false, None, 2).unwrap();
        assert_eq!(r.hits.len(), 2);
        assert!(r.capped);

        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn bad_regex_is_an_error() {
        let d = tmpdir();
        fs::write(d.join("a.txt"), "x\n").unwrap();
        assert!(run_grep(&d, "(unclosed", true, false, false, None, 100).is_err());
        let _ = fs::remove_dir_all(&d);
    }
}
