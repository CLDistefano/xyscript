---
title: split-panes-build-plan
tags: [design, build-plan, gui, tabs, splits, panes]
created: 2026-07-08
---
# Split panes — build plan

Adds horizontal/vertical **split panes** to the GUI so two (or more) tabs are visible at once in the
main window. The verb suite (`NEW/H`, `NEW/V`, `THS`, `TVS`, `TW`, `TUS`, `TRV`, `TZ`) is the easy
10%; the real work is generalizing the single-canvas renderer into an N-pane layout. This plan
front-loads a **no-op prep refactor** (Phase 0) that de-risks that before any tree or verb exists.

All line numbers reference `xyscript-gui/src/main.rs` at the time of writing — re-grep to confirm,
they drift. Core state lives in `AppHost` (`xyscript-app/src/host.rs`); the VM/verbs in
`xyscript-xpl/src/{vm.rs,host.rs,library.rs}`.

## Phases at a glance

> **Status (2026-07-10):** Phase 0a DONE. **Phase 1 DONE** (`xyscript-app/src/panes.rs` — tree +
> ops + 11 headless tests; `Document.id: DocId`; `PaneTree` hung on `AppHost`, synced at
> switch/close/open). **Phase 3 DONE** (verbs `NEW/H` `NEW/V` `THS` `TVS` `TW` `TUS` `TRV` `TZ`,
> `AB`-collapses-pane, `$PANES`/`$PANE`, reference `#cmd`/`#sv`, editor tests; the no-omission test
> passes). **Phase 2 DONE (pending eyeball)** — composite-raster renderer: each pane rastered into
> its sub-rect of one canvas via swap-state (point `active`/`pane_view` at each pane, render, restore,
> focused pane last), dividers (separator colour) + focus border (selection colour) drawn into the
> buffer, **drag-resizable** dividers (`pane_dividers`/`set_pane_ratio` + egui interact regions),
> click-in-pane focuses it. v1 simplifications: gutter + scrollbar are single-pane only; lint/find
> highlights use the focused doc; clicking a non-focused pane focuses it (a second click places the
> caret). `dist/xyscript.exe` rebuilt `--features lint`. Follow-ups: dedicated `PANEBORDERCOLOR`
> knob; per-pane gutter/scrollbar; precise first-click caret in a non-focused pane.

The ordering is deliberately **risk-front-loaded, visibility-back-loaded**: the invisible, riskiest
decoupling comes first; the user-facing surface comes last and cheap. (Realized ordering differs:
Phase 3 verbs were built before Phase 2 rendering so the tree ops could be exercised headlessly and
the user can drive real splits the moment the renderer lands.)

| Phase | Effort | User-visible | Ships alone | Risk |
|-------|--------|--------------|-------------|------|
| 0 · Prep refactor | Med-high | No | **Yes** | High (front-loaded) |
| 1 · Layout model | Low-med | No | No (dead code alone) | Low (headless-tested) |
| 2 · Render + input | **High** | Yes (debug hook) | No | Medium |
| 3 · Verbs + docs | Low | Yes (full) | No | Low |

- **Phase 0 — make the current renderer pane-shaped, invisibly.** Route the single canvas through a
  per-pane `PaneView` + explicit `(view, doc)`, with zero behavior change. *Done when* the
  definition-of-done grep is clean and the app behaves identically. ~80% of the total risk, front-
  loaded into a change that can't regress (there's only one pane). Independently shippable as a
  cleanup even if splits never follow.
- **Phase 1 — build the layout model in isolation.** The `DocId`/`PaneId` ids, the tree, and its
  operations (`split`/`collapse`/`focus`/`swap`/`zoom`/`layout_rects`) + the `active`-mirrors-focus
  invariant. Nothing renders yet. *Done when* the headless tree tests pass (split→collapse
  round-trips, focus survives collapse, guard holds).
- **Phase 2 — make panes real on screen.** The frame loop rasters each pane into its rect; dividers;
  click/wheel/scroll routing to the right pane. Splits become visible — triggered by a temporary
  debug hook, since verbs don't exist yet. *Done when* a forced 2-pane layout renders, scrolls, and
  focuses independently. The highest-effort phase, but it's "wire two solved things together," not a
  render rewrite (Phase 0 decoupled; Phase 1 proved the tree).
- **Phase 3 — hand it to the user.** The verbs (`NEW/H|V`, `THS`, `TVS`, `TW`, `TUS`, `TRV`, `TZ`),
  `$PANES`/`$PANE` readbacks, and reference docs. Each verb is a few lines over a Phase-1 tree op.
  *Done when* the editor tests + reference no-omission test pass and `dist` is rebuilt.

