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

use xyscript_xpl::{CaseKind, DirOpts, SortSpec};

/// Process-global source of content generations. Each whole-buffer swap / encoding /
/// highlighter change pulls a fresh value, so a document's [`revision`](Document::revision)
/// is unique across *all* documents and loads — two freshly-opened files never share a
/// revision (which would let a render cache show the wrong file). Starts at 1 so a never-
/// loaded document (gen 0) is still distinct.
static CONTENT_GEN: AtomicU64 = AtomicU64::new(1);

fn next_content_gen() -> u64 {
    CONTENT_GEN.fetch_add(1, Ordering::Relaxed)
}

use std::cell::RefCell;
use std::rc::Rc;

use crate::encoding::{Charset, Encoding};
use crate::highlight::{GrammarRegistry, Highlighter, Token};
use crate::text::{Buffer, Pattern};

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

/// A primitive change: at byte offset `at`, `removed` bytes were deleted and
/// `inserted` bytes were added. Inverse: delete `inserted` at `at`, insert
/// `removed`. Storing raw bytes (not strings) keeps undo byte-exact and is what
/// multi-byte encodings will need later.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Op {
    at: usize,
    removed: Vec<u8>,
    inserted: Vec<u8>,
}

/// The kind of edit, for coalescing decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Insert,
    DeleteBack,
    DeleteForward,
    /// A selection-replacing edit — never coalesces (its own undo unit).
    Other,
}

/// One undo step.
#[derive(Debug, Clone)]
struct Unit {
    op: Op,
    kind: Kind,
    cursor_before: usize,
    anchor_before: Option<usize>,
    cursor_after: usize,
    anchor_after: Option<usize>,
}

/// A text document: a buffer, undo/redo history, and file identity.
#[derive(Debug)]
pub struct Document {
    buffer: Buffer,
    undo: Vec<Unit>,
    redo: Vec<Unit>,
    /// True when the top undo unit is still open for coalescing.
    coalescing: bool,
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
    /// Cached highlight tokens for the current buffer (empty when off). Refreshed
    /// after every text mutation; byte offsets into the UTF-8 source.
    tokens: Vec<Token>,
    /// Transient per-selection foreground colours (`FGCOLOR` on a selection): `(start, end, rgb)`
    /// in ROPE-byte offsets (like `Op.at`, so `record` can track them across edits). Session-only:
    /// never serialized — a template (`|DP;FGCOLOR …|`) is the persistence path. Sorted,
    /// non-overlapping. Converted to decoded offsets for the renderer by `color_spans_decoded`.
    color_spans: Vec<(usize, usize, (u8, u8, u8))>,
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
    /// The nine `C1`..`C9` counters (XyWrite-style). Per-document and session-only (not persisted).
    counters: [crate::counter::Counter; 9],
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
}

impl RenderKind {
    /// The lowercase name used by `DOCTYPE` / `$DOCTYPE`.
    pub fn as_str(self) -> &'static str {
        match self {
            RenderKind::Gfm => "gfm",
            RenderKind::Typst => "typst",
            RenderKind::Html => "html",
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
            saved_len: self.saved_len,
            path: self.path.clone(),
            line_ending: self.line_ending,
            read_only: self.read_only,
            label: self.label.clone(),
            highlighter: self.highlighter.clone(),
            tokens: self.tokens.clone(),
            color_spans: self.color_spans.clone(),
            lints: self.lints.clone(),
            gen: self.gen,
            dir: self.dir.clone(),
            grammars: self.grammars.clone(),
            render_override: self.render_override,
            counters: self.counters.clone(),
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
            saved_len: Some(0), // a fresh document is unmodified
            path: None,
            line_ending: LineEnding::os_default(), // new docs follow the OS until a file is opened
            read_only: false,
            label: None,
            highlighter: None, // quiet screen by default
            tokens: Vec::new(),
            color_spans: Vec::new(),
            lints: Vec::new(),
            gen: 0,
            dir: None,
            grammars: Rc::new(RefCell::new(GrammarRegistry::default())),
            render_override: None, // auto: decide by extension
            counters: std::array::from_fn(|_| crate::counter::Counter::default()),
            id: fresh_doc_id(),
        }
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

