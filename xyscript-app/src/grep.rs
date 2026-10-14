//! Built-in `GREP` — a ripgrep-style recursive text search over `$CWD`, built on the ripgrep
//! libraries (`grep-regex`/`grep-searcher`/`ignore`) rather than reinvented by hand. Results land in
//! a read-only tab as `path:line:col: text` lines — one hit per line, so **line N → `hits[N]`**, the
//! same mapping a `DIR` listing uses — and the `GJ` verb opens the hit under the caret and jumps to
//! it. See host.rs (`EditorOp::Grep` / `EditorOp::GrepJump`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use grep_matcher::{LineTerminator, Match, Matcher, NoCaptures, NoError};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::WalkBuilder;

/// What a rendered `GREP` line is: a real match, a leading/trailing CONTEXT line (`GREPBEFORE`/
/// `GREPAFTER`), or a blank SEPARATOR between result groups (`GREPBLANK`). Kept per line so the
/// `line N → hits[N]` mapping holds and `GJ` can tell a jumpable line from a blank.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitKind {
    Match,
    Context,
    Separator,
}

/// One rendered line of `GREP` output: for a `Match`/`Context` line, the absolute file path plus its
/// 1-based line and (byte) column and the line's text (trailing newline stripped); a `Separator` is a
/// blank line (empty path, line 0).
#[derive(Debug, Clone)]
pub struct GrepHit {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
    pub text: String,
    pub kind: HitKind,
    /// For an `ALLTABS`/`ALLBUFS` search (in-memory open buffers), the raw id of the source document,
    /// so `GJ` switches to that exact live tab instead of (re)opening `path` from disk — which would
    /// miss unsaved edits and can't resolve an untitled buffer. `None` for a filesystem search.
    pub source: Option<u32>,
}

impl GrepHit {
    /// A blank separator line (`GREPBLANK`), which `GJ` skips.
    fn separator() -> GrepHit {
        GrepHit {
            path: PathBuf::new(),
            line: 0,
            col: 0,
            text: String::new(),
            kind: HitKind::Separator,
            source: None,
        }
    }
}

/// One open buffer to search for an `ALLTABS`/`ALLBUFS` `GREP`: its display `label` (file path, or an
/// untitled name), the source document's raw `id` (for `GJ` to switch to the live tab), and a snapshot
/// of its in-memory bytes (the raw encoded form, exactly what a disk search of the same file would see
/// — but reflecting unsaved edits). Snapshotted on the main thread, then searched on the worker.
pub struct GrepSource {
    pub label: PathBuf,
    pub id: u32,
    pub bytes: Vec<u8>,
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
    /// Whether `pattern` used the Perl-compatible engine (`/P`).
    pub perl: bool,
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

/// Build the linear-engine matcher for a `GREP` (regular expression when `regex`, else a literal —
/// escaped); `case` is case-insensitive, `whole` adds word boundaries. Separated out so callers can
/// validate a pattern up front (see [`compile_check`]).
fn build_matcher(
    pattern: &str,
    regex: bool,
    case: bool,
    whole: bool,
) -> Result<grep_regex::RegexMatcher, String> {
    let body = if regex { pattern.to_string() } else { regex::escape(pattern) };
    RegexMatcherBuilder::new()
        .case_insensitive(case)
        .word(whole)
        .build(&body)
        .map_err(|e| format!("invalid pattern: {e}"))
}

/// Per-line backtracking budget for a `GREP/P` (fancy-regex) match. GREP runs on a worker thread over
/// a whole tree, so a catastrophic pattern must fail fast per line rather than stall the search; a line
/// that trips the limit is treated as a non-match. Below `fancy-regex`'s 1,000,000 default.
const GREP_BACKTRACK_LIMIT: usize = 100_000;

/// Build the Perl-compatible (`fancy-regex`) matcher for a `GREP/P` — look-around, backreferences,
/// atomic groups — under a backtrack limit. `case` is case-insensitive; `whole` wraps the pattern in
/// word boundaries. Always a regex (there is no `/P` literal mode — plain `GREP` covers literals).
fn build_fancy(pattern: &str, case: bool, whole: bool) -> Result<FancyMatcher, String> {
    let mut body = if whole { format!(r"\b(?:{pattern})\b") } else { pattern.to_string() };
    // Case-insensitivity via an inline `(?i)` prefix (the same approach as `text::Pattern::build`),
    // which applies regardless of the builder's own flag handling.
    if case {
        body = format!("(?i){body}");
    }
    let re = fancy_regex::RegexBuilder::new(&body)
        .backtrack_limit(GREP_BACKTRACK_LIMIT)
        .build()
        .map_err(|e| format!("invalid pattern: {e}"))?;
    Ok(FancyMatcher { re })
}

/// A [`grep_matcher::Matcher`] adapter over a `fancy-regex` pattern, so a `GREP/P` search runs through
/// the very same `grep-searcher` pipeline (recursive walk, binary detection, context lines, blank
/// separators, line/column reporting) as the linear engine — only the matching is swapped.
///
/// Matching is per line: [`line_terminator`](FancyMatcher::line_terminator) is `\n` (the searcher's
/// own default), so `grep-searcher` feeds a line at a time WITHOUT its terminator and the pattern never
/// sees a newline — the same line-scoped model as `HLRULE/P`. `grep-matcher` assumes UTF-8/ASCII, so a
/// non-UTF-8 line (rare — binary files are skipped) yields no match, as does a match that exhausts the
/// backtrack limit; both degrade to "no hit" rather than erroring the search (`type Error = NoError`).
struct FancyMatcher {
    re: fancy_regex::Regex,
}

impl Matcher for FancyMatcher {
    type Captures = NoCaptures;
    type Error = NoError;

