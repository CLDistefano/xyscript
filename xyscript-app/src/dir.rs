//! Directory enumeration for the `DIR`/`LS` file picker (QN B10, cut 1).
//!
//! Headless and side-effect-free apart from the one `read_dir`: it turns a directory
//! into an ordered [`DirEntry`] list and a one-entry-per-line listing string, so the
//! editor can show the listing in an ordinary buffer where **line N maps to entry N**
//! (the browser state in [`crate::document`] relies on that alignment). Sorting is
//! **directories first, then files**, each case-insensitively by name; a `..` parent
//! entry leads the list whenever `base` has a parent.

use std::cmp::Ordering;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use xyscript_xpl::{DirOnly, DirOpts, DirOrder};

/// One row of a directory listing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirEntry {
    /// Display name. Normally the bare file/dir name; under `/S` it's the path **relative
    /// to the listing's base** (so the buffer's line N → entry N mapping survives). `..`
    /// for the synthesized parent entry.
    pub name: String,
    /// Absolute path the entry resolves to (the target to open / descend into).
    pub path: PathBuf,
    /// Whether the entry is a directory (drives the trailing-`\` marker and whether
    /// activating it re-lists vs opens a file).
    pub is_dir: bool,
    /// File size in bytes (0 for directories and `..`).
    pub size: u64,
    /// Last-modified time as Unix seconds (UTC), or `None` when unavailable (and for `..`).
    pub modified: Option<i64>,
    /// Whether the entry has the hidden attribute (Windows; always false elsewhere).
    pub hidden: bool,
    /// Whether the entry has the system attribute (Windows; always false elsewhere).
    pub system: bool,
}

/// Read the (hidden, system) attribute bits from metadata — Windows only; `(false, false)`
/// on other platforms (no equivalent attribute concept yet).
fn attrs_of(meta: &std::fs::Metadata) -> (bool, bool) {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let a = meta.file_attributes();
        (a & 0x2 != 0, a & 0x4 != 0) // FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        (false, false)
    }
}

/// Build a [`DirEntry`] for `path` with the given display `name` and known `is_dir`.
/// Size/mtime/attributes are best-effort (a metadata failure leaves them blank/false).
fn make_entry(name: String, path: PathBuf, is_dir: bool) -> DirEntry {
    let meta = std::fs::metadata(&path).ok();
    let size = if is_dir { 0 } else { meta.as_ref().map(|m| m.len()).unwrap_or(0) };
    let modified = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    let (hidden, system) = meta.as_ref().map(attrs_of).unwrap_or((false, false));
    DirEntry { name, path, is_dir, size, modified, hidden, system }
}

/// The `..` parent entry pointing at `parent`.
fn parent_entry(parent: &Path) -> DirEntry {
    DirEntry { name: "..".to_string(), path: parent.to_path_buf(), is_dir: true, ..DirEntry::default() }
}

/// Enumerate `base`: a `..` parent (when one exists) followed by its entries. Ordering is
/// the caller's job ([`sort_entries`]). The single `read_dir` can fail (missing dir,
/// permissions); that error is the caller's to surface.
pub fn list_dir(base: &Path) -> io::Result<Vec<DirEntry>> {
    let mut out = Vec::new();
    if let Some(parent) = base.parent() {
        out.push(parent_entry(parent));
    }
    for entry in std::fs::read_dir(base)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or_else(|_| path.is_dir());
        out.push(make_entry(name, path, is_dir));
    }
    Ok(out)
}

/// Like [`list_dir`] but walks `base` recursively (`/S`): every descendant is one entry,
/// its `name` the path relative to `base`. Symlinked directories are listed but not
/// descended into (avoids cycles). A read error on a *sub*directory is skipped; only a
/// failure to read `base` itself surfaces.
pub fn list_dir_recursive(base: &Path) -> io::Result<Vec<DirEntry>> {
    let mut out = Vec::new();
    if let Some(parent) = base.parent() {
        out.push(parent_entry(parent));
    }
    // Read `base` first so a bad base path is a real error (the rest is best-effort).
    let mut pending = vec![std::fs::read_dir(base)?];
    while let Some(rd) = pending.pop() {
        for entry in rd.flatten() {
            let path = entry.path();
            let ft = entry.file_type().ok();
            let is_dir = ft.map(|t| t.is_dir()).unwrap_or_else(|| path.is_dir());
            let is_link = ft.map(|t| t.is_symlink()).unwrap_or(false);
            let name = path.strip_prefix(base).unwrap_or(&path).to_string_lossy().into_owned();
            out.push(make_entry(name, path.clone(), is_dir));
            if is_dir && !is_link {
                if let Ok(rd) = std::fs::read_dir(&path) {
                    pending.push(rd);
                }
            }
        }
    }
    Ok(out)
}

