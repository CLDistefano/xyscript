# XyScript — release notes

What changed in each release, in plain terms. The newest release is first. After an
update, XyScript shows the section for the version you just moved to in a one-time
read-only tab; you can reopen it any time from **Help ▸ What's new**.

Each `## v…` section below is the text that tab displays. When a release is cut,
`packaging/gen-news.py <version>` scaffolds a fresh section from the merged pull
requests since the last tag; trim it to the user-facing highlights before tagging.

## v0.1.17

- **One-click updates from the menu.** When a check finds a newer version, the
  **Help** menu now offers **Install update v…** — it downloads and verifies the
  release in the background and applies it cleanly at the next launch, with no
  commands to type.
- **"New in" release notes.** After an update, XyScript opens a one-time read-only
  tab describing what changed, and **Help ▸ What's new** reopens it on demand.

## v0.1.16

New release to enable round-trip testing of auto-updates. Functionally identical to v0.1.15.

## v0.1.15

- **Automatic updates.** XyScript can watch its own release channel and tell you
  when a newer version is out (`UPDATEMODE`), download and verify it, and — at your
  choosing — install it at the next launch. Your edited files are never overwritten:
  a changed default is written beside your copy as `<name>.new`, and `UPDATE
  ROLLBACK` reverses an install in one step. See the manual's *Keeping XyScript up
  to date* chapter.
  
## v0.1.14

- Added Xlinks — in-document hyperlinks that jump to a location (in this file or another) or run arbitrary XPL actions on click, with a per-document trust gate so code from someone else’s file never runs unasked. Modeled on XyWrite-U2's Xyperlinks. The suite of commands and special variables includes:
  - `XLINK <XPL code>` creates an xlink over selected text. Example: `XLINK open report.txt;gotoline 345`   
  - `XLINK/N <name> <XPL code>` creates a named xlink
  - `XLINK/L [file]:<n>` creates an xlink that opens file and navigates to line <n>. Example: `XLINK/L report.txt:345`
  - `XLINK/M [file:]<name>` links to a named bookmark
  - `XLINK/H` creates an xlink to a location previously set with `XLHERE`
    - `XLHERE` sets a named bookmark at the cursor position in the current document
  - `XLGO` (or Ctrl+double click) activates the xlink under the cursor. Xlinks that perform actions are trust-gated (see `XLTRUST` / `XLUNTRUST`, below)
  - `XLGO <name>` activates a named xlink
    - Variable `$XLINK` returns the XPL code associated with the xlink under the cursor; among other things, this enables creating a dual-purpose XC|XLGO key, for example: `F9 :67=IF $TX AND $XLINK;XLGO;ELSE;IF $CM;CMH;XC;EI;EI`
  - `XLINKS`: opens a modal chooser listing every link in the current document
    - Variable `$XLINKS` returns the number of xlinks in the document
  - `XLNEXT` / `XLPREV` navigates to the next/previous xlink in the document
  - `XLFWD` / `XLBACK` moves backward and forward in a browser-style history of the places you've landed by following xlinks
  - `XLDEL` deletes the xlink under the cursor. `XLDEL/A` deletes all xlinks in the document
  - `XLINKHIGHLIGHT on|off|toggle` toggles highlighting of xlinks
  - `XLFGCOLOR(S)` / `XLBGCOLOR(S)` to set xlink foreground and background colors 
  - `XLTRUST` allows document-wide activation of xlinks that perform actions (navigation xlinks always run). Revoke with `XLUNTRUST`
    - Also: `XLTRUST/A on|off` (author mode), where `on` allows repeated edit-test cycles of xlinks without renewing XLTRUST after each edit

- Added support for named bookmarks: `MARKSET <name>` sets a named bookmark at the cursor position; `GOMARK <name>` jumps to it (see also `XLHERE` and `XLINK/H`, above)

- Added command `GOTOLINE <n>` to navigate to line <n> in the current document

- Added `CHECK/S` option to run spelling & grammar check on selected text
  - Enter on a result in the *Language Check* tab jumps to the place in the document

- New option `STATUSPATH on|off`, where `on` displays filename with full path on status line (long paths are truncated in accordance with `PATHLEN` setting)

## v0.1.13

- New setting `PVWIDTH p` [20 >= p <= 80] sets the Preview width as a percentage of the XyScript window width (default value: 50)
  - `PVWIDTH` re-proportions the pane on window resize

