---
title: development-plan
tags:
created: 2026-06-05
status: near-term roadmap (current stage + next two)
---
# XyScript — Staged Development Plan (near-term)

*A personal roadmap. Scope: where the build is **now** and the next two stages
that follow from it. Companion to `keyboard-layer-design.md` (the design spec)
and the `xyscript-kbd` prototype crate.*

The foundation of XyScript is the **configurable keyboard layer** that controls
the keyboard and can release keystrokes back to the OS (`KF`). That layer is
already built and verified headless; everything below sequences the work that
turns it into an editor. This document deliberately stops after Stage 2 — the
first point at which you can type into XyScript and see text — and only sketches
what lies beyond.

---

## Foundational premise — read this first

Three load-bearing principles govern **every** decision on this project. They
sit above the stages and override any framing below that drifts from them.

1. **QN is a feature *floor*, not a blueprint.** QuickNotes (QN,
   `C:\Users\cldis\QN2`) defines the *minimum* end-user feature set XyScript must
   eventually meet or exceed — the proven floor for *what the editor and its
   scripting language do for a user*. XyScript's feature set will be a
   substantial **superset** of QN's. QN is the reference for **features and
   behavior**, and for nothing else.

2. **QN's Python/Tkinter *structure* does not factor into XyScript's architecture
   at all.** XyScript starts from a **fresh slate in Rust**, with every structural
   possibility Rust opens up on the table — a real type system, ownership-based
   buffers, a proper parser/AST/IR, true concurrency, an owned regex engine, and
   so on. That QN happens to use a line/PC machine, a textual `$`-substitution
   pre-pass, stringly-typed slots, Tk-delegated text operations, or an
   `after_idle` run loop is **descriptive trivia about QN, never a constraint on
   XyScript.** Where a QN implementation detail appears in this plan, it is there
   only to explain a *feature's behavior* — not to propose how XyScript builds it.

3. **The sole non-negotiable is the keyboard-capture model.** XyScript's *sine qua
   non* — the thing it exists for — is the configurable keyboard layer that
   captures the keyboard and can release keys back to the OS (`KF`). This is
   itself a **stark departure from QN**, which uses Tk-wide `LOADKEY`/`.qnk`
   bindings that XyScript retires entirely. The keyboard model anchors the design;
   everything else is free to be redesigned around it.

**Consequence for every decision tree.** Start from *"what is the best Rust-native
design for this feature?"* and consult QN **only** to fix the floor of behavior.
Treat framings like "port QN's X," "be bug-compatible with QN," or "match QN's
internals" as off-pattern — reread them as **"meet QN's feature floor for X,
designed fresh in Rust."** Decisions and trade-off analyses produced for this
project should make this premise explicit rather than re-litigate it.

---

## Where things stand today

The keyboard layer prototype (`xyscript-kbd`) implements the full design: the
`(bitmask, key_code) -> assignment` model, bottom-up bit assignment, hold/toggle
modifiers, the tap-vs-hold dual-role state machine, strict lookup with swallow,
`KF` pass-through, the access-controlled injection channel, transactional
`LOADKBD` over an embedded default, and per-OS interception scaffolding for all
three platforms.

What is **proven**: the headless engine semantics. Two independent Python
oracles (parser/lookup against the real `CLD4R.KBD`, and a runtime simulation)
plus the Rust unit tests corroborate the model — bit values, dual-role tapping,
toggle latching, KF/miss/dispatch disposition, injection determinism.

What is **now proven on real hardware** — the §12 interception experiment has
**passed on all three platforms** (2026-06-07):

- **macOS — Sequoia (15) / Apple Silicon (ARM).** The highest-risk path: a
  sophisticated tester reported the `diag` run performing correctly, confirming
  the `macos.rs` `FlagsChanged` reconstruction of clean down/up (Caps Lock
  especially) and the non-exclusive `CGEventTap` behave as designed on-device.
  This was the single load-bearing risk of the whole project.
