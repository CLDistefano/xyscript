//! The [`Host`] seam (`stage1-interpreter-plan.md` §9, Appendix A.1).
//!
//! The interpreter never touches the OS, the keyboard crate, or a GUI directly.
//! It calls *out* through this trait, which the eventual app crate implements
//! and tests stub ([`StubHost`]). This is what keeps Stage 1 fully headless and
//! keeps the dependency arrow pointing the right way (the interpreter depends on
//! nothing from `xyscript-kbd`).
//!
//! Design rules from the spec:
//!
//! - **`KF` never reaches the interpreter.** It is a reserved input-layer
//!   directive decided synchronously in the keyboard layer's event callback; the
//!   interpreter only ever sees ordinary assignment strings.
//! - **`LOADKBD` is an ordinary XPL verb** that calls [`Host::load_kbd`], which
//!   the app wires to the kbd crate's transactional loader.
//! - **`$SC`** (last scan code) is preserved via [`Host::last_scan_code`]; the
//!   retired `LOADKEY`/`.qnk` binding verbs are not ported.

use crate::error::VmResult;
use crate::value::Value;

/// `MDA` text-attribute bits — the payload of a [`EditorOp::ModeAttr`] and of the document's
/// attribute spans. Independent flags; the `MDA` keywords select combinations.
pub const ATTR_BOLD: u8 = 1 << 0;
pub const ATTR_ITALIC: u8 = 1 << 1;
pub const ATTR_UNDERLINE: u8 = 1 << 2;
/// Reverse video: render the text in the current `BGCOLOR` over a `FGCOLOR` background (dynamic — it
/// tracks the editor colours, so it stays inverted across a theme change).
pub const ATTR_REVERSE: u8 = 1 << 3;
/// Strikethrough (`MDA SO`): a line drawn through the middle of the glyphs. Like underline, it has no
/// cosmic glyph attribute — the editor draws it as a rect; preview/PDF/HTML emit Typst `#strike[…]`.
pub const ATTR_STRIKE: u8 = 1 << 4;
/// Superscript (`MDA SU`): rendered as true superscript in preview/PDF/HTML (`#super[…]`). The editor
/// canvas can't raise/shrink a sub-run (cosmic owns per-line vertical layout), so on screen it shows
/// in the `SUCOLOR` tint — the XyWrite "mode colour" convention.
pub const ATTR_SUPER: u8 = 1 << 5;
/// Subscript (`MDA SD`): the lowered counterpart of [`ATTR_SUPER`] — `#sub[…]` in preview/PDF/HTML,
/// the `SDCOLOR` tint on screen.
pub const ATTR_SUB: u8 = 1 << 6;

/// Map an `MDA` attribute keyword to its action. `None` = not an attribute keyword (so the argument
/// is treated as a colour); `Some(None)` = `NM` (normal — strip attributes); `Some(Some(bits))` = set
/// that bitset. Keywords: `BO` bold, `IT` italic, `BI` bold+italic, `UL` underline, `BU` bold+underline,
/// `RV` reverse video, `SO` strikethrough, `SU` superscript, `SD` subscript. `SU`/`SD` are mutually
/// exclusive, and each `MDA` call SETS (replaces) the region's attribute bits, so applying one clears
/// the other.
pub fn parse_mda_attr(word: &str) -> Option<Option<u8>> {
    match word.trim().to_ascii_uppercase().as_str() {
        "BO" => Some(Some(ATTR_BOLD)),
        "IT" => Some(Some(ATTR_ITALIC)),
        "BI" => Some(Some(ATTR_BOLD | ATTR_ITALIC)),
        "UL" => Some(Some(ATTR_UNDERLINE)),
        "BU" => Some(Some(ATTR_BOLD | ATTR_UNDERLINE)),
        "RV" | "REVERSE" => Some(Some(ATTR_REVERSE)),
        "SO" | "STRIKE" | "STRIKEOUT" | "STRIKETHROUGH" => Some(Some(ATTR_STRIKE)),
        "SU" | "SUPER" | "SUPERSCRIPT" => Some(Some(ATTR_SUPER)),
        "SD" | "SUB" | "SUBSCRIPT" => Some(Some(ATTR_SUB)),
        "NM" | "NORMAL" => Some(None),
        _ => None,
    }
}

/// A text-match query for `MDALL` — the pattern plus the flags that decide how it matches, mirroring
/// the search verbs. Literal matches default to whole-word + case-insensitive (like `EMPH`); `/C` makes
/// them case-sensitive, `/R` switches to a regex (crate `regex`), `/P` to a Perl/fancy regex, and `/S`
/// confines the sweep to the current selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchQuery {
    pub pattern: String,
    pub case: bool,
    pub whole: bool,
    pub regex: bool,
    pub perl: bool,
    pub in_selection: bool,
}

/// A document's canvas font MODE (`USEFONT`). `Auto` follows the rule — syntax-highlighted / listing /
/// HEX buffers render monospace, prose renders proportional when an `EDPROPFAMILY` is set; `Mono`/`Prop`
/// pin it. Per-document, session-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontMode {
    Auto,
    Mono,
    Prop,
}

impl FontMode {
    /// Parse the `USEFONT` argument (case-insensitive). `""` is `None` (a bare report).
    pub fn parse(s: &str) -> Option<FontMode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(FontMode::Auto),
            "mono" | "monospace" | "monospaced" | "fixed" => Some(FontMode::Mono),
            "prop" | "proportional" | "variable" | "var" => Some(FontMode::Prop),
            _ => None,
        }
    }
    /// The canonical keyword (`$USEFONT`, status reports, `SAVECFG`).
    pub fn as_str(self) -> &'static str {
        match self {
            FontMode::Auto => "auto",
            FontMode::Mono => "mono",
            FontMode::Prop => "prop",
        }
    }
}

/// Auto-update policy (`UPDATEMODE off|alert|download|install`; `$UPDATEMODE`; SaveCfg).
/// - `Off` — never check.
/// - `Alert` — check on a throttled cadence and notify only.
/// - `Download` — also fetch + verify the new bundle into a staging area, but don't apply it.
/// - `Install` — also apply it (behind the overwrite guard), then prompt to restart.
///
/// The struct default is `Off`, so an unconfigured or lean build is inert; the shipped config sets
/// `alert`. This is pure policy state — selecting a mode never itself touches the network; the actual
/// check/download/install is driven by [`EditorOp::Update`] (manual) or a throttled startup poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpdateMode {
    #[default]
    Off,
    Alert,
    Download,
    Install,
}

impl UpdateMode {
    /// Parse the `UPDATEMODE` argument (case-insensitive). `""` is `None` (a bare report).
    pub fn parse(s: &str) -> Option<UpdateMode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" => Some(UpdateMode::Off),
            "alert" | "notify" => Some(UpdateMode::Alert),
            "download" => Some(UpdateMode::Download),
            "install" | "auto" => Some(UpdateMode::Install),
            _ => None,
        }
    }
    /// The canonical keyword (`$UPDATEMODE`, status reports, `SAVECFG`).
    pub fn as_str(self) -> &'static str {
        match self {
            UpdateMode::Off => "off",
            UpdateMode::Alert => "alert",
            UpdateMode::Download => "download",
            UpdateMode::Install => "install",
        }
    }
}

/// A manual `UPDATE` sub-action ([`EditorOp::Update`]). Bare `UPDATE` runs the step appropriate to the
/// current [`UpdateMode`]; a sub-command forces one step. Grows across the auto-update phases — Phase 1
/// implements `Check`; later phases add `Notes`, `Download`, `Install`, `Rollback`, and `Reset`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateAction {
    /// Query the release channel and report whether a newer version exists (records `$UPDATEAVAIL`).
    Check,
    /// Download the new release bundle into staging and verify every file's SHA-256 against its
    /// manifest, without touching the install (records `$UPDATESTAGED`). Needs the `update` build.
    Download,
    /// Apply a staged (or freshly downloaded) release behind the overwrite guard — write/replace files,
    /// drop `.new` beside edited ones, back up what it replaces — then self-replace the running binary.
    /// Prompts to restart. Needs the `update` build.
    Install,
    /// Schedule a staged (or freshly downloaded) release to be applied automatically at the NEXT launch
    /// (`UPDATE INSTALL /NEXT`), instead of now — the safe moment to swap the binary. Needs the `update`
    /// build. Cancel a schedule with `Cancel`.
    Schedule,
    /// Cancel a scheduled next-launch update (`UPDATE CANCEL`): drop the pending marker so the next
    /// launch won't apply it (the staged files are kept). Pure-local; works in any build.
    Cancel,
    /// Undo the last `Install` from its backup (restores the prior files + binary). Needs the backup.
    Rollback,
    /// Re-pull one shipped default, overriding the guard for that path: promote its `<name>.new` (or
    /// copy from the staged bundle) over the local copy, backing the local copy up first. The escape
    /// hatch for when you want the new default or a managed file got corrupted.
    Reset(String),
}

/// Which colour of the modal `MSG` box a `SetMsgColor`/`OpenMsgColorPicker` targets: the "Message"
/// title text, the box background, or the OK button's text / background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgColorSlot {
    HeaderFg,
    Bg,
    OkFg,
    OkBg,
}

/// Window-management actions for `XSW` (XyScript Window). Applied by the GUI runtime against the OS
/// window; the headless host only queues them. Tier 1 (self-window) is cross-platform; the
/// `*Others` variants (minimize/restore every *other* app's windows) are Windows-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowCmd {
    /// Maximize this window.
    Maximize,
    /// Un-maximize (and un-minimize) this window.
    Restore,
    /// Flip between maximized and restored.
    ToggleMax,
    /// Nudge this window by `(dx, dy)` physical pixels (right/down positive). Clamped onto a monitor.
    Move { dx: i32, dy: i32 },
    /// Grow (`percent` > 0) or shrink (`percent` < 0) this window's size by that percentage.
    Resize { percent: i32 },
    /// Center this window on its current monitor.
    Center,
    /// Minimize every other top-level app window (Windows-only). Remembers the set for `RestoreOthers`.
    MinimizeOthers,
    /// Restore the windows a prior `MinimizeOthers` minimized (Windows-only).
    RestoreOthers,
    /// Toggle between `MinimizeOthers` and `RestoreOthers` (Windows-only).
    ToggleOthers,
}

/// A document's line-ending style, as `NEWLINE lf|crlf|cr` selects it. `Cr` (classic Mac) is stored
/// LF-internally by the host and re-emitted as `\r` on save; see `document::LineEnding`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewlineStyle {
    Lf,
    Crlf,
    Cr,
}

