//! COMPILE mode — parse source locations out of build-tool output.
//!
//! The Emacs `compilation-mode` analogue: `COMPILE <cmd>` runs a build asynchronously and streams its
//! output into a read-only *Compile* tab; each line is run through [`CompileMatcher::parse`] to pull
//! out a `file:line:col` location. Lines that carry one become navigable hits (reusing the `GREP`
//! `GrepHit` model and `GJ`/`NERR`/`PERR`); the rest are shown as context.
//!
//! The matcher is a small ordered table of regexes, modelled on `compilation-error-regexp-alist` but
//! covering the common families with three patterns. First match wins. A path may carry a Windows
//! drive letter (`C:\…`), whose colon must not be mistaken for the `:line` separator; the column is
//! optional and defaults to 1.

use regex::Regex;
use std::path::PathBuf;

/// A source location parsed from one output line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub line: usize,
    /// 1-based column (1 when the tool reported none).
    pub col: usize,
}

/// An ordered set of location patterns. [`default`](CompileMatcher::default) builds the built-in
/// table; extra user patterns (a later `COMPILERULE` / `COMPILE/P:`) prepend to it so they win.
pub struct CompileMatcher {
    patterns: Vec<Regex>,
}

impl Default for CompileMatcher {
    fn default() -> Self {
        // `drive` = an optional `X:` Windows drive prefix, kept out of the `:line` split.
        let drive = r"(?:[A-Za-z]:)?";
        let pats = [
            // rustc / cargo: the location sits on an indented `-->` line.
            format!(r"^\s*-->\s+(?P<path>{drive}[^:\s][^:]*):(?P<line>\d+):(?P<col>\d+)"),
            // gcc / clang / Zig / `grep -n`: `path:line[:col]:` at the start of the line.
            format!(r"^(?P<path>{drive}[^:\s][^:]*):(?P<line>\d+)(?::(?P<col>\d+))?:"),
            // MSVC: `path(line[,col])`.
            format!(r"^(?P<path>{drive}[^(:\s][^(]*)\((?P<line>\d+)(?:,(?P<col>\d+))?\)"),
        ];
        CompileMatcher {
            patterns: pats.iter().map(|p| Regex::new(p).expect("built-in COMPILE pattern")).collect(),
        }
    }
}

impl CompileMatcher {
    /// Parse the first location out of `line`, trying each pattern in order. `None` if the line
    /// carries no recognized location (it becomes context in the results tab).
    pub fn parse(&self, line: &str) -> Option<Location> {
        for re in &self.patterns {
            if let Some(c) = re.captures(line) {
                let path = PathBuf::from(&c["path"]);
                let num = c["line"].parse::<usize>().ok()?;
                let col = c.name("col").and_then(|m| m.as_str().parse::<usize>().ok()).unwrap_or(1);
                return Some(Location { path, line: num, col });
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(path: &str, line: usize, col: usize) -> Option<Location> {
        Some(Location { path: PathBuf::from(path), line, col })
    }

    #[test]
    fn zig_diagnostics_and_backtraces_parse() {
        let m = CompileMatcher::default();
        // Zig uses the gcc/clang `file:line:col: error:` form — no Zig-specific pattern needed.
        assert_eq!(m.parse("src/main.zig:10:5: error: expected type 'u8', found 'i32'"), loc("src/main.zig", 10, 5));
        assert_eq!(m.parse("src/main.zig:5:10: note: struct declared here"), loc("src/main.zig", 5, 10));
        assert_eq!(
            m.parse("/home/u/proj/src/parse.zig:88:21: error: use of undeclared identifier 'foo'"),
            loc("/home/u/proj/src/parse.zig", 88, 21)
        );
        // Windows paths: backslash separators, and a drive letter (its colon must not split as :line).
        assert_eq!(m.parse(r"src\win.zig:3:9: error: something"), loc(r"src\win.zig", 3, 9));
        assert_eq!(m.parse(r"C:\dev\app\src\main.zig:42:17: error: oops"), loc(r"C:\dev\app\src\main.zig", 42, 17));
        // A runtime panic backtrace frame shares the prefix, so NERR jumps to the failing source line.
        assert_eq!(m.parse("src/main.zig:10:5: 0x1043f2 in main (test)"), loc("src/main.zig", 10, 5));
    }

    #[test]
    fn other_toolchains_parse() {
        let m = CompileMatcher::default();
        // gcc / clang.
        assert_eq!(m.parse("src/foo.c:42:17: error: 'x' undeclared"), loc("src/foo.c", 42, 17));
        // grep -n (no column): col defaults to 1.
        assert_eq!(m.parse("notes.txt:42:a matching line"), loc("notes.txt", 42, 1));
        // rustc / cargo: indented `-->` line.
        assert_eq!(m.parse("  --> src/main.rs:42:17"), loc("src/main.rs", 42, 17));
        // MSVC: path(line,col).
        assert_eq!(m.parse("foo.c(42,17): error C2065: 'x': undeclared"), loc("foo.c", 42, 17));
        // MSVC with no column.
        assert_eq!(m.parse("foo.c(42): warning C4101"), loc("foo.c", 42, 1));
    }

    #[test]
    fn context_lines_have_no_location() {
        let m = CompileMatcher::default();
        for ctx in [
            "    var x: u8 = y;",                      // source snippet
            "            ^~~~~",                       // caret line
            "thread 51863 panic: integer overflow",   // panic header
            "error: the following command failed:",    // zig build summary
            "referenced by:",                          // ref-trace header
            "",                                         // blank
            "Compiling foo v0.1.0",                     // cargo progress
        ] {
            assert_eq!(m.parse(ctx), None, "should be context: {ctx:?}");
        }
    }
}
