# emacs.kbd — a starter Emacs keyboard for XyScript

A keyboard layout that brings the everyday Emacs bindings to XyScript, plus its
companion library `emacs-kbd.lib`. Single-stroke bindings (`C-f`, `M-w`, …) are
ordinary keymap cells; the multi-key **prefix chords** (`C-x C-s`, `C-x 2`, …)
are driven by a small **data table** in the library, so they are easy to read,
rebind, and extend.

> It's a *starter*: enough to move, edit, search, and manage files and windows
> the Emacs way — not a complete Emacs. Copy it and tune it to taste.

---

## Install

Both files live in `examples/`. Load the library **before** the keyboard — put
this in your `startup.xpl` (or run it once):

```
ADDLIB emacs-kbd.lib
LOADKBD emacs.kbd
```

- `ADDLIB` (not `LOADLIB`) *adds* the routines to your current library instead
  of replacing it.
- Single-stroke bindings work even without the library; only the `C-x` and `Esc`
  prefixes call into it.
- `CLEARKBD` returns you to the built-in baseline keyboard at any time.

---

## Bindings

### Moving

| Key | Does | Verb |
|-----|------|------|
| `C-f` / `C-b` | forward / back one character | `CR` / `CL` |
| `C-n` / `C-p` | next / previous line (visual, wrap-aware) | `LD` / `LU` |
| `C-a` / `C-e` | beginning / end of line | `EL` / `ER` |
| `M-f` / `M-b` | forward / back one word | `NW` / `PW` |
| `C-v` / `M-v` | scroll down / up a page | `PD` / `PU` |
| `M-<` / `M->` | beginning / end of buffer | `TF` / `BF` |

### Editing

| Key | Does | Verb |
|-----|------|------|
| `C-d` | delete the character ahead | `RC` |
| `C-k` | kill to end of line | `RE` |
| `M-d` | kill the word ahead | `RW` |
| `C-o` | open a line (insert newline, stay) | `PUT $NL;CL` |
| `C-j` | newline | `ENTER` |
| `C-/` | undo | `UNDO` |

### The region and the kill ring

| Key | Does | Verb |
|-----|------|------|
| `C-SPC` | set the mark — then any motion extends the region | `DF` |
| `C-w` | kill (cut) the region | `CUT` |
| `M-w` | copy the region | `COPY` |
| `C-y` | yank (paste) | `PASTE` |

### Search and help

| Key | Does | Verb |
|-----|------|------|
| `C-s` / `C-r` | incremental search (live) | `FIND` |
| `C-h` | help | `HELP` |
| `C-g` | cancel the current selection | `DZ` |

### `C-x` — files, buffers, windows

The `C-x` prefix reads the next key and runs its command. Like real Emacs, it
tells a **Ctrl-modified** follow-up from a **plain** one, so `C-x C-s` and `C-x s`
are different commands.

| Key | Does | Verb | | Key | Does | Verb |
|-----|------|------|-|-----|------|------|
| `C-x C-s` | save buffer | `SAVE` | | `C-x s` | save all modified | `SL` |
| `C-x C-f` | find file (dialog) | `OPEN` | | `C-x b` | switch buffer | `BUFALT` |
| `C-x C-c` | save and quit | `QUIT` | | `C-x k` | kill buffer (asks to save) | `AB/V` |
| `C-x C-b` | list buffers | `LISTTABS` | | `C-x o` | other window | `TW` |
| `C-x C-h` | which-key list | `emacs_keys` | | `C-x 2` `3` `1` `0` | split / only / close pane | `THS` `TVS` `FS` `TUS` |

### `M-x` and the Meta prefix

| Key | Does | Verb |
|-----|------|------|
| `M-x` | run a command (opens the command line) | `BC` |

`M-` is **Alt**. It's also available as the **`Esc` prefix**, the Emacs way:
`Esc f` = `M-f`, `Esc x` = `M-x`, and so on. Because `Esc` is a prefix, it no
longer cancels in plain editing — modal dialogs still get it. Prefer
`Esc`-to-cancel? Set the `Esc` cell in `emacs.kbd` to `:1=KF`.

---

## How the prefix chords work (and why the library)

The `.kbd` format binds one key per cell; it has no native syntax for a sequence
like `C-x C-s`. So `C-x` is bound to a routine, `emacs_cx`, that:

1. reads the next keystroke with `READKEY/D` (the `/D` gives a readable key
   description, e.g. `"CTRL+S"`),
2. picks `cx_ctrl_map` if that description contains `CTRL`, else `cx_map`, and
   looks the follow-up's scancode up in the chosen table,
3. runs the command it finds with `RUNCODE`.

The maps *are* the keymap — plain data in `emacs-kbd.lib`:

```
SET cx_map      = {31:"SL", 48:"BUFALT", 37:"AB/V", 3:"THS", 4:"TVS", 2:"FS", 11:"TUS", 24:"TW"}
SET cx_ctrl_map = {31:"SAVE", 33:"OPEN", 46:"QUIT", 48:"LISTTABS", 35:"emacs_keys"}
```

Keys are the follow-up key's **scancode**; values are XPL commands. The Ctrl
test is by modifier **name** (from `READKEY/D`), so it doesn't depend on how the
modifier bit values happen to be numbered.

### Customizing

- **Rebind at runtime** — no editing the file:

  ```
  SET cx_map[19] = "REDO"      # 19 = the R key  ->  C-x r now redoes
  ```

  (Find a key's scancode with `KBCHART`, or with `READKEY/D` in a scratch
  script.)

- **List the bindings** — press `C-x C-h`, or call `emacs_keys`, which iterates
  both maps and shows them.

- **Add another prefix** (say `C-c`) — give it its own map and a two-line routine
  modeled on `emacs_cx`, then bind a `ctrl C` cell to it. That's the whole
  pattern: a table plus a dispatcher.

Because the dispatch lives in a `.lib`, its routines are compile-checked when the
library loads (`ADDLIB`) — a typo surfaces then, not later when you press the
key.

---

## Caveats

- **Ctrl vs plain follow-up** *is* distinguished (via `READKEY/D`), so `C-x C-s`
  and `C-x s` differ — but only **Ctrl** is tested. A follow-up carrying some
  other modifier (Alt, Shift) falls through to the plain `cx_map`; extend
  `emacs_cx` if you need to tell those apart too.
- **No prefix argument** (`C-u N`) and no keymap inheritance — those would be
  more XPL on top of this.
- **`Esc` as Meta** replaces `Esc`-to-cancel in plain editing (see above).
- The bindings map Emacs *intent* onto XyScript verbs; a few differ in detail
  (e.g. `C-a` goes to the logical line start; `C-n`/`C-p` follow *visual* wrapped
  lines). Retune any cell you like.

---

## Files

| File | What |
|------|------|
| `emacs.kbd` | the keyboard layout (single-stroke cells + the `C-x` / `Esc` prefix cells) |
| `emacs-kbd.lib` | `cx_map` / `meta_map` tables and the `emacs_cx` / `emacs_meta` / `emacs_keys` routines |
| `emacs-readme.md` | this file |
