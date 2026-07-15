---
title: qn-feature-parity-plan
tags: [design, features, quicknotes, parity, roadmap, command-set, xpl]
created: 2026-06-09
---
# QuickNotes → XyScript feature parity & extension

----------
CLD Notes (Tue Jun  9 00:32:44 2026):
Command line ("CMline"):
BC: Clears ("Blanks") the CMline and moves the insertion point to CMline position 0
XC: Executes the command displayed on the CMline
GH: Moves cursor to CMline (last cursor position)
GT: Moves cursor from CMline to text window (last cursor position)
TH (new): Toggle cursor between CMline and text
The last three require that XyScript memorize the cursor positions on CMline and in text.
Variable $CX should return the CMline cursor position if cursor is on the CMline.

Text selection ("defined text"):
DF: Opens a selection if none is started, else closes the selection (if selection start == selection end, cancel selection)
DZ: Closes an open selection (like DF if a selection is started, else a no-op)
DM: Modify selection: If cursor is outside of selection, extend selection to cursor position. If cursor is within the selection, shrink the selection to the cursor position (decrease $DN). In either case, leave selection open. DM is typically followed by optional cursor movement and then DZ to close the selection.

Block deletion:
RD: Modify to "copy selection to a deletion buffer [new], then delete selection".
UD (new, companion to RD): Paste deletion buffer at insertion point (do NOT clear buffer, keep it until next RD)
Note: RD + cursor movement + UD = Move
----------


QuickNotes (QN) is the **Python+tkinter prototype** for XyScript. Its command
set — XyWrite-derived, two-letter verbs + a real scripting language (QPL) — is the
**feature floor** XyScript must meet and then extend. We take its *features*, never
its *architecture* (single 1 MB `quicknotes.py`; Tk widgets). This doc catalogs the
QN surface from the manual's Command Reference + Command Index (every verb), the
Special-Variable and Function references, and the feature chapters, then maps each
onto XyScript with a status and a scope verdict.

Source of truth: `C:\Users\cldis\QN2\qnmanual.md` (213 KB user manual) and
`quicknotes.py`. Verb counts below are from the manual's alphabetical Command Index.

## Guiding filter (from xyscript-product-scope)

- **In scope:** everything that makes a fast keyboard-first *text editor* — motion,
  selection, deletion, case, search/replace, clipboard, counts, sort, date/time,
  file I/O, view toggles, and the whole scripting/library/keybinding stack.