    /// Whether the document has unsaved changes (`$MO`).
    pub fn is_modified(&self) -> bool {
        self.saved_len != Some(self.undo.len())
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

    /// Cap a log buffer to roughly its last `max_bytes`, dropping the oldest content from the
    /// front (so a flood like `dir /s C:\` can't grow it without bound). Cuts on a line boundary
    /// when possible, else a UTF-8 char boundary; no-op when already within the cap.
    pub fn truncate_log_front(&mut self, max_bytes: usize) {
        let len = self.buffer.len();
        if len <= max_bytes {
            return;
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
        // Shift the caret left with the dropped front so a pinned position (the start of a job's
        // output) tracks the content; clamp to 0 if that position was itself trimmed away.
        self.buffer.set_caret(saved.saturating_sub(cut).min(self.buffer.len()), None);
        self.undo.clear();
        self.redo.clear();
        self.coalescing = false;
        self.saved_len = Some(0);
        self.gen = next_content_gen(); // force the GUI to re-render the trimmed content
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
    /// extension (`.typ` → Typst, `.html`/`.htm`/`.xhtml` → HTML, everything else → GFM).
    pub fn render_kind(&self) -> RenderKind {
        self.render_override.unwrap_or_else(|| self.render_kind_from_ext())
    }

    /// The extension-driven render kind (ignores any override): `.typ` → Typst, `.html`/`.htm`/
    /// `.xhtml` → Html, else GFM.
    fn render_kind_from_ext(&self) -> RenderKind {
        match self.path.as_deref().and_then(|p| p.extension()).and_then(|e| e.to_str()) {
            Some(e) if e.eq_ignore_ascii_case("typ") => RenderKind::Typst,
            Some(e)
                if e.eq_ignore_ascii_case("html")
                    || e.eq_ignore_ascii_case("htm")
                    || e.eq_ignore_ascii_case("xhtml") =>
            {
                RenderKind::Html
            }
            _ => RenderKind::Gfm,
        }
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

    /// Recompute the highlight cache from the current source (no-op when off).
    fn refresh_highlights(&mut self) {
        match &self.highlighter {
            Some(h) => self.tokens = h.highlight(&self.buffer.text()),
            None => self.tokens.clear(),
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
        if start >= end {
            return;
        }
        let mut out: Vec<(usize, usize, (u8, u8, u8))> = Vec::new();
        for &(ss, se, c) in &self.color_spans {
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
        if let Some(rgb) = color {
            out.push((start, end, rgb));
        }
        out.sort_by_key(|&(s, _, _)| s);
        self.color_spans = out;
        self.gen = self.gen.wrapping_add(1);
    }

    /// Shift the transient colour spans to follow an edit at byte `at` that removed `removed` and
    /// inserted `inserted` bytes (called from `record`, so colours track typing/deletion). Points
    /// inside the removed range collapse to `at`; spans emptied by the edit are dropped.
    fn adjust_color_spans(&mut self, at: usize, removed: usize, inserted: usize) {
        if self.color_spans.is_empty() {
            return;
        }
        // Per-edge "gravity" so text typed AT a boundary isn't swept into the colour: the START
        // has right gravity (an insert at the start pushes it along, excluding the new text), the
        // END has left gravity (an insert at the end stays put). A point inside a deletion
        // collapses to `at`. An insert strictly INSIDE the span grows it.
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
        self.color_spans.retain_mut(|s| {
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
        self.lints.retain_mut(|m| {
            if m.end <= at {
                true // entirely before the edit — unaffected
            } else if m.start >= end_of_edit {
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
    }

    // ---- recorded mutations ------------------------------------------------

    /// Insert text at the cursor, replacing any selection.
    pub fn insert(&mut self, s: &str) {
        if s.is_empty() {
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
        self.record(Op { at, removed, inserted }, kind, cb, ab, ca, aa);
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
        self.buffer.commit_define();
        let keep = self.buffer.is_persistent() && self.buffer.has_selection();
        let replace = !keep && self.buffer.has_selection() && self.buffer.sel_head().is_none();
        let (cb, ab) = (self.buffer.cursor(), self.buffer.anchor());
        if replace {
            let (a, b) = self.buffer.selection_span().unwrap();
            let removed = self.buffer.slice_bytes(a, b);
            self.buffer.delete_back();
            let (ca, aa) = (self.buffer.cursor(), self.buffer.anchor());
            self.record(Op { at: a, removed, inserted: Vec::new() }, Kind::Other, cb, ab, ca, aa);
            return;
        }
        if cb > 0 {
            let removed = self.buffer.slice_bytes(cb - 1, cb);
            self.buffer.delete_back();
            let (ca, aa) = (self.buffer.cursor(), self.buffer.anchor());
            self.record(
                Op { at: cb - 1, removed, inserted: Vec::new() },
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
        self.buffer.commit_define();
        let keep = self.buffer.is_persistent() && self.buffer.has_selection();
        let replace = !keep && self.buffer.has_selection() && self.buffer.sel_head().is_none();
        let (cb, ab) = (self.buffer.cursor(), self.buffer.anchor());
        if replace {
            let (a, b) = self.buffer.selection_span().unwrap();
            let removed = self.buffer.slice_bytes(a, b);
            self.buffer.delete_forward();
            let (ca, aa) = (self.buffer.cursor(), self.buffer.anchor());
            self.record(Op { at: a, removed, inserted: Vec::new() }, Kind::Other, cb, ab, ca, aa);
            return;
        }
        if cb < self.buffer.len() {
            let removed = self.buffer.slice_bytes(cb, cb + 1);
            self.buffer.delete_forward();
            let (ca, aa) = (self.buffer.cursor(), self.buffer.anchor());
            self.record(
                Op { at: cb, removed, inserted: Vec::new() },
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
        self.record(Op { at: a, removed, inserted }, Kind::Other, cb, ab, ca, aa);
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
        let at = self.buffer.cursor();
        let (cb, ab) = (self.buffer.cursor(), self.buffer.anchor());
        let n = self.buffer.insert_keeping_selection(&s);
        let inserted = self.buffer.slice_bytes(at, at + n);
        let (ca, aa) = (self.buffer.cursor(), self.buffer.anchor());
        self.record(Op { at, removed: Vec::new(), inserted }, Kind::Other, cb, ab, ca, aa);
        true
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
        let dest = if c > b { c - (b - a) } else { c };

        // Unit 1: delete the selection from its origin, caret to the destination.
        let (cb1, ab1) = (self.buffer.cursor(), self.buffer.anchor());
        self.buffer.replace_range_bytes(a, b - a, &[]);
        self.buffer.set_caret(dest, None);
        let (ca1, aa1) = (self.buffer.cursor(), self.buffer.anchor());
        self.record(
            Op { at: a, removed, inserted: Vec::new() },
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
        self.record(Op { at: dest, removed: Vec::new(), inserted: ins }, Kind::Other, cb2, ab2, ca2, aa2);
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
            self.record(Op { at: rfs, removed, inserted }, Kind::Other, cb, ab, ca, aa);
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
        self.record(Op { at: rfs, removed, inserted }, Kind::Other, cb, ab, ca, aa);
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
        self.record(Op { at: start, removed, inserted }, Kind::Other, cb, ab, ca, aa);
        new_end
    }

    /// Select the byte range `[start, end)` (rope offsets), caret on the end.
    pub fn select_range(&mut self, start: usize, end: usize) {
        self.buffer.set_caret(end, Some(start));
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
        self.record(Op { at: start, removed, inserted }, Kind::Other, cb, ab, ca, aa);
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

    /// `RS` — delete the whole sentence at the cursor (+ trailing space). Recorded.
    pub fn delete_sentence(&mut self) {
        let (s, e) = self.buffer.sentence_span();
        if e > s {
            self.replace_span(s, e, "");
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
            self.record(Op { at: a, removed, inserted: Vec::new() }, Kind::Other, cb, ab, ca, aa);
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

    // ---- undo / redo -------------------------------------------------------

    /// Undo the most recent unit. Returns `false` if there was nothing to undo.
    pub fn undo(&mut self) -> bool {
        self.coalescing = false;
        if let Some(unit) = self.undo.pop() {
            let remove = unit.op.inserted.len();
            self.buffer.replace_range_bytes(unit.op.at, remove, &unit.op.removed);
            // Colour spans follow the undo edit (it removes `inserted`, restores `removed`).
            self.adjust_color_spans(unit.op.at, remove, unit.op.removed.len());
            self.buffer.set_caret(unit.cursor_before, unit.anchor_before);
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
        if let Some(unit) = self.redo.pop() {
            let remove = unit.op.removed.len();
            self.buffer.replace_range_bytes(unit.op.at, remove, &unit.op.inserted);
            // Colour spans follow the redo edit (removes `removed`, re-inserts `inserted`).
            self.adjust_color_spans(unit.op.at, remove, unit.op.inserted.len());
            self.buffer.set_caret(unit.cursor_after, unit.anchor_after);
            self.undo.push(unit);
            self.refresh_highlights();
            true
        } else {
            false
        }
    }

    // ---- recording / coalescing -------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn record(
        &mut self,
        op: Op,
        kind: Kind,
        cursor_before: usize,
        anchor_before: Option<usize>,
        cursor_after: usize,
        anchor_after: Option<usize>,
    ) {
        // Transient FGCOLOR-on-selection spans follow the edit (typing before a coloured word
        // shifts its colour along, an edit inside grows/shrinks it, a deletion over it clips it).
        self.adjust_color_spans(op.at, op.removed.len(), op.inserted.len());
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

        if self.coalescing && kind != Kind::Other {
            if let Some(last) = self.undo.last_mut() {
                if last.kind == kind && coalesces(last, &op) {
                    merge(last, op, cursor_after, anchor_after);
                    self.refresh_highlights();
                    return;
                }
            }
        }

        self.undo.push(Unit {
            op,
            kind,
            cursor_before,
            anchor_before,
            cursor_after,
            anchor_after,
        });
        self.coalescing = kind != Kind::Other;
        self.refresh_highlights();
    }
}

/// Provisional default encoding view for freshly-loaded bytes (decision ET2:
/// full detection/override is a later step). In priority order:
/// 1. **CP437** if the bytes contain XyWrite function-code delimiters
///    (`0xAE`/`0xAF`) — those render as the guillemets `«…»`.
/// 2. **UTF-8** if the bytes are valid UTF-8 — modern files (and pure ASCII).
/// 3. **Latin-1** otherwise — a lossless fallback for unknown single-byte text.
///
/// The chosen view never affects byte-exactness (load stores bytes verbatim); it
/// only affects display and how new typing encodes. The user can override live
/// with the `ENCODING` verb.
fn default_encoding_for(bytes: &[u8]) -> Charset {
    if bytes.iter().any(|&b| b == 0xAE || b == 0xAF) {
        crate::encoding::cp437()
    } else if std::str::from_utf8(bytes).is_ok() {
        crate::encoding::utf8()
    } else {
        crate::encoding::latin1()
    }
}

/// Whether `op` continues `last`'s coalescing run (same kind already checked).
fn coalesces(last: &Unit, op: &Op) -> bool {
    match last.kind {
        // Typing forward: the new insert begins right after the run's bytes.
        Kind::Insert => op.at == last.op.at + last.op.inserted.len(),
        // Backspace: the new deletion sits immediately before the run's region.
        Kind::DeleteBack => op.at + op.removed.len() == last.op.at,
        // Forward-delete: always removes at the same position.
        Kind::DeleteForward => op.at == last.op.at,
        Kind::Other => false,
    }
}

/// Merge `op` into `last`'s run (same kind, already confirmed contiguous).
fn merge(last: &mut Unit, op: Op, cursor_after: usize, anchor_after: Option<usize>) {
    match last.kind {
        Kind::Insert => last.op.inserted.extend_from_slice(&op.inserted),
        Kind::DeleteForward => last.op.removed.extend_from_slice(&op.removed),
        Kind::DeleteBack => {
            // Prepend (we're consuming bytes leftward) and move the anchor left.
            let mut merged = op.removed;
            merged.extend_from_slice(&last.op.removed);
            last.op.removed = merged;
            last.op.at = op.at;
        }
        Kind::Other => unreachable!("Other never coalesces"),
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
        // No path → GFM; `.typ` → Typst; other extensions → GFM.
        let mut d = Document::new();
        assert_eq!(d.render_kind(), RenderKind::Gfm);
        d.set_path(PathBuf::from("notes.typ"));
        assert_eq!(d.render_kind(), RenderKind::Typst);
        d.set_path(PathBuf::from("notes.md"));
        assert_eq!(d.render_kind(), RenderKind::Gfm);
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
        let pat = Pattern::build("a", false, false, false).unwrap();

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
        let pat = Pattern::build(".", false, false, false).unwrap();
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
        let pat = Pattern::build("a", false, false, false).unwrap();

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
        let pat = Pattern::build("a", false, false, false).unwrap();

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
        let pat = Pattern::build("a", false, false, false).unwrap();

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
        let pat = Pattern::build("a", false, false, false).unwrap();
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
}