- Mac Ascii-13/Cr newlines are now recognized natively (i.e., a Mac text
  file will display correctly in XyScript running in Windows or Linux).

   - NEWLINE command to convert newlines in current file now accepts new
    option Cr (Mac), in addition to CrLf (Windows) and Lf (Linux)
        
## v0.1.12

- Added special variable $XKBDMODE, which identifies the keyboard layout
  active *immediately before* the last KBDMODE command to change modes (empty
  if there was none). Binding `nn=KBDMODE $XKBDMODE` in a .kbd file switches
  to the previous layout; if that .kbd file has the same binding, pressing
  the key repeatedly alternates between the two layouts

- Added `./examples/window.kbd`, a keyboard file with bindings that implement
  the XSW window-management commands, as follows:
  - Esc:    Exit window.kbd (return to last active layout)
  - -:      Decrease window size by 3%
  - +:      Increase window size by 3%
  - F:      Maximize window
  - W:      Toggle maximize
  - R:      Restore windowed
  - M:      Minimize other windows (Windows only)
  - Shift+M: Restore other windows (Windows only)
  - <-,->  :  Move window left|right by 5 pixels
  - ↑,↓:    Move window up|down by 5 pixels
  - 1-9,0:  Increase window size by 1%-9%,10%
  - Sh+1-9,0: Decrease window size by 1%-9%,10%
  - Numpad 8/4/6/2 move; Numpad +/- resize ±3%
  
- New Unicode-related library routines, in ./xyscript.lib:
  - `CHARSE`: Unicode character-name search; e.g.: `CHARSE dot`
	  - Uses `unicode-latin-cyrillic-table.txt`, new in ./examples
  - `UCHAR`: Put single Unicode (UTF-8) character or list a range of chars
	  - Usage: `UCHAR n [- n]` (decimal)

## v0.1.11

Linux build pinned to glibc 2.35 (was 2.43 in v0.1.10), for compatibility
with older distros. Windows and macOS versions unchanged except for version
number

## v0.1.10

- Added XyScript window-management command `XSW`, with cross-platform functionality
  as follows:
  - `XSW max`:             Maximize the XyScript window
  - `XSW rest`:            Restore the XyScript window
  - `XSW tog`:             Toggle max/rest
  - `XSW minall`:          Minimize all windows except XyScript (Windows only)
  - `XSW restall`:         Restore all minimized windows (Windows only)
  - `XSW togall`:          Toggle minall/restall (Windows only)
  - `XSW mv right/left n`: Move XyScript window right/left n pixels
  - `XSW mv up/down n`:    Move XyScript window up/down n pixels
  - `XSW sz +/-n`:         Increase/decrease XyScript window size by n percent

- New command `ADDAR` loads auto-replace files additively (over .ar file
  loaded with `LOADAR`)
  - Added `emoji.ar` auto-replace file, providing ~250 gemoji-style :shortcode:
    replacements; e.g., \:smile\: => :smile: (UTF-8 files only)

- HTML preview/print now works with files having the `.shtml` extension

- New square-bracket usage `RUNCODE [<code>]` avoids need to save `<code>`
  snippets to a variable before passing it to RUNCODE. Incidentally, the
  new usage allows running FOR and WHILE constructs from the CMline. E.g.: \
  `RUNCODE [pr Press a key, Esc quits;while 1; rk k; if $SC==1 break;
  pr $(key($SC)); ew]`

- New XPL verb `RUNMENU` constructs a scrollable menu dialog in which Enter
  or double-click on any item runs the code associated with the item

- Enabled here-strings in XPL, modeled on PowerShell's @"..."@|@'...'@ usage

- Added Emacs-like compilation mode. Commands:
  - `COMPILE <command>`: streams output into a navigable *Compile* tab;
     error: file:line:col: lines become jump targets as they stream in.
  - `GJ` / Enter / double-click on a hit line opens the source at that
     location
  - `NERR` / `PERR` -> next/prev error from any buffer (works across COMPILE
     and GREP listings)
  - Bare COMPILE re-runs the last command (recompile).
  - Requires TRUST (it shells out, like X/BG)

- New routines in `xyscript.lib`:
	- ALARM: Set alarm with optional reminder (runs an XPL child process in
    the background)
	- EMOJI: List major Unicode emoji characters in a new tab
	- VIEW: View current document as PDF in external viewer

## v0.1.9

- `OPEN/EDIT/ED/CA <file>` now returns an error message if `<file>` does
  not exist (previously opened a new blank file)