    fn find_at(&self, haystack: &[u8], at: usize) -> Result<Option<Match>, NoError> {
        let Ok(text) = std::str::from_utf8(haystack) else {
            return Ok(None); // non-UTF-8 slice: grep's UTF-8 assumption — treat as no match
        };
        // `at` arrives on a char boundary (prior match ends), but round up defensively so
        // `find_from_pos` never panics on a mid-codepoint start.
        let mut pos = at.min(text.len());
        while pos < text.len() && !text.is_char_boundary(pos) {
            pos += 1;
        }
        // A backtrack-limit overrun (or any engine error) → no match, so one runaway line can't fail
        // the whole tree walk.
        match self.re.find_from_pos(text, pos) {
            Ok(Some(m)) => Ok(Some(Match::new(m.start(), m.end()))),
            _ => Ok(None),
        }
    }

    fn new_captures(&self) -> Result<NoCaptures, NoError> {
        Ok(NoCaptures::new())
    }

    fn line_terminator(&self) -> Option<LineTerminator> {
        Some(LineTerminator::byte(b'\n'))
    }
}

/// Validate a `GREP` pattern without searching — so a bad `/R` or `/P` regex errors immediately
/// (before a results tab or worker thread is created).
pub fn compile_check(
    pattern: &str,
    regex: bool,
    case: bool,
    whole: bool,
    perl: bool,
) -> Result<(), String> {
    if perl {
        build_fancy(pattern, case, whole).map(|_| ())
    } else {
        build_matcher(pattern, regex, case, whole).map(|_| ())
    }
}

/// Streaming recursive search under `base` (respecting `.gitignore`, skipping hidden and binary
/// files). Each match is handed to `emit`, which returns `false` to STOP early (e.g. the receiver of a
/// streaming channel went away); `cancel` is checked between files so a new `GREP`/close aborts
/// promptly; `cap` bounds the total. Same matching semantics as [`run_grep`], which is this plus a
/// collector. Runs on the caller's thread — the host runs it on a worker (see host.rs).
#[allow(clippy::too_many_arguments)]
pub fn search<F: FnMut(GrepHit) -> bool>(
    base: &Path,
    pattern: &str,
    regex: bool,
    case: bool,
    whole: bool,
    perl: bool,
    mask: Option<&str>,
    cap: usize,
    before: usize,
    after: usize,
    blank: bool,
    cancel: &AtomicBool,
    emit: F,
) -> Result<GrepSummary, String> {
    let searcher = SearcherBuilder::new()
        .line_number(true)
        .before_context(before) // `GREPBEFORE` leading context lines
        .after_context(after) // `GREPAFTER` trailing context lines
        .binary_detection(BinaryDetection::quit(0)) // skip binary files (stop at the first NUL byte)
        .build();

    // `/P` swaps in the fancy-regex matcher; everything else — the walk, binary skip, context, blank
    // separators — is identical, so the walk is generic over the matcher engine.
    if perl {
        let matcher = build_fancy(pattern, case, whole)?;
        Ok(walk(base, &matcher, searcher, mask, cap, before, after, blank, cancel, emit))
    } else {
        let matcher = build_matcher(pattern, regex, case, whole)?;
        Ok(walk(base, &matcher, searcher, mask, cap, before, after, blank, cancel, emit))
    }
}

/// The recursive walk + per-file search shared by both engines. Generic over the [`Matcher`] so the
/// linear (`grep-regex`) and Perl-compatible ([`FancyMatcher`]) paths run the identical pipeline.
#[allow(clippy::too_many_arguments)]
fn walk<M: Matcher, F: FnMut(GrepHit) -> bool>(
    base: &Path,
    matcher: &M,
    mut searcher: Searcher,
    mask: Option<&str>,
    cap: usize,
    before: usize,
    after: usize,
    blank: bool,
    cancel: &AtomicBool,
    mut emit: F,
) -> GrepSummary {
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
        let started = emitted > 0; // whether any prior file emitted a line (for cross-file GREPBLANK)
        let mut sink = HitSink {
            path,
            source: None, // filesystem search — no source document
            matcher,
            emit: &mut emit,
            emitted: &mut emitted,
            cap,
            stopped: &mut stopped,
            before,
            after,
            blank,
            emitted_any: started,
        };
        // A read error on one file (permissions, vanished mid-walk) isn't fatal — skip it.
        let _ = searcher.search_path(matcher, path, &mut sink);
    }
    GrepSummary { files_searched, capped: emitted >= cap }
}

