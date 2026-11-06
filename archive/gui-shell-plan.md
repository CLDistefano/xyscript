---
title: gui-shell-plan
tags: [design, gui, shell, framework, spike, preview, end-product-ui]
created: 2026-06-08
status: building — live shell (`xyscript-gui/`). M1–M4 ✅ LIVE-CONFIRMED 2026-06-08 (window/render/input · scroll/mouse/colors · multilingual typing/IME · preview pane + arrow motion). M5 (menus + file verbs on real disk: File/View menus, rfd dialogs, native CMline + focus semantics) ✅ LIVE-CONFIRMED 2026-06-08. M5.1 (view-settings cluster: CMline placement Top/Bottom/Hidden + persist-last-command, preview-pane toggle that skips all render work when hidden) ⏳ built & green, awaiting live confirmation. DECIDED: GUI1 = egui-hybrid + CPU-rastered cosmic-text canvas; GUI4 = cosmic-text (validated); GUI2 = physical-key→KBD-as-layout + OS-text/IME (validated EN+Cyrillic). NEXT: inline preedit, shift-select, multi-page preview, multi-window (GUI6).
companion_to: gfm-track-plan.md (8d preview), xyscript-product-scope.md, xyscript-foundational-premise.md (keyboard capture)
---

# GUI shell — plan

*The GUI is the **confirmed end-product UI** (the crossterm terminal is
transitional). This track stands up that shell and, with it, the GFM **live
preview** (8d). It's the first track that isn't cleanly headless-testable, so the
governing principle is: **keep it a thin shell over the tested core.***

---

## Multilingual requirement (2026-06-08) — reframes GUI1

The target audience is **international language professionals** (scholars,
lexicographers, translators); **multilingual type + render + print in one
document is a CORE requirement** (mixed scripts on a line: Latin + CJK +
Arabic/Hebrew [bidi] + IPA/combining diacritics + emoji). This promotes exactly
egui's weak spots — complex-script shaping, automatic font fallback, IME — from
"later" to "now," **for the editing surface**.

What this changes:
- **Print is already covered** — typst (G3) does shaping/bidi/fallback well; no
  new work on the print leg.
- **The editing surface needs `cosmic-text`-grade text from the start** (shaping +
  bidi + auto-fallback). So **egui's text layouter is ruled out as the editing
  canvas** — but egui (or another framework) may still be the **shell** (chrome).
- **Input must be OS layout/IME-aware** (see GUI2) — character entry via OS
  text/IME, commands via the KBD resolver.
- The egui spike still counts: it validated the **wiring** (core ↔ GUI ↔ typst
  preview), which transfers to any shell. Only the **editing-surface text +
  input** layer is re-decided.

So GUI1 narrows to: **which shell wraps a `cosmic-text` editing canvas?**

---

## Principles (the guardrails)

1. **Thin, swappable shell over the headless core.** All editor logic stays in
   the tested core (`Editor`/`Buffer`/`Document`/viewport/styled spans), exactly
   as `main.rs`+crossterm is today. The GUI only: opens a window, sources input,
   draws, and shows the preview. Because the display has always been a firewalled
   shell, **the framework choice is not a one-way door** — like crossterm→GUI, a
   later swap (e.g. to a lower-level stack) stays cheap.
2. **Reuse the keyboard *resolver*, swap the *capture*.** `xyscript-kbd`'s KBD
   model / modifier logic / "it's all XPL" resolution stays. Only the **capture
   source** changes: from the OS non-exclusive hook to the GUI framework's key
   events, normalized to the canonical `KeyEvent` and fed to `Editor::feed_key`.
   (The sine qua non — keyboard control — is preserved; see
   [[xyscript-foundational-premise]].)
3. **No webview.** Prior decision (G2): native rendering only.
4. **Preview = the same typst markup → raster image** (8d). Render the current
   doc's typst markup to an image and blit it in a pane — "what you preview *is*
   what you print." Live-refresh debounced (G5).
5. **We render our own editing surface — on a `cosmic-text`-grade text stack.**
   Draw the core's `styled_rows` + cursor/selection ourselves (so styling, cursor
   semantics, and large-doc virtualization match the core) **using a real text
   layout engine** (`cosmic-text`/`parley`) for shaping, bidi, and font fallback —
   not the shell framework's simple text layouter. This is the multilingual
   requirement made concrete.