/// Case-insensitive DOS-style wildcard match: `*` matches any run (including empty), `?`
/// matches exactly one character; every other character is literal. (No `[...]` classes —
/// `DIR` filespecs don't use them.) Linear two-pointer scan with `*` backtracking.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let s: Vec<char> = name.to_lowercase().chars().collect();
    let (mut pi, mut si) = (0, 0);
    let (mut star, mut after_star) = (None, 0); // last `*` position + where it resumed
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            after_star = si;
            pi += 1;
        } else if let Some(sp) = star {
            // Mismatch under a `*`: let the `*` swallow one more char and retry.
            pi = sp + 1;
            after_star += 1;
            si = after_star;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// The basename (final path component) of `name` — what a filespec matches, so `/S`'s
/// relative-path names still match on the file name (`DIR /S *.txt` → files named `*.txt`).
fn basename(name: &str) -> &str {
    name.rsplit(['\\', '/']).next().unwrap_or(name)
}

/// Keep entries whose **basename** matches the `pattern` glob, always preserving the `..`
/// parent (so a filtered listing is still navigable upward).
pub fn filter_entries(entries: Vec<DirEntry>, pattern: &str) -> Vec<DirEntry> {
    entries.into_iter().filter(|e| e.name == ".." || glob_match(pattern, basename(&e.name))).collect()
}

/// Apply the `/A` attribute filter (dirs/files + hidden/system visibility), always keeping
/// the `..` parent. Hidden/system entries are dropped unless `opts` opts them in (`/AH`,
/// `/AS`, or `/A`).
pub fn apply_attr(entries: &mut Vec<DirEntry>, opts: &DirOpts) {
    entries.retain(|e| {
        if e.name == ".." {
            return true;
        }
        match opts.only {
            DirOnly::Dirs if !e.is_dir => return false,
            DirOnly::Files if e.is_dir => return false,
            _ => {}
        }
        if e.hidden && !opts.show_hidden {
            return false;
        }
        if e.system && !opts.show_system {
            return false;
        }
        true
    });
}

/// The lowercased extension of a name (empty if none) — the `/OE` sort key.
fn ext_of(name: &str) -> String {
    basename(name).rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default()
}

/// Sort `entries` by `order` (reversed by `reverse`). The `..` parent stays pinned at the
/// top. When `group_dirs` (the non-recursive default), directories always group before
/// files regardless of key/reverse; under `/S` it's false so a pure name (relative-path)
/// sort groups entries by folder instead. Stable, so ties keep the prior order.
pub fn sort_entries(entries: &mut [DirEntry], order: DirOrder, reverse: bool, group_dirs: bool) {
    let start = usize::from(entries.first().is_some_and(|e| e.name == ".."));
    entries[start..].sort_by(|a, b| {
        if group_dirs {
            match (a.is_dir, b.is_dir) {
                (true, false) => return Ordering::Less,
                (false, true) => return Ordering::Greater,
                _ => {}
            }
        }
        let by_name = || a.name.to_lowercase().cmp(&b.name.to_lowercase());
        let o = match order {
            DirOrder::Name => by_name(),
            DirOrder::Size => a.size.cmp(&b.size).then_with(by_name),
            DirOrder::Date => a.modified.cmp(&b.modified).then_with(by_name),
            DirOrder::Ext => ext_of(&a.name).cmp(&ext_of(&b.name)).then_with(by_name),
        };
        if reverse {
            o.reverse()
        } else {
            o
        }
    });
}

/// The display name of an entry: directories carry a trailing `\` so they read as folders.
fn display_name(e: &DirEntry) -> String {
    if e.is_dir {
        format!("{}\\", e.name)
    } else {
        e.name.clone()
    }
}

/// Max stem characters shown before truncation kicks in (the "first 30 chars").
const NAME_STEM_BUDGET: usize = 30;

/// Truncate a name to `[first NAME_STEM_BUDGET chars of the stem]…[.ext]` when the stem is longer —
/// so the extension (file type) stays visible and one huge name can't blow out the column layout.
/// A dotfile (leading `.`) or extension-less name truncates as a whole.
fn truncate_name(name: &str) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]), // ext includes the dot
        _ => (name, ""),
    };
    if stem.chars().count() <= NAME_STEM_BUDGET {
        return name.to_string();
    }
    let head: String = stem.chars().take(NAME_STEM_BUDGET).collect();
    format!("{head}…{ext}")
}

