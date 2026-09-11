// manual.typ — The XyScript User Manual
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
  #text(9pt, fill: luma(140))[Check the version in #raw("Help ▸ About") (\$VE); when this manual and the program disagree, trust the program.]
]
#pagebreak()

// ---------------------------------------------------------------------------------------------------
// Foreword (front matter — unnumbered so it does not renumber the chapters; by Carl L. Distefano).
// The `#show link: underline` is scoped to this block so links elsewhere keep the manual's plain style.
// ---------------------------------------------------------------------------------------------------
#[
  #show link: underline
  #heading(numbering: none, outlined: true)[Foreword]

  XyScript is a cross-platform, multilingual text editor with a host of powerful features. The intended
  audience includes anyone who is serious about writing. It's especially well-suited to writers who want
  fine-grained control over their working environment.

  This user manual (`manual.pdf`) provides an introduction and guided tour. New users can get started
  quickly by using the menus and an array of familiar keyboard shortcuts for editing and file management
  (see Key Bindings, § 13.1, below).

  The command reference (`reference.pdf`) is the unabridged guide to XyScript's feature set and inner workings, organized
  alphabetically and by topic. The features include:

  - Customizable user interface: fonts, colors, 18 included languages and a template for adding others
  - Live preview and print-to-PDF for text, HTML, #link("https://github.github.com/gfm/", "Markdown"), and #link("https://typst.app", "Typst") files
  - Built-in #link("https://writewithharper.com", "Harper") spelling and grammar checking for English, with optional support for multilingual spell-checking via #link("https://hunspell.github.io", "Hunspell")
  - Multiple search facilities (Find, Grep, Bookmarks, word/phrase highlighting)
  - Auto-replace for inserting boilerplate and correcting typos on the fly
  - Built-in dark and light themes, plus an expandable collection of user-defined color schemes
  - Writer's View for distraction-free editing
  - Syntax highlighting, user-customizable and extensible
  - Multi-cursor/multi-selection editing
  - Code writing aids including auto-indent, bracket-matching, auto-close brackets and quotes
  - Editable hex view for binary and other files
  - Highly configurable keyboard, including the ability to create non-standard modifier keys and add-on functionality such as dead accents
  - Easy access to shell commands via a freely-editable Console tab, and tight integration with the underlying operating system (stdout to editor)
  - A command line that gives immediate access to all features and options

  A powerful scripting language---the XyScript Programming Language (XPL)---binds it all together to form a
  cohesive, endlessly adaptable writing instrument.

  XyScript is modeled on the classic DOS word-processor XyWrite, which enjoyed wide popularity in the
  newspaper and magazine publishing industries in the 1980s and ’90s. XyWrite's hallmark was its lean,
  command-driven interface; it was well-known not only for its speed and ability to
  automate repetitive tasks, but also for its potential to liberate the writer from the tyranny of menu
  and mouse by providing a minimalist environment that aids---or, at least, does not hinder---the
  creative process. XyScript pays homage to that tradition, and aspires to give it new vitality.

  I developed XyScript using Claude Code (Opus 4.8); it's still a work in progress.
  The source code is written in the Rust programming language. The process of creation was (and will continue to be) a slow
  and iterative one, informed by decades of using, customizing, extending, and talking about XyWrite (see
  #link("https://xywrite.org/xywwweb"), #link("https://xywrite.org/XySearch/XySearch.php"), and #link("https://www.freelists.org/list/xywrite")). I had
  invaluable assistance throughout from members of the XyWrite community, a worldwide group of dedicated
  users who remain devoted to this program many years past its heyday. Ultimately, I intend to release XyScript
  as an open-source application, to be further developed by anyone wishing
  to do so.

  #v(6pt)
  Carl L. Distefano \
  New York City \
  8 September 2026
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

#tip("Where startup.xpl looks for things")[At launch your *working directory* (#raw("$CWD")) is
wherever you started XyScript from — a shortcut's "Start in", the shell you launched from, or the
program's own folder if you double-clicked it. #raw("startup.xpl") runs first, with that same
#raw("$CWD"), so if it loads your library, colors, or keyboard with *relative* paths, what it finds
depends on how you were launched. The robust habit is to begin #raw("startup.xpl") with
#raw("cd $XSDIR") — the program's own folder, where your configuration files live — or to qualify
every path with #raw("$XSDIR"). Then it loads identically however you start the program, and a second
window opened with #raw("DUPVIEW/W") inherits the very same setup.]

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
- making the same edit in many places at once (*Working with Multiple Carets*);
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
command: a *preview pane* beside the editor (#raw("PV")), the interactive *Console* tab
(#raw("SH")/#raw("SW")), *line numbers* (#raw("SLN")), visible *paragraph marks*
(#raw("SVR")) and *whitespace* (#raw("SWS")), and the menu bar itself (#raw("SMB")). A separate movable
*directory window* opens with #raw("DIR/W").

For distraction-free writing, #raw("WV") — *Writer's View* — hides everything at once: the title bar,
menu bar, tab strip, command line, and status line, leaving just the text. Press #raw("Esc") to bring
them all back. It is a passing mode, never saved, so you always start a session with your normal
layout.

#raw("DIMONBLUR on") dims the window slightly whenever you switch away to another application, so you can
tell at a glance which window is active. (Switching to one of XyScript's own windows — the Find or
#raw("DIR/W") window — does not dim the editor; only leaving the app does.) It is off by default, and is
most useful alongside #raw("TITLEBAR off"), where the window otherwise looks identical whether or not it
has focus. Read the state with #raw("$DIMONBLUR"); the setting is saved with #raw("SaveCfg").

With #raw("TITLEBAR off") there is no title bar to grab, so hold #kbd("Ctrl") and drag anywhere in the
window to move it — the same as dragging a title bar. (A plain #kbd("Ctrl")+click still adds a caret; only
a #kbd("Ctrl")+drag moves the window.)

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

Each pane draws its own document in full: a #raw("FIND") or #raw("EMPH") highlight lights up only the
pane whose file it belongs to, and a pane whose file has bookmarks carries its own gutter dots (see
*Bookmarks*) — nothing bleeds across the divider into a neighbouring pane.

#tip("A two-pane workspace")[Bind a key to #raw("TVS") and another to #raw("TUS") (or #raw("TZ")) and
you can flick a reference file in beside your work and back out without leaving the keyboard. The *Shell
Output* tab can take a pane too — #raw("SW/V") / #raw("SW/H") — so a build's output sits right next to
the code that produced it.]

== Scratchpad windows

Sometimes you want a place to *park* text — a few lines you have cut and are not ready to drop back in,
a snippet pulled from another file, a running list of notes — without cluttering your document or
spending a tab on it. #raw("SCRATCH") opens a *scratchpad*: a small, free-floating window that is
nothing but a plain read / write text box. It has no file and no #raw("SAVE"); it is scratch space,
pure and simple.

Each #raw("SCRATCH") opens another, so several can float at once — move and resize them freely, or drag
one onto a second monitor to keep it beside your work. Typing in a scratchpad uses the ordinary editing
keys (arrows, #kbd("Home") / #kbd("End"), #kbd("Ctrl+C") / #kbd("Ctrl+V") / #kbd("Ctrl+Z")) and the
system clipboard, so text moves easily between a scratchpad and your document. Your keyboard file's
*typing* layer applies too — a custom layout types into a scratchpad the same way it does in your
document — but its command bindings and XPL verbs still act on the main window, not the box (a
scratchpad has no document for them to work on).

If #raw("WINGEO") is on, a scratchpad remembers its size and position: move or resize one, and the next
#raw("SCRATCH") opens where you left it.

Two switches save a step:

- #raw("SCRATCH/V") opens a new scratchpad and drops the *clipboard* straight into it — handy right
  after a cut or copy.
- #raw("SCRATCHCLOSE/V") closes the current scratchpad and inserts its text into the active document at
  the cursor — the natural way to move a parked draft into your file.

Close a scratchpad with its window's close button, or with #raw("SCRATCHCLOSE") (which closes the one
you used most recently). Closing discards the text — there is no save — but the contents of the last
scratchpad you closed are always kept in #raw("$SCRATCHTEXT"), so a stray close is never fatal.

#tip("Recovering a closed scratchpad")[The text of the last scratchpad you closed lives in
#raw("$SCRATCHTEXT"). Click into your document and type #raw("INSERT $SCRATCHTEXT") to bring it back.]

Colour a scratchpad to set it apart from your document: #raw("SCRATCHFGCOLOR") and #raw("SCRATCHBGCOLOR")
set the text and background — as #raw("R G B"), or with #raw("/HSL"); the #raw("SCRATCHFGCOLORS") /
#raw("SCRATCHBGCOLORS") forms open a colour picker. A bare verb or #raw("... default") clears it back to
the theme. Both persist with #raw("SAVECFG") and read back via #raw("$SCRATCHFGCOLOR") /
#raw("$SCRATCHBGCOLOR").

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
/ #raw("LU") / #raw("LD"): one row up / down _by the lines you see_ — following a wrapped line's rows on screen, the way the arrow keys do — keeping your column where it can
/ #raw("UP") / #raw("DOWN"): one _logical_ line up / down, by the file's real line breaks, no matter how the text wraps — the dependable by-line move for scripts and chains
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

== Bookmarks

When you keep jumping between a few places in a long file, drop a *bookmark* at each one and hop
between them. #raw("MARKSET") sets a bookmark at the cursor; #raw("MARKNEXT") and #raw("MARKPREV")
move the cursor to the next and previous bookmark, wrapping around the ends of the file, and scroll
it into view. A dot appears in the left margin on every bookmarked line.

#raw("MARKCLEAR") removes the bookmark at the cursor, #raw("MARKCLEARALL") removes them all, and
#raw("MARKTOGGLE") — the handy one to bind to a key — sets a bookmark if there isn't one at the
cursor and clears it if there is. From a script, #raw("$MARKS") is the count and #raw("$MARK") is
#raw("1") when the cursor is sitting on a bookmark.

When you have several, #raw("MARKLIST") opens a chooser — one row per bookmark, showing its line
number and the text there — and jumps to whichever you pick. It is the quickest way back to a spot
you can recognise but can't remember the order of; bind it to a key for a pop-up "go to bookmark" menu.

The margin strip has three modes, set with #raw("MARKGUTTER"). #raw("MARKGUTTER off") hides it
entirely (the bookmarks still work — you just navigate them with the commands above); #raw("MARKGUTTER
on"), the default, shows the strip only while the file has bookmarks, so an unbookmarked file's layout
is unchanged; and #raw("MARKGUTTER persist") keeps the strip reserved at all times, bookmarks or not,
for a stable left margin. Bare #raw("MARKGUTTER") toggles off and on. In a split layout each pane
carries its own gutter, following that pane's file and the same three modes.

Two colour settings let the gutter blend into the page or stand out. #raw("MARKFGCOLOR") sets the dot
colour and #raw("MARKBGCOLOR") the strip's background — each takes an #raw("r g b") triple (0–255), or
#raw("default") to restore the built-in bookmark orange and a transparent strip; add #raw("/HSL") to
give the triple as hue-saturation-lightness. #raw("MARKFGCOLORS") and #raw("MARKBGCOLORS") open a
colour picker instead. Paired with #raw("MARKGUTTER persist") and a background colour, the gutter
becomes a tidy coloured margin down the left of the page. All of these settings are saved with
#raw("SAVECFG").

All of this is on the menu too, under *Search ▸ Bookmarks*: toggle a bookmark here, jump to the next
or previous one, open the chooser, clear them all, or turn the gutter dot on and off.

Bookmarks belong to the file, not the session: they are saved alongside your other per-document
metadata in the hidden #raw(".xymeta") sidecar and come back when you reopen the file. If the file
was changed by another program in the meantime, a bookmark whose text has shifted is *relocated* to
that text where it can still be found nearby, and the status line notes that it moved.

#tip("A bookmark holds its place as you edit")[While the file is open, a bookmark rides its
surrounding text — insert or delete above it and it moves right along, still marking the same spot.
You only see the "moved" note when the file changed *outside* this session.]

== Right-to-left text and the cursor

XyScript shapes and displays right-to-left scripts — Arabic, Hebrew — correctly out of the box: Arabic
letters join into their cursive forms, and a line whose first strong character is right-to-left is laid
out right-to-left and aligned to the right edge. Mixed lines work too, with the Latin and numeric parts
reading left-to-right inside the right-to-left flow.

The one thing that is not automatic is which way the #kbd("←") and #kbd("→") keys move. By default —
#raw("CARETMOTION logical") — the cursor moves in *logical* order, the order the characters are stored, so
on a right-to-left line #kbd("→") carries the cursor toward the start of the text (the next character in
the text), which reads backwards on screen. Set #raw("CARETMOTION visual") and Left / Right instead move in
*visual* (screen) order: #kbd("→") always steps to the character sitting to the right of the cursor and
#kbd("←") to the one on the left. Inside a right-to-left run the arrows reverse, so the cursor threads
through Arabic or Hebrew — numbers and all — the way your eye reads it, and stays on the side it stepped to
where the two directions meet. Left-to-right text is unaffected either way (logical and visual order
coincide there), so #raw("CARETMOTION visual") is safe to leave set for mixed documents. #raw("CARETMOTION
logical") restores the default; the setting lasts for the session, and #raw("$CARETMOTION") reads it back.

#tip("How far this goes")[This is a prototype. Motion is per-glyph within a line; at a line's visual edge
the arrows move on to the adjacent line — the next line's start, or the previous line's end. #raw("EL") /
#raw("ER") (line start / end) are not yet direction-aware. Straight-line editing of mixed text — the common
case — is what it covers.]

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

== Calculating

The #raw("CALC") command is a calculator built into the editor. It speaks the same expression language
as #raw("$(…)") and #raw("IF") — arithmetic, parentheses, the built-in functions, exact big integers —
so anything you can write in a script, you can add up here.

Give it an expression and it reports the answer on the status line and copies it to the clipboard, ready
to paste wherever you need it:

#ex("CALC (1920 * 1080) / 2     # -> 1036800  (copied)
CALC 2^64                 # -> 18446744073709551616")

With *no* argument, #raw("CALC") works on the document itself. Put the cursor on a line that is an
expression and run #raw("CALC"): it evaluates the line and writes the answer on a fresh line just below
it. (This mode leaves the clipboard alone — the result is right there in the text.)

#ex("17 * 34
        # <- cursor here, run CALC, and the next line appears:
578")

Select several lines first and #raw("CALC") totals them: it evaluates each selected line, adds the
results (blank lines are skipped), and writes the *sum* on a new line below the selection — a quick way
to add up a column of figures. Run #raw("CALC") from the command line with nothing selected and it simply
reminds you to put the cursor on a line first.

The last result is always available as #raw("$CALC"), and every answer honours your #raw("DECIMALS")
setting, so #raw("DECIMALS 2") rounds the display to two places.

=== Cursor addition

When the figures are already sitting in the document — a column of prices, a list of amounts — you rarely
want to retype them into an expression. #emph[Cursor addition] adds them in place, XyWrite-style, with a
running total you build up number by number. Three commands drive it:

/ #raw("CLRSUM"): reset the running total to zero — start of a fresh column.
/ #raw("ADDSUM"): add the number the cursor is sitting on to the total.
/ #raw("PUTSUM"): drop the running total into the text at the cursor.

Put the cursor on a number and run #raw("ADDSUM"); move to the next and run it again; when you have gone
down the column, position the cursor where the total belongs and run #raw("PUTSUM"). The status line shows
each addition and the new subtotal as you go, and #raw("$RUNSUM") holds the total at any moment.

#raw("ADDSUM") is forgiving about how the number is written: a leading currency sign and thousands-separating
commas are stripped before it is read, a leading minus is kept, and integers and decimals mix freely — so
#raw("$1,234.56"), #raw("-42"), and #raw("3000") all add cleanly. The cursor need only be #emph[on] the
number (anywhere within it, or just past its last digit); if it is not on a number, #raw("ADDSUM") says so
and leaves the total untouched. The total is formatted with your #raw("DECIMALS") setting, both on the
status line and when #raw("PUTSUM") writes it, so #raw("DECIMALS 2") keeps currency to the penny.

#tip("Add a column with two keys")[Bind #raw("ADDSUM") and #raw("PUTSUM") to keys (see *Key Bindings*):
#raw("CLRSUM"), then tap the add key down the column, and tap the put key at the bottom. The running total
is global, so one #raw("CLRSUM") starts each new column.]

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
so it works even where you cannot type: a read-only tab, or the #raw("*Console*") pane. Each tab
keeps its own emphasis, and a bare #raw("EMPH") clears the current tab's. You can also reach it from
#raw("Search ▸ Emphasize"), where _Selection_ emphasizes the highlighted word in one click and the
_Word or phrase…_ / _Regular expression…_ items stage the command for you to finish typing. The colors default to your
selection colors; #raw("EMPHFGCOLOR") and #raw("EMPHBGCOLOR") (or their #raw("EMPHFGCOLORS") /
#raw("EMPHBGCOLORS") pickers) give the emphasis a look of its own, and #raw("… default") hands it back to
the selection colors. Read the effective colors as #raw("$EMPHFGCOLOR") / #raw("$EMPHBGCOLOR").

*Links.* #raw("URLHIGHLIGHT on") finds every web address in the document — #raw("http://") and
#raw("https://"), #raw("ftp://"), a bare #raw("www.") host, or a #raw("mailto:") address — and paints it
so it stands out, and #kbd("Ctrl")+click on a highlighted URL opens it in your default browser or mail
program (a #raw("www.") link opens as #raw("https://"); only web and mail addresses are ever launched).
Trailing sentence punctuation is left out of the link, so #raw("(see https://example.org).") highlights
just the address. Like emphasis it is a standing overlay, not an edit, so it works in read-only tabs too;
a bare #raw("URLHIGHLIGHT") toggles it, and it is off until you turn it on. The look is yours to set:
#raw("URLFGCOLOR") colors the link text (a link blue by default) and #raw("URLBGCOLOR") gives it a
background band (none by default) — each takes an #raw("r g b") triple or #raw("/HSL"), with
#raw("URLFGCOLORS") / #raw("URLBGCOLORS") opening a picker and #raw("default") restoring the built-ins.
The setting and colors are saved with #raw("SaveCfg").

#tip("Ctrl+click does two jobs")[With #raw("URLHIGHLIGHT on"), a #kbd("Ctrl")+click that lands on a
highlighted URL opens it; a #kbd("Ctrl")+click anywhere else still adds a multi-caret (see *Working with
Multiple Carets*). The link only wins when you click on one.]

== Deleting

XyScript keeps two independent stashes: the system *clipboard* (Cut / Copy / Paste) and the XyWrite
*deletion buffer*. The deletion verbs use the latter:

/ #raw("RD"): delete the selection into the deletion buffer; #raw("UD") pastes it back (an "undo delete" / move idiom)
/ #raw("BD") / #raw("RC"): rub out the character *before* / *after* the cursor (Backspace / Delete)
/ #raw("RW") / #raw("RL") / #raw("RLL") / #raw("RS") / #raw("RP"): clear the word / the current on-screen (visual) line / the whole logical line / sentence / paragraph at the cursor. As in XyWrite, #raw("RL") deletes the wrapped line you see; #raw("RLL") ("rubout line logical") deletes the entire logical line. With #raw("WRAP off") the two are the same.

#tip("Two stashes, not one")[The deletion buffer (read as #raw("$XB")) and the system clipboard (read
as #raw("$CB")) are separate. #raw("RD") then #raw("UD") moves text without disturbing whatever you have
on the clipboard — exactly the XyWrite delete-and-restore gesture.]