6. **Headless discipline preserved elsewhere.** The GUI is the one inherently-
   hard-to-unit-test layer; keeping it thin keeps the untested surface minimal.

---

## The decision, reframed: shell + a `cosmic-text` editing canvas

The editing canvas is `cosmic-text`-grade in every viable option (that's the
multilingual requirement). So the choice is **which shell** wraps it. Criteria:
**multi-window support** (for "open in a separate window" — see "Tabs & windows"
below; winit native, iced native, egui via *viewports*; single-window-only is a
strike) ·
chrome (menus/dialogs/file-browser — the "operating environment") · how cleanly a
custom `cosmic-text` canvas embeds · keyboard + IME access · maturity · ethos fit.

| Option | Editing canvas | Chrome | Notes |
|---|---|---|---|
| **egui shell + `cosmic-text`/`glyphon` canvas** (hybrid) | cosmic-text via a wgpu paint-callback | **egui (excellent)** | Keeps egui's fast chrome (the spike's wiring transfers); editing text done right. Cost: embedding a `glyphon` render pass inside egui + coordinating layout/hit-testing. **Lean.** |
| **`iced` shell + cosmic-text canvas** | iced's text *is* cosmic-text | good (retained widgets) | Text engine already cosmic-text; mature. Its text-*editor widget* imposes an editing model, so we'd still custom-render the buffer. |
| **`floem` shell** (Lapce's) | cosmic-text-based | good, editor-oriented | Best editor pedigree; younger/less-documented. |
| **`winit` + `cosmic-text` + `glyphon`** (purist) | cosmic-text (direct) | **none — hand-rolled** | Best text/input control; **worst** for the chrome-heavy international operating environment. |

---

## Decisions (OPEN — leans noted)

| # | Decision | Status / lean |
|---|---|---|
| **GUI4** | Editing-canvas text engine | ✅ **VALIDATED (2026-06-08): `cosmic-text`** (Spike 2 Phase A). A mixed-script render (`text-spike/`) confirmed shaping, **bidi + Arabic joining**, combining diacritics, CJK auto-fallback, and color emoji — all from the system fonts, default `Attrs` + `Shaping::Advanced`. |
| **GUI1** | Shell (wraps the canvas) | ✅ **DECIDED (2026-06-08): egui-hybrid + CPU-rastered cosmic-text canvas** — validated live in Milestone 1 (`xyscript-gui/`). egui for window/chrome/input; cosmic-text rasterized to an egui texture for the editing canvas (multilingual). GPU `glyphon` remains an available later optimization (wgpu-29-compatible). Must still satisfy multi-window (GUI6) when that lands. |
| **GUI2** | Input integration | ✅ **DECIDED (2026-06-08): the KBD is the layout.** Capture **physical keys** (positional, e.g. winit `KeyCode`) → the **`xyscript-kbd` resolver FIRST**, so a KBD file *is* the keyboard layout (XyWrite-faithful) — overriding the OS layout, **live-swappable** (`LOADKBD`), and UTF-8 so a KBD can `insert` any script's characters. **OS text/IME is the FALLBACK** — for keys the active KBD doesn't claim, and specifically for **composition input** (Pinyin→Hanzi, Japanese kana→kanji) that a keycode→string map can't do. The editing canvas positions the IME candidate window at the caret. (Supersedes the spike's logical-key stopgap *and* the earlier "OS-text-for-insertion" lean.) |
| **GUI3** | Preview mechanism | ✅ **typst markup → raster image** pane (verified). **Debounced / off-thread** — typst recompile per keystroke is heavy. |
| **GUI5** | Font bundling | Bundle broad script coverage (Latin/CJK/Arabic/Hebrew/IPA/emoji) for both the editing canvas and typst (G3), so out-of-the-box multilingual works. To detail. |
| **GUI6** | Tabs & windows | New document / directory screen opens in a **new tab** vs a **separate window**, user-selectable via **XPL verbs + menu**. *Lean / approach:* see below. Multi-window capability is a **GUI1 criterion**. |

---

## Tabs & windows (GUI6 — forward-compat)

Goal: let the user open a new document or directory screen in a **new tab** or a
**separate window**, via XPL *and* a menu. Two halves:

- **Tabs are essentially already modeled.** The multi-document core (`AppHost.docs:
  Vec<Document>` + `active`, with `NEW`/`OPEN`/`BUFNEXT`/`BUFGO`/`BUFLIST`) *is* the
  tab substrate — a tab = an open buffer. The GUI just renders a tab strip from
  `buffer_list()`/`active_index()`; clicking a tab is a `BUFGO`. Nearly free once a
  shell exists.