/// Editor/text operations the interpreter can request. The host (Stage 2's text
/// engine, or the test stub) applies them. The set grows as editor verbs are
/// registered.
#[derive(Debug, Clone, PartialEq)]
pub enum EditorOp {
    /// Insert literal text at the cursor (replacing any selection).
    InsertText(String),
    /// Overwrite the current selection with `text` as ONE undo unit, IGNORING persistent-selection
    /// mode (unlike `InsertText`, which under PS keeps the block and edits at the caret). The Find
    /// dialog's single Replace uses this so a match is always replaced/deleted. No-op if nothing is
    /// selected.
    ReplaceSelection(String),
    /// Interactively-typed text (the OS text/IME path, distinct from the scripted `INSERT` verb).
    /// Applies `TYPEPAIRS` auto-pairing when it's on and the document has focus; otherwise identical
    /// to [`InsertText`](EditorOp::InsertText).
    TypeText(String),
    /// Delete in some direction / the selection.
    Delete(DeleteKind),
    /// Move the cursor; `extend` grows the selection (Shift+motion).
    Move { motion: Motion, extend: bool },
    /// Multi-caret management (`CARET*` verbs). `CaretAdd(None)` = plant at the primary + enter the
    /// keyboard building state; `CaretAdd(Some(n))` = add/toggle a caret at byte offset n.
    /// `CaretVertical(down)` = `CARETUP`/`CARETDOWN`. `CaretOne` = `CARETONE` (collapse to primary).
    /// `CaretColumn` = `CARETCOL`: a selection becomes one caret per line at the goal column.
    /// `CaretAll` = `CARETALL`: a selection-caret at every match of the pattern (or of the current
    /// selection / word if `pattern` is `None`). `case_sensitive` = `/A`, `whole` = `/W`,
    /// `regex` = `/R`, `in_selection` = `/S` (confine the search to the current selection).
    CaretAdd(Option<usize>),
    CaretVertical(bool),
    CaretColumn,
    CaretAll {
        pattern: Option<String>,
        regex: bool,
        whole: bool,
        case_sensitive: bool,
        in_selection: bool,
    },
    /// `CaretNext` = `CARETNEXT`: add the next occurrence of the primary selection (or `text`) after
    /// the last caret as a selection-caret and make it primary; wraps at end-of-document. With nothing
    /// selected and no `text`, it seeds by selecting the word under the caret.
    CaretNext {
        text: Option<String>,
        regex: bool,
        whole: bool,
        case_sensitive: bool,
    },
    /// `CaretPop` = `CARETPOP`: undo the most recent caret-set change (remove the last-added caret /
    /// step back through the caret ring). `CaretPrimary(n)` = `CARETPRIMARY <n>`: make the caret at
    /// document-order index `n` the primary.
    /// `CaretSkip` = `CARETSKIP`: during a CARETNEXT run, MOVE the primary selection to the next
    /// occurrence (skip the current match) without adding a caret.
    CaretSkip {
        regex: bool,
        whole: bool,
        case_sensitive: bool,
    },
    CaretPop,
    CaretPrimary(usize),
    /// `CaretSplit` = `CARETSPLIT`: a multi-line selection becomes one bare caret per line (at each
    /// line's end). A single-line selection is left unchanged.
    CaretSplit,
    CaretOne,
    /// Move the cursor to the start (`DB`, `end = false`) or end (`DE`, `end = true`) of the current
    /// selection, collapsing it. Errors "no text is selected" (→ `$ERROR`) when nothing is selected.
    MoveToSelectionEdge { end: bool },
    /// Defined-text ("DF") selection control (`DF`/`DM`/`DZ`/`DFA`/`XD`).
    Define(DefineKind),
    /// Persistent-selection mode (`SPS`/`CPS`/`TPS`).
    Persistent(PersistKind),
    /// Cut the selection into the deletion buffer (`RD`), then delete it. Errors if
    /// there is no selection.
    RubDelete,
    /// Paste the deletion buffer at the cursor (`UD`); the buffer is *kept* (so one
    /// `RD` can be pasted repeatedly). Under persistent selection it does not replace
    /// an active block (inserts at the caret; the block rides along).
    PasteDeletion,
    /// Delete the selection (`DN`); errors "no text is selected" if there is none
    /// (a deliberate divergence from QN, which deletes the char under the cursor).
    DeleteSel,
    /// Change case of the selection, or the character under the cursor when there is
    /// no selection (`LC`/`UC`/`CC`).
    Case(CaseKind),
    /// `CP` — copy the selected text to the cursor position (leaves the selection).
    CopySelection,
    /// `MV` — move the selected text to the cursor position (delete + insert).
    MoveSelection,
    /// `SE`/`CH` — a literal search (`replace == None`) or replace-all.
    Search(SearchSpec),
    /// `SORT` — sort the selected lines (one undo unit), re-selecting the result.
    Sort(SortSpec),
    /// Clipboard (`$CB`) operation — copy/cut/paste/append/prepend/set.
    Clip(ClipKind),
    /// Focus the command line (CMline) at its remembered cursor — subsequent typing
    /// targets it. Does NOT clear it (`GH`, the `cmline` verb, the GUI open). B23.
    OpenCmdLine,
    /// Submit the command line: the editor runs its contents as XPL after the
    /// current script returns, then closes it.
    SubmitCmdLine,
    /// Close the command line without running it.
    CancelCmdLine,
    /// `GT` — focus the text window at its remembered cursor (keeps the cmline contents).
    FocusText,
    /// Shift focus from the CMline to the document IF the CMline is currently focused; otherwise a
    /// no-op. Editing/navigation verbs (`HM`/`NW`/`DW`/`RW`/…) emit this first so that, when fired
    /// while the cmline is focused, they act on the document and leave you there. Conditional on the
    /// live focus, so the transient `target=Document` of `XC`/SUBMIT is untouched.
    EnsureDocumentFocus,
    /// `TH` — toggle focus between the cmline and the text window, restoring each side's
    /// remembered cursor (no clearing).
    ToggleFocus,
    /// `BC` — blank the cmline and put its caret at position 0 (then focus it).
    BlankCmdLine,
    /// `BC <text>` — blank the cmline, then put the LITERAL `text` on it (no `$`-expansion) and
    /// focus it. Typically followed by `XC`, where the `$`-expansion happens.
    SetCmdLine(String),
    /// `XC` — run the cmline's current contents as XPL, keeping them and the focus.
    ExecCmdLine,
    /// Undo the last `levels` edits on the active document (1 = the most recent;
    /// stops early when there is nothing left to undo).
    Undo(usize),
    /// Redo the last `levels` undone edits on the active document (1 = the most recent;
    /// stops early when there is nothing left to redo).
    Redo(usize),
    /// `UNDOBEGIN` — open an undo group on the active document: every edit until the matching
    /// `UNDOEND` folds into ONE undo unit (so a composite command undoes in a single step).
    /// Depth-counted; `UNDO`/`REDO` force-close any open group.
    BeginUndoGroup,
    /// `UNDOEND` — close one level of the active document's undo group.
    EndUndoGroup,
    /// Start a fresh, empty document (`NEW`).
    NewDoc,
    /// `NEW <path>` — start a fresh buffer named for `path`, ready to `SAVE`. Forgiving: if the file
    /// already exists it's opened instead (switching to its tab if already open), with an
    /// informational "already exists" status.
    NewNamed { path: String },
    /// Open a file into a tab (`OPEN <path>`). `force` (`OPEN/NV`) skips the "already open in
    /// another tab?" prompt and opens a fresh tab unconditionally. `hex` (`OPEN/HEX`) opens the file
    /// straight into the hex + text view.
    Open { path: String, force: bool, hex: bool },
    /// Open an EXISTING file in a read-only tab (`READ <path>`). Unlike `Open`, a missing file is a
    /// trappable error (not a new buffer); the opened doc is locked (toggle off with `RO`). `force`
    /// (`READ/NV`) skips the already-open prompt. `hex` (`READ/HEX`) opens it in the hex view.
    OpenReadOnly { path: String, force: bool, hex: bool },
    /// Change the editor's working directory (`CD <path>`; `/D` is accepted and
    /// harmless since the cwd is managed in-process). Relative paths resolve against
    /// the current cwd.
    ChangeDir(String),
    /// Show a directory listing in its own buffer (`DIR`/`LS [switches] [path]`). `path`
    /// `None`/empty = the current working directory; otherwise resolved against the cwd
    /// (its last component may be a glob filespec). `opts` carries the view + sort flags.
    Dir { path: Option<String>, opts: DirOpts },
    /// `GREP <pattern> [path]` — ripgrep-style recursive search into a read-only results tab
    /// (`path:line:col: text`, one hit per line). `regex` (`/R`) treats the pattern as a regular
    /// expression, else it is a literal; `case` (`/I`) case-insensitive; `whole` (`/W`) whole-word.
    /// `path` is an optional search root: a directory (`GREP foo src`), a glob (`GREP foo *.rs`,
    /// searches `$CWD`), or both (`GREP foo src/*.rs`) — resolved like a `DIR` filespec. Absent = the
    /// current directory. See [`crate::host`] and grep.rs.
    Grep { pattern: String, path: Option<String>, regex: bool, case: bool, whole: bool, perl: bool },
    /// `GJ` — point-and-shoot on a `GREP` results listing: open the file for the hit on the caret's
    /// line (or switch to it if already open), jump to its `line:col`, and centre it. No-op message
    /// when the active buffer isn't a results listing / the caret isn't on a hit.
    GrepJump,
    /// `GREPBEFORE <n>` / `GREPAFTER <n>` — set the leading / trailing context-line count for `GREP`
    /// results (0 = none). `$GREPBEFORE` / `$GREPAFTER`; SaveCfg.
    SetGrepBefore(usize),
    SetGrepAfter(usize),
    /// `GREPBLANK on|off` — insert a blank line between `GREP` result groups. `$GREPBLANK`; SaveCfg.
    SetGrepBlank(bool),
    /// Like `Dir`, but instead of opening a listing buffer, RENDER the listing to text and return
    /// it as the op value — for `CAPTURE … DIR …`, which redirects the listing into a variable.
    DirToString { path: Option<String>, opts: DirOpts },
    /// `DIR/W` — open (or refocus) the listing in a separate movable/resizable child window (a
    /// single reusable window). Double-click opens a file in a new parent tab; the listing
    /// auto-refreshes when the window regains focus (if the directory changed).
    DirWindow { path: Option<String>, opts: DirOpts },
    /// `DIRNW on|off` — whether opening a file from an in-TAB directory listing opens it in a **new**
    /// tab (`on`, listing stays) or replaces the listing in place (`off`). Read back via `$DIRNW`.
    SetDirNewTab(bool),
    /// `DIRWNW on|off` — the same choice for the separate `DIR/W` WINDOW: `on` (the default) opens
    /// the picked file in a new tab; `off` reuses the active tab instead. Distinct from `DIRNW`
    /// because the two are different interactions — `DIR/W` floats beside your work, so it has
    /// always added a tab. Read back via `$DIRWNW`.
    SetDirWindowNewTab(bool),
    /// `DIRWCLOSE on|off` — whether the `DIR/W` window CLOSES once you open a file from it (`on`),
    /// or stays open to keep picking (`off`, the default). Independent of [`Self::SetDirWindowNewTab`],
    /// which chooses the TAB the file lands in. Descending into a subdirectory never closes it.
    /// Read back via `$DIRWCLOSE`.
    SetDirWindowClose(bool),
    /// `DIRRT on|off` — whether a `DIR` command reuses an already-open listing tab (`on`) instead of
    /// spawning a new one each time (`off`, the default). Read back via `$DIRRT`.
    SetDirReuse(bool),
    /// `DIRSET <switches>` — the default sort/filter/view switches applied to a `DIR`/`LS` command
    /// that doesn't override them. Each token is an already-validated `DirOpts` switch name (e.g.
    /// `"OD"`, `"R"`); an empty vec clears the default. A command switch overrides a default with the
    /// SAME leading letter (so `DIR /ON` beats a `/OD` default, `DIR /AH` leaves it alone). Read back
    /// via `$DIRSET`.
    SetDirDefaults(Vec<String>),
    /// `SESSION on|off` (bare = toggle) — auto-save-and-restore the open tabs across runs. `None` =
    /// toggle. Read back via `$SESSION`; persisted by `SAVECFG`. The actual save/restore is the GUI's
    /// job (on exit / at startup); this only flips the preference.
    SetSessionRestore(Option<bool>),
    /// Enable/disable shell-out for the session (`TRUST 1`/`TRUST 0`). Read back via `$TR`.
    SetTrust(bool),
    /// `RUNHERE [args…]` — run the fenced code block enclosing the caret as XPL (literate
    /// scripting). The host stashes the block; the editor runs it after the script returns. Any
    /// operand tokens are seeded as the block's positional args (`$1..$N`/`$*`/`$#`), like `RUN`.
    RunHere { args: Vec<String> },
    /// `ZT` — reset the stopwatch to zero (subsequent `$ET`/`ET`/`ETS` measure from now).
    ResetStopwatch,
    /// `MSG [/T:<secs>] <text>` — show `text` in a GUI message box. `timeout_secs` self-
    /// dismisses it after that many seconds (`None` = stays until the user closes it).
    ShowMessage { text: String, timeout_secs: Option<u64> },
    /// `LEARN <word>` — add a word to the user dictionary (stops being flagged; persisted).
    Learn(String),
    /// `UNLEARN <word>` — remove a word from the user dictionary.
    Unlearn(String),
    /// `LEARN` with several words at once (a list of args, the current selection via `/S`, or a file
    /// via `/F`) — adds them all, then rebuilds the checker and persists ONCE.
    LearnWords(Vec<String>),
    /// `UNLEARN` with several words at once — the bulk counterpart of [`Unlearn`](Self::Unlearn).
    UnlearnWords(Vec<String>),
    /// `DIALECT <name>` — set the English dialect harper checks against (american|british|…).
    /// The host parses `name`; `$DIALECT` reads it.
    SetDialect(String),
    /// `SPELLENGINE harper|hunspell` — pick the SPELLING checker (Harper built-in vs the Hunspell CLI
    /// for multilingual spelling). The host parses the name.
    SetSpellEngine(String),
    /// `SPELLDICT <names>` — the Hunspell dictionaries (`-d` value, e.g. `en_US,de_DE`).
    SetSpellDicts(String),
    /// `SPELLBIN <path>` — the Hunspell executable (name on `PATH` or a full path).
    SetSpellBin(String),
    /// `SPELLING on|off` (`SP`) — toggle LIVE spelling underlines. `$SPELLING`.
    SetSpelling(bool),
    /// `GRAMMAR on|off` (`GR`) — toggle LIVE grammar/word-choice underlines. `$GRAMMAR`.
    SetGrammar(bool),
    /// `SPELLNEXT`/`SPELLPREV`/`GRAMMARNEXT`/`GRAMMARPREV` — move the caret to (and select) the next
    /// or previous spelling / grammar issue, scanning beyond the viewport as needed.
    LintNav { spelling: bool, forward: bool },
    /// Start a background `CHECK` (harper on its own thread); its report opens in a read-only results
    /// tab when it finishes. The GUI shows "Working…" meanwhile. `selection` (`CHECK/S`) checks only the
    /// selected text (a trappable error if nothing is selected); the report's `line:col` stay
    /// document-relative so a jump still works.
    RequestFullCheck { selection: bool },
    /// `CHECK` — run the language checker (harper) over the active document (or, with `selection`, the
    /// selected text only) and return a report (one line per issue: `line:col [kind] "snippet":
    /// message`, or a "no issues" line). The report is the return `Value` (the CAPTURE path).
    RunLint { selection: bool },
    /// `MESSAGE [/T:"title"] <text>` — pop an OS-NATIVE message box (a real desktop dialog),
    /// launched as a separate detached process so it works from a headless background (`RUN/BG`)
    /// child and survives the script. Unlike `ShowMessage`, it needs no running GUI and never
    /// blocks. `title` is the window caption (`None` = the default app name).
    OsMessage { title: Option<String>, text: String },
    /// Copy a file (`COPY <src> <dst>`). Paths resolve against the working dir.
    FileCopy { src: String, dst: String },
    /// Delete a file or empty directory (`DEL/NV <path>` / `ERNV <path>`) immediately, no confirm.
    /// Path resolves against the cwd.
    FileDelete(String),
    /// Arm a delete-confirmation prompt for `DEL <path>` (no `/NV`): the GUI asks before deleting.
    /// Path resolves against the cwd; a missing file errors up front rather than arming the prompt.
    FileDeleteConfirm(String),
    /// Rename/move a file (`RENAME`/`REN <from> <to>`). Paths resolve against the cwd.
    FileRename { from: String, to: String },
    /// Save the active document. `None` = its current path; `Some(path)` =
    /// save-as (adopt `path`).
    Save(Option<String>),
    /// `SAVE <path>` / `SA <path>` — write the active document's bytes to `path` as a COPY,
    /// leaving the document's own path and tab untouched (it stays open as-is). `force=false`
    /// asks the GUI to confirm before overwriting an existing file; `force=true` (`/NV`)
    /// overwrites without asking.
    SaveCopy { path: String, force: bool },
    /// `ST` ("store") — save the active document, then close its tab. `path=None` saves in place
    /// (an untitled doc is a trappable error, so the tab isn't discarded); `path=Some(p)` saves-and-
    /// ADOPTS `p` then closes. When adopting an existing DIFFERENT file, `force=false` (`ST <path>`)
    /// asks the GUI to confirm the overwrite before saving+closing; `force=true` (`ST/NV <path>`)
    /// overwrites without asking.
    Store { path: Option<String>, force: bool },
    /// `SAAS`/`SAVEAS <path>` — save the active document to `path` and ADOPT it (the doc becomes that
    /// file). When `path` is a DIFFERENT existing file, `force=false` asks the GUI to confirm the
    /// overwrite; `force=true` (`/NV`) overwrites without asking. (The no-path form uses `SaveAsPrompt`;
    /// the GUI's native dialog does its own overwrite prompt, so it stays on `Save`.)
    SaveAs { path: String, force: bool },
    /// Request the GUI's native Save-As dialog (`SAAS` with no path). Headless hosts just
    /// record the request; the GUI drains it and opens the picker.
    SaveAsPrompt,
    /// Request the GUI's native Open-file dialog (`OPEN` with no path). The GUI drains it and
    /// opens the chosen file; headless hosts just record the request. `hex` (`OPEN/HEX` with no
    /// path) opens whatever the user picks in the hex view.
    OpenPrompt { hex: bool },
    /// Request the GUI's non-modal Find/Replace dialog (`FIND`). The GUI drains it and opens the
    /// dialog; headless hosts just record the request (it does nothing without a GUI).
    OpenFind,
    /// `SCRATCH` — open a new floating scratchpad window (a plain read/write text box for parking
    /// text; no file, no save). Each call opens another; the GUI drains the request and creates the
    /// window. `paste` (`SCRATCH/V`) prefills it with the clipboard. Headless hosts just record it.
    OpenScratch { paste: bool },
    /// `SCRATCHCLOSE` — close the most-recently-focused scratchpad window (its text is stashed into
    /// `$SCRATCHTEXT` first). `insert` (`SCRATCHCLOSE/V`) also inserts that text into the active tab
    /// at the caret. No-op if none is open.
    CloseScratch { insert: bool },
    /// `SH`/`SHELL` — open + focus the Console tab (creating it and writing a prompt if needed), and
    /// show it in the strip. `show: false` (`SH off`) hides the Console tab instead.
    OpenConsole { show: bool },
    /// `SHPROMPT [<template>]` — set the Console prompt template (`$`-expanded; `None` restores the
    /// default `$CWD> `). `$SHPROMPT` reads it; persisted by SaveCfg.
    SetConsolePrompt(Option<String>),
    /// `ANSICOLOR on|off` (`None` toggles) — whether Console output is ANSI-coloured. `$ANSICOLOR`;
    /// SaveCfg; default on. Off leaves the raw escape codes in the text.
    SetAnsiColor(Option<bool>),
    /// `CLS` / `CLEAR` — empty the Console scrollback and write a fresh prompt.
    ConsoleClear,
    /// Request the GUI's GREP dialog (bare `GREP` with no pattern). The GUI drains it and opens the
    /// dialog; headless hosts just record it.
    OpenGrepDialog,
    /// `RUN/BG <script>` — run an XPL script in the BACKGROUND by spawning a child `xyscript`
    /// process (`$ED --run <script>`) as a background job (reusing the `X/BG` machinery: streaming
    /// to *Shell Output*, `JOBS`/`KILL`/`$XO`/`$RC`). The child has its own editor state; its cwd is
    /// the editor's `$CWD`. Headless hosts just log it. `return_var` (`RUN/BG/R:<var>`) names a
    /// parent variable to receive the child's full output on completion.
    RunBackgroundScript { path: String, args: Vec<String>, return_var: Option<String> },
    /// `RUNCODE/BG <code>` — like `RunBackgroundScript`, but the XPL `code` (from a variable) is fed
    /// to a `$ED --run -` child via stdin (no temp file). `return_var` (`/R:<var>`) as above.
    RunBackgroundCode { code: String, return_var: Option<String> },
    /// `EDFONTS` — request the GUI's font-picker dialog for the editor canvas (family list filtered
    /// to monospaced + size, with a live preview). On OK the GUI applies `EDFAMILY`/`EDFONT`.
    /// Headless hosts just record the request.
    EditorFontPicker,
    /// `EDPROPFONTS` — request the GUI's font-picker dialog for the editor's PROPORTIONAL canvas font
    /// (all families). On OK the GUI applies `EDPROPFAMILY` (size/style shared with the mono editor font).
    EditorPropFontPicker,
    /// `UIFONTS` — request the GUI's font-picker dialog for the UI chrome (`UIFAMILY`/`UIFONT`).
    UiFontPicker,
    /// `TXTFONTS` — request the GUI's font-picker dialog for the `TXT` window (`TXTFAMILY`/`TXTFONT`).
    TxtFontPicker,
    /// `DWFONTS` — request the GUI's font-picker dialog for the `DIR/W` window (`DWFAMILY`/`DWFONT`).
    DwFontPicker,
    /// `PVFONTS` — request the GUI's font-picker dialog for the Markdown Preview/`TOPDF` BODY font
    /// (family + size). On OK the GUI applies `PVFAMILY`/`PVSIZE`.
    PreviewFontPicker,
    /// `PVCODES` — request the GUI's font-picker dialog for the Preview/`TOPDF` CODE font (monospace
    /// family). On OK the GUI applies `PVCODE`.
    PreviewCodePicker,
    /// Bare `MAN` — request the GUI to open the user manual (same as Help ▸ Manual). Headless builds
    /// have nowhere to show it, so it's a no-op there.
    ShowManual,
    /// Open (or reuse in place) the single read-only `HELP`/`MAN` results tab: `text` is the
    /// pre-formatted results body, and `emphasis` `(phrase, regex)` highlights the search term via
    /// `EMPH` (`None` = no highlight). A non-modal document tab (smooth scrolling), not a dialog; the
    /// tab keeps a fixed "Help" name so both `HELP` and `MAN` reuse the same recognizable tab.
    ShowResults { text: String, emphasis: Option<(String, bool)> },
    /// `COLORNAMES [filter]` — open the read-only results tab listing every known colour name with its
    /// `R G B` value (optionally filtered to names containing `filter`), each tinted in its own colour
    /// plus a solid swatch block so it's visible in any theme. Built from [`crate::color_names::listing`].
    ShowColorNames { filter: Option<String> },
    /// `REFPDF` (`manual = false`) / `MANPDF` (`manual = true`) — open the shipped reference / user-
    /// manual PDF in the OS default PDF application. The file is sought in `$XSDIR/docs` first, then
    /// `$XSDIR`. Headless builds still resolve + launch it (the OS opens the reader).
    OpenDocPdf { manual: bool },
    /// `DIFF [path]` — compare the active buffer against a file on disk and show a unified diff in the
    /// read-only results tab (`-` lines red, `+` green, `@@` hunk headers tinted). Bare `DIFF` compares
    /// the buffer against its OWN file; `DIFF <path>` compares it against the given file (`$CWD`-
    /// resolved). A content diff by `lines()` semantics, so CRLF-vs-LF / trailing-newline-only
    /// differences read as identical. Errors (no path / unreadable file) surface as the status string.
    Diff { path: Option<String> },
    /// `DJ` — from a line of the diff results tab, jump into the live source buffer at the corresponding
    /// line (the new/`+` side). The caret's enclosing `@@` hunk header plus the `+`/context lines above
    /// it give the target line. A no-op with a status message off a diff line or once the diff is stale.
    DiffJump,
    /// `BUNDLE [out]` / `BUNDLE/F <from> [out]` — pack a document together with its `.xymeta` sidecar
    /// into one transportable file. `from = None` bundles the active buffer (its current bytes +
    /// styling); `from = Some(path)` bundles a file on disk and its sidecar. `out` is the bundle path
    /// (default `<name>.xyb`). Returns the written path, or an error, as the status string.
    Bundle { from: Option<String>, out: Option<String> },
    /// `UNBUNDLE <path> [destdir]` — split a bundle back into the document and its `.xymeta` sidecar in
    /// `destdir` (default `$CWD`), then open the document. Refuses to overwrite an existing target.
    Unbundle { path: String, dest: Option<String> },
    /// `AR` — auto-replace: look at the word immediately behind the caret and, if it's a key in the
    /// loaded auto-replace map, replace it with the mapped text (one undo unit). No match = no-op.
    AutoReplace,
    /// `COMPILE <cmd>` — run a build command asynchronously (shelling out, TRUST-gated), streaming its
    /// output into a read-only results tab whose `file:line:col` lines become navigable hits
    /// (`GJ`/`NERR`/`PERR`). A bare `COMPILE` re-runs the last command. Reuses the `X/BG` job machinery.
    /// By default output refills the existing compile tab for the same command; `new_tab` forces a new one.
    Compile { cmd: String, new_tab: bool },
    /// `NERR` / `PERR` — step to the next / previous location in the most recent COMPILE/GREP results
    /// tab and jump to its source. `forward` = `NERR`.
    ErrorNav { forward: bool },
    /// `XSW <action>` — a window-management action (maximize/restore/move/resize/…). Queued by the
    /// headless host; the GUI runtime applies it to the OS window.
    Window(WindowCmd),
    /// `LoadAR <file>` / `AddAR <file>` — load `key=replacement` pairs from a file into the
    /// auto-replace map. `replace` clears the map first (the base, like `LOADLIB`); otherwise the
    /// pairs MERGE into what's there (augment, like `ADDLIB`). `#`/`;` comments + blank lines skipped;
    /// `\n`/`\t` escapes in the value decoded.
    LoadAutoReplace { path: String, replace: bool },
    /// `SAVEAR [file]` — write the current auto-replace map back as a `key=replacement` file (a
    /// commented sample template when the map is empty). `None` = a default path.
    SaveAutoReplace(Option<String>),
    /// The same `SAVEAR` file TEXT, returned (not written) — for `SAVEAR` under `CAPTURE`.
    SaveAutoReplaceToString,
    /// `CLEARAR` — empty the auto-replace map.
    ClearAutoReplace,
    /// `MERGE`/`ME <file>` — insert the (UTF-8 text) contents of `file` at the caret of the active
    /// buffer (or the blind-edit buffer). Path resolves against `$CWD`; a missing/unreadable file is
    /// a trappable error (`$ERROR`).
    Merge(String),
    /// `QUIET` / `QUIET/NV` / `QUIET OFF` — set the status-message quiet level for the rest of the
    /// script: `0` = off, `1` = suppress automatic op-result statuses (PR + errors still show), `2`
    /// = also suppress non-fatal (trappable) error text (`$ERROR` stays operative). Auto-resets
    /// when the script ends.
    SetQuiet(u8),
    /// `BEDIT [file]` — "blind edit": open `file` (OPEN semantics — new-named if missing) into an
    /// INVISIBLE buffer that editing commands + `$`-vars retarget to, while the GUI keeps rendering
    /// the visible document. `None` = a new unnamed blind buffer (NEW semantics). `SA`/`SAVE` writes
    /// it; `AB` exits early; otherwise it auto-exits (discarding) when the script finishes. Errors
    /// if already blind-editing (no nesting).
    BlindEdit(Option<String>),
    /// `BCLOSE [file]` — close (discard) the open blind-edit buffer, the explicit counterpart of an
    /// `AB` during a `BEDIT`. `Some(file)` asserts the open blind buffer is that file (by name) and
    /// errors on a mismatch; `None` closes whatever blind buffer is open. Errors if none is open.
    BlindClose(Option<String>),
    /// Write the current selection's bytes to a new file (`SAD <path>`), byte-exact. The
    /// active document and its path are untouched. Errors if there's no selection. When `path` is an
    /// existing file, `force=false` asks the GUI to confirm the overwrite; `force=true` (`/NV`) skips it.
    SaveSelection { path: String, force: bool },
    /// Revert the active document to its last saved state by re-reading its file
    /// (`REC`/`RECALL`). Errors if the document was never saved (no path).
    Revert,
    /// Open a second view of the current file (`DUPVIEW`), positioned at the current line. `window =
    /// false` opens it as a fresh in-process tab (a distinct buffer of the same path); `window = true`
    /// (`DUPVIEW/W`) launches it in a separate free-standing window (a second editor instance). Both
    /// stay in step on save via `AUTOREOPEN`. Errors if the file was never saved. Not TRUST-gated (it
    /// only re-opens a file you already have open, running our own editor for the `/W` window).
    DupView { window: bool },
    /// Lock/unlock the active document against edits (`RO`). Read back via `$RO`. When
    /// locked, buffer-content edits are rejected (the cmline is never locked).
    SetReadOnly(ViewToggle),
    /// Set/clear/toggle a keyboard lock key (`SCL`/`CCL`/`TCL` Caps, `SNL`/`CNL`/`TNL` Num,
    /// `SSL`/`CSL`/`TSL` Scroll). Windows synthesizes a key tap; a no-op elsewhere. Read back via
    /// `$CAPSLOCK`/`$NUMLOCK`/`$SCROLLLOCK`.
    SetLock { key: LockKey, mode: ViewToggle },
    /// `AB` — close the active buffer, discarding unsaved changes. Never leaves zero buffers
    /// (the last close opens a fresh empty one).
    CloseBuffer,
    /// `AB/V` — close, but if the buffer is modified first ask whether to save (the host arms a
    /// prompt the GUI resolves). An unmodified buffer closes immediately.
    CloseBufferVerify,
    /// `ABD` / `ABD/V` — delete the active document's backing file from disk, then close the
    /// buffer. `verify` (the `/V` switch) arms a confirm the GUI resolves; without it the delete +
    /// close happen immediately. A never-saved buffer (no file) just closes, reporting the fact.
    DeleteNote { verify: bool },
    /// `SAVECFG [path]` — write the current appearance/layout settings (colours, sizing, view
    /// toggles) as a runnable XPL config script. `None` = a default config file.
    SaveCfg(Option<String>),
    /// The same `SAVECFG` script TEXT, returned (not written) — for `SAVECFG` under `CAPTURE`.
    SaveCfgToString,
    /// `LOADCFG [path]` — load + run a config script written by `SAVECFG` (it's just XPL, run
    /// after the current script returns). `None` = the default config file.
    LoadCfg(Option<String>),
    /// `LANG <path>` — load a UI string table (translations) from a flat `key = value` file so the
    /// GUI can render menu/dialog labels in another language. `None` (bare `LANG` / `LANG off`)
    /// clears it, reverting to the built-in English. i18n phase-1 spike.
    LoadLang(Option<String>),
    /// `AUTOCFG [path]` / `AUTOCFG off` — live config tracking ("self-updating SAVECFG"): rewrite
    /// the config file whenever a setting changes. `on=false` stops; `on=true` with `path=None`
    /// uses the default config file. Settings only (no command history). Read back via `$ACF`.
    ConfigTrack { on: bool, path: Option<String> },
    /// `AUTOSAVE <secs>` — set the autosave interval in seconds (0 = off). Read back via
    /// `$AU`. The GUI does the timing and the periodic save.
    SetAutosave(u64),
    /// `TB <n>` — set the tab width in spaces (`>= 1`). Read back via `$TB`.
    SetTabWidth(usize),
    /// `TAB` (bare / `in`) indents, `TAB out` outdents. `out=false` with no selection inserts one
    /// indent unit at the caret; otherwise the touched lines are indented/outdented at their starts.
    Tab { out: bool },
    /// `TABSPACES on|off` — soft tabs: `TAB`/indent inserts `$TB` spaces instead of a tab character.
    /// Read back via `$TABSPACES`.
    SetTabSpaces(bool),
    /// `TABINDENT on|off` — auto-indent: pressing `ENTER` starts the new line at the same indentation
    /// as the line above. Read back via `$TABINDENT`. (`$INDENTED` reads the current line's indent.)
    SetTabIndent(bool),
    /// `WRAPINDENT on|off` — hanging indent: soft-wrapped continuation rows of a line align under that
    /// line's own indentation (`$INDENTED`) instead of the left margin. Read back via `$WRAPINDENT`.
    SetWrapIndent(bool),
    /// `DECIMALS <n>` / `DECIMALS off` — fixed decimal places for displaying floats (`None` = off,
    /// default precision). Read back via `$DECIMALS`; persisted by SaveCfg.
    SetDecimals(Option<u8>),
    /// `DOCTYPE [typst|gfm|auto]` — set (or query) how the active document renders to PDF/preview.
    /// The host returns a status reporting the effective kind. Read back via `$DOCTYPE`.
    DocType(DocTypeReq),
    /// `MU`/`MD` — scroll the viewport by `rows` visual (wrap-aware) lines WITHOUT moving the
    /// caret. Positive = viewport toward the document end (text moves up); negative = toward
    /// the start (text moves down). The GUI accumulates and applies it.
    ScrollView(i32),
    /// `X/BG <cmd>` — launch a shell command as a background job (a worker thread); the script
    /// continues immediately. Output streams to `*Shell Output*`; exit sets `$XO`/`$RC`. Listed
    /// by `JOBS`, counted by `$JOBS`. Trust-gated like the other shell verbs. `return_var`
    /// (`X/BG/R:<var>`) names a parent variable to receive the job's full output on completion.
    ShellBg { cmd: String, return_var: Option<String> },
    /// `KILL <id>` — terminate the running background job with that id.
    Kill(u64),
    /// `SL` — save every named, modified buffer; unnamed (never-saved) buffers are skipped
    /// and left open. Reports how many were saved and skipped.
    SaveAll,
    /// `SF` — save all (like `SL`), then close every buffer that is now clean, leaving any
    /// still-modified (unnamed) buffers open and at least one buffer overall.
    SaveCloseAll,
    /// Switch the active document's encoding view (`ENCODING <name>`). The bytes
    /// are untouched — a live EditPad-style reinterpret.
    SetEncoding(String),
    /// CONVERT the active document to charset `name` (`ENCODING/C`): re-encode the text (lossy → `?`).
    /// `out_file = None` converts the current doc in place; `Some(path)` writes a converted copy to
    /// `path` and opens it (current doc untouched).
    ConvertEncoding { name: String, out_file: Option<String> },
    /// Convert the active document's line endings to `style` and adopt it (`NEWLINE lf|crlf|cr`).
    /// Lossless. CR is stored LF-internally and reconstituted on save (see `NewlineStyle`).
    SetNewlineStyle(NewlineStyle),
    /// Show/hide the OS window title bar (`TITLEBAR on|off`; `None` toggles). The runtime applies it
    /// via `set_decorations`; read back via `$TITLEBAR`.
    SetTitlebar(Option<bool>),
    /// Writer's View (`WV on|off`; `None` toggles): a distraction-free mode where the GUI hides all
    /// chrome — title bar, menu bar, tab strip, command line, and status line — leaving just the text.
    /// Transient (not persisted); `Esc` turns it off. Read back via `$WV`.
    SetWriterView(Option<bool>),
    /// `EMPH <phrase>` sets the active tab's persistent emphasis phrase (all occurrences are
    /// highlighted, case-insensitively); `None` (bare `EMPH`) clears it. `regex` (from `EMPH/R`)
    /// matches the phrase as a regular expression instead of a whole-word literal. Per-document,
    /// session-only, works in read-only / `*Shell Output*` tabs (a visual overlay, not an edit).
    SetEmph { phrase: Option<String>, regex: bool },
    /// `EMPHFGCOLOR <r g b>` / `default` — the `EMPH` emphasis glyph colour (`None` = follow the
    /// selected-text colour). `$EMPHFGCOLOR`.
    SetEmphFgColor(Option<(u8, u8, u8)>),
    /// `EMPHBGCOLOR <r g b>` / `default` — the `EMPH` emphasis background band colour (`None` = follow
    /// the selection background). `$EMPHBGCOLOR`.
    SetEmphBgColor(Option<(u8, u8, u8)>),
    /// `SUCOLOR <r g b>` / `default` — the on-screen colour for `MDA SU` superscript text (a display
    /// convention; `SU` renders as real superscript only in preview/PDF/HTML). `None` = the built-in
    /// default. `$SUCOLOR`.
    SetSuColor(Option<(u8, u8, u8)>),
    /// `SDCOLOR <r g b>` / `default` — the on-screen colour for `MDA SD` subscript text. `None` = the
    /// built-in default. `$SDCOLOR`.
    SetSdColor(Option<(u8, u8, u8)>),
    /// Toggle whether the GUI remembers the main window's size + position across runs
    /// (`WINGEO on|off`; `None` toggles). The runtime gates both restore and save on it; `$WINGEO`.
    SetRememberGeometry(Option<bool>),
    /// Toggle whether the command line is BLANKED after a command runs from it (`CMBLANK on|off`;
    /// `None` toggles). Off (default) keeps the command (XyWrite B23). `$CMBLANK`.
    SetCmlineBlank(Option<bool>),
    /// Toggle whether the tab strip is shown when only ONE file is open (`SHOWONETAB on|off`;
    /// `None` toggles). Off (default) hides the strip for a single doc. `$SHOWONETAB`.
    SetShowOneTab(Option<bool>),
    /// Set Left/Right caret-motion order (`CARETMOTION logical|visual`). `true` = VISUAL (screen order:
    /// the arrows follow the on-screen layout, so in RTL text they track the reading flow); `false` =
    /// LOGICAL (storage order, the default). Visual is refined by the GUI via cosmic's layout; headless
    /// keeps the logical move.
    SetCaretMotion(bool),
    /// Toggle line wrapping (`WRAP on|off`; `None` toggles). On (default) wraps long lines to the
    /// window; off lets them run past the right edge, with a horizontal scrollbar. `$WRAP`.
    SetWrap(Option<bool>),
    /// Hanging indent for wrapped continuation rows, in spaces (`WRAPHANG <n>`; 0 = flush, the default).
    /// Applies only when `WRAP` is on and `WRAPINDENT` is off. `$WRAPHANG`.
    SetWrapHang(usize),
    /// The default column width `REFLOW` uses when given no explicit width (`REFLOWWIDTH <n>`;
    /// default 65). `$REFLOWWIDTH`; persisted by `SAVECFG`.
    SetReflowWidth(usize),
    /// Reflow (hard-wrap) text to `width` columns: within each blank-line-delimited paragraph,
    /// collapse internal whitespace and greedily re-wrap the words so no line exceeds `width`
    /// columns (never splitting a word); the first line's indent is applied to every line, and
    /// blank-line paragraph breaks are preserved. Acts on the selection, else the paragraph at the
    /// caret. One undo unit. `REFLOW [n]`.
    Reflow { width: usize },
    /// Toggle whether the pristine "Untitled" scratch tab is auto-closed when a document is opened
    /// (`UNTITLEDTAB on|off`; `None` toggles). On (default) closes it, and prunes any leftover empty
    /// untitled tabs, so a blank scratch never lingers beside real files; it reappears when the last
    /// document is closed. Off keeps the untitled tab as its own tab. `$UNTITLEDTAB`.
    SetUntitledTab(Option<bool>),
    /// Toggle whether opening an already-open file skips the "already open" prompt and just switches
    /// to the existing tab (`GOTOOPEN on|off`; `None` toggles). Off (default) keeps the prompt.
    /// `$GOTOOPEN`.
    SetGotoOpen(Option<bool>),
    /// Auto-reopen a file when it changes on disk (`AUTOREOPEN on|off`; `None` toggles). When on, an
    /// open file that another program modifies is reloaded automatically (only when it has no unsaved
    /// edits — otherwise the change is noted and your edits are kept). Off by default; independent of
    /// `TRUST` (no external code runs — the editor only re-reads a file you already have open).
    /// Persisted by SaveCfg. `$AUTOREOPEN`.
    SetAutoReopen(Option<bool>),
    /// Set the auto-update policy (`UPDATEMODE off|alert|download|install`). Persisted by SaveCfg;
    /// `$UPDATEMODE`. Pure state — it never itself touches the network (see [`UpdateMode`]).
    SetUpdateMode(UpdateMode),
    /// Run a manual auto-update step (`UPDATE [CHECK|DOWNLOAD]`). `Check` reports whether a newer
    /// version is available (`$UPDATEAVAIL`); `Download` fetches + verifies the new bundle into staging
    /// (`$UPDATESTAGED`). Behind the `update` cargo feature — a build without it reports as much.
    Update(UpdateAction),
    /// Error logging on/off (`ERRORLOG on|off`; `None` toggles). When on, each trappable *and* fatal
    /// error is appended as a TSV row to the error-log file. Off by default. Persisted by SaveCfg.
    /// `$ERRORLOG`.
    SetErrorLog(Option<bool>),
    /// Set the error-log file path (`ERRORLOGFILE <path>`; bare/empty resets to the default,
    /// `xyscript-errors.log` in `$XSDIR`). A given path resolves against `$CWD`. Persisted by
    /// SaveCfg. `$ERRORLOGFILE`.
    SetErrorLogFile(String),
    /// Dim the window slightly when it loses focus (`DIMONBLUR on|off`; `None` toggles). A cue that the
    /// window is inactive — useful with `TITLEBAR off`, where the frame gives none. Off by default.
    /// Persisted by SaveCfg. `$DIMONBLUR`.
    SetDimOnBlur(Option<bool>),
    /// Whether closing a tab (`AB`) returns to the LAST-VISITED tab rather than the left neighbor
    /// (`CLOSEBACK on|off`; `None` toggles). Off by default. Persisted by SaveCfg. `$CLOSEBACK`.
    SetCloseBack(Option<bool>),
    /// How the bookmark gutter is shown (`MARKGUTTER off|on|persist`; `None` toggles off↔on). `On` by
    /// default. Persisted by SaveCfg. `$MARKGUTTER`.
    SetMarkGutter(Option<MarkGutterMode>),
    /// Set the bookmark-gutter dot colour (`fg = true`, `MARKFGCOLOR`) or the gutter strip background
    /// (`fg = false`, `MARKBGCOLOR`). `None` = default (fg = bookmark orange; bg = transparent). Read
    /// back via `$MARKFGCOLOR`/`$MARKBGCOLOR`.
    SetMarkColor { fg: bool, color: Option<(u8, u8, u8)> },
    /// Open the colour picker for the bookmark-gutter dot (`MARKFGCOLORS`) or strip bg (`MARKBGCOLORS`).
    OpenMarkColorPicker { fg: bool },
    /// Set the line-number gutter's text colour (`fg = true`, `LNFGCOLOR`) or background colour
    /// (`fg = false`, `LNBGCOLOR`). `None` = default (fg = a muted slate; bg = transparent). Read back
    /// via `$LNFGCOLOR`/`$LNBGCOLOR`.
    SetLineNumColor { fg: bool, color: Option<(u8, u8, u8)> },
    /// Open the colour picker for the line-number text (`LNFGCOLORS`) or gutter bg (`LNBGCOLORS`).
    OpenLineNumColorPicker { fg: bool },
    /// Set one of the modal `MSG` box colours (`MSGHFGCOLOR`/`MSGBGCOLOR`/`MSGOKFGCOLOR`/
    /// `MSGOKBGCOLOR`). `None` = default (follow the theme). Read back via the matching `$…` special.
    SetMsgColor { slot: MsgColorSlot, color: Option<(u8, u8, u8)> },
    /// Open the colour picker for one of the MSG box colours (`MSGHFGCOLORS`/`MSGBGCOLORS`/
    /// `MSGOKFGCOLORS`/`MSGOKBGCOLORS`).
    OpenMsgColorPicker { slot: MsgColorSlot },
    /// Turn URL highlighting on/off (`URLHIGHLIGHT on|off`; `None` toggles). When on, URLs in the
    /// document are coloured (`URLFGCOLOR`/`URLBGCOLOR`) and Ctrl+click on one opens it in the OS
    /// default application. Off by default. Persisted by SaveCfg. `$URLHIGHLIGHT`.
    SetUrlHighlight(Option<bool>),
    /// Turn preview/`TOPDF` mark rendering on/off (`PVMARKS on|off`; `None` toggles). When on, the
    /// EMPH matches and URL highlights are painted into the preview/PDF (all styler-based kinds), like
    /// the editor overlay; MDA styling is always rendered regardless. Off by default. SaveCfg. `$PVMARKS`.
    SetPreviewMarks(Option<bool>),
    /// Set the URL-highlight text colour (`fg = true`, `URLFGCOLOR`) or background (`fg = false`,
    /// `URLBGCOLOR`). `None` = default (fg = a link blue; bg = transparent, no band). Read back via
    /// `$URLFGCOLOR`/`$URLBGCOLOR`.
    SetUrlColor { fg: bool, color: Option<(u8, u8, u8)> },
    /// Open the colour picker for the URL-highlight text (`URLFGCOLORS`) or background (`URLBGCOLORS`).
    OpenUrlColorPicker { fg: bool },
    /// Open the colour picker for the xlink-highlight text (`XLFGCOLORS`) or background (`XLBGCOLORS`).
    OpenXlinkColorPicker { fg: bool },
    /// Turn xlink highlighting on/off (`XLINKHIGHLIGHT on|off`; `None` toggles). When on, xlink spans
    /// in the document are coloured (`XLFGCOLOR`/`XLBGCOLOR`) and Ctrl+click on one activates it. On by
    /// default. Persisted by SaveCfg. `$XLINKHIGHLIGHT`.
    SetXlinkHighlight(Option<bool>),
    /// Set the xlink-highlight text colour (`fg = true`, `XLFGCOLOR`) or background (`fg = false`,
    /// `XLBGCOLOR`). `None` = default (fg = a link colour; bg = transparent, no band). Read back via
    /// `$XLFGCOLOR`/`$XLBGCOLOR`.
    SetXlinkColor { fg: bool, color: Option<(u8, u8, u8)> },
    /// Toggle whether `MDA` UNDERLINE styling underlines the spaces between words (`ULSPACES on|off`;
    /// `None` toggles). On (default) draws one continuous underline; off breaks it at spaces/tabs so
    /// only word glyphs are underlined. `$ULSPACES`.
    SetUlSpaces(Option<bool>),
    /// `SMTPURL`/`MAILUSER`/`MAILFROM`/`MAILPASS <value>` — set one SMTP config field (`field` is
    /// `url`|`user`|`from`|`pass`). Session-only; the password is never persisted. (email feature.)
    #[cfg(feature = "email")]
    MailSet { field: String, value: String },
    /// `SENDMAIL [<to> ["subject"]]` — send the active document as an email by shelling out to `curl`
    /// (a background job). Headers come from the args or a header block atop the buffer. With `/F`,
    /// `body_file` is read for the message text instead of the active document. With `/W` the verb runs
    /// SYNCHRONOUSLY — it blocks until curl finishes so `$RC` is set before it returns. (email feature.)
    #[cfg(feature = "email")]
    SendMail { to: Option<String>, subject: Option<String>, body_file: Option<String>, wait: bool },
    /// `ATTACH <path>` — stage a file to attach to the next `SENDMAIL` (validated + resolved now, read
    /// at send time). (email feature.)
    #[cfg(feature = "email")]
    MailAttach(String),
    /// `CLEARATTACH` — discard the staged attachment list. (email feature.)
    #[cfg(feature = "email")]
    MailClearAttach,
    /// `LISTMAIL` — FETCH message headers (Date/From/Subject + flags) from the mailbox and stream them
    /// to *Shell Output*, via `curl` IMAP. With `/W` it blocks until the fetch finishes (sets `$RC`
    /// synchronously). (email feature.)
    #[cfg(feature = "email")]
    ListMail { wait: bool },
    /// `SAVEMAIL <range> [<path>]` — download the raw messages in `range` (`1-3,6-8,10`) to
    /// `MSG<nnn>.TXT` files (named by message number) in `path` (default `$CWD`). `/W` blocks until the
    /// download finishes (sets `$RC` synchronously). (email feature.)
    #[cfg(feature = "email")]
    SaveMail { range: String, path: Option<String>, wait: bool },
    /// `DELMAIL <range>` — mark the messages in `range` (`1-3,6-8` or `all`) `\Deleted` and expunge
    /// them, via `curl` IMAP. `/W` blocks until the delete finishes (sets `$RC` synchronously). (email
    /// feature.)
    #[cfg(feature = "email")]
    DelMail { range: String, wait: bool },
    /// Set the tab close-`×` glyph colour (`TABXCOLOR <r g b>`; `None` = default to the UI foreground).
    /// Read back via `$TABXCOLOR`.
    SetTabXColor(Option<(u8, u8, u8)>),
    /// `BALPAIRS on|off` — highlight the matching bracket of the `()[]{}` at/adjacent to the caret,
    /// nesting-aware; an unbalanced one is reported on the status line. Read back via `$BALPAIRS`.
    SetBalPairs(bool),
    /// Set the matched-bracket highlight colour (`BALPAIRSCOLOR <r g b>`; `None` = default to the
    /// selection colour). Read back via `$BALPAIRSCOLOR`.
    SetBalColor(Option<(u8, u8, u8)>),
    /// `TYPEPAIRS on|off` — typing a left `()[]{}"'` auto-inserts the partner (over-typing the closer,
    /// wrapping a selection, and Backspace-deleting an empty pair). Read back via `$TYPEPAIRS`.
    SetTypePairs(bool),
    /// Set the `$CWD` path-display foreground (`fg = true`, `PAFGCOLOR`) or background (`fg = false`,
    /// `PABGCOLOR`) colour. `None` = default (fg follows the dimmed UI text; bg is transparent). Read
    /// back via `$PAFGCOLOR`/`$PABGCOLOR`.
    SetPathColor { fg: bool, color: Option<(u8, u8, u8)> },
    /// Open the colour picker for the `$CWD` path-display fg (`PAFGCOLORS`) or bg (`PABGCOLORS`).
    OpenPathColorPicker { fg: bool },
    /// Set the preview-pane text (`fg = true`, `PVFGCOLOR`) or background (`fg = false`, `PVBGCOLOR`)
    /// colour. `None` = Typst default (black text / white paper). PREVIEW ONLY — never applied to
    /// `TOPDF`/print output. Read back via `$PVFGCOLOR`/`$PVBGCOLOR`.
    SetPreviewColor { fg: bool, color: Option<(u8, u8, u8)> },
    /// Open the colour picker for the preview-pane fg (`PVFGCOLORS`) or bg (`PVBGCOLORS`).
    OpenPreviewColorPicker { fg: bool },
    /// Set the scratchpad-window foreground (`fg = true`, `SCRATCHFGCOLOR`) or background
    /// (`fg = false`, `SCRATCHBGCOLOR`) colour. `None` = default (egui's theme). `$SCRATCHFGCOLOR`/
    /// `$SCRATCHBGCOLOR`.
    SetScratchColor { fg: bool, color: Option<(u8, u8, u8)> },
    /// Set the `DIR/W`-window foreground (`fg = true`, `DWFGCOLOR`) or background (`fg = false`,
    /// `DWBGCOLOR`) colour. `None` = unset, so the window inherits the editor's colour. `$DWFGCOLOR`/
    /// `$DWBGCOLOR` report the *effective* (override-else-editor) colour.
    SetDwColor { fg: bool, color: Option<(u8, u8, u8)> },
    /// Open the colour picker for the scratchpad fg (`SCRATCHFGCOLORS`) or bg (`SCRATCHBGCOLORS`).
    OpenScratchColorPicker { fg: bool },
    /// Turn syntax highlighting on/off for the active document (`HIGHLIGHT
    /// on|off`). Off by default (a quiet screen).
    SetHighlight(bool),
    /// Show the active document as a hex + text grid (`HEX on|off`; bare `HEX` toggles). A canvas
    /// view mode; the bytes are unchanged. `None` toggles. Off by default. Read back via `$HEX`.
    SetHexView(Option<bool>),
    /// Bookmarks (`MARK*`): set / clear / clear-all / toggle a bookmark at the caret, or move the
    /// caret to the next / previous one (circular). Persisted per-document in the `.xymeta` sidecar;
    /// `$MARKS` counts them and `$MARK` reports whether the caret is on one.
    /// `MARKSET [name]` — set a bookmark at the caret, optionally NAMED (so `GOMARK <name>` / an
    /// `XLINK/M` link can target it). `None` = an unnamed positional bookmark, as before.
    MarkSet { name: Option<String> },
    MarkClear,
    MarkClearAll,
    MarkToggle,
    MarkNext,
    MarkPrev,
    /// `GOMARK <name>` — move the caret to the named bookmark (centered). A trappable error when no
    /// bookmark has that name.
    MarkGoto { name: String },
    /// `GOTOLINE <n>` — move the caret to the start of 1-based line `n` (clamped) and centre it. A
    /// plain navigation primitive; also the target a nav `XLINK/L` compiles to.
    GotoLine(usize),
    /// `XLINK <payload>` (or `XLINK/L …`) — create an xlink over the current SELECTION carrying
    /// `payload` (raw XPL, stored verbatim; run on activation), optionally NAMED (`XLINK/N <name> …`,
    /// so `XLGO <name>` can activate it). The host reads the selection range. No selection, an empty
    /// range, or an overlap with an existing xlink is a trappable error.
    XlinkCreate { payload: String, name: Option<String> },
    /// `XLDEL` / `XLDEL/A` — remove the xlink under the caret, or every xlink in the document.
    XlinkDelete { all: bool },
    /// `XLINK/E <payload>` — replace the payload of the xlink under the caret (keeping its span). A
    /// trappable error when the caret isn't on an xlink.
    XlinkEdit { payload: String },
    /// `XLHERE` — mark the caret as the destination an `XLINK/H` link will point at: ensure a named
    /// bookmark here and stash it (session-only). The "copy link to this location" capture.
    XlinkHere,
    /// `XLINK/H` — create an xlink over the SELECTION pointing at the `XLHERE` capture (compiles to a
    /// `GOMARK` of the captured bookmark, cross-document via `OPEN` when needed), optionally NAMED.
    XlinkFromCapture { name: Option<String> },
    /// `XLTRUST` (`trust = true`) / `XLUNTRUST` (`trust = false`) — trust or revoke the active document
    /// for running ACTION (non-navigation) xlink payloads. Trust is keyed by document path in a
    /// user-global ledger (session-only for a path-less buffer). `$XLTRUSTED`.
    SetXlinkTrust { trust: bool },
    /// `XLTRUST/A on|off` (bare toggles): author mode. While on, an author verb that changes a payload
    /// re-affirms session trust for the active doc, so editing-then-testing an action xlink doesn't
    /// re-prompt. `Some(true/false)` sets, `None` toggles. Session-only. `$XLTRUSTAUTHOR`.
    SetXlinkAuthor(Option<bool>),
    /// `XLNEXT` (`forward = true`) / `XLPREV` — move the caret to the next / previous xlink's start,
    /// wrapping at the ends, and centre it. A no-op (with a note) when the document has no xlinks.
    XlinkJump { forward: bool },
    /// `XLFWD` (`forward = true`) / `XLBACK` — walk the xlink JUMPLIST: go to the next forward / back
    /// location (where following links took you), centred. A no-op (with a note) when there's none.
    XlinkHistory { forward: bool },
    /// HEX-view editing (raw bytes, byte-granular; each is one recorded undo unit). These mutate the
    /// buffer but are EXEMPT from the hex read-only gate (they are the intended hex editing) while
    /// still blocked by a genuine `RO` lock. See the `hex_*` document methods.
    ///
    /// Set the hi (`hi = true`) or lo nibble of the byte at the caret to `val` (`0x0..=0xF`).
    HexSetNibble { hi: bool, val: u8 },
    /// Overwrite the whole byte at the caret (text-column entry / single-byte paste).
    HexOverwrite(u8),
    /// Insert bytes at the caret (byte insert; usually a single `0x00`).
    HexInsert(Vec<u8>),
    /// Delete one byte: at the caret (`before = false`) or just before it (`before = true`).
    HexDelete { before: bool },
    /// Paste bytes at the caret in overwrite mode (keeps file size stable), caret after the run.
    HexPaste(Vec<u8>),
    /// Delete the selected byte range (hex cut); caret lands at the range start. No-op without one.
    HexDeleteSelection,
    /// Set/clear/toggle a GUI view flag (`SMB`/`CMB`/`TMB` menu bar, `SVR`/`CVR`/`TVR`
    /// visible returns ¶, `SLN`/`CLN`/`TLN` line-number gutter). Read back via `$MB`/
    /// `$VR`/`$LNS`. The host holds the flag; the GUI renders it.
    SetView { flag: ViewFlag, kind: ViewToggle },
    /// Set a GUI sizing knob to `value` (`UISCALE`/`UIFONT`/`EDFONT`/`LEADING`(`LSP`)/`OF`).
    /// Read back via `$UISCALE`/`$UIFONT`/`$EDFONT`/`$LSP`/`$OF`. The host holds it (clamped);
    /// the GUI applies it. When `relative` is set (the verb's argument began with an explicit
    /// `+`/`-`, e.g. `EDFONT +2`), `value` is ADDED to the knob's current value rather than
    /// replacing it; for a `-` argument `value` is already negative, so the same add applies.
    SetUiSize { kind: UiSizeKind, value: f32, relative: bool },
    /// `EDFAMILY <name>` — set the editor canvas font family by name (must be a monospaced family;
    /// `""`/`default` resets to the built-in default). Read back via `$EDFAMILY`. The GUI renders
    /// with it via cosmic-text. (The `EDFONTS` picker is GUI sugar over this + `EDFONT`.)
    SetEditorFontFamily(String),
    /// `EDPROPFAMILY <name>` — set the editor's PROPORTIONAL (variable-width) canvas font family by
    /// name (any family; `""`/`default` clears it). Prose buffers render in it when one is set and the
    /// per-document font mode resolves to proportional (see [`EditorOp::SetFontMode`]); code, listings,
    /// and the HEX view always stay monospace (`EDFAMILY`). Read back via `$EDPROPFAMILY`.
    SetEditorPropFamily(String),
    /// `USEFONT mono|prop|auto` — override this document's canvas font MODE. `auto` follows the rule
    /// (syntax-highlighted / listing / hex buffers → monospace, prose → proportional); `mono`/`prop`
    /// pin it. Structural buffers (listings, HEX, results, console) ignore a `prop` override. Session-
    /// only, per document. Bare `USEFONT` reports; `$USEFONT` reads it.
    SetFontMode(FontMode),
    /// `UIFAMILY <name>` — set the UI-chrome font family (menu/status/cmline) by name
    /// (`""`/`default` resets). Read back via `$UIFAMILY`. The GUI loads the face bytes into egui
    /// to apply it. (The `UIFONTS` picker is GUI sugar over this + `UIFONT`.)
    SetUiFontFamily(String),
    /// `TXTFAMILY <name>` — set the `TXT` window font family by name (`""`/`default` resets). Read
    /// back via `$TXTFAMILY`. The GUI loads the face bytes into the TXT window's egui context.
    /// (The `TXTFONTS` picker is GUI sugar over this + `TXTFONT`.)
    SetTxtFontFamily(String),
    /// `EDSTYLE`/`UISTYLE`/`TXTSTYLE <normal|bold|italic|bolditalic>` — set the editor canvas / UI
    /// chrome / `TXT` window font style. Read back via `$EDSTYLE`/`$UISTYLE`/`$TXTSTYLE`. The GUI
    /// maps it to a face weight/slant (UI/TXT styles need the matching `*FAMILY` set to load a face).
    SetEditorFontStyle(String),
    SetUiFontStyle(String),
    SetTxtFontStyle(String),
    /// `DWFAMILY <name>` / `DWSTYLE <…>` — set the `DIR/W` window's font family / style. An
    /// empty/`default` family reverts to inheriting `EDFAMILY`. `$DWFAMILY`/`$DWSTYLE`.
    SetDwFontFamily(String),
    SetDwFontStyle(String),
    /// `PVFAMILY <name>` / `PVCODE <name>` — set the Markdown Preview/`TOPDF` body / code font
    /// family by name (`""`/`default` resets to Typst's default). `$PVFAMILY`/`$PVCODE`. The
    /// `PVFONTS`/`PVCODES` pickers are GUI sugar over these (+ `PVSIZE`).
    SetPreviewFontFamily(String),
    SetPreviewCodeFamily(String),
    /// Reset all GUI sizing knobs to their defaults (`UIRESET`).
    UiReset,
    /// `DWRESET` — reset the `DIR/W` window's appearance overrides (colours, font, family, style,
    /// scale) back to inheriting the editor / the defaults.
    DwReset,
    /// Set the editor's foreground (text) or background colour to an RGB triple
    /// (`FGCOLOR`/`BGCOLOR`). Read back via `$FGCOLOR`/`$BGCOLOR`. The host holds it; the
    /// GUI rasters with it.
    SetColor { ground: ColorGround, rgb: (u8, u8, u8) },
    /// `FGCOLOR` with selection-awareness: on a selection, colour just the selected text (a
    /// transient span); with no selection, set the global editor foreground. `None` = `FGCOLOR
    /// default` (clear the selection's colour, or reset the global to its default). `$FGCOLOR`
    /// still reads the global.
    FgColor(Option<(u8, u8, u8)>),
    /// `MDA <color>` / `MDA/B <color>` — colour the SELECTION as a persistent block (XyWrite "mode"):
    /// foreground when `background` is false, background when true. `rgb == None` (`MDA off`) clears
    /// that layer over the selection. Selection-only (a no-op with a note otherwise). The spans
    /// follow edits and persist to the doc's `.xymeta` sidecar.
    ModeColor { background: bool, rgb: Option<(u8, u8, u8)> },
    /// `MDA BO|IT|BI|UL|BU` — apply persistent TEXT ATTRIBUTES (bold / italic / underline, a bitset of
    /// `ATTR_*`) to the SELECTION as an independent block layer; `attrs == None` (`MDA NM`) strips them.
    /// Selection-only, edit-following, persisted to the `.xymeta` sidecar's `[attrs]` section.
    ModeAttr { attrs: Option<u8> },
    /// `MDALL <color> <pattern>` / `MDALL/B` — the bulk analogue of [`EditorOp::ModeColor`]: stamp the
    /// foreground (or, with `background`, the background) colour onto EVERY match of `find`, as
    /// persistent MDA spans (a one-time snapshot bake — the spans then follow edits / persist / print
    /// like any MDA styling, but do not re-match). `rgb == None` (`MDALL off`) clears the layer over the
    /// matches — and, for the foreground, its attributes too (mirrors bare `MDA`).
    ModeAllColor { background: bool, rgb: Option<(u8, u8, u8)>, find: MatchQuery },
    /// `MDALL BO|IT|… <pattern>` — the bulk analogue of [`EditorOp::ModeAttr`]: apply TEXT ATTRIBUTES to
    /// every match of `find`; `attrs == None` (`MDALL NM`) strips them over the matches.
    ModeAllAttr { attrs: Option<u8>, find: MatchQuery },
    /// `HLCOLOR <kind> <r g b>` — set the syntax-highlight colour for a token kind (by name).
    /// `color = None` (`HLCOLOR <kind> default`) clears it → that kind renders in `FGCOLOR`.
    /// ANY kind name is accepted (2026-06-18 decision: arbitrary string kinds) — a built-in name
    /// maps to its variant, anything else to a grammar-defined `Other` kind. So a colour may be set
    /// before (or without) a grammar emitting that kind; it is simply inert until an `HLRULE`
    /// produces tokens of it. The GUI rasters tokens with these.
    /// `HLCOLOR[/BG] <kind> <r g b|default>` — set (or clear, `None`) a token kind's foreground colour,
    /// or its BACKGROUND band when `bg` (the `/BG` switch).
    SetHighlightColor { kind: String, color: Option<(u8, u8, u8)>, bg: bool },
    /// `HLMODE <kind> <BO|IT|BI|UL|BU|RV|NM>` — set (or clear, `None` = `NM`) a token kind's persistent
    /// TEXT ATTRIBUTES (bold / italic / underline / reverse, an `ATTR_*` bitset), the complement of
    /// `HLCOLOR` for a highlighting rule.
    SetHighlightMode { kind: String, bits: Option<u8> },
    /// `HLRESET` — restore the built-in default highlight theme.
    ResetHighlightTheme,
    /// `HLLANG <name> <.ext1,.ext2>` — declare (or RESET, so re-running a grammar script is
    /// idempotent) a regex-grammar highlighter named `name`, bound to the given file extensions,
    /// and make it the target for subsequent `HLRULE`s. `exts` are extension tokens (leading
    /// dots/commas tolerated; lowercased by the host).
    DefineHighlightLang { name: String, exts: Vec<String> },
    /// `HLRULE[/P] <kind> <regex>` — append a `pattern → kind` rule to the grammar `HLLANG` last
    /// declared. `kind` is any name (built-in or grammar-defined); `regex` is matched per line. `perl`
    /// (the `/P` switch) selects the `fancy-regex` engine (look-around, backreferences, atomic groups)
    /// instead of the linear-time default. No current grammar, or a regex that does not compile, is a
    /// trappable error.
    AddHighlightRule { kind: String, regex: String, perl: bool },
    /// `HLAUTO on|off` — when on, opening a file whose type a grammar/Markdown claims auto-enables
    /// highlighting (default off: the quiet-screen default). Persisted by `SAVECFG`. `$HLAUTO`.
    SetHighlightAuto(bool),
    /// `THEME <name>` — apply a built-in colour scheme (`light`/`dark`): a one-shot macro that sets
    /// the canvas, UI chrome, separators, caret, selection, `TXT` dialog, and the highlight palette
    /// together. The individual colours are what persists (`SAVECFG`). Unknown name = trappable.
    ApplyTheme(String),
    /// `TABBGCOLOR <state> <r g b>` — set the background colour of a tab title in the GUI tab strip,
    /// per interaction `state` (`active` | `hover` | `inactive`). `color = None` (`… default`) clears
    /// it back to egui's theme colour. Unknown state = trappable.
    SetTabBgColor { state: String, color: Option<(u8, u8, u8)> },
    /// `FGCOLORS`/`BGCOLORS`/`UIFGCOLORS`/… — request the GUI's colour-picker dialog for a colour
    /// target. On OK the GUI applies the matching `*COLOR` setter. Headless hosts record it.
    OpenColorPicker(ColorGround),
    /// `HLCOLORS[/BG] <kind>` — request the colour-picker dialog for a syntax-highlight token kind's
    /// foreground, or its BACKGROUND band when `bg` (the `/BG` switch).
    OpenHighlightColorPicker { name: String, bg: bool },
    /// `TABBGCOLORS <state>` — request the colour-picker dialog for a tab-title background state
    /// (`active`/`hover`/`inactive`). On OK the GUI applies `TABBGCOLOR <state> r g b`.
    OpenTabColorPicker(String),
    /// Insert a newline in the active document's line-ending style (`ENTER`). The host knows
    /// the style (`\n` vs `\r\n`); read it back via `$NEWLINE` (`LF`/`CRLF`).
    Newline,
    /// Run a counter `C1`..`C9` (`idx` 0-based). `arg` is the substituted argument: empty = put +
    /// advance; `+n`/`-[n]`/`=` = relative put; anything else = configure from a seed (no output).
    /// Per-document, session-only. Peek the current value with `$C1`..`$C9`.
    Counter { idx: u8, arg: String },
    /// `PRINTEX` `|Cn …|` counter field: run counter `C1`..`C9` (`idx` 0-based, `arg` substituted,
    /// same grammar as [`Self::Counter`]) and RETURN its rendered text instead of inserting it, so
    /// the expander can place the value in the output. Operates on the snapshot taken by
    /// [`Self::SaveCounters`]; a configure-only seed returns an empty string.
    CounterField { idx: u8, arg: String },
    /// `PRINTEX` bracket: snapshot the active document's `C1..C9` registers so a run can advance
    /// them (via [`Self::CounterField`]) without permanently changing the template's counters.
    SaveCounters,
    /// `PRINTEX` bracket: restore the registers snapshotted by [`Self::SaveCounters`] (a no-op if
    /// none was taken). Run when a `PRINTEX` ends — including on error — so counter state can't leak.
    RestoreCounters,
    /// Set the text-caret shape (`CRSHAPE`). Read back via `$CRSHAPE`.
    SetCaretShape(CaretShape),
    /// Set the live language-check underline STYLE (`SPELLSTYLE`/`GRAMMARSTYLE`). `grammar` selects
    /// which category (false = spelling, true = grammar). Read back via `$SPELLSTYLE`/`$GRAMMARSTYLE`.
    SetLintStyle { grammar: bool, style: UnderlineStyle },
    /// Set the GFM Preview/`TOPDF` page margin (`PVMARGIN`/`PVMARGINL/R/T/B`). `side` picks which
    /// margin(s); `value` is the raw length text (validated + normalized by the host). Read back via
    /// `$PVMARGIN`/`$PVMARGINL/R/T/B`.
    SetPvMargin { side: PvMarginSide, value: String },
    /// The live Preview pane's initial width as a percent of the window (`PVWIDTH`; 20–80). `0` resets
    /// to the default. GUI-only layout (not the rendered content); `$PVWIDTH`; SaveCfg.
    SetPreviewWidth(u32),
    /// Preview/`TOPDF` page format (`PVPAPER` — letter/legal/tabloid/a4/a5/a6). `$PVPAPER`.
    SetPreviewPaper(String),
    /// Preview/`TOPDF` line spacing (`PVLEADING`) — a length, or ""/`default` for Typst's default.
    /// `$PVLEADING`.
    SetPreviewLeading(String),
    /// Preview/`TOPDF` extra space between paragraphs (`PVSPACING`) — a length, or ""/`default`.
    /// `$PVSPACING`.
    SetPreviewSpacing(String),
    /// Preview/`TOPDF` page-number pattern (`PVPAGENUM` — e.g. `1`, `Page 1`, `1 of 1`; ""/`off` = no
    /// numbering). `$PVPAGENUM`.
    SetPreviewPageNumber(String),
    /// Preview/`TOPDF` page-number position (`PVPAGEPOS` — a friendly token like `bottom-center`).
    /// `$PVPAGEPOS`.
    SetPreviewPageAlign(String),
    /// Preview/`TOPDF` body justification (`PVJUSTIFY on|off`). `$PVJUSTIFY`.
    SetPreviewJustify(bool),
    /// Preview/`TOPDF` first-line paragraph indent (`PVINDENT` — a length, or ""/`off`/`default` = none).
    /// `$PVINDENT`.
    SetPreviewIndent(String),
    /// Preview/`TOPDF` page header / footer template (`PVHEADER`/`PVFOOTER`; ""/`off` = none).
    /// `$PVHEADER`/`$PVFOOTER`.
    SetPreviewHeader(String),
    SetPreviewFooter(String),
    /// Set the command-line caret shape (`CMCRSHAPE`). Read back via `$CMCRSHAPE`.
    SetCmlineCaretShape(CaretShape),
    /// Set where the command line is drawn (`CMPLACE top|bottom|hidden`). Read back via `$CMPLACE`.
    SetCmlinePlacement(CmlinePlacement),
    /// Set where the status line is drawn: `true` = at the top just under the command line
    /// (XyWrite-style), `false` = the default bottom edge (`STATUSLINE top|bottom`). Read via
    /// `$STATUSLINE`.
    SetStatusLineTop(bool),
    /// Set where the `$CWD` path is shown (`PAPLACE cmline|status`). Read back via `$PAPLACE`.
    SetPathPlace(PathPlace),
    /// Set the max width (chars) of the displayed `$CWD` path (`PATHLEN n`). Read back via `$PATHLEN`.
    SetPathLen(usize),
    /// `STATUSPATH on|off` (bare toggles) — show the active file's FULL PATH on the status line instead
    /// of just its name (truncated to `PATHLEN`). Off by default. `$STATUSPATH`; SaveCfg.
    SetStatusPath(Option<bool>),
    /// `STATUSINFO <tokens>` — which informational elements the status line shows, as the resolved
    /// triple `(file, line:col, encoding)`. Each defaults to `true`; the command names the ones to
    /// show (`all`/`none` set every flag). `$STATUSINFO`; SaveCfg.
    SetStatusInfo { file: bool, linecol: bool, enc: bool },
    /// `TEXTFILES <.ext,…>` — the user's EXTRA text-file extensions, appended to the built-in set the
    /// File ▸ Open dialog's "Text files" filter matches. `Some(list)` replaces them (an empty list, from
    /// `TEXTFILES default`, clears); `None` reports the current list. Read back via `$TEXTFILES`.
    SetTextFileExts(Option<Vec<String>>),
    /// `HISTORY`/`HI [arg]` — ask the GUI to open the command-history picker. `arg` filters the
    /// listing: a number = the last N commands, any other non-empty text = commands containing it,
    /// empty = all. The host records the request; the GUI drains + applies it.
    ShowHistory(String),
    /// `MENU [letter]` — ask the GUI to drive the menu bar by keyboard. Bare `MENU` (arg `None`)
    /// toggles: open the first menu for arrow/Enter/Esc driving, or close it if already open.
    /// `MENU "f"` (arg `Some("f")`) opens the top menu whose accelerator letter is `f`. The host
    /// records a one-shot request; the GUI drains it and updates its keyboard-menu state. A no-op
    /// in headless builds.
    MenuKbd(Option<String>),
    /// `LoadCM <text>` — replace the command history from a `\n`-separated list (used by the
    /// config written by `SAVECFG`).
    LoadHistory(String),
    /// `SaveCM [path]` — write ONLY the command history to a runnable XPL script (a `LoadCM "…"`
    /// line) for a config-independent snapshot. `None` = the default history-file path.
    SaveHistory(Option<String>),
    /// The same `SaveCM` script TEXT, returned (not written) — used when `SAVECM` runs under
    /// `CAPTURE`, so the snapshot goes into a variable instead of a file.
    SaveHistoryToString,
    /// `SAVELDTX [path]` — write every non-empty TEXT register (`@A`..`@Z`/`@0`..`@9`) as a runnable
    /// XPL program of `LDTX <slot> "<literal>"` lines. `RUN`ning it restores the registers. `None` =
    /// the default binder-file path. Script binders (`&`) are not included.
    SaveLdtx(Option<String>),
    /// The same `SAVELDTX` script TEXT, returned (not written) — for `SAVELDTX` under `CAPTURE`.
    SaveLdtxToString,
    /// `SAVELDSC`/`SAVELDPM [path]` — write every non-empty SCRIPT register (`&A`..`&Z`/`&0`..`&9`)
    /// as a runnable XPL program of `LDSC <slot> "<xpl>"` lines. `RUN`ning it restores the registers.
    /// `None` = the default binder-file path. Text binders (`@`) are not included.
    SaveLdsc(Option<String>),
    /// The same `SAVELDSC` script TEXT, returned (not written) — for `SAVELDSC` under `CAPTURE`.
    SaveLdscToString,
    /// `LoadClosed <text>` — replace the reopen list (recently-closed files) from a serialized blob
    /// (`<caret>⇥<path>` per line). Used by the config written by `SAVECFG` and by `SaveClosed`.
    LoadClosed(String),
    /// `SaveClosed [path]` — write ONLY the reopen list to a runnable XPL script (a `LoadClosed "…"`
    /// line). `None` = the default reopen-file path.
    SaveClosed(Option<String>),
    /// The same `SaveClosed` script TEXT, returned (not written) — for `SaveClosed` under `CAPTURE`.
    SaveClosedToString,
    /// `REOPEN <n>` — reopen the first `n` recently-closed files immediately (no chooser), restoring
    /// each caret; the most-recent of the batch ends up active. `n` is clamped to the list length.
    ReopenRecent(usize),
    /// `CLEARCLOSED` — empty the reopen list.
    ClearClosed,
    /// `CMH ["command"]` — add a command to the TOP of the command history (dedup + move-to-front,
    /// like a submit), WITHOUT running it. `None` = the current command-line contents (for KBD
    /// bindings that stash what you're typing); `Some(text)` = a quoted-string command with its
    /// `$vars` already expanded (at stash time) by the verb.
    PushHistory(Option<String>),
    /// Render the active document (as GFM) to a typeset PDF (`TOPDF`/`PRINT
    /// [path]`). `None` = derive the path from the document's name.
    ToPdf(Option<String>),
    /// Render the active document to a single merged SVG (`TOSVG [path]`), by the same render kind as
    /// `TOPDF`. `None` = derive the path from the document's name (`<name>.svg`).
    ToSvg(Option<String>),
    /// `FPRINT <source> [<output>]` — render a file that ISN'T open to a PDF, exactly as `TOPDF` would
    /// if it were the active document: by the source file's own type (extension → render kind) with the
    /// current Preview/`TOPDF` settings. `output` `None` = derive `<source>.pdf`. No `MDA` styling
    /// (the file has none loaded).
    FilePrint { source: String, output: Option<String> },
    /// Report the word count of the selection (or whole document if none) to the status
    /// line (`WORDCOUNT`).
    WordCount,
    /// Report the character count of the selection (or whole document if none) to the
    /// status line (`CHARCOUNT`).
    CharCount,
    /// Switch to the next open buffer.
    BufNext,
    /// Switch to the previous open buffer.
    BufPrev,
    /// Toggle to the *alternate* buffer — the one active before the last switch — and back
    /// (`BUFALT`). Seeds from `BufPrev` on first use / when the alternate is stale.
    BufAlt,
    /// Switch to a buffer by 1-based index or file name (`BUFGO <n|name>`).
    BufGo(String),
    /// `GOFILE`/`GOF <text>` — switch to the NEXT open tab whose full path contains `text`
    /// (case-insensitive, separators normalized), cycling from the current tab. Stays put if the
    /// only match is the current tab; a trappable error (sets `$ERROR`) if nothing matches.
    GoFile(String),
    /// Split the focused pane, putting a fresh empty buffer in the NEW pane. `stacked` true =
    /// top/bottom (`NEW/H`); false = left/right (`NEW/V`). The new (empty) pane takes focus.
    PaneSplitNew { stacked: bool },
    /// Split the focused pane, keeping the current buffer and adding a PARTNER pane. `stacked` true =
    /// top/bottom (`THS`); false = left/right (`TVS`). `target` = a named open tab, else the alternate
    /// buffer, else a fresh empty buffer. The partner pane takes focus.
    PaneSplitTile { stacked: bool, target: Option<String> },
    /// `TW` — move focus to the next pane (wraps).
    PaneFocusNext,
    /// `TUS` — un-split: collapse the focused pane (its buffer stays open as a tab).
    PaneCollapse,
    /// `FS` — release the focused pane to full window: collapse the whole split to just it (the other
    /// panes' buffers stay open as tabs). Unlike `TUS`, this KEEPS the focused pane, not its sibling.
    PaneFullscreen,
    /// `SW/H` / `SW/V` — show the *Shell Output* buffer in a new split pane. `stacked` true =
    /// top/bottom, false = left/right. Creates the buffer if needed; focuses it if already shown.
    PaneShell { stacked: bool },
    /// `TRV` — swap the two panes of the split containing the focused pane.
    PaneSwap,
    /// `TZ` — toggle full-window zoom of the focused pane.
    PaneZoom,
    /// Request that the editor quit. `verify` = first prompt to save dirty/untitled tabs
    /// (`QUIT`); `verify == false` = close everything without warning (`QUIT/NV`). The GUI
    /// drives the prompts; the TUI just exits.
    Quit { verify: bool },
    /// `LDTX`/`LDSC`(`LDPM`) — load a **binder** slot. `script=false` → a text binder
    /// (`@`slot); `script=true` → an XPL-script binder (`&`slot). `slot` is an
    /// upper-cased `A`–`Z`/`0`–`9`. The host resolves `source` (selection / quoted
    /// literal / file) and stores the result.
    SetBinder { script: bool, slot: char, source: BinderSource },
    /// `@X` / `&X` in command position — use a binder. `script=false` inserts the stored
    /// text at the active caret; `script=true` runs the stored XPL (deferred via the same
    /// queue as `RUNHERE`). Trappable error if the slot is empty.
    UseBinder { script: bool, slot: char },
    /// `CLEARKBD` — reset the keyboard to the small built-in BASELINE keymap (a usable editing
    /// floor: typing, Backspace/Delete, arrows, Home/End, Enter, open-command-line, LOADKBD), not
    /// an empty map (an empty map can't edit — all editing keys are KBD-defined). Clears `$KB`.
    ClearKbd,
    /// `SAVEKBD [path]` — dump the currently-loaded keymap back to a KBD file (the inverse of
    /// `LOADKBD`). `None` = a default file. Serialized + written by the editor layer (which owns the
    /// operative keymap); the host just records the request.
    SaveKbd(Option<String>),
    /// The same `SAVEKBD` file TEXT, returned (not written) — for `SAVEKBD` under `CAPTURE`.
    SaveKbdToString,
}