- **Adapt (Tk → egui/Rust):** UI-sizing knobs (`UISCALE` is Tk-specific → egui
  zoom/scale), the notes-folder sidebar (→ XyScript's file/directory pane), fonts.
- **Trust-gated:** shell-out (`X`/`XY`/`XV`) and `RF`/`WF`/filesystem writes — power
  features; consistent with the cautious Xyperlink security model (never silent
  surprise), but normal for *user-initiated* local scripts. Default-on for the
  local user's own scripts; revisit gating when remote/untrusted scripts arrive.
- **Markdown:** QN ships a *compact subset*; XyScript is committed to **GFM 100% via
  comrak** — already a strict superset (tables, images, footnotes, ref links, etc.).
  We only need to adopt QN's markdown-adjacent *behaviors*: GFM task lists (comrak ✓),
  per-language fenced-code highlighting (our highlight seam), and `RUNHERE`.

---

## A. Already at parity (built or essentially present in XPL/app)

The **QPL language layer is largely done** in `xyscript-xpl` — this is the biggest
and least-obvious finding:

- **Control flow & structure:** `IF/IFNOT/ELSE/ELSEIF/ENDIF`, `WHILE/ENDWHILE`,
  `BREAK/CONTINUE`, `GOTO`+labels, `SUB/ENDSUB`, `CALL`, `RETURN`(→`$RV`),
  `LOCAL`, `SET`, line continuation, `.qns` scripts, `RUN`, `EXIT/EX`,
  `EXITALL/EX1`, error model (`$ERROR`, `SETERR/CLERR`), `QUIET`. ✅
- **Arrays + expressions.** ✅ (vm has array auto-extend; expr evaluator).
- **Expression functions — FULL QN set + JOIN:** `LEN UPPER LOWER STRIP LSTRIP
  RSTRIP TRIMLEFT TRIMRIGHT SUBSTR INDEX CONTAINS REPLACE CHR ASC SEL VAR SPLIT
  SPLITS JOIN`. ✅
- **Special-variable namespace — QN's entire set is already declared** in `env.rs`:
  `$AU $CB $CC $CF $CFL $CH $CL $CP $CT $CWD $CX $CY $DA $DF $DN $DS $ED $EDFONT
  $ERROR $ET $FB $FE $FI $FP $FS $GC $LC $LN $LS $MB $MO $NL $NP $OF $OS $PA $PS
  $QNDIR $RC $RE $RL $RV $SC $SE $SF $SL $SLASH $TB $TC $TM $TX $UIFONT $UISCALE
  $VE $VR $WC`. ⚠️ **Names reserved, but many return placeholders** until the
  backing feature exists (tabs, clipboard, fonts, selection geometry, …). Wiring
  each to real state is part of the tranches below — the *contract* is fixed.
- **Libraries (.qnu):** `LOADLIB ADDLIB RELOADLIB UNLOADLIB LISTLIB` + the `.qnu`
  format (frontmatter, `SUB…ENDSUB`, `# DOC:` strings, name-shadowing rules). ✅
- **Key bindings / keymaps:** `LOADKBD` + live-swappable keymaps (the `.kbd` model;
  QN's `.qnk` is the analogue). Press-a-key binding UI is GUI work (tranche 7). ◑
- **Core editor verbs:** `INSERT(WR-ish) BS DEL DELSEL UNDO REDO NEW(NE) OPEN(CA)
  SAVE(SA) BUFNEXT(NX) BUFPREV(PX) BUFGO BUFLIST CMLINE SUBMIT CANCEL QUIT/QQ
  ENCODING HIGHLIGHT TOPDF(PDF)`. ✅
- **Motion (basic):** `LEFT RIGHT UP DOWN HOME(LB) END(LE) TOP(TF) BOTTOM(BF)` +
  Shift-extend selection (M6). ✅
- **Discovery:** `LISTLIB LISTVAR HISTORY HELP COMMANDS ABOUT EXISTS ENV`. ✅

Legend: ✅ done · ◑ partial · ⬜ not yet · 🔒 trust-gated · ✂ adapt/out-of-scope.

---

## B. Gap catalog — features to add (by category)

### B1. Search & Replace ◑ (LITERAL CORE BUILT 2026-06-10; /R regex + CV verify ⬜)
**BUILT (literal core):** `SE/SEB/SEA/SEBA` (find fwd/back, case-insensitive default,
`A`=absolute) + `CH/CHA` (replace-all). Self-delimiting args (`SE /pat/`, `CH /a/b/`;
first non-blank char = delimiter; `\<delim>`/`\n`/`\t` escapes; bare `SE foo` allowed).
Order-free flags `/W` whole-word, `/T` from-top, `/A` absolute, `/B` backward. Empty
`SE` repeats last search. Found match → selection (caret on leading edge); forward walks
from match end so repeated `SE` steps through. Replace-all = ONE undo unit over the
affected span; from cursor→end by default, `/T`=whole doc. `$CC` = replace count;
not-found trips `$ERROR` (trappable, not fatal). Engine: `Buffer::search` + `find_all`
in text.rs (literal, Unicode case-fold, byte-offset exact via decoded↔rope mapping);
`Document::search_select`/`replace_all`; `EditorOp::Search(SearchSpec)`; `parse_search`
in vm.rs. 8 e2e tests (search_*, replace_all_*). **STILL ⬜:** `/R` regex (needs `regex`
crate — `parse_search` already errors clearly on `/R`); `CV/CVA` interactive change-verify
(Y/N/A/Q walk — needs a GUI keypress-prompt loop). Original stub notes below.
- **Verbs:** `SE/SEA/SEB/SEBA` (find; backward, case-absolute), `CH/CHA/CI/CIA`
  (replace all), `CV/CVA` (change-verify, Y/N/A/Q walk).
- **Modifiers (order-free):** `/W` whole-word, `/T` from-top, `/R` regex,
  `B` backward, `A` case-absolute.
- **Self-delimiting argument syntax** (`SE /pat/`, `CH /find/repl/`; any delimiter).
- **Escape sequences** in patterns/replacements; after-match behavior; `$CC`
  replace-count; not-found trips `$ERROR`.
- XyScript fit: byte-engine search over the rope; `regex` crate for `/R`. Wire
  `$CC`. This unlocks a huge fraction of real editing.

### B2. Rich cursor motion ⬜ (HIGH)
QN has ~25 motion verbs; XPL has 8. Add:
- **Word/sentence/paragraph:** `NW/PW NS/PS NP/PP`.
- **Line:** `NL/PL` (start of next/prev line), `LU/LD` (line up/down), `CL/CR`
  (left/right *with* line-wrap), `LL/LR` (*without* wrap), `NC/PC` (char synonyms).
- **Page/screen:** `PD/PU` (page), `HM/BS` (top/bottom of visible screen),
  `MU/MD` (scroll text up/down one line, cursor fixed — XyWrite semantics).
- **Absolute:** `JMP n` (char offset).
- Needs: word/sentence/paragraph segmentation (Unicode UAX-29 via
  `unicode-segmentation` — already on the radar for spell-check), and viewport
  awareness (the GUI canvas knows visible rows) for `HM/BS/MU/MD/PD/PU`.

### B3. Unit selection — the "defined text" model ◑ (HIGH — DF/DZ/DFA/XD BUILT 2026-06-09)
**STATUS:** built and green (M7/tranche-1a). **Model decided live: AUTO-TRACK** (QN/
XyWrite-standard), *after* trying the roam-and-mark "Model B" and finding it
unintuitive (2026-06-09). Final behavior: `DF` opens a selection and **plain cursor
motion extends/shrinks it live** (no Shift, no separate mark step); `DF`/`DZ` close it
(an empty span cancels); `XD`/`YD` cancel; `DFA` selects all. **`DM` re-opens a just-
closed selection for continued extending** (boundaries unchanged; the caret is on an
edge so the anchor is already the far edge → just turns auto-track back on). Usable
without PS in the post-`DZ` "keep going" case; reshaping a *roamed* block to the caret
arrives with PS (B17). Shift+motion
auto-extend (M6) coexists as the quick path. Engine: `Buffer` gained just a `defining:
bool`; `prepare_move` extends when `extend || defining`; selection stays the standard
`ordered(anchor, cursor)` (so undo + the GUI's normal render path are untouched — no
decoupled head, no custom caret). Verbs wired VM→`EditorOp::Define(DefineKind)`→host→
`Document`. Tests: 5 buffer unit tests + e2e. **Still ⬜ in B3:** `DW`/`DWONLY`/`DL`/
`DS`/`DP` (need B2 segmentation), and in-doc `CP`/`MV`.
NOTE: the long write-up just below describes the *abandoned* roam-and-mark model —
kept for history; the shipped behavior is auto-track as above.

XyScript adopts XyWrite's **open (live) selection** model, not just a static
anchor+cursor span: a selection can be *open* (being defined — it tracks cursor
moves and can be modified) and then *closed* (frozen). [CLD refinement 2026-06-09]
- **`DF`** — toggle: if no selection is open, **open** one (anchor at cursor); if one
  is open, **close** it. If at close the span is empty (start == end), cancel it.
- **`DZ`** (new) — **close** an open selection (same as `DF`-when-open); a no-op when
  none is open. Lets a macro close unconditionally without risk of toggling one open.
- **`DM`** (new) — **modify** the open selection toward the cursor: if the cursor is
  *outside* the span, extend the selection to it; if *inside*, shrink to it (lowering
  `$DN`). Leaves the selection **open**. Idiom: `DM` → optional motion → `DZ`.
- **Unit defines:** `DFA` (all, ≈ existing select-all), `DW`/`DWONLY` (word), `DL`
  (line), `DS` (sentence), `DP` (paragraph), `XD`/`YD` (cancel).
- **In-document move/copy:** `CP` (copy selection to cursor), `MV` (move selection
  to cursor) — distinct from the system clipboard (B7) and the deletion buffer (B4).
  ✅ **BUILT 2026-06-10.** PS-companion verbs (need the caret *off* the selection,
  i.e. the decoupled/PS-roam state; coupled `CP` duplicates adjacent, coupled `MV`
  is a no-op). `CP` = `Buffer::insert_keeping_selection` (rides the block along),
  recorded, leaves the original selected. `MV` = two recorded edits (delete origin
  → insert at the deletion-adjusted dest), moved text ends selected; undo takes two
  steps. Both error "no text is selected" with none. EditorOps `CopySelection`/
  `MoveSelection`; verbs `CP`/`MV`. Tests: 4 document + e2e.
- Engine work: add an **`open`/defining flag** to the buffer's selection state (today
  it's a static `anchor: Option<usize>`); open-selection tracking is what both `DM`
  and Shift-motion feed. Pairs with B2 segmentation for the unit-define verbs.

### B4. Deletion variants ◑ (RD/UD/$XB/DN BUILT 2026-06-09; case in B5; BD/RC/RL/RS ⬜)
**BUILT:** the **deletion buffer** — `RD` (cut selection → `$XB`, error if none), `UD`
(paste `$XB` at caret, buffer kept; PS-aware = doesn't replace a block under PS), `$XB`
special (declared in `env.rs`). `DN` (delete selection; trappable "no text is selected"
error if none — the QN divergence; verified it does NOT char-delete). Deletion buffer is
a single slot on `AppHost`, distinct from clipboard/`CP`/`MV`. Tests: e2e cut-move-paste,
dn-noop, + case. **Still ⬜:** `BD`/`RC` (plain backspace/forward-delete verbs — the
engine ops exist via `feed_key`, just no named verbs), `RL` (rest of line), `RS` (sentence,
needs B2 segmentation).
`BD` (backspace), `RC` (rub-out/forward-delete), `RL` (delete rest of line),
`RS` (delete sentence).
- **`DN`** — delete the current selection. **If no selection is active, raise a
  "no text is selected" error** (trips `$ERROR`) — a deliberate divergence from QN,
  where bare `DN` deletes the single character under the cursor. [CLD refinement
  2026-06-09] (Use `RC`/`BD` for character deletion.)

**Deletion buffer (new — XyWrite-style, distinct from the system clipboard):** [CLD
refinement 2026-06-09]
- **`RD`** — copy the selection into the **deletion buffer**, *then* delete the
  selection. (Was plain "delete selection"; now it also captures what it removed.)
- **`UD`** (new, companion to `RD`) — paste the deletion buffer at the insertion
  point; does **not** clear the buffer (it persists until the next `RD`), so one cut
  can be pasted repeatedly.
- **`$XB`** (new special var, name tentative) — returns the current deletion-buffer
  contents (companion to `$CB` for the OS clipboard). Must be **added** to `env.rs`;
  it is *not* part of QN's variable set. Alt names to weigh: `$DB` (clashes with no
  existing var) / `$KB`. [CLD refinement 2026-06-09]
- Idiom: **`RD` → motion → `UD` = move text.** The deletion buffer is a single-slot
  "kill register," separate from `CP`/`MV` (B3, in-document) and the OS clipboard
  (B7, `CY`/`APPEND`/`PREPEND`).

### B5. Case ✅ (BUILT 2026-06-09)
`LC` lower, `UC` upper, `CC` swap-case — act on the selection, else the char under the
cursor. `Document::change_case` (recorded edit; Unicode-correct via `to_lowercase`/
`to_uppercase`, length changes handled; selection re-established on the transformed text;
no-op when already in that case = no undo unit). Tests: selection-case + undo, swap under
cursor, e2e UC/LC.

### B6. Insert / overstrike mode ⬜ (MED)
`SIN/SOV/CIN/TIN/TOV`. Adds an **overstrike** editing mode to the buffer/editor
(currently insert-only). Status surfaced via cursor shape + `$`-var.

### B7. Clipboard ⬜ (MED-HIGH)
- System clipboard integration (egui/`arboard`) for Cut/Copy/Paste.
- `CLIP/CY <text>` (set, with `$`-sub + escapes), bare `CLIP` clears.
- `APPEND`/`PREPEND` selection → build a multi-piece clipboard for one paste.
- `$CB` reads clipboard (already declared).

### B8. Counts ✅ (BUILT 2026-06-10)
`CHARCOUNT`/`WORDCOUNT` verbs report the count of the **selection if one exists, else the
whole document** to the status line (`"Selection: N words"` / `"Document: N characters"`,
singular/plural aware; error on extra args). `$WC` special = whole-document word count
(`$SL` = selection char count, `$LC` = doc line count, all wired). Word = any non-empty
ASCII-whitespace-delimited run (matches QN's documented semantics, encoding-agnostic since
whitespace bytes are shared). Counts **stream over the rope** (`ByteRope::word_count`/
`char_count`, `count_words`) — no full-document materialization (100MB word count ≈18ms,
char count ≈24ms). Engine in `byte_rope.rs` + `text.rs`; `EditorOp::WordCount`/`CharCount`;
`vm.rs` dispatch. 2 tests (`counts_report_doc_and_selection`, `counts_singular_…`).

### B9. Insertion & file I/O ⬜ (MED)
- `WR/WRL <text>` insert with escape sequences (≈ current `INSERT`, formalize +
  `\n`/`\t`/`\dNNN`/`\xHH`/`\uHHHH`).
- `RF <path> <var>` / `WF <var> <path>` (`WF/V` prompting variant) — file ↔ variable.
- `MERGE/ME <path>` — insert a file's contents at the cursor (replace selection).

### B10. Filesystem ops 🔒 (MED — power features) — CUT 1 ✅ + CUT 2 ✅
`CD`/`CD/D` (working dir), `COPY`(/NV), `DEL`(/NV)/`ERNV`, `RENAME/REN`(/NV).
The **`DIR`/`LS` browser** is a standout QN feature worth reproducing faithfully:
scrollable listing, **XyWrite express-keys** (type-to-find substring, Backspace
shortens, F3 find-again, footer shows buffer), Enter=edit, Ctrl+Enter=OS-launch,
**bare Ctrl-tap=copy path**, Del=delete-with-confirm, Windows-DIR switches.
→ A great future "directory screen" (ties to the tabs-vs-windows question).

(Note: the compact names-only view switch is `/B` — earlier notes below say `/W`; it was
renamed 2026-06-11.)
CUT 1 DONE (listing-into-a-buffer model, headless-first): `AppHost.cwd` + `CD`/`CD/D`
(managed in-process, `$CWD` points at it; `/D` accepted as harmless). `dir.rs`
(`list_dir` dirs-first/case-insensitive + `..`; `render_listing` one entry per line,
dirs trailing-`\`, line N → entry N). `Document.dir: Option<DirState>` carries the
listing (entries + express-key `find`); `DIR`/`LS [path]` opens it as its own buffer
(reuses scratch); `dir_enter` descends (re-list in place, follows cwd) or opens a file
**in place**. Express-keys: printables → type-to-find seek, Backspace shortens, Esc
clears; footer shows base + find. GUI routes Enter/printables/Backspace/Esc to the
listing only when it's active AND the cmline is closed. Verbatim-open invariant locked
by a regression test (no header EVER added on open, any extension). app 157 + green.
`NEWWIN 0|1` + `$NW` (XyScript extension, not QN): `NEWWIN 1` opens a file from the
listing in a NEW buffer (listing stays behind to keep picking); `NEWWIN 0` (default)
replaces the listing in place. `$NW` reads the flag (0|1). State on `AppHost.new_window`;
`dir_enter` branches add_or_reuse vs replace-in-place.
CUT 2 SAFE TIER DONE (2026-06-11, non-destructive half): cwd-relative `OPEN` (real disk
joins relative paths onto cwd; virtual_fs still keyed by bare name); **F3 find-again**
(`dir_find_again` — next express-key match forward, wraps); **copy-path** (Ctrl+C in the
listing → entry path to `$CB`, GUI mirrors to OS clipboard); **size/date columns** =
default listing (`name  size  date`, dirs `<DIR>`, date `YYYY-MM-DD HH:MM` **UTC** via
hand-rolled civil-date math, no date crate) with **`DIR /W`** for the compact names-only
view (cut-1 style); `wide` flag stored on `DirState`, preserved across `dir_enter`
descents. Line N → entry N invariant holds in both views. app 162 + green.
CUT 2 DESTRUCTIVE TIER DONE (2026-06-11, CONFIRM-ONLY interactive model — no persistent
trust flag): **browser Del** = delete-with-confirm (in-app y/n sub-mode, `..` protected,
file or EMPTY dir only — no recursive wipe — then re-lists); **Ctrl+Enter** = OS-launch
(`cmd /C start` on win, `open`/`xdg-open` else; detached child); **file verbs** `COPY
<src> <dst>`, `DEL <path>`, `RENAME`/`REN <from> <to>` (paths resolve vs cwd; quote-aware
two-path parse; copy/rename onto a dir drops the file inside; virtual_fs branches for
headless tests). DEL & COPY are arg-overloaded: bare `DEL`/`COPY` stay editor delete-
forward / clipboard-copy, with a path they hit the filesystem. app 165 + xpl green.
FILESPEC (2026-06-11): `DIR [/W] [<dir>][<glob>]` accepts a wildcard filespec — last path
component is a `*`/`?` glob (case-insensitive) over the preceding dir (cwd if none); an
arg naming an existing dir still lists it whole. `..` always kept; filter persists across
re-list (e.g. after delete) but is dropped on descent. `dir.rs::glob_match`/`filter_entries`;
`DirState.filter`; `AppHost::resolve_dir_spec`. Status: "N items matching <pat> in <base>".
LOCAL-TIME + SWITCHES (2026-06-11): date column now shows **local time** (offset resolved
once at GUI startup via a tiny Windows `GetTimeZoneInformation` FFI → `dir::set_local_offset`;
`civil_format` stays pure/UTC-default so headless tests are deterministic; other OSes = UTC
for now). Broader **DIR switches**: `/O{N,S,D,E}` order (name/size/date/ext, optional `:`),
`/R` reverse (within dirs/files groups; dirs stay grouped first), `/A{D,F}` dirs-only/
files-only (always keeps `..`), and `/B` bare (names-only one-per-line; renamed from `/W`
since our format is bare-list, not Windows' multi-column wide grid — `/W` left free for a
real wide/grid view later). Parsed in `vm::parse_dir` (DIR now
early-dispatches before `validate_switches`); `DirOpts{wide,reverse,order,only}` carried on
`EditorOp::Dir` + stored in `DirState` (persists across descent/re-list, filespec dropped on
descent). `dir::sort_entries`/`apply_only`. NOTE divergence from DOS: reverse is `/R` (not
`/O:-X`) and files-only is `/AF` (not `/A:-D`) — the `-` sigil collides with switch syntax.
RECURSE + ATTRIBUTES (2026-06-11): `/S` recurses subdirectories (entry names become
base-relative paths so line N → entry N holds; sorted by path = folder-grouped; symlinked
dirs listed but not descended → no cycles; filespec matches the basename; Enter on a
subdir re-lists it non-recursively, dropping `/S`). Hidden/system now **hidden by default**
(DOS-faithful, behavior change accepted by user): `/AH` shows hidden, `/AS` shows system,
bare `/A` shows all; combinable like `/ADH`. Attrs read via Windows `Metadata::file_attributes`
(non-Windows = none). `dir::list_dir_recursive`, `apply_attr` (supersedes `apply_only`),
`sort_entries(.., group_dirs)`. `DirOpts` gained `recurse/show_hidden/show_system`.
STILL DEFERRED (not blocking): the trust/script-gate (only needed once untrusted-script
sources exist — interactive use is the confirm-only model). B10 is otherwise complete.

### B11. Sort ✅ (LOW-MED)
`SORT [/W /R /D /F:n /N:n]` over selected lines (word-key, reverse, dedup, skip-n,
take-n). Self-contained; do after selection+segmentation.
DONE: engine in `xyscript-app/src/sort.rs` (decorate-sort-undecorate, stable; letter
key = alphanumerics-only lowercased, `/W` = lowercased word tokens, `/F:n`+`/N:n`
char-window the key before stripping, `/R` stable-descending, `/D` dedup by key,
trailing-newline preserved). `EditorOp::Sort(SortSpec)` + `parse_sort` (vm.rs, switches
order-free, verb-attached or leading, value-bearing so parsed before `validate_switches`);
`Document::sort_selection` (one undo unit, re-selects, refreshes highlights). Status:
`SORT{flags}: sorted N line(s)` or `… N -> M lines (K dup(s) removed)`; no selection →
trappable `SORT: no text is selected`. 8 tests (5 engine + 3 editor).

### B12. Date / time / stopwatch ✅ (LOW)
`DA` (insert date/time, last-chosen format), `ZT/ET/ETS` (stopwatch reset / insert /
status). `$DA $TM $ET` declared. **Note:** date handling must respect the existing
`stamp-date.qns` workflow and the feature-notes "date vars" item.
DONE (2026-06-11): the feature-notes **AutoIt-style date vars** are wired — `$YEAR $MON
$MDAY $HOUR $MIN $SEC $MSEC $WDAY` (1-7 Sun-Sat) `$YDAY` (1-366), all LOCAL (offset from
the same `dir::set_local_offset` used by the DIR column; UTC default in headless tests).
`$DA`=`YYYY-MM-DD`, `$TM`=`HH:MM:SS`, `$ET`=stopwatch `H:MM:SS`, `$ETS`=stopwatch elapsed
**seconds** (Int). Verbs: `DA` inserts
`YYYY-MM-DD HH:MM:SS`; `ZT` resets the stopwatch; `ET` inserts elapsed. (`ETS` removed —
report elapsed with `PR $ET`.) Pure civil-date math in new `xyscript-app/src/clock.rs` (`fields_from_millis`,
tested with fixed timestamps incl. leap-year/midnight-cross); stopwatch = `AppHost.stopwatch_start:
Instant` + `EditorOp::ResetStopwatch`. AutoIt fields added to `env.rs` specials. 5 tests.

### B13. Sequencing ◑ (LOW-MED) — WAIT done, READKEY deferred
`WAIT [ms]` (sleep / await async like styled-PDF), `READKEY/RK [/NV]` (pause for one
keystroke → `$<var>` + `$SC`). Useful for interactive scripts.
`WAIT <ms>` DONE (2026-06-11): `EditorOp::Wait(u64)` → AppHost blocking `thread::sleep`
(capped 30s; headless host no-ops so tests don't sleep). Trappable error on a non-number.
**READKEY DEFERRED** (user, 2026-06-11): pausing a synchronous script for a keypress and
resuming arbitrary following commands needs VM suspend/resume — fold into the async/
continuation (background-jobs) architecture, which needs the same machinery. See
[the background-jobs memory note] / the B14 FUTURE block. (CV's suspend works only because
its post-key step is a FIXED host op, not arbitrary XPL — that pattern doesn't generalize.)

### B14. Shell-out ✅ (MED — trust-gated)
`X` (run, `$RC`), `X/NV` (fire-and-forget), `XY` (stdout→buffer), `XV` (stdout→var).
Power feature; allow for the local user's own scripts, surface clearly, revisit when
untrusted scripts exist. Cross-ref the Xyperlink XL2 security model.
DONE (2026-06-11): trust model = **session enable flag, default OFF** (`TRUST 1`/`TRUST 0`;
`$TR` reads it) — X/XY/XV give a trappable "shell-out is disabled; enable it with TRUST 1"
until enabled, so a stray loaded macro can't run commands. Once on: `X <cmd>` runs to
completion (`$RC` exit code, `$XO` merged stdout+stderr, status "exit code N"); `XY <cmd>`
inserts stdout into the buffer (recorded/undoable); `XV <var> <cmd>` captures stdout into a
user var; `X/NV <cmd>` fires detached. Runs via the OS shell with the shell's own parsing
(`cmd /C` + `raw_arg` on Windows so pipes/redirects work; `sh -c` elsewhere). Host trait
`shell`/`shell_spawn`; `EditorOp::SetTrust`; specials RC/TR/XO. X/XY/XV self-delimiting in
the splitter; `X` accepts `/NV`. NOTE: blocking — a long command freezes the GUI (a
threaded/streaming runner is a future enhancement). The "untrusted-script gate" deferral
is now subsumed: TRUST is that gate (session-scoped, default safe).

**FUTURE (user-requested 2026-06-11): background command execution.** Two layers:
(1) a **threaded streaming runner** for `X`/`XY`/`XV` so a long command doesn't freeze the
GUI (worker thread + incremental stdout streaming, QN-style); (2) a **general "run in the
background" facility** — syntax (e.g. `X/BG <cmd>`, or a `BG` prefix) + architecture (an
async job table on `AppHost`, output delivered to the editor loop via an `mpsc` channel
drained per-frame, completion sets `$RC`/`$XO`, plus `JOBS`/`KILL <id>`). Keep it general
so non-shell long ops (PDF export, big loads) can background too. Hold the per-frame
O(viewport) invariant for streamed inserts. Slot near tranche 7 (power + scripting polish).

### B15. View toggles ◑ (MED — GUI) — most DONE 2026-06-12
- **Preview pane** toggle ✅ (M5.1).
- **Menu-bar** ✅ `SMB/CMB/TMB` + `$MB` (GUI hides/shows the top panel).
- **Visible returns** ✅ `SVR/CVR/TVR` + `$VR` (dim ¶ at hard newlines via `layout_runs`,
  not on soft wraps or the last line).
- **Visible whitespace** ✅ (NEW, user-requested) `SWS/CWS/TWS` + `$WS` (dot per space,
  `>` arrow per tab; direct pixel marks per glyph). Added `WS` to SPECIAL_NAMES.
  ALSO: Tab now inserts `\t` — sample kbd `15=insert "\t"` + runtime captures Tab (was `15=KF`
  → egui ate it for focus nav). `TAB` added to the runtime's always-capture set.
- **Line numbers** ✅ gutter `SLN/CLN/TLN` + **`$LNS`**. GUI: canvas rendered at REDUCED width
  and offset right by the gutter (so `resp.rect` carries the offset → hit-test/caret/IME stay
  automatic, no threading); numbers painted with `ui.painter().text()` (egui monospace, dim),
  right-aligned, aligned to each logical line's first visual row via `gutter_lines:(line,y)`
  collected in `raster` from `layout_runs` (so wraps don't get their own number). Gutter width
  from `Buffer::line_count()` digit count.
- **Notes/file pane** `SNP/CNP/TNP` + `$NP` — DEFERRED to its own design (overlaps the DIR
  browser / directory-screen + tabs-vs-windows decision).
- Pattern: `EditorOp::SetView{flag:ViewFlag, kind:ViewToggle}` + AppHost flags + `$MB/$VR/$WS/
  $LNS` reads + `Editor` getters; GUI reads each frame. Mirrors `SPS/CPS/TPS`.
- `$MB $NP $VR $GC $LNS $WS` declared.

### B16. UI sizing ✅ (DONE 2026-06-12 — adapt Tk→egui)
Value-carrying host knobs (`EditorOp::SetUiSize{UiSizeKind,f32}` + `UiReset`) + AppHost f32
state (clamped) + `$`-vars + `Editor` getters; GUI reads/applies each frame. Bad number =
trappable (→ `last_error`, no crash). Verbs:
- **`UISCALE <f>`** ✅ egui global zoom (`ctx.set_zoom_factor`; whole UI). `$UISCALE`. (Canvas
  text BLURS above ~1.5× — it's a point-resolution pixel image; crisp-canvas/HiDPI raster is a
  noted refinement — would also help hidpi.)
- **`UIFONT <pt>`** ✅ chrome (menu/status/cmline) font → egui Body/Button/Monospace text styles
  (`global_style_mut`). `$UIFONT`. Default 14pt.
- **`EDFONT <pt>`** ✅ editor text size. **`LEADING`/`LSP <%>`** ✅ line spacing = % of EDFONT
  (`line_height = round(EDFONT × LSP/100)`; default 145% ≈ old 22/32). `$EDFONT`/`$LSP`. The old
  `const FONT_SIZE/LINE_HEIGHT` are now `GuiApp.font_size/line_height` fields recomputed each
  `ui` frame; all canvas-metric code (raster, hit, paging, gutter/¶/whitespace, caret, IME) reads
  them, so everything scales + stays hit-accurate.
- **`OF`/`OFL`/`OFR <cols>`** ✅ left+right / left / right margins in char columns (XyScript splits
  QN's single `OF` into three; `OF` sets both). `$OF`(=left)/`$OFL`/`$OFR`. GUI: left margin adds
  to the canvas left-offset (`add_space`, like the gutter → hit/caret/IME automatic); right margin
  narrows the canvas (text wraps earlier). Width = `cols × (font_size × 0.6)` APPROX cell — exact
  glyph-advance measurement is a small refinement.
- **`UIRESET`** ✅ resets all to defaults.
- `$UIFONT $EDFONT $UISCALE $LSP $OF $OFL $OFR` declared (OFL/OFR/LSP added this session).

### B17. Persistent selection (+ DM) ✅ (BUILT 2026-06-09 — PS-1 motion/DM/render + PS-2 edits)
**PS-2 (edits under PS) DONE:** under PS, typing/`BS`/`Del` **don't replace** the block —
they act at the caret and the block **rides along** (offsets shift so the same chars stay
selected: start-gravity `<=`, end-gravity `<`; deletes clamp/collapse). Engine in
`Buffer.insert/delete_back/delete_forward` (freeze `sel_head` then `shift_selection_for_*`);
`Document` branches the undo Op so it stays **byte-exact** (plain insert / char-delete, not
a selection-replace). Known limy: undo of a PS edit re-couples the selection cosmetically
(text is always correct) — acceptable; widening the undo record was deliberately avoided.
Tests: 3 buffer + 1 document-undo. app 124 + 7 e2e.

**STATUS (PS-1):** built & green. Shipped: `SPS`/`CPS`/`TPS` + `$PS`
(global flag on `AppHost`, synced onto the active buffer at the top of `editor_op`);
**decoupled selection storage** (`Buffer.sel_head`) so a frozen block persists while the
caret roams off it under PS; **PS-motion** (plain motion keeps the block + decouples;
non-PS clears as before); **roamed `DM`** (re-anchors at the far edge → reshapes the
block to the caret + resumes live extend); **custom GUI render** for the roamed state
(highlight the block via cosmic cursor=far-edge + transparent caret, paint the real caret
at the roamed spot). `$DF` also wired. **DEFERRED to a follow-up (PS-2):** edits under PS
— *don't-replace* (typing/`BS`/`Del` leave the block) and *ride-along* offset tracking.
Until then, editing while a PS block is active behaves like non-PS (replaces). Tests:
3 buffer unit + 1 e2e.

### (history) B17 original plan — PS decisions
`SPS/CPS/TPS` toggle; `$PS` reads state (declared). **Decisions locked 2026-06-09:**
- **`DM` lives here** (un-deprecated). With PS on, a frozen `DF`/`DZ` block persists
  while the cursor roams off it; **`DM` re-grabs the block and resumes live extending**:
  set the **anchor to the edge farther from the caret**, turn auto-track back ON, and
  leave the caret put. The selection becomes `ordered(far-edge, caret)` and is **live
  again** — subsequent motion extends/shrinks it with every step, no repeat `DM`. It
  stays open until **`DZ`/`DF`** closes it; **`DM` never closes the selection.** If the
  caret is already at an edge, the block is unchanged (`DM` "does nothing" visibly) but
  becomes live, so moving off that edge keeps extending. (The "nearest edge follows the
  caret" outcome falls out of anchoring at the far edge.) Without PS, plain motion
  cancels the block first, so `DM` is in practice a PS-companion verb — but it needs no
  explicit PS gate: it just re-opens whatever selection is current.
- **Requires decoupled selection storage:** for a block to persist while the caret
  roams, the selection must be stored **independently of the cursor** (a fixed span,
  not derived from `anchor↔cursor`). And the GUI needs the **custom caret render**
  back for the roamed state (cosmic ties its highlight to its cursor): draw the block
  via cosmic with its cursor at the block's far end + transparent caret, then paint
  the real caret at the roamed cursor. (This is the rendering removed when DF went
  auto-track; here it's genuinely required, not an unintuitive choice.)
- **Global / app-wide** mode (QN-faithful), applies to the active document.
- **Stricter-than-QN paste rule:** under PS, **nothing replaces the selection**
  except `XD`/`YD` (clear) and the explicit cut/delete (`RD`/`DN`). Typing, paste
  (`UD`/Ctrl+V), `BD`/`RC` all just insert/delete at the cursor and the highlight
  **rides along** (selection offsets adjust so the *same characters* stay selected).
  (QN carves out Ctrl+V as replacing; we don't — more internally consistent.)
- **Behavior delta (PS on):** plain motion keeps the selection (doesn't clear);
  every ordinary edit rides along; only `RD`/`DN` consume it; `XD`/`YD` clear it.
- **Engine work:** a global `persistent` flag (AppHost) consulted by the buffer's
  motion/edit paths; **ride-along = standard marker adjustment** of the stored
  selection offsets after each edit when PS is on. Layers on the DF model (defining =
  *building*; PS = *persistence after close*). Must be built **alongside** the 1a
  operations so `RD`/`UD`/`CP`/`MV`/case are PS-aware from the start.

### B18. Save/file lifecycle ◐ (MED) — Phase 1 DONE 2026-06-13
Phased build. **Phase 1 (file I/O) ✅ 2026-06-13:** `SAAS` (save-as: `SAAS <path>` =
save+adopt like `SAVE <path>`; bare `SAAS` → GUI Save-As dialog via host `save_as_requested`
flag drained by the GUI), `SAD <path>` (save selection to new file, byte-exact via
`slice_bytes(selection_span)`→`write_doc_bytes`; doc untouched; trappable if no selection),
`REC`/`RECALL` (revert to last save: `read_doc_bytes`+`Document::load`; trappable if never
saved). Ops: `SaveAsPrompt`/`SaveSelection`/`Revert`.
**Phase 2 (read-only) ✅ 2026-06-13:** `RO` (bare=toggle, `RO on|1`/`off|0` explicit) +
`$RO` (1/0). Per-document `Document.read_only`; `EditorOp::SetReadOnly(ViewToggle)`. Edit-
gating at the single chokepoint (`editor_op`): free fn `op_mutates_buffer` rejects buffer-
content edits (typing/Delete/Rub/Paste-deletion/DeleteSel/Case/CP/MV/Sort/Newline/Undo/Redo,
`Search` w/ replace, `Clip` Cut|Paste) with trappable "document is read-only (RO to unlock)"
(folds to status+$ERROR via host_op) — only when `target==Document` (the cmline is never
locked, so commands still run). Motion/selection/find/save/file/view all allowed. GUI status
shows `[RO]` next to `[+]`. Test `editor::ro_locks_edits_unlocks_and_reads_ro_special`.
**NAMING NOTE:** plan's `RW`/`RE` were taken by delete verbs (rub-word / rub-to-EOL) → used `RO`.
**Phase 3a (multi-tab close/save-all) ✅ 2026-06-13:** `AB` = close active buffer immediately,
discard unsaved, never-zero (last close → fresh empty), focus→Document. `SL` = save all named+
modified, skip unnamed (kept open), status "saved N, skipped M unnamed". `SF` = SL then
`docs.retain(is_modified)` (close clean, keep unsaved-unnamed), ≥1 buffer, status "saved N,
closed M, kept K unsaved". Ops `CloseBuffer`/`SaveAll`/`SaveCloseAll` + `save_all_modified` helper.
DECIDED SEMANTICS (user): bare `AB` = immediate discard (NO `/NV` — bare is already no-verify);
`AB/V` = close-with-save-prompt (3b, GUI); `ABD` = delete file + close with a destructive confirm
(3b, GUI). Tests: `ab_closes_the_active_buffer_and_never_leaves_zero`,
`sl_saves_named_skips_unnamed_and_sf_closes_clean_keeping_unsaved`. (Also backfilled this session's
verbs into `BUILTIN_VERBS`; a background task tracks the broader B15/B16/colour backfill.)
**Phase 3b (GUI prompts) ✅ 2026-06-13:** `AB/V` = close-with-save-prompt (unmodified → close now;
modified → status prompt "Save changes? (y) save (n) discard (esc) cancel"; untitled+yes → pops
Save-As, keeps open). `ABD` = delete file from disk + close, "Delete <name>? (y/n)" confirm;
trappable on a never-saved buffer. Ops `CloseBufferVerify`/`DeleteNote`; `pending_close` state
(Save/DeleteNote) + `close_prompt()` + `resolve_close(CloseAnswer{Yes,No,Cancel})`; shared
`close_active_buffer()` helper. GUI: armed close-prompt owns the keyboard (top input branch, Y/N/Esc)
+ status-line chain + `surface_command_feedback`. `AB/V` switch via validate_switches `"AB"=>&["V"]`.
Test `editor::abv_save_prompt_and_abd_delete_confirm`. **B18 Phase 3 (multi-tab) COMPLETE.**
**Phase 4 (config snapshots) ✅ 2026-06-13:** `SAVECFG [path]` / `LOADCFG [path]` (default
`xyscript.cfg`, cwd-relative). CONFIG IS GENERATED XPL — SaveCfg emits the verbs that recreate the
current look (colours FG/BG/UIFG/UIBG, sizing UISCALE/UIFONT/EDFONT/LSP/OFL/OFR, view toggles
MB/VR/WS/LNS/GC as S/C verbs), each preceded by a `#` comment + blank-line separated; LoadCfg
reads it + runs it via the `pending_run` deferral (RUNHERE pattern). EXCLUDES TRUST (security),
PS/NEWWIN (session), RO (per-doc). Ops `SaveCfg`/`LoadCfg`; `AppHost::config_script()`. Test
`editor::savecfg_loadcfg_round_trips_appearance`.
**`AUTOSAVE` ✅ 2026-06-13 (did NOT need async after all):** `AUTOSAVE <secs>` / `AUTOSAVE off`
(0=off) + `$AUTOSAVE`. `AppHost.autosave_secs` + `SetAutosave` op + `autosave()` (reuses
`save_all_modified` → saves all named+modified, skips unnamed). GUI holds `last_autosave: Instant`,
checks each frame, and uses `ctx.request_repaint_after(interval)` (the winit runtime already honors
egui repaint_after via `ControlFlow::WaitUntil`) so it fires WHILE IDLE — no threads/interpreter
change. Test `editor::autosave_setting_reads_back_and_saves_named_modified`. **B18 COMPLETE.**

### B19. Lock keys ✂ (LOW — mostly OS-level)
`SCL/CCL/TCL` (Caps), `SNL…` (Num), `SSL…` (Scroll). Windows-centric; low value in a
cross-platform GUI. Defer or implement as no-ops returning the state vars.

### B20. Display freeze ✅
`DX`/`DO` (suppress/flush refresh) — already in XPL builtins. Wire to the GUI
repaint when scripts do bulk edits.

### B21. Dialogs ◑ (LOW-MED — GUI) — TXT done, INPUT deferred
`TXT` (scrollable monospace dialog), `INPUT` (modal entry → `$<var>`). `PR`/`MSG`
exist; these two are the richer GUI dialogs.
`TXT [/T:"title"] <text>` DONE (2026-06-11): `EditorOp::ShowText{title,text}` →
`AppHost.pending_text` `(title,text)` (taken by the GUI), shown in a non-blocking
`egui::Window` (scrollable, monospace, `wrap_mode=Extend`, fixed id so position survives
title changes, closeable; stays open until dismissed — the script continues immediately).
Body uses `literal_text` like INSERT (quoted → `\n` escapes; bare → verbatim) + `$`-subst.
`/T:"My Title"` (quoted, may contain spaces) or `/T:Bare` sets the window title (default
"Text") — parsed by `vm::parse_txt` (the generic switch parser splits on whitespace, so a
quoted-value switch needs custom parsing).
**INPUT DEFERRED** (same suspend/resume as READKEY — QN's INPUT is a BLOCKING nested-modal
loop; egui can't nest, so the script must pause for the entered value and resume → needs
the async/continuation architecture). Folded in with READKEY; see [[xyscript-background-jobs]].
**FUTURE PR/MSG split (user, 2026-06-11):** today `PR`/`MSG` are synonyms (both → the
status line via `output`). Later: **`MSG` → a GUI message box** (modal); **`PR` → status
line that extends UPWARD for multiline** output; **both accept `\n` args** (already lexes
multiline; missing = the multiline status rendering + the MSG dialog + a separate MSG
signal so they're distinguishable). Do alongside this B21 dialog work.

### B22. RUNHERE ✅ (LOW — neat)
Run the fenced code block under the cursor as XPL. A "literate scripting" feature:
keep runnable XPL in a note, execute in place. Cheap once we detect the enclosing
fence (the highlighter already knows code spans).
DONE (2026-06-11): `RUNHERE` extracts the ```/~~~ fenced block enclosing the DOCUMENT
caret (caret may be on a content OR fence line; unterminated fence = no block) via free
fn `host::fenced_block_at`, and runs it as XPL. Like `SUBMIT`, it can't re-enter the
interpreter mid-dispatch, so `EditorOp::RunHere` stashes the block in `AppHost.pending_run`
and the editor's `post_run` dispatches it after the script returns. Errors (trappable, →
status): "not inside a code block" / "the code block is empty". `RUNHERE` reads the
document caret even when invoked from the cmline. 4 tests (2 unit fence-detection + e2e).

### B23. CMline focus & dual-cursor memory ✅ (MED — refines M5.1)
DONE (2026-06-11): the cmline is now PERSISTENT — `OpenCmdLine` (used by `GH`/the `cmline`
verb/the GUI open) no longer blanks it, so it keeps content+cursor; each buffer (cmline,
doc) already holds its own cursor, so dual-cursor memory falls out. Verbs: `GH` focus
cmline · `GT` focus text (`EditorOp::FocusText`) · `TH` toggle (`ToggleFocus`) · `BC` blank
cmline + caret 0 (`BlankCmdLine`) · `XC` run the cmline's shown text keeping it (`ExecCmdLine`
→ `cmline_exec` flag, editor `post_run` runs it without blanking, unlike SUBMIT). Esc
(`CancelCmdLine`) still discards (blanks); Enter still quick-submits. `$CX` is now
focus-aware (cmline column when focused, else text). GUI "Focus command line" toggle → `TH`
(preserves a half-typed command). GH/GT/BC/XC are verbs (bind to keys for live use). 5 tests.
(Old spec follows.)
The command line and the text window each **remember their own last cursor position**;
the focus verbs restore it. [CLD refinement 2026-06-09]
- **`GH`** — move focus to the CMline, at its last cursor position (no clearing).
- **`GT`** — move focus to the text window, at its last cursor position.
- **`TH`** (new) — toggle focus between CMline and text, restoring each side's
  remembered position.
- **`BC`** — blank the CMline and put the insertion point at CMline position 0
  (optionally stage text). **`XC`** — execute the command currently shown on the CMline.
- **`$CX`** returns the **CMline** cursor column when focus is on the CMline (and the
  text column otherwise). [CLD refinement 2026-06-09]
- Engine/GUI work: store a remembered cursor offset for *both* the CMline buffer and
  the active document; `GH`/`GT`/`TH` set focus + restore. Extends the M5.1 native-
  CMline model (which today tracks only open/closed focus, not a remembered position).
  The CMline already owns its own `Document`/`Buffer`, so it can carry its own cursor.

---

## C. Notable QN behaviors worth preserving (not just verbs)

- **CMline ergonomics:** `;`-chaining of commands, `$`-substitution at the source
  level (+ `SEL()`/`VAR()` to bypass mis-parsing), comments, **history** (✅), and
  **tab-completion** of verbs/paths ⬜. Focus/cursor verbs (`BC`/`XC`/`GH`/`GT`/`TH`
  + dual-cursor memory) are now their own section, **B23**. `SGC/CGC/TGC` (the
  always-on GUI command line — placement Top/Bottom/Hidden already built in M5.1).
- **Self-delimiting search args** (any delimiter char) — adopt verbatim for B1.
- **`DIR` express-keys** (type-to-find) — adopt for B10 and any list UI.
- **APPEND/PREPEND clipboard building** — multi-source gather-then-paste idiom.
- **Read-as-UTF-8-with-replacement** for MERGE/RF so binaries import as text-with-
  junk rather than erroring. (Note: our byte-exact round-trip rule still governs the
  *active document*; this leniency is for *imported* content only.)

---

## D. Proposed sequencing into XyScript milestones

Group the gaps into themed tranches, ordered by leverage and dependency. These slot
*after* the current live-shell milestones (M1–M6 done):

1. **Editing core I — motion + selection + deletion + case** (B2, B3, B4, B5):
   the segmentation engine (UAX-29) + viewport-aware motion unlock the largest set
   of daily verbs at once. Includes the **open/live "defined text" selection model**
   (`DF`/`DZ`/`DM` — adds an `open` flag to the buffer's selection state) and the
   **deletion buffer** (`RD`/`UD`). Wire the selection/cursor special-vars
   ($CX/$CY/$DS/$DN…). **B23 (CMline focus + dual-cursor memory) folds in here** since
   it extends the selection/cursor + M5.1 CMline work.
2. **Search & Replace** (B1): de-stub `EditorOp::Search`; SE/CH/CV families, modifiers,
   self-delimiting args, `$CC`. (Could lead, given it's a stub — decide with user.)
3. **Clipboard + counts + insert/overstrike** (B7, B8, B6): system clipboard,
   APPEND/PREPEND, CHARCOUNT/WORDCOUNT, overstrike mode.
4. **File I/O + insertion + sort + date** (B9, B11, B12): WR/WRL, RF/WF, MERGE, SORT,
   DA/stopwatch. Mostly engine-level, well-tested headlessly.
5. **View + UI-sizing + dialogs** (B15, B16, B21): line numbers, visible returns,
   menu/pane visibility verbs, font/scale/leading, TXT/INPUT.
6. **File lifecycle + filesystem** (B18, B10): save variants, autosave, read-only,
   config snapshots, the DIR browser + express-keys (the "directory screen").
7. **Power + scripting polish** (B14, B13, B22, C): shell-out (gated), WAIT/READKEY,
   RUNHERE, CMline chaining/tab-completion, press-a-key binding UI.
8. **Adapt/defer** (B17 persistent selection, B19 lock keys): low-leverage; fold in
   opportunistically.

Each tranche stays headless-testable in `xyscript-app` first, then surfaces in
`xyscript-gui`. Verbs land in `xyscript-xpl` (vm dispatch) + `EditorOp`s in the host;
the special-vars get wired to real state as their backing features arrive.

## E. Open decisions to surface (later, one at a time)

- **Lead with Search/Replace (B1) or Editing-core (tranche 1)?** Search is a stub
  (a visible hole); editing-core is higher total leverage.
- **Shell-out & filesystem-write gating** (B14, B10): default-on for local scripts vs
  confirm-prompt vs config flag. Tie to the Xyperlink trust model.
- **Notes/file pane model:** QN's single notes-folder sidebar vs XyScript's broader
  "directory screen" (and tabs-vs-windows, GUI6). The `DIR` browser may *be* this.
- **Overstrike** + **persistent selection**: faithful XyWrite modes — confirm they're
  wanted in an authoring-first tool (likely yes; XyWrite users expect them).
- **Verb naming:** keep QN's two-letter names verbatim (muscle memory) vs lightly
  modernize. Default: **keep verbatim** (it's the feature floor / muscle memory), add
  longer aliases (QN already pairs `CA`=`EDIT`, etc.).

> **B1 syntax update (2026-06-10):** flags now come FIRST (XyWrite-faithful). Accepted:
> attached `SE/T "x"`, spaced `SE /T "x"`, dash `SE -t "x"`; trailing still tolerated.
> Pattern self-delimiting after the flags. `\n` matches CRLF or LF.

> **Uniform switch convention (2026-06-10):** all switch-taking commands share one
> relaxed syntax — `/NAME` or `-name`, attached to the verb (`COPY/NV`) or leading
> before positional args (`COPY /NV a b`); never after args (paths stay safe).
> Search groups single letters (`/WA`=W+A); word-switches stay whole (`/NV`).
> Shared impl: `xyscript-xpl/src/switches.rs` (peel_verb + take_leading); dispatch
> peels the verb and `validate_switches` rejects undeclared switches. Future verbs
> (X/NV, COPY/NV, SORT, READKEY/RK, CD/D) plug into it.

> **B1 `/R` regex BUILT (2026-06-10):** `SE/R`, `CH/R` use the `regex` crate. Flags
> combine (`/RA`, `/RW`); `\d`/`\s`/etc. survive to the engine (regex-aware delimiter
> scan); case-insensitive default + `/A`; `/W` wraps in `\b…\b`; bad regex trips
> `$ERROR`. Replacement is literal (no `$1` backrefs — collides with `$`-subst). Only
> `CV/CVA` interactive change-verify remains for full B1.

> **B1 COMPLETE (2026-06-10):** `CV`/`CVA` interactive change-verify built. Five
> choices per stop: Yes (replace+next), No (skip+next), One more (replace+stop),
> Quit (stop, keep caret), Esc (stop, caret→start, keep prior edits). Engine holds a
> verify session; the GUI prompt owns the keyboard (Y/N/O/Q/Esc) and highlights the
> current match. **Search & Replace is now fully done** — literal, flags-first
> switches, `/R` regex, `$1` capture backrefs, and interactive verify.

> **B2 partial BUILT (2026-06-10):** word (NW/PW), paragraph (NP/PP), line-start
> (NL/PL), char/line synonyms (NC/PC/LU/LD), and JMP n. All accept trailing SEL.
> Ctrl+Left/Right = word, Ctrl+Up/Down = paragraph (sample .kbd CTRL layer). Word is
> a local rope scan (alnum+_, no segmentation dep). DEFERRED: sentence NS/PS;
> viewport motions PD/PU, HM/BS, MU/MD, CL/CR/LL/LR (need GUI viewport → host);
> Ctrl+Shift+arrow word-select (needs a CTRL+SHIFT kbd table).

> **B4 COMPLETE (2026-06-10):** BD (backspace), RC (rub-out forward), RL (delete to
> end of line; at EOL eats the break), RS (delete to end of sentence). QN deletion
> family = DN/BD/RC/RD/RL/RS, all built. Word/para delete = QN's DW/DL/DS/DP
> define-unit verbs (B3 selection family) + a delete — not yet built.

> **B4 deletion CORRECTED (2026-06-10):** whole-unit deletes — RW (word), RL (line,
> incl. break), RS (sentence), RP (paragraph), each removing the unit the cursor is in
> plus its trailing break/space. RE = delete cursor→end of line. RC = forward char,
> BD = backspace. `RO` reserved for the read-only toggle (QN's `RW`), not yet built.

> **B3 define-unit BUILT (2026-06-10):** DW/DL/DS/DP select the word/line/sentence/
> paragraph at the cursor as a defined block (persists across motion; CP/MV/case/delete
> it). Mixed spans: word & sentence = content only; line includes its break; paragraph
> = its lines minus the separating blank line. Deferred: DWONLY + extend-on-repeat.

> **Clipboard BUILT (2026-06-10):** CUT/COPY/PASTE (Ctrl+X/C/V) + CLIP/CY/APPEND/
> PREPEND, $CB. Engine owns internal $CB (testable); GUI mirrors it to the OS
> clipboard via arboard (bidirectional, mirror-string dedup). COPY bare = copy
> selection; `COPY <src> <dst>` reserved for future file-copy.

> **B2 sentence + Ctrl+Shift BUILT (2026-06-10):** NS/PS sentence motion; Ctrl+Shift+
> arrow = word/paragraph SELECT (new CTRL+SHIFT kbd table). B2 remainder = viewport
> motions only (PD/PU, HM/BS, MU/MD, CL/CR/LL/LR — need GUI viewport → host).

> **B2 viewport motions BUILT (2026-06-10):** CL/CR (wrap char), LL/LR (no-wrap char),
> PD/PU (page, logical lines), HM/BS (screen top/bottom). BS reconciled to bottom-of-
> screen (backspace = BD only; Backspace key → BD). Viewport-hint on host
> (Editor::set_viewport, fed by GUI each frame). PageUp/PageDown keys bound. Only MU/MD
> (scroll, caret fixed) deferred → B2 essentially complete.
