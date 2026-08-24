//! XyScript GUI shell — live build, Milestone 2 (`gui-shell-plan.md`).
//!
//! M1 (done): egui window + CPU-rastered cosmic-text canvas of the real `Editor`
//! doc + physical-key→KBD input + status. M2 adds **editor usability**:
//! - **Scrolling** — follow the caret on edit/motion; mouse-wheel free-scroll.
//! - **Highlight colors** — the core's tokens → cosmic-text rich-text per-span color.
//! - **Mouse** — click to place the caret; drag to select (via hit-testing).
//!
//! Still deferred: OS-text/IME multilingual typing, the typst preview pane,
//! menus + full verb wiring, multi-window/tabs.

// In a RELEASE build, run as a native Windows GUI app (no console window). Debug builds keep the
// console so dev `eprintln!`/panic output stays visible. A panic in a windowed build would otherwise
// vanish, so `install_crash_logger` writes crashes to a log file next to the exe.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::rc::Rc;

use cosmic_text::{Attrs, AttrsList, Buffer, BufferLine, Color as CColor, Cursor, Edit, Editor, Ellipsize, Family, FontSystem, Hinting, LineEnding, Metrics, Scroll, Selection, Shaping, SwashCache, Wrap};

mod runtime;

/// Pick the closest face of `family` to the requested weight (bold ⇒ 700, else 400) and slant
/// (italic ⇒ Italic, accepting Oblique as a near-match). Falls back to the nearest available face
/// when the family lacks a true bold/italic (egui can't synthesize styles — no faux-bolding).
fn pick_face<'a>(
    db: &'a cosmic_text::fontdb::Database,
    family: &str,
    bold: bool,
    italic: bool,
) -> Option<&'a cosmic_text::fontdb::FaceInfo> {
    use cosmic_text::fontdb::Style;
    let want_weight = if bold { 700 } else { 400 };
    db.faces()
        .filter(|f| f.families.iter().any(|(n, _)| n.eq_ignore_ascii_case(family)))
        .min_by_key(|f| {
            let style_pen: u32 = match (italic, f.style) {
                (false, Style::Normal) | (true, Style::Italic) => 0,
                (true, Style::Oblique) => 200, // oblique is an acceptable italic substitute
                _ => 1000,
            };
            style_pen + (i32::from(f.weight.0) - want_weight).unsigned_abs()
        })
}

/// Load a `family` face (matching `bold`/`italic`) from the cosmic font DB and register it in `defs`
/// under `key`, prepending `key` to each `target` family list. Returns `false` (leaving `defs`
/// untouched) if no installed face matched the name.
///
/// This is the only way a `UIFAMILY`/`TXTFAMILY`/preview choice reaches egui: unlike the editor
/// canvas (cosmic-text, which renders any installed family by name), egui has no system-font access
/// and only sees faces whose *bytes* we hand it.
fn load_face_into(
    defs: &mut egui::FontDefinitions,
    db: &cosmic_text::fontdb::Database,
    family: &str,
    key: &str,
    targets: &[egui::FontFamily],
    bold: bool,
    italic: bool,
) -> bool {
    let Some(face) = pick_face(db, family, bold, italic) else {
        return false;
    };
    let Some((bytes, index)) = db.with_face_data(face.id, |data, idx| (data.to_vec(), idx)) else {
        return false;
    };
    let mut data = egui::FontData::from_owned(bytes);
    data.index = index;
    defs.font_data.insert(key.to_owned(), std::sync::Arc::new(data));
    for fam in targets {
        defs.families.entry(fam.clone()).or_default().insert(0, key.to_owned());
    }
    true
}

/// Build `family` into a fresh `FontDefinitions` (egui's embedded fallbacks retained) and install
/// it on `ctx`, prepending it to each `target` family. Used for the separate `TXT` window context
/// (one custom family). `set_fonts` rebuilds the atlas, so callers gate this on an actual change.
pub(crate) fn apply_egui_family(
    ctx: &egui::Context,
    db: &cosmic_text::fontdb::Database,
    family: &str,
    targets: &[egui::FontFamily],
    bold: bool,
    italic: bool,
) -> bool {
    let mut defs = egui::FontDefinitions::default();
    if load_face_into(&mut defs, db, family, "xy-family", targets, bold, italic) {
        ctx.set_fonts(defs);
        true
    } else {
        false
    }
}

/// The egui family name the font-picker preview label renders in (always defined while the picker
/// is open — see `rebuild_main_fonts`).
fn preview_family_name() -> egui::FontFamily {
    egui::FontFamily::Name("xy-preview".into())
}

/// Format a point size for display/editing: integers without a trailing `.0`, else one decimal.
fn fmt_size(pt: f32) -> String {
    if (pt.fract()).abs() < 0.05 {
        format!("{}", pt.round() as i32)
    } else {
        format!("{pt:.1}")
    }
}

/// Common point sizes offered in the font-picker size pulldown.
const SIZE_PRESETS: &[f32] =
    &[8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 16.0, 18.0, 20.0, 24.0, 28.0, 32.0, 36.0, 48.0, 72.0];

/// Map an `EDSTYLE` value to cosmic `Attrs` weight + slant for the editor canvas (cosmic matches
/// the styled face from its DB, or synthesizes when absent).
fn editor_cosmic_style(style: FontStyle) -> (cosmic_text::Weight, cosmic_text::Style) {
    let weight = if style.bold() { cosmic_text::Weight::BOLD } else { cosmic_text::Weight::NORMAL };
    let slant = if style.italic() { cosmic_text::Style::Italic } else { cosmic_text::Style::Normal };
    (weight, slant)
}

use xyscript_app::document::RenderKind;
use xyscript_app::highlight::{Token, TokenKind};
use xyscript_app::host::{
    CaretShape, CloseAnswer, ColorTarget, FontPickerKind, FontStyle, OpenAnswer, VerifyChoice,
};
use xyscript_app::boot::resolve_startup_script;
use xyscript_app::Editor as XyEditor;
use xyscript_kbd::{load, LoadOutcome};

const INTERNAL_KBD: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../xyscript-kbd/kbd/xyscript-internal.kbd"));

/// The testers' reference, baked into the binary so Help ▸ Reference works on any install (written
/// to a temp `.typ` and opened on demand).
const REFERENCE_TYP: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../docs/reference.typ"));

/// The user manual, baked in the same way so Help ▸ Manual works on any install (written to a temp
/// `.typ` and opened on demand). Self-contained Typst — no external assets to resolve.
const MANUAL_TYP: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../docs/manual.typ"));

/// Crop an RGBA image to the top-left `max`×`max` at most, so a texture upload can't exceed the GPU's
/// max texture side (egui panics otherwise). A no-op when it already fits. Used for the preview pane,
/// where a tall page at a high zoom can overflow.
fn clamp_rgba(rgba: Vec<u8>, w: usize, h: usize, max: usize) -> (Vec<u8>, usize, usize) {
    if w <= max && h <= max {
        return (rgba, w, h);
    }
    let (cw, ch) = (w.min(max), h.min(max));
    let mut out = Vec::with_capacity(cw * ch * 4);
    for row in 0..ch {
        let start = row * w * 4;
        out.extend_from_slice(&rgba[start..start + cw * 4]);
    }
    (out, cw, ch)
}

#[allow(dead_code)] // retained as a handy paste-in sample
const SAMPLE_MD: &str = "\
# XyScript GUI — Milestone 2

Type (English) to edit; **click** to place the caret, **drag** to select, and use
the **mouse wheel** to scroll. Syntax highlighting is *on* here to show colors.

## Multilingual rendering
中文 · 日本語 · 한국어 · Ελληνικά · العربية · עברית · café · /ˈtʃɪkənz/

## A list
- first item
- second item with `inline code`
- third item

## Code
```rust
fn main() {
    println!(\"hello, world\");
}
```

## More lines (to exercise scrolling)
line A
line B
line C
line D
line E
line F
The end.
";

// Editor font size + line height are no longer constants: they come from the host's `EDFONT`
// / `LEADING` knobs (B16), recomputed into `GuiApp.font_size`/`line_height` each frame (default
// 22pt / 32px). All canvas-metric code reads those fields.

/// Command keys go through the KBD (`feed_key`). Printable characters do NOT —
/// they arrive as OS `Text`/IME events (layout/IME-resolved, multilingual), so
/// the KBD-as-layout path is reserved for keys the OS doesn't turn into text.
fn key_to_code(key: egui::Key) -> Option<u8> {
    use egui::Key::*;
    Some(match key {
        Enter => 28,     // sample KBD: ENTER (insert the doc's newline; CMline intercepts → submit)
        Backspace => 14, // sample KBD: bs (delete back)
        Delete => 90,    // sample KBD: RC (delete forward)
        F5 => 63,        // sample KBD: BC (blank command line)
        F9 => 67,        // sample KBD: XC (exec command line)
        F10 => 68,       // sample KBD: TH (toggle cmline/text focus)
        // Gray nav cluster — the realigned XyWrite chart codes (matching runtime.rs's intercept
        // path and what every KBD binds: arrows 100-103, Home/End 92/96, PageUp/PageDown 93/97).
        // NOT the numpad codes (71-81): those route through the keymap before egui (NumLock off →
        // canonical 72/80/… reclaimed for egui in `intercept_key`), so this path wants the gray set.
        ArrowLeft => 101,
        ArrowRight => 103,
        ArrowUp => 100,
        ArrowDown => 102,
        Home => 92,
        End => 96,
        PageUp => 93,
        PageDown => 97,
        _ => return None,
    })
}

// The offset-mapping helpers below take precomputed, sorted position indices so each
// call is O(log n) instead of O(offset). They operate within the current render window
// (see `DocWindow`): `nl` = `\n` byte offsets in the window's **display** string; `cr` =
// `\r` byte offsets in the window's **decoded** text. Since the window is only a few
// dozen lines, these indices are tiny and rebuilt per frame.

/// `display` byte offset → cosmic [`Cursor`] (line, index-within-line). `display` has no
/// `\r`, so a line's content ends at its `\n`.
fn byte_to_cursor(display: &str, offset: usize, nl: &[usize]) -> Cursor {
    let offset = offset.min(display.len());
    let line = nl.partition_point(|&p| p < offset);
    let line_start = if line > 0 { nl[line - 1] + 1 } else { 0 };
    let line_end = nl.get(line).copied().unwrap_or(display.len());
    Cursor::new(line, offset.min(line_end) - line_start)
}

fn cursor_to_byte(display: &str, cur: Cursor, nl: &[usize]) -> usize {
    let line_start = if cur.line == 0 {
        0
    } else {
        nl.get(cur.line - 1).map(|&p| p + 1).unwrap_or(display.len())
    };
    (line_start + cur.index).min(display.len())
}

/// The string handed to cosmic-text for layout: the decoded document text with
/// every `\r` removed. cosmic's line splitter falls back to a Unicode-bidi path
/// the moment the text contains any control byte other than `\n`/`\r`/`\t` (a
/// trailing Ctrl-Z / DOS EOF marker is enough), and that path treats the CR and
/// the LF of a CRLF as *separate* paragraph breaks — doubling the line count and
/// stranding our `\n`-based caret on phantom empty lines. Feeding cosmic
/// `\n`-only text keeps its line indices equal to our logical lines.
fn strip_cr(text: &str) -> String {
    text.chars().filter(|&c| c != '\r').collect()
}

/// Decoded offset (has `\r`) → display offset (`\r`-stripped): subtract the `\r`s before
/// it. `cr` = sorted `\r` offsets in the decoded text.
fn decoded_to_display(off: usize, cr: &[usize]) -> usize {
    off - cr.partition_point(|&p| p < off)
}

/// Alpha-blend one `(x,y,cw,ch)` cell of a cosmic `Color` (its own alpha) into the RGBA `canvas`,
/// clipped. The per-cell primitive behind both the default `ed.draw` closure and the `WRAPINDENT`
/// manual glyph draw, so the two paths blend glyph pixels identically.
fn blend_cell(canvas: &mut [u8], w: usize, h: usize, x: i32, y: i32, cw: u32, ch: u32, color: CColor) {
    let sa = color.a();
    if sa == 0 {
        return; // fully transparent (glyph background) — nothing to paint
    }
    let (sr, sg, sb) = (color.r(), color.g(), color.b());
    // Clip once, up front, instead of a per-pixel bounds check.
    let x0 = x.max(0) as usize;
    let y0 = y.max(0) as usize;
    let x1 = ((x + cw as i32).max(0) as usize).min(w);
    let y1 = ((y + ch as i32).max(0) as usize).min(h);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    if sa == 255 {
        // Opaque coverage (glyph interiors — the common case): write pixels directly.
        for py in y0..y1 {
            let row = (py * w + x0) * 4;
            for k in 0..(x1 - x0) {
                let i = row + k * 4;
                canvas[i] = sr;
                canvas[i + 1] = sg;
                canvas[i + 2] = sb;
            }
        }
        return;
    }
    // Partial coverage (anti-aliased edges): integer blend, no per-pixel f32 multiply + round.
    let (a, ia) = (sa as u32, 256 - sa as u32);
    let (ra, ga, ba) = (sr as u32 * a, sg as u32 * a, sb as u32 * a);
    for py in y0..y1 {
        let row = (py * w + x0) * 4;
        for k in 0..(x1 - x0) {
            let i = row + k * 4;
            canvas[i] = ((ra + canvas[i] as u32 * ia) >> 8) as u8;
            canvas[i + 1] = ((ga + canvas[i + 1] as u32 * ia) >> 8) as u8;
            canvas[i + 2] = ((ba + canvas[i + 2] as u32 * ia) >> 8) as u8;
        }
    }
}

/// Alpha-blend a solid `(r,g,b)` rectangle at `alpha` into an RGBA `canvas` of `w`×`h` px, clipped
/// to the canvas. Used for the FIND "highlight all" bands, painted BEHIND the glyphs (which then
/// alpha-blend on top), so highlighted text stays legible on any theme.
#[allow(clippy::too_many_arguments)]
fn fill_blend(canvas: &mut [u8], w: usize, h: usize, x0: i32, y0: i32, x1: i32, y1: i32, r: u8, g: u8, b: u8, alpha: f32) {
    let x0 = x0.max(0) as usize;
    let y0 = y0.max(0) as usize;
    let x1 = (x1.max(0) as usize).min(w);
    let y1 = (y1.max(0) as usize).min(h);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    // Opaque fill (the EMPH/selection/current-match bands): write the pixels directly — no per-pixel
    // float blend. A GREP results tab paints a band on every visible line (each line IS a match), so
    // this is the hot path there.
    if alpha >= 1.0 {
        for py in y0..y1 {
            let row = (py * w + x0) * 4;
            for k in 0..(x1 - x0) {
                let i = row + k * 4;
                canvas[i] = r;
                canvas[i + 1] = g;
                canvas[i + 2] = b;
            }
        }
        return;
    }
    // Translucent: integer blend (0..=256 weight), avoiding the f32 multiply + round per channel.
    let a = (alpha.clamp(0.0, 1.0) * 256.0) as u32;
    let (ia, ra, ga, ba) = (256 - a, r as u32 * a, g as u32 * a, b as u32 * a);
    for py in y0..y1 {
        let row = (py * w + x0) * 4;
        for k in 0..(x1 - x0) {
            let i = row + k * 4;
            canvas[i] = ((ra + canvas[i] as u32 * ia) >> 8) as u8;
            canvas[i + 1] = ((ga + canvas[i + 1] as u32 * ia) >> 8) as u8;
            canvas[i + 2] = ((ba + canvas[i + 2] as u32 * ia) >> 8) as u8;
        }
    }
}

/// Display offset → decoded offset (inverse of [`decoded_to_display`]). Returns the
/// decoded position of the `disp`-th retained (non-`\r`) byte, or `decoded_len` if `disp`
/// is past the last one (caret at end). `g(d) = d − (#cr < d)` = retained bytes before d.
fn display_to_decoded(disp: usize, cr: &[usize], decoded_len: usize) -> usize {
    if disp + cr.len() >= decoded_len {
        return decoded_len;
    }
    // Smallest d with g(d) ≥ disp+1, minus 1 — i.e. the disp-th retained byte's position.
    let target = disp + 1;
    let (mut lo, mut hi) = (0usize, decoded_len);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let g = mid - cr.partition_point(|&p| p < mid);
        if g < target {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo - 1
}

/// Abbreviate a long working-directory path for the status line: keep the drive/root, elide the
/// middle as `…`, and show the last two components (e.g. `C:\…\cldis\rusted`). Short paths and
/// those with ≤2 components are returned unchanged.
/// Filter a `PICK` item list by the live type-to-find query (case-insensitive substring; empty =
/// all), pairing each surviving item with its ORIGINAL index so a selection resumes the script with
/// the right position even when the view is filtered.
fn pick_filter_view(items: &[String], filter: &str) -> Vec<(usize, String)> {
    let needle = filter.to_lowercase();
    items
        .iter()
        .enumerate()
        .filter(|(_, s)| needle.is_empty() || s.to_lowercase().contains(&needle))
        .map(|(i, s)| (i, s.clone()))
        .collect()
}

/// Truncate `path` to at most `max` characters for the `$CWD` display (`PATHLEN`). Keeps the drive
/// (first 2 chars) and a `...` marker, then FILLS the remaining width with the END of the path — so
/// the final subdir is always visible and the whole `PATHLEN` budget is used (the user's choice).
/// Paths already within `max` are returned unchanged. Operates on chars (not bytes) — Unicode-safe.
fn truncate_path(path: &str, max: usize) -> String {
    let chars: Vec<char> = path.chars().collect();
    let len = chars.len();
    if len <= max {
        return path.to_string();
    }
    // Too narrow for head(2) + "..."(3) + ≥1 tail char: just show the last `max` chars.
    if max < 6 {
        return chars[len - max..].iter().collect();
    }
    let head: String = chars[..2].iter().collect();
    let tail: String = chars[len - (max - 5)..].iter().collect(); // 2 head + 3 ellipsis
    format!("{head}...{tail}")
}

/// Cap a tab's FILENAME to `max` chars for the tab strip, keeping the meaningful start and the
/// extension: "my-very-long-report-final.txt" → "my-very-long-report-fi….txt". Short names are
/// returned unchanged (the full name shows on hover). Without this a long filename widens its tab to
/// fit the whole thing.
fn truncate_tab_name(name: &str, max: usize) -> String {
    if name.chars().count() <= max {
        return name.to_string();
    }
    // Preserve a short, sane extension (incl. its dot) at the end; a spaced/oversized "extension"
    // (e.g. a name that just has a late dot) is treated as no extension.
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e)
        .filter(|e| !e.is_empty() && e.chars().count() <= 5 && !e.contains(' '))
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let head_len = max.saturating_sub(1 + ext.chars().count()).max(1); // 1 for the "…"
    let head: String = name.chars().take(head_len).collect();
    format!("{head}…{ext}")
}

/// Paint token kinds onto the text and group into char-aligned colored runs.
fn color_runs<'a>(text: &str, tokens: &'a [Token]) -> Vec<(usize, usize, Option<&'a TokenKind>)> {
    let n = text.len();
    let mut kinds: Vec<Option<&TokenKind>> = vec![None; n];
    for t in tokens {
        for slot in kinds.iter_mut().take(t.end.min(n)).skip(t.start.min(n)) {
            *slot = Some(&t.kind);
        }
    }
    let mut runs = Vec::new();
    let mut start = 0usize;
    let mut cur = if n == 0 { None } else { kinds[0] };
    for (bi, _) in text.char_indices().skip(1) {
        if kinds[bi] != cur {
            runs.push((start, bi, cur));
            start = bi;
            cur = kinds[bi];
        }
    }
    if n > 0 {
        runs.push((start, n, cur));
    }
    runs
}

// Command-line placement (top/bottom/hidden) lives on the host as `CMPLACE`/`$CMPLACE`; the GUI reads
// it via `editor.special_str("CMPLACE")` and the menu fires the `CMPLACE` verb.

/// A menu/command action collected during the UI pass, then applied afterwards.
/// Collecting first keeps `self`-mutation out of egui's nested menu closures.
enum Act {
    New,
    Save,
    Highlight(bool),
    TogglePreview,
    /// The menu workhorse: run an arbitrary XPL command string (the "it's all XPL" path). Most
    /// menu items are just a verb (optionally with a picked path interpolated in); they surface
    /// their own feedback via the status line, exactly as if typed on the command line.
    Run(String),
    /// Stage text on the command line (focused, unrun) — the menu path for "…" items that need the
    /// user to finish typing an argument (e.g. Search ▸ Emphasize ▸ Word or phrase…).
    StageCmdline(String),
    /// Open the Find/Replace dialog (Search ▸ Find… / Replace…). `replace` focuses the Replace field.
    OpenFind { replace: bool },
    /// Open the Run-command dialog (Tools ▸ Shell command…).
    OpenRunCmd,
    /// Open the GREP dialog (Search ▸ Grep…).
    OpenGrep,
    /// Show the Document ▸ Properties info box.
    ShowProperties,
    /// Show the Help ▸ About box.
    ShowAbout,
    /// Help ▸ Reference — open the embedded reference doc in a tab.
    ShowReference,
    /// Help ▸ Manual — open the embedded user manual in a tab.
    ShowManual,
}

/// A native file dialog deferred to run AFTER the egui frame. The runtime drains it in
/// `about_to_wait` (outside any event dispatch) and parents it to the main window, so (a) the
/// blocking OS modal loop never re-enters `run_ui` mid-frame — which deadlocked egui's Context and
/// froze the app — and (b) the OS makes it *owner-modal*: clicks on the main window are refused
/// while it's open (it stays put), while clicking another application still switches to it.
#[derive(Clone)]
enum PendingDialog {
    /// `hex` = open the picked file in the hex view (`OPEN/HEX` with no path).
    Open { hex: bool },
    SaveAs,
    Export,
    Run { save: bool, filter: Option<(String, Vec<String>)>, template: String, dir: Option<std::path::PathBuf> },
}

// ===================================================================================================
// Menu model — the single source of truth the mouse menu bar and the keyboard navigator both render.
// The tree is rebuilt each frame from the live editor (so ✓ marks and generated submenus stay fresh);
// `MenuNav` is a pure, unit-tested state machine that drives keyboard navigation over that tree.
// ===================================================================================================

/// What activating a menu item does. `Run` is the common case (an XPL verb); the rest map to the
/// GUI's existing [`Act`] dispatch (file dialogs, info boxes). Cloned when an item is chosen.
#[derive(Clone)]
enum MenuAct {
    /// Run an XPL command string (the "it's all XPL" path).
    Run(String),
    /// Stage the given text on the command line (focused, caret at end) without running it — for "…"
    /// items whose argument the user finishes typing (e.g. `EMPH ` for a word to emphasize).
    StageCmdline(String),
    New,
    /// Save — to the current path, or prompt for one if untitled.
    Save,
    /// Pick a file to open (text/markdown filter).
    PickOpen,
    /// Pick a destination for Save As (markdown filter).
    PickSaveAs,
    /// Pick a destination for the PDF export.
    PickExport,
    /// Pick a file (open or save; optional extension filter), then run `template` with `{}` → the
    /// chosen path. Covers READ / SAD / MERGE / RUN / LOADLIB / LOADKBD / SAVECFG / ENCODING-C / …
    /// `dir` seeds the dialog's starting folder (used only when it exists); `None` = the default.
    PickRun {
        save: bool,
        filter: Option<(&'static str, &'static [&'static str])>,
        template: String,
        dir: Option<std::path::PathBuf>,
    },
    Highlight(bool),
    TogglePreview,
    OpenFind { replace: bool },
    OpenRunCmd,
    OpenGrep,
    ShowProperties,
    ShowAbout,
    ShowReference,
    ShowManual,
}

/// One node in the menu tree.
#[derive(Clone)]
enum MenuNode {
    /// A horizontal rule between groups of items.
    Sep,
    /// A leaf: a (possibly ✓-prefixed) label and the action it runs.
    Item { label: String, act: MenuAct },
    /// A submenu: a label and its children. The top-level menus are themselves `Sub`s.
    Sub { label: String, children: Vec<MenuNode> },
}

impl MenuNode {
    fn is_sep(&self) -> bool {
        matches!(self, MenuNode::Sep)
    }
    fn label(&self) -> &str {
        match self {
            MenuNode::Item { label, .. } | MenuNode::Sub { label, .. } => label,
            MenuNode::Sep => "",
        }
    }
    /// The children of a submenu (`None` for a leaf or separator).
    fn children(&self) -> Option<&[MenuNode]> {
        match self {
            MenuNode::Sub { children, .. } => Some(children),
            _ => None,
        }
    }
}

// -- Convenience constructors for the tree builder. ------------------------------------------------
fn m_item(label: impl Into<String>, cmd: impl Into<String>) -> MenuNode {
    MenuNode::Item { label: label.into(), act: MenuAct::Run(cmd.into()) }
}
/// A menu leaf that *stages* `text` on the command line (focused, caret at end) without running it —
/// for "…" items that need the user to finish typing an argument. Unlike a `BC … ;CMLINE` command,
/// this preserves the exact text (e.g. the trailing space after `EMPH `).
fn m_stage(label: impl Into<String>, text: impl Into<String>) -> MenuNode {
    MenuNode::Item { label: label.into(), act: MenuAct::StageCmdline(text.into()) }
}
fn m_act(label: impl Into<String>, act: MenuAct) -> MenuNode {
    MenuNode::Item { label: label.into(), act }
}
fn m_sub(label: impl Into<String>, children: Vec<MenuNode>) -> MenuNode {
    MenuNode::Sub { label: label.into(), children }
}
/// A ✔-prefixed toggle item that runs `cmd` (the menu's "tog!" idiom). Uses U+2714 (HEAVY CHECK
/// MARK), NOT U+2713 — the latter is absent from every bundled egui font and renders as tofu.
fn m_tog(label: impl std::fmt::Display, on: bool, cmd: impl Into<String>) -> MenuNode {
    let mark = if on { "✔ " } else { "   " };
    m_item(format!("{mark}{label}"), cmd)
}

/// The six spelling/grammar underline styles as a submenu — a ✔-marked toggle per style that runs
/// `<verb> <name>` (e.g. `SPELLSTYLE squiggle-thick`). `current` is the live `$SPELLSTYLE`-style name.
fn style_choices(verb: &str, current: &str) -> Vec<MenuNode> {
    [
        ("Dotted", "dotted"),
        ("Dotted (thick)", "dotted-thick"),
        ("Solid", "solid"),
        ("Solid (thick)", "solid-thick"),
        ("Squiggle", "squiggle"),
        ("Squiggle (thick)", "squiggle-thick"),
    ]
    .iter()
    .map(|(label, name)| m_tog(label, current == *name, format!("{verb} {name}")))
    .collect()
}

/// Where the FIND dialog's live landing jumps the caret, given the sorted (by start) match list.
/// With `from_top` the search BEGINS at the top, so the target is always the first match in the
/// document. Otherwise the search runs FORWARD from the fixed `origin` (the caret when the dialog
/// opened): the first match at/after `origin`, wrapping to the first match only when none lies ahead.
/// Using a fixed origin — not the live caret, which the preview moves each keystroke — is what keeps
/// the landing from walking to the top of the file as you type.
fn find_landing_target(from_top: bool, matches: &[(usize, usize)], origin: usize) -> Option<usize> {
    if from_top {
        return matches.first().map(|(s, _)| *s);
    }
    matches
        .iter()
        .find(|(s, _)| *s >= origin)
        .or_else(|| matches.first())
        .map(|(s, _)| *s)
}

/// Shrink `[start, end)` to exclude leading/trailing ASCII whitespace in `doc`'s CURRENT text,
/// clamping stale offsets to the buffer length. Returns an empty span (`start == end`) when the range
/// held only whitespace. A lint underline is drawn over the returned span, so a mark that a pending
/// re-check hasn't yet realigned after an edit never paints a space or newline (kills the flash on
/// `<space>`/`<enter>`). Only ASCII whitespace is trimmed, so the byte counts are valid rope offset
/// deltas regardless of the buffer's text encoding.
fn clip_ws_span(doc: &xyscript_app::Buffer, start: usize, end: usize) -> (usize, usize) {
    let len = doc.len();
    let (mut s, mut e) = (start.min(len), end.min(len));
    if s >= e {
        return (s, s);
    }
    let txt = doc.slice(s, e);
    s += txt.len() - txt.trim_start_matches(|c: char| c.is_ascii_whitespace()).len();
    e -= txt.len() - txt.trim_end_matches(|c: char| c.is_ascii_whitespace()).len();
    (s, e.max(s))
}

/// Capitalize the first character — for menu labels built from lowercase token-kind names.
fn cap_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// The first non-separator index in a list (0 if there is none) — where a freshly-opened menu lands.
fn first_selectable(list: &[MenuNode]) -> usize {
    list.iter().position(|n| !n.is_sep()).unwrap_or(0)
}

/// Position a dropdown column so it stays on screen. `to_right = false` hangs it BELOW `anchor`,
/// left-aligned (a top-menu dropdown under its bar button); `true` hangs it to the RIGHT of
/// `anchor`, top-aligned (a cascading submenu off its parent item). If the preferred side would
/// overflow the screen's right edge the column flips to the opposite side of `anchor` (a submenu
/// opens leftward; a top dropdown right-aligns under its button); if it would overflow the bottom
/// it shifts up. `size` is the column's size measured on the previous frame — menus are stable, so
/// a freshly-opened column settles within one frame.
/// Which egui layer a menu column paints in — deeper columns MUST paint above shallower ones.
///
/// `ctx.move_to_top()` cannot deliver that. egui applies it at end-of-pass as a STABLE sort keyed on
/// a bool (`order.sort_by_key(|l| (l.order, wants_to_be_on_top.contains(l)))`), so calling it for
/// every column drops them all in the same bucket and leaves their relative order untouched. Worse,
/// egui raises an area when the POINTER interacts with it — so hovering a parent item lifts the
/// PARENT above the child it just opened. Reported from the field: `Settings ▸ Appearance ▸ Colors`
/// in an ~897 px window, where the flipped Appearance column covered the top half of the Colors
/// column it had just spawned.
///
/// `Order` is that sort's PRIMARY key, so ranking columns by depth is authoritative regardless of any
/// of that bookkeeping. The mapping is FIXED per depth (not "is this the deepest?") so a column's
/// `LayerId` never changes between frames. It also never places a column BELOW `Foreground`, where
/// they all used to be, so nothing can now fall behind an `egui::Window` (About / Properties / MSG).
/// Three distinct levels; a 4th-level column ties with the 3rd, which is no worse than before.
fn menu_col_order(depth: usize) -> egui::Order {
    match depth {
        0 => egui::Order::Foreground,
        1 => egui::Order::Tooltip,
        _ => egui::Order::Debug,
    }
}

fn place_menu_col(anchor: egui::Rect, size: egui::Vec2, screen: egui::Rect, to_right: bool) -> egui::Pos2 {
    let mut x = if to_right { anchor.right() } else { anchor.left() };
    let mut y = if to_right { anchor.top() } else { anchor.bottom() };
    if x + size.x > screen.right() {
        x = if to_right { anchor.left() - size.x } else { anchor.right() - size.x };
    }
    x = x.clamp(screen.left(), (screen.right() - size.x).max(screen.left()));
    if y + size.y > screen.bottom() {
        y = (screen.bottom() - size.y).max(screen.top());
    }
    egui::pos2(x, y)
}

/// Populate a right-click clipboard context menu — the Edit-menu clipboard block (Cut / Copy /
/// Append / Prepend / Paste). Records the chosen verb in `out` and closes the menu. Shared by the
/// document canvas and the command line; the caller runs the verb against whatever buffer is then
/// focused (the host's `active()` follows focus). `color` adds "Text colour…"/"Clear text colour"
/// — only for the DOCUMENT (a cmline selection has no rendered colour).
fn clipboard_context_menu(ui: &mut egui::Ui, out: &mut Option<&'static str>, color: bool) {
    ui.set_min_width(210.0); // wide enough that "Prepend to clipboard" doesn't wrap
    for (label, cmd) in
        [("Cut", "CUT"), ("Copy", "COPY"), ("Append to clipboard", "APPEND"), ("Prepend to clipboard", "PREPEND")]
    {
        if ui.button(label).clicked() {
            *out = Some(cmd);
            ui.close();
        }
    }
    ui.separator();
    if ui.button("Paste").clicked() {
        *out = Some("PASTE");
        ui.close();
    }
    if color {
        ui.separator();
        // Colour the selection (`FGCOLORS` picker) / clear it (`FGCOLOR default`). The caller runs
        // these with the text focused, so they act on the document's selection.
        if ui.button("Text colour…").clicked() {
            *out = Some("FGCOLORS");
            ui.close();
        }
        if ui.button("Clear text colour").clicked() {
            *out = Some("FGCOLOR default");
            ui.close();
        }
    }
}

/// Right-click menu when NOTHING is selected: Paste (dimmed when the clipboard is empty) plus
/// Select word / line / sentence / paragraph — the "define" verbs (`DW`/`DL`/`DS`/`DP`), which act
/// at the caret (so the caller drops the caret at the click point first). `multiline` includes the
/// sentence + paragraph items; they're omitted on the single-line command line, where they'd just
/// equal Select line.
fn no_selection_context_menu(
    ui: &mut egui::Ui,
    out: &mut Option<&'static str>,
    clipboard_empty: bool,
    multiline: bool,
) {
    ui.set_min_width(210.0);
    if ui.add_enabled(!clipboard_empty, egui::Button::new("Paste")).clicked() {
        *out = Some("PASTE");
        ui.close();
    }
    ui.separator();
    if ui.button("Select word").clicked() {
        *out = Some("DW");
        ui.close();
    }
    if ui.button("Select line").clicked() {
        *out = Some("DL");
        ui.close();
    }
    if multiline && ui.button("Select sentence").clicked() {
        *out = Some("DS");
        ui.close();
    }
    if multiline && ui.button("Select paragraph").clicked() {
        *out = Some("DP");
        ui.close();
    }
}

/// Auto-assign an accelerator letter to each node in one menu level: the first ASCII-alphanumeric
/// char of its label not already claimed by a sibling (case-insensitive). Returns, per node, the
/// BYTE index of the assigned char in the label — or `None` (separator, or every letter taken).
/// Deduping per level guarantees each accelerator is unambiguous within its menu (e.g. the top
/// level's `Search`→S forces `Settings`→e). No label edits needed; the chosen letter is underlined
/// at render time and matched by [`accel_match`].
fn level_accels(list: &[MenuNode]) -> Vec<Option<usize>> {
    let mut used: Vec<char> = Vec::new();
    let mut out = Vec::with_capacity(list.len());
    for node in list {
        let label = match node {
            MenuNode::Item { label, .. } | MenuNode::Sub { label, .. } => label.as_str(),
            MenuNode::Sep => {
                out.push(None);
                continue;
            }
        };
        let mut assigned = None;
        for (bi, ch) in label.char_indices() {
            if ch.is_ascii_alphanumeric() {
                let lc = ch.to_ascii_lowercase();
                if !used.contains(&lc) {
                    used.push(lc);
                    assigned = Some(bi);
                    break;
                }
            }
        }
        out.push(assigned);
    }
    out
}

/// Index of the node in `list` whose accelerator letter is `ch` (case-insensitive), given the
/// level's `accels` (from [`level_accels`]). `None` if no item claims that letter.
fn accel_match(list: &[MenuNode], accels: &[Option<usize>], ch: char) -> Option<usize> {
    let lc = ch.to_ascii_lowercase();
    accels.iter().enumerate().find_map(|(i, &a)| {
        let bi = a?;
        let accel_ch = list[i].label()[bi..].chars().next()?;
        (accel_ch.to_ascii_lowercase() == lc).then_some(i)
    })
}

/// A menu label as egui text with its accelerator character underlined (or plain when `accel` is
/// `None`). Built as a `LayoutJob` so a single char carries the underline; colour/font match the
/// surrounding chrome (`override_text_color` from the user's `UIFGCOLOR`).
fn accel_label(ui: &egui::Ui, label: &str, accel: Option<usize>) -> egui::WidgetText {
    let Some(bi) = accel else { return label.into() };
    let ch_len = label[bi..].chars().next().map(|c| c.len_utf8()).unwrap_or(0);
    if ch_len == 0 {
        return label.into();
    }
    let color = ui.visuals().text_color();
    let font_id = egui::TextStyle::Button.resolve(ui.style());
    let plain = egui::TextFormat { font_id: font_id.clone(), color, ..Default::default() };
    let under = egui::TextFormat {
        font_id,
        color,
        underline: egui::Stroke::new(1.0, color),
        ..Default::default()
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(&label[..bi], 0.0, plain.clone());
    job.append(&label[bi..bi + ch_len], 0.0, under);
    job.append(&label[bi + ch_len..], 0.0, plain);
    job.into()
}

/// The result of feeding a key to [`MenuNav`].
enum NavResult {
    /// State changed (or nothing happened); keep the menu open.
    None,
    /// An item was chosen — run this and close the menu.
    Activate(MenuAct),
    /// Close the menu (Esc at the top level).
    Close,
}

/// Keyboard navigation state over a [`MenuNode`] tree. `path[0]` is the open top-level menu; each
/// further index is the highlighted entry inside a successively-opened submenu. While active the
/// path is always length ≥ 2 (a top menu is open with one item highlighted).
#[derive(Clone, Default, PartialEq, Debug)]
struct MenuNav {
    path: Vec<usize>,
}

impl MenuNav {
    /// Open `top` with its first selectable item highlighted.
    fn open(tree: &[MenuNode], top: usize) -> MenuNav {
        let item = tree.get(top).and_then(|t| t.children()).map(first_selectable).unwrap_or(0);
        MenuNav { path: vec![top, item] }
    }

    /// The sibling list containing the highlighted node (the deepest open level).
    fn level<'a>(&self, tree: &'a [MenuNode]) -> Option<&'a [MenuNode]> {
        let mut list = tree.get(*self.path.first()?)?.children()?;
        for &i in &self.path[1..self.path.len().saturating_sub(1)] {
            list = list.get(i)?.children()?;
        }
        Some(list)
    }

    /// The currently highlighted node.
    fn highlighted<'a>(&self, tree: &'a [MenuNode]) -> Option<&'a MenuNode> {
        self.level(tree)?.get(*self.path.last()?)
    }

    /// Up/Down: move the highlight to the next selectable sibling, wrapping, skipping separators.
    fn move_sel(&mut self, tree: &[MenuNode], dir: isize) {
        let Some(list) = self.level(tree) else { return };
        let n = list.len();
        if n == 0 {
            return;
        }
        let mut idx = *self.path.last().unwrap() as isize;
        for _ in 0..n {
            idx = (idx + dir).rem_euclid(n as isize);
            if !list[idx as usize].is_sep() {
                break;
            }
        }
        *self.path.last_mut().unwrap() = idx as usize;
    }

    /// Right: open the highlighted submenu, or (on a leaf) move to the next top-level menu.
    fn go_right(&mut self, tree: &[MenuNode]) -> NavResult {
        if let Some(node) = self.highlighted(tree) {
            if let Some(children) = node.children() {
                self.path.push(first_selectable(children));
                return NavResult::None;
            }
        }
        self.switch_top(tree, 1);
        NavResult::None
    }

    /// Left: close the current submenu, or (at the top dropdown) move to the previous top menu.
    fn go_left(&mut self, tree: &[MenuNode]) -> NavResult {
        if self.path.len() > 2 {
            self.path.pop();
        } else {
            self.switch_top(tree, -1);
        }
        NavResult::None
    }

    fn switch_top(&mut self, tree: &[MenuNode], dir: isize) {
        if tree.is_empty() {
            return;
        }
        let next = (self.path[0] as isize + dir).rem_euclid(tree.len() as isize) as usize;
        *self = MenuNav::open(tree, next);
    }

    /// Enter: open the highlighted submenu, or activate the highlighted item.
    fn activate(&mut self, tree: &[MenuNode]) -> NavResult {
        match self.highlighted(tree) {
            Some(MenuNode::Sub { children, .. }) => {
                self.path.push(first_selectable(children));
                NavResult::None
            }
            Some(MenuNode::Item { act, .. }) => NavResult::Activate(act.clone()),
            _ => NavResult::None,
        }
    }

    /// Esc: back out one level, or close the menu when at the top dropdown.
    fn escape(&mut self) -> NavResult {
        if self.path.len() > 2 {
            self.path.pop();
            NavResult::None
        } else {
            NavResult::Close
        }
    }
}

/// The ✔ / blank prefix for a toggle item's label (U+2714 — see [`m_tog`] for why not U+2713).
fn tick(on: bool) -> &'static str {
    if on {
        "✔ "
    } else {
        "   "
    }
}

/// `Document ▸ Text encoding ▸ Display as` — one ✓-toggle per catalog encoding (reinterprets bytes).
fn encoding_display_items(cur: &str) -> Vec<MenuNode> {
    xyscript_app::encoding::CATALOG
        .iter()
        .map(|&(name, label)| m_tog(label, cur == name, format!("ENCODING {name}")))
        .collect()
}

/// `Document ▸ Text encoding ▸ Convert to (new file)` — pick a destination, write a converted copy.
fn encoding_convert_items() -> Vec<MenuNode> {
    xyscript_app::encoding::CATALOG
        .iter()
        .map(|&(name, _label)| {
            m_act(
                _label,
                MenuAct::PickRun { save: true, filter: None, template: format!("ENCODING/C {name} {{}}"), dir: None },
            )
        })
        .collect()
}

/// A `<xx>.lang` UI-translation catalog found on disk, for the `Settings ▸ Language` menu.
#[derive(Clone)]
struct LangChoice {
    /// Endonym shown in the menu — the catalog's own native name (e.g. "Français"), or the bare
    /// language code when the file has no readable header.
    name: String,
    /// Path to hand to `LANG` (forward-slashed so it drops cleanly into a command string).
    cmd_path: String,
    /// Canonicalized path, compared against `Editor::lang_path` to tick the active catalog.
    canon: String,
}

/// First letter upper, the rest lower (Unicode-aware): a header's all-caps "FRANÇAIS" → "Français".
fn titlecase(s: &str) -> String {
    let mut cs = s.chars();
    match cs.next() {
        Some(c) => c.to_uppercase().chain(cs.flat_map(char::to_lowercase)).collect(),
        None => String::new(),
    }
}

/// A catalog's endonym, read from its header line (`# XyScript UI strings — FRANÇAIS (French)`) and
/// title-cased. `None` if the file can't be read or has no such `—`-delimited native name.
fn lang_endonym(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines().take(12) {
        if let Some((_, rest)) = line.split_once('—') {
            let native = rest.split('(').next().unwrap_or("").trim();
            if !native.is_empty() {
                return Some(titlecase(native));
            }
        }
    }
    None
}

/// Non-language `<xx>.lang` files to keep out of the picker: the generated English source (English
/// is the menu's built-in entry) and the pseudolocale (a layout smoke test, not a real language).
const NON_LANGUAGE_LANGS: &[&str] = &["en", "pseudo"];

/// Scan `dirs` for loadable `<xx>.lang` catalogs, skipping [`NON_LANGUAGE_LANGS`]. Deduped by file
/// stem — the first directory to provide a code wins — and sorted by display name so the menu order
/// is stable.
fn discover_langs(dirs: &[std::path::PathBuf]) -> Vec<LangChoice> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<LangChoice> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("lang") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            // `<code>.local.lang` is a user OVERRIDE layered onto a base catalog, not a language of
            // its own (its stem is e.g. "fi.local") — keep it out of the picker.
            if stem.rsplit('.').next().is_some_and(|s| s.eq_ignore_ascii_case("local"))
                || NON_LANGUAGE_LANGS.iter().any(|s| stem.eq_ignore_ascii_case(s))
                || !seen.insert(stem.to_ascii_lowercase())
            {
                continue;
            }
            let name = lang_endonym(&path).unwrap_or_else(|| stem.to_uppercase());
            let canon = std::fs::canonicalize(&path)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| path.to_string_lossy().into_owned());
            out.push(LangChoice {
                name,
                cmd_path: path.to_string_lossy().replace('\\', "/"),
                canon,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
mod menu_tests {
    use super::*;

    #[test]
    fn discover_langs_skips_english_reads_endonyms_and_dedups() {
        let base = std::env::temp_dir().join(format!("xyscript-langdisc-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("fr.lang"), "# XyScript UI strings — FRANÇAIS (French)\nmenubar.file = Fichier\n").unwrap();
        std::fs::write(base.join("de.lang"), "# XyScript UI strings — DEUTSCH (German)\n").unwrap();
        std::fs::write(base.join("xx.lang"), "no header here\n").unwrap(); // falls back to the code
        std::fs::write(base.join("en.lang"), "# English source\n").unwrap(); // skipped — English is built in
        std::fs::write(base.join("pseudo.lang"), "# — PSEUDO (smoke test)\n").unwrap(); // skipped — not a language
        std::fs::write(base.join("fi.local.lang"), "# — SUOMI\n").unwrap(); // skipped — user override, not a language
        std::fs::write(base.join("notes.txt"), "not a catalog").unwrap(); // ignored — not .lang

        let got = discover_langs(&[base.clone()]);
        let names: Vec<_> = got.iter().map(|c| c.name.as_str()).collect();
        // en.lang + pseudo.lang + *.local.lang skipped, .txt ignored; endonyms title-cased; missing
        // header → uppercased code; name-sorted.
        assert_eq!(names, ["Deutsch", "Français", "XX"]);
        // Same dir listed twice → deduped by stem (first wins), not doubled.
        assert_eq!(discover_langs(&[base.clone(), base.clone()]).len(), 3);
    }

    /// Deeper menu columns must sit in a STRICTLY higher egui layer than their parent, so a submenu
    /// can never be painted under the column that spawned it (the field report was
    /// `Settings ▸ Appearance ▸ Colors` at ~897 px, Colors half-covered by Appearance). `Order` is
    /// the primary key egui sorts layers by, so this ordering is what actually guarantees it.
    #[test]
    fn deeper_menu_columns_paint_in_higher_layers() {
        let (c0, c1, c2) = (menu_col_order(0), menu_col_order(1), menu_col_order(2));
        assert!(c0 < c1, "column 1 must be above column 0");
        assert!(c1 < c2, "column 2 must be above column 1");
        // Never below Foreground, so a column can't fall behind an `egui::Window`.
        assert!(c0 >= egui::Order::Foreground);
        // Fixed per depth: a column's layer must not change between frames.
        assert_eq!(menu_col_order(2), menu_col_order(3));
    }

    /// The crash log's human-readable stamp. Tests run with the default (unset) local offset, i.e.
    /// UTC, so this is deterministic. The epoch here is a real crash report from a tester.
    #[test]
    fn crash_log_timestamp_is_human_readable_with_its_utc_offset() {
        // 1784430921 s == 2026-07-19 03:15:21 UTC.
        assert_eq!(local_timestamp(1_784_430_921_000), "2026-07-19 03:15:21 +00:00");
        // Sub-second input truncates rather than rounding up, and the epoch itself is sane.
        assert_eq!(local_timestamp(1_784_430_921_999), "2026-07-19 03:15:21 +00:00");
        assert_eq!(local_timestamp(0), "1970-01-01 00:00:00 +00:00");
    }

    #[test]
    fn leading_glyph_cursor_anchors_on_the_char_under_the_point() {
        // Drag-select anchoring: a press ANYWHERE within a character's cell must anchor at that
        // character's leading edge (so it's included) — the fix for "first char sometimes dropped".
        let mut fs = FontSystem::new();
        let mut buffer = Buffer::new(&mut fs, Metrics::new(16.0, 20.0));
        buffer.set_size(Some(2000.0), Some(200.0));
        buffer.set_text("hello", &Attrs::new().family(Family::Monospace), Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut fs, false);
        let (g0x, g0w, g1x, g1w, y) = {
            let run = buffer.layout_runs().next().expect("one run");
            let g0 = &run.glyphs[0];
            let g1 = &run.glyphs[1];
            (g0.x, g0.w, g1.x, g1.w, run.line_top + run.line_height * 0.5)
        };
        // Far RIGHT of the first glyph (well past its midpoint) still anchors at 0 — the whole point.
        assert_eq!(leading_glyph_cursor(&buffer, g0x + g0w * 0.9, y).map(|c| c.index), Some(0));
        // Left edge of the first glyph also anchors at 0.
        assert_eq!(leading_glyph_cursor(&buffer, g0x + g0w * 0.1, y).map(|c| c.index), Some(0));
        // A point on the SECOND glyph anchors at 1, not 0.
        assert_eq!(leading_glyph_cursor(&buffer, g1x + g1w * 0.5, y).map(|c| c.index), Some(1));
        // Off the row (above it) → no containing glyph (caller falls back to nearest-boundary hit).
        assert_eq!(leading_glyph_cursor(&buffer, g0x + g0w * 0.5, -100.0), None);
        // TRAILING edge (leftward-drag anchor): a press anywhere on glyph 0 anchors at its END (1),
        // so a drag to the left still encloses char 0. Glyph 1's trailing edge is 2.
        assert_eq!(glyph_edge_cursor(&buffer, g0x + g0w * 0.1, y, true).map(|c| c.index), Some(1));
        assert_eq!(glyph_edge_cursor(&buffer, g1x + g1w * 0.5, y, true).map(|c| c.index), Some(2));
    }

    #[test]
    fn truncate_path_fills_width_keeping_drive_and_tail() {
        let p = r"C:\Users\cldis\rusted\xyscript-gui\src"; // 38 chars
        assert_eq!(truncate_path(p, 100), p); // within budget → unchanged
        // Fill the width: drive (2) + "..." (3) + last (n-5) chars = exactly n.
        let t = truncate_path(p, 18);
        assert_eq!(t.chars().count(), 18);
        assert!(t.starts_with("C:..."), "keeps the drive + ellipsis: {t}");
        assert!(t.ends_with(r"\src"), "final subdir always visible: {t}");
        assert_eq!(t, r"C:...cript-gui\src");
        // Degenerate narrow width: just the last `n` chars.
        assert_eq!(truncate_path(p, 4), r"\src");
    }

    #[test]
    fn visual_stop_pick_moves_by_screen_position_across_bidi() {
        // Pure LTR "abc": three 10px glyphs at x=0,10,20; logical index == visual position.
        // (is_ltr, x, w, start, end)
        let ltr = [(true, 0.0, 10.0, 0, 1), (true, 10.0, 10.0, 1, 2), (true, 20.0, 10.0, 2, 3)];
        let idx = |o: Option<(usize, f32)>| o.map(|(i, _)| i);
        // Caret between a and b sits at x=10. Right → index 2 (x=20); Left → index 0 (x=0).
        assert_eq!(visual_stop_pick(10.0, true, &ltr), Some((2, 20.0)));
        assert_eq!(idx(visual_stop_pick(10.0, false, &ltr)), Some(0));
        // At the far right (x=30) there's nothing further right; left → index 2 (x=20).
        assert_eq!(visual_stop_pick(30.0, true, &ltr), None);
        assert_eq!(idx(visual_stop_pick(30.0, false, &ltr)), Some(2));

        // Pure RTL logical "AB" (A=idx0, B=idx1) renders B A: glyphs in VISUAL order are B (left,
        // x=0) then A (right, x=10). RTL glyph's start is on its RIGHT, end on its left.
        let rtl = [(false, 0.0, 10.0, 1, 2), (false, 10.0, 10.0, 0, 1)];
        // Caret at logical index 0 (= start of A) is visually rightmost (x=20). Pressing LEFT must move
        // the caret to the LEFT on screen → the next stop down in x is index 1 (x=10). This is exactly
        // the reported bug: with the old code CL did nothing here.
        assert_eq!(idx(visual_stop_pick(20.0, false, &rtl)), Some(1));
        // And RIGHT from there is nothing (already at the visual right edge).
        assert_eq!(visual_stop_pick(20.0, true, &rtl), None);
        // From the visual-left edge (x=0, logical index 2 = end of B): RIGHT → index 1 (x=10).
        assert_eq!(idx(visual_stop_pick(0.0, true, &rtl)), Some(1));
        assert_eq!(visual_stop_pick(0.0, false, &rtl), None);

        // Number-in-Arabic seam (the reported Tier-3 bug). Visual L→R: C(RTL) 1(LTR) 2(LTR) B(RTL),
        // logical: B=1, {1,2}=digits idx2,3, C=4. Glyphs (is_ltr,x,w,start,end):
        let seam = [
            (false, 0.0, 10.0, 4, 5),  // C : lead(start=4)=10(right), trail(end=5)=0(left)
            (true, 10.0, 10.0, 2, 3),  // '1': lead(2)=10, trail(3)=20
            (true, 20.0, 10.0, 3, 4),  // '2': lead(3)=20, trail(4)=30
            (false, 30.0, 10.0, 1, 2), // B : lead(1)=40, trail(2)=30
        ];
        // THE CRUX (the reported bounce): logical index 4 lives at BOTH x=10 (C, right edge) and x=30
        // ('2' end). Approaching the seam from the number side (carry cx=30) and stepping RIGHT must
        // continue OUTWARD to x=40 (index 1), not bounce back into the digits. The carried visual x is
        // what makes this possible — from the ambiguous index alone it would collapse to x=10 and reverse.
        assert_eq!(visual_stop_pick(30.0, true, &seam), Some((1, 40.0)));
        // Stepping left from between the digits (cx=20) lands at the digit/C seam, visual x=10. Which
        // logical index (the digit's start=2 or C's start=4, both AT x=10) is unspecified — either keeps
        // the caret at the same visual spot, and the carried x=10 makes the next step unambiguous.
        assert_eq!(visual_stop_pick(20.0, false, &seam).map(|(_, x)| x), Some(10.0));
    }

    #[test]
    fn truncate_tab_name_keeps_head_and_extension() {
        // Within budget → unchanged.
        assert_eq!(truncate_tab_name("report.txt", 28), "report.txt");
        // Long name → capped at max, keeping the start and the extension.
        let long = "my-very-long-report-final-v3.txt"; // 32 chars
        let t = truncate_tab_name(long, 28);
        assert_eq!(t.chars().count(), 28);
        assert!(t.starts_with("my-very-long-report"), "keeps the start: {t}");
        assert!(t.ends_with(".txt") && t.contains('…'), "keeps the extension + ellipsis: {t}");
        // No extension → head + ellipsis.
        let noext = truncate_tab_name("abcdefghijklmnopqrstuvwxyz012345", 10);
        assert_eq!(noext.chars().count(), 10);
        assert_eq!(noext, "abcdefghi…");
        // Unicode filename counts by chars, not bytes.
        assert_eq!(truncate_tab_name("café", 28), "café");
    }

    fn tree() -> Vec<MenuNode> {
        vec![
            m_sub("File", vec![m_item("New", "NEW"), MenuNode::Sep, m_item("Quit", "QUIT")]),
            m_sub(
                "Edit",
                vec![
                    m_item("Undo", "UNDO"),
                    m_sub("Case", vec![m_item("Upper", "UC"), m_item("Lower", "LC")]),
                ],
            ),
        ]
    }

    #[test]
    fn accelerators_dedupe_within_a_level_and_match() {
        // Two siblings starting with the same letter get DISTINCT accelerators: the second falls
        // through to its next free letter (mirrors the real top bar's Search/Settings collision).
        let list = vec![m_sub("Search", vec![]), m_sub("Settings", vec![])];
        let ac = level_accels(&list);
        assert_eq!(ac, vec![Some(0), Some(1)], "Search→S(0), Settings→e(1)");
        // Matching is case-insensitive and honours the dedup.
        assert_eq!(accel_match(&list, &ac, 's'), Some(0));
        assert_eq!(accel_match(&list, &ac, 'S'), Some(0));
        assert_eq!(accel_match(&list, &ac, 'e'), Some(1)); // Settings' assigned letter
        assert_eq!(accel_match(&list, &ac, 'z'), None);
        // Separators claim no letter and get no accelerator.
        let list2 = vec![m_item("New", "NEW"), MenuNode::Sep, m_item("Quit", "QUIT")];
        assert_eq!(level_accels(&list2), vec![Some(0), None, Some(0)]);
    }

    #[test]
    fn down_keeps_visual_column_across_a_blank_line() {
        let mut app = GuiApp::new();
        app.editor.run_command(r#"INSERT "first line here""#);
        app.editor.run_command(r#"INSERT "\n\nlast line here""#); // line0, blank line1, line2
        app.editor.run_command("TF");
        app.editor.run_command("JMP +5"); // column 5 of line 0
        app.render_canvas(400, 400); // wide → no wrap
        app.visual_vmove(true, false); // → blank line (visually column 0, goal kept)
        app.render_canvas(400, 400);
        app.visual_vmove(true, false); // → line 2
        app.render_canvas(400, 400);
        let text = app.editor.document().text();
        let line2 = text.rfind("last line here").unwrap();
        assert_eq!(
            app.editor.document().cursor(),
            line2 + 5,
            "column not maintained across the blank line"
        );
    }

    #[test]
    fn down_holds_the_visual_column_within_a_wrapped_line() {
        let mut app = GuiApp::new();
        // One long LOGICAL line (no explicit newlines) that wraps into several visual rows.
        app.editor.run_command(
            r#"INSERT "the quick brown fox jumps over the lazy dog and then keeps running onward and on""#,
        );
        app.editor.run_command("TF");
        app.editor.run_command("JMP +5"); // column 5 on the first visual row
        app.render_canvas(160, 500); // narrow → the line wraps
        assert_eq!(app.editor.host().cursor_visual_col(), Some(5), "setup: at column 5");
        // Step DOWN through several visual rows of the SAME wrapped line; the visual column must stay 5.
        for step in 1..=3 {
            app.visual_vmove(true, false);
            app.render_canvas(160, 500);
            assert_eq!(
                app.editor.host().cursor_visual_col(),
                Some(5),
                "DOWN step {step}: visual column not held across a wrapped row"
            );
        }
        // And back UP holds it too.
        for step in 1..=2 {
            app.visual_vmove(false, false);
            app.render_canvas(160, 500);
            assert_eq!(
                app.editor.host().cursor_visual_col(),
                Some(5),
                "UP step {step}: visual column not held across a wrapped row"
            );
        }
    }

    #[test]
    fn down_holds_the_visual_column_at_the_viewport_edge() {
        let mut app = GuiApp::new();
        app.editor.run_command(
            r#"INSERT "the quick brown fox jumps over the lazy dog and then keeps running onward and on and on further still""#,
        );
        app.editor.run_command("TF");
        app.editor.run_command("JMP +5");
        let h = 90; // only ~2-3 visual rows tall → forces the viewport-edge path on each step
        app.render_canvas(160, h);
        assert_eq!(app.editor.host().cursor_visual_col(), Some(5), "setup");
        for step in 1..=6 {
            app.visual_vmove(true, false);
            app.render_canvas(160, h);
            assert_eq!(
                app.editor.host().cursor_visual_col(),
                Some(5),
                "DOWN step {step}: column lost at the viewport edge (fell back to a logical move)"
            );
        }
    }

    #[test]
    fn down_from_the_end_of_a_wrapped_row_advances() {
        let mut app = GuiApp::new();
        app.editor.run_command(
            r#"INSERT "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima""#,
        );
        app.editor.run_command(r#"INSERT "\nsecond line""#);
        app.editor.run_command("TF");
        app.render_canvas(200, 500); // narrow → line 0 wraps into several rows
                                     // Put the caret at the visual END of the first wrapped row.
        let from = app.editor.document().cursor();
        app.visual_line_edge(true, false, from);
        app.render_canvas(200, 500);
        let before = app.editor.document().cursor();
        app.visual_vmove(true, false); // DOWN from the end of the wrapped row
        app.render_canvas(200, 500);
        assert_ne!(app.editor.document().cursor(), before, "DOWN wedged at the end of a wrapped row");
    }

    #[test]
    fn bound_line_motion_verbs_route_to_the_visual_mover() {
        let mut app = GuiApp::new();
        app.editor
            .run_command(r#"INSERT "alpha bravo charlie delta echo foxtrot golf hotel india juliet""#);
        app.editor.run_command(r#"INSERT "\nsecond""#);
        app.editor.run_command("TF");
        app.render_canvas(200, 500); // narrow → line 0 wraps
        let before = app.editor.document().cursor();
        // A key bound to `LD` moves by VISUAL row (advances within the wrapped line), not by logical
        // line — so it returns true and the caret advances.
        assert!(app.try_visual_vmotion("LD"));
        app.render_canvas(200, 500);
        assert!(app.editor.document().cursor() > before, "bound LD should step by visual row");
        // Non-motion verbs, chains, and arg forms are NOT redirected (run as written).
        assert!(!app.try_visual_vmotion("PR hi"));
        assert!(!app.try_visual_vmotion("LD ; LD"));
        assert!(!app.try_visual_vmotion("DOWN 3"));
        // In the command line, motion is NOT redirected (the verb moves the cmline caret).
        app.editor.run_command("GH");
        assert!(!app.try_visual_vmotion("LD"));
    }

    #[test]
    fn up_down_hold_the_visual_goal_column_and_a_break_clears_it() {
        let mut app = GuiApp::new();
        // Long line, short line, long line.
        app.editor.run_command(r#"INSERT "aaaaaaaaaa""#);
        app.editor.run_command(r#"INSERT "\nbb\ncccccccccc""#);
        let l2 = app.editor.document().text().rfind("cccccccccc").unwrap();
        app.editor.run_command("TF");
        app.editor.run_command("JMP +5"); // column 5 of line 0
        app.render_canvas(400, 400);
        // DOWN onto the short line clamps to its end but HOLDS the goal (column 5).
        app.visual_vmove(true, false);
        app.render_canvas(400, 400);
        assert!(app.goal_x.is_some(), "the goal column is held across a short line");
        // DOWN onto the long line RESUMES column 5 — the goal was maintained.
        app.visual_vmove(true, false);
        app.render_canvas(400, 400);
        assert_eq!(app.editor.document().cursor(), l2 + 5, "resumes the memorized column");
        // A bound horizontal move (via `run_captured`, the keymap path) is a BREAK: it clears the goal.
        app.run_captured("EL"); // go to line start (column 0)
        assert!(app.goal_x.is_none(), "a horizontal move clears the sticky goal column");
        app.render_canvas(400, 400);
        // The next DOWN now uses the NEW column (0), not the stale 5 — nothing to move down to here
        // (last line), but the goal must have re-based to the current column, not resurrected 5.
        app.editor.run_command("TF");
        app.editor.run_command("JMP +5");
        app.render_canvas(400, 400);
        app.run_captured("NC"); // one char right → column 6, clears goal
        assert!(app.goal_x.is_none());
    }

    #[test]
    fn split_composites_two_panes_and_restores_active() {
        let mut app = GuiApp::new();
        app.editor.run_command(r#"INSERT "left pane text""#);
        app.editor.run_command("NEW/V"); // side-by-side split; the new empty buffer takes focus
        assert!(app.editor.host().pane_count() >= 2);
        let focused_doc = app.editor.active_index();
        // The composite render must produce a full-size image and NOT leave `active` pointing at some
        // other pane's doc (the swap-state loop restores it).
        let s = app.render_scale;
        let img = app.render_canvas_panes(400, 300);
        assert_eq!(
            img.size,
            [(400.0 * s).round() as usize, (300.0 * s).round() as usize]
        );
        assert_eq!(app.editor.active_index(), focused_doc, "active must be restored to the focused pane");
        // One resizable divider between the two panes.
        let divs = app.editor.host().pane_dividers(400.0, 300.0);
        assert_eq!(divs.len(), 1);
        app.editor.host_mut().set_pane_ratio(divs[0].index, 0.3); // must not panic
        // Collapse back to a single pane; the single-pane render path still works.
        app.editor.run_command("TUS");
        assert_eq!(app.editor.host().pane_count(), 1);
        let _ = app.render_canvas(400, 300);
    }

    #[test]
    #[cfg(feature = "lint")]
    fn spelling_underline_waits_for_word_completion() {
        // The word being typed (ending at the caret, no separator yet) must NOT be underlined; once a
        // separator is typed and the word is finished, its underline appears.
        let mut app = GuiApp::new();
        app.editor.run_command("SPELLING on");
        app.editor.insert_str("recieve"); // a misspelling, caret parked at its end (still "typing")
        app.editor.recheck_lints(0, 10_000);
        assert!(!app.editor.lints().is_empty(), "harper should flag 'recieve'");
        let (buffer, win) = app.windowed_buffer(400, 300);
        app.lint_focus_doc = Some(app.editor.active_index());
        let typing = app.lint_underline_rects(&buffer, &win, 400, 16);
        assert!(
            !typing.iter().any(|r| r.4),
            "no spelling underline while the word is still being typed"
        );
        // Finish the word with a separator → the underline is no longer deferred.
        app.editor.insert_str(" ");
        app.editor.recheck_lints(0, 10_000);
        let (buffer, win) = app.windowed_buffer(400, 300);
        app.lint_focus_doc = Some(app.editor.active_index());
        let done = app.lint_underline_rects(&buffer, &win, 400, 16);
        assert!(
            done.iter().any(|r| r.4),
            "spelling underline appears once a separator finishes the word"
        );
    }

    #[test]
    #[cfg(feature = "lint")]
    fn grammar_underline_also_waits_for_word_completion() {
        // Same deferral as spelling, but for a GRAMMAR mark (rect flag `false`): the repeated word is
        // a grammar issue, and its underline must not show while the second word is still being typed.
        let mut app = GuiApp::new();
        app.editor.run_command("GRAMMAR on");
        app.editor.insert_str("the the"); // repeated word — a non-spelling (grammar) issue
        app.editor.recheck_lints(0, 10_000);
        assert!(
            app.editor.lints().iter().any(|m| !m.is_spelling()),
            "harper should flag the repeated word as grammar"
        );
        // Caret is at the end of the second "the" (still typing it) → its underline is deferred.
        let (buffer, win) = app.windowed_buffer(400, 300);
        app.lint_focus_doc = Some(app.editor.active_index());
        let typing = app.lint_underline_rects(&buffer, &win, 400, 16);
        assert!(
            !typing.iter().any(|r| !r.4),
            "no grammar underline while the word is still being typed"
        );
        // Finish the word → the grammar underline appears.
        app.editor.insert_str(" ");
        app.editor.recheck_lints(0, 10_000);
        let (buffer, win) = app.windowed_buffer(400, 300);
        app.lint_focus_doc = Some(app.editor.active_index());
        let done = app.lint_underline_rects(&buffer, &win, 400, 16);
        assert!(
            done.iter().any(|r| !r.4),
            "grammar underline appears once a separator finishes the word"
        );
    }

    #[test]
    fn find_landing_from_top_vs_forward_from_origin() {
        let matches = [(10usize, 12usize), (50, 52), (300, 302)];
        // From top ON: always the first match, regardless of the origin.
        assert_eq!(super::find_landing_target(true, &matches, 999), Some(10));
        assert_eq!(super::find_landing_target(true, &matches, 100), Some(10));
        // From top OFF: the first match AT OR AFTER the fixed origin — forward, never toward the top.
        assert_eq!(super::find_landing_target(false, &matches, 0), Some(10));
        assert_eq!(super::find_landing_target(false, &matches, 11), Some(50)); // past the first
        assert_eq!(super::find_landing_target(false, &matches, 50), Some(50)); // origin AT a match → stay
        assert_eq!(super::find_landing_target(false, &matches, 300), Some(300));
        // Nothing at/after the origin → wrap to the first (all remaining matches are above it).
        assert_eq!(super::find_landing_target(false, &matches, 999), Some(10));
        // No matches → nothing either way.
        assert_eq!(super::find_landing_target(true, &[], 0), None);
        assert_eq!(super::find_landing_target(false, &[], 0), None);
    }

    #[test]
    fn clip_ws_span_trims_whitespace_edges() {
        // The draw-time whitespace clip that keeps a stale/misaligned lint mark from underlining a
        // just-typed space or newline (the <space>/<enter> flash).
        let mut app = GuiApp::new();
        app.editor.insert_str("  hello world  "); // 0,1 sp | 2-6 hello | 7 sp | 8-12 world | 13,14 sp
        let doc = app.editor.document();
        // Leading + trailing whitespace stripped down to the "hello world" core.
        assert_eq!(clip_ws_span(doc, 0, 15), (2, 13));
        // Internal whitespace is preserved (a legitimate multi-word span).
        assert_eq!(clip_ws_span(doc, 2, 13), (2, 13));
        // An all-whitespace span collapses to empty (so it draws nothing).
        let (s, e) = clip_ws_span(doc, 0, 2);
        assert_eq!(s, e, "all-whitespace span is empty");
        // Stale offsets past the end are clamped, never a panic.
        let (s, e) = clip_ws_span(doc, 6, 9999);
        assert!(s <= e && e <= doc.len());
    }

    #[test]
    fn every_menu_command_is_a_real_verb() {
        // The menus are thin renderers of XPL verbs — guard against a typo'd command string (in any
        // item, incl. the split-pane / language-check / coding-aids additions) by checking every
        // `Run` command's leading token against the authoritative verb list.
        use xyscript_app::{BUILTIN_VERBS, CONTROL_KEYWORDS};
        fn walk<'a>(nodes: &'a [MenuNode], out: &mut Vec<&'a str>) {
            for n in nodes {
                match n {
                    MenuNode::Item { act: MenuAct::Run(cmd), .. } => out.push(cmd.as_str()),
                    MenuNode::Sub { children, .. } => walk(children, out),
                    _ => {}
                }
            }
        }
        let verbs: std::collections::HashSet<&str> =
            BUILTIN_VERBS.iter().chain(CONTROL_KEYWORDS).copied().collect();
        let app = GuiApp::new();
        let tree = app.menu_tree();
        let mut cmds = Vec::new();
        walk(&tree, &mut cmds);
        assert!(cmds.len() > 30, "menu tree looks empty ({} run-items)", cmds.len());
        for cmd in cmds {
            // Peel any attached switch (`LEARN/S` → `LEARN`) before checking the base verb.
            let verb = cmd.split_whitespace().next().unwrap_or("").split('/').next().unwrap_or("");
            assert!(verbs.contains(verb), "menu runs unknown verb {verb:?} (from {cmd:?})");
        }
    }

    #[test]
    fn menu_labels_stay_within_width() {
        // Menu columns render with `TextWrapMode::Extend`, so a label never WRAPS — the column widens
        // to fit it. This guard keeps that widening sane: no item (any submenu depth) may exceed a
        // budget that comfortably fits a readable column. A submenu item also shows a "    ⏵" marker,
        // so it's charged 5 extra. Counts CHARS (so "…"/"⏵"/"✔" count as one), matching visual width.
        const MAX: usize = 40;
        let app = GuiApp::new();
        for (label, width) in menu_label_widths(&app.menu_tree()) {
            assert!(width <= MAX, "menu label {label:?} is {width} chars (> {MAX}) — it may wrap / over-widen");
        }
    }

    /// Collect every menu label with its rendered char-width — an item counts its own chars; a
    /// submenu is charged +5 for the trailing "    ⏵" marker. Shared by the English and translated
    /// width guards.
    fn menu_label_widths(tree: &[MenuNode]) -> Vec<(String, usize)> {
        fn walk(nodes: &[MenuNode], out: &mut Vec<(String, usize)>) {
            for n in nodes {
                match n {
                    MenuNode::Item { label, .. } => out.push((label.clone(), label.chars().count())),
                    MenuNode::Sub { label, children } => {
                        out.push((label.clone(), label.chars().count() + 5));
                        walk(children, out);
                    }
                    MenuNode::Sep => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(tree, &mut out);
        out
    }

    #[test]
    fn translated_menu_labels_stay_within_width() {
        // Translations run longer than English — German/Dutch compounds especially (e.g. "Klammern und
        // Anführungszeichen automatisch schließen") — so the column-width budget is looser than the
        // 40-char English guard. It's still bounded, to catch a runaway label (a bad or comically
        // verbose translation) before it ships an unusably wide menu column. EVERY shipped catalog is
        // discovered and checked, so a new <xx>.lang is covered automatically. If a legitimate
        // translation ever exceeds this, shorten it or raise MAX deliberately.
        const MAX: usize = 62;
        let i18n = format!("{}/../docs/i18n", env!("CARGO_MANIFEST_DIR").replace('\\', "/"));
        let mut checked = 0;
        for entry in std::fs::read_dir(&i18n).expect("docs/i18n is readable") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|x| x.to_str()) != Some("lang") {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            // Skip the English source and the pseudolocale — same exclusions as the Language menu.
            if NON_LANGUAGE_LANGS.iter().any(|s| stem.eq_ignore_ascii_case(s)) {
                continue;
            }
            let mut app = GuiApp::new();
            app.editor.run_command(&format!("LANG {}", path.to_string_lossy().replace('\\', "/")));
            assert!(
                app.editor.lang_get("menubar.file").is_some(),
                "{stem}.lang did not load — the menu would silently fall back to English, hiding width problems"
            );
            for (label, width) in menu_label_widths(&app.menu_tree()) {
                assert!(
                    width <= MAX,
                    "{stem}: menu label {label:?} is {width} chars (> {MAX}) — the column would over-widen"
                );
            }
            checked += 1;
        }
        assert!(checked >= 6, "expected at least the six sample catalogs; only checked {checked}");
    }

    #[test]
    fn opens_first_selectable_and_navigates_items() {
        let t = tree();
        let mut nav = MenuNav::open(&t, 0);
        assert_eq!(nav.path, vec![0, 0]); // File ▸ New
        nav.move_sel(&t, 1); // skips the separator → Quit
        assert_eq!(nav.path, vec![0, 2]);
        nav.move_sel(&t, 1); // wraps back to New
        assert_eq!(nav.path, vec![0, 0]);
        nav.move_sel(&t, -1); // wraps up, skipping the separator → Quit
        assert_eq!(nav.path, vec![0, 2]);
    }

    #[test]
    fn left_right_switch_top_menus_and_open_submenus() {
        let t = tree();
        let mut nav = MenuNav::open(&t, 0);
        nav.go_right(&t); // on a leaf (New) → next top menu (Edit)
        assert_eq!(nav.path, vec![1, 0]); // Edit ▸ Undo
        nav.move_sel(&t, 1); // → Case submenu item
        assert_eq!(nav.path, vec![1, 1]);
        nav.go_right(&t); // Case is a Sub → open it
        assert_eq!(nav.path, vec![1, 1, 0]); // Case ▸ Upper
        nav.go_left(&t); // close the submenu
        assert_eq!(nav.path, vec![1, 1]);
        nav.go_left(&t); // at the top dropdown → previous top menu (File), wraps within 2
        assert_eq!(nav.path[0], 0);
    }

    #[test]
    fn enter_activates_items_and_opens_submenus_escape_backs_out() {
        let t = tree();
        let mut nav = MenuNav::open(&t, 1); // Edit ▸ Undo
        match nav.activate(&t) {
            NavResult::Activate(MenuAct::Run(c)) => assert_eq!(c, "UNDO"),
            _ => panic!("expected Undo to activate"),
        }
        // Descend into Case, activate Lower.
        let mut nav = MenuNav { path: vec![1, 1] }; // Edit ▸ Case
        assert!(matches!(nav.activate(&t), NavResult::None)); // opens the submenu
        assert_eq!(nav.path, vec![1, 1, 0]);
        nav.move_sel(&t, 1); // → Lower
        match nav.activate(&t) {
            NavResult::Activate(MenuAct::Run(c)) => assert_eq!(c, "LC"),
            _ => panic!("expected Lower"),
        }
        // Esc backs out of the submenu, then closes.
        let mut nav = MenuNav { path: vec![1, 1, 0] };
        assert!(matches!(nav.escape(), NavResult::None));
        assert_eq!(nav.path, vec![1, 1]);
        assert!(matches!(nav.escape(), NavResult::Close));
    }
}

/// The slice of the document currently handed to cosmic — a **window** of whole logical
/// lines around the viewport (plus margin), so render cost is `O(window)`, not
/// `O(document)`, regardless of file size. cosmic line `i` is global logical line
/// `start_line + i`. Offsets map to/from the rope through `start_decoded` (the global
/// decoded offset of the window's first byte) and the window-local `\r`/`\n` indices.
struct DocWindow {
    start_line: usize,
    /// Rope byte offset of the window's first line. Paired with `start_decoded`, this is a
    /// KNOWN (rope, decoded) correspondence the offset translators anchor to — so mapping a
    /// window-local offset to/from a global rope offset walks only the window (~a page), never
    /// from a document-global checkpoint (which is O(file) at the bottom of a big non-UTF-8 doc).
    start_byte: usize,
    start_decoded: usize,
    decoded_len: usize,
    at_eof: bool,
    display: Rc<str>,
    cr: Rc<[usize]>,
    nl: Rc<[usize]>,
}

/// A `MSG` message box on screen: the text, and an optional deadline after which it
/// self-dismisses (`MSG /T:<secs>`).
struct MessageBox {
    text: String,
    dismiss_at: Option<std::time::Instant>,
    /// `true` for a bare-`MSG` box that PARKED the script — dismissing it must resume the script.
    /// `false` for a `MSG/T:` toast (fire-and-forget; just clear it).
    blocking: bool,
}

/// The open font-picker dialog (`EDFONTS`/`UIFONTS`/`TXTFONTS`). GUI sugar over the family + size
/// setters: on OK it runs the target's `*FAMILY` + `*FONT` verbs; Cancel/× just closes (nothing is
/// applied while browsing — the live preview is a separate egui family, not the real setting).
struct FontPicker {
    kind: FontPickerKind,
    /// Candidate family names (sorted, deduped; monospaced-only for `Editor`).
    families: Vec<String>,
    /// The highlighted family (also what the preview renders in).
    selected: String,
    /// The chosen size in points (the source of truth applied on OK).
    size: f32,
    /// The size combo's editable text buffer (kept across frames so typing isn't wiped). Parsed
    /// into `size` when it's a valid in-range number; a garbled value just leaves `size` untouched.
    size_text: String,
    /// The chosen style (normal/bold/italic/bolditalic); the preview renders in it too.
    style: FontStyle,
    /// One-shot: scroll the list to `selected` on the next render (set on open). Cleared after,
    /// so the mouse wheel owns the scroll position afterward (else `scroll_to_me` fights it).
    scroll_to_selected: bool,
}

/// What `find_window_ui` reports back to the runtime after a FIND-window frame: whether to close the
/// window, and whether the MAIN document window needs a redraw (a search moved the caret / changed
/// the live highlights, which the main canvas renders).
#[derive(Default)]
pub struct FindOutcome {
    pub close: bool,
    pub main_redraw: bool,
}

/// The Find/Replace dialog (Search menu / `FIND`), rendered in its OWN resizable/movable OS window
/// (`FindWindow`). A thin renderer: each button builds a search through `editor.search_spec(...)`
/// (which constructs a `SearchSpec` directly, so the pattern/replacement are taken literally — no
/// command parsing or `$`-substitution). Find Next = `SE`, Find Prev = `SEB`, Replace All = `CH`;
/// single Replace overwrites the selected match then advances. The document updates live beneath it.
struct FindDialog {
    find: String,
    replace: String,
    case: bool,
    whole_word: bool,
    regex: bool,
    in_selection: bool,
    /// `/K` — recase each replacement to match the case of the text it replaces (Replace / Replace
    /// all only). See `xyscript_app::host::recase_to_match`.
    keep_case: bool,
    /// "From top of file": when on, a search BEGINS at the top — the live type-to-find lands on the
    /// first match in the document (as you type, or when you tick the box) instead of the first match
    /// at/after the search origin. `Find Next` still advances forward from there, so it's a
    /// starting-point mode.
    from_top: bool,
    /// The search ORIGIN for the live type-to-find with "From top" OFF: the caret when the dialog
    /// opened, re-pinned whenever the field goes empty. Landings jump to the first match at/after this
    /// FIXED point rather than the live caret — the caret drifts each keystroke as the preview moves it,
    /// and searching from a moving caret walked results toward the top of the file.
    origin: usize,
    /// "Wrap around": when on, a `Find Next` past the last match wraps to the first (and `Find Prev`
    /// past the first wraps to the last), instead of stopping with "not found".
    wrap: bool,
    /// One-shot: focus the Find field on the next frame (set on open via "Find…").
    focus_find: bool,
    /// One-shot: focus the Replace field on the next frame (set on open via "Replace…").
    focus_replace: bool,
    /// The `from_top` state from the previous frame, so ticking the box re-triggers the landing jump
    /// (it isn't part of `find_win_params`, which gates the text/option landing).
    prev_from_top: bool,
}

/// The non-modal Run-command dialog (Tools ▸ Shell command…). A thin renderer over the shell verbs:
/// Run composes `X` (to the *Shell Output* tab), `X/BG` (background), or `XY` (insert output at the
/// caret) from the field + checkboxes and runs it. Shell-out is `TRUST`-gated, so Run is disabled
/// (with a hint) until trust is on.
struct RunCmdDialog {
    cmd: String,
    background: bool,
    insert_output: bool,
    /// One-shot: focus the command field on the next frame (set on open).
    focus_cmd: bool,
}

/// The GREP dialog (Search ▸ Grep…), a companion to the FIND window. A thin renderer: Search dispatches
/// `EditorOp::Grep` DIRECTLY via `editor.run_grep` (the pattern is literal — no command parsing or
/// `$`-substitution), and the matches stream into a read-only results tab exactly as if `GREP` were
/// typed. The flags mirror the `GREP` switches: `regex` (`/R`), `ignore_case` (`/I`), `whole` (`/W`).
struct GrepDialog {
    pattern: String,
    /// Optional search root/glob (a directory, `*.rs`, or `dir/*.rs`); blank = `$CWD`.
    path: String,
    regex: bool,
    ignore_case: bool,
    whole: bool,
    /// One-shot: focus the pattern field on the next frame (set on open).
    focus_pattern: bool,
}

/// Per-pane view state: the scroll anchor + follow flag + canvas size that logically belong to
/// "one viewport". Today there is exactly one (`GuiApp.pane_view`); split panes (Phase 1+) promote
/// this to one-per-pane. Holding it in its own field lets the render path read/write a viewport's
/// state without it implicitly meaning "the active document".
#[derive(Clone, Default)]
struct PaneView {
    /// Scroll anchor: a document logical line at the top of the viewport (cosmic visual space).
    scroll_line: usize,
    /// Sub-line pixel offset into the wrapped layout of `scroll_line`.
    scroll_vertical: f32,
    /// This frame, snap the caret back into view (set after edits/motion; cleared after a free scroll).
    follow: bool,
    /// Horizontal scroll offset in PIXELS (device space), used only when `WRAP off`. The raster shifts
    /// every glyph/caret/highlight left by this much, and the bottom scrollbar drives it. `0` while
    /// wrapping (there is no horizontal overflow to reach).
    scroll_h: f32,
    /// The widest visible line in pixels this frame (device space), from `layout_runs`. Feeds the
    /// horizontal scrollbar's thumb ratio and clamps `scroll_h`. Only meaningful when `WRAP off`.
    content_w: f32,
    /// Canvas size in PHYSICAL PIXELS (the cosmic/raster layer works in pixels; layout in points).
    canvas_px: (usize, usize),
    /// While a vertical-scrollbar DRAG is in progress, the wrap ratio frozen at drag-start. The
    /// per-frame `wrap_ratio` is sampled from the visible window, so a drag that scrolls into a
    /// denser/sparser-wrapping region shifts it — and since the drag maps pointer→line THROUGH the
    /// ratio, that shift feeds back into the position and the document jitters. Holding the ratio for
    /// the duration of the drag makes the mapping a fixed linear function (smooth); `None` when idle.
    scroll_drag_ratio: Option<f32>,
}

// --- Split-pane compositing helpers (Phase 2) -----------------------------------------------------
// The multi-pane path renders each pane into its own RGBA sub-buffer, then blits them into one big
// canvas and draws the dividers + focus border with these small pixel ops. All buffers are
// premultiplied RGBA (what `ColorImage::from_rgba_premultiplied` wants); panes are opaque, so a blit
// is a straight row copy.

/// Copy a `sw`×`sh` sub-image into `dst` (`dw`×`dh`) at pixel `(ox, oy)`, clipping to `dst`'s bounds.
fn blit_rgba(dst: &mut [u8], dw: usize, dh: usize, src: &[u8], sw: usize, sh: usize, ox: usize, oy: usize) {
    let cols = sw.min(dw.saturating_sub(ox));
    let rows = sh.min(dh.saturating_sub(oy));
    for y in 0..rows {
        let d0 = ((oy + y) * dw + ox) * 4;
        let s0 = (y * sw) * 4;
        dst[d0..d0 + cols * 4].copy_from_slice(&src[s0..s0 + cols * 4]);
    }
}

/// Fill an opaque rectangle (point coords `r` scaled by `s` to pixels) into `dst`.
fn fill_rect_px(dst: &mut [u8], dw: usize, dh: usize, r: xyscript_app::PaneRectF, s: f32, c: (u8, u8, u8)) {
    let x0 = ((r.x * s).round() as isize).max(0) as usize;
    let y0 = ((r.y * s).round() as isize).max(0) as usize;
    let x1 = (((r.x + r.w) * s).round() as usize).min(dw);
    let y1 = (((r.y + r.h) * s).round() as usize).min(dh);
    for y in y0..y1 {
        for x in x0..x1.min(dw) {
            let i = (y * dw + x) * 4;
            dst[i] = c.0;
            dst[i + 1] = c.1;
            dst[i + 2] = c.2;
            dst[i + 3] = 255;
        }
    }
    let _ = dh;
}

/// Width (points) of the vertical-scrollbar strip reserved on the right of each split pane. Matches
/// the single-pane `sb_w`.
const PANE_SB_W: f32 = 12.0;

/// Draw + handle a vertical scrollbar in `track` for a viewport (`view`) over a document of
/// `line_count` logical lines at the given wrap ratio. Shared by the single-pane path and each split
/// pane; mutates `view` on click/drag. `id_salt` disambiguates the interaction id per pane. The total
/// visual extent is ESTIMATED (logical lines × wrap ratio) so the cost stays O(viewport).
fn draw_vscrollbar(
    ui: &egui::Ui,
    id_salt: u32,
    track: egui::Rect,
    line_count: usize,
    wrap_ratio: f32,
    line_height: f32,
    render_scale: f32,
    fg: (u8, u8, u8),
    view: &mut PaneView,
) {
    let total_logical = line_count.max(1) as f32;
    let visible = (track.height() / line_height).max(1.0); // viewport visual rows
    let lh_px = line_height * render_scale;
    let track_h = track.height();
    // Thumb SIZE is held CONSTANT as you scroll: it's the viewport rows over the document's LOGICAL
    // line count — both fixed while scrolling (only a resize/edit changes them). Sizing off the
    // wrap-aware total instead made the thumb breathe, because the total is re-estimated from the
    // visible window each frame and denser/sparser-wrapping regions gave different wrap ratios.
    // Min thumb 24px, but never taller than the track — a very short pane makes `track_h < 24`, and
    // `clamp(24, track_h)` would then have min > max and PANIC (crashing from the render path).
    let thumb_h =
        (track_h * (visible / total_logical).clamp(0.0, 1.0)).clamp(24.0_f32.min(track_h), track_h);

    // Interact BEFORE computing the position, so a drag can FREEZE the wrap ratio for its whole
    // duration. `wrap_ratio` is sampled per-frame from the visible window; letting the drag map
    // pointer→line through a ratio that changes as the drag scrolls creates a feedback loop that
    // makes the document jitter. Capturing it at drag-start and holding it makes the mapping a fixed
    // linear function — smooth. On release we drop back to the live ratio (one tiny thumb settle,
    // no content move). A click (no drag) just uses the live ratio for its single-shot jump.
    let interactive = track_h - thumb_h > 0.5;
    let sb = interactive.then(|| {
        ui.interact(track, ui.id().with(("vscrollbar", id_salt)), egui::Sense::click_and_drag())
    });
    if let Some(sb) = &sb {
        if sb.drag_started() {
            view.scroll_drag_ratio = Some(wrap_ratio.max(1.0));
        } else if !sb.dragged() {
            view.scroll_drag_ratio = None;
        }
    } else {
        view.scroll_drag_ratio = None;
    }
    let ratio = view.scroll_drag_ratio.unwrap_or(wrap_ratio).max(1.0);

    let total = (total_logical * ratio).max(1.0); // estimated total visual rows (drives POSITION)
    // Current scroll position in visual rows: whole logical lines above (× ratio) + the sub-line
    // residual already scrolled within the window.
    let pos_visual = view.scroll_line as f32 * ratio + view.scroll_vertical / lh_px;
    let denom = (total - visible).max(1.0);
    let pos_frac = (pos_visual / denom).clamp(0.0, 1.0);
    let thumb_top = track.min.y + pos_frac * (track_h - thumb_h);
    let (fr, fgc, fb) = fg;
    let painter = ui.painter();
    painter.rect_filled(track, 0.0, egui::Color32::from_rgba_unmultiplied(fr, fgc, fb, 28));
    painter.rect_filled(
        egui::Rect::from_min_size(
            egui::pos2(track.min.x + 2.0, thumb_top),
            egui::vec2((track.width() - 4.0).max(1.0), thumb_h),
        ),
        3.0,
        egui::Color32::from_rgba_unmultiplied(fr, fgc, fb, 120),
    );
    // Click / drag to scroll — only when the content actually overflows the view.
    if let Some(sb) = &sb {
        if sb.dragged() || sb.clicked() {
            if let Some(p) = sb.interact_pointer_pos() {
                let f = ((p.y - track.min.y - thumb_h / 2.0) / (track_h - thumb_h)).clamp(0.0, 1.0);
                // Map the dragged fraction (visual rows from the top) back to a logical line via the
                // (frozen-during-drag) wrap-ratio estimate; `follow=false` + the next shape settle it.
                let target_visual = f * denom;
                view.scroll_line =
                    ((target_visual / ratio).round() as usize).min((total_logical as usize).saturating_sub(1));
                view.scroll_vertical = 0.0;
                view.follow = false;
            }
        }
    }
}

/// Draw + handle a HORIZONTAL scrollbar in `track` (points) for a `WRAP off` viewport. `content_w` is
/// the widest visible line and `visible_w` the text area, both in DEVICE PIXELS (matching `view.scroll_h`);
/// the thumb and drag map through their ratio so the point/pixel spaces cancel. Draws nothing when the
/// content fits. Mirrors [`draw_vscrollbar`]; mutates `view.scroll_h` (and clears `follow`) on drag.
fn draw_hscrollbar(
    ui: &egui::Ui,
    id_salt: u32,
    track: egui::Rect,
    content_w: f32,
    visible_w: f32,
    fg: (u8, u8, u8),
    view: &mut PaneView,
) {
    let total = content_w.max(1.0);
    let visible = visible_w.max(1.0);
    let track_w = track.width();
    let (fr, fgc, fb) = fg;
    let painter = ui.painter();
    painter.rect_filled(track, 0.0, egui::Color32::from_rgba_unmultiplied(fr, fgc, fb, 28));
    // Thumb: fraction of the content that's visible. Min 24px so it stays grabbable.
    let thumb_w = (track_w * (visible / total).clamp(0.0, 1.0)).clamp(24.0_f32.min(track_w), track_w);
    let denom = (total - visible).max(1.0);
    let pos_frac = (view.scroll_h / denom).clamp(0.0, 1.0);
    let thumb_left = track.min.x + pos_frac * (track_w - thumb_w);
    painter.rect_filled(
        egui::Rect::from_min_size(
            egui::pos2(thumb_left, track.min.y + 2.0),
            egui::vec2(thumb_w, (track.height() - 4.0).max(1.0)),
        ),
        3.0,
        egui::Color32::from_rgba_unmultiplied(fr, fgc, fb, 120),
    );
    // Click / drag — only when the content overflows the view.
    if total > visible + 0.5 && track_w - thumb_w > 0.5 {
        let sb = ui.interact(track, ui.id().with(("hscrollbar", id_salt)), egui::Sense::click_and_drag());
        if sb.dragged() || sb.clicked() {
            if let Some(p) = sb.interact_pointer_pos() {
                let f = ((p.x - track.min.x - thumb_w / 2.0) / (track_w - thumb_w)).clamp(0.0, 1.0);
                view.scroll_h = f * denom;
                view.follow = false;
            }
        }
    }
}

/// Draw a `t`-pixel-thick opaque border around a rectangle (point coords `r` scaled by `s`).
fn draw_border_px(dst: &mut [u8], dw: usize, dh: usize, r: xyscript_app::PaneRectF, s: f32, c: (u8, u8, u8), t: f32) {
    let top = xyscript_app::PaneRectF { x: r.x, y: r.y, w: r.w, h: t };
    let bot = xyscript_app::PaneRectF { x: r.x, y: r.y + r.h - t, w: r.w, h: t };
    let left = xyscript_app::PaneRectF { x: r.x, y: r.y, w: t, h: r.h };
    let right = xyscript_app::PaneRectF { x: r.x + r.w - t, y: r.y, w: t, h: r.h };
    for edge in [top, bot, left, right] {
        fill_rect_px(dst, dw, dh, edge, s, c);
    }
}

struct GuiApp {
    editor: XyEditor,
    font_system: FontSystem,
    swash: SwashCache,
    /// A concrete monospace family name resolved once at startup. cosmic-text's generic
    /// `Family::Monospace` re-runs font fallback per shaped run (~30× slower per line in
    /// profiling), so we shape against this fixed family by name instead.
    mono_family: Option<String>,
    /// The `UIFAMILY` + `UISTYLE` currently installed into the main egui context (`None` = egui's
    /// built-in default). Compared each frame so we only `set_fonts` (atlas rebuild) on a change.
    applied_ui_family: Option<(String, FontStyle)>,
    /// The family + style currently loaded into the main ctx's `xy-preview` egui family for the
    /// font picker's live preview (`None` = picker closed). Gates the (atlas-rebuilding) `set_fonts`.
    applied_preview: Option<(String, FontStyle)>,
    /// The open font-picker dialog (`None` = closed).
    font_picker: Option<FontPicker>,
    /// The open Find/Replace dialog (`None` = closed). Its UI lives in a separate OS window the
    /// runtime creates/destroys to match this being `Some`/`None`.
    find_dialog: Option<FindDialog>,
    /// One-shot: the FIND window should raise + focus itself (set when `open_find_dialog` is invoked
    /// while the window is already open — e.g. Search ▸ Find again). Consumed by the runtime.
    find_refocus: bool,
    /// Live "highlight all" for the FIND dialog: every match span (rope offsets) of the current Find
    /// text, refreshed as you type. Empty when the dialog is closed or the field is empty; `raster`
    /// paints these yellow (behind the glyphs). Cached via `find_hl_key` so we only re-search when
    /// the text / options / document actually change.
    find_highlights: Vec<(usize, usize)>,
    /// The match the caret is on / heading to (the next hit at-or-after the caret, wrapping to the
    /// first) — drawn in a distinct colour so you can see where Enter/F3 will land.
    find_current: Option<(usize, usize)>,
    /// Cache key for `find_highlights` = (text, case, whole-word, regex, in-selection, doc revision).
    find_hl_key: Option<(String, bool, bool, bool, bool, u64)>,
    /// PER-DOCUMENT "highlight all" cache, keyed by `DocId` → (doc revision, match spans): revisiting a
    /// file (e.g. clicking back to a tab) reuses its prior search instead of re-scanning — so a tab
    /// switch is instant even on a big file. Cleared when the Find text/options change (all entries
    /// stale then); in-selection searches bypass it (selection-dependent, not tracked by revision).
    find_hl_cache: std::collections::HashMap<xyscript_app::DocId, (u64, Vec<(usize, usize)>)>,
    /// The Find text/options the `find_hl_cache` was built for; a change invalidates the whole cache.
    find_hl_params: Option<(String, bool, bool, bool, bool)>,
    /// The Find text/options the *Find window* last recomputed highlights for. The Find window is a
    /// separate OS window on its own repaint schedule; it must only rewrite the shared highlight set
    /// when ITS OWN inputs change (find text / an option toggle — needed for type-to-find), NOT on a
    /// document edit. Document edits are handled by the main window's pre-render refresh, which is then
    /// the SOLE writer during editing — so the highlights can't micro-jitter from two windows writing
    /// them at different frame times (a timing artifact that showed on some machines but not others).
    find_win_params: Option<(String, bool, bool, bool, bool)>,
    /// `EMPH` emphasis matches for the ACTIVE document (rope offsets) — a persistent, host-driven
    /// highlight (unlike `find_highlights`, which the Find dialog drives). Painted behind + over the
    /// glyphs with `EMPHBGCOLOR`/`EMPHFGCOLOR`. Cached via `emph_key`.
    emph_highlights: Vec<(usize, usize)>,
    /// Cache key for `emph_highlights` = (active index, doc revision, phrase, is-regex). `None` = no
    /// emphasis. The regex flag is in the key so toggling `EMPH/R` on the same phrase re-searches.
    emph_key: Option<(usize, u64, String, bool)>,
    /// `URLHIGHLIGHT` match spans (rope offsets) in the active doc — coloured with `URLFGCOLOR`/
    /// `URLBGCOLOR` and hit-tested for Ctrl+click-to-open. Cached via `url_key`.
    url_highlights: Vec<(usize, usize)>,
    /// Cache key for `url_highlights` = (active index, doc revision). `None` = off / not yet computed.
    url_key: Option<(usize, u64)>,
    /// The last `BALPAIRS` bracket-match state we saw, so we surface the "no matching bracket" status
    /// only on a transition (not every frame — which would stomp all other status text). `None` = not
    /// yet computed.
    bal_last: Option<xyscript_app::BalMatch>,
    /// `WRAPINDENT` per-frame state (empty when off). `wi_indent[li]` = the hanging-indent pixel offset
    /// for display line `li` (0 = none); continuation rows of that line are drawn shifted right by it.
    wi_indent: Vec<f32>,
    /// `WRAPINDENT` per-frame row table: `(y_top, y_bot, x_offset)` for every visible visual row, so
    /// caret / hit-test / span-rect geometry can look up a row's indent offset by its `y`.
    wi_rows: Vec<(f32, f32, f32)>,
    /// The active document's visible byte window `(decoded_lo, decoded_hi)` from the last `raster` —
    /// the region the live checker scopes to (so a huge file doesn't block the UI). `None` until the
    /// first frame renders. Handed to the background lint worker via `request_lint`.
    lint_visible: Option<(usize, usize)>,
    /// Per VISIBLE pane, `(doc index, decoded_lo, decoded_hi)` collected during the render — so the
    /// live checker can refresh EVERY pane's document in a split, not just the focused one. One entry
    /// in single-pane. `lint_rr` round-robins which one is (re)checked each frame (one worker, one at a
    /// time), so both panes' underlines stay current without linting all of them every frame.
    lint_panes: Vec<(usize, usize, usize)>,
    lint_rr: usize,
    /// The doc index of the pane that currently has keyboard focus (set at the top of each render,
    /// before the composite loop switches `active` per pane). Only in THIS doc do we hide the spelling
    /// underline for the word being typed — inactive panes always show all their marks.
    lint_focus_doc: Option<usize>,
    /// One-shot: a search just found a match — if it's off-screen, `follow` scrolls it to just above
    /// mid-screen (instead of the minimal edge scroll used for ordinary caret motion). Set from the
    /// host's `take_reveal_center` each frame, consumed by the follow logic.
    center_caret: bool,
    /// Console scroll-lock: whether the Console's tail was visible at the end of the last frame. New
    /// output auto-scrolls to the tail only when this is true, so scrolling UP to read back pauses the
    /// follow (and scrolling back to the bottom resumes it) — standard terminal behavior.
    console_at_bottom: bool,
    /// The spelling correction choices for the word under the last right-click (drives the spell
    /// context menu), or `None` if the menu isn't open. Rendered as a manual popup (egui's
    /// `context_menu` auto-open was unreliable on the drag-sensed canvas).
    spell_menu: Option<xyscript_app::host::LintFixChoices>,
    /// Screen position to open the spell popup at (the right-click point).
    spell_menu_pos: Option<egui::Pos2>,
    /// Frames the spell popup has been open — a settle guard so the opening right-click can't be
    /// mistaken for a click-outside and dismiss it instantly.
    spell_menu_frames: u32,
    /// The open Run-command dialog (`None` = closed).
    run_dialog: Option<RunCmdDialog>,
    grep_dialog: Option<GrepDialog>,
    pending_page: i32,
    pending_page_extend: bool,
    /// Persistent cosmic buffer for the render window: its `lines` ARE the shaped-line cache. Each
    /// frame `build_window_cosmic` reconciles it against the new window, reusing lines whose content
    /// + colour signature is unchanged so their (expensive) shaped layout survives — a 25 000-char
    /// paragraph then shapes ONCE, not every frame. Taken out via `Option::take` while a frame uses
    /// it, put back at the end of `render_canvas`/`page_move`. `win_sigs` parallels `.lines` (one
    /// per-line signature each); `win_global_sig` is the shaping-parameter key (width/wrap/font) —
    /// a change there invalidates every cached layout, so the whole cache is dropped.
    win_buffer: Option<Buffer>,
    win_sigs: Vec<u64>,
    win_global_sig: u64,
    /// Whether the Document ▸ Properties info box is showing.
    properties_open: bool,
    /// Whether the Help ▸ About box is showing.
    about_open: bool,
    /// Active keyboard menu navigation (the `MENU` verb). `None` = the menu bar is mouse-only.
    menu_nav: Option<MenuNav>,
    /// Each open dropdown column's measured size, by depth, carried one frame so `place_menu_col`
    /// can decide off-screen flips before rendering (menus are stable, so it settles in a frame).
    menu_col_sizes: Vec<egui::Vec2>,
    /// A menu item the keyboard navigator chose; drained by `update` (runs its dialog/Act there).
    pending_menu_act: Option<MenuAct>,
    /// A native file dialog requested this frame (menu Open/Save-As/Export/Run, or a `OPEN`/`save`
    /// key). The runtime drains it AFTER the frame and runs it parented + modal — see [`PendingDialog`].
    pending_dialog: Option<PendingDialog>,
    /// Set when the pending Save-As dialog was launched by the quit sweep: a cancel then aborts the
    /// quit (rather than just closing the dialog), matching the old inline behavior.
    save_as_for_quit: bool,
    /// Set when the user grabs the title-bar-replacement drag strip; the runtime drains it and starts
    /// an OS window move (`window.drag_window`).
    start_window_drag: bool,
    /// Last-used Find dialog options, remembered across opens: (case, whole word, regex, in selection).
    find_opts: (bool, bool, bool, bool, bool, bool, bool),
    /// The open colour-picker dialog: its target + the colour currently chosen in it (`None` =
    /// closed). On OK the matching `*COLOR`/`HLCOLOR` setter is run.
    color_picker: Option<(ColorTarget, egui::Color32)>,
    /// The `MSG` message box currently shown (text + optional self-dismiss deadline). `None`
    /// when no message is up.
    message_box: Option<MessageBox>,
    /// The desugared operative keymap, handed to the winit runtime to build its key resolver
    /// (taken once at startup). See `runtime.rs`.
    capture_keymap: Option<xyscript_kbd::Keymap>,
    /// A `save` command (e.g. Ctrl+S) fired on an untitled doc: the editor can't open a native
    /// dialog, so the next frame opens Save-As (mirrors the menu). Set by `run_captured`.
    pending_save_as: bool,
    // The active viewport's scroll/follow/canvas state. Windows the document around
    // `pane_view.scroll_line` each frame (see `DocWindow`), so cosmic only ever holds the window —
    // nothing here scales with file size. One today; one-per-pane once splits land.
    pane_view: PaneView,
    /// Split panes (Phase 2): saved viewport state for the NON-focused panes, keyed by `PaneId`.
    /// The focused pane's live view is always `pane_view` (so the 80-odd `pane_view` reads keep
    /// meaning "the focused viewport"); on a focus change we swap the two through this map.
    pane_views: std::collections::HashMap<xyscript_app::PaneId, PaneView>,
    /// The pane focused as of last frame — compared each frame to detect a focus change (a `TW`
    /// verb or a click in another pane) so `pane_view` can be swapped to the newly focused pane.
    last_focus: Option<xyscript_app::PaneId>,
    /// SINGLE-PANE per-document scroll memory, keyed by the stable `DocId`: switching tabs (or
    /// `BUFNEXT`/`LISTTABS`/…) returns you to where you were in each file instead of snapping to the
    /// caret. Swapped through on an active-document change, mirroring `pane_views` for split panes.
    doc_scroll: std::collections::HashMap<xyscript_app::DocId, PaneView>,
    /// The active document as of last frame — compared each frame to detect a tab/buffer switch.
    last_active_doc: Option<xyscript_app::DocId>,
    /// A click that just focused a non-focused pane: its SCREEN position, applied as a caret placement
    /// on the NEXT frame once that pane is focused + rendered (its window is only hit-testable then).
    /// Gives a single physical click both focus + caret, instead of needing a second click.
    pending_pane_click: Option<egui::Pos2>,
    /// Per-pane wrap ratio (visual rows ÷ logical lines) captured during the composite render, keyed
    /// by `PaneId` — feeds each pane's scrollbar thumb sizing. Only populated in a split.
    pane_wrap: std::collections::HashMap<xyscript_app::PaneId, f32>,
    drag_anchor: Option<usize>,
    /// Alt+drag box-select anchor corner, as `(line, column)` (0-based, logical). `Some` for the
    /// duration of a column-select drag; each drag update rebuilds a caret per spanned line.
    box_anchor: Option<(usize, usize)>,
    /// Device-pixel width of one monospace character cell in the HEX view, measured from the shaped
    /// glyphs on each `render_hex_canvas` pass and reused by the mouse hit-test so clicks land on the
    /// right byte. `0.0` until the first hex frame (the hit-test falls back to the 0.6·em estimate).
    hex_cell_w: f32,
    /// HEX-view editing focus: `false` = the hex-pair column (nibble entry), `true` = the text column
    /// (whole-byte entry). Tab toggles it. Only meaningful while `hex_view()` is on.
    hex_text_col: bool,
    /// HEX-view nibble sub-position in the hex column: `Some(off)` means a HIGH nibble was just
    /// entered at byte `off` and the next hex digit sets its LOW nibble; `None` means the next digit
    /// starts a fresh byte. Keyed by byte offset so it SELF-CORRECTS — any caret move (nav/click)
    /// leaves `off` pointing at a byte the caret is no longer on, so the next digit starts fresh.
    hex_nibble_at: Option<usize>,
    /// Mouse drag-select anchor for the command line (rope offset), like `drag_anchor` for the doc.
    cmline_drag_anchor: Option<usize>,
    preedit: String,
    // Visual (wrapped-line) Up/Down: the target x column held across consecutive
    // vertical moves, and the last canvas size (so the move can re-shape the layout).
    goal_x: Option<f32>,
    // `CARETMOTION` (visual) Tier-3: the caret's visual x (in windowed-buffer px) and the rope offset it belongs
    // to, carried across consecutive per-glyph moves. At a direction boundary (e.g. a number inside
    // Arabic) one logical offset has two visual positions and `cursor_position` collapses it to one
    // side; tracking the x we actually moved to keeps stepping — and drawing — on the correct side.
    // Ignored once the caret leaves this offset (any non-visual move). See `visual_char_move`.
    bidi_vx: Option<(usize, f32)>,
    /// Device-pixels-per-point (HiDPI scale × UISCALE zoom): the canvas is rastered at this scale
    /// and displayed at logical size, so text stays crisp instead of egui upscaling a point-res image.
    render_scale: f32,
    // Editor font size (pt) + line height (px), recomputed from the host's EDFONT/LEADING
    // knobs at the top of each `ui` frame (B16). All canvas-metric code reads these.
    font_size: f32,
    line_height: f32,
    // Line-number gutter (`$LNS`): `(document line, canvas-relative y)` for the first visual
    // row of each visible logical line, collected during `raster` and painted by the gutter.
    gutter_lines: Vec<(usize, f32)>,
    // Bookmark rows: canvas-relative y of the first visual row of each visible logical line that
    // holds a `MARKSET` bookmark. Collected during `raster`, painted as a glyph in the left strip.
    mark_rows: Vec<f32>,
    // Average VISUAL rows per LOGICAL line over the laid-out window, measured each frame in
    // `raster`. The visual scrollbar multiplies the document's logical-line count by this to
    // estimate the total visual extent — an O(viewport) estimate (no whole-document shaping).
    wrap_ratio: f32,
    // OS clipboard bridge: the handle (None if unavailable) and the last value we
    // reconciled both ways, so we can tell an app-side change from an external one.
    os_clipboard: Option<arboard::Clipboard>,
    os_clip_mirror: String,
    // The editing canvas texture, kept across frames and updated in place (rather
    // than reallocating each frame, which can leave the display showing a stale one).
    canvas_tex: Option<egui::TextureHandle>,
    // Debounced PDF preview (right pane). Visibility is host-owned (`PV`/`$PV`); these are the
    // GUI-side render cache + debounce state.
    preview_tex: Option<egui::TextureHandle>,
    preview_src: String,
    preview_err: Option<String>,
    preview_was_visible: bool,
    preview_was_kind: RenderKind,
    /// The Preview/TOPDF font choice (`PVFAMILY`/`PVSIZE`/`PVCODE`) the cached raster was rendered
    /// with — re-render when it changes so a font switch shows immediately in the GFM preview.
    preview_font_sig: String,
    /// Pane width the preview texture was last RENDERED at — re-render (rescale) when it changes, so
    /// the preview fills the pane instead of staying at a first-frame thumbnail size.
    preview_pane_w: f32,
    /// Pane width seen on the previous frame — to restart the debounce while a resize is in motion.
    preview_seen_w: f32,
    /// The editor viewport-top line the preview was last sync-scrolled to (re-sync only on change).
    preview_synced_line: usize,
    last_text: String,
    last_change_time: f64,
    // M5: menus + status. The command line is the editor's *native* CMline
    // (`editor.cmline_open()` is the focus state); there is no separate egui widget,
    // which is what keeps Enter from leaking a newline into the document.
    status: String,
    /// Whether ANY XyScript window (main / TXT / DIR-W / Find) currently holds OS focus — the runtime
    /// keeps this current from `Focused` events. `DIMONBLUR` dims the main window only when this is
    /// false (the whole app is inactive), so focusing a child window doesn't dim the editor.
    app_has_focus: bool,
    /// When the app went unfocused (`None` while focused). `DIMONBLUR` waits a short grace period before
    /// dimming, so a within-app focus hop (e.g. opening the Find window, which briefly leaves the main
    /// unfocused before the Find window's focus registers) doesn't flash the dim on and off.
    unfocused_since: Option<std::time::Instant>,
    /// Wall-clock of the last autosave pass (drives the `AUTOSAVE` interval timer).
    last_autosave: std::time::Instant,
    /// Wall-clock of the last `AUTOREOPEN` external-change poll (throttles the file-change check).
    last_reopen_check: std::time::Instant,
    /// Wall-clock of the last `SESSION` snapshot (drives the crash-resilience interval timer).
    last_session_snapshot: std::time::Instant,
    /// The session's structure fingerprint at the last snapshot — a change (tab open/close/switch,
    /// split/unsplit) triggers an immediate snapshot rather than waiting for the interval.
    last_session_signature: String,
    /// The `HISTORY`/`HI` picker, when open: the highlighted row in the filtered view.
    history_dialog: Option<usize>,
    /// The picker's filter arg (number = last N, text = contains, empty = all).
    history_filter: String,
    /// The `PICK` list chooser, when open: the highlighted row in the (type-to-find) filtered view.
    pick_dialog: Option<usize>,
    /// The `PICK` chooser's live type-to-find filter (case-insensitive substring; empty = all).
    pick_filter: String,
    /// A `QUIT` (verify) is sweeping dirty tabs, prompting save/discard/cancel for each.
    quitting: bool,
    /// Set when the app should exit the event loop (the runtime checks it after each frame).
    should_exit: bool,
    /// The line being typed for a script parked on `INPUT` (a modal prompt owns the
    /// keyboard while `editor.awaiting_line()`). `READKEY` is captured raw in `runtime.rs`.
    input_line: String,
    /// Cached discovery of loadable `<xx>.lang` catalogs for `Settings ▸ Language`, with the instant
    /// it was scanned. `menu_tree` (hence this scan) runs every frame, so `available_languages`
    /// throttles the filesystem walk. `RefCell` because `menu_tree` takes `&self`.
    lang_menu_cache: std::cell::RefCell<Option<(std::time::Instant, Vec<LangChoice>)>>,
}

impl GuiApp {
    fn new() -> GuiApp {
        let km = match load(INTERNAL_KBD, Some(120)) {
            LoadOutcome::Installed(km) => km,
            LoadOutcome::Rejected(_) => panic!("sample kbd failed to parse"),
        };
        // Real disk, real files: start on an empty untitled document. The user
        // drives File ops from the menu (Open / Save / Save As / Export PDF) and
        // arbitrary XPL from the command bar.
        let mut editor = XyEditor::new(km.clone());
        // Personal dictionary (`LEARN`): load `xyscript-dict.txt` beside the executable, if present.
        editor.load_learned_words();
        // Startup script: `-s <path>` if given, else `startup.xpl` beside the executable. It may
        // `LOADKBD` (→ a fresh operative keymap), set config, `AUTOCFG`, etc.
        let args: Vec<String> = std::env::args().skip(1).collect();
        // Startup feedback (a fatal error, or PR/MSG output) seeds the initial status line. A fatal
        // error aborts the REST of the startup script, so without this a bad line looks like a silent
        // no-op — the reported "CD in startup.xpl doesn't take effect".
        let mut startup_status = String::new();
        match resolve_startup_script(&args) {
            Ok(Some((label, src))) => {
                editor.run_startup(&src, &label);
                if let Some(fb) = editor.command_feedback() {
                    startup_status = fb;
                }
                if let Some(e) = editor.last_error() {
                    // `e` already self-locates (`<file>:<line>: …`) for a command error; keep a short
                    // "startup" tag for the console without repeating the full path.
                    eprintln!("xyscript: startup: {e}");
                }
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("xyscript: {e}");
                startup_status = format!("error: {e}");
            }
        }
        // The capture resolver needs the operative (desugared) keymap: the startup-loaded one if
        // startup swapped it, else the bundled sample. (Consuming `reloaded_keymap` here also stops
        // the runtime's first resync from re-applying it.)
        let capture_keymap =
            editor.take_reloaded_keymap().unwrap_or_else(|| xyscript_app::desugar_keymap(km));
        let font_system = FontSystem::new();
        // Resolve a concrete monospace family once (prefer common, well-covered faces;
        // else the first monospaced face the DB knows). Shaping by name avoids the
        // generic-`Monospace` fallback cost.
        let mono_family = {
            let db = font_system.db();
            let preferred = [
                "Consolas",
                "Cascadia Mono",
                "Cascadia Code",
                "Courier New",
                "Lucida Console",
                "DejaVu Sans Mono",
            ];
            preferred
                .iter()
                .find(|name| {
                    db.faces()
                        .any(|f| f.families.iter().any(|(fam, _)| fam.eq_ignore_ascii_case(name)))
                })
                .map(|s| s.to_string())
                .or_else(|| {
                    db.faces()
                        .find(|f| f.monospaced)
                        .and_then(|f| f.families.first().map(|(n, _)| n.clone()))
                })
        };
        GuiApp {
            editor,
            font_system,
            swash: SwashCache::new(),
            mono_family,
            applied_ui_family: None,
            applied_preview: None,
            font_picker: None,
            find_dialog: None,
            find_refocus: false,
            find_highlights: Vec::new(),
            emph_highlights: Vec::new(),
            emph_key: None,
            url_highlights: Vec::new(),
            url_key: None,
            bal_last: None,
            wi_indent: Vec::new(),
            wi_rows: Vec::new(),
            find_current: None,
            find_hl_key: None,
            find_hl_cache: std::collections::HashMap::new(),
            find_hl_params: None,
            find_win_params: None,
            lint_visible: None,
            lint_panes: Vec::new(),
            lint_rr: 0,
            lint_focus_doc: None,
            center_caret: false,
            console_at_bottom: true, // follow the tail until the user scrolls up
            spell_menu: None,
            spell_menu_pos: None,
            spell_menu_frames: 0,
            run_dialog: None,
            grep_dialog: None,
            pending_page: 0,
            pending_page_extend: false,
            win_buffer: None,
            win_sigs: Vec::new(),
            win_global_sig: 0,
            properties_open: false,
            about_open: false,
            menu_nav: None,
            menu_col_sizes: Vec::new(),
            pending_menu_act: None,
            pending_dialog: None,
            save_as_for_quit: false,
            start_window_drag: false,
            find_opts: (false, false, false, false, false, false, false),
            color_picker: None,
            message_box: None,
            capture_keymap: Some(capture_keymap),
            pending_save_as: false,
            pane_view: PaneView { scroll_line: 0, scroll_vertical: 0.0, follow: true, scroll_h: 0.0, content_w: 0.0, canvas_px: (0, 0), scroll_drag_ratio: None },
            pane_views: std::collections::HashMap::new(),
            last_focus: None,
            doc_scroll: std::collections::HashMap::new(),
            last_active_doc: None,
            pending_pane_click: None,
            pane_wrap: std::collections::HashMap::new(),
            drag_anchor: None,
            box_anchor: None,
            hex_cell_w: 0.0,
            hex_text_col: false,
            hex_nibble_at: None,
            cmline_drag_anchor: None,
            preedit: String::new(),
            goal_x: None,
            bidi_vx: None,
            render_scale: 1.0,
            font_size: 22.0,
            line_height: 32.0,
            gutter_lines: Vec::new(),
            mark_rows: Vec::new(),
            wrap_ratio: 1.0,
            os_clipboard: arboard::Clipboard::new().ok(),
            os_clip_mirror: String::new(),
            canvas_tex: None,
            preview_tex: None,
            preview_src: String::new(),
            preview_font_sig: String::new(),
            preview_err: None,
            preview_was_visible: false,
            preview_was_kind: RenderKind::Gfm,
            preview_pane_w: 0.0,
            preview_seen_w: 0.0,
            preview_synced_line: usize::MAX, // force an initial sync
            last_text: String::new(),
            last_change_time: f64::MIN,
            status: startup_status,
            app_has_focus: true, // assume focused at launch until a Focused event says otherwise
            unfocused_since: None,
            last_autosave: std::time::Instant::now(),
            last_reopen_check: std::time::Instant::now(),
            last_session_snapshot: std::time::Instant::now(),
            last_session_signature: String::new(),
            history_dialog: None,
            history_filter: String::new(),
            pick_dialog: None,
            pick_filter: String::new(),
            quitting: false,
            should_exit: false,
            input_line: String::new(),
            lang_menu_cache: std::cell::RefCell::new(None),
        }
    }

    /// The command-line panel's label: live buffer + caret while focused, else the parked
    /// command (the cmline retains it after submit), or a bare prompt when blanked.
    fn cmline_label(&self) -> String {
        // The caret is painted as an OVERLAY in `cmline_row` — it is NOT inserted as a `│` glyph.
        // Inserting one shifts every character to its right by a whole monospace cell as the caret
        // passes (the "cushion" effect); an overlay leaves the text fixed. So the label is simply
        // the prompt plus the buffer, whether focused, parked-with-command, or blank.
        format!(": {}", self.editor.command_line().text())
    }

    /// The cmline rope offset under a pointer position (for click / drag-select hit-testing).
    /// `lead_bias` biases the point left by half a cell so a drag that starts ON a character
    /// includes it (round-to-leading-edge), mirroring the document canvas.
    fn cmline_offset_at(
        &self,
        ui: &egui::Ui,
        label: &str,
        font: &egui::FontId,
        r_min: egui::Pos2,
        pos: egui::Pos2,
        lead_bias: f32,
    ) -> usize {
        let galley = ui.painter().layout_no_wrap(label.to_string(), font.clone(), egui::Color32::WHITE);
        let mut rel = pos - r_min;
        rel.x = (rel.x - lead_bias).max(0.0);
        let ci = galley.cursor_from_pos(rel).index;
        self.cmline_display_index_to_rope(ci)
    }

    /// Render the CMline row: the (optional) `$CWD` prefix, the buffer text, its selection band and
    /// caret. Click places the caret; mouse-DRAG selects (feeding `CUT`/`COPY` while focused). The
    /// labels are `.selectable(false)` so the ONLY selection is the cmline buffer's own — and the
    /// `$CWD` prefix is inert (out of bounds for the mouse). Returns whether the mouse interacted
    /// this frame (so the caller can request a repaint). Takes `&mut self`: it applies focus + caret
    /// + selection directly.
    fn cmline_row(&mut self, ui: &mut egui::Ui) -> bool {
        let label = self.cmline_label();
        // Match the Label's resolved font (Body size, Monospace family) so a galley we build for
        // hit-testing / caret geometry measures identically to what's drawn.
        let font = egui::FontId::new(self.editor.chrome_font(), egui::FontFamily::Monospace);
        let cwd_text = (self.editor.special_str("PAPLACE") == "cmline").then(|| {
            let plen: usize = self.editor.special_str("PATHLEN").parse().unwrap_or(45);
            format!("{} ", truncate_path(&self.editor.cwd(), plen))
        });
        let mut interacted = false;
        // Right-click context menu on the cmline (clipboard block with a selection; Paste + Select
        // word/line with none).
        let cmline_has_sel = self.editor.command_line().has_selection();
        let cmline_clip_empty = self.editor.clipboard().is_empty();
        let mut ctx_cmd: Option<&'static str> = None;
        ui.horizontal(|ui| {
            // `$CWD` prefix (PAPLACE cmline): non-interactive and NON-selectable so the mouse can't
            // grab it — it reads as context, not editable text. Colour: `PAFGCOLOR`/`PABGCOLOR` when
            // set, else the default dimmed (`.weak()`) UI text on a transparent background.
            if let Some(cwd) = &cwd_text {
                let mut rt = egui::RichText::new(cwd.clone()).monospace();
                rt = match self.editor.host().path_fg_color() {
                    Some((r, g, b)) => rt.color(egui::Color32::from_rgb(r, g, b)),
                    None => rt.weak(),
                };
                if let Some((r, g, b)) = self.editor.host().path_bg_color() {
                    rt = rt.background_color(egui::Color32::from_rgb(r, g, b));
                }
                ui.add(egui::Label::new(rt).selectable(false));
            }
            // The cmline text. `.selectable(false)`: egui's own label selection is off, so the only
            // selection is the cmline BUFFER's (rendered below) — the one `CUT`/`COPY` act on.
            let r = ui.add(
                egui::Label::new(egui::RichText::new(label.clone()).monospace())
                    .selectable(false)
                    .sense(egui::Sense::click_and_drag()),
            );
            let cell = ui
                .painter()
                .layout_no_wrap("0".to_string(), font.clone(), egui::Color32::WHITE)
                .rect
                .width();
            // Selection band + caret overlay (focused only). In monospace the x of any offset is the
            // pixel width of the displayed text up to it.
            if self.editor.cmline_open() {
                let painter = ui.painter().clone();
                let (top, bot, left) = (r.rect.top(), r.rect.bottom(), r.rect.left());
                let wx = |upto: &str| {
                    painter.layout_no_wrap(format!(": {upto}"), font.clone(), egui::Color32::WHITE).rect.width()
                };
                let cl = self.editor.command_line();
                let text = cl.text();
                // Selection band: a translucent SELBGCOLOR fill behind the selected range.
                if cl.has_selection() {
                    if let Some(anc) = cl.anchor() {
                        let a = cl.decoded_offset(anc).min(text.len());
                        let c = cl.decoded_offset(cl.cursor()).min(text.len());
                        let (s, e) = (a.min(c), a.max(c));
                        let x0 = left + wx(&text[..s]);
                        let x1 = left + wx(&text[..e]);
                        let (sr, sg, sb) = self.editor.sel_bg_color();
                        let selcol = egui::Color32::from_rgb(sr, sg, sb).gamma_multiply(0.5);
                        painter.rect_filled(
                            egui::Rect::from_min_max(egui::pos2(x0, top), egui::pos2(x1, bot)),
                            0.0,
                            selcol,
                        );
                    }
                }
                // Caret (CMCRSHAPE bar | block | underline, in CMCRCOLOR).
                let cur = cl.decoded_offset(cl.cursor()).min(text.len());
                let x = left + wx(&text[..cur]);
                let (cr, cg, cb) = self.editor.cmline_caret_color();
                let col = egui::Color32::from_rgb(cr, cg, cb);
                let bar = 2.0_f32;
                let shape = self.editor.cmline_caret_shape();
                let rect = match shape {
                    CaretShape::Bar => {
                        egui::Rect::from_min_max(egui::pos2(x, top), egui::pos2(x + bar, bot))
                    }
                    CaretShape::Underline => {
                        egui::Rect::from_min_max(egui::pos2(x, bot - bar), egui::pos2(x + cell, bot))
                    }
                    CaretShape::Block => {
                        egui::Rect::from_min_max(egui::pos2(x, top), egui::pos2(x + cell, bot))
                    }
                };
                let fill =
                    if matches!(shape, CaretShape::Block) { col.gamma_multiply(0.45) } else { col };
                painter.rect_filled(rect, 0.0, fill);
            }
            // The rest of the row is click+drag too, so clicking the empty space focuses / places
            // the caret at the end.
            let rest = ui.allocate_response(ui.available_size(), egui::Sense::click_and_drag());
            let resp = r.union(rest);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
            }
            // Interaction: a click places the caret (clearing any selection); a drag selects.
            if resp.drag_started() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    if !self.editor.cmline_open() {
                        self.editor.open_cmdline();
                    }
                    let anchor =
                        self.cmline_offset_at(ui, &label, &font, r.rect.min, pos, 0.5 * cell);
                    self.cmline_drag_anchor = Some(anchor);
                    self.editor.click_cmline(anchor); // caret at anchor; no selection yet
                    interacted = true;
                }
            } else if resp.dragged() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let cursor = self.cmline_offset_at(ui, &label, &font, r.rect.min, pos, 0.0);
                    match self.cmline_drag_anchor {
                        Some(a) if a != cursor => self.editor.select_cmline(cursor, a),
                        _ => self.editor.click_cmline(cursor),
                    }
                    interacted = true;
                }
            } else if resp.clicked() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    if !self.editor.cmline_open() {
                        self.editor.open_cmdline();
                    }
                    let off = self.cmline_offset_at(ui, &label, &font, r.rect.min, pos, 0.0);
                    self.editor.click_cmline(off);
                    self.cmline_drag_anchor = None;
                    interacted = true;
                }
            }
            if cmline_has_sel {
                resp.context_menu(|ui| clipboard_context_menu(ui, &mut ctx_cmd, false));
            } else {
                // No selection: drop the caret at the right-click point, then Paste + Select
                // word/line (no paragraph on the single-line cmline).
                if resp.secondary_clicked() {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        if !self.editor.cmline_open() {
                            self.editor.open_cmdline();
                        }
                        let off = self.cmline_offset_at(ui, &label, &font, r.rect.min, pos, 0.0);
                        self.editor.click_cmline(off);
                    }
                }
                resp.context_menu(|ui| {
                    no_selection_context_menu(ui, &mut ctx_cmd, cmline_clip_empty, false)
                });
            }
        });
        // Apply a chosen verb. Clipboard verbs run via `run_command` (they stay on the focused
        // cmline). The "Select …" verbs (`DW`/`DL`/`DP`) are focus-doc verbs — run as commands
        // they'd define in the DOCUMENT — so they're applied DIRECTLY to the cmline buffer instead.
        if let Some(cmd) = ctx_cmd {
            if !self.editor.cmline_open() {
                self.editor.open_cmdline();
            }
            match cmd {
                "DW" => self.editor.cmline_define_word(),
                "DL" | "DP" => self.editor.cmline_define_line(), // single line: DP == DL
                _ => self.editor.run_command(cmd),
            }
            interacted = true;
        }
        interacted
    }

    /// Map a character index in the *displayed* cmline label (which carries a `": "`
    /// prefix and, while focused, a `│` caret marker) back to a rope offset in the
    /// cmline buffer.
    fn cmline_display_index_to_rope(&self, display_ci: usize) -> usize {
        const PREFIX_CHARS: usize = 2; // ": "
        let cl = self.editor.command_line();
        let text = cl.text();
        // The label no longer carries a `│` caret glyph (it's painted as an overlay), so the
        // displayed index maps straight through the `": "` prefix to the buffer.
        let p = display_ci.saturating_sub(PREFIX_CHARS);
        let total = text.chars().count();
        let byte = text
            .char_indices()
            .nth(p.min(total))
            .map(|(b, _)| b)
            .unwrap_or(text.len());
        cl.rope_offset(byte)
    }

    /// Reset the viewport after switching documents (New / Open).
    fn reset_view(&mut self) {
        self.pane_view.scroll_line = 0;
        self.pane_view.scroll_vertical = 0.0;
        self.pane_view.scroll_h = 0.0;
        self.pane_view.follow = true;
        self.preedit.clear();
        self.preview_src.clear();
        self.last_text.clear();
    }

    /// Open file arguments named on the command line as editing tabs (`xyscript.exe a.md notes.txt`).
    /// A missing path starts a new buffer named for it (OPEN semantics). Files open in order; the
    /// FIRST one is left active so you start on the file you named first. A genuine open error (e.g.
    /// permission) surfaces on the status line; switches are filtered out by the caller.
    fn open_startup_files(&mut self, paths: &[String]) {
        let mut first: Option<usize> = None;
        for p in paths {
            let r = self.editor.open(p);
            self.reset_view();
            if first.is_none() {
                first = Some(self.editor.active_index());
            }
            if let Err(e) = r {
                self.note(Err(e));
            }
        }
        if let Some(i) = first {
            self.editor.set_active_buffer(i);
            self.reset_view();
        }
    }

    /// Record the outcome of a file op as the status line message.
    fn note(&mut self, r: Result<String, String>) {
        self.status = match r {
            Ok(s) if !s.is_empty() => s,
            Ok(_) => String::new(),
            Err(e) => format!("error: {e}"),
        };
    }

    /// Slice a window of whole logical lines around `scroll_line` (plus a screenful of
    /// margin each side) out of the rope. `O(window)` regardless of file size: one rope
    /// slice + decode + two index scans, all over a few-dozen-line span. The margin must
    /// be ≥ a page so a single PgDn/PgUp lands inside the same window.
    fn compute_window(&mut self, h: usize) -> DocWindow {
        // `h` is in PIXELS; the row count uses the pixel line height (= logical rows).
        let visible = ((h as f32 / (self.line_height * self.render_scale)).floor() as usize).max(1);
        let margin = visible; // one screen each side — covers a full page jump
        let total = self.editor.doc(self.editor.active_index()).line_count().max(1);
        if self.pane_view.scroll_line >= total {
            self.pane_view.scroll_line = total - 1;
        }
        let mut start_line = self.pane_view.scroll_line.saturating_sub(margin);
        let mut end_line = (self.pane_view.scroll_line + visible + margin).min(total);
        let doc = self.editor.doc(self.editor.active_index());
        // Cap the window's BYTE extent, not just its logical-line count. A logical-line margin pulls
        // MEGABYTES into every frame's shaping on paragraph-formatted files (one "line" can be a
        // 25 000-char paragraph); bound the span up/down from the scroll anchor by a byte budget.
        // Whole lines only, so wrapping and caret geometry stay exact. Short-line files never reach
        // the cap (their logical-line margin spans far fewer bytes than the budget), so their window
        // is unchanged. The down budget must exceed one screenful of the longest single paragraph so
        // the visible rows are always covered; the up budget is just smooth-scroll context.
        const WIN_UP_BYTES: usize = 48 * 1024;
        const WIN_DOWN_BYTES: usize = 64 * 1024;
        let anchor = doc.line_byte_range(self.pane_view.scroll_line).0;
        start_line = start_line.max(doc.line_at_byte(anchor.saturating_sub(WIN_UP_BYTES)));
        let down = doc.line_at_byte((anchor + WIN_DOWN_BYTES).min(doc.len())) + 1;
        end_line = end_line.min(down.max(self.pane_view.scroll_line + 1)).min(total);
        let start_byte = doc.line_byte_range(start_line).0;
        let end_byte = if end_line >= total { doc.len() } else { doc.line_byte_range(end_line).0 };
        let decoded = doc.slice(start_byte, end_byte); // window text, decoded (has `\r`)
        let start_decoded = doc.decoded_offset(start_byte);
        let display = strip_cr(&decoded);
        let cr: Vec<usize> =
            decoded.bytes().enumerate().filter(|&(_, b)| b == b'\r').map(|(i, _)| i).collect();
        let nl: Vec<usize> =
            display.bytes().enumerate().filter(|&(_, b)| b == b'\n').map(|(i, _)| i).collect();
        DocWindow {
            start_line,
            start_byte,
            start_decoded,
            decoded_len: decoded.len(),
            at_eof: end_line >= total,
            display: Rc::from(display.as_str()),
            cr: Rc::from(cr),
            nl: Rc::from(nl),
        }
    }

    /// Build a fresh cosmic buffer over the window's display text, with highlight colors
    /// (tokens overlapping the window, rebased to window-local display offsets). Cheap —
    /// the window is only a few dozen `BufferLine`s, not the whole document.
    fn build_window_cosmic(&mut self, w: usize, h: usize, win: &DocWindow) -> Buffer {
        self.refresh_emph(); // keep the active tab's EMPH match set current before drawing
        self.refresh_url(); // and the URLHIGHLIGHT match set
        let lo = win.start_decoded;
        let hi = win.start_decoded + win.decoded_len;
        let wt: Vec<Token> = self
            .editor
            .highlights()
            .iter()
            .filter_map(|t| {
                let s = t.start.max(lo);
                let e = t.end.min(hi);
                if s >= e {
                    return None;
                }
                let sd = decoded_to_display(s - lo, &win.cr);
                let ed = decoded_to_display(e - lo, &win.cr);
                (sd < ed).then(|| Token { start: sd, end: ed, kind: t.kind.clone() })
            })
            .collect();
        // The canvas is a fixed-pitch (monospace) text surface — required for the DIR
        // listing's aligned columns and natural for a XyWrite-style editor. Shape against
        // the concrete resolved family (by name) rather than the generic `Monospace`,
        // which is far slower per line.
        let fam = self.editor_family();
        // `EDSTYLE` → cosmic weight/slant (cosmic does its own face matching/synthesis).
        let (e_weight, e_style) = editor_cosmic_style(self.editor.editor_font_style());
        let mono = || {
            let a = match &fam {
                Some(n) => Attrs::new().family(Family::Name(n)),
                None => Attrs::new().family(Family::Monospace),
            };
            a.weight(e_weight).style(e_style)
        };
        // Like `mono()` but with a colour and the `MDA` bold/italic bits folded in: a set bold/italic
        // bit forces that face, otherwise the base `EDSTYLE` weight/slant applies (attributes ADD
        // emphasis on top of the document's font style). Underline is not a cosmic attr — drawn later.
        let mono_attr = |color: CColor, attr: u8| -> Attrs {
            let base = match &fam {
                Some(n) => Attrs::new().family(Family::Name(n)),
                None => Attrs::new().family(Family::Monospace),
            };
            let weight = if attr & xyscript_app::document::ATTR_BOLD != 0 {
                cosmic_text::Weight::BOLD
            } else {
                e_weight
            };
            let style = if attr & xyscript_app::document::ATTR_ITALIC != 0 {
                cosmic_text::Style::Italic
            } else {
                e_style
            };
            base.weight(weight).style(style).color(color)
        };
        // Foreground colour (`FGCOLOR`): the default text colour. Syntax-highlighted tokens keep
        // their theme colour; un-kinded (plain) text uses the configured foreground.
        let (fr, fg_, fb) = self.editor.fg_color();
        let fg_c = CColor::rgb(fr, fg_, fb);
        // A token's colour comes from the live highlight theme (`HLCOLOR`); a kind that isn't
        // themed — and all un-kinded text — draws in `FGCOLOR`. `BGCOLOR` is the canvas underneath.
        let col = |k: Option<&TokenKind>| -> CColor {
            match k.and_then(|kind| self.editor.highlight_color(kind)) {
                Some((r, g, b)) => CColor::rgb(r, g, b),
                None => fg_c,
            }
        };
        // Transient `FGCOLOR`-on-selection spans, rebased to window-local display offsets exactly
        // like the tokens above. They OVERRIDE the syntax colour for their characters.
        let manual: Vec<(usize, usize, CColor)> = self
            .editor
            .color_spans()
            .iter()
            .filter_map(|&(ms, me, (r, g, b))| {
                let s = ms.max(lo);
                let e = me.min(hi);
                if s >= e {
                    return None;
                }
                let sd = decoded_to_display(s - lo, &win.cr);
                let ed = decoded_to_display(e - lo, &win.cr);
                (sd < ed).then(|| (sd, ed, CColor::rgb(r, g, b)))
            })
            .collect();
        // EMPH emphasis match text: every occurrence's glyph colour (`EMPHFGCOLOR`, default
        // `SELFGCOLOR`). Like `find_spans` but host-driven and persistent; painted UNDER FIND (a live
        // search wins over standing emphasis). `emph_highlights` are ROPE offsets.
        let emph_spans: Vec<(usize, usize, CColor)> = if self.emph_highlights.is_empty() {
            Vec::new()
        } else {
            let doc = self.editor.doc(self.editor.active_index());
            let (er, eg, eb) = self.editor.emph_fg_color();
            let emph_c = CColor::rgb(er, eg, eb);
            self.emph_highlights
                .iter()
                .filter_map(|&(ms_rope, me_rope)| {
                    let s = doc.decoded_offset(ms_rope).max(lo);
                    let e = doc.decoded_offset(me_rope).min(hi);
                    if s >= e {
                        return None;
                    }
                    let sd = decoded_to_display(s - lo, &win.cr);
                    let ed = decoded_to_display(e - lo, &win.cr);
                    (sd < ed).then_some((sd, ed, emph_c))
                })
                .collect()
        };
        // URLHIGHLIGHT match text: every URL's glyph colour (`URLFGCOLOR`, default link blue). Same
        // shape as `emph_spans`; painted under FIND so a live search still wins. `url_highlights` are
        // ROPE offsets.
        let url_spans: Vec<(usize, usize, CColor)> = if self.url_highlights.is_empty() {
            Vec::new()
        } else {
            let doc = self.editor.doc(self.editor.active_index());
            let (ur, ug, ub) = self.editor.url_fg_color();
            let url_c = CColor::rgb(ur, ug, ub);
            self.url_highlights
                .iter()
                .filter_map(|&(ms_rope, me_rope)| {
                    let s = doc.decoded_offset(ms_rope).max(lo);
                    let e = doc.decoded_offset(me_rope).min(hi);
                    if s >= e {
                        return None;
                    }
                    let sd = decoded_to_display(s - lo, &win.cr);
                    let ed = decoded_to_display(e - lo, &win.cr);
                    (sd < ed).then_some((sd, ed, url_c))
                })
                .collect()
        };
        // FIND-highlight match text: the glyph colour of highlighted matches — `FINDFGCOLOR` for
        // ordinary hits, `SELFGCOLOR` for the current (Enter/F3 target). `find_highlights` are ROPE
        // offsets, so convert to decoded before rebasing to window-local display (like `manual`).
        let find_spans: Vec<(usize, usize, CColor)> = if self.find_highlights.is_empty() {
            Vec::new()
        } else {
            let doc = self.editor.doc(self.editor.active_index());
            let (ffr, ffg, ffb) = self.editor.find_fg_color();
            let (sfr2, sfg2, sfb2) = self.editor.sel_fg_color();
            let (find_c, cur_c) = (CColor::rgb(ffr, ffg, ffb), CColor::rgb(sfr2, sfg2, sfb2));
            self.find_highlights
                .iter()
                .filter_map(|&(ms_rope, me_rope)| {
                    let s = doc.decoded_offset(ms_rope).max(lo);
                    let e = doc.decoded_offset(me_rope).min(hi);
                    if s >= e {
                        return None;
                    }
                    let sd = decoded_to_display(s - lo, &win.cr);
                    let ed = decoded_to_display(e - lo, &win.cr);
                    let c = if self.find_current == Some((ms_rope, me_rope)) { cur_c } else { find_c };
                    (sd < ed).then_some((sd, ed, c))
                })
                .collect()
        };
        // Multi-caret SECONDARY selections: recolour their glyphs to `SELFGCOLOR`, matching the
        // primary selection (which cosmic recolours at draw time). Caret offsets are ROPE offsets, so
        // convert to decoded then rebase to window-local display, like `emph_spans`/`find_spans`.
        let sec_sel_spans: Vec<(usize, usize, CColor)> = {
            let doc = self.editor.doc(self.editor.active_index());
            if doc.is_multi() {
                let (sfr3, sfg3, sfb3) = self.editor.sel_fg_color();
                let sel_c = CColor::rgb(sfr3, sfg3, sfb3);
                doc.secondary_carets()
                    .iter()
                    .filter_map(|c| {
                        let a = c.anchor?;
                        let s = doc.decoded_offset(a.min(c.pos)).max(lo);
                        let e = doc.decoded_offset(a.max(c.pos)).min(hi);
                        if s >= e {
                            return None;
                        }
                        let sd = decoded_to_display(s - lo, &win.cr);
                        let ed = decoded_to_display(e - lo, &win.cr);
                        (sd < ed).then_some((sd, ed, sel_c))
                    })
                    .collect()
            } else {
                Vec::new()
            }
        };
        // `MDA` text attributes (bold/italic/underline) per selection, rebased to window-local display
        // offsets like `manual`. `attr_spans` are DECODED offsets. Bold/italic feed the per-glyph
        // weight/slant below; the underline bit is drawn as a rect in `raster` (cosmic has no glyph
        // underline attr, same as the lint underlines).
        let attr_disp: Vec<(usize, usize, u8)> = self
            .editor
            .attr_spans()
            .iter()
            .filter_map(|&(ms, me, bits)| {
                let s = ms.max(lo);
                let e = me.min(hi);
                if s >= e {
                    return None;
                }
                let sd = decoded_to_display(s - lo, &win.cr);
                let ed = decoded_to_display(e - lo, &win.cr);
                (sd < ed).then_some((sd, ed, bits))
            })
            .collect();
        // Effective colour per display byte: default FGCOLOR, overridden by the token colour, then a
        // manual FGCOLOR span, then a FIND-match colour (highest priority). Filled from char-aligned
        // ranges, so grouping by char below stays on valid UTF-8 boundaries.
        let n = win.display.len();
        let mut colors = vec![fg_c; n];
        let runs = color_runs(&win.display, &wt);
        for &(a, b, k) in &runs {
            let c = col(k);
            colors[a..b.min(n)].iter_mut().for_each(|slot| *slot = c);
        }
        for &(s, e, cc) in &manual {
            colors[s..e.min(n)].iter_mut().for_each(|slot| *slot = cc);
        }
        for &(s, e, cc) in &emph_spans {
            colors[s..e.min(n)].iter_mut().for_each(|slot| *slot = cc);
        }
        for &(s, e, cc) in &url_spans {
            colors[s..e.min(n)].iter_mut().for_each(|slot| *slot = cc);
        }
        for &(s, e, cc) in &find_spans {
            colors[s..e.min(n)].iter_mut().for_each(|slot| *slot = cc);
        }
        // Secondary-caret selections read as selections, so their glyph colour wins over syntax/find,
        // matching the primary selection's SELFGCOLOR.
        for &(s, e, cc) in &sec_sel_spans {
            colors[s..e.min(n)].iter_mut().for_each(|slot| *slot = cc);
        }
        // Per display byte: the `MDA` bold/italic bits (underline is drawn separately). Grouped with
        // the colour into the AttrsList runs below, so a bold/italic block shapes with the right face.
        let mut cattrs = vec![0u8; n];
        for &(s, e, bits) in &attr_disp {
            cattrs[s..e.min(n)].iter_mut().for_each(|slot| *slot = bits);
        }
        // `MDA RV` reverse video: paint the glyph in the current BGCOLOR (the background rect is drawn
        // in FGCOLOR in `raster`). Dynamic — reads the live editor colours, so it stays inverted across
        // a theme change. Applied LAST so a reversed run always reads as inverted.
        let rev_glyph = {
            let (br, bg_, bb) = self.editor.bg_color();
            CColor::rgb(br, bg_, bb)
        };
        for i in 0..n {
            if cattrs[i] & xyscript_app::document::ATTR_REVERSE != 0 {
                colors[i] = rev_glyph;
            }
        }
        // ---- Incremental per-line build (the shaped-line cache) --------------------------------
        // Rather than `set_rich_text` the whole window every frame (which RE-SHAPES every line — the
        // killer on a 25 000-char paragraph), split the window into one `BufferLine` per logical line
        // and REUSE lines whose text + colours match the persistent buffer's, so their cached shaped
        // layout survives. A long paragraph then shapes once and scrolling reuses it. We can't call
        // `set_rich_text` (it resets every line's cache), so its splitting/attrs are replicated here.
        use std::hash::{Hash, Hasher};
        let s = self.render_scale;
        let metrics = Metrics::new(self.font_size * s, self.line_height * s);
        // Shaping-parameter key: any change here (width, wrap, font face/style/size) invalidates every
        // cached layout, so the reuse pool is dropped wholesale.
        let global_sig = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            w.hash(&mut h);
            self.editor.wrap().hash(&mut h);
            fam.hash(&mut h);
            format!("{e_weight:?}{e_style:?}").hash(&mut h);
            (self.font_size * s).to_bits().hash(&mut h);
            (self.line_height * s).to_bits().hash(&mut h);
            h.finish()
        };
        let (pool, pool_sigs): (Vec<BufferLine>, Vec<u64>) = match self.win_buffer.take() {
            Some(b) if self.win_global_sig == global_sig => (b.lines, std::mem::take(&mut self.win_sigs)),
            _ => (Vec::new(), Vec::new()),
        };
        self.win_global_sig = global_sig;
        // Index the pool by per-line signature (a multimap — identical lines are each reused once).
        let mut by_sig: std::collections::HashMap<u64, Vec<BufferLine>> = std::collections::HashMap::new();
        for (line, sig) in pool.into_iter().zip(pool_sigs) {
            by_sig.entry(sig).or_default().push(line);
        }
        // Byte ranges of the window's logical lines (split on '\n'). A mid-document window ends with the
        // separator '\n'; that trailing empty is an artifact — drop it, EXCEPT at true EOF where cosmic
        // keeps a trailing empty line so the last line stays reachable.
        let display: &str = &win.display;
        let mut segs: Vec<(usize, usize)> = Vec::new();
        let mut p = 0usize;
        loop {
            match display[p..].find('\n') {
                Some(off) => {
                    segs.push((p, p + off));
                    p += off + 1;
                }
                None => {
                    segs.push((p, display.len()));
                    break;
                }
            }
        }
        if display.ends_with('\n') && !win.at_eof {
            segs.pop();
        }
        let mut new_lines: Vec<BufferLine> = Vec::with_capacity(segs.len());
        let mut new_sigs: Vec<u64> = Vec::with_capacity(segs.len());
        for (li, &(a, b)) in segs.iter().enumerate() {
            let text = &display[a..b];
            let lc = &colors[a..b];
            let la = &cattrs[a..b];
            let sig = {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                text.hash(&mut h);
                lc.hash(&mut h);
                la.hash(&mut h);
                h.finish()
            };
            let line = by_sig.get_mut(&sig).and_then(|v| v.pop()).unwrap_or_else(|| {
                // Cache miss: build the line's `AttrsList` by grouping runs with the same (colour,
                // attribute) pair, matching what `set_rich_text` produced from the window-wide spans.
                let mut al = AttrsList::new(&mono());
                let (mut rs, mut rc): (usize, Option<(CColor, u8)>) = (0, None);
                for (i, _) in text.char_indices() {
                    let key = (lc[i], la[i]);
                    if rc != Some(key) {
                        if let Some((pc, pa)) = rc {
                            al.add_span(rs..i, &mono_attr(pc, pa));
                        }
                        rs = i;
                        rc = Some(key);
                    }
                }
                if let Some((pc, pa)) = rc {
                    al.add_span(rs..text.len(), &mono_attr(pc, pa));
                }
                let mut bl =
                    BufferLine::new(text.to_string(), LineEnding::default(), al, Shaping::Advanced);
                bl.set_metadata(win.start_line + li);
                bl
            });
            new_lines.push(line);
            new_sigs.push(sig);
        }
        // An empty window still needs one (empty) line, matching cosmic's `set_text`.
        if new_lines.is_empty() {
            new_lines.push(BufferLine::new(
                String::new(),
                LineEnding::default(),
                AttrsList::new(&mono()),
                Shaping::Advanced,
            ));
            new_sigs.push(0);
        }
        self.win_sigs = new_sigs;
        // `w`/`h` are PIXELS; scale the font metrics to match so glyphs rasterize at device resolution
        // (crisp). All cosmic-space geometry (caret/hit/scroll) is then in pixels.
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.lines = new_lines;
        // `WRAP off` — lay each logical line out on ONE visual row (no width clamp), so it runs past the
        // right edge; the horizontal scrollbar reaches the overflow. `WRAP on` (default) wraps to text
        // width. cosmic's default is `WordOrGlyph`, set explicitly so a toggle takes effect.
        if self.editor.wrap() {
            buffer.set_wrap(Wrap::WordOrGlyph);
            buffer.set_size(Some(w as f32), Some(h as f32));
        } else {
            buffer.set_wrap(Wrap::None);
            buffer.set_size(None, Some(h as f32));
        }
        // Visual tab stops: render `\t` at the configured width (`TB`/`$TB`). cosmic computes the stops
        // from the space advance, so caret/hit-testing stay consistent automatically.
        let tw = self.editor.tab_width().clamp(1, u16::MAX as usize) as u16;
        buffer.set_tab_width(tw); // reshaped by the later `shape_until_scroll`
        buffer
    }

    /// The per-frame entry point: window the document around `scroll_line`, build the
    /// cosmic buffer for it, and apply the scroll (translated to window-local space,
    /// then normalized back to global). Returns the shaped buffer and its window.
    fn windowed_buffer(&mut self, w: usize, h: usize) -> (Buffer, DocWindow) {
        let win = self.compute_window(h);
        let mut buffer = self.build_window_cosmic(w, h, &win);
        let rel = self.pane_view.scroll_line.saturating_sub(win.start_line);
        buffer.set_scroll(Scroll::new(rel, self.pane_view.scroll_vertical, 0.0));
        buffer.shape_until_scroll(&mut self.font_system, false);
        // `WRAPINDENT`/`WRAPHANG`: narrow the indented lines + build the offset tables so EVERY consumer
        // of the windowed buffer (vertical motion, visual line-edge, hit-test) reads the same wrapped
        // layout that gets drawn. `raster` re-runs this after its follow-refine reshape too.
        self.refresh_wrap_indent(&mut buffer, &win, w as f32);
        // The `shape_until_scroll` above normalized the scroll against the UN-narrowed layout; the
        // narrowing just now wraps hanging-indent lines into MORE rows, so the scroll must be re-clamped
        // against the FINAL (narrowed, cached) layout — otherwise the drawn caret drifts off-screen at
        // the file's end (`refresh_wrap_indent` also rebuilds `wi_rows` for the settled scroll). When no
        // line is narrowed this second pass is a cheap no-op (nothing dirty, nothing to rebuild).
        buffer.set_scroll(Scroll::new(rel, self.pane_view.scroll_vertical, 0.0));
        buffer.shape_until_scroll(&mut self.font_system, false);
        let s = buffer.scroll();
        self.pane_view.scroll_line = win.start_line + s.line;
        self.pane_view.scroll_vertical = s.vertical;
        self.refresh_wrap_indent(&mut buffer, &win, w as f32);
        (buffer, win)
    }

    /// Run [`build_wrap_indent`](Self::build_wrap_indent) when `WRAPINDENT` is on, else clear its
    /// per-frame tables. MUST be called on the FINAL shaped layout (after any `shape_until_cursor`
    /// follow-refine), so the narrowed per-line layout the geometry reads is the one that gets drawn —
    /// otherwise a later reshape silently un-narrows the lines and the offsets no longer match.
    fn refresh_wrap_indent(&mut self, buffer: &mut Buffer, win: &DocWindow, w: f32) {
        // Hanging-indent only makes sense while wrapping; `WRAP off` has no continuation rows, and
        // re-narrowing the width here would re-introduce wrapping — so treat it as off.
        if self.editor.wrap() {
            if self.editor.wrap_indent() {
                // WRAPINDENT: hang continuation rows under each line's OWN measured indent.
                self.build_wrap_indent(buffer, win, w, None);
                return;
            }
            let hang = self.editor.wrap_hang();
            if hang > 0 {
                // WRAPHANG <n>: a FIXED n-space hang for every line (n spaces → px via the monospace
                // cell advance, the same convention margins/caret use).
                let cell = self.font_size * self.render_scale * 0.6;
                self.build_wrap_indent(buffer, win, w, Some(hang as f32 * cell));
                return;
            }
        }
        if !self.wi_rows.is_empty() || !self.wi_indent.is_empty() {
            self.wi_rows.clear();
            self.wi_indent.clear();
        }
    }

    /// `WRAPINDENT` core: for each visible display line with leading indentation, re-lay-out that line
    /// at a wrap width reduced by its indent (so a wrapped row reserves room to hang under the text),
    /// and build the per-row offset table [`wi_rows`](Self::wi_rows) + per-line indent
    /// [`wi_indent`](Self::wi_indent). The indent width is measured from cosmic's own layout (the pixel
    /// x where content begins on the first row), so it's exact and tab-aware. Deep indents are clamped
    /// to half the width so a continuation row always has room.
    fn build_wrap_indent(&mut self, buffer: &mut Buffer, win: &DocWindow, w: f32, fixed_hang: Option<f32>) {
        self.wi_indent.clear();
        self.wi_rows.clear();
        let line_count = win.nl.len() + 1;
        self.wi_indent.resize(line_count, 0.0);
        // Phase 1 — indent pixels per line. `WRAPHANG` (fixed_hang) uses the SAME n-space hang for every
        // line; `WRAPINDENT` (None) MEASURES each line's own leading-whitespace width from cosmic's
        // layout (`ws_len` is the leading-ws byte length — spaces/tabs are ASCII → one byte each — and
        // the content's start x on row 0 is exactly the hang width). Both clamp to half the width so a
        // continuation row always has room.
        if let Some(px) = fixed_hang {
            let ind = px.clamp(0.0, w * 0.5);
            if ind > 0.5 {
                self.wi_indent.iter_mut().for_each(|v| *v = ind);
            }
        } else {
            for li in 0..line_count {
                let ls = if li > 0 { win.nl[li - 1] + 1 } else { 0 };
                let le = win.nl.get(li).copied().unwrap_or(win.display.len());
                let text = &win.display[ls..le];
                let ws_len = text.len() - text.trim_start_matches([' ', '\t']).len();
                if ws_len == 0 {
                    continue;
                }
                // Content-start x = the hang width. Read it from the line's OWN shaped layout (row 0,
                // first glyph at/after the leading whitespace) rather than `cursor_position`, which
                // clips to the visible viewport and returns None — hence indent 0 — for any off-screen
                // line. That mattered: a follow scroll to the file's end reveals long indented lines
                // that were below the fold, and if those are left un-narrowed the scroll is computed on
                // a shorter layout than the one drawn, shoving the caret off-screen. Measuring straight
                // from the layout narrows EVERY window line, so scroll and render agree.
                let ind = buffer
                    .line_layout(&mut self.font_system, li)
                    .and_then(|rows| rows.first())
                    .and_then(|row0| row0.glyphs.iter().find(|g| g.start >= ws_len))
                    .map_or(0.0, |g| g.x)
                    .clamp(0.0, w * 0.5);
                if ind > 0.5 {
                    self.wi_indent[li] = ind;
                }
            }
        }
        // Phase 2 — re-lay-out each indented line at the narrowed width. `layout` caches by
        // invalidation (not by width), so these per-line layouts stick and cosmic's `layout_runs`
        // /`cursor_position`/`hit` all read them. Params mirror `Buffer::line_layout`'s defaults.
        let wrap = buffer.wrap();
        let fsz = buffer.metrics().font_size;
        let tw = self.editor.tab_width().clamp(1, u16::MAX as usize) as u16;
        for li in 0..line_count {
            let ind = self.wi_indent[li];
            if ind <= 0.5 {
                continue;
            }
            if let Some(line) = buffer.lines.get_mut(li) {
                line.reset_layout();
                line.layout(
                    &mut self.font_system,
                    fsz,
                    Some((w - ind).max(1.0)),
                    wrap,
                    Ellipsize::None,
                    None,
                    tw,
                    Hinting::default(),
                );
            }
        }
        // Phase 3 — row→offset table over the (re-laid-out) visible runs. The FIRST visual row of a
        // logical line sits at the margin (offset 0); its wrapped continuation rows hang at the indent.
        let mut prev: Option<usize> = None;
        for run in buffer.layout_runs() {
            let first = prev != Some(run.line_i);
            prev = Some(run.line_i);
            let offset = if first { 0.0 } else { self.wi_indent.get(run.line_i).copied().unwrap_or(0.0) };
            self.wi_rows.push((run.line_top, run.line_top + run.line_height, offset));
        }
    }

    /// The `WRAPINDENT` hanging-indent x-offset for the visual row containing pixel `y` (0 when the
    /// mode is off, the row is a first row, or `y` is outside any recorded row).
    fn wi_offset_at_y(&self, y: f32) -> f32 {
        for &(top, bot, off) in &self.wi_rows {
            if y >= top && y < bot {
                return off;
            }
        }
        0.0
    }

    /// `WRAPINDENT` glyph draw: the manual equivalent of cosmic's `Editor::render` (selection band +
    /// glyphs + selected-text colour) but with each visual row shifted right by its hanging-indent
    /// offset — the first row of a logical line sits at the margin, its wrapped rows hang under the
    /// text. Mirrors cosmic's own render logic so the output matches the default path pixel-for-pixel
    /// when the offset is 0. The caret is painted separately by the caller (as on the default path).
    #[allow(clippy::too_many_arguments)]
    fn draw_indented(
        &mut self,
        buffer: &mut Buffer,
        canvas: &mut [u8],
        w: usize,
        h: usize,
        caret: Cursor,
        selection: Selection,
        text_c: CColor,
        sel_c: CColor,
        seltxt_c: CColor,
    ) {
        // Ordered selection bounds. The GUI only ever hands us `Normal`/`None`.
        let sel_bounds: Option<(Cursor, Cursor)> = match selection {
            Selection::Normal(anchor) => {
                let a = (caret.line, caret.index);
                let b = (anchor.line, anchor.index);
                Some(if a <= b { (caret, anchor) } else { (anchor, caret) })
            }
            _ => None,
        };
        let margin = w as f32;
        let mut prev: Option<usize> = None;
        for run in buffer.layout_runs() {
            let first = prev != Some(run.line_i);
            prev = Some(run.line_i);
            let off = if first { 0.0 } else { self.wi_indent.get(run.line_i).copied().unwrap_or(0.0) };
            let line_top = run.line_top as i32;
            let line_height = run.line_height as u32;

            // Selection band for this run (cosmic's `render` logic, shifted by `off`).
            if let Some((start, end)) = sel_bounds {
                if run.line_i >= start.line && run.line_i <= end.line {
                    let hls: Vec<(f32, f32)> = run.highlight(start, end).collect();
                    if hls.is_empty() && run.glyphs.is_empty() && end.line > run.line_i {
                        // A wholly-empty selected interior line: band from the indent to the margin.
                        blend_cell(canvas, w, h, off as i32, line_top, (margin - off).max(0.0) as u32, line_height, sel_c);
                    } else {
                        let len = hls.len();
                        for (idx, (x, width)) in hls.into_iter().enumerate() {
                            let min = (x + off) as i32;
                            // Extend the last band to the margin for a selection continuing past this line.
                            let max = if idx + 1 == len && end.line > run.line_i {
                                margin as i32
                            } else {
                                (x + width + off) as i32
                            };
                            blend_cell(canvas, w, h, min, line_top, (max - min).max(0) as u32, line_height, sel_c);
                        }
                    }
                }
            }

            // Glyphs — shifted by `off` via the physical-glyph offset; selected glyphs recolour.
            for glyph in run.glyphs.iter() {
                let physical = glyph.physical((off, run.line_y), 1.0);
                let mut gc = glyph.color_opt.unwrap_or(text_c);
                if text_c != seltxt_c {
                    if let Some((start, end)) = sel_bounds {
                        if run.line_i >= start.line
                            && run.line_i <= end.line
                            && (start.line != run.line_i || glyph.end > start.index)
                            && (end.line != run.line_i || glyph.start < end.index)
                        {
                            gc = seltxt_c;
                        }
                    }
                }
                self.swash.with_pixels(&mut self.font_system, physical.cache_key, gc, |gx, gy, pc| {
                    blend_cell(canvas, w, h, physical.x + gx, physical.y + gy, 1, 1, pc);
                });
            }
        }
    }

    /// Global rope offset → window-local cosmic [`Cursor`] (line is window-relative).
    /// `None` if the offset falls outside the window.
    fn win_cursor_at(&self, win: &DocWindow, rope_off: usize) -> Option<Cursor> {
        // Anchor the decode on the window's own (start_byte ↔ start_decoded) fixed point, so a
        // caret near the bottom of a big non-UTF-8 doc translates by walking the window, not the
        // whole file from a global checkpoint (which was ~186 ms per PageUp).
        let g = self.editor.doc(self.editor.active_index()).decoded_offset_from(
            rope_off,
            win.start_byte,
            win.start_decoded,
        );
        let wd = g.checked_sub(win.start_decoded)?;
        if wd > win.decoded_len {
            return None;
        }
        Some(byte_to_cursor(&win.display, decoded_to_display(wd, &win.cr), &win.nl))
    }

    /// Like [`win_cursor_at`] but clamps an out-of-window offset to the window's edge —
    /// for drawing a selection whose end runs off-screen (the highlight reaches the margin).
    fn win_cursor_clamped(&self, win: &DocWindow, rope_off: usize) -> Cursor {
        let g = self.editor.doc(self.editor.active_index()).decoded_offset_from(
            rope_off,
            win.start_byte,
            win.start_decoded,
        );
        let wd = g.saturating_sub(win.start_decoded).min(win.decoded_len);
        byte_to_cursor(&win.display, decoded_to_display(wd, &win.cr), &win.nl)
    }

    /// Window-local cosmic [`Cursor`] → global rope offset (mouse/hit-test result → caret).
    fn win_rope_at(&self, win: &DocWindow, cur: Cursor) -> usize {
        let disp = cursor_to_byte(&win.display, cur, &win.nl);
        let wd = display_to_decoded(disp, &win.cr, win.decoded_len);
        // Anchored inverse: walk forward from the window start (~a page), never the whole doc.
        self.editor.doc(self.editor.active_index()).rope_offset_from(
            win.start_decoded + wd,
            win.start_byte,
            win.start_decoded,
        )
    }

    /// Pixel rects `(x0,y0,x1,y1,is_current)` for the visible portions of the FIND matches
    /// (`self.find_highlights`), for `raster` to paint. Offsets clamp to the window, so matches
    /// off-screen contribute nothing; a match wrapped across visual rows yields one rect per row.
    /// Refresh the `EMPH` emphasis match set for the active document (cached on `emph_key`, so we
    /// only re-search when the active tab, its text, or its phrase changes). Cheap when unchanged.
    fn refresh_emph(&mut self) {
        // While a GREP streams into the active results tab, its buffer grows every frame, so its
        // revision changes every frame — recomputing the whole-buffer emphasis (a `find_all` scan)
        // each frame would be O(buffer) per frame, i.e. O(n²) over the search, which is what made a
        // big-tree GREP crawl. Suppress it while streaming (show no match highlight yet); it computes
        // ONCE when the search settles — `emph_key = None` forces that recompute on the next frame.
        if self.editor.grep_in_progress() && self.editor.active_is_grep_listing() {
            self.emph_highlights.clear();
            self.emph_key = None;
            return;
        }
        let ai = self.editor.active_index();
        let phrase = self.editor.active_emph();
        let regex = self.editor.active_emph_is_regex();
        let key = phrase.as_ref().map(|p| (ai, self.editor.doc_revision(), p.clone(), regex));
        if key != self.emph_key {
            // Delegate to the host's `emph_highlights` so the match semantics (case-insensitive,
            // whole-word) live in ONE place — never re-spell the flags here.
            self.emph_highlights =
                if phrase.is_some() { self.editor.emph_highlights() } else { Vec::new() };
            self.emph_key = key;
        }
    }

    /// Refresh the `URLHIGHLIGHT` match set for the active document (cached on `url_key`, like
    /// `refresh_emph`). Empty when highlighting is off. Suppressed while a GREP streams (same O(n²)
    /// concern as EMPH).
    fn refresh_url(&mut self) {
        if !self.editor.url_highlight()
            || (self.editor.grep_in_progress() && self.editor.active_is_grep_listing())
        {
            self.url_highlights.clear();
            self.url_key = None;
            return;
        }
        let key = Some((self.editor.active_index(), self.editor.doc_revision()));
        if key != self.url_key {
            self.url_highlights = self.editor.url_highlights();
            self.url_key = key;
        }
    }

    /// The URL text at rope offset `off`, if it falls within a highlighted URL span (for Ctrl+click).
    fn url_at_offset(&self, off: usize) -> Option<String> {
        let (s, e) = *self.url_highlights.iter().find(|&&(s, e)| off >= s && off < e)?;
        let doc = self.editor.doc(self.editor.active_index());
        Some(String::from_utf8_lossy(&doc.slice_bytes(s, e)).into_owned())
    }

    /// Convenience wrapper: FIND "highlight all" rects (the current match flagged).
    fn find_highlight_rects(
        &self,
        buffer: &Buffer,
        win: &DocWindow,
        w_px: i32,
        lh_px: i32,
    ) -> Vec<(i32, i32, i32, i32, bool)> {
        self.highlight_rects(&self.find_highlights, self.find_current, buffer, win, w_px, lh_px)
    }

    /// Rects (per visual row) for a set of rope-offset match `spans`, clamped to the visible window;
    /// the trailing `bool` flags the `current` match (FIND's Enter/F3 target — `None` for `EMPH`).
    fn highlight_rects(
        &self,
        spans: &[(usize, usize)],
        current: Option<(usize, usize)>,
        buffer: &Buffer,
        win: &DocWindow,
        w_px: i32,
        lh_px: i32,
    ) -> Vec<(i32, i32, i32, i32, bool)> {
        let lh_px = lh_px.max(1);
        // The visible window's rope-offset range (two cheap conversions). Matches outside it are
        // skipped with a plain integer compare — CRUCIAL: the per-match cursor lookups below are
        // expensive, and a common letter can have thousands of hits, almost all off-screen. Without
        // this pre-filter, every caret-blink repaint would re-look-up all of them and hang the UI.
        let doc = self.editor.doc(self.editor.active_index());
        let win_lo = doc.rope_offset(win.start_decoded);
        let win_hi = doc.rope_offset(win.start_decoded + win.decoded_len);
        let mut rects = Vec::new();
        for &(ms, me) in spans {
            if me <= ms || me < win_lo || ms > win_hi {
                continue; // zero-width, or entirely outside the visible window
            }
            let sc = self.win_cursor_clamped(win, ms);
            let ec = self.win_cursor_clamped(win, me);
            let (Some((sxf, syf)), Some((exf, eyf))) =
                (buffer.cursor_position(&sc), buffer.cursor_position(&ec))
            else {
                continue;
            };
            let (sx, sy, ex, ey) = (sxf as i32, syf as i32, exf as i32, eyf as i32);
            let current = current == Some((ms, me));
            // Bake the hanging-indent (`WRAPINDENT`/`WRAPHANG`) x-offset in HERE, from the RAW f32 row
            // top — not from the i32 `sy`. A row's `line_top` can carry a fractional part; truncating it
            // first can drop the lookup into the previous row's band (offset 0), shifting the band left
            // by the whole hang. The caret avoids this the same way (see `caret_px`). `wi_offset_at_y`
            // returns 0 when the mode is off (`wi_rows` empty), so this is a no-op then.
            let lh_f = lh_px as f32;
            if sy == ey {
                let o = self.wi_offset_at_y(syf) as i32;
                if ex > sx {
                    rects.push((sx + o, sy, ex + o, sy + lh_px, current));
                }
            } else {
                // Wrapped match: first row to the right margin, full middle rows, last row from the left.
                let o0 = self.wi_offset_at_y(syf) as i32;
                rects.push((sx + o0, sy, w_px + o0, sy + lh_px, current));
                let (mut y, mut yf) = (sy + lh_px, syf + lh_f);
                while y < ey {
                    let o = self.wi_offset_at_y(yf) as i32;
                    rects.push((o, y, w_px + o, y + lh_px, current));
                    y += lh_px;
                    yf += lh_f;
                }
                if ex > 0 {
                    let oe = self.wi_offset_at_y(eyf) as i32;
                    rects.push((oe, ey, ex + oe, ey + lh_px, current));
                }
            }
        }
        rects
    }

    /// Clamp each rect's right edge to the actual text extent on its visual row, so a span that crosses
    /// a soft-wrap stops under the last glyph rather than running out to the right margin. Used for the
    /// `MDA` underline and reverse-video rects — where `highlight_rects`' band-to-margin fill (right for
    /// a selection/FIND highlight) would otherwise paint under the empty margin past a wrap. The row's
    /// text-right edge is the layout run's `line_w` plus any hanging-indent offset. Call BEFORE the
    /// horizontal-scroll (`hshift`) adjustment, so the rects and the layout runs share coordinates.
    fn clamp_rects_to_text(&self, rects: &mut [(i32, i32, i32, i32)], buffer: &Buffer) {
        if rects.is_empty() {
            return;
        }
        let rows: Vec<(i32, i32)> = buffer
            .layout_runs()
            .map(|r| (r.line_top as i32, self.wi_offset_at_y(r.line_top) as i32 + r.line_w.ceil() as i32))
            .collect();
        for rect in rects.iter_mut() {
            if let Some(&(_, right)) = rows.iter().find(|&&(y, _)| (y - rect.1).abs() <= 1) {
                rect.2 = rect.2.min(right);
            }
        }
    }

    /// Single-glyph rects for the `BALPAIRS` matched-bracket pair at/adjacent to the caret. Each
    /// bracket is one ASCII char, so a `(p, p+1)` span on a single visual row — no wrap handling. Off-
    /// screen brackets clamp out to nothing. Empty unless `BALPAIRS` is on and the caret is on a
    /// balanced bracket.
    fn bracket_rects(
        &self,
        buffer: &Buffer,
        win: &DocWindow,
        lh_px: i32,
    ) -> Vec<(i32, i32, i32, i32)> {
        let (a, b) = match self.editor.bracket_match() {
            xyscript_app::BalMatch::Matched(a, b) => (a, b),
            _ => return Vec::new(),
        };
        let lh_px = lh_px.max(1);
        let doc = self.editor.doc(self.editor.active_index());
        let win_lo = doc.rope_offset(win.start_decoded);
        let win_hi = doc.rope_offset(win.start_decoded + win.decoded_len);
        let mut rects = Vec::new();
        for p in [a, b] {
            if p < win_lo || p > win_hi {
                continue; // outside the visible window
            }
            let sc = self.win_cursor_clamped(win, p);
            let ec = self.win_cursor_clamped(win, p + 1);
            let (Some((sxf, syf)), Some((exf, _eyf))) =
                (buffer.cursor_position(&sc), buffer.cursor_position(&ec))
            else {
                continue;
            };
            // Bake the hanging-indent x-offset in from the RAW f32 row top (see `highlight_rects` — a
            // truncated `sy` can land in the previous row's band and drop the whole hang). A bracket is
            // one char on a single visual row, so one offset suffices.
            let off = self.wi_offset_at_y(syf) as i32;
            let (sx, sy, ex) = ((sxf as i32) + off, syf as i32, (exf as i32) + off);
            if ex > sx {
                rects.push((sx, sy, ex, sy + lh_px));
            }
        }
        rects
    }

    /// Straight-underline rects for live language-check issues (`self.editor.lints()`), each tagged
    /// with whether it's a SPELLING issue (vs grammar/other) so `raster` picks the colour. Same
    /// window-clamped, per-visual-row geometry as [`find_highlight_rects`](Self::find_highlight_rects);
    /// `raster` draws a thin line at each rect's bottom edge.
    fn lint_underline_rects(
        &self,
        buffer: &Buffer,
        win: &DocWindow,
        w_px: i32,
        lh_px: i32,
    ) -> Vec<(i32, i32, i32, i32, bool)> {
        let lints = self.editor.lints();
        if lints.is_empty() {
            return Vec::new();
        }
        // The worker produces ALL kinds; draw only the categories currently toggled on
        // (`SPELLING`/`SP`, `GRAMMAR`/`GR`), so toggling a flag is an instant render change.
        let (show_spell, show_grammar) = (self.editor.spelling_on(), self.editor.grammar_on());
        let lh_px = lh_px.max(1);
        let active = self.editor.active_index();
        let doc = self.editor.doc(active);
        let win_lo = doc.rope_offset(win.start_decoded);
        let win_hi = doc.rope_offset(win.start_decoded + win.decoded_len);
        // Hide the underline (spelling AND grammar) for the word being actively typed: the word ending
        // at the caret (until a separator is typed, `word_before_cursor` is `Some`, so its underline is
        // deferred). Only in the focused pane — inactive panes always show every mark.
        let typing_word = (self.lint_focus_doc == Some(active))
            .then(|| doc.word_before_cursor())
            .flatten();
        let mut rects = Vec::new();
        for mark in lints {
            let is_spelling = mark.is_spelling();
            if !(if is_spelling { show_spell } else { show_grammar }) {
                continue; // this category is toggled off
            }
            // Coarse cull on the raw span, then CLIP whitespace against the LIVE buffer text: a mark
            // computed one revision ago (not yet replaced by the async re-check) can momentarily
            // straddle a just-typed space or newline — its offsets predate the edit, so against the new
            // text its leading edge falls on the inserted whitespace. Trimming here means the underline
            // never covers whitespace even for that one frame — no flash on <space>/<enter>.
            if mark.end <= mark.start || mark.end < win_lo || mark.start > win_hi {
                continue; // zero-width, or entirely outside the visible window
            }
            let (ms, me) = clip_ws_span(doc, mark.start, mark.end);
            if me <= ms {
                continue; // nothing left once whitespace is clipped
            }
            // Defer the underline for the word under construction — spelling or grammar — until a
            // separator finishes it, so nothing flickers under a half-typed word.
            if let Some((ws, we)) = typing_word {
                if ms < we && me > ws {
                    continue;
                }
            }
            let sc = self.win_cursor_clamped(win, ms);
            let ec = self.win_cursor_clamped(win, me);
            let (Some((sxf, syf)), Some((exf, eyf))) =
                (buffer.cursor_position(&sc), buffer.cursor_position(&ec))
            else {
                continue;
            };
            let (sx, sy, ex, ey) = (sxf as i32, syf as i32, exf as i32, eyf as i32);
            // Bake the hanging-indent x-offset in from the RAW f32 row top (see `highlight_rects`).
            let lh_f = lh_px as f32;
            if sy == ey {
                let o = self.wi_offset_at_y(syf) as i32;
                if ex > sx {
                    rects.push((sx + o, sy, ex + o, sy + lh_px, is_spelling));
                }
            } else {
                // Wrapped issue: first row to the right margin, full middle rows, last row from left.
                let o0 = self.wi_offset_at_y(syf) as i32;
                rects.push((sx + o0, sy, w_px + o0, sy + lh_px, is_spelling));
                let (mut y, mut yf) = (sy + lh_px, syf + lh_f);
                while y < ey {
                    let o = self.wi_offset_at_y(yf) as i32;
                    rects.push((o, y, w_px + o, y + lh_px, is_spelling));
                    y += lh_px;
                    yf += lh_f;
                }
                if ex > 0 {
                    let oe = self.wi_offset_at_y(eyf) as i32;
                    rects.push((oe, ey, ex + oe, ey + lh_px, is_spelling));
                }
            }
        }
        rects
    }

    fn raster(&mut self, w: usize, h: usize) -> Vec<u8> {
        // `w`/`h` are PIXELS; cosmic geometry is in pixels, so use the scaled line height.
        let scale = self.render_scale;
        let lh_px = self.line_height * scale;
        let visible_rows = ((h as f32 / lh_px).floor() as usize).max(1);
        // Auto-scroll the Console to the tail when new output or a fresh prompt just arrived. A
        // foreground command's output appends BELOW the (consumed) prompt, so without this it streams
        // off the bottom of the window; re-arming `follow` each dirty frame keeps the tail — and the
        // next prompt — in view. Drain the flag regardless so it can't go stale; only follow when the
        // Console is the active tab (its caret sits at the tail, so `follow` reveals it).
        let console_dirty = self.editor.take_console_dirty();
        // An explicit `SH`/`SHELL` open forces the view to the prompt even if the user had scrolled up
        // to read old output (e.g. a long `X/BG` log) — scroll-lock is for passive streaming, not for a
        // deliberate "take me to the prompt." Drain the flag unconditionally so it can't go stale.
        let console_reveal_tail = self.editor.take_console_reveal_tail();
        if console_reveal_tail && self.editor.console_is_active() {
            self.console_at_bottom = true;
            self.pane_view.follow = true;
        }
        if console_dirty && self.editor.console_is_active() && self.console_at_bottom {
            self.pane_view.follow = true;
        }
        // Capture `follow` up front: the vertical-follow block below CONSUMES it (sets it false), but the
        // horizontal caret-follow (for `WRAP off`, further down) needs the same signal.
        let following = self.pane_view.follow;

        // Follow: nudge `scroll_line` toward the caret BEFORE we window, so the window
        // contains the caret and shape_until_cursor can find it. (A far jump — go-to-end
        // — lands here too: one cheap logical snap, then a visual refine below.)
        // Set when a FIND match was off-screen and should be re-centered in VISUAL space by the follow
        // refine below (the logical-line snap here only brings the window to the caret; wrapping makes
        // logical lines a poor proxy for screen rows).
        let mut center_offscreen = false;
        if self.pane_view.follow {
            let cline = self.editor.doc(self.editor.active_index()).line().saturating_sub(1);
            let top = self.pane_view.scroll_line;
            let bot = self.pane_view.scroll_line + visible_rows;
            let offscreen = cline < top || cline >= bot;
            if offscreen {
                // Scroll MINIMALLY to bring the caret to the nearest edge — so stepping just past the
                // top/bottom scrolls one line and the caret stays at that edge — but RECENTER on a far
                // jump (more than a page out, e.g. go-to-end) so the caret lands with context. A FIND
                // match (`center_caret`) is snapped roughly mid so the window contains it; the follow
                // refine then centers it precisely in visual rows.
                let far = cline + visible_rows < top || cline >= bot + visible_rows;
                self.pane_view.scroll_line = if self.center_caret || far {
                    cline.saturating_sub(visible_rows / 2)
                } else if cline < top {
                    cline // caret to the top edge
                } else {
                    (cline + 1).saturating_sub(visible_rows) // caret to the bottom edge
                };
                self.pane_view.scroll_vertical = 0.0;
            }
            center_offscreen = self.center_caret && offscreen;
            self.center_caret = false; // one-shot: consumed whether or not it triggered a scroll
        }

        // Keep the "current" FIND match synced with the LIVE caret this very frame (the dialog's
        // recompute runs after `raster`, so relying on it lags one frame behind the caret/selection
        // — which made a match flash unhighlighted for a frame as the caret left it). Recompute from
        // the fresh caret so a match reverts to the highlight colour the instant Enter/F3 moves off it.
        if !self.find_highlights.is_empty() {
            let caret = self.editor.doc(self.editor.active_index()).cursor();
            self.find_current = self
                .find_highlights
                .iter()
                .copied()
                .find(|&(s, e)| s <= caret && caret <= e)
                .or_else(|| self.find_highlights.iter().copied().find(|&(s, _)| s >= caret))
                .or_else(|| self.find_highlights.first().copied());
        }

        let (mut buffer, win) = self.windowed_buffer(w, h);
        let doc_cursor = self.editor.doc(self.editor.active_index()).cursor();
        // The real caret as a window-local cosmic cursor (None if scrolled off-window).
        let cursor = self.win_cursor_at(&win, doc_cursor);

        // Follow refine: scroll in VISUAL (wrapped) space so the caret's row is visible.
        if self.pane_view.follow {
            if let Some(c) = cursor {
                buffer.shape_until_cursor(&mut self.font_system, c, false);
                let s = buffer.scroll();
                self.pane_view.scroll_line = win.start_line + s.line;
                self.pane_view.scroll_vertical = s.vertical;
                // A FIND match that was off-screen: `shape_until_cursor` left it at the nearest EDGE.
                // Nudge the scroll (page_move-style pixel offset, folded into line+residual) so it sits
                // ~2/5 down the screen — measured in real screen rows, so it centres even with wrapping.
                if center_offscreen {
                    if let Some((_, y)) = buffer.cursor_position(&c) {
                        self.pane_view.scroll_vertical += y - h as f32 * 0.40;
                        let rel = self.pane_view.scroll_line.saturating_sub(win.start_line);
                        buffer.set_scroll(Scroll::new(rel, self.pane_view.scroll_vertical, 0.0));
                        buffer.shape_until_scroll(&mut self.font_system, false);
                        let s = buffer.scroll();
                        self.pane_view.scroll_line = win.start_line + s.line;
                        self.pane_view.scroll_vertical = s.vertical;
                    }
                }
            }
            self.pane_view.follow = false;
        }
        // Snap the vertical scroll to a WHOLE PIXEL so the glyph rows and the solid highlight/selection
        // bands land on one integer grid. cosmic rounds each glyph to whole pixels when it draws, but the
        // bands are truncated from the float layout via `cursor_position`; if `scroll_vertical` carries a
        // sub-pixel residual the two cross pixel boundaries at DIFFERENT thresholds, so the bands flip a
        // pixel against otherwise-stationary text. The follow refine re-settles that residual to a
        // slightly different value on every keystroke, which is the "Find hits jump around while editing
        // above them" report (worst on fractional-DPI displays, where the residual is largest — and why
        // it shows on some machines but not others). Reshape so `cursor_position` and the draw below both
        // read the snapped layout. Guarded on a real residual, so it's a no-op once settled (e.g. idle).
        let snapped = self.pane_view.scroll_vertical.round();
        if (snapped - self.pane_view.scroll_vertical).abs() > 0.01 {
            let rel = self.pane_view.scroll_line.saturating_sub(win.start_line);
            buffer.set_scroll(Scroll::new(rel, snapped, 0.0));
            buffer.shape_until_scroll(&mut self.font_system, false);
            let s = buffer.scroll();
            self.pane_view.scroll_line = win.start_line + s.line;
            self.pane_view.scroll_vertical = s.vertical;
        }
        // `WRAPINDENT`/`WRAPHANG`: rebuild the hanging-indent offset tables for the settled scroll —
        // after the follow refine, so `wi_rows` matches the final layout the caret/rects/draw below read.
        // The follow refine ran `shape_until_cursor` on the already-narrowed lines `windowed_buffer` left
        // cached, so the caret scroll was computed against the SAME narrowed layout that gets drawn; the
        // narrowing here is idempotent (lines already narrowed) and just refreshes `wi_rows`.
        self.refresh_wrap_indent(&mut buffer, &win, w as f32);
        // Feed the viewport hint (top line, visible rows) to the host so the KBD viewport
        // motions (HM/BS, and PD/PU when routed through the resolver) have the page size.
        self.editor.set_viewport(self.pane_view.scroll_line, visible_rows);

        // Persistent selection can decouple the caret from the block (the caret roams off
        // a frozen selection). cosmic ties its highlight to its cursor, so for the
        // decoupled state we draw the block with cosmic's cursor at the block's far end
        // and paint the real caret separately. Selection ends are clamped to the window
        // so an end scrolled off-screen still extends the highlight to the margin.
        let roamed = self.editor.doc(self.editor.active_index()).sel_head().is_some_and(|hd| hd != doc_cursor);
        // In a DIR/LS listing, highlight the WHOLE caret line (the entry under the cursor) like a
        // `DL` selection — but only for rendering, leaving the buffer's caret/selection untouched so
        // the listing's `cursor_line`-based navigation is unaffected. Tracks the caret automatically.
        let dir_line = self
            .editor
            .active_is_dir_listing()
            .then(|| self.editor.doc(self.editor.active_index()).line_span());
        let (cosmic_cursor, selection) = if let Some((a, b)) = dir_line {
            (
                self.win_cursor_clamped(&win, b),
                Selection::Normal(self.win_cursor_clamped(&win, a)),
            )
        } else {
            match self.editor.doc(self.editor.active_index()).selection_span() {
            Some((a, b)) if roamed => (
                self.win_cursor_clamped(&win, b),
                Selection::Normal(self.win_cursor_clamped(&win, a)),
            ),
            Some((a, b)) => {
                // cosmic highlights between its cursor (the real caret) and the anchor.
                // Use the selection end OPPOSITE the caret as the anchor: for a backward
                // selection (caret before anchor) the ordered start `a` *is* the caret, so
                // anchoring on `a` would collapse the highlight to empty (up/left wouldn't
                // appear to select). Pick whichever end isn't the caret.
                let anchor = if doc_cursor == a { b } else { a };
                (
                    cursor.unwrap_or_else(|| self.win_cursor_clamped(&win, doc_cursor)),
                    Selection::Normal(self.win_cursor_clamped(&win, anchor)),
                )
            }
            None => (cursor.unwrap_or(Cursor::new(0, 0)), Selection::None),
            }
        };

        // Paint the caret ourselves at the real cursor (cosmic's built-in caret draw was
        // unreliable beyond the first line). `None` ⇒ caret scrolled off-window, none drawn.
        // `WRAPINDENT`: shift by the caret row's hanging-indent offset so it sits with its text.
        let mut caret_px = cursor.and_then(|c| buffer.cursor_position(&c)).map(|(x, y)| {
            // `CARETMOTION` (visual): draw the caret at the visual x its last per-glyph step landed on, so at a
            // direction boundary it sits on the side we moved to — `cursor_position` alone collapses a
            // boundary index to one fixed side. Only while the caret is still at that offset.
            let x = match self.bidi_vx {
                Some((o, vx)) if self.editor.caret_visual() && o == doc_cursor => vx,
                _ => x,
            };
            (x + self.wi_offset_at_y(y), y)
        });

        // Shape the caret's VISUAL (wrapped) line for `RL` (delete visual line): the rope-byte range
        // `[start of the caret's row, start of the next row)`. Only under `WRAP` — with wrap off a
        // visual line IS the logical line, so `RL` falls back to that. Last visual row → to EOF (so it
        // takes the whole row + its break). Published to the host below (revision-stamped there).
        let vline_span: Option<(usize, usize)> = if self.editor.wrap() {
            cursor.and_then(|c| buffer.cursor_position(&c)).and_then(|(_x, cy)| {
                let lh_px = self.line_height * self.render_scale;
                let row_start =
                    buffer.hit(0.0, cy + 0.5 * lh_px).map(|cur| self.win_rope_at(&win, cur))?;
                let next = buffer.hit(0.0, cy + 1.5 * lh_px).map(|cur| self.win_rope_at(&win, cur));
                let end = match next {
                    Some(n) if n > row_start => n, // start of the next visual row
                    _ => self.editor.doc(self.editor.active_index()).len(), // last row → EOF
                };
                (end > row_start).then_some((row_start, end))
            })
        } else {
            None
        };

        // Publish the caret's VISUAL position (wrap-aware screen coords, in cells) for `$CX`/`$CY`.
        // Pixels → cells: cosmic geometry is at device resolution, so divide by the device line
        // height and the monospace cell width. Row is relative to the viewport top (canvas y=0).
        if let Some((cx, cy)) = caret_px {
            let lh_px = (self.line_height * self.render_scale).max(1.0);
            let cell_px = (self.font_size * self.render_scale * 0.6).max(1.0);
            let col = (cx / cell_px).round().max(0.0) as i64;
            let row = (cy / lh_px).round().max(0.0) as i64;
            self.editor.host_mut().set_cursor_visual(col, row);
        }

        // `WRAP off` — horizontal scrolling. Everything below is drawn shifted left by `hshift` px so a
        // window into a long line is visible; the bottom scrollbar (and Shift+wheel) drive `scroll_h`,
        // and typing/motion (`follow`) keeps the caret on-screen. `content_w` is the widest visible line
        // (for the scrollbar thumb + the scroll clamp). A no-op while wrapping (`hshift` 0, offsets skip).
        let hshift: i32 = if self.editor.wrap() {
            self.pane_view.scroll_h = 0.0;
            self.pane_view.content_w = 0.0;
            0
        } else {
            let content_w = buffer.layout_runs().map(|r| r.line_w).fold(0.0_f32, f32::max);
            let cell = (self.font_size * self.render_scale * 0.6).max(1.0);
            let mut sh = self.pane_view.scroll_h;
            if following {
                if let Some((cx, _)) = caret_px {
                    // Keep the caret one cell inside each edge (so you see context past it as you type).
                    if cx < sh + cell {
                        sh = (cx - cell).max(0.0);
                    } else if cx > sh + w as f32 - cell {
                        sh = cx - w as f32 + cell;
                    }
                }
            }
            sh = sh.clamp(0.0, (content_w - w as f32).max(0.0));
            self.pane_view.scroll_h = sh;
            self.pane_view.content_w = content_w;
            let hshift = sh.round() as i32;
            caret_px = caret_px.map(|(x, y)| (x - hshift as f32, y)); // caret draws in shifted space
            hshift
        };

        // Multi-caret: screen positions for the SECONDARY carets — the same mapping as the primary
        // `caret_px` above (window cursor → cosmic position + wrap-indent − hshift), skipping any
        // scrolled off-window. Painted alongside the primary in the caret block below.
        let sec_px: Vec<(f32, f32)> = {
            let active = self.editor.active_index();
            let offsets: Vec<usize> = if self.editor.cmline_open() {
                Vec::new()
            } else {
                let buf = self.editor.doc(active);
                if buf.is_multi() {
                    buf.secondary_carets().iter().map(|c| c.pos).collect()
                } else {
                    Vec::new()
                }
            };
            offsets
                .into_iter()
                .filter_map(|off| {
                    let doc = self.editor.doc(active);
                    let g = doc.decoded_offset_from(off, win.start_byte, win.start_decoded);
                    if g < win.start_decoded || g > win.start_decoded + win.decoded_len {
                        return None; // this caret is scrolled off-window
                    }
                    let cur = self.win_cursor_clamped(&win, off);
                    buffer
                        .cursor_position(&cur)
                        .map(|(x, y)| (x + self.wi_offset_at_y(y) - hshift as f32, y))
                })
                .collect()
        };

        // Multi-caret: selection BANDS for the SECONDARY carets (their per-caret selections, e.g. from
        // CARETALL / CARETNEXT / Shift+motion), rendered like the primary selection — an opaque
        // SELBGCOLOR band behind the glyphs. Same borrow window + rope-offset spans as the emph/find
        // rects. Empty unless a selection-caret set is active. (Only the PRIMARY selection gets
        // cosmic's selected-text recolour; secondary selected text keeps the normal foreground.)
        let mut sec_sel_rects: Vec<(i32, i32, i32, i32, bool)> = {
            let active = self.editor.active_index();
            let buf = self.editor.doc(active);
            if !self.editor.cmline_open() && buf.is_multi() {
                let spans: Vec<(usize, usize)> = buf
                    .secondary_carets()
                    .iter()
                    .filter_map(|c| c.anchor.map(|a| (a.min(c.pos), a.max(c.pos))))
                    .collect();
                if spans.is_empty() {
                    Vec::new()
                } else {
                    self.highlight_rects(&spans, None, &buffer, &win, w as i32, lh_px as i32)
                }
            } else {
                Vec::new()
            }
        };

        // FIND "highlight all" rects — computed while `buffer` is still freely borrowable, before
        // `ed` takes it mutably below. Empty unless the Find dialog is driving `find_highlights`.
        let mut find_rects = if self.find_highlights.is_empty() {
            Vec::new()
        } else {
            self.find_highlight_rects(&buffer, &win, w as i32, lh_px as i32)
        };
        // EMPH "emphasis" rects — same borrow window as `find_rects`; drawn behind the glyphs (below),
        // under FIND. No "current" match, so the flag is always false.
        let mut emph_rects = if self.emph_highlights.is_empty() {
            Vec::new()
        } else {
            self.highlight_rects(&self.emph_highlights, None, &buffer, &win, w as i32, lh_px as i32)
        };
        // URLHIGHLIGHT background band — only when `URLBGCOLOR` is set (otherwise URLs are just coloured
        // text, no band). Same borrow window as `emph_rects`.
        let mut url_rects = if self.url_highlights.is_empty() || self.editor.url_bg_color().is_none() {
            Vec::new()
        } else {
            self.highlight_rects(&self.url_highlights, None, &buffer, &win, w as i32, lh_px as i32)
        };
        // MD block-background rects (`MD/B` colour-on-selection). Persistent per-doc spans in ROPE
        // offsets — feed them one at a time through `highlight_rects` so each carries its own colour,
        // then tag every rect with that colour. Painted at the BOTTOM of the z-order (under EMPH/FIND/
        // selection), since these are user "block" backgrounds. Empty unless MD colours are set.
        let mut md_bg_rects: Vec<(i32, i32, i32, i32, (u8, u8, u8))> = {
            let bg = self.editor.bg_color_spans_raw();
            let mut out = Vec::new();
            for (ms, me, rgb) in bg {
                for (x0, y0, x1, y1, _) in
                    self.highlight_rects(&[(ms, me)], None, &buffer, &win, w as i32, lh_px as i32)
                {
                    out.push((x0, y0, x1, y1, rgb));
                }
            }
            out
        };
        // `MDA RV` reverse-video backgrounds — a rect in the current FGCOLOR behind each reversed span
        // (the glyphs are painted in BGCOLOR above). Same bottom layer as `md_bg_rects`.
        let mut mda_reverse_rects: Vec<(i32, i32, i32, i32)> = {
            let rv_bit = xyscript_app::document::ATTR_REVERSE;
            let mut out = Vec::new();
            for (ms, me, bits) in self.editor.attr_spans_raw() {
                if bits & rv_bit == 0 {
                    continue;
                }
                for (x0, y0, x1, y1, _) in
                    self.highlight_rects(&[(ms, me)], None, &buffer, &win, w as i32, lh_px as i32)
                {
                    out.push((x0, y0, x1, y1));
                }
            }
            out
        };
        // `MDA` underline rects — the wrap-aware pixel geometry of every attribute span carrying the
        // underline bit (drawn as a solid line under the glyphs, in the text colour, since cosmic has
        // no glyph underline attribute). Bold/italic ride the glyphs' AttrsList and need no rects here.
        let mut mda_underline_rects: Vec<(i32, i32, i32, i32)> = {
            let ul_bit = xyscript_app::document::ATTR_UNDERLINE;
            // `ULSPACES off` breaks the underline at inter-word spaces/tabs — draw only the non-blank
            // runs; `on` (default) underlines the whole span, spaces included.
            let ul_spaces = self.editor.ul_spaces();
            let doc_idx = self.editor.active_index();
            let mut out = Vec::new();
            for (ms, me, bits) in self.editor.attr_spans_raw() {
                if bits & ul_bit == 0 {
                    continue;
                }
                // The rope sub-ranges to underline: the whole span, or its non-blank runs.
                let ranges: Vec<(usize, usize)> = if ul_spaces {
                    vec![(ms, me)]
                } else {
                    let bytes = self.editor.doc(doc_idx).slice_bytes(ms, me);
                    let mut v = Vec::new();
                    let mut run: Option<usize> = None; // start of the current non-blank run (byte index)
                    for (i, &b) in bytes.iter().enumerate() {
                        // ASCII space/tab are standalone bytes (never UTF-8 continuation bytes), so a
                        // byte-level split stays on char boundaries for any encoding.
                        if b == b' ' || b == b'\t' {
                            if let Some(rs) = run.take() {
                                v.push((ms + rs, ms + i));
                            }
                        } else if run.is_none() {
                            run = Some(i);
                        }
                    }
                    if let Some(rs) = run {
                        v.push((ms + rs, me));
                    }
                    v
                };
                for (a, b) in ranges {
                    for (x0, y0, x1, y1, _) in
                        self.highlight_rects(&[(a, b)], None, &buffer, &win, w as i32, lh_px as i32)
                    {
                        out.push((x0, y0, x1, y1));
                    }
                }
            }
            out
        };
        // `highlight_rects` fills a soft-WRAPPED row out to the right margin (correct for a selection/
        // FIND band, but for an MDA underline or reverse-video block it paints under the empty margin
        // past the wrap). Clamp both to the actual text extent on each row so a wrapped span stops
        // under its last glyph. Done here — before the `hshift` loop — so rects and layout share coords.
        self.clamp_rects_to_text(&mut mda_underline_rects, &buffer);
        self.clamp_rects_to_text(&mut mda_reverse_rects, &buffer);
        // Record the visible byte window so the live checker can scope to it (bounded work on huge
        // files). Decoded-byte offsets, matching what `recheck_lints`/`compute_lints` expect.
        self.lint_visible = Some((win.start_decoded, win.start_decoded + win.decoded_len));
        // Live language-check underlines — same borrow window as `find_rects` (before `ed` takes the
        // buffer mutably); painted AFTER the glyphs (below). Empty unless `LINT on` found issues.
        let mut lint_rects = self.lint_underline_rects(&buffer, &win, w as i32, lh_px as i32);

        // BALPAIRS: rects for the matched pair (drawn behind the glyphs, below). Also surface an
        // "unmatched" note on the status line — but only on a *transition* into that state, so we
        // don't stomp all other status text on every frame.
        let bal = self.editor.bracket_match();
        if self.bal_last != Some(bal) {
            match bal {
                // Only a STRAY CLOSING bracket earns a note — that's a genuine orphan. An unmatched
                // OPENING bracket is just the normal "haven't typed the closer yet" state (the missing
                // pair-highlight already signals it), so stay quiet: otherwise the note popped up the
                // instant you typed '(' and sat there the whole time you paused before the contents.
                xyscript_app::BalMatch::Unmatched(_, ch) if !matches!(ch, b'(' | b'[' | b'{') => {
                    self.status = format!("No matching bracket for '{}'", ch as char);
                }
                // Left the flagged state (closed the pair, moved off it, or it's an unmatched opener we
                // now stay quiet about): drop our OWN note so it can't linger — guarded so a newer
                // status from another action isn't stomped.
                _ if self.status.starts_with("No matching bracket") => self.status.clear(),
                _ => {}
            }
            self.bal_last = Some(bal);
        }
        let mut bal_rects = self.bracket_rects(&buffer, &win, lh_px as i32);

        // NOTE: the `WRAPINDENT`/`WRAPHANG` hanging-indent x-offset is now baked into each rect at
        // construction (in `highlight_rects`/`bracket_rects`/`lint_underline_rects`) from the RAW f32
        // row top — NOT re-applied here from the truncated `r.1`, which could land in the wrong row's
        // band and shift a band left by the whole hang (the "every other paragraph" bracket bug).
        // `WRAP off` horizontal offset: slide the highlight/underline bands left with the glyphs.
        if hshift != 0 {
            for r in &mut find_rects {
                r.0 -= hshift;
                r.2 -= hshift;
            }
            for r in &mut emph_rects {
                r.0 -= hshift;
                r.2 -= hshift;
            }
            for r in &mut url_rects {
                r.0 -= hshift;
                r.2 -= hshift;
            }
            for r in &mut sec_sel_rects {
                r.0 -= hshift;
                r.2 -= hshift;
            }
            for r in &mut lint_rects {
                r.0 -= hshift;
                r.2 -= hshift;
            }
            for r in &mut bal_rects {
                r.0 -= hshift;
                r.2 -= hshift;
            }
            for r in &mut md_bg_rects {
                r.0 -= hshift;
                r.2 -= hshift;
            }
            for r in &mut mda_underline_rects {
                r.0 -= hshift;
                r.2 -= hshift;
            }
            for r in &mut mda_reverse_rects {
                r.0 -= hshift;
                r.2 -= hshift;
            }
        }


        // Background colour (`BGCOLOR`): pre-fill the canvas with it (opaque), then the text
        // alpha-blends over it. Foreground (`FGCOLOR`) is the default glyph colour.
        let (br, bg_, bb) = self.editor.bg_color();
        let (fr, fg_, fb) = self.editor.fg_color();
        let mut canvas = vec![0u8; w * h * 4];
        for px in canvas.chunks_exact_mut(4) {
            px.copy_from_slice(&[br, bg_, bb, 255]);
        }
        // MD block backgrounds — the BOTTOM highlight layer, painted directly over the canvas fill and
        // under everything else (EMPH/FIND/selection). Opaque, each rect in its own persisted colour.
        for &(x0, y0, x1, y1, (r, g, b)) in &md_bg_rects {
            fill_blend(&mut canvas, w, h, x0, y0, x1, y1, r, g, b, 1.0);
        }
        // `MDA RV` reverse-video backgrounds — an opaque FGCOLOR rect behind each reversed span (its
        // glyphs were set to BGCOLOR above), at the same bottom layer.
        if !mda_reverse_rects.is_empty() {
            let (fr2, fg2, fb2) = self.editor.fg_color();
            for &(x0, y0, x1, y1) in &mda_reverse_rects {
                fill_blend(&mut canvas, w, h, x0, y0, x1, y1, fr2, fg2, fb2, 1.0);
            }
        }
        // Paint EMPH emphasis BEHIND the glyphs (and under FIND): an opaque band in EMPHBGCOLOR
        // (default: the selection background) so the emphasis text (EMPHFGCOLOR) reads as configured.
        if !emph_rects.is_empty() {
            let (ebr, ebg, ebb) = self.editor.emph_bg_color();
            for &(x0, y0, x1, y1, _) in &emph_rects {
                fill_blend(&mut canvas, w, h, x0, y0, x1, y1, ebr, ebg, ebb, 1.0);
            }
        }
        // URLHIGHLIGHT background band — an opaque `URLBGCOLOR` fill behind each URL (only present when
        // that colour is set; see `url_rects`).
        if let Some((ubr, ubg, ubb)) = self.editor.url_bg_color() {
            for &(x0, y0, x1, y1, _) in &url_rects {
                fill_blend(&mut canvas, w, h, x0, y0, x1, y1, ubr, ubg, ubb, 1.0);
            }
        }
        // Multi-caret secondary selections — an opaque SELBGCOLOR band behind the glyphs, matching the
        // primary selection's look, so a CARETALL / CARETNEXT / Shift-extended set reads as N selections.
        if !sec_sel_rects.is_empty() {
            let (sbr, sbg, sbb) = self.editor.sel_bg_color();
            for &(x0, y0, x1, y1, _) in &sec_sel_rects {
                fill_blend(&mut canvas, w, h, x0, y0, x1, y1, sbr, sbg, sbb, 1.0);
            }
        }
        // Paint FIND matches BEHIND the glyphs: non-current hits get the translucent FINDBGCOLOR band
        // (default yellow); the current match (the Enter/F3 target) reuses the SELECTION background,
        // opaque, so it reads like a selection preview. Glyphs draw on top next (fg handled in
        // build_window_cosmic), and once Enter lands, cosmic's own selection band covers the current
        // match identically.
        let (fbr, fbg, fbb) = self.editor.find_bg_color();
        let (cbr, cbg, cbb) = self.editor.sel_bg_color();
        for &(x0, y0, x1, y1, current) in &find_rects {
            let ((hr, hg, hb), a) =
                if current { ((cbr, cbg, cbb), 1.0) } else { ((fbr, fbg, fbb), 0.55) };
            fill_blend(&mut canvas, w, h, x0, y0, x1, y1, hr, hg, hb, a);
        }
        // BALPAIRS matched-bracket highlight — behind the glyphs, using BALPAIRSCOLOR or (unset) the
        // selection background, so it reads like the standard selection tint.
        if !bal_rects.is_empty() {
            let (pr, pg, pb) = self.editor.bal_pairs_color().unwrap_or_else(|| self.editor.sel_bg_color());
            for &(x0, y0, x1, y1) in &bal_rects {
                fill_blend(&mut canvas, w, h, x0, y0, x1, y1, pr, pg, pb, 0.9);
            }
        }
        let (selr, selg, selb) = self.editor.sel_bg_color(); // `SELBGCOLOR` — selection band
        let (sfr, sfg, sfb) = self.editor.sel_fg_color(); // `SELFGCOLOR` — selected-text colour
        let text_c = CColor::rgb(fr, fg_, fb);
        let sel_c = CColor::rgb(selr, selg, selb);
        let seltxt_c = CColor::rgb(sfr, sfg, sfb);
        if self.wi_rows.is_empty() {
            // Default path: cosmic draws glyphs + selection band at the margin (unchanged behaviour).
            let mut ed = Editor::new(&mut buffer);
            ed.set_cursor(cosmic_cursor);
            ed.set_selection(selection);
            ed.draw(
                &mut self.font_system,
                &mut self.swash,
                text_c,
                CColor::rgba(0, 0, 0, 0), // caret painted manually below
                sel_c,
                seltxt_c,
                // `hshift` (0 unless `WRAP off`) slides glyphs + the selection band left into the
                // horizontal window; `blend_cell` clips anything off either edge.
                |x, y, cw, ch, color| blend_cell(&mut canvas, w, h, x - hshift, y, cw, ch, color),
            );
        } else {
            // WRAPINDENT path: draw each visual row shifted right by its hanging-indent offset.
            self.draw_indented(&mut buffer, &mut canvas, w, h, cosmic_cursor, selection, text_c, sel_c, seltxt_c);
        }

        // Paint the caret at the real cursor — works on every line, whether the
        // selection is coupled, decoupled-roam, or absent. Colour + shape are configurable
        // (`CRCOLOR`/`CRSHAPE`): bar = thin vertical line, block = semi-transparent full cell
        // (so the glyph shows through), underline = horizontal bar at the cell's bottom.
        // Hidden while the command line holds focus — only ONE caret shows at a time (the cmline
        // caret is likewise painted only when focused), so the active input is unambiguous.
        if !self.editor.cmline_open() {
            let (cr, cg, cb) = self.editor.caret_color();
            // Pixel-space metrics (the canvas is at device resolution); thicknesses scale so the
            // bar/underline keep the same visual weight at any zoom/HiDPI.
            let cell = (self.font_size * scale * 0.6).max(1.0) as i32; // monospace cell width
            let lh = lh_px as i32;
            let bar = (2.0 * scale).round().max(1.0) as i32; // caret bar width / underline height
            let shape = self.editor.caret_shape();
            // Paint one caret rect at (cx, cy). Shared by the primary caret and every secondary
            // (multi-caret): each caret draws identically, so the whole set is visible at once.
            let mut paint_caret = |cx: i32, cy: i32| {
                let (x0, x1, y0, y1, alpha) = match shape {
                    CaretShape::Bar => (cx, cx + bar, cy, cy + lh, 255u16),
                    CaretShape::Block => (cx, cx + cell, cy, cy + lh, 115u16), // see-through over glyph
                    CaretShape::Underline => (cx, cx + cell, cy + lh - bar, cy + lh, 255u16),
                };
                for py in y0..y1 {
                    for px in x0..x1 {
                        if px < 0 || py < 0 || px as usize >= w || py as usize >= h {
                            continue;
                        }
                        let i = (py as usize * w + px as usize) * 4;
                        if alpha >= 255 {
                            canvas[i] = cr;
                            canvas[i + 1] = cg;
                            canvas[i + 2] = cb;
                        } else {
                            let a = alpha as f32 / 255.0;
                            for (c, s) in [(0usize, cr as f32), (1, cg as f32), (2, cb as f32)] {
                                let d = canvas[i + c] as f32;
                                canvas[i + c] = (s * a + d * (1.0 - a)).round() as u8;
                            }
                        }
                    }
                }
            };
            if let Some((cx, cy)) = caret_px {
                paint_caret(cx as i32, cy as i32);
            }
            for &(cx, cy) in &sec_px {
                paint_caret(cx as i32, cy as i32);
            }
        }

        // Live language-check underlines: a FAINT, thin line just under each flagged span, painted ON
        // TOP of the glyphs. Spelling → SPELLCOLOR, grammar/other → GRAMMARCOLOR; the SHAPE is per
        // category (`SPELLSTYLE`/`GRAMMARSTYLE`): dotted (default) / solid / squiggle.
        if !lint_rects.is_empty() {
            use xyscript_app::host::UnderlineStyle::{Dotted, Solid, Squiggle};
            let (spr, spg, spb) = self.editor.spell_color();
            let (grr, grg, grb) = self.editor.grammar_color();
            let (spell_style, grammar_style) = (self.editor.spell_style(), self.editor.grammar_style());
            let thin = scale.round().max(1.0) as i32; // ~1 logical px — thin line height
            let lift = (lh_px * 0.12).round().max(1.0) as i32; // sit close under the glyphs, not the cell floor
            let dot = scale.round().max(1.0) as i32; // dot length (thin)
            let period = (scale * 3.0).round().max(3.0) as i32; // dot + gap (roughly 1-on / 2-off)
            let amp = (scale * 1.8).round().max(1.0) as i32; // squiggle amplitude (± px around the baseline)
            let wl = (scale * 6.0).round().max(4.0) as i32; // squiggle wavelength (px per full up-down) — looser
            let alpha = 0.55; // faint
            for &(x0, y0, x1, y1, is_spelling) in &lint_rects {
                let (r, g, b) = if is_spelling { (spr, spg, spb) } else { (grr, grg, grb) };
                let style = if is_spelling { spell_style } else { grammar_style };
                // Heavy variants: taller line, chunkier dots, thicker squiggle stroke.
                let heavy = style.is_thick();
                let thick = if heavy { thin * 2 } else { thin };
                let dot = if heavy { dot * 2 } else { dot }; // longer dashes so gaps stay legible
                let period = if heavy { period + dot } else { period };
                let base = (y1 - lift).max(y0);
                match style.shape() {
                    Solid => {
                        fill_blend(&mut canvas, w, h, x0, base, x1, base + thick, r, g, b, alpha);
                    }
                    Dotted => {
                        let mut x = x0;
                        while x < x1 {
                            fill_blend(&mut canvas, w, h, x, base, (x + dot).min(x1), base + thick, r, g, b, alpha);
                            x += period;
                        }
                    }
                    Squiggle => {
                        // A triangle wave: y = base + amp·(1 − 4·|t−0.5|), t ∈ [0,1) over one wavelength.
                        let mut x = x0;
                        while x < x1 {
                            let t = (x - x0).rem_euclid(wl) as f32 / wl as f32;
                            let off = (amp as f32 * (1.0 - 4.0 * (t - 0.5).abs())).round() as i32;
                            let yy = base + off;
                            fill_blend(&mut canvas, w, h, x, yy, x + thick, yy + thick, r, g, b, alpha);
                            x += thin.max(1); // step by the thin stride so the wave stays continuous
                        }
                    }
                    _ => unreachable!("UnderlineStyle::shape() only ever yields a thin variant"),
                }
            }
        }

        // `MDA` underlines: a solid line in the TEXT colour just under each underlined span, opaque
        // (a real typographic underline, unlike the faint lint marks). Painted on top of the glyphs.
        if !mda_underline_rects.is_empty() {
            let (ur, ug, ub) = self.editor.fg_color();
            let thick = (scale.round()).max(1.0) as i32; // ~1 logical px
            let lift = (lh_px * 0.10).round().max(1.0) as i32; // sit just below the glyph baseline
            for &(x0, _y0, x1, y1) in &mda_underline_rects {
                let base = y1 - lift;
                fill_blend(&mut canvas, w, h, x0, base, x1, base + thick, ur, ug, ub, 1.0);
            }
        }

        // Wrap ratio for the visual scrollbar: visual rows ÷ distinct logical lines over the
        // laid-out window. A cheap O(viewport) sample of how heavily text is wrapping right now;
        // the scrollbar extrapolates it to the whole document (no full-document shaping).
        {
            let mut rows = 0usize;
            let mut logical = 0usize;
            let mut last: Option<usize> = None;
            for run in buffer.layout_runs() {
                rows += 1;
                if last != Some(run.line_i) {
                    last = Some(run.line_i);
                    logical += 1;
                }
            }
            if logical > 0 {
                self.wrap_ratio = (rows as f32 / logical as f32).max(1.0);
            }
        }

        // Line-number gutter (`$LNS`): record each logical line's number + the canvas-y of
        // its FIRST visual run (so wrapped continuations don't get their own number). The
        // gutter is painted by the ui pass to the left of this (reduced-width) canvas.
        self.gutter_lines.clear();
        if self.editor.line_numbers() {
            let mut last: Option<usize> = None;
            for run in buffer.layout_runs() {
                if last != Some(run.line_i) {
                    last = Some(run.line_i);
                    self.gutter_lines.push((win.start_line + run.line_i + 1, run.line_top));
                }
            }
        }

        // Bookmark gutter glyphs (`MARKSET`): the canvas-y of the first visual row of each visible
        // logical line that holds a bookmark. Collected independent of `$LNS` so bookmarks show even
        // with line numbers off (a dedicated strip is reserved in the layout when the doc has marks).
        self.mark_rows.clear();
        if self.editor.mark_gutter() {
            let mark_lines: std::collections::HashSet<usize> =
                self.editor.mark_lines().into_iter().collect();
            if !mark_lines.is_empty() {
                let mut last: Option<usize> = None;
                for run in buffer.layout_runs() {
                    if last != Some(run.line_i) {
                        last = Some(run.line_i);
                        if mark_lines.contains(&(win.start_line + run.line_i)) {
                            self.mark_rows.push(run.line_top);
                        }
                    }
                }
            }
        }

        // Visible returns (`$VR`): a dim pilcrow ¶ at each HARD newline in the window. A
        // logical line `i` is followed by a real `\n` iff it isn't the buffer's last line;
        // the ¶ goes at the end of that line's LAST visual run (so it sits past soft wraps).
        if self.editor.visible_returns() {
            let line_count = buffer.lines.len();
            let runs: Vec<(usize, f32, f32)> =
                buffer.layout_runs().map(|r| (r.line_i, r.line_top, r.line_w)).collect();
            let marks: Vec<(i32, i32)> = runs
                .iter()
                .enumerate()
                .filter(|(k, &(li, ..))| {
                    let last_of_line = runs.get(k + 1).map_or(true, |&(nli, ..)| nli != li);
                    last_of_line && li + 1 < line_count
                })
                .map(|(_, &(_, top, wd))| (wd as i32 - hshift, top as i32))
                .collect();
            if !marks.is_empty() {
                const PIL: CColor = CColor::rgb(120, 150, 210);
                let fam = self.editor_family();
                let (e_weight, e_style) = editor_cosmic_style(self.editor.editor_font_style());
                let attrs = match &fam {
                    Some(n) => Attrs::new().family(Family::Name(n)).color(PIL),
                    None => Attrs::new().family(Family::Monospace).color(PIL),
                }
                .weight(e_weight)
                .style(e_style);
                let mut pbuf = Buffer::new(
                    &mut self.font_system,
                    Metrics::new(self.font_size * scale, self.line_height * scale),
                );
                pbuf.set_text("¶", &attrs, Shaping::Advanced, None);
                for (mx, my) in marks {
                    pbuf.draw(&mut self.font_system, &mut self.swash, PIL, |x, y, cw, ch, color| {
                        let sa = color.a() as f32 / 255.0;
                        if sa <= 0.0 {
                            return;
                        }
                        let (sr, sg, sb) = (color.r() as f32, color.g() as f32, color.b() as f32);
                        for py in (y + my)..(y + my + ch as i32) {
                            for px in (x + mx)..(x + mx + cw as i32) {
                                if px < 0 || py < 0 || px as usize >= w || py as usize >= h {
                                    continue;
                                }
                                let i = (py as usize * w + px as usize) * 4;
                                for (c, s) in [(0usize, sr), (1, sg), (2, sb)] {
                                    let d = canvas[i + c] as f32;
                                    canvas[i + c] = (s * sa + d * (1.0 - sa)).round() as u8;
                                }
                            }
                        }
                    });
                }
            }
        }

        // Visible whitespace (`$WS`): a small dot at each space, a right-arrow at each tab.
        // Direct pixel marks (no glyph rasterization) since spaces are dense; whitespace
        // cells carry no text glyph, so overwriting their pixels is safe.
        if self.editor.visible_whitespace() {
            const WS: [u8; 3] = [150, 170, 200];
            let put = |canvas: &mut [u8], px: i32, py: i32| {
                if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                    let i = (py as usize * w + px as usize) * 4;
                    canvas[i] = WS[0];
                    canvas[i + 1] = WS[1];
                    canvas[i + 2] = WS[2];
                }
            };
            for run in buffer.layout_runs() {
                let cy = (run.line_top + run.line_height * 0.5) as i32;
                for g in run.glyphs {
                    match run.text.get(g.start..g.end) {
                        Some(" ") => {
                            // 2×2 dot centered in the cell (shifted into the horizontal window).
                            let cx = (g.x + g.w * 0.5) as i32 - hshift;
                            for dx in -1..=0 {
                                for dy in -1..=0 {
                                    put(&mut canvas, cx + dx, cy + dy);
                                }
                            }
                        }
                        Some("\t") => {
                            // A shaft across the tab cell with a `>` chevron at the right.
                            let x0 = (g.x + 2.0) as i32 - hshift;
                            let x1 = (g.x + g.w - 2.0) as i32 - hshift;
                            for px in x0..x1 {
                                put(&mut canvas, px, cy);
                            }
                            for k in 1..=3 {
                                put(&mut canvas, x1 - k, cy - k);
                                put(&mut canvas, x1 - k, cy + k);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        // Return the shaped buffer to the persistent cache so its per-line layouts survive to next
        // frame (see `win_buffer`). `win_sigs` already parallels `buffer.lines` (shaping didn't change
        // the line set), so the pair stays consistent.
        self.win_buffer = Some(buffer);
        // Publish the caret's visual-line span (shaped above) for `RL`, stamped with the current
        // revision (no edit happens during a redraw, so it still matches what it was computed against).
        let rev = self.editor.doc_revision();
        self.editor.host_mut().set_visual_line_span(vline_span, rev);
        // Console scroll-lock bookkeeping: remember whether the tail is visible now (the last logical
        // line sits within the window), so the NEXT dirty frame only auto-follows when we were at the
        // bottom. A small overshoot from wrapping just errs toward following, which is the safe side.
        if self.editor.console_is_active() {
            let total = self.editor.doc(self.editor.active_index()).line_count();
            self.console_at_bottom = self.pane_view.scroll_line + visible_rows >= total.saturating_sub(1);
        }
        canvas
    }

    /// `w`/`h` are the LOGICAL (point) canvas size. We raster at PHYSICAL pixels (× render_scale)
    /// and the caller displays the texture at the logical size, so it maps 1:1 to device pixels.
    fn render_canvas(&mut self, w: usize, h: usize) -> egui::ColorImage {
        let s = self.render_scale;
        let wp = ((w as f32 * s).round() as usize).max(1);
        let hp = ((h as f32 * s).round() as usize).max(1);
        self.pane_view.canvas_px = (wp, hp); // PIXELS (the cosmic/raster layer's coordinate space)
        self.lint_focus_doc = Some(self.editor.active_index()); // single pane == focused
        let rgba = self.raster(wp, hp);
        // One visible pane: the live checker targets the active doc's window (set by `raster`).
        self.lint_panes.clear();
        if let Some((lo, hi)) = self.lint_visible {
            self.lint_panes.push((self.editor.active_index(), lo, hi));
        }
        egui::ColorImage::from_rgba_premultiplied([wp, hp], &rgba)
    }

    /// Read-only **hex + text** ("hex dump") canvas — the `HEX on` view. Renders the active document's
    /// raw bytes as a fixed 16-byte grid via [`xyscript_app::hex`] (offset gutter, hex pairs grouped
    /// 8 + 8, text column through the active encoding), overlaying the caret (a byte offset) and any
    /// selection with the module's cell geometry. `w`/`h` are POINTS like [`render_canvas`]; this is
    /// the single-pane path only (a split renders normally). Scroll is reused as the top ROW index.
    fn render_hex_canvas(&mut self, w: usize, h: usize) -> egui::ColorImage {
        use xyscript_app::hex;
        let scale = self.render_scale;
        let wp = ((w as f32 * scale).round() as usize).max(1);
        let hp = ((h as f32 * scale).round() as usize).max(1);
        self.pane_view.canvas_px = (wp, hp);
        // No live language-check overlay in hex view.
        self.lint_panes.clear();
        self.lint_visible = None;

        let lh_px = self.line_height * scale;
        let visible_rows = ((hp as f32 / lh_px).floor() as usize).max(1);

        // Fold any accumulated pixel scroll (mouse wheel, `MU`/`MD`) into whole grid ROWS — the hex
        // view scrolls one row at a time, so it reads `scroll_line` only and drains the pixel remainder.
        if self.pane_view.scroll_vertical.abs() >= lh_px {
            let delta_rows = (self.pane_view.scroll_vertical / lh_px).trunc();
            self.pane_view.scroll_line =
                (self.pane_view.scroll_line as f32 + delta_rows).max(0.0) as usize;
            self.pane_view.scroll_vertical -= delta_rows * lh_px;
        }

        // Pull the document's byte state up front, then drop the borrow before touching `font_system`.
        let (total, enc, caret, sel) = {
            let buf = self.editor.doc(self.editor.active_index());
            let total = buf.len();
            (total, buf.encoding(), buf.cursor().min(total), buf.selection_span())
        };
        // One trailing (possibly empty) row always exists so the append caret at end-of-file has a home.
        let total_rows = total / hex::BYTES_PER_ROW + 1;
        let caret_row = caret / hex::BYTES_PER_ROW;

        // Follow: bring the caret's row on screen (one-shot, mirrors the text raster).
        if self.pane_view.follow {
            let top = self.pane_view.scroll_line;
            if caret_row < top {
                self.pane_view.scroll_line = caret_row;
            } else if caret_row >= top + visible_rows {
                self.pane_view.scroll_line = caret_row + 1 - visible_rows;
            }
            self.pane_view.follow = false;
        }
        // Never scroll past the last row.
        let max_top = total_rows.saturating_sub(1);
        if self.pane_view.scroll_line > max_top {
            self.pane_view.scroll_line = max_top;
        }
        let top_row = self.pane_view.scroll_line;

        // Colours + font, captured as owned values (no `self` borrow held while cosmic draws).
        let (br, bgc, bb) = self.editor.bg_color();
        let (fr, fgc, fbc) = self.editor.fg_color();
        let (selr, selg, selb) = self.editor.sel_bg_color();
        let fam = self.editor_family();
        let (e_weight, e_style) = editor_cosmic_style(self.editor.editor_font_style());
        let cmline_open = self.editor.cmline_open();
        let (crr, crg, crb) = self.editor.caret_color();
        let caret_shape = self.editor.caret_shape();
        // Hex-editing focus: which column the caret sits in, and (in the hex column) which nibble —
        // the low nibble is active only while a high nibble was just entered AT this caret byte.
        let hex_text_col = self.hex_text_col;
        let hex_nibble_lo = self.hex_nibble_at == Some(caret);

        // Compose the visible rows into one monospace block; count how many actually rendered.
        let mut text = String::new();
        let mut row_count = 0usize;
        {
            let buf = self.editor.doc(self.editor.active_index());
            for r in 0..visible_rows {
                let base = (top_row + r) * hex::BYTES_PER_ROW;
                if base > total {
                    break;
                }
                let end = (base + hex::BYTES_PER_ROW).min(total);
                let bytes = buf.slice_bytes(base, end);
                if r > 0 {
                    text.push('\n');
                }
                text.push_str(&hex::format_row(&bytes, base, &enc));
                row_count += 1;
            }
        }

        // Canvas: opaque `BGCOLOR` fill.
        let mut canvas = vec![0u8; wp * hp * 4];
        for px in canvas.chunks_exact_mut(4) {
            px.copy_from_slice(&[br, bgc, bb, 255]);
        }

        // Cosmic buffer: shape once, measure the true monospace advance, then draw.
        let text_c = CColor::rgb(fr, fgc, fbc);
        let attrs = match &fam {
            Some(n) => Attrs::new().family(Family::Name(n)),
            None => Attrs::new().family(Family::Monospace),
        }
        .weight(e_weight)
        .style(e_style)
        .color(text_c);
        let mut cbuf = Buffer::new(&mut self.font_system, Metrics::new(self.font_size * scale, lh_px));
        cbuf.set_wrap(Wrap::None);
        cbuf.set_size(Some(wp as f32), Some(hp as f32));
        cbuf.set_text(&text, &attrs, Shaping::Advanced, None);
        cbuf.shape_until_scroll(&mut self.font_system, false);
        // Exact cell width from the offset gutter's consecutive monospace glyphs; fall back to the
        // 0.6·em approximation the editor caret uses if a row somehow lays out fewer than two glyphs.
        let mut cell_w = self.font_size * scale * 0.6;
        for run in cbuf.layout_runs() {
            if run.glyphs.len() >= 2 {
                let dx = (run.glyphs[1].x - run.glyphs[0].x).abs();
                if dx > 0.0 {
                    cell_w = dx;
                }
                break;
            }
        }
        self.hex_cell_w = cell_w; // remembered for the mouse hit-test (see `hex_hit`)

        // Selection band: each selected byte's hex pair + text glyph, painted BEHIND the glyphs.
        if let Some((a, b)) = sel {
            let (a, b) = (a.min(total), b.min(total));
            let cellw_i = cell_w as i32;
            let lh_i = lh_px as i32;
            for off in a..b {
                let row = off / hex::BYTES_PER_ROW;
                if row < top_row || row >= top_row + row_count {
                    continue;
                }
                let ry = ((row - top_row) as f32 * lh_px) as i32;
                let i = off % hex::BYTES_PER_ROW;
                let hx = (hex::hex_digit_col(i) as f32 * cell_w) as i32;
                fill_blend(&mut canvas, wp, hp, hx, ry, hx + 2 * cellw_i, ry + lh_i, selr, selg, selb, 1.0);
                let tx = (hex::text_col(i) as f32 * cell_w) as i32;
                fill_blend(&mut canvas, wp, hp, tx, ry, tx + cellw_i, ry + lh_i, selr, selg, selb, 1.0);
            }
        }

        // Glyphs.
        cbuf.draw(&mut self.font_system, &mut self.swash, text_c, |x, y, cw, ch, color| {
            blend_cell(&mut canvas, wp, hp, x, y, cw, ch, color);
        });

        // Caret at the byte offset (hidden while the command line is focused, like the text caret).
        // In hex view the caret sits on ONE cell — the active hex nibble (high, or low mid-entry) or
        // the text glyph — in the FOCUSED column, with a faint companion marking the same byte in the
        // other column so you can orient across the two.
        if !cmline_open && caret_row >= top_row && caret_row < top_row + row_count {
            let ry = ((caret_row - top_row) as f32 * lh_px) as i32;
            let i = caret % hex::BYTES_PER_ROW;
            let cellw_i = cell_w as i32;
            let lh_i = lh_px as i32;
            let bar = (2.0 * scale).round().max(1.0) as i32;
            let col_x = |col: usize| (col as f32 * cell_w) as i32;
            let mut paint = |x0: i32, x1: i32, y0: i32, y1: i32, alpha: u16| {
                for py in y0..y1 {
                    for px in x0..x1 {
                        if px < 0 || py < 0 || px as usize >= wp || py as usize >= hp {
                            continue;
                        }
                        let idx = (py as usize * wp + px as usize) * 4;
                        if alpha >= 255 {
                            canvas[idx] = crr;
                            canvas[idx + 1] = crg;
                            canvas[idx + 2] = crb;
                        } else {
                            let al = alpha as f32 / 255.0;
                            for (c, s) in [(0usize, crr as f32), (1, crg as f32), (2, crb as f32)] {
                                let d = canvas[idx + c] as f32;
                                canvas[idx + c] = (s * al + d * (1.0 - al)).round() as u8;
                            }
                        }
                    }
                }
            };
            // Focused caret cell (one cell): the active nibble in the hex column, or the text glyph.
            let focus_col = if hex_text_col {
                hex::text_col(i)
            } else {
                hex::hex_digit_col(i) + usize::from(hex_nibble_lo)
            };
            let fx = col_x(focus_col);
            let (x0, x1, y0, y1, alpha) = match caret_shape {
                CaretShape::Bar => (fx, fx + bar, ry, ry + lh_i, 255u16),
                CaretShape::Block => (fx, fx + cellw_i, ry, ry + lh_i, 130u16),
                CaretShape::Underline => (fx, fx + cellw_i, ry + lh_i - bar, ry + lh_i, 255u16),
            };
            paint(x0, x1, y0, y1, alpha);
            // Faint companion in the OTHER column (the whole hex pair, or the text glyph).
            let (comp_start, comp_cells) = if hex_text_col {
                (hex::hex_digit_col(i), 2)
            } else {
                (hex::text_col(i), 1)
            };
            let cxs = col_x(comp_start);
            paint(cxs, cxs + comp_cells * cellw_i, ry, ry + lh_i, 55);
        }

        egui::ColorImage::from_rgba_premultiplied([wp, hp], &canvas)
    }

    /// Composite renderer for a SPLIT layout: raster each pane into its sub-rect of one big canvas via
    /// swap-state (point `active`/`pane_view` at each pane, render, restore), then paint the dividers
    /// and a border around the focused pane. `w`/`h` are POINTS, like [`render_canvas`]. Single-pane
    /// rendering still goes through `render_canvas` (this path is only taken when `pane_count > 1`).
    fn render_canvas_panes(&mut self, w: usize, h: usize) -> egui::ColorImage {
        let s = self.render_scale;
        let wp = ((w as f32 * s).round() as usize).max(1);
        let hp = ((h as f32 * s).round() as usize).max(1);
        // Editor background fills every gap (divider gutters + rounding slack) so nothing shows through.
        let (br, bgc, bb) = self.editor.bg_color();
        let mut big = vec![0u8; wp * hp * 4];
        for px in big.chunks_exact_mut(4) {
            px[0] = br;
            px[1] = bgc;
            px[2] = bb;
            px[3] = 255;
        }
        let focused = self.editor.host().focused_pane_id();
        let saved_active = self.editor.active_index();
        self.lint_focus_doc = Some(saved_active); // only the focused pane hides the word-being-typed
        let live_focused = self.pane_view.clone(); // the focused pane's authoritative viewport
        let layout = self.editor.host().pane_layout(w as f32, h as f32);
        // Prune saved views/metrics for panes that have closed.
        let present: std::collections::HashSet<_> = layout.iter().map(|(p, _, _)| *p).collect();
        self.pane_views.retain(|k, _| present.contains(k));
        self.pane_wrap.retain(|k, _| present.contains(k));
        // Reserve a scrollbar strip on the right of each pane, so text stops before its scrollbar
        // (as in single-pane). The strip stays editor-bg (already filled); the scrollbar is drawn as
        // an egui overlay after the image.
        let sbw_px = (PANE_SB_W * s).round() as usize;
        // Honor the `OF`/`OFL`/`OFR` offset (global, in char columns) inside each pane, matching the
        // single-pane layout `[margin_l][text][margin_r][scrollbar]`. The margins stay editor-bg (the
        // big buffer is already bg-filled), so the text just sits inset.
        let cell_w = self.font_size * 0.6;
        let ml_px = ((self.editor.margin_left() * cell_w).round() * s).round() as usize;
        let mr_px = ((self.editor.margin_right() * cell_w).round() * s).round() as usize;
        // Render NON-focused panes first, the focused pane LAST — so the per-frame globals `raster`
        // writes (gutter_lines, wrap_ratio, lint_visible, self.win) end up describing the focused
        // pane, which the overlays + hit-testing downstream read.
        let mut order = layout.clone();
        order.sort_by_key(|(p, _, _)| *p == focused); // false (0) before true (1) → focused last
        self.lint_panes.clear(); // rebuilt below: (doc, window) for EACH pane, so all get checked
        for (pid, didx, r) in order {
            let (rx, ry) = ((r.x * s).round() as usize, (r.y * s).round() as usize);
            let full_rw = ((r.w * s).round() as usize).max(1);
            // Text area = pane width minus the left/right offsets and the scrollbar strip.
            let rw = full_rw.saturating_sub(ml_px + mr_px + sbw_px).max(1);
            let rh = ((r.h * s).round() as usize).max(1);
            self.editor.host_mut().set_active_raw(didx);
            self.pane_view = if pid == focused {
                live_focused.clone()
            } else {
                self.pane_views.get(&pid).cloned().unwrap_or_default()
            };
            self.pane_view.canvas_px = (rw, rh);
            let sub = self.raster(rw, rh);
            // Record this pane's document + visible window so the live checker refreshes it too
            // (`raster` set `lint_visible` for the doc that's active right now — `didx`).
            if let Some((lo, hi)) = self.lint_visible {
                self.lint_panes.push((didx, lo, hi));
            }
            blit_rgba(&mut big, wp, hp, &sub, rw, rh, rx + ml_px, ry); // offset right by OFL
            self.pane_wrap.insert(pid, self.wrap_ratio); // this pane's wrap ratio → its scrollbar thumb
            if pid != focused {
                self.pane_views.insert(pid, self.pane_view.clone());
            }
        }
        // The focused pane rendered last, so `self.pane_view` now holds its scroll-clamped view; keep
        // it (it's the live viewport the rest of the frame reads). Restore `active`.
        let focused_view_out = self.pane_view.clone();
        self.editor.host_mut().set_active_raw(saved_active);
        self.pane_view = focused_view_out;
        // Dividers (separator colour) + a border around the focused pane (selection colour).
        let sep = self.editor.sep_color();
        for d in self.editor.host().pane_dividers(w as f32, h as f32) {
            fill_rect_px(&mut big, wp, hp, d.bar, s, sep);
        }
        if let Some((_, _, fr)) = layout.iter().find(|(p, _, _)| *p == focused) {
            draw_border_px(&mut big, wp, hp, *fr, s, self.editor.pane_border_color(), 2.0 * s);
        }
        egui::ColorImage::from_rgba_premultiplied([wp, hp], &big)
    }

    /// Page the view up/down by a screenful of **visual** (wrapped) rows, keeping the
    /// caret on the same screen row as the text pages underneath it. Reckoning in
    /// visual rows (not logical lines) is what keeps paging sane on wrapped files — a
    /// logical-line page leaps an unpredictable distance when lines wrap. `extend`
    /// grows the selection (Shift+PageUp/Down).
    fn page_move(&mut self, down: bool, extend: bool) {
        let (w, h) = self.pane_view.canvas_px; // PIXELS
        if w == 0 || h == 0 {
            return;
        }
        let lh_px = self.line_height * self.render_scale; // cosmic geometry is in pixels
        let visible = ((h as f32 / lh_px).floor() as usize).max(1);
        // Two fewer than a screenful so the last couple of rows carry over for continuity.
        let page = visible.saturating_sub(2).max(1) as f32;

        let (mut buffer, win) = self.windowed_buffer(w, h);
        let doc_cursor = self.editor.doc(self.editor.active_index()).cursor();
        // The caret's current screen position, to keep it on the same row while paging.
        // It may be off-window (None) — e.g. after a wheel scroll — but we still page the
        // view; we never bail out here, or holding the key would wedge.
        let caret = self.win_cursor_at(&win, doc_cursor).and_then(|c| buffer.cursor_position(&c));
        // Screen-column goal (content x + the row's hanging indent), converted back to content x at
        // each hit below so paging holds the caret's visual column across differently-indented rows.
        let goal = self.goal_x.or(caret.map(|(cx, cy)| cx + self.wi_offset_at_y(cy))).unwrap_or(0.0);

        // Scroll the VIEW by a page of visual rows (pixel offset; cosmic folds it into
        // line + residual on the next shape, clamped to the window ends). The window's
        // margin (≥ a screen) guarantees a single page lands inside it.
        let before = (self.pane_view.scroll_line, self.pane_view.scroll_vertical);
        self.pane_view.scroll_vertical += if down { page } else { -page } * lh_px;
        let rel = self.pane_view.scroll_line.saturating_sub(win.start_line);
        buffer.set_scroll(Scroll::new(rel, self.pane_view.scroll_vertical, 0.0));
        buffer.shape_until_scroll(&mut self.font_system, false);
        let s = buffer.scroll();
        self.pane_view.scroll_line = win.start_line + s.line;
        self.pane_view.scroll_vertical = s.vertical;
        // The page scroll reshaped the buffer, which un-narrows the WRAPINDENT lines — re-narrow so the
        // hit below (and the row offsets it uses) match the layout that will be drawn at this scroll.
        self.refresh_wrap_indent(&mut buffer, &win, w as f32);

        // Drop the caret back onto the same screen row. At an edge we couldn't scroll past,
        // aim at the extreme so PgUp/PgDn still reach the first/last line; if the caret was
        // off-screen, aim at the leading edge. The top-row fallback guarantees a hit (the
        // top line is always laid out), so a failed hit-test near EOF can't strand the
        // caret off-screen and wedge subsequent pages.
        let stuck = (self.pane_view.scroll_line, self.pane_view.scroll_vertical) == before;
        let aim_y = match (stuck, down) {
            (true, true) => h as f32,
            (true, false) => 0.0,
            (false, _) => caret
                .map(|(_, cy)| cy + 0.5 * lh_px)
                .unwrap_or(if down { 0.5 * lh_px } else { h as f32 - 0.5 * lh_px }),
        };
        let aim = aim_y.clamp(0.0, h as f32);
        let target = buffer
            .hit(goal - self.wi_offset_at_y(aim), aim)
            .or_else(|| buffer.hit(goal - self.wi_offset_at_y(0.5 * lh_px), 0.5 * lh_px))
            .map(|cur| self.win_rope_at(&win, cur));
        if let Some(off) = target {
            self.editor.host_mut().move_active_to(off, extend);
        }
        self.goal_x = Some(goal); // hold the column for the next page / arrow
        self.pane_view.follow = false; // scroll + caret already positioned; don't re-clamp
        self.win_buffer = Some(buffer); // return the shaped buffer to the cache (see `win_buffer`)
    }

    /// Pull an external OS-clipboard change into the editor's `$CB`, so a paste sees
    /// text copied from another app. Called before processing input.
    fn sync_clipboard_in(&mut self) {
        let os = match self.os_clipboard.as_mut() {
            Some(cb) => cb.get_text().ok(),
            None => None,
        };
        if let Some(os) = os {
            if os != self.os_clip_mirror && os != self.editor.clipboard() {
                self.editor.set_clipboard(os.clone());
                self.os_clip_mirror = os;
            }
        }
    }

    /// Push an app-side `$CB` change (copy/cut/CLIP/…) out to the OS clipboard.
    fn sync_clipboard_out(&mut self) {
        let cur = self.editor.clipboard().to_string();
        if cur != self.os_clip_mirror {
            if let Some(cb) = self.os_clipboard.as_mut() {
                let _ = cb.set_text(cur.clone());
            }
            self.os_clip_mirror = cur;
        }
    }

    /// If `xpl` is a bare vertical-motion verb — `LU`/`UP`/`LD`/`DOWN`, optionally `… SEL` to extend
    /// — and we're in normal text editing (not the command line, a modal dialog, or a directory
    /// listing), move by VISUAL row (wrap-aware, exactly like the arrow keys) and return `true`.
    /// Otherwise return `false` so the caller runs the verb as written (the logical by-line move,
    /// which is what the cmline / a listing / a script wants). This is what makes a key bound to
    /// `LD` behave like the Down arrow instead of jumping over a wrapped line's continuation rows.
    /// Whether the active document is in the hex-dump view (`HEX on`). The runtime reads this to keep
    /// the vertical-nav keys CAPTURED in hex view (so they run the host's byte-row motion) instead of
    /// handing them to egui's wrap-aware text scrolling, which is meaningless over a hex grid.
    fn hex_view(&self) -> bool {
        self.editor.hex_view()
    }

    /// Toggle the hex-view edit column (hex pairs ↔ text). Driven from the runtime's Tab intercept —
    /// Tab can't go through egui here (egui hijacks it for focus navigation) or the KBD (a bound TAB
    /// verb would insert a tab), so the runtime consumes it and calls this directly.
    fn hex_toggle_column(&mut self) {
        self.hex_text_col = !self.hex_text_col;
        self.hex_nibble_at = None;
    }

    /// Encode a typed char to a single byte for text-column entry: ASCII maps directly; under a
    /// single-byte view (CP437/Latin-1/…) reverse-look-up the byte whose glyph is `ch`; under UTF-8 a
    /// non-ASCII char is multi-byte, so it's skipped (`None`) — basic byte entry, not text insertion.
    fn hex_encode_char(&self, ch: char) -> Option<u8> {
        use xyscript_app::Encoding; // `decode`/`is_single_byte` live on the trait
        if ch.is_ascii() {
            return Some(ch as u8);
        }
        let enc = self.editor.doc(self.editor.active_index()).encoding();
        if enc.is_single_byte() {
            (0u8..=255).find(|&b| {
                let s = enc.decode(&[b]);
                let mut it = s.chars();
                it.next() == Some(ch) && it.next().is_none()
            })
        } else {
            None
        }
    }

    /// Advance the hex caret one byte (clamped), after a completed byte entry.
    fn hex_advance(&mut self) {
        let next = (self.editor.hex_caret() + 1).min(self.editor.hex_len());
        self.editor.hex_set_caret(next);
    }

    /// Delete in the hex view: the selected byte range if there is one, else a single byte — the one
    /// AT the caret (`before = false`, Delete) or just before it (`before = true`, Backspace).
    fn hex_delete_key(&mut self, before: bool) {
        if self.editor.hex_selection_bytes().is_some() {
            self.editor.hex_delete_selection();
        } else {
            self.editor.hex_delete(before);
        }
        self.hex_nibble_at = None;
        self.pane_view.follow = true;
    }

    /// HEX-view editing input: handle a typed char / edit key as a byte edit. Returns `true` when the
    /// event was consumed (so the caller skips the normal text path). Copy/cut/paste and navigation
    /// go through the host verbs (Ctrl+C/X/V, arrows) — not here. Only called while the hex view is
    /// on, the doc is writable, and the cmline is closed.
    fn hex_edit_event(&mut self, ev: &egui::Event) -> bool {
        match ev {
            egui::Event::Text(s) => {
                for ch in s.chars() {
                    // Skip control chars: Tab/Enter/etc. arrive as BOTH a Key event (handled below or
                    // captured) AND a Text event ("\t"…); the Text copy must not write a byte.
                    if ch.is_control() {
                        continue;
                    }
                    let caret = self.editor.hex_caret();
                    if self.hex_text_col {
                        // Text column: overwrite the whole byte with the char's byte, then advance.
                        if let Some(b) = self.hex_encode_char(ch) {
                            self.editor.hex_overwrite(b);
                            self.hex_advance();
                        }
                    } else if let Some(v) = ch.to_digit(16) {
                        // Hex column: the first digit at a byte sets its HIGH nibble (remembering the
                        // offset); a second digit at the SAME byte sets the LOW nibble and advances.
                        // Keying on the offset means a caret move between digits starts a fresh byte.
                        if self.hex_nibble_at == Some(caret) {
                            self.editor.hex_set_nibble(false, v as u8);
                            self.hex_nibble_at = None;
                            self.hex_advance();
                        } else {
                            self.editor.hex_set_nibble(true, v as u8);
                            self.hex_nibble_at = Some(caret);
                        }
                    }
                    // Non-hex chars in the hex column are ignored (still consumed).
                }
                self.pane_view.follow = true;
                true // consume ALL typed text in hex view — never fall through to a text insert
            }
            // (Tab is intercepted in the runtime — egui would hijack it here for focus navigation.)
            // Insert / Delete / Backspace edit bytes (the runtime leaves these to egui in hex view).
            egui::Event::Key { key: egui::Key::Insert, pressed: true, modifiers, .. }
                if !modifiers.ctrl && !modifiers.alt =>
            {
                self.editor.hex_insert(vec![0x00]);
                self.hex_nibble_at = None;
                self.pane_view.follow = true;
                true
            }
            egui::Event::Key { key: egui::Key::Delete, pressed: true, modifiers, .. }
                if !modifiers.ctrl && !modifiers.alt =>
            {
                self.hex_delete_key(false);
                true
            }
            egui::Event::Key { key: egui::Key::Backspace, pressed: true, modifiers, .. }
                if !modifiers.ctrl && !modifiers.alt =>
            {
                self.hex_delete_key(true);
                true
            }
            _ => false,
        }
    }

    fn try_visual_vmotion(&mut self, xpl: &str) -> bool {
        let mut words = xpl.split_whitespace();
        let down = match words.next().map(|w| w.to_ascii_uppercase()).as_deref() {
            Some("LU") | Some("UP") => false,
            Some("LD") | Some("DOWN") => true,
            _ => return false,
        };
        // Only a bare verb, or the verb + `SEL` (extend). Anything else (args, a `;`-chain) runs as-is.
        let extend = match words.next() {
            None => false,
            Some(w) if w.eq_ignore_ascii_case("SEL") && words.next().is_none() => true,
            _ => return false,
        };
        if self.editor.cmline_open() || self.modal_owns_keyboard() || self.active_is_dir_listing() {
            return false;
        }
        // Hex view moves the caret by a whole grid ROW (±16 bytes), not by a visual text row — let the
        // verb run as an ordinary command so the host's hex byte-motion handles it.
        if self.editor.hex_view() {
            return false;
        }
        self.visual_vmove(down, extend);
        true
    }

    /// Visual (wrapped-line) vertical move: place the caret one screen row up/down, holding the goal x
    /// column across consecutive presses. `extend` grows the selection (Shift+Up/Down).
    ///
    /// The target visual row may be off the laid-out viewport (the caret was on the top/bottom edge).
    /// When that happens we NUDGE the scroll one row toward it and retry the visual hit — so a wrapped
    /// line's continuation rows are NOT skipped at the edge (the old code fell straight back to the
    /// engine's LOGICAL by-line move there, which jumped over the wrap rows / to the line end and lost
    /// the column). Only if there's genuinely no adjacent row (first/last line) do we do a logical move.
    fn visual_vmove(&mut self, down: bool, extend: bool) {
        let (w, h) = self.pane_view.canvas_px;
        if w == 0 || h == 0 {
            return;
        }
        let doc_cursor = self.editor.doc(self.editor.active_index()).cursor();
        let lh_px = self.line_height * self.render_scale;
        // Up to two attempts: hit in the current window; if the target row is off-screen (or the hit
        // clamps to the caret's own row), scroll one row toward it and try once more.
        for attempt in 0..2 {
            let (buffer, win) = self.windowed_buffer(w, h);
            // Buffer-dependent step: the caret's screen column + the candidate target offset on the
            // adjacent visual row (`None` off ⇒ off-screen / clamped-to-own-row). Computed before the
            // buffer is returned to the cache so no `windowed_buffer` result escapes unstashed.
            let step: Option<(f32, Option<usize>)> = self
                .win_cursor_at(&win, doc_cursor)
                .and_then(|c| buffer.cursor_position(&c))
                .map(|(cx, cy)| {
                    // Goal is a SCREEN column (content x + the caret row's hanging indent), holding its
                    // visual position across rows of differing indentation. WRAPINDENT off ⇒ plain `cx`.
                    let goal = self.goal_x.unwrap_or(cx + self.wi_offset_at_y(cy));
                    let target_y = cy + if down { 1.5 * lh_px } else { -0.5 * lh_px };
                    let off = if target_y >= 0.0 && target_y < h as f32 {
                        buffer
                            .hit(goal - self.wi_offset_at_y(target_y), target_y)
                            .map(|cur| self.win_rope_at(&win, cur))
                            // Accept only a move that changes rows; at the very edge `hit` can clamp
                            // back to the caret's own row — then we scroll and retry instead.
                            .filter(|&o| o != doc_cursor)
                    } else {
                        None
                    };
                    (goal, off)
                });
            self.win_buffer = Some(buffer); // return the shaped buffer to the cache
            match step {
                None => break, // caret off-window → logical fallback + `follow` reveals it
                Some((goal, Some(off))) => {
                    self.editor.host_mut().move_active_to(off, extend);
                    self.goal_x = Some(goal);
                    self.pane_view.follow = true;
                    return;
                }
                Some((goal, None)) => {
                    if attempt == 0 {
                        self.goal_x = Some(goal); // hold the column across the retry + the fallback
                        self.pane_view.scroll_vertical += if down { lh_px } else { -lh_px };
                    }
                }
            }
        }
        // No adjacent visual row (first/last line): a one-line LOGICAL move is the reliable last resort
        // (`follow` scrolls it into view); the goal column is preserved for the next press.
        self.editor.run_command(match (down, extend) {
            (false, false) => "UP",
            (false, true) => "UP SEL",
            (true, false) => "DOWN",
            (true, true) => "DOWN SEL",
        });
        self.pane_view.follow = true;
    }

    /// Visual (wrapped-line) horizontal edge for `LB`/`LE`: place the caret at the start
    /// (`end=false`) or end (`end=true`) of the VISUAL row that `from` — the caret offset BEFORE
    /// the engine's logical-edge fallback — sits on. `extend` grows the selection. Refines the
    /// engine's logical move; if the row isn't laid out (scrolled off), the logical edge stands.
    fn visual_line_edge(&mut self, end: bool, extend: bool, from: usize) {
        let (w, h) = self.pane_view.canvas_px;
        if w == 0 || h == 0 {
            return;
        }
        let (buffer, win) = self.windowed_buffer(w, h);
        let lh_px = self.line_height * self.render_scale;
        // Resolve the visual row edge (None if the caret's row is scrolled off-window), then ALWAYS
        // return the buffer to the cache before acting on the result.
        let off = (|| {
            // The visual row of the ORIGINAL caret (cy = its top, in pixels).
            let (_cx, cy) = self.win_cursor_at(&win, from).and_then(|c| buffer.cursor_position(&c))?;
            let target_y = cy + 0.5 * lh_px; // vertical center of that visual row
            // x far past the right edge → end of the row; x = 0 → start of the row.
            let target_x = if end { w as f32 * 4.0 } else { 0.0 };
            let cur = (target_y >= 0.0).then(|| buffer.hit(target_x, target_y)).flatten()?;
            Some(self.win_rope_at(&win, cur))
        })();
        self.win_buffer = Some(buffer); // keep the shaped-line cache (see `win_buffer`)
        if let Some(off) = off {
            self.editor.host_mut().move_active_to(off, extend);
            self.goal_x = None;
            self.pane_view.follow = true;
        }
    }

    /// `CARETMOTION visual` Left/Right refine (Tier 3, PER-GLYPH): the engine did the LOGICAL move as a
    /// baseline; replace the caret with the neighbor one step in VISUAL (screen) order. Method: take
    /// the caret's current visual x, then among the caret STOPS in its visual row — each glyph's
    /// leading edge (its logical start) and trailing edge (its logical end), placed on the correct
    /// side for the glyph's own direction (an RTL glyph's start sits on its RIGHT) — pick the nearest
    /// stop strictly to the right (for `right`) or left. Because the pick is by screen-x, motion
    /// REVERSES inside an embedded RTL run, not just when the whole line is RTL (that was Tier 2).
    /// Robust where `layout_cursor` was not: `cursor_glyph`/`cursor_position` match the caret by index
    /// alone (no affinity guess). `from` is the PRE-move caret. Returns None (leaving the logical move)
    /// at a visual row edge (crossing rows/lines) or when the row is scrolled off-window. Mirrors
    /// [`visual_line_edge`](Self::visual_line_edge).
    fn visual_char_move(&mut self, right: bool, extend: bool, from: usize) {
        let (w, h) = self.pane_view.canvas_px;
        if w == 0 || h == 0 {
            return;
        }
        let (buffer, win) = self.windowed_buffer(w, h);
        // The action to take once the buffer borrow is released:
        //   Some((off, Some(x))) = a per-glyph VISUAL move to `off` (remember visual x for the next step)
        //   Some((off, None))    = a plain jump to `off` (a line cross, or restore-to-`from`; no visual x)
        //   None                 = leave the engine's logical baseline in place (it legitimately crossed).
        let mut action: Option<(usize, Option<f32>)> = None;
        if let Some(cur) = self.win_cursor_at(&win, from) {
            // The caret's visual x: carried from the previous step while the caret is still at this
            // offset (so a boundary index keeps the side we approached from), else fresh from the layout.
            let cx = match self.bidi_vx {
                Some((o, x)) if o == from => Some(x),
                _ => buffer.cursor_position(&cur).map(|(x, _)| x),
            };
            if let Some(cx) = cx {
                // Collect the caret's visual row (a wrapped line has several runs) as glyph tuples,
                // then delegate the geometric pick to `visual_stop_pick` (unit-tested).
                let mut glyphs: Vec<(bool, f32, f32, usize, usize)> = Vec::new();
                for run in buffer.layout_runs() {
                    if run.line_i != cur.line || run.cursor_glyph(&cur).is_none() {
                        continue;
                    }
                    glyphs.extend(
                        run.glyphs.iter().map(|g| (g.level.is_ltr(), g.x, g.w, g.start, g.end)),
                    );
                    break; // handled the caret's row
                }
                if let Some((idx, x)) = visual_stop_pick(cx, right, &glyphs) {
                    action = Some((self.win_rope_at(&win, Cursor::new(cur.line, idx)), Some(x)));
                } else {
                    // No visual neighbor this direction in the row — we're at a visual row edge. The engine
                    // ALREADY applied a logical baseline move; the logical neighbor only matches the visual
                    // direction where logical and visual order coincide. Decide by WHERE the baseline
                    // landed:
                    //  • another visual row/line  ⇒ a legitimate cross (LTR line-crossing, wrapped rows) —
                    //    leave the baseline (action stays None);
                    //  • still on THIS visual row ⇒ a wrong-direction bounce (the "oscillates at the end"
                    //    report, e.g. the visual right end of an Arabic run). The logical neighbor is
                    //    visually BEHIND us, so instead CROSS to the adjacent line in DOCUMENT order:
                    //    right-edge-of-LTR / left-edge-of-RTL ⇒ forward (next line's start), else backward
                    //    (previous line's end). No adjacent line ⇒ stop (restore to `from`).
                    let row_lo = glyphs.iter().map(|g| g.3).min();
                    let row_hi = glyphs.iter().map(|g| g.4).max();
                    let baseline = self.editor.doc(self.editor.active_index()).cursor();
                    let baseline_same_row = match (self.win_cursor_at(&win, baseline), row_lo, row_hi) {
                        (Some(bc), Some(lo), Some(hi)) => {
                            bc.line == cur.line && bc.index >= lo && bc.index <= hi
                        }
                        // Baseline off-window / empty row: only "same row" if it didn't actually move.
                        _ => baseline == from,
                    };
                    if baseline_same_row {
                        let row_rtl = buffer.is_rtl(cur.line) == Some(true);
                        let forward = right != row_rtl; // right-edge-of-LTR or left-edge-of-RTL
                        let b = self.editor.doc(self.editor.active_index());
                        let li = b.line_at_byte(from);
                        let target = if forward {
                            (li + 1 < b.line_count()).then(|| b.line_byte_range(li + 1).0)
                        } else {
                            (li > 0).then(|| b.line_byte_range(li - 1).1)
                        };
                        // Cross to the adjacent line, or (no adjacent line) stop by restoring `from`.
                        action = Some((target.unwrap_or(from), None));
                    }
                }
            }
        }
        self.win_buffer = Some(buffer); // keep the shaped-line cache (see `win_buffer`)
        match action {
            Some((off, Some(x))) => {
                self.editor.host_mut().move_active_to(off, extend);
                self.bidi_vx = Some((off, x)); // remember the side, for the next step and the caret draw
                self.goal_x = None;
                self.pane_view.follow = true;
            }
            Some((off, None)) => {
                // A line cross (or restore-to-`from` when there's no adjacent line): plain move, and drop
                // the tracked visual x so the next step re-derives it fresh at the new position.
                self.editor.host_mut().move_active_to(off, extend);
                self.bidi_vx = None;
                self.goal_x = None;
                self.pane_view.follow = true;
            }
            None => {} // baseline crossed rows/lines — leave it (LTR line-crossing still works)
        }
    }

    /// HEX view: map a device-pixel point `(x, y)` within the canvas to a byte offset, using the grid
    /// geometry (`scroll_line` = top row; row height = the scaled line height; cell width = the advance
    /// measured by `render_hex_canvas`). The column → byte-within-row inverse lives in
    /// [`xyscript_app::hex::byte_at_col`]; the result is clamped to the document length.
    fn hex_hit(&self, x: f32, y: f32) -> Option<usize> {
        let s = self.render_scale;
        let lh_px = self.line_height * s;
        let cell_w = if self.hex_cell_w > 0.0 { self.hex_cell_w } else { (self.font_size * s * 0.6).max(1.0) };
        if lh_px <= 0.0 {
            return None;
        }
        let row_rel = (y.max(0.0) / lh_px).floor() as usize;
        let row = self.pane_view.scroll_line + row_rel;
        let col = (x.max(0.0) / cell_w) as usize;
        let i = xyscript_app::hex::byte_at_col(col);
        let total = self.editor.doc(self.editor.active_index()).len();
        Some((row * xyscript_app::hex::BYTES_PER_ROW + i).min(total))
    }

    /// Pixel (in canvas space) → document **rope** byte offset, via cosmic-text
    /// hit-testing. The hit lands in decoded-string space; map it back to a rope
    /// offset (they differ under single-byte views) before it becomes a caret.
    /// Map a click at PIXEL offset `(x, y)` within the canvas to a rope offset.
    fn hit(&mut self, x: f32, y: f32) -> Option<usize> {
        if self.editor.hex_view() {
            return self.hex_hit(x, y);
        }
        let (w, h) = self.pane_view.canvas_px; // PIXELS
        let (buffer, win) = self.windowed_buffer(w, h);
        // WRAPINDENT: a continuation row is drawn shifted right by its indent, so undo that on the
        // click x before hit-testing (no-op when the mode is off / the row is a first row). `WRAP off`
        // draws glyphs shifted LEFT by `scroll_h`, so add it back to map the click into cosmic space.
        let x = x - self.wi_offset_at_y(y) + self.pane_view.scroll_h;
        let r = buffer.hit(x, y).map(|cur| self.win_rope_at(&win, cur));
        self.win_buffer = Some(buffer); // keep the shaped-line cache (see `win_buffer`)
        r
    }

    /// Like [`hit`](Self::hit) but returns the LEADING (logical start) offset of the character whose
    /// cell the point `(x, y)` falls within — not the nearest glyph boundary. Used to ANCHOR a
    /// drag-select so a press that starts anywhere on a character INCLUDES that character (matching
    /// Notepad++ / most editors), instead of dropping it when the press lands past the char's
    /// midpoint. Reads the containing glyph straight from the shaped layout, so it's exact for any
    /// font (no cell-width guessing). Falls back to `hit` when the point is left/right of the row's
    /// glyphs, on an empty row, or off the text entirely.
    fn hit_leading(&mut self, x: f32, y: f32) -> Option<usize> {
        if self.editor.hex_view() {
            return self.hex_hit(x, y); // a byte IS its own leading edge in the grid
        }
        let (w, h) = self.pane_view.canvas_px; // PIXELS
        let (buffer, win) = self.windowed_buffer(w, h);
        // WRAPINDENT: undo the continuation row's draw shift before matching against its glyphs.
        // `WRAP off`: also add back the horizontal scroll so the click maps into cosmic space.
        let x = x - self.wi_offset_at_y(y) + self.pane_view.scroll_h;
        let r = leading_glyph_cursor(&buffer, x, y)
            // Point is left/right of the row's glyphs, on an empty row, or off the text → nearest.
            .or_else(|| buffer.hit(x, y))
            .map(|cur| self.win_rope_at(&win, cur));
        self.win_buffer = Some(buffer); // keep the shaped-line cache (see `win_buffer`)
        r
    }

    /// Like [`hit_leading`](Self::hit_leading) but the TRAILING (logical end) edge of the character
    /// under `(x, y)`. Anchors a LEFTWARD drag-select on the pressed character: with the anchor at the
    /// char's right edge, a drag to the left encloses it (mirror of `hit_leading` for rightward drags).
    fn hit_trailing(&mut self, x: f32, y: f32) -> Option<usize> {
        if self.editor.hex_view() {
            // Trailing edge = just past the pressed byte, so a leftward drag encloses it.
            let total = self.editor.doc(self.editor.active_index()).len();
            return self.hex_hit(x, y).map(|b| (b + 1).min(total));
        }
        let (w, h) = self.pane_view.canvas_px; // PIXELS
        let (buffer, win) = self.windowed_buffer(w, h);
        let x = x - self.wi_offset_at_y(y) + self.pane_view.scroll_h;
        let r = glyph_edge_cursor(&buffer, x, y, true)
            .or_else(|| buffer.hit(x, y))
            .map(|cur| self.win_rope_at(&win, cur));
        self.win_buffer = Some(buffer);
        r
    }

    /// Map a device-pixel point to a `(logical line, column)` grid cell for Alt+drag box-select. The
    /// LINE is the hit-tested line (robust to wrap / vertical scroll); the COLUMN is the raw monospace
    /// grid position from the pixel x (NOT snapped to the line's text), so dragging a rectangle past a
    /// short line's end still records the intended column. Uses the same cell width as the caret paint.
    fn grid_line_col(&mut self, px_x: f32, px_y: f32) -> (usize, usize) {
        let line = self
            .hit(px_x, px_y)
            .map(|off| self.editor.doc(self.editor.active_index()).line_of(off))
            .unwrap_or(self.pane_view.scroll_line);
        let s = self.render_scale;
        let cell = (self.font_size * s * 0.6).max(1.0); // device px per monospace cell (as in caret paint)
        let x_cosmic = px_x - self.wi_offset_at_y(px_y) + self.pane_view.scroll_h;
        let col = (x_cosmic / cell).round().max(0.0) as usize;
        (line, col)
    }
}

/// The cursor at one EDGE of the character whose horizontal cell contains `x` on the visual row
/// containing `y` — the LEADING (logical start) edge when `trailing` is false, else the TRAILING
/// (logical end) edge. `None` when `(x, y)` is outside the laid-out glyphs (before the first / past
/// the last glyph, an empty row, or off the text). Anchors a drag-select on the pressed character:
/// leading for a rightward drag, trailing for a leftward one, so the char is enclosed either way.
/// Free fn so it's unit-testable over a raw shaped buffer.
fn glyph_edge_cursor(buffer: &Buffer, x: f32, y: f32, trailing: bool) -> Option<Cursor> {
    for run in buffer.layout_runs() {
        if y < run.line_top || y >= run.line_top + run.line_height {
            continue;
        }
        for g in run.glyphs.iter() {
            if x >= g.x && x < g.x + g.w {
                return Some(Cursor::new(run.line_i, if trailing { g.end } else { g.start }));
            }
        }
        break; // row found, but x is before/after every glyph
    }
    None
}

/// The LEADING-edge cursor for the character under `(x, y)` — see [`glyph_edge_cursor`].
fn leading_glyph_cursor(buffer: &Buffer, x: f32, y: f32) -> Option<Cursor> {
    glyph_edge_cursor(buffer, x, y, false)
}

/// `CARETMOTION` (visual) core: from the caret's visual x `cx` and its row's glyphs (visual order), pick the
/// logical index of the caret STOP nearest in the requested direction. Each glyph contributes two
/// stops — its leading edge (logical `start`) and trailing edge (logical `end`) — placed on the side
/// matching the glyph's own direction: an LTR glyph's start is on its left, an RTL glyph's start is on
/// its RIGHT. So the pick is purely by screen-x and REVERSES inside an RTL run. `None` = no stop that
/// side (a visual row edge). `glyphs` = `(is_ltr, x, w, start, end)` per glyph. Pure fn → unit-tested.
fn visual_stop_pick(cx: f32, right: bool, glyphs: &[(bool, f32, f32, usize, usize)]) -> Option<(usize, f32)> {
    const EPS: f32 = 0.5; // device px: skip the current stop and stops co-located with it
    let mut best: Option<(f32, usize)> = None;
    for &(ltr, x, w, start, end) in glyphs {
        let lead_x = if ltr { x } else { x + w };
        let trail_x = if ltr { x + w } else { x };
        for (gx, idx) in [(lead_x, start), (trail_x, end)] {
            let ok = if right { gx > cx + EPS } else { gx < cx - EPS };
            let closer = best.map_or(true, |(bx, _)| if right { gx < bx } else { gx > bx });
            if ok && closer {
                best = Some((gx, idx));
            }
        }
    }
    // Return the chosen (logical index, visual x) — the caller carries x forward so a boundary index
    // keeps the side it was approached from (the number-in-Arabic bounce fix).
    best.map(|(x, idx)| (idx, x))
}

impl GuiApp {
    /// Take the desugared operative keymap once, for the winit runtime's key resolver.
    fn take_capture_keymap(&mut self) -> Option<xyscript_kbd::Keymap> {
        self.capture_keymap.take()
    }

    /// Run one resolved command (from a captured key) and follow the caret. Called by the
    /// runtime when a `KeyboardInput` resolves to an assigned command.
    /// Whether the app should exit the event loop (the runtime checks this after each frame).
    pub fn should_exit(&self) -> bool {
        self.should_exit
    }

    /// The keymap a `LOADKBD` just installed (desugared), for the runtime to rebuild its
    /// capture resolver. `None` unless a reload happened since the last drain.
    fn take_reloaded_keymap(&mut self) -> Option<xyscript_kbd::Keymap> {
        self.editor.take_reloaded_keymap()
    }

    /// The `(title, text)` of a parked `TXT` modal dialog, if any — the runtime opens a
    /// separate native window to show it (and keeps it open while this stays `Some`).
    fn txt_request(&self) -> Option<(Option<String>, String)> {
        self.editor.text_request()
    }

    /// The optional two-column grid for the parked TXT dialog (only `LISTLIB` today); the
    /// runtime renders it as a wrapping name/description grid instead of plain lines.
    fn txt_columns(&self) -> Option<Vec<(String, String)>> {
        self.editor.columns_request()
    }

    /// Optional per-line label colours for the parked TXT dialog (only `HLKINDS` today); the runtime
    /// paints each line's leading label in its colour. Aligned with `txt_request`'s lines.
    fn txt_line_colors(&self) -> Option<Vec<Option<(u8, u8, u8)>>> {
        self.editor.line_colors_request()
    }

    // ---- DIR/W separate listing window (the runtime drives these) ----------------------------
    fn take_dir_window_open(&mut self) -> bool {
        self.editor.host_mut().take_dir_window_open()
    }
    fn dir_window_view(&self) -> Option<xyscript_app::host::DirWindowView> {
        self.editor.host().dir_window_view()
    }
    fn close_dir_window(&mut self) {
        self.editor.host_mut().close_dir_window();
    }
    /// Whether the engine still has a `DIR/W` listing. The host can drop it on its own (`DIRWCLOSE
    /// on` dismisses it once a file opens), so the runtime polls this to tear the OS window down.
    fn dir_window_active(&self) -> bool {
        self.editor.host().dir_window_active()
    }
    fn dir_window_move(&mut self, delta: isize) {
        self.editor.host_mut().dir_window_move(delta);
    }
    fn dir_window_to_edge(&mut self, end: bool) {
        self.editor.host_mut().dir_window_to_edge(end);
    }
    fn dir_window_select_row(&mut self, row: usize) {
        self.editor.host_mut().dir_window_select_row(row);
    }
    /// True when a modal dialog or a DIR/LS listing currently owns the keyboard — i.e. one of the
    /// `update()` keyboard branches (quit/breakpoint/MSG/history/PICK/INPUT/close/open/verify/dir
    /// listing) is active rather than normal text editing. Used to route a key bound to the `ENTER`
    /// verb into the active list's activate action (open/run/choose) instead of inserting a newline.
    fn modal_owns_keyboard(&self) -> bool {
        self.quitting
            || self.message_box.is_some()
            || self.history_dialog.is_some()
            || self.pick_dialog.is_some()
            || self.editor.breakpoint_request().is_some()
            || self.editor.awaiting_line()
            || self.editor.close_prompt().is_some()
            || self.editor.open_prompt().is_some()
            || self.editor.verify_active()
            || (self.editor.active_is_dir_listing() && !self.editor.cmline_open())
    }
    /// The command line is open — the primary Enter submits it (it reaches egui, which hits the
    /// cmline handler). Any OTHER key bound to the ENTER verb must submit too, not run the verb
    /// (which would insert a newline into the cmline). Used alongside `modal_owns_keyboard` for the
    /// ENTER synthetic-key case in `intercept_key`.
    fn cmline_open(&self) -> bool {
        self.editor.cmline_open()
    }
    /// True when a *genuine blocking modal* (a prompt/dialog/INPUT/PICK/history/breakpoint/verify/
    /// close-open/quit sweep) currently owns the keyboard. This is `modal_owns_keyboard()` MINUS the
    /// read-only DIR/LS listing — a listing is a normal tab you can Tab away from and run commands
    /// in, not a modal. `intercept_key` uses this to make a bound KBD command inert while a real
    /// modal is up (so a stray F-key can't reorder the document behind a `PICK` list, etc.), while
    /// still letting bindings run over a directory listing.
    fn modal_blocks_bindings(&self) -> bool {
        self.quitting
            || self.message_box.is_some()
            || self.history_dialog.is_some()
            || self.pick_dialog.is_some()
            || self.editor.breakpoint_request().is_some()
            || self.editor.awaiting_line()
            || self.editor.close_prompt().is_some()
            || self.editor.open_prompt().is_some()
            || self.editor.verify_active()
    }
    fn dir_window_copy_path(&mut self) {
        self.editor.host_mut().dir_window_copy_path();
    }
    fn dir_window_find_push(&mut self, ch: char) {
        self.editor.host_mut().dir_window_find_push(ch);
    }
    fn dir_window_find_backspace(&mut self) {
        self.editor.host_mut().dir_window_find_backspace();
    }
    fn dir_window_find_again(&mut self) {
        self.editor.host_mut().dir_window_find_again();
    }
    fn dir_window_find_clear(&mut self) {
        self.editor.host_mut().dir_window_find_clear();
    }
    fn dir_window_refresh_if_changed(&mut self) {
        self.editor.host_mut().dir_window_refresh_if_changed();
    }
    /// Activate the entry under the window's caret. Returns `true` if it opened a file in the
    /// parent (so the runtime can focus the main window to show the new tab).
    fn dir_window_activate(&mut self) -> bool {
        matches!(
            self.editor.host_mut().dir_window_activate(),
            Ok(xyscript_app::host::DirWindowAction::Opened(_))
        )
    }
    /// UI chrome foreground, for the `DIR/W` window's footer text.
    fn editor_ui_fg_color(&self) -> (u8, u8, u8) {
        self.editor.ui_fg_color()
    }
    /// Main-text selection colours (`SELFGCOLOR`/`SELBGCOLOR`), used to highlight the matched
    /// type-to-find prefix within `DIR/W` filename rows.
    fn editor_sel_fg_color(&self) -> (u8, u8, u8) {
        self.editor.sel_fg_color()
    }
    fn editor_sel_bg_color(&self) -> (u8, u8, u8) {
        self.editor.sel_bg_color()
    }
    /// `DIR/W` window appearance — EFFECTIVE values (the `DW…` override, else the inherited editor
    /// setting). The window renders with these so it looks like the editor until customized.
    fn dw_fg_color(&self) -> (u8, u8, u8) {
        self.editor.host().dw_fg_color()
    }
    fn dw_bg_color(&self) -> (u8, u8, u8) {
        self.editor.host().dw_bg_color()
    }
    fn dw_font_pt(&self) -> f32 {
        self.editor.host().dw_font()
    }
    fn dw_scale(&self) -> f32 {
        self.editor.host().dw_scale()
    }
    fn dw_style(&self) -> FontStyle {
        self.editor.host().dw_font_style()
    }
    /// The `DIR/W` window's effective font family: the `DWFAMILY` override if set, else the resolved
    /// editor family (`EDFAMILY` or the default monospace) — so an unset `DWFAMILY` uses `EDFAMILY`.
    fn dw_family(&self) -> Option<String> {
        self.editor
            .host()
            .dw_font_family()
            .map(str::to_string)
            .or_else(|| self.editor_family())
    }

    /// Dismiss the `MSG` box. A blocking (bare-`MSG`) box resumes the parked script; a toast
    /// (`MSG/T:`) just clears. Used by every dismissal path (OK button, ×, Esc/Enter/Space).
    fn dismiss_message_box(&mut self) {
        if let Some(mb) = self.message_box.take() {
            if mb.blocking {
                self.editor.dismiss_message();
            }
        }
    }

    /// `TXT` dialog text size (points) and zoom factor (`TXTFONT`/`TXTSCALE`), for the TXT window.
    fn txt_font(&self) -> f32 {
        self.editor.txt_font()
    }
    fn txt_scale(&self) -> f32 {
        self.editor.txt_scale()
    }
    /// `TXT` window font family (`TXTFAMILY`), or `None` for egui's built-in default monospace.
    fn txt_font_family(&self) -> Option<&str> {
        self.editor.txt_font_family()
    }
    /// `TXT` window font style (`TXTSTYLE`).
    fn txt_font_style(&self) -> FontStyle {
        self.editor.txt_font_style()
    }
    /// The editor canvas font family for cosmic `Attrs`: the user's `EDFAMILY` choice if set,
    /// else the GUI's resolved default monospace (`None` ⇒ cosmic's generic `Monospace`).
    fn editor_family(&self) -> Option<String> {
        self.editor
            .editor_font_family()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| self.mono_family.clone())
    }
    /// The cosmic font DB (the only source of installed-font bytes available to us). Used to feed
    /// `UIFAMILY`/`TXTFAMILY` faces into egui — egui has no system-font access of its own.
    pub(crate) fn font_db(&self) -> &cosmic_text::fontdb::Database {
        self.font_system.db()
    }

    /// Rebuild the main egui context's fonts from scratch (egui's embedded fallbacks retained),
    /// installing the `UIFAMILY` chrome font and the font-picker preview face. The `xy-preview`
    /// family is ALWAYS defined (falling back to the default proportional fonts) so the picker's
    /// preview label has a valid family even before a face loads. One `set_fonts` per change.
    fn rebuild_main_fonts(
        &self,
        ctx: &egui::Context,
        ui_family: Option<&(String, FontStyle)>,
        preview: Option<&(String, FontStyle)>,
    ) {
        use egui::FontFamily::{Monospace, Proportional};
        let db = self.font_system.db();
        let mut defs = egui::FontDefinitions::default();
        if let Some((fam, st)) = ui_family {
            load_face_into(&mut defs, db, fam, "xy-ui", &[Proportional, Monospace], st.bold(), st.italic());
        }
        // Define the preview family (default proportional fallback), then prepend the chosen face.
        let fallback = defs.families.get(&Proportional).cloned().unwrap_or_default();
        defs.families.insert(preview_family_name(), fallback);
        if let Some((fam, st)) = preview {
            load_face_into(&mut defs, db, fam, "xy-preview-face", &[preview_family_name()], st.bold(), st.italic());
        }
        ctx.set_fonts(defs);
    }

    /// The candidate family names for a picker: unique + sorted; monospaced-only when `mono`.
    fn font_families(&self, mono: bool) -> Vec<String> {
        let mut set = std::collections::BTreeSet::new();
        for f in self.font_system.db().faces() {
            if mono && !f.monospaced {
                continue;
            }
            for (name, _) in &f.families {
                set.insert(name.clone());
            }
        }
        set.into_iter().collect()
    }

    /// Open the font picker for `kind`, seeding it with the current family + size for that target.
    fn open_font_picker(&mut self, kind: FontPickerKind) {
        // The editor canvas + DIR/W listing + Preview CODE font are monospaced (column-aligned /
        // code), so their pickers list monospaced families only.
        let mono =
            matches!(kind, FontPickerKind::Editor | FontPickerKind::Dir | FontPickerKind::PreviewCode);
        let families = self.font_families(mono);
        let (current, size, style) = match kind {
            FontPickerKind::Editor => (
                self.editor.editor_font_family().map(str::to_string).or_else(|| self.mono_family.clone()),
                self.editor.editor_font(),
                self.editor.editor_font_style(),
            ),
            FontPickerKind::Ui => (
                self.editor.ui_font_family().map(str::to_string),
                self.editor.chrome_font(),
                self.editor.ui_font_style(),
            ),
            FontPickerKind::Txt => (
                self.editor.txt_font_family().map(str::to_string),
                self.editor.txt_font(),
                self.editor.txt_font_style(),
            ),
            FontPickerKind::Dir => (
                self.dw_family(), // effective: DWFAMILY override else EDFAMILY/default
                self.dw_font_pt(),
                self.dw_style(),
            ),
            // Preview/TOPDF body: family + size (no style — markdown emphasis is per-run). Seed
            // size from PVSIZE, else Typst's ~11 pt default.
            FontPickerKind::PreviewBody => (
                self.editor.preview_font_family().map(str::to_string),
                self.editor.preview_font_size().unwrap_or(11.0),
                FontStyle::Normal,
            ),
            // Preview/TOPDF code: family only (size/style irrelevant — dummy values).
            FontPickerKind::PreviewCode => (
                self.editor.preview_code_family().map(str::to_string),
                11.0,
                FontStyle::Normal,
            ),
        };
        // Highlight the current family if it's in the list; else the first available.
        let selected = current
            .filter(|c| !c.is_empty() && families.iter().any(|f| f.eq_ignore_ascii_case(c)))
            .or_else(|| families.first().cloned())
            .unwrap_or_default();
        self.font_picker = Some(FontPicker {
            kind,
            families,
            selected,
            size,
            size_text: fmt_size(size),
            style,
            scroll_to_selected: true,
        });
    }

    /// Render the open font-picker window (family list + size + live preview + OK/Cancel). On OK,
    /// run the target's `*FAMILY` + `*FONT` verbs; Cancel/× closes without applying.
    fn render_font_picker(&mut self, ctx: &egui::Context) {
        let Some(picker) = self.font_picker.as_mut() else {
            return;
        };
        let title = match picker.kind {
            FontPickerKind::Editor => "Editor Font",
            FontPickerKind::Ui => "UI Font",
            FontPickerKind::Txt => "TXT Font",
            FontPickerKind::Dir => "Directory Window Font",
            FontPickerKind::PreviewBody => "Preview / PDF Body Font",
            FontPickerKind::PreviewCode => "Preview / PDF Code Font",
        };
        // Preview fonts have no global style (markdown carries its own emphasis); the code font also
        // has no size. Hide the controls that don't apply so the dialog isn't misleading.
        let show_size = !matches!(picker.kind, FontPickerKind::PreviewCode);
        let show_style =
            !matches!(picker.kind, FontPickerKind::PreviewBody | FontPickerKind::PreviewCode);
        let preview_size = picker.size;
        // The `xy-preview` family is installed by `rebuild_main_fonts`, but `set_fonts` only takes
        // effect at the START of the next frame — so on the frame the picker opens (and on each
        // selection change) it isn't bound yet. Using an unbound `FontFamily::Name` panics, so fall
        // back to Monospace until it's live; the next frame self-heals once the face is loaded.
        let preview_family = if ctx.fonts(|f| f.families().contains(&preview_family_name())) {
            preview_family_name()
        } else {
            egui::FontFamily::Monospace
        };
        // Esc dismisses (as Cancel), matching the other dialogs. A focused search/size field eats
        // the first Esc to defocus; a second then closes — standard egui behaviour.
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let mut open = true;
        let mut do_ok = false;
        let mut do_cancel = false;
        egui::Window::new(title)
            .id(egui::Id::new("font_picker"))
            .open(&mut open)
            .resizable(true)
            .default_size(egui::vec2(520.0, 460.0))
            .show(ctx, |ui| {
                if show_size || show_style {
                    ui.horizontal(|ui| {
                        if show_size {
                            ui.label("Size:");
                            // An always-visible text box for typing (a TextEdit can't live INSIDE a
                            // ComboBox popup — egui closes the popup on the click). `size_text` is the
                            // typing buffer; `size` follows when it parses in-range, so a half-typed
                            // value never sticks and the box is always editable.
                            let edit = ui.add(
                                egui::TextEdit::singleline(&mut picker.size_text).desired_width(44.0),
                            );
                            if edit.changed() {
                                if let Ok(v) = picker.size_text.trim().parse::<f32>() {
                                    if (6.0..=200.0).contains(&v) {
                                        picker.size = v;
                                    }
                                }
                            }
                            ui.label("pt");
                            // The pulldown holds only presets (selecting one closes the popup).
                            egui::ComboBox::from_id_salt("font_size_presets")
                                .selected_text("▾")
                                .width(0.0)
                                .show_ui(ui, |ui| {
                                    for &s in SIZE_PRESETS {
                                        let on = (picker.size - s).abs() < 0.05;
                                        if ui.selectable_label(on, format!("{} pt", fmt_size(s))).clicked() {
                                            picker.size = s;
                                            picker.size_text = fmt_size(s);
                                        }
                                    }
                                });
                        }
                        if show_style {
                            if show_size {
                                ui.separator();
                            }
                            ui.label("Style:");
                            for (label, st) in [
                                ("Normal", FontStyle::Normal),
                                ("Bold", FontStyle::Bold),
                                ("Italic", FontStyle::Italic),
                                ("Bold Italic", FontStyle::BoldItalic),
                            ] {
                                ui.selectable_value(&mut picker.style, st, label);
                            }
                        }
                    });
                }
                ui.label(format!("Family: {}", picker.selected));
                ui.separator();
                egui::ScrollArea::vertical().max_height(240.0).auto_shrink([false, false]).show(ui, |ui| {
                    if picker.families.is_empty() {
                        ui.weak("(no matching font families installed)");
                    }
                    for fam in &picker.families {
                        let sel = fam.eq_ignore_ascii_case(&picker.selected);
                        let row = ui.selectable_label(sel, fam.as_str());
                        // Bring the current selection into view only on open; afterward the wheel
                        // owns the scroll position (a per-frame `scroll_to_me` would fight it).
                        if sel && picker.scroll_to_selected {
                            row.scroll_to_me(None);
                        }
                        if row.clicked() {
                            picker.selected = fam.clone();
                        }
                    }
                    picker.scroll_to_selected = false;
                });
                ui.separator();
                ui.label("Preview:");
                ui.add(
                    egui::Label::new(
                        egui::RichText::new("The quick brown fox jumps over the lazy dog 0123456789")
                            .family(preview_family.clone())
                            .size(preview_size),
                    )
                    .wrap(),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        do_ok = true;
                    }
                    if ui.button("Cancel").clicked() {
                        do_cancel = true;
                    }
                });
            });
        if do_ok {
            let picker = self.font_picker.take().unwrap();
            match picker.kind {
                // Preview/TOPDF body: family + size, no style.
                FontPickerKind::PreviewBody => {
                    if !picker.selected.is_empty() {
                        self.editor.run_command(&format!("PVFAMILY {}", picker.selected));
                    }
                    self.editor.run_command(&format!("PVSIZE {}", picker.size));
                }
                // Preview/TOPDF code: family only.
                FontPickerKind::PreviewCode => {
                    if !picker.selected.is_empty() {
                        self.editor.run_command(&format!("PVCODE {}", picker.selected));
                    }
                }
                _ => {
                    let (fam_verb, size_verb, style_verb) = match picker.kind {
                        FontPickerKind::Editor => ("EDFAMILY", "EDFONT", "EDSTYLE"),
                        FontPickerKind::Ui => ("UIFAMILY", "UIFONT", "UISTYLE"),
                        FontPickerKind::Txt => ("TXTFAMILY", "TXTFONT", "TXTSTYLE"),
                        _ => ("DWFAMILY", "DWFONT", "DWSTYLE"), // Dir
                    };
                    if !picker.selected.is_empty() {
                        self.editor.run_command(&format!("{fam_verb} {}", picker.selected));
                    }
                    self.editor.run_command(&format!("{size_verb} {}", picker.size));
                    self.editor.run_command(&format!("{style_verb} {}", picker.style.keyword()));
                }
            }
        } else if !open || do_cancel || esc {
            self.font_picker = None;
        }
    }

    /// Open the Find/Replace dialog, seeding the Find field from the current selection (single-line
    /// only). `replace` focuses the Replace field (opened via "Replace…"). If it's already open,
    /// just refocus the relevant field.
    /// The origin for a "From top" OFF search: the caret, but clamped DOWN-file to the top visible line
    /// when the caret is scrolled off-screen above it. Scrolling the view (mouse wheel) doesn't move the
    /// caret, so a search from the raw caret would jump UP to near the top of the file — reading as
    /// "Find starts at the top" — while the user is looking lower down. Using the top of what they can
    /// see keeps the search "from where I am".
    fn find_origin(&self) -> usize {
        let scroll_line = self.pane_view.scroll_line;
        let doc = self.editor.doc(self.editor.active_index());
        let top_visible = doc.line_byte_range(scroll_line).0;
        doc.cursor().max(top_visible)
    }

    fn open_find_dialog(&mut self, replace: bool) {
        if self.find_dialog.is_some() {
            self.find_refocus = true; // raise + focus the existing FIND window
            let d = self.find_dialog.as_mut().unwrap();
            d.focus_find = !replace;
            d.focus_replace = replace;
            return;
        }
        // Seed from a single-line selection if there is one; otherwise remember the last search
        // term (from any source). On open the field is selected (below), so Backspace/typing replaces it.
        let sel = self.editor.selection_text();
        let seed = if !sel.is_empty() && !sel.contains('\n') {
            sel
        } else {
            self.editor.last_search()
        };
        let (case, whole_word, regex, in_selection, keep_case, from_top, wrap) = self.find_opts;
        // Pin the search origin NOW (before any type-to-find moves the caret), so "From top" OFF
        // searches forward from where the user actually is (see `find_origin`).
        let origin = self.find_origin();
        self.find_dialog = Some(FindDialog {
            find: seed.clone(),
            replace: String::new(),
            case,
            whole_word,
            regex,
            in_selection,
            keep_case,
            from_top,
            wrap,
            focus_find: !replace,
            focus_replace: replace,
            prev_from_top: from_top,
            origin,
        });
    }

    /// Whether the FIND dialog is open — the runtime creates/destroys its OS window to match.
    fn find_dialog_open(&self) -> bool {
        self.find_dialog.is_some()
    }

    /// Consume the one-shot "raise + focus the FIND window" request (set by `open_find_dialog` when
    /// invoked while the window is already open).
    fn take_find_refocus(&mut self) -> bool {
        std::mem::take(&mut self.find_refocus)
    }

    /// Clear all FIND state (dialog + live highlight-all) — the runtime calls this when the window is
    /// closed (Esc / Close button / the window's ×).
    fn close_find_state(&mut self) {
        // Commit the field as the "last search" so a later F3 (dialog closed) continues it — even if the
        // last thing in the dialog was only a live type-to-find (which never touches the engine's search
        // state). A no-op for an empty field.
        if let Some(d) = self.find_dialog.as_ref() {
            let (find, case, whole, regex) = (d.find.clone(), d.case, d.whole_word, d.regex);
            self.editor.commit_last_search(&find, case, whole, regex);
        }
        self.find_dialog = None;
        self.find_refocus = false;
        self.find_highlights.clear();
        self.find_current = None;
        self.find_hl_key = None;
        self.find_hl_cache.clear();
        self.find_hl_params = None;
        self.find_win_params = None;
    }

    /// Whether the active buffer is a directory listing (F3 there is express-find, not Find Next).
    fn active_is_dir_listing(&self) -> bool {
        self.editor.active_is_dir_listing()
    }

    /// Whether the OS window title bar should be shown (the runtime applies it via `set_decorations`).
    fn show_titlebar(&self) -> bool {
        self.editor.show_titlebar()
    }

    /// Whether Writer's View is on — the runtime reads it (with `show_titlebar`) to drop the OS title
    /// bar, and `Esc` handling checks it to exit the mode.
    fn writer_view(&self) -> bool {
        self.editor.writer_view()
    }

    /// Turn Writer's View off (the runtime's `Esc`-exits-WV path). Returns whether it WAS on.
    fn exit_writer_view(&mut self) -> bool {
        self.editor.exit_writer_view()
    }

    /// Whether to remember the main window's size + position across runs (the runtime gates both
    /// restore and save on this; `$WINGEO`).
    fn remember_window_geometry(&self) -> bool {
        self.editor.remember_window_geometry()
    }

    /// Whether the auto-save-and-restore session feature (`SESSION`) is on.
    fn session_enabled(&self) -> bool {
        self.editor.host().session_restore_enabled()
    }

    /// Snapshot the open tabs to the per-user session dir if `SESSION` is on. Called both on the
    /// periodic timer (crash resilience — the snapshot is changed-only, so an idle tick does no disk
    /// work) and on graceful exit (a final flush). Best-effort: a failure is logged, not surfaced.
    fn snapshot_session(&mut self) {
        if !self.editor.host().session_restore_enabled() {
            return;
        }
        self.sync_pane_views_to_host(); // so the saved layout carries each pane's live scroll offset
        if let Some(dir) = crate::runtime::session_dir() {
            if let Err(e) = self.editor.host_mut().save_session(&dir) {
                eprintln!("xyscript: session save failed: {e}");
            }
        }
    }

    /// Push the live per-pane scroll (held here in `pane_view`/`pane_views`, not in the host) into the
    /// host's pane tree so a `SESSION` snapshot records it. The focused pane's authoritative viewport is
    /// `pane_view`; every other pane's is in `pane_views`. Set the others first, then the focused pane,
    /// so the focused offset always wins even if a stale copy lingers in the map.
    fn sync_pane_views_to_host(&mut self) {
        let focused = self.editor.host().focused_pane_id();
        let others: Vec<_> = self
            .pane_views
            .iter()
            .map(|(p, v)| (*p, v.scroll_line, v.scroll_vertical, v.follow))
            .collect();
        for (p, sl, sv, f) in others {
            self.editor.host_mut().set_pane_scroll(p, sl, sv, f);
        }
        self.editor.host_mut().set_pane_scroll(
            focused,
            self.pane_view.scroll_line,
            self.pane_view.scroll_vertical,
            self.pane_view.follow,
        );
    }

    /// At startup, restore the previous session if `SESSION` is on (a no-op otherwise, or when there's
    /// nothing saved). The per-user dir mirrors the window-geometry location.
    fn restore_session_if_enabled(&mut self) {
        if !self.editor.host().session_restore_enabled() {
            return;
        }
        if let Some(dir) = crate::runtime::session_dir() {
            match self.editor.host_mut().restore_session(&dir) {
                Ok(n) if n > 0 => self.load_pane_views_from_host(),
                Ok(_) => {}
                Err(e) => eprintln!("xyscript: session restore failed: {e}"),
            }
        }
    }

    /// Seed the live per-pane viewports from the pane scroll offsets a restore just loaded into the host
    /// tree (the inverse of [`sync_pane_views_to_host`](Self::sync_pane_views_to_host)). Without this the
    /// panes would come back at the top of their docs (or merely following the caret); with it they
    /// return to the exact spot they were scrolled to. Pinning `last_focus` stops the first frame's
    /// focus-swap from clobbering the focused pane's restored view.
    fn load_pane_views_from_host(&mut self) {
        let focused = self.editor.host().focused_pane_id();
        self.pane_views.clear();
        for (pid, _doc) in self.editor.host().panes().panes() {
            if let Some((sl, sv, f)) = self.editor.host().pane_scroll(pid) {
                let pv = PaneView { scroll_line: sl, scroll_vertical: sv, follow: f, scroll_h: 0.0, content_w: 0.0, canvas_px: (0, 0), scroll_drag_ratio: None };
                if pid == focused {
                    self.pane_view = pv;
                } else {
                    self.pane_views.insert(pid, pv);
                }
            }
        }
        self.last_focus = Some(focused);
    }

    /// Whether the title-bar drag strip was grabbed this frame (consumed; the runtime starts the move).
    fn take_window_drag(&mut self) -> bool {
        std::mem::take(&mut self.start_window_drag)
    }

    /// F3 / Shift+F3 — repeat the last search forward / backward (Find Next / Find Prev). A bare
    /// `SE`/`SEB` repeats the last search with its options (case/whole/regex) via the host's
    /// `last_spec`. Status/feedback surface as usual; the match scrolls into view.
    /// The runtime calls this from `Focused` events: `true` iff SOME XyScript window holds OS focus.
    /// Drives `DIMONBLUR` (dim the main window only when the whole app is inactive).
    fn set_app_focus(&mut self, focused: bool) {
        self.app_has_focus = focused;
        // Stamp when we went unfocused (keep the earliest of a run); clear on regaining focus. The dim
        // check below only fires after a grace period, so a brief within-app focus hop never dims.
        if focused {
            self.unfocused_since = None;
        } else if self.unfocused_since.is_none() {
            self.unfocused_since = Some(std::time::Instant::now());
        }
    }

    fn find_again(&mut self, backward: bool) {
        // With the FIND dialog open, F3 / Shift+F3 search the CURRENT field text + options — identical
        // to Enter / Find Prev — rather than the engine's `last_search`, which the live type-to-find
        // never updates (so a bare `SE` here would search a STALE string that differs from the field).
        // With the dialog closed, F3 is the standalone "repeat the last committed search".
        if self.find_dialog.is_some() {
            self.find_step(backward);
        } else {
            self.editor.run_command(if backward { "SEB" } else { "SE" });
            self.pane_view.follow = true;
        }
    }

    /// One search step for the FIND dialog: the CURRENT field text + options, `backward` or forward,
    /// honouring "Wrap around" (retry a not-found search from the far end). Shared by the dialog's
    /// Find Next / Find Prev / Enter AND by F3 / Shift+F3, so F3 always matches Enter. No-op if the
    /// dialog is closed or the field is empty.
    fn find_step(&mut self, backward: bool) {
        let Some((find, case, whole, regex, scope, wrap)) = self
            .find_dialog
            .as_ref()
            .map(|d| (d.find.clone(), d.case, d.whole_word, d.regex, d.in_selection, d.wrap))
        else {
            return;
        };
        if find.is_empty() {
            return;
        }
        let r = self.editor.search_spec(&find, None, backward, case, whole, regex, scope, false, false);
        let r = if r.is_err() && wrap {
            self.editor.search_spec(&find, None, backward, case, whole, regex, scope, false, true)
        } else {
            r
        };
        self.note(r);
        self.pane_view.follow = true;
    }

    /// Handle a directory-listing express-find CONTROL key ahead of the keymap (`find_again` = F3
    /// steps to the next match; otherwise Backspace shortens the type-to-find query). Routing these
    /// before the keymap is what lets them work even though e.g. `Backspace` is bound to `BD`.
    /// Returns `true` if consumed; a no-op (false) outside a dir listing, during a delete-confirm,
    /// or while the command line is open.
    fn dir_express_control(&mut self, find_again: bool) -> bool {
        if !self.editor.active_is_dir_listing()
            || self.editor.dir_delete_prompt().is_some()
            || self.editor.cmline_open()
        {
            return false;
        }
        if find_again {
            self.editor.dir_find_again();
        } else {
            self.editor.dir_find_backspace();
        }
        self.pane_view.follow = true;
        true
    }

    /// The Find/Replace form, rendered in the separate FIND *OS window* (`FindWindow`, driven by the
    /// runtime). Returns the outcome: `close` when Esc / the Close button dismisses it, and
    /// `main_redraw` when a search moved the caret or changed the live highlights (the main document
    /// window must then repaint to show it). The search composes a real `SE`/`CH` via
    /// `editor.search_spec` — the dialog never duplicates search logic.
    /// Recompute the FIND "highlight all" match set for the ACTIVE document when the Find text/options
    /// or the document changed (cheap — key-cached on `find_hl_key`, a no-op otherwise). Returns whether
    /// the set changed. Called BOTH from the Find window's own render AND from the main window before the
    /// canvas paints — the latter matters because the Find window is a SEPARATE OS window on its own
    /// render schedule, so without the main-loop call a tab switch shows the file a frame (or more)
    /// before its hits reappear. Doing it here paints file + hits together.
    fn refresh_find_highlights(&mut self) -> bool {
        let Some(d) = self.find_dialog.as_ref() else { return false };
        let (find, case, whole, regex, scope) =
            (d.find.clone(), d.case, d.whole_word, d.regex, d.in_selection);
        if find.is_empty() {
            let had = !self.find_highlights.is_empty();
            self.find_highlights.clear();
            self.find_current = None;
            self.find_hl_key = None;
            self.find_hl_cache.clear();
            self.find_hl_params = None;
            return had;
        }
        // A change to the Find text/options makes EVERY document's cached matches stale.
        let params = (find.clone(), case, whole, regex, scope);
        if self.find_hl_params.as_ref() != Some(&params) {
            self.find_hl_cache.clear();
            self.find_hl_params = Some(params);
        }
        let rev = self.editor.doc_revision();
        let key = (find.clone(), case, whole, regex, scope, rev);
        if self.find_hl_key.as_ref() == Some(&key) {
            return false; // `find_highlights` already current for this exact (doc, options, revision)
        }
        // Reuse this document's prior search when it's still valid (same revision) — the per-doc cache
        // makes a tab switch instant even on a big file. In-selection is selection-dependent (revision
        // doesn't track selection), so it bypasses the cache and always recomputes.
        let did = self.editor.active_doc_id();
        let cached = (!scope)
            .then(|| self.find_hl_cache.get(&did))
            .flatten()
            .filter(|(crev, _)| *crev == rev)
            .map(|(_, spans)| spans.clone());
        self.find_highlights = match cached {
            Some(spans) => spans,
            None => {
                let hits = self.editor.find_all(&find, case, whole, regex, scope);
                if !scope {
                    self.find_hl_cache.insert(did, (rev, hits.clone()));
                }
                hits
            }
        };
        self.find_hl_key = Some(key);
        true
    }

    fn find_window_ui(&mut self, ui: &mut egui::Ui) -> FindOutcome {
        let mut outcome = FindOutcome::default();
        // Esc dismisses. Read it before the text field runs (a focused TextEdit would otherwise
        // consume the Escape for focus-surrender).
        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
        // Button intents collected inside the closure, applied after (so `self.editor` is freely
        // borrowable — the closure only touches the dialog state `d`).
        let mut do_find_next = false;
        let mut do_find_prev = false;
        let mut do_replace = false;
        let mut do_replace_all = false;
        let mut do_close = false;
        // Localized labels — before the `find_dialog` borrow (see `render_grep_dialog`).
        let l_find = self.tr("dialog.find.find_label", "Find:");
        let l_find_hint = self.tr("dialog.find.find_hint", "text to find");
        let l_replace_label = self.tr("dialog.find.replace_label", "Replace:");
        let l_replace_hint = self.tr("dialog.find.replace_hint", "replacement");
        let l_case = self.tr("dialog.find.case", "Case sensitive");
        let l_whole = self.tr("dialog.find.whole_word", "Whole word");
        let l_regex = self.tr("dialog.find.regex", "Regular expression");
        let l_in_sel = self.tr("dialog.find.in_selection", "In selection");
        let l_from_top = self.tr("dialog.find.from_top", "From top of file");
        let l_from_top_hint = self.tr("dialog.find.from_top_hint", "Begin the search at the top of the file (land on the first match); Find Next then advances forward. Off: begin at the cursor.");
        let l_wrap = self.tr("dialog.find.wrap", "Wrap around");
        let l_wrap_hint = self.tr("dialog.find.wrap_hint", "Find Next past the last match wraps to the first (and Find Prev past the first wraps to the last), instead of stopping at the end.");
        let l_keep = self.tr("dialog.find.keep_case", "Keep case in replacement");
        let l_keep_hint = self.tr("dialog.find.keep_case_hint", "Replace / Replace all: recase each replacement to match the case of the text it replaces (lower / Capitalized / UPPER).");
        let l_next = self.tr("dialog.find.find_next", "Find next (F3)");
        let l_prev = self.tr("dialog.find.find_prev", "Find prev (Shift+F3)");
        let l_replace = self.tr("dialog.find.replace", "Replace");
        let l_replace_all = self.tr("dialog.find.replace_all", "Replace all");
        let l_close = self.tr("button.close", "Close");
        let Some(d) = self.find_dialog.as_mut() else {
            return outcome;
        };
        // The FIND window uses egui's default light theme, whose foreground strokes are a medium
        // gray that reads as faint. Darken the widget text (labels, field text, checkbox/button
        // labels) to a near-black so it's easy to read; leave the weak/placeholder colour alone so
        // the field hint text still reads as a hint.
        {
            let dark = egui::Color32::from_gray(10);
            let v = ui.visuals_mut();
            v.widgets.noninteractive.fg_stroke.color = dark; // labels ("Find:", "Replace:")
            v.widgets.inactive.fg_stroke.color = dark; // typed field text, checkbox/button text
            v.widgets.hovered.fg_stroke.color = dark;
            v.widgets.active.fg_stroke.color = dark;
        }
        egui::Frame::new().inner_margin(egui::Margin::same(12)).show(ui, |ui| {
                egui::Grid::new("find_fields")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(&l_find);
                        let mut find_out = egui::TextEdit::singleline(&mut d.find)
                            .desired_width(260.0)
                            .hint_text(&l_find_hint)
                            .show(ui);
                        let find_edit = find_out.response;
                        if d.focus_find {
                            find_edit.request_focus();
                            // Select the whole seeded term so Backspace clears it / typing replaces it.
                            let len = d.find.chars().count();
                            find_out.state.cursor.set_char_range(Some(
                                egui::text::CCursorRange::two(
                                    egui::text::CCursor::new(0),
                                    egui::text::CCursor::new(len),
                                ),
                            ));
                            find_out.state.store(ui.ctx(), find_edit.id);
                            d.focus_find = false;
                        }
                        // Enter in the Find field = Find Next. A single-line TextEdit LOSES focus on
                        // Enter, so re-request it — otherwise only the first Enter fires and pressing
                        // Enter repeatedly (to walk through the matches) does nothing until you click
                        // back into the field.
                        if find_edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            do_find_next = true;
                            find_edit.request_focus();
                        }
                        ui.end_row();
                        ui.label(&l_replace_label);
                        let replace_edit = ui.add(
                            egui::TextEdit::singleline(&mut d.replace)
                                .desired_width(260.0)
                                .hint_text(&l_replace_hint),
                        );
                        if d.focus_replace {
                            replace_edit.request_focus();
                            d.focus_replace = false;
                        }
                        ui.end_row();
                    });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut d.case, &l_case);
                    ui.checkbox(&mut d.whole_word, &l_whole);
                    ui.checkbox(&mut d.from_top, &l_from_top).on_hover_text(&l_from_top_hint);
                    ui.checkbox(&mut d.wrap, &l_wrap).on_hover_text(&l_wrap_hint);
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut d.regex, &l_regex);
                    ui.checkbox(&mut d.in_selection, &l_in_sel);
                    ui.checkbox(&mut d.keep_case, &l_keep).on_hover_text(&l_keep_hint);
                });
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    // The F3 / Shift+F3 hints match the universal shortcuts (runtime.rs) — they work
                    // even while this dialog's field has focus. Wrapped so a narrow window doesn't clip.
                    if ui.button(&l_next).clicked() {
                        do_find_next = true;
                    }
                    if ui.button(&l_prev).clicked() {
                        do_find_prev = true;
                    }
                    ui.separator();
                    if ui.button(&l_replace).clicked() {
                        do_replace = true;
                    }
                    if ui.button(&l_replace_all).clicked() {
                        do_replace_all = true;
                    }
                    ui.separator();
                    if ui.button(&l_close).clicked() {
                        do_close = true;
                    }
                });
            });

        // Apply (now `self.editor` is borrowable; the `d` borrow ended at its last use above).
        let (find, replace, case, whole, regex, scope, keep_case, from_top, wrap) = {
            let d = self.find_dialog.as_ref().unwrap();
            (d.find.clone(), d.replace.clone(), d.case, d.whole_word, d.regex, d.in_selection, d.keep_case, d.from_top, d.wrap)
        };
        // Remember the options so reopening the dialog restores them.
        self.find_opts = (case, whole, regex, scope, keep_case, from_top, wrap);
        if !find.is_empty() && (do_find_next || do_find_prev || do_replace || do_replace_all) {
            if do_replace_all {
                let r = self.editor.search_spec(&find, Some(replace), false, case, whole, regex, scope, keep_case, false);
                self.note(r);
            } else {
                // Single replace: overwrite the selected match (if any) first, then find next.
                // `replace_selection` OVERWRITES the span as one undo unit, ignoring persistent-
                // selection mode — under PS a plain insert keeps the block and edits at the caret,
                // leaving the match behind. `Keep case` recases the replacement to the match's case
                // first (the engine's per-match rule can't reach this one-off overwrite).
                if do_replace && self.editor.has_selection() {
                    let repl = if keep_case {
                        xyscript_app::host::recase_to_match(&self.editor.selection_text(), &replace)
                    } else {
                        replace.clone()
                    };
                    self.editor.replace_selection(&repl);
                }
                // Find Next / Find Prev / find-after-replace — the SAME shared step F3/Shift+F3 use.
                self.find_step(do_find_prev);
            }
            self.pane_view.follow = true;
            outcome.main_redraw = true;
        }

        // Esc / Close → tell the runtime to tear the window down (it clears the find state).
        if do_close || esc {
            outcome.close = true;
            return outcome;
        }
        // Live "highlight all": recompute the match set from the FIND WINDOW only when ITS OWN inputs
        // changed (the find text or an option toggle — needed so the type-to-find below sees fresh
        // matches). A DOCUMENT edit is deliberately NOT handled here: the main window's pre-render
        // refresh owns that, and keeping it the sole writer during editing stops the highlights from
        // micro-jittering when both windows recompute on independent repaint schedules. `find_current`
        // is (re)computed in `raster` from the live caret.
        // `find_win_params` is None right after open/close, so `inputs_changed` is also true on the
        // FIRST frame — which makes OPENING the dialog perform an initial highlight + landing.
        let win_params = (find.clone(), case, whole, regex, scope);
        let inputs_changed = self.find_win_params.as_ref() != Some(&win_params);
        if inputs_changed {
            self.find_win_params = Some(win_params);
            if self.refresh_find_highlights() {
                outcome.main_redraw = true; // main window repaints to show the updated highlights
            }
        }

        // Live type-to-find: whenever the search TEXT or any OPTION changes — or "From top" is toggled —
        // jump the caret to a match and centre it (without Enter/F3). Keying the jump off ANY option
        // change is what makes ticking Case / Whole word / Regex / In selection / From top take effect
        // immediately, rather than only after closing and reopening. With "From top" ON the target is
        // the FIRST match in the document; OFF, the first match at/after the fixed search ORIGIN (forward
        // from where the user was). The centering is one-shot (`center_caret`), so an on-screen target
        // isn't scrolled.
        let (prev_from_top, origin) = {
            let d = self.find_dialog.as_ref().unwrap();
            (d.prev_from_top, d.origin)
        };
        if find.is_empty() {
            // Field cleared — re-pin the origin so the NEXT term searches forward from where we are now.
            self.find_dialog.as_mut().unwrap().origin = self.find_origin();
        } else if inputs_changed || from_top != prev_from_top {
            if let Some(pos) = find_landing_target(from_top, &self.find_highlights, origin) {
                self.editor.host_mut().set_active_caret(pos, None);
                self.pane_view.follow = true;
                self.center_caret = true; // centre the target if it's off-screen
                outcome.main_redraw = true;
            }
        }
        self.find_dialog.as_mut().unwrap().prev_from_top = from_top;
        outcome
    }

    /// Open the Run-command dialog (Tools ▸ Shell command…), focused and empty.
    fn open_run_dialog(&mut self) {
        if let Some(d) = self.run_dialog.as_mut() {
            d.focus_cmd = true;
            return;
        }
        self.run_dialog = Some(RunCmdDialog {
            cmd: String::new(),
            background: false,
            insert_output: false,
            focus_cmd: true,
        });
    }

    /// Render the non-modal Run-command dialog. Run composes a real shell verb and runs it:
    /// `XY` (insert output at the caret) when "Insert output" is ticked, else `X` (to the *Shell
    /// Output* tab) with an optional `/BG`. Shell-out is `TRUST`-gated.
    fn render_run_dialog(&mut self, ctx: &egui::Context) {
        // Localized labels — before the `run_dialog` borrow (see `render_grep_dialog`).
        let l_title = self.tr("dialog.run.title", "Run command");
        let l_shell_off = self.tr("dialog.run.shell_off", "Shell commands are off — enable in Settings ⏵ Allow shell commands.");
        let l_command = self.tr("dialog.run.command", "Command:");
        let l_command_hint = self.tr("dialog.run.command_hint", "a shell command");
        let l_background = self.tr("dialog.run.background", "Run in background");
        let l_insert = self.tr("dialog.run.insert_output", "Insert output at cursor");
        let l_run = self.tr("dialog.run.run", "Run");
        let l_close = self.tr("button.close", "Close");
        let Some(d) = self.run_dialog.as_mut() else {
            return;
        };
        let trusted = self.editor.trusted();
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let mut open = true;
        let mut do_run = false;
        let mut do_close = false;
        egui::Window::new(l_title)
            .id(egui::Id::new("run_dialog"))
            .open(&mut open)
            .resizable(true)
            .default_width(420.0)
            .show(ctx, |ui| {
                if !trusted {
                    // `⏵` (U+23F5), not `▸` (U+25B8) — see the submenu marker: only the former is in
                    // egui's bundled fonts, and this label renders with them.
                    ui.colored_label(ui.visuals().warn_fg_color, &l_shell_off);
                    ui.add_space(2.0);
                }
                ui.horizontal(|ui| {
                    ui.label(&l_command);
                    let cmd_edit = ui.add(
                        egui::TextEdit::singleline(&mut d.cmd)
                            .desired_width(320.0)
                            .hint_text(&l_command_hint),
                    );
                    if d.focus_cmd {
                        cmd_edit.request_focus();
                        d.focus_cmd = false;
                    }
                    // Enter in the field = Run (only when trusted and non-empty).
                    if trusted
                        && !d.cmd.trim().is_empty()
                        && cmd_edit.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        do_run = true;
                    }
                });
                ui.add_space(2.0);
                // Background is meaningless once output is being inserted at the caret (that needs a
                // synchronous run), so it's disabled while "Insert output" is on.
                ui.add_enabled_ui(!d.insert_output, |ui| {
                    ui.checkbox(&mut d.background, &l_background);
                });
                ui.checkbox(&mut d.insert_output, &l_insert);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(trusted && !d.cmd.trim().is_empty(), |ui| {
                        if ui.button(&l_run).clicked() {
                            do_run = true;
                        }
                    });
                    if ui.button(&l_close).clicked() {
                        do_close = true;
                    }
                });
            });

        // Apply (self.editor borrowable now).
        let (cmd, background, insert_output) = {
            let d = self.run_dialog.as_ref().unwrap();
            (d.cmd.trim().to_string(), d.background, d.insert_output)
        };
        if do_run && trusted && !cmd.is_empty() {
            // Compose the verb: XY inserts output at the caret; otherwise X (to the *Shell Output*
            // tab), backgrounded with /BG. XY never gets /BG (it must run synchronously to insert).
            let composed = if insert_output {
                format!("XY {cmd}")
            } else if background {
                format!("X/BG {cmd}")
            } else {
                format!("X {cmd}")
            };
            self.editor.run_command(&composed);
            self.pane_view.follow = true;
        }
        if do_close || esc || !open {
            self.run_dialog = None;
        }
    }

    fn open_grep_dialog(&mut self) {
        if let Some(d) = self.grep_dialog.as_mut() {
            d.focus_pattern = true;
            return;
        }
        self.grep_dialog = Some(GrepDialog {
            pattern: String::new(),
            path: String::new(),
            regex: false,
            ignore_case: false,
            whole: false,
            focus_pattern: true,
        });
    }

    /// Render the GREP dialog (Search ▸ Grep…). Search dispatches `EditorOp::Grep` directly (literal
    /// pattern, no `$`-expansion); results stream into a read-only tab, so the dialog is pure front-end
    /// and can be closed while the search runs. Open Search ▸ Emphasize / `GJ` still work on the tab.
    fn render_grep_dialog(&mut self, ctx: &egui::Context) {
        // Localized labels — computed BEFORE the `grep_dialog` borrow so `tr` can read `self`.
        let l_title = self.tr("dialog.grep.title", "Grep");
        let l_pattern = self.tr("dialog.grep.pattern", "Search for:");
        let l_pattern_hint = self.tr("dialog.grep.pattern_hint", "text (or a regex with Regular expression)");
        let l_folder = self.tr("dialog.grep.folder", "In folder:");
        let l_folder_hint = self.tr("dialog.grep.folder_hint", "blank = current folder — or  src  ·  *.rs  ·  src/*.rs");
        let l_regex = self.tr("dialog.grep.regex", "Regular expression");
        let l_ignore = self.tr("dialog.grep.ignore_case", "Ignore case");
        let l_whole = self.tr("dialog.grep.whole_word", "Whole word");
        let l_search = self.tr("dialog.grep.search", "Search");
        let l_close = self.tr("button.close", "Close");
        let Some(d) = self.grep_dialog.as_mut() else {
            return;
        };
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let mut open = true;
        let mut do_search = false;
        let mut do_close = false;
        egui::Window::new(l_title)
            .id(egui::Id::new("grep_dialog"))
            .open(&mut open)
            .resizable(true)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(&l_pattern);
                    let e = ui.add(
                        egui::TextEdit::singleline(&mut d.pattern)
                            .desired_width(340.0)
                            .hint_text(&l_pattern_hint),
                    );
                    if d.focus_pattern {
                        e.request_focus();
                        d.focus_pattern = false;
                    }
                    // Enter in a field = Search (when the pattern is non-empty).
                    if !d.pattern.trim().is_empty()
                        && e.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        do_search = true;
                    }
                });
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label(&l_folder);
                    let e = ui.add(
                        egui::TextEdit::singleline(&mut d.path)
                            .desired_width(340.0)
                            .hint_text(&l_folder_hint),
                    );
                    if !d.pattern.trim().is_empty()
                        && e.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        do_search = true;
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut d.regex, &l_regex);
                    ui.checkbox(&mut d.ignore_case, &l_ignore);
                    ui.checkbox(&mut d.whole, &l_whole);
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!d.pattern.trim().is_empty(), |ui| {
                        if ui.button(&l_search).clicked() {
                            do_search = true;
                        }
                    });
                    if ui.button(&l_close).clicked() {
                        do_close = true;
                    }
                });
            });

        // Apply (self.editor borrowable now).
        let (pattern, path, regex, ignore_case, whole) = {
            let d = self.grep_dialog.as_ref().unwrap();
            (d.pattern.trim().to_string(), d.path.trim().to_string(), d.regex, d.ignore_case, d.whole)
        };
        if do_search && !pattern.is_empty() {
            let path_opt = (!path.is_empty()).then_some(path);
            // `ignore_case` IS the `/I` (case-insensitive) flag the op expects.
            match self.editor.run_grep(&pattern, path_opt, regex, ignore_case, whole) {
                Ok(msg) => self.status = msg,
                Err(e) => self.status = e,
            }
            self.pane_view.follow = true;
            ctx.request_repaint();
        }
        if do_close || esc || !open {
            self.grep_dialog = None;
        }
    }

    /// Render the read-only Document ▸ Properties box (live label/value pairs from `editor.doc_info`).
    fn render_properties(&mut self, ctx: &egui::Context) {
        if !self.properties_open {
            return;
        }
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let info = self.editor.doc_info();
        let mut open = true;
        let mut do_close = false;
        egui::Window::new("Document properties")
            .id(egui::Id::new("doc_properties"))
            .open(&mut open)
            .resizable(true)
            .default_width(420.0)
            .show(ctx, |ui| {
                egui::Grid::new("doc_props").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                    for (label, value) in &info {
                        ui.label(*label);
                        ui.label(value);
                        ui.end_row();
                    }
                });
                ui.separator();
                if ui.button("Close").clicked() {
                    do_close = true;
                }
            });
        if do_close || esc || !open {
            self.properties_open = false;
        }
    }

    /// Localized UI label: the `LANG` translation for string `id`, or the built-in English `en` if
    /// no table is loaded / the id is missing. Menu/dialog labels route through this so a loaded
    /// translation (or the pseudolocale) shows without hard-coding a language. i18n phase-1 spike.
    fn tr(&self, id: &str, en: &str) -> String {
        self.editor.lang_get(id).map(str::to_string).unwrap_or_else(|| en.to_string())
    }

    /// The `<xx>.lang` catalogs to offer under `Settings ▸ Language`, discovered from `$CWD`, the
    /// executable's own directory, and a `lang/` folder beside it. `menu_tree` (hence this) runs
    /// every frame, so the filesystem walk is throttled to a few times a second and cached.
    fn available_languages(&self) -> Vec<LangChoice> {
        const TTL: std::time::Duration = std::time::Duration::from_millis(1500);
        let mut cache = self.lang_menu_cache.borrow_mut();
        if let Some((at, choices)) = cache.as_ref() {
            if at.elapsed() < TTL {
                return choices.clone();
            }
        }
        let mut dirs = vec![std::path::PathBuf::from(self.editor.cwd())];
        if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf())) {
            dirs.push(dir.join("lang"));
            dirs.push(dir);
        }
        let choices = discover_langs(&dirs);
        *cache = Some((std::time::Instant::now(), choices.clone()));
        choices
    }

    /// Build the menu tree from the live editor state — the single source the mouse menu bar and the
    /// keyboard navigator both render. Rebuilt each frame so ✓ marks and generated submenus stay
    /// current. Item commands are the same XPL the old inline menus fired.
    fn menu_tree(&self) -> Vec<MenuNode> {
        let ed = &self.editor;
        let cur_placement = ed.special_str("CMPLACE");
        let hl_on = ed.highlighter_name().is_some();
        let para_on = ed.visible_returns();
        let ws_on = ed.visible_whitespace();
        let lnum_on = ed.line_numbers();
        let shellout_on = ed.shell_output_visible();
        let menubar_on = ed.menu_bar_shown();
        let titlebar_on = ed.show_titlebar();
        let wv_on = ed.writer_view();
        let wingeo_on = ed.remember_window_geometry();
        let ps_on = ed.special_str("PS") == "1";
        let trusted = ed.trusted();
        let hlauto_on = ed.special_str("HLAUTO") == "1";
        let autocfg_on = !ed.special_str("CFA").is_empty();
        let doctype = ed.special_str("DOCTYPE");
        let hex_on = ed.hex_view();
        let mark_here_on = ed.special_str("MARK") == "1"; // caret sits on a bookmark
        let markgutter_on = ed.special_str("MARKGUTTER") != "off"; // on or persist → shown
        let enc = ed.special_str("EN");
        let autosave = ed.special_str("AU");
        let tabw = ed.special_str("TB");
        let decimals = ed.special_str("DECIMALS");
        let popup_on = ed.gui_cmline();
        let preview_on = ed.preview_visible();
        let ro_on = ed.read_only();
        // Coding-aids + language-check toggle states (read as live specials, like `ps_on`/`hlauto_on`).
        let tabindent_on = ed.special_str("TABINDENT") == "1";
        let tabspaces_on = ed.special_str("TABSPACES") == "1";
        let wrapindent_on = ed.special_str("WRAPINDENT") == "1";
        let wrap_on = ed.special_str("WRAP") == "1";
        let wrap_hang = ed.special_str("WRAPHANG"); // current n (string), for the value-entry item
        let balpairs_on = ed.special_str("BALPAIRS") == "1";
        let typepairs_on = ed.special_str("TYPEPAIRS") == "1";
        let spelling_on = ed.special_str("SPELLING") == "1";
        let grammar_on = ed.special_str("GRAMMAR") == "1";
        let dialect = ed.special_str("DIALECT");
        let spell_style = ed.special_str("SPELLSTYLE");
        let grammar_style = ed.special_str("GRAMMARSTYLE");
        let spell_engine = ed.special_str("SPELLENGINE");
        let mail_debug = ed.special_str("MAILDEBUG") == "1";
        // EMPH phrase-entry prefills: seed the "…" items with the current emphasis so re-editing is
        // one step. Staged via `StageCmdline` → `load_cmline`, which takes the EXACT string (the
        // trailing space survives, unlike a `BC … ;CMLINE` chain the command splitter would trim).
        let (cur_emph, cur_emph_rx) = (ed.active_emph(), ed.active_emph_is_regex());
        let emph_lit_stage = match &cur_emph {
            Some(p) if !cur_emph_rx => format!("EMPH {p}"),
            _ => "EMPH ".to_string(),
        };
        let emph_rx_stage = match &cur_emph {
            Some(p) if cur_emph_rx => format!("EMPH/R {p}"),
            _ => "EMPH/R ".to_string(),
        };
        use MenuAct::*;
        let txtmd: Option<(&str, &[&str])> = Some(("Text / Markdown", &["md", "markdown", "txt"]));
        let xpl_f: Option<(&str, &[&str])> = Some(("XPL script", &["xpl"]));
        let cfg_f: Option<(&str, &[&str])> = Some(("XPL config", &["xpl"]));
        let kbd_f: Option<(&str, &[&str])> = Some(("Keyboard", &["kbd"]));
        let pick = |save: bool, filter: Option<(&'static str, &'static [&'static str])>, tmpl: &str| {
            PickRun { save, filter, template: tmpl.to_string(), dir: None }
        };
        // The "Load theme file…" picker opens in $XSDIR\themes (the exe's own themes/ folder) when
        // that folder exists; otherwise the dialog falls back to its default location.
        let themes_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("themes")))
            .filter(|d| d.is_dir());
        vec![
            m_sub(self.tr("menubar.file", "File"), vec![
                m_act(self.tr("menu.file.new", "New"), New),
                m_act(self.tr("menu.file.open", "Open…"), PickOpen),
                m_act(self.tr("menu.file.open_readonly", "Open read-only…"), pick(false, txtmd, "READ {}")),
                m_item(self.tr("menu.file.reopen", "Reopen closed…"), "REOPEN"),
                m_item(self.tr("menu.file.revert", "Revert to saved"), "REC"),
                MenuNode::Sep,
                m_act(self.tr("menu.file.save", "Save"), Save),
                m_act(self.tr("menu.file.save_as", "Save as…"), PickSaveAs),
                m_act(self.tr("menu.file.save_selection_as", "Save selection as…"), pick(true, None, "SAD {}")),
                m_item(self.tr("menu.file.save_all", "Save all"), "SL"),
                MenuNode::Sep,
                m_act(self.tr("menu.file.export_pdf", "Export PDF…"), PickExport),
                MenuNode::Sep,
                m_item(self.tr("menu.file.close", "Close"), "AB"),
                m_item(self.tr("menu.file.quit", "Quit"), "QUIT"),
            ]),
            m_sub(self.tr("menubar.edit", "Edit"), vec![
                m_item(self.tr("menu.edit.undo", "Undo"), "UNDO"),
                m_item(self.tr("menu.edit.redo", "Redo"), "REDO"),
                MenuNode::Sep,
                m_item(self.tr("menu.edit.cut", "Cut"), "CUT"),
                m_item(self.tr("menu.edit.copy", "Copy"), "COPY"),
                m_item(self.tr("menu.edit.append_clipboard", "Append to clipboard"), "APPEND"),
                m_item(self.tr("menu.edit.prepend_clipboard", "Prepend to clipboard"), "PREPEND"),
                m_item(self.tr("menu.edit.paste", "Paste"), "PASTE"),
                MenuNode::Sep,
                m_item(self.tr("menu.edit.select_all", "Select all"), "DFA"),
                m_item(self.tr("menu.edit.delete_selection", "Delete selection"), "DN"),
                MenuNode::Sep,
                m_item(self.tr("menu.edit.copy_to_cursor", "Copy to cursor"), "CP"),
                m_item(self.tr("menu.edit.move_to_cursor", "Move to cursor"), "MV"),
                MenuNode::Sep,
                m_sub(self.tr("menu.edit.case", "Change case"), vec![
                    m_item(self.tr("menu.edit.case.upper", "UPPERCASE"), "UC"),
                    m_item(self.tr("menu.edit.case.lower", "lowercase"), "LC"),
                    m_item(self.tr("menu.edit.case.swap", "Swap case"), "CC"),
                ]),
                MenuNode::Sep,
                // Multi-caret: the argument-free CARET* verbs. Once more than one caret is active every
                // edit acts on them all at once. The verbs collapse to no-ops with a single caret / no
                // selection, so they're safe to pick anytime.
                m_sub(self.tr("menu.edit.multicaret", "Multiple carets"), vec![
                    m_item(self.tr("menu.edit.multicaret.add_above", "Add caret above"), "CARETUP"),
                    m_item(self.tr("menu.edit.multicaret.add_below", "Add caret below"), "CARETDOWN"),
                    MenuNode::Sep,
                    m_item(self.tr("menu.edit.multicaret.next", "Add next occurrence"), "CARETNEXT"),
                    m_item(self.tr("menu.edit.multicaret.skip", "Skip this occurrence"), "CARETSKIP"),
                    m_item(self.tr("menu.edit.multicaret.all", "Select all occurrences"), "CARETALL"),
                    MenuNode::Sep,
                    m_item(self.tr("menu.edit.multicaret.column", "Column from selection"), "CARETCOL"),
                    m_item(self.tr("menu.edit.multicaret.split", "Split selection into lines"), "CARETSPLIT"),
                    MenuNode::Sep,
                    m_item(self.tr("menu.edit.multicaret.pop", "Remove last caret"), "CARETPOP"),
                    m_item(self.tr("menu.edit.multicaret.one", "Collapse to one caret"), "CARETONE"),
                ]),
                MenuNode::Sep,
                // Colour the SELECTION (a transient span) — or the whole editor when nothing is
                // selected. `FGCOLOR default` clears a selected span (selection-only).
                m_item(self.tr("menu.edit.text_colour", "Text colour…"), "FGCOLORS"),
                m_item(self.tr("menu.edit.clear_text_colour", "Clear text colour"), "FGCOLOR default"),
                MenuNode::Sep,
                m_act(self.tr("menu.edit.insert_file", "Insert file…"), pick(false, None, "MERGE {}")),
            ]),
            m_sub(self.tr("menubar.search", "Search"), vec![
                m_act(self.tr("menu.search.find", "Find…"), OpenFind { replace: false }),
                m_act(self.tr("menu.search.replace", "Replace…"), OpenFind { replace: true }),
                MenuNode::Sep,
                m_item(self.tr("menu.search.find_next", "Find next"), "SE"),
                m_item(self.tr("menu.search.find_prev", "Find previous"), "SEB"),
                MenuNode::Sep,
                // GREP — recursive search across files into a results tab (a companion to Find).
                m_act(self.tr("menu.search.grep", "Grep…"), OpenGrep),
                MenuNode::Sep,
                // EMPH — highlight every occurrence of a word/phrase in the current tab. A sibling of
                // Find; the "…" items stage the verb on the command line for you to finish typing.
                m_sub(self.tr("menu.search.emphasize", "Emphasize"), vec![
                    m_item(self.tr("menu.search.emphasize.selection", "Selection"), "EMPH $SE"),
                    m_stage(self.tr("menu.search.emphasize.word", "Word or phrase…"), emph_lit_stage),
                    m_stage(self.tr("menu.search.emphasize.regex", "Regular expression…"), emph_rx_stage),
                    MenuNode::Sep,
                    m_item(self.tr("menu.search.emphasize.clear", "Clear"), "EMPH"),
                ]),
                MenuNode::Sep,
                // Bookmarks — mark positions and jump between them (the navigation sibling of Find
                // next/previous). The checkmark on "Bookmark" reflects whether the caret is on one.
                m_sub(self.tr("menu.search.bookmarks", "Bookmarks"), vec![
                    m_tog(self.tr("menu.search.bookmarks.toggle", "Bookmark"), mark_here_on, "MARKTOGGLE"),
                    MenuNode::Sep,
                    m_item(self.tr("menu.search.bookmarks.next", "Next bookmark"), "MARKNEXT"),
                    m_item(self.tr("menu.search.bookmarks.prev", "Previous bookmark"), "MARKPREV"),
                    m_item(self.tr("menu.search.bookmarks.list", "Go to bookmark…"), "MARKLIST"),
                    MenuNode::Sep,
                    m_item(self.tr("menu.search.bookmarks.clear_all", "Clear all bookmarks"), "MARKCLEARALL"),
                    MenuNode::Sep,
                    m_tog(self.tr("menu.search.bookmarks.gutter", "Gutter marks"), markgutter_on, "MARKGUTTER"),
                ]),
            ]),
            m_sub(self.tr("menubar.view", "View"), vec![
                m_act(format!("{}{}", tick(hl_on), self.tr("menu.view.highlighting", "Highlighting")), Highlight(!hl_on)),
                MenuNode::Sep,
                m_tog(self.tr("menu.view.paragraph_marks", "Paragraph marks"), para_on, "TVR"),
                m_tog(self.tr("menu.view.whitespace", "Whitespace"), ws_on, "TWS"),
                m_tog(self.tr("menu.view.line_numbers", "Line numbers"), lnum_on, "TLN"),
                m_tog(self.tr("menu.view.word_wrap", "Word wrap"), wrap_on, "WRAP"),
                // Not a toggle — a value. Stage `WRAPHANG <current>` on the command line and focus it
                // (via `BC … ;CMLINE`, like the Email "…" items) so you edit the number and press Enter.
                // `{n}` in the translated label is filled with the current width.
                m_item(self.tr("menu.view.wrap_hang", "Wrap hang ({n})…").replace("{n}", &wrap_hang.to_string()), format!("BC WRAPHANG {wrap_hang} ;CMLINE")),
                MenuNode::Sep,
                m_act(format!("{}{}", tick(preview_on), self.tr("menu.view.preview_pane", "Preview pane")), TogglePreview),
                m_item(self.tr("menu.view.console", "Console"), "SH"),
                m_tog(self.tr("menu.view.console_tab", "Show Console tab"), shellout_on, "SW"),
                MenuNode::Sep,
                m_sub(self.tr("menu.view.split", "Split panes"), vec![
                    m_item(self.tr("menu.view.split.side_by_side", "Split side by side"), "TVS"),
                    m_item(self.tr("menu.view.split.top_bottom", "Split top and bottom"), "THS"),
                    MenuNode::Sep,
                    m_item(self.tr("menu.view.split.focus_next", "Focus next pane"), "TW"),
                    m_item(self.tr("menu.view.split.swap", "Swap the two panes"), "TRV"),
                    MenuNode::Sep,
                    m_item(self.tr("menu.view.split.zoom", "Zoom active pane (toggle)"), "TZ"),
                    m_item(self.tr("menu.view.split.fullscreen", "Active pane full screen"), "FS"),
                    m_item(self.tr("menu.view.split.remove", "Remove split"), "TUS"),
                ]),
                MenuNode::Sep,
                m_tog(self.tr("menu.view.menu_bar", "Menu bar"), menubar_on, "TMB"),
                m_tog(self.tr("menu.view.title_bar", "Title bar"), titlebar_on, "TITLEBAR"),
                m_tog(self.tr("menu.view.writers_view", "Writer's View (Esc to exit)"), wv_on, "WV"),
                m_sub(self.tr("menu.view.cmdline", "Command line"), vec![
                    m_tog(self.tr("menu.view.cmdline.popup", "Pop-up"), popup_on, "TGC"),
                    m_sub(self.tr("menu.view.cmdline.position", "Position"), vec![
                        m_tog(self.tr("menu.view.cmdline.position.top", "Top"), cur_placement == "top", "CMPLACE top"),
                        m_tog(self.tr("menu.view.cmdline.position.bottom", "Bottom"), cur_placement == "bottom", "CMPLACE bottom"),
                        m_tog(self.tr("menu.view.cmdline.position.hidden", "Hidden"), cur_placement == "hidden", "CMPLACE hidden"),
                    ]),
                ]),
            ]),
            m_sub(self.tr("menubar.document", "Document"), vec![
                m_sub(self.tr("menu.document.doctype", "Document type"), vec![
                    m_tog(self.tr("menu.document.doctype.typst", "Typst"), doctype == "typst", "DOCTYPE typst"),
                    m_tog(self.tr("menu.document.doctype.gfm", "GFM (Markdown)"), doctype == "gfm", "DOCTYPE gfm"),
                    m_item(self.tr("menu.document.doctype.auto", "Auto (by extension)"), "DOCTYPE auto"),
                ]),
                m_sub(self.tr("menu.document.encoding", "Text encoding"), vec![
                    m_sub(self.tr("menu.document.encoding.display_as", "Display as"), encoding_display_items(&enc)),
                    m_sub(self.tr("menu.document.encoding.convert_to", "Convert to (new file)"), encoding_convert_items()),
                ]),
                m_sub(self.tr("menu.document.line_endings", "Line endings"), vec![
                    m_item(self.tr("menu.document.line_endings.lf", "LF (Unix)"), "NEWLINE lf"),
                    m_item(self.tr("menu.document.line_endings.crlf", "CRLF (Windows)"), "NEWLINE crlf"),
                ]),
                MenuNode::Sep,
                // Hex + text view of the raw bytes (a per-document display mode; bytes unchanged).
                m_tog(self.tr("menu.document.hex_view", "Hex view"), hex_on, if hex_on { "HEX off" } else { "HEX on" }),
                m_tog(self.tr("menu.document.read_only", "Read-only"), ro_on, "RO"),
                MenuNode::Sep,
                m_item(self.tr("menu.document.word_count", "Word count"), "WORDCOUNT"),
                m_item(self.tr("menu.document.char_count", "Character count"), "CHARCOUNT"),
                m_act(self.tr("menu.document.properties", "Properties…"), ShowProperties),
            ]),
            m_sub(self.tr("menubar.tools", "Tools"), vec![
                m_act(self.tr("menu.tools.run_script", "Run script…"), pick(false, xpl_f, "RUN {}")),
                m_act(self.tr("menu.tools.run_script_bg", "Run script in background…"), pick(false, xpl_f, "RUN/BG {}")),
                m_item(self.tr("menu.tools.run_here", "Run code under cursor"), "RUNHERE"),
                MenuNode::Sep,
                m_act(self.tr("menu.tools.shell_command", "Shell command…"), OpenRunCmd),
                m_item(self.tr("menu.tools.jobs", "Background jobs"), "JOBS"),
                MenuNode::Sep,
                m_sub(self.tr("menu.tools.lang", "Language check"), vec![
                    m_item(self.tr("menu.tools.lang.check", "Check document"), "CHECK"),
                    MenuNode::Sep,
                    m_tog(self.tr("menu.tools.lang.spelling", "Spelling underlines"), spelling_on, if spelling_on { "SPELLING off" } else { "SPELLING on" }),
                    m_tog(self.tr("menu.tools.lang.grammar", "Grammar underlines"), grammar_on, if grammar_on { "GRAMMAR off" } else { "GRAMMAR on" }),
                    MenuNode::Sep,
                    m_item(self.tr("menu.tools.lang.spell_next", "Next spelling issue"), "SPELLNEXT"),
                    m_item(self.tr("menu.tools.lang.spell_prev", "Previous spelling issue"), "SPELLPREV"),
                    m_item(self.tr("menu.tools.lang.grammar_next", "Next grammar issue"), "GRAMMARNEXT"),
                    m_item(self.tr("menu.tools.lang.grammar_prev", "Previous grammar issue"), "GRAMMARPREV"),
                    m_item(self.tr("menu.tools.lang.fix", "Fix issue at cursor…"), "LINTFIX"),
                    m_item(self.tr("menu.tools.lang.learn", "Learn selected words"), "LEARN/S"),
                    MenuNode::Sep,
                    m_sub(self.tr("menu.tools.lang.dialect", "Dialect"), vec![
                        m_tog(self.tr("menu.tools.lang.dialect.american", "American"), dialect == "american", "DIALECT american"),
                        m_tog(self.tr("menu.tools.lang.dialect.british", "British"), dialect == "british", "DIALECT british"),
                        m_tog(self.tr("menu.tools.lang.dialect.canadian", "Canadian"), dialect == "canadian", "DIALECT canadian"),
                        m_tog(self.tr("menu.tools.lang.dialect.australian", "Australian"), dialect == "australian", "DIALECT australian"),
                        m_tog(self.tr("menu.tools.lang.dialect.indian", "Indian"), dialect == "indian", "DIALECT indian"),
                    ]),
                    m_sub(self.tr("menu.tools.lang.spell_style", "Spelling style"), style_choices("SPELLSTYLE", &spell_style)),
                    m_sub(self.tr("menu.tools.lang.grammar_style", "Grammar style"), style_choices("GRAMMARSTYLE", &grammar_style)),
                    // Spelling engine — Harper (English grammar+spelling) or the Hunspell CLI
                    // (multilingual spelling). Engine names are literal (not translated); dictionary /
                    // binary stay verb-only (SPELLDICT/SPELLBIN — they need free-text input).
                    m_sub(self.tr("menu.tools.lang.engine", "Spelling engine"), vec![
                        m_tog("Harper", spell_engine == "harper", "SPELLENGINE harper"),
                        m_tog("Hunspell", spell_engine == "hunspell", "SPELLENGINE hunspell"),
                    ]),
                ]),
                MenuNode::Sep,
                // Email: send/receive via curl. The "…" items prefill the command line (via `BC`) so
                // you can add a recipient / message range, then Enter. `MAILUSER`/`MAILPASS`/`SMTPURL`
                // (send) and `IMAPURL` (receive) must be configured first — see the reference.
                m_sub(self.tr("menu.tools.email", "Email"), vec![
                    m_item(self.tr("menu.tools.email.list", "List mailbox"), "LISTMAIL"),
                    // The "…" items stage a command on the command line (via `BC`) and focus it, so you
                    // finish the range / recipient and press Enter. `SAVEMAIL all` is prefilled whole so
                    // its space survives (a trailing space would be trimmed); edit `all` to a range.
                    m_item(self.tr("menu.tools.email.save", "Save messages…"), "BC SAVEMAIL all ;CMLINE"),
                    m_item(self.tr("menu.tools.email.delete", "Delete messages…"), "BC DELMAIL ;CMLINE"),
                    MenuNode::Sep,
                    m_item(self.tr("menu.tools.email.send", "Send…"), "BC SENDMAIL ;CMLINE"),
                    m_act(self.tr("menu.tools.email.attach", "Attach file…"), pick(false, None, "ATTACH {}")),
                    m_item(self.tr("menu.tools.email.clear_attach", "Clear attachments"), "CLEARATTACH"),
                    MenuNode::Sep,
                    m_tog(self.tr("menu.tools.email.verbose", "Verbose logging"), mail_debug, if mail_debug { "MAILDEBUG off" } else { "MAILDEBUG on" }),
                ]),
                MenuNode::Sep,
                m_sub(self.tr("menu.tools.library", "Library"), vec![
                    m_act(self.tr("menu.tools.library.load", "Load…"), pick(false, Some(("Library", &["lib"])), "LOADLIB {}")),
                    m_item(self.tr("menu.tools.library.reload", "Reload"), "RELOADLIB"),
                    m_item(self.tr("menu.tools.library.unload", "Unload all"), "UNLOADLIB"),
                    m_item(self.tr("menu.tools.library.list", "List routines"), "LISTLIB"),
                ]),
                MenuNode::Sep,
                // Opens the keymap's chart (all modifier tables) in a scrollable TXT modal.
                m_item(self.tr("menu.tools.keyboard_chart", "Keyboard chart"), "KBCHART"),
            ]),
            m_sub(self.tr("menubar.settings", "Settings"), vec![
                m_sub(self.tr("menu.settings.appearance", "Appearance"), vec![
                    // `THEME` — a coordinated colour scheme: the two built-ins, or any theme file
                    // (a SAVECFG-format colour preset) chosen from disk and run to activate it.
                    m_sub(self.tr("menu.settings.appearance.theme", "Theme"), vec![
                        m_item(self.tr("menu.settings.appearance.theme.light", "Light"), "THEME light"),
                        m_item(self.tr("menu.settings.appearance.theme.dark", "Dark"), "THEME dark"),
                        MenuNode::Sep,
                        m_act(
                            self.tr("menu.settings.appearance.theme.file", "Load theme file…"),
                            PickRun { save: false, filter: cfg_f, template: "THEME {}".to_string(), dir: themes_dir.clone() },
                        ),
                    ]),
                    m_sub(self.tr("menu.settings.appearance.colors", "Colors"), vec![
                        m_item(self.tr("menu.settings.appearance.colors.editor_text", "Editor text…"), "FGCOLORS"),
                        m_item(self.tr("menu.settings.appearance.colors.editor_bg", "Editor background…"), "BGCOLORS"),
                        m_item(self.tr("menu.settings.appearance.colors.selection_text", "Selection text…"), "SELFGCOLORS"),
                        m_item(self.tr("menu.settings.appearance.colors.selection_bg", "Selection background…"), "SELBGCOLORS"),
                        // `MDA` block styling: PERSISTENT per-selection colour + text attributes (saved
                        // with the file via the `.xymeta` sidecar), distinct from the global selection
                        // colours above. Colours open a picker; attributes apply straight away.
                        m_sub(self.tr("menu.settings.appearance.colors.block", "Block style (selection)"), vec![
                            m_item(self.tr("menu.settings.appearance.colors.block.fg", "Text colour…"), "MDAS"),
                            m_item(self.tr("menu.settings.appearance.colors.block.bg", "Background colour…"), "MDAS/B"),
                            MenuNode::Sep,
                            m_item(self.tr("menu.settings.appearance.colors.block.bold", "Bold"), "MDA BO"),
                            m_item(self.tr("menu.settings.appearance.colors.block.italic", "Italic"), "MDA IT"),
                            m_item(self.tr("menu.settings.appearance.colors.block.bolditalic", "Bold italic"), "MDA BI"),
                            m_item(self.tr("menu.settings.appearance.colors.block.underline", "Underline"), "MDA UL"),
                            m_item(self.tr("menu.settings.appearance.colors.block.boldunderline", "Bold underline"), "MDA BU"),
                            m_item(self.tr("menu.settings.appearance.colors.block.reverse", "Reverse video"), "MDA RV"),
                            m_item(self.tr("menu.settings.appearance.colors.block.normal", "Normal (clear attributes)"), "MDA NM"),
                            MenuNode::Sep,
                            m_item(self.tr("menu.settings.appearance.colors.block.clear_fg", "Clear text colour"), "MDA off"),
                            m_item(self.tr("menu.settings.appearance.colors.block.clear_bg", "Clear background colour"), "MDA/B off"),
                        ]),
                        m_item(self.tr("menu.settings.appearance.colors.caret", "Caret…"), "CRCOLORS"),
                        m_item(self.tr("menu.settings.appearance.colors.separators", "Separators…"), "SEPCOLORS"),
                        m_sub(self.tr("menu.settings.appearance.colors.tab_bg", "Tab bar background"), vec![
                            m_item(self.tr("menu.settings.appearance.colors.tab_bg.active", "Active tab…"), "TABBGCOLORS active"),
                            m_item(self.tr("menu.settings.appearance.colors.tab_bg.hover", "Hovered tab…"), "TABBGCOLORS hover"),
                            m_item(self.tr("menu.settings.appearance.colors.tab_bg.inactive", "Inactive tab…"), "TABBGCOLORS inactive"),
                        ]),
                        m_item(self.tr("menu.settings.appearance.colors.tab_close", "Tab close ×…"), "TABXCOLORS"),
                        m_item(self.tr("menu.settings.appearance.colors.ui_text", "UI text…"), "UIFGCOLORS"),
                        m_item(self.tr("menu.settings.appearance.colors.ui_bg", "UI background…"), "UIBGCOLORS"),
                        m_item(self.tr("menu.settings.appearance.colors.txt_text", "TXT window text…"), "TXTFGCOLORS"),
                        m_item(self.tr("menu.settings.appearance.colors.txt_bg", "TXT window background…"), "TXTBGCOLORS"),
                        m_item(self.tr("menu.settings.appearance.colors.dir_text", "Directory window text…"), "DWFGCOLORS"),
                        m_item(self.tr("menu.settings.appearance.colors.dir_bg", "Directory window background…"), "DWBGCOLORS"),
                    ]),
                    m_sub(self.tr("menu.settings.appearance.fonts", "Fonts"), vec![
                        m_item(self.tr("menu.settings.appearance.fonts.editor", "Editor…"), "EDFONTS"),
                        m_item(self.tr("menu.settings.appearance.fonts.ui", "UI…"), "UIFONTS"),
                        m_item(self.tr("menu.settings.appearance.fonts.txt", "TXT window…"), "TXTFONTS"),
                        m_item(self.tr("menu.settings.appearance.fonts.dir", "Directory window…"), "DWFONTS"),
                        MenuNode::Sep,
                        m_item(self.tr("menu.settings.appearance.fonts.preview_body", "Preview / PDF body…"), "PVFONTS"),
                        m_item(self.tr("menu.settings.appearance.fonts.preview_code", "Preview / PDF code…"), "PVCODES"),
                    ]),
                    m_sub(self.tr("menu.settings.appearance.ui_zoom", "UI zoom"), vec![
                        m_item(self.tr("menu.settings.appearance.ui_zoom.in", "Zoom in"), "UISCALE +0.1"),
                        m_item(self.tr("menu.settings.appearance.ui_zoom.out", "Zoom out"), "UISCALE -0.1"),
                        m_item(self.tr("menu.settings.appearance.ui_zoom.reset", "Reset (100%)"), "UISCALE 1"),
                    ]),
                    m_sub(self.tr("menu.settings.appearance.margins", "Margins"), vec![
                        m_item(self.tr("menu.settings.appearance.margins.wider", "Wider"), "OF +2"),
                        m_item(self.tr("menu.settings.appearance.margins.narrower", "Narrower"), "OF -2"),
                        m_item(self.tr("menu.settings.appearance.margins.none", "None"), "OF 0"),
                    ]),
                    m_sub(self.tr("menu.settings.appearance.line_spacing", "Line spacing"), vec![
                        // Leading is a PERCENT (default 145, 50–400), so step by 10 for a visible
                        // change — ±1 was a 1% nudge you couldn't see.
                        m_item(self.tr("menu.settings.appearance.line_spacing.more", "More"), "LEADING +10"),
                        m_item(self.tr("menu.settings.appearance.line_spacing.less", "Less"), "LEADING -10"),
                    ]),
                    MenuNode::Sep,
                    m_item(self.tr("menu.settings.appearance.reset_ui", "Reset UI sizing"), "UIRESET"),
                    m_item(self.tr("menu.settings.appearance.reset_dw", "Reset directory window"), "DWRESET"),
                ]),
                m_sub(self.tr("menu.settings.syntax", "Syntax highlighting"), vec![
                    // One colour-picker entry per themeable token kind — `HLCOLORS <kind>` (the bare
                    // verb needs a kind, so a single item did nothing). Built from the live kind list.
                    // The per-kind item labels are the (generated) token-kind names — not translated.
                    m_sub(
                        self.tr("menu.settings.syntax.theme_colors", "Theme colors"),
                        ed.highlight_kinds()
                            .into_iter()
                            .map(|k| m_item(format!("{}…", cap_first(&k)), format!("HLCOLORS {k}")))
                            .collect(),
                    ),
                    m_item(self.tr("menu.settings.syntax.token_kinds", "Token kinds…"), "HLKINDS"),
                    m_item(self.tr("menu.settings.syntax.reset_theme", "Reset theme"), "HLRESET"),
                    m_tog(self.tr("menu.settings.syntax.auto", "Auto-highlight on open"), hlauto_on, if hlauto_on { "HLAUTO off" } else { "HLAUTO on" }),
                ]),
                m_sub(self.tr("menu.settings.editor", "Editor behavior"), vec![
                    m_tog(self.tr("menu.settings.editor.persistent_sel", "Persistent selection"), ps_on, "TPS"),
                    m_tog(self.tr("menu.settings.editor.remember_geom", "Remember window size+position"), wingeo_on, "WINGEO"),
                    m_sub(self.tr("menu.settings.editor.coding", "Coding aids"), vec![
                        m_tog(self.tr("menu.settings.editor.coding.autoindent", "Auto-indent new lines"), tabindent_on, if tabindent_on { "TABINDENT off" } else { "TABINDENT on" }),
                        m_tog(self.tr("menu.settings.editor.coding.soft_tabs", "Soft tabs (insert spaces)"), tabspaces_on, if tabspaces_on { "TABSPACES off" } else { "TABSPACES on" }),
                        m_tog(self.tr("menu.settings.editor.coding.hang_indent", "Hanging indent for wrapped lines"), wrapindent_on, if wrapindent_on { "WRAPINDENT off" } else { "WRAPINDENT on" }),
                        m_tog(self.tr("menu.settings.editor.coding.match_brackets", "Highlight matching brackets"), balpairs_on, if balpairs_on { "BALPAIRS off" } else { "BALPAIRS on" }),
                        m_tog(self.tr("menu.settings.editor.coding.close_brackets", "Auto-close brackets and quotes"), typepairs_on, if typepairs_on { "TYPEPAIRS off" } else { "TYPEPAIRS on" }),
                    ]),
                    m_sub(self.tr("menu.settings.editor.autosave", "Autosave"), vec![
                        m_tog(self.tr("menu.settings.editor.autosave.off", "Off"), autosave == "0", "AUTOSAVE off"),
                        m_tog(self.tr("menu.settings.editor.autosave.30s", "Every 30 s"), autosave == "30", "AUTOSAVE 30"),
                        m_tog(self.tr("menu.settings.editor.autosave.1m", "Every 1 min"), autosave == "60", "AUTOSAVE 60"),
                        m_tog(self.tr("menu.settings.editor.autosave.2m", "Every 2 min"), autosave == "120", "AUTOSAVE 120"),
                        m_tog(self.tr("menu.settings.editor.autosave.5m", "Every 5 min"), autosave == "300", "AUTOSAVE 300"),
                    ]),
                    // Tab-width items are bare numbers (2/4/8) — not translated.
                    m_sub(self.tr("menu.settings.editor.tab_width", "Tab width"), vec![
                        m_tog("2", tabw == "2", "TB 2"),
                        m_tog("4", tabw == "4", "TB 4"),
                        m_tog("8", tabw == "8", "TB 8"),
                    ]),
                    m_sub(self.tr("menu.settings.editor.precision", "Number precision"), vec![
                        m_tog(self.tr("menu.settings.editor.precision.off", "Off (full)"), decimals == "off", "DECIMALS off"),
                        m_tog("0", decimals == "0", "DECIMALS 0"),
                        m_tog("1", decimals == "1", "DECIMALS 1"),
                        m_tog("2", decimals == "2", "DECIMALS 2"),
                        m_tog("3", decimals == "3", "DECIMALS 3"),
                        m_tog("4", decimals == "4", "DECIMALS 4"),
                    ]),
                ]),
                m_sub(self.tr("menu.settings.keyboard", "Keyboard"), vec![
                    m_act(self.tr("menu.settings.keyboard.load", "Load…"), pick(false, kbd_f, "LOADKBD {}")),
                    m_act(self.tr("menu.settings.keyboard.save", "Save…"), pick(true, kbd_f, "SAVEKBD {}")),
                    m_item(self.tr("menu.settings.keyboard.reset", "Reset to built-in"), "CLEARKBD"),
                ]),
                // UI language — discovered `<xx>.lang` catalogs plus a built-in English entry. Item
                // labels are endonyms (each language in its own name), so they are NOT translated;
                // only the submenu title is. The active catalog is ticked via `Editor::lang_path`.
                m_sub(self.tr("menu.settings.language", "Language"), {
                    let active_canon = self
                        .editor
                        .lang_path()
                        .and_then(|p| std::fs::canonicalize(p).ok())
                        .map(|p| p.to_string_lossy().into_owned());
                    let mut items = vec![m_tog("English", active_canon.is_none(), "LANG off")];
                    for c in self.available_languages() {
                        let on = active_canon.as_deref() == Some(c.canon.as_str());
                        items.push(m_tog(c.name, on, format!("LANG {}", c.cmd_path)));
                    }
                    items
                }),
                m_tog(self.tr("menu.settings.allow_shell", "Allow shell commands"), trusted, if trusted { "TRUST 0" } else { "TRUST 1" }),
                MenuNode::Sep,
                m_act(self.tr("menu.settings.save_settings", "Save settings…"), pick(true, cfg_f, "SAVECFG {}")),
                m_act(self.tr("menu.settings.load_settings", "Load settings…"), pick(false, cfg_f, "LOADCFG {}")),
                m_tog(self.tr("menu.settings.autosave_settings", "Auto-save settings"), autocfg_on, if autocfg_on { "AUTOCFG off" } else { "AUTOCFG" }),
            ]),
            m_sub(self.tr("menubar.help", "Help"), vec![
                m_act(self.tr("menu.help.manual", "Manual"), ShowManual),
                m_act(self.tr("menu.help.reference", "Reference"), ShowReference),
                MenuNode::Sep,
                m_act(self.tr("menu.help.about", "About XyScript"), ShowAbout),
            ]),
        ]
    }

    /// Turn a chosen [`MenuAct`] into the GUI's [`Act`] (running any needed file dialog first).
    /// Shared by the mouse menus and the keyboard navigator.
    fn perform_menu_act(&mut self, a: MenuAct) -> Option<Act> {
        match a {
            MenuAct::Run(cmd) => Some(Act::Run(cmd)),
            MenuAct::StageCmdline(text) => Some(Act::StageCmdline(text)),
            MenuAct::New => Some(Act::New),
            // File dialogs are DEFERRED (never run inline mid-frame): record the request; the runtime
            // runs it parented + modal after the frame and applies the result. See [`PendingDialog`].
            MenuAct::Save => {
                if self.editor.active_path().is_some() {
                    Some(Act::Save)
                } else {
                    self.pending_dialog = Some(PendingDialog::SaveAs);
                    None
                }
            }
            MenuAct::PickOpen => {
                self.pending_dialog = Some(PendingDialog::Open { hex: false });
                None
            }
            MenuAct::PickSaveAs => {
                self.pending_dialog = Some(PendingDialog::SaveAs);
                None
            }
            MenuAct::PickExport => {
                self.pending_dialog = Some(PendingDialog::Export);
                None
            }
            MenuAct::PickRun { save, filter, template, dir } => {
                let filter = filter.map(|(label, exts)| {
                    (label.to_string(), exts.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                });
                self.pending_dialog = Some(PendingDialog::Run { save, filter, template, dir });
                None
            }
            MenuAct::Highlight(b) => Some(Act::Highlight(b)),
            MenuAct::TogglePreview => Some(Act::TogglePreview),
            MenuAct::OpenFind { replace } => Some(Act::OpenFind { replace }),
            MenuAct::OpenRunCmd => Some(Act::OpenRunCmd),
            MenuAct::OpenGrep => Some(Act::OpenGrep),
            MenuAct::ShowProperties => Some(Act::ShowProperties),
            MenuAct::ShowAbout => Some(Act::ShowAbout),
            MenuAct::ShowReference => Some(Act::ShowReference),
            MenuAct::ShowManual => Some(Act::ShowManual),
        }
    }

    /// The native file dialog requested this frame, if any. The runtime drains this AFTER the egui
    /// frame, runs it parented + modal, and hands the result back to [`apply_dialog_result`].
    fn take_pending_dialog(&mut self) -> Option<PendingDialog> {
        self.pending_dialog.take()
    }

    /// Whether the active document renders as Typst (vs GFM) — the runtime uses this to default the
    /// Save-As format to the active document's flavor.
    fn active_is_typst(&self) -> bool {
        self.editor.active_render_is_typst()
    }

    /// Apply the outcome of a deferred file dialog (`picked` = the chosen path, or `None` if the
    /// user cancelled). Runs OUTSIDE the egui frame (from the runtime), so it must not touch egui.
    fn apply_dialog_result(&mut self, req: PendingDialog, picked: Option<std::path::PathBuf>) {
        // A Save-As launched by the quit sweep: a cancel aborts the quit (a successful save marks
        // the doc saved, so the sweep advances on the next frame).
        let for_quit = std::mem::take(&mut self.save_as_for_quit);
        let Some(p) = picked else {
            if for_quit {
                self.quitting = false;
            }
            return; // cancelled — nothing else to do
        };
        match req {
            PendingDialog::Open { hex } => {
                let r = self.editor.open_hex(&p.to_string_lossy(), hex);
                self.reset_view();
                self.note(r);
            }
            PendingDialog::SaveAs => {
                let r = self.editor.save_as(&p.to_string_lossy());
                self.note(r);
                self.pane_view.follow = true;
            }
            PendingDialog::Export => {
                let r = self.editor.export_pdf(&p.to_string_lossy());
                self.note(r);
            }
            PendingDialog::Run { template, .. } => {
                self.editor.run_command(&template.replace("{}", &p.display().to_string()));
                self.pane_view.follow = true;
            }
        }
    }

    /// Whether keyboard menu navigation is active (the runtime diverts nav keys to it).
    fn menu_active(&self) -> bool {
        self.menu_nav.is_some()
    }

    /// Toggle keyboard menu navigation (the `MENU` verb): open the first menu, or close if open.
    fn toggle_menu_kbd(&mut self) {
        self.menu_nav = if self.menu_nav.is_some() {
            None
        } else {
            Some(MenuNav::open(&self.menu_tree(), 0))
        };
    }

    /// Feed a canonical key code to the active keyboard menu. Records a chosen action (run in the
    /// next `update`) and closes the menu on activate / Esc-at-top.
    fn menu_key(&mut self, code: u8) {
        if self.menu_nav.is_none() {
            return;
        }
        let tree = self.menu_tree();
        let result = {
            let nav = self.menu_nav.as_mut().unwrap();
            match code {
                100 | 72 => {
                    nav.move_sel(&tree, -1);
                    NavResult::None
                }
                102 | 80 => {
                    nav.move_sel(&tree, 1);
                    NavResult::None
                }
                101 | 75 => nav.go_left(&tree),
                103 | 77 => nav.go_right(&tree),
                28 | 104 => nav.activate(&tree),
                1 => nav.escape(),
                _ => NavResult::None, // modal: swallow other keys
            }
        };
        self.apply_nav_result(result);
    }

    /// Apply a [`NavResult`] to `menu_nav`: an activation queues the action for the next frame and
    /// closes the menu; a close clears it; `None` leaves it open. Shared by `menu_key` (nav keys)
    /// and `menu_char` (accelerator letters).
    fn apply_nav_result(&mut self, result: NavResult) {
        match result {
            NavResult::Activate(ma) => {
                self.pending_menu_act = Some(ma);
                self.menu_nav = None;
            }
            NavResult::Close => self.menu_nav = None,
            NavResult::None => {}
        }
    }

    /// Feed a printable letter to the active menu as an ACCELERATOR: jump to the item in the
    /// current (deepest) level whose underlined letter is `ch` and activate it — a submenu opens,
    /// a command runs (as if its row were chosen). No match → no-op. Case-insensitive.
    fn menu_char(&mut self, ch: char) {
        let Some(nav) = self.menu_nav.clone() else { return };
        let tree = self.menu_tree();
        let Some(level) = nav.level(&tree) else { return };
        let accels = level_accels(level);
        let Some(idx) = accel_match(level, &accels, ch) else { return };
        let mut nav = nav;
        if let Some(last) = nav.path.last_mut() {
            *last = idx;
        }
        let result = nav.activate(&tree);
        self.menu_nav = Some(nav);
        self.apply_nav_result(result);
    }

    /// Open the top menu whose accelerator letter matches `letter` (the `MENU "f"` verb form).
    /// Switches even if another menu is already open; an unknown letter is a no-op.
    fn open_menu_accel(&mut self, letter: &str) {
        let tree = self.menu_tree();
        let accels = level_accels(&tree);
        if let Some(ch) = letter.chars().next() {
            if let Some(idx) = accel_match(&tree, &accels, ch) {
                self.menu_nav = Some(MenuNav::open(&tree, idx));
            }
        }
    }

    /// Render the unified menu system in one place: the mouse-clickable top bar PLUS the cascading
    /// dropdown columns, all driven by the single `menu_nav` state and responding to BOTH mouse and
    /// keyboard. Replaces egui's built-in `MenuBar` (old mouse-only path) and the floating keyboard
    /// overlay. Returns an action the mouse chose this frame; keyboard activations still arrive via
    /// `pending_menu_act`. The bar draws only when `$MB` shows it, but the dropdowns draw whenever a
    /// menu is open — so `MENU` / an accelerator works even with the bar hidden.
    fn render_menus(&mut self, ui: &mut egui::Ui) -> Option<MenuAct> {
        let ctx = ui.ctx().clone();
        let tree = self.menu_tree();
        let bar_shown = self.editor.menu_bar_shown();
        let mut chosen: Option<MenuAct> = None;

        // --- Top bar: one clickable label per top menu; the open one stays highlighted. ---
        let open_top = self.menu_nav.as_ref().and_then(|n| n.path.first().copied());
        let mut top_rects: Vec<egui::Rect> = vec![egui::Rect::NOTHING; tree.len()];
        let mut bar_rect = egui::Rect::NOTHING;
        let mut top_click: Option<usize> = None;
        let mut top_hover: Option<usize> = None;
        let top_accels = level_accels(&tree);
        if bar_shown {
            egui::Panel::top("menubar").show_inside(ui, |ui| {
                bar_rect = ui.max_rect();
                // WRAPPED, not a plain row: when the titles don't fit the window width (a narrow
                // window, or a localized build whose translated titles run long) the bar flows onto a
                // second row instead of clipping — a clipped title is an unreachable menu. It stays a
                // single row whenever everything fits (the English / wide-window common case).
                ui.horizontal_wrapped(|ui| {
                    for (i, top) in tree.iter().enumerate() {
                        if let MenuNode::Sub { label, .. } = top {
                            let text = accel_label(ui, label, top_accels[i]);
                            let resp = ui.selectable_label(open_top == Some(i), text);
                            top_rects[i] = resp.rect;
                            if resp.clicked() {
                                top_click = Some(i);
                            }
                            if resp.hovered() {
                                top_hover = Some(i);
                            }
                        }
                    }
                });
            });
        }
        // Apply top-bar intent: a click toggles that menu; while a menu is already open, hovering a
        // different top switches to it (the standard menu-bar follow).
        if let Some(i) = top_click {
            self.menu_nav = if open_top == Some(i) { None } else { Some(MenuNav::open(&tree, i)) };
        } else if self.menu_nav.is_some() {
            if let Some(i) = top_hover {
                if open_top != Some(i) {
                    self.menu_nav = Some(MenuNav::open(&tree, i));
                }
            }
        }

        // --- Cascading dropdown columns (only while a menu is open). ---
        if let Some(nav) = self.menu_nav.clone() {
            let path = nav.path.clone();
            // Gather the list shown in each open column: column 0 = the top menu's children; each
            // deeper column = the highlighted submenu's children (per `path`).
            let mut lists: Vec<&[MenuNode]> = Vec::new();
            match tree.get(path[0]).and_then(|t| t.children()) {
                Some(top_children) => {
                    lists.push(top_children);
                    let n_cols = path.len().saturating_sub(1);
                    for c in 1..n_cols {
                        match lists[c - 1].get(path[c]).and_then(|nd| nd.children()) {
                            Some(sub) => lists.push(sub),
                            None => break,
                        }
                    }
                }
                None => {
                    self.menu_nav = None; // tree shape changed under us — bail safely
                    return chosen;
                }
            }

            // Column 0 hangs under its top button (or a fixed spot when the bar is hidden); each
            // deeper column hangs to the right of its parent's highlighted item. `place_menu_col`
            // flips a column that would run off the right edge, and shifts a tall one up off the
            // bottom, using each column's size cached from the previous frame.
            let screen = ctx.content_rect();
            let mut anchor = if bar_shown
                && path[0] < top_rects.len()
                && top_rects[path[0]] != egui::Rect::NOTHING
            {
                top_rects[path[0]]
            } else {
                // Bar hidden: a zero-width anchor whose left_bottom is the old fixed (6, 30) spot.
                egui::Rect::from_min_max(egui::pos2(6.0, 18.0), egui::pos2(6.0, 30.0))
            };
            let mut menu_rects: Vec<egui::Rect> = Vec::new();
            let mut hover_path: Option<Vec<usize>> = None;
            for (c, list) in lists.iter().enumerate() {
                let sel = path[c + 1];
                let accels = level_accels(list);
                let size = self.menu_col_sizes.get(c).copied().unwrap_or(egui::vec2(180.0, 0.0));
                let pos = place_menu_col(anchor, size, screen, c > 0);
                let mut hl_rect: Option<egui::Rect> = None;
                let area = egui::Area::new(egui::Id::new(("menu_col", c)))
                    .order(menu_col_order(c)) // deeper column = higher layer; see `menu_col_order`
                    .fixed_pos(pos)
                    .show(&ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.set_min_width(180.0);
                            // Never wrap a menu item: the column widens to fit the longest label
                            // instead (a wrapped item reads as two entries). The label-length test
                            // `menu_labels_stay_within_width` keeps that widening in check.
                            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                            for (i, node) in list.iter().enumerate() {
                                match node {
                                    MenuNode::Sep => {
                                        ui.separator();
                                    }
                                    MenuNode::Item { label, act } => {
                                        let text = accel_label(ui, label, accels[i]);
                                        let resp = ui.selectable_label(i == sel, text);
                                        if i == sel {
                                            hl_rect = Some(resp.rect);
                                        }
                                        if resp.hovered() {
                                            hover_path =
                                                Some(path[..=c].iter().copied().chain([i]).collect());
                                        }
                                        if resp.clicked() {
                                            chosen = Some(act.clone());
                                        }
                                    }
                                    MenuNode::Sub { label, children } => {
                                        // U+23F5 — the SAME glyph egui uses for its own submenu
                                        // icon, so it is guaranteed to exist in egui's bundled
                                        // fonts. U+25B8 (`▸`) does NOT, and rendered as an empty
                                        // tofu box. Menu glyphs must be in the bundled fonts —
                                        // see `m_tog` for the same rule forcing ✔ over ✓.
                                        let disp = format!("{label}    ⏵");
                                        let text = accel_label(ui, &disp, accels[i]);
                                        let resp = ui.selectable_label(i == sel, text);
                                        if i == sel {
                                            hl_rect = Some(resp.rect);
                                        }
                                        // Hover (or click) a submenu → open its cascade.
                                        if resp.hovered() || resp.clicked() {
                                            let mut p: Vec<usize> =
                                                path[..=c].iter().copied().chain([i]).collect();
                                            p.push(first_selectable(children));
                                            hover_path = Some(p);
                                        }
                                    }
                                }
                            }
                        });
                    });
                // Force cascade z-order: each deeper column must paint ON TOP of its parent. Columns
                // are all `Order::Foreground`, and a submenu that flips left (no room to the right —
                // e.g. under the right-most Help menu) OVERLAPS its parent; without this the child can
                // render behind the parent and be invisible. Moving each to top in `c` order leaves
                // the deepest one on top.
                ctx.move_to_top(area.response.layer_id);
                menu_rects.push(area.response.rect);
                // Cache this column's measured size (by depth) for next frame's placement, and
                // anchor the next cascade on the highlighted item (falling back to the column).
                let measured = area.response.rect.size();
                if c < self.menu_col_sizes.len() {
                    self.menu_col_sizes[c] = measured;
                } else {
                    self.menu_col_sizes.push(measured);
                }
                // Anchor the NEXT cascade on the highlighted item's VERTICAL position but this whole
                // column's HORIZONTAL extent, so a submenu opens cleanly beside the column — and a
                // left-flip (under a right-side menu like Help) lands to the LEFT of the column, not at
                // the item's inner padded edge (which overlapped the parent).
                anchor = match hl_rect {
                    Some(hl) => egui::Rect::from_min_max(
                        egui::pos2(area.response.rect.left(), hl.top()),
                        egui::pos2(area.response.rect.right(), hl.bottom()),
                    ),
                    None => area.response.rect,
                };
            }
            self.menu_col_sizes.truncate(lists.len());

            // Mouse moved onto an item → take over the highlight / open a cascade. Gate on actual
            // motion so a RESTING mouse doesn't fight keyboard navigation (which writes the same
            // `path` every frame).
            let pointer_moved = ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
            if pointer_moved {
                if let (Some(hp), Some(n)) = (hover_path, self.menu_nav.as_mut()) {
                    n.path = hp;
                }
            }
            // A press outside the bar and every column closes the menu (click-away).
            let closed_out = ctx.input(|i| {
                i.pointer.any_pressed()
                    && i.pointer.interact_pos().map_or(false, |p| {
                        !bar_rect.contains(p) && !menu_rects.iter().any(|r| r.contains(p))
                    })
            });
            if closed_out {
                self.menu_nav = None;
            }
        }

        // Activating an item (mouse) closes the menu.
        if chosen.is_some() {
            self.menu_nav = None;
        }
        chosen
    }

    /// Render the Help ▸ About box (name/version/OS).
    fn render_about(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let info = self.editor.about_info();
        let l_title = self.tr("dialog.about.title", "About XyScript");
        let l_tagline = self.tr("dialog.about.tagline", "A configurable editor in the XyWrite tradition.");
        let l_close = self.tr("button.close", "Close");
        let mut open = true;
        let mut do_close = false;
        egui::Window::new(l_title)
            .id(egui::Id::new("about"))
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                // "XyScript" is the product name — never translated.
                ui.label(egui::RichText::new("XyScript").strong().size(16.0));
                // Don't let the tag line wrap — the window auto-sizes wide enough to fit it.
                ui.add(egui::Label::new(&l_tagline).wrap_mode(egui::TextWrapMode::Extend));
                ui.add_space(6.0);
                egui::Grid::new("about_grid").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                    for (label, value) in &info {
                        ui.label(*label);
                        ui.label(value);
                        ui.end_row();
                    }
                });
                ui.separator();
                ui.vertical_centered(|ui| {
                    if ui.button(&l_close).clicked() {
                        do_close = true;
                    }
                });
            });
        if do_close || esc || !open {
            self.about_open = false;
        }
    }

    /// Open the colour picker for `target`, seeded with that target's current colour.
    fn open_color_picker(&mut self, target: ColorTarget) {
        // Borrow `target` (it's non-Copy now — `Highlight` carries a `TokenKind` name) so it can be
        // stored below.
        let (r, g, b) = match &target {
            ColorTarget::Fg => self.editor.fg_color(),
            ColorTarget::Bg => self.editor.bg_color(),
            ColorTarget::UiFg => self.editor.ui_fg_color(),
            ColorTarget::UiBg => self.editor.ui_bg_color(),
            ColorTarget::Caret => self.editor.caret_color(),
            ColorTarget::CmlineCaret => self.editor.cmline_caret_color(),
            ColorTarget::Separator => self.editor.sep_color(),
            ColorTarget::TxtFg => self.editor.txt_fg_color(),
            ColorTarget::TxtBg => self.editor.txt_bg_color(),
            ColorTarget::DwFg => self.editor.host().dw_fg_color(),
            ColorTarget::DwBg => self.editor.host().dw_bg_color(),
            ColorTarget::SelBg => self.editor.sel_bg_color(),
            ColorTarget::SelFg => self.editor.sel_fg_color(),
            ColorTarget::FindBg => self.editor.find_bg_color(),
            ColorTarget::FindFg => self.editor.find_fg_color(),
            ColorTarget::SpellUnderline => self.editor.spell_color(),
            ColorTarget::GrammarUnderline => self.editor.grammar_color(),
            ColorTarget::PaneBorder => self.editor.pane_border_color(),
            // A cleared highlight kind shows as the editor foreground — seed the picker with that.
            ColorTarget::Highlight(k) => self.editor.highlight_color(k).unwrap_or(self.editor.fg_color()),
            // An unset tab state uses egui's theme colour (no fixed RGB) — seed from the chrome bg.
            ColorTarget::Tab(state) => {
                let cur = match state {
                    xyscript_app::host::TabState::Active => self.editor.tab_active_bg(),
                    xyscript_app::host::TabState::Hover => self.editor.tab_hover_bg(),
                    xyscript_app::host::TabState::Inactive => self.editor.tab_inactive_bg(),
                };
                cur.unwrap_or(self.editor.ui_bg_color())
            }
            // The close-× defaults to the UI foreground when unset — seed the picker with that.
            ColorTarget::TabClose => self.editor.tab_x_color().unwrap_or(self.editor.ui_fg_color()),
            // Matched-bracket highlight defaults to the selection background when unset.
            ColorTarget::BalPairs => self.editor.bal_pairs_color().unwrap_or(self.editor.sel_bg_color()),
            // The path colours default to the UI text / background when unset — seed with those.
            ColorTarget::PathFg => {
                self.editor.host().path_fg_color().unwrap_or(self.editor.ui_fg_color())
            }
            ColorTarget::PathBg => {
                self.editor.host().path_bg_color().unwrap_or(self.editor.ui_bg_color())
            }
            ColorTarget::MarkFg => {
                self.editor.host().mark_fg_color().unwrap_or((216, 118, 42)) // bookmark orange
            }
            ColorTarget::MarkBg => {
                self.editor.host().mark_bg_color().unwrap_or(self.editor.bg_color())
            }
            ColorTarget::UrlFg => self.editor.url_fg_color(), // effective (default link blue)
            ColorTarget::UrlBg => self.editor.url_bg_color().unwrap_or(self.editor.bg_color()),
            // EMPH emphasis colours default to the selection colours when unset — seed with those.
            ColorTarget::EmphFg => self.editor.emph_fg_override().unwrap_or(self.editor.sel_fg_color()),
            ColorTarget::EmphBg => self.editor.emph_bg_override().unwrap_or(self.editor.sel_bg_color()),
            // MDA block colours have no single "current" value (they're per-span) — seed the picker
            // with the editor's text / background colour as a sensible starting swatch.
            ColorTarget::MdaFg => self.editor.fg_color(),
            ColorTarget::MdaBg => self.editor.bg_color(),
        };
        self.color_picker = Some((target, egui::Color32::from_rgb(r, g, b)));
    }

    /// Render the open colour-picker window (egui sat/val + hue + RGB sliders). On OK, run the
    /// target's `*COLOR`/`HLCOLOR` setter with the chosen colour; Cancel/× closes without applying.
    fn render_color_picker(&mut self, ctx: &egui::Context) {
        // `ColorTarget` is no longer `Copy` (a highlight target carries a kind name), so clone the
        // small request out rather than move it from the field.
        let Some((target, mut color)) = self.color_picker.clone() else {
            return;
        };
        // Esc dismisses (as Cancel — discard the edit), matching the other dialogs (About /
        // keyboard chart / find). A hex field with focus eats the first Esc to defocus itself;
        // a second Esc then closes — standard egui behaviour.
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let mut open = true;
        let mut do_ok = false;
        let mut do_cancel = false;
        egui::Window::new(target.title())
            .id(egui::Id::new("color_picker"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                egui::color_picker::color_picker_color32(
                    ui,
                    &mut color,
                    egui::color_picker::Alpha::Opaque,
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        do_ok = true;
                    }
                    if ui.button("Cancel").clicked() {
                        do_cancel = true;
                    }
                });
            });
        // Keep the live edits so the picker persists across frames while open.
        if let Some(slot) = self.color_picker.as_mut() {
            slot.1 = color;
        }
        if do_ok {
            self.color_picker = None;
            self.editor.run_command(&target.set_command(color.r(), color.g(), color.b()));
        } else if !open || do_cancel || esc {
            self.color_picker = None;
        }
    }

    fn txt_fg_color(&self) -> (u8, u8, u8) {
        self.editor.txt_fg_color()
    }
    fn txt_bg_color(&self) -> (u8, u8, u8) {
        self.editor.txt_bg_color()
    }

    /// Dismiss the `TXT` window (× / Esc), resuming the paused script. The runtime then
    /// requests a main-window repaint so the resumed script's effects show.
    fn dismiss_txt(&mut self) {
        self.editor.dismiss_text();
        self.pane_view.follow = true;
    }

    /// Whether a script is parked on `READKEY` awaiting one keystroke. The runtime captures
    /// the next raw key (before the keymap) and routes it here via `feed_input_key`.
    fn awaiting_key(&self) -> bool {
        self.editor.awaiting_key()
    }

    /// Whether the parked `READKEY` was `/D` (wants a key description, not the raw character).
    fn readkey_wants_description(&self) -> bool {
        self.editor.readkey_describe()
    }

    /// Whether any in-process script is running (yielding on a CPU slice, or parked on
    /// `WAIT`/`INPUT`/`TXT`). The runtime gates the Ctrl+Break abort on this.
    fn is_script_running(&self) -> bool {
        self.editor.is_script_running()
    }

    /// Ctrl+Break: request the running/hung script be aborted (honored at the next slice).
    fn request_script_abort(&mut self) {
        self.editor.request_script_abort();
        self.status = "Script aborted".to_string();
    }

    /// Deliver the captured keystroke (`ch` empty for keys with no character), its scancode (`$SC`),
    /// and the held modifier bits (→ `$SCC`) to a `READKEY`-parked script, then resume it.
    fn feed_input_key(&mut self, ch: &str, scancode: u16, mods: i64) {
        self.editor.feed_input_key(ch, scancode, mods);
        self.surface_command_feedback();
        self.pane_view.follow = true;
    }

    fn run_captured(&mut self, xpl: &str) {
        // A break in an up/down series: any captured (keymap-bound) command that reaches here is NOT a
        // bare visual up/down motion — those are intercepted by `try_visual_vmotion` first — so it ends
        // the series and clears the sticky goal column. The NEXT up/down then re-memorizes the current
        // column. Without this, a bound LEFT/RIGHT/HOME/END (or any edit) left the goal stale, so the
        // next Down snapped back to the OLD column instead of the one you moved to.
        self.goal_x = None;
        // A bare `save` (Ctrl+S) on an untitled doc needs the native Save-As dialog, which
        // the editor can't open — defer it to the next frame's egui pass (same as the menu).
        if xpl.trim().eq_ignore_ascii_case("save") && self.editor.active_path().is_none() {
            self.pending_save_as = true;
        } else {
            self.editor.run_command(xpl);
            // Surface feedback for keymap-run commands too (e.g. `XC`/F9 exec, or any bound
            // key that runs `PR`/`MSG`) — not just the Enter-submit path.
            self.surface_command_feedback();
        }
        self.pane_view.follow = true;
    }

    /// Status-line text with the leading `$CWD` prefix (`cwd_len` bytes) coloured by
    /// `PAFGCOLOR`/`PABGCOLOR`. Returns a plain string (egui's default styling) when no path colours
    /// are set or there's no prefix, so the common case is byte-for-byte unchanged. `Color32::
    /// PLACEHOLDER` on a segment defers to egui's resolved default text colour at paint time.
    fn status_text(&self, ui: &egui::Ui, text: &str, cwd_len: usize) -> egui::WidgetText {
        let fg = self.editor.host().path_fg_color();
        let bg = self.editor.host().path_bg_color();
        // An `MDA` live-typing indicator flags that subsequently typed text will be styled, and shows
        // WHAT: reverse-video attribute letters (B/I/U/R) plus a colour swatch (the actual armed
        // foreground / background colour). Per-document, so it appears only for the current file. Plain
        // ASCII + coloured backgrounds — no Unicode glyph (a symbol tofus in an arbitrary UI font).
        let (tfg, tbg, tattr) = self.editor.mda_typing();
        let mda = tfg.is_some() || tbg.is_some() || tattr.is_some();
        let has_cwd = cwd_len != 0 && cwd_len <= text.len() && (fg.is_some() || bg.is_some());
        if !mda && !has_cwd {
            return text.to_owned().into(); // fast path — unchanged when there's nothing to decorate
        }
        use egui::text::{LayoutJob, TextFormat};
        let font = egui::TextStyle::Body.resolve(ui.style());
        let col = |c: Option<(u8, u8, u8)>| {
            c.map(|(r, g, b)| egui::Color32::from_rgb(r, g, b)).unwrap_or(egui::Color32::PLACEHOLDER)
        };
        let mut job = LayoutJob::default();
        if mda {
            use xyscript_app::document::{ATTR_BOLD, ATTR_ITALIC, ATTR_REVERSE, ATTR_UNDERLINE};
            // Reverse-video block of attribute letters (only the armed ones).
            if let Some(a) = tattr {
                let mut letters = String::new();
                if a & ATTR_BOLD != 0 { letters.push('B'); }
                if a & ATTR_ITALIC != 0 { letters.push('I'); }
                if a & ATTR_UNDERLINE != 0 { letters.push('U'); }
                if a & ATTR_REVERSE != 0 { letters.push('R'); }
                if !letters.is_empty() {
                    let (uf, ub) = (self.editor.ui_fg_color(), self.editor.ui_bg_color());
                    let mut fmt =
                        TextFormat::simple(font.clone(), egui::Color32::from_rgb(ub.0, ub.1, ub.2));
                    fmt.background = egui::Color32::from_rgb(uf.0, uf.1, uf.2);
                    job.append(&format!(" {letters} "), 0.0, fmt);
                }
            }
            // Colour swatches: a filled block of the actual armed foreground / background colour (two
            // blank spaces whose BACKGROUND is the colour — glyph-free, so it can't tofu).
            for c in [tfg, tbg].into_iter().flatten() {
                let mut sw = TextFormat::simple(font.clone(), egui::Color32::from_rgb(c.0, c.1, c.2));
                sw.background = egui::Color32::from_rgb(c.0, c.1, c.2);
                job.append("  ", 0.0, sw);
            }
            job.append(" ", 0.0, TextFormat::simple(font.clone(), egui::Color32::PLACEHOLDER));
        }
        if has_cwd {
            let mut prefix = TextFormat::simple(font.clone(), col(fg));
            if let Some((r, g, b)) = bg {
                prefix.background = egui::Color32::from_rgb(r, g, b);
            }
            job.append(&text[..cwd_len], 0.0, prefix);
            job.append(&text[cwd_len..], 0.0, TextFormat::simple(font, egui::Color32::PLACEHOLDER));
        } else {
            job.append(text, 0.0, TextFormat::simple(font, egui::Color32::PLACEHOLDER));
        }
        job.into()
    }

    /// Put a just-run command's feedback on the status line: `PR`/`MSG` output if any, else
    /// the op-result status message. Each run clears these, so this reflects just that command;
    /// when there's nothing to show the status is left untouched.
    fn surface_command_feedback(&mut self) {
        // `command_feedback` already applies QUIET (PR always shown; op-statuses/errors filtered).
        if let Some(s) = self.editor.command_feedback() {
            self.status = s;
        }
    }

    /// Render one frame into the central panel. Driven by `runtime.rs` on RedrawRequested
    /// (eframe used to wrap this in a no-margin CentralPanel and call it `App::ui`).
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        // A held-still press "becomes a drag" (starting a text-canvas selection) after egui's
        // `max_click_duration` — default 0.8 s, which feels sluggish. Shorten it so drag-select
        // arms quickly. It's global: it ALSO caps how long a press can last and still count as a
        // click, so keep it high enough (0.2 s) that ordinary menu/button clicks aren't misread.
        ctx.options_mut(|o| o.input_options.max_click_duration = 0.2);

        // B16 UI sizing: `UISCALE` = egui global zoom; `UIFONT` = chrome (menu/status/cmline)
        // font size. (Editor font / line-spacing / margin are applied in the canvas render.)
        let scale = self.editor.ui_scale();
        if (ctx.zoom_factor() - scale).abs() > 0.001 {
            ctx.set_zoom_factor(scale);
        }
        // Open the font picker (`EDFONTS`/`UIFONTS`/`TXTFONTS`) BEFORE the font rebuild below, so a
        // freshly-opened picker's preview family is queued into egui this frame (the picker window
        // itself renders near the end of `ui()`).
        if let Some(kind) = self.editor.take_font_picker_request() {
            self.open_font_picker(kind);
        }
        // Open the colour picker (`*COLORS`/`HLCOLORS`); the window itself renders near `ui()`'s end.
        if let Some(target) = self.editor.take_color_picker_request() {
            self.open_color_picker(target);
        }

        // Main-context egui fonts: the `UIFAMILY` chrome font and the font-picker's live-preview
        // family share one `FontDefinitions` (one `set_fonts` rebuilds the atlas), so install both
        // together and only when either changes. `UIFAMILY` is prepended to BOTH Proportional and
        // Monospace so all chrome text (menus/status use Proportional; the cmline pop-up uses
        // Monospace) picks it up; the preview goes into a dedicated `xy-preview` family. `set_fonts`
        // only takes effect next frame, so request a repaint to apply it promptly.
        let want_ui_family = self
            .editor
            .ui_font_family()
            .filter(|s| !s.is_empty())
            .map(|s| (s.to_string(), self.editor.ui_font_style()));
        let want_preview = self.font_picker.as_ref().map(|p| (p.selected.clone(), p.style));
        if want_ui_family != self.applied_ui_family || want_preview != self.applied_preview {
            self.rebuild_main_fonts(&ctx, want_ui_family.as_ref(), want_preview.as_ref());
            self.applied_ui_family = want_ui_family;
            self.applied_preview = want_preview;
            ctx.request_repaint();
        }
        let cf = self.editor.chrome_font();
        ctx.global_style_mut(|s| {
            use egui::{FontFamily, FontId, TextStyle};
            s.text_styles.insert(TextStyle::Body, FontId::new(cf, FontFamily::Proportional));
            s.text_styles.insert(TextStyle::Button, FontId::new(cf, FontFamily::Proportional));
            s.text_styles.insert(TextStyle::Monospace, FontId::new(cf, FontFamily::Monospace));
        });
        // Editor font + line height for the canvas this frame (`EDFONT` / `LEADING`) — in POINTS.
        self.font_size = self.editor.editor_font();
        self.line_height = (self.editor.editor_font() * self.editor.leading() / 100.0).round().max(1.0);
        // Device scale (HiDPI × UISCALE zoom): raster the canvas at this many pixels per point so
        // egui maps it 1:1 to physical pixels (crisp) instead of upscaling a point-resolution image.
        // CAP it so the canvas texture can't exceed the GPU's max texture side (some adapters cap at
        // 2048) — otherwise egui panics on upload (a large/HiDPI window would overflow it). The
        // window's physical size bounds the canvas, so cap the scale to keep the longer window edge
        // within the limit; on such configs the canvas just renders a touch less crisp.
        let raw_scale = ctx.pixels_per_point().max(0.1);
        let max_side = ctx.input(|i| i.max_texture_side).max(256) as f32;
        let longest_pt = ctx.content_rect().size().max_elem().max(1.0);
        self.render_scale = raw_scale.min((max_side / longest_pt).max(0.1));

        // UI-chrome colours (`UIFGCOLOR`/`UIBGCOLOR`) theme the whole window chrome — menu bar,
        // status line, gutter/margin strips, and pop-ups. The editor text canvas is coloured
        // separately by `FGCOLOR`/`BGCOLOR` in the raster.
        let (br, bgc, bb) = self.editor.ui_bg_color();
        let (fr, fgc, fb) = self.editor.ui_fg_color();
        let (sr, sg, sb) = self.editor.sep_color();
        let ui_bg = egui::Color32::from_rgb(br, bgc, bb);
        let ui_fg = egui::Color32::from_rgb(fr, fgc, fb);
        // `SEPCOLOR` themes egui's separator stroke — the panel boundary rules (menu/tabs/cmline/
        // status) AND the `ui.separator()` bars between tabs both draw with `noninteractive.bg_stroke`.
        let ui_sep = egui::Color32::from_rgb(sr, sg, sb);
        ctx.global_style_mut(|s| {
            s.visuals.panel_fill = ui_bg;
            s.visuals.window_fill = ui_bg;
            s.visuals.window_stroke.color = ui_fg;
            s.visuals.override_text_color = Some(ui_fg);
            s.visuals.widgets.noninteractive.bg_stroke.color = ui_sep;
        });

        // Mirror an external clipboard change in before handling input (so PASTE
        // picks up text copied from another app).
        if ctx.input(|i| !i.events.is_empty()) {
            self.sync_clipboard_in();
        }

        // Autosave (`AUTOSAVE <secs>`): on each interval, save all named+modified buffers.
        // `request_repaint_after` keeps the (otherwise reactive) loop waking while idle, so it
        // fires without user input; `autosave_now` is a no-op when nothing is modified.
        let autosave_secs = self.editor.autosave_secs();
        if autosave_secs > 0 {
            let interval = std::time::Duration::from_secs(autosave_secs);
            if self.last_autosave.elapsed() >= interval {
                match self.editor.autosave_now() {
                    Ok(n) if n > 0 => {
                        self.status = format!("autosaved {n} file{}", if n == 1 { "" } else { "s" });
                    }
                    Ok(_) => {} // nothing modified — stay quiet
                    // A failed autosave must NOT be silent (the file wasn't written).
                    Err(e) => self.status = format!("autosave failed: {e}"),
                }
                self.last_autosave = std::time::Instant::now();
            }
            ctx.request_repaint_after(interval);
        }

        // Auto-reopen (`AUTOREOPEN on`): poll open files for external changes ~every second and reload
        // the clean ones (caret/viewport preserved). `request_repaint_after` keeps the reactive loop
        // waking while idle so a change lands even with the app unfocused. Independent of AUTOSAVE.
        if self.editor.auto_reopen() {
            let interval = std::time::Duration::from_secs(1);
            if self.last_reopen_check.elapsed() >= interval {
                let notes = self.editor.poll_external_changes();
                let reloaded = !notes.is_empty();
                for note in notes {
                    self.status = note; // last note wins on the status line
                }
                self.last_reopen_check = std::time::Instant::now();
                // The poll runs AFTER this frame's canvas was drawn, so a reload's new content would
                // otherwise wait for the next 1s tick — repaint now to show it immediately.
                if reloaded {
                    ctx.request_repaint();
                }
            }
            ctx.request_repaint_after(interval);
        }

        // Session snapshot (`SESSION on`): keep the on-disk session current so a CRASH or forced kill
        // still restores the tabs, not just a graceful quit. The snapshot is changed-only (a shadow
        // is rewritten only when its buffer changed, the manifest skipped when unchanged), so an idle
        // tick does no disk work. `request_repaint_after` keeps the reactive loop waking to run it.
        //
        // A STRUCTURAL change (tab open/close/switch, split/unsplit) snapshots IMMEDIATELY — cheap
        // (manifest only) and it makes the layout crash-safe the instant you change it; buffer
        // CONTENT rides the interval below.
        if self.session_enabled() {
            const SNAP: std::time::Duration = std::time::Duration::from_secs(5);
            let sig = self.editor.host().session_structure_signature();
            let structural = sig != self.last_session_signature;
            if structural || self.last_session_snapshot.elapsed() >= SNAP {
                self.snapshot_session();
                self.last_session_snapshot = std::time::Instant::now();
                self.last_session_signature = sig;
            }
            ctx.request_repaint_after(SNAP);
        }

        // Live language check (`LINT on`): harper runs on a BACKGROUND worker so the UI NEVER blocks
        // — no per-keystroke hitch, no freeze on a huge file. Each frame: apply any finished result,
        // then (re)request the current viewport (the worker coalesces bursts, so scrolling/typing just
        // hands it the freshest view; `request_lint` dedups an unchanged view and is a no-op when off).
        // While a check is out, keep the loop polling so its async result lands without further input.
        if self.editor.poll_lint() {
            ctx.request_repaint(); // underlines changed → redraw this frame
        }
        // Round-robin the checker across the visible panes (one worker, one request at a time), and
        // only issue the next when nothing is in flight — so a request COMPLETES before we send the
        // next (else round-robin would keep replacing the single pending request and neither pane would
        // ever get its result). `request_lint` skips docs already satisfied, so this cycles to whichever
        // pane needs a (re)check and then idles. Single-pane is a one-element list → same as before.
        if !self.editor.lint_in_flight() && !self.lint_panes.is_empty() {
            self.lint_rr = self.lint_rr.wrapping_add(1) % self.lint_panes.len();
            let (doc, lo, hi) = self.lint_panes[self.lint_rr];
            self.editor.request_lint(doc, lo, hi);
        }
        if self.editor.lint_in_flight() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }

        // Background `CHECK` (whole-doc report on its own thread — never freezes the UI). Show a
        // "Working…" status while it runs; when it lands, `poll_check` opens the results tab.
        if self.editor.poll_check() {
            self.status = "Language check complete — see the *Language Check* tab.".to_string();
            ctx.request_repaint();
        } else if self.editor.check_in_progress() {
            self.status = "Checking the document…".to_string();
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }

        // Streaming `GREP` (recursive search on a worker thread — never freezes the UI on a big tree).
        // Drain newly-found hits into the results tab each frame; show progress / the final count on
        // the status line, and keep repainting while a search is in flight.
        let (grep_changed, grep_status) = self.editor.poll_grep();
        if let Some(s) = grep_status {
            self.status = s;
        }
        if grep_changed {
            ctx.request_repaint();
        }
        if self.editor.grep_in_progress() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }

        // `AUTOCFG`: mirror settings to the tracked config file when they change (no-op when off).
        self.editor.poll_config_track();

        // Background shell jobs (`X/BG`): drain completions each frame (sets $RC/$XO + a status
        // notice), and keep the loop polling while any are still running.
        for notice in self.editor.poll_jobs() {
            self.status = notice;
        }
        // A non-freezing `WAIT` parks the script; resume it once the condition holds (jobs
        // finished, or the `WAIT <ms>` interval elapsed). poll_wait runs AFTER poll_jobs so the
        // running set is current this frame; the repaint hint wakes us when the wait can advance.
        let was_waiting = self.editor.wait_repaint_after().is_some();
        self.editor.poll_wait();
        if was_waiting {
            // A resume may have moved the caret (e.g. `TF`/`BF`); bring it back into view.
            self.pane_view.follow = true;
            // …and may have produced PR/MSG output (e.g. a RUN-ed counting/WAIT loop) — surface it
            // so the script visibly progresses each tick instead of looking frozen.
            self.surface_command_feedback();
        }
        if let Some(after) = self.editor.wait_repaint_after() {
            ctx.request_repaint_after(after);
        } else if self.editor.job_count() > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }

        // Cooperative preemption: drive a preemptively-sliced foreground script forward this
        // frame (time-boxed inside the editor), honoring a pending Ctrl+Break abort. While it's
        // still yielding — a CPU-bound loop with no WAIT — repaint IMMEDIATELY so the next slice
        // runs next frame instead of freezing the window, and surface PR/MSG output so a long
        // loop visibly progresses rather than looking hung.
        let was_yielding = self.editor.is_yielding();
        self.editor.pump_yield();
        if self.editor.is_yielding() {
            self.pane_view.follow = true;
            // Prefer the script's own PR/MSG output (so a progress loop shows its messages); when
            // it's a silent loop, show a running indicator so the editor reads as busy-not-frozen
            // and the user knows how to stop it.
            if let Some(s) = self.editor.command_feedback() {
                self.status = s;
            } else {
                self.status = "Running script…  (Ctrl+Break to abort)".to_string();
            }
            ctx.request_repaint();
        } else if was_yielding {
            // Just finished (or was aborted): bring the caret into view and surface the result.
            self.pane_view.follow = true;
            self.surface_command_feedback();
        }

        // --- Menu bar (File / View). Collect an action; apply it below so we
        //     never mutate `self` inside egui's nested menu closures. ---
        let titlebar_on = self.editor.show_titlebar();
        // Writer's View (`WV`): hide ALL chrome — the drag strip, menu bar, tab strip, command line,
        // and status line — leaving only the text window. Each chrome block below is gated on this.
        let writer_view = self.editor.writer_view();
        let mut act: Option<Act> = None;
        // Title-bar replacement: when the OS title bar is hidden (`TITLEBAR off`), draw a thin
        // draggable strip at the very top so the window can still be moved (XyWrite-DOS-in-a-window
        // muscle memory). Grabbing it asks the runtime to start an OS window move.
        if !titlebar_on && !writer_view {
            // Fill the strip with the UI chrome background so it blends with the user's scheme
            // (rather than a stark default). The Grab cursor on hover signals it's draggable.
            let (sr, sg, sb) = self.editor.ui_bg_color();
            let strip_fill = egui::Color32::from_rgb(sr, sg, sb);
            let mut grabbed = false;
            egui::Panel::top("titlebar_strip").exact_size(7.0).show_inside(ui, |ui| {
                let rect = ui.max_rect();
                let resp = ui
                    .interact(
                        rect,
                        egui::Id::new("titlebar_strip_handle"),
                        egui::Sense::click_and_drag(),
                    )
                    .on_hover_cursor(egui::CursorIcon::Grab);
                ui.painter().rect_filled(rect, 0.0, strip_fill);
                if resp.drag_started() {
                    grabbed = true;
                }
            });
            if grabbed {
                self.start_window_drag = true;
            }
        }
        // `$MB` (SMB/CMB/TMB) hides/shows the menu bar. When hidden, re-show it from the
        // command line (`TMB`) or a bound key.
        // `MENU` (verb / accelerator) opens or toggles the keyboard menu — applied here, BEFORE the
        // render below, so a request made this frame shows its dropdown the same frame. Bare `MENU`
        // toggles; `MENU "f"` opens the top menu with accelerator letter `f`.
        if let Some(arg) = self.editor.take_menu_kbd_request() {
            match arg {
                None => self.toggle_menu_kbd(),
                Some(letter) => self.open_menu_accel(&letter),
            }
        }
        // Unified menu system (mouse + keyboard, one `menu_nav` state): the top bar plus cascading
        // dropdowns. One renderer for both input methods — replaces egui's built-in MenuBar and the
        // old floating keyboard overlay. Suppressed entirely in Writer's View.
        if !writer_view {
            if let Some(ma) = self.render_menus(ui) {
                act = self.perform_menu_act(ma);
            }
        }
        // The keyboard navigator chose an item on a prior frame (in the runtime key path) — run it
        // now, through the same dispatch as the mouse menus.
        if act.is_none() {
            if let Some(ma) = self.pending_menu_act.take() {
                act = self.perform_menu_act(ma);
            }
        }

        // --- Apply the collected action (now `self` is freely borrowable). ---
        if let Some(a) = act {
            match a {
                Act::New => {
                    let r = self.editor.new_doc();
                    self.reset_view();
                    self.note(r);
                }
                Act::Save => {
                    let r = self.editor.save();
                    self.note(r);
                }
                Act::Highlight(on) => {
                    let r = self.editor.set_highlight(on);
                    self.note(r);
                }
                Act::TogglePreview => {
                    // Host-owned (so PV/$PV/SAVECFG all work); the menu just runs the verb. The
                    // render cache is cleared on the visible→hidden transition in the pane code.
                    self.editor.run_command("PV");
                }
                Act::Run(cmd) => {
                    // Run the verb and follow the caret, so a navigating/editing command from
                    // the menu scrolls its result into view. Surface the feedback so verbs whose
                    // ONLY effect is a status message (WORDCOUNT/CHARCOUNT) actually show it.
                    self.editor.run_command(&cmd);
                    self.surface_command_feedback();
                    self.pane_view.follow = true;
                }
                Act::StageCmdline(text) => {
                    // Put the (unrun) text on the command line, focused with the caret at the end, so
                    // the user finishes typing the argument and presses Enter. See `m_stage`.
                    self.editor.load_cmline(&text);
                }
                Act::OpenFind { replace } => self.open_find_dialog(replace),
                Act::OpenRunCmd => self.open_run_dialog(),
                Act::OpenGrep => self.open_grep_dialog(),
                Act::ShowProperties => self.properties_open = true,
                Act::ShowAbout => self.about_open = true,
                Act::ShowReference => {
                    // Write the embedded reference to a temp `.typ` and open it (typst DOCTYPE, so it
                    // previews/exports); reopening reuses the tab.
                    let path = std::env::temp_dir().join("xyscript-reference.typ");
                    match std::fs::write(&path, REFERENCE_TYP) {
                        Ok(()) => {
                            let r = self.editor.open(&path.to_string_lossy());
                            self.reset_view();
                            self.note(r);
                        }
                        Err(e) => self.note(Err(format!("could not open the reference: {e}"))),
                    }
                }
                Act::ShowManual => {
                    // Same path as the reference: write the embedded manual to a temp `.typ` and open
                    // it (typst DOCTYPE, so it previews/exports); reopening reuses the tab.
                    let path = std::env::temp_dir().join("xyscript-manual.typ");
                    match std::fs::write(&path, MANUAL_TYP) {
                        Ok(()) => {
                            let r = self.editor.open(&path.to_string_lossy());
                            self.reset_view();
                            self.note(r);
                        }
                        Err(e) => self.note(Err(format!("could not open the manual: {e}"))),
                    }
                }
            }
        }

        // `SAAS` with no path (run from the cmline / a key) requests the native Save-As dialog;
        // fold it into the same pending flag the Ctrl+S path uses.
        if self.editor.take_save_as_request() {
            self.pending_save_as = true;
        }
        // A keymap `save` (Ctrl+S) on an untitled doc, and bare `OPEN` (Ctrl+O): request the native
        // dialog. It is DEFERRED to after the frame (parented + modal) — never run inline, which
        // deadlocked egui's Context mid-frame and froze the app.
        if std::mem::take(&mut self.pending_save_as) {
            self.pending_dialog = Some(PendingDialog::SaveAs);
        }
        if let Some(hex) = self.editor.take_open_request() {
            self.pending_dialog = Some(PendingDialog::Open { hex });
        }
        // `FIND` requests the non-modal Find/Replace dialog (e.g. a bound key or the command line).
        if self.editor.take_find_request() {
            self.open_find_dialog(false);
        }
        // A bare `GREP` (no pattern) requests the GREP dialog — the interactive companion to FIND.
        if self.editor.take_grep_dialog_request() {
            self.open_grep_dialog();
        }

        // (GUI2) Input — two channels:
        //  - command keys (Enter/Backspace) → KBD resolver (`feed_key`);
        //  - printable text → OS `Text`/IME `Commit` (layout/IME-resolved, multilingual).
        //  Skipped while an egui widget (e.g. the command bar) wants the keyboard.
        let canvas_has_keys = !ctx.egui_wants_keyboard_input();
        // `QUIT` (verify): sweep tabs with unsaved changes, prompting save/discard/cancel for
        // each (this branch owns the keyboard), then exit. When none remain dirty, exit.
        // Pause the sweep while a Save-As dialog it launched is pending (the deferred dialog runs
        // after this frame; we resume next frame once the doc is saved or the quit is aborted).
        if self.quitting && self.pending_dialog.is_none() {
            match self.editor.next_dirty_tab() {
                None => {
                    self.should_exit = true;
                    self.quitting = false;
                }
                Some(i) => {
                    self.editor.set_active_buffer(i); // surface the tab being asked about
                    for ev in ctx.input(|i| i.events.clone()) {
                        if let egui::Event::Key { key, pressed: true, .. } = ev {
                            match key {
                                egui::Key::Y => {
                                    if self.editor.active_path().is_some() {
                                        let r = self.editor.save();
                                        self.note(r);
                                    } else {
                                        // Defer the Save-As (parented + modal, run after the frame);
                                        // its result advances or aborts the quit in apply_dialog_result.
                                        self.pending_dialog = Some(PendingDialog::SaveAs);
                                        self.save_as_for_quit = true;
                                    }
                                }
                                egui::Key::N => self.editor.mark_active_saved(), // discard
                                egui::Key::Escape => self.quitting = false,      // cancel quit
                                _ => {}
                            }
                        }
                    }
                    ctx.request_repaint();
                }
            }
        } else if let Some(debug) = self.editor.breakpoint_request() {
            // A `BK` breakpoint is MODAL. `BK on`: Y stops, any other key continues. `BK debug`:
            // Y stops; N / Esc / Enter continue; other keys are ignored (use the buttons). The
            // render block below also draws Continue / Stop buttons.
            for ev in ctx.input(|i| i.events.clone()) {
                if let egui::Event::Key { key, pressed: true, .. } = ev {
                    let stop = key == egui::Key::Y;
                    let cont = !debug
                        || matches!(key, egui::Key::N | egui::Key::Escape | egui::Key::Enter);
                    if stop || cont {
                        self.editor.resolve_breakpoint(stop);
                        self.surface_command_feedback();
                        self.pane_view.follow = true;
                        break; // one resolution per frame (don't skip a following breakpoint)
                    }
                }
            }
            ctx.request_repaint();
        } else if self.message_box.is_some() {
            // A `MSG` box is MODAL: it owns the keyboard until dismissed. Esc / Enter / Space
            // (or the OK button / × in the render block) close it; all other keys are swallowed.
            for ev in ctx.input(|i| i.events.clone()) {
                if let egui::Event::Key { key, pressed: true, .. } = ev {
                    if matches!(key, egui::Key::Escape | egui::Key::Enter | egui::Key::Space) {
                        self.dismiss_message_box(); // resumes the script if it was a blocking MSG
                        break;
                    }
                }
            }
            ctx.request_repaint();
        } else if self.history_dialog.is_some() {
            for ev in ctx.input(|i| i.events.clone()) {
                let Some(sel) = self.history_dialog else { break };
                // The filtered view: sel indexes this; deletion uses the real history index.
                let view = self.editor.history_view(&self.history_filter);
                let last = view.len().saturating_sub(1);
                match ev {
                    // Type-to-find: printable input narrows the list (substring, case-insensitive),
                    // like the PICK dialog. Jump the highlight to the top of the narrowed view.
                    egui::Event::Text(s) => {
                        self.history_filter.push_str(&s);
                        self.history_dialog = Some(0);
                    }
                    egui::Event::Key { key, pressed: true, modifiers, .. } => match key {
                        egui::Key::ArrowUp => {
                            if sel > 0 {
                                self.history_dialog = Some(sel - 1);
                            }
                        }
                        egui::Key::ArrowDown => {
                            if sel + 1 < view.len() {
                                self.history_dialog = Some(sel + 1);
                            }
                        }
                        // Page + jump navigation (PU/PD page; TF/BF and Home/End jump top/bottom),
                        // matching the PICK dialog — so the user's own nav keys work in the list.
                        egui::Key::PageUp => self.history_dialog = Some(sel.saturating_sub(10)),
                        egui::Key::PageDown => self.history_dialog = Some((sel + 10).min(last)),
                        egui::Key::Home => self.history_dialog = Some(0),
                        egui::Key::End => self.history_dialog = Some(last),
                        egui::Key::Enter => {
                            if let Some((_, cmd)) = view.get(sel) {
                                let cmd = cmd.clone();
                                self.history_dialog = None;
                                self.history_filter.clear();
                                if modifiers.shift {
                                    // Shift+Enter: pull the command back to the CMline to edit /
                                    // inspect — do NOT run it (and don't reorder the history).
                                    self.editor.load_cmline(&cmd);
                                } else {
                                    // Enter: run it AND promote it to the top of the history, exactly
                                    // like re-running it from the command line.
                                    self.editor.push_history(&cmd);
                                    self.editor.run_command(&cmd);
                                    self.surface_command_feedback();
                                    self.pane_view.follow = true;
                                }
                            }
                        }
                        // Backspace edits the type-to-find filter; Delete removes the highlighted
                        // history entry (they shared a key before type-to-find existed).
                        egui::Key::Backspace => {
                            self.history_filter.pop();
                            self.history_dialog = Some(0);
                        }
                        egui::Key::Delete => {
                            if let Some((real, _)) = view.get(sel) {
                                self.editor.delete_history(*real);
                                let n = self.editor.history_view(&self.history_filter).len();
                                self.history_dialog =
                                    Some(if n == 0 { 0 } else { sel.min(n - 1) });
                            }
                        }
                        egui::Key::Escape => {
                            self.history_dialog = None;
                            self.history_filter.clear();
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
            ctx.request_repaint();
        } else if self.pick_dialog.is_some() {
            // A script parked on PICK owns the keyboard: ↑/↓ (and PgUp/PgDn) move the highlight,
            // typing filters (type-to-find), Backspace shortens the filter, Enter chooses the
            // highlighted item, Esc cancels. The chosen row's ORIGINAL index resumes the script.
            if let Some((_, items)) = self.editor.pick_request() {
                for ev in ctx.input(|i| i.events.clone()) {
                    let filtered = pick_filter_view(&items, &self.pick_filter);
                    let last = filtered.len().saturating_sub(1);
                    let sel = self.pick_dialog.unwrap_or(0).min(last);
                    match ev {
                        egui::Event::Key { key: egui::Key::ArrowUp, pressed: true, .. } => {
                            self.pick_dialog = Some(sel.saturating_sub(1));
                        }
                        egui::Event::Key { key: egui::Key::ArrowDown, pressed: true, .. } => {
                            self.pick_dialog = Some((sel + 1).min(last));
                        }
                        egui::Event::Key { key: egui::Key::PageUp, pressed: true, .. } => {
                            self.pick_dialog = Some(sel.saturating_sub(10));
                        }
                        egui::Event::Key { key: egui::Key::PageDown, pressed: true, .. } => {
                            self.pick_dialog = Some((sel + 10).min(last));
                        }
                        egui::Event::Key { key: egui::Key::Home, pressed: true, .. } => {
                            self.pick_dialog = Some(0);
                        }
                        egui::Event::Key { key: egui::Key::End, pressed: true, .. } => {
                            self.pick_dialog = Some(last);
                        }
                        egui::Event::Key { key: egui::Key::Enter, pressed: true, .. } => {
                            let choice = filtered.get(sel).map(|(orig, _)| *orig);
                            self.pick_dialog = None;
                            self.pick_filter.clear();
                            self.editor.pick_respond(choice);
                            self.surface_command_feedback();
                            self.pane_view.follow = true;
                        }
                        egui::Event::Key { key: egui::Key::Escape, pressed: true, .. } => {
                            self.pick_dialog = None;
                            self.pick_filter.clear();
                            self.editor.pick_respond(None);
                            self.surface_command_feedback();
                        }
                        egui::Event::Key { key: egui::Key::Backspace, pressed: true, .. } => {
                            self.pick_filter.pop();
                            self.pick_dialog = Some(0);
                        }
                        egui::Event::Text(s) => {
                            self.pick_filter.push_str(&s);
                            self.pick_dialog = Some(0);
                        }
                        _ => {}
                    }
                }
            } else {
                self.pick_dialog = None; // the request vanished (script aborted) — close
            }
            ctx.request_repaint();
        } else if self.editor.awaiting_line() {
            // A script parked on INPUT owns the keyboard: collect a line (Text/Paste/
            // Backspace). Enter submits it; Esc delivers an empty line so the script can
            // branch on "no answer". The resumed run may park again (next INPUT).
            for ev in ctx.input(|i| i.events.clone()) {
                match ev {
                    egui::Event::Text(s) => self.input_line.push_str(&s),
                    egui::Event::Paste(s) => self.input_line.push_str(&s),
                    egui::Event::Key { key: egui::Key::Backspace, pressed: true, .. } => {
                        self.input_line.pop();
                    }
                    egui::Event::Key { key: egui::Key::Enter, pressed: true, .. } => {
                        let line = std::mem::take(&mut self.input_line);
                        self.editor.feed_input_line(line);
                        self.surface_command_feedback();
                        self.pane_view.follow = true;
                    }
                    egui::Event::Key { key: egui::Key::Escape, pressed: true, .. } => {
                        self.input_line.clear();
                        self.editor.feed_input_line(String::new());
                        self.surface_command_feedback();
                    }
                    _ => {}
                }
            }
            ctx.request_repaint();
        } else if self.editor.close_prompt().is_some() {
            for ev in ctx.input(|i| i.events.clone()) {
                if let egui::Event::Key { key, pressed: true, .. } = ev {
                    let ans = match key {
                        egui::Key::Y => Some(CloseAnswer::Yes),
                        egui::Key::N => Some(CloseAnswer::No),
                        egui::Key::Escape => Some(CloseAnswer::Cancel),
                        _ => None,
                    };
                    if let Some(a) = ans {
                        self.editor.resolve_close(a);
                        self.surface_command_feedback();
                        self.pane_view.follow = true;
                    }
                }
            }
            ctx.request_repaint();
        } else if self.editor.open_prompt().is_some() {
            // "File is already open" — (Y)es open again · (N)o cancel · (G)o to it · (R)ead-only.
            for ev in ctx.input(|i| i.events.clone()) {
                if let egui::Event::Key { key, pressed: true, .. } = ev {
                    let ans = match key {
                        egui::Key::Y => Some(OpenAnswer::Yes),
                        egui::Key::N | egui::Key::Escape => Some(OpenAnswer::No),
                        egui::Key::G => Some(OpenAnswer::Go),
                        egui::Key::R => Some(OpenAnswer::Read),
                        _ => None,
                    };
                    if let Some(a) = ans {
                        self.editor.resolve_open(a);
                        self.surface_command_feedback();
                        self.pane_view.follow = true;
                        break;
                    }
                }
            }
            ctx.request_repaint();
        } else if self.editor.verify_active() {
            for ev in ctx.input(|i| i.events.clone()) {
                if let egui::Event::Key { key, pressed: true, .. } = ev {
                    let choice = match key {
                        egui::Key::Y | egui::Key::Enter => Some(VerifyChoice::Yes),
                        egui::Key::N => Some(VerifyChoice::No),
                        egui::Key::O => Some(VerifyChoice::OneMore),
                        egui::Key::Q => Some(VerifyChoice::Quit),
                        egui::Key::Escape => Some(VerifyChoice::Esc),
                        _ => None,
                    };
                    if let Some(c) = choice {
                        self.editor.verify_respond(c);
                        self.pane_view.follow = true;
                    }
                }
            }
            ctx.request_repaint();
        } else if canvas_has_keys
            && self.editor.active_is_dir_listing()
            && !self.editor.cmline_open()
        {
            // A DIR/LS listing owns the keyboard. When a delete-with-confirm is armed
            // (Del was pressed), the listing is in a y/n sub-mode and ignores everything
            // else; otherwise: arrows/page navigate, Enter descends/opens, Ctrl+Enter
            // launches via the OS, Del arms a delete-confirm, F3 steps express-key matches,
            // Ctrl+C copies the path, printables drive type-to-find, Backspace shortens,
            // Esc clears. The buffer is read-only, so nothing here inserts text.
            if self.editor.dir_delete_prompt().is_some() {
                for ev in ctx.input(|i| i.events.clone()) {
                    if let egui::Event::Key { key, pressed: true, .. } = ev {
                        match key {
                            egui::Key::Y => {
                                self.editor.dir_resolve_delete(true);
                                self.pane_view.follow = true;
                            }
                            egui::Key::N | egui::Key::Escape => {
                                self.editor.dir_resolve_delete(false);
                            }
                            _ => {}
                        }
                    }
                }
            } else {
                for ev in ctx.input(|i| i.events.clone()) {
                    match ev {
                        egui::Event::Key { key, pressed: true, modifiers, .. } => match key {
                            // Ctrl+Enter launches the entry with the OS handler (must
                            // precede the plain-Enter arm).
                            egui::Key::Enter if modifiers.ctrl => self.editor.dir_launch(),
                            egui::Key::Enter => {
                                self.editor.dir_enter();
                                self.pane_view.follow = true;
                            }
                            // Del arms a delete-with-confirm for the caret entry.
                            egui::Key::Delete => self.editor.dir_request_delete(),
                            // Ctrl+C copies the caret entry's path to the clipboard.
                            egui::Key::C if modifiers.ctrl => self.editor.dir_copy_path(),
                            egui::Key::F3 => {
                                self.editor.dir_find_again();
                                self.pane_view.follow = true;
                            }
                            egui::Key::Backspace => {
                                self.editor.dir_find_backspace();
                                self.pane_view.follow = true;
                            }
                            egui::Key::Escape => self.editor.dir_find_clear(),
                            egui::Key::ArrowUp => {
                                self.visual_vmove(false, false);
                                self.pane_view.follow = true;
                            }
                            egui::Key::ArrowDown => {
                                self.visual_vmove(true, false);
                                self.pane_view.follow = true;
                            }
                            egui::Key::PageUp => self.page_move(false, false),
                            egui::Key::PageDown => self.page_move(true, false),
                            _ => {}
                        },
                        egui::Event::Text(s) if !s.is_empty() => {
                            for ch in s.chars() {
                                self.editor.dir_find_push(ch);
                            }
                            self.pane_view.follow = true;
                        }
                        _ => {}
                    }
                }
            }
            // No unconditional repaint here: like the normal editing branch, the listing
            // updates reactively (a keypress wakes a frame that re-renders the moved caret).
            // A per-frame repaint pins the render loop and backs input up — felt as lag.
        } else if canvas_has_keys {
        for ev in ctx.input(|i| i.events.clone()) {
            // HEX editing: in a writable hex view (cmline closed), typed chars and edit keys become
            // byte edits (nibble / whole-byte / insert / delete / cut-copy-paste). Consumed events
            // skip the normal text path; navigation keys fall through to the host's byte-motion.
            if self.editor.hex_view() && !self.editor.active_read_only() && !self.editor.cmline_open() {
                if self.hex_edit_event(&ev) {
                    ctx.request_repaint();
                    continue;
                }
            }
            match ev {
                egui::Event::Key { key, physical_key, pressed: true, modifiers, .. } => {
                    // In command-line mode, Enter submits and Esc cancels; every
                    // other key still feeds the resolver, and editing/motion route
                    // to the CMline (the host's `active()` follows `target`).
                    if self.editor.cmline_open() {
                        match key {
                            egui::Key::Enter => {
                                self.editor.submit_cmdline();
                                // Surface the command's feedback (PR/MSG output or op status).
                                self.surface_command_feedback();
                                // A command may have opened a file, jumped to a match,
                                // etc. — bring the caret (now at the new spot, e.g. the
                                // top of a freshly opened file) into view.
                                self.pane_view.follow = true;
                                // The canvas already rastered earlier THIS frame, so a search's
                                // request to centre an off-screen match (`take_reveal_center`) — and
                                // this `follow` — only take effect on the NEXT render. Schedule it now,
                                // else the view wouldn't move until the next unrelated input.
                                ctx.request_repaint();
                            }
                            egui::Key::Escape => self.editor.cancel_cmdline(),
                            // Up/Down recall the command history into the CMline (shell-style).
                            egui::Key::ArrowUp => self.editor.history_prev(),
                            egui::Key::ArrowDown => self.editor.history_next(),
                            _ => {
                                let code =
                                    physical_key.and_then(key_to_code).or_else(|| key_to_code(key));
                                if let Some(code) = code {
                                    self.editor.feed_key_mods(
                                        code,
                                        modifiers.shift,
                                        modifiers.ctrl,
                                        modifiers.alt,
                                    );
                                }
                            }
                        }
                    } else if self.editor.active_is_grep_listing()
                        && key == egui::Key::Enter
                        && !modifiers.ctrl
                        && !modifiers.alt
                        && !modifiers.shift
                    {
                        // Enter on a GREP results tab opens the hit under the caret and jumps to it
                        // (the keyboard equivalent of `GJ` / a double-click). Other keys fall through
                        // to normal navigation, so arrows/PageUp-Down still scroll the results.
                        self.editor.grep_jump();
                        self.pane_view.follow = true;
                        ctx.request_repaint();
                    } else if !modifiers.ctrl
                        && !modifiers.alt
                        && matches!(key, egui::Key::ArrowUp | egui::Key::ArrowDown)
                    {
                        // Plain / Shift Up-Down move by VISUAL row (wrapped-line
                        // aware); Shift extends. (Ctrl+Up/Down fall through to the KBD
                        // for paragraph motion.) This is the one place arrows bypass
                        // the resolver, since wrapping is a GUI-layout concern.
                        self.visual_vmove(matches!(key, egui::Key::ArrowDown), modifiers.shift);
                    } else if !modifiers.ctrl
                        && !modifiers.alt
                        && matches!(key, egui::Key::PageUp | egui::Key::PageDown)
                    {
                        // Page motion scrolls the view as well as moving the caret, so
                        // it's handled here rather than via the KBD's PD/PU verbs. Coalesce
                        // the net delta and apply it ONCE after the event loop: holding
                        // PageUp/Down queues many key-repeats into a single frame, and doing
                        // a full window-build+shape per event freezes the frame (measured:
                        // 60 queued PgDn = 60 reshapes = ~24ms in one frozen frame).
                        self.pending_page += if matches!(key, egui::Key::PageDown) { 1 } else { -1 };
                        self.pending_page_extend = modifiers.shift;
                    } else {
                        // Prefer the physical key (KBD-as-layout); fall back to the
                        // logical key so numpad nav (NumLock off → logical ArrowLeft/
                        // Home/…) works even though its physical key is a numpad code.
                        // Modifiers select the layer: Shift+arrow → SHIFT table
                        // `LEFT SEL` etc. (extend selection).
                        self.goal_x = None; // any non-vertical key drops the goal column
                        let code = physical_key.and_then(key_to_code).or_else(|| key_to_code(key));
                        if let Some(code) = code {
                            self.editor.feed_key_mods(
                                code,
                                modifiers.shift,
                                modifiers.ctrl,
                                modifiers.alt,
                            );
                            self.pane_view.follow = true;
                        }
                    }
                }
                egui::Event::Text(s) if !s.is_empty() => {
                    self.goal_x = None;
                    self.editor.insert_str(&s);
                    if !self.editor.cmline_open() {
                        self.pane_view.follow = true;
                    }
                }
                egui::Event::Ime(ime) => match ime {
                    egui::ImeEvent::Commit(s) => {
                        self.preedit.clear();
                        if !s.is_empty() {
                            self.editor.insert_str(&s);
                            if !self.editor.cmline_open() {
                                self.pane_view.follow = true;
                            }
                        }
                    }
                    egui::ImeEvent::Preedit(s) => self.preedit = s,
                    egui::ImeEvent::Enabled | egui::ImeEvent::Disabled => self.preedit.clear(),
                },
                _ => {}
            }
        }
        // Apply the frame's coalesced page motion. The window margin is only one
        // screen, so each `page_move` advances ~one page and must rebuild+reshape;
        // we cap the per-frame steps so a heavy key-repeat burst can't wedge the
        // frame (a hold still flies — up to CAP pages per repaint is far more than
        // the OS key-repeat rate delivers).
        if self.pending_page != 0 {
            const PAGE_STEP_CAP: u32 = 4;
            let down = self.pending_page > 0;
            let extend = self.pending_page_extend;
            let steps = self.pending_page.unsigned_abs().min(PAGE_STEP_CAP);
            self.pending_page = 0;
            for _ in 0..steps {
                self.page_move(down, extend);
            }
        }
        }
        // --- Tab strip: one tab per open buffer, shown when more than one is VISIBLE — or, with
        //     `SHOWONETAB on`, even for a single doc (default off keeps a lone doc uncluttered).
        //     Click a tab to switch; the × closes it via `AB/V` (save-prompt if modified). Mutations
        //     are deferred past the render borrow. The *Shell Output* tab is hidden while `SW`/`$SW`
        //     is off (the buffer keeps streaming; we just skip its tab — and keep `enumerate`
        //     indices = real buffer order).
        let tabs = self.editor.tabs();
        let shell_visible = self.editor.shell_output_visible();
        let is_hidden = |t: &xyscript_app::host::TabInfo| !shell_visible && t.name == "*Console*";
        let visible_tabs = tabs.iter().filter(|t| !is_hidden(t)).count();
        // The tab strip's on-screen rect (set below when it's shown), so the mouse-wheel handler can
        // tell when the pointer is over the *scrollable* strip and NOT also scroll the canvas.
        let mut tab_strip_rect: Option<egui::Rect> = None;
        if !writer_view && (visible_tabs > 1 || (self.editor.show_one_tab() && visible_tabs == 1)) {
            let active = self.editor.active_index();
            let mut switch_to: Option<usize> = None;
            let mut close: Option<usize> = None; // right-click "Close tab" / the × button (AB/V)
            let mut close_force: Option<usize> = None; // right-click "Close without saving" (AB)
            let mut new_tab = false;
            let tab_active_bg = self.editor.tab_active_bg();
            let tab_hover_bg = self.editor.tab_hover_bg();
            let tab_inactive_bg = self.editor.tab_inactive_bg();
            // The close-× and new-tab-+ glyphs are frameless (sit on the themed tab/chrome bg). The ×
            // uses its own `TABXCOLOR` knob, falling back to the UI foreground; + follows the UI fg.
            let ui_fg = {
                let (r, g, b) = self.editor.ui_fg_color();
                egui::Color32::from_rgb(r, g, b)
            };
            let x_glyph = self
                .editor
                .tab_x_color()
                .map(|(r, g, b)| egui::Color32::from_rgb(r, g, b))
                .unwrap_or(ui_fg);
            let tabs_panel = egui::Panel::top("tabs").show_inside(ui, |ui| {
                // Tab-title backgrounds (`TABBGCOLOR`), scoped to this strip's style: active tab uses
                // the selection fill, hover uses the hovered widget fill, inactive the idle fill.
                // Each stays egui's theme colour unless the user set it.
                let vis = &mut ui.style_mut().visuals;
                if let Some((r, g, b)) = tab_active_bg {
                    vis.selection.bg_fill = egui::Color32::from_rgb(r, g, b);
                }
                if let Some((r, g, b)) = tab_hover_bg {
                    let c = egui::Color32::from_rgb(r, g, b);
                    vis.widgets.hovered.weak_bg_fill = c;
                    vis.widgets.hovered.bg_fill = c;
                }
                if let Some((r, g, b)) = tab_inactive_bg {
                    let c = egui::Color32::from_rgb(r, g, b);
                    vis.widgets.inactive.weak_bg_fill = c;
                    vis.widgets.inactive.bg_fill = c;
                }
                // Cap the strip at TWO rows and scroll vertically when there are more tabs than
                // fit: without this the wrapped strip grows unbounded and eats the canvas. Height =
                // two button rows + the inter-row gap; `auto_shrink([false, true])` keeps the full
                // width (so the wrap width is right) while collapsing to one row when tabs are few.
                let row_h = ui
                    .spacing()
                    .interact_size
                    .y
                    .max(ui.text_style_height(&egui::TextStyle::Body));
                let two_rows = row_h * 2.0 + ui.spacing().item_spacing.y;
                egui::ScrollArea::vertical()
                    .max_height(two_rows)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (i, t) in tabs.iter().enumerate() {
                        // Skip the *Shell Output* tab while SW is off — but keep `i` as the real
                        // buffer index so switch/close still target the right buffer.
                        if is_hidden(t) {
                            continue;
                        }
                        // truncated name [lock if read-only] [* if modified]. A long filename would
                        // otherwise stretch its tab to fit; cap it. Hover ALWAYS shows the full path +
                        // filename (or just the name for an unsaved / special buffer that has no path).
                        const TAB_NAME_MAX: usize = 28;
                        let mut label = truncate_tab_name(&t.name, TAB_NAME_MAX);
                        if t.read_only {
                            label.push_str(" 🔒");
                        }
                        if t.modified {
                            label.push_str(" *");
                        }
                        let resp = ui
                            .selectable_label(i == active, label)
                            .on_hover_text(t.path.clone().unwrap_or_else(|| t.name.clone()));
                        if resp.clicked() {
                            switch_to = Some(i);
                        }
                        // Right-click menu: close with the save-prompt, or (for a savable tab) discard.
                        resp.context_menu(|ui| {
                            if ui.button("Close tab").clicked() {
                                close = Some(i);
                                ui.close();
                            }
                            // A DIR listing has nothing to save, so "close without saving" is moot.
                            if !t.dir_listing && ui.button("Close without saving").clicked() {
                                close_force = Some(i);
                                ui.close();
                            }
                        });
                        let x = egui::RichText::new("×").color(x_glyph);
                        if ui.add(egui::Button::new(x).frame(false).small()).clicked() {
                            close = Some(i);
                        }
                        ui.separator();
                    }
                    // New-tab button (opens a fresh empty buffer, like the File ▸ New menu).
                    let plus = egui::RichText::new("+").color(ui_fg);
                    if ui.add(egui::Button::new(plus).frame(false)).on_hover_text("New tab").clicked()
                    {
                        new_tab = true;
                    }
                });
                    });
            });
            tab_strip_rect = Some(tabs_panel.response.rect);
            if let Some(i) = switch_to {
                self.editor.set_active_buffer(i);
                self.pane_view.follow = true;
            }
            if let Some(i) = close {
                // Focus that tab, then close it through the save-prompt path (`AB/V`): a clean
                // buffer closes at once; a modified one arms the prompt the next frame handles.
                self.editor.set_active_buffer(i);
                self.editor.run_command("AB/V");
                self.surface_command_feedback();
                self.pane_view.follow = true;
            }
            if let Some(i) = close_force {
                // "Close without saving": focus the tab, then close it with no save prompt (`AB`),
                // discarding any unsaved changes.
                self.editor.set_active_buffer(i);
                self.editor.run_command("AB");
                self.surface_command_feedback();
                self.pane_view.follow = true;
            }
            if new_tab {
                let r = self.editor.new_doc();
                self.reset_view();
                self.note(r);
            }
        }

        // Mouse-wheel free-scroll (doesn't trigger follow). Runs AFTER the tab strip so we know its
        // rect. Skip it while the pointer is over a FLOATING egui area (a Window/popup/menu, e.g. the
        // Keyboard chart) — otherwise the wheel would scroll that window AND leak into the canvas
        // underneath. We test the layer ORDER directly rather than `is_pointer_over_egui()`: that also
        // returns true over the *background* central panel once the canvas image (an allocated
        // `ui.image().interact()`) has consumed the root UI's available rect — which is everywhere over
        // the text, so it swallowed every wheel event. Panels + canvas live in `Order::Background`;
        // only real floating areas are above. ALSO skip it over the tab strip itself: that's a
        // background panel too (same order as the canvas), but its own `ScrollArea` handles the wheel,
        // so letting it fall through here would scroll the tabs AND the canvas at once.
        let pointer = ctx.input(|i| i.pointer.interact_pos());
        let over_floating = pointer
            .and_then(|pos| ctx.layer_id_at(pos))
            .is_some_and(|layer| layer.order != egui::Order::Background);
        let over_tabs = matches!((pointer, tab_strip_rect), (Some(p), Some(r)) if r.contains(p));
        let (wheel, hwheel, shift) =
            ctx.input(|i| (i.smooth_scroll_delta.y, i.smooth_scroll_delta.x, i.modifiers.shift));
        if !over_floating && !over_tabs {
            // `WRAP off`: a horizontal wheel/trackpad swipe, or Shift+wheel, PANS horizontally (and then
            // owns the scroll — no vertical move). The raster clamps the upper bound to the content width.
            if !self.editor.wrap() && (hwheel.abs() > 0.0 || (shift && wheel.abs() > 0.0)) {
                let d = if hwheel.abs() > 0.0 { hwheel } else { wheel };
                self.pane_view.scroll_h = (self.pane_view.scroll_h - d * self.render_scale).max(0.0);
                self.pane_view.follow = false;
            } else if wheel.abs() > 0.0 {
                // Scroll by pixels in VISUAL space; the next shape folds the offset into
                // (line, residual) and clamps at the buffer ends. Wheel up (positive y)
                // scrolls the content up, i.e. moves the view toward the top.
                self.pane_view.scroll_vertical -= wheel * self.render_scale; // wheel is points; scroll is pixels
            }
        }

        // --- Command line panel (the editor's *native* CMline; not an egui text
        //     widget, so Enter can never leak a newline). Placement-aware:
        //     Top = reserved line below the menu; Bottom = reserved line above the
        //     status; Hidden = shown transiently at the top only while focused.
        //     When unfocused it keeps the last-submitted command on screen. ---
        let cmline_focused = self.editor.cmline_open();
        let placement = self.editor.special_str("CMPLACE"); // "top" | "bottom" | "hidden" (CMPLACE)
        let show_top = placement == "top" || (placement == "hidden" && cmline_focused);
        if show_top && !writer_view {
            // `cmline_row` applies focus + caret + selection itself; it returns whether the mouse
            // interacted so we can wake a repaint.
            let interacted =
                egui::Panel::top("cmdline_top").show_inside(ui, |ui| self.cmline_row(ui)).inner;
            if interacted {
                ctx.request_repaint();
            }
        }

        // Status line. While a change-verify walk is active, the prompt takes over;
        // a DIR/LS listing shows its own footer (base dir + live express-key).
        let mut plain_status = false; // true = ordinary status (not a prompt/listing footer)
        let mut cwd_len = 0usize; // byte length of the leading `$CWD` prefix in `status` (0 = none)
        let status = if self.quitting && self.editor.next_dirty_tab().is_some() {
            let name = self.editor.active_file_name().unwrap_or_else(|| "(untitled)".into());
            format!("Quit — save changes to {name}?  (y) save  (n) discard  (esc) cancel")
        } else if let Some(close) = self.editor.close_prompt() {
            close
        } else if let Some(open) = self.editor.open_prompt() {
            open
        } else if let Some(prompt) = self.editor.verify_prompt() {
            prompt
        } else if let Some(confirm) = self.editor.dir_delete_prompt() {
            confirm
        } else if let Some(footer) = self.editor.dir_footer() {
            // A DIR/LS listing shows its footer here — but a transient status or ERROR message must
            // still surface over it (otherwise command feedback and errors were invisible whenever the
            // active tab was a listing). Append it, so the footer's base-dir + express-key context is
            // kept alongside the message.
            if self.status.is_empty() {
                footer
            } else {
                format!("{footer}  —  {}", self.status)
            }
        } else {
            plain_status = true;
            let doc = self.editor.document();
            let name = self.editor.active_file_name().unwrap_or_else(|| "(untitled)".into());
            let ime = if self.preedit.is_empty() {
                String::new()
            } else {
                format!("  IME: {}", self.preedit)
            };
            let msg = if self.status.is_empty() {
                String::new()
            } else {
                format!("  —  {}", self.status)
            };
            let flags = format!(
                "{}{}",
                if self.editor.modified() { " [+]" } else { "" },
                if self.editor.read_only() { " [RO]" } else { "" },
            );
            // The `$CWD` path shows here (PAPLACE status, the default) or as a command-line prefix
            // (PAPLACE cmline); PATHLEN caps its width. When it's on the cmline, drop it here.
            let cwd_prefix = if self.editor.special_str("PAPLACE") == "cmline" {
                String::new()
            } else {
                let plen: usize = self.editor.special_str("PATHLEN").parse().unwrap_or(45);
                format!("{}  │  ", truncate_path(&self.editor.cwd(), plen))
            };
            cwd_len = cwd_prefix.len(); // colour just this leading span (PAFGCOLOR/PABGCOLOR)
            format!(
                "{cwd_prefix}{name}{flags}  {}:{}  {}{ime}{msg}",
                doc.line(),
                doc.column(),
                doc.encoding_name(),
            )
        };
        // In Hidden placement there's no command-line row to click; let the always-visible status
        // line be the mouse affordance that opens it. A click isn't a key, so this works under an
        // empty keymap (`CLEARKBD`) without putting a binding outside the KBD-file model.
        let status_opens_cmline =
            plain_status && placement == "hidden" && !self.editor.cmline_open();
        if !writer_view {
        egui::Panel::bottom("status").show_inside(ui, |ui| {
            if status.contains('\n') {
                // Multiline status (e.g. `PR $CMH` or any PR output with real newlines): stack the
                // lines as a plain label so the bottom panel auto-sizes (grows UPWARD) to fit them.
                // (A ScrollArea here reports only a small default size during the panel's measure
                // pass, so the panel never grows — hence a plain label.) Cap at MAX rows so a big
                // dump can't swallow the editor; the overflow collapses into a "(+N more)" tail —
                // use `PRINTEX`/a buffer for the full text. Top line shows first (newest-first
                // content like `$CMH` reads from the top).
                const MAX_STATUS_ROWS: usize = 10;
                let total = status.lines().count();
                let shown = if total > MAX_STATUS_ROWS {
                    let head = status.lines().take(MAX_STATUS_ROWS - 1).collect::<Vec<_>>().join("\n");
                    format!("{head}\n… (+{} more)", total - (MAX_STATUS_ROWS - 1))
                } else {
                    status.clone()
                };
                let wt = self.status_text(ui, &shown, cwd_len);
                ui.add(egui::Label::new(wt).wrap()); // wrap long individual lines too
            } else if status_opens_cmline {
                // `.wrap()` (not inside `ui.horizontal`, which would force no-wrap `Extend`): a long
                // single-line message wraps to the panel width and grows it upward, instead of
                // scrolling off the right edge.
                let wt = self.status_text(ui, &status, cwd_len);
                let r = ui
                    .add(egui::Label::new(wt).wrap().sense(egui::Sense::click()))
                    .on_hover_text("Click to open the command line");
                if r.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
                }
                if r.clicked() {
                    self.editor.open_cmdline();
                    ctx.request_repaint();
                }
            } else {
                let wt = self.status_text(ui, &status, cwd_len);
                ui.add(egui::Label::new(wt).wrap());
            }
        });

        // Bottom-placed command line sits just above the status line (shown after
        // it, so the status stays at the very bottom edge).
        if placement == "bottom" {
            let interacted = egui::Panel::bottom("cmdline_bottom")
                .show_inside(ui, |ui| self.cmline_row(ui))
                .inner;
            if interacted {
                ctx.request_repaint();
            }
        }
        } // Writer's View: end of the hidden status + bottom-command-line block

        // Pop-up command line (`$GC`, SGC/CGC/TGC): a prominent box mirroring the same native
        // CMline, shown *in addition* to the inline line while focused. Clicking it places the
        // cmline caret just like the inline row (typing is still keymap-driven to the cmline).
        // A resizable/movable `Window` so the user can drag its dimensions; egui remembers the
        // geometry for the session (`UISCALE` scales its font + frame, both in egui points).
        if self.editor.gui_cmline() && cmline_focused {
            let label = self.cmline_label();
            // Size the pop-up text off the chrome font (`UIFONT`) with a modest prominence bump,
            // so it stays proportionate to the rest of the UI and tracks the font setting.
            let popup_font = self.editor.chrome_font() * 1.3;
            let rect = ui.ctx().content_rect();
            let default_w = (rect.width() * 0.8).clamp(360.0, 1200.0);
            let mut interacted = false;
            let cmline_has_sel = self.editor.command_line().has_selection();
            let cmline_clip_empty = self.editor.clipboard().is_empty();
            let mut ctx_cmd: Option<&'static str> = None;
            // No native title bar — egui hard-centers window titles. We render our own
            // left-justified "Command:" header instead. The window stays movable (a movable
            // Area drags from its non-interactive body) and resizable.
            egui::Window::new("Command:") // title text hidden; still the window's stable label
                .id(egui::Id::new("cmline_popup"))
                .title_bar(false)
                .collapsible(false)
                .resizable(true)
                .default_size(egui::vec2(default_w, 110.0))
                .default_pos(egui::pos2(rect.left() + 12.0, rect.top() + 80.0))
                .show(ui.ctx(), |ui| {
                    // Make the content fill the window's width (and a min height) so the window
                    // holds its size instead of collapsing to the short label — otherwise egui
                    // auto-shrinks to content each frame and the resize drag never "takes".
                    let w = ui.available_width();
                    ui.set_width(w);
                    ui.set_min_height(60.0);
                    ui.label(egui::RichText::new("Command:").strong()); // left-justified header
                    ui.separator();
                    // Wrap width the Label will use — build a matching galley for hit-testing and
                    // caret/selection geometry (the pop-up text wraps, so positions come from the
                    // galley by CCursor, unlike the single-line inline row).
                    let wrap_w = ui.available_width();
                    let font = egui::FontId::new(popup_font, egui::FontFamily::Monospace);
                    // `.selectable(false)`: the only selection is the cmline buffer's (drawn below),
                    // so mouse drag-select feeds `CUT`/`COPY` just like the inline row.
                    let resp = ui.add(
                        egui::Label::new(
                            egui::RichText::new(label.clone()).monospace().size(popup_font),
                        )
                        .selectable(false)
                        .wrap()
                        .sense(egui::Sense::click_and_drag()),
                    );
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
                    }
                    let galley =
                        ui.painter().layout(label.clone(), font.clone(), egui::Color32::WHITE, wrap_w);
                    let cell =
                        ui.painter().layout_no_wrap("0".to_string(), font, egui::Color32::WHITE).rect.width();
                    let origin = resp.rect.min.to_vec2();
                    // Selection band (per wrapped row) + caret overlay.
                    {
                        let painter = ui.painter().clone();
                        let cl = self.editor.command_line();
                        let text = cl.text();
                        let disp_ci =
                            |b: usize| 2 + text[..b.min(text.len())].chars().count(); // ": " prefix
                        if cl.has_selection() {
                            if let Some(anc) = cl.anchor() {
                                let a = cl.decoded_offset(anc).min(text.len());
                                let c = cl.decoded_offset(cl.cursor()).min(text.len());
                                let (sb, eb) = (a.min(c), a.max(c));
                                let sr = galley.pos_from_cursor(egui::text::CCursor::new(disp_ci(sb)));
                                let er = galley.pos_from_cursor(egui::text::CCursor::new(disp_ci(eb)));
                                let (selr, selg, selb) = self.editor.sel_bg_color();
                                let selcol =
                                    egui::Color32::from_rgb(selr, selg, selb).gamma_multiply(0.5);
                                // Paint one band per row the selection touches; a middle row fills
                                // edge-to-edge, the start/end rows clip to the caret x.
                                for prow in &galley.rows {
                                    let rr = prow.rect();
                                    if rr.bottom() <= sr.top() || rr.top() >= er.bottom() {
                                        continue;
                                    }
                                    let on = |y: f32| rr.top() <= y && y <= rr.bottom();
                                    let x0 = if on(sr.center().y) { sr.left() } else { rr.left() };
                                    let x1 = if on(er.center().y) { er.left() } else { rr.right() };
                                    let band = egui::Rect::from_min_max(
                                        egui::pos2(x0, rr.top()),
                                        egui::pos2(x1, rr.bottom()),
                                    )
                                    .translate(origin);
                                    painter.rect_filled(band, 0.0, selcol);
                                }
                            }
                        }
                        let cur = cl.decoded_offset(cl.cursor()).min(text.len());
                        let cg_rect = galley
                            .pos_from_cursor(egui::text::CCursor::new(disp_ci(cur)))
                            .translate(origin);
                        let (ccr, ccg, ccb) = self.editor.cmline_caret_color();
                        let ccol = egui::Color32::from_rgb(ccr, ccg, ccb);
                        let bar = 2.0_f32;
                        let (l, t, b) = (cg_rect.left(), cg_rect.top(), cg_rect.bottom());
                        let shape = self.editor.cmline_caret_shape();
                        let caret = match shape {
                            CaretShape::Bar => {
                                egui::Rect::from_min_max(egui::pos2(l, t), egui::pos2(l + bar, b))
                            }
                            CaretShape::Underline => {
                                egui::Rect::from_min_max(egui::pos2(l, b - bar), egui::pos2(l + cell, b))
                            }
                            CaretShape::Block => {
                                egui::Rect::from_min_max(egui::pos2(l, t), egui::pos2(l + cell, b))
                            }
                        };
                        let fill = if matches!(shape, CaretShape::Block) {
                            ccol.gamma_multiply(0.45)
                        } else {
                            ccol
                        };
                        painter.rect_filled(caret, 0.0, fill);
                    }
                    // Interaction: click places the caret; drag selects (same buffer as the inline
                    // row, so `cmline_drag_anchor` is shared). Focus is already the cmline here.
                    let off_at = |g: &egui::Galley, pos: egui::Pos2, bias: f32| -> usize {
                        let mut rel = pos - resp.rect.min;
                        rel.x = (rel.x - bias).max(0.0);
                        g.cursor_from_pos(rel).index
                    };
                    if resp.drag_started() {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            let anchor =
                                self.cmline_display_index_to_rope(off_at(&galley, pos, 0.5 * cell));
                            self.cmline_drag_anchor = Some(anchor);
                            self.editor.click_cmline(anchor);
                            interacted = true;
                        }
                    } else if resp.dragged() {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            let cursor =
                                self.cmline_display_index_to_rope(off_at(&galley, pos, 0.0));
                            match self.cmline_drag_anchor {
                                Some(a) if a != cursor => self.editor.select_cmline(cursor, a),
                                _ => self.editor.click_cmline(cursor),
                            }
                            interacted = true;
                        }
                    } else if resp.clicked() {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            let off = self.cmline_display_index_to_rope(off_at(&galley, pos, 0.0));
                            self.editor.click_cmline(off);
                            self.cmline_drag_anchor = None;
                            interacted = true;
                        }
                    }
                    if cmline_has_sel {
                        resp.context_menu(|ui| clipboard_context_menu(ui, &mut ctx_cmd, false));
                    } else {
                        if resp.secondary_clicked() {
                            if let Some(pos) = resp.interact_pointer_pos() {
                                let off =
                                    self.cmline_display_index_to_rope(off_at(&galley, pos, 0.0));
                                self.editor.click_cmline(off);
                            }
                        }
                        resp.context_menu(|ui| {
                            no_selection_context_menu(ui, &mut ctx_cmd, cmline_clip_empty, false)
                        });
                    }
                });
            if let Some(cmd) = ctx_cmd {
                // Clipboard verbs run as commands; the "Select …" verbs define directly on the
                // cmline (as commands they're focus-doc verbs → they'd define in the document).
                match cmd {
                    "DW" => self.editor.cmline_define_word(),
                    "DL" | "DP" => self.editor.cmline_define_line(),
                    _ => self.editor.run_command(cmd), // cmline is focused whenever the pop-up shows
                }
                interacted = true;
            }
            if interacted {
                ctx.request_repaint();
            }
        }

        // INPUT prompt: a script parked on INPUT shows a centred modal box with the prompt
        // and the line being typed (a `│` caret marks the end). Keystrokes are handled in the
        // `awaiting_line` branch above — this is display only. Mirrors the cmline pop-up.
        if self.editor.awaiting_line() {
            let prompt = self.editor.input_prompt().unwrap_or_else(|| "Input".to_string());
            // Colour the prompt with UIFGCOLOR. `.strong()` on its own uses egui's DEFAULT strong-text
            // colour (it ignores `override_text_color`), which can go low-contrast / invisible against a
            // themed (UIBGCOLOR) window background — the reported bug.
            let ui_fg = {
                let (r, g, b) = self.editor.ui_fg_color();
                egui::Color32::from_rgb(r, g, b)
            };
            let popup_font = self.editor.chrome_font() * 1.3;
            let rect = ui.ctx().content_rect();
            let default_w = (rect.width() * 0.6).clamp(320.0, 900.0);
            egui::Window::new("Input")
                .id(egui::Id::new("input_prompt"))
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -40.0))
                .default_size(egui::vec2(default_w, 90.0))
                .show(ui.ctx(), |ui| {
                    let w = ui.available_width().max(default_w);
                    ui.set_width(w);
                    ui.label(egui::RichText::new(prompt).strong().color(ui_fg));
                    ui.separator();
                    let shown = format!("{}\u{2502}", self.input_line); // trailing │ caret
                    ui.add(
                        egui::Label::new(egui::RichText::new(shown).monospace().size(popup_font))
                            .wrap(),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("Enter to accept · Esc to cancel").weak().small(),
                    );
                });
        }

        // READKEY message: a script parked on `READKEY <var> <message>` shows the message in a
        // centred modal box while it waits for a single keystroke (captured raw in `runtime.rs`;
        // the box vanishes once the key resumes the script). A bare `READKEY` (no message) shows
        // nothing and captures the key silently, as before. Display only.
        if self.editor.awaiting_key() {
            if let Some(message) = self.editor.input_prompt() {
                let popup_font = self.editor.chrome_font() * 1.3;
                let rect = ui.ctx().content_rect();
                let default_w = (rect.width() * 0.6).clamp(320.0, 900.0);
                egui::Window::new("Key")
                    .id(egui::Id::new("readkey_prompt"))
                    .title_bar(false)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -40.0))
                    .default_size(egui::vec2(default_w, 90.0))
                    .show(ui.ctx(), |ui| {
                        let w = ui.available_width().max(default_w);
                        ui.set_width(w);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(message).monospace().size(popup_font),
                            )
                            .wrap(),
                        );
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new("Press a key…").weak().small());
                    });
            }
        }

        // Preview pane (right): debounced raster of the active document, rendered per its DOCTYPE
        // (Typst compiled verbatim, GFM translated). Visibility is host-owned (PV/$PV). While hidden,
        // the whole closure is skipped — no compile, no debounce timer, no repaint request.
        let preview_on = self.editor.preview_visible();
        if !preview_on && self.preview_was_visible {
            // Visible → hidden: drop the cache so re-showing renders fresh, and do no preview work.
            self.preview_tex = None;
            self.preview_src.clear();
            self.last_text.clear();
            self.preview_err = None;
        }
        self.preview_was_visible = preview_on;
        if preview_on {
        let kind = self.editor.active_render_kind();
        // Authored typst styles itself; GFM and HTML both take the user's Preview font settings.
        let is_typst = matches!(kind, RenderKind::Typst);
        egui::Panel::right("preview").resizable(true).default_size(380.0).show_inside(ui, |ui| {
            ui.label(match kind {
                RenderKind::Typst => "preview — typst",
                RenderKind::Gfm => "preview — gfm",
                RenderKind::Html => "preview — html",
            });
            ui.separator();
            let pane_w = ui.available_size().x.max(50.0);
            let now = ctx.input(|i| i.time);
            let cur = self.editor.document().text();
            // Restart the debounce on a text edit OR a pane-width change (a resize, or the panel
            // settling to its real width on the first frames after PV is shown — that first-frame
            // width was the "thumbnail" bug).
            if cur != self.last_text || (pane_w - self.preview_seen_w).abs() > 1.0 {
                self.last_text = cur.clone();
                self.last_change_time = now;
            }
            self.preview_seen_w = pane_w;
            // The user-chosen Preview/TOPDF font (GFM only). A change re-rasters immediately (its
            // signature is part of the staleness test), so switching fonts shows right away.
            let fonts = self.editor.host().preview_fonts();
            let m = &fonts.margins;
            let font_sig = format!(
                "{:?}|{:?}|{:?}|{}|{}|{}|{}",
                fonts.family, fonts.size, fonts.code, m.left, m.right, m.top, m.bottom
            );
            let font_stale = !is_typst && font_sig != self.preview_font_sig;
            let kind_changed = kind != self.preview_was_kind;
            // The cached texture is stale if the text, the render kind, the chosen font, OR the width
            // it was rendered at changed — so resizing the pane (or its first-frame settle) re-rasters.
            let width_stale = (pane_w - self.preview_pane_w).abs() > 2.0;
            if cur != self.preview_src || kind_changed || width_stale || font_stale {
                if now - self.last_change_time >= 0.4 || kind_changed || font_stale {
                    let scale = (pane_w / 595.0).clamp(0.4, 3.0); // page width ≈ 595 pt
                    let doc_dir = self.editor.host().typst_doc_dir(); // roots #image/#read resolution
                    let res = match kind {
                        RenderKind::Typst => {
                            // Splice MDA styling into the authored-Typst preview too (wraps only prose,
                            // so the document's own Typst markup is untouched).
                            let styler = self.editor.host().mda_styler(&cur);
                            xyscript_app::typst_to_image(&cur, &doc_dir, scale, Some(&styler))
                        }
                        RenderKind::Gfm => {
                            // Splice the document's MDA styling into the preview so it matches the
                            // editor canvas (bold/italic/underline/colour/reverse).
                            let styler = self.editor.host().mda_styler(&cur);
                            xyscript_app::gfm_to_image(&cur, &doc_dir, scale, &fonts, Some(&styler))
                        }
                        RenderKind::Html => {
                            xyscript_app::html_to_image(&cur, &doc_dir, scale, &fonts)
                        }
                    };
                    match res {
                        Ok((rgba, w, h)) => {
                            // Crop so a tall page at high zoom can't exceed the GPU max texture side.
                            let max = ctx.input(|i| i.max_texture_side).max(256);
                            let (rgba, cw, ch) = clamp_rgba(rgba, w as usize, h as usize, max);
                            let img = egui::ColorImage::from_rgba_premultiplied([cw, ch], &rgba);
                            self.preview_tex = Some(ctx.load_texture("preview", img, Default::default()));
                            self.preview_err = None;
                        }
                        // Keep the last good image; show the compile error (line:col from Phase 2).
                        Err(e) => self.preview_err = Some(e),
                    }
                    self.preview_src = cur;
                    self.preview_was_kind = kind;
                    self.preview_font_sig = font_sig;
                    self.preview_pane_w = pane_w;
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_millis(400));
                }
            }
            if let Some(err) = &self.preview_err {
                ui.colored_label(egui::Color32::from_rgb(200, 60, 60), err);
                ui.separator();
            }
            // Follow the editor: when the editor's viewport top line changes, scroll the preview to
            // the same fraction of the document (proportional by logical line — typst reflow makes an
            // exact line→y map impractical, but it tracks as you scroll). Only forced ON CHANGE, so
            // when the text is still the preview can also be scrolled independently.
            let total_lines = self.editor.document().line_count().max(1);
            let mut scroll = egui::ScrollArea::both();
            if self.pane_view.scroll_line != self.preview_synced_line {
                self.preview_synced_line = self.pane_view.scroll_line;
                let frac = (self.pane_view.scroll_line as f32 / total_lines as f32).clamp(0.0, 1.0);
                let tex_h = self.preview_tex.as_ref().map(|t| t.size_vec2().y).unwrap_or(0.0);
                scroll = scroll.vertical_scroll_offset(frac * tex_h);
            }
            scroll.show(ui, |ui| {
                if let Some(t) = &self.preview_tex {
                    ui.image((t.id(), t.size_vec2()));
                } else if self.preview_err.is_none() {
                    ui.label("(rendering…)");
                }
            });
        });
        }

        // Editing canvas, with an optional line-number gutter (`$LNS`) and left margin (`OF`,
        // in char columns) to its left: layout is [gutter][margin][text]. The canvas is
        // rendered at the REDUCED width and offset right by gutter+margin, so `resp.rect`
        // carries the offset and hit-testing/caret/IME stay automatic.
        let size = ui.available_size();
        let cell_w = self.font_size * 0.6; // approx monospace advance
        // Split panes: with more than one pane the canvas fills the whole area and the panes divide
        // it; the line-number gutter + scrollbar overlays are single-pane only for now (they auto-skip
        // because their reserved widths go to 0 below).
        let multi = self.editor.host().pane_count() > 1;
        let focused_pane = self.editor.host().focused_pane_id();
        // Detect a focus change (a `TW` verb, or a click in another pane last frame) and swap the live
        // viewport: stash the outgoing pane's `pane_view`, load the incoming pane's (default = top of
        // the doc for a freshly-split pane). The first frame just adopts the current pane_view.
        if self.last_focus != Some(focused_pane) {
            if let Some(old) = self.last_focus {
                self.pane_views.insert(old, self.pane_view.clone());
                self.pane_view = self.pane_views.remove(&focused_pane).unwrap_or_default();
            }
            self.last_focus = Some(focused_pane);
        }
        // SINGLE PANE: remember scroll PER DOCUMENT. On a tab/buffer switch, stash the outgoing doc's
        // viewport and load the incoming doc's — so returning to a file lands where you left it instead
        // of snapping to its caret (`follow`). A doc not yet viewed follows the caret (top). Skipped in a
        // split, where `pane_views` already keeps per-pane scroll (above).
        if !multi {
            let cur_doc = self.editor.active_doc_id();
            if self.last_active_doc != Some(cur_doc) {
                if let Some(old) = self.last_active_doc {
                    self.doc_scroll.insert(old, self.pane_view.clone());
                    self.pane_view = match self.doc_scroll.remove(&cur_doc) {
                        Some(mut v) => {
                            v.follow = false; // honour the saved scroll, don't jump to the caret
                            v
                        }
                        None => PaneView { follow: true, ..PaneView::default() },
                    };
                }
                self.last_active_doc = Some(cur_doc);
            }
        }
        let sb_w = if multi { 0.0 } else { 12.0_f32 }; // vertical scrollbar strip at the far right
        let gutter_w = if self.editor.line_numbers() && !multi {
            let digits = self.editor.document().line_count().max(1).to_string().len();
            (digits as f32 * cell_w + 12.0).ceil()
        } else {
            0.0
        };
        // A thin strip at the far left for bookmark glyphs — reserved when the doc HAS bookmarks (single
        // pane), so layout is unchanged otherwise. With `MARKGUTTER persist` it is reserved always (even
        // with no bookmarks), giving a stable left margin. Sits left of the line-number gutter.
        let mark_w = if !multi
            && ((self.editor.mark_gutter() && self.editor.has_marks())
                || self.editor.mark_gutter_persist())
        {
            (cell_w * 1.4).ceil()
        } else {
            0.0
        };
        let margin_l = if multi { 0.0 } else { (self.editor.margin_left() * cell_w).round() };
        let margin_r = if multi { 0.0 } else { (self.editor.margin_right() * cell_w).round() };
        let left = mark_w + gutter_w + margin_l;
        // `WRAP off` reserves a bottom strip (single pane) for the horizontal scrollbar, mirroring the
        // vertical strip's `sb_w`. Zero while wrapping, so the layout is unchanged there.
        let hb_h = if !multi && !self.editor.wrap() { 12.0_f32 } else { 0.0 };
        let w = (((size.x - left - margin_r - sb_w).max(1.0)) as usize).clamp(1, 4000);
        let h = (((size.y - hb_h).max(1.0)) as usize).clamp(1, 4000);
        // `MU`/`MD` viewport scroll (caret unmoved): shift the scroll offset by N visual rows
        // before rendering; cosmic folds it wrap-aware and clamps on the next shape. `follow`
        // off so the render doesn't snap back to the caret.
        let scroll_rows = self.editor.take_scroll_rows();
        if scroll_rows != 0 {
            let lh_px = self.line_height * self.render_scale;
            self.pane_view.scroll_vertical += scroll_rows as f32 * lh_px;
            self.pane_view.follow = false;
        }
        // `LB`/`LE` visual-line-edge refine: the engine moved to the logical edge as a fallback;
        // refine it to the true visual-row edge via cosmic (using the pre-move caret to find the
        // row). Uses last frame's `last_canvas`, like `visual_vmove`.
        if let Some((end, extend, from)) = self.editor.take_visual_edge() {
            self.visual_line_edge(end, extend, from);
        }
        // `CARETMOTION visual` Left/Right refine (mirror of the visual-line-edge refine above).
        if let Some((right, extend, from)) = self.editor.take_visual_move() {
            self.visual_char_move(right, extend, from);
        }
        // A search just found a match → center it this frame (the follow logic reads `center_caret`).
        if self.editor.take_reveal_center() {
            self.center_caret = true;
            self.pane_view.follow = true;
        }
        // Refresh the FIND "highlight all" set for the active doc BEFORE painting the canvas. The Find
        // window is a separate OS window on its own render schedule, so relying on it alone made a tab
        // switch show the file a frame (or more) before its hits reappeared. Key-cached → a no-op unless
        // the active document (or the Find text/options) changed, so it only recomputes on a switch/edit.
        if self.find_dialog.is_some() {
            self.refresh_find_highlights();
        }
        let img = if !multi && self.editor.hex_view() {
            self.render_hex_canvas(w, h)
        } else if multi {
            self.render_canvas_panes(w, h)
        } else {
            self.render_canvas(w, h)
        };
        // Publish the settled viewport top line so `$CY` (cursor's row in the viewport) reads it.
        self.editor.host_mut().set_viewport_top(self.pane_view.scroll_line);
        let tex = match &mut self.canvas_tex {
            Some(t) => {
                t.set(img, egui::TextureOptions::default());
                t.clone()
            }
            None => {
                let t = ctx.load_texture("canvas", img, egui::TextureOptions::default());
                self.canvas_tex = Some(t.clone());
                t
            }
        };
        let resp = ui
            .horizontal(|ui| {
                if left > 0.0 {
                    ui.add_space(left);
                }
                // Display at the LOGICAL size (the texture is rastered at device pixels, so this
                // maps it 1:1 to physical pixels → crisp, instead of egui upscaling a point image).
                ui.image((tex.id(), egui::vec2(w as f32, h as f32)))
                    .interact(egui::Sense::click_and_drag())
            })
            .inner;
        // The focused pane's on-screen rect (points): the whole image for a single pane, else the
        // focused sub-rect. Overlays, caret/IME, and hit-testing use THIS (the last-rendered pane's
        // window + globals describe the focused pane), so a click lands on the focused pane's text.
        let canvas_rect = if multi {
            // Text area = the focused pane inset by the OF offsets (`OFL` left, `OFR` right) and its
            // scrollbar strip — matching where `render_canvas_panes` blits the text.
            let cell_w = self.font_size * 0.6;
            let ml = (self.editor.margin_left() * cell_w).round();
            let mr = (self.editor.margin_right() * cell_w).round();
            self.editor
                .host()
                .pane_layout(w as f32, h as f32)
                .into_iter()
                .find(|(p, _, _)| *p == focused_pane)
                .map(|(_, _, r)| {
                    egui::Rect::from_min_size(
                        resp.rect.min + egui::vec2(r.x + ml, r.y),
                        egui::vec2((r.w - ml - mr - PANE_SB_W).max(1.0), r.h),
                    )
                })
                .unwrap_or(resp.rect)
        } else {
            resp.rect
        };
        // Apply a deferred click from LAST frame that just focused this pane: now that it's the focused
        // pane (its window + `canvas_rect` are current this frame), drop the caret at the click point —
        // so a single click on a non-focused pane both focuses it and positions the caret. Runs before
        // the mouse block below sets a NEW pending click, so it can't consume its own set.
        if let Some(cp) = self.pending_pane_click.take() {
            if canvas_rect.contains(cp) {
                let s = self.render_scale;
                let rel = cp - canvas_rect.min;
                if let Some(off) = self.hit(rel.x * s, rel.y * s) {
                    if self.editor.cmline_open() {
                        self.editor.focus_text();
                    }
                    self.editor.host_mut().click_active_caret(off);
                    ctx.request_repaint();
                }
            }
        }
        // Right-click context menu on the canvas (skipped on a DIR/LS listing, where right-click
        // already copies the entry's path). With a selection → the Edit-menu clipboard block; with
        // NONE → Paste + Select word/line/sentence/paragraph, having first dropped the caret at the click
        // point so the "define" verbs target what was clicked. Runs text-focused so it operates on
        // the document even if the command line currently holds focus.
        if !self.editor.active_is_dir_listing() {
            enum SpellPick {
                Fix(usize),
                Learn,
                Check,
            }
            // Detect the right-click ourselves: `secondary_clicked()` on a `click_and_drag`-sensed
            // widget is FALSE if the press-to-release moved even a pixel or two (egui reads it as a
            // drag), which made the menu "hit or miss". A secondary-button RELEASE while the canvas is
            // hovered is robust to that micro-movement.
            let right_click = resp.secondary_clicked()
                || (resp.hovered()
                    && ctx.input(|i| i.pointer.button_released(egui::PointerButton::Secondary)));
            // On a right-click, decide whether it landed on a flagged (spelling) word.
            if right_click {
                self.spell_menu = None;
                self.spell_menu_pos = None;
                // `secondary_clicked()` fires on RELEASE, where `interact_pointer_pos()` can be
                // `None` — fall back to the hover position.
                if let Some(p) = resp.interact_pointer_pos().or_else(|| resp.hover_pos()) {
                    let rel = p - canvas_rect.min; // focused pane's local space
                    if rel.x >= 0.0 && rel.y >= 0.0 && (!multi || canvas_rect.contains(p)) {
                        let s = self.render_scale;
                        // The CONTAINING glyph (`hit_leading`) so a click anywhere on the word lands
                        // inside its span.
                        let word_off = self.hit_leading(rel.x * s, rel.y * s);
                        // Any underlined issue (spelling OR grammar) → explanation + suggestions.
                        self.spell_menu = word_off.and_then(|o| self.editor.lint_explain_at(o));
                        if self.spell_menu.is_some() {
                            self.spell_menu_pos = Some(p); // open the spell popup here
                            self.spell_menu_frames = 0; // reset the settle guard
                        } else if !self.editor.document().has_selection() {
                            // Not a flagged word + no selection → drop the caret at the click.
                            if let Some(off) = self.hit(rel.x * s, rel.y * s) {
                                if self.editor.cmline_open() {
                                    self.editor.focus_text();
                                }
                                self.editor.host_mut().click_active_caret(off);
                            }
                        }
                    }
                }
            }

            // The SPELL menu: a MANUAL popup (egui's `context_menu` auto-open was unreliable on this
            // drag-sensed canvas — it opened then self-dismissed on the opening click, hence "2-3
            // clicks"). Rendered while `spell_menu` is set; dismissed on pick / Esc / click-outside.
            if self.spell_menu.is_some() {
                self.spell_menu_frames = self.spell_menu_frames.saturating_add(1);
                let pos = self.spell_menu_pos.unwrap_or_else(|| resp.rect.center());
                let mut pick: Option<SpellPick> = None;
                // Frameless full-width rows = clean menu items (highlight only on hover); plain
                // `ui.button` renders a boxed/filled button that reads as "highlighted".
                let item = |ui: &mut egui::Ui, w: f32, label: &str| -> bool {
                    ui.add_sized(
                        [w, ui.spacing().interact_size.y],
                        egui::Button::new(label).frame(false),
                    )
                    .clicked()
                };
                let area = egui::Area::new(egui::Id::new("xy_spell_menu"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(pos)
                    .constrain(true)
                    .show(&ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            let cx = self.spell_menu.as_ref().unwrap();
                            // Width = the WIDEST item's text (measured, no wrap) + button padding, so
                            // nothing wraps — e.g. "Check whole document" or a long suggestion.
                            let font = egui::TextStyle::Button.resolve(ui.style());
                            let mut text_w = 0.0f32;
                            for s in cx
                                .labels
                                .iter()
                                .map(String::as_str)
                                .chain(["Add to dictionary", "Check whole document"])
                            {
                                let w = ui
                                    .painter()
                                    .layout_no_wrap(s.to_owned(), font.clone(), egui::Color32::WHITE)
                                    .size()
                                    .x;
                                text_w = text_w.max(w);
                            }
                            // Cap width so a long explanation wraps instead of stretching the popup.
                            let item_w = (text_w + 2.0 * ui.spacing().button_padding.x + 8.0).min(360.0);
                            ui.set_width(item_w);
                            // The issue's EXPLANATION at the top (Harper's description) — a wrapped,
                            // non-clickable header. Empty for a plain Hunspell misspelling.
                            if !cx.message.is_empty() {
                                ui.add(egui::Label::new(egui::RichText::new(&cx.message).italics()).wrap());
                                ui.separator();
                            }
                            for (i, label) in cx.labels.iter().enumerate() {
                                if item(ui, item_w, label) {
                                    pick = Some(SpellPick::Fix(i));
                                }
                            }
                            if !cx.labels.is_empty() {
                                ui.separator();
                            }
                            // "Add to dictionary" only makes sense for a misspelled WORD, not a grammar span.
                            if cx.is_spelling && item(ui, item_w, "Add to dictionary") {
                                pick = Some(SpellPick::Learn);
                            }
                            if item(ui, item_w, "Check whole document") {
                                pick = Some(SpellPick::Check);
                            }
                        });
                    });
                // Dismiss on Esc, or (once settled) a fresh pointer press that isn't over the popup.
                // `any_pressed` is a single-frame event (no lag), and the ≥3-frame guard skips the
                // opening right-click entirely, so the popup can't self-close on the click that opened it.
                // Use `contains_pointer()` (a GEOMETRIC hit-test of the popup rect), NOT `hovered()`:
                // an Area's own response reports `hovered() == false` when the pointer is over a child
                // button (the frameless items fill the popup), so a PRESS on an item was misread as a
                // click-outside and dismissed the menu before the button's click landed on release.
                let press_outside = self.spell_menu_frames >= 3
                    && ctx.input(|i| i.pointer.any_pressed())
                    && !area.response.contains_pointer();
                let dismiss = ctx.input(|i| i.key_pressed(egui::Key::Escape)) || press_outside;
                if let Some(pk) = pick {
                    if let Some(cx) = self.spell_menu.take() {
                        self.editor.focus_text();
                        match pk {
                            SpellPick::Fix(i) => {
                                self.editor.apply_lint_fix(cx.span, &cx.replacements[i])
                            }
                            SpellPick::Learn => {
                                let word = self.editor.document().slice(cx.span.0, cx.span.1);
                                self.editor.run_command(&format!("LEARN {}", word.trim()));
                            }
                            SpellPick::Check => self.editor.run_command("CHECK"),
                        }
                    }
                    self.spell_menu_pos = None;
                    ctx.request_repaint();
                } else if dismiss {
                    self.spell_menu = None;
                    self.spell_menu_pos = None;
                } else {
                    // Keep the (reactive) loop ticking while the popup is open, so the settle counter
                    // advances and the dismiss/hover checks stay live without needing input.
                    ctx.request_repaint_after(std::time::Duration::from_millis(30));
                }
            } else {
                // Not on a flagged word → the usual clipboard / select context menu (egui auto-open).
                let has_sel = self.editor.document().has_selection();
                let clip_empty = self.editor.clipboard().is_empty();
                let mut ctx_cmd: Option<&'static str> = None;
                resp.context_menu(|ui| {
                    if has_sel {
                        clipboard_context_menu(ui, &mut ctx_cmd, true);
                    } else {
                        no_selection_context_menu(ui, &mut ctx_cmd, clip_empty, true);
                    }
                });
                if let Some(cmd) = ctx_cmd {
                    self.editor.focus_text();
                    self.editor.run_command(cmd);
                    ctx.request_repaint();
                }
            }
        }
        // The OF offset margins follow the EDITOR background (`$BGCOLOR`) so the offset blends
        // with the text canvas; only the line-number gutter keeps the UI-chrome background
        // (`$UIBGCOLOR`, the panel fill). Paint the left/right margin strips with the editor bg.
        if margin_l > 0.0 || margin_r > 0.0 {
            let (br, bgc, bb) = self.editor.bg_color();
            let ed_bg = egui::Color32::from_rgb(br, bgc, bb);
            let painter = ui.painter();
            if margin_l > 0.0 {
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(resp.rect.min.x - margin_l, resp.rect.min.y),
                        egui::vec2(margin_l, resp.rect.height()),
                    ),
                    0.0,
                    ed_bg,
                );
            }
            if margin_r > 0.0 {
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(resp.rect.max.x, resp.rect.min.y),
                        egui::vec2(margin_r, resp.rect.height()),
                    ),
                    0.0,
                    ed_bg,
                );
            }
        }
        // Paint the line numbers in the gutter (right-aligned at the gutter's right edge,
        // i.e. left of the OF margin), aligned to each logical line's first visual row.
        if gutter_w > 0.0 {
            let painter = ui.painter();
            let color = egui::Color32::from_rgb(120, 130, 150);
            let s = self.render_scale; // gutter_lines y is in canvas pixels → back to points
            for &(line, y) in &self.gutter_lines {
                painter.text(
                    egui::pos2(resp.rect.min.x - margin_l - 6.0, resp.rect.min.y + y / s),
                    egui::Align2::RIGHT_TOP,
                    line.to_string(),
                    egui::FontId::monospace(self.font_size),
                    color,
                );
            }
        }
        // Bookmark glyphs: a filled dot in the reserved left strip, on each bookmarked line's first
        // visual row. A shape (not a font glyph) so it renders regardless of font coverage/theme. The
        // dot colour is MARKFGCOLOR (default bookmark orange); with MARKBGCOLOR set, the whole strip is
        // filled with that colour first (so the gutter can blend in or stand out).
        if mark_w > 0.0 {
            let painter = ui.painter();
            if let Some((r, g, b)) = self.editor.host().mark_bg_color() {
                let strip = egui::Rect::from_min_max(
                    egui::pos2(resp.rect.min.x - left, resp.rect.min.y),
                    egui::pos2(resp.rect.min.x - left + mark_w, resp.rect.max.y),
                );
                painter.rect_filled(strip, 0.0, egui::Color32::from_rgb(r, g, b));
            }
            let color = self
                .editor
                .host()
                .mark_fg_color()
                .map(|(r, g, b)| egui::Color32::from_rgb(r, g, b))
                .unwrap_or(egui::Color32::from_rgb(216, 118, 42)); // bookmark orange, reads in light + dark
            let s = self.render_scale; // mark_rows y is in canvas pixels → back to points
            let cx = resp.rect.min.x - left + mark_w * 0.5; // centre of the far-left strip
            let r = (self.font_size * 0.24).max(2.0);
            for &y in &self.mark_rows {
                let cy = resp.rect.min.y + y / s + self.line_height * 0.5;
                painter.circle_filled(egui::pos2(cx, cy), r, color);
            }
        }

        // Vertical scrollbar(s), sized + positioned by each document's VISUAL (wrapped-line) extent,
        // click/drag-to-scroll. Single pane → one strip at the far right; split → one per pane in its
        // own reserved strip (see `render_canvas_panes`). See [`draw_vscrollbar`].
        if !multi {
            let track = egui::Rect::from_min_size(
                egui::pos2(resp.rect.max.x + margin_r, resp.rect.min.y),
                egui::vec2(sb_w, resp.rect.height()),
            );
            let line_count = self.editor.document().line_count();
            let (wrap, lh, rs, fg) =
                (self.wrap_ratio, self.line_height, self.render_scale, self.editor.ui_fg_color());
            draw_vscrollbar(ui, 0, track, line_count, wrap, lh, rs, fg, &mut self.pane_view);
            // `WRAP off`: horizontal scrollbar along the reserved bottom strip, spanning the text width.
            if !self.editor.wrap() {
                let htrack = egui::Rect::from_min_size(
                    egui::pos2(resp.rect.min.x, resp.rect.max.y),
                    egui::vec2(resp.rect.width(), hb_h),
                );
                let (content_w, visible_w) = (self.pane_view.content_w, self.pane_view.canvas_px.0 as f32);
                draw_hscrollbar(ui, 0, htrack, content_w, visible_w, fg, &mut self.pane_view);
            }
        } else {
            let origin = resp.rect.min;
            let (lh, rs, fg) = (self.line_height, self.render_scale, self.editor.ui_fg_color());
            for (i, (pid, didx, r)) in
                self.editor.host().pane_layout(w as f32, h as f32).into_iter().enumerate()
            {
                let line_count = self.editor.doc(didx).line_count();
                let wrap = self.pane_wrap.get(&pid).copied().unwrap_or(1.0);
                let track = egui::Rect::from_min_size(
                    origin + egui::vec2(r.x + r.w - PANE_SB_W, r.y),
                    egui::vec2(PANE_SB_W, r.h),
                );
                // The focused pane's live view is `self.pane_view`; others live in the map. A drag on a
                // non-focused pane's scrollbar scrolls it WITHOUT focusing it (next frame's render reads
                // the updated view back).
                if pid == focused_pane {
                    draw_vscrollbar(ui, i as u32, track, line_count, wrap, lh, rs, fg, &mut self.pane_view);
                } else {
                    let mut v = self.pane_views.get(&pid).cloned().unwrap_or_default();
                    draw_vscrollbar(ui, i as u32, track, line_count, wrap, lh, rs, fg, &mut v);
                    self.pane_views.insert(pid, v);
                }
            }
        }

        // Mouse: a plain click places the caret (PS-aware — under persistent
        // selection it's navigation and keeps the block); a drag makes a fresh
        // selection.
        let click = resp.clicked();
        // A double-click in a DIR/LS listing opens/descends the clicked entry — the
        // mouse equivalent of Enter (matches the `DIR/W` window's behavior).
        let dir_double = resp.double_clicked() && self.editor.active_is_dir_listing();
        // A double-click on a GREP results tab opens the clicked hit (the mouse equivalent of Enter/GJ).
        let grep_double = resp.double_clicked() && self.editor.active_is_grep_listing();
        // A right-click in a DIR/LS listing copies the clicked entry's full path to the
        // clipboard (the mouse equivalent of the listing's Ctrl+C).
        let dir_rightclick = resp.secondary_clicked() && self.editor.active_is_dir_listing();
        // In a split, a press in a NON-focused pane focuses that pane (its doc becomes active). Precise
        // caret placement then happens on the next click, now that the pane is focused (its window is
        // what gets hit-tested). A press already inside the focused pane falls through to the caret
        // code below unchanged.
        if multi && (resp.clicked() || resp.drag_started() || resp.secondary_clicked()) {
            if let Some(p) = resp.interact_pointer_pos() {
                let rel = p - resp.rect.min; // points into the whole canvas
                for (pid, _didx, r) in self.editor.host().pane_layout(w as f32, h as f32) {
                    if rel.x >= r.x && rel.x < r.x + r.w && rel.y >= r.y && rel.y < r.y + r.h {
                        if pid != focused_pane {
                            self.editor.host_mut().focus_pane(pid);
                            // Place the caret on this click too, next frame (once the pane is focused
                            // and rendered, so its window is hit-testable) — no second click needed.
                            self.pending_pane_click = Some(p);
                            ctx.request_repaint();
                        }
                        break;
                    }
                }
            }
        }
        // Mouse caret placement is suspended during a change-verify walk (the walk
        // controls the caret/selection until it ends). In a split, only presses inside the FOCUSED
        // pane place/drag the caret (other panes are handled by the focus block above).
        if let Some(p) = resp
            .interact_pointer_pos()
            .filter(|_| !self.editor.verify_active())
            .filter(|p| !multi || canvas_rect.contains(*p))
        {
            // Clicking/dragging the text canvas focuses the text — but PRESERVES the
            // cmline (B23), unlike the old behavior which discarded it. Do this before
            // placing the caret so the caret lands on the document, not the parked cmline.
            if self.editor.cmline_open() {
                self.editor.focus_text();
            }
            // `rel` is in points relative to the focused pane; the canvas/hit-test work in pixels.
            let s = self.render_scale;
            let rel = p - canvas_rect.min;
            if resp.dragged() && !resp.drag_started() {
                // A CONTINUING selection drag. When the pointer reaches (or passes) the top/bottom
                // edge of the canvas, auto-scroll so the selection can extend beyond the visible
                // region. This runs UNGUARDED by the `rel >= 0` caret check, because at the edges
                // the pointer is usually outside the canvas rect (above the top / below the bottom).
                let edge = self.line_height.max(8.0); // edge-zone thickness in points
                let dir = if p.y > canvas_rect.max.y - edge {
                    1.0 // toward the bottom → reveal later content
                } else if p.y < canvas_rect.min.y + edge {
                    -1.0 // toward the top
                } else {
                    0.0
                };
                if dir != 0.0 {
                    // One row minimum, faster the further past the edge, capped so it stays usable.
                    let past = (if dir > 0.0 {
                        p.y - (canvas_rect.max.y - edge)
                    } else {
                        (canvas_rect.min.y + edge) - p.y
                    })
                    .max(0.0);
                    let step = (self.line_height + past * 0.6).min(self.line_height * 4.0);
                    self.pane_view.scroll_vertical += dir * step * s; // scroll_vertical is pixels
                    ctx.request_repaint(); // keep stepping while the button is held, even if the mouse is still
                }
                // Extend to the edge-most visible row: clamp the hit point into the canvas so a
                // pointer past the bottom lands on the last visible row (which advances as we scroll).
                let hx = rel.x.clamp(0.0, canvas_rect.width()) * s;
                let hy = rel.y.clamp(0.0, canvas_rect.height()) * s;
                if let Some((al, ac)) = self.box_anchor {
                    // Alt+drag box (column) select: rebuild a caret per line across the rectangle.
                    let (cl, cc) = self.grid_line_col(hx, hy);
                    self.editor.host_mut().set_box_active_carets(al, ac, cl, cc);
                    ctx.request_repaint();
                } else if let Some(off) = self.hit(hx, hy) {
                    self.goal_x = None;
                    let anchor = self.drag_anchor.filter(|&a| a != off);
                    self.editor.host_mut().set_active_caret(off, anchor);
                    ctx.request_repaint();
                }
            } else if rel.x >= 0.0 && rel.y >= 0.0 {
                if let Some(off) = self.hit(rel.x * s, rel.y * s) {
                    self.goal_x = None; // a click/drag drops the vertical goal column
                    if self.editor.hex_view() {
                        // HEX: place the caret on the EXACT byte (no char snap), switch the edit column
                        // to whichever the click landed in (hex pairs vs text), and reset the nibble.
                        self.hex_nibble_at = None;
                        let col = (rel.x * s / self.hex_cell_w.max(1.0)) as usize;
                        self.hex_text_col = col >= xyscript_app::hex::text_col(0);
                        if resp.drag_started() {
                            self.drag_anchor = Some(off); // start a byte-range drag-select
                        }
                        self.editor.hex_set_caret(off);
                    } else if dir_double {
                        // Land the caret on the clicked entry, then activate it like Enter.
                        self.editor.host_mut().click_active_caret(off);
                        self.editor.dir_enter();
                        self.pane_view.follow = true;
                    } else if grep_double {
                        // Land the caret on the clicked result, then jump to it (like Enter / GJ).
                        self.editor.host_mut().click_active_caret(off);
                        self.editor.grep_jump();
                        self.pane_view.follow = true;
                    } else if dir_rightclick {
                        // Land the caret on the clicked entry, then copy its full path.
                        self.editor.host_mut().click_active_caret(off);
                        self.editor.dir_copy_path();
                    } else if resp.drag_started() && ctx.input(|i| i.modifiers.alt) {
                        // Alt+drag: begin a BOX (column) select. Anchor the rectangle at the press-
                        // origin grid cell; the continuing-drag branch rebuilds a caret per line.
                        let press = ctx.input(|i| i.pointer.press_origin()).unwrap_or(p);
                        let prel = press - canvas_rect.min;
                        let (al, ac) = self.grid_line_col(prel.x * s, prel.y * s);
                        self.box_anchor = Some((al, ac));
                        self.drag_anchor = None;
                        self.editor.host_mut().set_box_active_carets(al, ac, al, ac);
                    } else if resp.drag_started() {
                        // Anchor on the pressed character so a drag that starts on it INCLUDES it,
                        // whichever way it goes. Two things have to be right:
                        //  1. Use the PRESS ORIGIN, not `interact_pointer_pos()`. `drag_started` only
                        //     fires once the pointer crosses egui's drag threshold (~6 px), so by then
                        //     the pointer has already moved a char or two off where the button went
                        //     down — sampling there dropped the edge character(s).
                        //  2. Anchor on the char EDGE opposite the drag direction — its LEADING edge
                        //     for a rightward drag, its TRAILING edge for a leftward one — so the
                        //     selection encloses the pressed char either way. Read straight from the
                        //     shaped layout (exact for any font), not a guessed cell width.
                        let press = ctx.input(|i| i.pointer.press_origin()).unwrap_or(p);
                        let prel = press - canvas_rect.min;
                        let dragging_left = p.x < press.x;
                        let hx = prel.x * s;
                        let hy = prel.y * s;
                        let anchor = if dragging_left {
                            self.hit_trailing(hx, hy)
                        } else {
                            self.hit_leading(hx, hy)
                        }
                        .unwrap_or(off);
                        self.drag_anchor = Some(anchor);
                        self.editor.host_mut().set_active_caret(anchor, None);
                    } else if click {
                        // Ctrl+click ADDS a caret (multi-caret); a plain click collapses the set
                        // back to one and places the caret there (the spec's "plain click resets").
                        // Exception: with URLHIGHLIGHT on, a Ctrl+click that lands on a highlighted URL
                        // opens it in the OS default app instead of adding a caret.
                        if ctx.input(|i| i.modifiers.ctrl) {
                            if let Some(url) = self.url_at_offset(off) {
                                self.editor.open_url(&url);
                            } else {
                                self.editor.host_mut().add_active_caret(off);
                            }
                        } else {
                            self.editor.host_mut().collapse_active_carets();
                            self.editor.host_mut().click_active_caret(off);
                        }
                    }
                    // The caret moved *after* this frame's canvas was drawn; force a
                    // repaint so the new position is shown even when the app was
                    // otherwise idle (e.g. right after arrow-key navigation).
                    ctx.request_repaint();
                }
            }
        }
        if resp.clicked() {
            // Focus already moved to the text above (preserving the cmline).
            self.drag_anchor = None;
        }
        if resp.drag_stopped() {
            self.box_anchor = None; // end the Alt+drag box-select gesture
        }

        // Enable IME and place its candidate window near the caret's line. (x is
        // approximate — line start; exact caret-x is a later refinement.)
        // 0-based caret line via the encoding-correct `line()` (no decoded-string
        // slicing by a rope offset).
        let caret_line = self.editor.document().line().saturating_sub(1);
        let y = canvas_rect.min.y + caret_line.saturating_sub(self.pane_view.scroll_line) as f32 * self.line_height;
        let caret_rect =
            egui::Rect::from_min_size(egui::pos2(canvas_rect.min.x, y), egui::vec2(2.0, self.line_height));
        if canvas_has_keys {
            ctx.output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput { rect: caret_rect, cursor_rect: caret_rect })
            });
        }
        // Split-pane dividers: a draggable bar per split (separator-coloured in the raster). Registered
        // AFTER the canvas image so it wins pointer events over the bar; a drag updates the split ratio.
        if multi {
            let origin = resp.rect.min;
            for d in self.editor.host().pane_dividers(w as f32, h as f32) {
                let bar = egui::Rect::from_min_size(
                    origin + egui::vec2(d.bar.x, d.bar.y),
                    egui::vec2(d.bar.w, d.bar.h),
                );
                let dr = ui.interact(bar, ui.id().with(("pane_divider", d.index)), egui::Sense::click_and_drag());
                if dr.hovered() || dr.dragged() {
                    ctx.set_cursor_icon(if d.side_by_side {
                        egui::CursorIcon::ResizeHorizontal
                    } else {
                        egui::CursorIcon::ResizeVertical
                    });
                }
                if dr.dragged() {
                    if let Some(p) = dr.interact_pointer_pos() {
                        let local = p - origin; // points into the whole canvas
                        let ratio = if d.side_by_side {
                            (local.x - d.area.x) / d.area.w.max(1.0)
                        } else {
                            (local.y - d.area.y) / d.area.h.max(1.0)
                        };
                        self.editor.host_mut().set_pane_ratio(d.index, ratio);
                        ctx.request_repaint();
                    }
                }
            }
        }

        // `TXT` opens a MODAL, separate native window (managed by the winit runtime — see
        // `runtime.rs`), not an in-app panel; the script is paused on `SuspendReason::ShowText`
        // until that window is dismissed. Nothing to render here.

        // `MSG` message box: a centred box the script popped (distinct from `PR`, which goes to
        // the status line). Non-blocking — the script continues; the box stays until the user
        // closes it (OK / ×) or its `/T:<secs>` timer elapses.
        if let Some((text, secs)) = self.editor.take_pending_message() {
            let dismiss_at = secs.map(|s| std::time::Instant::now() + std::time::Duration::from_secs(s));
            self.message_box = Some(MessageBox { text, dismiss_at, blocking: false });
        }
        // A bare `MSG` parks the script on a modal box — show it (no auto-dismiss) and resume the
        // script when it's dismissed. Recreated each frame while the script stays parked, so each
        // `MSG` in a loop pauses (the previous box is cleared + the script resumed on dismiss first).
        if self.message_box.is_none() {
            if let Some(text) = self.editor.message_request() {
                self.message_box = Some(MessageBox { text, dismiss_at: None, blocking: true });
            }
        }
        if let Some(mb) = &self.message_box {
            // Auto-dismiss when the timer elapses; otherwise keep waking to check it.
            if let Some(at) = mb.dismiss_at {
                let now = std::time::Instant::now();
                if now >= at {
                    self.message_box = None;
                } else {
                    ctx.request_repaint_after(at - now);
                }
            }
        }
        // `BK` breakpoint modal: `BK on` is a small Y/N stop prompt; `BK debug` lists the live
        // variables. Both offer Continue / Stop buttons (keyboard is handled in the input chain).
        if let Some(debug) = self.editor.breakpoint_request() {
            let mut choice: Option<bool> = None; // Some(true)=stop, Some(false)=continue
            // Header: locate the BK in the script (line, or line:col when the column is known).
            let loc = match self.editor.breakpoint_location() {
                Some((line, col)) if col > 0 => format!("line {line}:{col}"),
                Some((line, _)) => format!("line {line}"),
                None => "?".to_string(),
            };
            let title = if debug {
                format!("Breakpoint at {loc} — variables")
            } else {
                format!("Breakpoint at {loc}")
            };
            egui::Window::new(title)
                .id(egui::Id::new("breakpoint"))
                .collapsible(false)
                .resizable(debug)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(&ctx, |ui| {
                    if debug {
                        let vars = self.editor.breakpoint_vars();
                        // Grow the dialog to the widest `$name = value` line (up to a cap) so values
                        // don't wrap into a narrow column — same treatment as the MSG box.
                        let mono = egui::TextStyle::Monospace.resolve(ui.style());
                        let widest = vars
                            .iter()
                            .map(|(n, v)| {
                                ui.painter()
                                    .layout_no_wrap(
                                        format!("${n} = {v}"),
                                        mono.clone(),
                                        egui::Color32::WHITE,
                                    )
                                    .rect
                                    .width()
                            })
                            .fold(0.0_f32, f32::max);
                        let cap = (ui.ctx().content_rect().width() * 0.85).clamp(320.0, 900.0);
                        ui.set_max_width((widest + 28.0).clamp(320.0, cap));
                        ui.label(format!("{} variable(s) at this point:", vars.len()));
                        ui.separator();
                        egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show(
                            ui,
                            |ui| {
                                if vars.is_empty() {
                                    ui.weak("(no variables set)");
                                }
                                for (name, val) in &vars {
                                    ui.label(
                                        egui::RichText::new(format!("${name} = {val}")).monospace(),
                                    );
                                }
                            },
                        );
                        ui.separator();
                    } else {
                        ui.add_space(4.0);
                        ui.label("Stop script?");
                        ui.weak("Y = stop · any other key = continue");
                        ui.add_space(8.0);
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Continue").clicked() {
                            choice = Some(false);
                        }
                        if ui.button("Stop script").clicked() {
                            choice = Some(true);
                        }
                    });
                });
            if let Some(stop) = choice {
                self.editor.resolve_breakpoint(stop);
                self.surface_command_feedback();
                self.pane_view.follow = true;
                ctx.request_repaint();
            }
        }

        if let Some(mb) = self.message_box.as_ref().map(|m| m.text.clone()) {
            let mut open = true;
            let mut dismissed = false;
            // Scale the box up a touch — the default reads tiny. Applied to the text font, the width
            // caps, the paddings, and the OK button so it grows uniformly.
            const MSG_SCALE: f32 = 1.1;
            egui::Window::new("Message")
                .id(egui::Id::new("msg_box"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(&ctx, |ui| {
                    // Grow the dialog width with the text up to a cap, THEN wrap — instead of
                    // wrapping at egui's narrow auto-size default. Measure the widest (unwrapped)
                    // line, clamp to [min, max], and size the content to it; the label wraps only
                    // when a line would exceed the cap.
                    let font_id =
                        egui::FontId::proportional(self.editor.chrome_font() * 1.1 * MSG_SCALE);
                    let max_w = (ui.ctx().content_rect().width() * 0.75)
                        .clamp(240.0 * MSG_SCALE, 800.0 * MSG_SCALE);
                    let natural_w = mb
                        .lines()
                        .map(|l| {
                            ui.painter()
                                .layout_no_wrap(l.to_owned(), font_id.clone(), egui::Color32::WHITE)
                                .rect
                                .width()
                        })
                        .fold(0.0_f32, f32::max);
                    ui.set_max_width((natural_w + 4.0 * MSG_SCALE).clamp(160.0 * MSG_SCALE, max_w));
                    ui.add_space(4.0 * MSG_SCALE);
                    ui.add(egui::Label::new(egui::RichText::new(&mb).font(font_id)).wrap());
                    ui.add_space(8.0 * MSG_SCALE);
                    ui.vertical_centered(|ui| {
                        let ok = egui::RichText::new("OK")
                            .font(egui::FontId::proportional(self.editor.chrome_font() * MSG_SCALE));
                        if ui.button(ok).clicked() {
                            dismissed = true;
                        }
                    });
                });
            if !open || dismissed {
                self.dismiss_message_box(); // resumes the script if it was a blocking MSG
            }
        }

        // `HISTORY`/`HI` picker: open on request (highlight the most recent), then render the
        // interactive list. Keyboard (↑/↓/Enter/Del/Esc) is handled in the input branch above;
        // here a click highlights a row and the window's × closes it.
        if let Some(filter) = self.editor.take_history_request() {
            self.history_filter = filter;
            self.history_dialog = Some(0); // highlight the most recent (top of the newest-first list)
        }
        if let Some(sel) = self.history_dialog {
            let view = self.editor.history_view(&self.history_filter);
            let title = if self.history_filter.trim().is_empty() {
                "Command History".to_string()
            } else {
                format!("Command History — {}", self.history_filter.trim())
            };
            // Never narrower than the top legend line (proportional body font) so it can't wrap;
            // measure it and use it as the window's min width (+ padding for frame/scrollbar).
            let legend =
                "↑/↓ select · type to filter · Enter run · Shift+Enter edit · Del delete · Esc close";
            let body = egui::TextStyle::Body.resolve(&ctx.global_style());
            let legend_w =
                ctx.fonts_mut(|f| f.layout_no_wrap(legend.to_string(), body, egui::Color32::WHITE).size().x);
            let min_w = legend_w + 40.0;
            let mut open = true;
            let mut clicked: Option<usize> = None;
            egui::Window::new(title)
                .id(egui::Id::new("history_dialog"))
                .open(&mut open)
                .resizable(true)
                .min_width(min_w)
                .default_size(egui::vec2(560.0_f32.max(min_w), 360.0))
                .show(&ctx, |ui| {
                    ui.label(legend);
                    ui.separator();
                    // Fill the (legend-driven) width so the scrollbar sits at the window's right
                    // edge instead of shrinking to the content and floating mid-window.
                    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                        if view.is_empty() {
                            ui.weak("(no matching commands)");
                        }
                        for (row_i, (_, cmd)) in view.iter().enumerate() {
                            let row = ui
                                .selectable_label(row_i == sel, egui::RichText::new(cmd).monospace());
                            if row_i == sel {
                                row.scroll_to_me(None); // keep the highlight visible
                            }
                            if row.clicked() {
                                clicked = Some(row_i);
                            }
                        }
                    });
                });
            if let Some(i) = clicked {
                self.history_dialog = Some(i);
            }
            if !open {
                self.history_dialog = None;
            }
        }

        // `PICK` list chooser: open on request (a parked script), then render the filterable list.
        // Keyboard (↑/↓/PgUp/PgDn/type/Enter/Esc) is handled in the input branch above; here a click
        // highlights a row and a double-click (or the × / Esc) resolves it.
        if let Some((title, items)) = self.editor.pick_request() {
            // True only on the frame the dialog first appears — used to size the window to the
            // content once, before egui starts remembering the user's manual resizes.
            let just_opened = self.pick_dialog.is_none();
            if just_opened {
                self.pick_dialog = Some(0);
                self.pick_filter.clear();
            }
            let filtered = pick_filter_view(&items, &self.pick_filter);
            let sel = self.pick_dialog.unwrap_or(0).min(filtered.len().saturating_sub(1));
            let title = title.unwrap_or_else(|| "Choose one".to_string());
            // Open wide enough that the widest entry doesn't wrap. The rows are monospace, so their
            // content width is (longest entry in chars) × (one glyph's advance). The window must also
            // never be narrower than the legend line at the top (proportional body font), so measure
            // that too and take the wider. Add room for row padding + scrollbar + window frame, and
            // clamp so a single huge path can't make an absurd window.
            let legend = "↑/↓ select · type to filter · Enter choose · Esc cancel";
            let style = ctx.global_style();
            let mono = egui::TextStyle::Monospace.resolve(&style);
            let body = egui::TextStyle::Body.resolve(&style);
            let char_w = ctx.fonts_mut(|f| f.glyph_width(&mono, '0'));
            let legend_w =
                ctx.fonts_mut(|f| f.layout_no_wrap(legend.to_string(), body, egui::Color32::WHITE).size().x);
            let max_chars = items.iter().map(|s| s.chars().count()).max().unwrap_or(0);
            let entries_w = max_chars as f32 * char_w;
            let want_w = (entries_w.max(legend_w) + 56.0).clamp(420.0, 1200.0);
            let mut open = true;
            let mut chosen: Option<usize> = None; // ORIGINAL index, on double-click (or single, for tabs)
            // A `LISTTABS` chooser is act-on-click (like clicking the tab itself): a single click
            // switches. Other pickers highlight on single click and commit on double-click/Enter.
            let tab_mode = self.editor.pick_is_tab_switch();
            let mut win = egui::Window::new(title).id(egui::Id::new("pick_dialog")).open(&mut open);
            // On the opening frame force the computed size (overrides any remembered width so the
            // dialog always opens fit-to-content); afterwards it's freely resizable.
            win = if just_opened {
                win.fixed_size(egui::vec2(want_w, 320.0))
            } else {
                win.resizable(true).default_size(egui::vec2(want_w, 320.0))
            };
            win.show(&ctx, |ui| {
                    ui.label(legend);
                    if !self.pick_filter.is_empty() {
                        ui.label(egui::RichText::new(format!("filter: {}", self.pick_filter)).weak());
                    }
                    ui.separator();
                    egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                        // Entries extend instead of wrapping (a row may be wider than the window —
                        // the horizontal scrollbar is the fallback past the width clamp). Fill the
                        // width so the vertical scrollbar sits at the window's right edge.
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                        if filtered.is_empty() {
                            ui.weak("(no matches)");
                        }
                        for (row_i, (orig, label)) in filtered.iter().enumerate() {
                            let row = ui.selectable_label(
                                row_i == sel,
                                egui::RichText::new(label).monospace(),
                            );
                            if row_i == sel {
                                row.scroll_to_me(None);
                            }
                            if row.clicked() {
                                if tab_mode {
                                    chosen = Some(*orig); // single click switches tabs
                                } else {
                                    self.pick_dialog = Some(row_i);
                                }
                            }
                            if row.double_clicked() {
                                chosen = Some(*orig);
                            }
                        }
                    });
                });
            if let Some(orig) = chosen {
                self.pick_dialog = None;
                self.pick_filter.clear();
                self.editor.pick_respond(Some(orig));
                self.surface_command_feedback();
                self.pane_view.follow = true;
            } else if !open {
                // Window × — treat as cancel.
                self.pick_dialog = None;
                self.pick_filter.clear();
                self.editor.pick_respond(None);
                self.surface_command_feedback();
            }
        }

        // `EDFONTS`/`UIFONTS`/`TXTFONTS` font picker (opened at the top of `ui()`): render the
        // window. Its live preview renders in the `xy-preview` egui family installed up there.
        if self.font_picker.is_some() {
            self.render_font_picker(&ctx);
        }
        if self.color_picker.is_some() {
            self.render_color_picker(&ctx);
        }
        // The Find/Replace dialog is now its OWN OS window (`FindWindow`, driven by the runtime); it is
        // no longer drawn in the main window's frame. See `find_window_ui`.
        if self.run_dialog.is_some() {
            self.render_run_dialog(&ctx);
        }
        if self.grep_dialog.is_some() {
            self.render_grep_dialog(&ctx);
        }
        if self.properties_open {
            self.render_properties(&ctx);
        }
        if self.about_open {
            self.render_about(&ctx);
        }
        // (The unified menu bar + dropdowns and the `MENU` request are handled earlier in `ui()`,
        // right where the top bar is laid out — see `render_menus`.)

        // `QUIT` request: `/NV` (verify=false) exits at once; bare `QUIT` (verify=true) starts the
        // dirty-tab save sweep (handled at the top of the next frame). Wake a frame to act on it.
        if let Some(verify) = self.editor.take_quit() {
            if verify {
                self.quitting = true;
            } else {
                self.should_exit = true;
            }
            ctx.request_repaint();
        }

        // Mirror an app-side clipboard change (copy/cut/CLIP/…) out to the OS.
        self.sync_clipboard_out();

        // Dim-on-blur (`DIMONBLUR on`): when the WHOLE app is unfocused (no XyScript window active —
        // `app_has_focus`, tracked by the runtime across the main + child windows, so switching to our
        // own Find / DIR-W / TXT window does NOT dim), lay a faint translucent veil over everything so
        // it reads as inactive — a cue otherwise missing with `TITLEBAR off`. Painted last, on a
        // foreground layer above all content and chrome.
        if self.editor.dim_on_blur() && !self.app_has_focus {
            // Debounce: a within-app focus hop (opening the Find/DIR-W window) leaves the main briefly
            // unfocused before the child's focus registers — wait a grace period so that never flashes.
            const DIM_GRACE: std::time::Duration = std::time::Duration::from_millis(180);
            match self.unfocused_since {
                Some(t) if t.elapsed() >= DIM_GRACE => {
                    let painter = ctx.layer_painter(egui::LayerId::new(
                        egui::Order::Foreground,
                        egui::Id::new("dim-on-blur"),
                    ));
                    painter.rect_filled(ctx.screen_rect(), 0.0, egui::Color32::from_black_alpha(48));
                }
                // Still within the grace window — repaint once it elapses so an idle unfocus still dims.
                Some(_) => ctx.request_repaint_after(DIM_GRACE),
                None => {}
            }
        }
    }
}

/// Where crash reports are written: next to the executable, falling back to the OS temp dir if the
/// exe's folder isn't writable (e.g. installed under Program Files).
fn crash_log_path() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("xyscript-crash.log")))
        .unwrap_or_else(|| std::env::temp_dir().join("xyscript-crash.log"))
}

/// Format Unix `millis` as a human-readable LOCAL timestamp with its UTC offset, e.g.
/// `2026-07-19 03:15:21 +00:00`. Bug reports quote these, so a bare epoch number is unhelpful — and
/// the offset makes a report from another timezone unambiguous. Reuses the engine's own civil-date
/// math (`clock::fields_from_millis`, no date crate) and the offset resolved at startup
/// (`dir::set_local_offset`); both are pure/atomic, so this is safe to call from a panic hook.
/// A crash before the offset is resolved simply reports UTC (`+00:00`), which is still correct.
fn local_timestamp(millis: i64) -> String {
    let offset = xyscript_app::dir::local_offset_secs();
    let t = xyscript_app::clock::fields_from_millis(millis, offset);
    let (sign, abs) = if offset < 0 { ('-', -offset) } else { ('+', offset) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {sign}{:02}:{:02}",
        t.year,
        t.month,
        t.mday,
        t.hour,
        t.min,
        t.sec,
        abs / 3600,
        (abs % 3600) / 60,
    )
}

/// Install a panic hook that appends a timestamped report (message, location, backtrace) to a log
/// file, then chains to the previous hook so a debug build still prints to its console. A
/// release/windowed build has no console, so without this a crash would vanish silently.
fn install_crash_logger() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let secs = millis / 1000;
        let when = local_timestamp(millis);
        let backtrace = std::backtrace::Backtrace::force_capture();
        let report = format!(
            "\n===== XyScript crash =====\n\
             version : {}\n\
             time    : {when}\n\
             unix-ts : {secs}\n\
             {info}\n\
             backtrace:\n{backtrace}\n",
            env!("CARGO_PKG_VERSION"),
        );
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(crash_log_path())
        {
            use std::io::Write;
            let _ = f.write_all(report.as_bytes());
        }
        previous(info);
    }));
}

/// Attach this (GUI-subsystem) process to the console of whatever launched it, so a headless
/// `--run` invocation's stdout/stderr reach that console. `AttachConsole` does nothing if the
/// handles are already set (output redirected to a file/pipe), so the `RUN/BG` child and shell
/// piping keep working unchanged. No-op off Windows (those builds keep their console anyway).
#[cfg(windows)]
fn attach_parent_console() {
    extern "system" {
        fn AttachConsole(dw_process_id: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF; // (DWORD)-1
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
#[cfg(not(windows))]
fn attach_parent_console() {}

fn main() {
    // Record panics to a log file (a release/windowed build has no console to print them to).
    install_crash_logger();

    // Resolve the local timezone offset once, up front (the process is still single-
    // threaded here), so DIR listings show modification times in local time.
    xyscript_app::dir::set_local_offset(xyscript_app::dir::system_local_offset_secs());

    // Headless batch mode (`--run <script>`): run the script with no window and exit. This is what a
    // `RUN/BG`-spawned child invokes (`$ED --run <path>`), so background XPL is an `X/BG` job that
    // runs xyscript headlessly. Checked before any window/wgpu setup.
    let cli_args: Vec<String> = std::env::args().skip(1).collect();
    // A release build is a GUI-subsystem app (no console), so a CLI run from a normal shell would
    // have nowhere to print. Attach the launching console for the headless paths so `--run` output is
    // visible (a no-op when output is already redirected to a pipe, e.g. the `RUN/BG` child).
    if cli_args
        .iter()
        .any(|a| matches!(a.as_str(), "--run" | "-run" | "--render-only" | "-render-only"))
    {
        attach_parent_console();
    }
    // `--message <text>`: show a native OS box and exit (what `MESSAGE` re-execs on Windows).
    if let Some(code) = xyscript_app::os_message::maybe_render(&cli_args) {
        std::process::exit(code);
    }
    if let Some(code) = xyscript_app::boot::run_batch(&cli_args) {
        std::process::exit(code);
    }

    if std::env::args().any(|a| a == "--render-only" || a == "-render-only") {
        let mut app = GuiApp::new();
        for (name, scroll) in [("canvas.png", 0usize), ("canvas_scrolled.png", 8)] {
            app.pane_view.scroll_line = scroll;
            app.pane_view.follow = false;
            let (w, h) = (900usize, 300usize);
            let rgba = app.raster(w, h);
            image::RgbaImage::from_raw(w as u32, h as u32, rgba)
                .expect("size")
                .save(name)
                .expect("save");
            println!("wrote {name} (scroll line {scroll})");
        }
        return;
    }

    // Remaining POSITIONAL arguments are files to open on startup (`xyscript.exe a.md notes.txt`).
    // Skip switches AND the value of a value-taking switch, so a stray `-run` — or the `<script>`
    // after `-s`/`--startup` — is never opened as a file (the earlier `!starts_with("--")` filter
    // did exactly that: `xyscript.exe -run startup.new` opened files named "-run" and "startup.new").
    let mut app = GuiApp::new();
    // Restore the previous session's tabs (if `SESSION` is on) — before command-line files, so those
    // open ALONGSIDE the restored set (Notepad++-style) and land active.
    app.restore_session_if_enabled();
    let mut files: Vec<String> = Vec::new();
    let mut goto_line: Option<usize> = None;
    let mut cwd_override: Option<String> = None;
    let mut it = cli_args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            // Value-taking switches: skip the switch AND its following value.
            "-s" | "--startup" | "-run" | "--run" => {
                it.next();
            }
            // `--goto <line>` — 1-based line to land on after opening the file(s). Used by `DUPVIEW/W`
            // so the new window opens at the source view's line.
            "--goto" | "-goto" => {
                goto_line = it.next().and_then(|v| v.parse::<usize>().ok());
            }
            // `--cwd <dir>` — set $CWD AFTER startup.xpl has run (so it overrides any startup `cd`).
            // `DUPVIEW/W` uses it to restore the parent window's working directory, since the child is
            // booted in the exe directory (so startup's relative config loads resolve).
            "--cwd" | "-cwd" => {
                cwd_override = it.next().cloned();
            }
            // Any other switch (e.g. `--render-only`, or an unknown `-x`): skip just it.
            _ if a.starts_with('-') => {}
            // A genuine positional argument → a file to open.
            _ => files.push(a.clone()),
        }
    }
    if !files.is_empty() {
        app.open_startup_files(&files);
    }
    if let Some(line) = goto_line {
        app.editor.goto_line(line);
        app.pane_view.follow = true; // reveal the line
    }
    if let Some(dir) = cwd_override {
        app.editor.set_cwd(std::path::PathBuf::from(dir));
    }
    runtime::run(app);
}