/// Where a [`EditorOp::SetBinder`] takes its content. The host resolves each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinderSource {
    /// No source argument → the current selection (trappable error if none).
    Selection,
    /// A quoted literal (escapes already processed by the caller).
    Literal(String),
    /// A bare path → read the file's text (resolved against the editor cwd).
    File(String),
}

/// What a [`EditorOp::Delete`] removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteKind {
    /// The character before the cursor (Backspace / `BD`).
    Back,
    /// The character at the cursor (Delete/Del / `RC`).
    Forward,
    /// The current selection.
    Selection,
    /// `RE` — from the cursor to the end of the line (the break, if already there).
    ToEol,
    /// `RW` — the whole word at the cursor.
    Word,
    /// `RLL` — the whole LOGICAL line at the cursor (including its break).
    Line,
    /// `RL` — the whole VISUAL (wrapped, on-screen) line at the cursor, for XyWrite compatibility.
    /// Needs the GUI's shaped layout; without it (headless / `WRAP off`) it falls back to `Line`.
    VisualLine,
    /// `RS` — the whole sentence at the cursor.
    Sentence,
    /// `RP` — the whole paragraph at the cursor.
    Paragraph,
}

/// Defined-text selection control for [`EditorOp::Define`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefineKind {
    /// `DF` — toggle an open defined-text selection (open at cursor / close).
    Toggle,
    /// `DM` — snap the open selection's active end to the cursor.
    Modify,
    /// `DZ` — close an open defined-text selection (no-op if none).
    Close,
    /// `DFA` — select the whole document.
    All,
    /// `XD` / `YD` — cancel any selection.
    Cancel,
    /// `DW` — define (select) the word at the cursor.
    Word,
    /// `DL` — define the line at the cursor.
    Line,
    /// `DS` — define the sentence at the cursor.
    Sentence,
    /// `DP` — define the paragraph at the cursor.
    Paragraph,
    /// `DW+` — define the word **plus** its trailing space.
    WordPlus,
    /// `DS+` — define the sentence **plus** its trailing space.
    SentencePlus,
    /// `DP+` — define the paragraph **plus** the separating blank line(s).
    ParagraphPlus,
    /// `DIB` — define INSIDE the bracket pair enclosing the caret (Vim's `i(`/`ib` text object):
    /// the nearest enclosing `()`/`[]`/`{}`, delimiters excluded. No-op if the caret isn't inside one.
    InsideBracket,
    /// `DIQ` — define INSIDE the quote pair enclosing the caret on its line (Vim's `i"`/`i'`): the
    /// nearest enclosing `"`/`'`/`` ` `` pair, delimiters excluded. No-op if not inside one.
    InsideQuote,
}

