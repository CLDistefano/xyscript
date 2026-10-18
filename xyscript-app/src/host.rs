//! The real [`Host`] implementation, backed by the text engine
//! (`development-plan.md` Stage 2: "Connect XPL to the buffer").
//!
//! This is the seam the interpreter calls *out* through. Editor verbs become
//! [`EditorOp`]s that mutate the [`Buffer`]; special variables (`$SE`, `$CP`,
//! `$LN`, …) read live buffer state; `LOADKBD` parses a `.kbd` file and records
//! the new keymap for the [`Editor`](crate::editor::Editor) to install
//! transactionally after the script returns.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use xyscript_kbd::{load, Keymap, LoadOutcome};
use xyscript_xpl::{
    BinderSource, CalcContext, ClipKind, ColorGround, DefineKind, DeleteKind, DirOpts, DocTypeReq, EditorOp,
    ErrCode, HelpResponse, Host, LockKey, MarkGutterMode, MatchQuery, Motion, MsgColorSlot, Number, PersistKind, PvMarginSide, SearchSpec, UiSizeKind, Value,
    ViewFlag, ViewToggle, VmResult, XplError,
};

// Re-exported so the GUI (which doesn't depend on `xyscript-xpl` directly) can match on it.
pub use xyscript_xpl::CaretShape;
pub use xyscript_xpl::UnderlineStyle;
pub use crate::text::BalMatch;
pub use xyscript_xpl::LintFixChoices;
pub use xyscript_xpl::CmlinePlacement;
pub use xyscript_xpl::PathPlace;

use crate::document::{Bookmark, Document};
use crate::encoding::Encoding;
use crate::text::{Buffer, Pattern};

/// The reserved virtual key-code band used when parsing a loaded `.kbd`
/// (matches the embedded default's convention; codes `>= 120` are injection-only).
const VIRTUAL_BASE: Option<u8> = Some(120);

/// Strip Windows' `\\?\` verbatim prefix that `canonicalize` adds, so working-directory
/// paths display and store cleanly. A no-op on other platforms / unprefixed paths.
/// Normalize a Windows drive-qualified path so that naming a drive always switches to it
/// (`CD` == `CD/D`): bare `D:` → `D:\`, drive-relative `D:rel` → `D:\rel`. Returns `None` for an
/// already-rooted `D:\abs`/`D:/abs` (handled as an ordinary absolute path) or any non-drive path.
/// No-op off Windows (drive letters are a Windows concept).
/// Escape a binder's value for a `LDTX`/`LDSC` quoted literal so it round-trips through the
/// quoted-string parser (`literal_text`/lexer): backslash, quote, and the whitespace escapes.
fn escape_binder_literal(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn normalize_drive_path(p: &str) -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let b = p.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        let drive = &p[..1];
        let rest = &p[2..];
        if rest.is_empty() {
            return Some(PathBuf::from(format!("{drive}:\\")));
        }
        if !rest.starts_with('\\') && !rest.starts_with('/') {
            return Some(PathBuf::from(format!("{drive}:\\{rest}")));
        }
    }
    None
}

fn clean_path(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => p,
    }
}

/// The drive letter (upper-cased) of an absolute Windows path `X:\…`, or `None` for a
/// non-drive path (UNC, or off Windows). Used to track a per-drive current directory.
fn drive_of(path: &Path) -> Option<char> {
    let s = path.as_os_str().to_string_lossy();
    let b = s.as_bytes();
    (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
        .then(|| b[0].to_ascii_uppercase() as char)
}

/// The regex matching a numeric token for cursor addition (`ADDSUM`), built once. Accepts an optional
/// leading sign, an optional currency symbol (either order), thousands-separated or plain digits, and an
/// optional decimal part (or a bare `.5`). Currency/separators are stripped by [`clean_number`].
fn number_token_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"[-+]?[$€£¥₩₹]?[-+]?(?:\d[\d,]*(?:\.\d+)?|\.\d+)")
            .expect("number-token regex is valid")
    })
}

/// Normalize a matched number token for parsing: drop currency symbols, spaces, and thousands-separator
/// commas, keeping only the digits and one decimal point, with a leading `-` if the token was negative
/// (a sign anywhere in the prefix before the first digit — `-$5` and `$-5` are both negative).
fn clean_number(raw: &str) -> String {
    let first_digit = raw.find(|c: char| c.is_ascii_digit() || c == '.').unwrap_or(raw.len());
    let negative = raw[..first_digit].contains('-');
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    out.extend(raw.chars().filter(|&c| c.is_ascii_digit() || c == '.'));
    out
}

/// The `MARKGUTTER` word for a mode — the form used on the status line, `$MARKGUTTER`, and SaveCfg.
fn mark_gutter_word(m: MarkGutterMode) -> &'static str {
    match m {
        MarkGutterMode::Off => "off",
        MarkGutterMode::On => "on",
        MarkGutterMode::Persist => "persist",
    }
}

/// A command that is EXACTLY a bare drive letter (`X:` — letter + colon, nothing after) → that
/// drive's upper-cased letter. `None` for `X:\` (explicit root), `X:sub` (drive-relative), or any
/// other path. XyWrite/`cmd.exe`: a bare `X:` switches to that drive's *remembered* directory.
fn bare_drive_letter(p: &str) -> Option<char> {
    let b = p.trim().as_bytes();
    (b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
        .then(|| b[0].to_ascii_uppercase() as char)
}

/// The current directory Windows maintains for drive `letter` (its per-drive cwd), as an absolute
/// path that keeps the drive letter (no SUBST/symlink resolution). Used the FIRST time a session
/// switches to a drive, before it has a remembered directory. `None` if the drive isn't ready or
/// off Windows. `GetFullPathNameW("X:")` expands the drive-relative "X:" against that per-drive cwd.
#[cfg(windows)]
fn os_drive_current_dir(letter: char) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFullPathNameW(
            file: *const u16,
            buf_len: u32,
            buf: *mut u16,
            file_part: *mut *mut u16,
        ) -> u32;
    }
    let input: Vec<u16> = [letter as u16, b':' as u16, 0].to_vec(); // "X:\0"
    let mut buf = [0u16; 260]; // MAX_PATH; plenty for a directory path
    let n = unsafe {
        GetFullPathNameW(input.as_ptr(), buf.len() as u32, buf.as_mut_ptr(), std::ptr::null_mut())
    };
    // 0 = failure; >= buf.len() = truncated (a pathologically deep cwd) — treat both as "unknown".
    if n == 0 || n as usize >= buf.len() {
        return None;
    }
    Some(PathBuf::from(std::ffi::OsString::from_wide(&buf[..n as usize])))
}
#[cfg(not(windows))]
fn os_drive_current_dir(_letter: char) -> Option<PathBuf> {
    None
}

// ---- MDA block-colour sidecar (`.<name>.xymeta`) ----------------------------------------------
// A document's MDA / FGCOLOR block colours live in a hidden companion file next to it, so they
// survive close-and-reopen without touching the document's own bytes. The sidecar is DATA, never
// executed — it can travel alongside arbitrary (even downloaded) files, so auto-running it would be
// a code-execution vector. A content length+hash guard ties it to the exact bytes it was written
// for: if another tool edited the file, the stored offsets no longer mean anything and the sidecar
// is ignored (fail-closed). The body is sectioned (`[colors]`) so other metadata can join later.

type ColorSpan = (usize, usize, (u8, u8, u8));

/// FNV-1a 64-bit — a small, deterministic (version-stable) fingerprint of the saved bytes for the
/// guard. Not a security boundary: a mismatch just means "ignore a stale sidecar," the safe default.
fn xymeta_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The sidecar path for a document at `target`: `dir/.name.ext.xymeta` (leading dot). `None` for a
/// pathless target — an untitled buffer has nothing to anchor a sidecar to.
fn xymeta_path(target: &Path) -> Option<PathBuf> {
    let name = target.file_name()?.to_string_lossy().into_owned();
    let mut p = target.to_path_buf();
    p.set_file_name(format!(".{name}.xymeta"));
    Some(p)
}

/// Best-effort removal of a document's hidden `.xymeta` sidecar when the file itself is deleted, so no
/// orphan metadata is left behind. A no-op when there is no sidecar (and harmless for a directory
/// target — `.<dir>.xymeta` simply won't exist).
fn remove_xymeta_sidecar(target: &Path) {
    if let Some(scp) = xymeta_path(target) {
        let _ = std::fs::remove_file(scp);
    }
}

/// Best-effort: move a document's hidden `.xymeta` sidecar to follow a rename/move of the file, so its
/// MDA styling stays attached (and no orphan is left at the old name). A no-op when the source file
/// has no sidecar. `to` is the FINAL destination path (after any "rename into a directory" join).
fn rename_xymeta_sidecar(from: &Path, to: &Path) {
    if let (Some(src), Some(dst)) = (xymeta_path(from), xymeta_path(to)) {
        if src.exists() {
            let _ = std::fs::remove_file(&dst); // Windows won't rename onto an existing hidden file
            let _ = std::fs::rename(&src, &dst); // keeps the hidden attribute across the move
        }
    }
}

/// Give a file the Windows HIDDEN attribute (a leading dot alone doesn't hide it there). No-op on
/// Unix, where the dot prefix already hides it.
#[cfg(windows)]
fn set_file_hidden(p: &Path) {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn SetFileAttributesW(name: *const u16, attrs: u32) -> i32;
    }
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    let wide: Vec<u16> = p.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    unsafe {
        SetFileAttributesW(wide.as_ptr(), FILE_ATTRIBUTE_HIDDEN);
    }
}
#[cfg(not(windows))]
fn set_file_hidden(_p: &Path) {}

/// Serialize a document's styling to the `.xymeta` sidecar text, keyed to `bytes` (its length+hash
/// guard). `None` when there's nothing to persist. Shared by the on-disk sidecar ([`xymeta_write`])
/// and the transport bundle (`BUNDLE`), so both carry byte-identical metadata.
fn xymeta_serialize(
    bytes: &[u8],
    fg: &[ColorSpan],
    bg: &[ColorSpan],
    attrs: &[(usize, usize, u8)],
    marks: &[(usize, usize, Vec<u8>)],
) -> Option<String> {
    if fg.is_empty() && bg.is_empty() && attrs.is_empty() && marks.is_empty() {
        return None;
    }
    let mut s = String::new();
    s.push_str("xymeta 1\n");
    s.push_str(&format!("len {}\n", bytes.len()));
    s.push_str(&format!("hash {:016x}\n", xymeta_hash(bytes)));
    if !fg.is_empty() || !bg.is_empty() {
        s.push_str("[colors]\n");
        for &(a, b, (r, g, bl)) in fg {
            s.push_str(&format!("fg {a} {b} {r} {g} {bl}\n"));
        }
        for &(a, b, (r, g, bl)) in bg {
            s.push_str(&format!("bg {a} {b} {r} {g} {bl}\n"));
        }
    }
    if !attrs.is_empty() {
        s.push_str("[attrs]\n");
        for &(a, b, bits) in attrs {
            s.push_str(&format!("at {a} {b} {bits}\n"));
        }
    }
    if !marks.is_empty() {
        // `m <off> <split> <ctx-hex>` — the context is hex so it survives line-trim and can't collide
        // with the field separators, whatever raw bytes it holds (any encoding).
        s.push_str("[marks]\n");
        for (off, split, ctx) in marks {
            let hex: String = ctx.iter().map(|b| format!("{b:02x}")).collect();
            s.push_str(&format!("m {off} {split} {hex}\n"));
        }
    }
    Some(s)
}

/// Write (or, with nothing to persist, remove) the sidecar next to `target`, keyed to `bytes`.
/// Best-effort: the caller ignores the outcome, since a sidecar problem must never fail a save.
fn xymeta_write(
    target: &Path,
    bytes: &[u8],
    fg: &[ColorSpan],
    bg: &[ColorSpan],
    attrs: &[(usize, usize, u8)],
    marks: &[(usize, usize, Vec<u8>)],
) {
    let Some(scp) = xymeta_path(target) else { return };
    let Some(s) = xymeta_serialize(bytes, fg, bg, attrs, marks) else {
        // Nothing to persist — drop any stale sidecar so a doc whose styling was cleared doesn't
        // reopen still wearing it.
        let _ = std::fs::remove_file(&scp);
        return;
    };
    // Remove first: on Windows you can't truncate-create over a HIDDEN file (access denied), and
    // this also clears the attribute so the fresh write starts clean — then re-hide it.
    let _ = std::fs::remove_file(&scp);
    if std::fs::write(&scp, s.as_bytes()).is_ok() {
        set_file_hidden(&scp);
    }
}

/// Write a sidecar's already-serialized `text` next to `target` (hidden). Used by `UNBUNDLE` to
/// materialize the metadata member verbatim beside the document it unpacked.
fn xymeta_write_text(target: &Path, text: &str) {
    let Some(scp) = xymeta_path(target) else { return };
    let _ = std::fs::remove_file(&scp);
    if std::fs::write(&scp, text.as_bytes()).is_ok() {
        set_file_hidden(&scp);
    }
}

/// A parsed `.xymeta` sidecar. The colour/attribute spans are only trustworthy when `guard_ok` (the
/// file's bytes still match), but `marks` are ALWAYS returned: a bookmark carries its own text anchor,
/// so it can be relocated against changed bytes (see [`Document::set_marks_restored`]).
struct XymetaRestore {
    guard_ok: bool,
    fg: Vec<ColorSpan>,
    bg: Vec<ColorSpan>,
    attrs: Vec<(usize, usize, u8)>,
    marks: Vec<Bookmark>,
}

/// Read the sidecar next to `target`. Returns `None` only when there's nothing usable (no file, wrong
/// version). Otherwise every section is parsed; `guard_ok` tells the caller whether the byte-guard
/// still matches (so it can trust `fg`/`bg`/`attrs`), while `marks` are returned regardless.
fn xymeta_read(target: &Path, bytes: &[u8]) -> Option<XymetaRestore> {
    let scp = xymeta_path(target)?;
    let text = std::fs::read_to_string(&scp).ok()?;
    xymeta_parse(&text, bytes)
}

/// Parse sidecar `text` (from a file or a bundle's `meta` member) against the document `bytes` it's
/// keyed to. Shared by [`xymeta_read`] and the in-memory `OPEN` of a bundle.
fn xymeta_parse(text: &str, bytes: &[u8]) -> Option<XymetaRestore> {
    let mut lines = text.lines();
    if lines.next()?.trim() != "xymeta 1" {
        return None; // unknown format/version → ignore
    }
    let mut len_ok = false;
    let mut hash_ok = false;
    let mut section = "";
    let (mut fg, mut bg): (Vec<ColorSpan>, Vec<ColorSpan>) = (Vec::new(), Vec::new());
    let mut attrs: Vec<(usize, usize, u8)> = Vec::new();
    let mut marks: Vec<Bookmark> = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(sec) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = match sec {
                "colors" => "colors",
                "attrs" => "attrs",
                "marks" => "marks",
                _ => "", // unknown section: skip its lines
            };
            continue;
        }
        if let Some(v) = line.strip_prefix("len ") {
            len_ok = v.trim().parse::<usize>().ok() == Some(bytes.len());
            continue;
        }
        if let Some(v) = line.strip_prefix("hash ") {
            hash_ok = v.trim().eq_ignore_ascii_case(&format!("{:016x}", xymeta_hash(bytes)));
            continue;
        }
        match section {
            "colors" => {
                if let Some((is_bg, sp)) = parse_xymeta_color(line) {
                    if is_bg {
                        bg.push(sp)
                    } else {
                        fg.push(sp)
                    }
                }
            }
            "attrs" => {
                if let Some(sp) = parse_xymeta_attr(line) {
                    attrs.push(sp);
                }
            }
            "marks" => {
                if let Some(bm) = parse_xymeta_mark(line) {
                    marks.push(bm);
                }
            }
            _ => {}
        }
    }
    Some(XymetaRestore { guard_ok: len_ok && hash_ok, fg, bg, attrs, marks })
}

/// Parse one `[marks]` line: `m <off> <split> <ctx-hex>`. The context hex may be empty (a mark with
/// no surrounding text). Any malformed field drops just that line.
fn parse_xymeta_mark(line: &str) -> Option<Bookmark> {
    let rest = line.strip_prefix("m ")?;
    let mut it = rest.splitn(3, ' ');
    let off: usize = it.next()?.trim().parse().ok()?;
    let split: usize = it.next()?.trim().parse().ok()?;
    let hex = it.next().unwrap_or("").trim();
    if hex.len() % 2 != 0 {
        return None;
    }
    let ctx: Option<Vec<u8>> =
        (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok()).collect();
    Some(Bookmark { off, ctx: ctx?, split, moved: false })
}

/// Apply a parsed sidecar to a freshly-loaded document: colour/attribute spans only when the guard
/// still holds, but bookmarks always (they relocate themselves against the current text).
fn apply_xymeta_restore(doc: &mut Document, r: XymetaRestore) {
    if r.guard_ok {
        doc.set_color_spans_raw(r.fg, r.bg, r.attrs);
    }
    doc.set_marks_restored(r.marks);
}

/// The REFERENCE-matches block for `HELP`/`REF`: the formatted list of entries matching `q` (name
/// hits first, exact headword promoted to the top), or `None` when nothing matches. Shared so `REF`
/// (reference only) and `HELP` (reference + manual) format results identically.
fn reference_block(q: &str) -> Option<String> {
    const CAP: usize = 40; // entries listed before "…and N more"
    let mut hits = crate::help::search_query(q);
    if let Some(exact) = crate::help::lookup(q) {
        hits.retain(|e| !(e.names == exact.names && e.kind == exact.kind));
        hits.insert(0, exact);
    }
    if hits.is_empty() {
        return None;
    }
    let shown = hits.len().min(CAP);
    let mut body = format!("{} reference match{} for {q:?}:\n", hits.len(), if hits.len() == 1 { "" } else { "es" });
    for e in &hits[..shown] {
        // One line of names + kind, then the entry's FULL synopsis (flattened to one line — the tab
        // word-wraps). No character cap / trailing `…`: a result reads as complete text.
        let flat: String = e.synopsis.split_whitespace().collect::<Vec<_>>().join(" ");
        body.push_str(&format!("\n{}  ({})\n    {flat}", e.names.join(" / "), e.kind.label()));
    }
    if hits.len() > shown {
        body.push_str(&format!("\n\n…and {} more — narrow the search term.", hits.len() - shown));
    }
    Some(body)
}

/// The MANUAL-matches block for `HELP`/`MAN`: matching sections' titles + full bodies, or `None`.
fn manual_block(q: &str) -> Option<String> {
    const CAP: usize = 25; // sections listed before "…and N more"
    let hits = crate::manual::search_query(q);
    if hits.is_empty() {
        return None;
    }
    let shown = hits.len().min(CAP);
    let mut body = format!("{} manual section{} for {q:?}:\n", hits.len(), if hits.len() == 1 { "" } else { "s" });
    for s in &hits[..shown] {
        body.push_str(&format!("\n{}\n    {}", s.title, s.body));
    }
    if hits.len() > shown {
        body.push_str(&format!("\n\n…and {} more — narrow the search term.", hits.len() - shown));
    }
    Some(body)
}

/// The miss response when a `HELP`/`REF` query matches nothing: near-name typo suggestions, reported
/// on the status line (not an empty tab).
fn help_miss(q: &str) -> HelpResponse {
    let near = crate::help::suggest(q, 5);
    let body = if near.is_empty() {
        format!("No help matches for {q:?}. Type HELP for an overview.")
    } else {
        format!("No help matches for {q:?} — did you mean: {}?", near.join(", "))
    };
    HelpResponse { body, not_found: true, emphasis: None }
}

/// Human-readable names for an `MDA` attribute bitset (e.g. "bold underline"), for status messages.
fn mda_attr_names(a: u8) -> String {
    let mut parts = Vec::new();
    if a & xyscript_xpl::ATTR_BOLD != 0 {
        parts.push("bold");
    }
    if a & xyscript_xpl::ATTR_ITALIC != 0 {
        parts.push("italic");
    }
    if a & xyscript_xpl::ATTR_UNDERLINE != 0 {
        parts.push("underline");
    }
    if a & xyscript_xpl::ATTR_REVERSE != 0 {
        parts.push("reverse");
    }
    parts.join(" ")
}

/// The compact `MDA`/`HLMODE` keyword(s) for an attribute bitset (`BO IT UL RV`), space-joined — the
/// form `HLMODE` parses back, so `SAVECFG` round-trips a mode. Empty for no bits.
fn mda_attr_keywords(a: u8) -> String {
    let mut parts = Vec::new();
    if a & xyscript_xpl::ATTR_BOLD != 0 {
        parts.push("BO");
    }
    if a & xyscript_xpl::ATTR_ITALIC != 0 {
        parts.push("IT");
    }
    if a & xyscript_xpl::ATTR_UNDERLINE != 0 {
        parts.push("UL");
    }
    if a & xyscript_xpl::ATTR_REVERSE != 0 {
        parts.push("RV");
    }
    parts.join(" ")
}

/// Parse one `at <start> <end> <bits>` attribute line. `None` on any malformed/zero-width/empty field.
fn parse_xymeta_attr(line: &str) -> Option<(usize, usize, u8)> {
    let mut it = line.split_whitespace();
    if it.next()? != "at" {
        return None;
    }
    let s: usize = it.next()?.parse().ok()?;
    let e: usize = it.next()?.parse().ok()?;
    let bits: u8 = it.next()?.parse().ok()?;
    (e > s && bits != 0).then_some((s, e, bits))
}

/// Parse one `fg|bg <start> <end> <r> <g> <b>` colour line. `None` on any malformed/zero-width field.
fn parse_xymeta_color(line: &str) -> Option<(bool, ColorSpan)> {
    let mut it = line.split_whitespace();
    let is_bg = match it.next()? {
        "fg" => false,
        "bg" => true,
        _ => return None,
    };
    let s: usize = it.next()?.parse().ok()?;
    let e: usize = it.next()?.parse().ok()?;
    let r: u8 = it.next()?.parse().ok()?;
    let g: u8 = it.next()?.parse().ok()?;
    let b: u8 = it.next()?.parse().ok()?;
    (e > s).then_some((is_bg, (s, e, (r, g, b))))
}

/// Serialize a document's LIVE `MDA` spans for the session manifest — one tab-free field: `-` when
/// unstyled, else `;`-joined entries `f,s,e,r,g,b` (fg colour) / `b,s,e,r,g,b` (bg colour) /
/// `a,s,e,bits` (attributes). Unlike the `.xymeta` sidecar (which only reflects the last SAVE), this
/// captures the current in-memory styling, so `SESSION` restore preserves UNSAVED styling too.
fn session_spans_field(doc: &Document) -> String {
    let (fg, bg) = doc.color_spans_raw();
    let attr = doc.attr_spans_raw();
    if fg.is_empty() && bg.is_empty() && attr.is_empty() {
        return "-".to_string();
    }
    let mut parts: Vec<String> = Vec::with_capacity(fg.len() + bg.len() + attr.len());
    for &(s, e, (r, g, b)) in fg {
        parts.push(format!("f,{s},{e},{r},{g},{b}"));
    }
    for &(s, e, (r, g, b)) in bg {
        parts.push(format!("b,{s},{e},{r},{g},{b}"));
    }
    for &(s, e, bits) in attr {
        parts.push(format!("a,{s},{e},{bits}"));
    }
    parts.join(";")
}

/// Parse a [`session_spans_field`] back into `(fg, bg, attr)` span lists, each clamped to `len` (a file
/// edited smaller since the snapshot drops out-of-range spans). Malformed entries are skipped.
fn parse_session_spans(
    field: &str,
    len: usize,
) -> (Vec<ColorSpan>, Vec<ColorSpan>, Vec<(usize, usize, u8)>) {
    let (mut fg, mut bg, mut attr) = (Vec::new(), Vec::new(), Vec::new());
    if field == "-" || field.is_empty() {
        return (fg, bg, attr);
    }
    for entry in field.split(';') {
        let mut it = entry.split(',');
        let tag = it.next().unwrap_or("");
        let nums: Vec<usize> = it.filter_map(|n| n.parse().ok()).collect();
        match tag {
            "f" | "b" if nums.len() == 5 => {
                let (s, e) = (nums[0], nums[1].min(len));
                let rgb = (nums[2] as u8, nums[3] as u8, nums[4] as u8);
                if s < e {
                    if tag == "b" {
                        bg.push((s, e, rgb));
                    } else {
                        fg.push((s, e, rgb));
                    }
                }
            }
            "a" if nums.len() == 3 => {
                let (s, e, bits) = (nums[0], nums[1].min(len), nums[2] as u8);
                if s < e && bits != 0 {
                    attr.push((s, e, bits));
                }
            }
            _ => {}
        }
    }
    (fg, bg, attr)
}

/// Make `path` absolute and lexically clean: resolve `.`/`..` by pure path logic (never touching
/// the filesystem) and drop any `\\?\` verbatim prefix. Unlike [`std::fs::canonicalize`], this does
/// NOT resolve symlinks or `subst`ed drives — so a `subst X: C:\dir` path stays `X:\…` instead of
/// being rewritten to its backing `C:\dir\…` (the behavior this replaces). A relative `path` is
/// resolved against `cwd`; an already-absolute `path` ignores `cwd`. A `..` at the root is a no-op
/// (it can't rewind past the drive/root), matching how `cmd.exe`/PowerShell resolve `cd ..`.
fn lexical_abs(cwd: &Path, path: &Path) -> PathBuf {
    use std::path::Component;
    let joined = if path.is_absolute() { path.to_path_buf() } else { cwd.join(path) };
    let mut out = PathBuf::new();
    for comp in clean_path(joined).components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// Virtual-key codes for the lock keys (for `$CAPSLOCK`/`$NUMLOCK`/`$SCROLLLOCK`).
const VK_CAPITAL: i32 = 0x14;
const VK_NUMLOCK: i32 = 0x90;
const VK_SCROLL: i32 = 0x91;

/// Whether a lock key is currently engaged, queried from the OS. Windows uses
/// `GetKeyState`'s low (toggle) bit; other platforms have no portable query, so report off.
#[cfg(windows)]
fn lock_key_on(vk: i32) -> bool {
    #[link(name = "user32")]
    extern "system" {
        fn GetKeyState(n_virt_key: i32) -> i16;
    }
    (unsafe { GetKeyState(vk) } & 1) != 0
}
#[cfg(not(windows))]
fn lock_key_on(_vk: i32) -> bool {
    false
}

/// Set/clear/toggle a lock key by synthesizing a key tap (a press+release toggles the lock).
/// `mode`: `Some(true)` = on, `Some(false)` = off (tap only if the current state differs),
/// `None` = toggle. Returns the resulting state. Windows only; a no-op elsewhere (matching the
/// read side, which can't query lock state off Windows either).
#[cfg(windows)]
fn set_lock_key(vk: i32, mode: Option<bool>) -> bool {
    #[link(name = "user32")]
    extern "system" {
        fn keybd_event(b_vk: u8, b_scan: u8, dw_flags: u32, dw_extra_info: usize);
    }
    const KEYEVENTF_KEYUP: u32 = 0x0002;
    let cur = lock_key_on(vk);
    let want = mode.unwrap_or(!cur);
    if want != cur {
        unsafe {
            keybd_event(vk as u8, 0, 0, 0); // press
            keybd_event(vk as u8, 0, KEYEVENTF_KEYUP, 0); // release → toggles the lock
        }
    }
    want
}
#[cfg(not(windows))]
fn set_lock_key(_vk: i32, mode: Option<bool>) -> bool {
    mode.unwrap_or(false)
}

/// Whether an op mutates the active buffer's *content* (so it must be blocked when the
/// document is read-only). Motion, selection, search-find, view/UI/colour, file/dir ops,
/// and the read-only toggle itself are all non-mutating. `CH`/`CV` (replace) and clipboard
/// cut/paste do mutate; plain `SE` and clipboard copy/append/prepend/set do not.
fn op_mutates_buffer(op: &EditorOp) -> bool {
    match op {
        EditorOp::InsertText(_)
        | EditorOp::ReplaceSelection(_)
        | EditorOp::TypeText(_)
        | EditorOp::Merge(_)
        | EditorOp::Delete(_)
        | EditorOp::RubDelete
        | EditorOp::PasteDeletion
        | EditorOp::DeleteSel
        | EditorOp::Case(_)
        | EditorOp::CopySelection
        | EditorOp::MoveSelection
        | EditorOp::Sort(_)
        | EditorOp::Undo(_)
        | EditorOp::Redo(_)
        | EditorOp::AutoReplace
        | EditorOp::HexSetNibble { .. }
        | EditorOp::HexOverwrite(_)
        | EditorOp::HexInsert(_)
        | EditorOp::HexDelete { .. }
        | EditorOp::HexPaste(_)
        | EditorOp::HexDeleteSelection
        | EditorOp::Newline => true,
        EditorOp::Search(spec) => spec.replace.is_some(),
        EditorOp::Clip(ClipKind::Cut | ClipKind::Paste) => true,
        _ => false,
    }
}

/// The HEX-view editing ops — buffer mutations that ARE allowed while the hex view is on (they are
/// the intended editing). A genuine `RO` lock still blocks them; only the hex read-only gate defers.
fn is_hex_edit_op(op: &EditorOp) -> bool {
    matches!(
        op,
        EditorOp::HexSetNibble { .. }
            | EditorOp::HexOverwrite(_)
            | EditorOp::HexInsert(_)
            | EditorOp::HexDelete { .. }
            | EditorOp::HexPaste(_)
            | EditorOp::HexDeleteSelection
    )
}

/// Return the contents of the Markdown fenced code block that line `cursor_line` sits in
/// (the lines strictly between the opening and closing ``` / ~~~ fences), or `None` if the
/// caret isn't inside a *closed* fenced block. A fence line is one whose trimmed text starts
/// with ``` or ~~~; the caret may be on a fence line or any content line of the block.
fn fenced_block_at(text: &str, cursor_line: usize) -> Option<String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let is_fence = |l: &str| {
        let t = l.trim_start();
        t.starts_with("```") || t.starts_with("~~~")
    };
    let mut i = 0;
    while i < lines.len() {
        if is_fence(lines[i]) {
            let open = i;
            let mut close = open + 1;
            while close < lines.len() && !is_fence(lines[close]) {
                close += 1;
            }
            if close >= lines.len() {
                break; // unterminated fence — no runnable block
            }
            if (open..=close).contains(&cursor_line) {
                return Some(lines[open + 1..close].join("\n"));
            }
            i = close + 1;
        } else {
            i += 1;
        }
    }
    None
}

/// Run `cmd` through the OS shell to completion, returning (merged stdout+stderr, exit
/// code). Blocking — the caller (a trusted `X`/`XY`/`XV`) waits for the command. Uses the
/// shell's own command-line parsing (so pipes/redirects work): `cmd /C <raw>` on Windows,
/// `sh -c <cmd>` elsewhere.
fn run_shell_capture(cmd: &str) -> (String, i64) {
    match shell_command(cmd).output() {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            (s, o.status.code().map(i64::from).unwrap_or(-1))
        }
        Err(e) => (format!("[error] {e}\n"), -1),
    }
}

/// Build the OS-shell command for `cmd`, using the shell's own parsing (so pipes/redirects
/// work): `cmd /C <raw>` on Windows (verbatim via `raw_arg`), `sh -c <cmd>` elsewhere.
fn shell_command(cmd: &str) -> std::process::Command {
    use std::process::Command;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: don't give `cmd` its own console. A GUI-subsystem build (the dist exe)
        // has no console for the child to inherit, so without this `cmd /C <prog.exe>` flashes a
        // throwaway console window before the launched program appears. Output is captured/piped
        // regardless, so nothing is lost. (`X dir` already showed no window — its output is piped.)
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut c = Command::new("cmd");
        c.arg("/C").raw_arg(cmd).creation_flags(CREATE_NO_WINDOW);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    }
}

/// A queued lint request handed to the background worker (a viewport-text snapshot).
#[cfg(feature = "lint")]
struct LintJob {
    seq: u64,
    text: String,
    flavor: crate::lint::Flavor,
    engine: crate::lint::SpellEngine,
    dicts: String,
    bin: String,
}

/// A completed lint result from the worker: byte spans into the requested region text.
#[cfg(feature = "lint")]
struct LintDone {
    seq: u64,
    spans: Vec<crate::lint::LintSpan>,
}

/// The in-flight request's context, kept on the main thread so a returning result can be validated
/// (still the same doc + revision?) and its byte spans rebased to absolute rope offsets.
#[cfg(feature = "lint")]
struct LintPending {
    seq: u64,
    active: usize,
    rev: u64,
    abs_dec_lo: usize,
    lo: usize,
    hi: usize,
}

/// The background language-check worker: harper (the slow neural POS tagger) runs on ITS thread so
/// the UI never blocks. It coalesces bursts — while you scroll/type, many snapshots queue and it
/// skips to the newest — so it's always working on the freshest view, one check at a time.
#[cfg(feature = "lint")]
struct LintWorker {
    tx: std::sync::mpsc::Sender<LintJob>,
    rx: std::sync::mpsc::Receiver<LintDone>,
    _handle: std::thread::JoinHandle<()>,
}

#[cfg(feature = "lint")]
impl LintWorker {
    fn spawn(dialect: crate::lint::Dialect) -> LintWorker {
        let (job_tx, job_rx) = std::sync::mpsc::channel::<LintJob>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<LintDone>();
        let handle = std::thread::spawn(move || {
            let mut engine = crate::lint::LintEngine::new(dialect);
            while let Ok(mut job) = job_rx.recv() {
                // Coalesce a burst: skip to the newest queued request (scrolling/typing floods these).
                while let Ok(newer) = job_rx.try_recv() {
                    job = newer;
                }
                // Harper (built-in) or Hunspell (multilingual CLI spelling). Hunspell is stateless —
                // it spawns a `hunspell` process per job — so the reusable harper `engine` sits idle
                // in Hunspell mode. `hunspell_check` returns no spans if the binary is absent.
                let spans = match job.engine {
                    crate::lint::SpellEngine::Hunspell => {
                        // Mask code/URLs first so hunspell checks only prose (harper's parser does
                        // this itself). Length-preserving, so span offsets stay valid.
                        let prose = crate::lint::mask_markup(&job.text, job.flavor);
                        crate::lint::hunspell_check(&prose, &job.dicts, &job.bin)
                    }
                    crate::lint::SpellEngine::Harper => engine.lint_region(&job.text, job.flavor),
                };
                if done_tx.send(LintDone { seq: job.seq, spans }).is_err() {
                    break; // the host (Receiver) is gone
                }
            }
        });
        LintWorker { tx: job_tx, rx: done_rx, _handle: handle }
    }
}

/// Build the whole-document `CHECK` report (one line per issue, honouring the user dictionary). A
/// FREE function of plain data so it can run on a background thread (harper over a big doc is slow).
#[cfg(feature = "lint")]
#[allow(clippy::too_many_arguments)]
fn build_check_report(
    text: &str,
    flavor: crate::lint::Flavor,
    dialect: crate::lint::Dialect,
    engine: crate::lint::SpellEngine,
    dicts: &str,
    bin: &str,
    learned: &std::collections::BTreeSet<String>,
) -> String {
    // Same engine split as the live underlines: Hunspell gives multilingual SPELLING issues; Harper
    // gives English spelling + grammar. (Hunspell mode is spelling-only — it's a speller, not a
    // grammar checker.)
    let spans = match engine {
        crate::lint::SpellEngine::Hunspell => {
            crate::lint::hunspell_check(&crate::lint::mask_markup(text, flavor), dicts, bin)
        }
        crate::lint::SpellEngine::Harper => crate::lint::LintEngine::new(dialect).lint_region(text, flavor),
    };
    let has_learned = !learned.is_empty();
    let mut body = String::new();
    let mut count = 0usize;
    for (bs, be, kind, message) in &spans {
        let snippet = &text[*bs..*be];
        // Honour the user dictionary — same filter as the underlines (`map_lint_spans`).
        if has_learned
            && crate::lint::is_spelling_kind(kind)
            && learned.contains(snippet.trim().to_lowercase().as_str())
        {
            continue;
        }
        let (line, col) = lint_line_col(text, *bs);
        body.push_str(&format!("{line}:{col} [{kind}] {snippet:?}: {message}\n"));
        count += 1;
    }
    if count == 0 {
        "No language issues found.".to_string()
    } else {
        format!("{count} issue(s):\n{}", body.trim_end())
    }
}

/// 1-based line and column (column counted in **characters**) for a byte offset into `text`.
/// Used to render `CHECK`'s issue locations.
#[cfg(feature = "lint")]
fn lint_line_col(text: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(text.len());
    let before = &text[..byte];
    let line = before.bytes().filter(|&b| b == b'\n').count() + 1;
    let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    (line, col)
}

/// Spawn `cmd` through the OS shell detached (the `X/NV` fire-and-forget): don't wait or
/// capture. The child handle is dropped immediately.
fn spawn_shell(cmd: &str) -> std::io::Result<()> {
    shell_command(cmd).spawn()?;
    Ok(())
}

/// A message from a running background job's worker thread(s) to `poll_jobs`.
enum JobMsg {
    /// A chunk of output (a stdout or stderr line) as it streams in.
    Chunk(String),
    /// The process finished with this exit code.
    Done(i64),
}

/// A message for an interactive Console child's stdin pump: a line to write (newline appended by the
/// pump), or `Eof` to close stdin (Ctrl+Z / Ctrl+D). The sender lives on the host (`console_stdin`).
enum ConsoleStdin {
    Line(String),
    Eof,
}

/// Launch `cmd` as a streaming background job: a worker thread spawns the shell child with
/// piped stdout+stderr, streams each line over `tx` as it arrives (a second thread drains
/// stderr so a full pipe can't deadlock), then sends `Done(code)`. The live `Child` is parked
/// in `child_slot` so `KILL` can terminate it; the worker takes it out to reap once the pipes
/// close (on normal exit or after a kill).
/// A file staged for a `SENDMAIL` attachment, already read + base64-encoded (so `build_mail_message`
/// stays pure and testable).
#[cfg(feature = "email")]
struct MailAttachment {
    filename: String,
    mime: &'static str,
    b64: String, // base64, wrapped at 76 columns
}

/// Standard base64 (RFC 4648) encode. Small hand-rolled encoder so the email feature adds no crate.
#[cfg(feature = "email")]
fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Wrap a base64 (ASCII) string at 76 columns per line, as RFC 2045 requires.
#[cfg(feature = "email")]
fn wrap76(s: &str) -> String {
    s.as_bytes()
        .chunks(76)
        .map(|c| std::str::from_utf8(c).unwrap_or("")) // base64 is ASCII → always valid
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse a message-range spec (`1-3,6-8,10,12`) into a sorted, de-duplicated list of message numbers
/// (1-based). Used by `SAVEMAIL` to name `MSG<nnn>.TXT` files. Errors on malformed input.
#[cfg(feature = "email")]
fn parse_range(s: &str) -> Result<Vec<u32>, String> {
    let mut out = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((a, b)) = part.split_once('-') {
            let a = a.trim().parse::<u32>().map_err(|_| format!("bad range {part:?}"))?;
            let b = b.trim().parse::<u32>().map_err(|_| format!("bad range {part:?}"))?;
            if a < 1 || b < a {
                return Err(format!("bad range {part:?}"));
            }
            out.extend(a..=b);
        } else {
            let n = part.parse::<u32>().map_err(|_| format!("bad number {part:?}"))?;
            if n < 1 {
                return Err(format!("bad number {part:?}"));
            }
            out.push(n);
        }
    }
    out.sort_unstable();
    out.dedup();
    if out.is_empty() {
        return Err("empty range".into());
    }
    Ok(out)
}

/// Convert a range spec to an IMAP sequence set (no expansion, so `all` stays compact): `all` → `1:*`;
/// `1-3,6-8,10` → `1:3,6:8,10`. Used by `DELMAIL`.
#[cfg(feature = "email")]
fn range_to_imap_seqset(s: &str) -> Result<String, String> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("all") {
        return Ok("1:*".to_string());
    }
    let mut parts = Vec::new();
    for p in s.split(',') {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        if let Some((a, b)) = p.split_once('-') {
            let a = a.trim().parse::<u32>().map_err(|_| format!("bad range {p:?}"))?;
            let b = b.trim().parse::<u32>().map_err(|_| format!("bad range {p:?}"))?;
            if a < 1 || b < a {
                return Err(format!("bad range {p:?}"));
            }
            parts.push(format!("{a}:{b}"));
        } else {
            let n = p.parse::<u32>().map_err(|_| format!("bad number {p:?}"))?;
            if n < 1 {
                return Err(format!("bad number {p:?}"));
            }
            parts.push(n.to_string());
        }
    }
    if parts.is_empty() {
        return Err("empty range".into());
    }
    Ok(parts.join(","))
}

/// Case-insensitive prefix strip: `"Date: x"` with prefix `"Date:"` → `Some(" x")`.
#[cfg(feature = "email")]
fn strip_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.get(..prefix.len()).filter(|h| h.eq_ignore_ascii_case(prefix)).map(|_| &s[prefix.len()..])
}

/// Turn a raw `curl -v` IMAP FETCH trace into a clean numbered Date/From/Subject listing. curl marks
/// incoming protocol lines with `< `; a message record opens on `* N FETCH …` (N is the sequence
/// number to `SAVEMAIL`/`DELMAIL` by), and folded header lines (leading whitespace) continue the
/// previous field. Returns a friendly note if no records were found.
#[cfg(feature = "email")]
fn format_mail_headers(raw: &str, mailbox: &str) -> String {
    #[derive(Default)]
    struct Rec {
        n: u32,
        date: String,
        from: String,
        subj: String,
    }
    #[derive(Clone, Copy)]
    enum F {
        Date,
        From,
        Subj,
    }
    let mut recs: Vec<Rec> = Vec::new();
    let mut last: Option<F> = None;
    for line in raw.lines() {
        let s = line.strip_prefix("< ").unwrap_or(line);
        let s = s.strip_suffix('\r').unwrap_or(s);
        // A folded continuation (leading whitespace) extends the current field.
        if s.starts_with([' ', '\t']) {
            if let (Some(f), Some(r)) = (last, recs.last_mut()) {
                let t = s.trim();
                if !t.is_empty() {
                    let field = match f {
                        F::Date => &mut r.date,
                        F::From => &mut r.from,
                        F::Subj => &mut r.subj,
                    };
                    if !field.is_empty() {
                        field.push(' ');
                    }
                    field.push_str(t);
                }
            }
            continue;
        }
        let t = s.trim_start();
        let toks: Vec<&str> = t.split_whitespace().collect();
        if toks.len() >= 3 && toks[0] == "*" && toks[2].eq_ignore_ascii_case("FETCH") {
            if let Ok(n) = toks[1].parse::<u32>() {
                recs.push(Rec { n, ..Default::default() });
                last = None;
                continue;
            }
        }
        if let Some(v) = strip_ci(t, "Date:") {
            if let Some(r) = recs.last_mut() {
                r.date = v.trim().to_string();
                last = Some(F::Date);
            }
        } else if let Some(v) = strip_ci(t, "From:") {
            if let Some(r) = recs.last_mut() {
                r.from = v.trim().to_string();
                last = Some(F::From);
            }
        } else if let Some(v) = strip_ci(t, "Subject:") {
            if let Some(r) = recs.last_mut() {
                r.subj = v.trim().to_string();
                last = Some(F::Subj);
            }
        }
    }
    if recs.is_empty() {
        return format!(
            "No messages in {mailbox} (or the fetch failed — run `MAILDEBUG on` then `LISTMAIL` to \
             see the full server exchange).\n"
        );
    }
    let mut out = format!("{} message(s) in {mailbox}:\n\n", recs.len());
    for r in &recs {
        out.push_str(&format!("[{}] From: {}\n", r.n, r.from));
        if !r.date.is_empty() {
            out.push_str(&format!("    Date: {}\n", r.date));
        }
        out.push_str(&format!("    Subj: {}\n\n", r.subj));
    }
    out.push_str("Act on these numbers: SAVEMAIL <range> [<path>]  or  DELMAIL <range>\n");
    out
}

/// The character a Normal-layer key cell types, for naming a printable key. Accepts the desugared
/// `insert "X"` form (what a letter cell like `21=y` becomes) or a bare single-char cell. `None` if the
/// cell is a command / multi-character / empty.
fn key_cell_char(binding: &str) -> Option<char> {
    let b = binding.trim();
    let inner = b.strip_prefix("insert \"").and_then(|x| x.strip_suffix('"')).unwrap_or(b);
    let mut it = inner.chars();
    match (it.next(), it.next()) {
        (Some(c), None) if !c.is_control() => Some(c),
        _ => None,
    }
}

/// The canonical display name of a NAMED key by its canonical code (the XyWrite-classic chart codes;
/// see `xyscript-kbd/src/normalize.rs`). Matches the GUI's `key_name` so `GETKEY` reads the same as
/// `READKEY/D`. `None` for keys named by their printable character instead (handled by the caller),
/// for the pure lock/modifier keys (Caps/Num/Scroll — `READKEY` ignores them), and for unknown codes.
fn canonical_key_name(code: u8) -> Option<String> {
    Some(match code {
        1 => "Esc".into(),
        14 => "Backspace".into(),
        15 => "Tab".into(),
        28 | 104 => "Enter".into(), // main Enter + numpad Enter
        55 => "Numpad *".into(),
        57 => "Space".into(),
        58 => "CapsLock".into(),
        59..=68 => format!("F{}", code - 58), // 59→F1 … 68→F10
        69 => "NumLock".into(),
        70 => "ScrLock".into(),
        // Numpad cluster (set-1 71–83; NumLock-on cap labels). Distinct from the gray nav keys (91–104).
        71 => "Numpad 7".into(),
        72 => "Numpad 8".into(),
        73 => "Numpad 9".into(),
        74 => "Numpad -".into(),
        75 => "Numpad 4".into(),
        76 => "Numpad 5".into(),
        77 => "Numpad 6".into(),
        78 => "Numpad +".into(),
        79 => "Numpad 1".into(),
        80 => "Numpad 2".into(),
        81 => "Numpad 3".into(),
        82 => "Numpad 0".into(),
        83 => "Numpad .".into(),
        87 => "F11".into(),
        88 => "F12".into(),
        90 => "Pause/Bk".into(),
        91 => "Ins".into(),
        92 => "Home".into(),
        93 => "PgUp".into(),
        94 => "Numpad /".into(),
        95 => "Del".into(),
        96 => "End".into(),
        97 => "PgDn".into(),
        100 => "Up".into(),
        101 => "Left".into(),
        102 => "Down".into(),
        103 => "Right".into(),
        _ => return None,
    })
}

/// The US-physical key-cap character for a printable code — a fallback for a key the keymap leaves
/// unbound (digits/symbols usually fall through to the OS), so `GETKEY` still names it. The keymap's
/// own `insert "X"` cell is preferred when present (layout-aware); this is the last resort.
fn canonical_printable(code: u8) -> Option<&'static str> {
    Some(match code {
        2 => "1", 3 => "2", 4 => "3", 5 => "4", 6 => "5", 7 => "6", 8 => "7", 9 => "8", 10 => "9",
        11 => "0", 12 => "-", 13 => "=",
        16 => "Q", 17 => "W", 18 => "E", 19 => "R", 20 => "T", 21 => "Y", 22 => "U", 23 => "I",
        24 => "O", 25 => "P", 26 => "[", 27 => "]",
        30 => "A", 31 => "S", 32 => "D", 33 => "F", 34 => "G", 35 => "H", 36 => "J", 37 => "K",
        38 => "L", 39 => ";", 40 => "'", 41 => "`", 43 => "\\",
        44 => "Z", 45 => "X", 46 => "C", 47 => "V", 48 => "B", 49 => "N", 50 => "M", 51 => ",",
        52 => ".", 53 => "/",
        _ => return None,
    })
}

/// Highest IMAP sequence number in a `curl -v` FETCH trace (`* N FETCH …` markers). Since IMAP
/// sequence numbers are contiguous `1..N`, this is the message count. `0` if none found.
#[cfg(feature = "email")]
fn max_fetch_seq(raw: &str) -> u32 {
    let mut max = 0;
    for line in raw.lines() {
        let s = line.strip_prefix("< ").unwrap_or(line);
        let toks: Vec<&str> = s.trim().split_whitespace().collect();
        if toks.len() >= 3 && toks[0] == "*" && toks[2].eq_ignore_ascii_case("FETCH") {
            if let Ok(n) = toks[1].parse::<u32>() {
                max = max.max(n);
            }
        }
    }
    max
}

/// `$RC` value used when the IMAP server refused a command (a tagged `NO`/`BAD` response) even though
/// curl connected and exited `0`. Deliberately outside curl's own exit-code range (1–99) so a script
/// can tell "the server said no" apart from a curl/transport error.
#[cfg(feature = "email")]
pub const IMAP_REJECT_CODE: i64 = 100;

/// Scan a `curl -v` IMAP trace for a tagged failure response — `<tag> NO …` or `<tag> BAD …`, where
/// curl's tags look like `A001`, `A002`, … — and return that response line. curl only checks the
/// transport, so a refused STORE/EXPUNGE/FETCH leaves curl exiting `0`; this catches it. Returns `None`
/// when the trace shows only `OK`s (or no tagged responses, e.g. a non-verbose run). Anchoring to the
/// `A<digits>` tag avoids matching the word "NO"/"BAD" inside fetched header text.
#[cfg(feature = "email")]
fn imap_rejection(trace: &str) -> Option<String> {
    for line in trace.lines() {
        // curl prints server responses as `< <tag> <status> …`; tolerate a missing `< ` prefix too.
        let s = line.strip_prefix("< ").unwrap_or(line).trim();
        let mut it = s.splitn(3, ' ');
        let (Some(tag), Some(status)) = (it.next(), it.next()) else { continue };
        let tagged = tag.len() >= 2
            && tag.starts_with('A')
            && tag[1..].bytes().all(|b| b.is_ascii_digit());
        if tagged && (status == "NO" || status == "BAD") {
            return Some(s.to_string());
        }
    }
    None
}

/// The bare address for the SMTP envelope (`--mail-from` / `--mail-rcpt`): the text inside `<…>` if the
/// address carries a display name (`"Name" <addr>`), else the whole (trimmed) token. The display-name
/// form is kept only in the visible `From:`/`To:`/`Cc:` headers.
#[cfg(feature = "email")]
fn envelope_addr(s: &str) -> String {
    match (s.rfind('<'), s.rfind('>')) {
        (Some(lt), Some(gt)) if lt < gt => s[lt + 1..gt].trim().to_string(),
        _ => s.trim().to_string(),
    }
}

/// A conservative Content-Type guess from a filename's extension; unknown → octet-stream.
#[cfg(feature = "email")]
fn guess_mime(filename: &str) -> &'static str {
    match filename.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "txt" | "text" | "md" | "log" | "typ" | "xpl" => "text/plain",
        "html" | "htm" => "text/html",
        "csv" => "text/csv",
        "json" => "application/json",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "rtf" => "application/rtf",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

/// Classify a header-block line: `Some((lowercase_key, value))` if it's a `Key: value` line whose key
/// is a mail header we honour (incl. `Attach`), else `None` (which ends the header block). Shared by
/// [`build_mail_message`](AppHost::build_mail_message) and [`header_attach_paths`] so they agree on
/// exactly where the block ends.
#[cfg(feature = "email")]
fn parse_header_field(line: &str) -> Option<(String, String)> {
    let (name, val) = line.split_once(':')?;
    let name = name.trim();
    // A valid RFC 5322 field name: non-empty, printable ASCII, no spaces/controls (colon already
    // excluded by split_once). Anything else — blank line, prose, `Dear John: hi` — ends the block.
    if name.is_empty() || name.bytes().any(|b| b <= b' ' || b >= 0x7f) {
        return None;
    }
    Some((name.to_string(), val.trim().to_string()))
}

/// The file paths named by `Attach:` (or `Attachment:`) lines in a header block. Used by `send_mail`
/// to read those files (the pure `build_mail_message` only consumes the lines, it can't do I/O). Scans
/// past any other header (including custom ones), stopping only at the blank line or a non-header line.
#[cfg(feature = "email")]
fn header_attach_paths(doc_text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in doc_text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        let Some((name, val)) = parse_header_field(trimmed) else { break };
        let key = name.to_ascii_lowercase();
        if (key == "attach" || key == "attachment") && !val.is_empty() {
            out.push(val);
        }
    }
    out
}

/// Format a Unix timestamp (seconds since the epoch) as an RFC 5322 `Date` in UTC, e.g.
/// `Fri, 01 Jan 2021 00:00:00 +0000`. UTC avoids needing the local timezone offset (which std can't
/// portably give without a date crate); mail clients display it in the reader's local time anyway.
#[cfg(feature = "email")]
fn rfc5322_utc(secs: u64) -> String {
    const DOW: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"]; // epoch day 0 = Thursday
    const MON: [&str; 12] =
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let dow = DOW[(days % 7) as usize];
    // civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(m <= 2);
    format!("{dow}, {d:02} {} {year} {hh:02}:{mm:02}:{ss:02} +0000", MON[(m - 1) as usize])
}

fn spawn_shell_job(
    mut command: std::process::Command,
    stdin_data: Option<String>,
    stdin_rx: Option<std::sync::mpsc::Receiver<ConsoleStdin>>,
    tx: std::sync::mpsc::SyncSender<JobMsg>,
    child_slot: std::sync::Arc<std::sync::Mutex<Option<std::process::Child>>>,
) {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    std::thread::spawn(move || {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        if stdin_data.is_some() || stdin_rx.is_some() {
            command.stdin(Stdio::piped());
        }
        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(JobMsg::Chunk(format!("[error] failed to start: {e}\n")));
                let _ = tx.send(JobMsg::Done(-1));
                return;
            }
        };
        // Feed the script to the child via stdin (RUNCODE/BG), then close it (EOF). Safe to write
        // sequentially: a `--run -` child reads stdin to EOF before producing any output, so it
        // drains as we write (no stdout-fills-pipe deadlock).
        if let Some(data) = stdin_data {
            if let Some(mut si) = child.stdin.take() {
                use std::io::Write;
                let _ = si.write_all(data.as_bytes());
                // `si` dropped here → child sees EOF on stdin.
            }
        }
        // Interactive stdin (Console REPL): a pump thread owns the child's stdin and writes each line
        // the editor sends (`console_submit` while the command runs), closing it on EOF — an explicit
        // `Eof` (Ctrl+Z/Ctrl+D) or the sender being dropped when the job ends. Lines already exclude
        // the newline; the pump appends one. Runs off-thread so a full stdin pipe never blocks the UI.
        if let Some(rx) = stdin_rx {
            if let Some(mut si) = child.stdin.take() {
                std::thread::spawn(move || {
                    use std::io::Write;
                    for msg in rx {
                        match msg {
                            ConsoleStdin::Line(line) => {
                                if si.write_all(line.as_bytes()).is_err()
                                    || si.write_all(b"\n").is_err()
                                {
                                    break;
                                }
                                let _ = si.flush();
                            }
                            ConsoleStdin::Eof => break, // drop `si` below → EOF
                        }
                    }
                    // `si` dropped here → the child sees EOF on stdin.
                });
            }
        }
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        *child_slot.lock().unwrap() = Some(child); // parked for KILL

        // Read RAW BYTES (not `read_line`, which errors on the first non-UTF-8 byte — Windows
        // shell output is in the OEM console codepage) and decode lossily, so a stray byte can't
        // abort the read (which would drop the pipe and kill the child with a broken-pipe 255).
        fn pump(reader: impl std::io::Read, tx: &std::sync::mpsc::SyncSender<JobMsg>) {
            let mut r = BufReader::new(reader);
            let mut buf: Vec<u8> = Vec::new();
            loop {
                buf.clear();
                match r.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break, // EOF, or a genuine pipe error
                    Ok(_) => {
                        let _ = tx.send(JobMsg::Chunk(String::from_utf8_lossy(&buf).into_owned()));
                    }
                }
            }
        }
        // stderr on its own thread so it can't fill + block the child while we read stdout.
        let tx_err = tx.clone();
        let err_thread = stderr.map(|se| std::thread::spawn(move || pump(se, &tx_err)));
        if let Some(so) = stdout {
            pump(so, &tx);
        }
        if let Some(h) = err_thread {
            let _ = h.join();
        }
        // Reap (take the child out so we never hold the lock across the blocking wait()).
        let child = child_slot.lock().unwrap().take();
        let code = child
            .and_then(|mut c| c.wait().ok())
            .and_then(|s| s.code())
            .map(i64::from)
            .unwrap_or(-1);
        let _ = tx.send(JobMsg::Done(code));
    });
}

/// Hand `path` to the OS default handler (the `Ctrl+Enter` launch). The child is
/// detached (not awaited) so the editor never blocks on the launched program.
fn open_in_os(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // `start` is a cmd builtin; the empty "" is its window-title argument. CREATE_NO_WINDOW
        // keeps `cmd` from flashing a throwaway console (the GUI build has none to inherit).
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(path).spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(path).spawn()?;
    }
    Ok(())
}

/// The default URL-highlight text colour (`URLFGCOLOR` unset) — a link blue that reads on light and
/// dark backgrounds.
const DEFAULT_URL_FG: (u8, u8, u8) = (74, 137, 232);

/// The regex for `URLHIGHLIGHT` matches: an `http(s)`/`ftp` scheme, a bare `www.`, or a `mailto:`,
/// then a run of non-whitespace (stopping at angle brackets and quotes). Trailing sentence punctuation
/// is trimmed afterwards by [`trim_url_len`]. Case-insensitive on the scheme.
const URL_PATTERN: &str = r#"(?i)(?:\b(?:https?|ftp)://|\bwww\.|\bmailto:)[^\s<>"'`]+"#;

/// The trimmed BYTE length of a matched URL: drop trailing sentence punctuation and unbalanced closing
/// brackets (so `(see http://x.org).` and `http://x.org, foo` don't swallow the punctuation, while a
/// balanced `http://en.wikipedia.org/wiki/Foo_(bar)` keeps its `)`).
fn trim_url_len(raw: &[u8]) -> usize {
    let s = String::from_utf8_lossy(raw);
    let mut end = s.len();
    while let Some(last) = s[..end].chars().next_back() {
        let cur = &s[..end];
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '"' | '\'' | '»' | '”' | '’' => true,
            ')' => cur.matches('(').count() < cur.matches(')').count(),
            ']' => cur.matches('[').count() < cur.matches(']').count(),
            '}' => cur.matches('{').count() < cur.matches('}').count(),
            _ => false,
        };
        if drop {
            end -= last.len_utf8();
        } else {
            break;
        }
    }
    end
}

/// Normalize a highlighted URL for opening: pass web/ftp/mail schemes through, prepend `https://` to a
/// bare `www.`, and reject anything else (so Ctrl+click never launches an unexpected scheme).
fn normalize_url(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("ftp://")
        || lower.starts_with("mailto:")
    {
        Some(url.to_string())
    } else if lower.starts_with("www.") {
        Some(format!("https://{url}"))
    } else {
        None
    }
}

/// Hand a URL to the OS default handler (Ctrl+click). Detached, like [`open_in_os`], but launched via
/// `explorer` on Windows so a query string's `&` is safe (a `cmd /C start` would mis-parse it).
fn open_url_os(url: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("explorer")
            .arg(url)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
    }
    Ok(())
}

/// Where editor operations and inserted text are currently directed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The main document buffer.
    Document,
    /// The command line (CMline).
    CmLine,
}

/// Where a job's output goes as it streams / on completion. `poll_jobs` routes by this.
#[derive(Debug, Clone, Copy)]
enum JobSink {
    /// The `*Shell Output*` tab (the `X/BG` default) — streamed live.
    ShellTab,
    /// A FOREGROUND Console REPL command (`console_submit`): streamed live into the Console above its
    /// prompt; on completion an `(exit N)` line lands and a fresh prompt is written for the next input.
    Console,
    /// `LISTMAIL`: buffer the raw `curl -v` IMAP trace silently, then on completion parse it into a
    /// clean numbered Date/From/Subject listing written to *Shell Output*. (A custom IMAP FETCH only
    /// surfaces on curl's protocol stream, never stdout — so we must capture `-v` and reformat.)
    #[cfg(feature = "email")]
    MailHeaders,
    /// `SAVEMAIL all`: buffer a FETCH-headers trace silently, then on completion count the messages
    /// (`* N FETCH` markers) and spawn the real download of `1..N` into the pending directory (looked
    /// up in `mail_save_all` by job id).
    #[cfg(feature = "email")]
    MailSearchSave,
    /// `DELMAIL`: run curl `-v` (so the server's tagged `OK`/`NO`/`BAD` response is captured for the
    /// IMAP-rejection check), but keep *Shell Output* clean — buffer the trace silently and, unless
    /// `MAILDEBUG` is on, show only the exit line (plus a warning if the server refused the command).
    #[cfg(feature = "email")]
    MailAction,
}

/// A running background shell job (`X/BG`, or an `SH` pane command). The worker streams output lines
/// over `rx` and a final `Done(code)`; [`AppHost::poll_jobs`] drains it. `child` is the live process
/// (for `KILL`); `output` accumulates the streamed text (→ `$XO` on completion); `sink` routes it.
struct Job {
    id: u64,
    cmd: String,
    rx: std::sync::mpsc::Receiver<JobMsg>,
    child: std::sync::Arc<std::sync::Mutex<Option<std::process::Child>>>,
    output: String,
    /// `/R:<var>` — a parent variable to receive this job's full output on completion. The editor
    /// drains `pending_returns` after `poll_jobs` and writes it into the interpreter's `Env`.
    return_var: Option<String>,
    sink: JobSink,
    /// A temp file to delete once the job finishes (the `SENDMAIL` message file curl uploads). `None`
    /// for ordinary jobs.
    cleanup: Option<std::path::PathBuf>,
    /// A mail job (curl to SMTP/IMAP). `poll_jobs` scans a mail job's output for an IMAP `NO`/`BAD`
    /// rejection and elevates `$RC` — curl exits `0` even when the server refuses a command. `false`
    /// for ordinary shell jobs (whose output must never be scanned for false positives).
    is_mail: bool,
}

/// A streaming `GREP` search running on a worker thread (see `EditorOp::Grep`). The worker sends
/// batches of hits over `rx` and a final `Done`/`Error`; [`AppHost::poll_grep`] drains it into the
/// results tab identified by `doc`. `cancel` aborts the walk (a new `GREP`, or the tab being closed).
struct GrepJob {
    rx: std::sync::mpsc::Receiver<GrepMsg>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    doc: crate::panes::DocId,
    /// The search root (`$CWD` at launch) — the results tab renders paths relative to it.
    base: PathBuf,
}

/// A message from the `GREP` worker thread to [`AppHost::poll_grep`].
enum GrepMsg {
    /// A batch of matches to append to the results tab.
    Hits(Vec<crate::grep::GrepHit>),
    /// The search finished: how many files were searched, and whether the cap was hit.
    Done { files: usize, capped: bool },
    /// The search failed to start (e.g. an invalid regex slipped through) — shouldn't normally happen
    /// since `compile_check` validates up front.
    Error(String),
}

/// Total-hit cap for a streaming `GREP` (bounds memory + the results buffer on a pathological tree).
const GREP_CAP: usize = 50_000;
/// The ceiling on how many carets `CARETALL` will create from a match set. If a pattern matches
/// more, we create this many and REPORT the cap on the status line (never silently truncate).
const MAX_CARETS: usize = 2000;
/// The worker flushes a batch of hits to the channel every this many matches (responsive without
/// per-hit channel chatter).
const GREP_BATCH: usize = 200;
/// Per-frame cap on how many batches `poll_grep` appends, so a flood can't stall a frame.
const GREP_DRAIN_BATCHES: usize = 32;

/// Bounded job-channel capacity: the worker blocks on `send` once this many lines are unread,
/// throttling a flood (e.g. `dir /s C:\`) to the GUI's drain rate so memory + the UI stay bounded.
const JOB_CHANNEL_CAP: usize = 4096;
/// Per-frame append budget (bytes) for streamed job output — caps the GUI's per-frame work.
const JOB_DRAIN_BYTES: usize = 256 * 1024;

/// One open buffer's display info for the GUI tab strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabInfo {
    /// File name (or "untitled").
    pub name: String,
    /// The document's full path (including the file name), or `None` for an unsaved / special buffer.
    /// Used for the tab's hover tooltip.
    pub path: Option<String>,
    /// Has unsaved changes.
    pub modified: bool,
    /// Locked against edits (`RO`).
    pub read_only: bool,
    /// A `DIR`/`LS` directory listing (transient — nothing to save, so "close without saving" is moot).
    pub dir_listing: bool,
}

/// The user's answer to a close-confirmation prompt (`AB/V` save-prompt, `ABD` delete-confirm).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAnswer {
    Yes,
    No,
    Cancel,
}

/// The user's answer to the "file is already open" prompt (`OPEN`/`READ` of an already-open file).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAnswer {
    /// (Y)es — open it again in a new tab (the original request: writable, or read-only for `READ`).
    Yes,
    /// (N)o — cancel; do nothing.
    No,
    /// (G)o — switch to the tab already showing it.
    Go,
    /// (R)ead — open it again, but read-only.
    Read,
}

/// A pending "file is already open" prompt: the requested path, the tab already showing it, and
/// whether the original request was `READ` (read-only).
struct PendingOpen {
    path: String,
    existing: usize,
    read_only: bool,
    hex: bool,
}

/// Which font the GUI font-picker dialog targets (`EDFONTS`/`UIFONTS`/`TXTFONTS`). The dialog is
/// GUI sugar: on OK it runs the family + size setters for the chosen target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontPickerKind {
    /// `EDFONTS` — editor canvas (`EDFAMILY`/`EDFONT`); family list is monospaced-only.
    Editor,
    /// `EDPROPFONTS` — editor PROPORTIONAL canvas font (`EDPROPFAMILY`); all families.
    EditorProp,
    /// `UIFONTS` — UI chrome (`UIFAMILY`/`UIFONT`).
    Ui,
    /// `TXTFONTS` — `TXT` window (`TXTFAMILY`/`TXTFONT`).
    Txt,
    /// `DWFONTS` — `DIR/W` window (`DWFAMILY`/`DWFONT`); family list is monospaced-only.
    Dir,
    /// `PVFONTS` — Markdown Preview/`TOPDF` BODY font (`PVFAMILY`/`PVSIZE`); all families.
    PreviewBody,
    /// `PVCODES` — Markdown Preview/`TOPDF` CODE font (`PVCODE`); monospaced-only.
    PreviewCode,
}

/// A GUI tab-title interaction state (`TABBGCOLOR`/`TABBGCOLORS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabState {
    Active,
    Hover,
    Inactive,
}

impl TabState {
    pub fn name(self) -> &'static str {
        match self {
            TabState::Active => "active",
            TabState::Hover => "hover",
            TabState::Inactive => "inactive",
        }
    }
    /// Parse a state name (with the accepted aliases). `None` for an unknown state.
    pub fn from_name(s: &str) -> Option<TabState> {
        match s.trim().to_ascii_lowercase().as_str() {
            "active" | "selected" => Some(TabState::Active),
            "hover" | "hovered" => Some(TabState::Hover),
            "inactive" | "normal" | "idle" => Some(TabState::Inactive),
            _ => None,
        }
    }
}

/// Format an optional tab-title colour for `$TAB…` readback: `"R G B"`, or `""` when unset (the
/// state is using egui's theme colour).
fn tab_color_str(c: Option<(u8, u8, u8)>) -> String {
    match c {
        Some((r, g, b)) => format!("{r} {g} {b}"),
        None => String::new(),
    }
}

/// Which colour the GUI colour-picker dialog targets (the plural `*COLORS` verbs). On OK the dialog
/// applies the matching singular `*COLOR` setter (or `HLCOLOR <kind>` / `TABBGCOLOR <state>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColorTarget {
    Fg,
    Bg,
    UiFg,
    UiBg,
    Caret,
    CmlineCaret,
    Separator,
    TxtFg,
    TxtBg,
    DwFg,
    DwBg,
    SelBg,
    SelFg,
    FindBg,
    FindFg,
    SpellUnderline,
    GrammarUnderline,
    PaneBorder,
    Highlight(crate::highlight::TokenKind),
    HighlightBg(crate::highlight::TokenKind),
    Tab(TabState),
    TabClose,
    BalPairs,
    PathFg,
    PathBg,
    PreviewFg,
    PreviewBg,
    ScratchFg,
    ScratchBg,
    MarkFg,
    MarkBg,
    LineNumFg,
    LineNumBg,
    MsgHeaderFg,
    MsgBg,
    MsgOkFg,
    MsgOkBg,
    UrlFg,
    UrlBg,
    EmphFg,
    EmphBg,
    MdaFg,
    MdaBg,
}

impl ColorTarget {
    /// Title for the picker window.
    pub fn title(&self) -> String {
        match self {
            ColorTarget::Fg => "Editor foreground colour".into(),
            ColorTarget::Bg => "Editor background colour".into(),
            ColorTarget::UiFg => "UI foreground colour".into(),
            ColorTarget::UiBg => "UI background colour".into(),
            ColorTarget::Caret => "Caret colour".into(),
            ColorTarget::CmlineCaret => "Command-line caret colour".into(),
            ColorTarget::Separator => "Separator colour".into(),
            ColorTarget::TxtFg => "TXT foreground colour".into(),
            ColorTarget::TxtBg => "TXT background colour".into(),
            ColorTarget::DwFg => "DIR-window foreground colour".into(),
            ColorTarget::DwBg => "DIR-window background colour".into(),
            ColorTarget::SelBg => "Selection background colour".into(),
            ColorTarget::SelFg => "Selected-text colour".into(),
            ColorTarget::FindBg => "FIND highlight background colour".into(),
            ColorTarget::FindFg => "FIND-match text colour".into(),
            ColorTarget::SpellUnderline => "Spelling underline colour".into(),
            ColorTarget::GrammarUnderline => "Grammar underline colour".into(),
            ColorTarget::PaneBorder => "Active-pane border colour".into(),
            ColorTarget::Highlight(k) => format!("Highlight colour: {}", k.name()),
            ColorTarget::HighlightBg(k) => format!("Highlight background colour: {}", k.name()),
            ColorTarget::Tab(state) => format!("Tab {} background colour", state.name()),
            ColorTarget::TabClose => "Tab close-× colour".into(),
            ColorTarget::BalPairs => "Matched-bracket highlight colour".into(),
            ColorTarget::PathFg => "$CWD path text colour".into(),
            ColorTarget::PathBg => "$CWD path background colour".into(),
            ColorTarget::PreviewFg => "Preview text colour".into(),
            ColorTarget::PreviewBg => "Preview background colour".into(),
            ColorTarget::ScratchFg => "Scratchpad text colour".into(),
            ColorTarget::ScratchBg => "Scratchpad background colour".into(),
            ColorTarget::MarkFg => "Bookmark gutter dot colour".into(),
            ColorTarget::MarkBg => "Bookmark gutter background colour".into(),
            ColorTarget::LineNumFg => "Line-number gutter text colour".into(),
            ColorTarget::LineNumBg => "Line-number gutter background colour".into(),
            ColorTarget::MsgHeaderFg => "MSG box header text colour".into(),
            ColorTarget::MsgBg => "MSG box background colour".into(),
            ColorTarget::MsgOkFg => "MSG box OK-button text colour".into(),
            ColorTarget::MsgOkBg => "MSG box OK-button background colour".into(),
            ColorTarget::UrlFg => "URL highlight text colour".into(),
            ColorTarget::UrlBg => "URL highlight background colour".into(),
            ColorTarget::EmphFg => "EMPH emphasis text colour".into(),
            ColorTarget::EmphBg => "EMPH emphasis background colour".into(),
            ColorTarget::MdaFg => "Block colour — selection text".into(),
            ColorTarget::MdaBg => "Block colour — selection background".into(),
        }
    }
    /// The setter command to run (with the chosen `R G B`) when the user accepts the picker.
    pub fn set_command(&self, r: u8, g: u8, b: u8) -> String {
        let verb = match self {
            ColorTarget::Fg => "FGCOLOR",
            ColorTarget::Bg => "BGCOLOR",
            ColorTarget::UiFg => "UIFGCOLOR",
            ColorTarget::UiBg => "UIBGCOLOR",
            ColorTarget::Caret => "CRCOLOR",
            ColorTarget::CmlineCaret => "CMCRCOLOR",
            ColorTarget::Separator => "SEPCOLOR",
            ColorTarget::TxtFg => "TXTFGCOLOR",
            ColorTarget::TxtBg => "TXTBGCOLOR",
            ColorTarget::DwFg => "DWFGCOLOR",
            ColorTarget::DwBg => "DWBGCOLOR",
            ColorTarget::SelBg => "SELBGCOLOR",
            ColorTarget::SelFg => "SELFGCOLOR",
            ColorTarget::FindBg => "FINDBGCOLOR",
            ColorTarget::FindFg => "FINDFGCOLOR",
            ColorTarget::SpellUnderline => "SPELLCOLOR",
            ColorTarget::GrammarUnderline => "GRAMMARCOLOR",
            ColorTarget::PaneBorder => "PANECOLOR",
            ColorTarget::Highlight(k) => return format!("HLCOLOR {} {r} {g} {b}", k.name()),
            ColorTarget::HighlightBg(k) => return format!("HLCOLOR/BG {} {r} {g} {b}", k.name()),
            ColorTarget::Tab(state) => return format!("TABBGCOLOR {} {r} {g} {b}", state.name()),
            ColorTarget::TabClose => return format!("TABXCOLOR {r} {g} {b}"),
            ColorTarget::BalPairs => return format!("BALPAIRSCOLOR {r} {g} {b}"),
            ColorTarget::PathFg => "PAFGCOLOR",
            ColorTarget::PathBg => "PABGCOLOR",
            ColorTarget::PreviewFg => "PVFGCOLOR",
            ColorTarget::PreviewBg => "PVBGCOLOR",
            ColorTarget::ScratchFg => "SCRATCHFGCOLOR",
            ColorTarget::ScratchBg => "SCRATCHBGCOLOR",
            ColorTarget::MarkFg => "MARKFGCOLOR",
            ColorTarget::MarkBg => "MARKBGCOLOR",
            ColorTarget::LineNumFg => "LNFGCOLOR",
            ColorTarget::LineNumBg => "LNBGCOLOR",
            ColorTarget::MsgHeaderFg => "MSGHFGCOLOR",
            ColorTarget::MsgBg => "MSGBGCOLOR",
            ColorTarget::MsgOkFg => "MSGOKFGCOLOR",
            ColorTarget::MsgOkBg => "MSGOKBGCOLOR",
            ColorTarget::UrlFg => "URLFGCOLOR",
            ColorTarget::UrlBg => "URLBGCOLOR",
            ColorTarget::EmphFg => "EMPHFGCOLOR",
            ColorTarget::EmphBg => "EMPHBGCOLOR",
            // MDA colours the SELECTION's persistent block: bare = foreground, `/B` = background.
            ColorTarget::MdaFg => return format!("MDA {r} {g} {b}"),
            ColorTarget::MdaBg => return format!("MDA/B {r} {g} {b}"),
        };
        format!("{verb} {r} {g} {b}")
    }
}

/// A font style for `EDSTYLE`/`UISTYLE`/`TXTSTYLE` (the editor canvas / UI chrome / `TXT` window).
/// The GUI maps it to a face weight + slant: bold ⇒ ~700, italic ⇒ the italic/oblique face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FontStyle {
    #[default]
    Normal,
    Bold,
    Italic,
    BoldItalic,
}

impl FontStyle {
    /// Parse the verb keyword (`normal`/`regular`/`bold`/`italic`/`bolditalic`, dashes/spaces and
    /// order-insensitive). Returns `None` for anything else.
    pub fn parse(s: &str) -> Option<FontStyle> {
        let k: String = s.chars().filter(|c| !matches!(c, ' ' | '-' | '_')).collect();
        match k.to_ascii_lowercase().as_str() {
            "normal" | "regular" | "plain" | "" => Some(FontStyle::Normal),
            "bold" => Some(FontStyle::Bold),
            "italic" | "oblique" => Some(FontStyle::Italic),
            "bolditalic" | "italicbold" => Some(FontStyle::BoldItalic),
            _ => None,
        }
    }
    /// The canonical keyword (for `$EDSTYLE` etc. and SaveCfg emission).
    pub fn keyword(self) -> &'static str {
        match self {
            FontStyle::Normal => "normal",
            FontStyle::Bold => "bold",
            FontStyle::Italic => "italic",
            FontStyle::BoldItalic => "bolditalic",
        }
    }
    pub fn bold(self) -> bool {
        matches!(self, FontStyle::Bold | FontStyle::BoldItalic)
    }
    pub fn italic(self) -> bool {
        matches!(self, FontStyle::Italic | FontStyle::BoldItalic)
    }
}

/// Parse a font-style keyword for a `*STYLE` verb, or a trappable error naming the valid set.
fn parse_font_style(s: &str) -> Result<FontStyle, XplError> {
    FontStyle::parse(s).ok_or_else(|| {
        XplError::trappable(format!(
            "font style: expected normal|bold|italic|bolditalic, got `{}`",
            s.trim()
        ))
    })
}

/// State for the separate `DIR/W` listing window (a single reusable window). Holds its own
/// listing `Document` (with `dir_state`), independent of the main tab set. The GUI renders it and
/// forwards navigation/activation back through the `dir_window_*` methods.
pub struct DirWindowState {
    /// The listing buffer shown in the window.
    doc: Document,
    /// The base directory's last-seen modification time, for refresh-on-focus (re-list only when
    /// it has actually changed, so refocusing a huge/slow directory doesn't rescan needlessly).
    base_mtime: Option<std::time::SystemTime>,
    /// One-shot: a fresh `DIR/W` was issued, so the GUI should create the window (or focus +
    /// refresh the existing one). Cleared by `take_dir_window_open`.
    just_opened: bool,
}

/// A snapshot of the `DIR/W` window's listing for the GUI to render (it's a thin renderer of
/// this host state). Rebuilt each frame from the window's `Document`.
pub struct DirWindowView {
    /// The formatted listing, one entry per line (display order).
    pub lines: Vec<String>,
    /// The selected row (0-based) to highlight.
    pub selected: usize,
    /// The directory being shown (for the footer).
    pub base: PathBuf,
    /// The `$DP` string (directory + any file mask) — used for the window title.
    pub path: String,
    /// The live type-to-find buffer (for the footer).
    pub find: String,
}

/// What activating a `DIR/W` entry did (Enter / double-click), for the status line.
pub enum DirWindowAction {
    /// Descended into a subdirectory (the window re-listed it).
    Descended(PathBuf),
    /// Opened a file in a new tab in the parent window.
    Opened(PathBuf),
    /// Nothing actionable under the caret.
    None,
}

/// The directory's modification time, if obtainable (used to gate refresh-on-focus).
fn dir_mtime(base: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(base).and_then(|m| m.modified()).ok()
}

/// Default `SAVEAR` path (relative → `$CWD`).
const DEFAULT_AR: &str = "autoreplace.ar";

/// The commented sample written by `SAVEAR` when no auto-replace pairs are loaded.
const AR_SAMPLE: &str = "\
# XyScript auto-replace file — one `key=replacement` per line.
# AR replaces the word typed just before the caret with its replacement (case-sensitive, exact).
# `#` starts a comment; blank lines are ignored. In a replacement, `\\n` → newline and `\\t` → tab
# (for multi-line snippets); `\\\\` → a literal backslash.
#
# Load it with:   LoadAR <thisfile>
# Then bind AR to a trigger, e.g. in your KBD:   57=AR;\" \"   (Shift+Space = fix-then-space)

# --- typo corrections ---
teh=the
adn=and
recieve=receive

# --- abbreviations / snippets ---
btw=by the way
sig=Best regards,\\nAda
";

/// Decode `\\n`/`\\t`/`\\\\` escapes in an auto-replace replacement value (other `\\x` kept verbatim).
/// Whether a successful op should leave the caret IN THE TEXT (vs. returning to the cmline) when the
/// command was run from the command line — the per-verb focus rule. TRUE for verbs that "touch the
/// text area": text edits, search/replace, file/screen navigation (`TF/BF/HM/BS/PD/PU`, `JMP`), and
/// file/tab ops. FALSE (→ stay in the cmline) for buffer-agnostic motions (line/word/sentence/cursor
/// moves, `LB/LE/EL/ER`, `NP/PP`), `UNDO`/`REDO`, cmline-meta, and neutral commands (settings, view,
/// save, queries). Default FALSE, so an unlisted op safely stays in the cmline.
fn is_document_focusing(op: &EditorOp) -> bool {
    use EditorOp::*;
    match op {
        // File/screen navigation lands you in the document; line/word/cursor moves do not.
        Move { motion, .. } => matches!(
            motion,
            Motion::DocStart
                | Motion::DocEnd
                | Motion::ScreenTop
                | Motion::ScreenBottom
                | Motion::PageUp
                | Motion::PageDown
                | Motion::ToChar(_)
                | Motion::ByChar(_)
        ),
        // Text-touching edits, selection, and search.
        InsertText(_) | TypeText(_) | Delete(_) | RubDelete | PasteDeletion | DeleteSel | Case(_)
        | CopySelection | MoveSelection | Define(_) | Sort(_) | Clip(_) | Search(_) | Merge(_)
        | AutoReplace | Revert | Counter { .. } => true,
        // File/tab ops put you in a document.
        NewDoc | NewNamed { .. } | Open { .. } | OpenReadOnly { .. } | Dir { .. } | Grep { .. }
        | GrepJump | CloseBuffer | CloseBufferVerify => true,
        // Focus-management ops that intend the document (must not be overridden back to the cmline).
        FocusText | EnsureDocumentFocus | ToggleFocus => true,
        // Undo/Redo, cmline-meta, settings, view, save, queries, scroll, … → stay in the cmline.
        _ => false,
    }
}

/// Resolve a 1-based `(line, byte-col)` (as `GREP`/`GJ` report) to a byte offset in `text` (the decoded
/// buffer). The column is a BYTE offset within the line; it is clamped to the line's content and snapped
/// to a UTF-8 char boundary so the result is always a valid caret position. Exact for a UTF-8 file; for a
/// re-decoded non-UTF-8 file the LINE is still right and the column is best-effort.
fn line_col_to_offset(text: &str, line: usize, col: usize) -> usize {
    let bytes = text.as_bytes();
    let line0 = line.saturating_sub(1);
    // Byte offset of the start of line `line0` (0 for the first line).
    let mut start = 0usize;
    if line0 > 0 {
        let mut seen = 0usize;
        let mut found = false;
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' {
                seen += 1;
                if seen == line0 {
                    start = i + 1;
                    found = true;
                    break;
                }
            }
        }
        if !found {
            return text.len(); // line past EOF → land at the end
        }
    }
    // End of this line (before its newline); clamp the column into it, then to a char boundary.
    let line_end = bytes[start..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|p| start + p)
        .unwrap_or(text.len());
    let mut target = (start + col.saturating_sub(1)).min(line_end);
    while target > start && !text.is_char_boundary(target) {
        target -= 1;
    }
    target
}

/// The new-side (`+`) start line of a unified-diff hunk header, e.g. `@@ -12,4 +15,6 @@` → `15`
/// (for `DJ`). `None` if the line isn't a hunk header or has no parseable `+start`.
fn parse_hunk_new_start(header: &str) -> Option<usize> {
    let tok = header.split_whitespace().find(|t| t.starts_with('+'))?;
    let digits: String = tok[1..].chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Fail fast on a **cloud-only placeholder** (Dropbox/OneDrive "online-only") before a `read` — so
/// the read doesn't BLOCK the calling thread while the file is hydrated on demand, which freezes the
/// GUI (file reads run on the UI thread). Detects the `OFFLINE` / `RECALL_ON_DATA_ACCESS` attributes
/// via a non-hydrating `symlink_metadata` (reparse-point stat, doesn't trigger download). A *hydrated*
/// cloud file (local copy present) has neither flag, so it's not tripped — no false positives on
/// normal files. No-op on non-Windows or when the attributes can't be read (the read just proceeds).
/// Raw bytes → a continuous lowercase hex string (the `READFILE(path, "hex")` output).
fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0xF) as u32, 16).unwrap());
    }
    s
}

/// A hex string → raw bytes (the `WRITEFILE/HEX` input). Tolerant: ASCII whitespace, `:` separators,
/// and `0x`/`0X` prefixes are ignored, so a hand-edited or tool-dumped blob pastes cleanly. An odd
/// number of hex digits, or any non-hex character, is an `Err(message)`.
fn hex_to_bytes(s: &str) -> Result<Vec<u8>, String> {
    let mut nibbles: Vec<u8> = Vec::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_whitespace() || c == ':' {
            continue;
        }
        // Skip a `0x`/`0X` prefix (a `0` immediately followed by `x`/`X`).
        if c == '0' && matches!(chars.peek(), Some('x') | Some('X')) {
            chars.next();
            continue;
        }
        match c.to_digit(16) {
            Some(d) => nibbles.push(d as u8),
            None => return Err(format!("not a hex digit: {c:?}")),
        }
    }
    if nibbles.len() % 2 != 0 {
        return Err(format!("odd number of hex digits ({})", nibbles.len()));
    }
    Ok(nibbles.chunks(2).map(|p| (p[0] << 4) | p[1]).collect())
}

fn cloud_only_guard(path: &Path) -> VmResult<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const OFFLINE: u32 = 0x0000_1000; // FILE_ATTRIBUTE_OFFLINE
        const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000; // FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
        if let Ok(md) = std::fs::symlink_metadata(path) {
            if md.file_attributes() & (OFFLINE | RECALL_ON_DATA_ACCESS) != 0 {
                return Err(XplError::trappable(format!(
                    "{}: cloud-only (not downloaded) — make it available offline, then retry",
                    path.display()
                )));
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path; // unused off Windows
    }
    Ok(())
}

/// Truncate `s` to at most `width` characters, appending `…` when it had to cut (for the REOPEN
/// chooser's fixed-width filename column).
fn truncate_col(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        s.to_string()
    } else {
        let head: String = s.chars().take(width.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

/// Parse a `LANG` UI-string table: a flat UTF-8 file of `key = value   # english-source` lines.
/// The key is everything left of the FIRST `=`; the value is the rest with any trailing `# …`
/// reference comment stripped and trimmed. Blank/`#`-comment lines and empty values are skipped —
/// an empty value means "fall back to English", so a partially-filled catalog works. (Values never
/// contain `#` in practice; the reference comment is split off at the first one.)
/// Parse a `.lang` catalog into `(key, value, english_anchor)` triples in file order. `value` is the
/// text left of the first `#` (may be empty = untranslated); `english_anchor` is the `# English`
/// source comment right of it (`None` if absent). Comment/blank lines are skipped. The anchor lets
/// the overlay loader self-heal (skip an override written against English that has since changed).
fn parse_lang_entries(text: &str) -> Vec<(String, String, Option<String>)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_start_matches('\u{feff}'); // tolerate a leading BOM
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, rest)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let (value, anchor) = match rest.split_once('#') {
            Some((v, a)) => (v.trim(), Some(a.trim().to_string())),
            None => (rest.trim(), None),
        };
        out.push((key.to_string(), value.to_string(), anchor));
    }
    out
}

/// Hard-wrap `text` to `width` columns for `REFLOW`. Each blank-line-delimited paragraph is
/// collapsed (all runs of whitespace, line breaks included, become single spaces) and greedily
/// re-wrapped so no line exceeds `width` columns — counting Unicode scalar values — without ever
/// splitting a word (a word wider than the room left by the indent overflows onto its own line).
/// The leading whitespace of a paragraph's FIRST line becomes the indent applied to every one of
/// its wrapped lines. Blank lines (paragraph breaks) are preserved verbatim, and a trailing line
/// break in the input is kept. `nl` is the line ending used to join. Pure + deterministic (no
/// layout), so it behaves identically from the cmline, a chain, or a script.
fn reflow_text(text: &str, width: usize, nl: &str) -> String {
    let width = width.max(1);
    let ended_with_newline = text.ends_with('\n');
    // Logical lines, each stripped of a trailing '\r' (CRLF-safe). Drop the empty tail that
    // `split('\n')` yields for a trailing newline — the newline is re-added at the end.
    let mut lines: Vec<&str> =
        text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    if ended_with_newline {
        lines.pop(); // the "" after the final '\n'
    }
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim().is_empty() {
            out.push(String::new()); // preserve a blank line (paragraph break)
            i += 1;
            continue;
        }
        // Gather one paragraph: consecutive non-blank lines. Its indent is the first line's leading
        // whitespace, applied to every wrapped line.
        let indent: String = lines[i].chars().take_while(|c| c.is_whitespace()).collect();
        let mut words: Vec<&str> = Vec::new();
        while i < lines.len() && !lines[i].trim().is_empty() {
            words.extend(lines[i].split_whitespace());
            i += 1;
        }
        let avail = width.saturating_sub(indent.chars().count()).max(1);
        let mut line = String::new();
        let mut line_cols = 0usize;
        for w in words {
            let wcols = w.chars().count();
            if line_cols == 0 {
                line.push_str(w); // first word always goes on the line (may overflow `avail`)
                line_cols = wcols;
            } else if line_cols + 1 + wcols <= avail {
                line.push(' ');
                line.push_str(w);
                line_cols += 1 + wcols;
            } else {
                out.push(format!("{indent}{line}"));
                line = w.to_string();
                line_cols = wcols;
            }
        }
        out.push(format!("{indent}{line}")); // the paragraph always had >= 1 word
    }
    let mut result = out.join(nl);
    if ended_with_newline {
        result.push_str(nl);
    }
    result
}

/// The `string-id -> value` table for a catalog (untranslated/blank entries dropped, so a partial
/// catalog falls back to English). Anchors are discarded — [`parse_lang_entries`] keeps them.
fn parse_lang_table(text: &str) -> HashMap<String, String> {
    parse_lang_entries(text)
        .into_iter()
        .filter(|(_, value, _)| !value.is_empty())
        .map(|(key, value, _)| (key, value))
        .collect()
}

/// A one-line preview of selection text for the enhanced-`CP` chooser: the first non-blank-trimmed
/// line, truncated to ~40 characters with an ellipsis.
fn preview_one_line(s: &str) -> String {
    const MAX: usize = 40;
    let first = s.lines().next().unwrap_or("").trim();
    if first.chars().count() <= MAX {
        first.to_string()
    } else {
        let head: String = first.chars().take(MAX).collect();
        format!("{head}…")
    }
}

/// How the word the user actually typed was capitalized — used to case-match an auto-replace
/// expansion, so one entry `equ=equilibrium` covers `equ`/`Equ`/`EQU`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyCase {
    /// All lower case, no letters, or a mixed pattern we don't try to interpret (`eQu`): use the
    /// replacement EXACTLY as written, so a deliberately-cased value (`usa=United States`) survives.
    AsIs,
    /// Leading capital, remainder lower (`Equ`) — or a lone capital (`I`), which is deliberately
    /// treated as capitalised rather than all-caps.
    Capitalized,
    /// Two or more letters, all capital (`EQU`).
    Upper,
}

/// Classify the typed key's capitalisation. Only alphabetic characters count, so `e-mail` /
/// `E-Mail` classify on their letters alone.
pub(crate) fn key_case(word: &str) -> KeyCase {
    let letters: Vec<char> = word.chars().filter(|c| c.is_alphabetic()).collect();
    let Some(first) = letters.first().copied() else {
        return KeyCase::AsIs; // no letters to judge by
    };
    if !first.is_uppercase() {
        return KeyCase::AsIs; // starts lower → plain
    }
    let rest_upper = letters[1..].iter().all(|c| c.is_uppercase());
    let rest_lower = letters[1..].iter().all(|c| !c.is_uppercase());
    match () {
        // `EQU` — needs 2+ letters, so a lone `I` doesn't shout the whole expansion.
        _ if letters.len() > 1 && rest_upper => KeyCase::Upper,
        _ if rest_lower => KeyCase::Capitalized,
        _ => KeyCase::AsIs, // `eQu`, `EqU` — no clear intent
    }
}

/// Recase `replacement` to match the case (lower / Capitalized / UPPER) of `matched` — the `/K`
/// keep-case rule. Exposed for the GUI's single-Replace path, which recases outside the search
/// engine; `Pattern::replace_all` applies the same rule per match for `CH/K` / Replace-all.
pub fn recase_to_match(matched: &str, replacement: &str) -> String {
    apply_key_case(key_case(matched), replacement)
}

/// Recase an auto-replace expansion to match how its key was typed.
pub(crate) fn apply_key_case(case: KeyCase, val: &str) -> String {
    match case {
        KeyCase::AsIs => val.to_string(),
        KeyCase::Upper => val.to_uppercase(),
        KeyCase::Capitalized => {
            let mut chars = val.chars();
            match chars.next() {
                // `to_uppercase` can yield several chars (ß → SS), so extend rather than push.
                Some(c) => c.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        }
    }
}

fn decode_ar_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Inverse of [`decode_ar_escapes`] for `SAVEAR` round-trips: newline/tab/backslash → escapes.
fn encode_ar_escapes(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\t', "\\t")
}

/// Recognize a `cd` command typed in the shell pane: `Some(None)` = bare `cd` (report cwd),
/// `Some(Some(dir))` = `cd <dir>`, `None` = not a `cd` command. Case-insensitive on the verb.
fn parse_cd(cmd: &str) -> Option<Option<&str>> {
    let t = cmd.trim();
    let mut it = t.splitn(2, char::is_whitespace);
    if !it.next()?.eq_ignore_ascii_case("cd") {
        return None;
    }
    match it.next().map(str::trim) {
        Some(rest) if !rest.is_empty() => Some(Some(rest)),
        _ => Some(None),
    }
}

/// `$DP` string for a document: its listing directory with any file mask joined on, or "".
fn dir_dp_string(doc: &Document) -> String {
    match doc.dir_state() {
        Some(d) => match &d.filter {
            Some(mask) => d.base.join(mask).to_string_lossy().into_owned(),
            None => d.base.to_string_lossy().into_owned(),
        },
        None => String::new(),
    }
}

/// A pending close-confirmation, awaiting the user's `CloseAnswer`.
enum PendingClose {
    /// `AB/V` on a modified buffer: save changes before closing?
    Save { name: String },
    /// `ABD`: delete the backing file from disk, then close the buffer.
    DeleteNote { path: PathBuf, name: String },
    /// `DEL <path>` (no `/NV`): delete a standalone file/empty dir from disk (no buffer involved).
    DeleteFile { path: PathBuf, name: String },
    /// `SAVE <path>` whose target already exists: overwrite it with a copy of the active
    /// document? The document's own path/tab is untouched either way.
    Overwrite { path: PathBuf, name: String },
    /// `ST <path>` (store) whose target already exists: overwrite it, then ADOPT the path and
    /// CLOSE the tab? (`ST/NV` skips this.)
    StoreOverwrite { path: PathBuf, name: String },
    /// `SAAS`/`SAVEAS <path>` whose target already exists: overwrite it, then ADOPT the path (the
    /// tab stays open). (`SAAS/NV` skips this.)
    SaveAsOverwrite { path: PathBuf, name: String },
    /// `SAD <path>` whose target already exists: overwrite it with the selection bytes captured when
    /// the prompt was armed (the doc is untouched). (`SAD/NV` skips this.)
    SaveSelectionOverwrite { path: PathBuf, name: String, bytes: Vec<u8> },
}

/// Normalize line endings to the OS default (CRLF on Windows, LF elsewhere) for the GENERATED
/// script/config files written by `SAVECFG`/`AUTOCFG`, `SAVECM`, and `SAVEKBD`. (Document saves —
/// `SAVE`/`SAD`/Save-As — keep their bytes verbatim and are NOT routed through here.)
fn to_os_newlines(s: &str) -> String {
    let lf = s.replace("\r\n", "\n");
    match crate::document::LineEnding::os_default() {
        crate::document::LineEnding::Crlf => lf.replace('\n', "\r\n"),
        crate::document::LineEnding::Lf => lf,
    }
}

/// A user's response at each stop of an interactive change-verify (`CV`) walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyChoice {
    /// Replace this match, then move on to the next.
    Yes,
    /// Skip this match, then move on to the next.
    No,
    /// Replace this match, then stop the walk.
    OneMore,
    /// Stop now without replacing this match; leave the caret where it is.
    Quit,
    /// Stop now without replacing this match; jump the caret back to the start.
    Esc,
}

/// In-flight interactive change-verify state (`CV`/`CVA`). Holds the compiled
/// pattern + replacement and the match currently being prompted.
struct VerifySession {
    pattern: Pattern,
    replacement: String,
    /// Where the caret was when `CV` started — `Esc` returns here (kept current as
    /// earlier replacements shift offsets).
    origin: usize,
    /// The match (rope offsets) currently shown/selected, awaiting a choice.
    current: (usize, usize),
    /// `CV/n` — stop the walk after this many changes have been made. `None` = no cap.
    limit: Option<usize>,
}

/// Live editor state the interpreter reaches through the [`Host`] trait.
pub struct AppHost {
    /// Open documents (D3: multi-document from the start). Never empty.
    pub(crate) docs: Vec<Document>,
    /// Index of the active document within `docs`.
    pub(crate) active: usize,
    /// The *alternate* buffer: the document that was active before the most recent
    /// navigation switch. `BUFALT` toggles between this and the current tab. `None`
    /// until the first switch (or after a close invalidates it).
    pub(crate) alt_active: Option<usize>,
    /// The split-pane layout tree. v1 default = one pane; the focused pane always shows the active
    /// document (kept in sync by [`sync_panes`](AppHost::sync_panes) after any change to `active`).
    /// The GUI reads [`panes`](AppHost::panes) to lay out and render (Phase 2); verbs mutate it.
    pub(crate) panes: crate::panes::PaneTree,
    /// The command-line (CMline) — also a document so editing/undo are uniform.
    pub(crate) cmline: Document,
    /// `BEDIT` blind-edit buffer: when `Some`, editing commands + content `$`-vars retarget to it
    /// (the `Document` target only — the CMline still works), while the GUI keeps rendering the
    /// visible doc. Invisible: never in `docs`, no tab, not rendered.
    pub(crate) blind: Option<Document>,
    /// PRINTEX counter snapshot: the active document's `C1..C9` registers cloned at the start of a
    /// `PRINTEX` run (by `SaveCounters`) and written back when it ends (by `RestoreCounters`), so
    /// `|Cn|` counter fields can advance during expansion without permanently disturbing the
    /// template's live counters — each run renumbers from the template's seed. `None` outside a run.
    pub(crate) saved_counters: Option<[crate::counter::Counter; 9]>,
    /// Which buffer editor ops currently target.
    pub(crate) target: Target,
    /// The user's PERSISTENT input focus (text vs cmline), for `$TX`. Distinct from `target`,
    /// which is transiently flipped to `Document` while a cmline-invoked command runs. Updated
    /// only on genuine focus changes (via `set_focus`), not those transient flips.
    pub(crate) input_focus: Target,
    /// Set by `editor_op` whenever a DOCUMENT-FOCUSING op runs (see [`is_document_focusing`]). The
    /// cmline-submit path resets it before a command and reads it after, to decide whether to land
    /// the caret in the text (a text-touching command) or return to the cmline (an exempt/neutral
    /// one). Only meaningful around a submit; harmlessly accumulates otherwise.
    pub(crate) doc_focus_ran: bool,
    /// Set when an EXPLICIT focus verb ran this command (`GT`/`FocusText`, `TH`/`ToggleFocus`) — a
    /// deliberate user choice of where focus lands. The submit/XC resting-focus heuristic must NOT
    /// override it (e.g. a later `$ERROR` in the same chain/routine mustn't drag focus back to the
    /// cmline after an explicit `GT`). Reset before each submit/XC run, like `doc_focus_ran`.
    pub(crate) explicit_focus: bool,
    /// Clipboard contents (`$CB`) — the joined/OS-visible form.
    clipboard: String,
    /// HEX-view byte clipboard — raw bytes from a hex copy/cut, byte-faithful (the text clipboard
    /// would lose non-text bytes). Used only by the hex-view copy/cut/paste branch of `do_clip`.
    hex_clipboard: Vec<u8>,
    /// Multi-caret clipboard pieces: the N selections a multi-caret COPY/CUT stored, in document
    /// order. When it holds ≥ 2 pieces AND the count matches the carets at PASTE time, paste
    /// DISTRIBUTES piece i → caret i; otherwise paste falls back to `clipboard` at every caret. Any
    /// single-caret copy or external (OS) clipboard change collapses this to one piece.
    clipboard_pieces: Vec<String>,
    /// MDA styling of the last in-app single-caret CUT/COPY, carried so a subsequent PASTE restores it
    /// (`None` when the copy was unstyled, multi-caret, or the clipboard was set externally). Dropped
    /// whenever the clipboard text is replaced from any source that can't carry styling.
    clipboard_style: Option<crate::document::StyleClip>,
    /// Deletion buffer (`$XB`) — a single-slot "kill register" filled by `RD` and
    /// pasted by `UD`; kept until the next `RD` (distinct from the OS clipboard).
    deletion_buffer: String,
    /// The most recent non-empty selection's text (`$SEL`). Captured at the top of `editor_op`
    /// just before any op that might clear the live selection (`$SE`), so the contents survive the
    /// clear. Host-global, so it persists across tab switches (unlike `$SE`, which is per-doc).
    last_selection: String,
    /// Host platform identifier (`$OS`).
    os: String,
    /// Last captured scan code (`$SC`).
    last_scan: i64,
    /// Modifier bits held for the last captured key (`$SCC` = `$SC` + this).
    last_mod_bits: i64,
    /// A keymap parsed by `LOADKBD`, awaiting installation by the editor.
    pending_kbd: Option<Keymap>,
    /// Set by `SUBMIT`; the editor runs the cmline contents after the script. Holds the cmline text
    /// SNAPSHOTTED at `SUBMIT` time, so a later `BC` in the same key chain can't blank it first.
    cmline_submit: Option<String>,
    /// Set by `XC`; the editor runs the cmline contents after the script. Also a snapshot (see
    /// `cmline_submit`), so `XC;BC` runs the command and *then* blanks — not the other way round.
    cmline_exec: Option<String>,
    /// Set by `QUIT`; the shell exits after the script returns. `Some(true)` = prompt to save
    /// dirty tabs first (`QUIT`); `Some(false)` = quit without warning (`QUIT/NV`).
    quit_request: Option<bool>,
    /// Persistent-selection mode (`$PS`) — global (applies to whichever document is
    /// active). Synced onto the active document's buffer before each editor op.
    persistent: bool,
    /// The last search pattern (lets an empty `SE` repeat the previous find).
    last_search: String,
    /// The full last search spec — pattern AND options (case/whole/regex). An empty `SE`/`SEB`
    /// (Find Next / F3, Find Prev / Shift+F3) repeats with these flags, taking only direction from
    /// the current verb. Set on every `do_search`.
    last_spec: Option<SearchSpec>,
    /// A search just landed on a match — the GUI drains this (`take_reveal_center`) and, if the match
    /// is off-screen, scrolls it to just above mid-screen instead of the minimal edge scroll.
    reveal_center: bool,
    /// `FIND` requested the GUI's Find/Replace dialog; the GUI drains this and opens it.
    find_requested: bool,
    grep_dialog_requested: bool,
    /// `SCRATCH` open requests not yet drained by the GUI — one entry per window, holding its initial
    /// text (empty, or the clipboard contents for `SCRATCH/V`).
    scratch_open_queue: Vec<String>,
    /// `SCRATCHCLOSE` requested closing the most-recently-focused scratchpad; the GUI drains this.
    /// `Some(true)` = also insert its text into the active tab at the caret (`SCRATCHCLOSE/V`).
    scratch_close_requested: Option<bool>,
    /// Full text of the most-recently-closed scratchpad window (`$SCRATCHTEXT`); empty until one closes.
    scratch_text: String,
    /// Number of replacements made by the most recent `CH`/`CV` (`$CC`).
    change_count: i64,
    /// Active interactive change-verify walk, if any (`CV`/`CVA`).
    verify: Option<VerifySession>,
    /// Viewport hint from the GUI: (top visible line, visible row count). Drives the
    /// viewport motions (`PD`/`PU`/`HM`/`BS`); `(0, 1)` until the GUI sets it.
    viewport: (usize, usize),
    /// The caret's VISUAL (wrapped) line as a rope-byte range, shaped by the GUI each frame — used by
    /// `RL` (delete visual line). `None` headless / `WRAP off` / caret off-window. Stamped with the
    /// document revision it was computed at (`visual_line_rev`) so an edit since then invalidates it.
    visual_line_span: Option<(usize, usize)>,
    visual_line_rev: u64,
    /// Virtual filesystem overlay consulted by `read_file` before the real disk —
    /// lets the closed loop be tested headlessly. Empty in the live app.
    pub files: HashMap<String, String>,
    /// When true, file I/O uses **only** the in-memory `files` map (no disk),
    /// so `OPEN`/`SAVE` round-trips are headless-testable. Default false (disk).
    pub virtual_fs: bool,
    /// The editor's working directory (`$CWD`), set by `CD`. `DIR`/`LS` list it and
    /// relative `CD` paths resolve against it. Defaults to the process cwd; managed
    /// here (not via `std::env::set_current_dir`) so it's per-instance and testable.
    pub cwd: PathBuf,
    /// The process working directory captured at launch, BEFORE `startup.xpl` can `cd` away. This is
    /// where the running instance resolved its relative config kit (`loadcfg cld.cfg`, `loadkbd …`), so
    /// a `DUPVIEW/W` child boots here to reproduce the same loads. Immutable after construction.
    pub boot_cwd: PathBuf,
    /// The startup script the running instance actually ran (the `-s <file>` argument, or the
    /// auto-discovered `startup.xpl`), set by the GUI at boot. A `DUPVIEW/W` child is launched with
    /// `-s <this>` so it runs the SAME startup even when the parent used an explicit `-s` we'd
    /// otherwise not propagate. `None` when the instance booted with no startup script.
    pub startup_path: Option<PathBuf>,
    /// Per-drive remembered current directory (upper-cased drive letter → last cwd on that drive),
    /// so a bare drive change (`D:`) returns to where you were on that drive — like `cmd.exe` —
    /// instead of its root. Updated on every cwd change; seeded with the startup drive.
    drive_dirs: HashMap<char, PathBuf>,
    /// `DIRNW` — when true (`DIRNW on`), opening a file from an in-TAB directory listing opens it in
    /// a **new** tab; when false (`DIRNW off`, the default), it replaces the listing in place. Read
    /// back via `$DIRNW`.
    new_window: bool,
    /// `DIRWNW` — the same choice for the separate `DIR/W` WINDOW. True (`DIRWNW on`, the DEFAULT)
    /// opens the picked file in a new tab, which is how `DIR/W` has always behaved; false reuses the
    /// active tab. Kept separate from `DIRNW` because they are different interactions: an in-tab
    /// listing replaces ITSELF (a read-only buffer, safe to discard), whereas `DIR/W` floats beside
    /// whatever you are editing — so `off` never discards a MODIFIED buffer, it opens alongside it
    /// instead. Read back via `$DIRWNW`.
    dir_window_new_tab: bool,
    /// `DIRWCLOSE` — whether the `DIR/W` window closes once you open a file from it. False
    /// (`DIRWCLOSE off`, the DEFAULT) keeps it open to carry on picking, which is how `DIR/W` has
    /// always behaved; true dismisses it as soon as a file opens. Independent of `DIRWNW` (which
    /// chooses the TAB): the two answer different questions. Descending into a SUBDIRECTORY never
    /// closes the window — only opening a file does. Read back via `$DIRWCLOSE`.
    dir_window_close_on_open: bool,
    /// `DIRRT` — when true (`DIRRT on`), a `DIR` command reuses an already-open listing tab instead
    /// of spawning a new one each time (avoids a proliferation of listing tabs). Default false
    /// (`DIRRT off` = a new tab per `DIR`). Read back via `$DIRRT`.
    dir_reuse: bool,
    /// `DIRSET` — default sort/filter/view switch names for `DIR`/`LS` (each a validated `DirOpts`
    /// switch like `"OD"`, `"R"`), applied to a command that doesn't override the same category.
    /// Empty = no defaults. Read back via `$DIRSET`; persisted by `SAVECFG`.
    dir_defaults: Vec<String>,
    /// `SESSION` — auto-save-and-restore the set of open tabs across runs (Notepad++-style). When on,
    /// the GUI writes a session on graceful exit and restores it at startup; dirty/untitled buffers
    /// keep SHADOW backups so the user's real files are never touched. Off by default. `$SESSION`.
    session_restore: bool,
    /// Change-detection for the periodic session snapshot (Phase 2, crash resilience): the buffer
    /// revision last written to each doc's shadow backup, so an unchanged buffer isn't rewritten
    /// every tick. Keyed by the stable `DocId`. Session-only.
    session_snapshot_revs: std::collections::HashMap<crate::panes::DocId, u64>,
    /// The last session manifest written to disk — so a snapshot with nothing new skips the write.
    last_session_manifest: Option<String>,
    /// `TITLEBAR` — whether the OS window title bar is shown (default true). The GUI runtime applies
    /// it via `set_decorations`; read back via `$TITLEBAR`. Persisted by SAVECFG.
    show_titlebar: bool,
    /// `WV` — Writer's View: a distraction-free mode where the GUI hides ALL chrome (title bar, menu
    /// bar, tab strip, command line, status line), leaving only the text. Transient (NOT persisted —
    /// you never launch stuck in it); `Esc` turns it off. Read back via `$WV`.
    writer_view: bool,
    /// `WINGEO` — whether the GUI remembers the main window's size + position across runs (default
    /// true). When off, the runtime neither restores nor saves `window.txt` (each launch uses the
    /// default size). Useful on mixed-DPI multi-monitor setups where a remembered physical size is
    /// reinterpreted wrongly on a different-scale display. Read back via `$WINGEO`; SAVECFG-persisted.
    remember_window_geometry: bool,
    /// A delete-with-confirm armed in the browser by the `Del` key: `(path, name)`
    /// awaiting a y/n. The GUI shows the prompt and routes the answer to
    /// [`dir_resolve_delete`](Self::dir_resolve_delete).
    pending_delete: Option<(PathBuf, String)>,
    /// Whether shell-out (`X`/`XY`/`XV`) is enabled this session (`TRUST 1`; `$TR`).
    /// Default false — a stray loaded macro can't run commands until the user opts in.
    shell_trusted: bool,
    /// Exit code of the last shell-out (`$RC`).
    last_rc: i64,
    /// Merged stdout+stderr of the last shell-out (`$XO`).
    last_output: String,
    /// Full path of the last keymap loaded with `LOADKBD` (`$KB`); `None` until one loads.
    last_kbd: Option<PathBuf>,
    /// The message from the last FAILED `LOADKBD`, or `None` if the most recent load succeeded. Recorded
    /// on the host (not just the transient status line) so a `startup.xpl` keymap failure survives to be
    /// surfaced after startup — later commands and nested `LOADCFG`/`THEME` runs can't clobber it.
    last_kbd_error: Option<String>,
    /// Resident keyboard-layout slots for modal / multi-layout switching (`KBD <name> <file>` loads
    /// one; `KBDMODE <name>` switches). Each is the parsed `.kbd` kept in memory so a switch is a
    /// pointer swap (no disk read / parse). The operative keymap is always `kbd_modes[kbd_mode]`.
    kbd_modes: HashMap<String, Keymap>,
    /// Name of the active layout slot (`$KBDMODE`). Starts as `default` — the keymap the editor booted
    /// with (embedded default or a startup `LOADKBD`). A plain `LOADKBD`/`CLEARKBD` replaces whichever
    /// slot is active; `KBDMODE` selects another resident slot.
    kbd_mode: String,
    /// The last feedback message surfaced to the status line (`$LS`); the editor records it.
    last_status_line: String,
    /// XPL awaiting execution by the editor after the current script returns (can't
    /// re-enter the interpreter mid-dispatch; same deferral as `SUBMIT`). A QUEUE so
    /// multiple deferrals in one dispatch (`&A;&B`, or `RUNHERE` + a binder) all run, and
    /// nested `&X`→`&Y` chains accumulate. Sources: `RUNHERE` and script binders (`&X`).
    /// Each entry is `(source, positional_args)`; args are the `RUNHERE [args…]` operands
    /// (`$1..$N` for the block) and empty for binders / config runs.
    pending_run: Vec<(String, Vec<String>)>,
    /// Text binders `@A`–`@Z`/`@0`–`@9` (`LDTX`): inserted at the caret by `@X`.
    text_binders: HashMap<char, String>,
    /// Script binders `&A`–`&Z`/`&0`–`&9` (`LDSC`/`LDPM`): run as XPL by `&X`.
    script_binders: HashMap<char, String>,
    /// Stopwatch origin (`$ET`/`ET`/`ETS` measure from here; `ZT` resets it).
    stopwatch_start: std::time::Instant,
    /// `(text, timeout_secs)` a `MSG` verb wants shown in a GUI message box, awaiting the GUI.
    /// `timeout_secs = Some(n)` self-dismisses after `n` seconds; `None` stays until closed.
    pending_message: Option<(String, Option<u64>)>,
    /// `SAAS` with no path asked for the native Save-As dialog; the GUI drains this.
    save_as_requested: bool,
    /// `OPEN` with no path asked for the native Open-file dialog; the GUI drains this. `Some(hex)`
    /// carries the `/HEX` intent (open the chosen file in the hex view); `None` = no request.
    open_requested: Option<bool>,
    /// `EDFONTS`/`UIFONTS`/`TXTFONTS` asked for the GUI font-picker dialog; the GUI drains this.
    font_picker_request: Option<FontPickerKind>,
    /// Bare `MAN` set this; the GUI polls `take_show_manual_request` and opens the manual.
    show_manual_request: bool,
    /// `*COLORS`/`HLCOLORS <kind>` asked for the GUI colour-picker dialog; the GUI drains this.
    color_picker_request: Option<ColorTarget>,
    /// A pending `AB/V` save-prompt or `ABD` delete-confirm; the GUI shows it + sends the answer.
    pending_close: Option<PendingClose>,
    /// A pending "file is already open" prompt (`OPEN`/`READ` of a file open in another tab); the
    /// GUI shows it on the status line and sends back a `OpenAnswer`.
    pending_open: Option<PendingOpen>,
    /// Autosave interval in seconds (`AUTOSAVE`; 0 = off; read as `$AU`). GUI does the timing.
    autosave_secs: u64,
    /// Tab width in spaces (`$TB`), set by `TB <n>`.
    tab_width: usize,
    /// Soft tabs (`TABSPACES`): `TAB`/indent inserts `tab_width` spaces instead of a tab. `$TABSPACES`.
    tab_soft: bool,
    /// Auto-indent (`TABINDENT`): `ENTER` starts the new line at the previous line's indent. `$TABINDENT`.
    tab_indent: bool,
    /// Matching-bracket highlight (`BALPAIRS`): highlight the partner of the `()[]{}` at the caret.
    /// `$BALPAIRS`.
    bal_pairs: bool,
    /// The matched-bracket highlight colour (`BALPAIRSCOLOR`; `None` = use the selection colour).
    /// `$BALPAIRSCOLOR`.
    bal_color: Option<(u8, u8, u8)>,
    /// The `EMPH` emphasis glyph / background colours (`EMPHFGCOLOR`/`EMPHBGCOLOR`; `None` = follow
    /// the selection colours, `SELFGCOLOR`/`SELBGCOLOR`). `$EMPHFGCOLOR`/`$EMPHBGCOLOR`.
    emph_fg: Option<(u8, u8, u8)>,
    emph_bg: Option<(u8, u8, u8)>,
    /// Auto-close typed pairs (`TYPEPAIRS`): typing a left `()[]{}"'` inserts the partner; Backspace
    /// deletes an empty pair. Interactive typing only (not the scripted `INSERT`). `$TYPEPAIRS`.
    type_pairs: bool,
    /// Hanging indent (`WRAPINDENT`): soft-wrapped continuation rows align under the line's own
    /// indentation (`$INDENTED`) rather than the left margin. Rendering-only. `$WRAPINDENT`.
    wrap_indent: bool,
    /// Fixed decimal places for displaying floats (`$DECIMALS`), set by `DECIMALS <n>`/`off`.
    /// `None` = default precision. Applied when values are stringified for display (substitution
    /// / `$(…)`) via the `EvalContext::float_decimals` hook; computation keeps full precision.
    float_decimals: Option<u8>,
    /// Accumulated viewport scroll (`MU`/`MD`) in visual rows, awaiting the GUI: positive =
    /// toward the document end (text up), negative = toward the start (text down).
    pending_scroll_rows: i32,
    /// A pending visual-line-edge refine (`LB`/`LE`) for the GUI: `(end, extend, from)` where
    /// `end` selects the visual-row end vs start, `extend` grows the selection, and `from` is the
    /// caret offset BEFORE the engine's logical-edge fallback (so the GUI finds the right visual
    /// row). Drained each frame by the GUI; ignored (logical fallback stands) headless/TUI.
    pending_visual_edge: Option<(bool, bool, usize)>,
    /// `CARETMOTION visual`: a deferred VISUAL Left/Right move `(right, extend, from)` — the pre-move
    /// caret, which the GUI refines to the visual (screen-order) neighbor via cosmic's layout. Set only
    /// when `caret_visual` is on; the engine still does the logical move as a headless/off-window fallback.
    pending_visual_move: Option<(bool, bool, usize)>,
    /// The GUI-published VISUAL-ROW MAP for the active document's window: rope offsets of each wrapped
    /// row's start, with a trailing sentinel = the end of the last row. `LU`/`LD` (`Motion::VisualUp`/
    /// `VisualDown`) step over these instead of logical lines, so they move by screen row inside
    /// `;`-chains / macros / GUI-run scripts — not just the bare interactive form. `visual_rows_rev` is
    /// the doc revision it was built at; a stale map (an edit bumped the rev) or a headless session (no
    /// GUI → empty) falls back to the logical move. Published each frame by the GUI.
    visual_rows: Vec<usize>,
    visual_rows_rev: u64,
    /// A clone of the operative keymap, kept in sync by the editor on every install — so `SAVEKBD`
    /// can serialize it here (the authoritative copy lives in the editor's resolver). Desugared,
    /// like the resolver's; the dumper re-sugars typing cells.
    current_keymap: Option<Keymap>,
    /// Bumped on every keymap install, so views (the Help keyboard chart) can cache and rebuild only
    /// when the layout actually changes rather than every frame.
    keymap_gen: u64,
    /// `AUTOCFG`: the resolved path the config is being live-tracked to (`None` = off). `$CFA`.
    config_track_path: Option<String>,
    /// The resolved path of the most recent `LOADCFG` (persists; `None` = none yet). `$CF`.
    last_loadcfg_path: Option<String>,
    /// UI translation table (`LANG`): `string-id -> localized text`. Empty = built-in English. The
    /// GUI looks up menu/dialog labels here, falling back to English on a miss. i18n phase-1 spike.
    lang: HashMap<String, String>,
    /// The (resolved) path of the catalog loaded by `LANG`, or `None` when English. Read back as
    /// `$LANG`; persisted by `SAVECFG` as a re-runnable `LANG <path>` line so the choice restores.
    lang_path: Option<String>,
    /// The `<code>.local.lang` OVERRIDE file layered on top of the base catalog, if one was found
    /// (`None` otherwise). A user keeps only their changed lines here; it wins over the base and
    /// survives upgrades (never shipped). Not persisted — re-discovered from the base code on reload.
    lang_overlay_path: Option<String>,
    /// `QUIET` status-suppression level for the current script: 0 = off, 1 = hide op-result
    /// statuses (PR + errors still show), 2 = also hide non-fatal error text. Reset per top-level
    /// script run (the editor clears it at the start of each outermost dispatch).
    quiet: u8,
    /// The settings-only config script last written by tracking, to skip redundant rewrites.
    config_last_written: String,
    /// Top visible logical line of the GUI viewport, pushed by the GUI each frame; `$CY` =
    /// cursor's logical row relative to it. 0 in headless / before the GUI sets it.
    viewport_top: usize,
    /// The caret's VISUAL position `(col, row)` — wrap-aware screen coords — pushed by the GUI each
    /// frame for `$CX`/`$CY`. Row is relative to the viewport top. `None` headless/before the GUI
    /// sets it → `$CX`/`$CY` fall back to logical column / viewport-relative logical row.
    cursor_vis: Option<(i64, i64)>,
    /// Running background shell jobs (`X/BG`), drained by `poll_jobs`. `$JOBS` = count.
    jobs: Vec<Job>,
    next_job_id: u64,
    /// The in-flight streaming `GREP` search, if any (drained by `poll_grep`). At most one at a time —
    /// a new `GREP` cancels the previous.
    grep_job: Option<GrepJob>,
    /// `GREPBEFORE` / `GREPAFTER` — leading / trailing context lines shown around each `GREP` match
    /// (0 = none). `GREPBLANK` — insert a blank line between result groups. `$GREP*`; SaveCfg.
    grep_before: usize,
    grep_after: usize,
    grep_blank: bool,
    /// The source buffer id of the diff currently shown in the shared results tab (set by `DIFF`,
    /// cleared whenever HELP/MAN/COLORNAMES rebuild that tab). `DJ` reads it to jump from a hunk line
    /// back into the live buffer; `None` means the results tab isn't a current diff.
    diff_source: Option<u32>,
    /// The id of the most-recently-started background job (`X/BG` or `RUN/BG`); `0` = none yet.
    /// Read as `$LJ` so a script can `WAIT/J $LJ` on the job it just launched.
    last_job_id: u64,
    /// `(var, output)` deliveries from finished `/R:<var>` jobs, awaiting the editor (which owns the
    /// interpreter's `Env`) to write each into its variable. Populated by `poll_jobs` on completion.
    pending_returns: Vec<(String, String)>,
    /// Submitted CMline commands, **newest first** (index 0 = most recent), for Up/Down recall +
    /// the `HISTORY` picker.
    command_history: Vec<String>,
    /// Recently-closed files for the `REOPEN` facility: `(path, caret-byte-offset)`, **newest first**,
    /// deduped by path, capped at [`REOPEN_CAP`]. Recorded when a named file buffer is closed (not a
    /// deleted/unnamed/listing buffer). Persisted via `SaveClosed`/`LoadClosed` + SAVECFG.
    recently_closed: Vec<(PathBuf, usize)>,
    /// Index into `command_history` while walking with Up/Down (`None` = at the in-progress line).
    history_pos: Option<usize>,
    /// The in-progress CMline text saved when history navigation starts (restored by Down).
    history_draft: String,
    /// `HISTORY`/`HI [arg]` asked the GUI to open the picker (with this filter arg); GUI drains it.
    history_request: Option<String>,
    /// `MENU [letter]` — one-shot request for the GUI to drive the menu bar by keyboard. Outer
    /// `Some` = a request is pending this frame; inner `None` = bare `MENU` (toggle), inner
    /// `Some("f")` = open the top menu with accelerator letter `f`.
    menu_kbd_request: Option<Option<String>>,
    /// GUI view flags (the GUI reads them each frame). `$MB`/`$VR`/`$WS`/`$LNS`/`$GC`.
    menu_bar: bool,
    visible_returns: bool,
    visible_whitespace: bool,
    line_numbers: bool,
    /// Show the prominent pop-up command line (alongside the inline cmline) while focused.
    gui_cmline: bool,
    /// Whether the `*Shell Output*` tab is shown in the tab strip (`SW`/`SHELLWIN`; `$SW`; default
    /// on). When off, background jobs keep streaming into the (hidden) buffer — the GUI just filters
    /// the tab out. Toggling does not affect job execution, `$XO`/`$RC`, `JOBS`, or scrollback.
    shell_output_visible: bool,
    /// Transient: set when `SW off` hid the tab *while it was focused*, so the next `SW on` restores
    /// focus to it (remember-where-I-was). Cleared once consumed. Not a setting — not in SaveCfg.
    shell_focus_restore: bool,
    /// The separate `DIR/W` listing window's state (`None` = closed). A single reusable window.
    dir_window: Option<DirWindowState>,
    /// The live PDF preview pane on the right (`PREVIEW`/`PV`; `$PV`; default off). GUI-only state.
    preview_visible: bool,
    /// Console REPL (unified successor to the SH pane + `*Shell Output*`): the prompt template
    /// (`SHPROMPT`; `$`-expanded like `$CWD> `), whether a prompt is currently displayed and awaiting
    /// input, and — while it is — the byte offset of the prompt's start and the prompt's byte length
    /// (so the input is `[prompt_at + prompt_len .. end]`). Streamed output splices in ABOVE
    /// `console_prompt_at`, keeping the prompt + input pinned at the bottom.
    console_prompt: String,
    console_showing: bool,
    console_prompt_at: usize,
    console_prompt_len: usize,
    /// Set whenever Console output or a new prompt is written; the GUI drains it each frame and, when
    /// the Console is the active tab, scrolls to the tail so streamed output + the new prompt stay in
    /// view (otherwise foreground output appends below the visible prompt and streams off-screen).
    console_dirty: bool,
    /// A deliberate "jump to the Console prompt" just happened (`SH`/`SHELL` opened or re-focused the
    /// Console). Unlike `console_dirty`, this FORCES the view to the tail even if the user had scrolled
    /// up to read old output (scroll-lock is bypassed for an explicit open) — the whole point of `SH`
    /// is to land the user at the prompt. Drained by the GUI's `take_console_reveal_tail`.
    console_reveal_tail: bool,
    /// Interactive stdin: while a foreground Console command is running, the sender for its stdin pump
    /// (`None` when nothing is running). Enter sends the current input line here; Ctrl+Z/Ctrl+D sends
    /// `Eof`. `console_input_at` is the byte offset where the user's current input begins — the end of
    /// the last streamed output / echo — so the line sent is `[console_input_at .. end]` (never the
    /// child's own prompt text, which sits before it).
    console_stdin: Option<std::sync::mpsc::Sender<ConsoleStdin>>,
    console_input_at: usize,
    /// Per-session Console command history (oldest first), and the current recall position
    /// (`None` = not recalling / at the fresh input line). Shift+Up/Shift+Down walk it, replacing the
    /// prompt input. Session-only — never persisted.
    console_history: Vec<String>,
    console_hist_pos: Option<usize>,
    /// The last `CALC` result string, read back as `$CALC`. Session-only.
    last_calc: String,
    /// The running total for cursor addition (`CLRSUM`/`ADDSUM`/`PUTSUM`), read back as `$SUM`. Kept as
    /// a `Number` (full precision; formatted with `DECIMALS` on read/put). Session-only, global.
    sum_total: Number,
    /// Streaming ANSI-SGR parser for Console output: escapes are stripped and turned into MDA colour /
    /// attribute spans. Reset between commands (and before our own `(exit N)`/`^C`) so leftover colour
    /// never bleeds across.
    console_ansi: crate::ansi::AnsiState,
    /// Whether Console output is ANSI-coloured (`ANSICOLOR`; `$ANSICOLOR`; SaveCfg; default on). Off
    /// leaves the raw escape codes in the text (for debugging / a plain transcript).
    ansi_color: bool,
    /// Auto-replace map (`AR`/`LoadAR`): `key word → replacement text`. `AR` replaces the word behind
    /// the caret when it matches a key (case-sensitive, exact). Loaded files merge into this.
    auto_replace: std::collections::HashMap<String, String>,
    /// The path most recently loaded by `LoadAR` (`$AR`); cleared by `CLEARAR`. `None` = nothing loaded.
    last_ar_path: Option<String>,
    /// XPL queued by an `AR` whose replacement value started with `#` (the key is already deleted);
    /// the VM drains it via `take_ar_xpl`, runs it, and aborts the rest of the `AR` chain.
    ar_pending_xpl: Option<String>,
    // GUI sizing knobs (B16; the GUI reads + applies them). `$UISCALE/$UIFONT/$EDFONT/$LSP/$OF`.
    ui_scale: f32,
    chrome_font: f32,
    editor_font: f32,
    leading: f32, // line spacing, percent of the editor font
    margin_left: f32, // left/right margins in char columns (`OFL`/`OFR`; `OF` sets both)
    margin_right: f32,
    txt_font: f32, // `TXT` dialog text size (pt) and zoom; `$TXTFONT`/`$TXTSCALE`
    txt_scale: f32,
    /// Editor canvas font family (`EDFAMILY`; monospaced). `None` = the GUI's resolved default.
    /// The GUI renders with it via cosmic-text. `$EDFAMILY`.
    editor_font_family: Option<String>,
    /// Editor PROPORTIONAL (variable-width) canvas font family (`EDPROPFAMILY`; any family). `None` =
    /// unset → prose buffers stay monospace. The GUI renders prose in it when the per-document font mode
    /// resolves to proportional (`USEFONT`). `$EDPROPFAMILY`.
    editor_prop_family: Option<String>,
    /// UI-chrome font family (`UIFAMILY`; menu/status/cmline). `None` = egui's built-in default.
    /// The GUI loads the face bytes into egui to apply it. `$UIFAMILY`.
    ui_font_family: Option<String>,
    /// `TXT` window font family (`TXTFAMILY`). `None` = egui's built-in default monospace.
    /// The GUI loads the face bytes into the TXT window's egui context. `$TXTFAMILY`.
    txt_font_family: Option<String>,
    /// Font styles (`EDSTYLE`/`UISTYLE`/`TXTSTYLE`; `$EDSTYLE`/`$UISTYLE`/`$TXTSTYLE`). The GUI maps
    /// each to a face weight/slant — editor via cosmic `Attrs`, UI/TXT by loading the styled face.
    /// UI/TXT styles only take effect when the matching `*FAMILY` is set (egui needs a face to load).
    editor_font_style: FontStyle,
    ui_font_style: FontStyle,
    txt_font_style: FontStyle,
    // Editor colours (the GUI rasters with them). `$FGCOLOR`/`$BGCOLOR`. RGB 0-255.
    fg_color: (u8, u8, u8),
    bg_color: (u8, u8, u8),
    // UI-chrome colours (the GUI themes egui with them). `$UIFGCOLOR`/`$UIBGCOLOR`. RGB 0-255.
    ui_fg_color: (u8, u8, u8),
    ui_bg_color: (u8, u8, u8),
    // Text caret appearance (`CRCOLOR`/`CRSHAPE`; the GUI paints with them). `$CRCOLOR`/`$CRSHAPE`.
    caret_color: (u8, u8, u8),
    caret_shape: CaretShape,
    /// Command-line caret shape (`CMCRSHAPE`; the GUI paints the cmline caret with it). `$CMCRSHAPE`.
    cmline_caret_shape: CaretShape,
    /// Command-line caret colour (`CMCRCOLOR`; the GUI paints the cmline caret with it). `$CMCRCOLOR`.
    /// Defaults to (and is re-set by `THEME` to) the text caret colour; set it to diverge.
    cmline_caret_color: (u8, u8, u8),
    /// Where the command line is drawn (`CMPLACE`; the GUI renders from this). `$CMPLACE`.
    cmline_placement: CmlinePlacement,
    /// Draw the status line at the TOP, just under the command line (XyWrite-style) instead of the
    /// default bottom edge (`STATUSLINE top|bottom`; the GUI renders from this). `$STATUSLINE`.
    status_line_top: bool,
    /// Where the `$CWD` path is shown — status line or command-line prefix (`PAPLACE`; `$PAPLACE`).
    path_place: PathPlace,
    /// Max width (chars) of the displayed `$CWD` path before it's truncated (`PATHLEN`; `$PATHLEN`).
    path_len: usize,
    /// User-added text-file extensions (`TEXTFILES`; `$TEXTFILES`), lowercased and dot-stripped —
    /// appended to the built-in list the File ▸ Open dialog's "Text files" filter matches. Empty by
    /// default (`OPEN <path>` opens any file regardless; this only widens the dialog's convenience filter).
    text_file_exts: Vec<String>,
    /// Whether the command line is blanked after a command runs from it (`CMBLANK`; `$CMBLANK`).
    /// Off (default) keeps the command for re-running / editing (XyWrite B23).
    cmline_blank: bool,
    /// `CARETMOTION`: whether Left/Right caret motion is VISUAL (screen order). `false` (default) =
    /// logical (storage) order; `true` defers Left/Right to the GUI's cosmic layout so they follow the
    /// on-screen reading flow. `$CARETMOTION` reads back `logical`/`visual`.
    caret_visual: bool,
    /// Whether the tab strip is shown when only ONE file is open (`SHOWONETAB`; `$SHOWONETAB`).
    /// Off (default) keeps a single-document view uncluttered.
    show_one_tab: bool,
    /// Whether long lines word-wrap to the window (`WRAP`; `$WRAP`). On (default) wraps; off lets lines
    /// run past the right edge, and the GUI shows a horizontal scrollbar to reach the overflow.
    wrap: bool,
    /// Hanging indent (in spaces) for wrapped continuation rows (`WRAPHANG`; `$WRAPHANG`). 0 (default) =
    /// flush wrap. Applies only when `WRAP` is on and `WRAPINDENT` is off (which auto-hangs instead).
    wrap_hang: usize,
    /// Default column width `REFLOW` hard-wraps to when given no explicit width (`REFLOWWIDTH`;
    /// `$REFLOWWIDTH`; persisted by SAVECFG). 65 by default.
    reflow_width: usize,
    /// Whether the blank "Untitled" scratch tab is auto-closed when a document is opened — and any
    /// leftover empty untitled tab pruned — so a scratch never lingers beside real files (it reappears
    /// when the last document closes). On (default). (`UNTITLEDTAB`; `$UNTITLEDTAB`.)
    untitled_auto_close: bool,
    /// Whether opening an already-open file skips the "already open" prompt and switches to the
    /// existing tab. Off (default) prompts. (`GOTOOPEN`; `$GOTOOPEN`.)
    goto_open: bool,
    /// How the bookmark gutter is shown: `Off` (never), `On` (only when the doc has bookmarks; the
    /// default), or `Persist` (always, giving a stable left margin). (`MARKGUTTER`; `$MARKGUTTER`;
    /// SaveCfg.) A pure display setting — bookmarks still work regardless.
    mark_gutter: MarkGutterMode,
    /// Bookmark-gutter colours (`MARKFGCOLOR`/`MARKBGCOLOR`). `None` = default: the dot is bookmark
    /// orange, the strip background is transparent. `$MARKFGCOLOR`/`$MARKBGCOLOR`; SaveCfg.
    mark_fg_color: Option<(u8, u8, u8)>,
    mark_bg_color: Option<(u8, u8, u8)>,
    /// Line-number gutter colours (`LNFGCOLOR`/`LNBGCOLOR`). `None` = default: the numbers are a muted
    /// slate, the gutter background is transparent. `$LNFGCOLOR`/`$LNBGCOLOR`; SaveCfg.
    line_num_fg_color: Option<(u8, u8, u8)>,
    line_num_bg_color: Option<(u8, u8, u8)>,
    /// Modal `MSG` box colours (`MSGHFGCOLOR`/`MSGBGCOLOR`/`MSGOKFGCOLOR`/`MSGOKBGCOLOR`). `None` =
    /// follow the theme. `$MSGHFGCOLOR`/`$MSGBGCOLOR`/`$MSGOKFGCOLOR`/`$MSGOKBGCOLOR`; SaveCfg.
    msg_header_fg_color: Option<(u8, u8, u8)>,
    msg_bg_color: Option<(u8, u8, u8)>,
    msg_ok_fg_color: Option<(u8, u8, u8)>,
    msg_ok_bg_color: Option<(u8, u8, u8)>,
    /// Whether URLs in the document are highlighted and Ctrl+click-to-open (`URLHIGHLIGHT`). Off by
    /// default. `$URLHIGHLIGHT`; SaveCfg.
    url_highlight: bool,
    /// URL-highlight colours (`URLFGCOLOR`/`URLBGCOLOR`). `None` = default: link-blue text, transparent
    /// background (no band). `$URLFGCOLOR`/`$URLBGCOLOR`; SaveCfg.
    url_fg_color: Option<(u8, u8, u8)>,
    url_bg_color: Option<(u8, u8, u8)>,
    /// Whether the preview/`TOPDF` render also paints the transient EMPH matches and URL highlights
    /// (`PVMARKS`). Off by default — a printout stays clean unless asked. MDA styling is unaffected
    /// (always rendered). `$PVMARKS`; SaveCfg. Applies to the styler-based kinds (GFM/Typst/Text).
    preview_marks: bool,
    /// Auto-reopen a file when it changes on disk (`AUTOREOPEN`). Off by default. `$AUTOREOPEN`; SaveCfg.
    /// Independent of `TRUST` — it only re-reads a file already open, running no external code.
    auto_reopen: bool,
    /// Append every trappable + fatal error to a log file (`ERRORLOG`). Off by default. `$ERRORLOG`; SaveCfg.
    error_log: bool,
    /// The error-log file path (`ERRORLOGFILE`; `None` = the default `xyscript-errors.log` in `$XSDIR`).
    /// A set path is stored as given and resolved against `$CWD` at write time. `$ERRORLOGFILE`; SaveCfg.
    error_log_file: Option<String>,
    /// Dim the window slightly when it loses focus (`DIMONBLUR`). Off by default. `$DIMONBLUR`; SaveCfg.
    dim_on_blur: bool,
    /// When on (`CLOSEBACK`), closing a tab (`AB`) returns to the LAST-VISITED tab instead of the left
    /// neighbor. Off by default. `$CLOSEBACK`; SaveCfg.
    close_back: bool,
    /// The tab that was active just BEFORE the current one (by stable `DocId`), updated on every
    /// deliberate switch AND on opening a new tab. `CLOSEBACK` closes back to this. Session-only.
    focus_prev: Option<crate::panes::DocId>,
    /// Last-seen on-disk modification time per open file (keyed by the stable `DocId`), for
    /// `AUTOREOPEN` change detection. Baselined on first poll and after each save/reload. Session-only.
    disk_mtimes: HashMap<crate::panes::DocId, std::time::SystemTime>,
    /// Whether `MDA` UNDERLINE styling underlines the spaces between words (`ULSPACES`; `$ULSPACES`).
    /// On (default) = one continuous underline; off = broken at spaces/tabs (word glyphs only).
    ul_spaces: bool,
    /// SMTP config for `SENDMAIL` (email feature). Session-only — `mail_pass` is NEVER persisted; set
    /// it each session (or leave empty to fall back to the `XYSCRIPT_MAILPASS` env var).
    #[cfg(feature = "email")]
    smtp_url: String,
    #[cfg(feature = "email")]
    mail_user: String,
    #[cfg(feature = "email")]
    mail_from: String,
    #[cfg(feature = "email")]
    mail_pass: String,
    /// `MAILDEBUG on` runs curl with `-v` so the full SMTP dialogue (incl. the server's final
    /// `250 … queued as …`) streams to *Shell Output* — for diagnosing deliverability. (`$MAILDEBUG`.)
    #[cfg(feature = "email")]
    mail_debug: bool,
    /// Files staged by `ATTACH` for the next `SENDMAIL` (absolute paths, resolved at attach time; read
    /// at send time, then cleared). `$ATTACH` lists them. (email feature.)
    #[cfg(feature = "email")]
    mail_attachments: Vec<std::path::PathBuf>,
    /// IMAP receive config: `IMAPURL` (`imaps://host:993`) and `MAILBOX` (default `INBOX`) for
    /// `LISTMAIL`/`SAVEMAIL`/`DELMAIL`; auth reuses `mail_user`/`mail_pass`. (email feature.)
    #[cfg(feature = "email")]
    imap_url: String,
    #[cfg(feature = "email")]
    mailbox: String,
    /// `SAVEMAIL all <path>` is two-phase: a background FETCH-headers job counts the mailbox, then its
    /// completion (keyed here by job id) spawns the real per-message download into this directory.
    #[cfg(feature = "email")]
    mail_save_all: Vec<(u64, std::path::PathBuf)>,
    // UI separator colour (`SEPCOLOR`): the horizontal panel rules (menu/tabs/cmline/status) and
    // the vertical bars between tabs. The GUI themes egui's separator stroke with it. `$SEPCOLOR`.
    sep_color: (u8, u8, u8),
    // Selection colours: the band painted behind selected text (`SELBGCOLOR`) and the glyph colour
    // of the selected text itself (`SELFGCOLOR`). The GUI passes both to cosmic's `draw`.
    // `$SELBGCOLOR`/`$SELFGCOLOR`.
    sel_bg_color: (u8, u8, u8),
    sel_fg_color: (u8, u8, u8),
    find_bg_color: (u8, u8, u8),
    find_fg_color: (u8, u8, u8),
    /// Underline colours for live language-check issues — spelling (`SPELLCOLOR`) vs grammar/other
    /// (`GRAMMARCOLOR`). `$SPELLCOLOR`/`$GRAMMARCOLOR`.
    spell_color: (u8, u8, u8),
    grammar_color: (u8, u8, u8),
    /// Underline STYLE for live language-check issues — spelling (`SPELLSTYLE`) vs grammar
    /// (`GRAMMARSTYLE`): dotted (default) / solid / squiggle. `$SPELLSTYLE`/`$GRAMMARSTYLE`.
    spell_style: UnderlineStyle,
    grammar_style: UnderlineStyle,
    /// Border colour drawn around the FOCUSED split pane (`PANECOLOR`; inactive panes use `sep_color`).
    /// `$PANECOLOR`. Re-set by `THEME`.
    pane_border_color: (u8, u8, u8),
    /// Live SPELLING underlines on? (`SPELLING`/`SP on|off`, `$SPELLING`.) (Language-check marks live
    /// PER-DOCUMENT now — on each [`Document`] — so a split shows each pane's own marks.)
    spell_on: bool,
    /// Live GRAMMAR/word-choice underlines on? (`GRAMMAR`/`GR on|off`, `$GRAMMAR`.) The worker runs
    /// whenever EITHER flag is on and produces all kinds; the flags gate which marks are drawn.
    grammar_on: bool,
    /// The English dialect harper checks against (`DIALECT`, `$DIALECT`). Changing it respawns the
    /// worker (the linter is dialect-bound) and forces a re-check.
    lint_dialect: crate::lint::Dialect,
    /// Which checker produces SPELLING underlines (`SPELLENGINE`): `Harper` (built-in) or `Hunspell`
    /// (the CLI, for multilingual spelling). Changing it respawns the worker + forces a re-check.
    spell_engine: crate::lint::SpellEngine,
    /// Hunspell dictionaries for its `-d` flag (`SPELLDICT`, e.g. `en_US,de_DE`). Default `en_US`.
    spell_dicts: String,
    /// The Hunspell executable (`SPELLBIN`): a name on `PATH` or a full path. Default `hunspell`.
    spell_bin: String,
    /// The user's learned words (`LEARN`/`UNLEARN`) — merged into the checker's dictionary so they
    /// stop being flagged. Sorted/deduped; persisted to a dictionary file beside the executable.
    learned_words: std::collections::BTreeSet<String>,
    /// `(active doc index, revision, region_lo, region_hi)` of the last SYNCHRONOUS `recheck_lints` —
    /// the staleness key for `lint_stale` (the sync/headless path; the GUI drives the async worker).
    lint_key: Option<(usize, u64, usize, usize)>,
    /// The background lint worker (spawned lazily on first `request_lint`). `None` until then.
    #[cfg(feature = "lint")]
    lint_worker: Option<LintWorker>,
    /// The request currently out at the worker (so its result can be validated + mapped), or `None`.
    #[cfg(feature = "lint")]
    lint_pending: Option<LintPending>,
    /// An in-flight background `CHECK` (whole-doc report on its own thread) — the GUI shows "Working…"
    /// until its result lands, then opens the results tab. `None` = no check running.
    #[cfg(feature = "lint")]
    check_pending: Option<std::sync::mpsc::Receiver<String>>,
    /// Per document, the `(rev, lo, hi)` view its underlines currently SATISFY — so `request_lint`
    /// skips a doc whose visible window is already checked (dedup keyed on the applied RESULT, not the
    /// send, so a request dropped by the worker's coalescing is re-issued rather than starved). Keyed
    /// by doc index; on a tab close a stale entry harmlessly self-corrects (its `rev` won't match).
    #[cfg(feature = "lint")]
    lint_done: std::collections::HashMap<usize, (u64, usize, usize)>,
    #[cfg(feature = "lint")]
    lint_req_seq: u64,
    // GUI tab-title background per interaction state (`TABBGCOLOR active|hover|inactive`). `None` =
    // egui's theme colour (so tabs look unchanged until set). The GUI applies them to the tab strip.
    tab_active_bg: Option<(u8, u8, u8)>,
    tab_hover_bg: Option<(u8, u8, u8)>,
    tab_inactive_bg: Option<(u8, u8, u8)>,
    // Tab close-`×` glyph colour (`TABXCOLOR`). `None` = default to the UI foreground. `$TABXCOLOR`.
    tab_x_color: Option<(u8, u8, u8)>,
    // `$CWD` path-display colours (`PAFGCOLOR`/`PABGCOLOR`). `None` = default: the fg follows the
    // dimmed UI text, the bg is transparent. `$PAFGCOLOR`/`$PABGCOLOR`.
    path_fg_color: Option<(u8, u8, u8)>,
    path_bg_color: Option<(u8, u8, u8)>,
    // Preview-pane tint (`PVFGCOLOR`/`PVBGCOLOR`). `None` = Typst default (white paper / black body
    // text). PREVIEW ONLY — never applied to `TOPDF`/print output. `$PVFGCOLOR`/`$PVBGCOLOR`.
    preview_fg_color: Option<(u8, u8, u8)>,
    preview_bg_color: Option<(u8, u8, u8)>,
    // Scratchpad-window colours (`SCRATCHFGCOLOR`/`SCRATCHBGCOLOR`). `None` = egui's theme default
    // (light background, dark text). `$SCRATCHFGCOLOR`/`$SCRATCHBGCOLOR`.
    scratch_fg_color: Option<(u8, u8, u8)>,
    scratch_bg_color: Option<(u8, u8, u8)>,
    // `TXT` dialog text/background colours (`TXTFGCOLOR`/`TXTBGCOLOR`). `$TXTFGCOLOR`/`$TXTBGCOLOR`.
    txt_fg_color: (u8, u8, u8),
    txt_bg_color: (u8, u8, u8),
    // `DIR/W` listing window appearance (`DW…`). Every field is an override: `None` = inherit the
    // matching editor setting (so the window looks like the editor until customized). `dw_scale`
    // has no editor twin, so it's a concrete zoom factor (default 1.0). `DWRESET` clears them all.
    dw_fg_color: Option<(u8, u8, u8)>,
    dw_bg_color: Option<(u8, u8, u8)>,
    dw_font: Option<f32>,
    dw_scale: f32,
    dw_font_family: Option<String>,
    dw_font_style: Option<FontStyle>,
    /// Syntax-highlight theme: token kind → RGB. A kind ABSENT from the map renders in `fg_color`
    /// (the editor default). `HLCOLOR` edits it; `HLRESET` restores `default_highlight_theme()`.
    highlight_theme: std::collections::HashMap<crate::highlight::TokenKind, (u8, u8, u8)>,
    /// Syntax-highlight BACKGROUND theme: token kind → RGB, drawn as a band behind that kind's glyphs.
    /// Opt-in and empty by default (no kind has a background), unlike `highlight_theme`; a kind absent
    /// here has no background. `HLCOLOR/BG` edits it; `HLRESET` and `THEME` clear it.
    highlight_bg: std::collections::HashMap<crate::highlight::TokenKind, (u8, u8, u8)>,
    /// Syntax-highlight MODE theme: token kind → `ATTR_*` bitset (bold/italic/underline/reverse), the
    /// complement of the colour for a highlighting rule. Opt-in and empty by default; a kind absent here
    /// has no attributes. `HLMODE` edits it; `HLRESET` and `THEME` clear it.
    highlight_mode: std::collections::HashMap<crate::highlight::TokenKind, u8>,
    /// User-defined regex grammars (`HLLANG`/`HLRULE`), SHARED with every document (so a doc can
    /// resolve its own highlighter by extension on open/reload). `HIGHLIGHT on` resolves against
    /// this first, falling back to the built-in Markdown highlighter.
    highlight_grammars: std::rc::Rc<std::cell::RefCell<crate::highlight::GrammarRegistry>>,
    /// `HLAUTO on` — auto-enable highlighting whenever a file whose type a grammar/Markdown claims
    /// is opened (default off: the quiet-screen default is preserved). `$HLAUTO`.
    hl_auto: bool,
    /// State for the `RANDOM(min, max)` function's generator (SplitMix64). `Cell` for interior
    /// mutability, so `RANDOM` can advance it through the shared `&self` eval context; the `SRANDOM`
    /// verb reseeds it. Seeded from entropy at startup, so unseeded `RANDOM` varies per launch.
    rng: std::cell::Cell<u64>,
    /// User-selectable font for MARKDOWN/GFM rendering — the live Preview pane AND the `TOPDF`
    /// export (they share one pipeline). `None` = Typst's defaults. Set via `PVFAMILY`/`PVSIZE`/
    /// `PVCODE` (+ the `PVFONTS`/`PVCODES` pickers), read via `$PVFAMILY`/`$PVSIZE`/`$PVCODE`,
    /// persisted by SaveCfg. Authored-Typst docs style themselves, so this doesn't touch them.
    preview_font_family: Option<String>,
    preview_font_size: Option<f32>,
    preview_code_family: Option<String>,
    /// Code (monospace) size in points for Markdown Preview/`TOPDF` (`PVCODESIZE`). `None` = code
    /// inherits the body size (`preview_font_size`); `Some` pins it independently. `$PVCODESIZE`; SaveCfg.
    preview_code_size: Option<f32>,
    /// GFM Preview/`TOPDF` page margins (`PVMARGIN`/`PVMARGINL/R/T/B`) — each a validated Typst length
    /// string (default `"1in"`). `$PVMARGIN*`; SaveCfg. Authored-Typst docs set their own page.
    preview_margins: crate::pdf::PageMargins,
    /// Preview/`TOPDF` page format (`PVPAPER`) — a friendly token letter/legal/tabloid/a4/a5/a6.
    /// Default `"letter"`. `$PVPAPER`; SaveCfg.
    preview_paper: String,
    /// Preview/`TOPDF` line spacing (`PVLEADING`) and paragraph spacing (`PVSPACING`) — validated Typst
    /// lengths, or `None` for Typst's defaults (0.65em / 1.2em). `$PVLEADING`/`$PVSPACING`; SaveCfg.
    preview_leading: Option<String>,
    preview_spacing: Option<String>,
    /// Preview/`TOPDF` page numbering (`PVPAGENUM` — a numbering pattern like `1`, `Page 1`, `1 of 1`;
    /// `None` = off) and its position (`PVPAGEPOS` — a friendly token, default `"bottom-center"`).
    /// `$PVPAGENUM`/`$PVPAGEPOS`; SaveCfg.
    preview_page_num: Option<String>,
    preview_page_align: String,
    /// Preview/`TOPDF` body justification (`PVJUSTIFY on|off`, default on) and first-line paragraph
    /// indent (`PVINDENT` — a length, or `None` for none). `$PVJUSTIFY`/`$PVINDENT`; SaveCfg.
    preview_justify: bool,
    preview_indent: Option<String>,
    /// Preview/`TOPDF` page header / footer templates (`PVHEADER`/`PVFOOTER`; `None` = none). `|`
    /// splits left/center/right; tokens `{date}`/`{file}`/`{page}`/`{pages}`. `$PVHEADER`/`$PVFOOTER`.
    preview_header: Option<String>,
    preview_footer: Option<String>,
}

// Factory defaults for the GUI sizing knobs (B16 / `UIRESET`). `editor_font`/`leading` match
// the GUI's historical `FONT_SIZE`/`LINE_HEIGHT` (22pt / 32px ≈ 22 × 145%).
const DEF_UI_SCALE: f32 = 1.0;
const DEF_CHROME_FONT: f32 = 14.0;
const DEF_EDITOR_FONT: f32 = 22.0;
const DEF_LEADING: f32 = 145.0;
const DEF_MARGIN: f32 = 0.0;
const DEF_TXT_FONT: f32 = 14.0; // TXT dialog text size, pt (egui monospace default look)
const DEF_TXT_SCALE: f32 = 1.0; // TXT dialog zoom factor
const DEF_FG_COLOR: (u8, u8, u8) = (30, 30, 30); // matches the GUI's prior fixed text colour
const DEF_BG_COLOR: (u8, u8, u8) = (255, 255, 255); // white canvas
const DEF_UI_FG_COLOR: (u8, u8, u8) = (30, 30, 30); // chrome text
const DEF_UI_BG_COLOR: (u8, u8, u8) = (255, 255, 255); // chrome background
const DEF_CARET_COLOR: (u8, u8, u8) = (220, 40, 40); // the GUI's prior fixed caret red
const DEF_SEP_COLOR: (u8, u8, u8) = (190, 190, 190); // ≈ egui's prior default separator gray
const DEF_SEL_BG_COLOR: (u8, u8, u8) = (180, 210, 255); // the prior hardcoded selection band blue
const DEF_SEL_FG_COLOR: (u8, u8, u8) = (20, 20, 20); // the prior hardcoded selected-text colour
const DEF_FIND_BG_COLOR: (u8, u8, u8) = (250, 230, 90); // FIND highlight band — the prior hardcoded yellow
const DEF_FIND_FG_COLOR: (u8, u8, u8) = (20, 20, 20); // FIND-match text — a strong/intense fg on the yellow
const DEF_SPELL_COLOR: (u8, u8, u8) = (220, 50, 50); // spelling underline — classic red
const DEF_GRAMMAR_COLOR: (u8, u8, u8) = (60, 110, 220); // grammar/other underline — blue
const DEF_PANE_BORDER_COLOR: (u8, u8, u8) = (70, 130, 240); // active split-pane border — a clear focus blue
const DEF_TXT_FG_COLOR: (u8, u8, u8) = (30, 30, 30); // TXT dialog text (matches chrome default)
const DEF_TXT_BG_COLOR: (u8, u8, u8) = (255, 255, 255); // TXT dialog background
/// The built-in baseline keyboard `CLEARKBD` resets to (a usable editing floor; see that handler).
/// Baked in at build time but authored as an ordinary, editable `.kbd`.
const MINIMAL_KBD: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../xyscript-kbd/kbd/xyscript-minimal.kbd"));
const DEFAULT_CFG: &str = "xyscript.cfg"; // SAVECFG/LOADCFG default path (relative → cwd)
const DEFAULT_CM: &str = "xyscript-history.xpl"; // SaveCM default path (a runnable LoadCM script)
const DEFAULT_LDTX: &str = "xyscript-binders.xpl"; // SAVELDTX default path (runnable LDTX lines)
const DEFAULT_LDSC: &str = "xyscript-scripts.xpl"; // SAVELDSC/SAVELDPM default path (runnable LDSC lines)
const DEFAULT_CLOSED: &str = "xyscript-reopen.xpl"; // SaveClosed default path (a runnable LoadClosed script)
const REOPEN_CAP: usize = 50; // max entries kept in the reopen list
const REOPEN_NAME_COL: usize = 30; // max width of the filename column in the REOPEN chooser
const DEFAULT_KBD_DUMP: &str = "xyscript.kbd"; // SAVEKBD default path when none is given
/// Name of the keyboard-layout slot the editor boots into (`$KBDMODE` before any `KBDMODE`). The
/// base layout a plain `LOADKBD`/`CLEARKBD` targets.
pub(crate) const DEFAULT_KBD_MODE: &str = "default";
const SHELL_OUTPUT_NAME: &str = "*Console*"; // the unified Console REPL + X/BG output buffer
const SHELL_OUTPUT_CAP: usize = 512 * 1024; // scrollback cap (bytes) for *Shell Output*

impl Default for AppHost {
    fn default() -> Self {
        AppHost::new()
    }
}

impl AppHost {
    pub fn new() -> AppHost {
        let mut host = AppHost {
            docs: vec![Document::new()],
            active: 0,
            alt_active: None,
            panes: crate::panes::PaneTree::new(crate::panes::DocId(0)), // fixed up below to docs[0].id()
            cmline: Document::new(),
            blind: None,
            saved_counters: None,
            target: Target::Document,
            input_focus: Target::Document,
            doc_focus_ran: false,
            explicit_focus: false,
            clipboard: String::new(),
            clipboard_pieces: Vec::new(),
            clipboard_style: None,
            hex_clipboard: Vec::new(),
            deletion_buffer: String::new(),
            last_selection: String::new(),
            os: std::env::consts::OS.to_string(),
            last_scan: 0,
            last_mod_bits: 0,
            pending_kbd: None,
            cmline_submit: None,
            cmline_exec: None,
            quit_request: None,
            persistent: false,
            last_search: String::new(),
            last_spec: None,
            change_count: 0,
            verify: None,
            viewport: (0, 1),
            visual_line_span: None,
            visual_line_rev: 0,
            files: HashMap::new(),
            virtual_fs: false,
            cwd: std::env::current_dir().unwrap_or_default(),
            boot_cwd: std::env::current_dir().unwrap_or_default(),
            startup_path: None,
            drive_dirs: HashMap::new(), // seeded from the startup cwd just after construction
            new_window: false,
            dir_window_new_tab: true, // DIR/W has always opened a new tab — keep that the default
            dir_window_close_on_open: false, // DIR/W is a picker — it has always stayed open
            dir_reuse: false,
            dir_defaults: Vec::new(),
            session_restore: false,
            session_snapshot_revs: std::collections::HashMap::new(),
            last_session_manifest: None,
            show_titlebar: true,
            writer_view: false,
            remember_window_geometry: true,
            pending_delete: None,
            shell_trusted: false,
            last_rc: 0,
            last_kbd: None,
            last_kbd_error: None,
            kbd_modes: HashMap::new(),
            kbd_mode: DEFAULT_KBD_MODE.to_string(),
            last_status_line: String::new(),
            last_output: String::new(),
            pending_run: Vec::new(),
            text_binders: HashMap::new(),
            script_binders: HashMap::new(),
            stopwatch_start: std::time::Instant::now(),
            pending_message: None,
            save_as_requested: false,
            open_requested: None,
            find_requested: false,
            grep_dialog_requested: false,
            scratch_open_queue: Vec::new(),
            scratch_close_requested: None,
            scratch_text: String::new(),
            reveal_center: false,
            font_picker_request: None,
            show_manual_request: false,
            color_picker_request: None,
            pending_close: None,
            pending_open: None,
            autosave_secs: 0, // off by default
            tab_width: 4,
            tab_soft: false,
            tab_indent: false,
            bal_pairs: false,
            type_pairs: false,
            wrap_indent: false,
            bal_color: None,
            emph_fg: None,
            emph_bg: None,
            float_decimals: None, // default precision
            viewport_top: 0,
            cursor_vis: None,
            pending_scroll_rows: 0,
            pending_visual_edge: None,
            pending_visual_move: None,
            visual_rows: Vec::new(),
            visual_rows_rev: u64::MAX,
            current_keymap: None,
            keymap_gen: 0,
            config_track_path: None,
            last_loadcfg_path: None,
            lang: HashMap::new(),
            lang_path: None,
            lang_overlay_path: None,
            quiet: 0,
            config_last_written: String::new(),
            jobs: Vec::new(),
            next_job_id: 1,
            last_job_id: 0,
            grep_job: None,
            grep_before: 0,
            grep_after: 0,
            grep_blank: false,
            diff_source: None,
            pending_returns: Vec::new(),
            command_history: Vec::new(),
            recently_closed: Vec::new(),
            history_pos: None,
            history_draft: String::new(),
            history_request: None,
            menu_kbd_request: None,
            menu_bar: true, // shown by default
            visible_returns: false,
            visible_whitespace: false,
            line_numbers: false,
            gui_cmline: false,
            shell_output_visible: true, // the *Shell Output* tab shows by default
            shell_focus_restore: false,
            dir_window: None,
            preview_visible: false,
            console_prompt: "$CWD> ".to_string(),
            console_showing: false,
            console_prompt_at: 0,
            console_prompt_len: 0,
            console_dirty: false,
            console_reveal_tail: false,
            console_stdin: None,
            console_input_at: 0,
            console_history: Vec::new(),
            console_hist_pos: None,
            last_calc: String::new(),
            sum_total: Number::Int(0),
            console_ansi: crate::ansi::AnsiState::new(),
            ansi_color: true,
            auto_replace: std::collections::HashMap::new(),
            last_ar_path: None,
            ar_pending_xpl: None,
            ui_scale: DEF_UI_SCALE,
            chrome_font: DEF_CHROME_FONT,
            editor_font: DEF_EDITOR_FONT,
            leading: DEF_LEADING,
            margin_left: DEF_MARGIN,
            margin_right: DEF_MARGIN,
            txt_font: DEF_TXT_FONT,
            txt_scale: DEF_TXT_SCALE,
            editor_font_family: None,
            editor_prop_family: None,
            ui_font_family: None,
            txt_font_family: None,
            editor_font_style: FontStyle::Normal,
            ui_font_style: FontStyle::Normal,
            txt_font_style: FontStyle::Normal,
            fg_color: DEF_FG_COLOR,
            bg_color: DEF_BG_COLOR,
            ui_fg_color: DEF_UI_FG_COLOR,
            ui_bg_color: DEF_UI_BG_COLOR,
            caret_color: DEF_CARET_COLOR,
            cmline_caret_color: DEF_CARET_COLOR,
            sep_color: DEF_SEP_COLOR,
            sel_bg_color: DEF_SEL_BG_COLOR,
            sel_fg_color: DEF_SEL_FG_COLOR,
            find_bg_color: DEF_FIND_BG_COLOR,
            find_fg_color: DEF_FIND_FG_COLOR,
            spell_color: DEF_SPELL_COLOR,
            grammar_color: DEF_GRAMMAR_COLOR,
            spell_style: UnderlineStyle::Dotted,
            grammar_style: UnderlineStyle::Dotted,
            pane_border_color: DEF_PANE_BORDER_COLOR,
            spell_on: false,
            grammar_on: false,
            lint_dialect: crate::lint::Dialect::default(),
            spell_engine: crate::lint::SpellEngine::default(),
            spell_dicts: "en_US".to_string(),
            spell_bin: "hunspell".to_string(),
            learned_words: std::collections::BTreeSet::new(),
            lint_key: None,
            #[cfg(feature = "lint")]
            lint_worker: None,
            #[cfg(feature = "lint")]
            lint_pending: None,
            #[cfg(feature = "lint")]
            check_pending: None,
            #[cfg(feature = "lint")]
            lint_done: std::collections::HashMap::new(),
            #[cfg(feature = "lint")]
            lint_req_seq: 0,
            tab_active_bg: None,
            tab_x_color: None,
            path_fg_color: None,
            path_bg_color: None,
            preview_fg_color: None,
            preview_bg_color: None,
            scratch_fg_color: None,
            scratch_bg_color: None,
            tab_hover_bg: None,
            tab_inactive_bg: None,
            txt_fg_color: DEF_TXT_FG_COLOR,
            txt_bg_color: DEF_TXT_BG_COLOR,
            dw_fg_color: None,
            dw_bg_color: None,
            dw_font: None,
            dw_scale: 1.0,
            dw_font_family: None,
            dw_font_style: None,
            highlight_theme: crate::highlight::default_highlight_theme(),
            highlight_bg: std::collections::HashMap::new(),
            highlight_mode: std::collections::HashMap::new(),
            highlight_grammars: std::rc::Rc::new(std::cell::RefCell::new(
                crate::highlight::GrammarRegistry::default(),
            )),
            hl_auto: false,
            rng: std::cell::Cell::new({
                // Entropy-based startup seed (SystemTime nanos), so an unseeded `RANDOM` still
                // differs per launch; `SRANDOM` overrides this for a reproducible sequence.
                use std::time::{SystemTime, UNIX_EPOCH};
                let nanos = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0x853c_49e6_748f_ea9b);
                nanos ^ 0x9E37_79B9_7F4A_7C15
            }),
            preview_margins: crate::pdf::PageMargins::default(),
            preview_paper: "letter".to_string(),
            preview_leading: None,
            preview_spacing: None,
            preview_page_num: None,
            preview_page_align: "bottom-center".to_string(),
            preview_justify: true,
            preview_indent: None,
            preview_header: None,
            preview_footer: None,
            preview_font_family: None,
            preview_font_size: None,
            preview_code_family: None,
            preview_code_size: None,
            caret_shape: CaretShape::Bar,
            cmline_caret_shape: CaretShape::Bar,
            cmline_placement: CmlinePlacement::Top,
            status_line_top: false, // default: status at the bottom edge
            path_place: PathPlace::Status,
            path_len: 45,
            text_file_exts: Vec::new(),
            cmline_blank: false,
            caret_visual: false,
            show_one_tab: false,
            wrap: true,
            wrap_hang: 0,
            reflow_width: 65,
            untitled_auto_close: true,
            goto_open: false, // default: keep the "already open" prompt
            mark_gutter: MarkGutterMode::On, // default: strip appears when the doc has bookmarks
            mark_fg_color: None,
            mark_bg_color: None,
            line_num_fg_color: None,
            line_num_bg_color: None,
            msg_header_fg_color: None,
            msg_bg_color: None,
            msg_ok_fg_color: None,
            msg_ok_bg_color: None,
            url_highlight: false, // off by default
            url_fg_color: None,
            url_bg_color: None,
            preview_marks: false, // off by default: printouts don't carry transient EMPH/URL marks
            auto_reopen: false, // off by default
            error_log: false, // off by default
            error_log_file: None, // default: xyscript-errors.log in $XSDIR
            disk_mtimes: HashMap::new(),
            dim_on_blur: false, // off by default
            close_back: false, // off by default: AB backs up to the left neighbor
            focus_prev: None,
            ul_spaces: true, // underline through spaces by default (matches selection styling)
            #[cfg(feature = "email")]
            smtp_url: String::new(),
            #[cfg(feature = "email")]
            mail_user: String::new(),
            #[cfg(feature = "email")]
            mail_from: String::new(),
            #[cfg(feature = "email")]
            mail_pass: String::new(),
            #[cfg(feature = "email")]
            mail_debug: false,
            #[cfg(feature = "email")]
            mail_attachments: Vec::new(),
            #[cfg(feature = "email")]
            imap_url: String::new(),
            #[cfg(feature = "email")]
            mailbox: "INBOX".to_string(),
            #[cfg(feature = "email")]
            mail_save_all: Vec::new(),
        };
        // Share the grammar registry into the initial scratch buffer (every later document gets it
        // via `add_or_reuse`), so highlighting resolves grammars uniformly.
        let reg = host.highlight_grammars.clone();
        host.docs[0].share_grammars(reg);
        host.panes = crate::panes::PaneTree::new(host.docs[0].id()); // real id now that docs[0] exists
        host.remember_current_drive(); // seed the startup drive's remembered directory
        host
    }

    /// GUI sizing knobs (read each frame by the GUI). See `EditorOp::SetUiSize`.
    pub fn ui_scale(&self) -> f32 {
        self.ui_scale
    }
    pub fn chrome_font(&self) -> f32 {
        self.chrome_font
    }
    pub fn editor_font(&self) -> f32 {
        self.editor_font
    }
    /// Editor canvas font family (`EDFAMILY`), or `None` for the GUI's resolved default.
    pub fn editor_font_family(&self) -> Option<&str> {
        self.editor_font_family.as_deref()
    }
    /// Editor PROPORTIONAL canvas font family (`EDPROPFAMILY`), or `None` when unset (prose stays
    /// monospace). The GUI shapes prose buffers with it when their font mode resolves to proportional.
    pub fn editor_prop_family(&self) -> Option<&str> {
        self.editor_prop_family.as_deref()
    }
    /// Body font family for Markdown Preview/`TOPDF` (`PVFAMILY`), or `None` for Typst's default.
    pub fn preview_font_family(&self) -> Option<&str> {
        self.preview_font_family.as_deref()
    }
    /// Body font size (pt) for Markdown Preview/`TOPDF` (`PVSIZE`), or `None` for Typst's default.
    pub fn preview_font_size(&self) -> Option<f32> {
        self.preview_font_size
    }
    /// Code (monospace) family for Markdown Preview/`TOPDF` (`PVCODE`), or `None` for the default.
    pub fn preview_code_family(&self) -> Option<&str> {
        self.preview_code_family.as_deref()
    }
    /// Code (monospace) size (pt) for Markdown Preview/`TOPDF` (`PVCODESIZE`), or `None` to inherit
    /// the body size.
    pub fn preview_code_size(&self) -> Option<f32> {
        self.preview_code_size
    }
    /// The bundled font choice for GFM rendering (Preview pane + `TOPDF`).
    pub fn preview_fonts(&self) -> crate::pdf::PreviewFonts {
        crate::pdf::PreviewFonts {
            family: self.preview_font_family.clone(),
            size: self.preview_font_size,
            code: self.preview_code_family.clone(),
            code_size: self.preview_code_size,
            margins: self.preview_margins.clone(),
            paper: self.preview_paper.clone(),
            leading: self.preview_leading.clone(),
            spacing: self.preview_spacing.clone(),
            page_num: self.preview_page_num.clone(),
            page_align: self.preview_page_align.clone(),
            justify: self.preview_justify,
            indent: self.preview_indent.clone(),
            header: self.preview_header.clone(),
            footer: self.preview_footer.clone(),
            doc_name: self.active_file_name(),
        }
    }
    /// UI-chrome font family (`UIFAMILY`), or `None` for egui's built-in default.
    pub fn ui_font_family(&self) -> Option<&str> {
        self.ui_font_family.as_deref()
    }
    /// `TXT` window font family (`TXTFAMILY`), or `None` for egui's built-in default monospace.
    pub fn txt_font_family(&self) -> Option<&str> {
        self.txt_font_family.as_deref()
    }
    /// Editor canvas / UI chrome / `TXT` window font styles (`EDSTYLE`/`UISTYLE`/`TXTSTYLE`).
    pub fn editor_font_style(&self) -> FontStyle {
        self.editor_font_style
    }
    pub fn ui_font_style(&self) -> FontStyle {
        self.ui_font_style
    }
    pub fn txt_font_style(&self) -> FontStyle {
        self.txt_font_style
    }
    pub fn leading(&self) -> f32 {
        self.leading
    }
    pub fn margin_left(&self) -> f32 {
        self.margin_left
    }
    pub fn margin_right(&self) -> f32 {
        self.margin_right
    }
    /// `TXT` dialog text size (points) and zoom factor (read each frame by the GUI's TXT window).
    pub fn txt_font(&self) -> f32 {
        self.txt_font
    }
    pub fn txt_scale(&self) -> f32 {
        self.txt_scale
    }
    /// Editor colours as RGB triples (read each frame by the GUI). See `EditorOp::SetColor`.
    pub fn fg_color(&self) -> (u8, u8, u8) {
        self.fg_color
    }
    /// The active highlight colour for a token kind, or `None` when the kind isn't themed (the
    /// renderer then draws it in `fg_color`). Driven by `HLCOLOR`/`HLRESET`.
    pub fn highlight_color(&self, kind: &crate::highlight::TokenKind) -> Option<(u8, u8, u8)> {
        self.highlight_theme.get(kind).copied()
    }
    /// The active BACKGROUND colour for a token kind, or `None` when the kind has none (the common
    /// case — backgrounds are opt-in). Driven by `HLCOLOR/BG`; the renderer draws it as a band behind
    /// the kind's glyphs.
    pub fn highlight_bg_color(&self, kind: &crate::highlight::TokenKind) -> Option<(u8, u8, u8)> {
        self.highlight_bg.get(kind).copied()
    }
    /// Background-highlight spans for the ACTIVE document as `(rope_start, rope_end, rgb)` — one per
    /// visible-or-not token whose kind carries an `HLCOLOR/BG`, for the GUI to paint behind the glyphs
    /// via `highlight_rects` (which expects rope offsets, like the `MDA/B` bg layer). Empty (and cheap)
    /// when no kind has a background. Token offsets are DECODED, converted to rope here.
    pub fn highlight_bg_spans_raw(&self) -> Vec<(usize, usize, (u8, u8, u8))> {
        if self.highlight_bg.is_empty() {
            return Vec::new();
        }
        let doc = self.active_doc();
        let buf = doc.buffer();
        doc.highlights()
            .iter()
            .filter_map(|t| {
                self.highlight_bg
                    .get(&t.kind)
                    .map(|&rgb| (buf.rope_offset(t.start), buf.rope_offset(t.end), rgb))
            })
            .collect()
    }
    /// Like [`highlight_bg_spans_raw`](Self::highlight_bg_spans_raw) but in DECODED offsets — the
    /// tokens' native coordinate, so NO per-token `rope_offset` conversion (which on a big non-UTF-8
    /// doc is a bounded-but-costly walk — thousands of them per frame on a dense grammar froze the UI).
    /// The GUI renders bands in decoded-window-local space, so this feeds it directly.
    pub fn highlight_bg_spans_decoded(&self) -> Vec<(usize, usize, (u8, u8, u8))> {
        if self.highlight_bg.is_empty() {
            return Vec::new();
        }
        self.active_doc()
            .highlights()
            .iter()
            .filter_map(|t| self.highlight_bg.get(&t.kind).map(|&rgb| (t.start, t.end, rgb)))
            .collect()
    }

    /// The active MODE attributes (`HLMODE`, an `ATTR_*` bitset) for a token kind, or `None` when the
    /// kind has none (the common case — modes are opt-in). The renderer folds the bits into the glyphs
    /// (bold/italic) and underline/reverse rects, exactly as `MDA` block attributes do.
    pub fn highlight_mode(&self, kind: &crate::highlight::TokenKind) -> Option<u8> {
        self.highlight_mode.get(kind).copied()
    }
    /// Mode-attribute spans for the ACTIVE document as `(rope_start, rope_end, bits)` — one per token
    /// whose kind carries an `HLMODE`, for the GUI to feed the same underline/reverse rect machinery as
    /// `MDA`. Empty (and cheap) when no kind has a mode. Token offsets are DECODED, converted to rope.
    pub fn highlight_mode_spans_raw(&self) -> Vec<(usize, usize, u8)> {
        if self.highlight_mode.is_empty() {
            return Vec::new();
        }
        let doc = self.active_doc();
        let buf = doc.buffer();
        doc.highlights()
            .iter()
            .filter_map(|t| {
                self.highlight_mode
                    .get(&t.kind)
                    .map(|&bits| (buf.rope_offset(t.start), buf.rope_offset(t.end), bits))
            })
            .collect()
    }
    /// Like [`highlight_mode_spans_raw`](Self::highlight_mode_spans_raw) but in DECODED offsets (no
    /// per-token `rope_offset` walk) — see [`highlight_bg_spans_decoded`](Self::highlight_bg_spans_decoded).
    pub fn highlight_mode_spans_decoded(&self) -> Vec<(usize, usize, u8)> {
        if self.highlight_mode.is_empty() {
            return Vec::new();
        }
        self.active_doc()
            .highlights()
            .iter()
            .filter_map(|t| self.highlight_mode.get(&t.kind).map(|&bits| (t.start, t.end, bits)))
            .collect()
    }
    /// The token kinds shown by `HLKINDS`, in display order: the built-ins (chart order) followed by
    /// any grammar-defined / themed extras (sorted). Shared by `highlight_kinds` (labels) and
    /// `highlight_kind_colors` (colours) so the two stay perfectly aligned.
    fn ordered_highlight_kinds(&self) -> Vec<crate::highlight::TokenKind> {
        use crate::highlight::TokenKind;
        let mut kinds: Vec<TokenKind> = TokenKind::ALL.to_vec();
        let mut extra: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for k in self.highlight_theme.keys() {
            if !TokenKind::ALL.contains(k) {
                extra.insert(k.name().to_string());
            }
        }
        for g in self.highlight_grammars.borrow().grammars() {
            for r in &g.rules {
                let k = TokenKind::from_name(&r.kind);
                if !TokenKind::ALL.contains(&k) {
                    extra.insert(k.name().to_string());
                }
            }
        }
        kinds.extend(extra.iter().map(|n| TokenKind::from_name(n)));
        kinds
    }
    /// Apply a built-in colour scheme (`THEME light|dark`): a coordinated one-shot over the canvas,
    /// UI chrome, separators, caret, selection, `TXT` dialog, and the highlight palette. The
    /// individual colours are what persists (SaveCfg emits them), so a later tweak still survives.
    /// Returns the canonical name applied; an unknown name is an error.
    fn apply_theme(&mut self, name: &str) -> Result<&'static str, String> {
        match name.trim().to_ascii_lowercase().as_str() {
            "light" => {
                // The factory defaults (the first-run look).
                self.fg_color = DEF_FG_COLOR;
                self.bg_color = DEF_BG_COLOR;
                self.ui_fg_color = DEF_UI_FG_COLOR;
                self.ui_bg_color = DEF_UI_BG_COLOR;
                self.sep_color = DEF_SEP_COLOR;
                self.caret_color = DEF_CARET_COLOR;
                self.cmline_caret_color = DEF_CARET_COLOR;
                self.sel_bg_color = DEF_SEL_BG_COLOR;
                self.sel_fg_color = DEF_SEL_FG_COLOR;
                self.find_bg_color = DEF_FIND_BG_COLOR;
                self.find_fg_color = DEF_FIND_FG_COLOR;
                self.spell_color = DEF_SPELL_COLOR;
                self.grammar_color = DEF_GRAMMAR_COLOR;
                self.pane_border_color = DEF_PANE_BORDER_COLOR;
                self.txt_fg_color = DEF_TXT_FG_COLOR;
                self.txt_bg_color = DEF_TXT_BG_COLOR;
                self.highlight_theme = crate::highlight::default_highlight_theme();
                self.highlight_bg.clear(); // backgrounds are opt-in; a theme defines the fg palette only
                self.highlight_mode.clear(); // modes are opt-in too
                Ok("light")
            }
            "dark" => {
                self.fg_color = (220, 220, 220);
                self.bg_color = (30, 30, 34);
                self.ui_fg_color = (210, 210, 210);
                self.ui_bg_color = (24, 24, 28);
                self.sep_color = (70, 70, 78);
                self.caret_color = (240, 220, 80);
                self.cmline_caret_color = (240, 220, 80);
                self.sel_bg_color = (60, 80, 120);
                self.sel_fg_color = (240, 240, 240);
                self.find_bg_color = (205, 172, 66); // a softer gold on a dark canvas
                self.find_fg_color = (20, 20, 20);
                self.spell_color = (240, 100, 100); // brighter red/blue so underlines read on a dark canvas
                self.grammar_color = (110, 160, 250);
                self.pane_border_color = (110, 160, 250); // a bright focus blue that reads on dark
                self.txt_fg_color = (220, 220, 220);
                self.txt_bg_color = (30, 30, 34);
                self.highlight_theme = crate::highlight::dark_highlight_theme();
                self.highlight_bg.clear(); // backgrounds are opt-in; a theme defines the fg palette only
                self.highlight_mode.clear(); // modes are opt-in too
                Ok("dark")
            }
            _ => Err(format!("THEME: unknown theme {:?} (try light|dark)", name.trim())),
        }
    }
    pub fn bg_color(&self) -> (u8, u8, u8) {
        self.bg_color
    }
    /// UI-chrome colours as RGB triples (read each frame by the GUI). See `EditorOp::SetColor`.
    pub fn ui_fg_color(&self) -> (u8, u8, u8) {
        self.ui_fg_color
    }
    pub fn ui_bg_color(&self) -> (u8, u8, u8) {
        self.ui_bg_color
    }
    /// Caret appearance (read each frame by the GUI). See `EditorOp::SetColor`/`SetCaretShape`.
    pub fn caret_color(&self) -> (u8, u8, u8) {
        self.caret_color
    }
    /// Command-line caret colour (`CMCRCOLOR`/`$CMCRCOLOR`); the GUI paints the cmline caret with it.
    pub fn cmline_caret_color(&self) -> (u8, u8, u8) {
        self.cmline_caret_color
    }
    /// UI separator colour as an RGB triple (read each frame by the GUI). See `EditorOp::SetColor`.
    pub fn sep_color(&self) -> (u8, u8, u8) {
        self.sep_color
    }
    /// Selection background (`SELBGCOLOR`) — the band painted behind selected text on the canvas.
    pub fn sel_bg_color(&self) -> (u8, u8, u8) {
        self.sel_bg_color
    }
    /// Selected-text foreground (`SELFGCOLOR`) — the glyph colour of text inside the selection.
    pub fn sel_fg_color(&self) -> (u8, u8, u8) {
        self.sel_fg_color
    }
    /// FIND-highlight background (`FINDBGCOLOR`) — the band behind non-current matches.
    pub fn find_bg_color(&self) -> (u8, u8, u8) {
        self.find_bg_color
    }
    /// FIND-match foreground (`FINDFGCOLOR`) — the glyph colour of highlighted match text.
    pub fn find_fg_color(&self) -> (u8, u8, u8) {
        self.find_fg_color
    }
    /// Spelling-issue underline colour (`SPELLCOLOR`).
    pub fn spell_color(&self) -> (u8, u8, u8) {
        self.spell_color
    }
    /// Grammar/other-issue underline colour (`GRAMMARCOLOR`).
    pub fn grammar_color(&self) -> (u8, u8, u8) {
        self.grammar_color
    }
    /// Spelling / grammar underline STYLE (`SPELLSTYLE`/`GRAMMARSTYLE`) — dotted/solid/squiggle.
    pub fn spell_style(&self) -> UnderlineStyle {
        self.spell_style
    }
    pub fn grammar_style(&self) -> UnderlineStyle {
        self.grammar_style
    }
    /// Active split-pane border colour (`PANECOLOR`). Inactive panes use [`sep_color`](Self::sep_color).
    pub fn pane_border_color(&self) -> (u8, u8, u8) {
        self.pane_border_color
    }
    /// Tab-title background colours per state (`TABBGCOLOR`), `None` = egui's theme colour. The GUI
    /// applies these to the tab strip; the active and hover states are the visible ones.
    pub fn tab_active_bg(&self) -> Option<(u8, u8, u8)> {
        self.tab_active_bg
    }
    pub fn tab_hover_bg(&self) -> Option<(u8, u8, u8)> {
        self.tab_hover_bg
    }
    pub fn tab_inactive_bg(&self) -> Option<(u8, u8, u8)> {
        self.tab_inactive_bg
    }
    /// The tab close-× glyph colour override (`None` = default to the UI foreground).
    pub fn tab_x_color(&self) -> Option<(u8, u8, u8)> {
        self.tab_x_color
    }
    /// `$CWD` path-display colours (`PAFGCOLOR`/`PABGCOLOR`). `None` = default (fg = dimmed UI text,
    /// bg = transparent). The GUI applies them to the path prefix on the cmline / status line.
    pub fn path_fg_color(&self) -> Option<(u8, u8, u8)> {
        self.path_fg_color
    }
    pub fn path_bg_color(&self) -> Option<(u8, u8, u8)> {
        self.path_bg_color
    }
    /// Preview-pane tint colours (`PVFGCOLOR`/`PVBGCOLOR`). `None` = Typst default (white paper / black
    /// body text). PREVIEW ONLY. Read individually, or as a [`crate::pdf::PreviewTint`] via
    /// [`Self::preview_tint`] for the render call.
    pub fn preview_fg_color(&self) -> Option<(u8, u8, u8)> {
        self.preview_fg_color
    }
    pub fn preview_bg_color(&self) -> Option<(u8, u8, u8)> {
        self.preview_bg_color
    }
    /// The preview-pane tint bundled for the raster render path (`*_to_image`). Never used for `TOPDF`.
    pub fn preview_tint(&self) -> crate::pdf::PreviewTint {
        crate::pdf::PreviewTint { bg: self.preview_bg_color, fg: self.preview_fg_color }
    }
    /// Scratchpad-window colours (`SCRATCHFGCOLOR`/`SCRATCHBGCOLOR`). `None` = egui's theme default.
    /// The GUI applies them to each scratchpad window's text / background.
    pub fn scratch_fg_color(&self) -> Option<(u8, u8, u8)> {
        self.scratch_fg_color
    }
    pub fn scratch_bg_color(&self) -> Option<(u8, u8, u8)> {
        self.scratch_bg_color
    }
    /// `TXT` dialog text/background colours (read by the GUI's TXT window). See `EditorOp::SetColor`.
    pub fn txt_fg_color(&self) -> (u8, u8, u8) {
        self.txt_fg_color
    }
    pub fn txt_bg_color(&self) -> (u8, u8, u8) {
        self.txt_bg_color
    }

    // `DIR/W` window appearance — EFFECTIVE values (override else inherit the editor), read by the
    // GUI's DIR window each frame and by the `$DW…` readbacks.
    pub fn dw_fg_color(&self) -> (u8, u8, u8) {
        self.dw_fg_color.unwrap_or(self.fg_color)
    }
    pub fn dw_bg_color(&self) -> (u8, u8, u8) {
        self.dw_bg_color.unwrap_or(self.bg_color)
    }
    pub fn dw_font(&self) -> f32 {
        self.dw_font.unwrap_or(self.editor_font)
    }
    pub fn dw_scale(&self) -> f32 {
        self.dw_scale
    }
    /// The `DIR/W` font family OVERRIDE only (`DWFAMILY`); `None` = inherit `EDFAMILY`. The GUI
    /// combines this with its resolved editor family so an unset `DWFAMILY` uses `EDFAMILY`.
    pub fn dw_font_family(&self) -> Option<&str> {
        self.dw_font_family.as_deref()
    }
    pub fn dw_font_style(&self) -> FontStyle {
        self.dw_font_style.unwrap_or(self.editor_font_style)
    }
    pub fn caret_shape(&self) -> CaretShape {
        self.caret_shape
    }
    /// Command-line caret shape (`CMCRSHAPE`/`$CMCRSHAPE`); the GUI paints the cmline caret with it.
    pub fn cmline_caret_shape(&self) -> CaretShape {
        self.cmline_caret_shape
    }
    /// Where the command line is drawn (`CMPLACE`/`$CMPLACE`); the GUI renders from this each frame.
    pub fn cmline_placement(&self) -> CmlinePlacement {
        self.cmline_placement
    }
    /// Whether the status line is drawn at the top under the command line (`STATUSLINE`/`$STATUSLINE`);
    /// the GUI renders from this each frame.
    pub fn status_line_top(&self) -> bool {
        self.status_line_top
    }
    /// Where the `$CWD` path is shown (`PAPLACE`/`$PAPLACE`); the GUI renders from this each frame.
    pub fn path_place(&self) -> PathPlace {
        self.path_place
    }
    /// Max width (chars) of the displayed `$CWD` path (`PATHLEN`/`$PATHLEN`).
    pub fn path_len(&self) -> usize {
        self.path_len
    }
    /// The user's extra text-file extensions (`TEXTFILES`), lowercased and dot-stripped — the GUI
    /// appends these to the built-in list its File ▸ Open "Text files" filter matches.
    pub fn text_file_exts(&self) -> &[String] {
        &self.text_file_exts
    }
    /// Whether the command line is blanked after a command runs from it (`CMBLANK`/`$CMBLANK`).
    pub fn cmline_blank(&self) -> bool {
        self.cmline_blank
    }
    /// Whether the tab strip shows when only one file is open (`SHOWONETAB`/`$SHOWONETAB`).
    pub fn show_one_tab(&self) -> bool {
        self.show_one_tab
    }
    /// Whether long lines wrap to the window (`WRAP`/`$WRAP`). Off → lines run past the edge and the
    /// GUI shows a horizontal scrollbar.
    pub fn wrap(&self) -> bool {
        self.wrap
    }
    /// Hanging indent (spaces) for wrapped continuation rows (`WRAPHANG`/`$WRAPHANG`). Effective only
    /// when `WRAP` is on and `WRAPINDENT` is off.
    pub fn wrap_hang(&self) -> usize {
        self.wrap_hang
    }

    /// Whether the blank untitled tab is auto-closed on open (`UNTITLEDTAB`/`$UNTITLEDTAB`).
    pub fn untitled_tab(&self) -> bool {
        self.untitled_auto_close
    }
    /// Whether opening an already-open file skips the prompt and switches to its tab
    /// (`GOTOOPEN`/`$GOTOOPEN`).
    pub fn goto_open(&self) -> bool {
        self.goto_open
    }
    /// Whether the bookmark gutter is ENABLED — the strip is shown at all (`On` or `Persist`). Dots are
    /// drawn on bookmarked lines when enabled. (`MARKGUTTER`/`$MARKGUTTER`.)
    pub fn mark_gutter(&self) -> bool {
        !matches!(self.mark_gutter, MarkGutterMode::Off)
    }
    /// Whether the bookmark-gutter strip is reserved even when the doc has NO bookmarks (`MARKGUTTER
    /// persist`) — a stable left margin.
    pub fn mark_gutter_persist(&self) -> bool {
        matches!(self.mark_gutter, MarkGutterMode::Persist)
    }
    /// The bookmark-gutter dot / strip-background colours (`MARKFGCOLOR`/`MARKBGCOLOR`). `None` =
    /// default (dot = bookmark orange, strip = transparent).
    pub fn mark_fg_color(&self) -> Option<(u8, u8, u8)> {
        self.mark_fg_color
    }
    pub fn mark_bg_color(&self) -> Option<(u8, u8, u8)> {
        self.mark_bg_color
    }
    /// The line-number gutter text / background colours (`LNFGCOLOR`/`LNBGCOLOR`). `None` = default
    /// (a muted slate for the numbers; transparent background).
    pub fn line_num_fg_color(&self) -> Option<(u8, u8, u8)> {
        self.line_num_fg_color
    }
    pub fn line_num_bg_color(&self) -> Option<(u8, u8, u8)> {
        self.line_num_bg_color
    }
    /// Modal `MSG` box colours (`None` = follow the theme).
    pub fn msg_header_fg_color(&self) -> Option<(u8, u8, u8)> {
        self.msg_header_fg_color
    }
    pub fn msg_bg_color(&self) -> Option<(u8, u8, u8)> {
        self.msg_bg_color
    }
    pub fn msg_ok_fg_color(&self) -> Option<(u8, u8, u8)> {
        self.msg_ok_fg_color
    }
    pub fn msg_ok_bg_color(&self) -> Option<(u8, u8, u8)> {
        self.msg_ok_bg_color
    }
    /// Whether URL highlighting + Ctrl+click-to-open is on (`URLHIGHLIGHT`/`$URLHIGHLIGHT`).
    pub fn url_highlight(&self) -> bool {
        self.url_highlight
    }

    /// Whether preview/`TOPDF` renders the EMPH + URL marks (`PVMARKS`/`$PVMARKS`).
    pub fn preview_marks(&self) -> bool {
        self.preview_marks
    }
    /// The URL-highlight text colour, EFFECTIVE (the override, or the default link blue).
    pub fn url_fg_color(&self) -> (u8, u8, u8) {
        self.url_fg_color.unwrap_or(DEFAULT_URL_FG)
    }
    /// The URL-highlight text-colour override as set (`None` = the default link blue). For the picker
    /// seed, config emit, and `$URLFGCOLOR`.
    pub fn url_fg_override(&self) -> Option<(u8, u8, u8)> {
        self.url_fg_color
    }
    /// The URL-highlight background colour (`None` = transparent — no band is drawn).
    pub fn url_bg_color(&self) -> Option<(u8, u8, u8)> {
        self.url_bg_color
    }
    /// The URL spans (rope offsets) in the active document, for highlighting and Ctrl+click hit-testing.
    /// Empty when `URLHIGHLIGHT` is off. Trailing sentence punctuation is trimmed from each match.
    pub fn url_highlights(&self) -> Vec<(usize, usize)> {
        if !self.url_highlight {
            return Vec::new();
        }
        let buffer = self.active_doc().buffer();
        self.find_all(URL_PATTERN, false, false, true, false)
            .into_iter()
            .filter_map(|(s, e)| {
                let raw = buffer.slice_bytes(s, e);
                let trimmed = trim_url_len(&raw);
                (trimmed > 0).then_some((s, s + trimmed))
            })
            .collect()
    }

    /// URL spans within the rope range `[rope_lo, rope_hi)` only — the windowed form the GUI calls
    /// each frame so URL highlighting never scans the whole buffer (a 263 MB file took ~25 s to scan
    /// whole on the first paint). Rope offsets, trimmed like [`url_highlights`](Host::url_highlights).
    pub fn url_highlights_in(&self, rope_lo: usize, rope_hi: usize) -> Vec<(usize, usize)> {
        if !self.url_highlight {
            return Vec::new();
        }
        let pattern = match Pattern::build(URL_PATTERN, false, false, true, false) {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };
        let buffer = self.active_doc().buffer();
        const CAP: usize = 5000;
        buffer
            .find_all_in_range(&pattern, rope_lo, rope_hi, CAP)
            .into_iter()
            .filter_map(|(s, e)| {
                let raw = buffer.slice_bytes(s, e);
                let trimmed = trim_url_len(&raw);
                (trimmed > 0).then_some((s, s + trimmed))
            })
            .collect()
    }
    /// Whether files auto-reopen when changed on disk (`AUTOREOPEN`/`$AUTOREOPEN`).
    pub fn auto_reopen(&self) -> bool {
        self.auto_reopen
    }

    /// Whether error logging is on (`ERRORLOG`/`$ERRORLOG`).
    pub fn error_log(&self) -> bool {
        self.error_log
    }

    /// The effective error-log file path (`$ERRORLOGFILE`): the configured path resolved against
    /// `$CWD`, or the default `xyscript-errors.log` in `$XSDIR` when none is set. Falls back to a
    /// bare relative name if the executable's directory can't be determined.
    pub fn error_log_path(&self) -> std::path::PathBuf {
        match &self.error_log_file {
            Some(f) if !f.trim().is_empty() => self.resolve_path(f),
            _ => std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("xyscript-errors.log")))
                .unwrap_or_else(|| std::path::PathBuf::from("xyscript-errors.log")),
        }
    }

    /// Whether the window dims when it loses focus (`DIMONBLUR`/`$DIMONBLUR`).
    pub fn dim_on_blur(&self) -> bool {
        self.dim_on_blur
    }
    /// Whether closing a tab returns to the last-visited tab (`CLOSEBACK`/`$CLOSEBACK`).
    pub fn close_back(&self) -> bool {
        self.close_back
    }

    /// Launch a second editor window (a fresh instance) on `path`, positioned at 1-based `line`
    /// (`DUPVIEW/W`). Runs OUR OWN executable — so it is NOT shell-out and needs no `TRUST`; the new
    /// instance runs its normal `startup.xpl` (persistent config), opens the file positionally, and
    /// jumps to the line via `--goto`. Detached; the editor never waits on it.
    fn spawn_second_window(&mut self, path: &Path, line: usize) -> VmResult<Value> {
        if self.virtual_fs {
            return Ok(Value::str("(virtual-fs: window not launched)"));
        }
        let exe = std::env::current_exe().map_err(|e| {
            XplError::trappable(format!("DUPVIEW: cannot find the editor executable: {e}"))
        })?;
        // Absolute file path, so opening it doesn't depend on the child's working directory.
        let abs = self.cwd.join(path); // `join` returns `path` unchanged when it's already absolute
        let (args, child_cwd) = self.dupview_child_command(&abs, line);
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(&args);
        cmd.current_dir(&child_cwd);
        let child = cmd
            .spawn()
            .map_err(|e| XplError::trappable(format!("DUPVIEW: cannot open a new window: {e}")))?;
        // Windows blocks a background process from taking the foreground (the "foreground lock"), so
        // the freshly-spawned second instance would open WITHOUT keyboard focus — keys did nothing until
        // you clicked it into focus by hand. As the current foreground process we may hand that right to
        // the child by PID; it then calls `focus_window()` on resume and comes up ready to type. Granting
        // it now (well before the child's window exists) beats the race. No-op / not needed elsewhere.
        #[cfg(windows)]
        unsafe {
            #[link(name = "user32")]
            extern "system" {
                fn AllowSetForegroundWindow(dwProcessId: u32) -> i32;
            }
            // Grant the freshly-spawned second instance permission to take the foreground (Windows
            // blocks a background process from doing so on its own). The child also forces the
            // foreground itself on resume; this is the belt to that suspenders.
            AllowSetForegroundWindow(child.id());
        }
        let _ = child; // detached; dropping a Child does not wait or kill
        Ok(Value::str(""))
    }

    /// Build the argument list and working directory for a `DUPVIEW/W` child so it reproduces THIS
    /// instance's boot — split out from `spawn_second_window` so the wiring is unit-testable. The child:
    ///   * opens `abs` at `--goto <line>` (absolute path — independent of its working directory),
    ///   * runs the SAME startup script via `-s` (covers a parent booted from an explicit `-s <file>`
    ///     not beside the exe — otherwise the child auto-discovers nothing and comes up with defaults),
    ///   * defaults `$CWD` to its own `$XSDIR` before startup (the GUI-boot portable-folder default), so
    ///     the startup's RELATIVE config kit (`loadcfg cld.cfg`, `loadkbd CLD.kbd`, `loadlib …`) resolves
    ///     against the app folder exactly as it did for the parent (the child spawns in `boot_cwd`, but
    ///     that OS dir no longer governs config resolution — `$XSDIR` does),
    ///   * then has its `$CWD` restored to the parent's CURRENT working dir via `--cwd` (applied AFTER
    ///     startup runs, so it overrides both that default and any `cd` the startup performed).
    fn dupview_child_command(&self, abs: &Path, line: usize) -> (Vec<std::ffi::OsString>, PathBuf) {
        use std::ffi::OsString;
        let mut args: Vec<OsString> = vec![
            abs.as_os_str().to_os_string(),
            "--goto".into(),
            line.to_string().into(),
            "--cwd".into(),
            self.cwd.as_os_str().to_os_string(),
        ];
        if let Some(sp) = &self.startup_path {
            args.push("-s".into());
            args.push(sp.as_os_str().to_os_string());
        }
        (args, self.boot_cwd.clone())
    }

    /// Move the active document's caret to the start of 1-based `line` (clamped). Used by the `--goto`
    /// CLI flag (`DUPVIEW/W`'s new window lands at the source view's line).
    pub fn goto_line(&mut self, one_based: usize) {
        let doc = self.active();
        let total = doc.buffer().line_count();
        let l = one_based.saturating_sub(1).min(total.saturating_sub(1));
        let off = doc.buffer().line_byte_range(l).0;
        doc.set_caret(off, None);
    }

    /// Record the current on-disk modification time of `docs[idx]`'s file as the baseline for
    /// `AUTOREOPEN` change detection. No-op without a path, under `virtual_fs`, or if the stat fails.
    fn refresh_disk_mtime(&mut self, idx: usize) {
        if self.virtual_fs {
            return;
        }
        let Some(id) = self.docs.get(idx).map(|d| d.id()) else {
            return;
        };
        let Some(path) = self.docs[idx].path().map(|p| p.to_path_buf()) else {
            return;
        };
        if let Ok(mtime) = std::fs::metadata(&path).and_then(|m| m.modified()) {
            self.disk_mtimes.insert(id, mtime);
        }
    }

    /// Reload `docs[idx]` from its file on disk, preserving the caret + viewport (like `REC`). Used by
    /// `REC` and by `AUTOREOPEN`. Returns the file's display name. Refreshes the disk-mtime baseline.
    fn reload_doc_from_disk(&mut self, idx: usize) -> VmResult<String> {
        let path = match self.docs[idx].path() {
            Some(p) => p.to_path_buf(),
            None => return Err(XplError::trappable("REC: nothing to revert to (never saved)")),
        };
        let key = path.to_string_lossy().into_owned();
        let bytes = self.read_doc_bytes(&key)?;
        let restore = (!self.virtual_fs).then(|| xymeta_read(&path, &bytes)).flatten();
        let old_caret = self.docs[idx].buffer().cursor();
        self.docs[idx].load(bytes, path.clone());
        if let Some(r) = restore {
            apply_xymeta_restore(&mut self.docs[idx], r);
        }
        let len = self.docs[idx].buffer().len();
        self.docs[idx].set_caret(old_caret.min(len), None);
        self.refresh_disk_mtime(idx);
        Ok(path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(key))
    }

    /// `AUTOREOPEN` poll: for each open file whose on-disk copy changed since last seen, reload it if
    /// the buffer is clean (caret/viewport preserved), or — if it has unsaved edits — leave it and note
    /// the conflict. Returns status notes for the GUI. A no-op when off or under `virtual_fs`. The GUI
    /// calls this on a throttled timer (see the AUTOSAVE cadence).
    pub fn poll_external_changes(&mut self) -> Vec<String> {
        if !self.auto_reopen || self.virtual_fs {
            return Vec::new();
        }
        // Snapshot (index, id, path) first so the reload's `&mut self` doesn't clash with the scan.
        let open: Vec<(usize, crate::panes::DocId, PathBuf)> = self
            .docs
            .iter()
            .enumerate()
            .filter_map(|(i, d)| d.path().map(|p| (i, d.id(), p.to_path_buf())))
            .collect();
        let mut notes = Vec::new();
        for (idx, id, path) in open {
            let fs_mtime = match std::fs::metadata(&path).and_then(|m| m.modified()) {
                Ok(t) => t,
                Err(_) => continue, // deleted or unreadable — leave the buffer untouched
            };
            match self.disk_mtimes.get(&id).copied() {
                None => {
                    self.disk_mtimes.insert(id, fs_mtime); // first sight — baseline, don't reload
                }
                Some(prev) if fs_mtime == prev => {} // unchanged
                Some(_) => {
                    // Any DIFFERENCE (not just a newer time) counts as a change — some tools rewrite a
                    // file with an equal-or-older timestamp (git checkout, rsync --times, editors that
                    // preserve mtime), which a `>` test would silently miss.
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.to_string_lossy().into_owned());
                    if self.docs[idx].is_modified() {
                        // Don't clobber unsaved edits — note it once and re-baseline.
                        self.disk_mtimes.insert(id, fs_mtime);
                        notes.push(format!("{name} changed on disk — your unsaved edits are kept (REC to discard them)"));
                    } else if self.reload_doc_from_disk(idx).is_ok() {
                        notes.push(format!("reloaded {name} (changed on disk)"));
                    }
                }
            }
        }
        notes
    }

    /// Hand a URL to the OS default handler (Ctrl+click on a highlighted URL). Only web/mail schemes
    /// are launched; a `www.` URL is opened as `https://`. Errors surface on the status line.
    pub fn open_url(&mut self, url: &str) {
        let launch = normalize_url(url);
        match launch {
            Some(u) => match open_url_os(&u) {
                Ok(()) => self.last_status_line = format!("opening {u}"),
                Err(e) => self.last_status_line = format!("open failed: {e}"),
            },
            None => self.last_status_line = format!("not an openable URL: {url}"),
        }
    }
    /// Whether `MDA` underlines run through inter-word spaces (`ULSPACES`/`$ULSPACES`).
    pub fn ul_spaces(&self) -> bool {
        self.ul_spaces
    }
    /// Whether the active document has an `MDA` live-typing mode armed (for the status-line indicator).
    pub fn mda_typing_active(&self) -> bool {
        self.docs[self.active].mda_typing_active()
    }
    /// The active document's armed `MDA` typing layers `(fg, bg, attrs)` (for the status indicator).
    pub fn mda_typing(&self) -> (Option<(u8, u8, u8)>, Option<(u8, u8, u8)>, Option<u8>) {
        self.docs[self.active].mda_typing()
    }
    /// Clear the command-line contents WITHOUT touching focus — used by `CMBLANK on` after a command
    /// runs (the resting-focus rule has already decided where focus lands). Unlike `BlankCmdLine`,
    /// this does NOT re-focus the cmline.
    pub fn clear_command_line(&mut self) {
        self.cmline = Document::new();
    }

    /// GUI view flags (read each frame by the GUI). See `EditorOp::SetView`.
    pub fn menu_bar(&self) -> bool {
        self.menu_bar
    }
    /// Whether Writer's View is on (`WV`/`$WV`) — the GUI hides all chrome when it is.
    pub fn writer_view(&self) -> bool {
        self.writer_view
    }
    /// Turn Writer's View off directly (for the GUI's `Esc`-exits-WV path), without routing a command.
    pub fn set_writer_view(&mut self, on: bool) {
        self.writer_view = on;
    }
    pub fn visible_returns(&self) -> bool {
        self.visible_returns
    }
    pub fn visible_whitespace(&self) -> bool {
        self.visible_whitespace
    }
    pub fn line_numbers(&self) -> bool {
        self.line_numbers
    }
    pub fn gui_cmline(&self) -> bool {
        self.gui_cmline
    }
    /// Whether the `*Shell Output*` tab should appear in the tab strip (`SW`/`$SW`). The GUI hides
    /// that one tab when this is false; the buffer itself keeps existing and accumulating.
    pub fn shell_output_visible(&self) -> bool {
        self.shell_output_visible
    }

    /// Take the `(title, text)` a `TXT` verb requested be shown (consumed by the GUI, which
    /// opens the scrollable dialog).
    /// Drain a pending `MSG` message box `(text, timeout_secs)` for the GUI to display.
    pub fn take_pending_message(&mut self) -> Option<(String, Option<u64>)> {
        self.pending_message.take()
    }
    /// Whether `SAAS` (no path) asked for the native Save-As dialog; consumes the request.
    pub fn take_save_as_request(&mut self) -> bool {
        std::mem::take(&mut self.save_as_requested)
    }
    /// Whether `OPEN` (no path) asked for the native Open-file dialog; consumes the request.
    /// Drain a pending native-Open-dialog request: `Some(hex)` when `OPEN` (with no path) asked for
    /// it — `hex` is the `/HEX` intent — else `None`.
    pub fn take_open_request(&mut self) -> Option<bool> {
        self.open_requested.take()
    }

    pub fn take_find_request(&mut self) -> bool {
        std::mem::take(&mut self.find_requested)
    }

    /// Consume the pending `SCRATCH` open requests — one entry per window, each carrying its initial
    /// text (empty, or the clipboard for `SCRATCH/V`). The GUI drains this every frame.
    pub fn take_scratch_open_queue(&mut self) -> Vec<String> {
        std::mem::take(&mut self.scratch_open_queue)
    }

    /// Consume the `SCRATCHCLOSE` request (close the most-recently-focused scratchpad). `Some(true)`
    /// means also insert its text into the active tab at the caret (`SCRATCHCLOSE/V`). GUI-drained.
    pub fn take_scratch_close_request(&mut self) -> Option<bool> {
        std::mem::take(&mut self.scratch_close_requested)
    }

    /// Record the text of a scratchpad window as it closes, so `$SCRATCHTEXT` can recover it.
    pub fn set_scratch_text(&mut self, text: String) {
        self.scratch_text = text;
    }

    /// Consume the "open the GREP dialog" request (set by a bare `GREP`). The GUI drains it each frame.
    pub fn take_grep_dialog_request(&mut self) -> bool {
        std::mem::take(&mut self.grep_dialog_requested)
    }

    /// A one-shot: a search just found a match and the GUI should scroll it to mid-screen (if it's
    /// off-screen). Consumed once per frame by the GUI.
    pub fn take_reveal_center(&mut self) -> bool {
        std::mem::take(&mut self.reveal_center)
    }

    /// Whether the active document currently has a non-empty selection (e.g. a just-found match).
    pub fn active_has_selection(&self) -> bool {
        self.file_doc().buffer().has_selection()
    }

    /// The last search pattern (any source: SE/CH, F3, or the Find dialog). Seeds the Find dialog
    /// on reopen so it remembers the last term.
    pub fn last_search(&self) -> String {
        self.last_search.clone()
    }

    /// Commit a search string + its options as the "last search" WITHOUT performing a search, so a
    /// later bare `SE`/`SEB` (F3) repeats it. The FIND dialog calls this when it closes, so F3 continues
    /// the field's search even if the last action there was only a live type-to-find (which never
    /// touches the engine's search state). A no-op for an empty string.
    pub fn commit_last_search(&mut self, find: &str, case: bool, whole: bool, regex: bool) {
        if find.is_empty() {
            return;
        }
        self.last_search = find.to_string();
        self.last_spec = Some(SearchSpec {
            find: find.to_string(),
            replace: None,
            backward: false,
            case_sensitive: case,
            whole_word: whole,
            from_top: false,
            scope: false,
            regex,
            perl: false,
            keep_case: false,
            extend: false,
            verify: false,
            count: None,
        });
    }

    /// The active document's selected text (empty if nothing is selected). Used to seed the Find
    /// dialog from the current selection.
    pub fn active_selection_text(&self) -> String {
        let b = self.file_doc().buffer();
        if b.has_selection() {
            b.selection_text()
        } else {
            String::new()
        }
    }
    /// Drain `(var, output)` deliveries from finished `/R:<var>` background jobs. The editor writes
    /// each into the interpreter's `Env` after `poll_jobs` (the host can't reach user variables).
    pub fn take_pending_returns(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.pending_returns)
    }
    /// The font-picker dialog a `EDFONTS`/`UIFONTS`/`TXTFONTS` verb requested, if any; consumes it.
    pub fn take_font_picker_request(&mut self) -> Option<FontPickerKind> {
        self.font_picker_request.take()
    }
    /// Bare `MAN` requested the manual be opened (Help ▸ Manual). The GUI polls this each frame.
    pub fn take_show_manual_request(&mut self) -> bool {
        std::mem::take(&mut self.show_manual_request)
    }
    /// The colour-picker dialog a `*COLORS`/`HLCOLORS` verb requested, if any; consumes it.
    pub fn take_color_picker_request(&mut self) -> Option<ColorTarget> {
        self.color_picker_request.take()
    }

    /// Current LOCAL date/time, decomposed for the date specials (`$YEAR`, `$DA`, …).
    fn now_local(&self) -> crate::clock::DateTime {
        crate::clock::fields_from_millis(crate::clock::now_millis(), crate::dir::local_offset_secs())
    }

    /// The stopwatch elapsed time as `H:MM:SS` (`$ET`).
    fn elapsed_string(&self) -> String {
        let s = self.stopwatch_start.elapsed().as_secs();
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    }

    /// The working directory as a display string (`$CWD`; shown in the GUI status line).
    pub fn cwd_display(&self) -> String {
        self.cwd.to_string_lossy().into_owned()
    }

    /// The working directory (`$CWD`) as a path — e.g. to seed a native file dialog.
    pub fn cwd_path(&self) -> std::path::PathBuf {
        self.cwd.clone()
    }

    /// The active document's buffer (for rendering / inspection).
    pub fn document(&self) -> &Buffer {
        self.docs[self.active].buffer()
    }

    /// The buffer of the document at `idx` (0-based, into the tab order). Lets the render path window
    /// a specific pane's document rather than always the active one — `doc(active_index())` ==
    /// [`document`](Self::document). Panics on an out-of-range index (callers pass a live tab index).
    pub fn doc(&self, idx: usize) -> &Buffer {
        self.docs[idx].buffer()
    }

    /// The command-line buffer.
    pub fn command_line(&self) -> &Buffer {
        self.cmline.buffer()
    }

    /// The active document (read-only).
    pub fn active_doc(&self) -> &Document {
        &self.docs[self.active]
    }

    /// Ensure the active document's highlight cache covers the DECODED range `[dec_lo, dec_hi]` (the
    /// renderer's visible span) — windowed retokenize only when the view moved off the cached span or
    /// the content changed. The GUI calls this each frame before reading the tokens.
    pub fn ensure_highlights(&mut self, dec_lo: usize, dec_hi: usize) {
        self.docs[self.active].ensure_highlights(dec_lo, dec_hi);
    }

    /// The document at tab index `idx` (read-only) — for inspecting a non-active tab (highlighter,
    /// path, render override).
    pub fn document_at(&self, idx: usize) -> &Document {
        &self.docs[idx]
    }

    /// Cursor addition (`ADDSUM`): the cleaned numeric text of the number token at (or just after) the
    /// caret on the current line — currency signs and thousands-separators stripped, a leading sign
    /// kept — ready to parse as a `Number`. `Err` when the caret is not on a number.
    fn number_under_cursor(&self) -> Result<String, String> {
        let b = self.active_doc().buffer();
        let cur = b.cursor();
        let line = b.line_of(cur);
        let (line_start, _line_end) = b.line_byte_range(line);
        let text = b.line_text(line); // the line's content, without its trailing break
        let col = cur.saturating_sub(line_start); // caret's byte offset within the line
        // The number token the caret sits on: its span [s, e] contains the caret (end inclusive, so the
        // caret just past the last digit still counts). Pick the first such token on the line.
        let span = number_token_regex()
            .find_iter(&text)
            .map(|m| (m.start(), m.end()))
            .find(|&(s, e)| col >= s && col <= e);
        let (s, e) = span.ok_or_else(|| "no number under the cursor".to_string())?;
        Ok(clean_number(&text[s..e]))
    }

    /// Active document's file name (or `None` if untitled).
    pub fn active_file_name(&self) -> Option<String> {
        self.docs[self.active].file_name()
    }

    /// Whether the active document has unsaved changes.
    pub fn is_active_modified(&self) -> bool {
        self.docs[self.active].is_modified()
    }

    /// Number of open documents, and the active 0-based index.
    pub fn buffer_count(&self) -> usize {
        self.docs.len()
    }
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// Per-buffer info for the GUI tab strip (in buffer order; active is `active_index()`).
    pub fn tabs(&self) -> Vec<TabInfo> {
        self.docs
            .iter()
            .map(|d| TabInfo {
                name: d.file_name().unwrap_or_else(|| "untitled".to_string()),
                path: d.path().map(|p| p.display().to_string()),
                modified: d.is_modified(),
                read_only: d.read_only(),
                dir_listing: d.is_dir_listing(),
            })
            .collect()
    }

    /// Make buffer `i` active (clicking a tab); focuses the document. Out-of-range is ignored.
    /// Change the user's input focus (cmline vs text): sets both the operating `target` and the
    /// persistent `input_focus` that `$TX` reads. Use for GENUINE focus changes — NOT the
    /// transient `target = Document` flip that runs a cmline-invoked command on the document.
    fn set_focus(&mut self, t: Target) {
        self.target = t;
        self.input_focus = t;
    }

    /// Direct editing (`InsertText`/`TypeText`) at the ACTIVE DOCUMENT's caret rather than wherever the
    /// input focus happens to be (e.g. the command line). Used by `SCRATCHCLOSE/V`, which inserts a
    /// closing scratchpad's text into the document even though the command was issued from the cmline.
    pub fn focus_document(&mut self) {
        self.set_focus(Target::Document);
    }

    pub fn set_active(&mut self, i: usize) {
        if i < self.docs.len() {
            self.switch_buffer(i);
            self.set_focus(Target::Document);
        }
    }

    /// Move the active buffer to `i`, remembering the buffer we're leaving as the
    /// *alternate* (for `BUFALT`). No-op focus change if `i` is out of range or already
    /// active. All deliberate buffer-navigation paths funnel through here so the
    /// alternate always reflects the last distinct buffer you were on.
    pub(crate) fn switch_buffer(&mut self, i: usize) {
        if i >= self.docs.len() || i == self.active {
            return;
        }
        // Remember the tab we're leaving (by stable id) as the last-visited one, for `CLOSEBACK`.
        self.focus_prev = self.docs.get(self.active).map(|d| d.id());
        self.alt_active = Some(self.active);
        self.active = i;
        self.sync_panes();
    }

    /// Keep the split-pane layout consistent with the shared tab list and `active` — the stable-views
    /// reconcile. Panes are stable single-doc views onto the one global tab list (`self.docs`): this
    /// shows `active` in the focused pane (or moves focus to the pane already showing it) and keeps
    /// every OTHER pane exactly where and what it was. On a close it repoints a pane that was showing
    /// the closed doc to another open doc (so the pane stays put) rather than hoisting/collapsing it;
    /// a pane collapses only when there are fewer open docs than panes. Called after any change to
    /// `docs`/`active` (tab switch, open, close). Cheap; safe to over-call.
    pub(crate) fn sync_panes(&mut self) {
        if self.docs.is_empty() {
            return;
        }
        if self.active >= self.docs.len() {
            self.active = self.docs.len() - 1;
        }
        let ids: Vec<crate::panes::DocId> = self.docs.iter().map(|d| d.id()).collect();
        let active = self.docs[self.active].id();
        self.panes.reconcile(&ids, active);
    }

    /// The split-pane layout tree (read by the GUI to lay out and render; Phase 2).
    pub fn panes(&self) -> &crate::panes::PaneTree {
        &self.panes
    }

    /// Mutable access to the split-pane tree (verbs: split/close/focus/zoom; Phase 3).
    pub fn panes_mut(&mut self) -> &mut crate::panes::PaneTree {
        &mut self.panes
    }

    /// Index of the open document with this [`DocId`], if any.
    fn doc_index_by_id(&self, id: crate::panes::DocId) -> Option<usize> {
        self.docs.iter().position(|d| d.id() == id)
    }

    /// After a TREE-focus change (`TW`, split, collapse), make `active` follow the focused pane's doc
    /// — the reverse of [`sync_panes`](AppHost::sync_panes) (which makes the tree follow `active`).
    /// Keyboard input and the content `$`-vars target `active`, so the focused pane IS the active doc.
    fn adopt_pane_focus(&mut self) {
        let id = self.panes.focused_doc();
        if let Some(i) = self.doc_index_by_id(id) {
            self.active = i;
        }
    }

    /// Create a fresh empty buffer (sharing the grammar registry), append it to `docs`, return its id.
    fn push_fresh_buffer(&mut self) -> crate::panes::DocId {
        let mut doc = Document::new();
        doc.share_grammars(self.highlight_grammars.clone());
        let id = doc.id();
        self.docs.push(doc);
        id
    }

    /// `NEW/H`, `NEW/V`: split the focused pane and put a fresh empty buffer in the NEW pane (above for
    /// Stacked, left for SideBySide — the `First` side), which takes focus. Errs only on an impossible
    /// tree state (guard can't trip: the buffer is brand-new).
    pub fn pane_split_new(&mut self, dir: crate::panes::Split) -> Result<(), String> {
        let id = self.push_fresh_buffer();
        match self.panes.split(dir, id, crate::panes::Side::First) {
            Ok(_) => {
                self.adopt_pane_focus();
                Ok(())
            }
            Err(e) => {
                self.docs.retain(|d| d.id() != id); // undo the push on failure
                Err(e.to_string())
            }
        }
    }

    /// `THS`, `TVS`: split the focused pane, keeping the current buffer on top/left (`First`) and the
    /// PARTNER on the bottom/right (`Second`, which takes focus). Partner = the named open tab if
    /// `target` is given, else the alternate buffer (`alt_active`) when it's open and not already
    /// shown, else a fresh empty buffer.
    pub fn pane_split_tile(
        &mut self,
        dir: crate::panes::Split,
        target: Option<String>,
    ) -> Result<(), String> {
        let t = target.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let (partner, pushed) = self.resolve_pane_partner(t)?;
        match self.panes.split(dir, partner, crate::panes::Side::Second) {
            Ok(_) => {
                self.adopt_pane_focus();
                Ok(())
            }
            Err(e) => {
                if pushed {
                    self.docs.retain(|d| d.id() != partner);
                }
                Err(e.to_string())
            }
        }
    }

    /// Resolve the partner buffer for a tile split. Returns `(id, pushed_new)` where `pushed_new` marks
    /// a brand-new buffer appended to `docs` (so a failed split can undo it).
    fn resolve_pane_partner(
        &mut self,
        target: Option<&str>,
    ) -> Result<(crate::panes::DocId, bool), String> {
        if let Some(t) = target {
            match self.docs.iter().position(|d| d.file_name().as_deref() == Some(t)) {
                Some(i) => {
                    let id = self.docs[i].id();
                    if self.panes.contains_doc(id) {
                        return Err(format!("{t} is already shown in a pane"));
                    }
                    Ok((id, false))
                }
                None => Err(format!("{t}: no open tab by that name — open it first")),
            }
        } else {
            // Prefer the alternate buffer (the BUFALT target) when it's open and not already paned.
            if let Some(alt) = self.alt_active {
                if alt < self.docs.len() {
                    let id = self.docs[alt].id();
                    if !self.panes.contains_doc(id) {
                        return Ok((id, false));
                    }
                }
            }
            Ok((self.push_fresh_buffer(), true)) // nothing suitable → a fresh empty buffer
        }
    }

    /// `SW/H` / `SW/V`: show the *Shell Output* buffer in a new split pane (creating the buffer if
    /// needed, and making sure it's marked visible). If it's already shown in a pane, just focus that
    /// pane. Splits DIRECTLY with the SO buffer's id — no throwaway empty pane — since it pre-exists.
    pub fn pane_open_shell(&mut self, dir: crate::panes::Split) -> Result<(), String> {
        let idx = self.shell_output_doc_index(); // find-or-create the SO buffer
        self.shell_output_visible = true; // showing it in a pane implies it's visible
        let so_id = self.docs[idx].id();
        if self.panes.focus_doc_if_shown(so_id) {
            self.adopt_pane_focus(); // already in a pane → just focus it
            return Ok(());
        }
        match self.panes.split(dir, so_id, crate::panes::Side::Second) {
            Ok(_) => {
                self.adopt_pane_focus();
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// `TW`: move focus to the next pane (wraps); `active` follows.
    pub fn pane_focus_next(&mut self) {
        self.panes.focus_cycle(true);
        self.adopt_pane_focus();
    }

    /// `TUS` (and `AB` when split): collapse the focused pane, hoisting its sibling. Returns whether a
    /// collapse happened (false when already a single pane). The collapsed pane's BUFFER stays open as
    /// a tab — only the pane goes away.
    pub fn pane_collapse(&mut self) -> bool {
        let did = self.panes.close_focused();
        if did {
            self.adopt_pane_focus();
        }
        did
    }

    /// `TRV`: swap the two panes of the split containing the focused pane. Focus (and `active`) stay on
    /// the same pane, which is now in the other position.
    pub fn pane_swap(&mut self) -> bool {
        self.panes.swap_focused()
    }

    /// `FS`: release the focused pane to full window — collapse the entire split layout down to just
    /// the focused pane (the other panes' buffers stay open as tabs). Returns whether anything changed
    /// (false when already a single pane). Distinct from `TUS`/`AB` (which remove the focused pane and
    /// keep its sibling) and from the `TZ` toggle-zoom preview.
    pub fn pane_fullscreen(&mut self) -> bool {
        let did = self.panes.collapse_to_focused();
        if did {
            self.adopt_pane_focus(); // active already = the focused pane's doc; keep it in sync
        }
        did
    }

    /// `TZ`: toggle full-window zoom of the focused pane.
    pub fn pane_zoom(&mut self) {
        self.panes.toggle_zoom();
    }

    /// Number of visible panes (`$PANES`).
    pub fn pane_count(&self) -> usize {
        self.panes.pane_count()
    }

    /// The focused pane's id (for the GUI to compare against `layout_rects`).
    pub fn focused_pane_id(&self) -> crate::panes::PaneId {
        self.panes.focus()
    }

    /// Mirror a pane's live scroll offset (held in the GUI's per-pane viewport) into its `ViewState`.
    /// The GUI calls this for every pane just before a `SESSION` snapshot so the saved layout carries
    /// each pane's exact scroll position, and reads it back with [`pane_scroll`](Self::pane_scroll)
    /// after a restore.
    pub fn set_pane_scroll(&mut self, pane: crate::panes::PaneId, scroll_line: usize, scroll_vertical: f32, follow: bool) {
        self.panes.set_view(pane, crate::panes::ViewState { scroll_line, scroll_vertical, follow });
    }

    /// Read a pane's stored scroll offset as `(scroll_line, scroll_vertical, follow)` — `None` if the
    /// pane is gone. The GUI seeds its per-pane viewport from this after restoring a session.
    pub fn pane_scroll(&self, pane: crate::panes::PaneId) -> Option<(usize, f32, bool)> {
        self.panes.view(pane).map(|v| (v.scroll_line, v.scroll_vertical, v.follow))
    }

    /// Resolve the pane layout for a `w`×`h` area into `(pane, doc-index, rect)` triples — the GUI
    /// renders each doc-index into its rect. A pane whose `DocId` isn't currently open (shouldn't
    /// happen) is dropped.
    pub fn pane_layout(
        &self,
        w: f32,
        h: f32,
    ) -> Vec<(crate::panes::PaneId, usize, crate::panes::Rect)> {
        let area = crate::panes::Rect { x: 0.0, y: 0.0, w, h };
        self.panes
            .layout_rects(area)
            .into_iter()
            .filter_map(|pr| self.doc_index_by_id(pr.doc).map(|i| (pr.pane, i, pr.rect)))
            .collect()
    }

    /// The draggable dividers for a `w`×`h` area (see [`PaneTree::dividers`](crate::panes::PaneTree::dividers)).
    pub fn pane_dividers(&self, w: f32, h: f32) -> Vec<crate::panes::Divider> {
        self.panes.dividers(crate::panes::Rect { x: 0.0, y: 0.0, w, h })
    }

    /// Resize the `index`-th split (from [`pane_dividers`](AppHost::pane_dividers)) to `ratio`.
    pub fn set_pane_ratio(&mut self, index: usize, ratio: f32) {
        self.panes.set_divider_ratio(index, ratio);
    }

    /// Focus a pane by id (a GUI click), pointing `active` at its document.
    pub fn focus_pane(&mut self, pane: crate::panes::PaneId) {
        self.panes.set_focus(pane);
        self.adopt_pane_focus();
    }

    /// Point `active` at doc `i` WITHOUT the alternate/sync side effects of `switch_buffer` — for the
    /// GUI's render-time swap-state loop (render each pane's doc, then restore). Not a navigation.
    pub fn set_active_raw(&mut self, i: usize) {
        if i < self.docs.len() {
            self.active = i;
        }
    }

    /// 1-based position of the focused pane in layout order (`$PANE`).
    pub fn focused_pane_position(&self) -> usize {
        let focus = self.panes.focus();
        self.panes
            .panes()
            .iter()
            .position(|&(p, _)| p == focus)
            .map(|i| i + 1)
            .unwrap_or(1)
    }

    /// Record a submitted CMline command in the history. It goes to the FRONT (newest); any existing
    /// identical entry is removed first, so re-running a command moves it to the top and the history
    /// never holds duplicates. Skips blanks; caps the list. Resets any in-progress Up/Down walk.
    pub fn push_history(&mut self, cmd: &str) {
        self.history_pos = None;
        let c = cmd.trim();
        if c.is_empty() {
            return;
        }
        // Move-to-front + dedup: drop any existing identical entry, then add it at the front.
        self.command_history.retain(|h| h.as_str() != c);
        self.command_history.insert(0, c.to_string()); // newest first
        const CAP: usize = 500;
        self.command_history.truncate(CAP);
    }

    /// Replace the CMline buffer with `text` (caret at end) and focus it — used by history recall
    /// and the HISTORY picker's Shift+Enter (load a command to edit without running it).
    pub(crate) fn set_cmline_text(&mut self, text: &str) {
        self.cmline = Document::new();
        if !text.is_empty() {
            self.cmline.insert(text);
        }
        self.set_focus(Target::CmLine);
    }

    /// Up-arrow recall: step to an older history entry into the CMline (newest first, so index
    /// grows as you go back in time).
    pub fn history_prev(&mut self) {
        if self.command_history.is_empty() {
            return;
        }
        let pos = match self.history_pos {
            None => {
                self.history_draft = self.cmline.buffer().text(); // save the in-progress line
                0 // the most recent
            }
            Some(p) if p + 1 < self.command_history.len() => p + 1, // older
            Some(p) => p,                                           // already the oldest
        };
        self.history_pos = Some(pos);
        let cmd = self.command_history[pos].clone();
        self.set_cmline_text(&cmd);
    }

    /// Down-arrow recall: step to a newer entry, or back to the in-progress line.
    pub fn history_next(&mut self) {
        match self.history_pos {
            None => {}
            Some(0) => {
                self.history_pos = None;
                let draft = self.history_draft.clone();
                self.set_cmline_text(&draft);
            }
            Some(p) => {
                self.history_pos = Some(p - 1);
                let cmd = self.command_history[p - 1].clone();
                self.set_cmline_text(&cmd);
            }
        }
    }

    /// The command history (newest first), for recall + the `HISTORY` picker.
    pub fn command_history(&self) -> &[String] {
        &self.command_history
    }

    /// The reopen list (recently-closed files, newest first): `(path, caret)`. For the `REOPEN`
    /// chooser (Phase 2) and tests.
    pub fn recently_closed(&self) -> &[(PathBuf, usize)] {
        &self.recently_closed
    }

    /// A filtered view of the history for the `HISTORY` picker: returns `(index, command)` pairs
    /// (the index is into `command_history`, for deletion). `filter` = a number → the last N
    /// (most-recent) commands; any other non-empty text → commands containing it (case-insensitive);
    /// empty → all. Order is newest first.
    pub fn history_view(&self, filter: &str) -> Vec<(usize, String)> {
        let f = filter.trim();
        let all = self.command_history.iter().enumerate().map(|(i, s)| (i, s.clone()));
        if f.is_empty() {
            return all.collect();
        }
        if let Ok(n) = f.parse::<usize>() {
            return all.take(n).collect(); // the N most recent (newest first)
        }
        let needle = f.to_lowercase();
        all.filter(|(_, s)| s.to_lowercase().contains(&needle)).collect()
    }
    /// Delete the history entry at `i` (the picker's Del). Out-of-range is ignored.
    pub fn delete_history(&mut self, i: usize) {
        if i < self.command_history.len() {
            self.command_history.remove(i);
            self.history_pos = None;
        }
    }
    /// The pending `HISTORY`/`HI` picker request (with its filter arg), if any; consumes it.
    pub fn take_history_request(&mut self) -> Option<String> {
        self.history_request.take()
    }
    /// A pending `MENU` request since the last poll (`Some(None)` = bare toggle, `Some(Some("f"))`
    /// = open the top menu with accelerator `f`). The GUI drains + applies it.
    pub fn take_menu_kbd_request(&mut self) -> Option<Option<String>> {
        self.menu_kbd_request.take()
    }

    /// Autosave interval in seconds (0 = off); the GUI reads this each frame to drive timing.
    pub fn autosave_secs(&self) -> u64 {
        self.autosave_secs
    }
    /// Tab width in spaces (`TB`/`$TB`); the GUI uses it for visual tab stops.
    pub fn tab_width(&self) -> usize {
        self.tab_width
    }
    /// The `LANG` translation for UI string `id`, or `None` to fall back to built-in English. The
    /// GUI wraps this in `tr(id, english)`; empty table (no `LANG` loaded) ⇒ always English.
    pub fn lang_get(&self, id: &str) -> Option<&str> {
        self.lang.get(id).map(String::as_str)
    }
    /// The (resolved) path of the loaded `LANG` catalog, or `None` when English. Read back as
    /// `$LANG`; the GUI uses it to tick the active language in the `Settings ▸ Language` menu.
    pub fn lang_path(&self) -> Option<&str> {
        self.lang_path.as_deref()
    }
    /// Find a `LANG` override file `<stem>.local.lang` for a just-loaded base catalog: sits next to
    /// the base (so `.\lang` overrides live with the catalogs) or in `$CWD`. Returns its `(path,
    /// contents)`, or `None`. Read errors (no such file) are the common case and mean "no overlay".
    fn find_overlay(&self, base_resolved: &str, stem: &str) -> Option<(String, String)> {
        let name = format!("{stem}.local.lang");
        let base_dir = std::path::Path::new(base_resolved).parent().map(|p| p.to_path_buf()).unwrap_or_default();
        for dir in [base_dir, self.cwd.clone()] {
            let cand = dir.join(&name).to_string_lossy().into_owned();
            if let Ok(text) = self.read_file(&cand) {
                return Some((cand, text));
            }
        }
        None
    }
    /// A localized status/error MESSAGE: the `LANG` translation for `id`, else the English `en`.
    /// Host-side sibling of the GUI's `tr` (the `lang` table lives here, so messages the host emits
    /// translate at the source). Plain — no placeholders; use [`tr_fmt`](Self::tr_fmt) for those.
    fn tr_msg(&self, id: &str, en: &str) -> String {
        self.lang.get(id).map(String::as_str).unwrap_or(en).to_string()
    }
    /// Like [`tr_msg`](Self::tr_msg) but with named `{placeholder}` substitution: takes the translated
    /// (or English) template and replaces each `{name}` with its value. Named — not positional — so a
    /// translation can reorder the pieces (e.g. put `{path}` first). Values are substituted verbatim.
    fn tr_fmt(&self, id: &str, en: &str, args: &[(&str, &dyn std::fmt::Display)]) -> String {
        let mut s = self.lang.get(id).map(String::as_str).unwrap_or(en).to_string();
        for (name, value) in args {
            s = s.replace(&format!("{{{name}}}"), &value.to_string());
        }
        s
    }
    /// The CLDR-style plural CATEGORY for `n` under the loaded catalog's declared rule (the `_plural`
    /// key). English and most languages are the implicit `one`/`other`; Slavic/Romanian catalogs
    /// declare a rule that adds `few`/`many`. The rule NAMES and arithmetic here are mirrored by
    /// `i18n-sync.py`, which generates the matching `<base>.<category>` keys per language — keep them
    /// in step. Only the grep count phrases pluralize, and `n == 0` never reaches here (grep reports
    /// "no matches" via `msg.grep.none`), so the zero row of each rule is academic.
    fn plural_category(&self, n: usize) -> &'static str {
        match self.lang.get("_plural").map(String::as_str) {
            // Polish: one=1; few=2–4 excluding the teens; many=the rest.
            Some("polish") => {
                let (unit, teen) = (n % 10, n % 100);
                if n == 1 {
                    "one"
                } else if (2..=4).contains(&unit) && !(12..=14).contains(&teen) {
                    "few"
                } else {
                    "many"
                }
            }
            // Russian/Ukrainian: one=n%10==1 except the teens; few=n%10 in 2–4 except the teens;
            // many=the rest. Unlike Polish, "one" covers 21, 31, … (not just 1).
            Some("east-slavic") => {
                let (unit, teen) = (n % 10, n % 100);
                if unit == 1 && teen != 11 {
                    "one"
                } else if (2..=4).contains(&unit) && !(12..=14).contains(&teen) {
                    "few"
                } else {
                    "many"
                }
            }
            // Czech: one=1; few=2–4 exactly; other=the rest.
            Some("czech") => {
                if n == 1 {
                    "one"
                } else if (2..=4).contains(&n) {
                    "few"
                } else {
                    "other"
                }
            }
            // Romanian: one=1; few=0 or n%100 in 2–19 (bare noun); other=the rest (the "de" form).
            Some("romanian") => {
                let teen = n % 100;
                if n == 1 {
                    "one"
                } else if n == 0 || (2..=19).contains(&teen) {
                    "few"
                } else {
                    "other"
                }
            }
            // Default one/other: en, de, es, it, nl, pt, sv, da, nb, fi, hu, fr, …
            _ => {
                if n == 1 {
                    "one"
                } else {
                    "other"
                }
            }
        }
    }

    /// A count phrase with plural selection: `<base>.<category>` for the loaded catalog's plural rule
    /// (see [`plural_category`](Self::plural_category)), with `{n}` filled in. `en_one`/`en_other`
    /// are the built-in English templates — English has only one/other, so `few`/`many`/`other` all
    /// fall back to `en_other`. NOTE: `i18n-sync.py` extracts BOTH `<base>.one` and `<base>.other`
    /// from each `tr_plural(...)` call and expands the extra categories per language — keep the base
    /// and both English templates as string literals at the call site so translators see the forms.
    fn tr_plural(&self, base: &str, en_one: &str, en_other: &str, n: usize) -> String {
        let cat = self.plural_category(n);
        let en = if cat == "one" { en_one } else { en_other };
        self.tr_fmt(&format!("{base}.{cat}"), en, &[("n", &n)])
    }
    /// `BALPAIRS` — is matched-bracket highlighting on?
    pub fn bal_pairs(&self) -> bool {
        self.bal_pairs
    }
    /// `WRAPINDENT` — should soft-wrapped continuation rows hang-indent under the line's indentation?
    pub fn wrap_indent(&self) -> bool {
        self.wrap_indent
    }
    /// `BALPAIRSCOLOR` — the highlight colour, or `None` to follow the selection colour.
    pub fn bal_pairs_color(&self) -> Option<(u8, u8, u8)> {
        self.bal_color
    }
    /// The `EMPH` emphasis colours as SET (`None` = follow the selection colour) — for SaveCfg / the
    /// picker seed. `emph_fg_color`/`emph_bg_color` give the EFFECTIVE colour used for drawing.
    pub fn emph_fg_override(&self) -> Option<(u8, u8, u8)> {
        self.emph_fg
    }
    pub fn emph_bg_override(&self) -> Option<(u8, u8, u8)> {
        self.emph_bg
    }
    /// The EFFECTIVE `EMPH` emphasis glyph colour: the `EMPHFGCOLOR` override, or the selected-text
    /// colour (`SELFGCOLOR`) when unset.
    pub fn emph_fg_color(&self) -> (u8, u8, u8) {
        self.emph_fg.unwrap_or(self.sel_fg_color)
    }
    /// The EFFECTIVE `EMPH` emphasis background colour: the `EMPHBGCOLOR` override, or the selection
    /// background (`SELBGCOLOR`) when unset.
    pub fn emph_bg_color(&self) -> (u8, u8, u8) {
        self.emph_bg.unwrap_or(self.sel_bg_color)
    }
    /// The rope-offset spans of every case-insensitive occurrence of the ACTIVE document's `EMPH`
    /// phrase (empty when no emphasis is set). The GUI paints these as a persistent highlight.
    pub fn emph_highlights(&self) -> Vec<(usize, usize)> {
        let doc = &self.docs[self.active];
        let phrase = match doc.emph() {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => return Vec::new(),
        };
        // Regex (`EMPH/R`): match as a regular expression. Otherwise case-insensitive whole-word
        // literal. Either way, whole document; `find_all` yields nothing for an invalid regex.
        let regex = doc.emph_is_regex();
        self.find_all(&phrase, false, !regex, regex, false)
    }

    /// EMPH spans within the rope range `[rope_lo, rope_hi)` only — the windowed form (same whole-file
    /// hazard as [`url_highlights_in`](Host::url_highlights_in): a standing EMPH on a huge file would
    /// otherwise re-scan the whole buffer on the first paint and every edit).
    pub fn emph_highlights_in(&self, rope_lo: usize, rope_hi: usize) -> Vec<(usize, usize)> {
        let doc = &self.docs[self.active];
        let phrase = match doc.emph() {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => return Vec::new(),
        };
        let regex = doc.emph_is_regex();
        let pattern = match Pattern::build(&phrase, false, !regex, regex, false) {
            Ok(p) => p,
            Err(_) => return Vec::new(),
        };
        const CAP: usize = 5000;
        doc.buffer().find_all_in_range(&pattern, rope_lo, rope_hi, CAP)
    }
    /// The bracket match at the active document's caret, for the GUI to highlight. Returns
    /// [`BalMatch::None`] when `BALPAIRS` is off (so the GUI can skip all bracket work in one check).
    pub fn bracket_match(&self) -> BalMatch {
        if !self.bal_pairs {
            return BalMatch::None;
        }
        let b = self.docs[self.active].buffer();
        b.bracket_match(b.cursor())
    }
    /// Perform one autosave pass: save every named, modified buffer. `Ok(n)` = how many were written
    /// (unnamed buffers can't be autosaved); `Err(msg)` = a disk-write error (surfaced so a silently
    /// failing autosave — e.g. a full disk or a locked file — can't quietly lose the user's work).
    pub fn autosave(&mut self) -> Result<usize, String> {
        self.save_all_modified().map(|(saved, _)| saved).map_err(|e| e.message().to_string())
    }

    /// Number of background jobs (`X/BG`) still running. `$JOBS`.
    pub fn job_count(&self) -> usize {
        self.jobs.len()
    }

    /// Whether a background job with `id` is still running (a finished one is dropped from
    /// the list by `poll_jobs`, so an unknown/finished id reads as not-running).
    pub fn is_job_running(&self, id: u64) -> bool {
        self.jobs.iter().any(|j| j.id == id)
    }

    /// Append text to the shared `*Shell Output*` buffer (creating it as a new tab the first
    /// time). Does NOT change the active buffer — output appears in a background tab.
    /// The `*Shell Output*` buffer's index, creating the tab the first time.
    fn shell_output_doc_index(&mut self) -> usize {
        match self.docs.iter().position(|d| d.label() == Some(SHELL_OUTPUT_NAME)) {
            Some(i) => i,
            None => {
                let mut d = Document::new();
                d.set_label(SHELL_OUTPUT_NAME.to_string());
                self.docs.push(d);
                self.docs.len() - 1
            }
        }
    }

    /// `X/BG` / background-job output → the Console. A thin wrapper over [`console_emit`] so background
    /// output splices in above a waiting prompt just like a foreground command's does.
    fn append_shell_output_text(&mut self, text: &str) {
        self.console_emit(text);
    }

    /// The prompt string with its `$`-fields expanded (currently `$CWD`). Written at the tail by
    /// [`console_write_prompt`]; the rest of the template is emitted verbatim.
    fn expand_prompt(&self) -> String {
        self.console_prompt.replace("$CWD", &self.cwd.display().to_string())
    }

    /// Emit CONSOLE output. While a prompt is displayed the text splices in ABOVE it (so the prompt +
    /// the user's in-progress input stay pinned at the bottom); otherwise it appends at the tail. The
    /// scrollback stays bounded, and the prompt anchor slides left with any front-trim.
    fn console_emit(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let idx = self.shell_output_doc_index();
        // `ANSICOLOR off`: emit the raw text (escape codes and all), no parsing or colour.
        let runs = if self.ansi_color {
            self.console_ansi.feed(text)
        } else {
            vec![(text.to_string(), crate::ansi::Style::default())]
        };
        // Each run is plain text (escapes stripped) + its style. Insert the text, then record the
        // matching MDA colour/attribute span over the range it landed on.
        for (run, style) in runs {
            if run.is_empty() {
                continue;
            }
            let (start, end) = if self.console_showing {
                let at = self.console_prompt_at;
                let n = self.docs[idx].splice_console(at, &run); // shifts existing spans + the prompt
                self.console_prompt_at += n;
                (at, at + n)
            } else {
                let at = self.docs[idx].buffer().len();
                self.docs[idx].append_console(&run);
                (at, self.docs[idx].buffer().len())
            };
            if !style.is_default() && end > start {
                if let Some(fg) = style.fg {
                    self.docs[idx].set_color_span(start, end, Some(fg));
                }
                if let Some(bg) = style.bg {
                    self.docs[idx].set_bg_color_span(start, end, Some(bg));
                }
                if style.attrs != 0 {
                    self.docs[idx].set_attr_span(start, end, Some(style.attrs));
                }
            }
        }
        let cut = self.docs[idx].truncate_log_front(SHELL_OUTPUT_CAP);
        self.console_prompt_at = self.console_prompt_at.saturating_sub(cut);
        // The user's interactive-stdin input begins after the freshest output (so a child's own prompt,
        // e.g. "Name? ", is never captured as part of the typed line). Only consulted while a command
        // is running (`console_stdin` is Some, hence no prompt showing → output appended at the tail).
        self.console_input_at = self.docs[idx].buffer().len();
        self.console_dirty = true; // the GUI scrolls the Console to the tail
    }

    /// Test hook: push raw output (including ANSI escapes) through the Console emit path.
    #[cfg(test)]
    pub fn console_emit_test(&mut self, text: &str) {
        self.console_emit(text);
    }

    /// The GUI drains this each frame; `true` means Console output (or a new prompt) just arrived, so
    /// scroll the Console tab to the tail. Paired with [`console_is_active`].
    pub fn take_console_dirty(&mut self) -> bool {
        std::mem::take(&mut self.console_dirty)
    }

    /// The GUI drains this each frame; `true` means `SH` just opened/focused the Console, so the view
    /// must jump to the tail (the prompt) even if scroll-lock had paused following. See
    /// [`console_reveal_tail`](Self::console_reveal_tail).
    pub fn take_console_reveal_tail(&mut self) -> bool {
        std::mem::take(&mut self.console_reveal_tail)
    }

    /// Whether the Console tab is the active document (the GUI only auto-scrolls it when it's on top).
    pub fn console_is_active(&self) -> bool {
        self.docs.get(self.active).and_then(|d| d.label()) == Some(SHELL_OUTPUT_NAME)
    }

    /// Write a fresh prompt at the tail (on its own line) and mark the Console ready for input. Records
    /// the prompt's start offset + length so [`console_submit`] can extract the command that follows.
    pub fn console_write_prompt(&mut self) {
        let idx = self.shell_output_doc_index();
        let end = self.docs[idx].buffer().len();
        // Start the prompt on a fresh line unless we're already at the start of one.
        if end > 0 && self.docs[idx].buffer().slice_bytes(end - 1, end) != b"\n" {
            self.docs[idx].append_console("\n");
        }
        let prompt = self.expand_prompt();
        let at = self.docs[idx].buffer().len();
        self.docs[idx].append_console(&prompt);
        self.console_prompt_at = at;
        self.console_prompt_len = prompt.len();
        self.console_showing = true;
        self.console_hist_pos = None; // a fresh prompt starts at the (empty) newest input
        self.console_dirty = true; // reveal the fresh prompt
    }

    /// `CLS` / `CLEAR` — empty the Console scrollback and start fresh. A fresh prompt is written unless
    /// a command is still running (its output keeps streaming into the cleared buffer). History is
    /// kept (Shift+Up still recalls). Reuses the Console doc, so its tab/pane identity is unchanged.
    pub fn console_clear(&mut self) {
        let idx = self.shell_output_doc_index();
        self.docs[idx].console_clear();
        self.console_prompt_at = 0;
        self.console_prompt_len = 0;
        self.console_input_at = 0;
        self.console_showing = false;
        self.console_ansi.reset();
        if !self.console_command_running() {
            self.console_write_prompt();
        }
        self.console_dirty = true;
    }

    /// Handle Enter in the Console. Returns `false` — so the GUI inserts a normal newline — unless the
    /// Console is the active tab AND the caret is at the very end (typing at the tail, not editing
    /// scrollback). Then: with a prompt showing, the input line is committed and RUN; while a command
    /// is RUNNING with interactive stdin open, the line is SENT to the child's stdin instead.
    pub fn console_submit(&mut self) -> bool {
        if self.docs.get(self.active).and_then(|d| d.label()) != Some(SHELL_OUTPUT_NAME) {
            return false; // Enter in another document must not touch the Console's input
        }
        let idx = self.shell_output_doc_index();
        let end = self.docs[idx].buffer().len();
        if self.docs[idx].buffer().cursor() != end {
            return false; // editing scrollback, not entering input
        }
        if self.console_showing {
            let input_at = (self.console_prompt_at + self.console_prompt_len).min(end);
            let cmd = String::from_utf8_lossy(&self.docs[idx].buffer().slice_bytes(input_at, end))
                .into_owned();
            // Remember it in the session history (skip blanks and an immediate repeat of the last).
            let trimmed = cmd.trim();
            if !trimmed.is_empty() && self.console_history.last().map(String::as_str) != Some(trimmed) {
                self.console_history.push(trimmed.to_string());
            }
            self.console_hist_pos = None;
            self.console_showing = false; // consume the prompt; the command line stays as echoed text
            self.docs[idx].append_console("\n"); // output starts on a fresh line below the command
            self.console_input_at = self.docs[idx].buffer().len();
            self.console_ansi.reset(); // this command's output starts from the default style
            self.console_run_command(trimmed);
            true
        } else if let Some(tx) = self.console_stdin.clone() {
            // A command is running — feed the typed line to its stdin (never the child's own prompt
            // text, which sits before `console_input_at`).
            let line = String::from_utf8_lossy(
                &self.docs[idx].buffer().slice_bytes(self.console_input_at.min(end), end),
            )
            .into_owned();
            self.docs[idx].append_console("\n"); // echo the newline
            self.console_input_at = self.docs[idx].buffer().len();
            let _ = tx.send(ConsoleStdin::Line(line));
            self.console_dirty = true;
            true
        } else {
            false // no prompt and nothing running → let the GUI insert a newline
        }
    }

    /// Ctrl+Z / Ctrl+D in the running Console: close the child's stdin (EOF), so filters like `sort`
    /// finish. Returns whether there was an interactive command to signal.
    pub fn console_stdin_eof(&mut self) -> bool {
        if let Some(tx) = self.console_stdin.take() {
            let _ = tx.send(ConsoleStdin::Eof);
            let idx = self.shell_output_doc_index();
            self.docs[idx].append_console("\n"); // the child's post-EOF output starts fresh
            self.console_input_at = self.docs[idx].buffer().len();
            self.console_dirty = true;
            return true;
        }
        false
    }

    /// Whether a foreground Console command with interactive stdin is currently running (the GUI gates
    /// its Ctrl+Z/Ctrl+D EOF interception on this, and routes Enter to stdin). False once stdin is
    /// closed (post-EOF) even though the command may still be finishing — see [`console_command_running`].
    pub fn console_job_running(&self) -> bool {
        self.console_stdin.is_some()
    }

    /// Whether a foreground Console command is running (a `JobSink::Console` job is in flight), stdin
    /// open or not. The GUI gates Ctrl+C (interrupt) on this so you can break out even after EOF.
    pub fn console_command_running(&self) -> bool {
        self.jobs.iter().any(|j| matches!(j.sink, JobSink::Console))
    }

    /// Ctrl+C in the running Console: interrupt (kill) the foreground command, like a terminal SIGINT.
    /// The job's completion then clears stdin, prints the exit line, and re-prompts. Returns whether
    /// there was a command to interrupt.
    pub fn console_interrupt(&mut self) -> bool {
        let Some(id) = self.jobs.iter().find(|j| matches!(j.sink, JobSink::Console)).map(|j| j.id)
        else {
            return false;
        };
        self.console_stdin = None; // stop feeding stdin; the pump thread ends
        self.console_ansi.reset(); // the interrupt line isn't tinted by the command's leftover colour
        self.console_emit("^C\n"); // show the interrupt in the transcript
        self.kill_job(id); // the killed child's pipes close → Done handler re-prompts
        true
    }

    /// Shift+Up (`older`) / Shift+Down: recall the session command history into the prompt input,
    /// replacing whatever is typed. Returns `false` — so the GUI keeps the normal Shift+arrow (extend
    /// selection) — unless a prompt is showing on the active Console AND the caret is on the input line
    /// AND there is somewhere to go (older with history; newer while already recalling).
    pub fn console_history_recall(&mut self, older: bool) -> bool {
        if !self.console_showing || self.console_history.is_empty() {
            return false;
        }
        if self.docs.get(self.active).and_then(|d| d.label()) != Some(SHELL_OUTPUT_NAME) {
            return false;
        }
        let idx = self.shell_output_doc_index();
        let input_at =
            (self.console_prompt_at + self.console_prompt_len).min(self.docs[idx].buffer().len());
        if self.docs[idx].buffer().cursor() < input_at {
            return false; // caret up in scrollback → leave Shift+arrow as select
        }
        let last = self.console_history.len() - 1;
        let new_pos = match (self.console_hist_pos, older) {
            (None, true) => Some(last),                   // start at the newest
            (Some(p), true) => Some(p.saturating_sub(1)), // older
            (Some(p), false) if p < last => Some(p + 1),  // newer
            (Some(_), false) => None,                     // past the newest → a fresh, empty line
            (None, false) => return false,                // not recalling; nothing newer
        };
        self.console_hist_pos = new_pos;
        let text = new_pos.map(|p| self.console_history[p].clone()).unwrap_or_default();
        self.docs[idx].console_replace_input(input_at, &text);
        self.console_dirty = true;
        true
    }

    /// Run one Console command: `cd` updates `$CWD` in place (then re-prompts), an untrusted shell-out
    /// is refused, otherwise the command spawns via the job worker in `$CWD` and streams back through
    /// [`JobSink::Console`] (which re-prompts on completion). Reuses the SH-pane semantics.
    fn console_run_command(&mut self, cmd: &str) {
        if cmd.is_empty() {
            self.console_write_prompt();
            return;
        }
        // `cls` / `clear` at the prompt clears the CONSOLE (not the OS screen, which a piped child
        // couldn't do usefully anyway) — the same as the `CLS`/`CLEAR` verb.
        if cmd.eq_ignore_ascii_case("cls") || cmd.eq_ignore_ascii_case("clear") {
            self.console_clear();
            return;
        }
        if let Some(arg) = parse_cd(cmd) {
            match arg {
                None => self.console_emit(&format!("{}\n", self.cwd.display())),
                Some(dir) => {
                    if let Err(e) = self.change_directory(dir) {
                        self.console_emit(&format!("cd: {e}\n"));
                    }
                }
            }
            self.console_write_prompt();
            return;
        }
        // A BARE drive letter (`D:`) at the prompt switches drives the same as at the command line —
        // applied to the Console's `$CWD` so it PERSISTS. (Left to the subshell, a `D:` would only
        // change the child's drive and be lost — the "changing the drive letter doesn't take" bug.)
        // A drive-qualified PATH (`D:\sub`) stays a runnable shell command, matching `cmd.exe`.
        if bare_drive_letter(cmd).is_some() {
            if let Err(e) = self.change_directory(cmd) {
                self.console_emit(&format!("{cmd}: {e}\n"));
            }
            self.console_write_prompt();
            return;
        }
        if !self.shell_trusted {
            self.console_emit("shell-out is disabled; enable it with TRUST 1\n");
            self.console_write_prompt();
            return;
        }
        let id = self.next_job_id;
        self.next_job_id += 1;
        self.last_job_id = id;
        let (tx, rx) = std::sync::mpsc::sync_channel(JOB_CHANNEL_CAP);
        let child = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut command = shell_command(cmd);
        command.current_dir(&self.cwd); // run in $CWD
        // Interactive stdin: keep the child's stdin open, fed by Enter (line) / Ctrl+Z-D (EOF). The
        // sender lives here until the job ends (dropping it, or an explicit EOF, closes the pipe).
        let (stdin_tx, stdin_rx) = std::sync::mpsc::channel();
        self.console_stdin = Some(stdin_tx);
        spawn_shell_job(command, None, Some(stdin_rx), tx, std::sync::Arc::clone(&child));
        self.jobs.push(Job {
            id,
            cmd: cmd.to_string(),
            rx,
            child,
            output: String::new(),
            return_var: None,
            sink: JobSink::Console,
            cleanup: None,
            is_mail: false,
        });
    }

    /// The prompt template (`SHPROMPT`; `$SHPROMPT`; SaveCfg). `None` on the setter restores the default.
    pub fn console_prompt(&self) -> &str {
        &self.console_prompt
    }

    pub fn set_console_prompt(&mut self, tmpl: Option<&str>) {
        self.console_prompt = tmpl.map(str::to_string).unwrap_or_else(|| "$CWD> ".to_string());
    }

    /// Put a `CHECK` report into a dedicated read-only `*Language Check*` tab (replacing any prior
    /// report) and switch to it — so a long report is scrollable/searchable in its own buffer instead
    /// of crammed onto the status line.
    fn show_check_results(&mut self, report: &str) {
        const NAME: &str = "*Language Check*";
        let mut d = Document::new();
        d.set_label(NAME.to_string());
        d.append_log(report);
        let idx = match self.docs.iter().position(|x| x.label() == Some(NAME)) {
            Some(i) => {
                self.docs[i] = d;
                i
            }
            None => {
                self.docs.push(d);
                self.docs.len() - 1
            }
        };
        // (Underlines are per-document now, so switching to the results tab shows its own — none — with
        // no risk of dragging the checked doc's marks along.)
        self.switch_buffer(idx);
    }

    /// Start a whole-document `CHECK` on a BACKGROUND thread (harper over a big doc freezes the UI if
    /// run inline). The GUI shows "Working…" via [`check_in_progress`](Self::check_in_progress) and
    /// opens the results tab when [`poll_check`](Self::poll_check) picks up the result. Supersedes any
    /// prior in-flight check. Without the `lint` feature, opens the "not available" note immediately.
    pub fn request_full_check(&mut self) {
        #[cfg(not(feature = "lint"))]
        {
            self.show_check_results(
                "Language checking is not available in this build (rebuild with --features lint).",
            );
        }
        #[cfg(feature = "lint")]
        {
            let text = self.docs[self.active].buffer().text();
            let (flavor, dialect) = (self.lint_flavor(), self.lint_dialect);
            let (engine, dicts, bin) = (self.spell_engine, self.spell_dicts.clone(), self.spell_bin.clone());
            let learned = self.learned_words.clone();
            let (tx, rx) = std::sync::mpsc::channel::<String>();
            std::thread::spawn(move || {
                let _ = tx.send(build_check_report(&text, flavor, dialect, engine, &dicts, &bin, &learned));
            });
            self.check_pending = Some(rx); // drops any prior receiver → its result is ignored
        }
    }

    /// If a background `CHECK` has finished, open its report in the results tab and return `true`.
    /// Call each frame (the GUI repaints on `true`). A no-op without a check running.
    pub fn poll_check(&mut self) -> bool {
        #[cfg(feature = "lint")]
        {
            if let Some(report) = self.check_pending.as_ref().and_then(|rx| rx.try_recv().ok()) {
                self.check_pending = None;
                self.show_check_results(&report);
                return true;
            }
        }
        false
    }

    /// Whether a background `CHECK` is still running (the GUI shows a "Working…" status).
    pub fn check_in_progress(&self) -> bool {
        #[cfg(feature = "lint")]
        {
            self.check_pending.is_some()
        }
        #[cfg(not(feature = "lint"))]
        {
            false
        }
    }

    /// Whether the live preview pane is shown (`PREVIEW`/`PV`/`$PV`; the GUI renders the right pane).
    pub fn preview_visible(&self) -> bool {
        self.preview_visible
    }

    /// The active document's effective render kind (`DOCTYPE` override, else by extension) — routes
    /// the GUI preview's rasterizer and `TOPDF`.
    pub fn active_render_kind(&self) -> crate::document::RenderKind {
        self.docs[self.active].render_kind()
    }

    /// Whether the active document renders as authored Typst. Narrower than [`Self::active_render_kind`]
    /// and kept separate because it answers a different question — the Save-As format default.
    pub fn active_render_is_typst(&self) -> bool {
        matches!(self.active_render_kind(), crate::document::RenderKind::Typst)
    }

    /// Append a job-launch header to the `*Shell Output*` buffer and, when `SW` is on, switch focus
    /// to it so the user watches the job's output stream live (`X/BG` follows `SW`). With `SW` off
    /// the buffer is created/appended but stays hidden and unfocused — the job runs silently. Used
    /// at job launch only; the per-line streaming in `poll_jobs` uses the plain append so it never
    /// steals focus.
    fn start_shell_output_log(&mut self, header: &str) {
        // The header — and the output streamed after it — lands the caret at the END of the buffer
        // (see `append_shell_output_text`), so the view follows the newest output as it arrives.
        let idx = self.shell_output_doc_index();
        self.append_shell_output_text(header);
        if self.shell_output_visible {
            self.active = idx;
            self.set_focus(Target::Document);
        }
    }

    /// If the active buffer is the `*Shell Output*` tab, move focus to another buffer. Called when
    /// `SW off` hides that tab so the user isn't left focused on a now-invisible tab. Returns
    /// whether that tab WAS the active one (so the caller can restore focus on the next `SW on`).
    fn focus_away_from_shell_output(&mut self) -> bool {
        if self.docs.get(self.active).and_then(|d| d.label()) != Some(SHELL_OUTPUT_NAME) {
            return false;
        }
        if let Some(i) = self.docs.iter().position(|d| d.label() != Some(SHELL_OUTPUT_NAME)) {
            self.active = i;
            self.set_focus(Target::Document);
        }
        true
    }

    /// Terminate a running background job by id (`KILL <id>`). The worker reaps it and `poll_jobs`
    /// finalizes it next frame. Returns whether a job with that id was found.
    pub fn kill_job(&mut self, id: u64) -> bool {
        if let Some(job) = self.jobs.iter().find(|j| j.id == id) {
            if let Ok(mut slot) = job.child.lock() {
                if let Some(c) = slot.as_mut() {
                    let _ = c.kill(); // already-exited → harmless error, ignored
                }
            }
            true
        } else {
            false
        }
    }

    /// Whether a streaming `GREP` search is currently running.
    pub fn grep_in_progress(&self) -> bool {
        self.grep_job.is_some()
    }

    /// Drain the streaming `GREP` worker: append newly-arrived hit batches to the results tab, and on
    /// completion finalize it. Returns `(changed, status)` — `changed` = the results buffer grew this
    /// frame (→ repaint); `status` = a status-line message while a search is active or just finished
    /// (`None` when idle). Called each frame by the GUI.
    pub fn poll_grep(&mut self) -> (bool, Option<String>) {
        use std::sync::atomic::Ordering;
        use std::sync::mpsc::TryRecvError;
        let Some(job) = self.grep_job.take() else {
            return (false, None);
        };
        let doc_id = job.doc;
        // If the results tab was closed, cancel the search and drop the job.
        if !self.docs.iter().any(|d| d.id() == doc_id) {
            job.cancel.store(true, Ordering::Relaxed);
            return (false, None);
        }
        let base = job.base.clone();
        let mut changed = false;
        let mut finished: Option<Result<(usize, bool), String>> = None;
        let mut drained = 0usize;
        loop {
            match job.rx.try_recv() {
                Ok(GrepMsg::Hits(hits)) => {
                    if let Some(d) = self.docs.iter_mut().find(|d| d.id() == doc_id) {
                        d.append_grep_hits(&hits, &base);
                        changed = true;
                    }
                    drained += 1;
                    if drained >= GREP_DRAIN_BATCHES {
                        break; // yield the rest to the next frame so a flood can't stall the UI
                    }
                }
                Ok(GrepMsg::Done { files, capped }) => {
                    finished = Some(Ok((files, capped)));
                    break;
                }
                Ok(GrepMsg::Error(e)) => {
                    finished = Some(Err(e));
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    finished = Some(Ok((0, false))); // worker vanished — treat as done
                    break;
                }
            }
        }
        let n = self.docs.iter().find(|d| d.id() == doc_id).map(|d| d.grep_hit_count()).unwrap_or(0);
        match finished {
            Some(res) => {
                // Done — the job is dropped (not re-stored).
                let msg = match res {
                    Ok((files, capped)) => {
                        if let Some(d) = self.docs.iter_mut().find(|d| d.id() == doc_id) {
                            d.finalize_grep(capped);
                        }
                        if n == 0 {
                            self.tr_msg("msg.grep.none", "GREP: no matches")
                        } else {
                            // Compose from localizable pieces: each count word picks its own singular/
                            // plural form, then drops into the summary frame. A separate capped frame
                            // avoids a leading-space fragment (catalog values are trimmed on load).
                            let matches = self.tr_plural("msg.grep.match", "{n} match", "{n} matches", n);
                            let files = self.tr_plural("msg.grep.file", "{n} file", "{n} files", files);
                            if capped {
                                self.tr_fmt(
                                    "msg.grep.summary_capped",
                                    "GREP: {matches} in {files} (capped at {cap})",
                                    &[("matches", &matches), ("files", &files), ("cap", &GREP_CAP)],
                                )
                            } else {
                                self.tr_fmt(
                                    "msg.grep.summary",
                                    "GREP: {matches} in {files}",
                                    &[("matches", &matches), ("files", &files)],
                                )
                            }
                        }
                    }
                    Err(e) => format!("GREP: {e}"),
                };
                (true, Some(msg))
            }
            None => {
                // Still running — put the job back and report progress.
                self.grep_job = Some(job);
                let matches = self.tr_plural("msg.grep.match", "{n} match", "{n} matches", n);
                (changed, Some(self.tr_fmt("msg.grep.progress", "GREP: {matches} so far…", &[("matches", &matches)])))
            }
        }
    }

    /// Drain background-job messages: stream output lines into the `*Shell Output*` tab as they
    /// arrive, and on completion set `$RC`/`$XO`, finalize the entry, and drop the job. Returns a
    /// status notice per finished job. Called each frame by the GUI (and in tests).
    pub fn poll_jobs(&mut self) -> Vec<String> {
        use std::sync::mpsc::TryRecvError;
        let mut notices = Vec::new();
        let mut i = 0;
        while i < self.jobs.len() {
            // Coalesce this job's available lines into ONE batch (a single rope insert), bounded
            // by a byte budget so a flood can't block the GUI thread; the rest waits for next frame.
            let mut batch = String::new();
            let mut done: Option<i64> = None;
            let mut disconnected = false;
            while batch.len() < JOB_DRAIN_BYTES {
                match self.jobs[i].rx.try_recv() {
                    Ok(JobMsg::Chunk(line)) => batch.push_str(&line),
                    Ok(JobMsg::Done(code)) => {
                        done = Some(code);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
            if !batch.is_empty() {
                self.jobs[i].output.push_str(&batch);
                // Stream live to its sink (a Document-sink job buffers silently → inserted on done).
                match self.jobs[i].sink {
                    JobSink::ShellTab => self.append_shell_output_text(&batch),
                    JobSink::Console => self.console_emit(&batch),
                    #[cfg(feature = "email")]
                    JobSink::MailHeaders | JobSink::MailSearchSave | JobSink::MailAction => {}
                }
            }
            if let Some(code) = done {
                let job = self.jobs.remove(i); // don't advance i (next job shifted in)
                if let Some(p) = &job.cleanup {
                    let _ = std::fs::remove_file(p); // curl is done with the SENDMAIL message file
                }
                // A mail job whose curl exited 0 may still have been REFUSED by the IMAP server (curl
                // only checks the transport). Detect a tagged NO/BAD and elevate `$RC` so scripts and
                // the user see a failure. Non-mail jobs are never scanned (false-positive risk).
                #[cfg(feature = "email")]
                let imap_reject = (job.is_mail && code == 0)
                    .then(|| imap_rejection(&job.output))
                    .flatten();
                #[cfg(feature = "email")]
                let code = if imap_reject.is_some() { IMAP_REJECT_CODE } else { code };
                self.last_rc = code;
                // `/R:<var>` — queue the job's full output for delivery into its parent variable
                // (the editor writes it into the interpreter Env after this drain).
                if let Some(var) = &job.return_var {
                    self.pending_returns.push((var.clone(), job.output.clone()));
                }
                match job.sink {
                    JobSink::ShellTab => {
                        self.append_shell_output_text(&format!("(exit {code})\n\n"));
                    }
                    JobSink::Console => {
                        // The command is done: drop the stdin sender (the pump thread ends), then land
                        // the exit line and re-prompt for the next command.
                        self.console_stdin = None;
                        self.console_ansi.reset(); // don't let a command's trailing colour tint our lines
                        self.console_emit(&format!("(exit {code})\n"));
                        self.console_write_prompt();
                    }
                    #[cfg(feature = "email")]
                    JobSink::MailHeaders => {
                        let listing = format_mail_headers(&job.output, &self.mailbox);
                        self.append_shell_output_text(&listing);
                        self.append_shell_output_text(&format!("(exit {code})\n\n"));
                    }
                    #[cfg(feature = "email")]
                    JobSink::MailSearchSave => {
                        // Phase 1 (count) done → look up the target dir, then download 1..N.
                        let pos = self.mail_save_all.iter().position(|(id, _)| *id == job.id);
                        let dir = pos.map(|i| self.mail_save_all.remove(i).1);
                        let n = max_fetch_seq(&job.output);
                        match dir {
                            Some(dir) if n > 0 => {
                                self.append_shell_output_text(&format!(
                                    "found {n} message(s); downloading…\n"
                                ));
                                let _ = self.save_mail_numbers((1..=n).collect(), dir);
                            }
                            Some(_) => self.append_shell_output_text(
                                "SAVEMAIL all: no messages to save (mailbox empty, or the count \
                                 failed — run `MAILDEBUG on` then `LISTMAIL` to see why).\n\n",
                            ),
                            None => self.append_shell_output_text(&format!(
                                "(exit {code}) SAVEMAIL all: lost the target directory\n\n"
                            )),
                        }
                    }
                    #[cfg(feature = "email")]
                    JobSink::MailAction => {
                        // Ran with `-v` to capture the server's tagged response for the check above,
                        // but keep *Shell Output* clean: show the raw trace only under MAILDEBUG.
                        if self.mail_debug {
                            self.append_shell_output_text(&job.output);
                        }
                        self.append_shell_output_text(&format!("(exit {code})\n\n"));
                    }
                }
                // Surface an IMAP refusal (curl said OK at the transport level, the server said NO).
                #[cfg(feature = "email")]
                if let Some(resp) = imap_reject {
                    self.append_shell_output_text(&format!(
                        "⚠ IMAP command refused by the server (RC {code}): {resp}\n\n"
                    ));
                }
                self.last_output = job.output;
                notices.push(format!("job {} done (exit {code}): {}", job.id, job.cmd));
            } else if disconnected {
                let job = self.jobs.remove(i);
                if let Some(p) = &job.cleanup {
                    let _ = std::fs::remove_file(p);
                }
                notices.push(format!("job {} ended unexpectedly: {}", job.id, job.cmd));
            } else {
                i += 1; // still running (Empty, or hit the byte budget — more next frame)
            }
        }
        notices
    }

    /// `SENDMAIL` — assemble the message from the active document (+ args or a header block) and send
    /// it by shelling out to `curl` as a background job. The password is fed to curl through a config
    /// on stdin (`-K -`), never on the command line; the message goes in a temp file curl uploads,
    /// which `poll_jobs` deletes when the job finishes. Requires `SMTPURL`/`MAILUSER` and a password
    /// (via `MAILPASS` or the `XYSCRIPT_MAILPASS` env var).
    #[cfg(feature = "email")]
    fn send_mail(
        &mut self,
        to: Option<String>,
        subject: Option<String>,
        body_file: Option<String>,
    ) -> VmResult<Value> {
        if self.smtp_url.trim().is_empty() {
            return Err(XplError::trappable(
                "SENDMAIL: no server — set it first, e.g. SMTPURL smtps://smtp.example.com:465",
            ));
        }
        if self.mail_user.trim().is_empty() {
            return Err(XplError::trappable("SENDMAIL: no account — set MAILUSER first"));
        }
        let pass = if !self.mail_pass.is_empty() {
            self.mail_pass.clone()
        } else {
            std::env::var("XYSCRIPT_MAILPASS").unwrap_or_default()
        };
        if pass.is_empty() {
            return Err(XplError::trappable(
                "SENDMAIL: no password — set MAILPASS or the XYSCRIPT_MAILPASS environment variable",
            ));
        }
        let from = if self.mail_from.trim().is_empty() {
            self.mail_user.clone()
        } else {
            self.mail_from.clone()
        };
        // `/F <file>`: read the message text from a file (resolved against $CWD); otherwise use the
        // active document. Either source may carry a header block + body, exactly the same way.
        let doc_text = match &body_file {
            Some(f) => self
                .read_file(f)
                .map_err(|e| XplError::trappable(format!("SENDMAIL/F: {}", e.message())))?,
            None => self.docs[self.active].buffer().text(),
        };
        let id = self.next_job_id;
        // A `Date:` (and `Message-ID:`) header is required for a well-formed message: without Date,
        // filters treat it as spam and some clients can't sort it into the inbox. Use UTC (`+0000`) so
        // we needn't chase the local timezone offset without a date crate.
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let date = rfc5322_utc(secs);
        let domain = from.rsplit('@').next().filter(|d| !d.is_empty()).unwrap_or("localhost");
        let message_id = format!("<{secs}.{id}.{}@{domain}>", std::process::id());
        // Attachments come from two places: files staged with `ATTACH`, plus (in header-block mode)
        // any `Attach:` header lines in the document. Read + base64-encode them all, then clear the
        // staged list below.
        let header_attach: Vec<std::path::PathBuf> = if to.is_none() {
            header_attach_paths(&doc_text).iter().map(|p| self.resolve_path(p)).collect()
        } else {
            Vec::new()
        };
        let mut attachments = Vec::new();
        for path in self.mail_attachments.iter().chain(header_attach.iter()) {
            let bytes = std::fs::read(path).map_err(|e| {
                XplError::trappable(format!("SENDMAIL: cannot read attachment {}: {e}", path.display()))
            })?;
            let filename = path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| "attachment".to_string());
            let mime = guess_mime(&filename);
            attachments.push(MailAttachment { filename, mime, b64: wrap76(&base64_encode(&bytes)) });
        }
        let boundary = format!("=_XyScript_{secs}_{id}_=");
        let (recipients, message) = Self::build_mail_message(
            &from, to, subject, &doc_text, &date, &message_id, &attachments, &boundary,
        )
        .map_err(XplError::trappable)?;
        self.mail_attachments.clear(); // consumed into this message

        // Message → temp file (curl uploads it); cleaned up on job completion. No secrets in it.
        let tmp = std::env::temp_dir().join(format!("xyscript-mail-{id}.eml"));
        if let Err(e) = std::fs::write(&tmp, message.as_bytes()) {
            return Err(XplError::trappable(format!("SENDMAIL: cannot write temp message: {e}")));
        }
        // curl config on stdin (`-K -`): keeps the "user:password" out of the process argv.
        let q = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let up = tmp.to_string_lossy().replace('\\', "/");
        let mut cfg = String::new();
        cfg.push_str(&format!("url = \"{}\"\n", q(self.smtp_url.trim())));
        cfg.push_str(&format!("user = \"{}:{}\"\n", q(self.mail_user.trim()), q(&pass)));
        // Envelope sender is a BARE address; a display-name `MAILFROM` keeps its full form only in the
        // visible `From:` header (built in build_mail_message).
        cfg.push_str(&format!("mail-from = \"{}\"\n", q(&envelope_addr(&from))));
        for r in &recipients {
            cfg.push_str(&format!("mail-rcpt = \"{}\"\n", q(r)));
        }
        cfg.push_str(&format!("upload-file = \"{}\"\n", q(&up)));
        cfg.push_str("ssl-reqd\n");

        self.next_job_id += 1;
        self.last_job_id = id; // `$LJ` — a script can WAIT/J $LJ then check $RC
        let (tx, rx) = std::sync::mpsc::sync_channel(JOB_CHANNEL_CAP);
        let child = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut command = std::process::Command::new("curl");
        // `-v` (MAILDEBUG on) streams the whole SMTP dialogue to *Shell Output*; else `-s` is quiet
        // but `-S` still surfaces errors. `--fail-early` stops + reports on the first error (so a
        // failed step isn't masked by a later success); `--connect-timeout` bounds a stuck connect.
        command.args([
            if self.mail_debug { "-v" } else { "-s" },
            "-S",
            "--fail-early",
            "--connect-timeout",
            "30",
            "-K",
            "-",
        ]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW — no console flash from the GUI
        }
        spawn_shell_job(command, Some(cfg), None, tx, std::sync::Arc::clone(&child));
        self.jobs.push(Job {
            id,
            cmd: format!("curl → send mail ({} recipient(s))", recipients.len()),
            rx,
            child,
            output: String::new(),
            return_var: None,
            sink: JobSink::ShellTab,
            cleanup: Some(tmp),
            is_mail: true,
        });
        self.start_shell_output_log(&format!(
            "sending mail to {} (job {id})…\n",
            recipients.join(", ")
        ));
        Ok(Value::str(format!(
            "sending to {} — job {id}; $RC is the result when it finishes (0 = sent)",
            recipients.join(", ")
        )))
    }

    /// IMAP auth for `LISTMAIL`/`SAVEMAIL`/`DELMAIL`: validate config and return the `user = "u:p"`
    /// config line (password kept off argv) and the full mailbox URL (`imaps://host:993/INBOX`).
    #[cfg(feature = "email")]
    fn imap_setup(&self) -> Result<(String, String), String> {
        if self.imap_url.trim().is_empty() {
            return Err("no server — set IMAPURL first, e.g. IMAPURL imaps://imap.example.com:993".into());
        }
        if self.mail_user.trim().is_empty() {
            return Err("no account — set MAILUSER first".into());
        }
        let pass = if !self.mail_pass.is_empty() {
            self.mail_pass.clone()
        } else {
            std::env::var("XYSCRIPT_MAILPASS").unwrap_or_default()
        };
        if pass.is_empty() {
            return Err("no password — set MAILPASS or the XYSCRIPT_MAILPASS environment variable".into());
        }
        let q = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let user_line = format!("user = \"{}:{}\"", q(self.mail_user.trim()), q(&pass));
        let mbox = self.mailbox.trim();
        let mbox = if mbox.is_empty() { "INBOX" } else { mbox };
        let url = format!("{}/{}", self.imap_url.trim().trim_end_matches('/'), mbox);
        Ok((user_line, url))
    }

    /// Spawn a background `curl` IMAP job (config on stdin, `-v` under MAILDEBUG), streaming to
    /// *Shell Output*, and return its job id. Shared by the three receive verbs.
    #[cfg(feature = "email")]
    fn spawn_mail_curl(
        &mut self,
        cfg: String,
        cmd_label: String,
        header: String,
        verbose: bool,
        sink: JobSink,
    ) -> u64 {
        let id = self.next_job_id;
        self.next_job_id += 1;
        self.last_job_id = id; // `$LJ`
        let (tx, rx) = std::sync::mpsc::sync_channel(JOB_CHANNEL_CAP);
        let child = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut command = std::process::Command::new("curl");
        // `--fail-early`: on a multi-request config (SAVEMAIL's several messages, DELMAIL's STORE then
        // EXPUNGE) fail + report on the FIRST error instead of letting a later success mask it.
        // `--connect-timeout` bounds a stuck connect (matters for the synchronous `/W` path).
        command.args([
            if verbose { "-v" } else { "-s" },
            "-S",
            "--fail-early",
            "--connect-timeout",
            "30",
            "-K",
            "-",
        ]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        spawn_shell_job(command, Some(cfg), None, tx, std::sync::Arc::clone(&child));
        self.jobs.push(Job {
            id,
            cmd: cmd_label,
            rx,
            child,
            output: String::new(),
            return_var: None,
            sink,
            cleanup: None,
            is_mail: true,
        });
        self.start_shell_output_log(&header);
        id
    }

    /// `LISTMAIL` — FETCH each message's Date/From/Subject + flags and stream the raw IMAP response to
    /// *Shell Output* (each block tagged `* N FETCH …` — N is the number to SAVEMAIL/DELMAIL by).
    #[cfg(feature = "email")]
    fn list_mail(&mut self) -> VmResult<Value> {
        let (user_line, url) =
            self.imap_setup().map_err(|e| XplError::trappable(format!("LISTMAIL: {e}")))?;
        let q = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let mut cfg = String::new();
        cfg.push_str(&format!("{user_line}\nssl-reqd\n"));
        cfg.push_str(&format!("url = \"{}\"\n", q(&url)));
        // PEEK so listing headers doesn't mark messages \Seen.
        cfg.push_str(
            "request = \"FETCH 1:* (FLAGS BODY.PEEK[HEADER.FIELDS (DATE FROM SUBJECT)])\"\n",
        );
        let id = self.next_job_id;
        let mbox = self.mailbox.clone();
        // A custom IMAP FETCH surfaces only on curl's protocol stream, so force `-v`. Normally we parse
        // it into a clean listing (MailHeaders sink); under MAILDEBUG we stream the raw trace instead.
        let sink = if self.mail_debug { JobSink::ShellTab } else { JobSink::MailHeaders };
        self.spawn_mail_curl(
            cfg,
            "curl → LISTMAIL (headers)".to_string(),
            format!("fetching headers from {mbox} (job {id})…\n"),
            true,
            sink,
        );
        Ok(Value::str(format!(
            "fetching headers — job {id}; results in *Shell Output* (WAIT, then $RC; 0 = ok)"
        )))
    }

    /// `SAVEMAIL <range> [<path>]` — download each raw message in `range` to `MSG<nnn>.TXT` (named by
    /// message number) in `path` (default `$CWD`, created if missing). `range` = `all` downloads the
    /// whole mailbox (counted first, then saved 1..N).
    #[cfg(feature = "email")]
    fn save_mail(&mut self, range: &str, path: Option<&str>) -> VmResult<Value> {
        let dir = match path {
            Some(p) => self.resolve_path(p),
            None => self.cwd.clone(),
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return Err(XplError::trappable(format!(
                "SAVEMAIL: cannot create {}: {e}",
                dir.display()
            )));
        }
        if range.trim().eq_ignore_ascii_case("all") {
            return self.save_mail_all(dir);
        }
        let nums = parse_range(range).map_err(|e| XplError::trappable(format!("SAVEMAIL: {e}")))?;
        self.save_mail_numbers(nums, dir)
    }

    /// Spawn the actual per-message download of `nums` into `dir` (already resolved + created). Each
    /// message N → `MSG<nnn>.TXT` via curl's `;MAILINDEX=N` URL form (which streams to stdout/file).
    #[cfg(feature = "email")]
    fn save_mail_numbers(&mut self, nums: Vec<u32>, dir: std::path::PathBuf) -> VmResult<Value> {
        let (user_line, url) =
            self.imap_setup().map_err(|e| XplError::trappable(format!("SAVEMAIL: {e}")))?;
        let q = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let mut cfg = String::new();
        cfg.push_str(&format!("{user_line}\nssl-reqd\n"));
        for n in &nums {
            let file = dir.join(format!("MSG{n:03}.TXT"));
            let out = file.to_string_lossy().replace('\\', "/");
            cfg.push_str(&format!("url = \"{};MAILINDEX={n}\"\n", q(&url)));
            cfg.push_str(&format!("output = \"{}\"\n", q(&out)));
        }
        let id = self.next_job_id;
        let (n, dir_disp) = (nums.len(), dir.display().to_string());
        self.spawn_mail_curl(
            cfg,
            format!("curl → SAVEMAIL ({n} message(s))"),
            format!("saving {n} message(s) to {dir_disp} (job {id})…\n"),
            self.mail_debug,
            JobSink::ShellTab,
        );
        Ok(Value::str(format!(
            "saving {n} message(s) to {dir_disp} — job {id} (WAIT, then $RC; 0 = ok)"
        )))
    }

    /// `SAVEMAIL all` — count the mailbox with a FETCH-headers job (the proven `LISTMAIL` path), then
    /// its completion spawns the real download of `1..N` (see `poll_jobs`, `JobSink::MailSearchSave`).
    #[cfg(feature = "email")]
    fn save_mail_all(&mut self, dir: std::path::PathBuf) -> VmResult<Value> {
        let (user_line, url) =
            self.imap_setup().map_err(|e| XplError::trappable(format!("SAVEMAIL: {e}")))?;
        let q = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let mut cfg = String::new();
        cfg.push_str(&format!("{user_line}\nssl-reqd\n"));
        cfg.push_str(&format!("url = \"{}\"\n", q(&url)));
        cfg.push_str(
            "request = \"FETCH 1:* (FLAGS BODY.PEEK[HEADER.FIELDS (DATE FROM SUBJECT)])\"\n",
        );
        let id = self.next_job_id;
        self.spawn_mail_curl(
            cfg,
            "curl → SAVEMAIL all (counting)".to_string(),
            format!("counting messages in {} (job {id})…\n", self.mailbox),
            true, // custom FETCH surfaces only on the -v stream
            JobSink::MailSearchSave,
        );
        self.mail_save_all.push((id, dir));
        Ok(Value::str(format!(
            "counting messages — job {id}; download follows in *Shell Output* \
             (bare WAIT covers both phases, then $RC; 0 = ok)"
        )))
    }

    /// `DELMAIL <range>` — mark the messages in `range` (`1-3,6-8` or `all`) `\Deleted`, then EXPUNGE
    /// (a second curl request via `--next`).
    #[cfg(feature = "email")]
    fn del_mail(&mut self, range: &str) -> VmResult<Value> {
        let seqset =
            range_to_imap_seqset(range).map_err(|e| XplError::trappable(format!("DELMAIL: {e}")))?;
        let (user_line, url) =
            self.imap_setup().map_err(|e| XplError::trappable(format!("DELMAIL: {e}")))?;
        let q = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let (u, store) = (q(&url), q(&format!("STORE {seqset} +FLAGS (\\Deleted)")));
        let mut cfg = String::new();
        cfg.push_str(&format!("{user_line}\nssl-reqd\nurl = \"{u}\"\nrequest = \"{store}\"\n"));
        cfg.push_str(&format!("--next\n{user_line}\nssl-reqd\nurl = \"{u}\"\nrequest = \"EXPUNGE\"\n"));
        let id = self.next_job_id;
        let mbox = self.mailbox.clone();
        // Always `-v` (the `MailAction` sink keeps the trace hidden unless MAILDEBUG) so the server's
        // tagged `OK`/`NO`/`BAD` response is captured — a refused delete otherwise leaves curl at 0.
        self.spawn_mail_curl(
            cfg,
            "curl → DELMAIL (delete + expunge)".to_string(),
            format!("deleting {seqset} from {mbox} (job {id})…\n"),
            true,
            JobSink::MailAction,
        );
        Ok(Value::str(format!(
            "deleting {seqset} — job {id} (WAIT, then $RC; 0 = deleted)"
        )))
    }

    /// The ids of the jobs running right now — snapshotted before a `/W` mail verb spawns its own, so
    /// [`drain_jobs_blocking`](Self::drain_jobs_blocking) can wait for *just* that verb's job(s).
    #[cfg(feature = "email")]
    fn mail_job_ids(&self) -> std::collections::HashSet<u64> {
        self.jobs.iter().map(|j| j.id).collect()
    }

    /// Block (driving [`poll_jobs`](Self::poll_jobs) in a tight loop) until every job spawned at/after
    /// the `before` snapshot has finished — i.e. the `/W` mail verb's job and any follow-on it triggers
    /// (a `SAVEMAIL all` count job spawns its download job in the very `poll_jobs` call that retires the
    /// count, so there's never a gap where none of "our" jobs are present). Reuses the normal completion
    /// path, so `$RC`/`$XO`, the sinks, cleanup, and the IMAP-rejection check all run before we return.
    /// Freezes the editor for the duration — that is what `/W` (synchronous) asks for.
    #[cfg(feature = "email")]
    fn drain_jobs_blocking(&mut self, before: &std::collections::HashSet<u64>) {
        loop {
            let _ = self.poll_jobs();
            if !self.jobs.iter().any(|j| !before.contains(&j.id)) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Shared tail for the mail verbs' `/W` (synchronous) switch. `spawn` is the verb's normal result
    /// (which already launched the background job, or is a config error to propagate). Without `/W` we
    /// just return that async result; with it we block until the job(s) finish and return a completion
    /// message reporting the now-settled `$RC` (`ok_word` labels a `0`). `before` is the job snapshot
    /// taken *before* the verb spawned, so we wait for only its work.
    #[cfg(feature = "email")]
    fn finish_mail(
        &mut self,
        spawn: VmResult<Value>,
        wait: bool,
        before: std::collections::HashSet<u64>,
        label: &str,
        ok_word: &str,
    ) -> VmResult<Value> {
        let async_msg = spawn?; // a config/spawn error propagates unchanged
        if !wait {
            return Ok(async_msg);
        }
        self.drain_jobs_blocking(&before);
        let rc = self.last_rc;
        let outcome = if rc == 0 { ok_word } else { "FAILED" };
        Ok(Value::str(format!("{label}: done — $RC {rc} ({outcome})")))
    }

    /// Build the RFC822 message + recipient list for [`send_mail`](Self::send_mail). With `to_arg`
    /// set, the recipient and subject come from the verb args and the body is the whole document;
    /// otherwise a header block atop the buffer (`To:`/`Cc:`/`Bcc:`/`Subject:`/`From:` lines then a
    /// blank line) supplies them and the body is what follows. Pure (no `self`) so it is unit-tested.
    #[cfg(feature = "email")]
    #[allow(clippy::too_many_arguments)]
    fn build_mail_message(
        from: &str,
        to_arg: Option<String>,
        subject_arg: Option<String>,
        doc_text: &str,
        date: &str,
        message_id: &str,
        attachments: &[MailAttachment],
        boundary: &str,
    ) -> Result<(Vec<String>, String), String> {
        // Split a recipient list on commas, but NOT on a comma inside a quoted display name
        // (`"Public, John" <j@x>`) or inside the angle-bracketed address.
        fn split_addrs(s: &str) -> Vec<String> {
            let (mut out, mut cur) = (Vec::new(), String::new());
            let (mut in_quote, mut in_angle) = (false, false);
            for c in s.chars() {
                match c {
                    '"' => {
                        in_quote = !in_quote;
                        cur.push(c);
                    }
                    '<' => {
                        in_angle = true;
                        cur.push(c);
                    }
                    '>' => {
                        in_angle = false;
                        cur.push(c);
                    }
                    ',' if !in_quote && !in_angle => {
                        let t = cur.trim();
                        if !t.is_empty() {
                            out.push(t.to_string());
                        }
                        cur.clear();
                    }
                    _ => cur.push(c),
                }
            }
            let t = cur.trim();
            if !t.is_empty() {
                out.push(t.to_string());
            }
            out
        }
        let (mut to, mut cc, mut bcc) = (Vec::new(), Vec::new(), Vec::new());
        let mut subject = String::new();
        let mut from = from.trim().to_string();
        // Custom (pass-through) headers from the block, and user-supplied Date/Message-ID overrides.
        let mut custom: Vec<(String, String)> = Vec::new();
        let (mut user_date, mut user_msgid): (Option<String>, Option<String>) = (None, None);
        let body: String;
        if let Some(t) = to_arg {
            to = split_addrs(&t);
            subject = subject_arg.unwrap_or_default();
            body = doc_text.to_string();
        } else {
            // Header block: consume leading `Field: value` lines up to the first blank line (or the
            // first non-header line, which starts the body). Known fields are handled specially; any
            // other valid field is passed through as a custom header (see the guardrails below).
            let mut consumed = 0usize;
            for line in doc_text.split_inclusive('\n') {
                let trimmed = line.trim_end_matches(['\r', '\n']);
                if trimmed.is_empty() {
                    consumed += line.len(); // body starts AFTER the blank separator line
                    break;
                }
                // A non-header line (prose, or a "name" with spaces) → the body starts here.
                let Some((name, val)) = parse_header_field(trimmed) else { break };
                match name.to_ascii_lowercase().as_str() {
                    "to" => to.extend(split_addrs(&val)),
                    "cc" => cc.extend(split_addrs(&val)),
                    "bcc" => bcc.extend(split_addrs(&val)),
                    "subject" => subject = val,
                    "from" => from = val,
                    "attach" | "attachment" => {} // send_mail reads the files separately
                    // Guardrail: the structural MIME headers are computed to match the body/attachments
                    // — never let a user set them (a duplicate would break the message). Dropped.
                    "content-type" | "content-transfer-encoding" | "mime-version" => {}
                    // Guardrail: Date / Message-ID OVERRIDE ours (emitting both would be malformed).
                    "date" => user_date = Some(val),
                    "message-id" => user_msgid = Some(val),
                    // Everything else valid (Reply-To, In-Reply-To, References, X-*, …) passes through.
                    _ => custom.push((name, val)),
                }
                consumed += line.len();
            }
            body = doc_text[consumed..].to_string();
        }
        // Envelope recipients for `--mail-rcpt` are BARE addresses (the display-name form stays only in
        // the visible To:/Cc: headers below).
        let recipients: Vec<String> =
            to.iter().chain(&cc).chain(&bcc).map(|a| envelope_addr(a)).filter(|a| !a.is_empty()).collect();
        if recipients.is_empty() {
            return Err(
                "SENDMAIL: no recipient — pass one as an argument or add a `To:` header line".into(),
            );
        }
        if from.is_empty() {
            return Err("SENDMAIL: no From address — set MAILFROM (or MAILUSER)".into());
        }
        // Visible headers (Bcc is intentionally omitted from the message; it only drives --mail-rcpt).
        let mut msg = String::new();
        msg.push_str(&format!("Date: {}\n", user_date.as_deref().unwrap_or(date)));
        msg.push_str(&format!("From: {from}\n"));
        if !to.is_empty() {
            msg.push_str(&format!("To: {}\n", to.join(", ")));
        }
        if !cc.is_empty() {
            msg.push_str(&format!("Cc: {}\n", cc.join(", ")));
        }
        msg.push_str(&format!("Subject: {subject}\n"));
        msg.push_str(&format!("Message-ID: {}\n", user_msgid.as_deref().unwrap_or(message_id)));
        // Custom pass-through headers from the block, in the order the user wrote them.
        for (name, val) in &custom {
            msg.push_str(&format!("{name}: {val}\n"));
        }
        msg.push_str("X-Mailer: XyScript\n"); // identify the sender software (looks less anonymous)
        msg.push_str("MIME-Version: 1.0\n");
        // Declare the transfer encoding a normal client would: 7bit for pure ASCII, else 8bit.
        let body_cte = if body.is_ascii() { "7bit" } else { "8bit" };
        if attachments.is_empty() {
            // Simple single-part text message.
            msg.push_str("Content-Type: text/plain; charset=utf-8\n");
            msg.push_str(&format!("Content-Transfer-Encoding: {body_cte}\n"));
            msg.push('\n');
            msg.push_str(&body);
        } else {
            // multipart/mixed: the text body first, then each attachment (base64).
            msg.push_str(&format!("Content-Type: multipart/mixed; boundary=\"{boundary}\"\n"));
            msg.push('\n');
            msg.push_str("This is a multi-part message in MIME format.\n");
            msg.push_str(&format!("--{boundary}\n"));
            msg.push_str("Content-Type: text/plain; charset=utf-8\n");
            msg.push_str(&format!("Content-Transfer-Encoding: {body_cte}\n"));
            msg.push('\n');
            msg.push_str(&body);
            msg.push('\n');
            for a in attachments {
                let name = a.filename.replace('"', ""); // keep the quoted param well-formed
                msg.push_str(&format!("--{boundary}\n"));
                msg.push_str(&format!("Content-Type: {}; name=\"{name}\"\n", a.mime));
                msg.push_str("Content-Transfer-Encoding: base64\n");
                msg.push_str(&format!("Content-Disposition: attachment; filename=\"{name}\"\n"));
                msg.push('\n');
                msg.push_str(&a.b64);
                msg.push('\n');
            }
            msg.push_str(&format!("--{boundary}--\n"));
        }
        // SMTP wants CRLF line endings; normalize the whole message.
        let msg = msg.replace("\r\n", "\n").replace('\n', "\r\n");
        Ok((recipients, msg))
    }

    /// The buffer / document editor ops currently target (CMline or active doc).
    fn active(&mut self) -> &mut Document {
        match self.target {
            // During a blind edit (`BEDIT`), the document target is the hidden buffer; the CMline
            // target is unaffected so commands can still be typed/run.
            Target::Document => match &mut self.blind {
                Some(b) => b,
                None => &mut self.docs[self.active],
            },
            Target::CmLine => &mut self.cmline,
        }
    }

    /// Whether a `BEDIT` blind edit is open.
    pub fn blind_active(&self) -> bool {
        self.blind.is_some()
    }

    /// The current `QUIET` level (0 off / 1 hide op-statuses / 2 also hide non-fatal errors).
    pub fn quiet(&self) -> u8 {
        self.quiet
    }

    /// Clear `QUIET` — the editor calls this at the start of each top-level script run, so quiet is
    /// script-scoped (it lasts until the script ends, per spec).
    pub fn reset_quiet(&mut self) {
        self.quiet = 0;
    }

    /// The document file ops (`SAVE`/`SAAS`/`SAD`/`REC`) act on: the blind-edit buffer when one is
    /// open, else the visible active doc. (Unlike `active()`, never the cmline.)
    fn file_doc(&self) -> &Document {
        self.blind.as_ref().unwrap_or(&self.docs[self.active])
    }
    fn file_doc_mut(&mut self) -> &mut Document {
        match &mut self.blind {
            Some(b) => b,
            None => &mut self.docs[self.active],
        }
    }

    /// Exit a blind edit if one is open, DISCARDING the hidden buffer (only an explicit `SA`/`SAVE`
    /// persists it). Returns whether a blind edit was open. Used by `AB` and by the editor's
    /// auto-exit at script end.
    pub fn exit_blind_if_open(&mut self) -> bool {
        self.blind.take().is_some()
    }

    /// Run a parsed `SE`/`CH` request against the active target. Find selects the
    /// match; replace-all edits and records `$CC`. Not-found raises a trappable
    /// error (which trips `$ERROR`). An empty pattern repeats the last search.
    /// All match spans (rope offsets `(start, end)`, in document order) of `find` in the active
    /// document — for the FIND dialog's live "highlight all" as you type. Uses the same `Pattern`
    /// engine as `SE`/Search, so it honors case/whole-word/regex identically; `in_selection` limits
    /// hits to the current selection span (like `/S`). Empty pattern or a bad regex → no hits.
    /// Read-only (never moves the caret). Capped so a pathological pattern can't run away.
    /// The markup-aware harper parser for the active document (Typst via harper-typst, else GFM).
    /// HTML has no harper parser of its own, but Markdown's already skips raw HTML tags and checks
    /// the prose around them — which is exactly what we want for an HTML source buffer.
    #[cfg(feature = "lint")]
    fn lint_flavor(&self) -> crate::lint::Flavor {
        match self.docs[self.active].render_kind() {
            crate::document::RenderKind::Typst => crate::lint::Flavor::Typst,
            // Plain text is pure prose; Markdown's parser checks prose (skipping any markup it sees),
            // which is exactly right for a buffer with no markup at all.
            crate::document::RenderKind::Gfm
            | crate::document::RenderKind::Html
            | crate::document::RenderKind::Text => crate::lint::Flavor::Markdown,
        }
    }

    /// Map harper byte spans (into a region starting at absolute decoded offset `abs_dec_lo`) to
    /// [`LintMark`]s in absolute ROPE coordinates for the active buffer. Shared by the synchronous
    /// path and the async worker's result handler.
    #[cfg(feature = "lint")]
    fn map_lint_spans(
        &self,
        doc: usize,
        abs_dec_lo: usize,
        spans: &[crate::lint::LintSpan],
    ) -> Vec<crate::lint::LintMark> {
        let buf = self.docs[doc].buffer();
        let has_learned = !self.learned_words.is_empty();
        spans
            .iter()
            .filter_map(|(bs, be, kind, message)| {
                let (rs, re) = (buf.rope_offset(abs_dec_lo + bs), buf.rope_offset(abs_dec_lo + be));
                if re <= rs {
                    return None;
                }
                // Honour the user dictionary: drop SPELLING/typo marks on words they've LEARN-ed
                // (stored lowercase; harper matches any capitalization, so compare case-insensitively).
                if has_learned && crate::lint::is_spelling_kind(kind) {
                    let word = buf.slice(rs, re);
                    if self.learned_words.contains(word.trim().to_lowercase().as_str()) {
                        return None;
                    }
                }
                Some(crate::lint::LintMark {
                    start: rs,
                    end: re,
                    kind: kind.clone(),
                    message: message.clone(),
                })
            })
            .collect()
    }

    /// Live-check marks for just the VISIBLE window, computed SYNCHRONOUSLY (used by tests / headless;
    /// the GUI drives the background worker instead). `dec_lo`/`dec_hi` are viewport decoded-byte
    /// offsets; harper runs over that slice grown by a margin, NEVER the whole doc — bounded work.
    #[cfg(feature = "lint")]
    fn compute_lint_marks(&self, doc: usize, dec_lo: usize, dec_hi: usize) -> Vec<crate::lint::LintMark> {
        const MARGIN: usize = 4096;
        let (region_text, abs_dec_lo) = self.docs[doc]
            .buffer()
            .decoded_region(dec_lo.saturating_sub(MARGIN), dec_hi + MARGIN);
        if region_text.is_empty() {
            return Vec::new();
        }
        let spans = match self.spell_engine {
            crate::lint::SpellEngine::Hunspell => {
                let prose = crate::lint::mask_markup(&region_text, self.lint_flavor());
                crate::lint::hunspell_check(&prose, &self.spell_dicts, &self.spell_bin)
            }
            crate::lint::SpellEngine::Harper => {
                crate::lint::LintEngine::new(self.lint_dialect).lint_region(&region_text, self.lint_flavor())
            }
        };
        self.map_lint_spans(doc, abs_dec_lo, &spans)
    }

    /// From `marks`, the rope span of the next (`forward`) issue after `caret`, or the previous one
    /// before it, of the requested kind (spelling vs everything-else) — or `None`.
    #[cfg(feature = "lint")]
    fn pick_lint(
        marks: &[crate::lint::LintMark],
        caret: usize,
        spelling: bool,
        forward: bool,
    ) -> Option<(usize, usize)> {
        let of_kind = |m: &&crate::lint::LintMark| m.is_spelling() == spelling;
        if forward {
            marks
                .iter()
                .filter(|m| of_kind(m) && m.start > caret)
                .min_by_key(|m| m.start)
        } else {
            marks
                .iter()
                .filter(|m| of_kind(m) && m.end <= caret)
                .max_by_key(|m| m.start)
        }
        .map(|m| (m.start, m.end))
    }

    /// Find the next/previous spelling or grammar issue to jump to. Fast path: the already-computed
    /// viewport marks (walking visible issues is instant). Otherwise scan the document in bounded
    /// chunks from the caret (marks only cover the viewport, so going beyond it requires linting more)
    /// — capped so a clean/huge stretch can't freeze the UI; `None` = none found within the cap.
    #[cfg(feature = "lint")]
    fn find_lint_target(&self, spelling: bool, forward: bool) -> Option<(usize, usize)> {
        let caret = self.docs[self.active].buffer().cursor();
        if let Some(t) = Self::pick_lint(self.docs[self.active].lints(), caret, spelling, forward) {
            return Some(t); // an issue already visible in the viewport
        }
        const CHUNK: usize = 256 * 1024; // decoded bytes per scan step
        const MAX_CHUNKS: usize = 8; // ~2 MB scan cap before giving up
        let buf = self.docs[self.active].buffer();
        let caret_dec = buf.decoded_offset(caret);
        let total_dec = buf.decoded_offset(buf.len());
        for i in 0..MAX_CHUNKS {
            let (lo, hi) = if forward {
                let lo = caret_dec.saturating_add(i * CHUNK);
                if lo >= total_dec {
                    break;
                }
                (lo, (lo + CHUNK).min(total_dec))
            } else {
                let hi = caret_dec.saturating_sub(i * CHUNK);
                if hi == 0 {
                    break;
                }
                (hi.saturating_sub(CHUNK), hi)
            };
            let marks = self.compute_lint_marks(self.active, lo, hi);
            if let Some(t) = Self::pick_lint(&marks, caret, spelling, forward) {
                return Some(t);
            }
        }
        None
    }

    /// The correction choices for the issue at the caret (`LINTFIX`), or `None`.
    #[cfg(feature = "lint")]
    fn compute_lint_fix(&self) -> Option<LintFixChoices> {
        self.compute_lint_fix_at(self.docs[self.active].buffer().cursor(), false)
    }

    /// The correction choices for the issue at rope offset `pos`, or `None`. Freshly lints a small
    /// region around `pos` (so it works regardless of the live toggles / nav state), finds the first
    /// issue whose span contains `pos` and has suggestions, and maps it to rope coordinates.
    /// `spelling_only` restricts to spelling issues (for the right-click spell menu).
    #[cfg(feature = "lint")]
    fn compute_lint_fix_at(&self, pos: usize, spelling_only: bool) -> Option<LintFixChoices> {
        let buf = self.docs[self.active].buffer();
        if self.docs[self.active].read_only() {
            return None;
        }
        let pos_dec = buf.decoded_offset(pos);
        const HALF: usize = 8 * 1024;
        let (region_text, abs_dec_lo) =
            buf.decoded_region(pos_dec.saturating_sub(HALF), pos_dec + HALF);
        if region_text.is_empty() {
            return None;
        }
        // Hunspell: suggestions for the caret word via `hunspell -a`. The span comes from OUR
        // tokenizer (word_at), so we never trust `-a`'s per-line offsets. Spelling-only by nature,
        // which is what both callers want (`spelling_only` is moot here).
        if self.spell_engine == crate::lint::SpellEngine::Hunspell {
            // Mask code/URLs so a caret inside them finds no word (consistent with the underlines).
            let prose = crate::lint::mask_markup(&region_text, self.lint_flavor());
            let rel = pos_dec.saturating_sub(abs_dec_lo);
            let (ws, we) = crate::lint::word_at(&prose, rel)?;
            let sugs = crate::lint::hunspell_suggest(&prose[ws..we], &self.spell_dicts, &self.spell_bin);
            if sugs.is_empty() {
                return None;
            }
            let (rs, re) = (buf.rope_offset(abs_dec_lo + ws), buf.rope_offset(abs_dec_lo + we));
            // Hunspell provides no explanation; `lint_explain_at` fills `message` from the mark.
            return Some(LintFixChoices {
                span: (rs, re),
                labels: sugs.clone(),
                replacements: sugs,
                message: String::new(),
                is_spelling: true,
            });
        }
        let fixes = crate::lint::LintEngine::new(self.lint_dialect)
            .lint_region_fixes(&region_text, self.lint_flavor());
        for (bs, be, is_spelling, choices) in &fixes {
            if choices.is_empty() || (spelling_only && !is_spelling) {
                continue;
            }
            let (rs, re) = (buf.rope_offset(abs_dec_lo + bs), buf.rope_offset(abs_dec_lo + be));
            if rs <= pos && pos <= re {
                return Some(LintFixChoices {
                    span: (rs, re),
                    labels: choices.iter().map(|(l, _)| l.clone()).collect(),
                    replacements: choices.iter().map(|(_, r)| r.clone()).collect(),
                    message: String::new(), // the mark carries it; `lint_explain_at` supplies it
                    is_spelling: *is_spelling,
                });
            }
        }
        None
    }

    /// For the right-click popup: the underlined issue at rope offset `pos` — its EXPLANATION and
    /// kind (read from the live mark, which already carries them) plus any correction suggestions
    /// (from the fix path). Works for SPELLING and GRAMMAR underlines alike (Harper's sandbox
    /// explains both). `None` when the click isn't on an underlined span. A no-op without `lint`.
    pub fn lint_explain_at(&self, pos: usize) -> Option<LintFixChoices> {
        #[cfg(feature = "lint")]
        {
            let doc = &self.docs[self.active];
            if doc.read_only() {
                return None;
            }
            let m = doc.lints().iter().find(|m| m.start <= pos && pos <= m.end)?;
            let (mspan, is_spelling, message) = ((m.start, m.end), m.is_spelling(), m.message.clone());
            // Suggestions for this issue (spelling or grammar), if any; else a message-only popup.
            let (span, labels, replacements) = match self.compute_lint_fix_at(pos, false) {
                Some(f) => (f.span, f.labels, f.replacements),
                None => (mspan, Vec::new(), Vec::new()),
            };
            Some(LintFixChoices { span, labels, replacements, message, is_spelling })
        }
        #[cfg(not(feature = "lint"))]
        {
            let _ = pos;
            None
        }
    }

    /// The SPELLING correction choices for the word at rope offset `pos` (for the right-click spell
    /// menu), or `None` if the click isn't on a flagged word. A no-op without the `lint` feature.
    pub fn lint_context_at(&self, pos: usize) -> Option<LintFixChoices> {
        #[cfg(feature = "lint")]
        {
            self.compute_lint_fix_at(pos, true)
        }
        #[cfg(not(feature = "lint"))]
        {
            let _ = pos;
            None
        }
    }

    /// `(active doc index, revision)` — the per-document part of the live-check staleness key.
    fn lint_doc_key(&self) -> (usize, u64) {
        (self.active, self.docs[self.active].buffer().revision())
    }

    /// Live checking runs whenever spelling OR grammar underlines are on.
    pub fn lint_active(&self) -> bool {
        self.spell_on || self.grammar_on
    }

    /// Whether live checking is on AND the active document's *visible region* `(lo, hi)` hasn't been
    /// checked at its current revision (so the GUI knows to start its idle timer). Cheap.
    pub fn lint_stale(&self, lo: usize, hi: usize) -> bool {
        let (a, r) = self.lint_doc_key();
        self.lint_active() && self.lint_key != Some((a, r, lo, hi))
    }

    /// Re-run the checker over the visible byte window `(lo, hi)` — but only if live checking is on
    /// and that region+revision hasn't been checked (so calling it every frame / on save is cheap).
    /// Updates [`lints`](Self::lints). A no-op without the `lint` feature.
    #[cfg_attr(not(feature = "lint"), allow(unused_variables))]
    pub fn recheck_lints(&mut self, lo: usize, hi: usize) {
        #[cfg(feature = "lint")]
        {
            if self.docs[self.active].read_only() {
                return; // scratch/results/log tab — not checked
            }
            let (a, r) = self.lint_doc_key();
            if self.lint_active() && self.lint_key != Some((a, r, lo, hi)) {
                let marks = self.compute_lint_marks(self.active, lo, hi);
                self.docs[self.active].set_lints(marks);
                self.lint_key = Some((a, r, lo, hi));
            }
        }
    }

    /// Ask the BACKGROUND worker to check document `doc`'s visible window `(lo, hi)` — the async path
    /// the GUI uses so harper never blocks the UI thread. `doc` lets the GUI check EACH visible pane's
    /// document in a split (round-robin), not just the focused one. Cheap: extracts only the ~viewport
    /// slice on this thread and hands the snapshot off. Dedups repeat requests for an unchanged view; a
    /// no-op when live checking is off, `doc` is read-only, or the `lint` feature is absent.
    #[cfg_attr(not(feature = "lint"), allow(unused_variables))]
    pub fn request_lint(&mut self, doc: usize, lo: usize, hi: usize) {
        #[cfg(feature = "lint")]
        {
            if !self.lint_active()
                || self.docs.get(doc).map_or(true, |d| d.read_only())
                || self.docs.get(doc).and_then(|d| d.label()) == Some(SHELL_OUTPUT_NAME)
            {
                return; // read-only tab, or the (now-editable) shell console — don't underline output
            }
            let rev = self.docs[doc].buffer().revision();
            if self.lint_done.get(&doc) == Some(&(rev, lo, hi)) {
                return; // this doc's visible window is already checked (and unedited)
            }
            const MARGIN: usize = 4096;
            let (text, abs_dec_lo) = self.docs[doc]
                .buffer()
                .decoded_region(lo.saturating_sub(MARGIN), hi + MARGIN);
            if self.lint_worker.is_none() {
                self.lint_worker = Some(LintWorker::spawn(self.lint_dialect));
            }
            self.lint_req_seq += 1;
            let seq = self.lint_req_seq;
            let job = LintJob {
                seq,
                text,
                flavor: self.lint_flavor(),
                engine: self.spell_engine,
                dicts: self.spell_dicts.clone(),
                bin: self.spell_bin.clone(),
            };
            if self.lint_worker.as_ref().unwrap().tx.send(job).is_ok() {
                self.lint_pending = Some(LintPending { seq, active: doc, rev, abs_dec_lo, lo, hi });
            }
        }
    }

    /// Drain any finished background lint results; apply the one matching the in-flight request IF the
    /// document is unchanged since it was sent (else discard — a fresher request is/will be out).
    /// Returns whether the underlines were updated (the GUI repaints if so). Call every frame.
    pub fn poll_lint(&mut self) -> bool {
        #[cfg(not(feature = "lint"))]
        {
            false
        }
        #[cfg(feature = "lint")]
        {
            let mut applied = false;
            while let Some(done) =
                self.lint_worker.as_ref().and_then(|w| w.rx.try_recv().ok())
            {
                let Some(p) = self.lint_pending.as_ref() else {
                    continue;
                };
                let (pseq, pdoc, prev, pabs, plo, phi) =
                    (p.seq, p.active, p.rev, p.abs_dec_lo, p.lo, p.hi);
                if done.seq != pseq {
                    continue; // a stale result from a superseded request
                }
                // Apply to the DOCUMENT that was checked (which may be an INACTIVE pane's doc in a
                // split — not necessarily the focused one), as long as it hasn't been edited since.
                if self.docs.get(pdoc).map(|d| d.buffer().revision()) == Some(prev) {
                    let marks = self.map_lint_spans(pdoc, pabs, &done.spans);
                    self.docs[pdoc].set_lints(marks);
                    self.lint_done.insert(pdoc, (prev, plo, phi)); // this view is now satisfied
                    applied = true;
                }
                self.lint_pending = None; // resolved (applied, or discarded as out-of-date)
            }
            applied
        }
    }

    /// Whether a background lint request is still out (so the GUI keeps polling for its result).
    pub fn lint_in_flight(&self) -> bool {
        #[cfg(feature = "lint")]
        {
            self.lint_pending.is_some()
        }
        #[cfg(not(feature = "lint"))]
        {
            false
        }
    }

    /// The active document's language-check marks (rope spans) — the GUI draws these as underlines.
    pub fn lints(&self) -> &[crate::lint::LintMark] {
        self.docs[self.active].lints()
    }

    /// Are spelling underlines on? (`$SPELLING`.)
    pub fn spelling_on(&self) -> bool {
        self.spell_on
    }
    /// Are grammar/word-choice underlines on? (`$GRAMMAR`.)
    pub fn grammar_on(&self) -> bool {
        self.grammar_on
    }

    /// Toggle live SPELLING underlines (`SPELLING`/`SP on|off`).
    pub fn set_spelling(&mut self, on: bool) {
        let was = self.lint_active();
        self.spell_on = on;
        self.on_lint_flags_changed(was);
    }
    /// Toggle live GRAMMAR underlines (`GRAMMAR`/`GR on|off`).
    pub fn set_grammar(&mut self, on: bool) {
        let was = self.lint_active();
        self.grammar_on = on;
        self.on_lint_flags_changed(was);
    }

    /// The English dialect harper checks against (`$DIALECT`).
    pub fn dialect(&self) -> crate::lint::Dialect {
        self.lint_dialect
    }

    /// Set the checking dialect (`DIALECT`). The linter is dialect-bound, so this respawns the worker
    /// and forces a re-check; existing (other-dialect) marks are cleared.
    pub fn set_dialect(&mut self, dialect: crate::lint::Dialect) {
        if self.lint_dialect == dialect {
            return;
        }
        self.lint_dialect = dialect;
        self.reset_lint_engine();
    }

    /// The spelling checker (`$`-less; SaveCfg + tests read it). `SPELLENGINE`.
    pub fn spell_engine(&self) -> crate::lint::SpellEngine {
        self.spell_engine
    }
    /// Set the spelling checker (`SPELLENGINE`). Invalidates existing marks + re-checks.
    pub fn set_spell_engine(&mut self, engine: crate::lint::SpellEngine) {
        if self.spell_engine == engine {
            return;
        }
        self.spell_engine = engine;
        self.reset_lint_engine();
    }
    /// The Hunspell dictionaries (`SPELLDICT`, the `-d` value).
    pub fn spell_dicts(&self) -> &str {
        &self.spell_dicts
    }
    pub fn set_spell_dicts(&mut self, dicts: String) {
        if self.spell_dicts == dicts {
            return;
        }
        self.spell_dicts = dicts;
        self.reset_lint_engine();
    }
    /// The Hunspell executable (`SPELLBIN`).
    pub fn spell_bin(&self) -> &str {
        &self.spell_bin
    }
    pub fn set_spell_bin(&mut self, bin: String) {
        if self.spell_bin == bin {
            return;
        }
        self.spell_bin = bin;
        self.reset_lint_engine();
    }

    /// Clear every document's language-check underlines (checking off, or a config change that
    /// invalidates all of them).
    fn clear_all_lints(&mut self) {
        for d in &mut self.docs {
            d.clear_lints();
        }
    }

    /// Discard cached marks + the in-flight/last requests and drop the worker, so the next check
    /// rebuilds the linter with the current dialect + learned words. Called on any config change.
    fn reset_lint_engine(&mut self) {
        self.clear_all_lints();
        self.lint_key = None;
        #[cfg(feature = "lint")]
        {
            self.lint_worker = None; // respawn with the new config on the next request
            self.lint_pending = None;
            self.lint_done.clear();
        }
    }

    /// Add a word to the user dictionary (`LEARN`): rebuilds the linter (so it stops being flagged)
    /// and persists. Returns whether it was newly added.
    pub fn learn_word(&mut self, word: &str) -> bool {
        let w = word.trim().to_lowercase(); // stored lowercase → case-insensitive matching
        if w.is_empty() || !self.learned_words.insert(w) {
            return false;
        }
        self.reset_lint_engine();
        self.save_learned_words();
        true
    }

    /// Remove a word from the user dictionary (`UNLEARN`). Returns whether it was present.
    pub fn unlearn_word(&mut self, word: &str) -> bool {
        if !self.learned_words.remove(&word.trim().to_lowercase()) {
            return false;
        }
        self.reset_lint_engine();
        self.save_learned_words();
        true
    }

    /// Add several words at once (`LEARN` with a list / selection / file). Inserts them all, then
    /// rebuilds the checker and persists ONCE (not per word). Returns `(newly_added, already_known)`.
    pub fn learn_words(&mut self, words: &[String]) -> (usize, usize) {
        let (mut added, mut known) = (0, 0);
        for w in words {
            let lw = w.trim().to_lowercase();
            if lw.is_empty() {
                continue;
            }
            if self.learned_words.insert(lw) {
                added += 1;
            } else {
                known += 1;
            }
        }
        if added > 0 {
            self.reset_lint_engine();
            self.save_learned_words();
        }
        (added, known)
    }

    /// Remove several words at once (`UNLEARN` bulk). One rebuild + save. Returns
    /// `(removed, not_present)`.
    pub fn unlearn_words(&mut self, words: &[String]) -> (usize, usize) {
        let (mut removed, mut absent) = (0, 0);
        for w in words {
            if self.learned_words.remove(&w.trim().to_lowercase()) {
                removed += 1;
            } else {
                absent += 1;
            }
        }
        if removed > 0 {
            self.reset_lint_engine();
            self.save_learned_words();
        }
        (removed, absent)
    }

    /// Path to the user-dictionary file: beside the executable (global, like `startup.xpl`), falling
    /// back to the working dir.
    fn learned_path(&self) -> String {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("xyscript-dict.txt")))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "xyscript-dict.txt".to_string())
    }

    /// Write the learned words (one per line) to the dictionary file. Best-effort (a read-only
    /// location shouldn't crash a `LEARN`).
    fn save_learned_words(&mut self) {
        let path = self.learned_path();
        let mut text =
            String::from("# XyScript learned words — one per line. Managed by LEARN/UNLEARN.\n");
        for w in &self.learned_words {
            text.push_str(w);
            text.push('\n');
        }
        let _ = self.write_file(&path, &text);
    }

    /// Load learned words from the dictionary file (if present) — call at startup. Rebuilds the linter.
    pub fn load_learned_words(&mut self) {
        let path = self.learned_path();
        if let Ok(text) = self.read_file(&path) {
            for line in text.lines() {
                let line = line.trim();
                if !line.is_empty() && !line.starts_with('#') {
                    self.learned_words.insert(line.to_lowercase());
                }
            }
            self.reset_lint_engine();
        }
    }

    /// Reconcile after a spelling/grammar flag change. The worker always produces ALL kinds and the
    /// GUI filters by flag, so toggling a flag while still active is a pure render change (no re-lint).
    /// Only two transitions touch the checker: everything-off (stop + clear) and off→active (kick a
    /// first check).
    fn on_lint_flags_changed(&mut self, was_active: bool) {
        let now_active = self.lint_active();
        if !now_active {
            self.clear_all_lints();
            self.lint_key = None;
            #[cfg(feature = "lint")]
            {
                self.lint_pending = None;
                self.lint_done.clear();
            }
        } else if !was_active {
            // Just turned on → force a check for the current view.
            self.lint_key = None;
            #[cfg(feature = "lint")]
            {
                self.lint_done.clear();
            }
        }
        // else: still active — the marks already cover all kinds; the GUI just draws more/fewer.
    }

    pub fn find_all(
        &self,
        find: &str,
        case: bool,
        whole: bool,
        regex: bool,
        in_selection: bool,
    ) -> Vec<(usize, usize)> {
        if find.is_empty() {
            return Vec::new();
        }
        let pattern = match Pattern::build(find, case, whole, regex, false) {
            Ok(p) => p,
            Err(_) => return Vec::new(), // a half-typed `/R` regex just yields no hits yet
        };
        let buffer = self.active_doc().buffer();
        let scope = if in_selection {
            buffer.selection_span().filter(|&(s, e)| e > s)
        } else {
            None
        };
        let (lo, hi) = scope.unwrap_or((0, buffer.len()));
        const CAP: usize = 5000;
        buffer.find_all(&pattern, lo, hi, CAP)
    }

    /// Maximum matches `MDALL` styles in one pass — a guard against a runaway `/R` on a huge file. The
    /// bulk span-setter handles this many cheaply; report it when the cap is hit.
    pub(crate) const MDALL_CAP: usize = 20_000;

    /// Run a `MatchQuery` (the `MDALL` operand) against the active document, returning the match spans as
    /// ROPE offsets (the same space the MDA span setters use). A malformed regex is a trappable error
    /// (so `MDALL/R (` reports instead of silently doing nothing), matching `EMPH`'s validation.
    fn find_all_query(&self, q: &MatchQuery) -> VmResult<Vec<(usize, usize)>> {
        let pattern = Pattern::build(&q.pattern, q.case, q.whole, q.regex, q.perl)
            .map_err(|e| XplError::trappable(format!("MDALL: {e}")))?;
        let buffer = self.active_doc().buffer();
        let scope = if q.in_selection {
            buffer.selection_span().filter(|&(s, e)| e > s)
        } else {
            None
        };
        let (lo, hi) = scope.unwrap_or((0, buffer.len()));
        Ok(buffer.find_all(&pattern, lo, hi, Self::MDALL_CAP))
    }

    fn do_search(&mut self, mut spec: SearchSpec) -> VmResult<Value> {
        // An empty pattern repeats the last search — carrying its OPTIONS (case/whole/regex), not
        // just the text — so Find Next / F3 (bare `SE`) and Find Prev / Shift+F3 (bare `SEB`) repeat
        // with the same flags; only direction (and replace/verify) comes from the current verb.
        if spec.find.is_empty() {
            if let Some(last) = self.last_spec.clone() {
                spec.find = last.find;
                spec.case_sensitive = last.case_sensitive;
                spec.whole_word = last.whole_word;
                spec.regex = last.regex;
                spec.perl = last.perl;
            } else {
                spec.find = self.last_search.clone();
            }
        }
        let find = spec.find.clone();
        if find.is_empty() {
            return Err(XplError::trappable("no search pattern"));
        }
        self.last_search = find.clone();
        self.last_spec = Some(spec.clone());

        // Compile once (a bad `/R`/`/P` regex is a trappable usage error).
        let pattern =
            Pattern::build(&find, spec.case_sensitive, spec.whole_word, spec.regex, spec.perl)
                .map_err(XplError::trappable)?;

        // Interactive change-verify (`CV`): open a walk; the GUI drives it.
        if spec.verify {
            if spec.scope {
                return Err(XplError::trappable("/S (search in selection) is not supported with CV"));
            }
            let replacement = spec.replace.clone().unwrap_or_default();
            return self.verify_start(pattern, replacement, spec.from_top, &find, spec.count);
        }

        // `/S` limits the action to the current selection; capture its span up front (a missing or
        // empty selection is a usage error). `None` = the usual whole-from-cursor behavior.
        let scope = if spec.scope {
            match self.active().buffer().selection_span() {
                Some((s, e)) if e > s => Some((s, e)),
                _ => return Err(XplError::trappable_code(ErrCode::NothingSelected, "/S: no selection to search within")),
            }
        } else {
            None
        };

        match spec.replace.clone() {
            None => {
                // `/n` (find the nth match): walk the search n times — each step continues from the
                // previous match. Only the first honors `/T`. A failed nth search leaves the caret
                // where it started, like an ordinary not-found search.
                let nth = spec.count.unwrap_or(1);
                let restore = (self.active().buffer().cursor(), self.active().buffer().anchor());
                let mut found = false;
                for i in 0..nth {
                    found = self.active().search_select(
                        &pattern,
                        !spec.backward,
                        spec.from_top && i == 0,
                        scope,
                        spec.extend,
                    );
                    if !found {
                        break;
                    }
                }
                if found {
                    self.reveal_center = true; // scroll an off-screen match to mid-screen
                    Ok(Value::str(""))
                } else {
                    self.active().set_caret(restore.0, restore.1);
                    Err(XplError::trappable_code(ErrCode::SearchNotFound, self.tr_fmt("msg.search.not_found", "not found: {find}", &[("find", &find)])))
                }
            }
            Some(replace) => {
                // `/n` caps the replace at n changes; with a cap, fewer than n (including zero) is NOT
                // an error — only an uncapped `CH` treats zero matches as a trappable failure.
                let n = self
                    .active()
                    .replace_all(&pattern, &replace, spec.from_top, scope, spec.count, spec.keep_case);
                self.change_count = n as i64;
                if n == 0 && spec.count.is_none() {
                    Err(XplError::trappable_code(ErrCode::SearchNotFound, self.tr_fmt("msg.search.not_found", "not found: {find}", &[("find", &find)])))
                } else {
                    // XyWrite-style: the caret stayed put, so report the tally on the status line.
                    let msg = if n == 1 { "1 change".to_string() } else { format!("{n} changes") };
                    Ok(Value::str(msg))
                }
            }
        }
    }

    /// Run a clipboard (`$CB`) operation. Copy/cut/append/prepend require a
    /// selection (trappable error otherwise); paste inserts at the cursor,
    /// replacing any selection. The GUI mirrors `$CB` to/from the OS clipboard.
    fn do_clip(&mut self, kind: ClipKind) -> VmResult<Value> {
        // HEX view: copy/cut/paste operate on RAW BYTES via a dedicated byte clipboard — the text
        // clipboard would lose any non-text bytes. Paste overwrites at the caret (keeps file size
        // stable). `Set` (programmatic) falls through to the normal text path.
        if self.active_doc().hex_view()
            && matches!(kind, ClipKind::Copy | ClipKind::Cut | ClipKind::Paste)
        {
            match kind {
                ClipKind::Copy => {
                    let bytes = {
                        let b = self.active().buffer();
                        b.selection_span().map(|(a, e)| b.slice_bytes(a, e))
                    };
                    let bytes = bytes.ok_or_else(|| XplError::trappable_code(ErrCode::NothingSelected, "no bytes selected"))?;
                    let n = bytes.len();
                    self.hex_clipboard = bytes;
                    return Ok(Value::str(format!("copied {n} byte{}", if n == 1 { "" } else { "s" })));
                }
                ClipKind::Cut => {
                    let removed = self.active().hex_delete_selection();
                    if removed.is_empty() {
                        return Err(XplError::trappable_code(ErrCode::NothingSelected, "no bytes selected"));
                    }
                    let n = removed.len();
                    self.hex_clipboard = removed;
                    return Ok(Value::str(format!("cut {n} byte{}", if n == 1 { "" } else { "s" })));
                }
                ClipKind::Paste => {
                    if self.hex_clipboard.is_empty() {
                        return Ok(Value::str("byte clipboard is empty"));
                    }
                    let bytes = self.hex_clipboard.clone();
                    let n = bytes.len();
                    self.active().hex_paste(&bytes);
                    return Ok(Value::str(format!("pasted {n} byte{}", if n == 1 { "" } else { "s" })));
                }
                _ => unreachable!("guard restricts to Copy/Cut/Paste"),
            }
        }
        // Read the active selection (empty `None` when there isn't one).
        let selection = {
            let b = self.active().buffer();
            b.has_selection().then(|| b.selection_text())
        };
        let need_selection = || XplError::trappable_code(ErrCode::NothingSelected, "no text is selected");
        // With a multi-caret set, gather one selection piece per caret (document order); `""` for a
        // bare caret. Empty unless multi.
        let multi_pieces = |host: &mut Self| -> Vec<String> {
            let b = host.active().buffer();
            if b.caret_count() > 1 {
                b.caret_spans().iter().map(|&(s, e)| b.slice(s, e)).collect()
            } else {
                Vec::new()
            }
        };
        // Each arm yields the status message (surfaced by the VM's `host_op`). `chars` = "N character(s)".
        let chars = |n: usize| format!("{n} character{}", if n == 1 { "" } else { "s" });
        let msg = match kind {
            ClipKind::Set(text) => {
                self.clipboard = text;
                self.clipboard_pieces = vec![self.clipboard.clone()];
                self.clipboard_style = None; // programmatic text carries no styling
                String::new() // programmatic (`CLIP`) — no status message
            }
            ClipKind::Copy => {
                let pieces = multi_pieces(self);
                if pieces.is_empty() {
                    // single caret
                    self.clipboard = selection.ok_or_else(need_selection)?;
                    self.clipboard_pieces = vec![self.clipboard.clone()];
                    self.clipboard_style = self.active().selection_style_clip();
                } else {
                    if pieces.iter().all(|p| p.is_empty()) {
                        return Err(need_selection());
                    }
                    self.clipboard = pieces.join(self.active().line_ending().as_str());
                    self.clipboard_pieces = pieces;
                    self.clipboard_style = None; // per-piece multi-caret styling not carried yet
                }
                format!("copied {}", chars(self.clipboard.chars().count()))
            }
            ClipKind::Cut => {
                let pieces = multi_pieces(self);
                if pieces.is_empty() {
                    self.clipboard = selection.ok_or_else(need_selection)?;
                    self.clipboard_pieces = vec![self.clipboard.clone()];
                    self.clipboard_style = self.active().selection_style_clip(); // BEFORE the delete
                    self.active().delete_selection();
                } else {
                    if pieces.iter().all(|p| p.is_empty()) {
                        return Err(need_selection());
                    }
                    self.clipboard = pieces.join(self.active().line_ending().as_str());
                    self.clipboard_pieces = pieces;
                    self.clipboard_style = None;
                    self.active().delete_all_selections();
                }
                format!("cut {}", chars(self.clipboard.chars().count()))
            }
            ClipKind::Append => {
                self.clipboard.push_str(&selection.ok_or_else(need_selection)?);
                self.clipboard_pieces = vec![self.clipboard.clone()];
                self.clipboard_style = None; // the concatenation no longer maps to one styled run
                "appended the selection to the clipboard".to_string()
            }
            ClipKind::Prepend => {
                self.clipboard =
                    format!("{}{}", selection.ok_or_else(need_selection)?, self.clipboard);
                self.clipboard_pieces = vec![self.clipboard.clone()];
                self.clipboard_style = None;
                "prepended the selection to the clipboard".to_string()
            }
            ClipKind::Paste => {
                let n = self.active().caret_count();
                // Distribute one stored piece per caret only on an exact count match (and >1 piece);
                // otherwise paste the joined clipboard at every caret (the ordinary path, which carries
                // any single-run MDA styling the copy/cut captured).
                let pasted = if self.clipboard_pieces.len() >= 2 && self.clipboard_pieces.len() == n {
                    let pieces = self.clipboard_pieces.clone();
                    let count = pieces.iter().map(|p| p.chars().count()).sum::<usize>();
                    self.active().paste_distributed(&pieces);
                    count
                } else {
                    let text = self.clipboard.clone();
                    let count = text.chars().count();
                    let style = self.clipboard_style.clone();
                    self.active().insert_styled(&text, style.as_ref());
                    count
                };
                format!("pasted {}", chars(pasted))
            }
        };
        Ok(Value::str(msg))
    }

    /// Set the viewport hint (top visible line, visible row count) used by the
    /// viewport motions (`PD`/`PU`/`HM`/`BS`). The GUI updates this each frame.
    pub fn set_viewport(&mut self, top_line: usize, rows: usize) {
        self.viewport = (top_line, rows.max(1));
    }

    /// The GUI publishes the caret's shaped VISUAL-line byte range here each frame (see
    /// [`Self::visual_line_span`]), stamped with the revision it was computed at. `RL` reads it.
    pub fn set_visual_line_span(&mut self, span: Option<(usize, usize)>, rev: u64) {
        self.visual_line_span = span;
        self.visual_line_rev = rev;
    }

    /// The GUI publishes the active window's VISUAL-ROW MAP here each frame: `rows` = rope offsets of
    /// each wrapped row's start plus a trailing sentinel (end of the last row), stamped with the doc
    /// `rev` it was built at. `LU`/`LD` consult it (see [`visual_rows`](Self::visual_rows) field).
    /// An empty `rows` (WRAP off, or no GUI) means "no map" → `LU`/`LD` move logically.
    pub fn set_visual_rows(&mut self, rows: Vec<usize>, rev: u64) {
        self.visual_rows = rows;
        self.visual_rows_rev = rev;
    }

    /// Move the active caret by `delta` logical lines (column-preserving). The GUI
    /// uses this for page motion so it can scroll the view by the same amount.
    pub fn move_active_by_lines(&mut self, delta: i32, extend: bool) {
        let p = self.persistent;
        let d = self.active();
        d.set_persistent(p);
        d.move_lines(delta, extend);
    }

    /// The clipboard (`$CB`) contents — the GUI reads this to mirror to the OS.
    pub fn clipboard(&self) -> &str {
        &self.clipboard
    }

    /// Replace the clipboard (`$CB`) — the GUI uses this to mirror in an external
    /// (OS-clipboard) change. An external change collapses the multi-caret pieces to a single piece,
    /// so a subsequent multi-caret paste inserts the same text at every caret (no stale distribution).
    pub fn set_clipboard(&mut self, text: String) {
        self.clipboard = text;
        self.clipboard_pieces = vec![self.clipboard.clone()];
        self.clipboard_style = None; // external text carries no MDA styling
    }

    /// Begin an interactive change-verify walk: find the first match (cursor→end,
    /// or top→end with `/T`), select it, and arm the session. The GUI then prompts
    /// per match via [`verify_active`](Self::verify_active) /
    /// [`verify_respond`](Self::verify_respond). Not-found is a trappable error.
    fn verify_start(
        &mut self,
        pattern: Pattern,
        replacement: String,
        from_top: bool,
        find: &str,
        limit: Option<usize>,
    ) -> VmResult<Value> {
        self.change_count = 0;
        let doc = &mut self.docs[self.active];
        let origin = doc.buffer().cursor();
        let start = if from_top { 0 } else { origin };
        match doc.buffer().search(&pattern, start, true) {
            Some((ms, me)) => {
                doc.select_range(ms, me);
                self.verify =
                    Some(VerifySession { pattern, replacement, origin, current: (ms, me), limit });
                Ok(Value::str(""))
            }
            None => Err(XplError::trappable_code(ErrCode::SearchNotFound, self.tr_fmt("msg.search.not_found", "not found: {find}", &[("find", &find)]))),
        }
    }

    /// Whether an interactive change-verify walk is awaiting a choice.
    pub fn verify_active(&self) -> bool {
        self.verify.is_some()
    }

    /// The prompt to show while a change-verify walk is active (`None` otherwise).
    pub fn verify_prompt(&self) -> Option<String> {
        self.verify.as_ref().map(|_| {
            "Change? [Y]es  [N]o  [O]ne more  [Q]uit  [Esc] back to start".to_string()
        })
    }

    /// Apply the user's choice at the current change-verify stop and advance (or
    /// end) the walk. No-op when no walk is active.
    pub fn verify_respond(&mut self, choice: VerifyChoice) {
        let Some(mut session) = self.verify.take() else {
            return;
        };
        let (ms, me) = session.current;
        let doc = &mut self.docs[self.active];

        // Yes / One-more replace the current match; No / Quit / Esc don't.
        let after = if matches!(choice, VerifyChoice::Yes | VerifyChoice::OneMore) {
            let matched = doc.buffer().slice(ms, me);
            let text = session.pattern.expand_match(&matched, &session.replacement);
            let new_end = doc.replace_span(ms, me, &text);
            self.change_count += 1;
            // Keep `origin` valid: a replacement before it shifts it.
            let delta = new_end as i64 - me as i64;
            if ms < session.origin {
                session.origin = (session.origin as i64 + delta).max(0) as usize;
            }
            new_end
        } else {
            me // skip — continue scanning past the match
        };

        match choice {
            // One-more / Quit / Esc end the walk.
            VerifyChoice::OneMore => doc.set_caret(after, None),
            VerifyChoice::Quit => doc.set_caret(ms, None),
            VerifyChoice::Esc => doc.set_caret(session.origin.min(doc.buffer().len()), None),
            // Yes / No continue: find the next match and re-arm, else finish. But `CV/n` ends the
            // walk once `n` changes have been made (the Yes that hits the cap is the last).
            VerifyChoice::Yes | VerifyChoice::No => {
                let capped = session.limit.is_some_and(|n| self.change_count as usize >= n);
                match (capped, doc.buffer().search(&session.pattern, after, true)) {
                    (false, Some((ns, ne))) => {
                        doc.select_range(ns, ne);
                        session.current = (ns, ne);
                        self.verify = Some(session);
                    }
                    _ => doc.set_caret(after, None), // walk complete (cap reached or no more matches)
                }
            }
        }
    }

    /// True when the active document is an empty, unmodified, untitled scratch —
    /// a slot worth reusing instead of accumulating empty buffers on open/new.
    fn active_is_empty_scratch(&self) -> bool {
        Self::is_pristine_untitled(&self.docs[self.active])
    }

    /// A blank, unmodified, untitled scratch buffer (never saved, nothing typed, not a DIR listing) —
    /// the disposable "Untitled" tab `UNTITLEDTAB` closes.
    fn is_pristine_untitled(d: &Document) -> bool {
        d.path().is_none() && d.buffer().is_empty() && !d.is_modified() && !d.is_dir_listing()
    }

    /// Whether the auto-save-and-restore session feature is on (`SESSION`).
    pub fn session_restore_enabled(&self) -> bool {
        self.session_restore
    }

    /// A cheap fingerprint of the session's STRUCTURE — the restorable tabs (in order), which one is
    /// active, and the split-pane layout. The GUI compares this each frame so a structural change
    /// (open/close a tab, switch tabs, split/unsplit) triggers an IMMEDIATE snapshot instead of
    /// waiting for the periodic timer. Deliberately excludes buffer content (that's the timer's job),
    /// so typing doesn't churn it.
    pub fn session_structure_signature(&self) -> String {
        let ids: Vec<String> = self
            .docs
            .iter()
            .filter(|d| Self::is_session_restorable(d))
            .map(|d| d.id().0.to_string())
            .collect();
        let active_id = self.docs.get(self.active).map(|d| d.id().0).unwrap_or(0);
        format!("{}|{active_id}|{}", ids.join(","), self.panes.serialize())
    }

    /// Whether `doc` should be part of a saved session: a real editing buffer, not a transient one
    /// (directory listing, the *Shell Output* tab, or a blank scratch that has nothing to restore).
    fn is_session_restorable(d: &Document) -> bool {
        !d.is_dir_listing() && d.label() != Some(SHELL_OUTPUT_NAME) && !Self::is_pristine_untitled(d)
    }

    /// Snapshot the current session (open tabs + their state) to `dir`, Notepad++-style. Dirty named
    /// buffers and untitled buffers keep a SHADOW backup of their bytes (`bak-<id>`); the user's real
    /// files are never written. Clean named buffers record only their path and reopen from disk.
    ///
    /// Safe to call repeatedly on a timer (Phase 2 crash resilience): CHANGED-ONLY — a shadow is only
    /// rewritten when its buffer's revision moved since the last snapshot, the manifest is skipped
    /// when unchanged, and backups for tabs that closed (or stragglers from a previous run) are
    /// garbage-collected. Returns how many tabs were recorded.
    pub fn save_session(&mut self, dir: &Path) -> Result<usize, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("session: {e}"))?;
        let mut manifest = String::from("v4\n");
        let mut count = 0usize;
        let mut live = std::collections::HashSet::new(); // backup filenames still in use this pass
        for (i, doc) in self.docs.iter().enumerate() {
            if !Self::is_session_restorable(doc) {
                continue;
            }
            let untitled = doc.path().is_none();
            let needs_backup = untitled || doc.is_modified();
            let backup = if needs_backup {
                let id = doc.id();
                let name = format!("bak-{}", id.0); // stable per live document
                let rev = doc.buffer().revision();
                // Only (re)write the shadow when the content actually changed since last snapshot.
                if self.session_snapshot_revs.get(&id) != Some(&rev) {
                    std::fs::write(dir.join(&name), doc.save_bytes())
                        .map_err(|e| format!("session: {e}"))?;
                    self.session_snapshot_revs.insert(id, rev);
                }
                live.insert(name.clone());
                name
            } else {
                self.session_snapshot_revs.remove(&doc.id()); // was dirty, now clean → drop its cache
                "-".to_string()
            };
            let kind = if untitled { "unti" } else { "file" };
            let active = if i == self.active { "1" } else { "0" };
            let caret = doc.buffer().cursor();
            let anchor = doc.buffer().anchor().map(|a| a.to_string()).unwrap_or_else(|| "-".to_string());
            // `as_str` is the single source of truth for these names, so a new RenderKind can't be
            // silently dropped from the session manifest.
            let doctype = doc.render_override().map_or("auto", |k| k.as_str());
            let enc = doc.encoding_name();
            // The reference (path or untitled label) is LAST, so it may itself contain tabs safely.
            let reff = if untitled {
                doc.label().unwrap_or("Untitled").to_string()
            } else {
                doc.path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
            };
            // The doc's stable id (field 2) lets RESTORE map the saved pane layout onto the new docs.
            let did = doc.id().0;
            // LIVE MDA spans (v4) — so unsaved styling survives restore; `reff` stays LAST (tab-safe).
            let spans = session_spans_field(doc);
            manifest.push_str(&format!(
                "{kind}\t{did}\t{active}\t{caret}\t{anchor}\t{doctype}\t{enc}\t{backup}\t{spans}\t{reff}\n"
            ));
            count += 1;
        }
        // The pane layout + each pane's scroll offset. Always emitted (even for a single pane) so a
        // lone tab restores to the exact spot it was scrolled to, not just its caret.
        // `panes\t<focused-docid>\t<tree>`, where the tree carries per-pane `ViewState`.
        manifest.push_str(&format!(
            "panes\t{}\t{}\n",
            self.panes.focused_doc().0,
            self.panes.serialize_session()
        ));
        // GC: drop any `bak-*` file no longer referenced (a closed tab, or a straggler from a prior
        // run whose docs had different ids). Cheap — a handful of small files.
        if let Ok(rd) = std::fs::read_dir(dir) {
            for ent in rd.flatten() {
                let fname = ent.file_name();
                let name = fname.to_string_lossy();
                if name.starts_with("bak-") && !live.contains(name.as_ref()) {
                    let _ = std::fs::remove_file(ent.path());
                }
            }
        }
        // Manifest last (so every backup it names already exists); skip the write when unchanged.
        if self.last_session_manifest.as_deref() != Some(manifest.as_str()) {
            std::fs::write(dir.join("manifest.txt"), &manifest).map_err(|e| format!("session: {e}"))?;
            self.last_session_manifest = Some(manifest);
        }
        Ok(count)
    }

    /// Restore a session written by [`save_session`](Self::save_session) from `dir`, appending a tab
    /// per recorded entry and re-selecting the one that was active. A shadow-backed entry comes back
    /// marked MODIFIED (its unsaved content, still pointing at the real path); a clean entry reopens
    /// from disk and is skipped if the file is gone. Returns how many tabs were restored. Best-effort:
    /// a malformed line is skipped rather than aborting the whole restore.
    pub fn restore_session(&mut self, dir: &Path) -> Result<usize, String> {
        let manifest = match std::fs::read_to_string(dir.join("manifest.txt")) {
            Ok(s) => s,
            Err(_) => return Ok(0), // no session to restore
        };
        let mut lines = manifest.lines();
        // v2 = tabs + (split-only) layout without scroll; v3 adds per-pane scroll and always writes the
        // layout line; v4 adds a LIVE MDA-spans field per tab line. All parse the same way here — the
        // scroll tokens are optional per leaf, and the spans field is read only for v4.
        let ver = lines.next();
        if !matches!(ver, Some("v2") | Some("v3") | Some("v4")) {
            return Ok(0); // unknown/foreign format — leave the editor as-is
        }
        let has_spans = ver == Some("v4");
        let mut restored = 0usize;
        let mut active_target: Option<usize> = None;
        // Saved doc id → restored `DocId`, for remapping the split-pane layout below.
        let mut id_map: std::collections::HashMap<u32, crate::panes::DocId> = std::collections::HashMap::new();
        let mut panes_spec: Option<(u32, String)> = None; // (focused saved-id, tree tokens)
        for line in lines {
            // The optional trailing `panes\t<focus>\t<tree>` line describes the split layout.
            if let Some(rest) = line.strip_prefix("panes\t") {
                if let Some((focus, tree)) = rest.split_once('\t') {
                    if let Ok(f) = focus.parse::<u32>() {
                        panes_spec = Some((f, tree.to_string()));
                    }
                }
                continue;
            }
            let mut it = line.splitn(if has_spans { 10 } else { 9 }, '\t');
            let kind = it.next().unwrap_or("");
            let saved_id: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let active = it.next() == Some("1");
            let caret: usize = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let anchor: Option<usize> = match it.next() {
                Some("-") | None => None,
                Some(s) => s.parse().ok(),
            };
            let doctype = it.next().unwrap_or("auto");
            let enc = it.next().unwrap_or("");
            let backup = it.next().unwrap_or("-");
            // v4 inserts the LIVE MDA-spans field here (before `reff`, which stays last / tab-safe).
            let spans_field = if has_spans { it.next().unwrap_or("-").to_string() } else { String::new() };
            let reff = it.next().unwrap_or("").to_string();

            let mut doc = Document::new();
            if backup != "-" {
                // Shadow backup: exact unsaved bytes. Load, then re-mark modified.
                let Ok(bytes) = std::fs::read(dir.join(backup)) else { continue };
                doc.load(bytes, PathBuf::from(&reff));
                if kind == "unti" {
                    doc.forget_path();
                    doc.set_label(reff.clone());
                }
                doc.mark_modified();
            } else {
                // Clean named file: reopen from disk. Gone since exit → skip its tab.
                let Ok(bytes) = self.read_doc_bytes(&reff) else { continue };
                doc.load(bytes, PathBuf::from(&reff));
            }
            // Re-apply the recorded encoding (in case auto-detect on load differed).
            if let Some(cs) = crate::encoding::by_name(enc) {
                doc.set_encoding(cs);
            }
            doc.set_render_override(match doctype {
                "gfm" => Some(crate::document::RenderKind::Gfm),
                "typst" => Some(crate::document::RenderKind::Typst),
                "html" => Some(crate::document::RenderKind::Html),
                "text" => Some(crate::document::RenderKind::Text),
                _ => None,
            });
            let len = doc.buffer().len();
            doc.set_caret(caret.min(len), anchor.map(|a| a.min(len)));
            doc.share_grammars(self.highlight_grammars.clone());
            // Restore MDA styling. v4 sessions carry the doc's LIVE spans in the manifest — apply them
            // authoritatively (this preserves UNSAVED styling, and an empty field correctly restores a
            // doc whose styling was cleared but not saved; the sidecar is NOT consulted, since it would
            // re-add the last-saved styling). Older sessions (v2/v3) have no spans, so fall back to the
            // on-disk `.xymeta` sidecar — the same restore the normal open path does. Untitled → skip.
            if has_spans {
                let len = doc.buffer().len();
                let (fg, bg, attrs) = parse_session_spans(&spans_field, len);
                doc.set_color_spans_raw(fg, bg, attrs);
            }
            // Bookmarks aren't carried in the session manifest, so restore them from the sidecar (for
            // ANY session version). For older sessions without live spans, also take the sidecar's
            // colours (guard-gated). `set_marks_restored` relocates the marks against the current text.
            if !self.virtual_fs {
                if let Some(p) = doc.path().map(|p| p.to_path_buf()) {
                    let bytes = doc.save_bytes();
                    if let Some(r) = xymeta_read(&p, &bytes) {
                        if !has_spans && r.guard_ok {
                            doc.set_color_spans_raw(r.fg, r.bg, r.attrs);
                        }
                        doc.set_marks_restored(r.marks);
                    }
                }
            }
            let new_id = doc.id();
            id_map.insert(saved_id, new_id);
            self.docs.push(doc);
            if active {
                active_target = Some(self.docs.len() - 1);
            }
            restored += 1;
        }
        if restored > 0 {
            if let Some(a) = active_target {
                self.active = a;
            }
            // Restore the split-pane layout if one was saved and every pane maps to a restored doc
            // (a pane that had shown a transient doc — a listing / shell output — won't map, so we
            // fall back to a single pane). Otherwise repoint the single pane at the active tab.
            let tree = panes_spec.as_ref().and_then(|(focus, tokens)| {
                let t = crate::panes::PaneTree::from_serialized(tokens, &|d: crate::panes::DocId| {
                    id_map.get(&d.0).copied()
                })?;
                Some((t, *focus))
            });
            if let Some((mut tree, focus)) = tree {
                if let Some(&f) = id_map.get(&focus) {
                    tree.focus_doc_if_shown(f);
                    // Keep the active TAB in step with the focused pane.
                    if let Some(i) = self.docs.iter().position(|d| d.id() == f) {
                        self.active = i;
                    }
                }
                self.panes = tree;
            } else {
                // Repoint the single pane at the restored active tab FIRST, so the blank scratch the
                // editor started with is no longer shown in a pane — otherwise `prune_untitled_tabs`
                // spares it (it won't remove a doc a pane is displaying).
                self.sync_panes();
            }
            self.prune_untitled_tabs();
            self.auto_highlight_all(); // light EVERY restored tab, not just the focused one
        }
        Ok(restored)
    }

    /// Append `doc` as a NEW active tab (no reuse). Shares the grammar registry and repoints the
    /// focused pane, like [`add_or_reuse`](Self::add_or_reuse)'s push path.
    fn add_doc_new(&mut self, mut doc: Document) {
        doc.share_grammars(self.highlight_grammars.clone());
        // Opening a new tab LEAVES the current one — record it as the last-visited tab so `CLOSEBACK`
        // returns here when the new tab is closed (e.g. open a file from a DIR listing, then `AB`).
        self.focus_prev = self.docs.get(self.active).map(|d| d.id());
        self.docs.push(doc);
        self.active = self.docs.len() - 1;
        self.auto_highlight_active();
        self.sync_panes();
    }

    /// Add `doc` as a buffer and make it active — reusing the active slot if it
    /// is an empty scratch (so opening a file from a blank start doesn't leave a
    /// junk empty buffer behind).
    fn add_or_reuse(&mut self, mut doc: Document) {
        if self.active_is_empty_scratch() {
            // Every editor document shares the host's grammar registry so it can resolve its own
            // highlighter by extension (on open/reload).
            doc.share_grammars(self.highlight_grammars.clone());
            self.docs[self.active] = doc;
            self.auto_highlight_active();
            self.sync_panes(); // the focused pane shows the newly opened/replaced document
        } else {
            self.add_doc_new(doc);
        }
    }

    /// Add an OPENED document, honouring `UNTITLEDTAB`. On (default): reuse the blank scratch slot and
    /// then prune any leftover pristine untitled tabs, so no scratch lingers beside the file. Off: keep
    /// the untitled tab — the file opens as its own new tab.
    fn add_opened_doc(&mut self, doc: Document) {
        if self.untitled_auto_close {
            self.add_or_reuse(doc);
            self.prune_untitled_tabs();
        } else {
            self.add_doc_new(doc);
        }
    }

    /// With `UNTITLEDTAB` on, drop every pristine untitled tab that isn't the active document and isn't
    /// shown in a split pane — as long as at least one real document remains (never leaves zero tabs).
    /// Documents are addressed by their stable `DocId`, so removing from the middle of `docs` can't
    /// point `active`/`alt_active` at the wrong buffer.
    fn prune_untitled_tabs(&mut self) {
        if !self.untitled_auto_close {
            return;
        }
        if !self.docs.iter().any(|d| !Self::is_pristine_untitled(d)) {
            return; // nothing real open — keep the scratch
        }
        let keep = self.docs[self.active].id();
        let alt = self.alt_active.and_then(|i| self.docs.get(i)).map(|d| d.id());
        let remove: Vec<crate::panes::DocId> = self
            .docs
            .iter()
            .filter(|d| {
                Self::is_pristine_untitled(d) && d.id() != keep && !self.panes.contains_doc(d.id())
            })
            .map(|d| d.id())
            .collect();
        if remove.is_empty() {
            return;
        }
        self.docs.retain(|d| !remove.contains(&d.id()));
        self.active = self.docs.iter().position(|d| d.id() == keep).unwrap_or(0);
        self.alt_active = alt.and_then(|id| self.docs.iter().position(|d| d.id() == id));
        self.sync_panes();
    }

    /// The `DIRSET` default switches as a command-style string (`/OD/R`), or empty when there are
    /// none. Used by `$DIRSET` and `SAVECFG`.
    fn dir_defaults_string(&self) -> String {
        if self.dir_defaults.is_empty() {
            String::new()
        } else {
            format!("/{}", self.dir_defaults.join("/"))
        }
    }

    /// Same, but a friendly `(none)` for the empty case — for the status line after `DIRSET`.
    fn dir_defaults_display(&self) -> String {
        let s = self.dir_defaults_string();
        if s.is_empty() {
            "(none)".to_string()
        } else {
            s
        }
    }

    /// The index of an open tab already showing `path` (resolved against `$CWD`), if any — for the
    /// `OPEN`/`READ` "already open?" prompt.
    fn find_open_tab(&self, path: &str) -> Option<usize> {
        let target: PathBuf =
            if self.virtual_fs { PathBuf::from(path) } else { self.resolve_path(path) };
        self.docs.iter().position(|d| d.path() == Some(target.as_path()))
    }

    /// A file was renamed on disk from `old` to `new`. If it is open in a tab, repoint that tab's
    /// document at the new path so its label (the file name) follows the rename, and re-run
    /// auto-highlight in case the new extension binds a different grammar. Exact-path match, mirroring
    /// `find_open_tab`. A no-op when the file isn't open. (`RENAME` doesn't touch the buffer's text or
    /// modified flag, so nothing else about the tab changes.)
    fn retarget_open_tab(&mut self, old: &Path, new: PathBuf) {
        if let Some(idx) = self.docs.iter().position(|d| d.path() == Some(old)) {
            self.docs[idx].set_path(new);
            if self.hl_auto {
                // The new extension may bind a different grammar (or none); re-resolve it.
                self.docs[idx].set_highlight(true);
            }
        }
    }

    /// Load `path` into a new tab (the shared body of `OPEN`/`READ`, with the already-open check
    /// already done). `read_only` locks the tab and makes a missing file an error (vs `OPEN`, which
    /// starts a new buffer for a missing file).
    fn open_doc(&mut self, path: &str, read_only: bool, hex: bool) -> VmResult<Value> {
        let target: PathBuf =
            if self.virtual_fs { PathBuf::from(path) } else { self.resolve_path(path) };
        let key = target.to_string_lossy().into_owned();
        let exists = if self.virtual_fs { self.files.contains_key(&key) } else { target.exists() };
        let mut doc = Document::new();
        let msg = if exists {
            let bytes = self.read_doc_bytes(&key)?; // genuine read errors stay trappable
            // A `.xyb` bundle opens transparently: load the packed document + its styling IN MEMORY
            // (nothing is written to disk until SAVE), named for the real file inside it.
            if !self.virtual_fs && !hex && crate::bundle::is_bundle(&bytes) {
                return self.open_bundle_in_memory(&target, bytes, read_only);
            }
            // Restore MDA/FGCOLOR block colours from the hidden `.xymeta` sidecar (guard read BEFORE
            // `load` consumes the bytes; applied after). Ignored on a guard mismatch / no sidecar.
            let restore = (!self.virtual_fs).then(|| xymeta_read(&target, &bytes)).flatten();
            doc.load(bytes, target.clone());
            if let Some(r) = restore {
                apply_xymeta_restore(&mut doc, r);
                }
            doc.set_read_only(read_only);
            let name = doc.file_name().unwrap_or_else(|| key.clone());
            if read_only {
                format!(
                    "opened {name} read-only ({} lines, {}) — RO to unlock",
                    doc.buffer().line_count(),
                    doc.encoding_name()
                )
            } else {
                format!("opened {name} ({} lines, {})", doc.buffer().line_count(), doc.encoding_name())
            }
        } else if read_only {
            return Err(XplError::trappable_code(ErrCode::FileNotFound, format!("READ: {path}: not found")));
        } else {
            // OPEN of a missing file: start a new, empty buffer named for it (ready to SAVE).
            doc.set_path(target.clone());
            let name = doc.file_name().unwrap_or_else(|| key.clone());
            format!("new file {name}")
        };
        doc.set_hex_view(hex); // `/HEX` — open straight into the hex + text view
        self.add_opened_doc(doc);
        // Baseline the AUTOREOPEN mtime NOW (at open), not lazily on the first poll — otherwise an
        // external save landing in the up-to-1s gap before that poll is absorbed into the baseline and
        // never triggers a reload (the reported "the other DUPVIEW/W window didn't reopen").
        self.refresh_disk_mtime(self.active);
        // Leave the caret IN the opened text (not the command line), so the user is editing the
        // file immediately — even when OPEN/READ was run from the cmline. Matches DIR's behavior.
        self.set_focus(Target::Document);
        Ok(Value::str(msg))
    }

    /// Open a `.xyb` bundle transparently: unpack it and load the document + its styling straight into a
    /// buffer, named for the real file inside the bundle (placed in the bundle's own directory). Nothing
    /// is written to disk — the buffer is marked MODIFIED so a later `SAVE` materializes the real file and
    /// its `.xymeta` sidecar. The styling guard holds by construction (doc + meta were packed together).
    fn open_bundle_in_memory(&mut self, bundle_path: &Path, bytes: Vec<u8>, read_only: bool) -> VmResult<Value> {
        let b = crate::bundle::unpack(&bytes).map_err(|e| {
            XplError::trappable(format!("OPEN: {} is not a valid bundle ({e})", bundle_path.display()))
        })?;
        let dest = bundle_path.parent().map(|d| d.join(&b.name)).unwrap_or_else(|| PathBuf::from(&b.name));
        // Collision: a tab for the SAME file is already open (possibly a newer version). Don't risk
        // the bundle's snapshot clobbering it — open it as a DETACHED read-only view (no backing path,
        // labelled "<name> (bundle)"), so it can be read/DIFFed but never saved over the open file.
        let collide = self.find_open_tab(&dest.to_string_lossy()).is_some();
        // Parse the styling (borrows the doc bytes) BEFORE they are moved into the buffer.
        let restore = (!b.meta.is_empty())
            .then(|| std::str::from_utf8(&b.meta).ok().and_then(|t| xymeta_parse(t, &b.doc)))
            .flatten();
        let bare = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| b.name.clone());
        let mut doc = Document::new();
        doc.load(b.doc, dest.clone());
        if let Some(r) = restore {
            apply_xymeta_restore(&mut doc, r);
        }
        let (lines, enc) = (doc.buffer().line_count(), doc.encoding_name().to_string());
        let msg = if collide {
            doc.forget_path(); // detach — SAVE has nothing to overwrite; SAAS <path> to adopt
            doc.set_label(format!("{bare} (bundle)"));
            doc.set_read_only(true);
            format!("{bare} is already open — opened the bundle's copy read-only (SAAS to adopt, or DIFF {bare} to compare)")
        } else {
            doc.mark_modified(); // not yet on disk under its real name → SAVE writes it (+ its sidecar)
            doc.set_read_only(read_only);
            format!("opened {bare} from bundle ({lines} lines, {enc}) — SAVE to write it out")
        };
        self.add_opened_doc(doc);
        self.set_focus(Target::Document);
        Ok(Value::str(msg))
    }

    /// Open a STARTUP file argument (a command-line positional). If the file is already open — because
    /// `SESSION` restored it, or (for a `DUPVIEW/W` child) because it's the very file the parent had
    /// open and the session brought back — SWITCH to that tab instead of arming the interactive
    /// "already open?" prompt. At boot that prompt is spurious (the named file IS the restored one) and
    /// blocks startup waiting for a keypress. Interactive `OPEN` still prompts as before.
    pub fn open_startup_file(&mut self, path: &str) -> VmResult<Value> {
        if path.trim().is_empty() {
            return Err(XplError::trappable("OPEN: a path is required"));
        }
        if let Some(existing) = self.find_open_tab(path) {
            let i = existing.min(self.docs.len().saturating_sub(1));
            self.set_active(i);
            return Ok(Value::str("switched to the open copy"));
        }
        self.open_doc(path, false, false)
    }

    /// The "file is already open" prompt text, if armed (`OPEN`/`READ` of an open file). Surfaced
    /// on the status line; answered with [`resolve_open`](Self::resolve_open).
    pub fn open_prompt(&self) -> Option<String> {
        self.pending_open.as_ref().map(|p| {
            let name = std::path::Path::new(&p.path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.path.clone());
            format!("{name} is already open — open again?  (Y)es  (N)o  (G)o  (R)ead")
        })
    }

    /// Resolve the "already open" prompt: (Y)es opens again, (G)o switches to the open tab, (R)ead
    /// opens again read-only, (N)o cancels.
    pub fn resolve_open(&mut self, ans: OpenAnswer) -> VmResult<Value> {
        let Some(p) = self.pending_open.take() else { return Ok(Value::str("")) };
        match ans {
            OpenAnswer::No => Ok(Value::str("open cancelled")),
            OpenAnswer::Go => {
                let i = p.existing.min(self.docs.len().saturating_sub(1));
                self.set_active(i);
                Ok(Value::str("switched to the open copy"))
            }
            OpenAnswer::Yes => self.open_doc(&p.path, p.read_only, p.hex),
            OpenAnswer::Read => self.open_doc(&p.path, true, p.hex),
        }
    }

    /// When `HLAUTO` is on, enable highlighting for the active document so a recognized file type
    /// (a grammar's extension, or Markdown) colours on open. A no-op when off, and harmless when no
    /// highlighter matches (the document just stays a quiet screen).
    fn auto_highlight_active(&mut self) {
        if self.hl_auto {
            self.docs[self.active].set_highlight(true);
        }
    }

    /// Like [`auto_highlight_active`](Self::auto_highlight_active) but for EVERY open document — used
    /// after a session restore, which reopens many tabs at once. Each document resolves its own
    /// highlighter from its own path, so lighting only the focused tab (as `auto_highlight_active`
    /// did) left the rest colourless until they were manually reopened. A pathless/transient buffer
    /// (scratch, listing, shell output) resolves to no highlighter, so this is harmless for it.
    fn auto_highlight_all(&mut self) {
        if self.hl_auto {
            for doc in &mut self.docs {
                doc.set_highlight(true);
            }
        }
    }

    /// Resolve `path` against the working directory: absolute paths pass through,
    /// relative ones join onto `cwd`; empty resolves to the cwd itself. A Windows
    /// drive-qualified path (`D:`, `D:rel`) always switches to that drive (so `CD` acts
    /// like `CD/D`); a single in-process `cwd` means a drive-relative path roots on the
    /// drive rather than that drive's own remembered directory.
    /// After a file is deleted, drop its row from every open in-tab `DIR`/`LS` listing that shows it
    /// (matched by absolute path). Each listing re-renders and keeps its caret on the row, so the
    /// entry that followed the deleted one is now selected — natural after a point-and-shoot delete.
    fn refresh_listings_after_delete(&mut self, deleted: &Path) {
        for doc in &mut self.docs {
            doc.listing_remove_path(deleted);
        }
    }

    /// After a file is renamed, update its entry in every open in-tab listing (matched by the old
    /// absolute path) — the row shows the new name and re-sorts into place, with the caret following.
    fn refresh_listings_after_rename(&mut self, from: &Path, to: &Path) {
        for doc in &mut self.docs {
            doc.listing_rename_path(from, to);
        }
    }

    fn resolve_path(&self, path: &str) -> PathBuf {
        let p = path.trim();
        if p.is_empty() {
            return self.cwd.clone();
        }
        if let Some(drive_path) = normalize_drive_path(p) {
            return drive_path;
        }
        let pb = Path::new(p);
        if pb.is_absolute() {
            pb.to_path_buf()
        } else {
            self.cwd.join(pb)
        }
    }

    /// Read a file's RAW bytes (for the charset-aware `READFILE(path, charset)` path). In the virtual
    /// test FS a stored entry is a byte-mirror (each `char` is one byte 0–255, as [`Self::write_bytes`]
    /// stores it); on real disk it's `std::fs::read`. Missing/unreadable is a trappable error.
    fn read_bytes(&self, path: &str) -> VmResult<Vec<u8>> {
        if let Some(text) = self.files.get(path) {
            return Ok(text.chars().map(|c| c as u32 as u8).collect());
        }
        if self.virtual_fs {
            return Err(XplError::trappable_code(
                ErrCode::FileNotFound,
                self.tr_fmt("msg.open.not_found", "file not found: {path}", &[("path", &path)]),
            ));
        }
        let resolved = self.resolve_path(path);
        cloud_only_guard(&resolved)?;
        std::fs::read(&resolved)
            .map_err(|e| XplError::trappable(format!("cannot read {}: {e}", resolved.display())))
    }

    /// Write RAW bytes atomically (for the charset-aware `WRITEFILE` path). The virtual test FS stores
    /// them as a byte-mirror string (recoverable by [`Self::read_bytes`]); real disk uses the same
    /// temp-file + rename as [`Host::write_file`] so a failed write can't corrupt the target.
    fn write_bytes(&mut self, path: &str, bytes: &[u8]) -> VmResult<()> {
        if self.virtual_fs {
            let mirror: String = bytes.iter().map(|&b| b as char).collect();
            self.files.insert(path.to_string(), mirror);
            return Ok(());
        }
        let target = self.resolve_path(path);
        let mut tmp_os = target.as_os_str().to_os_string();
        tmp_os.push(".xyscript-tmp");
        let tmp = PathBuf::from(tmp_os);
        std::fs::write(&tmp, bytes)
            .map_err(|e| XplError::trappable(format!("cannot write {}: {e}", target.display())))?;
        std::fs::rename(&tmp, &target).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            XplError::trappable(format!("cannot finalize {}: {e}", target.display()))
        })
    }

    /// Change `$CWD` to `path`, matching the `CD` verb exactly: a BARE drive letter (`D:`) goes to
    /// that drive's REMEMBERED directory (per-drive, `cmd.exe`-style), any other path resolves against
    /// `$CWD`; `.`/`..` are resolved lexically (no `canonicalize`, so a `subst`ed drive is preserved),
    /// the target is verified to be a directory, and per-drive last-dir memory is kept. Returns the new
    /// `$CWD` display string on success, or a short error message. Shared by the `CD` verb and the
    /// Console prompt so `D:` / `cd …` behave identically in both.
    fn change_directory(&mut self, path: &str) -> Result<String, String> {
        let target = match bare_drive_letter(path) {
            Some(d) => self.drive_current_dir(d),
            None => self.resolve_path(path),
        };
        let canon = lexical_abs(&self.cwd, &target);
        if !canon.is_dir() {
            return Err(format!("not a directory: {}", canon.display()));
        }
        self.remember_current_drive(); // save the drive we're leaving
        self.cwd = canon;
        self.remember_current_drive(); // and keep the drive we've landed on current
        Ok(self.cwd.display().to_string())
    }

    /// The directory that roots the active document's Typst file resolution (`#image`/`#include`/
    /// `#read` in the preview + `TOPDF`): the document's own directory when it's saved, else `$CWD`.
    pub fn typst_doc_dir(&self) -> PathBuf {
        self.docs[self.active]
            .path()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.cwd.clone())
    }

    /// Build the MDA styling overlay for the active document's preview/PDF render. `source` is the
    /// text being rendered (`buffer().text()`); the spans are taken in DECODED offsets so they line up
    /// with `source` for any encoding. Reverse video uses the document's current FGCOLOR/BGCOLOR.
    pub fn mda_styler(&self, source: &str) -> crate::gfm::Styler {
        let doc = &self.docs[self.active];
        let mut fg = doc.color_spans_decoded();
        let mut bg = doc.bg_color_spans_decoded();
        // With `PVMARKS` on, fold the EMPH + URL highlight spans in AHEAD of the MDA spans, so the
        // Styler's first-match `cover()` favours a mark where it overlaps MDA — the preview/printout
        // then shows the same emphasis / link colouring as the editor. `PVMARKS` off = MDA only.
        if self.preview_marks {
            let (mut mfg, mut mbg) = self.mark_spans(source);
            mfg.append(&mut fg);
            fg = mfg;
            mbg.append(&mut bg);
            bg = mbg;
        }
        crate::gfm::Styler::new(
            source,
            fg,
            bg,
            doc.attr_spans_decoded(),
            self.fg_color,
            self.bg_color,
            self.ul_spaces,
        )
    }

    /// The EMPH + URL highlight spans over `source` (decoded offsets), as `(fg, bg)` colour-span lists,
    /// for `PVMARKS` rendering. EMPH matches come first so they win over a URL where the two overlap
    /// (and both win over MDA once the caller prepends them). Computed directly over `source` — the
    /// exact decoded string the styler splices into — so offsets align for any encoding. Empty when the
    /// document has no EMPH phrase / `URLHIGHLIGHT` is off.
    fn mark_spans(&self, source: &str) -> (Vec<(usize, usize, (u8, u8, u8))>, Vec<(usize, usize, (u8, u8, u8))>) {
        let (mut fg, mut bg): (Vec<(usize, usize, (u8, u8, u8))>, Vec<(usize, usize, (u8, u8, u8))>) =
            (Vec::new(), Vec::new());
        let doc = &self.docs[self.active];
        // EMPH: the phrase's matches, in the EMPH colours (bare = whole-word literal; `EMPH/R` = regex;
        // always case-insensitive — same as the editor overlay).
        if let Some(phrase) = doc.emph().filter(|p| !p.is_empty()) {
            let pattern = if doc.emph_is_regex() {
                phrase.to_string()
            } else {
                format!(r"\b{}\b", regex::escape(phrase))
            };
            if let Ok(re) = regex::RegexBuilder::new(&pattern).case_insensitive(true).build() {
                let (efg, ebg) = (self.emph_fg_color(), self.emph_bg_color());
                for m in re.find_iter(source).filter(|m| m.end() > m.start()) {
                    fg.push((m.start(), m.end(), efg));
                    bg.push((m.start(), m.end(), ebg));
                }
            }
        }
        // URLs: the same pattern the editor highlights, trailing punctuation trimmed. Link colour for
        // the text; a background band only if `URLBGCOLOR` is set.
        if self.url_highlight {
            if let Ok(re) = regex::Regex::new(URL_PATTERN) {
                let (ufg, ubg) = (self.url_fg_color(), self.url_bg_color());
                for m in re.find_iter(source) {
                    let trimmed = trim_url_len(&source.as_bytes()[m.start()..m.end()]);
                    if trimmed > 0 {
                        let (s, e) = (m.start(), m.start() + trimmed);
                        fg.push((s, e, ufg));
                        if let Some(c) = ubg {
                            bg.push((s, e, c));
                        }
                    }
                }
            }
        }
        (fg, bg)
    }

    /// Record the current `$CWD` as the remembered directory for its drive, so a later bare `X:`
    /// returns here. Called whenever the cwd changes (and once at startup).
    fn remember_current_drive(&mut self) {
        if let Some(d) = drive_of(&self.cwd) {
            self.drive_dirs.insert(d, self.cwd.clone());
        }
    }

    /// Set the working directory (`$CWD`) directly, without the `CD` verb's prompts/status. Used by the
    /// `--cwd` CLI flag so a `DUPVIEW/W` window, booted in the exe directory (for startup's relative
    /// config loads), can restore the parent's `$CWD` afterward. Ignored if `dir` isn't a directory.
    pub fn set_cwd(&mut self, dir: PathBuf) {
        if dir.is_dir() {
            self.cwd = dir;
            self.remember_current_drive();
        }
    }

    /// Record the startup script this instance ran, so a `DUPVIEW/W` child can be launched with the
    /// same `-s <file>` and boot with identical settings. Called by the GUI once, at boot.
    pub fn set_startup_path(&mut self, path: Option<PathBuf>) {
        self.startup_path = path;
    }

    /// The message from the last FAILED `LOADKBD` (`None` if the most recent load succeeded). The GUI
    /// reads this after `startup.xpl` runs so a bad keymap load surfaces on the status line instead of
    /// being overwritten by later startup commands.
    pub fn kbd_load_error(&self) -> Option<&str> {
        self.last_kbd_error.as_deref()
    }

    /// Take (and clear) the last-failed-`LOADKBD` message — a ONE-SHOT so the GUI can surface it with
    /// priority the first time it would otherwise be overwritten by a later command's feedback, then
    /// let normal feedback resume. Cleared anyway by a subsequent successful load.
    pub fn take_kbd_load_error(&mut self) -> Option<String> {
        self.last_kbd_error.take()
    }

    /// Where a bare `X:` drive change should land: the session's remembered directory for that drive
    /// if it still exists, else the OS's per-drive current directory (first visit), else the drive
    /// root. Keeps the drive letter intact (SUBST-safe).
    fn drive_current_dir(&self, letter: char) -> PathBuf {
        if let Some(dir) = self.drive_dirs.get(&letter) {
            if dir.is_dir() {
                return dir.clone();
            }
        }
        if let Some(dir) = os_drive_current_dir(letter) {
            if dir.is_dir() {
                return dir;
            }
        }
        PathBuf::from(format!("{letter}:\\"))
    }

    /// Split a `DIR` argument into `(directory, optional filespec)`. No arg → the cwd, no
    /// filter. An arg naming an existing directory lists it whole. Otherwise the last path
    /// component is a glob filespec (`*.txt`, `a?.rs`) over the preceding directory (the
    /// cwd when the arg has no directory part).
    fn resolve_dir_spec(&self, arg: Option<&str>) -> (PathBuf, Option<String>) {
        let Some(raw) = arg.map(str::trim).filter(|s| !s.is_empty()) else {
            return (self.cwd.clone(), None);
        };
        let candidate = self.resolve_path(raw);
        if candidate.is_dir() {
            return (candidate, None);
        }
        let pattern = candidate.file_name().map(|n| n.to_string_lossy().into_owned());
        let base = candidate
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.cwd.clone());
        (base, pattern)
    }

    /// List `base` and apply the listing's `/A` attribute filter and `/O` sort (the glob
    /// filespec is applied later, in `open_listing`). Shared by `DIR`, descent, and the
    /// post-delete re-list so all three honor the same options.
    fn list_for(&self, base: &Path, opts: &DirOpts) -> VmResult<Vec<crate::dir::DirEntry>> {
        let listed = if opts.recurse {
            crate::dir::list_dir_recursive(base)
        } else {
            crate::dir::list_dir(base)
        };
        let mut entries =
            listed.map_err(|e| XplError::trappable(format!("DIR: {}: {e}", base.display())))?;
        crate::dir::apply_attr(&mut entries, opts);
        // `/S` sorts purely by name (relative path) so entries group by folder; otherwise
        // directories group first.
        crate::dir::sort_entries(&mut entries, opts.order, opts.reverse, !opts.recurse);
        Ok(entries)
    }

    /// Activate the entry on the caret's line of a directory listing (the GUI calls this
    /// on Enter): descend into a directory (re-listing the same buffer and following the
    /// cwd) or open a file for editing in place. A no-op when the active buffer isn't a
    /// listing or the caret's line has no entry.
    pub fn dir_enter(&mut self) -> VmResult<()> {
        let entry = match self.docs[self.active].dir_entry_at_cursor() {
            Some(e) => e.clone(),
            None => return Ok(()),
        };
        if entry.is_dir {
            let base = lexical_abs(&self.cwd, &entry.path);
            // Keep the view options across the descent, but drop the filespec filter and
            // `/S` recursion (navigating into a subdirectory shows just that dir).
            let mut opts = self.docs[self.active].dir_state().map(|d| d.opts).unwrap_or_default();
            opts.recurse = false;
            let entries = self.list_for(&base, &opts)?;
            self.cwd = base.clone();
            self.remember_current_drive();
            self.docs[self.active].open_listing(base, entries, opts, None);
        } else {
            // Open the file. `DIRNW on` ($DIRNW) opens it in a new tab (leaving the
            // listing behind to keep picking); the default replaces the listing in place.
            let bytes = self.read_doc_bytes(&entry.path.to_string_lossy())?;
            let mut doc = Document::new();
            let restore = (!self.virtual_fs).then(|| xymeta_read(&entry.path, &bytes)).flatten();
            doc.load(bytes, entry.path.clone());
            if let Some(r) = restore {
                apply_xymeta_restore(&mut doc, r);
                }
            if self.new_window {
                self.add_or_reuse(doc);
            } else {
                doc.share_grammars(self.highlight_grammars.clone());
                self.docs[self.active] = doc;
                self.auto_highlight_active();
                // A `DocId` is unique per live `Document`, so replacing the slot gives the pane tree
                // a doc id it has never seen — the focused pane would still point at the LISTING's
                // dead id, and the file surfaced as a separate tab instead of taking the listing's
                // place (i.e. `DIRNW off` looked like `DIRNW on`). Every other in-place replacement
                // (see `add_or_reuse`) re-points the pane; this one has to as well.
                self.sync_panes();
            }
        }
        Ok(())
    }

    /// Whether the active buffer is a `DIR`/`LS` listing (the GUI routes Enter and
    /// printable keys to the express-key finder instead of editing when true).
    pub fn active_is_dir_listing(&self) -> bool {
        self.docs[self.active].is_dir_listing()
    }

    /// Whether the active buffer is a `GREP` results listing.
    pub fn active_is_grep_listing(&self) -> bool {
        self.docs[self.active].is_grep_listing()
    }

    /// Whether the active buffer is a `HELP`/`MAN` results listing (the reusable read-only tab).
    pub fn active_is_help_listing(&self) -> bool {
        self.docs[self.active].is_help_listing()
    }

    /// `GJ` — open the file for the `GREP` hit on the caret's line (or switch to it if already open),
    /// jump to its `line:col`, and centre it. Trappable error when the active buffer isn't a results
    /// listing or the caret isn't on a hit. Public so the GUI can drive it from Enter / a double-click.
    pub fn grep_jump(&mut self) -> VmResult<Value> {
        let hit = match self.docs[self.active].grep_hit_at_cursor() {
            Some(h) => h.clone(),
            None => {
                return Err(XplError::trappable(
                    "GJ: no GREP result on this line (run GREP, then put the caret on a result)",
                ))
            }
        };
        let path_str = hit.path.to_string_lossy().into_owned();
        // Carry the results tab's match highlight (EMPH) onto the file we open, so the same matches
        // light up there. (The active buffer IS the results tab — `grep_hit_at_cursor` guaranteed it.)
        let emph = self.docs[self.active].emph().map(str::to_string);
        let emph_regex = self.docs[self.active].emph_is_regex();
        // An `ALLTABS`/`ALLBUFS` hit carries its SOURCE document id: switch to that exact live tab
        // (its in-memory content, with any unsaved edits, is what was searched) rather than reopening
        // a path from disk — which would miss edits and can't resolve an untitled buffer.
        if let Some(src_id) = hit.source {
            match self.docs.iter().position(|d| d.id().0 == src_id) {
                Some(idx) => self.set_active(idx),
                // The source tab was closed since the search — fall back to the path if it has one.
                None if !path_str.is_empty() && PathBuf::from(&path_str).is_file() => {
                    self.open_doc(&path_str, false, false)?;
                }
                None => {
                    return Err(XplError::trappable(format!(
                        "GJ: the tab for this result ({path_str}) is no longer open"
                    )))
                }
            }
        } else if let Some(idx) = self.find_open_tab(&path_str) {
            // Filesystem hit: open, or switch to it if already open — either way, no prompt.
            self.set_active(idx);
        } else {
            self.open_doc(&path_str, false, false)?;
        }
        if let Some(phrase) = emph {
            self.active().set_emph(Some(phrase), emph_regex);
        }
        // Resolve line:col → a rope offset in the (decoded) buffer and place the caret there.
        let off = {
            let text = self.active().buffer().text();
            line_col_to_offset(&text, hit.line, hit.col)
        };
        self.set_active_caret(off, None);
        self.reveal_center = true; // scroll the target to mid-screen, like a search landing
        self.set_focus(Target::Document);
        Ok(Value::str(format!("{path_str}:{}", hit.line)))
    }

    /// `DJ` — from a line of the diff results tab, jump into the live source buffer at the
    /// corresponding new-side (`+`) line. The target line is derived from the caret's enclosing `@@`
    /// hunk header plus the context/added lines above it, so no per-line table is stored; only the
    /// diff's source buffer id is remembered (`diff_source`), cleared when the tab stops being a diff.
    /// Returns a status string ("name:line" on success, else why not) and never errors hard.
    pub fn diff_jump(&mut self) -> VmResult<Value> {
        let Some(src_id) = self.diff_source else {
            return Ok(Value::str("DJ: run DIFF first, then put the caret on a diff line"));
        };
        if !self.docs[self.active].is_help_listing() {
            return Ok(Value::str("DJ: not on the diff results tab"));
        }
        let text = self.docs[self.active].buffer().text();
        let cur = self.docs[self.active].buffer().cursor();
        let lines: Vec<&str> = text.split('\n').collect();
        if lines.is_empty() {
            return Ok(Value::str("DJ: put the caret on a line inside a diff hunk"));
        }
        // Which display line is the caret on?
        let line_idx = text[..cur.min(text.len())].bytes().filter(|&b| b == b'\n').count();
        let line_idx = line_idx.min(lines.len() - 1);
        // Walk up to the nearest `@@ ... +c,d @@` header.
        let Some(header_idx) = (0..=line_idx).rev().find(|&i| lines[i].starts_with("@@")) else {
            return Ok(Value::str("DJ: put the caret on a line inside a diff hunk"));
        };
        let Some(new_start) = parse_hunk_new_start(lines[header_idx]) else {
            return Ok(Value::str("DJ: could not read the hunk header"));
        };
        // Count new-side lines (context ` ` or added `+`) between the header and the caret line; the
        // caret's own new-side line number is new_start + that count (a `-` line maps to the next one).
        let mut seen = 0usize;
        if header_idx + 1 < line_idx + 1 {
            for l in &lines[header_idx + 1..line_idx] {
                match l.chars().next() {
                    Some(' ') | Some('+') => seen += 1,
                    _ => {}
                }
            }
        }
        let target = new_start.saturating_add(seen).max(1);
        // Find the source buffer and jump into it.
        let Some(idx) = self.docs.iter().position(|d| d.id().0 == src_id) else {
            return Ok(Value::str("DJ: the diffed buffer is no longer open"));
        };
        self.set_active(idx);
        self.goto_line(target);
        self.reveal_center = true;
        self.set_focus(Target::Document);
        let name = self.docs[self.active]
            .path()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("buffer")
            .to_string();
        Ok(Value::str(format!("{name}:{target}")))
    }

    /// The active listing's base directory and live express-key buffer, for the footer.
    /// `None` when the active buffer isn't a listing.
    pub fn dir_footer(&self) -> Option<(PathBuf, String)> {
        self.docs[self.active]
            .dir_state()
            .map(|d| (d.base.clone(), d.find.clone()))
    }

    /// Express-key (type-to-find) input for the active listing — extend by `ch`.
    pub fn dir_find_push(&mut self, ch: char) {
        self.docs[self.active].dir_find_push(ch);
    }

    /// Express-key: shorten the active listing's type-to-find buffer by one char.
    pub fn dir_find_backspace(&mut self) {
        self.docs[self.active].dir_find_backspace();
    }

    /// Express-key: clear the active listing's type-to-find buffer (e.g. on Esc).
    pub fn dir_find_clear(&mut self) {
        self.docs[self.active].dir_find_clear();
    }

    /// `F3` in a listing: jump to the next express-key match (wraps).
    pub fn dir_find_again(&mut self) {
        self.docs[self.active].dir_find_again();
    }

    /// Copy the caret entry's full path to the clipboard (`$CB`; the GUI mirrors it to
    /// the OS clipboard). Returns the copied path for the status line, or `None` when the
    /// active buffer isn't a listing / the caret has no entry.
    pub fn dir_copy_path(&mut self) -> Option<String> {
        let path = self.docs[self.active]
            .dir_entry_at_cursor()
            .map(|e| e.path.to_string_lossy().into_owned())?;
        self.clipboard = path.clone();
        Some(path)
    }

    /// Re-list the active listing's directory in place (after a delete), preserving the
    /// compact/columns view style. No-op when the active buffer isn't a listing.
    fn refresh_listing(&mut self) {
        let Some((base, opts, filter)) = self.docs[self.active]
            .dir_state()
            .map(|d| (d.base.clone(), d.opts, d.filter.clone()))
        else {
            return;
        };
        if let Ok(entries) = self.list_for(&base, &opts) {
            self.docs[self.active].open_listing(base, entries, opts, filter);
        }
    }

    // ---- DIR/W separate listing window ------------------------------------------------------

    /// `DIR/W` — open (or refocus + refresh) the separate listing window for `path`.
    fn open_dir_window(&mut self, path: Option<String>, opts: DirOpts) -> VmResult<Value> {
        let (base, filter) = self.resolve_dir_spec(path.as_deref());
        let base = lexical_abs(&self.cwd, &base);
        let entries = self.list_for(&base, &opts)?;
        let count = entries.iter().filter(|e| e.name != "..").count();
        let mut doc = Document::new();
        doc.open_listing(base.clone(), entries, opts, filter.clone());
        self.dir_window =
            Some(DirWindowState { base_mtime: dir_mtime(&base), doc, just_opened: true });
        let s = if count == 1 { "" } else { "s" };
        let msg = match &filter {
            Some(p) => format!("{count} item{s} matching {p} in {} (window)", base.display()),
            None => format!("{count} item{s} in {} (window)", base.display()),
        };
        Ok(Value::str(msg))
    }

    /// Whether the `DIR/W` window is open (the GUI shows it while true).
    pub fn dir_window_active(&self) -> bool {
        self.dir_window.is_some()
    }

    /// The document `$DP`/`$DR` should read: the separate `DIR/W` listing when it's open and the
    /// given (active) document isn't itself a listing, otherwise the active document.
    fn dir_special_source<'a>(&'a self, active: &'a Document) -> &'a Document {
        match &self.dir_window {
            Some(w) if !active.is_dir_listing() => &w.doc,
            _ => active,
        }
    }

    /// One-shot: a fresh `DIR/W` was issued, so the GUI should create the window (or focus +
    /// refresh an existing one). Cleared on read.
    pub fn take_dir_window_open(&mut self) -> bool {
        match self.dir_window.as_mut() {
            Some(w) if w.just_opened => {
                w.just_opened = false;
                true
            }
            _ => false,
        }
    }

    /// Close the `DIR/W` window (× / Esc).
    pub fn close_dir_window(&mut self) {
        self.dir_window = None;
    }

    /// Render data for the `DIR/W` window (lines, selected row, footer). `None` when closed.
    pub fn dir_window_view(&self) -> Option<DirWindowView> {
        let w = self.dir_window.as_ref()?;
        let d = w.doc.dir_state()?;
        Some(DirWindowView {
            lines: w.doc.buffer().text().lines().map(str::to_string).collect(),
            selected: w.doc.dir_cursor_row(),
            base: d.base.clone(),
            path: dir_dp_string(&w.doc),
            find: d.find.clone(),
        })
    }

    /// Move the `DIR/W` selection by `delta` rows (arrows / PageUp / PageDown).
    pub fn dir_window_move(&mut self, delta: isize) {
        if let Some(w) = self.dir_window.as_mut() {
            for _ in 0..delta.unsigned_abs() {
                if delta >= 0 {
                    w.doc.move_down(false);
                } else {
                    w.doc.move_up(false);
                }
            }
        }
    }

    /// Move the `DIR/W` selection to the first / last row (Home / End).
    pub fn dir_window_to_edge(&mut self, end: bool) {
        if let Some(w) = self.dir_window.as_mut() {
            if end {
                w.doc.move_doc_end(false);
            } else {
                w.doc.move_doc_start(false);
            }
        }
    }

    /// Select listing row `row` (a click in the window).
    pub fn dir_window_select_row(&mut self, row: usize) {
        if let Some(w) = self.dir_window.as_mut() {
            w.doc.dir_goto_row(row);
        }
    }

    /// Copy the `DIR/W` window's caret-entry full path to the clipboard (`$CB`) — the window's
    /// equivalent of the in-tab listing's [`dir_copy_path`]. Returns the path (None if empty).
    pub fn dir_window_copy_path(&mut self) -> Option<String> {
        let path = self
            .dir_window
            .as_ref()?
            .doc
            .dir_entry_at_cursor()
            .map(|e| e.path.to_string_lossy().into_owned())?;
        self.clipboard = path.clone();
        Some(path)
    }

    /// Type-to-find in the window's listing (printable char / Backspace / F3 / clear).
    pub fn dir_window_find_push(&mut self, ch: char) {
        if let Some(w) = self.dir_window.as_mut() {
            w.doc.dir_find_push(ch);
        }
    }
    pub fn dir_window_find_backspace(&mut self) {
        if let Some(w) = self.dir_window.as_mut() {
            w.doc.dir_find_backspace();
        }
    }
    pub fn dir_window_find_again(&mut self) {
        if let Some(w) = self.dir_window.as_mut() {
            w.doc.dir_find_again();
        }
    }
    pub fn dir_window_find_clear(&mut self) {
        if let Some(w) = self.dir_window.as_mut() {
            w.doc.dir_find_clear();
        }
    }

    /// Activate the entry under the window's caret (Enter / double-click): descend into a
    /// subdirectory (re-listing in the window) or open a file in a NEW tab in the parent window
    /// (always a new tab — the window is a picker).
    pub fn dir_window_activate(&mut self) -> VmResult<DirWindowAction> {
        // Snapshot the entry so we don't hold a borrow across `list_for` / `add_or_reuse`.
        let entry =
            match self.dir_window.as_ref().and_then(|w| w.doc.dir_entry_at_cursor().cloned()) {
                Some(e) => e,
                None => return Ok(DirWindowAction::None),
            };
        if entry.is_dir {
            let base = lexical_abs(&self.cwd, &entry.path);
            let mut opts = self
                .dir_window
                .as_ref()
                .and_then(|w| w.doc.dir_state().map(|d| d.opts))
                .unwrap_or_default();
            opts.recurse = false; // descending shows just that directory
            let entries = self.list_for(&base, &opts)?;
            if let Some(w) = self.dir_window.as_mut() {
                w.doc.open_listing(base.clone(), entries, opts, None);
                w.base_mtime = dir_mtime(&base);
            }
            Ok(DirWindowAction::Descended(base))
        } else {
            let bytes = self.read_doc_bytes(&entry.path.to_string_lossy())?;
            let mut doc = Document::new();
            let restore = (!self.virtual_fs).then(|| xymeta_read(&entry.path, &bytes)).flatten();
            doc.load(bytes, entry.path.clone());
            if let Some(r) = restore {
                apply_xymeta_restore(&mut doc, r);
                }
            doc.share_grammars(self.highlight_grammars.clone());
            // `DIRWNW on` (default) opens a new tab; `off` reuses the ACTIVE one. Unsaved work is
            // never discarded to do so — a modified buffer is left alone and the file opens beside
            // it. (`DIRNW`'s in-tab case has no such risk: what it replaces is the listing itself.)
            if self.dir_window_new_tab || self.docs[self.active].is_modified() {
                self.add_or_reuse(doc);
            } else {
                self.docs[self.active] = doc;
                self.sync_panes(); // fresh `DocId` — re-point the focused pane (see `dir_enter`)
            }
            self.auto_highlight_active();
            // `DIRWCLOSE on` dismisses the picker now that it has done its job. Only on a FILE —
            // descending into a subdirectory (the branch above) keeps the window so you can carry on
            // browsing. The GUI notices the listing is gone and tears the OS window down to match.
            if self.dir_window_close_on_open {
                self.close_dir_window();
            }
            Ok(DirWindowAction::Opened(entry.path))
        }
    }

    /// On the window regaining focus: re-list only if the directory changed since the last scan
    /// (cheap mtime check), preserving the selected filename when it still exists.
    pub fn dir_window_refresh_if_changed(&mut self) {
        let Some((base, opts, filter, selected_name, prev_mtime)) =
            self.dir_window.as_ref().and_then(|w| {
                w.doc.dir_state().map(|d| {
                    (
                        d.base.clone(),
                        d.opts,
                        d.filter.clone(),
                        w.doc.dir_entry_at_cursor().map(|e| e.name.clone()),
                        w.base_mtime,
                    )
                })
            })
        else {
            return;
        };
        let now = dir_mtime(&base);
        if now == prev_mtime {
            return; // unchanged — skip the rescan
        }
        if let Ok(entries) = self.list_for(&base, &opts) {
            if let Some(w) = self.dir_window.as_mut() {
                w.doc.open_listing(base, entries, opts, filter);
                w.base_mtime = now;
                if let Some(name) = selected_name {
                    if let Some(i) = w
                        .doc
                        .dir_state()
                        .and_then(|d| d.entries.iter().position(|e| e.name == name))
                    {
                        w.doc.dir_goto_row(i);
                    }
                }
            }
        }
    }

    /// `Del` in a listing: arm a delete-with-confirm for the caret entry. No-op on the
    /// `..` parent or a non-listing buffer.
    pub fn dir_request_delete(&mut self) {
        if let Some(e) = self.docs[self.active].dir_entry_at_cursor() {
            if e.name != ".." {
                self.pending_delete = Some((e.path.clone(), e.name.clone()));
            }
        }
    }

    /// The pending delete-with-confirm prompt (`Del` was pressed), if armed.
    pub fn dir_delete_prompt(&self) -> Option<String> {
        self.pending_delete.as_ref().map(|(_, name)| format!("Delete {name}?  (y/n)"))
    }

    /// Resolve a pending delete: `yes` deletes (file, or **empty** directory) then re-lists;
    /// `no` cancels. Returns a status string, or `None` if nothing was armed.
    pub fn dir_resolve_delete(&mut self, yes: bool) -> Option<String> {
        let (path, name) = self.pending_delete.take()?;
        if !yes {
            return Some("delete cancelled".to_string());
        }
        let res = if path.is_dir() {
            std::fs::remove_dir(&path)
        } else {
            std::fs::remove_file(&path)
        };
        Some(match res {
            Ok(()) => {
                remove_xymeta_sidecar(&path); // don't orphan the MDA sidecar
                self.refresh_listing();
                format!("deleted {name}")
            }
            Err(e) => format!("delete failed: {e}"),
        })
    }

    /// Remove the active buffer, never leaving zero (the last close opens a fresh empty one),
    /// clamp the active index, and return focus to the document. Returns the closed name.
    fn close_active_buffer(&mut self) -> String {
        let doc = &self.docs[self.active];
        let name = doc.file_name().unwrap_or_else(|| "(untitled)".to_string());
        // Record a reopenable close: a real named file (not an unnamed scratch buffer or a DIR
        // listing). The `ABD` delete path un-records afterward (a deleted file isn't reopenable).
        if !doc.is_dir_listing() {
            if let Some(path) = doc.path() {
                let (path, caret) = (path.to_path_buf(), doc.buffer().cursor());
                self.record_closed(path, caret);
            }
        }
        // `CLOSEBACK on`: land on the LAST-VISITED tab (resolved by stable id, so it survives the index
        // shift) rather than the left neighbor — e.g. return to the DIR listing a file was opened from.
        let back_target = self.close_back.then_some(self.focus_prev).flatten();
        self.docs.remove(self.active);
        // Removing shifts every index after `active`, so the stored alternate would point at the
        // wrong (or a gone) tab — drop it. `BUFALT` falls back to `BUFPREV` until the next switch.
        self.alt_active = None;
        self.focus_prev = None; // consumed; the next switch/open sets it afresh
        if self.docs.is_empty() {
            self.docs.push(Document::new());
            self.active = 0;
        } else if let Some(idx) = back_target.and_then(|id| self.docs.iter().position(|d| d.id() == id)) {
            self.active = idx; // CLOSEBACK: the last-visited tab
        } else {
            // End on the PREVIOUS (left) buffer, not the next: closing tab N lands on N-1 (closing
            // the first tab lands on the new first). `BUFPREV`-style "back up" feel on `AB`.
            self.active = self.active.saturating_sub(1);
        }
        self.set_focus(Target::Document);
        // Stable-views: the pane that was showing the closed buffer stays in place and reconcile
        // repoints it to another open doc (the focused pane shows the new active). A pane collapses
        // ONLY when there are now fewer open docs than panes. `TUS`/`FS` remain the explicit unsplit
        // verbs — closing a file no longer undoes a pane.
        self.sync_panes();
        name
    }

    /// Delete the active document's backing file from disk, then close the buffer. Shared by the
    /// immediate `ABD` path and the confirmed `ABD/V` resolution.
    fn delete_note_now(&mut self, path: &Path, name: &str) -> String {
        let res = if self.virtual_fs {
            self.files.remove(&path.to_string_lossy().into_owned());
            Ok(())
        } else {
            std::fs::remove_file(path)
        };
        match res {
            Ok(()) => {
                if !self.virtual_fs {
                    remove_xymeta_sidecar(path); // don't orphan the MDA sidecar
                }
                let _ = self.close_active_buffer();
                self.recently_closed.retain(|(p, _)| p != path); // deleted → not reopenable
                format!("deleted {name}")
            }
            Err(e) => format!("delete failed: {e}"),
        }
    }

    /// The pending close-confirmation prompt (`AB/V` save / `ABD` delete), if armed.
    pub fn close_prompt(&self) -> Option<String> {
        self.pending_close.as_ref().map(|p| match p {
            PendingClose::Save { name } => {
                format!("Save changes to {name}?  (y) save  (n) discard  (esc) cancel")
            }
            PendingClose::DeleteNote { name, .. } => format!("Delete {name} from disk?  (y/n)"),
            PendingClose::DeleteFile { name, .. } => format!("Delete {name} from disk?  (y/n)"),
            PendingClose::StoreOverwrite { name, .. } => {
                format!("{name} already exists. Overwrite and close?  (y) overwrite  (n/esc) cancel")
            }
            PendingClose::Overwrite { name, .. } => {
                format!("{name} already exists. Overwrite?  (y) overwrite  (n/esc) cancel")
            }
            PendingClose::SaveAsOverwrite { name, .. } => {
                format!("{name} already exists. Overwrite?  (y) overwrite  (n/esc) cancel")
            }
            PendingClose::SaveSelectionOverwrite { name, .. } => {
                format!("{name} already exists. Overwrite?  (y) overwrite  (n/esc) cancel")
            }
        })
    }

    /// Resolve a pending close-confirmation. Returns a status string, or `None` if nothing
    /// was armed.
    pub fn resolve_close(&mut self, ans: CloseAnswer) -> Option<String> {
        let pending = self.pending_close.take()?;
        match pending {
            PendingClose::Save { .. } => match ans {
                CloseAnswer::Cancel => Some("close cancelled".to_string()),
                CloseAnswer::No => {
                    let name = self.close_active_buffer();
                    Some(format!("closed {name} (changes discarded)"))
                }
                CloseAnswer::Yes => match self.docs[self.active].path() {
                    // Has a file: save to it, then close.
                    Some(p) => {
                        let path = p.to_string_lossy().into_owned();
                        let bytes = self.docs[self.active].save_bytes();
                        match self.write_doc_bytes(&path, &bytes) {
                            Ok(()) => {
                                self.sync_color_sidecar(&self.docs[self.active], &path, &bytes);
                                self.docs[self.active].mark_saved();
                                let name = self.close_active_buffer();
                                Some(format!("saved + closed {name}"))
                            }
                            Err(e) => Some(format!("save failed: {}", e.message())),
                        }
                    }
                    // Untitled: needs a name — pop Save-As and keep the buffer open.
                    None => {
                        self.save_as_requested = true;
                        Some("name the file (Save-As), then close".to_string())
                    }
                },
            },
            PendingClose::DeleteNote { path, name } => match ans {
                CloseAnswer::Yes => Some(self.delete_note_now(&path, &name)),
                _ => Some("delete cancelled".to_string()),
            },
            PendingClose::DeleteFile { path, name } => match ans {
                CloseAnswer::Yes => {
                    let res = if self.virtual_fs {
                        self.files.remove(&path.to_string_lossy().into_owned());
                        Ok(())
                    } else if path.is_dir() {
                        std::fs::remove_dir(&path) // empty dirs only (no recursive wipe)
                    } else {
                        std::fs::remove_file(&path)
                    };
                    match res {
                        Ok(()) => {
                            if !self.virtual_fs {
                                remove_xymeta_sidecar(&path); // don't orphan the MDA sidecar
                            }
                            self.refresh_listings_after_delete(&path);
                            Some(format!("deleted {name}"))
                        }
                        Err(e) => Some(format!("delete failed: {e}")),
                    }
                }
                _ => Some("delete cancelled".to_string()),
            },
            PendingClose::Overwrite { path, name } => match ans {
                CloseAnswer::Yes => {
                    let key = path.to_string_lossy().into_owned();
                    let bytes = self.docs[self.active].save_bytes();
                    match self.write_doc_bytes(&key, &bytes) {
                        Ok(()) => {
                            self.sync_color_sidecar(&self.docs[self.active], &key, &bytes);
                            Some(format!("saved copy to {name}"))
                        }
                        Err(e) => Some(format!("save failed: {}", e.message())),
                    }
                }
                _ => Some("save cancelled".to_string()),
            },
            PendingClose::StoreOverwrite { path, name: _ } => match ans {
                CloseAnswer::Yes => {
                    // Overwrite confirmed: save + ADOPT the path, then close the tab.
                    let key = path.to_string_lossy().into_owned();
                    let bytes = self.docs[self.active].save_bytes();
                    match self.write_doc_bytes(&key, &bytes) {
                        Ok(()) => {
                            self.docs[self.active].set_path(path);
                            self.sync_color_sidecar(&self.docs[self.active], &key, &bytes);
                            self.docs[self.active].mark_saved();
                            let closed = self.close_active_buffer();
                            Some(format!("stored + closed {closed}"))
                        }
                        Err(e) => Some(format!("save failed: {}", e.message())),
                    }
                }
                _ => Some("store cancelled".to_string()),
            },
            PendingClose::SaveAsOverwrite { path, name } => match ans {
                CloseAnswer::Yes => {
                    // Overwrite confirmed: save + ADOPT the path (the tab stays open).
                    let key = path.to_string_lossy().into_owned();
                    let bytes = self.docs[self.active].save_bytes();
                    match self.write_doc_bytes(&key, &bytes) {
                        Ok(()) => {
                            self.docs[self.active].set_path(path);
                            self.sync_color_sidecar(&self.docs[self.active], &key, &bytes);
                            self.docs[self.active].mark_saved();
                            Some(self.tr_fmt("msg.save.written", "saved {name}", &[("name", &name)]))
                        }
                        Err(e) => Some(format!("save failed: {}", e.message())),
                    }
                }
                _ => Some("save cancelled".to_string()),
            },
            PendingClose::SaveSelectionOverwrite { path, name, bytes } => match ans {
                CloseAnswer::Yes => {
                    // Overwrite confirmed: write the selection bytes captured when armed.
                    let key = path.to_string_lossy().into_owned();
                    match self.write_doc_bytes(&key, &bytes) {
                        Ok(()) => Some(format!("saved selection to {name}")),
                        Err(e) => Some(format!("save failed: {}", e.message())),
                    }
                }
                _ => Some("save cancelled".to_string()),
            },
        }
    }

    /// `Ctrl+Enter` in a listing: hand the caret entry to the OS default handler. Returns
    /// a status string (empty when the active buffer isn't a listing / has no entry).
    pub fn dir_launch(&mut self) -> String {
        let entry = match self.docs[self.active].dir_entry_at_cursor() {
            Some(e) => (e.path.clone(), e.name.clone()),
            None => return String::new(),
        };
        match open_in_os(&entry.0) {
            Ok(()) => format!("launched {}", entry.1),
            Err(e) => format!("launch failed: {e}"),
        }
    }

    /// Record the scan code of the key being dispatched (so `$SC` can read it).
    pub fn set_last_scan(&mut self, code: i64) {
        self.last_scan = code;
    }

    /// Record the modifier bitmask held for the key being dispatched (so `$SCC` = `$SC` + this).
    pub fn set_last_modifier_bits(&mut self, bits: i64) {
        self.last_mod_bits = bits;
    }

    /// Record the last feedback message shown on the status line (for `$LS`).
    pub fn set_last_status_line(&mut self, msg: &str) {
        self.last_status_line = msg.to_string();
    }

    /// The GUI's top visible logical line (its scroll position), for `$CY`. Pushed each frame.
    pub fn set_viewport_top(&mut self, line: usize) {
        self.viewport_top = line;
    }

    /// Push the caret's VISUAL position `(col, row)` (wrap-aware; row relative to the viewport top)
    /// for `$CX`/`$CY`. The GUI calls this each frame from the rendered caret.
    pub fn set_cursor_visual(&mut self, col: i64, row: i64) {
        self.cursor_vis = Some((col, row));
    }

    /// The caret's last-published VISUAL column (`$CX`), if the GUI has rendered a frame. `None`
    /// headless (before any render). For tests + anything that needs the wrap-aware column.
    pub fn cursor_visual_col(&self) -> Option<i64> {
        self.cursor_vis.map(|(c, _)| c)
    }

    /// Drain the accumulated `MU`/`MD` viewport scroll (visual rows; +end/−start), zeroing it.
    pub fn take_scroll_rows(&mut self) -> i32 {
        std::mem::take(&mut self.pending_scroll_rows)
    }

    /// Drain a pending `LB`/`LE` visual-line-edge refine `(end, extend, from)`, clearing it. The
    /// GUI calls this each frame; `from` is the caret offset before the logical-edge fallback.
    pub fn take_visual_edge(&mut self) -> Option<(bool, bool, usize)> {
        self.pending_visual_edge.take()
    }

    /// Drain a pending `CARETMOTION visual` Left/Right request `(right, extend, from)`. GUI-only; `from`
    /// is the caret offset before the engine's logical fallback move.
    pub fn take_visual_move(&mut self) -> Option<(bool, bool, usize)> {
        self.pending_visual_move.take()
    }

    /// Whether `CARETMOTION` is `visual` (the GUI checks this to know a Left/Right may have queued a
    /// visual refine).
    pub fn caret_visual(&self) -> bool {
        self.caret_visual
    }

    /// Track the operative keymap (the editor calls this on every install) so `SAVEKBD` can
    /// serialize the current layout from here.
    pub fn set_keymap(&mut self, km: Keymap) {
        self.current_keymap = Some(km);
        self.keymap_gen = self.keymap_gen.wrapping_add(1);
    }

    /// Register the editor's boot keymap as the `default` layout slot, so `KBDMODE default` can always
    /// return to it even if the user never issued a `LOADKBD`. Called once at `Editor` construction.
    pub fn init_kbd_mode(&mut self, km: Keymap) {
        self.kbd_mode = DEFAULT_KBD_MODE.to_string();
        self.kbd_modes.insert(DEFAULT_KBD_MODE.to_string(), km);
    }

    /// The active keyboard-layout slot name (`$KBDMODE`).
    pub fn kbd_mode(&self) -> &str {
        &self.kbd_mode
    }

    /// Number of resident keyboard-layout slots — the GUI shows the mode indicator only when more than
    /// one layout is loaded (i.e. the user has opted into multi-layout switching).
    pub fn kbd_mode_count(&self) -> usize {
        self.kbd_modes.len()
    }

    /// Resident layout slot names, sorted (`$KBDMODES`).
    pub fn kbd_mode_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.kbd_modes.keys().cloned().collect();
        v.sort();
        v
    }

    /// A counter bumped on each keymap install — a cheap cache key for the keyboard chart.
    pub fn keymap_generation(&self) -> u64 {
        self.keymap_gen
    }

    /// The loaded keymap's tables for the Help keyboard chart: one `(table name, [(key code, binding)])`
    /// per distinct modifier layer present in the KBD, ordered Normal-first then by bit value. The
    /// table name decomposes the layer's bitmask into its modifier names (e.g. `SHIFT+CTRL`); each
    /// binding is resolved the same way `KEY($SCC)` does — `key_binding(code + mask)` — since a chart
    /// entry is exactly a `$SCC` value (scan code + the table's modifier bits).
    pub fn keyboard_tables(&self) -> Vec<(String, Vec<(u8, String)>)> {
        let Some(km) = self.current_keymap.as_ref() else {
            return Vec::new();
        };
        // Distinct layer bitmasks that actually have bindings, Normal (0) first.
        let mut masks: Vec<u32> = km.tables.keys().map(|&(m, _)| m).collect();
        masks.sort_unstable();
        masks.dedup();
        masks
            .into_iter()
            .map(|mask| {
                // Name the layer by decomposing its bitmask into the modifiers whose bits are set
                // (declaration order), e.g. "SHIFT+CTRL"; the unshifted layer (0) is "Normal".
                let name = if mask == 0 {
                    "Normal".to_string()
                } else {
                    let names: Vec<&str> = km
                        .modifiers
                        .iter()
                        .filter(|m| mask & m.bit != 0)
                        .map(|m| m.name.as_str())
                        .collect();
                    if names.is_empty() { format!("Layer {mask}") } else { names.join("+") }
                };
                let mut entries: Vec<(u8, String)> = Vec::new();
                for code in 1..=km.keys_max {
                    if let Some(binding) = self.key_binding(i64::from(code) + i64::from(mask)) {
                        entries.push((code, binding));
                    }
                }
                (name, entries)
            })
            .collect()
    }

    /// `AUTOCFG`: if config tracking is on, rewrite the tracked file when the (settings-only)
    /// config has changed since the last write. Called each frame (GUI) / after each command (TUI);
    /// cheap (a tiny string build + compare) and a no-op when nothing changed. Write errors are
    /// swallowed (best-effort mirroring; the manual `SAVECFG` surfaces real errors).
    pub fn poll_config_track(&mut self) {
        let Some(path) = self.config_track_path.clone() else { return };
        let text = self.config_script(false, false);
        if text != self.config_last_written {
            if self.write_file(&path, &text).is_ok() {
                self.config_last_written = text;
            }
        }
    }

    /// Take the keymap a `LOADKBD` produced, if any (consumed by the editor).
    pub fn take_pending_kbd(&mut self) -> Option<Keymap> {
        self.pending_kbd.take()
    }

    /// The snapshotted command to run for a pending `SUBMIT` (consumed by the editor), if any.
    pub fn take_cmline_submit(&mut self) -> Option<String> {
        std::mem::take(&mut self.cmline_submit)
    }

    /// The snapshotted command to run for a pending `XC` (exec-keep) (consumed by the editor), if any.
    pub fn take_cmline_exec(&mut self) -> Option<String> {
        std::mem::take(&mut self.cmline_exec)
    }

    /// Next deferred XPL to run (FIFO), if any — from `RUNHERE` or a `&X` script binder — paired
    /// with its positional args (the `RUNHERE [args…]` operands; empty for binders / config runs).
    /// The editor drains this in a loop after the current script returns.
    pub fn take_pending_run(&mut self) -> Option<(String, Vec<String>)> {
        if self.pending_run.is_empty() {
            None
        } else {
            Some(self.pending_run.remove(0))
        }
    }

    /// A pending `QUIT` request, if any: `Some(true)` = prompt to save dirty tabs first,
    /// `Some(false)` = quit without warning. Consumed by the shell.
    pub fn take_quit(&mut self) -> Option<bool> {
        self.quit_request.take()
    }

    /// The first buffer with unsaved changes (modified, incl. typed-into untitled), or `None`.
    /// Drives the `QUIT` save-prompt sweep.
    pub fn next_dirty_tab(&self) -> Option<usize> {
        self.docs.iter().position(|d| d.is_modified())
    }

    /// Mark the active document as saved without writing (the `QUIT` "discard" choice — its
    /// changes are abandoned because the app is exiting anyway).
    pub fn mark_active_saved(&mut self) {
        self.docs[self.active].mark_saved();
    }

    /// Place the active document's caret/selection (e.g. from a GUI mouse click).
    /// Stash the live selection (the `$SE` source: the blind buffer or active doc) into `$SEL` if
    /// it's non-empty. Called before anything that might clear/replace the selection — both the
    /// command path (`editor_op`) and the GUI-direct caret moves (mouse click/drag, visual arrows) —
    /// so the contents survive the clear regardless of how it happens.
    fn capture_last_selection(&mut self) {
        let captured = {
            let doc = self.blind.as_ref().unwrap_or(&self.docs[self.active]);
            match doc.buffer().selection_span() {
                Some((s, e)) if s < e => Some(doc.buffer().selection_text()),
                _ => None,
            }
        };
        if let Some(sel) = captured {
            self.last_selection = sel;
        }
    }

    pub fn set_active_caret(&mut self, cursor: usize, anchor: Option<usize>) {
        self.capture_last_selection(); // a mouse drag/click may clear the selection — keep it in $SEL
        let ps = self.persistent;
        let d = &mut self.docs[self.active];
        d.set_caret(cursor, anchor);
        // Under persistent-selection mode a mouse DRAG makes a DEFINED (persistent) block, so it
        // survives the next click/motion like a `DF`/`DW` selection instead of being transient.
        if anchor.is_some() && ps {
            d.mark_defined();
        }
    }

    /// Move the active document's caret to an EXACT byte offset (no char-boundary snap) — the hex
    /// view's byte-granular caret advance after an edit. Clears any selection.
    pub fn hex_set_caret(&mut self, byte: usize) {
        self.capture_last_selection();
        self.docs[self.active].move_primary_to_byte(byte, false);
    }

    /// Place the command line's caret at rope offset `off` (a GUI mouse click into the
    /// CMline). The cmline is a buffer separate from the document stack, so this is its
    /// own entry point rather than going through [`set_active_caret`].
    /// Place the command-line caret at `cursor`, optionally anchoring a selection at `anchor`
    /// (mouse drag-select). `None` clears any selection — a plain click or caret move.
    pub fn set_cmline_caret(&mut self, cursor: usize, anchor: Option<usize>) {
        self.cmline.set_caret(cursor, anchor);
    }
    /// Select the word / whole line at the command-line caret, ON the cmline buffer directly. The
    /// `DW`/`DL` verbs can't be used here: they're `FOCUS_DOC_VERBS`, so run from the cmline they'd
    /// shift focus and define in the document. Used by the cmline's right-click "Select …" menu.
    pub fn cmline_define_word(&mut self) {
        self.cmline.define_word();
    }
    pub fn cmline_define_line(&mut self) {
        self.cmline.define_line();
    }

    /// Place the active document's caret from a mouse **click** — PS-aware, so under
    /// persistent selection the click keeps the block (navigation) instead of
    /// clearing it. (Drag-select still uses [`set_active_caret`].)
    pub fn click_active_caret(&mut self, off: usize) {
        self.capture_last_selection(); // a click clears the selection — keep its text in $SEL
        let p = self.persistent;
        self.docs[self.active].set_persistent(p); // ensure the flag is current
        self.docs[self.active].click_caret(off);
    }

    /// Add / toggle a secondary caret at byte offset `off` in the active document (`CARETADD` — the
    /// Ctrl+click gesture). See the multi-caret specs.
    pub fn add_active_caret(&mut self, off: usize) {
        self.docs[self.active].add_caret(off);
    }

    /// Collapse the active document to its single primary caret (`CARETONE` — a plain click / Escape).
    pub fn collapse_active_carets(&mut self) {
        self.docs[self.active].collapse_carets();
    }

    /// Alt+drag box-select: rebuild the caret set from the drag rectangle `(anchor_line, anchor_col)`
    /// → `(cursor_line, cursor_col)`. Called live on each drag update.
    pub fn set_box_active_carets(&mut self, anchor_line: usize, anchor_col: usize, cursor_line: usize, cursor_col: usize) {
        self.capture_last_selection();
        self.docs[self.active].set_box_carets(anchor_line, cursor_line, anchor_col, cursor_col);
    }

    /// Move the active document's caret to byte offset `off` with motion-selection
    /// semantics (`extend` grows/keeps the selection). Used by the GUI's visual
    /// wrapped-line Up/Down.
    pub fn move_active_to(&mut self, off: usize, extend: bool) {
        if !extend {
            self.capture_last_selection(); // a non-extending visual move clears the selection
        }
        let p = self.persistent;
        self.docs[self.active].set_persistent(p);
        self.docs[self.active].move_to_offset(off, extend);
    }


    /// Read a document's **raw bytes** (virtual overlay first, then disk). Unlike
    /// [`read_file`](Host::read_file) (UTF-8 text for kbd/lib), this imposes no
    /// encoding requirement, so legacy/binary files open fine. The virtual
    /// overlay holds text (tests use ASCII), exposed here as its bytes.
    fn read_doc_bytes(&mut self, path: &str) -> VmResult<Vec<u8>> {
        if let Some(text) = self.files.get(path) {
            return Ok(text.clone().into_bytes());
        }
        if self.virtual_fs {
            return Err(XplError::trappable_code(ErrCode::FileNotFound, self.tr_fmt("msg.open.not_found", "file not found: {path}", &[("path", &path)])));
        }
        cloud_only_guard(Path::new(path))?;
        std::fs::read(path).map_err(|e| {
            use std::io::ErrorKind::*;
            XplError::trappable(match e.kind() {
                PermissionDenied => format!("{path}: permission denied"),
                NotFound => format!("{path}: not found"),
                _ => format!("cannot read {path}: {e}"),
            })
        })
    }

    /// The current appearance/layout settings as a runnable XPL config script — what `SAVECFG`
    /// writes and `LOADCFG` runs back. Captures colours, sizing, view toggles, and DIR-listing
    /// behavior (`DIRNW`/`DIRRT`); deliberately excludes session/security flags (`TRUST`/`PS`) and
    /// per-document state (`RO`).
    /// The `LoadCM "..."` line that restores the current command history — one quoted, `\n`-joined
    /// arg with each command's backslashes + quotes escaped so it round-trips through `LoadCM`'s
    /// literal decoding. `None` when the history is empty. Shared by `config_script` (SAVECFG) and
    /// the `SaveCM` history-only save.
    /// Record a just-closed file at the top of the reopen list (newest first), deduped by path,
    /// capped at [`REOPEN_CAP`]. Only named, reopenable files should reach here.
    fn record_closed(&mut self, path: PathBuf, caret: usize) {
        self.recently_closed.retain(|(p, _)| p != &path);
        self.recently_closed.insert(0, (path, caret));
        self.recently_closed.truncate(REOPEN_CAP);
    }

    /// The `LoadClosed "..."` line that restores the reopen list — `<caret>⇥<path>` per entry,
    /// newline-joined, all escaped so it round-trips through `LoadClosed`'s literal decoding. `None`
    /// when the list is empty. Shared by `config_script` (SAVECFG) and the `SaveClosed` save.
    fn reopen_loadclosed_line(&self) -> Option<String> {
        if self.recently_closed.is_empty() {
            return None;
        }
        let mut blob = String::new();
        for (i, (path, caret)) in self.recently_closed.iter().enumerate() {
            if i > 0 {
                blob.push('\n');
            }
            blob.push_str(&caret.to_string());
            blob.push('\t');
            blob.push_str(&path.to_string_lossy());
        }
        // Escape backslash first, then quote, then the tab/newline separators (so a path's `\`s and
        // the structural `\t`/`\n` all survive the quoted-string lexing on load).
        let esc = blob
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\t', "\\t")
            .replace('\n', "\\n");
        Some(format!("LoadClosed \"{esc}\""))
    }

    /// The full `SaveClosed` snapshot text (comment header + the `LoadClosed "…"` line).
    fn reopen_script_text(&self) -> String {
        let body = self.reopen_loadclosed_line().unwrap_or_else(|| "LoadClosed \"\"".to_string());
        format!("# XyScript reopen list (recently-closed files, newest first)\n{body}\n")
    }

    fn history_loadcm_line(&self) -> Option<String> {
        if self.command_history.is_empty() {
            return None;
        }
        // Escape backslash first, then the quote, then any LITERAL control chars an entry may hold
        // (a recorded path/command can contain a real CR/TAB/newline). Without escaping the control
        // chars, a literal newline in an entry would split the `LoadCM "…"` across physical lines, so
        // re-`RUN`ning the saved file mis-parses the tail as commands (`EW: unknown switch …`) and a
        // fatal error aborts the rest of the caller's script (e.g. a startup that restores history).
        // Entries are joined by a `\n` separator ADDED AFTER this per-entry escaping.
        let joined = self
            .command_history
            .iter()
            .map(|c| {
                c.replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\t', "\\t")
                    .replace('\r', "\\r")
                    .replace('\n', "\\n")
            })
            .collect::<Vec<_>>()
            .join("\\n");
        Some(format!("LoadCM \"{joined}\""))
    }

    /// The full `SaveCM` snapshot text (comment header + the `LoadCM "…"` line). Shared by the
    /// file-writing `SaveHistory` op and the `CAPTURE`-variant `SaveHistoryToString`.
    fn history_script_text(&self) -> String {
        let body = self.history_loadcm_line().unwrap_or_else(|| "LoadCM \"\"".to_string());
        format!("# XyScript command history (newest first)\n{body}\n")
    }

    /// The `SAVEAR` file body: `key=replacement` lines (escapes encoded), sorted; or a commented
    /// sample when nothing is loaded. Shared by the file write and the `CAPTURE` (`ToString`) path.
    fn ar_dump_text(&self) -> String {
        if self.auto_replace.is_empty() {
            return AR_SAMPLE.to_string();
        }
        let mut pairs: Vec<_> = self.auto_replace.iter().collect();
        pairs.sort();
        let mut s = String::from("# XyScript auto-replace pairs (key=replacement)\n\n");
        for (k, v) in pairs {
            s.push_str(&format!("{k}={}\n", encode_ar_escapes(v)));
        }
        s
    }

    /// Serialize the non-empty TEXT registers (`@A`..`@Z`/`@0`..`@9`, set by `LDTX`) as a runnable
    /// XPL program of `LDTX <slot> "<literal>"` lines (sorted by slot, escaped so they round-trip
    /// through the quoted-literal parser). `SAVELDTX` writes this; `RUN`ning it restores the registers.
    /// Script binders (`&`, `LDSC`) are intentionally excluded — this is text-only.
    fn text_binder_script_text(&self) -> String {
        let mut s = String::new();
        let mut text: Vec<_> = self.text_binders.iter().filter(|(_, v)| !v.is_empty()).collect();
        text.sort_by_key(|(c, _)| **c);
        for (slot, content) in text {
            s.push_str(&format!("LDTX {slot} \"{}\"\n", escape_binder_literal(content)));
        }
        s
    }

    /// Serialize the non-empty SCRIPT registers (`&A`..`&Z`/`&0`..`&9`, set by `LDSC`/`LDPM`) as a
    /// runnable XPL program of `LDSC <slot> "<xpl>"` lines (sorted, escaped). `SAVELDSC`/`SAVELDPM`
    /// writes this; `RUN`ning it restores the registers. Text binders (`@`, `LDTX`) are excluded.
    fn script_binder_script_text(&self) -> String {
        let mut s = String::new();
        let mut script: Vec<_> = self.script_binders.iter().filter(|(_, v)| !v.is_empty()).collect();
        script.sort_by_key(|(c, _)| **c);
        for (slot, content) in script {
            s.push_str(&format!("LDSC {slot} \"{}\"\n", escape_binder_literal(content)));
        }
        s
    }

    /// Emit the config as a re-runnable script. `full` = the COMPLETE template (the shipped
    /// `xyscript-default.cfg`): also lists the ~30 optional settings that are unset on a fresh
    /// instance — colours/fonts/styles at their `default`/`normal` no-op form, and the handful with
    /// no reset keyword (LANG/DIRSET/DECIMALS/ERRORLOGFILE/SPELLDICT/SPELLBIN/inherited sizes) as
    /// commented `#` template lines. So `LOADCFG` stays a true no-op yet every knob is visible and
    /// editable. `full = false` (SAVECFG/AUTOCFG) keeps the minimal, only-what's-customized form.
    /// `include_history` appends the command-history / reopen lists (manual SAVECFG only).
    pub(crate) fn config_script(&self, full: bool, include_history: bool) -> String {
        let mut s = String::from("# XyScript configuration (generated by SAVECFG)\n\n");
        // Each setting: a `#` comment describing it, the verb line, then a blank separator.
        let put = |s: &mut String, comment: &str, line: String| {
            s.push_str("# ");
            s.push_str(comment);
            s.push('\n');
            s.push_str(&line);
            s.push_str("\n\n");
        };
        // An OPTIONAL colour knob (`None` = follows the theme): its `r g b` when set; in `full` mode
        // its no-op `<VERB> default` when unset; nothing when unset and not `full`.
        // An OPTIONAL colour: emit `<verb> r g b` when set, else `<verb> default` (which reloads to the
        // unset/theme-following state). Emitted in BOTH minimal (`SAVECFG`) and full modes, so every
        // colour knob — including the ones that follow the editor/theme when unset — is visible and
        // discoverable in a saved config, and every emitted line round-trips. Every colour verb used
        // here accepts `default`.
        let put_opt_color =
            |s: &mut String, comment: &str, verb: &str, c: Option<(u8, u8, u8)>| match c {
                Some((r, g, b)) => put(s, comment, format!("{verb} {r} {g} {b}")),
                None => put(s, comment, format!("{verb} default")),
            };
        // A COMMENTED template line — shown only in `full` mode, for a setting whose verb has no
        // no-op reset form (so we can't emit a runnable default). Documents the knob; inert on load.
        let put_tmpl = |s: &mut String, comment: &str, line: &str| {
            if full {
                s.push_str("# ");
                s.push_str(comment);
                s.push_str("\n# ");
                s.push_str(line);
                s.push_str("\n\n");
            }
        };
        let (r, g, b) = self.fg_color;
        put(&mut s, "Editor text (foreground) colour -- R G B, 0-255", format!("FGCOLOR {r} {g} {b}"));
        let (r, g, b) = self.bg_color;
        put(&mut s, "Editor background colour -- R G B, 0-255", format!("BGCOLOR {r} {g} {b}"));
        let (r, g, b) = self.ui_fg_color;
        put(&mut s, "UI chrome text colour -- R G B, 0-255", format!("UIFGCOLOR {r} {g} {b}"));
        let (r, g, b) = self.ui_bg_color;
        put(&mut s, "UI chrome background colour -- R G B, 0-255", format!("UIBGCOLOR {r} {g} {b}"));
        let (r, g, b) = self.caret_color;
        put(&mut s, "Caret colour -- R G B, 0-255", format!("CRCOLOR {r} {g} {b}"));
        let (r, g, b) = self.cmline_caret_color;
        put(&mut s, "Command-line caret colour -- R G B, 0-255", format!("CMCRCOLOR {r} {g} {b}"));
        let (r, g, b) = self.sep_color;
        put(&mut s, "UI separator colour (panel rules + tab dividers) -- R G B, 0-255", format!("SEPCOLOR {r} {g} {b}"));
        let (r, g, b) = self.sel_bg_color;
        put(&mut s, "Selection background colour -- R G B, 0-255", format!("SELBGCOLOR {r} {g} {b}"));
        let (r, g, b) = self.sel_fg_color;
        put(&mut s, "Selected-text colour -- R G B, 0-255", format!("SELFGCOLOR {r} {g} {b}"));
        let (r, g, b) = self.find_bg_color;
        put(&mut s, "FIND highlight background colour -- R G B, 0-255", format!("FINDBGCOLOR {r} {g} {b}"));
        let (r, g, b) = self.find_fg_color;
        put(&mut s, "FIND-match text colour -- R G B, 0-255", format!("FINDFGCOLOR {r} {g} {b}"));
        let (r, g, b) = self.spell_color;
        put(&mut s, "Spelling underline colour -- R G B, 0-255", format!("SPELLCOLOR {r} {g} {b}"));
        let (r, g, b) = self.grammar_color;
        put(&mut s, "Grammar underline colour -- R G B, 0-255", format!("GRAMMARCOLOR {r} {g} {b}"));
        put(&mut s, "Spelling underline style -- dotted|solid|squiggle", format!("SPELLSTYLE {}", self.spell_style.name()));
        put(&mut s, "Grammar underline style -- dotted|solid|squiggle", format!("GRAMMARSTYLE {}", self.grammar_style.name()));
        let (r, g, b) = self.pane_border_color;
        put(&mut s, "Active split-pane border colour -- R G B, 0-255", format!("PANECOLOR {r} {g} {b}"));
        put(&mut s, "Language-check dialect", format!("DIALECT {}", self.lint_dialect.name()));
        // Spell-check engine — emit only when NOT the default (harper), plus the Hunspell knobs so a
        // hunspell setup restores fully. (Harper needs no dict/bin.)
        if self.spell_engine != crate::lint::SpellEngine::default() {
            put(&mut s, "Spell-check engine -- harper | hunspell", format!("SPELLENGINE {}", self.spell_engine.name()));
            put(&mut s, "Hunspell dictionaries (the -d value, e.g. en_US,de_DE)", format!("SPELLDICT {}", self.spell_dicts));
            put(&mut s, "Hunspell executable (name on PATH or a full path)", format!("SPELLBIN {}", self.spell_bin));
        } else if full {
            put(&mut s, "Spell-check engine -- harper | hunspell", "SPELLENGINE harper".to_string());
            put_tmpl(&mut s, "Hunspell dictionaries (hunspell only; the -d value, e.g. en_US,de_DE)", "SPELLDICT en_US");
            put_tmpl(&mut s, "Hunspell executable (hunspell only; name on PATH or a full path)", "SPELLBIN hunspell");
        }
        // UI localization catalog — emit only when one is loaded (default is built-in English).
        if let Some(path) = &self.lang_path {
            put(&mut s, "UI translation catalog (LANG) -- path to a key=value strings file", format!("LANG {path}"));
        } else {
            put_tmpl(&mut s, "UI translation catalog (LANG) -- path to a key=value strings file (default: built-in English)", "LANG path/to/catalog.lang");
        }
        // Tab-title backgrounds — the states the user set (others stay egui's theme; `default` clears).
        for (state, color) in
            [("active", self.tab_active_bg), ("hover", self.tab_hover_bg), ("inactive", self.tab_inactive_bg)]
        {
            match color {
                Some((r, g, b)) => put(&mut s, "Tab-title background -- R G B, 0-255", format!("TABBGCOLOR {state} {r} {g} {b}")),
                None if full => put(&mut s, "Tab-title background -- R G B, 0-255", format!("TABBGCOLOR {state} default")),
                None => {}
            }
        }
        // Tab close-× colour — emit only when set (default follows the UI foreground).
        put_opt_color(&mut s, "Tab close-× colour -- R G B, 0-255", "TABXCOLOR", self.tab_x_color);
        // `$CWD` path-display colours (defaults: dimmed UI text / transparent — `default` restores them).
        put_opt_color(&mut s, "$CWD path text colour -- R G B, 0-255", "PAFGCOLOR", self.path_fg_color);
        put_opt_color(&mut s, "$CWD path background colour -- R G B, 0-255", "PABGCOLOR", self.path_bg_color);
        put_opt_color(&mut s, "Preview-pane text colour (preview only, not TOPDF) -- R G B, 0-255", "PVFGCOLOR", self.preview_fg_color);
        put_opt_color(&mut s, "Preview-pane background colour (preview only, not TOPDF) -- R G B, 0-255", "PVBGCOLOR", self.preview_bg_color);
        put_opt_color(&mut s, "Scratchpad text colour -- R G B, 0-255", "SCRATCHFGCOLOR", self.scratch_fg_color);
        put_opt_color(&mut s, "Scratchpad background colour -- R G B, 0-255", "SCRATCHBGCOLOR", self.scratch_bg_color);
        let (r, g, b) = self.txt_fg_color;
        put(&mut s, "TXT dialog text colour -- R G B, 0-255", format!("TXTFGCOLOR {r} {g} {b}"));
        let (r, g, b) = self.txt_bg_color;
        put(&mut s, "TXT dialog background colour -- R G B, 0-255", format!("TXTBGCOLOR {r} {g} {b}"));
        put(&mut s, "Caret shape -- bar | block | underline", format!("CRSHAPE {}", self.caret_shape.name()));
        put(&mut s, "Command-line caret shape -- bar | block | underline", format!("CMCRSHAPE {}", self.cmline_caret_shape.name()));
        put(&mut s, "Command-line placement -- top | bottom | hidden", format!("CMPLACE {}", self.cmline_placement.name()));
        put(&mut s, "Status-line placement -- top | bottom", format!("STATUSLINE {}", if self.status_line_top { "top" } else { "bottom" }));
        put(&mut s, "Blank the command line after a command runs from it -- on | off", format!("CMBLANK {}", if self.cmline_blank { "on" } else { "off" }));
        put(&mut s, "Show the tab strip when only one file is open -- on | off", format!("SHOWONETAB {}", if self.show_one_tab { "on" } else { "off" }));
        put(&mut s, "Word-wrap long lines to the window -- on | off (off shows a horizontal scrollbar)", format!("WRAP {}", if self.wrap { "on" } else { "off" }));
        put(&mut s, "Hanging indent for wrapped continuation rows, in spaces (0 = flush; ignored when WRAPINDENT is on)", format!("WRAPHANG {}", self.wrap_hang));
        put(&mut s, "Default column width REFLOW hard-wraps to when given no width", format!("REFLOWWIDTH {}", self.reflow_width));
        put(&mut s, "Close the blank Untitled tab when a document is opened -- on | off", format!("UNTITLEDTAB {}", if self.untitled_auto_close { "on" } else { "off" }));
        put(&mut s, "Opening an already-open file switches to its tab instead of prompting -- on | off", format!("GOTOOPEN {}", if self.goto_open { "on" } else { "off" }));
        put(&mut s, "Bookmark gutter -- off | on (when bookmarked) | persist (always)", format!("MARKGUTTER {}", mark_gutter_word(self.mark_gutter)));
        put_opt_color(&mut s, "Bookmark gutter dot colour -- R G B, 0-255", "MARKFGCOLOR", self.mark_fg_color);
        put_opt_color(&mut s, "Line-number gutter text colour -- R G B, 0-255", "LNFGCOLOR", self.line_num_fg_color);
        put_opt_color(&mut s, "Line-number gutter background colour -- R G B, 0-255", "LNBGCOLOR", self.line_num_bg_color);
        put_opt_color(&mut s, "MSG box header text colour -- R G B, 0-255", "MSGHFGCOLOR", self.msg_header_fg_color);
        put_opt_color(&mut s, "MSG box background colour -- R G B, 0-255", "MSGBGCOLOR", self.msg_bg_color);
        put_opt_color(&mut s, "MSG box OK-button text colour -- R G B, 0-255", "MSGOKFGCOLOR", self.msg_ok_fg_color);
        put_opt_color(&mut s, "MSG box OK-button background colour -- R G B, 0-255", "MSGOKBGCOLOR", self.msg_ok_bg_color);
        put_opt_color(&mut s, "Bookmark gutter background colour -- R G B, 0-255", "MARKBGCOLOR", self.mark_bg_color);
        put(&mut s, "Reload a file automatically when changed on disk -- on | off", format!("AUTOREOPEN {}", if self.auto_reopen { "on" } else { "off" }));
        put(&mut s, "Log trappable + fatal errors to a file -- on | off", format!("ERRORLOG {}", if self.error_log { "on" } else { "off" }));
        if self.error_log_file.is_none() {
            put_tmpl(&mut s, "Error-log file path (default: xyscript-errors.log in $XSDIR)", "ERRORLOGFILE my-errors.log");
        }
        if let Some(f) = &self.error_log_file {
            put(&mut s, "Error-log file path (default: xyscript-errors.log in $XSDIR)", format!("ERRORLOGFILE {f}"));
        }
        put(&mut s, "Dim the window when it loses focus -- on | off", format!("DIMONBLUR {}", if self.dim_on_blur { "on" } else { "off" }));
        put(&mut s, "Close a tab back to the last-visited tab -- on | off", format!("CLOSEBACK {}", if self.close_back { "on" } else { "off" }));
        put(&mut s, "Highlight URLs and Ctrl+click to open them -- on | off", format!("URLHIGHLIGHT {}", if self.url_highlight { "on" } else { "off" }));
        put_opt_color(&mut s, "URL highlight text colour -- R G B, 0-255", "URLFGCOLOR", self.url_fg_color);
        put_opt_color(&mut s, "URL highlight background colour -- R G B, 0-255", "URLBGCOLOR", self.url_bg_color);
        put(&mut s, "Render EMPH + URL highlights in preview/TOPDF (MDA always renders) -- on | off", format!("PVMARKS {}", if self.preview_marks { "on" } else { "off" }));
        put(&mut s, "Underline the spaces between words in MDA underline styling -- on | off", format!("ULSPACES {}", if self.ul_spaces { "on" } else { "off" }));
        put(&mut s, "Working-directory path placement -- cmline | status", format!("PAPLACE {}", self.path_place.name()));
        put(&mut s, "Working-directory path -- max display width in characters", format!("PATHLEN {}", self.path_len));
        {
            // Extra text-file extensions for the File ▸ Open "Text files" filter (the built-in set is
            // always matched too). Emitted even when empty (`default`) so the setting is visible.
            let line = if self.text_file_exts.is_empty() {
                "TEXTFILES default".to_string()
            } else {
                let joined = self.text_file_exts.iter().map(|e| format!(".{e}")).collect::<Vec<_>>().join(",");
                format!("TEXTFILES {joined}")
            };
            put(&mut s, "Extra extensions the File > Open text-file filter matches -- .ext,.ext (default = built-in set only)", line);
        }
        // Syntax-highlight theme — emit only the kinds that differ from the built-in defaults:
        // a recolour `HLCOLOR <kind> r g b`, a clear `HLCOLOR <kind> default`, or a grammar-defined
        // (custom) kind `HLCOLOR <kind> r g b`. Built-ins first (stable chart order), then custom
        // kinds sorted by name, so the emitted config is reproducible.
        let defaults = crate::highlight::default_highlight_theme();
        for kind in crate::highlight::TokenKind::ALL {
            let cur = self.highlight_theme.get(&kind).copied();
            if cur != defaults.get(&kind).copied() {
                let line = match cur {
                    Some((r, g, b)) => format!("HLCOLOR {} {r} {g} {b}", kind.name()),
                    None => format!("HLCOLOR {} default", kind.name()),
                };
                put(&mut s, "Highlight colour", line);
            }
        }
        let mut custom: Vec<(String, (u8, u8, u8))> = self
            .highlight_theme
            .iter()
            .filter(|(k, _)| !defaults.contains_key(*k))
            .map(|(k, v)| (k.name().to_string(), *v))
            .collect();
        custom.sort();
        for (name, (r, g, b)) in custom {
            put(&mut s, "Highlight colour (grammar kind)", format!("HLCOLOR {name} {r} {g} {b}"));
        }
        // Syntax-highlight BACKGROUND colours (`HLCOLOR/BG`) — opt-in (no built-in defaults), so emit
        // every kind that has one: built-ins in chart order, then custom kinds sorted, like the fg block.
        for kind in crate::highlight::TokenKind::ALL {
            if let Some((r, g, b)) = self.highlight_bg.get(&kind).copied() {
                put(&mut s, "Highlight background colour", format!("HLCOLOR/BG {} {r} {g} {b}", kind.name()));
            }
        }
        let mut custom_bg: Vec<(String, (u8, u8, u8))> = self
            .highlight_bg
            .iter()
            .filter(|(k, _)| !crate::highlight::TokenKind::ALL.contains(k))
            .map(|(k, v)| (k.name().to_string(), *v))
            .collect();
        custom_bg.sort();
        for (name, (r, g, b)) in custom_bg {
            put(&mut s, "Highlight background colour (grammar kind)", format!("HLCOLOR/BG {name} {r} {g} {b}"));
        }
        // Syntax-highlight MODE attributes (`HLMODE`) — opt-in, so emit every kind that has one:
        // built-ins in chart order, then custom kinds sorted, like the colour blocks.
        for kind in crate::highlight::TokenKind::ALL {
            if let Some(&bits) = self.highlight_mode.get(&kind) {
                put(&mut s, "Highlight text attributes", format!("HLMODE {} {}", kind.name(), mda_attr_keywords(bits)));
            }
        }
        let mut custom_mode: Vec<(String, u8)> = self
            .highlight_mode
            .iter()
            .filter(|(k, _)| !crate::highlight::TokenKind::ALL.contains(k))
            .map(|(k, v)| (k.name().to_string(), *v))
            .collect();
        custom_mode.sort();
        for (name, bits) in custom_mode {
            put(&mut s, "Highlight text attributes (grammar kind)", format!("HLMODE {name} {}", mda_attr_keywords(bits)));
        }
        // Regex grammars (`HLLANG` + its `HLRULE`s) — re-runnable definitions, so reopening restores
        // them. HLLANG resets the grammar, so re-running this script is idempotent.
        for g in self.highlight_grammars.borrow().grammars() {
            let line = if g.exts.is_empty() {
                format!("HLLANG {}", g.name)
            } else {
                let exts = g.exts.iter().map(|e| format!(".{e}")).collect::<Vec<_>>().join(",");
                format!("HLLANG {} {exts}", g.name)
            };
            put(&mut s, "Highlight grammar", line);
            for rule in &g.rules {
                let verb = if rule.perl { "HLRULE/P" } else { "HLRULE" };
                put(&mut s, "Highlight rule", format!("{verb} {} \"{}\"", rule.kind, rule.source));
            }
        }
        if self.hl_auto {
            put(&mut s, "Auto-enable highlighting for recognized file types", "HLAUTO on".into());
        } else if full {
            put(&mut s, "Auto-enable highlighting for recognized file types -- on | off", "HLAUTO off".into());
        }
        put(&mut s, "UI zoom factor (whole-window scale)", format!("UISCALE {}", self.ui_scale));
        put(&mut s, "Chrome (menu/status/command-line) font size, points", format!("UIFONT {}", self.chrome_font));
        put(&mut s, "Editor text font size, points", format!("EDFONT {}", self.editor_font));
        match &self.editor_font_family {
            Some(fam) => put(&mut s, "Editor font family (monospaced; EDFAMILY)", format!("EDFAMILY {fam}")),
            None if full => put(&mut s, "Editor font family (monospaced; EDFAMILY; 'default' = built-in)", "EDFAMILY default".to_string()),
            None => {}
        }
        // Emitted unconditionally (even at its default) so a plain SaveCfg always surfaces it — matches
        // the user's "defaults visible in SaveCfg, like PVMARGIN" preference.
        match &self.editor_prop_family {
            Some(fam) => put(&mut s, "Editor PROPORTIONAL font family (prose; EDPROPFAMILY)", format!("EDPROPFAMILY {fam}")),
            None => put(&mut s, "Editor PROPORTIONAL font family (prose; EDPROPFAMILY; 'default' = none, prose stays monospace)", "EDPROPFAMILY default".to_string()),
        }
        if self.editor_font_style != FontStyle::Normal {
            put(&mut s, "Editor font style (EDSTYLE)", format!("EDSTYLE {}", self.editor_font_style.keyword()));
        } else if full {
            put(&mut s, "Editor font style -- normal | bold | italic | bolditalic", "EDSTYLE normal".to_string());
        }
        match &self.ui_font_family {
            Some(fam) => put(&mut s, "UI chrome font family (UIFAMILY)", format!("UIFAMILY {fam}")),
            None if full => put(&mut s, "UI chrome font family (UIFAMILY; 'default' = built-in)", "UIFAMILY default".to_string()),
            None => {}
        }
        if self.ui_font_style != FontStyle::Normal {
            put(&mut s, "UI chrome font style (UISTYLE)", format!("UISTYLE {}", self.ui_font_style.keyword()));
        } else if full {
            put(&mut s, "UI chrome font style -- normal | bold | italic | bolditalic", "UISTYLE normal".to_string());
        }
        put(&mut s, "TXT dialog text size, points", format!("TXTFONT {}", self.txt_font));
        put(&mut s, "TXT dialog zoom factor", format!("TXTSCALE {}", self.txt_scale));
        match &self.txt_font_family {
            Some(fam) => put(&mut s, "TXT window font family (TXTFAMILY)", format!("TXTFAMILY {fam}")),
            None if full => put(&mut s, "TXT window font family (TXTFAMILY; 'default' = built-in)", "TXTFAMILY default".to_string()),
            None => {}
        }
        if self.txt_font_style != FontStyle::Normal {
            put(&mut s, "TXT window font style (TXTSTYLE)", format!("TXTSTYLE {}", self.txt_font_style.keyword()));
        } else if full {
            put(&mut s, "TXT window font style -- normal | bold | italic | bolditalic", "TXTSTYLE normal".to_string());
        }
        // DIR/W window appearance — the overrides the user set (the rest inherit the editor).
        put_opt_color(&mut s, "DIR-window text colour -- R G B, 0-255", "DWFGCOLOR", self.dw_fg_color);
        put_opt_color(&mut s, "DIR-window background colour -- R G B, 0-255", "DWBGCOLOR", self.dw_bg_color);
        if let Some(pt) = self.dw_font {
            put(&mut s, "DIR-window text size, points", format!("DWFONT {pt}"));
        } else {
            put_tmpl(&mut s, "DIR-window text size, points (default: inherits the editor font)", "DWFONT 14");
        }
        if self.dw_scale != 1.0 {
            put(&mut s, "DIR-window zoom factor", format!("DWSCALE {}", self.dw_scale));
        } else if full {
            put(&mut s, "DIR-window zoom factor", "DWSCALE 1".to_string());
        }
        match &self.dw_font_family {
            Some(fam) => put(&mut s, "DIR-window font family (DWFAMILY)", format!("DWFAMILY {fam}")),
            None if full => put(&mut s, "DIR-window font family (DWFAMILY; 'default' = built-in)", "DWFAMILY default".to_string()),
            None => {}
        }
        match self.dw_font_style {
            Some(st) => put(&mut s, "DIR-window font style -- default | normal | bold | italic | bolditalic", format!("DWSTYLE {}", st.keyword())),
            None if full => put(&mut s, "DIR-window font style -- default | normal | bold | italic | bolditalic ('default' inherits the editor)", "DWSTYLE default".to_string()),
            None => {}
        }
        match &self.preview_font_family {
            Some(fam) => put(&mut s, "Markdown Preview/TOPDF body font (PVFAMILY)", format!("PVFAMILY {fam}")),
            None if full => put(&mut s, "Markdown Preview/TOPDF body font (PVFAMILY; 'default' = built-in)", "PVFAMILY default".to_string()),
            None => {}
        }
        if let Some(pt) = self.preview_font_size {
            put(&mut s, "Markdown Preview/TOPDF body size, points (PVSIZE)", format!("PVSIZE {pt}"));
        } else {
            put_tmpl(&mut s, "Markdown Preview/TOPDF body size, points (PVSIZE; default: built-in)", "PVSIZE 11");
        }
        match &self.preview_code_family {
            Some(fam) => put(&mut s, "Markdown Preview/TOPDF code font (PVCODE)", format!("PVCODE {fam}")),
            None if full => put(&mut s, "Markdown Preview/TOPDF code font (PVCODE; 'default' = built-in)", "PVCODE default".to_string()),
            None => {}
        }
        if let Some(pt) = self.preview_code_size {
            put(&mut s, "Markdown Preview/TOPDF code size, points (PVCODESIZE)", format!("PVCODESIZE {pt}"));
        } else {
            put_tmpl(&mut s, "Markdown Preview/TOPDF code size, points (PVCODESIZE; default: inherit body size)", "PVCODESIZE 10");
        }
        // Preview/TOPDF page margins (per side — emit all four so any mix round-trips). Each value is
        // a length (in/cm/mm/pt); a bare number means inches.
        let m = &self.preview_margins;
        put(&mut s, "Preview/TOPDF page margin -- left", format!("PVMARGINL {}", m.left));
        put(&mut s, "Preview/TOPDF page margin -- right", format!("PVMARGINR {}", m.right));
        put(&mut s, "Preview/TOPDF page margin -- top", format!("PVMARGINT {}", m.top));
        put(&mut s, "Preview/TOPDF page margin -- bottom", format!("PVMARGINB {}", m.bottom));
        // Preview/TOPDF page format, spacing, and page numbering. Emitted UNCONDITIONALLY (like the
        // page margins above), so the settings are visible in every saved config even at their
        // defaults. A `None` optional writes its no-op form (`default`/`off`), which reloads as a no-op.
        let or = |o: &Option<String>, dflt: &str| o.clone().unwrap_or_else(|| dflt.to_string());
        put(&mut s, "Preview/TOPDF page format (letter/legal/tabloid/a4/a5/a6)", format!("PVPAPER {}", self.preview_paper));
        put(&mut s, "Preview/TOPDF line spacing (em/pt/cm/mm/in; 'default' = Typst 0.65em)", format!("PVLEADING {}", or(&self.preview_leading, "default")));
        put(&mut s, "Preview/TOPDF extra space between paragraphs ('default' = Typst 1.2em)", format!("PVSPACING {}", or(&self.preview_spacing, "default")));
        put(&mut s, "Preview/TOPDF page numbers (pattern like '1', 'Page 1', '1 of 1'; 'off' = none)", format!("PVPAGENUM {}", or(&self.preview_page_num, "off")));
        put(&mut s, "Preview/TOPDF page-number position (e.g. bottom-center, top-right)", format!("PVPAGEPOS {}", self.preview_page_align));
        put(&mut s, "Preview/TOPDF justify body text (on/off)", format!("PVJUSTIFY {}", if self.preview_justify { "on" } else { "off" }));
        put(&mut s, "Preview/TOPDF first-line paragraph indent ('off' = none)", format!("PVINDENT {}", or(&self.preview_indent, "off")));
        put(&mut s, "Preview/TOPDF page header ('|' = left/center/right; {date} {file} {page} {pages}; 'off' = none)", format!("PVHEADER {}", or(&self.preview_header, "off")));
        put(&mut s, "Preview/TOPDF page footer ('|' = left/center/right; {date} {file} {page} {pages}; 'off' = none)", format!("PVFOOTER {}", or(&self.preview_footer, "off")));
        put(&mut s, "Line spacing, percent of the editor font", format!("LSP {}", self.leading));
        put(&mut s, "Left margin (offset), character columns", format!("OFL {}", self.margin_left as i64));
        put(&mut s, "Right margin (offset), character columns", format!("OFR {}", self.margin_right as i64));
        put(&mut s, "Menu bar -- SMB shows it, CMB hides it", if self.menu_bar { "SMB" } else { "CMB" }.to_string());
        put(&mut s, "Visible paragraph marks ¶ -- SVR on, CVR off", if self.visible_returns { "SVR" } else { "CVR" }.to_string());
        put(&mut s, "Visible whitespace (space dots / tab arrows) -- SWS on, CWS off", if self.visible_whitespace { "SWS" } else { "CWS" }.to_string());
        put(&mut s, "Line-number gutter -- SLN on, CLN off", if self.line_numbers { "SLN" } else { "CLN" }.to_string());
        put(&mut s, "Pop-up command line -- SGC on, CGC off", if self.gui_cmline { "SGC" } else { "CGC" }.to_string());
        put(&mut s, "Shell-output tab visibility -- SW on/off", format!("SW {}", if self.shell_output_visible { "on" } else { "off" }));
        put(&mut s, "Console prompt template (SHPROMPT; $CWD expands at display; quotes keep spaces)", format!("SHPROMPT \"{}\"", self.console_prompt));
        put(&mut s, "Colour Console output from ANSI codes -- on | off", format!("ANSICOLOR {}", if self.ansi_color { "on" } else { "off" }));
        put(&mut s, "Preview pane visibility -- PV on/off", format!("PV {}", if self.preview_visible { "on" } else { "off" }));
        put(&mut s, "Persistent selection -- SPS on, CPS off", if self.persistent { "SPS" } else { "CPS" }.to_string());
        put(&mut s, "OS window title bar -- on shows it, off uses a draggable strip", if self.show_titlebar { "TITLEBAR on" } else { "TITLEBAR off" }.to_string());
        put(&mut s, "Remember window size+position across runs -- off helps mixed-DPI multi-monitor setups", if self.remember_window_geometry { "WINGEO on" } else { "WINGEO off" }.to_string());
        put(&mut s, "Auto-save-and-restore the open tabs across runs (Notepad++-style)", if self.session_restore { "SESSION on" } else { "SESSION off" }.to_string());
        put(&mut s, "Open a file from a DIR listing in a new tab (on) or in place (off) -- DIRNW", format!("DIRNW {}", if self.new_window { "on" } else { "off" }));
        put(&mut s, "Open a file from the DIR/W window in a new tab (on) or the active tab (off) -- DIRWNW", format!("DIRWNW {}", if self.dir_window_new_tab { "on" } else { "off" }));
        put(&mut s, "Close the DIR/W window after opening a file (on) or keep it open (off) -- DIRWCLOSE", format!("DIRWCLOSE {}", if self.dir_window_close_on_open { "on" } else { "off" }));
        put(&mut s, "Reuse an already-open DIR listing tab (on) or open a new one per DIR (off) -- DIRRT", format!("DIRRT {}", if self.dir_reuse { "on" } else { "off" }));
        put(&mut s, "GREP leading context lines -- GREPBEFORE", format!("GREPBEFORE {}", self.grep_before));
        put(&mut s, "GREP trailing context lines -- GREPAFTER", format!("GREPAFTER {}", self.grep_after));
        put(&mut s, "Blank line between GREP result groups (on|off) -- GREPBLANK", format!("GREPBLANK {}", if self.grep_blank { "on" } else { "off" }));
        if !self.dir_defaults.is_empty() {
            put(&mut s, "Default DIR sort/filter/view switches -- DIRSET", format!("DIRSET {}", self.dir_defaults_string()));
        } else {
            put_tmpl(&mut s, "Default DIR sort/filter/view switches -- DIRSET (default: none)", "DIRSET /on /p");
        }
        put(
            &mut s,
            "Autosave interval, seconds (off = disabled)",
            if self.autosave_secs == 0 {
                "AUTOSAVE off".to_string()
            } else {
                format!("AUTOSAVE {}", self.autosave_secs)
            },
        );
        put(&mut s, "Tab width, spaces", format!("TB {}", self.tab_width));
        put(&mut s, "Soft tabs -- insert spaces for tabs", format!("TABSPACES {}", if self.tab_soft { "on" } else { "off" }));
        put(&mut s, "Auto-indent new lines on Enter", format!("TABINDENT {}", if self.tab_indent { "on" } else { "off" }));
        put(&mut s, "Highlight matching brackets", format!("BALPAIRS {}", if self.bal_pairs { "on" } else { "off" }));
        put(&mut s, "Auto-close typed bracket/quote pairs", format!("TYPEPAIRS {}", if self.type_pairs { "on" } else { "off" }));
        put(&mut s, "Hanging indent for soft-wrapped lines", format!("WRAPINDENT {}", if self.wrap_indent { "on" } else { "off" }));
        put_opt_color(&mut s, "Matched-bracket highlight colour -- R G B, 0-255", "BALPAIRSCOLOR", self.bal_color);
        put_opt_color(&mut s, "EMPH emphasis text colour -- R G B, 0-255", "EMPHFGCOLOR", self.emph_fg);
        put_opt_color(&mut s, "EMPH emphasis background colour -- R G B, 0-255", "EMPHBGCOLOR", self.emph_bg);
        if let Some(n) = self.float_decimals {
            put(&mut s, "Fixed decimal places for float display", format!("DECIMALS {n}"));
        } else {
            put_tmpl(&mut s, "Fixed decimal places for float display (default: full precision)", "DECIMALS 2");
        }
        if include_history {
            if let Some(line) = self.history_loadcm_line() {
                put(&mut s, "Command history (newest first)", line);
            }
            if let Some(line) = self.reopen_loadclosed_line() {
                put(&mut s, "Reopen list (recently-closed files, newest first)", line);
            }
        }
        // Binders (key registers): emit a LDTX/LDSC line per non-empty slot (sorted, escaped as a
        // quoted literal) so they round-trip — LoadCfg just runs them.
        if !self.text_binders.is_empty() || !self.script_binders.is_empty() {
            s.push_str("# Binders (key registers): @slot = text, &slot = XPL\n");
            let mut text: Vec<_> = self.text_binders.iter().collect();
            text.sort_by_key(|(c, _)| **c);
            for (slot, content) in text {
                s.push_str(&format!("LDTX {slot} \"{}\"\n", escape_binder_literal(content)));
            }
            let mut script: Vec<_> = self.script_binders.iter().collect();
            script.sort_by_key(|(c, _)| **c);
            for (slot, content) in script {
                s.push_str(&format!("LDSC {slot} \"{}\"\n", escape_binder_literal(content)));
            }
            s.push('\n');
        }
        // File-output: OS-appropriate newlines. This is the single point feeding SAVECFG + both
        // AUTOCFG write paths, so the change-detection compare (`config_last_written`) stays
        // consistent (it stores this same normalized text).
        to_os_newlines(&s)
    }

    /// Save every named, modified buffer byte-exact (`SL`/`SF`). Unnamed (never-saved)
    /// buffers are skipped and left untouched. Returns `(saved, skipped)`.
    fn save_all_modified(&mut self) -> VmResult<(usize, usize)> {
        let mut saved = 0;
        let mut skipped = 0;
        for i in 0..self.docs.len() {
            if !self.docs[i].is_modified() {
                continue;
            }
            let path = match self.docs[i].path() {
                Some(p) => p.to_string_lossy().into_owned(),
                None => {
                    skipped += 1;
                    continue;
                }
            };
            let bytes = self.docs[i].save_bytes();
            self.write_doc_bytes(&path, &bytes)?;
            self.sync_color_sidecar(&self.docs[i], &path, &bytes);
            self.docs[i].mark_saved();
            saved += 1;
        }
        Ok((saved, skipped))
    }

    /// Write a document's **raw bytes** (byte-exact). The disk path is atomic
    /// (temp + rename) so a failed write can't corrupt the original. The virtual
    /// overlay stores them lossily as text (fine for the ASCII test fixtures).
    fn write_doc_bytes(&mut self, path: &str, bytes: &[u8]) -> VmResult<()> {
        if self.virtual_fs {
            self.files
                .insert(path.to_string(), String::from_utf8_lossy(bytes).into_owned());
            return Ok(());
        }
        // Relative paths resolve against the editor's working dir ($CWD), like every other
        // file op (absolutes pass through) — so `TOPDF out.pdf`/`SAD sel.txt` land in $CWD,
        // not the process cwd. Callers that already resolved (SAVE/SAAS) pass absolutes, which
        // resolve_path returns unchanged.
        let target = self.resolve_path(path);
        let mut tmp_os = target.as_os_str().to_os_string();
        tmp_os.push(".xyscript-tmp");
        let tmp = PathBuf::from(tmp_os);
        // Atomic + DURABLE save: write to a temp file, FLUSH IT TO DISK (`sync_all`), then rename
        // over the target. The fsync matters on cloud-synced folders (Dropbox/OneDrive) and across a
        // crash/power-loss — without it a "saved" file can be left empty or stale because the bytes
        // sat only in the OS write cache (the reported "saved but didn't actually save"). Every OS
        // failure (no space, permission denied, missing directory, file locked) becomes a trappable
        // error, so it reaches `$ERROR` and the status line rather than passing silently.
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| XplError::trappable(format!("cannot write {path}: {e}")))?;
        if let Err(e) = f.write_all(bytes).and_then(|()| f.sync_all()) {
            drop(f);
            let _ = std::fs::remove_file(&tmp);
            return Err(XplError::trappable(format!("cannot write {path}: {e}")));
        }
        drop(f); // close before rename (Windows won't rename an open handle)
        std::fs::rename(&tmp, &target).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            XplError::trappable(format!("cannot finalize {path}: {e}"))
        })?;
        Ok(())
    }

    /// After a full-document save, mirror `doc`'s `MDA` / `FGCOLOR` block colours into its hidden
    /// `.xymeta` sidecar (or remove the sidecar when the doc has none). `path` is the SAME string
    /// handed to [`write_doc_bytes`](Self::write_doc_bytes), resolved identically, and `bytes` are
    /// the bytes just written — together they key the sidecar's content guard. Best-effort: a
    /// sidecar problem never turns a good save into a failure, and the virtual test FS keeps none.
    fn sync_color_sidecar(&self, doc: &Document, path: &str, bytes: &[u8]) {
        if self.virtual_fs {
            return;
        }
        let target = self.resolve_path(path);
        let (fg, bg) = doc.color_spans_raw();
        xymeta_write(&target, bytes, fg, bg, doc.attr_spans_raw(), &doc.marks_for_persist());
    }

    /// Build the status line for a `MARKNEXT`/`MARKPREV` jump and request the target be scrolled into
    /// view. `None` (no bookmarks) is a trappable error.
    fn mark_nav_status(&mut self, nav: Option<crate::MarkNav>) -> VmResult<Value> {
        match nav {
            Some(nav) => {
                self.reveal_center = true; // scroll the target to mid-screen, like a search landing
                let moved = if nav.moved { " — moved" } else { "" };
                Ok(Value::str(format!("bookmark {}/{}{moved}", nav.index, nav.total)))
            }
            None => Err(XplError::trappable("no bookmarks")),
        }
    }
}

impl Host for AppHost {
    fn random_int(&self, min: i64, max: i64) -> Option<i64> {
        // SplitMix64: advance the state by the golden-ratio increment, then scramble it to an
        // output word. Good statistical quality, tiny state, deterministic from a seed.
        let mut z = self.rng.get().wrapping_add(0x9E37_79B9_7F4A_7C15);
        self.rng.set(z);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        let bits = z ^ (z >> 31);
        // Map into the INCLUSIVE range [lo, hi] (args swapped if reversed). i128 avoids overflow on
        // very wide spans (e.g. i64::MIN..i64::MAX); the modulo bias is negligible at real ranges.
        let (lo, hi) = if min <= max { (min, max) } else { (max, min) };
        let span = (hi as i128 - lo as i128 + 1) as u128; // >= 1
        Some((lo as i128 + (bits as u128 % span) as i128) as i64)
    }

    fn seed_random(&self, seed: u64) {
        self.rng.set(seed);
    }

    fn load_kbd(&mut self, path: &str) -> VmResult<()> {
        // Read the file (virtual FS first, then disk), then parse + validate.
        // On any failure the operative keymap is left untouched and the error is
        // trappable ($ERROR), matching the design's transactional / last-known-
        // good rule (§9.2/§9.3).
        let body = self.read_file(path)?;
        match load(&body, VIRTUAL_BASE) {
            LoadOutcome::Installed(km) => {
                // A plain LOADKBD (re)loads the ACTIVE layout slot, so switching away and back stays
                // consistent. Non-mode users only ever touch the single `default` slot.
                self.kbd_modes.insert(self.kbd_mode.clone(), km.clone());
                self.pending_kbd = Some(km);
                self.last_kbd = Some(self.resolve_path(path)); // $KB (resolved full path)
                self.last_kbd_error = None; // a good load clears any prior failure
                Ok(())
            }
            LoadOutcome::Rejected(e) => {
                let msg = format!("LOADKBD {path}: {e}");
                self.last_kbd_error = Some(msg.clone()); // survives to the startup status (see field doc)
                Err(XplError::trappable(msg))
            }
        }
    }

    fn load_kbd_named(&mut self, name: &str, path: &str) -> VmResult<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(XplError::trappable("ADDKBD: a layout name is required (ADDKBD <name> <file>)"));
        }
        let body = self.read_file(path)?; // virtual FS first, then disk (same as load_kbd)
        match load(&body, VIRTUAL_BASE) {
            LoadOutcome::Installed(km) => {
                // Store the compiled map in its named slot. If it IS the active layout, also install it
                // operatively (editor post_run picks up pending_kbd); otherwise just keep it resident.
                if name == self.kbd_mode {
                    self.pending_kbd = Some(km.clone());
                }
                self.kbd_modes.insert(name.to_string(), km);
                self.last_kbd = Some(self.resolve_path(path));
                self.last_kbd_error = None;
                Ok(())
            }
            LoadOutcome::Rejected(e) => {
                let msg = format!("ADDKBD {name} {path}: {e}");
                self.last_kbd_error = Some(msg.clone());
                Err(XplError::trappable(msg))
            }
        }
    }

    fn set_kbd_mode(&mut self, name: &str) -> VmResult<()> {
        let name = name.trim();
        match self.kbd_modes.get(name) {
            Some(km) => {
                let km = km.clone();
                self.kbd_mode = name.to_string();
                self.pending_kbd = Some(km); // editor post_run installs into resolver + GUI capture
                Ok(())
            }
            None => Err(XplError::trappable(format!(
                "KBDMODE: no keyboard layout named '{name}' — load it with ADDKBD {name} <file>"
            ))),
        }
    }

    fn last_scan_code(&self) -> i64 {
        self.last_scan
    }

    fn last_modifier_bits(&self) -> i64 {
        self.last_mod_bits
    }

    fn key_binding(&self, scan: i64) -> Option<String> {
        // The argument may carry modifier bits ABOVE the key-code range, as `$SCC` does: key codes are
        // 0..=255 and the KBD parser assigns modifier bits ≥ 256 (CTRL=256, ALT=512, …) so codes and
        // bits never collide. Split the low byte (the key) from the high bits (the modifier layer):
        // `KEY($SC)` has no high bits → the unshifted layer (unchanged); `KEY($SCC)` resolves the
        // Shift/Ctrl/Alt layer the key was pressed on.
        if scan < 0 {
            return None;
        }
        let code = (scan & 0xFF) as u8;
        let bitmask = u32::try_from(scan & !0xFF).ok()?;
        match self.current_keymap.as_ref()?.resolve(bitmask, code)? {
            xyscript_kbd::Assignment::Flip => Some("KF".to_string()),
            xyscript_kbd::Assignment::Xpl(s) => Some(s.clone()),
        }
    }

    fn scc_key_name(&self, scc: i64) -> Option<String> {
        if scc < 0 {
            return None;
        }
        // `$SCC` = key code (low byte, 0..=255) + modifier bits (≥256), the same split `KEY($SCC)` uses.
        let code = (scc & 0xFF) as u8;
        let bitmask = u32::try_from(scc & !0xFF).ok()?;
        let km = self.current_keymap.as_ref()?;
        // Every set modifier bit must belong to a declared modifier — otherwise this isn't a valid
        // `$SCC` for the current keymap, so name nothing.
        let known: u32 = km.modifiers.iter().fold(0, |acc, m| acc | m.bit);
        if bitmask & !known != 0 {
            return None;
        }
        // Base key name — mirrors the GUI's `describe_key`/`key_name`, in priority order:
        //   1. a NAMED key by canonical code (Enter, Backspace, F3, Up, Space, …);
        //   2. the keymap's OWN character for a printable key (its `insert "X"` cell — layout-aware);
        //   3. the US key-cap character (for a printable the keymap leaves unbound).
        // A key that is none of these can't be named → the whole `$SCC` is invalid.
        let base = canonical_key_name(code)
            .or_else(|| {
                let cell = self.key_binding(i64::from(code))?; // Normal-layer operative cell
                key_cell_char(&cell).map(|c| c.to_uppercase().to_string())
            })
            .or_else(|| canonical_printable(code).map(str::to_string))?;
        // Modifier prefix, ascending by bit value (matches `describe_key`), using the keymap's own
        // modifier names so a custom KBD's modifiers (e.g. a `HOTKEY`) prefix correctly.
        let mut mods: Vec<_> = km.modifiers.iter().filter(|m| bitmask & m.bit != 0).collect();
        mods.sort_by_key(|m| m.bit);
        let mut out = String::new();
        for m in mods {
            out.push_str(&m.name);
            out.push('+');
        }
        out.push_str(&base);
        Some(out)
    }

    fn keyboard_chart_text(&self) -> String {
        let tables = self.keyboard_tables();
        if tables.is_empty() {
            return "No keymap loaded.".to_string();
        }
        let mut s = String::new();
        for (name, entries) in tables {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&format!("{name}  ({} key{})\n", entries.len(), if entries.len() == 1 { "" } else { "s" }));
            for (code, binding) in entries {
                s.push_str(&format!("  {code:>4}  {binding}\n"));
            }
        }
        s
    }

    fn path_is_dir(&self, path: &str) -> bool {
        // Resolve against the editor's working dir ($CWD), like every other file op, then stat it.
        // (The virtual FS used in tests has no directories, so this is real-disk only.)
        !self.virtual_fs && self.resolve_path(path).is_dir()
    }

    fn path_exists(&self, path: &str) -> bool {
        if self.virtual_fs {
            self.files.contains_key(path)
        } else {
            self.resolve_path(path).exists()
        }
    }

    fn take_ar_xpl(&mut self) -> Option<String> {
        self.ar_pending_xpl.take()
    }

    fn editor_op(&mut self, op: EditorOp) -> VmResult<Value> {
        // A document-focusing op (text edit, search, file/screen navigation, file/tab op, …) means a
        // cmline-submitted command should land the caret in the text. Recorded here (set optimistically
        // — an op that then errors is overridden by the `$ERROR`→cmline rule in the submit path).
        if is_document_focusing(&op) {
            self.doc_focus_ran = true;
        }
        // Capture the live selection into `$SEL` BEFORE running the op, so a selection's contents
        // survive the op that clears it (the mouse/visual-arrow paths capture in their own methods).
        self.capture_last_selection();
        // Sync the global persistent-selection flag onto the target buffer before any
        // op, so motion/selection behave per the current mode (single chokepoint).
        {
            let p = self.persistent;
            self.active().set_persistent(p);
        }
        // Read-only gate: when the active *document* is locked (`RO`), reject buffer-content
        // edits. The cmline is never locked, so commands still run while text-focus is on a
        // read-only doc only blocks edits to that doc.
        if matches!(self.target, Target::Document)
            && self.blind.is_none() // blind-edit buffers are always writable (the RO lock is the visible doc's)
            && op_mutates_buffer(&op)
        {
            if self.docs[self.active].read_only() {
                return Err(XplError::trappable(self.tr_msg("msg.readonly", "document is read-only (RO to unlock)")));
            }
            // In the hex view, TEXT edits (typing, newline, paste-as-text) don't make sense and would
            // corrupt the grid, so they're rejected — but the dedicated HEX editing ops (nibble /
            // overwrite / byte insert-delete / byte paste) ARE the intended editing, and undo/redo
            // replay recorded byte edits, so all of those pass through.
            // Byte edits, undo/redo replay, AND the clipboard ops (which get a byte branch in
            // `do_clip`) are the hex-view editing surface; everything else that mutates is a text edit.
            let hex_ok = is_hex_edit_op(&op)
                || matches!(op, EditorOp::Undo(_) | EditorOp::Redo(_) | EditorOp::Clip(_));
            if self.docs[self.active].hex_view() && !hex_ok {
                return Err(XplError::trappable(
                    self.tr_msg("msg.hex_readonly", "hex view is read-only for text edits (HEX off)"),
                ));
            }
        }
        match op {
            EditorOp::InsertText(s) => {
                // A KBD-bound character insert (e.g. the spacebar, `57=" "`) comes through here rather
                // than `TypeText`, so the `MDA` typing mode must style it too — otherwise typed spaces
                // (and any user-bound literal keys) would break the run's styling. Document target only.
                let from = matches!(self.target, Target::Document)
                    .then(|| self.active().buffer().cursor());
                self.active().insert(&s);
                if let Some(from) = from {
                    self.active().apply_typing_style(from);
                }
            }
            EditorOp::ReplaceSelection(s) => {
                // Overwrite the selected span with `s` as one undo unit, regardless of persistent-
                // selection mode (`replace_span` operates on the explicit span, so PS can't keep it).
                // Empty `s` deletes the selection. No selection → nothing to replace.
                if let Some((a, b)) = self.active().buffer().selection_span() {
                    self.active().replace_span(a, b, &s);
                }
            }
            EditorOp::Reflow { width } => {
                // Hard-wrap the selection, else the paragraph at the caret, to `width` columns as one
                // undo unit. `paragraph_span` returns an empty range on a blank line (nothing to do).
                let nl = self.active().line_ending().as_str().to_string();
                let (a, b) = {
                    let buf = self.active().buffer();
                    buf.selection_span().unwrap_or_else(|| buf.paragraph_span())
                };
                if a < b {
                    let text = self.active().buffer().slice(a, b);
                    let reflowed = reflow_text(&text, width, &nl);
                    if reflowed != text {
                        self.active().replace_span(a, b, &reflowed);
                    }
                }
            }
            EditorOp::TypeText(s) => {
                // Interactive typing: apply TYPEPAIRS to a single bracket/quote char in the document;
                // everything else (multi-char, cmline focus, mode off) is a plain insert.
                let style_from = matches!(self.target, Target::Document)
                    .then(|| self.active().buffer().cursor());
                let paired = self.type_pairs
                    && matches!(self.target, Target::Document)
                    && {
                        let mut it = s.chars();
                        match (it.next(), it.next()) {
                            (Some(c), None) => self.active().type_pair(c),
                            _ => false,
                        }
                    };
                if !paired {
                    self.active().insert(&s);
                }
                // `MDA` typing mode: style the run just typed (in the document only).
                if let Some(from) = style_from {
                    self.active().apply_typing_style(from);
                }
            }
            EditorOp::Delete(DeleteKind::Back) => {
                // TYPEPAIRS: Backspace between an empty auto-pair deletes both members.
                let handled = self.type_pairs
                    && matches!(self.target, Target::Document)
                    && self.active().try_delete_pair();
                if !handled {
                    self.active().delete_back();
                }
            }
            EditorOp::Delete(DeleteKind::Forward) => self.active().delete_forward(),
            EditorOp::Delete(DeleteKind::Selection) => {
                self.active().delete_selection();
            }
            EditorOp::Delete(DeleteKind::ToEol) => self.active().delete_to_eol(),
            EditorOp::Delete(DeleteKind::Word) => self.active().delete_word(),
            EditorOp::Delete(DeleteKind::Line) => self.active().delete_line(),
            EditorOp::Delete(DeleteKind::VisualLine) => {
                // Use the GUI-shaped visual-line span only if it's for the CURRENT text (revision
                // matches — no edit since it was computed); otherwise `delete_visual_line` sees `None`
                // and falls back to the logical line. The caret-containment check lives there too.
                let span = (self.visual_line_rev == self.active_doc().revision())
                    .then_some(self.visual_line_span)
                    .flatten();
                self.active().delete_visual_line(span);
                self.visual_line_span = None; // consumed; the GUI recomputes it next frame
            }
            EditorOp::Delete(DeleteKind::Sentence) => self.active().delete_sentence(),
            EditorOp::Delete(DeleteKind::Paragraph) => self.active().delete_paragraph(),
            EditorOp::Move { motion, extend } if self.active_doc().hex_view() => {
                // HEX view navigates raw BYTES, not logical text: Left/Right = ±1 byte, Up/Down = ±16
                // (one grid row), Home/End = the row's edges, PgUp/PgDn = ±(page × 16), Doc edges = file
                // ends. Word/paragraph/sentence motions have no byte meaning, so they nudge one byte (the
                // keys stay live rather than dead). Only the PRIMARY caret moves — hex is single-caret.
                let (vtop, vrows) = (self.viewport.0, self.viewport.1.max(1));
                let page = (vrows.saturating_sub(1)).max(1);
                let row = crate::hex::BYTES_PER_ROW;
                let (cur, len) = {
                    let b = self.active().buffer();
                    (b.cursor(), b.len())
                };
                let row_start = cur - (cur % row);
                let target: usize = match motion {
                    Motion::Left | Motion::LeftSameLine => cur.saturating_sub(1),
                    Motion::Right | Motion::RightSameLine => (cur + 1).min(len),
                    Motion::Up | Motion::VisualUp | Motion::PrevLineStart | Motion::PrevPara => cur.saturating_sub(row),
                    Motion::Down | Motion::VisualDown | Motion::NextLineStart | Motion::NextPara => (cur + row).min(len),
                    Motion::LineStart | Motion::VisualLineStart => row_start,
                    Motion::LineEnd | Motion::VisualLineEnd => (row_start + row - 1).min(len),
                    Motion::DocStart => 0,
                    Motion::DocEnd => len,
                    Motion::PageDown => (cur + page * row).min(len),
                    Motion::PageUp => cur.saturating_sub(page * row),
                    Motion::ScreenTop => (vtop * row).min(len),
                    Motion::ScreenBottom => ((vtop + vrows - 1) * row + row - 1).min(len),
                    Motion::ToChar(n) => n.min(len),
                    Motion::ByChar(d) => {
                        if d >= 0 {
                            (cur + d as usize).min(len)
                        } else {
                            cur.saturating_sub((-d) as usize)
                        }
                    }
                    Motion::NextWord | Motion::NextSentence => (cur + 1).min(len),
                    Motion::PrevWord | Motion::PrevSentence => cur.saturating_sub(1),
                };
                self.active().move_primary_to_byte(target, extend);
                // No visual-caret refinement in hex view; clear any stale intent from a prior text move.
                self.pending_visual_edge = None;
                self.pending_visual_move = None;
            }
            EditorOp::Move { motion, extend } => {
                // Read the viewport hint before borrowing the active document.
                let (vtop, vrows) = (self.viewport.0, self.viewport.1.max(1));
                let page = (vrows as i32 - 1).max(1);
                // Any move resets the pending visual-edge request; only the two visual arms set it
                // (so a later move in the same frame, e.g. a chained `LB; RIGHT`, cancels it).
                let mut visual_edge: Option<(bool, bool, usize)> = None;
                // `CARETMOTION visual`: when on, Left/Right defer to the GUI for a visual (screen-order)
                // move. Read the flag before borrowing the active document (the match below borrows `self`).
                let caret_visual = self.caret_visual;
                let mut visual_move: Option<(bool, bool, usize)> = None;
                // `LU`/`LD` (VisualUp/VisualDown): step the primary over the GUI's visual-row map when
                // it's FRESH (same doc revision) — so they move by screen row inside `;`-chains, macros
                // and GUI-run scripts, not just the bare interactive form the GUI intercepts. Snapshot
                // the map before borrowing the doc mutably; motions are infrequent and it's window-sized.
                let visual_rows = if matches!(motion, Motion::VisualUp | Motion::VisualDown)
                    && self.visual_rows_rev == self.active_doc().revision()
                    && self.visual_rows.len() >= 2
                {
                    Some(self.visual_rows.clone())
                } else {
                    None
                };
                let b = self.active();
                // Visual-caret intents (CARETMOTION visual / VisualLine edges) concern the PRIMARY
                // only; record them from its pre-move position. The GUI refines just the primary.
                let from = b.buffer().cursor();
                match motion {
                    Motion::Left if caret_visual => visual_move = Some((false, extend, from)),
                    Motion::Right if caret_visual => visual_move = Some((true, extend, from)),
                    Motion::VisualLineStart => visual_edge = Some((false, extend, from)),
                    Motion::VisualLineEnd => visual_edge = Some((true, extend, from)),
                    _ => {}
                }
                // `LU`/`LD` with a fresh map: move the primary by VISUAL row and we're done. If it
                // declines (off-window / target off-map / multi-caret), fall through to the logical fan-out.
                let visual_done = match &visual_rows {
                    Some(rows) => b.move_visual_row(matches!(motion, Motion::VisualDown), extend, rows),
                    None => false,
                };
                // Fan the LOGICAL motion out to every caret (single-caret = the ordinary move).
                // Left/Right/VisualLine* keep their logical-baseline fallback here (the GUI refines
                // the primary to the visual neighbour / visual-row edge via the intents above);
                // VisualUp/Down fall back to the logical up/down when the map didn't apply.
                if !visual_done {
                    b.move_carets(|bb| match motion {
                    Motion::Left => bb.move_left(extend),
                    Motion::Right => bb.move_right(extend),
                    Motion::Up | Motion::VisualUp => bb.move_up(extend),
                    Motion::Down | Motion::VisualDown => bb.move_down(extend),
                    Motion::LineStart => bb.move_line_start(extend),
                    Motion::LineEnd => bb.move_line_end(extend),
                    Motion::VisualLineStart => bb.move_line_start(extend),
                    Motion::VisualLineEnd => bb.move_line_end(extend),
                    Motion::DocStart => bb.move_doc_start(extend),
                    Motion::DocEnd => bb.move_doc_end(extend),
                    Motion::NextWord => bb.move_next_word(extend),
                    Motion::PrevWord => bb.move_prev_word(extend),
                    Motion::NextPara => bb.move_next_para(extend),
                    Motion::PrevPara => bb.move_prev_para(extend),
                    Motion::NextSentence => bb.move_next_sentence(extend),
                    Motion::PrevSentence => bb.move_prev_sentence(extend),
                    Motion::NextLineStart => bb.move_next_line_start(extend),
                    Motion::PrevLineStart => bb.move_prev_line_start(extend),
                    Motion::ToChar(n) => bb.move_to_char(n, extend),
                    Motion::ByChar(d) => bb.move_by_char(d, extend),
                    Motion::LeftSameLine => bb.move_left_same_line(extend),
                    Motion::RightSameLine => bb.move_right_same_line(extend),
                    Motion::PageDown => bb.move_lines(page, extend),
                    Motion::PageUp => bb.move_lines(-page, extend),
                    Motion::ScreenTop => {
                        bb.move_to_line(vtop, extend);
                        bb.move_line_start(extend);
                    }
                    Motion::ScreenBottom => {
                        bb.move_to_line(vtop + vrows - 1, extend);
                        bb.move_line_end(extend);
                    }
                    });
                }
                self.pending_visual_edge = visual_edge;
                self.pending_visual_move = visual_move;
            }
            EditorOp::CaretAdd(pos) => match pos {
                // Bare CARETADD: with a block defined, spray a column of carets (the gesture
                // delegates to CARETCOL); otherwise plant one caret and enter the building state.
                None => {
                    if self.active().buffer().has_selection() {
                        self.active().add_column_carets();
                    } else {
                        self.active().plant_caret();
                    }
                }
                Some(n) => {
                    self.active().add_caret(n);
                } // add/toggle at offset
            },
            EditorOp::CaretVertical(down) => self.active().add_carets_vertical(down),
            EditorOp::CaretColumn => self.active().add_column_carets(),
            EditorOp::CaretAll { pattern, regex, whole, case_sensitive, in_selection } => {
                // The needle: an explicit pattern, else the current selection text, else the word under
                // the caret (also the /S case, where the selection is the haystack, not the needle).
                let needle = match pattern.filter(|p| !p.is_empty()) {
                    Some(p) => p,
                    None => {
                        let b = self.active().buffer();
                        if !in_selection && b.has_selection() {
                            b.selection_text()
                        } else {
                            b.word_at()
                        }
                    }
                };
                if needle.is_empty() {
                    return Ok(Value::str("CARETALL: nothing to match".to_string()));
                }
                let pat = Pattern::build(&needle, case_sensitive, whole, regex, false)
                    .map_err(|e| XplError::trappable(format!("CARETALL: {e}")))?;
                // Search the whole document, or confine to the current selection with /S.
                let (lo, hi) = if in_selection {
                    self.active().buffer().selection_span().unwrap_or((0, self.active().buffer().len()))
                } else {
                    (0, self.active().buffer().len())
                };
                // Fetch one past the cap so we can tell "exactly MAX" from "capped".
                let mut spans = self.active().buffer().find_all(&pat, lo, hi, MAX_CARETS + 1);
                if spans.is_empty() {
                    return Ok(Value::str(format!("CARETALL: no matches for {needle:?}")));
                }
                let capped = spans.len() > MAX_CARETS;
                if capped {
                    spans.truncate(MAX_CARETS);
                }
                let n = self.active().set_selection_carets(&spans);
                let noun = if n == 1 { "caret" } else { "carets" };
                return Ok(Value::str(if capped {
                    format!("{n} {noun} (capped at {MAX_CARETS})")
                } else {
                    format!("{n} {noun}")
                }));
            }
            EditorOp::CaretNext { text, regex, whole, case_sensitive } => {
                let explicit = text.filter(|t| !t.is_empty());
                // Seed: nothing selected and no explicit text → select the word under the caret and stop
                // (the first press just picks the word, like Ctrl+D elsewhere).
                if explicit.is_none() && !self.active().buffer().has_selection() {
                    return Ok(Value::str(if self.active().select_word_at() {
                        "selected word".to_string()
                    } else {
                        "CARETNEXT: no word under the caret".to_string()
                    }));
                }
                let needle = match &explicit {
                    Some(t) => t.clone(),
                    None => self.active().buffer().selection_text(),
                };
                if needle.is_empty() {
                    return Ok(Value::str("CARETNEXT: nothing to match".to_string()));
                }
                let pat = Pattern::build(&needle, case_sensitive, whole, regex, false)
                    .map_err(|e| XplError::trappable(format!("CARETNEXT: {e}")))?;
                // Search forward from just past the LAST caret; wrap to the top if nothing follows.
                let b = self.active().buffer();
                let from = b.caret_positions().into_iter().max().unwrap_or(0);
                let hit = b.search(&pat, from, true).or_else(|| b.search(&pat, 0, true));
                let Some((s, e)) = hit else {
                    return Ok(Value::str(format!("CARETNEXT: no match for {needle:?}")));
                };
                // If the next match is already one of our selections, every occurrence is selected.
                if self.active().buffer().caret_spans().iter().any(|&sp| sp == (s, e)) {
                    return Ok(Value::str("CARETNEXT: all occurrences selected".to_string()));
                }
                let n = self.active().add_selection_caret_as_primary(s, e);
                let noun = if n == 1 { "caret" } else { "carets" };
                return Ok(Value::str(format!("{n} {noun}")));
            }
            EditorOp::CaretPop => {
                let popped = self.active().caret_pop();
                return Ok(Value::str(if popped {
                    let n = self.active().caret_count();
                    let noun = if n == 1 { "caret" } else { "carets" };
                    format!("{n} {noun}")
                } else {
                    "CARETPOP: no caret change to undo".to_string()
                }));
            }
            EditorOp::CaretPrimary(n) => {
                return Ok(Value::str(if self.active().set_primary_caret(n) {
                    format!("primary caret {n}")
                } else {
                    format!("CARETPRIMARY: no caret at index {n}")
                }));
            }
            EditorOp::CaretSkip { regex, whole, case_sensitive } => {
                let b = self.active().buffer();
                if !b.has_selection() {
                    return Ok(Value::str("CARETSKIP: no current match to skip".to_string()));
                }
                let needle = b.selection_text();
                let pat = Pattern::build(&needle, case_sensitive, whole, regex, false)
                    .map_err(|e| XplError::trappable(format!("CARETSKIP: {e}")))?;
                // Look forward from the END of the current (primary) match; wrap at EOF.
                let from = b.selection_span().map(|(_, e)| e).unwrap_or_else(|| b.cursor());
                let hit = b.search(&pat, from, true).or_else(|| b.search(&pat, 0, true));
                let Some((s, e)) = hit else {
                    return Ok(Value::str(format!("CARETSKIP: no match for {needle:?}")));
                };
                // Landing on an existing caret (or back on this one) means there's nowhere new to go.
                if self.active().buffer().caret_spans().iter().any(|&sp| sp == (s, e)) {
                    return Ok(Value::str("CARETSKIP: no other occurrence to move to".to_string()));
                }
                self.active().move_primary_selection(s, e);
                let n = self.active().caret_count();
                let noun = if n == 1 { "caret" } else { "carets" };
                return Ok(Value::str(format!("{n} {noun}")));
            }
            EditorOp::CaretSplit => {
                let n = self.active().split_carets();
                let noun = if n == 1 { "caret" } else { "carets" };
                return Ok(Value::str(format!("{n} {noun}")));
            }
            EditorOp::CaretOne => self.active().collapse_carets(),
            EditorOp::Define(kind) => {
                let d = self.active();
                match kind {
                    DefineKind::Toggle => {
                        d.define_toggle();
                    }
                    DefineKind::Modify => d.define_modify(),
                    DefineKind::Close => d.define_close(),
                    DefineKind::All => d.select_all(),
                    DefineKind::Cancel => d.cancel_selection(),
                    DefineKind::Word => d.define_word(),
                    DefineKind::Line => d.define_line(),
                    DefineKind::Sentence => d.define_sentence(),
                    DefineKind::Paragraph => d.define_paragraph(),
                    DefineKind::WordPlus => d.define_word_plus(),
                    DefineKind::SentencePlus => d.define_sentence_plus(),
                    DefineKind::ParagraphPlus => d.define_paragraph_plus(),
                    DefineKind::InsideBracket => d.define_inside_bracket(),
                    DefineKind::InsideQuote => d.define_inside_quote(),
                }
            }
            EditorOp::Persistent(kind) => {
                self.persistent = match kind {
                    PersistKind::On => true,
                    PersistKind::Off => false,
                    PersistKind::Toggle => !self.persistent,
                };
                // Apply immediately to the active buffer (CPS re-couples a roamed block).
                let p = self.persistent;
                self.active().set_persistent(p);
            }
            EditorOp::RubDelete => {
                let sel = self.active().buffer().selection_text();
                if sel.is_empty() {
                    return Err(XplError::trappable_code(ErrCode::NothingSelected, "no text is selected"));
                }
                self.deletion_buffer = sel;
                self.active().delete_selection();
            }
            EditorOp::PasteDeletion => {
                let s = self.deletion_buffer.clone();
                if !s.is_empty() {
                    self.active().insert(&s);
                }
            }
            EditorOp::DeleteSel => {
                let sel = self.active().buffer().selection_text();
                if sel.is_empty() {
                    return Err(XplError::trappable_code(ErrCode::NothingSelected, "no text is selected"));
                }
                self.active().delete_selection();
            }
            EditorOp::MoveToSelectionEdge { end } => {
                match self.active().buffer().selection_span() {
                    Some((s, e)) if s < e => {
                        // Move to the requested edge with the SAME motion semantics as `JMP` (a plain,
                        // non-extending move): a DEFINED block (`DF`/`DW`/…) is preserved — the caret
                        // decouples and roams to the edge — while a transient Shift/mouse selection
                        // collapses, identical to `JMP $DS` / `JMP $DN`. The old `set_caret(_, None)`
                        // force-cleared even a defined block (and reset the defining/goal state).
                        let target = if end { e } else { s };
                        self.active().move_carets(|bb| bb.move_to_offset(target, false));
                    }
                    _ => return Err(XplError::trappable_code(ErrCode::NothingSelected, "no text is selected")),
                }
            }
            EditorOp::Case(kind) => self.active().change_case(kind),
            EditorOp::CopySelection => {
                if !self.active().copy_selection_to_cursor() {
                    return Err(XplError::trappable_code(ErrCode::NothingSelected, "no text is selected"));
                }
            }
            EditorOp::MoveSelection => {
                if !self.active().move_selection_to_cursor() {
                    return Err(XplError::trappable_code(ErrCode::NothingSelected, "no text is selected"));
                }
            }
            EditorOp::Search(spec) => return self.do_search(spec),
            EditorOp::Clip(kind) => return self.do_clip(kind),
            EditorOp::Undo(levels) => {
                for _ in 0..levels {
                    if !self.active().undo() {
                        break; // nothing left to undo
                    }
                }
            }
            EditorOp::Redo(levels) => {
                for _ in 0..levels {
                    if !self.active().redo() {
                        break; // nothing left to redo
                    }
                }
            }
            EditorOp::BeginUndoGroup => self.active().begin_undo_group(),
            EditorOp::EndUndoGroup => self.active().end_undo_group(),
            EditorOp::NewDoc => {
                self.add_or_reuse(Document::new());
                self.set_focus(Target::Document); // land the caret in the new buffer, not the cmline
            }
            EditorOp::NewNamed { path } => {
                let target: PathBuf =
                    if self.virtual_fs { PathBuf::from(&path) } else { self.resolve_path(&path) };
                let key = target.to_string_lossy().into_owned();
                let exists =
                    if self.virtual_fs { self.files.contains_key(&key) } else { target.exists() };
                if exists {
                    // Forgiving: `NEW` of an existing file is just an `OPEN`, with a heads-up. If it's
                    // already open in a tab, switch to it rather than duplicating.
                    if let Some(idx) = self.find_open_tab(&key) {
                        self.set_active(idx); // switch tab + focus the document
                        return Ok(Value::str(format!("{path} already exists (switched to open tab)")));
                    }
                    self.open_doc(&key, false, false)?; // loads it into a tab
                    return Ok(Value::str(format!("{path} already exists — opened")));
                }
                // New file: a fresh empty buffer named for it, ready to SAVE.
                let mut doc = Document::new();
                doc.set_path(target.clone());
                let name = doc.file_name().unwrap_or_else(|| key.clone());
                self.add_or_reuse(doc);
                self.set_focus(Target::Document); // caret in the fresh buffer, not the cmline
                return Ok(Value::str(format!("new file {name}")));
            }
            EditorOp::Open { path, force, hex } => {
                if path.trim().is_empty() {
                    return Err(XplError::trappable("OPEN: a path is required"));
                }
                // Already open in another tab? Prompt instead of silently duplicating — unless
                // `/NV` (force) was given, which opens a fresh tab outright.
                if !force {
                    if let Some(existing) = self.find_open_tab(&path) {
                        // `GOTOOPEN on` skips the prompt and switches straight to the open tab.
                        if self.goto_open {
                            let i = existing.min(self.docs.len().saturating_sub(1));
                            self.set_active(i);
                            return Ok(Value::str("switched to the open copy"));
                        }
                        self.pending_open =
                            Some(PendingOpen { path, existing, read_only: false, hex });
                        return Ok(Value::str("")); // prompt surfaced via `open_prompt`
                    }
                }
                return self.open_doc(&path, false, hex);
            }
            EditorOp::OpenReadOnly { path, force: _, hex } => {
                // `READ <path>` — open an EXISTING file in a locked (read-only) tab. For XyWrite
                // parity, READ ALWAYS opens a fresh copy with no "already open?" prompt, so you can
                // hold several read-only views of the same file at once (`/NV` is thus a no-op here).
                if path.trim().is_empty() {
                    return Err(XplError::trappable("READ: a filename is required"));
                }
                return self.open_doc(&path, true, hex);
            }
            EditorOp::BlindEdit(path) => {
                if self.blind.is_some() {
                    return Err(XplError::trappable(
                        "BEDIT: already blind-editing (AB to exit first)",
                    ));
                }
                let msg = match path {
                    None => {
                        self.blind = Some(Document::new());
                        "blind-editing a new buffer".to_string()
                    }
                    Some(path) => {
                        // OPEN semantics into the hidden buffer: load if present, else a new buffer
                        // named for the file (ready to SAVE).
                        let target: PathBuf =
                            if self.virtual_fs { PathBuf::from(&path) } else { self.resolve_path(&path) };
                        let key = target.to_string_lossy().into_owned();
                        let exists =
                            if self.virtual_fs { self.files.contains_key(&key) } else { target.exists() };
                        let mut doc = Document::new();
                        let msg = if exists {
                            let bytes = self.read_doc_bytes(&key)?;
                            doc.load(bytes, target.clone());
                            let name = doc.file_name().unwrap_or_else(|| key.clone());
                            format!("blind-editing {name} ({} lines)", doc.buffer().line_count())
                        } else {
                            doc.set_path(target.clone());
                            let name = doc.file_name().unwrap_or_else(|| key.clone());
                            format!("blind-editing new file {name}")
                        };
                        self.blind = Some(doc);
                        msg
                    }
                };
                return Ok(Value::str(msg));
            }
            EditorOp::BlindClose(file) => {
                // Close (discard) the blind buffer a `BEDIT` opened — the explicit sibling of `AB`
                // while blind. The named form asserts WHICH file is being closed.
                let open_name = match self.blind.as_ref() {
                    Some(d) => d.file_name(), // Option<String> (None for a bare/unnamed BEDIT)
                    None => return Err(XplError::trappable("BCLOSE: no blind edit is open")),
                };
                if let Some(want) = &file {
                    // Lenient match on the base name, so `data/foo.txt` still names `foo.txt`.
                    let want_name = Path::new(want.trim())
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| want.trim().to_string());
                    if open_name.as_deref() != Some(want_name.as_str()) {
                        let shown = open_name.clone().unwrap_or_else(|| "(unnamed)".to_string());
                        return Err(XplError::trappable(format!(
                            "BCLOSE: the blind edit is {shown}, not {want_name}"
                        )));
                    }
                }
                self.exit_blind_if_open();
                let name = open_name.unwrap_or_else(|| "(unnamed)".to_string());
                return Ok(Value::str(format!("closed blind edit {name}")));
            }
            EditorOp::ChangeDir(path) => {
                return match self.change_directory(&path) {
                    Ok(cwd) => Ok(Value::str(format!("cwd: {cwd}"))),
                    Err(e) => Err(XplError::trappable(format!("CD: {e}"))),
                };
            }
            EditorOp::Dir { path, opts } => {
                let (base, filter) = self.resolve_dir_spec(path.as_deref());
                let base = lexical_abs(&self.cwd, &base);
                let entries = self.list_for(&base, &opts)?;
                // `DIRRT on`: reuse an open listing tab (the active one if it's a listing, else the
                // first listing tab) instead of spawning a new one. Else add a fresh tab as usual.
                let reuse_idx = if self.dir_reuse {
                    if self.docs[self.active].is_dir_listing() {
                        Some(self.active)
                    } else {
                        self.docs.iter().position(|d| d.is_dir_listing())
                    }
                } else {
                    None
                };
                if let Some(i) = reuse_idx {
                    self.active = i;
                    self.docs[i].open_listing(base.clone(), entries, opts, filter.clone());
                } else {
                    let mut doc = Document::new();
                    doc.open_listing(base.clone(), entries, opts, filter.clone());
                    self.add_or_reuse(doc);
                }
                // Land the caret IN the listing: focus the document (not the command line, even when
                // `DIR` was run from it), so arrows / type-to-find / Enter work immediately.
                self.set_focus(Target::Document);
                // Count the shown items (post-filter), excluding the `..` parent.
                let count = self.docs[self.active]
                    .dir_state()
                    .map(|d| d.entries.iter().filter(|e| e.name != "..").count())
                    .unwrap_or(0);
                let s = if count == 1 { "" } else { "s" };
                let msg = match &filter {
                    Some(p) => format!("{count} item{s} matching {p} in {}", base.display()),
                    None => format!("{count} item{s} in {}", base.display()),
                };
                return Ok(Value::str(msg));
            }
            EditorOp::Grep { pattern, path, regex, case, whole, perl } => {
                use std::sync::atomic::{AtomicBool, Ordering};
                // Validate the pattern up front so a bad `/R`/`/P` errors immediately — no tab, no thread.
                crate::grep::compile_check(&pattern, regex, case, whole, perl)
                    .map_err(|e| XplError::trappable(format!("GREP: {e}")))?;
                // Special filespec `ALLTABS`/`ALLBUFS`: search the in-memory content of every open
                // document (unsaved edits included) instead of walking the filesystem.
                let all_tabs = path.as_deref().map(str::trim).is_some_and(|p| {
                    p.eq_ignore_ascii_case("ALLTABS") || p.eq_ignore_ascii_case("ALLBUFS")
                });
                // Snapshot the buffers to search BEFORE opening the results tab (so it can't search
                // itself). Skip the machinery tabs — GREP/DIR listings, HEX views and the file-less
                // *Console*/*Shell Output* (they carry a synthetic label) — keeping real text docs:
                // saved files (even with unsaved edits) and plain untitled buffers.
                let sources: Vec<crate::grep::GrepSource> = if all_tabs {
                    self.docs
                        .iter()
                        .filter(|d| {
                            !d.is_grep_listing()
                                && !d.is_dir_listing()
                                && !d.hex_view()
                                && d.label().is_none()
                        })
                        .map(|d| crate::grep::GrepSource {
                            label: d.path().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("Untitled")),
                            id: d.id().0,
                            bytes: d.save_bytes(),
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                // Resolve the optional path/glob into a search root + file-name mask, exactly like a
                // `DIR` filespec: a directory searches it; a glob (`*.rs`) filters `$CWD`; `dir/*.rs`
                // does both; absent = `$CWD`. For `ALLTABS` the base is empty (labels show verbatim).
                let (base, mask) = if all_tabs {
                    (PathBuf::new(), None)
                } else {
                    let (base, mask) = self.resolve_dir_spec(path.as_deref());
                    let base = lexical_abs(&self.cwd, &base);
                    if !base.is_dir() {
                        return Err(XplError::trappable(format!(
                            "GREP: {}: not a directory",
                            base.display()
                        )));
                    }
                    (base, mask)
                };
                // Supersede any search already running.
                if let Some(job) = self.grep_job.take() {
                    job.cancel.store(true, Ordering::Relaxed);
                }
                // Open an EMPTY results tab immediately (so the search is visibly under way); the worker
                // streams hits into it via `poll_grep`.
                let mut doc = Document::new();
                doc.open_grep_listing(base.clone(), Vec::new(), pattern.clone(), regex, perl, false);
                let doc_id = doc.id();
                self.add_or_reuse(doc);
                // Highlight the matches with EMPH — in the results tab now, and (via `grep_jump`) in
                // any file opened with `GJ`. Map the GREP pattern to an EMPH phrase so the highlight
                // covers exactly what GREP matched: a regex GREP → `EMPH/R`; a whole-word GREP → EMPH's
                // own whole-word literal; a plain literal GREP → `EMPH/R` on the ESCAPED pattern, so
                // substring matches light up too (EMPH's non-regex mode is whole-word only). EMPH is
                // always case-insensitive, so a case-sensitive GREP may highlight extra case variants.
                let (emph_phrase, emph_regex) = if regex || perl {
                    // A `/P` pattern feeds EMPH's linear `/R` engine: if it uses a fancy-only construct
                    // (look-around, backreference) EMPH can't compile it and simply leaves the results
                    // un-emphasised — the hits still show. A `/P` pattern that is also valid linear
                    // regex highlights normally.
                    (pattern.clone(), true)
                } else if whole {
                    (pattern.clone(), false)
                } else {
                    (regex::escape(&pattern), true)
                };
                if let Some(d) = self.docs.iter_mut().find(|d| d.id() == doc_id) {
                    d.set_emph(Some(emph_phrase), emph_regex);
                }
                self.set_focus(Target::Document); // land the caret in the results, ready for GJ

                // Spawn the search on a worker thread; it sends batches of hits over the channel.
                // Either a filesystem walk (`base`/`mask`) or the snapshotted open buffers (`sources`).
                enum GrepTarget {
                    Files { base: PathBuf, mask: Option<String> },
                    Buffers(Vec<crate::grep::GrepSource>),
                }
                let cancel = std::sync::Arc::new(AtomicBool::new(false));
                let (tx, rx) = std::sync::mpsc::channel::<GrepMsg>();
                {
                    let (cancel, pat) = (cancel.clone(), pattern.clone());
                    let target = if all_tabs {
                        GrepTarget::Buffers(sources)
                    } else {
                        GrepTarget::Files { base: base.clone(), mask: mask.clone() }
                    };
                    // Context/blank options (`GREPBEFORE`/`GREPAFTER`/`GREPBLANK`), captured for the worker.
                    let (before, after, blank) = (self.grep_before, self.grep_after, self.grep_blank);
                    std::thread::spawn(move || {
                        let mut batch: Vec<crate::grep::GrepHit> = Vec::new();
                        let emit = |hit| {
                            batch.push(hit);
                            if batch.len() >= GREP_BATCH {
                                // A send error means the receiver is gone (tab closed) → stop.
                                return tx.send(GrepMsg::Hits(std::mem::take(&mut batch))).is_ok();
                            }
                            true
                        };
                        let summary = match target {
                            GrepTarget::Files { base, mask } => crate::grep::search(
                                &base, &pat, regex, case, whole, perl, mask.as_deref(), GREP_CAP,
                                before, after, blank, &cancel, emit,
                            ),
                            GrepTarget::Buffers(srcs) => crate::grep::search_slices(
                                &srcs, &pat, regex, case, whole, perl, GREP_CAP, before, after, blank,
                                &cancel, emit,
                            ),
                        };
                        if !batch.is_empty() {
                            let _ = tx.send(GrepMsg::Hits(batch));
                        }
                        let _ = match summary {
                            Ok(s) => tx.send(GrepMsg::Done { files: s.files_searched, capped: s.capped }),
                            Err(e) => tx.send(GrepMsg::Error(e)),
                        };
                    });
                }
                self.grep_job = Some(GrepJob { rx, cancel, doc: doc_id, base });
                return Ok(Value::str(self.tr_msg("msg.grep.searching", "GREP: searching…")));
            }
            EditorOp::GrepJump => {
                return self.grep_jump();
            }
            EditorOp::SetGrepBefore(n) => {
                self.grep_before = n;
                return Ok(Value::str(format!("GREPBEFORE = {n}")));
            }
            EditorOp::SetGrepAfter(n) => {
                self.grep_after = n;
                return Ok(Value::str(format!("GREPAFTER = {n}")));
            }
            EditorOp::SetGrepBlank(on) => {
                self.grep_blank = on;
                return Ok(Value::str(format!("GREPBLANK = {}", if on { "on" } else { "off" })));
            }
            EditorOp::DirToString { path, opts } => {
                // Same enumeration/filter/sort as `Dir`, but render to text (honouring `/B` etc.)
                // and return it — for `CAPTURE`. No buffer is opened.
                let (base, filter) = self.resolve_dir_spec(path.as_deref());
                let base = lexical_abs(&self.cwd, &base);
                let entries = self.list_for(&base, &opts)?;
                let entries = match &filter {
                    Some(pat) => crate::dir::filter_entries(entries, pat),
                    None => entries,
                };
                return Ok(Value::str(crate::dir::render_listing(&entries, opts.bare)));
            }
            EditorOp::DirWindow { path, opts } => {
                return self.open_dir_window(path, opts);
            }
            EditorOp::SetDirNewTab(on) => {
                self.new_window = on;
                return Ok(Value::str(format!("DIRNW {}", if on { "on" } else { "off" })));
            }
            EditorOp::SetDirWindowNewTab(on) => {
                self.dir_window_new_tab = on;
                return Ok(Value::str(format!("DIRWNW {}", if on { "on" } else { "off" })));
            }
            EditorOp::SetDirWindowClose(on) => {
                self.dir_window_close_on_open = on;
                return Ok(Value::str(format!("DIRWCLOSE {}", if on { "on" } else { "off" })));
            }
            EditorOp::SetDirReuse(on) => {
                self.dir_reuse = on;
                return Ok(Value::str(format!("DIRRT {}", if on { "on" } else { "off" })));
            }
            EditorOp::SetDirDefaults(switches) => {
                self.dir_defaults = switches;
                let shown = self.dir_defaults_display();
                return Ok(Value::str(format!("DIRSET {shown}")));
            }
            EditorOp::SetTitlebar(opt) => {
                self.show_titlebar = opt.unwrap_or(!self.show_titlebar);
                return Ok(Value::str(format!(
                    "TITLEBAR {}",
                    if self.show_titlebar { "on" } else { "off" }
                )));
            }
            EditorOp::SetWriterView(opt) => {
                self.writer_view = opt.unwrap_or(!self.writer_view);
                return Ok(Value::str(format!(
                    "Writer's View {}",
                    if self.writer_view { "on" } else { "off" }
                )));
            }
            EditorOp::SetRememberGeometry(opt) => {
                self.remember_window_geometry = opt.unwrap_or(!self.remember_window_geometry);
                return Ok(Value::str(format!(
                    "WINGEO {}",
                    if self.remember_window_geometry { "on" } else { "off" }
                )));
            }
            EditorOp::SetSessionRestore(opt) => {
                self.session_restore = opt.unwrap_or(!self.session_restore);
                return Ok(Value::str(format!(
                    "SESSION {}",
                    if self.session_restore { "on" } else { "off" }
                )));
            }
            EditorOp::SetTrust(on) => {
                self.shell_trusted = on;
                let msg = if on {
                    "shell-out enabled (TRUST 1)"
                } else {
                    "shell-out disabled (TRUST 0)"
                };
                return Ok(Value::str(msg));
            }
            EditorOp::RunHere { args } => {
                // Move the caret to the text area FIRST (if it was on the cmline), then run — so
                // `RUNHERE` fired from the command line lands you in the document and never refuses
                // just because focus was elsewhere. The block is found at the DOCUMENT's caret.
                self.set_focus(Target::Document);
                let doc = &self.docs[self.active];
                let block = fenced_block_at(&doc.buffer().text(), doc.buffer().cursor_line());
                return match block {
                    Some(code) if !code.trim().is_empty() => {
                        let lines = code.lines().count();
                        self.pending_run.push((code, args));
                        let s = if lines == 1 { "" } else { "s" };
                        Ok(Value::str(format!("RUNHERE: running {lines} line{s}")))
                    }
                    Some(_) => Err(XplError::trappable("RUNHERE: the code block is empty")),
                    None => Err(XplError::trappable("RUNHERE: not inside a code block")),
                };
            }
            EditorOp::SetBinder { script, slot, source } => {
                // Resolve the content: selection (host-side), quoted literal, or file.
                let text = match source {
                    BinderSource::Literal(s) => s,
                    BinderSource::File(p) => {
                        // Resolve relative paths against the editor cwd (like OPEN); the
                        // virtual test fs is keyed by the raw path.
                        let path = if self.virtual_fs {
                            p
                        } else {
                            self.resolve_path(&p).to_string_lossy().into_owned()
                        };
                        self.read_file(&path)?
                    }
                    BinderSource::Selection => {
                        let b = self.active().buffer();
                        b.has_selection()
                            .then(|| b.selection_text())
                            .ok_or_else(|| XplError::trappable_code(ErrCode::NothingSelected, "no text is selected"))?
                    }
                };
                let sigil = if script { '&' } else { '@' };
                if script {
                    self.script_binders.insert(slot, text);
                } else {
                    self.text_binders.insert(slot, text);
                }
                return Ok(Value::str(format!("loaded {sigil}{slot}")));
            }
            EditorOp::UseBinder { script, slot } => {
                let store = if script { &self.script_binders } else { &self.text_binders };
                let sigil = if script { '&' } else { '@' };
                let content = store.get(&slot).cloned().ok_or_else(|| {
                    XplError::trappable(format!("binder {sigil}{slot} is empty"))
                })?;
                if script {
                    self.pending_run.push((content, Vec::new())); // run after the current script returns
                } else {
                    self.active().insert(&content); // insert text at the active caret
                }
                return Ok(Value::str(""));
            }
            EditorOp::ResetStopwatch => {
                self.stopwatch_start = std::time::Instant::now();
                return Ok(Value::str("stopwatch reset"));
            }
            EditorOp::ShowMessage { text, timeout_secs } => {
                self.pending_message = Some((text, timeout_secs));
                return Ok(Value::str(""));
            }
            EditorOp::OsMessage { title, text } => {
                // Pop a real desktop dialog as a detached process — works headless (RUN/BG) and
                // survives this (often short-lived) process. See `os_message`.
                crate::os_message::show(title.as_deref(), &text);
                return Ok(Value::str(""));
            }
            EditorOp::RunLint => {
                #[cfg(not(feature = "lint"))]
                {
                    return Ok(Value::str(
                        "Language checking is not available in this build (rebuild with --features lint).",
                    ));
                }
                // Run harper over the active doc and return the report (the CAPTURE path — synchronous
                // so the variable is filled immediately). The interactive path uses the background
                // `RequestFullCheck` instead. `CHECK` is a report only — no touching `self.lints`.
                #[cfg(feature = "lint")]
                {
                    let text = self.docs[self.active].buffer().text();
                    let report = build_check_report(
                        &text,
                        self.lint_flavor(),
                        self.lint_dialect,
                        self.spell_engine,
                        &self.spell_dicts,
                        &self.spell_bin,
                        &self.learned_words,
                    );
                    return Ok(Value::str(report));
                }
            }
            EditorOp::RequestFullCheck => {
                self.request_full_check();
                return Ok(Value::str(""));
            }
            EditorOp::LintNav { spelling, forward } => {
                #[cfg(not(feature = "lint"))]
                {
                    let _ = (spelling, forward);
                    return Ok(Value::str(
                        "Language checking is not available in this build (rebuild with --features lint).",
                    ));
                }
                #[cfg(feature = "lint")]
                {
                    match self.find_lint_target(spelling, forward) {
                        Some((s, e)) => {
                            self.active().set_caret(e, Some(s)); // select the issue; GUI scrolls to it
                            return Ok(Value::str(""));
                        }
                        None => {
                            let kind = if spelling { "spelling" } else { "grammar" };
                            let dir = if forward { "" } else { " above" };
                            return Err(XplError::trappable(format!("no more {kind} issues{dir}")));
                        }
                    }
                }
            }
            EditorOp::Learn(word) => {
                let w = word.trim().to_string();
                if w.is_empty() {
                    return Err(XplError::trappable("LEARN: a word is required"));
                }
                let msg = if self.learn_word(&w) {
                    format!("learned: {w}")
                } else {
                    format!("already known: {w}")
                };
                return Ok(Value::str(msg));
            }
            EditorOp::Unlearn(word) => {
                let w = word.trim().to_string();
                let msg = if self.unlearn_word(&w) {
                    format!("unlearned: {w}")
                } else {
                    format!("not in your dictionary: {w}")
                };
                return Ok(Value::str(msg));
            }
            EditorOp::LearnWords(words) => {
                let (added, known) = self.learn_words(&words);
                return Ok(Value::str(format!(
                    "learned {added} new word{} ({known} already known)",
                    if added == 1 { "" } else { "s" }
                )));
            }
            EditorOp::UnlearnWords(words) => {
                let (removed, absent) = self.unlearn_words(&words);
                return Ok(Value::str(format!(
                    "unlearned {removed} word{} ({absent} not in your dictionary)",
                    if removed == 1 { "" } else { "s" }
                )));
            }
            EditorOp::SetDialect(name) => {
                match crate::lint::Dialect::parse(&name) {
                    Some(d) => {
                        self.set_dialect(d);
                        return Ok(Value::str(format!("dialect = {}", d.name())));
                    }
                    None => {
                        return Err(XplError::trappable(format!(
                            "DIALECT: unknown dialect {name:?} (try american|british|canadian|australian|indian)"
                        )))
                    }
                }
            }
            EditorOp::SetSpellEngine(name) => match crate::lint::SpellEngine::parse(&name) {
                Some(e) => {
                    self.set_spell_engine(e);
                    return Ok(Value::str(format!("spell engine = {}", e.name())));
                }
                None => {
                    return Err(XplError::trappable(format!(
                        "SPELLENGINE: unknown engine {name:?} (harper | hunspell)"
                    )))
                }
            },
            EditorOp::SetSpellDicts(names) => {
                self.set_spell_dicts(names.clone());
                return Ok(Value::str(format!("spell dictionaries = {names}")));
            }
            EditorOp::SetSpellBin(path) => {
                self.set_spell_bin(path.clone());
                return Ok(Value::str(format!("hunspell = {path}")));
            }
            EditorOp::SetSpelling(on) => {
                self.set_spelling(on);
                return Ok(Value::str(format!("live spelling check {}", if on { "on" } else { "off" })));
            }
            EditorOp::SetGrammar(on) => {
                self.set_grammar(on);
                return Ok(Value::str(format!("live grammar check {}", if on { "on" } else { "off" })));
            }
            EditorOp::FileCopy { src, dst } => {
                if self.virtual_fs {
                    let content = self.files.get(&src).cloned().ok_or_else(|| {
                        XplError::trappable_code(ErrCode::FileNotFound, format!("COPY: {src}: not found"))
                    })?;
                    self.files.insert(dst.clone(), content);
                    return Ok(Value::str(format!("copied {src} -> {dst}")));
                }
                let s = self.resolve_path(&src);
                let mut d = self.resolve_path(&dst);
                if s.is_dir() {
                    return Err(XplError::trappable(format!(
                        "COPY: {} is a directory",
                        s.display()
                    )));
                }
                // Copying onto a directory drops the file inside it (DOS/Unix behavior).
                if d.is_dir() {
                    if let Some(n) = s.file_name() {
                        d = d.join(n);
                    }
                }
                std::fs::copy(&s, &d).map_err(|e| XplError::trappable(format!("COPY: {e}")))?;
                return Ok(Value::str(format!("copied {} -> {}", s.display(), d.display())));
            }
            EditorOp::FileDelete(path) => {
                if path.trim().is_empty() {
                    return Err(XplError::trappable("DEL: a filename is required"));
                }
                if self.virtual_fs {
                    if self.files.remove(&path).is_none() {
                        return Err(XplError::trappable_code(ErrCode::FileNotFound, format!("DEL: {path}: not found")));
                    }
                    let p = self.resolve_path(&path);
                    self.refresh_listings_after_delete(&p);
                    return Ok(Value::str(format!("deleted {path}")));
                }
                let p = self.resolve_path(&path);
                // A directory deletes only when empty (no silent recursive wipe).
                let res =
                    if p.is_dir() { std::fs::remove_dir(&p) } else { std::fs::remove_file(&p) };
                res.map_err(|e| XplError::trappable(format!("DEL: {}: {e}", p.display())))?;
                remove_xymeta_sidecar(&p); // don't orphan the MDA sidecar
                self.refresh_listings_after_delete(&p);
                return Ok(Value::str(format!("deleted {}", p.display())));
            }
            EditorOp::FileDeleteConfirm(path) => {
                if path.trim().is_empty() {
                    return Err(XplError::trappable("DEL: a filename is required"));
                }
                // Validate existence up front so a typo errors immediately instead of arming a
                // prompt for a file that isn't there; then arm the confirm (GUI shows close_prompt).
                let (key, exists, name) = if self.virtual_fs {
                    (path.clone(), self.files.contains_key(&path), path.clone())
                } else {
                    let p = self.resolve_path(&path);
                    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
                    (p.to_string_lossy().into_owned(), p.exists(), name)
                };
                if !exists {
                    return Err(XplError::trappable_code(ErrCode::FileNotFound, format!("DEL: {path}: not found")));
                }
                self.pending_close =
                    Some(PendingClose::DeleteFile { path: PathBuf::from(key), name });
                return Ok(Value::str(""));
            }
            EditorOp::FileRename { from, to } => {
                if self.virtual_fs {
                    let content = self.files.remove(&from).ok_or_else(|| {
                        XplError::trappable_code(ErrCode::FileNotFound, format!("RENAME: {from}: not found"))
                    })?;
                    self.files.insert(to.clone(), content);
                    // Retarget an open tab showing the old name so its tab label follows the rename.
                    self.retarget_open_tab(Path::new(&from), PathBuf::from(&to));
                    let (f, t) = (self.resolve_path(&from), self.resolve_path(&to));
                    self.refresh_listings_after_rename(&f, &t);
                    return Ok(Value::str(format!("renamed {from} -> {to}")));
                }
                let f = self.resolve_path(&from);
                let mut t = self.resolve_path(&to);
                if t.is_dir() {
                    if let Some(n) = f.file_name() {
                        t = t.join(n);
                    }
                }
                std::fs::rename(&f, &t).map_err(|e| XplError::trappable(format!("RENAME: {e}")))?;
                rename_xymeta_sidecar(&f, &t); // the MDA sidecar follows the file
                // If the renamed file is open in a tab, update its path so the tab label (derived
                // from the file name) follows the rename instead of showing the old name.
                self.retarget_open_tab(&f, t.clone());
                self.refresh_listings_after_rename(&f, &t);
                return Ok(Value::str(format!("renamed {} -> {}", f.display(), t.display())));
            }
            EditorOp::Save(opt_path) => {
                // XyWrite rule: a read-only document can't be stored over its own file. Writing it
                // ELSEWHERE (an editable copy) is still allowed via `SAVE <path>` / `SAAS` / `SAD`.
                if self.file_doc().read_only() {
                    return Err(XplError::trappable(
                        "SAVE: document is read-only (RO to unlock, or SAAS <path> to save a copy)",
                    ));
                }
                // File ops act on the active document (or the blind-edit buffer), not the cmline.
                let path: PathBuf = match opt_path {
                    // Resolve a relative SAVE/SAAS path against the editor's `$CWD` (via
                    // `resolve_path`), NOT the process cwd — otherwise a bare name silently lands
                    // wherever the binary was launched from. (Mirrors `SaveCopy`.)
                    Some(p) if !p.trim().is_empty() => {
                        if self.virtual_fs {
                            PathBuf::from(p)
                        } else {
                            self.resolve_path(&p)
                        }
                    }
                    _ => match self.file_doc().path() {
                        Some(p) => p.to_path_buf(),
                        None => {
                            return Err(XplError::trappable(
                                "SAVE: no file name; use SAVE <path>",
                            ))
                        }
                    },
                };
                let bytes = self.file_doc().save_bytes();
                self.write_doc_bytes(&path.to_string_lossy(), &bytes)?;
                self.sync_color_sidecar(self.file_doc(), &path.to_string_lossy(), &bytes);
                self.file_doc_mut().set_path(path.clone());
                self.file_doc_mut().mark_saved();
                // Re-baseline the AUTOREOPEN mtime to OUR write, so the poll doesn't treat a save as an
                // external change and needlessly reload. (No-op when blind / under virtual_fs.)
                if self.blind.is_none() {
                    self.refresh_disk_mtime(self.active);
                }
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
                return Ok(Value::str(self.tr_fmt("msg.save.written", "saved {name}", &[("name", &name)])));
            }
            EditorOp::SaveCopy { path, force } => {
                // Write the active document's bytes to `path` as a COPY: the doc's own path,
                // tab and modified-state are untouched. Confirm before clobbering an existing
                // file unless `/NV` (force) was given.
                if path.trim().is_empty() {
                    return Err(XplError::trappable("SAVE: a path is required"));
                }
                let target: PathBuf = if self.virtual_fs {
                    PathBuf::from(&path)
                } else {
                    self.resolve_path(&path)
                };
                let key = target.to_string_lossy().into_owned();
                let exists = if self.virtual_fs {
                    self.files.contains_key(&key)
                } else {
                    target.exists()
                };
                let name = target
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| key.clone());
                if exists && !force && self.blind.is_none() {
                    // Arm a confirmation the GUI (or headless caller) resolves via resolve_close.
                    // (Skipped during a blind edit — scripts don't get an interactive prompt.)
                    self.pending_close = Some(PendingClose::Overwrite { path: target, name });
                    return Ok(Value::str(""));
                }
                let bytes = self.file_doc().save_bytes();
                self.write_doc_bytes(&key, &bytes)?;
                self.sync_color_sidecar(self.file_doc(), &key, &bytes);
                return Ok(Value::str(format!("saved copy to {name}")));
            }
            EditorOp::Store { path, force } => {
                // `ST [/NV] [<path>]` — save (adopting `<path>` if given), then close the tab.
                let adopt: Option<PathBuf> = match &path {
                    Some(p) if !p.trim().is_empty() => Some(if self.virtual_fs {
                        PathBuf::from(p)
                    } else {
                        self.resolve_path(p)
                    }),
                    _ => None,
                };
                let own = self.file_doc().path().map(|p| p.to_path_buf());
                let Some(target) = adopt.clone().or_else(|| own.clone()) else {
                    // Bare ST on an untitled doc: nowhere to save — do NOT close (no data loss).
                    return Err(XplError::trappable("ST: no file name; use ST <path>"));
                };
                // XyWrite rule: a read-only document can't be stored over its own file. Storing it to
                // a DIFFERENT new path (an editable copy) is allowed.
                if self.file_doc().read_only() && own.as_deref() == Some(target.as_path()) {
                    return Err(XplError::trappable(
                        "ST: document is read-only (RO to unlock, or ST <path> to store a copy)",
                    ));
                }
                let key = target.to_string_lossy().into_owned();
                let name = target
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| key.clone());
                let exists =
                    if self.virtual_fs { self.files.contains_key(&key) } else { target.exists() };
                // Confirm ONLY when adopting a DIFFERENT existing file (overwrite risk) — not when
                // saving in place to the doc's own file. `/NV` and blind edits skip the prompt.
                let overwriting_other =
                    exists && adopt.is_some() && own.as_deref() != Some(target.as_path());
                if overwriting_other && !force && self.blind.is_none() {
                    self.pending_close = Some(PendingClose::StoreOverwrite { path: target, name });
                    return Ok(Value::str("")); // prompt surfaced via close_prompt / resolve_close
                }
                // Save (adopting the path if one was given), then close the tab.
                let bytes = self.file_doc().save_bytes();
                self.write_doc_bytes(&key, &bytes)?;
                self.sync_color_sidecar(self.file_doc(), &key, &bytes);
                self.file_doc_mut().set_path(target);
                self.file_doc_mut().mark_saved();
                let closed = self.close_active_buffer();
                return Ok(Value::str(format!("stored + closed {closed}")));
            }
            EditorOp::SaveAs { path, force } => {
                // `SAAS`/`SAVEAS <path>` — save the active document to `path` and ADOPT it (the tab
                // stays open). Confirm before clobbering a DIFFERENT existing file unless `/NV` (force)
                // or a blind edit. (The bare, no-path form goes through SaveAsPrompt / the native GUI
                // dialog, which does its own overwrite prompt — so it isn't routed here.)
                if path.trim().is_empty() {
                    return Err(XplError::trappable("SAAS: a path is required"));
                }
                let target: PathBuf =
                    if self.virtual_fs { PathBuf::from(&path) } else { self.resolve_path(&path) };
                let key = target.to_string_lossy().into_owned();
                let exists =
                    if self.virtual_fs { self.files.contains_key(&key) } else { target.exists() };
                let own = self.file_doc().path().map(|p| p.to_path_buf());
                let name = target
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| key.clone());
                // Only a DIFFERENT existing file is an overwrite risk; saving-as to the doc's own path
                // is just a save.
                let overwriting_other = exists && own.as_deref() != Some(target.as_path());
                if overwriting_other && !force && self.blind.is_none() {
                    self.pending_close = Some(PendingClose::SaveAsOverwrite { path: target, name });
                    return Ok(Value::str(""));
                }
                let bytes = self.file_doc().save_bytes();
                self.write_doc_bytes(&key, &bytes)?;
                self.sync_color_sidecar(self.file_doc(), &key, &bytes);
                self.file_doc_mut().set_path(target);
                self.file_doc_mut().mark_saved();
                return Ok(Value::str(self.tr_fmt("msg.save.written", "saved {name}", &[("name", &name)])));
            }
            EditorOp::ClearKbd => {
                // "Clear" = RESET to the small built-in baseline KBD (a usable editing floor), NOT
                // an empty map — an empty map leaves the GUI unable to edit, since Backspace/arrows/
                // Enter/etc. are all KBD-defined commands (only raw printables bypass the KBD). The
                // baseline is a real KBD loaded through the normal loader; the editor/GUI pick it up
                // via take_pending_kbd / take_reloaded_keymap, same path as LOADKBD.
                match load(MINIMAL_KBD, VIRTUAL_BASE) {
                    LoadOutcome::Installed(km) => {
                        self.kbd_modes.insert(self.kbd_mode.clone(), km.clone()); // reset the active slot
                        self.pending_kbd = Some(km);
                        self.last_kbd = None; // built in, not a file on disk — $KB clears
                        return Ok(Value::str("keyboard reset to the built-in default"));
                    }
                    LoadOutcome::Rejected(e) => {
                        return Err(XplError::trappable(format!(
                            "CLEARKBD: built-in default failed to load: {e:?}"
                        )))
                    }
                }
            }
            EditorOp::SaveKbd(opt) => {
                // Dump the operative keymap back to a KBD file (inverse of LOADKBD). Mirrors
                // SaveCfg: resolve the path (default if none), serialize, write.
                let km = self.current_keymap.as_ref().ok_or_else(|| {
                    XplError::trappable("SAVEKBD: no keymap is loaded")
                })?;
                let text = to_os_newlines(&crate::kbd_dump::dump_keymap(km));
                let path = opt
                    .filter(|p| !p.trim().is_empty())
                    .unwrap_or_else(|| DEFAULT_KBD_DUMP.to_string());
                let resolved = if self.virtual_fs {
                    path.clone()
                } else {
                    self.resolve_path(&path).to_string_lossy().into_owned()
                };
                self.write_file(&resolved, &text)?;
                return Ok(Value::str(format!("saved keymap to {path}")));
            }
            EditorOp::SaveKbdToString => {
                // CAPTURE variant of SAVEKBD: return the keymap-dump text instead of writing a file.
                let km = self
                    .current_keymap
                    .as_ref()
                    .ok_or_else(|| XplError::trappable("SAVEKBD: no keymap is loaded"))?;
                return Ok(Value::str(crate::kbd_dump::dump_keymap(km)));
            }
            EditorOp::SaveAsPrompt => {
                // The engine can't open a dialog; record the request for the GUI to drain.
                self.save_as_requested = true;
                return Ok(Value::str(""));
            }
            EditorOp::OpenPrompt { hex } => {
                self.open_requested = Some(hex);
                return Ok(Value::str(""));
            }
            EditorOp::OpenFind => {
                self.find_requested = true;
                return Ok(Value::str(""));
            }
            EditorOp::OpenScratch { paste } => {
                // `/V` prefills the window with the clipboard ($CB, kept synced with the OS clipboard).
                let init = if paste { self.clipboard.clone() } else { String::new() };
                self.scratch_open_queue.push(init);
                return Ok(Value::str(""));
            }
            EditorOp::CloseScratch { insert } => {
                self.scratch_close_requested = Some(insert);
                return Ok(Value::str(""));
            }
            EditorOp::OpenConsole { show } => {
                if show {
                    let idx = self.shell_output_doc_index();
                    self.shell_output_visible = true;
                    if !self.console_showing {
                        self.console_write_prompt();
                    }
                    // Land the caret at the prompt (the tail) so the user can type immediately, and
                    // reveal it (a fresh SH shouldn't leave the view scrolled up in old output).
                    let end = self.docs[idx].buffer().len();
                    self.docs[idx].set_caret(end, None);
                    self.console_dirty = true;
                    self.console_reveal_tail = true; // force the view to the prompt (bypass scroll-lock)
                    self.active = idx;
                    self.set_focus(Target::Document);
                    return Ok(Value::str("console"));
                }
                // `SH off` — hide the Console tab (mirrors `SW off`), stepping focus away if it's active.
                if self.shell_output_visible {
                    self.shell_output_visible = false;
                    self.focus_away_from_shell_output();
                }
                return Ok(Value::str("console hidden"));
            }
            EditorOp::SetConsolePrompt(tmpl) => {
                self.set_console_prompt(tmpl.as_deref());
                return Ok(Value::str(format!("SHPROMPT {}", self.console_prompt)));
            }
            EditorOp::SetAnsiColor(opt) => {
                self.ansi_color = opt.unwrap_or(!self.ansi_color);
                self.console_ansi.reset(); // a mid-command flip shouldn't carry a partial escape
                return Ok(Value::str(format!(
                    "ANSICOLOR {}",
                    if self.ansi_color { "on" } else { "off" }
                )));
            }
            EditorOp::ConsoleClear => {
                self.console_clear();
                return Ok(Value::str("console cleared"));
            }
            EditorOp::OpenGrepDialog => {
                self.grep_dialog_requested = true;
                return Ok(Value::str(""));
            }
            EditorOp::EditorFontPicker => {
                self.font_picker_request = Some(FontPickerKind::Editor);
                return Ok(Value::str(""));
            }
            EditorOp::EditorPropFontPicker => {
                self.font_picker_request = Some(FontPickerKind::EditorProp);
                return Ok(Value::str(""));
            }
            EditorOp::UiFontPicker => {
                self.font_picker_request = Some(FontPickerKind::Ui);
                return Ok(Value::str(""));
            }
            EditorOp::TxtFontPicker => {
                self.font_picker_request = Some(FontPickerKind::Txt);
                return Ok(Value::str(""));
            }
            EditorOp::OpenColorPicker(ground) => {
                self.color_picker_request = Some(match ground {
                    ColorGround::Fg => ColorTarget::Fg,
                    ColorGround::Bg => ColorTarget::Bg,
                    ColorGround::UiFg => ColorTarget::UiFg,
                    ColorGround::UiBg => ColorTarget::UiBg,
                    ColorGround::Caret => ColorTarget::Caret,
                    ColorGround::CmlineCaret => ColorTarget::CmlineCaret,
                    ColorGround::Separator => ColorTarget::Separator,
                    ColorGround::TabClose => ColorTarget::TabClose,
                    ColorGround::SelBg => ColorTarget::SelBg,
                    ColorGround::SelFg => ColorTarget::SelFg,
                    ColorGround::FindBg => ColorTarget::FindBg,
                    ColorGround::FindFg => ColorTarget::FindFg,
                    ColorGround::SpellUnderline => ColorTarget::SpellUnderline,
                    ColorGround::GrammarUnderline => ColorTarget::GrammarUnderline,
                    ColorGround::PaneBorder => ColorTarget::PaneBorder,
                    ColorGround::BalPairs => ColorTarget::BalPairs,
                    ColorGround::TxtFg => ColorTarget::TxtFg,
                    ColorGround::TxtBg => ColorTarget::TxtBg,
                    ColorGround::DwFg => ColorTarget::DwFg,
                    ColorGround::DwBg => ColorTarget::DwBg,
                    ColorGround::EmphFg => ColorTarget::EmphFg,
                    ColorGround::EmphBg => ColorTarget::EmphBg,
                    ColorGround::MdaFg => ColorTarget::MdaFg,
                    ColorGround::MdaBg => ColorTarget::MdaBg,
                });
                return Ok(Value::str(""));
            }
            EditorOp::OpenHighlightColorPicker { name, bg } => {
                let k = crate::highlight::TokenKind::from_name(&name);
                self.color_picker_request =
                    Some(if bg { ColorTarget::HighlightBg(k) } else { ColorTarget::Highlight(k) });
                return Ok(Value::str(""));
            }
            EditorOp::OpenTabColorPicker(state) => {
                let Some(st) = TabState::from_name(&state) else {
                    return Err(XplError::trappable(format!(
                        "TABBGCOLORS: unknown state {:?} (active|hover|inactive)",
                        state.trim()
                    )));
                };
                self.color_picker_request = Some(ColorTarget::Tab(st));
                return Ok(Value::str(""));
            }
            EditorOp::Merge(path) => {
                if path.trim().is_empty() {
                    return Err(XplError::trappable("MERGE: a file is required"));
                }
                let text = self.read_file(&path)?; // resolves $CWD; trappable if missing/non-UTF-8
                let chars = text.chars().count();
                self.active().insert(&text); // at the caret of the active (or blind) buffer
                let name = Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or(path);
                return Ok(Value::str(format!("merged {chars} chars from {name}")));
            }
            EditorOp::AutoReplace => {
                // The word ending at the caret; replace it if it's a key.
                let found = {
                    let b = self.docs[self.active].buffer();
                    b.word_before_cursor().map(|(s, e)| (s, e, b.slice(s, e)))
                };
                if let Some((s, e, word)) = found {
                    // An EXACT key wins and is used verbatim — defining both `equ` and `EQU` keeps
                    // each one's literal value. Otherwise fall back to a case-INSENSITIVE match and
                    // recase the expansion to match how the key was typed, so a single
                    // `equ=equilibrium` serves `equ`→equilibrium, `Equ`→Equilibrium, `EQU`→EQUILIBRIUM.
                    let hit = self.auto_replace.get(&word).map(|v| (v.clone(), KeyCase::AsIs)).or_else(|| {
                        let lower = word.to_lowercase();
                        // A HashMap has no stable iteration order, so pick deterministically:
                        // an all-lowercase key wins, then the lexicographically smallest.
                        self.auto_replace
                            .iter()
                            .filter(|(k, _)| k.to_lowercase() == lower)
                            .min_by(|(a, _), (b, _)| {
                                let rank = |k: &String| (k.chars().any(|c| c.is_uppercase()), k.clone());
                                rank(a).cmp(&rank(b))
                            })
                            .map(|(_, v)| (v.clone(), key_case(&word)))
                    });
                    if let Some((val, case)) = hit {
                        if let Some(xpl) = val.strip_prefix('#') {
                            // XPL ACTION: delete the matched key, then queue the XPL for the VM to
                            // run (and abort the rest of the AR chain). E.g. `pd=#DA;PW`. The value
                            // is CODE, so it is never recased — only the key match is case-loose.
                            self.docs[self.active].replace_span(s, e, "");
                            self.ar_pending_xpl = Some(xpl.to_string());
                            return Ok(Value::str(""));
                        }
                        // Plain text replacement, recased to match the typed key (`AsIs` for an
                        // exact hit, so an explicitly-cased entry is reproduced verbatim).
                        let val = apply_key_case(case, &val);
                        self.docs[self.active].replace_span(s, e, &val);
                        return Ok(Value::str(format!("AR: {word} → {val}")));
                    }
                }
                return Ok(Value::str("")); // no word / no match → silent no-op
            }
            EditorOp::LoadAutoReplace(path) => {
                if path.trim().is_empty() {
                    return Err(XplError::trappable("LoadAR: a file is required"));
                }
                let content = self.read_file(&path)?; // resolves $CWD; trappable if missing
                // `$AR` = the fully-qualified path (lexically resolved like CD/$CF; a `subst`ed
                // drive is preserved rather than rewritten to its backing path).
                let resolved = lexical_abs(&self.cwd, &self.resolve_path(&path));
                self.last_ar_path = Some(resolved.to_string_lossy().into_owned());
                let mut added = 0;
                for line in content.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                        continue; // blank or comment line (`#` or `;`)
                    }
                    if let Some((k, v)) = line.split_once('=') {
                        let key = k.trim();
                        if !key.is_empty() {
                            self.auto_replace.insert(key.to_string(), decode_ar_escapes(v.trim()));
                            added += 1;
                        }
                    }
                }
                return Ok(Value::str(format!(
                    "LoadAR: {added} pair(s); {} loaded",
                    self.auto_replace.len()
                )));
            }
            EditorOp::SaveAutoReplace(opt) => {
                let path = opt.filter(|p| !p.trim().is_empty()).unwrap_or_else(|| DEFAULT_AR.into());
                let resolved = if self.virtual_fs {
                    path.clone()
                } else {
                    self.resolve_path(&path).to_string_lossy().into_owned()
                };
                self.write_file(&resolved, &to_os_newlines(&self.ar_dump_text()))?;
                return Ok(Value::str(format!("SAVEAR: wrote {path}")));
            }
            EditorOp::SaveAutoReplaceToString => {
                // CAPTURE variant of SAVEAR: return the file text instead of writing it.
                return Ok(Value::str(self.ar_dump_text()));
            }
            EditorOp::ClearAutoReplace => {
                self.auto_replace.clear();
                self.last_ar_path = None;
                return Ok(Value::str("auto-replace cleared"));
            }
            EditorOp::SaveSelection { path, force } => {
                if path.trim().is_empty() {
                    return Err(XplError::trappable("SAD: a path is required"));
                }
                let b = self.file_doc().buffer();
                let (a, c) = b
                    .selection_span()
                    .ok_or_else(|| XplError::trappable_code(ErrCode::NothingSelected, "SAD: no text is selected"))?;
                let bytes = b.slice_bytes(a, c);
                let target: PathBuf =
                    if self.virtual_fs { PathBuf::from(&path) } else { self.resolve_path(&path) };
                let key = target.to_string_lossy().into_owned();
                let exists =
                    if self.virtual_fs { self.files.contains_key(&key) } else { target.exists() };
                let name = target
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| key.clone());
                // Confirm before clobbering an existing file unless `/NV` (force) or a blind edit.
                if exists && !force && self.blind.is_none() {
                    self.pending_close =
                        Some(PendingClose::SaveSelectionOverwrite { path: target, name, bytes });
                    return Ok(Value::str(""));
                }
                self.write_doc_bytes(&key, &bytes)?;
                return Ok(Value::str(format!("saved selection to {name}")));
            }
            EditorOp::Revert => {
                // The common case reverts the visible document — reuse the shared reload path (which
                // preserves the caret + viewport and re-baselines the AUTOREOPEN mtime). A blind edit
                // (BEDIT) reverts its hidden buffer inline, since it isn't in `self.docs`.
                if self.blind.is_none() {
                    let name = self.reload_doc_from_disk(self.active)?;
                    return Ok(Value::str(format!("reverted to {name}")));
                }
                let path = match self.file_doc().path() {
                    Some(p) => p.to_path_buf(),
                    None => {
                        return Err(XplError::trappable("REC: nothing to revert to (never saved)"))
                    }
                };
                let key = path.to_string_lossy().into_owned();
                let bytes = self.read_doc_bytes(&key)?;
                let restore = (!self.virtual_fs).then(|| xymeta_read(&path, &bytes)).flatten();
                let old_caret = self.file_doc().buffer().cursor();
                self.file_doc_mut().load(bytes, path.clone());
                if let Some(r) = restore {
                    apply_xymeta_restore(self.file_doc_mut(), r);
                }
                let len = self.file_doc().buffer().len();
                self.file_doc_mut().set_caret(old_caret.min(len), None);
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or(key);
                return Ok(Value::str(format!("reverted to {name}")));
            }
            EditorOp::DupView { window } => {
                // A second view needs a file on disk to point at (both the in-process tab and the new
                // window open it by path). The on-disk content is used, so unsaved edits in this buffer
                // aren't reflected until you SAVE (then AUTOREOPEN can sync the views).
                let path = match self.file_doc().path() {
                    Some(p) => p.to_path_buf(),
                    None => {
                        return Err(XplError::trappable(
                            "DUPVIEW: save the file first — there is no file on disk to open a second view of",
                        ))
                    }
                };
                let caret = self.file_doc().buffer().cursor();
                let line = self.file_doc().buffer().line_of(caret) + 1; // 1-based, for the window's --goto
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
                if window {
                    self.spawn_second_window(&path, line)?;
                    return Ok(Value::str(format!("opened a new window on {name} (line {line})")));
                }
                // In-process: open a fresh, distinct buffer of the same file (force = no "already open"
                // prompt), then land the caret where it was.
                self.open_doc(&path.to_string_lossy(), false, false)?;
                let len = self.active().buffer().len();
                self.active().set_caret(caret.min(len), None);
                return Ok(Value::str(format!("second view of {name}")));
            }
            EditorOp::SetReadOnly(kind) => {
                let doc = &mut self.docs[self.active];
                let on = match kind {
                    ViewToggle::On => true,
                    ViewToggle::Off => false,
                    ViewToggle::Toggle => !doc.read_only(),
                };
                doc.set_read_only(on);
                return Ok(Value::str(if on { "read-only on" } else { "read-only off" }));
            }
            EditorOp::SetLock { key, mode } => {
                let (vk, name) = match key {
                    LockKey::Caps => (VK_CAPITAL, "CapsLock"),
                    LockKey::Num => (VK_NUMLOCK, "NumLock"),
                    LockKey::Scroll => (VK_SCROLL, "ScrollLock"),
                };
                let want = match mode {
                    ViewToggle::On => Some(true),
                    ViewToggle::Off => Some(false),
                    ViewToggle::Toggle => None,
                };
                let now = set_lock_key(vk, want);
                return Ok(Value::str(format!("{name} {}", if now { "on" } else { "off" })));
            }
            EditorOp::CloseBuffer => {
                // During a blind edit, `AB` exits it (discarding — `SA` first to keep changes),
                // leaving the visible buffers untouched.
                if self.exit_blind_if_open() {
                    return Ok(Value::str("closed blind edit"));
                }
                // Close the file AND, if it was in a split pane, undo that pane — a single operation
                // (`close_active_buffer` handles the pane). Un-splitting WITHOUT closing is `TUS`.
                let name = self.close_active_buffer();
                return Ok(Value::str(format!("closed {name}")));
            }
            EditorOp::CloseBufferVerify => {
                // `AB/V` while blind has no visible buffer to prompt about → behave like `AB`.
                if self.exit_blind_if_open() {
                    return Ok(Value::str("closed blind edit"));
                }
                if !self.docs[self.active].is_modified() {
                    let name = self.close_active_buffer();
                    return Ok(Value::str(format!("closed {name}")));
                }
                let name =
                    self.docs[self.active].file_name().unwrap_or_else(|| "(untitled)".to_string());
                self.pending_close = Some(PendingClose::Save { name });
                return Ok(Value::str("")); // the GUI shows close_prompt() + sends the answer
            }
            EditorOp::DeleteNote { verify } => {
                let name =
                    self.docs[self.active].file_name().unwrap_or_else(|| "(untitled)".to_string());
                let path = match self.docs[self.active].path() {
                    Some(p) => p.to_path_buf(),
                    // Never saved: there's no file to delete, but the user's intent is to close the
                    // window — so do that, just note the absence on the status line.
                    None => {
                        let _ = self.close_active_buffer();
                        return Ok(Value::str(format!(
                            "ABD: no file on disk (never saved); closed {name}"
                        )));
                    }
                };
                // The file was saved once but is no longer on disk (deleted elsewhere): there's
                // nothing to delete, so `ABD`/`ABD/V` quietly close like `AB` — no error, and no
                // confirm prompt for `/V` (checked here, before the verify branch).
                let on_disk = if self.virtual_fs {
                    self.files.contains_key(&path.to_string_lossy().into_owned())
                } else {
                    path.exists()
                };
                if !on_disk {
                    let _ = self.close_active_buffer();
                    return Ok(Value::str(format!("closed {name}")));
                }
                if verify {
                    // `ABD/V`: arm a confirm the GUI (or headless caller) resolves.
                    self.pending_close = Some(PendingClose::DeleteNote { path, name });
                    return Ok(Value::str(""));
                }
                // `ABD`: delete + close immediately.
                return Ok(Value::str(self.delete_note_now(&path, &name)));
            }
            EditorOp::SaveAll => {
                let (saved, skipped) = self.save_all_modified()?;
                return Ok(Value::str(format!("saved {saved}, skipped {skipped} unnamed")));
            }
            EditorOp::SaveCloseAll => {
                let (saved, _) = self.save_all_modified()?;
                let before = self.docs.len();
                // Keep only buffers still modified (the unnamed ones that couldn't be saved);
                // close everything now clean.
                self.docs.retain(|d| d.is_modified());
                let kept = self.docs.len();
                if self.docs.is_empty() {
                    self.docs.push(Document::new());
                }
                if self.active >= self.docs.len() {
                    self.active = self.docs.len() - 1;
                }
                self.set_focus(Target::Document);
                let closed = before - kept;
                return Ok(Value::str(format!("saved {saved}, closed {closed}, kept {kept} unsaved")));
            }
            EditorOp::SaveCfg(opt) => {
                let path = opt
                    .filter(|p| !p.trim().is_empty())
                    .unwrap_or_else(|| DEFAULT_CFG.to_string());
                let resolved = if self.virtual_fs {
                    path.clone()
                } else {
                    self.resolve_path(&path).to_string_lossy().into_owned()
                };
                let text = self.config_script(false, true); // manual SAVECFG includes command history
                self.write_file(&resolved, &text)?;
                return Ok(Value::str(format!("saved config to {path}")));
            }
            EditorOp::SaveCfgToString => {
                // CAPTURE variant of SAVECFG: return the config-script text instead of writing a file.
                return Ok(Value::str(self.config_script(false, true)));
            }
            EditorOp::SaveHistory(opt) => {
                // `SaveCM [path]` — write ONLY the command history to a runnable XPL script (a
                // `LoadCM "…"` line). `RUN <file>` restores it. Default path = `xyscript-history.xpl`.
                let path = opt
                    .filter(|p| !p.trim().is_empty())
                    .unwrap_or_else(|| DEFAULT_CM.to_string());
                let resolved = if self.virtual_fs {
                    path.clone()
                } else {
                    self.resolve_path(&path).to_string_lossy().into_owned()
                };
                let text = to_os_newlines(&self.history_script_text());
                self.write_file(&resolved, &text)?;
                let n = self.command_history.len();
                return Ok(Value::str(format!(
                    "saved {n} history command{} to {path}",
                    if n == 1 { "" } else { "s" }
                )));
            }
            EditorOp::SaveHistoryToString => {
                // CAPTURE variant of SAVECM: return the script text instead of writing a file.
                return Ok(Value::str(self.history_script_text()));
            }
            EditorOp::SaveLdtx(opt) => {
                // `SAVELDTX [path]` — write all non-empty text registers as a runnable program of
                // `LDTX <slot> "<literal>"` lines. `RUN <file>` restores them. Default file below.
                let path = opt
                    .filter(|p| !p.trim().is_empty())
                    .unwrap_or_else(|| DEFAULT_LDTX.to_string());
                let resolved = if self.virtual_fs {
                    path.clone()
                } else {
                    self.resolve_path(&path).to_string_lossy().into_owned()
                };
                let text = to_os_newlines(&self.text_binder_script_text());
                self.write_file(&resolved, &text)?;
                let n = self.text_binders.values().filter(|v| !v.is_empty()).count();
                return Ok(Value::str(format!(
                    "saved {n} text register{} to {path}",
                    if n == 1 { "" } else { "s" }
                )));
            }
            EditorOp::SaveLdtxToString => {
                // CAPTURE variant of SAVELDTX: return the script text instead of writing a file.
                return Ok(Value::str(self.text_binder_script_text()));
            }
            EditorOp::SaveLdsc(opt) => {
                // `SAVELDSC`/`SAVELDPM [path]` — write all non-empty script registers as a runnable
                // program of `LDSC <slot> "<xpl>"` lines. `RUN <file>` restores them.
                let path = opt
                    .filter(|p| !p.trim().is_empty())
                    .unwrap_or_else(|| DEFAULT_LDSC.to_string());
                let resolved = if self.virtual_fs {
                    path.clone()
                } else {
                    self.resolve_path(&path).to_string_lossy().into_owned()
                };
                let text = to_os_newlines(&self.script_binder_script_text());
                self.write_file(&resolved, &text)?;
                let n = self.script_binders.values().filter(|v| !v.is_empty()).count();
                return Ok(Value::str(format!(
                    "saved {n} script register{} to {path}",
                    if n == 1 { "" } else { "s" }
                )));
            }
            EditorOp::SaveLdscToString => {
                // CAPTURE variant of SAVELDSC: return the script text instead of writing a file.
                return Ok(Value::str(self.script_binder_script_text()));
            }
            EditorOp::LoadClosed(text) => {
                self.recently_closed = text
                    .split('\n')
                    .filter(|l| !l.trim().is_empty())
                    .filter_map(|line| {
                        let (caret, path) = line.split_once('\t')?;
                        let caret = caret.trim().parse::<usize>().ok()?;
                        Some((PathBuf::from(path), caret))
                    })
                    .take(REOPEN_CAP)
                    .collect();
                let n = self.recently_closed.len();
                return Ok(Value::str(format!(
                    "loaded {n} recently-closed file{}",
                    if n == 1 { "" } else { "s" }
                )));
            }
            EditorOp::SaveClosed(opt) => {
                // `SaveClosed [path]` — write ONLY the reopen list to a runnable `LoadClosed` script.
                let path = opt
                    .filter(|p| !p.trim().is_empty())
                    .unwrap_or_else(|| DEFAULT_CLOSED.to_string());
                let resolved = if self.virtual_fs {
                    path.clone()
                } else {
                    self.resolve_path(&path).to_string_lossy().into_owned()
                };
                let text = to_os_newlines(&self.reopen_script_text());
                self.write_file(&resolved, &text)?;
                let n = self.recently_closed.len();
                return Ok(Value::str(format!(
                    "saved {n} recently-closed file{} to {path}",
                    if n == 1 { "" } else { "s" }
                )));
            }
            EditorOp::SaveClosedToString => {
                return Ok(Value::str(self.reopen_script_text()));
            }
            EditorOp::ReopenRecent(n) => {
                if self.recently_closed.is_empty() {
                    return Err(XplError::trappable("REOPEN: no recently-closed file"));
                }
                // Snapshot the first `n` (each `reopen_file` mutates the list). Reopen the OLDEST of
                // the batch first so the MOST-recent ends up active — matching a manual reopen chain.
                let n = n.min(self.recently_closed.len());
                let batch: Vec<(String, usize)> = self
                    .recently_closed
                    .iter()
                    .take(n)
                    .map(|(p, c)| (p.to_string_lossy().into_owned(), *c))
                    .collect();
                let mut opened = 0usize;
                for (path, caret) in batch.into_iter().rev() {
                    // Best-effort: a file gone since it was closed is skipped (and dropped from the
                    // list by `reopen_file`), not fatal, so the rest of the batch still reopens.
                    if self.reopen_file(&path, caret).is_ok() {
                        opened += 1;
                    }
                }
                return Ok(Value::str(format!(
                    "reopened {opened} file{}",
                    if opened == 1 { "" } else { "s" }
                )));
            }
            EditorOp::ClearClosed => {
                let n = self.recently_closed.len();
                self.recently_closed.clear();
                return Ok(Value::str(format!(
                    "cleared {n} recently-closed entr{}",
                    if n == 1 { "y" } else { "ies" }
                )));
            }
            EditorOp::PushHistory(cmd) => {
                // `CMH [command]` — stash a command at the top of history (push_history trims, dedups,
                // moves-to-front, and skips blanks). Silent — no status (KBD-binding friendly). With no
                // argument, stash the current cmline; with one, stash that literal command.
                let text = cmd.unwrap_or_else(|| self.command_line().text());
                self.push_history(&text);
                return Ok(Value::str(""));
            }
            EditorOp::ConfigTrack { on, path } => {
                if !on {
                    self.config_track_path = None;
                    self.config_last_written.clear();
                    return Ok(Value::str("config tracking off"));
                }
                let path = path
                    .filter(|p| !p.trim().is_empty())
                    .unwrap_or_else(|| DEFAULT_CFG.to_string());
                let resolved = if self.virtual_fs {
                    path.clone()
                } else {
                    self.resolve_path(&path).to_string_lossy().into_owned()
                };
                // Write once now, then mirror on every settings change (settings only — no history).
                let text = self.config_script(false, false);
                self.write_file(&resolved, &text)?;
                self.config_last_written = text;
                self.config_track_path = Some(resolved);
                return Ok(Value::str(format!("tracking config to {path}")));
            }
            EditorOp::LoadCfg(opt) => {
                let path = opt
                    .filter(|p| !p.trim().is_empty())
                    .unwrap_or_else(|| DEFAULT_CFG.to_string());
                let resolved = if self.virtual_fs {
                    path.clone()
                } else {
                    self.resolve_path(&path).to_string_lossy().into_owned()
                };
                let content = self.read_file(&resolved)?;
                self.last_loadcfg_path = Some(resolved); // $CF
                // It's just XPL — run it after the current script returns (the RUNHERE pattern).
                self.pending_run.push((content, Vec::new()));
                // The config's own commands would clobber any status we set now, so confirm the load
                // AFTER it runs, as a final queued line — so LOADCFG reports like LOADLIB/LOADAR.
                // (`$CF` is the resolved path just set above.)
                self.pending_run.push(("PR loaded config from $CF".to_string(), Vec::new()));
                return Ok(Value::str(format!("loading config from {path}")));
            }
            EditorOp::LoadLang(opt) => {
                // `LANG off` / bare `LANG` (None) clears the table → built-in English.
                let Some(path) = opt.filter(|p| !p.trim().is_empty()) else {
                    self.lang.clear();
                    self.lang_path = None;
                    self.lang_overlay_path = None;
                    return Ok(Value::str("language reset to English".to_string()));
                };
                // Two-letter language-code shorthand: `LANG fi` → `fi.lang` (resolved against `$CWD`
                // like any relative path). Give a name with an extension or path to load a file
                // directly (`LANG fr.txt`, `LANG i18n\de.lang`) — that form is untouched.
                let code = path.trim();
                let path = if code.len() == 2 && code.bytes().all(|b| b.is_ascii_alphabetic()) {
                    format!("{code}.lang")
                } else {
                    path
                };
                let resolved = if self.virtual_fs {
                    path.clone()
                } else {
                    self.resolve_path(&path).to_string_lossy().into_owned()
                };
                let content = self.read_file(&resolved)?;
                // Base catalog: values populate the live table; the English anchors are kept for the
                // overlay's self-heal check below.
                let base = parse_lang_entries(&content);
                self.lang = base
                    .iter()
                    .filter(|(_, v, _)| !v.is_empty())
                    .map(|(k, v, _)| (k.clone(), v.clone()))
                    .collect();
                self.lang_path = Some(resolved.clone());
                self.lang_overlay_path = None;
                // Overlay: a sibling `<code>.local.lang` of user overrides (next to the base, or in
                // `$CWD`), layered on top. An override whose `# English` anchor no longer matches the
                // base is SKIPPED (self-heal) — so a stale override steps aside for the updated base
                // rather than showing an outdated translation of text whose English has moved on.
                let stem = std::path::Path::new(&resolved)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let (mut applied, mut stale) = (0usize, 0usize);
                if !stem.is_empty() {
                    if let Some((over_path, over_text)) = self.find_overlay(&resolved, &stem) {
                        let base_anchor: HashMap<&str, &str> = base
                            .iter()
                            .filter_map(|(k, _, a)| a.as_deref().map(|a| (k.as_str(), a)))
                            .collect();
                        for (key, value, anchor) in parse_lang_entries(&over_text) {
                            if value.is_empty() {
                                continue;
                            }
                            if let (Some(ov), Some(&cur)) = (anchor.as_deref(), base_anchor.get(key.as_str())) {
                                if ov != cur {
                                    stale += 1;
                                    continue; // written against different English — let the base win
                                }
                            }
                            self.lang.insert(key, value);
                            applied += 1;
                        }
                        self.lang_overlay_path = Some(over_path);
                    }
                }
                let total = self.lang.len();
                let s_suffix = if applied == 1 { "" } else { "s" };
                let msg = match (applied, stale) {
                    (0, _) => format!("loaded {total} UI strings from {path}"),
                    (a, 0) => format!("loaded {total} UI strings from {path} + {a} override{s_suffix}"),
                    (a, s) => format!(
                        "loaded {total} UI strings from {path} + {a} override{s_suffix} ({s} skipped: English changed)"
                    ),
                };
                return Ok(Value::str(msg));
            }
            EditorOp::SetTabWidth(n) => {
                self.tab_width = n;
                return Ok(Value::str(format!("tab width {n}")));
            }
            EditorOp::Tab { out } => {
                // Indent unit for the LINE-BASED indent/outdent (selection, or `TAB out`): soft tabs
                // → `tab_width` spaces, else a tab character. Each touched line gets a whole unit
                // added/removed at its start, so a full-width unit is correct there.
                let unit = if self.tab_soft {
                    " ".repeat(self.tab_width)
                } else {
                    "\t".to_string()
                };
                let width = self.tab_width;
                let soft = self.tab_soft;
                let doc = self.active();
                // `TAB in` with no selection inserts at the CARET; everything else is a line-based
                // indent/outdent of the touched lines.
                if !out && doc.buffer().selection_span().is_none() {
                    if soft {
                        // Soft tab at the caret pads to the NEXT tab stop, not a blind `tab_width`
                        // spaces: if the caret is already `d` columns into a stop, only `tab_width -
                        // d` spaces are needed so the text lands on the stop (a real Tab key's
                        // behaviour). `d == 0` (already on a stop) inserts a full `tab_width`.
                        let col = doc.buffer().column();
                        let pad = width - (col % width);
                        doc.insert(&" ".repeat(pad));
                    } else {
                        doc.insert(&unit);
                    }
                } else {
                    doc.indent(out, &unit, width);
                }
                return Ok(Value::str(""));
            }
            EditorOp::SetTabSpaces(on) => {
                self.tab_soft = on;
                return Ok(Value::str(format!("soft tabs {}", if on { "on" } else { "off" })));
            }
            EditorOp::SetTabIndent(on) => {
                self.tab_indent = on;
                return Ok(Value::str(format!("auto-indent {}", if on { "on" } else { "off" })));
            }
            EditorOp::SetBalPairs(on) => {
                self.bal_pairs = on;
                return Ok(Value::str(format!("bracket matching {}", if on { "on" } else { "off" })));
            }
            EditorOp::SetBalColor(color) => {
                self.bal_color = color;
                return Ok(Value::str(match color {
                    Some((r, g, b)) => format!("bracket highlight colour = {r} {g} {b}"),
                    None => "bracket highlight colour = selection colour".to_string(),
                }));
            }
            EditorOp::SetEmph { phrase, regex } => {
                // A bad `EMPH/R` regex is reported rather than silently matching nothing; the emphasis
                // is left unchanged so the user can correct it.
                if regex {
                    if let Some(p) = phrase.as_deref().filter(|p| !p.is_empty()) {
                        if Pattern::build(p, false, false, true, false).is_err() {
                            return Err(XplError::trappable(format!(
                                "EMPH/R: invalid regular expression: {p}"
                            )));
                        }
                    }
                }
                self.docs[self.active].set_emph(phrase.clone(), regex);
                return Ok(Value::str(match phrase {
                    Some(p) if !p.is_empty() && regex => format!("emphasis (regex): {p:?}"),
                    Some(p) if !p.is_empty() => format!("emphasis: {p:?}"),
                    _ => "emphasis cleared".to_string(),
                }));
            }
            EditorOp::SetEmphFgColor(color) => {
                self.emph_fg = color;
                return Ok(Value::str(match color {
                    Some((r, g, b)) => format!("emphasis text colour = {r} {g} {b}"),
                    None => "emphasis text colour = selection colour".to_string(),
                }));
            }
            EditorOp::SetEmphBgColor(color) => {
                self.emph_bg = color;
                return Ok(Value::str(match color {
                    Some((r, g, b)) => format!("emphasis background colour = {r} {g} {b}"),
                    None => "emphasis background colour = selection colour".to_string(),
                }));
            }
            EditorOp::SetTypePairs(on) => {
                self.type_pairs = on;
                return Ok(Value::str(format!("auto-close pairs {}", if on { "on" } else { "off" })));
            }
            EditorOp::SetWrapIndent(on) => {
                self.wrap_indent = on;
                return Ok(Value::str(format!("wrap-indent {}", if on { "on" } else { "off" })));
            }
            EditorOp::SetDecimals(dp) => {
                self.float_decimals = dp;
                return Ok(Value::str(match dp {
                    Some(n) => format!("decimal places {n}"),
                    None => "decimal places off (default precision)".to_string(),
                }));
            }
            EditorOp::ScrollView(rows) => {
                self.pending_scroll_rows = self.pending_scroll_rows.saturating_add(rows);
                return Ok(Value::str(""));
            }
            EditorOp::SetAutosave(secs) => {
                self.autosave_secs = secs;
                return Ok(Value::str(if secs == 0 {
                    "autosave off".to_string()
                } else {
                    format!("autosave every {secs}s")
                }));
            }
            EditorOp::SetQuiet(level) => {
                self.quiet = level;
                // No status (it would itself be a suppressible op-result; QUIET stays silent).
                return Ok(Value::str(""));
            }
            EditorOp::ShellBg { cmd, return_var } => {
                if !self.shell_trusted {
                    return Err(XplError::trappable(
                        "shell-out is disabled; enable it with TRUST 1",
                    ));
                }
                let id = self.next_job_id;
                self.next_job_id += 1;
                self.last_job_id = id; // `$LJ` — so a script can `WAIT/J $LJ` on it
                let (tx, rx) = std::sync::mpsc::sync_channel(JOB_CHANNEL_CAP);
                let child = std::sync::Arc::new(std::sync::Mutex::new(None));
                spawn_shell_job(shell_command(&cmd), None, None, tx, std::sync::Arc::clone(&child));
                self.jobs.push(Job { id, cmd: cmd.clone(), rx, child, output: String::new(), return_var, sink: JobSink::ShellTab, cleanup: None, is_mail: false });
                // Stream into the *Shell Output* tab under a header, live as the job runs. With SW
                // on, switch to that tab so the output is visible; with SW off it streams hidden.
                self.start_shell_output_log(&format!("$ {cmd}  (job {id})\n"));
                return Ok(Value::str(format!("job {id} started: {cmd}")));
            }
            EditorOp::RunBackgroundScript { path, args, return_var } => {
                // Spawn `$ED --run <abs-script>` as a background child process — same job
                // machinery as `X/BG` (streaming, KILL, JOBS, $XO/$RC), but the child is OUR
                // own headless xyscript on the user's script, so it's NOT trust-gated (parity
                // with foreground RUN). The child's cwd = the editor's `$CWD` so the script's
                // relative file ops resolve as the user expects.
                let trimmed = path.trim();
                if trimmed.is_empty() {
                    return Err(XplError::trappable("RUN/BG: a script path is required"));
                }
                let exe = std::env::current_exe().map_err(|e| {
                    XplError::trappable(format!("RUN/BG: cannot find the xyscript executable: {e}"))
                })?;
                let abs = self.resolve_path(trimmed);
                let id = self.next_job_id;
                self.next_job_id += 1;
                self.last_job_id = id; // `$LJ` — so a script can `WAIT/J $LJ` on it
                let (tx, rx) = std::sync::mpsc::sync_channel(JOB_CHANNEL_CAP);
                let child = std::sync::Arc::new(std::sync::Mutex::new(None));
                let mut command = std::process::Command::new(&exe);
                command.arg("--run").arg(&abs).args(&args).current_dir(&self.cwd);
                spawn_shell_job(command, None, None, tx, std::sync::Arc::clone(&child));
                let exe_name = exe
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_else(|| exe.to_string_lossy().into_owned());
                let display = if args.is_empty() {
                    format!("{exe_name} --run {}", abs.display())
                } else {
                    format!("{exe_name} --run {} {}", abs.display(), args.join(" "))
                };
                self.jobs.push(Job {
                    id,
                    cmd: display.clone(),
                    rx,
                    child,
                    output: String::new(),
                    return_var,
                    sink: JobSink::ShellTab,
                    cleanup: None,
                    is_mail: false,
                });
                self.start_shell_output_log(&format!("$ {display}  (job {id})\n"));
                return Ok(Value::str(format!("background script: job {id}: {}", abs.display())));
            }
            EditorOp::RunBackgroundCode { code, return_var } => {
                // Like RunBackgroundScript, but the XPL code (from a variable) is piped to a
                // `$ED --run -` child via stdin — no temp file. Not trust-gated (parity with RUNCODE).
                if code.trim().is_empty() {
                    return Err(XplError::trappable("RUNCODE/BG: no code to run"));
                }
                let exe = std::env::current_exe().map_err(|e| {
                    XplError::trappable(format!("RUNCODE/BG: cannot find the xyscript executable: {e}"))
                })?;
                let id = self.next_job_id;
                self.next_job_id += 1;
                self.last_job_id = id; // `$LJ`
                let (tx, rx) = std::sync::mpsc::sync_channel(JOB_CHANNEL_CAP);
                let child = std::sync::Arc::new(std::sync::Mutex::new(None));
                let mut command = std::process::Command::new(&exe);
                command.arg("--run").arg("-").current_dir(&self.cwd);
                spawn_shell_job(command, Some(code), None, tx, std::sync::Arc::clone(&child));
                let exe_name = exe
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_else(|| exe.to_string_lossy().into_owned());
                let display = format!("{exe_name} --run -  (RUNCODE)");
                self.jobs.push(Job {
                    id,
                    cmd: display.clone(),
                    rx,
                    child,
                    output: String::new(),
                    return_var,
                    sink: JobSink::ShellTab,
                    cleanup: None,
                    is_mail: false,
                });
                self.start_shell_output_log(&format!("$ {display}  (job {id})\n"));
                return Ok(Value::str(format!("background code: job {id}")));
            }
            EditorOp::Kill(id) => {
                return Ok(Value::str(if self.kill_job(id) {
                    format!("killing job {id}")
                } else {
                    format!("no job {id}")
                }));
            }
            EditorOp::ToPdf(opt_path) => {
                // Render the active document (as GFM) to a typeset PDF.
                let path: PathBuf = match opt_path {
                    Some(p) if !p.trim().is_empty() => PathBuf::from(p),
                    _ => match self.docs[self.active].path() {
                        Some(p) => p.with_extension("pdf"),
                        None => {
                            return Err(XplError::trappable(
                                "TOPDF: no file name; use TOPDF <path>",
                            ))
                        }
                    },
                };
                let source = self.docs[self.active].buffer().text();
                let doc_dir = self.typst_doc_dir();
                // Render per the document's effective kind (DOCTYPE override, else by extension).
                let pdf = match self.docs[self.active].render_kind() {
                    crate::document::RenderKind::Typst => {
                        let styler = self.mda_styler(&source);
                        crate::pdf::typst_to_pdf(&source, &doc_dir, Some(&styler))
                    }
                    crate::document::RenderKind::Gfm => {
                        let styler = self.mda_styler(&source);
                        crate::pdf::gfm_to_pdf(&source, &doc_dir, &self.preview_fonts(), Some(&styler))
                    }
                    crate::document::RenderKind::Html => {
                        crate::pdf::html_to_pdf(&source, &doc_dir, &self.preview_fonts())
                    }
                    crate::document::RenderKind::Text => {
                        let styler = self.mda_styler(&source);
                        crate::pdf::text_to_pdf(&source, &doc_dir, &self.preview_fonts(), Some(&styler))
                    }
                }
                .map_err(XplError::trappable)?;
                self.write_doc_bytes(&path.to_string_lossy(), &pdf)?;
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
                return Ok(Value::str(format!("exported {name}")));
            }
            EditorOp::FilePrint { source, output } => {
                // Render a file that isn't open, exactly as `TOPDF` would if it were the active doc:
                // by the SOURCE file's own type (extension → render kind) with the current Preview
                // settings. `#image`/`#read` resolve against the source's own directory. No MDA styling
                // (nothing is loaded), and no tab is opened or disturbed.
                let raw = PathBuf::from(&source); // the name as typed — governs kind + derived output
                let src_path = self.resolve_path(&source); // absolute — for #image resolution
                let text = self.read_file(&source)?; // decoded text; honours $CWD + the cloud guard
                let kind = crate::document::RenderKind::from_path(&raw);
                let doc_dir =
                    src_path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.cwd.clone());
                let pdf = match kind {
                    crate::document::RenderKind::Typst => {
                        crate::pdf::typst_to_pdf(&text, &doc_dir, None)
                    }
                    crate::document::RenderKind::Gfm => {
                        crate::pdf::gfm_to_pdf(&text, &doc_dir, &self.preview_fonts(), None)
                    }
                    crate::document::RenderKind::Html => {
                        crate::pdf::html_to_pdf(&text, &doc_dir, &self.preview_fonts())
                    }
                    crate::document::RenderKind::Text => {
                        // FPRINT renders a file that isn't open — no MDA/marks apply (nothing loaded).
                        crate::pdf::text_to_pdf(&text, &doc_dir, &self.preview_fonts(), None)
                    }
                }
                .map_err(XplError::trappable)?;
                // Output: the given path (relative → $CWD, like every file op), else `<source>.pdf`
                // derived from the name as typed (so a relative source yields a relative PDF).
                let out = match output {
                    Some(o) if !o.trim().is_empty() => o,
                    _ => raw.with_extension("pdf").to_string_lossy().into_owned(),
                };
                self.write_doc_bytes(&out, &pdf)?;
                let out_name = self
                    .resolve_path(&out)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| out.clone());
                return Ok(Value::str(format!("printed as {} \u{2192} {out_name}", kind.as_str())));
            }
            EditorOp::DocType(req) => {
                use crate::document::RenderKind;
                let doc = &mut self.docs[self.active];
                match req {
                    DocTypeReq::Query => {}
                    DocTypeReq::Auto => doc.set_render_override(None),
                    DocTypeReq::Gfm => doc.set_render_override(Some(RenderKind::Gfm)),
                    DocTypeReq::Typst => doc.set_render_override(Some(RenderKind::Typst)),
                    DocTypeReq::Html => doc.set_render_override(Some(RenderKind::Html)),
                    DocTypeReq::Text => doc.set_render_override(Some(RenderKind::Text)),
                }
                let kind = self.docs[self.active].render_kind().as_str();
                let how = if matches!(req, DocTypeReq::Query | DocTypeReq::Auto) {
                    "auto"
                } else {
                    "forced"
                };
                return Ok(Value::str(format!("document type: {kind} ({how})")));
            }
            EditorOp::WordCount => {
                let b = self.docs[self.active].buffer();
                let (n, label) = if b.has_selection() {
                    (b.selection_word_count(), "Selection")
                } else {
                    (b.word_count(), "Document")
                };
                let s = if n == 1 { "" } else { "s" };
                return Ok(Value::str(format!("{label}: {n} word{s}")));
            }
            EditorOp::CharCount => {
                let b = self.docs[self.active].buffer();
                let (n, label) = if b.has_selection() {
                    (b.selection_char_count(), "Selection")
                } else {
                    (b.char_count(), "Document")
                };
                let s = if n == 1 { "" } else { "s" };
                return Ok(Value::str(format!("{label}: {n} character{s}")));
            }
            EditorOp::Sort(spec) => {
                let (n_in, n_out) = self.active().sort_selection(&spec).map_err(XplError::trappable)?;
                let mut flags = String::new();
                if spec.word {
                    flags.push_str("/W");
                }
                if spec.reverse {
                    flags.push_str("/R");
                }
                if spec.dedup {
                    flags.push_str("/D");
                }
                if let Some(f) = spec.f_skip {
                    flags.push_str(&format!("/F:{f}"));
                }
                if let Some(n) = spec.n_take {
                    flags.push_str(&format!("/N:{n}"));
                }
                let msg = if spec.dedup && n_out < n_in {
                    let removed = n_in - n_out;
                    let s = if removed == 1 { "" } else { "s" };
                    format!("SORT{flags}: {n_in} -> {n_out} lines ({removed} dup{s} removed)")
                } else {
                    let s = if n_out == 1 { "" } else { "s" };
                    format!("SORT{flags}: sorted {n_out} line{s}")
                };
                return Ok(Value::str(msg));
            }
            EditorOp::SetHighlight(on) => {
                // Highlighting acts on the active document (not the cmline). The document resolves
                // its own highlighter by extension (a grammar wins, else Markdown).
                self.docs[self.active].set_highlight(on);
                let msg = match (on, self.docs[self.active].highlighter_name()) {
                    (true, Some(name)) => format!("highlight on ({name})"),
                    (true, None) => "highlight: none for this file type".to_string(),
                    (false, _) => "highlight off".to_string(),
                };
                return Ok(Value::str(msg));
            }
            EditorOp::SetHexView(want) => {
                // A per-document canvas view mode (hex + text grid); the bytes are untouched.
                // `None` toggles the active document's current state (bare `HEX`).
                let on = want.unwrap_or_else(|| !self.docs[self.active].hex_view());
                self.docs[self.active].set_hex_view(on);
                return Ok(Value::str(if on { "hex view on" } else { "hex view off" }.to_string()));
            }
            EditorOp::MarkSet => {
                if self.active().mark_set() {
                    let n = self.active().mark_count();
                    return Ok(Value::str(format!("bookmark set ({n} total)")));
                }
                return Err(XplError::trappable("bookmark already set here"));
            }
            EditorOp::MarkClear => {
                if self.active().mark_clear() {
                    let n = self.active().mark_count();
                    return Ok(Value::str(format!("bookmark cleared ({n} left)")));
                }
                return Err(XplError::trappable("no bookmark here"));
            }
            EditorOp::MarkClearAll => {
                let n = self.active().mark_clear_all();
                return Ok(Value::str(match n {
                    0 => "no bookmarks to clear".to_string(),
                    1 => "1 bookmark cleared".to_string(),
                    _ => format!("{n} bookmarks cleared"),
                }));
            }
            EditorOp::MarkToggle => {
                let now = self.active().mark_toggle();
                let n = self.active().mark_count();
                return Ok(Value::str(if now {
                    format!("bookmark set ({n} total)")
                } else {
                    format!("bookmark cleared ({n} left)")
                }));
            }
            EditorOp::MarkNext => {
                let nav = self.active().mark_next();
                return self.mark_nav_status(nav);
            }
            EditorOp::MarkPrev => {
                let nav = self.active().mark_prev();
                return self.mark_nav_status(nav);
            }
            // HEX-view editing (byte-granular; the read-only gate above lets these through in hex view).
            EditorOp::HexSetNibble { hi, val } => {
                self.active().hex_set_nibble(hi, val);
                return Ok(Value::str(""));
            }
            EditorOp::HexOverwrite(byte) => {
                self.active().hex_overwrite(byte);
                return Ok(Value::str(""));
            }
            EditorOp::HexInsert(bytes) => {
                self.active().hex_insert(&bytes);
                return Ok(Value::str(""));
            }
            EditorOp::HexDelete { before } => {
                self.active().hex_delete(before);
                return Ok(Value::str(""));
            }
            EditorOp::HexPaste(bytes) => {
                self.active().hex_paste(&bytes);
                return Ok(Value::str(""));
            }
            EditorOp::HexDeleteSelection => {
                self.active().hex_delete_selection();
                return Ok(Value::str(""));
            }
            EditorOp::SetView { flag, kind } => {
                let slot = match flag {
                    ViewFlag::MenuBar => &mut self.menu_bar,
                    ViewFlag::VisibleReturns => &mut self.visible_returns,
                    ViewFlag::VisibleWhitespace => &mut self.visible_whitespace,
                    ViewFlag::LineNumbers => &mut self.line_numbers,
                    ViewFlag::GuiCmline => &mut self.gui_cmline,
                    ViewFlag::ShellWindow => &mut self.shell_output_visible,
                    ViewFlag::Preview => &mut self.preview_visible,
                };
                let prev = *slot;
                *slot = match kind {
                    ViewToggle::On => true,
                    ViewToggle::Off => false,
                    ViewToggle::Toggle => !*slot,
                };
                let (name, on) = match flag {
                    ViewFlag::MenuBar => ("menu bar", *slot),
                    ViewFlag::VisibleReturns => ("visible returns", *slot),
                    ViewFlag::VisibleWhitespace => ("visible whitespace", *slot),
                    ViewFlag::LineNumbers => ("line numbers", *slot),
                    ViewFlag::GuiCmline => ("pop-up command line", *slot),
                    ViewFlag::ShellWindow => ("shell output", *slot),
                    ViewFlag::Preview => ("preview", *slot),
                };
                if flag == ViewFlag::ShellWindow {
                    if prev && !on {
                        // Hiding it: remember whether we were looking at the *Shell Output* tab,
                        // then hand focus to another buffer so the user lands somewhere visible.
                        // (`focus_away_from_shell_output` returns true iff that tab was active.)
                        self.shell_focus_restore = self.focus_away_from_shell_output();
                    } else if !prev && on {
                        // Showing it again: restore focus to the tab ONLY if it had focus when it
                        // was hidden — otherwise just reveal it in the strip, leaving focus put.
                        if self.shell_focus_restore {
                            if let Some(i) =
                                self.docs.iter().position(|d| d.label() == Some(SHELL_OUTPUT_NAME))
                            {
                                self.active = i;
                                self.set_focus(Target::Document);
                            }
                        }
                        self.shell_focus_restore = false;
                    }
                }
                return Ok(Value::str(format!("{name} {}", if on { "on" } else { "off" })));
            }
            EditorOp::SetEditorFontFamily(name) => {
                let name = name.trim();
                if name.is_empty() || name.eq_ignore_ascii_case("default") {
                    self.editor_font_family = None;
                    return Ok(Value::str("editor font family = (default)"));
                }
                self.editor_font_family = Some(name.to_string());
                return Ok(Value::str(format!("editor font family = {name}")));
            }
            EditorOp::SetEditorPropFamily(name) => {
                let name = name.trim();
                if name.is_empty() || name.eq_ignore_ascii_case("default") {
                    self.editor_prop_family = None;
                    return Ok(Value::str("editor proportional font = (none — prose stays monospace)"));
                }
                self.editor_prop_family = Some(name.to_string());
                return Ok(Value::str(format!("editor proportional font = {name}")));
            }
            EditorOp::SetFontMode(mode) => {
                self.docs[self.active].set_font_mode(mode);
                return Ok(Value::str(format!("USEFONT: {} (this document)", mode.as_str())));
            }
            EditorOp::SetUiFontFamily(name) => {
                let name = name.trim();
                if name.is_empty() || name.eq_ignore_ascii_case("default") {
                    self.ui_font_family = None;
                    return Ok(Value::str("UI font family = (default)"));
                }
                self.ui_font_family = Some(name.to_string());
                return Ok(Value::str(format!("UI font family = {name}")));
            }
            EditorOp::SetTxtFontFamily(name) => {
                let name = name.trim();
                if name.is_empty() || name.eq_ignore_ascii_case("default") {
                    self.txt_font_family = None;
                    return Ok(Value::str("TXT font family = (default)"));
                }
                self.txt_font_family = Some(name.to_string());
                return Ok(Value::str(format!("TXT font family = {name}")));
            }
            EditorOp::SetEditorFontStyle(s) => {
                self.editor_font_style = parse_font_style(&s)?;
                return Ok(Value::str(format!("editor font style = {}", self.editor_font_style.keyword())));
            }
            EditorOp::SetUiFontStyle(s) => {
                self.ui_font_style = parse_font_style(&s)?;
                return Ok(Value::str(format!("UI font style = {}", self.ui_font_style.keyword())));
            }
            EditorOp::SetTxtFontStyle(s) => {
                self.txt_font_style = parse_font_style(&s)?;
                return Ok(Value::str(format!("TXT font style = {}", self.txt_font_style.keyword())));
            }
            EditorOp::SetDwFontFamily(name) => {
                let name = name.trim();
                if name.is_empty() || name.eq_ignore_ascii_case("default") {
                    self.dw_font_family = None;
                    return Ok(Value::str("DIR-window font family = (inherit EDFAMILY)"));
                }
                self.dw_font_family = Some(name.to_string());
                return Ok(Value::str(format!("DIR-window font family = {name}")));
            }
            EditorOp::SetDwFontStyle(s) => {
                // `default` (or bare) = UNSET → inherit the editor's style; otherwise force a style.
                // The `default` sentinel is what a saved config emits for the unset state, so it must
                // round-trip back to `None` (not to `Some(Normal)`, which would force-normal and drift).
                let t = s.trim();
                self.dw_font_style = if t.is_empty() || t.eq_ignore_ascii_case("default") {
                    None
                } else {
                    Some(parse_font_style(t)?)
                };
                return Ok(Value::str(match self.dw_font_style {
                    Some(st) => format!("DIR-window font style = {}", st.keyword()),
                    None => "DIR-window font style = (inherits editor)".to_string(),
                }));
            }
            EditorOp::DwReset => {
                self.dw_fg_color = None;
                self.dw_bg_color = None;
                self.dw_font = None;
                self.dw_scale = 1.0;
                self.dw_font_family = None;
                self.dw_font_style = None;
                return Ok(Value::str("DIR-window appearance reset (inherits the editor)"));
            }
            EditorOp::DwFontPicker => {
                self.font_picker_request = Some(FontPickerKind::Dir);
                return Ok(Value::str(""));
            }
            EditorOp::SetPreviewFontFamily(name) => {
                let name = name.trim();
                if name.is_empty() || name.eq_ignore_ascii_case("default") {
                    self.preview_font_family = None;
                    return Ok(Value::str("Preview/TOPDF font = (Typst default)"));
                }
                self.preview_font_family = Some(name.to_string());
                return Ok(Value::str(format!("Preview/TOPDF font = {name}")));
            }
            EditorOp::SetPreviewCodeFamily(name) => {
                let name = name.trim();
                if name.is_empty() || name.eq_ignore_ascii_case("default") {
                    self.preview_code_family = None;
                    return Ok(Value::str("Preview/TOPDF code font = (Typst default)"));
                }
                self.preview_code_family = Some(name.to_string());
                return Ok(Value::str(format!("Preview/TOPDF code font = {name}")));
            }
            EditorOp::PreviewFontPicker => {
                self.font_picker_request = Some(FontPickerKind::PreviewBody);
                return Ok(Value::str(""));
            }
            EditorOp::PreviewCodePicker => {
                self.font_picker_request = Some(FontPickerKind::PreviewCode);
                return Ok(Value::str(""));
            }
            EditorOp::ShowManual => {
                self.show_manual_request = true;
                return Ok(Value::str(""));
            }
            EditorOp::ShowResults { text, emphasis } => {
                // Open the results in a read-only DOCUMENT tab (smooth wrap-aware scrolling). BOTH HELP
                // and MAN reuse ONE tab — a fixed "Help" name, found by the `is_help_listing` marker —
                // replacing its content in place so results never accumulate into extra tabs. The search
                // term is highlighted with EMPH (the same document renderer as GREP's results); `None`
                // clears any stale emphasis. The query context lives in the body's first line.
                const HELP_TAB: &str = "Help";
                self.diff_source = None; // this tab is no longer a diff → DJ is inert
                let (phrase, regex) = match emphasis {
                    Some((p, r)) => (Some(p), r),
                    None => (None, false),
                };
                if let Some(i) = self.docs.iter().position(|d| d.is_help_listing()) {
                    self.docs[i].open_help_listing(HELP_TAB.to_string(), &text);
                    self.docs[i].set_emph(phrase, regex);
                    self.active = i;
                    self.sync_panes();
                } else {
                    let mut doc = Document::new();
                    doc.open_help_listing(HELP_TAB.to_string(), &text);
                    doc.set_emph(phrase, regex);
                    self.add_or_reuse(doc);
                }
                self.set_focus(Target::Document); // land the caret in the results for scrolling
                return Ok(Value::str(""));
            }
            EditorOp::ShowColorNames { filter } => {
                // Build the colour-name listing (text + per-name fg tint + swatch bg) and show it in the
                // shared read-only results tab, exactly like HELP/MAN — then paint the colour spans so
                // each name renders in its own colour with a solid swatch block.
                const HELP_TAB: &str = "Help";
                self.diff_source = None; // this tab is no longer a diff → DJ is inert
                let listing = xyscript_xpl::color_names::listing(filter.as_deref());
                let n = listing.fg.len();
                let idx = if let Some(i) = self.docs.iter().position(|d| d.is_help_listing()) {
                    self.docs[i].open_help_listing(HELP_TAB.to_string(), &listing.text);
                    self.docs[i].set_emph(None, false);
                    self.active = i;
                    self.sync_panes();
                    i
                } else {
                    let mut doc = Document::new();
                    doc.open_help_listing(HELP_TAB.to_string(), &listing.text);
                    self.add_or_reuse(doc);
                    self.active
                };
                self.docs[idx].set_color_spans_raw(listing.fg, listing.bg, Vec::new());
                self.set_focus(Target::Document);
                return Ok(Value::str(format!("{n} colour name(s)")));
            }
            EditorOp::Diff { path: arg } => {
                // Compare the ACTIVE buffer against a file on disk and render a unified diff into the
                // shared read-only results tab, colouring `-`/`+`/`@@` lines (same tab + painting path as
                // COLORNAMES). Bare DIFF uses the buffer's OWN file; `DIFF <path>` uses the given file
                // ($CWD-resolved). The on-disk side is decoded through the buffer's current encoding so a
                // CP437/Latin-1 file compares as the same text the user sees; the comparison is by
                // `lines()` (content), so CRLF-vs-LF / trailing-newline-only changes read as identical.
                const HELP_TAB: &str = "Help";
                let doc = &self.docs[self.active];
                // The active buffer must be a real document, not one of the read-only listings.
                if doc.is_help_listing() || doc.is_grep_listing() || doc.is_dir_listing() || doc.hex_view() {
                    return Ok(Value::str("DIFF: the active tab is not a document buffer"));
                }
                // Resolve which file is the OLD (`-`) side, and its display label.
                let (file, old_label) = match &arg {
                    Some(p) => {
                        let resolved = self.resolve_path(p);
                        (resolved, p.clone())
                    }
                    None => match doc.path() {
                        Some(p) => {
                            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("file").to_string();
                            (p.to_path_buf(), format!("{name} (on disk)"))
                        }
                        None => return Ok(Value::str("DIFF: the active buffer has no file on disk (use DIFF <path>)")),
                    },
                };
                let disk = match std::fs::read(&file) {
                    Ok(b) => b,
                    Err(e) => return Ok(Value::str(format!("DIFF: cannot read {} ({e})", file.display()))),
                };
                // Label for the NEW (`+`) side and the message name.
                let buf_name = doc.path().and_then(|p| p.file_name()).and_then(|s| s.to_str()).unwrap_or("buffer").to_string();
                let new_label = format!("{buf_name} (buffer)");
                let msg_name = arg.clone().unwrap_or_else(|| buf_name.clone());
                let source_id = doc.id().0;
                let old = doc.buffer().encoding().decode(&disk); // on-disk side
                let new = doc.buffer().text(); // current (in-memory) side
                let lines = crate::diff::unified(&old, &new, &old_label, &new_label, 3);
                if lines.is_empty() {
                    self.diff_source = None;
                    return Ok(Value::str(format!("DIFF: buffer matches {msg_name}")));
                }

                // Build the body and the per-line foreground spans (byte offsets into the joined text).
                const RED: (u8, u8, u8) = (220, 70, 70);
                const GREEN: (u8, u8, u8) = (60, 170, 90);
                const CYAN: (u8, u8, u8) = (90, 160, 200);
                let hunks = lines.iter().filter(|l| l.tag == crate::diff::Tag::Hunk).count();
                let changed = lines.iter().filter(|l| matches!(l.tag, crate::diff::Tag::Del | crate::diff::Tag::Ins)).count();
                let mut body = format!("DIFF  {msg_name}  —  {hunks} hunk(s), {changed} line(s) changed  (DJ jumps to a line)\n\n");
                let mut fg: Vec<(usize, usize, (u8, u8, u8))> = Vec::new();
                for l in &lines {
                    let start = body.len();
                    body.push_str(&l.text);
                    let end = body.len();
                    match l.tag {
                        crate::diff::Tag::Del => fg.push((start, end, RED)),
                        crate::diff::Tag::Ins => fg.push((start, end, GREEN)),
                        crate::diff::Tag::Hunk => fg.push((start, end, CYAN)),
                        _ => {}
                    }
                    body.push('\n');
                }

                let idx = if let Some(i) = self.docs.iter().position(|d| d.is_help_listing()) {
                    self.docs[i].open_help_listing(HELP_TAB.to_string(), &body);
                    self.docs[i].set_emph(None, false);
                    self.active = i;
                    self.sync_panes();
                    i
                } else {
                    let mut d = Document::new();
                    d.open_help_listing(HELP_TAB.to_string(), &body);
                    self.add_or_reuse(d);
                    self.active
                };
                self.docs[idx].set_color_spans_raw(fg, Vec::new(), Vec::new());
                self.diff_source = Some(source_id); // DJ jumps back into this buffer
                self.set_focus(Target::Document);
                return Ok(Value::str(format!("DIFF: {hunks} hunk(s), {changed} line(s) — {msg_name}")));
            }
            EditorOp::DiffJump => {
                return self.diff_jump();
            }
            EditorOp::Bundle { from, out } => {
                // Gather the document's name, bytes, sidecar text, and the directory to default the
                // output into — from either the active buffer (live styling) or a file on disk.
                let (name, doc_bytes, meta, src_dir): (String, Vec<u8>, Option<String>, PathBuf) =
                    match &from {
                        Some(src) => {
                            let p = self.resolve_path(src);
                            let bytes = std::fs::read(&p).map_err(|e| {
                                XplError::trappable(format!("BUNDLE: cannot read {} ({e})", p.display()))
                            })?;
                            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("file").to_string();
                            let meta = xymeta_path(&p).and_then(|scp| std::fs::read_to_string(scp).ok());
                            let dir = p.parent().map(|d| d.to_path_buf()).unwrap_or_else(|| self.cwd.clone());
                            (name, bytes, meta, dir)
                        }
                        None => {
                            let doc = &self.docs[self.active];
                            if doc.is_help_listing() || doc.is_grep_listing() || doc.is_dir_listing() || doc.hex_view() {
                                return Ok(Value::str("BUNDLE: the active tab is not a document buffer"));
                            }
                            let bytes = doc.save_bytes();
                            let name = doc.file_name().unwrap_or_else(|| "Untitled".to_string());
                            let (fg, bg) = doc.color_spans_raw();
                            let meta = xymeta_serialize(&bytes, fg, bg, doc.attr_spans_raw(), &doc.marks_for_persist());
                            let dir = doc.path().and_then(|p| p.parent()).map(|d| d.to_path_buf()).unwrap_or_else(|| self.cwd.clone());
                            (name, bytes, meta, dir)
                        }
                    };
                let packed = crate::bundle::pack(&name, &doc_bytes, meta.as_deref().map(str::as_bytes).unwrap_or(&[]));
                let out_path = match &out {
                    Some(o) => self.resolve_path(o),
                    None => src_dir.join(format!("{name}.xyb")),
                };
                std::fs::write(&out_path, &packed).map_err(|e| {
                    XplError::trappable(format!("BUNDLE: cannot write {} ({e})", out_path.display()))
                })?;
                let note = if meta.is_some() { "doc + styling" } else { "doc, no styling to carry" };
                return Ok(Value::str(format!("BUNDLE: wrote {} ({note})", out_path.display())));
            }
            EditorOp::Unbundle { path, dest } => {
                let bundle_path = self.resolve_path(&path);
                let bytes = std::fs::read(&bundle_path).map_err(|e| {
                    XplError::trappable(format!("UNBUNDLE: cannot read {} ({e})", bundle_path.display()))
                })?;
                let b = crate::bundle::unpack(&bytes).map_err(|e| {
                    XplError::trappable(format!("UNBUNDLE: {} is not a valid bundle ({e})", bundle_path.display()))
                })?;
                let dir = match &dest {
                    Some(d) => self.resolve_path(d),
                    None => self.cwd.clone(),
                };
                let doc_path = dir.join(&b.name);
                if doc_path.exists() {
                    return Ok(Value::str(format!(
                        "UNBUNDLE: {} already exists (choose another destination)",
                        doc_path.display()
                    )));
                }
                std::fs::write(&doc_path, &b.doc).map_err(|e| {
                    XplError::trappable(format!("UNBUNDLE: cannot write {} ({e})", doc_path.display()))
                })?;
                if !b.meta.is_empty() {
                    if let Ok(text) = std::str::from_utf8(&b.meta) {
                        xymeta_write_text(&doc_path, text);
                    }
                }
                // Open it the normal way — the sidecar we just wrote restores the styling via the guard.
                let opened = self.open_doc(&doc_path.to_string_lossy(), false, false)?;
                return Ok(Value::str(format!("UNBUNDLE: {} — {}", doc_path.display(), opened.to_str())));
            }
            EditorOp::OpenDocPdf { manual } => {
                // `REFPDF`/`MANPDF` — open the shipped reference/manual PDF in the OS default app. Look
                // in `$XSDIR/docs` first (the repo layout), then `$XSDIR` (the deployed-alongside-the-exe
                // layout). `$XSDIR` is the running executable's directory.
                let name = if manual { "manual.pdf" } else { "reference.pdf" };
                let dir = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                    .ok_or_else(|| XplError::trappable("cannot locate the program directory ($XSDIR)"))?;
                let candidates = [dir.join("docs").join(name), dir.join(name)];
                let Some(path) = candidates.iter().find(|p| p.exists()) else {
                    return Err(XplError::trappable(format!(
                        "{name} not found in {}\\docs or {}",
                        dir.display(),
                        dir.display()
                    )));
                };
                match open_in_os(path) {
                    Ok(()) => return Ok(Value::str(format!("opened {name}"))),
                    Err(e) => return Err(XplError::trappable(format!("could not open {name}: {e}"))),
                }
            }
            EditorOp::SetUiSize { kind, value, relative } => {
                // `relative` (the verb's arg began with an explicit +/-): add `value` to the knob's
                // current value; otherwise `value` replaces it. Each knob then clamps to its range
                // (margins 0..200 cols; fonts 6..200 pt; scales 0.5..4; leading 50..400 %).
                let adj = |cur: f32| if relative { cur + value } else { value };
                let (name, shown) = match kind {
                    UiSizeKind::Scale => {
                        self.ui_scale = adj(self.ui_scale).clamp(0.5, 4.0);
                        ("UI scale", self.ui_scale)
                    }
                    UiSizeKind::ChromeFont => {
                        self.chrome_font = adj(self.chrome_font).clamp(6.0, 200.0);
                        ("chrome font", self.chrome_font)
                    }
                    UiSizeKind::EditorFont => {
                        self.editor_font = adj(self.editor_font).clamp(6.0, 200.0);
                        ("editor font", self.editor_font)
                    }
                    UiSizeKind::Leading => {
                        self.leading = adj(self.leading).clamp(50.0, 400.0);
                        ("line spacing", self.leading)
                    }
                    UiSizeKind::MarginBoth => {
                        self.margin_left = adj(self.margin_left).clamp(0.0, 200.0);
                        self.margin_right = adj(self.margin_right).clamp(0.0, 200.0);
                        ("margins", self.margin_left)
                    }
                    UiSizeKind::MarginLeft => {
                        self.margin_left = adj(self.margin_left).clamp(0.0, 200.0);
                        ("left margin", self.margin_left)
                    }
                    UiSizeKind::MarginRight => {
                        self.margin_right = adj(self.margin_right).clamp(0.0, 200.0);
                        ("right margin", self.margin_right)
                    }
                    UiSizeKind::TxtFont => {
                        self.txt_font = adj(self.txt_font).clamp(6.0, 200.0);
                        ("TXT font", self.txt_font)
                    }
                    UiSizeKind::TxtScale => {
                        self.txt_scale = adj(self.txt_scale).clamp(0.5, 4.0);
                        ("TXT scale", self.txt_scale)
                    }
                    UiSizeKind::DwFont => {
                        // Override field is `Option` (None = inherit EDFONT); a relative change
                        // works off the current effective value.
                        let cur = self.dw_font.unwrap_or(self.editor_font);
                        let v = if relative { cur + value } else { value }.clamp(6.0, 200.0);
                        self.dw_font = Some(v);
                        ("DIR-window font", v)
                    }
                    UiSizeKind::DwScale => {
                        self.dw_scale = adj(self.dw_scale).clamp(0.5, 4.0);
                        ("DIR-window scale", self.dw_scale)
                    }
                    UiSizeKind::PreviewFont => {
                        // Override field is `Option` (None = Typst default). `PVSIZE` sends 0.0 for
                        // `default`/empty → clear; a relative change works off the effective 11pt base.
                        let base = self.preview_font_size.unwrap_or(11.0);
                        let v = if relative { base + value } else { value };
                        if v <= 0.0 {
                            self.preview_font_size = None;
                            return Ok(Value::str("Preview/TOPDF font size = (Typst default)"));
                        }
                        let v = v.clamp(4.0, 200.0);
                        self.preview_font_size = Some(v);
                        ("Preview/TOPDF font size", v)
                    }
                    UiSizeKind::PreviewCodeFont => {
                        // Like PreviewFont: `Option` (None = inherit the body size). `PVCODESIZE` sends
                        // 0.0 for `default`/empty → clear; a relative change works off the effective
                        // code size (the pinned value, else the body size, else the 11pt base).
                        let base =
                            self.preview_code_size.or(self.preview_font_size).unwrap_or(11.0);
                        let v = if relative { base + value } else { value };
                        if v <= 0.0 {
                            self.preview_code_size = None;
                            return Ok(Value::str("Preview/TOPDF code size = (inherit body size)"));
                        }
                        let v = v.clamp(4.0, 200.0);
                        self.preview_code_size = Some(v);
                        ("Preview/TOPDF code size", v)
                    }
                };
                return Ok(Value::str(format!("{name} = {shown}")));
            }
            EditorOp::UiReset => {
                self.ui_scale = DEF_UI_SCALE;
                self.chrome_font = DEF_CHROME_FONT;
                self.editor_font = DEF_EDITOR_FONT;
                self.leading = DEF_LEADING;
                self.margin_left = DEF_MARGIN;
                self.margin_right = DEF_MARGIN;
                self.txt_font = DEF_TXT_FONT;
                self.txt_scale = DEF_TXT_SCALE;
                return Ok(Value::str("UI sizing reset to defaults"));
            }
            EditorOp::FgColor(opt) => {
                // Always target the ACTIVE DOCUMENT (what the canvas renders), NOT `active()` —
                // colouring is a document op, and `FGCOLORS`' picker runs its `FGCOLOR` after focus
                // has returned to the command line, so `active()` would be the cmline and miss the
                // selection. On a selection: colour just the selected text (a transient span that
                // follows edits). `FGCOLOR default` (`opt == None`) is selection-ONLY — it clears
                // the selection's colour and is a no-op (with a note) when nothing is selected. A
                // concrete colour with no selection still sets the GLOBAL foreground (back-compat).
                let sel = self.docs[self.active].selection_range();
                let msg = match (sel, opt) {
                    (Some((s, e)), _) => {
                        self.docs[self.active].set_color_span(s, e, opt);
                        match opt {
                            Some((r, g, b)) => format!("selection colour: {r} {g} {b}"),
                            None => "selection colour cleared".to_string(),
                        }
                    }
                    (None, Some(rgb)) => {
                        self.fg_color = rgb;
                        let (r, g, b) = rgb;
                        format!("foreground: {r} {g} {b}")
                    }
                    (None, None) => "FGCOLOR default: select text first (nothing to clear)".to_string(),
                };
                return Ok(Value::str(msg));
            }
            EditorOp::ModeColor { background, rgb } => {
                // XyWrite-style block colour on the SELECTION — foreground or background layer.
                let sel = self.docs[self.active].selection_range();
                let msg = match sel {
                    Some((s, e)) => {
                        self.docs[self.active].style_selection_color(s, e, background, rgb);
                        let layer = if background { "background" } else { "foreground" };
                        match rgb {
                            Some((r, g, b)) => format!("selection {layer}: {r} {g} {b}"),
                            None => format!("selection {layer} cleared"),
                        }
                    }
                    // No selection → TYPING MODE (XyWrite's modal "MD"): a colour turns the mode on so
                    // subsequently typed text is styled; a bare `MDA`/`MDA/B` (rgb None) turns it off —
                    // `MDA` clears the fg colour AND attribute modes, `MDA/B` clears the bg mode.
                    None => match (rgb, background) {
                        (Some((r, g, b)), true) => {
                            self.docs[self.active].set_typing_bg(Some((r, g, b)));
                            format!("MDA/B typing on: {r} {g} {b}")
                        }
                        (Some((r, g, b)), false) => {
                            self.docs[self.active].set_typing_fg(Some((r, g, b)));
                            format!("MDA typing on: {r} {g} {b}")
                        }
                        (None, true) => {
                            self.docs[self.active].set_typing_bg(None);
                            "MDA/B typing off".to_string()
                        }
                        (None, false) => {
                            self.docs[self.active].clear_typing_fg_attr();
                            "MDA typing off".to_string()
                        }
                    },
                };
                return Ok(Value::str(msg));
            }
            EditorOp::ModeAttr { attrs } => {
                // XyWrite-style block ATTRIBUTES (bold/italic/underline) on the SELECTION.
                let sel = self.docs[self.active].selection_range();
                let msg = match sel {
                    Some((s, e)) => {
                        self.docs[self.active].style_selection_attr(s, e, attrs);
                        match attrs {
                            Some(a) => format!("selection: {}", mda_attr_names(a)),
                            None => "selection attributes cleared".to_string(),
                        }
                    }
                    // No selection → attribute TYPING MODE: a keyword turns it on, `MDA NM` turns it off.
                    None => match attrs {
                        Some(a) => {
                            self.docs[self.active].set_typing_attr(Some(a));
                            format!("MDA typing on: {}", mda_attr_names(a))
                        }
                        None => {
                            self.docs[self.active].set_typing_attr(None);
                            "MDA attribute typing off".to_string()
                        }
                    },
                };
                return Ok(Value::str(msg));
            }
            EditorOp::ModeAllColor { background, rgb, find } => {
                // Bulk MDA colour: stamp the fg (or bg) colour onto EVERY match, as persistent spans.
                let spans = self.find_all_query(&find)?;
                if spans.is_empty() {
                    return Err(XplError::trappable(format!("MDALL: no matches for {:?}", find.pattern)));
                }
                let n = spans.len();
                // Bare `MDALL off` mirrors bare `MDA`: clearing the foreground clears attributes too.
                let also_clear_attr = !background && rgb.is_none();
                self.docs[self.active].style_all_color(&spans, background, rgb, also_clear_attr);
                let layer = if background { "background" } else { "foreground" };
                let capped = if n >= Self::MDALL_CAP { " (capped)" } else { "" };
                return Ok(Value::str(match rgb {
                    Some((r, g, b)) => format!("MDALL {layer}: {r} {g} {b} on {n} match(es){capped}"),
                    None => format!("MDALL cleared {layer} on {n} match(es){capped}"),
                }));
            }
            EditorOp::ModeAllAttr { attrs, find } => {
                // Bulk MDA attributes: apply (or, with `NM`, clear) attributes on EVERY match.
                let spans = self.find_all_query(&find)?;
                if spans.is_empty() {
                    return Err(XplError::trappable(format!("MDALL: no matches for {:?}", find.pattern)));
                }
                let n = spans.len();
                self.docs[self.active].style_all_attr(&spans, attrs);
                let capped = if n >= Self::MDALL_CAP { " (capped)" } else { "" };
                return Ok(Value::str(match attrs {
                    Some(a) => format!("MDALL {}: {n} match(es){capped}", mda_attr_names(a)),
                    None => format!("MDALL cleared attributes on {n} match(es){capped}"),
                }));
            }
            EditorOp::SetColor { ground, rgb } => {
                let (r, g, b) = rgb;
                let name = match ground {
                    ColorGround::Fg => {
                        self.fg_color = rgb;
                        "foreground"
                    }
                    ColorGround::Bg => {
                        self.bg_color = rgb;
                        "background"
                    }
                    ColorGround::UiFg => {
                        self.ui_fg_color = rgb;
                        "UI foreground"
                    }
                    ColorGround::UiBg => {
                        self.ui_bg_color = rgb;
                        "UI background"
                    }
                    ColorGround::Caret => {
                        self.caret_color = rgb;
                        "caret"
                    }
                    ColorGround::CmlineCaret => {
                        self.cmline_caret_color = rgb;
                        "command-line caret"
                    }
                    ColorGround::Separator => {
                        self.sep_color = rgb;
                        "separator"
                    }
                    // `TABXCOLOR` sets via the dedicated `SetTabXColor` op (it's optional); this arm
                    // exists only to keep the match exhaustive, but handle it coherently anyway.
                    ColorGround::TabClose => {
                        self.tab_x_color = Some(rgb);
                        "tab ×"
                    }
                    ColorGround::TxtFg => {
                        self.txt_fg_color = rgb;
                        "TXT foreground"
                    }
                    ColorGround::TxtBg => {
                        self.txt_bg_color = rgb;
                        "TXT background"
                    }
                    ColorGround::DwFg => {
                        self.dw_fg_color = Some(rgb);
                        "DIR-window foreground"
                    }
                    ColorGround::DwBg => {
                        self.dw_bg_color = Some(rgb);
                        "DIR-window background"
                    }
                    ColorGround::SelBg => {
                        self.sel_bg_color = rgb;
                        "selection background"
                    }
                    ColorGround::SelFg => {
                        self.sel_fg_color = rgb;
                        "selected-text"
                    }
                    ColorGround::FindBg => {
                        self.find_bg_color = rgb;
                        "FIND highlight background"
                    }
                    ColorGround::FindFg => {
                        self.find_fg_color = rgb;
                        "FIND-match text"
                    }
                    ColorGround::SpellUnderline => {
                        self.spell_color = rgb;
                        "spelling underline"
                    }
                    ColorGround::GrammarUnderline => {
                        self.grammar_color = rgb;
                        "grammar underline"
                    }
                    ColorGround::PaneBorder => {
                        self.pane_border_color = rgb;
                        "active-pane border"
                    }
                    // `BALPAIRSCOLOR` sets via the dedicated `SetBalColor` op (it's optional); this arm
                    // keeps the match exhaustive and stores a concrete pick coherently (picker → OK).
                    ColorGround::BalPairs => {
                        self.bal_color = Some(rgb);
                        "bracket highlight"
                    }
                    // `EMPHFGCOLOR`/`EMPHBGCOLOR` set via dedicated ops (they're optional); these arms
                    // keep the match exhaustive and store a concrete pick coherently (picker → OK).
                    ColorGround::EmphFg => {
                        self.emph_fg = Some(rgb);
                        "emphasis text"
                    }
                    ColorGround::EmphBg => {
                        self.emph_bg = Some(rgb);
                        "emphasis background"
                    }
                    // MDA block colours are applied to the SELECTION through the `MDA` verb (an
                    // `EditorOp::ModeColor`), never as a global `SetColor` — but the enum is shared,
                    // so these arms exist only to keep the match exhaustive. Reaching them would be a
                    // routing bug; do nothing to global state.
                    ColorGround::MdaFg => "block text",
                    ColorGround::MdaBg => "block background",
                };
                return Ok(Value::str(format!("{name} colour = {r} {g} {b}")));
            }
            EditorOp::SetHighlightColor { kind, color, bg } => {
                // Any kind name is accepted (built-in or grammar-defined → `Other`).
                let k = crate::highlight::TokenKind::from_name(&kind);
                let label = k.name().to_string();
                let map = if bg { &mut self.highlight_bg } else { &mut self.highlight_theme };
                let what = if bg { "background" } else { "foreground" };
                match color {
                    Some(rgb) => {
                        map.insert(k, rgb);
                        let (r, g, b) = rgb;
                        return Ok(Value::str(format!("highlight {label} {what} = {r} {g} {b}")));
                    }
                    None => {
                        map.remove(&k);
                        let cleared =
                            if bg { "(no background)" } else { "(default text colour)" };
                        return Ok(Value::str(format!("highlight {label} {what} = {cleared}")));
                    }
                }
            }
            EditorOp::SetHighlightMode { kind, bits } => {
                let k = crate::highlight::TokenKind::from_name(&kind);
                let label = k.name().to_string();
                match bits {
                    Some(b) => {
                        self.highlight_mode.insert(k, b);
                        return Ok(Value::str(format!("highlight {label} mode = {}", mda_attr_names(b))));
                    }
                    None => {
                        self.highlight_mode.remove(&k);
                        return Ok(Value::str(format!("highlight {label} mode = (none)")));
                    }
                }
            }
            EditorOp::ResetHighlightTheme => {
                self.highlight_theme = crate::highlight::default_highlight_theme();
                self.highlight_bg.clear(); // backgrounds are opt-in — reset removes them all
                self.highlight_mode.clear(); // modes are opt-in too — reset clears them
                return Ok(Value::str("highlight theme reset"));
            }
            EditorOp::DefineHighlightLang { name, exts } => {
                self.highlight_grammars.borrow_mut().define(&name, exts);
                return Ok(Value::str(format!("highlight grammar: {name}")));
            }
            EditorOp::AddHighlightRule { kind, regex, perl } => {
                // A bad rule is FATAL (not a swallowed trappable): when a grammar is loaded with `RUN`,
                // a fatal unwinds and is stamped `<file>:<line>` on the status line, so the author sees
                // exactly which rule failed and why — instead of the rule being silently dropped.
                self.highlight_grammars
                    .borrow_mut()
                    .add_rule(&kind, &regex, perl)
                    .map_err(XplError::fatal)?;
                return Ok(Value::str(format!("highlight rule: {kind}")));
            }
            EditorOp::SetHighlightAuto(on) => {
                self.hl_auto = on;
                if on {
                    // Apply immediately to the active document so the effect is visible at once.
                    self.auto_highlight_active();
                }
                return Ok(Value::str(if on { "highlight auto on" } else { "highlight auto off" }));
            }
            EditorOp::ApplyTheme(name) => {
                let applied = self.apply_theme(&name).map_err(XplError::trappable)?;
                return Ok(Value::str(format!("theme: {applied}")));
            }
            EditorOp::SetTabBgColor { state, color } => {
                let slot = match state.trim().to_ascii_lowercase().as_str() {
                    "active" | "selected" => &mut self.tab_active_bg,
                    "hover" | "hovered" => &mut self.tab_hover_bg,
                    "inactive" | "normal" | "idle" => &mut self.tab_inactive_bg,
                    _ => {
                        return Err(XplError::trappable(format!(
                            "TABBGCOLOR: unknown state {:?} (active|hover|inactive)",
                            state.trim()
                        )))
                    }
                };
                *slot = color;
                let st = state.trim().to_ascii_lowercase();
                return Ok(Value::str(match color {
                    Some((r, g, b)) => format!("tab {st} background = {r} {g} {b}"),
                    None => format!("tab {st} background = (theme default)"),
                }));
            }
            EditorOp::SetTabXColor(color) => {
                self.tab_x_color = color;
                return Ok(Value::str(match color {
                    Some((r, g, b)) => format!("tab × colour = {r} {g} {b}"),
                    None => "tab × colour = (UI foreground)".to_string(),
                }));
            }
            EditorOp::SetPathColor { fg, color } => {
                if fg {
                    self.path_fg_color = color;
                } else {
                    self.path_bg_color = color;
                }
                let what = if fg { "text" } else { "background" };
                return Ok(Value::str(match color {
                    Some((r, g, b)) => format!("$CWD path {what} colour = {r} {g} {b}"),
                    None => format!("$CWD path {what} colour = (default)"),
                }));
            }
            EditorOp::OpenPathColorPicker { fg } => {
                self.color_picker_request =
                    Some(if fg { ColorTarget::PathFg } else { ColorTarget::PathBg });
                return Ok(Value::str(""));
            }
            EditorOp::SetPreviewColor { fg, color } => {
                if fg {
                    self.preview_fg_color = color;
                } else {
                    self.preview_bg_color = color;
                }
                let what = if fg { "text" } else { "background" };
                return Ok(Value::str(match color {
                    Some((r, g, b)) => format!("preview {what} colour = {r} {g} {b}"),
                    None => format!("preview {what} colour = (default)"),
                }));
            }
            EditorOp::OpenPreviewColorPicker { fg } => {
                self.color_picker_request =
                    Some(if fg { ColorTarget::PreviewFg } else { ColorTarget::PreviewBg });
                return Ok(Value::str(""));
            }
            EditorOp::SetScratchColor { fg, color } => {
                if fg {
                    self.scratch_fg_color = color;
                } else {
                    self.scratch_bg_color = color;
                }
                let what = if fg { "text" } else { "background" };
                return Ok(Value::str(match color {
                    Some((r, g, b)) => format!("scratchpad {what} colour = {r} {g} {b}"),
                    None => format!("scratchpad {what} colour = (default)"),
                }));
            }
            EditorOp::SetDwColor { fg, color } => {
                if fg {
                    self.dw_fg_color = color;
                } else {
                    self.dw_bg_color = color;
                }
                let what = if fg { "text" } else { "background" };
                return Ok(Value::str(match color {
                    Some((r, g, b)) => format!("DIR-window {what} colour = {r} {g} {b}"),
                    None => format!("DIR-window {what} colour = (inherits editor)"),
                }));
            }
            EditorOp::OpenScratchColorPicker { fg } => {
                self.color_picker_request =
                    Some(if fg { ColorTarget::ScratchFg } else { ColorTarget::ScratchBg });
                return Ok(Value::str(""));
            }
            EditorOp::SetStatusLineTop(v) => {
                self.status_line_top = v;
            }
            EditorOp::SetCmlinePlacement(p) => {
                self.cmline_placement = p;
                return Ok(Value::str(format!("command line: {}", p.name())));
            }
            EditorOp::SetCmlineBlank(opt) => {
                self.cmline_blank = opt.unwrap_or(!self.cmline_blank);
                return Ok(Value::str(format!(
                    "CMBLANK {}",
                    if self.cmline_blank { "on" } else { "off" }
                )));
            }
            EditorOp::SetShowOneTab(opt) => {
                self.show_one_tab = opt.unwrap_or(!self.show_one_tab);
                return Ok(Value::str(format!(
                    "SHOWONETAB {}",
                    if self.show_one_tab { "on" } else { "off" }
                )));
            }
            EditorOp::SetCaretMotion(visual) => {
                self.caret_visual = visual;
                return Ok(Value::str(format!(
                    "CARETMOTION {}",
                    if self.caret_visual { "visual" } else { "logical" }
                )));
            }
            EditorOp::SetWrap(opt) => {
                self.wrap = opt.unwrap_or(!self.wrap);
                return Ok(Value::str(format!("WRAP {}", if self.wrap { "on" } else { "off" })));
            }
            EditorOp::SetWrapHang(n) => {
                self.wrap_hang = n;
                return Ok(Value::str(format!("wrap hang: {}", self.wrap_hang)));
            }
            EditorOp::SetReflowWidth(n) => {
                self.reflow_width = n.max(1);
                return Ok(Value::str(format!("reflow width: {}", self.reflow_width)));
            }
            EditorOp::SetUntitledTab(opt) => {
                self.untitled_auto_close = opt.unwrap_or(!self.untitled_auto_close);
                // Turning it on should immediately tidy up any scratch tab already lingering.
                self.prune_untitled_tabs();
                return Ok(Value::str(format!(
                    "UNTITLEDTAB {}",
                    if self.untitled_auto_close { "on" } else { "off" }
                )));
            }
            EditorOp::SetGotoOpen(opt) => {
                self.goto_open = opt.unwrap_or(!self.goto_open);
                return Ok(Value::str(format!(
                    "GOTOOPEN {}",
                    if self.goto_open { "on" } else { "off" }
                )));
            }
            EditorOp::SetAutoReopen(opt) => {
                self.auto_reopen = opt.unwrap_or(!self.auto_reopen);
                if !self.auto_reopen {
                    self.disk_mtimes.clear(); // drop baselines so a later re-enable re-baselines cleanly
                }
                return Ok(Value::str(format!(
                    "AUTOREOPEN {}",
                    if self.auto_reopen { "on" } else { "off" }
                )));
            }
            EditorOp::SetErrorLog(opt) => {
                self.error_log = opt.unwrap_or(!self.error_log);
                return Ok(Value::str(format!(
                    "ERRORLOG {} ({})",
                    if self.error_log { "on" } else { "off" },
                    self.error_log_path().display()
                )));
            }
            EditorOp::SetErrorLogFile(path) => {
                let p = path.trim();
                self.error_log_file = if p.is_empty() { None } else { Some(p.to_string()) };
                return Ok(Value::str(format!(
                    "ERRORLOGFILE {}",
                    self.error_log_path().display()
                )));
            }
            EditorOp::SetCloseBack(opt) => {
                self.close_back = opt.unwrap_or(!self.close_back);
            }
            EditorOp::SetDimOnBlur(opt) => {
                self.dim_on_blur = opt.unwrap_or(!self.dim_on_blur);
                return Ok(Value::str(format!(
                    "DIMONBLUR {}",
                    if self.dim_on_blur { "on" } else { "off" }
                )));
            }
            EditorOp::SetMarkGutter(opt) => {
                // Bare (None) toggles off↔on; from `persist`, a bare toggle hides it (→ off).
                self.mark_gutter = opt.unwrap_or(match self.mark_gutter {
                    MarkGutterMode::Off => MarkGutterMode::On,
                    MarkGutterMode::On | MarkGutterMode::Persist => MarkGutterMode::Off,
                });
                return Ok(Value::str(format!(
                    "MARKGUTTER {}",
                    mark_gutter_word(self.mark_gutter)
                )));
            }
            EditorOp::SetMarkColor { fg, color } => {
                if fg {
                    self.mark_fg_color = color;
                } else {
                    self.mark_bg_color = color;
                }
                let what = if fg { "dot" } else { "background" };
                return Ok(Value::str(match color {
                    Some((r, g, b)) => format!("bookmark gutter {what} colour = {r} {g} {b}"),
                    None => format!("bookmark gutter {what} colour = (default)"),
                }));
            }
            EditorOp::OpenMarkColorPicker { fg } => {
                self.color_picker_request =
                    Some(if fg { ColorTarget::MarkFg } else { ColorTarget::MarkBg });
                return Ok(Value::str(""));
            }
            EditorOp::SetLineNumColor { fg, color } => {
                if fg {
                    self.line_num_fg_color = color;
                } else {
                    self.line_num_bg_color = color;
                }
                let what = if fg { "text" } else { "background" };
                return Ok(Value::str(match color {
                    Some((r, g, b)) => format!("line-number gutter {what} colour = {r} {g} {b}"),
                    None => format!("line-number gutter {what} colour = (default)"),
                }));
            }
            EditorOp::OpenLineNumColorPicker { fg } => {
                self.color_picker_request =
                    Some(if fg { ColorTarget::LineNumFg } else { ColorTarget::LineNumBg });
                return Ok(Value::str(""));
            }
            EditorOp::SetMsgColor { slot, color } => {
                let (field, what) = match slot {
                    MsgColorSlot::HeaderFg => (&mut self.msg_header_fg_color, "header text"),
                    MsgColorSlot::Bg => (&mut self.msg_bg_color, "background"),
                    MsgColorSlot::OkFg => (&mut self.msg_ok_fg_color, "OK button text"),
                    MsgColorSlot::OkBg => (&mut self.msg_ok_bg_color, "OK button background"),
                };
                *field = color;
                return Ok(Value::str(match color {
                    Some((r, g, b)) => format!("MSG box {what} colour = {r} {g} {b}"),
                    None => format!("MSG box {what} colour = (default)"),
                }));
            }
            EditorOp::OpenMsgColorPicker { slot } => {
                self.color_picker_request = Some(match slot {
                    MsgColorSlot::HeaderFg => ColorTarget::MsgHeaderFg,
                    MsgColorSlot::Bg => ColorTarget::MsgBg,
                    MsgColorSlot::OkFg => ColorTarget::MsgOkFg,
                    MsgColorSlot::OkBg => ColorTarget::MsgOkBg,
                });
                return Ok(Value::str(""));
            }
            EditorOp::SetUrlHighlight(opt) => {
                self.url_highlight = opt.unwrap_or(!self.url_highlight);
                return Ok(Value::str(format!(
                    "URLHIGHLIGHT {}",
                    if self.url_highlight { "on" } else { "off" }
                )));
            }
            EditorOp::SetPreviewMarks(opt) => {
                self.preview_marks = opt.unwrap_or(!self.preview_marks);
                return Ok(Value::str(format!(
                    "PVMARKS {}",
                    if self.preview_marks { "on" } else { "off" }
                )));
            }
            EditorOp::SetUrlColor { fg, color } => {
                if fg {
                    self.url_fg_color = color;
                } else {
                    self.url_bg_color = color;
                }
                let what = if fg { "text" } else { "background" };
                return Ok(Value::str(match color {
                    Some((r, g, b)) => format!("URL highlight {what} colour = {r} {g} {b}"),
                    None => format!("URL highlight {what} colour = (default)"),
                }));
            }
            EditorOp::OpenUrlColorPicker { fg } => {
                self.color_picker_request =
                    Some(if fg { ColorTarget::UrlFg } else { ColorTarget::UrlBg });
                return Ok(Value::str(""));
            }
            EditorOp::SetUlSpaces(opt) => {
                self.ul_spaces = opt.unwrap_or(!self.ul_spaces);
                return Ok(Value::str(format!(
                    "ULSPACES {}",
                    if self.ul_spaces { "on" } else { "off" }
                )));
            }
            #[cfg(feature = "email")]
            EditorOp::MailSet { field, value } => {
                let (label, shown) = match field.as_str() {
                    "url" => {
                        self.smtp_url = value.clone();
                        ("SMTP URL", value)
                    }
                    "user" => {
                        self.mail_user = value.clone();
                        ("mail user", value)
                    }
                    "from" => {
                        self.mail_from = value.clone();
                        ("mail from", value)
                    }
                    "debug" => {
                        self.mail_debug = match value.as_str() {
                            "on" => true,
                            "off" => false,
                            _ => !self.mail_debug,
                        };
                        ("mail debug", if self.mail_debug { "on" } else { "off" }.to_string())
                    }
                    "imapurl" => {
                        self.imap_url = value.clone();
                        ("IMAP URL", value)
                    }
                    "mailbox" => {
                        self.mailbox = value.clone();
                        ("mailbox", value)
                    }
                    _ => {
                        self.mail_pass = value;
                        ("mail password", "(set)".to_string())
                    }
                };
                return Ok(Value::str(format!("{label}: {shown}")));
            }
            #[cfg(feature = "email")]
            EditorOp::SendMail { to, subject, body_file, wait } => {
                let before = if wait { self.mail_job_ids() } else { Default::default() };
                let r = self.send_mail(to, subject, body_file);
                return self.finish_mail(r, wait, before, "SENDMAIL", "sent");
            }
            #[cfg(feature = "email")]
            EditorOp::MailAttach(path) => {
                let raw = path.trim();
                // Store an absolute path (resolved against $CWD) for real files so a later `CD` can't
                // break it; the virtual fs (tests) keys on the literal path, like the other file ops.
                let (stored, exists) = if self.virtual_fs {
                    (std::path::PathBuf::from(raw), self.files.contains_key(raw))
                } else {
                    let r = self.resolve_path(raw);
                    let ok = r.is_file();
                    (r, ok)
                };
                if !exists {
                    return Err(XplError::trappable_code(ErrCode::FileNotFound, format!("ATTACH: file not found: {raw}")));
                }
                let name = stored
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_else(|| raw.to_string());
                self.mail_attachments.push(stored);
                return Ok(Value::str(format!(
                    "attached: {name} ({} staged)",
                    self.mail_attachments.len()
                )));
            }
            #[cfg(feature = "email")]
            EditorOp::MailClearAttach => {
                let n = self.mail_attachments.len();
                self.mail_attachments.clear();
                return Ok(Value::str(format!("cleared {n} staged attachment(s)")));
            }
            #[cfg(feature = "email")]
            EditorOp::ListMail { wait } => {
                let before = if wait { self.mail_job_ids() } else { Default::default() };
                let r = self.list_mail();
                return self.finish_mail(r, wait, before, "LISTMAIL", "ok");
            }
            #[cfg(feature = "email")]
            EditorOp::SaveMail { range, path, wait } => {
                let before = if wait { self.mail_job_ids() } else { Default::default() };
                let r = self.save_mail(&range, path.as_deref());
                let label = format!("SAVEMAIL {range}");
                return self.finish_mail(r, wait, before, &label, "ok");
            }
            #[cfg(feature = "email")]
            EditorOp::DelMail { range, wait } => {
                let before = if wait { self.mail_job_ids() } else { Default::default() };
                let r = self.del_mail(&range);
                let label = format!("DELMAIL {range}");
                return self.finish_mail(r, wait, before, &label, "deleted");
            }
            EditorOp::SetPathPlace(p) => {
                self.path_place = p;
                return Ok(Value::str(format!("path shown on: {}", p.name())));
            }
            EditorOp::SetPathLen(n) => {
                self.path_len = n.max(1); // never zero — a length of 0 would hide the path entirely
                return Ok(Value::str(format!("path length: {}", self.path_len)));
            }
            EditorOp::SetTextFileExts(list) => {
                let report = |exts: &[String]| {
                    if exts.is_empty() {
                        "text-file extensions: (built-in set only)".to_string()
                    } else {
                        let joined = exts.iter().map(|e| format!(".{e}")).collect::<Vec<_>>().join(",");
                        format!("text-file extensions (added to the built-in set): {joined}")
                    }
                };
                match list {
                    Some(exts) => {
                        self.text_file_exts = exts;
                        return Ok(Value::str(report(&self.text_file_exts)));
                    }
                    None => return Ok(Value::str(report(&self.text_file_exts))), // bare TEXTFILES = report
                }
            }
            EditorOp::SetCaretShape(shape) => {
                self.caret_shape = shape;
                return Ok(Value::str(format!("caret shape: {}", shape.name())));
            }
            EditorOp::SetLintStyle { grammar, style } => {
                if grammar {
                    self.grammar_style = style;
                } else {
                    self.spell_style = style;
                }
                let which = if grammar { "grammar" } else { "spelling" };
                return Ok(Value::str(format!("{which} underline: {}", style.name())));
            }
            EditorOp::SetPvMargin { side, value } => {
                // Validate + normalize (bare number = inches; in/cm/mm/pt), then apply to the side(s).
                let norm = crate::pdf::normalize_length(&value)
                    .map_err(|e| XplError::trappable(format!("PVMARGIN: {e}")))?;
                let m = &mut self.preview_margins;
                match side {
                    PvMarginSide::All => {
                        m.left = norm.clone();
                        m.right = norm.clone();
                        m.top = norm.clone();
                        m.bottom = norm.clone();
                    }
                    PvMarginSide::Left => m.left = norm.clone(),
                    PvMarginSide::Right => m.right = norm.clone(),
                    PvMarginSide::Top => m.top = norm.clone(),
                    PvMarginSide::Bottom => m.bottom = norm.clone(),
                }
                let which = match side {
                    PvMarginSide::All => "page margin",
                    PvMarginSide::Left => "left margin",
                    PvMarginSide::Right => "right margin",
                    PvMarginSide::Top => "top margin",
                    PvMarginSide::Bottom => "bottom margin",
                };
                return Ok(Value::str(format!("{which}: {norm}")));
            }
            EditorOp::SetPreviewPaper(v) => {
                let paper = crate::pdf::canon_paper(&v)
                    .map_err(|e| XplError::trappable(format!("PVPAPER: {e}")))?;
                self.preview_paper = paper.clone();
                return Ok(Value::str(format!("page format: {paper}")));
            }
            EditorOp::SetPreviewLeading(v) => {
                let t = v.trim();
                if t.is_empty() || t.eq_ignore_ascii_case("default") {
                    self.preview_leading = None;
                    return Ok(Value::str("line spacing: default"));
                }
                let norm = crate::pdf::normalize_par_len(t)
                    .map_err(|e| XplError::trappable(format!("PVLEADING: {e}")))?;
                self.preview_leading = Some(norm.clone());
                return Ok(Value::str(format!("line spacing: {norm}")));
            }
            EditorOp::SetPreviewSpacing(v) => {
                let t = v.trim();
                if t.is_empty() || t.eq_ignore_ascii_case("default") {
                    self.preview_spacing = None;
                    return Ok(Value::str("paragraph spacing: default"));
                }
                let norm = crate::pdf::normalize_par_len(t)
                    .map_err(|e| XplError::trappable(format!("PVSPACING: {e}")))?;
                self.preview_spacing = Some(norm.clone());
                return Ok(Value::str(format!("paragraph spacing: {norm}")));
            }
            EditorOp::SetPreviewPageNumber(v) => {
                let t = v.trim();
                if t.is_empty() || t.eq_ignore_ascii_case("off") {
                    self.preview_page_num = None;
                    return Ok(Value::str("page numbers: off"));
                }
                // Allow a quoted pattern (`PVPAGENUM "Page 1"`) — strip one surrounding pair.
                let pat = if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
                    &t[1..t.len() - 1]
                } else {
                    t
                };
                self.preview_page_num = Some(pat.to_string());
                return Ok(Value::str(format!("page numbers: {pat}")));
            }
            EditorOp::SetPreviewPageAlign(v) => {
                let pos = crate::pdf::canon_page_align(&v)
                    .map_err(|e| XplError::trappable(format!("PVPAGEPOS: {e}")))?;
                self.preview_page_align = pos.clone();
                return Ok(Value::str(format!("page-number position: {pos}")));
            }
            EditorOp::SetPreviewJustify(on) => {
                self.preview_justify = on;
                return Ok(Value::str(format!("justify: {}", if on { "on" } else { "off" })));
            }
            EditorOp::SetPreviewIndent(v) => {
                let t = v.trim();
                if t.is_empty() || t.eq_ignore_ascii_case("default") || t.eq_ignore_ascii_case("off") {
                    self.preview_indent = None;
                    return Ok(Value::str("first-line indent: none"));
                }
                let norm = crate::pdf::normalize_par_len(t)
                    .map_err(|e| XplError::trappable(format!("PVINDENT: {e}")))?;
                self.preview_indent = Some(norm.clone());
                return Ok(Value::str(format!("first-line indent: {norm}")));
            }
            EditorOp::SetPreviewHeader(v) => {
                let t = v.trim();
                if t.is_empty() || t.eq_ignore_ascii_case("off") {
                    self.preview_header = None;
                    return Ok(Value::str("header: off"));
                }
                // Allow a quoted template (spaces / `;`-free) — strip one surrounding pair.
                let text = if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
                    &t[1..t.len() - 1]
                } else {
                    t
                };
                self.preview_header = Some(text.to_string());
                return Ok(Value::str(format!("header: {text}")));
            }
            EditorOp::SetPreviewFooter(v) => {
                let t = v.trim();
                if t.is_empty() || t.eq_ignore_ascii_case("off") {
                    self.preview_footer = None;
                    return Ok(Value::str("footer: off"));
                }
                let text = if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
                    &t[1..t.len() - 1]
                } else {
                    t
                };
                self.preview_footer = Some(text.to_string());
                return Ok(Value::str(format!("footer: {text}")));
            }
            EditorOp::SetCmlineCaretShape(shape) => {
                self.cmline_caret_shape = shape;
                return Ok(Value::str(format!("command-line caret shape: {}", shape.name())));
            }
            EditorOp::ShowHistory(arg) => {
                self.history_request = Some(arg);
                return Ok(Value::str(""));
            }
            EditorOp::MenuKbd(letter) => {
                self.menu_kbd_request = Some(letter);
                return Ok(Value::str(""));
            }
            EditorOp::LoadHistory(text) => {
                self.command_history =
                    text.split('\n').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
                self.history_pos = None;
                let n = self.command_history.len();
                return Ok(Value::str(format!("loaded {n} history command{}", if n == 1 { "" } else { "s" })));
            }
            EditorOp::Newline => {
                // In the Console tab, Enter at the prompt SUBMITS the command (console_submit is a
                // no-op — returns false — when the console isn't active, no prompt is showing, or the
                // caret is up in scrollback, so a normal newline is inserted there instead).
                if self.console_submit() {
                    return Ok(Value::str(""));
                }
                if self.tab_indent {
                    self.active().insert_newline_autoindent();
                } else {
                    self.active().insert_newline();
                }
                return Ok(Value::str(""));
            }
            EditorOp::Counter { idx, arg } => {
                // Counters belong to the document being authored: read/advance the ACTIVE document's
                // register (the blind buffer during BEDIT) and insert any output there — never the
                // cmline — so a `Cn` run from the command line lands in the text.
                let doc = match &mut self.blind {
                    Some(b) => b,
                    None => &mut self.docs[self.active],
                };
                let out = doc.counter_op(idx as usize, &arg).map_err(XplError::trappable)?;
                if let Some(text) = out {
                    if doc.read_only() {
                        return Err(XplError::trappable(self.tr_msg("msg.readonly", "document is read-only (RO to unlock)")));
                    }
                    doc.insert(&text);
                }
                return Ok(Value::str(""));
            }
            EditorOp::CounterField { idx, arg } => {
                // `PRINTEX` `|Cn …|`: advance/render the counter but RETURN the text (the expander
                // places it) — never insert into a document. Targets the same register set the bare
                // `Cn` verb would (the blind buffer during BEDIT), already snapshotted by SaveCounters.
                let doc = match &mut self.blind {
                    Some(b) => b,
                    None => &mut self.docs[self.active],
                };
                let out = doc.counter_op(idx as usize, &arg).map_err(XplError::trappable)?;
                return Ok(Value::str(out.unwrap_or_default()));
            }
            EditorOp::SaveCounters => {
                let snap = match &self.blind {
                    Some(b) => b,
                    None => &self.docs[self.active],
                }
                .counters_snapshot();
                self.saved_counters = Some(snap);
                return Ok(Value::str(""));
            }
            EditorOp::RestoreCounters => {
                if let Some(snap) = self.saved_counters.take() {
                    let doc = match &mut self.blind {
                        Some(b) => b,
                        None => &mut self.docs[self.active],
                    };
                    doc.restore_counters(snap);
                }
                return Ok(Value::str(""));
            }
            EditorOp::SetEncoding(name) => {
                // Reinterpret the active document's bytes through a new view
                // (live; bytes untouched). An unknown name is trappable.
                let nm = name.trim();
                match crate::encoding::by_name(nm) {
                    Some(enc) => {
                        let label = enc.name().to_string();
                        self.docs[self.active].set_encoding(enc);
                        return Ok(Value::str(format!("encoding: {label}")));
                    }
                    None => {
                        return Err(XplError::trappable(format!(
                            "ENCODING: unknown encoding {nm:?}"
                        )))
                    }
                }
            }
            EditorOp::ConvertEncoding { name, out_file } => {
                let nm = name.trim();
                let target = crate::encoding::by_name(nm).ok_or_else(|| {
                    XplError::trappable(format!("ENCODING/C: unknown encoding {nm:?}"))
                })?;
                let label = target.name().to_string();
                match out_file {
                    Some(path) => {
                        // Write a converted COPY and open it (viewed as the target charset); the
                        // current document is left untouched. The safe form for a lossy conversion.
                        let text = self.docs[self.active].buffer().text();
                        let bytes = target.encode(&text);
                        self.write_doc_bytes(&path, &bytes)?;
                        let resolved = self.resolve_path(&path);
                        let mut doc = Document::new();
                        doc.load(bytes, resolved.clone());
                        doc.set_encoding(target);
                        self.add_or_reuse(doc);
                        return Ok(Value::str(format!(
                            "converted to {label} → {}",
                            resolved.display()
                        )));
                    }
                    None => {
                        // Convert the current document in place (lossy → `?`; leaves it modified).
                        self.docs[self.active].convert_to_encoding(target);
                        return Ok(Value::str(format!("converted to {label} (in place)")));
                    }
                }
            }
            EditorOp::SetNewlineStyle(crlf) => {
                let target = if crlf {
                    crate::document::LineEnding::Crlf
                } else {
                    crate::document::LineEnding::Lf
                };
                self.docs[self.active].convert_line_endings(target);
                return Ok(Value::str(format!("line endings: {}", target.label())));
            }
            EditorOp::BufNext => {
                self.switch_buffer((self.active + 1) % self.docs.len());
            }
            EditorOp::BufPrev => {
                self.switch_buffer((self.active + self.docs.len() - 1) % self.docs.len());
            }
            EditorOp::BufAlt => {
                // Toggle to the alternate buffer (the one active before the last switch);
                // seed it from BUFPREV on first use or if it's gone stale.
                let target = match self.alt_active {
                    Some(i) if i < self.docs.len() && i != self.active => i,
                    _ => (self.active + self.docs.len() - 1) % self.docs.len(),
                };
                self.switch_buffer(target);
            }
            EditorOp::BufGo(arg) => {
                let arg = arg.trim();
                let found = if let Ok(n) = arg.parse::<usize>() {
                    // 1-based index.
                    if n >= 1 && n <= self.docs.len() {
                        self.switch_buffer(n - 1);
                        true
                    } else {
                        false
                    }
                } else {
                    let want = arg.to_lowercase();
                    match self.docs.iter().position(|d| {
                        d.file_name().map(|n| n.to_lowercase()) == Some(want.clone())
                    }) {
                        Some(i) => {
                            self.switch_buffer(i);
                            true
                        }
                        None => false,
                    }
                };
                if !found {
                    return Err(XplError::trappable(format!("BUFGO: no buffer {arg:?}")));
                }
            }
            EditorOp::GoFile(arg) => {
                let arg = arg.trim();
                if arg.is_empty() {
                    return Err(XplError::trappable("GOFILE: a filename or substring is required"));
                }
                // Forgiving contains-match: lowercase + unify path separators, so `GOF a.txt`,
                // `GOF DIR/A.TXT`, and a full path all match the tab's full path.
                let norm = |s: &str| s.to_lowercase().replace('\\', "/");
                let needle = norm(arg);
                let n = self.docs.len();
                // Tabs whose FULL path contains the needle (tabs with no path never match).
                let matches: Vec<usize> = (0..n)
                    .filter(|&i| {
                        self.docs[i]
                            .path()
                            .is_some_and(|p| norm(&p.to_string_lossy()).contains(&needle))
                    })
                    .collect();
                if matches.is_empty() {
                    return Err(XplError::trappable(format!("GOFILE: no open file matching {arg:?}")));
                }
                // The NEXT matching tab after the current one, cycling. So repeated GOFILE walks
                // through several copies; if the current tab is the only match we come back around
                // to it (offset == n) and stay put.
                let target = (1..=n)
                    .map(|off| (self.active + off) % n)
                    .find(|i| matches.contains(i))
                    .unwrap_or(self.active);
                let name = self.docs[target].file_name().unwrap_or_else(|| arg.to_string());
                let msg = if target == self.active {
                    format!("already at {name}")
                } else {
                    self.switch_buffer(target);
                    format!("switched to {name}")
                };
                self.set_focus(Target::Document);
                return Ok(Value::str(msg));
            }
            EditorOp::PaneSplitNew { stacked } => {
                let dir = if stacked {
                    crate::panes::Split::Stacked
                } else {
                    crate::panes::Split::SideBySide
                };
                self.pane_split_new(dir).map_err(XplError::trappable)?;
                self.set_focus(Target::Document);
                return Ok(Value::str(format!("{} panes", self.pane_count())));
            }
            EditorOp::PaneSplitTile { stacked, target } => {
                let dir = if stacked {
                    crate::panes::Split::Stacked
                } else {
                    crate::panes::Split::SideBySide
                };
                self.pane_split_tile(dir, target).map_err(XplError::trappable)?;
                self.set_focus(Target::Document);
                return Ok(Value::str(format!("{} panes", self.pane_count())));
            }
            EditorOp::PaneFocusNext => {
                if self.pane_count() < 2 {
                    return Err(XplError::trappable("TW: only one pane"));
                }
                self.pane_focus_next();
                self.set_focus(Target::Document);
                return Ok(Value::str(format!("pane {}", self.focused_pane_position())));
            }
            EditorOp::PaneCollapse => {
                if !self.pane_collapse() {
                    return Err(XplError::trappable("TUS: only one pane"));
                }
                self.set_focus(Target::Document);
                return Ok(Value::str(format!("{} panes", self.pane_count())));
            }
            EditorOp::PaneFullscreen => {
                // Idempotent: `FS` when already single-pane just confirms full screen (no error).
                self.pane_fullscreen();
                self.set_focus(Target::Document);
                return Ok(Value::str("full screen"));
            }
            EditorOp::PaneShell { stacked } => {
                let dir = if stacked {
                    crate::panes::Split::Stacked
                } else {
                    crate::panes::Split::SideBySide
                };
                self.pane_open_shell(dir).map_err(XplError::trappable)?;
                self.set_focus(Target::Document);
                return Ok(Value::str("shell output in a pane"));
            }
            EditorOp::PaneSwap => {
                if !self.pane_swap() {
                    return Err(XplError::trappable("TRV: nothing to swap (one pane)"));
                }
                return Ok(Value::str("swapped panes"));
            }
            EditorOp::PaneZoom => {
                self.pane_zoom();
                return Ok(Value::str(if self.panes.is_zoomed() {
                    "pane zoomed"
                } else {
                    "zoom off"
                }));
            }
            EditorOp::OpenCmdLine => {
                // Focus the cmline WITHOUT clearing it — it remembers its content + cursor
                // (B23). `BC` blanks it explicitly; Esc (CancelCmdLine) discards it.
                self.set_focus(Target::CmLine);
            }
            EditorOp::CancelCmdLine => {
                self.cmline = Document::new();
                self.set_focus(Target::Document);
            }
            EditorOp::FocusText => {
                // Focus the text window (its buffer already holds its own cursor); the
                // cmline contents are preserved. An EXPLICIT focus choice — the resting-focus rule
                // must not override it (e.g. a later error in the same chain/routine).
                self.set_focus(Target::Document);
                self.explicit_focus = true;
            }
            EditorOp::EnsureDocumentFocus => {
                // An editing/navigation verb is about to fire: if the CMline is focused, move
                // focus to the document so the verb acts there (and leaves you there). Conditional,
                // so `XC`/SUBMIT (which set `target=Document` transiently) are unaffected.
                if matches!(self.target, Target::CmLine) {
                    self.set_focus(Target::Document);
                }
            }
            EditorOp::ToggleFocus => {
                let t = match self.target {
                    Target::CmLine => Target::Document,
                    Target::Document => Target::CmLine,
                };
                self.set_focus(t);
                self.explicit_focus = true; // deliberate focus choice — authoritative (see FocusText)
            }
            EditorOp::BlankCmdLine => {
                self.cmline = Document::new();
                self.set_focus(Target::CmLine);
            }
            EditorOp::SetCmdLine(text) => {
                // Blank, then put the literal text on the cmline (caret at end) and focus it.
                self.set_cmline_text(&text);
            }
            EditorOp::ExecCmdLine => {
                self.cmline_exec = Some(self.command_line().text()); // snapshot now (a later BC in
                                                                     // the chain won't blank it)
            }
            EditorOp::SubmitCmdLine => {
                self.cmline_submit = Some(self.command_line().text());
            }
            EditorOp::Quit { verify } => {
                self.quit_request = Some(verify);
            }
        }
        Ok(Value::str(""))
    }

    fn binder(&self, script: bool, slot: char) -> Option<String> {
        let slot = slot.to_ascii_uppercase();
        let store = if script { &self.script_binders } else { &self.text_binders };
        store.get(&slot).cloned()
    }

    fn float_decimals(&self) -> Option<u8> {
        self.float_decimals
    }

    fn dir_default_switches(&self) -> Vec<String> {
        self.dir_defaults.clone()
    }

    fn special(&self, name: &str) -> Option<Value> {
        // Content `$`-vars read the active document — or, during a blind edit (`BEDIT`), the hidden
        // buffer, so a script can branch on the file it's editing (`$SE`/`$LN`/`$LI`/`$FI`/…).
        let doc = self.blind.as_ref().unwrap_or(&self.docs[self.active]);
        let b = doc.buffer();
        match name {
            "OS" => Some(Value::str(self.os.clone())),
            "CB" => Some(Value::str(self.clipboard.clone())),
            "XB" => Some(Value::str(self.deletion_buffer.clone())),
            "SE" => Some(Value::str(b.selection_text())),
            // The most recent non-empty selection's text — kept after the selection clears, and
            // shared across tabs (captured before each op; see `editor_op`).
            "SEL" => Some(Value::str(self.last_selection.clone())),
            "SL" => Some(Value::Int(b.selection_char_count() as i64)),
            // The character to the right of the caret (a `\r\n` break reports as "\n"; empty
            // at end-of-buffer). The `CH` *verb* is replace-all — `$CH` is this read.
            "CH" => Some(Value::str(b.char_right())),
            // The text of the line under the caret, without its trailing newline.
            "LI" => Some(Value::str(b.line_text(b.cursor_line()))),
            // The word under the caret (no surrounding whitespace); empty off a word.
            "WD" => Some(Value::str(b.word_at())),
            // The ENTIRE text of the active buffer (or the blind-edit buffer during `BEDIT`) — every
            // character, newlines included. `SET all = $DOC` grabs the whole tab at once.
            "DOC" => Some(Value::str(b.text())),
            // Whole-document word count (selection word count is via the WORDCOUNT verb).
            "WC" => Some(Value::Int(b.word_count() as i64)),
            // Replacement count from the most recent `CH` (replace-all).
            "CC" => Some(Value::Int(self.change_count)),
            "GREPBEFORE" => Some(Value::Int(self.grep_before as i64)),
            "GREPAFTER" => Some(Value::Int(self.grep_after as i64)),
            "GREPBLANK" => Some(Value::Int(if self.grep_blank { 1 } else { 0 })),
            "PS" => Some(Value::Int(if self.persistent { 1 } else { 0 })),
            "DIRNW" => Some(Value::Int(if self.new_window { 1 } else { 0 })),
            "DIRWNW" => Some(Value::Int(if self.dir_window_new_tab { 1 } else { 0 })),
            "DIRWCLOSE" => Some(Value::Int(if self.dir_window_close_on_open { 1 } else { 0 })),
            "DIRRT" => Some(Value::Int(if self.dir_reuse { 1 } else { 0 })),
            "DIRSET" => Some(Value::str(self.dir_defaults_string())),
            "TITLEBAR" => Some(Value::Int(if self.show_titlebar { 1 } else { 0 })),
            "WV" => Some(Value::Int(if self.writer_view { 1 } else { 0 })),
            "WINGEO" => Some(Value::Int(if self.remember_window_geometry { 1 } else { 0 })),
            "SESSION" => Some(Value::Int(if self.session_restore { 1 } else { 0 })),
            // GUI view flags (see EditorOp::SetView).
            "MB" => Some(Value::Int(if self.menu_bar { 1 } else { 0 })),
            "VR" => Some(Value::Int(if self.visible_returns { 1 } else { 0 })),
            "WS" => Some(Value::Int(if self.visible_whitespace { 1 } else { 0 })),
            "LNS" => Some(Value::Int(if self.line_numbers { 1 } else { 0 })),
            "GC" => Some(Value::Int(if self.gui_cmline { 1 } else { 0 })),
            "SW" => Some(Value::Int(if self.shell_output_visible { 1 } else { 0 })),
            // Full text of the most-recently-closed scratchpad window (empty until one closes).
            "SCRATCHTEXT" => Some(Value::str(self.scratch_text.clone())),
            // `$SH` now reports whether the Console tab is shown (the SH pane it once meant is gone).
            "SH" => Some(Value::Int(if self.shell_output_visible { 1 } else { 0 })),
            "SHPROMPT" => Some(Value::str(self.console_prompt.clone())),
            "ANSICOLOR" => Some(Value::Int(if self.ansi_color { 1 } else { 0 })),
            "CALC" => Some(Value::str(self.last_calc.clone())), // last CALC result
            "RUNSUM" => Some(Value::str(
                Value::from_number(self.sum_total.clone()).to_str_dp(self.float_decimals),
            )), // cursor-addition running total
            "PV" => Some(Value::Int(if self.preview_visible { 1 } else { 0 })),
            "AR" => Some(Value::str(self.last_ar_path.clone().unwrap_or_default())), // last LoadAR path
            "HLAUTO" => Some(Value::Int(if self.hl_auto { 1 } else { 0 })),
            // `$RO`/`$RE` both report the active tab's read-only flag.
            "RO" | "RE" => Some(Value::Int(if doc.read_only() { 1 } else { 0 })),
            "AU" => Some(Value::Int(self.autosave_secs as i64)), // (renamed from $AUTOSAVE)
            // Tab width in spaces.
            "TB" => Some(Value::Int(self.tab_width as i64)),
            "TABSPACES" => Some(Value::Int(if self.tab_soft { 1 } else { 0 })),
            "TABINDENT" => Some(Value::Int(if self.tab_indent { 1 } else { 0 })),
            "INDENTED" => Some(Value::Int(b.indent_columns(self.tab_width) as i64)),
            "BALPAIRS" => Some(Value::Int(if self.bal_pairs { 1 } else { 0 })),
            "TYPEPAIRS" => Some(Value::Int(if self.type_pairs { 1 } else { 0 })),
            "WRAPINDENT" => Some(Value::Int(if self.wrap_indent { 1 } else { 0 })),
            "BALPAIRSCOLOR" => Some(Value::str(match self.bal_color {
                Some((r, g, b)) => format!("{r} {g} {b}"),
                None => String::new(), // empty = follows the selection colour
            })),
            // Fixed decimal places for float display, or "off" when at default precision.
            "DECIMALS" => Some(match self.float_decimals {
                Some(n) => Value::Int(n as i64),
                None => Value::str("off"),
            }),
            // The active doc's effective PDF/preview render kind: "typst" or "gfm".
            "DOCTYPE" => Some(Value::str(doc.render_kind().as_str())),
            // `$TX` = 1 when the insertion point is in the editor text, 0 in the CMline. Reads
            // the PERSISTENT `input_focus`, not `target` (which is transiently `Document` while a
            // cmline-invoked command runs), so a `PR $TX` typed in the cmline reports 0.
            "TX" => Some(Value::Int(match self.input_focus {
                Target::Document => 1,
                Target::CmLine => 0,
            })),
            // GUI sizing knobs (see EditorOp::SetUiSize).
            "UISCALE" => Some(Value::Float(self.ui_scale as f64)),
            "UIFONT" => Some(Value::Float(self.chrome_font as f64)),
            "EDFONT" => Some(Value::Float(self.editor_font as f64)),
            "EDFAMILY" => Some(Value::str(self.editor_font_family.clone().unwrap_or_default())),
            "EDPROPFAMILY" => Some(Value::str(self.editor_prop_family.clone().unwrap_or_default())),
            "USEFONT" => Some(Value::str(self.docs[self.active].font_mode().as_str())),
            "UIFAMILY" => Some(Value::str(self.ui_font_family.clone().unwrap_or_default())),
            "TXTFAMILY" => Some(Value::str(self.txt_font_family.clone().unwrap_or_default())),
            "EDSTYLE" => Some(Value::str(self.editor_font_style.keyword())),
            "UISTYLE" => Some(Value::str(self.ui_font_style.keyword())),
            "TXTSTYLE" => Some(Value::str(self.txt_font_style.keyword())),
            "TXTFONT" => Some(Value::Float(self.txt_font as f64)),
            "TXTSCALE" => Some(Value::Float(self.txt_scale as f64)),
            // `DIR/W` window appearance — EFFECTIVE values (override else the inherited editor value).
            "DWFAMILY" => Some(Value::str(
                self.dw_font_family.clone().or_else(|| self.editor_font_family.clone()).unwrap_or_default(),
            )),
            "DWSTYLE" => Some(Value::str(self.dw_font_style().keyword())),
            "DWFONT" => Some(Value::Float(self.dw_font() as f64)),
            "DWSCALE" => Some(Value::Float(self.dw_scale as f64)),
            // Markdown Preview / TOPDF fonts — empty string / 0 when unset (Typst's default).
            "PVFAMILY" => Some(Value::str(self.preview_font_family.clone().unwrap_or_default())),
            "PVCODE" => Some(Value::str(self.preview_code_family.clone().unwrap_or_default())),
            "PVSIZE" => Some(Value::Float(self.preview_font_size.unwrap_or(0.0) as f64)),
            "PVCODESIZE" => Some(Value::Float(self.preview_code_size.unwrap_or(0.0) as f64)),
            // Preview/TOPDF page margins. `$PVMARGIN` = the uniform value if all four sides match, else
            // the four "L R T B"; per-side reads each.
            "PVMARGIN" => {
                let m = &self.preview_margins;
                let s = if m.left == m.right && m.right == m.top && m.top == m.bottom {
                    m.left.clone()
                } else {
                    format!("{} {} {} {}", m.left, m.right, m.top, m.bottom)
                };
                Some(Value::str(s))
            }
            "PVMARGINL" => Some(Value::str(self.preview_margins.left.clone())),
            "PVMARGINR" => Some(Value::str(self.preview_margins.right.clone())),
            "PVMARGINT" => Some(Value::str(self.preview_margins.top.clone())),
            "PVMARGINB" => Some(Value::str(self.preview_margins.bottom.clone())),
            "PVPAPER" => Some(Value::str(self.preview_paper.clone())),
            "PVLEADING" => Some(Value::str(self.preview_leading.clone().unwrap_or_default())),
            "PVSPACING" => Some(Value::str(self.preview_spacing.clone().unwrap_or_default())),
            "PVPAGENUM" => Some(Value::str(self.preview_page_num.clone().unwrap_or_default())),
            "PVPAGEPOS" => Some(Value::str(self.preview_page_align.clone())),
            "PVJUSTIFY" => Some(Value::Int(if self.preview_justify { 1 } else { 0 })),
            "PVINDENT" => Some(Value::str(self.preview_indent.clone().unwrap_or_default())),
            "PVHEADER" => Some(Value::str(self.preview_header.clone().unwrap_or_default())),
            "PVFOOTER" => Some(Value::str(self.preview_footer.clone().unwrap_or_default())),
            "LSP" => Some(Value::Float(self.leading as f64)),
            // `$OF` reads the left margin (compat); `$OFL`/`$OFR` the explicit sides.
            "OF" | "OFL" => Some(Value::Int(self.margin_left as i64)),
            "OFR" => Some(Value::Int(self.margin_right as i64)),
            // Editor colours as "R G B" strings (see EditorOp::SetColor).
            "FGCOLOR" => {
                let (r, g, b) = self.fg_color;
                Some(Value::str(format!("{r} {g} {b}")))
            }
            "BGCOLOR" => {
                let (r, g, b) = self.bg_color;
                Some(Value::str(format!("{r} {g} {b}")))
            }
            "UIFGCOLOR" => {
                let (r, g, b) = self.ui_fg_color;
                Some(Value::str(format!("{r} {g} {b}")))
            }
            "UIBGCOLOR" => {
                let (r, g, b) = self.ui_bg_color;
                Some(Value::str(format!("{r} {g} {b}")))
            }
            "CRCOLOR" => {
                let (r, g, b) = self.caret_color;
                Some(Value::str(format!("{r} {g} {b}")))
            }
            "CMCRCOLOR" => {
                let (r, g, b) = self.cmline_caret_color;
                Some(Value::str(format!("{r} {g} {b}")))
            }
            "SEPCOLOR" => {
                let (r, g, b) = self.sep_color;
                Some(Value::str(format!("{r} {g} {b}")))
            }
            // The tab close-× colour: "r g b" when set, "" when defaulting to the UI foreground.
            "TABXCOLOR" => Some(Value::str(match self.tab_x_color {
                Some((r, g, b)) => format!("{r} {g} {b}"),
                None => String::new(),
            })),
            // `$CWD` path colours: "r g b" when set, "" when at the default.
            "PAFGCOLOR" => Some(Value::str(match self.path_fg_color {
                Some((r, g, b)) => format!("{r} {g} {b}"),
                None => String::new(),
            })),
            "PABGCOLOR" => Some(Value::str(match self.path_bg_color {
                Some((r, g, b)) => format!("{r} {g} {b}"),
                None => String::new(),
            })),
            "PVFGCOLOR" => Some(Value::str(match self.preview_fg_color {
                Some((r, g, b)) => format!("{r} {g} {b}"),
                None => String::new(),
            })),
            "PVBGCOLOR" => Some(Value::str(match self.preview_bg_color {
                Some((r, g, b)) => format!("{r} {g} {b}"),
                None => String::new(),
            })),
            "SCRATCHFGCOLOR" => Some(Value::str(match self.scratch_fg_color {
                Some((r, g, b)) => format!("{r} {g} {b}"),
                None => String::new(),
            })),
            "SCRATCHBGCOLOR" => Some(Value::str(match self.scratch_bg_color {
                Some((r, g, b)) => format!("{r} {g} {b}"),
                None => String::new(),
            })),
            "SELBGCOLOR" => {
                let (r, g, b) = self.sel_bg_color;
                Some(Value::str(format!("{r} {g} {b}")))
            }
            "SELFGCOLOR" => {
                let (r, g, b) = self.sel_fg_color;
                Some(Value::str(format!("{r} {g} {b}")))
            }
            // The EFFECTIVE emphasis colours (the override if set, else the selection colour).
            "EMPHFGCOLOR" => {
                let (r, g, b) = self.emph_fg_color();
                Some(Value::str(format!("{r} {g} {b}")))
            }
            "EMPHBGCOLOR" => {
                let (r, g, b) = self.emph_bg_color();
                Some(Value::str(format!("{r} {g} {b}")))
            }
            "FINDBGCOLOR" => {
                let (r, g, b) = self.find_bg_color;
                Some(Value::str(format!("{r} {g} {b}")))
            }
            "FINDFGCOLOR" => {
                let (r, g, b) = self.find_fg_color;
                Some(Value::str(format!("{r} {g} {b}")))
            }
            "SPELLCOLOR" => {
                let (r, g, b) = self.spell_color;
                Some(Value::str(format!("{r} {g} {b}")))
            }
            "GRAMMARCOLOR" => {
                let (r, g, b) = self.grammar_color;
                Some(Value::str(format!("{r} {g} {b}")))
            }
            "PANECOLOR" => {
                let (r, g, b) = self.pane_border_color;
                Some(Value::str(format!("{r} {g} {b}")))
            }
            // Tab-title backgrounds: "R G B" when set, empty string when at egui's theme default.
            "TABACTIVE" => Some(Value::str(tab_color_str(self.tab_active_bg))),
            "TABHOVER" => Some(Value::str(tab_color_str(self.tab_hover_bg))),
            "TABINACTIVE" => Some(Value::str(tab_color_str(self.tab_inactive_bg))),
            "TXTFGCOLOR" => {
                let (r, g, b) = self.txt_fg_color;
                Some(Value::str(format!("{r} {g} {b}")))
            }
            "TXTBGCOLOR" => {
                let (r, g, b) = self.txt_bg_color;
                Some(Value::str(format!("{r} {g} {b}")))
            }
            "DWFGCOLOR" => {
                let (r, g, b) = self.dw_fg_color(); // effective (override else editor fg)
                Some(Value::str(format!("{r} {g} {b}")))
            }
            "DWBGCOLOR" => {
                let (r, g, b) = self.dw_bg_color();
                Some(Value::str(format!("{r} {g} {b}")))
            }
            "CRSHAPE" => Some(Value::str(self.caret_shape.name())),
            "CMCRSHAPE" => Some(Value::str(self.cmline_caret_shape.name())),
            "CMPLACE" => Some(Value::str(self.cmline_placement.name())),
            "STATUSLINE" => Some(Value::str(if self.status_line_top { "top" } else { "bottom" })),
            "CMBLANK" => Some(Value::Int(if self.cmline_blank { 1 } else { 0 })),
            "SHOWONETAB" => Some(Value::Int(if self.show_one_tab { 1 } else { 0 })),
            "CARETMOTION" => {
                Some(Value::str(if self.caret_visual { "visual" } else { "logical" }.to_string()))
            }
            "WRAP" => Some(Value::Int(if self.wrap { 1 } else { 0 })),
            "HEX" => Some(Value::Int(if self.active_doc().hex_view() { 1 } else { 0 })),
            "MARKS" => Some(Value::Int(self.active_doc().mark_count() as i64)),
            "MARK" => Some(Value::Int(if self.active_doc().mark_at_cursor() { 1 } else { 0 })),
            "WRAPHANG" => Some(Value::Int(self.wrap_hang as i64)),
            "REFLOWWIDTH" => Some(Value::Int(self.reflow_width as i64)),
            "UNTITLEDTAB" => Some(Value::Int(if self.untitled_auto_close { 1 } else { 0 })),
            "GOTOOPEN" => Some(Value::Int(if self.goto_open { 1 } else { 0 })),
            "MARKGUTTER" => Some(Value::str(mark_gutter_word(self.mark_gutter))),
            "MARKFGCOLOR" => Some(Value::str(match self.mark_fg_color {
                Some((r, g, b)) => format!("{r} {g} {b}"),
                None => String::new(),
            })),
            "MARKBGCOLOR" => Some(Value::str(match self.mark_bg_color {
                Some((r, g, b)) => format!("{r} {g} {b}"),
                None => String::new(),
            })),
            "LNFGCOLOR" => Some(Value::str(match self.line_num_fg_color {
                Some((r, g, b)) => format!("{r} {g} {b}"),
                None => String::new(),
            })),
            "LNBGCOLOR" => Some(Value::str(match self.line_num_bg_color {
                Some((r, g, b)) => format!("{r} {g} {b}"),
                None => String::new(),
            })),
            "MSGHFGCOLOR" => Some(Value::str(match self.msg_header_fg_color {
                Some((r, g, b)) => format!("{r} {g} {b}"),
                None => String::new(),
            })),
            "MSGBGCOLOR" => Some(Value::str(match self.msg_bg_color {
                Some((r, g, b)) => format!("{r} {g} {b}"),
                None => String::new(),
            })),
            "MSGOKFGCOLOR" => Some(Value::str(match self.msg_ok_fg_color {
                Some((r, g, b)) => format!("{r} {g} {b}"),
                None => String::new(),
            })),
            "MSGOKBGCOLOR" => Some(Value::str(match self.msg_ok_bg_color {
                Some((r, g, b)) => format!("{r} {g} {b}"),
                None => String::new(),
            })),
            "AUTOREOPEN" => Some(Value::Int(if self.auto_reopen { 1 } else { 0 })),
            "ERRORLOG" => Some(Value::Int(if self.error_log { 1 } else { 0 })),
            // The effective log path (default included), so a script/user can see where it writes.
            "ERRORLOGFILE" => Some(Value::str(self.error_log_path().to_string_lossy().into_owned())),
            "DIMONBLUR" => Some(Value::Int(if self.dim_on_blur { 1 } else { 0 })),
            "CLOSEBACK" => Some(Value::Int(if self.close_back { 1 } else { 0 })),
            "URLHIGHLIGHT" => Some(Value::Int(if self.url_highlight { 1 } else { 0 })),
            "PVMARKS" => Some(Value::Int(if self.preview_marks { 1 } else { 0 })),
            "URLFGCOLOR" => Some(Value::str(match self.url_fg_color {
                Some((r, g, b)) => format!("{r} {g} {b}"),
                None => String::new(),
            })),
            "URLBGCOLOR" => Some(Value::str(match self.url_bg_color {
                Some((r, g, b)) => format!("{r} {g} {b}"),
                None => String::new(),
            })),
            "ULSPACES" => Some(Value::Int(if self.ul_spaces { 1 } else { 0 })),
            #[cfg(feature = "email")]
            "SMTPURL" => Some(Value::str(self.smtp_url.clone())),
            #[cfg(feature = "email")]
            "MAILUSER" => Some(Value::str(self.mail_user.clone())),
            #[cfg(feature = "email")]
            "MAILFROM" => Some(Value::str(self.mail_from.clone())),
            #[cfg(feature = "email")]
            "MAILPASS" => Some(Value::str(if self.mail_pass.is_empty() { "not set" } else { "set" })),
            #[cfg(feature = "email")]
            "MAILDEBUG" => Some(Value::Int(if self.mail_debug { 1 } else { 0 })),
            #[cfg(feature = "email")]
            "IMAPURL" => Some(Value::str(self.imap_url.clone())),
            #[cfg(feature = "email")]
            "MAILBOX" => Some(Value::str(self.mailbox.clone())),
            #[cfg(feature = "email")]
            "ATTACH" => Some(Value::str(
                self.mail_attachments
                    .iter()
                    .map(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join(", "),
            )),
            // Email specials are named unconditionally (documented + $-readable); without the `email`
            // feature they read empty so a script degrades gracefully rather than erroring.
            #[cfg(not(feature = "email"))]
            "SMTPURL" | "MAILUSER" | "MAILFROM" | "IMAPURL" | "MAILBOX" | "ATTACH" => {
                Some(Value::str(String::new()))
            }
            #[cfg(not(feature = "email"))]
            "MAILPASS" => Some(Value::str("not set")),
            #[cfg(not(feature = "email"))]
            "MAILDEBUG" => Some(Value::Int(0)),
            "PAPLACE" => Some(Value::str(self.path_place.name())),
            "PATHLEN" => Some(Value::Int(self.path_len as i64)),
            "TEXTFILES" => Some(Value::str(
                self.text_file_exts.iter().map(|e| format!(".{e}")).collect::<Vec<_>>().join(","),
            )),
            // The active document's line-ending style (`LF`/`CRLF`).
            "NEWLINE" => Some(Value::str(doc.line_ending().label())),
            // The active document's line-ending CHARACTERS (`\n` or `\r\n`) — the character twin of
            // `$NEWLINE`, for building text/file content that matches the document (a string `\n`
            // escape is always a lone LF). New documents adopt the OS default, so this is
            // OS-appropriate until a loaded file says otherwise.
            "NL" => Some(Value::str(doc.line_ending().as_str())),
            "TR" => Some(Value::Int(if self.shell_trusted { 1 } else { 0 })),
            "SPELLING" => Some(Value::Int(if self.spell_on { 1 } else { 0 })),
            "GRAMMAR" => Some(Value::Int(if self.grammar_on { 1 } else { 0 })),
            "DIALECT" => Some(Value::str(self.lint_dialect.name())),
            "SPELLENGINE" => Some(Value::str(self.spell_engine.name())),
            "SPELLDICT" => Some(Value::str(self.spell_dicts.clone())),
            "SPELLBIN" => Some(Value::str(self.spell_bin.clone())),
            // Path of the loaded LANG catalog; empty string when English (no catalog).
            "LANG" => Some(Value::str(self.lang_path.clone().unwrap_or_default())),
            "SPELLSTYLE" => Some(Value::str(self.spell_style.name())),
            "GRAMMARSTYLE" => Some(Value::str(self.grammar_style.name())),
            "PANES" => Some(Value::Int(self.pane_count() as i64)),
            "PANE" => Some(Value::Int(self.focused_pane_position() as i64)),
            "RC" => Some(Value::Int(self.last_rc)),
            "XO" => Some(Value::str(self.last_output.clone())),
            "JOBS" => Some(Value::Int(self.jobs.len() as i64)),
            "LJ" => Some(Value::Int(self.last_job_id as i64)),
            "ET" => Some(Value::str(self.elapsed_string())),
            "ETS" => Some(Value::Int(self.stopwatch_start.elapsed().as_secs() as i64)),
            // AutoIt-style date/time fields + `$DA`/`$TM` strings, all in LOCAL time.
            "YEAR" | "MON" | "MDAY" | "HOUR" | "MIN" | "SEC" | "MSEC" | "WDAY" | "YDAY" | "DA"
            | "TM" => {
                let dt = self.now_local();
                Some(match name {
                    "YEAR" => Value::str(format!("{:04}", dt.year)),
                    "MON" => Value::str(format!("{:02}", dt.month)),
                    "MDAY" => Value::str(format!("{:02}", dt.mday)),
                    "HOUR" => Value::str(format!("{:02}", dt.hour)),
                    "MIN" => Value::str(format!("{:02}", dt.min)),
                    "SEC" => Value::str(format!("{:02}", dt.sec)),
                    "MSEC" => Value::str(format!("{:03}", dt.msec)),
                    "WDAY" => Value::str(format!("{}", dt.wday)),
                    "YDAY" => Value::str(format!("{:03}", dt.yday)),
                    "DA" => Value::str(format!("{:04}-{:02}-{:02}", dt.year, dt.month, dt.mday)),
                    _ => Value::str(format!("{:02}:{:02}:{:02}", dt.hour, dt.min, dt.sec)), // TM
                })
            }
            "DF" => Some(Value::Int(if b.has_selection() { 1 } else { 0 })),
            // Directory-listing (`DIR`/`LS`) reads — all empty / 0 when the active buffer isn't a listing.
            "D1" => Some(Value::Int(if doc.is_dir_listing() { 1 } else { 0 })),
            // The listing's directory, with the active file mask (`DIR *.txt`) joined on if any.
            // When the separate `DIR/W` window is open and the active tab is NOT itself a listing,
            // these report the DIR/W listing instead (so a script can read it without switching).
            "DP" => Some(Value::str(dir_dp_string(self.dir_special_source(doc)))),
            // The full path of the file/subdirectory on the caret's line in the listing.
            "DR" => Some(Value::str(match self.dir_special_source(doc).dir_entry_at_cursor() {
                Some(e) => e.path.to_string_lossy().into_owned(),
                None => String::new(),
            })),
            "CP" => Some(Value::Int(b.cursor() as i64)),
            "LN" => Some(Value::Int(b.line() as i64)),
            // Multi-caret: `$CARETS` = number of carets, `$CARETP` = the primary's index in the
            // document-order caret list. The classic caret specials above read the PRIMARY; the `$M…`
            // arrays below MIRROR each classic caret-special, one 0-based element per caret in document
            // order (so `$X == $MX[$CARETP]`, and `LEN($MX) == $CARETS`). See the multi-caret specs.
            "CARETS" => Some(Value::Int(b.caret_count() as i64)),
            "CARETP" => Some(Value::Int(b.primary_index() as i64)),
            // `$MCP` — every caret's byte offset (mirror of `$CP`).
            "MCP" => Some(Value::Array(
                b.all_carets().iter().map(|c| Value::Int(c.pos as i64)).collect(),
            )),
            // `$MSE` — each caret's selected text (`""` for a bare caret); mirror of `$SE`.
            "MSE" => Some(Value::Array(
                b.all_carets()
                    .iter()
                    .map(|c| {
                        Value::str(match c.anchor {
                            Some(a) => b.slice(a.min(c.pos), a.max(c.pos)),
                            None => String::new(),
                        })
                    })
                    .collect(),
            )),
            // `$MSL` — chars selected by each caret (0 if bare); mirror of `$SL`.
            "MSL" => Some(Value::Array(
                b.all_carets()
                    .iter()
                    .map(|c| {
                        Value::Int(match c.anchor {
                            Some(a) => b.slice(a.min(c.pos), a.max(c.pos)).chars().count() as i64,
                            None => 0,
                        })
                    })
                    .collect(),
            )),
            // `$MDF` — 1 if each caret has a selection, else 0; mirror of `$DF`.
            "MDF" => Some(Value::Array(
                b.all_carets().iter().map(|c| Value::Int(c.anchor.is_some() as i64)).collect(),
            )),
            // `$MDS` / `$MDN` — each caret's selection start / end (== its position if bare); mirrors
            // of `$DS` / `$DN`.
            "MDS" => Some(Value::Array(
                b.all_carets()
                    .iter()
                    .map(|c| Value::Int(c.anchor.map_or(c.pos, |a| a.min(c.pos)) as i64))
                    .collect(),
            )),
            "MDN" => Some(Value::Array(
                b.all_carets()
                    .iter()
                    .map(|c| Value::Int(c.anchor.map_or(c.pos, |a| a.max(c.pos)) as i64))
                    .collect(),
            )),
            // `$MCS` — 1 if each caret is on its own selection (always true when it has one), else 0;
            // mirror of `$CS`.
            "MCS" => Some(Value::Array(
                b.all_carets().iter().map(|c| Value::Int(c.anchor.is_some() as i64)).collect(),
            )),
            // `$MCH` — the char to the right of each caret; mirror of `$CH`.
            "MCH" => Some(Value::Array(
                b.all_carets().iter().map(|c| Value::str(b.char_right_at(c.pos))).collect(),
            )),
            // `$MWD` — the word under each caret; mirror of `$WD`.
            "MWD" => Some(Value::Array(
                b.all_carets().iter().map(|c| Value::str(b.word_at_offset(c.pos))).collect(),
            )),
            // `$MLI` — the text of each caret's line; mirror of `$LI`.
            "MLI" => Some(Value::Array(
                b.all_carets().iter().map(|c| Value::str(b.line_text(b.line_of(c.pos)))).collect(),
            )),
            // `$MLN` — each caret's 1-based line number; mirror of `$LN`.
            "MLN" => Some(Value::Array(
                b.all_carets().iter().map(|c| Value::Int(b.line_of(c.pos) as i64 + 1)).collect(),
            )),
            // Caret's VISUAL column (0-based, wrap-aware) — the GUI pushes it each frame; headless
            // falls back to the logical column. (Logical line:column is `$LN`:`$LC`.)
            "CX" => Some(Value::Int(
                self.cursor_vis.map(|(c, _)| c).unwrap_or(b.column() as i64),
            )),
            // Caret's VISUAL row (0-based, relative to the viewport top, wrap-aware) — GUI-pushed;
            // headless falls back to the logical row relative to the viewport top.
            "CY" => Some(Value::Int(self.cursor_vis.map(|(_, r)| r).unwrap_or_else(|| {
                b.line().saturating_sub(1).saturating_sub(self.viewport_top) as i64
            }))),
            // Selection start/end as offsets in `$CP` space; fall back to the cursor when there's
            // no selection.
            "DS" => Some(Value::Int(b.selection_span().map(|(s, _)| s).unwrap_or(b.cursor()) as i64)),
            "DN" => Some(Value::Int(b.selection_span().map(|(_, e)| e).unwrap_or(b.cursor()) as i64)),
            // `$CS` — 1 if the caret is on the current selection (inside it, or at either edge), else 0.
            "CS" => Some(Value::Int(match b.selection_span() {
                Some((s, e)) if (s..=e).contains(&b.cursor()) => 1,
                _ => 0,
            })),
            // Caret's logical column (1-based), the complement to `$LN` (logical line, 1-based).
            "LC" => Some(Value::Int(b.column() as i64 + 1)),
            // Document line count (was `$LC`; rehomed to `$TL` — "total lines" — when `$LC` became
            // the column).
            "TL" => Some(Value::Int(b.line_count() as i64)),
            // Current encoding-view name of the active document.
            "EN" => Some(Value::str(doc.encoding_name().to_string())),
            // File identity of the active document.
            "MO" => Some(Value::Int(if doc.is_modified() { 1 } else { 0 })),
            "FI" => Some(Value::str(doc.file_name().unwrap_or_default())),
            "FP" => Some(Value::str(
                doc.path()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            )),
            // `$CWD` and its synonym `$PA`; `$DC` is the OS directory separator.
            "CWD" | "PA" => Some(Value::str(self.cwd.to_string_lossy().into_owned())),
            // OS directory-separator char (`\` on Windows, `/` elsewhere). `$DC` = "directory char".
            "DC" => Some(Value::str(std::path::MAIN_SEPARATOR.to_string())),
            // Cursor at buffer start (`$FB`) / end (`$FE`); buffer byte size (`$FS`).
            "FB" => Some(Value::Int(if b.cursor() == 0 { 1 } else { 0 })),
            "FE" => Some(Value::Int(if b.cursor() == b.len() { 1 } else { 0 })),
            "FS" => Some(Value::Int(b.len() as i64)),
            // Tabs: total count (`$TC`) and 1-based active index (`$CT`).
            "TC" => Some(Value::Int(self.docs.len() as i64)),
            "CT" => Some(Value::Int((self.active + 1) as i64)),
            // Lock-key OS toggle state.
            // Lock-key states (renamed from $CL/$NL/$RL to free $CL for the last cmline command).
            "CAPSLOCK" => Some(Value::Int(lock_key_on(VK_CAPITAL) as i64)),
            "NUMLOCK" => Some(Value::Int(lock_key_on(VK_NUMLOCK) as i64)),
            "SCROLLLOCK" => Some(Value::Int(lock_key_on(VK_SCROLL) as i64)),
            // Command line: `$CM` = its current content; `$CL` = the last command issued from it
            // (the newest command-history entry; empty if none yet).
            "CM" => Some(Value::str(self.cmline.buffer().text())),
            "CL" => Some(Value::str(
                self.command_history.first().cloned().unwrap_or_default(),
            )),
            // The whole command history (newest first) as one string, joined with OS-appropriate
            // newlines (CRLF on Windows, LF elsewhere). The `CMH` verb is a separate namespace.
            "CMH" => Some(Value::str(
                self.command_history.join(crate::document::LineEnding::os_default().as_str()),
            )),
            // Config files: `$CF` = last LOADCFG'd path (persists); `$CFA` = the AUTOCFG tracked
            // path (the file being live-updated; empty when tracking is off).
            "CF" => Some(Value::str(self.last_loadcfg_path.clone().unwrap_or_default())),
            "CFA" => Some(Value::str(self.config_track_path.clone().unwrap_or_default())),
            // Last keymap LOADKBD'd (full path) and last status-line message.
            "KB" => Some(Value::str(
                self.last_kbd.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
            )),
            // Active keyboard-layout slot (`$KBDMODE`) and the space-joined list of resident slots
            // (`$KBDMODES`) — for modal / multi-layout switching (KBD / KBDMODE).
            "KBDMODE" => Some(Value::str(self.kbd_mode.clone())),
            "KBDMODES" => Some(Value::str(self.kbd_mode_names().join(" "))),
            "LS" => Some(Value::str(self.last_status_line.clone())),
            // The running XyScript executable's full path (`$ED`) and its directory (`$XSDIR`).
            "ED" => Some(Value::str(
                std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
            )),
            "XSDIR" => Some(Value::str(
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.to_string_lossy().into_owned()))
                    .unwrap_or_default(),
            )),
            // XyScript version — the build's crate version (tracks Cargo.toml), e.g. "0.1.0".
            "VE" => Some(Value::str(env!("CARGO_PKG_VERSION"))),
            // `$C1`..`$C9` peek the counter's current value (what a bare `Cn` would put next),
            // rendered in its kind/case but WITHOUT the decorator, and without advancing it.
            "C1" | "C2" | "C3" | "C4" | "C5" | "C6" | "C7" | "C8" | "C9" => {
                let idx = (name.as_bytes()[1] - b'1') as usize;
                doc.counter_peek(idx).map(Value::str)
            }
            _ => None,
        }
    }

    fn read_file(&self, path: &str) -> VmResult<String> {
        if let Some(text) = self.files.get(path) {
            return Ok(text.clone());
        }
        if self.virtual_fs {
            return Err(XplError::trappable_code(ErrCode::FileNotFound, self.tr_fmt("msg.open.not_found", "file not found: {path}", &[("path", &path)])));
        }
        // Resolve relative paths against the editor's working dir ($CWD / `CD`), like every
        // other file op — NOT the process cwd. Absolute paths (e.g. `$FP`) pass through. This
        // is what lets `RUN file`/`LOADKBD file` find files where the user expects.
        let resolved = self.resolve_path(path);
        cloud_only_guard(&resolved)?;
        std::fs::read_to_string(&resolved).map_err(|e| {
            use std::io::ErrorKind::*;
            let p = resolved.display();
            XplError::trappable(match e.kind() {
                // read_to_string rejects non-UTF-8 content as InvalidData; say so
                // clearly rather than leaving it to look like an access failure.
                InvalidData => {
                    format!("{p}: not a UTF-8 text file (legacy/binary encoding not supported yet)")
                }
                PermissionDenied => format!("{p}: permission denied"),
                NotFound => format!("{p}: not found"),
                _ => format!("cannot read {p}: {e}"),
            })
        })
    }

    fn active_text(&self) -> String {
        // Same target resolution as the content `$`-vars: the on-screen doc, or the blind-edit
        // buffer when one is open.
        let doc = self.blind.as_ref().unwrap_or(&self.docs[self.active]);
        doc.buffer().text()
    }

    fn point_and_shoot_file(&self) -> Option<String> {
        // The on-screen active buffer (blind-edit never applies to a listing). `dir_entry_at_cursor`
        // is line-based, so the caret anywhere on an entry's row counts (whole-row, per the design).
        let doc = &self.docs[self.active];
        if !doc.is_dir_listing() {
            return None;
        }
        let entry = doc.dir_entry_at_cursor()?;
        if entry.name == ".." {
            return None; // the synthesized parent — operating on it makes no sense
        }
        Some(entry.path.to_string_lossy().into_owned())
    }

    // `REF <term>` — search the command REFERENCE only (the former `HELP` behaviour). Boolean/
    // proximity when the query reads that way, else substring/regex; an exact headword is promoted
    // to the top. Bare `REF` gives the reference overview.
    fn help(&self, query: Option<&str>, _search: bool) -> HelpResponse {
        let Some(q) = query else {
            return HelpResponse { body: crate::help::overview(), not_found: false, emphasis: None };
        };
        match reference_block(q) {
            Some(body) => HelpResponse { body, not_found: false, emphasis: crate::help::query_emphasis(q) },
            None => help_miss(q),
        }
    }

    // `MAN <topic>` — search the MANUAL only. Boolean/proximity like `REF`/`HELP`.
    fn manual_search(&self, q: &str) -> HelpResponse {
        match manual_block(q) {
            Some(body) => HelpResponse { body, not_found: false, emphasis: crate::help::query_emphasis(q) },
            None => HelpResponse {
                body: format!("No manual sections match {q:?}. Type MAN for an overview."),
                not_found: true,
                emphasis: None,
            },
        }
    }

    // `HELP <term>` — search BOTH: the reference first, then the manual, in one results tab. Bare
    // `HELP` gives the reference overview (which points at `MAN` for the manual).
    fn help_all(&self, query: Option<&str>) -> HelpResponse {
        let Some(q) = query else {
            return HelpResponse { body: crate::help::overview(), not_found: false, emphasis: None };
        };
        let blocks: Vec<String> = [reference_block(q), manual_block(q)].into_iter().flatten().collect();
        if blocks.is_empty() {
            return help_miss(q); // nothing in either corpus → near-name suggestions
        }
        HelpResponse {
            body: blocks.join("\n\n\n"),
            not_found: false,
            emphasis: crate::help::query_emphasis(q),
        }
    }

    fn selection_in(&self, n: i64) -> String {
        // `SEL(n)` — tab n's selection (1-based; "" if out of range or no selection there).
        if n < 1 {
            return String::new();
        }
        match self.docs.get((n - 1) as usize) {
            Some(doc) => doc.buffer().selection_text(),
            None => String::new(),
        }
    }

    fn cross_tab_selection_candidates(&self) -> Vec<(usize, String)> {
        // Every tab except the active one that has a non-empty selection, with a one-line preview
        // label: `<1-based#>  <name>: <preview…>`.
        self.docs
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != self.active)
            .filter_map(|(i, doc)| {
                let sel = doc.buffer().selection_text();
                if sel.is_empty() {
                    return None;
                }
                let name = doc.file_name().unwrap_or_else(|| "(unnamed)".to_string());
                Some((i, format!("{}  {}: {}", i + 1, name, preview_one_line(&sel))))
            })
            .collect()
    }

    fn copy_selection_from(&mut self, buffer_index: usize) -> VmResult<String> {
        let text = match self.docs.get(buffer_index) {
            Some(doc) => doc.buffer().selection_text(),
            None => return Err(XplError::trappable("CP: that tab is no longer open")),
        };
        if text.is_empty() {
            return Err(XplError::trappable("CP: that tab has no selection"));
        }
        let n = text.chars().count();
        self.active().insert(&text); // insert at the active caret (a normal, undoable edit)
        Ok(format!("CP: copied {n} char(s) from tab {}", buffer_index + 1))
    }

    fn reopen_candidates(&self) -> Vec<(String, usize, String)> {
        // Two columns: filename (left-justified, padded to the longest, capped) then its directory.
        let names: Vec<String> = self
            .recently_closed
            .iter()
            .map(|(p, _)| {
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.to_string_lossy().into_owned())
            })
            .collect();
        let width = names.iter().map(|n| n.chars().count()).max().unwrap_or(0).min(REOPEN_NAME_COL);
        self.recently_closed
            .iter()
            .enumerate()
            .map(|(i, (p, caret))| {
                let shown = truncate_col(&names[i], width);
                let dir = p.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
                let label = format!("{shown:<width$}  {dir}");
                (p.to_string_lossy().into_owned(), *caret, label)
            })
            .collect()
    }

    fn reopen_file(&mut self, path: &str, caret: usize) -> VmResult<String> {
        self.recently_closed.retain(|(p, _)| p.to_string_lossy() != path); // consumed by reopen
        if let Some(idx) = self.find_open_tab(path) {
            self.set_active(idx); // already open → just switch to it
            return Ok(format!("switched to {path}"));
        }
        let msg = self.open_doc(path, false, false)?; // opens + focuses the document
        let len = self.active().buffer().len();
        self.set_active_caret(caret.min(len), None); // restore caret (clamped; byte-rope, any offset valid)
        Ok(msg.to_str())
    }

    fn write_file(&mut self, path: &str, content: &str) -> VmResult<()> {
        if self.virtual_fs {
            self.files.insert(path.to_string(), content.to_string());
            return Ok(());
        }
        // Relative paths resolve against the editor's working dir ($CWD), like every other
        // file op (absolutes pass through). Atomic on disk: write a sibling temp file, then
        // rename over the target (Rust's rename replaces on every platform), so a failed
        // write can't corrupt the original.
        let target = self.resolve_path(path);
        let mut tmp_os = target.as_os_str().to_os_string();
        tmp_os.push(".xyscript-tmp");
        let tmp = PathBuf::from(tmp_os);
        std::fs::write(&tmp, content)
            .map_err(|e| XplError::trappable(format!("cannot write {}: {e}", target.display())))?;
        std::fs::rename(&tmp, &target).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            XplError::trappable(format!("cannot finalize {}: {e}", target.display()))
        })?;
        Ok(())
    }

    fn read_file_enc(&self, path: &str, charset: Option<&str>) -> VmResult<String> {
        // No charset (or UTF-8) → the plain text path, unchanged.
        let cs = charset.unwrap_or("utf-8");
        if cs.eq_ignore_ascii_case("utf-8") || cs.eq_ignore_ascii_case("utf8") {
            return self.read_file(path);
        }
        let bytes = self.read_bytes(path)?;
        // `hex` is a pseudo-encoding: the raw bytes as a lowercase hex string (safe to display/edit).
        if cs.eq_ignore_ascii_case("hex") {
            return Ok(bytes_to_hex(&bytes));
        }
        // Otherwise a real single-byte code page from the catalog (decode never fails — undefined bytes
        // map to private-use round-trip points). `latin1` is the byte-exact "binary" read.
        let enc = crate::encoding::by_name(cs)
            .ok_or_else(|| XplError::trappable(format!("READFILE: unknown encoding {cs:?}")))?;
        Ok(enc.decode(&bytes))
    }

    fn write_file_enc(
        &mut self,
        path: &str,
        charset: Option<&str>,
        strict: bool,
        content: &str,
    ) -> VmResult<()> {
        let cs = charset.unwrap_or("utf-8");
        if cs.eq_ignore_ascii_case("utf-8") || cs.eq_ignore_ascii_case("utf8") {
            return self.write_file(path, content); // nothing is unrepresentable in UTF-8
        }
        let bytes = if cs.eq_ignore_ascii_case("hex") {
            hex_to_bytes(content).map_err(|e| XplError::trappable(format!("WRITEFILE/HEX: {e}")))?
        } else {
            // Code-page encode. `latin1` = binary. With `/STRICT`, a character the page can't hold is a
            // trappable error (nothing written); otherwise it becomes `?` (as `ENCODING/C` does).
            let enc = crate::encoding::by_name(cs)
                .ok_or_else(|| XplError::trappable(format!("WRITEFILE: unknown encoding {cs:?}")))?;
            if strict {
                enc.encode_strict(content).map_err(|c| {
                    XplError::trappable(format!(
                        "WRITEFILE/STRICT: character {c:?} (U+{:04X}) is not representable in {cs}",
                        c as u32
                    ))
                })?
            } else {
                enc.encode(content)
            }
        };
        self.write_bytes(path, &bytes)
    }

    fn error_log_enabled(&self) -> bool {
        self.error_log
    }

    fn log_error(&mut self, fatal: bool, code: u32, command: &str, location: &str, message: &str) {
        if !self.error_log {
            return;
        }
        // Assemble the 8-column TSV row. Fields never contain a tab/newline (the message might, so
        // sanitize); fatals carry no $ERROR code/class, and unknown fields use "-" to keep 8 columns.
        let clean = |s: &str| s.replace('\t', " ").replace(['\r', '\n'], " ");
        let dt = self.now_local();
        let ts = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            dt.year, dt.month, dt.mday, dt.hour, dt.min, dt.sec
        );
        let tier = if fatal { "FATAL" } else { "TRAP" };
        let code_col = if fatal { "-".to_string() } else { code.to_string() };
        let class_col = if fatal { "-".to_string() } else { xyscript_xpl::error_message(code).to_string() };
        let doc = self
            .active_doc()
            .path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let non_empty = |s: String| if s.is_empty() { "-".to_string() } else { s };
        let row = format!(
            "{tier}\t{ts}\t{code}\t{class}\t{cmd}\t{loc}\t{doc}\t{msg}\n",
            code = code_col,
            class = clean(&class_col),
            cmd = clean(command),
            loc = clean(&non_empty(location.to_string())),
            doc = clean(&non_empty(doc)),
            msg = clean(message),
        );
        const HEADER: &str =
            "# tier\ttimestamp\tcode\tclass\tcommand\tlocation\tdoc\tmessage\n";
        let path = self.error_log_path();
        // Best-effort: a logging failure is swallowed — it must never set $ERROR or recurse.
        if self.virtual_fs {
            let key = path.to_string_lossy().into_owned();
            let entry = self.files.entry(key).or_default();
            if entry.is_empty() {
                entry.push_str(HEADER);
            }
            entry.push_str(&row);
            return;
        }
        let need_header = !path.exists();
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            if need_header {
                let _ = f.write_all(HEADER.as_bytes());
            }
            let _ = f.write_all(row.as_bytes());
            let _ = f.flush();
        }
    }

    fn shell(&mut self, cmd: &str) -> VmResult<(String, i64)> {
        if !self.shell_trusted {
            return Err(XplError::trappable(
                "shell-out is disabled; enable it with TRUST 1",
            ));
        }
        let (output, code) = run_shell_capture(cmd);
        self.last_rc = code;
        self.last_output = output.clone();
        Ok((output, code))
    }

    fn shell_spawn(&mut self, cmd: &str) -> VmResult<()> {
        if !self.shell_trusted {
            return Err(XplError::trappable(
                "shell-out is disabled; enable it with TRUST 1",
            ));
        }
        spawn_shell(cmd).map_err(|e| XplError::trappable(format!("X/NV: {e}")))?;
        self.last_rc = 0;
        Ok(())
    }

    fn buffer_list(&self) -> Vec<String> {
        self.docs
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let active = if i == self.active { '*' } else { ' ' };
                let modified = if d.is_modified() { "[+]" } else { "   " };
                let name = d.file_name().unwrap_or_else(|| "(untitled)".to_string());
                format!("{active} {} {modified} {name}", i + 1)
            })
            .collect()
    }

    fn switch_to_buffer(&mut self, index: usize) -> VmResult<()> {
        if index < self.docs.len() {
            self.switch_buffer(index); // records the alternate (BUFALT) + moves active
            self.set_focus(Target::Document);
        }
        Ok(())
    }

    fn bookmark_list(&self) -> Vec<(usize, String)> {
        self.active_doc().mark_previews()
    }

    fn calc_context(&self) -> CalcContext {
        let b = self.active_doc().buffer();
        // A selection always wins — it can outlive a focus flip to the command
        // line (persistent selection), and `CALC` typed on the cmline should
        // still sum the lines the user has highlighted in the text.
        if let Some((s, e)) = b.selection_span().filter(|&(s, e)| e > s) {
            // Whole logical lines spanned by the selection (the last line is the
            // one containing the byte just before the selection end).
            let first = b.line_of(s);
            let last = b.line_of(e.saturating_sub(1)).max(first);
            let lines = (first..=last).map(|l| b.line_text(l)).collect();
            return CalcContext::Lines(lines);
        }
        // No selection: on the command line there is no text line to evaluate.
        if matches!(self.input_focus, Target::CmLine) {
            return CalcContext::CmLine;
        }
        CalcContext::Line(b.line_text(b.line_of(b.cursor())))
    }

    fn calc_insert_result(&mut self, text: &str) {
        let le = self.active_doc().line_ending().as_str().to_string();
        // The result goes on a fresh line below the current line — or below the
        // last selected line when a selection drove the evaluation.
        let content_end = {
            let b = self.active_doc().buffer();
            let target = match b.selection_span().filter(|&(s, e)| e > s) {
                Some((_, e)) => b.line_of(e.saturating_sub(1)),
                None => b.line_of(b.cursor()),
            };
            b.line_byte_range(target).1
        };
        self.active().set_caret(content_end, None);
        self.active().insert(&format!("{le}{text}"));
    }

    fn calc_set_clipboard(&mut self, text: &str) {
        // Mirrors a COPY: the GUI's `sync_clipboard_out` pushes `$CB` to the OS.
        self.set_clipboard(text.to_string());
    }

    fn set_last_calc(&mut self, text: &str) {
        self.last_calc = text.to_string();
    }

    fn sum_clear(&mut self) {
        self.sum_total = Number::Int(0);
    }

    fn sum_add_under_cursor(&mut self) -> Result<(String, String), String> {
        let cleaned = self.number_under_cursor()?;
        let addend = Value::str(cleaned.as_str())
            .to_number()
            .map_err(|_| format!("not a number: `{cleaned}`"))?;
        self.sum_total = Number::add(&self.sum_total, &addend);
        let dp = self.float_decimals;
        let added = Value::from_number(addend).to_str_dp(dp);
        let total = Value::from_number(self.sum_total.clone()).to_str_dp(dp);
        Ok((added, total))
    }

    fn sum_put(&mut self) -> String {
        let total = Value::from_number(self.sum_total.clone()).to_str_dp(self.float_decimals);
        self.active().insert(&total);
        total
    }

    fn jump_to_offset(&mut self, offset: usize) -> VmResult<()> {
        let len = self.active().buffer().len();
        self.active().set_caret(offset.min(len), None);
        self.reveal_center = true; // scroll the target to mid-screen, like MARKNEXT/PREV
        self.set_focus(Target::Document);
        Ok(())
    }

    fn lint_fix_options(&mut self) -> Option<LintFixChoices> {
        #[cfg(feature = "lint")]
        {
            self.compute_lint_fix()
        }
        #[cfg(not(feature = "lint"))]
        {
            None
        }
    }

    fn apply_lint_fix(&mut self, span: (usize, usize), replacement: &str) -> VmResult<()> {
        let (start, end) = span;
        let doc = self.active();
        doc.set_caret(end, Some(start)); // select the issue span…
        doc.delete_selection(); // …remove it UNCONDITIONALLY (a plain `insert` keeps the selection
        doc.insert(replacement); // under persistent-selection mode, which appended the correction)…
        Ok(())
    }

    fn job_list(&self) -> Vec<String> {
        if self.jobs.is_empty() {
            return vec!["no background jobs".to_string()];
        }
        self.jobs.iter().map(|j| format!("job {}: {}", j.id, j.cmd)).collect()
    }

    fn highlight_kinds(&self) -> Vec<String> {
        self.ordered_highlight_kinds()
            .iter()
            .map(|k| match self.highlight_color(k) {
                Some((r, g, b)) => format!("{:<10} {r} {g} {b}", k.name()),
                None => format!("{:<10} (default text colour)", k.name()),
            })
            .collect()
    }

    fn highlight_kind_colors(&self) -> Vec<Option<(u8, u8, u8)>> {
        self.ordered_highlight_kinds().iter().map(|k| self.highlight_color(k)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::fenced_block_at;
    use super::reflow_text;

    #[test]
    fn reflow_wraps_words_to_width_without_splitting() {
        let out = reflow_text("one two three four five six", 11, "\n");
        // Greedy fill at 11 cols: "one two" (7), +"three"=13 > 11 → wrap.
        assert_eq!(out, "one two\nthree four\nfive six");
        for line in out.split('\n') {
            assert!(line.chars().count() <= 11, "line over width: {line:?}");
        }
    }

    #[test]
    fn reflow_collapses_existing_line_breaks_and_whitespace() {
        // Soft-broken lines + runs of spaces collapse to single spaces, then re-wrap.
        let out = reflow_text("alpha   beta\ngamma\n\ndelta epsilon", 12, "\n");
        assert_eq!(out, "alpha beta\ngamma\n\ndelta\nepsilon");
    }

    #[test]
    fn reflow_preserves_first_line_indent_on_every_line() {
        let out = reflow_text("    aaa bbb ccc ddd", 10, "\n");
        // 4-space indent → 6 cols of room; each wrapped line carries the indent.
        assert_eq!(out, "    aaa\n    bbb\n    ccc\n    ddd");
    }

    #[test]
    fn reflow_keeps_blank_lines_and_a_trailing_newline() {
        let out = reflow_text("aa bb\n\ncc dd\n", 80, "\n");
        assert_eq!(out, "aa bb\n\ncc dd\n"); // paragraphs fit; structure + trailing NL intact
    }

    #[test]
    fn reflow_overflows_a_word_longer_than_the_width() {
        let out = reflow_text("hi supercalifragilistic bye", 8, "\n");
        assert_eq!(out, "hi\nsupercalifragilistic\nbye"); // the long word sits alone, uncut
    }

    #[test]
    fn reflow_honors_crlf_join() {
        let out = reflow_text("one two three", 7, "\r\n");
        assert_eq!(out, "one two\r\nthree");
    }

    #[test]
    fn dupview_child_command_propagates_startup_and_boots_in_launch_dir() {
        // A DUPVIEW/W child must reproduce the parent's boot: same startup script (`-s`), same LAUNCH
        // directory (so relative config loads resolve), opening the file, then $CWD restored to where
        // the parent is now working.
        let mut h = super::AppHost::new();
        h.boot_cwd = std::path::PathBuf::from("/boot/dir");
        h.cwd = std::path::PathBuf::from("/work/dir");
        h.startup_path = Some(std::path::PathBuf::from("my-startup.xpl"));
        let (args, cwd) = h.dupview_child_command(std::path::Path::new("/files/doc.md"), 42);
        assert_eq!(cwd, std::path::PathBuf::from("/boot/dir"), "child boots in the parent's launch dir");
        let a: Vec<String> = args.iter().map(|x| x.to_string_lossy().into_owned()).collect();
        assert_eq!(a[0], "/files/doc.md", "opens the absolute file path");
        assert!(a.windows(2).any(|w| w[0] == "--goto" && w[1] == "42"), "at the requested line: {a:?}");
        assert!(a.windows(2).any(|w| w[0] == "--cwd" && w[1] == "/work/dir"), "restores current $CWD: {a:?}");
        assert!(a.windows(2).any(|w| w[0] == "-s" && w[1] == "my-startup.xpl"), "runs the same startup: {a:?}");
    }

    #[test]
    fn dupview_child_command_omits_dash_s_when_the_parent_had_no_startup() {
        let mut h = super::AppHost::new();
        h.startup_path = None;
        let (args, _) = h.dupview_child_command(std::path::Path::new("/f.md"), 1);
        let a: Vec<String> = args.iter().map(|x| x.to_string_lossy().into_owned()).collect();
        assert!(!a.iter().any(|x| x == "-s"), "no -s when there was no startup script: {a:?}");
    }

    #[test]
    fn url_trim_drops_trailing_punctuation_but_keeps_balanced_parens() {
        let len = |s: &str| super::trim_url_len(s.as_bytes());
        assert_eq!(&"http://x.org."[..len("http://x.org.")], "http://x.org");
        assert_eq!(&"http://x.org,"[..len("http://x.org,")], "http://x.org");
        assert_eq!(&"http://x.org).."[..len("http://x.org)..")], "http://x.org");
        // A balanced trailing paren is part of the URL (Wikipedia-style).
        let wiki = "http://en.wikipedia.org/wiki/Rust_(programming_language)";
        assert_eq!(&wiki[..len(wiki)], wiki);
        // An unbalanced trailing paren is dropped (the URL was inside parentheses).
        assert_eq!(&"http://x.org/a)"[..len("http://x.org/a)")], "http://x.org/a");
    }

    #[test]
    fn url_normalize_gates_schemes_and_prepends_https_to_www() {
        assert_eq!(super::normalize_url("https://x.org").as_deref(), Some("https://x.org"));
        assert_eq!(super::normalize_url("HTTP://x.org").as_deref(), Some("HTTP://x.org"));
        assert_eq!(super::normalize_url("mailto:a@b.com").as_deref(), Some("mailto:a@b.com"));
        assert_eq!(super::normalize_url("www.x.org").as_deref(), Some("https://www.x.org"));
        // Unknown / dangerous schemes are not openable.
        assert_eq!(super::normalize_url("file:///etc/passwd"), None);
        assert_eq!(super::normalize_url("javascript:alert(1)"), None);
        assert_eq!(super::normalize_url("plain text"), None);
    }

    #[test]
    fn cloud_guard_passes_a_normal_local_file() {
        // No false positives: an ordinary local file (no OFFLINE/RECALL attr) is never tripped, so
        // the read proceeds as usual.
        let p = std::env::temp_dir().join(format!("xy_cloudguard_{}.txt", std::process::id()));
        std::fs::write(&p, b"hello").unwrap();
        assert!(super::cloud_only_guard(&p).is_ok());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    #[cfg(feature = "email")]
    fn rfc5322_utc_formats_a_known_date() {
        // 1609459200 = 2021-01-01 00:00:00 UTC (a Friday).
        assert_eq!(super::rfc5322_utc(1_609_459_200), "Fri, 01 Jan 2021 00:00:00 +0000");
    }

    #[test]
    #[cfg(feature = "email")]
    fn mail_message_from_args() {
        let (rcpt, msg) = super::AppHost::build_mail_message(
            "me@example.com",
            Some("alice@example.com".to_string()),
            Some("Hi there".to_string()),
            "Line one\nLine two",
            "Fri, 01 Jan 2021 00:00:00 +0000",
            "<abc@example.com>",
            &[],
            "BOUNDARY",
        )
        .unwrap();
        assert_eq!(rcpt, vec!["alice@example.com"]);
        assert!(msg.contains("Date: Fri, 01 Jan 2021 00:00:00 +0000\r\n"));
        assert!(msg.contains("Message-ID: <abc@example.com>\r\n"));
        assert!(msg.contains("X-Mailer: XyScript\r\n"));
        assert!(msg.contains("Content-Transfer-Encoding: 7bit\r\n"), "ASCII body → 7bit");
        assert!(msg.contains("From: me@example.com\r\n"));
        assert!(msg.contains("To: alice@example.com\r\n"));
        assert!(msg.contains("Subject: Hi there\r\n"));
        assert!(msg.contains("\r\n\r\nLine one\r\nLine two"), "CRLF body after a blank line: {msg:?}");
    }

    #[test]
    #[cfg(feature = "email")]
    fn mail_message_from_header_block() {
        let doc = "To: alice@example.com, bob@example.com\nCc: carol@example.com\nBcc: dave@example.com\nSubject: Meeting\n\nBody: see agenda below.\nSecond line.";
        let (rcpt, msg) = super::AppHost::build_mail_message(
            "me@example.com",
            None,
            None,
            doc,
            "Fri, 01 Jan 2021 00:00:00 +0000",
            "<abc@example.com>",
            &[],
            "BOUNDARY",
        )
        .unwrap();
        // Recipients = To + Cc + Bcc; Bcc must NOT appear in the visible headers.
        assert_eq!(
            rcpt,
            vec!["alice@example.com", "bob@example.com", "carol@example.com", "dave@example.com"]
        );
        assert!(msg.contains("To: alice@example.com, bob@example.com\r\n"));
        assert!(msg.contains("Cc: carol@example.com\r\n"));
        assert!(!msg.to_lowercase().contains("bcc"), "Bcc leaked into the message: {msg:?}");
        assert!(msg.contains("Subject: Meeting\r\n"));
        // The `Body:` line has a colon but isn't a known header → it's body, not a header.
        assert!(msg.contains("\r\n\r\nBody: see agenda below.\r\nSecond line."), "{msg:?}");
    }

    #[test]
    #[cfg(feature = "email")]
    fn mail_display_name_addresses_split_envelope_from_headers() {
        // `To:`/`Cc:` in display-name form: the SMTP envelope (--mail-rcpt) must get the BARE address,
        // while the visible header keeps the full `"Name" <addr>` form. A comma inside a quoted name
        // must NOT split the address.
        let doc = "To: \"Public, John\" <john@everyman.org>, Jane Roe <jane@example.com>\nCc: <bob@x.org>\nSubject: Hi\n\nHello.";
        let (rcpt, msg) = super::AppHost::build_mail_message(
            "me@example.com",
            None,
            None,
            doc,
            "Fri, 01 Jan 2021 00:00:00 +0000",
            "<abc@example.com>",
            &[],
            "BOUNDARY",
        )
        .unwrap();
        // Envelope = bare addresses only.
        assert_eq!(rcpt, vec!["john@everyman.org", "jane@example.com", "bob@x.org"]);
        // Visible headers keep the display-name forms verbatim (the quoted comma didn't split them).
        assert!(
            msg.contains(
                "To: \"Public, John\" <john@everyman.org>, Jane Roe <jane@example.com>\r\n"
            ),
            "{msg:?}"
        );
        assert!(msg.contains("Cc: <bob@x.org>\r\n"), "{msg:?}");
    }

    #[test]
    #[cfg(feature = "email")]
    fn mail_from_display_name_kept_in_header_bare_in_envelope() {
        // envelope_addr extracts the bare address for the SMTP envelope (--mail-from / --mail-rcpt)…
        assert_eq!(
            super::envelope_addr("\"Carl Distefano\" <cldistefano@att.net>"),
            "cldistefano@att.net"
        );
        assert_eq!(super::envelope_addr("cldistefano@att.net"), "cldistefano@att.net");
        assert_eq!(super::envelope_addr("Carl <c@x.org>"), "c@x.org");
        // …while the visible From: header keeps the full display-name form.
        let (_rcpt, msg) = super::AppHost::build_mail_message(
            "\"Carl Distefano\" <cldistefano@att.net>",
            Some("bob@example.com".to_string()),
            None,
            "hi",
            "Fri, 01 Jan 2021 00:00:00 +0000",
            "<abc@example.com>",
            &[],
            "BOUNDARY",
        )
        .unwrap();
        assert!(msg.contains("From: \"Carl Distefano\" <cldistefano@att.net>\r\n"), "{msg:?}");
    }

    #[test]
    #[cfg(feature = "email")]
    fn mail_custom_headers_pass_through_with_guardrails() {
        let doc = "To: alice@example.com\nReply-To: bob@example.com\nIn-Reply-To: <prev@example.com>\nX-Priority: 1\nDate: Wed, 02 Feb 2022 03:04:05 +0000\nMessage-ID: <mine@example.com>\nContent-Type: text/evil\nMIME-Version: 9.9\nAttach: /tmp/a.pdf\n\nBody here.";
        let (rcpt, msg) = super::AppHost::build_mail_message(
            "me@example.com",
            None,
            None,
            doc,
            "Fri, 01 Jan 2021 00:00:00 +0000", // our generated Date — should be overridden
            "<generated@example.com>",         // our generated Message-ID — should be overridden
            &[],
            "BOUNDARY",
        )
        .unwrap();
        assert_eq!(rcpt, vec!["alice@example.com"]);
        // Custom headers pass through, in order.
        assert!(msg.contains("Reply-To: bob@example.com\r\n"), "{msg:?}");
        assert!(msg.contains("In-Reply-To: <prev@example.com>\r\n"), "{msg:?}");
        assert!(msg.contains("X-Priority: 1\r\n"), "{msg:?}");
        // Guardrail: Date / Message-ID are the USER's, and ours are NOT also emitted.
        assert!(msg.contains("Date: Wed, 02 Feb 2022 03:04:05 +0000\r\n"), "{msg:?}");
        assert!(!msg.contains("Fri, 01 Jan 2021"), "our Date leaked: {msg:?}");
        assert!(msg.contains("Message-ID: <mine@example.com>\r\n"), "{msg:?}");
        assert!(!msg.contains("<generated@example.com>"), "our Message-ID leaked: {msg:?}");
        // Guardrail: structural MIME headers are never user-set — exactly one (ours), no bogus values.
        assert!(!msg.contains("text/evil"), "user Content-Type leaked: {msg:?}");
        assert!(!msg.contains("MIME-Version: 9.9"), "user MIME-Version leaked: {msg:?}");
        assert_eq!(msg.matches("Content-Type:").count(), 1, "duplicate Content-Type: {msg:?}");
        assert!(msg.contains("Content-Type: text/plain; charset=utf-8\r\n"), "{msg:?}");
        // The Attach line (after custom headers) is still consumed, not body — and found for I/O.
        assert_eq!(super::header_attach_paths(doc), vec!["/tmp/a.pdf"]);
        assert!(msg.contains("\r\n\r\nBody here."), "body wrong: {msg:?}");
        assert!(!msg.contains("Attach:"), "Attach leaked into body: {msg:?}");
    }

    #[test]
    #[cfg(feature = "email")]
    fn mail_header_block_ends_at_a_non_field_line() {
        // A "name" containing a space isn't a valid field → the body starts there (no header eaten).
        let doc = "To: a@x.com\nDear Bob: hello\nmore body";
        let (_r, msg) = super::AppHost::build_mail_message(
            "me@x.com",
            None,
            None,
            doc,
            "Fri, 01 Jan 2021 00:00:00 +0000",
            "<id@x.com>",
            &[],
            "B",
        )
        .unwrap();
        assert!(msg.contains("\r\n\r\nDear Bob: hello\r\nmore body"), "{msg:?}");
    }

    #[test]
    #[cfg(feature = "email")]
    fn header_block_attach_lines_are_parsed_not_body() {
        let doc = "To: alice@example.com\nSubject: Files\nAttach: C:/docs/a.pdf\nAttachment: /tmp/b.png\n\nSee attached.\nNote: colon in body is fine.";
        // `header_attach_paths` picks up both spellings, in order.
        assert_eq!(super::header_attach_paths(doc), vec!["C:/docs/a.pdf", "/tmp/b.png"]);
        // And the Attach lines are consumed as headers — the body starts at the real text.
        let (rcpt, msg) = super::AppHost::build_mail_message(
            "me@example.com",
            None,
            None,
            doc,
            "Fri, 01 Jan 2021 00:00:00 +0000",
            "<abc@example.com>",
            &[],
            "BOUNDARY",
        )
        .unwrap();
        assert_eq!(rcpt, vec!["alice@example.com"]);
        assert!(msg.contains("\r\n\r\nSee attached.\r\nNote: colon in body is fine."), "{msg:?}");
        assert!(!msg.contains("Attach:"), "Attach header must not leak into the message body: {msg:?}");
    }

    #[test]
    #[cfg(feature = "email")]
    fn header_attach_paths_ignores_args_mode_and_body() {
        // No header block (a body line with a colon) → no attach paths, no false positives.
        assert!(super::header_attach_paths("Just a note: hello\nAttach: late.pdf").is_empty());
    }

    #[test]
    #[cfg(feature = "email")]
    fn mail_message_requires_a_recipient() {
        let err = super::AppHost::build_mail_message(
            "me@example.com",
            None,
            None,
            "just a body",
            "Fri, 01 Jan 2021 00:00:00 +0000",
            "<abc@example.com>",
            &[],
            "BOUNDARY",
        )
        .unwrap_err();
        assert!(err.contains("no recipient"), "{err}");
    }

    #[test]
    #[cfg(feature = "email")]
    fn base64_encode_matches_known_vectors() {
        // RFC 4648 test vectors (incl. the padding cases).
        assert_eq!(super::base64_encode(b""), "");
        assert_eq!(super::base64_encode(b"f"), "Zg==");
        assert_eq!(super::base64_encode(b"fo"), "Zm8=");
        assert_eq!(super::base64_encode(b"foo"), "Zm9v");
        assert_eq!(super::base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(super::base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(super::base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    #[cfg(feature = "email")]
    fn guess_mime_covers_common_types() {
        assert_eq!(super::guess_mime("report.PDF"), "application/pdf");
        assert_eq!(super::guess_mime("photo.jpeg"), "image/jpeg");
        assert_eq!(super::guess_mime("archive.tar.gz"), "application/gzip");
        assert_eq!(super::guess_mime("mystery"), "application/octet-stream");
    }

    #[test]
    #[cfg(feature = "email")]
    fn mail_message_with_attachment_is_multipart() {
        let att = [super::MailAttachment {
            filename: "report.pdf".into(),
            mime: "application/pdf",
            b64: super::wrap76(&super::base64_encode(b"%PDF-1.7 hello")),
        }];
        let (_rcpt, msg) = super::AppHost::build_mail_message(
            "me@example.com",
            Some("alice@example.com".to_string()),
            Some("Docs".to_string()),
            "See attached.",
            "Fri, 01 Jan 2021 00:00:00 +0000",
            "<abc@example.com>",
            &att,
            "=_XyScript_BOUND_=",
        )
        .unwrap();
        assert!(msg.contains("Content-Type: multipart/mixed; boundary=\"=_XyScript_BOUND_=\"\r\n"));
        assert!(msg.contains("--=_XyScript_BOUND_=\r\n")); // part boundary
        assert!(msg.contains("\r\n\r\nSee attached.\r\n"), "text part carries the body");
        assert!(msg.contains("Content-Type: application/pdf; name=\"report.pdf\"\r\n"));
        assert!(msg.contains("Content-Transfer-Encoding: base64\r\n"));
        assert!(msg.contains("Content-Disposition: attachment; filename=\"report.pdf\"\r\n"));
        assert!(msg.contains(&super::base64_encode(b"%PDF-1.7 hello")));
        assert!(msg.trim_end().ends_with("--=_XyScript_BOUND_=--"), "closing boundary: {msg:?}");
    }

    #[test]
    #[cfg(feature = "email")]
    fn mail_range_parsing_for_save_and_delete() {
        // SAVEMAIL: expand to individual numbers, sorted + de-duped.
        assert_eq!(super::parse_range("1-3,6-8,10,12").unwrap(), vec![1, 2, 3, 6, 7, 8, 10, 12]);
        assert_eq!(super::parse_range("5,5,3-4").unwrap(), vec![3, 4, 5]);
        assert!(super::parse_range("").is_err());
        assert!(super::parse_range("3-1").is_err()); // reversed
        assert!(super::parse_range("all").is_err()); // SAVEMAIL needs explicit numbers
        assert!(super::parse_range("x").is_err());
        // DELMAIL: compact IMAP sequence set (no expansion); `all` → 1:*.
        assert_eq!(super::range_to_imap_seqset("1-3,6-8,10").unwrap(), "1:3,6:8,10");
        assert_eq!(super::range_to_imap_seqset("ALL").unwrap(), "1:*");
        assert!(super::range_to_imap_seqset("2-1").is_err());
    }

    #[test]
    #[cfg(feature = "email")]
    fn mail_headers_parse_curl_verbose_trace() {
        // A real `curl -v` IMAP FETCH trace (incoming lines marked `< `), two messages, one folded
        // Subject continuation.
        let raw = "\
* OK ready\r
< * 1 FETCH (FLAGS (\\Seen) BODY[HEADER.FIELDS (DATE FROM SUBJECT)] {80}\r
< Date: Mon, 13 Jul 2026 07:26:07 +0000 (UTC)\r
< From: Carl Distefano <cldistefano@att.net>\r
< Subject: Fw: Crash with five\r
<  injured in New York\r
< )\r
< * 2 FETCH (FLAGS () BODY[HEADER.FIELDS (DATE FROM SUBJECT)] {40}\r
< From: Someone Else <a@b.com>\r
< Subject: Hello\r
< A001 OK FETCH completed\r
";
        let out = super::format_mail_headers(raw, "INBOX");
        assert!(out.contains("2 message(s) in INBOX"));
        assert!(out.contains("[1] From: Carl Distefano <cldistefano@att.net>"));
        assert!(out.contains("Date: Mon, 13 Jul 2026 07:26:07 +0000 (UTC)"));
        // folded Subject re-joined:
        assert!(out.contains("Subj: Fw: Crash with five injured in New York"));
        assert!(out.contains("[2] From: Someone Else <a@b.com>"));
        assert!(out.contains("Subj: Hello"));
        // empty trace → friendly note, no panic:
        assert!(super::format_mail_headers("", "INBOX").contains("No messages in INBOX"));
        // message count for SAVEMAIL all = highest `* N FETCH` marker:
        assert_eq!(super::max_fetch_seq(raw), 2);
        assert_eq!(super::max_fetch_seq(""), 0);
        assert_eq!(super::max_fetch_seq("< * 7 FETCH (FLAGS ())\r\n< * 3 FETCH (FLAGS ())\r"), 7);
    }

    #[test]
    #[cfg(feature = "email")]
    fn imap_rejection_detects_tagged_no_or_bad() {
        // A refused STORE — curl exits 0, but the tagged response is NO. Detected + returned.
        let refused = "\
< A001 OK LOGIN completed\r
< A002 OK SELECT completed\r
< A003 NO [CANNOT] permission denied\r
";
        let hit = super::imap_rejection(refused).expect("a NO response is a rejection");
        assert!(hit.contains("A003 NO"), "returns the offending line: {hit}");
        // BAD is also a rejection.
        assert!(super::imap_rejection("< A007 BAD syntax error\r").is_some());
        // All-OK trace → no rejection.
        assert!(super::imap_rejection("< A001 OK\r\n< A002 OK done\r").is_none());
        // No false positive when "NO"/"BAD" appears in fetched header TEXT (not a tagged response).
        assert!(super::imap_rejection("< Subject: NO refunds, BAD idea\r\n< A001 OK\r").is_none());
        // Untagged responses (`*`) are progress, not the command verdict — ignored.
        assert!(super::imap_rejection("< * NO some untagged notice\r\n< A001 OK\r").is_none());
        assert!(super::imap_rejection("").is_none());
    }

    #[test]
    #[cfg(feature = "email")]
    fn mail_config_sets_and_masks_the_password() {
        use xyscript_xpl::{Host, Value};
        let mut h = super::AppHost::new();
        for (f, v) in [("url", "smtps://smtp.example.com:465"), ("user", "me@example.com"), ("pass", "s3cret")] {
            h.editor_op(super::EditorOp::MailSet { field: f.into(), value: v.into() }).unwrap();
        }
        assert_eq!(h.special("SMTPURL"), Some(Value::str("smtps://smtp.example.com:465")));
        assert_eq!(h.special("MAILUSER"), Some(Value::str("me@example.com")));
        // The password is never echoed — the special reports only whether one is set.
        assert_eq!(h.special("MAILPASS"), Some(Value::str("set")));
    }

    #[test]
    #[cfg(windows)]
    fn drive_qualified_paths_switch_drive() {
        use super::normalize_drive_path;
        use std::path::PathBuf;
        // Bare drive and drive-relative forms switch to that drive (CD == CD/D).
        assert_eq!(normalize_drive_path("D:"), Some(PathBuf::from(r"D:\")));
        assert_eq!(normalize_drive_path("D:work"), Some(PathBuf::from(r"D:\work")));
        // Already-rooted or non-drive paths are left to the normal absolute/relative handling.
        assert_eq!(normalize_drive_path(r"D:\work"), None);
        assert_eq!(normalize_drive_path("D:/work"), None);
        assert_eq!(normalize_drive_path("notes.txt"), None);
        assert_eq!(normalize_drive_path(r"\\server\share"), None);
    }

    #[test]
    fn bare_drive_letter_only_matches_a_lone_drive() {
        use super::{bare_drive_letter, drive_of};
        use std::path::Path;
        // Only `X:` (letter + colon, nothing after) is a "remembered-dir" drive switch.
        assert_eq!(bare_drive_letter("d:"), Some('D'));
        assert_eq!(bare_drive_letter("  X:  "), Some('X')); // trimmed
        // Explicit root / drive-relative / other paths are NOT bare (they resolve normally).
        assert_eq!(bare_drive_letter(r"D:\"), None);
        assert_eq!(bare_drive_letter(r"D:\sub"), None);
        assert_eq!(bare_drive_letter("D:rel"), None);
        assert_eq!(bare_drive_letter("PR"), None);
        // Drive extraction for the per-drive memory (upper-cased; None for UNC).
        assert_eq!(drive_of(Path::new(r"x:\proj\src")), Some('X'));
        assert_eq!(drive_of(Path::new(r"\\server\share\x")), None);
    }

    #[test]
    fn bufalt_toggles_between_current_and_alternate() {
        use super::{AppHost, Document, EditorOp};
        use xyscript_xpl::Host;
        let mut h = AppHost::new();
        h.docs.push(Document::new());
        h.docs.push(Document::new()); // 3 buffers (0,1,2); start on 0
        assert_eq!(h.active, 0);
        // Switch to buffer 3 (index 2): the alternate becomes the buffer we left (0).
        h.editor_op(EditorOp::BufGo("3".into())).unwrap();
        assert_eq!(h.active, 2);
        assert_eq!(h.alt_active, Some(0));
        // BUFALT flips to the alternate (0) and now remembers 2.
        h.editor_op(EditorOp::BufAlt).unwrap();
        assert_eq!((h.active, h.alt_active), (0, Some(2)));
        // BUFALT again flips straight back to 2 — a two-tab toggle.
        h.editor_op(EditorOp::BufAlt).unwrap();
        assert_eq!((h.active, h.alt_active), (2, Some(0)));
    }

    #[test]
    fn bufalt_seeds_from_bufprev_and_close_clears_the_alternate() {
        use super::{AppHost, Document, EditorOp};
        use xyscript_xpl::Host;
        let mut h = AppHost::new();
        h.docs.push(Document::new());
        h.docs.push(Document::new()); // 3 buffers; on 0, no alternate yet
        assert_eq!(h.alt_active, None);
        // With no stored alternate, the first BUFALT acts like BUFPREV: 0 -> 2 (wrap).
        h.editor_op(EditorOp::BufAlt).unwrap();
        assert_eq!(h.active, 2);
        // A BUFNEXT records its own alternate, which a close must invalidate.
        h.editor_op(EditorOp::BufNext).unwrap(); // 2 -> 0, alt = 2
        assert_eq!(h.alt_active, Some(2));
        h.editor_op(EditorOp::CloseBuffer).unwrap();
        assert_eq!(h.alt_active, None);
    }

    #[test]
    fn pane_split_new_makes_two_panes_focused_on_the_fresh_buffer() {
        use super::{AppHost, EditorOp};
        use xyscript_xpl::Host;
        let mut h = AppHost::new();
        assert_eq!(h.pane_count(), 1);
        let before = h.docs.len();
        h.editor_op(EditorOp::PaneSplitNew { stacked: false }).unwrap(); // NEW/V
        assert_eq!(h.pane_count(), 2);
        assert_eq!(h.docs.len(), before + 1); // a fresh empty buffer was opened
        // The new (empty) pane holds focus, and `active` follows it.
        assert_eq!(h.panes().focused_doc(), h.docs[h.active].id());
        assert_eq!(h.active, h.docs.len() - 1);
    }

    #[test]
    fn ths_tiles_the_alternate_buffer_and_respects_the_one_pane_per_doc_guard() {
        use super::{AppHost, Document, EditorOp};
        use xyscript_xpl::Host;
        let mut h = AppHost::new();
        h.docs.push(Document::new()); // two tabs (0, 1)
        h.editor_op(EditorOp::BufGo("2".into())).unwrap(); // active=1, alt_active=0
        let n = h.docs.len();
        // THS with no arg tiles the alternate buffer (0) — no new buffer created.
        h.editor_op(EditorOp::PaneSplitTile { stacked: true, target: None }).unwrap();
        assert_eq!(h.pane_count(), 2);
        assert_eq!(h.docs.len(), n); // reused the alternate, didn't open one
        // Both distinct buffers are shown; the guard forbids showing either twice.
        assert!(h.panes().contains_doc(h.docs[0].id()));
        assert!(h.panes().contains_doc(h.docs[1].id()));
    }

    #[test]
    fn session_reff_preserves_subdir_for_mixed_separator_open() {
        use super::AppHost;
        let base = std::env::temp_dir().join(format!("xyscript-reff-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        let sub = base.join("xys");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("lines.txt"), b"hello\nworld\n").unwrap();
        let mut h = AppHost::new();
        // Mixed separators, like OPENALL's old `$aspec[0]/$fnm`: <sub>/lines.txt where <sub> itself
        // uses the OS separator (backslash on Windows) → "...\xys/lines.txt".
        let mixed = format!("{}/lines.txt", sub.display());
        h.open_doc(&mixed, false, false).unwrap();
        assert!(h.docs[h.active].buffer().len() > 0, "content loaded from the mixed-separator path");
        let sdir = base.join("session");
        h.save_session(&sdir).unwrap();
        let manifest = std::fs::read_to_string(sdir.join("manifest.txt")).unwrap();
        let reff_line = manifest.lines().find(|l| l.starts_with("file")).expect("a file tab line");
        // The session must record a path that still points at the real file — i.e. it keeps the `xys`
        // subdir. The bug dropped it (reff became "X:/lines.txt"), so restore reopened a nonexistent
        // path and produced an empty buffer.
        assert!(
            reff_line.contains("xys"),
            "session reff must keep the subdir, got: {reff_line}"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn tw_cycles_focus_and_tus_collapses() {
        use super::{AppHost, EditorOp};
        use xyscript_xpl::Host;
        let mut h = AppHost::new();
        let orig = h.docs[0].id();
        h.editor_op(EditorOp::PaneSplitNew { stacked: false }).unwrap(); // focus the new pane
        let new_doc = h.panes().focused_doc();
        assert_ne!(new_doc, orig);
        // TW wraps focus back to the original buffer's pane.
        h.editor_op(EditorOp::PaneFocusNext).unwrap();
        assert_eq!(h.panes().focused_doc(), orig);
        assert_eq!(h.docs[h.active].id(), orig); // `active` follows the focus
        // TUS collapses back to a single pane; the collapsed buffer stays open as a tab.
        h.editor_op(EditorOp::PaneCollapse).unwrap();
        assert_eq!(h.pane_count(), 1);
        assert_eq!(h.docs.len(), 2); // both buffers still open
        // A second TUS is an error (only one pane left).
        assert!(h.editor_op(EditorOp::PaneCollapse).is_err());
    }

    #[test]
    fn ab_closes_the_file_and_undoes_its_pane_in_one_op() {
        use super::{AppHost, EditorOp};
        use xyscript_xpl::Host;
        let mut h = AppHost::new();
        let orig = h.docs[0].id();
        h.editor_op(EditorOp::PaneSplitNew { stacked: true }).unwrap(); // 2 panes, 2 buffers (new focused)
        assert_eq!(h.pane_count(), 2);
        assert_eq!(h.docs.len(), 2);
        // A single AB closes the focused file AND removes its pane — both in one operation.
        h.editor_op(EditorOp::CloseBuffer).unwrap();
        assert_eq!(h.pane_count(), 1); // pane undone
        assert_eq!(h.docs.len(), 1); // buffer closed
        assert_eq!(h.docs[h.active].id(), orig); // back to the original, full-window
        assert_eq!(h.panes().focused_doc(), orig);
    }

    #[test]
    fn sw_h_v_show_shell_output_in_a_split_pane() {
        use super::{AppHost, EditorOp, SHELL_OUTPUT_NAME};
        use xyscript_xpl::Host;
        let mut h = AppHost::new();
        assert_eq!(h.pane_count(), 1);
        // SW/V splits and shows the *Shell Output* buffer in the new focused pane.
        h.editor_op(EditorOp::PaneShell { stacked: false }).unwrap();
        assert_eq!(h.pane_count(), 2);
        assert!(h.shell_output_visible());
        assert_eq!(h.docs[h.active].label(), Some(SHELL_OUTPUT_NAME)); // focused pane = SO buffer
        let so_id = h.docs[h.active].id();
        // A second SW/V doesn't add a pane — it just focuses the existing SO pane (guard: ≤1 per doc).
        h.editor_op(EditorOp::PaneFocusNext).unwrap(); // move focus away first
        assert_ne!(h.panes().focused_doc(), so_id);
        h.editor_op(EditorOp::PaneShell { stacked: true }).unwrap();
        assert_eq!(h.pane_count(), 2); // no new pane
        assert_eq!(h.panes().focused_doc(), so_id); // focus jumped back to the SO pane
    }

    #[test]
    fn fs_releases_the_focused_pane_to_full_screen() {
        use super::{AppHost, EditorOp};
        use xyscript_xpl::Host;
        let mut h = AppHost::new();
        let orig = h.docs[0].id();
        // Build a 3-pane layout: split twice.
        h.editor_op(EditorOp::PaneSplitNew { stacked: false }).unwrap(); // pane B (focused)
        h.editor_op(EditorOp::PaneSplitNew { stacked: true }).unwrap(); // pane C (focused), nested
        assert_eq!(h.pane_count(), 3);
        let focused = h.panes().focused_doc();
        // FS collapses everything down to the FOCUSED pane — not its sibling.
        h.editor_op(EditorOp::PaneFullscreen).unwrap();
        assert_eq!(h.pane_count(), 1);
        assert_eq!(h.panes().focused_doc(), focused); // the pane we were in survived
        assert_ne!(focused, orig);
        // All buffers are still open as tabs (nothing was closed).
        assert_eq!(h.docs.len(), 3);
        // FS is idempotent when already single-pane (no error).
        assert!(h.editor_op(EditorOp::PaneFullscreen).is_ok());
        assert_eq!(h.pane_count(), 1);
    }

    #[test]
    fn tz_zoom_toggles_and_trv_swaps() {
        use super::{AppHost, EditorOp};
        use xyscript_xpl::Host;
        let mut h = AppHost::new();
        h.editor_op(EditorOp::PaneSplitNew { stacked: false }).unwrap();
        // Zoom on, then off.
        h.editor_op(EditorOp::PaneZoom).unwrap();
        assert!(h.panes().is_zoomed());
        h.editor_op(EditorOp::PaneZoom).unwrap();
        assert!(!h.panes().is_zoomed());
        // Swap trades the two panes' positions; focus (and active) stay put.
        let focus_doc = h.panes().focused_doc();
        h.editor_op(EditorOp::PaneSwap).unwrap();
        assert_eq!(h.panes().focused_doc(), focus_doc);
        assert_eq!(h.pane_count(), 2);
    }

    #[test]
    fn pane_specials_report_count_and_position() {
        use super::{AppHost, EditorOp};
        use xyscript_xpl::{Host, Value};
        let mut h = AppHost::new();
        assert_eq!(h.special("PANES"), Some(Value::Int(1)));
        assert_eq!(h.special("PANE"), Some(Value::Int(1)));
        h.editor_op(EditorOp::PaneSplitNew { stacked: false }).unwrap();
        assert_eq!(h.special("PANES"), Some(Value::Int(2)));
        // The new pane is on the FIRST side (left), so its position is 1.
        assert_eq!(h.special("PANE"), Some(Value::Int(1)));
        h.editor_op(EditorOp::PaneFocusNext).unwrap(); // focus the other (right) pane → position 2
        assert_eq!(h.special("PANE"), Some(Value::Int(2)));
    }

    #[test]
    fn pv_margins_set_per_side_normalize_units_and_read_back() {
        use super::{AppHost, EditorOp, PvMarginSide};
        use xyscript_xpl::{Host, Value};
        let mut h = AppHost::new();
        // Default 1 inch all around → `$PVMARGIN` collapses to the uniform value.
        assert_eq!(h.special("PVMARGIN"), Some(Value::str("1in")));
        // `PVMARGIN 2.5cm` sets all four; bare number = inches on a per-side set.
        h.editor_op(EditorOp::SetPvMargin { side: PvMarginSide::All, value: "2.5cm".into() }).unwrap();
        assert_eq!(h.special("PVMARGIN"), Some(Value::str("2.5cm")));
        h.editor_op(EditorOp::SetPvMargin { side: PvMarginSide::Top, value: "1".into() }).unwrap();
        assert_eq!(h.special("PVMARGINT"), Some(Value::str("1in"))); // bare → inches
        // Now the sides differ → `$PVMARGIN` reports all four (L R T B).
        assert_eq!(h.special("PVMARGIN"), Some(Value::str("2.5cm 2.5cm 1in 2.5cm")));
        // The render config carries the normalized strings.
        let m = h.preview_fonts().margins;
        assert_eq!((m.left.as_str(), m.top.as_str()), ("2.5cm", "1in"));
        // A bad unit is a trappable error and leaves the value unchanged.
        assert!(h.editor_op(EditorOp::SetPvMargin { side: PvMarginSide::Left, value: "3 furlongs".into() }).is_err());
        assert_eq!(h.special("PVMARGINL"), Some(Value::str("2.5cm")));
    }

    #[test]
    fn pv_paper_spacing_and_page_numbers_set_read_and_feed_the_render() {
        use super::{AppHost, EditorOp};
        use xyscript_xpl::{Host, Value};
        let mut h = AppHost::new();
        // Defaults.
        assert_eq!(h.special("PVPAPER"), Some(Value::str("letter")));
        assert_eq!(h.special("PVLEADING"), Some(Value::str(""))); // unset = Typst default
        assert_eq!(h.special("PVPAGENUM"), Some(Value::str(""))); // off
        assert_eq!(h.special("PVPAGEPOS"), Some(Value::str("bottom-center")));
        // Paper canonicalizes ("A4" → "a4"); a bad one is trappable and leaves it unchanged.
        h.editor_op(EditorOp::SetPreviewPaper("Legal".into())).unwrap();
        assert_eq!(h.special("PVPAPER"), Some(Value::str("legal")));
        assert!(h.editor_op(EditorOp::SetPreviewPaper("foolscap".into())).is_err());
        assert_eq!(h.special("PVPAPER"), Some(Value::str("legal")));
        // Spacing: bare number → em; "default" clears.
        h.editor_op(EditorOp::SetPreviewLeading("1.5".into())).unwrap();
        assert_eq!(h.special("PVLEADING"), Some(Value::str("1.5em")));
        h.editor_op(EditorOp::SetPreviewSpacing("2em".into())).unwrap();
        assert_eq!(h.special("PVSPACING"), Some(Value::str("2em")));
        h.editor_op(EditorOp::SetPreviewLeading("default".into())).unwrap();
        assert_eq!(h.special("PVLEADING"), Some(Value::str("")));
        // Page numbers: pattern (quotes stripped) + position; "off" clears.
        h.editor_op(EditorOp::SetPreviewPageNumber("\"Page 1\"".into())).unwrap();
        assert_eq!(h.special("PVPAGENUM"), Some(Value::str("Page 1")));
        h.editor_op(EditorOp::SetPreviewPageAlign("br".into())).unwrap();
        assert_eq!(h.special("PVPAGEPOS"), Some(Value::str("bottom-right")));
        // The render config carries the choices.
        let f = h.preview_fonts();
        assert_eq!(f.paper, "legal");
        assert_eq!(f.spacing.as_deref(), Some("2em"));
        assert_eq!(f.page_num.as_deref(), Some("Page 1"));
        assert_eq!(f.page_align, "bottom-right");
        h.editor_op(EditorOp::SetPreviewPageNumber("off".into())).unwrap();
        assert_eq!(h.special("PVPAGENUM"), Some(Value::str("")));
    }

    #[test]
    fn pv_justify_indent_header_footer_set_read_and_feed_the_render() {
        use super::{AppHost, EditorOp};
        use xyscript_xpl::{Host, Value};
        let mut h = AppHost::new();
        // Defaults: justify on, no indent, no header/footer.
        assert_eq!(h.special("PVJUSTIFY"), Some(Value::Int(1)));
        assert_eq!(h.special("PVINDENT"), Some(Value::str("")));
        assert_eq!(h.special("PVHEADER"), Some(Value::str("")));
        h.editor_op(EditorOp::SetPreviewJustify(false)).unwrap();
        assert_eq!(h.special("PVJUSTIFY"), Some(Value::Int(0)));
        h.editor_op(EditorOp::SetPreviewIndent("2".into())).unwrap(); // bare → em
        assert_eq!(h.special("PVINDENT"), Some(Value::str("2em")));
        h.editor_op(EditorOp::SetPreviewIndent("off".into())).unwrap();
        assert_eq!(h.special("PVINDENT"), Some(Value::str("")));
        h.editor_op(EditorOp::SetPreviewHeader("\"{file} | {date}\"".into())).unwrap();
        assert_eq!(h.special("PVHEADER"), Some(Value::str("{file} | {date}"))); // quotes stripped
        h.editor_op(EditorOp::SetPreviewFooter("|{page}|".into())).unwrap();
        // The render config carries the choices.
        let f = h.preview_fonts();
        assert!(!f.justify);
        assert_eq!(f.header.as_deref(), Some("{file} | {date}"));
        assert_eq!(f.footer.as_deref(), Some("|{page}|"));
        h.editor_op(EditorOp::SetPreviewHeader("off".into())).unwrap();
        assert_eq!(h.special("PVHEADER"), Some(Value::str("")));
    }

    #[test]
    fn spell_and_grammar_underline_styles_set_and_read_back() {
        use super::{AppHost, EditorOp, UnderlineStyle};
        use xyscript_xpl::{Host, Value};
        let mut h = AppHost::new();
        // Default is dotted for both.
        assert_eq!(h.special("SPELLSTYLE"), Some(Value::str("dotted")));
        assert_eq!(h.special("GRAMMARSTYLE"), Some(Value::str("dotted")));
        h.editor_op(EditorOp::SetLintStyle { grammar: false, style: UnderlineStyle::Squiggle }).unwrap();
        h.editor_op(EditorOp::SetLintStyle { grammar: true, style: UnderlineStyle::Solid }).unwrap();
        assert_eq!(h.spell_style(), UnderlineStyle::Squiggle);
        assert_eq!(h.grammar_style(), UnderlineStyle::Solid);
        assert_eq!(h.special("SPELLSTYLE"), Some(Value::str("squiggle")));
        assert_eq!(h.special("GRAMMARSTYLE"), Some(Value::str("solid")));
        // Parsing accepts aliases and rejects junk.
        assert_eq!(UnderlineStyle::parse("wavy"), Some(UnderlineStyle::Squiggle));
        assert_eq!(UnderlineStyle::parse("straight"), Some(UnderlineStyle::Solid));
        assert_eq!(UnderlineStyle::parse("nope"), None);
        // Thick variants: canonical name, a `thick`/`bold` qualifier in either order, round-trips.
        assert_eq!(UnderlineStyle::parse("dotted-thick"), Some(UnderlineStyle::DottedThick));
        assert_eq!(UnderlineStyle::parse("thick squiggle"), Some(UnderlineStyle::SquiggleThick));
        assert_eq!(UnderlineStyle::parse("solid_bold"), Some(UnderlineStyle::SolidThick));
        assert_eq!(UnderlineStyle::DottedThick.name(), "dotted-thick");
        assert_eq!(UnderlineStyle::parse(UnderlineStyle::SquiggleThick.name()), Some(UnderlineStyle::SquiggleThick));
        assert!(UnderlineStyle::SolidThick.is_thick() && !UnderlineStyle::Solid.is_thick());
        assert_eq!(UnderlineStyle::SquiggleThick.shape(), UnderlineStyle::Squiggle);
        h.editor_op(EditorOp::SetLintStyle { grammar: false, style: UnderlineStyle::DottedThick }).unwrap();
        assert_eq!(h.special("SPELLSTYLE"), Some(Value::str("dotted-thick")));
    }

    #[test]
    fn panecolor_sets_reads_back_and_theme_resets_it() {
        use super::{AppHost, ColorGround, EditorOp};
        use xyscript_xpl::{Host, Value};
        let mut h = AppHost::new();
        h.editor_op(EditorOp::SetColor { ground: ColorGround::PaneBorder, rgb: (10, 20, 30) })
            .unwrap();
        assert_eq!(h.pane_border_color(), (10, 20, 30));
        assert_eq!(h.special("PANECOLOR"), Some(Value::str("10 20 30")));
        // THEME re-sets it (like the other themed colours).
        h.editor_op(EditorOp::ApplyTheme("light".into())).unwrap();
        assert_ne!(h.pane_border_color(), (10, 20, 30));
    }

    #[test]
    fn buffer_nav_moves_focus_to_a_visible_pane() {
        use super::{AppHost, EditorOp};
        use xyscript_xpl::Host;
        let mut h = AppHost::new();
        let a = h.docs[0].id();
        // Split: two panes — A (left) and a fresh empty buffer E (right, focused).
        h.editor_op(EditorOp::PaneSplitTile { stacked: false, target: None }).unwrap();
        assert_eq!(h.pane_count(), 2);
        let e = h.panes().focused_doc();
        assert_ne!(e, a);
        // BUFNEXT lands on A, which is already shown in the left pane → focus (border) moves THERE
        // rather than the guard blocking a content swap.
        h.editor_op(EditorOp::BufNext).unwrap();
        assert_eq!(h.panes().focused_doc(), a);
        assert_eq!(h.docs[h.active].id(), a);
        // BUFNEXT again lands on E (right pane) → focus jumps back to it.
        h.editor_op(EditorOp::BufNext).unwrap();
        assert_eq!(h.panes().focused_doc(), e);
    }

    #[test]
    fn shipped_default_cfg_matches_config_script() {
        // The shipped runtime cfg must equal what a fresh instance emits, so `LOADCFG xyscript-default.cfg`
        // is a true no-op on defaults. If a default changes in the code, this fails — regenerate with
        // `cargo test -p xyscript-app --lib regenerate_default_cfg -- --ignored` (or `SAVECFG` on a fresh
        // instance). Line endings are normalized so a CRLF checkout does not trip it.
        const SHIPPED: &str = include_str!("../../packaging/runtime/xyscript-default.cfg");
        let generated = super::AppHost::new().config_script(true, false);
        assert_eq!(
            generated.replace("\r\n", "\n"),
            SHIPPED.replace("\r\n", "\n"),
            "packaging/runtime/xyscript-default.cfg is stale — regenerate it (see the test comment)"
        );
    }

    #[test]
    #[ignore]
    fn regenerate_default_cfg() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../packaging/runtime/xyscript-default.cfg");
        std::fs::write(path, super::AppHost::new().config_script(true, false)).expect("write default cfg");
    }

    #[test]
    #[cfg(windows)]
    fn lexical_abs_preserves_subst_drive_and_resolves_dots() {
        use super::lexical_abs;
        use std::path::{Path, PathBuf};
        let cwd = Path::new(r"X:\proj\src");
        // A `subst`ed drive letter is kept as-is — NOT rewritten to a backing path.
        assert_eq!(lexical_abs(cwd, Path::new(r"X:\a\b")), PathBuf::from(r"X:\a\b"));
        // `.`/`..` resolve by pure path logic.
        assert_eq!(lexical_abs(cwd, Path::new(r"X:\a\.\b\..\c")), PathBuf::from(r"X:\a\c"));
        // `..` at the root is a no-op (can't rewind past the drive).
        assert_eq!(lexical_abs(cwd, Path::new(r"X:\..\..")), PathBuf::from(r"X:\"));
        // A relative path resolves against cwd (and cleans dots), staying on the SUBST drive.
        assert_eq!(lexical_abs(cwd, Path::new(r"..\lib")), PathBuf::from(r"X:\proj\lib"));
        // A `\\?\` verbatim prefix is stripped.
        assert_eq!(lexical_abs(cwd, Path::new(r"\\?\X:\a")), PathBuf::from(r"X:\a"));
    }

    const NOTE: &str = "before\n```\nMSG one\nMSG two\n```\nmiddle\n~~~\nMSG three\n~~~\nafter";
    // lines: 0 before, 1 ```, 2 MSG one, 3 MSG two, 4 ```, 5 middle,
    //        6 ~~~, 7 MSG three, 8 ~~~, 9 after

    #[test]
    fn fenced_block_found_from_content_or_fence_line() {
        // Caret on a content line of the first block.
        assert_eq!(fenced_block_at(NOTE, 2).as_deref(), Some("MSG one\nMSG two"));
        assert_eq!(fenced_block_at(NOTE, 3).as_deref(), Some("MSG one\nMSG two"));
        // Caret on the opening or closing fence line still finds the block.
        assert_eq!(fenced_block_at(NOTE, 1).as_deref(), Some("MSG one\nMSG two"));
        assert_eq!(fenced_block_at(NOTE, 4).as_deref(), Some("MSG one\nMSG two"));
        // The second (~~~) block.
        assert_eq!(fenced_block_at(NOTE, 7).as_deref(), Some("MSG three"));
    }

    #[test]
    fn no_block_outside_fences_or_when_unterminated() {
        // Caret on prose between/around blocks.
        assert!(fenced_block_at(NOTE, 0).is_none());
        assert!(fenced_block_at(NOTE, 5).is_none());
        assert!(fenced_block_at(NOTE, 9).is_none());
        // An unterminated fence yields no runnable block.
        assert!(fenced_block_at("text\n```\nMSG x\nmore", 2).is_none());
    }

    #[test]
    fn lang_table_parses_keys_strips_comments_and_skips_blanks() {
        let src = "\
# a comment line
menu.file.new = Nouveau            # New
menu.file.open = Ouvrir…
menu.file.save =                   # Save  (blank value → falls back to English)

msg.grep.summary = {count} correspondances   # {count} matches
not a kv line
";
        let t = super::parse_lang_table(src);
        assert_eq!(t.get("menu.file.new").map(String::as_str), Some("Nouveau")); // comment stripped
        assert_eq!(t.get("menu.file.open").map(String::as_str), Some("Ouvrir…")); // no comment
        assert_eq!(t.get("menu.file.save"), None); // blank value → skipped (English fallback)
        assert_eq!(t.get("msg.grep.summary").map(String::as_str), Some("{count} correspondances")); // placeholder kept
        assert_eq!(t.len(), 3); // the comment line and "not a kv line" are ignored
    }

    #[test]
    fn tr_msg_and_tr_fmt_translate_or_fall_back_and_substitute() {
        let mut host = super::AppHost::new();
        host.lang = super::parse_lang_table(
            "msg.readonly = document en lecture seule\nmsg.search.not_found = introuvable : {find}\n",
        );
        // Present key → translated; absent key → English fallback verbatim.
        assert_eq!(host.tr_msg("msg.readonly", "document is read-only"), "document en lecture seule");
        assert_eq!(host.tr_msg("msg.grep.none", "GREP: no matches"), "GREP: no matches");
        // Named placeholder is substituted into the translated template.
        assert_eq!(
            host.tr_fmt("msg.search.not_found", "not found: {find}", &[("find", &"foo")]),
            "introuvable : foo",
        );
        // Missing key → English template still gets its placeholder filled.
        assert_eq!(
            host.tr_fmt("msg.open.not_found", "file not found: {path}", &[("path", &"a/b.txt")]),
            "file not found: a/b.txt",
        );
    }

    #[test]
    fn tr_plural_selects_one_or_other_form() {
        let mut host = super::AppHost::new();
        // English fallback: n == 1 → the `.one` template, else `.other`; `{n}` filled in.
        assert_eq!(host.tr_plural("msg.grep.match", "{n} match", "{n} matches", 1), "1 match");
        assert_eq!(host.tr_plural("msg.grep.match", "{n} match", "{n} matches", 0), "0 matches");
        assert_eq!(host.tr_plural("msg.grep.match", "{n} match", "{n} matches", 7), "7 matches");
        // With a catalog: the localized singular/plural forms win (French agrees with English that
        // only 1 is singular — and the zero case never reaches here in the grep summary anyway).
        host.lang = super::parse_lang_table(
            "msg.grep.match.one = {n} correspondance\nmsg.grep.match.other = {n} correspondances\n",
        );
        assert_eq!(host.tr_plural("msg.grep.match", "{n} match", "{n} matches", 1), "1 correspondance");
        assert_eq!(host.tr_plural("msg.grep.match", "{n} match", "{n} matches", 4), "4 correspondances");
    }

    #[test]
    fn tr_plural_honors_a_three_form_rule() {
        let mut host = super::AppHost::new();
        // Polish declares its rule via `_plural` and provides one/few/many forms.
        host.lang = super::parse_lang_table(
            "_plural = polish\n\
             msg.grep.match.one = {n} dopasowanie\n\
             msg.grep.match.few = {n} dopasowania\n\
             msg.grep.match.many = {n} dopasowań\n",
        );
        assert_eq!(host.tr_plural("msg.grep.match", "{n} match", "{n} matches", 1), "1 dopasowanie"); // one
        assert_eq!(host.tr_plural("msg.grep.match", "{n} match", "{n} matches", 3), "3 dopasowania"); // few
        assert_eq!(host.tr_plural("msg.grep.match", "{n} match", "{n} matches", 5), "5 dopasowań"); // many
        assert_eq!(host.tr_plural("msg.grep.match", "{n} match", "{n} matches", 12), "12 dopasowań"); // teens → many
        assert_eq!(host.tr_plural("msg.grep.match", "{n} match", "{n} matches", 22), "22 dopasowania"); // few again
        // A missing category (e.g. an incomplete catalog) falls back to the English plural.
        assert_eq!(host.tr_plural("msg.grep.file", "{n} file", "{n} files", 3), "3 files");
    }

    #[test]
    fn plural_category_boundaries_per_rule() {
        let cat = |rule: &str, n: usize| {
            let mut h = super::AppHost::new();
            h.lang = super::parse_lang_table(&format!("_plural = {rule}\n"));
            h.plural_category(n)
        };
        // one/other (default / unknown rule): only 1 is "one".
        assert_eq!(cat("", 1), "one");
        assert_eq!(cat("", 2), "other");
        // Polish: 1=one; 2–4 except teens=few; else many.
        assert_eq!(cat("polish", 1), "one");
        assert_eq!(cat("polish", 4), "few");
        assert_eq!(cat("polish", 5), "many");
        assert_eq!(cat("polish", 14), "many"); // teen → many, not few
        assert_eq!(cat("polish", 24), "few"); // 24 % 10 == 4, not a teen
        // Russian/Ukrainian: one covers 1,21,31 (n%10==1, not the teens); few 2–4 & 22–24; many rest.
        assert_eq!(cat("east-slavic", 1), "one");
        assert_eq!(cat("east-slavic", 21), "one"); // unlike Polish
        assert_eq!(cat("east-slavic", 11), "many"); // teen
        assert_eq!(cat("east-slavic", 2), "few");
        assert_eq!(cat("east-slavic", 24), "few");
        assert_eq!(cat("east-slavic", 12), "many"); // teen
        assert_eq!(cat("east-slavic", 5), "many");
        // Czech: 1=one; 2–4 exactly=few; else other (22 is NOT few).
        assert_eq!(cat("czech", 1), "one");
        assert_eq!(cat("czech", 3), "few");
        assert_eq!(cat("czech", 5), "other");
        assert_eq!(cat("czech", 22), "other");
        // Romanian: 1=one; n%100 in 2–19=few; else other (the "de" form).
        assert_eq!(cat("romanian", 1), "one");
        assert_eq!(cat("romanian", 2), "few");
        assert_eq!(cat("romanian", 19), "few");
        assert_eq!(cat("romanian", 20), "other");
        assert_eq!(cat("romanian", 119), "few"); // 119 % 100 == 19
    }
}