- **Separate windows** need: **(a)** a multi-window shell (now a **GUI1** criterion
  — winit/iced native; egui via *viewports*); **(b)** a clean core generalization —
  split today's conflated `AppHost` into a **shared document store** + per-**window**
  *tab-sets* (each window: which docs it shows as tabs + its active tab). The
  current single-window case = "one window holding all docs." *No per-view cursor
  duplication needed:* a doc lives in exactly one tab/window, and the cursor already
  lives in the `Document`. (Showing the **same** doc in two places — split views —
  is a separate, later feature.)

Surface: a placement modifier on `NEW`/`OPEN` (`/tab` vs `/window`) or dedicated
`TABNEW`/`WINDOW` verbs; menu items invoke the same verbs ("it's all XPL").

Uniformity: model "what a tab/window hosts" as a **view** (document-view *or*
directory-view), so tab-vs-window applies identically to docs and the file browser.

Bounded extension of the existing multi-doc model; **deferrable** to after the GUI
shell lands — nothing now forecloses it.

---

## Spike 1 outcome (2026-06-08) — egui *wiring* validated ✅ (text engine since superseded)

Built `gui-spike/` (detached crate, path-deps on the real `xyscript-app` +
`xyscript-kbd`). **Note:** the multilingual requirement (above) later ruled out
egui's *text layouter* as the editing canvas; what this spike validated — the
**wiring** — still holds and transfers to any shell. Findings:
- **Integration compiles cleanly** against the real core: egui key events →
  `Editor::feed_key`; the highlighter's `styled_line` spans → an egui `LayoutJob`;
  the typst preview → an egui texture. egui hosts our core with little ceremony —
  egui remains a strong **shell** candidate (chrome), per GUI1.
- **Preview path verified** (the part needing real rendering): `--render-only`
  produced a clean raster PNG of the doc (heading, justified paragraph with
  bold/italic/mono, bullet list) — i.e. **GUI3 (typst → raster) works**; the live
  pane shows the same image. The full live *window* wasn't visually verified from
  the build host (headless); run `cargo run` in `gui-spike/` to see it.
- **GUI2 finding (the real decision):** egui (like winit) delivers **logical
  keys**, not OS scancodes — so `xyscript-kbd`'s native-scancode *normalization*
  layer doesn't plug in directly. The spike hand-maps `egui::Key` → canonical
  set-1 codes and feeds `feed_key`, routing through the KBD pipeline (preserves
  "it's all XPL"). Decision to make: **(a)** keep a logical-key→canonical map at
  the GUI boundary, or **(b)** drop to winit's physical `KeyCode`. *Lean: (a)* —
  it's small and keeps the resolver intact.
- **Perf note:** re-compiling typst on every keystroke is heavy (~tens–hundreds
  ms) → the live preview must be **debounced / off-thread** (G5).
- eframe 0.34 API notes: `App::ui` is now the required method, `update` is
  defaulted; we override `update`.

## Spike 2 outcome — Phase A (cosmic-text rendering) ✅ (2026-06-08)

`text-spike/` (detached crate; `cosmic-text` 0.19 + `image`) rendered a
deliberately mixed sample to PNG via `FontSystem` (auto fallback) + `Buffer`
(`Shaping::Advanced`) + `SwashCache`, blending each glyph rect (mask alpha *and*
color-emoji) onto a canvas. **Verified visually:** Latin/Greek/Cyrillic/CJK/Korean
shaping with automatic font fallback; **bidi with cursive Arabic joining** (RTL
runs inside an LTR line); IPA + stacked/combining diacritics (Vietnamese);
**color emoji**. (Windows quirk: the regional-indicator flag shows as "US" — a
Segoe-UI-Emoji limitation, not cosmic-text.) **Conclusion: cosmic-text is the
editing canvas (GUI4).** Print side already covered by typst; this closes the
*rendering* question.

## Spike 2 outcome — Phase B (verifiable parts) ✅ (2026-06-08)