Two practical notes: **Phase 0 is the natural stopping point** to bank progress without committing to
the whole feature. **Phases 1–3 are a unit** — a tree with no renderer, or a renderer with no verbs,
is dead code — so starting Phase 1 means committing to reach Phase 3 for anything user-facing.

## Locked decisions (2026-07-08)

- **v1 scope = distinct tabs in a layout tree.** A pane shows one *existing tab*; the constraint is
  **≤1 pane per *buffer* (`DocId`)** — not per file. Two panes may show two *different* buffers,
  **including two `OPEN/NV` copies of the same file**, which are distinct `Document`s (each
  `open_doc` does `Document::new()`) with independent carets — so splitting them is fine. What's
  forbidden is the *same* `DocId` in two panes. The tab bar still lists all docs; the tree
  references a subset. Activating a tab (`BUFGO`, tab click) sets the **focused pane's** doc — it
  swaps content into the current pane, it does NOT open a third pane.
- **NOT v1: two views of the *same* buffer.** A true split-view (one `DocId` shown twice — type at
  the top, see it update at the bottom, one save target) needs separating *buffer* (text/undo) from
  *view* (caret/scroll), which `Document` fuses today. That's the case the `≤1 pane per DocId` guard
  blocks: one caret can't drive two `follow`s. Deferred to v2; the `PaneView` struct is shaped so
  caret/selection can move into it later. (Note: `OPEN/NV` + split gives *a* same-file-in-two-panes
  view in v1, but the two are independent copies — edits don't sync and both save to the same path,
  last-write-wins. The *synced* experience is the v2 shared-buffer split-view.)
- **Additive, not a rewrite.** `docs`/`active`/every editing verb stay as-is. The layout tree drives
  *rendering* and *which doc is focused*. **Invariant: `self.active` always mirrors the focused
  pane's doc index**, so all ~146 `self.active` sites in host.rs keep working unchanged.
- **Reference docs by a stable id, not index.** `Document` gains `id: DocId` (from an AppHost
  counter). The tree references `DocId`; indices shift on tab close and must not leak into the tree.
- **Terminology (name by arrangement in code, map at the verb layer):** `Split::Stacked` = top/bottom
  (`NEW/H`, `THS`); `Split::SideBySide` = left/right (`NEW/V`, `TVS`). Avoid "H/V" inside the code.
- **`TZ` (zoom), not `TFS`.** "Full screen" collides with OS fullscreen. `TZ` toggles maximizing the
  active pane (tree untouched → toggle restores). `THS`/`TVS` default partner = the **alternate
  buffer** (`alt_active`, the BUFALT target), else a **new empty buffer** — NOT raw `BUFPREV`.
- **Two verbs the original suite lacked are required:** `TW` (move focus to next pane) and `TUS`
  (un-split / collapse to the focused pane). Without them splits are unusable from the keyboard.

---

## Phase 0 — Prep refactor (NO-OP, single pane through `PaneView`)

Goal: the current single canvas renders through per-pane state, with zero user-visible change. After
this phase the tree + verbs are purely additive. **Do this first; it is ~80% of the risk.**

> **Status (2026-07-08): Phase 0a DONE; 0b remaining.**
> **0a (landed, behavior-identical, all tests green):** the four viewport fields are consolidated
> into a `PaneView` struct + a single `pane_view` field (main.rs); a `doc(idx)` accessor exists on
> `AppHost`/`Editor` (`doc(active_index()) == document()`); all 12 render-path `self.editor.document()`
> calls were swapped to `self.editor.doc(self.editor.active_index())`. The 0.5 grep **as written
> passes**. These are the *containers* Phase 2 needs (a `PaneView` to promote to a vec/map; a
> doc-by-index accessor).
> **0b (NOT done — the real de-risk):** the render helpers still implicitly target the single
> `pane_view` and the active doc (`active_index()`) — they don't yet *take a pane parameter*, so the
> "wrong-pane" latent bug the grep targets still exists, just spelled differently. **Finding: the
> 0.5 grep is a weak proxy** — a field rename passes it without true parameterization. 0b = do the
> real threading (0.3 below) AND strengthen the grep (see 0.5). This is where the borrow friction
> lives (`raster` is ~300 lines over lots of `self`; the leaf helpers call each other).

### 0.0 The struct

```rust
struct PaneView { scroll_line: usize, scroll_vertical: f32, follow: bool, canvas_px: (usize,usize) }
```

