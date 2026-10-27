//! A document: a [`Buffer`] plus an **inverse-op undo log** (`beyond-stage2-plan.md`
//! decision D2). Every text mutation flows through here so its inverse can be
//! recorded; motions break the coalescing run. In Stage 3 a `Document` also gains
//! a file path / dirty flag / encoding (step 3); for now it is buffer + history.
//!
//! ## Undo model (D2)
//!
//! An edit is recorded as one [`Op`] — `(at, removed, inserted)` in **byte units**
//! (the byte-faithful truth; `byte-text-engine-plan.md` step 3) — whose inverse
//! swaps `removed`/`inserted`. An [`Unit`] is one undo step: a
//! single (possibly coalesced) op plus the caret state **before** and **after**,
//! so undo/redo restore the cursor and selection too. Memory scales with edited
//! text, not file size (the reason this beat COW snapshots for 100–300MB files).
//!
//! **Coalescing.** Consecutive single-character inserts at adjacent positions
//! merge into one unit, and consecutive same-direction deletes likewise; a
//! **cursor jump** (any motion), a change of edit kind, a selection-replacing
//! edit, or an undo/redo closes the current unit. So a typed run undoes as a
//! chunk, not character-by-character. A fresh edit clears the redo stack.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use xyscript_xpl::{CaseKind, DirOpts, FontMode, SortSpec};

/// Process-global source of content generations. Each whole-buffer swap / encoding /
/// highlighter change pulls a fresh value, so a document's [`revision`](Document::revision)
/// is unique across *all* documents and loads — two freshly-opened files never share a
/// revision (which would let a render cache show the wrong file). Starts at 1 so a never-
/// loaded document (gen 0) is still distinct.
static CONTENT_GEN: AtomicU64 = AtomicU64::new(1);

fn next_content_gen() -> u64 {
    CONTENT_GEN.fetch_add(1, Ordering::Relaxed)
}

/// Byte margin tokenized on each side of the visible window by [`Document::refresh_highlights`]. Big
/// enough that a document this size or smaller is highlighted whole (so small files and tests are
/// unchanged), and that a few screens of scrolling reuse the cache; small enough that a re-tokenize of
/// the window stays cheap on a multi-megabyte file.
const HL_WINDOW_PAD: usize = 64 * 1024;

use std::cell::RefCell;
use std::rc::Rc;

use crate::encoding::{Charset, Encoding};
use crate::highlight::{GrammarRegistry, Highlighter, Token};
use crate::text::{Buffer, Caret, CaretState, Pattern};

/// Swap the case of each character (`CC`).
fn swap_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_uppercase() {
            out.extend(c.to_lowercase());
        } else if c.is_lowercase() {
            out.extend(c.to_uppercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Browser state for a buffer showing a `DIR`/`LS` listing (QN B10). The buffer's text
/// is [`crate::dir::render_listing`] of `entries`, so **line N is `entries[N]`**; `find`
/// is the live express-key (type-to-find) substring shown in the footer.
#[derive(Debug, Clone, Default)]
pub struct DirState {
    /// The directory being shown (the base the entries were listed from).
    pub base: PathBuf,
    /// One entry per buffer line, in display order.
    pub entries: Vec<crate::dir::DirEntry>,
    /// The current express-key search buffer (case-insensitive name PREFIX).
    pub find: String,
    /// View + sort options (`/W /O… /A… /R`); preserved across `dir_enter` descents and
    /// re-lists so the view style/order stays put.
    pub opts: DirOpts,
    /// The active filespec glob (`DIR *.txt`), if any — kept so a re-list (e.g. after a
    /// delete) re-applies the same filter. Dropped when descending into a subdirectory.
    pub filter: Option<String>,
}

/// A file's line-ending style, detected on open and preserved on save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    /// `\n` (Unix).
    Lf,
    /// `\r\n` (Windows).
    Crlf,
}

impl LineEnding {
    /// The platform default for brand-new documents: CRLF on Windows, LF elsewhere.
    pub fn os_default() -> LineEnding {
        if cfg!(windows) {
            LineEnding::Crlf
        } else {
            LineEnding::Lf
        }
    }
    /// The literal byte sequence this style inserts.
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
        }
    }
    /// A human-readable label for `$NEWLINE`.
    pub fn label(self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::Crlf => "CRLF",
        }
    }
}

/// Rewrite line endings at the BYTE level: drop every `\r` that immediately precedes a `\n` (→ LF),
/// then for CRLF re-insert a `\r` before each `\n`. Encoding-agnostic — `\r` (0x0D) and `\n` (0x0A)
/// are identical across every encoding we support — and lossless. Lone `\r` (old-Mac) is left as-is.
fn normalize_newlines(bytes: &[u8], target: LineEnding) -> Vec<u8> {
    let mut lf = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            i += 1; // drop the \r, keep the \n
            continue;
        }
        lf.push(bytes[i]);
        i += 1;
    }
    match target {
        LineEnding::Lf => lf,
        LineEnding::Crlf => {
            let mut out = Vec::with_capacity(lf.len() + lf.len() / 40);
            for b in lf {
                if b == b'\n' {
                    out.push(b'\r');
                }
                out.push(b);
            }
            out
        }
    }
}

/// MDA styling (fg/bg/attr spans) over an edit's affected region, REBASED so offset 0 is the op's
/// `at`. Carried in the undo [`Op`] so styling survives the edit instead of being torn out
/// irrecoverably: `removed_spans` is what the edit removed (restored when undo re-inserts `removed`),
/// `inserted_spans` is styling to (re)apply over `inserted` on the forward edit and on redo — which is
/// how `MV`, COPY-to-cursor and a styled paste carry the style along with the relocated text. Empty
/// for an ordinary edit, so the hot typing path pays nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct SpanSnapshot {
    fg: Vec<(usize, usize, (u8, u8, u8))>,
    bg: Vec<(usize, usize, (u8, u8, u8))>,
    attr: Vec<(usize, usize, u8)>,
}

impl SpanSnapshot {
    fn is_empty(&self) -> bool {
        self.fg.is_empty() && self.bg.is_empty() && self.attr.is_empty()
    }
}

/// The MDA styling of a COPIED/CUT selection, held on the clipboard alongside the text so a paste can
/// restore it. The spans are rebased to the selection start; `byte_len` is the selection's byte length
/// so a paste into a DIFFERENT encoding (where the re-encoded text has a different byte length, and the
/// rebased offsets would no longer line up) can decline to apply the styling rather than smear it.
/// Opaque to the host, which just carries it between copy/cut and paste.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StyleClip {
    snap: SpanSnapshot,
    byte_len: usize,
}

impl StyleClip {
    /// Whether the clip carries any styling (an unstyled copy produces `None`, not an empty clip).
    pub fn is_empty(&self) -> bool {
        self.snap.is_empty()
    }
}

/// A primitive change: at byte offset `at`, `removed` bytes were deleted and
/// `inserted` bytes were added. Inverse: delete `inserted` at `at`, insert
/// `removed`. Storing raw bytes (not strings) keeps undo byte-exact and is what
/// multi-byte encodings will need later. `removed_spans`/`inserted_spans` carry the MDA styling
/// across the edit (see [`SpanSnapshot`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Op {
    at: usize,
    removed: Vec<u8>,
    inserted: Vec<u8>,
    removed_spans: SpanSnapshot,
    inserted_spans: SpanSnapshot,
}

/// The kind of edit, for coalescing decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Insert,
    DeleteBack,
    DeleteForward,
    /// A multi-caret typing insert (N pure inserts, one per caret). Coalesces with a continuing
    /// multi-caret run — matched by caret index, extending each op's inserted bytes — so a typed run
    /// undoes as ONE unit, like a single-caret run.
    MultiInsert,
    /// A selection-replacing edit — never coalesces (its own undo unit).
    Other,
}

/// A pure STYLING change (`MDA`/`MDALL`) recorded for undo: over the ROPE region `[lo, hi)`, the MDA
/// styling went from `before` to `after` (both rebased to `lo`). Undo restores `before`, redo `after`.
/// Carried on a [`Unit`] with no text ops, so Ctrl+Z reverts an applied style — the piece previously
/// deferred (styling was off the undo stack entirely).
#[derive(Debug, Clone, PartialEq, Eq)]
struct StyleDelta {
    lo: usize,
    hi: usize,
    before: SpanSnapshot,
    after: SpanSnapshot,
}

/// One undo step. Usually ONE op (single caret), but a multi-caret keystroke records N ops that
/// undo together as this one unit — applied in order, reversed on undo. `secondary_before/after`
/// snapshot the multi-caret set so undo/redo restore the carets (EMPTY for single-caret edits,
/// which leave the current caret set untouched). A unit with no `ops` but a `style` is a pure
/// styling step (`MDA`/`MDALL`).
#[derive(Debug, Clone)]
struct Unit {
    ops: Vec<Op>,
    kind: Kind,
    cursor_before: usize,
    anchor_before: Option<usize>,
    cursor_after: usize,
    anchor_after: Option<usize>,
    secondary_before: Vec<Caret>,
    secondary_after: Vec<Caret>,
    /// Set for a pure styling step; `None` for an ordinary text edit.
    style: Option<StyleDelta>,
}

/// `MDA` text-attribute bits, stored per span in [`Document::attr_spans`] and persisted in the
/// `.xymeta` sidecar's `[attrs]` section. Independent flags — the `MDA` keywords map to combinations
/// (`BO`→bold, `IT`→italic, `BI`→bold+italic, `UL`→underline, `BU`→bold+underline). Defined once in
/// `xyscript-xpl` (the verb layer that parses the keywords) and re-exported here for the app/GUI.
pub use xyscript_xpl::{ATTR_BOLD, ATTR_ITALIC, ATTR_REVERSE, ATTR_UNDERLINE};

/// How many caret-set snapshots the `CARETPOP` ring keeps (a build sequence rarely runs deep; this
/// is a runaway backstop, oldest dropped past it).
const CARET_RING_CAP: usize = 128;

/// Bytes of surrounding text kept on each side of a bookmark for content-based relocation. Small
/// enough to keep the sidecar compact, large enough to be distinctive within a document.
const MARK_CTX: usize = 48;

/// A saved caret location — a bookmark (`MARKSET`). Persisted per-document in the `.xymeta`
/// sidecar's `[marks]` section and restored on reopen. `off` is a raw rope-byte offset (like `$CP`)
/// that rides edits in-session (a live anchor); `ctx`/`split` let a mark be RELOCATED by content when
/// the file changed on disk between sessions (see [`Document::reconcile_marks`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Bookmark {
    /// Rope-byte offset of the bookmarked point. Shifts with edits so it stays on its text.
    pub off: usize,
    /// Raw bytes of a window straddling the point (up to [`MARK_CTX`] each side), for relocation.
    /// Empty only for a mark with no surrounding text (an empty buffer).
    pub ctx: Vec<u8>,
    /// How many bytes of `ctx` precede the point (relocated point = match start + `split`).
    pub split: usize,
    /// Session-only: set when load-time reconciliation had to RELOCATE this mark (the text moved),
    /// so the next `MARKNEXT`/`MARKPREV` that lands on it can report the move. Not persisted.
    pub moved: bool,
}

/// The outcome of a `MARKNEXT`/`MARKPREV` jump: where the caret landed, whether that bookmark had
/// been relocated (so the caller can say so), and its 1-based index / total (for the status line).
pub struct MarkNav {
    pub off: usize,
    pub moved: bool,
    pub index: usize,
    pub total: usize,
}

/// Find `ctx` in `text` so the resulting point (`match_start + split`) lands within `[lo, hi]`,
/// choosing the occurrence whose point is NEAREST `orig`. Raw-byte search (encoding-agnostic); the
/// scan is bounded to the vicinity so it stays cheap. `None` if the context can't be placed in range.
fn find_ctx_near(
    text: &[u8],
    ctx: &[u8],
    split: usize,
    lo: usize,
    hi: usize,
    orig: usize,
) -> Option<usize> {
    if ctx.is_empty() || ctx.len() > text.len() {
        return None;
    }
    // A match starting in this window can put its point anywhere in [lo, hi].
    let scan_lo = lo.saturating_sub(split);
    let scan_hi = (hi + (ctx.len() - split)).min(text.len());
    let mut best: Option<usize> = None;
    let mut best_dist = usize::MAX;
    let mut i = scan_lo;
    while i + ctx.len() <= scan_hi {
        if &text[i..i + ctx.len()] == ctx {
            let point = i + split;
            if (lo..=hi).contains(&point) {
                let dist = point.abs_diff(orig);
                if dist < best_dist {
                    best_dist = dist;
                    best = Some(point);
                }
            }
        }
        i += 1;
    }
    best
}

/// A text document: a buffer, undo/redo history, and file identity.
#[derive(Debug)]
pub struct Document {
    buffer: Buffer,
    undo: Vec<Unit>,
    redo: Vec<Unit>,
    /// True when the top undo unit is still open for coalescing.
    coalescing: bool,
    /// Open-undo-group nesting depth (`UNDOBEGIN`/`UNDOEND`). While > 0, every recorded edit folds into
    /// ONE undo unit instead of pushing its own — so a composite command (e.g. a Vim `dw` = `DF;NW;DM;RD`)
    /// undoes in a single step. Depth-counted so nested groups collapse to the outermost. Per-document,
    /// like the undo stack itself; `UNDO`/`REDO` force-close any open group.
    group_depth: usize,
    /// True once the open group has pushed its (shared) unit — the first edit opens it, later edits
    /// extend it. Reset when the group closes, and self-heals if the undo stack is cleared mid-group
    /// (the extend path re-opens a fresh unit when the stack is empty).
    group_active: bool,
    /// The `CARETPOP` undo ring: caret-set snapshots taken before each caret-set-changing verb, so
    /// popping restores the previous set. Transient (selection state only, no content) and cleared by
    /// any document edit. Capped at [`CARET_RING_CAP`].
    caret_ring: Vec<CaretState>,
    /// Undo-stack length at the last save (the "saved" point). `None` means the
    /// saved state is unreachable by undo (a new edit clobbered the redo path
    /// that led back to it) — i.e. always dirty. The document is *not modified*
    /// iff `saved_len == Some(undo.len())`.
    saved_len: Option<usize>,
    /// Backing file path, if any.
    path: Option<PathBuf>,
    /// Line-ending style to write on save (detected on open).
    line_ending: LineEnding,
    /// When true the buffer content is locked (`RO`): edits are rejected. `$RO`.
    read_only: bool,
    /// A synthetic display name for buffers with no backing file (e.g. `*Shell Output*`).
    /// Used by `file_name` when `path` is `None`.
    label: Option<String>,
    /// The active syntax highlighter, if any. `None` = off (the quiet-screen
    /// default). Selected by file type when highlighting is turned on.
    highlighter: Option<Highlighter>,
    /// Cached highlight tokens for the current buffer (empty when off), sorted by start and covering the
    /// contiguous DECODED range `[hl_lo, hl_hi]`. For a large document this is only a WINDOW around the
    /// view, not the whole file, and it's maintained INCREMENTALLY: an edit re-tokenizes just the changed
    /// line(s) ([`note_edit`](Document::note_edit)) and a scroll extends the covered range
    /// ([`ensure_highlights`](Document::ensure_highlights)) — never the whole window per keystroke.
    tokens: Vec<Token>,
    /// The DECODED range the GUI last wanted highlighted (its visible span). `None` until the renderer
    /// reports one — then `refresh_highlights` tokenizes only a padded, line-snapped window around it
    /// instead of the whole (possibly huge) document. See [`Document::ensure_highlights`].
    hl_window: Option<(usize, usize)>,
    /// The contiguous DECODED range `tokens` currently covers (line-aligned). Valid only when `hl_valid`.
    hl_lo: usize,
    hl_hi: usize,
    /// The buffer's decoded length when the cache was last synced — subtract from the current decoded
    /// length to get an edit's decoded delta (for shifting tokens after the edit) without decoding the
    /// changed bytes.
    hl_doc_len: usize,
    /// Whether `tokens`/`hl_lo`/`hl_hi` are usable. Cleared when an edit lands outside the covered range
    /// (or highlighting is off); [`ensure_highlights`](Document::ensure_highlights) then rebuilds.
    hl_valid: bool,
    /// The content `gen` at which the cache was last synced — a mismatch with `gen` means an edit landed
    /// that the incremental path didn't handle, so the cache is stale and must be rebuilt.
    hl_gen: u64,
    /// Per-selection FOREGROUND colour spans (`FGCOLOR`/`MD` on a selection): `(start, end, rgb)` in
    /// ROPE-byte offsets (like `Op.at`, so `record` tracks them across edits). Sorted, non-overlapping.
    /// Converted to decoded offsets for the renderer by `color_spans_decoded`. Persisted to the doc's
    /// `.xymeta` sidecar (phase 2); independent of `bg_spans`.
    color_spans: Vec<(usize, usize, (u8, u8, u8))>,
    /// Per-selection BACKGROUND colour spans (`MDA/B` on a selection) — an INDEPENDENT layer from
    /// `color_spans`, drawn as rectangles behind the glyphs (a bg block can cover a fg-coloured word).
    /// Same rope-offset, edit-following, non-overlapping model. `bg_color_spans_decoded` for the renderer.
    bg_spans: Vec<(usize, usize, (u8, u8, u8))>,
    /// Per-selection TEXT-ATTRIBUTE spans (`MDA BO|IT|BI|UL|BU`) — a THIRD independent layer, the value
    /// a bitset of [`ATTR_BOLD`]/[`ATTR_ITALIC`]/[`ATTR_UNDERLINE`]. Same rope-offset, edit-following,
    /// non-overlapping model as the colour layers; rendered as glyph weight/slant + a drawn underline.
    /// Persisted to the `.xymeta` sidecar's `[attrs]` section. `attr_spans_decoded` for the renderer.
    attr_spans: Vec<(usize, usize, u8)>,
    /// A cheap gate: set the moment any `MDA`/`FGCOLOR`/mark styling is TOUCHED, so a never-styled doc
    /// skips the signature compare in [`is_modified`](Self::is_modified) entirely. Once set it stays set
    /// for the session; whether the styling is *actually* unsaved is then decided by comparing
    /// [`styling_sig`](Self::styling_sig) to [`saved_styling_sig`](Self::saved_styling_sig) — which is
    /// what lets an UNDO back to the saved styling clear the modified flag (the signature matches again).
    styling_dirty: bool,
    /// Signature of the styling (fg/bg/attr spans + marks) at the last saved / freshly-opened baseline.
    /// `is_modified` compares the live signature against it, so styling is "modified" only when the
    /// content genuinely differs — undoing a style change back to the baseline reports clean again.
    saved_styling_sig: u64,
    /// Canvas font MODE for THIS document (`USEFONT`; session-only, never saved). `Auto` follows the
    /// rule (code/listing/HEX → monospace, prose → proportional when an `EDPROPFAMILY` is set);
    /// `Mono`/`Prop` pin it. The GUI consults it to choose the shaping family.
    font_mode: FontMode,
    /// The `MDA` live-typing mode for THIS document (session-only, never saved): when a layer is set,
    /// each run typed into the doc is styled with it (`apply_typing_style`). Per-document — armed with a
    /// no-selection `MDA` and carried with the tab, so switching files doesn't inherit another's mode.
    mda_type_fg: Option<(u8, u8, u8)>,
    mda_type_bg: Option<(u8, u8, u8)>,
    mda_type_attr: Option<u8>,
    /// Bookmarks (`MARKSET`) for THIS document, in rope-byte offsets, sorted by offset. Ride edits
    /// like the colour spans (via `adjust_color_spans`), persisted to the `.xymeta` sidecar's
    /// `[marks]` section, and relocated by content on reopen. See [`Bookmark`].
    marks: Vec<Bookmark>,
    /// Live language-check underlines for THIS document (`SPELLING`/`GRAMMAR`), in rope-byte offsets.
    /// Held per-document (not host-global) so each pane in a split shows its OWN doc's marks; refreshed
    /// asynchronously by the host's lint worker (see `AppHost::poll_lint`). Session-only, never saved.
    lints: Vec<crate::lint::LintMark>,
    /// Generation counter for content changes the rope's edit-counter can't see:
    /// a whole-buffer swap (load), an encoding switch (re-decode), or a highlighter
    /// toggle (recolor). Combined with the rope rev in [`Document::revision`] to give
    /// a single value that changes whenever the rendered text or its colors change.
    gen: u64,
    /// When set, this buffer is a `DIR`/`LS` listing rather than an editable document;
    /// holds the entries (line N → `entries[N]`) and the live express-key buffer.
    dir: Option<DirState>,
    /// The user's regex grammars, SHARED with the host (and every other document) so this
    /// document can resolve its own highlighter by file extension on open/reload. An empty
    /// registry (the default) means "Markdown only" — the host injects its real one via
    /// [`share_grammars`](Document::share_grammars).
    grammars: Rc<RefCell<GrammarRegistry>>,
    /// `DOCTYPE` override for `TOPDF`/preview rendering. `None` = auto (decide by extension:
    /// `.typ` → Typst, else GFM); `Some(_)` forces that kind regardless of extension.
    render_override: Option<RenderKind>,
    /// `HEX on|off`: view the document's raw bytes as a hex + text grid (a CANVAS display mode,
    /// orthogonal to `render_override`/`DOCTYPE` which is about preview/print). Per-document,
    /// session-only. The byte content is unchanged; only the GUI's rendering + navigation differ.
    hex_view: bool,
    /// The nine `C1`..`C9` counters (XyWrite-style). Per-document and session-only (not persisted).
    counters: [crate::counter::Counter; 9],
    /// `EMPH` emphasis phrase: every case-insensitive occurrence is highlighted (a visual overlay,
    /// never an edit — so it works in read-only / `*Shell Output*` tabs). Per-document, session-only.
    /// `None` = no emphasis. Colours come from the host's `EMPHFGCOLOR`/`EMPHBGCOLOR`.
    emph: Option<String>,
    /// Whether [`emph`](Self::emph) is a regular expression (`EMPH/R`) rather than a whole-word literal.
    emph_regex: bool,
    /// When set, this buffer is a `GREP` results listing (read-only) rather than an editable document;
    /// holds the hits (line N → `hits[N]`) so `GJ` resolves the hit under the caret. Session-only.
    grep: Option<crate::grep::GrepState>,
    /// Whether this buffer is a `HELP`/`MAN` results listing (read-only): a generated, reusable tab
    /// that the next `HELP`/`MAN` replaces in place. Session-only. See [`Document::open_help_listing`].
    help_listing: bool,
    /// A stable per-buffer handle for the split-pane layout tree. Tab indices shift when tabs close,
    /// so the pane tree references buffers by this id. Assigned once in [`Document::default`] (the
    /// single choke point all constructors route through), never reused. See [`crate::panes`].
    id: crate::panes::DocId,
}

/// Monotonic source of [`DocId`](crate::panes::DocId)s. Every `Document::default()` mints a fresh one.
static NEXT_DOC_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

fn fresh_doc_id() -> crate::panes::DocId {
    crate::panes::DocId(NEXT_DOC_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

/// How a document is rendered to PDF / preview: as GitHub-Flavored Markdown or as HTML (each
/// translated to typst markup), or as authored typst markup (compiled verbatim). Chosen per document
/// — see [`Document::render_kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderKind {
    Gfm,
    Typst,
    Html,
    /// Plain text: the buffer rendered verbatim (each newline a hard line break), with **no** Markdown
    /// interpretation. The auto default for non-`.md` text files; forced by `DOCTYPE text`.
    Text,
}

impl RenderKind {
    /// The lowercase name used by `DOCTYPE` / `$DOCTYPE`.
    pub fn as_str(self) -> &'static str {
        match self {
            RenderKind::Gfm => "gfm",
            RenderKind::Typst => "typst",
            RenderKind::Html => "html",
            RenderKind::Text => "text",
        }
    }

    /// The AUTO (extension-driven) render kind for a file path — ignoring any `DOCTYPE` override:
    /// `.md`/`.markdown` → Gfm, `.typ` → Typst, `.html`/`.htm`/`.xhtml` → Html, everything else
    /// (incl. no extension) → plain Text. Used by [`Document::render_kind`] and by `FPRINT`, which
    /// renders a file that isn't open by its own type.
    pub fn from_path(path: &std::path::Path) -> RenderKind {
        match path.extension().and_then(|e| e.to_str()) {
            Some(e) if e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown") => {
                RenderKind::Gfm
            }
            Some(e) if e.eq_ignore_ascii_case("typ") => RenderKind::Typst,
            Some(e)
                if e.eq_ignore_ascii_case("html")
                    || e.eq_ignore_ascii_case("htm")
                    || e.eq_ignore_ascii_case("xhtml") =>
            {
                RenderKind::Html
            }
            _ => RenderKind::Text,
        }
    }
}

impl Clone for Document {
    /// A clone is a *distinct* buffer, so it gets a **fresh** [`DocId`] — never the source's. This
    /// keeps the "unique id per live buffer" invariant that the split-pane tree relies on structurally
    /// intact even if a document is ever cloned into coexistence. Every other field is copied.
    fn clone(&self) -> Self {
        Document {
            buffer: self.buffer.clone(),
            undo: self.undo.clone(),
            redo: self.redo.clone(),
            coalescing: self.coalescing,
            group_depth: self.group_depth,
            group_active: self.group_active,
            caret_ring: self.caret_ring.clone(),
            saved_len: self.saved_len,
            path: self.path.clone(),
            line_ending: self.line_ending,
            read_only: self.read_only,
            label: self.label.clone(),
            highlighter: self.highlighter.clone(),
            tokens: self.tokens.clone(),
            hl_window: self.hl_window,
            hl_lo: self.hl_lo,
            hl_hi: self.hl_hi,
            hl_doc_len: self.hl_doc_len,
            hl_valid: self.hl_valid,
            hl_gen: self.hl_gen,
            color_spans: self.color_spans.clone(),
            bg_spans: self.bg_spans.clone(),
            attr_spans: self.attr_spans.clone(),
            marks: self.marks.clone(),
            styling_dirty: self.styling_dirty,
            saved_styling_sig: self.saved_styling_sig,
            font_mode: self.font_mode,
            mda_type_fg: self.mda_type_fg,
            mda_type_bg: self.mda_type_bg,
            mda_type_attr: self.mda_type_attr,
            lints: self.lints.clone(),
            gen: self.gen,
            dir: self.dir.clone(),
            grammars: self.grammars.clone(),
            render_override: self.render_override,
            hex_view: self.hex_view,
            counters: self.counters.clone(),
            emph: self.emph.clone(),
            emph_regex: self.emph_regex,
            grep: self.grep.clone(),
            help_listing: self.help_listing,
            id: fresh_doc_id(),
        }
    }
}

impl Default for Document {
    fn default() -> Self {
        Document {
            buffer: Buffer::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            coalescing: false,
            group_depth: 0,
            group_active: false,
            caret_ring: Vec::new(),
            saved_len: Some(0), // a fresh document is unmodified
            path: None,
            line_ending: LineEnding::os_default(), // new docs follow the OS until a file is opened
            read_only: false,
            label: None,
            highlighter: None, // quiet screen by default
            tokens: Vec::new(),
            hl_window: None,
            hl_lo: 0,
            hl_hi: 0,
            hl_doc_len: 0,
            hl_valid: false,
            hl_gen: 0,
            color_spans: Vec::new(),
            bg_spans: Vec::new(),
            attr_spans: Vec::new(),
            marks: Vec::new(),
            styling_dirty: false,
            saved_styling_sig: 0,
            font_mode: FontMode::Auto,
            mda_type_fg: None,
            mda_type_bg: None,
            mda_type_attr: None,
            lints: Vec::new(),
            gen: 0,
            dir: None,
            grammars: Rc::new(RefCell::new(GrammarRegistry::default())),
            render_override: None, // auto: decide by extension
            hex_view: false,       // documents open in the normal text view
            emph: None,
            emph_regex: false,
            grep: None,
            help_listing: false,
            counters: std::array::from_fn(|_| crate::counter::Counter::default()),
            id: fresh_doc_id(),
        }
    }
}

/// The `(cursor, anchor)` pair that [`Buffer::set_caret`] needs to restore the selection `(a, b)` with
/// its original orientation. Used as the undo before-state for `CP`/`MV`, whose selection can be a
/// DEFINED block the caret has roamed away from — in which case `cursor()`/`anchor()` describe the
/// roamed caret and the block's anchor, so restoring them would select everything from the block start
/// to the roamed position instead of the block itself.
fn selection_restore_state(buf: &Buffer, a: usize, b: usize) -> (usize, Option<usize>) {
    if buf.anchor() == Some(b) {
        (a, Some(b)) // backward selection: anchor at the end, caret at the start
    } else {
        (b, Some(a)) // forward (or unknown): anchor at the start, caret at the end
    }
}

impl Document {
    pub fn new() -> Document {
        Document::default()
    }