- **Windows.** The primary developer's machine: `diag` produces clean
  distinguishable edges, including the `E0` extended-key folding (right Ctrl→99,
  right Alt→98) and the swallow-on-handle behavior.
- **Linux.** `diag` shows clean down/up for the hard keys at a physical console
  (observe-only mode, as designed for §12).

Across all three the crate therefore **compiles and self-tests** — `cargo build
&& cargo test` precede `diag` in `TESTING.md`, so a successful live run implies a
green build/test on that host.

What **remains** of the current stage is now **engineering work, not risk
discovery** — no test gate is outstanding:

- The **Linux selective-consume path** (`device.grab()` + `uinput`
  re-injection) is still unbuilt; Linux currently only *observes* edges. This is
  a build task, not an interception-correctness question.
- The two surfaced **sharp-edge decisions** and the **normalization-table
  extension** below.

So the current stage is no longer gated on any of its risks — the §12 question
is fully answered on all three OSes. What's left is **closing out Stage 0's
remaining engineering items** (below) before Stage 1 stands on the layer.

---

## Stage 0 (current) — Land the keyboard layer on real hardware

**Goal.** Convert the headless-verified prototype into a keyboard layer that is
demonstrably correct on Windows, macOS, and Linux — closing the project's single
load-bearing risk (design §12) before any downstream work depends on it.

**Why this is the gate.** Every later stage assumes the layer reliably turns
physical keystrokes into assignment strings. If a class of keys a user would
plausibly designate as a modifier (Caps Lock, function keys, Numpad 5) can't be
cleanly intercepted on a given OS — macOS most at risk — that constrains the
whole "invent your own shift key" premise and must be known now, not after the
interpreter and editor are built on top of it.

**Work items.**

- **Compile and self-test on each target.** ✅ **Done (2026-06-07).** `cargo
  build && cargo test` green on Windows, macOS, and Linux — no per-OS dependency
  or `cfg` issues survived contact with real toolchains.
- **Run the §12 `diag` experiment on real keyboards.** ✅ **Done — all three
  (2026-06-07).** Caps Lock, both Shifts/Ctrls/Alts, Numpad 5, and F1–F10 each
  produce a clean `DOWN`/`UP` line on **macOS** (Sequoia/ARM), **Windows**, and
  **Linux** (physical console). The §12 interception question is answered.
- **Resolve macOS `FlagsChanged`.** ✅ **Done (2026-06-07).** The tap
  reconstructs distinguishable down/up for the keys macOS reports as flag
  changes, Caps Lock included, on Sequoia/ARM. This was the highest-risk single
  item; it is retired.
- **Confirm non-exclusive capture holds on each OS.** ✅ **Verified on Windows
  and macOS (2026-06-07)** — a `KF`-assigned key reaches the OS while everything
  else is captured (`WH_KEYBOARD_LL` unhandled passthrough; `CGEventTap` passing
  the event). On **Linux** this is bounded by the consume path below: until
  `device.grab()` + `uinput` lands, Linux observes without swallowing, so
  per-key `KF` selectivity can't be fully exercised there yet.
- **Finish the Linux consume path.** Observing edges is enough for §12, but real
  selective consumption on Linux needs `device.grab()` plus re-injecting the
  `KF` keys through a `uinput` virtual device. This is the remaining Linux work
  flagged in `platform/linux.rs`.
- **Decide the two surfaced sharp-edges.** (1) Structural dual-role detection
  routes `NO`-cell modifiers (Shift/Ctrl/Caps in `CLD4R.KBD`) through the
  tap-vs-hold machine — accept it, or let the parser treat a reserved no-op token
  as "no tap." (2) A declared-but-unreferenced shift key silently vanishes and
  shifts bit assignment — add a load-time warning.
- **Extend the normalization tables toward the canonical chart.** They cover the
  §12 offenders and main key blocks today. Begin filling the stable canonical
  key-code chart past 104 (design §3.4) for all three platforms — this is the
  maintained contract later stages and KBD authors rely on.

**Exit criteria.**