- Email message-body line endings automatically regularized to CrLf before
  message is sent, as required by SMTP (see RFC 5321 sec. 2.3.8 & RFC 5322
  sec. 2.3) 

- Further optimizations for editing large (>100MB) files

- Fixed bug in HTML Preview/Print-to-PDF/Print-to-SVG (incomplete conversion
  from HTML to Typst markup)


2026-10-20 v0.1.8
-----------------
- Added support for inputting Unicode characters in ranges above \u9999
  using five-digit hex escape codes wrapped in curly braces, e.g. `INSERT "
  \u{1F308}"` for rainbow emoji

- Enabled IME in the editor window for multilingual/accent/CJK/emoji input

- Added command `TOSVG` to print current file to SVG (Scalable Vector Graphics)
  format; output is one .svg file per page (companion to TOPDF/PRINT)

- Added support for printing color emoji to preview, PDF and SVG (`NotoColorEmoji
  -Regular.ttf` in new program subdirectory ./fonts)

- Added new `BUNDLE` / `UNBUNDLE` commands to combine a file and its styling
  metadata (text modes, colorful text, bookmarks, saved as a hidden .xymeta
  file) into a single file (extension .xyb) for tranport and unbundling
  at destination. OPEN/CA/ED/EDIT unbundles a .xyb file on first SAve 
  (opened as read-only if file is already open in another tab)

- New `OUTLINE` (synonym: `OL`) command opens a modal with an outline view
  of the current Markdown/Typst/HTML/XPL file, built from headings (or
  SUBroutine names in the case of an .xpl script)

- Added `DIFF` command to list differences between file in current tab
  and (1) disk version of same file or (2) another specified file; Linux
  -style "diff -u" output

- `HELP` now searches both the command reference and the manual (reference
  results displayed first). `REF` and `MAN` search reference and manual,
  respectively, as before


- Added Boolean, proximity and quoted-phrase search facility to `HELP`, 
  `REF` and `MAN` commands, with new keywords AND/OR/NOT; e.g. `HELP margin
  OR indent NEAR/35 offset`; `HELP "persistent selection"`

- `GREP <pattern> ALLTABS` (synonym: `ALLBUFS`) searches the in-memory
  content of every open document - including unsaved edits and untitled
  buffers that a disk grep cannot reach

- `SAVE` and `STORE` return an error message if file is opened as read-only


- `MDA`/`MDALL` text-mode stylings did not persist afte moves and undos
   -- fixed

- Laggy typing in big files with legacy text encodings (CP437 et al.) and/or
  dense syntax highlighting -- fixed

- Optimization tweaks for opening big files

## v0.1.7

- New multi-layout keyboard facility for immediate switching between KBD
  files (alternate languages, special-purpose layouts, Vim-style modal
  editing). New commands:
  - `ADDKBD <name> <file>` - load a .kbd into a resident named slot
  - `KBDMODE <name>` - switch the operative keyboard instantly
  - `$KBDMODE` / `$KBDMODES` for scripts (returns current mode / all modes)

- New tools that implement Vim-style modal editing: A suite of two KBD
  files (`./examples/vim-nav.kbd`, `./examples/vim-insert.kbd`) and a related
  library file (`./examples/vim.lib`), supplemented by new native commands 
  `UNDOBEGIN` / `UNDOEND`, to facilitate single-stroke undo, and `DIB` / 
  `DIQ` to automate selecting text enclosed in brackets or quotes

- Added option to use proportional fonts in the editor pane: `EDPROPFAMILY 
  <font name>` sets the font; `EDPROPFONTS` opens a font picker; `USEFONT
  mono|prop|auto` sets the font mode for the current document (`auto` uses
  the proportional font, if set, for tabs other than syntax-highlighted,
  Console, and dir|grep output)

- XPL function ReadFile()/RF() and verb WriteFile/WF now support reading
  and writing text files encoded in legacy code pages and binary files,
  as well as a "hex" option to read/write a file as two-character hexadecimal
  bytes. For details, command `REF ReadFile` and `MAN reading and writing files`

- Added library (xyscript.lib) routine VIEWIMG: view image file in the
  Preview pane