/// A `DOCTYPE` request for [`EditorOp::DocType`]. `Query` reports the effective kind without changing
/// it; the others set the active document's override (`Auto` = clear → decide by extension).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocTypeReq {
    Query,
    Auto,
    Gfm,
    Typst,
    Html,
    Text,
}

/// Clipboard operation for [`EditorOp::Clip`] (the OS clipboard, `$CB`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipKind {
    /// Copy the selection to the clipboard (`COPY` / Ctrl+C).
    Copy,
    /// Cut the selection to the clipboard, then delete it (`CUT` / Ctrl+X).
    Cut,
    /// Paste the clipboard at the cursor, replacing any selection (`PASTE` / Ctrl+V).
    Paste,
    /// Append the selection to the clipboard's contents (`APPEND`).
    Append,
    /// Prepend the selection to the clipboard's contents (`PREPEND`).
    Prepend,
    /// Replace the clipboard with literal text (`CLIP`; empty clears it).
    Set(String),
}

/// Case transform for [`EditorOp::Case`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseKind {
    /// `LC` — lower case.
    Lower,
    /// `UC` — upper case.
    Upper,
    /// `CC` — swap the case of each character.
    Swap,
}

/// A parsed search/replace request for [`EditorOp::Search`]. `SE` variants set
/// `replace = None` (find-only); `CH` variants set `replace = Some(_)`
/// (replace-all). Modifiers are order-free flags combined from the verb suffix
/// (`B`ackward, `A`bsolute) and the trailing argument flags (`W` whole-word,
/// `T` from-top, `A` absolute, `B` backward).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchSpec {
    /// The literal pattern to find. Empty means "repeat the last search".
    pub find: String,
    /// The replacement text for `CH` (replace-all); `None` for a plain `SE` find.
    pub replace: Option<String>,
    /// Search toward the start of the document (`SEB`/`B`).
    pub backward: bool,
    /// Match case exactly (`SEA`/`CHA`/`A`); default is case-insensitive.
    pub case_sensitive: bool,
    /// Match whole words only (`W`).
    pub whole_word: bool,
    /// Start at the document edge instead of the cursor (`T`).
    pub from_top: bool,
    /// Limit the action to the current selection (`S`): search/replace only within the selected
    /// span, which stays put as a scope. The start point depends on where the caret sits relative
    /// to the selection (see `Document::search_select`/`replace_all`).
    pub scope: bool,
    /// Treat the pattern as a regular expression (`R`) — the linear-time `regex` engine.
    pub regex: bool,
    /// Treat the pattern as a Perl-compatible regex (`P`) — the `fancy-regex` engine, adding
    /// lookaround / backreferences / atomic groups. Wins over `regex` when both are set.
    pub perl: bool,
    /// `/K` — recase each replacement to match the case of the text it replaces: lower, Capitalized,
    /// or UPPER. So `CH/K /alligator/crocodile/` maps `alligator`→`crocodile`, `Alligator`→`Crocodile`,
    /// and `ALLIGATOR`→`CROCODILE`. Uses the same classification as `AR` auto-replace; a mixed-case
    /// match (`aLLIGATOR`) takes the replacement verbatim. Replace-only (meaningless for a bare `SE`).
    pub keep_case: bool,
    /// Extend the current selection to the match (`X`) instead of replacing it: keep the existing
    /// anchor (e.g. a `DF` define) and move only the active edge to the match, so the selection grows
    /// from the anchor through the match. Find-only (meaningless for replace); mutually exclusive with
    /// `/S` (scope keeps the selection fixed, extend grows it). See `Document::search_select`.
    pub extend: bool,
    /// Interactive change-verify (`CV`/`CVA`): prompt at each match instead of
    /// replacing all. Implies a replacement.
    pub verify: bool,
    /// The numeric `/n` switch (`SE/3`, `CH/5`). For a find, land on the *n*th match (error if there
    /// are fewer than `n`); for a replace, make *at most* `n` changes (no error if fewer are made).
    /// `None` = the default (first match / replace all).
    pub count: Option<usize>,
}

