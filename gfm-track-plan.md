---
title: gfm-track-plan
tags: [design, gfm, markdown, formatting, preview, pdf, step-8]
created: 2026-06-08
status: in progress — 8a DONE (highlighter seam + Markdown via comrak; off by default; HIGHLIGHT verb). DECIDED: G1 comrak, G2 native/GUI-targeted, G3 **typst** (spike done), G4 highlight off by default, PDF-first sequencing (GUI separate). G7 superseded by G3 (typst markup is the IR; typst the only renderer). 8b DONE (GFM→typst markup, `gfm.rs`). 8c DONE (typst→PDF, `pdf.rs`, `TOPDF` verb). **GFM core 8a+8b+8c COMPLETE — prints GFM to PDF.** NEXT: 8d (GUI preview via typst→SVG/raster) is a separate later track needing a GUI shell; or pick up another track (links/Xyperlinks, language highlighting SH1). OPEN: G5, G6, SH1 (future). Expands byte-text-engine-plan.md step 8. Companion to xyscript-product-scope.
companion_to: byte-text-engine-plan.md (step 8), xyscript-product-scope.md, xyscript-feature-notes.md
---

# Step 8 — the GFM formatting track (plan)

*The product scope (2026-06-07) makes **authoring new documents** the primary use,
with **GitHub Flavored Markdown** as the formatting model: a text editor with a
**modicum** of formatting/printing — screen **preview** and **print-to-PDF** —
without sliding into word-processor territory. Step 8 builds that, riding the
styled-span pipeline (step 5) and the UTF-8 byte engine (steps 1–7).*

---

## Principles (the guardrails)

- **The editing surface stays plain Markdown text.** We are **not** building a
  WYSIWYG/word processor. You type Markdown; the editor adds **syntax
  highlighting** over the source. Preview and PDF are **read-only outputs**
  derived from the source — a separate view, never an editing mode.
- **100% GFM is the bar for rendered outputs** (preview + PDF). Source
  highlighting can be best-effort (a visual aid), but what you preview and print
  must match GFM faithfully.
- **It's all XPL.** Preview/print/highlight are reached through **verbs**
  (`PREVIEW`, `PRINT`/`TOPDF`, maybe `HIGHLIGHT`), consistent with
  OPEN/SAVE/ENCODING. No new modal UI beyond a read-only preview surface.
- **Headless-testable cores.** Each sub-step has a pure function at its heart:
  source → highlight spans; source → HTML; (PDF is the one integration boundary).