== Changing case

Three verbs change the case of the selection:

/ #raw("UC") / #raw("LC"): upper-case / lower-case
/ #raw("CC"): swap the case of each letter

With *no* selection, each acts on the single character under the cursor and then steps one character
right — so you can sweep along a word, flipping characters as you go, by pressing the key repeatedly.

== Reflowing to a column

#raw("WRAP") is a *display* setting — it wraps long lines to fit the window without touching the text.
When you want to bake wrapping into the text itself — hard line breaks at a fixed width, the way an
email or a plain-text file wants them — use #raw("REFLOW"):

/ #raw("REFLOW <n>"): re-wrap to `n` columns.
/ #raw("REFLOW"): re-wrap to the #raw("REFLOWWIDTH") default (65 to start).
/ #raw("REFLOWWIDTH <n>"): set that default (saved with your config; read it back as #raw("$REFLOWWIDTH")).

It works on the selection if you have one, otherwise on the paragraph (delimited by blank lines) around
the cursor — and a selection spanning several paragraphs reflows each and keeps the blank lines between
them. Inside a paragraph it collapses runs of spaces and existing line breaks to single spaces and then
fills lines greedily up to the width, never splitting a word (a word too long to fit sits on its own
line). The indentation of a paragraph's first line is carried onto every line it produces, so an
indented block stays indented. The whole thing is one undo, and it uses the document's own line ending.