Extended `text-spike/` to a `cosmic_text::Editor` view:
- **Editing surface renders** — a **caret** + a **selection** that correctly spans
  the **bidi** line (one contiguous visual highlight across Hebrew-RTL → "and
  Arabic"-LTR → Arabic-RTL), verified by PNG. So cosmic-text hosts a real *editor*
  canvas, not just static text.
- **Hit-testing works** — `Buffer::hit(x,y) → Cursor{line, byte-index}` on every
  line incl. the bidi one. And `Cursor{line, index(byte)}` is **exactly our core's
  model** (a doc byte offset ↔ line + byte-in-line via the rope's geometry) — the
  bridge is concrete.
- **egui-hybrid is dependency-compatible** — `glyphon` 0.11 and `egui-wgpu` 0.34
  both ride **wgpu 29**, so cargo unifies them; the usual glyphon-in-egui
  wgpu-version coupling is **not** a blocker here. Clears the main technical risk
  on the egui-hybrid GUI1 path.

**Still needs a live window (your follow-up):** IME preedit/commit + candidate
placement; **physical-key** capture → KBD resolver (GUI2); wiring the glyphon
canvas to the core in a running shell; and the GUI1 *feel* decision (egui-hybrid
vs iced/floem).

## Live shell build — Milestone 1 (2026-06-08)

`xyscript-gui/` (detached crate; eframe + cosmic-text + core path-deps) — the real
shell, MVP. **Built ✅; canvas verified headlessly.** Approach: **CPU-raster the
cosmic-text canvas → egui texture** (reuses the validated spike path; no GPU
glyphon for the MVP — that's a later optimization, and we confirmed it's
wgpu-29-compatible if/when wanted). M1 contents:
- **Editing canvas** renders the real `Editor`'s document via cosmic-text (caret
  from the core cursor via `byte_to_cursor`, selection from `selection_span`).
  `--render-only` PNG confirmed multilingual shaping/bidi + caret on the seeded doc.
- **Input (GUI2):** egui **physical** keys → canonical set-1 codes → `feed_key`
  (KBD-as-layout path; English letters/space/enter/backspace in M1).
- **Status panel** (file/ln:col/encoding). Thin shell — logic stays in the core.
- **Deferred to later milestones:** OS-text/IME fallback (multilingual typing),
  per-span highlight colors in the canvas, scrolling/virtualization, the typst
  **preview pane**, menus + full verb wiring, multi-window/tabs (GUI6).

**✅ LIVE-CONFIRMED (2026-06-08, user-run):** the window shows the multilingual doc
+ caret + status, and English typing/motion edits it — "everything works, nothing
off." So **GUI1 is decided: egui-hybrid shell + CPU-rastered cosmic-text canvas**,
validated live. The thin-shell-over-tested-core architecture holds in a real window.

## Live shell build — Milestone 2 (2026-06-08): editor usability

Added to `xyscript-gui/`: **scrolling** (follow-caret on edit/motion; mouse-wheel
free-scroll via cosmic-text `Scroll`), **highlight colors** (core tokens →
cosmic-text `set_rich_text` per-span color), **mouse** (click → `set_active_caret`;
drag → selection, via `Buffer::hit` → byte offset). New core passthrough:
`Document::set_caret` + `AppHost::set_active_caret`. **Colors + scrolling verified
headlessly** (`--render-only` → `canvas.png` colored, `canvas_scrolled.png` line 8
shows list/code-block in green) — mouse is **live-only (your run)**. Note: bold/
italic *weight* in the canvas not yet applied (colors only); needs registered
variant fonts — a later refinement. **Fix (user-reported):** wheel-up from the
bottom took a few dead turns — `self.scroll_line` accumulated out-of-bounds slack;
now synced back to cosmic-text's clamped `buffer.scroll().line` after layout.

## Live shell build — Milestone 3 (2026-06-08): multilingual typing (GUI2 channel 2)

Wired OS-resolved character input into `xyscript-gui/`:
- **`egui::Event::Text`** → `Editor::insert_str` (new core method → `EditorOp::
  InsertText`) — OS layout-resolved chars (accents, non-Latin layouts, **uppercase
  via Shift**, multilingual).
- **`egui::Event::Ime`** — `Commit` → insert; `Preedit` → shown in the status line;
  `Enabled/Disabled` → clear. `output.ime` set each frame (enables IME + places the
  candidate window near the caret line).
- **`key_to_code` reduced to command keys** (Enter→`insert "\n"`, Backspace→`bs`)
  so printables go via `Text` (no double-insert); KBD-as-layout override for bound
  keys is a later refinement.
- **✅ LIVE-CONFIRMED (2026-06-08): English + Cyrillic OS layouts type correctly**
  (the `Text` channel; uppercase via Shift works). **IME *composition* (CJK/JP)
  is wired (`Ime` path) but UNTESTED** — no such keyboard installed; confirm later.
  MVP gaps: inline preedit not drawn in-canvas (status only); candidate-window x
  ≈ line start; RTL caret-while-typing may be rough; arrow-key motion still
  unbound (use mouse).

## Live shell build — Milestone 4 (2026-06-08): preview pane + arrow motion

- **Preview pane** (right panel): `xyscript_app::gfm_to_image(source, scale)` (NEW
  core fn — typst→raster page-0 via `typst-render`, the 8d building block) →
  egui texture. **Debounced** (~0.4 s after typing pauses; `request_repaint_after`).
  Scale = pane_w / 595 (A4 width pt). Preview render verified headlessly (app pdf
  test); the live pane is user-verified.
- **Arrow motion**: added `75=LEFT 77=RIGHT 72=UP 80=DOWN 71=HOME 79=END` to the
  base table of `xyscript-sample.kbd`; `key_to_code` maps egui arrow/Home/End →
  those codes → `feed_key` → motion verbs (the KBD-as-command path). kbd tests green.
  **Fix (user-reported):** numpad nav didn't work — its *physical* key is a numpad
  code, not `ArrowLeft`; now we try `physical_key` then fall back to the *logical*
  `key` (which is `ArrowLeft`/`Home`/… when NumLock is off; digits→Text when on).
- Workspace green (app lib 108). MVP gaps unchanged (inline preedit; exact IME x;
  shift-arrow selection needs modifier plumbing; multi-page preview shows page 0).

## Live shell build — Milestone 5 (2026-06-08): menus + file verbs

Turns the shell from a seeded virtual doc into a **day-to-day app on real files.**

- **Real disk:** removed the `virtual_fs=true` + `scratch.md` seed; the shell now
  starts on an **empty untitled** document (highlight off by default — the quiet
  screen). The `SAMPLE_MD` constant is kept (`#[allow(dead_code)]`) as a paste-in.
- **Menu bar** (`egui::MenuBar::new().ui` — the non-deprecated API; items use
  `ui.close()`):
  - **File:** New · Open… · Save · Save As… · Export PDF…
  - **View:** Highlight on/off · Command line (toggle).
- **Native dialogs** via `rfd 0.17` (Win32 default features): Open = `pick_file`
  (md/markdown/txt filter); Save As/Export = `save_file` (md / pdf filters). Save
  with no path falls through to Save As.
- **New core seam:** path-safe file-op methods on `Editor`
  (`open/save/save_as/export_pdf/new_doc/set_highlight/active_path`) calling
  `host.editor_op(EditorOp::…)` through a private `op_status` helper
  (`VmResult<Value>` → `Result<String,String>`). These bypass command-line escaping,
  so paths with spaces/`\` are safe (the command bar is for *typed XPL*; menus are
  for *picked paths*).
- **Command line = the editor's NATIVE CMline** (corrected from the first cut, which
  used a separate egui `TextEdit` and (a) leaked Enter as a newline into the document
  and (b) couldn't express XyWrite focus semantics). Now:
  - `View ▸ Command line` toggles `editor.open_cmdline()` / `cancel_cmdline()` —
    these set the host `Target` (`Document` ↔ `CmLine`); `editor.cmline_open()` IS
    the focus state. The GUI renders `editor.command_line()` (text + caret) as a
    top panel **only while open** — no egui text widget at all.
  - All keystrokes keep flowing through the editor; the host's `active()` already
    routes inserts/motion to the CMline vs the document by `target`. The ONLY
    special-cased keys in CMline mode: **Enter → `submit_cmdline()`**, **Esc →
    `cancel_cmdline()`**. So Enter can never insert a newline into the document.
  - New `Editor` methods: `open_cmdline` / `cancel_cmdline` (via `EditorOp`), and
    `submit_cmdline` (reads the CMline text, clears it, `target=Document`, then
    `dispatch`es it — mirrors `post_run`'s submit branch since the GUI invokes it
    directly, not via a key→dispatch cycle).
- **Focus semantics (the refinement, now correct):** after a submitted command,
  focus follows the host `target`, which the command itself manages — so it
  **returns to the document by default**; a command that switches/closes buffers
  (`BUFNEXT`/`BUFGO`/future `AB`) leaves `target=Document` with a new active doc, so
  focus lands **in the next document in Z-order** at its own caret; a command can
  keep the command line by re-issuing `OpenCmdLine`. A **canvas click also cancels**
  the CMline (click in text ⇒ focus to document).
- **Menu-vs-canvas keyboard guard:** `let canvas_has_keys =
  !ctx.egui_wants_keyboard_input();` still wraps the input loop + `output.ime`, so an
  open *menu* doesn't double-drive the canvas. (No widget contention from the command
  line anymore — it isn't a widget.) Mouse/wheel stay unconditional.
- **Status line** now appends the last op/command message (`— opened …` / `error: …`).
- Pattern note: menu/command actions are collected into a local `enum Act` during
  the UI pass and **applied afterwards**, so `self` is never mutated inside egui's
  nested menu closures (avoids the reborrow tangle).
- Workspace green (app lib 108 + kbd 4); GUI builds warning-free. ✅ **LIVE-CONFIRMED
  by the user 2026-06-08** (menus, dialogs, command line, focus semantics all work).

## Live shell build — Milestone 5.1 (2026-06-08): view-settings cluster ⏳ built, awaiting live-confirm

Built per the locked decisions (scope = both; CMline default Top; idle line keeps the
last-submitted command in normal text).
- **`Placement` enum {Top, Bottom, Hidden}** + GUI state `cmline_placement` (default
  **Top**), `last_cmd`, `preview_visible` (default **off** — authoring-first quiet
  screen, like highlight-off-by-default; user-set 2026-06-08).
- **Display ≠ focus** (the key decoupling): the CMline panel renders from placement,
  **not** from `cmline_open()` (which stays the pure focus/target signal):
  - **Top** → reserved `Panel::top("cmdline_top")` below the menu, always visible.
  - **Bottom** → reserved `Panel::bottom("cmdline_bottom")`, shown *after* the status
    panel so status stays at the very bottom edge.
  - **Hidden** → no reserved line; shown transiently at the top only while focused.
  - `cmline_label()`: focused → live buffer text + caret; **unfocused → the
    last-submitted command** (`last_cmd`, captured on Enter before submit clears the
    buffer), or a bare `:` prompt. ⇒ satisfies "persist after a command."
- **View menu:** `Preview pane` toggle (✓), `Focus command line`, and a
  `Command line position ▸ Top/Bottom/Hidden` submenu (✓ on the current one). Tick
  marks via a small `tick()` helper; current state read into locals before the menu
  closures (keeps `self` out of them).
- **Preview toggle = real savings:** the entire `Panel::right("preview")` closure is
  wrapped in `if self.preview_visible` — when off, **no `gfm_to_image` typst compile,
  no debounce, no `request_repaint`**. On hide we drop `preview_tex`/`preview_src`/
  `last_text` so re-show renders fresh.
- GUI builds warning-free; app/kbd unchanged (108 + 4 green).

### (original deferral note) CMline display model — user-requested 2026-06-08
Two related requests that together form a "CMline display model"; design them as one
unit, since both pull apart **display** (is the line on screen?) from **focus** (is it
the active target?) — currently the panel renders *iff* `cmline_open()`, conflating the
two.
1. **Persist the CMline after entering a command** — it should stay displayed
   (showing the last command / ready for the next) rather than vanishing when
   `submit_cmdline()` flips `target` back to `Document`. ⇒ introduce a separate
   *visible* flag (and likely keep the last-submitted text shown), decoupled from the
   `target`-based focus state. Focus still returns to the document per the M5 rules;
   only the on-screen presence persists.
2. **Placement option: top / bottom / hidden** — a user setting (XPL verb + a View
   menu submenu) for where the CMline sits (`Panel::top` vs `Panel::bottom`) or
   whether it shows at all. "Hidden" = command line still reachable (menu/keybind) but
   no reserved line. Pairs naturally with binding a KBD key to `OpenCmdLine` (today
   it's menu-only).
Implementation sketch: add e.g. `cmline_placement: Top|Bottom|Hidden` + `cmline_visible`
GUI state (or, cleaner, model it in the host so XPL can drive it); render the CMline
panel from those, not directly from `cmline_open()`; keep `cmline_open()` purely as the
focus/target signal.

### Preview pane toggle — FUTURE refinement (user-requested 2026-06-08, deferred)
Make the right-hand typst preview pane **optional** (a View-menu toggle + XPL verb).
The point is not just chrome: when the pane is **hidden, skip the preview work
entirely** — no `gfm_to_image` typst compile, no debounce timer, no `request_repaint`.
- Cheap because the debounced render already lives *inside* the `Panel::right("preview")`
  closure; gate the whole panel behind a `preview_visible` flag and the compile path
  simply never runs when hidden. Drop the stale `preview_tex`/`preview_src` on hide so
  it re-renders fresh on re-show.
- Belongs with the **CMline display model** as part of a small "pane/line visibility"
  settings cluster (View menu + XPL verbs; eventually host-modeled so XPL can drive
  layout). Authoring-first default could even be **preview off** (quiet screen),
  consistent with highlight-off-by-default.

## Live shell build — Milestone 6 (2026-06-08): Shift+arrow selection (modifier plumbing) ⏳ built, awaiting live-confirm

The engine + XPL already supported extend-motion (`Buffer::prepare_move`: extend sets
the anchor, non-extend clears it; VM: `LEFT SEL`/`RIGHT SEL`/… → `Move{extend:true}`).
What was missing was the **modifier path from egui → the resolver**:
- **The seam:** `KeyEvent::inject(code, bitmask)` resolves `keymap.resolve(bitmask, code)`
  *directly* (the carried bitmask IS the layer; injection skips physical bookkeeping).
  The GUI was using `::down` (bitmask 0; resolver computes from `InputState` it never
  receives, since the GUI doesn't feed shift/ctrl down/up) → always the base table.
- **New `Editor::feed_key_mods(code, shift, ctrl, alt)`** — ORs the operative keymap's
  bits for the active modifiers (`keymap.bitmask_for_names`) and injects. Undeclared
  modifiers contribute nothing (strict lookup then finds no binding — never a wrong
  layer). The GUI passes egui's `modifiers.{shift,ctrl,alt}` through it for every
  command key (arrows/Home/End/Backspace/Enter), replacing the old `::down`.
- **KBD:** added `75=LEFT SEL … 79=END SEL` to the SHIFT table (the base table already
  had the plain-motion arrows). So Shift+arrow/Home/End extend the existing
  mouse-rendered selection (`selection_span`); a plain arrow collapses it.
- **Letters stay on the OS Text/IME channel** (Shift→uppercase is the OS's job); only
  command keys go through `feed_key_mods`. Note: this does NOT yet enable Ctrl+letter
  commands (Ctrl+S/E/N/Q exist in the CTRL table but letters aren't in `key_to_code`)
  — that's the separate "full physical-key mapping / KBD-as-layout override" item.
- Tests: new e2e `shift_arrow_extends_the_selection` (type, Shift+Left ×2 selects "we",
  plain Left collapses). app 113 (108+5) + kbd 15 green; GUI warning-free (dropped the
  now-unused `KeyEvent` import).

## Later: remaining milestones — Ctrl+letter commands (full physical-key map) · CMline keybind (Ctrl+E already in KBD) · inline preedit · multi-page preview · multi-window

Targets the now-critical layer (the part Spike 1 deliberately stubbed): proving a
`cosmic-text`-grade editing canvas, and choosing the shell around it.

1. **Render a deliberately mixed sample** in a `cosmic-text` (and/or `parley`)
   canvas: Latin + Hebrew/Arabic (**bidi**) + CJK + IPA/combining diacritics +
   emoji on the same lines. Verify **shaping, bidi, and automatic font fallback**,
   plus cursor/selection hit-testing across scripts.
2. **Input (GUI2 as decided):** capture **physical keys** → `xyscript-kbd`
   resolver first (the KBD *is* the layout; a language KBD inserts that script's
   chars; live-swappable via `LOADKBD`). Confirm **OS IME fallback** — preedit/
   commit for composition scripts (Pinyin/Japanese) the KBD doesn't claim — with
   the candidate window positioned at the caret. Verify the physical-key path is
   reachable in the chosen shell (winit `KeyCode`; egui exposes less, a factor in
   GUI1).
3. **Embed it in a shell:** validate the **egui-hybrid** path (a `glyphon` wgpu
   paint-callback inside an egui region) — and, if it's fighting us (incl. on the
   physical-key point above), compare an **iced**/**floem** host. Decide **GUI1**.

Print is unaffected (typst already multilingual). Spike 1's core↔GUI↔preview
wiring is reused.

> Effort note: heavier than Spike 1 — a real text-layout/IME canvas is the crux of
> a serious multilingual editor. But it's the right place to spend the effort, and
> it de-risks the single most important UI decision.