/// Like [`search`] but over the in-memory bytes of OPEN BUFFERS (`GREP … ALLTABS`/`ALLBUFS`) instead
/// of a filesystem walk. Each [`GrepSource`] is searched with `search_slice`; hits carry the source
/// document id so `GJ` jumps to the live tab. Same matcher/searcher pipeline as the file search, so
/// flags, context lines and blank separators behave identically.
#[allow(clippy::too_many_arguments)]
pub fn search_slices<F: FnMut(GrepHit) -> bool>(
    sources: &[GrepSource],
    pattern: &str,
    regex: bool,
    case: bool,
    whole: bool,
    perl: bool,
    cap: usize,
    before: usize,
    after: usize,
    blank: bool,
    cancel: &AtomicBool,
    emit: F,
) -> Result<GrepSummary, String> {
    let searcher = SearcherBuilder::new()
        .line_number(true)
        .before_context(before)
        .after_context(after)
        .binary_detection(BinaryDetection::quit(0))
        .build();
    if perl {
        let matcher = build_fancy(pattern, case, whole)?;
        Ok(walk_slices(sources, &matcher, searcher, cap, before, after, blank, cancel, emit))
    } else {
        let matcher = build_matcher(pattern, regex, case, whole)?;
        Ok(walk_slices(sources, &matcher, searcher, cap, before, after, blank, cancel, emit))
    }
}