    // Deliberate infallible constructor, not `std::str::FromStr` (no parse error).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Document {
        Document {
            buffer: Buffer::from_str(s),
            ..Document::default()
        }
    }

    /// This document's live language-check underlines (`SPELLING`/`GRAMMAR`), in rope-byte offsets.
    pub fn lints(&self) -> &[crate::lint::LintMark] {
        &self.lints
    }
    /// Replace this document's language-check underlines (the lint worker's latest result for it).
    pub fn set_lints(&mut self, lints: Vec<crate::lint::LintMark>) {
        self.lints = lints;
    }
    /// Drop this document's language-check underlines (checking turned off / config changed).
    pub fn clear_lints(&mut self) {
        self.lints.clear();
    }

    /// The underlying buffer (for rendering / specials / inspection).
    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// Move the PRIMARY caret to an exact byte offset (the hex view's byte-granular navigation). Only
    /// the primary moves — hex is a single-caret view — so any secondary carets are left untouched.
    pub fn move_primary_to_byte(&mut self, off: usize, extend: bool) {
        self.buffer.move_to_byte(off, extend);
    }

    // ---- multi-caret set (the CARET* verb layer calls these) --------------

    /// Snapshot the caret set onto the `CARETPOP` ring before a set-changing verb mutates it. Bounded
    /// by [`CARET_RING_CAP`] (drop the oldest). A caret-set change also breaks any coalescing run.
    fn push_caret_ring(&mut self) {
        self.coalescing = false;
        self.caret_ring.push(self.buffer.caret_state());
        if self.caret_ring.len() > CARET_RING_CAP {
            self.caret_ring.remove(0);
        }
    }

    /// Add / toggle a secondary caret at byte offset `pos` (`CARETADD <pos>` / Ctrl+click). Returns
    /// the caret count. A caret-set change breaks any coalescing typing run.
    pub fn add_caret(&mut self, pos: usize) -> usize {
        self.push_caret_ring();
        self.buffer.add_caret(pos)
    }

    /// `CARETADD` (bare): plant a caret at the primary and enter the keyboard building state.
    pub fn plant_caret(&mut self) {
        self.push_caret_ring();
        self.buffer.plant_caret_here();
    }

    /// `CARETUP`/`CARETDOWN`: add a caret one line above/below each current caret.
    pub fn add_carets_vertical(&mut self, down: bool) {
        self.push_caret_ring();
        self.buffer.add_carets_vertical(down);
    }

    /// `CARETCOL`: turn a linear selection into a column of carets (one per line at the goal column).
    pub fn add_column_carets(&mut self) {
        self.push_caret_ring();
        self.buffer.add_column_carets();
    }

    /// `CARETALL`/`CARETNEXT`: replace the caret set with one selection-caret per match span.
    pub fn set_selection_carets(&mut self, spans: &[(usize, usize)]) -> usize {
        self.push_caret_ring();
        self.buffer.set_selection_carets(spans)
    }

    /// `CARETSPLIT`: split a multi-line selection into one bare caret per line (at each line's end).
    pub fn split_carets(&mut self) -> usize {
        self.push_caret_ring();
        self.buffer.split_selection_to_line_carets()
    }

    /// Alt+drag box-select: rebuild the caret set from the drag rectangle. Called live on every drag
    /// update, so it does NOT touch the CARETPOP ring (like an ordinary drag-select, it's a gesture).
    pub fn set_box_carets(&mut self, anchor_line: usize, cursor_line: usize, col_a: usize, col_c: usize) {
        self.coalescing = false;
        self.buffer.set_box_carets(anchor_line, cursor_line, col_a, col_c);
    }

    /// `CARETNEXT` seed: select the word under the caret. Returns false if there's no word.
    pub fn select_word_at(&mut self) -> bool {
        self.push_caret_ring();
        self.buffer.select_word_at()
    }

    /// `CARETNEXT`: add `(start, end)` as a new selection-caret and make it primary.
    pub fn add_selection_caret_as_primary(&mut self, start: usize, end: usize) -> usize {
        self.push_caret_ring();
        self.buffer.add_selection_caret_as_primary(start, end)
    }

    /// `CARETSKIP`: MOVE the primary selection to `(start, end)` (skip the current match) without
    /// adding a caret — the secondaries are untouched.
    pub fn move_primary_selection(&mut self, start: usize, end: usize) {
        self.push_caret_ring();
        self.buffer.set_caret(end, Some(start));
    }

    /// `CARETPRIMARY <n>`: make the caret at document-order index `n` the primary. Returns false if
    /// `n` is out of range (the set is left unchanged, and nothing is pushed to the ring).
    pub fn set_primary_caret(&mut self, n: usize) -> bool {
        self.push_caret_ring();
        if self.buffer.set_primary_index(n) {
            true
        } else {
            self.caret_ring.pop(); // no change → don't leave a phantom ring entry
            false
        }
    }

    /// `CARETPOP`: undo the most recent caret-set change, restoring the previous set. Returns false if
    /// the ring is empty (nothing to pop). Cleared by any document edit.
    pub fn caret_pop(&mut self) -> bool {
        self.coalescing = false;
        if let Some(s) = self.caret_ring.pop() {
            self.buffer.set_caret_state(s);
            true
        } else {
            false
        }
    }

    /// Collapse to the single primary caret (`CARETONE`): drop every secondary, end building.
    pub fn collapse_carets(&mut self) {
        self.push_caret_ring();
        self.buffer.collapse_carets();
    }

    /// Number of active carets — primary + secondaries (`$CARETS`).
    pub fn caret_count(&self) -> usize {
        self.buffer.caret_count()
    }

    /// Whether a multi-caret set is active.
    pub fn is_multi(&self) -> bool {
        self.buffer.is_multi()
    }

    /// Apply a per-buffer cursor motion to EVERY caret (breaks any coalescing run). The host's Move
    /// dispatch routes all motion through this, so a single motion verb moves a multi-caret set
    /// together; with a single caret it is just the ordinary move.
    pub fn move_carets(&mut self, motion: impl FnMut(&mut Buffer)) {
        self.coalescing = false;
        self.buffer.move_all_carets(motion);
    }

    /// `LU`/`LD` visual-row move for the PRIMARY caret, using the window's visual-row map (rope-offset
    /// row starts + end sentinel). Returns `false` — the caller then does the logical `UP`/`DOWN`
    /// fallback — for a multi-caret set or when the caret/target is off the mapped window. See
    /// [`Buffer::move_visual_row`](crate::text::Buffer::move_visual_row).
    pub fn move_visual_row(&mut self, down: bool, extend: bool, rows: &[usize]) -> bool {
        if self.buffer.caret_count() > 1 {
            return false; // visual multi-caret vmove is exotic — fall back to the logical fan-out
        }
        let moved = self.buffer.move_visual_row(down, extend, rows);
        if moved {
            self.coalescing = false; // a motion closes the coalescing run, like every other move
        }
        moved
    }

    /// This buffer's stable identity for the split-pane layout tree. Unique per live `Document`,
    /// assigned at construction, never reused. See [`crate::panes`].
    pub fn id(&self) -> crate::panes::DocId {
        self.id
    }

    /// Run a `Cn` counter invocation (`idx` 0-based for `C1`..`C9`). Returns `Ok(Some(text))` to
    /// insert at the caret (an emit), `Ok(None)` for a configure-only `Cn <seed>`, or `Err(msg)`.
    pub fn counter_op(&mut self, idx: usize, arg: &str) -> Result<Option<String>, String> {
        self.counters
            .get_mut(idx)
            .ok_or_else(|| "C: counter index out of range".to_string())?
            .run(arg)
    }

    /// The rendered current value of counter `idx` (what a bare `Cn` would emit, sans decorator) —
    /// the `$Cn` read. `None` if `idx` is out of range.
    pub fn counter_peek(&self, idx: usize) -> Option<String> {
        self.counters.get(idx).map(|c| c.peek())
    }

    /// Clone all nine counters — PRINTEX's save half (see [`Self::restore_counters`]).
    pub fn counters_snapshot(&self) -> [crate::counter::Counter; 9] {
        self.counters.clone()
    }

    /// Replace all nine counters with a snapshot — PRINTEX's restore half.
    pub fn restore_counters(&mut self, snap: [crate::counter::Counter; 9]) {
        self.counters = snap;
    }

    /// A single value that changes whenever the rendered text **or its highlight
    /// colors** change: byte edits (rope rev), plus whole-buffer swaps, encoding
    /// switches, and highlighter toggles (`gen`). A render cache can rebuild only when
    /// this differs and otherwise just re-scroll. Distinct per content state (the
    /// combine's collision odds are negligible); it need not be monotonic.
    pub fn revision(&self) -> u64 {
        self.gen
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(self.buffer.revision())
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    // ---- file identity -----------------------------------------------------

    /// Whether the document has unsaved changes (`$MO`). Text changes are tracked by the undo position
    /// (`saved_len`); styling changes (which also ride the undo stack now, but are persisted to a
    /// SEPARATE sidecar) are tracked by comparing the live styling signature to the saved baseline —
    /// so undoing a style change back to the saved state correctly reports the doc clean again.
    pub fn is_modified(&self) -> bool {
        if self.saved_len != Some(self.undo.len()) {
            return true;
        }
        // `styling_dirty` is only a gate: a doc whose styling was never touched this session skips the
        // signature compare. Once touched, the signature decides (so an undo-to-baseline clears it).
        self.styling_dirty && self.styling_sig() != self.saved_styling_sig
    }

    /// A cheap order-independent-enough signature of the styling persisted to the `.xymeta` sidecar —
    /// the fg/bg/attr spans and the bookmark points. `is_modified` compares it to the saved baseline.
    /// (The span lists are kept sorted + coalesced, so equal styling yields an equal signature.)
    fn styling_sig(&self) -> u64 {
        // FNV-1a over the three span lists and the marks (offset + context, not the session-only
        // `moved` flag). Span lists are small in practice; this runs only when `styling_dirty`.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut mix = |x: u64| {
            h ^= x;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        };
        for &(s, e, (r, g, b)) in &self.color_spans {
            mix(s as u64);
            mix(e as u64);
            mix(((r as u64) << 16) | ((g as u64) << 8) | b as u64);
        }
        mix(0xF0); // layer separator, so an fg span can't alias a bg span at the same coords
        for &(s, e, (r, g, b)) in &self.bg_spans {
            mix(s as u64);
            mix(e as u64);
            mix(((r as u64) << 16) | ((g as u64) << 8) | b as u64);
        }
        mix(0xA7);
        for &(s, e, a) in &self.attr_spans {
            mix(s as u64);
            mix(e as u64);
            mix(a as u64);
        }
        mix(0x3C);
        for m in &self.marks {
            mix(m.off as u64);
            mix(m.split as u64);
            for &byte in &m.ctx {
                mix(byte as u64);
            }
            mix(0xFF); // terminator between marks
        }
        h
    }

    /// Record the current styling as the saved baseline (styling now matches disk / the sidecar).
    /// Clears the `styling_dirty` gate too. Called on save and on a freshly-opened/ restored doc.
    fn mark_styling_baseline(&mut self) {
        self.styling_dirty = false;
        self.saved_styling_sig = self.styling_sig();
    }

    /// The backing file path, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The file name (last path component), or the synthetic `label` for file-less buffers.
    pub fn file_name(&self) -> Option<String> {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .or_else(|| self.label.clone())
    }

    /// The synthetic display label (set for file-less buffers like `*Shell Output*`).
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }
    pub fn set_label(&mut self, label: String) {
        self.label = Some(label);
    }

    /// Append `text` at the end as a programmatic log entry (e.g. background-job output): inserts
    /// past the read-only gate, keeps the buffer "clean" (no save-on-close prompt), and locks it.
    pub fn append_log(&mut self, text: &str) {
        // Append at the end, but KEEP the caret where it was — each caller sets its own resting
        // position afterward (the shell-output log lands it at the end to follow the tail; the
        // *Language Check* report leaves it at the top). `truncate_log_front` shifts a pinned caret
        // with any trimmed front so it tracks the content.
        let saved = self.buffer.cursor();
        let end = self.buffer.len();
        self.buffer.set_caret(end, None);
        self.insert(text);
        self.buffer.set_caret(saved.min(self.buffer.len()), None);
        self.mark_saved(); // a log isn't a document to save
        self.read_only = true; // the user can't type into it (we appended programmatically)
    }

    /// Append streamed CONSOLE output at the tail, keeping the buffer EDITABLE. Unlike [`append_log`]
    /// this does NOT record an undo unit (so Ctrl+Z only unwinds the user's own edits, never streamed
    /// output) and does NOT lock the buffer read-only. The caret tail-follows only when it was already
    /// at the very end with no selection — so streaming never yanks a caret or clears a selection while
    /// the user reads or edits scrollback above. `truncate_log_front` still bounds the scrollback.
    pub fn append_console(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let end = self.buffer.len();
        let follow = self.buffer.cursor() == end && !self.buffer.has_selection();
        self.buffer.append_str(text); // tail insert, no caret/selection touch, not recorded
        if follow {
            let new_end = self.buffer.len();
            self.buffer.set_caret(new_end, None); // ride the tail
        }
        self.mark_saved(); // a console isn't a document to save
        self.gen = next_content_gen(); // the raw append is outside the rope-rev the GUI keys on
    }

    /// Splice streamed CONSOLE output in at byte `at` (typically the start of the prompt line), so the
    /// prompt + the user's in-progress input — which sit AFTER `at` — ride DOWN intact and stay pinned
    /// at the bottom. Editable and NON-recording (like [`append_console`]); the caret and any selection
    /// shift right by the inserted length when they're at or after `at`. Returns the bytes inserted so
    /// the host can advance its prompt anchor.
    pub fn splice_console(&mut self, at: usize, text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }
        let n = self.buffer.insert_str_at(at, text);
        self.adjust_color_spans(at, 0, n); // keep ANSI colour spans aligned across the mid-buffer insert
        let shift = |x: usize| if x >= at { x + n } else { x };
        let cursor = shift(self.buffer.cursor());
        let anchor = self.buffer.anchor().map(shift);
        self.buffer.set_caret(cursor, anchor);
        self.mark_saved();
        self.gen = next_content_gen();
        n
    }

    /// Clear the Console: empty the buffer and its colour/attribute spans, non-recording. Used by
    /// `CLS` / `CLEAR`; the host then writes a fresh prompt.
    pub fn console_clear(&mut self) {
        let len = self.buffer.len();
        if len > 0 {
            self.buffer.replace_range_bytes(0, len, &[]);
        }
        self.color_spans.clear();
        self.bg_spans.clear();
        self.attr_spans.clear();
        self.buffer.set_caret(0, None);
        self.undo.clear();
        self.redo.clear();
        self.saved_len = Some(0);
        self.gen = next_content_gen();
    }

    /// Replace the Console's current input — everything from byte `at` (just after the prompt) to the
    /// end — with `text`, non-recording, caret to the new end. Used by Shift+Up/Down history recall.
    pub fn console_replace_input(&mut self, at: usize, text: &str) {
        let end = self.buffer.len();
        let at = at.min(end);
        if end > at {
            self.buffer.replace_range_bytes(at, end - at, &[]); // clear the old input
        }
        self.buffer.insert_str_at(at, text); // encode + insert the recalled command
        let new_end = self.buffer.len();
        self.buffer.set_caret(new_end, None);
        self.gen = next_content_gen();
    }

    /// Cap a log buffer to roughly its last `max_bytes`, dropping the oldest content from the
    /// front (so a flood like `dir /s C:\` can't grow it without bound). Cuts on a line boundary
    /// when possible, else a UTF-8 char boundary; no-op when already within the cap. Returns the
    /// number of bytes trimmed off the front (`0` when within the cap), so a caller tracking an
    /// absolute offset (the Console's prompt anchor) can slide it left to match.
    pub fn truncate_log_front(&mut self, max_bytes: usize) -> usize {
        let len = self.buffer.len();
        if len <= max_bytes {
            return 0;
        }
        let mut cut = len - max_bytes;
        // Snap forward to just past the next newline (drop whole lines); if none nearby, snap to
        // the next UTF-8 char boundary so we never split a multi-byte char.
        let probe = self.buffer.slice_bytes(cut, (cut + 8192).min(len));
        let snap = probe
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| p + 1)
            .or_else(|| probe.iter().position(|&b| b & 0xC0 != 0x80))
            .unwrap_or(0);
        cut += snap;
        let saved = self.buffer.cursor();
        self.buffer.replace_range_bytes(0, cut, &[]);
        self.adjust_color_spans(0, cut, 0); // slide ANSI colour spans left with the trimmed front
        // Shift the caret left with the dropped front so a pinned position (the start of a job's
        // output) tracks the content; clamp to 0 if that position was itself trimmed away.
        self.buffer.set_caret(saved.saturating_sub(cut).min(self.buffer.len()), None);
        self.undo.clear();
        self.redo.clear();
        self.coalescing = false;
        self.saved_len = Some(0);
        self.gen = next_content_gen(); // force the GUI to re-render the trimmed content
        cut
    }

    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// Whether the buffer content is locked against edits (`RO`).
    pub fn read_only(&self) -> bool {
        self.read_only
    }
    pub fn set_read_only(&mut self, on: bool) {
        self.read_only = on;
    }

    /// The current encoding view's name (for `$`-specials / status / the
    /// `ENCODING` verb).
    pub fn encoding_name(&self) -> &str {
        self.buffer.encoding_name()
    }

    /// Switch the encoding view (live EditPad-style reinterpret). The bytes are
    /// unchanged, so this is **not** an edit and not undoable; it just closes any
    /// open coalescing run.
    pub fn set_encoding(&mut self, enc: Charset) {
        self.coalescing = false;
        self.gen = next_content_gen(); // re-decode changes the rendered text
        self.buffer.set_encoding(enc);
        self.refresh_highlights(); // the decoded source may differ under the new view
    }

    /// CONVERT to `enc` (`ENCODING/C`): re-encode the current text into the target charset and adopt
    /// it as the new bytes + view. LOSSY — characters the target can't represent become `?`. The
    /// bytes change, so this is a modifying whole-buffer op (clears undo, caret to top, marks dirty).
    pub fn convert_to_encoding(&mut self, enc: Charset) {
        let text = self.buffer.text();
        let bytes = enc.encode(&text);
        self.gen = next_content_gen();
        self.buffer = Buffer::from_bytes(bytes, enc);
        self.buffer.set_caret(0, None);
        self.undo.clear();
        self.redo.clear();
        self.coalescing = false;
        self.saved_len = None; // bytes changed → modified
        self.refresh_highlights();
    }

    /// Convert every line ending to `target` (`NEWLINE lf|crlf`) and adopt it as the doc's style.
    /// The rewrite is byte-level (`\r`=0x0D/`\n`=0x0A are identical across our encodings), so it's
    /// not lossy. If the bytes actually change it's a modifying whole-buffer op.
    pub fn convert_line_endings(&mut self, target: LineEnding) {
        let bytes = self.buffer.bytes();
        let out = normalize_newlines(&bytes, target);
        if out != bytes {
            let enc = self.buffer.encoding();
            self.gen = next_content_gen();
            self.buffer = Buffer::from_bytes(out, enc);
            self.buffer.set_caret(0, None);
            self.undo.clear();
            self.redo.clear();
            self.coalescing = false;
            self.saved_len = None; // bytes changed → modified
            self.refresh_highlights();
        }
        self.line_ending = target;
    }

    // ---- syntax highlighting (off by default) ------------------------------

    /// Share the host's grammar registry into this document so it can resolve its own
    /// highlighter by file extension (`HLLANG` grammars). Without this a document only
    /// knows the built-in Markdown highlighter.
    pub fn share_grammars(&mut self, grammars: Rc<RefCell<GrammarRegistry>>) {
        self.grammars = grammars;
    }

    /// The effective render kind for `TOPDF`/preview: the `DOCTYPE` override if set, else by file
    /// extension (`.md`/`.markdown` → GFM, `.typ` → Typst, `.html`/`.htm`/`.xhtml` → HTML, everything
    /// else → plain Text).
    pub fn render_kind(&self) -> RenderKind {
        self.render_override.unwrap_or_else(|| self.render_kind_from_ext())
    }

    /// The extension-driven render kind (ignores any override): `.md`/`.markdown` → Gfm, `.typ` →
    /// Typst, `.html`/`.htm`/`.xhtml` → Html, everything else (`.txt`, `.log`, `.rs`, no extension, …)
    /// → plain Text (rendered verbatim, no Markdown interpretation).
    fn render_kind_from_ext(&self) -> RenderKind {
        self.path.as_deref().map(RenderKind::from_path).unwrap_or(RenderKind::Text)
    }

    /// Set (or clear, with `None` = auto) the `DOCTYPE` render-kind override.
    pub fn set_render_override(&mut self, kind: Option<RenderKind>) {
        self.render_override = kind;
    }

    /// The `DOCTYPE` override as set (`None` = auto/by-extension) — for session persistence, which
    /// records exactly what the user chose rather than the effective kind.
    pub fn render_override(&self) -> Option<RenderKind> {
        self.render_override
    }

    /// `HEX on|off`: whether this document is shown as a hex + text grid rather than the normal
    /// text canvas. A view mode only — the bytes are unchanged.
    pub fn set_hex_view(&mut self, on: bool) {
        self.hex_view = on;
    }
    /// Whether the hex + text view is active for this document.
    pub fn hex_view(&self) -> bool {
        self.hex_view
    }

    /// The `EMPH` emphasis phrase for this document (`None` = none). Every case-insensitive
    /// occurrence is highlighted by the GUI as a visual overlay.
    pub fn emph(&self) -> Option<&str> {
        self.emph.as_deref()
    }

    /// Whether this document's `EMPH` phrase is a regular expression (`EMPH/R`) vs a whole-word literal.
    pub fn emph_is_regex(&self) -> bool {
        self.emph.is_some() && self.emph_regex
    }

    /// Set (or clear, with `None`) the `EMPH` emphasis phrase for this document. `regex` marks it as a
    /// regular expression (`EMPH/R`); it is irrelevant (and reset) when the phrase is cleared.
    pub fn set_emph(&mut self, phrase: Option<String>, regex: bool) {
        self.emph = phrase.filter(|p| !p.is_empty());
        self.emph_regex = self.emph.is_some() && regex;
    }

    /// Force the buffer to report as modified (`is_modified()` → true) without editing it. Used by
    /// session RESTORE, where a shadow-backed buffer must come back showing its unsaved state so a
    /// later `SAVE` writes it to the real file. (`saved_len = None` can never equal `Some(undo.len())`.)
    pub fn mark_modified(&mut self) {
        self.saved_len = None;
    }

    /// Drop the backing path, turning the document back into an untitled buffer (keeping its content).
    /// Session RESTORE uses this to rebuild an untitled tab from a shadow backup.
    pub fn forget_path(&mut self) {
        self.path = None;
    }

    /// The highlighter for this document's path: a user grammar matching the extension
    /// wins, else the built-in (Markdown). `None` ⇒ nothing highlights this file type.
    fn resolve_highlighter(&self) -> Option<Highlighter> {
        if let Some(ext) = self.path.as_deref().and_then(|p| p.extension()).and_then(|e| e.to_str()) {
            if let Some(g) = self.grammars.borrow().for_ext(ext) {
                return Some(Highlighter::Regex(g.clone()));
            }
        }
        Highlighter::for_path(self.path.as_deref())
    }

    /// Turn highlighting on/off, resolving the highlighter by file type (grammar by
    /// extension, else Markdown; `None` if unknown). Not an edit / not undoable.
    pub fn set_highlight(&mut self, on: bool) {
        let h = if on { self.resolve_highlighter() } else { None };
        self.set_highlighter(h);
    }

    /// Set the active highlighter directly (the host resolves grammars by extension
    /// and hands the document a self-contained snapshot). `None` turns it off.
    pub fn set_highlighter(&mut self, h: Option<Highlighter>) {
        self.gen = next_content_gen(); // recolor changes the rendered spans
        self.highlighter = h;
        self.refresh_highlights();
    }

    /// The active highlighter's name (e.g. `"markdown"`, or a grammar name), or
    /// `None` when off.
    pub fn highlighter_name(&self) -> Option<&str> {
        self.highlighter.as_ref().map(|h| h.name())
    }

    /// The cached highlight tokens for the current buffer (empty when off).
    pub fn highlights(&self) -> &[Token] {
        &self.tokens
    }

    /// Place the caret/selection directly (e.g. from a GUI mouse click). A cursor
    /// jump, so it closes any coalescing run; not an edit. `anchor` `Some` makes a
    /// selection between `anchor` and `cursor`.
    pub fn set_caret(&mut self, cursor: usize, anchor: Option<usize>) {
        self.coalescing = false;
        self.buffer.set_caret(cursor, anchor);
    }

    /// Tokenize the whole-line span covering decoded `[lo, hi]` (via
    /// [`windowed_lines`](crate::text::Buffer::windowed_lines) with `pad`) and return `(base, end,
    /// tokens)` — the covered decoded range and its tokens, offsets already absolute. Caller must have
    /// checked the highlighter is `Some`. `base`/`end` are line boundaries, so tokens never straddle
    /// them (the grammar is per-line), which is what makes the incremental splicing below exact.
    fn tokenize_span(&self, lo: usize, hi: usize, pad: usize) -> (usize, usize, Vec<Token>) {
        let (src, base, end) = self.buffer.windowed_lines(lo, hi, pad);
        let mut toks = self.highlighter.as_ref().unwrap().highlight(&src);
        for t in &mut toks {
            t.start += base;
            t.end += base;
        }
        (base, end, toks)
    }

    /// Rebuild the highlight cache from scratch over a WINDOW around the view (or the caret when the GUI
    /// hasn't reported a visible range yet) — the fallback when the incremental paths can't be used
    /// (first light, an edit outside the covered range, a view jump). A document smaller than the padding
    /// is covered whole, so small files and tests behave as before. The grammar is per-line and
    /// stateless, so tokenizing an arbitrary line span is correct with no cross-line context.
    fn refresh_highlights(&mut self) {
        if self.highlighter.is_none() {
            self.tokens.clear();
            self.hl_valid = false;
            self.hl_gen = self.gen;
            return;
        }
        let (clo, chi) = self.hl_window.unwrap_or_else(|| {
            let c = self.buffer.decoded_offset(self.buffer.cursor());
            (c, c)
        });
        let (base, end, toks) = self.tokenize_span(clo, chi, HL_WINDOW_PAD);
        self.tokens = toks;
        self.hl_lo = base;
        self.hl_hi = end;
        self.hl_doc_len = self.buffer.decoded_len();
        self.hl_valid = true;
        self.hl_gen = self.gen;
    }

    /// Re-highlight just the line(s) an edit touched, in place — the key to staying fast on a big file
    /// (a keystroke re-tokenizes one line, not the whole visible window). Called from the edit sink with
    /// the applied edit's ROPE offset and inserted byte length; the buffer is already in its new state.
    ///
    /// Splices the token list: tokens before the changed line(s) are untouched, tokens after shift by the
    /// edit's decoded delta (derived from the buffer's total decoded-length change — no need to decode the
    /// changed bytes), and the changed line(s) are re-tokenized. If the cache isn't usable or the edit
    /// falls outside the covered range, it just invalidates so the next [`ensure_highlights`] rebuilds.
    fn note_edit(&mut self, at_rope: usize, inserted_rope_len: usize) {
        if self.highlighter.is_none() {
            self.tokens.clear();
            self.hl_valid = false;
            self.hl_gen = self.gen;
            return;
        }
        let new_doc_len = self.buffer.decoded_len();
        if !self.hl_valid {
            self.hl_doc_len = new_doc_len; // nothing cached; ensure_highlights will build (hl_gen stale)
            return;
        }
        let at_dec = self.buffer.decoded_offset(at_rope);
        // Only splice when the edit is inside the covered range; otherwise let ensure rebuild.
        if at_dec < self.hl_lo || at_dec > self.hl_hi {
            self.hl_valid = false;
            return;
        }
        let delta = new_doc_len as isize - self.hl_doc_len as isize;
        let ins_dec = self.buffer.decoded_offset(at_rope + inserted_rope_len).saturating_sub(at_dec);
        // The whole new line(s) the edit now occupies, and the OLD range they replaced.
        let (src, alo, ahi) = self.buffer.windowed_lines(at_dec, at_dec + ins_dec, 0);
        let old_ahi = (ahi as isize - delta).max(0) as usize;
        if old_ahi > self.hl_hi {
            self.hl_valid = false; // the change reaches past the covered tail — rebuild instead
            return;
        }
        let mut new_toks = self.highlighter.as_ref().unwrap().highlight(&src);
        for t in &mut new_toks {
            t.start += alo;
            t.end += alo;
        }
        // before (ends at/at-boundary ≤ alo) ++ re-tokenized line(s) ++ after (starts ≥ old_ahi, shifted).
        let mut out = Vec::with_capacity(self.tokens.len() + new_toks.len());
        out.extend(self.tokens.iter().filter(|t| t.end <= alo).cloned());
        out.extend(new_toks);
        for t in &self.tokens {
            if t.start >= old_ahi {
                out.push(Token {
                    start: (t.start as isize + delta) as usize,
                    end: (t.end as isize + delta) as usize,
                    kind: t.kind.clone(),
                });
            }
        }
        self.tokens = out;
        self.hl_hi = (self.hl_hi as isize + delta).max(alo as isize) as usize;
        self.hl_doc_len = new_doc_len;
        self.hl_gen = self.gen;
    }

    /// Ensure the highlight cache covers the DECODED range `[dec_lo, dec_hi]` (the renderer's visible
    /// span). Called each frame by the GUI before it reads [`highlights`](Document::highlights); a no-op
    /// while the view sits still over already-lit text. On a scroll it EXTENDS the covered range by
    /// tokenizing only the newly-revealed lines (not the whole window); a stale cache or a far jump falls
    /// back to a full window [`refresh_highlights`](Document::refresh_highlights).
    pub fn ensure_highlights(&mut self, dec_lo: usize, dec_hi: usize) {
        if self.highlighter.is_none() {
            return;
        }
        self.hl_window = Some((dec_lo, dec_hi));
        // Stale (an edit the incremental path didn't handle) or never built → full rebuild.
        if !self.hl_valid || self.hl_gen != self.gen {
            self.refresh_highlights();
            return;
        }
        if dec_lo >= self.hl_lo && dec_hi <= self.hl_hi {
            return; // already covered
        }
        // A view jump with no overlap: cheaper to rebuild one window than to tokenize the huge gap.
        if dec_hi < self.hl_lo || dec_lo > self.hl_hi {
            self.refresh_highlights();
            return;
        }
        // Scroll: extend only past the covered edge(s), padded forward so small scrolls stay covered.
        if dec_hi > self.hl_hi {
            let (base, end, toks) = self.tokenize_span(self.hl_hi, dec_hi + HL_WINDOW_PAD, 0);
            debug_assert_eq!(base, self.hl_hi, "down-extend must abut the covered tail");
            self.tokens.extend(toks);
            self.hl_hi = end;
        }
        if dec_lo < self.hl_lo {
            // End at `hl_lo - 1` (a position in the PREVIOUS line) so the span stops exactly at the
            // covered head's line boundary — `windowed_lines(_, hl_lo, _)` would re-include hl_lo's own
            // line and duplicate its tokens.
            let (base, end, mut toks) =
                self.tokenize_span(dec_lo.saturating_sub(HL_WINDOW_PAD), self.hl_lo - 1, 0);
            debug_assert_eq!(end, self.hl_lo, "up-extend must abut the covered head");
            toks.append(&mut self.tokens);
            self.tokens = toks;
            self.hl_lo = base;
        }
    }

    /// The current selection as a `(start, end)` ROPE-byte range, or `None` when there's no
    /// selection. Ordered (start ≤ end).
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        if !self.buffer.has_selection() {
            return None;
        }
        let a = self.buffer.anchor()?;
        let c = self.buffer.cursor();
        Some((a.min(c), a.max(c)))
    }

    /// Apply (or, with `color == None`, CLEAR) a transient foreground colour over the ROPE-byte
    /// range `[start, end)`: any existing span there is trimmed away first (interval subtraction),
    /// then — for a set — the new span is added. Keeps the list sorted and non-overlapping. Bumps
    /// `gen` so the renderer re-colours. Used by `FGCOLOR <rgb>` / `FGCOLOR default` on a selection.
    pub fn set_color_span(&mut self, start: usize, end: usize, color: Option<(u8, u8, u8)>) {
        Self::set_span_in(&mut self.color_spans, start, end, color);
        self.gen = self.gen.wrapping_add(1);
        self.styling_dirty = true; // persisted to the sidecar, so it counts as an unsaved change
    }

    /// Like [`set_color_span`](Self::set_color_span) but for the BACKGROUND layer (`MDA/B`).
    pub fn set_bg_color_span(&mut self, start: usize, end: usize, color: Option<(u8, u8, u8)>) {
        Self::set_span_in(&mut self.bg_spans, start, end, color);
        self.gen = self.gen.wrapping_add(1);
        self.styling_dirty = true;
    }

    // ---- MDA live-typing mode (per-document) -------------------------------------------------------

    /// Arm/clear the typing-mode FOREGROUND colour (`Some` sets it, `None` clears it).
    pub fn set_typing_fg(&mut self, c: Option<(u8, u8, u8)>) {
        self.mda_type_fg = c;
    }
    /// Arm/clear the typing-mode BACKGROUND colour.
    pub fn set_typing_bg(&mut self, c: Option<(u8, u8, u8)>) {
        self.mda_type_bg = c;
    }
    /// Arm/clear the typing-mode text ATTRIBUTES (`None`/`0` clears).
    pub fn set_typing_attr(&mut self, a: Option<u8>) {
        self.mda_type_attr = a.filter(|&a| a != 0);
    }
    /// A bare `MDA` cancels the foreground-colour AND attribute typing modes (the non-`/B` layers).
    pub fn clear_typing_fg_attr(&mut self) {
        self.mda_type_fg = None;
        self.mda_type_attr = None;
    }
    /// Whether any `MDA` typing-mode layer is armed for this document (for the status indicator).
    pub fn mda_typing_active(&self) -> bool {
        self.mda_type_fg.is_some() || self.mda_type_bg.is_some() || self.mda_type_attr.is_some()
    }
    /// The armed `MDA` typing-mode layers `(foreground, background, attributes)` — for an informative
    /// status indicator (colour swatches + attribute letters).
    pub fn mda_typing(&self) -> (Option<(u8, u8, u8)>, Option<(u8, u8, u8)>, Option<u8>) {
        (self.mda_type_fg, self.mda_type_bg, self.mda_type_attr)
    }

    /// Style the run just typed — the range from `from` to the current caret — with any armed typing
    /// mode. A no-op when no mode is active or nothing advanced. Each layer paints its span, exactly as
    /// an `MDA` on a selection would.
    pub fn apply_typing_style(&mut self, from: usize) {
        let (fg, bg, at) = (self.mda_type_fg, self.mda_type_bg, self.mda_type_attr);
        if fg.is_none() && bg.is_none() && at.is_none() {
            return;
        }
        let to = self.buffer.cursor();
        if to <= from {
            return; // caret didn't advance (e.g. an empty insert)
        }
        if let Some(rgb) = fg {
            self.set_color_span(from, to, Some(rgb));
        }
        if let Some(rgb) = bg {
            self.set_bg_color_span(from, to, Some(rgb));
        }
        if let Some(a) = at {
            self.set_attr_span(from, to, Some(a));
        }
        // Make the typed run's styling ride the undo record so REDO restores it (this runs right after
        // the insert, so the top undo unit's last op IS that insert — or the coalescing run it merged
        // into). Recompute the op's whole inserted-run styling from the doc each keystroke, so a
        // coalescing run accumulates correctly.
        self.stamp_typed_run_styling();
    }

    /// Snapshot the current styling of the TOP undo unit's last op's inserted run into its
    /// `inserted_spans`, so a redo of that (typing-mode) insert re-applies the style. A no-op when the
    /// top isn't a plain insert (nothing to stamp). See [`apply_typing_style`](Self::apply_typing_style).
    fn stamp_typed_run_styling(&mut self) {
        let Some((at, len)) = self.undo.last().and_then(|u| u.ops.last()).map(|o| (o.at, o.inserted.len()))
        else {
            return;
        };
        if len == 0 {
            return;
        }
        let snap = self.capture_spans(at, len);
        if let Some(op) = self.undo.last_mut().and_then(|u| u.ops.last_mut()) {
            op.inserted_spans = snap;
        }
    }

    /// Apply (or, with `attrs == None`, CLEAR) `MDA` text attributes over ROPE range `[start, end)`.
    /// Replace semantics like the colour layers — the region's attributes become exactly `attrs` (a
    /// bitset of [`ATTR_BOLD`]/[`ATTR_ITALIC`]/[`ATTR_UNDERLINE`]); `None` (or an empty set) strips them.
    pub fn set_attr_span(&mut self, start: usize, end: usize, attrs: Option<u8>) {
        // An all-zero bitset means "no attributes" — treat it as a clear so we never store an empty span.
        Self::set_span_in(&mut self.attr_spans, start, end, attrs.filter(|&a| a != 0));
        self.gen = self.gen.wrapping_add(1);
        self.styling_dirty = true;
    }

    /// Apply (or, with `value == None`, CLEAR) a payload over ROPE range `[start, end)` in one span
    /// list: subtract any overlap first (interval subtraction), then — for a set — add the new span.
    /// Keeps the list sorted and non-overlapping. Shared by the FG / BG colour and attribute layers.
    fn set_span_in<P: Copy + PartialEq>(
        list: &mut Vec<(usize, usize, P)>,
        start: usize,
        end: usize,
        value: Option<P>,
    ) {
        if start >= end {
            return;
        }
        let mut out: Vec<(usize, usize, P)> = Vec::new();
        for &(ss, se, c) in list.iter() {
            if se <= start || ss >= end {
                out.push((ss, se, c)); // no overlap — keep
            } else {
                if ss < start {
                    out.push((ss, start, c)); // left remainder
                }
                if end < se {
                    out.push((end, se, c)); // right remainder
                }
                // the covered middle [max(ss,start), min(se,end)) is dropped
            }
        }
        if let Some(v) = value {
            out.push((start, end, v));
        }
        out.sort_by_key(|&(s, _, _)| s);
        // Coalesce touching runs of the SAME value into one span — otherwise typing char-by-char in a
        // TYPING MODE would leave a trail of 1-byte spans (bloating the list and the `.xymeta` sidecar).
        let mut merged: Vec<(usize, usize, P)> = Vec::with_capacity(out.len());
        for span in out {
            match merged.last_mut() {
                Some(prev) if prev.1 == span.0 && prev.2 == span.2 => prev.1 = span.1,
                _ => merged.push(span),
            }
        }
        *list = merged;
    }

    /// Apply (or, with `value == None`, CLEAR) `value` over MANY ROPE ranges in ONE pass — the bulk
    /// analogue of [`set_span_in`], for `MDALL` (style every match). Calling `set_span_in` per range
    /// would be O(ranges × spans) (it rebuilds + re-sorts the whole list each time); this subtracts all
    /// the ranges from the existing spans, adds the new ones, then sorts + coalesces exactly once.
    /// `ranges` need not be sorted or disjoint; empty ranges (`start >= end`) are skipped.
    fn set_spans_bulk<P: Copy + PartialEq>(
        list: &mut Vec<(usize, usize, P)>,
        ranges: &[(usize, usize)],
        value: Option<P>,
    ) {
        // Normalize + sort the target ranges so overlap tests are a linear sweep.
        let mut cuts: Vec<(usize, usize)> = ranges.iter().copied().filter(|&(s, e)| s < e).collect();
        if cuts.is_empty() {
            return;
        }
        cuts.sort_unstable();
        // Subtract every target range from each existing span (interval subtraction), keeping the
        // uncovered remainders. A span can be split by several ranges, so walk the sorted cuts.
        let mut out: Vec<(usize, usize, P)> = Vec::with_capacity(list.len() + cuts.len());
        for &(ss, se, c) in list.iter() {
            let mut pos = ss;
            for &(cs, ce) in &cuts {
                if ce <= pos || cs >= se {
                    continue; // this cut doesn't touch what's left of the span
                }
                if cs > pos {
                    out.push((pos, cs.min(se), c)); // keep the uncovered left remainder
                }
                pos = pos.max(ce);
                if pos >= se {
                    break;
                }
            }
            if pos < se {
                out.push((pos, se, c)); // trailing uncovered remainder
            }
        }
        // Add the new spans (a set); a clear just leaves the holes.
        if let Some(v) = value {
            for &(cs, ce) in &cuts {
                out.push((cs, ce, v));
            }
        }
        out.sort_by_key(|&(s, _, _)| s);
        let mut merged: Vec<(usize, usize, P)> = Vec::with_capacity(out.len());
        for span in out {
            match merged.last_mut() {
                Some(prev) if prev.1 == span.0 && prev.2 == span.2 => prev.1 = span.1,
                _ => merged.push(span),
            }
        }
        *list = merged;
    }

    /// Bulk-apply a FOREGROUND colour to many ranges (`MDALL`). `None` clears them.
    pub fn set_color_spans_bulk(&mut self, ranges: &[(usize, usize)], color: Option<(u8, u8, u8)>) {
        Self::set_spans_bulk(&mut self.color_spans, ranges, color);
        self.gen = self.gen.wrapping_add(1);
        self.styling_dirty = true;
    }

    /// Bulk-apply a BACKGROUND colour to many ranges (`MDALL/B`). `None` clears them.
    pub fn set_bg_color_spans_bulk(&mut self, ranges: &[(usize, usize)], color: Option<(u8, u8, u8)>) {
        Self::set_spans_bulk(&mut self.bg_spans, ranges, color);
        self.gen = self.gen.wrapping_add(1);
        self.styling_dirty = true;
    }

    /// Bulk-apply text ATTRIBUTES to many ranges (`MDALL BO|IT|…`). `None` (or an all-zero bitset)
    /// clears them.
    pub fn set_attr_spans_bulk(&mut self, ranges: &[(usize, usize)], attrs: Option<u8>) {
        Self::set_spans_bulk(&mut self.attr_spans, ranges, attrs.filter(|&a| a != 0));
        self.gen = self.gen.wrapping_add(1);
        self.styling_dirty = true;
    }

    /// This document's canvas font MODE (`USEFONT`; session-only). Not an edit / not undoable.
    pub fn font_mode(&self) -> FontMode {
        self.font_mode
    }
    pub fn set_font_mode(&mut self, mode: FontMode) {
        self.font_mode = mode;
    }
    /// Whether syntax highlighting is active on this document (a highlighter is assigned). The
    /// proportional-font rule treats a highlighted buffer as "code" → monospace in `Auto` mode.
    pub fn is_highlighting(&self) -> bool {
        self.highlighter.is_some()
    }

    /// Shift ALL span layers (both colours + attributes) to follow an edit at byte `at` that removed
    /// `removed` and inserted `inserted` bytes (called from `record`, so they track typing/deletion).
    /// Points inside the removed range collapse to `at`; spans emptied by the edit are dropped.
    fn adjust_color_spans(&mut self, at: usize, removed: usize, inserted: usize) {
        Self::shift_spans(&mut self.color_spans, at, removed, inserted);
        Self::shift_spans(&mut self.bg_spans, at, removed, inserted);
        Self::shift_spans(&mut self.attr_spans, at, removed, inserted);
        Self::shift_marks(&mut self.marks, at, removed, inserted);
    }

    /// Snapshot the MDA styling over the ROPE range `[at, at+len)` from all three layers, clipped to
    /// the region and rebased so offset 0 is `at`. The inverse of [`apply_span_snapshot`]. Empty (and
    /// cheap) when no span overlaps — the usual case, so the typing/undo path pays nothing.
    fn capture_spans(&self, at: usize, len: usize) -> SpanSnapshot {
        SpanSnapshot {
            fg: Self::clip_rebase(&self.color_spans, at, len),
            bg: Self::clip_rebase(&self.bg_spans, at, len),
            attr: Self::clip_rebase(&self.attr_spans, at, len),
        }
    }

    /// Collect one span list's overlap with `[at, at+len)`, clipped to the region and rebased to 0.
    fn clip_rebase<P: Copy>(list: &[(usize, usize, P)], at: usize, len: usize) -> Vec<(usize, usize, P)> {
        if len == 0 {
            return Vec::new();
        }
        let end = at + len;
        list.iter()
            .filter_map(|&(s, e, v)| {
                let (s2, e2) = (s.max(at), e.min(end));
                (s2 < e2).then_some((s2 - at, e2 - at, v))
            })
            .collect()
    }

    /// Re-apply a captured [`SpanSnapshot`] with offset 0 mapped to the ROPE offset `at`. Used by undo
    /// (restore the styling an edit removed) and by MV / COPY-to-cursor / styled paste + redo (style
    /// the inserted text). A no-op for an empty snapshot.
    fn apply_span_snapshot(&mut self, at: usize, snap: &SpanSnapshot) {
        if snap.is_empty() {
            return;
        }
        for &(s, e, c) in &snap.fg {
            Self::set_span_in(&mut self.color_spans, at + s, at + e, Some(c));
        }
        for &(s, e, c) in &snap.bg {
            Self::set_span_in(&mut self.bg_spans, at + s, at + e, Some(c));
        }
        for &(s, e, a) in &snap.attr {
            Self::set_span_in(&mut self.attr_spans, at + s, at + e, Some(a));
        }
        self.gen = self.gen.wrapping_add(1);
        self.styling_dirty = true;
    }

    /// Replace ALL styling in the ROPE region `[lo, hi)` with `snap` (rebased to `lo`) — used by
    /// undo/redo of a [`StyleDelta`] to put the region back exactly as it was before/after the command.
    fn restore_style_region(&mut self, lo: usize, hi: usize, snap: &SpanSnapshot) {
        Self::set_span_in(&mut self.color_spans, lo, hi, None);
        Self::set_span_in(&mut self.bg_spans, lo, hi, None);
        Self::set_span_in(&mut self.attr_spans, lo, hi, None);
        self.apply_span_snapshot(lo, snap); // also bumps gen + styling_dirty
        self.gen = self.gen.wrapping_add(1);
        self.styling_dirty = true;
    }

    /// Finish an undoable styling command: snapshot the region AGAIN (its `after` state) and, if it
    /// actually changed, push a pure styling [`Unit`] so Ctrl+Z reverts it. `before` is the snapshot
    /// taken just before the change. Mirrors [`record`](Self::record)'s bookkeeping (redo-clear,
    /// saved-state tracking) but carries a [`StyleDelta`] instead of text ops. A styling change inside
    /// an open undo GROUP still records its own step (groups fold TEXT edits; styling is standalone).
    fn record_style_change(&mut self, lo: usize, hi: usize, before: SpanSnapshot) {
        let after = self.capture_spans(lo, hi);
        if before == after {
            return; // a no-op command (e.g. clearing an already-unstyled run) adds no undo step
        }
        self.caret_ring.clear();
        self.coalescing = false;
        if let Some(sl) = self.saved_len {
            if sl > self.undo.len() {
                self.saved_len = None;
            }
        }
        self.redo.clear();
        let (cursor, anchor) = (self.buffer.cursor(), self.buffer.anchor());
        self.undo.push(Unit {
            ops: Vec::new(),
            kind: Kind::Other,
            cursor_before: cursor,
            anchor_before: anchor,
            cursor_after: cursor,
            anchor_after: anchor,
            secondary_before: Vec::new(),
            secondary_after: Vec::new(),
            style: Some(StyleDelta { lo, hi, before, after }),
        });
    }

    /// `MDA`/`MDA/B` colour on a selection, as an UNDOABLE step. `rgb == None` clears the layer.
    pub fn style_selection_color(&mut self, lo: usize, hi: usize, background: bool, rgb: Option<(u8, u8, u8)>) {
        let before = self.capture_spans(lo, hi);
        if background {
            self.set_bg_color_span(lo, hi, rgb);
        } else {
            self.set_color_span(lo, hi, rgb);
        }
        self.record_style_change(lo, hi, before);
    }

    /// `MDA <attr>` on a selection, as an UNDOABLE step. `attrs == None` (or `NM`) clears attributes.
    pub fn style_selection_attr(&mut self, lo: usize, hi: usize, attrs: Option<u8>) {
        let before = self.capture_spans(lo, hi);
        self.set_attr_span(lo, hi, attrs);
        self.record_style_change(lo, hi, before);
    }

    /// `MDALL` colour over many match `ranges`, as ONE undoable step (the whole-document styling is
    /// snapshotted before/after — matches are scattered, so the region is the whole buffer).
    /// `also_clear_attr` mirrors bare `MDALL off` clearing attributes alongside the foreground.
    pub fn style_all_color(
        &mut self,
        ranges: &[(usize, usize)],
        background: bool,
        rgb: Option<(u8, u8, u8)>,
        also_clear_attr: bool,
    ) {
        let (lo, hi) = (0, self.buffer.len());
        let before = self.capture_spans(lo, hi);
        if background {
            self.set_bg_color_spans_bulk(ranges, rgb);
        } else {
            self.set_color_spans_bulk(ranges, rgb);
            if also_clear_attr {
                self.set_attr_spans_bulk(ranges, None);
            }
        }
        self.record_style_change(lo, hi, before);
    }

    /// `MDALL <attr>` over many match `ranges`, as ONE undoable step.
    pub fn style_all_attr(&mut self, ranges: &[(usize, usize)], attrs: Option<u8>) {
        let (lo, hi) = (0, self.buffer.len());
        let before = self.capture_spans(lo, hi);
        self.set_attr_spans_bulk(ranges, attrs);
        self.record_style_change(lo, hi, before);
    }

    /// Edit-follow the bookmark points (live anchors), so a mark stays on its text as you type/delete.
    /// Left gravity at the edit point: a mark exactly at an insertion stays put (the new text lands
    /// after it); a mark the edit DELETES over collapses to the edit start. Mirrors [`shift_spans`],
    /// but a point is never dropped (a bookmark has no width to be emptied).
    fn shift_marks(list: &mut [Bookmark], at: usize, removed: usize, inserted: usize) {
        for m in list.iter_mut() {
            let x = m.off;
            m.off = if x <= at {
                x
            } else if x >= at + removed {
                x - removed + inserted
            } else {
                at // inside a deletion → collapse to the edit start
            };
        }
    }

    /// Edit-follow one span list. Per-edge "gravity" so text typed AT a boundary isn't swept into the
    /// colour: START has right gravity (an insert at the start excludes the new text), END has left
    /// gravity (an insert at the end stays put). A point inside a deletion collapses to `at`; an
    /// insert strictly INSIDE the span grows it.
    fn shift_spans<P>(list: &mut Vec<(usize, usize, P)>, at: usize, removed: usize, inserted: usize) {
        if list.is_empty() {
            return;
        }
        let shift = |x: usize, left_gravity: bool| -> usize {
            let before = if left_gravity { x <= at } else { x < at };
            if before {
                x
            } else if x >= at + removed {
                x - removed + inserted
            } else {
                at
            }
        };
        list.retain_mut(|s| {
            s.0 = shift(s.0, false); // start: right gravity
            s.1 = shift(s.1, true); // end: left gravity
            s.0 < s.1
        });
    }

    /// Slide the live language-check underlines along with an edit, so they stay on the words they
    /// actually mark until the background re-check lands.
    ///
    /// Without this the marks keep the byte offsets they were computed at, so EVERY keystroke draws
    /// every underline after the caret one byte out of place — the reported symptom was the last
    /// letter of each flagged word flickering while typing (and the first letter while backspacing),
    /// a screen-wide flash on every key. `poll_lint` correctly refuses to apply a result computed
    /// against a superseded revision, which is precisely why the OLD marks have to be kept honest in
    /// the meantime.
    ///
    /// Marks wholly before the edit are untouched; marks wholly after it shift by the edit's delta;
    /// a mark the edit lands INSIDE is dropped — that word is being retyped, so its squiggle is
    /// meaningless until the checker reports on the new text.
    fn adjust_lints(&mut self, at: usize, removed: usize, inserted: usize) {
        if self.lints.is_empty() {
            return;
        }
        let end_of_edit = at + removed;
        // A word char typed AGAINST a flagged word EXTENDS it, but the mark (end has left gravity)
        // wouldn't grow — so it would keep underlining just the old prefix ("recei" of "receipt") until
        // the async re-check lands (slow on big files, hence the partial-underline-while-typing report).
        // Detect that and DROP the mark instead: `head_word` = the first inserted byte is a word char
        // (an append at a mark's END); `tail_word` = the last inserted byte is one (a prepend at a
        // mark's START). A boundary edit with a NON-word char (space/punctuation) leaves the word intact,
        // so its squiggle stays — no flicker when you finish a misspelling and press space.
        let head_word = inserted > 0 && self.buffer.is_word_byte(at);
        let tail_word = inserted > 0 && self.buffer.is_word_byte(at + inserted - 1);
        self.lints.retain_mut(|m| {
            if m.end <= at {
                !(m.end == at && head_word) // append of a word char at its end → extended → drop
            } else if m.start >= end_of_edit {
                if m.start == end_of_edit && tail_word {
                    return false; // prepend of a word char at its start → extended → drop
                }
                m.start = m.start - removed + inserted; // entirely after — slide along
                m.end = m.end - removed + inserted;
                true
            } else {
                false // the edit touches this word — drop it until the re-check
            }
        });
    }

    /// The transient colour spans converted to DECODED-byte offsets (what the renderer/tokens use).
    /// Identity for a UTF-8 document; the conversion matters only for non-UTF-8 encodings.
    pub fn color_spans_decoded(&self) -> Vec<(usize, usize, (u8, u8, u8))> {
        self.color_spans
            .iter()
            .map(|&(s, e, c)| (self.buffer.decoded_offset(s), self.buffer.decoded_offset(e), c))
            .collect()
    }

    /// The BACKGROUND colour spans in DECODED-byte offsets (what the renderer's rectangle pass uses).
    pub fn bg_color_spans_decoded(&self) -> Vec<(usize, usize, (u8, u8, u8))> {
        self.bg_spans
            .iter()
            .map(|&(s, e, c)| (self.buffer.decoded_offset(s), self.buffer.decoded_offset(e), c))
            .collect()
    }

    /// The text-attribute spans in DECODED-byte offsets (what the renderer reads for glyph weight/
    /// slant and the drawn underline).
    pub fn attr_spans_decoded(&self) -> Vec<(usize, usize, u8)> {
        self.attr_spans
            .iter()
            .map(|&(s, e, a)| (self.buffer.decoded_offset(s), self.buffer.decoded_offset(e), a))
            .collect()
    }

    /// The raw ROPE-offset colour spans (FG, BG) — for sidecar persistence, which needs the offsets
    /// into the saved text, not the decoded-render offsets.
    pub fn color_spans_raw(&self) -> (&[(usize, usize, (u8, u8, u8))], &[(usize, usize, (u8, u8, u8))]) {
        (&self.color_spans, &self.bg_spans)
    }

    /// The raw ROPE-offset text-attribute spans — for sidecar persistence (the `[attrs]` section).
    pub fn attr_spans_raw(&self) -> &[(usize, usize, u8)] {
        &self.attr_spans
    }

    /// Install `MDA` foreground/background/attribute spans wholesale (raw rope offsets), replacing any
    /// current ones — used by the host to restore a document's block styling from its `.xymeta` sidecar
    /// right after [`load`](Self::load). Sorted by start; the renderer and edit-tracking expect order.
    pub fn set_color_spans_raw(
        &mut self,
        fg: Vec<(usize, usize, (u8, u8, u8))>,
        bg: Vec<(usize, usize, (u8, u8, u8))>,
        attrs: Vec<(usize, usize, u8)>,
    ) {
        self.color_spans = fg;
        self.bg_spans = bg;
        self.attr_spans = attrs;
        self.color_spans.sort_by_key(|s| s.0);
        self.bg_spans.sort_by_key(|s| s.0);
        self.attr_spans.sort_by_key(|s| s.0);
        self.gen = self.gen.wrapping_add(1);
        // This installs the persisted (sidecar/session) styling, which IS the saved baseline — so a
        // freshly-opened styled doc reports clean. (On a clean open, `set_marks_restored` runs right
        // after and re-baselines to include the marks too.)
        self.mark_styling_baseline();
    }

    // ---- bookmarks (`MARK*`) -----------------------------------------------

    /// Capture the raw-byte context window straddling `off`, returning `(ctx, split)` where `split`
    /// is how many bytes precede the point. Encoding-agnostic (works on the raw rope bytes).
    fn capture_ctx(&self, off: usize) -> (Vec<u8>, usize) {
        let len = self.buffer.len();
        let off = off.min(len);
        let start = off.saturating_sub(MARK_CTX);
        let end = (off + MARK_CTX).min(len);
        (self.buffer.slice_bytes(start, end), off - start)
    }

    /// The document's bookmarks (sorted by offset), for the gutter glyph and tests.
    pub fn marks(&self) -> &[Bookmark] {
        &self.marks
    }

    /// Number of bookmarks (`$MARKS`).
    pub fn mark_count(&self) -> usize {
        self.marks.len()
    }

    /// Whether the caret sits exactly on a bookmark (`$MARK`).
    pub fn mark_at_cursor(&self) -> bool {
        let off = self.buffer.cursor();
        self.marks.iter().any(|m| m.off == off)
    }

    /// `MARKSET`: set a bookmark at the caret. Returns `false` (no change) if one is already there.
    pub fn mark_set(&mut self) -> bool {
        let off = self.buffer.cursor();
        if self.marks.iter().any(|m| m.off == off) {
            return false;
        }
        let (ctx, split) = self.capture_ctx(off);
        self.marks.push(Bookmark { off, ctx, split, moved: false });
        self.marks.sort_by_key(|m| m.off);
        self.styling_dirty = true; // metadata change → persist on save, count for the save prompt
        true
    }

    /// `MARKCLEAR`: remove the bookmark at the caret. Returns `false` if there was none.
    pub fn mark_clear(&mut self) -> bool {
        let off = self.buffer.cursor();
        let before = self.marks.len();
        self.marks.retain(|m| m.off != off);
        let removed = self.marks.len() != before;
        if removed {
            self.styling_dirty = true;
        }
        removed
    }

    /// `MARKTOGGLE`: clear the bookmark at the caret if present, else set one. Returns the new state
    /// (`true` = a bookmark is now set here).
    pub fn mark_toggle(&mut self) -> bool {
        if self.mark_clear() {
            false
        } else {
            self.mark_set();
            true
        }
    }

    /// `MARKCLEARALL`: drop every bookmark. Returns how many were cleared.
    pub fn mark_clear_all(&mut self) -> usize {
        let n = self.marks.len();
        if n > 0 {
            self.marks.clear();
            self.styling_dirty = true;
        }
        n
    }

    /// `MARKNEXT`: move the caret to the next bookmark after it (wrapping). `None` if there are none.
    pub fn mark_next(&mut self) -> Option<MarkNav> {
        self.mark_jump(true)
    }

    /// `MARKPREV`: move the caret to the previous bookmark before it (wrapping). `None` if none.
    pub fn mark_prev(&mut self) -> Option<MarkNav> {
        self.mark_jump(false)
    }

    fn mark_jump(&mut self, forward: bool) -> Option<MarkNav> {
        if self.marks.is_empty() {
            return None;
        }
        let cur = self.buffer.cursor();
        // Live anchors keep offsets correct in-session, so no reconcile is needed here — the `moved`
        // flags were set at load time. Pick the target by offset, circularly.
        let target = if forward {
            self.marks.iter().position(|m| m.off > cur).or(Some(0))
        } else {
            self.marks.iter().rposition(|m| m.off < cur).or(Some(self.marks.len() - 1))
        }?;
        let bm = &mut self.marks[target];
        let off = bm.off;
        let moved = std::mem::take(&mut bm.moved); // report once, then clear
        self.set_caret(off, None);
        Some(MarkNav { off, moved, index: target + 1, total: self.marks.len() })
    }

    /// Restore bookmarks read from the sidecar, then reconcile them against the CURRENT text: a mark
    /// whose stored context no longer sits at its offset is relocated by content (within its vicinity),
    /// flagging `moved`. Marks that can't be placed keep their clamped offset. De-duplicated by offset.
    pub fn set_marks_restored(&mut self, marks: Vec<Bookmark>) {
        self.marks = marks;
        self.reconcile_marks();
        // Restored marks match what was last SAVED, so fold them into the styling baseline (they are
        // persisted in the same sidecar) — this is not an unsaved change.
        self.mark_styling_baseline();
    }

    /// Verify each mark still sits on its captured context; relocate the ones that don't (see
    /// [`find_ctx_near`]). Clamps offsets, sorts, de-duplicates. Returns how many were relocated.
    fn reconcile_marks(&mut self) -> usize {
        if self.marks.is_empty() {
            return 0;
        }
        let len = self.buffer.len();
        let text = self.buffer.slice_bytes(0, len);
        self.marks.sort_by_key(|m| m.off);
        let bounds: Vec<usize> = self.marks.iter().map(|m| m.off.min(len)).collect();
        let mut moved = 0;
        for i in 0..self.marks.len() {
            let (off, split, ctx_len) = {
                let m = &self.marks[i];
                (m.off, m.split, m.ctx.len())
            };
            if ctx_len == 0 {
                continue;
            }
            let start = off.saturating_sub(split);
            let ok = start + ctx_len <= len && text[start..start + ctx_len] == self.marks[i].ctx[..];
            if ok {
                continue;
            }
            let lo = if i > 0 { bounds[i - 1] } else { 0 };
            let hi = if i + 1 < bounds.len() { bounds[i + 1] } else { len };
            if let Some(new_off) = find_ctx_near(&text, &self.marks[i].ctx, split, lo, hi, off) {
                self.marks[i].off = new_off;
                self.marks[i].moved = true;
                moved += 1;
            }
        }
        for m in &mut self.marks {
            m.off = m.off.min(len);
        }
        self.marks.sort_by_key(|m| m.off);
        self.marks.dedup_by_key(|m| m.off);
        moved
    }

    /// `(offset, preview)` for each bookmark, for the `MARKLIST` chooser. The preview is the 1-based
    /// line number plus the trimmed line text (elided when long), e.g. `L42  the marked line`.
    pub fn mark_previews(&self) -> Vec<(usize, String)> {
        self.marks
            .iter()
            .map(|m| {
                let line = self.buffer.line_of(m.off);
                let text = self.buffer.line_text(line);
                let trimmed = text.trim();
                let snippet: String = trimmed.chars().take(60).collect();
                let snippet = if trimmed.chars().count() > 60 {
                    format!("{snippet}…")
                } else if snippet.is_empty() {
                    "(blank line)".to_string()
                } else {
                    snippet
                };
                (m.off, format!("L{}  {snippet}", line + 1))
            })
            .collect()
    }

    /// Bookmarks with FRESH context captured from the current buffer, for persisting to the sidecar
    /// (`(off, split, ctx)`). Recomputed at save time so the stored anchor matches the saved bytes,
    /// even after a session of edits shifted the live offsets.
    pub fn marks_for_persist(&self) -> Vec<(usize, usize, Vec<u8>)> {
        self.marks
            .iter()
            .map(|m| {
                let (ctx, split) = self.capture_ctx(m.off);
                (m.off, split, ctx)
            })
            .collect()
    }

    /// Replace the document's content from freshly-read file **bytes**, adopting
    /// `path`. The bytes are stored **verbatim** (never transformed) — the
    /// byte-exact-round-trip guarantee. The encoding view is chosen by a
    /// provisional default ([`default_encoding_for`]); the line-ending style is
    /// detected for information / future use; the cursor moves to the top; history
    /// is cleared and the document is marked unmodified.
    pub fn load(&mut self, bytes: Vec<u8>, path: PathBuf) {
        let crlf = bytes.windows(2).any(|w| w == b"\r\n");
        let enc = default_encoding_for(&bytes);
        self.gen = next_content_gen(); // whole-buffer swap resets the rope rev
        self.buffer = Buffer::from_bytes(bytes, enc);
        self.buffer.set_caret(0, None); // top of file on open
        // A fresh file starts with no block colours; any belong to the OLD content. The host restores
        // them from the `.xymeta` sidecar right after (when the guard matches the new bytes).
        self.color_spans.clear();
        self.bg_spans.clear();
        self.attr_spans.clear();
        self.marks.clear(); // old content's bookmarks; restored from the sidecar right after
        // A freshly-loaded doc matches disk; the host restores sidecar styling right after, which
        // re-baselines. (This empty baseline holds for a file with no sidecar.)
        self.mark_styling_baseline();
        self.undo.clear();
        self.redo.clear();
        self.coalescing = false;
        self.saved_len = Some(0);
        self.path = Some(path);
        self.line_ending = if crlf { LineEnding::Crlf } else { LineEnding::Lf };
        // Preserve the on/off preference across opens, re-selecting by file type (a grammar
        // for the new extension, else Markdown).
        if self.highlighter.is_some() {
            self.highlighter = self.resolve_highlighter();
        }
        self.refresh_highlights();
    }

    /// The raw bytes to write on save: the document's bytes, **verbatim**. Edits
    /// already produced bytes through the current encoding, and untouched content
    /// is the original bytes, so save is a straight write — nothing to re-encode,
    /// nothing to corrupt. (Materialized from the rope, so it's an owned snapshot.)
    pub fn save_bytes(&self) -> Vec<u8> {
        self.buffer.bytes()
    }

    /// Adopt a path (for save-as).
    pub fn set_path(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    /// Mark the current state as saved (clears the modified flag). Also closes
    /// any open coalescing run so post-save edits start a new undo unit (which is
    /// what keeps the modified flag correct under coalescing).
    pub fn mark_saved(&mut self) {
        self.saved_len = Some(self.undo.len());
        self.coalescing = false;
        self.mark_styling_baseline(); // styling was just persisted to the sidecar
    }

    // ---- recorded mutations ------------------------------------------------

    /// Insert text at the cursor, replacing any selection.
    pub fn insert(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if self.buffer.is_multi() {
            self.insert_multi_carets(s);
            return;
        }
        self.buffer.commit_define(); // resolve an open DF selection to standard form
        self.buffer.snap_caret(); // never insert INSIDE a `\r\n` (would split it → stray bytes)
        // An edit *replaces* the selection only when it's a coupled selection and PS
        // is off (the caret sits on the block). Under PS the block is kept + ridden
        // along; for a *decoupled* (roamed) block the buffer clears it and edits at
        // the caret — both are plain inserts for the undo record.
        let keep = self.buffer.is_persistent() && self.buffer.has_selection();
        let replace = !keep && self.buffer.has_selection() && self.buffer.sel_head().is_none();
        let (cb, ab) = (self.buffer.cursor(), self.buffer.anchor());
        let (at, removed, kind) = if replace {
            let (a, b) = self.buffer.selection_span().unwrap();
            (a, self.buffer.slice_bytes(a, b), Kind::Other)
        } else {
            (cb, Vec::new(), Kind::Insert)
        };
        self.buffer.insert(s);
        let (ca, aa) = (self.buffer.cursor(), self.buffer.anchor());
        // The bytes actually inserted are everything from `at` to the new cursor
        // (the view encoded `s`; an unrepresentable char became `?`).
        let inserted = self.buffer.slice_bytes(at, ca);
        self.record(Op { at, removed, inserted, ..Default::default() }, kind, cb, ab, ca, aa);
    }

    /// Insert a newline in this document's line-ending style (`\n` or `\r\n`) — what the
    /// `ENTER` verb does, so typed newlines follow the file (and the OS for new files).
    pub fn insert_newline(&mut self) {
        self.insert(self.line_ending.as_str());
    }

    /// `ENTER` with `TABINDENT` on: insert a newline, then copy the (about-to-be-)previous line's
    /// leading indentation so the new line starts at the same column. One undo unit.
    pub fn insert_newline_autoindent(&mut self) {
        // Where the break lands: the selection start (Enter replaces a selection) or the caret.
        let point = self.buffer.selection_span().map(|(a, _)| a).unwrap_or_else(|| self.buffer.cursor());
        let indent = self.buffer.indent_at(point);
        self.insert(&format!("{}{}", self.line_ending.as_str(), indent));
    }

    /// Delete the char before the cursor (Backspace), or the selection. Under
    /// persistent selection it's always a char-delete (the block is kept + rides along).
    pub fn delete_back(&mut self) {
        if self.buffer.is_multi() {
            self.delete_multi_carets(false);
            return;
        }
        self.buffer.commit_define();
        let keep = self.buffer.is_persistent() && self.buffer.has_selection();
        let replace = !keep && self.buffer.has_selection() && self.buffer.sel_head().is_none();
        let (cb, ab) = (self.buffer.cursor(), self.buffer.anchor());
        if replace {
            let (a, b) = self.buffer.selection_span().unwrap();
            let removed = self.buffer.slice_bytes(a, b);
            self.buffer.delete_back();
            let (ca, aa) = (self.buffer.cursor(), self.buffer.anchor());
            self.record(Op { at: a, removed, inserted: Vec::new(), ..Default::default() }, Kind::Other, cb, ab, ca, aa);
            return;
        }
        if cb > 0 {
            let removed = self.buffer.slice_bytes(cb - 1, cb);
            self.buffer.delete_back();
            let (ca, aa) = (self.buffer.cursor(), self.buffer.anchor());
            self.record(
                Op { at: cb - 1, removed, inserted: Vec::new(), ..Default::default() },
                Kind::DeleteBack,
                cb,
                ab,
                ca,
                aa,
            );
        }
    }

    /// Delete the char at the cursor (Delete/Del), or the selection. Under persistent
    /// selection it's always a char-delete (the block is kept + rides along).
    pub fn delete_forward(&mut self) {
        if self.buffer.is_multi() {
            self.delete_multi_carets(true);
            return;
        }
        self.buffer.commit_define();
        let keep = self.buffer.is_persistent() && self.buffer.has_selection();
        let replace = !keep && self.buffer.has_selection() && self.buffer.sel_head().is_none();
        let (cb, ab) = (self.buffer.cursor(), self.buffer.anchor());
        if replace {
            let (a, b) = self.buffer.selection_span().unwrap();
            let removed = self.buffer.slice_bytes(a, b);
            self.buffer.delete_forward();
            let (ca, aa) = (self.buffer.cursor(), self.buffer.anchor());
            self.record(Op { at: a, removed, inserted: Vec::new(), ..Default::default() }, Kind::Other, cb, ab, ca, aa);
            return;
        }
        if cb < self.buffer.len() {
            let removed = self.buffer.slice_bytes(cb, cb + 1);
            self.buffer.delete_forward();
            let (ca, aa) = (self.buffer.cursor(), self.buffer.anchor());
            self.record(
                Op { at: cb, removed, inserted: Vec::new(), ..Default::default() },
                Kind::DeleteForward,
                cb,
                ab,
                ca,
                aa,
            );
        }
    }

    /// Change the case of the selection, or — when there is none — the character
    /// under the cursor (`LC`/`UC`/`CC`). A recorded edit; the (possibly
    /// different-length) transformed text stays selected. No-op when the text is
    /// already in that case (no undo unit for a no-op).
    pub fn change_case(&mut self, kind: CaseKind) {
        self.buffer.commit_define();
        let (a, b, had_sel) = match self.buffer.selection_span() {
            Some((a, b)) if a != b => (a, b, true),
            _ => {
                let (s, e) = self.buffer.char_span_at(self.buffer.cursor());
                (s, e, false)
            }
        };
        if a == b {
            return; // nothing under the cursor (end of buffer)
        }
        let src = self.buffer.slice(a, b);
        let dst = match kind {
            CaseKind::Lower => src.to_lowercase(),
            CaseKind::Upper => src.to_uppercase(),
            CaseKind::Swap => swap_case(&src),
        };
        if dst == src {
            // No change (digit/punctuation/already-cased). With no selection, still advance the
            // caret one char to the right — so repeated UC/LC/CC sweeps along the text instead of
            // stalling on an unchanged character. A pure caret move, not a recorded edit.
            if !had_sel {
                self.buffer.set_caret(b, None);
            }
            return;
        }
        let removed = self.buffer.slice_bytes(a, b);
        let inserted = self.buffer.encode(&dst);
        let (cb, ab) = (self.buffer.cursor(), self.buffer.anchor());
        self.buffer.replace_range_bytes(a, removed.len(), &inserted);
        if had_sel {
            // Keep the transformed text selected (it may differ in byte length).
            self.buffer.set_caret(a + inserted.len(), Some(a));
        } else {
            // No selection: advance past the just-changed char (one char to the right) so the
            // gesture sweeps along the text, XyWrite-style.
            self.buffer.set_caret(a + inserted.len(), None);
        }
        let (ca, aa) = (self.buffer.cursor(), self.buffer.anchor());
        self.record(Op { at: a, removed, inserted, ..Default::default() }, Kind::Other, cb, ab, ca, aa);
    }

    /// `CP` — copy the selected text to the cursor position. Inserts a copy at the
    /// caret and leaves the original selection in place (it rides along). Returns
    /// `false` when there is no selection. A recorded edit.
    pub fn copy_selection_to_cursor(&mut self) -> bool {
        self.buffer.commit_define();
        let Some((a, b)) = self.buffer.selection_span() else {
            return false;
        };
        if a == b {
            return false;
        }
        let s = self.buffer.slice(a, b);
        // The copied block's MDA styling (rebased to the block start) rides the copy to the drop point.
        let moved_spans = self.capture_spans(a, b - a);
        let at = self.buffer.cursor();
        // Undo must restore the ORIGINAL SELECTION, not (drop point, block anchor) — a DEFINED block
        // persists while the caret roams to the drop point. See `move_selection_to_cursor`.
        let (cb, ab) = selection_restore_state(&self.buffer, a, b);
        let n = self.buffer.insert_keeping_selection(&s);
        let inserted = self.buffer.slice_bytes(at, at + n);
        let (ca, aa) = (self.buffer.cursor(), self.buffer.anchor());
        self.record(
            Op { at, removed: Vec::new(), inserted, inserted_spans: moved_spans, ..Default::default() },
            Kind::Other,
            cb,
            ab,
            ca,
            aa,
        );
        true
    }

    /// Snapshot the MDA styling over the current selection for the clipboard (`CUT`/`COPY`), rebased to
    /// the selection start. `None` when there's no selection or it carries no styling (so an ordinary
    /// unstyled copy clears any stale clip). Call it BEFORE a cut's delete.
    pub fn selection_style_clip(&self) -> Option<StyleClip> {
        let (a, b) = self.buffer.selection_span()?;
        if a >= b {
            return None;
        }
        let snap = self.capture_spans(a, b - a);
        (!snap.is_empty()).then_some(StyleClip { snap, byte_len: b - a })
    }

    /// `PASTE` carrying clipboard styling: like [`insert`](Self::insert) but, when `clip` lines up with
    /// the text actually inserted (same byte length — the destination encoding produced the same bytes),
    /// the pasted text keeps the copied MDA styling. A mismatch (different encoding) or an empty/absent
    /// clip pastes plain. Single-caret only; the multi-caret distribute path stays plain for now.
    pub fn insert_styled(&mut self, s: &str, clip: Option<&StyleClip>) {
        let clip = match clip {
            Some(c) if !c.is_empty() && !self.buffer.is_multi() => c,
            _ => return self.insert(s),
        };
        if s.is_empty() {
            return;
        }
        self.buffer.commit_define();
        self.buffer.snap_caret();
        let keep = self.buffer.is_persistent() && self.buffer.has_selection();
        let replace = !keep && self.buffer.has_selection() && self.buffer.sel_head().is_none();
        let (cb, ab) = (self.buffer.cursor(), self.buffer.anchor());
        let (at, removed) = if replace {
            let (a, b) = self.buffer.selection_span().unwrap();
            (a, self.buffer.slice_bytes(a, b))
        } else {
            (cb, Vec::new())
        };
        self.buffer.insert(s);
        let (ca, aa) = (self.buffer.cursor(), self.buffer.anchor());
        let inserted = self.buffer.slice_bytes(at, ca);
        // Apply styling only when the re-encoded paste has the same byte length as the source — else the
        // rebased span offsets wouldn't line up (a cross-encoding paste), so paste plain.
        let inserted_spans =
            if inserted.len() == clip.byte_len { clip.snap.clone() } else { SpanSnapshot::default() };
        // A styled paste is always its own undo unit (`Kind::Other`) — never coalesced.
        self.record(Op { at, removed, inserted, inserted_spans, ..Default::default() }, Kind::Other, cb, ab, ca, aa);
    }

    /// `MV` — move the selected text to the cursor position (delete from origin,
    /// insert at the caret). Needs the caret *outside* the selection (it's a no-op
    /// when the caret is within it — e.g. the coupled case where the caret sits on
    /// an edge). The moved text ends up selected. Returns `false` when there is no
    /// selection. Two recorded edits (delete + insert) — undo takes two steps.
    pub fn move_selection_to_cursor(&mut self) -> bool {
        self.buffer.commit_define();
        let Some((a, b)) = self.buffer.selection_span() else {
            return false;
        };
        if a == b {
            return false;
        }
        let c = self.buffer.cursor();
        if a <= c && c <= b {
            return true; // caret within the selection: nothing to move (no-op)
        }
        let removed = self.buffer.slice_bytes(a, b);
        let s = self.buffer.slice(a, b);
        // Capture the moved block's MDA styling BEFORE the delete tears it out, rebased to the block
        // start, so it can ride the text to the drop point (and the delete op's auto-captured
        // `removed_spans` restores it at the origin on undo).
        let moved_spans = self.capture_spans(a, b - a);
        let dest = if c > b { c - (b - a) } else { c };

        // Unit 1: delete the selection from its origin, caret to the destination.
        // Undo must restore the ORIGINAL SELECTION (a,b) — NOT (roamed caret, block anchor). A DEFINED
        // block persists while the caret roams to the drop point, so `cursor()`/`anchor()` here are the
        // drop point and the block's anchor; restoring those selects everything from the block start to
        // the drop point. Record the selection's own span instead, preserving its orientation.
        let (cb1, ab1) = selection_restore_state(&self.buffer, a, b);
        self.buffer.replace_range_bytes(a, b - a, &[]);
        self.buffer.set_caret(dest, None);
        let (ca1, aa1) = (self.buffer.cursor(), self.buffer.anchor());
        self.record(
            Op { at: a, removed, inserted: Vec::new(), ..Default::default() },
            Kind::Other,
            cb1,
            ab1,
            ca1,
            aa1,
        );

        // Unit 2: insert the text at the destination and select it.
        let ins = self.buffer.encode(&s);
        let len = ins.len();
        let (cb2, ab2) = (self.buffer.cursor(), self.buffer.anchor());
        self.buffer.replace_range_bytes(dest, 0, &ins);
        self.buffer.set_caret(dest + len, Some(dest));
        let (ca2, aa2) = (self.buffer.cursor(), self.buffer.anchor());
        self.record(
            Op { at: dest, removed: Vec::new(), inserted: ins, inserted_spans: moved_spans, ..Default::default() },
            Kind::Other,
            cb2,
            ab2,
            ca2,
            aa2,
        );
        true
    }

    /// `SE` — find `pattern` and select the match. Forward searches start at the
    /// end of any current selection (so repeated `SE` walks matches); backward
    /// starts at its beginning; `from_top` overrides to the document edge. Wraps
    /// around once. Returns `false` (caller raises `$ERROR`) when not found. Pure
    /// navigation — no edit, no undo unit.
    pub fn search_select(
        &mut self,
        pattern: &Pattern,
        forward: bool,
        from_top: bool,
        scope: Option<(usize, usize)>,
        extend: bool,
    ) -> bool {
        // A plain `SE` while a `DF` define is OPEN grows the block from the define's anchor (the
        // classic `SE /a/; DF; SE /b/; DZ` gesture), rather than replacing the selection with just
        // the match. `/X` requests the same explicitly. Only commit (freeze) the define when we're
        // NOT extending — otherwise `commit_define` would cancel the empty span `DF` just created.
        let extend = extend || self.buffer.is_defining();
        if !extend {
            self.buffer.commit_define();
        }
        let len = self.buffer.len();

        // `/S`: search WITHIN the selection `[s, e]`, which stays put as a scope while the caret
        // roams over matches inside it. Start point (per the documented rule):
        //   • caret strictly inside the span → start at the caret (so repeated SE/S walks through
        //     matches: each lands the caret at the match, the next continues from there);
        //   • caret outside (before/after the span) or on the *near* edge → start at the top of the
        //     span (forward) / the bottom (backward), i.e. search the whole selection.
        // The search is bounded to the span and does not wrap. To keep that no-wrap guarantee when a
        // match ends exactly on the FAR edge, we distinguish "walked to the edge" from "freshly
        // positioned there": a `/S` match leaves the caret roamed (`sel_head` set), so a caret on the
        // far edge *with* `sel_head` means the walk is exhausted → stop. A fresh caret on that edge
        // (e.g. an ordinary left-to-right selection, `sel_head` unset) still searches the whole span.
        if let Some((s, e)) = scope {
            let c = self.buffer.cursor();
            let roamed = self.buffer.sel_head().is_some();
            let inside = c > s && c < e;
            let start = if inside {
                c
            } else if forward {
                if c == e && roamed {
                    return false; // walked off the end of the scope
                }
                s
            } else {
                if c == s && roamed {
                    return false; // walked off the top of the scope
                }
                e
            };
            return match self.buffer.search(pattern, start, forward) {
                // Only accept a match that lies fully inside the scope.
                Some((ms, me)) if ms >= s && me <= e => {
                    // Move only the caret to the match (leading edge of travel), keeping the
                    // selection fixed as the scope so the next SE/S continues within it.
                    self.buffer.roam_to(if forward { me } else { ms });
                    true
                }
                _ => false,
            };
        }

        let start = if from_top {
            if forward {
                0
            } else {
                len
            }
        } else if extend {
            // `/X`: keep growing from the existing anchor — always search from the active caret edge,
            // never the selection edge (which could be the anchor for a backward-built block).
            self.buffer.cursor()
        } else if let Some((s, e)) = self.buffer.selection_span() {
            if forward {
                e
            } else {
                s
            }
        } else {
            self.buffer.cursor()
        };
        // XyWrite semantics: search does NOT wrap. SE stops at the last occurrence,
        // SEB at the first; when none remain in the travel direction we return false,
        // and the caller raises a trappable error ($ERROR) so loop-until-not-found
        // scripts terminate instead of cycling forever.
        let found = self.buffer.search(pattern, start, forward);
        match found {
            Some((ms, me)) => {
                let edge = if forward { me } else { ms };
                if extend {
                    // `/X`: grow the current selection (e.g. a `DF` block) to the match's leading
                    // edge of travel, keeping the existing anchor — don't replace the selection.
                    self.buffer.extend_to(edge);
                } else if forward {
                    // Select just the match; caret on the leading edge of travel.
                    self.buffer.set_caret(me, Some(ms));
                } else {
                    self.buffer.set_caret(ms, Some(me));
                }
                true
            }
            None => false,
        }
    }

    /// `CH` — replace every match of `find` with `replace`, from the cursor to the
    /// end of the document (`from_top` restarts at the top = whole document).
    /// Recorded as a **single** undo unit spanning the affected region. Returns the
    /// number of replacements (wired to `$CC`); `0` lets the caller raise `$ERROR`.
    pub fn replace_all(
        &mut self,
        pattern: &Pattern,
        replace: &str,
        from_top: bool,
        scope: Option<(usize, usize)>,
        limit: Option<usize>,
        keep_case: bool,
    ) -> usize {
        self.buffer.commit_define();
        let text = self.buffer.text();

        // `/S`: replace only WITHIN the selection `[s, e]`. Start at the caret when it's inside the
        // span (caret → end of selection), otherwise at the top of the span; never past `e`. The
        // selection is re-established over the (now-modified) region afterward so it stays the scope.
        if let Some((s, e)) = scope {
            let c = self.buffer.cursor();
            let start = if c > s && c < e { c } else { s };
            let start_dec = self.buffer.decoded_offset(start);
            let e_dec = self.buffer.decoded_offset(e);
            // Replace within just the scope slice, then map the offsets back to whole-document space.
            let Some((fs, le, region, count)) =
                pattern.replace_all(&text[start_dec..e_dec], replace, 0, limit, keep_case)
            else {
                return 0;
            };
            let rfs = self.buffer.rope_offset(start_dec + fs);
            let rle = self.buffer.rope_offset(start_dec + le);
            let removed = self.buffer.slice_bytes(rfs, rle);
            let inserted = self.buffer.encode(&region);
            let (cb, ab) = (self.buffer.cursor(), self.buffer.anchor());
            self.buffer.replace_range_bytes(rfs, rle - rfs, &inserted);
            // The text from `rle` onward shifted by this many bytes, so the scope end moves too.
            let new_e = (e as isize + inserted.len() as isize - (rle - rfs) as isize) as usize;
            self.buffer.set_caret(new_e, Some(s)); // re-select the modified region as the scope
            let (ca, aa) = (self.buffer.cursor(), self.buffer.anchor());
            self.record(Op { at: rfs, removed, inserted, ..Default::default() }, Kind::Other, cb, ab, ca, aa);
            return count;
        }

        let start_dec =
            if from_top { 0 } else { self.buffer.decoded_offset(self.buffer.cursor()) };
        // The engine builds the transformed region (with regex capture expansion);
        // we splice it in as one recorded byte edit spanning [first, last) match.
        let Some((fs, le, region, count)) =
            pattern.replace_all(&text, replace, start_dec, limit, keep_case)
        else {
            return 0;
        };
        let rfs = self.buffer.rope_offset(fs);
        let rle = self.buffer.rope_offset(le);
        let removed = self.buffer.slice_bytes(rfs, rle);
        let inserted = self.buffer.encode(&region);
        let (cb, ab) = (self.buffer.cursor(), self.buffer.anchor());
        self.buffer.replace_range_bytes(rfs, rle - rfs, &inserted);
        // XyWrite-style: a change leaves the caret where it was rather than dragging it to the last
        // match. Adjust only for the net text shift — unchanged if the caret was before the edited
        // span, shifted by the length delta if after it, clamped to the span start if inside it.
        let delta = inserted.len() as isize - (rle - rfs) as isize;
        let restored = if cb <= rfs {
            cb
        } else if cb >= rle {
            (cb as isize + delta).max(0) as usize
        } else {
            rfs
        };
        self.buffer.set_caret(restored.min(self.buffer.len()), None);
        let (ca, aa) = (self.buffer.cursor(), self.buffer.anchor());
        self.record(Op { at: rfs, removed, inserted, ..Default::default() }, Kind::Other, cb, ab, ca, aa);
        count
    }

    /// Replace the byte range `[start, end)` (rope offsets) with `text`, as one
    /// recorded edit, and leave the caret just after the inserted text. Returns the
    /// rope offset of that caret (the new end). Used by interactive change-verify.
    pub fn replace_span(&mut self, start: usize, end: usize, text: &str) -> usize {
        self.buffer.commit_define();
        let removed = self.buffer.slice_bytes(start, end);
        let inserted = self.buffer.encode(text);
        let (cb, ab) = (self.buffer.cursor(), self.buffer.anchor());
        self.buffer.replace_range_bytes(start, end - start, &inserted);
        let new_end = start + inserted.len();
        self.buffer.set_caret(new_end, None);
        let (ca, aa) = (self.buffer.cursor(), self.buffer.anchor());
        self.record(Op { at: start, removed, inserted, ..Default::default() }, Kind::Other, cb, ab, ca, aa);
        new_end
    }

    /// Select the byte range `[start, end)` (rope offsets), caret on the end.
    pub fn select_range(&mut self, start: usize, end: usize) {
        self.buffer.set_caret(end, Some(start));
    }

    // ---- hex-view editing (raw bytes, byte-granular; see `hex.rs` + the HEX view) --------------
    // Overwrite is the default (keeps file size + offsets stable, which matters for binary formats);
    // insert/delete are available. Every edit here is byte-faithful (no re-encoding) and recorded for
    // undo. The caret is a byte offset, so these rest on any byte boundary — matching the grid.

    /// Core raw-byte recorded edit: at byte `at`, remove `remove` bytes and splice `insert` verbatim,
    /// then place the caret at `caret` (clamped). Records the inverse for undo. All the hex editors
    /// below funnel through here.
    fn hex_write(&mut self, at: usize, remove: usize, insert: &[u8], caret: usize) {
        self.buffer.commit_define();
        let removed = self.buffer.slice_bytes(at, at + remove);
        let (cb, ab) = (self.buffer.cursor(), self.buffer.anchor());
        self.buffer.replace_range_bytes(at, remove, insert);
        let new_caret = caret.min(self.buffer.len());
        self.buffer.set_caret(new_caret, None);
        let (ca, aa) = (self.buffer.cursor(), self.buffer.anchor());
        self.record(Op { at, removed, inserted: insert.to_vec(), ..Default::default() }, Kind::Other, cb, ab, ca, aa);
    }

    /// Set the hi or lo nibble of the byte at the caret to `val` (`0x0..=0xF`) — hex-column entry.
    /// Reads the current byte (0 when the caret is at end-of-file, where a fresh byte is created),
    /// replaces the chosen nibble, and records the edit. The caret stays put (the GUI advances after
    /// the low nibble completes the byte).
    pub fn hex_set_nibble(&mut self, hi: bool, val: u8) {
        let at = self.buffer.cursor();
        let cur = self.buffer.byte_at(at).unwrap_or(0);
        let byte = if hi { (val << 4) | (cur & 0x0F) } else { (cur & 0xF0) | (val & 0x0F) };
        let remove = usize::from(at < self.buffer.len());
        self.hex_write(at, remove, &[byte], at);
    }

    /// Overwrite the byte at the caret with `byte` (text-column entry) — creates a byte at end-of-file.
    /// Caret unchanged; the GUI advances.
    pub fn hex_overwrite(&mut self, byte: u8) {
        let at = self.buffer.cursor();
        let remove = usize::from(at < self.buffer.len());
        self.hex_write(at, remove, &[byte], at);
    }

    /// Insert `bytes` at the caret (byte insert). The caret stays at the insertion point.
    pub fn hex_insert(&mut self, bytes: &[u8]) {
        let at = self.buffer.cursor();
        self.hex_write(at, 0, bytes, at);
    }

    /// Delete one byte: the byte AT the caret (`before == false`) or the byte just before it
    /// (`before == true`, caret moves back one). No-op at the respective edge.
    pub fn hex_delete(&mut self, before: bool) {
        let at = self.buffer.cursor();
        if before {
            if at > 0 {
                self.hex_write(at - 1, 1, &[], at - 1);
            }
        } else if at < self.buffer.len() {
            self.hex_write(at, 1, &[], at);
        }
    }

    /// Paste `bytes` at the caret in OVERWRITE mode (replaces up to `bytes.len()` existing bytes, so
    /// file size stays stable unless it runs past end-of-file, which extends). Leaves the caret just
    /// after the pasted run.
    pub fn hex_paste(&mut self, bytes: &[u8]) {
        let at = self.buffer.cursor();
        let remove = bytes.len().min(self.buffer.len() - at);
        self.hex_write(at, remove, bytes, at + bytes.len());
    }

    /// Delete the selected byte range (hex cut); the caret lands at the range start. No-op without a
    /// selection. Returns the removed bytes (for the caller's clipboard).
    pub fn hex_delete_selection(&mut self) -> Vec<u8> {
        match self.buffer.selection_span() {
            Some((a, b)) if b > a => {
                let removed = self.buffer.slice_bytes(a, b);
                self.hex_write(a, b - a, &[], a);
                removed
            }
            _ => Vec::new(),
        }
    }

    /// `TYPEPAIRS` — handle a single interactively-typed `c` that is a bracket or quote. Returns
    /// `true` if the character was handled specially (so the caller must NOT also insert it):
    ///  - **wrap:** with a selection, typing an opener/quote wraps the selection (`(sel)`), keeping
    ///    the inner text selected;
    ///  - **over-type:** with the caret right before an identical closer/quote, step over it instead
    ///    of inserting a duplicate;
    ///  - **auto-close:** typing an opener (or a fresh quote) inserts the matching partner and leaves
    ///    the caret between the pair.
    /// Returns `false` for any other character (or a quote right after a word char — so apostrophes in
    /// prose aren't paired), leaving a plain insert to the caller. Each special case is one undo unit.
    pub fn type_pair(&mut self, c: char) -> bool {
        let closer = match c {
            '(' => Some(')'),
            '[' => Some(']'),
            '{' => Some('}'),
            _ => None,
        };
        let is_open = closer.is_some();
        let is_close = matches!(c, ')' | ']' | '}');
        let is_quote = matches!(c, '"' | '\'');
        if !(is_open || is_close || is_quote) {
            return false;
        }

        // Wrap a selection in an opener/quote (never a bare closer).
        if is_open || is_quote {
            if let Some((a, b)) = self.selection_range() {
                let close = if is_quote { c } else { closer.unwrap() };
                let inner = self.buffer.slice(a, b);
                let new_end = self.replace_span(a, b, &format!("{c}{inner}{close}"));
                // Openers/quotes are ASCII (one byte in every supported encoding); reselect the inner.
                self.select_range(a + 1, new_end - 1);
                return true;
            }
        }

        // Over-type: caret sits just before an identical closer/quote → skip over it.
        if (is_close || is_quote) && self.buffer.byte_at(self.buffer.cursor()) == Some(c as u8) {
            let c0 = self.buffer.cursor();
            self.buffer.set_caret(c0 + 1, None);
            return true;
        }
        if is_close {
            return false; // a plain closer with nothing to over-type → normal insert
        }

        // Opener or a fresh quote with no selection → auto-close. A quote right after a word char is
        // treated as an apostrophe (don't pair) so prose like `don't` isn't mangled.
        if is_quote {
            let before = self.buffer.cursor().checked_sub(1).and_then(|p| self.buffer.byte_at(p));
            if before.is_some_and(|b| b.is_ascii_alphanumeric()) {
                return false;
            }
        }
        let close = if is_quote { c } else { closer.unwrap() };
        self.insert(&format!("{c}{close}"));
        let after = self.buffer.cursor();
        self.buffer.set_caret(after - 1, None); // caret between the pair
        true
    }

    /// `TYPEPAIRS` — Backspace between an empty auto-pair (`(|)`, `[|]`, `{|}`, `"|"`, `'|'`) deletes
    /// BOTH members as one undo unit. Returns `true` if it did so; `false` leaves an ordinary
    /// backspace to the caller. Only fires with no selection and the caret exactly between a matching
    /// empty pair.
    pub fn try_delete_pair(&mut self) -> bool {
        if self.buffer.has_selection() {
            return false;
        }
        let c = self.buffer.cursor();
        let (Some(prev), Some(next)) = (
            c.checked_sub(1).and_then(|p| self.buffer.byte_at(p)),
            self.buffer.byte_at(c),
        ) else {
            return false;
        };
        let paired = matches!(
            (prev, next),
            (b'(', b')') | (b'[', b']') | (b'{', b'}') | (b'"', b'"') | (b'\'', b'\'')
        );
        if !paired {
            return false;
        }
        self.replace_span(c - 1, c + 1, "");
        true
    }

    /// `TAB in`/`out`: indent (`out=false`) or outdent (`out=true`) the line(s) the caret/selection
    /// covers. `indent` is the per-line indent unit (a tab, or `width` spaces for soft tabs); `width`
    /// bounds an outdent's leading-space removal. Recorded as one undo unit; a no-op (nothing to
    /// change) records nothing. The selection (if any) is kept over the affected lines.
    pub fn indent(&mut self, out: bool, indent: &str, width: usize) {
        self.buffer.commit_define();
        let Some((start, end, new_text, new_cursor, new_anchor)) =
            self.buffer.plan_indent(out, indent, width)
        else {
            return;
        };
        let removed = self.buffer.slice_bytes(start, end);
        let inserted = self.buffer.encode(&new_text);
        let (cb, ab) = (self.buffer.cursor(), self.buffer.anchor());
        self.buffer.replace_range_bytes(start, end - start, &inserted);
        self.buffer.set_caret(new_cursor, new_anchor);
        let (ca, aa) = (self.buffer.cursor(), self.buffer.anchor());
        self.record(Op { at: start, removed, inserted, ..Default::default() }, Kind::Other, cb, ab, ca, aa);
        self.refresh_highlights();
    }

    // ---- directory listing (DIR/LS, B10) -----------------------------------

    /// Turn this buffer into a `DIR`/`LS` listing of `base`/`entries`: the text becomes
    /// the rendered listing (line N → `entries[N]`), the caret goes to the top, and undo
    /// history is dropped (a listing isn't an editable, undoable document). Reuses an
    /// existing `Document` slot so `DIR` into a subdirectory replaces the listing in place.
    pub fn open_listing(
        &mut self,
        base: PathBuf,
        entries: Vec<crate::dir::DirEntry>,
        opts: DirOpts,
        filter: Option<String>,
    ) {
        // A filespec (`DIR *.txt`) narrows the entries here, so the stored `entries` are
        // exactly what's shown (line N → entries[N] still holds). The caller has already
        // applied the `/A` filter and `/O` sort.
        let entries = match &filter {
            Some(pat) => crate::dir::filter_entries(entries, pat),
            None => entries,
        };
        self.gen = next_content_gen(); // whole-buffer swap resets the rope rev
        self.buffer = Buffer::from_str(&crate::dir::render_listing(&entries, opts.bare));
        self.buffer.set_caret(0, None);
        self.undo.clear();
        self.redo.clear();
        self.coalescing = false;
        self.saved_len = Some(0);
        self.path = None;
        self.line_ending = LineEnding::Lf;
        self.highlighter = None;
        self.tokens.clear();
        // Title the tab `dir <filespec>` (a concise base name + any glob) instead of "untitled".
        let name = base
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| base.display().to_string()); // drive/root has no file_name
        self.label = Some(match &filter {
            Some(glob) => format!("dir {name}{}{glob}", std::path::MAIN_SEPARATOR),
            None => format!("dir {name}"),
        });
        self.dir = Some(DirState { base, entries, find: String::new(), opts, filter });
    }

    /// Whether this buffer is a directory listing (vs an editable document).
    pub fn is_dir_listing(&self) -> bool {
        self.dir.is_some()
    }

    // ---- GREP results listing --------------------------------------------------

    /// Turn this buffer into a `GREP` results listing: the text becomes `render_hits` (line N →
    /// `hits[N]`), the caret goes to the top, undo history is dropped, and it is marked read-only so
    /// the results can't be accidentally edited. `GJ` reads the hit under the caret.
    pub fn open_grep_listing(
        &mut self,
        base: PathBuf,
        hits: Vec<crate::grep::GrepHit>,
        pattern: String,
        regex: bool,
        perl: bool,
        capped: bool,
    ) {
        self.gen = next_content_gen(); // whole-buffer swap resets the rope rev
        self.buffer = Buffer::from_str(&crate::grep::render_hits(&hits, &base));
        self.buffer.set_caret(0, None);
        self.undo.clear();
        self.redo.clear();
        self.coalescing = false;
        self.saved_len = Some(0);
        self.path = None;
        self.line_ending = LineEnding::Lf;
        self.read_only = true;
        self.highlighter = None;
        self.tokens.clear();
        self.label = Some(format!("grep {pattern}"));
        self.grep = Some(crate::grep::GrepState { base, pattern, regex, perl, capped, hits });
    }

    /// Append a streamed batch of `GREP` hits: push them onto the state and append their rendered
    /// `path:line:col: text` lines to the buffer (raw, caret-preserving — a results listing isn't
    /// editable/undoable), keeping **line N → `hits[N]`**. No-op if this isn't a results listing.
    pub fn append_grep_hits(&mut self, hits: &[crate::grep::GrepHit], base: &std::path::Path) {
        if self.grep.is_none() || hits.is_empty() {
            return;
        }
        let text = crate::grep::render_hits(hits, base);
        self.append_log(&text); // end-append, keeps the caret put, sets read-only
        self.undo.clear(); // a listing isn't undoable — don't accumulate history as it streams
        self.redo.clear();
        self.mark_saved(); // re-sync the saved point AFTER clearing undo, else the tab reads modified
        self.grep.as_mut().unwrap().hits.extend_from_slice(hits);
    }

    /// Turn this buffer into a `COMPILE` results listing: empty, read-only, caret at the top, tracked
    /// as a hit listing (so `GJ`/`NERR` work) with the command as its label. The build's output lines
    /// stream in via [`push_output_line`](Self::push_output_line). Reuses the `GREP` listing state —
    /// a compile tab is the same kind of read-only, line-indexed hit buffer, just filled from raw
    /// tool output rather than formatted `path:line:col` hits.
    pub fn open_compile_listing(&mut self, cmd: String, cwd: PathBuf) {
        self.gen = next_content_gen();
        self.buffer = Buffer::from_str("");
        self.buffer.set_caret(0, None);
        self.undo.clear();
        self.redo.clear();
        self.coalescing = false;
        self.saved_len = Some(0);
        self.path = None;
        self.line_ending = LineEnding::Lf;
        self.read_only = true;
        self.highlighter = None;
        self.tokens.clear();
        self.label = Some(format!("compile {cmd}"));
        self.grep = Some(crate::grep::GrepState {
            base: cwd,
            pattern: cmd,
            regex: false,
            perl: false,
            capped: false,
            hits: Vec::new(),
        });
    }

    /// Append one raw output line (WITHOUT a trailing newline) to a `COMPILE`/`GREP` listing, plus its
    /// parsed location: `Some((path,line,col))` → a navigable `Match`, `None` → a non-navigable
    /// `Separator`. Keeps the invariant **line N → `hits[N]`** so `grep_hit_at_cursor`/`GJ`/`NERR`
    /// resolve the right hit. No-op if this isn't a listing.
    pub fn push_output_line(&mut self, text: &str, loc: Option<(PathBuf, usize, usize)>) {
        if self.grep.is_none() {
            return;
        }
        self.append_log(&format!("{text}\n"));
        self.undo.clear(); // a listing isn't undoable — don't accumulate history as it streams
        self.redo.clear();
        self.mark_saved(); // keep the tab from reading "modified" as output streams
        let hit = match loc {
            Some((path, line, col)) => crate::grep::GrepHit::match_at(path, line, col, text.to_string()),
            None => crate::grep::GrepHit::separator(),
        };
        self.grep.as_mut().unwrap().hits.push(hit);
    }

    /// Mark a streaming `GREP` search complete (records whether the hit cap was reached).
    pub fn finalize_grep(&mut self, capped: bool) {
        if let Some(g) = self.grep.as_mut() {
            g.capped = capped;
        }
    }

    /// Whether this buffer is a `GREP` results listing.
    pub fn is_grep_listing(&self) -> bool {
        self.grep.is_some()
    }

    /// Load a `HELP`/`MAN` results listing into this buffer: read-only, unnamed (no path), with the
    /// caret at the top and marked so the next `HELP`/`MAN` reuses this same tab in place. `label` is
    /// the tab title; `text` is the pre-formatted results body (the caller sets `EMPH` separately).
    pub fn open_help_listing(&mut self, label: String, text: &str) {
        self.gen = next_content_gen(); // whole-buffer swap resets the rope rev
        self.buffer = Buffer::from_str(text);
        self.buffer.set_caret(0, None);
        self.undo.clear();
        self.redo.clear();
        self.coalescing = false;
        self.saved_len = Some(0);
        self.path = None;
        self.line_ending = LineEnding::Lf;
        self.read_only = true;
        self.highlighter = None;
        self.tokens.clear();
        self.label = Some(label);
        self.help_listing = true;
    }

    /// Whether this buffer is a `HELP`/`MAN` results listing (the reusable read-only results tab).
    pub fn is_help_listing(&self) -> bool {
        self.help_listing
    }

    /// The number of `GREP` hits accumulated so far (0 when this isn't a results listing).
    pub fn grep_hit_count(&self) -> usize {
        self.grep.as_ref().map(|g| g.hits.len()).unwrap_or(0)
    }

    /// The number of navigable `Match` hits (locations) in a listing — the error count for a
    /// `COMPILE` tab (excludes `Separator`/`Context` lines). 0 when this isn't a listing.
    pub fn compile_match_count(&self) -> usize {
        self.grep
            .as_ref()
            .map(|g| g.hits.iter().filter(|h| h.kind == crate::grep::HitKind::Match).count())
            .unwrap_or(0)
    }

    /// The `GREP` hit on the caret's line, if this is a results listing — a `Match` or `Context`
    /// line (both jumpable via `GJ`); a blank `Separator` line yields `None`.
    pub fn grep_hit_at_cursor(&self) -> Option<&crate::grep::GrepHit> {
        let grep = self.grep.as_ref()?;
        grep.hits.get(self.buffer.cursor_line()).filter(|h| h.kind != crate::grep::HitKind::Separator)
    }

    /// The listing's browser state, if this is a directory listing.
    pub fn dir_state(&self) -> Option<&DirState> {
        self.dir.as_ref()
    }

    /// The entry on the caret's line, if this is a directory listing.
    pub fn dir_entry_at_cursor(&self) -> Option<&crate::dir::DirEntry> {
        let dir = self.dir.as_ref()?;
        dir.entries.get(self.buffer.cursor_line())
    }

    /// The 0-based line/entry the caret is on in a listing (the selected row). Used by the
    /// separate `DIR/W` window to render the highlight.
    pub fn dir_cursor_row(&self) -> usize {
        self.buffer.cursor_line()
    }

    /// Move the caret to listing row `row` (a click in the `DIR/W` window). Clamped to the
    /// entry count; lands at the line start so `dir_entry_at_cursor` resolves it.
    pub fn dir_goto_row(&mut self, row: usize) {
        let last = self.dir.as_ref().map(|d| d.entries.len().saturating_sub(1)).unwrap_or(0);
        self.buffer.move_to_line(row.min(last), false);
        self.buffer.move_line_start(false);
    }

    /// Remove the entry for `path` from this listing (when it IS one and the path is present),
    /// re-render, and keep the caret on the same ROW — now showing the entry that followed. Returns
    /// whether a row was removed. Used after a delete so the file's line vanishes from an open
    /// listing (`line N → entries[N]` is preserved).
    pub fn listing_remove_path(&mut self, path: &std::path::Path) -> bool {
        let Some(idx) = self.dir.as_ref().and_then(|d| d.entries.iter().position(|e| e.path == path))
        else {
            return false;
        };
        let row = self.buffer.cursor_line();
        let bare = self.dir.as_ref().is_some_and(|d| d.opts.bare);
        let text = {
            let dir = self.dir.as_mut().unwrap();
            dir.entries.remove(idx);
            crate::dir::render_listing(&dir.entries, bare)
        };
        self.gen = next_content_gen(); // whole-buffer swap resets the rope rev
        self.buffer = Buffer::from_str(&text);
        // Keep the caret on the same visual row (the entry that shifted up into the gap); a deletion
        // at or before it moves it up one. `dir_goto_row` clamps to the new last entry.
        self.dir_goto_row(if row > idx { row - 1 } else { row });
        true
    }

    /// After a file is renamed, update its entry's name + path in this listing (when it IS one and
    /// the old path is present), re-sort with the listing's OWN order/reverse/group options, re-render,
    /// and keep the caret on the SAME entry — which is the renamed file if the caret was on it, so it
    /// follows the rename to its new sorted row. Returns whether an entry was updated.
    pub fn listing_rename_path(&mut self, from: &std::path::Path, to: &std::path::Path) -> bool {
        let Some(idx) = self.dir.as_ref().and_then(|d| d.entries.iter().position(|e| e.path == from))
        else {
            return false;
        };
        let row = self.buffer.cursor_line();
        let (text, caret_row) = {
            let dir = self.dir.as_mut().unwrap();
            // Track the entry the caret is on so it can follow the re-sort (→ the renamed file if the
            // caret was pointing at it).
            let mut caret_path = dir.entries.get(row).map(|e| e.path.clone());
            let new_name = to
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| to.to_string_lossy().into_owned());
            dir.entries[idx].name = new_name;
            dir.entries[idx].path = to.to_path_buf();
            if caret_path.as_deref() == Some(from) {
                caret_path = Some(to.to_path_buf());
            }
            crate::dir::sort_entries(&mut dir.entries, dir.opts.order, dir.opts.reverse, !dir.opts.recurse);
            let caret_row = caret_path
                .and_then(|p| dir.entries.iter().position(|e| e.path == p))
                .unwrap_or(row);
            (crate::dir::render_listing(&dir.entries, dir.opts.bare), caret_row)
        };
        self.gen = next_content_gen();
        self.buffer = Buffer::from_str(&text);
        self.dir_goto_row(caret_row);
        true
    }


    /// Move the caret to the first entry whose name STARTS WITH the express-key buffer
    /// (case-insensitive) — a user typing in a listing wants the file beginning with what they type.
    /// No match (or an empty buffer) leaves the caret put.
    fn dir_seek(&mut self) {
        let Some(dir) = self.dir.as_ref() else { return };
        if dir.find.is_empty() {
            return;
        }
        let needle = dir.find.to_lowercase();
        if let Some(i) = dir.entries.iter().position(|e| e.name.to_lowercase().starts_with(&needle)) {
            self.buffer.move_to_line(i, false);
            self.buffer.move_line_start(false);
        }
    }

    /// Express-key: extend the type-to-find buffer by `ch` and seek to the first match.
    pub fn dir_find_push(&mut self, ch: char) {
        if let Some(dir) = self.dir.as_mut() {
            dir.find.push(ch);
        }
        self.dir_seek();
    }

    /// Express-key: shorten the type-to-find buffer by one char and re-seek.
    pub fn dir_find_backspace(&mut self) {
        if let Some(dir) = self.dir.as_mut() {
            dir.find.pop();
        }
        self.dir_seek();
    }

    /// Express-key: clear the type-to-find buffer (e.g. on Esc). The caret stays put.
    pub fn dir_find_clear(&mut self) {
        if let Some(dir) = self.dir.as_mut() {
            dir.find.clear();
        }
    }

    /// `F3` — jump to the NEXT entry matching the current express-key buffer, searching
    /// forward from the caret's line and wrapping around. No-op if the buffer is empty or
    /// nothing matches. (`dir_find_push`/`backspace` always seek the FIRST match; this
    /// steps through the rest.)
    pub fn dir_find_again(&mut self) {
        let target = {
            let Some(dir) = self.dir.as_ref() else { return };
            if dir.find.is_empty() || dir.entries.is_empty() {
                return;
            }
            let needle = dir.find.to_lowercase();
            let n = dir.entries.len();
            let cur = self.buffer.cursor_line();
            (1..=n)
                .map(|off| (cur + off) % n)
                .find(|&i| dir.entries[i].name.to_lowercase().starts_with(&needle))
        };
        if let Some(i) = target {
            self.buffer.move_to_line(i, false);
            self.buffer.move_line_start(false);
        }
    }

    /// `SORT` — sort the selected lines under `spec` as one undo unit, then re-select the
    /// sorted region. Errors when there is no non-empty selection. Returns
    /// `(lines_in, lines_out)` (they differ only when `/D` dropped duplicates).
    pub fn sort_selection(&mut self, spec: &SortSpec) -> Result<(usize, usize), &'static str> {
        let (start, end) = match self.buffer.selection_span() {
            Some((a, b)) if b > a => (a, b),
            Some(_) => return Err("SORT: empty selection"),
            None => return Err("SORT: no text is selected"),
        };
        let (result, n_in, n_out) = crate::sort::sort_lines(&self.buffer.slice(start, end), spec);
        let new_end = self.replace_span(start, end, &result);
        // Keep the sorted region selected, but put the caret at its START (so the next
        // keystroke / motion begins at the top of the block, matching QN).
        self.buffer.set_caret(start, Some(new_end));
        self.refresh_highlights();
        Ok((n_in, n_out))
    }

    /// `RE` — delete from the cursor to the end of the line (recorded; at EOL eats
    /// the break, joining the next line up).
    pub fn delete_to_eol(&mut self) {
        let c = self.buffer.cursor();
        let end = self.buffer.rest_of_line_end();
        if end > c {
            self.replace_span(c, end, "");
        }
    }

    /// `RW` — delete the whole word at the cursor (+ a trailing space). Recorded.
    pub fn delete_word(&mut self) {
        let (s, e) = self.buffer.word_span();
        if e > s {
            self.replace_span(s, e, "");
        }
    }

    /// `RL` — delete the whole line at the cursor, including its break. Recorded.
    pub fn delete_line(&mut self) {
        let (s, e) = self.buffer.line_span();
        if e > s {
            self.replace_span(s, e, "");
        }
    }

    /// `RL` — delete the current VISUAL (wrapped, on-screen) line. `span` is the caret row's byte range
    /// computed by the GUI from the shaped layout (`[row start, next row start)` in rope offsets). When
    /// it's absent or doesn't actually contain the caret — headless, `WRAP off`, or a stale span from a
    /// caret move since the last frame — fall back to deleting the whole LOGICAL line (`RLL`), which is
    /// the visual line whenever nothing wraps. Recorded.
    pub fn delete_visual_line(&mut self, span: Option<(usize, usize)>) {
        let cur = self.buffer.cursor();
        match span {
            Some((s, e)) if e > s && s <= cur && cur <= e => {
                self.replace_span(s, e, "");
            }
            _ => self.delete_line(),
        }
    }

    /// `RS` — delete the whole sentence at the cursor (+ trailing space). Recorded.
    pub fn delete_sentence(&mut self) {
        let cursor = self.buffer.cursor();
        let (s, e) = self.buffer.sentence_span();
        let content_end = self.buffer.content_end(s, e);
        // A) If the caret sits in the SAME-LINE whitespace gap AFTER this sentence (e.g. on the space
        //    between two sentences), delete the sentence AFTER it — the one the caret points toward —
        //    not the one before. A gap that crosses a line break (the sentence ended at end of line,
        //    the next is on another line) does NOT count, so RS there still deletes the current one.
        let same_line_gap = content_end < e
            && cursor >= content_end
            && self.buffer.first_line_break(content_end, e).is_none();
        let (start, mut end) = if same_line_gap {
            let (ns, ne) = self.buffer.sentence_span_from(e);
            if ne > ns { (ns, ne) } else { (s, e) }
        } else {
            (s, e)
        };
        // B) Leave a trailing NEWLINE in place: if the target sentence's trailing whitespace crosses a
        //    line break (the sentence ends at end of line), stop the delete before that break — so the
        //    line structure survives; only the sentence (and any spaces before the break) go.
        let ce = self.buffer.content_end(start, end);
        if let Some(nl) = self.buffer.first_line_break(ce, end) {
            end = nl;
        }
        if end > start {
            self.replace_span(start, end, "");
        }
    }

    /// `RP` — delete the whole paragraph at the cursor (+ trailing blanks). Recorded.
    pub fn delete_paragraph(&mut self) {
        let (s, e) = self.buffer.para_span();
        if e > s {
            self.replace_span(s, e, "");
        }
    }

    /// Delete the current selection (if any).
    pub fn delete_selection(&mut self) -> bool {
        self.buffer.commit_define();
        let (cb, ab) = (self.buffer.cursor(), self.buffer.anchor());
        if let Some((a, b)) = self.buffer.selection_span() {
            let removed = self.buffer.slice_bytes(a, b);
            self.buffer.delete_selection();
            let (ca, aa) = (self.buffer.cursor(), self.buffer.anchor());
            self.record(Op { at: a, removed, inserted: Vec::new(), ..Default::default() }, Kind::Other, cb, ab, ca, aa);
            true
        } else {
            false
        }
    }

    // ---- motions (delegate + break coalescing) -----------------------------

    pub fn move_left(&mut self, extend: bool) {
        self.coalescing = false;
        self.buffer.move_left(extend);
    }
    pub fn move_right(&mut self, extend: bool) {
        self.coalescing = false;
        self.buffer.move_right(extend);
    }
    pub fn move_up(&mut self, extend: bool) {
        self.coalescing = false;
        self.buffer.move_up(extend);
    }
    pub fn move_down(&mut self, extend: bool) {
        self.coalescing = false;
        self.buffer.move_down(extend);
    }
    pub fn move_line_start(&mut self, extend: bool) {
        self.coalescing = false;
        self.buffer.move_line_start(extend);
    }
    pub fn move_line_end(&mut self, extend: bool) {
        self.coalescing = false;
        self.buffer.move_line_end(extend);
    }
    pub fn move_doc_start(&mut self, extend: bool) {
        self.coalescing = false;
        self.buffer.move_doc_start(extend);
    }
    pub fn move_doc_end(&mut self, extend: bool) {
        self.coalescing = false;
        self.buffer.move_doc_end(extend);
    }
    pub fn move_next_word(&mut self, extend: bool) {
        self.coalescing = false;
        self.buffer.move_next_word(extend);
    }
    pub fn move_prev_word(&mut self, extend: bool) {
        self.coalescing = false;
        self.buffer.move_prev_word(extend);
    }
    pub fn move_next_para(&mut self, extend: bool) {
        self.coalescing = false;
        self.buffer.move_next_para(extend);
    }
    pub fn move_prev_para(&mut self, extend: bool) {
        self.coalescing = false;
        self.buffer.move_prev_para(extend);
    }
    pub fn move_next_sentence(&mut self, extend: bool) {
        self.coalescing = false;
        self.buffer.move_next_sentence(extend);
    }
    pub fn move_prev_sentence(&mut self, extend: bool) {
        self.coalescing = false;
        self.buffer.move_prev_sentence(extend);
    }
    pub fn move_next_line_start(&mut self, extend: bool) {
        self.coalescing = false;
        self.buffer.move_next_line_start(extend);
    }
    pub fn move_prev_line_start(&mut self, extend: bool) {
        self.coalescing = false;
        self.buffer.move_prev_line_start(extend);
    }
    pub fn move_to_char(&mut self, n: usize, extend: bool) {
        self.coalescing = false;
        self.buffer.move_to_char(n, extend);
    }
    pub fn move_to_line(&mut self, line: usize, extend: bool) {
        self.coalescing = false;
        self.buffer.move_to_line(line, extend);
    }
    pub fn move_lines(&mut self, delta: i32, extend: bool) {
        self.coalescing = false;
        self.buffer.move_lines(delta, extend);
    }
    pub fn move_left_same_line(&mut self, extend: bool) {
        self.coalescing = false;
        self.buffer.move_left_same_line(extend);
    }
    pub fn move_right_same_line(&mut self, extend: bool) {
        self.coalescing = false;
        self.buffer.move_right_same_line(extend);
    }
    pub fn move_by_char(&mut self, delta: i64, extend: bool) {
        self.coalescing = false;
        self.buffer.move_by_char(delta, extend);
    }
    pub fn move_to_offset(&mut self, off: usize, extend: bool) {
        self.coalescing = false;
        self.buffer.move_to_offset(off, extend);
    }

    // ---- defined-text selection (DF/DM/DZ/DFA/XD) --------------------------
    // Selection-state changes, not edits: they break coalescing but record no undo.

    /// `DF` — toggle a defined-text selection. Returns `true` if it opened.
    pub fn define_toggle(&mut self) -> bool {
        self.coalescing = false;
        self.buffer.toggle_define()
    }
    /// `DZ` / `DF`-close — freeze an open defined-text selection.
    pub fn define_close(&mut self) {
        self.coalescing = false;
        self.buffer.close_define();
    }
    /// `DM` — snap the open selection's active end to the cursor.
    pub fn define_modify(&mut self) {
        self.coalescing = false;
        self.buffer.modify_define();
    }
    /// `DFA` — select the whole document.
    pub fn select_all(&mut self) {
        self.coalescing = false;
        self.buffer.select_all();
    }
    /// `XD` / `YD` — cancel any selection (and end defining).
    pub fn cancel_selection(&mut self) {
        self.coalescing = false;
        self.buffer.clear_selection();
    }
    /// `DW` — define (select) the word at the cursor as a defined block.
    pub fn define_word(&mut self) {
        self.coalescing = false;
        let (s, e) = self.buffer.word_select_span();
        self.buffer.define_range(s, e);
    }
    /// `DL` — define the whole line (including its break).
    pub fn define_line(&mut self) {
        self.coalescing = false;
        let (s, e) = self.buffer.line_select_span();
        self.buffer.define_range(s, e);
    }
    /// `DS` — define the sentence at the cursor.
    pub fn define_sentence(&mut self) {
        self.coalescing = false;
        let (s, e) = self.buffer.sentence_select_span();
        self.buffer.define_range(s, e);
    }
    /// `DP` — define the paragraph at the cursor.
    pub fn define_paragraph(&mut self) {
        self.coalescing = false;
        let (s, e) = self.buffer.para_select_span();
        self.buffer.define_range(s, e);
    }
    /// `DW+` — define the word plus its trailing space.
    pub fn define_word_plus(&mut self) {
        self.coalescing = false;
        let (s, e) = self.buffer.word_span();
        self.buffer.define_range(s, e);
    }
    /// `DS+` — define the sentence plus its trailing space.
    pub fn define_sentence_plus(&mut self) {
        self.coalescing = false;
        let (s, e) = self.buffer.sentence_span();
        self.buffer.define_range(s, e);
    }
    /// `DP+` — define the paragraph plus the separating blank line(s).
    pub fn define_paragraph_plus(&mut self) {
        self.coalescing = false;
        let (s, e) = self.buffer.para_span();
        self.buffer.define_range(s, e);
    }
    /// `DIB` — select INSIDE the bracket pair enclosing the caret. No-op (selection unchanged) if the
    /// caret isn't inside a matching `()`/`[]`/`{}`.
    pub fn define_inside_bracket(&mut self) {
        self.coalescing = false;
        if let Some((s, e)) = self.buffer.inside_bracket_span(None) {
            self.buffer.define_range(s, e);
        }
    }
    /// `DIQ` — select INSIDE the quote pair enclosing the caret on its line. No-op if not inside one.
    pub fn define_inside_quote(&mut self) {
        self.coalescing = false;
        if let Some((s, e)) = self.buffer.inside_quote_span(None) {
            self.buffer.define_range(s, e);
        }
    }
    /// True while a `DF` defined-text selection is open.
    pub fn is_defining(&self) -> bool {
        self.buffer.is_defining()
    }
    /// Set persistent-selection mode on the buffer (mirrors the global flag).
    pub fn set_persistent(&mut self, on: bool) {
        self.buffer.set_persistent(on);
    }
    /// Promote a transient (mouse-drag) selection to a defined block so it persists like a
    /// `DF`/`DW` selection. See [`Buffer::mark_defined`].
    pub fn mark_defined(&mut self) {
        self.buffer.mark_defined();
    }
    /// Place the caret from a mouse click (PS-aware: keeps the block under PS). Not
    /// an edit; closes any coalescing run.
    pub fn click_caret(&mut self, off: usize) {
        self.coalescing = false;
        self.buffer.click_caret(off);
    }

    // ---- undo grouping -----------------------------------------------------

    /// `UNDOBEGIN` — open an undo group: every edit until the matching `UNDOEND` folds into ONE undo
    /// unit. Depth-counted, so nested begins need matching ends; the first edit opens the shared unit.
    pub fn begin_undo_group(&mut self) {
        self.coalescing = false; // close any open typing run so the group starts a clean unit
        self.group_depth += 1;
    }

    /// `UNDOEND` — close one level of undo grouping. Closing the outermost level finalizes the group so
    /// the next edit starts a fresh unit. Unbalanced `UNDOEND`s are harmless (saturating).
    pub fn end_undo_group(&mut self) {
        self.group_depth = self.group_depth.saturating_sub(1);
        if self.group_depth == 0 {
            self.group_active = false;
            self.coalescing = false;
        }
    }

    /// Fold an edit's op(s) into the open group's single unit — opening that unit on the first edit,
    /// extending it thereafter. Self-heals if the undo stack was cleared mid-group (re-opens a unit).
    /// A grouped unit is `Kind::Other` so normal coalescing never merges across its boundary.
    #[allow(clippy::too_many_arguments)]
    fn group_push(
        &mut self,
        mut ops: Vec<Op>,
        cursor_before: usize,
        anchor_before: Option<usize>,
        cursor_after: usize,
        anchor_after: Option<usize>,
        secondary_before: Vec<Caret>,
        secondary_after: Vec<Caret>,
    ) {
        if self.group_active {
            if let Some(last) = self.undo.last_mut() {
                last.ops.append(&mut ops);
                last.cursor_after = cursor_after;
                last.anchor_after = anchor_after;
                // Keep the group's own caret restore target current; adopt a multi-caret set if this
                // edit brought one (a single-caret group leaves `secondary_after` empty throughout).
                if !secondary_after.is_empty() {
                    last.secondary_after = secondary_after;
                }
                return;
            }
        }
        self.undo.push(Unit {
            ops,
            kind: Kind::Other,
            cursor_before,
            anchor_before,
            cursor_after,
            anchor_after,
            secondary_before,
            secondary_after,
            style: None,
        });
        self.group_active = true;
    }

    // ---- undo / redo -------------------------------------------------------

    /// Undo the most recent unit. Returns `false` if there was nothing to undo.
    pub fn undo(&mut self) -> bool {
        self.coalescing = false;
        // A manual UNDO force-closes any open group (a script that began a group and never ended it
        // must not keep folding later edits into a unit that was just undone).
        self.group_depth = 0;
        self.group_active = false;
        if let Some(unit) = self.undo.pop() {
            // Reverse each op in REVERSE application order — the ops were recorded low-to-high with
            // apply-time offsets, so undoing high-to-low keeps every stored offset valid.
            for op in unit.ops.iter().rev() {
                let remove = op.inserted.len();
                self.buffer.replace_range_bytes(op.at, remove, &op.removed);
                // Colour spans follow the undo edit (it removes `inserted`, restores `removed`), then
                // the styling the original edit tore out of `removed` is restored over the gap.
                self.adjust_color_spans(op.at, remove, op.removed.len());
                self.apply_span_snapshot(op.at, &op.removed_spans);
            }
            // A pure styling step (MDA/MDALL): restore the region's PRE-command styling.
            if let Some(sd) = &unit.style {
                let (lo, hi, before) = (sd.lo, sd.hi, sd.before.clone());
                self.restore_style_region(lo, hi, &before);
            }
            self.buffer.set_caret(unit.cursor_before, unit.anchor_before);
            // Restore the whole caret SET only for a genuine multi-caret edit (one that recorded a
            // secondary set); a single-caret edit OR an undo-group of single-caret edits (which also
            // has ops.len() > 1) leaves the caret set alone.
            if !unit.secondary_before.is_empty() {
                self.buffer.set_secondary_carets(unit.secondary_before.clone());
            }
            self.redo.push(unit);
            self.refresh_highlights();
            true
        } else {
            false
        }
    }

    /// Redo the most recently undone unit. Returns `false` if nothing to redo.
    pub fn redo(&mut self) -> bool {
        self.coalescing = false;
        self.group_depth = 0; // force-close any open group (mirrors undo)
        self.group_active = false;
        if let Some(unit) = self.redo.pop() {
            // Re-apply the ops in their original (forward) order.
            for op in unit.ops.iter() {
                let remove = op.removed.len();
                self.buffer.replace_range_bytes(op.at, remove, &op.inserted);
                // Colour spans follow the redo edit (removes `removed`, re-inserts `inserted`), then
                // the inserted text's styling is re-applied (MV/COPY/paste carry it here).
                self.adjust_color_spans(op.at, remove, op.inserted.len());
                self.apply_span_snapshot(op.at, &op.inserted_spans);
            }
            // A pure styling step (MDA/MDALL): re-apply the region's POST-command styling.
            if let Some(sd) = &unit.style {
                let (lo, hi, after) = (sd.lo, sd.hi, sd.after.clone());
                self.restore_style_region(lo, hi, &after);
            }
            self.buffer.set_caret(unit.cursor_after, unit.anchor_after);
            if !unit.secondary_after.is_empty() {
                self.buffer.set_secondary_carets(unit.secondary_after.clone());
            }
            self.undo.push(unit);
            self.refresh_highlights();
            true
        } else {
            false
        }
    }

    // ---- multi-caret fan-out ----------------------------------------------

    /// Insert `s` at EVERY caret (the multi-caret path). Edits are applied low-to-high with a
    /// cumulative shift (so each caret's live position accounts for the inserts before it), and the
    /// whole fan-out is recorded as ONE undo unit. Each caret replaces its own selection if it has
    /// one (bare carets just insert). Ends the keyboard building state.
    fn insert_multi_carets(&mut self, s: &str) {
        self.buffer.commit_define();
        self.buffer.normalize_carets(); // fold any caret coincident with the primary → insert ONCE
        let ins = self.buffer.encode(s);
        let sec_before = self.buffer.secondary_carets().to_vec();
        let (cb, ab) = (self.buffer.cursor(), self.buffer.anchor());
        // Collect every caret's edit span in current coords: (start, end, is_primary).
        let mut items: Vec<(usize, usize, bool)> = Vec::new();
        let (ps, pe) = self.buffer.selection_span().unwrap_or((cb, cb));
        items.push((ps, pe, true));
        for c in &sec_before {
            let (a, b) = match c.anchor {
                Some(an) => (an.min(c.pos), an.max(c.pos)),
                None => (c.pos, c.pos),
            };
            items.push((a, b, false));
        }
        items.sort_by(|x, y| x.0.cmp(&y.0));
        // Pure inserts (no caret replaced a selection) form a coalescing typing run; a replace is its
        // own step (like a single-caret selection-replacing insert).
        let kind = if items.iter().any(|&(a, b, _)| b > a) { Kind::Other } else { Kind::MultiInsert };
        // Apply ascending with a running shift; collect one Op per caret + the resulting positions.
        let mut ops: Vec<Op> = Vec::new();
        let mut shift: i64 = 0;
        let mut new_primary = cb;
        let mut new_secs: Vec<Caret> = Vec::new();
        for (a, b, is_primary) in items {
            let at = (a as i64 + shift) as usize;
            let end = (b as i64 + shift) as usize;
            let removed = self.buffer.slice_bytes(at, end);
            self.buffer.replace_range_bytes(at, end - at, &ins);
            let after = at + ins.len();
            ops.push(Op { at, removed, inserted: ins.clone(), ..Default::default() });
            shift += ins.len() as i64 - (end - at) as i64;
            if is_primary {
                new_primary = after;
            } else {
                new_secs.push(Caret { pos: after, anchor: None, goal_col: None });
            }
        }
        self.buffer.set_caret(new_primary, None);
        self.buffer.set_secondary_carets(new_secs);
        self.buffer.normalize_carets();
        let sec_after = self.buffer.secondary_carets().to_vec();
        self.buffer.set_building(false); // the first edit ends the building state
        self.record_multi(ops, kind, cb, ab, new_primary, None, sec_before, sec_after);
    }

    /// Delete at EVERY caret (Backspace when `forward` is false, Delete when true) — the multi-caret
    /// path. Each caret removes its own selection if it has one, else the char before/at it (char-
    /// aware via the buffer's delete spans). Applied low-to-high with a cumulative shift, recorded as
    /// ONE undo unit. No-op carets (Backspace at BOF / Delete at EOF) contribute nothing.
    fn delete_multi_carets(&mut self, forward: bool) {
        self.buffer.commit_define();
        self.buffer.normalize_carets(); // fold any caret coincident with the primary → delete ONCE
        let sec_before = self.buffer.secondary_carets().to_vec();
        let (cb, ab) = (self.buffer.cursor(), self.buffer.anchor());
        // Each caret's delete span in current coords: (start, end, is_primary).
        let span_of = |buf: &Buffer, pos: usize, anchor: Option<usize>| -> (usize, usize) {
            if let Some(an) = anchor {
                (an.min(pos), an.max(pos))
            } else if forward {
                buf.delete_forward_span(pos)
            } else {
                buf.delete_back_span(pos)
            }
        };
        let mut items: Vec<(usize, usize, bool)> = Vec::new();
        let (pa, pb) = match self.buffer.selection_span() {
            Some((a, b)) => (a, b),
            None => span_of(&self.buffer, cb, None),
        };
        items.push((pa, pb, true));
        for c in &sec_before {
            let (a, b) = span_of(&self.buffer, c.pos, c.anchor);
            items.push((a, b, false));
        }
        items.sort_by(|x, y| x.0.cmp(&y.0));
        let mut ops: Vec<Op> = Vec::new();
        let mut shift: i64 = 0;
        let mut new_primary = cb;
        let mut new_secs: Vec<Caret> = Vec::new();
        for (a, b, is_primary) in items {
            let at = (a as i64 + shift) as usize;
            let end = (b as i64 + shift) as usize;
            if end > at {
                let removed = self.buffer.slice_bytes(at, end);
                self.buffer.replace_range_bytes(at, end - at, &[]);
                ops.push(Op { at, removed, inserted: Vec::new(), ..Default::default() });
                shift -= (end - at) as i64;
            }
            // The caret lands at the span start (BOF/EOF no-ops leave it where it was).
            if is_primary {
                new_primary = at;
            } else {
                new_secs.push(Caret { pos: at, anchor: None, goal_col: None });
            }
        }
        self.buffer.set_caret(new_primary, None);
        self.buffer.set_secondary_carets(new_secs);
        self.buffer.normalize_carets();
        let sec_after = self.buffer.secondary_carets().to_vec();
        self.buffer.set_building(false);
        if !ops.is_empty() {
            // Multi-caret deletes are each their own undo step (no run-coalescing yet).
            self.record_multi(ops, Kind::Other, cb, ab, new_primary, None, sec_before, sec_after);
        }
    }

    /// Multi-caret `PASTE` with N stored clipboard pieces and N carets: insert piece `i` at caret `i`
    /// (document order), replacing each caret's selection. Offset-safe (low-to-high with a running
    /// shift), one undo unit. Extra/short pieces map to `""` (the host only calls this on an exact
    /// count match). Each caret lands after its inserted piece.
    pub fn paste_distributed(&mut self, pieces: &[String]) {
        self.buffer.commit_define();
        self.buffer.normalize_carets();
        self.coalescing = false;
        let carets = self.buffer.all_carets(); // document order
        let sec_before = self.buffer.secondary_carets().to_vec();
        let (cb, ab) = (self.buffer.cursor(), self.buffer.anchor());
        // (start, end, piece bytes, is_primary) per caret; sort by start so the running shift is valid.
        let mut items: Vec<(usize, usize, Vec<u8>, bool)> = carets
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let (a, b) = match c.anchor {
                    Some(an) => (an.min(c.pos), an.max(c.pos)),
                    None => (c.pos, c.pos),
                };
                let ins = self.buffer.encode(pieces.get(i).map(String::as_str).unwrap_or(""));
                (a, b, ins, c.pos == cb)
            })
            .collect();
        items.sort_by(|x, y| x.0.cmp(&y.0));
        let mut ops: Vec<Op> = Vec::new();
        let mut shift: i64 = 0;
        let mut new_primary = cb;
        let mut new_secs: Vec<Caret> = Vec::new();
        for (a, b, ins, is_primary) in items {
            let at = (a as i64 + shift) as usize;
            let end = (b as i64 + shift) as usize;
            let removed = self.buffer.slice_bytes(at, end);
            self.buffer.replace_range_bytes(at, end - at, &ins);
            let after = at + ins.len();
            ops.push(Op { at, removed, inserted: ins.clone(), ..Default::default() });
            shift += ins.len() as i64 - (end - at) as i64;
            if is_primary {
                new_primary = after;
            } else {
                new_secs.push(Caret { pos: after, anchor: None, goal_col: None });
            }
        }
        self.buffer.set_caret(new_primary, None);
        self.buffer.set_secondary_carets(new_secs);
        self.buffer.normalize_carets();
        let sec_after = self.buffer.secondary_carets().to_vec();
        self.buffer.set_building(false);
        if !ops.is_empty() {
            self.record_multi(ops, Kind::Other, cb, ab, new_primary, None, sec_before, sec_after);
        }
    }

    /// Delete every caret's SELECTION as one undo unit (the multi-caret `CUT`). A bare caret deletes
    /// nothing but rides along with the shift; each selection collapses to a bare caret at its start.
    pub fn delete_all_selections(&mut self) {
        self.buffer.commit_define();
        self.buffer.normalize_carets();
        self.coalescing = false;
        let carets = self.buffer.all_carets();
        let sec_before = self.buffer.secondary_carets().to_vec();
        let (cb, ab) = (self.buffer.cursor(), self.buffer.anchor());
        let mut items: Vec<(usize, usize, bool)> = carets
            .iter()
            .map(|c| {
                let (a, b) = match c.anchor {
                    Some(an) => (an.min(c.pos), an.max(c.pos)),
                    None => (c.pos, c.pos), // bare: no-op span
                };
                (a, b, c.pos == cb)
            })
            .collect();
        items.sort_by(|x, y| x.0.cmp(&y.0));
        let mut ops: Vec<Op> = Vec::new();
        let mut shift: i64 = 0;
        let mut new_primary = cb;
        let mut new_secs: Vec<Caret> = Vec::new();
        for (a, b, is_primary) in items {
            let at = (a as i64 + shift) as usize;
            let end = (b as i64 + shift) as usize;
            if end > at {
                let removed = self.buffer.slice_bytes(at, end);
                self.buffer.replace_range_bytes(at, end - at, &[]);
                ops.push(Op { at, removed, inserted: Vec::new(), ..Default::default() });
                shift -= (end - at) as i64;
            }
            if is_primary {
                new_primary = at;
            } else {
                new_secs.push(Caret { pos: at, anchor: None, goal_col: None });
            }
        }
        self.buffer.set_caret(new_primary, None);
        self.buffer.set_secondary_carets(new_secs);
        self.buffer.normalize_carets();
        let sec_after = self.buffer.secondary_carets().to_vec();
        self.buffer.set_building(false);
        if !ops.is_empty() {
            self.record_multi(ops, Kind::Other, cb, ab, new_primary, None, sec_before, sec_after);
        }
    }

    /// Record a multi-caret edit (N ops) as ONE undo unit. A `MultiInsert` typing run coalesces into
    /// the previous unit (matched by caret index) so a run undoes at once; anything else is its own step.
    #[allow(clippy::too_many_arguments)]
    fn record_multi(
        &mut self,
        mut ops: Vec<Op>,
        kind: Kind,
        cursor_before: usize,
        anchor_before: Option<usize>,
        cursor_after: usize,
        anchor_after: Option<usize>,
        secondary_before: Vec<Caret>,
        secondary_after: Vec<Caret>,
    ) {
        self.caret_ring.clear(); // any edit invalidates the transient CARETPOP snapshots
        // Colour spans + lint underlines follow each op, in application order; snapshot the styling
        // each op removes (for undo) before the shift tears it out, and style any inserted text.
        for op in &mut ops {
            op.removed_spans = self.capture_spans(op.at, op.removed.len());
            self.adjust_color_spans(op.at, op.removed.len(), op.inserted.len());
            self.apply_span_snapshot(op.at, &op.inserted_spans);
            self.adjust_lints(op.at, op.removed.len(), op.inserted.len());
        }
        if let Some(sl) = self.saved_len {
            if sl > self.undo.len() {
                self.saved_len = None;
            }
        }
        self.redo.clear();
        // Inside an undo group, fold the multi-caret edit's ops into the group's single unit too.
        if self.group_depth > 0 {
            self.group_push(ops, cursor_before, anchor_before, cursor_after, anchor_after, secondary_before, secondary_after);
            self.coalescing = false;
            self.refresh_highlights();
            return;
        }
        // Coalesce a continuing multi-caret typing run: same kind + same op count + the carets are
        // exactly where the last run left them (nothing moved/clicked between). Extend each op's
        // inserted bytes by caret index; the carets stay in the same order across a run.
        if kind == Kind::MultiInsert && self.coalescing {
            if let Some(last) = self.undo.last_mut() {
                if last.kind == Kind::MultiInsert
                    && last.ops.len() == ops.len()
                    && last.cursor_after == cursor_before
                    && last.secondary_after == secondary_before
                {
                    // Extend each caret's run, and shift its start right by the bytes the LOWER
                    // carets added this keystroke (idx carets each added `o.inserted.len()`), so the
                    // stored `at` stays valid in the growing text (undo reverses high-offset-first).
                    for (idx, (lo, o)) in last.ops.iter_mut().zip(ops.iter()).enumerate() {
                        lo.at += idx * o.inserted.len();
                        lo.inserted.extend_from_slice(&o.inserted);
                    }
                    last.cursor_after = cursor_after;
                    last.anchor_after = anchor_after;
                    last.secondary_after = secondary_after;
                    self.refresh_highlights();
                    return;
                }
            }
        }
        self.coalescing = kind == Kind::MultiInsert; // only insert runs continue coalescing
        self.undo.push(Unit {
            ops,
            kind,
            cursor_before,
            anchor_before,
            cursor_after,
            anchor_after,
            secondary_before,
            secondary_after,
            style: None,
        });
        self.refresh_highlights();
    }

    // ---- recording / coalescing -------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn record(
        &mut self,
        mut op: Op,
        kind: Kind,
        cursor_before: usize,
        anchor_before: Option<usize>,
        cursor_after: usize,
        anchor_after: Option<usize>,
    ) {
        // Capture the edit's location + inserted length before `op` may be moved into `merge` below —
        // the incremental highlighter re-tokenizes just the changed line(s) from these.
        let (edit_at, edit_ins) = (op.at, op.inserted.len());
        self.caret_ring.clear(); // any edit invalidates the transient CARETPOP snapshots
        // Snapshot the MDA styling the edit is about to tear out, BEFORE `adjust_color_spans` destroys
        // it, so undo can restore it. An edit touching no styled text snapshots nothing (free).
        op.removed_spans = self.capture_spans(op.at, op.removed.len());
        let op_has_spans = !op.removed_spans.is_empty() || !op.inserted_spans.is_empty();
        // Transient FGCOLOR-on-selection spans follow the edit (typing before a coloured word
        // shifts its colour along, an edit inside grows/shrinks it, a deletion over it clips it).
        self.adjust_color_spans(op.at, op.removed.len(), op.inserted.len());
        // Style the inserted text (MV / COPY-to-cursor / styled paste carry a non-empty snapshot);
        // a no-op for ordinary typing.
        self.apply_span_snapshot(op.at, &op.inserted_spans);
        // Live spelling/grammar underlines follow the edit too — otherwise every keystroke redraws
        // them one byte out of place until the async re-check lands (a screen-wide flicker).
        self.adjust_lints(op.at, op.removed.len(), op.inserted.len());
        // If the saved state lived in the redo region we're about to discard, it
        // becomes unreachable by undo -> the document is permanently modified.
        if let Some(sl) = self.saved_len {
            if sl > self.undo.len() {
                self.saved_len = None;
            }
        }
        self.redo.clear(); // a new edit invalidates the redo stack

        // Inside an undo group (UNDOBEGIN…UNDOEND), fold this edit into the group's single unit
        // instead of pushing its own — so the whole composite undoes as one step.
        if self.group_depth > 0 {
            self.group_push(vec![op], cursor_before, anchor_before, cursor_after, anchor_after, Vec::new(), Vec::new());
            self.coalescing = false;
            self.note_edit(edit_at, edit_ins);
            return;
        }

        // An op that REMOVED styled text never coalesces — merging two ops would have to merge their
        // `removed_spans` too (fiddly, and rare). A clean undo boundary there is fine. `inserted_spans`
        // (typing-mode styling, stamped onto the op AFTER this runs) is fine to coalesce: a styled typed
        // run still undoes as one unit, and `stamp_typed_run_styling` recomputes the whole run each key.
        let blocks_coalesce = !op.removed_spans.is_empty();
        if self.coalescing && kind != Kind::Other && !blocks_coalesce {
            if let Some(last) = self.undo.last_mut() {
                if last.kind == kind && last.ops.last().is_some_and(|o| o.removed_spans.is_empty()) && coalesces(last, &op) {
                    merge(last, op, cursor_after, anchor_after);
                    self.note_edit(edit_at, edit_ins);
                    return;
                }
            }
        }

        self.undo.push(Unit {
            ops: vec![op],
            kind,
            cursor_before,
            anchor_before,
            cursor_after,
            anchor_after,
            secondary_before: Vec::new(),
            secondary_after: Vec::new(),
            style: None,
        });
        self.coalescing = kind != Kind::Other && !op_has_spans;
        self.note_edit(edit_at, edit_ins);
    }
}