/// A parsed `SORT` request for [`EditorOp::Sort`]. Sorts the selected lines (order-free
/// flags: `/W` word-key, `/R` reverse, `/D` dedup, `/F:n` skip n key chars, `/N:n` take n).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SortSpec {
    /// `/W` — compare by lowercased whitespace-delimited word tokens (vs the default
    /// letter-by-letter: alphanumerics only, lowercased).
    pub word: bool,
    /// `/R` — reverse (descending) order.
    pub reverse: bool,
    /// `/D` — drop later lines whose sort key already appeared.
    pub dedup: bool,
    /// `/F:n` — skip the first `n` characters of each line before computing the key.
    pub f_skip: Option<usize>,
    /// `/N:n` — after `/F`, key on only the first `n` characters of what remains.
    pub n_take: Option<usize>,
}

/// Sort key for a `DIR` listing (`/O{N,S,D,E}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DirOrder {
    /// `/ON` — by name (the default; directories grouped first).
    #[default]
    Name,
    /// `/OS` — by size.
    Size,
    /// `/OD` — by modification date.
    Date,
    /// `/OE` — by extension, then name.
    Ext,
}

/// Attribute filter for a `DIR` listing (`/A{D,F}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DirOnly {
    /// No filter (the default).
    #[default]
    All,
    /// `/AD` — directories only.
    Dirs,
    /// `/AF` — files only.
    Files,
}

