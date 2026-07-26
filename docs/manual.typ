// manual.typ — The XyScript User Manual (testers' draft).
//
// A narrative companion to docs/reference.typ. The reference is the exhaustive, machine-tested A–Z
// catalog of every command, function, and special variable; THIS manual is the guided tour — how to
// use XyScript, with worked examples and the "why" behind the verbs. It points to the reference for
// lookups rather than duplicating it.
//
// Authored in Typst and rendered by XyScript itself (open it and TOPDF, or use the build script).
// Keep it accurate against the code; when the two disagree, trust the code.

#set document(title: "The XyScript User Manual", author: "The XyScript Project")
#set page(paper: "us-letter", margin: (x: 1.1in, y: 1in), numbering: "1")
#set text(font: "New Computer Modern", size: 11pt)
#set par(justify: true, leading: 0.62em)
#set heading(numbering: "1.1")
#show raw: set text(font: "DejaVu Sans Mono", size: 9.5pt)
#show link: set text(fill: rgb("#1a4f8a"))
#set terms(spacing: 0.6em, hanging-indent: 1.2em)

// Chapters (level-1 headings) start on a fresh page and are set large.
#show heading.where(level: 1): it => {
  pagebreak(weak: true)
  block(above: 0pt, below: 14pt, text(size: 17pt, weight: "bold", it))
}
#show heading.where(level: 2): set text(size: 12.5pt)

// A keystroke, e.g. #kbd("F9") or #kbd("Ctrl+S").
#let kbd(k) = box(
  stroke: 0.5pt + luma(140), inset: (x: 3.5pt, y: 0.5pt), radius: 2.5pt, outset: (y: 2pt),
  text(size: 9pt, font: "DejaVu Sans Mono", k),
)
// A friendly aside.
#let tip(title, body) = block(
  width: 100%, fill: rgb("#eef5ff"), stroke: 0.5pt + rgb("#bcd6f5"), radius: 4pt, inset: 9pt,
  [#text(weight: "bold")[#title] \ #body],
)
// A code / command example block.
#let ex(code) = block(
  width: 100%, fill: luma(246), radius: 3pt, inset: (x: 9pt, y: 7pt), above: 8pt, below: 8pt,
  raw(code),
)

// ---------------------------------------------------------------------------------------------------
// Cover
// ---------------------------------------------------------------------------------------------------
#align(center + horizon)[
  #text(30pt, weight: "bold")[XyScript]
  #v(6pt)
  #text(16pt)[User Manual]
  #v(2pt)
  #text(11pt, style: "italic", fill: luma(110))[A completely customizable editor for text files, with live preview for Markdown and Typst files, user-definable syntax highlighting, and a built-in rich programming language

]
  #v(40pt)
  #text(10pt, fill: luma(120))[Draft for beta testers — a work in progress]
  #v(2pt)
  #text(9pt, fill: luma(140))[Check the version in #raw("Help ▸ About") (\$VE); when this manual and the program disagree, trust the program.]
]
#pagebreak()

// Table of contents
#outline(title: "Contents", depth: 2, indent: auto)

// ===================================================================================================
= Overview and Quick Start

This chapter introduces XyScript, gets you to your first saved file, and ends with a one-line script —
enough to hint at where the depths lie before later chapters take you down into them.

== What XyScript is

XyScript is a configurable, keyboard-driven editor inspired by the classic word processor *XyWrite*. Like
XyWrite, its center of gravity is not the menu bar but the *command line* — a one-line bar, the
*CMline*, where you drive the program with short, mnemonic commands. You can use XyScript on day one as
an ordinary editor with familiar shortcuts; a few weeks in you are chaining commands on the CMline; and
before long a personal library of routines is a keystroke away.

XyScript writes plain text. It understands two markup flavors for preview and PDF export — GitHub
Flavored Markdown and Typst — but a file is always just text on disk, openable in any other editor.
Its scripting language, *XPL* (the XyScript Programming Language), descends from XyWrite's XPL but
modernizes it: plain-text scripts, readable keywords, expressions with functions and arrays, and access
to the operating system shell.

#tip("Two books in one shelf")[This manual is the *guided tour*. Its companion, the *XyScript
Reference* (#raw("Help ▸ Reference")), is the exhaustive A–Z catalog of every command, function, and
special variable, checked against the code automatically. When you want to learn *how* to do something,
read here; when you want to look up *exactly* what a verb takes, go there.]

== Running XyScript

XyScript is a desktop application. Launch the XyScript program; a single window opens with an empty,
untitled document. On first run it can create a small configuration file to remember your settings
between sessions (see the *Customization and Configuration* chapter); deleting it is harmless — it is
recreated.

From a terminal you can also pass arguments (paths are relative to the directory you launch from):

/ #raw("xyscript file1.md notes.txt"): open one or more files on startup (the first becomes the active tab). A path that does not exist opens as a new, named document, ready to save.
/ #raw("xyscript -s startup.xpl"): run a *startup script* first, then open the editor — the place to load your library, set colors, and bind keys. With no #raw("-s"), a #raw("startup.xpl") sitting beside the program is used automatically.
/ #raw("xyscript -run script.xpl"): run a script *headlessly* — no window; its #raw("PR")/#raw("MSG") output goes to the terminal and the program exits with the script's status. Good for batch jobs. (This is also how #raw("RUN/BG") runs a script in the background.)

Each flag accepts a single or double dash (#raw("-s") or #raw("--startup"), #raw("-run") or #raw("--run")).

== First launch

You will see a single window with, from top to bottom:

- a *menu bar* with eight menus: File, Edit, Search, View, Document, Tools, Settings, Help;
- a *tab strip*, one tab per open document (a fresh, unsaved document is untitled);
- the *text window* — the main editing area; and
- at the very bottom, the *command line* (the CMline) with a *status line* beneath it, where XyScript
  reports the result of what you just did (#raw("saved"), #raw("found"), an error message, a computed
  value).

Focus moves between the text and the CMline with a few short verbs — #raw("GH") (or #raw("CMLINE"))
jumps to the command line, #raw("GT") returns to the text, and #raw("TH") toggles between them. Most
users bind these to function keys in their keyboard file (see *Key Bindings*); the shipped keyboard
provides sensible defaults.

#tip("A note on conventions")[Keys are written like #kbd("F9") or #kbd("Ctrl+S"). Commands you type on
the CMline are shown like #raw("PR hello"). Anything you would type verbatim — on the CMline or in a
script — appears in a fixed-width block.]

== A ten-minute tour

We will write a short document, save it, search inside it, run a one-line command, and finish with a
small script — touching every major part of the program at least once.

=== A. Write something

Click in the text window and type a few lines:

#ex("Things to remember
==================

- Buy coffee.
- Email Alex about Tuesday's meeting.
- Read chapter 3 of the XyScript manual.")