/// The per-buffer search shared by both engines for [`search_slices`] (mirrors [`walk`] but over
/// in-memory [`GrepSource`] slices rather than a directory walk).
#[allow(clippy::too_many_arguments)]
fn walk_slices<M: Matcher, F: FnMut(GrepHit) -> bool>(
    sources: &[GrepSource],
    matcher: &M,
    mut searcher: Searcher,
    cap: usize,
    before: usize,
    after: usize,
    blank: bool,
    cancel: &AtomicBool,
    mut emit: F,
) -> GrepSummary {
    let mut emitted = 0usize;
    let mut files_searched = 0usize;
    let mut stopped = false;
    for src in sources {
        if stopped || cancel.load(Ordering::Relaxed) {
            break;
        }
        files_searched += 1;
        let started = emitted > 0;
        let mut sink = HitSink {
            path: &src.label,
            source: Some(src.id),
            matcher,
            emit: &mut emit,
            emitted: &mut emitted,
            cap,
            stopped: &mut stopped,
            before,
            after,
            blank,
            emitted_any: started,
        };
        // An in-memory search can't fail on I/O; a matcher error on one buffer just skips it.
        let _ = searcher.search_slice(matcher, &src.bytes, &mut sink);
    }
    GrepSummary { files_searched, capped: emitted >= cap }
}

/// Run `GREP` synchronously, collecting every hit into a `GrepResult` (used by tests and any
/// non-streaming caller). Wraps [`search`] with a never-cancelling flag and a push collector.
#[allow(clippy::too_many_arguments)]
pub fn run_grep(
    base: &Path,
    pattern: &str,
    regex: bool,
    case: bool,
    whole: bool,
    perl: bool,
    mask: Option<&str>,
    cap: usize,
    before: usize,
    after: usize,
    blank: bool,
) -> Result<GrepResult, String> {
    let cancel = AtomicBool::new(false);
    let mut hits: Vec<GrepHit> = Vec::new();
    let summary = search(
        base, pattern, regex, case, whole, perl, mask, cap, before, after, blank, &cancel, |h| {
            hits.push(h);
            true
        },
    )?;
    Ok(GrepResult { hits, capped: summary.capped, files_searched: summary.files_searched })
}

/// Emits hits from one file's search via `emit`, stopping (via `stopped`) at `cap` or when `emit`
/// returns `false`.
struct HitSink<'a, M: Matcher, F: FnMut(GrepHit) -> bool> {
    path: &'a Path,
    /// Source document id for an `ALLTABS` buffer search (stamped on each hit for `GJ`); `None` for a
    /// filesystem search.
    source: Option<u32>,
    matcher: &'a M,
    emit: &'a mut F,
    emitted: &'a mut usize,
    cap: usize,
    stopped: &'a mut bool,
    /// `GREPBEFORE`/`GREPAFTER` context sizes and the `GREPBLANK` toggle.
    before: usize,
    after: usize,
    blank: bool,
    /// Whether any line has been emitted yet (across files), so `GREPBLANK` puts a blank BETWEEN
    /// results, not before the first.
    emitted_any: bool,
}

impl<M: Matcher, F: FnMut(GrepHit) -> bool> HitSink<'_, M, F> {
    /// Push one rendered line through `emit`, bumping the counter; returns `false` (and sets
    /// `stopped`) at the cap or when the collector asks to stop.
    fn push(&mut self, hit: GrepHit) -> bool {
        *self.emitted += 1;
        self.emitted_any = true;
        if !(self.emit)(hit) || *self.emitted >= self.cap {
            *self.stopped = true;
            return false;
        }
        true
    }
    /// The whole line's text, trailing newline stripped (kept in full — no clipping — since the
    /// paragraph-formatted reference texts are meant to be read entire; the render layer handles
    /// long lines).
    fn line_text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).trim_end_matches(['\r', '\n']).to_string()
    }
}

