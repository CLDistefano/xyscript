//! Built-in `GREP` — a ripgrep-style recursive text search over `$CWD`, built on the ripgrep
//! libraries (`grep-regex`/`grep-searcher`/`ignore`) rather than reinvented by hand. Results land in
//! a read-only tab as `path:line:col: text` lines — one hit per line, so **line N → `hits[N]`**, the
//! same mapping a `DIR` listing uses — and the `GJ` verb opens the hit under the caret and jumps to
//! it. See host.rs (`EditorOp::Grep` / `EditorOp::GrepJump`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
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

/// A summary of a completed (or cancelled) [`search`].
pub struct GrepSummary {
    pub files_searched: usize,
    /// True if the hit cap was reached (results truncated).
    pub capped: bool,
}

/// Build the matcher for a `GREP` (regular expression when `regex`, else a literal — escaped); `case`
/// is case-insensitive, `whole` adds word boundaries. Separated out so callers can validate a pattern
/// up front (see [`compile_check`]).
fn build_matcher(pattern: &str, regex: bool, case: bool, whole: bool) -> Result<RegexMatcher, String> {
    let body = if regex { pattern.to_string() } else { regex::escape(pattern) };
    RegexMatcherBuilder::new()
        .case_insensitive(case)
        .word(whole)
        .build(&body)
        .map_err(|e| format!("invalid pattern: {e}"))
}

/// Validate a `GREP` pattern without searching — so a bad `/R` regex errors immediately (before a
/// results tab or worker thread is created).
pub fn compile_check(pattern: &str, regex: bool, case: bool, whole: bool) -> Result<(), String> {
    build_matcher(pattern, regex, case, whole).map(|_| ())
}

/// Streaming recursive search under `base` (respecting `.gitignore`, skipping hidden and binary
/// files). Each match is handed to `emit`, which returns `false` to STOP early (e.g. the receiver of a
/// streaming channel went away); `cancel` is checked between files so a new `GREP`/close aborts
/// promptly; `cap` bounds the total. Same matching semantics as [`run_grep`], which is this plus a
/// collector. Runs on the caller's thread — the host runs it on a worker (see host.rs).
pub fn search<F: FnMut(GrepHit) -> bool>(
    base: &Path,
    pattern: &str,
    regex: bool,
    case: bool,
    whole: bool,
    mask: Option<&str>,
    cap: usize,
    cancel: &AtomicBool,
    mut emit: F,
) -> Result<GrepSummary, String> {
    let matcher = build_matcher(pattern, regex, case, whole)?;
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(0)) // skip binary files (stop at the first NUL byte)
        .build();

    let mut emitted = 0usize;
    let mut files_searched = 0usize;
    let mut stopped = false;

    for result in WalkBuilder::new(base).build() {
        if stopped || cancel.load(Ordering::Relaxed) {
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
        let mut sink = HitSink {
            path,
            matcher: &matcher,
            emit: &mut emit,
            emitted: &mut emitted,
            cap,
            stopped: &mut stopped,
        };
        // A read error on one file (permissions, vanished mid-walk) isn't fatal — skip it.
        let _ = searcher.search_path(&matcher, path, &mut sink);
    }
    Ok(GrepSummary { files_searched, capped: emitted >= cap })
}

/// Run `GREP` synchronously, collecting every hit into a `GrepResult` (used by tests and any
/// non-streaming caller). Wraps [`search`] with a never-cancelling flag and a push collector.
pub fn run_grep(
    base: &Path,
    pattern: &str,
    regex: bool,
    case: bool,
    whole: bool,
    mask: Option<&str>,
    cap: usize,
) -> Result<GrepResult, String> {
    let cancel = AtomicBool::new(false);
    let mut hits: Vec<GrepHit> = Vec::new();
    let summary = search(base, pattern, regex, case, whole, mask, cap, &cancel, |h| {
        hits.push(h);
        true
    })?;
    Ok(GrepResult { hits, capped: summary.capped, files_searched: summary.files_searched })
}

/// Emits hits from one file's search via `emit`, stopping (via `stopped`) at `cap` or when `emit`
/// returns `false`.
struct HitSink<'a, F: FnMut(GrepHit) -> bool> {
    path: &'a Path,
    matcher: &'a RegexMatcher,
    emit: &'a mut F,
    emitted: &'a mut usize,
    cap: usize,
    stopped: &'a mut bool,
}

impl<F: FnMut(GrepHit) -> bool> Sink for HitSink<'_, F> {
    type Error = std::io::Error;
    fn matched(
        &mut self,
        _searcher: &grep_searcher::Searcher,
        mat: &SinkMatch<'_>,
    ) -> Result<bool, std::io::Error> {
        if *self.emitted >= self.cap {
            *self.stopped = true;
            return Ok(false);
        }
        let line = mat.line_number().unwrap_or(0) as usize;
        let bytes = mat.bytes();
        // Column = byte offset of the first match within the line (1-based). `find` re-runs the matcher
        // on the line; a miss (shouldn't happen on a matched line) falls back to column 1.
        let col = self.matcher.find(bytes).ok().flatten().map(|m| m.start()).unwrap_or(0) + 1;
        let text = String::from_utf8_lossy(bytes).trim_end_matches(['\r', '\n']).to_string();
        let hit = GrepHit { path: self.path.to_path_buf(), line, col, text };
        *self.emitted += 1;
        if !(self.emit)(hit) || *self.emitted >= self.cap {
            *self.stopped = true;
            return Ok(false);
        }
        Ok(true)
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
        assert!(compile_check("(unclosed", true, false, false).is_err());
        assert!(compile_check("ok", true, false, false).is_ok());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn search_streams_respects_cancel_and_emit_stop() {
        let d = tmpdir();
        fs::write(d.join("a.txt"), "cat\ncat\ncat\ncat\n").unwrap();

        // A cancel flag set before the walk yields nothing.
        let cancelled = AtomicBool::new(true);
        let mut got = 0;
        search(&d, "cat", false, false, false, None, 100, &cancelled, |_| {
            got += 1;
            true
        })
        .unwrap();
        assert_eq!(got, 0, "a pre-set cancel flag stops the search before any file");

        // `emit` returning false stops after the first hit.
        let go = AtomicBool::new(false);
        let mut got = 0;
        search(&d, "cat", false, false, false, None, 100, &go, |_| {
            got += 1;
            false // stop immediately
        })
        .unwrap();
        assert_eq!(got, 1, "emit=false stops after the first hit");

        let _ = fs::remove_dir_all(&d);
    }
}