Store panes in a **map**, not an inline field — `self.pane_views: HashMap<PaneId, PaneView>` plus
`focus: PaneId`, with one entry in this phase. Reason: the render hub `windowed_buffer(&mut self)`
also mutates `self.font_system`; `self.pane_views.get_mut(&id)` + `&mut self.font_system` are
*disjoint* field borrows (allowed), whereas an inline `self.view` + `&mut self.font_system` is a
double `&mut self`. Designing for the map now avoids borrow surgery when N panes arrive.

### 0.1 Relocate the four fields (mechanical rename) — *no-op*

- [ ] `scroll_line` (~31 sites) → focused pane's `view.scroll_line`
- [ ] `scroll_vertical` (~18) → `view.scroll_vertical`
- [ ] `follow` (~45) → `view.follow`
- [ ] `last_canvas` (~9) → `view.canvas_px`
- [ ] Add `focused_view(&self) -> &PaneView` / `focused_view_mut(&mut self) -> &mut PaneView`.

### 0.2 Collapse `follow` sets — *no-op*

- [ ] Add `fn request_follow(&mut self) { self.focused_view_mut().follow = true; }`
- [ ] Replace the ~40 `self.follow = true` sprinkles (2286…6010) with `self.request_follow()`.
- [ ] The ~6 logic reads / `= false` (1814, 1840, 1847, 2189, 5442, 5606) move into the core fns via
      the `view` param in 0.3.

### 0.3 Parameterize the core render/scroll fns with `(view: &mut PaneView, doc: &Document)` — *no-op*

Thread explicit view + doc through these; keep font config from `&self`. Call each at its single site
with `(self.pane_views.get_mut(&self.focus).unwrap(), self.editor.document())`.

- [ ] `compute_window` (1613) → `(doc, scroll_line, h)`
- [ ] `build_window_cosmic` (1647) → add `doc`
- [ ] `windowed_buffer` (1767) → `(view, doc, w, h)` — the hub
- [ ] `win_cursor_at` (1781) / `win_cursor_clamped` (1792) / `win_rope_at` (1799) → add `doc`
- [ ] `raster` (1805) → `(view, doc, w, h)` (includes the `follow` clamp block 1811–1851)
- [ ] `page_move` (2138), `visual_vmove` (2248), `visual_line_edge` (2293) → target focused view
- [ ] `hit` (2322), `hit_leading` (2335) → will take the target pane later; for now focused view

### 0.4 Read-only non-active doc accessor — *no-op*

- [ ] Add `AppHost::doc(&self, idx: usize) -> &Document` (and editor pass-through). Rendering a
      non-focused pane must read its doc without disturbing `active`.
- [ ] Swap the 19 internal `self.editor.document()` reads inside the core fns to the passed `doc`.

### 0.5 Definition of done (proves the no-op)

- [x] **(0a)** Inside the render path (`compute_window`…`hit_leading`), the *original* grep returns
      **zero**: `self.scroll_line | self.scroll_vertical | self.follow | self.last_canvas | self.editor.document()`.
      *Passes — but see the caveat: a field rename passes it without real parameterization.*
- [ ] **(0b — STRONGER grep, the real checkpoint)** Inside the render path, this also returns
      **zero**: `self.pane_view\b | active_index()`. These only vanish once the helpers **take** the
      pane's view + doc index as parameters (0.3) rather than reading the single `pane_view` / active
      doc. This is the grep that actually proves "Phase 2 touches none of this."
- [x] `cargo build` clean; app builds; behavior identical (528 tests green; the 1 red test is the
      concurrent `$(…)` task, unrelated).
- [ ] **(0b)** Eyeball wheel-scroll, PgUp/Dn, click-to-caret, go-to-end, preview-sync (~5407) after
      the 0b threading, since those touch the moved state.

---

## Phase 1 — Layout tree data model

### 1.1 Ids

- [ ] `Document` gains `id: DocId(u32)` assigned from `AppHost.next_doc_id`.
- [ ] `AppHost::doc_index(&self, id: DocId) -> Option<usize>` (the id⇄index bridge).
- [ ] `PaneId(u32)` from `PaneTree.next_pane`.

### 1.2 Tree types (new module, e.g. `xyscript-app/src/panes.rs`)

```rust
enum Split { Stacked, SideBySide }
struct Pane { id: PaneId, doc: DocId, view: PaneView }   // PaneView moves here from the GUI map
enum Node { Leaf(Pane), Split { dir: Split, ratio: f32, a: Box<Node>, b: Box<Node> } }
struct PaneTree { root: Node, focus: PaneId, zoom: Option<PaneId>, next_pane: u32 }
```

- [ ] `PaneTree` hangs on `AppHost` next to `active`; a fresh host = one `Leaf` for the active doc.
- [ ] `sync_active_to_focus(&mut self)` — set `self.active = doc_index(focused_pane().doc)`.
- [ ] Enforce the **≤1 pane per `DocId`** guard in `split` (per *buffer*, not per file — two
      `OPEN/NV` copies of one file are distinct `DocId`s and may share a split).

