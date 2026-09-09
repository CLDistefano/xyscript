# XyScript

**A cross-platform, keyboard-driven text editor for people who are serious about writing — modeled on the classic DOS word processor [XyWrite](https://en.wikipedia.org/wiki/XyWrite), and bound together by its own scripting language, XPL.**

XyScript writes plain text. Its center of gravity is not the menu bar but the **command line** — a one-line bar (the *CMline*) where you drive the program with short, mnemonic commands. You can use it on day one as an ordinary editor with familiar shortcuts; a few weeks in you are chaining commands on the CMline; and before long a personal library of routines is a keystroke away. It's especially well-suited to writers who want fine-grained control over their working environment.

A file is always just text on disk, openable in any other editor. XyScript understands two markup flavors for preview and PDF export — [GitHub Flavored Markdown](https://github.github.com/gfm/) and [Typst](https://typst.app) — but never locks your work in a proprietary format.

> XyScript is a work in progress, developed with [Claude Code](https://claude.com/claude-code) and written in [Rust](https://www.rust-lang.org/).

## Features

- **Customizable interface** — fonts, colors, 18 included UI languages (and a template for adding others), built-in dark and light themes plus an expandable collection of user-defined color schemes
- **Live preview and print-to-PDF** for text, HTML, Markdown, and Typst files
- **Spelling and grammar checking** — built-in [Harper](https://writewithharper.com) for English, with optional multilingual spell-checking via [Hunspell](https://hunspell.github.io)
- **Multiple search facilities** — Find, Grep, bookmarks, and word/phrase highlighting
- **Auto-replace** for inserting boilerplate and fixing typos on the fly
- **Writer's View** for distraction-free editing
- **Syntax highlighting** — user-customizable and extensible
- **Multi-cursor / multi-selection** editing
- **Coding aids** — auto-indent, bracket matching, auto-close brackets and quotes
- **Editable hex view** for binary and other files
- **A highly configurable keyboard** — including non-standard modifier keys and add-on behavior such as dead accents
- **Tight OS integration** — a freely editable Console tab, shell command output routed into the editor, and email (send/receive) via `curl`
- **A command line** that gives immediate access to every feature and option
- **XPL**, the XyScript Programming Language — plain-text scripts, readable keywords, expressions with functions/arrays/maps, arbitrary-precision integers, and access to the operating system shell

## Download

Grab the latest build from the [**Releases**](https://github.com/CLDistefano/xyscript/releases) page — one self-contained bundle per platform:

| Platform | Asset |
|----------|-------|
| Windows (x86-64) | `xyscript-windows-x86_64.zip` |
| macOS (universal — Intel + Apple Silicon) | `xyscript-macos-universal.zip` |
| Linux (x86-64) | `xyscript-linux-x86_64.zip` |

Unzip and run — the editor ships with its keyboard, config, themes, library, language catalogs, and docs beside the binary, so it works out of the box. Email requires `curl` on your `PATH` (standard on Windows 10+, macOS, and Linux).

**First-run notes** (details in [`packaging/README.md`](packaging/README.md)):

- **macOS** — the release binary is unsigned, so Gatekeeper blocks it the first time. Right-click ▸ **Open** (then confirm), or clear the quarantine flag: `xattr -dr com.apple.quarantine /path/to/XyScript.app`.
- **Linux** — the binary links against system libraries most desktops already have; a minimal install may need a few (`libxkbcommon-x11-0` is the one most often missing). See the packaging notes for the full `apt` line and a `.desktop` launcher installer.

## Build from source

Requires a recent [Rust toolchain](https://rustup.rs/). The GUI is its own Cargo workspace:

```bash
cargo build --release --features lint,email --manifest-path xyscript-gui/Cargo.toml
```

The binary lands at `xyscript-gui/target/release/xyscript-gui`. The `lint` feature enables Harper spell/grammar checking; `email` enables the mail verbs. To run it as a released bundle would (config, themes, library, docs beside it), copy the contents of `packaging/runtime/`, plus `themes/`, `grammars/`, `examples/`, `lang/`, and the `docs/` PDFs, next to the executable.

## Documentation

- **User manual** (`docs/manual.pdf`) — a guided tour, from first launch to writing your own routines. Open it in the app with `Help ▸ Manual`.
- **Command reference** (`docs/reference.pdf`) — the exhaustive A–Z catalog of every command, function, and special variable, checked against the code automatically. Open it with `Help ▸ Reference`.
- Focused guides in [`docs/`](docs/): [email](docs/email.md), [regular expressions](docs/regex.md), [running shell commands](docs/run-commands.md), and an [overview](docs/overview.md); plus a keyboard chart (`docs/keyboard.png`).
- Runnable samples in [`examples/`](examples/) — starter `.kbd` keyboards (including Dvorak and an Emacs layout), `.xpl` scripts, and PRINTEX templates.

## A taste of XPL

Everything the menus and shortcuts do is an XPL verb you can also type on the command line or put in a script:

```
PR $(UPPER("hello"))          ; prints: HELLO
CALC 2^128                    ; exact big-integer math → the status line + clipboard
SE /needle/                   ; search; $ERROR tells a script whether it found a match

SUB greet                     ; a reusable routine, callable by name (no CALL needed)
  PR Hello, $1!
ES
greet world                   ; → Hello, world!
```

XyScript comes with a library of useful XPL routines -- `xyscript.lib` -- which, by default, is loaded at startup. Modify `xyscript.lib` as you see fit or collect your own routines in a separate `.lib` file (`.lib` files can be loaded additively).

## About

XyScript is modeled on XyWrite, which enjoyed wide popularity in the newspaper and magazine publishing industries in the 1980s and ’90s. XyWrite's hallmark was its lean, command-driven interface; it was known not only for its speed and its ability to automate repetitive tasks, but also for its potential to liberate the writer from the tyranny of menu and mouse — a minimalist environment that aids, or at least does not hinder, the creative process. XyScript pays homage to that tradition, and aspires to give it new vitality.

I developed XyScript using Claude Code (Opus 4.8); it is still a work in progress. The process of creation was — and will continue to be — a slow and iterative one, informed by decades of using, customizing, extending, and talking about XyWrite (see [xywwweb](https://xywrite.org/xywwweb), [XySearch](https://xywrite.org/XySearch/XySearch.php), and the [XyWrite mailing list](https://www.freelists.org/list/xywrite)). I had invaluable assistance throughout from members of the XyWrite community, a worldwide group of dedicated users who remain devoted to this program many years past its heyday.

— Carl L. Distefano, New York City

## Status & license

XyScript is under active, iterative development; expect rough edges and changing internals. When it is released to the public it will be **free and open source under the [MIT License](https://opensource.org/license/mit)** — a permissive license with no non-commercial or other field-of-use restrictions. Until that release, and pending a `LICENSE` file, all rights are reserved.