/// Display + ordering options for a `DIR`/`LS` listing, parsed from its switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DirOpts {
    /// `/B` — bare names-only view, one per line (vs the default `name  size  date`
    /// columns). Keeps the `..` parent and the trailing-`\` dir marker for navigation.
    pub bare: bool,
    /// `/R` — reverse the sort order (within each dirs/files group).
    pub reverse: bool,
    /// `/S` — recurse into subdirectories (entry names become base-relative paths).
    pub recurse: bool,
    /// `/O…` — the sort key.
    pub order: DirOrder,
    /// `/AD` / `/AF` — directories-only / files-only filter.
    pub only: DirOnly,
    /// `/AH` (or `/A`) — include hidden files (hidden by default, DOS-style).
    pub show_hidden: bool,
    /// `/AS` (or `/A`) — include system files (hidden by default, DOS-style).
    pub show_system: bool,
}

/// Which GUI view flag a [`EditorOp::SetView`] targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewFlag {
    /// The top menu bar (`$MB`; default on).
    MenuBar,
    /// Visible returns — a pilcrow ¶ at each hard newline (`$VR`; default off).
    VisibleReturns,
    /// Visible whitespace — a dot at each space and an arrow at each tab (`$WS`; default off).
    VisibleWhitespace,
    /// The line-number gutter (`$LNS`; default off).
    LineNumbers,
    /// The prominent pop-up command line — shown (in addition to the inline cmline) while the
    /// cmline is focused (`$GC`; default off).
    GuiCmline,
    /// The `*Shell Output*` tab's visibility in the tab strip (`SW`/`SHELLWIN`; `$SW`; default on).
    /// Turning it off hides the tab but keeps the buffer streaming; `X/BG` follows this state.
    ShellWindow,
    /// The live PDF preview pane on the right (`PREVIEW`/`PV`; `$PV`; default off) — a debounced
    /// raster of the active document (rendered per its `DOCTYPE`: Typst or GFM).
    Preview,
}

/// Set/clear/toggle for a [`EditorOp::SetView`] (the `S…`/`C…`/`T…` verb prefixes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewToggle {
    On,
    Off,
    Toggle,
}

/// How the bookmark gutter is shown (`MARKGUTTER off|on|persist`). `On` reserves the left strip only
/// when the document HAS bookmarks (so layout is unchanged otherwise); `Persist` reserves it always,
/// bookmarks or not, giving a stable left margin; `Off` never shows it. Read back via `$MARKGUTTER`
/// (`off`/`on`/`persist`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkGutterMode {
    Off,
    On,
    Persist,
}

/// Which keyboard lock a [`EditorOp::SetLock`] targets. Read back via `$CAPSLOCK`/`$NUMLOCK`/
/// `$SCROLLLOCK`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockKey {
    /// Caps Lock (`SCL`/`CCL`/`TCL`).
    Caps,
    /// Num Lock (`SNL`/`CNL`/`TNL`).
    Num,
    /// Scroll Lock (`SSL`/`CSL`/`TSL`).
    Scroll,
}

/// Which GUI sizing knob a [`EditorOp::SetUiSize`] targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiSizeKind {
    /// `UISCALE` — egui global zoom factor (whole UI). `$UISCALE`.
    Scale,
    /// `UIFONT` — chrome (menu/status/cmline) font size in points. `$UIFONT`.
    ChromeFont,
    /// `EDFONT` — editor text size in points. `$EDFONT`.
    EditorFont,
    /// `LEADING`/`LSP` — line spacing as a percent of the editor font. `$LSP`.
    Leading,
    /// `OF` — BOTH margins in character columns (sets left and right). `$OF` reads the left.
    MarginBoth,
    /// `OFL` — left margin in character columns. `$OFL`.
    MarginLeft,
    /// `OFR` — right margin in character columns. `$OFR`.
    MarginRight,
    /// `TXTFONT` — the `TXT` dialog window's text size in points. `$TXTFONT`.
    TxtFont,
    /// `TXTSCALE` — the `TXT` dialog window's zoom factor. `$TXTSCALE`.
    TxtScale,
    /// `DWFONT` — the `DIR/W` listing window's text size in points (`None`/default = inherit
    /// `EDFONT`). `$DWFONT`.
    DwFont,
    /// `DWSCALE` — the `DIR/W` listing window's zoom factor. `$DWSCALE`.
    DwScale,
    /// `PVSIZE` — the Markdown Preview/`TOPDF` body text size in points (`None`/default = Typst's
    /// default). `$PVSIZE`.
    PreviewFont,
    /// `PVCODESIZE` — the Markdown Preview/`TOPDF` code (monospace) size in points (`None`/default =
    /// inherit the body size). `$PVCODESIZE`.
    PreviewCodeFont,
}

