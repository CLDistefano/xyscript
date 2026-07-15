---
title: beyond-stage2-plan
tags: [roadmap, stage-3, stage-4, stage-5, text-engine, document-model, undo]
created: 2026-06-07
status: STAGE 3 COMPLETE (2026-06-07) — the usable file editor. All four decisions LOCKED + D1/D2 validated by the ropey spike (300MB); all 7 build steps done, 211 tests green (headless), live on-disk verification pending a developer run. Stages 4–5 remain sketches.
companion_to: development-plan.md (Foundational premise + Stages 0–2), keyboard-layer-design.md, stage1-interpreter-plan.md
supersedes: development-plan.md §"Beyond Stage 2 (sketch only)"
---

# XyScript — Staged Development Plan, Stages 3–5: the usable editor

*A personal roadmap picking up exactly where `development-plan.md` stopped. That
document deliberately ended at Stage 2 — "the first point at which you can type
into XyScript and see text" — and said the rest "become a proper plan once Stage
2 has surfaced concrete requirements." Stage 2 is now complete and live-verified
on Windows, so this is that proper plan. Same format as the original: the next
stage (**Stage 3**) in full depth, the two after it (**4, 5**) sketched, and a
dependency chain.*

This roadmap was scoped with three direction decisions (2026-06-07): Stage 3's
goal is a **usable file-based editor**; the display **stays a provisional TUI**
(the real GUI is deferred); and this document is a **staged roadmap**, not a
single-stage plan.

---

## Foundational premise — still governing

The three load-bearing principles from `development-plan.md` are unchanged and
override anything below that drifts from them. In one line each:

1. **QN is a feature *floor*, not a blueprint** (`C:\Users\cldis\QN2`).
2. **QN's Python/Tkinter *structure* never constrains XyScript** — fresh Rust
   slate.
3. **The keyboard-capture model is the sine qua non.**

**Consequence for Stage 3 specifically.** QN delegates its text, selection, and
file handling to the Tkinter `Text` widget and a notes/tab store. That is *Tk
trivia*, never a design target. XyScript owns its **own** text engine, document
model, and undo from scratch — chosen for what Rust makes possible (ownership,
real data structures, no GC), with QN consulted only for the *behavior floor*
(what open/save/undo/multi-file do for a user, and the file/cursor special
variables scripts can read). Where this plan names a QN verb or special, it is
fixing the contract, not proposing an implementation.

This stage also inherits the project's working **discipline**, which is itself
load-bearing: tight single-purpose stages; a **headless-testable core** (no GUI,
no OS in the tests — a virtual filesystem stands in for disk); explicit exit
criteria; and load-bearing decisions resolved in their own short decision docs
*before* implementation (the model `stage1-interpreter-shape.md` set for the AST
choice). Holding this discipline is what kept Stages 0–2 correct; Stage 3 is
larger and needs it more, not less.

---

## Where things stand, and what Stage 2 surfaced

**Done and verified (2026-06-07):** Stages 0–2. The vertical slice runs
end-to-end on Windows — physical key → kbd resolver → XPL interpreter → text
engine → terminal display — with a custom KBD, library routines, transactional
`LOADKBD`, a CMline, and a clean quit. 175 headless tests across the workspace.

**What the thin slice deliberately is *not* yet — i.e. the concrete requirements
Stage 2 was built to surface.** These are the gaps that make it a demo rather
than an editor, and they define Stage 3:

- **No persistence.** There is no way to open a file or save one. You can type
  into a buffer and lose it. `read_file` exists on the `Host` (for `LOADKBD`/
  `RUN`); there is no `write_file` and no document-level open/save.
- **No document identity.** A single in-memory `Buffer` with no path, no
  modified flag, no encoding/line-ending awareness. The file/path special
  variables the floor promises (`$FI`, `$FP`, `$CWD`, `$MO`, …) resolve to
  nothing.