Unlike #raw("LB")/#raw("LE") — whose "visual line" only exists once the text is laid out on screen —
#raw("REFLOW") is pure arithmetic on a fixed width, so it behaves identically whether you run it from
the command line, chain it, bind it to a key, or call it from a script.

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
= Working with Multiple Carets

Most of the time there is one caret — the blinking bar where your typing lands. But XyScript can carry
*many* carets at once, each with its own optional selection. Type, delete, or move, and every caret
does the same thing at the same moment, as a single undo step. It is the fastest way to make one edit
in many places: rename a variable at every use, add a prefix to a block of lines, or wrap a list of
items in quotes.

Everything in this chapter is inert until you actually make a second caret. With a single caret the
editor behaves exactly as the rest of this manual describes, and the classic variables (#raw("$SE"),
#raw("$LN"), …) read that one caret as always. The verbs all begin with #raw("CARET"). One caret is the
*primary* — the window follows it and the classic variables read it — and the rest ride along. The
common ones are gathered under *Edit ▸ Multiple carets* in the menu bar, if you would rather point than
type.

== Making a set of carets

The quickest way is the mouse: hold #kbd("Ctrl") and click to drop an extra caret wherever you click,
and #kbd("Ctrl")-click a caret again to take it away. A plain click, with no #kbd("Ctrl"), collapses
back to one.

From the keyboard, #raw("CARETADD") (#kbd("Ctrl+Alt+A")) plants a caret at the current spot and enters
a *building* state: the arrow keys now move only the primary caret, so you can roam to the next place
and #raw("CARETADD") again, and again. The first edit you make ends building, and from then on the
carets are equal partners. (#raw("CARETADD <n>") plants one at an exact character offset — the
scriptable form.)

To stack a column of carets, use #raw("CARETUP") and #raw("CARETDOWN") (#kbd("Ctrl+Alt+Up") /
#kbd("Ctrl+Alt+Down")): each adds a caret one line above or below every current caret, keeping its
column. Four presses of #kbd("Ctrl+Alt+Down") give you five carets stacked down the same column.

#raw("CARETCOL") turns a *selection* into a column of carets — one on every line the selection spans,
all at the cursor's column (a line too short for that column gets its caret at the line's end). Because
that is the natural thing to want when a block is highlighted, a bare #raw("CARETADD") with a selection
active does #raw("CARETCOL") for you — so #kbd("Ctrl+Alt+A") over a highlighted block lays a caret down
every line at once.

Finally, hold #kbd("Alt") and drag the mouse for a *box* (column) selection: a caret per line across
the rectangle you drag, each selecting the same range of columns. Drag straight down (a zero-width box)
and you get a plain insertion caret on each line.

== Selecting every occurrence of something

Two verbs turn matches into carets, so you can edit every occurrence of a word at once.

#raw("CARETNEXT") (#kbd("Ctrl+D")) is the "add the next one" verb familiar from other editors. With
nothing selected, the first press selects the word under the caret; each further press finds the next
occurrence and adds a caret there, so you build the set one match at a time and watch it grow. It wraps
around at the end of the document. When an occurrence is one you *don't* want, run #raw("CARETSKIP")
instead of pressing #kbd("Ctrl+D") again: it moves your latest selection past that match to the next
one, leaving the unwanted occurrence untouched.

#raw("CARETALL") (#kbd("Ctrl+Shift+L")) does them all at once: a selection-caret at *every* match of
the current selection (or of the word under the caret). Give it a pattern to match that instead, and
add the familiar Find switches — #raw("/R") for a regular expression, #raw("/W") whole word, #raw("/A")
case-sensitive, and #raw("/S") to confine the search to the current selection (turning one selection
into a caret per match inside it). It is capped at 2000 carets and reports the count on the status line.

#ex("CARETALL/R \\bid\\b")

== Editing with many carets

Once more than one caret is active, *every* editing and motion verb acts on all of them — there are no
special multi-caret editing commands to learn. Type and the text appears at each caret; press
#kbd("Backspace") and each caret deletes; word- and line-motions move the whole set (each caret keeps
its own goal column); change case, apply #raw("MDA") styling, run auto-replace — all of it lands at
every caret. The whole keystroke is one undo step, so a single #raw("UNDO") takes it all back.

The clipboard is caret-aware, too. #raw("COPY") or #raw("CUT") with several selections remembers each
piece separately (and puts the newline-joined text on the system clipboard for other apps). When you
#raw("PASTE") into the *same number* of carets, each caret receives its own piece, in order — so you
can lift a column of values and drop them one per line somewhere else. If the counts differ, the whole
clipboard is pasted at every caret instead.

== Splitting a selection into carets

#raw("CARETSPLIT") replaces a selection that spans several lines with one caret at the *end* of each
line it covers. Highlight a paragraph, #raw("CARETSPLIT"), and you have a caret waiting at the end of
every line — ready to add a trailing comma or close a tag.

== Managing the set

/ #raw("CARETONE") (#kbd("Ctrl+Alt+O"), or a plain click): drop every caret but the primary — back to normal.
/ #raw("CARETPOP") (#kbd("Ctrl+Shift+U")): remove the last caret you added, stepping back through the set one change at a time. (Any edit clears this history — it is for building the set, not for the document, which #raw("UNDO") handles.)
/ #raw("CARETPRIMARY <n>"): make caret number #raw("n") (counting from 0 in document order) the primary, so the window follows it.

Switching to another tab, or loading a file, also collapses back to a single caret.

== Reading the carets from a script

