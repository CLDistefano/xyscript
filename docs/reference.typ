// reference.typ — The XyScript Reference (testers' draft).
//
// Authored in Typst and rendered by XyScript itself (TOPDF). It is checked against the code by
// xyscript-app/tests/reference_doc.rs: the file must COMPILE, every command/function/special it
// documents must really exist (and every one in the code must be documented), and the runnable
// examples are executed and their output compared to what's printed here.
//
// Authoring conventions (these markers double as the machine-checkable index):
//   #cmd("NEW", "NE")        a command entry header; list synonyms together
//   #sv("CWD")               a readable special variable (rendered as $CWD)
//   #func("UPPER")           an expression function (rendered as UPPER())
//   #ex("code", "output")    a runnable example — executed by the test suite
//   #tip[...]                a friendly aside

#set page(paper: "us-letter", margin: 1in, numbering: "1")
#set text(font: "New Computer Modern", size: 11pt)
#set par(justify: true, leading: 0.70em, spacing: 0.75em)
#set heading(numbering: "1.1")
#show raw: set text(font: "DejaVu Sans Mono")
#show heading.where(level: 1): set text(size: 15pt)
#set terms(spacing: 0.65em, hanging-indent: 1.2em)

#let cmd(..names) = text(weight: "bold", names.pos().map(n => raw(n)).join("  /  "))
#let sv(name) = raw("$" + name)
// `#func("NAME")` → NAME(); `#func("NAME", "a, b")` → NAME(a, b). The first arg is the function name
// (the coverage index reads only that); the optional second is the rendered argument list.
#let func(name, ..sig) = raw(name + "(" + sig.pos().at(0, default: "") + ")")
#let tip(body) = block(width: 100%, fill: rgb("#eef5ff"), stroke: 0.5pt + rgb("#bcd6f5"),
  radius: 4pt, inset: 8pt, [#text(weight: "bold")[Tip.] #body])
#let ex(code, out) = block(width: 100%, fill: luma(245), radius: 3pt, inset: (x: 8pt, y: 6pt),
  grid(columns: (1fr, auto), column-gutter: 10pt, raw(code), text(fill: luma(90))[→ #raw(out)]))

#align(center)[
  #text(20pt, weight: "bold")[The XyScript Reference] \
  #v(2pt) #text(11pt, style: "italic")[A field guide for the first testers] \
  #v(2pt) #text(9pt, fill: luma(110))[draft — a work in progress]
]
#v(8pt)
#line(length: 100%, stroke: 0.5pt)
#v(6pt)

= Welcome

If you've spent years in XyWrite, most of XyScript will feel like coming home.
It's a configurable, keyboard-driven editor built on the same idea you already trust: the
command line is where the real work happens, and what you type there is a small,
readable language — not a maze of menus.

This reference is in two halves. The first is a *guided tour*: the commands
grouped by what you're actually trying to do, with enough plain-English
explanation to get you moving. The second is the *complete reference* — every
command, function, operator, and special variable, listed alphabetically for the
moments when you just need to look one thing up.

You don't have to read it cover to cover. Skim the tour, try a few things, and
come back when you want more.

== How to read this

Commands are shown in *bold monospace*. When several names do the same job, they
share an entry: #cmd("BOTTOM", "BF") means `BOTTOM` and `BF` are
interchangeable — use whichever your fingers remember.

Two other kinds of name show up a lot. A *special variable* carries a piece of
live information and is written with a leading dollar sign — #sv("CWD") is the
folder you're working in. A *function* transforms a value and is written with
parentheses around its arguments — #func("UPPER", "s") shouts the string `s` into
capitals. You'll meet both properly in the tour.

XyScript can also *do arithmetic and more* right on the command line. Anything you
wrap in `$( )` is worked out before the command runs, so the command sees the
answer, not the formula:

#ex("PR $(2 + 3)", "5")
#ex("SET x = 10 ; PR $($x * 2)", "20")
#ex("PR $(UPPER(\"hello\"))", "HELLO")

#tip[Every example in this guide is run automatically whenever the code changes,
and its output is checked against what you see here — so if it's printed in this
box, it really behaves this way.]

= A guided tour

== Moving around

Getting the cursor where you want it is half of editing, so XyScript gives you a
rich set of motions — and, true to XyWrite, most have a short two-letter name you
can rattle off without thinking. Hold the thought that *every* motion can also
*extend a selection* (see _Selecting text_, later): add `SEL` after it and the
motion drags a highlight along with it.

The everyday nudges move by one character or one line:

#cmd("CL", "LEFT", "PC")
#cmd("CR", "RIGHT", "NC")
One character left or right. They glide over a Windows line-ending (the
carriage-return/line-feed pair) as a single step, so the cursor never gets stuck
"inside" a line break.

#cmd("LU", "UP")
#cmd("LD", "DOWN")
One line up or down, keeping your column where you can.

When you want to travel by meaning rather than by keystroke, move by word,
sentence, or paragraph:

#cmd("NW")
#cmd("PW")
Next or previous *word*.

#cmd("NS")
#cmd("PS")
Next or previous *sentence*.

#cmd("NP")
#cmd("PP")
Next or previous *paragraph*.

To snap to the edges of the line you're on:

#cmd("HOME", "EL")
#cmd("END", "ER")
Start or end of the line. (`EL`/`ER` are the "Express Left/Right" names.)

#cmd("LB")
#cmd("LE")
Start or end of the *visual* line — useful when a long line has wrapped on screen
and you want the beginning or end of the wrapped row rather than the whole line.

#cmd("LL")
#cmd("LR")
One character left or right *but never crossing to another line* — handy in
scripts when you want to stay put on the current line.

And to cover whole pages or the whole document:

#cmd("PU")
#cmd("PD")
Up or down by a screenful (the page also scrolls to follow).

#cmd("HM")
#cmd("BS")
To the top or bottom of what's currently *on screen*.

#cmd("NL")
#cmd("PL")
To the start of the next or previous line.

#cmd("TOP", "TF")
#cmd("BOTTOM", "BF")
To the very start or end of the document. (`TF`/`BF` are the XyWrite "Top/Bottom
of File" names.)

#cmd("JMP")
Jump straight to a character position you name — `JMP 0` for the start. Prefix
the number with a sign to move *relative* to where you are: `JMP +20` jumps
twenty characters forward, `JMP -20` twenty back.

#tip[Whenever you read a position back, #sv("CP") is the cursor's character
offset and #sv("LN") is its line number — convenient when a script needs to
remember a spot and return to it.]

== Selecting and editing text

In XyWrite you "defined" a block; XyScript keeps the name. Press `DF` to drop one end
of a *defined selection*, then move the cursor — the highlight follows, no Shift
needed. `DF` again (or `DZ`) closes it; `XD` throws it away. There are shortcuts for
the common shapes: `DW` grabs the word under the cursor, `DL` the line, `DS` the
sentence, `DP` the paragraph, and `DFA` the whole document.

With something selected, the editing verbs act on it: `CUT`, `COPY`, and `PASTE` use
the clipboard; `UC` and `LC` change its case; `RD` deletes it into a side buffer that
`UD` pastes back. To delete in smaller bites, `BD` rubs out the character before the
cursor and `RC` the one after, while `RW` / `RL` / `RS` / `RP` clear the word, line,
sentence, or paragraph at the cursor.

#tip[Typing or pasting over a selection replaces it — the usual thing — unless you turn
on *persistent selection* with `SPS`, which keeps the block in place so you can act on
it more than once.]

== Working with files

`NEW` starts an empty document; `NEW report.typ` starts one already named (and simply
opens the file if it happens to exist). `OPEN` brings up a file, `SAVE` writes it back,
and `SAAS` ("save as") saves under a new name and adopts it. `READ` opens a file
*read-only* so you can look without risk. When you're done, `AB` closes a buffer and
`QUIT` leaves the program, each offering to save anything unsaved.

Everything resolves against your *working directory* — shown on the status line and
readable as #sv("CWD"). Change it with `CD`. To turn a finished document into a PDF,
use `TOPDF`: a `.typ` file is typeset with Typst, anything else rendered as Markdown
(see `DOCTYPE`).

== Finding and changing text

`SE` searches forward from the cursor for the next match (`SEB` goes backward); add `A`
for case-sensitive (`SEA`) or the `/R` switch for a regular expression. To replace,
`CH` changes every match from the cursor onward, and `CV` walks you through them one at
a time so you can approve each. The `/S` switch keeps a search or replace inside the
current selection.

== Windows, tabs, and panes

Open several files and they ride in tabs along the top — `BUFNEXT` / `BUFPREV` step
between them. A long file name is truncated to fit its tab (the start and extension
are kept); hovering a tab shows a tooltip with the file's full path and name — always,
not only when the label is shortened. `DIR` browses a folder, and `DIR/W` puts that browser in its own window.
Two side panes can ride along: `SH` opens a shell at the bottom for running commands,
and `PV` opens the live PDF preview on the right, which re-renders as you type.

= Adapting your keyboard file

One of the first things you'll want to do is carry over your keyboard. Good news:
an XyScript KBD file will look familiar. It opens with a short stanza, the layers
are written as tables, and the keys are still the classic scan codes 1–104.

The stanza declares how many keys there are and which scan codes act as shift
keys; each `TABLE=` line then starts a layer (the bare `TABLE=` is the unshifted
layer):

```
KEYS=104
SHIFT=42,54
CTRL=29,99
ALT=56,98
CAPS=58,T:C

TABLE=
14=BD
16=q
57=" "

TABLE=SHIFT
16=Q
```

