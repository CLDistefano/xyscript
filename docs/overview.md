---
title: overview
tags:
created: 2026-06-17
---
# XyScript overview — commands & special variables

*The canonical command & variable reference. **Part 1** groups commands and their related special
variables by function; **Part 2** is the full alphabetical command list; **Part 3** is the full
alphabetical special-variable list. See [run-commands.md](run-commands.md) for the background-script
workflow guide.*

Conventions: synonyms are shown together (`NEW / NE`); switches as `/X`; special variables are
read-only `$NAME` values typed as *(string)* / *(number)* / *(flag)*. A trailing `SEL` on a motion
verb extends the selection.

---

# Part 1 — By function

## Cursor & motion
**Commands:** `LEFT/PC/CL` `RIGHT/NC/CR` `UP/LU` `DOWN/LD` (char/line) · `HOME/EL` `END/ER` (logical
line) · `LB` `LE` (visual line) · `LL` `LR` (same line) · `NW` `PW` (word) · `NS` `PS` (sentence) ·
`NP` `PP` (paragraph) · `NL` `PL` (line start) · `TOP/TF` `BOTTOM/BF` (file) · `HM` `BS` (screen) ·
`PU` `PD` (page) · `JMP` (to offset) · `MU` `MD` (scroll, caret fixed)
**Variables:** `$CP` caret offset · `$CX`/`$CY` visual column/row · `$LN`/`$LC` logical line/column ·
`$LI` line under caret · `$WD` word under caret · `$CH` char right of caret · `$FB`/`$FE` at buffer
start/end.

## Selection & define
**Commands:** `DF` toggle · `DM` extend to caret · `DZ` close · `DFA` whole document · `XD/YD` cancel ·
`DW/DW+` word · `DL` line · `DS/DS+` sentence · `DP/DP+` paragraph · `SPS/CPS/TPS` persistent selection
on/off/toggle.
**Variables:** `$SE` selected text · `$SL` selected length · `$DF` selection-exists flag · `$DS`/`$DN`
selection start/end · `$CS` caret-on-selection flag · `$PS` persistent-mode flag.

## Delete & remove
**Commands:** `BD` backspace · `RC` delete char · `DELSEL/DN` delete selection · `RW`/`RL`/`RS`/`RP`
word/line/sentence/paragraph · `RE` to end of line · `RD` delete into the deletion buffer · `UD` paste
it back · `DEL` delete a file.
**Variables:** `$XB` deletion-buffer contents.

## Insert, typing & case
**Commands:** `INSERT` literal text · `MERGE/ME` insert a file at the caret · `ENTER` newline ·
`LC`/`UC`/`CC` lower/upper/swap case · `TB` set tab width.
**Variables:** `$TB` tab width · `$NEWLINE` line ending (`LF`/`CRLF`).

## Clipboard & in-document move
**Commands:** `CUT` `COPY` `PASTE` · `CLIP` set from a string · `APPEND`/`PREPEND` extend with the
selection · `CP`/`MV` copy/move the selection to the caret.
**Variables:** `$CB` clipboard contents.

