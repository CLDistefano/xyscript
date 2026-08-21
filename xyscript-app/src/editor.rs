//! The closed editing loop (`development-plan.md` Stage 2).
//!
//! [`Editor`] owns the three foundations and runs the loop:
//! **key event → resolve (kbd) → dispatch XPL (interpreter) → mutate the buffer
//! (text engine) → render.** It is fully headless: drive it with synthetic
//! [`KeyEvent`]s and inspect [`Editor::document`] / [`Editor::render`]. The live
//! shell (`main.rs`) is the only platform-specific part.
//!
//! Two editor-level effects are *reconciled* after each script returns (a script
//! can't perform them mid-run): a `LOADKBD` swaps the resolver's operative
//! keymap (transactionally — a bad load leaves the old map in place), and a
//! `SUBMIT` runs the command line's contents.

use xyscript_kbd::event::{KeyEvent, KeyPhase};
use xyscript_kbd::{embedded_default, Disposition, Keymap, Resolver};
use xyscript_xpl::{EditorOp, Host, Interpreter, Value, VmResult};

use crate::host::{AppHost, Target};
use crate::text::Buffer;

/// Wall-clock cap on how long one frame may spend running a preemptively-sliced foreground script
/// before yielding back to the GUI to repaint and process input (notably a Ctrl+Break abort). Kept
/// well under a 60 fps frame (~16 ms) so the editor stays fluid while a heavy/looping script runs.
const FRAME_BUDGET_MS: u64 = 8;

/// The editor: keyboard resolver + interpreter + text-backed host.
/// Map an editor-op result to a status string (Ok) or error text (Err).
fn op_status(r: VmResult<Value>) -> Result<String, String> {
    match r {
        Ok(v) => Ok(v.to_str()),
        Err(e) => Err(e.message().to_string()),
    }
}

/// XyWrite parity: a few "action" commands blank the command line after they run FROM it, regardless
/// of the global `CMBLANK` setting — `AB` (abandon), `CD` (change dir), a bare drive-letter change
/// (`D:`, which the parser rewrites to `CD`), and `ST` (store/save). Tests the LEADING verb of `cmd`.
fn cmd_forces_cmline_blank(cmd: &str) -> bool {
    let head = cmd.trim_start();
    // A bare drive-letter path (`D:`, `X:\dir`) is a drive change.
    let b = head.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return true;
    }
    // First token, minus any attached switch (`AB/V`, `CD/D`), upper-cased.
    let verb = head.split([' ', '\t', '/']).next().unwrap_or("").to_ascii_uppercase();
    matches!(verb.as_str(), "AB" | "CD" | "ST")
}

pub struct Editor {
    resolver: Resolver,
    interp: Interpreter,
    host: AppHost,
    last_error: Option<String>,
    /// Deadline for a script parked on `WAIT <ms>` (a non-freezing timed pause): set
    /// when the pause is first seen, cleared when it elapses. The GUI drives the clock
    /// by calling `poll_wait` each frame.
    sleep_deadline: Option<std::time::Instant>,
    /// The desugared keymap a `LOADKBD` just installed, for the GUI to resync its own
    /// (separate) capture resolver. Drained via `take_reloaded_keymap`.
    reloaded_keymap: Option<Keymap>,
    /// Re-entrancy depth of `settle` (it recurses via deferred runs). A `BEDIT` blind edit left
    /// open is auto-exited only when the OUTERMOST settle finishes (the script truly done).
    settle_depth: usize,
    /// Set by the GUI's Ctrl+Break handler (`request_script_abort`) to abort a runaway/hung
    /// script. Honored at the next preemption slice (`pump_yield`) — checked between slices, so it
    /// fires within a frame even mid-loop. Cleared when a fresh top-level run starts.
    abort_requested: bool,
    /// A cmline-submitted command that SUSPENDED (MSG/INPUT/WAIT/TXT/BK) before it could reconcile
    /// focus — the command whose resting-focus rule `settle` must apply once the script finally
    /// finishes. `submit_cmdline`/`post_run` reconcile inline when the command completes synchronously,
    /// but skip it on suspend (`is_suspended`), which used to leave the transient `target = Document`
    /// flip in place: after the modal was dismissed the caret sat in the text and nav keys moved it,
    /// even though `$TX`/`input_focus` said the command line. `Some(cmd)` = pending; the cmd string is
    /// kept for the force-blank check.
    cmline_submit_pending: Option<String>,
    /// Recursion depth of `XC`/`SUBMIT` re-running the command line. Each re-exec is NATIVE recursion
    /// (`post_run` → dispatch → `post_run`), so a bare `XC` typed on the cmline — whose command IS
    /// `XC` — would re-run itself until the stack overflows and the app crashes. Capped at
    /// [`CMLINE_REEXEC_MAX`]; incremented around each re-exec dispatch, so it returns to 0 as the
    /// recursion unwinds.
    reexec_depth: usize,
}

/// Max `XC`/`SUBMIT` re-exec recursion depth before we abort (guarding a self-running command line
/// from overflowing the stack). Far above any legitimate nesting, small enough to stay well within
/// the stack.
const CMLINE_REEXEC_MAX: usize = 64;
/// The error shown when the [`CMLINE_REEXEC_MAX`] cap trips.
const CMLINE_REEXEC_MSG: &str = "XC/SUBMIT aborted: the command line runs itself (infinite loop)";

impl Editor {
    /// Create an editor with an explicit operative keymap. The keymap's literal
    /// cells are desugared to `insert "..."` (D4) before it goes operative.
    pub fn new(keymap: Keymap) -> Editor {
        let km = crate::kbd_sugar::desugar_keymap(keymap);
        let mut host = AppHost::new();
        host.set_keymap(km.clone()); // keep the host's copy in sync for SAVEKBD
        Editor {
            resolver: Resolver::new(km),
            interp: Interpreter::new(),
            host,
            last_error: None,
            sleep_deadline: None,
            reloaded_keymap: None,
            settle_depth: 0,
            abort_requested: false,
            reexec_depth: 0,
            cmline_submit_pending: None,
        }
    }

    /// The resting-focus rule for a cmline-submitted command (mirrors `submit_cmdline`'s inline logic),
    /// factored out so `settle` can apply it after a command that SUSPENDED finally finishes. Without
    /// it the transient `target = Document` flip lingers — caret in the text, nav moving it — after a
    /// `MSG`/`INPUT`/etc. dismissed, even though `$TX` says the command line.
    fn reconcile_cmline_focus(&mut self, cmd: &str) {
        if !self.host.explicit_focus && self.host.target != Target::CmLine {
            self.host.target = if self.interp.error_flag() || !self.host.doc_focus_ran {
                Target::CmLine
            } else {
                Target::Document
            };
        }
        self.host.input_focus = self.host.target;
        if self.host.cmline_blank() || cmd_forces_cmline_blank(cmd) {
            self.host.clear_command_line();
        }
    }

    /// Create an editor with the self-sufficient embedded default keymap (the
    /// floor that always loads — it can reach `LOADKBD` and `KF`s the OS).
    pub fn with_default_keymap() -> Editor {
        Editor::new(embedded_default())
    }

    // ---- inspection --------------------------------------------------------

    pub fn document(&self) -> &Buffer {
        self.host.document()
    }
    /// The buffer of the document at `idx` (into the tab order) — for rendering a specific pane's
    /// document. `doc(active_index())` == [`document`](Self::document).
    pub fn doc(&self, idx: usize) -> &Buffer {
        self.host.doc(idx)
    }
    /// The active document's content revision (see [`Document::revision`]). Changes on
    /// every byte edit, encoding switch, highlighter toggle, or file load — a render
    /// cache can key on this (with the active index) to know when to rebuild.
    pub fn doc_revision(&self) -> u64 {
        self.host.active_doc().revision()
    }
    pub fn command_line(&self) -> &Buffer {
        self.host.command_line()
    }
    /// Insert text into the active document — the OS text/IME path (GUI2
    /// channel 2: layout/IME-resolved characters, multilingual). Distinct from
    /// the KBD/`feed_key` command path.
    pub fn insert_str(&mut self, s: &str) {
        let _ = self.host.editor_op(EditorOp::TypeText(s.to_string()));
    }

    /// Insert literal text, bypassing `TYPEPAIRS` auto-pairing — the scripted `INSERT` semantics.
    /// Used where a substitution must overwrite verbatim (e.g. Find/Replace), not auto-close or wrap.
    pub fn insert_literal(&mut self, s: &str) {
        let _ = self.host.editor_op(EditorOp::InsertText(s.to_string()));
    }

    /// Overwrite the current selection with `s` (empty = delete it) as one undo unit, IGNORING
    /// persistent-selection mode — so the Find dialog's single Replace always replaces/deletes the
    /// selected match rather than editing at the caret and leaving the match behind. No-op if nothing
    /// is selected.
    pub fn replace_selection(&mut self, s: &str) {
        let _ = self.host.editor_op(EditorOp::ReplaceSelection(s.to_string()));
    }

    // ---- file/editor operations (GUI menus; path-safe, bypassing XPL parsing) ----
    // Each returns the status message on success or the (trappable) error text.

    pub fn open(&mut self, path: &str) -> Result<String, String> {
        self.open_hex(path, false)
    }
    /// Like [`open`](Self::open) but opens the file in the hex view when `hex` — the native
    /// Open dialog's `/HEX` path (a file the user picked via `OPEN/HEX` with no path).
    pub fn open_hex(&mut self, path: &str, hex: bool) -> Result<String, String> {
        op_status(self.host.editor_op(EditorOp::Open { path: path.to_string(), force: false, hex }))
    }
    pub fn save(&mut self) -> Result<String, String> {
        op_status(self.host.editor_op(EditorOp::Save(None)))
    }
    pub fn save_as(&mut self, path: &str) -> Result<String, String> {
        op_status(self.host.editor_op(EditorOp::Save(Some(path.to_string()))))
    }
    pub fn export_pdf(&mut self, path: &str) -> Result<String, String> {
        op_status(self.host.editor_op(EditorOp::ToPdf(Some(path.to_string()))))
    }
    pub fn new_doc(&mut self) -> Result<String, String> {
        op_status(self.host.editor_op(EditorOp::NewDoc))
    }
    pub fn set_highlight(&mut self, on: bool) -> Result<String, String> {
        op_status(self.host.editor_op(EditorOp::SetHighlight(on)))
    }
    /// The active document's path (for "Save" vs "Save As" decisions in the UI).
    pub fn active_path(&self) -> Option<std::path::PathBuf> {
        self.host.active_doc().path().map(|p| p.to_path_buf())
    }

    /// The active document's highlight tokens (empty when highlighting is off).
    pub fn highlights(&self) -> &[crate::highlight::Token] {
        self.host.active_doc().highlights()
    }
    /// The active document's transient FGCOLOR-on-selection colour spans, in DECODED-byte offsets
    /// (aligned with `highlights()`), each `(start, end, rgb)`. Empty when none are set.
    pub fn color_spans(&self) -> Vec<(usize, usize, (u8, u8, u8))> {
        self.host.active_doc().color_spans_decoded()
    }
    /// The active document's per-selection BACKGROUND colour spans (`MD/B`), in DECODED-byte offsets,
    /// each `(start, end, rgb)` — the renderer draws these as rectangles behind the glyphs.
    pub fn bg_color_spans(&self) -> Vec<(usize, usize, (u8, u8, u8))> {
        self.host.active_doc().bg_color_spans_decoded()
    }
    /// The active document's `MDA/B` background spans in RAW rope-byte offsets, `(start, end, rgb)` —
    /// what `highlight_rects` (wrap-aware pixel geometry) expects, mirroring `emph_highlights`.
    pub fn bg_color_spans_raw(&self) -> Vec<(usize, usize, (u8, u8, u8))> {
        self.host.active_doc().color_spans_raw().1.to_vec()
    }
    /// The active document's `MDA` text-attribute spans (`(start, end, bits)`) in DECODED-byte offsets
    /// — the renderer maps the bits to glyph weight/slant (bold/italic).
    pub fn attr_spans(&self) -> Vec<(usize, usize, u8)> {
        self.host.active_doc().attr_spans_decoded()
    }
    /// The active document's `MDA` attribute spans in RAW rope-byte offsets — for the underline pass,
    /// which feeds them through `highlight_rects` (wrap-aware pixel geometry), like the bg layer.
    pub fn attr_spans_raw(&self) -> Vec<(usize, usize, u8)> {
        self.host.active_doc().attr_spans_raw().to_vec()
    }
    /// Whether `MDA` underlines run through inter-word spaces (`ULSPACES`).
    pub fn ul_spaces(&self) -> bool {
        self.host.ul_spaces()
    }
    /// Whether the active document has an `MDA` live-typing mode armed (status-line indicator).
    pub fn mda_typing_active(&self) -> bool {
        self.host.mda_typing_active()
    }
    /// The active document's armed `MDA` typing layers `(fg, bg, attrs)` (status-line indicator).
    pub fn mda_typing(&self) -> (Option<(u8, u8, u8)>, Option<(u8, u8, u8)>, Option<u8>) {
        self.host.mda_typing()
    }
    /// The active highlighter's name, or `None` when off.
    pub fn highlighter_name(&self) -> Option<&str> {
        self.host.active_doc().highlighter_name()
    }
    /// Whether the active document is shown as a hex + text grid (`HEX on`). The GUI reads this to
    /// pick the hex renderer + navigation instead of the text canvas.
    pub fn hex_view(&self) -> bool {
        self.host.active_doc().hex_view()
    }

    /// Whether the active document has any bookmarks (`MARKSET`). The GUI reserves the left gutter
    /// strip only when this is true.
    pub fn has_marks(&self) -> bool {
        !self.host.active_doc().marks().is_empty()
    }

    /// Whether the bookmark gutter dot is enabled (`MARKGUTTER`). The GUI skips the strip when off.
    pub fn mark_gutter(&self) -> bool {
        self.host.mark_gutter()
    }

    /// The 0-based logical line of each bookmark in the active document, for the gutter glyphs.
    pub fn mark_lines(&self) -> Vec<usize> {
        let doc = self.host.active_doc();
        let b = doc.buffer();
        doc.marks().iter().map(|m| b.line_of(m.off)).collect()
    }

    // ---- hex-view editing (Phase 2): the GUI's byte-entry keystrokes route through these ----------
    /// The hex caret as a byte offset, and the document's byte length.
    pub fn hex_caret(&self) -> usize {
        self.host.active_doc().buffer().cursor()
    }
    pub fn hex_len(&self) -> usize {
        self.host.active_doc().buffer().len()
    }
    /// Whether the active document is `RO`-locked (blocks even hex editing).
    pub fn active_read_only(&self) -> bool {
        self.host.active_doc().read_only()
    }
    /// Move the hex caret to an exact byte offset (NO char-boundary snap), clearing any selection.
    pub fn hex_set_caret(&mut self, byte: usize) {
        self.host.hex_set_caret(byte);
    }
    /// The selected bytes (verbatim), for a hex copy — `None` when nothing is selected.
    pub fn hex_selection_bytes(&self) -> Option<Vec<u8>> {
        let b = self.host.active_doc().buffer();
        b.selection_span().map(|(a, e)| b.slice_bytes(a, e))
    }
    /// Set the hi/lo nibble of the byte at the caret (hex-column entry). Undoable.
    pub fn hex_set_nibble(&mut self, hi: bool, val: u8) {
        let _ = self.host.editor_op(EditorOp::HexSetNibble { hi, val });
    }
    /// Overwrite the whole byte at the caret (text-column entry). Undoable.
    pub fn hex_overwrite(&mut self, byte: u8) {
        let _ = self.host.editor_op(EditorOp::HexOverwrite(byte));
    }
    /// Insert bytes at the caret (byte insert). Undoable.
    pub fn hex_insert(&mut self, bytes: Vec<u8>) {
        let _ = self.host.editor_op(EditorOp::HexInsert(bytes));
    }
    /// Delete one byte — at the caret (`before = false`) or before it (`before = true`). Undoable.
    pub fn hex_delete(&mut self, before: bool) {
        let _ = self.host.editor_op(EditorOp::HexDelete { before });
    }
    /// Paste bytes at the caret in overwrite mode. Undoable.
    pub fn hex_paste(&mut self, bytes: Vec<u8>) {
        let _ = self.host.editor_op(EditorOp::HexPaste(bytes));
    }
    /// Delete the selected byte range (hex cut). Undoable.
    pub fn hex_delete_selection(&mut self) {
        let _ = self.host.editor_op(EditorOp::HexDeleteSelection);
    }
    pub fn cmline_open(&self) -> bool {
        self.host.target == Target::CmLine
    }

    /// Open the command line: subsequent typed text / motion is directed there
    /// (the host's `active()` follows `target`), not to the document.
    pub fn open_cmdline(&mut self) {
        let _ = self.host.editor_op(EditorOp::OpenCmdLine);
    }

    /// Move the command line's caret to rope offset `off` (a GUI mouse click into it), clearing
    /// any selection.
    pub fn click_cmline(&mut self, off: usize) {
        self.host.set_cmline_caret(off, None);
    }
    /// Select command-line text from `anchor` to `cursor` (a GUI mouse drag). The selection feeds
    /// `CUT`/`COPY` while the cmline is focused (the host's `active()` follows focus).
    pub fn select_cmline(&mut self, cursor: usize, anchor: usize) {
        self.host.set_cmline_caret(cursor, Some(anchor));
    }
    /// Select the word / whole line at the cmline caret, directly on the cmline (the `DW`/`DL`
    /// verbs would define in the document — they're focus-doc verbs). The cmline's right-click menu.
    pub fn cmline_define_word(&mut self) {
        self.host.cmline_define_word();
    }
    pub fn cmline_define_line(&mut self) {
        self.host.cmline_define_line();
    }

    /// Drain the keymap a `LOADKBD` just installed (desugared, operative). The GUI calls this
    /// to rebuild its own capture resolver — without it, `LOADKBD` would change only the
    /// editor's internal resolver, not the one the GUI intercepts keystrokes with.
    pub fn take_reloaded_keymap(&mut self) -> Option<Keymap> {
        self.reloaded_keymap.take()
    }

    /// Dismiss the command line without running it; focus returns to the document.
    pub fn cancel_cmdline(&mut self) {
        let _ = self.host.editor_op(EditorOp::CancelCmdLine);
    }

    /// `TH` — toggle focus between the cmline and the text window, preserving each side's
    /// content + cursor (the GUI's "Focus command line" toggle).
    pub fn toggle_focus(&mut self) {
        let _ = self.host.editor_op(EditorOp::ToggleFocus);
    }

    /// `GT` — focus the text window, preserving the cmline contents (e.g. when the GUI
    /// clicks into the text canvas).
    pub fn focus_text(&mut self) {
        let _ = self.host.editor_op(EditorOp::FocusText);
    }

    /// Run the command line's contents, then return focus to the document — unless
    /// the command itself re-opens the command line or switches the active buffer,
    /// in which case focus follows that (it lands in whatever document is now
    /// active). Mirrors `post_run`'s submit branch, since we invoke it directly
    /// rather than via a key→dispatch cycle.
    pub fn submit_cmdline(&mut self) {
        let cmd = self.host.command_line().text();
        self.host.push_history(&cmd); // record for Up/Down recall + the HISTORY picker
        // Run against the document, but KEEP the command in the cmline (XyWrite-style: the
        // command line retains its command so you can return to it — via TH/click — and
        // re-run or edit it; `BC` blanks it explicitly). The command's own ops decide the
        // final focus (e.g. `BC` re-targets the cmline; `DFA` leaves focus on the document).
        // Only `target` flips here (transient — so the command operates on the document);
        // `input_focus` stays the cmline so `$TX` reads 0 *during* a command typed in the cmline.
        // After it settles, resync `input_focus` to the final `target` (Document normally, or the
        // cmline if a command re-focused it, e.g. `BC`).
        self.host.doc_focus_ran = false;
        self.host.explicit_focus = false;
        self.host.target = Target::Document;
        self.dispatch(&cmd);
        // Resting focus (per-verb rule): if the command re-focused the cmline itself (e.g. `BC`),
        // respect it. Otherwise land the caret in the text only for a SUCCESSFUL document-focusing
        // command (an edit/search/file-or-screen motion/file op); an exempt or neutral command, or a
        // failure ($ERROR), returns to the cmline. An EXPLICIT focus verb (`GT`/`TH`) is authoritative
        // and skips the heuristic entirely.
        if !self.interp.is_suspended() {
            self.reconcile_cmline_focus(&cmd);
        } else {
            // Parked (MSG/INPUT/WAIT/TXT/BK): defer the reconcile until the script finishes, or the
            // transient `target = Document` flip is left in place (caret in text, nav moving it) even
            // though $TX/input_focus say the cmline — `settle` applies it on completion.
            self.cmline_submit_pending = Some(cmd);
        }
    }
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Active document's file name (`None` if untitled).
    pub fn active_file_name(&self) -> Option<String> {
        self.host.active_file_name()
    }
    /// Whether the active document has unsaved changes.
    pub fn modified(&self) -> bool {
        self.host.is_active_modified()
    }
    /// Number of open buffers and the active 0-based index.
    pub fn buffer_count(&self) -> usize {
        self.host.buffer_count()
    }
    pub fn active_index(&self) -> usize {
        self.host.active_index()
    }
    /// The working directory as a display string (for the GUI status line).
    pub fn cwd(&self) -> String {
        self.host.cwd_display()
    }
    /// Command-history recall (Up/Down on the CMline) + the `HISTORY` picker.
    pub fn history_prev(&mut self) {
        self.host.history_prev();
    }
    pub fn history_next(&mut self) {
        self.host.history_next();
    }
    pub fn command_history(&self) -> &[String] {
        self.host.command_history()
    }
    /// Filtered `(index, command)` view for the picker (number → last N, text → contains).
    pub fn history_view(&self, filter: &str) -> Vec<(usize, String)> {
        self.host.history_view(filter)
    }
    pub fn delete_history(&mut self, i: usize) {
        self.host.delete_history(i);
    }
    /// Move `cmd` to the front of the history (dedup) — the HISTORY picker uses this so running an
    /// entry with Enter promotes it to the top, exactly like re-running it from the command line.
    pub fn push_history(&mut self, cmd: &str) {
        self.host.push_history(cmd);
    }
    /// Load `cmd` into the command line (focused, caret at end) WITHOUT running it — the HISTORY
    /// picker's Shift+Enter (pull a past command back to edit or inspect).
    pub fn load_cmline(&mut self, cmd: &str) {
        self.host.set_cmline_text(cmd);
    }
    /// The pending `HISTORY`/`HI` picker request + its filter arg, if any; consumes it.
    pub fn take_history_request(&mut self) -> Option<String> {
        self.host.take_history_request()
    }
    /// A pending `MENU` request since the last poll (`Some(None)` = toggle, `Some(Some("f"))` =
    /// open the top menu with accelerator `f`). The GUI drains + applies it.
    pub fn take_menu_kbd_request(&mut self) -> Option<Option<String>> {
        self.host.take_menu_kbd_request()
    }
    /// Per-buffer info for the GUI tab strip (buffer order; active is `active_index()`).
    pub fn tabs(&self) -> Vec<crate::host::TabInfo> {
        self.host.tabs()
    }
    /// Make buffer `i` active (clicking a tab).
    pub fn set_active_buffer(&mut self, i: usize) {
        self.host.set_active(i);
    }
    /// The active document's stable id — lets the GUI remember scroll position per document.
    pub fn active_doc_id(&self) -> crate::panes::DocId {
        self.host.active_doc().id()
    }
    /// Whether the `*Shell Output*` tab should be shown in the tab strip (`SW`/`$SW`).
    pub fn shell_output_visible(&self) -> bool {
        self.host.shell_output_visible()
    }
    /// Whether the tab strip shows when only one file is open (`SHOWONETAB`/`$SHOWONETAB`).
    pub fn show_one_tab(&self) -> bool {
        self.host.show_one_tab()
    }
    /// Whether the live preview pane is shown (`PREVIEW`/`PV`).
    pub fn preview_visible(&self) -> bool {
        self.host.preview_visible()
    }
    /// The active document's effective render kind — the GUI preview routes its rasterizer on this.
    pub fn active_render_kind(&self) -> crate::document::RenderKind {
        self.host.active_render_kind()
    }
    /// Whether the active document renders as authored Typst (the Save-As format default).
    pub fn active_render_is_typst(&self) -> bool {
        self.host.active_render_is_typst()
    }
    /// Write a fresh Console prompt (the GUI calls this when opening/focusing the Console tab).
    pub fn console_write_prompt(&mut self) {
        self.host.console_write_prompt();
    }
    /// Submit the current Console input line (Enter at the tail). Returns `false` when it's not a
    /// submit (no prompt showing, or the caret is up in scrollback) so the GUI inserts a newline.
    pub fn console_submit(&mut self) -> bool {
        self.host.console_submit()
    }
    /// `true` (once) when Console output or a new prompt just arrived — the GUI scrolls the Console to
    /// the tail. Only meaningful together with [`console_is_active`](Self::console_is_active).
    pub fn take_console_dirty(&mut self) -> bool {
        self.host.take_console_dirty()
    }
    /// Whether the Console tab is the active document.
    pub fn console_is_active(&self) -> bool {
        self.host.console_is_active()
    }
    /// Whether a foreground Console command with interactive stdin is running (the GUI gates its
    /// Ctrl+Z/Ctrl+D EOF interception on this).
    pub fn console_job_running(&self) -> bool {
        self.host.console_job_running()
    }
    /// Ctrl+Z / Ctrl+D in the running Console: close the child's stdin (EOF). Returns whether it did.
    pub fn console_stdin_eof(&mut self) -> bool {
        self.host.console_stdin_eof()
    }
    /// Whether a foreground Console command is running (the GUI gates Ctrl+C interrupt on this).
    pub fn console_command_running(&self) -> bool {
        self.host.console_command_running()
    }
    /// Ctrl+C in the running Console: interrupt (kill) the foreground command. Returns whether it did.
    pub fn console_interrupt(&mut self) -> bool {
        self.host.console_interrupt()
    }
    /// Shift+Up (`older`=true) / Shift+Down: recall the session Console history into the prompt input.
    /// Returns whether it handled the key (else the GUI does the normal Shift+arrow selection).
    pub fn console_history_recall(&mut self, older: bool) -> bool {
        self.host.console_history_recall(older)
    }
    /// Autosave interval in seconds (0 = off); the GUI reads it each frame to drive the timer.
    pub fn autosave_secs(&self) -> u64 {
        self.host.autosave_secs()
    }
    /// Tab width in spaces (`TB`/`$TB`); the GUI applies it as the cosmic tab stop.
    /// The `LANG` translation for UI string `id` (or `None` → fall back to English). See
    /// [`AppHost::lang_get`].
    pub fn lang_get(&self, id: &str) -> Option<&str> {
        self.host.lang_get(id)
    }
    /// Path of the loaded `LANG` catalog, or `None` when English. See [`AppHost::lang_path`].
    pub fn lang_path(&self) -> Option<&str> {
        self.host.lang_path()
    }
    pub fn tab_width(&self) -> usize {
        self.host.tab_width()
    }
    /// The bracket match at the active caret, for the GUI to highlight (`BALPAIRS`). Returns
    /// [`BalMatch::None`] when bracket matching is off.
    pub fn bracket_match(&self) -> crate::host::BalMatch {
        self.host.bracket_match()
    }
    /// `BALPAIRSCOLOR` — the matched-bracket highlight colour, or `None` to follow the selection colour.
    pub fn bal_pairs_color(&self) -> Option<(u8, u8, u8)> {
        self.host.bal_pairs_color()
    }
    /// `WRAPINDENT` — whether soft-wrapped continuation rows hang-indent under the line's indentation.
    pub fn wrap_indent(&self) -> bool {
        self.host.wrap_indent()
    }
    /// `WRAP` — whether long lines word-wrap to the window (off → lines run past the edge; the GUI
    /// then shows a horizontal scrollbar).
    pub fn wrap(&self) -> bool {
        self.host.wrap()
    }
    /// `WRAPHANG` — hanging indent (spaces) for wrapped continuation rows (WRAP on, WRAPINDENT off).
    pub fn wrap_hang(&self) -> usize {
        self.host.wrap_hang()
    }
    /// Drain pending `MU`/`MD` viewport scroll (visual rows; + toward end / − toward start).
    /// The GUI applies it to its scroll offset each frame.
    pub fn take_scroll_rows(&mut self) -> i32 {
        self.host.take_scroll_rows()
    }
    /// Drain a pending `LB`/`LE` visual-line-edge refine `(end, extend, from)`. The GUI fulfils it
    /// via cosmic each frame; headless callers ignore it (the engine's logical-edge fallback stands).
    pub fn take_visual_edge(&mut self) -> Option<(bool, bool, usize)> {
        self.host.take_visual_edge()
    }
    /// Drain a pending `CARETMOTION visual` Left/Right refine `(right, extend, from)` — the GUI fulfils
    /// it via cosmic; headless callers ignore it (the engine's logical move stands). See `caret_visual`.
    pub fn take_visual_move(&mut self) -> Option<(bool, bool, usize)> {
        self.host.take_visual_move()
    }
    /// Whether `CARETMOTION` is `visual`.
    pub fn caret_visual(&self) -> bool {
        self.host.caret_visual()
    }
    /// `AUTOCFG`: rewrite the tracked config file if settings changed (no-op when tracking is off).
    /// Front-ends call this each frame (GUI) / after each command (TUI).
    pub fn poll_config_track(&mut self) {
        self.host.poll_config_track();
    }
    /// Run one autosave pass (save all named+modified buffers); returns how many were written.
    pub fn autosave_now(&mut self) -> Result<usize, String> {
        self.host.autosave()
    }
    /// Number of background jobs (`X/BG`) still running (the GUI polls while > 0).
    pub fn job_count(&self) -> usize {
        self.host.job_count()
    }
    /// Drain completed background jobs (sets `$RC`/`$XO`); returns a status notice per finish.
    /// Also delivers any finished `/R:<var>` job's output into its parent variable (the host
    /// queues these; only the editor can reach the interpreter's `Env`).
    pub fn poll_jobs(&mut self) -> Vec<String> {
        let notices = self.host.poll_jobs();
        for (var, output) in self.host.take_pending_returns() {
            self.interp.set_var(&var, Value::str(output));
        }
        notices
    }
    /// Drain the streaming `GREP` worker (append hits to the results tab). Returns `(changed, status)`:
    /// `changed` → the results buffer grew (repaint); `status` → a status-line message while a search
    /// is active or just finished. See [`crate::host::AppHost::poll_grep`].
    pub fn poll_grep(&mut self) -> (bool, Option<String>) {
        self.host.poll_grep()
    }
    /// Whether a streaming `GREP` search is currently running (the GUI keeps polling while true).
    pub fn grep_in_progress(&self) -> bool {
        self.host.grep_in_progress()
    }
    /// Output (`PR`/`LISTLIB`) from the most recent dispatch (the GUI shows it on the status line).
    pub fn output(&self) -> &[String] {
        self.interp.output()
    }

    /// A pending `MSG` message box `(text, timeout_secs)` for the GUI to show; consumes it.
    /// `timeout_secs = Some(n)` self-dismisses after `n` seconds.
    pub fn take_pending_message(&mut self) -> Option<(String, Option<u64>)> {
        self.host.take_pending_message()
    }

    /// Transient status feedback from the most recent dispatch (op result like
    /// "opened X", or a trappable error), if any.
    pub fn status_message(&self) -> Option<&str> {
        self.interp.status_message()
    }
    /// Mutable access to the host (e.g. tests populating the virtual filesystem).
    pub fn host_mut(&mut self) -> &mut AppHost {
        &mut self.host
    }
    /// Read-only access to the host (e.g. the GUI reading `DIR/W` window state to render).
    pub fn host(&self) -> &AppHost {
        &self.host
    }

    /// GUI view flags (`SMB`/`SVR`/`SLN` families; read each frame by the GUI).
    pub fn menu_bar_shown(&self) -> bool {
        self.host.menu_bar()
    }
    pub fn visible_returns(&self) -> bool {
        self.host.visible_returns()
    }
    pub fn visible_whitespace(&self) -> bool {
        self.host.visible_whitespace()
    }
    pub fn line_numbers(&self) -> bool {
        self.host.line_numbers()
    }
    pub fn gui_cmline(&self) -> bool {
        self.host.gui_cmline()
    }
    /// Whether the active document is locked against edits (`RO`); read each frame by the GUI.
    pub fn read_only(&self) -> bool {
        self.host.active_doc().read_only()
    }
    /// Editor colours as RGB triples (read each frame by the GUI). See `EditorOp::SetColor`.
    pub fn fg_color(&self) -> (u8, u8, u8) {
        self.host.fg_color()
    }
    pub fn bg_color(&self) -> (u8, u8, u8) {
        self.host.bg_color()
    }
    /// UI-chrome colours as RGB triples (read each frame by the GUI). See `EditorOp::SetColor`.
    pub fn ui_fg_color(&self) -> (u8, u8, u8) {
        self.host.ui_fg_color()
    }
    pub fn ui_bg_color(&self) -> (u8, u8, u8) {
        self.host.ui_bg_color()
    }
    /// Caret appearance (read each frame by the GUI). See `EditorOp::SetColor`/`SetCaretShape`.
    pub fn caret_color(&self) -> (u8, u8, u8) {
        self.host.caret_color()
    }
    pub fn cmline_caret_color(&self) -> (u8, u8, u8) {
        self.host.cmline_caret_color()
    }
    /// UI separator colour as an RGB triple (read each frame by the GUI). See `EditorOp::SetColor`.
    pub fn sep_color(&self) -> (u8, u8, u8) {
        self.host.sep_color()
    }
    /// Selection background (`SELBGCOLOR`) — the band behind selected text on the canvas.
    pub fn sel_bg_color(&self) -> (u8, u8, u8) {
        self.host.sel_bg_color()
    }
    /// Selected-text foreground (`SELFGCOLOR`) — the glyph colour of text inside the selection.
    pub fn sel_fg_color(&self) -> (u8, u8, u8) {
        self.host.sel_fg_color()
    }
    /// FIND-highlight background (`FINDBGCOLOR`) — the band behind non-current matches.
    pub fn find_bg_color(&self) -> (u8, u8, u8) {
        self.host.find_bg_color()
    }
    /// FIND-match foreground (`FINDFGCOLOR`) — the glyph colour of highlighted match text.
    pub fn find_fg_color(&self) -> (u8, u8, u8) {
        self.host.find_fg_color()
    }
    /// Effective `EMPH` emphasis glyph colour (`EMPHFGCOLOR`, or `SELFGCOLOR` when unset).
    pub fn emph_fg_color(&self) -> (u8, u8, u8) {
        self.host.emph_fg_color()
    }
    /// Effective `EMPH` emphasis background colour (`EMPHBGCOLOR`, or `SELBGCOLOR` when unset).
    pub fn emph_bg_color(&self) -> (u8, u8, u8) {
        self.host.emph_bg_color()
    }
    /// The `EMPH` emphasis colours as SET (`None` = follow selection) — for the colour-picker seed.
    pub fn emph_fg_override(&self) -> Option<(u8, u8, u8)> {
        self.host.emph_fg_override()
    }
    pub fn emph_bg_override(&self) -> Option<(u8, u8, u8)> {
        self.host.emph_bg_override()
    }
    /// The active document's `EMPH` phrase (`None` = none) — the GUI keys its highlight cache on this.
    pub fn active_emph(&self) -> Option<String> {
        self.host.active_doc().emph().map(|s| s.to_string())
    }
    /// Whether the active document's `EMPH` phrase is a regular expression (`EMPH/R`) — part of the
    /// GUI's highlight cache key, so toggling `/R` on the same phrase re-searches.
    pub fn active_emph_is_regex(&self) -> bool {
        self.host.active_doc().emph_is_regex()
    }
    /// Rope-offset spans of every occurrence of the active document's `EMPH` phrase (case-insensitive).
    pub fn emph_highlights(&self) -> Vec<(usize, usize)> {
        self.host.emph_highlights()
    }
    /// Spelling-issue underline colour (`SPELLCOLOR`).
    pub fn spell_color(&self) -> (u8, u8, u8) {
        self.host.spell_color()
    }
    /// Grammar/other-issue underline colour (`GRAMMARCOLOR`).
    pub fn grammar_color(&self) -> (u8, u8, u8) {
        self.host.grammar_color()
    }
    /// Spelling / grammar underline STYLE (`SPELLSTYLE`/`GRAMMARSTYLE`) — dotted/solid/squiggle.
    pub fn spell_style(&self) -> crate::host::UnderlineStyle {
        self.host.spell_style()
    }
    pub fn grammar_style(&self) -> crate::host::UnderlineStyle {
        self.host.grammar_style()
    }
    /// Active split-pane border colour (`PANECOLOR`); inactive panes use `sep_color`.
    pub fn pane_border_color(&self) -> (u8, u8, u8) {
        self.host.pane_border_color()
    }
    /// Tab-title background colours per state (`TABBGCOLOR`); `None` = egui's theme colour.
    pub fn tab_active_bg(&self) -> Option<(u8, u8, u8)> {
        self.host.tab_active_bg()
    }
    pub fn tab_hover_bg(&self) -> Option<(u8, u8, u8)> {
        self.host.tab_hover_bg()
    }
    pub fn tab_inactive_bg(&self) -> Option<(u8, u8, u8)> {
        self.host.tab_inactive_bg()
    }
    pub fn tab_x_color(&self) -> Option<(u8, u8, u8)> {
        self.host.tab_x_color()
    }
    pub fn caret_shape(&self) -> crate::host::CaretShape {
        self.host.caret_shape()
    }
    pub fn cmline_caret_shape(&self) -> crate::host::CaretShape {
        self.host.cmline_caret_shape()
    }

    /// GUI sizing knobs (B16; read each frame by the GUI). See `EditorOp::SetUiSize`.
    pub fn ui_scale(&self) -> f32 {
        self.host.ui_scale()
    }
    pub fn chrome_font(&self) -> f32 {
        self.host.chrome_font()
    }
    pub fn editor_font(&self) -> f32 {
        self.host.editor_font()
    }
    /// Editor canvas font family (`EDFAMILY`), or `None` for the GUI's resolved default.
    pub fn editor_font_family(&self) -> Option<&str> {
        self.host.editor_font_family()
    }
    /// Markdown Preview/`TOPDF` body family (`PVFAMILY`), or `None` for Typst's default.
    pub fn preview_font_family(&self) -> Option<&str> {
        self.host.preview_font_family()
    }
    /// Markdown Preview/`TOPDF` body size in points (`PVSIZE`), or `None` for Typst's default.
    pub fn preview_font_size(&self) -> Option<f32> {
        self.host.preview_font_size()
    }
    /// Markdown Preview/`TOPDF` code (monospace) family (`PVCODE`), or `None` for the default.
    pub fn preview_code_family(&self) -> Option<&str> {
        self.host.preview_code_family()
    }
    /// Active syntax-highlight colour for a token kind, or `None` to draw it in `fg_color`
    /// (`HLCOLOR`/`HLRESET`). The GUI rasters tokens with this.
    pub fn highlight_color(&self, kind: &crate::highlight::TokenKind) -> Option<(u8, u8, u8)> {
        self.host.highlight_color(kind)
    }
    /// The themeable syntax-highlight token kinds (built-ins + any grammar-defined), lowercase names —
    /// for `HLKINDS` and the Settings ▸ Syntax highlighting ▸ Theme colours menu.
    pub fn highlight_kinds(&self) -> Vec<String> {
        self.host.highlight_kinds()
    }
    /// UI-chrome font family (`UIFAMILY`), or `None` for egui's built-in default.
    pub fn ui_font_family(&self) -> Option<&str> {
        self.host.ui_font_family()
    }
    /// `TXT` window font family (`TXTFAMILY`), or `None` for egui's built-in default monospace.
    pub fn txt_font_family(&self) -> Option<&str> {
        self.host.txt_font_family()
    }
    /// Editor canvas / UI chrome / `TXT` window font styles (`EDSTYLE`/`UISTYLE`/`TXTSTYLE`).
    pub fn editor_font_style(&self) -> crate::host::FontStyle {
        self.host.editor_font_style()
    }
    pub fn ui_font_style(&self) -> crate::host::FontStyle {
        self.host.ui_font_style()
    }
    pub fn txt_font_style(&self) -> crate::host::FontStyle {
        self.host.txt_font_style()
    }
    /// `TXT` dialog text size (points) / zoom factor (read by the GUI's TXT window).
    pub fn txt_font(&self) -> f32 {
        self.host.txt_font()
    }
    pub fn txt_scale(&self) -> f32 {
        self.host.txt_scale()
    }
    /// `TXT` dialog text/background colours (read by the GUI's TXT window).
    pub fn txt_fg_color(&self) -> (u8, u8, u8) {
        self.host.txt_fg_color()
    }
    pub fn txt_bg_color(&self) -> (u8, u8, u8) {
        self.host.txt_bg_color()
    }
    pub fn leading(&self) -> f32 {
        self.host.leading()
    }
    pub fn margin_left(&self) -> f32 {
        self.host.margin_left()
    }
    pub fn margin_right(&self) -> f32 {
        self.host.margin_right()
    }

    /// The `(title, text)` of a parked `TXT` modal dialog (`title` `None` = default), if a
    /// script is paused on one. The GUI shows the scrollable dialog while this is `Some`.
    pub fn text_request(&self) -> Option<(Option<String>, String)> {
        self.interp.text_request()
    }

    /// The optional two-column `(name, description)` grid for the parked TXT dialog
    /// (currently only `LISTLIB`). The GUI renders this as a wrapping grid in place of
    /// the flat [`text_request`](Self::text_request) lines.
    pub fn columns_request(&self) -> Option<Vec<(String, String)>> {
        self.interp.columns_request()
    }

    /// Optional per-line label colours for the parked TXT dialog (currently only `HLKINDS`), aligned
    /// with the flat [`text_request`](Self::text_request) lines. `None` = the line's default colour.
    pub fn line_colors_request(&self) -> Option<Vec<Option<(u8, u8, u8)>>> {
        self.interp.line_colors_request()
    }

    /// Dismiss the `TXT` modal dialog (Esc / ×), resuming the paused script.
    pub fn dismiss_text(&mut self) {
        if self.interp.text_request().is_none() {
            return;
        }
        self.last_error =
            self.interp.resume_wait(&mut self.host).err().map(|e| e.message().to_string());
        self.settle();
    }

    /// The text of a parked bare-`MSG` modal box, if a script is paused on one. The GUI shows the
    /// box while this is `Some` and resumes via [`dismiss_message`](Self::dismiss_message).
    pub fn message_request(&self) -> Option<String> {
        self.interp.message_request()
    }

    /// Dismiss the bare-`MSG` modal box (OK / Esc / ×), resuming the paused script.
    pub fn dismiss_message(&mut self) {
        if self.interp.message_request().is_none() {
            return;
        }
        self.last_error =
            self.interp.resume_wait(&mut self.host).err().map(|e| e.message().to_string());
        self.settle();
    }

    /// A pending `BK` breakpoint, if the script is paused on one: `Some(true)` = `BK debug`
    /// (variable dump), `Some(false)` = `BK on` (Y/N stop prompt). The GUI shows the dialog.
    pub fn breakpoint_request(&self) -> Option<bool> {
        self.interp.breakpoint_request()
    }

    /// The live user variables (name, value), sorted — for the `BK debug` dialog.
    pub fn breakpoint_vars(&self) -> Vec<(String, String)> {
        self.interp.vars_snapshot()
    }

    /// The `(line, col)` of the `BK` the script is paused on (1-based; `col` 0 = unknown), for the
    /// breakpoint dialog header.
    pub fn breakpoint_location(&self) -> Option<(usize, usize)> {
        self.interp.breakpoint_location()
    }

    /// Resolve a `BK` breakpoint: `stop` aborts the whole script (like Ctrl+Break); otherwise the
    /// script CONTINUES from just after the `BK`.
    pub fn resolve_breakpoint(&mut self, stop: bool) {
        if self.interp.breakpoint_request().is_none() {
            return;
        }
        if stop {
            self.abort_script();
        } else {
            self.last_error =
                self.interp.resume_wait(&mut self.host).err().map(|e| e.message().to_string());
            self.record_status_line();
            self.settle();
        }
    }
    /// Whether `SAAS` (no path) requested the native Save-As dialog; consumes the request.
    pub fn take_save_as_request(&mut self) -> bool {
        self.host.take_save_as_request()
    }
    /// Whether `OPEN` (no path) requested the native Open-file dialog; consumes the request.
    /// `Some(hex)` carries the `/HEX` intent (open the picked file in the hex view).
    pub fn take_open_request(&mut self) -> Option<bool> {
        self.host.take_open_request()
    }
    /// Whether `FIND` requested the GUI's Find/Replace dialog; consumes the request.
    pub fn take_find_request(&mut self) -> bool {
        self.host.take_find_request()
    }

    /// Whether a bare `GREP` requested the GREP dialog (drained each frame by the GUI).
    pub fn take_grep_dialog_request(&mut self) -> bool {
        self.host.take_grep_dialog_request()
    }

    /// A search just landed on a match — the GUI centers it if it's off-screen. One-shot.
    pub fn take_reveal_center(&mut self) -> bool {
        self.host.take_reveal_center()
    }
    /// Whether the active document has a non-empty selection (e.g. a match just found). Used by the
    /// Find dialog's single "Replace" to decide whether to overwrite the selected match.
    pub fn has_selection(&self) -> bool {
        self.host.active_has_selection()
    }
    /// The active document's selected text (empty if none) — seeds the Find dialog.
    pub fn selection_text(&self) -> String {
        self.host.active_selection_text()
    }
    /// The last search pattern (any source) — seeds the Find dialog on reopen.
    pub fn last_search(&self) -> String {
        self.host.last_search()
    }
    /// Whether shell-out is enabled (`TRUST 1`) — the Run-command dialog gates Run on this.
    pub fn trusted(&self) -> bool {
        self.host.special("TR").map(|v| v.to_str() == "1").unwrap_or(false)
    }
    /// Whether the OS window title bar is shown (`$TITLEBAR`) — the runtime applies it via
    /// `set_decorations` and the menu/drag-strip read it.
    pub fn show_titlebar(&self) -> bool {
        self.host.special("TITLEBAR").map(|v| v.to_str() == "1").unwrap_or(true)
    }
    /// Whether Writer's View is on (`WV`/`$WV`): the GUI hides all chrome, and the runtime drops the
    /// OS title bar. Read every frame by the GUI.
    pub fn writer_view(&self) -> bool {
        self.host.writer_view()
    }
    /// Turn Writer's View off (the GUI's `Esc`-exits-WV). Returns whether it WAS on (so the caller
    /// knows to consume the `Esc`).
    pub fn exit_writer_view(&mut self) -> bool {
        let was = self.host.writer_view();
        self.host.set_writer_view(false);
        was
    }
    /// Whether the GUI remembers the main window's size + position across runs (`$WINGEO`). The
    /// runtime gates both restore (`resumed`) and save (`exiting`) on this.
    pub fn remember_window_geometry(&self) -> bool {
        self.host.special("WINGEO").map(|v| v.to_str() == "1").unwrap_or(true)
    }
    /// Read any special variable as a string (`""` if unknown) — the menus use it to show ✓ state.
    pub fn special_str(&self, name: &str) -> String {
        self.host.special(name).map(|v| v.to_str()).unwrap_or_default()
    }
    /// A snapshot of the active document's properties for the Document ▸ Properties box: label/value
    /// pairs read from the live `$`-specials.
    pub fn doc_info(&self) -> Vec<(&'static str, String)> {
        let sp = |n: &str| self.host.special(n).map(|v| v.to_str()).unwrap_or_default();
        let path = self
            .active_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(unsaved)".to_string());
        vec![
            ("Path", path),
            ("Encoding", sp("EN")),
            ("Line endings", sp("NEWLINE")),
            ("Lines", sp("TL")),
            ("Words", sp("WC")),
            ("Size (bytes)", sp("FS")),
            ("Modified", if sp("MO") == "1" { "yes" } else { "no" }.to_string()),
        ]
    }
    /// Label/value pairs for the Help ▸ About box (name, version, OS).
    pub fn about_info(&self) -> Vec<(&'static str, String)> {
        let sp = |n: &str| self.host.special(n).map(|v| v.to_str()).unwrap_or_default();
        vec![("Name", "XyScript".to_string()), ("Version", sp("VE")), ("OS", sp("OS"))]
    }
    /// A readable chart of the current keymap's UNSHIFTED layer (scan code → command) for Help ▸
    /// Keyboard chart. Only bound keys are listed.
    pub fn keyboard_chart(&self) -> String {
        let mut s = String::from("Keyboard chart — unshifted layer (scan code → command)\n\n");
        for code in 1u8..=104 {
            if let Some(binding) = self.host.key_binding(code as i64) {
                s.push_str(&format!("{code:>4}  {binding}\n"));
            }
        }
        s
    }
    /// The loaded keymap's per-layer chart — one `(table name, [(scan code, binding)])` per modifier
    /// table (Normal, SHIFT, CTRL, …) — for the Help ▸ Keyboard chart cascade. See
    /// [`AppHost::keyboard_tables`].
    pub fn keyboard_tables(&self) -> Vec<(String, Vec<(u8, String)>)> {
        self.host.keyboard_tables()
    }
    /// Cache key for the keyboard chart — changes only when a keymap is (re)installed.
    pub fn keymap_generation(&self) -> u64 {
        self.host.keymap_generation()
    }
    /// Run a search/replace from the Find dialog. Builds a [`SearchSpec`] directly and dispatches
    /// it through the same engine path as `SE`/`CH` — so the pattern/replacement are taken LITERALLY
    /// (no command parsing, no `$`-substitution, so `$`, `/`, spaces, quotes all work). `replace`
    /// = `Some(text)` does a replace-all; `None` is a plain find. Returns the status string.
    pub fn search_spec(
        &mut self,
        find: &str,
        replace: Option<String>,
        backward: bool,
        case: bool,
        whole_word: bool,
        regex: bool,
        scope: bool,
        keep_case: bool,
    ) -> Result<String, String> {
        let spec = xyscript_xpl::SearchSpec {
            find: find.to_string(),
            replace,
            backward,
            case_sensitive: case,
            whole_word,
            from_top: false,
            scope,
            regex,
            keep_case,
            extend: false,
            verify: false,
            count: None,
        };
        op_status(self.host.editor_op(EditorOp::Search(spec)))
    }

    /// Run a `GREP` from the GREP dialog (Search ▸ Grep…): dispatches the op DIRECTLY with the literal
    /// pattern/path — no command parsing or `$`-substitution, so a `$` or space in the pattern is taken
    /// verbatim. `case` is case-INSENSITIVE (the `/I` flag); `path` is the optional search root/glob.
    pub fn run_grep(
        &mut self,
        pattern: &str,
        path: Option<String>,
        regex: bool,
        case: bool,
        whole: bool,
    ) -> Result<String, String> {
        op_status(self.host.editor_op(EditorOp::Grep {
            pattern: pattern.to_string(),
            path,
            regex,
            case,
            whole,
        }))
    }
    /// All match spans of `find` in the active document (for the FIND dialog's live highlight-all).
    /// Read-only; honors case/whole-word/regex/in-selection exactly like `search_spec`/`SE`.
    pub fn find_all(
        &self,
        find: &str,
        case: bool,
        whole: bool,
        regex: bool,
        in_selection: bool,
    ) -> Vec<(usize, usize)> {
        self.host.find_all(find, case, whole, regex, in_selection)
    }
    /// Language-check marks (rope spans) for the active document — the GUI draws these as underlines.
    pub fn lints(&self) -> &[crate::lint::LintMark] {
        self.host.lints()
    }
    /// Load the user's learned words from the dictionary file (call once at startup). See
    /// [`crate::host::AppHost::load_learned_words`].
    pub fn load_learned_words(&mut self) {
        self.host.load_learned_words()
    }
    /// If a background `CHECK` finished, open its report tab; returns whether it did. Call each frame.
    pub fn poll_check(&mut self) -> bool {
        self.host.poll_check()
    }
    /// Whether a background `CHECK` is still running (the GUI shows a "Working…" status).
    pub fn check_in_progress(&self) -> bool {
        self.host.check_in_progress()
    }
    /// The spelling correction choices for the word at rope offset `pos` (right-click spell menu), or
    /// `None` if the click isn't on a flagged word.
    pub fn lint_context_at(&self, pos: usize) -> Option<xyscript_xpl::LintFixChoices> {
        self.host.lint_context_at(pos)
    }
    /// The underlined issue at rope offset `pos` for the right-click popup — explanation + kind +
    /// any suggestions — for SPELLING or GRAMMAR alike. `None` if the click isn't on an underline.
    pub fn lint_explain_at(&self, pos: usize) -> Option<xyscript_xpl::LintFixChoices> {
        self.host.lint_explain_at(pos)
    }
    /// Apply a language correction: replace document range `span` with `replacement`.
    pub fn apply_lint_fix(&mut self, span: (usize, usize), replacement: &str) {
        use xyscript_xpl::Host;
        let _ = self.host.apply_lint_fix(span, replacement);
    }
    /// Are spelling underlines on? (`SPELLING`/`SP`.) The GUI filters marks by this.
    pub fn spelling_on(&self) -> bool {
        self.host.spelling_on()
    }
    /// Are grammar/word-choice underlines on? (`GRAMMAR`/`GR`.) The GUI filters marks by this.
    pub fn grammar_on(&self) -> bool {
        self.host.grammar_on()
    }
    /// Whether live checking is on and the visible byte window `(lo, hi)` is unchecked at this
    /// revision (the GUI's idle-timer trigger). See [`crate::host::AppHost::lint_stale`].
    pub fn lint_stale(&self, lo: usize, hi: usize) -> bool {
        self.host.lint_stale(lo, hi)
    }
    /// Re-run the checker SYNCHRONOUSLY over the visible byte window `(lo, hi)` (tests / headless; the
    /// GUI uses the async [`request_lint`](Self::request_lint) + [`poll_lint`](Self::poll_lint) pair).
    pub fn recheck_lints(&mut self, lo: usize, hi: usize) {
        self.host.recheck_lints(lo, hi)
    }
    /// Ask the background worker to check document `doc`'s visible window `(lo, hi)` — harper runs off
    /// the UI thread. `doc` lets a split check each pane's document. Pair with [`poll_lint`](Self::poll_lint).
    pub fn request_lint(&mut self, doc: usize, lo: usize, hi: usize) {
        self.host.request_lint(doc, lo, hi)
    }
    /// Apply any finished background lint result (if still current); returns whether underlines
    /// changed. Call each frame.
    pub fn poll_lint(&mut self) -> bool {
        self.host.poll_lint()
    }
    /// Whether a background lint request is still out (the GUI keeps polling until it lands).
    pub fn lint_in_flight(&self) -> bool {
        self.host.lint_in_flight()
    }
    /// The font-picker dialog a `EDFONTS`/`UIFONTS`/`TXTFONTS` verb requested, if any; consumes it.
    pub fn take_font_picker_request(&mut self) -> Option<crate::host::FontPickerKind> {
        self.host.take_font_picker_request()
    }
    /// The colour-picker dialog a `*COLORS`/`HLCOLORS` verb requested, if any; consumes it.
    pub fn take_color_picker_request(&mut self) -> Option<crate::host::ColorTarget> {
        self.host.take_color_picker_request()
    }

    // ---- directory listing (DIR/LS, B10) -----------------------------------
    // The GUI routes Enter and printable keys here (instead of editing) while the
    // active buffer is a listing.

    /// Whether the active buffer is a `DIR`/`LS` listing.
    pub fn active_is_dir_listing(&self) -> bool {
        self.host.active_is_dir_listing()
    }

    /// Whether the active buffer is a `GREP` results listing.
    pub fn active_is_grep_listing(&self) -> bool {
        self.host.active_is_grep_listing()
    }

    /// A footer line for the active listing (base dir + live express-key), if any.
    pub fn dir_footer(&self) -> Option<String> {
        self.host.dir_footer().map(|(base, find)| {
            if find.is_empty() {
                format!("DIR  {}", base.display())
            } else {
                format!("DIR  {}    find: {find}", base.display())
            }
        })
    }

    /// Express-key (type-to-find): extend the buffer by `ch` and seek.
    pub fn dir_find_push(&mut self, ch: char) {
        self.host.dir_find_push(ch);
    }

    /// Express-key: shorten the type-to-find buffer by one char.
    pub fn dir_find_backspace(&mut self) {
        self.host.dir_find_backspace();
    }

    /// Express-key: clear the type-to-find buffer (Esc).
    pub fn dir_find_clear(&mut self) {
        self.host.dir_find_clear();
    }

    /// `F3`: jump to the next express-key match in the active listing (wraps).
    pub fn dir_find_again(&mut self) {
        self.host.dir_find_again();
    }

    /// Copy the caret entry's path to the clipboard; reports it on the status line.
    pub fn dir_copy_path(&mut self) {
        if let Some(p) = self.host.dir_copy_path() {
            self.interp.set_status(Some(format!("copied {p}")));
        }
    }

    /// `Del` in a listing: arm a delete-with-confirm for the caret entry.
    pub fn dir_request_delete(&mut self) {
        self.host.dir_request_delete();
    }

    /// The pending delete-confirm prompt, if `Del` armed one.
    pub fn dir_delete_prompt(&self) -> Option<String> {
        self.host.dir_delete_prompt()
    }

    /// Answer a pending delete-confirm (`yes` deletes, `no` cancels); reports the outcome.
    pub fn dir_resolve_delete(&mut self, yes: bool) {
        if let Some(s) = self.host.dir_resolve_delete(yes) {
            self.interp.set_status(Some(s));
        }
    }

    /// The pending buffer-close confirmation (`AB/V` save-prompt / `ABD` delete-confirm), if any.
    pub fn close_prompt(&self) -> Option<String> {
        self.host.close_prompt()
    }
    /// Answer a pending close-confirmation; reports the outcome on the status line.
    pub fn resolve_close(&mut self, ans: crate::host::CloseAnswer) {
        if let Some(s) = self.host.resolve_close(ans) {
            self.interp.set_status(Some(s));
        }
    }

    /// The pending "file is already open" prompt (`OPEN`/`READ` of an already-open file), if any.
    pub fn open_prompt(&self) -> Option<String> {
        self.host.open_prompt()
    }
    /// Answer the "already open" prompt; reports the outcome on the status line.
    pub fn resolve_open(&mut self, ans: crate::host::OpenAnswer) {
        match self.host.resolve_open(ans) {
            Ok(v) => {
                let s = v.to_str();
                if !s.is_empty() {
                    self.interp.set_status(Some(s));
                }
            }
            Err(e) => self.last_error = Some(e.message().to_string()),
        }
    }

    /// `Ctrl+Enter` in a listing: launch the caret entry with the OS default handler.
    pub fn dir_launch(&mut self) {
        let s = self.host.dir_launch();
        if !s.is_empty() {
            self.interp.set_status(Some(s));
        }
    }

    /// Activate the caret's entry in the active listing (descend / open). Surfaces any
    /// error on the status line.
    pub fn dir_enter(&mut self) {
        match self.host.dir_enter() {
            Ok(()) => {}
            Err(e) => self.interp.set_status(Some(e.message().to_string())),
        }
    }

    /// `GJ` from a key/mouse path (Enter or double-click on a `GREP` results tab): open the hit under
    /// the caret and jump to it. Surfaces the destination (or the error) on the status line.
    pub fn grep_jump(&mut self) {
        let msg = match self.host.grep_jump() {
            Ok(v) => v.to_str(),
            Err(e) => e.message().to_string(),
        };
        self.interp.set_status(Some(msg));
    }

    /// Whether an interactive change-verify (`CV`) walk is awaiting a choice — the
    /// GUI shows the prompt and routes the next keypress to [`verify_respond`].
    pub fn verify_active(&self) -> bool {
        self.host.verify_active()
    }

    /// The prompt text for the active change-verify walk (`None` when idle).
    pub fn verify_prompt(&self) -> Option<String> {
        self.host.verify_prompt()
    }

    /// Apply the user's choice at the current change-verify stop.
    pub fn verify_respond(&mut self, choice: crate::host::VerifyChoice) {
        self.host.verify_respond(choice);
    }

    /// Update the viewport hint (top visible line, visible rows) the GUI maintains
    /// for the viewport motions (`PD`/`PU`/`HM`/`BS`).
    pub fn set_viewport(&mut self, top_line: usize, rows: usize) {
        self.host.set_viewport(top_line, rows);
    }

    /// Move the caret by `delta` logical lines (the GUI's page motion pairs this
    /// with an equal view scroll so the caret keeps its screen row).
    pub fn move_lines(&mut self, delta: i32, extend: bool) {
        self.host.move_active_by_lines(delta, extend);
    }

    /// The clipboard (`$CB`) contents — the GUI mirrors this to the OS clipboard.
    pub fn clipboard(&self) -> &str {
        self.host.clipboard()
    }

    /// Replace the clipboard (`$CB`) — used to mirror in an external OS-clipboard change.
    pub fn set_clipboard(&mut self, text: String) {
        self.host.set_clipboard(text);
    }

    /// A pending `QUIT` request, if any: `Some(true)` = prompt to save dirty tabs first,
    /// `Some(false)` = quit immediately. Consumes the request.
    pub fn take_quit(&mut self) -> Option<bool> {
        self.host.take_quit()
    }
    /// The first buffer with unsaved changes, or `None` (drives the `QUIT` save sweep).
    pub fn next_dirty_tab(&self) -> Option<usize> {
        self.host.next_dirty_tab()
    }
    /// Mark the active document saved without writing (the `QUIT` "discard" choice).
    pub fn mark_active_saved(&mut self) {
        self.host.mark_active_saved();
    }

    /// Render the document, and the command line beneath it when open.
    pub fn render(&self) -> String {
        let mut s = self.host.document().render();
        if self.cmline_open() {
            s.push_str("\n:");
            s.push_str(&self.host.command_line().render());
        }
        s
    }

    // ---- library passthrough (app/startup setup) ---------------------------

    pub fn load_system_library_source(
        &mut self,
        text: &str,
    ) -> Result<Vec<String>, Vec<String>> {
        self.interp.load_system_library_source(text)
    }
    pub fn load_user_library_source(&mut self, text: &str) -> Result<Vec<String>, Vec<String>> {
        self.interp.load_user_library_source(text)
    }

    // ---- the loop ----------------------------------------------------------

    /// Feed one physical key event. Returns the resolver's disposition so the
    /// live shell can release `KF` keys to the OS.
    pub fn feed_key(&mut self, ev: KeyEvent) -> Disposition {
        self.host.set_last_scan(ev.key_code as i64);
        // `$SCC` = `$SC` + the modifier bits held for this key. An injected event (the GUI reports
        // modifiers per event via `feed_key_mods`) carries them in `ev.bitmask`; a physical event has
        // them in the resolver's live state (held | latched modifier bits set by prior modifier-downs).
        let mods = if ev.phase == KeyPhase::Inject {
            ev.bitmask as i64
        } else {
            self.resolver.state.current_bitmask() as i64
        };
        self.host.set_last_modifier_bits(mods);

        // Modal command line: while the CMline is open, Enter submits and Esc
        // cancels — the universal convention. These two canonical keys are
        // intercepted here (synthesizing the `submit`/`cancel` XPL) only in
        // CMline mode; everywhere else the keymap decides. (Provisional: a
        // dedicated CMline keymap layer could later replace this.)
        const ENTER: u8 = 28;
        const ESC: u8 = 1;
        if self.cmline_open() && ev.phase == KeyPhase::Down {
            match ev.key_code {
                ENTER => {
                    self.run_command("submit");
                    return Disposition::Swallow;
                }
                ESC => {
                    self.run_command("cancel");
                    return Disposition::Swallow;
                }
                _ => {}
            }
        }

        let disp = self.resolver.resolve(ev);
        if let Disposition::Dispatch(code) = &disp {
            let code = code.clone();
            self.dispatch(&code);
        }
        disp
    }

    /// Feed a command key with explicit modifier state (e.g. from a GUI that
    /// reports modifiers per event rather than as held physical keys). We build the
    /// operative keymap's bitmask for the active modifiers and **inject** it, so the
    /// right layer (SHIFT/CTRL/…) is consulted — e.g. Shift+arrow resolves to the
    /// SHIFT table's `LEFT SEL` (extend-selection). Undeclared modifiers contribute
    /// nothing (strict lookup then simply finds no binding), never a wrong layer.
    pub fn feed_key_mods(&mut self, code: u8, shift: bool, ctrl: bool, alt: bool) -> Disposition {
        let mut bitmask = 0;
        for (active, name) in [(shift, "SHIFT"), (ctrl, "CTRL"), (alt, "ALT")] {
            if active {
                if let Some(bit) = self.resolver.keymap.bitmask_for_names(&[name]) {
                    bitmask |= bit;
                }
            }
        }
        self.feed_key(KeyEvent::inject(code, bitmask))
    }

    /// Run a startup script (`startup.xpl`): may `LOADKBD`, set vars, define
    /// nothing more than the language allows. The embedded default stands if it
    /// loads no KBD.
    pub fn run_startup(&mut self, src: &str, label: &str) {
        self.dispatch_with_args(src, &[], Some(label));
    }

    /// Run an XPL command directly — the CMline's manual command path / fallback
    /// when a key isn't bound.
    pub fn run_command(&mut self, src: &str) {
        self.dispatch(src);
    }

    /// Like [`run_command`](Self::run_command), but seeds the top-level script's positional args
    /// (`$1..$N`) and a source `label` (the `--run` file) so a top-level fatal error is stamped
    /// `<file>:<line>`. Used by the headless `--run <path> [args…]` entry.
    pub fn run_command_with_args(&mut self, src: &str, args: &[Value], label: Option<&str>) {
        self.dispatch_with_args(src, args, label);
    }

    /// Execute XPL through the interpreter, then reconcile editor-level effects.
    fn dispatch(&mut self, src: &str) {
        self.dispatch_with_args(src, &[], None);
    }

    fn dispatch_with_args(&mut self, src: &str, args: &[Value], label: Option<&str>) {
        // A parked script (READKEY/INPUT/WAIT) owns the interpreter until resumed; ignore
        // stray commands so a new run can't clobber the saved continuation.
        if self.interp.is_suspended() {
            return;
        }
        // A fresh top-level script: clear QUIET (it's script-scoped). Nested deferred runs
        // (settle_depth > 0) keep the script's level; a resumed run continues through `settle`.
        if self.settle_depth == 0 {
            self.host.reset_quiet();
        }
        self.sleep_deadline = None; // fresh top-level run — no carried-over timed pause
        if self.settle_depth == 0 {
            self.abort_requested = false; // a fresh top-level run clears a stale abort request
        }
        // Run through the cooperative-preemption slicer instead of the unbudgeted `run`: a tight
        // loop with no `WAIT` yields every `PREEMPT_SLICE` instructions instead of freezing the
        // GUI. `pump_yield` then drives the slices, time-boxed, to completion (short scripts finish
        // synchronously here; long/looping ones spill across frames via the GUI's per-frame poll).
        self.last_error = self
            .interp
            .run_sliced_with_args(src, args, label, xyscript_xpl::PREEMPT_SLICE, &mut self.host)
            .err()
            .map(|e| e.message().to_string());
        self.advance_script();
    }

    /// Per-frame driver the GUI calls every frame. Honors a pending Ctrl+Break abort (even while
    /// the script is parked on `WAIT`/`INPUT`), and drives a CPU-yielding script forward another
    /// time-boxed slice. A no-op when nothing is actively yielding — `WAIT`/`INPUT` resumes go
    /// through `poll_wait`/`feed_input_*`, and an idle editor does no work here.
    pub fn pump_yield(&mut self) {
        if self.abort_requested && self.interp.is_suspended() {
            self.abort_script();
            return;
        }
        if self.interp.is_yielded() {
            self.advance_script();
        }
    }

    /// Run a preemptively-sliced script forward, time-boxed to one frame budget, honoring a pending
    /// Ctrl+Break *between* slices (so abort fires within a frame even mid-loop). When the script
    /// finishes — or parks on a real `WAIT`/`INPUT`/`TXT` — surface its status and run deferred
    /// effects via `settle`. While it's still yielding, return so the GUI can repaint and re-enter
    /// via `pump_yield` next frame. Shared by `dispatch` (first slice) and `pump_yield`.
    fn advance_script(&mut self) {
        let start = std::time::Instant::now();
        let frame_budget = std::time::Duration::from_millis(FRAME_BUDGET_MS);
        while self.interp.is_yielded() {
            if self.abort_requested {
                self.abort_script();
                return;
            }
            self.last_error = self
                .interp
                .resume_yield(xyscript_xpl::PREEMPT_SLICE, &mut self.host)
                .err()
                .map(|e| e.message().to_string());
            if start.elapsed() >= frame_budget {
                break;
            }
        }
        if self.interp.is_yielded() {
            return; // still working — the GUI repaints and calls pump_yield again
        }
        self.record_status_line();
        self.settle();
    }

    /// Whether a script is currently parked (yielding on a CPU slice, or on `WAIT`/`INPUT`/`TXT`).
    /// The GUI uses this to gate the Ctrl+Break abort and show a "running" indicator.
    pub fn is_script_running(&self) -> bool {
        self.interp.is_suspended()
    }

    /// Whether a script is actively CPU-yielding (vs. cooperatively parked on `WAIT`/`INPUT`). The
    /// GUI requests an immediate repaint while this holds so the slices keep flowing.
    pub fn is_yielding(&self) -> bool {
        self.interp.is_yielded()
    }

    /// Request that the running/hung script be aborted (wired to Ctrl+Break in the GUI). Honored at
    /// the next `pump_yield` slice. A no-op label-wise if nothing is running.
    pub fn request_script_abort(&mut self) {
        self.abort_requested = true;
    }

    /// Drop the entire in-process continuation stack and reconcile editor state: clear any timed
    /// pause, discard a blind edit the aborted script left open, and surface "Script aborted".
    fn abort_script(&mut self) {
        self.abort_requested = false;
        self.sleep_deadline = None;
        let aborted = self.interp.abort();
        self.host.reset_quiet();
        if aborted {
            self.host.set_last_status_line("Script aborted");
        }
        if self.host.blind_active() {
            self.host.exit_blind_if_open();
        }
    }

    /// The status text to surface for the just-run command, honoring `QUIET`. `PR` output is
    /// always shown; an automatic op-result/error status obeys the quiet level: 1 hides
    /// informational statuses (errors still show), 2 also hides non-fatal (trappable) error text
    /// — `$ERROR` is set either way. Used by the GUI/TUI status line and `$LS`.
    pub fn command_feedback(&self) -> Option<String> {
        // A FATAL error (unknown command, parse/compile failure, assigning a read-only special, …)
        // aborts the run and is reported via `last_error` — it never sets `$ERROR`/`status_message`,
        // so without this it would never reach the status line. Always shown (it means the command
        // did not run) and takes precedence, even under QUIET.
        if let Some(e) = &self.last_error {
            return Some(format!("error: {e}"));
        }
        let out = self.interp.output();
        if !out.is_empty() {
            return Some(out.join("  ")); // PR — never suppressed
        }
        let msg = self.interp.status_message().filter(|s| !s.is_empty())?;
        let is_error = self.interp.error_flag();
        match self.host.quiet() {
            0 => Some(msg.to_string()),
            1 => is_error.then(|| msg.to_string()),
            _ => None, // QUIET/NV: hide informational + non-fatal errors
        }
    }

    /// Capture the command's surfaced feedback (PR output, else the op/error status) into the
    /// host for `$LS` — mirrors the GUI's status-line logic, so `$LS` matches what's shown.
    fn record_status_line(&mut self) {
        let fb = self.command_feedback().unwrap_or_default();
        if !fb.is_empty() {
            self.host.set_last_status_line(&fb);
        }
    }

    /// After a run/resume: resolve any `WAIT` whose condition already holds (which may
    /// resume the script — and finish it or re-park it), then run the deferred post-run
    /// effects once the script is fully done. While still parked, everything is deferred.
    fn settle(&mut self) {
        self.settle_depth += 1;
        self.drain_ready_waits();
        if !self.interp.is_suspended() {
            self.post_run();
        }
        self.settle_depth -= 1;
        // A cmline-submitted command that had SUSPENDED (MSG/INPUT/WAIT/TXT/BK) has now finished:
        // apply the resting-focus rule it couldn't run inline while parked. Only at the OUTERMOST
        // settle and only when truly done. (No-op unless a suspend set `cmline_submit_pending`.)
        if self.settle_depth == 0 && !self.interp.is_suspended() {
            if let Some(cmd) = self.cmline_submit_pending.take() {
                self.reconcile_cmline_focus(&cmd);
            }
        }
        // Auto-exit a `BEDIT` blind edit the script left open — but only at the OUTERMOST settle
        // (deferred runs recurse through here) and only when truly done, not parked on input/WAIT.
        // Discards (an explicit `SA` is what persists); `AB` is the manual early exit.
        if self.settle_depth == 0 && !self.interp.is_suspended() && self.host.blind_active() {
            self.host.exit_blind_if_open();
        }
    }

    /// Resume the parked script while a `WAIT` condition is already met — a job-wait whose
    /// jobs are gone, or a `WAIT <ms>` whose interval has elapsed. A bare `WAIT` with nothing
    /// running (or `WAIT 0`) resolves immediately; a chain of satisfied waits collapses.
    /// Stops when not waiting, or the current wait isn't satisfied yet.
    fn drain_ready_waits(&mut self) {
        loop {
            if let Some(target) = self.interp.wait_request() {
                let done = match target {
                    xyscript_xpl::WaitTarget::AllJobs => self.host.job_count() == 0,
                    xyscript_xpl::WaitTarget::Job(id) => !self.host.is_job_running(id),
                };
                if !done {
                    break;
                }
            } else if let Some(ms) = self.interp.sleep_request() {
                // Start the timer on first sight; resume once it elapses. `WAIT 0` is "now".
                let now = std::time::Instant::now();
                let deadline = *self
                    .sleep_deadline
                    .get_or_insert_with(|| now + std::time::Duration::from_millis(ms));
                if now < deadline {
                    break; // still pausing — the GUI re-polls each frame
                }
                self.sleep_deadline = None;
            } else {
                break; // not parked on a WAIT
            }
            self.last_error =
                self.interp.resume_wait(&mut self.host).err().map(|e| e.message().to_string());
        }
    }

    /// How long the GUI should wait before re-polling a parked `WAIT`: the remaining sleep
    /// interval for `WAIT <ms>`, or a short fixed tick while waiting on background jobs.
    /// `None` when no `WAIT` is pending. The GUI feeds this to `request_repaint_after`.
    pub fn wait_repaint_after(&self) -> Option<std::time::Duration> {
        if self.interp.sleep_request().is_some() {
            let now = std::time::Instant::now();
            Some(match self.sleep_deadline {
                Some(d) => d.saturating_duration_since(now),
                None => std::time::Duration::ZERO,
            })
        } else if self.interp.wait_request().is_some() {
            Some(std::time::Duration::from_millis(200))
        } else {
            None
        }
    }

    /// A pending `READKEY`/`INPUT` request: the GUI captures a keystroke (`Key`) or a
    /// command-line entry (`Line`) and feeds it back via `feed_input_*`.
    pub fn input_request(&self) -> Option<xyscript_xpl::InputRequest> {
        self.interp.input_request()
    }
    /// A pending `PICK` list chooser: `(title, items)`. The GUI shows the chooser and feeds the
    /// chosen item's index back via [`pick_respond`](Self::pick_respond).
    pub fn pick_request(&self) -> Option<(Option<String>, Vec<String>)> {
        self.interp.pick_request()
    }
    /// Whether the open chooser is a `LISTTABS` tab-switch (a single click acts immediately).
    pub fn pick_is_tab_switch(&self) -> bool {
        self.interp.pick_is_tab_switch()
    }
    /// Resume a `PICK`-parked script: `choice` is the chosen item's index (`None` = cancelled).
    pub fn pick_respond(&mut self, choice: Option<usize>) {
        if self.interp.pick_request().is_none() {
            return;
        }
        self.last_error =
            self.interp.resume_pick(choice, &mut self.host).err().map(|e| e.message().to_string());
        self.record_status_line();
        // The resumed run may pause again (another PICK/INPUT/WAIT); settle handles it.
        self.settle();
    }
    /// Parked on `READKEY` — the GUI captures the next raw keystroke for it.
    pub fn awaiting_key(&self) -> bool {
        matches!(
            self.interp.input_request().map(|r| r.kind),
            Some(xyscript_xpl::InputKind::Key)
        )
    }
    /// Parked on `INPUT` — the GUI collects a line of text for it.
    pub fn awaiting_line(&self) -> bool {
        matches!(
            self.interp.input_request().map(|r| r.kind),
            Some(xyscript_xpl::InputKind::Line)
        )
    }
    /// The `INPUT` prompt to display (`None` if not awaiting a line or no prompt given).
    pub fn input_prompt(&self) -> Option<String> {
        self.interp.input_request().and_then(|r| r.prompt)
    }

    /// What a parked `WAIT` is waiting on, if any (the GUI uses this to keep repainting).
    pub fn wait_request(&self) -> Option<xyscript_xpl::WaitTarget> {
        self.interp.wait_request()
    }

    /// Drive a pending `WAIT`: resume the parked script if its background jobs have finished
    /// (`WAIT`/`WAIT/J`) or its timed interval has elapsed (`WAIT <ms>`), running any deferred
    /// effects. The GUI calls this each frame after `poll_jobs`; a no-op unless a `WAIT` is
    /// pending and now satisfied.
    pub fn poll_wait(&mut self) {
        if self.interp.wait_request().is_some() || self.interp.sleep_request().is_some() {
            self.settle();
        }
    }

    /// Resume an `INPUT`-parked script with the line the user typed.
    pub fn feed_input_line(&mut self, text: String) {
        self.resume_input(xyscript_xpl::Value::str(text), None, 0); // a typed line carries no key/mods
    }

    /// Resume a `READKEY`-parked script with the character read and its scancode
    /// (surfaced as `$SC`). `ch` is empty for keys with no character (function keys).
    /// Whether the parked `READKEY` wants a key DESCRIPTION (`READKEY/D`) rather than the raw
    /// character — the GUI builds `Ctrl+Y`/`Enter`/… and feeds that instead.
    pub fn readkey_describe(&self) -> bool {
        self.interp.input_request().map(|r| r.describe).unwrap_or(false)
    }

    pub fn feed_input_key(&mut self, ch: &str, scancode: u16, mods: i64) {
        self.resume_input(xyscript_xpl::Value::str(ch.to_string()), Some(scancode), mods);
    }

    fn resume_input(&mut self, value: xyscript_xpl::Value, scancode: Option<u16>, mods: i64) {
        if self.interp.input_request().is_none() {
            return;
        }
        // READKEY delivers a scancode (→ `$SC`) plus the modifier bits held for that key (→ `$SCC`, so
        // `KEY($SCC)` reads the modified binding). An INPUT line has no key, so both are 0.
        self.host.set_last_scan(scancode.map(i64::from).unwrap_or(0));
        self.host.set_last_modifier_bits(mods);
        self.last_error = self
            .interp
            .resume_input(value, &mut self.host)
            .err()
            .map(|e| e.message().to_string());
        self.record_status_line();
        // The resumed run may pause again (another READKEY/INPUT/WAIT); settle handles it.
        self.settle();
    }

    /// Apply effects a script can only *request* mid-run: a keymap swap, a command-line
    /// submission, and a `RUNHERE` code-block execution.
    fn post_run(&mut self) {
        if let Some(km) = self.host.take_pending_kbd() {
            // Desugar literal cells (D4) before the loaded keymap goes operative.
            let km = crate::kbd_sugar::desugar_keymap(km);
            self.resolver.install_keymap(km.clone());
            self.host.set_keymap(km.clone()); // keep the host's copy in sync for SAVEKBD
            // Surface it so the GUI can resync its OWN (separate) capture resolver — the
            // editor's resolver above isn't the one the GUI intercepts keys with.
            self.reloaded_keymap = Some(km);
        }
        if let Some(cmd) = self.host.take_cmline_submit() {
            // A `SUBMIT` whose command re-`SUBMIT`s the same line recurses forever (native stack) —
            // cap it. Over the cap: abort with an error and blank the offending line so the next Enter
            // can't just re-trigger it. `reexec_depth` returns to 0 as the recursion unwinds.
            if self.reexec_depth >= CMLINE_REEXEC_MAX {
                self.host.clear_command_line();
                self.last_error = Some(CMLINE_REEXEC_MSG.to_string());
            } else {
                self.reexec_depth += 1;
                // Run the command SNAPSHOTTED at `SUBMIT` time — a later `BC` in the same chain can't
                // blank it out from under us. Keep the command in the cmline unless `BC`/`CMBLANK`.
                self.host.doc_focus_ran = false;
                self.host.explicit_focus = false;
                self.host.target = Target::Document;
                self.dispatch(&cmd);
                if self.interp.is_suspended() {
                    self.cmline_submit_pending = Some(cmd.clone()); // reconcile on completion (settle)
                    self.reexec_depth -= 1;
                    return; // parked; remaining effects wait for resume
                }
                // Per-verb resting focus (see `submit_cmdline`): respect a self-refocus to the cmline;
                // else land in the text only for a successful document-focusing command. An explicit
                // focus verb (`GT`/`TH`) is authoritative and skips the heuristic.
                if !self.host.explicit_focus && self.host.target != Target::CmLine {
                    self.host.target = if self.interp.error_flag() || !self.host.doc_focus_ran {
                        Target::CmLine
                    } else {
                        Target::Document
                    };
                }
                self.host.input_focus = self.host.target; // resync $TX focus (see submit_cmdline)
                if self.host.cmline_blank() || cmd_forces_cmline_blank(&cmd) {
                    self.host.clear_command_line(); // CMBLANK on, or a force-blank verb (AB/CD/D:/ST)
                }
                self.reexec_depth -= 1;
            }
        }
        // `XC` runs the cmline's shown contents (keeping them, unlike SUBMIT which blanks nothing but
        // is the "real" submit). It runs the command against the DOCUMENT (without the switch, a
        // focused cmline would run it against itself — e.g. `dfa` would select the cmline).
        if let Some(cmd) = self.host.take_cmline_exec() {
            // A bare `XC` on the cmline re-execs itself forever (native `post_run`→dispatch recursion)
            // — the exact mishap that crashed the app. Cap the depth; over it, abort + blank the line.
            if self.reexec_depth >= CMLINE_REEXEC_MAX {
                self.host.clear_command_line();
                self.last_error = Some(CMLINE_REEXEC_MSG.to_string());
            } else {
                self.reexec_depth += 1;
                // Run the SNAPSHOT taken at `XC` time, so `XC;BC` runs the command and THEN blanks.
                // Resting focus (per-verb rule, XC variant): a SUCCESSFUL document-focusing command
                // lands the caret in the text; otherwise (failure, or an exempt/neutral command) the
                // caret stays WHERE IT WAS when XC ran — `prev`, which may be the cmline OR the text.
                let prev = self.host.target;
                self.host.doc_focus_ran = false;
                self.host.explicit_focus = false;
                self.host.target = Target::Document;
                self.dispatch(&cmd);
                if self.interp.is_suspended() {
                    self.cmline_submit_pending = Some(cmd.clone()); // reconcile on completion (settle)
                    self.reexec_depth -= 1;
                    return;
                }
                // An explicit focus verb (`GT`/`TH`) is authoritative; otherwise a SUCCESSFUL
                // document-focusing command lands in the text, else focus returns to where it was.
                if !self.host.explicit_focus {
                    self.host.target = if !self.interp.error_flag() && self.host.doc_focus_ran {
                        Target::Document
                    } else {
                        prev
                    };
                }
                self.host.input_focus = self.host.target;
                if self.host.cmline_blank() || cmd_forces_cmline_blank(&cmd) {
                    self.host.clear_command_line(); // CMBLANK on, or a force-blank verb (AB/CD/D:/ST)
                }
                self.reexec_depth -= 1;
            }
        }
        // Deferred runs (RUNHERE blocks, `&X` script binders) now that the interpreter is
        // free again. FIFO queue: each `dispatch` may enqueue more (`&X`→`&Y` nesting),
        // drained until empty. A cap guards a self-referential binder (`&A` = "&A") from
        // hanging the editor. A pause mid-drain leaves the rest queued for after resume.
        let mut guard = 0;
        while let Some(src) = self.host.take_pending_run() {
            self.dispatch(&src);
            if self.interp.is_suspended() {
                return;
            }
            guard += 1;
            if guard > 1000 {
                self.last_error = Some("binder/RUNHERE recursion too deep".to_string());
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use xyscript_kbd::{Assignment, Keymap};

    /// Build a tiny "typing" keymap whose cells are runnable XPL.
    fn typing_keymap() -> Keymap {
        let mut tables: HashMap<(u32, u8), Assignment> = HashMap::new();
        let cell = |s: &str| Assignment::Xpl(s.to_string());
        tables.insert((0, 16), cell("insert \"h\"")); // q-position -> h
        tables.insert((0, 17), cell("insert \"i\"")); // w-position -> i
        tables.insert((0, 18), cell("bd")); // e-position -> backspace (BD)
        tables.insert((0, 19), cell("left")); // r-position -> left
        tables.insert((0, 20), cell("right")); // t-position -> right
        tables.insert((0, 21), cell("insert \"X\"")); // y-position -> X
        Keymap {
            modifiers: Vec::new(),
            tables,
            keys_max: 104,
            virtual_base: None,
        }
    }

    fn ed() -> Editor {
        Editor::new(typing_keymap())
    }


    #[test]
    fn gt_focuses_the_text_from_the_script_or_key_path() {
        let mut e = ed();
        e.open_cmdline();
        assert!(e.cmline_open());
        // GT via the raw dispatch path (a bound key, a script line, or a library routine) — NOT
        // the cmline-submit path. It must still move focus to the text.
        e.run_command("GT");
        assert!(!e.cmline_open(), "GT must focus the text on the key/script path");
    }

    #[test]
    fn a_bound_key_to_gt_and_th_changes_focus_via_the_resolver() {
        let mut km = typing_keymap();
        km.tables.insert((0, 30), Assignment::Xpl("GT".to_string())); // a-position → GT
        km.tables.insert((0, 31), Assignment::Xpl("TH".to_string())); // s-position → TH
        let mut e = Editor::new(km);
        e.open_cmdline();
        assert!(e.cmline_open());
        e.feed_key_mods(30, false, false, false); // press the GT key
        assert!(!e.cmline_open(), "a key bound to GT focuses the text");
        e.feed_key_mods(31, false, false, false); // press the TH key
        assert!(e.cmline_open(), "a key bound to TH toggles to the cmline");
        e.feed_key_mods(31, false, false, false);
        assert!(!e.cmline_open(), "TH toggles back to the text");
    }

    #[test]
    fn th_toggles_focus_from_the_key_path_both_directions() {
        let mut e = ed();
        e.open_cmdline();
        assert!(e.cmline_open(), "start focused on the cmline");
        e.run_command("TH"); // key/script path (like a bound F10)
        assert!(!e.cmline_open(), "TH from the cmline toggles to the text");
        e.run_command("TH");
        assert!(e.cmline_open(), "TH from the text toggles back to the cmline");
    }

    #[test]
    fn gt_focuses_the_text_via_submit_even_after_an_error() {
        // Bare GT submitted from the cmline works.
        let mut e = ed();
        e.open_cmdline();
        e.insert_str("GT");
        e.submit_cmdline();
        assert!(!e.cmline_open(), "submitting GT lands focus in the text");

        // GT after a command that set $ERROR earlier in the chain (a common shape in a routine):
        // the explicit focus command must still win over the resting-focus heuristic.
        let mut e = ed();
        e.run_command("SET s = hello");
        e.open_cmdline();
        e.insert_str("SE /zzznotfound/; GT");
        e.submit_cmdline();
        assert!(!e.cmline_open(), "an explicit GT wins even after an earlier error");

        // GT FIRST, then a command that errors — the error must NOT undo the explicit focus.
        let mut e = ed();
        e.open_cmdline();
        e.insert_str("GT; SE /zzznotfound/");
        e.submit_cmdline();
        assert!(!e.cmline_open(), "a later error must not undo an explicit GT");
    }

    #[test]
    fn xc_reruns_the_cmline_command_regardless_of_focus() {
        // A key-bound `XC` (F9) executes the command line's CONTENTS wherever focus is. A chained
        // command ending in a text-emitting verb moves focus to the document — but a further `XC`
        // must still re-run the (retained) cmline command from the text, so the user can iterate.
        let nl = crate::document::LineEnding::os_default();
        let mut e = ed();
        e.run_command("SET s=58;SET f=0");
        e.open_cmdline();
        e.insert_str("SET s+=1;SET f+=1;WRITELN $s=F$f is free");
        e.run_command("XC"); // from the cmline
        assert!(!e.cmline_open(), "WRITELN is document-focusing → focus moved to the text");
        e.run_command("XC"); // now from the document — still runs the cmline command
        e.run_command("XC");
        assert_eq!(
            e.document().text(),
            format!(
                "59=F1 is free{nl}60=F2 is free{nl}61=F3 is free{nl}",
                nl = nl.as_str()
            )
        );
    }

    #[test]
    fn typing_keys_appear_in_the_document() {
        let mut e = ed();
        e.feed_key(KeyEvent::down(16)); // h
        e.feed_key(KeyEvent::down(17)); // i
        assert_eq!(e.document().text(), "hi");
        assert_eq!(e.render(), "hi‸");
    }

    #[test]
    fn backspace_deletes() {
        let mut e = ed();
        e.feed_key(KeyEvent::down(16)); // h
        e.feed_key(KeyEvent::down(17)); // i
        e.feed_key(KeyEvent::down(18)); // backspace
        assert_eq!(e.document().text(), "h");
    }

    #[test]
    fn key_function_reports_the_current_binding() {
        let mut e = ed();
        // Bound keys report their operative cell (post-desugar).
        e.run_command(r#"PR $(KEY(18))"#); // (0,18) = bd
        assert_eq!(e.output().last().map(String::as_str), Some("bd"));
        e.run_command(r#"PR $(KEY(16))"#); // (0,16) = insert "h"
        assert_eq!(e.output().last().map(String::as_str), Some(r#"insert "h""#));
        // Unbound key → empty string.
        e.run_command(r#"PR [$(KEY(99))]"#);
        assert_eq!(e.output().last().map(String::as_str), Some("[]"));
        // A Keyboard-Flip (KF) cell reports as "KF"; a value carrying modifier bits (≥ 256, as `$SCC`
        // produces) resolves the MODIFIER layer rather than the unshifted one.
        let mut km = typing_keymap();
        km.tables.insert((0, 30), Assignment::Flip); // unshifted 30 → KF
        km.tables.insert((256, 30), Assignment::Xpl("insert \"^30\"".into())); // Ctrl(256) + 30
        e.host_mut().set_keymap(km);
        e.run_command(r#"PR $(KEY(30))"#); // unshifted layer
        assert_eq!(e.output().last().map(String::as_str), Some("KF"));
        e.run_command(r#"PR $(KEY(286))"#); // 30 + 256 → the Ctrl layer
        assert_eq!(e.output().last().map(String::as_str), Some(r#"insert "^30""#));
        e.run_command(r#"PR [$(KEY(542))]"#); // 30 + 512 (Alt) → unbound → empty
        assert_eq!(e.output().last().map(String::as_str), Some("[]"));
    }

    #[test]
    fn key_of_scc_resolves_the_modifier_layer() {
        let mut e = ed();
        let mut km = typing_keymap();
        km.tables.insert((256, 30), Assignment::Xpl("insert \"^30\"".into()));
        e.host_mut().set_keymap(km);
        // Press key 30 with the Ctrl bit (256) held — what the GUI's `feed_key_mods` injects. So
        // `$SC` = 30, `$SCC` = 30 + 256 = 286, and `KEY($SCC)` reads that key's Ctrl-layer binding.
        e.feed_key(KeyEvent::inject(30, 256));
        e.run_command("PR $(KEY($SCC))");
        assert_eq!(e.output().last().map(String::as_str), Some(r#"insert "^30""#));
        // `KEY($SC)` still reads the unshifted layer (here unbound → empty), unchanged.
        e.run_command("PR [$(KEY($SC))]");
        assert_eq!(e.output().last().map(String::as_str), Some("[]"));
    }

    #[test]
    fn getkey_names_a_computed_scancode_like_readkey_describe() {
        use xyscript_kbd::{ModBehavior, ModifierDef};
        let mut e = ed();
        let check = |e: &mut Editor, scc: i64, want: &str| {
            e.run_command(&format!("PR [$(GETKEY({scc}))]"));
            assert_eq!(e.output().last().map(String::as_str), Some(format!("[{want}]").as_str()), "GETKEY({scc})");
        };
        // Named keys are named by physical identity (canonical chart code), independent of any binding.
        for (scc, want) in [
            (14, "Backspace"), (28, "Enter"), (1, "Esc"), (15, "Tab"), (57, "Space"),
            (59, "F1"), (68, "F10"), (87, "F11"), (88, "F12"),
            (91, "Ins"), (92, "Home"), (95, "Del"), (100, "Up"), (103, "Right"),
            // Numpad cluster, lock keys, and Pause — named even though READKEY itself skips them.
            (55, "Numpad *"), (71, "Numpad 7"), (82, "Numpad 0"), (83, "Numpad ."),
            (94, "Numpad /"), (58, "CapsLock"), (69, "NumLock"), (70, "ScrLock"), (90, "Pause/Bk"),
        ] {
            check(&mut e, scc, want);
        }
        // Invalid $SCC → empty: an unnameable code, and modifier bits this (modifier-less) keymap
        // doesn't define (256 is not a known modifier here).
        check(&mut e, 200, "");
        check(&mut e, 14 + 256, "");

        // A keymap WITH modifiers + printable Normal bindings: a printable key reads its own char, and
        // a held modifier prefixes it — exactly what READKEY/D would insert.
        let modifier = |name: &str, bit: u32| ModifierDef {
            name: name.into(), bit, trigger_keys: vec![], behavior: ModBehavior::Hold, dual_role: false,
        };
        let mut tables: HashMap<(u32, u8), Assignment> = HashMap::new();
        tables.insert((0, 21), Assignment::Xpl("y".into())); // Y key types 'y'
        tables.insert((0, 2), Assignment::Xpl("1".into()));  // '1' key
        e.host_mut().set_keymap(Keymap {
            modifiers: vec![modifier("CTRL", 256), modifier("SHIFT", 1024)],
            tables,
            keys_max: 104,
            virtual_base: None,
        });
        check(&mut e, 21, "Y");                    // printable → its char, upper-cased
        check(&mut e, 2, "1");
        check(&mut e, 21 + 256, "CTRL+Y");         // held modifier prefixes it
        check(&mut e, 21 + 1024, "SHIFT+Y");
        check(&mut e, 21 + 256 + 1024, "CTRL+SHIFT+Y"); // prefix ordered by bit value
        check(&mut e, 28 + 256, "CTRL+Enter");     // named key + modifier, even unbound
    }

    #[test]
    fn key_uses_the_same_canonical_codes_as_sc() {
        // `scan` is the KBD-table / `$SC` numbering (canonical key code), not a native scancode:
        // press a key (sets `$SC`), then `KEY($SC)` reports that key's own binding.
        let mut e = ed();
        e.feed_key(KeyEvent::down(18)); // canonical code 18 → runs `bd`; $SC becomes 18
        e.run_command("PR $(KEY($SC))");
        assert_eq!(e.output().last().map(String::as_str), Some("bd"));
    }

    #[test]
    fn scc_adds_the_modifier_bit_to_the_scan_code() {
        let mut e = ed();
        let read = |e: &mut Editor, expr: &str| {
            e.run_command(&format!("PR {expr}"));
            e.output().last().cloned().unwrap_or_default()
        };
        // No modifier: $SCC == $SC (== the key's scan code). (This is exactly what the GUI's
        // `feed_key_mods` injects — an event carrying the OR-ed modifier bits as its bitmask.)
        e.feed_key(KeyEvent::inject(30, 0));
        assert_eq!(read(&mut e, "$SC"), "30");
        assert_eq!(read(&mut e, "$SCC"), "30");
        // A modifier bit (say 4 for Alt) is added to $SC in $SCC; $SC itself is unchanged.
        e.feed_key(KeyEvent::inject(30, 4));
        assert_eq!(read(&mut e, "$SC"), "30");
        assert_eq!(read(&mut e, "$SCC"), "34");
        // Combined modifiers OR together (1 | 4 = 5 → 30 + 5).
        e.feed_key(KeyEvent::inject(30, 5));
        assert_eq!(read(&mut e, "$SCC"), "35");
        // A subsequent unmodified key clears the bits again (no stale modifier).
        e.feed_key(KeyEvent::inject(31, 0));
        assert_eq!(read(&mut e, "$SCC"), "31");
    }

    #[test]
    fn bk_breakpoint_off_is_a_noop() {
        let mut e = ed();
        e.run_command("SET x = 1\nBK off\nSET x = 2\nPR $x");
        assert!(e.breakpoint_request().is_none()); // never paused
        assert_eq!(e.output().last().map(String::as_str), Some("2"));
    }

    #[test]
    fn bk_breakpoint_on_pauses_then_continue_or_stop() {
        // Continue resumes the rest of the script.
        let mut e = ed();
        e.run_command("SET x = 1\nBK on\nSET x = 2\nPR $x");
        assert_eq!(e.breakpoint_request(), Some(false)); // paused at BK on
        e.resolve_breakpoint(false); // continue
        assert!(e.breakpoint_request().is_none());
        assert_eq!(e.output().last().map(String::as_str), Some("2"));

        // Stop aborts: the post-BK statements never run.
        let mut e = ed();
        e.run_command("SET x = 1\nBK on\nSET x = 2");
        assert_eq!(e.breakpoint_request(), Some(false));
        e.resolve_breakpoint(true); // stop
        assert!(e.breakpoint_request().is_none());
        e.run_command("PR $x");
        assert_eq!(e.output().last().map(String::as_str), Some("1")); // x never became 2
    }

    #[test]
    fn bk_breakpoint_inside_a_while_loop_pauses_each_iteration() {
        let mut e = ed();
        e.run_command("SET i = 0\nWHILE $i < 3\nSET i += 1\nBK on\nENDWHILE\nPR done");
        let mut hits = 0;
        while e.breakpoint_request().is_some() {
            hits += 1;
            assert!(hits < 50, "breakpoint never cleared (infinite)");
            e.resolve_breakpoint(false);
        }
        assert_eq!(hits, 3, "should pause once per iteration");
        assert_eq!(e.output().last().map(String::as_str), Some("done"));
    }

    #[test]
    fn bk_breakpoint_after_a_wait_in_a_loop() {
        // The user's shape: WAIT then BK inside a loop. WAIT 0 resolves immediately (drain_ready_waits
        // during settle), so after each resume the script should land on the BK breakpoint.
        let mut e = ed();
        e.run_command("SET i = 0\nWHILE $i < 2\nSET i += 1\nWAIT 0\nBK on\nENDWHILE\nPR done");
        let mut hits = 0;
        while e.breakpoint_request().is_some() {
            hits += 1;
            assert!(hits < 50, "infinite / never cleared");
            e.resolve_breakpoint(false);
            e.poll_wait(); // drive any WAIT the resume parked on (mirror the GUI frame loop)
        }
        assert_eq!(hits, 2, "should hit the breakpoint each iteration (after the WAIT)");
        assert_eq!(e.output().last().map(String::as_str), Some("done"));
    }

    #[test]
    fn bk_after_a_timed_wait_in_a_loop_surfaces() {
        // The user's exact shape: a TIMED `WAIT` then `BK` inside a loop, driven over real time by
        // poll_wait (mirrors the GUI frame loop). The breakpoint must surface each iteration.
        let mut e = ed();
        e.run_command("SET i = 0\nWHILE $i < 2\nSET i += 1\nWAIT 50\nBK on\nENDWHILE\nPR done");
        let mut hits = 0;
        let mut guard = 0;
        loop {
            e.poll_wait();
            if e.breakpoint_request().is_some() {
                hits += 1;
                e.resolve_breakpoint(false);
                continue;
            }
            if e.output().last().map(String::as_str) == Some("done") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            guard += 1;
            assert!(guard < 300, "timed out (hits so far: {hits})");
        }
        assert_eq!(hits, 2, "breakpoint should surface after each timed WAIT");
    }

    #[test]
    fn bk_in_a_loop_via_nested_run_surfaces() {
        // The user's real setup: RUN a script file whose loop has WAIT then BK (nested-run cascade).
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert(
            "t.xpl".into(),
            "SET i = 0\nWHILE $i < 3\nSET i += 1\nWAIT 0\nBK on\nENDWHILE\nPR done".into(),
        );
        e.run_command("RUN t.xpl");
        let mut hits = 0;
        while e.breakpoint_request().is_some() {
            hits += 1;
            assert!(hits < 50, "infinite / never cleared");
            e.resolve_breakpoint(false);
            e.poll_wait();
        }
        assert_eq!(hits, 3, "breakpoint should surface each iteration inside a RUN-ed loop");
        assert_eq!(e.output().last().map(String::as_str), Some("done"));
    }

    #[test]
    fn while_condition_needs_dollar_for_a_variable() {
        // Bare `num` in an expression is the STRING "num", not the variable — so `num <= 10` is
        // false and the loop never runs. (This is why a `BK` inside such a loop never fires.)
        let mut e = ed();
        e.run_command("SET num = 0\nWHILE num <= 10\nSET num += 1\nENDWHILE\nPR $num");
        assert_eq!(e.output().last().map(String::as_str), Some("0")); // loop body never executed
        // With `$num` the loop runs as intended.
        let mut e2 = ed();
        e2.run_command("SET num = 0\nWHILE $num <= 10\nSET num += 1\nENDWHILE\nPR $num");
        assert_eq!(e2.output().last().map(String::as_str), Some("11"));
    }

    #[test]
    fn bk_reports_its_line_and_column() {
        let mut e = ed();
        e.run_command("SET x = 1\n\tBK on"); // BK on line 2, after a tab → column 2
        assert_eq!(e.breakpoint_request(), Some(false));
        assert_eq!(e.breakpoint_location(), Some((2, 2)));
        // No indentation → column 1.
        let mut e2 = ed();
        e2.run_command("BK debug");
        assert_eq!(e2.breakpoint_location(), Some((1, 1)));
    }

    #[test]
    fn bk_debug_reports_the_live_variables() {
        let mut e = ed();
        e.run_command("SET name = \"Ada\"\nSET n = 42\nBK debug");
        assert_eq!(e.breakpoint_request(), Some(true));
        let vars = e.breakpoint_vars(); // names are case-folded (uppercase) in the Env
        assert!(vars.iter().any(|(k, v)| k == "NAME" && v == "Ada"));
        assert!(vars.iter().any(|(k, v)| k == "N" && v == "42"));
        e.resolve_breakpoint(false); // continue to finish
        assert!(e.breakpoint_request().is_none());
    }

    #[test]
    fn runaway_loop_yields_instead_of_freezing_then_aborts() {
        let mut e = ed();
        // An infinite loop with no WAIT. Under the old unbudgeted runner this would spin the CPU
        // forever and hard-freeze the GUI; with cooperative preemption it must time-box and PARK
        // (yield) so the editor stays responsive and a Ctrl+Break can be seen.
        e.run_command("SET x = 0\nWHILE 1\nSET x = $x + 1\nENDWHILE\n");
        assert!(
            e.is_script_running(),
            "a runaway loop must park (yield), not run to completion or freeze"
        );
        assert!(e.is_yielding(), "a no-WAIT loop parks specifically on a preemption yield");

        // Ctrl+Break: request the abort, then the next pump drops the whole continuation stack.
        e.request_script_abort();
        e.pump_yield();
        assert!(!e.is_script_running(), "abort must drop the parked runaway script");

        // The editor is usable again afterward: a normal command runs to completion.
        e.run_command("SET y = 7\n");
        assert!(!e.is_script_running());
    }

    #[test]
    fn decimals_setting_controls_float_display() {
        let mut e = ed();
        e.run_command("SET x = SQRT(2)");
        // Default: full precision.
        e.run_command("PR $x");
        assert!(e.output().last().unwrap().starts_with("1.4142135"));
        // DECIMALS 2 → two fixed places, in both $var and inline $(…); integers unaffected.
        e.run_command("DECIMALS 2");
        e.run_command("PR $x");
        assert_eq!(e.output().last().map(String::as_str), Some("1.41"));
        e.run_command("PR $(SQRT(2))");
        assert_eq!(e.output().last().map(String::as_str), Some("1.41"));
        e.run_command("PR $(SQRT(4))"); // whole float still shows fixed places
        assert_eq!(e.output().last().map(String::as_str), Some("2.00"));
        e.run_command("PR $(LEN(\"abc\"))"); // an integer is untouched
        assert_eq!(e.output().last().map(String::as_str), Some("3"));
        // Readback.
        e.run_command("PR $DECIMALS");
        assert_eq!(e.output().last().map(String::as_str), Some("2"));
        // Computation keeps full precision — only display rounds.
        e.run_command("SET y = $x * 2");
        e.run_command("DECIMALS off");
        e.run_command("PR $y");
        assert!(e.output().last().unwrap().starts_with("2.8284271")); // not 2.82 * 2
        e.run_command("PR $DECIMALS");
        assert_eq!(e.output().last().map(String::as_str), Some("off"));
    }

    #[test]
    fn inline_expr_substitution_works_in_commands() {
        let mut e = ed();
        // $(expr) is evaluated inline in any command argument (functions, arithmetic).
        e.run_command("PR $(CHR(65))");
        assert_eq!(e.output().last().map(String::as_str), Some("A"));
        e.run_command("PR $(ASC(\"A\"))");
        assert_eq!(e.output().last().map(String::as_str), Some("65"));
        // …and via PUT into the document.
        e.run_command("PUT $(CHR(66))$(CHR(67))");
        assert_eq!(e.document().text(), "BC");
    }

    #[test]
    fn savecm_is_captureable() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.run_command("LoadCM \"first cmd\\nsecond cmd\""); // populate the history
        // Under CAPTURE, SAVECM yields the LoadCM snapshot text into the variable (no file written).
        e.run_command("CAPTURE snap SAVECM");
        assert!(e.host_mut().files.is_empty(), "capture must not write a file");
        e.run_command("PR $snap");
        let out = e.output().last().unwrap();
        assert!(out.contains("LoadCM \""));
        assert!(out.contains("first cmd") && out.contains("second cmd"));
    }

    #[test]
    fn savecm_reescapes_control_chars_so_the_loadcm_line_cannot_break() {
        // Regression: a recorded entry can hold LITERAL control chars (a corrupted/multi-char path or
        // command). SaveCM must re-escape them, or a literal newline splits the `LoadCM "…"` across
        // physical lines and re-RUNning the saved file mis-parses the tail (`EW: unknown switch …`),
        // fatally aborting the caller's script — e.g. a startup that restores command history.
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        // `\r`/`\t` decode to a real CR and TAB stored inside the single entry (no `\n` separator).
        e.run_command(r#"LoadCM "a\rb\tc""#);
        assert_eq!(e.command_history(), ["a\rb\tc"]);
        e.run_command("CAPTURE snap SAVECM");
        e.run_command("PR $snap");
        // The snapshot is `# header\nLoadCM "…"\n` (one output entry); pull out the LoadCM line.
        let snap = e.output().iter().rev().find(|l| l.contains("LoadCM \"")).unwrap().clone();
        let line = snap.lines().find(|l| l.starts_with("LoadCM")).unwrap().to_string();
        assert!(!line.contains('\r') && !line.contains('\t'), "raw control chars reached the file: {line:?}");
        assert!(line.contains(r"\r") && line.contains(r"\t"), "control chars must be escaped: {line:?}");
        // Round-trip: re-running the saved line restores the exact entry (no fatal parse error).
        e.run_command(r#"LoadCM "clobber""#);
        e.run_command(&line);
        assert_eq!(e.command_history(), ["a\rb\tc"], "control-char entry round-trips exactly");
    }

    #[test]
    fn savelib_round_trips_user_routines() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert(
            "src.lib".into(),
            "SUB greet\n# DOC: says hi\nPR hello\nENDSUB\nSUB bye\nPR goodbye\nENDSUB".into(),
        );
        e.run_command("LOADLIB src.lib");
        // Save the loaded user library to a fresh file, then wipe it from memory.
        e.run_command("SAVELIB out.lib");
        e.run_command("UNLOADLIB");
        // Reload from the SAVELIB output and confirm the routine still runs.
        e.run_command("LOADLIB out.lib");
        e.run_command("CALL greet");
        assert!(e.output().iter().any(|l| l == "hello"), "reloaded routine ran: {:?}", e.output());
        // The saved file preserves the SUB/ENDSUB shape and the `# DOC:` line.
        let saved = e.host_mut().files.get("out.lib").cloned().unwrap();
        assert!(saved.contains("SUB GREET") && saved.contains("ENDSUB"));
        assert!(saved.contains("# DOC: says hi"), "DOC preserved: {saved}");
    }

    /// A routine loaded by `LOADLIB` must be callable LATER IN THE SAME SCRIPT — the `startup.xpl`
    /// shape. Library routines are lowered into the program up front, so the compiled routine table
    /// is a snapshot from before the `LOADLIB` ran; the call used to die as a FATAL "unknown
    /// command", which killed the rest of the script (reported from the field: startup.xpl
    /// terminating early). The VM now falls back to the host's CURRENT library.
    #[test]
    fn a_routine_loadlib_ed_mid_script_is_callable_by_the_same_script() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("src.lib".into(), "SUB greet\nPR hello\nENDSUB".into());
        // ONE program: load, then call, then keep going.
        e.run_command("LOADLIB src.lib\ngreet\nPR still-running");
        assert_eq!(e.last_error(), None, "the call must not be a fatal unknown command");
        let out = e.output();
        assert!(out.iter().any(|l| l == "hello"), "routine ran: {out:?}");
        assert!(out.iter().any(|l| l == "still-running"), "script continued past it: {out:?}");
    }

    /// ...but a `SUB` defined inside a RUN-ed script stays LOCAL to that script (user decision
    /// 2026-07-20): `.lib` + `LOADLIB` is the mechanism for sharing routines. Pinned so the
    /// `LOADLIB` fallback above is never widened into "any SUB anyone ever ran".
    #[test]
    fn a_sub_defined_in_a_run_ed_script_stays_local_to_it() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("defs.xpl".into(), "SUB helper\nPR from-helper\nENDSUB".into());
        e.run_command("RUN defs.xpl\nhelper");
        assert!(
            e.last_error().unwrap_or_default().contains("unknown command"),
            "a RUN-ed script's SUB must not leak to the caller: {:?}",
            e.last_error()
        );
    }

    /// Renaming a file that is OPEN must move the tab with it: the tab label is derived from the
    /// document's path, so the open doc has to be repointed at the new name (reported: the tab kept
    /// showing the old file name after `RENAME`).
    #[test]
    fn renaming_an_open_file_updates_its_tab() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("old.txt".into(), "hello\n".into());
        e.run_command("OPEN old.txt");
        assert_eq!(e.host().active_doc().file_name().as_deref(), Some("old.txt"));

        e.run_command("RENAME old.txt new.txt");
        assert_eq!(e.last_error(), None);
        // The open tab now shows the new name, and its path followed.
        assert_eq!(
            e.host().active_doc().file_name().as_deref(),
            Some("new.txt"),
            "the tab label must follow the rename"
        );
        assert_eq!(e.active_path().unwrap().to_string_lossy(), "new.txt");

        // Renaming a file that ISN'T open is unaffected (and doesn't disturb the active tab).
        e.host_mut().files.insert("other.txt".into(), "x".into());
        e.run_command("RENAME other.txt renamed-other.txt");
        assert_eq!(e.last_error(), None);
        assert_eq!(e.host().active_doc().file_name().as_deref(), Some("new.txt"));
    }

    /// Session save+restore (Phase 1, shadow-backup model): a clean named file, a DIRTY named file,
    /// and an untitled buffer all come back — the dirty/untitled ones marked modified with their
    /// unsaved content, the clean one reopened from disk, and the active tab re-selected. Transient
    /// buffers (a blank scratch) are not restored.
    #[test]
    fn session_save_and_restore_round_trips_tabs() {
        let base = std::env::temp_dir().join(format!("xyscript-session-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let sdir = base.join("session");
        std::fs::write(base.join("clean.txt"), b"on disk\n").unwrap();
        std::fs::write(base.join("named.txt"), b"original\n").unwrap();

        // --- First session: three real tabs + a blank scratch. ---
        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("OPEN clean.txt"); // reuses the blank scratch; stays clean
        e.run_command("OPEN named.txt");
        e.run_command("TF"); // top of file
        e.run_command(r#"INSERT "EDIT ""#); // named.txt now dirty: "EDIT original\n"
        e.run_command("NEW"); // a fresh untitled tab
        e.run_command(r#"INSERT "scratch notes""#); // untitled, now with content
        assert_eq!(e.buffer_count(), 3);

        let saved = e.host_mut().save_session(&sdir).unwrap();
        assert!(saved >= 3, "at least clean + dirty + untitled recorded, got {saved}");

        // The user's REAL file on disk is untouched (shadow model).
        assert_eq!(std::fs::read(base.join("named.txt")).unwrap(), b"original\n");

        // --- Fresh editor: restore. ---
        let mut e2 = ed();
        e2.run_command(&format!("CD {}", base.display()));
        let restored = e2.host_mut().restore_session(&sdir).unwrap();
        assert_eq!(restored, saved, "every recorded tab restored");
        // No leftover blank scratch: the editor's initial untitled is pruned once real tabs land.
        assert_eq!(e2.buffer_count(), restored, "the startup scratch is pruned after restore");

        // Find the restored docs by their content/path.
        let texts: Vec<String> = (0..e2.buffer_count())
            .map(|i| e2.host().doc(i).text())
            .collect();
        assert!(texts.iter().any(|t| t.contains("on disk")), "clean file reopened from disk");
        assert!(texts.iter().any(|t| t.starts_with("EDIT")), "dirty file restored with unsaved edit");
        assert!(texts.iter().any(|t| t.contains("scratch notes")), "untitled restored");
        // The shadow-backed tabs come back MODIFIED (so a later save writes them out).
        assert!(e2.next_dirty_tab().is_some(), "restored dirty/untitled tabs report modified");

        std::fs::remove_dir_all(&base).ok();
    }

    /// Phase 2 split-pane layout restore: a side-by-side split of two files comes back as a split
    /// (not flattened to tabs), with the same docs in the panes, after remapping the saved DocIds.
    #[test]
    fn session_restores_split_pane_layout() {
        let base = std::env::temp_dir().join(format!("xyscript-splitsess-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let sdir = base.join("session");
        std::fs::write(base.join("a.txt"), b"AAA\n").unwrap();
        std::fs::write(base.join("b.txt"), b"BBB\n").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("OPEN a.txt");
        e.run_command("OPEN/V b.txt"); // open b.txt in a side-by-side split pane
        assert_eq!(e.host().panes().pane_count(), 2, "setup: two panes");
        e.host_mut().save_session(&sdir).unwrap();

        // Restore into a fresh editor.
        let mut e2 = ed();
        e2.run_command(&format!("CD {}", base.display()));
        e2.host_mut().restore_session(&sdir).unwrap();

        // The split is rebuilt (not flattened), and it's side-by-side (two rects, different x).
        assert_eq!(e2.host().panes().pane_count(), 2, "split layout restored");
        let rects = e2
            .host()
            .panes()
            .layout_rects(crate::panes::Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 });
        assert_eq!(rects.len(), 2);
        assert_ne!(rects[0].rect.x, rects[1].rect.x, "side-by-side split preserved");
        // Both pane docs resolve to restored tabs (the DocId remap worked).
        for pr in &rects {
            assert!(
                e2.host().panes().contains_doc(pr.doc),
                "pane doc is a restored, live document"
            );
        }

        std::fs::remove_dir_all(&base).ok();
    }

    /// Per-pane scroll fidelity: a pane's scroll offset (`ViewState`, normally mirrored from the GUI
    /// viewport via `set_pane_scroll`) round-trips through a session save/restore — even for a single
    /// pane, whose layout line is now always written. Restored offsets come back exactly.
    #[test]
    fn session_restores_per_pane_scroll() {
        let base = std::env::temp_dir().join(format!("xyscript-scrollsess-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let sdir = base.join("session");
        std::fs::write(base.join("a.txt"), b"AAA\n").unwrap();
        std::fs::write(base.join("b.txt"), b"BBB\n").unwrap();

        // Single pane: scroll offset survives even though there's no split.
        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("OPEN a.txt");
        let p = e.host().focused_pane_id();
        e.host_mut().set_pane_scroll(p, 37, 4.5, false); // scrolled away from the caret
        e.host_mut().save_session(&sdir).unwrap();

        let mut e2 = ed();
        e2.run_command(&format!("CD {}", base.display()));
        e2.host_mut().restore_session(&sdir).unwrap();
        let rp = e2.host().focused_pane_id();
        let (sl, sv, follow) = e2.host().pane_scroll(rp).expect("focused pane exists");
        assert_eq!(sl, 37, "scroll_line restored");
        assert!((sv - 4.5).abs() < 1e-3, "scroll_vertical restored");
        assert!(!follow, "follow flag restored");

        // Split panes: each pane keeps its own offset.
        let mut e3 = ed();
        e3.run_command(&format!("CD {}", base.display()));
        e3.run_command("OPEN a.txt");
        e3.run_command("OPEN/V b.txt");
        let panes: Vec<_> = e3.host().panes().panes();
        assert_eq!(panes.len(), 2);
        for (i, (pid, _)) in panes.iter().enumerate() {
            e3.host_mut().set_pane_scroll(*pid, (i + 1) * 10, 0.0, false);
        }
        let sdir2 = base.join("session2");
        e3.host_mut().save_session(&sdir2).unwrap();

        let mut e4 = ed();
        e4.run_command(&format!("CD {}", base.display()));
        e4.host_mut().restore_session(&sdir2).unwrap();
        let restored: std::collections::HashSet<usize> = e4
            .host()
            .panes()
            .panes()
            .iter()
            .map(|(pid, _)| e4.host().pane_scroll(*pid).unwrap().0)
            .collect();
        assert_eq!(restored, [10, 20].into_iter().collect(), "each pane's scroll restored");

        std::fs::remove_dir_all(&base).ok();
    }

    /// Phase 2 crash resilience: repeated snapshots are CHANGED-ONLY (an unchanged buffer's shadow
    /// isn't rewritten) and stale backups are garbage-collected. Verified by sentinelling the shadow
    /// file on disk and watching whether a snapshot overwrites it.
    #[test]
    fn session_snapshot_is_changed_only_and_gcs_stale_backups() {
        let base = std::env::temp_dir().join(format!("xyscript-snap-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let sdir = base.join("session");

        let mut e = ed();
        e.run_command("NEW"); // untitled tab
        e.run_command(r#"INSERT "aaa""#); // with content → always shadow-backed
        e.host_mut().save_session(&sdir).unwrap();

        // Find the single shadow backup and overwrite it with a sentinel.
        let bak = std::fs::read_dir(&sdir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.file_name().unwrap().to_string_lossy().starts_with("bak-"))
            .expect("a shadow backup was written");
        std::fs::write(&bak, b"SENTINEL").unwrap();

        // No edit → the snapshot must NOT rewrite the (unchanged) shadow.
        e.host_mut().save_session(&sdir).unwrap();
        assert_eq!(std::fs::read(&bak).unwrap(), b"SENTINEL", "unchanged buffer's shadow left alone");

        // Edit → its revision moves → the shadow is rewritten (sentinel gone, new content in).
        e.run_command(r#"INSERT "bbb""#);
        e.host_mut().save_session(&sdir).unwrap();
        let now = std::fs::read(&bak).unwrap();
        assert_ne!(now, b"SENTINEL", "changed buffer's shadow rewritten");
        assert!(String::from_utf8_lossy(&now).contains("bbb"));

        // GC: a straggler backup (e.g. from a prior run) is removed on the next snapshot.
        std::fs::write(sdir.join("bak-99999"), b"stale").unwrap();
        e.host_mut().save_session(&sdir).unwrap();
        assert!(!sdir.join("bak-99999").exists(), "straggler backup garbage-collected");
        assert!(bak.exists(), "the live backup is kept");

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn savelib_is_captureable() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("src.lib".into(), "SUB f\nPR ok\nENDSUB".into());
        e.run_command("LOADLIB src.lib");
        e.host_mut().files.remove("src.lib");
        e.run_command("CAPTURE lib SAVELIB");
        assert!(e.host_mut().files.is_empty(), "CAPTURE must not write a file");
        let m: std::collections::HashMap<String, String> = e.breakpoint_vars().into_iter().collect();
        assert!(m.get("LIB").cloned().unwrap_or_default().contains("SUB F"), "captured lib text");
    }

    #[test]
    fn savecfg_savear_savekbd_are_captureable() {
        use std::collections::HashMap;
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        // Load a real auto-replace pair, then drop the source file so we can assert that CAPTURE
        // writes NO file of its own.
        e.host_mut().files.insert("fix.ar".into(), "teh=the\n".into());
        e.run_command("LoadAR fix.ar");
        e.host_mut().files.remove("fix.ar");
        // Under CAPTURE, each SAVE* yields the would-be file text into the variable (no file written).
        e.run_command("CAPTURE cfg SAVECFG");
        e.run_command("CAPTURE ar SAVEAR");
        e.run_command("CAPTURE kbd SAVEKBD");
        assert!(e.host_mut().files.is_empty(), "CAPTURE must not write any file");
        let m: HashMap<String, String> = e.breakpoint_vars().into_iter().collect();
        assert!(!m.get("CFG").cloned().unwrap_or_default().trim().is_empty(), "SAVECFG capture empty");
        assert!(m.get("AR").cloned().unwrap_or_default().contains("teh=the"), "SAVEAR capture missing pair");
        assert!(!m.get("KBD").cloned().unwrap_or_default().trim().is_empty(), "SAVEKBD capture empty");
    }

    #[test]
    fn listspecials_generates_printexable_template() {
        let mut e = ed();
        // Default: insert one `$NAME: |$NAME|` line per special into the active document.
        e.run_command("LISTSPECIALS");
        let doc = e.document().text();
        assert!(doc.starts_with("$FI: |$FI|\n"));
        assert!(doc.contains("\n$DECIMALS: |$DECIMALS|\n"));
        assert!(doc.trim_end().ends_with("$RV: |$RV|"));
        assert!(doc.lines().count() > 50);

        // Under CAPTURE it feeds the variable instead of touching the document.
        let mut e2 = ed();
        e2.run_command("CAPTURE t LISTSPECIALS");
        assert_eq!(e2.document().text(), "");
        e2.run_command("PR $t");
        assert!(e2.output().last().unwrap().contains("$OS: |$OS|"));
    }

    #[test]
    fn printex_expands_pipe_fields_to_a_file() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.run_command("SET name = \"Ada\"");
        // `\$` keeps each field literal in the document; PRINTEX expands it later. `\|` is a
        // literal pipe.
        e.run_command("INSERT \"Hi |\\$name|, code |\\$(CHR(65))|, pipe \\| done\"");
        // The document keeps the un-expanded fields…
        assert_eq!(e.document().text(), "Hi |$name|, code |$(CHR(65))|, pipe \\| done");
        // …and PRINTEX <file> writes the expanded copy AND opens it in a new tab.
        e.run_command("PRINTEX out.txt");
        let out = e.host_mut().files.get("out.txt").cloned().expect("file written");
        assert_eq!(out, "Hi Ada, code A, pipe | done");
        assert_eq!(e.buffer_count(), 2); // opened a new tab
        assert_eq!(e.document().text(), "Hi Ada, code A, pipe | done"); // active = the opened result
        assert!(e.active_path().unwrap().to_string_lossy().contains("out.txt"));
    }

    #[test]
    fn new_with_a_filename_creates_or_opens() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("exists.txt".into(), "HELLO".into());

        // Bare NEW still makes an unnamed empty buffer.
        e.run_command("NEW");
        assert!(e.active_path().is_none());

        // NEW <new-file>: a fresh buffer named for it (ready to SAVE), empty.
        e.run_command("NEW fresh.txt");
        assert!(e.active_path().unwrap().to_string_lossy().contains("fresh.txt"));
        assert_eq!(e.document().text(), "");
        assert_eq!(e.status_message(), Some("new file fresh.txt"));

        // NEW <existing-file>: forgiving — opens it, with an "already exists" heads-up.
        e.run_command("NEW exists.txt");
        assert!(e.active_path().unwrap().to_string_lossy().contains("exists.txt"));
        assert_eq!(e.document().text(), "HELLO");
        assert!(e.status_message().unwrap().contains("already exists"));

        // NEW <existing-and-already-open>: switch to that tab, don't duplicate it.
        let n = e.buffer_count();
        e.run_command("NEW exists.txt");
        assert_eq!(e.buffer_count(), n);
        assert!(e.active_path().unwrap().to_string_lossy().contains("exists.txt"));
        assert!(e.status_message().unwrap().contains("already exists"));

        // `NE` is the synonym and behaves identically.
        e.run_command("NE another.txt");
        assert!(e.active_path().unwrap().to_string_lossy().contains("another.txt"));
    }

    #[test]
    fn tab_in_out_indents_outdents_lines_and_soft_tabs_insert_spaces() {
        // `TAB in` with no selection inserts one indent unit (a tab) at the caret.
        let mut e = ed();
        e.run_command(r#"INSERT "abc""#);
        e.run_command("TF");
        e.run_command("TAB");
        assert_eq!(e.document().text(), "\tabc");
        e.run_command("UNDO");
        assert_eq!(e.document().text(), "abc", "TAB is one undo unit");

        // Multi-line selection: `TAB in` indents every touched line; `TAB out` outdents them.
        let mut e = ed();
        e.run_command(r#"INSERT "one\ntwo\nthree""#);
        e.run_command("TF");
        e.run_command("BF SEL"); // select all three lines
        e.run_command("TAB");
        assert_eq!(e.document().text(), "\tone\n\ttwo\n\tthree");
        e.run_command("TAB out");
        assert_eq!(e.document().text(), "one\ntwo\nthree", "outdent removes the tab");
        // A second outdent with no indentation left is a no-op (nothing to remove).
        e.run_command("TAB out");
        assert_eq!(e.document().text(), "one\ntwo\nthree");

        // Soft tabs: TAB pads to the NEXT tab stop (not a blind `$TB` spaces); outdent removes up to
        // `$TB` spaces.
        let mut e = ed();
        e.run_command("TB 2");
        e.run_command("TABSPACES on");
        e.run_command(r#"INSERT "x""#);
        e.run_command("TF");
        e.run_command("TAB");
        assert_eq!(e.document().text(), "  x"); // caret on a stop (col 0) → a full 2 spaces

        // Caret NOT on a tab stop: pad only to the next one. With TB=2, typing one char puts the
        // caret at column 1, so TAB adds a SINGLE space to reach column 2 — the bug was inserting a
        // full 2 regardless, landing at column 3.
        let mut e = ed();
        e.run_command("TB 2");
        e.run_command("TABSPACES on");
        e.run_command(r#"INSERT "x""#); // caret at column 1
        e.run_command("TAB");
        assert_eq!(e.document().text(), "x "); // one space → column 2

        // With TB=4 and the caret at column 3 (some spaces already present), TAB adds one space to
        // reach column 4, not four.
        let mut e = ed();
        e.run_command("TB 4");
        e.run_command("TABSPACES on");
        e.run_command(r#"INSERT "ab ""#); // 'a','b',' ' → caret at column 3
        e.run_command("TAB");
        assert_eq!(e.document().text(), "ab  "); // one more space → column 4
        // Outdent removes up to $TB(=2) leading spaces even when there are more.
        let mut e = ed();
        e.run_command("TB 4");
        e.run_command(r#"INSERT "      code""#); // 6 leading spaces
        e.run_command("TF");
        e.run_command("TAB out");
        assert_eq!(e.document().text(), "  code"); // removed 4 of the 6
    }

    #[test]
    fn tabindent_auto_indents_on_enter_and_indented_reads_columns() {
        use xyscript_xpl::{Host, Value};
        // Off (default): Enter inserts a plain newline. (LF line endings for a stable assertion.)
        let mut e = ed();
        e.run_command("NEWLINE lf");
        e.run_command(r#"INSERT "    foo""#);
        e.run_command("ENTER");
        e.run_command(r#"INSERT "bar""#);
        assert_eq!(e.document().text(), "    foo\nbar");

        // On: the new line inherits the previous line's leading whitespace.
        let mut e = ed();
        e.run_command("NEWLINE lf");
        e.run_command("TABINDENT on");
        e.run_command(r#"INSERT "    foo""#);
        e.run_command("ENTER");
        e.run_command(r#"INSERT "bar""#);
        assert_eq!(e.document().text(), "    foo\n    bar");

        // Enter mid-line splits and indents the moved remainder.
        let mut e = ed();
        e.run_command("NEWLINE lf");
        e.run_command("TABINDENT on");
        e.run_command(r#"INSERT "    foobar""#);
        e.run_command("TF");
        e.run_command("JMP +7"); // caret after "foo"
        e.run_command("ENTER");
        assert_eq!(e.document().text(), "    foo\n    bar");

        // `$INDENTED` — tabs expand to the next `$TB` stop; leading spaces count 1 each.
        let mut e = ed();
        e.run_command("TB 4");
        e.run_command(r#"INSERT "\t\tcode""#); // two tabs → 8 columns
        assert_eq!(e.host().special("INDENTED"), Some(Value::Int(8)));
        let mut e = ed();
        e.run_command("TB 4");
        e.run_command(r#"INSERT "\t  x""#); // tab(→4) + 2 spaces → 6
        assert_eq!(e.host().special("INDENTED"), Some(Value::Int(6)));

        // `$TABINDENT` readback.
        let mut e = ed();
        assert_eq!(e.host().special("TABINDENT"), Some(Value::Int(0)));
        e.run_command("TABINDENT on");
        assert_eq!(e.host().special("TABINDENT"), Some(Value::Int(1)));
    }

    #[test]
    fn tabspaces_reads_back() {
        use xyscript_xpl::{Host, Value};
        let mut e = ed();
        assert_eq!(e.host().special("TABSPACES"), Some(Value::Int(0)));
        e.run_command("TABSPACES on");
        assert_eq!(e.host().special("TABSPACES"), Some(Value::Int(1)));
        e.run_command("TABSPACES off");
        assert_eq!(e.host().special("TABSPACES"), Some(Value::Int(0)));
    }

    #[test]
    fn wrapindent_reads_back() {
        use xyscript_xpl::{Host, Value};
        let mut e = ed();
        assert_eq!(e.host().special("WRAPINDENT"), Some(Value::Int(0)));
        e.run_command("WRAPINDENT on");
        assert_eq!(e.host().special("WRAPINDENT"), Some(Value::Int(1)));
        e.run_command("WRAPINDENT off");
        assert_eq!(e.host().special("WRAPINDENT"), Some(Value::Int(0)));
    }

    #[test]
    fn typepairs_end_to_end_typed_path_and_readback() {
        use xyscript_xpl::{Host, Value};
        // Off (default): the interactive typed path inserts a bare '('.
        let mut e = ed();
        assert_eq!(e.host().special("TYPEPAIRS"), Some(Value::Int(0)));
        e.insert_str("("); // insert_str = the OS text/IME (TypeText) path
        assert_eq!(e.document().text(), "(");

        // On: auto-close via the typed path, caret between, then over-type the closer.
        let mut e = ed();
        e.run_command("TYPEPAIRS on");
        assert_eq!(e.host().special("TYPEPAIRS"), Some(Value::Int(1)));
        e.insert_str("(");
        assert_eq!(e.document().text(), "()");
        e.insert_str("x");
        assert_eq!(e.document().text(), "(x)");
        e.insert_str(")"); // over-type the ')'
        assert_eq!(e.document().text(), "(x)");
        e.insert_str("y");
        assert_eq!(e.document().text(), "(x)y");

        // Backspace (BD) between an empty pair deletes both members.
        let mut e = ed();
        e.run_command("TYPEPAIRS on");
        e.insert_str("(");
        assert_eq!(e.document().text(), "()");
        e.run_command("BD");
        assert_eq!(e.document().text(), "");

        // The scripted INSERT verb is NEVER auto-paired, even with TYPEPAIRS on.
        let mut e = ed();
        e.run_command("TYPEPAIRS on");
        e.run_command(r#"INSERT "(""#);
        assert_eq!(e.document().text(), "(");
    }

    #[test]
    fn balpairs_matches_nesting_aware_and_reads_back() {
        use crate::text::BalMatch;
        use xyscript_xpl::{Host, Value};
        // Off by default: no match reported even with the caret on a bracket.
        let mut e = ed();
        assert_eq!(e.host().special("BALPAIRS"), Some(Value::Int(0)));
        e.run_command(r#"INSERT "(a[b]c)""#); // offsets: 0'(' 2'[' 4']' 6')'
        e.run_command("TF"); // caret to offset 0, on '('
        assert_eq!(e.bracket_match(), BalMatch::None);

        // On: the outer '(' at 0 matches its partner ')' at 6.
        e.run_command("BALPAIRS on");
        assert_eq!(e.host().special("BALPAIRS"), Some(Value::Int(1)));
        assert_eq!(e.bracket_match(), BalMatch::Matched(0, 6));

        // Nesting-aware: caret on the inner '[' (offset 2) matches ']' at 4, not the outer ')'.
        e.run_command("JMP +2");
        assert_eq!(e.bracket_match(), BalMatch::Matched(2, 4));

        // A lone bracket reports Unmatched (status-line note), not a highlight.
        let mut e = ed();
        e.run_command("BALPAIRS on");
        e.run_command(r#"INSERT "(x""#);
        e.run_command("TF");
        assert_eq!(e.bracket_match(), BalMatch::Unmatched(0, b'('));

        // `BALPAIRSCOLOR` — set / read back / clear-to-follow-selection.
        assert_eq!(e.host().special("BALPAIRSCOLOR"), Some(Value::str("")));
        e.run_command("BALPAIRSCOLOR 10 20 30");
        assert_eq!(e.host().special("BALPAIRSCOLOR"), Some(Value::str("10 20 30")));
        e.run_command("BALPAIRSCOLOR default");
        assert_eq!(e.host().special("BALPAIRSCOLOR"), Some(Value::str("")));
    }

    #[test]
    fn saveldtx_serializes_text_registers_and_round_trips() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.run_command(r#"LDTX A "hello""#);
        e.run_command(r#"LDTX B "line1\nline2""#); // newline → escaped on one line
        e.run_command(r#"LDTX C "q\"x\\y""#); // embedded quote + backslash
        e.run_command(r#"LDSC Z "PR skip""#); // a SCRIPT binder — must NOT appear in SAVELDTX
        e.run_command("SAVELDTX regs.xpl");
        let saved = e.host().files.get("regs.xpl").cloned().unwrap_or_default();
        // Sorted, escaped, one LDTX line per non-empty text register; no LDSC.
        assert!(saved.contains(r#"LDTX A "hello""#), "A: {saved:?}");
        assert!(saved.contains(r#"LDTX B "line1\nline2""#), "B: {saved:?}");
        assert!(saved.contains(r#"LDTX C "q\"x\\y""#), "C: {saved:?}");
        assert!(!saved.contains("LDSC"), "script binders leaked: {saved:?}");
        // Round-trip: run the saved program into a fresh editor, then `@C` inserts the exact value.
        let mut e2 = ed();
        e2.host_mut().virtual_fs = true;
        for line in saved.lines() {
            e2.run_command(line);
        }
        e2.run_command("@C");
        assert_eq!(e2.document().text(), "q\"x\\y"); // the quote + backslash survived the round-trip
    }

    #[test]
    fn saveldsc_serializes_script_registers_and_round_trips() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.run_command(r#"LDSC A "PR hi""#);
        e.run_command(r#"LDPM B "SE \"x\"""#); // LDPM synonym; embedded quotes in the XPL
        e.run_command(r#"LDTX Z "text""#); // a TEXT binder — must NOT appear in SAVELDSC
        e.run_command("SAVELDSC scripts.xpl");
        let saved = e.host().files.get("scripts.xpl").cloned().unwrap_or_default();
        assert!(saved.contains(r#"LDSC A "PR hi""#), "A: {saved:?}");
        assert!(saved.contains(r#"LDSC B "SE \"x\"""#), "B: {saved:?}");
        assert!(!saved.contains("LDTX"), "text binders leaked into SAVELDSC: {saved:?}");
        // Round-trip: run the saved program into a fresh editor, re-save, and the output is identical
        // (the script registers were restored byte-exact).
        let mut e2 = ed();
        e2.host_mut().virtual_fs = true;
        for line in saved.lines() {
            e2.run_command(line);
        }
        e2.run_command("SAVELDSC scripts2.xpl");
        let saved2 = e2.host().files.get("scripts2.xpl").cloned().unwrap_or_default();
        assert_eq!(saved, saved2, "reload → re-save should reproduce the same program");
    }

    #[test]
    fn open_and_read_with_h_v_open_into_a_split_pane() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("a.txt".into(), "AAA".into());
        e.host_mut().files.insert("b.txt".into(), "BBB".into());
        assert_eq!(e.host().pane_count(), 1);
        // OPEN/V splits the pane and opens a.txt in the new (focused) pane.
        e.run_command("OPEN/V a.txt");
        assert_eq!(e.host().pane_count(), 2);
        assert_eq!(e.document().text(), "AAA"); // the focused pane shows the opened file
        assert_eq!(e.read_only(), false);
        // READ/H splits again and reads b.txt (read-only) into a third pane.
        e.run_command("READ/H b.txt");
        assert_eq!(e.host().pane_count(), 3);
        assert_eq!(e.document().text(), "BBB");
        assert!(e.read_only()); // READ opens locked
    }

    #[test]
    fn open_already_open_prompts_and_resolves() {
        use crate::host::OpenAnswer;
        let setup = || {
            let mut e = ed();
            e.host_mut().virtual_fs = true;
            e.host_mut().files.insert("a.txt".into(), "AAA".into());
            e.host_mut().files.insert("b.txt".into(), "BBB".into());
            e.run_command("OPEN a.txt"); // fills the scratch → tab for a.txt
            e.run_command("OPEN b.txt"); // a second tab (active), so a.txt is "another tab"
            e
        };

        // A file that isn't open yet just opens — no prompt.
        let mut e = setup();
        e.run_command("OPEN c.txt");
        assert!(e.open_prompt().is_none());

        // Re-opening an already-open file prompts and does NOT add a tab yet.
        let mut e = setup();
        let n = e.buffer_count();
        e.run_command("OPEN a.txt");
        assert!(e.open_prompt().unwrap().contains("already open"));
        assert_eq!(e.buffer_count(), n);

        // (G)o → switch to the existing tab, no new tab.
        e.resolve_open(OpenAnswer::Go);
        assert!(e.open_prompt().is_none());
        assert_eq!(e.buffer_count(), n);
        assert!(e.active_path().unwrap().to_string_lossy().contains("a.txt"));

        // (Y)es → a duplicate (writable) tab.
        let mut e = setup();
        let n = e.buffer_count();
        e.run_command("OPEN a.txt");
        e.resolve_open(OpenAnswer::Yes);
        assert_eq!(e.buffer_count(), n + 1);
        assert!(!e.read_only());

        // (R)ead → a duplicate read-only tab.
        let mut e = setup();
        let n = e.buffer_count();
        e.run_command("OPEN a.txt");
        e.resolve_open(OpenAnswer::Read);
        assert_eq!(e.buffer_count(), n + 1);
        assert!(e.read_only());

        // (N)o → nothing happens.
        let mut e = setup();
        let n = e.buffer_count();
        e.run_command("OPEN a.txt");
        e.resolve_open(OpenAnswer::No);
        assert_eq!(e.buffer_count(), n);

        // READ of an already-open file does NOT prompt (XyWrite parity): it opens a fresh
        // read-only copy outright, so you can hold several views of the same file.
        let mut e = setup();
        let n = e.buffer_count();
        e.run_command("READ a.txt");
        assert!(e.open_prompt().is_none(), "READ must not prompt on an already-open file");
        assert_eq!(e.buffer_count(), n + 1, "READ opens a duplicate tab");
        assert!(e.read_only()); // the duplicate is read-only

        // `/NV` skips the prompt entirely: OPEN/NV and READ/NV open a fresh tab outright.
        let mut e = setup();
        let n = e.buffer_count();
        e.run_command("OPEN/NV a.txt");
        assert!(e.open_prompt().is_none(), "/NV must not prompt");
        assert_eq!(e.buffer_count(), n + 1);
        assert!(!e.read_only());
        e.run_command("READ/NV a.txt");
        assert!(e.open_prompt().is_none());
        assert_eq!(e.buffer_count(), n + 2);
        assert!(e.read_only());

        // The OPEN synonyms honor `/NV` identically (shared dispatch): each opens a fresh,
        // editable duplicate with no prompt.
        for verb in ["CA", "ED", "EDIT"] {
            let mut e = setup();
            let n = e.buffer_count();
            e.run_command(&format!("{verb}/NV a.txt"));
            assert!(e.open_prompt().is_none(), "{verb}/NV must not prompt");
            assert_eq!(e.buffer_count(), n + 1, "{verb}/NV opens a duplicate tab");
            assert!(!e.read_only(), "{verb}/NV opens editable");
        }
    }

    #[test]
    fn gotoopen_toggles_reads_back_and_bypasses_the_prompt() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("a.txt".into(), "AAA".into());
        e.host_mut().files.insert("b.txt".into(), "BBB".into());

        // Default off; toggles + reads back via $GOTOOPEN.
        assert!(!e.host().goto_open());
        e.run_command("PR $GOTOOPEN");
        assert_eq!(e.output().last().map(String::as_str), Some("0"));
        e.run_command("GOTOOPEN on");
        assert!(e.host().goto_open());
        e.run_command("PR $GOTOOPEN");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
        e.run_command("GOTOOPEN"); // bare = toggle
        assert!(!e.host().goto_open());

        // With GOTOOPEN on, re-opening an already-open file switches to its tab — no prompt, no dup.
        e.run_command("GOTOOPEN on");
        e.run_command("OPEN a.txt");
        e.run_command("OPEN b.txt"); // b.txt active; a.txt is in another tab
        let n = e.buffer_count();
        e.run_command("OPEN a.txt");
        assert!(e.open_prompt().is_none(), "GOTOOPEN on must not prompt");
        assert_eq!(e.buffer_count(), n, "no duplicate tab");
        assert!(e.active_path().unwrap().to_string_lossy().contains("a.txt"), "switched to a.txt");

        // With GOTOOPEN off, the prompt returns (the default behaviour is preserved).
        e.run_command("GOTOOPEN off");
        e.run_command("OPEN b.txt"); // make b.txt active again
        e.run_command("OPEN a.txt");
        assert!(e.open_prompt().unwrap().contains("already open"), "GOTOOPEN off still prompts");
    }

    #[test]
    fn gofile_switches_to_open_tabs_by_path_substring() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        for f in ["alpha.txt", "beta.txt", "alpha.md", "gamma.txt"] {
            e.host_mut().files.insert(f.into(), "x".into());
        }
        // Four tabs: 0=alpha.txt, 1=beta.txt, 2=alpha.md, 3=gamma.txt (the first reuses the scratch).
        for f in ["alpha.txt", "beta.txt", "alpha.md", "gamma.txt"] {
            e.run_command(&format!("OPEN {f}"));
        }
        assert_eq!(e.buffer_count(), 4);
        assert!(e.active_path().unwrap().to_string_lossy().contains("gamma.txt"));

        // A substring matching one tab switches to it (GOF is the alias).
        e.run_command("GOFILE beta");
        assert!(e.active_path().unwrap().to_string_lossy().contains("beta.txt"));
        e.run_command("GOF gamma");
        assert!(e.active_path().unwrap().to_string_lossy().contains("gamma.txt"));

        // "alpha" matches two tabs; repeated GOFILE cycles through them.
        e.run_command("GOFILE alpha");
        let first = e.active_path().unwrap().to_string_lossy().to_string();
        assert!(first.contains("alpha"));
        e.run_command("GOFILE alpha");
        let second = e.active_path().unwrap().to_string_lossy().to_string();
        assert!(second.contains("alpha") && second != first, "cycles to the other copy");
        e.run_command("GOFILE alpha");
        assert_eq!(e.active_path().unwrap().to_string_lossy(), first, "wraps back around");

        // The sole match being the current tab stays put and is NOT an error.
        e.run_command("GOFILE beta");
        let at = e.active_index();
        e.run_command("GOFILE beta");
        assert_eq!(e.active_index(), at, "sole match = current tab → stay");
        e.run_command("PR $ERROR");
        assert_eq!(e.output().last().map(String::as_str), Some("0"));

        // No match → $ERROR, and the active tab is unchanged.
        let before = e.active_index();
        e.run_command("GOFILE nonesuch");
        assert_eq!(e.active_index(), before, "no match leaves the active tab put");
        e.run_command("PR $ERROR");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
    }

    #[test]
    fn read_opens_a_file_read_only_and_ro_toggles_it() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("notes.txt".into(), "hello".into());
        e.run_command("READ notes.txt");
        assert_eq!(e.document().text(), "hello");
        assert!(e.read_only(), "READ should open the file locked");
        assert!(e.active_path().unwrap().to_string_lossy().contains("notes.txt"));
        // Edits are blocked while locked.
        e.run_command("INSERT \"x\"");
        assert_eq!(e.document().text(), "hello"); // unchanged (read-only)
        // RO toggles the lock off; now edits land.
        e.run_command("RO");
        assert!(!e.read_only());
        e.run_command("INSERT \"!\"");
        assert_eq!(e.document().text(), "!hello"); // caret at top after open

        // A missing file errors and doesn't lock the previous doc.
        let mut e2 = ed();
        e2.host_mut().virtual_fs = true;
        e2.run_command("READ nope.txt");
        assert!(!e2.read_only());
        assert!(e2.status_message().unwrap_or("").contains("not found"));
    }

    #[test]
    fn printex_normalizes_output_to_os_newlines() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        // A source with LF (and even a stray CRLF) must be written with OS-appropriate newlines.
        e.run_command("INSERT \"alpha\\nbeta\\r\\ngamma\""); // literal_text → real LF / CRLF in buffer
        e.run_command("PRINTEX out.txt");
        let out = e.host_mut().files.get("out.txt").cloned().unwrap();
        let nl = if cfg!(windows) { "\r\n" } else { "\n" };
        assert_eq!(out, format!("alpha{nl}beta{nl}gamma"));
    }

    #[test]
    fn generated_script_files_use_os_newlines() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.run_command("LoadCM \"alpha\\nbeta\""); // give SAVECM some history
        e.run_command("SAVECFG cfg.cfg");
        e.run_command("SAVECM hist.xpl");
        e.run_command("SAVEKBD keys.kbd");
        let check = |label: &str, s: &str| {
            if cfg!(windows) {
                assert!(s.contains("\r\n"), "{label}: expected CRLF");
                assert!(!s.replace("\r\n", "").contains('\n'), "{label}: bare LF found");
            } else {
                assert!(!s.contains('\r'), "{label}: unexpected CR");
            }
        };
        check("SAVECFG", &e.host_mut().files.get("cfg.cfg").cloned().unwrap());
        check("SAVECM", &e.host_mut().files.get("hist.xpl").cloned().unwrap());
        check("SAVEKBD", &e.host_mut().files.get("keys.kbd").cloned().unwrap());
    }

    #[test]
    fn printex_live_code_accumulates_a_counter_across_fields() {
        let mut e = ed();
        e.run_command("TRUST 1");
        // `\$ctr` keeps `$ctr` literal in the source doc (INSERT would otherwise substitute it).
        e.run_command(r#"INSERT "|set ctr=0|start|set ctr+=1;\$ctr|-|set ctr+=1;\$ctr|""#);
        e.run_command("PRINTEX");
        // `|set ctr=0|` is deleted (statement → nil); each `;$ctr` inserts the running count.
        assert_eq!(e.document().text(), "start1-2");
    }

    #[test]
    fn printex_field_ending_in_a_block_closer_runs_the_block() {
        let mut e = ed();
        e.run_command("TRUST 1");
        // The first field ENDS in `ef` (ENDFOR). PRINTEX must run the whole block, not peel `ef` off
        // as a value (which left the FOR unclosed → parse error). The next field reads the var the
        // loop accumulated, proving it actually ran.
        e.run_command(r#"INSERT "|set g=0;for n in [1,2,3];set g=\$g+\$n;ef;|sum=|\$g|""#);
        e.run_command("PRINTEX");
        assert!(e.last_error().is_none(), "err: {:?}", e.last_error());
        assert_eq!(e.document().text(), "sum=6");
    }

    #[test]
    fn printex_live_code_requires_trust() {
        let mut e = ed();
        e.run_command(r#"INSERT "|set ctr=0|x""#); // no TRUST
        let before = e.buffer_count();
        e.run_command("PRINTEX");
        assert!(e.last_error().unwrap_or_default().contains("TRUST"));
        assert_eq!(e.buffer_count(), before, "no output tab on a refused live-code doc");
    }

    #[test]
    fn printex_substitution_only_needs_no_trust() {
        let mut e = ed();
        e.run_command(r#"INSERT "code |\$(CHR(65))|""#); // no TRUST; pure substitution
        e.run_command("PRINTEX");
        assert_eq!(e.document().text(), "code A");
    }

    #[test]
    fn printex_counter_fields_advance_and_render() {
        let mut e = ed();
        // A `|Cn …|` field is a third PRINTEX field kind: it advances and renders a `C1..C9`
        // register, with the full `Cn` grammar — and needs NO trust (it's bounded, unlike live code).
        // `|C1 1|` seeds (emits nothing); the FIRST bare `|C1|` emits 1 (put-then-advance), not 2.
        e.run_command(r#"INSERT "|C1 1|a=|C1| b=|C1| c=|C1| rel=|C1 +2| same=|C1 =|""#);
        e.run_command("PRINTEX"); // no TRUST
        assert!(e.last_error().is_none(), "err: {:?}", e.last_error());
        assert_eq!(e.document().text(), "a=1 b=2 c=3 rel=5 same=5");
    }

    #[test]
    fn printex_counter_fields_are_idempotent_and_leave_the_template_alone() {
        let mut e = ed();
        // A SEEDLESS template relies on the snapshot/restore bracket (not re-seeding) to renumber.
        e.run_command(r#"INSERT "a=|C1| b=|C1|""#);
        e.run_command("PRINTEX");
        assert_eq!(e.document().text(), "a=1 b=2"); // run 1 → output tab is active
        // Back to the template: its own live counter was restored (peek 1, not advanced to 3)…
        e.run_command("BUFGO 1");
        e.run_command("PR $C1");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
        // …so a second run produces identical output rather than continuing 3,4.
        e.run_command("PRINTEX");
        assert_eq!(e.document().text(), "a=1 b=2");
    }

    #[test]
    fn printex_dashes_stay_literal_and_template_vars_do_not_leak() {
        let mut e = ed();
        e.run_command("TRUST 1");
        e.run_command("SET y = 2026");
        // `|$y-$m|` is a substitution (string), NOT the expression 2026 - 6; and `tmp` set in a
        // live field is template-local — gone after PRINTEX finishes.
        e.run_command(r#"INSERT "|set m=6|\$y-\$m = |\$y-\$m| tmp=|set tmp=99;\$tmp|""#);
        e.run_command("PRINTEX");
        assert_eq!(e.document().text(), "$y-$m = 2026-6 tmp=99");
        e.run_command("PR $tmp");
        assert_eq!(e.output().last().map(String::as_str), Some("")); // template-local, cleared
        e.run_command("PR $m");
        assert_eq!(e.output().last().map(String::as_str), Some("")); // `m` set in-template, not leaked
    }

    #[test]
    fn printex_no_file_opens_a_new_unnamed_tab() {
        let mut e = ed();
        e.run_command("SET name = \"Ada\"");
        e.run_command("INSERT \"Hi |\\$name|!\"");
        assert_eq!(e.buffer_count(), 1);
        // No file argument → the expanded text goes into a new, unnamed tab (no file written).
        e.run_command("PRINTEX");
        assert_eq!(e.buffer_count(), 2);
        assert_eq!(e.document().text(), "Hi Ada!");
        assert!(e.active_path().is_none(), "the output tab is unnamed");
    }

    #[test]
    fn put_inserts_substituted_text_at_the_caret_respecting_focus() {
        let mut e = ed();
        // Into the document (default focus): PUT inserts the $-substituted arg at the caret.
        e.run_command("SET name = \"Ada\"");
        e.run_command("PUT Hello $name");
        assert_eq!(e.document().text(), "Hello Ada");

        // Into the CMline when it has focus — the document is left untouched.
        e.run_command("GH"); // focus the command line
        assert!(e.cmline_open());
        e.run_command("PUT abc");
        assert_eq!(e.command_line().text(), "abc");
        assert_eq!(e.document().text(), "Hello Ada");
    }

    #[test]
    fn search_count_nth_find_and_capped_replace() {
        let mut e = ed();
        e.run_command("INSERT \"a a a a a\""); // five "a" at offsets 0,2,4,6,8
        e.run_command("TF");
        // SE/3 lands on the 3rd "a" (4..5), caret on its end edge.
        e.run_command("SE/3 /a/");
        assert_eq!(e.document().cursor(), 5);
        // SE/9 — only five "a", so it errors ($ERROR) and the caret returns to where it started.
        e.run_command("TF");
        e.run_command("SE/9 /a/");
        assert_eq!(e.document().cursor(), 0, "failed SE/n leaves the caret put");
        e.run_command("PR $ERROR");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
        // CH/2 changes at most two.
        e.run_command("TF");
        e.run_command("CH/2 /a/X/");
        assert_eq!(e.document().text(), "X X a a a");
        // CH/9 over the three remaining changes all three — fewer than n is fine.
        e.run_command("TF");
        e.run_command("CH/9 /a/Y/");
        assert_eq!(e.document().text(), "X X Y Y Y");
        // CH/n with no matches is zero changes and NO error (a capped replace is best-effort).
        e.run_command("CH/2 /zzz/Q/");
        e.run_command("PR $ERROR");
        assert_eq!(e.output().last().map(String::as_str), Some("0"));
        assert_eq!(e.document().text(), "X X Y Y Y");
    }

    #[test]
    fn count_switch_applies_to_case_and_alias_change_variants() {
        // The `/n` cap is parsed independently of the verb, so it must hold for the
        // case-insensitive aliases (CH/CI) and the case-absolute forms (CHA/CIA) alike.
        let cap2 = |verb: &str| {
            let mut e = ed();
            e.run_command("INSERT \"A a A a A\""); // A@0 a@2 A@4 a@6 A@8
            e.run_command("TF");
            e.run_command(verb);
            e.document().text()
        };
        // Case-insensitive: every letter matches; the first two → X.
        assert_eq!(cap2("CH/2 /a/X/"), "X X A a A");
        assert_eq!(cap2("CI/2 /a/X/"), "X X A a A");
        // Case-absolute: only the uppercase A's match; the first two → X.
        assert_eq!(cap2("CHA/2 /A/X/"), "X a X a A");
        assert_eq!(cap2("CIA/2 /A/X/"), "X a X a A");
    }

    #[test]
    fn change_keeps_the_caret_put_and_reports_the_count() {
        let mut e = ed();
        e.run_command("INSERT \"x a a a\""); // x@0 a@2 a@4 a@6
        e.run_command("TF"); // caret to the top (offset 0)
        // CH replaces from the cursor forward; the caret must stay at the command point, NOT jump to
        // the last change, and the tally lands on the status line.
        e.run_command("CH /a/Z/");
        assert_eq!(e.document().text(), "x Z Z Z");
        assert_eq!(e.document().cursor(), 0, "the caret stays at the command point");
        assert_eq!(e.status_message(), Some("3 changes"));
        // A single change reports the singular form.
        e.run_command("TF");
        e.run_command("CH /x/y/");
        assert_eq!(e.status_message(), Some("1 change"));
    }

    #[test]
    fn change_before_the_caret_shifts_it_with_the_text() {
        let mut e = ed();
        e.run_command("INSERT \"aa aa aa\""); // 8 bytes; INSERT leaves the caret at the end (offset 8)
        // `/T` changes from the TOP, so the edits fall BEFORE the caret. Each "aa" → "b" shrinks the
        // text by 1 (3 matches → −3); the caret must shift left with it and rest at the new end.
        e.run_command("CH/T /aa/b/");
        assert_eq!(e.document().text(), "b b b"); // 5 bytes
        assert_eq!(e.document().cursor(), 5, "the caret tracked the text shift and stayed at the end");
        assert_eq!(e.status_message(), Some("3 changes"));
    }

    #[test]
    fn change_keep_case_recases_the_replacement_per_match() {
        let mut e = ed();
        e.run_command("INSERT \"alligator ALLIGATOR Alligator aLLIGATOR\"");
        e.run_command("TF");
        // /K recases the replacement to each match's case: lower → lower, UPPER → UPPER, Capitalized →
        // Capitalized; a mixed-case match (`aLLIGATOR`) takes the replacement verbatim.
        e.run_command("CH/K /alligator/crocodile/");
        assert_eq!(e.document().text(), "crocodile CROCODILE Crocodile crocodile");
        assert_eq!(e.status_message(), Some("4 changes"));
    }

    #[test]
    fn change_keep_case_works_with_regex() {
        let mut e = ed();
        e.run_command("INSERT \"Foobar FOOBAR foobar\"");
        e.run_command("TF");
        // Regex /K: the expanded result is recased to the whole match's case.
        e.run_command("CH/KR /foo\\w+/baz/");
        assert_eq!(e.document().text(), "Baz BAZ baz");
    }

    #[test]
    fn keep_case_is_rejected_on_a_plain_search() {
        let mut e = ed();
        e.run_command("INSERT \"text\"");
        // `/K` has nothing to recase in a find — a usage error, not a silent no-op.
        e.run_command("SE/K /x/");
        assert!(e.last_error().is_some_and(|m| m.contains("/K")), "SE/K should be rejected: {:?}", e.last_error());
    }

    #[test]
    fn cv_count_stops_after_n_changes() {
        use crate::host::VerifyChoice;
        let mut e = ed();
        e.run_command("INSERT \"a a a a\""); // "a" at offsets 0,2,4,6
        e.run_command("TF");
        e.run_command("CV/2 /a/X/");
        assert!(e.verify_active());
        e.verify_respond(VerifyChoice::Yes); // change a@0 — 1st of 2
        assert!(e.verify_active(), "more to come");
        e.verify_respond(VerifyChoice::No); // skip a@2 — a No does NOT count toward the cap
        assert!(e.verify_active(), "a No didn't reach the cap");
        e.verify_respond(VerifyChoice::Yes); // change a@4 — 2nd change → cap reached, walk ends
        assert!(!e.verify_active(), "CV/2 stops after the 2nd change");
        assert_eq!(e.document().text(), "X a X a");
    }

    #[test]
    fn cmplace_sets_and_reports_and_persists() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        assert_eq!(e.special_str("CMPLACE"), "top"); // default
        e.run_command("CMPLACE bottom");
        assert_eq!(e.special_str("CMPLACE"), "bottom");
        // Bare CMPLACE reports without changing it.
        e.run_command("CMPLACE");
        assert_eq!(e.special_str("CMPLACE"), "bottom");
        // A bad value errors (consistent with DOCTYPE/CRSHAPE) and leaves it unchanged.
        e.run_command("CMPLACE sideways");
        assert!(e.last_error().is_some(), "bad CMPLACE value should error");
        assert_eq!(e.special_str("CMPLACE"), "bottom");
        // $CMPLACE is read-only.
        e.run_command("SET CMPLACE = top");
        assert!(e.last_error().is_some(), "CMPLACE is a read-only special");
        // SAVECFG captures it; a fresh editor that loads the config picks it up.
        e.run_command("CMPLACE hidden");
        e.run_command("SAVECFG cfg.cfg");
        let cfg = e.host_mut().files.get("cfg.cfg").unwrap().clone();
        assert!(cfg.contains("CMPLACE hidden"), "config should carry the placement: {cfg}");
    }

    #[test]
    fn xc_snapshots_the_cmline_so_xc_then_bc_runs_before_blanking() {
        // The F9 = `XC;BC;GT` case: XC captures the command at invocation, so the trailing BC blanks
        // the line only AFTER the command has run — not before.
        let mut e = ed();
        e.open_cmdline();
        e.insert_str("INSERT \"XX\"");
        e.run_command("XC;BC;GT"); // the raw key-chain path
        assert_eq!(e.document().text(), "XX", "XC ran the snapshotted command, not the blanked line");
        assert_eq!(e.command_line().text(), "", "BC blanked the cmline after XC ran");
        assert!(!e.cmline_open(), "GT focused the text");
    }

    #[test]
    fn cmblank_blanks_the_command_line_after_a_command_runs() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        assert_eq!(e.special_str("CMBLANK"), "0"); // default off (keep, B23)
        // On: submitting a command from the cmline blanks it afterward.
        e.run_command("CMBLANK on");
        assert_eq!(e.special_str("CMBLANK"), "1");
        e.open_cmdline();
        e.insert_str("PR hi");
        e.submit_cmdline();
        assert_eq!(e.command_line().text(), "", "CMBLANK on blanks after submit");
        // Off: the command is kept for re-running.
        e.run_command("CMBLANK off");
        e.open_cmdline();
        e.insert_str("PR ho");
        e.submit_cmdline();
        assert_eq!(e.command_line().text(), "PR ho", "CMBLANK off keeps the command");
        // Bad value errors; read-only special; SaveCfg carries it.
        e.run_command("CMBLANK maybe");
        assert!(e.last_error().is_some());
        e.run_command("SET CMBLANK = 1");
        assert!(e.last_error().is_some(), "CMBLANK is read-only");
        e.run_command("CMBLANK on");
        e.run_command("SAVECFG cmb.cfg");
        let cfg = e.host_mut().files.get("cmb.cfg").unwrap().clone();
        assert!(cfg.contains("CMBLANK on"), "config carries CMBLANK: {cfg}");
    }

    #[test]
    fn force_blank_verbs_are_recognized() {
        // AB / CD / ST (with or without a switch) and a bare drive-letter change.
        for c in ["AB", "ab/v", "CD foo", "cd/d x", "ST", "st/nv a.txt", "D:", "  x:\\dir "] {
            assert!(cmd_forces_cmline_blank(c), "should force-blank: {c:?}");
        }
        // Everything else keeps the command (subject to the global CMBLANK).
        for c in ["PR hi", "SET x = 1", "SAVE", "SA", "CDX", "STORE", "ABDUCT"] {
            assert!(!cmd_forces_cmline_blank(c), "should NOT force-blank: {c:?}");
        }
    }

    #[test]
    fn action_verbs_blank_the_cmline_even_with_cmblank_off() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        assert_eq!(e.special_str("CMBLANK"), "0"); // default off (keep the command)
        // XyWrite parity: CD blanks the cmline after running from it, regardless of CMBLANK.
        e.open_cmdline();
        e.insert_str("CD .");
        e.submit_cmdline();
        assert_eq!(e.command_line().text(), "", "CD blanks the cmline");
        // A bare drive-letter change (parser rewrites `Q:` → `CD Q:`) blanks too — even though the
        // drive doesn't exist here and CD errors, the blank still fires.
        e.open_cmdline();
        e.insert_str("Q:");
        e.submit_cmdline();
        assert_eq!(e.command_line().text(), "", "drive change blanks the cmline");
        // A non-action command is still kept (CMBLANK is off).
        e.open_cmdline();
        e.insert_str("PR hi");
        e.submit_cmdline();
        assert_eq!(e.command_line().text(), "PR hi", "PR is retained with CMBLANK off");
    }

    #[test]
    fn cmline_clipboard_ops_target_the_command_line_when_focused() {
        // Mouse drag-select on the cmline sets the cmline BUFFER's selection; CUT/COPY/PASTE then
        // act on it because the host's `active()` follows focus (target == CmLine).
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.open_cmdline();
        e.insert_str("hello world");
        e.select_cmline(5, 0); // select "hello" (the drag-select path: cursor 5, anchor 0)
        assert!(e.command_line().has_selection());
        e.run_command("COPY");
        assert_eq!(e.clipboard(), "hello", "COPY reads the cmline selection");
        e.run_command("CUT");
        assert_eq!(e.command_line().text(), " world", "CUT removes it from the cmline");
        e.run_command("PASTE");
        assert_eq!(e.command_line().text(), "hello world", "PASTE inserts at the cmline caret");
    }

    #[test]
    fn cmline_define_word_selects_on_the_command_line() {
        // The cmline's right-click "Select word/line" uses the DIRECT define methods, NOT the
        // DW/DL verbs: those are FOCUS_DOC_VERBS, so run from the cmline they'd shift focus and
        // define in the DOCUMENT (asserted below) — wrong for a cmline menu.
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.open_cmdline();
        e.insert_str("hello world");
        e.click_cmline(2); // caret inside "hello"
        // Direct method → selection lands on the cmline.
        e.cmline_define_word();
        assert!(e.command_line().has_selection());
        assert_eq!(e.command_line().selection_text(), "hello");
        e.cmline_define_line(); // whole cmline line
        assert_eq!(e.command_line().selection_text(), "hello world");
        // Contrast: the DW verb is focus-doc — it defines in the document, not the cmline.
        let mut e2 = ed();
        e2.host_mut().virtual_fs = true;
        e2.run_command(r#"INSERT "abc def""#); // into the document
        e2.run_command("TF"); // top of file, caret in "abc"
        e2.run_command("DW");
        assert!(e2.document().has_selection(), "DW verb defines in the document");
    }

    #[test]
    fn fgcolor_on_selection_makes_a_transient_span_that_tracks_edits() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.run_command(r#"INSERT "hello world""#);
        e.run_command("TF"); // top of file; caret in "hello"
        e.run_command("DW"); // define (select) "hello"
        assert!(e.document().has_selection());
        e.run_command("FGCOLOR 255 0 0");
        assert_eq!(e.color_spans(), vec![(0, 5, (255, 0, 0))], "colours the selection [0,5)");
        // Insert 3 chars at the very start → the span shifts right (the new text is NOT coloured).
        e.run_command("TF");
        e.run_command(r#"INSERT "XYZ""#);
        assert_eq!(e.color_spans(), vec![(3, 8, (255, 0, 0))], "span follows the edit before it");
        // Type inside the coloured run → it grows (caret is at 3 after the insert; +2 → 5, inside).
        e.run_command("JMP +2");
        e.run_command(r#"INSERT "!""#);
        assert_eq!(e.color_spans(), vec![(3, 9, (255, 0, 0))], "insert inside grows the span");
        // Undo the last insert → the span reverts; redo → it returns. (Tracks undo/redo.)
        e.run_command("UNDO");
        assert_eq!(e.color_spans(), vec![(3, 8, (255, 0, 0))], "undo re-tracks the span");
        e.run_command("REDO");
        assert_eq!(e.color_spans(), vec![(3, 9, (255, 0, 0))], "redo re-tracks the span");
        // `FGCOLOR default` with NO selection is a no-op (selection-only).
        e.run_command("XD"); // cancel any selection
        e.run_command("FGCOLOR default");
        assert_eq!(e.color_spans(), vec![(3, 9, (255, 0, 0))], "default with no selection is a no-op");
        // Over a covering selection it clears.
        e.run_command("DFA");
        e.run_command("FGCOLOR default");
        assert!(e.color_spans().is_empty(), "FGCOLOR default over a selection clears");
        // A concrete colour with no selection still sets the GLOBAL foreground (backward-compatible).
        e.run_command("XD");
        e.run_command("FGCOLOR 1 2 3");
        assert_eq!(e.fg_color(), (1, 2, 3));
        assert!(e.color_spans().is_empty(), "no selection → global, no span");
    }

    #[test]
    fn mda_paints_persistent_fg_and_bg_block_spans_that_track_edits() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.run_command(r#"INSERT "hello world""#);
        e.run_command("TF"); // top of file; caret in "hello"
        e.run_command("DW"); // define (select) "hello"
        assert!(e.document().has_selection());
        // `MDA/B` paints the BACKGROUND layer; the foreground layer stays empty.
        e.run_command("MDA/B 0 0 255");
        assert_eq!(e.bg_color_spans(), vec![(0, 5, (0, 0, 255))], "MDA/B colours the selection bg");
        assert!(e.color_spans().is_empty(), "bg layer is independent of the fg layer");
        // A plain `MDA` paints the FOREGROUND layer over the same run — the two coexist.
        e.run_command("TF");
        e.run_command("DW"); // re-select "hello"
        e.run_command("MDA 255 0 0");
        assert_eq!(e.color_spans(), vec![(0, 5, (255, 0, 0))], "MDA colours the selection fg");
        assert_eq!(e.bg_color_spans(), vec![(0, 5, (0, 0, 255))], "bg span still stands");
        // Insert at the very start → BOTH layers shift right (the new text is uncoloured).
        e.run_command("TF");
        e.run_command(r#"INSERT "XYZ""#);
        assert_eq!(e.color_spans(), vec![(3, 8, (255, 0, 0))], "fg span follows the edit");
        assert_eq!(e.bg_color_spans(), vec![(3, 8, (0, 0, 255))], "bg span follows the edit");
        // `MDA/B off` clears just the background layer, leaving the foreground.
        e.run_command("DFA");
        e.run_command("MDA/B off");
        assert!(e.bg_color_spans().is_empty(), "MDA/B off clears the bg layer");
        assert_eq!(e.color_spans(), vec![(3, 8, (255, 0, 0))], "fg layer untouched");
        // Bare `MDA` over the covering selection clears the foreground layer too.
        e.run_command("MDA");
        assert!(e.color_spans().is_empty(), "bare MDA over a selection clears the fg layer");
    }

    #[test]
    fn mda_colours_persist_across_save_and_reopen_via_sidecar() {
        // Real filesystem (NOT the virtual FS) — the sidecar is a genuine hidden companion file.
        let base = std::env::temp_dir().join(format!("xyscript-xymeta-{:p}", &"mda"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        // Session 1: type, colour a block (fg + bg), save.
        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("NEW doc.txt"); // fresh named buffer, ready to SAVE
        e.run_command(r#"INSERT "hello world""#);
        e.run_command("TF");
        e.run_command("DW"); // select "hello"
        e.run_command("MDA 255 0 0"); // foreground
        e.run_command("TF");
        e.run_command("DW");
        e.run_command("MDA/B 0 0 255"); // background
        e.run_command("SAVE");
        let sidecar = base.join(".doc.txt.xymeta");
        assert!(sidecar.exists(), "a hidden .xymeta sidecar is written next to the file");

        // Session 2: reopen — both colour layers come back.
        let mut e2 = ed();
        e2.run_command(&format!("CD {}", base.display()));
        e2.run_command("OPEN doc.txt");
        assert_eq!(e2.color_spans(), vec![(0, 5, (255, 0, 0))], "fg block restored on reopen");
        assert_eq!(e2.bg_color_spans(), vec![(0, 5, (0, 0, 255))], "bg block restored on reopen");

        // Guard: an out-of-band edit changes the bytes → the content guard fails → colours dropped
        // (the offsets would be meaningless against different content).
        std::fs::write(base.join("doc.txt"), b"totally different content here").unwrap();
        let mut e3 = ed();
        e3.run_command(&format!("CD {}", base.display()));
        e3.run_command("OPEN doc.txt");
        assert!(e3.color_spans().is_empty(), "stale sidecar ignored after an out-of-band edit");
        assert!(e3.bg_color_spans().is_empty(), "stale sidecar ignored (bg layer)");

        // Clearing every colour and re-saving removes the sidecar (no orphan metadata).
        e2.run_command("DFA");
        e2.run_command("MDA off");
        e2.run_command("DFA");
        e2.run_command("MDA/B off");
        e2.run_command("SAVE");
        assert!(!sidecar.exists(), "the sidecar is removed once the document has no colours");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn mda_text_attributes_track_edits_and_are_independent_of_colours() {
        use crate::document::{ATTR_BOLD, ATTR_ITALIC, ATTR_REVERSE, ATTR_UNDERLINE};
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.run_command(r#"INSERT "hello world""#);
        e.run_command("TF");
        e.run_command("DW"); // select "hello"
        // `MDA BI` = bold + italic over the selection.
        e.run_command("MDA BI");
        assert_eq!(
            e.attr_spans(),
            vec![(0, 5, ATTR_BOLD | ATTR_ITALIC)],
            "MDA BI sets bold+italic over the selection"
        );
        // A colour on the SAME run is an independent layer — both coexist.
        e.run_command("TF");
        e.run_command("DW");
        e.run_command("MDA 255 0 0");
        assert_eq!(e.color_spans(), vec![(0, 5, (255, 0, 0))], "colour layer independent of attrs");
        assert_eq!(e.attr_spans(), vec![(0, 5, ATTR_BOLD | ATTR_ITALIC)], "attrs untouched by colour");
        // Insert at the very start → the attribute span follows the edit (like colours).
        e.run_command("TF");
        e.run_command(r#"INSERT "XYZ""#);
        assert_eq!(e.attr_spans(), vec![(3, 8, ATTR_BOLD | ATTR_ITALIC)], "attr span follows the edit");
        // `MDA UL` REPLACES the attribute set over the covering selection (not additive).
        e.run_command("DFA");
        e.run_command("MDA UL");
        assert_eq!(e.attr_spans(), vec![(0, 14, ATTR_UNDERLINE)], "MDA UL replaces the attribute set");
        // `MDA RV` (reverse) is just another attribute bit — it SETS (replaces) the region's attrs.
        e.run_command("DFA");
        e.run_command("MDA RV");
        assert_eq!(e.attr_spans(), vec![(0, 14, ATTR_REVERSE)], "MDA RV sets the reverse bit");
        // `MDA NM` (normal) strips attributes over the selection; the colour layer is untouched.
        e.run_command("DFA");
        e.run_command("MDA NM");
        assert!(e.attr_spans().is_empty(), "MDA NM clears the attribute layer");
        assert!(!e.color_spans().is_empty(), "MDA NM leaves the colour layer alone");
    }

    #[test]
    fn mda_attributes_persist_across_save_and_reopen_via_sidecar() {
        use crate::document::{ATTR_BOLD, ATTR_UNDERLINE};
        let base = std::env::temp_dir().join(format!("xyscript-xymeta-attr-{:p}", &"mda"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("NEW doc.txt");
        e.run_command(r#"INSERT "hello world""#);
        e.run_command("TF");
        e.run_command("DW");
        e.run_command("MDA BU"); // bold + underline
        e.run_command("SAVE");
        let sidecar = base.join(".doc.txt.xymeta");
        assert!(sidecar.exists(), "sidecar written for an attribute-only document");

        let mut e2 = ed();
        e2.run_command(&format!("CD {}", base.display()));
        e2.run_command("OPEN doc.txt");
        assert_eq!(
            e2.attr_spans(),
            vec![(0, 5, ATTR_BOLD | ATTR_UNDERLINE)],
            "attributes restored from the sidecar on reopen"
        );
        assert!(!e2.modified(), "a freshly-opened styled doc matches disk — not modified");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn deleting_a_file_removes_its_xymeta_sidecar() {
        let base = std::env::temp_dir().join(format!("xyscript-xymeta-del-{:p}", &"del"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("NEW doc.txt");
        e.run_command(r#"INSERT "hello world""#);
        e.run_command("TF");
        e.run_command("DW");
        e.run_command("MDA BO");
        e.run_command("SAVE");
        let sidecar = base.join(".doc.txt.xymeta");
        assert!(sidecar.exists(), "sidecar written for the styled doc");

        // `ERNV` (erase, no verify) deletes the file immediately — the sidecar must go with it.
        e.run_command("ERNV doc.txt");
        assert!(!base.join("doc.txt").exists(), "the file was deleted");
        assert!(!sidecar.exists(), "the .xymeta sidecar is deleted alongside the file");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn renaming_a_file_moves_its_xymeta_sidecar() {
        let base = std::env::temp_dir().join(format!("xyscript-xymeta-ren-{:p}", &"ren"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("NEW old.txt");
        e.run_command(r#"INSERT "hello world""#);
        e.run_command("TF");
        e.run_command("DW");
        e.run_command("MDA UL");
        e.run_command("SAVE");
        assert!(base.join(".old.txt.xymeta").exists(), "sidecar written under the old name");

        e.run_command("RENAME old.txt new.txt");
        assert!(base.join("new.txt").exists(), "file renamed");
        assert!(!base.join(".old.txt.xymeta").exists(), "no orphan sidecar at the old name");
        assert!(base.join(".new.txt.xymeta").exists(), "sidecar followed the rename");

        // And the styling actually survives: open the renamed file and the underline span is restored.
        let mut e2 = ed();
        e2.run_command(&format!("CD {}", base.display()));
        e2.run_command("OPEN new.txt");
        assert_eq!(
            e2.attr_spans(),
            vec![(0, 5, crate::document::ATTR_UNDERLINE)],
            "MDA styling restored from the moved sidecar"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn session_restore_reapplies_mda_styling_from_the_sidecar() {
        // SESSION on reconstructs docs itself; it must still restore MDA styling from the sidecar, or a
        // restored file shows no colours/attributes until you close and reopen it.
        let base = std::env::temp_dir().join(format!("xyscript-session-mda-{:p}", &"smda"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let sdir = base.join("session");

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("NEW doc.txt");
        e.run_command(r#"INSERT "hello world""#);
        e.run_command("TF");
        e.run_command("DW");
        e.run_command("MDA UL");
        e.run_command("SAVE"); // clean file + sidecar; session records it by PATH
        assert!(base.join(".doc.txt.xymeta").exists(), "sidecar written");
        e.host_mut().save_session(&sdir).unwrap();

        let mut e2 = ed();
        e2.run_command(&format!("CD {}", base.display()));
        let n = e2.host_mut().restore_session(&sdir).unwrap();
        assert!(n >= 1, "the styled tab restored");
        assert_eq!(
            e2.attr_spans(),
            vec![(0, 5, crate::document::ATTR_UNDERLINE)],
            "MDA styling shows on session-restore (was: only after a manual close+reopen)"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn session_restore_preserves_unsaved_mda_styling() {
        // The session snapshot records the LIVE spans, so styling applied but NOT saved (which the
        // on-disk sidecar can't know about) still comes back on restore.
        let base = std::env::temp_dir().join(format!("xyscript-session-unsaved-{:p}", &"su"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let sdir = base.join("session");

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("NEW doc.txt");
        e.run_command(r#"INSERT "hello world""#);
        e.run_command("SAVE"); // save the CONTENT; no styling yet → sidecar has none
        e.run_command("TF");
        e.run_command("DW");
        e.run_command("MDA BO"); // style WITHOUT saving → dirty; sidecar still has no styling
        assert!(e.modified(), "styling made the doc dirty");
        e.host_mut().save_session(&sdir).unwrap();

        let mut e2 = ed();
        e2.run_command(&format!("CD {}", base.display()));
        e2.host_mut().restore_session(&sdir).unwrap();
        assert_eq!(
            e2.attr_spans(),
            vec![(0, 5, crate::document::ATTR_BOLD)],
            "unsaved styling restored from the session snapshot (sidecar couldn't have it)"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn session_restore_keeps_unsaved_cleared_styling_cleared() {
        // A file SAVED with styling, then CLEARED but not re-saved: the session's live (empty) spans
        // must win over the stale sidecar, or restore would wrongly re-add the old styling.
        let base = std::env::temp_dir().join(format!("xyscript-session-cleared-{:p}", &"sc"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let sdir = base.join("session");

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("NEW doc.txt");
        e.run_command(r#"INSERT "hello world""#);
        e.run_command("TF");
        e.run_command("DW");
        e.run_command("MDA UL");
        e.run_command("SAVE"); // sidecar now HAS the underline
        assert!(base.join(".doc.txt.xymeta").exists());
        e.run_command("DFA");
        e.run_command("MDA NM"); // clear the attribute WITHOUT saving → dirty, in-memory empty
        assert!(e.attr_spans().is_empty());
        e.host_mut().save_session(&sdir).unwrap();

        let mut e2 = ed();
        e2.run_command(&format!("CD {}", base.display()));
        e2.host_mut().restore_session(&sdir).unwrap();
        assert!(
            e2.attr_spans().is_empty(),
            "cleared-but-unsaved styling stays cleared — the session's spans win over the stale sidecar"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn mda_styling_marks_the_document_dirty_until_saved() {
        // Styling doesn't ride the undo stack, but it IS persisted, so it must flip the modified flag
        // — otherwise autosave and the save-on-close prompt would silently skip a styling-only change.
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.run_command("NEW doc.txt");
        e.run_command(r#"INSERT "hello world""#);
        e.run_command("SAVE");
        assert!(!e.modified(), "clean baseline right after a save");
        // Apply an attribute with NO text edit → the doc is dirty again.
        e.run_command("TF");
        e.run_command("DW");
        e.run_command("MDA BO");
        assert!(e.modified(), "MDA styling alone marks the doc modified");
        e.run_command("SAVE");
        assert!(!e.modified(), "saving clears it (styling persisted to the sidecar)");
        // A background colour on the selection marks it dirty too.
        e.run_command("TF");
        e.run_command("DW");
        e.run_command("MDA/B 0 0 255");
        assert!(e.modified(), "a background block colour also marks the doc modified");
    }

    #[test]
    fn mda_typing_mode_styles_subsequently_typed_text() {
        use crate::document::{ATTR_BOLD, ATTR_UNDERLINE};
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        // No selection → typing mode. `insert_str` is the interactive (TypeText) path a keystroke takes.
        e.run_command("MDA BU"); // bold + underline typing mode ON
        e.insert_str("hello");
        assert_eq!(
            e.attr_spans(),
            vec![(0, 5, ATTR_BOLD | ATTR_UNDERLINE)],
            "typed text takes the mode's attributes, coalesced into one span"
        );
        // A colour layer can be armed too — it stacks with the attribute mode.
        e.run_command("MDA 255 0 0"); // fg-red typing mode ON (attr mode still on)
        e.insert_str("X");
        assert_eq!(e.color_spans(), vec![(5, 6, (255, 0, 0))], "new text gets the fg colour");
        assert_eq!(
            e.attr_spans(),
            vec![(0, 6, ATTR_BOLD | ATTR_UNDERLINE)],
            "attribute mode still applies and coalesces with the prior run"
        );
        // Bare `MDA` cancels the fg colour AND attribute modes; later text is unstyled.
        e.run_command("MDA");
        e.insert_str("Y");
        assert_eq!(e.color_spans(), vec![(5, 6, (255, 0, 0))], "fg mode off — no new colour span");
        assert_eq!(
            e.attr_spans(),
            vec![(0, 6, ATTR_BOLD | ATTR_UNDERLINE)],
            "attribute mode off — the 'Y' is not styled"
        );
    }

    #[test]
    fn mda_typing_mode_is_per_file() {
        // The live-typing mode belongs to the document, not the session — switching tabs must not carry
        // one file's mode into another.
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.run_command("NEW a.txt");
        e.run_command("MDA BO"); // arm bold typing on a.txt
        assert!(e.mda_typing_active(), "mode is armed on a.txt");

        e.run_command("NEW b.txt"); // switch to a different file
        assert!(!e.mda_typing_active(), "b.txt does not inherit a.txt's mode");
        e.insert_str("plain");
        assert!(e.attr_spans().is_empty(), "text typed in b.txt is unstyled");

        e.run_command("BUFPREV"); // back to a.txt
        assert!(e.mda_typing_active(), "a.txt still has its own mode");
        e.insert_str("X");
        assert!(!e.attr_spans().is_empty(), "typing in a.txt is styled");
    }

    #[test]
    fn mda_typing_mode_styles_kbd_bound_inserts_so_spaces_join_the_run() {
        use crate::document::ATTR_UNDERLINE;
        // A KBD-bound key (e.g. the spacebar, `57=" "`) inserts via the `INSERT`/InsertText path, not
        // the OS-text `TypeText` path — the typing mode must catch it too, or a typed space would leave
        // a gap in the underline. `insert_literal` is that InsertText path.
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.run_command("MDA UL");
        e.insert_str("ab"); // OS-text path (letters)
        e.insert_literal(" "); // KBD-bound space (InsertText path)
        e.insert_str("cd");
        assert_eq!(
            e.attr_spans(),
            vec![(0, 5, ATTR_UNDERLINE)],
            "the KBD-bound space is styled, so the underline run is contiguous across it"
        );
    }

    #[test]
    fn ulspaces_toggles_and_reads_back() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        assert!(e.ul_spaces(), "ULSPACES on by default");
        e.run_command("ULSPACES off");
        assert!(!e.ul_spaces(), "ULSPACES off");
        e.run_command("ULSPACES"); // bare toggles
        assert!(e.ul_spaces(), "bare ULSPACES toggles back on");
    }

    #[test]
    fn df_then_search_extends_the_defined_block() {
        // The classic gesture `SE /a/; DF; SE /b/; DZ` selects from the DF point to the second
        // match — a plain SE while a define is OPEN must GROW the block, not replace it.
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.run_command(r#"INSERT "aaa foo bbb bar ccc""#);
        e.run_command("TF");
        e.run_command("SE /foo/"); // caret lands at 7 (end of "foo"), selecting "foo"
        assert!(e.last_error().is_none(), "SE /foo/ found");
        e.run_command("DF"); // anchor the define at 7
        e.run_command("SE /bar/"); // extend the block to the end of "bar" (15)
        assert!(e.last_error().is_none(), "SE /bar/ found");
        e.run_command("DZ"); // close the block
        assert_eq!(e.document().selection_text(), " bbb bar", "block spans DF-point → 2nd match");
    }

    #[test]
    fn preview_font_settings_round_trip_and_persist() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.run_command("PVFAMILY Georgia");
        e.run_command("PVSIZE 13");
        e.run_command("PVCODE Consolas");
        assert_eq!(e.host().preview_font_family(), Some("Georgia"));
        assert_eq!(e.host().preview_font_size(), Some(13.0));
        assert_eq!(e.host().preview_code_family(), Some("Consolas"));
        // SaveCfg writes the three verbs so the choice restores on load.
        e.run_command("SAVECFG cfg.cfg");
        let cfg = e.host_mut().files.get("cfg.cfg").unwrap().clone();
        assert!(cfg.contains("PVFAMILY Georgia"), "cfg: {cfg}");
        assert!(cfg.contains("PVSIZE 13"), "cfg: {cfg}");
        assert!(cfg.contains("PVCODE Consolas"), "cfg: {cfg}");
        // `default` reverts each to Typst's built-in (None).
        e.run_command("PVFAMILY default");
        e.run_command("PVSIZE default");
        e.run_command("PVCODE default");
        assert_eq!(e.host().preview_font_family(), None);
        assert_eq!(e.host().preview_font_size(), None);
        assert_eq!(e.host().preview_code_family(), None);
    }

    #[test]
    fn random_int_range_seed_and_reproducibility() {
        let e = ed();
        let h = e.host();
        // Inclusive range; swapped args (min > max) tolerated.
        for _ in 0..2000 {
            assert!((1..=6).contains(&h.random_int(1, 6).unwrap()));
            assert!((1..=10).contains(&h.random_int(10, 1).unwrap()));
        }
        assert_eq!(h.random_int(5, 5), Some(5)); // single-value range
        // Seeding is reproducible; a different seed (almost surely) diverges.
        let draw =
            |h: &AppHost| (0..12).map(|_| h.random_int(0, 1_000_000).unwrap()).collect::<Vec<_>>();
        h.seed_random(42);
        let a = draw(h);
        h.seed_random(42);
        assert_eq!(a, draw(h), "same seed → same sequence");
        h.seed_random(43);
        assert_ne!(a, draw(h), "different seed → different sequence");
    }

    #[test]
    fn random_and_srandom_verbs_wire_through_commands() {
        // End-to-end: `SRANDOM` seeds, `RANDOM(min, max)` is a function usable from `$(…)`.
        let rolls = |seed: &str| -> Vec<i64> {
            let mut e = ed();
            e.run_command(&format!("SRANDOM {seed}"));
            for _ in 0..10 {
                e.run_command(r#"INSERT "$(RANDOM(1,6)) ""#);
            }
            e.document().text().split_whitespace().map(|t| t.parse::<i64>().unwrap()).collect()
        };
        let a = rolls("7");
        assert_eq!(a.len(), 10);
        assert!(a.iter().all(|&n| (1..=6).contains(&n)), "all rolls in [1,6]: {a:?}");
        assert_eq!(a, rolls("7"), "SRANDOM makes the RANDOM sequence reproducible");
    }

    #[test]
    fn mouse_drag_selection_is_persistent_under_ps() {
        let mut e = ed();
        e.run_command(r#"insert "hello world""#);
        e.run_command("SPS"); // persistent-selection on
        // Simulate a mouse drag selecting "hello" [0,5): press at 0, then drag to 5.
        e.host_mut().set_active_caret(0, None);
        e.host_mut().set_active_caret(5, Some(0));
        assert_eq!(e.document().selection_span(), Some((0, 5)));
        // A click elsewhere: under PS a DEFINED block is kept (the caret roams), not cleared.
        e.host_mut().click_active_caret(9);
        assert_eq!(
            e.document().selection_span(),
            Some((0, 5)),
            "drag selection persists under PS after a click"
        );
        // Without PS, a drag selection is transient — a click clears it.
        let mut e = ed();
        e.run_command(r#"insert "hello world""#);
        e.host_mut().set_active_caret(0, None);
        e.host_mut().set_active_caret(5, Some(0));
        e.host_mut().click_active_caret(9);
        assert!(!e.document().has_selection(), "no PS: a click clears the drag selection");
    }

    #[test]
    fn showonetab_toggles_reports_and_persists() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        assert_eq!(e.special_str("SHOWONETAB"), "0"); // default off
        assert!(!e.show_one_tab());
        e.run_command("SHOWONETAB on");
        assert_eq!(e.special_str("SHOWONETAB"), "1");
        assert!(e.show_one_tab());
        e.run_command("SHOWONETAB"); // bare toggles
        assert!(!e.show_one_tab());
        // Bad value errors; read-only special; SaveCfg carries it.
        e.run_command("SHOWONETAB maybe");
        assert!(e.last_error().is_some());
        e.run_command("SET SHOWONETAB = 1");
        assert!(e.last_error().is_some(), "SHOWONETAB is read-only");
        e.run_command("SHOWONETAB on");
        e.run_command("SAVECFG sot.cfg");
        let cfg = e.host_mut().files.get("sot.cfg").unwrap().clone();
        assert!(cfg.contains("SHOWONETAB on"), "config carries SHOWONETAB: {cfg}");
    }

    #[test]
    fn wrap_toggles_reports_and_persists() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        assert_eq!(e.special_str("WRAP"), "1"); // default on (wrapping)
        assert!(e.wrap());
        e.run_command("WRAP off");
        assert_eq!(e.special_str("WRAP"), "0");
        assert!(!e.wrap());
        e.run_command("WRAP"); // bare toggles
        assert!(e.wrap());
        // Bad value errors; read-only special; SaveCfg carries it.
        e.run_command("WRAP maybe");
        assert!(e.last_error().is_some());
        e.run_command("SET WRAP = 0");
        assert!(e.last_error().is_some(), "WRAP is read-only");
        e.run_command("WRAP off");
        e.run_command("SAVECFG wrap.cfg");
        let cfg = e.host_mut().files.get("wrap.cfg").unwrap().clone();
        assert!(cfg.contains("WRAP off"), "config carries WRAP: {cfg}");
    }

    #[test]
    fn wraphang_sets_reports_and_persists() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        assert_eq!(e.special_str("WRAPHANG"), "0"); // default 0 (flush wrap)
        assert_eq!(e.wrap_hang(), 0);
        e.run_command("WRAPHANG 4");
        assert_eq!(e.special_str("WRAPHANG"), "4");
        assert_eq!(e.wrap_hang(), 4);
        e.run_command("WRAPHANG 0"); // back to flush
        assert_eq!(e.wrap_hang(), 0);
        // Bad value errors; read-only special; SaveCfg carries it.
        e.run_command("WRAPHANG lots");
        assert!(e.last_error().is_some());
        e.run_command("SET WRAPHANG = 2");
        assert!(e.last_error().is_some(), "WRAPHANG is read-only");
        e.run_command("WRAPHANG 4");
        e.run_command("SAVECFG wh.cfg");
        let cfg = e.host_mut().files.get("wh.cfg").unwrap().clone();
        assert!(cfg.contains("WRAPHANG 4"), "config carries WRAPHANG: {cfg}");
    }

    #[test]
    fn markgutter_toggles_reports_and_persists() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        assert_eq!(e.special_str("MARKGUTTER"), "1"); // default on
        e.run_command("MARKGUTTER off");
        assert_eq!(e.special_str("MARKGUTTER"), "0");
        e.run_command("MARKGUTTER"); // bare toggles → back on
        assert_eq!(e.special_str("MARKGUTTER"), "1");
        e.run_command("MARKGUTTER maybe");
        assert!(e.last_error().is_some(), "bad value errors");
        // SaveCfg carries it.
        e.run_command("MARKGUTTER off");
        e.run_command("SAVECFG mg.cfg");
        let cfg = e.host_mut().files.get("mg.cfg").unwrap().clone();
        assert!(cfg.contains("MARKGUTTER off"), "config carries MARKGUTTER: {cfg}");
    }

    #[test]
    fn untitledtab_toggles_reports_and_persists() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        assert_eq!(e.special_str("UNTITLEDTAB"), "1"); // default on
        e.run_command("UNTITLEDTAB off");
        assert_eq!(e.special_str("UNTITLEDTAB"), "0");
        e.run_command("UNTITLEDTAB"); // bare toggles → back on
        assert_eq!(e.special_str("UNTITLEDTAB"), "1");
        e.run_command("UNTITLEDTAB maybe");
        assert!(e.last_error().is_some(), "bad value errors");
        e.run_command("SET UNTITLEDTAB = 1");
        assert!(e.last_error().is_some(), "UNTITLEDTAB is read-only");
        e.run_command("SAVECFG ut.cfg");
        let cfg = e.host_mut().files.get("ut.cfg").unwrap().clone();
        assert!(cfg.contains("UNTITLEDTAB on"), "config carries UNTITLEDTAB: {cfg}");
    }

    #[test]
    fn untitledtab_on_closes_scratch_and_reappears() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        assert_eq!(e.buffer_count(), 1); // launch: a single blank Untitled scratch
        e.host_mut().write_file("a.txt", "hello").unwrap();
        e.run_command("OPEN a.txt");
        assert_eq!(e.buffer_count(), 1, "the blank Untitled tab is closed on open");
        assert!(e.active_path().unwrap().to_string_lossy().contains("a.txt"));
        // Closing the last document brings an Untitled tab back.
        e.host_mut().editor_op(EditorOp::CloseBuffer).unwrap();
        assert_eq!(e.buffer_count(), 1);
        assert!(e.active_path().is_none(), "Untitled reappears when the last document closes");
    }

    #[test]
    fn untitledtab_prunes_a_leftover_scratch_on_open() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.host_mut().write_file("a.txt", "x").unwrap();
        e.host_mut().write_file("b.txt", "y").unwrap();
        e.run_command("OPEN a.txt"); // reuses the launch scratch → [a]
        e.run_command("NEW"); // [a, Untitled] (Untitled active)
        e.run_command("BUFPREV"); // active = a; a leftover Untitled sits in the other tab
        assert_eq!(e.buffer_count(), 2);
        e.run_command("OPEN b.txt"); // opens b as a new tab AND prunes the leftover Untitled
        assert_eq!(e.buffer_count(), 2, "leftover Untitled pruned, leaving [a, b]");
        assert!(e.active_path().unwrap().to_string_lossy().contains("b.txt"));
    }

    #[test]
    fn untitledtab_off_keeps_the_scratch() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.run_command("UNTITLEDTAB off");
        e.host_mut().write_file("c.txt", "z").unwrap();
        e.run_command("OPEN c.txt");
        assert_eq!(e.buffer_count(), 2, "with UNTITLEDTAB off the Untitled tab stays");
    }

    #[test]
    fn loadcfg_confirms_after_running_the_config() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.host_mut().write_file("my.cfg", "TB 4\n").unwrap(); // a trivial config
        e.run_command("LOADCFG my.cfg");
        // The config runs (pending_run), then a final line confirms the load — like LOADLIB/LOADAR.
        let out = e.output().join("\n");
        assert!(out.contains("loaded config from"), "LOADCFG should confirm success: {out:?}");
        assert!(out.contains("my.cfg"), "the confirmation names the file ($CF): {out:?}");
    }

    #[test]
    fn paplace_and_pathlen_set_report_and_persist() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        // Defaults: path on the status line, width 45.
        assert_eq!(e.special_str("PAPLACE"), "status");
        assert_eq!(e.special_str("PATHLEN"), "45");
        // Set both.
        e.run_command("PAPLACE cmline");
        assert_eq!(e.special_str("PAPLACE"), "cmline");
        e.run_command("PATHLEN 20");
        assert_eq!(e.special_str("PATHLEN"), "20");
        // Bare forms report without changing.
        e.run_command("PAPLACE");
        assert_eq!(e.special_str("PAPLACE"), "cmline");
        // Bad values are trappable errors, state unchanged.
        e.run_command("PAPLACE nowhere");
        assert!(e.last_error().is_some());
        assert_eq!(e.special_str("PAPLACE"), "cmline");
        e.run_command("PATHLEN wide");
        assert!(e.last_error().is_some());
        assert_eq!(e.special_str("PATHLEN"), "20");
        // Read-only specials.
        e.run_command("SET PAPLACE = status");
        assert!(e.last_error().is_some(), "PAPLACE is read-only");
        // SAVECFG carries both.
        e.run_command("SAVECFG cfg2.cfg");
        let cfg = e.host_mut().files.get("cfg2.cfg").unwrap().clone();
        assert!(cfg.contains("PAPLACE cmline"), "config carries placement: {cfg}");
        assert!(cfg.contains("PATHLEN 20"), "config carries length: {cfg}");
    }

    #[test]
    fn writefile_verb_and_readfile_function_round_trip() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        // WRITEFILE writes a variable's value verbatim; READFILE() reads it back.
        e.run_command("SET x = \"hello world\"");
        e.run_command("WRITEFILE notes.txt $x");
        e.run_command("SET y = READFILE(\"notes.txt\")");
        e.run_command("PR $y");
        assert_eq!(e.output().last().map(String::as_str), Some("hello world"));
        // The short aliases work the same, and READFILE composes inside an expression.
        e.run_command("WF a.txt done");
        e.run_command("PR $(UPPER(RF(\"a.txt\")))");
        assert_eq!(e.output().last().map(String::as_str), Some("DONE"));
        // READFILE of a missing file is RECOVERABLE: it sets $ERROR instead of halting, so a script
        // can react. (Unlike most expression errors — bad arity, divide-by-zero — which stay fatal.)
        e.run_command("SET z = READFILE(\"nope.txt\")");
        e.run_command("PR $ERROR");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
        // ...and the script keeps running afterward.
        e.run_command("PR alive");
        assert_eq!(e.output().last().map(String::as_str), Some("alive"));
    }

    #[test]
    fn write_aliases_insert_and_writeln_adds_a_newline() {
        use crate::document::LineEnding;
        let mut e = ed();
        let nl = LineEnding::os_default(); // WRITELN adds the document's line ending, not a bare LF
        // WRITE / WR are exact synonyms for INSERT (literal text, \n escapes processed).
        e.run_command("WRITE Hello");
        e.run_command("WR \" World\"");
        assert_eq!(e.document().text(), "Hello World");
        // WRITELN / WRL insert the text then a trailing CANONICAL newline (\n or \r\n).
        e.run_command("WRITELN !");
        e.run_command("WRL done");
        assert_eq!(e.document().text(), format!("Hello World!{nl}done{nl}", nl = nl.as_str()));
    }

    #[test]
    fn motion_then_insert_lands_in_the_middle() {
        let mut e = ed();
        e.feed_key(KeyEvent::down(16)); // h
        e.feed_key(KeyEvent::down(17)); // i  -> "hi", cursor at end
        e.feed_key(KeyEvent::down(19)); // left -> between h and i
        e.feed_key(KeyEvent::down(21)); // X
        assert_eq!(e.document().text(), "hXi");
    }

    #[test]
    fn undo_redo_via_verbs() {
        // A keymap: keys type h/i; key 1 = UNDO, key 2 = REDO.
        let mut tables: HashMap<(u32, u8), Assignment> = HashMap::new();
        let cell = |s: &str| Assignment::Xpl(s.to_string());
        tables.insert((0, 16), cell("insert \"h\""));
        tables.insert((0, 17), cell("insert \"i\""));
        tables.insert((0, 1), cell("undo"));
        tables.insert((0, 2), cell("redo"));
        let km = Keymap { modifiers: Vec::new(), tables, keys_max: 104, virtual_base: None };
        let mut e = Editor::new(km);

        e.feed_key(KeyEvent::down(16)); // h
        e.feed_key(KeyEvent::down(17)); // i  -> "hi" (one coalesced typing run)
        assert_eq!(e.document().text(), "hi");
        e.feed_key(KeyEvent::down(1)); // UNDO -> whole run gone
        assert_eq!(e.document().text(), "");
        e.feed_key(KeyEvent::down(2)); // REDO
        assert_eq!(e.document().text(), "hi");
    }

    #[test]
    fn undo_with_a_count_undoes_multiple_levels() {
        let mut e = ed();
        // Three separate undo units (the RIGHT motions break the coalescing run).
        e.run_command(r#"INSERT "a"; RIGHT; INSERT "b"; RIGHT; INSERT "c""#);
        assert_eq!(e.document().text(), "abc");
        e.run_command("UNDO 2"); // undo "c" and "b"
        assert_eq!(e.document().text(), "a");
        e.run_command("REDO 2"); // redo "b" and "c"
        assert_eq!(e.document().text(), "abc");
        e.run_command("UNDO 3"); // back to empty
        assert_eq!(e.document().text(), "");
        e.run_command("UNDO"); // bare = one level (already empty: no-op)
        assert_eq!(e.document().text(), "");
        // Asking for more levels than exist stops cleanly (no error/panic).
        e.run_command("REDO 9");
        assert_eq!(e.document().text(), "abc");
        // A non-numeric argument is a trappable error (surfaced; document untouched).
        e.run_command("UNDO foo");
        assert!(e.last_error().is_some());
        assert_eq!(e.document().text(), "abc");
        e.run_command("REDO bar");
        assert!(e.last_error().is_some());
        assert_eq!(e.document().text(), "abc");
    }

    #[test]
    fn embedded_default_bare_letters_type_via_desugar() {
        // Before D4, the embedded default's bare `q`/`w` cells errored as unknown
        // verbs. After desugaring they self-insert.
        let mut e = Editor::with_default_keymap();
        e.feed_key(KeyEvent::down(16)); // 'q' cell
        e.feed_key(KeyEvent::down(17)); // 'w' cell
        assert_eq!(e.document().text(), "qw");
        assert!(e.last_error().is_none());
    }

    #[test]
    fn bare_and_quoted_cells_type_after_desugar() {
        let mut tables: HashMap<(u32, u8), Assignment> = HashMap::new();
        tables.insert((0, 16), Assignment::Xpl("h".into())); // bare char
        tables.insert((0, 17), Assignment::Xpl("\"!\"".into())); // quoted literal
        let km = Keymap { modifiers: Vec::new(), tables, keys_max: 104, virtual_base: None };
        let mut e = Editor::new(km);
        e.feed_key(KeyEvent::down(16));
        e.feed_key(KeyEvent::down(17));
        assert_eq!(e.document().text(), "h!");
    }

    #[test]
    fn can_type_path_characters_backslash_and_colon() {
        // The chars an absolute Windows path needs, end-to-end through
        // desugar -> splitter -> INSERT.
        let mut tables: HashMap<(u32, u8), Assignment> = HashMap::new();
        tables.insert((0, 16), Assignment::Xpl("\\".into())); // bare backslash (desugared)
        tables.insert((0, 17), Assignment::Xpl("insert \":\"".into())); // explicit colon
        tables.insert((0, 18), Assignment::Xpl("/".into())); // bare slash
        let km = Keymap { modifiers: Vec::new(), tables, keys_max: 104, virtual_base: None };
        let mut e = Editor::new(km);
        e.feed_key(KeyEvent::down(16));
        e.feed_key(KeyEvent::down(17));
        e.feed_key(KeyEvent::down(18));
        assert_eq!(e.document().text(), "\\:/");
        assert!(e.last_error().is_none());
    }

    #[test]
    fn unbound_key_is_swallowed_without_editing() {
        let mut e = ed();
        // key 99 is not in the table -> miss -> Swallow, no dispatch.
        assert_eq!(e.feed_key(KeyEvent::down(99)), Disposition::Swallow);
        assert_eq!(e.document().text(), "");
    }

    #[test]
    fn a_bad_cell_does_not_wedge_the_loop() {
        // A cell with an unknown verb is a fatal script error, but the editor
        // keeps running (the error is captured, not propagated).
        let mut tables: HashMap<(u32, u8), Assignment> = HashMap::new();
        tables.insert((0, 16), Assignment::Xpl("floooob".into()));
        tables.insert((0, 17), Assignment::Xpl("insert \"ok\"".into()));
        let km = Keymap { modifiers: Vec::new(), tables, keys_max: 104, virtual_base: None };
        let mut e = Editor::new(km);
        e.feed_key(KeyEvent::down(16)); // fatal cell
        assert!(e.last_error().is_some());
        e.feed_key(KeyEvent::down(17)); // still works
        assert_eq!(e.document().text(), "ok");
    }

    // ---- LOADKBD swap (startup / transactional load) -----------------------

    #[test]
    fn loadkbd_swaps_the_operative_keymap() {
        let mut e = ed();
        // A loaded layout where key 16 inserts "Z" instead of "h".
        let new_kbd = "KEYS=104\nTABLE=\n16=insert \"Z\"\n";
        e.host_mut().files.insert("other.kbd".into(), new_kbd.into());
        // Bind a key to LOADKBD other.kbd and press it.
        // (Reuse run_command as the manual path to issue LOADKBD.)
        e.run_command("loadkbd other.kbd");
        assert!(e.last_error().is_none(), "load failed: {:?}", e.last_error());
        e.feed_key(KeyEvent::down(16));
        assert_eq!(e.document().text(), "Z");
    }

    #[test]
    fn broken_loadkbd_falls_back_without_stranding() {
        let mut e = ed();
        e.host_mut().files.insert("bad.kbd".into(), "this is not a kbd".into());
        e.run_command("loadkbd bad.kbd");
        // The load failed (trappable), but the old keymap still works.
        e.feed_key(KeyEvent::down(16));
        assert_eq!(e.document().text(), "h"); // original "h" cell, not stranded
    }

    #[test]
    fn startup_script_loads_a_layout() {
        let mut e = ed();
        let layout = "KEYS=104\nTABLE=\n16=insert \"S\"\n";
        e.host_mut().files.insert("custom.kbd".into(), layout.into());
        e.run_startup("loadkbd custom.kbd", "startup.xpl");
        e.feed_key(KeyEvent::down(16));
        assert_eq!(e.document().text(), "S");
    }

    // ---- CMline ------------------------------------------------------------

    #[test]
    fn run_command_executes_xpl_against_the_document() {
        let mut e = ed();
        e.run_command("insert \"from cmd\"");
        assert_eq!(e.document().text(), "from cmd");
    }

    // ---- binders (LDTX/LDSC/LDPM + @X/&X) ----------------------------------

    #[test]
    fn binder_text_literal_roundtrip() {
        let mut e = ed();
        e.run_command(r#"LDTX h "hello""#);
        e.run_command("@H"); // insert @H's text at the caret
        assert_eq!(e.document().text(), "hello");
        e.run_command("@h"); // lowercase hits the same (case-folded) slot
        assert_eq!(e.document().text(), "hellohello");
    }

    #[test]
    fn binder_script_literal_runs() {
        let mut e = ed();
        e.run_command(r#"LDSC a "insert \"hi\"""#);
        e.run_command("&A"); // deferred-run the stored XPL
        assert_eq!(e.document().text(), "hi");
        // LDPM is a synonym for LDSC.
        e.run_command(r#"LDPM b "insert \"!\"""#);
        e.run_command("&B");
        assert_eq!(e.document().text(), "hi!");
    }

    #[test]
    fn binder_loads_from_selection() {
        let mut e = ed();
        e.run_command(r#"insert "abc""#);
        e.run_command("DFA"); // select all → "abc"
        e.run_command("LDTX z"); // no source → the selection
        e.run_command("NEW"); // fresh empty doc
        e.run_command("@Z");
        assert_eq!(e.document().text(), "abc");
    }

    #[test]
    fn binder_loads_from_file() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("snip.txt".into(), "canned".into());
        e.run_command("LDTX p snip.txt"); // bare token → file path
        e.run_command("@P");
        assert_eq!(e.document().text(), "canned");
    }

    #[test]
    fn binder_empty_slot_is_trappable() {
        let mut e = ed();
        e.run_command("@Q");
        assert!(e.status_message().unwrap().contains("is empty"));
        assert_eq!(e.document().text(), ""); // nothing inserted
    }

    #[test]
    fn binder_script_queue_runs_all_and_nests() {
        // Two script binders in one line both run, in order (FIFO queue, not last-wins).
        let mut e = ed();
        e.run_command(r#"LDSC a "insert \"a\"""#);
        e.run_command(r#"LDSC b "insert \"b\"""#);
        e.run_command("&A;&B");
        assert_eq!(e.document().text(), "ab");

        // Nesting: &C's script runs &D, which inserts.
        let mut e = ed();
        e.run_command(r#"LDSC d "insert \"x\"""#);
        e.run_command(r#"LDSC c "&D""#);
        e.run_command("&C");
        assert_eq!(e.document().text(), "x");
    }

    #[test]
    fn binder_value_access_in_args_and_expressions() {
        let mut e = ed();
        e.run_command(r#"LDTX h "hi""#);
        e.run_command(r#"LDSC z "DFA""#);
        // Arg-tail value: PR prints the stored text.
        e.run_command("PR @H");
        assert_eq!(e.output().last().map(String::as_str), Some("hi"));
        // Script-binder source as a value.
        e.run_command("PR &Z");
        assert_eq!(e.output().last().map(String::as_str), Some("DFA"));
        // Expression value.
        e.run_command("SET x = @H");
        e.run_command("PR $x");
        assert_eq!(e.output().last().map(String::as_str), Some("hi"));
    }

    #[test]
    fn literal_at_in_arg_text_is_not_a_binder() {
        let mut e = ed();
        e.run_command(r#"LDTX b "SHOULD-NOT-APPEAR""#); // a @B binder exists
        // `a@b.com` — `@b` is glued to a word char, so it stays literal (not the binder).
        e.run_command("INSERT a@b.com");
        assert_eq!(e.document().text(), "a@b.com");
    }

    // ---- B15 view toggles (SMB/CMB/TMB, SVR/CVR/TVR, SLN/CLN/TLN + $MB/$VR/$LNS) ----

    #[test]
    fn view_toggles_set_clear_toggle_and_read_specials() {
        let mut e = ed();
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        // Defaults: menu bar on; visible returns + line numbers off.
        assert!(e.menu_bar_shown() && !e.visible_returns() && !e.line_numbers());
        e.run_command("PR $MB");
        assert_eq!(out(&e).as_deref(), Some("1"));
        e.run_command("PR $VR");
        assert_eq!(out(&e).as_deref(), Some("0"));
        e.run_command("PR $LNS");
        assert_eq!(out(&e).as_deref(), Some("0"));
        // Clear/toggle the menu bar.
        e.run_command("CMB");
        assert!(!e.menu_bar_shown());
        e.run_command("TMB");
        assert!(e.menu_bar_shown());
        // Visible returns on/toggle.
        e.run_command("SVR");
        assert!(e.visible_returns());
        e.run_command("TVR");
        assert!(!e.visible_returns());
        // Visible whitespace.
        e.run_command("SWS");
        assert!(e.visible_whitespace());
        e.run_command("PR $WS");
        assert_eq!(out(&e).as_deref(), Some("1"));
        e.run_command("CWS");
        assert!(!e.visible_whitespace());
        // Line numbers on; $LNS reflects it.
        e.run_command("SLN");
        assert!(e.line_numbers());
        e.run_command("PR $LNS");
        assert_eq!(out(&e).as_deref(), Some("1"));
        e.run_command("CLN");
        assert!(!e.line_numbers());
        // Pop-up command line: off by default; set/toggle; $GC reflects it.
        assert!(!e.gui_cmline());
        e.run_command("PR $GC");
        assert_eq!(out(&e).as_deref(), Some("0"));
        e.run_command("SGC");
        assert!(e.gui_cmline());
        e.run_command("PR $GC");
        assert_eq!(out(&e).as_deref(), Some("1"));
        e.run_command("TGC");
        assert!(!e.gui_cmline());
    }

    #[test]
    fn ch_special_reads_the_character_to_the_right_of_the_caret() {
        let mut e = ed();
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        e.run_command(r#"insert "ab""#);
        e.run_command("ENTER"); // a line break (\n or \r\n per the doc)
        e.run_command(r#"insert "c""#); // text: "ab<nl>c", caret at end
        // End-of-buffer: nothing to the right.
        e.run_command("PR $CH");
        assert_eq!(out(&e).as_deref(), Some(""));
        // Top of file: the first character.
        e.run_command("TOP");
        e.run_command("PR $CH");
        assert_eq!(out(&e).as_deref(), Some("a"));
        // One char right: 'b'.
        e.run_command("RIGHT");
        e.run_command("PR $CH");
        assert_eq!(out(&e).as_deref(), Some("b"));
        // Next char right is the line break — reported as "\n" even when the doc uses \r\n.
        e.run_command("RIGHT");
        e.run_command("PR $CH");
        assert_eq!(out(&e).as_deref(), Some("\n"));
    }

    #[test]
    fn wd_special_reads_the_word_under_the_caret() {
        let mut e = ed();
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        e.run_command(r#"insert "alpha beta""#); // caret at end (after "beta")
        e.run_command("PR $WD");
        assert_eq!(out(&e).as_deref(), Some("beta"));
        // Jump to the top — caret on the first word.
        e.run_command("TOP");
        e.run_command("PR $WD");
        assert_eq!(out(&e).as_deref(), Some("alpha"));
        // Caret not adjacent to any word (whitespace-only) → empty.
        let mut e = ed();
        e.run_command(r#"insert "  ""#); // two spaces, caret at end
        e.run_command("PR $WD");
        assert_eq!(out(&e).as_deref(), Some(""));
    }

    #[test]
    fn multicaret_specials_report_count_primary_and_positions() {
        let mut e = ed();
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        e.run_command(r#"insert "abcdef""#);
        e.host_mut().set_active_caret(0, None); // primary at 0
        e.host_mut().add_active_caret(2);
        e.host_mut().add_active_caret(4);
        // $CARETS counts the whole set; $CARETP is the primary's document-order index.
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("3"));
        e.run_command("PR $CARETP");
        assert_eq!(out(&e).as_deref(), Some("0"));
        // $MCP is a real array: joinable, indexable, LEN-able.
        e.run_command(r#"PR $(JOIN($MCP, "-"))"#);
        assert_eq!(out(&e).as_deref(), Some("0-2-4"));
        e.run_command("PR $($MCP[2])");
        assert_eq!(out(&e).as_deref(), Some("4"));
        e.run_command("PR $(LEN($MCP))");
        assert_eq!(out(&e).as_deref(), Some("3"));
        // The invariant: $CP (primary) == $MCP[$CARETP].
        e.run_command("PR $(($CP == $MCP[$CARETP]))");
        assert_eq!(out(&e).as_deref(), Some("1"));
    }

    #[test]
    fn caret_verbs_add_collapse_and_stack() {
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        let text = |e: &Editor| e.doc(e.active_index()).text();

        let mut e = ed();
        e.run_command(r#"insert "abcdef""#);
        e.run_command("TOP"); // primary at 0
        // CARETADD <n> adds carets at offsets; CARETS reports the whole set.
        e.run_command("CARETADD 2");
        e.run_command("CARETADD 4");
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("3"));
        // Typing at all three carets is ONE undo unit.
        e.run_command(r#"insert "X""#);
        assert_eq!(text(&e), "XabXcdXef");
        e.run_command("GT;UNDO");
        assert_eq!(text(&e), "abcdef");
        // CARETONE collapses back to a single caret.
        e.run_command("CARETONE");
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("1"));

        // CARETDOWN stacks a caret onto the next line (count doesn't depend on newline length).
        let mut e = ed();
        e.run_command(r#"insert "aaa""#);
        e.run_command("ENTER");
        e.run_command(r#"insert "bbb""#); // two lines
        e.run_command("TOP");
        e.run_command("CARETDOWN");
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("2"));
    }

    #[test]
    fn hex_view_verb_toggles_the_per_document_flag() {
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        let mut e = ed();
        assert!(!e.hex_view());
        e.run_command("PR $HEX");
        assert_eq!(out(&e).as_deref(), Some("0"));
        e.run_command("HEX on");
        assert!(e.hex_view());
        e.run_command("PR $HEX");
        assert_eq!(out(&e).as_deref(), Some("1"));
        e.run_command("HEX off");
        assert!(!e.hex_view());
        // Bare HEX = toggle: off → on → off.
        e.run_command("HEX");
        assert!(e.hex_view());
        e.run_command("PR $HEX");
        assert_eq!(out(&e).as_deref(), Some("1"));
        e.run_command("HEX");
        assert!(!e.hex_view());
        e.run_command("PR $HEX");
        assert_eq!(out(&e).as_deref(), Some("0"));
    }

    #[test]
    fn hex_view_navigates_by_byte_and_blocks_edits() {
        let cur = |e: &Editor| e.doc(e.active_index()).cursor();
        let len = |e: &Editor| e.doc(e.active_index()).len();
        let mut e = ed();
        // 40 bytes — spans three 16-byte grid rows.
        e.run_command(r#"insert "0123456789abcdefghijklmnopqrstuvwxyzABCD""#);
        e.run_command("HEX on");
        e.run_command("TOP");
        assert_eq!(cur(&e), 0);
        e.run_command("RIGHT");
        assert_eq!(cur(&e), 1, "Right = +1 byte");
        e.run_command("DOWN");
        assert_eq!(cur(&e), 17, "Down = +16 bytes (one grid row)");
        e.run_command("UP");
        assert_eq!(cur(&e), 1, "Up = -16 bytes");
        e.run_command("LEFT");
        assert_eq!(cur(&e), 0, "Left clamps at the start");
        // TEXT edits (typing) are rejected in hex view — only the dedicated hex-edit ops mutate bytes.
        let before = len(&e);
        e.run_command(r#"insert "X""#);
        assert_eq!(len(&e), before, "hex view blocks text inserts");
        // Turning hex off restores text editability.
        e.run_command("HEX off");
        e.run_command(r#"insert "X""#);
        assert_eq!(len(&e), before + 1, "text edits resume once hex view is off");
    }

    #[test]
    fn rs_deletes_the_sentence_after_a_gap_and_keeps_a_trailing_newline() {
        let text = |e: &Editor| e.doc(e.active_index()).text();
        // A) Caret on the space between two sentences deletes the sentence AFTER (was: the one before).
        let mut e = ed();
        e.run_command(r#"insert "Foo. Bar. Baz.""#);
        e.run_command("JMP 4"); // caret on the space after "Foo."
        e.run_command("RS");
        assert_eq!(text(&e), "Foo. Baz.", "gap → deletes Bar. (after), keeps Foo.");
        // Baseline: caret WITHIN a sentence still deletes that sentence (+ its trailing space).
        let mut e = ed();
        e.run_command(r#"insert "Foo. Bar. Baz.""#);
        e.run_command("TOP");
        e.run_command("RS");
        assert_eq!(text(&e), "Bar. Baz.");
        // B) A sentence that ends at end of line: RS deletes the sentence but LEAVES the newline.
        let mut e = ed();
        e.run_command(r#"insert "Hello world.\nNext line.""#);
        e.run_command("TOP");
        e.run_command("RS");
        assert_eq!(text(&e), "\nNext line.", "B → keeps the line break");
        // The A "delete after" must NOT jump across a line break: caret on trailing spaces before a
        // newline deletes the CURRENT sentence (and the spaces) but keeps the newline.
        let mut e = ed();
        e.run_command(r#"insert "First.  \nSecond.""#);
        e.run_command("JMP 6"); // caret on a trailing space, before the newline
        e.run_command("RS");
        assert_eq!(text(&e), "\nSecond.", "gap across a break → deletes First., keeps the newline");
    }

    #[test]
    fn open_and_read_hex_switch_open_in_hex_view() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        // OPEN/HEX of a (new) file opens straight into the hex view.
        e.run_command("OPEN/HEX newbin.dat");
        assert!(e.hex_view(), "OPEN/HEX opens in hex view");
        // A plain OPEN (no /HEX) stays in the normal text view.
        e.run_command("OPEN another.txt");
        assert!(!e.hex_view(), "plain OPEN is the text view");
        // READ/HEX of an existing file: hex view AND read-only.
        e.host_mut().files.insert("data.bin".to_string(), "ABCD".to_string());
        e.run_command("READ/HEX data.bin");
        assert!(e.hex_view(), "READ/HEX opens in hex view");
        assert!(e.active_read_only(), "READ opens read-only");
    }

    #[test]
    fn hex_editing_overwrites_inserts_deletes_and_undoes() {
        let bytes = |e: &Editor| {
            let b = e.doc(e.active_index());
            b.slice_bytes(0, b.len())
        };
        let mut e = ed();
        e.run_command(r#"insert "ABCD""#); // 0x41 0x42 0x43 0x44
        e.run_command("HEX on");
        e.run_command("TOP");
        // Hex-column nibble entry: set byte 0 to 0x7F (hi=7, then lo=F).
        e.hex_set_nibble(true, 0x7);
        e.hex_set_nibble(false, 0xF);
        assert_eq!(bytes(&e)[0], 0x7F, "two nibbles compose a byte");
        // Text-column overwrite: whole byte at the caret.
        e.hex_overwrite(0x00);
        assert_eq!(bytes(&e)[0], 0x00);
        // Text typing is still blocked (only hex-edit ops mutate).
        let before = e.hex_len();
        e.run_command(r#"insert "z""#);
        assert_eq!(e.hex_len(), before, "text insert still blocked while editing hex");
        // Byte insert at the caret (byte 0).
        e.hex_insert(vec![0xAA]);
        assert_eq!(bytes(&e), vec![0xAA, 0x00, 0x42, 0x43, 0x44]);
        // Delete the byte at the caret.
        e.hex_delete(false);
        assert_eq!(bytes(&e), vec![0x00, 0x42, 0x43, 0x44]);
        // Undo restores the deleted byte (each hex edit is one undo unit).
        e.run_command("UNDO");
        assert_eq!(bytes(&e), vec![0xAA, 0x00, 0x42, 0x43, 0x44], "undo restores the delete");
        // Byte copy/paste through the clipboard verbs (COPY/PASTE operate on raw bytes in hex view).
        // Bytes are now [AA, 00, 42, 43, 44]; select the first two and copy them.
        e.hex_set_caret(0);
        e.run_command("RIGHT SEL"); // extend to byte 1
        e.run_command("RIGHT SEL"); // extend to byte 2 → selection [0,2) = [AA,00]
        assert_eq!(e.hex_selection_bytes(), Some(vec![0xAA, 0x00]));
        e.run_command("COPY");
        // Paste over byte 2 (overwrite): move caret to 2, paste → bytes 2,3 become AA,00.
        e.hex_set_caret(2);
        e.run_command("PASTE");
        assert_eq!(bytes(&e), vec![0xAA, 0x00, 0xAA, 0x00, 0x44], "paste overwrites bytes at the caret");
        // A real RO lock blocks hex editing too.
        e.run_command("RO");
        let locked = bytes(&e);
        e.hex_overwrite(0x55);
        e.run_command("PASTE");
        assert_eq!(bytes(&e), locked, "RO lock blocks hex edits and paste");
    }

    #[test]
    fn find_replace_overwrites_the_match_even_under_persistent_selection() {
        let text = |e: &Editor| e.doc(e.active_index()).text();
        // The Find dialog's single Replace uses replace_selection, which overwrites the selected match
        // as one undo unit REGARDLESS of persistent-selection mode (a plain insert under PS keeps the
        // block and leaves the match behind — the reported bug).
        for ps in [false, true] {
            // Replace-with-nothing deletes the match.
            let mut e = ed();
            if ps {
                e.run_command("SPS"); // persistent selection ON
            }
            e.run_command(r#"insert "foo bar foo""#);
            e.run_command("TOP");
            e.run_command("SE foo"); // selects the first "foo"
            assert!(e.has_selection());
            e.replace_selection(""); // empty replacement → delete the match
            assert_eq!(text(&e), " bar foo", "ps={ps}");
            e.run_command("GT;UNDO");
            assert_eq!(text(&e), "foo bar foo", "ps={ps}");
            // Non-empty replacement overwrites the match.
            e.run_command("TOP");
            e.run_command("SE foo");
            e.replace_selection("X");
            assert_eq!(text(&e), "X bar foo", "ps={ps}");
        }
        // No selection → replace_selection is a no-op.
        let mut e = ed();
        e.run_command(r#"insert "abc""#);
        e.replace_selection("Z");
        assert_eq!(text(&e), "abc");
    }

    #[test]
    fn caretall_selects_every_match_then_edits_all_at_once() {
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        let text = |e: &Editor| e.doc(e.active_index()).text();

        // Bare CARETALL <pattern>: a selection-caret at every match; typing replaces them all.
        let mut e = ed();
        e.run_command(r#"insert "foo bar foo baz foo""#);
        e.run_command("TOP");
        e.run_command("CARETALL foo");
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("3"));
        e.run_command(r#"insert "X""#); // replace each selected "foo"
        assert_eq!(text(&e), "X bar X baz X");

        // No matches: the caret set is left unchanged (still a single caret).
        let mut e = ed();
        e.run_command(r#"insert "alpha beta""#);
        e.run_command("CARETALL zzz");
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("1"));

        // /R regex: every run of digits becomes a caret.
        let mut e = ed();
        e.run_command(r#"insert "a1 b22 c333""#);
        e.run_command("CARETALL/R \\d+");
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("3"));

        // Bare CARETALL with a selection uses the selection text as the needle.
        let mut e = ed();
        e.run_command(r#"insert "cat dog cat dog cat""#);
        e.run_command("TOP");
        e.run_command("RIGHT SEL;RIGHT SEL;RIGHT SEL"); // select "cat"
        e.run_command("CARETALL");
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("3"));
    }

    #[test]
    fn caretnext_seeds_then_walks_occurrences_and_wraps() {
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        let text = |e: &Editor| e.doc(e.active_index()).text();

        let mut e = ed();
        e.run_command(r#"insert "foo bar foo baz foo""#);
        e.run_command("TOP");
        // First press seeds: selects the word "foo" under the caret, no new caret yet.
        e.run_command("CARETNEXT");
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("1"));
        // Next presses add the following occurrences as selection-carets.
        e.run_command("CARETNEXT");
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("2"));
        e.run_command("CARETNEXT");
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("3"));
        // All three selected: another press wraps and finds an already-selected match — no change.
        e.run_command("CARETNEXT");
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("3"));
        // Typing replaces all three selected occurrences at once.
        e.run_command(r#"insert "X""#);
        assert_eq!(text(&e), "X bar X baz X");

        // Explicit-text form needs no seed: CARETNEXT <text> starts matching immediately.
        let mut e = ed();
        e.run_command(r#"insert "one two one two one""#);
        e.run_command("TOP");
        e.run_command("CARETNEXT two");
        e.run_command("CARETNEXT two");
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("2"));
    }

    #[test]
    fn multi_caret_copy_distributes_pieces_on_paste() {
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        let text = |e: &Editor| e.doc(e.active_index()).text();

        // Copy three DISTINCT selections, then paste into three bare carets → piece i lands at caret i.
        let mut e = ed();
        e.run_command("insert \"1 2 3 .....\""); // digits to copy, dots as paste targets
        e.run_command("TOP");
        e.run_command(r#"CARETALL/R \d"#); // three selection-carets: "1", "2", "3"
        e.run_command("COPY"); // pieces = ["1","2","3"]
        e.run_command("CARETONE");
        e.run_command("JMP 6"); // primary at the first dot
        e.run_command("CARETADD 7");
        e.run_command("CARETADD 8"); // three bare carets at offsets 6,7,8
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("3"));
        e.run_command("PASTE"); // distribute: "1"→6, "2"→7, "3"→8 (before the first three dots)
        assert_eq!(text(&e), "1 2 3 1.2.3...");

        // Multi-caret CUT removes every selection and stores the pieces.
        let mut e = ed();
        e.run_command(r#"insert "aaa bbb ccc""#);
        e.run_command("TOP");
        e.run_command(r#"CARETALL/R [a-c]+"#); // select aaa, bbb, ccc
        e.run_command("CUT");
        assert_eq!(text(&e), "  "); // the three words gone, the two spaces remain

        // Count mismatch falls back to pasting the joined clipboard at every caret.
        let mut e = ed();
        e.run_command(r#"insert "x y""#);
        e.run_command("TOP");
        e.run_command(r#"CARETALL/R \w"#); // 2 carets ("x","y"), pieces ["x","y"]
        e.run_command("COPY");
        e.run_command("CARETONE"); // now a single caret → count (1) != pieces (2)
        e.run_command("BOTTOM");
        e.run_command("PASTE"); // fallback: insert the joined "x<nl>y" once
        assert!(text(&e).replace('\r', "").ends_with("x\ny"));
    }

    #[test]
    fn caretskip_moves_the_primary_past_an_unwanted_match() {
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        let text = |e: &Editor| e.doc(e.active_index()).text();

        let mut e = ed();
        e.run_command(r#"insert "foo foo foo foo""#); // four occurrences
        e.run_command("TOP");
        e.run_command("CARETNEXT"); // seed: select foo #1
        e.run_command("CARETSKIP"); // move that selection to foo #2 (no new caret)
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("1"));
        e.run_command("PR $($MDS[0])"); // primary now starts at foo #2 (offset 4)
        assert_eq!(out(&e).as_deref(), Some("4"));
        e.run_command("CARETNEXT"); // add foo #3
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("2"));
        // The two selected occurrences are #2 and #3 — typing skips #1 and #4.
        e.run_command(r#"insert "X""#);
        assert_eq!(text(&e), "foo X X foo");

        // CARETSKIP with nothing selected is a no-op.
        let mut e = ed();
        e.run_command(r#"insert "alpha""#);
        e.run_command("CARETSKIP");
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("1"));
    }

    #[test]
    fn m_array_mirrors_report_every_caret_and_hold_the_invariants() {
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);

        // A selection-caret set (three "foo"s).
        let mut e = ed();
        e.run_command(r#"insert "foo bar foo baz foo""#);
        e.run_command("TOP");
        e.run_command("CARETALL foo");
        // LEN of every mirror == $CARETS.
        e.run_command("PR $(LEN($MSE))");
        assert_eq!(out(&e).as_deref(), Some("3"));
        // Each caret's selected text.
        e.run_command("PR $($MSE[0])");
        assert_eq!(out(&e).as_deref(), Some("foo"));
        // The primary correspondence invariant: $SE == $MSE[$CARETP].
        e.run_command("PR $(($SE == $MSE[$CARETP]))");
        assert_eq!(out(&e).as_deref(), Some("1"));
        // Start/end/length mirrors: $MSL[k] == $MDN[k] - $MDS[k].
        e.run_command("PR $(($MDN[2] - $MDS[2]))");
        assert_eq!(out(&e).as_deref(), Some("3"));
        e.run_command("PR $($MSL[1])");
        assert_eq!(out(&e).as_deref(), Some("3"));
        // Selection-present flag, word, and line number.
        e.run_command("PR $($MDF[0])");
        assert_eq!(out(&e).as_deref(), Some("1"));
        e.run_command("PR $($MWD[2])");
        assert_eq!(out(&e).as_deref(), Some("foo"));
        e.run_command("PR $($MLN[0])");
        assert_eq!(out(&e).as_deref(), Some("1"));

        // A BARE-caret set: mirrors report empty selections but valid positions/chars.
        let mut e = ed();
        e.run_command(r#"insert "abcdefgh""#);
        e.run_command("TOP");
        e.run_command("CARETADD 2");
        e.run_command("CARETADD 4");
        e.run_command("PR $(LEN($MCP))");
        assert_eq!(out(&e).as_deref(), Some("3"));
        e.run_command("PR $($MDF[0])"); // no selection
        assert_eq!(out(&e).as_deref(), Some("0"));
        e.run_command("PR $($MSE[1])"); // empty selected text
        assert_eq!(out(&e).as_deref(), Some(""));
        e.run_command("PR $($MCH[0])"); // char to the right of caret 0 (offset 0)
        assert_eq!(out(&e).as_deref(), Some("a"));
        e.run_command("PR $($MCH[1])"); // caret at offset 2 → 'c'
        assert_eq!(out(&e).as_deref(), Some("c"));
    }

    #[test]
    fn caretsplit_drops_a_caret_at_each_selected_line_end() {
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        let text = |e: &Editor| e.doc(e.active_index()).text();

        let mut e = ed();
        e.run_command(r#"insert "abc""#);
        e.run_command("ENTER");
        e.run_command(r#"insert "def""#);
        e.run_command("ENTER");
        e.run_command(r#"insert "ghi""#); // three lines
        e.run_command("TOP");
        e.run_command("BOTTOM SEL"); // select the whole document
        e.run_command("CARETSPLIT");
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("3"));
        // A caret at each line's end → typing appends to every line at once.
        e.run_command(r#"insert "X""#);
        assert_eq!(text(&e).replace('\r', ""), "abcX\ndefX\nghiX");

        // A single-line selection is left unchanged.
        let mut e = ed();
        e.run_command(r#"insert "hello world""#);
        e.run_command("TOP");
        e.run_command("NW SEL"); // select within one line
        e.run_command("CARETSPLIT");
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("1"));
    }

    #[test]
    fn caretpop_steps_back_through_the_ring_and_edits_clear_it() {
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        let text = |e: &Editor| e.doc(e.active_index()).text();

        let mut e = ed();
        e.run_command(r#"insert "foo bar foo baz foo""#);
        e.run_command("TOP");
        e.run_command("CARETNEXT"); // seed → 1
        e.run_command("CARETNEXT"); // → 2
        e.run_command("CARETNEXT"); // → 3
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("3"));
        e.run_command("CARETPOP"); // → 2
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("2"));
        e.run_command("CARETPOP"); // → 1 (the seed selection)
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("1"));

        // An edit clears the ring: a later CARETPOP is a no-op.
        let mut e = ed();
        e.run_command(r#"insert "foo foo foo""#);
        e.run_command("TOP");
        e.run_command("CARETALL foo"); // 3 carets
        e.run_command(r#"insert "X""#); // replaces all three; ring cleared by the edit
        assert_eq!(text(&e), "X X X");
        e.run_command("CARETPOP"); // nothing to undo
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("3")); // the three (now bare) carets remain

        // CARETPRIMARY moves which caret is primary (0-based document order).
        let mut e = ed();
        e.run_command(r#"insert "foo bar foo baz foo""#);
        e.run_command("TOP");
        e.run_command("CARETALL foo");
        e.run_command("PR $CARETP");
        assert_eq!(out(&e).as_deref(), Some("0")); // primary = the match nearest the cursor (the first)
        e.run_command("CARETPRIMARY 2");
        e.run_command("PR $CARETP");
        assert_eq!(out(&e).as_deref(), Some("2"));
    }

    #[test]
    fn caret_column_verb_and_gesture_spray_a_column_from_a_selection() {
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        let text = |e: &Editor| e.doc(e.active_index()).text();

        // Three equal-length lines; select a column (col 1) spanning all three, then CARETCOL.
        let setup = |e: &mut Editor| {
            e.run_command(r#"insert "abc""#);
            e.run_command("ENTER");
            e.run_command(r#"insert "def""#);
            e.run_command("ENTER");
            e.run_command(r#"insert "ghi""#); // "abc\ndef\nghi"
            // Build a column selection at col 1 spanning all three lines, using column-preserving
            // vertical motion (newline-length-agnostic — CRLF vs LF doesn't shift it).
            e.run_command("TOP");
            e.run_command("RIGHT"); // line 0 col 1
            e.run_command("DOWN SEL"); // extend to line 1 col 1
            e.run_command("DOWN SEL"); // extend to line 2 col 1
        };

        // The CARETCOL verb: one caret per line at col 1.
        let mut e = ed();
        setup(&mut e);
        e.run_command("CARETCOL");
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("3"));
        // Typing at all three carets, one undo unit — a prefix on every line. (ENTER inserts the
        // doc's line ending, CRLF on Windows, so compare newline-agnostic.)
        e.run_command(r#"insert "X""#);
        assert_eq!(text(&e).replace('\r', ""), "aXbc\ndXef\ngXhi");

        // The gesture: bare CARETADD with a block defined delegates to CARETCOL.
        let mut e = ed();
        setup(&mut e);
        e.run_command("CARETADD");
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("3"));

        // With NO selection, bare CARETADD still just plants at the primary (building state).
        let mut e = ed();
        e.run_command(r#"insert "abc""#);
        e.run_command("CARETADD");
        e.run_command("PR $CARETS");
        assert_eq!(out(&e).as_deref(), Some("2"));
    }

    #[test]
    fn li_special_reads_the_line_under_the_caret_without_the_newline() {
        let mut e = ed();
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        e.run_command(r#"insert "one""#);
        e.run_command("ENTER");
        e.run_command(r#"insert "two""#); // text: "one<nl>two", caret on line 2
        e.run_command("PR $LI");
        assert_eq!(out(&e).as_deref(), Some("two")); // line under caret, no trailing newline
        // Move up to the first line — $LI follows the caret.
        e.run_command("TOP");
        e.run_command("PR $LI");
        assert_eq!(out(&e).as_deref(), Some("one"));
    }

    #[test]
    fn doc_special_returns_the_whole_buffer() {
        let mut e = ed();
        e.run_command(r#"insert "alpha""#);
        e.run_command("ENTER");
        e.run_command(r#"insert "beta""#);
        e.run_command("ENTER");
        e.run_command(r#"insert "gamma""#); // "alpha<nl>beta<nl>gamma", caret at the end
        // $DOC is the ENTIRE buffer, independent of caret position (put it at the top to prove it).
        e.run_command("TOP");
        e.run_command("SET all = $DOC");
        let all = e
            .breakpoint_vars()
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>()
            .get("ALL")
            .cloned();
        // Equals the buffer's own text verbatim — three lines joined by the doc's newline (CRLF on
        // Windows), so compare to the live text rather than a hard-coded newline.
        let doc_text = e.document().text();
        assert_eq!(all.as_deref(), Some(doc_text.as_str()));
        assert!(doc_text.contains("alpha") && doc_text.contains("gamma")); // sanity: all lines present
    }

    #[test]
    fn enter_inserts_the_documents_newline_and_newline_special_reads_it() {
        use crate::document::LineEnding;
        let mut e = ed();
        let nl = LineEnding::os_default(); // new docs follow the OS
        // $NEWLINE reports the active document's style as a label.
        e.run_command("PR $NEWLINE");
        assert_eq!(e.output().last().map(String::as_str), Some(nl.label()));
        // ENTER inserts that exact sequence (\n or \r\n) into the document text.
        e.run_command(r#"insert "ab""#);
        e.run_command("ENTER");
        e.run_command(r#"insert "cd""#);
        assert_eq!(e.document().text(), format!("ab{}cd", nl.as_str()));
    }

    #[test]
    fn line_down_keeps_a_sticky_goal_column_and_horizontal_moves_reset_it() {
        let mut e = ed();
        e.run_command(r#"INSERT "first line""#);
        e.run_command(r#"INSERT "\n\nx\nlast line here""#); // line0, blank, "x", line3
        let l3 = e.document().text().rfind("last line here").unwrap();

        // Sticky column: from column 5, DOWN through a blank line and a 1-char line lands back at
        // column 5 — the column is maintained, not collapsed by the short lines.
        e.run_command("TF");
        e.run_command("JMP +5");
        e.run_command("LD"); // blank line
        e.run_command("LD"); // "x"
        e.run_command("LD"); // "last line here"
        assert_eq!(e.document().cursor(), l3 + 5, "sticky goal column across short/blank lines");

        // A horizontal move clears the goal: DOWN to the blank line, RIGHT, then DOWN lands at
        // column 0 (the new column) rather than the old goal of 5.
        e.run_command("TF");
        e.run_command("JMP +5");
        e.run_command("LD"); // blank (goal armed at 5)
        e.run_command("RIGHT"); // horizontal → resets the goal; caret now at column 0 of "x"
        e.run_command("LD"); // → "last line here" at column 0
        assert_eq!(e.document().cursor(), l3, "a horizontal move resets the goal column");
    }

    #[test]
    fn nl_special_is_the_documents_newline_characters() {
        let mut e = ed();
        // CRLF document → $NL is the two characters \r\n (the character form, not the "CRLF" label).
        e.run_command("NEWLINE crlf");
        e.run_command("PUT a");
        e.run_command("PUT $NL");
        e.run_command("PUT b");
        assert_eq!(e.document().text(), "a\r\nb");
        // Switch the document to LF → $NL becomes a lone \n.
        e.run_command("NEWLINE lf");
        e.run_command("DFA");
        e.run_command("DN");
        e.run_command("PUT a");
        e.run_command("PUT $NL");
        e.run_command("PUT b");
        assert_eq!(e.document().text(), "a\nb");
    }

    #[test]
    fn color_readback_through_the_cmline_submit_path() {
        // Mirror the GUI: set a colour, then type `MSG $FGCOLOR` into the CMline and submit it
        // exactly as pressing Enter does. The output the GUI reads back must carry the value.
        let mut e = ed();
        e.run_command("FGCOLOR 10 20 30");
        e.open_cmdline();
        e.insert_str("PR $FGCOLOR");
        e.submit_cmdline();
        assert_eq!(e.output().last().map(String::as_str), Some("10 20 30"));
    }

    #[test]
    fn quit_request_verify_force_and_dirty_sweep_helpers() {
        let mut e = Editor::new(typing_keymap());
        assert_eq!(e.take_quit(), None);
        // QUIT = verify (prompt first); QUIT/NV = force.
        e.run_command("QUIT");
        assert_eq!(e.take_quit(), Some(true));
        e.run_command("QUIT/NV");
        assert_eq!(e.take_quit(), Some(false));
        assert_eq!(e.take_quit(), None); // consumed

        // The dirty-tab sweep helpers: next_dirty_tab finds modified buffers; "discard" clears it.
        assert_eq!(e.next_dirty_tab(), None);
        e.run_command(r#"insert "hi""#);
        assert_eq!(e.next_dirty_tab(), Some(0));
        e.mark_active_saved(); // the QUIT "discard" choice
        assert_eq!(e.next_dirty_tab(), None);
    }

    #[test]
    fn binders_persist_through_savecfg_loadcfg() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        // A text binder (with a newline) and a script binder.
        e.run_command(r#"LDTX H "hello\nworld""#);
        e.run_command(r#"LDSC G "PR hi""#);
        e.run_command("SAVECFG");
        // Change them, then reload from the config.
        e.run_command(r#"LDTX H "x""#);
        e.run_command(r#"LDSC G "y""#);
        e.run_command("LOADCFG");
        // @H = the text (verbatim, incl. the newline); &G = the script source.
        e.run_command("PR @H");
        assert_eq!(e.output().last().map(String::as_str), Some("hello\nworld"));
        e.run_command("PR &G");
        assert_eq!(e.output().last().map(String::as_str), Some("PR hi"));
    }

    #[test]
    fn savecfg_persists_persistent_selection_mode() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        // Default is off → CPS in the config.
        e.run_command("SAVECFG cfg.cfg");
        assert!(e.host_mut().files.get("cfg.cfg").unwrap().contains("CPS"));
        // Turn it on → SPS round-trips.
        e.run_command("SPS");
        e.run_command("SAVECFG cfg.cfg");
        let cfg = e.host_mut().files.get("cfg.cfg").unwrap().clone();
        assert!(cfg.contains("SPS"), "cfg: {cfg}");
        // Running the saved script back restores the mode ($PS reads 1).
        e.run_command("CPS");
        e.run_command("LOADCFG cfg.cfg");
        e.run_command("PR $PS");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
    }

    #[test]
    fn savecfg_persists_dir_listing_behavior() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        // Defaults are off → the config records them explicitly (so they're captured even when the
        // user never issued the commands).
        e.run_command("SAVECFG cfg.cfg");
        let cfg = e.host_mut().files.get("cfg.cfg").unwrap().clone();
        assert!(cfg.contains("DIRNW off"), "cfg: {cfg}");
        assert!(cfg.contains("DIRRT off"), "cfg: {cfg}");
        // Turn both on → they round-trip, and loading the saved script restores them.
        e.run_command("DIRNW on\nDIRRT on\nSAVECFG cfg.cfg");
        let cfg = e.host_mut().files.get("cfg.cfg").unwrap().clone();
        assert!(cfg.contains("DIRNW on") && cfg.contains("DIRRT on"), "cfg: {cfg}");
        e.run_command("DIRNW off\nDIRRT off\nLOADCFG cfg.cfg");
        e.run_command("PR $DIRNW $DIRRT");
        assert_eq!(e.output().last().map(String::as_str), Some("1 1"));
    }

    #[test]
    fn lang_two_letter_shorthand_loads_the_dot_lang_file() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("fi.lang".into(), "menubar.file = Tiedosto\n".into());
        e.host_mut().files.insert("catalog.txt".into(), "menubar.file = Zzz\n".into());

        // `LANG fi` → loads `fi.lang`; $LANG reports the (resolved) path it loaded.
        e.run_command("LANG fi");
        assert_eq!(e.host().lang_get("menubar.file"), Some("Tiedosto"));
        e.run_command("PR $LANG");
        assert_eq!(e.output().last().map(String::as_str), Some("fi.lang"));

        // A name with an extension is the explicit-path form — no `.lang` appended.
        e.run_command("LANG catalog.txt");
        assert_eq!(e.host().lang_get("menubar.file"), Some("Zzz"));
        e.run_command("PR $LANG");
        assert_eq!(e.output().last().map(String::as_str), Some("catalog.txt"));

        // A 3-letter argument is NOT shorthand — it's a literal path (here, missing → traps).
        e.run_command("LANG eng");
        assert!(e.host().lang_get("menubar.file").is_some(), "failed load left the prior catalog intact");
    }

    #[test]
    fn lang_overlay_layers_user_overrides_with_self_heal() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert(
            "fi.lang".into(),
            "menubar.file = Tiedosto   # File\n\
             menu.file.save = Tallenna   # Save\n\
             menu.file.open = Avaa   # Open\n"
                .into(),
        );
        // A user override file: one entry whose English anchor still matches the base (applies), one
        // whose anchor is stale — the base's English for that key has since changed (self-heal skips).
        e.host_mut().files.insert(
            "fi.local.lang".into(),
            "menu.file.save = Tallenna muutokset   # Save\n\
             menu.file.open = Avaa tiedosto   # Open file\n"
                .into(),
        );
        e.run_command("LANG fi");

        // Fresh anchor → the override wins over the base.
        assert_eq!(e.host().lang_get("menu.file.save"), Some("Tallenna muutokset"));
        // Stale anchor ("Open file" != base "Open") → override skipped, base value stands.
        assert_eq!(e.host().lang_get("menu.file.open"), Some("Avaa"));
        // A key the overlay never mentions is untouched.
        assert_eq!(e.host().lang_get("menubar.file"), Some("Tiedosto"));
        // $LANG still reports the BASE path (the overlay is auto-discovered, not separately tracked).
        e.run_command("PR $LANG");
        assert_eq!(e.output().last().map(String::as_str), Some("fi.lang"));

        // Switching away and back re-applies the overlay (it isn't persisted, it's re-discovered).
        e.run_command("LANG off");
        assert_eq!(e.host().lang_get("menu.file.save"), None);
        e.run_command("LANG fi");
        assert_eq!(e.host().lang_get("menu.file.save"), Some("Tallenna muutokset"));
    }

    #[test]
    fn spellengine_switches_and_persists() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        // Harper is the default → SaveCfg stays silent about the engine.
        e.run_command("SAVECFG cfg.cfg");
        assert!(!e.host_mut().files.get("cfg.cfg").unwrap().contains("SPELLENGINE"), "harper default → no line");

        // Switch to Hunspell and set its knobs — all three persist.
        e.run_command("SPELLENGINE hunspell");
        e.run_command("SPELLDICT en_US,fi_FI");
        e.run_command("SPELLBIN C:/tools/hunspell.exe");
        assert_eq!(e.host().spell_engine(), crate::lint::SpellEngine::Hunspell);
        // $-reads (drive the menu checkmark + scripting).
        e.run_command("PR $SPELLENGINE $SPELLDICT $SPELLBIN");
        assert_eq!(e.output().last().map(String::as_str), Some("hunspell en_US,fi_FI C:/tools/hunspell.exe"));
        e.run_command("SAVECFG cfg.cfg");
        let cfg = e.host_mut().files.get("cfg.cfg").unwrap().clone();
        assert!(cfg.contains("SPELLENGINE hunspell"), "cfg: {cfg}");
        assert!(cfg.contains("SPELLDICT en_US,fi_FI") && cfg.contains("SPELLBIN C:/tools/hunspell.exe"));

        // An unknown engine traps and leaves the setting unchanged.
        e.run_command("SPELLENGINE bogus");
        assert_eq!(e.host().spell_engine(), crate::lint::SpellEngine::Hunspell);

        // LOADCFG restores the engine.
        e.run_command("SPELLENGINE harper");
        e.run_command("LOADCFG cfg.cfg");
        assert_eq!(e.host().spell_engine(), crate::lint::SpellEngine::Hunspell);
    }

    #[test]
    fn savecfg_persists_the_lang_catalog_and_reads_back_as_lang() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("cat.txt".into(), "menubar.file = Zzz\n".into());

        // No catalog → $LANG is empty and SaveCfg emits no LANG line.
        e.run_command("PR $LANG");
        assert_eq!(e.output().last().map(String::as_str), Some(""));
        e.run_command("SAVECFG cfg.cfg");
        assert!(!e.host_mut().files.get("cfg.cfg").unwrap().contains("\nLANG "), "no LANG line yet");

        // Load a catalog → $LANG reports its path and SaveCfg records a re-runnable LANG line.
        e.run_command("LANG cat.txt");
        e.run_command("PR $LANG");
        assert_eq!(e.output().last().map(String::as_str), Some("cat.txt"));
        e.run_command("SAVECFG cfg.cfg");
        assert!(e.host_mut().files.get("cfg.cfg").unwrap().contains("LANG cat.txt"), "cfg records the catalog");

        // Clear, then LOADCFG restores it (the saved LANG line reloads the catalog).
        e.run_command("LANG off");
        assert_eq!(e.host().lang_get("menubar.file"), None);
        e.run_command("PR $LANG");
        assert_eq!(e.output().last().map(String::as_str), Some(""));
        e.run_command("LOADCFG cfg.cfg");
        assert_eq!(e.host().lang_get("menubar.file"), Some("Zzz"), "LOADCFG reloaded the catalog");
        e.run_command("PR $LANG");
        assert_eq!(e.output().last().map(String::as_str), Some("cat.txt"));
    }

    #[test]
    fn cmh_with_an_argument_stashes_a_quoted_string_with_vars_expanded() {
        let mut e = Editor::new(typing_keymap());
        // `CMH "command"` adds the quoted string to the top of history without running it.
        e.run_command(r#"CMH "PR hello""#);
        assert_eq!(e.command_history(), ["PR hello"]);
        // `$vars` are EXPANDED at stash time (not stored literally).
        e.run_command("SET x = 99");
        e.run_command(r#"CMH "PR $x""#);
        assert_eq!(e.command_history(), ["PR 99", "PR hello"]);
        // Dedup + move-to-front works for the arg form too.
        e.run_command(r#"CMH "PR hello""#);
        assert_eq!(e.command_history(), ["PR hello", "PR 99"]);
        // Bare `CMH` still stashes the current (empty) cmline → a no-op here.
        e.run_command("CMH");
        assert_eq!(e.command_history(), ["PR hello", "PR 99"]);
    }

    #[test]
    fn history_picker_helpers_promote_on_run_and_load_on_shift_enter() {
        let mut e = Editor::new(typing_keymap());
        for c in ["PR one", "PR two", "PR three"] {
            e.run_command(&format!("CMH \"{c}\"")); // seed history newest-first: three, two, one
        }
        assert_eq!(e.command_history(), ["PR three", "PR two", "PR one"]);

        // Enter on an entry: the picker promotes it to the top (then runs it) — like re-running.
        e.push_history("PR one");
        assert_eq!(e.command_history(), ["PR one", "PR three", "PR two"]);
        assert_eq!(e.command_history().len(), 3, "dedup — no duplicate entry");

        // Shift+Enter: load the entry into the CMline to edit, WITHOUT running or reordering.
        let before = e.command_history().to_vec();
        e.load_cmline("PR two");
        assert!(e.cmline_open(), "load_cmline focuses the command line");
        assert_eq!(e.command_line().text(), "PR two");
        assert_eq!(e.command_history(), before.as_slice(), "loading doesn't reorder the history");
    }

    #[test]
    fn cmh_special_returns_history_joined_with_os_newlines() {
        let mut e = Editor::new(typing_keymap());
        for cmd in ["PR one", "PR two"] {
            e.run_command("BC");
            e.insert_str(cmd);
            e.submit_cmdline();
        }
        // `$CMH` = the whole history, newest first, joined with the OS newline.
        let sep = if cfg!(windows) { "\r\n" } else { "\n" };
        let want = format!("PR two{sep}PR one");
        e.run_command("PR $CMH");
        assert_eq!(e.output().last(), Some(&want));
        // Empty history → empty string.
        let mut e2 = Editor::new(typing_keymap());
        e2.run_command("PR $CMH");
        assert_eq!(e2.output().last().map(String::as_str), Some(""));
    }

    #[test]
    fn cmh_stashes_the_cmline_into_history_with_dedup() {
        let mut e = Editor::new(typing_keymap());
        // CMH adds the current command line to the top of history WITHOUT running it.
        e.run_command("BC");
        e.insert_str("PR alpha");
        e.run_command("CMH");
        assert_eq!(e.command_history(), ["PR alpha"]);
        // A different stash goes on top.
        e.run_command("BC");
        e.insert_str("PR beta");
        e.run_command("CMH");
        assert_eq!(e.command_history(), ["PR beta", "PR alpha"]);
        // Re-stashing an existing entry dedups + moves to front (no duplicate).
        e.run_command("BC");
        e.insert_str("PR alpha");
        e.run_command("CMH");
        assert_eq!(e.command_history(), ["PR alpha", "PR beta"]);
        // A blank command line is a no-op.
        e.run_command("BC");
        e.run_command("CMH");
        assert_eq!(e.command_history(), ["PR alpha", "PR beta"]);
    }

    #[test]
    fn history_rerun_moves_to_front_and_dedups() {
        let mut e = Editor::new(typing_keymap());
        for cmd in ["PR one", "PR two", "PR three"] {
            e.run_command("BC");
            e.insert_str(cmd);
            e.submit_cmdline();
        }
        assert_eq!(e.command_history(), ["PR three", "PR two", "PR one"]);
        // Re-running an OLDER command moves it to the top — and leaves no duplicate behind.
        e.run_command("BC");
        e.insert_str("PR one");
        e.submit_cmdline();
        assert_eq!(e.command_history(), ["PR one", "PR three", "PR two"]);
        // Re-running the current top is a no-op for order (still deduped, still length 3).
        e.run_command("BC");
        e.insert_str("PR one");
        e.submit_cmdline();
        assert_eq!(e.command_history(), ["PR one", "PR three", "PR two"]);
    }

    #[test]
    fn command_history_recall_delete_and_config_round_trip() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        // Submit a few commands via the CMline — each is recorded in history.
        for cmd in ["PR one", "PR two", "PR three"] {
            e.run_command("BC"); // blank + focus the cmline first
            e.insert_str(cmd);
            e.submit_cmdline();
        }
        // Stored newest-first (index 0 = most recent).
        assert_eq!(e.command_history(), ["PR three", "PR two", "PR one"]);
        // Re-submitting the current top keeps it on top (deduped — the history never grows here).
        e.run_command("BC");
        e.insert_str("PR three");
        e.submit_cmdline();
        assert_eq!(e.command_history().len(), 3);

        // Filtered views: a number = the last N (newest first); text = commands containing it.
        let names = |v: Vec<(usize, String)>| v.into_iter().map(|(_, c)| c).collect::<Vec<_>>();
        assert_eq!(names(e.history_view("2")), ["PR three", "PR two"]);
        assert_eq!(names(e.history_view("two")), ["PR two"]);
        assert_eq!(e.history_view("").len(), 3);

        // Up/Down recall walks the history into the CMline (Up = older, starting from newest).
        e.run_command("BC"); // blank + focus the cmline
        e.history_prev();
        assert_eq!(e.command_line().text(), "PR three");
        e.history_prev();
        assert_eq!(e.command_line().text(), "PR two");
        e.history_next();
        assert_eq!(e.command_line().text(), "PR three");
        e.history_next(); // back to the (empty) in-progress line
        assert_eq!(e.command_line().text(), "");

        // Delete the newest (index 0, the picker's Del on the top row).
        e.delete_history(0);
        assert_eq!(e.command_history(), ["PR two", "PR one"]);

        // SaveCfg captures the history (newest first); LoadCM replaces it; LoadCfg restores it.
        e.run_command("SAVECFG");
        e.run_command(r#"LoadCM "X""#);
        assert_eq!(e.command_history(), ["X"]);
        e.run_command("LOADCFG");
        assert_eq!(e.command_history(), ["PR two", "PR one"]);
    }

    #[test]
    fn caret_color_and_shape_set_and_read() {
        use crate::host::CaretShape;
        let mut e = ed();
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        // Defaults: red bar.
        assert_eq!(e.caret_color(), (220, 40, 40));
        assert_eq!(e.caret_shape(), CaretShape::Bar);
        e.run_command("PR $CRSHAPE");
        assert_eq!(out(&e).as_deref(), Some("bar"));
        // Colour set + read.
        e.run_command("CRCOLOR 10 200 90");
        assert_eq!(e.caret_color(), (10, 200, 90));
        e.run_command("PR $CRCOLOR");
        assert_eq!(out(&e).as_deref(), Some("10 200 90"));
        // Shape set, plus an alias.
        e.run_command("CRSHAPE block");
        assert_eq!(e.caret_shape(), CaretShape::Block);
        e.run_command("CRSHAPE underscore"); // alias → underline
        assert_eq!(e.caret_shape(), CaretShape::Underline);
        e.run_command("PR $CRSHAPE");
        assert_eq!(out(&e).as_deref(), Some("underline"));
        // Bad shape is trappable and leaves it unchanged.
        e.run_command("CRSHAPE diamond");
        assert_eq!(e.caret_shape(), CaretShape::Underline);
        assert!(e.last_error().unwrap_or_default().contains("expected bar|block|underline"));
    }

    #[test]
    fn cmline_caret_shape_is_independent_of_text_caret() {
        use crate::host::CaretShape;
        let mut e = ed();
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        // Default matches the text caret's default (bar), but they're separate knobs.
        assert_eq!(e.cmline_caret_shape(), CaretShape::Bar);
        e.run_command("PR $CMCRSHAPE");
        assert_eq!(out(&e).as_deref(), Some("bar"));
        e.run_command("CMCRSHAPE underscore"); // alias → underline
        assert_eq!(e.cmline_caret_shape(), CaretShape::Underline);
        assert_eq!(e.caret_shape(), CaretShape::Bar, "CMCRSHAPE must not touch the text caret");
        e.run_command("PR $CMCRSHAPE");
        assert_eq!(out(&e).as_deref(), Some("underline"));
        // Bad value is trappable and leaves it unchanged.
        e.run_command("CMCRSHAPE diamond");
        assert_eq!(e.cmline_caret_shape(), CaretShape::Underline);
        assert!(e.last_error().unwrap_or_default().contains("expected bar|block|underline"));
        // CMCRCOLOR is likewise a separate colour from the text caret's CRCOLOR.
        assert_eq!(e.cmline_caret_color(), e.caret_color(), "defaults match the text caret");
        e.run_command("CMCRCOLOR 10 20 30");
        assert_eq!(e.cmline_caret_color(), (10, 20, 30));
        assert_ne!(e.caret_color(), (10, 20, 30), "CMCRCOLOR must not touch the text caret colour");
        e.run_command("PR $CMCRCOLOR");
        assert_eq!(out(&e).as_deref(), Some("10 20 30"));
    }

    #[test]
    fn editor_colors_rgb_hsl_and_read_specials() {
        let mut e = ed();
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        // Defaults: dark text on white.
        assert_eq!(e.fg_color(), (30, 30, 30));
        assert_eq!(e.bg_color(), (255, 255, 255));
        // RGB form sets the triple; $FGCOLOR reads it back as "R G B".
        e.run_command("FGCOLOR 200 100 50");
        assert_eq!(e.fg_color(), (200, 100, 50));
        e.run_command("PR $FGCOLOR");
        assert_eq!(out(&e).as_deref(), Some("200 100 50"));
        e.run_command("BGCOLOR 0 0 0");
        assert_eq!(e.bg_color(), (0, 0, 0));
        e.run_command("PR $BGCOLOR");
        assert_eq!(out(&e).as_deref(), Some("0 0 0"));
        // /HSL form (0-255 scale): H any, S=0 → a pure grey at lightness L.
        e.run_command("FGCOLOR/HSL 128 0 128");
        assert_eq!(e.fg_color(), (128, 128, 128));
        // Out-of-range and wrong arity are trappable (no panic; value unchanged).
        e.run_command("BGCOLOR 0 0 300");
        assert_eq!(e.bg_color(), (0, 0, 0));
        e.run_command("FGCOLOR 1 2");
        assert_eq!(e.fg_color(), (128, 128, 128));
        // UI-chrome colours are a SEPARATE pair, independent of the editor canvas colours.
        assert_eq!(e.ui_fg_color(), (30, 30, 30));
        assert_eq!(e.ui_bg_color(), (255, 255, 255));
        e.run_command("UIFGCOLOR 90 91 92");
        e.run_command("UIBGCOLOR 5 6 7");
        assert_eq!(e.ui_fg_color(), (90, 91, 92));
        assert_eq!(e.ui_bg_color(), (5, 6, 7));
        e.run_command("PR $UIBGCOLOR");
        assert_eq!(out(&e).as_deref(), Some("5 6 7"));
        // Editor canvas colours are untouched by the UI commands.
        assert_eq!(e.fg_color(), (128, 128, 128));
        assert_eq!(e.bg_color(), (0, 0, 0));
        // SEPCOLOR (separator rules + tab dividers) is its own independent triple, RGB + /HSL.
        assert_eq!(e.sep_color(), (190, 190, 190)); // default grey
        e.run_command("SEPCOLOR 10 20 30");
        assert_eq!(e.sep_color(), (10, 20, 30));
        e.run_command("PR $SEPCOLOR");
        assert_eq!(out(&e).as_deref(), Some("10 20 30"));
        e.run_command("SEPCOLOR/HSL 200 0 64"); // S=0 → grey at L
        assert_eq!(e.sep_color(), (64, 64, 64));
        // Untouched by the other colour commands.
        assert_eq!(e.ui_fg_color(), (90, 91, 92));
        // TXT dialog colours: own independent pair (default dark-on-white), RGB + read-back.
        assert_eq!((e.txt_fg_color(), e.txt_bg_color()), ((30, 30, 30), (255, 255, 255)));
        e.run_command("TXTFGCOLOR 1 2 3");
        e.run_command("TXTBGCOLOR 240 240 240");
        assert_eq!((e.txt_fg_color(), e.txt_bg_color()), ((1, 2, 3), (240, 240, 240)));
        e.run_command("PR $TXTFGCOLOR");
        assert_eq!(out(&e).as_deref(), Some("1 2 3"));
    }

    #[test]
    fn ui_sizing_knobs_set_clamp_reset_and_read() {
        let mut e = ed();
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        // Defaults.
        assert_eq!(e.ui_scale(), 1.0);
        assert_eq!(e.editor_font(), 22.0);
        // Set each; $-vars read back.
        e.run_command("UISCALE 1.5");
        assert_eq!(e.ui_scale(), 1.5);
        e.run_command("PR $UISCALE");
        assert_eq!(out(&e).as_deref(), Some("1.5"));
        e.run_command("EDFONT 18");
        assert_eq!(e.editor_font(), 18.0);
        e.run_command("PR $EDFONT");
        assert_eq!(out(&e).as_deref(), Some("18"));
        e.run_command("LSP 160");
        assert_eq!(e.leading(), 160.0);
        // OF sets BOTH margins; OFL/OFR set each; $OF reads the left.
        e.run_command("OF 4");
        assert_eq!((e.margin_left(), e.margin_right()), (4.0, 4.0));
        e.run_command("PR $OF");
        assert_eq!(out(&e).as_deref(), Some("4"));
        e.run_command("OFL 6");
        e.run_command("OFR 2");
        assert_eq!((e.margin_left(), e.margin_right()), (6.0, 2.0));
        e.run_command("PR $OFR");
        assert_eq!(out(&e).as_deref(), Some("2"));
        // Out-of-range clamps (scale max 4.0).
        e.run_command("UISCALE 99");
        assert_eq!(e.ui_scale(), 4.0);
        // Bad number → editor surfaces a trappable error (no panic), value unchanged.
        e.run_command("EDFONT abc");
        assert!(e.last_error().unwrap().contains("expected a number"));
        assert_eq!(e.editor_font(), 18.0);
        // TXT dialog knobs: own font size + zoom, set/read/clamp.
        assert_eq!(e.txt_font(), 14.0);
        assert_eq!(e.txt_scale(), 1.0);
        e.run_command("TXTFONT 20\nPR $TXTFONT");
        assert_eq!(out(&e).as_deref(), Some("20"));
        e.run_command("TXTSCALE 99\nPR $TXTSCALE"); // clamps to 4
        assert_eq!(out(&e).as_deref(), Some("4"));
        // UIRESET restores defaults (including the TXT knobs).
        e.run_command("UIRESET");
        assert_eq!(e.ui_scale(), 1.0);
        assert_eq!(e.editor_font(), 22.0);
        assert_eq!((e.margin_left(), e.margin_right()), (0.0, 0.0));
        assert_eq!((e.txt_font(), e.txt_scale()), (14.0, 1.0));
    }

    #[test]
    fn relative_sizes_adjust_the_current_value() {
        let mut e = Editor::new(typing_keymap());
        // A leading +/- adjusts relative to the current value; a bare number replaces it.
        assert_eq!(e.editor_font(), 22.0);
        e.run_command("EDFONT +3"); // 22 -> 25
        assert_eq!(e.editor_font(), 25.0);
        e.run_command("EDFONT -5"); // 25 -> 20
        assert_eq!(e.editor_font(), 20.0);
        e.run_command("EDFONT 14"); // bare = absolute
        assert_eq!(e.editor_font(), 14.0);
        // Relative respects the per-knob clamp (editor font floor = 6pt).
        e.run_command("EDFONT -100");
        assert_eq!(e.editor_font(), 6.0);
        // Works for the other font knobs too.
        assert_eq!(e.chrome_font(), 14.0);
        e.run_command("UIFONT +2");
        assert_eq!(e.chrome_font(), 16.0);
        assert_eq!(e.txt_font(), 14.0);
        e.run_command("TXTFONT -4");
        assert_eq!(e.txt_font(), 10.0);
        // And generically for the shared sizing knobs (scale, leading, margins).
        e.run_command("OF 4");
        e.run_command("OF +2"); // both margins 4 -> 6
        assert_eq!((e.margin_left(), e.margin_right()), (6.0, 6.0));
    }

    #[test]
    fn edfamily_sets_editor_font_family_resets_and_persists() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        // Default: none ($EDFAMILY empty).
        assert_eq!(e.editor_font_family(), None);
        e.run_command("PR $EDFAMILY");
        assert_eq!(out(&e).as_deref(), Some(""));
        // Set a family (a multi-word name is the whole trimmed argument).
        e.run_command("EDFAMILY Cascadia Mono");
        assert_eq!(e.editor_font_family(), Some("Cascadia Mono"));
        e.run_command("PR $EDFAMILY");
        assert_eq!(out(&e).as_deref(), Some("Cascadia Mono"));
        // `default` (and bare/"") reset to the GUI default.
        e.run_command("EDFAMILY default");
        assert_eq!(e.editor_font_family(), None);
        // SAVECFG emits EDFAMILY when set, so it round-trips.
        e.run_command("EDFAMILY Consolas\nSAVECFG cfg.cfg");
        assert!(e.host_mut().files.get("cfg.cfg").unwrap().contains("EDFAMILY Consolas"));
    }

    #[test]
    fn uifamily_and_txtfamily_set_reset_and_persist() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        // Both default to none.
        assert_eq!(e.ui_font_family(), None);
        assert_eq!(e.txt_font_family(), None);
        // Set each (multi-word names are the whole trimmed argument).
        e.run_command("UIFAMILY Segoe UI");
        e.run_command("TXTFAMILY Cascadia Mono");
        assert_eq!(e.ui_font_family(), Some("Segoe UI"));
        assert_eq!(e.txt_font_family(), Some("Cascadia Mono"));
        e.run_command("PR $UIFAMILY");
        assert_eq!(out(&e).as_deref(), Some("Segoe UI"));
        e.run_command("PR $TXTFAMILY");
        assert_eq!(out(&e).as_deref(), Some("Cascadia Mono"));
        // `default` resets independently.
        e.run_command("UIFAMILY default");
        assert_eq!(e.ui_font_family(), None);
        assert_eq!(e.txt_font_family(), Some("Cascadia Mono"));
        // SAVECFG emits each only when set.
        e.run_command("UIFAMILY Tahoma\nSAVECFG cfg.cfg");
        let cfg = e.host_mut().files.get("cfg.cfg").unwrap().clone();
        assert!(cfg.contains("UIFAMILY Tahoma"));
        assert!(cfg.contains("TXTFAMILY Cascadia Mono"));
    }

    #[test]
    fn font_style_verbs_set_parse_read_and_persist() {
        use crate::host::FontStyle;
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        // Default normal for all three.
        assert_eq!(e.editor_font_style(), FontStyle::Normal);
        assert_eq!(e.ui_font_style(), FontStyle::Normal);
        assert_eq!(e.txt_font_style(), FontStyle::Normal);
        // Keyword parsing (order/dash/case-insensitive for the compound).
        e.run_command("EDSTYLE bold");
        assert_eq!(e.editor_font_style(), FontStyle::Bold);
        e.run_command("UISTYLE Italic-Bold");
        assert_eq!(e.ui_font_style(), FontStyle::BoldItalic);
        e.run_command("TXTSTYLE italic");
        assert_eq!(e.txt_font_style(), FontStyle::Italic);
        // Readbacks return the canonical keyword.
        e.run_command("PR $EDSTYLE");
        assert_eq!(out(&e).as_deref(), Some("bold"));
        e.run_command("PR $UISTYLE");
        assert_eq!(out(&e).as_deref(), Some("bolditalic"));
        // `regular`/`normal` reset.
        e.run_command("EDSTYLE regular");
        assert_eq!(e.editor_font_style(), FontStyle::Normal);
        // An invalid keyword is rejected (trappable error → $ERROR; the style is left unchanged).
        e.run_command("UISTYLE wobbly");
        assert_eq!(e.ui_font_style(), FontStyle::BoldItalic);
        // SAVECFG emits only the non-normal styles.
        e.run_command("EDSTYLE bolditalic\nTXTSTYLE normal\nSAVECFG cfg.cfg");
        let cfg = e.host_mut().files.get("cfg.cfg").unwrap().clone();
        assert!(cfg.contains("EDSTYLE bolditalic"));
        assert!(cfg.contains("UISTYLE bolditalic"));
        assert!(!cfg.contains("TXTSTYLE")); // normal is the default → not emitted
    }

    #[test]
    fn font_picker_verbs_set_request_for_gui() {
        use crate::host::FontPickerKind;
        let mut e = Editor::new(typing_keymap());
        // Nothing pending by default.
        assert_eq!(e.take_font_picker_request(), None);
        // Each plural verb records its target for the GUI to drain, consumed once.
        e.run_command("EDFONTS");
        assert_eq!(e.take_font_picker_request(), Some(FontPickerKind::Editor));
        assert_eq!(e.take_font_picker_request(), None);
        e.run_command("UIFONTS");
        assert_eq!(e.take_font_picker_request(), Some(FontPickerKind::Ui));
        e.run_command("TXTFONTS");
        assert_eq!(e.take_font_picker_request(), Some(FontPickerKind::Txt));
    }

    #[test]
    fn lint_underline_colors_set_read_and_persist() {
        use crate::host::ColorTarget;
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        // Defaults: spelling red, grammar blue.
        assert_eq!(e.spell_color(), (220, 50, 50));
        assert_eq!(e.grammar_color(), (60, 110, 220));
        e.run_command("SPELLCOLOR 200 0 0");
        assert_eq!(e.spell_color(), (200, 0, 0));
        e.run_command("GRAMMARCOLOR 0 120 90");
        assert_eq!(e.grammar_color(), (0, 120, 90));
        e.run_command("PR $SPELLCOLOR");
        assert_eq!(e.output().last().map(String::as_str), Some("200 0 0"));
        e.run_command("PR $GRAMMARCOLOR");
        assert_eq!(e.output().last().map(String::as_str), Some("0 120 90"));
        // Picker verbs record the target.
        e.run_command("SPELLCOLORS");
        assert_eq!(e.take_color_picker_request(), Some(ColorTarget::SpellUnderline));
        e.run_command("GRAMMARCOLORS");
        assert_eq!(e.take_color_picker_request(), Some(ColorTarget::GrammarUnderline));
        // SaveCfg round-trip.
        e.run_command("SAVECFG cfg.cfg");
        let cfg = e.host_mut().files.get("cfg.cfg").unwrap().clone();
        assert!(cfg.contains("SPELLCOLOR 200 0 0"));
        assert!(cfg.contains("GRAMMARCOLOR 0 120 90"));
    }

    #[test]
    fn findcolor_sets_reads_and_persists() {
        use crate::host::ColorTarget;
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        // Defaults: yellow highlight band + a strong/intense match-text colour.
        assert_eq!(e.find_bg_color(), (250, 230, 90));
        assert_eq!(e.find_fg_color(), (20, 20, 20));
        e.run_command("FINDBGCOLOR 240 200 40");
        assert_eq!(e.find_bg_color(), (240, 200, 40));
        e.run_command("FINDFGCOLOR 10 10 10");
        assert_eq!(e.find_fg_color(), (10, 10, 10));
        e.run_command("PR $FINDBGCOLOR");
        assert_eq!(e.output().last().map(String::as_str), Some("240 200 40"));
        e.run_command("PR $FINDFGCOLOR");
        assert_eq!(e.output().last().map(String::as_str), Some("10 10 10"));
        // Picker verbs record the target.
        e.run_command("FINDBGCOLORS");
        assert_eq!(e.take_color_picker_request(), Some(ColorTarget::FindBg));
        e.run_command("FINDFGCOLORS");
        assert_eq!(e.take_color_picker_request(), Some(ColorTarget::FindFg));
        // SaveCfg round-trip persists both.
        e.run_command("SAVECFG cfg.cfg");
        let cfg = e.host_mut().files.get("cfg.cfg").unwrap().clone();
        assert!(cfg.contains("FINDBGCOLOR 240 200 40"));
        assert!(cfg.contains("FINDFGCOLOR 10 10 10"));
    }

    #[test]
    fn selcolor_sets_reads_and_persists() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        // Defaults are the prior hardcoded selection band / selected-text colours.
        assert_eq!(e.sel_bg_color(), (180, 210, 255));
        assert_eq!(e.sel_fg_color(), (20, 20, 20));
        e.run_command("SELBGCOLOR 60 120 200");
        assert_eq!(e.sel_bg_color(), (60, 120, 200));
        e.run_command("SELFGCOLOR 250 250 250");
        assert_eq!(e.sel_fg_color(), (250, 250, 250));
        e.run_command("PR $SELBGCOLOR");
        assert_eq!(e.output().last().map(String::as_str), Some("60 120 200"));
        e.run_command("PR $SELFGCOLOR");
        assert_eq!(e.output().last().map(String::as_str), Some("250 250 250"));
        // SaveCfg round-trip persists both.
        e.run_command("SAVECFG cfg.cfg");
        let cfg = e.host_mut().files.get("cfg.cfg").unwrap().clone();
        assert!(cfg.contains("SELBGCOLOR 60 120 200"));
        assert!(cfg.contains("SELFGCOLOR 250 250 250"));
    }

    #[test]
    fn color_picker_verbs_request_the_dialog() {
        use crate::highlight::TokenKind;
        use crate::host::ColorTarget;
        let mut e = Editor::new(typing_keymap());
        assert_eq!(e.take_color_picker_request(), None);
        // Each plural verb records its colour target, consumed once.
        e.run_command("FGCOLORS");
        assert_eq!(e.take_color_picker_request(), Some(ColorTarget::Fg));
        assert_eq!(e.take_color_picker_request(), None);
        e.run_command("BGCOLORS");
        assert_eq!(e.take_color_picker_request(), Some(ColorTarget::Bg));
        e.run_command("UIFGCOLORS");
        assert_eq!(e.take_color_picker_request(), Some(ColorTarget::UiFg));
        e.run_command("CRCOLORS");
        assert_eq!(e.take_color_picker_request(), Some(ColorTarget::Caret));
        e.run_command("SEPCOLORS");
        assert_eq!(e.take_color_picker_request(), Some(ColorTarget::Separator));
        e.run_command("TXTBGCOLORS");
        assert_eq!(e.take_color_picker_request(), Some(ColorTarget::TxtBg));
        e.run_command("SELBGCOLORS");
        assert_eq!(e.take_color_picker_request(), Some(ColorTarget::SelBg));
        e.run_command("SELFGCOLORS");
        assert_eq!(e.take_color_picker_request(), Some(ColorTarget::SelFg));
        // `HLCOLORS <kind>` targets a highlight kind — built-in or grammar-defined (any name).
        e.run_command("HLCOLORS keyword");
        assert_eq!(e.take_color_picker_request(), Some(ColorTarget::Highlight(TokenKind::Keyword)));
        e.run_command("HLCOLORS macro");
        assert_eq!(
            e.take_color_picker_request(),
            Some(ColorTarget::Highlight(TokenKind::Other("macro".into())))
        );
        // `TABBGCOLORS <state>` targets a tab-title state; an unknown state is rejected (no request).
        use crate::host::TabState;
        e.run_command("TABBGCOLORS active");
        assert_eq!(e.take_color_picker_request(), Some(ColorTarget::Tab(TabState::Active)));
        e.run_command("TABBGCOLORS hover");
        assert_eq!(e.take_color_picker_request(), Some(ColorTarget::Tab(TabState::Hover)));
        e.run_command("TABBGCOLORS bogus");
        assert_eq!(e.take_color_picker_request(), None);
    }

    #[test]
    fn cs_special_reports_caret_on_selection() {
        let mut e = Editor::new(typing_keymap());
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        e.run_command("insert \"hello\"");
        // No selection → caret is not on a selection.
        e.run_command("PR $CS");
        assert_eq!(out(&e).as_deref(), Some("0"));
        // Make a selection; the caret sits at its active end → on the selection.
        e.run_command("TOP");
        e.run_command("RIGHT SEL");
        e.run_command("PR $CS");
        assert_eq!(out(&e).as_deref(), Some("1"));
    }

    #[test]
    fn counts_report_doc_and_selection() {
        let mut e = ed();
        e.run_command("insert \"the quick brown fox\""); // 4 words, 19 chars
        // Whole document (no selection).
        e.run_command("WORDCOUNT");
        assert_eq!(e.status_message(), Some("Document: 4 words"));
        e.run_command("CHARCOUNT");
        assert_eq!(e.status_message(), Some("Document: 19 characters"));
        // $WC special = whole-document word count (MSG echoes it to output).
        e.run_command("PR $WC");
        assert_eq!(e.output().last().map(String::as_str), Some("4"));
        // Selection: select "the quick" (chars 0..9, 2 words).
        e.run_command("TOP");
        e.run_command("DF");
        for _ in 0..9 {
            e.run_command("RIGHT");
        }
        e.run_command("WORDCOUNT");
        assert_eq!(e.status_message(), Some("Selection: 2 words"));
        e.run_command("CHARCOUNT");
        assert_eq!(e.status_message(), Some("Selection: 9 characters"));
    }

    #[test]
    fn extend_search_switch_grows_a_define_to_the_match() {
        // The "delete to the next period" gesture: DF at the cursor, then SE/X extends the define
        // through the match instead of replacing the selection with just ".".
        let mut e = ed();
        e.run_command("insert \"hello world. next sentence.\"");
        e.run_command("TOP");
        e.run_command("DF");
        e.run_command("SE/X /./"); // extend to the first period (index 11; caret lands at 12)
        assert_eq!(e.document().selection_span(), Some((0, 12)));
        // /X leaves the define still auto-tracking, so a follow-up CL shrinks the span to exclude
        // the period — the full "delete up to (not including) the next period" gesture.
        e.run_command("CL");
        assert_eq!(e.document().selection_span(), Some((0, 11)));

        // A fresh buffer: while a define is OPEN, a PLAIN SE also extends (the define auto-tracks),
        // exactly like /X — the classic `DF; SE` gesture. (/X is what extends when NO define is open.)
        let mut e = ed();
        e.run_command("insert \"hello world. next sentence.\"");
        e.run_command("TOP");
        e.run_command("DF");
        e.run_command("SE /./");
        assert_eq!(e.document().selection_span(), Some((0, 12)));
        // With NO define open, a plain SE replaces the selection with just the matched ".".
        let mut e = ed();
        e.run_command("insert \"hello world. next sentence.\"");
        e.run_command("TOP");
        e.run_command("SE /./"); // no DF
        assert_eq!(e.document().selection_span(), Some((11, 12)));
    }

    #[test]
    fn counts_singular_and_extra_whitespace() {
        let mut e = ed();
        e.run_command("insert \"  one  \""); // padding + runs collapse to 1 word, 7 chars
        e.run_command("WORDCOUNT");
        assert_eq!(e.status_message(), Some("Document: 1 word")); // singular
        e.run_command("CHARCOUNT");
        assert_eq!(e.status_message(), Some("Document: 7 characters"));
    }

    #[test]
    fn sort_reorders_selected_lines() {
        let mut e = ed();
        e.run_command("insert \"charlie\\nbravo\\nalpha\\n\"");
        // Select the whole document, then sort.
        e.run_command("TOP");
        e.run_command("DF");
        e.run_command("BOTTOM");
        e.run_command("SORT");
        assert_eq!(e.document().text(), "alpha\nbravo\ncharlie\n");
        assert_eq!(e.status_message(), Some("SORT: sorted 3 lines"));
        // Caret jumps to the START of the sorted block (region stays selected).
        assert_eq!(e.document().cursor(), 0);
        assert_eq!(e.document().selection_span(), Some((0, 20)));
    }

    #[test]
    fn sort_reverse_and_dedup_switches() {
        let mut e = ed();
        e.run_command("insert \"apple\\nApple\\nbanana\\n\"");
        e.run_command("TOP");
        e.run_command("DF");
        e.run_command("BOTTOM");
        // /R reverse + /D dedup: "apple"/"Apple" share a key; descending order.
        e.run_command("SORT/R/D");
        assert_eq!(e.document().text(), "banana\napple\n");
        assert_eq!(
            e.status_message(),
            Some("SORT/R/D: 3 -> 2 lines (1 dup removed)")
        );
    }

    #[test]
    fn sort_without_selection_is_an_error() {
        let mut e = ed();
        e.run_command("insert \"b\\na\\n\"");
        e.run_command("SORT");
        assert_eq!(e.document().text(), "b\na\n"); // unchanged
        // Trappable: surfaces on the status line / $ERROR, not as a fatal error.
        assert_eq!(e.status_message(), Some("SORT: no text is selected"));
        assert!(e.last_error().is_none());
    }

    #[test]
    fn topdf_resolves_a_relative_path_against_cwd() {
        // Regression: TOPDF (through write_doc_bytes) must resolve a relative output path
        // against the editor's $CWD — not the process cwd — like every other file op.
        let base = std::env::temp_dir().join(format!("xyscript-topdf-cwd-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.insert_str("# Hello");
        e.run_command("TOPDF out.pdf");
        assert!(
            base.join("out.pdf").exists(),
            "TOPDF wrote a relative path to the process cwd instead of $CWD"
        );

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn find_verb_requests_the_dialog() {
        let mut e = ed();
        assert!(!e.take_find_request());
        e.run_command("FIND");
        assert!(e.take_find_request());
        assert!(!e.take_find_request()); // consumed
    }

    #[test]
    fn successful_search_requests_reveal_center() {
        let mut e = ed();
        e.insert_str("alpha\nbeta\ngamma\ndelta\nfound me here\n");
        e.run_command("TF"); // top of file — not a search
        assert!(!e.take_reveal_center(), "no request from a plain caret move");
        // A found match asks the GUI to scroll it to mid-screen.
        e.run_command(r#"SE "found""#);
        assert!(e.take_reveal_center(), "a successful search requests centering");
        assert!(!e.take_reveal_center(), "one-shot: consumed");
        // A miss does NOT request centering.
        e.run_command(r#"SE "nonexistent""#);
        assert!(!e.take_reveal_center());
    }

    #[test]
    #[cfg(feature = "lint")]
    fn live_lint_toggles_rechecks_and_clears() {
        // A window wide enough to cover the whole tiny doc.
        let (lo, hi) = (0usize, 10_000usize);
        let mut e = ed();
        e.insert_str("The qick fox");
        // Off by default: no marks, never stale, no re-check.
        assert!(e.lints().is_empty());
        assert!(!e.lint_stale(lo, hi));
        e.recheck_lints(lo, hi);
        assert!(e.lints().is_empty());
        // Turn spelling on → `$SPELLING` reads 1, and a first check is due (stale).
        e.run_command("SPELLING on");
        e.run_command("PR $SPELLING");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
        assert!(e.lint_stale(lo, hi), "just turned on → a check is due");
        e.recheck_lints(lo, hi);
        assert!(!e.lints().is_empty(), "re-check should have found 'qick'");
        assert!(!e.lint_stale(lo, hi), "checked → no longer stale");
        // Edit again → stale until the next re-check.
        e.insert_str(" jamps");
        assert!(e.lint_stale(lo, hi));
        e.recheck_lints(lo, hi);
        assert!(!e.lint_stale(lo, hi));
        // Turn off (nothing else on) → marks cleared, `$SPELLING` reads 0.
        e.run_command("SP off");
        assert!(e.lints().is_empty());
        e.run_command("PR $SPELLING");
        assert_eq!(e.output().last().map(String::as_str), Some("0"));
        // `$GRAMMAR` is independent and still off.
        e.run_command("PR $GRAMMAR");
        assert_eq!(e.output().last().map(String::as_str), Some("0"));
    }

    #[test]
    #[cfg(feature = "lint")]
    fn learn_and_unlearn_toggle_flagging() {
        let mut e = ed();
        e.host_mut().virtual_fs = true; // persist to the virtual fs, not the real disk
        e.insert_str("The florbnak sat down.");
        e.run_command("SPELLING on");
        let flagged = |e: &mut Editor| -> bool {
            e.recheck_lints(0, 10_000);
            e.lints().iter().any(|m| e.document().slice(m.start, m.end) == "florbnak")
        };
        assert!(flagged(&mut e), "the non-word should be flagged before LEARN");
        e.run_command("LEARN florbnak");
        assert!(!flagged(&mut e), "after LEARN the word is in the dictionary → not flagged");
        e.run_command("UNLEARN florbnak");
        assert!(flagged(&mut e), "after UNLEARN it is flagged again");
    }

    #[test]
    #[cfg(feature = "lint")]
    fn learn_accepts_a_list_a_selection_and_a_file() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.insert_str("florbnak glorbnak zorblat snorbfle");
        e.run_command("SPELLING on");
        let flagged = |e: &mut Editor, w: &str| -> bool {
            e.recheck_lints(0, 10_000);
            e.lints().iter().any(|m| e.document().slice(m.start, m.end) == w)
        };
        for w in ["florbnak", "glorbnak", "zorblat", "snorbfle"] {
            assert!(flagged(&mut e, w), "{w} should be flagged before it is learned");
        }
        // (1) A whitespace-separated LIST on the command line — all learned at once.
        e.run_command("LEARN florbnak glorbnak");
        assert!(!flagged(&mut e, "florbnak") && !flagged(&mut e, "glorbnak"));
        assert!(flagged(&mut e, "zorblat"), "a word not in the list stays flagged");
        // (2) A FILE listing words (one per line) via /F.
        e.host_mut().write_file("dict.txt", "zorblat\n").unwrap();
        e.run_command("LEARN/F dict.txt");
        assert!(!flagged(&mut e, "zorblat"), "the word listed in the file is learned");
        assert!(flagged(&mut e, "snorbfle"), "still flagged until the selection is learned");
        // (3) The current SELECTION via /S — DFA selects the whole doc, then LEARN/S sweeps it.
        e.run_command("DFA");
        e.run_command("LEARN/S");
        assert!(!flagged(&mut e, "snorbfle"), "the last word is learned from the selection");
    }

    #[test]
    #[cfg(feature = "lint")]
    fn learn_from_file_reports_missing_file() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.run_command("LEARN/F nope.txt");
        // A missing file is a trappable error, surfaced via $ERROR (not a panic).
        e.run_command("PR $ERROR");
        assert!(e.output().last().is_some_and(|s| !s.is_empty()));
    }

    #[test]
    #[cfg(feature = "email")]
    fn sendmail_f_reads_the_body_from_a_file() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        // Configure enough to get past the SMTP-config checks to the body-read step.
        e.run_command("SMTPURL smtps://smtp.example.com:465");
        e.run_command("MAILUSER me@example.com");
        e.run_command("MAILPASS secret");
        // A missing /F file fails at the body-read step — proving SENDMAIL/F sources the message from
        // the file (not the active buffer), and it never reaches the actual send.
        e.run_command("SENDMAIL/F nope.txt alice@example.com \"Hi\"");
        e.run_command("PR $ERROR");
        assert_eq!(e.output().last().map(String::as_str), Some("1"), "missing /F file sets $ERROR");
    }

    #[test]
    #[cfg(feature = "email")]
    fn attach_stages_lists_and_clears() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.host_mut().write_file("a.pdf", "PDF").unwrap();
        e.host_mut().write_file("b.png", "PNG").unwrap();
        assert_eq!(e.special_str("ATTACH"), ""); // nothing staged
        e.run_command("ATTACH a.pdf");
        e.run_command("ATTACH b.png");
        assert_eq!(e.special_str("ATTACH"), "a.pdf, b.png");
        // A missing file errors (sets $ERROR) and leaves the list untouched.
        e.run_command("ATTACH nope.pdf");
        e.run_command("PR $ERROR");
        assert_eq!(e.output().last().map(String::as_str), Some("1"), "missing file sets $ERROR");
        assert_eq!(e.special_str("ATTACH"), "a.pdf, b.png");
        e.run_command("CLEARATTACH");
        assert_eq!(e.special_str("ATTACH"), "");
    }

    #[test]
    #[cfg(feature = "lint")]
    fn dialect_sets_reads_and_persists() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.run_command("PR $DIALECT");
        assert_eq!(e.output().last().map(String::as_str), Some("american")); // default
        e.run_command("DIALECT british");
        e.run_command("PR $DIALECT");
        assert_eq!(e.output().last().map(String::as_str), Some("british"));
        e.run_command("DIALECT US"); // alias
        e.run_command("PR $DIALECT");
        assert_eq!(e.output().last().map(String::as_str), Some("american"));
        e.run_command("DIALECT klingon"); // unknown → trappable, unchanged
        e.run_command("PR $DIALECT");
        assert_eq!(e.output().last().map(String::as_str), Some("american"));
        e.run_command("DIALECT canadian");
        e.run_command("SAVECFG cfg.cfg");
        let cfg = e.host_mut().files.get("cfg.cfg").unwrap().clone();
        assert!(cfg.contains("DIALECT canadian"), "SaveCfg persists the dialect");
    }

    #[test]
    #[cfg(feature = "lint")]
    fn dialect_changes_what_is_flagged() {
        // British dialect should flag American spellings that American accepts.
        let text = |e: &mut Editor| -> Vec<String> {
            e.recheck_lints(0, 10_000);
            e.lints().iter().map(|m| e.document().slice(m.start, m.end)).collect()
        };
        let mut e = ed();
        e.insert_str("The colour of the theatre."); // British spellings
        e.run_command("SPELLING on");
        let american = text(&mut e); // American dialect: 'colour'/'theatre' may be flagged
        e.run_command("DIALECT british");
        let british = text(&mut e); // British dialect: these are correct → fewer/no flags
        assert_ne!(american, british, "switching dialect should change what's flagged: us={american:?} uk={british:?}");
    }

    #[test]
    #[cfg(feature = "lint")]
    fn lint_context_at_finds_spelling_choices_for_the_right_click() {
        let mut e = ed();
        e.insert_str("The qick fox. I has a apple."); // 'qick' spelling; 'has'/'a' grammar
        // A click inside "qick" (rope 4..8) → its spelling correction choices.
        let cx = e.lint_context_at(5).expect("a flagged spelling word should yield choices");
        assert_eq!(e.document().slice(cx.span.0, cx.span.1), "qick");
        assert!(!cx.labels.is_empty(), "should offer suggestions");
        // A click on a correctly-spelled word → nothing.
        assert!(e.lint_context_at(1).is_none(), "'The' is not flagged");
        // A click on a GRAMMAR issue ("has") → nothing (lint_context_at is spelling-only; the
        // right-click popup uses `lint_explain_at`, which DOES cover grammar — see below).
        let has_at = e.document().text().find("has").unwrap() + 1;
        assert!(e.lint_context_at(has_at).is_none(), "lint_context_at is spelling-only");
    }

    #[test]
    #[cfg(feature = "lint")]
    fn lint_explain_at_covers_spelling_and_grammar() {
        let mut e = ed();
        e.insert_str("The qick fox. I has a apple."); // 'qick' spelling; 'has'/'a' grammar
        e.run_command("SPELLING on");
        e.run_command("GRAMMAR on");
        e.recheck_lints(0, 10_000);
        // A SPELLING underline → flagged as spelling, with correction suggestions.
        let (ss, se) = {
            let m = e.lints().iter().find(|m| m.is_spelling()).expect("a spelling mark exists");
            (m.start, m.end)
        };
        let sp = e.lint_explain_at((ss + se) / 2).expect("the spelling word is underlined");
        assert!(sp.is_spelling && !sp.labels.is_empty(), "spelling → suggestions");
        // A GRAMMAR underline → an explanation, NOT flagged as spelling (so no "Add to dictionary").
        let (gs, ge) = {
            let m = e.lints().iter().find(|m| !m.is_spelling()).expect("a grammar mark exists");
            (m.start, m.end)
        };
        let gr = e.lint_explain_at((gs + ge) / 2).expect("the grammar span is underlined");
        assert!(!gr.is_spelling, "grammar is not a spelling issue");
        assert!(!gr.message.is_empty(), "grammar issue carries an explanation");
        // A non-underlined word → nothing.
        assert!(e.lint_explain_at(1).is_none(), "'The' is not underlined");
    }

    #[test]
    #[cfg(feature = "lint")]
    fn lintfix_corrects_the_word_at_the_caret() {
        let mut e = ed();
        e.insert_str("The qick brown fox.");
        e.run_command("SPS"); // persistent-selection mode ON — the case where insert used to APPEND
        e.run_command("TF");
        e.run_command("SPELLNEXT"); // caret selects the misspelling "qick"
        e.run_command("PR $SE");
        assert_eq!(e.output().last().map(String::as_str), Some("qick"));
        e.run_command("LINTFIX");
        // "qick" has several suggestions → a chooser opens; pick the first.
        if e.pick_request().is_some() {
            e.pick_respond(Some(0));
        }
        let text = e.document().text();
        assert!(!text.contains("qick"), "the misspelling should be replaced: {text:?}");
        // Surrounding text is preserved (only the issue span changed).
        assert!(
            text.starts_with("The ") && text.ends_with(" brown fox."),
            "context preserved: {text:?}"
        );
    }

    #[test]
    #[cfg(feature = "lint")]
    fn spell_and_grammar_navigation_select_the_issue() {
        let mut e = ed();
        // One clear misspelling ("qick"); "I has a apple" carries grammar issues (agreement / article).
        e.insert_str("Good start. I has a apple. A qick brown fox jumps.");
        // Caret is at end after insert → SPELLPREV finds the misspelling backward and selects it.
        e.run_command("SPELLPREV");
        e.run_command("PR $SE");
        assert_eq!(e.output().last().map(String::as_str), Some("qick"), "SPELLPREV selects the misspelling");
        // From the top, SPELLNEXT selects it too (skipping the grammar issues).
        e.run_command("TF");
        e.run_command("SPELLNEXT");
        e.run_command("PR $SE");
        assert_eq!(e.output().last().map(String::as_str), Some("qick"), "SPELLNEXT selects the misspelling");
        // GRAMMARNEXT from the top selects the FIRST grammar issue ("has", before "qick").
        e.run_command("TF");
        e.run_command("GRAMMARNEXT");
        e.run_command("PR $SE");
        let g = e.output().last().cloned().unwrap_or_default();
        assert!(!g.is_empty() && g != "qick", "GRAMMARNEXT selects a grammar issue, got {g:?}");
        // ...and not a spelling one — it should be a real word flagged for grammar (here "has").
        assert_eq!(g, "has", "GRAMMARNEXT lands on the first grammar issue");
    }

    #[test]
    #[cfg(feature = "lint")]
    fn spelling_and_grammar_are_independent() {
        let (lo, hi) = (0usize, 10_000usize);
        let mut e = ed();
        e.insert_str("I has a apple");
        // Grammar alone → active; a check runs and finds issues.
        e.run_command("GR on");
        e.run_command("PR $GRAMMAR");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
        e.run_command("PR $SPELLING");
        assert_eq!(e.output().last().map(String::as_str), Some("0"));
        e.recheck_lints(lo, hi);
        assert!(!e.lints().is_empty(), "grammar-only check should find issues");
        // Adding spelling keeps the marks (still active; the worker already produced all kinds).
        e.run_command("SPELLING on");
        assert!(!e.lints().is_empty(), "turning on the second flag must not clear marks");
        // One off while the other stays on → still active, marks retained.
        e.run_command("GR off");
        assert!(!e.lints().is_empty(), "spelling still on → marks retained");
        // Both off → cleared.
        e.run_command("SP off");
        assert!(e.lints().is_empty());
    }

    #[test]
    #[cfg(feature = "lint")]
    fn background_worker_delivers_lints() {
        // The async path: harper runs on the worker thread; the main thread requests then polls.
        let mut e = ed();
        e.insert_str("The qick brown fox");
        e.run_command("SPELLING on");
        e.request_lint(0, 0, 10_000);
        assert!(e.lint_in_flight(), "a request should be outstanding");
        // Wait for the real worker thread to deliver (poll up to ~5s).
        let mut applied = false;
        for _ in 0..500 {
            if e.poll_lint() {
                applied = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(applied, "background worker never delivered a result");
        assert!(!e.lints().is_empty(), "the worker result should include the 'qick' misspelling");
        assert!(!e.lint_in_flight(), "pending cleared once applied");
        // An unchanged view isn't re-requested.
        e.request_lint(0, 0, 10_000);
        assert!(!e.lint_in_flight(), "same view → deduped, no new request");
    }

    #[test]
    #[cfg(feature = "lint")]
    fn lint_marks_are_per_document() {
        // Each document keeps its OWN underlines (the split-pane fix): checking doc 1 must not
        // overwrite doc 0's marks, and switching back to doc 0 shows doc 0's marks — the marks a pane
        // draws come from its own document.
        let wait = |e: &mut Editor| {
            for _ in 0..500 {
                if e.poll_lint() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("worker never delivered");
        };
        let mut e = ed();
        e.insert_str("teh cat"); // doc 0: a misspelling
        e.run_command("SPELLING on");
        e.run_command("NEW"); // doc 1 (now active)
        e.insert_str("wrlod peace"); // doc 1: a different misspelling
        e.request_lint(1, 0, 10_000); // check doc 1 (the active one)
        wait(&mut e);
        e.request_lint(0, 0, 10_000); // check doc 0 (INACTIVE — must still land on doc 0)
        wait(&mut e);
        // Active is doc 1 → its own marks.
        assert!(!e.lints().is_empty(), "doc 1 keeps its own marks");
        // Switch to doc 0 → ITS marks survived doc 1's check.
        e.run_command("BUFPREV");
        assert!(!e.lints().is_empty(), "doc 0's marks are independent and survive");
    }

    #[test]
    #[cfg(feature = "lint")]
    fn lint_marks_never_cover_whitespace() {
        // Underlines must sit on the WORD, never on surrounding spaces/tabs/newlines (harper can
        // report a whitespace-padded span; `lint_region` trims it).
        let mut e = ed();
        e.insert_str("This sentance   is    wrrong.  \n"); // misspellings amid runs of spaces
        e.run_command("SPELLING on");
        e.recheck_lints(0, 10_000);
        assert!(!e.lints().is_empty(), "should flag the misspellings");
        let marks: Vec<(usize, usize)> = e.lints().iter().map(|m| (m.start, m.end)).collect();
        let buf = e.document();
        for (s, en) in marks {
            let text = buf.slice(s, en);
            assert!(
                !text.starts_with(char::is_whitespace) && !text.ends_with(char::is_whitespace),
                "a lint mark covers whitespace: {text:?}"
            );
        }
    }

    #[test]
    #[cfg(feature = "lint")]
    fn live_lint_scopes_to_the_visible_window() {
        // The freeze fix: live checking lints only the visible byte window (+margin), not the whole
        // document. A typo far outside the window must NOT be reported, but the mark that IS found
        // must still carry an absolute rope offset (it maps back through the whole doc).
        let mut e = ed();
        // "qick" near the start; a big filler; "jamps" far away (well past the window+margin).
        let filler = "word ".repeat(4000); // ~20 KB, past the 4 KB margin
        e.insert_str(&format!("The qick fox. {filler}and it jamps."));
        e.run_command("SPELLING on");
        // Window over just the start of the document.
        e.recheck_lints(0, 40);
        let joined: String = e
            .lints()
            .iter()
            .map(|m| m.kind.clone())
            .collect::<Vec<_>>()
            .join(",");
        // Found the near typo; the far one is outside the window, so no marks reach past it.
        assert!(!e.lints().is_empty(), "the near typo should be found: {joined}");
        let doc_len = e.doc(e.active_index()).len();
        assert!(
            e.lints().iter().all(|m| m.end <= doc_len && m.start < m.end),
            "marks carry valid absolute rope offsets"
        );
        // Every mark lands in the early part of the doc (the window+margin), not near the far typo.
        assert!(
            e.lints().iter().all(|m| m.start < 5000),
            "no marks from outside the visible window"
        );
    }

    #[test]
    #[cfg(feature = "lint")]
    fn check_reports_a_misspelling_at_the_right_location() {
        // Phase 0 language check. The multibyte 'é' (2 UTF-8 bytes) BEFORE the typo is the point:
        // harper reports a CHAR index, and the host must map char→byte→rope correctly, or the
        // quoted snippet / column would be off by the multibyte delta.
        let mut e = ed();
        e.insert_str("café qick brown fox"); // 'qick' is a misspelling; starts at char 5
        e.run_command("CHECK");
        // CHECK runs on a background thread and opens a scrollable `*Language Check*` tab when done.
        while e.check_in_progress() {
            if !e.poll_check() {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        let out = e.document().text(); // the results tab is now active
        assert!(
            out.contains("\"qick\""),
            "the misspelled word should be located and quoted:\n{out}"
        );
        // 'qick' is char index 5 (byte 6, after the 2-byte é) on line 1 → column 6.
        assert!(out.contains("1:6"), "expected the typo at line:col 1:6:\n{out}");
    }

    #[test]
    fn find_all_returns_every_match_span_and_honors_case() {
        // Backs the FIND dialog's live "highlight all". Read-only, whole-document, in order.
        let mut e = ed();
        e.insert_str("aXaXa"); // 'a' at 0,2,4 ; 'X' at 1,3
        // Case-insensitive (default): every 'a'.
        assert_eq!(e.find_all("a", false, false, false, false), vec![(0, 1), (2, 3), (4, 5)]);
        // 'x' case-insensitive matches the two uppercase X.
        assert_eq!(e.find_all("x", false, false, false, false), vec![(1, 2), (3, 4)]);
        // 'x' case-SENSITIVE matches nothing (they are uppercase).
        assert!(e.find_all("x", true, false, false, false).is_empty());
        // Empty pattern → no hits (dialog shows nothing until you type).
        assert!(e.find_all("", false, false, false, false).is_empty());
        // Overlapping-safe advance: a 2-char pattern steps past each match.
        e.run_command("NEW");
        e.insert_str("ababab");
        assert_eq!(e.find_all("ab", false, false, false, false), vec![(0, 2), (2, 4), (4, 6)]);
    }

    #[test]
    fn search_spec_takes_the_pattern_literally() {
        // The Find dialog builds a SearchSpec directly, so `$x` is the literal text — NOT the
        // variable `x` (a typed `SE $x` would substitute). This is why the dialog uses search_spec.
        let mut e = ed();
        e.run_command("SET x = 99");
        e.insert_str("price is $x dollars");
        e.run_command("TF");
        assert!(e.search_spec("$x", None, false, false, false, false, false, false).is_ok());
        assert_eq!(e.selection_text(), "$x");
    }

    #[test]
    fn search_spec_replace_all_replaces_every_match() {
        let mut e = ed();
        e.insert_str("a-a-a");
        e.run_command("TF");
        assert!(e
            .search_spec("a", Some("X".to_string()), false, false, false, false, false, false)
            .is_ok());
        assert_eq!(e.document().text(), "X-X-X");
    }

    #[test]
    fn search_spec_keep_case_recases_replacements() {
        // The path the FIND dialog's "Replace all" + "Keep case" checkbox drives.
        let mut e = ed();
        e.insert_str("cat CAT Cat cAt");
        e.run_command("TF");
        assert!(e
            .search_spec("cat", Some("dog".to_string()), false, false, false, false, false, true)
            .is_ok());
        assert_eq!(e.document().text(), "dog DOG Dog dog"); // mixed-case cAt → verbatim
    }

    #[test]
    fn recase_to_match_follows_the_matched_case() {
        // The helper the FIND dialog's single "Replace" button uses to recase one overwrite.
        use crate::host::recase_to_match;
        assert_eq!(recase_to_match("cat", "dog"), "dog");
        assert_eq!(recase_to_match("CAT", "dog"), "DOG");
        assert_eq!(recase_to_match("Cat", "dog"), "Dog");
        assert_eq!(recase_to_match("cAt", "dog"), "dog"); // mixed → verbatim
    }

    #[test]
    fn bare_se_repeats_with_the_last_search_options() {
        // A dialog search with options stores the full spec; a bare `SE` (Find Next / F3) repeats
        // WITH them. Here a case-sensitive find of "ABC" must, on repeat, skip the lowercase "abc"
        // in between and land on the next uppercase "ABC".
        let mut e = ed();
        e.insert_str("abc ABC abc ABC");
        e.run_command("TF");
        assert!(e.search_spec("ABC", None, false, true, false, false, false, false).is_ok());
        assert_eq!(e.selection_text(), "ABC");
        e.run_command("SE"); // bare repeat
        // If options were dropped, this case-insensitive repeat would match the lowercase "abc".
        assert_eq!(e.selection_text(), "ABC");
    }

    #[test]
    fn ar_hash_rhs_runs_xpl_and_aborts_the_chain() {
        let base = std::env::temp_dir().join(format!("xyscript-ar-xpl-{:p}", &"t"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let arfile = base.join("ar.txt").display().to_string().replace('\\', "/");
        // `xx` = an XPL action (`#…`); `teh` = a plain text replacement.
        std::fs::write(&arfile, b"xx=#WRITE DONE\nteh=the\n").unwrap();

        let mut e = ed();
        e.run_command(&format!("LOADAR {arfile}"));

        // `#`-action: AR deletes the key, runs the XPL, and HALTS the chain — `WRITE Z` is skipped.
        e.insert_str("xx");
        e.run_command("AR;WRITE Z");
        assert_eq!(e.document().text(), "DONE");

        // Plain replacement: AR swaps the word and the chain CONTINUES — `WRITE !` appends.
        e.run_command("NEW");
        e.insert_str("teh");
        e.run_command("AR;WRITE !");
        assert_eq!(e.document().text(), "the!");

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn bc_with_text_sets_literal_cmline_and_xc_expands() {
        let mut e = ed();
        e.run_command("SET name = \"Ada\"");
        // `BC <text>` puts the LITERAL text on the cmline (no `$`-expansion) and focuses it.
        e.run_command("BC PR hello $name");
        assert_eq!(e.command_line().text(), "PR hello $name");
        assert!(e.cmline_open());
        // `XC` runs the cmline now → `$name` expands at run time.
        e.run_command("XC");
        assert_eq!(e.output().last().map(String::as_str), Some("hello Ada"));
        // Bare `BC` still blanks the cmline.
        e.run_command("BC");
        assert_eq!(e.command_line().text(), "");
    }

    #[test]
    fn xc_that_reruns_itself_is_capped_not_a_crash() {
        // The reported mishap: a bare `XC` on the command line. `XC` snapshots the cmline text ("XC")
        // and re-execs it, which snapshots "XC" and re-execs it … Before the fix this recursed on the
        // native stack until it overflowed and crashed. Now it's capped and aborts trappably.
        let mut e = ed();
        e.run_command("BC XC"); // literal "XC" on the cmline
        assert_eq!(e.command_line().text(), "XC");
        e.run_command("XC"); // must NOT overflow the stack / crash
        assert!(
            e.last_error().is_some_and(|m| m.contains("runs itself")),
            "expected the re-exec cap to abort with an error, got {:?}",
            e.last_error()
        );
        // The offending line is blanked so a follow-up Enter can't immediately re-trigger it.
        assert_eq!(e.command_line().text(), "");
        // The editor is still usable afterwards.
        e.run_command("NEW");
        e.insert_str("ok");
        assert_eq!(e.document().text(), "ok");
    }

    #[test]
    fn caret_motion_visual_defers_left_right_to_a_visual_move() {
        let mut e = ed();
        e.insert_str("abc");
        assert!(!e.caret_visual(), "CARETMOTION defaults to logical");
        assert_eq!(e.special_str("CARETMOTION"), "logical");
        // Logical: Left moves logically and queues NO visual refine.
        e.run_command("LEFT");
        assert!(e.take_visual_move().is_none(), "no visual refine while CARETMOTION is logical");
        // Switch to visual.
        e.run_command("CARETMOTION visual");
        assert!(e.caret_visual());
        assert_eq!(e.special_str("CARETMOTION"), "visual");
        // Visual: Left still moves logically (baseline) AND queues a visual refine (right=false).
        e.run_command("LEFT");
        assert!(
            matches!(e.take_visual_move(), Some((false, false, _))),
            "Left should queue a (right=false, extend=false) visual refine"
        );
        // Right queues (right=true); draining clears it.
        e.run_command("RIGHT");
        assert!(matches!(e.take_visual_move(), Some((true, false, _))));
        assert!(e.take_visual_move().is_none(), "the request is drained once taken");
        // A bad argument is a trappable error, and `CARETMOTION` is read-only via SET.
        e.run_command("CARETMOTION sideways");
        assert!(e.last_error().is_some(), "an unknown mode is rejected");
        // Back to logical.
        e.run_command("CARETMOTION logical");
        assert!(!e.caret_visual());
        e.run_command("LEFT");
        assert!(e.take_visual_move().is_none(), "logical again → no refine");
    }

    #[test]
    fn msg_from_cmline_returns_focus_to_the_cmline_after_dismiss() {
        // Reported bug: `MSG $TX` submitted from the command line. `$TX` correctly reads 0 (the
        // command line is the input focus), but after the modal was dismissed the caret showed in
        // the TEXT and nav keys moved it there — because `submit_cmdline` flips `target = Document`
        // transiently and, on a SUSPEND (bare MSG parks the script), skipped its focus reconcile,
        // leaving `target` in the text. The deferred reconcile in `settle` must fix that.
        let mut e = ed();
        e.run_command("BC MSG hi"); // literal "MSG hi" typed on the command line, focused there
        assert!(e.cmline_open(), "setup: focus is on the command line");
        e.submit_cmdline(); // Enter → runs "MSG hi"; the bare MSG parks on a modal box
        assert!(
            e.message_request().is_some(),
            "MSG should park the script on a modal message box"
        );
        e.dismiss_message(); // OK/Esc → resume → settle → deferred reconcile
        assert!(
            e.cmline_open(),
            "focus must return to the command line after the MSG is dismissed, \
             not be left in the text"
        );
    }

    #[test]
    fn path_predicate_functions_exist_isdir_isfile() {
        let base = std::env::temp_dir().join(format!("xyscript-paths-{:p}", &"t"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sub")).unwrap();
        std::fs::write(base.join("f.txt"), b"x").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("SET f = \"f.txt\"");
        e.run_command("SET d = \"sub\"");
        e.run_command("SET n = \"nope\"");
        // file: exists, not a dir, is a file
        e.run_command("PR $(EXISTS($f)) $(ISDIR($f)) $(ISFILE($f))");
        assert_eq!(e.output().last().map(String::as_str), Some("1 0 1"));
        // directory: exists, is a dir, not a file
        e.run_command("PR $(EXISTS($d)) $(ISDIR($d)) $(ISFILE($d))");
        assert_eq!(e.output().last().map(String::as_str), Some("1 1 0"));
        // missing: all 0
        e.run_command("PR $(EXISTS($n)) $(ISDIR($n)) $(ISFILE($n))");
        assert_eq!(e.output().last().map(String::as_str), Some("0 0 0"));

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn getpath_detects_an_existing_directory() {
        // GETPATH on a REAL directory must put the whole path in element 0 and leave the filename
        // parts empty — not treat the last component as a filename.
        let base = std::env::temp_dir().join(format!("xyscript-getpath-{:p}", &"t"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let dir = base.display().to_string().replace('\\', "/");

        let mut e = ed();
        e.run_command(&format!("SET p = GETPATH(\"{dir}\")"));
        e.run_command("PR $($p[0])|$($p[1])|$($p[2])|$($p[3])");
        assert_eq!(e.output().last().map(String::as_str), Some(format!("{dir}|||").as_str()));

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn cd_then_dir_lists_cwd_and_enter_opens_a_file() {
        // A real temp directory (DIR reads the disk, not the virtual FS).
        let base = std::env::temp_dir().join(format!("xyscript-ed-dir-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sub")).unwrap();
        std::fs::write(base.join("hello.txt"), b"hi there\n").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        assert!(e.status_message().unwrap().starts_with("cwd: "));

        // DIR/B lists the working dir (compact names-only) into its own scratch buffer.
        e.run_command("DIR/B");
        assert!(e.host_mut().active_is_dir_listing());
        assert_eq!(e.document().text(), "..\\\nsub\\\nhello.txt");
        // `..` isn't counted; only `sub` and `hello.txt` are the dir's two items.
        assert_eq!(e.status_message(), Some(format!("2 items in {}", base.display())).as_deref());

        // Express-key to hello.txt, then Enter opens it in place.
        e.host_mut().dir_find_push('h');
        e.host_mut().dir_find_push('e');
        e.host_mut().dir_enter().unwrap();
        assert!(!e.host_mut().active_is_dir_listing());
        assert_eq!(e.document().text(), "hi there\n");
        assert_eq!(e.host_mut().active_file_name().as_deref(), Some("hello.txt"));

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn dir_window_lists_navigates_and_opens_in_a_parent_tab() {
        let base = std::env::temp_dir().join(format!("xyscript-dirw-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sub")).unwrap();
        std::fs::write(base.join("a.txt"), b"alpha\n").unwrap();
        std::fs::write(base.join("b.txt"), b"bravo\n").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command(r#"INSERT "scratch""#); // non-empty parent buffer, so opening adds a real tab
        let tabs_before = e.buffer_count();

        // DIR/W opens the listing in the separate window — NOT a tab.
        e.run_command("DIR/W/B");
        assert!(e.host_mut().dir_window_active());
        assert!(e.host_mut().take_dir_window_open(), "GUI is signalled to open the window");
        assert!(!e.host_mut().take_dir_window_open(), "the open signal is one-shot");
        assert_eq!(e.buffer_count(), tabs_before, "DIR/W must not add a tab");

        let view = e.host_mut().dir_window_view().unwrap();
        assert_eq!(view.lines, vec!["..\\", "sub\\", "a.txt", "b.txt"]);
        assert_eq!(view.selected, 0);

        // Select b.txt (row 3, as a double-click would) and activate → opens it in a NEW parent tab.
        e.host_mut().dir_window_select_row(3);
        let action = e.host_mut().dir_window_activate().unwrap();
        assert!(matches!(action, crate::host::DirWindowAction::Opened(_)));
        assert!(e.host_mut().dir_window_active(), "the window stays open after opening a file");
        assert_eq!(e.buffer_count(), tabs_before + 1);
        assert_eq!(e.document().text(), "bravo\n");
        assert_eq!(e.host_mut().active_file_name().as_deref(), Some("b.txt"));

        e.host_mut().close_dir_window();
        assert!(!e.host_mut().dir_window_active());
        assert!(e.host_mut().dir_window_view().is_none());

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn dw_appearance_overrides_inherit_persist_and_reset() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.run_command("FGCOLOR 1 2 3"); // editor fg, which DW inherits by default

        // Unset → DW inherits the editor foreground.
        e.run_command("PR $DWFGCOLOR");
        assert_eq!(e.output().last().map(String::as_str), Some("1 2 3"));

        // Overrides take effect and read back.
        e.run_command("DWFGCOLOR 10 20 30");
        e.run_command("PR $DWFGCOLOR");
        assert_eq!(e.output().last().map(String::as_str), Some("10 20 30"));
        e.run_command("DWBGCOLOR 40 50 60");
        e.run_command("DWFAMILY Consolas");
        e.run_command("PR $DWFAMILY");
        assert_eq!(e.output().last().map(String::as_str), Some("Consolas"));
        e.run_command("DWSTYLE bold");
        e.run_command("PR $DWSTYLE");
        assert_eq!(e.output().last().map(String::as_str), Some("bold"));
        e.run_command("DWFONT 18");

        // SAVECFG captures only the overrides that were set.
        e.run_command("SAVECFG cfg.xpl");
        let cfg = e.host_mut().files.get("cfg.xpl").cloned().unwrap();
        assert!(cfg.contains("DWFGCOLOR 10 20 30"));
        assert!(cfg.contains("DWBGCOLOR 40 50 60"));
        assert!(cfg.contains("DWFAMILY Consolas"));
        assert!(cfg.contains("DWSTYLE bold"));
        assert!(cfg.contains("DWFONT 18"));

        // DWRESET reverts every override back to inheriting the editor.
        e.run_command("DWRESET");
        e.run_command("PR $DWFGCOLOR");
        assert_eq!(e.output().last().map(String::as_str), Some("1 2 3")); // inherits editor fg again
        e.run_command("PR $DWFAMILY");
        assert_eq!(e.output().last().map(String::as_str), Some("")); // inherits (unset) EDFAMILY
        e.run_command("PR $DWSTYLE");
        assert_eq!(e.output().last().map(String::as_str), Some("normal"));
    }

    #[test]
    fn dp_dr_read_the_dir_window_when_active_is_not_a_listing() {
        let base = std::env::temp_dir().join(format!("xyscript-dwdp-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("z.txt"), b"hi\n").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command(r#"INSERT "doc""#); // the active tab is a normal document, not a listing

        // No DIR/W yet, active isn't a listing → $DP is empty.
        e.run_command("PR $DP");
        assert_eq!(e.output().last().map(String::as_str), Some(""));

        // Open the DIR/W window; now $DP/$DR reflect ITS listing despite the active tab being a doc.
        e.run_command("DIR/W/B");
        e.run_command("PR $DP");
        assert!(e.output().last().unwrap().contains("xyscript-dwdp"));

        let view = e.host_mut().dir_window_view().unwrap();
        let row = view.lines.iter().position(|l| l.starts_with("z.txt")).unwrap();
        e.host_mut().dir_window_select_row(row);
        e.run_command("PR $DR");
        assert!(e.output().last().unwrap().contains("z.txt"));

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn dir_window_descends_into_a_subdirectory() {
        let base = std::env::temp_dir().join(format!("xyscript-dirw-desc-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sub")).unwrap();
        std::fs::write(base.join("sub").join("inner.txt"), b"x\n").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("DIR/W/B");
        let tabs_before = e.buffer_count();

        // Select `sub\` and activate → the window re-lists that directory (no new tab).
        let view = e.host_mut().dir_window_view().unwrap();
        let sub_row = view.lines.iter().position(|l| l.starts_with("sub")).unwrap();
        e.host_mut().dir_window_select_row(sub_row);
        let action = e.host_mut().dir_window_activate().unwrap();
        assert!(matches!(action, crate::host::DirWindowAction::Descended(_)));
        assert_eq!(e.buffer_count(), tabs_before, "descending opens no tab");

        let view = e.host_mut().dir_window_view().unwrap();
        assert!(view.base.ends_with("sub"));
        assert!(view.lines.iter().any(|l| l.starts_with("inner.txt")));

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn opening_a_file_never_modifies_it() {
        // Directive: opening adds nothing (no metadata header), for ANY extension. The
        // buffer must equal the file byte-for-byte, the document must be unmodified, and
        // the bytes on disk must be untouched. (A `.md` exception may come later.)
        let base = std::env::temp_dir().join(format!("xyscript-open-verbatim-{:p}", &"t"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let plain = base.join("plain.txt");
        let markdown = base.join("doc.md");
        std::fs::write(&plain, b"line one\nline two\n").unwrap();
        // A markdown file with NO front matter — opening must not synthesize one.
        std::fs::write(&markdown, b"# Heading\n\nbody text\n").unwrap();

        for (path, want) in [(&plain, "line one\nline two\n"), (&markdown, "# Heading\n\nbody text\n")] {
            let mut e = ed();
            e.open(&path.to_string_lossy()).unwrap();
            assert_eq!(e.document().text(), want, "buffer content changed on open");
            assert!(!e.modified(), "document marked modified by open alone");
            // Disk bytes untouched (open is read-only).
            assert_eq!(std::fs::read(path).unwrap(), want.as_bytes());
        }
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// `DIRWCLOSE` dismisses the `DIR/W` picker once a file is opened — independent of `DIRWNW`,
    /// which only chooses the TAB. Off by default (the window has always stayed open), and
    /// descending into a subdirectory never closes it.
    #[test]
    fn dirwclose_dismisses_the_picker_only_after_opening_a_file() {
        let base = std::env::temp_dir().join(format!("xyscript-dirwclose-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("a.txt"), b"alpha\n").unwrap();
        std::fs::create_dir_all(base.join("sub")).unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("PR $DIRWCLOSE");
        assert_eq!(e.output().last().map(String::as_str), Some("0"), "off by default");

        // Default (off): the window survives opening a file.
        e.run_command("DIR/W");
        e.host_mut().dir_window_find_push('a');
        e.host_mut().dir_window_activate().unwrap();
        assert!(e.host_mut().dir_window_active(), "off keeps the picker open");

        // On: opening a file dismisses it.
        e.run_command("DIRWCLOSE on");
        assert_eq!(e.status_message(), Some("DIRWCLOSE on"));
        e.run_command("DIR/W");
        e.host_mut().dir_window_find_push('a');
        e.host_mut().dir_window_activate().unwrap();
        assert!(!e.host_mut().dir_window_active(), "on closes it once a file opens");

        // ...but DESCENDING keeps it open, even with DIRWCLOSE on.
        e.run_command("DIR/W");
        e.host_mut().dir_window_find_push('s'); // the "sub" directory
        e.host_mut().dir_window_activate().unwrap();
        assert!(e.host_mut().dir_window_active(), "descending must not close the picker");

        // Independent of DIRWNW.
        e.run_command("PR $DIRWNW");
        assert_eq!(e.output().last().map(String::as_str), Some("1"), "DIRWNW untouched");

        e.run_command("DIRWCLOSE 2");
        assert!(e.last_error().unwrap().starts_with("DIRWCLOSE: expected on or off"));

        let _ = std::fs::remove_dir_all(&base);
    }

    /// `DIRWNW` governs the SEPARATE `DIR/W` window (vs `DIRNW`, which governs an in-tab listing).
    /// Default `on` preserves how `DIR/W` has always behaved; `off` reuses the active tab — but never
    /// discards a MODIFIED buffer to do it.
    #[test]
    fn dirwnw_controls_the_dir_window_open_and_never_discards_unsaved_work() {
        let base = std::env::temp_dir().join(format!("xyscript-dirwnw-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("a.txt"), b"alpha\n").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));

        // Default is ON — and it is INDEPENDENT of DIRNW (which defaults off).
        e.run_command("PR $DIRWNW");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
        e.run_command("PR $DIRNW");
        assert_eq!(e.output().last().map(String::as_str), Some("0"));

        // DIRWNW off → the picked file takes over the active (unmodified) tab.
        e.run_command("DIRWNW off");
        assert_eq!(e.status_message(), Some("DIRWNW off"));
        e.run_command("DIR/W");
        e.host_mut().dir_window_find_push('a');
        let before = e.host_mut().buffer_count();
        e.host_mut().dir_window_activate().unwrap();
        assert_eq!(e.host_mut().buffer_count(), before, "reused the active tab");
        assert_eq!(e.document().text(), "alpha\n");
        // The focused pane must follow the replacement (a fresh `DocId`).
        let active_id = e.host_mut().active_doc().id();
        assert_eq!(e.host_mut().panes().focused_doc(), active_id);

        // With UNSAVED changes in the active buffer, `off` must NOT clobber it.
        e.run_command(r#"INSERT "dirty""#);
        assert!(e.host_mut().active_doc().is_modified());
        let before = e.host_mut().buffer_count();
        e.host_mut().dir_window_find_clear();
        e.host_mut().dir_window_find_push('a');
        e.host_mut().dir_window_activate().unwrap();
        assert_eq!(e.host_mut().buffer_count(), before + 1, "opened beside the modified buffer");

        // DIRWNW on → always a new tab.
        e.run_command("DIRWNW on");
        let before = e.host_mut().buffer_count();
        e.host_mut().dir_window_find_clear();
        e.host_mut().dir_window_find_push('a');
        e.host_mut().dir_window_activate().unwrap();
        assert_eq!(e.host_mut().buffer_count(), before + 1);

        // Bad argument is trappable; state unchanged.
        e.run_command("DIRWNW 2");
        assert!(e.last_error().unwrap().starts_with("DIRWNW: expected on or off"));
        e.run_command("PR $DIRWNW");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));

        // Best-effort: the DIR/W window is still open on this directory, so Windows can refuse the
        // removal. It is a temp dir either way.
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn dirnw_controls_in_place_vs_new_tab_open() {
        let base = std::env::temp_dir().join(format!("xyscript-dirnw-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("a.txt"), b"alpha\n").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));

        // Default: $DIRNW = 0 → opening from the listing replaces it in place (1 buffer).
        e.run_command("PR $DIRNW");
        assert_eq!(e.output().last().map(String::as_str), Some("0"));
        e.run_command("DIR");
        assert_eq!(e.host_mut().buffer_count(), 1);
        e.host_mut().dir_find_push('a');
        e.host_mut().dir_enter().unwrap();
        assert!(!e.host_mut().active_is_dir_listing());
        assert_eq!(e.host_mut().buffer_count(), 1); // replaced in place
        assert_eq!(e.document().text(), "alpha\n");
        // ...and the focused PANE must follow the replacement. A `DocId` is unique per live
        // `Document`, so the new doc has an id the pane tree has never seen; without a re-sync the
        // pane still pointed at the listing's dead id and the file showed up as its own tab —
        // `DIRNW off` behaving exactly like `DIRNW on`, which is what a tester reported.
        let active_id = e.host_mut().active_doc().id();
        assert_eq!(
            e.host_mut().panes().focused_doc(),
            active_id,
            "the focused pane must show the file that replaced the listing"
        );

        // DIRNW on: $DIRNW = 1 → opening lands in a NEW tab, the listing stays behind.
        e.run_command("DIRNW on");
        assert_eq!(e.status_message(), Some("DIRNW on"));
        e.run_command("PR $DIRNW");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
        e.run_command("DIR");
        let before = e.host_mut().buffer_count();
        e.host_mut().dir_find_push('a');
        e.host_mut().dir_enter().unwrap();
        assert!(!e.host_mut().active_is_dir_listing());
        assert_eq!(e.host_mut().buffer_count(), before + 1); // opened alongside
        assert_eq!(e.document().text(), "alpha\n");

        // A bad argument is a trappable error; state is unchanged ($DIRNW stays 1).
        e.run_command("DIRNW 2");
        assert!(e.last_error().unwrap().starts_with("DIRNW: expected on or off"));
        e.run_command("PR $DIRNW");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn newline_converts_line_endings() {
        let mut e = ed();
        e.insert_str("a\nb\nc");
        e.run_command("NEWLINE crlf");
        assert_eq!(e.document().bytes(), b"a\r\nb\r\nc");
        e.run_command("PR $NEWLINE");
        assert_eq!(e.output().last().map(String::as_str), Some("CRLF"));
        e.run_command("NEWLINE lf");
        assert_eq!(e.document().bytes(), b"a\nb\nc");
    }

    #[test]
    fn encoding_convert_changes_bytes_in_place_and_to_a_file() {
        let base = std::env::temp_dir().join(format!("xyscript-enc-{:p}", &"t"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("ENCODING utf8");
        e.insert_str("é"); // U+00E9 — 2 bytes in UTF-8, 1 byte (0xE9) in Latin-1

        // Convert to a FILE: a converted copy is written + opened; the original tab is untouched.
        let n0 = e.host_mut().buffer_count();
        e.run_command("ENCODING/C latin1 out.txt");
        assert_eq!(e.host_mut().buffer_count(), n0 + 1);
        assert_eq!(std::fs::read(base.join("out.txt")).unwrap(), vec![0xE9]);
        assert_eq!(e.document().text(), "é"); // opened, viewed as Latin-1

        // Convert IN PLACE: re-encode the bytes (UTF-8 → Latin-1) of a fresh buffer.
        e.run_command("NEW");
        e.run_command("ENCODING utf8");
        e.insert_str("é");
        e.run_command("ENCODING/C latin1");
        assert_eq!(e.document().bytes(), vec![0xE9]);
        assert_eq!(e.document().text(), "é");

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn help_accessors_produce_content() {
        let mut e = ed();
        // About: the version line is present.
        assert!(e.about_info().iter().any(|(k, _)| *k == "Version"));
        // Keyboard chart: header + bound keys from the built-in baseline keymap.
        e.run_command("CLEARKBD");
        let chart = e.keyboard_chart();
        assert!(chart.starts_with("Keyboard chart"));
        assert!(chart.lines().count() > 3, "expected bound keys in the chart");
    }

    #[test]
    fn keyboard_tables_group_bindings_by_modifier_layer() {
        let mut e = ed();
        e.run_command("CLEARKBD"); // built-in baseline: a Normal layer + a Ctrl+S(31)=SAVE cell
        let tables = e.keyboard_tables();
        // The unshifted layer is named "Normal" and has bindings.
        let normal = tables.iter().find(|(n, _)| n == "Normal").expect("a Normal table");
        assert!(!normal.1.is_empty(), "the Normal layer should have bindings");
        // A modifier layer is named by its modifiers and resolves via the $SCC path
        // (`key_binding(code + mask)`): Ctrl+S is scan 31 on the CTRL (bit 256) table → SAVE.
        let ctrl = tables.iter().find(|(n, _)| n == "CTRL").expect("a CTRL table");
        assert!(
            ctrl.1.iter().any(|(code, b)| *code == 31 && b == "SAVE"),
            "expected Ctrl+S=SAVE in the CTRL table; got {:?}",
            ctrl.1
        );
        // The generation counter bumps on each install (the GUI caches the chart on it).
        let g = e.keymap_generation();
        e.run_command("CLEARKBD");
        assert!(e.keymap_generation() > g, "keymap_generation must bump on install");
    }

    #[test]
    fn kbchart_text_has_a_section_per_table() {
        use xyscript_xpl::Host;
        let mut e = ed();
        e.run_command("CLEARKBD");
        let text = e.host().keyboard_chart_text(); // what the `KBCHART` verb shows in a TXT modal
        assert!(text.contains("Normal"), "expected a Normal section:\n{text}");
        assert!(text.contains("CTRL"), "expected a CTRL section:\n{text}");
        assert!(text.contains("SAVE"), "expected the Ctrl+S=SAVE binding:\n{text}");
    }

    #[test]
    fn wingeo_toggles_and_reads_back() {
        let mut e = ed();
        assert!(e.remember_window_geometry()); // default on
        e.run_command("PR $WINGEO");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
        e.run_command("WINGEO off");
        assert!(!e.remember_window_geometry());
        e.run_command("PR $WINGEO");
        assert_eq!(e.output().last().map(String::as_str), Some("0"));
        e.run_command("WINGEO"); // bare = toggle
        assert!(e.remember_window_geometry());
    }

    #[test]
    fn tabxcolor_sets_reads_and_resets() {
        let mut e = ed();
        assert_eq!(e.tab_x_color(), None); // default: follows UI fg
        e.run_command("PR $TABXCOLOR");
        assert_eq!(e.output().last().map(String::as_str), Some("")); // empty when unset
        e.run_command("TABXCOLOR 200 120 60");
        assert_eq!(e.tab_x_color(), Some((200, 120, 60)));
        e.run_command("PR $TABXCOLOR");
        assert_eq!(e.output().last().map(String::as_str), Some("200 120 60"));
        e.run_command("TABXCOLOR default"); // back to UI fg
        assert_eq!(e.tab_x_color(), None);
    }

    #[test]
    fn titlebar_toggles_and_reads_back() {
        let mut e = ed();
        assert!(e.show_titlebar()); // default: shown
        e.run_command("TITLEBAR off");
        assert!(!e.show_titlebar());
        e.run_command("PR $TITLEBAR");
        assert_eq!(e.output().last().map(String::as_str), Some("0"));
        e.run_command("TITLEBAR"); // bare = toggle
        assert!(e.show_titlebar());
    }

    #[test]
    fn writer_view_toggles_reads_back_and_exits() {
        let mut e = ed();
        assert!(!e.writer_view()); // default: off (transient, never persisted)
        e.run_command("WV on");
        assert!(e.writer_view());
        e.run_command("PR $WV");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
        e.run_command("WV"); // bare = toggle → off
        assert!(!e.writer_view());
        // `exit_writer_view` (the GUI's Esc path) turns it off and reports whether it WAS on.
        e.run_command("WV on");
        assert!(e.exit_writer_view(), "was on → returns true");
        assert!(!e.writer_view());
        assert!(!e.exit_writer_view(), "already off → returns false");
        // A bad argument is a trappable error, leaving the state unchanged.
        e.run_command("WV on");
        e.run_command("WV sideways");
        assert!(e.writer_view(), "a rejected WV arg changes nothing");
    }

    #[test]
    fn emph_sets_and_clears_the_active_tabs_phrase_and_finds_it_case_insensitively() {
        let mut e = ed();
        // Whole-word matches only: "concatenate", "category", "scat" must NOT count.
        e.insert_str("The cat sat. A CAT ran. cAt. concatenate category scat");
        assert!(e.active_emph().is_none(), "no emphasis by default");
        assert!(e.emph_highlights().is_empty());
        e.run_command("EMPH cat");
        assert_eq!(e.active_emph().as_deref(), Some("cat"));
        // Three WHOLE-WORD occurrences, case-insensitive ("cat", "CAT", "cAt"); the "cat" inside
        // "concatenate"/"category"/"scat" is a substring, so it is excluded.
        assert_eq!(e.emph_highlights().len(), 3, "case-insensitive whole-word matches only");
        // Bare EMPH clears it.
        e.run_command("EMPH");
        assert!(e.active_emph().is_none());
        assert!(e.emph_highlights().is_empty());
    }

    #[test]
    fn emph_regex_matches_a_pattern_and_a_bad_regex_is_reported() {
        let mut e = ed();
        e.insert_str("cat cot cut cart");
        // `EMPH/R c.t` matches cat/cot/cut (three) but not "cart" (c-a-r-t is 4 chars).
        e.run_command("EMPH/R c.t");
        assert_eq!(e.active_emph().as_deref(), Some("c.t"));
        assert!(e.active_emph_is_regex(), "the /R flag is recorded");
        assert_eq!(e.emph_highlights().len(), 3, "regex c.t matches cat, cot, cut");
        // Without /R the same text is a whole-word literal — "c.t" appears literally nowhere.
        e.run_command("EMPH c.t");
        assert!(!e.active_emph_is_regex());
        assert!(e.emph_highlights().is_empty(), "literal c.t is not present");
        // A malformed regex is reported (trappable) and leaves the current emphasis untouched.
        e.run_command("EMPH cat"); // a known-good literal emphasis
        e.run_command("EMPH/R c(t"); // unbalanced paren
        e.run_command("PR $ERROR");
        assert_eq!(
            e.output().last().map(String::as_str),
            Some("1"),
            "a bad EMPH/R regex sets the trappable-error flag"
        );
        assert_eq!(e.active_emph().as_deref(), Some("cat"), "bad regex leaves emphasis unchanged");
        assert!(!e.active_emph_is_regex());
    }

    #[test]
    fn emph_menu_staging_preserves_the_trailing_space_and_runs() {
        let mut e = ed();
        e.insert_str("cat cat cat");
        // The Search ▸ Emphasize ▸ "Word or phrase…" item stages "EMPH " (trailing space) on the
        // command line via `load_cmline`. The space MUST survive — otherwise a typed phrase becomes
        // "EMPHcat" (a bad verb) instead of the argument "cat". This is why the menu uses a direct
        // cmline stage, not a `BC EMPH ;CMLINE` chain (whose segment the splitter would trim).
        e.load_cmline("EMPH ");
        assert_eq!(e.special_str("CM"), "EMPH ", "trailing space preserved on the command line");
        // Prefilled with the current phrase and submitted (as if the user pressed Enter).
        e.load_cmline("EMPH cat");
        e.submit_cmdline();
        assert_eq!(e.emph_highlights().len(), 3, "the staged EMPH command ran");
    }

    #[test]
    fn emph_is_per_tab() {
        let mut e = ed();
        e.insert_str("alpha alpha");
        e.run_command("EMPH alpha"); // tab 1 gets an emphasis
        assert_eq!(e.active_emph().as_deref(), Some("alpha"));
        e.run_command("NEW"); // fresh tab — its own (absent) emphasis
        assert!(e.active_emph().is_none(), "a new tab starts with no emphasis");
        assert!(e.emph_highlights().is_empty());
    }

    #[test]
    fn run_grep_dispatches_a_search_like_the_dialog() {
        use std::fs;
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "xyscript-rungrep-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("a.txt"), "the cat sat\n").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", d.display()));
        // The GREP dialog dispatches this directly (literal pattern, no $-expansion). It opens a
        // results tab; ignore_case=true is the /I flag.
        let msg = e.run_grep("cat", None, false, false, false).unwrap();
        assert!(msg.contains("GREP"), "status: {msg}");
        assert!(e.active_is_grep_listing(), "a results tab is now active");
        assert_eq!(e.active_emph().as_deref(), Some("cat")); // matches highlighted

        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn grep_emphasizes_matches_on_the_results_tab() {
        use std::fs;
        use std::sync::atomic::{AtomicU32, Ordering};
        // A real temp dir (GREP walks the real filesystem, not the virtual FS).
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "xyscript-grepemph-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("a.txt"), "concatenate\n").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", d.display()));
        // Plain literal GREP → EMPH highlights via an ESCAPED regex, so substrings light up exactly
        // like GREP's substring matches (EMPH's own literal mode is whole-word only).
        e.run_command("GREP cat");
        assert_eq!(e.active_emph().as_deref(), Some("cat"));
        assert!(e.active_emph_is_regex(), "literal GREP → escaped-regex EMPH (substring highlight)");
        // Regex GREP → EMPH/R with the pattern.
        e.run_command("GREP/R c.t");
        assert_eq!(e.active_emph().as_deref(), Some("c.t"));
        assert!(e.active_emph_is_regex());
        // Whole-word GREP → EMPH's whole-word literal.
        e.run_command("GREP/W cat");
        assert_eq!(e.active_emph().as_deref(), Some("cat"));
        assert!(!e.active_emph_is_regex());

        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn emph_colors_default_to_selection_and_are_overridable() {
        let mut e = ed();
        e.run_command("SELFGCOLOR 10 20 30");
        e.run_command("SELBGCOLOR 40 50 60");
        // Unset EMPH colours follow the selection colours.
        e.run_command("PR $EMPHFGCOLOR");
        assert_eq!(e.output().last().map(String::as_str), Some("10 20 30"));
        e.run_command("PR $EMPHBGCOLOR");
        assert_eq!(e.output().last().map(String::as_str), Some("40 50 60"));
        assert_eq!(e.emph_fg_color(), (10, 20, 30));
        assert_eq!(e.emph_bg_color(), (40, 50, 60));
        // Overriding EMPHFGCOLOR wins; `default` restores the follow-selection behaviour.
        e.run_command("EMPHFGCOLOR 200 100 50");
        assert_eq!(e.emph_fg_color(), (200, 100, 50));
        assert_eq!(e.emph_fg_override(), Some((200, 100, 50)));
        e.run_command("PR $EMPHFGCOLOR");
        assert_eq!(e.output().last().map(String::as_str), Some("200 100 50"));
        e.run_command("EMPHFGCOLOR default");
        assert_eq!(e.emph_fg_override(), None);
        assert_eq!(e.emph_fg_color(), (10, 20, 30), "cleared → follows selection again");
    }

    #[test]
    fn trusted_reflects_the_trust_setting() {
        let mut e = ed();
        assert!(!e.trusted()); // default: shell-out off
        e.run_command("TRUST 1");
        assert!(e.trusted());
        e.run_command("TRUST 0");
        assert!(!e.trusted());
    }

    #[test]
    fn runhere_focuses_the_document_before_running() {
        let mut e = ed();
        e.insert_str("```\nWRITE HELLO\n```\n");
        e.run_command("JMP 4"); // caret inside the fence (the `WRITE HELLO` line)
        e.run_command("GH"); // fire RUNHERE from the command line
        assert!(e.cmline_open());
        let before = e.document().text().matches("HELLO").count();
        e.run_command("RUNHERE");
        // Focus moved to the text area, and the block ran against the document.
        assert!(!e.cmline_open());
        assert_eq!(e.document().text().matches("HELLO").count(), before + 1);
    }

    #[test]
    fn dir_focuses_the_listing_even_from_the_cmline() {
        let base = std::env::temp_dir().join(format!("xyscript-dirfocus-{:p}", &"t"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("a.txt"), b"a\n").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("GH"); // focus the command line
        assert!(e.cmline_open());
        e.run_command("DIR");
        assert!(e.host_mut().active_is_dir_listing());
        assert!(!e.cmline_open()); // the caret landed in the listing, not the cmline

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn cmline_search_hit_focuses_text_miss_stays_in_cmline() {
        let mut e = ed();
        e.run_command("INSERT \"hello world\"");
        e.run_command("TOP");
        // Hit: a cmline search that finds → caret lands in the document.
        e.open_cmdline();
        e.insert_str("SE \"world\"");
        e.submit_cmdline();
        assert!(!e.cmline_open(), "a hit focuses the document");
        // Miss: not found ($ERROR) → stay in the cmline to fix the term.
        e.open_cmdline();
        e.insert_str("SE \"nope\"");
        e.submit_cmdline();
        assert!(e.cmline_open(), "a miss keeps the cmline focused");
    }

    #[test]
    fn xc_lands_in_text_on_success_else_stays_put() {
        let mut e = ed();
        e.run_command("INSERT \"hello world\"");
        e.run_command("TOP");
        // XC from the cmline: a successful document-focusing command lands in the text.
        e.run_command("BC");
        e.insert_str("SE \"hello\"");
        e.run_command("XC");
        assert!(!e.cmline_open(), "XC of a successful search → text");
        // XC from the cmline: a failure stays put (the cmline).
        e.run_command("BC");
        e.insert_str("SE \"nope\"");
        e.run_command("XC");
        assert!(e.cmline_open(), "XC failure from the cmline → stays in the cmline");
        // XC from the DOCUMENT: a failure stays put (the text).
        e.run_command("BC");
        e.insert_str("SE \"nope\"");
        e.run_command("GT"); // focus the document (cmline contents kept)
        e.run_command("XC");
        assert!(!e.cmline_open(), "XC failure from the text → stays in the text");
    }

    #[test]
    fn cmline_resting_focus_follows_the_per_verb_rule() {
        let mut e = ed();
        e.run_command("INSERT \"hello world\"");
        e.run_command("TOP");
        // Submit a command the way pressing Enter in the cmline does (BC blanks + focuses it first).
        let submit = |e: &mut Editor, cmd: &str| {
            e.run_command("BC");
            e.insert_str(cmd);
            e.submit_cmdline();
        };
        // Document-focusing → land in the text.
        submit(&mut e, "SE \"hello\""); // search hit (caret starts at top)
        assert!(!e.cmline_open(), "SE hit → text");
        submit(&mut e, "BF"); // bottom of file
        assert!(!e.cmline_open(), "BF (doc motion) → text");
        submit(&mut e, "JMP 0"); // go-to-char is document-facing
        assert!(!e.cmline_open(), "JMP → text");
        // Buffer-agnostic / exempt → stay in the cmline.
        submit(&mut e, "EL"); // line start (cursor motion)
        assert!(e.cmline_open(), "EL (line motion) → cmline");
        submit(&mut e, "UNDO");
        assert!(e.cmline_open(), "UNDO → cmline");
        // Neutral (doesn't touch the text) → stay in the cmline.
        submit(&mut e, "FGCOLOR 1 2 3");
        assert!(e.cmline_open(), "neutral setting → cmline");
    }

    #[test]
    fn open_family_from_the_cmline_lands_in_the_text() {
        // OPEN/CA/ED/EDIT/READ self-focus the document (open_doc → set_focus(Document)), so they were
        // already covered without FOCUS_DOC_VERBS — confirm each closes the cmline and opens the file.
        for verb in ["OPEN", "CA", "ED", "EDIT", "READ"] {
            let mut e = ed();
            e.host_mut().virtual_fs = true;
            e.host_mut().files.insert("f.txt".into(), "body".into());
            e.open_cmdline();
            assert!(e.cmline_open());
            e.run_command(&format!("{verb} f.txt"));
            assert!(!e.cmline_open(), "{verb} from the cmline → text");
            assert_eq!(e.document().text(), "body", "{verb} opened the file");
        }
    }

    #[test]
    fn a_script_or_routine_launched_from_the_cmline_lands_in_the_text() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        // A document-focusing command (SE) that is NOT a "big motion" — so it relies on the
        // script-launch focusing the document, not on being in FOCUS_DOC_VERBS itself.
        e.host_mut().files.insert("s.xpl".into(), "SE \"world\"".into());
        e.host_mut().files.insert("lib.lib".into(), "SUB godoc\nSE \"hello\"\nENDSUB".into());
        e.run_command("INSERT \"hello world\"");

        // RUN of a doc-focusing script, LAUNCHED while the CMline is focused (the non-submit path,
        // like a bound key or the Run dialog), acts on the document and leaves the caret in the text.
        e.run_command("TOP");
        e.open_cmdline();
        assert!(e.cmline_open(), "cmline focused at launch");
        e.run_command("RUN s.xpl");
        assert!(!e.cmline_open(), "RUN of a doc-focusing script from the cmline → text");

        // A library routine invoked by name (also a "script") behaves the same.
        e.run_command("LOADLIB lib.lib");
        e.run_command("TOP");
        e.open_cmdline();
        assert!(e.cmline_open());
        e.run_command("godoc");
        assert!(!e.cmline_open(), "a routine launched from the cmline → text");
    }

    #[test]
    fn cmline_change_miss_stays_in_cmline() {
        let mut e = ed();
        e.run_command("INSERT \"a a a\"");
        e.run_command("TOP");
        // Hit: replace-all that matches → focus the document.
        e.open_cmdline();
        e.insert_str("CH /a/b/");
        e.submit_cmdline();
        assert!(!e.cmline_open());
        assert_eq!(e.document().text(), "b b b");
        // Miss: nothing to replace ($ERROR) → stay in the cmline.
        e.open_cmdline();
        e.insert_str("CH /zzz/q/");
        e.submit_cmdline();
        assert!(e.cmline_open());
    }

    #[test]
    fn opening_a_file_leaves_the_caret_in_the_text() {
        let base = std::env::temp_dir().join(format!("xyscript-openfocus-{:p}", &"o"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let file = base.join("doc.txt");
        std::fs::write(&file, b"hello\n").unwrap();

        let mut e = ed();
        // NEW from the cmline → caret in the new buffer, not the cmline.
        e.run_command("GH");
        assert!(e.cmline_open());
        e.run_command("NEW");
        assert!(!e.cmline_open());

        // OPEN from the cmline → caret in the opened text.
        e.run_command("GH");
        assert!(e.cmline_open());
        e.run_command(&format!("OPEN {}", file.display()));
        assert!(!e.cmline_open());

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn opening_multiple_files_reuses_the_scratch_and_can_activate_the_first() {
        // The primitives behind the GUI's startup file-open (`xyscript.exe a b c`): open in order
        // (the first reuses the empty scratch buffer — no stray tab), then activate the first.
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        for (n, c) in [("a.txt", "AAA"), ("b.txt", "BBB"), ("c.txt", "CCC")] {
            e.host_mut().files.insert(n.into(), c.into());
        }
        let mut first = None;
        for f in ["a.txt", "b.txt", "c.txt"] {
            e.open(f).unwrap();
            if first.is_none() {
                first = Some(e.active_index());
            }
        }
        assert_eq!(e.buffer_count(), 3, "first file reused the scratch buffer → no stray empty tab");
        e.set_active_buffer(first.unwrap());
        assert_eq!(e.document().text(), "AAA"); // start on the first-named file
    }

    #[test]
    fn point_and_shoot_opens_the_file_under_the_caret() {
        let base = std::env::temp_dir().join(format!("xyscript-pns-open-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("alpha.txt"), b"ALPHA").unwrap();
        std::fs::write(base.join("beta.txt"), b"BETA").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("DIR");
        assert!(e.active_is_dir_listing(), "DIR opened a listing tab");

        // Park the caret on the beta.txt row, then OPEN with NO argument — point-and-shoot opens it.
        e.run_command("SE /beta/");
        assert!(e.host().point_and_shoot_file().is_some_and(|p| p.ends_with("beta.txt")));
        e.run_command("OPEN");
        assert!(!e.active_is_dir_listing(), "OPEN switched to the file's tab");
        assert_eq!(e.document().text(), "BETA");
    }

    #[test]
    fn point_and_shoot_lang_loads_the_catalog_under_the_caret() {
        let base = std::env::temp_dir().join(format!("xyscript-pns-lang-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("strings-xx.txt"), b"menubar.file = Zzz\n").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("DIR");
        assert!(e.active_is_dir_listing(), "DIR opened a listing tab");
        assert_eq!(e.host().lang_get("menubar.file"), None, "no catalog loaded yet");

        // Caret on the catalog row, then bare LANG (no argument) — point-and-shoot loads THAT file.
        e.run_command("SE /strings-xx/");
        assert!(e.host().point_and_shoot_file().is_some_and(|p| p.ends_with("strings-xx.txt")));
        e.run_command("LANG");
        assert_eq!(e.host().lang_get("menubar.file"), Some("Zzz"), "the catalog under the caret loaded");
        assert!(e.active_is_dir_listing(), "LANG loads in place — it doesn't switch tabs");

        // Bare LANG off a listing still clears (point-and-shoot only fires on a listing entry).
        e.run_command("NEW");
        assert!(!e.active_is_dir_listing());
        e.run_command("LANG");
        assert_eq!(e.host().lang_get("menubar.file"), None, "bare LANG on a normal buffer cleared it");
    }

    #[test]
    fn point_and_shoot_deletes_the_file_under_the_caret() {
        let base = std::env::temp_dir().join(format!("xyscript-pns-del-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("keep.txt"), b"k").unwrap();
        std::fs::write(base.join("gone.txt"), b"g").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("DIR");
        // Caret on gone.txt, then ERNV (delete-no-verify) with no argument deletes THAT file.
        e.run_command("SE /gone/");
        e.run_command("ERNV");
        assert!(!base.join("gone.txt").exists(), "point-and-shoot ERNV deleted the caret's file");
        assert!(base.join("keep.txt").exists(), "the other file is untouched");
        // The deleted file's row is gone from the listing; the survivor stays; the caret lands on a
        // real entry (so a follow-up point-and-shoot still resolves).
        assert!(e.active_is_dir_listing(), "still the listing");
        assert!(!e.document().text().contains("gone.txt"), "the deleted row was removed from the listing");
        assert!(e.document().text().contains("keep.txt"), "the surviving row stays");
        assert!(
            e.host().point_and_shoot_file().is_some_and(|p| p.ends_with("keep.txt")),
            "the caret followed onto the surviving entry"
        );
    }

    #[test]
    fn confirmed_delete_also_removes_the_row_from_the_listing() {
        use crate::host::CloseAnswer;
        let base = std::env::temp_dir().join(format!("xyscript-pns-confirm-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("doomed.txt"), b"x").unwrap();
        std::fs::write(base.join("safe.txt"), b"y").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("DIR");
        e.run_command("SE /doomed/");
        // Bare DEL (point-and-shoot) arms the confirm; answering Yes deletes AND updates the listing.
        e.run_command("DEL");
        assert!(
            e.close_prompt().is_some_and(|m| m.contains("doomed.txt")),
            "DEL armed the confirm for the caret's file"
        );
        e.resolve_close(CloseAnswer::Yes);
        assert!(!base.join("doomed.txt").exists(), "confirmed delete removed the file");
        assert!(!e.document().text().contains("doomed.txt"), "and its row left the listing");
        assert!(e.document().text().contains("safe.txt"), "the survivor stays");
    }

    #[test]
    fn rename_updates_and_resorts_the_dir_listing() {
        let base = std::env::temp_dir().join(format!("xyscript-rename-list-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("banana.txt"), b"b").unwrap();
        std::fs::write(base.join("cherry.txt"), b"c").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("DIR"); // sorted: .., banana.txt, cherry.txt
        e.run_command("SE /banana/"); // caret on banana
        e.run_command("RENAME banana.txt zulu.txt");

        assert!(base.join("zulu.txt").exists() && !base.join("banana.txt").exists(), "renamed on disk");
        let text = e.document().text();
        assert!(!text.contains("banana"), "old name gone from the listing");
        assert!(text.contains("zulu.txt"), "new name shown");
        // Re-sorted: cherry now precedes the renamed zulu.
        assert!(text.find("cherry").unwrap() < text.find("zulu").unwrap(), "listing re-sorted");
        // The caret followed the renamed file to its new row.
        assert!(
            e.host().point_and_shoot_file().is_some_and(|p| p.ends_with("zulu.txt")),
            "caret follows the renamed file"
        );
    }

    #[test]
    fn point_and_shoot_only_fires_on_a_listing_with_no_arg() {
        let mut e = ed();
        // Not a listing → no target, even for a file verb.
        e.insert_str("ordinary document");
        assert!(e.host().point_and_shoot_file().is_none(), "a normal doc is not a listing");
        // An explicit argument is never overridden — proven by DEL/RUN elsewhere; here just confirm
        // the parent `..` (top of a listing) yields no target.
        let base = std::env::temp_dir().join(format!("xyscript-pns-dotdot-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("f.txt"), b"f").unwrap();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("DIR");
        e.run_command("TF"); // top of the listing — the synthesized `..`
        assert!(
            e.host().point_and_shoot_file().is_none_or(|p| !p.ends_with("..")),
            "the `..` parent row is not a point-and-shoot target"
        );
    }

    #[test]
    fn dirrt_reuses_the_listing_tab() {
        let base = std::env::temp_dir().join(format!("xyscript-dirrt-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sub")).unwrap();
        std::fs::write(base.join("a.txt"), b"a\n").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));

        // Default (DIRRT off): the first DIR reuses the empty scratch; a second adds a new tab.
        e.run_command("DIR");
        let n1 = e.host_mut().buffer_count();
        e.run_command("DIR");
        assert_eq!(e.host_mut().buffer_count(), n1 + 1); // proliferation

        // DIRRT on: a DIR reuses the open listing tab — no new tab.
        e.run_command("DIRRT on");
        assert_eq!(e.status_message(), Some("DIRRT on"));
        e.run_command("PR $DIRRT");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
        let n2 = e.host_mut().buffer_count();
        e.run_command("DIR");
        assert_eq!(e.host_mut().buffer_count(), n2); // reused in place
        assert!(e.host_mut().active_is_dir_listing());

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn open_resolves_relative_paths_against_cwd() {
        let base = std::env::temp_dir().join(format!("xyscript-openrel-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("note.txt"), b"hello\n").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        // A bare relative name resolves under the working dir.
        e.run_command("OPEN note.txt");
        assert_eq!(e.document().text(), "hello\n");
        // The adopted path is absolute (cwd-joined), so SAVE etc. target the right file.
        let fp = e.host_mut().active_doc().path().map(|p| p.to_path_buf());
        assert_eq!(fp, Some(base.join("note.txt")));

        std::fs::remove_dir_all(&base).unwrap();
    }

    /// `DIRSET` stores default DIR switches, reports via `$DIRSET`, validates its argument, and its
    /// defaults reach a real `DIR` (here, `/R` reverses the listing) unless the command overrides the
    /// same category.
    #[test]
    fn dirset_defaults_apply_to_dir_and_validate() {
        use xyscript_xpl::{Host, Value};
        let base = std::env::temp_dir().join(format!("xyscript-dirset-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        for n in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(base.join(n), b"x").unwrap();
        }

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));

        // Set + readback.
        e.run_command("DIRSET /ON/R");
        assert_eq!(e.host().special("DIRSET"), Some(Value::str("/ON/R")));

        // The /R default reverses a bare DIR/B (name order → c, b, a among the files).
        e.run_command("CAPTURE lst DIR/B");
        let lst = e.breakpoint_vars().into_iter().collect::<std::collections::HashMap<_, _>>();
        let files: Vec<&str> =
            lst["LST"].lines().filter(|l| l.ends_with(".txt")).collect();
        assert_eq!(files, vec!["c.txt", "b.txt", "a.txt"], "the /R default reversed the listing");

        // BUT an explicit order on the CMline overrides the default /R too — otherwise `DIRSET /ON/R`
        // forces reverse onto every listing, with no way back to ascending. `DIR/B/ON` sorts by name
        // ASCENDING despite the DIRSET /R.
        e.run_command("CAPTURE asc DIR/B/ON");
        let asc = e.breakpoint_vars().into_iter().collect::<std::collections::HashMap<_, _>>();
        let asc_files: Vec<&str> = asc["ASC"].lines().filter(|l| l.ends_with(".txt")).collect();
        assert_eq!(asc_files, vec!["a.txt", "b.txt", "c.txt"], "a CMline /O cancels the DIRSET /R");
        // …and the user can still force reverse explicitly — their own /R wins.
        e.run_command("CAPTURE rev DIR/B/ON/R");
        let rev = e.breakpoint_vars().into_iter().collect::<std::collections::HashMap<_, _>>();
        let rev_files: Vec<&str> = rev["REV"].lines().filter(|l| l.ends_with(".txt")).collect();
        assert_eq!(rev_files, vec!["c.txt", "b.txt", "a.txt"], "an explicit /R still reverses");

        // Validation: /W (where it opens) and an unknown switch are rejected; state is unchanged.
        e.run_command("DIRSET /W");
        assert!(e.last_error().unwrap().contains("not a default"));
        e.run_command("DIRSET /ZZ");
        assert!(e.last_error().unwrap().contains("unknown switch"));
        assert_eq!(e.host().special("DIRSET"), Some(Value::str("/ON/R")), "a bad DIRSET leaves it unchanged");

        // Clear.
        e.run_command("DIRSET off");
        assert_eq!(e.host().special("DIRSET"), Some(Value::str("")));
        e.run_command("CAPTURE lst2 DIR/B");
        let lst2 = e.breakpoint_vars().into_iter().collect::<std::collections::HashMap<_, _>>();
        let files2: Vec<&str> =
            lst2["LST2"].lines().filter(|l| l.ends_with(".txt")).collect();
        assert_eq!(files2, vec!["a.txt", "b.txt", "c.txt"], "cleared → plain name order");

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn file_verbs_copy_rename_delete_on_disk() {
        let base = std::env::temp_dir().join(format!("xyscript-fileverbs-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("a.txt"), b"data\n").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        // COPY (relative paths resolve against cwd).
        e.run_command("COPY a.txt b.txt");
        assert!(e.status_message().unwrap().starts_with("copied"));
        assert_eq!(std::fs::read(base.join("b.txt")).unwrap(), b"data\n");
        // RENAME (move).
        e.run_command("RENAME b.txt c.txt");
        assert!(e.status_message().unwrap().starts_with("renamed"));
        assert!(!base.join("b.txt").exists());
        assert!(base.join("c.txt").exists());
        // DEL/NV <path> removes the file immediately (no confirmation).
        e.run_command("DEL/NV c.txt");
        assert!(e.status_message().unwrap().starts_with("deleted"));
        assert!(!base.join("c.txt").exists());
        // The original is untouched by all of the above.
        assert!(base.join("a.txt").exists());

        // COPY needs two paths; one is a trappable error (raised in dispatch → last_error).
        e.run_command("COPY only-one.txt");
        assert!(e.last_error().unwrap().starts_with("COPY: expected two paths"));

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn del_confirms_but_del_nv_and_ernv_do_not() {
        use crate::host::CloseAnswer;
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        for f in ["a.txt", "b.txt", "c.txt", "d.txt"] {
            e.host_mut().files.insert(f.into(), "x".into());
        }
        // Bare DEL arms a confirmation; the file survives until answered.
        e.run_command("DEL a.txt");
        assert!(e.close_prompt().unwrap().contains("Delete a.txt"));
        assert!(e.host_mut().files.contains_key("a.txt"));
        e.resolve_close(CloseAnswer::No); // decline → kept
        assert!(e.host_mut().files.contains_key("a.txt"));
        e.run_command("DEL a.txt");
        e.resolve_close(CloseAnswer::Yes); // confirm → gone
        assert!(!e.host_mut().files.contains_key("a.txt"));

        // DEL/NV and ERNV delete immediately, no prompt.
        e.run_command("DEL/NV b.txt");
        assert!(e.close_prompt().is_none());
        assert!(!e.host_mut().files.contains_key("b.txt"));
        e.run_command("ERNV c.txt");
        assert!(!e.host_mut().files.contains_key("c.txt"));

        // A missing file errors up front (no prompt armed).
        e.run_command("DEL nope.txt");
        assert!(e.close_prompt().is_none());
        assert_eq!(e.status_message(), Some("DEL: nope.txt: not found"));
        assert!(e.host_mut().files.contains_key("d.txt")); // untouched
    }

    #[test]
    fn bare_del_requires_a_filename() {
        let mut e = ed();
        e.run_command("insert \"abc\"");
        e.run_command("TOP");
        // DEL is a file-delete verb: no path → error, the document is untouched.
        e.run_command("DEL");
        assert_eq!(e.document().text(), "abc");
        assert_eq!(e.status_message(), Some("DEL: a filename is required"));
        // RC is the editor's forward character-delete (unchanged).
        e.run_command("RC");
        assert_eq!(e.document().text(), "bc");
    }

    #[test]
    fn browser_delete_confirm_removes_file_then_relists() {
        let base = std::env::temp_dir().join(format!("xyscript-browserdel-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("doomed.txt"), b"x").unwrap();
        std::fs::write(base.join("keep.txt"), b"y").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        e.run_command("DIR/B"); // entries: .., doomed.txt, keep.txt

        // The `..` parent can't be deleted (caret starts on it).
        e.dir_request_delete();
        assert!(e.dir_delete_prompt().is_none());

        // Express-key to doomed.txt, arm the confirm.
        e.dir_find_push('d');
        e.dir_request_delete();
        assert_eq!(e.dir_delete_prompt(), Some("Delete doomed.txt?  (y/n)".to_string()));

        // Cancelling leaves the file; the prompt clears.
        e.dir_resolve_delete(false);
        assert_eq!(e.status_message(), Some("delete cancelled"));
        assert!(base.join("doomed.txt").exists());
        assert!(e.dir_delete_prompt().is_none());

        // Caret is still on doomed.txt (cancel doesn't move it). Confirming deletes and
        // re-lists (the entry is gone, keep.txt remains).
        e.dir_request_delete();
        e.dir_resolve_delete(true);
        assert_eq!(e.status_message(), Some("deleted doomed.txt"));
        assert!(!base.join("doomed.txt").exists());
        assert!(base.join("keep.txt").exists());
        assert!(!e.document().text().contains("doomed.txt"));
        assert!(e.document().text().contains("keep.txt"));

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn dir_accepts_a_filespec_wildcard() {
        let base = std::env::temp_dir().join(format!("xyscript-filespec-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sub")).unwrap();
        std::fs::write(base.join("a.txt"), b"1").unwrap();
        std::fs::write(base.join("b.txt"), b"2").unwrap();
        std::fs::write(base.join("c.md"), b"3").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));

        // Filespec over the cwd: only *.txt files (plus the `..` parent), sub/ excluded.
        e.run_command("DIR/B *.txt");
        assert_eq!(e.document().text(), "..\\\na.txt\nb.txt");
        assert!(e.status_message().unwrap().starts_with("2 items matching *.txt in "));

        // No spec → everything (dirs first), so sub\ and c.md reappear.
        e.run_command("DIR/B");
        assert_eq!(e.document().text(), "..\\\nsub\\\na.txt\nb.txt\nc.md");

        // An explicit directory argument lists it whole (not treated as a filespec).
        e.run_command("DIR/B sub");
        assert_eq!(e.document().text(), "..\\"); // empty dir → just the parent link

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn dir_switches_order_reverse_and_attr_filter() {
        let base = std::env::temp_dir().join(format!("xyscript-dirsw-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sub")).unwrap();
        std::fs::write(base.join("a.txt"), b"x").unwrap();
        std::fs::write(base.join("b.txt"), b"y").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));

        // /AD = directories only (plus `..`), bare for a stable assertion.
        e.run_command("DIR/B/AD");
        assert_eq!(e.document().text(), "..\\\nsub\\");

        // /AF = files only (plus `..`).
        e.run_command("DIR/B/AF");
        assert_eq!(e.document().text(), "..\\\na.txt\nb.txt");

        // /ON/R = by name, reversed (within the files group): b.txt before a.txt.
        e.run_command("DIR/B/AF/ON/R");
        assert_eq!(e.document().text(), "..\\\nb.txt\na.txt");

        // Unknown switch is a trappable error (surfaces via last_error).
        e.run_command("DIR/Q");
        assert!(e.last_error().unwrap().starts_with("DIR: unknown switch /Q"));

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn dir_recurse_lists_subtree_with_relative_paths() {
        let base = std::env::temp_dir().join(format!("xyscript-recurse-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sub").join("inner")).unwrap();
        std::fs::write(base.join("a.txt"), b"1").unwrap();
        std::fs::write(base.join("sub").join("b.txt"), b"2").unwrap();
        std::fs::write(base.join("sub").join("inner").join("c.txt"), b"3").unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));

        // /S walks the tree; names are base-relative, sorted by path (folder-grouped).
        // (Normalize `/`→`\` so the assertion holds regardless of platform separator.)
        e.run_command("DIR/B/S");
        let actual = e.document().text().replace('/', "\\");
        assert_eq!(actual, "..\\\na.txt\nsub\\\nsub\\b.txt\nsub\\inner\\\nsub\\inner\\c.txt");

        // A filespec under /S matches the FILE NAME anywhere in the tree.
        e.run_command("DIR/B/S *.txt");
        let actual = e.document().text().replace('/', "\\");
        assert_eq!(actual, "..\\\na.txt\nsub\\b.txt\nsub\\inner\\c.txt");

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn shell_out_is_trust_gated() {
        let mut e = ed();
        // Disabled by default ($TR = 0): X errors, nothing runs.
        e.run_command("PR $TR");
        assert_eq!(e.output().last().map(String::as_str), Some("0"));
        e.run_command("X echo hi");
        assert_eq!(e.status_message(), Some("shell-out is disabled; enable it with TRUST 1"));

        // Enable for the session.
        e.run_command("TRUST 1");
        assert_eq!(e.status_message(), Some("shell-out enabled (TRUST 1)"));
        e.run_command("PR $TR");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));

        // Now X runs and sets $RC.
        e.run_command("X echo hi");
        assert_eq!(e.status_message(), Some("exit code 0"));
        e.run_command("PR $RC");
        assert_eq!(e.output().last().map(String::as_str), Some("0"));
    }

    #[test]
    fn xy_inserts_stdout_and_xv_captures_to_var() {
        let mut e = ed();
        e.run_command("TRUST 1");
        // XY inserts the command's stdout into the document (trim the shell's newline).
        e.run_command("XY echo hello");
        assert_eq!(e.document().text().trim(), "hello");
        // XV captures stdout into a user variable.
        e.run_command("XV out echo world");
        e.run_command("PR $out");
        assert_eq!(e.output().last().map(|s| s.trim()), Some("world"));
        // XV with no command is a usage error.
        e.run_command("XV lonely");
        assert!(e.last_error().unwrap().starts_with("XV: usage"));
    }

    #[test]
    fn runhere_runs_the_enclosing_code_block() {
        let mut e = ed();
        // A note with a fenced XPL block that pushes output via PR.
        e.run_command("insert \"before\\n```\\nPR ranok\\n```\\nafter\"");
        e.run_command("TOP");
        e.run_command("DOWN");
        e.run_command("DOWN"); // caret now on the `PR ranok` line (line 2)
        e.run_command("RUNHERE");
        assert_eq!(e.output().last().map(String::as_str), Some("ranok"));

        // On prose (outside any fence) → a clear error, nothing runs.
        e.run_command("TOP");
        e.run_command("RUNHERE");
        assert_eq!(e.status_message(), Some("RUNHERE: not inside a code block"));
    }

    #[test]
    fn date_specials_are_well_formed() {
        let mut e = ed();
        e.run_command("PR $DA");
        let da = e.output().last().cloned().unwrap();
        assert_eq!(da.len(), 10); // YYYY-MM-DD
        assert_eq!(&da[4..5], "-");
        assert_eq!(&da[7..8], "-");
        assert!(da[..4].chars().all(|c| c.is_ascii_digit()));

        e.run_command("PR $TM");
        let tm = e.output().last().cloned().unwrap();
        assert_eq!(tm.len(), 8); // HH:MM:SS
        assert_eq!(&tm[2..3], ":");

        // Component fields, in their AutoIt ranges.
        for (var, lo, hi) in [("$MON", 1, 12), ("$MDAY", 1, 31), ("$WDAY", 1, 7), ("$YDAY", 1, 366)]
        {
            e.run_command(&format!("PR {var}"));
            let n: u32 = e.output().last().unwrap().parse().unwrap();
            assert!((lo..=hi).contains(&n), "{var} = {n} out of range");
        }
    }

    #[test]
    fn stopwatch_and_date_verbs() {
        let mut e = ed();
        // ZT resets; `$ET` is the elapsed time (H:MM:SS), `$ETS` the elapsed seconds.
        e.run_command("ZT");
        assert_eq!(e.status_message(), Some("stopwatch reset"));
        e.run_command("PR $ET");
        assert!(e.output().last().unwrap().contains(':'));
        e.run_command("PR $ETS");
        assert!(e.output().last().unwrap().parse::<u64>().is_ok()); // a plain seconds count

        // DA inserts a "YYYY-MM-DD HH:MM:SS" timestamp.
        e.run_command("DA");
        let txt = e.document().text();
        assert_eq!(txt.len(), 19);
        assert_eq!(&txt[10..11], " ");

        // ET inserts the elapsed time (into a fresh buffer).
        e.run_command("NEW");
        e.run_command("ET");
        assert!(e.document().text().contains(':'));
    }

    #[test]
    fn txt_opens_a_modal_dialog_parsing_title_and_body() {
        let mut e = ed();
        // Bare text is verbatim, no explicit title (None → GUI default). It does NOT go to
        // output; the request stays until dismissed.
        e.run_command("TXT hello world");
        assert_eq!(e.text_request(), Some((None, "hello world".into())));
        assert!(e.output().is_empty());
        e.dismiss_text();
        assert!(e.text_request().is_none());
        // A quoted literal honors `\n` (multiline dialog content).
        e.run_command("TXT \"line1\\nline2\"");
        assert_eq!(e.text_request().map(|(_, t)| t).as_deref(), Some("line1\nline2"));
        e.dismiss_text();
        // `$`-substitution works (e.g. show a special's value).
        e.run_command("TXT $OS");
        assert!(!e.text_request().unwrap().1.is_empty());
        e.dismiss_text();
        // `/T:"..."` sets the window title (quoted, with spaces); rest is the body.
        e.run_command("TXT /T:\"My Title\" the body");
        assert_eq!(e.text_request(), Some((Some("My Title".into()), "the body".into())));
        e.dismiss_text();
        // A bare (unquoted) title.
        e.run_command("TXT /T:Notes hello");
        assert_eq!(e.text_request(), Some((Some("Notes".into()), "hello".into())));
    }

    #[test]
    fn txt_pauses_the_script_until_dismissed() {
        let mut e = ed();
        e.run_command("TXT show me; PR after");
        assert!(e.text_request().is_some());
        assert!(e.output().is_empty()); // `PR after` hasn't run — the script is paused
        // A stray command is ignored while the modal dialog is up.
        e.run_command("INSERT \"clobber\"");
        assert_eq!(e.document().text(), "");
        e.dismiss_text(); // Esc → resume
        assert!(e.text_request().is_none());
        assert_eq!(e.output(), &["after".to_string()]);
    }

    #[test]
    fn wait_parses_milliseconds() {
        let mut e = ed();
        e.run_command("WAIT 0"); // no real sleep; just dispatches cleanly
        assert!(e.last_error().is_none());
        e.run_command("WAIT abc");
        assert!(e.last_error().unwrap().starts_with("WAIT: expected milliseconds"));
    }

    #[test]
    fn cmline_focus_and_dual_cursor_memory() {
        let mut e = ed();
        e.run_command("insert \"document text\"");
        e.run_command("TOP");
        e.run_command("RIGHT");
        e.run_command("RIGHT");
        e.run_command("RIGHT");
        let doc_off = e.document().cursor(); // remembered text caret

        // GH focuses the cmline; typing now targets it.
        e.run_command("GH");
        assert!(e.cmline_open());
        e.run_command("insert \"SE /foo\"");
        assert_eq!(e.command_line().text(), "SE /foo");

        // GT returns to the text: its caret is restored, and the cmline is PRESERVED.
        e.run_command("GT");
        assert!(!e.cmline_open());
        assert_eq!(e.document().cursor(), doc_off);

        // GH again: the half-typed command is still there (dual-cursor memory).
        e.run_command("GH");
        assert_eq!(e.command_line().text(), "SE /foo");

        // BC blanks the cmline (caret to 0); TH toggles back to the text.
        e.run_command("BC");
        assert_eq!(e.command_line().text(), "");
        assert!(e.cmline_open());
        e.run_command("TH");
        assert!(!e.cmline_open());
        // The document was never touched by any of this.
        assert_eq!(e.document().text(), "document text");
    }

    #[test]
    fn cd_to_missing_directory_errors() {
        let mut e = ed();
        e.run_command("CD this-directory-does-not-exist-zzz");
        assert!(e.status_message().unwrap().starts_with("CD: not a directory"));
    }

    #[test]
    fn interactive_cmline_open_type_submit() {
        // A keymap that opens the cmline, types an INSERT command into it, and
        // submits it. The submitted command runs against the document.
        let mut tables: HashMap<(u32, u8), Assignment> = HashMap::new();
        tables.insert((0, 1), Assignment::Xpl("cmline".into())); // open
        tables.insert((0, 2), Assignment::Xpl("submit".into())); // run it
        // While the cmline is open, this key types the command text into it
        // (the quoted literal becomes the cmline contents `insert hello`).
        tables.insert((0, 3), Assignment::Xpl("insert \"insert hello\"".into()));
        let km = Keymap { modifiers: Vec::new(), tables, keys_max: 104, virtual_base: None };
        let mut e = Editor::new(km);

        e.feed_key(KeyEvent::down(1)); // open cmline
        assert!(e.cmline_open());
        e.feed_key(KeyEvent::down(3)); // type the command into the cmline
        assert_eq!(e.command_line().text(), "insert hello");
        assert_eq!(e.document().text(), ""); // document untouched while typing
        e.feed_key(KeyEvent::down(2)); // submit
        assert!(!e.cmline_open());
        assert_eq!(e.document().text(), "hello"); // command ran on the document
    }

    // ---- file I/O (step 3) -------------------------------------------------

    #[test]
    fn open_edit_save_roundtrip_preserves_crlf() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut()
            .files
            .insert("doc.txt".into(), "line1\r\nline2\r\n".into());

        e.run_command("open doc.txt");
        // Bytes stored verbatim (byte-exact model): CR is part of the content,
        // and the cursor is at the top.
        assert_eq!(e.document().text(), "line1\r\nline2\r\n");
        assert_eq!(e.document().line_text(0), "line1"); // display trims the CR
        assert_eq!(e.document().cursor(), 0);

        e.run_command("insert \"X\"");
        assert_eq!(e.document().text(), "Xline1\r\nline2\r\n");

        e.run_command("save");
        // Written back byte-for-byte (the original CRLF preserved, untouched).
        assert_eq!(
            e.host_mut().files.get("doc.txt").map(String::as_str),
            Some("Xline1\r\nline2\r\n")
        );
    }

    #[test]
    fn modified_and_filename_specials() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("notes.txt".into(), "hi".into());

        e.run_command("open notes.txt\nSET m = $MO\nSET f = $FI\nPR $m\nPR $f");
        assert_eq!(e.output(), &["0".to_string(), "notes.txt".to_string()]); // clean, named

        e.run_command("insert \"!\"\nSET m = $MO\nPR $m");
        assert_eq!(e.output(), &["1".to_string()]); // modified after edit

        e.run_command("save\nSET m = $MO\nPR $m");
        assert_eq!(e.output(), &["0".to_string()]); // clean again after save
        // Cursor was at the top after open, so "!" prepended.
        assert_eq!(e.host_mut().files.get("notes.txt").map(String::as_str), Some("!hi"));
    }

    #[test]
    fn logical_position_specials_ln_lc_tl() {
        let mut e = Editor::new(typing_keymap());
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        // Build "ab\ncd"; caret ends after "cd" → line 2, column 3 (1-based), 2 lines total.
        e.run_command(r#"INSERT "ab""#);
        e.run_command("ENTER");
        e.run_command(r#"INSERT "cd""#);
        e.run_command("PR $LN"); // logical line, 1-based
        assert_eq!(out(&e).as_deref(), Some("2"));
        e.run_command("PR $LC"); // logical column, 1-based (the new meaning)
        assert_eq!(out(&e).as_deref(), Some("3"));
        e.run_command("PR $TL"); // total lines (line-count, rehomed from $LC)
        assert_eq!(out(&e).as_deref(), Some("2"));
        e.run_command("HOME\nPR $LC"); // line start → column 1
        assert_eq!(out(&e).as_deref(), Some("1"));
    }

    #[test]
    fn visual_position_specials_cx_cy() {
        let mut e = Editor::new(typing_keymap());
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        e.run_command(r#"INSERT "abc""#); // caret after "abc" on line 1
        // Headless (no GUI push): fall back to logical column (0-based) and viewport-relative row.
        e.run_command("PR $CX");
        assert_eq!(out(&e).as_deref(), Some("3"));
        e.run_command("PR $CY");
        assert_eq!(out(&e).as_deref(), Some("0"));
        // Once the GUI pushes the wrap-aware visual position, $CX/$CY report it.
        e.host_mut().set_cursor_visual(7, 4);
        e.run_command("PR $CX\nPR $CY");
        assert_eq!(e.output(), &["7".to_string(), "4".to_string()]);
    }

    #[test]
    fn abv_save_prompt_and_abd_delete_confirm() {
        use crate::host::CloseAnswer;
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("a.txt".into(), "aaa".into());

        // AB/V on an UNMODIFIED buffer closes immediately (no prompt).
        e.run_command("open a.txt");
        assert!(e.close_prompt().is_none());
        e.run_command("AB/V");
        assert!(e.close_prompt().is_none());
        assert_eq!(e.active_file_name(), None); // closed → fresh untitled

        // AB/V on a MODIFIED buffer arms a save-prompt; "yes" saves then closes.
        e.run_command("open a.txt");
        e.run_command(r#"insert "Z""#); // modified → "Zaaa"
        e.run_command("AB/V");
        assert!(e.close_prompt().unwrap().contains("Save changes"));
        e.resolve_close(CloseAnswer::Yes);
        assert!(e.close_prompt().is_none());
        assert_eq!(e.host_mut().files.get("a.txt").map(String::as_str), Some("Zaaa")); // saved
        assert_eq!(e.active_file_name(), None); // and closed

        // "no" discards: re-open, modify, AB/V, answer No → closed without saving.
        e.run_command("open a.txt");
        e.run_command(r#"insert "Q""#);
        e.run_command("AB/V");
        e.resolve_close(CloseAnswer::No);
        assert_eq!(e.host_mut().files.get("a.txt").map(String::as_str), Some("Zaaa")); // unchanged

        // ABD/V arms a delete-confirm; "yes" removes the file from disk + closes.
        e.host_mut().files.insert("a.txt".into(), "Zaaa".into());
        e.run_command("open a.txt");
        e.run_command("ABD/V");
        assert!(e.close_prompt().unwrap().contains("Delete"));
        e.resolve_close(CloseAnswer::Yes);
        assert!(!e.host_mut().files.contains_key("a.txt")); // gone from disk
        assert!(e.close_prompt().is_none());

        // ABD/V with "no" cancels: the file stays, the buffer stays open.
        e.host_mut().files.insert("a.txt".into(), "Zaaa".into());
        e.run_command("open a.txt");
        e.run_command("ABD/V");
        e.resolve_close(CloseAnswer::No);
        assert!(e.host_mut().files.contains_key("a.txt")); // still on disk
        assert_eq!(e.active_file_name().as_deref(), Some("a.txt")); // still open

        // Bare ABD deletes + closes immediately, no prompt.
        e.run_command("ABD");
        assert!(e.close_prompt().is_none());
        assert!(!e.host_mut().files.contains_key("a.txt")); // gone from disk
        assert_eq!(e.active_file_name(), None); // and closed

        // ABD on an untitled (never-saved) buffer just closes, reporting the absence.
        e.run_command(r#"insert "draft""#);
        e.run_command("ABD");
        assert!(e.close_prompt().is_none());
        assert!(e.status_message().unwrap_or_default().contains("no file on disk"));
        assert!(!e.document().text().contains("draft")); // the draft buffer is gone
    }

    #[test]
    fn abd_on_a_file_already_gone_from_disk_just_closes_quietly() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("a.txt".into(), "content".into());
        e.run_command("OPEN a.txt");
        e.host_mut().files.remove("a.txt"); // the file disappears from disk while open
        // ABD: no error, no confirm — just closes like AB.
        e.run_command("ABD");
        assert!(e.close_prompt().is_none(), "ABD must not prompt for a missing file");
        assert_eq!(e.active_file_name(), None, "buffer closed");
        let st = e.status_message().unwrap_or_default();
        assert!(st.starts_with("closed") && !st.contains("fail"), "quiet close, not an error: {st:?}");

        // ABD/V on a missing file also closes quietly — no confirm prompt is armed.
        e.host_mut().files.insert("b.txt".into(), "content".into());
        e.run_command("OPEN b.txt");
        e.host_mut().files.remove("b.txt");
        e.run_command("ABD/V");
        assert!(e.close_prompt().is_none(), "ABD/V must not prompt for a missing file");
        assert_eq!(e.active_file_name(), None, "buffer closed");
    }

    #[test]
    fn save_copy_keeps_tab_and_confirms_overwrite() {
        use crate::host::CloseAnswer;
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("orig.txt".into(), "hello".into());

        e.run_command("open orig.txt");
        e.run_command(r#"insert "X""#); // buffer modified

        // SAVE <name> writes a COPY; the tab stays on orig.txt and orig.txt on disk is untouched.
        e.run_command("SAVE copy.txt");
        let buf = e.document().text();
        assert_eq!(e.host_mut().files.get("copy.txt").cloned(), Some(buf));
        assert_eq!(e.active_file_name().as_deref(), Some("orig.txt")); // tab unchanged
        assert_eq!(e.host_mut().files.get("orig.txt").map(String::as_str), Some("hello"));

        // SAVE onto an EXISTING file arms an overwrite confirm; "no" leaves the target as-is.
        e.run_command(r#"insert "Y""#);
        let before = e.host_mut().files.get("copy.txt").cloned();
        e.run_command("SAVE copy.txt");
        assert!(e.close_prompt().unwrap().contains("Overwrite"));
        e.resolve_close(CloseAnswer::No);
        assert_eq!(e.host_mut().files.get("copy.txt").cloned(), before);

        // "yes" overwrites with the current buffer.
        e.run_command("SAVE copy.txt");
        assert!(e.close_prompt().is_some());
        e.resolve_close(CloseAnswer::Yes);
        let buf = e.document().text();
        assert_eq!(e.host_mut().files.get("copy.txt").cloned(), Some(buf));

        // SAVE/NV overwrites silently — no prompt.
        e.run_command(r#"insert "Z""#);
        e.run_command("SAVE/NV copy.txt");
        assert!(e.close_prompt().is_none());
        let buf = e.document().text();
        assert_eq!(e.host_mut().files.get("copy.txt").cloned(), Some(buf));
    }

    #[test]
    fn savekbd_dumps_the_operative_keymap_and_reloads() {
        let mut e = Editor::new(typing_keymap()); // 16→'h', 17→'i', 18→BD, …
        e.host_mut().virtual_fs = true;
        e.run_command("SAVEKBD dump.kbd");
        let dumped = e.host_mut().files.get("dump.kbd").cloned().expect("dump written");
        assert!(dumped.contains("KEYS="));
        assert!(dumped.contains("16=h")); // desugared `insert "h"` re-sugared to the terse form
        assert!(dumped.contains("18=bd")); // a command cell is emitted verbatim
        // Re-loading the dump reproduces the layout.
        e.run_command("LOADKBD dump.kbd");
        e.feed_key(KeyEvent::down(16));
        e.feed_key(KeyEvent::down(17));
        assert_eq!(e.document().text(), "hi");
    }

    #[test]
    fn quiet_suppresses_statuses_keeps_pr_and_errors_by_level() {
        let mut e = ed();
        // Baseline: an automatic op-result status shows.
        e.run_command("TB 4");
        assert_eq!(e.command_feedback().as_deref(), Some("tab width 4"));
        // QUIET hides op-result statuses...
        e.run_command("QUIET\nTB 6");
        assert_eq!(e.command_feedback(), None);
        // ...but explicit PR still shows...
        e.run_command("QUIET\nPR hello");
        assert_eq!(e.command_feedback().as_deref(), Some("hello"));
        // ...and a non-fatal error still shows under plain QUIET.
        e.run_command("QUIET\nSAD nope.txt"); // no selection → trappable error
        assert!(e.command_feedback().unwrap_or_default().contains("no text is selected"));
        // QUIET/NV also hides the non-fatal error — but $ERROR is still set (read it back).
        e.run_command("QUIET/NV\nSAD nope.txt\nQUIET OFF\nPR $ERROR");
        assert_eq!(e.command_feedback().as_deref(), Some("1"));
        // Quiet is script-scoped: a fresh top-level run starts un-quieted.
        e.run_command("TB 8");
        assert_eq!(e.command_feedback().as_deref(), Some("tab width 8"));
    }

    #[test]
    fn fatal_error_reaches_command_feedback() {
        let mut e = ed();
        // An unknown command is a FATAL error: it aborts the run and (unlike a trappable `$ERROR`)
        // never sets status_message — but it must still surface on the status line.
        e.run_command("NOSUCHVERB xyz");
        let fb = e.command_feedback().unwrap_or_default();
        assert!(fb.starts_with("error:") && fb.contains("unknown command"), "fb = {fb:?}");
        // A later successful command clears it (last_error is reset on each run).
        e.run_command("TB 4");
        assert_eq!(e.command_feedback().as_deref(), Some("tab width 4"));
    }

    #[test]
    fn merge_inserts_file_contents_at_the_caret() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("snippet.txt".into(), "INSERTED".into());
        e.run_command(r#"INSERT "AB""#); // "AB", caret at end
        e.run_command("HOME"); // caret to the start
        e.run_command("ME snippet.txt"); // merge the file at the caret
        assert_eq!(e.document().text(), "INSERTEDAB");
        // Missing file → trappable error (sets $ERROR), buffer unchanged.
        e.run_command("MERGE nope.txt");
        assert_eq!(e.document().text(), "INSERTEDAB");
        e.run_command("PR $ERROR");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
    }

    #[test]
    fn bedit_edits_a_file_invisibly_then_auto_exits() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("data.txt".into(), "hello world".into());
        // The visible document has its own content.
        e.run_command(r#"INSERT "VISIBLE""#);
        assert_eq!(e.document().text(), "VISIBLE");
        // Blind-edit the file: specials read the hidden buffer mid-script (not the visible doc).
        e.run_command("BEDIT data.txt\nPR $LI");
        assert_eq!(e.output().last().map(String::as_str), Some("hello world"));
        // ...but the visible doc was unaffected, and the blind edit auto-exited at script end.
        assert_eq!(e.document().text(), "VISIBLE");
        // A full edit-and-save in one script writes the file; visible doc still untouched.
        e.run_command("BEDIT data.txt\nCHA /world/there/\nSA");
        assert_eq!(e.host_mut().files.get("data.txt").map(String::as_str), Some("hello there"));
        assert_eq!(e.document().text(), "VISIBLE");
        // Blind gone → subsequent edits hit the visible doc again.
        e.run_command(r#"INSERT "!""#);
        assert_eq!(e.document().text(), "VISIBLE!");
    }

    #[test]
    fn bedit_ab_discards_and_bare_bedit_creates() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("keep.txt".into(), "original".into());
        // Edit then AB without SA → the file is left unchanged (discard).
        e.run_command("BEDIT keep.txt\nCHA /original/changed/\nAB");
        assert_eq!(e.host_mut().files.get("keep.txt").map(String::as_str), Some("original"));
        // Bare BEDIT = a new unnamed blind buffer; build it and SAVE <path> to create the file.
        e.run_command(r#"BEDIT; INSERT "built from scratch"; SAVE made.txt"#);
        assert_eq!(e.host_mut().files.get("made.txt").map(String::as_str), Some("built from scratch"));
    }

    #[test]
    fn bclose_closes_the_blind_edit_by_name() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("keep.txt".into(), "original".into());
        // (A blind edit auto-exits at SCRIPT end, so `BEDIT` and `BCLOSE` share one script here; `$FI`
        // reads the blind target's name while it's open, else the visible untitled doc's empty name.)

        // Named BCLOSE discards changes (like AB) and closes the blind buffer.
        e.run_command("BEDIT keep.txt\nCHA /original/changed/\nBCLOSE keep.txt");
        assert!(!e.host_mut().blind_active(), "BCLOSE closed the blind edit");
        assert_eq!(e.host_mut().files.get("keep.txt").map(String::as_str), Some("original")); // discarded

        // A NAME MISMATCH is a trappable error ($ERROR) and leaves the blind edit OPEN (still `$FI`).
        e.run_command("BEDIT keep.txt\nCLERR\nSET hit = 0\nBCLOSE other.txt\nIF $ERROR SET hit = 1\nPR $hit $FI");
        assert_eq!(e.output().last().map(String::as_str), Some("1 keep.txt"), "mismatch: $ERROR, blind stays open");

        // A bare BCLOSE closes whatever blind buffer is open (afterwards `$FI` is the empty visible doc).
        e.run_command("BEDIT keep.txt\nBCLOSE\nPR [$FI]");
        assert_eq!(e.output().last().map(String::as_str), Some("[]"), "bare BCLOSE closed it");

        // BCLOSE with nothing open → trappable error.
        e.run_command("CLERR\nSET hit = 0\nBCLOSE\nIF $ERROR SET hit = 1\nPR $hit");
        assert_eq!(e.output().last().map(String::as_str), Some("1"), "no blind edit → $ERROR");
    }

    #[test]
    fn clearkbd_resets_to_the_builtin_default() {
        let mut e = Editor::new(typing_keymap());
        e.feed_key(KeyEvent::down(16)); // typing_keymap binds 16 → 'h'
        assert_eq!(e.document().text(), "h");
        // CLEARKBD now RESETS to the built-in baseline (not an empty map): the editing essentials
        // work — letters type their default values and Backspace deletes.
        e.run_command("CLEARKBD");
        e.feed_key(KeyEvent::down(16)); // default: 16 → 'q'
        e.feed_key(KeyEvent::down(17)); // default: 17 → 'w'
        assert_eq!(e.document().text(), "hqw");
        e.feed_key(KeyEvent::down(14)); // default: 14 → BD (backspace)
        assert_eq!(e.document().text(), "hq");
    }

    #[test]
    fn background_job_is_trust_gated() {
        let mut e = Editor::new(typing_keymap());
        e.run_command("X/BG echo hi"); // shell-out disabled by default
        assert!(e.status_message().unwrap_or_default().contains("shell-out is disabled"));
        assert_eq!(e.job_count(), 0);
    }

    #[test]
    fn last_job_id_special_tracks_launched_job() {
        let mut e = Editor::new(typing_keymap());
        // No job started yet → $LJ is 0.
        e.run_command("PR $LJ");
        assert_eq!(e.output().last().map(String::as_str), Some("0"));
        // Launch one; $LJ captures its id so a script can `WAIT/J $LJ`.
        e.run_command("TRUST 1");
        e.run_command("X/BG echo hi");
        e.run_command("PR $LJ");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
        // Reap so the worker thread/child doesn't linger past the test.
        let mut waited = 0;
        while e.job_count() > 0 && waited < 400 {
            let _ = e.poll_jobs();
            std::thread::sleep(std::time::Duration::from_millis(5));
            waited += 1;
        }
    }

    #[test]
    fn background_job_return_pipe_delivers_output_to_variable() {
        let mut e = Editor::new(typing_keymap());
        e.run_command("TRUST 1");
        // `/R:out` — when the job finishes, its full output is delivered into $out.
        e.run_command("X/BG/R:out echo piped");
        let mut waited = 0;
        while e.job_count() > 0 && waited < 400 {
            let _ = e.poll_jobs(); // drains the job AND delivers /R output into the Env
            std::thread::sleep(std::time::Duration::from_millis(5));
            waited += 1;
        }
        assert_eq!(e.job_count(), 0, "background job did not finish in time");
        e.run_command("PR $out");
        assert!(e.output().last().unwrap().contains("piped"), "out = {:?}", e.output());
    }

    #[test]
    fn nav_edit_verbs_pull_focus_from_cmline() {
        use crate::host::Target;
        let mut e = Editor::new(typing_keymap());

        // Focused in the cmline: a designated nav verb shifts focus to the document.
        e.host_mut().input_focus = Target::CmLine;
        e.host_mut().target = Target::CmLine;
        e.run_command("NW");
        assert_eq!(e.host_mut().target, Target::Document);
        assert_eq!(e.host_mut().input_focus, Target::Document);

        // A within-line move (`CL` = char left) is NOT in the set — it stays with the cmline so you
        // can still move the caret while editing a command.
        e.host_mut().input_focus = Target::CmLine;
        e.host_mut().target = Target::CmLine;
        e.run_command("CL");
        assert_eq!(e.host_mut().target, Target::CmLine);

        // A big-jump motion (`TF` top-of-file), a define (`DFA`), and an R-delete (`RL`) DO shift.
        for verb in ["TF", "DFA", "RL"] {
            e.host_mut().input_focus = Target::CmLine;
            e.host_mut().target = Target::CmLine;
            e.run_command(verb);
            assert_eq!(e.host_mut().target, Target::Document, "{verb} should shift focus");
        }

        // XC/SUBMIT state (target transiently Document, persistent focus still cmline) is NOT
        // disturbed — the shift is conditional on the live target being the cmline.
        e.host_mut().input_focus = Target::CmLine;
        e.host_mut().target = Target::Document;
        e.run_command("DW");
        assert_eq!(e.host_mut().input_focus, Target::CmLine);
    }

    #[test]
    fn savecm_writes_runnable_history_script_that_round_trips() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().push_history("dfa");
        e.host_mut().push_history("se /w \"x\"");
        e.run_command("SAVECM hist.xpl");
        let saved = e.host_mut().files.get("hist.xpl").expect("history file written").clone();
        assert!(saved.contains("LoadCM \""), "expected a LoadCM line, got: {saved}");
        // Round-trip: wipe the history, RUN the saved script, the commands come back.
        e.run_command("LoadCM \"\"");
        assert!(e.host_mut().command_history().is_empty());
        e.run_command("RUN hist.xpl");
        let hist: Vec<String> = e.host_mut().command_history().to_vec();
        assert!(hist.iter().any(|c| c == "dfa"), "not restored: {hist:?}");
        assert!(hist.iter().any(|c| c == "se /w \"x\""), "not restored (escaping?): {hist:?}");
    }

    #[test]
    fn hlcolor_sets_clears_resets_and_persists() {
        use crate::highlight::TokenKind;
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        // Built-in default for `keyword`.
        assert_eq!(e.highlight_color(&TokenKind::Keyword), Some((170, 40, 180)));
        // Recolour, and clear another kind to the default text colour.
        e.run_command("HLCOLOR keyword 10 20 30");
        assert_eq!(e.highlight_color(&TokenKind::Keyword), Some((10, 20, 30)));
        e.run_command("HLCOLOR comment default");
        assert_eq!(e.highlight_color(&TokenKind::Comment), None);
        // An arbitrary (grammar-defined) kind name is now accepted and themed.
        e.run_command("HLCOLOR macro 1 2 3");
        assert_eq!(e.highlight_color(&TokenKind::Other("macro".into())), Some((1, 2, 3)));
        assert_eq!(e.highlight_color(&TokenKind::Keyword), Some((10, 20, 30)));
        // SAVECFG emits the changed built-ins AND the custom kind.
        e.run_command("SAVECFG cfg.cfg");
        let cfg = e.host_mut().files.get("cfg.cfg").unwrap().clone();
        assert!(cfg.contains("HLCOLOR keyword 10 20 30"), "cfg: {cfg}");
        assert!(cfg.contains("HLCOLOR comment default"), "cfg: {cfg}");
        assert!(cfg.contains("HLCOLOR macro 1 2 3"), "cfg: {cfg}");
        // HLRESET restores the defaults and drops custom kinds.
        e.run_command("HLRESET");
        assert_eq!(e.highlight_color(&TokenKind::Keyword), Some((170, 40, 180)));
        assert_eq!(e.highlight_color(&TokenKind::Comment), Some((30, 150, 160)));
        assert_eq!(e.highlight_color(&TokenKind::Other("macro".into())), None);
    }

    #[test]
    fn hlkinds_opens_a_txt_dialog_listing_kinds() {
        let mut e = Editor::new(typing_keymap());
        e.run_command("HLCOLOR keyword 10 20 30");
        e.run_command("HLKINDS");
        let (title, text) = e.text_request().expect("HLKINDS should open a TXT dialog");
        assert_eq!(title.as_deref(), Some("Highlight kinds"));
        assert!(text.contains("keyword"), "lists the keyword kind: {text}");
        assert!(text.contains("10 20 30"), "reflects current colours: {text}");
        assert!(text.contains("comment"), "lists every kind: {text}");
        assert!(text.lines().count() >= 11, "all kinds present: {text}");
    }

    #[test]
    fn hlkinds_carries_per_line_colours_aligned_with_the_lines() {
        let mut e = Editor::new(typing_keymap());
        e.run_command("HLCOLOR keyword 10 20 30");
        e.run_command("HLKINDS");
        let (_t, text) = e.text_request().expect("HLKINDS opens a dialog");
        let colors = e.line_colors_request().expect("HLKINDS carries per-line colours");
        // One colour entry per line, exactly aligned.
        assert_eq!(colors.len(), text.lines().count(), "a colour per line");
        // The `keyword` line reports the colour just set (aligned by index with the text lines).
        let idx = text.lines().position(|l| l.starts_with("keyword")).expect("keyword listed");
        assert_eq!(colors[idx], Some((10, 20, 30)));
    }

    #[test]
    fn listlib_opens_a_txt_dialog() {
        let mut e = Editor::new(typing_keymap());
        e.run_command("LISTLIB");
        let (title, text) = e.text_request().expect("LISTLIB should open a TXT dialog");
        assert_eq!(title.as_deref(), Some("Library routines"));
        assert!(!text.is_empty(), "the dialog has a body (even '(no routines)'): {text}");
        e.dismiss_text();
        assert!(e.text_request().is_none());
    }

    #[test]
    fn listlib_under_capture_feeds_the_variable() {
        let mut e = Editor::new(typing_keymap());
        e.run_command("CAPTURE lib LISTLIB"); // captured, not shown in a dialog
        assert!(e.text_request().is_none(), "CAPTURE must not open the TXT dialog");
        e.run_command("PR $lib");
        assert!(e.output().last().map(|s| !s.is_empty()).unwrap_or(false));
    }

    #[test]
    fn help_verb_opens_a_txt_dialog_and_disambiguates() {
        let mut e = Editor::new(typing_keymap());
        // `HELP CV` shows the command's entry in a scrollable dialog.
        e.run_command("HELP CV");
        let (title, text) = e.text_request().expect("HELP should open a TXT dialog");
        assert_eq!(title.as_deref(), Some("Help: CV / CVA"));
        assert!(text.contains("change-with-verify"), "body: {text}");
        e.dismiss_text();
        assert!(e.text_request().is_none());
        // `?` is an alias; `$` disambiguates the special from the same-named command.
        e.run_command("? $SE");
        let (_, text) = e.text_request().expect("? alias opens the dialog");
        assert!(text.contains("special variable") && text.contains("selected text"), "body: {text}");
        e.dismiss_text();
    }

    #[test]
    fn help_unknown_name_reports_suggestions_without_a_dialog() {
        let mut e = Editor::new(typing_keymap());
        e.run_command("HELP CVV"); // a near-miss for CV/CVA
        assert!(e.text_request().is_none(), "no dialog for an unknown name");
        e.run_command("PR $ERROR");
        assert_eq!(e.output().last().map(String::as_str), Some("1"), "unknown HELP sets $ERROR");
    }

    #[test]
    fn unknown_command_suggests_near_matches() {
        let mut e = Editor::new(typing_keymap());
        e.run_command("CVV /a/b/"); // not a verb; one edit from CV
        let err = e.last_error().expect("unknown command errors");
        assert!(err.contains("unknown command"), "{err}");
        assert!(err.contains("did you mean") && err.contains("CV"), "{err}");
    }

    #[test]
    fn ab_closes_to_the_previous_buffer() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        for (n, c) in [("a.txt", "AAA"), ("b.txt", "BBB"), ("c.txt", "CCC")] {
            e.host_mut().files.insert(n.into(), c.into());
        }
        e.run_command("OPEN a.txt"); // reuses the scratch → [a]
        e.run_command("OPEN b.txt"); // [a, b]
        e.run_command("OPEN c.txt"); // [a, b, c]
        assert_eq!(e.buffer_count(), 3);
        // Close the MIDDLE tab → land on the PREVIOUS (left) buffer, not the next.
        e.run_command("BUFGO 2"); // → b
        assert_eq!(e.document().text(), "BBB");
        e.run_command("AB");
        assert_eq!(e.buffer_count(), 2);
        assert_eq!(e.document().text(), "AAA", "AB lands on the previous buffer");
        // Closing the FIRST tab lands on the new first (former c).
        e.run_command("AB");
        assert_eq!(e.document().text(), "CCC");
    }

    #[test]
    fn st_saves_then_closes_but_keeps_an_untitled_tab() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("a.txt".into(), "aaa".into());
        e.host_mut().files.insert("b.txt".into(), "bbb".into());
        e.run_command("OPEN a.txt"); // reuses the scratch → [a]
        e.run_command("OPEN b.txt"); // [a, b], active = b
        e.run_command("INSERT \"X\""); // b = "Xbbb"
        assert_eq!(e.buffer_count(), 2);
        e.run_command("ST"); // save b + close its tab
        assert_eq!(e.host_mut().files.get("b.txt").map(String::as_str), Some("Xbbb"), "ST saved");
        assert_eq!(e.buffer_count(), 1, "ST closed the saved tab");
        assert!(e.active_path().unwrap().to_string_lossy().contains("a.txt"), "landed on a.txt");

        // Bare ST on an UNTITLED doc must NOT discard it: save errors, tab stays.
        e.run_command("NEW"); // fresh untitled tab → [a, untitled]
        e.run_command("INSERT \"scratch\"");
        let n = e.buffer_count();
        e.run_command("ST");
        e.run_command("PR $ERROR");
        assert_eq!(e.output().last().map(String::as_str), Some("1"), "bare ST on untitled sets $ERROR");
        assert_eq!(e.buffer_count(), n, "ST did NOT close the unsaved untitled tab");

        // `ST <path>` names the untitled doc, saves it, and closes — addresses the untitled case.
        e.run_command("ST fresh.txt");
        assert_eq!(e.host_mut().files.get("fresh.txt").map(String::as_str), Some("scratch"));
        assert_eq!(e.buffer_count(), n - 1, "ST <path> saved + closed the untitled tab");
    }

    #[test]
    fn st_path_confirms_overwrite_unless_nv() {
        use crate::host::CloseAnswer;
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("keep.txt".into(), "keep".into());
        e.host_mut().files.insert("target.txt".into(), "OLD".into());
        e.run_command("OPEN keep.txt"); // a persistent tab so closing another gives a clean count
        // A doc whose ST would land on the existing target.txt.
        e.run_command("NEW");
        e.run_command("INSERT \"NEW\"");
        let n = e.buffer_count();
        // `ST <existing>` arms an overwrite confirmation; the file/tab are untouched until answered.
        e.run_command("ST target.txt");
        assert!(e.close_prompt().unwrap().contains("already exists"), "overwrite confirm armed");
        assert_eq!(e.host_mut().files.get("target.txt").map(String::as_str), Some("OLD"));
        assert_eq!(e.buffer_count(), n);
        // Decline → nothing changes, tab stays.
        e.resolve_close(CloseAnswer::No);
        assert_eq!(e.host_mut().files.get("target.txt").map(String::as_str), Some("OLD"));
        assert_eq!(e.buffer_count(), n, "declining the overwrite keeps the tab");
        // Confirm → overwrite, adopt, close.
        e.run_command("ST target.txt");
        e.resolve_close(CloseAnswer::Yes);
        assert_eq!(e.host_mut().files.get("target.txt").map(String::as_str), Some("NEW"), "overwritten");
        assert_eq!(e.buffer_count(), n - 1, "confirmed ST closed the tab");

        // `ST/NV <existing>` overwrites with no prompt.
        e.host_mut().files.insert("t2.txt".into(), "OLD2".into());
        e.run_command("NEW");
        e.run_command("INSERT \"NEW2\"");
        let m = e.buffer_count();
        e.run_command("ST/NV t2.txt");
        assert!(e.close_prompt().is_none(), "/NV skips the confirmation");
        assert_eq!(e.host_mut().files.get("t2.txt").map(String::as_str), Some("NEW2"));
        assert_eq!(e.buffer_count(), m - 1);
    }

    #[test]
    fn closing_a_named_file_records_it_with_caret_and_dedups() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("a.txt".into(), "hello world".into());
        e.host_mut().files.insert("b.txt".into(), "second".into());
        // Close a.txt with the caret at offset 6.
        e.run_command("OPEN a.txt");
        e.run_command("JMP 6");
        e.run_command("AB");
        {
            let rc = e.host_mut().recently_closed();
            assert_eq!(rc.len(), 1);
            assert_eq!(rc[0].0.to_string_lossy(), "a.txt");
            assert_eq!(rc[0].1, 6, "caret recorded");
        }
        // Close b.txt, then a.txt again → a.txt moves to the top, deduped (still 2 entries).
        e.run_command("OPEN b.txt");
        e.run_command("AB");
        e.run_command("OPEN a.txt");
        e.run_command("AB");
        let rc = e.host_mut().recently_closed();
        assert_eq!(rc.len(), 2);
        assert_eq!(rc[0].0.to_string_lossy(), "a.txt"); // most recent
        assert_eq!(rc[1].0.to_string_lossy(), "b.txt");
    }

    #[test]
    fn reopen_chooser_reopens_and_restores_caret() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("a.txt".into(), "hello world".into());
        e.host_mut().files.insert("b.txt".into(), "second file".into());
        // Close both (a, then b → b is newest).
        e.run_command("OPEN a.txt");
        e.run_command("JMP 6");
        e.run_command("AB");
        e.run_command("OPEN b.txt");
        e.run_command("AB");
        // REOPEN opens the chooser (shares the PICK dialog), newest first.
        e.run_command("REOPEN");
        let (title, items) = e.pick_request().expect("REOPEN opens the chooser");
        assert_eq!(title.as_deref(), Some("Reopen a recently-closed file"));
        assert_eq!(items.len(), 2);
        assert!(items[0].contains("b.txt"), "newest first: {:?}", items[0]);
        assert!(items[1].contains("a.txt"));
        // Pick a.txt (index 1) → reopens it with the caret restored to offset 6.
        e.pick_respond(Some(1));
        assert!(e.pick_request().is_none());
        assert_eq!(e.document().text(), "hello world");
        assert_eq!(e.document().cursor(), 6, "caret restored");
        // a.txt is now removed from the reopen list (it's open); b.txt remains.
        let rc = e.host_mut().recently_closed();
        assert_eq!(rc.len(), 1);
        assert_eq!(rc[0].0.to_string_lossy(), "b.txt");
    }

    #[test]
    fn listtabs_chooser_switches_to_the_picked_tab() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("a.txt".into(), "aaa".into());
        e.host_mut().files.insert("b.txt".into(), "bbb".into());
        e.host_mut().files.insert("c.txt".into(), "ccc".into());
        // Open three files; they ride in tabs 1/2/3 and we end on c.txt (the last).
        e.run_command("OPEN a.txt");
        e.run_command("OPEN b.txt");
        e.run_command("OPEN c.txt");
        assert!(e.document().text() == "ccc", "start on the last-opened tab");
        // LISTTABS opens the chooser: one row per tab, numbered, current flagged with `*`.
        e.run_command("LISTTABS");
        let (title, items) = e.pick_request().expect("LISTTABS opens the chooser");
        assert_eq!(title.as_deref(), Some("Switch to tab"));
        assert_eq!(items.len(), 3); // a/b/c (the first OPEN reuses the empty scratch buffer)
        assert!(items.last().unwrap().contains("c.txt"));
        assert!(items.iter().any(|r| r.starts_with('*')), "current tab flagged: {items:?}");
        // Pick the a.txt row → that tab becomes active.
        let a_row = items.iter().position(|r| r.contains("a.txt")).unwrap();
        e.pick_respond(Some(a_row));
        assert!(e.pick_request().is_none());
        assert_eq!(e.document().text(), "aaa", "switched to the a.txt tab");
    }

    #[test]
    fn marklist_chooser_jumps_to_the_picked_bookmark() {
        let mut e = Editor::new(typing_keymap());
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        e.run_command(r#"insert "abcdefghij""#);
        // No bookmarks yet → MARKLIST is a status note, not a chooser.
        e.run_command("MARKLIST");
        assert!(e.pick_request().is_none(), "no chooser without bookmarks");
        // Two bookmarks, at offsets 0 and 5.
        e.run_command("TF");
        e.run_command("MARKSET");
        e.run_command("JMP 5");
        e.run_command("MARKSET");
        // Caret away from both, then open the chooser.
        e.run_command("TF");
        e.run_command("MARKLIST");
        let (title, items) = e.pick_request().expect("MARKLIST opens the chooser");
        assert_eq!(title.as_deref(), Some("Go to bookmark"));
        assert_eq!(items.len(), 2);
        assert!(items[0].starts_with("L1"), "preview shows the line number: {items:?}");
        // Pick the second row → the caret jumps to that bookmark (offset 5).
        e.pick_respond(Some(1));
        assert!(e.pick_request().is_none());
        e.run_command("PR $CP");
        assert_eq!(out(&e).as_deref(), Some("5"));
        // Cancelling leaves the caret put and sets $ERROR.
        e.run_command("MARKLIST");
        assert!(e.pick_request().is_some());
        e.pick_respond(None);
        e.run_command("PR $ERROR");
        assert_eq!(out(&e).as_deref(), Some("1"));
    }

    #[test]
    fn listtabs_cancel_leaves_the_active_tab_unchanged() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("a.txt".into(), "aaa".into());
        e.run_command("OPEN a.txt");
        assert_eq!(e.document().text(), "aaa");
        e.run_command("LISTTABS");
        assert!(e.pick_request().is_some());
        e.pick_respond(None); // Esc / window ×
        assert!(e.pick_request().is_none());
        assert_eq!(e.document().text(), "aaa", "cancel is a no-op");
    }

    #[test]
    fn reopen_with_empty_list_is_a_graceful_noop() {
        let mut e = ed();
        e.run_command("REOPEN");
        assert!(e.pick_request().is_none(), "no chooser when nothing was closed");
    }

    #[test]
    fn savevars_loadvars_round_trip_all_types() {
        use std::collections::HashMap;
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        // A string with a `$`, an embedded quote, a tab, and a non-ASCII char — the tricky cases.
        e.run_command("SET greeting = \"cost is $5, say \\\"hi\\\"\\tok\"");
        e.run_command("SET n = 42");
        e.run_command("SET f = 3.5");
        e.run_command("SET arr = [1, 2, 3]");
        e.run_command("SET cfg = {\"k\": \"v\", \"z\": 7}");
        e.run_command("SAVEVARS vars.txt");
        // Wipe the globals, then prove LoadVars brings them all back verbatim.
        e.run_command("CLEAR greeting n f arr cfg");
        assert!(e.breakpoint_vars().is_empty(), "cleared before reload");
        e.run_command("LOADVARS vars.txt");
        let m: HashMap<String, String> = e.breakpoint_vars().into_iter().collect();
        assert_eq!(m.get("GREETING").map(String::as_str), Some("cost is $5, say \"hi\"\tok"));
        assert_eq!(m.get("N").map(String::as_str), Some("42"));
        assert_eq!(m.get("F").map(String::as_str), Some("3.5"));
        assert_eq!(m.get("ARR").map(String::as_str), Some("[1, 2, 3]"));
        assert_eq!(m.get("CFG").map(String::as_str), Some("{k=v, z=7}"));
    }

    #[test]
    fn listvars_lists_names_and_values_sorted() {
        let mut e = Editor::new(typing_keymap());
        e.run_command("SET beta = 2");
        e.run_command("SET alpha = \"hi\"");
        // Capture the listing into a var so we can assert on it without touching the document.
        e.run_command("CAPTURE out LISTVARS");
        let m: std::collections::HashMap<String, String> = e.breakpoint_vars().into_iter().collect();
        let out = m.get("OUT").cloned().unwrap_or_default();
        // Sorted by name; `$name: value` per line (OUT itself is empty at capture time).
        assert!(out.contains("$ALPHA: hi"), "listing: {out:?}");
        assert!(out.contains("$BETA: 2"), "listing: {out:?}");
        assert!(out.find("$ALPHA").unwrap() < out.find("$BETA").unwrap(), "sorted");
    }

    #[test]
    fn loadvars_missing_file_sets_error_and_does_not_halt() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.run_command("LOADVARS nope.txt ; SET reached = 1");
        let m: std::collections::HashMap<String, String> = e.breakpoint_vars().into_iter().collect();
        // Missing file is recoverable: $ERROR set, but the chained command still ran.
        assert_eq!(m.get("REACHED").map(String::as_str), Some("1"));
    }

    #[test]
    fn reopen_n_reopens_the_first_n_immediately() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("a.txt".into(), "hello world".into());
        e.host_mut().files.insert("b.txt".into(), "second".into());
        e.run_command("OPEN a.txt");
        e.run_command("AB");
        e.run_command("OPEN b.txt");
        e.run_command("JMP 3");
        e.run_command("AB"); // b.txt is the most-recent close, caret 3

        // `REOPEN 1` (replaces the retired `REOPEN1`): no dialog, reopens b.txt with the caret.
        e.run_command("REOPEN 1");
        assert!(e.pick_request().is_none(), "REOPEN 1 opens no chooser");
        assert_eq!(e.document().text(), "second");
        assert_eq!(e.document().cursor(), 3);
        // b.txt consumed; a.txt remains in the closed list.
        assert_eq!(e.host_mut().recently_closed().len(), 1);
        assert_eq!(e.host_mut().recently_closed()[0].0.to_string_lossy(), "a.txt");
    }

    #[test]
    fn reopen_n_reopens_a_batch_most_recent_active_and_clamps() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        for (n, body) in [("a.txt", "AAA"), ("b.txt", "BBB"), ("c.txt", "CCC")] {
            e.host_mut().files.insert(n.into(), body.into());
        }
        // Close a, then b, then c → recently-closed newest-first: c, b, a.
        for n in ["a.txt", "b.txt", "c.txt"] {
            e.run_command(&format!("OPEN {n}"));
            e.run_command("AB");
        }
        assert_eq!(e.host_mut().recently_closed().len(), 3);

        // `REOPEN 5` clamps to the 3 available; the most-recent (c.txt) ends up active.
        e.run_command("REOPEN 5");
        assert!(e.pick_request().is_none());
        assert_eq!(e.document().text(), "CCC", "most-recently-closed is the active tab");
        assert!(e.host_mut().recently_closed().is_empty(), "all consumed");
        // All three reopened (plus whatever scratch) → the three files are open.
        let n_files = (0..e.buffer_count())
            .filter(|&i| e.host().doc(i).text().ends_with("AA") || e.host().doc(i).text().ends_with("BB") || e.host().doc(i).text().ends_with("CC"))
            .count();
        assert_eq!(n_files, 3, "all three reopened");

        // A non-count argument is a trappable error.
        e.run_command("REOPEN x");
        assert!(e.last_error().unwrap().contains("expected a count"));
    }

    #[test]
    fn clearclosed_empties_the_list() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("a.txt".into(), "x".into());
        e.run_command("OPEN a.txt");
        e.run_command("AB");
        assert_eq!(e.host_mut().recently_closed().len(), 1);
        e.run_command("CLEARCLOSED");
        assert!(e.host_mut().recently_closed().is_empty());
        // REOPEN <n> on an empty list is a graceful no-op (sets $ERROR, opens nothing).
        e.run_command("REOPEN 1");
        assert!(e.host_mut().recently_closed().is_empty());
    }

    #[test]
    fn deleted_file_is_not_recorded_for_reopen() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("c.txt".into(), "x".into());
        e.run_command("OPEN c.txt");
        e.run_command("ABD"); // delete from disk + close (bare ABD = no confirm)
        assert!(
            e.host_mut().recently_closed().iter().all(|(p, _)| p.to_string_lossy() != "c.txt"),
            "a deleted file must not be reopenable"
        );
    }

    #[test]
    fn unnamed_buffer_close_is_not_recorded() {
        let mut e = Editor::new(typing_keymap());
        e.run_command("INSERT \"scratch\""); // the initial unnamed buffer, now dirty-ish
        e.run_command("NEW"); // a second unnamed buffer so AB has something to fall back to
        e.run_command("AB");
        assert!(e.host_mut().recently_closed().is_empty(), "unnamed buffers aren't reopenable");
    }

    #[test]
    fn reopen_list_saveclosed_loadclosed_round_trip() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("a.txt".into(), "hello world".into());
        e.run_command("OPEN a.txt");
        e.run_command("JMP 3");
        e.run_command("AB"); // record a.txt@3
        // SaveClosed writes a runnable LoadClosed script; SAVECFG embeds the same line.
        e.run_command("SAVECLOSED reopen.xpl");
        let saved = e.host_mut().files.get("reopen.xpl").cloned().unwrap();
        assert!(saved.contains("LoadClosed \""), "runnable LoadClosed line: {saved}");
        assert!(saved.contains("3\\ta.txt"), "caret⇥path serialized: {saved}");
        e.run_command("SAVECFG cfg.cfg");
        assert!(e.host_mut().files.get("cfg.cfg").unwrap().contains("LoadClosed \""));
        // Clear, then RUN the saved script to restore the list.
        e.run_command("LOADCLOSED \"\"");
        assert!(e.host_mut().recently_closed().is_empty());
        e.run_command("RUN reopen.xpl");
        let rc = e.host_mut().recently_closed();
        assert_eq!(rc.len(), 1);
        assert_eq!(rc[0].0.to_string_lossy(), "a.txt");
        assert_eq!(rc[0].1, 3);
    }

    #[test]
    fn library_routine_invokable_as_a_command_after_loadlib() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("my.lib".into(), "SUB mygreet\nPR hello $1\nENDSUB".into());
        e.run_command("LOADLIB my.lib");
        // A SEPARATE command (compiled with the now-loaded library): invoke the routine directly,
        // no CALL verb.
        e.run_command("mygreet World");
        assert_eq!(e.output(), &["hello World".to_string()]);
    }

    #[test]
    fn dollar_paren_works_in_compiled_expression_positions() {
        // `$(…)` inline sub-expressions used to be a substitution-pass feature only, so they
        // parsed in command-argument positions but hit a lexer error in COMPILED expression
        // positions (RETURN value, SET RHS, IF condition). They now lex as a redundant paren
        // group and evaluate to the typed value.
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut()
            .files
            .insert("m.lib".into(), "SUB addone\nRETURN $(1 + 1)\nENDSUB".into());
        // RETURN value: the routine now LOADS (before the fix, `RETURN $(…)` failed to parse and
        // LOADLIB rejected the whole file), and `CALL` returns the evaluated result via `$RV`.
        e.run_command("LOADLIB m.lib");
        e.run_command("CALL addone\nPR $RV");
        assert_eq!(e.output().last().map(String::as_str), Some("2"));

        // SET right-hand side.
        e.run_command("SET x = $(3 * 4)");
        e.run_command("PR $x");
        assert_eq!(e.output().last().map(String::as_str), Some("12"));

        // IF condition.
        e.run_command("IF $(2 > 1) ; PR yes ; ELSE ; PR no ; EI");
        assert_eq!(e.output().last().map(String::as_str), Some("yes"));
    }

    #[test]
    fn sel_n_reads_other_tabs_selections() {
        let mut e = ed();
        // Tab 1: "alpha", select all.
        e.run_command("INSERT \"alpha\"");
        e.run_command("DFA");
        // Tab 2 (new): "beta", select all. Now active = tab 2.
        e.run_command("NEW");
        e.run_command("INSERT \"beta\"");
        e.run_command("DFA");

        // SEL() = current = tab 2; SEL(n) reaches any tab; SEL($CT) == SEL().
        e.run_command("PR $(SEL())");
        assert_eq!(e.output().last().map(String::as_str), Some("beta"));
        e.run_command("PR $(SEL(1))"); // the OTHER tab's selection
        assert_eq!(e.output().last().map(String::as_str), Some("alpha"));
        e.run_command("PR $(SEL(2))");
        assert_eq!(e.output().last().map(String::as_str), Some("beta"));
        e.run_command("PR $(SEL($CT))"); // active tab, via the 1-based index special
        assert_eq!(e.output().last().map(String::as_str), Some("beta"));
        e.run_command("PR $(SEL(9))"); // out of range → ""
        assert_eq!(e.output().last().map(String::as_str), Some(""));
    }

    #[test]
    fn cp_with_no_local_selection_pulls_from_another_tab() {
        let mut e = ed();
        // Tab 1: "alpha", select all.
        e.run_command("INSERT \"alpha\"");
        e.run_command("DFA");
        // Tab 2 (new, active): NO selection; caret at start.
        e.run_command("NEW");
        e.run_command("INSERT \"X\"");
        e.run_command("TOP"); // no selection in tab 2

        // CP with no local selection → offers tab 1's selection in the chooser (shares PICK).
        e.run_command("CP");
        let (title, items) = e.pick_request().expect("CP should open the cross-tab chooser");
        assert_eq!(title.as_deref(), Some("Copy selection from tab"));
        assert_eq!(items.len(), 1);
        assert!(items[0].contains("alpha"), "label previews the selection: {:?}", items[0]);

        // Choose it → tab 1's selection is inserted at tab 2's caret.
        e.pick_respond(Some(0));
        assert!(e.pick_request().is_none());
        assert_eq!(e.document().text(), "alphaX");
    }

    #[test]
    fn cp_with_no_selections_anywhere_is_a_graceful_noop() {
        let mut e = ed();
        e.run_command("INSERT \"hello\""); // a doc, but nothing selected anywhere
        e.run_command("TOP");
        e.run_command("CP");
        assert!(e.pick_request().is_none(), "no chooser when nothing is selected");
        assert_eq!(e.document().text(), "hello"); // unchanged
    }

    #[test]
    fn kill_unknown_job_is_reported() {
        let mut e = Editor::new(typing_keymap());
        e.run_command("KILL 999"); // nothing running
        assert!(e.status_message().unwrap_or_default().contains("no job 999"));
    }

    #[test]
    fn background_job_runs_off_thread_and_delivers_output() {
        let mut e = Editor::new(typing_keymap());
        e.run_command("TRUST 1");
        e.run_command("X/BG echo hi"); // launches a worker thread, returns immediately
        // Poll until the job completes (bounded; `echo` is near-instant).
        let mut waited = 0;
        while e.job_count() > 0 && waited < 400 {
            let _ = e.poll_jobs();
            std::thread::sleep(std::time::Duration::from_millis(5));
            waited += 1;
        }
        assert_eq!(e.job_count(), 0, "background job did not finish in time");
        // The completion delivered output → $XO and exit code → $RC.
        e.run_command("PR $XO");
        assert!(e.output().last().unwrap().contains("hi"));
        e.run_command("PR $RC");
        assert_eq!(e.output().last().map(String::as_str), Some("0"));
        // ...and was appended to a new *Shell Output* tab (header + output), not the active doc.
        let tabs = e.tabs();
        let shell = tabs.iter().find(|t| t.name == "*Console*").expect("Shell Output tab");
        assert!(!shell.read_only, "the console tab is now editable, not a locked log");
        // Switch to it and check its contents carry the command header + output.
        let i = tabs.iter().position(|t| t.name == "*Console*").unwrap();
        e.set_active_buffer(i);
        let text = e.document().text();
        assert!(text.contains("echo hi"));
        assert!(text.contains("hi"));
        assert!(text.contains("(exit 0)"));
        // Editable: the user can type into it, and streamed output does NOT sit on the undo stack, so
        // Ctrl+Z unwinds only the user's own edit (leaving the streamed output intact).
        let before = e.document().text();
        e.run_command("TF");
        e.run_command(r#"insert "X""#);
        assert_eq!(e.document().text(), format!("X{before}"));
        e.run_command("UNDO");
        assert_eq!(e.document().text(), before, "undo removes the user edit, keeps streamed output");
    }

    #[test]
    fn console_repl_submits_input_runs_and_reprompts() {
        let mut e = Editor::new(typing_keymap());
        e.run_command("TRUST 1");
        // The GUI wiring is 1b-ii; here drive the host REPL directly: write a prompt, type after it.
        e.console_write_prompt();
        let i = e.tabs().iter().position(|t| t.name == "*Console*").expect("console tab");
        e.set_active_buffer(i);
        assert!(e.document().text().ends_with("> "), "a prompt is shown: {:?}", e.document().text());
        // Type a command at the prompt and submit it (Enter at the tail).
        e.run_command(r#"insert "echo hi""#);
        assert!(e.console_submit(), "caret at the tail with a prompt showing → submit");
        // Poll until the foreground console job finishes.
        let mut waited = 0;
        while e.job_count() > 0 && waited < 400 {
            let _ = e.poll_jobs();
            std::thread::sleep(std::time::Duration::from_millis(5));
            waited += 1;
        }
        assert_eq!(e.job_count(), 0, "console job did not finish in time");
        let text = e.document().text();
        assert!(text.contains("echo hi"), "command echoed: {text:?}");
        assert!(text.contains("hi\n") || text.contains("hi\r"), "command output present: {text:?}");
        assert!(text.contains("(exit 0)"), "exit line present: {text:?}");
        assert!(text.ends_with("> "), "a fresh prompt follows for the next command: {text:?}");
        // Enter with the caret UP in scrollback is not a submit (the GUI inserts a newline instead).
        e.run_command("TF");
        assert!(!e.console_submit(), "editing scrollback is not a submit");
    }

    #[test]
    fn shell_output_caret_lands_at_the_end_of_the_output() {
        let mut e = Editor::new(typing_keymap());
        e.run_command("TRUST 1");
        e.run_command("X/BG echo hi");
        let mut waited = 0;
        while e.job_count() > 0 && waited < 400 {
            let _ = e.poll_jobs();
            std::thread::sleep(std::time::Duration::from_millis(5));
            waited += 1;
        }
        assert_eq!(e.job_count(), 0, "background job did not finish in time");
        // The caret rests at the very end of the *Shell Output* buffer (the newest text), so the
        // view follows the tail rather than pinning to the top of the run.
        let i = e.tabs().iter().position(|t| t.name == "*Console*").expect("Shell Output tab");
        e.set_active_buffer(i);
        let len = e.document().text().len();
        assert_eq!(e.document().cursor(), len, "caret should be at the end of the output");
    }

    #[test]
    fn sw_toggles_shell_output_visibility_reads_back_and_persists() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;

        // Default: visible.
        e.run_command("PR $SW");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));

        // No arg toggles.
        e.run_command("SW");
        e.run_command("PR $SW");
        assert_eq!(e.output().last().map(String::as_str), Some("0"));
        e.run_command("SW");
        e.run_command("PR $SW");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));

        // Explicit on/off, and the SHELLWIN synonym.
        e.run_command("SW off");
        e.run_command("PR $SW");
        assert_eq!(e.output().last().map(String::as_str), Some("0"));
        e.run_command("SHELLWIN on");
        e.run_command("PR $SW");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));

        // A bad argument is a trappable error and leaves the state unchanged.
        e.run_command("SW maybe");
        assert!(e.last_error().unwrap_or_default().contains("expected on|off"));
        e.run_command("PR $SW");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));

        // The state is captured by SAVECFG.
        e.run_command("SW off");
        e.run_command("SAVECFG cfg.xpl");
        let cfg = e.host_mut().files.get("cfg.xpl").cloned().expect("config written");
        assert!(cfg.contains("SW off"));
    }

    #[test]
    fn sw_off_hides_shell_tab_but_keeps_buffer_and_hands_off_focus() {
        let mut e = Editor::new(typing_keymap());
        e.run_command("TRUST 1");
        e.run_command("X/BG echo hi");
        let mut waited = 0;
        while e.job_count() > 0 && waited < 400 {
            let _ = e.poll_jobs();
            std::thread::sleep(std::time::Duration::from_millis(5));
            waited += 1;
        }
        assert_eq!(e.job_count(), 0, "background job did not finish in time");

        // Focus the *Shell Output* tab.
        let shell = e.tabs().iter().position(|t| t.name == "*Console*").expect("shell tab");
        e.set_active_buffer(shell);
        assert_eq!(e.active_index(), shell);

        // SW off: the buffer stays (still in tabs()) but focus moves off it, since the GUI will
        // hide that tab. $SW reports the new state.
        e.run_command("SW off");
        e.run_command("PR $SW");
        assert_eq!(e.output().last().map(String::as_str), Some("0"));
        assert!(
            e.tabs().iter().any(|t| t.name == "*Console*"),
            "the shell buffer must survive being hidden"
        );
        assert_ne!(e.active_index(), shell, "focus should leave the hidden tab");
        assert_ne!(e.tabs()[e.active_index()].name, "*Console*");
    }

    #[test]
    fn xbg_focuses_shell_tab_when_sw_on_but_not_when_off() {
        // SW on (default): launching X/BG switches focus to the *Shell Output* tab immediately,
        // so the user watches output stream live.
        let mut e = Editor::new(typing_keymap());
        e.run_command("TRUST 1");
        e.run_command("X/BG echo hi");
        assert_eq!(
            e.tabs()[e.active_index()].name, "*Console*",
            "SW on → X/BG should focus the shell tab at launch"
        );
        drain_jobs(&mut e);

        // SW off: the job still runs and the buffer is created, but focus stays on the user's
        // document — the output streams into a hidden tab.
        let mut e = Editor::new(typing_keymap());
        e.run_command("TRUST 1");
        e.run_command("SW off");
        let before = e.active_index();
        assert_ne!(e.tabs().get(before).map(|t| t.name.as_str()), Some("*Console*"));
        e.run_command("X/BG echo hi");
        assert_eq!(e.active_index(), before, "SW off → focus must stay on the document");
        assert!(
            e.tabs().iter().any(|t| t.name == "*Console*"),
            "the buffer is still created (hidden), so output is captured"
        );
        drain_jobs(&mut e);
    }

    #[test]
    fn sw_on_restores_shell_focus_only_if_it_was_focused_when_hidden() {
        // Create the *Shell Output* buffer via a real job.
        let mut e = Editor::new(typing_keymap());
        e.run_command("TRUST 1");
        e.run_command("X/BG echo hi"); // SW on (default) → focuses the shell tab at launch
        drain_jobs(&mut e);
        let shell = e.tabs().iter().position(|t| t.name == "*Console*").expect("shell tab");

        // Case A: hide WHILE looking at the shell tab → SW on brings us back to it.
        e.set_active_buffer(shell);
        assert_eq!(e.active_index(), shell);
        e.run_command("SW off");
        assert_ne!(e.active_index(), shell, "hiding moves focus off the tab");
        e.run_command("SW on");
        assert_eq!(e.active_index(), shell, "was focused when hidden → focus restored");

        // Case B: hide while on the DOCUMENT (not the shell tab) → SW on leaves focus put.
        let doc = e.tabs().iter().position(|t| t.name != "*Console*").expect("a document");
        e.set_active_buffer(doc);
        e.run_command("SW off");
        assert_eq!(e.active_index(), doc, "focus already off the tab; stays put");
        e.run_command("SW on");
        assert_eq!(e.active_index(), doc, "wasn't focused when hidden → no switch on show");
        assert_ne!(e.tabs()[e.active_index()].name, "*Console*");
    }

    #[test]
    fn sh_opens_the_console_and_shprompt_persists() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        // SH from the CMline opens + focuses the Console tab, landing the caret at its prompt (not
        // back on the CMline) — SH is in FOCUS_DOC_VERBS.
        e.host_mut().input_focus = Target::CmLine;
        e.host_mut().target = Target::CmLine;
        e.run_command("SH");
        assert_eq!(e.host_mut().target, Target::Document, "SH leaves focus in the Console, not the CMline");
        let active = e.tabs().get(e.active_index()).map(|t| t.name.clone());
        assert_eq!(active.as_deref(), Some("*Console*"), "SH focuses the Console tab");
        e.run_command("PR $SH");
        assert_eq!(e.output().last().map(String::as_str), Some("1")); // Console shown
        e.run_command("SHELL off");
        e.run_command("PR $SH");
        assert_eq!(e.output().last().map(String::as_str), Some("0"));
        // SHPROMPT sets the prompt (trailing space preserved via quotes); $SHPROMPT reads it; SaveCfg keeps it.
        e.run_command(r#"SHPROMPT "ready> ""#);
        e.run_command("PR <$SHPROMPT>");
        assert_eq!(e.output().last().map(String::as_str), Some("<ready> >"));
        e.run_command("SAVECFG cfg.xpl");
        assert!(e.host_mut().files.get("cfg.xpl").cloned().unwrap().contains("SHPROMPT ready> "));
    }

    #[test]
    fn preview_pane_toggles_reads_back_and_persists() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.run_command("PR $PV");
        assert_eq!(e.output().last().map(String::as_str), Some("0")); // off by default
        assert!(!e.preview_visible());
        e.run_command("PV"); // toggle on
        e.run_command("PR $PV");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
        assert!(e.preview_visible());
        e.run_command("PREVIEW off");
        assert_eq!(e.preview_visible(), false);
        e.run_command("PV on");
        e.run_command("SAVECFG cfg.xpl");
        assert!(e.host_mut().files.get("cfg.xpl").cloned().unwrap().contains("PV on"));
    }

    #[test]
    fn preview_render_kind_follows_the_document() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("a.md".into(), "# md\n".into());
        e.host_mut().files.insert("b.typ".into(), "= typ\n".into());
        e.run_command("open a.md");
        assert!(!e.active_render_is_typst()); // GFM by extension
        e.run_command("open b.typ");
        assert!(e.active_render_is_typst()); // Typst by extension
        e.run_command("DOCTYPE gfm");
        assert!(!e.active_render_is_typst()); // override wins
    }

    /// Type a command at the Console prompt and submit it (the headless stand-in for the GUI's Enter).
    fn console_run(e: &mut Editor, cmd: &str) {
        if e.tabs().iter().all(|t| t.name != "*Console*") || e.special_str("SH") != "1" {
            e.run_command("SH"); // ensure the Console is open + a prompt is showing + focused
        } else {
            let i = e.tabs().iter().position(|t| t.name == "*Console*").unwrap();
            e.set_active_buffer(i);
        }
        e.run_command(&format!(r#"insert "{cmd}""#));
        assert!(e.console_submit(), "submit at the prompt");
    }

    #[test]
    fn console_refuses_to_run_without_trust() {
        let mut e = ed();
        console_run(&mut e, "echo hi"); // no TRUST
        assert_eq!(e.job_count(), 0, "nothing spawned without trust");
        assert!(e.document().text().contains("shell-out is disabled"));
    }

    #[test]
    fn console_interactive_stdin_feeds_a_running_child() {
        // `sort` reads stdin until EOF, then emits the lines sorted — a clean interactive-stdin test.
        let mut e = ed();
        e.run_command("TRUST 1");
        console_run(&mut e, "sort");
        assert!(e.console_job_running(), "sort is running with its stdin open");
        // Enter now feeds the child's stdin instead of starting a new command.
        e.run_command(r#"insert "zebra""#);
        assert!(e.console_submit(), "line sent to stdin");
        e.run_command(r#"insert "apple""#);
        assert!(e.console_submit());
        // Ctrl+Z/Ctrl+D → EOF; sort now produces its sorted output.
        assert!(e.console_stdin_eof(), "EOF closes the child's stdin");
        let mut waited = 0;
        while e.job_count() > 0 && waited < 1000 {
            let _ = e.poll_jobs();
            std::thread::sleep(std::time::Duration::from_millis(5));
            waited += 1;
        }
        assert_eq!(e.job_count(), 0, "sort did not finish in time");
        let text = e.document().text();
        // The SORTED output (last occurrences) has apple before zebra, regardless of typed order.
        let a = text.rfind("apple").expect("apple in output");
        let z = text.rfind("zebra").expect("zebra in output");
        assert!(a < z, "sorted output apple<zebra: {text:?}");
    }

    #[test]
    fn console_ansi_output_becomes_mda_colour_spans() {
        let mut e = ed();
        e.console_write_prompt();
        let i = e.tabs().iter().position(|t| t.name == "*Console*").unwrap();
        e.set_active_buffer(i);
        // A red "ERROR", then default " ok" — escapes must be stripped and a red fg span cover ERROR.
        e.host_mut().console_emit_test("\x1b[31mERROR\x1b[0m ok\n");
        let text = e.document().text();
        assert!(text.contains("ERROR ok"), "text present: {text:?}");
        assert!(!text.contains('\u{1b}'), "escapes stripped: {text:?}");
        let start = text.find("ERROR").unwrap();
        let fg = e.host_mut().active_doc().color_spans_decoded();
        assert!(
            fg.iter().any(|&(s, en, c)| s == start && en == start + 5 && c == (170, 0, 0)),
            "red span over ERROR: {fg:?}"
        );
        // A split escape across two emits still colours correctly and shows no stray bytes.
        e.host_mut().console_emit_test("\x1b[32mgr");
        e.host_mut().console_emit_test("een\x1b[0m\n");
        let text2 = e.document().text();
        assert!(text2.contains("green") && !text2.contains('\u{1b}'), "green shown, escapes stripped: {text2:?}");
        let fg2 = e.host_mut().active_doc().color_spans_decoded();
        let g = text2.find("green").unwrap();
        assert!(fg2.iter().any(|&(s, en, c)| s == g && en == g + 5 && c == (0, 170, 0)), "green: {fg2:?}");
    }

    #[test]
    fn console_cls_clears_the_scrollback() {
        let mut e = ed();
        e.run_command("TRUST 1");
        console_run(&mut e, "echo one");
        drain_jobs(&mut e);
        assert!(e.document().text().contains("one"));
        // The CLS verb clears the Console and leaves a fresh prompt.
        e.run_command("CLS");
        let text = e.document().text();
        assert!(!text.contains("one"), "scrollback cleared: {text:?}");
        assert!(text.ends_with("> "), "fresh prompt after clear: {text:?}");
        // Typing `clear` at the prompt does the same, spawning no process.
        console_run(&mut e, "echo two");
        drain_jobs(&mut e);
        console_run(&mut e, "clear");
        assert!(!e.document().text().contains("two"), "clear at the prompt cleared it");
        assert_eq!(e.job_count(), 0, "clear spawned nothing");
    }

    #[test]
    fn ansicolor_toggles_persists_and_gates_colouring() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        assert_eq!(e.special_str("ANSICOLOR"), "1"); // default on
        e.run_command("ANSICOLOR off");
        assert_eq!(e.special_str("ANSICOLOR"), "0");
        // With it off, output keeps the raw escapes and gets no colour span.
        e.console_write_prompt();
        let i = e.tabs().iter().position(|t| t.name == "*Console*").unwrap();
        e.set_active_buffer(i);
        e.host_mut().console_emit_test("\x1b[31mERR\x1b[0m\n");
        assert!(e.document().text().contains('\u{1b}'), "raw escapes kept when off");
        assert!(e.host_mut().active_doc().color_spans_decoded().is_empty(), "no colour when off");
        // Bare toggles back on; a bad value errors; SaveCfg carries the setting.
        e.run_command("ANSICOLOR");
        assert_eq!(e.special_str("ANSICOLOR"), "1");
        e.run_command("ANSICOLOR maybe");
        assert!(e.last_error().is_some(), "bad value errors");
        e.run_command("ANSICOLOR off");
        e.run_command("SAVECFG cfg.xpl");
        assert!(e.host_mut().files.get("cfg.xpl").unwrap().contains("ANSICOLOR off"));
    }

    #[test]
    fn console_history_recall_walks_submitted_commands() {
        let mut e = ed();
        e.run_command("TRUST 1");
        console_run(&mut e, "echo a");
        drain_jobs(&mut e); // completes → fresh prompt
        console_run(&mut e, "echo b");
        drain_jobs(&mut e);
        // At the fresh prompt, Shift+Up recalls the newest command, then older.
        assert!(e.console_history_recall(true), "recall newest");
        assert!(e.document().text().ends_with("echo b"), "newest recalled: {:?}", e.document().text());
        assert!(e.console_history_recall(true), "recall older");
        assert!(e.document().text().ends_with("echo a"), "older recalled: {:?}", e.document().text());
        // Shift+Down walks back toward the newest, then to a fresh empty line.
        assert!(e.console_history_recall(false));
        assert!(e.document().text().ends_with("echo b"));
        assert!(e.console_history_recall(false));
        assert!(e.document().text().ends_with("> "), "past newest → empty input: {:?}", e.document().text());
        // Nothing newer than a fresh line → not handled (GUI keeps normal Shift+Down).
        assert!(!e.console_history_recall(false));
    }

    #[test]
    fn console_ctrl_c_interrupts_a_running_command() {
        // `sort` with no EOF hangs waiting for stdin — Ctrl+C (interrupt) must kill it and re-prompt.
        let mut e = ed();
        e.run_command("TRUST 1");
        console_run(&mut e, "sort");
        assert!(e.console_command_running(), "sort is running");
        assert!(e.console_interrupt(), "Ctrl+C interrupts the running command");
        let mut waited = 0;
        while e.job_count() > 0 && waited < 1000 {
            let _ = e.poll_jobs();
            std::thread::sleep(std::time::Duration::from_millis(5));
            waited += 1;
        }
        assert_eq!(e.job_count(), 0, "the interrupted command finished");
        assert!(!e.console_command_running());
        let text = e.document().text();
        assert!(text.contains("^C"), "the interrupt is shown in the transcript: {text:?}");
        assert!(text.ends_with("> "), "a fresh prompt follows: {text:?}");
    }

    #[test]
    fn console_cd_is_intercepted_and_persists_cwd() {
        let base = std::env::temp_dir().join(format!("xyscript-console-cd-{:p}", &"tag"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sub")).unwrap();

        let mut e = ed();
        e.run_command(&format!("CD {}", base.display()));
        console_run(&mut e, "cd sub"); // a builtin → intercepted, no process
        assert_eq!(e.job_count(), 0);
        e.run_command("PR $CWD");
        assert!(e.output().last().unwrap().contains("sub"), "cwd followed the cd");

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn auto_replace_loads_and_fixes_the_word_behind_the_caret() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("fix.ar".into(), "# typos\nteh=the\nbtw=by the way\n".into());
        e.run_command("LoadAR fix.ar");
        e.run_command("PR $AR");
        let ar = e.output().last().cloned().unwrap_or_default();
        assert!(ar.ends_with("fix.ar") && ar.len() > "fix.ar".len(), "$AR fully-qualified: {ar}");

        // Type a typo (caret lands right after it) and run AR → it's corrected.
        e.run_command(r#"INSERT "teh""#);
        e.run_command("AR");
        assert_eq!(e.document().text(), "the");

        // A multi-word replacement, mid-text.
        e.run_command(r#"INSERT " btw""#);
        e.run_command("AR");
        assert_eq!(e.document().text(), "the by the way");
    }

    #[test]
    fn ar_then_quoted_space_in_a_chain_inserts_the_space() {
        // The `57=AR;" "` (Shift+Space) idiom: AR fixes the word, then the bare quoted `" "` inserts
        // a space (a quoted-string command in a chain desugars to INSERT — the bug fix).
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("fix.ar".into(), "teh=the\n".into());
        e.run_command("LoadAR fix.ar");
        e.run_command(r#"INSERT "teh""#);
        e.run_command(r#"AR;" ""#); // correct, then a space
        assert_eq!(e.document().text(), "the ");
        // And with no match, the chain still inserts the space (AR is a silent no-op).
        e.run_command(r#"INSERT "zzz""#);
        e.run_command(r#"AR;" ""#);
        assert_eq!(e.document().text(), "the zzz ");
    }

    #[test]
    fn auto_replace_matches_the_typed_case_and_noops_on_miss() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("fix.ar".into(), "equ=equilibrium\n".into());
        e.run_command("LoadAR fix.ar");

        // One entry serves all three capitalisations of the key.
        e.run_command(r#"INSERT "equ""#);
        e.run_command("AR");
        assert_eq!(e.document().text(), "equilibrium");

        e.run_command(r#"INSERT " Equ""#);
        e.run_command("AR");
        assert_eq!(e.document().text(), "equilibrium Equilibrium");

        e.run_command(r#"INSERT " EQU""#);
        e.run_command("AR");
        assert_eq!(e.document().text(), "equilibrium Equilibrium EQUILIBRIUM");

        // A mixed pattern has no clear intent → the value is used exactly as written.
        e.run_command(r#"INSERT " eQu""#);
        e.run_command("AR");
        assert_eq!(e.document().text(), "equilibrium Equilibrium EQUILIBRIUM equilibrium");

        e.run_command(r#"INSERT " zzz""#); // unknown word
        e.run_command("AR");
        assert_eq!(e.document().text(), "equilibrium Equilibrium EQUILIBRIUM equilibrium zzz");
    }

    /// A deliberately-cased expansion must survive when its key is typed plainly, and an EXACT key
    /// always wins over the case-insensitive fallback.
    #[test]
    fn auto_replace_preserves_value_case_and_prefers_an_exact_key() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.host_mut()
            .files
            .insert("fix.ar".into(), "usa=United States of America\nHTML=HyperText Markup Language\n".into());
        e.run_command("LoadAR fix.ar");

        // Lower-case key, capitalised value → value reproduced verbatim.
        e.run_command(r#"INSERT "usa""#);
        e.run_command("AR");
        assert_eq!(e.document().text(), "United States of America");

        // The exact key `HTML` wins (verbatim) rather than being upper-cased by the fallback.
        e.run_command(r#"INSERT " HTML""#);
        e.run_command("AR");
        assert_eq!(e.document().text(), "United States of America HyperText Markup Language");
    }

    #[test]
    fn auto_replace_decodes_escapes_and_clears() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        // `\n` in the file value becomes a real newline (multi-line snippet).
        e.host_mut().files.insert("s.ar".into(), "sig=Line1\\nLine2\n".into());
        e.run_command("LoadAR s.ar");
        e.run_command(r#"INSERT "sig""#);
        e.run_command("AR");
        assert_eq!(e.document().text(), "Line1\nLine2");

        e.run_command("CLEARAR");
        e.run_command("PR $AR");
        assert_eq!(e.output().last().map(String::as_str), Some("")); // path cleared
    }

    #[test]
    fn loadar_treats_semicolon_lines_as_comments() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        e.host_mut()
            .files
            .insert("c.ar".into(), "; a comment\n;nope=BAD\nteh=the\n".into());
        e.run_command("LoadAR c.ar");
        e.run_command("SAVEAR out.ar"); // dumps the loaded map
        let out = e.host_mut().files.get("out.ar").cloned().unwrap();
        assert!(out.contains("teh=the")); // real pair loaded
        assert!(!out.contains("nope") && !out.contains("BAD")); // `;` line skipped, not loaded
    }

    #[test]
    fn savear_writes_a_sample_when_empty_and_round_trips_when_loaded() {
        let mut e = ed();
        e.host_mut().virtual_fs = true;
        // Empty map → a commented sample template.
        e.run_command("SAVEAR sample.ar");
        let sample = e.host_mut().files.get("sample.ar").cloned().unwrap();
        assert!(sample.contains("teh=the"));
        assert!(sample.contains("LoadAR"));

        // Loaded map → round-trips, encoding newlines back to `\n`.
        e.host_mut().files.insert("in.ar".into(), "sig=A\\nB\nteh=the\n".into());
        e.run_command("LoadAR in.ar");
        e.run_command("SAVEAR out.ar");
        let out = e.host_mut().files.get("out.ar").cloned().unwrap();
        assert!(out.contains("teh=the"));
        assert!(out.contains("sig=A\\nB")); // newline re-encoded as \n
    }

    /// Poll a test editor until its background jobs finish (bounded; test commands are near-instant).
    fn drain_jobs(e: &mut Editor) {
        let mut waited = 0;
        while e.job_count() > 0 && waited < 400 {
            let _ = e.poll_jobs();
            std::thread::sleep(std::time::Duration::from_millis(5));
            waited += 1;
        }
    }

    #[test]
    fn autosave_setting_reads_back_and_saves_named_modified() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("a.txt".into(), "aaa".into());
        // Off by default; AUTOSAVE <secs> sets it; $AU reads it; AUTOSAVE off clears it.
        assert_eq!(e.autosave_secs(), 0);
        e.run_command("AUTOSAVE 30");
        assert_eq!(e.autosave_secs(), 30);
        e.run_command("PR $AU");
        assert_eq!(e.output().last().map(String::as_str), Some("30"));
        e.run_command("AUTOSAVE off");
        assert_eq!(e.autosave_secs(), 0);
        e.run_command("AUTOSAVE bogus"); // trappable (parse error → last_error), leaves it unchanged
        assert!(e.last_error().unwrap_or_default().contains("expected seconds"));
        assert_eq!(e.autosave_secs(), 0);
        // A pass saves named+modified buffers (the GUI calls this on the timer).
        e.run_command("open a.txt");
        e.run_command(r#"insert "X""#);
        assert_eq!(e.autosave_now(), Ok(1));
        assert_eq!(e.host_mut().files.get("a.txt").map(String::as_str), Some("Xaaa"));
        assert_eq!(e.autosave_now(), Ok(0)); // nothing modified now
    }

    #[test]
    fn save_surfaces_os_write_errors_to_the_status() {
        // Real filesystem (not the virtual overlay): a path whose parent directory does not exist
        // makes the OS write fail — the error must reach the status line, not pass silently.
        let mut e = Editor::new(typing_keymap());
        e.run_command(r#"INSERT "data""#);
        let dir = std::env::temp_dir().join("xy_missing_dir_zzz_save");
        let _ = std::fs::remove_dir_all(&dir);
        e.run_command(&format!("SA {}", dir.join("out.txt").display()));
        let fb = e.command_feedback().unwrap_or_default();
        assert!(fb.contains("cannot write"), "OS write error surfaced: {fb:?}");
    }

    #[test]
    #[cfg(windows)]
    fn save_over_a_file_locked_by_another_handle_reports_the_error() {
        use std::os::windows::fs::OpenOptionsExt;
        let mut e = Editor::new(typing_keymap());
        let path = std::env::temp_dir().join(format!("xy_locked_{}.txt", std::process::id()));
        std::fs::write(&path, b"original").unwrap();
        // Hold the target open with an EXCLUSIVE share mode (0 = deny all sharing), like another app
        // (or a virus scanner / cloud client) locking the file — so it can't be replaced.
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&path)
            .expect("lock the target");
        // Force-save over it (`/NV` skips the overwrite confirm): the atomic temp+rename can't replace
        // the locked target, so the OS error MUST surface — never a silent no-op.
        e.run_command(r#"INSERT "new content""#);
        e.run_command(&format!("SA/NV {}", path.display()));
        let fb = e.command_feedback().unwrap_or_default();        assert!(fb.to_lowercase().contains("cannot"), "locked-file error surfaced: {fb:?}");
        // The original file is untouched (atomic write never mutated the locked target) and no temp
        // is left behind.
        drop(lock);
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        let mut tmp = path.clone().into_os_string();
        tmp.push(".xyscript-tmp");
        assert!(!std::path::Path::new(&tmp).exists(), "temp cleaned up");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn autosave_surfaces_write_errors_instead_of_swallowing() {
        let mut e = Editor::new(typing_keymap());
        let dir = std::env::temp_dir().join("xy_missing_dir_zzz_autosave");
        let _ = std::fs::remove_dir_all(&dir);
        // A named buffer pointed at a non-existent directory, then modified → the autosave pass
        // can't write it. The failure must be reported (Err), never swallowed to a "0 saved".
        e.run_command(&format!("NEW {}", dir.join("a.txt").display()));
        e.run_command(r#"INSERT "X""#);
        let r = e.autosave_now();
        assert!(r.as_ref().is_err(), "autosave surfaces the write error: {r:?}");
        assert!(r.unwrap_err().contains("cannot write"));
    }

    #[test]
    fn autocfg_tracks_settings_only_to_a_file() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        // Start tracking → writes immediately; $CFA reports the path; settings-only (no history).
        e.run_command("AUTOCFG mycfg.cfg");
        let first = e.host_mut().files.get("mycfg.cfg").cloned().expect("tracked file written");
        assert!(first.contains("FGCOLOR"));
        assert!(!first.contains("LoadCM"));
        e.run_command("PR $CFA");
        assert_eq!(e.output().last().map(String::as_str), Some("mycfg.cfg"));
        // A settings change → the next poll rewrites the file.
        e.run_command("FGCOLOR 1 2 3");
        e.poll_config_track();
        assert!(e.host_mut().files.get("mycfg.cfg").unwrap().contains("FGCOLOR 1 2 3"));
        // A command-history change does NOT churn the tracked file (settings-only).
        e.host_mut().push_history("open x");
        let before = e.host_mut().files.get("mycfg.cfg").cloned().unwrap();
        e.poll_config_track();
        assert_eq!(e.host_mut().files.get("mycfg.cfg").cloned().unwrap(), before);
        // ...but a manual SAVECFG still includes history.
        e.run_command("SAVECFG manual.cfg");
        assert!(e.host_mut().files.get("manual.cfg").unwrap().contains("LoadCM"));
        // Stop: $CFA empties and further changes don't rewrite.
        e.run_command("AUTOCFG off\nPR $CFA");
        assert_eq!(e.output().last().map(String::as_str), Some(""));
        let frozen = e.host_mut().files.get("mycfg.cfg").cloned().unwrap();
        e.run_command("FGCOLOR 9 9 9");
        e.poll_config_track();
        assert_eq!(e.host_mut().files.get("mycfg.cfg").cloned().unwrap(), frozen);
    }

    #[test]
    fn cf_special_reports_the_last_loadcfg_path() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.run_command("PR $CF"); // none loaded yet
        assert_eq!(e.output().last().map(String::as_str), Some(""));
        e.run_command("SAVECFG cfg1.cfg");
        e.run_command("LOADCFG cfg1.cfg");
        e.run_command("PR $CF");
        assert_eq!(e.output().last().map(String::as_str), Some("cfg1.cfg"));
    }

    #[test]
    fn savecfg_loadcfg_round_trips_appearance() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        // A distinctive appearance: editor fg, zoom, line numbers + visible ¶.
        e.run_command("FGCOLOR 10 20 30");
        e.run_command("UISCALE 1.5");
        e.run_command("SLN");
        e.run_command("SVR");
        e.run_command("AUTOSAVE 45");
        e.run_command("CRCOLOR 40 120 255");
        e.run_command("CRSHAPE block");
        e.run_command("SAVECFG"); // → default xyscript.cfg
        assert!(e.host_mut().files.contains_key("xyscript.cfg"));
        // Change everything.
        e.run_command("FGCOLOR 0 0 0");
        e.run_command("UISCALE 1");
        e.run_command("CLN");
        e.run_command("CVR");
        e.run_command("AUTOSAVE off");
        e.run_command("CRCOLOR 0 0 0");
        e.run_command("CRSHAPE bar");
        assert_eq!(e.fg_color(), (0, 0, 0));
        assert!(!e.line_numbers());
        assert_eq!(e.autosave_secs(), 0);
        // Reload → the saved appearance is restored (LOADCFG runs the script via post_run).
        e.run_command("LOADCFG");
        assert_eq!(e.fg_color(), (10, 20, 30));
        assert_eq!(e.ui_scale(), 1.5);
        assert!(e.line_numbers());
        assert!(e.visible_returns());
        assert_eq!(e.autosave_secs(), 45);
        assert_eq!(e.caret_color(), (40, 120, 255));
        assert_eq!(e.caret_shape(), crate::host::CaretShape::Block);
    }

    #[test]
    fn tabs_reflect_buffers_and_set_active_switches() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("a.txt".into(), "aaa".into());
        e.host_mut().files.insert("b.txt".into(), "bbb".into());
        e.run_command("open a.txt"); // reuses initial → buffer 0
        e.run_command("open b.txt"); // → buffer 1 (active)
        let tabs = e.tabs();
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0].name, "a.txt");
        assert_eq!(tabs[1].name, "b.txt");
        assert!(!tabs[1].modified);
        assert_eq!(e.active_index(), 1);
        // Edit b.txt → its tab shows modified.
        e.run_command(r#"insert "X""#);
        assert!(e.tabs()[1].modified);
        // Click tab 0 → switches active, focuses the document.
        e.set_active_buffer(0);
        assert_eq!(e.active_index(), 0);
        assert_eq!(e.active_file_name().as_deref(), Some("a.txt"));
        // Out-of-range index is ignored.
        e.set_active_buffer(99);
        assert_eq!(e.active_index(), 0);
    }

    #[test]
    fn ab_closes_the_active_buffer_and_never_leaves_zero() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("a.txt".into(), "aaa".into());
        e.host_mut().files.insert("b.txt".into(), "bbb".into());
        e.run_command("open a.txt"); // reuses the empty initial buffer → 1
        e.run_command("open b.txt"); // → 2 buffers, active = b.txt
        assert_eq!(e.host_mut().buffer_count(), 2);
        // AB closes the active (b.txt), discarding it → back to a.txt.
        e.run_command("AB");
        assert_eq!(e.host_mut().buffer_count(), 1);
        assert_eq!(e.active_file_name().as_deref(), Some("a.txt"));
        // AB on the last buffer leaves one fresh, empty, untitled buffer (never zero).
        e.run_command("AB");
        assert_eq!(e.host_mut().buffer_count(), 1);
        assert_eq!(e.active_file_name(), None);
        assert_eq!(e.document().text(), "");
    }

    #[test]
    fn sl_saves_named_skips_unnamed_and_sf_closes_clean_keeping_unsaved() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("a.txt".into(), "aaa".into());
        e.run_command("open a.txt"); // 1 buffer (a.txt), unmodified
        e.run_command(r#"insert "X""#); // a.txt modified → "Xaaa"
        e.run_command("NEW"); // 2nd buffer: untitled
        e.run_command(r#"insert "draft""#); // untitled, modified
        assert_eq!(e.host_mut().buffer_count(), 2);
        // SL: saves the named+modified buffer, skips the unnamed one (kept open).
        e.run_command("SL");
        assert_eq!(e.host_mut().files.get("a.txt").map(String::as_str), Some("Xaaa"));
        assert!(e.status_message().unwrap_or_default().contains("saved 1"));
        assert!(e.status_message().unwrap_or_default().contains("skipped 1"));
        assert_eq!(e.host_mut().buffer_count(), 2); // SL doesn't close anything

        // SF on a fresh scenario: save+close all; the unnamed-modified buffer stays open.
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("a.txt".into(), "aaa".into());
        e.run_command("open a.txt");
        e.run_command(r#"insert "X""#); // a.txt modified
        e.run_command("NEW");
        e.run_command(r#"insert "draft""#); // untitled modified
        e.run_command("SF");
        assert_eq!(e.host_mut().files.get("a.txt").map(String::as_str), Some("Xaaa")); // saved
        assert_eq!(e.host_mut().buffer_count(), 1); // a.txt closed; untitled kept
        assert_eq!(e.active_file_name(), None);
        assert_eq!(e.document().text(), "draft"); // the kept, still-unsaved buffer
    }

    #[test]
    fn ro_locks_edits_unlocks_and_reads_ro_special() {
        let mut e = ed();
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        e.run_command(r#"insert "abc""#);
        assert!(!e.read_only());
        e.run_command("PR $RO");
        assert_eq!(out(&e).as_deref(), Some("0"));
        // Lock it (bare RO toggles).
        e.run_command("RO");
        assert!(e.read_only());
        e.run_command("PR $RO");
        assert_eq!(out(&e).as_deref(), Some("1"));
        // Edits are rejected (text unchanged) and the attempt is trappable feedback.
        e.run_command(r#"insert "X""#);
        assert_eq!(e.document().text(), "abc");
        assert!(e.status_message().unwrap_or_default().contains("read-only"));
        // Undo is a mutation too → blocked while locked.
        e.run_command("UNDO");
        assert_eq!(e.document().text(), "abc");
        // Reads / motion still work while locked.
        e.run_command("TOP");
        e.run_command("PR $CH");
        assert_eq!(out(&e).as_deref(), Some("a"));
        // Explicit unlock restores editing.
        e.run_command("RO off");
        assert!(!e.read_only());
        e.run_command(r#"insert "Y""#); // caret at top after TOP
        assert_eq!(e.document().text(), "Yabc");
    }

    #[test]
    fn saas_sad_and_rec_file_lifecycle() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("notes.txt".into(), "hi".into());

        // SAD: save the selection to a new file, byte-exact; the doc is untouched.
        e.run_command("open notes.txt"); // "hi", caret at top
        e.run_command("BOTTOM SEL"); // select "hi"
        e.run_command("SAD sel.txt");
        assert_eq!(e.host_mut().files.get("sel.txt").map(String::as_str), Some("hi"));
        assert_eq!(e.document().text(), "hi"); // unchanged
        assert_eq!(e.active_path().unwrap().to_string_lossy(), "notes.txt"); // path unchanged

        // SAAS <path>: save-as to a new path and adopt it.
        e.run_command("SAAS copy.txt");
        assert_eq!(e.host_mut().files.get("copy.txt").map(String::as_str), Some("hi"));
        assert_eq!(e.active_path().unwrap().to_string_lossy(), "copy.txt"); // adopted

        // SAAS with no path: requests the GUI Save-As dialog (no write here).
        assert!(!e.take_save_as_request());
        e.run_command("SAAS");
        assert!(e.take_save_as_request());
        assert!(!e.take_save_as_request()); // consumed once

        // REC: revert the active doc to its last saved state, discarding edits.
        e.run_command("open notes.txt");
        e.run_command(r#"insert "X""#); // now "Xhi", modified
        e.run_command("SET m = $MO\nPR $m");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
        e.run_command("REC");
        assert_eq!(e.document().text(), "hi"); // edits gone
        e.run_command("SET m = $MO\nPR $m");
        assert_eq!(e.output().last().map(String::as_str), Some("0")); // clean again
    }

    #[test]
    fn saas_confirms_overwrite_unless_nv() {
        use crate::host::CloseAnswer;
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("other.txt".into(), "OLD".into());

        // A fresh untitled doc with content.
        e.run_command(r#"insert "NEW""#);
        // SAAS onto an EXISTING different file arms an overwrite confirm; "no" leaves it as-is and
        // does NOT adopt the path.
        e.run_command("SAAS other.txt");
        assert!(e.close_prompt().unwrap().contains("Overwrite"), "confirm armed");
        e.resolve_close(CloseAnswer::No);
        assert_eq!(e.host_mut().files.get("other.txt").map(String::as_str), Some("OLD"));
        assert!(e.active_path().map(|p| p.to_string_lossy().into_owned()).as_deref() != Some("other.txt"));

        // "yes" overwrites AND adopts the path (the tab stays open on it).
        e.run_command("SAAS other.txt");
        assert!(e.close_prompt().is_some());
        e.resolve_close(CloseAnswer::Yes);
        assert_eq!(e.host_mut().files.get("other.txt").map(String::as_str), Some("NEW"));
        assert_eq!(e.active_path().unwrap().to_string_lossy(), "other.txt"); // adopted

        // SAAS to the doc's OWN path is a plain save — no prompt.
        e.run_command(r#"insert "!""#);
        e.run_command("SAAS other.txt");
        assert!(e.close_prompt().is_none(), "saving-as to own path must not prompt");
        assert_eq!(e.host_mut().files.get("other.txt").map(String::as_str), Some("NEW!"));

        // SAAS/NV overwrites a different existing file silently.
        e.host_mut().files.insert("third.txt".into(), "Z".into());
        e.run_command("SAAS/NV third.txt");
        assert!(e.close_prompt().is_none());
        assert_eq!(e.host_mut().files.get("third.txt").map(String::as_str), Some("NEW!"));
        assert_eq!(e.active_path().unwrap().to_string_lossy(), "third.txt"); // adopted
    }

    #[test]
    fn sad_confirms_overwrite_unless_nv() {
        use crate::host::CloseAnswer;
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("sel.txt".into(), "OLD".into());

        e.run_command(r#"insert "abc""#);
        e.run_command("TOP\nBOTTOM SEL"); // select "abc"
        // SAD onto an EXISTING file arms a confirm; "no" leaves it untouched.
        e.run_command("SAD sel.txt");
        assert!(e.close_prompt().unwrap().contains("Overwrite"), "confirm armed");
        e.resolve_close(CloseAnswer::No);
        assert_eq!(e.host_mut().files.get("sel.txt").map(String::as_str), Some("OLD"));

        // "yes" writes the selection captured when armed.
        e.run_command("SAD sel.txt");
        e.resolve_close(CloseAnswer::Yes);
        assert_eq!(e.host_mut().files.get("sel.txt").map(String::as_str), Some("abc"));

        // SAD/NV overwrites silently.
        e.host_mut().files.insert("s2.txt".into(), "OLD".into());
        e.run_command("SAD/NV s2.txt");
        assert!(e.close_prompt().is_none());
        assert_eq!(e.host_mut().files.get("s2.txt").map(String::as_str), Some("abc"));
    }

    #[test]
    fn sad_without_selection_is_trappable() {
        let mut e = Editor::new(typing_keymap());
        e.run_command(r#"insert "abc""#); // no selection
        e.run_command("SAD out.txt");
        assert!(e.status_message().unwrap_or_default().contains("no text is selected"));
    }

    #[test]
    fn rec_without_a_path_is_trappable() {
        let mut e = Editor::new(typing_keymap());
        e.run_command(r#"insert "draft""#); // untitled, never saved
        e.run_command("REC");
        assert!(e.status_message().unwrap_or_default().contains("never saved"));
        assert_eq!(e.document().text(), "draft"); // unchanged
    }

    #[test]
    fn save_without_a_name_is_trappable() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        // A NEW (unnamed) document with content, then SAVE -> $ERROR, no crash.
        e.run_command("insert \"draft\"\nsave\nIF $ERROR PR no-name");
        assert!(e.output().contains(&"no-name".to_string()));
    }

    #[test]
    fn open_absolute_windows_path_loads() {
        // Prove a backslash drive path survives splitter -> subst -> OPEN intact.
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert(r"d:\path\file.txt".into(), "hello".into());
        e.run_command(r"open d:\path\file.txt");
        assert_eq!(e.document().text(), "hello"); // existing file loaded
        assert!(e.last_error().is_none());
    }

    #[test]
    fn open_nonexistent_creates_a_new_named_buffer() {
        // Standard editor behavior: opening a path that doesn't exist yet starts
        // a new, empty buffer named for it (ready to SAVE), not an error.
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.run_command("open draft.txt");
        assert!(e.last_error().is_none());
        assert_eq!(e.active_file_name().as_deref(), Some("draft.txt"));
        assert_eq!(e.document().text(), ""); // empty, ready to edit
        // Not modified until you type.
        e.run_command("SET m = $MO\nPR $m");
        assert_eq!(e.output(), &["0".to_string()]);
    }

    #[test]
    fn saas_relative_path_resolves_against_cwd() {
        // Real filesystem: a relative SAAS name must land in the editor's $CWD, not the process cwd.
        let base = std::env::temp_dir().join(format!("xyscript_saas_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let mut e = ed(); // real fs (virtual_fs = false)
        // Forward slashes avoid the command splitter's backslash-unescaping on a Windows path.
        e.run_command(&format!("CD {}", base.to_string_lossy().replace('\\', "/")));
        e.run_command(r#"insert "hello""#);
        e.run_command("SAAS note.txt"); // relative name

        let target = base.join("note.txt");
        assert!(target.exists(), "SAAS should write to $CWD; nothing at {target:?}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
        assert!(e.active_path().unwrap().to_string_lossy().contains("note.txt")); // adopted the resolved path

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn topdf_verb_exports_a_pdf() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("doc.md".into(), "# Title\n\nSome **text**.\n".into());
        e.run_command("open doc.md");

        // Explicit path.
        e.run_command("TOPDF out.pdf");
        assert!(e.last_error().is_none(), "err: {:?}", e.last_error());
        let pdf = e.host_mut().files.get("out.pdf").cloned().unwrap_or_default();
        assert!(pdf.starts_with("%PDF-"), "out.pdf is not a PDF");

        // No path -> derived from the document name (doc.md -> doc.pdf).
        e.run_command("TOPDF");
        assert!(e.host_mut().files.contains_key("doc.pdf"), "derived path not written");
    }

    #[test]
    fn doctype_routes_topdf_and_reads_back() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        // A scratch buffer (no path) defaults to gfm; DOCTYPE forces, auto restores extension-driven.
        e.run_command("PR $DOCTYPE");
        assert_eq!(e.output().last().map(String::as_str), Some("gfm"));
        e.run_command("DOCTYPE typst\nPR $DOCTYPE");
        assert_eq!(e.output().last().map(String::as_str), Some("typst"));
        e.run_command("DOCTYPE auto\nPR $DOCTYPE");
        assert_eq!(e.output().last().map(String::as_str), Some("gfm"));

        // Routing proof: `#nope_fn()` is a typst compile error but harmless escaped text as GFM.
        e.run_command(r##"insert "#nope_fn()""##);
        e.run_command("DOCTYPE gfm\nTOPDF out.pdf");
        assert!(e.host_mut().files.contains_key("out.pdf"), "GFM should escape # and write a PDF");
        e.run_command("DOCTYPE typst\nTOPDF out2.pdf");
        assert!(!e.host_mut().files.contains_key("out2.pdf"), "typst compile error → nothing written");
    }

    #[test]
    fn doctype_html_renders_and_routes_by_extension() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        // `DOCTYPE html` is accepted and reads back.
        e.run_command("DOCTYPE html\nPR $DOCTYPE");
        assert_eq!(e.output().last().map(String::as_str), Some("html"));
        // An unknown kind is rejected and changes nothing.
        e.run_command("DOCTYPE nonsense");
        e.run_command("PR $DOCTYPE");
        assert_eq!(e.output().last().map(String::as_str), Some("html"), "a rejected kind changes nothing");

        // Routing proof: markup that is HTML *structure* renders as a heading, not as literal text.
        // (`#` in the text would also be a typst compile error, so this proves the HTML path ran.)
        e.run_command("DOCTYPE html");
        e.run_command(r##"insert "<h1>Title # here</h1><p>Body</p>""##);
        e.run_command("TOPDF page.pdf");
        assert!(e.host_mut().files.contains_key("page.pdf"), "HTML should convert and write a PDF");
    }

    #[test]
    fn html_extension_auto_routes_to_the_html_renderer() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.run_command("NEW page.html");
        e.run_command("PR $DOCTYPE");
        assert_eq!(
            e.output().last().map(String::as_str),
            Some("html"),
            "a .html file should auto-route to the HTML renderer"
        );
        // `auto` is still the state — an explicit override then wins, and clearing returns to html.
        e.run_command("DOCTYPE gfm\nPR $DOCTYPE");
        assert_eq!(e.output().last().map(String::as_str), Some("gfm"));
        e.run_command("DOCTYPE auto\nPR $DOCTYPE");
        assert_eq!(e.output().last().map(String::as_str), Some("html"));
    }

    #[test]
    fn highlight_verb_toggles_markdown_tokens() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("doc.md".into(), "# Title\n".into());
        e.run_command("open doc.md");
        assert!(e.highlights().is_empty()); // quiet screen by default
        assert_eq!(e.highlighter_name(), None);

        e.run_command("HIGHLIGHT on");
        assert!(!e.highlights().is_empty()); // tokens now present
        assert_eq!(e.highlighter_name(), Some("markdown"));

        e.run_command("HIGHLIGHT off");
        assert!(e.highlights().is_empty());
        assert_eq!(e.highlighter_name(), None);
    }

    #[test]
    fn regex_grammar_drives_highlighting_by_extension() {
        use crate::highlight::TokenKind;
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("code.ml".into(), "let x = 42\n".into());
        // Define a grammar (as a grammar script would), then open the file and highlight it.
        e.run_command("HLLANG mylang .ml");
        e.run_command(r#"HLRULE keyword "\blet\b""#);
        e.run_command(r#"HLRULE number "\b\d+\b""#);
        e.run_command("open code.ml");
        assert!(e.highlights().is_empty()); // off by default
        e.run_command("HIGHLIGHT on");
        assert_eq!(e.highlighter_name(), Some("mylang")); // grammar wins over Markdown
        let kinds: Vec<&TokenKind> = e.highlights().iter().map(|t| &t.kind).collect();
        assert!(kinds.contains(&&TokenKind::Keyword)); // "let"
        assert!(kinds.contains(&&TokenKind::Number)); // "42"
        // A bad regex in a rule is trappable (no panic; sets the $ERROR flag), and SAVECFG round-trips.
        e.run_command("HLRULE bad (unclosed");
        e.run_command("PR $ERROR");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
        e.run_command("SAVECFG cfg.cfg");
        let cfg = e.host_mut().files.get("cfg.cfg").unwrap().clone();
        assert!(cfg.contains("HLLANG mylang .ml"), "cfg: {cfg}");
        assert!(cfg.contains(r#"HLRULE keyword "\blet\b""#), "cfg: {cfg}");
    }

    #[test]
    fn grammar_string_rule_with_embedded_quotes_tokenizes() {
        use crate::highlight::TokenKind;
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("s.rs".into(), "let s = \"hi\";\n".into());
        // The exact quoting shipped in grammars/rust.xpl: outer HLRULE quotes, escaped inner quotes.
        e.run_command("HLLANG rust .rs");
        e.run_command(r#"HLRULE string "\"[^\"]*\"""#);
        e.run_command(r#"HLRULE keyword "\blet\b""#);
        e.run_command("open s.rs");
        e.run_command("HIGHLIGHT on");
        let strs: Vec<&crate::highlight::Token> =
            e.highlights().iter().filter(|t| t.kind == TokenKind::StringLit).collect();
        assert_eq!(strs.len(), 1, "tokens: {:?}", e.highlights());
        assert_eq!(&e.document().text()[strs[0].start..strs[0].end], "\"hi\"");
    }

    #[test]
    fn hlauto_enables_highlighting_on_open_for_recognized_types() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("a.rs".into(), "let x = 1\n".into());
        e.host_mut().files.insert("b.md".into(), "# Title\n".into());
        e.host_mut().files.insert("c.txt".into(), "plain\n".into());
        e.run_command("HLLANG rust .rs");
        e.run_command(r#"HLRULE keyword "\blet\b""#);
        // Default: quiet screen even on open.
        e.run_command("open a.rs");
        assert_eq!(e.highlighter_name(), None);
        // HLAUTO on → recognized types auto-highlight on open (no manual HIGHLIGHT on).
        e.run_command("HLAUTO on");
        e.run_command("PR $HLAUTO");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
        // Turning it on applied to the already-open .rs immediately.
        assert_eq!(e.highlighter_name(), Some("rust"));
        // Opening a Markdown file auto-selects Markdown; a .txt stays quiet.
        e.run_command("open b.md");
        assert_eq!(e.highlighter_name(), Some("markdown"));
        e.run_command("open c.txt");
        assert_eq!(e.highlighter_name(), None);
        // Opening another grammar file auto-binds the grammar (autoload on open). `a.rs` is already
        // open, so OPEN now prompts — confirm re-open with (Y)es.
        e.run_command("open a.rs");
        e.resolve_open(crate::host::OpenAnswer::Yes);
        assert_eq!(e.highlighter_name(), Some("rust"));
        assert!(!e.highlights().is_empty());
        // SAVECFG persists the HLAUTO preference.
        e.run_command("SAVECFG cfg.cfg");
        assert!(e.host_mut().files.get("cfg.cfg").unwrap().contains("HLAUTO on"));
    }

    #[test]
    fn shipped_grammars_load_and_tokenize() {
        use crate::highlight::TokenKind;
        // (grammar source, grammar name, sample file name, sample text). include_str! pulls the
        // ACTUAL shipped files, so this guards their regex syntax + HLRULE quoting.
        let cases: &[(&str, &str, &str, &str)] = &[
            (include_str!("../../grammars/rust.xpl"), "rust", "x.rs", "fn main() { let s = \"hi\"; } // c\n"),
            (include_str!("../../grammars/python.xpl"), "python", "x.py", "def f(): return \"x\"  # c\n"),
            (include_str!("../../grammars/javascript.xpl"), "javascript", "x.js", "function f() { return 'x' } // c\n"),
            (include_str!("../../grammars/toml.xpl"), "toml", "x.toml", "[sec]\nkey = true # c\n"),
            (include_str!("../../grammars/shell.xpl"), "shell", "x.sh", "if true; then echo hi; fi # c\n"),
            (include_str!("../../grammars/xpl.xpl"), "xpl", "x.xpl", "IF $x  # check\n  PR \"hi\"\n"),
            (include_str!("../../grammars/typst.xpl"), "typst", "x.typ", "#let s = \"hi\" // c\n"),
            (include_str!("../../grammars/html.xpl"), "html", "x.html", "<!-- c --><a href=\"x\">hi</a>\n"),
        ];
        for (src, name, file, sample) in cases {
            let mut e = Editor::new(typing_keymap());
            e.host_mut().virtual_fs = true;
            e.host_mut().files.insert((*file).into(), (*sample).into());
            e.run_command(src); // define the grammar from the shipped file's text
            assert!(e.last_error().is_none(), "{name}: grammar failed to load: {:?}", e.last_error());
            e.run_command(&format!("open {file}"));
            e.run_command("HIGHLIGHT on");
            assert_eq!(e.highlighter_name(), Some(*name), "{name}: wrong highlighter");
            let kinds: Vec<&TokenKind> = e.highlights().iter().map(|t| &t.kind).collect();
            assert!(kinds.contains(&&TokenKind::Keyword), "{name}: no keyword token in {:?}", e.highlights());
            assert!(kinds.contains(&&TokenKind::Comment), "{name}: no comment token in {:?}", e.highlights());
        }
    }

    #[test]
    fn xpl_grammar_highlights_its_own_constructs() {
        use crate::highlight::TokenKind;
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut()
            .files
            .insert("s.xpl".into(), ":loop\nRUN/BG \"script\"  # go\nSET $x @A\n".into());
        e.run_command(include_str!("../../grammars/xpl.xpl"));
        e.run_command("open s.xpl");
        e.run_command("HIGHLIGHT on");
        let kinds: Vec<&TokenKind> = e.highlights().iter().map(|t| &t.kind).collect();
        // label → heading, switch → rule, $var/@binder → link, plus keyword/string/comment.
        for want in [
            TokenKind::Heading, // :loop
            TokenKind::Rule,    // /BG
            TokenKind::Link,    // $x and @A
            TokenKind::Keyword, // SET
            TokenKind::StringLit, // "script"
            TokenKind::Comment, // # go
        ] {
            assert!(kinds.contains(&&want), "missing {want:?} in {:?}", e.highlights());
        }
    }

    #[test]
    fn typst_grammar_highlights_its_own_constructs() {
        use crate::highlight::TokenKind;
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert(
            "d.typ".into(),
            "= Heading\n#let x = 42  // note\nbody *bold* and $a^2$ with `raw`\n".into(),
        );
        e.run_command(include_str!("../../grammars/typst.xpl"));
        e.run_command("open d.typ");
        e.run_command("HIGHLIGHT on");
        let kinds: Vec<&TokenKind> = e.highlights().iter().map(|t| &t.kind).collect();
        // heading line, #-code keyword, number, // comment, *strong*, `raw`, and the custom $math$.
        for want in [
            TokenKind::Heading,
            TokenKind::Keyword,
            TokenKind::Number,
            TokenKind::Comment,
            TokenKind::Strong,
            TokenKind::Code,
            TokenKind::Other("math".into()),
        ] {
            assert!(kinds.contains(&&want), "missing {want:?} in {:?}", e.highlights());
        }
    }

    // ---- docs/reference.typ: kept honest against the code ---------------------
    //
    // The reference is authored in Typst (#cmd/#func/#sv markers double as a machine-checkable index;
    // #ex examples are runnable). These tests guarantee it compiles, documents only real items, runs
    // its examples truthfully, and (when complete) omits nothing.

    const REFERENCE_TYP: &str = include_str!("../../docs/reference.typ");

    /// Extract the string arguments of every `marker(...)` call (e.g. `#cmd(`), one Vec per call.
    /// Parses quoted args (with `\"`/`\\` escapes), so parens inside an arg (like `$( )` in an `#ex`
    /// example) don't end the call early.
    fn marker_calls(src: &str, marker: &str) -> Vec<Vec<String>> {
        let b = src.as_bytes();
        let mut out = Vec::new();
        let mut search = 0;
        while let Some(rel) = src[search..].find(marker) {
            let mut j = search + rel + marker.len();
            let mut args = Vec::new();
            loop {
                while j < b.len() && (b[j] as char).is_whitespace() {
                    j += 1;
                }
                if j >= b.len() || b[j] != b'"' {
                    break;
                }
                j += 1; // opening quote
                let mut s = String::new();
                while j < b.len() && b[j] != b'"' {
                    if b[j] == b'\\' && j + 1 < b.len() {
                        j += 1;
                    }
                    s.push(b[j] as char);
                    j += 1;
                }
                j += 1; // closing quote
                args.push(s);
                while j < b.len() && (b[j] as char).is_whitespace() {
                    j += 1;
                }
                if j < b.len() && b[j] == b',' {
                    j += 1;
                    continue;
                }
                break;
            }
            out.push(args);
            search = j;
        }
        out
    }

    /// The reference source with full-line `//` comments stripped (so the marker examples in the
    /// authoring-conventions header aren't mistaken for real entries).
    fn reference_body() -> String {
        REFERENCE_TYP
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn documented(marker: &str) -> std::collections::HashSet<String> {
        marker_calls(&reference_body(), marker).into_iter().flatten().collect()
    }

    /// The reference body between the top-level heading line `start` and the next `end` (both matched
    /// with surrounding newlines so `= Commands` doesn't also match `= Commands by topic`). Used to
    /// check that a SINGLE listing is complete on its own.
    fn reference_section(start: &str, end: &str) -> String {
        let body = format!("\n{}\n", reference_body()); // pad so first/last headings have newlines
        let s = body.find(&format!("\n{start}\n")).map(|i| i + start.len() + 2).unwrap_or(0);
        let rest = &body[s..];
        let e = rest.find(&format!("\n{end}\n")).unwrap_or(rest.len());
        rest[..e].to_string()
    }

    fn documented_in(section: &str, marker: &str) -> std::collections::HashSet<String> {
        marker_calls(section, marker).into_iter().flatten().collect()
    }

    /// Like `documented`, but only the FIRST arg of each call — for `#func("NAME", "sig")`, where the
    /// second arg is the rendered signature, not a function name.
    fn documented_first(marker: &str) -> std::collections::HashSet<String> {
        marker_calls(&reference_body(), marker)
            .into_iter()
            .filter_map(|mut v| (!v.is_empty()).then(|| v.swap_remove(0)))
            .collect()
    }

    #[test]
    fn reference_typ_compiles() {
        let pdf = crate::typst_to_pdf(REFERENCE_TYP, std::path::Path::new("."), None)
            .expect("reference.typ must compile as Typst");
        assert!(pdf.starts_with(b"%PDF-"));
    }

    #[test]
    fn manual_typ_compiles() {
        const MANUAL_TYP: &str = include_str!("../../docs/manual.typ");
        let pdf = crate::typst_to_pdf(MANUAL_TYP, std::path::Path::new("."), None)
            .expect("manual.typ must compile as Typst");
        assert!(pdf.starts_with(b"%PDF-"));
    }

    #[test]
    fn reference_documents_only_real_items() {
        use xyscript_xpl::{BUILTIN_VERBS, CONTROL_KEYWORDS, FUNCTION_NAMES, SPECIAL_NAMES};
        let verbs: std::collections::HashSet<&str> =
            BUILTIN_VERBS.iter().chain(CONTROL_KEYWORDS).copied().collect();
        for n in documented("#cmd(") {
            assert!(verbs.contains(n.as_str()), "reference documents nonexistent command {n:?}");
        }
        for n in documented_first("#func(") {
            assert!(FUNCTION_NAMES.contains(&n.as_str()), "reference documents nonexistent function {n:?}");
        }
        for n in documented("#sv(") {
            assert!(SPECIAL_NAMES.contains(&n.as_str()), "reference documents nonexistent special var {n:?}");
        }
    }

    #[test]
    fn reference_examples_match_actual_output() {
        for pair in marker_calls(&reference_body(), "#ex(") {
            assert_eq!(pair.len(), 2, "#ex needs (code, expected-output): {pair:?}");
            let (code, expected) = (&pair[0], &pair[1]);
            let mut e = ed();
            e.run_command("TRUST 1"); // some examples may need it; harmless otherwise
            e.run_command(code);
            let got = e.output().last().cloned().unwrap_or_default();
            assert_eq!(&got, expected, "example `{code}` printed {got:?}, doc says {expected:?}");
        }
    }

    #[test]
    fn reference_documents_every_item() {
        use xyscript_xpl::{BUILTIN_VERBS, CONTROL_KEYWORDS, FUNCTION_NAMES, SPECIAL_NAMES};
        let cmds = documented("#cmd(");
        let funcs = documented_first("#func(");
        let svs = documented("#sv(");
        let missing = |list: &[&str], have: &std::collections::HashSet<String>| -> Vec<String> {
            list.iter().filter(|n| !have.contains(**n)).map(|s| s.to_string()).collect()
        };
        let mc: Vec<String> = missing(BUILTIN_VERBS, &cmds)
            .into_iter()
            .chain(missing(CONTROL_KEYWORDS, &cmds))
            .collect();
        let mf = missing(FUNCTION_NAMES, &funcs);
        let ms = missing(SPECIAL_NAMES, &svs);
        eprintln!("undocumented commands ({}): {mc:?}", mc.len());
        eprintln!("undocumented functions ({}): {mf:?}", mf.len());
        eprintln!("undocumented specials ({}): {ms:?}", ms.len());
        assert!(mc.is_empty() && mf.is_empty() && ms.is_empty(), "reference omits items (see stderr)");

        // Symmetry with the Special-variables section (grouped + alphabetical, each complete): the
        // commands must appear in BOTH the topical "Commands by topic" listing AND the alphabetical
        // "Commands" A–Z — each complete on its own, so neither can silently drop a command.
        let both = |have: &std::collections::HashSet<String>| -> Vec<String> {
            missing(BUILTIN_VERBS, have).into_iter().chain(missing(CONTROL_KEYWORDS, have)).collect()
        };
        let topical = documented_in(&reference_section("= Commands by topic", "= Commands"), "#cmd(");
        let alpha = documented_in(&reference_section("= Commands", "= Functions"), "#cmd(");
        let mt = both(&topical);
        let ma = both(&alpha);
        eprintln!("missing from the TOPICAL command listing ({}): {mt:?}", mt.len());
        eprintln!("missing from the ALPHABETICAL command listing ({}): {ma:?}", ma.len());
        assert!(mt.is_empty(), "the topical 'Commands by topic' listing omits commands (see stderr)");
        assert!(ma.is_empty(), "the alphabetical 'Commands' listing omits commands (see stderr)");
    }

    #[test]
    fn help_resolves_every_documented_item() {
        // Phase 0 of Help v2: every built-in command, special, and function the reference documents
        // must be reachable via `HELP <name>` (the index is parsed from that same reference).
        use xyscript_xpl::{BUILTIN_VERBS, FUNCTION_NAMES, SPECIAL_NAMES};
        let mut missing = Vec::new();
        for v in BUILTIN_VERBS {
            if crate::help::lookup(v).is_none() {
                missing.push(format!("command {v}"));
            }
        }
        for s in SPECIAL_NAMES {
            if crate::help::lookup(&format!("${s}")).is_none() {
                missing.push(format!("$ {s}"));
            }
        }
        for f in FUNCTION_NAMES {
            if crate::help::lookup(&format!("{f}()")).is_none() {
                missing.push(format!("fn {f}"));
            }
        }
        assert!(missing.is_empty(), "HELP can't resolve {} item(s): {missing:?}", missing.len());
    }

    #[test]
    fn theme_applies_light_and_dark_schemes() {
        use crate::highlight::TokenKind;
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        // Dark: canvas/chrome/selection + highlight palette all flip together.
        e.run_command("THEME dark");
        assert_eq!(e.fg_color(), (220, 220, 220));
        assert_eq!(e.bg_color(), (30, 30, 34));
        assert_eq!(e.sel_bg_color(), (60, 80, 120));
        assert_eq!(e.highlight_color(&TokenKind::Keyword), Some((215, 130, 220)));
        // SaveCfg captures the resulting colours (the theme itself isn't a stored setting).
        e.run_command("SAVECFG cfg.cfg");
        let cfg = e.host_mut().files.get("cfg.cfg").unwrap().clone();
        assert!(cfg.contains("BGCOLOR 30 30 34"), "cfg: {cfg}");
        assert!(cfg.contains("HLCOLOR keyword 215 130 220"), "cfg: {cfg}");
        // Light returns to the factory defaults.
        e.run_command("THEME light");
        assert_eq!(e.fg_color(), (30, 30, 30));
        assert_eq!(e.bg_color(), (255, 255, 255));
        assert_eq!(e.highlight_color(&TokenKind::Keyword), Some((170, 40, 180)));
        // A post-theme tweak still persists (theme is a one-shot macro, not a mode lock).
        e.run_command("THEME dark");
        e.run_command("HLCOLOR keyword 1 2 3");
        assert_eq!(e.highlight_color(&TokenKind::Keyword), Some((1, 2, 3)));
        // A word that is neither a built-in nor an existing file is trappable (it is taken as a
        // theme-file path, which fails to load), leaving colours untouched.
        e.run_command("THEME bogus");
        e.run_command("PR $ERROR");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
        assert_eq!(e.bg_color(), (30, 30, 34)); // still dark
    }

    #[test]
    fn theme_path_runs_a_theme_file() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        // A theme file is just a SAVECFG-format script of colour settings.
        e.host_mut()
            .write_file("mytheme.xpl", "FGCOLOR 1 2 3\nBGCOLOR 4 5 6\nSELBGCOLOR 7 8 9\n")
            .unwrap();
        // A non-keyword argument is a path: the file is run to activate the theme.
        e.run_command("THEME mytheme.xpl");
        assert_eq!(e.fg_color(), (1, 2, 3));
        assert_eq!(e.bg_color(), (4, 5, 6));
        assert_eq!(e.sel_bg_color(), (7, 8, 9));
        assert!(e.output().join("\n").contains("loaded config from"), "confirms like LOADCFG");
        // The built-in keywords still win over the path interpretation (any case).
        e.run_command("THEME Dark");
        assert_eq!(e.fg_color(), (220, 220, 220));
        // A path that does not exist is trappable, leaving colours untouched.
        e.run_command("THEME no-such-theme.xpl");
        e.run_command("PR $ERROR");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
        assert_eq!(e.fg_color(), (220, 220, 220)); // still dark
    }

    #[test]
    fn tabbgcolor_sets_states_clears_and_persists() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        // Default: every state is egui's theme colour (None).
        assert_eq!(e.tab_active_bg(), None);
        assert_eq!(e.tab_hover_bg(), None);
        e.run_command("TABBGCOLOR active 20 40 60");
        assert_eq!(e.tab_active_bg(), Some((20, 40, 60)));
        e.run_command("PR $TABACTIVE");
        assert_eq!(e.output().last().map(String::as_str), Some("20 40 60"));
        e.run_command("TABBGCOLOR hover 80 80 90");
        assert_eq!(e.tab_hover_bg(), Some((80, 80, 90)));
        // An unset state reads back as empty (egui's theme colour).
        e.run_command("PR $TABINACTIVE");
        assert_eq!(e.output().last().map(String::as_str), Some(""));
        e.run_command("TABBGCOLOR inactive 30 30 34");
        assert_eq!(e.tab_inactive_bg(), Some((30, 30, 34)));
        // `default` clears one state back to the theme colour.
        e.run_command("TABBGCOLOR active default");
        assert_eq!(e.tab_active_bg(), None);
        // Unknown state is trappable; nothing else changes.
        e.run_command("TABBGCOLOR bogus 1 2 3");
        e.run_command("PR $ERROR");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
        assert_eq!(e.tab_hover_bg(), Some((80, 80, 90)));
        // SAVECFG emits only the set states.
        e.run_command("SAVECFG cfg.cfg");
        let cfg = e.host_mut().files.get("cfg.cfg").unwrap().clone();
        assert!(cfg.contains("TABBGCOLOR hover 80 80 90"), "cfg: {cfg}");
        assert!(cfg.contains("TABBGCOLOR inactive 30 30 34"), "cfg: {cfg}");
        assert!(!cfg.contains("TABBGCOLOR active"), "active was cleared; cfg: {cfg}");
    }

    #[test]
    fn dir_listing_special_vars() {
        use crate::dir::DirEntry;
        use std::path::PathBuf;
        use xyscript_xpl::DirOpts;
        let mut e = Editor::new(typing_keymap());
        // Not a listing: $D1 is 0, $DP/$DR empty.
        e.run_command("PR $D1");
        assert_eq!(e.output().last().map(String::as_str), Some("0"));
        e.run_command("PR $DP");
        assert_eq!(e.output().last().map(String::as_str), Some(""));
        e.run_command("PR $DR");
        assert_eq!(e.output().last().map(String::as_str), Some(""));
        // Make the active buffer a DIR listing of /d with a `*.txt` mask.
        let entries = vec![
            DirEntry { name: "readme.txt".into(), path: PathBuf::from("/d/readme.txt"), is_dir: false, ..Default::default() },
            DirEntry { name: "notes.txt".into(), path: PathBuf::from("/d/notes.txt"), is_dir: false, ..Default::default() },
        ];
        let opts = DirOpts { bare: true, ..Default::default() };
        e.host_mut().docs[0].open_listing(PathBuf::from("/d"), entries, opts, Some("*.txt".into()));
        e.run_command("PR $D1");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
        // $DP = listing dir + mask (the path separator is platform-specific).
        e.run_command("PR $DP");
        let dp = e.output().last().cloned().unwrap_or_default();
        assert!(dp.contains("*.txt") && dp.contains('d'), "DP: {dp}");
        // $DR = full path of the entry on the caret's line (top = readme.txt).
        e.run_command("PR $DR");
        assert_eq!(e.output().last().map(String::as_str), Some("/d/readme.txt"));
        // A listing with NO mask: $DP is just the directory; $DR follows the caret.
        let entries2 =
            vec![DirEntry { name: "alpha".into(), path: PathBuf::from("/d/alpha"), is_dir: true, ..Default::default() }];
        e.host_mut().docs[0].open_listing(
            PathBuf::from("/d"),
            entries2,
            DirOpts { bare: true, ..Default::default() },
            None,
        );
        e.run_command("PR $DP");
        assert_eq!(e.output().last().map(String::as_str), Some("/d"));
        e.run_command("PR $DR");
        assert_eq!(e.output().last().map(String::as_str), Some("/d/alpha"));
    }

    #[test]
    fn capture_redirects_command_output_to_a_variable() {
        let mut e = Editor::new(typing_keymap());
        // Inline form: the captured PR is redirected (not displayed) and lands in $greeting.
        // (PR prints its args verbatim — quotes aren't stripped — so use a bare literal here.)
        e.run_command("CAPTURE greeting PR hello");
        assert!(e.output().is_empty(), "captured PR should not display");
        e.run_command("PR $greeting");
        assert_eq!(e.output().last().map(String::as_str), Some("hello"));
        // Block form: multiple commands accumulate, joined with newlines.
        e.run_command("CAPTURE report\nPR line 1\nPR line 2\nENDCAPTURE");
        e.run_command("PR $report");
        assert_eq!(e.output().last().map(String::as_str), Some("line 1\nline 2"));
        // TXT under capture grabs its body and does NOT park the script (no modal).
        e.run_command(r#"CAPTURE t TXT "body text""#);
        assert!(e.text_request().is_none(), "TXT under capture must not open a modal");
        e.run_command("PR $t");
        assert_eq!(e.output().last().map(String::as_str), Some("body text"));
        // HLKINDS (normally a modal listing) is capturable too.
        e.run_command("CAPTURE k HLKINDS");
        e.run_command("PR $k");
        assert!(e.output().last().is_some_and(|s| s.contains("keyword")), "kinds: {:?}", e.output().last());
    }

    #[test]
    fn capture_grabs_a_dir_listing() {
        use std::fs;
        let dir = std::env::temp_dir().join(format!("xy_capture_dir_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("alpha.txt"), b"a").unwrap();
        fs::write(dir.join("beta.txt"), b"b").unwrap();
        let mut e = Editor::new(typing_keymap());
        // DIR under capture renders its (bare) listing into the var instead of opening a buffer.
        e.run_command(&format!("CAPTURE files DIR/B {}", dir.display()));
        assert_eq!(e.buffer_count(), 1, "DIR under capture must not open a listing tab");
        e.run_command("PR $files");
        let out = e.output().last().cloned().unwrap_or_default();
        assert!(out.contains("alpha.txt") && out.contains("beta.txt"), "listing: {out}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_wait_loop_drains_and_open_works_after() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert(
            "loop.xpl".into(),
            "SET c = 1\n:l\nPR c=$c\nWAIT 0\nSET c = $c + 1\nIF $c <= 3 GOTO l\nEXIT".into(),
        );
        e.host_mut().files.insert("foo.txt".into(), "hello world".into());
        // A RUN-ed WAIT loop drains through the editor's settle/drain path (WAIT 0 resolves at once).
        e.run_command("RUN loop.xpl");
        // A normal command works afterward — the editor isn't stuck-suspended.
        e.run_command("open foo.txt");
        assert_eq!(e.document().text(), "hello world", "OPEN ignored — editor stuck suspended?");
    }

    #[test]
    fn open_works_after_a_run_wait_loop_with_msg_and_exitall() {
        // Full loop10 shape through the editor: WAIT loop + CALL + MSG + EXITALL, then OPEN must work.
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert(
            "loop.xpl".into(),
            "SET c = 1\n:l\nPR c=$c\nWAIT 0\nSET c = $c + 1\nIF $c == 3 CALL out\nIF $c <= 5 GOTO l\nEXIT\nSUB out\nMSG bail\nEXITALL\nENDSUB".into(),
        );
        e.host_mut().files.insert("foo.txt".into(), "hello world".into());
        e.run_command("RUN loop.xpl");
        // Bare MSG now parks the script on a modal box — dismiss it so the sub's EXITALL can fire.
        assert_eq!(e.message_request().as_deref(), Some("bail"));
        e.dismiss_message();
        // EXITALL fired — editor must be free and OPEN must load the file into the active doc.
        e.run_command("open foo.txt");
        assert_eq!(e.document().text(), "hello world", "OPEN ignored — editor stuck suspended?");
    }

    #[test]
    fn db_de_move_cursor_to_selection_edges() {
        let mut e = Editor::new(typing_keymap());
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        e.run_command("insert \"hello\"");
        // No selection → DB/DE are trappable (set $ERROR), caret unchanged.
        e.run_command("DB");
        e.run_command("PR $ERROR");
        assert_eq!(out(&e).as_deref(), Some("1"));
        e.run_command("DE");
        e.run_command("PR $ERROR");
        assert_eq!(out(&e).as_deref(), Some("1"));
        // Forward-select "hel" (0..3); caret sits at the end (3).
        e.run_command("TOP");
        e.run_command("RIGHT SEL");
        e.run_command("RIGHT SEL");
        e.run_command("RIGHT SEL");
        e.run_command("PR $CP");
        assert_eq!(out(&e).as_deref(), Some("3"));
        // DB → caret to the start (0), selection collapsed, no error.
        e.run_command("DB");
        e.run_command("PR $ERROR");
        assert_eq!(out(&e).as_deref(), Some("0"));
        e.run_command("PR $CP");
        assert_eq!(out(&e).as_deref(), Some("0"));
        e.run_command("PR $DF"); // selection collapsed
        assert_eq!(out(&e).as_deref(), Some("0"));
        // Backward-select 0..3 (caret at the start, 0); DE → caret to the end (3).
        e.run_command("RIGHT");
        e.run_command("RIGHT");
        e.run_command("RIGHT");
        e.run_command("LEFT SEL");
        e.run_command("LEFT SEL");
        e.run_command("LEFT SEL");
        e.run_command("PR $CP");
        assert_eq!(out(&e).as_deref(), Some("0")); // caret at the start of a backward selection
        e.run_command("DE");
        e.run_command("PR $CP");
        assert_eq!(out(&e).as_deref(), Some("3"));
        e.run_command("PR $DF");
        assert_eq!(out(&e).as_deref(), Some("0"));
    }

    #[test]
    fn db_de_keep_a_defined_block() {
        // A DEFINED block (DW/DF/…) must survive DB/DE — the caret roams to the edge but the block
        // stays defined, exactly like `JMP $DS`/`JMP $DN`. (Transient Shift selections still collapse;
        // that path is covered by `db_de_move_cursor_to_selection_edges`.)
        let mut e = Editor::new(typing_keymap());
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        e.run_command(r#"insert "hello world""#);
        e.run_command("TF");
        e.run_command("DW"); // define (select) "hello" = bytes 0..5, a DEFINED block
        e.run_command("PR $DF");
        assert_eq!(out(&e).as_deref(), Some("1"));
        // DB → caret to the start; the block is PRESERVED (no error, still defined).
        e.run_command("DB");
        e.run_command("PR $ERROR");
        assert_eq!(out(&e).as_deref(), Some("0"));
        e.run_command("PR $CP");
        assert_eq!(out(&e).as_deref(), Some("0"), "DB lands the caret at the block start");
        e.run_command("PR $DF");
        assert_eq!(out(&e).as_deref(), Some("1"), "the defined block survives DB");
        // DE → caret to the end; still preserved.
        e.run_command("DE");
        e.run_command("PR $CP");
        assert_eq!(out(&e).as_deref(), Some("5"), "DE lands the caret at the block end");
        e.run_command("PR $DF");
        assert_eq!(out(&e).as_deref(), Some("1"), "the defined block survives DE");
    }

    #[test]
    fn mark_verbs_set_clear_and_navigate() {
        let mut e = Editor::new(typing_keymap());
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        e.run_command(r#"insert "abcdefghij""#);
        e.run_command("TF");
        e.run_command("PR $MARKS");
        assert_eq!(out(&e).as_deref(), Some("0"));
        // Set bookmarks at offsets 0 and 5.
        e.run_command("MARKSET");
        e.run_command("JMP 5");
        e.run_command("MARKSET");
        e.run_command("PR $MARKS");
        assert_eq!(out(&e).as_deref(), Some("2"));
        e.run_command("PR $MARK"); // caret at 5 sits on a bookmark
        assert_eq!(out(&e).as_deref(), Some("1"));
        // Re-setting where one already exists is a trappable error.
        e.run_command("MARKSET");
        e.run_command("PR $ERROR");
        assert_eq!(out(&e).as_deref(), Some("1"));
        // Off a bookmark, $MARK is 0.
        e.run_command("JMP 3");
        e.run_command("PR $MARK");
        assert_eq!(out(&e).as_deref(), Some("0"));
        // MARKNEXT: 3 → 5, then wraps 5 → 0.
        e.run_command("MARKNEXT");
        e.run_command("PR $CP");
        assert_eq!(out(&e).as_deref(), Some("5"));
        e.run_command("MARKNEXT");
        e.run_command("PR $CP");
        assert_eq!(out(&e).as_deref(), Some("0"));
        // MARKPREV from 0 wraps → 5.
        e.run_command("MARKPREV");
        e.run_command("PR $CP");
        assert_eq!(out(&e).as_deref(), Some("5"));
        // Clear at 5 (one left); clearing again there errors; MARKCLEARALL empties.
        e.run_command("MARKCLEAR");
        e.run_command("PR $MARKS");
        assert_eq!(out(&e).as_deref(), Some("1"));
        e.run_command("MARKCLEAR");
        e.run_command("PR $ERROR");
        assert_eq!(out(&e).as_deref(), Some("1"));
        e.run_command("MARKCLEARALL");
        e.run_command("PR $MARKS");
        assert_eq!(out(&e).as_deref(), Some("0"));
        // With no bookmarks, MARKNEXT is a trappable error.
        e.run_command("MARKNEXT");
        e.run_command("PR $ERROR");
        assert_eq!(out(&e).as_deref(), Some("1"));
    }

    #[test]
    fn bookmarks_ride_edits_and_relocate_when_text_moved() {
        use crate::Document;
        // Live anchor: an insert BEFORE the mark shifts its offset, no relocation.
        let mut d = Document::from_str("hello world foo");
        d.set_caret(6, None); // start of "world"
        assert!(d.mark_set());
        d.set_caret(0, None);
        d.insert("XYZ ");
        assert_eq!(d.marks()[0].off, 10, "live anchor followed the insert");
        assert!(!d.marks()[0].moved, "an in-session shift is not a relocation");

        // Cross-session: restore a mark captured in the ORIGINAL text into CHANGED text — its content
        // anchor relocates it (and flags the move).
        let mut src = Document::from_str("hello world foo");
        src.set_caret(6, None);
        src.mark_set();
        let saved = src.marks().to_vec();
        let mut d2 = Document::from_str("XYZ hello world foo");
        d2.set_marks_restored(saved);
        assert_eq!(d2.marks()[0].off, 10, "relocated by content search");
        assert!(d2.marks()[0].moved, "flagged as moved");
    }

    #[test]
    fn rl_visual_line_delete_and_rll_logical() {
        use crate::Document;
        // RLL / RL with no shaped span both delete the whole LOGICAL line (+ its break).
        let mut d = Document::from_str("alpha\nbravo\ncharlie");
        d.set_caret(8, None); // inside "bravo" (line 2)
        d.delete_visual_line(None); // no GUI span → logical line
        assert_eq!(d.buffer().text(), "alpha\ncharlie");

        // With a shaped visual-row span that CONTAINS the caret, RL deletes exactly that span (a
        // wrapped row is a slice of one logical line — here pretend "abcdefghij" wraps at 5).
        let mut d2 = Document::from_str("abcdefghij");
        d2.set_caret(2, None);
        d2.delete_visual_line(Some((0, 5)));
        assert_eq!(d2.buffer().text(), "fghij", "deleted just the visual row");

        // A span that does NOT contain the caret is stale — ignored, falls back to the logical line.
        let mut d3 = Document::from_str("abcdefghij");
        d3.set_caret(8, None);
        d3.delete_visual_line(Some((0, 5))); // caret 8 ∉ [0,5) → whole logical line
        assert_eq!(d3.buffer().text(), "");
    }

    #[test]
    fn rl_verb_uses_the_hosts_visual_line_span_with_revision_guard() {
        let mut e = Editor::new(typing_keymap());
        e.run_command(r#"insert "abcdefghij""#);
        e.run_command("JMP 2"); // caret at 2 (a move — doesn't change the revision)
        // The GUI publishes a shaped visual row [0,5) at the current revision.
        let rev = e.doc_revision();
        e.host_mut().set_visual_line_span(Some((0, 5)), rev);
        e.run_command("RL");
        assert_eq!(e.document().text(), "fghij", "RL used the GUI's visual-line span");

        // A span stamped with a STALE revision (an edit happened since) is ignored → logical fallback.
        e.run_command(r#"insert "PQRST""#); // an edit → bumps the revision, so `rev` is now stale
        e.host_mut().set_visual_line_span(Some((0, 3)), rev); // old rev — no longer current
        e.run_command("RL");
        assert_eq!(e.document().text(), "", "stale span ignored; RL deleted the logical line");
    }

    #[test]
    fn sel_special_holds_last_selection_across_clear_and_tabs() {
        let mut e = Editor::new(typing_keymap());
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        // Initially empty.
        e.run_command("PR <$SEL>");
        assert_eq!(out(&e).as_deref(), Some("<>"));
        e.run_command("insert \"hello\"");
        e.run_command("TOP");
        e.run_command("RIGHT SEL");
        e.run_command("RIGHT SEL");
        e.run_command("RIGHT SEL"); // selection = "hel"
        // A plain move clears the selection — its contents land in $SEL.
        e.run_command("RIGHT");
        e.run_command("PR <$SE><$SEL>");
        assert_eq!(out(&e).as_deref(), Some("<><hel>")); // $SE cleared, $SEL retains
        // Persists across tabs (host-global, unlike per-doc $SE).
        e.run_command("NEW");
        e.run_command("PR <$SE><$SEL>");
        assert_eq!(out(&e).as_deref(), Some("<><hel>"));
    }

    #[test]
    fn sel_special_captures_mouse_cleared_selections() {
        let mut e = Editor::new(typing_keymap());
        let out = |e: &Editor| e.output().last().map(String::as_str).map(str::to_string);
        e.run_command("insert \"hello\"");
        e.run_command("TOP");
        e.run_command("RIGHT SEL");
        e.run_command("RIGHT SEL");
        e.run_command("RIGHT SEL"); // selection "hel"
        // A mouse click clears the selection via the GUI's caret path (not editor_op).
        e.host_mut().click_active_caret(0);
        e.run_command("PR <$SE><$SEL>");
        assert_eq!(out(&e).as_deref(), Some("<><hel>")); // captured despite the mouse clear
    }

    #[test]
    fn highlight_on_for_unknown_type_is_a_noop() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("notes.txt".into(), "# not md\n".into());
        e.run_command("open notes.txt");
        e.run_command("HIGHLIGHT on"); // no highlighter for .txt
        assert!(e.highlights().is_empty());
        assert_eq!(e.highlighter_name(), None);
        assert!(e.last_error().is_none()); // not an error, just nothing to do
    }

    // ---- multi-document (step 4) -------------------------------------------

    #[test]
    fn open_adds_buffers_and_switch_cycles() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("a.txt".into(), "AAA".into());
        e.host_mut().files.insert("b.txt".into(), "BBB".into());

        e.run_command("open a.txt"); // reuses the empty scratch -> 1 buffer
        assert_eq!(e.buffer_count(), 1);
        assert_eq!(e.document().text(), "AAA");

        e.run_command("open b.txt"); // a.txt isn't empty-scratch -> adds -> 2
        assert_eq!(e.buffer_count(), 2);
        assert_eq!(e.document().text(), "BBB");
        assert_eq!(e.active_file_name().as_deref(), Some("b.txt"));

        e.run_command("bufnext"); // wraps to a.txt
        assert_eq!(e.document().text(), "AAA");
        e.run_command("bufprev"); // back to b.txt
        assert_eq!(e.document().text(), "BBB");
    }

    #[test]
    fn bufgo_by_name_and_index() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("one.txt".into(), "1".into());
        e.host_mut().files.insert("two.txt".into(), "2".into());
        e.run_command("open one.txt");
        e.run_command("open two.txt");

        e.run_command("bufgo one.txt");
        assert_eq!(e.document().text(), "1");
        e.run_command("bufgo 2"); // 1-based index -> two.txt
        assert_eq!(e.document().text(), "2");
        e.run_command("bufgo nope.txt\nIF $ERROR PR missing");
        assert!(e.output().contains(&"missing".to_string()));
    }

    #[test]
    fn buflist_shows_open_buffers_and_active_marker() {
        let mut e = Editor::new(typing_keymap());
        e.host_mut().virtual_fs = true;
        e.host_mut().files.insert("x.txt".into(), "x".into());
        e.run_command("open x.txt"); // reuses scratch -> 1 buffer (x.txt)
        e.run_command("new"); // adds an untitled buffer, now active
        e.run_command("buflist");
        let out = e.output();
        assert_eq!(out.len(), 2);
        assert!(out[0].contains("x.txt"));
        assert!(out[1].contains("(untitled)"));
        assert!(out[1].trim_start().starts_with('*')); // active marker
    }

    #[test]
    fn enter_submits_and_esc_cancels_the_cmline() {
        // Ctrl-ish keys: 18 opens the cmline; 16 types a command into it.
        let mut tables: HashMap<(u32, u8), Assignment> = HashMap::new();
        tables.insert((0, 18), Assignment::Xpl("cmline".into()));
        tables.insert((0, 16), Assignment::Xpl("insert \"insert hi\"".into()));
        let km = Keymap { modifiers: Vec::new(), tables, keys_max: 104, virtual_base: None };
        let mut e = Editor::new(km);

        // Open, type a command, Enter (key 28) submits it -> runs on the document.
        e.feed_key(KeyEvent::down(18));
        assert!(e.cmline_open());
        e.feed_key(KeyEvent::down(16)); // cmline now holds `insert hi`
        assert_eq!(e.command_line().text(), "insert hi");
        e.feed_key(KeyEvent::down(28)); // Enter -> submit
        assert!(!e.cmline_open());
        assert_eq!(e.document().text(), "hi");

        // Open again, Esc (key 1) cancels without running.
        e.feed_key(KeyEvent::down(18));
        assert!(e.cmline_open());
        e.feed_key(KeyEvent::down(16)); // type something
        e.feed_key(KeyEvent::down(1)); // Esc -> cancel
        assert!(!e.cmline_open());
        assert_eq!(e.document().text(), "hi"); // unchanged by the cancelled cmline
    }

    // ---- self-sufficiency of the embedded default --------------------------

    #[test]
    fn embedded_default_reaches_loadkbd_and_kfs_os_keys() {
        let mut e = Editor::with_default_keymap();
        // Esc (1) is a KF baseline key -> released to the OS.
        assert_eq!(e.feed_key(KeyEvent::down(1)), Disposition::Flip);
        // Ctrl+L reaches the LOADKBD command (Dispatch, not Swallow).
        assert!(matches!(
            e.feed_key(KeyEvent::inject(38, 256)),
            Disposition::Dispatch(_)
        ));
    }

    // ---- batch 2 special variables (renames/synonyms/new) ------------------

    #[test]
    fn autosave_special_renamed_to_au() {
        let mut e = ed();
        e.run_command("AUTOSAVE 15");
        e.run_command("PR $AU");
        assert_eq!(e.output(), &["15".to_string()]);
        // The old name is gone (unknown special → empty).
        e.run_command("PR $AUTOSAVE");
        assert_eq!(e.output(), &[String::new()]);
    }

    #[test]
    fn pa_is_a_cwd_synonym_and_dc_is_os_separator() {
        let mut e = ed();
        e.run_command("PR $PA");
        let pa = e.output()[0].clone();
        e.run_command("PR $CWD");
        assert_eq!(e.output(), &[pa]);
        e.run_command("PR $DC");
        assert_eq!(e.output(), &[std::path::MAIN_SEPARATOR.to_string()]);
    }

    #[test]
    fn re_mirrors_read_only_and_tx_is_focus_flag() {
        let mut e = ed();
        e.run_command("PR $RE");
        assert_eq!(e.output(), &["0".to_string()]);
        e.run_command("RO on");
        e.run_command("PR $RE");
        assert_eq!(e.output(), &["1".to_string()]);
        // $TX: 1 in the text area, 0 in the CMline.
        e.run_command("RO off");
        e.run_command("PR $TX");
        assert_eq!(e.output(), &["1".to_string()]);
        e.open_cmdline();
        e.run_command("PR $TX"); // focus is the cmline
        assert_eq!(e.output(), &["0".to_string()]);
    }

    #[test]
    fn tx_is_zero_for_a_command_submitted_from_the_cmline() {
        // The real GUI path: type a command INTO the cmline, then Enter (submit_cmdline) — which
        // flips `target` to Document. `$TX` must still report 0 (focus was the cmline).
        let mut e = ed();
        e.open_cmdline();
        e.run_command("INSERT \"PR $TX\""); // types into the focused cmline
        e.submit_cmdline(); // Enter
        assert_eq!(e.output(), &["0".to_string()]);
        // `PR` is a NEUTRAL command (doesn't touch the text), so per the per-verb focus rule you
        // remain in the cmline after submitting it (`$TX` still 0).
        assert!(e.cmline_open());
        e.run_command("PR $TX");
        assert_eq!(e.output(), &["0".to_string()]);
    }

    #[test]
    fn ds_dn_track_selection_and_fall_back_to_cursor() {
        let mut e = ed();
        e.run_command("INSERT \"hello world\"");
        e.run_command("TF"); // cursor at 0, no selection
        e.run_command("PR $DS");
        assert_eq!(e.output(), &["0".to_string()]); // falls back to $CP (0)
        e.run_command("PR $DN");
        assert_eq!(e.output(), &["0".to_string()]);
        // Select the first five chars (Right x5 with SEL extends).
        for _ in 0..5 {
            e.run_command("RIGHT SEL");
        }
        e.run_command("PR $DS");
        assert_eq!(e.output(), &["0".to_string()]);
        e.run_command("PR $DN");
        assert_eq!(e.output(), &["5".to_string()]);
    }

    #[test]
    fn cy_is_cursor_row_from_viewport_top() {
        let mut e = ed();
        e.run_command("INSERT \"l0\\nl1\\nl2\\nl3\""); // 4 lines, cursor on the last
        // viewport_top defaults to 0 (no GUI), so $CY == cursor's 0-based logical line.
        e.run_command("PR $CY");
        assert_eq!(e.output(), &["3".to_string()]);
        e.host_mut().set_viewport_top(2); // pretend the viewport starts at line 2
        e.run_command("PR $CY");
        assert_eq!(e.output(), &["1".to_string()]);
    }

    #[test]
    fn md_mu_accumulate_viewport_scroll() {
        let mut e = ed();
        e.run_command("MD"); // text down → viewport toward start → -1 row
        assert_eq!(e.take_scroll_rows(), -1);
        assert_eq!(e.take_scroll_rows(), 0); // drained
        e.run_command("MU"); // text up → toward end → +1
        e.run_command("MU");
        assert_eq!(e.take_scroll_rows(), 2); // accumulates between drains
    }

    #[test]
    fn tab_width_verb_and_special() {
        let mut e = ed();
        e.run_command("PR $TB");
        assert_eq!(e.output(), &["4".to_string()]); // default
        e.run_command("TB 8");
        e.run_command("PR $TB");
        assert_eq!(e.output(), &["8".to_string()]);
        // Bad / non-positive widths are trappable; the value is unchanged.
        e.run_command("TB 0");
        assert!(e.last_error().unwrap().starts_with("TB: expected"));
        e.run_command("TB wide");
        assert!(e.last_error().unwrap().starts_with("TB: expected"));
        e.run_command("PR $TB");
        assert_eq!(e.output(), &["8".to_string()]);
    }

    // ---- newly-wired special variables ------------------------------------

    #[test]
    fn file_begin_end_and_size_specials() {
        let mut e = ed();
        // Empty buffer: the cursor is at both the start AND the end.
        e.run_command("PR $FB");
        assert_eq!(e.output(), &["1".to_string()]);
        e.run_command("PR $FE");
        assert_eq!(e.output(), &["1".to_string()]);
        e.run_command("INSERT \"abc\""); // caret now at end
        e.run_command("PR $FS");
        assert_eq!(e.output(), &["3".to_string()]);
        e.run_command("PR $FB");
        assert_eq!(e.output(), &["0".to_string()]); // at end, not begin
        e.run_command("PR $FE");
        assert_eq!(e.output(), &["1".to_string()]);
        e.run_command("TF"); // back to top
        e.run_command("PR $FB");
        assert_eq!(e.output(), &["1".to_string()]);
        e.run_command("PR $FE");
        assert_eq!(e.output(), &["0".to_string()]);
    }

    #[test]
    fn tab_count_and_active_index_specials() {
        let mut e = ed();
        e.run_command("PR $TC");
        assert_eq!(e.output(), &["1".to_string()]);
        e.run_command("PR $CT");
        assert_eq!(e.output(), &["1".to_string()]); // 1-based
    }

    #[test]
    fn last_status_line_special() {
        let mut e = ed();
        e.run_command("PR hello there");
        e.run_command("PR $LS"); // reflects the previous command's status message
        assert_eq!(e.output(), &["hello there".to_string()]);
    }

    #[test]
    fn loadkbd_path_special() {
        let mut e = ed();
        e.host_mut()
            .files
            .insert("test.kbd".into(), "KEYS=104\nTABLE=\n16=q\n".into());
        e.run_command("LOADKBD test.kbd");
        assert!(e.last_error().is_none(), "{:?}", e.last_error());
        e.run_command("PR $KB");
        assert!(e.output()[0].ends_with("test.kbd"), "got {:?}", e.output());
    }

    #[test]
    fn lock_key_specials_return_a_bool() {
        let mut e = ed();
        for s in ["$CAPSLOCK", "$NUMLOCK", "$SCROLLLOCK"] {
            e.run_command(&format!("PR {s}"));
            let v = &e.output()[0];
            assert!(v == "0" || v == "1", "{s} = {v:?}");
        }
    }

    #[test]
    fn cmline_content_and_last_command_specials() {
        let mut e = ed();
        // Both empty initially: nothing typed, nothing run.
        e.run_command("PR $CM\nPR $CL");
        assert_eq!(e.output(), &["".to_string(), "".to_string()]);
        // Put text in the CMline → $CM reflects it (it reads the cmline buffer, focus-independent).
        e.open_cmdline();
        e.insert_str("hello world");
        e.run_command("PR $CM");
        assert_eq!(e.output().last().map(String::as_str), Some("hello world"));
        // $CL = the last command issued from the cmline (newest history entry).
        e.host_mut().push_history("open notes.txt");
        e.run_command("PR $CL");
        assert_eq!(e.output().last().map(String::as_str), Some("open notes.txt"));
    }

    #[test]
    fn executable_path_specials_are_nonempty() {
        let mut e = ed();
        e.run_command("PR $ED");
        assert!(!e.output()[0].is_empty());
        e.run_command("PR $XSDIR");
        assert!(!e.output()[0].is_empty());
    }

    #[test]
    fn ve_special_reports_the_crate_version() {
        let mut e = ed();
        e.run_command("PR $VE");
        assert_eq!(e.output().last().map(String::as_str), Some(env!("CARGO_PKG_VERSION")));
    }

    // ---- MSG: GUI message box (distinct from PR's status-line output) ------

    #[test]
    fn bare_msg_parks_on_a_modal_box_not_output() {
        let mut e = ed();
        e.run_command("MSG Hello there");
        assert!(e.output().is_empty()); // not the PR (status-line) channel
        // Bare MSG is now MODAL: it parks the script on a box (not a fire-and-forget toast).
        assert_eq!(e.message_request().as_deref(), Some("Hello there"));
        assert!(e.take_pending_message().is_none());
        e.dismiss_message(); // OK/Esc → resume
        assert!(e.message_request().is_none());
        // PR still goes to output.
        e.run_command("PR on the status line");
        assert_eq!(e.output(), &["on the status line".to_string()]);
    }

    #[test]
    fn bare_msg_blocks_on_each_loop_pass() {
        let mut e = ed();
        e.run_command("FOR i = 1 TO 3 ; MSG $i ; EF");
        // Parked on the FIRST message; each dismiss resumes to the next (not just the last "3").
        assert_eq!(e.message_request().as_deref(), Some("1"));
        e.dismiss_message();
        assert_eq!(e.message_request().as_deref(), Some("2"));
        e.dismiss_message();
        assert_eq!(e.message_request().as_deref(), Some("3"));
        e.dismiss_message();
        assert!(e.message_request().is_none()); // loop complete
    }

    #[test]
    fn timed_msg_is_a_nonblocking_toast() {
        let mut e = ed();
        e.run_command("MSG /T:5 quick");
        assert!(e.message_request().is_none()); // /T: does NOT park the script
        assert_eq!(e.take_pending_message(), Some(("quick".to_string(), Some(5))));
    }

    #[test]
    fn msg_timeout_switch_sets_the_self_dismiss_seconds() {
        let mut e = ed();
        e.run_command("MSG /T:5 Saved.");
        assert_eq!(e.take_pending_message(), Some(("Saved.".to_string(), Some(5))));
        // Quoted body processes escapes (like TXT/INSERT); `$`-vars substitute.
        e.run_command("SET who = World");
        e.run_command(r#"MSG /T:2 "Hi $who""#);
        assert_eq!(e.take_pending_message(), Some(("Hi World".to_string(), Some(2))));
    }

    #[test]
    fn msg_bad_timeout_is_trappable() {
        let mut e = ed();
        e.run_command("MSG /T:abc oops");
        assert!(e.last_error().unwrap().starts_with("MSG: /T:"));
        assert!(e.take_pending_message().is_none());
    }

    // ---- WAIT for background jobs (non-freezing suspend) -------------------

    #[test]
    fn bare_wait_with_no_jobs_resolves_immediately() {
        let mut e = ed();
        // Nothing running, so the WAIT condition holds at once and the script runs through.
        e.run_command("WAIT; PR done");
        assert!(e.wait_request().is_none());
        assert_eq!(e.output(), &["done".to_string()]);
    }

    #[test]
    fn wait_for_unknown_job_resolves_immediately() {
        let mut e = ed();
        e.run_command("WAIT/J 99; PR ok"); // no such job → not running → resume now
        assert!(e.wait_request().is_none());
        assert_eq!(e.output(), &["ok".to_string()]);
    }

    #[test]
    fn wait_zero_ms_resolves_immediately() {
        let mut e = ed();
        e.run_command("WAIT 0; PR slept"); // a 0ms pause is "now" → runs straight through
        assert!(e.wait_request().is_none());
        assert_eq!(e.output(), &["slept".to_string()]);
    }

    #[test]
    fn wait_milliseconds_parks_then_resumes_when_the_interval_elapses() {
        let mut e = ed();
        e.run_command("WAIT 40; PR slept"); // parks as a non-freezing timed pause
        assert_eq!(e.output().len(), 0); // not run yet
        assert!(e.wait_repaint_after().is_some()); // a pause is pending
        // Before the interval, polling does nothing.
        e.poll_wait();
        assert_eq!(e.output().len(), 0);
        // After it elapses, a poll (as the GUI does each frame) resumes the script.
        std::thread::sleep(std::time::Duration::from_millis(55));
        e.poll_wait();
        assert_eq!(e.output(), &["slept".to_string()]);
        assert!(e.wait_repaint_after().is_none());
    }

    // ---- READKEY / INPUT suspend-resume ------------------------------------

    #[test]
    fn tf_and_bf_are_synonyms_for_top_and_bottom() {
        let mut e = ed();
        e.run_command("INSERT \"abc\""); // caret at end: "abc‸"
        e.run_command("TF"); // XyWrite Top-of-File
        assert_eq!(e.render(), "‸abc");
        e.run_command("BF"); // XyWrite Bottom-of-File
        assert_eq!(e.render(), "abc‸");
    }

    #[test]
    fn el_er_logical_edges_and_lb_le_visual_fallback() {
        let mut e = ed();
        e.run_command("INSERT \"abc\""); // one line, caret at end: "abc‸" (cursor = 3)
        // EL / ER (Express Left/Right) = logical line begin/end, like HOME/END — and they do NOT
        // arm a visual-edge refine.
        e.run_command("EL");
        assert_eq!(e.render(), "‸abc");
        assert!(e.take_visual_edge().is_none());
        e.run_command("ER");
        assert_eq!(e.render(), "abc‸");
        assert!(e.take_visual_edge().is_none());
        // LB / LE fall back to the LOGICAL edge headless (no wrapping), but ALSO arm a
        // visual-edge request `(end, extend, from)` the GUI refines via cosmic.
        e.run_command("LB");
        assert_eq!(e.render(), "‸abc"); // logical fallback = line start
        assert_eq!(e.take_visual_edge(), Some((false, false, 3))); // from = pre-move caret (3)
        e.run_command("ER"); // a non-visual move clears any pending request
        e.run_command("LE");
        assert_eq!(e.render(), "abc‸");
        assert_eq!(e.take_visual_edge(), Some((true, false, 3)));
        // A plain move leaves no pending visual edge behind.
        e.run_command("EL");
        assert!(e.take_visual_edge().is_none());
    }

    // ---- command chaining (`;`) end-to-end through run_command -------------

    #[test]
    fn semicolon_chains_commands_end_to_end() {
        let mut e = ed();
        e.run_command("PR a; PR b; PR c");
        assert_eq!(e.output(), &["a".to_string(), "b".to_string(), "c".to_string()]);
    }

    #[test]
    fn semicolon_in_quotes_or_escaped_is_not_a_separator() {
        let mut e = ed();
        e.run_command("SET x = \"a;b\"; PR $x");
        assert_eq!(e.output(), &["a;b".to_string()]);
        let mut e = ed();
        e.run_command(r"PR foo\;bar");
        assert_eq!(e.output(), &["foo;bar".to_string()]);
    }

    #[test]
    fn search_pattern_semicolon_does_not_split() {
        // SE is self-delimiting: `;` inside the pattern is part of the search target.
        let mut e = ed();
        e.run_command("INSERT \"a;b;c\"");
        e.run_command("TOP");
        e.run_command("SE /;/"); // find the first ";" — must not be split into commands
        assert!(e.last_error().is_none());
    }

    #[test]
    fn scoped_search_requires_a_selection() {
        let mut e = ed();
        e.run_command(r#"INSERT "hello hello""#);
        e.run_command(r#"SE/S "hello""#); // no selection → trappable usage error ($ERROR + status)
        assert!(e.status_message().unwrap_or_default().contains("/S"));
        e.run_command("PR $ERROR");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
    }

    #[test]
    fn command_chains_after_a_search() {
        // The reported case: a `;`-chained command after `SE "..."` must execute.
        let mut e = ed();
        e.run_command("INSERT \"hello world\"");
        e.run_command("TOP");
        e.run_command("SE \"hello\"; PR found"); // search, THEN the chained PR runs
        assert!(e.last_error().is_none());
        assert_eq!(e.output(), &["found".to_string()]);
    }

    #[test]
    fn input_parks_then_resumes_with_typed_line() {
        let mut e = ed();
        e.run_command("INPUT name \"Who?\"\nPR $name");
        let req = e.input_request().expect("parked on INPUT");
        assert_eq!(req.kind, xyscript_xpl::InputKind::Line);
        assert_eq!(req.prompt.as_deref(), Some("Who?"));
        assert!(e.output().is_empty()); // nothing ran past INPUT yet
        e.feed_input_line("Bob".to_string());
        assert!(e.input_request().is_none());
        assert_eq!(e.output(), &["Bob".to_string()]);
    }

    #[test]
    fn readkey_resumes_with_char_and_sets_sc_special() {
        let mut e = ed();
        e.run_command("READKEY k\nPR $k\nPR $SC");
        assert_eq!(e.input_request().unwrap().kind, xyscript_xpl::InputKind::Key);
        // A bare READKEY shows no message.
        assert!(e.input_prompt().is_none(), "bare READKEY has no message box");
        e.feed_input_key("a", 30, 0);
        assert_eq!(e.output(), &["a".to_string(), "30".to_string()]);
    }

    #[test]
    fn readkey_describe_flag_writes_description_into_var() {
        let mut e = ed();
        e.run_command("READKEY/D k\nPR $k");
        assert!(e.awaiting_key());
        assert!(e.readkey_describe(), "/D selects describe mode");
        // The GUI builds the description (`Ctrl+Y`, `Enter`, …); here we feed one and confirm it lands
        // in the variable verbatim (not the raw character).
        e.feed_input_key("Ctrl+Y", 21, 256);
        assert_eq!(e.output(), &["Ctrl+Y".to_string()]);
        // Plain READKEY is NOT describe mode.
        e.run_command("READKEY k2");
        assert!(!e.readkey_describe());
    }

    #[test]
    fn readkey_reports_modifier_bits_in_scc() {
        // $SCC = $SC + the modifier bits held for the key (so KEY($SCC) reads the modified binding).
        let mut e = ed();
        e.run_command("READKEY k\nPR $SC\nPR $SCC");
        // 'A' (scancode 30) held with CTRL (bit 256) → $SC = 30, $SCC = 286.
        e.feed_input_key("A", 30, 256);
        assert_eq!(e.output(), &["30".to_string(), "286".to_string()]);
    }

    #[test]
    fn readkey_with_a_message_surfaces_it_as_a_prompt() {
        let mut e = ed();
        // `READKEY <var> <message>` — the message (a `$`-substituted quoted string with `\n`s)
        // is surfaced for the GUI's modal box while awaiting the key.
        e.run_command("READKEY letter \"Pick a letter:\\nA\\nB\"\nPR $letter");
        assert!(e.awaiting_key(), "still a key request");
        assert_eq!(e.input_prompt().as_deref(), Some("Pick a letter:\nA\nB"));
        e.feed_input_key("B", 48, 0);
        assert!(!e.awaiting_key(), "resumed after the key");
        assert_eq!(e.output(), &["B".to_string()]);
    }

    #[test]
    fn resumed_input_value_reaches_the_document() {
        let mut e = ed();
        e.run_command("INPUT name \"?\"\nINSERT $name");
        e.feed_input_line("hi".to_string());
        assert_eq!(e.document().text(), "hi");
    }

    #[test]
    fn pick_parks_then_resumes_with_chosen_item() {
        let mut e = ed();
        e.run_command("PICK c [\"red\",\"green\",\"blue\"]\nPR $c");
        let (title, items) = e.pick_request().expect("parked on PICK");
        assert_eq!(title, None);
        assert_eq!(items, vec!["red", "green", "blue"]);
        assert!(e.output().is_empty()); // nothing past PICK ran yet
        e.pick_respond(Some(2)); // choose "blue"
        assert!(e.pick_request().is_none());
        assert_eq!(e.output(), &["blue".to_string()]);
        e.run_command("PR $ERROR");
        assert_eq!(e.output().last().map(String::as_str), Some("0")); // success clears $ERROR
    }

    #[test]
    fn pick_title_and_variable_list() {
        let mut e = ed();
        e.run_command(r#"SET opts = ["a","b"]"#);
        e.run_command(r#"PICK p "Choose a letter" $opts"#);
        let (title, items) = e.pick_request().expect("parked on PICK");
        assert_eq!(title.as_deref(), Some("Choose a letter"));
        assert_eq!(items, vec!["a", "b"]);
        e.pick_respond(Some(0));
        e.run_command("PR $p");
        assert_eq!(e.output().last().map(String::as_str), Some("a"));
    }

    #[test]
    fn pick_cancel_sets_error_and_empty() {
        let mut e = ed();
        // The resumed continuation observes $ERROR (IF doesn't clear it, unlike action verbs) and
        // the empty value — so this checks both that cancel set $ERROR and that $c became "".
        e.run_command("PICK c [\"x\",\"y\"]\nIF $ERROR\nINSERT cancelled-$c\nEI");
        e.pick_respond(None); // cancel
        assert_eq!(e.document().text(), "cancelled-");
    }

    #[test]
    fn pick_index_mode_returns_one_based_index() {
        let mut e = ed();
        e.run_command(r#"PICK/I n ["x","y","z"]"#);
        e.pick_respond(Some(2)); // third item
        e.run_command("PR $n");
        assert_eq!(e.output().last().map(String::as_str), Some("3"));
    }

    #[test]
    fn pick_empty_list_errors_without_parking() {
        let mut e = ed();
        e.run_command("PICK c []");
        assert!(e.pick_request().is_none()); // no dialog opened
        e.run_command("PR $ERROR");
        assert_eq!(e.output().last().map(String::as_str), Some("1"));
    }

    #[test]
    fn stray_command_is_ignored_while_parked() {
        let mut e = ed();
        e.run_command("INPUT x \"?\"");
        assert!(e.input_request().is_some());
        // A command issued while parked must not clobber the saved continuation.
        e.run_command("INSERT \"clobber\"");
        assert!(e.input_request().is_some());
        assert_eq!(e.document().text(), "");
        e.feed_input_line("done".to_string());
        assert!(e.input_request().is_none());
    }
}