### 1.3 Operations (unit-testable, headless — no GUI needed)

- [ ] `layout_rects(&self, area) -> Vec<PaneRect{pane,doc,rect}>` (zoom short-circuits to one rect).
- [ ] `split(dir, incoming: DocId, new_on: Side)` — replace focused `Leaf` with a `Split`.
- [ ] `close_pane(pane)` via recursive `collapse` — hoist the sibling into the parent's place.
- [ ] `focus_cycle(forward)`, `swap_focused()` (TRV), `toggle_zoom()` (TZ).
- [ ] Tests: split→collapse round-trips to the original leaf; collapse of a nested tree keeps
      siblings; focus survives collapse; zoom toggle is idempotent-pair.

---

## Phase 2 — Per-pane render loop + input routing (GUI)

### 2.1 Render

- [ ] `PaneView`s now keyed by `PaneId` in the tree; GUI reads them via the host.
- [ ] Frame loop: `for PaneRect{pane,doc,rect} in layout_rects(area)` → set `view.canvas_px`,
      `raster_pane(view, host.doc(doc), px)`, blit as one texture per pane.
- [ ] Draw the focused pane's caret/selection overlay offset into its rect; non-focused panes keep
      **selection** visible but drop/dim the caret.
- [ ] Draggable divider(s) between panes; drag updates the parent `Split.ratio`.

### 2.2 Input routing

- [ ] Stash `self.pane_rects: Vec<PaneRect>` each frame; add `pane_at(pos) -> Option<PaneId>`.
- [ ] Click → focus that pane + `sync_active_to_focus` → screen→pane-local px → `win_rope_at` on that
      pane's window → move caret.
- [ ] Wheel → the pane **under the cursor** (not the focused one): nudge its `view.scroll_line`,
      clear its `follow`.
- [ ] `page_move` / MU / MD / `set_viewport*` (1851, 5452) → the **focused** pane.
- [ ] Scrollbar (5567, 5603, 5710) + preview-sync (5401) → the focused pane (one scrollbar in v1).

---

## Phase 3 — Verbs, readbacks, docs, tests

### 3.1 Verbs (VM `exec_command` + `EditorOp`s + `library.rs` BUILTIN_VERBS)

- [ ] `NEW/H`, `NEW/V` — new empty buffer, split Stacked-above / SideBySide-left of current.
- [ ] `THS [target]`, `TVS [target]` — split with `target` (default alternate buffer, else new empty);
      current pane on top / left.
- [ ] `TW` — move focus to the next pane (wraps).
- [ ] `TUS` — un-split: collapse to the focused pane. Define `AB` (close buffer) to also collapse.
- [ ] `TRV` — swap the two split panes.
- [ ] `TZ` — toggle-zoom the active pane.

### 3.2 Readbacks + introspection

- [ ] `$PANES` = pane count (`layout_rects().len()`); `$PANE` = focused pane's position.
- [ ] Optional `LISTPANES` (a la LISTTABS) and/or a layout-string special.

### 3.3 Docs + tests

- [ ] `#cmd` entries in `docs/reference.typ` for every verb (the no-omission test enforces this);
      spell out the Stacked/SideBySide meaning of `/H` `/V` explicitly (a tiny diagram helps).
- [ ] Editor tests: split creates 2 panes; TW cycles focus; TUS/`AB` collapse; TRV swaps; TZ toggles;
      `$PANES`/`$PANE` report correctly; the ≤1-pane-per-doc guard holds.
- [ ] Rebuild `dist/xyscript.exe`.

---

## Open questions (decide before the phase that needs them)

- **Arena vs Box tree** (Phase 1): `Box` is fine for a handful of panes; if `collapse`/`swap` get
  fiddly, a flat `Vec<Node>` arena keyed by `PaneId` mutates more easily.
- **Persistence** (post-v1): should the layout survive save/restore like WINGEO? The tree serializes
  to a short string cleanly; fold into `SaveCfg`/`LoadCfg` if wanted.
- **Same *buffer* (`DocId`) in two panes** (v2): only enable alongside buffer/view separation; keep
  the v1 guard until then, or two panes fight over one caret + `follow`. (Same *file* in two panes is
  already possible in v1 via `OPEN/NV` — those are distinct buffers; this is about one buffer twice.)

## Sequencing

Phase 0 is independently shippable and valuable on its own (cleaner render state). Land and verify it
first. Phases 1–3 can follow in order; Phase 1 is headless-testable before any GUI work in Phase 2.