/// Provisional default encoding view for freshly-loaded bytes (decision ET2:
/// full detection/override is a later step). In priority order:
/// 1. **UTF-8** if the bytes are valid UTF-8 — modern files (and pure ASCII).
/// 2. **CP437** if the bytes contain XyWrite function-code delimiters
///    (`0xAE`/`0xAF`) — those render as the guillemets `«…»`.
/// 3. **Latin-1** otherwise — a lossless fallback for unknown single-byte text.
///
/// The UTF-8 check comes FIRST on purpose: `0xAE`/`0xAF` are not only the XyWrite
/// delimiters, they are also perfectly ordinary UTF-8 *continuation* bytes, so a
/// real UTF-8 file routinely contains them — e.g. Arabic "د" (U+062F) encodes as
/// `0xD8 0xAF`. Sniffing for those bytes first (the old order) forced such a file
/// to CP437 and mojibake'd it on open. A genuine XyWrite `.26` file uses `0xAE`/
/// `0xAF` as STANDALONE delimiters, i.e. a continuation byte with no lead byte,
/// which is invalid UTF-8 — so it fails step 1 and still lands on CP437 at step 2.
///
/// The chosen view never affects byte-exactness (load stores bytes verbatim); it
/// only affects display and how new typing encodes. The user can override live
/// with the `ENCODING` verb.
fn default_encoding_for(bytes: &[u8]) -> Charset {
    if std::str::from_utf8(bytes).is_ok() {
        crate::encoding::utf8()
    } else if bytes.iter().any(|&b| b == 0xAE || b == 0xAF) {
        crate::encoding::cp437()
    } else {
        crate::encoding::latin1()
    }
}