For scripts and key bindings, a small family of variables reports the set. #raw("$CARETS") is the count
(#raw("1") when there is a single caret), and #raw("$CARETP") is the primary's index within it.

Every per-caret variable you already know has a mirror array with one entry per caret, in document
order: #raw("$SE") (selected text) has #raw("$MSE"), #raw("$CP") (caret offset) has #raw("$MCP"),
#raw("$WD") (the word) has #raw("$MWD"), #raw("$LN") (line number) has #raw("$MLN"), and likewise
#raw("$MSL"), #raw("$MDF"), #raw("$MDS"), #raw("$MDN"), #raw("$MCS"), #raw("$MCH"), and #raw("$MLI").
They are ordinary 0-based arrays — count them with #raw("LEN($MSE)") (which equals #raw("$CARETS")) and
loop them with #raw("FOR"):

#ex("FOR sel IN $MSE
  PR selection: $sel
EW")

The classic single variables keep reading the primary caret, so #raw("$SE") always equals
#raw("$MSE[$CARETP]").

// ===================================================================================================
= Styling Text with MDA

XyWrite had a "mode" command — #raw("MD") — for stamping typographic modes onto a stretch of text:
boldface, italics, and colors on screen. XyScript brings the idea back as #raw("MDA") ("mode
attribute"). It styles *blocks of text inside a document* — colors and attributes — without turning the
text into Markdown or Typst markup, and the styling sticks with the file between edits.

Three independent layers stack freely, so a run of text can be bold red on a pale background all at
once: a *foreground color*, a *background color*, and a set of *text attributes* (bold, italic,
underline, reverse video).

== Styling a selection

Select some text, then apply a layer:

/ #raw("MDA <r g b>"): color the selected text (the foreground). A color name works too — #raw("MDA red") — and #raw("/HSL") reads the triple as hue / saturation / lightness.
/ #raw("MDA/B <r g b>"): color the *background* behind the selection.
/ #raw("MDA BO") / #raw("IT") / #raw("BI") / #raw("UL") / #raw("BU"): bold / italic / bold-italic / underlined / bold-underlined.
/ #raw("MDA RV"): reverse video — the text is drawn in the background color over a foreground-colored block, so it stays inverted even if you switch themes.

The attribute keywords *replace* whatever attributes the region had — they do not accumulate — so
#raw("MDA BI") makes a run bold-italic in one step, and #raw("MDA NM") ("normal") strips the attributes
off again. To remove a color layer, use #raw("MDA off") (foreground) or #raw("MDA/B off") (background).

The spans follow your edits: type ahead of a styled block and it shifts along; type inside it and it
grows; undo and redo re-track it. The three layers are independent, so clearing one leaves the others
alone. #raw("FGCOLOR") on a selection writes the *same* foreground layer — it is the color-only door
into the same mechanism, and it persists the same way.

== The picker and the menu

#raw("MDAS") opens a color picker for the selection instead of typing numbers — bare for the
foreground, #raw("MDAS/B") for the background. Everything is on the menu as well, under *Settings ▸
Appearance ▸ Colors ▸ Block style*: the two color pickers, the attributes, and *Normal* to clear them.

== Styling as you type

With *nothing selected*, #raw("MDA") becomes a *typing mode* — the modern form of XyWrite's modal "MD".
Arm a mode and everything you type next is styled with it, and the layers stack:

#ex("MDA BO
MDA 0 0 255
The next words come out bold blue.")

To turn a mode off, run the bare command: #raw("MDA") cancels the foreground-color and attribute modes,
#raw("MDA/B") cancels the background mode, and #raw("MDA NM") cancels just the attributes.

Typing modes are *per file* — each tab keeps its own, so switching files never carries one document's
mode into another. When a mode is armed, the status line shows a small reverse-video tag at the left:
the attribute letters (#raw("B"), #raw("I"), #raw("U"), #raw("R")) and a swatch of the armed
foreground / background color, so you can see at a glance what the next keystrokes will look like.

== How the styling is stored

MDA styling is *not* written into your document's bytes — the text stays plain. Instead, when you save,
XyScript writes a small hidden companion file beside it, named #raw(".<filename>.xymeta"). Reopen the
file and the styling comes back exactly. That sidecar is guarded by a checksum of the file's contents:
if the file was changed by another program, the stored positions no longer line up, and the styling is
quietly ignored rather than misapplied.

The sidecar is plain data, never executed, and it keeps step with the file — it follows a
#raw("RENAME"), and it is deleted when you delete the file. Because styling counts as an unsaved change,
the tab's modified marker (and autosave, and the save-on-close prompt) all notice it, so a styling-only
change will not slip away unsaved.

== Styling in the preview and PDF

MDA styling is not only on screen: it is woven into the #raw("PV") preview and the #raw("TOPDF") export,
so it prints as it appears — including underline, color, and reverse video, which Markdown itself cannot
express. It applies to ordinary prose; styling that happens to fall inside markup (a code span, a link's
target) is left to render plainly.

This works for a document authored directly in *Typst* too: the styling is woven around the prose only,
leaving your own Typst markup — headings, #raw("#")-calls, math, code — untouched, so a bold or colored
phrase prints alongside everything Typst does. (HTML documents are the exception; there MDA stays a
screen-only overlay.)

One related knob: #raw("ULSPACES on|off") (default *on*) chooses whether an underline runs through the
spaces between words or breaks at them. It governs both selection styling and the typing mode, and the
printed underline follows the same rule — so the #raw("PV") preview and #raw("TOPDF") output match the
screen.

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

When the app starts, #raw("$CWD") is set to *its own folder* — the directory the XyScript executable
sits in, also readable as #raw("$XSDIR") — not whatever directory you happened to launch it from. That
is what makes XyScript *portable*: put the program and its companions (your #raw("startup.xpl"), keyboard
maps, themes, libraries) in one folder, zip it, drop it on another machine, unzip, and it just runs — a
relative #raw("LOADKBD mykeys.kbd") or #raw("OPEN notes.txt") in #raw("startup.xpl") resolves against
that folder, the same on Windows, macOS, and Linux. From there, #raw("CD") to wherever you keep your
documents (a good last line for #raw("startup.xpl")), or open them by absolute path.

#tip("Portability in one sentence")[Everything the program needs can live beside the executable and be
named with plain relative paths; #raw("$XSDIR") always points there, so #raw("THEME $XSDIR/themes/foolscap.xpl")
is an explicit way to reach a companion file no matter where you have since #raw("CD")-ed.]

#tip("On Windows, prefer forward slashes in paths")[XyScript accepts #raw("/") and #raw("\\")
interchangeably as path separators, so #raw("C:/reports/q3.typ") and #raw("C:\\reports\\q3.typ") name
the same file. Forward slashes are the safer habit, because in a few places — inside a quoted string
value, and in expressions (#raw("SET p = \"…\""), #raw("IF"), #raw("READFILE(\"…\")"),
#raw("FORMAT")) — a backslash is an *escape* character: #raw("\"C:\\temp\"") becomes #raw("C:") + a
TAB, and #raw("\"C:\\new\"") gains a newline (it is case-sensitive — #raw("\\U") is harmless but
#raw("\\u") is not). Forward slashes never trigger this. When you name a file *directly* to a command —
#raw("OPEN"), #raw("READ"), #raw("SAVE"), #raw("CD"), #raw("DIR"), and the like — either slash is fine,
and you may quote the path (needed when it contains spaces): #raw("OPEN \"C:/My Notes/today.typ\"").]

== Opening and creating

/ #raw("NEW"), #raw("NE"): start a fresh, empty document. With a name (#raw("NEW report.typ")) the new document is named and ready to save — or, if that file already exists, it is simply opened.
/ #raw("OPEN"): open an existing file in a tab, editable. (#raw("CA"), #raw("ED"), #raw("EDIT") are synonyms.)
/ #raw("READ"): open a file *read-only*, so you can look without risk of changing it.

Opening a file leaves the cursor in the text, ready to edit. If the file is already open, #raw("OPEN")
asks what you want — open a second copy, switch to the copy already open, or open a read-only view. If
you always mean "just take me to it," #raw("GOTOOPEN on") skips the question and switches straight to
the open copy (#raw("$GOTOOPEN") reads the state, #raw("SaveCfg") remembers it; the default keeps the
prompt). #raw("READ") skips that question a different way and always opens a fresh read-only copy, so
you can keep several views of the same file open at once.

*Two views of one file.* When you want to see two parts of a long file at once — reference the top while
editing the bottom, say — #raw("DUPVIEW") opens a *second view* of the current file, positioned at the
line you are on. A bare #raw("DUPVIEW") opens it as another tab (a separate view with its own cursor and
scroll position); pair it with #raw("NEW/H") or #raw("NEW/V") to sit the two side by side. #raw("DUPVIEW/W")
instead opens the second view in its own *free-standing window* you can drag anywhere — onto a second
monitor, tiled however you like — the way #raw("DIR/W") floats a directory browser. With
#raw("AUTOREOPEN on"), saving in either view updates the other, so the two windows stay in step. (The
on-disk copy is opened, so #raw("SAVE") first if you want unsaved edits carried into the new view; and
the file must have been saved at least once. The #raw("/W") window is a second copy of XyScript itself —
so, like a new instance you launch by hand, it is not gated by #raw("TRUST").)

== Saving

/ #raw("SAVE"): write the active document back to its file. A new, never-saved document has no name yet, so XyScript asks for one — that is *Save As*.
/ #raw("SAAS"): save under a new name and adopt it (#raw("SAAS draft2.md")). If that name is a different existing file you are asked before overwriting; #raw("SAAS/NV") overwrites without asking.
/ #raw("SAD"): save the current *selection* to a separate file. If the target already exists you are asked before overwriting; #raw("SAD/NV") overwrites without asking.
/ #raw("ST"): "store" — save the file and close its tab in one step (the XyWrite habit). #raw("ST report.md") saves to (and names) a given path — the way to store an untitled document; if that path already exists you are asked before overwriting, and #raw("ST/NV report.md") overwrites without asking. A bare #raw("ST") on a never-saved document has no name to save to, so the tab is left open rather than discarded.
/ #raw("SL"): save every open document at once.
/ #raw("REC"): revert the document to its last saved state (#raw("RECALL") is the long form), discarding changes since the last save. The cursor and the viewport are kept where they were, so a revert drops you back in place instead of at the top of the file.

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

When you close a tab with #raw("AB"), XyScript lands you on the tab to its *left* by default. If you would
rather go back to the tab you were *last in* — so opening a file from a #raw("DIR") listing and then
closing it returns you to that listing, not to whichever tab happens to sit to its left — turn on
#raw("CLOSEBACK") (`CLOSEBACK on`, or Settings ▸ Editor behavior ▸ *Close returns to last-visited tab*).
It is a single step back; put #raw("CLOSEBACK on") in startup.xpl to make it the default.

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
#raw("SE") and #raw("FIND"). Point it somewhere else with a second argument — a folder (#raw("GREP cat src")),
a file-name glob over the current folder (#raw("GREP cat *.rs")), or both at once (#raw("GREP cat src/*.rs")) —
just like a #raw("DIR") filespec; and quote a pattern with spaces (#raw("GREP \"foo bar\" src")).

Prefer a form to fill in? A bare #raw("GREP") (or #raw("Search ▸ Grep…")) pops the GREP dialog — a
companion to Find with fields for the pattern and the folder/glob, and the same three options as
checkboxes — and runs the search into a results tab just the same.

Every match is highlighted (using the #raw("EMPH") emphasis) right in the results tab, so the hits are easy
to scan. To open one, put the caret on its line and press #kbd("Enter") (or double-click it, or run #raw("GJ")) —
it opens that file (or switches to it if it's already open, without asking), jumps the caret right onto
the match, and carries the same highlight along so every occurrence lights up there too. Arrow keys and
#kbd("PageUp")/#kbd("PageDown") still scroll the results as usual. Think of the results tab as a launch pad:
#raw("GREP") to gather, #raw("GJ") to pounce.

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

== The hex view

When you need to look at — or edit — a file *as raw bytes*, turn on the hex view with #raw("HEX on")
(bare #raw("HEX") toggles it; #raw("HEX off") returns to the text view, or use *Document ▸ Hex
view*). It is a per-document mode: each tab remembers its own setting, and #raw("$HEX") reports it. The
canvas becomes the familiar hex-dump grid — an 8-digit offset gutter, sixteen bytes per row as hex pairs
(grouped 8 + 8), then a text column showing each byte through the current encoding
(non-printable bytes as `.`). Everything text-only — wrapping, styling, spell-check, syntax highlighting
— steps aside; this is a view of the bytes themselves. To open a file directly in the hex view, add #raw("/HEX") to any of the
open commands — #raw("OPEN/HEX <file>"), #raw("CA/HEX"), #raw("ED/HEX"), #raw("EDIT/HEX"), or
#raw("READ/HEX") (read-only).

*Navigating* is by byte: ←/→ move one byte, ↑/↓ move a whole row (sixteen bytes), Home/End jump to the
row's edges, PgUp/PgDn a screen, and a drag selects a byte range. A click lands the caret on the exact
byte under the pointer, in whichever column — hex or text — you clicked.

*Editing* is in place and *overwrites* by default, so a file keeps its size and its offsets stay put —
what binary formats need. In the hex column, type the digits #raw("0")–#raw("9") and #raw("a")–#raw("f"):
the first sets a byte's high nibble, the second its low nibble, then the caret steps on. Press #raw("Tab")
to jump to the text column, where a typed character sets that byte through the active encoding (so under
CP437 you can type accented letters, box-drawing characters, and the like). #raw("Insert") adds a
#raw("00") byte at the caret; #raw("Delete") and #raw("Backspace") remove the byte at or before it, or
the selection if one is defined. Copy, cut, and paste move raw bytes (paste overwrites at the caret), and
undo/redo behave as everywhere else. A document locked with #raw("RO") stays read-only here too.

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

#raw("AUTOREOPEN on") keeps the editor in step with the outside world: when another program changes a
file you have open, XyScript notices (it checks about once a second) and re-reads it for you, keeping the
cursor and viewport where they were — the same in-place reload as #raw("REC"). A buffer with #emph[unsaved]
edits is never overwritten: the change is noted on the status line and your edits are kept, so you decide
whether to #raw("REC") to the disk copy or #raw("SAVE") over it. It is off until you turn it on, and — unlike
shell-out — it is #emph[not] gated by #raw("TRUST"), because auto-reopening runs no external program; it
only re-reads a file you already opened. #raw("$AUTOREOPEN") reads the state, and the setting is saved
with #raw("SaveCfg").

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
forward and backward, whether or not the dialog is open. Pressing #kbd("Enter") in the *Find* field steps
to the next match — press it repeatedly to walk through them.

As you type in the *Find* field the view follows along — no need to press #kbd("Enter") or #kbd("F3").
Every match is highlighted, and the cursor lands on the first match at or after where you were when you
opened *Find*, scrolling it to mid-screen if it was off-screen. Editing the search text keeps that same
starting point, so the preview searches forward from where you were and never drifts toward the top of
the file as you type. (If you had scrolled the cursor off-screen, "where you were" is the top of what
you could see, not the off-screen cursor — so the search never jumps back up to the top of the file.)
Ticking any option — *Case sensitive*, *Whole word*, *Regular expression*, *In selection* — re-runs the
preview at once, so its effect is immediate.

By default the search begins at the cursor. Tick *From top of file* to begin at the top instead: the
moment you tick it (or type with it ticked) the cursor lands on the *first* match in the document, and
*Find Next* then walks forward from there as usual. It is a starting-point choice, not a permanent
restart — so you can start at the top and still step through every match.

Tick *Wrap around* and *Find Next* past the last match loops back to the first (and *Find Prev* past the
first loops back to the last), instead of stopping with "not found" at the end. (It applies to the
dialog's Find buttons and #kbd("Enter"); the standalone #kbd("F3") repeat is unaffected.)

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

== Spelling in another language

The built-in checker handles English — spelling and grammar both. To check spelling in another language,
XyScript can hand the job to Hunspell, the open-source speller behind LibreOffice, Firefox, and much
else. Switch with #raw("SPELLENGINE hunspell") (#raw("SPELLENGINE harper") returns to the built-in
checker), or from #raw("Tools ▸ Language check ▸ Spelling engine").

Tell it which dictionary to use with #raw("SPELLDICT") and the usual Hunspell dictionary codes —
#raw("SPELLDICT de_DE") for German, #raw("SPELLDICT fr_FR") for French — and list several separated by
commas to check more than one at once, as in #raw("SPELLDICT en_US,de_DE"). This needs Hunspell installed
on your system together with the dictionary files for the languages you name; if the program is not on
your #raw("PATH"), point at it with #raw("SPELLBIN <path>"). #raw("$SPELLENGINE"), #raw("$SPELLDICT"), and
#raw("$SPELLBIN") read the three settings back, and all persist with your configuration.

Once Hunspell is chosen, everything in this chapter that concerns *spelling* — the live underlines, the
whole-document #raw("CHECK") report, and the right-click and #raw("LINTFIX") corrections — comes from it
for the language you picked, and your personal dictionary (#raw("LEARN")) still applies. As with the
built-in checker, only prose is checked: code and links are left alone.

Two things to keep in mind. Hunspell is a speller, not a grammar checker, so grammar underlines
(#raw("GRAMMAR")) are inactive while it is selected — switch back to #raw("SPELLENGINE harper") when you
want English grammar checking. And if the program or its dictionaries cannot be found, spelling simply
produces nothing: no error, no underlines. (This multilingual spelling is newer, and still experimental.)

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

The *status line* can move too. By default it sits at the bottom of the window; #raw("STATUSLINE top")
places it just under the command line, the classic XyWrite arrangement (#raw("STATUSLINE bottom") returns
it; #raw("$STATUSLINE") reads it back). Pair it with #raw("CMPLACE top") for the full top-of-screen
command-line-plus-status layout.

Add a `CMPLACE` (and/or `STATUSLINE`) command to startup.xpl to make your choice persistent.

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

== Putting it together

Chaining, the shell pass-throughs, and highlighting combine into real one-liners. This one — on Windows —
opens a fresh tab, pipes a shell command's output into it, jumps to the top, and lights up the lines that
matter:

#ex("TRUST 1;NE;XY [type services|findstr /i \"time\"];TF;EMPH/R (?i)\\#.*protocol")

Because it is all XPL, the very same thing reads as a plain, commented script — one verb per line:

#ex("TRUST 1                               # enable shell-out (X / XY / XV)
NE                                    # open a new tab
XY [type services|findstr /i \"time\"]  # pull in a shell command's output
TF                                    # cursor to the top of the file
EMPH/R (?i)\\#.*protocol               # highlight protocol comment lines")

You land in a new tab holding just the #raw("services") lines that mention "time", with the protocol
comment lines highlighted. Read the script, change a pattern, and it is yours to keep.

The same recipe travels. On Linux, pull the distinct #raw("man") commands out of your shell history and
flag two of them:

#ex("TRUST 1;NE;XY [cat ~/.bash_history|grep -i \"man\"|sort -u];TF;EMPH/R \"attr|sort\"")

#tip("Shell-out is opt-in")[The #raw("X") / #raw("XY") / #raw("XV") pass-throughs run only after
#raw("TRUST 1") — they are off by default (see *The Console, shell-out, and the working directory*
below), which is why each one-liner starts by enabling them.]

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

== The Console, shell-out, and the working directory

XyScript can hand a line to the operating-system shell. The friendliest way is the *Console*: #raw("SH")
(or #raw("SHELL")) opens an editable REPL tab with a prompt. Type a command, press #kbd("Enter"), and it
runs; the output streams in *above* the prompt while the prompt and whatever you are typing stay pinned
at the bottom. The Console is an ordinary document — you can scroll back, select, copy, and search its
output like any other text, and #kbd("Enter") up in the scrollback just inserts a newline (only
#kbd("Enter") *at the prompt* submits). #kbd("Shift+Up") and #kbd("Shift+Down") walk the commands you
have run this session back into the prompt. #raw("cd") is understood and changes #raw("$CWD"). The prompt
itself is yours: #raw("SHPROMPT \"$CWD> \"") sets it (quote it to keep the trailing space; #raw("$CWD")
and other #raw("$")-fields expand), and #raw("SaveCfg") remembers it.

Colour comes through too: if a command emits *ANSI colour codes* (for example #raw("grep --color=always")
or #raw("git -c color.ui=always …")), the Console shows the colour rather than the raw escape codes —
the codes are turned into the same per-range styling #raw("MDA") uses, so coloured output reads cleanly
and can still be copied as plain text. #raw("ANSICOLOR off") turns this off and leaves the raw codes in
the text (bare #raw("ANSICOLOR") toggles; #raw("$ANSICOLOR") reads it; #raw("SaveCfg") remembers it).

A running command can also *read your input*. While a command is running, what you type at the tail and
send with #kbd("Enter") goes to the command's standard input — so a program that asks a question
(#raw("Name? ")) gets your answer on the same line, and a filter like #raw("sort") reads the lines you
type. Signal end-of-input with #kbd("Ctrl+Z") (or #kbd("Ctrl+D")) and the command finishes and prints
its result, and #kbd("Ctrl+C") interrupts a running command outright (like a terminal). (This is
line-at-a-time input, not a full terminal: a password prompt is *not* hidden, and full-screen programs
like an editor won't run in the Console — run those in a real terminal.)

From a script it is often simpler to run a command directly: #raw("X <command>") runs it and streams the
output to the Console; #raw("XY <command>") inserts the output at the cursor instead. Either way,
shell-out is *disabled by default* and must be enabled with #raw("TRUST 1"), so a stray command can't
reach the operating system by accident; #raw("$TR") reports whether it is on. Shell commands run in your
working directory (#raw("$CWD"), set with #raw("CD")). #raw("SW") shows or hides the Console tab, and
#raw("CLS") (or #raw("CLEAR"), or just typing #raw("cls")/#raw("clear") at the prompt) clears it.

Your working directory is shown on the status line by default; #raw("PAPLACE cmline") moves it to the
front of the command line instead (#raw("PAPLACE status") puts it back). A long path is shortened to
fit — keeping the drive and the trailing folders, with #raw("...") for the elided middle —
and #raw("PATHLEN n") sets how many characters that display may use. Hover the shortened path and a
tooltip shows it in full. #raw("SaveCfg <my.cfg>") makes both settings persist in your config.

#tip("Background jobs")[A long shell command or script can run in the background with the #raw("/BG")
switch (#raw("X/BG …"), #raw("RUN/BG …")) so the editor stays responsive; its output streams to the
*Console* tab, and #raw("JOBS") lists what is running. A background job runs in its own process
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

The library spans text (#raw("UPPER"), #raw("SUBSTR"), #raw("REPLACE"), #raw("INDEX"), …), building
strings from a template (#raw("FORMAT"), its own section below), splitting and joining (#raw("SPLIT"),
#raw("JOIN"), #raw("WORDS"), #raw("LINES")), arrays (#raw("PUSH"), #raw("SORT"), #raw("SLICE")), numbers
and trigonometry (#raw("ABS"), #raw("SQRT"), #raw("POW"), #raw("BASE"), …), and the masked clock
#raw("DATE()") / #raw("TIME()"). The *Reference* lists every one with its signature.

== Formatting strings with #raw("FORMAT")

When gluing pieces together with #raw(".") isn't enough — you want columns to line up, a fixed number
of decimals, or thousands separators — #raw("FORMAT") builds a string from a *template* and a list of
values. Each #raw("{}") in the template is replaced by the next argument in turn:

#ex("PR $(FORMAT(\"{} + {} = {}\", 2, 3, 5))     # 2 + 3 = 5")

Number a placeholder — #raw("{0}"), #raw("{1}") — to choose an argument explicitly or reuse one, and
double a brace (#raw("{{"), #raw("}}")) to print it literally.

After a colon comes an optional *field spec* controlling width, alignment, and number formatting. A
sampler:

#ex("PR $(FORMAT(\"[{:>8}]\", \"hi\"))           # [      hi]     right-aligned in a width of 8
PR $(FORMAT(\"[{:^8}]\", \"hi\"))           # [   hi   ]     centred
PR $(FORMAT(\"[{:*<8}]\", \"hi\"))          # [hi******]     left-aligned, filled with *
PR $(FORMAT(\"{:.2}\", 3.14159))            # 3.14          two decimal places
PR $(FORMAT(\"{:08.2}\", 3.14159))          # 00003.14      zero-padded to width 8
PR $(FORMAT(\"{:,.2}\", 1234567.5))         # 1,234,567.50  thousands separators
PR $(FORMAT(\"{:+.1%}\", 0.087))            # +8.7%         percent, always signed
PR $(FORMAT(\"{:#x}\", 255))                # 0xff          hex with a 0x prefix")

The whole grammar is #raw("[[fill]align][sign][#][0][width][,][.precision][type]"), where the trailing
`type` is one of #raw("s") (string), #raw("d") (integer), #raw("f") (fixed-point), #raw("e")/#raw("E")
(scientific), #raw("%") (percent), or #raw("x")/#raw("X")/#raw("o")/#raw("b") (hex, octal, binary).
Numbers right-align by default and text left-aligns. Because XyScript's integers are
arbitrary-precision, even a huge value formats cleanly:

#ex("PR $(FORMAT(\"{:,}\", 2^128))
# 340,282,366,920,938,463,463,374,607,431,768,211,456")

The *Reference* entry spells out every part of the spec.

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
a failure sets the *error code*, read as #raw("$ERROR"): `0` after success, and a non-zero number after
a failure. A bare truth test still reads naturally, so nothing below has to change:

#ex("SE /needle/
IF $ERROR
  PR Not found.
EI")

An ordinary action verb clears the code (back to `0`) when it starts, so test it on the line right after
the command that might fail. Because a search does not wrap and sets #raw("$ERROR") at the end of its
sweep, the standard "do-something-to-every-match" loop is a #raw("WHILE") that breaks when
#raw("$ERROR") becomes set.

The number is more than a yes/no. Each non-zero value names a *class* of failure — `2` file not found,
`3` nothing selected, and so on — so a script can tell them apart, and #raw("$ERRORMSG[$ERROR]") gives
the generic description of the current one (#raw("$ERRORMSG[0]") is `[none]`). The *specific* message
that reached the status line — with the offending file name, say — is always #raw("$LS"). A failure
that has not yet been given its own number reads as the generic `1`, so the numbers only ever get more
precise over time; code that checks #raw("IF $ERROR") or #raw("$ERROR == 0") keeps working either way.

#raw("$ERROR") is for *foldable* failures — the soft runtime outcomes a script is meant to react to
(a search finds nothing, a file is missing, no text is selected). A *malformed command* — a bad numeric
argument, an unknown verb — is a different thing: it is a mistake in the script, so it halts that
command and reports on the status line, and it does #emph[not] set #raw("$ERROR"). So test #raw("$ERROR")
for conditions you expect and handle, not for typos.

#ex("OPEN report.txt
IF $ERROR
  PR $ERRORMSG[$ERROR]: $LS
EI")

You can raise the code yourself: #raw("SETERR") sets the generic `1`, #raw("SETERR <n>") sets code `n`
(a script can signal its own numbered conditions that way), and #raw("CLERR") clears it back to `0`.

=== Logging errors

Turn on #raw("ERRORLOG on") and every error afterwards — both the trappable kind and the fatal kind that
aborts a script — is appended to a log file, one tab-separated row per error: the tier
(#raw("TRAP")/#raw("FATAL")), a timestamp, the #raw("$ERROR") code and its class, the command that
triggered it, a #raw("file:line") location (blank for an ad-hoc command line), the document that was open,
and the specific message. A #raw("#")-comment header names the columns, so the file both reads cleanly and
splits on tabs for a script that wants to analyze its own errors. #raw("ERRORLOGFILE <path>") chooses the
file; with no path it defaults to #raw("xyscript-errors.log") beside the program (#raw("$XSDIR")). Logging
is off by default, needs no #raw("TRUST"), and never interferes with the run — a log-write that fails is
silently skipped. The settings persist with #raw("SAVECFG"); read them back as #raw("$ERRORLOG") and
#raw("$ERRORLOGFILE").

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

== The bundled library

XyScript ships with a ready-made library, #raw("xyscript.lib"), and the default #raw("startup.xpl") loads
it (#raw("loadlib xyscript.lib")) — so every routine below answers to its name from the first launch,
called like any other command. They are ordinary #raw("SUB … ES") routines, so they also serve as worked
examples: read or adapt any of them with #raw("U2") (which opens the library, optionally at a search
string), and #raw("LISTLIB") shows the whole set with the fuller usage notes each carries in its
#raw("# DOC:") lines. A few read *user variables* you set once in #raw("user-variables.xpl") (copied from
#raw("user-variables.template")); each such routine's listing names the ones it uses, e.g.
#raw("$PDFreader") or #raw("ExternalEditor").

The routines, by what they do:

=== Help and information

/ #raw("kbd"): open the 104-key keyboard chart.
/ #raw("man"): open this user manual.
/ #raw("ref"): open the command reference.
/ #raw("typst"): open the Typst documentation (#raw("$PDFreader")).
/ #raw("idkey"): press a key to see the command it is bound to.
/ #raw("scancode"): show the scan code of a pressed key.
/ #raw("v3"): identify the character under the cursor.
/ #raw("values"): list XyScript's internal variables and their current values.
/ #raw("uservars"): open the user-variables file.
/ #raw("U2"): open #raw("xyscript.lib") itself for editing, optionally at a search string.

=== Directories

/ #raw("d"): change to a directory chosen from a saved list (#raw("$DirList")).
/ #raw("go"): change drive and directory by typing space-separated name fragments.
/ #raw("cdu"), #raw("cdd"): move up / down one directory level.
/ #raw("sd"): a "smart" #raw("DIR") listing.
/ #raw("locate"): find files with #raw("Locate.exe").

=== Files

/ #raw("opn"): open a file in the application associated with its type.
/ #raw("openall"): open every file matching a filespec, e.g. #raw("openall my*.txt").
/ #raw("renas"): rename the current file and reopen it under the new name.
/ #raw("af"): open a fresh throwaway file with an auto-generated name.
/ #raw("ecp"), #raw("emv"): copy / move the current file to a target directory, no overwrite prompt.
/ #raw("sab"): save a timestamped backup of the current file.
/ #raw("wipe"): close every open tab at once, discarding changes.
/ #raw("hash"): copy a file's SHA-256 hash to the clipboard.
/ #raw("siz"): report the length of the selected text.
/ #raw("linecount"): show the active file's line count.
/ #raw("xe"), #raw("xe2"), #raw("xe3"): open the file in external editor 1, 2, or 3 (#raw("ExternalEditor"), …).
/ #raw("pdfedit"): open a PDF for editing.
/ #raw("pdfread"), #raw("pdfread2"): open a PDF for reading (#raw("$PDFreader"), #raw("$PDFreader2")).

=== Editing and transforming text

/ #raw("acc"): apply a dead accent — run #raw("acc"), then an accent key, then the letter.
/ #raw("closeup"): reduce runs of blank lines to at most n (default 2).
/ #raw("closedn"): regularize blank-line spacing to n (default 2).
/ #raw("dellines"): delete every line containing a string.
/ #raw("grablines"): list every line containing a string.
/ #raw("dfb"): select from the caret to the end of the file, then run a command on it — e.g. #raw("dfb DN") deletes it.
/ #raw("dft"): select from the caret to the start of the file, then run a command on it.
/ #raw("puts"): insert a string a number of times; #raw("puts U") rules a line of dashes under the line above.
/ #raw("sef"): search, leaving the caret on the first character of the match.
/ #raw("apfil"), #raw("aptop"): append / prepend the selection to a file.
/ #raw("cmnt"): add or remove comment markers on the defined block (default #raw("#")).
/ #raw("chkcmnt"): check an XPL script for malformed comments.
/ #raw("fence"): wrap the selection in an XPL fenced code block.
/ #raw("qd"), #raw("qe"): quoted-printable decode / encode the defined block.

=== The web

/ #raw("ddg"), #raw("ggl"): search DuckDuckGo / Google for the selection or typed terms.
/ #raw("url"): open a URL — the selection, or one you type.
/ #raw("fnu"): jump to the next URL in the text.
/ #raw("GetIP"): show the current IP address.
/ #raw("checkwebupdate"), #raw("getETag"): detect when a web page has changed, via its ETag.
/ #raw("xysupdate"): check for a XyScript update.
/ #raw("xysweb"): open the XyScript web page.

=== Panes and appearance

/ #raw("panetoggle"): flip the current file between a split pane and full screen.
/ #raw("panezoom"): zoom the current pane to fill the window, keeping the split.
/ #raw("togs"): a menu of interface toggles.
/ #raw("inspect"): inspect and adjust the Appearance settings.

=== Dates and the calendar

/ #raw("cal"): print a month calendar, with holidays for your country (#raw("$CountryCode")).
/ #raw("nthday"): find the nth weekday of a month — e.g. the third Monday.
/ #raw("easter"): compute the date of Easter for a year.

=== Shell, processes, and sessions

/ #raw("cmd"): start a shell session (Windows or Linux).
/ #raw("shel"): run a shell command and show its output in a dialog.
/ #raw("ProcExists"): report whether a named process is running (Windows).
/ #raw("arg2"): pass the line under the cursor as an argument to a command.
/ #raw("clone"): open a second instance with the same configuration.

=== Small utilities

/ #raw("minm"), #raw("max"): the smallest / largest of a list of numbers.
/ #raw("num2words"): write a number out in words, e.g. #raw("num2words 42") gives forty-two.
/ #raw("log"): add a timestamped entry to the top of a log file (#raw("$Logfile")).
/ #raw("awaitfile"), #raw("awaitdel"): wait, with a timeout, until a file appears / disappears.

#tip("It is yours to change")[Every one of these is plain XPL you can read, rewrite, or delete. Trim the
ones you will never use, or fold in your own — #raw("U2") opens the file, and #raw("RELOADLIB") picks up
your edits without a restart.]

// ===================================================================================================
= Key Bindings

XyScript puts the keyboard first, and the keyboard is yours to shape. Bindings live in a *keyboard
file* — a #raw(".kbd") file — that maps each key, in each modifier layer, to a command. If you are
coming from XyWrite the format will look familiar.

== Familiar shortcuts to start with

Until you load a keyboard of your own, XyScript runs its built-in layout — the file
#raw("xyscript-internal.kbd") — and that layout binds the shortcuts most editors share, so a good deal
of what your fingers already know works from the first launch. The highlights, every one of them
remappable:

*Files.*
/ #kbd("Ctrl+N") / #kbd("Ctrl+O") / #kbd("Ctrl+S"): new, open, save.
/ #kbd("Ctrl+W"): close the current file (offers to save first).
/ #kbd("Ctrl+P"): print.

*Editing.*
/ #kbd("Ctrl+Z") / #kbd("Ctrl+Y"): undo / redo.
/ #kbd("Ctrl+X") / #kbd("Ctrl+C") / #kbd("Ctrl+V"): cut / copy / paste.
/ #kbd("Ctrl+A"): select the whole document.
/ #kbd("Ctrl+F"): open Find.

*Styling* (see _Styling Text with MDA_).
/ #kbd("Ctrl+B") / #kbd("Ctrl+I") / #kbd("Ctrl+U"): bold / italic / underline.
/ #kbd("Ctrl+]") / #kbd("Ctrl+["): grow / shrink the editor font (#kbd("Ctrl+Shift+=") also grows it).

*Moving and selecting.*
/ #kbd("Ctrl+Left") / #kbd("Ctrl+Right"): jump one word left / right.
/ #kbd("Ctrl+End"): go to the end of the document.
/ #kbd("Shift") with an arrow: extend the selection as you move — add #kbd("Ctrl") to extend by whole words.

#tip("See the whole map")[These are only the familiar few. *Tools ▸ Keyboard chart* (the #raw("KBCHART")
verb) lists every key on every modifier layer — the quickest way to discover what the shipped keyboard
already does. And because it is nothing but a #raw(".kbd") file, any of it can change: move a shortcut
onto a different key, or point the key at a command of your own.]

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

Because each #raw(";")-separated part of a chain is parsed as a command, a literal *character* you
want to type after a command must be *quoted*. The lone-character shortcut — where a bare character
types itself (see _What a key can do_, next) — applies only when the *whole* binding is that one
character; inside a chain it no longer does. So to auto-replace the word behind the cursor and then
type a comma, quote the comma:

#ex("51=AR;\",\"")

A bare #raw("51=AR;,") would instead try to run #raw(",") as a command — and fail with _unknown
command_.

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

The arrow, numeric-keypad, and #kbd("Page Up")/#kbd("Page Down") keys are yours to bind too. In the
default keyboard the vertical arrows are bound to #raw("LU")/#raw("LD"), so they move by *visual* row —
wrap-aware, following a long line's rows on screen. Bind them to #raw("UP")/#raw("DOWN") instead and
they move by *logical* line (by the file's real line breaks); #raw("PU")/#raw("PD") page as before. Bind
one to anything else — a command, a macro, #raw("GT"), #raw("TH") — and that is what runs. So the numeric
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

#raw("THEME") also takes a *path*: #raw("THEME <file>") activates a theme file — a small
#raw("SAVECFG")-format script of color settings, resolved against #raw("$CWD") and run. Save your own
with #raw("SAVECFG mytheme.xpl"), keep a shelf of them, and switch with a single command
(#raw("THEME themes/charcoal.xpl")). Because a theme file is just XPL that sets colors, you can also
hand-edit one or load it with #raw("LOADCFG"). On the menu it is *Settings ▸ Appearance ▸ Theme* —
*Light*, *Dark*, and *Load theme file…* (which opens a picker and runs #raw("THEME") on your choice).

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
numbers (#raw("SLN") / …), plus the preview pane (#raw("PV")), the Console (#raw("SH")), and the menu
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
= The Multilingual User Interface

XyScript's menus, dialogs, and its most visible messages can display in any of eighteen languages
besides English. It is one program for all of them: the translations are plain-text files loaded at run
time, so changing language is instant and nothing is compiled per locale. What never changes is the
command language itself — the verbs you type stay English in every interface language, for a reason the
middle of this chapter explains.

#tip("Interface language vs. the language you write in")[This chapter is about the language XyScript's
*interface* speaks. Checking the spelling of text you *write* in another language is a separate feature:
the built-in checker covers English, and it can hand spelling off to Hunspell for other languages — see
*Checking Spelling and Grammar*.]

== Choosing a language

The simplest way is the #raw("Settings ▸ Language") menu. It lists English and every translation
XyScript can find, each named in its *own* language — #raw("Français"), #raw("Deutsch"), #raw("Русский"),
#raw("Ελληνικά") — so you can pick yours without having to read English first. Choose one and the
interface changes at once; choose #raw("English") to return to the built-in text. Your choice is saved
with your configuration (see *Customization and Configuration*), so it survives across sessions.

The #raw("LANG") command does the same from the command line. A two-letter code loads that language —
#raw("LANG fr") for French, #raw("LANG el") for Greek — or give an explicit file, as in
#raw("LANG translations/fr.lang"). #raw("LANG") with no argument, or #raw("LANG off"), returns to
English. #raw("$LANG") reads back the active catalog's path, and is empty when English is in force.

The eighteen languages included out of the box, with the code you pass to #raw("LANG"):

Bulgarian #raw("bg"), Czech #raw("cs"), Danish #raw("da"), Dutch #raw("nl"), Finnish #raw("fi"),
French #raw("fr"), German #raw("de"), Greek #raw("el"), Hungarian #raw("hu"), Italian #raw("it"),
Norwegian #raw("no"), Polish #raw("pl"), Portuguese #raw("pt"), Romanian #raw("ro"), Russian #raw("ru"),
Spanish #raw("es"), Swedish #raw("sv"), and Ukrainian #raw("uk").

== What is translated, and what is not

Translated: the menu bar and its dropdowns, the dialog boxes — Find, Grep, Run, About — and the
high-visibility status and error messages ("file not found", "saved…", the search-result and
directory-search summaries, and the like).

Left in English on purpose: the command *verbs*. #raw("SAVE"), #raw("DIR"), #raw("PR"), #raw("LOADKBD")
and all the rest are the command *language* — the words you type, the words your scripts and keyboard
files are written in, and the words this manual documents. If they shifted with the interface language a
script would break the instant you switched, so they stay English everywhere; only the labels you *read*
are translated. (A few data-driven labels, such as encoding names and syntax token kinds, likewise stay
as they are.)

== Where the catalogs live

Each language is a single #raw(".lang") file — #raw("fr.lang"), #raw("de.lang"), and so on — a flat list
of #raw("key = translated text") lines. The shipped set sits in a #raw("lang") folder beside the program;
XyScript also looks in your current working directory and next to the executable, so a catalog can go
wherever is convenient. The #raw("Settings ▸ Language") menu is assembled from whatever turns up in those
places, each entry labelled by the native-language name in the file's header line.

== Tweaking a translation

If a label reads awkwardly, you needn't edit — and re-edit after every upgrade — the shipped catalog.
Make an *override* file named #raw("<code>.local.lang") — #raw("fr.local.lang") for French — holding only
the lines you want to change, and put it beside the shipped catalog (in the #raw("lang") folder) or in
your working directory.

To change one string, find its line in the shipped #raw("<code>.lang"). A French entry looks like

#raw("menubar.file = Fichier   # File")

— a key, your text, and after the #raw("#") the English it was translated from. Copy the line into your
override and edit the middle:

#raw("menubar.file = Dossier   # File")

Keep the #raw("# File") part: it is the English *source*, and XyScript reads it to keep the override
honest. Your version then wins over the shipped one, and because only the base catalog is replaced on an
upgrade — your overlay is never shipped — your changes ride through updates untouched.

#tip("Self-healing overrides")[If a later version reworks the English a key is built from, your override
— written against the *old* English — is quietly skipped for that one line, so you see the current
built-in text instead of a stale translation. Update the line, and its #raw("# …") source to match, to
put your version back in force.]

== Contributing a language

A language XyScript doesn't ship is simply a catalog it hasn't met yet. To begin one, copy #raw("en.lang")
— the English source of truth, in the #raw("docs/i18n") folder — translate the value to the right of each
#raw("="), leave the keys and the #raw("# …") English sources alone, save it as #raw("<code>.lang"), and
drop it where XyScript looks. It joins the #raw("Settings ▸ Language") menu automatically, named by the
#raw("— NATIVE (English)") line at the top of the file. The #raw("docs/i18n") folder holds the tooling and
conventions that keep a full translation in step with the program.

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
  [#raw("cpp.xpl")], [C / C++], [#raw(".c .h .cpp .cc .cxx .hpp .hh .hxx")],
  [#raw("zig.xpl")], [Zig], [#raw(".zig")],
  [#raw("python.xpl")], [Python], [#raw(".py .pyw")],
  [#raw("javascript.xpl")], [JavaScript / TypeScript], [#raw(".js .mjs .cjs .ts .jsx .tsx")],
  [#raw("html.xpl")], [HTML], [#raw(".html .htm .xhtml")],
  [#raw("shell.xpl")], [Shell], [#raw(".sh .bash .zsh .ksh")],
  [#raw("toml.xpl")], [TOML], [#raw(".toml")],
  [#raw("typst.xpl")], [Typst], [#raw(".typ")],
  [#raw("xpl.xpl")], [XPL (XyScript's own)], [#raw(".xpl .pm .pgm .cfg .lib")],
  [#raw("xywrite.xpl")], [XyWrite], [#raw(".xy .xy3 .xy4")],
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

Several recipients? Pass the whole comma-separated list as one quoted #raw("To:") argument:

#raw("SENDMAIL \"alice@example.com, bob@example.com, carol@example.com\" \"Lunch Thursday?\"")

To set #raw("Cc:")/#raw("Bcc:") as well, start the document with a header block — #raw("To:"),
#raw("Cc:"), #raw("Bcc:"), #raw("Subject:"), #raw("From:") lines, then a blank line, then the body —
and run a bare #raw("SENDMAIL"). Recipient lists are comma-separated in either form (a comma inside a
quoted display name, #raw("\"Public, John\" <j@x.org>"), does not split the address). To send a file you
have not opened, #raw("SENDMAIL/F <file>") reads the message from that file instead of the current
document (quote the path if it has spaces).

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
the #raw("*Console*") tab.

== Receiving

Receiving is deliberately minimal and raw — three verbs, no decoding. All three read the #raw("MAILBOX")
folder on #raw("IMAPURL") and authenticate with #raw("MAILUSER")/#raw("MAILPASS"):

/ #raw("LISTMAIL"): list the mailbox. Each message's number, date, sender, and subject appear in the
  #raw("*Console*") tab, so you can see what is there and pick numbers to act on. The numbers are
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
conversation with the server streams into #raw("*Console*"), including the server's own final
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

== The examples directory

The snippets above are the small end of the idea. The project's #raw("examples/") folder ships
complete, ready-to-run samples — a whole keyboard layout, input helpers, a styling demo, #raw("PRINTEX")
templates — and they
repay a closer look for a reason past what each one happens to do: every one is assembled from the
*same building blocks XyScript hands you*. Command verbs, keyboard files, string and regular-expression
functions, the #raw("MDA") styling layer — XyScript's own features are built from exactly these, and so
are the examples. Nothing here reaches for a private door you cannot open yourself. Read them as
templates, borrow freely, and grow your own add-ons the same way; a surprising amount of "the editor"
is just XPL you are free to rewrite.

What ships today:

/ #raw("russian-mnemonic.kbd"): a *mnemonic* Russian keyboard. Each Latin key types the
  phonetically-nearest Cyrillic letter (#kbd("K") → к, #kbd("T") → т), with smart digraphs for the
  sounds English spells with two letters (#kbd("S") then #kbd("H") → ш). An entire alternate alphabet,
  start to finish, in one #raw(".kbd") file — load it with #raw("LOADKBD examples/russian-mnemonic.kbd").
/ #raw("xyscript-dvorak.kbd"): the *Dvorak* keyboard layout — the full shipped keymap
  (#raw("xyscript-internal.kbd")) with its letter keys remapped to Dvorak, a drop-in alternate for Dvorak
  typists. Load it with #raw("LOADKBD examples/xyscript-dvorak.kbd") (and #raw("CLEARKBD") to return to the
  default). Where #raw("russian-mnemonic.kbd") builds a layout from scratch, this shows the other route:
  take the keymap XyScript already ships and re-letter it.
/ #raw("dead-accents.kbd") + #raw("dead-accents.xpl"): *dead-accent* composition. Press an accent —
  #raw("'") acute, #raw("`") grave, #raw("~") tilde, #raw("\"") umlaut, #raw("^") circumflex, #raw(",")
  cedilla/ogonek — then a letter, and the two combine into é, ñ, ü, ç. The keyboard file wires the
  accent keys to the script, which holds the composing logic: a #raw(".kbd") and an #raw(".xpl")
  cooperating.
/ #raw("emacs.kbd") + #raw("emacs-kbd.lib"): a starter *Emacs* keyboard, and the payoff of "it's all
  XPL." Load both — #raw("ADDLIB emacs-kbd.lib") then #raw("LOADKBD emacs.kbd"). The single-stroke Emacs
  bindings (#raw("C-f"), #raw("C-a"), #raw("M-f"), the #raw("C-SPC") mark, #raw("C-w")/#raw("C-y") kill
  and yank, …) are ordinary keymap cells. The multi-key *prefix chords* — #raw("C-x C-s"), #raw("C-x 2"),
  … — the #raw(".kbd") format has no native syntax for, so #raw("C-x") calls a library routine that reads
  the next key with #raw("READKEY/D") and runs whatever the maps hold. It even tells a Ctrl-modified
  follow-up from a plain one — #raw("C-x C-s") (save) vs #raw("C-x s") (save all), like real Emacs — by
  checking the modifier name #raw("READKEY/D") reports. The maps *are* the keymap: plain data in the
  library, so you can rebind at runtime (#raw("SET cx_map[19] = \"REDO\"")) and list the bindings with
  #raw("C-x C-h"). It shows a keyboard file, a library, and #raw("READKEY") / #raw("RUNCODE") / map
  lookups cooperating into a data-driven prefix keymap — and, in its footer, where the emulation still
  shows its seams (no #raw("C-u") prefix argument).
  #raw("emacs-readme.md") is the companion guide: the full binding tables, how the prefix dispatch works,
  and how to rebind or extend it.
/ #raw("addcolors.xpl") + #raw("color-names.lst"): a *styling demo*. The list is 653 named colours,
  each written as an #raw("R G B") triple; open it, run the script, and it walks the file with a regular
  expression, painting every value with its own colour as a background swatch via #raw("MDA/B"). Ten
  lines that turn regex, string functions, and the styling layer into something you can see.
/ #raw("multicaret.xpl"): a *multi-caret tour*. Three self-contained steps you can watch run — rename
  every match of a word at once with #raw("CARETALL"), append to every line of a list with
  #raw("CARETSPLIT"), then read the whole caret set back from a script through the #raw("$M…") mirror
  arrays (#raw("FOR i, piece IN $MSE")). See the *Working with Multiple Carets* chapter.
/ #raw("lines.txt"): a *#raw("PRINTEX") template*. Open it and run #raw("PRINTEX") (see *Filling in a
  template*) to expand every #raw("|…|") *field* in place: #raw("|$(date(yyyy-mm-dd))|") and
  #raw("|$(time(h:mm PM))|") stamp today's date and time, #raw("|C1|") auto-numbers the lines (1, 2, 3…),
  and a row of *decorated counters* — #raw("|C1 \"(a)\"|") for letters, its siblings for numbers and Roman
  numerals — renders a multi-level outline that renumbers itself on every expansion. Fields only: no live
  code, no #raw("TRUST") needed.
/ #raw("weather.txt"): a *live #raw("PRINTEX") template* — the powerful end of the same idea. Under
  #raw("TRUST 1") a #raw("|…|") field can hold real XPL, and here each one shells out to #raw("curl") for a
  current METAR report from a NOAA airport station, captures the output (#raw("$XO")), and a short loop
  pulls out the decoded observation. Run #raw("PRINTEX") and the page fills itself with live conditions at
  JFK, Toronto, Honolulu, Heathrow, and Helsinki. (Needs #raw("TRUST 1") and a network connection — a
  field runs code only when you trust it.)

#tip("The point is not the samples — it is that you could have written them")[Everything in
#raw("examples/") is made of verbs and files this manual documents: #raw("LOADKBD"), #raw("SUB"),
#raw("SE/R"), #raw("MDA"), the string functions of the *XPL* chapter. The same pieces compose into
keyboard add-ons, input methods, one-off tools, and background scripts. If XyScript does not yet do some
thing you want, the odds are good you can teach it.]

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