## Search & replace
**Commands:** `SE`/`SEB`/`SEA`/`SEBA` search · `CH`/`CHA` (= `CI`/`CIA`) replace all · `CV`/`CVA` change-with-verify.
*Flags:* `/W` whole word · `/T` from top · `/A` case-sensitive · `/B` backward · `/R` regex · `/S` limit to the selection · `/X` extend the selection to the match.
**Variables:** `$CC` replacement count from the last `CH`.
**`/X` (extend to the match):** instead of replacing the selection with just the found text, grow the current selection — keep its existing anchor (typically a `DF` define) and move only the active edge to the match. The define stays open (auto-tracking), so a follow-up motion adjusts the edge. This makes the common "act from the cursor to the next *X*" gesture possible: `DF` to drop an anchor, `SE/X /./` to extend through the next period, optionally `CL` to back the edge off the period, then `DZ` and an edit. Find-only (`SE` family); rejected on a replace (`CH`) and mutually exclusive with `/S`.
**`/S` (within the selection):** the search/replace acts only inside the current selection, which stays put as a scope (a `SE/S` match moves the caret but keeps the selection, so repeated `SE/S` walks through matches within it). Start point: caret **inside** the selection → from the caret (toward the selection's end; `SEB/S` toward its top); caret **outside** (before/after) or on the near edge → from the top of the selection (`SEB/S`: from the bottom). The walk terminates cleanly — once a `SE/S` walk reaches the far edge it stops rather than restarting (it does not wrap), while a freshly placed caret on that edge still searches the whole selection. `CH/S` replaces every match in the selection and re-selects the result. No selection → `$ERROR`. (`/S` overrides `/T`; not available with `CV`.)
Search **does not wrap**: `SE` stops at the last match, `SEB` at the first. When nothing more is found
in that direction the search sets `$ERROR` (a trappable error) — so a loop like
`WHILE 1 ; SE /x/ ; IF $ERROR ; BREAK ; EI ; … ; ENDWHILE` terminates. Search is **case-insensitive**
unless you use `SEA`/`/A`; bear that in mind in a search-and-uppercase loop (a case-insensitive
`SE /foo/` keeps matching the text you just changed to `FOO`).

## File, buffers & tabs
**Commands:** `NEW/NE` · `OPEN/CA/ED/EDIT` *(/NV)* · `READ` (open read-only) *(/NV)* · `BEDIT` blind edit · `SAVE/SA` · `SAAS/SAVEAS` ·
`SAD` save selection · `REC/RECALL` revert · `AB` close · `ABD` delete file + close (`/V` confirm) · `SL` save all ·
`SF` save & close all · `QUIT/QQ` · `BUFNEXT`/`BUFPREV`/`BUFGO`/`BUFLIST` · `CD` · `DIR/LS` (`/W` = separate window) ·
`RENAME/REN` · `NEWWIN` · `ENCODING` · `TOPDF/PRINT` · `WORDCOUNT`/`CHARCOUNT` · `SORT` · `UNDO`/`REDO`.
**Variables:** `$FI` file name · `$FP` full path · `$EN` encoding · `$MO` modified flag · `$RO`/`$RE`
read-only flag · `$FS` size (bytes) · `$TL` line count · `$WC` word count · `$TC` tab count · `$CT`
active tab · `$NW` new-window flag · `$CWD`/`$PA` working dir · `$DC` path separator.

## Run & scripts
**Commands:** `RUN` script file · `RUNCODE` XPL from a variable (`/BG`, `/R:<var>`) · `RUNHERE` the code
block under the caret · `CALL` a routine · `LOADLIB`/`ADDLIB`/`RELOADLIB`/`UNLOADLIB`/`LISTLIB` the
routine library.
**Variables:** `$ED` executable path (used by `RUN/BG`) · `$XSDIR` its directory · `$VE` build version.
*(Argument/return variables under **Flow control**.)*
**Stopping a runaway script:** scripts run in cooperative time-slices, so even an endless loop with no
`WAIT` keeps the editor responsive (the status line shows *Running script… (Ctrl+Break to abort)*).
Press **Ctrl+Break** to abort the running script in an orderly way. This stops in-process scripts
(including a `RUN`-ed file); a background `RUN/BG` child is stopped with `KILL` instead.

## Shell & background jobs
**Commands:** `X` shell command (`/NV`, `/BG`, `/R:<var>`) · `XY` insert its output · `XV` capture into
a variable · `JOBS` list · `KILL` stop · `WAIT` (`/J <id>`, all jobs, or `<ms>`) · `TRUST` enable
shell-out.
**Variables:** `$TR` trusted flag · `$RC` last exit code · `$XO` last output · `$JOBS` running count ·
`$LJ` last launched job id.

## Input & interaction
**Commands:** `READKEY/RK` one keystroke → variable · `INPUT` a typed line → variable · `TXT` scrollable
dialog · `MSG` message box · `PR` print to output/status.
**Variables:** `$SC` last key scancode.

## Command line & focus
**Commands:** `CMLINE/GH` focus it · `SUBMIT` run it · `XC` run but keep focus/text · `CANCEL` / `BC`
clear · `GT` focus document · `TH` toggle focus · `HISTORY/HI` picker · `SAVECM`/`LOADCM` save/load the
history.
**Variables:** `$CL` last issued command · `$CM` current command-line text · `$CMH` the whole history
(newest first, OS newlines) · `$TX` focus is text (1) vs command line (0).

## Date, time & stopwatch
**Commands:** `DA` insert date+time · `ET` insert elapsed time · `ZT` reset the stopwatch.
**Variables:** `$DA`/`$TM` date/time · `$ET`/`$ETS` stopwatch (formatted / whole seconds) ·
`$YEAR $MON $MDAY $HOUR $MIN $SEC $MSEC $WDAY $YDAY` component fields.

## View & appearance
**Commands:** `SMB/CMB/TMB` menu bar · `SVR/CVR/TVR` paragraph marks · `SWS/CWS/TWS` whitespace ·
`SLN/CLN/TLN` line gutter · `SGC/CGC/TGC` pop-up command line · `SW`/`SHELLWIN [on|off]` shell-output tab · `SH`/`SHELL [on|off]` interactive shell pane ·  `HIGHLIGHT` syntax colors ·
`DWFGCOLOR`/`DWBGCOLOR`/`DWFONT`/`DWSCALE`/`DWFAMILY`/`DWSTYLE`/`DWRESET` (+ `DWFGCOLORS`/`DWBGCOLORS`/`DWFONTS` pickers) — `DIR/W` window appearance (inherits the editor until set) ·
`HLCOLOR`/`HLRESET`/`HLKINDS` highlight theme (recolor a kind / reset / list kinds) · `THEME light|dark` whole-editor colour scheme ·
`HLLANG`/`HLRULE` user regex grammars (per-language `pattern → kind` rules; bundled: `grammars/{xpl,rust,python,javascript,toml,shell,typst}.xpl` — incl. XPL and Typst) ·
`UISCALE`/`UIFONT`/`EDFONT`/`TXTFONT`/`TXTSCALE` sizes · `LEADING/LSP` spacing · `OF`/`OFL`/`OFR`
margins (all of these take a bare number to set, or a signed `+n`/`-n` to adjust relative to the
current value, e.g. `EDFONT +2`) · `UIRESET` · `EDFAMILY`/`UIFAMILY`/`TXTFAMILY` families · `EDSTYLE`/`UISTYLE`/`TXTSTYLE` styles ·
`EDFONTS`/`UIFONTS`/`TXTFONTS` pickers · `FGCOLOR`/`BGCOLOR`/`UIFGCOLOR`/`UIBGCOLOR`/`TXTFGCOLOR`/`TXTBGCOLOR`/`CRCOLOR`/`SEPCOLOR`/`SELBGCOLOR`/`SELFGCOLOR`/`TABBGCOLOR`
colors · the plural `…COLORS` + `HLCOLORS <kind>` + `TABBGCOLORS <state>` open a GUI colour-picker · `CRSHAPE` caret shape.
**Variables:** `$MB`/`$VR`/`$WS`/`$LNS`/`$SW`/`$SH` view flags · `$UISCALE`/`$UIFONT`/`$EDFONT`/`$TXTFONT`/`$TXTSCALE`
sizes · `$LSP` spacing · `$OFL`/`$OFR`/`$OF` margins · `$EDFAMILY`/`$UIFAMILY`/`$TXTFAMILY` families ·
`$EDSTYLE`/`$UISTYLE`/`$TXTSTYLE` styles · `$FGCOLOR`/`$BGCOLOR`/`$UIFGCOLOR`/`$UIBGCOLOR`/`$TXTFGCOLOR`/`$TXTBGCOLOR`/`$CRCOLOR`/`$SEPCOLOR`/`$SELBGCOLOR`/`$SELFGCOLOR`
colors · `$CRSHAPE` caret shape · `$OS` OS name.

## Config & persistence
**Commands:** `SAVECFG`/`LOADCFG` write/run a settings script · `AUTOCFG` live config tracking ·
`AUTOSAVE` interval · `RO` read-only lock.
**Variables:** `$CF` last loaded config · `$CFA` AUTOCFG tracked path · `$AU` autosave interval · `$RO`
read-only flag.

## Keyboard & binders
**Commands:** `LOADKBD`/`CLEARKBD`/`SAVEKBD` keymap · `LDTX` text binder (`@slot`) · `LDSC`/`LDPM`
script binder (`&slot`) · `SCL/CCL/TCL` Caps · `SNL/CNL/TNL` Num · `SSL/CSL/TSL` Scroll lock.
**Variables:** `$SC` last scancode · `$KB` last keymap file · `$CAPSLOCK`/`$NUMLOCK`/`$SCROLLLOCK` lock
states.

## Flow control & language
**Commands:** `SET`/`LOCAL`/`CLEAR` variables · `IF`/`IFNOT`/`ELSEIF`/`ELSE`/`ENDIF`/`EI` ·
`WHILE`/`ENDWHILE`/`EW` · `FOR`/`ENDFOR`/`EF` · `BREAK`/`CONTINUE` · `SUB`/`ENDSUB`/`ES`/`RETURN` · `GOTO` · `EXIT`/`EX`/`EXITALL`/`EX1`
· `NOP`/`DO`/`DX` · `QUIET` · `SETERR`/`CLERR` · `SV` save the selection into a variable.
**Variables:** `$ERROR` trappable-error flag · `$#` arg count · `$*` all args · `$1, $2, …` positional
args · `$RV` last return value.

## Expressions & operators
Used in `IF` / `IFNOT` / `ELSEIF` and `WHILE` **conditions**, and in `SET <var> = <expr>`. The target
may be written with or without a leading `$` (`SET x = …` ≡ `SET $x = …`), and supports compound
assignment **`+=` `-=` `*=` `/=` `^=`** (`SET x += 1` is `SET x = $x + 1`). Operators,
**lowest to highest** precedence:

| Operators | Meaning |
|-----------|---------|
| `OR` | logical or |
| `AND` | logical and |
| `NOT <x>` | logical not (prefix) — `IFNOT <cond>` is exactly `IF NOT (<cond>)` |
| `==` `!=` `<` `>` `<=` `>=` | comparisons |
| `+` `-` `.` | add, subtract, and **`.` = string concatenation** |
| `*` `/` `%` | multiply, divide, modulo (and unary `-` to negate) |
| `^` | exponentiation — right-associative, binds tighter than `* / %` and unary `-`; same as `POW(base, exp)` (`2^10` → `1024`, `-2^2` → `-4`, `2^-1` → `0.5`) |

- **Variables vs. bare words:** read a variable with **`$name`**. A bare word (no `$`) is a **string
  literal** — so `WHILE num <= 10` compares the *string* `"num"` (the loop won't run); write
  `WHILE $num <= 10`. Likewise quoted strings are literal: `SET s = "$x"` stores `$x` verbatim — use
  `SET s = $x` or concatenation `SET s = $x . " more"`.
- **Grouping:** `( … )`. **Keywords are case-insensitive** (`and` = `AND`). `TRUE`/`FALSE` = `1`/`0`.
- **Truthiness** (what a condition treats as true): a non-zero number, or a non-empty string that
  isn't numeric zero — so `""` and `"0"` are **false**; `"abc"` and `"5"` are **true**.
- **Examples:** `IF $CS AND NOT $RO …` · `IFNOT $TX …` · `IF $LN >= 10 OR $err …` ·
  `WHILE $i < 5 …` · `SET msg = "line " . $LN`.

### Functions
Called as `NAME(arg, …)` (the `(` must be adjacent to the name) anywhere an expression is allowed:

| Function | Returns |
|----------|---------|
| `CHR(n)` | the character for code `n` (0–255) — e.g. `CHR(65)` → `A` |
| `ASC(s)` | the code of the first character of `s` — e.g. `ASC("A")` → `65` |
| `LEN(s)` | character count (or element/entry count of an array/map) |
| `UPPER(s)` / `LOWER(s)` | case conversion |
| `STRIP/LSTRIP/RSTRIP(s)` | trim surrounding / leading / trailing whitespace |
| `SUBSTR(s, start, len)` | substring (0-based) · `TRIMLEFT/TRIMRIGHT(s, n)` drop `n` chars |
| `LEFT(s, n)` / `RIGHT(s, n)` | the leftmost / rightmost `n` characters (whole string if `n` ≥ length) |
| `INDEX(s, sub)` | 0-based position of `sub` in `s`, or `-1` · `CONTAINS(s, sub)` → `1`/`0` |
| `REPLACE(s, from, to)` | replace all occurrences · `SPLIT/SPLITS/JOIN` array helpers |
| `CHARS(s)` `WORDS(s)` `LINES(s)` | split a string into a plain 0-based array of its characters / whitespace-separated words / lines (trailing newline dropped) — `IN`-friendly like any array; count via `LEN(…)`. E.g. `WORDS("a b c")` → `[a, b, c]`, `FOR w IN WORDS($s) … EF` |
| `[a, b, c]` | **array literal** (empty `[]`, nested, trailing comma ok); elements are expressions. Index with `[i]`: `[10,20,30][1]` → `20` |
| `{k: v, …}` | **map literal** (empty `{}`, nested, trailing comma ok); keys are strings (bare word `a`, quoted `"a"`, or a parenthesised expression `($k)`), values are expressions. Index with `["k"]`: `{x: 1}["x"]` → `1` |
| `PUSH(arr, v)` / `POP(arr)` | array with `v` appended / with the last element dropped (pure — `SET a = PUSH($a, v)`) |
| `SLICE(arr, start [, end])` | sub-array, `end` exclusive (default = length); negative indices count from the end, both clamped (`SLICE($a, 0, -1)` = all but last) |
| `SORT(arr [, reverse])` | sorted copy (same ordering as `<`; stable); a truthy 2nd arg sorts descending |
| `ITER(x)` | the sequence `FOR … IN` walks: an array's elements, a map's keys (sorted), a string's chars, or `[x]` for a scalar (e.g. `JOIN(ITER($map), ",")` lists a map's keys) |
| `ITERP(x)` | like `ITER` but `[key, value]` pairs (drives the tuple `FOR k, v IN …` form): array → `[index, element]`, map → `[key, value]`, string → `[index, char]` |
| `VAR(name)` | value of the variable named by a string · `SEL()` the current selection |
| `KEY(scan)` | the current binding of key code `scan` on the unshifted layer of the operative keymap, as a string (`"KF"` for a Keyboard-Flip cell); `""` if unbound / out of range / no keymap. E.g. `KEY($SC)` = what the last key does |
| `ABS(n)` `INT(n)` | absolute value (keeps int/float) · truncate toward zero to an integer |
| `SQRT(n)` `SQ(n)` `CUBE(n)` `FAC(n)` | square root · square · cube · factorial (exact to 20!) |
| `POW(base, exp)` | `base` raised to `exp` (int/float; exact integer when both int and `exp` ≥ 0); the named twin of `^` |
| `EXP(n)` `LN(n)` `LOG(n)` | e^n (natural exponential) · natural log · base-10 log |
| `SIN/COS/TAN(n)` + `ASIN/ACOS/ATAN` | trig in **radians**, and inverses · `ATAN2(y, x)` quadrant-correct angle |
| `PI()` `E()` | mathematical constants (called with empty parens) |
| `BASE(n, in, out)` | convert integer string `n` from base `in` to base `out` (2–36; e.g. `BASE("FF", 16, 10)` → `255`) |
| `SINH/COSH/TANH(n)` + `ASINH/ACOSH/ATANH` | hyperbolic, and inverses |

Floats compute at full precision; how many decimal places are *displayed* is controlled by the
**`DECIMALS`** command (see below). Only display rounds — stored values stay exact.

### Inline expression substitution — `$(…)`
Outside an expression context (in any command argument), wrap an expression in **`$(…)`** to evaluate
it inline. This is how you use functions in commands like `PR`/`PUT`/`XY`:
`PR $(CHR(65))` → `A` · `PUT $(UPPER($name))` · `PR $(ASC($CH))` · `PR $($a + $b)`. Nested `$vars` and
`$(…)` resolve inside. A malformed/unknown call yields empty (like an unknown `$var`). To pass a
*literal* `$(` on to an external shell (bash/PowerShell command substitution), escape it: `XY echo
\$(date)`.

---

# Part 2 — All commands (alphabetical)

### A
- **AB** — close the active buffer, discarding changes *(/V: ask to save first)*
- **ABD** — delete the active document's file from disk and close it *(/V: confirm first)*. A never-saved buffer just closes (the absence is noted on the status line)
- **ADDLIB** — merge a `.lib` library into the loaded user library
- **APPEND** — append the selection to the clipboard
- **AR** — auto-replace: if the word immediately behind the caret is a loaded key, replace it with its value (case-sensitive, exact; one undo unit). No match = silent no-op. Typically bound to a trigger, e.g. `57=AR;" "` (Shift+Space = fix-the-word-then-space). See **LoadAR**
- **AUTOCFG** — start/stop live self-updating config tracking *(arg `off` stops)*
- **AUTOSAVE** — set the autosave interval in seconds *(`off`/`0` disables)*

### B
- **BC** — blank (clear) the command line
- **BD** — delete the character before the caret (backspace)
- **BEDIT** — blind-edit a file in an invisible buffer (for scripting)
- **BGCOLOR** — set the editor background colour `R G B` *(/HSL)*
- **BGCOLORS** — open the colour-picker dialog for the editor background
- **BK** — inline breakpoint (debugging): `BK off` (no-op, a disabled breakpoint) · `BK on` (pause with a stop prompt — `Y` stops the whole script, any other key/Continue resumes) · `BK debug` (pause and show the live variables, with Continue/Stop). Bare `BK` = `on`. The dialog header shows the breakpoint's `line:col` in the script. A scriptable companion to Ctrl+Break; inert in headless `--run` (auto-continues)
- **BOTTOM / BF** — move to the bottom of the document
- **BREAK** — exit the enclosing `WHILE` loop *(language keyword)*
- **BS** — move to the bottom of the screen
- **BUFGO** — switch to a buffer by name or index
- **BUFLIST** — list the open buffers
- **BUFNEXT** — switch to the next buffer
- **BUFPREV** — switch to the previous buffer

### C
- **CALL** — call a sub/library routine with arguments
- **CANCEL** — cancel/close the command line
- **CAPTURE** — redirect command output into a variable instead of displaying it: `CAPTURE <var> <command>` (one command) or `CAPTURE <var>` … `ENDCAPTURE` (a block). Captures `PR`/`MSG`/`TXT`/`HLKINDS`/`LISTSPECIALS` text, `DIR`/`LS` listings, and the `SAVECM` snapshot script (multiple outputs joined with newlines); `TXT`/`MSG` don't pop their modal, `DIR` doesn't open a listing, and `SAVECM`/`LISTSPECIALS` don't write/insert. Status/error feedback isn't captured (it stays on `$ERROR`). *(language keyword)*
- **CC** — swap the case of the selection (or char under the caret)
- **CD** — change the working directory or drive *(/D)*
- **CH / CHA** — replace all (A = case-sensitive) *(/W /T /A /R /S)*. **CI / CIA** are XyWrite synonyms
- **CHARCOUNT** — count characters in the selection or document
- **CLEAR** — unset variables *(language keyword)*
- **CLEARKBD** — reset the keymap to the built-in baseline
- **CLERR** — clear the `$ERROR` flag
- **CLIP** — set the clipboard contents from a string argument
- **CMH** *(optional `"command"`)* — add a command to the top of the history (deduped) without running it. Bare `CMH` stashes the current command line (for a KBD binding that saves what you're typing); **`CMH "command"`** stashes that quoted string instead, with `$vars` expanded at stash time (like `INSERT "…"`). (Distinct from the `$CMH` read special.)
- **CMLINE / GH** — focus the command line
- **CONTINUE** — jump to the next `WHILE` iteration *(language keyword)*
- **COPY** — copy the selection to the clipboard; `COPY <src> <dst>` copies a file
- **CP** — copy the selection to the caret (within the document)
- **CPS** — turn persistent selection off
- **CRCOLOR** — set the caret colour `R G B` *(/HSL)*
- **CRCOLORS** — open the colour-picker dialog for the caret colour
- **CRSHAPE** — set the caret shape (`bar` / `block` / `underline`)
- **CUT** — cut the selection to the clipboard
- **CV / CVA** — interactive change-with-verify (A = case-sensitive) *(/W /T /A /R)*

### D
- **DA** — insert the current date and time
- **DB** — move the cursor to the start of the selection, collapsing it (sets `$ERROR` if nothing is selected)
- **DE** — move the cursor to the end of the selection, collapsing it (sets `$ERROR` if nothing is selected)
- **DECIMALS** — fixed decimal places when *displaying* floats (`DECIMALS <n>`; `DECIMALS off` = default precision). Read back via `$DECIMALS`; persisted by `SAVECFG`. Display-only — stored values keep full precision
- **DOCTYPE** — how the active document renders to PDF (`TOPDF`): `DOCTYPE typst` or `DOCTYPE gfm` forces a kind; `DOCTYPE auto` (default) decides by extension (`.typ` → Typst, else GFM); bare `DOCTYPE` reports the effective kind. Authored Typst is compiled verbatim (the document's own `#set` rules control the page); GFM is translated. Read back via `$DOCTYPE` (`typst`/`gfm`). A starter document with a small `#let` component library ships at `templates/report.typ`
- **DEL** — delete a file (or empty directory) from disk, with a confirmation prompt; requires a path. **`DEL/NV`** deletes without confirming (synonym **`ERNV`** — "erase, no verify"). In a headless/script run with no GUI to answer the prompt, use `DEL/NV`/`ERNV`
- **DELSEL / DN** — delete the current selection
- **DF** — toggle defined-text selection on/off
- **DFA** — define (select) the whole document
- **DIR / LS** — open a directory-listing browser. **`/W`** opens it in a separate movable/resizable window instead of a tab (a single reusable window; **title shows `$DP`**; inherits the editor's colours/font, overridable with the `DW…` verbs below): arrows/PageUp-Down/Home/End move the highlight, type-to-find + F3, Enter or double-click opens a file in a **new tab in the main window** (a subdirectory re-lists in place), Esc closes. The listing **auto-refreshes when the window regains focus** if the directory changed on disk. While the window is open and the active tab is *not* itself a listing, **`$DP`/`$DR` read the window's listing** *(switches: /B bare, /O… sort, /A… attrs, /S recurse, /W window)*
- **DWFGCOLOR / DWBGCOLOR** — `DIR/W` window text / background colour `R G B` *(/HSL)*; `DWFGCOLORS`/`DWBGCOLORS` open the colour-picker. Unset = inherit the editor's `FGCOLOR`/`BGCOLOR`
- **DWFAMILY / DWSTYLE / DWFONT / DWSCALE** — `DIR/W` window font family / style / size (pt) / zoom; `DWFONTS` opens the font picker. Unset family/size/style inherit `EDFAMILY`/`EDFONT`/`EDSTYLE`. **DWRESET** clears all `DW…` overrides back to inheriting the editor. Read back via `$DWFGCOLOR`/`$DWBGCOLOR`/`$DWFONT`/`$DWSCALE`/`$DWFAMILY`/`$DWSTYLE` (effective values); persisted by `SAVECFG`
- **DL** — define the line at the caret
- **DM** — extend the defined selection to the caret
- **DO / DX / NOP** — no-op
- **DOWN / LD** — move down one line
- **DP / DP+** — define the paragraph at the caret (`+` includes the trailing break)
- **DS / DS+** — define the sentence at the caret (`+` includes the trailing separator)
- **DW / DW+** — define the word at the caret (`+` includes the trailing separator)
- **DZ** — close the defined selection

### E
- **ED** — *see* **OPEN**
- **EDFAMILY** — set the editor font family by name
- **EDFONT** — set the editor font size (pt); `EDFONT +n`/`-n` adjusts relative to the current size
- **EDFONTS** — open the editor font-picker dialog
- **EDIT** — *see* **OPEN**
- **EDSTYLE** — set the editor font style (normal/bold/italic/bolditalic)
- **EL** — *see* **HOME**
- **ELSE / ELSEIF / EI / ENDIF** — conditional block parts *(language keywords)*
- **ENDCAPTURE / EC** — close a `CAPTURE` block *(language keyword)*
- **ENCODING** — set the file text encoding
- **END / ER** — move to the end of the logical line
- **ENDSUB / ES** — end a routine definition *(language keyword)*
- **ENDWHILE / EW** — end a `WHILE` loop *(language keyword)*
- **ERNV** — erase (delete) a file from disk with no confirmation — synonym for `DEL/NV` (see **DEL**)
- **ENTER** — insert a newline in the document's line-ending style
- **ET** — insert the elapsed stopwatch time
- **EXIT / EX / EXITALL / EX1** — halt the script

### F
- **FOR … ENDFOR / EF** — a counting or collection loop *(language keyword)*. Two forms: a range `FOR i = a TO b [STEP s]` (inclusive `TO`; default step `+1`; counts down with a negative step) and a collection `FOR v IN <expr>` (an array's elements, a map's keys, or a string's characters). A **tuple** collection form `FOR k, v IN <expr>` binds two variables: the first is the index/key, the second is the element/value/char (array → index+element, map → key+value, string → index+char). **`FOR/X`** makes the upper bound *exclusive* — handy for arrays: `FOR/X i = 0 TO LEN($a)` walks every index. The loop variable is set like `SET` (read it as `$i`); after a range loop it's left one past the last value. `BREAK`/`CONTINUE` work inside (`CONTINUE` still advances the counter). `STEP 0` is an error. Examples: `FOR/X i = 0 TO LEN($a) ; PR $($a[$i]) ; EF` · `FOR k, v IN {a: 1, b: 2} ; PR $($k . "=" . $v) ; EF`
- **FGCOLOR** — set the editor text colour `R G B` *(/HSL)*
- **FGCOLORS** — open the colour-picker dialog for the editor text colour

### G
- **GH** — *see* **CMLINE**
- **GOTO** — jump to a label *(language keyword)*
- **GT** — focus the document/text window

### H
- **HIGHLIGHT** — toggle syntax highlighting on/off (resolves the active file's extension against the loaded grammars, else Markdown)
- **HLAUTO** — `HLAUTO on|off`: auto-enable highlighting whenever a file whose type is recognized (a grammar's extension, or Markdown) is opened. Off by default (quiet screen); persisted by `SAVECFG`. Read `$HLAUTO`. Put `RUN grammars/rust.xpl` + `HLAUTO on` in `startup.xpl` so grammars load and apply automatically each session
- **HISTORY / HI** — open the command-history picker
- **HLCOLOR** — set a token kind's highlight colour `R G B` *(/HSL; `HLCOLOR <kind> default` → FGCOLOR)*. `<kind>` is any name — a built-in or one your grammar emits
- **HLCOLORS** — open the colour-picker dialog for a token kind (`HLCOLORS <kind>`)
- **HLKINDS** — list the highlight token kinds + current colours in a scrollable `TXT` dialog
- **HLLANG** — declare a regex-grammar highlighter: `HLLANG <name> <.ext1,.ext2>` (re-declaring resets it); selects it for following `HLRULE`s
- **HLRULE** — add a rule to the current grammar: `HLRULE <kind> <regex>` (regex matched per line; first matching rule wins, and a later match is dropped if it overlaps a claimed span — so list whole-line rules first). The regex is taken **verbatim** (not `$`-substituted), so anchors/`\$` work. Quote the regex if it contains spaces. See [regex.md](regex.md) for the supported regex syntax
- **HLRESET** — restore the built-in default highlight theme
- **THEME** — `THEME light|dark`: apply a built-in colour scheme across the whole editor (canvas, UI chrome, separators, caret, selection, TXT dialog, and the highlight palette) in one shot. A macro over the colour setters — the resulting colours are what `SAVECFG` persists, and later `*COLOR`/`HLCOLOR` tweaks survive. `THEME light` = the factory defaults
- **TABBGCOLOR** — `TABBGCOLOR <active|hover|inactive> <r g b>` *(/HSL)*: set the background colour of a GUI tab title for that interaction state (`TABBGCOLOR <state> default` clears it to egui's theme colour). Only takes effect when more than one tab is open. Read `$TABACTIVE`/`$TABHOVER`/`$TABINACTIVE`
- **TABBGCOLORS** — `TABBGCOLORS <state>`: open the colour-picker dialog for that tab-title state
- **HM** — move to the top of the screen
- **HOME / EL** — move to the start of the logical line

### I
- **IF / IFNOT** — conditional block *(language keyword)*
- **INPUT** — pause for a typed line into a variable (`INPUT <var> [prompt]`)
- **INSERT** — insert literal text at the caret (quoted strings unescape `\n` etc.)

### J
- **JMP** — jump to an absolute char offset, or relative with `+n`/`-n`
- **JOBS** — list running background jobs

### K
- **KILL** — terminate a background job by id

### L
- **LB** — move to the start of the visual (wrapped) line
- **LC** — lowercase the selection (or char under the caret)
- **LD** — *see* **DOWN**
- **LDPM / LDSC** — load a script binder slot (`&slot`)
- **LDTX** — load a text binder slot (`@slot`)
- **LE** — move to the end of the visual (wrapped) line
- **LEADING / LSP** — set line spacing (%)
- **LEFT / PC / CL** — move left one character
- **LISTLIB** — list the loaded library routines
- **LISTSPECIALS** — insert one `$NAME: |$NAME|` line per special variable (a PRINTEX-able template that stays in sync as specials are added). Under `CAPTURE` it feeds the variable instead. Pair with **PRINTEX** to see every special's live value
- **LL** — move left one char, staying on the same line
- **LoadAR** — load `key=replacement` pairs from a file into the auto-replace map (merges; lines starting with `#` or `;` are comments, blank lines skipped; `\n`/`\t`/`\\` escapes in the value decoded). Used by **AR**. `CLEARAR` empties the map; **SAVEAR** *[file]* writes it back (a commented sample when nothing's loaded). `$AR` = the last loaded file's path
- **LOADCFG** — run a saved config script
- **LOADCM** — load a saved command-history list
- **LOADKBD** — load a keymap file
- **LOADLIB** — load a `.lib` library, replacing the user library
- **LOCAL** — scope variables to the current routine *(language keyword)*
- **LR** — move right one char, staying on the same line
- **LS** — *see* **DIR**
- **LU** — *see* **UP**

### M
- **MD** — scroll the viewport down one visual line (caret unmoved)
- **MERGE / ME** — insert a file's contents at the caret
- **MSG** — show a GUI message box. A **bare `MSG`** is *modal*: it **pauses the script** until you dismiss it (OK / Esc), so each `MSG` in a loop shows in turn (contrast `PR`, which goes to the status line, and a non-blocking toast below). **`MSG /T:<seconds>`** is a non-blocking timed toast that self-dismisses and lets the script continue. In headless `--run`, a bare `MSG` auto-continues (its text goes to stdout). *(Use `TXT` for a scrollable modal dialog.)*
  - **Toasts are "last-wins" in a tight loop:** because a `/T:` toast doesn't pause, a loop of them all overwrite the one message slot before the screen repaints, so only the last shows. To flash each in turn, pause between them with a non-blocking `WAIT`: `FOR i = 1 TO 5 ; MSG /T:2 $i ; WAIT 1000 ; EF` (the `/T:` outlasts the gap so there's no flicker). For step-through, use bare `MSG`.
- **MU** — scroll the viewport up one visual line (caret unmoved)
- **MV** — move the selection to the caret (within the document)

### N
- **NEW / NE** — open a new empty buffer. **`NEW <file>`** starts a fresh buffer named for `<file>` (ready to `SAVE`); forgiving — if `<file>` already exists it is opened instead (switching to its tab if already open), with an *"&lt;file&gt; already exists"* status
- **NEWWIN** — set whether directory listings open in a new window (`0`/`1`)
- **NL** — move to the start of the next line
- **NOP / DO / DX** — no-op
- **NP** — move to the next paragraph
- **NS** — move to the next sentence
- **NW** — move to the next word

### O
- **OF** — set both left and right margins (cols); `+n`/`-n` adjusts both relative to current
- **OFL** — set the left margin (cols)
- **OFR** — set the right margin (cols)
- **OPEN / CA / ED / EDIT** — open a file (bare = GUI open dialog)

### P
- **PASTE** — paste the clipboard at the caret
- **PC** — *see* **LEFT**
- **PD** — page down
- **PL** — move to the start of the previous line
- **PP** — move to the previous paragraph
- **PR** — print to the script-output / status channel. If the text contains real newlines (e.g. `PR $CMH`, or `PR a$(CHR(10))b`), the status line stacks the lines and grows upward to fit (up to 10 rows; beyond that the overflow collapses into a "(+N more)" line — use `PRINTEX`/a buffer for the full text). Note `PR` doesn't decode `\n` escapes — `PR "a\nb"` shows literal `\n`
- **PREPEND** — prepend the selection to the clipboard
- **PRINT** — *see* **TOPDF**
- **PRINTEX** — "print + expand" `PRINTEX [<output_file>]`: copy the active document, expanding each `|…|` soft-field while copying the rest verbatim. `\|` is a literal pipe; an unpaired `|` is left as-is; the source document is unchanged. **With `<output_file>`**: writes it (OS-appropriate newlines — CRLF on Windows, LF elsewhere) and opens it in a new tab. **Without an argument**: drops the expanded text into a new, unnamed tab. A field is one of:
  - **Substitution** (default): `$vars`, `$(…)`, functions — e.g. `|$YEAR-$MON-$MDAY|` → the date. (Dashes stay literal: it's string substitution, *not* the expression `$YEAR - $MON`.) Needs no `TRUST`.
  - **Live code**: a field with a top-level `;` or a leading statement keyword (`set`/`if`/`while`/…) runs XPL statements. The **last `;`-segment** is the output — an expression inserts its value; a statement (e.g. `|set ctr=0|`) yields nothing and the field is deleted. Variables **persist across fields** within one run and are **template-local** (a snapshot of the editor's vars on entry; changes are discarded when PRINTEX finishes). Live-code documents require **`TRUST 1`** (substitution-only ones don't). Example: `|set ctr=0|` then, repeated, `|set ctr+=1;$ctr|` → 1, 2, 3, …
- **PS** — move to the previous sentence
- **PU** — page up
- **PUT** — like **PR**, but insert the (substituted) argument at the caret — into the CMline or the document, whichever has focus. Args are raw like `PR` (no quote/`\n` processing); `PUT $var` inserts the variable's value
- **PW** — move to the previous word

### Q
- **QUIET** — suppress status messages for the rest of the script *(/NV; arg `OFF`)*
- **QUIT / QQ** — quit, prompting to save dirty tabs *(/NV: quit without prompts)*

### R
- **RC** — delete the character after the caret (forward)
- **RD** — delete the selection into the deletion buffer
- **READ** — open an existing file in a **read-only** tab (locked against edits); toggle the lock off with **RO**. A missing file is an error (unlike **OPEN**, which starts a new buffer)
- *(both **OPEN**/synonyms and **READ**)* — if the file is already open in another tab, a status-line prompt offers: **(Y)es** open again · **(N)o** cancel · **(G)o** to the open tab · **(R)ead** open it read-only. Add **`/NV`** (`OPEN/NV`, `READ/NV`) to skip the prompt and open a fresh tab outright
- **READKEY / RK** — pause for one keystroke into a variable *(/NV)*
- **RE** — delete from the caret to the end of the line
- **REC / RECALL** — revert the document to its last saved state
- **REDO** — redo edits *(arg: count or `ALL`)*
- **RELOADLIB** — re-run the recorded library-load sequence
- **RENAME / REN** — rename or move a file
- **RETURN** — return from a routine, optionally with a value *(language keyword)*
- **RIGHT / NC / CR** — move right one character
- **RL** — delete the line at the caret
- **RO** — read-only lock (bare toggles; `on`/`off` set)
- **RP** — delete the paragraph at the caret
- **RS** — delete the sentence at the caret
- **RUN** — read and run a script file *(/BG; /R:`<var>`)*
- **RUNCODE** — evaluate XPL text directly (e.g. from a variable) *(/BG; /R:`<var>`)*
- **RUNHERE** — run the fenced code block under the caret
- **RW** — delete the word at the caret

### S
- **SAAS / SAVEAS** — save and adopt a new path (bare = native Save-As dialog)
- **SAD** — save the current selection to a new file
- **SAVE / SA** — save in place; `SAVE <file>` writes a copy *(/NV)*
- **SAVECFG** — write the current settings as an XPL config script (OS-appropriate newlines)
- **SAVECM** — write the command history to a runnable `LoadCM` script with OS-appropriate newlines (under `CAPTURE`, yields that script text to the variable instead of writing a file)
- **SAVEKBD** — dump the current keymap to a KBD file (OS-appropriate newlines)
- **SCL / CCL / TCL** — Caps Lock set / clear / toggle
- **SE / SEB / SEA / SEBA** — search (B = backward, A = case-sensitive) *(/W /T /A /B /R /S /X)*. `/S` limits the search to the selection (which persists as a scope). `/X` extends the current selection/`DF` define to the match instead of replacing it (find-only; not with `CH`/`/S`). With `/R` the pattern is a regex — see [regex.md](regex.md)
- **SELBGCOLOR** — set the selection background colour (the band behind selected text) `R G B` *(/HSL)*
- **SELBGCOLORS** — open the colour-picker dialog for the selection background
- **SELFGCOLOR** — set the selected-text colour (glyphs inside the selection) `R G B` *(/HSL)*
- **SELFGCOLORS** — open the colour-picker dialog for the selected text
- **SEPCOLOR** — set the separator colour `R G B` *(/HSL)*
- **SEPCOLORS** — open the colour-picker dialog for the separator colour
- **SET** — assign a variable *(language keyword)*; target may be `x` or `$x`; supports `+= -= *= /= ^=`
- **SETERR** — set the `$ERROR` flag
- **SF** — save and close all buffers
- **SGC / CGC / TGC** — pop-up command line on / off / toggle
- **SL** — save all named, modified buffers
- **SLN / CLN / TLN** — line-number gutter on / off / toggle
- **SMB / CMB / TMB** — menu bar on / off / toggle
- **SNL / CNL / TNL** — Num Lock set / clear / toggle
- **SORT** — reorder the selected lines *(/F:n /N:n)*
- **SPS / CPS / TPS** — persistent selection on / off / toggle
- **SSL / CSL / TSL** — Scroll Lock set / clear / toggle
- **SH / SHELL** *(no arg toggles; `on`/`off` set)* — show/hide the interactive shell pane at the bottom of the editor (a command line + scrollback, ≤10 lines, resizable). Each command runs via the job worker in `$CWD` (`cd` is intercepted so the working directory persists; needs `TRUST 1`). **Enter** runs the command with output in the pane; **Shift+Enter** pipes the output into the active document at the caret, inserted on completion as one undo unit. ↑/↓ recall history; **Esc** (focus in the pane) closes it. Read back via `$SH`; persisted by `SAVECFG`
- **PREVIEW / PV** *(no arg toggles; `on`/`off` set)* — show/hide the live PDF **preview pane** on the right: a debounced raster of the active document, re-rendered ~400 ms after you stop typing, using the document's `DOCTYPE` (Typst compiled verbatim, GFM translated). A compile error shows in the pane (`line:col: message`) over the last good render. Off by default; read back via `$PV`; persisted by `SAVECFG`
- **SUB** — begin a routine definition *(language keyword)*
- **SUBMIT** — submit (run) the command line
- **SWITCH … CASE … [DEFAULT] … ENDSWITCH / ESW** — multi-way branch *(language keyword)*. The `SWITCH <expr>` value is evaluated once; each `CASE <v1>[, <v2>…]` runs when the value equals any of its listed values, then the switch ends — **no fall-through**. An optional `DEFAULT` (must be last) runs when nothing matches; with no match and no `DEFAULT`, nothing runs. Values are expressions; the scrutinee and cases compare like `==`. Example: `SWITCH $day ; CASE 1, 7 ; PR weekend ; CASE 2,3,4,5,6 ; PR weekday ; DEFAULT ; PR "?" ; ESW`
- **SV** — save the current selection text into a variable
- **SVR / CVR / TVR** — visible paragraph marks on / off / toggle
- **SW / SHELLWIN** *(no arg toggles; `on`/`off` set)* — show/hide the *Shell Output* tab. `X/BG`/`RUN/BG`/`RUNCODE/BG` jobs follow this state: with **SW on**, launching a job switches to the *Shell Output* tab so you watch it stream live; with **SW off** the job still runs and is captured, but the tab stays hidden and focus stays on your document (`$XO`/`$RC`/`JOBS` are unaffected either way). The `SW` command itself only shows/hides the tab — with one exception: if the tab is re-shown after being hidden *while it had focus*, focus returns to it (hide-while-looking → show brings you back; hide from your document → show leaves you put). Read back via `$SW`. *(Background jobs need `TRUST 1` first.)*
- **SWS / CWS / TWS** — visible whitespace on / off / toggle

### T
- **TAB** — insert a tab character (renders at the `TB` width). Bind to the Tab key (`15=TAB`) — use this **command**, not a `"\t"` typing cell, or the GUI hands Tab to focus navigation and nothing is inserted (analogous to **ENTER** for the newline)
- **TB** — set the tab width (spaces)
- **TF** — *see* **TOP**
- **TH** — toggle focus between document and command line
- **TOP / TF** — move to the top of the document
- **TOPDF / PRINT** — export the document to PDF, rendered per its **`DOCTYPE`** (Typst compiled verbatim, GFM translated). `TOPDF <path>`, or bare `TOPDF` derives the name from the document (`.typ`/`.md` → `.pdf`). A compile error is reported on the status line as `line:col: message` (`$ERROR` set); for Typst-as-input the line/column map to your buffer
- **TRUST** — enable/disable shell-out (`0`/`1`)
- **TXT** — show a scrollable text dialog *(/T:"title")*
- **TXTBGCOLOR** — set the TXT-window background colour `R G B` *(/HSL)*
- **TXTBGCOLORS** — open the colour-picker dialog for the TXT-window background
- **TXTFAMILY** — set the TXT-window font family by name
- **TXTFGCOLOR** — set the TXT-window text colour `R G B` *(/HSL)*
- **TXTFGCOLORS** — open the colour-picker dialog for the TXT-window text colour
- **TXTFONT** — set the TXT-window font size (pt); `+n`/`-n` adjusts relative to the current size
- **TXTFONTS** — open the TXT-window font-picker dialog
- **TXTSCALE** — set the TXT-window zoom factor
- **TXTSTYLE** — set the TXT-window font style

### U
- **UC** — uppercase the selection (or char under the caret)
- **UD** — paste back the deletion buffer
- **UIBGCOLOR** — set the UI chrome background colour `R G B` *(/HSL)*
- **UIBGCOLORS** — open the colour-picker dialog for the UI chrome background
- **UIFAMILY** — set the UI chrome font family by name
- **UIFGCOLOR** — set the UI chrome text colour `R G B` *(/HSL)*
- **UIFGCOLORS** — open the colour-picker dialog for the UI chrome text colour
- **UIFONT** — set the UI chrome font size (pt); `+n`/`-n` adjusts relative to the current size
- **UIFONTS** — open the UI font-picker dialog
- **UIRESET** — reset the UI sizing knobs to defaults
- **UISCALE** — set the UI zoom factor; `+n`/`-n` adjusts relative to the current factor
- **UISTYLE** — set the UI font style
- **UNDO** — undo edits *(arg: count or `ALL`)*
- **UNLOADLIB** — drop all user-library routines
- **UP / LU** — move up one line

### W
- **WAIT** — pause until job(s) finish, or `WAIT <ms>` for a timed pause *(/J `<id>`)*
- **WHILE** — begin a loop *(language keyword)*
- **WORDCOUNT** — count words in the selection or document

### X
- **X** — run a shell command *(/NV; /BG; /R:`<var>`)*
- **XC** — run the command line's current contents (keeping focus + text)
- **XD / YD** — cancel the current selection
- **XV** — run a shell command, capturing stdout into a variable (`XV <var> <cmd>`)
- **XY** — run a shell command and insert its stdout at the caret

### Z
- **ZT** — reset (zero) the stopwatch

---

# Part 3 — All special variables (alphabetical)

- **$\*** — all positional arguments joined with spaces *(string)*
- **$#** — number of positional arguments passed *(number)*
- **$1, $2, …** — positional arguments by index, 1-based *(any)*
- **$AR** — fully-qualified path of the file most recently loaded by `LoadAR` (empty if none / after `CLEARAR`) *(string)*
- **$AU** — autosave interval in seconds; `0` = off *(number)*
- **$BGCOLOR** — editor background colour `R G B` *(string)*
- **$CAPSLOCK** — Caps Lock state *(flag)*
- **$CB** — clipboard contents *(string)*
- **$CC** — replacement count from the most recent `CH` replace-all *(number)*
- **$CF** — path of the last `LOADCFG`'d config file *(string)*
- **$CFA** — `AUTOCFG` tracked-config path; empty if off *(string)*
- **$CH** — character to the right of the caret (`\r\n` reported as `\n`) *(string)*
- **$CL** — last command issued from the command line *(string)*
- **$CM** — current command-line content *(string)*
- **$CMH** — the full command history as one string, newest first, joined with OS-appropriate newlines (CRLF on Windows, LF elsewhere) *(string)*
- **$CP** — caret character offset in the buffer *(number)*
- **$CRCOLOR** — caret colour `R G B` *(string)*
- **$CRSHAPE** — caret shape name (bar/block/underline) *(string)*
- **$CS** — `1` if the caret is on the current selection (inside or at either edge), else `0` *(flag)*
- **$CT** — active tab index, 1-based *(number)*
- **$CWD** — editor working directory *(string)*
- **$CX** — caret's visual column, 0-based, wrap-aware *(number)*
- **$CY** — caret's visual row relative to the viewport top *(number)*
- **$D1** — `1` if the active buffer is a directory listing (`DIR`/`LS`), else `0` *(flag)*
- **$DA** — current date `YYYY-MM-DD`, local *(string)*
- **$DC** — OS directory-separator character (`\` or `/`) *(string)*
- **$DF** — `1` if there is a selection *(flag)*
- **$DN** — selection end offset (caret if none) *(number)*
- **$DP** — the directory-listing's path, with the file mask appended if any (e.g. `DIR *.txt`); empty if not a listing *(string)*
- **$DR** — full path of the file/subdirectory on the caret's line in a listing; empty if not a listing *(string)*
- **$DS** — selection start offset (caret if none) *(number)*
- **$ED** — full path of the running XyScript executable *(string)*
- **$EDFAMILY** — editor font family name *(string)*
- **$EDFONT** — editor text font size *(number)*
- **$EDSTYLE** — editor font style keyword *(string)*
- **$EN** — active document's text encoding name *(string)*
- **$ERROR** — trappable-error flag *(flag)*
- **$ET** — stopwatch elapsed time, formatted *(string)*
- **$ETS** — stopwatch elapsed whole seconds *(number)*
- **$FB** — `1` if the caret is at the buffer start *(flag)*
- **$FE** — `1` if the caret is at the buffer end *(flag)*
- **$FGCOLOR** — editor text colour `R G B` *(string)*
- **$FI** — active document's file name (no directory) *(string)*
- **$FP** — active document's full file path *(string)*
- **$FS** — buffer size in bytes *(number)*
- **$HLAUTO** — auto-enable highlighting on open *(flag)*
- **$HOUR** — hour, 2 digits, 24-hour local *(string)*
- **$JOBS** — number of active background jobs *(number)*
- **$KB** — full path of the last `LOADKBD`'d keymap file *(string)*
- **$LC** — caret's logical column, 1-based *(number)*
- **$LI** — text of the line under the caret (no trailing newline) *(string)*
- **$LJ** — id of the last background job launched; `0` if none *(number)*
- **$LN** — caret's logical line, 1-based *(number)*
- **$LNS** — line-number gutter on *(flag)*
- **$LS** — last status-line message text *(string)*
- **$LSP** — line leading/spacing *(number)*
- **$MB** — menu bar visible *(flag)*
- **$MDAY** — day of month, 2 digits *(string)*
- **$MIN** — minute, 2 digits *(string)*
- **$MO** — `1` if the active document is modified *(flag)*
- **$MON** — month, 2 digits *(string)*
- **$MSEC** — millisecond, 3 digits *(string)*
- **$NEWLINE** — document's line ending, `LF` or `CRLF` *(string)*
- **$NUMLOCK** — Num Lock state *(flag)*
- **$NW** — new-window mode on *(flag)*
- **$OF** — left margin width (compat alias of `$OFL`) *(number)*
- **$OFL** — left margin width *(number)*
- **$OFR** — right margin width *(number)*
- **$OS** — operating-system name *(string)*
- **$PA** — synonym of `$CWD` *(string)*
- **$PS** — persistent-selection mode on *(flag)*
- **$RC** — exit code of the last shell command *(number)*
- **$RE** — active tab's read-only flag *(flag)*
- **$RO** — active tab's read-only flag *(flag)*
- **$DOCTYPE** — active document's effective PDF render kind, `typst` or `gfm` *(string)*
- **$PV** — live preview pane visibility *(flag)*
- **$RV** — last return value (`RETURN`) *(any)*
- **$SC** — last key scancode *(number)*
- **$SCROLLLOCK** — Scroll Lock state *(flag)*
- **$SE** — selected text *(string)*
- **$SEC** — second, 2 digits *(string)*
- **$SEL** — the most recent selection's text (retained after it clears; shared across tabs) *(string)*
- **$SELBGCOLOR** — selection background colour `R G B` *(string)*
- **$SELFGCOLOR** — selected-text colour `R G B` *(string)*
- **$SEPCOLOR** — separator colour `R G B` *(string)*
- **$SH** — interactive shell pane visibility on *(flag)*
- **$SL** — selected character count *(number)*
- **$SW** — *Shell Output* tab visibility on *(flag)*
- **$TABACTIVE** — active-tab background colour `R G B`, or empty if at the theme default *(string)*
- **$TABHOVER** — hover-tab background colour `R G B`, or empty if at the theme default *(string)*
- **$TABINACTIVE** — inactive-tab background colour `R G B`, or empty if at the theme default *(string)*
- **$TB** — tab width in spaces *(number)*
- **$TC** — total open tab count *(number)*
- **$TL** — total line count of the document *(number)*
- **$TM** — current time `HH:MM:SS`, local *(string)*
- **$TR** — `1` if shell-out is trusted/enabled *(flag)*
- **$TX** — `1` if focus is the editor text, `0` if the command line *(flag)*
- **$TXTBGCOLOR** — TXT-window background colour `R G B` *(string)*
- **$TXTFAMILY** — TXT-window font family name *(string)*
- **$TXTFGCOLOR** — TXT-window text colour `R G B` *(string)*
- **$TXTFONT** — TXT-window font size *(number)*
- **$TXTSCALE** — TXT-window scale factor *(number)*
- **$TXTSTYLE** — TXT-window font style keyword *(string)*
- **$UIBGCOLOR** — UI chrome background colour `R G B` *(string)*
- **$UIFAMILY** — UI chrome font family name *(string)*
- **$UIFGCOLOR** — UI chrome foreground colour `R G B` *(string)*
- **$UIFONT** — UI chrome font size *(number)*
- **$UISCALE** — overall UI scale factor *(number)*
- **$UISTYLE** — UI font style keyword *(string)*
- **$VE** — XyScript build version (e.g. `0.1.0`) *(string)*
- **$VR** — visible paragraph marks on *(flag)*
- **$WC** — whole-document word count *(number)*
- **$WD** — word under the caret (no surrounding whitespace) *(string)*
- **$WDAY** — day-of-week number *(string)*
- **$WS** — visible whitespace on *(flag)*
- **$XB** — deletion-buffer contents *(string)*
- **$XO** — captured output of the last shell command / job *(string)*
- **$XSDIR** — directory containing the executable *(string)*
- **$YDAY** — day of year, 3 digits *(string)*
- **$YEAR** — current year, 4 digits *(string)*