- **Minimal-deps ethos meets reality.** We hand-rolled the rope and encodings for
  control, but a 100%-GFM parser and a PDF renderer are *not* things to hand-roll
  (like `crossterm`, they're infrastructure). Step 8 introduces XyScript's first
  heavy external crates; choosing them well is the main work of this plan.

---

## Resolved direction (2026-06-08)

**Own the rendering — native, no HTML/browser pivot.** XyScript builds its **own**
GFM renderer that feeds both an on-screen preview and a **typeset PDF**. The
**end-product UI is a GUI**, so the terminal's inability to show 100% GFM is a
*transitional* limit, **not** a design constraint — the native renderer targets
**GUI-grade fidelity** (real fonts, images, layout), and the same renderer
typesets the PDF, so **what you preview is what you print**.

Consequence — **a display-agnostic render core, with PDF as its first consumer:**

```
GFM source ──parse──▶ AST ──layout──▶ Render model ──┬─▶ PDF painter      (now: no GUI needed)
                          (geometry + style,         ├─▶ GUI painter      (later: with the GUI shell)
                           display-agnostic)         └─▶ (terminal-approx) (optional stop-gap)
```

- The **render model** (laid-out blocks/inlines with geometry + style) is the
  shared, **headless-testable** core — independent of the output surface.
- **PDF is the first painter**: it needs **no GUI**, so "see rendered GFM" ships
  early (you open the PDF), and it exercises the render model end-to-end.
- The **GUI preview** is a second painter, deferred until the **GUI shell** exists
  (a separate workstream — see Sequencing). The provisional terminal remains the
  editing shell meanwhile.
- This mirrors the step-5 styled-span philosophy at 2-D scale: one model, many
  painters.

> **Display target evolution:** the earlier "stay provisional TUI" stance was for
> the byte-engine work; the **end-product UI is now confirmed to be a GUI**. The
> terminal shell is transitional. (Recorded in `xyscript-product-scope`.)

---

## Sub-steps (each delivers something testable)

### 8a — ✅ DONE (2026-06-08) — Syntax-highlighting **seam** (Markdown = first highlighter)
**Shipped:** `highlight.rs` — general `TokenKind` + `theme(kind)→Style` +
`Highlighter` enum (`Markdown`, via **comrak** sourcepos → exact byte ranges) +
pure `styled_line`/`clip_styled` span helpers. `Buffer::line_byte_range` added;
`Viewport::styled_rows(buffer, tokens, h, w)` (empty tokens = unchanged plain fast
path). `Document` owns the highlighter + a token cache, **off by default**, chosen
by file type, refreshed after every text mutation, on/off preference preserved
across opens. `EditorOp::SetHighlight` + **`HIGHLIGHT on|off`** verb + status
shows `hl:markdown`. comrak added with **default-features off** (CLI+syntect pull
deps needing rustc 1.88; we're on 1.86 and only need the parser). Tests: 6
highlight (offset-exact `# Title`/`**bold**`/`` `code` ``/`*em*`/links/fence/rule
+ multibyte runs + clip) + viewport integration + 2 editor verb-path; **app lib
94 green**, no new warnings. *(Original sub-step text preserved below.)*

#### (original 8a notes)
Syntax-highlighting **seam** (Markdown = first highlighter)  *(terminal-native; no render runtime)*
Highlighting is not a Markdown-only feature: it must extend to **many programming
languages via user-droppable add-ons** (see "Beyond step 8"). So 8a builds the
**abstraction**, with Markdown as the first implementation.
- **The seam (the durable part):** a `Highlighter` interface
  `source → Vec<(byte-range, TokenKind)>`, a **general `TokenKind` set** (keyword,
  string, comment, number, heading, emphasis, code, link, list-marker, … — shared
  by Markdown *and* future languages), and a **theme** `TokenKind → Style` (also
  the future Xy-MoDe-color hook). The viewport maps the active highlighter's
  token-ranges onto the visible lines' `Span`s.
- **First impl — Markdown:** rides **comrak** (we parse it for rendering anyway,
  and Markdown is hard to regex well). Pure, tested: `source → token-ranges`.
- **Selection — by file-type "profile", default NONE:** each buffer has an
  optional active highlighter chosen by file type; **off by default** (`HIGHLIGHT
  on|off`) — the quiet screen. When off, the viewport emits plain spans exactly as
  today (zero cost). "Profile" is the forward-looking unit (see below); for now it
  carries only the highlighter.
- **Wiring:** `Viewport::styled_rows` (already `Vec<StyledLine>`, one plain
  span/line today) consults the active highlighter for visible lines; clip to the
  horizontal window in span space.
- **Risk:** parser offsets ↔ rope byte offsets across horizontal scroll;
  performance (highlight only visible lines; cache per-line).
- **No renderer, no PDF, no GUI — lowest-risk first sub-step.**

> **Architecture revised by the G3 spike (2026-06-08): typst is the renderer.**
> typst is a typesetting *engine* — feed it **markup**, it owns layout, fonts,
> justification, pagination, Unicode (even code-block syntax highlighting). So the
> hand-built "render model with geometry" and the `Painter` abstraction (old G7)
> **collapse**: the intermediate representation is **typst markup** (a string),
> and **typst is the single renderer for every surface** — PDF now, SVG/raster for
> the GUI preview later. This removes the chief risk (we no longer write a
> text-layout/font engine) and gives one engine for *both* print and preview,
> in-process, no browser. The G2 spirit holds (we own the pipeline, no HTML pivot);
> the "renderer" is the embedded typst library.

### 8b — ✅ DONE (2026-06-08) — GFM → typst markup
**Shipped:** `xyscript-app/src/gfm.rs` — `to_typst(source) -> String` walks the
comrak AST → typst **body** markup (function-form: `#emph/#strong/#strike/#raw/
#list/#enum/#quote/#table/#link/#line`). `escape_text` neutralizes the markup set
(critically `#`/`$` → no typst code/math injection); `typst_str` builds string
literals for URLs/code. Covers headings, emphasis/strong/strike, inline+fenced
code (lang preserved), bullet/ordered lists, blockquotes, tables (bold header),
rules, links (targets kept), images (→ link), raw HTML (→ escaped text). 9 golden
unit tests. **Validated end-to-end:** `examples/gfm_dump.rs` → a deliberately
tricky doc → compiled via `pdf-spike` (`run_file`) → correct PDF (all metachars
escaped, code block syntax-highlighted). Known first-cut gaps: task-list
checkboxes shown as plain items; links not yet visually styled (a `#show link:`
in 8c's preamble / TH1). App lib 103 green. *(Plan text preserved below.)*

#### (original 8b notes)
- **Parse** GFM → comrak AST (**G1**), then **transform AST → typst markup**:
  headings, emphasis/strong, inline code & fenced code blocks (typst
  syntax-highlights these for free), lists, blockquotes, tables, rules, links,
  images, task items.
- **Escaping** is the real work: typst has its own metacharacters (`* _ # @ $ = -
  + / < > \`` …`) — content must be escaped into typst markup (analogous to
  HTML-escaping). Plus a small typst preamble (`#set page/text`, fonts).
- **Links carry targets natively** — GFM `[t](url)` → typst `#link("url")[t]`
  (clickable in PDF; preserved for the GUI + the live-links/Xyperlinks track,
  `links-and-xyperlinks-plan.md`). 8a's `Token` should still grow an optional
  target for the *editor* view.
- **Tested:** golden tests on `GFM → markup`; validate by compiling representative
  docs with typst (no panics, expected page count). Parse correctness leans on
  comrak's own GFM-spec conformance.

### 8c — ✅ DONE (2026-06-08) — typst → PDF + `TOPDF`
**Shipped:** `xyscript-app/src/pdf.rs` — `gfm_to_pdf(source) -> Result<Vec<u8>,
String>`: prepend a preamble (`#set page(margin:2cm)` · `#set par(justify:true)` ·
`#show link: set text(fill: blue)`) to the 8b body, compile via embedded **typst**
(`TypstEngine::builder().main_file(String).fonts(typst_assets::fonts()).build()
.compile().output`) → `typst_pdf::pdf`. Fonts: **bundled typst-assets defaults**
(portable, license-clean — no system fonts). New `EditorOp::ToPdf(Option<String>)`
+ **`TOPDF`/`PRINT [path]`** verb (path derived from doc name if omitted; trappable
on compile error). Deps added to the app: `typst`/`typst-as-lib`/`typst-pdf`
(0.14/0.15) + `typst-assets` (fonts). 4 tests (valid `%PDF`/`%%EOF`, tricky-
metachar compile, empty doc, `TOPDF` verb explicit+derived path) — these compile
real typst, so they also **validate the 8b transform end-to-end**. Manual check
`examples/gfm_pdf.rs` → 38 KB PDF: justified serif body, blue links, syntax-
highlighted code, gridded table. App lib 107 green; clippy clean. **The GFM core
(8a+8b+8c) is complete — XyScript prints GFM to PDF.** *(Plan text below.)*

#### (original 8c notes)
- Compile the markup with the embedded **typst** engine → PDF bytes → write.
  (`pdf-spike/src/typst_spike.rs` is the working seed: `TypstEngine::builder()
  .main_file(markup).fonts(…).build().compile()` → `typst_pdf::pdf`.)
- The **near-term way to *see* rendered GFM** — open the PDF — proving the whole
  pipeline before any GUI exists. Fonts: bundle/locate a small set (the spike used
  Arial + Consolas); typst **subsets** them (the spike's PDF was 68 KB).
- **Verb:** `TOPDF <path>` (alias `PRINT <path>`), trappable like SAVE.

### 8d — GUI preview via typst → SVG/raster  *(rides the GUI shell — see Sequencing)*
- Render the **same typst markup** to **SVG/raster** (`typst-svg` / `typst-render`)
  and show it in the GUI: a **paginated print-preview** (what you preview *is* what
  you print). `PREVIEW` verb; live-refresh on edit (debounced; cadence = **G5**).
- **Depends on a GUI shell** — a separate, larger workstream.

### 8e — polish *(optional)*
Editor highlight theme tied to the eventual Xy-MoDe colors; preview/print theming
(typst preamble); image handling; per-file-type defaults.

---

## Load-bearing decisions (OPEN — to decide one at a time)

| # | Decision | Options / status |
|---|---|---|
| **G2** | Rendering strategy | ✅ **DECIDED (2026-06-08): native, GUI-targeted.** Own renderer (no HTML/browser pivot); GUI is the end-state so no terminal-fidelity compromise; same renderer typesets the PDF. |
| **G4** | Highlight default | ✅ **DECIDED (2026-06-08): OFF by default** — a quiet screen is the norm; `HIGHLIGHT on` is opt-in. (Token set + `TokenKind→Style` map, and the relation to future Xy-MoDe colors, still to detail.) |
| **G1** | Markdown parse engine | ✅ **DECIDED (2026-06-07): `comrak`** (in use in 8a). Pure-Rust cmark-gfm port; full GFM; typed AST + sourcepos — one engine for 8a highlighting (sourcepos→byte ranges) and the 8b GFM→typst transform (AST walk). Added `default-features=false` (parser only). |
| **G3** | PDF backend (typeset) | ✅ **DECIDED (2026-06-08): `typst`.** Spike (`pdf-spike/`) verdict vs `genpdf`: typst gave far better output (justification, real bullets, **free code syntax-highlighting**) at **68 KB** vs genpdf's **6.15 MB** (genpdf/printpdf-0.3 embeds full TTFs; no subsetting), and — decisively — typst is an *engine* that also renders SVG/raster, so **one engine serves both PDF and the GUI preview**, removing the need to hand-build a layout engine. Costs accepted: heavy 277-crate tree, ~48 s clean build, typst version churn (`typst-as-lib` wrapper), GFM→typst escaping work. Pure-Rust, no external runtime. |
| **G5** | Preview cadence (8d) | **Live** (debounced re-render) vs **on-demand** (`PREVIEW`). *Lean:* on-demand first, live later. Only relevant once the GUI preview (8d) is in play. |
| **G6** | GFM scope vs Xy extensions | Pure GFM now; the feature-notes "colorful text via Xy MoDe (hex RGB)" is a **later** XyScript extension — out of scope for step 8. |
| **G7** | Render-model / painter API | ⛔ **SUPERSEDED by G3.** typst is the renderer for all surfaces, so there's no hand-built render model or `Painter` trait — the IR is **typst markup** and the single renderer is the embedded typst library (PDF now; SVG/raster for the GUI later). |
| **SH1** | Language-grammar engine (future) | How user add-on grammars are expressed: **(a)** adopt an existing **plain-text** format — Sublime/TextMate via `syntect` (large public ecosystem) or a nano-style regex format; **(b)** **invent** a small XPL-flavored regex grammar DSL (on-brand, user-rollable, but reinvents). Hard constraint from the user: **plain-text, users can roll their own** ⇒ *compiled* grammars (tree-sitter) are out. Decide when the language-highlighting subsystem is scheduled (after step 8). 8a's seam is designed so either choice slots in. |

---

## Beyond step 8 — extensible highlighting & editing aids (bank now, build later)

User direction (2026-06-08), recorded so current design doesn't foreclose it:

- **Programming-language syntax highlighting**, extensible via **plain-text
  add-ons** so users can roll their own (prefer an existing public-domain format
  over reinventing; see **SH1**). 8a's `Highlighter` seam + general `TokenKind`/
  theme is built to host these — a language highlighter is "a new grammar behind
  the same seam," no viewport rework.
- **Programmer's editing aids:** auto-brackets, auto soft-wrap indents, smart tabs,
  etc. These are **input/editor-layer** features (verbs / keymap / settings — "it's
  all XPL"), orthogonal to the byte engine and the renderer. Nothing to pre-build;
  nothing they foreclose.
- **Unifying abstraction — the file-type "profile":** per language, bundle
  *highlighting grammar + bracket pairs + indent rules + comment tokens*.
  Highlighting (8a) and the editing aids later all key off the same profile. 8a
  selects "the profile for this file type" (only the highlighting field populated
  at first), so the profile concept grows coherently.
- **Live links & Xyperlinks** (highlight URLs + make them live; XyWrite-style
  cursor+key-runs-embedded-XPL) — a related later track planned separately in
  **`links-and-xyperlinks-plan.md`**. Converges on an "actionable span" + a
  `FOLLOW` verb. Security DECIDED (XL2): URL + named-routine first; inline XPL
  gated by trust/confirm/sandbox, never auto-executed. Only forward-compat hook
  for the current work: **link tokens / render model carry a target** (folded into
  8b above).
- **No impact on current decisions** beyond making 8a a general seam: the byte
  engine, G1 (comrak for Markdown), and the typst path are unaffected.

---

## Dependencies & risks

- **Heavy external crates** (`comrak` for parse; **`typst`** for typeset — 277
  crates) — a real shift from the minimal-deps posture, but justified: a 100%-GFM
  parser and a typesetting engine are infrastructure, not things to hand-roll, and
  typst's weight *buys away* the layout-engine build. **No browser/external
  runtime** — pure-Rust, in-process. Pin & audit; watch typst/`typst-as-lib` churn.
- **typst replaces the layout build** — the chief former risk (hand-rolling text
  layout + font metrics) is gone; typst owns it. New, smaller risks: the GFM→typst
  **escaping/transform** correctness, font bundling/licensing, and tracking typst
  version bumps.
- **The GUI shell is a prerequisite for the preview (8d)** and a separate, large
  workstream. PDF-first sequencing avoids blocking the rest of step 8 on it.
- **Testability:** the `GFM → typst markup` transform is pure/golden-testable;
  parse correctness leans on comrak's GFM conformance; PDF output is integration
  (compiles without panic, valid header, non-empty, opens).
- **Performance:** highlight only visible lines (8a); don't re-typeset on every
  keystroke (debounce the 8d preview).

---

## Sequencing

The GUI is the end-state but a big separate workstream; this ordering delivers
value **without blocking on it**:

1. ✅ **8a — highlighting** (DONE). Self-contained; opt-in, off by default.
2. ✅ **G1 = comrak, G3 = typst** (DONE — spike in `pdf-spike/`).
3. **8b — GFM → typst markup** (the single IR; pure, golden-tested).
4. **8c — typst → PDF + `TOPDF`** — the first way to *see* rendered GFM (open the
   PDF); proves the whole pipeline with no GUI dependency. (Seed:
   `pdf-spike/src/typst_spike.rs`.)
5. **GUI shell** (separate track, when you're ready) → **8d — GUI preview** via
   typst → SVG/raster of the *same* markup → 8e polish.

✅ **DECIDED (2026-06-08): PDF-first.** After 8a we do 8b → 8c (rendered output
with no GUI dependency); the **GUI shell + preview (8d) is a clean separate later
track**. Rationale: a tangible "see your GFM rendered" win quickly, full exercise
of the typst pipeline, GUI kept as its own well-scoped effort.