- ✅ Clean, distinguishable down/up confirmed on **macOS, Windows, and Linux**
  for the historically-difficult keys (2026-06-07).
- `KF` demonstrably releases a chosen key to the OS while capture is otherwise
  total: ✅ confirmed on **Windows and macOS**; **Linux** pending the `uinput`
  consume path.
- ✅ `cargo test` green on all three targets (2026-06-07).
- The two sharp-edge decisions are made and reflected in the parser/loader.
  *(open)*

**Risk — substantially retired (2026-06-07).** macOS interception was the
make-or-break, and it **cleared** on Sequoia/ARM (clean edges for the hard key
classes, Caps Lock included); Windows and Linux passed their §12 runs as well.
No OS-specific modifier-eligibility constraint needs to be carved into the design
on any platform's account — the "invent your own shift key" premise holds across
all three. What remains is not risk discovery but engineering: the Linux
`uinput` selective-consume path, the two sharp-edge decisions, and extending the
normalization tables.

---

## Stage 1 — XPL interpreter core (the consumer of assignments)

**Goal.** Build, in Rust, the **XPL** (*XyScript Programming Language*)
interpreter — the engine that **executes** the assignment strings the keyboard
layer produces. Today those strings resolve correctly and then go
nowhere; this stage gives them an executor and closes the keyboard layer's
primary seam (design §10.1).

**Why this is next.** The keyboard layer is intentionally complete-but-inert: it
hands out opaque XPL strings and stops. Nothing those keys "do" exists until the
interpreter does. **QuickNotes** (QN, `C:\Users\cldis\QN2`) — ~23.8k lines of
Python with a manual (`qnmanual.md`) and a unit suite (`test_quicknotes.py`) —
is a mature, finished-feeling language that establishes the **feature floor**
this stage must meet: the verbs, the control constructs, the scoping and error
behavior a user can rely on. Per the foundational premise, QN tells us *what the
language does for a user*; it does **not** tell us how to build the interpreter.
XyScript's scripting language follows the same XyWrite-like, scripting-driven
spirit but aims to be a substantial **superset**, designed fresh in Rust.

**QN's feature surface (behavior reference — explicitly *not* a build template).**
Listed so the floor is legible. Each item is a *behavior* to meet or exceed;
none of QN's internal mechanisms below are prescriptions for XyScript, and several
are things XyScript should deliberately do differently from the start.

- **Behavior to meet: the control constructs.** `IF`/`IFNOT`/`ELSE`/`ELSEIF`,
  `WHILE`/`BREAK`/`CONTINUE`, `GOTO`+labels, `SUB`/`CALL`/`RETURN`. *(QN happens
  to implement these with a line-at-a-time program-counter machine and
  physical-line pre-scans — the source of footgun caveats like "don't chain a
  block `IF` with `;` on one line." That mechanism is QN trivia; XyScript resolves
  control flow structurally — see `stage1-interpreter-shape.md`.)*
- **Behavior to meet: variable substitution and the function set** —
  `VAR()`/`SEL()` and the "when to quote `$var`" surface. *(QN implements this as
  a textual `$`-pre-pass before tokenization; XyScript is free to choose a
  token-level/structured substitution model instead, and likely should.)*
- **Behavior to meet: value semantics.** Numeric-then-string comparison
  (`10 < 9` false, `"10" < "9"` true); case-insensitive verbs/vars/functions/
  labels/subs with case-preserving values. *(QN does this with a single
  stringly-typed slot; XyScript may introduce real types/maps from the outset —
  the floor is the observable comparison/coercion behavior, not the one-slot
  representation.)*
- **Behavior to meet: ~200 verbs + an expression sub-language**
  (`SET name = expr` and `IF`/`WHILE` conditions). Grammar
  `or→and→not→comp→add→mul→unary→atom`, `.` for concat, string functions (`LEN
  UPPER SUBSTR INDEX REPLACE CHR ASC SEL VAR …`). This is a feature checklist to
  cover, not a parser to copy.