/// Which colour a [`EditorOp::SetColor`] targets — the editor text canvas or the UI chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorGround {
    /// `FGCOLOR` — the editor foreground (text) colour. `$FGCOLOR`.
    Fg,
    /// `BGCOLOR` — the editor background colour. `$BGCOLOR`.
    Bg,
    /// `UIFGCOLOR` — the UI-chrome foreground (text) colour. `$UIFGCOLOR`.
    UiFg,
    /// `UIBGCOLOR` — the UI-chrome background colour. `$UIBGCOLOR`.
    UiBg,
    /// `CRCOLOR` — the text caret colour. `$CRCOLOR`.
    Caret,
    /// `CMCRCOLOR` — the command-line caret colour. `$CMCRCOLOR`.
    CmlineCaret,
    /// `SEPCOLOR` — the UI separator colour (the horizontal panel rules between the menu bar,
    /// tabs row, command line and status line, plus the vertical bars between tabs). `$SEPCOLOR`.
    Separator,
    /// `TABXCOLOR` — the tab close-`×` glyph colour (`None`/default = the UI foreground). `$TABXCOLOR`.
    TabClose,
    /// `BALPAIRSCOLOR` — the matched-bracket highlight colour (`None`/default = the selection colour).
    /// `$BALPAIRSCOLOR`.
    BalPairs,
    /// `TXTFGCOLOR` — the `TXT` dialog window's text colour. `$TXTFGCOLOR`.
    TxtFg,
    /// `TXTBGCOLOR` — the `TXT` dialog window's background colour. `$TXTBGCOLOR`.
    TxtBg,
    /// `DWFGCOLOR` — the `DIR/W` listing window's text colour (`None`/default = inherit editor).
    /// `$DWFGCOLOR`.
    DwFg,
    /// `DWBGCOLOR` — the `DIR/W` listing window's background colour. `$DWBGCOLOR`.
    DwBg,
    /// `SELBGCOLOR` — the selection background: the band painted behind selected text on the editor
    /// canvas. `$SELBGCOLOR`.
    SelBg,
    /// `SELFGCOLOR` — the selected-text foreground (the glyph colour of text inside the selection).
    /// `$SELFGCOLOR`.
    SelFg,
    /// `FINDBGCOLOR` — the FIND dialog's "highlight all" match background (default yellow).
    /// `$FINDBGCOLOR`.
    FindBg,
    /// `FINDFGCOLOR` — the FIND-match foreground (the glyph colour of highlighted match text; a more
    /// intense default so hits stand out on the highlight). `$FINDFGCOLOR`.
    FindFg,
    /// `SPELLCOLOR` — the underline colour for SPELLING issues from the live checker. `$SPELLCOLOR`.
    SpellUnderline,
    /// `GRAMMARCOLOR` — the underline colour for GRAMMAR/other issues from the live checker.
    /// `$GRAMMARCOLOR`.
    GrammarUnderline,
    /// `PANECOLOR` — the border colour drawn around the FOCUSED (active) split pane. Inactive panes
    /// use the separator colour. `$PANECOLOR`.
    PaneBorder,
    /// `EMPHFGCOLOR` — the `EMPH` emphasis foreground (glyph) colour (`None`/default = the
    /// selected-text colour, `SELFGCOLOR`). `$EMPHFGCOLOR`.
    EmphFg,
    /// `EMPHBGCOLOR` — the `EMPH` emphasis background band colour (`None`/default = the selection
    /// background, `SELBGCOLOR`). `$EMPHBGCOLOR`.
    EmphBg,
    /// `MDAS` — the `MDA` block foreground: colour the SELECTION's persistent text-block foreground
    /// layer (via the picker). Applied through `MDA <r g b>`.
    MdaFg,
    /// `MDAS/B` — the `MDA` block background layer for the SELECTION (via the picker). Applied
    /// through `MDA/B <r g b>`.
    MdaBg,
}

/// The text-caret shape (`CRSHAPE`). Read back via `$CRSHAPE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaretShape {
    /// A thin vertical I-beam at the caret (default).
    Bar,
    /// A filled (semi-transparent) block over the whole character cell.
    Block,
    /// A horizontal bar along the bottom of the cell.
    Underline,
}

impl CaretShape {
    /// The canonical name (`$CRSHAPE` / config).
    pub fn name(self) -> &'static str {
        match self {
            CaretShape::Bar => "bar",
            CaretShape::Block => "block",
            CaretShape::Underline => "underline",
        }
    }
    /// Parse a `CRSHAPE` argument (case-insensitive, with a couple of aliases).
    pub fn parse(s: &str) -> Option<CaretShape> {
        match s.trim().to_ascii_lowercase().as_str() {
            "bar" | "line" | "ibeam" => Some(CaretShape::Bar),
            "block" => Some(CaretShape::Block),
            "underline" | "underscore" => Some(CaretShape::Underline),
            _ => None,
        }
    }
}

/// The underline STYLE for live language-check issues (`SPELLSTYLE`/`GRAMMARSTYLE`). Read back via
/// `$SPELLSTYLE`/`$GRAMMARSTYLE`. The GUI draws the thin line under a flagged span in this shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnderlineStyle {
    /// A row of dots (the default).
    Dotted,
    /// A continuous straight line.
    Solid,
    /// A wavy / zig-zag line (the classic spell-check squiggle).
    Squiggle,
    /// [`Dotted`](Self::Dotted), drawn heavier (taller line, chunkier dots).
    DottedThick,
    /// [`Solid`](Self::Solid), drawn heavier (taller line).
    SolidThick,
    /// [`Squiggle`](Self::Squiggle), drawn heavier (thicker stroke).
    SquiggleThick,
}

impl UnderlineStyle {
    /// The canonical name (`$SPELLSTYLE`/`$GRAMMARSTYLE` / config).
    pub fn name(self) -> &'static str {
        match self {
            UnderlineStyle::Dotted => "dotted",
            UnderlineStyle::Solid => "solid",
            UnderlineStyle::Squiggle => "squiggle",
            UnderlineStyle::DottedThick => "dotted-thick",
            UnderlineStyle::SolidThick => "solid-thick",
            UnderlineStyle::SquiggleThick => "squiggle-thick",
        }
    }
    /// Parse a `SPELLSTYLE`/`GRAMMARSTYLE` argument (case-insensitive, with a couple of aliases).
    /// A `thick`/`bold`/`heavy` qualifier (in either order, any separator) selects the heavy variant.
    pub fn parse(s: &str) -> Option<UnderlineStyle> {
        let lower = s.trim().to_ascii_lowercase();
        // Split off a thickness qualifier wherever it appears (e.g. "thick squiggle", "dotted-bold").
        let mut thick = false;
        let shape: String = lower
            .split(|c: char| c == '-' || c == '_' || c == ' ')
            .filter(|w| {
                if matches!(*w, "thick" | "bold" | "heavy" | "fat") {
                    thick = true;
                    false
                } else {
                    !w.is_empty()
                }
            })
            .collect::<Vec<_>>()
            .join("");
        let base = match shape.as_str() {
            "dotted" | "dots" | "dot" => UnderlineStyle::Dotted,
            "solid" | "straight" | "line" => UnderlineStyle::Solid,
            "squiggle" | "squiggly" | "wavy" | "wave" | "zigzag" => UnderlineStyle::Squiggle,
            _ => return None,
        };
        Some(if thick { base.thickened() } else { base })
    }
    /// Whether this is a heavy variant (taller/chunkier line).
    pub fn is_thick(self) -> bool {
        matches!(
            self,
            UnderlineStyle::DottedThick | UnderlineStyle::SolidThick | UnderlineStyle::SquiggleThick
        )
    }
    /// The underlying SHAPE with any thickness stripped — always one of the three thin variants.
    /// The GUI matches on this and applies weight separately via [`is_thick`](Self::is_thick).
    pub fn shape(self) -> UnderlineStyle {
        match self {
            UnderlineStyle::DottedThick => UnderlineStyle::Dotted,
            UnderlineStyle::SolidThick => UnderlineStyle::Solid,
            UnderlineStyle::SquiggleThick => UnderlineStyle::Squiggle,
            thin => thin,
        }
    }
    /// The heavy counterpart of this shape.
    pub fn thickened(self) -> UnderlineStyle {
        match self.shape() {
            UnderlineStyle::Dotted => UnderlineStyle::DottedThick,
            UnderlineStyle::Solid => UnderlineStyle::SolidThick,
            _ => UnderlineStyle::SquiggleThick,
        }
    }
}

/// Which page margin(s) a `PVMARGIN*` verb sets — `All` (`PVMARGIN`) or one side (`PVMARGINL/R/T/B`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PvMarginSide {
    All,
    Left,
    Right,
    Top,
    Bottom,
}

/// Where the command line is drawn (`CMPLACE`). Read back via `$CMPLACE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmlinePlacement {
    /// At the top of the window, above the text (the default).
    Top,
    /// At the bottom of the window, below the text.
    Bottom,
    /// Out of the way until you focus it (then it appears at the top).
    Hidden,
}

impl CmlinePlacement {
    /// The canonical name (`$CMPLACE` / config).
    pub fn name(self) -> &'static str {
        match self {
            CmlinePlacement::Top => "top",
            CmlinePlacement::Bottom => "bottom",
            CmlinePlacement::Hidden => "hidden",
        }
    }
    /// Parse a `CMPLACE` argument (case-insensitive).
    pub fn parse(s: &str) -> Option<CmlinePlacement> {
        match s.trim().to_ascii_lowercase().as_str() {
            "top" => Some(CmlinePlacement::Top),
            "bottom" => Some(CmlinePlacement::Bottom),
            "hidden" | "hide" => Some(CmlinePlacement::Hidden),
            _ => None,
        }
    }
}

/// Where the working-directory path is shown (`PAPLACE`). Read back via `$PAPLACE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathPlace {
    /// On the status line (the default — the traditional spot).
    Status,
    /// As a prefix on the command line.
    Cmline,
}

impl PathPlace {
    /// The canonical name (`$PAPLACE` / config).
    pub fn name(self) -> &'static str {
        match self {
            PathPlace::Status => "status",
            PathPlace::Cmline => "cmline",
        }
    }
    /// Parse a `PAPLACE` argument (case-insensitive).
    pub fn parse(s: &str) -> Option<PathPlace> {
        match s.trim().to_ascii_lowercase().as_str() {
            "status" => Some(PathPlace::Status),
            "cmline" | "cm" | "cmd" => Some(PathPlace::Cmline),
            _ => None,
        }
    }
}

/// Persistent-selection control for [`EditorOp::Persistent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistKind {
    /// `SPS` — turn persistent selection on.
    On,
    /// `CPS` — turn it off.
    Off,
    /// `TPS` — toggle.
    Toggle,
}

/// A cursor motion for [`EditorOp::Move`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    /// `LU` — up one VISUAL (wrapped screen) row. The host steps over the GUI-published visual-row map
    /// when it's fresh (so it works in `;`-chains, macros and scripts); otherwise it falls back to the
    /// logical `Up`. (The bare interactive form is also refined pixel-exactly by the GUI.)
    VisualUp,
    /// `LD` — down one VISUAL (wrapped screen) row. See [`Motion::VisualUp`].
    VisualDown,
    /// Start of the current line.
    LineStart,
    /// End of the current line.
    LineEnd,
    /// `LB` — start of the current VISUAL (wrapped) line. The engine moves to the logical line
    /// start as a fallback and records a request the GUI refines to the true visual-row start.
    VisualLineStart,
    /// `LE` — end of the current VISUAL (wrapped) line (logical-line-end fallback + GUI refine).
    VisualLineEnd,
    /// Start of the buffer.
    DocStart,
    /// End of the buffer.
    DocEnd,
    /// `NW` — start of the next word.
    NextWord,
    /// `PW` — start of the current/previous word.
    PrevWord,
    /// `NP` — start of the next paragraph.
    NextPara,
    /// `PP` — start of the current/previous paragraph.
    PrevPara,
    /// `NS` — start of the next sentence.
    NextSentence,
    /// `PS` — start of the current/previous sentence.
    PrevSentence,
    /// `NL` — start of the next line.
    NextLineStart,
    /// `PL` — start of the previous line.
    PrevLineStart,
    /// `JMP n` — absolute character offset.
    ToChar(usize),
    /// `JMP +n` / `JMP -n` — relative character offset (clamped at the edges).
    ByChar(i64),
    /// `LL` — one char left, not crossing to the previous line.
    LeftSameLine,
    /// `LR` — one char right, not crossing to the next line.
    RightSameLine,
    /// `PD` — down one page (uses the host's viewport row count).
    PageDown,
    /// `PU` — up one page.
    PageUp,
    /// `HM` — to the top visible line (uses the host's viewport).
    ScreenTop,
    /// `BS` — to the bottom visible line.
    ScreenBottom,
}

/// A `HELP` lookup result (see [`Host::help`]). The app formats the title/body from the reference;
/// the VM decides how to surface it (a `TXT` modal, a `CAPTURE` variable, or — when `not_found` —
/// a status-line message).
pub struct HelpResponse {
    pub body: String,
    pub not_found: bool,
    /// `(phrase, regex)` to EMPHasize in `body` when shown in the results tab — the search term as a
    /// whole-word regex. `None` = no highlight (bare overview or a miss).
    pub emphasis: Option<(String, bool)>,
}

/// The correction choices for the language issue at the caret (`LINTFIX`): the document `span` (rope
/// offsets) to replace, human-readable `labels` for the chooser, and the parallel `replacements`.
pub struct LintFixChoices {
    pub span: (usize, usize),
    pub labels: Vec<String>,
    pub replacements: Vec<String>,
    /// The issue's explanation (the checker's description of the problem), for the right-click
    /// popup's header. Empty when there's none (e.g. a plain Hunspell misspelling).
    pub message: String,
    /// Whether this is a SPELLING issue (vs grammar/other) — gates the "Add to dictionary" item.
    pub is_spelling: bool,
}

/// What a no-argument `CALC` operates on, from the host's view of the active document.
pub enum CalcContext {
    /// The command line is focused — there's no document line to evaluate; `CALC` shows guidance.
    CmLine,
    /// No selection: evaluate this (the caret's line) and insert the result on a new line below it.
    Line(String),
    /// A selection: evaluate each line, sum the results, insert the total on a new line below it.
    Lines(Vec<String>),
}

/// The seam to the editor / OS / keyboard layer / clock.
pub trait Host {
    /// `LOADKBD <path>` — hand off to the transactional keymap loader.
    fn load_kbd(&mut self, path: &str) -> VmResult<()>;
    /// `ADDKBD <name> <path>` — load a `.kbd` into a RESIDENT named layout slot (for modal / multi-
    /// layout switching), compiled and kept in memory so `KBDMODE` can switch to it instantly. Loading
    /// the slot that is already active also installs it operatively; otherwise it is only stored. Same
    /// transactional / last-known-good rule as `load_kbd`. Default: treat as a plain `load_kbd` of the
    /// path (hosts without the mode registry still load the keyboard).
    fn load_kbd_named(&mut self, _name: &str, path: &str) -> VmResult<()> {
        self.load_kbd(path)
    }
    /// `KBDMODE <name>` — switch the operative keyboard to the resident layout slot `name` (loaded
    /// earlier with `ADDKBD`). Trappable error if no such slot. Instant: swaps the compiled map, no
    /// disk read or parse. Default: no-op success (hosts without the registry have a single layout).
    fn set_kbd_mode(&mut self, _name: &str) -> VmResult<()> {
        Ok(())
    }

    // --- UI-string localization (so interpreter-side status messages translate too) -----------------
    // The UI catalog lives on the app host; these let the interpreter localize the messages it emits
    // (keyboard/library/variable load confirmations, etc.) through the same catalog the app uses. The
    // defaults (hosts with no catalog) return the English text with placeholders filled, so behavior is
    // unchanged where there is no translation.

    /// Translate a plain message by catalog id, falling back to the English literal `en`.
    fn tr_msg(&self, _id: &str, en: &str) -> String {
        en.to_string()
    }
    /// Translate a message with `{name}` placeholders, substituting each from `args`. A translation may
    /// reorder the placeholders; values are substituted verbatim.
    fn tr_fmt(&self, _id: &str, en: &str, args: &[(&str, &dyn std::fmt::Display)]) -> String {
        let mut s = en.to_string();
        for (name, value) in args {
            s = s.replace(&format!("{{{name}}}"), &value.to_string());
        }
        s
    }
    /// Translate a pluralized message: `base` names the catalog entry, `en_one`/`en_other` are the
    /// English singular/plural templates (with `{n}`), and `n` picks the form. The default uses the
    /// English `n == 1` rule; a real catalog applies the target language's CLDR plural rule.
    fn tr_plural(&self, _base: &str, en_one: &str, en_other: &str, n: usize) -> String {
        let en = if n == 1 { en_one } else { en_other };
        en.replace("{n}", &n.to_string())
    }

    /// `$SC` / `READKEY` — the canonical scan code of the last captured key.
    fn last_scan_code(&self) -> i64 {
        0
    }
    /// `$SCC` = `$SC` + this: the bit value(s) of the modifier key(s) held for the last captured key
    /// (the keymap's SHIFT/CTRL/ALT/… bits OR-ed together), or `0` if none.
    fn last_modifier_bits(&self) -> i64 {
        0
    }
    /// `KEY(scan)` — the current binding of key code `scan` on the unshifted layer of the operative
    /// keymap, as a string (`"KF"` for a Keyboard-Flip cell). `None` if unbound, out of range, or no
    /// keymap is loaded (the function then yields the empty string).
    fn key_binding(&self, _scan: i64) -> Option<String> {
        None
    }

    /// `GETKEY(scc)` — the display NAME of a computed scan code (`$SCC` = key code + modifier bits),
    /// the string `READKEY/D` would write into its variable: a `+`-joined modifier prefix (from the
    /// keymap's own modifier definitions) then the key's name (`CTRL+Y`, `Enter`, `Backspace`). `None`
    /// if `scc` carries unknown modifier bits or names no key (the function then yields the empty
    /// string).
    fn scc_key_name(&self, _scc: i64) -> Option<String> {
        None
    }
    /// `KBCHART` — the loaded keymap's full chart as display text, one section per modifier table
    /// (Normal / SHIFT / CTRL / …) listing its `scan → binding` cells. Shown in a scrollable TXT modal.
    fn keyboard_chart_text(&self) -> String {
        String::new()
    }
    /// Whether `path` (resolved against the editor's working dir, like every other file op) is an
    /// existing DIRECTORY. Used by the `GETPATH()` function so a directory's last component isn't
    /// mistaken for a filename. Default `false` (no filesystem).
    fn path_is_dir(&self, _path: &str) -> bool {
        false
    }
    /// Whether `path` (working-dir-resolved) exists at all — file or directory. For `EXIST()`/
    /// `ISFILE()`. Default `false`.
    fn path_exists(&self, _path: &str) -> bool {
        false
    }
    /// Take the XPL action queued by an `AR` whose replacement value started with `#`: the host has
    /// already deleted the matched key, and the VM runs this XPL (then `AR` aborts the rest of its
    /// command chain). `None` when the last `AR` was a plain text replacement / no match.
    fn take_ar_xpl(&mut self) -> Option<String> {
        None
    }
    /// An editor/text operation. Stubbed in Stage 1.
    fn editor_op(&mut self, op: EditorOp) -> VmResult<Value>;
    /// Live read-only state for special variables (`$SE`, `$CB`, `$OS`, …).
    /// `None` if the host doesn't know the name (then user vars are consulted).
    fn special(&self, _name: &str) -> Option<Value> {
        None
    }
    /// Read a binder's stored content for value access (`@X`/`&X` used as a value):
    /// `script=false` → text binder `@slot`, `script=true` → script binder `&slot`.
    /// `slot` is upper-cased. `None` if unset (resolves to the empty string).
    fn binder(&self, _script: bool, _slot: char) -> Option<String> {
        None
    }
    /// Fixed decimal places for displaying floats (the `DECIMALS` setting); `None` = default
    /// precision. Consulted when values are stringified for display (substitution / `$(…)`).
    fn float_decimals(&self) -> Option<u8> {
        None
    }
    /// The default `DIR`/`LS` sort/filter/view switch names (`DIRSET`), each an already-validated
    /// `DirOpts` switch (e.g. `"OD"`, `"R"`). The VM merges these into a `DIR` command that doesn't
    /// override them (a command switch beats a default with the same leading letter). Empty = none.
    fn dir_default_switches(&self) -> Vec<String> {
        Vec::new()
    }
    /// Read a text file (for `LOADLIB`/`ADDLIB`/`RUN`/`OPEN`). A missing/
    /// unreadable file should be a [`crate::error::XplError::Trappable`] so a
    /// script can recover via `$ERROR`. The default rejects everything (headless
    /// tests opt in via [`StubHost`]'s virtual filesystem).
    fn read_file(&self, path: &str) -> VmResult<String> {
        Err(crate::error::XplError::trappable(format!(
            "cannot read file: {path}"
        )))
    }