/// Whether `op` continues `last`'s coalescing run (same kind already checked).
fn coalesces(last: &Unit, op: &Op) -> bool {
    // Only single-op (single-caret) runs coalesce; a multi-caret unit is its own step.
    if last.ops.len() != 1 {
        return false;
    }
    let lop = &last.ops[0];
    match last.kind {
        // Typing forward: the new insert begins right after the run's bytes.
        Kind::Insert => op.at == lop.at + lop.inserted.len(),
        // Backspace: the new deletion sits immediately before the run's region.
        Kind::DeleteBack => op.at + op.removed.len() == lop.at,
        // Forward-delete: always removes at the same position.
        Kind::DeleteForward => op.at == lop.at,
        // Multi-caret runs coalesce in `record_multi` (by caret index), not here.
        Kind::MultiInsert | Kind::Other => false,
    }
}

/// Merge `op` into `last`'s run (same kind, already confirmed contiguous). Only reached for a
/// single-op unit (coalescing is gated on `last.ops.len() == 1`), so `last.ops[0]` is the run.
fn merge(last: &mut Unit, op: Op, cursor_after: usize, anchor_after: Option<usize>) {
    let lop = &mut last.ops[0];
    match last.kind {
        Kind::Insert => lop.inserted.extend_from_slice(&op.inserted),
        Kind::DeleteForward => lop.removed.extend_from_slice(&op.removed),
        Kind::DeleteBack => {
            // Prepend (we're consuming bytes leftward) and move the anchor left.
            let mut merged = op.removed;
            merged.extend_from_slice(&lop.removed);
            lop.removed = merged;
            lop.at = op.at;
        }
        Kind::MultiInsert | Kind::Other => unreachable!("multi/other coalesce elsewhere or never"),
    }
    last.cursor_after = cursor_after;
    last.anchor_after = anchor_after;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dir::DirEntry;

    fn mark(start: usize, end: usize) -> crate::lint::LintMark {
        crate::lint::LintMark {
            start,
            end,
            kind: "spelling".into(),
            message: String::new(),
        }
    }

    #[test]
    fn set_spans_bulk_matches_sequential_set_span_in() {
        // The bulk setter (MDALL) must produce EXACTLY what applying each range one-by-one would —
        // same interval subtraction, sort, and coalesce — but in a single pass.
        let ranges = [(10usize, 13usize), (0, 3), (4, 7), (20, 25)];
        // Seed a pre-existing span that some ranges overlap/split, to exercise subtraction.
        let seed = || vec![(2usize, 22usize, (9u8, 9u8, 9u8))];

        let mut sequential = seed();
        for &(s, e) in &ranges {
            Document::set_span_in(&mut sequential, s, e, Some((1, 2, 3)));
        }
        let mut bulk = seed();
        Document::set_spans_bulk(&mut bulk, &ranges, Some((1, 2, 3)));
        assert_eq!(bulk, sequential, "bulk set == sequential set");

        // A bulk CLEAR (value None) must equal sequential clears too.
        let mut seq_clear = seed();
        for &(s, e) in &ranges {
            Document::set_span_in(&mut seq_clear, s, e, None::<(u8, u8, u8)>);
        }
        let mut bulk_clear = seed();
        Document::set_spans_bulk(&mut bulk_clear, &ranges, None);
        assert_eq!(bulk_clear, seq_clear, "bulk clear == sequential clear");

        // Adjacent same-value spans coalesce into one.
        let mut adj: Vec<(usize, usize, u8)> = Vec::new();
        Document::set_spans_bulk(&mut adj, &[(0, 3), (3, 6)], Some(1));
        assert_eq!(adj, vec![(0, 6, 1)], "touching same-value ranges coalesce");
    }

    #[test]
    fn highlighting_is_windowed_on_a_large_document() {
        use crate::highlight::{Highlighter, TokenKind};
        // ~450 KB of headings — well past HL_WINDOW_PAD (64 KB), so the whole file must NOT tokenize.
        let text = "# Heading here\n".repeat(30_000);
        let total = text.len();
        let mut d = Document::from_str(&text);
        d.set_highlighter(Some(Highlighter::Markdown)); // refresh: a bounded window around the caret

        // The first pass covers a bounded window (around wherever the caret landed), NOT the whole file.
        assert!(d.hl_valid, "cache built");
        assert!(d.hl_hi - d.hl_lo < total, "covered span is a window, not the whole {total} bytes");
        assert!(d.hl_hi - d.hl_lo <= 2 * HL_WINDOW_PAD + 64, "window is ~the pad, not the file");

        // Scroll to the TOP: a window at 0 is tokenized on demand.
        d.ensure_highlights(0, 100);
        assert!(d.highlights().iter().any(|t| t.kind == TokenKind::Heading && t.start < 64));
        assert!(!d.highlights().is_empty(), "headings at the top are lit");

        // Scroll to the middle (a far jump): the covered range moves there, still bounded.
        let mid = total / 2;
        d.ensure_highlights(mid, mid + 100);
        assert!(d.hl_hi - d.hl_lo < total, "still a bounded window after the jump");
        assert!(
            d.highlights().iter().any(|t| t.kind == TokenKind::Heading && t.start >= mid),
            "a heading in the middle is now lit"
        );

        // A small document is still covered whole (unchanged behaviour for normal files + tests).
        let mut small = Document::from_str("# a\ntext\n# b\n");
        small.set_highlighter(Some(Highlighter::Markdown));
        assert_eq!((small.hl_lo, small.hl_hi), (0, small.buffer().len()), "small file: whole doc");
    }

    #[test]
    fn incremental_edit_matches_a_full_retokenize() {
        use crate::highlight::{Highlighter, Token, TokenKind};
        // Sorted token signature, so within-line rule order can't spuriously fail the comparison.
        fn sig(toks: &[Token]) -> Vec<(usize, usize, String)> {
            let mut v: Vec<_> =
                toks.iter().map(|t| (t.start, t.end, t.kind.name().to_string())).collect();
            v.sort();
            v
        }
        // Whole-covered (small) doc so the incremental splice path (not a rebuild) runs on the edit.
        let mut d = Document::from_str(&"# H\nplain text here\n".repeat(40));
        d.set_highlighter(Some(Highlighter::Markdown));

        // Edit in the middle: insert a new heading line (adds a Heading, splits/reshapes a line, shifts
        // everything after). This goes through `record` -> `note_edit` (the incremental path).
        d.set_caret(100, None);
        d.insert("\n## Inserted heading\nmore\n");
        assert!(d.hl_valid, "still incrementally valid after the edit");

        // The incremental result must equal a from-scratch tokenize of the final text.
        let final_text = d.buffer().text();
        let mut fresh = Document::from_str(&final_text);
        fresh.set_highlighter(Some(Highlighter::Markdown));
        assert_eq!(sig(d.highlights()), sig(fresh.highlights()), "incremental == full re-tokenize");
        assert!(
            d.highlights().iter().any(|t| t.kind == TokenKind::Heading && t.start > 100),
            "the inserted heading is lit"
        );

        // A delete (removing a line) must also stay consistent.
        d.set_caret(0, None);
        d.replace_span(0, 4, ""); // drop the first "# H\n"
        let final_text = d.buffer().text();
        let mut fresh = Document::from_str(&final_text);
        fresh.set_highlighter(Some(Highlighter::Markdown));
        assert_eq!(sig(d.highlights()), sig(fresh.highlights()), "incremental delete == full");
    }

    /// Live underlines must ride along with edits. Before this, the marks kept the offsets they were
    /// computed at, so every keystroke drew each underline after the caret one byte off — the last
    /// letter of each flagged word flickered while typing, the first letter while backspacing.
    #[test]
    fn lint_marks_follow_edits_instead_of_flickering() {
        let mut d = Document::from_str("aaa bbb ccc");
        // Flag "bbb" (4..7) and "ccc" (8..11).
        d.set_lints(vec![mark(4, 7), mark(8, 11)]);

        // Type one character at the very start: both marks shift right by one.
        d.replace_span(0, 0, "X");
        assert_eq!(
            d.lints().iter().map(|m| (m.start, m.end)).collect::<Vec<_>>(),
            vec![(5, 8), (9, 12)],
            "marks after an insert must slide, not stay put"
        );

        // Backspace that character: they shift back.
        d.replace_span(0, 1, "");
        assert_eq!(
            d.lints().iter().map(|m| (m.start, m.end)).collect::<Vec<_>>(),
            vec![(4, 7), (8, 11)]
        );

        // A mark BEFORE the edit is untouched; the word the edit lands inside is dropped (it is
        // being retyped, so its squiggle is stale until the re-check reports).
        d.replace_span(5, 6, "Z"); // inside "bbb"
        assert_eq!(
            d.lints().iter().map(|m| (m.start, m.end)).collect::<Vec<_>>(),
            vec![(8, 11)],
            "the edited word's mark is dropped, the later one survives unmoved"
        );
    }

    /// Typing a letter onto the END of a flagged word EXTENDS it, but the mark's end has left gravity
    /// so it wouldn't grow — it would keep underlining just the old prefix ("recei" of "receipt") until
    /// the async re-check lands (slow on big files → the partial-underline-while-typing report). The
    /// extending edit must DROP the mark; a space/punctuation at the boundary must NOT.
    #[test]
    fn lint_mark_dropped_when_a_word_char_extends_the_flagged_word() {
        let mut d = Document::from_str("aaa recei");
        d.set_lints(vec![mark(4, 9)]); // "recei"
        d.replace_span(9, 9, "pt"); // → "aaa receipt"
        assert!(d.lints().is_empty(), "a word-char append extends the word → drop the stale mark");

        // A NON-word char at the boundary (finishing the misspelling with a space) keeps it flagged.
        let mut d2 = Document::from_str("aaa recei");
        d2.set_lints(vec![mark(4, 9)]);
        d2.replace_span(9, 9, " "); // → "aaa recei "
        assert_eq!(
            d2.lints().iter().map(|m| (m.start, m.end)).collect::<Vec<_>>(),
            vec![(4, 9)],
            "a space after the word is not an extension → keep the squiggle (no flicker)"
        );

        // A word char PREPENDED onto a flagged word's start also extends it → drop.
        let mut d3 = Document::from_str("aaa eipt");
        d3.set_lints(vec![mark(4, 8)]); // "eipt"
        d3.replace_span(4, 4, "rec"); // → "aaa receipt"
        assert!(d3.lints().is_empty(), "a word-char prepend extends the word → drop the stale mark");
    }

    fn entry(name: &str, is_dir: bool) -> DirEntry {
        DirEntry {
            name: name.into(),
            path: PathBuf::from(format!("/d/{name}")),
            is_dir,
            ..DirEntry::default()
        }
    }

    fn sample_listing() -> Document {
        let entries = vec![
            DirEntry { name: "..".into(), path: PathBuf::from("/"), is_dir: true, ..DirEntry::default() },
            entry("alpha", true),
            entry("readme.txt", false),
            entry("notes.md", false),
        ];
        let mut d = Document::new();
        // Wide (names-only) so the assertions below stay independent of size/date columns.
        let opts = DirOpts { bare: true, ..DirOpts::default() };
        d.open_listing(PathBuf::from("/d"), entries, opts, None);
        d
    }

    #[test]
    fn listing_buffer_renders_and_maps_lines_to_entries() {
        let d = sample_listing();
        assert!(d.is_dir_listing());
        // Dirs get a trailing `\`; one entry per line; line N → entries[N].
        assert_eq!(d.buffer().text(), "..\\\nalpha\\\nreadme.txt\nnotes.md");
        // Caret starts at the top → the `..` parent entry.
        assert_eq!(d.dir_entry_at_cursor().map(|e| e.name.as_str()), Some(".."));
    }

    #[test]
    fn express_key_seeks_to_first_prefix_match() {
        let mut d = sample_listing(); // .. , alpha, readme.txt, notes.md
        // "Not" → notes.md (the first name STARTING WITH it, case-insensitive).
        d.dir_find_push('N');
        d.dir_find_push('o');
        d.dir_find_push('t');
        assert_eq!(d.dir_state().unwrap().find, "Not");
        assert_eq!(d.dir_entry_at_cursor().map(|e| e.name.as_str()), Some("notes.md"));
        // Backspace shortens to "No" — still lands on notes.md.
        d.dir_find_backspace();
        assert_eq!(d.dir_entry_at_cursor().map(|e| e.name.as_str()), Some("notes.md"));
        // A substring that is NOT a prefix does not match: "ead" is inside "readme.txt", but no name
        // STARTS with it → the caret stays put (proves prefix, not substring).
        d.dir_find_clear();
        for c in "ead".chars() {
            d.dir_find_push(c);
        }
        assert_eq!(
            d.dir_entry_at_cursor().map(|e| e.name.as_str()),
            Some("notes.md"),
            "prefix-only: 'ead' must not jump to readme.txt"
        );
    }

    #[test]
    fn find_again_steps_through_prefix_matches_and_wraps() {
        // Two names share the "re" prefix: readme.txt and report.doc.
        let entries = vec![
            DirEntry { name: "..".into(), path: PathBuf::from("/"), is_dir: true, ..DirEntry::default() },
            entry("readme.txt", false),
            entry("notes.md", false),
            entry("report.doc", false),
        ];
        let mut d = Document::new();
        let opts = DirOpts { bare: true, ..DirOpts::default() };
        d.open_listing(PathBuf::from("/d"), entries, opts, None);
        for c in "re".chars() {
            d.dir_find_push(c); // first "re": readme.txt
        }
        assert_eq!(d.dir_entry_at_cursor().map(|e| e.name.as_str()), Some("readme.txt"));
        d.dir_find_again(); // next "re": report.doc
        assert_eq!(d.dir_entry_at_cursor().map(|e| e.name.as_str()), Some("report.doc"));
        d.dir_find_again(); // wraps back to readme.txt
        assert_eq!(d.dir_entry_at_cursor().map(|e| e.name.as_str()), Some("readme.txt"));
    }

    #[test]
    fn non_listing_document_has_no_dir_state() {
        let d = Document::from_str("just text");
        assert!(!d.is_dir_listing());
        assert!(d.dir_entry_at_cursor().is_none());
        assert!(d.dir_state().is_none());
    }

    #[test]
    fn typing_run_undoes_as_one_unit() {
        let mut d = Document::new();
        d.insert("h");
        d.insert("i");
        d.insert("!");
        assert_eq!(d.buffer().text(), "hi!");
        assert!(d.undo());
        assert_eq!(d.buffer().text(), ""); // whole run undone at once
        assert_eq!(d.buffer().cursor(), 0);
        assert!(!d.undo()); // nothing left
    }

    #[test]
    fn multicaret_insert_at_every_caret_is_one_undo_unit() {
        let mut d = Document::from_str("xxxx"); // primary at end (offset 4)
        // Plant secondaries at 0,1,2,3 → carets at 0,1,2,3 and primary at 4.
        d.add_caret(0);
        d.add_caret(1);
        d.add_caret(2);
        d.add_caret(3);
        assert_eq!(d.caret_count(), 5);
        assert!(d.is_multi());
        // One keystroke inserts "-" at every caret, offset-safe.
        d.insert("-");
        assert_eq!(d.buffer().text(), "-x-x-x-x-");
        // The whole fan-out undoes as ONE unit, and the caret set comes back.
        assert!(d.undo());
        assert_eq!(d.buffer().text(), "xxxx");
        assert_eq!(d.caret_count(), 5, "undo restores the caret set");
        // Redo re-applies all N edits together.
        assert!(d.redo());
        assert_eq!(d.buffer().text(), "-x-x-x-x-");
        assert_eq!(d.caret_count(), 5);
        // Collapsing drops the secondaries.
        d.collapse_carets();
        assert_eq!(d.caret_count(), 1);
        assert!(!d.is_multi());
    }

    #[test]
    fn multicaret_typing_run_undoes_as_one_unit() {
        let mut d = Document::from_str("..");  // primary at end (2)
        d.add_caret(0); // carets at 0 and 2
        // Type a RUN of three characters at both carets.
        d.insert("a");
        d.insert("b");
        d.insert("c");
        assert_eq!(d.buffer().text(), "abc..abc");
        // The whole run undoes at once (not character-by-character).
        assert!(d.undo());
        assert_eq!(d.buffer().text(), "..", "the multi-caret typing run undoes as one unit");
        assert!(!d.undo(), "nothing left after the run");
        // Redo brings the whole run back.
        assert!(d.redo());
        assert_eq!(d.buffer().text(), "abc..abc");
    }

    #[test]
    fn multicaret_delete_forward_and_back_are_one_undo_unit() {
        // Delete-forward at every caret removes the char AT each.
        let mut d = Document::from_str("aXbXcX"); // primary at EOF (6)
        d.add_caret(1);
        d.add_caret(3);
        d.add_caret(5);
        assert_eq!(d.caret_count(), 4); // 1,3,5 + primary at 6 (a no-op at EOF)
        d.delete_forward();
        assert_eq!(d.buffer().text(), "abc");
        assert!(d.undo());
        assert_eq!(d.buffer().text(), "aXbXcX");
        assert_eq!(d.caret_count(), 4, "undo restores the caret set");

        // Backspace at every caret removes the char BEFORE each.
        let mut d = Document::from_str("aXbXcX"); // primary at EOF (6)
        d.add_caret(2);
        d.add_caret(4);
        assert_eq!(d.caret_count(), 3); // 2,4 + primary at 6
        d.delete_back();
        assert_eq!(d.buffer().text(), "abc");
        assert!(d.undo());
        assert_eq!(d.buffer().text(), "aXbXcX");
    }

    #[test]
    fn multicaret_caret_coincident_with_primary_inserts_once() {
        // Ctrl+click on the line the primary already sits on can plant a secondary at the primary's
        // exact offset; the fan-out must insert there ONCE, not twice (regression for the double-text
        // bug). Same for delete.
        let mut d = Document::from_str("abc"); // primary at end (3)
        d.add_caret(3); // a secondary coincident with the primary
        assert_eq!(d.caret_count(), 2);
        d.insert("X");
        assert_eq!(d.buffer().text(), "abcX", "coincident carets insert once, not twice");
        assert_eq!(d.caret_count(), 1, "the coincident carets merged");

        let mut d = Document::from_str("abc"); // primary at 3
        d.add_caret(3);
        d.delete_back();
        assert_eq!(d.buffer().text(), "ab", "coincident carets delete once");
    }

    #[test]
    fn multicaret_motion_moves_all_carets_and_merges_on_collision() {
        let mut d = Document::from_str("abcdef"); // primary at end (6)
        d.add_caret(0);
        d.add_caret(2);
        d.add_caret(4);
        assert_eq!(d.caret_count(), 4); // 0,2,4 + primary at 6
        // One Right moves EVERY caret: 0->1, 2->3, 4->5, 6 stays (end of buffer).
        d.move_carets(|bb| bb.move_right(false));
        assert_eq!(d.buffer().caret_positions(), vec![1, 3, 5, 6]);

        // Carets that land on the same offset merge.
        let mut d = Document::from_str("abcdef"); // primary at 6
        d.add_caret(5);
        assert_eq!(d.caret_count(), 2);
        d.move_carets(|bb| bb.move_right(false)); // 5->6, 6->6 -> collide
        assert_eq!(d.caret_count(), 1, "collided carets merge");
    }

    #[test]
    fn motion_breaks_the_coalescing_run() {
        let mut d = Document::from_str("");
        d.insert("ab");
        d.move_left(false); // cursor jump closes the unit
        d.insert("X"); // new unit, inserted between a and b
        assert_eq!(d.buffer().text(), "aXb");
        d.undo(); // removes just "X"
        assert_eq!(d.buffer().text(), "ab");
        d.undo(); // removes "ab"
        assert_eq!(d.buffer().text(), "");
    }

    #[test]
    fn backspace_run_coalesces() {
        let mut d = Document::from_str("abc"); // cursor at 3
        d.delete_back();
        d.delete_back();
        d.delete_back();
        assert_eq!(d.buffer().text(), "");
        d.undo();
        assert_eq!(d.buffer().text(), "abc");
        assert_eq!(d.buffer().cursor(), 3); // caret restored to before the run
    }

    #[test]
    fn typepairs_autocloses_overtypes_wraps_and_backspace_deletes() {
        // Auto-close: typing '(' with no selection inserts the pair, caret between.
        let mut d = Document::from_str("");
        assert!(d.type_pair('('));
        assert_eq!(d.buffer().text(), "()");
        assert_eq!(d.buffer().cursor(), 1); // between the pair
        assert!(d.type_pair('x') == false); // 'x' isn't a pair char → caller inserts it
        d.insert("x");
        assert_eq!(d.buffer().text(), "(x)");
        assert_eq!(d.buffer().cursor(), 2);

        // Over-type: caret right before ')' → typing ')' steps over it (no duplicate).
        assert!(d.type_pair(')'));
        assert_eq!(d.buffer().text(), "(x)");
        assert_eq!(d.buffer().cursor(), 3); // now past the ')'

        // A plain closer with nothing to over-type is NOT handled (caller inserts it).
        let mut d = Document::from_str("");
        assert!(d.type_pair(')') == false);

        // Backspace between an empty pair deletes both.
        let mut d = Document::from_str("");
        d.type_pair('{');
        assert_eq!(d.buffer().text(), "{}");
        assert!(d.try_delete_pair());
        assert_eq!(d.buffer().text(), "");
        // Not between a pair → not handled.
        let mut d = Document::from_str("ab");
        assert!(d.try_delete_pair() == false);

        // Wrap: with a selection, typing an opener wraps it and keeps the inner selected.
        let mut d = Document::from_str("abc");
        d.set_caret(3, Some(0)); // select "abc"
        assert!(d.type_pair('['));
        assert_eq!(d.buffer().text(), "[abc]");
        assert_eq!(d.selection_range(), Some((1, 4))); // inner "abc" still selected

        // Quote auto-close, over-type, and the apostrophe-after-word exception.
        let mut d = Document::from_str("");
        assert!(d.type_pair('"'));
        assert_eq!(d.buffer().text(), "\"\"");
        assert!(d.type_pair('"')); // over-type the closing quote
        assert_eq!(d.buffer().cursor(), 2);
        let mut d = Document::from_str("don"); // caret at 3, after a letter
        assert!(d.type_pair('\'') == false); // apostrophe in prose → not paired
    }

    #[test]
    fn clicking_a_crlf_line_end_then_typing_keeps_crlf_intact() {
        // The GUI click→type path: a click at end of the first line maps to the \n (offset 3, inside
        // the \r\n); set_caret/click_caret snap it to end-of-content (offset 2), and insert can't split
        // the break — so no stray \r/\n bytes ever appear.
        let mut d = Document::from_str("ab\r\ncd");
        d.set_caret(3, None);
        assert_eq!(d.buffer().cursor(), 2);
        d.insert("X");
        assert_eq!(d.buffer().bytes(), b"abX\r\ncd");

        let mut d = Document::from_str("ab\r\ncd");
        d.click_caret(3); // the actual mouse path
        d.insert("Y");
        assert_eq!(d.buffer().bytes(), b"abY\r\ncd");
    }

    #[test]
    fn defining_to_a_crlf_break_then_deleting_leaves_no_stray() {
        // DF (anchor) + a visual down-move whose target is the \n (mid-CRLF) + delete the selection.
        // The edge snaps to end-of-content, so the deleted span is clean and the \r\n stays whole.
        let mut d = Document::from_str("ab\r\ncd");
        d.set_caret(0, None); // DF anchor at start
        d.move_to_offset(3, true); // extend toward the \n → snaps to 2
        assert_eq!(d.buffer().selection_span(), Some((0, 2)));
        d.delete_back(); // delete the selection
        assert_eq!(d.buffer().bytes(), b"\r\ncd"); // no stray \r or \n
    }

    #[test]
    fn render_kind_by_extension_and_override() {
        // No path → plain Text; `.md`/`.markdown` → GFM; `.typ` → Typst; other extensions → Text.
        let mut d = Document::new();
        assert_eq!(d.render_kind(), RenderKind::Text);
        d.set_path(PathBuf::from("notes.typ"));
        assert_eq!(d.render_kind(), RenderKind::Typst);
        d.set_path(PathBuf::from("notes.md"));
        assert_eq!(d.render_kind(), RenderKind::Gfm);
        d.set_path(PathBuf::from("notes.markdown"));
        assert_eq!(d.render_kind(), RenderKind::Gfm);
        // A non-Markdown text extension (and an extensionless file) → plain Text, NOT GFM.
        d.set_path(PathBuf::from("notes.txt"));
        assert_eq!(d.render_kind(), RenderKind::Text);
        d.set_path(PathBuf::from("README"));
        assert_eq!(d.render_kind(), RenderKind::Text);
        // `.html`/`.htm`/`.xhtml` route to the HTML converter, case-insensitively.
        d.set_path(PathBuf::from("page.html"));
        assert_eq!(d.render_kind(), RenderKind::Html);
        d.set_path(PathBuf::from("page.htm"));
        assert_eq!(d.render_kind(), RenderKind::Html);
        d.set_path(PathBuf::from("page.XHTML"));
        assert_eq!(d.render_kind(), RenderKind::Html);
        // An override still wins, and clearing it returns to the extension.
        d.set_render_override(Some(RenderKind::Gfm));
        assert_eq!(d.render_kind(), RenderKind::Gfm);
        d.set_render_override(None);
        assert_eq!(d.render_kind(), RenderKind::Html);
        // A DOCTYPE override wins over the extension; `None` (auto) restores extension-driven.
        d.set_path(PathBuf::from("x.typ"));
        d.set_render_override(Some(RenderKind::Gfm));
        assert_eq!(d.render_kind(), RenderKind::Gfm); // override beats `.typ`
        d.set_render_override(None);
        assert_eq!(d.render_kind(), RenderKind::Typst); // back to extension
    }

    #[test]
    fn backspace_and_delete_remove_a_whole_crlf_break() {
        // Backspace joining lines deletes BOTH bytes of `\r\n` — no stranded `\r` (`ab\rcd`).
        let mut d = Document::from_str("ab\r\ncd");
        d.set_caret(4, None); // start of "cd": a b \r \n | c d
        d.delete_back();
        assert_eq!(d.buffer().text(), "abcd");
        // Delete at a line end also removes BOTH bytes — no bare `\n` left behind.
        let mut d = Document::from_str("ab\r\ncd");
        d.set_caret(2, None); // on the `\r` (end of "ab")
        d.delete_forward();
        assert_eq!(d.buffer().text(), "abcd");
    }

    #[test]
    fn forward_delete_run_coalesces() {
        let mut d = Document::from_str("abc");
        d.move_doc_start(false);
        d.delete_forward();
        d.delete_forward();
        assert_eq!(d.buffer().text(), "c");
        d.undo();
        assert_eq!(d.buffer().text(), "abc");
        assert_eq!(d.buffer().cursor(), 0);
    }

    #[test]
    fn redo_reapplies_and_a_new_edit_clears_redo() {
        let mut d = Document::from_str("");
        d.insert("hello");
        d.undo();
        assert_eq!(d.buffer().text(), "");
        assert!(d.redo());
        assert_eq!(d.buffer().text(), "hello");
        // Undo, then a fresh edit must clear redo.
        d.undo();
        d.insert("new");
        assert!(!d.can_redo());
    }

    #[test]
    fn scoped_search_walks_matches_and_keeps_the_selection() {
        // "aXaXaXaz": 'a' at 0,2,4,6. Scope = [2,6] (covers the 'a's at 2 and 4); the 'a' at 6 is
        // just outside, and the 'a' at 0 is before the scope.
        let mut d = Document::from_str("aXaXaXaz");
        d.set_caret(6, Some(2)); // selection span (2,6), caret on the end edge
        let pat = Pattern::build("a", false, false, false, false).unwrap();

        // Caret on the edge → outside → search from the top of the scope: match at 2..3.
        let scope = d.buffer().selection_span();
        assert!(d.search_select(&pat, true, false, scope, false));
        assert_eq!(d.buffer().cursor(), 3);
        assert_eq!(d.buffer().selection_span(), Some((2, 6)), "selection survives as scope");

        // Caret now inside → continue from the caret: next match at 4..5.
        let scope = d.buffer().selection_span();
        assert!(d.search_select(&pat, true, false, scope, false));
        assert_eq!(d.buffer().cursor(), 5);
        assert_eq!(d.buffer().selection_span(), Some((2, 6)));

        // The 'a' at 6 is outside the scope → not found (the bound holds, no leak past the end).
        let scope = d.buffer().selection_span();
        assert!(!d.search_select(&pat, true, false, scope, false));
        assert_eq!(d.buffer().cursor(), 5, "caret stays put when nothing matches in scope");
    }

    #[test]
    fn extend_search_grows_a_define_to_the_match() {
        // "abc.def" — DF at cursor 0, then SE/X for "." extends the selection 0..4 (through the
        // period), keeping the DF anchor instead of selecting only the ".".
        let mut d = Document::from_str("abc.def");
        d.set_caret(0, None); // from_str leaves the caret at the end; start at the top
        d.define_toggle(); // DF at cursor 0
        let pat = Pattern::build(".", false, false, false, false).unwrap();
        assert!(d.search_select(&pat, true, false, None, true));
        assert_eq!(d.buffer().selection_span(), Some((0, 4)), "anchor kept; span grows to past the .");
        assert_eq!(d.buffer().cursor(), 4, "caret on the leading (far) edge of travel");

        // Backward extend: DF at end, SE/X backward for "." selects from the . to the end.
        let mut d = Document::from_str("abc.def");
        d.set_caret(7, None); // caret at end
        d.define_toggle();
        assert!(d.search_select(&pat, false, false, None, true));
        assert_eq!(d.buffer().selection_span(), Some((3, 7)), "grows back to the . start");
        assert_eq!(d.buffer().cursor(), 3);
    }

    #[test]
    fn scoped_backward_search_walks_up_within_the_selection() {
        // Scope [1,6] over "aXaXaXaz" → 'a's at 2 and 4 inside.
        let mut d = Document::from_str("aXaXaXaz");
        d.set_caret(1, Some(6)); // span (1,6), caret on the start edge
        let pat = Pattern::build("a", false, false, false, false).unwrap();

        // Caret on the start edge → outside → search backward from the bottom: last 'a' at 4.
        let scope = d.buffer().selection_span();
        assert!(d.search_select(&pat, false, false, scope, false));
        assert_eq!(d.buffer().cursor(), 4);
        // Inside → continue up from the caret: 'a' at 2.
        let scope = d.buffer().selection_span();
        assert!(d.search_select(&pat, false, false, scope, false));
        assert_eq!(d.buffer().cursor(), 2);
        // Nothing above 2 within the scope.
        let scope = d.buffer().selection_span();
        assert!(!d.search_select(&pat, false, false, scope, false));
        assert_eq!(d.buffer().selection_span(), Some((1, 6)));
    }

    #[test]
    fn scoped_search_does_not_cycle_at_the_far_boundary() {
        // Scope [2,5] over "aXaXaa" → 'a's at 2 and 4; the match at 4 ends exactly on the boundary 5.
        let mut d = Document::from_str("aXaXaa");
        d.set_caret(5, Some(2)); // fresh coupled selection, caret on the end edge
        let pat = Pattern::build("a", false, false, false, false).unwrap();

        // Fresh caret on the edge → searches the whole scope: 2..3, then 4..5 (lands ON the edge).
        let scope = d.buffer().selection_span();
        assert!(d.search_select(&pat, true, false, scope, false));
        assert_eq!(d.buffer().cursor(), 3);
        let scope = d.buffer().selection_span();
        assert!(d.search_select(&pat, true, false, scope, false));
        assert_eq!(d.buffer().cursor(), 5); // walked to the far edge

        // Now the caret sits on the edge but ROAMED there → exhausted, no restart from the top.
        let scope = d.buffer().selection_span();
        assert!(!d.search_select(&pat, true, false, scope, false));
        assert_eq!(d.buffer().cursor(), 5, "must not cycle back to the first match");
        assert_eq!(d.buffer().selection_span(), Some((2, 5)));
    }

    #[test]
    fn scoped_backward_search_does_not_cycle_at_the_top_boundary() {
        // Scope [0,3] over "aXaX" → 'a's at 0 and 2; the match at 0 starts exactly on the boundary.
        let mut d = Document::from_str("aXaX");
        d.set_caret(0, Some(3)); // fresh coupled selection, caret on the start edge
        let pat = Pattern::build("a", false, false, false, false).unwrap();

        let scope = d.buffer().selection_span();
        assert!(d.search_select(&pat, false, false, scope, false));
        assert_eq!(d.buffer().cursor(), 2); // last 'a' in scope
        let scope = d.buffer().selection_span();
        assert!(d.search_select(&pat, false, false, scope, false));
        assert_eq!(d.buffer().cursor(), 0); // walked up to the top edge

        let scope = d.buffer().selection_span();
        assert!(!d.search_select(&pat, false, false, scope, false));
        assert_eq!(d.buffer().cursor(), 0, "must not cycle back down to the last match");
    }

    #[test]
    fn scoped_replace_only_touches_the_selection() {
        let mut d = Document::from_str("aXaXaXaz");
        d.set_caret(2, Some(6)); // span (2,6): 'a's at 2 and 4; 'a' at 0 and 6 are outside
        let pat = Pattern::build("a", false, false, false, false).unwrap();
        let scope = d.buffer().selection_span();
        let n = d.replace_all(&pat, "Q", false, scope, None, false);
        assert_eq!(n, 2);
        assert_eq!(d.buffer().text(), "aXQXQXaz"); // a@0 and a@6 untouched
        assert_eq!(d.buffer().selection_span(), Some((2, 6)), "scope re-selected over the result");
    }

    #[test]
    fn undo_of_selection_replace_restores_text_and_reselects() {
        let mut d = Document::from_str("hello");
        d.move_doc_start(false);
        d.move_right(true);
        d.move_right(true); // select "he"
        d.insert("XY"); // replace selection
        assert_eq!(d.buffer().text(), "XYllo");
        assert!(d.undo());
        assert_eq!(d.buffer().text(), "hello");
        // The original selection is restored.
        assert_eq!(d.buffer().selection_text(), "he");
    }

    #[test]
    fn cp_at_selection_edge_duplicates_adjacent() {
        let mut d = Document::from_str("ab");
        d.move_doc_start(false);
        d.define_toggle();
        d.move_right(false);
        d.move_right(false); // select "ab", caret at the edge (2)
        d.define_close();
        assert!(d.copy_selection_to_cursor());
        assert_eq!(d.buffer().text(), "abab");
        assert_eq!(d.buffer().selection_text(), "ab"); // original still selected
        d.undo();
        assert_eq!(d.buffer().text(), "ab");
    }

    #[test]
    fn cp_copies_to_a_roamed_caret_under_ps() {
        let mut d = Document::from_str("hello world");
        d.set_persistent(true);
        d.move_doc_start(false);
        d.define_toggle();
        for _ in 0..5 {
            d.move_right(false); // select "hello"
        }
        d.define_close();
        d.move_doc_end(false); // caret roams to 11 (PS keeps the block)
        assert!(d.copy_selection_to_cursor());
        assert_eq!(d.buffer().text(), "hello worldhello");
        assert_eq!(d.buffer().selection_text(), "hello"); // original intact
    }

    #[test]
    fn mv_moves_selection_to_a_roamed_caret_under_ps() {
        let mut d = Document::from_str("hello world");
        d.set_persistent(true);
        d.move_doc_start(false);
        d.define_toggle();
        for _ in 0..6 {
            d.move_right(false); // select "hello "
        }
        d.define_close();
        d.move_doc_end(false); // caret to 11
        assert!(d.move_selection_to_cursor());
        assert_eq!(d.buffer().text(), "worldhello ");
        assert_eq!(d.buffer().selection_text(), "hello "); // moved text selected
    }

    #[test]
    fn mv_undo_twice_restores_the_original_selection_not_a_giant_span() {
        let mut d = Document::from_str("hello world");
        d.move_doc_start(false);
        d.define_toggle();
        for _ in 0..6 {
            d.move_right(false); // select "hello "
        }
        d.define_close();
        d.move_doc_end(false); // caret roams to 11
        assert!(d.move_selection_to_cursor());
        assert_eq!(d.buffer().text(), "worldhello ");
        // Undo the insert, then the delete.
        assert!(d.undo());
        assert!(d.undo());
        assert_eq!(d.buffer().text(), "hello world", "text fully restored");
        // BUG: selects "hello world" (start-of-selection .. roamed caret). Should be just "hello ".
        assert_eq!(d.buffer().selection_text(), "hello ", "original selection restored, not a giant span");
    }

    #[test]
    fn cp_undo_restores_the_original_selection_not_a_giant_span() {
        // Same root cause as MV: undo of a CP whose selection was a roamed DEFINED block must restore
        // the block, not a span from the block start to the drop point.
        let mut d = Document::from_str("hello world");
        d.move_doc_start(false);
        d.define_toggle();
        for _ in 0..5 {
            d.move_right(false); // select "hello"
        }
        d.define_close();
        d.move_doc_end(false); // caret roams to 11; the defined block persists
        assert!(d.copy_selection_to_cursor());
        assert_eq!(d.buffer().text(), "hello worldhello");
        assert!(d.undo());
        assert_eq!(d.buffer().text(), "hello world", "copy undone");
        assert_eq!(d.buffer().selection_text(), "hello", "original selection restored, not a giant span");
    }

    #[test]
    fn defined_block_persists_across_motion_so_cp_works_without_ps() {
        let mut d = Document::from_str("hello world");
        // No SPS.
        d.move_doc_start(false);
        d.define_toggle();
        for _ in 0..5 {
            d.move_right(false); // "hello" (defining extends)
        }
        d.define_close(); // DZ -> defined block
        d.move_doc_end(false); // roam — the defined block persists WITHOUT SPS
        assert_eq!(d.buffer().selection_text(), "hello");
        assert!(d.copy_selection_to_cursor());
        assert_eq!(d.buffer().text(), "hello worldhello");
    }

    #[test]
    fn shift_selection_is_transient_and_collapses_on_plain_motion() {
        let mut d = Document::from_str("hello");
        d.move_doc_start(false);
        d.move_right(true);
        d.move_right(true); // Shift-select "he" (transient, not a defined block)
        assert_eq!(d.buffer().selection_text(), "he");
        d.move_right(false); // a plain motion collapses a transient selection
        assert!(!d.buffer().has_selection());
    }

    #[test]
    fn cp_and_mv_need_a_selection() {
        let mut d = Document::from_str("abc");
        d.move_doc_start(false);
        assert!(!d.copy_selection_to_cursor());
        assert!(!d.move_selection_to_cursor());
        assert_eq!(d.buffer().text(), "abc");
    }

    #[test]
    fn change_case_on_selection_and_undo() {
        let mut d = Document::from_str("hello world");
        d.move_doc_start(false);
        d.define_toggle();
        for _ in 0..5 {
            d.move_right(false); // select "hello"
        }
        d.define_close();
        d.change_case(CaseKind::Upper);
        assert_eq!(d.buffer().text(), "HELLO world");
        assert_eq!(d.buffer().selection_text(), "HELLO"); // stays selected
        d.undo();
        assert_eq!(d.buffer().text(), "hello world");
    }

    #[test]
    fn swap_case_under_cursor_without_selection() {
        let mut d = Document::from_str("aB");
        d.move_doc_start(false); // cursor on 'a'
        d.change_case(CaseKind::Swap);
        assert_eq!(d.buffer().text(), "AB"); // 'a' -> 'A', cursor char only
    }

    #[test]
    fn case_change_without_selection_advances_the_caret() {
        let mut d = Document::from_str("aB3c");
        d.move_doc_start(false); // caret at 0, on 'a'
        d.change_case(CaseKind::Upper);
        assert_eq!(d.buffer().text(), "AB3c");
        assert_eq!(d.buffer().cursor(), 1); // advanced past the changed char
        // On 'B' (already upper): no edit, but the caret still advances (sweep).
        d.change_case(CaseKind::Upper);
        assert_eq!(d.buffer().text(), "AB3c");
        assert_eq!(d.buffer().cursor(), 2);
        // On '3' (no case): still advances.
        d.change_case(CaseKind::Upper);
        assert_eq!(d.buffer().cursor(), 3);
        // On 'c': changes and advances to the end.
        d.change_case(CaseKind::Upper);
        assert_eq!(d.buffer().text(), "AB3C");
        assert_eq!(d.buffer().cursor(), 4);
        // At end of buffer: nothing under the caret, so it stays put.
        d.change_case(CaseKind::Upper);
        assert_eq!(d.buffer().cursor(), 4);
    }

    #[test]
    fn ps_insert_keeps_selection_and_undo_restores_text() {
        let mut d = Document::from_str("hello");
        d.set_persistent(true);
        d.move_doc_start(false);
        d.define_toggle();
        d.move_right(false);
        d.move_right(false); // select "he"
        d.define_close();
        d.move_doc_end(false); // roam off the block (PS keeps it)
        d.insert("X"); // PS: insert at the caret, don't replace the block
        assert_eq!(d.buffer().text(), "helloX");
        assert_eq!(d.buffer().selection_text(), "he"); // block intact
        d.undo(); // text restored byte-exactly
        assert_eq!(d.buffer().text(), "hello");
    }

    #[test]
    fn modified_flag_tracks_edits_save_and_undo() {
        let mut d = Document::new();
        assert!(!d.is_modified()); // fresh doc
        d.insert("hello"); // one coalesced unit
        assert!(d.is_modified());
        d.mark_saved();
        assert!(!d.is_modified());
        d.insert(" world"); // save closed the run -> new unit
        assert!(d.is_modified());
        d.undo(); // back to the saved state
        assert!(!d.is_modified());
        d.redo(); // forward again
        assert!(d.is_modified());
    }

    #[test]
    fn modified_stays_set_when_saved_state_becomes_unreachable() {
        let mut d = Document::new();
        d.insert("a");
        d.move_left(false);
        d.insert("b"); // 2 units
        d.mark_saved(); // saved at len 2
        d.undo(); // len 1 (modified vs saved)
        assert!(d.is_modified());
        d.move_right(false);
        d.insert("c"); // clobbers the redo path back to the saved state
        assert!(d.is_modified()); // permanently modified
    }

    #[test]
    fn load_stores_bytes_verbatim_and_save_is_byte_exact() {
        let mut d = Document::new();
        d.load(b"line1\r\nline2\r\n".to_vec(), std::path::PathBuf::from("x.txt"));
        assert_eq!(d.line_ending(), LineEnding::Crlf);
        // Bytes are verbatim — CR is part of the content, not normalized away.
        assert_eq!(d.buffer().text(), "line1\r\nline2\r\n");
        // The viewport trims the trailing CR per line.
        assert_eq!(d.buffer().line_text(0), "line1");
        assert_eq!(d.buffer().line_text(1), "line2");
        assert_eq!(d.buffer().cursor(), 0); // cursor at top on open
        assert!(!d.is_modified());
        assert_eq!(d.file_name().as_deref(), Some("x.txt"));
        // Save writes the bytes back exactly.
        assert_eq!(d.save_bytes(), b"line1\r\nline2\r\n".to_vec());
    }

    #[test]
    fn load_lf_stays_lf() {
        let mut d = Document::new();
        d.load(b"a\nb\n".to_vec(), std::path::PathBuf::from("y.txt"));
        assert_eq!(d.line_ending(), LineEnding::Lf);
        assert_eq!(d.save_bytes(), b"a\nb\n".to_vec());
    }

    #[test]
    fn append_log_preserves_the_caret_and_truncate_shifts_it_left() {
        let mut d = Document::new();
        d.append_log("aaaa\n"); // caret preserved at 0 (was empty)
        d.append_log("bbbb\n");
        assert_eq!(d.buffer().cursor(), 0, "streamed appends don't drag the caret to the end");
        d.set_caret(5, None); // pin at the start of the second line (a job-output start)
        d.append_log("cccc\n"); // appends at the end; caret stays pinned
        assert_eq!(d.buffer().cursor(), 5);
        // Trimming the front (past the pinned position) shifts the caret and clamps to 0.
        d.truncate_log_front(6);
        assert_eq!(d.buffer().text(), "cccc\n");
        assert_eq!(d.buffer().cursor(), 0, "a trimmed-away pin clamps to the top of the buffer");
    }

    #[test]
    fn compile_listing_is_navigable() {
        // A COMPILE results tab is a read-only, line-indexed hit listing (reuses the GREP machinery):
        // a parsed line is a navigable Match, a plain line is a non-navigable Separator.
        let mut d = Document::new();
        d.open_compile_listing("zig build".into(), std::path::PathBuf::from("."));
        assert!(d.is_grep_listing() && d.read_only());
        d.push_output_line("src/main.zig:10:5: error: boom", Some((std::path::PathBuf::from("src/main.zig"), 10, 5)));
        d.push_output_line("    var x = y;", None); // a context line → separator
        assert_eq!(d.compile_match_count(), 1);
        assert_eq!(d.buffer().text(), "src/main.zig:10:5: error: boom\n    var x = y;\n");
        // Caret on the error line → the hit resolves to its location.
        d.set_caret(0, None);
        let h = d.grep_hit_at_cursor().expect("hit on the error line");
        assert_eq!((h.line, h.col), (10, 5));
        assert_eq!(h.path, std::path::PathBuf::from("src/main.zig"));
        // Caret on the context line → not navigable.
        let line1 = d.buffer().text().find('\n').unwrap() + 1;
        d.set_caret(line1, None);
        assert!(d.grep_hit_at_cursor().is_none());
    }

    #[test]
    fn truncate_log_front_caps_size_on_line_boundary() {
        let mut d = Document::new();
        for i in 0..1000 {
            d.append_log(&format!("line {i}\n"));
        }
        d.truncate_log_front(200); // keep ~the last 200 bytes
        let text = d.buffer().text();
        assert!(text.len() <= 200, "over cap: {}", text.len());
        assert!(text.ends_with("line 999\n")); // newest kept
        assert!(!text.contains("line 0\n")); // oldest dropped
        assert!(text.starts_with("line ")); // cut on a line boundary (no partial first line)
    }

    #[test]
    fn append_console_editable_non_recording_tail_follow() {
        let mut d = Document::new();
        d.append_console("aaa\n"); // empty → the caret (at end) rides the new output
        assert!(!d.read_only(), "console append keeps the buffer editable");
        assert_eq!(d.buffer().cursor(), d.buffer().len(), "tail-follows when the caret was at the end");

        // A caret parked in scrollback is left alone while output streams.
        d.set_caret(1, None);
        d.append_console("bbb\n");
        assert_eq!(d.buffer().cursor(), 1, "a caret above the tail is not yanked");

        // A selection survives a streamed append (no follow, no clear).
        d.set_caret(2, Some(0)); // select 0..2
        let sel = d.selection_range();
        d.append_console("ccc\n");
        assert_eq!(d.selection_range(), sel, "selection preserved across streamed output");

        // Streamed output is NOT recorded: a user edit + undo leaves the output intact.
        let out = d.buffer().text();
        d.set_caret(0, None);
        d.insert("Z");
        d.undo();
        assert_eq!(d.buffer().text(), out, "undo removes the user edit, not the streamed output");
    }

    #[test]
    fn insert_newline_follows_the_documents_line_ending() {
        // A CRLF file: ENTER (insert_newline) must add \r\n, not a bare \n — no mixed endings.
        // (`load` parks the cursor at the top, so the newline lands at offset 0.)
        let mut d = Document::new();
        d.load(b"a\r\nb\r\n".to_vec(), std::path::PathBuf::from("w.txt"));
        d.insert_newline();
        assert_eq!(d.buffer().text(), "\r\na\r\nb\r\n");
        // An LF document inserts a bare \n.
        let mut d = Document::new();
        d.load(b"a\nb\n".to_vec(), std::path::PathBuf::from("y.txt"));
        d.insert_newline();
        assert_eq!(d.buffer().text(), "\na\nb\n");
    }

    #[test]
    fn os_default_newline_matches_platform() {
        let want = if cfg!(windows) { LineEnding::Crlf } else { LineEnding::Lf };
        assert_eq!(LineEnding::os_default(), want);
        assert_eq!(Document::new().line_ending(), want); // new docs adopt it
    }

    #[test]
    fn load_xywrite_bytes_default_to_cp437_and_round_trip() {
        // 0xAE/0xAF present -> CP437 view; markers display as «…»; bytes round-trip.
        let raw = vec![0xAE, b'F', b'C', 0xAF, b'h', b'i', b'\r', b'\n'];
        let mut d = Document::new();
        d.load(raw.clone(), std::path::PathBuf::from("days.26"));
        assert_eq!(d.encoding_name(), "cp437");
        assert_eq!(d.buffer().line_text(0), "«FC»hi");
        assert_eq!(d.save_bytes(), raw); // byte-exact
    }

    #[test]
    fn utf8_arabic_containing_0xaf_is_not_mistaken_for_cp437() {
        // Regression: Arabic "دار" (house) contains 0xAF — "د" (U+062F) = 0xD8 0xAF. The old
        // heuristic sniffed for 0xAE/0xAF BEFORE checking UTF-8 validity, so it forced CP437 and
        // rendered the file as box-drawing mojibake. Valid UTF-8 must win.
        let raw = "دار".as_bytes().to_vec();
        assert!(raw.contains(&0xAF), "sample must carry the 0xAF byte the old sniff tripped on");
        let mut d = Document::new();
        d.load(raw.clone(), std::path::PathBuf::from("house.txt"));
        assert_eq!(d.encoding_name(), "utf-8");
        assert_eq!(d.buffer().line_text(0), "دار"); // decoded as UTF-8, not CP437
        assert_eq!(d.save_bytes(), raw); // still byte-exact
    }

    #[test]
    fn set_encoding_is_live_and_not_an_edit() {
        let mut d = Document::from_str("ab");
        d.insert("c"); // one undo unit; modified
        assert!(d.is_modified());
        d.set_encoding(crate::encoding::cp437()); // view switch only
        assert_eq!(d.encoding_name(), "cp437");
        assert!(d.is_modified()); // switching the view didn't add/clear an edit
        assert!(d.undo()); // undo still targets the insert, not the switch
        assert_eq!(d.buffer().text(), "ab");
        assert!(!d.can_undo());
    }

    #[test]
    fn undo_redo_across_multibyte_and_crlf() {
        // Multi-byte UTF-8 chars (é, ñ — 2 bytes each) and CRLF round-trip undo.
        let mut d = Document::from_str("café\r\nx");
        // Delete forward at start: removes 'c'.
        d.move_doc_start(false);
        d.delete_forward();
        assert_eq!(d.buffer().text(), "afé\r\nx");
        // Insert a high-byte char (representable in the single-byte view).
        d.insert("ñ");
        assert_eq!(d.buffer().text(), "ñafé\r\nx");
        // Undo both, back to original.
        d.undo();
        d.undo();
        assert_eq!(d.buffer().text(), "café\r\nx");
    }

    #[test]
    fn mda_fg_span_survives_delete_then_undo_then_redo() {
        // The reported bug: styling an edit DELETES is torn out irrecoverably. It must now ride the
        // undo record — undo restores both the text and the styling, redo tears it out again.
        let mut d = Document::from_str("hello world");
        d.set_color_span(0, 5, Some((255, 0, 0))); // "hello" red
        assert_eq!(d.color_spans_decoded(), vec![(0, 5, (255, 0, 0))]);
        d.replace_span(0, 6, ""); // delete "hello " — the styled span is gone from the doc
        assert_eq!(d.buffer().text(), "world");
        assert!(d.color_spans_decoded().is_empty(), "delete tore the styling out");
        assert!(d.undo());
        assert_eq!(d.buffer().text(), "hello world");
        assert_eq!(d.color_spans_decoded(), vec![(0, 5, (255, 0, 0))], "undo restored the styling");
        assert!(d.redo());
        assert_eq!(d.buffer().text(), "world");
        assert!(d.color_spans_decoded().is_empty(), "redo tore it out again");
    }

    #[test]
    fn mda_bg_and_attr_spans_also_restore_on_undo() {
        let mut d = Document::from_str("ABCDEFGHIJ");
        d.set_bg_color_span(2, 8, Some((10, 20, 30)));
        d.set_attr_span(2, 8, Some(ATTR_BOLD));
        d.replace_span(0, 10, ""); // wipe the whole line
        assert!(d.bg_color_spans_decoded().is_empty());
        assert!(d.attr_spans_decoded().is_empty());
        assert!(d.undo());
        assert_eq!(d.buffer().text(), "ABCDEFGHIJ");
        assert_eq!(d.bg_color_spans_decoded(), vec![(2, 8, (10, 20, 30))], "bg restored");
        assert_eq!(d.attr_spans_decoded(), vec![(2, 8, ATTR_BOLD)], "attrs restored");
    }

    #[test]
    fn mda_partial_delete_inside_a_span_restores_exactly_on_undo() {
        // Deleting a chunk from the MIDDLE of a styled run shrinks the span; undo must rebuild the
        // original span, not leave a hole.
        let mut d = Document::from_str("ABCDEFGHIJ");
        d.set_color_span(2, 8, Some((7, 7, 7))); // "CDEFGH"
        d.replace_span(4, 6, ""); // delete "EF" from inside the span
        assert_eq!(d.buffer().text(), "ABCDGHIJ");
        assert_eq!(d.color_spans_decoded(), vec![(2, 6, (7, 7, 7))], "span shrank with the edit");
        assert!(d.undo());
        assert_eq!(d.buffer().text(), "ABCDEFGHIJ");
        assert_eq!(d.color_spans_decoded(), vec![(2, 8, (7, 7, 7))], "span rebuilt exactly");
    }

    #[test]
    fn mda_span_rides_a_move_and_undo_restores_the_origin() {
        let mut d = Document::from_str("hello world");
        d.set_color_span(0, 5, Some((1, 2, 3))); // "hello"
        // Define "hello" as a block, roam the caret to EOF, then MV it there.
        d.move_doc_start(false);
        d.define_toggle();
        for _ in 0..5 {
            d.move_right(false);
        }
        d.define_close();
        d.move_doc_end(false);
        assert!(d.move_selection_to_cursor());
        assert_eq!(d.buffer().text(), " worldhello");
        assert_eq!(d.color_spans_decoded(), vec![(6, 11, (1, 2, 3))], "styling rode the move");
        // MV = two units (delete + insert): undo both → origin restored AND styled.
        assert!(d.undo());
        assert!(d.undo());
        assert_eq!(d.buffer().text(), "hello world");
        assert_eq!(d.color_spans_decoded(), vec![(0, 5, (1, 2, 3))], "origin styling restored");
        // Redo both → back at the drop point, styled.
        assert!(d.redo());
        assert!(d.redo());
        assert_eq!(d.buffer().text(), " worldhello");
        assert_eq!(d.color_spans_decoded(), vec![(6, 11, (1, 2, 3))], "redo re-applies at the drop point");
    }

    #[test]
    fn mda_span_is_copied_with_copy_to_cursor() {
        let mut d = Document::from_str("hello world");
        d.set_color_span(0, 5, Some((9, 9, 9)));
        d.move_doc_start(false);
        d.define_toggle();
        for _ in 0..5 {
            d.move_right(false);
        }
        d.define_close();
        d.move_doc_end(false);
        assert!(d.copy_selection_to_cursor());
        assert_eq!(d.buffer().text(), "hello worldhello");
        // Source keeps its style; the copy at the end is styled too.
        assert_eq!(
            d.color_spans_decoded(),
            vec![(0, 5, (9, 9, 9)), (11, 16, (9, 9, 9))],
            "both the source and the copy are styled"
        );
        assert!(d.undo());
        assert_eq!(d.buffer().text(), "hello world");
        assert_eq!(d.color_spans_decoded(), vec![(0, 5, (9, 9, 9))], "copy removed, source intact");
    }
}
