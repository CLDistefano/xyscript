# vim.lib + vim-nav.kbd + vim-insert.kbd — Vim-style modal editing (PROTOTYPE)

A working proof-of-concept of Vim's **composable grammar** built entirely in
XPL on top of XyScript's existing verbs — no engine changes. It exists to let
you *feel* how far the "XPL-on-top" approach gets before deciding whether any
of it is worth doing natively.

## Install

```
ADDLIB  examples/vim.lib
ADDKBD  vimnorm   examples/vim-nav.kbd       # normal / command mode
ADDKBD  viminsert examples/vim-insert.kbd    # insert mode
KBDMODE viminsert
```

Put that in `startup.xpl` to have it every session. **Esc** toggles the modes;
the status line shows which one is active (`vimnorm` / `viminsert`).

## What works

| Keys | Action |
|------|--------|
| `h j k l` | left / down / up / right |
| `w` `b` | next / previous word (`e` ≈ `w`) |
| `0` `$` `^` | line start / line end / first column |
| `gg` `G` | top / bottom of file |
| `3j`, `5w`, `3dd` | **counts** — repeat the motion/operator |
| `d` `c` `y` + motion | **operators**: `dw`, `cj`, `yb`, `d$`, `3dw`, … |
| `dd` `cc` `yy` | linewise (count = lines) |
| `diw` `ciw` `yiw`, `dip`, `dis` | **text objects**: inner word / paragraph / sentence |
| `di(` `ci"` `di{` `ci'` `yi[` … | **inner-delimiter objects** — inside the brackets / quotes (native `DIB`/`DIQ`) |
| `x` | delete char (count-aware) |
| `p` | paste |
| `u` | undo |
| `.` | repeat the last change (`d`/`x`/`y`) at the caret |
| `i a o` / `I A` | enter insert mode (at caret / after / open-below / line-start / line-end) |

The system clipboard is the "unnamed register", so `dw` then `p` pastes what
you deleted. The `Ctrl` layer (save, open, command line, clipboard) works in
both modes, and unmapped letters in normal mode are `NOP`, so nothing types by
accident. This is a *starter* — copy and extend it.

## Where it strains (the point of the prototype)

Spots where a **native** operator-pending layer would still do better. Two of
the gaps the prototype surfaced have since been filled by small native
primitives (noted below); the rest remain:

1. **Counts are one-sided.** A count typed *before* the operator works (`3dw`);
   a count *inside* it (`d3w`) does not. Vim allows both and multiplies them
   (`2d3w` = 6). Supporting that means threading a count through both the
   pending-count global and the operator — fiddly in XPL, free in a native
   input loop.
2. **Every motion is a SUB.** To honour counts, each motion key routes through
   `vim_move` instead of being a one-line keymap cell. That indirection is pure
   boilerplate a native layer would absorb.
3. ✅ **Dot-repeat (`.`) — DONE (pure XPL).** Each operator stashes its resolved
   action (op + how + a SAFE key token + count), and `.` replays it through the
   shared `vim_apply` with no new keypress — no engine change. Still missing:
   `.` after a change (`c`) is a no-op (its re-typed text isn't recorded), plus
   no **visual mode** or named **registers** — those would each be their own XPL
   sub-system, and natively would fall out of the edit/record layer.
4. ✅ **Inner-delimiter objects — DONE.** `ci"`, `di(`, `ci{`, `yi[` … now work,
   backed by the native `DIB` (define inside brackets) and `DIQ` (define inside
   quotes) verbs, which scan outward from the caret. (Still missing: tag objects
   `cit` and the "around" variants `ca(`/`ci"` → `a`.)
5. ✅ **Undo granularity — addressed.** The native `UNDOBEGIN`/`UNDOEND` verbs
   fold a multi-edit command into one undo step. (The simple operators here turn
   out to be a single edit already — a select-then-delete is one unit — so they
   need no wrapping; the group matters once a command makes two or more edits,
   and it's what dot-repeat will build on.)
6. **No operator-pending feedback.** After `d` you're waiting for a motion with
   no cue; a native mode could show a pending indicator.
7. **Completeness is manual.** Every normal-mode key must be mapped or `NOP`'d
   by hand (punctuation here still self-inserts unless you `NOP` it). A native
   mode starts from "nothing types unless it's a command."

## The hybrid these suggest

The common 80–90% (operators over word/sentence/para/line + motions + counts)
is genuinely clean in XPL *because the verbs already exist*. The awkward parts
clustered around three small primitives — and all three are now in hand: an
**undo-group** verb (`UNDOBEGIN`/`UNDOEND` — native), **inner-delimiter** define
verbs (`DIB`/`DIQ` — native), and **dot-repeat** (pure XPL, in this library).
Together they give real Vim feel without a bespoke modal engine baked into the
editor. What's left is the genuinely-stateful stuff (two-sided counts, visual
mode, registers, an operator-pending indicator) — worth a native layer only if
the appetite grows beyond this.

> Status: prototype, but functionally complete for everyday modal editing.
> Loading, compilation, mode-switching, the inner-delimiter objects, and
> dot-repeat are verified; the live operator/motion behaviour is driven from the
> keyboard.