- New command `COLORNAMES` lists built-in X11-style color names (about
  650 in all; source: http://web.njit.edu/~walsh), with color swatches;
  the listing can be filtered, e.g. `COLORNAMES red` (replaces library
  routine of same name)

- Named colors can now be used in place of RGB or HSL values in commands
  that set colors; e.g. `URLFGCOLOR blue`.

- New command `MDALL <BO|IT|UL|BU|BI|RV>|<r g b>|<color name> <text>` sets
  text mode/color for all occurrences of `<text>` in file. `MDALL/B` sets
  background color; `MDALL/R|/P` operates on all strings matching a regular
  expression. Undo with `MDALL off <text>|<regex>`

- `RUNHERE` now passes arguments to fenced code via positionals $1, $2, ...
  plus $* and $#

- Foreground and background colors of Preview pane are now adjustable via
  new commands `PVFGCOLOR <r g b>` and `PVBGCOLOR <r g b>` (read back with
  special variables $PVFGCOLOR and $PFBGCOLOR)

- Expanded regex engine now includes support for most Perl-compatible regular
  expressions, including lookarounds and back references. Use switch /P
  in SE, CH, GREP and HLRULE commands to access the new functionality

- New command `HLCOLOR/BG <kind> r g b` sets background colors for syntax
  highlighting

- New command `HLMODE <kind> IT|BO|BI|UL|BU|RV` enables incorporating a
  text-mode setting (italics, bold, bold + italics, underline, bold + underline,
  reverse-video) for syntax higlighing rules

- New syntax highlighting paradigm enables highlighting for big files

- Improved Typst-to-text conversion for HELP|REF results

## v0.1.6

- Preview and PDF printout of plain-text files now give effect to the
following:
  - `PVFAMILY` (preview/PDF font family) and `PVSIZE` (preview/PDF font
  size) settings
  - `MDA` text and color modes
  - Highlighted URLs and EMPHasized text (only if new config setting
  PVMARKS is enabled with `PVMARKS on`)

- Syntax highlighting: `HLRULE` now honors named capture groups -- a
group's name is the token kind for its span, while the ungrouped parts
of the match keep the rule's own kind (see, e.g., ./grammars/xywrite.xpl).
A rule with no named groups behaves the same as before

- Bug whereby built-in date/time functions returned UTC instead of
local time in Linux and macOS -- fixed

## v0.1.5

- Enhanced `HELP` command (synonym:
`REF`) and new command `MAN` perform a grep-like keyword search of the
command reference (reference.pdf) / user manual (manual.pdf) and
directs the results to a read-only document tab. Keywords can
be words/phrases or regular expressions

- New commands `MANPDF` and `REFPDG` open the user manual/reference PDFs
in the default PDF app.

- Enhancements to the `GREP` command: `GREPBEFORE n` and `GREPAFTER n`
to set the number of lines of context for GREP results; and `GREPBLANK
on|off`, where `on` inserts a blank line between GREP results
(readability aid)

- A new command, `FPRINT <source_file> [<output.pdf>]`, prints
<source_file> from disk to PDF

- `PREVIEW`/`PRINT`/`TOPDF`/`FPRINT` now render text files correctly
(previously, non-.md and -.typ files were interpreted as Markdown)


2026-09-13 v0.1.4
-----------------
- Top-level menu now responds to
Alt-key combinations (Alt+F, etc.) in Windows and Linux and to F10 and
Ctrl+F2 in macOS

- Command `SaveCfg <file>` now outputs a comprehensive list of
user-modifiable settings (default output file: `xyscript.cfg`)

- Preview/PDF printout formatting options now include: font family,
font size, code font, code font size, margins, page format
(letter/legal/tabloid/A4/A5/A6), page numbering, line spacing, extra
leading between paragraphs, paragraph first-line indent, header,
footer

- Scratchpad windows for parking notes and snippets alongside the
editing pane: commands `SCRATCH` (open scratchpad), `SCRATCH/V` (open
with text from clipboard), `SCRATCHCLOSE` (close), `SCRATCHCLOSE/V`
(close and insert text into document). Customizable via
`SCRATCHFGCOLOR(S) r g b` and `SCRATCHBGCOLOR(S)

- New document templates in Typst and Markdown formats (business
letter, interoffice memorandum, invoice, to-do list), in `./templates`

- New color themes (`plummy-plum.xpl`, `purple-haze.xpl`), in
`./themes`

- THEME command now has point-and-shoot functionality (point at a DIR
listing to load a color theme)

- Updated syntax-highlighting grammar for XyWrite-formatted files
(`./grammars/xywrite.xpl`)

- Routines in bundled XyScript program library (`xyscript.lib`) are
documented in the reference and user manual. New routines include
COLORS (adjust color settings on the fly) and COLORNAMES
(comprehensive listing of customization colors with RGB values)

- Intermittent bug whereby screen fonts shrink to a tiny size -- fixed

## v0.1.3

- Added new RGB settings to customize the foreground and background
colors of the line-number gutter (`LNFGCOLOR(S)`, `LNBGCOLOR(S)`).
Current values returned by special variables `$LNFGCOLOR` and
`$LNBGCOLOR`

- Added new RGB settings to customize message-box colors: Header
foreground color (`MSGHFGCOLOR`); background color (`MSGBGCOLOR`); OK
button foreground color (`MSGOKFGCOLOR`); OK button background color
(`MSGOKBGCOLOR`). Current values returned by special variables
`$MSGHFGCOLOR`, `$MSGBGCOLOR`); `$MSGOKFGCOLOR`, and `$MSGOKBGCOLOR`

- Added an error logging option. `ERRORLOG on|off` enables/disables;
`ERRORLOGFILE <path>` specifies the name of the error log file
(default: `xyscript-errors.log`, in the XyScript main directory) -
Special variable $ERROR, previously a boolean (0|1), now returns a
positive numeric error code for selected errors

- Added new commands to resize the width of lines of text delimited by
newline characters (for example, in email messages): `REFLOW
[width_in_characters]` and `REFLOWWIDTH` (default width, initial
setting 65 chars)

- New Dvorak keyboard file (`xyscript-dvorak.kbd`, in `./examples`)

- New Emacs starter keyboard layout (`emacs.kbd`, `emacs-kbd.lib` in
`./examples`)

- New color theme (`retro-green.xpl`, in `./themes`)

- `xyscript-default.cfg` now has a comprehensive listing of user-
adjustable settings and their default values

- Improved error trapping/reporting when KBD file fails to load on
startup

- Hover over truncated path display raises a tooltip with full path

- Ctrl+drag moves the XyScript window

- First click in an out-of-focus XyScript window restores focus but
does not move the cursor

- OPEN dialog initially displays current working directory (not the
XyScript main directory)

- Command to launch XyScript and edit a file (`xyscript[.exe]
<file_to_edit>`) now resolves the path based on the OS current working
directory (not the XyScript main directory)

- Status-line messages are erased upon next successful command
execution (avoids stale messages persisting)

- Incomplete internal keyboard table for macOS -- fixed

- Scrolling of LISTTABS open-tabs list was jumpy -- fixed

- `UP`/`DOWN` now reliably moves cursor between logical lines,
`LU`/`LD` between visual lines

- Added XPL template files `lines.txt` and `weather.txt` to ./examples
(see User Manual, Appendix C)

- XPL enhancement: a new `REVERSE` function that takes a string and
returns it with characters reversed (`REVERSE("string")` ==>
`"gnirts"`)

## v0.1.2

- Multilingual user interface
allows switching among the following languages: Bulgarian, Czech,
Danish, Dutch, Finnish, French, German, Greek, Hungarian, Italian,
Norwegian, Polish, Portuguese, Romanian, Russian, Spanish, Swedish,
Ukrainian (ChDir to .\lang, then command `LANG <cc>`, where `<cc>` is
the 2-letter country code; bare `LANG` reverts to English) - Language
strings are user-editable by adding changes to .\lang\local.lang.<cc>

- Enhanced support for right-to-left / bidirectional text (tested with
Arabic and Hebrew)

- Multilingual spell-check via Hunspell

- Multi-cursor / multi-selection editing, made scriptable via a suite
of new XPL commands and special variables

- Duplicate view, for side-by-side editing in adjacent panes or
separate windows

- The `SHELL` / `SH` command now opens a command prompt in a
dedicated, editable Console tab. Command output can be copied, pasted,
edited like any text. The Console tab supports ANSI colors; a new
setting, `ANSICOLOR on|off`, toggles between colors (`on`) and ANSI
escape codes (`off`)

- Added KBD facilities (collected in subdirectory .\examples) -
Dead-accent method for producing accented characters via KBD file
(`dead-accent.kbd`), XPL script (`dead-accent.xpl`), and library
routine (`ACC`, in `xyscript.lib`) - Russian mnemonic KBD file,
Microsoft layout (`russian-mnemonic.kbd`) - These KBD files can serve
as templates for other multi-function keyboard layouts

- Character modes (bold, italics, underlined, bold italics, bold
underlined, reverse video) and colorful text highlighting via the new
MDA ("mode attribute") command. Character and color modes are rendered
in Preview and PDF print-out - To see sample color modes, with
descriptive variable names: navigate to `.\examples`, open
`color-names.lst`, then `RUN addcolors.xpl`

- Eleven new color themes, with complementary syntax highlighting, are
collected in a new .\themes subdirectory, under the directory with
xyscript[.exe]. Reachable via the menus (Settings -> Appearance ->
Theme -> Load theme file) or the command line (`THEME
<[d:\path\]file>`)

- Optional auto-highlighting of URLs in text; Ctrl+click on a
highlighted URL opens it

- Bookmarks that "float" as their location in the file changes due to
editing

- A new option (`STATUSLINE top|bottom`) to move the status line from
the bottom of the screen to the top, just under the command line, as
in XyWrite-DOS

- A new setting (`CLOSEBACK on|off`) to ensure that, after a tab is
closed, focus returns to the last-visited tab

- Editable hex view (new command `HEX on|off`; also `CA/HEX`,
`READ/HEX`)

- XyWrite-style cursor addition via new commands `ADDSUM`, `PUTSUM`,
and `CLRSUM`

- A new calculator command, `CALC`, evaluates numerical expressions
and copies the result to the clipboard. You can also select a list of
numbers and `CALC` will sum them and insert the total in text

- Option to dim the XyScript window when it's not in focus

- Tab panel: - Scrollable when numerous files are open - Hovering over
filename shows full path

- Expanded library of XPL routines in `xyscript.lib` (add `LOADLIB
xyscript.lib` to startup.xpl if necessary; command `LISTLIB` to list
the routines)

- XPL enhancements including: - To ensure that config files load as
expected at startup, the XyScript program folder is automatically set
as the home directory from which relative paths are resolved in
`startup.xpl` - Arbitrary-precision integer math - Python-style string
formatting via a new function `FORMAT()` - New guidance in the manual
and command reference regarding the suitability of using forward
slashes in Windows paths to avoid errors due to backslash escaping

- Updated user manual and command reference

- Assorted minor enhancements and bug fixes

## v0.1.1

- Support for HTML
preview/print-to-PDF, including rendering of raw HTML code in Markdown
files

- Support for key-name annotations in KBD files and a new command
(`KBDDOC`) that adds this documentation to any KBD file

- A clone of the Notepad++ autosave-and-restore option to
automatically save all open files (including modified and untitled) on
quit, close or crash and restore the session on startup (`SESSION
on|off`)

- A GREP search facility, accessible from both the CMline and a
dialog, with point-and-shoot access to files in the results listing

- An option to match case (lower|UPPER|Title) in search-and-replace
operations (Find dialog and new switch /K for the CHange family of
commands)

- Commands that operate on a file now work in point-and-shoot mode
against directory listings

- A `DIRSET` command to set default DIR command switches

- Settings to set line wrap (WRAP on|off) and hanging indent
(WRAPHANG)

- Case-sensitive auto-replace: equ -> equilibrium, Equ -> Equilibrium,
and EQU -> EQUILIBRIUM

- REOPEN command expanded to support `REOPEN n`, to reopen the first n
files in the closed-file list

- A "Writer's View" that minimizes distractions by hiding all user-
interface elements except the text window itself (command WV, or View
-> Writer's View; cancel by pressing Esc)

- A command to add colorful emphasis to all instances of a word or
phrase in the current document (`EMPH word|phrase`, `EMPH/R` matches
regular expressions; cancel with a bare `EMPH`) -- a different way of
searching through a file

- HTML syntax highlighting

- Enhanced XPL (XyScript) syntax highlighting

- Improved navigation in big files

- Improved error trapping in email commands

- XPL enhancements including:

  - A suite of string-classification functions: `ISDEC`, `ISHEX`,
  `ISALPHA`, `ISALNUM`, `ISUPPER`, `ISLOWER`, `ISTITLE`, `ISSPACE`,
  `ISPRINT`, `ISPUNCT`, `ISASCII`, `ISUTF8`, `ISINT`, and `ISFLOAT`

  - A suite of date-arithmetic functions: `DATE2JDN`, `JDN2DATE`,
  `DOW`, `DATEDIFF`, `DATEADD`, `TODAY`, `LEAP`, `DAYSINMONTH`,
  `DAYOFYEAR`, `ISDATE`, and `DATEPARSE`

  - An optional 4th parameter to function `REPLACE(string, from, to,
  [[- ]n])`, where n is the maximum number of replacements to make,
  and negative n causes the replacements to start from the end of
  string

## v0.1.0

Initial release (16 July 2026)