That is just text. If you would like to see it rendered, XyScript can treat the document as Markdown:
turn on the preview pane (the *View* menu, or the #raw("PV") command) to see the formatted version
beside the source. The document type is yours to choose with #raw("DOCTYPE") — GFM, Typst, or
automatic by file extension.

=== B. Save it

Save with #kbd("Ctrl+S") (or *File ▸ Save*). A brand-new document has no name yet, so XyScript asks for
one — this is *Save As* (#raw("SAAS") on the CMline). Give it a filename and the tab takes that name.
From then on #kbd("Ctrl+S") / #raw("SAVE") writes straight to that file.

Files live wherever you put them — XyScript has no special "notes folder." The #raw("DIR") command
lists a directory right in a tab (and #raw("DIR/W") opens a movable directory window); pressing
#kbd("Enter") on an entry opens it.

=== C. Search it

Press the key that focuses the CMline (#raw("GH")), type a search, and press #kbd("Enter"):

#ex("SE /coffee/")

The cursor jumps to the first "coffee" and the status line reports the match. #raw("SE") (Search) takes
its pattern between two *separator* characters — here, slashes. Any character that is not in your
pattern works as the separator, which is handy when the text itself contains slashes:

#ex("SE !http://example.com!")

You can chain commands on one line with a semicolon. #raw("PR") (print) writes a message to the status
line:

#ex("SE /Alex/ ; PR Found a name.")

That is the first hint that the CMline is more than a search box — it is a small programming language.

=== D. Run an inline script

A *script* is just a sequence of CMline commands, one per line (or several per line, separated by
#raw(";")). The quickest way to try one is to write it into the document as a fenced code block and run
the block in place. Type this, fences and all:

#ex("```xpl
SET greeting = \"Hello, world!\"
MSG $greeting
```")

Put the cursor inside the block and run it with #raw("RUNHERE") (the *Tools* menu has it too). XyScript
recognizes the fenced block, runs it as a script, and a message box shows *Hello, world!*

#tip("Three ways to run code")[#raw("RUNHERE") runs the fenced block under the cursor. #raw("RUN file")
runs a script file; #raw("RUN/BG file") runs it in the background (its own process). And the shell-out
verbs #raw("X") / #raw("XY") run *operating-system* commands — see *The Command Line*. Shell-out is
gated by #raw("TRUST") so it can't fire by accident.]

=== E. Make it yours

If you reach for the same command often, bind it to a key in your keyboard file, or stash text and
scripts in the *binder* registers #raw("@A")–#raw("@Z") (text) and #raw("&A")–#raw("&Z") (script). The
*Key Bindings* chapter covers both. The *Settings* menu collects the appearance and behavior knobs —
colors, fonts, autosave — and can save them so the program opens the way you left it.

== What's next

That was the tour. The rest of the manual takes each piece in turn:

- the window and menus in detail (*The Interface*);
- moving, selecting, and editing (*Editing*);
- opening, saving, directory listings, and encodings (*Files, Tabs, and Directories*);
- the search-and-replace family (*Find and Replace*);
- the command line end to end (*The Command Line*);
- the XPL language — variables, control flow, functions, counters, the error model (*XPL: The
  Scripting Language*);
- your own routine collections (*User Libraries*);
- the keyboard file and binders (*Key Bindings*); and
- colors, fonts, and configuration (*Customization and Configuration*).

If you are learning XyScript from scratch, read the chapters in order — they build on one another. If
you are back to look something up, the *Reference* (#raw("Help ▸ Reference")) and the appendices here
are the fastest way in.

// ===================================================================================================
= The Interface

The previous chapter touched every major feature once. This one slows down and gives each part of the
window a proper introduction: what is on screen, what each menu offers, how tabs behave, and where the
command line lives. Most parts get a dedicated chapter later; this is orientation.

== The main window

From top to bottom you will find:

- the *menu bar* (eight menus, below);
- the *tab strip* — one tab per open document; an unsaved document shows a modified marker;
- the *text window* — the editing area; and
- the *command line* and *status line* across the bottom.

Several optional elements appear when you turn them on, each from the *View* menu and each with a
command: a *preview pane* beside the editor (#raw("PV")), an interactive *shell pane* (#raw("SH")) and
the *Shell Output* tab (#raw("SW")), *line numbers* (#raw("SLN")), visible *paragraph marks*
(#raw("SVR")) and *whitespace* (#raw("SWS")), and the menu bar itself (#raw("SMB")). A separate movable
*directory window* opens with #raw("DIR/W").

For distraction-free writing, #raw("WV") — *Writer's View* — hides everything at once: the title bar,
menu bar, tab strip, command line, and status line, leaving just the text. Press #raw("Esc") to bring
them all back. It is a passing mode, never saved, so you always start a session with your normal
layout.

== The menu bar

A one-line orientation to each of the eight menus. Every item simply runs an ordinary XyScript command —
the menus are a convenience layer over the CMline, never a separate set of features.

The menus respond to mouse and keyboard interchangeably. Click a menu to open it; once any menu is open —
by mouse or keyboard — the arrow keys navigate it (Left / Right between menus, Up / Down through items),
#kbd("Enter") opens a submenu or runs an item, and #kbd("Esc") backs out or closes. To reach the menus
without the mouse, run #raw("MENU") (bind it to a key for a mouse-free bar). Each menu and item carries one
_underlined accelerator letter_: #raw("MENU \"f\"") opens File directly, and while a menu is open, typing
an item's accelerator letter jumps to and runs it.

/ File: New, Open, Open read-only, Revert, Save, Save As, Save Selection As, Save All, Export PDF,
  Close, Quit. Every one has a CMline verb (covered in *Files, Tabs, and Directories*).
/ Edit: Undo / Redo; the system clipboard (Cut, Copy, Append, Prepend, Paste); Select All and Delete;
  the in-document Copy-to / Move-to; change case; and Insert file.
/ Search: the Find / Replace dialog (and #kbd("F3") / #kbd("Shift+F3") for find-next / find-previous).
  The underlying verbs — #raw("SE"), #raw("CH"), #raw("CV") — are the subject of *Find and Replace*.
/ View: the pane and marker toggles listed above, plus command-line focus and placement.
/ Document: per-document settings — document type (GFM / Typst / auto), text encoding (display and
  convert), line endings, read-only, and word / character counts.
/ Tools: run a script (foreground or background), run the block under the cursor, a one-off shell
  command, background-job control, and your library (load / reload / unload / list).
/ Settings: application-wide preferences — colors, fonts, UI zoom and spacing, syntax-highlighting
  defaults, the keyboard file, "allow shell commands" (#raw("TRUST")), and saving / loading settings.
/ Help: About (name, version, OS), the embedded *Reference*, and a chart of your current keyboard.

#tip("Document vs Settings")[The boundary is deliberate: *Document* holds choices that belong to the
file in front of you (its type, encoding, line endings); *Settings* holds program-wide preferences that
persist between sessions.]

== Tabs

Each open document lives in its own tab. Click a tab to switch to it; an unsaved document is marked.
Opening a file, starting a new one, and listing a directory all open tabs. The CMline mirrors the tab
operations — #raw("AB") closes the active document, for instance — so a script can manage tabs without
the mouse. Recently closed files can be brought back with #raw("REOPEN") (a picker) or #raw("REOPEN n")
(the most recent `n`, no picker — #raw("REOPEN 1") for just the last).

== Split panes

Tabs show one document at a time; *split panes* show two (or more) at once — the same file in two
places, or two different files side by side. Split the current pane with #raw("TVS") (side by side, left
and right) or #raw("THS") (stacked, top and bottom); the menu *View ▸ Split panes* has both, plus
everything below. The new pane shows the file you name (#raw("TVS notes.txt")), or your *alternate*
buffer (the #raw("BUFALT") target) if you name none, or a fresh empty buffer if there is no alternate.
To split straight into a brand-new empty buffer, use #raw("NEW/V") or #raw("NEW/H"); to open a file
directly into a split, add #raw("/V") or #raw("/H") to #raw("OPEN") / #raw("READ") / #raw("DIR"). A
document can only appear in one pane at a time.

The focused pane wears a colored border (set its color with #raw("PANECOLOR")). Click a pane to focus
it, or step around with #raw("TW") — *next pane*, wrapping around; your typing, the caret, and the
content specials all follow the focus. Drag the divider between panes to resize them, and #raw("TRV")
swaps the two panes over.

To put a split away again there are three moves, depending on what you want to keep:

/ #raw("TUS"): *un-split* — remove the focused pane and give its space back to its neighbour. The pane's document stays open as a tab; you have only stopped *showing* it in a pane.
/ #raw("FS"): *full screen* — collapse the whole layout down to the pane you are in (the other panes' files stay open as tabs). Where #raw("TUS") keeps the neighbour, #raw("FS") keeps you.
/ #raw("TZ"): *zoom* — temporarily show the focused pane full-window and hide the rest; press again to bring the split back. Nothing is closed, so it is the quick "let me see this one full size" toggle.

Closing a file with #raw("AB") also undoes its pane in the same step (the space returns to the
neighbour) — so #raw("AB") means "close the file *and* its pane," while #raw("TUS") means "keep the
file, drop the pane." For scripts that adapt to the layout, #raw("$PANES") reads how many panes are
open and #raw("$PANE") which one has focus.

#tip("A two-pane workspace")[Bind a key to #raw("TVS") and another to #raw("TUS") (or #raw("TZ")) and
you can flick a reference file in beside your work and back out without leaving the keyboard. The *Shell
Output* tab can take a pane too — #raw("SW/V") / #raw("SW/H") — so a build's output sits right next to
the code that produced it.]

== The text window

The editing area behaves the way you expect from a modern editor: arrow keys and Home / End / Page Up /
Page Down to move, #kbd("Shift")+motion to select, the usual #kbd("Ctrl+X") / #kbd("Ctrl+C") /
#kbd("Ctrl+V") / #kbd("Ctrl+Z") for cut / copy / paste / undo — all subject to your keyboard file,
which can remap anything. The *Editing* chapter covers the XyScript-specific verbs that make the
keyboard fast: terse two-letter motions, the DEFINE selection verbs, and persistent selection.

== The command line and status line

Two rows are the heart of XyScript. The *CMline* (by default, near the top of the screen, just below the tabs pane) is where you type commands; the
*status line*, at the bottom, is where XyScript replies. A few things worth knowing up front:

- Commands are *case-insensitive*: #raw("se /foo/") and #raw("SE /FOO/") are the same verb. (Whether
  the *contents* of a search are case-sensitive depends on the verb — see *Find and Replace*.)
- Multiple commands chain on one line with #raw(";").
- A #raw("#") begins a comment.
- Past commands are recalled with the up / down arrows, or browsed with #raw("HISTORY") (#raw("HI")).
- By default a command *stays* on the line after it runs, so you can re-run or edit it (XyWrite-style);
  #raw("CMBLANK on") clears the line after each command instead (whether run with #kbd("Enter") or #raw("XC")).

Whether you submit with #kbd("Enter") or execute the CMline's contents from elsewhere with #raw("XC"),
the command runs against the document. The full story — focus, the optional pop-up command line, staging
with #raw("BC")/#raw("XC"), variable substitution, and shell-out — is in *The Command Line*.

// ===================================================================================================
= Editing

This chapter covers the day-to-day mechanics of working in the text window: moving the cursor,
selecting, deleting, changing case, and *persistent selection* — a XyWrite habit that lets a selection
survive cursor movement. Each section pairs the familiar keystroke with its CMline verb. For everyday
editing the keyboard is usually enough; the verb form earns its keep once you start writing scripts and
key bindings.

== Moving the cursor

The standard keys behave as you would expect — arrows, Home / End, Page Up / Page Down, and
#kbd("Ctrl")+arrow for word-by-word — all subject to your keyboard file, which can remap anything. The
CMline motions are short, mnemonic, two-letter verbs in the XyWrite tradition. The ones you will reach
for most:

/ #raw("CL") / #raw("CR"): one character left / right (gliding over a CRLF line ending as a single step, so the cursor never gets stuck inside a line break)
/ #raw("LU") / #raw("LD"): one line up / down, keeping your column where it can
/ #raw("NW") / #raw("PW"): next / previous word
/ #raw("NS") / #raw("PS"): next / previous sentence
/ #raw("NP") / #raw("PP"): next / previous paragraph
/ #raw("EL") / #raw("ER"): start / end of the line (the "Express Left / Right" names; also Home / End)
/ #raw("LB") / #raw("LE"): start / end of the visual row — useful when a long line has wrapped on screen
/ #raw("LL") / #raw("LR"): one character left / right but never crossing to another line
/ #raw("PU") / #raw("PD"): up / down by one screenful (the view scrolls to follow)
/ #raw("HM") / #raw("BS"): to the top / bottom of what is currently on screen
/ #raw("TF") / #raw("BF"): to the very top / bottom of the file (the XyWrite "Top / Bottom of File")
/ #raw("JMP"): jump to a character position — #raw("JMP 0") for the start, or a signed number to move relative to where you are (#raw("JMP +20"), #raw("JMP -20"))

#tip("Selecting on the move")[Every motion can also *extend a selection*. Follow it with #raw("SEL") and
the cursor drags a highlight along with it as it moves — the keyboard equivalent of holding
#kbd("Shift"). So #raw("NW SEL") selects the next word, #raw("BF SEL") selects to the end of the file.]

When a script needs to remember a spot and return to it, #raw("$CP") is the cursor's character offset
and #raw("$LN") is its line number.

== Selecting text

In XyWrite you "defined" a block, and XyScript keeps the name. Press #raw("DF") to drop one end of a
*defined selection*, then move the cursor — the highlight follows, no #kbd("Shift") needed. #raw("DF")
again (or #raw("DZ")) closes it; #raw("XD") throws it away. There are shortcuts for the common shapes:

/ #raw("DW"): the word under the cursor
/ #raw("DL"): the current line
/ #raw("DS"): the sentence
/ #raw("DP"): the paragraph
/ #raw("DFA"): the whole document

The mouse works too — click and drag, double-click for a word, triple-click for a line. The current
selection's text is available two ways: the variable #raw("$SE") substitutes it into a command (handy
for short, simple selections), and the expression function #raw("SEL()") returns the same text as a
proper string value, so it survives selections that contain quotes or backslashes.

To capture a whole tab at once — no selecting required — read #raw("$DOC"), which returns the entire
buffer: #raw("SET all = $DOC"). Alongside it, #raw("$LI") is the current line, #raw("$WD") the word at
the caret, and #raw("$CH") the character just to the right.

== Inserting text

Most text you simply type. But XyScript also has verbs that *put* text into the document at the cursor
— handy on a key binding or in a script, where no one is at the keyboard to type it:

/ #raw("INSERT"), #raw("WRITE"), #raw("WR"): insert literal text at the cursor; a quoted argument processes the usual escapes (#raw("\\n"), #raw("\\t"), …).
/ #raw("WRITELN"), #raw("WRL"): the same, then a trailing newline in the document's line-ending style.
/ #raw("PUT"): insert text too, but taken *raw* (after #raw("$")-substitution), so #raw("PUT $name") drops the variable's value in with no quoting.
/ #raw("ENTER"): insert a newline in the document's line-ending style — this is what the Enter key is bound to. #raw("TAB"): indent the line or selection (see *Coding aids*, below); with nothing selected it inserts one level of indentation.

#ex("INSERT \"Dear \"
PUT $name
WRITELN \",\"")

== Counters

When you are composing an outline or a numbered list, *counters* save you from renumbering by hand.
XyScript keeps nine — #raw("C1") through #raw("C9") — each an independent register that *puts* its
current value into the document and then advances. Configure one by giving it a starting value wrapped
in whatever decoration you want around it; a bare call then puts the value and steps to the next:

#ex("C1 \"1. \"         # configure: produces  1.   2.   3.   ...
C1                # puts \"1. \" and advances
C1                # puts \"2. \" and advances")

The type follows the seed — a number (#raw("C1 \"1. \"")), a letter (#raw("C2 \"(a) \"") gives
#raw("(a) "), #raw("(b) "), …), or a Roman numeral (the marker #raw("ir"), so #raw("C3 \"ir) \"") gives
#raw("i) "), #raw("ii) "), …). The decoration — the spaces, dots, and parentheses around the value — is
part of the seed and comes back with each put. The relative forms nudge a sequence without
reconfiguring: #raw("C1 +2") jumps ahead, #raw("C1 -") steps back, #raw("C1 =") repeats the last value;
and #raw("$C1")…#raw("$C9") peek the next value without advancing. Counters are per-document, so every
file keeps its own.

#tip("Self-numbering lists")[Bind a counter to a key (see *Key Bindings*) and your list numbers itself:
tap the key for the next label, type the item, press #kbd("Enter"), tap again. The relative forms reset
nothing by hand when an item moves, is removed, or repeats.]

== Coding aids

Writing code — or any structured, indented text — is smoother with a handful of optional helpers. Each
is a simple #raw("on") / #raw("off") toggle you turn on when you want it and #raw("SaveCfg") remembers;
all are off by default, so ordinary prose editing is unchanged.

*Indenting.* Bound to the Tab key, #raw("TAB in") indents and #raw("TAB out") dedents. With several
lines selected they shift the whole block at once (a single undo step); with nothing selected,
#raw("TAB in") just inserts one level of indentation at the cursor. A "level" is one tab character — or,
with #raw("TABSPACES on"), spaces instead (soft tabs), so the file holds spaces rather than tabs.
#raw("$TB") is the tab width you set with #raw("TB"); at the cursor a soft tab pads to the next tab
stop, adding only as many spaces as it takes to reach it — so a Tab lands the text on the stop instead
of overshooting when some spaces are already there.

/ #raw("TABSPACES"): soft tabs — indenting inserts #raw("$TB") spaces rather than a tab character.

*Auto-indent.* With #raw("TABINDENT on"), pressing #kbd("Enter") starts the new line at the same
indentation as the line you just left, so you keep your place down a nested block without re-tabbing.
#raw("$INDENTED") reads the current line's indentation in columns, should a script want it.

*Hanging indent for wrapped lines.* When a long line wraps on screen, its continuation normally returns
to the left margin — which buries the shape of indented code. #raw("WRAPINDENT on") makes those wrapped
rows hang under the line's own indentation instead, so a deeply-indented passage stays visually
together. It changes only what you see; the file's bytes are untouched.

/ #raw("TABINDENT"): auto-indent — copy the previous line's indentation on #kbd("Enter").
/ #raw("WRAPINDENT"): hang a wrapped line's continuation rows under its own indentation.

*Matching brackets.* #raw("BALPAIRS on") highlights the partner of whatever bracket — #raw("()"),
#raw("[]"), or #raw("{}") — the cursor sits on or just past, counting nesting so it lands on the *right*
partner. A bracket with no partner is noted on the status line rather than highlighted. The highlight
borrows your selection color by default; #raw("BALPAIRSCOLOR") (or the #raw("BALPAIRSCOLORS") picker)
gives it a color of its own.

*Auto-closing pairs.* #raw("TYPEPAIRS on") finishes what you start: type an opening #raw("("),
#raw("["), #raw("{"), #raw("\""), or #raw("'") and its partner appears with the cursor between the two.
Type the closer yourself and it steps over the one already there instead of doubling it; type an opener
while text is selected and it wraps the selection; and #kbd("Backspace") between an empty pair clears
both halves. A quote typed right after a letter is left alone, so an apostrophe in prose is never
paired. This applies to what you type by hand — the scripted #raw("INSERT") verb is never auto-paired.

/ #raw("BALPAIRS"): highlight the bracket matching the one at the cursor.
/ #raw("TYPEPAIRS"): auto-close typed brackets and quotes.

#tip("Turn them on for good")[Drop the toggles you like into your startup file or a key binding — say
#raw("TABINDENT on"), #raw("WRAPINDENT on"), #raw("BALPAIRS on"), #raw("TYPEPAIRS on") — and they greet
you every launch. #raw("SaveCfg") writes the current set into your config file.]

*Emphasis.* #raw("EMPH cat") paints every whole-word occurrence of _cat_ in the current tab — matched
case-insensitively, so #raw("CAT") and #raw("cAt") light up too, while the _cat_ buried in _concatenate_
is left alone. Need more reach than a plain word? #raw("EMPH/R <regex>") emphasizes everything a regular
expression matches — #raw("EMPH/R \\d+") lights up every run of digits — and a malformed pattern is
reported rather than quietly matching nothing. It is a standing highlight, not an edit,
so it works even where you cannot type: a read-only tab, or the #raw("*Shell Output*") pane. Each tab
keeps its own emphasis, and a bare #raw("EMPH") clears the current tab's. You can also reach it from
#raw("Search ▸ Emphasize"), where _Selection_ emphasizes the highlighted word in one click and the
_Word or phrase…_ / _Regular expression…_ items stage the command for you to finish typing. The colors default to your
selection colors; #raw("EMPHFGCOLOR") and #raw("EMPHBGCOLOR") (or their #raw("EMPHFGCOLORS") /
#raw("EMPHBGCOLORS") pickers) give the emphasis a look of its own, and #raw("… default") hands it back to
the selection colors. Read the effective colors as #raw("$EMPHFGCOLOR") / #raw("$EMPHBGCOLOR").

== Deleting

XyScript keeps two independent stashes: the system *clipboard* (Cut / Copy / Paste) and the XyWrite
*deletion buffer*. The deletion verbs use the latter:

/ #raw("RD"): delete the selection into the deletion buffer; #raw("UD") pastes it back (an "undo delete" / move idiom)
/ #raw("BD") / #raw("RC"): rub out the character *before* / *after* the cursor (Backspace / Delete)
/ #raw("RW") / #raw("RL") / #raw("RS") / #raw("RP"): clear the word / line / sentence / paragraph at the cursor

#tip("Two stashes, not one")[The deletion buffer (read as #raw("$XB")) and the system clipboard (read
as #raw("$CB")) are separate. #raw("RD") then #raw("UD") moves text without disturbing whatever you have
on the clipboard — exactly the XyWrite delete-and-restore gesture.]

== Changing case

Three verbs change the case of the selection:

/ #raw("UC") / #raw("LC"): upper-case / lower-case
/ #raw("CC"): swap the case of each letter

With *no* selection, each acts on the single character under the cursor and then steps one character
right — so you can sweep along a word, flipping characters as you go, by pressing the key repeatedly.

== Persistent selection

Normally a selection is *transient*: any cursor movement that does not hold #kbd("Shift") drops it, and
typing over it replaces it. In *persistent-selection* mode the selection sticks until you cancel it
explicitly — you can move around, edit elsewhere, and come back to find it still there. Copy and cut
still act on the block (that is the point of those keys); everything else leaves it alone.

/ #raw("SPS") / #raw("CPS") / #raw("TPS"): set / clear / toggle persistent selection
/ #raw("$PS"): reads `1` when on, `0` when off

A routine that needs persistent selection only briefly can save and restore the state around its own
work:

#ex("SET saved = $PS
SPS
# ... operations that rely on the selection sticking ...
IF $saved == 0
  CPS
EI")

== Cut, copy, paste, undo, redo

The clipboard verbs — #raw("CUT"), #raw("COPY"), #raw("PASTE") — are the usual #kbd("Ctrl+X") /
#kbd("Ctrl+C") / #kbd("Ctrl+V") by default. Two more *extend* the clipboard instead of replacing it:
#raw("APPEND") adds the selection to the end of the clipboard and #raw("PREPEND") to the front, so you
can gather several pieces before pasting. #raw("UNDO") and #raw("REDO") step through the edit history;
a sweeping change such as a global #raw("CH") collapses to a single undo step, so one #raw("UNDO")
rolls the whole pass back.

// ===================================================================================================
= Files, Tabs, and Directories

This chapter is the on-disk view of XyScript: opening and saving files, listing directories, juggling
tabs, and the two things that matter for legacy text — *encodings* and *line endings*. The keyboard
shortcuts and menu items were introduced earlier; here is the substance behind them, with the CMline
verbs you would use from a script.

== Where your files live

XyScript has no special "notes folder." A file lives wherever you put it, and paths are resolved
against your *working directory* — shown on the status line and readable as #raw("$CWD"). Change it with
#raw("CD"); on Windows a bare drive letter (#raw("CD D:")) switches drives. Relative paths are relative
to #raw("$CWD"); absolute and `~`-style paths work too.

== Opening and creating

/ #raw("NEW"), #raw("NE"): start a fresh, empty document. With a name (#raw("NEW report.typ")) the new document is named and ready to save — or, if that file already exists, it is simply opened.
/ #raw("OPEN"): open an existing file in a tab, editable. (#raw("CA"), #raw("ED"), #raw("EDIT") are synonyms.)
/ #raw("READ"): open a file *read-only*, so you can look without risk of changing it.

Opening a file leaves the cursor in the text, ready to edit. If the file is already open, #raw("OPEN")
asks what you want — open a second copy, switch to the copy already open, or open a read-only view.
#raw("READ") skips that question and always opens a fresh read-only copy, so you can keep several views
of the same file open at once.

== Saving

/ #raw("SAVE"): write the active document back to its file. A new, never-saved document has no name yet, so XyScript asks for one — that is *Save As*.
/ #raw("SAAS"): save under a new name and adopt it (#raw("SAAS draft2.md")). If that name is a different existing file you are asked before overwriting; #raw("SAAS/NV") overwrites without asking.
/ #raw("SAD"): save the current *selection* to a separate file. If the target already exists you are asked before overwriting; #raw("SAD/NV") overwrites without asking.
/ #raw("ST"): "store" — save the file and close its tab in one step (the XyWrite habit). #raw("ST report.md") saves to (and names) a given path — the way to store an untitled document; if that path already exists you are asked before overwriting, and #raw("ST/NV report.md") overwrites without asking. A bare #raw("ST") on a never-saved document has no name to save to, so the tab is left open rather than discarded.
/ #raw("SL"): save every open document at once.
/ #raw("REC"): revert the document to its last saved state (#raw("RECALL") is the long form), discarding changes since the last save.

#tip("Two ways to read your file's identity")[#raw("$FI") is the active document's file name and
#raw("$FP") its full path — handy when a script needs to act on the file it is editing, for example
passing #raw("$FP") to a shell command.]

== Closing and quitting

/ #raw("AB"): close ("abort"/"abandon") the active document immediately, discarding changes. #raw("AB/V") ("verify") prompts you to save or discard unsaved changes, or cancel.
/ #raw("QUIT"): leave the program, offering to save anything unsaved (#raw("QUIT/NV") to skip the prompts).

== Tabs

Each open document rides in a tab along the top; click one to switch, and an unsaved document is
marked. #raw("BUFNEXT") and #raw("BUFPREV") step between tabs from the keyboard or a script, and
#raw("BUFGO") jumps to one by number or exact name. To reach a tab by part of its path, #raw("GOFILE")
(#raw("GOF")) switches to an open file whose full path contains the text you give — #raw("GOF report") —
cycling to the next match (and through several copies of one file) on each call; it stays put if the
only match is the current tab, and sets #raw("$ERROR") if nothing matches. Closed a file by mistake?
#raw("REOPEN") pops a picker of recently-closed files (restoring the cursor where you left it), and
#raw("REOPEN n") reopens the most recent `n` without asking (#raw("REOPEN 1") for just the last one).

When you launch with nothing to open, XyScript starts you on a single blank _Untitled_ tab. By default
that scratch tab steps aside the moment you open a real document — and quietly reappears once you close
the last one — so it never clutters the strip. Prefer to keep it around as its own tab? #raw("UNTITLEDTAB
off") turns the auto-close off (#raw("UNTITLEDTAB on") restores it, bare #raw("UNTITLEDTAB") toggles);
#raw("$UNTITLEDTAB") reads the state and #raw("SaveCfg") remembers it. A companion toggle,
#raw("SHOWONETAB on"), keeps the tab strip visible even when only one document is open — it is hidden
for a lone document by default, to keep a single-file view uncluttered.

== Directory listings

#raw("DIR") lists a folder right inside a tab; #raw("LS") is a synonym, and #raw("DIR *.txt") filters by
a file mask. Inside a listing, arrow keys move the highlight and type-to-find jumps to the first entry
whose name *starts with* what you type (#kbd("F3") steps to the next such name, #kbd("Backspace")
shortens the query); #kbd("Enter") (or a double-click) opens the entry — descending into a sub-directory
or opening a file. #raw("DIR/W") puts the same browser in its own movable window that floats beside your
work; there, the matched leading portion of each name is highlighted as you type, using your text-selection
colours (#raw("SELFGCOLOR") / #raw("SELBGCOLOR")).

Two toggles tune how a listing behaves, each with a read-back:

/ #raw("DIRNW"): when on, opening a file from an in-tab listing lands it in a *new* tab and leaves the listing in place; off (the default) replaces the listing. Reads as #raw("$DIRNW").
/ #raw("DIRWCLOSE"): when on, the #raw("DIR/W") window closes as soon as you open a file from it; off (the default) leaves it open so you can carry on picking. Descending into a sub-directory never closes it. This is independent of #raw("DIRWNW") — one decides whether the *picker* stays, the other which *tab* the file lands in. Reads as #raw("$DIRWCLOSE").
/ #raw("DIRSET"): set the switches applied to *every* #raw("DIR") by default, so you don't retype them — for example #raw("DIRSET /OD/R") to have every listing sort by date, newest first. A switch you put on a particular #raw("DIR") overrides the matching default (a #raw("/O") on the command replaces the default order but leaves an unrelated #raw("/R") in place), and switches in an unrelated group leave the defaults untouched. Only the *how it looks* switches count — order (#raw("/O")), attributes (#raw("/A")), reverse (#raw("/R")), recurse (#raw("/S")), bare (#raw("/B")); #raw("/W"), #raw("/H"), and #raw("/V") (where the listing opens) aren't accepted, though the defaults do apply to a #raw("DIR/W") window too. #raw("DIRSET off") clears it; bare #raw("DIRSET") reports the current value. Reads as #raw("$DIRSET").
/ #raw("DIRWNW"): the same choice for the separate #raw("DIR/W") window — on (the default) opens the picked file in a new tab, off reuses the active tab. It is separate from #raw("DIRNW") because #raw("DIR/W") floats beside your work rather than occupying a tab of its own. With off, a file you have *unsaved changes* in is never replaced; the picked file opens beside it. Reads as #raw("$DIRWNW").
/ #raw("DIRRT"): when on, a #raw("DIR") command *reuses* an open listing tab instead of spawning a new one each time. Reads as #raw("$DIRRT").

#tip("Right-click copies a path")[In any listing — in-tab or the #raw("DIR/W") window — right-clicking
an entry copies its full path to the clipboard, the mouse equivalent of the listing's copy-path key.]

=== Searching across files

#raw("GREP cat") searches every file under the current folder (recursively, skipping what your
#raw(".gitignore") hides and binary files) and drops the matches into a read-only tab, one
#raw("path:line:col: text") line each. The search runs in the background and streams results in as it
finds them, so even a big tree never freezes the editor — the status line shows a running count, and
starting another #raw("GREP") (or closing the tab) cancels one still in progress. It's a literal search by default; #raw("/R") makes the pattern a
regular expression, #raw("/I") ignores case, #raw("/W") matches whole words — the same letters as
#raw("SE") and #raw("FIND"). Add a file-name mask to narrow it (#raw("GREP cat *.rs")), and quote a
pattern with spaces (#raw("GREP \"foo bar\"")).

To open a result, put the caret on its line and run #raw("GJ") — it opens that file (or switches to it if
it's already open, without asking) and jumps the caret right onto the match. Think of the results tab as a
launch pad: #raw("GREP") to gather, #raw("GJ") to pounce.

== Encodings

A file on disk is *bytes*; an *encoding* is the lens that turns those bytes into the characters you see
(and your typing back into bytes). XyScript ships a catalog of encodings — UTF-8 (the default), the DOS
code pages CP437, CP850, CP852, CP857, CP866, CP775, Windows-1252, Latin-1, Latin-9, and Mac OS Roman —
reachable two ways from the *Document ▸ Text encoding* menu or the CMline:

/ #raw("ENCODING <name>"): change how the current bytes are *displayed* — re-decode the view, leaving the file's bytes untouched. #raw("$EN") reports the active encoding.
/ #raw("ENCODING/C <name> [outfile]"): *convert* — re-encode the data to a new charset (the bytes change; anything the target can't represent becomes `?`). With a filename it writes a converted copy and opens it; without one it converts in place.

#tip("XyWrite files and CP866X")[XyWrite marks embedded commands with the guillemets #raw("«»"). For
Cyrillic XyWrite documents there is a manufactured variant, #raw("cp866x"): it is CP866 with #raw("«")
and #raw("»") restored to bytes 174 / 175 (where XyWrite expects them) and the two displaced Cyrillic
letters relocated, so old XyWrite Cyrillic files read correctly. An unrecognized byte in any single-byte
encoding is preserved intact through an edit-and-save, never silently dropped.]

== Line endings

#raw("NEWLINE lf") and #raw("NEWLINE crlf") convert the document's line endings and adopt that style
for new lines; #raw("$NEWLINE") reads the current one (`LF` or `CRLF`). The conversion is lossless and
independent of the text encoding. New documents follow your operating system's convention until you open
a file with a different one.

== Read-only and autosave

#raw("RO") locks the active document against edits (a read-only tab refuses editing keystrokes);
#raw("$RO") reads the state, and #raw("READ") opens a file locked from the start. Autosave, when on,
writes the active document quietly after a brief idle period so you can't lose more than a few seconds
of work; #raw("AUTOSAVE <seconds>") sets the interval (or #raw("AUTOSAVE off")), and #raw("$AU") reads
it.

// ===================================================================================================
= Find and Replace

XyScript's search and replace follow XyWrite's terse, separator-delimited syntax. The *Search* menu and
its Find / Replace dialog drive the same machinery, but the CMline form is where the power lives: you
can combine switches, chain operations, and make the result the first step of a script.

== The three families

/ Search — #raw("SE") (forward), #raw("SEB") (backward): find the next match and select it.
/ Change — #raw("CH"), #raw("CI"): replace every match from the cursor onward in one pass (#raw("CI") is a synonym).
/ Change-Verify — #raw("CV"): walk the matches one at a time, approving each.

Within each family, a trailing #raw("A") makes the match *case-absolute* (case-sensitive): #raw("SEA"),
#raw("CHA") / #raw("CIA"), #raw("CVA"). The bare forms are case-insensitive.

== The self-delimiting argument

Every verb in these families takes its argument between *separator* characters. The first character
after the verb (and any switches) is the separator; the rest is the pattern. The one rule is that the
separator must not appear in the pattern itself — so when your text contains slashes, pick another
character:

#ex("SE /coffee/
SE !http://example.com/path!
SE ,a/b/c,")

Change takes two halves — search and replacement — delimited the same way:

#ex("CH /color/color/
CH ,old/path,new/path,")

== Switches

Switches attach to the verb, before the separator, and combine in any order:

/ #raw("/W"): whole-word match — surrounding characters must be non-word characters.
/ #raw("/R"): treat the search half as a *regular expression* (see Appendix B). The replacement half is still taken literally except for its backreferences.
/ #raw("/T"): from the *top* — start the sweep at the beginning of the file (or the end, for a backward search) rather than at the cursor.
/ #raw("/S"): confine the search or replace to the current *selection*.
/ #raw("/K"): *keep case* (change only) — recase each replacement to match the text it replaces, so #raw("CH/K /alligator/crocodile/") turns `alligator`, `Alligator`, and `ALLIGATOR` into `crocodile`, `Crocodile`, and `CROCODILE`. A mixed-case match takes the replacement exactly as written.

#ex("SE/W /the/
SEA/W/T /TODO/
CH/W /color/color/
CH/T/R /\\n\\n+/\\n\\n/")

The first finds `the` as a whole word, skipping `there`. The second is case-sensitive, whole-word, from
the top. The third changes `color` to `color` only as a whole word. The fourth is a regular-expression
replace that collapses any run of two or more blank lines down to one, in a single pass.

== After a match

A *search* leaves the match selected, the cursor at its far edge, and reports the result on the status
line; a miss sets #raw("$ERROR"). Crucially, a search is a *single sweep* in the verb's direction —
cursor to end-of-file forward, cursor to top backward — and does *not* wrap around. This is what lets a
script call #raw("SE") repeatedly until #raw("$ERROR") is set and visit every match exactly once before
stopping. Use #raw("/T") to force the sweep to cover the whole file.

A *change* reports how many replacements it made; a zero-count change is a trappable failure (it sets
#raw("$ERROR")). The whole pass collapses to a single undo step, so a runaway replace is one
#raw("UNDO") away. A *change-verify* (#raw("CV")) highlights each match in turn and asks whether to
replace it, skip it, replace it and all the rest, or stop — the on-screen prompt lists the keys.

== The Find dialog

If you prefer a dialog, *Search ▸ Find* (or #raw("FIND")) opens a non-modal Find / Replace window with
fields and checkboxes for the same options; #kbd("F3") and #kbd("Shift+F3") repeat the last search
forward and backward, whether or not the dialog is open.

As you type in the *Find* field the view follows along — no need to press #kbd("Enter") or #kbd("F3").
Every match is highlighted, and each letter you add jumps to the next match ahead of the cursor,
scrolling it to mid-screen if it was off-screen. Press #kbd("Backspace") and it jumps back to the
*first* match of the now-shorter text, so widening the search returns you to the top.

== Worked examples

*Step through every TODO.* Land on the first, then repeat the search to walk forward; the cursor stops
on each in turn and #raw("$ERROR") tells a script when there are no more:

#ex("SE/T /TODO/")

*Rename a variable, case-sensitively, checking each.* Case-absolute matters so you don't touch
#raw("Old_Name") or #raw("OLD_NAME"); #raw("CVA") walks you through:

#ex("CVA/T /old_name/new_name/")

*Collapse blank-line runs.* With #raw("/R") it is a one-shot replace — the pattern matches two or more
newlines and the replacement puts back exactly two:

#ex("CH/T/R /\\n\\n+/\\n\\n/")

// ===================================================================================================
= Checking Spelling and Grammar

XyScript can check your writing as you type, underlining suspect words and phrases, and it keeps a
personal dictionary so your own vocabulary stops being flagged. The checker runs on a background
thread, so it never interrupts your typing.

== Turning it on

Spelling and grammar are two independent switches:

/ #raw("SPELLING on") (or #raw("SP on")): underline misspelled words.
/ #raw("GRAMMAR on") (or #raw("GR on")): underline grammar and word-choice issues.

Turn either off the same way, and #raw("$SPELLING") / #raw("$GRAMMAR") read the current state. From
the menus everything in this chapter is under #raw("Tools ▸ Language check"). Put #raw("SPELLING on")
in your startup file if you want it always.

== What the underlines look like

A misspelling gets a red underline, a grammar issue a blue one. Both the color and the *shape* are
yours to set: #raw("SPELLSTYLE") and #raw("GRAMMARSTYLE") take #raw("dotted") (the default),
#raw("solid"), or #raw("squiggle") — each also in a heavier weight, #raw("dotted-thick"),
#raw("solid-thick"), or #raw("squiggle-thick") (you can also just say #raw("SPELLSTYLE thick squiggle")).
#raw("SPELLCOLOR") and #raw("GRAMMARCOLOR") (or the #raw("…COLORS") pickers) change the colors. Pick a
style from #raw("Tools ▸ Language check ▸ Spelling style") too.

== Moving between issues, and fixing them

/ #raw("SPELLNEXT") / #raw("SPELLPREV"): jump to and select the next / previous spelling issue.
/ #raw("GRAMMARNEXT") / #raw("GRAMMARPREV"): the same for grammar issues.
/ #raw("LINTFIX"): correct the issue at the cursor. With a single suggestion it applies it; with
  several it opens a chooser so you pick the right one.

The step verbs scan past what's on screen when they need to, and tell you when nothing is found nearby.
A natural rhythm is #raw("SPELLNEXT") to land on a flagged word, then #raw("LINTFIX") to mend it.

== Your personal dictionary

When the checker flags a name or term that is perfectly good, teach it:

/ #raw("LEARN <word>"): add a word so it stops being flagged. #raw("UNLEARN <word>") removes it again.

#raw("LEARN") also takes a whole *list* at once, from any of three sources: several words on the line
(#raw("LEARN Anthropic Claude XyScript")), every word in the current selection (#raw("LEARN/S") — one
per line or a run of text), or the words listed in a file (#raw("LEARN/F <path>")). Each is split on
whitespace and stripped of clinging punctuation, then added in a single pass. There is a
#raw("Tools ▸ Language check ▸ Learn selected words") menu item for the selection form.

Your dictionary is saved to #raw("xyscript-dict.txt") beside the program and reloaded every launch, so
it grows with you.

#tip("Learn a batch in one go")[Paste your proper nouns into a scratch tab, one per line, select them,
and run #raw("LEARN/S"). Or keep a word list on disk and load it with #raw("LEARN/F names.txt"). Either
way the whole list is learned at once.]

== Dialects

Spelling follows a dialect: #raw("DIALECT american") (the default), #raw("british"), #raw("canadian"),
#raw("australian"), or #raw("indian"). #raw("$DIALECT") reads it back, and the choice persists.

== A one-shot report

When you would rather see everything at once than hunt underline by underline, #raw("CHECK") runs the
whole document and opens a read-only #raw("*Language Check*") tab listing every issue as
#raw("line:col [kind] \"text\": message"). Under #raw("CAPTURE") the report goes into a variable
instead, for a script to sift.

// ===================================================================================================
= The Command Line (CMline)

The CMline is the one-line command bar near the top (default) or bottom of the window. Almost everything you can do through
a menu — and a great deal you can't — is reachable as a short command (or verb) typed here. Earlier chapters reached
for it whenever they needed to drive the editor; this chapter is the CMline itself, end to end.

-- Moving/hiding the CMline

You can position the CMline near the top or bottom of the window, or hide it when it's not being used:

/ #raw("CMPLACE"): top / bottom / hidden
/ #raw("$CMPLACE"): reads back the current position

Add a `CMPLACE` command to startup.xpl to make your choice persistent.

== Focus: getting in and out

Focus moves between the CMline and the text with three verbs:

/ #raw("GH"), #raw("CMLINE"): move focus to the command line.
/ #raw("GT"): move focus back to the document text.
/ #raw("TH"): toggle between the two.

You run a command by typing it and pressing #kbd("Enter"). After a successful command that touches the
document — an edit, a search, a motion — the cursor lands in the text, ready to keep working; a command
that fails, or one that only reports something, leaves you on the command line. #raw("$TX") tells a
script where focus is: `1` in the document text, `0` on the command line.

== Executing from anywhere: #raw("XC")

#raw("XC") runs whatever is currently on the command line, exactly as if you had pressed #kbd("Enter") —
but from wherever focus happens to be, which makes it ideal to bind to a key. Because the command line
keeps its text after running, pressing your #raw("XC") key again re-runs it. That is the idiom behind
self-incrementing one-liners: because a counter puts a value and advances (see *Editing*), a line that
ends in #raw("XC") lays down the next label each time you fire the key.

== Staging and firing: #raw("BC")

#raw("BC") ("blank command line") clears the command line and moves focus to it — the keyboard gesture
for "jump to the CMline." Given an argument, #raw("BC <text>") stages that text *literally* into the
command line, ready to edit or to run, without expanding it. Paired with #raw("XC") it gives the classic
*stage-and-fire* pattern, useful from a key binding or script:

#ex("BC SE/T /TODO/ ; XC")

That builds the command #raw("SE/T /TODO/") in the line and runs it.

== The pop-up command line

For longer commands the bottom strip can feel cramped. *View ▸ Command line ▸ Pop-up* replaces it with a
centered pop-up above the editor; every CMline behavior works identically inside it. Three verbs drive
the toggle, with a read-back:

/ #raw("SGC") / #raw("CGC") / #raw("TGC"): set / clear / toggle the pop-up command line.
/ #raw("$GC"): `1` when the pop-up is on, `0` when off.

== Chaining and comments

Several commands on one line are separated by a semicolon; they run left to right:

#ex("SE /TODO/ ; PR Found one")

Two backslash escapes let a literal character through where the splitter would otherwise act on it:
#raw("\\;") is a literal semicolon (not a separator) and #raw("\\#") a literal hash (not a comment). A
#raw("#") otherwise begins a *comment* — on its own line, or after a command with a space in front of it
— and runs to the end of the line.

The shell pass-throughs #raw("X"), #raw("XY") and #raw("XV") are the exception: they hand the *whole*
rest of the line to the shell — semicolons and all — so nothing after them is read as a separate
XyScript command. To run an XyScript command after one, wrap the shell command in brackets:
#raw("X [make; test] ; PR $RC") runs #raw("make; test") in the shell, then prints the return code, #raw("$RC"). The
brackets nest, so a subshell needs no doubling (#raw("X [(cd d; ls)]")), and #raw("\\[") / #raw("\\]")
pass a literal bracket through.

Note: Use #raw("X") for commands that run quickly and exit, and #raw("X/NV") to launch apps that stay open.

== Variable substitution

A leading #raw("$") substitutes a variable's value into the line before the command runs; a bare word
with no #raw("$") is just literal text. This is the one thing to watch: #raw("PR count") prints the word
"count", while #raw("PR $count") prints its value. An inline #raw("$(…)") evaluates an *expression* —
arithmetic, functions, comparisons — and drops the result in place:

#ex("SET n = 6
PR $(n * 7)
PR Today is $(DATE(m-d-yyyy))")

Variables, expressions, and the function library are the subject of the next chapter.

== History

The up and down arrows recall previous commands while the command line has focus. #raw("HISTORY") (or
#raw("HI")) opens a searchable picker of past commands; #raw("HI 20") shows the last twenty, #raw("HI
save") those containing "save." In the picker, #kbd("Enter") runs the highlighted command (and moves it
to the top of the history), #kbd("Shift+Enter") pulls it back to the command line to edit without
running, and #kbd("Del") removes it. #raw("CMH") stashes the current command-line text into the history
*without* running it, so you can park a half-built command for later.

== Shell-out and the working directory

XyScript can hand a line to the operating-system shell. #raw("X <command>") runs it and streams the
output to the *Shell Output* tab; #raw("XY <command>") inserts the output at the cursor instead.
Shell-out is *disabled by default* and must be enabled with #raw("TRUST 1"), so a stray command can't
reach the operating system by accident; #raw("$TR") reports whether it is on. Shell commands run in your
working directory (#raw("$CWD"), set with #raw("CD")).

Your working directory is shown on the status line by default; #raw("PAPLACE cmline") moves it to the
front of the command line instead (#raw("PAPLACE status") puts it back). A long path is shortened to
fit — keeping the drive and the trailing folders, with #raw("...") for the elided middle —
and #raw("PATHLEN n") sets how many characters that display may use. #raw("SaveCfg <my.cfg>") makes both
settings persist in your config.

#tip("Background jobs")[A long shell command or script can run in the background with the #raw("/BG")
switch (#raw("X/BG …"), #raw("RUN/BG …")) so the editor stays responsive; its output streams to the
*Shell Output* tab, and #raw("JOBS") lists what is running. A background job runs in its own process
with its own state — it cannot see or change your live document. Each job's output begins with its
command line, and the cursor rests at the top of that block, so the tab shows the newest job from its
start rather than scrolling to the bottom.]

// ===================================================================================================
= Preview, PDF Export, and Field Expansion

A document in XyScript is plain text, but that text can become *finished output* in three ways: rendered
live in a preview pane, typeset to a PDF, or copied with its #raw("|…|") fields filled in. The first two
treat the document as *markup*; the third fills a *template*.

== Choosing how a document renders

Two markup languages are supported, and #raw("DOCTYPE") picks which one the preview and PDF use:

/ #raw("DOCTYPE gfm"): GitHub-Flavored *Markdown* — quick formatted notes with headings, lists, tables, and code.
/ #raw("DOCTYPE typst"): *Typst* markup — the full typesetting language, for fine layout, precise control, and mathematics. (This manual is a Typst document, rendered by XyScript itself.)
/ #raw("DOCTYPE auto"): decide by the file's extension — a #raw(".typ") file is Typst, anything else is Markdown.

#raw("$DOCTYPE") reports the effective choice, and *Document ▸ Document type* sets it from the menu.
Reach for Markdown when you just want readable formatting; reach for Typst when you need control. The
full catalog of Markdown constructs XyScript renders is in *Appendix D*.

== The live preview

#raw("PV") (or #raw("PREVIEW")) opens a preview pane beside the editor and re-renders it as you type, so
you watch the formatted result take shape while you write. A bare #raw("PV") toggles the pane;
#raw("PV on") / #raw("PV off") set it explicitly, and #raw("$PV") reads whether it is open. (The preview
is a GUI feature.)

== Fonts and margins

The look of *Markdown* rendering — in both the preview and the exported PDF — is adjustable:

/ #raw("PVFAMILY <name>") / #raw("PVSIZE <points>") / #raw("PVCODE <name>"): the body typeface, the body size, and the monospace family used for #raw("`code`") and fenced blocks. Each takes #raw("default") to revert to the built-in.
/ #raw("PVMARGIN <len>"): the page margins — one value sets all four sides, while #raw("PVMARGINL") / #raw("PVMARGINR") / #raw("PVMARGINT") / #raw("PVMARGINB") set them individually. A length is a number with an optional unit — #raw("in") (the default), #raw("cm"), #raw("mm"), or #raw("pt") — so #raw("PVMARGIN 1") is one inch and #raw("PVMARGIN 2.5cm") is metric.

#raw("PVFONTS") and #raw("PVCODES") open font pickers for the body and code fonts — GUI shortcuts for the
verbs above. All of these persist in your configuration, and only installed fonts are used (Typst falls
back otherwise). A document authored in *Typst* styles its own page and ignores these settings.

== Exporting to PDF

#raw("TOPDF") typesets the document to a PDF, rendered according to its #raw("DOCTYPE") — Typst for a
#raw(".typ") document, the Markdown renderer otherwise. #raw("TOPDF <path>") names the output file; a
bare #raw("TOPDF") derives the name from the document. #raw("PRINT") is a synonym — note that it
*exports a PDF*, it does not send pages to a printer.

#ex("DOCTYPE gfm
TOPDF report.pdf        # render this Markdown document to report.pdf")

Because #raw("TOPDF") needs no window, a script can generate PDFs headlessly — a batch that opens each of
a set of Markdown files and exports it, for instance. (Both this manual and the reference were produced
exactly that way.)

== Filling in a template: PRINTEX

#raw("PRINTEX") — "print and expand" — makes a copy of the document with every #raw("|…|") *field*
replaced by its value, leaving the original untouched. It is XyScript's mail-merge: write a template
once and expand it as often as you like. #raw("PRINTEX <file>") writes the expanded copy to a file; a
bare #raw("PRINTEX") opens it in a new tab.

A field between #raw("|") bars is one of three kinds:

/ *A substitution*: a variable, an expression, or a function — #raw("|$name|"), #raw("|$(price * qty)|"), #raw("|UPPER($city)|").
/ *A counter field*: #raw("|Cn …|") advances and prints one of the #raw("C1")–#raw("C9") registers using the full counter grammar, so #raw("|C1 1|") seeds a counter and each later #raw("|C1|") emits 1, 2, 3, …. Counters need no #raw("TRUST"); the registers are *snapshotted* for the run, so expanding a template renumbers from its own seed and leaves the document's live counters untouched.
/ *Live code*: statements to run — this requires #raw("TRUST 1"), since it executes arbitrary XPL.

#ex("Dear |$name|,
Your order number is |C1 1000|.

# With  SET name = Ada  in effect, PRINTEX letter.txt writes:
#   Dear Ada,
#   Your order number is 1000.")

To build a starting template of every readable value, #raw("LISTSPECIALS") inserts a #raw("$NAME: |$NAME|")
line for each special variable — a ready-made #raw("PRINTEX") sheet.

#tip("A list that numbers itself")[Because a counter field advances on each use, repeating one down the
page numbers it for you: #raw("|C1 1|. First"), then #raw("|C1|. Second"), #raw("|C1|. Third") expands to
a clean #raw("1.") #raw("2.") #raw("3.") list — no hand-numbering, and inserting a line renumbers the
rest on the next #raw("PRINTEX").]

// ===================================================================================================
= XPL: The Scripting Language

The language you have been typing all along is *XPL* — the XyScript Programming Language, named for and
descended from XyWrite's XPL. The idea is the same: a script is a list of commands run start to finish.
What is new is how it reads — plain words, named variables, and ordinary arithmetic in place of
function-key codes and frames.

Anything you can type on the command line is a line of a script. Three ways to run one:

- save the lines in a file (`.xpl` by convention) and run it with #raw("RUN <file>");
- write a fenced code block in your document and run it in place with #raw("RUNHERE");
- bind the whole thing to a key (see *Key Bindings*).

Comments start with #raw("#") — on their own line, or after a command with a space in front of it — and
run to the end of the line.

== Variables

#raw("SET") assigns a variable; you read it back with a leading #raw("$"). A bare word with no #raw("$")
is just literal text — the one thing to watch for.

#ex("SET name = \"Ada\"
PR Hello $name        # Hello Ada
PR Hello name         # Hello name")

The target may be written with or without the #raw("$") (#raw("SET x = 1") or #raw("SET $x = 1")), and
the compound assignments #raw("+="), #raw("-="), #raw("*="), #raw("/="), and #raw("^=") update a
variable in place. #raw("CLEAR") unsets one or more variables.

#ex("SET total = 0
SET total += 5
SET total *= 2        # 10")

By default variables are *global* — visible everywhere, and they outlive the script. Inside a routine,
#raw("LOCAL") scopes a variable to that routine so it does not clash with identically-named global variables
(see *Subroutines*).

== Substitution and expressions

A value reaches a command two ways. *Substitution* replaces #raw("$name") (or #raw("${name}") when you
need to butt it against other text) with the variable's value before the command runs. *Inline
evaluation* with #raw("$(…)") works out a whole expression and drops the answer in:

#ex("SET n = 6
PR $(n * 7)                   # 42
PR The file is ${name}.txt")

Inside an expression — in #raw("SET"), #raw("IF"), #raw("WHILE"), a #raw("CASE"), or #raw("$(…)") — you
read a variable with #raw("$name"). Compare with #raw("$count < 10"), not #raw("count < 10"): the bare
word is the literal text "count". The operators are the usual ones:

/ #raw("+ - * / %"): arithmetic. Integer math stays integer (#raw("9 / 2") is `4`); a decimal anywhere makes it a float (#raw("9.0 / 2") is `4.5`).
/ #raw("^") and #raw("."): raise to a power, and join two strings — #raw("\"file\" . \".txt\"") is #raw("\"file.txt\"").
/ #raw("== != < > <= >="): compare two values, giving `1` (true) or `0` (false).
/ #raw("AND OR NOT"): combine conditions (#raw("TRUE") and #raw("FALSE") are just `1` and `0`).

== Arrays and maps

An *array* is a list in square brackets; a *map* is a set of key/value pairs in braces. Index either
with #raw("[…]"):

#ex("SET days = [\"Mon\", \"Tue\", \"Wed\"]
PR $(days[0])                # Mon
SET who = {name: \"Ada\", year: 1815}
PR $(who[\"name\"])           # Ada")

Functions build and reshape them — #raw("SPLIT") cuts a string into an array, #raw("JOIN") glues one
back, and #raw("PUSH") / #raw("POP") / #raw("SLICE") / #raw("SORT") transform them (each returns a
*copy*, so assign the result). #raw("LEN") gives an array's length or a string's character count.

== String literals and escapes

A double-quoted string may contain the escapes #raw("\\n") (newline), #raw("\\t") (tab), #raw("\\\"")
(a quote), and #raw("\\\\") (a backslash), among others. A backslash sitting against ordinary text in a
path (#raw("C:\\Users")) is left alone — only the recognized escapes are processed.

== The function library

Functions take values and return new ones; they never change your document. Call one by name with its
arguments in parentheses, anywhere a value is expected:

#ex("PR $(UPPER(\"hello\"))             # HELLO
PR $(LEN(\"hello\"))                # 5
PR Today is $(DATE(m-d-yyyy))")

The library spans text (#raw("UPPER"), #raw("SUBSTR"), #raw("REPLACE"), #raw("INDEX"), …), splitting
and joining (#raw("SPLIT"), #raw("JOIN"), #raw("WORDS"), #raw("LINES")), arrays (#raw("PUSH"),
#raw("SORT"), #raw("SLICE")), numbers and trigonometry (#raw("ABS"), #raw("SQRT"), #raw("POW"),
#raw("BASE"), …), and the masked clock #raw("DATE()") / #raw("TIME()"). The *Reference* lists every one
with its signature.

== Branching

#raw("IF") runs a block when its condition is true; #raw("IFNOT") (or the two-word #raw("IF NOT …"))
when it is false. #raw("ELSEIF") and #raw("ELSE") add alternatives, and #raw("EI") (or #raw("ENDIF"))
closes the block.

#ex("IF $MO
  PR The document has unsaved changes.
ELSE
  PR Saved.
EI")

== Looping

#raw("WHILE") repeats a block while its condition holds, closing at #raw("EW") (or #raw("ENDWHILE")).
#raw("BREAK") leaves the loop early; #raw("CONTINUE") skips to the next turn.

#ex("SET i = 0
WHILE $i < 3
  PR line $i
  SET i += 1
EW")

#raw("FOR") comes in three shapes, all closed by #raw("EF") (or #raw("ENDFOR")):

#ex("FOR i = 1 TO 5             # counting; add STEP s to change the stride,
  SET total += $i          # or the /X switch to exclude the end value
EF

FOR w IN WORDS($SE)        # over a collection
  PR $w
EF

FOR k, v IN $config        # a map, as key/value pairs
  PR $k = $v
EF")

== Multi-way branch

#raw("SWITCH") tests one expression against several #raw("CASE") arms — with no fall-through — plus an
optional final #raw("DEFAULT"), closed at #raw("ESW") (or #raw("ENDSWITCH")). A #raw("CASE") may list
several values:

#ex("SWITCH $EN
CASE \"utf-8\"
  PR Unicode.
CASE \"cp437\", \"cp850\"
  PR DOS code page.
DEFAULT
  PR Something else.
ESW")

== Labels and line continuation

#raw("GOTO :label") jumps to a #raw(":label") elsewhere in the script — useful sparingly. And a long
line breaks across several by ending it with *a space and a backslash*; the next line joins on as if it
were one. The space matters: a backslash tucked against text (a path ending #raw("C:\\")) is left alone,
so only a trailing space-backslash continues the line.

== Scripts on disk

Save a script as a #raw(".xpl") file and run it with #raw("RUN <file>"). Variants: #raw("RUN/BG") runs
it in the background — its own process, so it cannot see or change your live document — and
#raw("RUN/R:<var>") captures its output into a variable. #raw("RUNCODE") runs XPL held in a variable,
and #raw("RUNHERE") runs the fenced block under the cursor. A script named #raw("startup.xpl") beside
the program runs automatically at launch — the place to load your library, set your colors, and bind
your keys.

== Subroutines

Work you do more than once goes in a *subroutine*. Define it with #raw("SUB"), close it with #raw("ES")
(or #raw("ENDSUB")), and hand a result back with #raw("RETURN") (read afterward as #raw("$RV") ("return value")). Inside,
the arguments are #raw("$1"), #raw("$2"), …, with #raw("$*") for all of them joined and #raw("$#") for
how many there were.

#ex("SUB greet
  PR Hello, $1!
  RETURN UPPER($1)
ES

greet Ada                # prints  Hello, Ada!
PR $RV                   # ADA")

Call a routine with #raw("CALL") — or, since a routine is invoked like any other command, just by name:
#raw("greet Ada") is the same as #raw("CALL greet Ada"). Use #raw("LOCAL") inside a routine for
variables that should not leak back to the caller. Collections of routines live in *library* files;
see *User Libraries*.

== Editing a file blindly

Sometimes a script needs to *transform a file* — fix every occurrence of a term, sort a list, rewrite a
config — and the natural way to express that is with the ordinary editing verbs (#raw("SE"), #raw("CH"),
#raw("INSERT"), the motions, #raw("SORT"), the clipboard) rather than reading the whole file into a
variable and doing string surgery. #raw("BEDIT") ("blind edit") lets you do exactly that without opening
a visible tab: it loads the file into an *invisible* buffer that the editing commands and
#raw("$")-readouts retarget to, while the screen keeps showing whatever you were working on.

#raw("BEDIT <file>") opens the file — loading it if it exists, or starting a new buffer named for it if
not. While it is open, everything you would normally aim at the document lands on the hidden buffer
instead: #raw("CHA /old/new/") replaces throughout it, #raw("$LI") reads its current line, #raw("$FI")
is its name, and so on. A bare #raw("BEDIT") (no file) opens a fresh, unnamed hidden buffer to build
from scratch.

Nothing is written until you say so. #raw("SA") — or #raw("SAVE <path>") / #raw("SAAS <path>") — saves
the hidden buffer; if you never save, the edits are simply discarded. A blind edit is a scratchpad, not
an autosave.

Close it with #raw("BCLOSE"), the explicit end of a blind edit. #raw("BCLOSE <file>") also states
*which* file you meant to close, and complains — setting #raw("$ERROR") — if the open blind buffer is a
different one (or if none is open at all): a small guard that keeps a longer script honest. A bare
#raw("BCLOSE") closes whatever blind buffer is open. #raw("AB") does the same thing, but #raw("BCLOSE")
reads more clearly in a script.

#ex("BEDIT config.cfg              # open it invisibly
CHA /timeout=30/timeout=60/   # fix every occurrence
SA                            # write it back
BCLOSE config.cfg             # done")

Two rules to keep in mind. Only one blind edit is open at a time, so #raw("BCLOSE") the current one
before you #raw("BEDIT") another. And a blind edit a script leaves open is closed automatically —
discarding — when the script ends, so it never lingers; #raw("BCLOSE") matters when you want to end it
*early*, typically to move on to a second file or to return to visible-document commands within the same
script. (Blind editing is a scripting tool, not something to drive by hand — while it is open the screen
looks frozen, since your edits are landing out of sight.)

#tip("Build a file from nothing")[A bare #raw("BEDIT") starts an empty hidden buffer; fill it with
#raw("INSERT") / #raw("WRITELN") and #raw("SAVE <path>") to generate a file — a report, a config, a data
dump — entirely from a script, without disturbing what is on screen.]

== Asking the user

#raw("INPUT <var> [prompt]") pauses for a typed line and stores it in a variable. #raw("PICK <var>
[\"title\"] <list>") pops a filterable chooser over a list — an expression that yields an array — and
stores the chosen item, or with #raw("/I") its 1-based index. Cancelling either leaves the variable
empty and sets #raw("$ERROR").

#ex("PICK choice \"Pick a color\" [\"red\", \"green\", \"blue\"]
IF NOT $ERROR
  PR You chose $choice.
EI")

#raw("READKEY <var> [message]") pauses for a single keystroke and stores its character in a variable
(the scancode lands in #raw("$SC")). With a `message`, it pops a box showing it while it waits — a quick
keypress menu:

#ex("READKEY letter \"Pick a letter:\\nA\\nB\\nC\"
SWITCH UPPER($letter)
  CASE \"A\"; PR apple
  CASE \"B\"; PR banana
  DEFAULT; PR ?
ESW")

== The error model

Many commands can *fail* in a way a script can handle — a search finds nothing, a file is missing. Such
a failure sets the *error flag*, read as #raw("$ERROR") (`1` after a failure, `0` after success). An
ordinary action verb clears the flag when it starts, so test it on the line right after the command
that might fail:

#ex("SE /needle/
IF $ERROR
  PR Not found.
EI")

Because a search does not wrap and sets #raw("$ERROR") at the end of its sweep, the standard
"do-something-to-every-match" loop is a #raw("WHILE") that breaks when #raw("$ERROR") becomes set.

== Quiet

A script that does a lot of small steps can be noisy on the status line. #raw("QUIET") suppresses status
messages for the rest of the script (#raw("QUIET off") re-enables them); with #raw("/NV") it also
silences non-fatal error messages. Output you ask for explicitly with #raw("PR") or #raw("MSG") is
always shown.

== Putting it together

A short routine that shouts the current selection — upper-cases it and reports its length, or complains
if nothing is selected:

#ex("SUB shout
  LOCAL text
  SET text = SEL()
  IF LEN($text) == 0
    PR Nothing selected.
    RETURN
  EI
  PR $(UPPER($text))  ($(LEN($text)) chars)
ES")

Bind it to a key, or call it by name from the command line — #raw("shout").

// ===================================================================================================
= User Libraries

A *library* is a file of routines you keep around and reuse. Where a script in a #raw(".xpl") file runs
start to finish, a library is a *collection*: each routine sits ready to be called by name — from the
command line, a key binding, or another script. Your personal toolkit lives in one.

== The library file

A library is a plain text file, #raw(".lib") by convention, holding one or more routines. Each is an
ordinary #raw("SUB … ES") definition — exactly what *XPL: The Scripting Language* described — free to
use the functions, control flow, and other routines you already know.

A comment line of the form #raw("# DOC: <text>") inside a routine documents it; every such line shows up
under that routine in the library listing. Mind the space — #raw("#DOC:") with no space is not a comment
and would error; write #raw("# DOC:").

#ex("# greet.lib — a tiny example library

SUB hello
  # DOC: greet someone by name
  # DOC: usage:  hello <name>
  PR Hello, $1!
ES

SUB shout
  # DOC: upper-case the current selection
  PR $(UPPER(SEL()))
ES")

== Calling a routine

A loaded routine is invoked like any other command — by name, with its arguments after it. The
#raw("CALL") verb is optional:

#ex("hello Ada             # same as:  CALL hello Ada")

Inside, the arguments arrive as #raw("$1"), #raw("$2"), …, with #raw("$*") for all of them and #raw("$#")
for the count; #raw("RETURN") hands a value back, read afterward as #raw("$RV"). (See *XPL: The
Scripting Language* for the full story on routines.)

== Loading, listing, and unloading

Libraries are managed with a small set of verbs, also on the *Tools ▸ Library* menu:

/ #raw("LOADLIB <file>"): load a #raw(".lib"), *replacing* the current user library.
/ #raw("ADDLIB <file>"): *merge* a #raw(".lib") into the loaded library, keeping what is already there.
/ #raw("LISTLIB"): list the loaded routines, with their #raw("# DOC:") descriptions, in a scrollable dialog (or into a #raw("CAPTURE") variable).
/ #raw("RELOADLIB"): re-run the recorded load sequence, picking up edits you have made to the files.
/ #raw("UNLOADLIB"): drop all user-library routines.

While you are developing a library the cycle is quick: save the file, #raw("RELOADLIB"), call the
routine again.

#tip("A library is how routines are shared — RUN is not")[A #raw("SUB") defined inside a script is
*local to that script*. If #raw("startup.xpl") does #raw("RUN mydefs.xpl") and then calls a routine
that #raw("mydefs.xpl") defined, that call fails with #raw("unknown command") — and because such an
error is fatal, the rest of #raw("startup.xpl") stops there. Put routines you want to reuse in a
#raw(".lib") and #raw("LOADLIB") it instead; a routine loaded that way is callable by the rest of the
very same script, including the startup script that loaded it.]

#tip("A bad routine is skipped, not fatal")[Loading validates each routine. If one will not compile —
say it jumps to a label that isn't there — XyScript skips just that routine with a message and loads the
rest, so a single typo can't take your whole library down with it.]

== Name clashes are caught early

A routine may not *shadow* a built-in command. If a #raw(".lib") defines a routine named, say,
#raw("SAVE"), the load is rejected rather than leaving it ambiguous whether #raw("SAVE") means the verb
or your routine. Give a routine a name the program doesn't already use, and calling it by name is never
ambiguous.

== Loading your library at startup

So your toolkit is there every session, load it from #raw("startup.xpl"), the script that runs at launch
(see *XPL: The Scripting Language*):

#ex("# startup.xpl
LOADLIB mytools.lib")

From then on, every routine in #raw("mytools.lib") answers to its name the moment the program opens.

// ===================================================================================================
= Key Bindings

XyScript puts the keyboard first, and the keyboard is yours to shape. Bindings live in a *keyboard
file* — a #raw(".kbd") file — that maps each key, in each modifier layer, to a command. If you are
coming from XyWrite the format will look familiar.

== Claiming a key — and releasing it

The map runs in *both* directions, and this is the first thing to understand. Any key can run any
XyScript command — but any key can also be handed straight back to the operating system. Two ways
release a key:

/ bind it to the special verb #raw("KF") ("keyboard flip", #raw("nn=KF")): an explicit "let this one
  through";
/ or simply leave it out of the keyboard file: a key XyScript does not claim is passed through too.

Either way, XyScript does not consume the press — it reaches the OS exactly as if the editor were not
watching.

That second direction is the quiet flexibility. It means XyScript never locks you out of a key you
would rather the system keep — a media key, a window-manager shortcut, your IME. And it means you can
*deliberately* reserve keys for some *other* program: point a key at #raw("KF") and a global macro tool
— a hot-key utility, your keyboard's own macro firmware, whatever — can own that key while you work
inside XyScript. Claim the keys you want; release the rest.

== The keyboard file

A #raw(".kbd") file opens with a short stanza declaring how many keys there are and which scan codes act
as the modifier keys; each #raw("TABLE=") line then begins a layer (a bare #raw("TABLE=") is the
unshifted layer):

#ex("KEYS=104
SHIFT=42,54
CTRL=29,99
ALT=56,98
CAPS=58,T:C

TABLE=
; the unshifted layer
14=BD
16=q
57=\" \"

TABLE=SHIFT
16=Q")

Keys are the classic scan codes 1–104. Comments are *whole-line only* — a line whose first non-blank
character is a #raw(";"). There are no inline comments, because a #raw(";") after the #raw("=") is part
of the assignment: it chains commands, as below.

== What a key can do

The right-hand side of an assignment is just XPL. A few forms:

/ a *single character* (#raw("16=q")): types that character.
/ a *quoted string* (#raw("16=\"Hello\"")): types the whole text; use #raw("\\\"") for a quote and #raw("\\\\") for a backslash inside it.
/ a *command* (#raw("nn=AB")): runs that verb.
/ *chained commands* (#raw("nn=SA;AB")): run in sequence, separated by #raw(";").
/ the verb #raw("KF") (#raw("nn=KF")): *releases* the key to the operating system instead of consuming it (see _Claiming a key — and releasing it_, above).

Because the right-hand side is the full language, a whole conditional can live on one key:

#ex("; if a selection is defined, delete it; otherwise delete the character ahead
nn=IF $DF;DZ;RD;ELSE;RC;EI")

The arrow, numeric-keypad, and #kbd("Page Up")/#kbd("Page Down") keys are yours to bind too. Left
unbound, they perform their built-in *wrap-aware* cursor and page movement; bound to a vertical-motion
verb (#raw("UP"), #raw("DOWN"), #raw("PU"), #raw("PD")) they still move by screen line or page. Bind one
to anything else — a command, a macro, #raw("GT"), #raw("TH") — and that is what runs. So the numeric
keypad is fully remappable, not reserved for cursor motion.

== Loading and saving keyboards

/ #raw("LOADKBD <file>"): load a #raw(".kbd") file, replacing the current map.
/ #raw("SAVEKBD <file>"): write the current keymap back out to a #raw(".kbd") file.
/ #raw("CLEARKBD"): reset to the built-in baseline keyboard.

Load your keyboard from #raw("startup.xpl") so it is in place every session. *Tools ▸ Keyboard chart*
(the #raw("KBCHART") verb) opens a scrollable window listing every modifier table — Normal, Shift, Ctrl,
and so on — with each table's bindings, so you can see the whole map at a glance. To read a single
binding from a script, the #raw("KEY(scan)") function does it: #raw("KEY($SC)") tells you what the key
you just pressed does on its plain layer, while #raw("KEY($SCC)") reads the binding on the *modifier
layer* it was pressed with. #raw("$SC") is the last scan code; #raw("$SCC") is that plus the held
modifier's bit value, so one number names the key and its shift state together. Where #raw("KEY") tells
you what a key _does_, #raw("GETKEY(scc)") tells you what it is _called_ — #raw("GETKEY($SCC)") returns
the readable name of the key, the same string #raw("READKEY/D") would capture (#raw("CTRL+Y"),
#raw("Enter"), #raw("Backspace")), or the empty string if the number names no key.

== Binders: registers you can re-fill at runtime

Editing the #raw(".kbd") file sets *permanent* bindings. For the throwaway kind — "remember this text,"
"remember this little script for the next few minutes" — XyScript has *binders*: lettered registers you
bind to a key once and re-fill on the fly.

/ #raw("@A")…#raw("@Z"): *text* registers. In a key or command position, #raw("@A") inserts whatever text the slot holds.
/ #raw("&A")…#raw("&Z"): *script* registers. #raw("&A") runs the XPL the slot holds.

You fill a slot at runtime, so the same key can mean different things through the day:

/ #raw("LDTX <slot> [<text>|<file>]"): load a text register — from the selection (no second argument), a #raw("\"quoted string\""), or a source file.
/ #raw("LDSC <slot> [<file>]"): load a script register (#raw("LDPM") is a synonym).

The idiom is to bind a key to #raw("@A") (or #raw("&A")) *once* in your keyboard file, then change what
#raw("A") contains with #raw("LDTX") / #raw("LDSC") whenever you like — a register/macro system without
reloading the keymap. The shipped keyboard follows a simple convention: #kbd("Alt")+letter inserts that
text register, #kbd("Ctrl+Alt")+letter runs that script register.

#tip("Capture a selection into a register")[Select some boilerplate, run #raw("LDTX A") (no argument
takes the selection), and from then on your #raw("@A") key drops that text wherever the cursor is —
until you re-load the slot with something else. Binders are saved with your settings, so they survive
between sessions.]

// ===================================================================================================
= Customization and Configuration

Almost every part of XyScript's appearance and behavior is adjustable, each reachable both from the
*Settings* menu and as a command — so you can tune the program interactively, then capture the result to
load automatically next time.

== Colors

The editor's colors are set with verbs that take an #raw("R G B") triple (each 0–255), or #raw("/HSL")
for hue / saturation / lightness:

/ #raw("FGCOLOR") / #raw("BGCOLOR"): the text and background of the editing area.
/ #raw("UIFGCOLOR") / #raw("UIBGCOLOR"): the program chrome — menus, status line, tabs.
/ #raw("SELBGCOLOR") / #raw("CRCOLOR") / #raw("SEPCOLOR"): the selection highlight, the caret, and the pane separators.

#raw("THEME light") and #raw("THEME dark") set them all at once to a coordinated scheme. Each color has
a matching read-back (#raw("$FGCOLOR"), …) returning its #raw("R G B"), and *Settings ▸ Appearance ▸
Colors* opens a live picker for each.

== Fonts and sizing

Four font pickers cover the four text surfaces — the editor, the program chrome, the pop-up text
windows, and the directory window — each choosing family, size, and style with a live preview:
#raw("EDFONTS"), #raw("UIFONTS"), #raw("TXTFONTS"), #raw("DWFONTS"). For quick adjustments without the
picker, #raw("UISCALE") zooms the whole interface, #raw("EDFONT +1") nudges the editor font size,
#raw("LEADING") changes line spacing, and #raw("OF") / #raw("OFL") / #raw("OFR") set the text margins
(each takes a signed #raw("+n") / #raw("-n") or a bare number).

== View toggles

The *View* menu's toggles each have a verb and a read-back, following a *Set* / *Clear* / *Toggle*
pattern: paragraph marks (#raw("SVR") / #raw("CVR") / #raw("TVR")), whitespace (#raw("SWS") / …), line
numbers (#raw("SLN") / …), plus the preview pane (#raw("PV")), the shell pane (#raw("SH")), and the menu
bar (#raw("SMB")).

== Syntax highlighting

XyScript can color your text according to what it _means_ — headings and links in a Markdown file,
keywords and strings and comments in a program — and every one of those colors is yours to change with
#raw("HLCOLOR"). There is enough to it that it has a chapter of its own: which files highlight, the
ready-made grammars for a dozen languages, the token kinds and their colors, and how to make your
scheme come back every session. See _Syntax Highlighting_, the next chapter.

== Saving and loading your settings

A *configuration* is the whole set of your preferences — colors, fonts, toggles, key binders, and more.
Three verbs manage it:

/ #raw("SAVECFG <file>"): write your current settings to a #raw(".cfg") file (itself a runnable XPL script).
/ #raw("LOADCFG <file>"): load and apply a saved configuration.
/ #raw("AUTOCFG <file>"): *track* a config file — XyScript keeps it up to date automatically as you change settings, so they persist with no manual save.

XyScript also remembers your *window* size and position between runs — the main window and each of the
secondary #raw("FIND"), #raw("DIR/W"), and #raw("TXT") windows (toggle all of them with
#raw("WINGEO on") / #raw("WINGEO off")).

== Restoring your tabs across runs

With #raw("SESSION on"), XyScript remembers the tabs you had open and reopens them the next time you
start — the way Notepad++ does. Every deliberate quit (the #raw("QUIT") command, the File menu, or the
window's close button) saves the set of open tabs; the next launch brings them back in order, with the
cursor where you left it.

It never touches your real files to do this. A tab with *unsaved* changes — or an untitled buffer — is
copied to a private *shadow* backup, so it comes back with your unsaved content still showing as
modified; your file on disk is untouched until you #raw("SAVE"). A clean file is simply reopened from
disk. If you launch with a file on the command line, it opens *alongside* the restored tabs.

Your session is also saved quietly as you work, so it survives a *crash* or a forced kill — not just a
tidy quit. Opening, closing, or switching tabs, and splitting or un-splitting panes, are saved the
instant they happen; the text you type is saved every few seconds. (The save is cheap: a tab's content
is only re-written when it actually changed.)

A split-pane arrangement is restored too — the same files in the same panes, split the same way. (If a
pane had been showing a working view rather than a file — a directory listing or the shell output —
that arrangement can't be rebuilt, so the session comes back as plain tabs instead.) Each pane also
comes back scrolled to exactly where you left it, so if you had scrolled away from the cursor to read
elsewhere in a file, that spot — not just the cursor — is what you see on return.

#tip("What isn't restored")[Directory listings and the shell-output pane are working views, not
documents, so they aren't part of a session. #raw("SESSION") is off until you turn it on, and the
setting is saved with your configuration.]

== The files XyScript uses

By convention four kinds of plain-text file carry your setup, each runnable or loadable:

/ #raw(".xpl"): a script (#raw("RUN")).
/ #raw(".kbd"): a keyboard file (#raw("LOADKBD")).
/ #raw(".lib"): a routine library (#raw("LOADLIB")).
/ #raw(".cfg"): a saved configuration (#raw("LOADCFG")).

#tip("Set it up once")[A script named #raw("startup.xpl") beside the program runs at launch — the
natural place to load everything: #raw("LOADCFG") your settings, #raw("LOADKBD") your keyboard, and
#raw("LOADLIB") your library. Drop it in place and XyScript opens fully outfitted every time.]

// ===================================================================================================
= Syntax Highlighting

*Syntax highlighting* colors parts of your text according to what they _mean_: in a Markdown file,
headings, *bold* and _italic_ runs, `inline code`, links, and block quotes each take their own color,
so the document's structure is visible at a glance; in a program, keywords, strings, and comments
separate from the code around them. It is purely visual — it never changes a single byte of the file —
and it is *off by default*.

Three things have to line up before you see any color, and they are the shape of this chapter:
XyScript has to *recognize the file's type*, highlighting has to be *switched on*, and each *token
kind* carries a *color* you can change.

== Which files can be highlighted

Highlighting is chosen by the file's *extension*. *Markdown* is built in — #raw(".md"),
#raw(".markdown"), #raw(".mkd"), #raw(".mdown") — and needs nothing loaded. Every other language is
described by a *grammar*, a small script XyScript reads. You rarely have to write one: ready-made
grammars for a dozen languages ship with the program, so in practice "teaching" XyScript a language
means running one (see _The ready-made grammars_).

So the very first step is simply to be editing a file XyScript recognizes:

/ *A new document*: #raw("NEW notes.md") opens a fresh buffer already named #raw(".md").
/ *Text you already have*: save the current buffer under a #raw(".md") name — #raw("SAAS notes.md") — or open an existing #raw(".md") file.

#tip("Highlighting keys off the name, not the content")[Typing Markdown into an *untitled* buffer, or
one named #raw(".txt"), shows no colors no matter what — there is no recognized type to highlight. Give
the file a #raw(".md") name first and the colors become available.]

== Turning it on

With a recognized file in front of you, switch highlighting on in either of two ways:

/ *From the menu*: #raw("View ▸ Highlighting") — a checkmark shows when it is on; choose it again to turn it off.
/ *From the command line*: #raw("HIGHLIGHT on") (and #raw("HIGHLIGHT off") to stop).

The colors appear at once. Because highlighting is only a lens over the text, turning it on or off leaves the
file exactly as it was.

#ex("NEW notes.md
HIGHLIGHT on")

=== Highlighting every file automatically

Flipping the switch for each file soon gets tiresome. #raw("HLAUTO on") — also #raw("Settings ▸ Syntax
highlighting ▸ Auto-highlight on open") — tells XyScript to highlight *every* recognized file the moment
it opens, so you never think about it again; #raw("HLAUTO off") returns to per-file control. Like other
preferences it is saved with your configuration.

== The ready-made grammars

Markdown aside, a language only lights up once its *grammar* has been loaded. XyScript ships these in
the #raw("grammars") folder beside the program:

#table(
  columns: (auto, auto, 1fr),
  inset: 5pt,
  stroke: 0.5pt + luma(200),
  table.header([*Script*], [*Language*], [*Extensions*]),
  [#raw("rust.xpl")], [Rust], [#raw(".rs")],
  [#raw("python.xpl")], [Python], [#raw(".py .pyw")],
  [#raw("javascript.xpl")], [JavaScript / TypeScript], [#raw(".js .mjs .cjs .ts .jsx .tsx")],
  [#raw("html.xpl")], [HTML], [#raw(".html .htm .xhtml")],
  [#raw("shell.xpl")], [Shell], [#raw(".sh .bash .zsh .ksh")],
  [#raw("toml.xpl")], [TOML], [#raw(".toml")],
  [#raw("typst.xpl")], [Typst], [#raw(".typ")],
  [#raw("xpl.xpl")], [XPL (XyScript's own)], [#raw(".xpl .pm .pgm .cfg .lib")],
  [#raw("xywrite.xpl")], [XyWrite], [#raw(".xy3 .xy4")],
)

Load the one you want with #raw("RUN"), then open your file:

#ex("RUN grammars/rust.xpl
OPEN main.rs
HIGHLIGHT on")

#tip("The step that is easy to miss")[Without the #raw("RUN"), a #raw(".rs") file shows *no*
highlighting at all — not the wrong colors, simply none, because no grammar has claimed the extension.
#raw("HIGHLIGHT on") by itself is enough only for Markdown.]

You will not want to type that every session; _Making it stick_, below, shows the two ways to have your
grammars loaded automatically at launch.

=== Highlighting your own XPL scripts

Because XyScript is itself programmed in XPL, the #raw("xpl.xpl") grammar highlights the very scripts,
configurations (#raw(".cfg")), and libraries (#raw(".lib")) you write. Alongside the usual #raw("#")
comments, #raw("\"strings\""), #raw(":labels"), #raw("$variables"), #raw("@")/#raw("&") binders, and
#raw("/switches"), it colors two things by name:

/ *Control-flow keywords* (the #raw("keyword") kind): the block words #raw("IF"), #raw("WHILE"),
  #raw("FOR"), #raw("SWITCH"), #raw("SUB"), #raw("CAPTURE") and their closers and short aliases
  (#raw("ENDIF")/#raw("EI"), #raw("ENDWHILE")/#raw("EW"), …), the branch and loop words
  #raw("ELSE"), #raw("ELSEIF"), #raw("CASE"), #raw("DEFAULT"), #raw("TO"), #raw("STEP"), #raw("IN"),
  the jumps #raw("GOTO"), #raw("BREAK"), #raw("CONTINUE"), #raw("RETURN"), #raw("EXIT"), and the
  expression logic #raw("AND") / #raw("OR") / #raw("NOT").
/ *Built-in expression functions* (the #raw("function") kind): the complete library — #raw("LEN"),
  #raw("UPPER"), #raw("SPLIT"), #raw("SUBSTR"), #raw("SIN"), #raw("SQRT"), #raw("DATE"), and the rest
  (the _Reference_ lists them all).

Ordinary verbs are deliberately left uncolored — there are hundreds, and painting them all would bury
the structure rather than reveal it. Matching is case-insensitive, so #raw("if"), #raw("If"), and
#raw("IF") all light up. Because #raw("function") is one of the built-in kinds, it already has a
theme-aware color and follows #raw("THEME light") / #raw("THEME dark") with everything else; recolor it
like any other with #raw("HLCOLOR function <R G B>").

== Token kinds and their colors

Each highlighted element belongs to a *token kind*, and the kind is what carries the color. The
Markdown kinds are #raw("heading"), #raw("strong"), #raw("emphasis"), #raw("code"), #raw("link"),
#raw("quote"), and #raw("rule"); grammars for code add #raw("keyword"), #raw("string"),
#raw("comment"), #raw("number"), and #raw("function"). To see the current palette, run #raw("HLKINDS") (#raw("Settings ▸
Syntax highlighting ▸ Token kinds")) — it lists every kind beside its color, each name drawn in the
color itself.

There are three ways to change a color:

/ *A picker*: #raw("Settings ▸ Syntax highlighting ▸ Theme colors ▸ ") _kind_ — or the command #raw("HLCOLORS <kind>") — opens a live color picker for that one kind.
/ *A command*: #raw("HLCOLOR <kind> <R G B>") — for example #raw("HLCOLOR heading 200 40 40") for red headings. The three numbers each run #raw("0")–#raw("255"); there are no color names or hex codes. Add #raw("/HSL") to give hue / saturation / lightness instead.
/ *Switching one off*: #raw("HLCOLOR <kind> default") drops that kind's color so it draws in the ordinary text color.

#ex("HLCOLOR heading 200 40 40
HLCOLOR comment 120 120 120")

#raw("HLRESET") (#raw("Settings ▸ Syntax highlighting ▸ Reset theme")) restores the built-in colors, and
the whole-editor schemes #raw("THEME light") / #raw("THEME dark") set a coordinated highlight palette
along with everything else.

#tip("`default` is not the same as `HLRESET`")[#raw("HLCOLOR <kind> default") does *not* put back that
kind's built-in color — it turns that kind's highlighting *off*, so it draws like ordinary text.
#raw("HLRESET") is what brings the built-in palette back.]

== Teaching XyScript a new file type

If no ready-made grammar covers your language, write one — a grammar is just a short XPL script.
Declare the language with #raw("HLLANG <name> <.ext,…>"), then add #raw("HLRULE <kind> <regex>") rules.
Each line is matched on its own, in rule order, and the first rule to match a stretch of text wins.

#ex("HLLANG mylang .foo,.bar
HLRULE comment #.*$
HLRULE keyword \\b(fn|let|return)\\b
HLRULE number \\b[0-9]+\\b")

Save that as a #raw(".xpl") file, #raw("RUN") it, and any #raw(".foo") you open highlights. The
Reference has the full grammar syntax; _Appendix B_ covers the regular-expression dialect.

=== Inventing your own kinds

A rule's kind may be *any name you like* — you are not limited to the twelve built-ins. Naming a new
one in an #raw("HLRULE") is all it takes to bring it into being; give it a color with #raw("HLCOLOR")
and it appears in #raw("HLKINDS") alongside the rest. (The shipped #raw("typst.xpl") does exactly this
with a #raw("math") kind.)

#ex("HLRULE mykind \\btodo\\b
HLCOLOR mykind 10 20 30")

#tip("Rules create kinds; colors paint them")[#raw("HLRULE") is what brings a kind into existence.
#raw("HLCOLOR") on a name that nothing emits is accepted and remembered, but nothing will ever be drawn
in it. Conversely, a kind with no color of its own still highlights — in the ordinary text color. Order
does not matter: you may color a kind before or after the rule that creates it.]

== Making it stick

#raw("SAVECFG") captures everything about highlighting: whether #raw("HLAUTO") is on, every color you
have set, *and* every grammar you have loaded — each one written back out as its #raw("HLLANG") line
followed by its #raw("HLRULE") rules. Your whole scheme therefore returns with a single
#raw("LOADCFG").

That gives two ways to have your grammars ready at every launch. Put either in #raw("startup.xpl"),
which runs automatically:

/ *Run the scripts*: a #raw("RUN grammars/rust.xpl") line per language, plus #raw("HLAUTO on"). The grammar files stay the master copy — edit one and the change takes effect next launch.
/ *Load your config*: #raw("RUN") the grammars once and #raw("SAVECFG"); from then on a single #raw("LOADCFG") restores grammars and colors together.

#tip("Snapshot versus source")[The second route copies each grammar into your configuration as a
*snapshot*. Edit #raw("grammars/rust.xpl") afterwards and your config still holds the old rules —
#raw("RUN") it again and #raw("SAVECFG") to refresh. While you are still tinkering with a grammar, the
first route is the friendlier one.]

#tip("THEME overwrites the palette")[#raw("THEME light") and #raw("THEME dark") replace every highlight
color at once. If your #raw("startup.xpl") runs #raw("THEME"), put it *before* your #raw("HLCOLOR")
lines or they will be overwritten. A configuration written by #raw("SAVECFG") is immune: it stores the
resulting colors, not the #raw("THEME") command.]

#tip("The whole feature in ten seconds")[Run #raw("NEW demo.md"); type a line starting with #raw("# "),
then a line with #raw("**bold**") and #raw("`code`"); choose #raw("View ▸ Highlighting") to color it, and
open #raw("Settings ▸ Syntax highlighting ▸ Theme colors ▸ Heading") to recolor headings with the picker.]

// ===================================================================================================
= Email

XyScript can send and receive email. It leans on #raw("curl") — the
command-line network tool that ships with Windows 10+, macOS, and Linux — to talk to your provider's
servers, so there is no mailbox to manage and no address book: you compose in an ordinary document and
send it, or you pull raw messages down to files. Everything in this chapter is also on the menus under
#raw("Tools ▸ Email").

== Setting up an account

Sending needs four settings; receiving adds two more. Each is a verb, and typing it bare reports the
current value:

/ #raw("SMTPURL <url>"): your provider's SMTP submission URL — #raw("smtps://smtp.example.com:465")
  for implicit TLS (port 465), or #raw("smtp://smtp.example.com:587") for STARTTLS (port 587). Matching
  the scheme to the port matters; a mismatch shows up as a #raw("curl") TLS error.
/ #raw("MAILUSER <name>"): the account username, usually your full email address. It is used for both
  sending and receiving.
/ #raw("MAILFROM <address>"): the #raw("From:") address. It defaults to #raw("MAILUSER"); most servers
  require it to equal the account you logged in as.
/ #raw("MAILPASS <secret>"): the password — for most providers an *app password* rather than your login
  password. It is never written to disk and never echoed (#raw("$MAILPASS") reads only #raw("set") or
  #raw("not set")).
/ #raw("IMAPURL <url>"): for receiving — the IMAP server, e.g. #raw("imaps://imap.example.com:993").
/ #raw("MAILBOX <name>"): the folder to read, #raw("INBOX") by default.

None of this is saved. The tidy way is to put everything except the password in your #raw("startup.xpl")
and supply the password each session (or through the #raw("XYSCRIPT_MAILPASS") environment variable,
which #raw("MAILPASS") falls back to). The password is handed to #raw("curl") through a channel that
keeps it off the command line, so it never shows up in a process listing.

#tip("App passwords")[Gmail, Outlook, and most providers no longer accept your ordinary login password
for SMTP/IMAP. Create an *app password* in your account's security settings and use that as
#raw("MAILPASS").]

== Sending a message

The active document is the body. The simplest form gives the recipient and an optional subject on the
command line:

#raw("SENDMAIL alice@example.com \"Lunch Thursday?\"")

For more than one recipient, or to set #raw("Cc:")/#raw("Bcc:"), start the document with a header block
— #raw("To:"), #raw("Cc:"), #raw("Bcc:"), #raw("Subject:"), #raw("From:") lines, then a blank line, then
the body — and run a bare #raw("SENDMAIL"). To send a file you have not opened, #raw("SENDMAIL/F <file>")
reads the message from that file instead of the current document (quote the path if it has spaces).

Each address may be a bare one or the display-name form, #raw("\"John Q. Public\" <john@everyman.org>")
— the full form shows in the message, while the server sees just the address. This works for
#raw("From:")/#raw("MAILFROM") too.

The header block isn't limited to those known fields: any other proper header line you add is carried
into the message, so you can set #raw("Reply-To:"), #raw("In-Reply-To:"), #raw("References:"), an
#raw("X-…") of your own, and so on. Three sensible limits apply — the structural
#raw("Content-Type") / #raw("Content-Transfer-Encoding") / #raw("MIME-Version") headers are computed for
you and can't be overridden; a #raw("Date:") or #raw("Message-ID:") you supply replaces XyScript's
generated one; and a "header" whose name contains a space is treated as the start of the body, so keep
the blank line before your body as usual.

#tip("Reply into a thread")[Paste the original message's #raw("Message-ID") into an
#raw("In-Reply-To:") line (and optionally #raw("References:")) in your header block, and mail clients
will file your reply in the same conversation.]

Attach files with #raw("ATTACH <path>"); they ride along on the next #raw("SENDMAIL") and the staged
list is then cleared. Bare #raw("ATTACH") lists what is staged, #raw("CLEARATTACH") empties it, and in a
header block you can write #raw("Attach:") lines instead.

Sending runs in the background, so the editor never freezes. When it finishes, #raw("$RC") is
#raw("curl")'s exit code — #raw("0") means the server accepted the message — and any error appears in
the #raw("*Shell Output*") tab.

== Receiving

Receiving is deliberately minimal and raw — three verbs, no decoding. All three read the #raw("MAILBOX")
folder on #raw("IMAPURL") and authenticate with #raw("MAILUSER")/#raw("MAILPASS"):

/ #raw("LISTMAIL"): list the mailbox. Each message's number, date, sender, and subject appear in the
  #raw("*Shell Output*") tab, so you can see what is there and pick numbers to act on. The numbers are
  IMAP sequence numbers, #raw("1") being the oldest.
/ #raw("SAVEMAIL <range> [<path>]"): download the raw source of messages to disk, one file each, named
  #raw("MSG001.TXT"), #raw("MSG002.TXT"), … by message number, into #raw("<path>") (created if needed;
  your working directory by default). #raw("<range>") is a comma list of numbers and #raw("1-3") spans,
  or the word #raw("all") for the whole mailbox. Nothing is decoded — you get exactly what the server
  sent.
/ #raw("DELMAIL <range>"): delete messages by the same #raw("<range>") form (here #raw("all") is
  accepted too). The messages are flagged deleted and expunged.

#tip("Numbers shift after a delete")[IMAP renumbers messages once you expunge, so run #raw("LISTMAIL")
again to see the current numbers before a second #raw("SAVEMAIL") or #raw("DELMAIL") — or, when working
from one listing, delete the highest numbers first.]

== When something goes wrong

Turn on #raw("MAILDEBUG on") and repeat the command: #raw("curl") then runs verbosely and the full
conversation with the server streams into #raw("*Shell Output*"), including the server's own final
word — usually enough to see whether a send was truly accepted or a login was refused. For
#raw("LISTMAIL") specifically, #raw("MAILDEBUG") also switches the display from the tidy listing to the
raw exchange. Turn it off again with #raw("MAILDEBUG off").

// ===================================================================================================
= Appendix A — The Reference

The exhaustive, always-current catalog of every command, function, operator, and special variable lives
in the *XyScript Reference* (#raw("Help ▸ Reference")), which is checked against the code automatically.
This manual deliberately does not repeat it. Use the Reference for lookups; use this manual to learn how
the pieces fit together.

= Appendix B — Regular Expressions

With the #raw("/R") switch, the search half of #raw("SE") or #raw("CH") is a *regular expression*.
XyScript uses a linear-time, RE2-style engine: patterns are fast and safe on any input, with one
trade-off — *no backreferences or look-around* in the pattern (alternatives below). A search regex is
case-insensitive unless you use the case-absolute verb (#raw("SEA"), #raw("CHA")); #raw("/W") wraps the
pattern so it must match a whole word.

== Common constructs

/ #raw("."): any character except a newline.
/ #raw("\\d") / #raw("\\w") / #raw("\\s"): a digit / word character / whitespace (#raw("\\D"), #raw("\\W"), #raw("\\S") negate them).
/ #raw("[abc]") / #raw("[^abc]") / #raw("[a-z]"): a character class, a negated class, a range.
/ #raw("^") / #raw("$"): start / end of the text — or of each line under the #raw("(?m)") flag.
/ #raw("\\b"): a word boundary.
/ #raw("*") / #raw("+") / #raw("?"): zero-or-more / one-or-more / optional (greedy; add a trailing #raw("?") for lazy).
/ #raw("{3}") / #raw("{2,}") / #raw("{2,5}"): exactly / at-least / between n and m repeats.
/ #raw("(…)") / #raw("(?:…)"): a capturing / non-capturing group; #raw("a|b") is "a or b".
/ #raw("(?i)") / #raw("(?s)") / #raw("(?m)"): inline flags (case-insensitive, dot-matches-newline, multi-line) for the rest of the pattern.

== Backreferences in the replacement

In a #raw("/R") change, the replacement may reuse what the pattern captured:

/ #raw("$0"): the whole match.
/ #raw("$1"), #raw("$2"), …: the n-th capturing group.
/ #raw("${name}"): a named group, written #raw("(?<name>…)") in the pattern.
/ #raw("$$"): a literal dollar sign.

Swap "Last, First" pairs into "First Last" (the separator is #raw("\"") so the slashes and backslashes
read cleanly):

#ex("CH/R \"(\\w+),\\s+(\\w+)\"$2 $1\"")

== Not supported

The linear-time engine omits the features that require backtracking: *backreferences in the pattern*
(such as #raw("(\\w)\\1")), *look-around* (#raw("(?=…)"), #raw("(?<=…)")), and *atomic groups*. When a
pattern seems to want one, a capturing group plus a replacement — or splitting the job into two commands
— usually does it.

= Appendix C — Sample Scripts

A few short routines to adapt. Drop each into a #raw(".lib") library and call it by name, or run it in
place with #raw("RUNHERE").

*Insert the date and time at the cursor.*

#ex("SUB stamp
  PUT $(DATE(yyyy-mm-dd)) $(TIME(hh:mm))
ES")

*Report the word and character count of the selection.*

#ex("SUB wc
  LOCAL t
  SET t = SEL()
  PR $(LEN(WORDS($t))) words, $(LEN($t)) chars
ES")

*Wrap the selection in a pair of delimiters* — call #raw("wrap **") to bold it in Markdown. The text is
captured first, then put back between the delimiters:

#ex("SUB wrap
  LOCAL t
  SET t = SEL()
  PUT $1
  PUT $t
  PUT $1
ES")

*Strip trailing whitespace from every line, document-wide.* A one-shot regex replace; the #raw("(?m)")
makes #raw("$") mean the end of each line:

#ex("SUB cleanends
  CH/T/R \"(?m)[ \\t]+$\"\"
ES")

= Appendix D — Markdown and Typst Support

XyScript renders a document as either GitHub-Flavored Markdown or Typst markup, for the live preview and
PDF export. The chapter *Preview, PDF Export, and Field Expansion* covers how to choose a
#raw("DOCTYPE"), preview, and export; this appendix lists what the *Markdown* renderer supports.

== What the Markdown renderer covers

The preview and Markdown PDF export support the common GitHub-Flavored constructs:

/ *Block*: headings (#raw("#") … #raw("######")), paragraphs, blockquotes, fenced and indented code blocks, horizontal rules, and tables.
/ *Lists*: ordered, unordered, and nested, plus task-list checkboxes.
/ *Inline*: #raw("*bold*"), #raw("_italic_"), #raw("`code`"), strikethrough, links, and images.

== Typst

For anything more elaborate — fine typography, precise layout, mathematics — author the document in
Typst and set #raw("DOCTYPE typst"). The full Typst language is then available, and #raw("PV") previews
it live.

Typst is a full typesetting language in its own right, with its own extensive documentation. This
manual only notes that XyScript can render and export it; for the complete reference to Typst's markup,
functions, and styling, see #link("https://typst.app/docs/").