Comments are *whole-line only* — a line whose first non-space character is `;`.
There are no inline comments, because a `;` after the `=` is part of the
assignment (it chains commands, as you'll see below).

== Writing key assignments

What changes is mostly the right-hand side. A few forms work exactly as they did
in XyWrite:

```
; a single character — types that character
nn=A
```

The rest are new. A *quoted string* types its whole text (use `\"` for a quote
and `\\` for a backslash inside it); a bare word is a *command*; and you can
*chain* commands with `;` — the same separator you use on the command line:

```
; multiple characters (a string)
nn="My name is \"Ada\", what's yours?"

; a single command
nn=AB

; several commands in a row
nn=SA;AB
```

Because the right-hand side is just XyScript, a whole conditional can live on one
key — test something with `IF`, separate the branches with `ELSE`, and close with
`EI`:

```
; if text is defined, delete it; otherwise delete the character ahead
nn=IF $DF;DZ;RD;ELSE;RC;EI
```

The catch is the vocabulary, not the format: some XyWrite commands no longer
exist and a good many new ones have arrived. When a key stops doing what it used
to, look the command up in the reference that follows.

= Writing scripts (XPL)

The language you've been reading is *XPL* — the same name XyWrite gave its
programming language, and the same idea: a list of commands run start to finish.
What's changed is how it reads. Where the old XPL leaned on function-key codes
and frames, this one is plain words, named variables, and ordinary arithmetic —
much closer to how you'd describe the steps out loud.

Anything you can type on the command line is a line of a script. Save the lines
in a file (`.xpl` by convention) and run them with `RUN <file>`; run a fenced
block sitting in your document with `RUNHERE`; or bind the whole thing to a key.
Comments start with `#` — on their own line, or after a command with a space in
front of it — and run to the end of the line.

A long line can be broken across several by ending it with *a space and a
backslash* — the next line is joined on as if it were one. The space matters: a
backslash tucked against text (like a path ending `C:\`) is left alone, so only
` \` at the very end continues the line; write `\\` for a literal trailing
backslash.

A script keeps values in *variables*. `SET` assigns one and you read it back with
a leading `$`. A bare word with no `$` is just literal text, which is the one
thing to watch for: `WHILE count < 10` compares the word "count", not your
counter — you want `$count`.

#ex("SET name = \"Ada\" ; PR Hello $name", "Hello Ada")

Expressions handle arithmetic, comparison, and string work (the `.` operator
joins strings), and a library of functions like #func("UPPER") is available
wherever a value is expected — all gathered in the _Operators_ and _Functions_
sections. Branching and looping use the block commands from the reference:
`IF … ELSE … EI`, `WHILE … EW`, `FOR … EF`, and `SWITCH … ESW`. To add up the
first few numbers:

```
SET total = 0
FOR i = 1 TO 5
  SET total += $i
EF
PR total is $total      # total is 15
```

Work you do more than once goes in a *routine* — define it with `SUB`, run it
with `CALL` (or just by name — `myroutine $arg` works like `CALL myroutine $arg`),
and hand a result back with `RETURN` (read afterwards as #sv("RV")).
Inside a routine the arguments arrive as `$1`, `$2`, … with `$#` for how many.
A comment line of the form `# DOC: <text>` inside a routine documents it — every
such line shows up under that routine in `LISTLIB`. (Note the space: `#DOC:` with
no space isn't a comment and would error; write `# DOC:`.)
That, plus the command reference that follows, is the whole language.

= Commands by topic

A scannable index of every command, grouped by what it does. Synonyms share one
#cmd entry; a few commands appear under more than one heading. The full alphabetical
listing — with switches, arguments, and the finer points — follows in _Commands_.

== Moving the cursor

/ #cmd("CL", "LEFT", "PC") / #cmd("CR", "RIGHT", "NC"): one character left / right (gliding over a CRLF as one step).
/ #cmd("LU", "UP") / #cmd("LD", "DOWN"): one line up / down, keeping the column.
/ #cmd("NW") / #cmd("PW"): next / previous word.
/ #cmd("NS") / #cmd("PS"): next / previous sentence.
/ #cmd("NP") / #cmd("PP"): next / previous paragraph.
/ #cmd("NL") / #cmd("PL"): start of the next / previous line.
/ #cmd("HOME", "EL") / #cmd("END", "ER"): start / end of the line.
/ #cmd("LB") / #cmd("LE"): start / end of the visual (wrapped) row.
/ #cmd("LL") / #cmd("LR"): one character left / right without crossing to another line.
/ #cmd("PU") / #cmd("PD"): up / down one screenful.
/ #cmd("HM") / #cmd("BS"): to the top / bottom of what's on screen.
/ #cmd("TOP", "TF") / #cmd("BOTTOM", "BF"): to the start / end of the document.
/ #cmd("JMP"): jump to a character offset — absolute or `+n`/`-n` relative.
/ #cmd("MD") / #cmd("MU"): scroll the view down / up one line without moving the caret.

== Bookmarks

/ #cmd("MARKSET") / #cmd("MARKCLEAR") / #cmd("MARKTOGGLE"): set / clear / toggle a bookmark at the caret. #cmd("MARKCLEARALL"): remove every bookmark in the document.
/ #cmd("MARKNEXT") / #cmd("MARKPREV"): move the caret to the next / previous bookmark, wrapping around the ends. #cmd("MARKLIST"): a chooser of every bookmark (line number + text) — pick one to jump to it. #sv("MARKS") is the count; #sv("MARK") is `1` when the caret sits on one.
/ #cmd("MARKGUTTER"): show or hide the left-gutter bookmark dot — `MARKGUTTER on|off` (bare toggles). On by default; saved by `SAVECFG`. Read back with #sv("MARKGUTTER").
Bookmarks are per-document, marked by a dot in the left gutter, and saved in the file's hidden `.xymeta` sidecar — a mark whose text has moved is relocated to the nearby text (a status note says so).

== Selecting and defining blocks

/ #cmd("DF"): start (or close) a defined selection. #cmd("DFA"): select the whole document. #cmd("DM"): extend it to the caret. #cmd("DZ"): close it.
/ #cmd("DW", "DW+") / #cmd("DS", "DS+") / #cmd("DP", "DP+") / #cmd("DL"): define the word / sentence / paragraph (`+` takes the trailing break) / line.
/ #cmd("DB") / #cmd("DE"): move to the start / end of the selection. #cmd("XD", "YD"): cancel the selection.
/ #cmd("SPS", "CPS", "TPS"): persistent selection — on / off / toggle.
/ #cmd("SV"): save the selection's text into a variable.

== Inserting and typing text

/ #cmd("INSERT", "WRITE", "WR") / #cmd("WRITELN", "WRL"): insert text at the caret (`WRITELN` adds a trailing newline).
/ #cmd("PUT"): insert substituted text at the caret. #cmd("MERGE", "ME"): insert a file's contents.
/ #cmd("ENTER"): insert a newline in the document's line-ending style.
/ #cmd("PRINTEX"): copy the document expanding each `|…|` field (substitutions, `Cn` counters, or live code).

== Deleting text

/ #cmd("BD") / #cmd("RC"): delete the character before / after the caret.
/ #cmd("RE") / #cmd("RL") / #cmd("RLL") / #cmd("RW") / #cmd("RS") / #cmd("RP"): delete to end-of-line / the current visual (wrapped) line / the whole logical line / word / sentence / paragraph.
/ #cmd("DELSEL", "DN"): delete the selection.

== Changing case and sorting

/ #cmd("LC") / #cmd("UC") / #cmd("CC"): lower-case / upper-case / swap-case the selection (or the character at the caret).
/ #cmd("SORT"): reorder the selected lines.

== Clipboard and the deletion buffer

/ #cmd("CUT") / #cmd("COPY") / #cmd("PASTE"): the clipboard verbs (`COPY <src> <dst>` copies a file).
/ #cmd("APPEND") / #cmd("PREPEND") / #cmd("CLIP"): add the selection to the end / start of the clipboard / set it from a string.
/ #cmd("CP") / #cmd("MV"): copy / move the selection to the caret.
/ #cmd("RD") / #cmd("UD"): delete the selection into the deletion buffer / paste it back.

== Undo and redo

/ #cmd("UNDO") / #cmd("REDO"): undo / redo edits (a count or `ALL`).

== Finding and changing text

/ #cmd("SE", "SEB", "SEA", "SEBA"): search (`B` backward, `A` case-sensitive).
/ #cmd("CH", "CHA", "CI", "CIA") / #cmd("CV", "CVA"): replace every match / with a prompt at each.
/ #cmd("FIND"): open the Find/Replace dialog.
/ #cmd("GREP") / #cmd("GJ"): recursively search a folder (default `$CWD`) for a pattern into a results tab / jump to the hit under the caret.

== Counting

/ #cmd("WORDCOUNT") / #cmd("CHARCOUNT"): count words / characters in the selection or document.

== Files

/ #cmd("NEW", "NE"): a new empty (or named) buffer. #cmd("OPEN", "CA", "ED", "EDIT"): open a file. #cmd("READ"): open read-only. #cmd("BEDIT"): edit invisibly (for scripts). #cmd("BCLOSE"): close that invisible buffer.
/ #cmd("SAVE", "SA") / #cmd("SAAS", "SAVEAS") / #cmd("SAD"): save / save-as-and-adopt / save just the selection.
/ #cmd("ST") / #cmd("SF") / #cmd("SL"): save then close the tab / save and close all / save every modified buffer.
/ #cmd("REC", "RECALL"): revert to the last saved state. #cmd("RO"): the read-only lock. #cmd("AUTOSAVE"): the autosave interval.
/ #cmd("AB") / #cmd("ABD"): close the buffer / delete its file and close. #cmd("QUIT", "QQ"): quit the program.
/ #cmd("DEL") / #cmd("ERNV"): delete a file with / without confirmation. #cmd("RENAME", "REN"): rename or move a file. #cmd("WRITEFILE", "WF"): write content to a file.
/ #cmd("REOPEN") / #cmd("CLEARCLOSED"): reopen recently-closed files (a picker, or `REOPEN <n>` for the first n) / empty the list.

== Tabs and buffers

/ #cmd("BUFNEXT") / #cmd("BUFPREV") / #cmd("BUFALT"): switch to the next / previous / alternate buffer.
/ #cmd("BUFGO") / #cmd("GOFILE", "GOF"): switch to a buffer by name or index / by a path substring.
/ #cmd("BUFLIST") / #cmd("LISTTABS"): list the open buffers / open a modal tab chooser.

== Directories and listings

*Point-and-shoot.* In a `DIR`/`LS` listing, a file command typed with NO argument acts on the file
on the caret's row — so parking the caret on a name and typing `OPEN`, `DEL`, `RUN`, `ED`, `BEDIT`,
`LOADKBD`/`LOADLIB`/`LOADAR`/`LOADVARS`, `LANG`, or `NE` operates on that file, as if you had typed
its path.
It applies only when the argument is empty and the caret is on a real entry (not the `..` parent);
give an explicit path and that always wins. Two-argument commands (`COPY`, `RENAME`) and the
save-to-a-path verbs (`ST`, `SAAS`) are excluded — their argument isn't a file to read but a
destination. Deleting a file (`DEL`/`ERNV`, however the path was given) also drops its row from any
open listing, and the caret follows onto the entry that came next; renaming one (`RENAME`) updates
its row to the new name and re-sorts it into place, with the caret following the file.

/ #cmd("CD"): change the working directory (or switch drive).
/ #cmd("DIR", "LS"): open a directory-listing browser.
/ #cmd("DIRNW") / #cmd("DIRWNW") / #cmd("DIRWCLOSE") / #cmd("DIRRT"): open-in-new-tab modes for an in-tab listing / the `DIR/W` window, whether `DIR/W` closes after opening a file, and reuse-the-listing-tab mode.
/ #cmd("DIRSET"): default sort/filter/view switches applied to every `DIR`/`LS`.

== Split panes

/ #cmd("THS") / #cmd("TVS"): split the pane stacked / side-by-side.
/ #cmd("TW") / #cmd("TRV"): move focus to the next pane / swap the two panes.
/ #cmd("TUS") / #cmd("FS"): un-split the focused pane / release it to full screen.
/ #cmd("TZ"): toggle zoom of the focused pane (the split is preserved).

== The command line

/ #cmd("CMLINE", "GH") / #cmd("GT") / #cmd("TH"): focus the command line / the document / toggle between them.
/ #cmd("SUBMIT") / #cmd("XC") / #cmd("CANCEL"): run the command line / run it keeping focus / cancel and close it.
/ #cmd("BC"): blank the command line (`BC <text>` puts literal text on it). #cmd("CMH"): stash a command in the history without running it.
/ #cmd("HISTORY", "HI") / #cmd("LOADCM") / #cmd("SAVECM"): open the command-history picker / load / save the history.
/ #cmd("CMPLACE") / #cmd("CMBLANK"): where the command line sits / whether it clears after a command.
/ #cmd("SGC", "CGC", "TGC"): the pop-up command line — on / off / toggle.

== Coding aids

/ #cmd("TAB") / #cmd("TB"): indent/outdent / the tab width.
/ #cmd("TABSPACES") / #cmd("TABINDENT") / #cmd("WRAPINDENT"): soft tabs / auto-indent / hanging indent (on/off).
/ #cmd("BALPAIRS") / #cmd("BALPAIRSCOLOR", "BALPAIRSCOLORS") / #cmd("TYPEPAIRS"): matched-bracket highlighting / its color / auto-close typed pairs.

== Counters

/ #cmd("C1", "C2", "C3", "C4", "C5", "C6", "C7", "C8", "C9"): nine per-document counters (number / letter / Roman) — seed one with a type and decorator, then a bare `Cn` puts the value and advances.

== Auto-replace

/ #cmd("AR"): swap the word behind the caret for its loaded value.
/ #cmd("LOADAR") / #cmd("SAVEAR") / #cmd("CLEARAR"): load / save / empty the auto-replace map.

== View and display toggles

/ #cmd("SMB", "CMB", "TMB"): the menu bar — on / off / toggle. #cmd("MENU"): keyboard control of the menu bar.
/ #cmd("SVR", "CVR", "TVR") / #cmd("SWS", "CWS", "TWS") / #cmd("SLN", "CLN", "TLN"): paragraph marks / whitespace / the line-number gutter — on / off / toggle.
/ #cmd("SCL", "CCL", "TCL") / #cmd("SNL", "CNL", "TNL") / #cmd("SSL", "CSL", "TSL"): Caps / Num / Scroll Lock — set / clear / toggle.
/ #cmd("SHOWONETAB") / #cmd("UNTITLEDTAB") / #cmd("GOTOOPEN") / #cmd("TITLEBAR") / #cmd("WV") / #cmd("WINGEO") / #cmd("SESSION"): show the tab strip for a lone file / auto-close the blank Untitled tab on open / switch to a file's existing tab instead of prompting when it is already open / the OS title bar / Writer's View (hide all chrome; `Esc` exits) / remember the window geometry / auto-save-and-restore the open tabs across runs.
/ #cmd("CARETMOTION"): the order Left/Right move the caret — `logical` (default) or `visual`. Visual steps in screen order, reversing inside a right-to-left run so the cursor moves the way your eye travels.
/ #cmd("CARETADD") / #cmd("CARETUP") / #cmd("CARETDOWN") / #cmd("CARETCOL") / #cmd("CARETALL") / #cmd("CARETNEXT") / #cmd("CARETSKIP") / #cmd("CARETSPLIT") / #cmd("CARETPOP") / #cmd("CARETPRIMARY") / #cmd("CARETONE"): multi-caret — add a caret (bare `CARETADD` plants one and lets you roam to plant more; `CARETADD <n>` at an offset), add a caret above / below each, turn a selection into a column of carets (one per line at the cursor's column — short lines get their caret at the line end), put a selection-caret at every match of a pattern (or of the current selection / word), add the next occurrence one at a time (like Ctrl+D), skip an unwanted occurrence mid-run (`CARETSKIP`), split a multi-line selection into one caret per line-end (`CARETSPLIT`), undo the last caret change (`CARETPOP`), make a numbered caret the primary (`CARETPRIMARY <n>`), or collapse back to one. Every edit and motion then acts on all carets at once. Also: Ctrl+click adds, plain click collapses, Alt+drag makes a box (column) selection — a caret per line across the dragged rectangle — and a bare `CARETADD` with a block defined does `CARETCOL`.
/ #cmd("WRAP"): word-wrap long lines to the window (`on`, default) or let them run past the edge with a horizontal scrollbar (`off`).
/ #cmd("WRAPHANG"): hanging indent (spaces) for wrapped continuation rows — `WRAPHANG <n>` (0 = flush; ignored when #cmd("WRAPINDENT") is on).

== Colors and appearance

/ #cmd("FGCOLOR", "FGCOLORS") / #cmd("BGCOLOR", "BGCOLORS"): the editor text / background color.
/ #cmd("UIFGCOLOR", "UIFGCOLORS") / #cmd("UIBGCOLOR", "UIBGCOLORS") / #cmd("SEPCOLOR", "SEPCOLORS"): the UI chrome's text / background / separator color.
/ #cmd("SELFGCOLOR", "SELFGCOLORS") / #cmd("SELBGCOLOR", "SELBGCOLORS"): the selected text's color / highlight.
/ #cmd("FINDFGCOLOR", "FINDFGCOLORS") / #cmd("FINDBGCOLOR", "FINDBGCOLORS"): the FIND "highlight all" color / background.
/ #cmd("EMPH") / #cmd("EMPHFGCOLOR", "EMPHFGCOLORS") / #cmd("EMPHBGCOLOR", "EMPHBGCOLORS"): emphasize a word/phrase in the current tab / its text color / its background.
/ #cmd("MDA") / #cmd("MDAS"): style the selection as a persistent text block — foreground colour, background colour (`/B`), or an attribute (`BO`/`IT`/`BI`/`UL`/`BU`/`RV`, `NM` to clear) — saved with the file; `MDAS` opens a colour picker.
/ #cmd("ULSPACES"): whether an `MDA` underline runs through the spaces between words (`on`, default) or breaks at them (`off`).
/ #cmd("CRCOLOR", "CRCOLORS") / #cmd("CRSHAPE") / #cmd("CMCRCOLOR", "CMCRCOLORS") / #cmd("CMCRSHAPE"): the text / command-line caret color and shape.
/ #cmd("TABBGCOLOR", "TABBGCOLORS") / #cmd("TABXCOLOR", "TABXCOLORS") / #cmd("PANECOLOR", "PANECOLORS"): the tab background / close-× / focused-pane border color.
/ #cmd("TXTFGCOLOR", "TXTFGCOLORS") / #cmd("TXTBGCOLOR", "TXTBGCOLORS") / #cmd("DWFGCOLOR", "DWFGCOLORS") / #cmd("DWBGCOLOR", "DWBGCOLORS"): the `TXT` and `DIR/W` window text / background colors.
/ #cmd("PAPLACE") / #cmd("PATHLEN") / #cmd("PAFGCOLOR", "PAFGCOLORS") / #cmd("PABGCOLOR", "PABGCOLORS"): where the `$CWD` path shows / its max width / its text and background color.
/ #cmd("THEME"): apply a built-in light or dark color scheme, or activate a theme file by path.

== Fonts and spacing

/ #cmd("EDFONT", "EDFONTS") / #cmd("EDFAMILY") / #cmd("EDSTYLE"): the editor font's size / family / style.
/ #cmd("UIFONT", "UIFONTS") / #cmd("UIFAMILY") / #cmd("UISTYLE") / #cmd("UISCALE") / #cmd("UIRESET"): the UI chrome's font size / family / style / scale / reset.
/ #cmd("TXTFONT", "TXTFONTS") / #cmd("TXTFAMILY") / #cmd("TXTSTYLE") / #cmd("TXTSCALE"): the `TXT` window's font.
/ #cmd("DWFONT", "DWFONTS") / #cmd("DWFAMILY") / #cmd("DWSTYLE") / #cmd("DWSCALE") / #cmd("DWRESET"): the `DIR/W` window's font.
/ #cmd("LEADING", "LSP"): the line spacing. #cmd("OF") / #cmd("OFL") / #cmd("OFR"): both margins / the left / the right.

== Syntax highlighting

/ #cmd("HIGHLIGHT") / #cmd("HLAUTO"): turn highlighting on/off / auto-enable it for recognized files.
/ #cmd("HEX"): `HEX on|off` (bare `HEX` toggles) — show and edit the active document as a hex + text grid (for binary files). A per-document view mode; navigate by byte (arrows ±1, ↑/↓ one row) and edit in place: type hex pairs, `Tab` to the text column to type characters, `Insert`/`Delete`/`Backspace` add and remove bytes, and copy/cut/paste bytes. #sv("HEX") reads it.
/ #cmd("HLLANG") / #cmd("HLRULE"): declare a regex-grammar highlighter and its rules.
/ #cmd("HLCOLOR", "HLCOLORS") / #cmd("HLKINDS") / #cmd("HLRESET"): a token kind's color / list the kinds / restore the default theme.

== Encodings and line endings

/ #cmd("ENCODING"): set (or convert) the file's text encoding. #cmd("NEWLINE"): convert line endings to `lf` / `crlf`.

== Markdown, HTML, and Typst preview and export

A document is rendered by its `DOCTYPE`: Markdown (GFM) and HTML are each translated to
typst markup and typeset by XyScript's own renderer, while an authored `.typ` document is
compiled as-is. Under `auto` the extension picks: `.typ` → typst, `.html` / `.htm` /
`.xhtml` → HTML, everything else → Markdown.

HTML is converted *structurally*. Headings, paragraphs, bold/italic, lists, blockquotes,
links, images, tables, and `<pre>` code blocks become their typst equivalents; `<script>`,
`<style>`, and `<head>` are dropped; unknown or layout-only elements (`<div>`, `<span>`)
simply contribute their contents. What is *not* interpreted is presentation: `class`,
`style`, and stylesheets are ignored. So an HTML document renders as a readable document —
but a web page whose appearance comes from CSS (a marketing email built out of nested
layout tables, say) renders as its content, not as a copy of the page.

The same converter handles a raw HTML *block* inside a Markdown file, so a table or figure
written in HTML in the middle of Markdown renders instead of appearing as its own source.
Raw HTML written *inline* in a line of Markdown (`a <b>word</b> here`) is still shown as
text — Markdown hands those to us as separate opening and closing fragments, with no
self-contained element to convert.

/ #cmd("PREVIEW", "PV"): show/hide the live PDF preview pane. #cmd("TOPDF", "PRINT"): export to PDF. #cmd("DOCTYPE"): how the document renders (`typst` / `gfm` / `html` / `auto`).
/ #cmd("PVFAMILY") / #cmd("PVSIZE") / #cmd("PVCODE") / #cmd("PVFONTS") / #cmd("PVCODES"): the body / code fonts for the Markdown and HTML renderers (and their pickers). An authored typst document sets its own fonts instead.
/ #cmd("PVMARGIN") / #cmd("PVMARGINL") / #cmd("PVMARGINR") / #cmd("PVMARGINT") / #cmd("PVMARGINB"): the page margins for the Markdown and HTML renderers — all sides / left / right / top / bottom.

== Language checking

/ #cmd("CHECK"): a one-shot spelling-and-grammar report. #cmd("SPELLING", "SP") / #cmd("GRAMMAR", "GR"): live spelling / grammar underlines (on/off).
/ #cmd("SPELLNEXT") / #cmd("SPELLPREV") / #cmd("GRAMMARNEXT") / #cmd("GRAMMARPREV"): jump to the next / previous issue.
/ #cmd("LINTFIX"): correct the issue at the caret. #cmd("DIALECT"): the English dialect. #cmd("SPELLENGINE") / #cmd("SPELLDICT") / #cmd("SPELLBIN"): switch spelling to the Hunspell CLI (multilingual) and set its dictionaries / executable. #cmd("LEARN") / #cmd("UNLEARN"): add / remove a personal-dictionary word.
/ #cmd("SPELLCOLOR", "SPELLCOLORS") / #cmd("GRAMMARCOLOR", "GRAMMARCOLORS") / #cmd("SPELLSTYLE") / #cmd("GRAMMARSTYLE"): the underline colors and shapes.

== Shell and background jobs

/ #cmd("X") / #cmd("XV") / #cmd("XY"): run a shell command / capturing its output to a variable / inserting its output at the caret.
/ #cmd("TRUST"): enable or disable shell-out. #cmd("JOBS") / #cmd("KILL") / #cmd("WAIT"): list background jobs / end one / wait for jobs (or a timed pause).
/ #cmd("SH", "SHELL"): open the Console — an editable REPL tab (type a command at the prompt, press Enter). #cmd("SHPROMPT"): its prompt template. #cmd("ANSICOLOR"): colour output from ANSI codes. #cmd("SW", "SHELLWIN"): show/hide the Console tab.

== Keyboard files and binders

/ #cmd("LOADKBD") / #cmd("SAVEKBD") / #cmd("CLEARKBD"): load / save / reset the keyboard map. #cmd("KBDDOC"): annotate a KBD file with a key-name column.
/ #cmd("LDTX") / #cmd("LDSC", "LDPM"): load a text / script binder register.
/ #cmd("SAVELDTX") / #cmd("SAVELDSC", "SAVELDPM"): save the text / script binder registers as a runnable program.

== Scripting and flow control

/ #cmd("IF", "IFNOT") / #cmd("ELSE") / #cmd("ELSEIF") / #cmd("ENDIF", "EI"): conditional blocks.
/ #cmd("WHILE", "ENDWHILE", "EW") / #cmd("FOR", "ENDFOR", "EF") / #cmd("BREAK") / #cmd("CONTINUE"): loops and loop control.
/ #cmd("SWITCH", "CASE", "DEFAULT", "ENDSWITCH", "ESW"): a multi-way branch.
/ #cmd("SUB", "ENDSUB", "ES") / #cmd("CALL") / #cmd("RETURN") / #cmd("GOTO"): routines, calls, returns, and label jumps.
/ #cmd("SET") / #cmd("LOCAL") / #cmd("CLEAR"): assign / scope / unset variables.
/ #cmd("CAPTURE", "ENDCAPTURE", "EC"): redirect a command's output into a variable.
/ #cmd("RUN") / #cmd("RUNCODE") / #cmd("RUNHERE"): run a script file / XPL text in a variable / the fenced block under the caret.
/ #cmd("EXIT", "EX") / #cmd("EXITALL", "EX1") / #cmd("NOP"): halt this script / every script / do nothing.
/ #cmd("QUIET") / #cmd("SETERR") / #cmd("CLERR") / #cmd("BK"): suppress messages / set / clear the `$ERROR` flag / a debugging breakpoint.
/ #cmd("SRANDOM"): seed the `RANDOM` generator. #cmd("LISTSPECIALS"): insert a template of every special variable.

== Libraries

/ #cmd("LOADLIB") / #cmd("ADDLIB") / #cmd("RELOADLIB") / #cmd("UNLOADLIB"): load / merge / reload / drop library routines.
/ #cmd("SAVELIB") / #cmd("LISTLIB"): write the loaded routines to a `.lib` / list them.

== Variables, config, and persistence

/ #cmd("SAVEVARS") / #cmd("LOADVARS") / #cmd("LISTVARS", "LISTVAR"): save / restore / list global variables.
/ #cmd("SAVECFG") / #cmd("LOADCFG") / #cmd("AUTOCFG"): write / run / live-track the config script.
/ #cmd("LANG"): load a UI translation table (localize the menu/dialog labels); `LANG <xx>` is shorthand for `xx.lang`; bare / `off` reverts to English.
/ #cmd("LOADCLOSED") / #cmd("SAVECLOSED"): load / save the recently-closed (reopen) list.
/ #cmd("DECIMALS"): the number of decimal places floats display with.

== Dialogs and interaction

/ #cmd("MSG") / #cmd("MESSAGE") / #cmd("TXT"): a message box / an OS-native box (works headless) / a scrollable text dialog.
/ #cmd("PR"): print to the status line or script output.
/ #cmd("INPUT") / #cmd("READKEY", "RK") / #cmd("PICK", "CHOOSE"): read a typed line / a single keystroke / a list choice into a variable.

== Date and time

/ #cmd("DA") / #cmd("ET") / #cmd("ZT"): insert the date and time / the stopwatch's elapsed time / reset the stopwatch.

== Help

/ #cmd("HELP", "?"): look up reference help for a command, special, or function.
/ #cmd("KBCHART"): show the loaded keymap's chart — every modifier table and its key bindings — in a scrollable text box.

== Email

Send and receive mail by shelling out to `curl` (which must be on your `PATH`). Configure an account once — ideally in `startup.xpl` — then send or receive. Passwords are session-only and never persisted.

/ #cmd("SMTPURL") / #cmd("MAILUSER") / #cmd("MAILFROM") / #cmd("MAILPASS"): SMTP account config — the submission URL, username, `From:` address (defaults to the username), and password (never echoed; also read from `$XYSCRIPT_MAILPASS`).
/ #cmd("SENDMAIL"): send the active document (or, with `/F`, a file) as email — recipient and subject from arguments or a `To:`/`Subject:` header block atop the buffer.
/ #cmd("ATTACH") / #cmd("CLEARATTACH"): stage / clear files to attach to the next #cmd("SENDMAIL").
/ #cmd("IMAPURL") / #cmd("MAILBOX"): IMAP receive config — the server URL and the mailbox to read (default `INBOX`); auth reuses #cmd("MAILUSER")/#cmd("MAILPASS").
/ #cmd("LISTMAIL"): list the mailbox (numbered Date/From/Subject) in *Console*.
/ #cmd("SAVEMAIL"): download raw messages by number (or `all`) to `MSG###.TXT` files.
/ #cmd("DELMAIL"): delete messages by number (or `all`).
/ #cmd("MAILDEBUG"): show the full curl / server dialogue for troubleshooting.

= Commands

The full command list, alphabetical. Cursor motions are in the tour (_Moving around_);
everything else is here. Synonyms share an entry, so #cmd("OPEN", "CA", "ED", "EDIT") means
all four do the same thing. A note in parentheses — like `(/HSL)` — flags a switch the command
accepts. Block-structure keywords (`IF`, `FOR`, …) are grouped near their openers.

== A

/ #cmd("AB"): close the active buffer, discarding changes — `/V` asks to save first. If the buffer is showing in a split pane, that pane is undone too (its space goes back to the neighbor) — closing the file and its pane is a single operation. To un-split *without* closing the file, use #cmd("TUS").
/ #cmd("ABD"): delete the active document's file from disk and close it — `/V` confirms first.
/ #cmd("ADDLIB"): merge a `.lib` library into the loaded user library.
/ #cmd("APPEND"): add the selection to the end of the clipboard.
/ #cmd("AR"): auto-replace — swap the word behind the caret for its loaded value (see `LoadAR`). The expansion MATCHES THE CASE you typed the key in, so one entry `equ=equilibrium` covers all three: `equ` → `equilibrium`, `Equ` → `Equilibrium`, `EQU` → `EQUILIBRIUM`. A key typed in lower case (or in a mixed pattern like `eQu`) reproduces the value exactly as written, so a deliberately-capitalized value such as `usa=United States of America` survives; and an exactly-matching key always wins over case-matching, so you can still define `equ` and `EQU` separately. If that value begins with `#`, the rest is run as XPL instead of inserted (and `AR` then stops the rest of its key's command chain — so `AR;" "` won't add the space); XPL values are never recased.
/ #cmd("ATTACH"): stage a file to send with the next #cmd("SENDMAIL") — `ATTACH <path>` (resolved against `$CWD`; a missing file errors and stages nothing). Bare `ATTACH` lists the staged files, `$ATTACH` reads them (comma-separated), and #cmd("CLEARATTACH") empties the list. The send consumes the list, so the next message starts clean. (In a compose header block you can add `Attach:` lines instead.)
/ #cmd("AUTOCFG"): start (or, with `off`, stop) live self-updating config tracking.
/ #cmd("AUTOSAVE"): set the autosave interval in seconds (`off` or `0` disables it).

== B

/ #cmd("BS"): move to the bottom of what's currently on screen.
/ #cmd("BOTTOM", "BF"): move to the very end of the document.
/ #cmd("BC"): blank (clear) the command line. `BC <text>` blanks it then puts the *literal* text on it — no `$`-expansion — typically followed by `XC`, where the expansion happens.
/ #cmd("BD"): delete the character before the caret (backspace).
/ #cmd("BEDIT"): blind-edit a file in an invisible buffer (for scripting). #cmd("BCLOSE"): close (discard) that blind buffer — the explicit counterpart of an `AB` while blind. `BCLOSE <file>` also asserts *which* file it's closing (a name mismatch, or no blind edit open, sets `$ERROR`); bare `BCLOSE` closes whatever blind buffer is open.
/ #cmd("BGCOLOR", "BGCOLORS"): the editor background color `R G B` (`/HSL`); the `…S` form opens a color picker.
/ #cmd("BK"): an inline debugging breakpoint — `BK on` pauses with a stop prompt, `BK debug` shows the live variables, `BK off` disables it.
/ #cmd("BREAK"): exit the enclosing `WHILE` or `FOR` loop.
/ #cmd("BUFNEXT"): switch to the next buffer.
/ #cmd("BUFPREV"): switch to the previous buffer.
/ #cmd("BUFALT"): toggle between the current buffer and the *alternate* — the one you were on before the last switch. Press it again to flip back. Handy on a keybinding for bouncing between two files. (Seeds from #cmd("BUFPREV") the first time, or after a buffer is closed.)
/ #cmd("BUFGO"): switch to a buffer by name or index.
/ #cmd("BUFLIST"): list the open buffers.
/ #cmd("LISTTABS"): open a modal chooser of the open tabs (number + filename; the current tab is flagged with `*`). Switch to a tab by clicking it, or by selecting it with the arrow keys and pressing Enter; Esc cancels. Type to filter. The interactive sibling of #cmd("BUFLIST").

== C

/ #cmd("CL", "LEFT", "PC") / #cmd("CR", "RIGHT", "NC"): move one character left / right (gliding over a Windows CRLF line ending as a single step).
/ #cmd("C1", "C2", "C3", "C4", "C5", "C6", "C7", "C8", "C9"): nine per-document counters. `Cn <seed>` sets the type, start value, and an optional text decorator (`C1 "1. "`, `C2 "(A) "`, `C3 ivr`, `C4 Ir`) and prints nothing; a bare `Cn` puts the value with its decorator and advances; `Cn +n` / `Cn -[n]` / `Cn =` put a value n greater / n less (`-` ≡ `-1`) / the same as the last one. Types: number (`1`), letter (`a`/`A`, running `a…z, aa, ab…`), and Roman numeral (numerals + a trailing `r`/`R` marker — `ir`→i, `Ir`→I, `ivr`→iv). Counters are session-only. Peek the next value (no decorator, no advance) with `$C1`..`$C9`. #ex("C1 \"1. \" ; C1 ; PR $C1", "2")
/ #cmd("CALL"): call a `SUB` or library routine, with arguments. The `CALL` is optional — a routine can be invoked by name like any command (`myroutine $a`).
/ #cmd("CANCEL"): cancel and close the command line.
/ #cmd("CAPTURE", "ENDCAPTURE", "EC"): redirect a command's output into a variable — `CAPTURE <var> <command>`, or a block ending in `ENDCAPTURE`/`EC`.
/ #cmd("CC"): swap the case of the selection — or, with no selection, the character under the caret, then step one character right (so you can sweep along the text).
/ #cmd("CD"): change the working directory; if you name a drive it switches to that drive too (the `/D` switch is optional). A bare drive-qualified path on its own — `D:`, `X:\proj`, `x:rel` — is itself a `CD` (XyWrite muscle memory: type `D:` to switch drives). `subst`ed drives are kept as `drive:\…`, not rewritten to their backing path.
/ #cmd("CHECK"): run the language checker (spelling and grammar) over the active document; the report (one line per issue, `line:col [kind] "text": message`) opens in a scrollable read-only `*Language Check*` tab. Under CAPTURE it goes to the variable instead. (A one-shot; #cmd("SPELLING")/#cmd("GRAMMAR") are the live versions.)
/ #cmd("SPELLING", "SP") / #cmd("GRAMMAR", "GR"): `<verb> on|off` — toggle LIVE spelling / grammar underlines in the canvas, independently. Checking runs on a background thread (never blocks); #sv("SPELLING") / #sv("GRAMMAR") read the states. A spelling underline is held back for the word you are actively typing — it appears only once you finish the word (type a space or other separator) — and every underline hugs the word itself, never the surrounding whitespace. Right-click an underline (spelling OR grammar) for a popup with the issue's explanation, its correction suggestions, "Add to dictionary" (spelling only), and "Check whole document". (Need a build compiled `--features lint`.)
/ #cmd("SPELLNEXT") / #cmd("SPELLPREV"): jump the caret to (and select) the next / previous SPELLING issue, scanning beyond the visible text as needed (bounded; reports if none is found nearby).
/ #cmd("GRAMMARNEXT") / #cmd("GRAMMARPREV"): the same for GRAMMAR / word-choice issues.
/ #cmd("LINTFIX"): correct the language issue at the caret. With one suggestion it applies it directly; with several it opens a chooser (the PICK dialog) to pick the correction. (Put the caret on an underlined word — e.g. after #cmd("SPELLNEXT").)
/ #cmd("DIALECT"): `DIALECT <name>` — set the English dialect the checker uses: `american` (default), `british`, `canadian`, `australian`, or `indian`. #sv("DIALECT") reads it. Persisted by `SaveCfg`.
/ #cmd("SPELLENGINE") / #cmd("SPELLDICT") / #cmd("SPELLBIN"): pick and configure the SPELLING checker. `SPELLENGINE harper` (default, the built-in English grammar+spelling) or `SPELLENGINE hunspell` (shell out to the Hunspell CLI for multilingual spelling). For Hunspell, `SPELLDICT <names>` sets the dictionaries passed to `-d` (e.g. `SPELLDICT en_US,de_DE`) and `SPELLBIN <path>` sets the executable (a name on `PATH`, the default `hunspell`, or a full path); the `SP on` underlines, the whole-document `CHECK` report, and the right-click corrections / `LINTFIX` (via `hunspell -a`) then all come from Hunspell. Like Harper, only PROSE is checked — fenced and inline code and URLs are skipped. Hunspell mode is SPELLING-ONLY (it is a speller, not a grammar checker), so `GR` grammar underlines are inactive — use `SPELLENGINE harper` when you want the built-in English grammar. `LEARN`/"add to dictionary" works in either mode (the personal dictionary filters both). If the binary or dictionaries are missing, spelling simply produces no output. #sv("SPELLENGINE") / #sv("SPELLDICT") / #sv("SPELLBIN") read the three settings; all persist in `SaveCfg`. *Experimental.*
/ #cmd("LEARN") / #cmd("UNLEARN"): `LEARN <word>` adds a word to your personal dictionary so the checker stops flagging it (and #cmd("UNLEARN") removes it). Also takes a LIST: several words on the line (`LEARN alpha beta gamma`), every word in the current selection (`LEARN/S`, one per line or run of text), or the words listed in a file (`LEARN/F <path>`) — each split on whitespace and stripped of clinging punctuation, added in one pass. #cmd("UNLEARN") accepts the same forms. Saved to `xyscript-dict.txt` beside the program and reloaded at startup.
/ #cmd("CH", "CHA", "CI", "CIA"): replace every match from the cursor onward; the caret stays put and the number of changes is reported on the status line (`A` = case-sensitive; `CI`/`CIA` are synonyms) (`/W /T /A /R /S /K`; `/T` changes from the top of the document instead). `/K` (keep case) recases each replacement to match the case of the text it replaces — `CH/K /alligator/crocodile/` maps `alligator`→`crocodile`, `Alligator`→`Crocodile`, and `ALLIGATOR`→`CROCODILE` (a mixed-case match takes the replacement verbatim; the same rule as `AR` auto-replace). `/n` (a number, e.g. `CH/5`) makes *at most* `n` changes — and, unlike a plain `CH`, fewer than `n` (even zero) is not an error.
/ #cmd("CHARCOUNT"): count the characters in the selection or document.
/ #cmd("CLEAR"): unset one or more variables.
/ #cmd("CLEARATTACH"): empty the #cmd("ATTACH") staging list (the files queued for the next #cmd("SENDMAIL")).
/ #cmd("CLEARKBD"): reset the keyboard map to the built-in baseline.
/ #cmd("CLERR"): clear the `$ERROR` flag (and #cmd("SETERR") sets it).
/ #cmd("CLIP"): set the clipboard from a string argument.
/ #cmd("CMH"): stash a command at the top of the history without running it — bare uses the command line, `CMH "command"` uses that string.
/ #cmd("CMBLANK"): whether the command line is blanked after a command runs from it — `CMBLANK on|off` (bare toggles). Off (the default) keeps the command on the line for re-running or editing (XyWrite-style); `on` clears it after each command (via Enter or `XC`). Read back via `$CMBLANK`; persists in the config.
/ #cmd("SHOWONETAB"): whether the tab strip shows when only one file is open — `SHOWONETAB on|off` (bare toggles). Off (the default) hides the strip for a lone document to keep it uncluttered; `on` always shows it. Read back via `$SHOWONETAB`; persists in the config.
/ #cmd("CARETMOTION"): the order Left/Right move the caret — `CARETMOTION logical|visual`. `logical` (the default) moves the caret in storage order — Right is always the next character in the text (identical to visual for left-to-right text; the two only differ in bidirectional text). `visual` moves it in screen order — Right always steps to the character on the right of the caret and Left to the one on the left, stepping through a right-to-left run (numbers included) in reverse and keeping the caret on the side it stepped to at a direction seam. Read back via #sv("CARETMOTION") (`logical`/`visual`); session-only (not saved to the config). The `visual` mode is a prototype: per-glyph within a line, and at a line's visual edge Left/Right cross to the adjacent line in reading order (the next line's start, or the previous line's end); #cmd("EL")/#cmd("ER") (Home/End) are not yet direction-aware.
/ #cmd("CARETADD") / #cmd("CARETUP") / #cmd("CARETDOWN") / #cmd("CARETCOL") / #cmd("CARETONE"): multi-caret editing. #cmd("CARETADD") with no argument plants a caret at the current caret and enters a *building* state where plain motion moves only the primary caret — so you can roam to the next spot and plant again — until the first edit ends it; `CARETADD <n>` adds (or toggles off) a caret at byte offset `n`. #cmd("CARETUP") / #cmd("CARETDOWN") add a caret one line above / below each existing caret (a column of carets). #cmd("CARETCOL") turns a linear selection into a column of carets: one bare caret on every line the selection touches, all at the cursor's column, with a short line's caret clamped to its end and the boundary carets clamped inside the selection; the selection is collapsed into the caret set. A bare #cmd("CARETADD") with a block defined does #cmd("CARETCOL"). #cmd("CARETALL") puts a selection-caret at EVERY match: bare, it matches the current selection (or the word under the caret); `CARETALL <pattern>` matches a pattern; switches mirror the FIND family — `/R` regex, `/W` whole word, `/A` case-absolute, `/S` confine the search to the current selection. It is capped at 2000 carets and reports the count (and the cap when hit) on the status line; no matches leaves the set unchanged. #cmd("CARETNEXT") adds occurrences ONE AT A TIME (like the "select next occurrence" of other editors): with nothing selected it first selects the word under the caret; each further press finds the next occurrence of the selection (or `CARETNEXT <text>`) after the last caret, adds a selection-caret there, and makes it primary, wrapping at end-of-document. Switches `/R` `/W` `/A` mirror the FIND family. #cmd("CARETSKIP") skips an unwanted occurrence during a #cmd("CARETNEXT") run: it MOVES the primary selection to the next occurrence (wrapping at end-of-document) without adding a caret, so the skipped match is left alone; same `/R` `/W` `/A` flags. #cmd("CARETSPLIT") replaces a multi-line selection with one BARE caret at each spanned line's end ("split selection into lines"); the last partly-selected line clamps to the selection end, and a single-line selection is left unchanged. #cmd("CARETPOP") undoes the most recent caret-set change (removes the last-added caret / steps back through the caret ring); any document edit clears the ring. #cmd("CARETPRIMARY") `<n>` makes the caret at document-order index `n` (0-based, as #sv("MCP") indexes) the primary, so the viewport and the classic specials track it. #cmd("CARETONE") collapses back to a single caret. With more than one caret, every editing and motion verb acts on them ALL at once — typing, deleting, moving — as one undo step. Read the set with #sv("CARETS") (count) / #sv("CARETP") (the primary's index) / #sv("MCP") (the array of offsets). On the mouse, Ctrl+click adds a caret, a plain click collapses, and Alt+drag makes a box (column) selection — a caret per line across the dragged rectangle (a line too short for the left column gets a bare caret at its end).
/ #cmd("WRAP"): whether long lines word-wrap to the window — `WRAP on|off` (bare toggles). On (the default) wraps each line to the text width; `off` lays every line on one row that runs past the right edge, and a horizontal scrollbar appears along the bottom to reach the overflow (the caret stays in view as you type or move, and Shift+wheel pans sideways). Read back via `$WRAP`; persists in the config.
/ #cmd("WRAPHANG"): hanging indent for wrapped continuation rows — `WRAPHANG <n>` sets `n` spaces (bare reports the current value; `0`, the default, is a flush wrap). The FIRST row of each line sits at the margin and its wrapped rows hang `n` spaces under it. Applies only when #cmd("WRAP") is on; it is ignored when #cmd("WRAPINDENT") is on (that mode hangs each line under its OWN indentation instead). Read back via `$WRAPHANG`; persists in the config.
/ #cmd("ULSPACES"): whether an `MDA` #underline[underline] paints through the spaces between words — `ULSPACES on|off` (bare toggles). `on` (the default) draws one continuous line, matching how a selection underlines; `off` breaks the line at spaces and tabs, so only word glyphs are underlined. Applies to both selection styling and the `MDA UL`/`BU` typing mode — and governs the underline in the `PV` preview and `TOPDF` output the same way, so print matches the screen. Read back via `$ULSPACES`; persists in the config.
/ #cmd("UNTITLEDTAB"): whether the blank *Untitled* scratch tab is closed when you open a document — `UNTITLEDTAB on|off` (bare toggles). On (the default) closes it and prunes any leftover empty untitled tab, so a scratch never lingers beside real files; the Untitled tab reappears when the last document is closed. `off` keeps the Untitled tab as its own tab. Read back via `$UNTITLEDTAB`; persists in the config.
/ #cmd("GOTOOPEN"): whether opening a file that is already open switches to its tab instead of asking — `GOTOOPEN on|off` (bare toggles). Off (the default) shows the "already open" prompt (*(Y)es (N)o (G)o (R)ead*); `on` skips the prompt and goes straight to the existing tab (the prompt's *(G)o*). Affects `OPEN` only — `READ` always opens a fresh read-only view. Read back via `$GOTOOPEN`; persists in the config.
/ #cmd("CMLINE"): move focus to the command line (synonym #cmd("GH")).
/ #cmd("CMPLACE"): where the command line is drawn — `CMPLACE top|bottom|hidden` (bare reports it). `hidden` keeps it out of the way until you focus it. Read back via `$CMPLACE`; persists in the config.
/ #cmd("CONTINUE"): skip to the next iteration of the loop.
/ #cmd("COPY"): copy the selection to the clipboard; `COPY <src> <dst>` copies a file. With a multi-caret set it stores every caret's selection as an ordered list (and puts the newline-joined text on the OS clipboard), so a later #cmd("PASTE") into the same number of carets distributes the pieces.
/ #cmd("CP"): copy the selection to the caret. With no selection in the current tab, it instead offers the selections in other open tabs in a chooser and inserts the one you pick at the caret.
/ #cmd("CRCOLOR", "CRCOLORS"): the caret color `R G B` (`/HSL`; `…S` opens a picker).
/ #cmd("CMCRCOLOR", "CMCRCOLORS"): the command-line caret color `R G B` (`/HSL`; `…S` opens a picker) — the CMline equivalent of #cmd("CRCOLOR"). Defaults to (and is re-set by #cmd("THEME") to) the text-caret color; set it to make the two differ.
/ #cmd("CRSHAPE"): the caret shape — `bar`, `block`, or `underline`.
/ #cmd("CMCRSHAPE"): the command-line caret shape — `bar`, `block`, or `underline` (the CMline equivalent of #cmd("CRSHAPE")).
/ #cmd("CUT"): cut the selection to the clipboard.
/ #cmd("CV", "CVA"): change-with-verify — prompt at each match (`A` = case-sensitive) (`/W /T /A /R`). `/n` (e.g. `CV/3`) stops the walk after `n` changes are made (a skipped match doesn't count).

== D

/ #cmd("DA"): insert the current date and time.
/ #cmd("DB") / #cmd("DE"): move the cursor to the beginning / end of the selection (`$ERROR` if nothing is selected).
/ #cmd("DECIMALS"): the decimal places floats display with (`off` = full precision).
/ #cmd("DEL"): delete a file or empty folder from disk, with confirmation; #cmd("ERNV") (or `DEL/NV`) skips the prompt.
/ #cmd("DELMAIL"): delete messages from the IMAP mailbox by number — `DELMAIL <range>`, where `<range>` is a comma list of numbers and `first-last` spans (`DELMAIL 1-3,6-8,10`) or the word `all`. It flags them `\Deleted` and expunges. IMAP sequence numbers renumber after an expunge, so re-run #cmd("LISTMAIL") between deletes (or delete highest-first). Needs #cmd("IMAPURL") and #cmd("MAILUSER")/#cmd("MAILPASS"). Runs as a background curl job — #sv("RC") (curl's exit code; `0` = deleted) and #sv("XO") are set when it finishes, so `WAIT` first, then read #sv("RC") (reading it immediately gets the *previous* command's code). `DELMAIL/W` runs *synchronously* instead — it blocks until curl finishes, so #sv("RC") is already set when it returns. A command the server *refuses* (a tagged IMAP `NO`/`BAD`) sets #sv("RC") to `100`, even though curl connected and exited `0` — so a rejected delete is never mistaken for success.
/ #cmd("DELSEL", "DN"): delete the current selection.
/ #cmd("DF"): start a defined-text selection — or close it if text is already selected. #cmd("DFA"): select the whole document. #cmd("DM"): extend the defined selection to the caret.
/ #cmd("DZ"): close the current selection; a no-op if nothing is selected.
/ #cmd("DW", "DW+") / #cmd("DS", "DS+") / #cmd("DP", "DP+"): define the word / sentence / paragraph at the caret (`+` also takes the trailing break). #cmd("DL"): define the whole line.
/ #cmd("DIR", "LS"): open a directory-listing browser — `/W` in a separate window, `/H` / `/V` in a new split pane (stacked / side-by-side) (`/B` bare, `/O…` sort, `/A…` attributes, `/S` recurse).
/ #cmd("DIRNW"): `DIRNW on|off` — whether opening a file from an in-TAB listing lands in a new tab (`on`) or replaces the listing in place (`off`, the default). For the separate `DIR/W` window see #cmd("DIRWNW").
/ #cmd("DIRWNW"): `DIRWNW on|off` — the same choice for the separate `DIR/W` WINDOW: `on` (the default) opens the picked file in a new tab, `off` reuses the active tab. It is a separate setting because the two are different interactions — `DIR/W` floats beside your work, so it has always added a tab. With `off`, a MODIFIED buffer is never discarded: the file opens beside it instead. Whether the window itself then closes is the separate #cmd("DIRWCLOSE").
/ #cmd("DIRWCLOSE"): `DIRWCLOSE on|off` — whether the `DIR/W` window CLOSES once you open a file from it (`on`), or stays open so you can carry on picking (`off`, the default — how `DIR/W` has always behaved). Independent of #cmd("DIRWNW"), which chooses the TAB the file lands in; the two answer different questions. Descending into a subdirectory never closes the window — only opening a file does.
/ #cmd("DIRSET"): `DIRSET <switches>` — the default sort/filter/view switches applied to a `DIR`/`LS` that doesn't override them, e.g. `DIRSET /OD/R` to sort every listing by date, reversed. A command switch overrides the default with the SAME leading letter: `DIR /ON` replaces the `/OD` order; `DIR /AH` adds a filter and leaves the order default in place (for `/A`, any command `/A…` replaces the whole `/A` default). Because `/R` (reverse) has no "off" form, a command `/O` order ALSO cancels a default `/R` — otherwise `DIRSET /OD/R` would force reverse onto every listing with no way back to ascending; type `/R` yourself (`DIR /ON/R`) to reverse explicitly. Only the how-it-looks switches are allowed — `/O` (order), `/A` (attributes/filter), `/R` (reverse), `/S` (recurse), `/B` (bare); the where-it-opens switches (`/W`, `/H`, `/V`) are rejected, but the defaults still apply to a `DIR/W` or split-pane listing. `DIRSET off` (or `none`/`clear`) clears it; bare `DIRSET` reports the current value. Read back via #sv("DIRSET"); persists in the config.
/ #cmd("DIRRT"): `DIRRT on|off` — whether a `DIR` reuses an already-open listing tab (`on`) instead of spawning a new one each time (`off`, the default).
/ #cmd("DOCTYPE"): how the document renders to PDF — `typst`, `gfm`, `html`, or `auto`; bare reports the effective kind. Under `auto` the extension decides: `.typ` is typst, `.html`/`.htm`/`.xhtml` are HTML, everything else is GFM. HTML is converted *structurally* — headings, lists, tables, links, and code become their typst equivalents, while `class`, `style`, and stylesheets are ignored, so a page laid out with CSS renders as readable content rather than as a copy of the page.
/ #cmd("DWFGCOLOR", "DWFGCOLORS") / #cmd("DWBGCOLOR", "DWBGCOLORS"): the `DIR/W` window's text / background color (`…S` opens a picker; unset inherits the editor).
/ #cmd("DWFAMILY") / #cmd("DWSTYLE") / #cmd("DWFONT", "DWFONTS") / #cmd("DWSCALE"): the `DIR/W` window's font family / style / size / zoom. #cmd("DWRESET"): clear all `DW…` overrides.

== E

/ #cmd("END", "ER"): move to the end of the line (`ER` is the "Express Right" name).
/ #cmd("EDFAMILY") / #cmd("EDSTYLE") / #cmd("EDFONT", "EDFONTS"): the editor font's family / style / size (`EDFONT +n` adjusts; `…S` opens a picker).
/ #cmd("EMPH"): `EMPH <word|phrase>` — highlight every whole-word occurrence of the word or phrase in the current tab (case-insensitive; a match inside a longer word is skipped), a persistent visual overlay that works even in read-only and `*Console*` tabs. `EMPH/R <regex>` matches a regular expression instead (an invalid pattern is reported and leaves the emphasis unchanged). Bare `EMPH` clears it. Per-tab and session-only (each tab keeps its own emphasis).
/ #cmd("EMPHFGCOLOR", "EMPHFGCOLORS") / #cmd("EMPHBGCOLOR", "EMPHBGCOLORS"): the `EMPH` emphasis text / background color — `EMPHFGCOLOR <r g b>` (`/HSL`; `…S` opens a picker); `EMPHFGCOLOR default` clears it so the emphasis follows the selection colors (`SELFGCOLOR`/`SELBGCOLOR`). Read back via #sv("EMPHFGCOLOR") / #sv("EMPHBGCOLOR"); persist in the config.
/ #cmd("ENCODING"): set the file's text encoding view (re-decodes the same bytes). `ENCODING/C <name> [outfile]` *converts* instead — re-encodes the text to `<name>` (lossy → `?`), in place or to a new file.
/ #cmd("ENTER"): insert a newline in the document's line-ending style (bind it to the Enter key).
/ #cmd("ET"): insert the stopwatch's elapsed time.
/ #cmd("ELSE") / #cmd("ELSEIF") / #cmd("ENDIF", "EI"): the other parts of an `IF` block (see `IF`).
/ #cmd("ENDWHILE", "EW") / #cmd("ENDFOR", "EF") / #cmd("ENDSUB", "ES") / #cmd("ENDSWITCH", "ESW"): close a `WHILE` / `FOR` / `SUB` / `SWITCH`.
/ #cmd("EXIT", "EX") / #cmd("EXITALL", "EX1"): halt the current script / halt every running script.

== F

/ #cmd("FGCOLOR", "FGCOLORS"): the editor text color `R G B` (`/HSL`; `…S` opens a picker). *With text selected*, it instead colors just the selection — a span that follows edits (typing before it shifts it along, and undo/redo re-track it) and, like #cmd("MDA"), #emph[persists] with the file (both write the same foreground layer, saved to the doc's hidden `.xymeta` sidecar). `FGCOLOR default` clears the selected text's color — selection-only (a no-op, with a note, when nothing is selected). `$FGCOLOR` always reads the global color. To colour a block's #emph[background], use #cmd("MDA")`/B`.
/ #cmd("FIND"): open the Find/Replace dialog (a window over the editor); the actual searching is `SE`/`CH`. Type-to-find is live — each letter jumps to and centres the next match ahead of the caret, and Backspace jumps back to the first match of the shorter text (no Enter/F3 needed).
/ #cmd("FS"): release the focused split pane to FULL SCREEN — collapse the whole split layout down to just the current pane (the other panes' buffers stay open as tabs). Unlike #cmd("TUS"), which removes the focused pane and keeps its neighbor, `FS` keeps the pane you're in. (Distinct from #cmd("TZ"), a temporary toggle-zoom that preserves the split.)
/ #cmd("FOR", "ENDFOR", "EF"): a loop — counting (`FOR i = a TO b [STEP s]`, `/X` excludes the end) or over a collection (`FOR v IN <expr>`, or `FOR k, v IN <expr>` for index/key + value); ends at `ENDFOR`/`EF`.

== G

/ #cmd("GOFILE", "GOF"): switch to an open tab whose full path contains the given text (case-insensitive) — `GOF report`. With several matches (or several copies of one file), each call moves to the *next* match, cycling; if the only match is the current tab it stays put. No match sets `$ERROR`.
/ #cmd("GOTO"): jump to a `:label` in the script.
/ #cmd("GREP"): `GREP <pattern> [path]` — ripgrep-style recursive search (respecting `.gitignore`, skipping hidden and binary files) into a read-only results tab, one `path:line:col: text` line per match. The search runs on a background thread and *streams* matches into the tab as it goes (so it never freezes on a big tree, and the status line shows a running count); running another `GREP` (or closing the tab) cancels an in-progress one. The optional second argument is a search root, resolved like a `DIR` filespec: a directory searches it (`GREP foo src`), a glob filters `$CWD` (`GREP foo *.rs`), and both together do both (`GREP foo src/*.rs`); with no path it searches `$CWD`. `/R` treats the pattern as a regular expression (else literal), `/I` case-insensitive, `/W` whole-word — the same flag letters as `SE`/`FIND`. Quote a pattern that contains spaces (`GREP "foo bar" src`). Matches are highlighted (via #cmd("EMPH")) in the results tab, and `GJ` carries that highlight onto the file it opens. Results are capped (currently 50000 hits). A *bare* `GREP` (no pattern) opens the GREP dialog (also `Search ▸ Grep…`) — an interactive form with the pattern, an optional folder/glob, and the `/R` / `/I` / `/W` flags as checkboxes; the companion to #cmd("FIND").
/ #cmd("GJ"): jump from a `GREP` results tab to the match on the caret's line — opens the file (or switches to it if already open, no prompt), moves the caret to the match's `line:col`, and centres it. Point-and-shoot: put the caret on a result line and run `GJ`, or just press Enter or double-click the line (arrow keys still scroll the results).
/ #cmd("GT"): move focus to the document text (the counterpart to `CMLINE`/`GH`).

== H

/ #cmd("HOME", "EL"): move to the start of the line (`EL` is the "Express Left" name).
/ #cmd("HM"): move to the top of what's currently on screen.
/ #cmd("HELP", "?"): look up reference help for a command, special, or function — `HELP CV`, `HELP $CWD`, `HELP UPPER` — shown in a scrollable box. A bare `HELP` gives a short overview; an unrecognized name suggests near matches. Same text as this reference, so it never drifts.
/ #cmd("HIGHLIGHT"): turn syntax highlighting on or off for the active file's type.
/ #cmd("HEX"): `HEX on|off` (bare `HEX` = on) shows — and edits — the active document as a hex + text grid: an offset gutter, 16 bytes per row as hex pairs, and a text column (the bytes through the current encoding, non-printable as `.`). A per-document canvas view mode for binary files. *Navigate* by byte: ←/→ move ±1, ↑/↓ move one row (16 bytes), Home/End jump to the row edges, PgUp/PgDn a screen, a selection spans a byte range, and a click lands on the exact byte in whichever column you click. *Edit* in place (OVERWRITE by default, keeping file size and offsets stable): in the hex column type `0`–`9`/`a`–`f` — two digits compose a byte; `Tab` switches to the text column, where a typed character sets that byte through the active encoding. `Insert` adds a `00` byte, `Delete`/`Backspace` remove the byte at / before the caret (or the selection), and copy/cut/paste (byte-faithful) plus undo/redo all work; a genuine `RO` lock blocks editing. #sv("HEX") reads it.
/ #cmd("HLAUTO"): auto-enable highlighting whenever a recognized file is opened (`on`/`off`).
/ #cmd("HLLANG"): declare a regex-grammar highlighter — `HLLANG <name> <.ext,…>` — then add rules with #cmd("HLRULE") (`HLRULE <kind> <regex>`, first match wins). Ready-made grammars ship in the `grammars/` folder beside the program (Rust, Python, JavaScript, HTML, shell, TOML, Typst, XyWrite, and `xpl.xpl` for XyScript's own XPL — `.xpl`/`.cfg`/`.lib`, coloring its control-flow keywords and every built-in `function`); `RUN` the one you want, then highlighting applies to that file type.
/ #cmd("HLCOLOR", "HLCOLORS"): a token kind's highlight color `R G B` (`…S` opens a picker; `HLCOLOR <kind> default` clears it).
/ #cmd("HLKINDS"): list the highlight token kinds and their colors. #cmd("HLRESET"): restore the default highlight theme.
/ #cmd("HISTORY", "HI"): open the command-history picker. Enter runs the highlighted entry and moves it to the top; Shift+Enter pulls it back to the command line to edit without running; Del removes it.

== I

/ #cmd("IF", "IFNOT"): run a block when a condition is true. `IFNOT` (or the two-word `IF NOT …`) runs it when the condition is false. Pair with `ELSE`/`ELSEIF` and close at `ENDIF`/`EI`.
/ #cmd("IMAPURL"): the IMAP server URL for receiving mail — `IMAPURL imaps://host:993` (bare reports it, `$IMAPURL` reads it). Used by #cmd("LISTMAIL")/#cmd("SAVEMAIL")/#cmd("DELMAIL") together with #cmd("MAILBOX"); authentication reuses #cmd("MAILUSER")/#cmd("MAILPASS"). Session-only (put it in `startup.xpl`).
/ #cmd("INPUT"): pause for a typed line into a variable — `INPUT <var> [prompt]`.
/ #cmd("INSERT", "WRITE", "WR"): insert literal text at the caret (a quoted string unescapes `\n`, etc.).

== J

/ #cmd("JMP"): move the caret to a character offset — absolute (`JMP 100`) or relative (`JMP +n` / `JMP -n`); add `SEL` to select on the way.
/ #cmd("JOBS"): list the running background jobs. #cmd("KILL"): end one by id.

== K

/ #cmd("KBCHART"): show the loaded keymap's chart in a scrollable text box — one section per modifier table (Normal / SHIFT / CTRL / …) listing each table's `scan → command` bindings. Under `CAPTURE`, the text goes to the variable instead.
/ #cmd("KBDDOC"): annotate a `.kbd` file in place with a key-NAME column — each table row gains a `<layer> <keyname>  :` prefix (from #func("GETKEY")), so `1=KF` reads `unmod Esc  :1=KF` and you can see at a glance which physical key each scan code is. `KBDDOC` (no argument) annotates the KBD file in the current tab; `KBDDOC <file>` opens it (blind-editing and saving it if it isn't already open). It refuses a non-KBD file, and is a no-op on an already-annotated one. #cmd("LOADKBD") ignores the annotations, so an annotated file still loads.

== L

/ #cmd("LU", "UP") / #cmd("LD", "DOWN"): move one line up / down, keeping the column where possible.
/ #cmd("LL") / #cmd("LR"): move one character left / right but never crossing to another line.
/ #cmd("LB") / #cmd("LE"): move to the start / end of the visual (wrapped) row.
/ #cmd("LC") / #cmd("UC"): lower-case / upper-case the selection — or, with no selection, the character under the caret, then step one character right (so you can sweep along the text).
/ #cmd("LEADING", "LSP"): set the line spacing (percent).
/ #cmd("LOADAR"): load `key=replacement` pairs into the auto-replace map (used by `AR`); a `replacement` starting with `#` is run as XPL (e.g. `pd=#DA;PW`). #cmd("SAVEAR") writes it back, #cmd("CLEARAR") empties it.
/ #cmd("LOADCFG"): run a saved config script. #cmd("LOADCM"): load a saved command-history list. #cmd("LOADCLOSED"): load a saved reopen list (recently-closed files). #cmd("LOADKBD"): load a keyboard map (a `.kbd` file annotated by #cmd("KBDDOC") loads fine — the key-name column is ignored). #cmd("LOADLIB"): load a `.lib` library (replacing the current one).
/ #cmd("LDTX"): load a text binder slot (`@slot`) — `LDTX <A-Z|0-9> [<source>]`, where the source is the current selection (no argument), a quoted literal (`LDTX A "some text"`, escapes processed, `$`/`@`/`&` kept verbatim), or a file path. #cmd("LDSC") / #cmd("LDPM") load a script binder slot (`&slot`) the same way. Save all text registers with #cmd("SAVELDTX").
/ #cmd("LISTLIB"): list the loaded library routines (name and description, in a two-column scrollable dialog; or into a `CAPTURE` variable). #cmd("LISTSPECIALS"): insert a `$NAME: |$NAME|` line for every special variable (a `PRINTEX`-able template).
/ #cmd("LISTMAIL"): list the IMAP mailbox — fetch each message's Date/From/Subject into a numbered listing in the *Console* tab, so you can see what's there and pick numbers for #cmd("SAVEMAIL")/#cmd("DELMAIL"). Message numbers are IMAP sequence numbers (1 = oldest). Needs #cmd("IMAPURL") and #cmd("MAILUSER")/#cmd("MAILPASS") (and #cmd("MAILBOX"), default `INBOX`). With #cmd("MAILDEBUG") on it streams the raw server exchange instead of the parsed listing. Runs as a background curl job; #sv("RC") (curl's exit code) and #sv("XO") are set on completion, so `WAIT` before reading them, or use `LISTMAIL/W` to run synchronously (blocks until done, so #sv("RC") is set on return).
/ #cmd("SAVEVARS"): save every global variable and its value to a file (`SaveVars [path]`; bare → `xyscript-vars.txt`, resolved against `$CWD`). The file is `name⇥value-literal` lines — a data file, not a script. Under `CAPTURE` the text goes into the variable instead. #cmd("LOADVARS"): restore variables written by `SaveVars` (`LoadVars [path]`); each value is evaluated directly, so a `$` in a saved string stays literal. A missing file or a bad line sets `$ERROR`.
/ #cmd("LISTVARS"): list every global variable and its current value (`$name: value`, sorted) — into a `CAPTURE` variable if one is open, else inserted at the caret. #cmd("LISTVAR") is a synonym.
/ #cmd("LOCAL"): scope variables to the current routine.
/ #cmd("LANG"): `LANG <file>` — load a UI translation table so the interface shows in another language. The file is a flat UTF-8 `key = value` catalog of translated menu/dialog labels (`#`-comments and blank values allowed; a blank or missing key falls back to the built-in English, so a partial catalog works). A two-letter argument is shorthand for a `.lang` file: `LANG fi` loads `fi.lang` (resolved against `$CWD`) — the convenient way to keep catalogs named by language code; give a name with an extension or a path (`LANG fr.txt`, `LANG i18n\de.lang`) to load a specific file. Bare `LANG` or `LANG off` reverts to English. Point-and-shoot: with the caret on a catalog file in a `DIR` listing, bare `LANG` loads that file. Path resolved against `$CWD`, read back as #sv("LANG"), and persisted by `SaveCfg` (a `LANG <path>` line) so the choice restores on load. *Overrides:* loading `<xx>.lang` also layers a sibling `<xx>.local.lang` (next to it, or in `$CWD`) on top if present — a file of just your changed lines that wins over the catalog and survives upgrades (it is never shipped). An override whose trailing `# English` anchor no longer matches the catalog is skipped, so a stale override falls back to the updated built-in translation. Experimental (phase-1: the menu bar and the File and Search menus are wired so far).

== M

/ #cmd("MAILUSER"): the mail account username for both SMTP and IMAP (usually your full email address) — `MAILUSER <name>` (bare reports it, `$MAILUSER` reads it). Session-only.
/ #cmd("MAILPASS"): the mail account password / app-password (SMTP send and IMAP receive) — `MAILPASS <secret>`. Never persisted and never echoed: `$MAILPASS` reads `set` / `not set`. If unset, the `XYSCRIPT_MAILPASS` environment variable is used. Fed to curl through a config on stdin, so it never appears on the command line.
/ #cmd("MAILFROM"): the envelope / `From:` address for #cmd("SENDMAIL") — `MAILFROM <address>` (defaults to #cmd("MAILUSER"); bare reports it, `$MAILFROM` reads it). A display-name form (`MAILFROM "Jane Roe" <jane@x.org>`) is fine — the envelope uses the bare address inside `<…>`, which most servers require to equal the authenticated account. Session-only.
/ #cmd("MAILBOX"): the IMAP mailbox (folder) #cmd("LISTMAIL")/#cmd("SAVEMAIL")/#cmd("DELMAIL") operate on — `MAILBOX <name>` (default `INBOX`; bare reports it, `$MAILBOX` reads it). Session-only.
/ #cmd("MAILDEBUG"): `MAILDEBUG on|off` (bare toggles) — run curl verbosely so the full SMTP / IMAP dialogue streams to the *Console* tab, for diagnosing a send or receive problem. `$MAILDEBUG` reads the state.
/ #cmd("MD") / #cmd("MU"): scroll the view down / up one line without moving the caret.
/ #cmd("MDA"): #emph[mode attribute] — style the *selection* as a persistent text block (XyWrite "mode", reborn as a sidecar). Three independent layers, all edit-following and saved to the doc's hidden `.xymeta` sidecar (so they survive close-and-reopen):
  - #emph[Foreground colour] — `MDA <r g b>` (`/HSL`; a colour name also works); `MDA off` / bare `MDA` clears it.
  - #emph[Background colour] — `MDA/B <r g b>`; `MDA/B off` clears it.
  - #emph[Text attributes] — `MDA BO` (bold), `MDA IT` (italic), `MDA BI` (bold-italic), `MDA UL` (underlined), `MDA BU` (bold-underlined), `MDA RV` (reverse video). Each SETS the region's attributes (replace, not additive); `MDA NM` (normal) strips them. Bold/italic render as the glyph face; underline is drawn in the text colour; reverse paints the text in the current `BGCOLOR` over a `FGCOLOR` background (dynamic — it stays inverted across a theme change).
  With #emph[no selection], `MDA` becomes a #emph[typing mode] (XyWrite's modal "MD"): the run above turns the mode on so subsequently typed text is styled with it, and the three layers stack (e.g. `MDA BO` then `MDA 0 0 255` types bold blue). A bare `MDA` cancels the foreground-colour and attribute modes; `MDA/B` cancels the background mode; `MDA NM` cancels just the attribute mode. Typed styling persists like any other. Unlike #cmd("FGCOLOR")-on-selection (which shares the foreground layer, so it also persists), the attribute and background layers are `MDA`-only.

  #emph[In print.] `MDA` styling isn't only on-screen: a #emph[Markdown/GFM] document carries it into the #cmd("PV") preview and #cmd("TOPDF") export — every layer (bold / italic / underline / reverse / foreground / background) is woven into the generated Typst so it renders in the PDF, even the attributes Markdown itself can't express. It applies to prose text; styling that falls inside Markdown syntax (a code span, a link's target) is left to render plainly. #emph[Authored-Typst] documents (#cmd("DOCTYPE") `typst`) style themselves in Typst markup, so `MDA` there stays a screen-only overlay.
/ #cmd("MDAS"): open a colour #underline[picker] for the selection's #cmd("MDA") block colour — bare picks the foreground, `MDAS/B` the background. On accept it runs `MDA`/`MDA/B <r g b>`. In the GUI: #emph[Settings ▸ Appearance ▸ Colors ▸ Block colour].
/ #cmd("MARKSET") / #cmd("MARKCLEAR"): set / clear a bookmark at the caret (`MARKCLEAR` where there is none is a trappable error). #cmd("MARKTOGGLE"): set one if absent, clear it if present. #cmd("MARKCLEARALL"): remove every bookmark in the document.
/ #cmd("MARKNEXT") / #cmd("MARKPREV"): move the caret to the next / previous bookmark, wrapping at the ends, and scroll it into view. A bookmark whose text has moved (the file changed since it was saved) is relocated to the nearby text, noted in the status line. Bookmarks are per-document, persist in the hidden `.xymeta` sidecar, and show as a dot in the left gutter. See #sv("MARKS") / #sv("MARK").
/ #cmd("MARKLIST"): open a modal chooser of the document's bookmarks — each row is the line number and its text — and jump the caret to the one you pick (centered). Bind it to a key for a quick "go to bookmark" menu. A no-op note when there are none.
/ #cmd("MARKGUTTER"): whether bookmarks show a dot in the left gutter — `MARKGUTTER on|off`, bare toggles. On by default; a pure display switch (bookmarks still work when off). Persisted by `SAVECFG`; read back with #sv("MARKGUTTER").
/ #cmd("MENU"): keyboard control of the menu bar. Bare `MENU` toggles it — opens the first menu for arrow-key navigation (Left/Right between menus, Up/Down through items, Enter to open a submenu or run an item, Esc to back out) or closes it if open. `MENU "f"` opens a specific menu by its #underline[accelerator] letter (the underlined letter in each label; e.g. `f` = File). While a menu is open, typing an item's accelerator letter jumps to and runs it. The bar takes mouse and keyboard interchangeably — a menu opened with the mouse is immediately arrow-navigable, and vice versa. Bind it to a key (e.g. `nn=MENU` or `nn=MENU "f"`) for mouse-free menus. GUI only.
/ #cmd("MERGE", "ME"): insert a file's contents at the caret.
/ #cmd("MSG"): show a message box — a bare `MSG` is modal and pauses the script; `MSG /T:<seconds>` is a non-blocking toast.
/ #cmd("MESSAGE"): pop an OS-native desktop message box (`MESSAGE [/T:"title"] <text>`). Unlike `MSG` it needs no running editor window, so it works from a background (`RUN/BG`) script — that is what it is for. `/T:"…"` sets the window caption (default is the app name). Fire-and-forget (the box is its own process, so the script continues and the box survives even after the script ends); no `TRUST` needed. Cross-platform (Windows / macOS / Linux).
/ #cmd("MV"): move the selection to the caret, within the document.

== N

/ #cmd("NW") / #cmd("NS") / #cmd("NP"): move to the next word / sentence / paragraph.
/ #cmd("NL"): move to the start of the next line.
/ #cmd("NEW", "NE"): open a new empty buffer; `NEW <file>` starts a buffer named for the file (or opens it if it already exists). `NEW/H` and `NEW/V` instead open the new buffer in a *split pane*: `/H` stacks it top-over-bottom, `/V` sets it side-by-side left-and-right. See the split-pane verbs in section T (#cmd("THS"), #cmd("TVS"), #cmd("TW"), #cmd("TUS"), #cmd("TRV"), #cmd("TZ")).
/ #cmd("NEWLINE"): `NEWLINE lf|crlf` — convert every line ending in the document and adopt that style (`$NEWLINE` reads it).
/ #cmd("NOP"): do nothing (a no-op).

== O

/ #cmd("OF") / #cmd("OFL") / #cmd("OFR"): set both margins / the left margin / the right margin, in columns (`+n`/`-n` adjusts).
/ #cmd("OPEN", "CA", "ED", "EDIT"): open a file (bare = the GUI open dialog). `/H` / `/V` open it in a new split pane (stacked / side-by-side) instead of a new tab; `/NV` reopens without the "already open?" prompt; `/HEX` opens it straight into the #cmd("HEX") view.

== P

/ #cmd("PW") / #cmd("PS") / #cmd("PP"): move to the previous word / sentence / paragraph.
/ #cmd("PL"): move to the start of the previous line.
/ #cmd("PU") / #cmd("PD"): move up / down one screenful (the page scrolls to follow).
/ #cmd("PAPLACE"): where the working-directory (`$CWD`) path is shown — `PAPLACE cmline|status` (bare reports it). `status` (default) shows it on the status line; `cmline` shows it as a prefix on the command line. Read back via `$PAPLACE`; persists in the config.
/ #cmd("PAFGCOLOR", "PAFGCOLORS"): the `$CWD` path text color — `PAFGCOLOR <r g b>` (`/HSL`; `…S` opens a picker); `PAFGCOLOR` / `PAFGCOLOR default` clears it (back to the dimmed UI text). Read back via `$PAFGCOLOR`; persists in the config.
/ #cmd("PABGCOLOR", "PABGCOLORS"): the `$CWD` path background color — `PABGCOLOR <r g b>` (`/HSL`; `…S` opens a picker); `PABGCOLOR` / `PABGCOLOR default` clears it (back to transparent). Read back via `$PABGCOLOR`; persists in the config.
/ #cmd("PANECOLOR", "PANECOLORS"): the border color drawn around the FOCUSED split pane — `PANECOLOR <r g b>` (`/HSL`; `…S` opens a picker). Inactive panes use the separator color (#cmd("SEPCOLOR")). Read back via #sv("PANECOLOR"); re-set by #cmd("THEME"); persists in the config. See the split-pane verbs #cmd("THS")/#cmd("TVS").
/ #cmd("PASTE"): paste the clipboard at the caret. With a multi-caret set: if the clipboard was filled by a multi-caret #cmd("COPY")/#cmd("CUT") whose piece count equals the current caret count, each piece is DISTRIBUTED to its caret in document order; otherwise the whole clipboard is pasted at every caret. An external (OS) clipboard change resets to paste-the-same.
/ #cmd("PATHLEN"): max width (characters) of the displayed `$CWD` path — `PATHLEN n` (bare reports it). A longer path keeps the drive and the trailing directories, eliding the middle with `...`. Read back via `$PATHLEN`; persists in the config.
/ #cmd("PICK", "CHOOSE"): pop a list chooser and put the picked item in a variable — `PICK <var> ["title"] <list-expr>` (the list is an expression yielding an array; `/I` returns the 1-based index instead of the item). Cancelling leaves the variable empty and sets `$ERROR`. Arrow/type-to-find to choose; Enter picks, Esc cancels.
/ #cmd("PR"): print to the status line / script output. #cmd("PUT"): the same, but insert the (substituted) text at the caret instead.
/ #cmd("PREPEND"): add the selection to the start of the clipboard.
/ #cmd("PREVIEW", "PV"): show/hide the live PDF preview pane on the right (no arg toggles; `on`/`off` set). A Markdown/GFM #emph[or authored-Typst] document's #cmd("MDA") styling shows in the preview just as it will in #cmd("TOPDF").
/ #cmd("PVFAMILY") / #cmd("PVSIZE") / #cmd("PVCODE"): the font for *Markdown* rendering — used by both the live Preview pane and the `TOPDF` export. `PVFAMILY <name>` sets the body typeface, `PVSIZE <n>` the body size in points, `PVCODE <name>` the monospace family for `code` spans and fenced blocks. Each takes `default` (or empty) to revert to Typst's built-in. Only installed fonts render (else Typst falls back). Authored-Typst documents style themselves and ignore these. Read back via `$PVFAMILY`/`$PVSIZE`/`$PVCODE`; persists in the config.
/ #cmd("PVFONTS") / #cmd("PVCODES"): open the font picker for the Preview/`TOPDF` *body* font (family + size) or *code* font (monospace family) — GUI sugar over `PVFAMILY`/`PVSIZE` and `PVCODE`.
/ #cmd("PVMARGIN") / #cmd("PVMARGINL") / #cmd("PVMARGINR") / #cmd("PVMARGINT") / #cmd("PVMARGINB"): the page margins for *Markdown* rendering (Preview pane + `TOPDF`). `PVMARGIN <len>` sets all four sides; `PVMARGINL`/`R`/`T`/`B` set the left / right / top / bottom individually; bare reports the current value. `<len>` is a number with an optional unit — `in` (the default when none is given), `cm`, `mm`, or `pt` — so `PVMARGIN 1` = 1 inch, `PVMARGIN 2.5cm`, `PVMARGIN 18pt`. Default is 1 inch all around. Authored-Typst documents set their own page and ignore these. Read back via #sv("PVMARGIN") (the uniform value, or all four sides) / #sv("PVMARGINL") / #sv("PVMARGINR") / #sv("PVMARGINT") / #sv("PVMARGINB"); persists in the config.
/ #cmd("PRINTEX"): "print + expand" — copy the document expanding each `|…|` field; `PRINTEX <file>` writes it, bare opens a new tab. A field is one of three kinds: a *substitution* (`$vars`/`$(…)`/functions); a *counter field* `|Cn …|` that advances and renders a `C1`–`C9` register using the full `Cn` grammar (`|C1 1|` seeds, then each `|C1|` emits 1, 2, 3, …; needs no `TRUST`); or *live code* (statements), which requires `TRUST 1`. Counter registers are snapshotted for the run, so it renumbers from the template's seed and the document's live counters are left untouched.
/ #cmd("QUIET"): suppress status messages for the rest of the script (`off` re-enables); with `/NV` it also suppresses non-fatal error messages.

== Q

/ #cmd("QUIT", "QQ"): quit, prompting to save dirty tabs (`/NV` skips the prompts).

== R

/ #cmd("RC"): delete the character after the caret (forward delete).
/ #cmd("RD"): delete the selection into the deletion buffer; #cmd("UD") pastes it back.
/ #cmd("RE") / #cmd("RL") / #cmd("RLL") / #cmd("RW") / #cmd("RS") / #cmd("RP"): delete from the caret to end-of-line / the current VISUAL (wrapped) line / the whole LOGICAL line / the word / the sentence / the paragraph. `RL` deletes the on-screen row for XyWrite compatibility; with `WRAP off` (or headless) it deletes the logical line, the same as `RLL` ("rubout line logical").
/ #cmd("READ"): open an existing file in a read-only tab (unlock it with `RO`); a missing file is an error. Always opens a fresh copy — no "already open?" prompt — so you can hold several read-only views of one file. `/H` / `/V` open it in a new split pane (stacked / side-by-side) instead of a new tab; `/HEX` opens it in the #cmd("HEX") view.
/ #cmd("READKEY", "RK"): pause for a single keystroke into a variable — `READKEY <var> [message]`. With a `message` (a quoted string; `\n` for line breaks), it pops a modal box showing the message while it waits (handy for a keypress menu: `READKEY k "Pick:\nA\nB\nC"` then `SWITCH UPPER($k)`). A bare modifier press (Shift / Ctrl / Alt / …) is ignored — it waits for a real key. That key's scancode lands in #sv("SC"), and #sv("SCC") holds the *computed* scancode (the scancode plus the bit value of any held modifier keys), so a script can tell a plain key from its Shift / Ctrl / Alt-modified form (e.g. `SWITCH $SCC` or `KEY($SCC)`). With `/D` (`READKEY/D <var>`) the variable instead gets a readable *description* of the key — the held modifiers (named and bit-valued exactly as the loaded KBD file defines them — the built-in default uses `CTRL` / `ALT` / `SHIFT` / `CAPS`, and any user-defined modifier such as `HOTKEY` is recognised) joined with `+` to the key label, e.g. `CTRL+Y`, `SHIFT+F3`, `HOTKEY+A`, `Enter`, `A` — handy for showing the user which key they pressed.
/ #cmd("REC", "RECALL"): revert the document to its last saved state.
/ #cmd("REOPEN"): choose a recently-closed file from a list to reopen, restoring the caret to where it was (in the GUI: #emph[File ▸ Reopen closed…]). `REOPEN <n>` skips the picker and reopens the first `n` closed files straight away (`REOPEN 1` = the most recent; `n` is clamped to the list length, and the most-recent of the batch ends up active). (The list is `SaveCfg`-persisted; see `SaveClosed`/`LoadClosed`.) #cmd("CLEARCLOSED"): empty the recently-closed list.
/ #cmd("RELOADLIB"): re-run the recorded library-load sequence. #cmd("UNLOADLIB"): drop all user-library routines.
/ #cmd("SAVELIB"): write all currently-loaded *user* library routines to one `.lib` file (`SUB…ENDSUB` per routine, `# DOC:` lines preserved), reloadable with #cmd("LOADLIB") or #cmd("ADDLIB"). `SaveLib [path]`; bare → `xyscript.lib` (resolved against `$CWD`). Under `CAPTURE` the text goes into the variable instead.
/ #cmd("RENAME", "REN"): rename or move a file.
/ #cmd("RETURN"): return from a routine, optionally with a value (read back as `$RV`).
/ #cmd("RO"): the read-only lock (bare toggles; `on`/`off` set).
/ #cmd("RUN"): read and run a script file: `RUN <file> [args…]`. A `SUB` the run file defines is LOCAL to it — the calling script cannot invoke it afterwards (that is a fatal `unknown command`, which stops the caller). Put routines you want to share in a `.lib` and #cmd("LOADLIB") it; a routine loaded that way IS callable by the rest of the same script. Words after the file are positional arguments the script reads as `$1`, `$2`, … (`$*` = all, `$#` = count) — exactly like a library routine. Quote a file path that contains spaces (`RUN "my script.xpl" a b`); an unquoted spaced path with no args still works. `/BG` runs it in the background (args are forwarded to the child); `/R:<var>` captures its output. #cmd("RUNCODE"): the same for XPL text in a variable (no positional args — the whole operand is code). #cmd("RUNHERE"): run the fenced code block under the caret.

== S

/ #cmd("SAVE", "SA"): save in place; `SAVE <file>` writes a copy — overwriting an existing file asks first (`/NV` skips). #cmd("SAAS", "SAVEAS"): save to and adopt a new path (bare = the Save-As dialog); overwriting a *different* existing file asks first, `/NV` overwrites without asking. #cmd("SAD"): save just the selection to a new file, byte-exact; overwriting an existing file asks first, `/NV` skips.
/ #cmd("ST"): "store" — save the file, then close its tab. Bare `ST` saves in place; `ST <path>` saves to `<path>` (adopting it), which also names an untitled document. Overwriting a different existing file asks first; `ST/NV <path>` overwrites without asking. The tab closes only if the save succeeds, so a bare `ST` on an untitled document (no name → `$ERROR`) is left open, not discarded.
/ #cmd("SAVECFG"): write the current settings as a runnable config script. #cmd("SAVECM"): write the command history as a runnable script that restores the saved history. #cmd("SAVECLOSED"): write the reopen list (recently-closed files) as a runnable script. #cmd("SAVEKBD"): dump the current keyboard map to a file.
/ #cmd("SAVELDTX"): write every non-empty text register (`@A`..`@Z`/`@0`..`@9`, set by #cmd("LDTX")) as a runnable XPL program — one `LDTX <slot> "<literal>"` line per register, sorted and escaped. `SAVELDTX <file>` names the file (bare uses a default); `RUN`ning it restores the registers. Script binders (`&`, #cmd("LDSC")) are not included. Under `CAPTURE`, the program text goes into the variable instead of a file.
/ #cmd("SAVELDSC", "SAVELDPM"): the script-binder counterpart of #cmd("SAVELDTX") — write every non-empty script register (`&A`..`&Z`/`&0`..`&9`, set by #cmd("LDSC")/#cmd("LDPM")) as a runnable XPL program of `LDSC <slot> "<xpl>"` lines (sorted, escaped). `SAVELDSC <file>` names the file (bare uses a default); `RUN`ning it restores the registers. Text binders (`@`, #cmd("LDTX")) are not included. Under `CAPTURE`, the program text goes into the variable.
/ #cmd("SAVEMAIL"): download raw messages from the IMAP mailbox to disk — `SAVEMAIL <range> [<path>]`, one message per file named `MSG001.TXT`, `MSG002.TXT`, … by message number, into `<path>` (created if needed; default `$CWD`). `<range>` is a comma list of numbers and `first-last` spans (`1-3,6-8,10`), or `all` for the whole mailbox (counted first, then saved). No decoding — the exact RFC-822 source is written. Needs #cmd("IMAPURL") and #cmd("MAILUSER")/#cmd("MAILPASS"). Runs as a background curl job — `WAIT`, then #sv("RC") is curl's exit code (`0` = ok) and #sv("XO") its output. For `all`, use a *bare* `WAIT` (it spans both the count and download phases; `WAIT/J $LJ` would return after only the count). `SAVEMAIL/W` runs synchronously (blocks until the whole download finishes, both phases, so #sv("RC") is set on return); a server refusal shows as #sv("RC") `100`.
/ #cmd("SENDMAIL"): send the active document as email via curl — `SENDMAIL [<to> ["subject"]]`. With no arguments, `To:`/`Cc:`/`Bcc:`/`Subject:`/`From:`/`Attach:` are read from a header block at the top of the buffer (up to the first blank line); the rest is the body. Any *other* valid header line you add (`Reply-To:`, `In-Reply-To:`, `X-…:`, …) is passed through into the message — except the structural MIME headers (`Content-Type`/`Content-Transfer-Encoding`/`MIME-Version`), which are computed and can't be overridden, while a `Date:`/`Message-ID:` you supply replaces the generated one. Addresses may be bare or the display-name form (`"Name" <addr>`). `SENDMAIL/F <file>` reads the message from a file instead of the active document (quote the path for spaces). Stage files with #cmd("ATTACH"). Needs #cmd("SMTPURL") and #cmd("MAILUSER")/#cmd("MAILPASS") (and usually #cmd("MAILFROM")). Runs as a background job; #sv("RC") is curl's exit code (0 = sent), streamed to *Console*. Add `/W` (combinable with `/F`) to send synchronously — it blocks until curl finishes, so #sv("RC") is set on return.
/ #cmd("SMTPURL"): the SMTP submission URL for #cmd("SENDMAIL") — `smtps://host:465` (implicit TLS) or `smtp://host:587` (STARTTLS); set it with `SMTPURL <url>` (bare reports it, `$SMTPURL` reads it). Session-only.
/ #cmd("SE", "SEB", "SEA", "SEBA"): search (`B` = backward, `A` = case-sensitive) (`/W /T /A /B /R /S /X`); `/X` extends a selection to the match. `/n` (a number, e.g. `SE/3`) finds the n-th match from the cursor, and errors if there are fewer than `n`.
/ #cmd("SELFGCOLOR", "SELFGCOLORS") / #cmd("SELBGCOLOR", "SELBGCOLORS"): the selected-text color / its highlight (`/HSL`; `…S` opens a picker).
/ #cmd("FINDFGCOLOR", "FINDFGCOLORS") / #cmd("FINDBGCOLOR", "FINDBGCOLORS"): the FIND dialog's live "highlight all" — matched-text color / its highlight background (default yellow; `/HSL`; `…S` opens a picker). The *current* match (the Enter/F3 target) reuses the selection colors instead.
/ #cmd("SPELLCOLOR", "SPELLCOLORS") / #cmd("GRAMMARCOLOR", "GRAMMARCOLORS"): the underline color for live language-check issues — spelling (default red) / grammar & word-choice (default blue) (`/HSL`; `…S` opens a picker). See #cmd("SPELLING").
/ #cmd("SPELLSTYLE") / #cmd("GRAMMARSTYLE"): the underline SHAPE for live spelling / grammar issues — `dotted` (the default), `solid`, or `squiggle`, each also in a heavier weight (`dotted-thick`, `solid-thick`, `squiggle-thick`; a `thick` / `bold` qualifier in either order works too, e.g. `SPELLSTYLE thick squiggle`). `SPELLSTYLE <shape>`; bare reports it. Read back via #sv("SPELLSTYLE") / #sv("GRAMMARSTYLE"); persists in the config.
/ #cmd("SEPCOLOR", "SEPCOLORS"): the separator color `R G B` (`/HSL`; `…S` opens a picker).
/ #cmd("SET"): assign a variable — target `x` or `$x`; supports `+= -= *= /= ^=`.
/ #cmd("SF"): save and close all buffers. #cmd("SL"): save every named, modified buffer.
/ #cmd("SORT"): reorder the selected lines (`/F:n` skip, `/N:n` take key characters).
/ #cmd("SRANDOM"): `SRANDOM <seed>` — seed the generator behind the #func("RANDOM") function, so the sequence it produces repeats. Left unseeded, `RANDOM` starts from an entropy-based value that differs each run.
/ #cmd("SUB", "ENDSUB", "ES"): define a routine, ending at `ENDSUB`/`ES`.
/ #cmd("SUBMIT"): run the command line's contents. #cmd("XC"): run them but keep the focus and text.
/ #cmd("SV"): save the selection's text into a variable.
/ #cmd("SWITCH", "CASE", "DEFAULT", "ENDSWITCH", "ESW"): a multi-way branch — `SWITCH <expr>`, then `CASE <value>[, <value>…]` arms (no fall-through), an optional last `DEFAULT`, closed at `ENDSWITCH`/`ESW`. A `CASE` matches when `<expr>` equals ANY of its values, so `CASE 75, 101` fires for either. The values are a *comma* list, not an expression: write `CASE 75, 101`, never `CASE 75 or 101` — the latter is silently accepted but wrong, because `75 or 101` is evaluated as a boolean (both non-zero, so it means `CASE 1`).
/ #cmd("SMB", "CMB", "TMB"): the menu bar — on / off / toggle.
/ #cmd("SVR", "CVR", "TVR"): visible paragraph marks — on / off / toggle.
/ #cmd("SWS", "CWS", "TWS"): visible whitespace — on / off / toggle.
/ #cmd("SLN", "CLN", "TLN"): the line-number gutter — on / off / toggle.
/ #cmd("SGC", "CGC", "TGC"): the pop-up command line — on / off / toggle.
/ #cmd("SPS", "CPS", "TPS"): persistent selection — on / off / toggle.
/ #cmd("SCL", "CCL", "TCL") / #cmd("SNL", "CNL", "TNL") / #cmd("SSL", "CSL", "TSL"): Caps / Num / Scroll Lock — set / clear / toggle.
/ #cmd("SH", "SHELL"): open + focus the *Console* — an editable REPL tab. Type a command at the prompt and press Enter to run it (needs `TRUST 1`); output streams in above the prompt, and the tab is a normal document you can edit, copy, and search. `cd` is intercepted to change `$CWD`. `SH off` hides the Console tab. `$SH` reads its visibility.
/ #cmd("SHPROMPT"): `SHPROMPT [<template>]` — the Console's prompt string, `$`-expanded (default `$CWD> `). Quote it to keep a trailing space (`SHPROMPT "$CWD> "`); bare restores the default. `$SHPROMPT` reads it; saved by `SAVECFG`.
/ #cmd("ANSICOLOR"): `ANSICOLOR on|off` (bare toggles) — whether Console output is coloured from its ANSI escape codes (on, default) or shown raw. `$ANSICOLOR` reads it; saved by `SAVECFG`.
/ #cmd("SW", "SHELLWIN"): show/hide the *Console* tab — `SW on|off`, bare toggles (see also `X/BG`, which streams into it). `/H` / `/V` instead show the Console in a new split pane (stacked / side-by-side); if it's already in a pane, focus moves there. `$SW` reads the tab visibility.

== T

/ #cmd("TOP", "TF"): move to the very start of the document (`TF` is the "Top of File" name).
/ #cmd("TAB"): `TAB` / `TAB in` indents; `TAB out` outdents. With a multi-line selection, every touched line is indented/outdented (one undo unit); with no selection, `TAB in` inserts one indent unit at the caret. The unit is a tab character, or spaces when #cmd("TABSPACES") is on — and as soft tabs it pads to the NEXT `$TB` tab stop, so it inserts only as many spaces as are needed to reach it (a full `$TB` only when the caret is already on a stop), never overshooting. Bind it to the Tab key (e.g. `15=TAB` to indent, `TABLE=SHIFT 15=TAB out` to outdent).
/ #cmd("TB"): set the tab width, in spaces (the visual tab stop, and the indent-unit / outdent-step width for #cmd("TABSPACES")). Read back via #sv("TB").
/ #cmd("TABSPACES"): `TABSPACES on|off` — soft tabs. When on, `TAB` / indenting inserts spaces instead of a tab character (so the file contains spaces). At the caret it pads to the next `$TB` tab stop — enough spaces to reach it, not a blind `$TB` — so the text always lands on a stop. Read back via #sv("TABSPACES"); persists in the config.
/ #cmd("TABINDENT"): `TABINDENT on|off` — auto-indent. When on, pressing Enter starts the new line at the same indentation as the line above (its leading whitespace is copied). Read back via #sv("TABINDENT"); persists in the config. #sv("INDENTED") reads the current line's indentation in columns (leading spaces count 1 each, a tab advances to the next `$TB` stop).
/ #cmd("WRAPINDENT"): `WRAPINDENT on|off` — hanging indent. When on, the soft-wrapped continuation rows of a long line align under that line's own indentation (its #sv("INDENTED") level) instead of returning to the left margin — so wrapped code/prose keeps its block shape. Affects display only (the file's bytes are unchanged). Read back via #sv("WRAPINDENT"); persists in the config.
/ #cmd("BALPAIRS"): `BALPAIRS on|off` — matched-bracket highlighting. When on, placing the caret on (or just after) one of `()[]{}` highlights that bracket and its nesting-aware partner. A bracket with no partner is reported on the status line but not highlighted. Read back via #sv("BALPAIRS"); persists in the config.
/ #cmd("BALPAIRSCOLOR", "BALPAIRSCOLORS"): the matched-bracket highlight color — `BALPAIRSCOLOR <r g b>` (`/HSL`; `BALPAIRSCOLORS` opens a picker); `BALPAIRSCOLOR default` clears it so the highlight follows the selection color. Read back via #sv("BALPAIRSCOLOR") (empty = follows the selection color); persists in the config.
/ #cmd("TYPEPAIRS"): `TYPEPAIRS on|off` — auto-close typed pairs. When on, typing a left `(`, `[`, `{`, `"`, or `'` also inserts its partner and leaves the caret between them. Typing the closer right before an auto-inserted one steps over it instead of doubling it; typing an opener with a selection wraps the selection (keeping it selected); Backspace between an empty pair deletes both. A quote typed right after a letter or digit is left alone (so apostrophes in prose aren't paired). Interactive typing only — the scripted `INSERT` verb is never auto-paired. Read back via #sv("TYPEPAIRS"); persists in the config.
/ #cmd("TH"): toggle focus between the document and the command line.
/ #cmd("THEME"): apply a color scheme across the whole editor — `THEME light` or `THEME dark` for the built-ins, or `THEME <path>` to activate a theme file (a SAVECFG-format color preset, e.g. one under `themes/`), which is resolved against #sv("CWD") and run.
/ #cmd("THS") / #cmd("TVS"): split the current pane in two, showing a second buffer alongside. #cmd("THS") stacks the panes *horizontally* — the split runs left-to-right, so the current buffer is on top and the second below; #cmd("TVS") splits *vertically* — the current buffer on the left, the second on the right. The second buffer is the one named in an argument (`THS notes.txt`, an already-open tab), else the *alternate* buffer (the #cmd("BUFALT") target), else a fresh empty buffer. A buffer can only be shown in one pane at a time. Move focus between panes with #cmd("TW"); un-split with #cmd("TUS"). (For a split that starts a brand-new empty buffer, use `NEW/H` / `NEW/V`.) #sv("PANES") reads the pane count; #sv("PANE") the focused pane's position.
/ #cmd("TW"): move focus to the next pane (wraps around). Keyboard input, the caret, and the content specials all follow the focused pane.
/ #cmd("TUS"): un-split — collapse the focused pane, giving its space back to the neighbor; the collapsed pane's buffer stays open as a tab. (To close the file *and* undo its pane in one step, use #cmd("AB").)
/ #cmd("TRV"): swap the two panes of the current split (the focused buffer trades places with its neighbor).
/ #cmd("TZ"): toggle *zoom* — show the focused pane full-window, hiding the others; press again to restore the split. The layout is untouched, so nothing is lost.
/ #cmd("TITLEBAR"): `TITLEBAR on|off` (bare = toggle) — show/hide the OS window title bar; when off, a thin draggable strip at the top moves the window.
/ #cmd("WV"): `WV on|off` (bare = toggle) — *Writer's View*, a distraction-free mode that hides ALL chrome (title bar, menu bar, tab strip, command line, and status line), leaving just the text window. `Esc` turns it off, restoring the previous view. Transient — it is never saved, so you can't launch stuck in it. Read back via #sv("WV"); on the `View` menu.
/ #cmd("TABBGCOLOR", "TABBGCOLORS"): a GUI tab's background color for a state — `TABBGCOLOR <active|hover|inactive> <r g b>` (`…S` opens a picker).
/ #cmd("TABXCOLOR", "TABXCOLORS"): the tab close-× glyph color — `TABXCOLOR <r g b>` (`/HSL`; `…S` opens a picker); `TABXCOLOR default` resets it to the UI foreground.
/ #cmd("TOPDF", "PRINT"): export the document to PDF, rendered per its `DOCTYPE` (`TOPDF <path>`, or bare derives the name). For a Markdown/GFM #emph[or authored-Typst] document, any #cmd("MDA") styling (colours + bold/italic/underline/reverse) is woven into the output, so it prints as it appears on screen. In an authored-Typst document the styling wraps only prose, leaving your own Typst markup (headings, `#`-calls, math, code) untouched.
/ #cmd("TRUST"): enable or disable shell-out — `TRUST 1` / `TRUST 0`.
/ #cmd("TXT"): show a scrollable text dialog (`/T:"title"`).
/ #cmd("TXTFGCOLOR", "TXTFGCOLORS") / #cmd("TXTBGCOLOR", "TXTBGCOLORS"): the `TXT` window's text / background color (`/HSL`; `…S` opens a picker).
/ #cmd("TXTFAMILY") / #cmd("TXTSTYLE") / #cmd("TXTFONT", "TXTFONTS") / #cmd("TXTSCALE"): the `TXT` window's font family / style / size / zoom.

== U

/ #cmd("UIFGCOLOR", "UIFGCOLORS") / #cmd("UIBGCOLOR", "UIBGCOLORS"): the UI chrome's text / background color (`/HSL`; `…S` opens a picker).
/ #cmd("UIFAMILY") / #cmd("UISTYLE") / #cmd("UIFONT", "UIFONTS") / #cmd("UISCALE"): the UI chrome's font family / style / size / zoom (`+n`/`-n` adjusts). #cmd("UIRESET"): reset the UI sizing knobs to their defaults.
/ #cmd("UNDO") / #cmd("REDO"): undo / redo edits (argument: a count or `ALL`).

== W

/ #cmd("WAIT"): pause until background job(s) finish, or `WAIT <ms>` for a timed pause (`/J <id>`).
/ #cmd("WHILE", "ENDWHILE", "EW"): loop while a condition holds, ending at `ENDWHILE`/`EW`.
/ #cmd("WINGEO"): `WINGEO on|off` (bare = toggle) — whether XyScript's windows remember their size and position between runs (the main window and the secondary `FIND`, `DIR/W`, and `TXT` windows, each in its own file); turn it off on mixed-DPI multi-monitor setups where the remembered size misbehaves.
/ #cmd("SESSION"): `SESSION on|off` (bare = toggle) — auto-save-and-restore your open tabs across runs, Notepad++-style. When on, every graceful quit (command, menu, or the window's close button) writes the set of open tabs, and the next launch reopens them in order with the caret where you left it. Dirty and untitled buffers keep a private *shadow* backup, so they come back with their unsaved content (still marked modified) and your real files are never written to until you `SAVE`. A clean file is simply reopened from disk. The session is also snapshotted as you work, so a *crash* or forced kill still restores your tabs, not only a deliberate quit: a change to the set of tabs or the pane layout is saved *immediately*, while buffer text is saved every few seconds. A split-pane arrangement is restored as well (unless a pane held a transient view, in which case the tabs come back un-split). Off by default; persists in the config; read back via #sv("SESSION"). (Transient tabs — directory listings and the Shell-output pane — aren't part of a session.)
/ #cmd("WORDCOUNT"): count the words in the selection or document.
/ #cmd("WRITEFILE", "WF"): `WRITEFILE <path> <content>` — write `content` to a file (resolved against `$CWD`), overwriting it. The content is taken raw after `$`-substitution (like `PUT`), so `WF out.txt $x` writes the variable's value verbatim; quote the path for spaces. Read it back with the `READFILE()` function.
/ #cmd("WRITELN", "WRL"): insert text at the caret like `INSERT`, then add a trailing newline in the document's line-ending style (`LF`/`CRLF`), the same as `ENTER`.

== X

/ #cmd("X"): run a shell command (`/NV` detached, `/BG` background, `/R:<var>` captures output). #cmd("XV"): run one capturing its output into a variable. #cmd("XY"): run one and insert its output at the caret. These three take the rest of the line verbatim, so a `;` reaches the shell; to chain an XyScript command afterwards, wrap the shell command in brackets — `X [make; test]; PR $RC` runs `make; test`, then prints `$RC`. Brackets nest (a subshell needs no doubling: `X [(cd d; ls)]`) and `\[` / `\]` pass a literal bracket.
/ #cmd("XD", "YD"): cancel the current selection.

== Z

/ #cmd("ZT"): reset (zero) the stopwatch.

= Functions

Functions take a value and hand you back a new one. Call them by name with the
arguments in parentheses — `LEN("hello")` — and use them wherever XyScript expects
an expression: in `SET`, `IF`, and `WHILE`, or inline on any command line inside
`$( )`. They never change your document; they just compute.

== Working with text

/ #func("LEN", "s"): number of characters in string `s` (or the number of items, if `s` is an array or map).
/ #func("UPPER", "s") / #func("LOWER", "s"): `s` in upper- or lower-case.
/ #func("STRIP", "s"): `s` with leading *and* trailing whitespace removed.
/ #func("LSTRIP", "s") / #func("RSTRIP", "s"): `s` with whitespace removed from just the left or just the right.
/ #func("TRIMLEFT", "s, n") / #func("TRIMRIGHT", "s, n"): `s` with its first / last `n` characters dropped.
/ #func("SUBSTR", "s, i, n"): the `n`-character piece of `s` starting at 0-based offset `i`.
/ #func("LEFT", "s, n") / #func("RIGHT", "s, n"): the leftmost / rightmost `n` characters of `s`.
/ #func("INDEX", "s, sub"): the 0-based position of `sub` within `s`, or `-1` if it isn't found.
/ #func("CONTAINS", "s, sub"): `1` if `sub` appears anywhere in `s`, otherwise `0`.
/ #func("REPLACE", "s, from, to, [[-]n]"): `s` with occurrences of `from` replaced by `to` — all of them, or, with the optional 4th argument, at most `n`: a positive `n` replaces the first `n` (from the start), a negative `-n` the last `n` (from the end), and `0` replaces none. So `REPLACE("a-b-c-d", "-", "+", -2)` = `a-b+c+d`.
/ #func("CHR", "n"): the character whose code is `n` (0–255). #func("ASC", "s"): the reverse — the code of `s`'s first character.
/ #func("FORMAT", "template, ..."): interpolate the remaining arguments into `template`, at each `{}` placeholder. `{}` takes the next argument in order, `{n}` an explicit 0-based one (reusable), and `{{`/`}}` print literal braces. A placeholder may carry a field spec after a colon — `{:spec}` or `{n:spec}` — of the form `[[fill]align][sign][#][0][width][,][.precision][type]`: `align` is `<` left, `>` right, or `^` centre (with an optional fill character in front); `sign` is `+` (always show it), `-` (default), or a space (a leading space on positives, so columns line up); `#` adds a `0x`/`0o`/`0b` radix prefix; a leading `0` zero-pads numbers (keeping the sign/prefix in front); `width` is the minimum field width; `,` inserts thousands separators; `.precision` sets decimal places (for a float) or a maximum length (for a string); and `type` is `s` string, `d` integer, `f` fixed-point, `e`/`E` scientific, `%` percent, or `x`/`X`/`o`/`b` hex/octal/binary. Numbers right-align by default, text left-aligns, and integers stay #emph[exact] at any size (`FORMAT("{:,}", 2^100)` groups all 31 digits). #ex("PR $(FORMAT(\"{} = {:>8.2}\", \"rate\", 3.14159))", "rate =     3.14")

== Classifying a string

Each returns `1` (true) or `0`. Every one is `0` for an empty string. The character-class tests below check that *every* character is in the given set (and do not trim, so a stray space fails).

/ #func("ISDEC", "s"): all characters are decimal digits `0`–`9`.
/ #func("ISHEX", "s"): all characters are hex digits `0`–`9`, `A`–`F`, `a`–`f` (no `0x` prefix).
/ #func("ISALPHA", "s"): all characters are ASCII letters `A`–`Z`, `a`–`z`.
/ #func("ISALNUM", "s"): all characters are ASCII letters or digits.
/ #func("ISUPPER", "s"): every ASCII *letter* in `s` is upper-case, and there is at least one — non-letters (spaces, digits, punctuation) are ignored, so `ISUPPER("HELLO, WORLD!")` is true but `ISUPPER("Hello")` and `ISUPPER("123")` are not.
/ #func("ISLOWER", "s"): the same for lower-case: every ASCII letter is lower-case and there is at least one.
/ #func("ISTITLE", "s"): `s` is title-cased — each run of letters starts with an upper-case letter and continues in lower-case, with at least one letter. `ISTITLE("Hello World")` is true; `ISTITLE("Hello world")` and `ISTITLE("HELLO")` are not.
/ #func("ISSPACE", "s"): all characters are ASCII whitespace — space, tab, newline, carriage return, vertical tab, or form feed.
/ #func("ISPRINT", "s"): all characters are printable ASCII — space through `~` (code `32`–`126`); control characters and non-ASCII fail.
/ #func("ISPUNCT", "s"): all characters are ASCII punctuation — the printable marks and symbols that are not letters, digits, or spaces (`!`, `?`, `.`, `,`, `;`, `:`, `-`, `_`, brackets, quotes, and so on: 32 in all).
/ #func("ISASCII", "s"): all characters are 7-bit ASCII (code `0`–`127`).
/ #func("ISUTF8", "s"): all characters are in the Basic Multilingual Plane (`U+0000`–`U+FFFF`) — i.e. no "astral" characters such as most emoji. (Every XyScript string is valid UTF-8, so this tests the range, not the encoding.)
/ #func("ISINT", "s"): `s` is a whole number — read as XyScript reads a value, so leading/trailing spaces and a leading sign are allowed (`ISINT(" -42 ")` is true). `4.2` is not an integer.
/ #func("ISFLOAT", "s"): `s` is a number written in *float* form — a finite value with a decimal point or exponent (`4.2`, `1e3`, `-0.5`). A plain integer is #func("ISINT") instead, and `inf`/`nan` are excluded — so `ISINT` and `ISFLOAT` split the numeric strings between them.

== Splitting and joining

/ #func("SPLIT", "s, sep"): an array made by cutting `s` at each occurrence of the text `sep`.
/ #func("SPLITS", "s, chars"): like `SPLIT`, but cuts at *any* single character that appears in `chars`.
/ #func("JOIN", "arr, sep"): the reverse — glue the items of array `arr` into one string, with `sep` between them.
/ #func("CHARS", "s") / #func("WORDS", "s") / #func("LINES", "s"): an array of `s`'s characters / whitespace-separated words / lines.

== Arrays and collections

/ #func("ITER", "x"): the sequence a `FOR … IN` walks over `x` — an array's items, a map's keys, or a string's characters.
/ #func("ITERP", "x"): like `ITER`, but yields `[key, value]` pairs (for the `FOR k, v IN …` form).
/ #func("PUSH", "arr, v") / #func("POP", "arr"): a copy of `arr` with value `v` appended / with its last item removed — the original is unchanged, so assign the result (`SET a = PUSH($a, v)`).
/ #func("SLICE", "arr, start[, end]"): the part of `arr` from index `start` up to (but not including) `end`; omit `end` to mean "to the end", and a negative index counts from the end.
/ #func("SORT", "arr[, reverse]"): a sorted copy of `arr`; pass a truthy `reverse` to sort descending.
/ #func("VAR", "name"): the value of the variable whose name is the string `name` (an indirect lookup).
/ #func("SEL", "[n]"): the text selected in the current tab — or, given a 1-based tab number `n`, the selection in that tab (empty when there is none, or `n` is out of range). So `SEL($CT)` is the current tab.

== Numbers

/ #func("ABS", "n"): the absolute value of `n`. #func("INT", "n"): `n` with any fractional part dropped (toward zero).
/ #func("SQRT", "n") / #func("SQ", "n") / #func("CUBE", "n"): the square root, square, and cube of `n`.
/ #func("POW", "base, exp"): `base` raised to the power `exp` (the same as `base ^ exp`) — exact at any size for whole-number operands (`POW(2, 256)` is the full 78-digit value). #func("FAC", "n"): the factorial of `n`, computed in exact arbitrary precision (`FAC(30)` is `265252859812191058636308480000000`, every digit).
/ #func("EXP", "n"): _e_ raised to the power `n`. #func("LN", "n"): natural logarithm. #func("LOG", "n"): base-10 logarithm.
/ #func("BASE", "num, from, to"): the number string `num`, read in base `from`, rewritten in base `to` — `BASE("ff", 16, 10)` is `255` (bases 2–36).
/ #func("HEX", "n, p"): `n` as a lowercase hexadecimal string, left-padded with zeroes to at least `p` places — `HEX(255, 4)` is `00ff`. Never truncates: a value wider than `p` keeps all its digits (`HEX(4096, 2)` is `1000`). The count `p` is optional (`HEX(255)` is `ff`); a fractional `n` truncates to an integer first; a negative `n` prints as `-<hex>`.
/ #func("SIN", "n") / #func("COS", "n") / #func("TAN", "n"): trig functions of the angle `n` (in radians); #func("ASIN", "n") / #func("ACOS", "n") / #func("ATAN", "n") invert them, and #func("ATAN2", "y, x") takes two coordinates.
/ #func("SINH", "n") / #func("COSH", "n") / #func("TANH", "n"): the hyperbolic versions, inverted by #func("ASINH", "n") / #func("ACOSH", "n") / #func("ATANH", "n").
/ #func("PI") / #func("E"): the constants π and _e_ (called with empty parentheses).
/ #func("RANDOM", "min, max"): a random integer from `min` to `max` *inclusive* (arguments swapped if `min > max`). Seed the generator with the #cmd("SRANDOM") command for a repeatable sequence; unseeded it varies each run. #ex("PR $(RANDOM(4,4))", "4")

== The keyboard

/ #func("KEY", "scan"): the command bound to the key with scan code `scan` — `KEY($SC)` tells you what the last key you pressed does (on the unshifted layer). Pass #sv("SCC") instead — `KEY($SCC)` — and it reads the binding on the *modifier layer* the key was pressed on, since the modifier bits in `scan` (≥ 256) select the Shift / Ctrl / Alt table.
/ #func("GETKEY", "scc"): the NAME of the key with computed scan code `scc` (a #sv("SCC") value — key code plus modifier bits) — the string #cmd("READKEY")`/D` would write into its variable, e.g. `CTRL+Y`, `Enter`, `Backspace`. The modifier prefix comes from the current keymap's own modifier definitions (so a custom KBD's modifiers read correctly); the key itself is named by the keymap's character for a printable key, else its standard name (`Enter`, `F3`, `Up`, `Numpad 5`, `NumLock`, `Pause/Bk`, …). Unlike a live #cmd("READKEY"), it also names keys that verb skips — the numpad, `NumLock`/`ScrLock`/`CapsLock`, and `Pause`. Empty string if `scc` carries unknown modifier bits or names no key. (Contrast #func("KEY"), which returns what the key *does*, not what it is *called*.)

== Files and paths

/ #func("GETPATH", "p"): split the path `p` into a 4-element array — element `0` the directory (no trailing slash), `1` the filename with extension, `2` the filename alone, `3` the extension (no dot). If `p` is an existing directory it becomes element `0` and the three filename parts are empty, so a folder's name isn't mistaken for a file. Index the result with `[0]`…`[3]`.
/ #func("EXISTS", "p"): `1` if `p` exists (a file or a directory), else `0`.
/ #func("ISDIR", "p"): `1` if `p` exists and is a directory, else `0`.
/ #func("ISFILE", "p"): `1` if `p` exists and is a file (not a directory), else `0`.
/ #func("READFILE", "path") / #func("RF", "path"): the text contents of the file at `path` (resolved against `$CWD`). A missing, unreadable, or non-UTF-8 file is a *trappable* error — it sets `$ERROR` rather than halting, so a script can recover. Pairs with the `WRITEFILE` verb — e.g. `SET text = READFILE("notes.txt")`.

== Date and time

The mask is taken literally (no quotes needed): #func("DATE", "m-d-yyyy") → `6-27-2026`, #func("DATE", "mm-dd-yy") → `06-27-26`. Letters are format codes; any other character (`-` `/` `:` space `.`) passes through. Quote the mask (`DATE("m.d.yy")`) only if it contains characters the expression lexer would reject.

/ #func("DATE", "mask"): the current local date by `mask` — `yyyy`/`yy` (year), `mmmm`/`mmm` (month name, full / abbreviated: `July` / `Jul`), `mm`/`m` (month number, padded / bare), `dddd`/`ddd` (weekday name, full / abbreviated: `Wednesday` / `Wed`), `dd`/`d` (day). The longest matching code wins, so a name code is tried before the numeric one.
/ #func("TIME", "mask"): the current local time by `mask` — `hh`/`h`, `mm`/`m`, `ss`/`s`. Appending an `am`/`pm` (lowercase) or `AM`/`PM` (uppercase) marker switches to 12-hour and appends the correct meridiem in that case: `TIME(h:mm PM)` → `3:05 PM`, `TIME(h:mm am)` → `3:05 pm`.

Date arithmetic works in the *proleptic Gregorian* calendar (today's leap rules for every year, no 1582 gap) at day resolution. The Julian Day Number — a single integer that counts calendar days — is the pivot: convert a date to a JDN, do ordinary integer math (`+`, `-`), and convert back. A malformed date (a bad month, or a day past the month's length) is a *trappable* error (it sets `$ERROR` rather than halting); #func("ISDATE") checks validity without erroring. Where these functions take a `mask` it is a normal *quoted* argument (unlike `DATE`/`TIME`, whose whole argument is the mask), using the same format codes as #func("DATE").

/ #func("DATE2JDN", "y, m, d"): the Julian Day Number of date `y`-`m`-`d`. The result increases by exactly 1 per calendar day, so differences and offsets are plain integer arithmetic. #ex("PR $(DATE2JDN(2000, 1, 1))", "2451545")
/ #func("JDN2DATE", "jdn, [mask]"): a Julian Day Number back to a date string — ISO `yyyy-mm-dd` with no `mask`, or formatted by a quoted `mask`. #ex("PR $(JDN2DATE(2451545))", "2000-01-01") #ex("PR $(JDN2DATE(2451545, \"dddd\"))", "Saturday")
/ #func("DOW", "y, m, d"): the day of the week, numbered ISO-style `1` = Monday … `7` = Sunday. #ex("PR $(DOW(2000, 1, 1))", "6")
/ #func("DATEDIFF", "y1, m1, d1, y2, m2, d2"): the whole days from the first date to the second — negative if the second is earlier (it is just `DATE2JDN(date2) - DATE2JDN(date1)`). #ex("PR $(DATEDIFF(2000, 1, 1, 2000, 12, 31))", "365")
/ #func("DATEADD", "y, m, d, n, [mask]"): the date `n` days after `y`-`m`-`d` (`n` may be negative), as a string — ISO by default, or by a quoted `mask`. #ex("PR $(DATEADD(2000, 1, 1, 31))", "2000-02-01")
/ #func("TODAY"): today's Julian Day Number, from the same clock as #func("DATE") — the anchor for "days until / since" sums, e.g. `DATE2JDN(2026, 12, 25) - TODAY()`. Like `DATE`, it errors in a context with no clock.
/ #func("LEAP", "y"): `1` if `y` is a leap year, else `0`. #ex("PR $(LEAP(2000))", "1")
/ #func("DAYSINMONTH", "y, m"): the number of days in month `m` of year `y`, `28`–`31`. #ex("PR $(DAYSINMONTH(2000, 2))", "29")
/ #func("DAYOFYEAR", "y, m, d"): the ordinal day within the year, `1`–`366` (January 1 is `1`). #ex("PR $(DAYOFYEAR(2000, 3, 1))", "61")
/ #func("ISDATE", "y, m, d"): `1` if `y`-`m`-`d` is a real calendar date, else `0`. It never traps, so use it to validate input before calling the functions that do. #ex("PR $(ISDATE(2001, 2, 29))", "0")
/ #func("DATEPARSE", "str, mask"): read the date string `str` into its parts — the inverse of #func("JDN2DATE"). Returns a 3-element array `[year, month, day]`; index it (`[0]`/`[1]`/`[2]`) or hand the parts to #func("DATE2JDN"). The quoted `mask` uses the same codes as #func("DATE"): `yyyy`, or `yy` (read with a 1969–2068 window); `mmmm`/`mmm` (month name / abbreviation); `mm`/`m` (month number); a weekday `dddd`/`ddd` (matched in the string but ignored); and `dd`/`d` (day). A padded code (`mm`, `dd`) wants exactly two digits, a bare one (`m`, `d`) one or two; every other mask character must appear literally. A string that doesn't fit the mask, has text left over, omits the year, month, or day, or spells an unreal date is a *trappable* error. #ex("PR $(DATEPARSE(\"07/23/2026\", \"mm/dd/yyyy\"))", "[2026, 7, 23]")

= Operators

When XyScript works out an expression — in `SET`, `IF`, `WHILE`, a `CASE`, or inside
`$( )` — it understands the usual operators:

/ `+` `-` `*` `/` `%`: add, subtract, multiply, divide, remainder. Integer math stays integer (`9 / 2` is `4`, floor division); a decimal anywhere makes it a float (`9.0 / 2` is `4.5`). Integers are #emph[arbitrary-precision] — they never overflow or round, growing as large as the answer needs (see below).
/ `^`: raise to a power — `2 ^ 10` is `1024` (the same as `POW`). With a whole-number base and exponent the result is an #emph[exact] integer of any size: `2 ^ 256` is the full 78-digit value, not a rounded approximation.
/ `.`: join two strings — `"file" . ".txt"` is `"file.txt"`.
/ `==` `!=` `<` `>` `<=` `>=`: compare two values; the result is `1` (true) or `0` (false).
/ `AND` `OR` `NOT`: combine conditions; `TRUE` and `FALSE` are just `1` and `0`, and the keywords are case-insensitive.
/ `[ ... ]`: an array — `[1, 2, 3]`; read an item (or a string's character, or a map's member) with `value[i]`.
/ `{ ... }`: a map — `{name: "Ada", year: 1815}`; read a member with `value["name"]`.
/ `$name` and `${name}`: read a variable; `$( ... )` works out a whole expression and drops the answer in, anywhere on a command line.

*Big integers.* Whole-number arithmetic is exact at any size: `+`, `-`, `*`, `^`, floor `/` and `%` never overflow or round, so `$(2 ^ 256)` prints all 78 digits and a factorial loop stays precise however large it grows. The one rule to remember is that a #emph[decimal] anywhere in the expression makes the whole result a floating-point number — with the usual ~15–16 significant digits — so keep a calculation in whole numbers when you need every digit (`2 ^ 256` is exact; `2.0 ^ 256` is not). (Base-conversion helpers — #func("HEX") and #func("BASE") — still work in ordinary machine integers.)

= Special variables

A *special variable* is a piece of live information XyScript keeps for you. Read one
by writing its name with a leading `$` — `PR $CWD`, `IF $MO …`, or inside `$( )` like
`$(LEN($SE))`. They are read-only: you can use one anywhere a value is expected, but
you can't assign to it. This section groups them by what they tell you; the next is a
plain alphabetical index.

== Files and the program

/ #sv("FI") / #sv("FP"): the active document's file name (no folder) / its full path.
/ #sv("CWD") / #sv("PA"): the editor's working directory (`$PA` is a synonym).
/ #sv("DC"): the operating system's folder-separator character (`\` or `/`).
/ #sv("NEWLINE"): the document's line ending — `LF` or `CRLF`.
/ #sv("NL"): the document's line-ending *characters* (a real newline) — the character form of #sv("NEWLINE"), for building text or file content that matches the document. (A string `\n` escape is always a lone LF; #sv("NL") is `\r\n` in a CRLF document.) New documents adopt the OS default.
/ #sv("CF") / #sv("CFA"): path of the last config loaded with `LOADCFG` / the file `AUTOCFG` is tracking (empty if off).
/ #sv("KB"): full path of the last keymap loaded with `LOADKBD`.
/ #sv("LANG"): path of the loaded `LANG` UI-translation catalog (empty when the interface is built-in English).
/ #sv("ED") / #sv("XSDIR"): full path of the running XyScript program / the folder it lives in.
/ #sv("VE") / #sv("OS"): the XyScript version / the operating-system name.

== The directory listing

/ #sv("D1"): `1` if the active buffer is a `DIR`/`LS` listing, else `0`.
/ #sv("DP"): the listing's path, with the file mask appended (e.g. after `DIR *.txt`).
/ #sv("DR"): the full path of the file or folder on the caret's line.

== The cursor and the line

/ #sv("CP"): the caret's character offset into the buffer (0-based).
/ #sv("LN") / #sv("LC"): the caret's line and column, both 1-based.
/ #sv("CX") / #sv("CY"): the caret's visual column (0-based, wrap-aware) / its visual row from the top of the view.
/ #sv("LI") / #sv("WD"): the text of the line / the word under the caret.
/ #sv("DOC"): the ENTIRE text of the active buffer (every character, newlines included) — the whole tab in one read. `SET all = $DOC`.
/ #sv("CH"): the character just to the right of the caret (a line break reads as `\n`).
/ #sv("TL"): the document's total line count.
/ #sv("FB") / #sv("FE"): `1` if the caret is at the very start / end of the buffer.

== The selection

/ #sv("SE"): the selected text (empty when nothing is selected).
/ #sv("SEL"): the most recent selection's text — kept even after it clears, and shared across tabs.
/ #sv("SL"): the number of characters selected.
/ #sv("DF"): `1` if there is a selection.
/ #sv("DS") / #sv("DN"): the selection's start / end offset (each the caret position when there's no selection).
/ #sv("CS"): `1` if the caret is on the selection (inside it or at either edge).

== Document and editor state

/ #sv("MO"): `1` if the document has unsaved changes.
/ #sv("RO") / #sv("RE"): `1` if the active tab is read-only.
/ #sv("FS") / #sv("WC"): the buffer's size in bytes / its whole-document word count.
/ #sv("EN"): the active document's encoding-view name (e.g. `utf-8`, `cp437`) — the one `ENCODING` sets.
/ #sv("CC"): how many replacements the last `CH` made.
/ #sv("TC") / #sv("CT"): the number of open tabs / the active tab's index (1-based).
/ #sv("TX"): `1` if the focus is in the document text, `0` if it's on the command line.
/ #sv("TB"): the tab width in spaces.
/ #sv("DOCTYPE"): how the document renders to PDF — `typst`, `gfm`, or `html`.
/ #sv("AU"): the autosave interval in seconds (`0` = off).
/ #sv("DECIMALS"): the fixed number of decimal places floats display with (or `off`).
/ #sv("AR"): the full path of the file most recently loaded with `LoadAR` (empty if none).

== View toggles

Each reads `1` when the feature is on, `0` when off.

/ #sv("MB"): the menu bar. #sv("GC"): the pop-up command line. #sv("TITLEBAR"): the OS window title bar. #sv("WINGEO"): whether window size+position is remembered across runs. #sv("SESSION"): whether open tabs are auto-saved and restored across runs.
/ #sv("CMPLACE"): where the command line sits — `top`, `bottom`, or `hidden` (set with `CMPLACE`).
/ #sv("VR") / #sv("WS") / #sv("LNS"): visible paragraph marks / visible whitespace / the line-number gutter.
/ #sv("PS"): persistent-selection mode. #sv("DIRNW"): open-in-new-tab mode for files picked from a listing. #sv("DIRWNW"): the same for the separate `DIR/W` window. #sv("DIRWCLOSE"): whether `DIR/W` closes once a file is opened. #sv("DIRSET"): the default DIR sort/filter/view switches. #sv("DIRRT"): reuse-the-listing-tab mode for `DIR`.
/ #sv("HLAUTO"): auto-highlighting when a file is opened.
/ #sv("SH") / #sv("SW") / #sv("PV"): the Console tab / the Console tab (synonym) / the live preview pane visibility. #sv("SHPROMPT"): the Console prompt template.

== Coding aids

Toggles and readbacks for the writing-code helpers (set with #cmd("TABSPACES"), #cmd("TABINDENT"), #cmd("WRAPINDENT"), #cmd("BALPAIRS"), #cmd("TYPEPAIRS")). The on/off ones read `1`/`0`.

/ #sv("TABSPACES"): soft tabs on — `TAB` and indenting insert spaces, not a tab character.
/ #sv("TABINDENT"): auto-indent on — pressing Enter copies the previous line's indentation.
/ #sv("INDENTED"): the current line's indentation, in columns (a leading tab counts to the next `$TB` stop).
/ #sv("WRAPINDENT"): hanging-indent on — a wrapped line's continuation rows align under its own indentation.
/ #sv("BALPAIRS"): matched-bracket highlighting on.
/ #sv("BALPAIRSCOLOR"): the matched-bracket highlight color (`R G B`; empty = follows the selection color).
/ #sv("TYPEPAIRS"): auto-close-typed-pairs on.

== Appearance

Colors read back as `R G B`; sizes and scales are numbers; families and styles are names.

/ #sv("FGCOLOR") / #sv("BGCOLOR"): the editor's text and background colors.
/ #sv("UIFGCOLOR") / #sv("UIBGCOLOR") / #sv("SEPCOLOR"): the UI chrome's text, background, and separator colors. #sv("TABXCOLOR"): the tab close-× color (empty = follows the UI text color).
/ #sv("SELFGCOLOR") / #sv("SELBGCOLOR"): the selected text's color and its highlight.
/ #sv("FINDFGCOLOR") / #sv("FINDBGCOLOR"): the FIND "highlight all" matched-text color and its highlight background.
/ #sv("EMPHFGCOLOR") / #sv("EMPHBGCOLOR"): the effective `EMPH` emphasis text / background color (the override if set, else the selection color).
/ #sv("SPELLCOLOR") / #sv("GRAMMARCOLOR"): the underline colors for live language-check spelling / grammar issues.
/ #sv("CRCOLOR") / #sv("CRSHAPE"): the caret's color and shape (bar / block / underline).
/ #sv("TABACTIVE") / #sv("TABHOVER") / #sv("TABINACTIVE"): the tab-strip colors (empty when at the theme default).
/ #sv("EDFONT") / #sv("EDFAMILY") / #sv("EDSTYLE"): the editor font's size, family, and style.
/ #sv("UIFONT") / #sv("UIFAMILY") / #sv("UISTYLE") / #sv("UISCALE"): the UI chrome's font size, family, style, and overall scale.
/ #sv("LSP"): the line spacing. #sv("OFL") / #sv("OF") / #sv("OFR"): the left margin (`$OF` is an alias of `$OFL`) and right margin, in columns.

== The TXT and directory-listing windows

/ #sv("TXTFGCOLOR") / #sv("TXTBGCOLOR"): the `TXT` pop-up window's text and background colors.
/ #sv("TXTFONT") / #sv("TXTFAMILY") / #sv("TXTSTYLE") / #sv("TXTSCALE"): its font size, family, style, and scale.
/ #sv("DWFGCOLOR") / #sv("DWBGCOLOR"): the `DIR/W` window's text and background colors.
/ #sv("DWFONT") / #sv("DWFAMILY") / #sv("DWSTYLE") / #sv("DWSCALE"): its font size, family, style, and scale.
/ #sv("PVFAMILY") / #sv("PVSIZE") / #sv("PVCODE"): the Markdown Preview/`TOPDF` body family, body size (points), and code family — empty / `0` when unset (Typst's default).

== The command line, keyboard, and locks

/ #sv("CL"): the last command you issued. #sv("CM"): the command line's current contents.
/ #sv("CMH"): the whole command history as one string (newest first).
/ #sv("SC"): the scan code of the last key pressed (feed it to `KEY()`).
/ #sv("SCC"): "scan code computed" — #sv("SC") plus the bit value of any modifier key (Shift / Ctrl / Alt) held with it, so one number identifies the key *and* its layer. Feed it to `KEY()` (`KEY($SCC)`) to read the binding on that modifier layer.
/ #sv("CAPSLOCK") / #sv("NUMLOCK") / #sv("SCROLLLOCK"): the three lock-key states.

== Date and time

/ #sv("DA") / #sv("TM"): the current date (`YYYY-MM-DD`) and time (`HH:MM:SS`), local.
/ #sv("ET") / #sv("ETS"): the stopwatch's elapsed time, formatted / as whole seconds.
/ #sv("YEAR") / #sv("MON") / #sv("MDAY"): the year (4 digits), month, and day (2 digits each).
/ #sv("HOUR") / #sv("MIN") / #sv("SEC") / #sv("MSEC"): the hour (24-hour), minute, second, and millisecond.
/ #sv("WDAY") / #sv("YDAY"): the day-of-week number (1 = Sunday) and the day of year (3 digits).

== Shell, jobs, and the clipboard

/ #sv("TR"): `1` if shell-out is enabled (see `TRUST`).
/ #sv("RC") / #sv("XO"): the exit code / captured output of the last shell command or job.
/ #sv("JOBS") / #sv("LJ"): the number of running background jobs / the id of the last one launched.
/ #sv("LS"): the text of the last status-line message.
/ #sv("CB") / #sv("XB"): the clipboard's contents / the deletion buffer's contents.

== Scripting

/ #sv("ERROR"): the trappable-error flag — `1` right after a command failed, so a script can react.
/ #sv("RV"): the value the last `RETURN` produced.

Inside a `SUB`, the arguments passed to it are also available: `$1`, `$2`, … by position,
`$*` for all of them joined with spaces, and `$#` for how many there are.

== Email

/ #sv("SMTPURL") / #sv("MAILUSER") / #sv("MAILFROM"): the SMTP submission URL, account username, and `From:` address (set with the matching verbs).
/ #sv("MAILPASS"): `set` or `not set` — the mail password is never revealed.
/ #sv("IMAPURL") / #sv("MAILBOX"): the IMAP server URL and the mailbox being read (default `INBOX`).
/ #sv("ATTACH"): the files staged for the next `SENDMAIL` (comma-separated names).
/ #sv("MAILDEBUG"): `1` if verbose curl logging is on.

== Alphabetical index

Every special variable in one list, for quick lookup.

/ #sv("AR"): path of the last `LoadAR` file.
/ #sv("ATTACH"): files staged for the next `SENDMAIL`.
/ #sv("AU"): autosave interval (seconds; 0 = off).
/ #sv("BALPAIRS"): matched-bracket highlighting on.
/ #sv("BALPAIRSCOLOR"): matched-bracket highlight color (empty = follows the selection color).
/ #sv("BGCOLOR"): editor background color.
/ #sv("C1") / #sv("C2") / #sv("C3") / #sv("C4") / #sv("C5") / #sv("C6") / #sv("C7") / #sv("C8") / #sv("C9"): the counters' current values (what a bare `Cn` would put next), without the decorator.
/ #sv("CAPSLOCK"): Caps Lock state.
/ #sv("CB"): clipboard contents.
/ #sv("CC"): replacements made by the last `CH`.
/ #sv("CF"): last `LOADCFG` file path.
/ #sv("CFA"): `AUTOCFG` tracked-file path.
/ #sv("CH"): character to the right of the caret.
/ #sv("CL"): last command issued.
/ #sv("CM"): current command-line contents.
/ #sv("CMH"): full command history (newest first).
/ #sv("CMBLANK"): `1` if the command line is blanked after each command (set with `CMBLANK`).
/ #sv("SHOWONETAB"): `1` if the tab strip shows for a single open file (set with `SHOWONETAB`).
/ #sv("CARETMOTION"): the Left/Right caret-motion order — `logical` or `visual` (set with `CARETMOTION`).
/ #sv("WRAP"): `1` if long lines wrap to the window, `0` if they run past the edge with a horizontal scrollbar (set with `WRAP`).
/ #sv("HEX"): `1` if the active document is shown as a hex + text grid, `0` for the normal text view (set with `HEX on|off`).
/ #sv("MARKS"): the number of bookmarks in the active document. #sv("MARK"): `1` when the caret sits on a bookmark, else `0` (set with `MARKSET` / `MARKCLEAR`).
/ #sv("MARKGUTTER"): `1` if the bookmark gutter dot is shown, `0` if hidden (set with `MARKGUTTER`).
/ #sv("WRAPHANG"): the hanging indent (in spaces) for wrapped continuation rows, `0` for flush (set with `WRAPHANG`).
/ #sv("ULSPACES"): `1` if `MDA` underlines paint through inter-word spaces, `0` if they break at spaces (set with `ULSPACES`).
/ #sv("UNTITLEDTAB"): `1` if the blank Untitled tab is auto-closed when a document is opened (set with `UNTITLEDTAB`).
/ #sv("GOTOOPEN"): `1` if opening an already-open file switches to its tab instead of prompting (set with `GOTOOPEN`).
/ #sv("CMPLACE"): command-line placement (top / bottom / hidden).
/ #sv("CP"): caret's character offset.
/ #sv("CARETS"): number of active carets (primary + secondaries) — `1` normally, more under multi-caret.
/ #sv("CARETP"): the primary caret's index in the document-order caret list, so `$CP == $MCP[$CARETP]`.
/ #sv("MCP"): array of every caret's character offset, in document order (the mirror of #sv("CP")). Loop it with `FOR p IN $MCP`; the count is `LEN($MCP)` (or #sv("CARETS")). The classic caret specials (#sv("CP")/#sv("WD")/#sv("LI")/…) read the primary caret.
/ #sv("MSE") / #sv("MSL") / #sv("MDF") / #sv("MDS") / #sv("MDN") / #sv("MCS") / #sv("MCH") / #sv("MWD") / #sv("MLI") / #sv("MLN"): the `$M…` mirror family — one 0-based array element per caret, in document order, mirroring the classic caret specials #sv("SE") / #sv("SL") / #sv("DF") / #sv("DS") / #sv("DN") / #sv("CS") / #sv("CH") / #sv("WD") / #sv("LI") / #sv("LN") respectively (selected text, chars selected, has-selection 1/0, selection start / end offset, caret-on-selection 1/0, char to the right, word under caret, line text, 1-based line number). Every mirror has `LEN == ` #sv("CARETS") and `$X == $MX[$CARETP]`; a bare caret reports `""` / `0` for its selection but a valid position, char, word, and line.
/ #sv("MARK"): `1` when the caret is on a bookmark, else `0`. #sv("MARKS"): the number of bookmarks in the document. #sv("MARKGUTTER"): `1` if the gutter dot is shown.
/ #sv("CRCOLOR"): caret color.
/ #sv("CMCRCOLOR"): command-line caret color.
/ #sv("CRSHAPE"): caret shape.
/ #sv("CMCRSHAPE"): command-line caret shape.
/ #sv("CS"): `1` if the caret is on the selection.
/ #sv("CT"): active tab index.
/ #sv("CWD"): the working directory.
/ #sv("CX"): caret's visual column.
/ #sv("CY"): caret's visual row in the view.
/ #sv("D1"): `1` if the buffer is a directory listing.
/ #sv("DA"): current date.
/ #sv("DC"): folder-separator character.
/ #sv("DECIMALS"): float display precision (or `off`).
/ #sv("DF"): `1` if there is a selection.
/ #sv("DIRNW"): open files from a listing in a new tab.
/ #sv("DIRWNW"): open files from the `DIR/W` window in a new tab.
/ #sv("DIRWCLOSE"): close the `DIR/W` window after opening a file.
/ #sv("DIRSET"): default DIR sort/filter/view switches (e.g. `/OD/R`).
/ #sv("DIRRT"): reuse the listing tab for `DIR`.
/ #sv("DN"): selection end offset.
/ #sv("DOC"): the entire text of the active buffer.
/ #sv("DOCTYPE"): PDF render kind (typst/gfm/html).
/ #sv("DP"): directory-listing path + mask.
/ #sv("DR"): file under the listing caret.
/ #sv("DS"): selection start offset.
/ #sv("DWBGCOLOR"): DIR/W window background color.
/ #sv("DWFAMILY"): DIR/W window font family.
/ #sv("DWFGCOLOR"): DIR/W window text color.
/ #sv("DWFONT"): DIR/W window font size.
/ #sv("DWSCALE"): DIR/W window zoom.
/ #sv("DWSTYLE"): DIR/W window font style.
/ #sv("ED"): path of the XyScript program.
/ #sv("EDFAMILY"): editor font family.
/ #sv("EDFONT"): editor font size.
/ #sv("EDSTYLE"): editor font style.
/ #sv("EMPHBGCOLOR"): effective EMPH emphasis background color.
/ #sv("EMPHFGCOLOR"): effective EMPH emphasis text color.
/ #sv("EN"): active document's encoding-view name.
/ #sv("ERROR"): trappable-error flag.
/ #sv("ET"): stopwatch elapsed (formatted).
/ #sv("ETS"): stopwatch elapsed (seconds).
/ #sv("FB"): `1` if the caret is at the buffer start.
/ #sv("FE"): `1` if the caret is at the buffer end.
/ #sv("FGCOLOR"): editor text color.
/ #sv("FI"): document's file name.
/ #sv("FP"): document's full path.
/ #sv("FS"): buffer size in bytes.
/ #sv("GC"): pop-up command line visible.
/ #sv("HLAUTO"): auto-highlight on open.
/ #sv("HOUR"): hour (24-hour).
/ #sv("IMAPURL"): IMAP server URL for receiving.
/ #sv("INDENTED"): current line's indentation, in columns.
/ #sv("JOBS"): running background-job count.
/ #sv("KB"): last `LOADKBD` file path.
/ #sv("LANG"): loaded `LANG` UI-translation catalog path (empty = English).
/ #sv("LC"): caret's column (1-based).
/ #sv("LI"): text of the line under the caret.
/ #sv("LJ"): id of the last job launched.
/ #sv("LN"): caret's line (1-based).
/ #sv("LNS"): line-number gutter visible.
/ #sv("LS"): last status-line message.
/ #sv("LSP"): line spacing.
/ #sv("MAILBOX"): IMAP mailbox being read (default `INBOX`).
/ #sv("MAILDEBUG"): `1` if verbose curl logging is on.
/ #sv("MAILFROM"): `From:` address for `SENDMAIL`.
/ #sv("MAILPASS"): `set` / `not set` (password never revealed).
/ #sv("MAILUSER"): mail account username.
/ #sv("MB"): menu bar visible.
/ #sv("MDAY"): day of month.
/ #sv("MIN"): minute.
/ #sv("MO"): `1` if the document is modified.
/ #sv("MON"): month.
/ #sv("MSEC"): millisecond.
/ #sv("NEWLINE"): line ending (LF/CRLF).
/ #sv("NL"): line-ending characters (real newline; `\r\n` in a CRLF doc).
/ #sv("NUMLOCK"): Num Lock state.
/ #sv("OF"): left margin (alias of `$OFL`).
/ #sv("OFL"): left margin.
/ #sv("OFR"): right margin.
/ #sv("OS"): operating-system name.
/ #sv("PA"): synonym of `$CWD`.
/ #sv("PAPLACE"): where the `$CWD` path is shown — `cmline` or `status` (set with `PAPLACE`).
/ #sv("PAFGCOLOR") / #sv("PABGCOLOR"): the `$CWD` path text / background color (empty = default: dimmed UI text / transparent). Set with `PAFGCOLOR`/`PABGCOLOR`.
/ #sv("PATHLEN"): max display width (chars) of the `$CWD` path (set with `PATHLEN`).
/ #sv("PS"): persistent-selection mode on.
/ #sv("PV"): preview pane visible.
/ #sv("RC"): last shell command's exit code.
/ #sv("RE"): active tab read-only.
/ #sv("RO"): active tab read-only.
/ #sv("RV"): last `RETURN` value.
/ #sv("SC"): last key's scan code.
/ #sv("SCC"): scan code plus the held modifier's bit value.
/ #sv("SCROLLLOCK"): Scroll Lock state.
/ #sv("SE"): selected text.
/ #sv("SEC"): second.
/ #sv("SEL"): most recent selection's text.
/ #sv("SELBGCOLOR"): selection highlight color.
/ #sv("SELFGCOLOR"): selected-text color.
/ #sv("SEPCOLOR"): separator color.
/ #sv("SH"): Console tab visible (once the interactive shell pane; now a synonym for `$SW`).
/ #sv("SHPROMPT"): the Console prompt template (`SHPROMPT`).
/ #sv("ANSICOLOR"): `1` if Console output is ANSI-coloured, `0` if shown raw (`ANSICOLOR`).
/ #sv("SL"): selected character count.
/ #sv("SMTPURL"): SMTP submission URL for sending.
/ #sv("SW"): *Console* tab visible.
/ #sv("TABACTIVE"): active-tab color.
/ #sv("TABHOVER"): hover-tab color.
/ #sv("TABINACTIVE"): inactive-tab color.
/ #sv("TABINDENT"): auto-indent on Enter on.
/ #sv("TABSPACES"): soft tabs (insert spaces) on.
/ #sv("TABXCOLOR"): tab close-× color (empty = UI text color).
/ #sv("TB"): tab width in spaces.
/ #sv("TC"): total tab count.
/ #sv("TITLEBAR"): OS window title bar visible.
/ #sv("WV"): Writer's View on (all chrome hidden).
/ #sv("TL"): total line count.
/ #sv("TM"): current time.
/ #sv("TR"): `1` if shell-out is enabled.
/ #sv("TX"): `1` if focus is the text (`0` = command line).
/ #sv("TXTBGCOLOR"): TXT window background color.
/ #sv("TXTFAMILY"): TXT window font family.
/ #sv("TXTFGCOLOR"): TXT window text color.
/ #sv("TXTFONT"): TXT window font size.
/ #sv("TXTSCALE"): TXT window zoom.
/ #sv("TXTSTYLE"): TXT window font style.
/ #sv("TYPEPAIRS"): auto-close typed pairs on.
/ #sv("UIBGCOLOR"): UI chrome background color.
/ #sv("UIFAMILY"): UI chrome font family.
/ #sv("UIFGCOLOR"): UI chrome text color.
/ #sv("UIFONT"): UI chrome font size.
/ #sv("UISCALE"): UI zoom factor.
/ #sv("UISTYLE"): UI font style.
/ #sv("VE"): XyScript version.
/ #sv("VR"): visible paragraph marks on.
/ #sv("WC"): document word count.
/ #sv("WD"): word under the caret.
/ #sv("WDAY"): day-of-week number (1 = Sunday).
/ #sv("WINGEO"): window geometry remembered across runs.
/ #sv("SESSION"): open tabs auto-saved and restored across runs.
/ #sv("WRAPINDENT"): hanging indent for wrapped lines on.
/ #sv("WS"): visible whitespace on.
/ #sv("XB"): deletion-buffer contents.
/ #sv("XO"): last shell command's output.
/ #sv("XSDIR"): folder of the XyScript program.
/ #sv("YDAY"): day of year.
/ #sv("YEAR"): current year.