/// The columnar-listing display name: [`truncate_name`] applied, with the dir `\` re-appended. Used
/// by the in-tab listing and the `DIR/W` window so both truncate consistently.
pub fn render_name(e: &DirEntry) -> String {
    if e.is_dir {
        format!("{}\\", truncate_name(&e.name))
    } else {
        truncate_name(&e.name)
    }
}

/// Render `entries`, one per line, so the buffer's line count equals `entries.len()` and
/// **line N is `entries[N]`** (the browser relies on this). `bare` gives the compact
/// names-only view; otherwise each line is `name  size  date` in aligned columns
/// (directories show `<DIR>`; the date is `YYYY-MM-DD HH:MM`, local). No trailing newline.
pub fn render_listing(entries: &[DirEntry], bare: bool) -> String {
    if bare {
        return entries.iter().map(display_name).collect::<Vec<_>>().join("\n");
    }

    let names: Vec<String> = entries.iter().map(render_name).collect();
    let sizes: Vec<String> = entries
        .iter()
        .map(|e| if e.is_dir { "<DIR>".to_string() } else { e.size.to_string() })
        .collect();
    let dates: Vec<String> =
        entries.iter().map(|e| e.modified.map(format_local).unwrap_or_default()).collect();
    let name_w = names.iter().map(|s| s.chars().count()).max().unwrap_or(0);
    let size_w = sizes.iter().map(|s| s.chars().count()).max().unwrap_or(0);

    (0..entries.len())
        .map(|i| {
            // name left-aligned, size right-aligned, then date; trailing pad trimmed.
            format!("{:<name_w$}  {:>size_w$}  {}", names[i], sizes[i], dates[i]).trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The local UTC offset in seconds (east positive), applied to file timestamps for
/// display. Default 0 = UTC, so headless tests are deterministic; the live binaries set
/// it once at startup via [`set_local_offset`].
static LOCAL_OFFSET_SECS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// Set the local UTC offset (seconds east of UTC) used to display modification times and
/// the date/time specials. Call once at startup with [`system_local_offset_secs`].
pub fn set_local_offset(secs: i64) {
    LOCAL_OFFSET_SECS.store(secs, std::sync::atomic::Ordering::Relaxed);
}

/// The configured local UTC offset in seconds (east of UTC); 0 (UTC) until set.
pub fn local_offset_secs() -> i64 {
    LOCAL_OFFSET_SECS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The host system's current local UTC offset in seconds (east positive). Windows reads
/// it from the OS (`GetTimeZoneInformation`, DST-aware); elsewhere it's UTC (0) for now.
pub fn system_local_offset_secs() -> i64 {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct SystemTime {
            _f: [u16; 8],
        }
        #[repr(C)]
        struct Tzi {
            bias: i32,
            standard_name: [u16; 32],
            standard_date: SystemTime,
            standard_bias: i32,
            daylight_name: [u16; 32],
            daylight_date: SystemTime,
            daylight_bias: i32,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn GetTimeZoneInformation(tzi: *mut Tzi) -> u32;
        }
        // SAFETY: a plain out-param FFI into a zeroed POD struct; no aliasing or lifetimes.
        unsafe {
            let mut tzi: Tzi = std::mem::zeroed();
            let r = GetTimeZoneInformation(&mut tzi);
            // UTC = local + (bias + active_bias) minutes ⇒ offset(east) = -(bias+active)*60s.
            // r == 2 (TIME_ZONE_ID_DAYLIGHT) ⇒ daylight bias is active; else standard.
            let active = if r == 2 { tzi.daylight_bias } else { tzi.standard_bias };
            -((tzi.bias + active) as i64) * 60
        }
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// Format a file's modification time (Unix seconds, UTC) for display, shifting by the
/// configured local offset. See [`civil_format`].
fn format_local(utc_secs: i64) -> String {
    civil_format(utc_secs + LOCAL_OFFSET_SECS.load(std::sync::atomic::Ordering::Relaxed))
}

/// Format Unix seconds as `YYYY-MM-DD HH:MM` via pure integer civil-date math (Howard
/// Hinnant's algorithm) — no date crate. Interprets `secs` as already-offset wall-clock
/// seconds (the caller applies any timezone shift).
fn civil_format(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (hh, mm) = ((tod / 3600) as u32, ((tod % 3600) / 60) as u32);
    // civil_from_days: days since 1970-01-01 → (year, month, day).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{day:02} {hh:02}:{mm:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a unique temp directory with the given child dirs/files, returning its path.
    /// (Named by `tag` + the test thread's address to avoid cross-test collisions without
    /// needing a clock or RNG.)
    fn scratch_dir(tag: &str, dirs: &[&str], files: &[&str]) -> PathBuf {
        let marker = format!("{:p}", &tag);
        let base = std::env::temp_dir().join(format!("xyscript-dir-{tag}-{}", marker.trim_start_matches("0x")));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        for d in dirs {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        for f in files {
            std::fs::write(base.join(f), b"x").unwrap();
        }
        base
    }

    #[test]
    fn dirs_first_then_files_each_sorted_case_insensitively() {
        let base = scratch_dir("order", &["Zeta", "alpha"], &["readme.txt", "Build.rs"]);
        let mut entries = list_dir(&base).unwrap();
        sort_entries(&mut entries, DirOrder::Name, false, true); // the default DIR sort
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        // ".." first, then the two dirs (alpha < Zeta, case-insensitive), then the two
        // files (Build.rs < readme.txt, case-insensitive).
        assert_eq!(names, vec!["..", "alpha", "Zeta", "Build.rs", "readme.txt"]);
        // Directory flags line up with the dir entries (and `..`).
        assert!(entries[0].is_dir && entries[1].is_dir && entries[2].is_dir);
        assert!(!entries[3].is_dir && !entries[4].is_dir);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn wide_render_marks_directories_and_aligns_lines_to_entries() {
        let base = scratch_dir("render", &["sub"], &["a.txt"]);
        let mut entries = list_dir(&base).unwrap();
        sort_entries(&mut entries, DirOrder::Name, false, true);
        // Bare (names-only): one line per entry, dirs trailing-`\`, no trailing newline.
        let text = render_listing(&entries, true);
        assert_eq!(text, "..\\\nsub\\\na.txt");
        assert_eq!(text.split('\n').count(), entries.len());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn columnar_render_aligns_name_size_date_one_line_per_entry() {
        // Controlled entries so size/date columns are deterministic.
        let entries = vec![
            DirEntry { name: "..".into(), path: "/".into(), is_dir: true, ..DirEntry::default() },
            DirEntry { name: "sub".into(), path: "/d/sub".into(), is_dir: true, ..DirEntry::default() },
            // 1970-01-01 00:00 UTC and a 7-byte file.
            DirEntry { name: "a.txt".into(), path: "/d/a.txt".into(), size: 7, modified: Some(0), ..DirEntry::default() },
        ];
        let text = render_listing(&entries, false);
        let lines: Vec<&str> = text.split('\n').collect();
        // Still one line per entry (the browser's line N → entry N invariant).
        assert_eq!(lines.len(), entries.len());
        // name left-aligned to the widest name ("..\\","sub\\","a.txt" → width 5);
        // size right-aligned ("<DIR>","<DIR>","7" → width 5); date or blank.
        assert_eq!(lines[0], "..\\    <DIR>"); // no date → trailing pad trimmed
        assert_eq!(lines[1], "sub\\   <DIR>");
        assert_eq!(lines[2], "a.txt      7  1970-01-01 00:00");
    }

    #[test]
    fn truncate_name_keeps_extension_and_handles_edge_cases() {
        // Short stem: untouched.
        assert_eq!(truncate_name("notes.txt"), "notes.txt");
        // Long stem (>30): first 30 chars of the stem + … + extension.
        let long = "a".repeat(40);
        assert_eq!(truncate_name(&format!("{long}.rs")), format!("{}….rs", "a".repeat(30)));
        // Exactly 30-char stem: still untouched (boundary).
        assert_eq!(truncate_name(&format!("{}.md", "b".repeat(30))), format!("{}.md", "b".repeat(30)));
        // Extension-less long name: truncated as a whole, no dot.
        assert_eq!(truncate_name(&"c".repeat(40)), format!("{}…", "c".repeat(30)));
        // Dotfile (leading dot, no real stem): rfind('.') at index 0 → treated whole.
        let dotfile = format!(".{}", "d".repeat(40));
        assert_eq!(truncate_name(&dotfile), format!(".{}…", "d".repeat(29)));
        // render_name re-appends the dir `\` after truncating the stem.
        let dir = DirEntry { name: long.clone(), is_dir: true, ..DirEntry::default() };
        assert_eq!(render_name(&dir), format!("{}…\\", "a".repeat(30)));
    }

    #[test]
    fn glob_matching_handles_star_question_and_case() {
        assert!(glob_match("*.txt", "notes.txt"));
        assert!(glob_match("*.txt", "a.b.txt"));
        assert!(!glob_match("*.txt", "notes.md"));
        assert!(glob_match("a?.rs", "a1.rs"));
        assert!(!glob_match("a?.rs", "a12.rs"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("READ*", "readme.md")); // case-insensitive
        assert!(glob_match("note?.*", "Notes.TXT"));
        assert!(!glob_match("x*y", "xabc")); // must end in y
        assert!(glob_match("x*y", "xaby"));
    }

    #[test]
    fn filter_keeps_parent_and_matches() {
        let mk = |name: &str, is_dir: bool| DirEntry {
            name: name.into(),
            path: format!("/d/{name}").into(),
            is_dir,
            ..DirEntry::default()
        };
        let entries = vec![mk("..", true), mk("a.txt", false), mk("b.md", false), mk("sub", true)];
        let kept = filter_entries(entries, "*.txt");
        let names: Vec<&str> = kept.iter().map(|e| e.name.as_str()).collect();
        // `..` always kept; only a.txt matches *.txt.
        assert_eq!(names, vec!["..", "a.txt"]);
    }

    #[test]
    fn sort_and_only_helpers() {
        let mk = |name: &str, is_dir: bool, size: u64, modified: i64| DirEntry {
            name: name.into(),
            path: format!("/d/{name}").into(),
            is_dir,
            size,
            modified: Some(modified),
            ..DirEntry::default()
        };
        // `..`, dirs z & a, files big.bin(100), a.txt(10), m.md(50) — deliberately unsorted.
        let base = vec![
            DirEntry { name: "..".into(), path: "/".into(), is_dir: true, ..DirEntry::default() },
            mk("z", true, 0, 1),
            mk("a", true, 0, 2),
            mk("big.bin", false, 100, 5),
            mk("a.txt", false, 10, 9),
            mk("m.md", false, 50, 3),
        ];
        let names = |e: &[DirEntry]| e.iter().map(|x| x.name.clone()).collect::<Vec<_>>();

        // By name: `..`, dirs (a,z), files (a.txt,big.bin,m.md).
        let mut e = base.clone();
        sort_entries(&mut e, DirOrder::Name, false, true);
        assert_eq!(names(&e), ["..", "a", "z", "a.txt", "big.bin", "m.md"]);

        // By size: dirs first (tie→name a,z), files ascending by size.
        let mut e = base.clone();
        sort_entries(&mut e, DirOrder::Size, false, true);
        assert_eq!(names(&e), ["..", "a", "z", "a.txt", "m.md", "big.bin"]);

        // Size reversed: `..` pinned, dirs stay first (within-group reversed), files desc.
        let mut e = base.clone();
        sort_entries(&mut e, DirOrder::Size, true, true);
        assert_eq!(names(&e), ["..", "z", "a", "big.bin", "m.md", "a.txt"]);

        // By date: dirs by mtime (z=1,a=2), files by mtime (m=3,big=5,a.txt=9).
        let mut e = base.clone();
        sort_entries(&mut e, DirOrder::Date, false, true);
        assert_eq!(names(&e), ["..", "z", "a", "m.md", "big.bin", "a.txt"]);

        // group_dirs=false (the /S sort) ignores dir-first grouping: pure name order.
        let mut e = base.clone();
        sort_entries(&mut e, DirOrder::Name, false, false);
        assert_eq!(names(&e), ["..", "a", "a.txt", "big.bin", "m.md", "z"]);

        // /AD dirs-only and /AF files-only both keep `..`.
        let dirs_only = DirOpts { only: DirOnly::Dirs, ..DirOpts::default() };
        let files_only = DirOpts { only: DirOnly::Files, ..DirOpts::default() };
        let mut e = base.clone();
        sort_entries(&mut e, DirOrder::Name, false, true);
        apply_attr(&mut e, &dirs_only);
        assert_eq!(names(&e), ["..", "a", "z"]);
        let mut e = base.clone();
        sort_entries(&mut e, DirOrder::Name, false, true);
        apply_attr(&mut e, &files_only);
        assert_eq!(names(&e), ["..", "a.txt", "big.bin", "m.md"]);
    }

    #[test]
    fn apply_attr_hides_hidden_and_system_by_default() {
        let mk = |name: &str, hidden: bool, system: bool| DirEntry {
            name: name.into(),
            path: format!("/d/{name}").into(),
            hidden,
            system,
            ..DirEntry::default()
        };
        let base = vec![
            DirEntry { name: "..".into(), path: "/".into(), is_dir: true, ..DirEntry::default() },
            mk("normal.txt", false, false),
            mk("secret.txt", true, false),
            mk("driver.sys", false, true),
        ];
        let names = |e: &[DirEntry]| e.iter().map(|x| x.name.clone()).collect::<Vec<_>>();

        // Default: hidden + system are dropped (DOS-style); `..` always kept.
        let mut e = base.clone();
        apply_attr(&mut e, &DirOpts::default());
        assert_eq!(names(&e), ["..", "normal.txt"]);

        // /AH reveals hidden.
        let mut e = base.clone();
        apply_attr(&mut e, &DirOpts { show_hidden: true, ..DirOpts::default() });
        assert_eq!(names(&e), ["..", "normal.txt", "secret.txt"]);

        // /A (show all) reveals both hidden and system.
        let mut e = base.clone();
        apply_attr(&mut e, &DirOpts { show_hidden: true, show_system: true, ..DirOpts::default() });
        assert_eq!(names(&e), ["..", "normal.txt", "secret.txt", "driver.sys"]);
    }

    #[test]
    fn civil_date_formatting_is_correct() {
        assert_eq!(civil_format(0), "1970-01-01 00:00");
        // 2021-01-01 00:00:00 = 1609459200.
        assert_eq!(civil_format(1_609_459_200), "2021-01-01 00:00");
        // Add 13h37m to check time-of-day + a mid-year date: 2021-07-15 13:37.
        assert_eq!(civil_format(1_626_356_220), "2021-07-15 13:37");
        // Negative offset crosses midnight backward: 2021-01-01 00:00 − 1h = 2020-12-31 23:00.
        assert_eq!(civil_format(1_609_459_200 - 3600), "2020-12-31 23:00");
    }

    #[test]
    fn parent_entry_present_and_points_up() {
        let base = scratch_dir("parent", &["child"], &[]);
        let entries = list_dir(&base).unwrap();
        assert_eq!(entries[0].name, "..");
        assert_eq!(entries[0].path, base.parent().unwrap());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn missing_directory_is_an_error() {
        let base = std::env::temp_dir().join("xyscript-dir-does-not-exist-zzz");
        let _ = std::fs::remove_dir_all(&base);
        assert!(list_dir(&base).is_err());
    }
}