    /// Read a file decoding its bytes through a named `charset` (for `READFILE(path, charset)`).
    /// `charset == None` (or `"utf-8"`) is the plain [`Self::read_file`] path. Other names are the
    /// editor's single-byte code pages (`cp437`, `windows-1252`, `latin1`, …) plus the pseudo-encoding
    /// `"hex"` (the file's raw bytes as a lowercase hex string); `latin1` is the byte-exact "binary"
    /// read. An unknown charset is a trappable error. The default ignores `charset` (UTF-8 only).
    fn read_file_enc(&self, path: &str, _charset: Option<&str>) -> VmResult<String> {
        self.read_file(path)
    }

    /// The full text of the active document (the one on screen, or the blind-edit buffer if one
    /// is open) — used by `PRINTEX` to scan it for `|expr|` soft-fields. Default empty.
    fn active_text(&self) -> String {
        String::new()
    }

    /// Point-and-shoot target: the path of the file/dir under the caret when the active buffer is a
    /// `DIR`/`LS` listing and the caret is on a real entry (not the `..` parent). `None` otherwise.
    /// The dispatcher uses this to fill an EMPTY file argument for the file commands (`OPEN`, `DEL`,
    /// `RUN`, …), so `DEL` alone on a listing line deletes that line's file — XyWrite point-and-shoot.
    fn point_and_shoot_file(&self) -> Option<String> {
        None
    }

    /// A random integer in the inclusive range `[min, max]` from the host's seedable generator, for
    /// the `RANDOM(min, max)` function. `None` where there is no generator (the bare/stub host).
    /// Args are swapped if `min > max`.
    fn random_int(&self, _min: i64, _max: i64) -> Option<i64> {
        None
    }
    /// Seed the host's random generator (the `SRANDOM <seed>` verb), making the subsequent `RANDOM`
    /// sequence reproducible. Default: no-op (no generator).
    fn seed_random(&self, _seed: u64) {}

    /// `HELP [name]` — look up reference help for a command, `$special`, or function. The app owns
    /// the reference text and returns a formatted title + body; `query = None` is the overview.
    /// `not_found` is true when no entry matched (the verb reports it on the status line, with
    /// suggestions, instead of opening a modal). `search` (the `/S` flag) forces the grep-like
    /// search over names + descriptions instead of an exact lookup; an exact lookup that misses
    /// also falls back to that search. Default: help is unavailable.
    fn help(&self, _query: Option<&str>, _search: bool, _substring: bool) -> HelpResponse {
        HelpResponse {
            body: "Help is not available in this build.".to_string(),
            not_found: true,
            emphasis: None,
        }
    }

    /// `HELP <term>` — search BOTH the command reference and the manual (reference first), merged into
    /// one results tab. `REF` (via [`Self::help`]) is the reference alone; `MAN` the manual alone.
    /// Bare `HELP` gives the overview. Default: unavailable.
    fn help_all(&self, _query: Option<&str>, _substring: bool) -> HelpResponse {
        HelpResponse {
            body: "Help is not available in this build.".to_string(),
            not_found: true,
            emphasis: None,
        }
    }

    /// `MAN <topic>` — search the user manual by section (the prose companion to [`Self::help`]).
    /// Returns a formatted title + body listing the matching sections with their full text;
    /// `not_found` when nothing matched (reported on the status line, like `HELP`). Bare `MAN` opens
    /// the manual via `EditorOp::ShowManual` instead of calling this. Default: unavailable.
    fn manual_search(&self, _query: &str, _substring: bool) -> HelpResponse {
        HelpResponse {
            body: "The manual is not available in this build.".to_string(),
            not_found: true,
            emphasis: None,
        }
    }

    /// The selection text of tab `n` (**1-based**, matching `$CT`/`$TC`) — for the `SEL(n)`
    /// function. `""` if `n` is out of range or that tab has no selection. Default empty.
    fn selection_in(&self, _n: i64) -> String {
        String::new()
    }

    /// Selections in tabs OTHER than the active one, for the enhanced `CP` chooser: `(buffer_index,
    /// label)` pairs (label = a one-line preview). Default empty (no other tabs).
    fn cross_tab_selection_candidates(&self) -> Vec<(usize, String)> {
        Vec::new()
    }

    /// Insert the selection of buffer `buffer_index` at the active caret (enhanced `CP` on resume).
    /// Returns a status message; trappable if the tab/selection is gone. Default: unavailable.
    fn copy_selection_from(&mut self, _buffer_index: usize) -> VmResult<String> {
        Err(crate::error::XplError::trappable("CP: cross-tab copy unavailable"))
    }

    /// The recently-closed files for the `REOPEN` chooser: `(path, caret, label)` per entry, newest
    /// first (`label` = the two-column display line). Default empty.
    fn reopen_candidates(&self) -> Vec<(String, usize, String)> {
        Vec::new()
    }

    /// Reopen `path` (REOPEN on resume): open it (or switch if already open), restore the caret to
    /// `caret` (clamped), and drop it from the reopen list. Returns a status message; trappable if
    /// the file is gone. Default: unavailable.
    fn reopen_file(&mut self, _path: &str, _caret: usize) -> VmResult<String> {
        Err(crate::error::XplError::trappable("REOPEN: unavailable"))
    }

    /// Write a text file (for `SAVE`). Should be atomic where possible (write a
    /// temp file then rename) so a failure can't corrupt the target. Errors are
    /// trappable. The default rejects everything.
    fn write_file(&mut self, path: &str, _content: &str) -> VmResult<()> {
        Err(crate::error::XplError::trappable(format!(
            "cannot write file: {path}"
        )))
    }

    /// Write a file encoding `content` through a named `charset` (for `WRITEFILE/ENC:<cp>` / `/B` /
    /// `/HEX`). `charset == None` (or `"utf-8"`) is the plain [`Self::write_file`] path. Single-byte
    /// code pages encode text → bytes (characters the page can't represent become `?`, unless `strict`,
    /// which makes such a character a trappable error and writes nothing); `latin1` is the byte-exact
    /// "binary" write; `"hex"` parses the content as a hex string and writes those raw bytes (odd length
    /// / non-hex digit is a trappable error). An unknown charset is a trappable error. The default
    /// ignores `charset`/`strict` (UTF-8 only, where nothing is unrepresentable).
    fn write_file_enc(
        &mut self,
        path: &str,
        _charset: Option<&str>,
        _strict: bool,
        content: &str,
    ) -> VmResult<()> {
        self.write_file(path, content)
    }

    /// Run a shell command to completion; returns `(merged stdout+stderr, exit_code)` and
    /// sets `$RC`/`$XO`. Trust-gated (a disabled host returns a trappable error).
    fn shell(&mut self, _cmd: &str) -> VmResult<(String, i64)> {
        Ok((String::new(), 0))
    }
    /// Fire-and-forget shell spawn (`X/NV`): start the command detached, don't wait or
    /// capture. Trust-gated. Default no-op.
    fn shell_spawn(&mut self, _cmd: &str) -> VmResult<()> {
        Ok(())
    }
    /// Current `($DA, $TM)` — date and time strings.
    fn now(&self) -> (String, String) {
        (String::new(), String::new())
    }

    /// Whether error logging (`ERRORLOG`) is on — the VM checks this before assembling a log entry,
    /// so nothing is built when logging is off. Default: off.
    fn error_log_enabled(&self) -> bool {
        false
    }
    /// Append one error to the error log (best-effort — a write failure is swallowed, never sets
    /// `$ERROR` or recurses). `fatal` picks the tier column (`FATAL` vs `TRAP`); `code` is the
    /// `$ERROR` code (`0` for a fatal); `command` is the verb + raw args that triggered it;
    /// `location` is `file:line` (empty if unknown); `message` is the specific error text. The host
    /// adds the timestamp + active-document path and writes the TSV row. Default: no-op.
    fn log_error(&mut self, _fatal: bool, _code: u32, _command: &str, _location: &str, _message: &str) {}

    /// One line per open buffer (for `BUFLIST`). Default: none.
    fn buffer_list(&self) -> Vec<String> {
        Vec::new()
    }
    /// Switch the active buffer to `index` (0-based, into the tab order shown by [`buffer_list`]).
    /// Used by the `LISTTABS` chooser on resume. Out-of-range is a no-op. Default: unavailable.
    fn switch_to_buffer(&mut self, _index: usize) -> VmResult<()> {
        Err(crate::error::XplError::trappable("LISTTABS: unavailable"))
    }
    /// The active document's bookmarks for the `MARKLIST` chooser: `(offset, preview)` per row, in
    /// document order. `preview` is a short label (line number + surrounding text). Default: none.
    fn bookmark_list(&self) -> Vec<(usize, String)> {
        Vec::new()
    }

    /// The active document's xlinks for the `XLINKS` chooser: `(start-offset, preview)` per row, in
    /// document order. `preview` is a short label (line number + link text + payload). Default: none.
    fn xlink_list(&self) -> Vec<(usize, String)> {
        Vec::new()
    }

    /// The payload (XPL source) of the xlink whose span contains the caret, for `XLGO` and `$XLINK`.
    /// `None` when the caret isn't on an xlink. Default: none.
    fn xlink_payload_at_caret(&self) -> Option<String> {
        None
    }

    /// The payload of the xlink with the given NAME (`XLGO <name>`), or `None` if none has it.
    /// Default: none.
    fn xlink_payload_named(&self, _name: &str) -> Option<String> {
        None
    }

    /// Whether the active document is trusted to run ACTION (non-navigation) xlink payloads — used by
    /// `XLGO` to decide whether an action payload runs or needs the confirm dialog. Default: not
    /// trusted (so an action payload is always gated).
    fn xlink_action_trusted(&self) -> bool {
        false
    }

    /// Record the active document as trusted for action xlink payloads — the confirm dialog's "Trust
    /// this document" choice (and `XLTRUST`). Default: no-op.
    fn xlink_trust_active(&mut self) {}

    /// Snapshot the current location just before an xlink activation runs, for the jumplist (`XLBACK`/
    /// `XLFWD`). Paired with [`xlink_jump_commit`](Self::xlink_jump_commit). Default: no-op.
    fn xlink_jump_mark(&mut self) {}

    /// After an xlink activation, record the marked location on the jumplist if the activation moved
    /// the caret. Default: no-op.
    fn xlink_jump_commit(&mut self) {}

    /// The active document's structure for the `OUTLINE`/`OL` chooser, plus the row to pre-highlight.
    /// Returns `(entries, current)`: `entries[i]` is `(offset, label)` per row in document order (label
    /// indented by heading level — Markdown/Typst/HTML headings or XPL `SUB` routines), and `current` is
    /// the index of the section the caret is currently in (the last entry at or before the caret).
    /// Default: none.
    fn outline(&self) -> (Vec<(usize, String)>, usize) {
        (Vec::new(), 0)
    }

    /// What a no-argument `CALC` should work with (see [`CalcContext`]): the command line is focused
    /// (show guidance), a single caret line to evaluate, or a multi-line selection to sum. Default:
    /// the command line (nothing to evaluate).
    fn calc_context(&self) -> CalcContext {
        CalcContext::CmLine
    }
    /// Insert `text` on a NEW line immediately below the current line (no selection) or below the whole
    /// selection — where `CALC` puts its result. Recorded (undoable). Default: no-op.
    fn calc_insert_result(&mut self, _text: &str) {}
    /// Copy `text` to the clipboard (`CALC <expr>`), the same clipboard `COPY` uses. Default: no-op.
    fn calc_set_clipboard(&mut self, _text: &str) {}
    /// Remember the last `CALC` result string (read back as `$CALC`). Default: no-op.
    fn set_last_calc(&mut self, _text: &str) {}

    /// Cursor-addition (XyWrite-style): reset the running total (`$SUM`) to 0 (`CLRSUM`). Default: no-op.
    fn sum_clear(&mut self) {}
    /// Add the number under the caret to the running total (`ADDSUM`): the number token at (or just
    /// after) the caret, with any leading currency sign and thousands-separators stripped. Returns
    /// `(added, total)` — both display-formatted (honouring `DECIMALS`) — or an error string (no number
    /// under the caret, or an unparseable token). Default: unavailable.
    fn sum_add_under_cursor(&mut self) -> Result<(String, String), String> {
        Err("cursor addition is unavailable".to_string())
    }
    /// Insert the running total at the caret (`PUTSUM`), recorded (undoable). Returns the inserted
    /// (display-formatted) total. Default: no-op, empty string.
    fn sum_put(&mut self) -> String {
        String::new()
    }
    /// Move the active document's caret to byte `offset` and scroll it into view. Used by the
    /// `MARKLIST` chooser on resume. Default: unavailable.
    fn jump_to_offset(&mut self, _offset: usize) -> VmResult<()> {
        Err(crate::error::XplError::trappable("MARKLIST: unavailable"))
    }
    /// The correction choices for the language issue at the caret (`LINTFIX`), or `None` if there
    /// isn't one. Default: none (no language checking).
    fn lint_fix_options(&mut self) -> Option<LintFixChoices> {
        None
    }
    /// Apply a `LINTFIX` correction: replace the document range `span` (rope offsets) with
    /// `replacement`. Default: unavailable.
    fn apply_lint_fix(&mut self, _span: (usize, usize), _replacement: &str) -> VmResult<()> {
        Err(crate::error::XplError::trappable("LINTFIX: unavailable"))
    }
    /// Lines describing the running background jobs (`JOBS`). Default: none.
    fn job_list(&self) -> Vec<String> {
        Vec::new()
    }
    /// One line per syntax-highlight token kind with its current colour (`HLKINDS`) — the names
    /// accepted by `HLCOLOR`. Default: none.
    fn highlight_kinds(&self) -> Vec<String> {
        Vec::new()
    }
    /// The RGB colour of each `highlight_kinds()` line, in the SAME order (`None` = the kind uses the
    /// default text colour). Lets the `HLKINDS` dialog render each kind's label in its own colour.
    fn highlight_kind_colors(&self) -> Vec<Option<(u8, u8, u8)>> {
        Vec::new()
    }
}

/// A headless [`Host`] for tests: records every effect so assertions can be made
/// without a GUI or OS. (Also handy as the default app-less host.)
#[derive(Default)]
pub struct StubHost {
    /// Ordered log of effects (editor ops, LOADKBD, shell), for assertions.
    pub log: Vec<String>,
    /// The last path passed to `LOADKBD`.
    pub loaded_kbd: Option<String>,
    /// Value returned by `last_scan_code` / `$SC`.
    pub last_scan: i64,
    /// Preloaded special-variable values (`$SE`, `$OS`, …).
    pub specials: std::collections::HashMap<String, Value>,
    /// If set, the next `editor_op` returns this error (to exercise the error
    /// model). Consumed on use.
    pub next_editor_error: Option<crate::error::XplError>,
    /// Virtual filesystem for `read_file` (path -> contents), so `LOADLIB`/`RUN`
    /// are testable headlessly.
    pub files: std::collections::HashMap<String, String>,
}

impl Host for StubHost {
    fn load_kbd(&mut self, path: &str) -> VmResult<()> {
        self.log.push(format!("LOADKBD {path}"));
        self.loaded_kbd = Some(path.to_string());
        Ok(())
    }
    fn last_scan_code(&self) -> i64 {
        self.last_scan
    }
    fn editor_op(&mut self, op: EditorOp) -> VmResult<Value> {
        if let Some(err) = self.next_editor_error.take() {
            return Err(err);
        }
        self.log.push(format!("{op:?}"));
        Ok(Value::str(""))
    }
    fn special(&self, name: &str) -> Option<Value> {
        self.specials.get(name).cloned()
    }
    fn read_file(&self, path: &str) -> VmResult<String> {
        match self.files.get(path) {
            Some(text) => Ok(text.clone()),
            None => Err(crate::error::XplError::trappable(format!(
                "file not found: {path}"
            ))),
        }
    }
    fn path_exists(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }
    fn write_file(&mut self, path: &str, content: &str) -> VmResult<()> {
        self.files.insert(path.to_string(), content.to_string());
        Ok(())
    }
    fn shell(&mut self, cmd: &str) -> VmResult<(String, i64)> {
        self.log.push(format!("SHELL {cmd}"));
        Ok((String::new(), 0))
    }
    fn now(&self) -> (String, String) {
        ("2026-01-01".into(), "00:00".into())
    }
}
