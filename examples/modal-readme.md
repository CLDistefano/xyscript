# modal-insert.kbd + modal-nav.kbd — a modal-editing starter pair

Two keyboards that **switch between each other instantly**, demonstrating
XyScript's multi-layout feature (`ADDKBD` + `KBDMODE`). Together they give a
simple Vim-flavored *modal* editor:

- **`modal-insert.kbd`** — an ordinary typing keyboard (the `CLEARKBD`
  baseline). Press **Esc** to switch to the navigation layer.
- **`modal-nav.kbd`** — letter keys **move the caret** instead of typing:
  **h j k l** = left / down / up / right, **w b** = next / previous word,
  **x** = delete the character, **0** = line start. Press **i** or **a** (or
  **Esc**) to return to the layer you came from — these keys run
  `KBDMODE $XKBDMODE`, not a hard-coded `KBDMODE insert`. Every other letter is
  `NOP`, so nothing is typed by accident.

The arrow keys, Backspace, Enter, and the whole **Ctrl** layer (command line,
save, open, clipboard) work in *both* layers, so the editor is fully usable at
all times.

> A *starter*, not a full Vim: this is modal **layout switching** (a different
> key map per mode). The composing grammar — counts (`3dd`), operator-pending
> (`d` then a motion), text objects (`ciw`) — is a layer above this; see the
> *Key Bindings ▸ Multiple keyboard layouts* chapter of the manual.

---

## Install

Put this in your `startup.xpl` (or run it once):

```
ADDKBD insert examples/modal-insert.kbd
ADDKBD nav    examples/modal-nav.kbd
KBDMODE insert
```

- `ADDKBD <name> <file>` loads a keyboard into a **resident, named layer** kept
  in memory. The names `insert` and `nav` are what `KBDMODE` switches between —
  they must match the `KBDMODE` cells inside the files (unshifted Esc, and
  `i`/`a` in nav).
- `KBDMODE insert` makes the typing layer active to start.
- Switching is a **pointer swap** — no file is re-read — so flipping modes is
  instant, however often you do it.
- Once more than one layer is loaded, the **status line** shows the active one.
- The document, selection, bookmarks, and binders are **shared** across layers;
  only the key→action table changes.
- `$KBDMODE` is the active layer and `$KBDMODES` lists all loaded ones, so a
  script can read or display them.
- `$XKBDMODE` is the layer you were in before the last switch, which is why the
  `nav` layer binds its return keys to `KBDMODE $XKBDMODE`: one generic cell
  backs out to wherever you came from, so the same binding works in any layer no
  matter how many you add. (Before the first switch it is empty, and
  `KBDMODE` with an empty name just reports the current layer.)
- `CLEARKBD` returns you to the built-in baseline at any time; a `KBDMODE` to a
  name you have not loaded is a harmless error that leaves the keyboard in
  place (and **Ctrl+Q** always quits), so a typo can't strand you.

---

## Make it your own

Copy either file and edit the cells (the `NN=command` lines; `:NN=` annotations
are for readers and ignored on load). A few ideas:

- Add more motions to `modal-nav.kbd` — e.g. bind a key to `TF` / `BF` (top /
  bottom of file) or `FIND`.
- Build a *third* layer (a symbol/number pad, or another language) with its own
  `ADDKBD name file`, and bind a key in each layer to `KBDMODE name`.
- Keep a mnemonic for the active mode in view: the status line already shows it,
  and `$KBDMODE` is available to any script you write.