impl<M: Matcher, F: FnMut(GrepHit) -> bool> Sink for HitSink<'_, M, F> {
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
        // With no context, `GREPBLANK` still separates individual matches (ripgrep only fires
        // `context_break` when context is enabled), so insert the blank before this match here.
        if self.blank && self.before == 0 && self.after == 0 && self.emitted_any && !self.push(GrepHit::separator())
        {
            return Ok(false);
        }
        let line = mat.line_number().unwrap_or(0) as usize;
        let bytes = mat.bytes();
        // Column = byte offset of the first match within the line (1-based). `find` re-runs the matcher
        // on the line; a miss (shouldn't happen on a matched line) falls back to column 1.
        let col = self.matcher.find(bytes).ok().flatten().map(|m| m.start()).unwrap_or(0) + 1;
        let hit = GrepHit {
            path: self.path.to_path_buf(),
            line,
            col,
            text: Self::line_text(bytes),
            kind: HitKind::Match,
            source: self.source,
        };
        Ok(self.push(hit))
    }

    fn context(
        &mut self,
        _searcher: &grep_searcher::Searcher,
        ctx: &SinkContext<'_>,
    ) -> Result<bool, std::io::Error> {
        if *self.emitted >= self.cap {
            *self.stopped = true;
            return Ok(false);
        }
        // A leading/trailing context line (both rendered the same); `Other`/passthru kinds show as
        // context too.
        let hit = GrepHit {
            path: self.path.to_path_buf(),
            line: ctx.line_number().unwrap_or(0) as usize,
            col: 1,
            text: Self::line_text(ctx.bytes()),
            kind: HitKind::Context,
            source: self.source,
        };
        Ok(self.push(hit))
    }

    fn context_break(&mut self, _searcher: &grep_searcher::Searcher) -> Result<bool, std::io::Error> {
        // ripgrep marks the gap between non-contiguous context groups; with `GREPBLANK` on, that's
        // where the blank line goes. (Only fires when context is enabled — the no-context case is
        // handled in `matched`.)
        if self.blank && self.emitted_any {
            return Ok(self.push(GrepHit::separator()));
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
        match h.kind {
            HitKind::Separator => out.push('\n'), // a blank line (GREPBLANK)
            HitKind::Match => {
                let rel = h.path.strip_prefix(base).unwrap_or(h.path.as_path());
                out.push_str(&format!("{}:{}:{}: {}\n", rel.display(), h.line, h.col, h.text));
            }
            // Context lines use grep's `-` separators (not `:`) so they read as context, not matches.
            HitKind::Context => {
                let rel = h.path.strip_prefix(base).unwrap_or(h.path.as_path());
                out.push_str(&format!("{}-{}- {}\n", rel.display(), h.line, h.text));
            }
        }
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
    fn search_slices_searches_in_memory_buffers_and_stamps_source_ids() {
        // `GREP … ALLTABS`: search open buffers in memory (no filesystem), each hit tagged with its
        // source document id so `GJ` can switch to the live tab.
        let sources = vec![
            GrepSource { label: PathBuf::from("one.txt"), id: 7, bytes: b"hello cat world\n".to_vec() },
            GrepSource { label: PathBuf::from("two.txt"), id: 9, bytes: b"no feline here\n".to_vec() },
            GrepSource { label: PathBuf::from("Untitled"), id: 42, bytes: b"a\nanother cat sits\n".to_vec() },
        ];
        let cancel = AtomicBool::new(false);
        let mut hits = Vec::new();
        let summary = search_slices(
            &sources, "cat", false, false, false, false, 1000, 0, 0, false, &cancel,
            |h| {
                hits.push(h);
                true
            },
        )
        .unwrap();
        assert_eq!(summary.files_searched, 3, "every source is searched");
        let matches: Vec<_> = hits.iter().filter(|h| h.kind == HitKind::Match).collect();
        assert_eq!(matches.len(), 2, "two buffers contain 'cat'");
        assert_eq!(matches[0].source, Some(7));
        assert_eq!(matches[0].path, PathBuf::from("one.txt"));
        assert_eq!(matches[0].line, 1);
        assert_eq!(matches[0].col, 7); // "hello " then "cat"
        assert_eq!(matches[1].source, Some(42), "untitled buffer tagged by its doc id");
        assert_eq!(matches[1].line, 2);
    }

    #[test]
    fn grep_keeps_the_whole_matched_line() {
        // Paragraph-formatted sources: the full (long) matched line is kept verbatim — no clipping —
        // so results stay discursive and the first hit is never hidden.
        let d = tmpdir();
        let long = format!("MATCH {}", "the quick brown fox ".repeat(3000)); // ~60 KB line
        fs::write(d.join("wide.txt"), format!("{long}\n")).unwrap();
        let r = run_grep(&d, "MATCH", false, false, false, false, None, 100, 0, 0, false).unwrap();
        assert_eq!(r.hits.len(), 1);
        assert_eq!(r.hits[0].text, long); // full line kept verbatim (only the \n is stripped)
        assert!(r.hits[0].text.len() > 60_000, "the whole ~60 KB line is present");
        assert_eq!(r.hits[0].col, 1);
    }

    #[test]
    fn grep_literal_mask_regex_case_and_whole_word() {
        let d = tmpdir();
        fs::write(d.join("a.txt"), "hello world\nfoo BAR baz\ncat\n").unwrap();
        fs::write(d.join("b.rs"), "let x = 42;\nfn cat() {}\n").unwrap();
        fs::write(d.join("c.txt"), "concatenate\ncat\n").unwrap();

        // Literal, case-sensitive: "cat" as a substring → a.txt:3, b.rs:2, c.txt:1 (concatenate), c.txt:2.
        let r = run_grep(&d, "cat", false, false, false, false, None, 100, 0, 0, false).unwrap();
        assert_eq!(r.hits.len(), 4, "literal substring matches");

        // Whole-word drops the "cat" inside "concatenate".
        let r = run_grep(&d, "cat", false, false, true, false, None, 100, 0, 0, false).unwrap();
        assert_eq!(r.hits.len(), 3, "whole-word excludes concatenate");
        assert!(r.hits.iter().all(|h| h.text != "concatenate"));

        // File-name mask.
        let r = run_grep(&d, "cat", false, false, false, false, Some("*.rs"), 100, 0, 0, false).unwrap();
        assert_eq!(r.hits.len(), 1);
        assert!(r.hits[0].path.ends_with("b.rs"));
        assert_eq!(r.hits[0].line, 2);
        assert_eq!(r.hits[0].col, 4, "col is the 1-based byte column of the match (fn |cat)");

        // Regex.
        let r = run_grep(&d, r"\d+", true, false, false, false, None, 100, 0, 0, false).unwrap();
        assert_eq!(r.hits.len(), 1);
        assert!(r.hits[0].text.contains("42"));

        // Case-insensitive literal.
        let r = run_grep(&d, "bar", false, true, false, false, None, 100, 0, 0, false).unwrap();
        assert_eq!(r.hits.len(), 1);
        assert!(r.hits[0].text.contains("BAR"));

        // Cap truncates.
        let r = run_grep(&d, "cat", false, false, false, false, None, 2, 0, 0, false).unwrap();
        assert_eq!(r.hits.len(), 2);
        assert!(r.capped);

        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn context_lines_and_blank_separators() {
        let d = tmpdir();
        // Two matches, each with neighbours, so before/after context has something to show.
        fs::write(d.join("a.txt"), "l1\nl2\nMATCH-a\nl4\nl5\nl6\nMATCH-b\nl8\n").unwrap();

        // 1 before + 1 after around each match: b(l2) match(l3) a(l4)  b(l6) match(l7) a(l8).
        let r = run_grep(&d, "MATCH", false, false, false, false, None, 100, 1, 1, false).unwrap();
        let kinds: Vec<HitKind> = r.hits.iter().map(|h| h.kind).collect();
        use HitKind::*;
        assert_eq!(kinds, vec![Context, Match, Context, Context, Match, Context], "1-before/1-after");
        assert_eq!(r.hits[1].text, "MATCH-a");
        assert_eq!(r.hits[0].text, "l2"); // leading context
        assert_eq!(r.hits[2].text, "l4"); // trailing context

        // GREPBLANK inserts a Separator between the two groups (context_break).
        let r = run_grep(&d, "MATCH", false, false, false, false, None, 100, 1, 1, true).unwrap();
        assert_eq!(r.hits.iter().filter(|h| h.kind == HitKind::Separator).count(), 1);
        // The separator sits between the two match groups.
        let sep = r.hits.iter().position(|h| h.kind == HitKind::Separator).unwrap();
        assert!(sep > 0 && sep < r.hits.len() - 1);

        // With NO context, GREPBLANK still separates the two matches with one blank.
        let r = run_grep(&d, "MATCH", false, false, false, false, None, 100, 0, 0, true).unwrap();
        let kinds: Vec<HitKind> = r.hits.iter().map(|h| h.kind).collect();
        assert_eq!(kinds, vec![Match, Separator, Match], "blank between matches, none leading");

        // render_hits: matches use `:`, context uses `-`, separators are blank lines.
        let r = run_grep(&d, "MATCH", false, false, false, false, None, 100, 1, 0, true).unwrap();
        let text = render_hits(&r.hits, &d);
        assert!(text.contains("a.txt:3:1: MATCH-a"), "match line: {text}");
        assert!(text.contains("a.txt-2- l2"), "context line uses dashes: {text}");
        assert!(text.contains("\n\n"), "a blank separator line is present: {text:?}");

        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn bad_regex_is_an_error() {
        let d = tmpdir();
        fs::write(d.join("a.txt"), "x\n").unwrap();
        assert!(run_grep(&d, "(unclosed", true, false, false, false, None, 100, 0, 0, false).is_err());
        assert!(compile_check("(unclosed", true, false, false, false).is_err());
        assert!(compile_check("ok", true, false, false, false).is_ok());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn perl_engine_does_lookaround_and_backreferences() {
        let d = tmpdir();
        fs::write(d.join("a.txt"), "foobar\nfoobaz\ngreet\nhello\n").unwrap();
        fs::write(d.join("b.rs"), "let user = 1;\n").unwrap();

        // The perl flag (6th arg) with a lookahead — impossible on the linear engine, which the
        // compile_check confirms rejects it there.
        assert!(compile_check("foo(?=bar)", true, false, false, false).is_err(), "linear /R can't");
        assert!(compile_check("foo(?=bar)", true, false, false, true).is_ok(), "/P can");
        let r = run_grep(&d, "foo(?=bar)", true, false, false, true, None, 100, 0, 0, false).unwrap();
        assert_eq!(r.hits.len(), 1, "matches only the foobar line");
        assert!(r.hits[0].text.contains("foobar"));
        assert_eq!(r.hits[0].line, 1);
        assert_eq!(r.hits[0].col, 1, "col is the 1-based byte column of the match");

        // A backreference (fancy-only) — a doubled letter.
        let r = run_grep(&d, r"(\w)\1", true, false, false, true, None, 100, 0, 0, false).unwrap();
        let lines: Vec<&str> = r.hits.iter().map(|h| h.text.as_str()).collect();
        assert!(lines.contains(&"greet"), "ee in greet"); // e-e
        assert!(lines.contains(&"hello"), "ll in hello"); // l-l
        // "let user = 1;" has no doubled letter, so the backreference doesn't match it.
        assert!(!lines.iter().any(|l| l.contains("user")), "no doubled letter in the b.rs line");

        // /P still honours case-insensitivity and whole-word, and the file-name mask.
        let r = run_grep(&d, "USER", true, true, true, true, Some("*.rs"), 100, 0, 0, false).unwrap();
        assert_eq!(r.hits.len(), 1);
        assert!(r.hits[0].path.ends_with("b.rs"));

        // A bad /P pattern is an error up front (no search).
        assert!(compile_check("foo(?=", false, false, false, true).is_err());

        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn search_streams_respects_cancel_and_emit_stop() {
        let d = tmpdir();
        fs::write(d.join("a.txt"), "cat\ncat\ncat\ncat\n").unwrap();

        // A cancel flag set before the walk yields nothing.
        let cancelled = AtomicBool::new(true);
        let mut got = 0;
        search(&d, "cat", false, false, false, false, None, 100, 0, 0, false, &cancelled, |_| {
            got += 1;
            true
        })
        .unwrap();
        assert_eq!(got, 0, "a pre-set cancel flag stops the search before any file");

        // `emit` returning false stops after the first hit.
        let go = AtomicBool::new(false);
        let mut got = 0;
        search(&d, "cat", false, false, false, false, None, 100, 0, 0, false, &go, |_| {
            got += 1;
            false // stop immediately
        })
        .unwrap();
        assert_eq!(got, 1, "emit=false stops after the first hit");

        let _ = fs::remove_dir_all(&d);
    }
}