- **Feature whose QN *implementation* is irrelevant here: text/selection/search.**
  In QN these delegate to the Tkinter `Text` widget (including a `/R` flag that is
  Tcl regex, not PCRE). XyScript owns its own text engine (Stage 2) and its own
  regex; QN's Tk coupling is **not** a reimplementation target — it's the seam
  where Stage 1 meets Stage 2, to be designed fresh.

**Work items.**

- **Interpreter shape — already settled by the foundational premise.** Fresh
  Rust-native pipeline: parse → structured AST → flat IR with resolved jump
  targets → execute. QN's line/PC machine is **not** a candidate (it's QN
  structure, which doesn't factor in). This is no longer an open decision; the
  reasoning and the corroborating measurement are in
  `stage1-interpreter-shape.md`.
- **Implement the scripting core (meet QN's floor, design fresh).** `SET`/`SV`/
  `LOCAL`/`CLEAR`, `IF`/`IFNOT`/`ELSE`/`ELSEIF`/`ENDIF`, `WHILE`/`BREAK`/
  `CONTINUE`, `GOTO`+labels, `SUB`/`CALL`/`RETURN`/`EXIT`/`EXITALL`, positional
  args (`$1..$N`, `$#`, `$*`), and the expression evaluator + string functions.
  `test_quicknotes.py` is a **behavior reference** for the leaf semantics it
  covers (expressions, substitution, splitter, scoping) — *not* a bug-for-bug
  conformance target, and notably it does **not** exercise block control flow at
  all. XyScript writes its own tests, especially for control flow.
- **Provide at least the two-tier error model (floor; XyScript may exceed it).**
  *Trappable* failures set `$ERROR=1` and keep running (auto-cleared at each
  action dispatch; `SETERR`/`CLERR` to manage; an exempt-verb set so
  `<action>; IF $ERROR …` works). *Fatal* failures (unknown command/label/sub,
  divide-by-zero, write to a read-only special) **abort the script** and surface
  visibly. This floor keeps a bad key-cell from crashing the input loop;
  XyScript's backlog includes going beyond it with *trappable fatals*.
- **Provide the variable & scoping model's feature floor.** Script-global by
  default; `LOCAL` save/clear/restore for subroutine and library scope; the
  read-only **special variables** (`$SE` selection, `$CB` clipboard, `$CP/$CX/$CY/
  $LN/$LC`, `$RC` shell exit, `$OS`, `$SC`, …). *(QN expresses arrays as a
  `$arr_0`/`$arr[i]` naming convention with `$name_0` = count — a stringly-typed
  workaround. XyScript should weigh a **real array/map type** from the start; the
  floor is array-like behavior, not the naming convention.)*
- **Build the library system.** `.qnu` files of `SUB`…`ENDSUB` routines with
  `# DOC:` strings; a bundled **system** library overlaid by a **user** library
  (`scripts/user.qnu`); `LOADLIB`/`ADDLIB`/`RELOADLIB`/`UNLOADLIB`/`LISTLIB`;
  last-wins merge; **shadow-rejection** (a routine may not collide with a built-in
  verb or special-var name). Plus `RUN <script.qns>` and `startup.qns`, which the
  loading model (§9.4) already expects.
- **Wire the keyboard-layer seam.** The resolver's `Dispatch(XPL)` outcome feeds
  the interpreter, which owns all validation of assignment content. Keep the
  reserved-directive split intact — `KF` is handled in the layer's event callback
  and never reaches the interpreter.
- **Implement `LOADKBD` as a XPL command** invoking the loader's transactional
  swap (design §9.2/§10.2) — the first command that closes a loop: a key runs XPL
  that reloads the keymap. (Note QN's *active* binding model is Tk-wide
  `LOADKEY`/`.qnk`; XyScript **retires** it in favor of `.KBD`/`LOADKBD`, so the
  binding verbs are intentionally *not* ported.)
- **Expose key context to scripts.** Preserve a `$SC` equivalent — the canonical
  key code and/or current bitmask queryable from XPL (design §10.3) — and decide
  whether `READKEY`/`$SC` stays as in QN.
- **Author KBD content in real XPL.** `CLD4R.KBD`'s right-hand sides are XyWrite
  verbs and won't run; grow `kbd/xyscript-sample.kbd` into a working XPL layout
  that exercises the interpreter through actual keystrokes.

**Where XyScript exceeds QN — and where the fresh slate lets it start ahead.**
Because XyScript designs the interpreter from scratch in Rust, several of these
aren't merely deferred backlog — they're choices to make *at the foundation*,
since retrofitting them later is the more expensive path: a **real array/map
type** (vs. the `$arr_0` naming convention), a **token-level / structured
substitution model** with `${}`/nesting (vs. a textual pre-pass), **real types**
under the value model, an **XyScript-owned PCRE-class regex** (vs. borrowed
Tcl/Tk), **trappable fatals**, **user-defined expression functions**, and
`$0`-style script identity. Decide deliberately which land in Stage 1's
foundation vs. a later stage — but make these decisions *as a superset designer*,
never as a porter constrained by QN's shape. The discipline to hold is **scope**
(don't let Stage 1 balloon), not **fidelity to QN's internals**.

**Exit criteria.**

- A keyed assignment runs end-to-end: physical key → canonical event → resolve →
  dispatch → **interpreter executes XPL** → observable effect (a logged action or
  a variable change is enough at this stage).
- The scripting core passes XyScript's **own** headless language test suite
  (control flow, scoping, `$ERROR`, expressions, library routines), mirroring the
  keyboard layer's testing discipline. `test_quicknotes.py`'s leaf cases inform
  the *behavior* to match where relevant, but XyScript owns its tests — and must
  add the block-control-flow coverage QN's suite never had.
- `LOADKBD <path>` works as a XPL command and respects the transactional /
  last-known-good rules.
- The foundation-level superset decisions (real types/maps, substitution model,
  owned regex, trappable fatals — see above) are made and written down, with
  rationale, as deliberate Rust-native choices.

**Risk.** Two real ones. (1) **Text-engine-dependent verbs don't belong here** —
cursor, selection, and search need a text engine that doesn't exist until Stage 2,
so Stage 1 must scope to the *language* (control flow, variables, expressions,
libraries, error model, shell/`$`-vars) and stub or defer the editing verbs.
(2) **Scope creep** — the language surface is large and XyScript's superset
ambition makes "one more feature" tempting; hold this stage to a coherent
language core (QN's floor plus the foundational upgrades chosen deliberately
above), and let the editor (Stage 2+) motivate the rest. The discipline is
scope, not fidelity to QN's internals.

---

## Stage 2 — Minimal text engine + the closed editing loop

**Goal.** The first stage where XyScript is an **editor**: a key produces a
character (or a command runs) and you *see* the result. A vertical slice — narrow
but complete — through input → interpreter → text → display.

**Why this is next.** Stages 0 and 1 produce a correct but invisible pipeline.
The cheapest way to validate that the whole foundation is sound is to close the
loop with the *smallest* text buffer and display that can render keystrokes and
run commands. This also turns the project into something you can dogfood, which
is where real requirements surface.

**Implementation status (2026-06-07) — core complete, headless-verified.** The
`xyscript-app` crate now closes the loop: a minimal text engine (`text.rs`:
insert/delete/motion/selection over a `Vec<char>`, with line/column/specials), a
real `Host` impl backed by it (`host.rs`), and an `Editor` (`editor.rs`) that
runs **key → resolve → dispatch XPL → mutate buffer → render**. Editor verbs
(`INSERT`, `BS`/`DEL`/`DELSEL`, `LEFT`/`RIGHT`/`UP`/`DOWN`/`HOME`/`END`/`TOP`/
`BOTTOM` with a `SEL` extend), the CMline (`CMLINE`/`SUBMIT`/`CANCEL` + a direct
`run_command`), and `LOADKBD` (transactional swap, reconciled after the script)
are wired. **25 app tests + the workspace's 150 others = 175 green.** The live
shell (`main.rs`) installs the global hook, normalizes native→canonical, feeds
the editor, and renders to the terminal. **All three exit criteria are now met:**
1–2 tested headlessly, and **criterion 3 — live on a platform — is confirmed on
Windows (2026-06-07):** `cargo run -p xyscript-app` (from the workspace root)
brings up the editor; typing (letters/digits/space/Enter/Backspace, Shift/Caps
for uppercase), command cells (Ctrl+S → a library routine), and `Ctrl+Q` to quit
all work as designed. The demo binary compiles in the sample layout + system
library (works from any directory), shows a status line, and renders the cursor
as `▏`. **Stage 2 is complete.**

**Work items.**

- **Minimal text engine.** An insert/delete buffer with a cursor — enough for
  ordinary typing and basic motion. No styling, no multi-file, no undo tree yet;
  just a correct, observable model the interpreter can mutate.
- **A bare display.** Render the buffer and cursor. Toolkit choice can stay
  provisional — the design keeps input toolkit-independent (only the per-OS
  normalization table touches the platform), so the display can be the simplest
  thing that shows text and be swapped later.
- **Connect XPL to the buffer.** Editing verbs (insert character, move cursor,
  delete) become interpreter operations on the text engine, so ordinary
  letter-cells in the KBD actually type and command-cells actually edit.
- **The CMline.** Bring up the command line as QN has it, executed by the same
  interpreter — the user's manual path to commands and the fallback when a key
  isn't bound.
- **Startup integration.** Load and run a `startup.qns` that may call `LOADKBD`,
  wiring the documented startup → load → operative-keymap flow (design §9.4). If
  it loads no KBD, the embedded default stands.
- **Self-sufficiency check.** Assert the embedded default can reach `LOADKBD`
  (binding and/or CMline) and `KF`s a baseline of OS keys, so a failed custom
  load never strands you (design §9.1).

**Exit criteria.**

- You can launch XyScript, type with a XPL-authored KBD, see characters appear,
  move and delete, and run a command from the CMline.
- `startup.qns` loads a custom layout at launch; a deliberately broken KBD falls
  back visibly to the last-known-good / default without locking the keyboard.
- The slice runs on at least one platform end-to-end (whichever Stage 0 validated
  most cleanly — likely Windows or Linux first, with macOS following its test
  results).

**Risk.** Premature breadth. The temptation is to start building "the editor"
(files, styling, undo, real GUI). Keep Stage 2 to the thinnest loop that proves
the foundation; defer everything else to the post-Stage-2 arc below.

---

## Dependency chain

```
Stage 0  Keyboard layer on real hardware  (foundation; proves §12 risk)
   │   produces assignment strings reliably
   ▼
Stage 1  XPL interpreter core             (executes those strings; §10.1 seam)
   │   gives keystrokes their effects + LOADKBD
   ▼
Stage 2  Text engine + closed loop        (first visible editing; CMline, startup)
   │
   ▼
(beyond) full editor
```

Each stage strictly depends on the one above: assignments are meaningless without
an interpreter, and an interpreter has nothing to act on without a text engine.

---

## Beyond Stage 2 (sketch only)

> **Superseded (2026-06-07).** Now that Stage 2 is complete and live-verified,
> this sketch has been turned into a proper staged plan in
> **`beyond-stage2-plan.md`** (Stage 3 — the usable file editor — in depth, with
> Stages 4–5 sketched and the load-bearing decisions called out). The paragraph
> below is retained as the original sketch it grew from.

Out of scope for this near-term plan, noted so the next two stages aren't
designed into a corner: a real text engine (styling, large files, undo), the
production GUI and rendering, file I/O and document model, the full ~200-verb
editing/file/display command set, the QN-beyond language upgrades flagged in
Stage 1 (real arrays/maps, user-defined functions, trappable fatals, an
XyScript-owned regex, token-level substitution), completing the canonical
key-code chart and normalization tables across all platforms, and hardening the
injection channel's access control for any real external-automation use. These
become a proper plan once Stage 2 has surfaced concrete requirements.