- **One buffer.** No multi-file / multi-buffer model, so no switching between
  open documents (QN's tabs are the floor here).
- **No undo/redo.** A mistake is permanent.
- **A toy text data structure.** The buffer is a `Vec<char>`: correct and
  perfectly fine for a slice, but every insert/delete is O(n) and the whole
  buffer is re-rendered each keystroke. It will not scale to a real file, and it
  offers no natural undo.
- **A whole-buffer display.** The TUI clears and reprints everything on every
  event; there is no scrolling viewport, no resize handling, and VT mode isn't
  explicitly enabled on Windows (older consoles may show escape codes).

None of these were oversights — Stage 2's risk was *premature breadth*, and it
correctly stayed thin. Stage 3 is where they get built, in the order that makes
the editor genuinely usable for dogfooding.

---

## Stage 3 — the usable file editor

**Goal.** XyScript becomes an editor you can actually *use on real files*: open a
file from disk, edit it with a correct and reasonably-scalable text model, undo
and redo, switch between several open files, and save. This is the first build
you can dogfood in earnest — which, per Stage 2's rationale, is where real
requirements surface.

**Why this is next (the dependency argument).** Everything past here — the broad
editing/search/file verb set (Stage 4), language depth and the real GUI (Stage
5) — wants a real document underneath it. You cannot meaningfully grow the verb
surface, or motivate language upgrades, or judge a GUI, while the thing under
test can't load or keep a file. File I/O + a real text engine + undo is the
smallest addition that turns the slice into a tool, so it is the cheapest way to
unlock the rest. Display stays provisional on purpose (the design keeps input
toolkit-independent, so the renderer swaps later without touching the core) — the
value of this stage is *capability*, not polish.

**Work items.**

- **A real text engine (data structure).** Replace `Vec<char>` with a **rope
  (`ropey`)** — D1 below, decided 2026-06-07, driven by the 100–300MB-file
  requirement. Wrap it behind the existing `Buffer` API. It must:
  - preserve the existing `Buffer` API (insert / delete / motion / selection /
    line+column / selection-text) so the proven Stage 2 editor loop is
    *unchanged* — this is a swap behind a stable interface, not a rewrite of the
    loop;
  - use `ropey`'s built-in **line/char/byte indexing** so `$LN`/`$CY`, vertical
    motion, and viewport rendering are O(log n), not O(n) scans of the buffer
    (this is a big reason ropey was chosen — we don't hand-roll a line index);
  - stay fully headless-testable.

- **Document model + file I/O.** Introduce a `Document` = buffer + file path +
  modified ("dirty") flag + encoding + line-ending style. Add the file verbs the
  floor needs — **new, open (read), save (write), save-as** — routed through the
  interpreter to the `Host`. Extend `Host` with `write_file` (companion to the
  existing `read_file`), kept headless via the test virtual FS. Wire the
  **file/path special variables** to live document state: at least `$FI`
  (filename), `$FP` (full path), `$CWD`, `$MO` (modified), and whichever of QN's
  `$PA`/`$ED`/`$SF`/`$VE` the floor genuinely needs — implement the names that
  carry behavior, stub the rest as before.

- **Multiple documents.** A set of open documents with a notion of the **active**
  one, and verbs to switch (QN's tab model is the behavior floor; the minimum is
  a list + current index + next/prev/select-by-name, not a full tab UI). The
  active document is what the editor verbs and specials act on.

- **Undo / redo.** An edit-history model with `UNDO`/`REDO` verbs. Decide
  granularity (per-keystroke vs. coalesced typing runs) and representation
  (depends on the text-engine choice — see "Decisions"). Cursor/selection
  position must be restored along with text. This is a classic correctness
  hazard; it gets first-class headless tests.

- **Encoding & line endings (minimal, correct).** UTF-8 by default; detect and
  **preserve** the file's existing line-ending style (LF vs. CRLF) on save so
  round-tripping a file doesn't rewrite every line. Do not over-build a
  conversion subsystem — just don't corrupt files.

- **Wire it through the interpreter + Host, loop unchanged.** New verbs become
  `EditorOp`s / `Host` calls exactly as the Stage 2 editor verbs did; the
  key→resolve→dispatch→edit→render loop is not touched. Add the file/undo/buffer
  verbs to the shadow-rejection list.

- **A real-enough TUI (still provisional).** Upgrade the display from
  "reprint the whole buffer" to a **scrolling viewport** that renders only the
  visible window of a (possibly large) file, with a status line showing
  file name / modified flag / cursor position, terminal **resize** handling, and
  **explicit VT-mode enable on Windows** (fixing the escape-code-garbage risk
  noted in Stage 2). This stays "the simplest thing that shows a file" — not a
  GUI, no styling, no line numbers unless trivial.

- **Resolve the deferred typing-model question.** Stages 1–2 deferred the terse
  self-inserting-letter KBD convention (today letter cells are explicit `insert
  "x"`). With real editing now the focus, either adopt a self-insert convention
  or consciously keep the explicit form, and record the choice (see "Decisions").

**Exit criteria.**

- Open a real file from disk, see it, edit it (type / move / delete / select),
  `UNDO` and `REDO` correctly, `SAVE` it, then reopen it and see the changes —
  all on the live Windows TUI.
- A large-ish file (e.g. tens of thousands of lines) opens and edits without
  perceptible lag, demonstrating the data-structure and viewport wins.
- Several files open at once; switching between them works; `$MO` and the file
  specials reflect each document's real state; quitting with an unsaved document
  is handled deliberately (at minimum, not silently lost — a prompt or a clear
  status).
- The headless suite covers the text engine, undo/redo, the document model, and
  file I/O (against the virtual FS), in the project's established style.
- The three Stage-3 decisions (text engine, undo model, document model) and the
  typing-model choice are **written down with rationale** before the code they
  govern lands.

**Risk.**

- **Scope creep — "usable editor" → "the editor."** The single biggest risk.
  Hold the line at: correct edits, files, undo, multi-buffer, scrolling viewport,
  encoding-safe round-trip. **Defer** styling, syntax highlighting, search/
  replace, line numbers, the broad verb set, and the real GUI — those are Stages
  4–5 and must be *motivated by dogfooding*, not front-run.
- **Data-structure over-engineering.** Don't build a sophisticated rope if a
  simpler structure meets the large-file + undo bar. Pick the simplest thing that
  clears the exit criteria; the decision doc must justify complexity, not assume
  it.
- **Undo correctness.** Undo/redo is where editors hide their worst bugs. Treat
  it as a first-class tested component (position restoration, coalescing
  boundaries, redo invalidation on new edits).
- **Display creep.** Staying terminal is a decision; honor it. A scrolling
  viewport + status line is the ceiling for Stage 3's display.
- **File-safety.** Saving must never truncate/corrupt on error (write to temp +
  rename, or equivalent) — the project's "look before you overwrite" instinct
  applies to the editor's own save path.

---

## Stage 3 — load-bearing decisions to resolve first

*Each gets a short decision doc before its module is built, the way the AST
choice did. Recommendations below are the lean, not the commitment.*

### D1 — Text-engine data structure — **DECIDED (2026-06-07): rope via `ropey`**

**Decision.** Use a **rope**, specifically the vetted **`ropey`** crate, wrapped
behind the existing `Buffer` API. **Driving requirement:** the engine must edit
**100–300MB files** without visibly compromising normal-sized files.

*Why rope/`ropey` over the alternatives (analysis 2026-06-07):*

- **Gap buffer — rejected.** Fine for normal files, but a far cursor jump forces
  a gap move = a memmove of up to the whole file; on 300MB a single post-jump
  keystroke stalls visibly. It also keeps the whole file resident. Wrong tool the
  moment "edit anywhere in a huge file" is required.
- **Piece table + balanced tree + mmap — the theoretical optimum, not chosen.**
  It alone gives *instant, lazy* open (mmap pages in only what you touch) and
  edit cost independent of file size. But it is by far the most code to get
  correct (tree rebalancing, subtree byte/char/line counts, CRLF spanning piece
  boundaries, mmap + atomic-save lifetime). Its headline win — lazy open/memory
  — is **consciously forgone**.
- **Rope (`ropey`) — chosen.** O(log n) insert/delete *anywhere* (flat edit
  latency at any size, no memmoves) and newline-augmented nodes give O(log n)
  line/column navigation — the two behaviors that matter for 300MB — at a
  fraction of the piece-tree's implementation risk, as a mature dependency.
  **Accepted trade-offs:** it reads and holds the whole file (~file-size RAM,
  fine on a modern machine) and pays a one-time O(n) load (~1–2s for 300MB)
  rather than opening instantly/lazily. Normal files are unaffected (the rope's
  per-op overhead is an invisible constant).

*Premise check (recorded so it isn't re-litigated).* Taking a **crate** for the
rope does not violate "fresh Rust slate." Hand-rolling is reserved for
XyScript's **identity** — the language/interpreter and the keyboard-capture
model, which are the project — not for generic infrastructure. A UTF-8 rope is
generic infrastructure; using `ropey` is in-bounds and the right call.

*Implementation notes.* `ropey` exposes char/byte/line indices, line iteration,
and slicing, which map cleanly onto the `Buffer` API (cursor as a char index,
`line()`/`column()`, selection, viewport slicing). Add `ropey = "1"` to
`xyscript-app`. A small **validation spike** preceded real work
(`examples/ropey_spike.rs`).

**Spike result — VALIDATED (2026-06-07, release build, 300MB / 265.8M chars /
4.88M lines on the dev's Windows machine):**

| Operation | Result |
|---|---|
| open (`from_reader`, streamed) | **0.34s** |
| scattered single-char insert | **~1.0µs/op** (flat) |
| scattered single-char delete | **~0.51µs/op** (flat) |
| char↔line lookup | **~0.68µs/op** (O(log n)) |
| save (`write_to`) | **0.19s** |
| D2 inverse-op undo round-trips (ASCII / multi-byte / CRLF) | **all ✓** |

Edit and lookup latencies are flat at microsecond scale even at 300MB — the
property a gap buffer would fail — and open/save are sub-second (well under the
"few seconds" estimate). Memory is ~file-size resident, as expected. **D1 and D2
are confirmed on real hardware; no surprises.**

### D2 — Undo model *(depends on D1 = rope; still open)*

### D2 — Undo model — **DECIDED (2026-06-07): inverse-op edit log + coalescing**

**Decision.** An **inverse-op edit log** at the document level. Each edit records
what is needed to both reverse and replay it — the position, the text removed,
and the text inserted — plus the **cursor/selection before and after**. `UNDO`
applies the inverse and restores the pre-edit cursor; `REDO` re-applies the
forward op and restores the post-edit cursor; a fresh edit clears the redo stack.

**Coalescing.** Consecutive single-character **inserts** at adjacent positions
merge into one undo unit, and consecutive **deletes** likewise; a **cursor jump**
(a non-contiguous edit) or a change of op kind closes the current unit and opens
a new one. So a typed run undoes as a chunk, not character-by-character.

**Why this over COW snapshots (the prior lean):** memory is **proportional to
edited text, not file size** — the right property for 100–300MB files, where
accumulating diverged rope snapshots could grow unboundedly. Edits already flow
through document-level methods, so recording the inverse there is natural. A
small **validation point** folds into the ropey spike: confirm round-trip
correctness of inverse ops against the rope (incl. multi-byte/CRLF boundaries).

### D3 — Document/buffer model — **DECIDED (2026-06-07): multi-document from the start**

**Decision.** The host holds a set of open **documents** (`Vec<Document>` + an
active index), not a single buffer; the **active document** backs the editor
verbs and the file/cursor special variables. A few switch verbs (next / prev /
select-by-name) change which is active. The **CMline stays a separate transient
buffer**, not a document. Kept minimal — no tab-bar UI, just the list + switch
verbs + the status line showing the active file. Rationale: multi-buffer is cheap
to design in now and expensive to retrofit (the same "decide at the foundation"
logic that put real arrays/maps in Stage 1); QN's tabs are the behavior floor.

### D4 — Typing model — **DECIDED (2026-06-07): KBD literal-sugar that *desugars* to XPL**

This decision is governed by XyScript's headline tenet — **"one syntax, one
morphology: it's all XPL"** (the deliberate fix for XyWrite's three distinct
syntaxes). The CMline, KBD-cell RHS, `.qns` scripts, and `.qnu` routines are all
the same XPL. So the needed KBD terseness is delivered as **sugar that desugars
to XPL**, never a separate semantics.

**Rule — what is literal text.** A KBD cell (trimmed) is literal text if it is
either:
- **a single bare character** — any UTF-8 scalar except `"` (and not whitespace;
  use the quoted form for a space), or
- **a single quoted-string literal** `"..."` (internal `"` escaped as `\"`).

Anything else is ordinary XPL (a command).

**Mechanism — desugar, don't intercept.** A literal cell is rewritten to an
`insert` command: a bare char `c` → `insert "c"`; a quoted cell `"..."` →
`insert "..."`. This runs at **keymap-install time in the app layer**
(`xyscript-app`), so at runtime *every* cell is real XPL executed by the one
interpreter with the one error model — the tenet is preserved exactly (one
execution path; the sugar is pure surface syntax). Chosen over an editor-seam
special-case precisely because interception would make a literal cell *not*
execute as XPL — a fork from "it's all XPL."

**Consequences / why this is clean.**
- The keyboard crate stays opaque (it never parses XPL — design 3.3) and its
  **Stage-0 tests are untouched**, because desugaring is *downstream* of it, in
  the app. The embedded default's bare `q` cells type without changing the kbd
  crate at all.
- The interpreter's strict "unknown verb → fatal" typo-detection is untouched
  (the sugar is upstream of it).
- Edge: the only single-char built-in verb is `?` (help) — a bare `?` cell types
  `?`; use `HELP` to invoke help. To type a literal `"`, use the quoted form
  `"\""`.

---

## Stage 4 — sketch: editing breadth (selection, search, the verb set)

Once a real document exists, grow toward QN's editing surface — the part the
floor is largest in and that dogfooding Stage 3 will prioritize concretely:

- A **richer selection model** and the selection/clipboard verbs (copy/cut/
  paste become real, backed by the document).
- **Search & replace**, backed by the **XyScript-owned, PCRE-class regex** (a
  flagged QN-beyond item; QN borrowed Tcl/Tk regex, which XyScript does not). The
  owned regex may warrant its own decision/sub-stage.
- Filling in the **file/display/editing verbs** toward the ~200-verb floor —
  driven by what real editing in Stage 3 shows is actually needed, not by
  enumerating QN.

Depends on Stage 3's document model; the owned regex is its own load-bearing
piece.

## Stage 5 — sketch: language depth and the real GUI fork

Two threads, sequenced by what dogfooding demands:

- **Language depth** — the remaining QN-beyond interpreter upgrades:
  user-defined expression functions and **trappable fatals** (`TRY`/catch; the
  error enum was deliberately shaped in Stage 1 to make this additive). These
  land when editing/scripting motivates them.
- **The real GUI** — retire the provisional TUI for a windowed renderer (the
  display decision deferred here): pick a Rust GUI/text-rendering approach, with
  scrolling/fonts/focus-aware input. This is itself large and may split into its
  own stage; the design's toolkit-independence is what lets it wait this long.
- **Foundation cleanups carried from Stage 0** — completing the canonical
  key-code chart and per-platform normalization tables, and hardening the
  injection channel's access control for any real external-automation use.

---

## Dependency chain

```
Stage 2  Thin editing loop                 (DONE — type & see text; CMline; LOADKBD)
   │   needs a real document to grow on
   ▼
Stage 3  Usable file editor                (text engine + files + undo + multi-buffer + viewport)
   │   gives every later feature a real document to act on
   ▼
Stage 4  Editing breadth                   (selection, search + owned regex, the verb set)
   │
   ▼
Stage 5  Language depth + real GUI         (user fns, trappable fatals; windowed renderer; Stage-0 cleanups)
```

Each stage strictly depends on the one above: breadth and depth are meaningless
without a real document; the GUI is judged against a working editor, not a demo.

---

## Decision log & recommended next actions

**Locked (2026-06-07):**

- **D1 — text engine:** rope via the **`ropey`** crate (driver: edit 100–300MB
  files without compromising normal ones).
- **D2 — undo:** **inverse-op edit log** + coalesce consecutive inserts/deletes
  between cursor jumps (memory scales with edited text, not file size).

- **D3 — document model:** **multi-document from the start** (`Vec<Document>` +
  active index; CMline a separate transient buffer).
- **D4 — typing model:** KBD **literal-sugar that desugars to XPL** — a cell
  that is a single bare char or a quoted-string literal is rewritten to
  `insert "..."` at keymap-install time (app layer), honoring "it's all XPL."

**All four Stage-3 decisions are now locked, and D1/D2 are validated by the spike
(2026-06-07, release/300MB — see the D1 result table).**

1. ✅ **`ropey` validation spike** — done; D1 latency/load/save and D2 undo
   round-trips all confirmed on the dev's hardware.
3. ✅ **Implemented in dependency order (2026-06-07):** rope-backed text engine
   (`text.rs`, behind the existing `Buffer` API) → inverse-op undo/redo
   (`document.rs`) → document model + file I/O + `write_file` (atomic) →
   multi-document + switch verbs (`host.rs`) → file specials → D4 desugar
   (`kbd_sugar.rs`) → scrolling-viewport TUI (`viewport.rs` + `main.rs` via
   `crossterm`). Each step landed with headless tests green, exactly as Stages
   1–2 were built. **Stage 3 done; Stage 4 is next (sketched below).**
4. **Record any further decision** (and any intentional QN-behavior divergence)
   here or in a decision doc, per the project's standing rule.
