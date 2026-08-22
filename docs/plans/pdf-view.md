# VIEW — in-tab PDF viewer: implementation plan

Status: **planned** (not started). Feasibility spike completed 2026-08-22 — see "Spike results" below.

## Approach in one line

A PDF tab is a **read-only "render-mode" Document** (mirroring the HEX view): the host tracks
"this tab is a PDF at path X + its bytes"; the GUI renders its pages to per-page textures via
[`pdfium-render`](https://crates.io/crates/pdfium-render), lazily and cached. `VIEW <path>` opens it;
it falls back to OS-open when pdfium is unavailable.

## Spike results (what's already proven)

A throwaway Windows spike (`pdfium-render` 0.9.3 + bblanchon `pdfium.dll`, chromium/8009) established:

- **Rendering works and is faithful** — rasterized a real `manual.pdf` page (custom fonts, italics,
  monospace) to a pixel-perfect PNG.
- **`+crt-static` is a non-issue** — built and ran under the project's exact static-CRT setting; the
  static-CRT exe drives the dynamically-linked `pdfium.dll` with no friction.
- **The exe stays self-contained** — `dumpbin` shows no `vcruntime140.dll` dependency; only system
  DLLs. `pdfium.dll` itself needs only `KERNEL32/ADVAPI32/GDI32/USER32` (no VC++ redist).
- **License** — use `pdfium-render` (MIT OR Apache-2.0). Do **not** use the `pdfium` crate
  (`pdfium-rs`, newinnovations): it is **GPL-3.0** and would force all of XyScript to GPL.

The one concession: there is **no off-the-shelf Windows static pdfium** (a static build means compiling
pdfium from Chromium source). So the Windows distribution becomes **two files** — `xyscript.exe` +
`pdfium.dll` (~7 MB), the DLL beside the exe (like the `themes/` folder). Same pattern on Linux
(`libpdfium.so`) and macOS (`libpdfium.dylib`), both provided by bblanchon.

## Cross-platform notes

The plan is **cross-platform, not Windows-only**. The split is clean: the editor work (Phases 1–5) is
100% platform-agnostic; only the packaging/loading in Phase 0 has per-OS specifics.

**Transfers unchanged:**

- `pdfium-render` is genuinely cross-platform — the same Rust/egui API on all three OSes. Phases 1–5
  (PDF-tab model, `VIEW` verb, texture rendering, scroll/zoom, session/AUTOREOPEN) have no OS-specific
  code.
- bblanchon ships the native lib for every target, **including ARM**: `pdfium.dll` (Windows),
  `libpdfium.so` (Linux x86_64 + aarch64), `libpdfium.dylib` (macOS x86_64 + aarch64). `release.yml`
  already builds Linux / macOS-universal / Windows, so each job just fetches the matching lib.
- Load by **explicit path** — `Pdfium::pdfium_platform_library_name_at_path(exe_dir)` does a
  `dlopen`/`LoadLibrary` of the full path. This is what makes "lib beside the exe" work uniformly: it
  sidesteps Linux `rpath`/`LD_LIBRARY_PATH` and macOS `@executable_path` entirely.

**Per-OS wrinkles (Phase 0 only):**

- **macOS-universal is the fiddliest.** The mac build is a universal (fat) binary, so either `lipo` the
  two per-arch dylibs into one fat `libpdfium.dylib`, or ship arch-matched. The one bit of genuinely new
  work beyond Windows.
- **macOS distribution:** a bundled third-party dylib means Gatekeeper/notarization must cover it —
  standard mac-release hygiene, but a step. In a `.app` bundle, place the dylib in the bundle and point
  the explicit-path load at that directory.
- **Linux:** `libpdfium.so` depends on system glibc; bblanchon builds against an older glibc floor
  (broad compatibility), but note a minimum-glibc for the release.

**Honesty caveat:** the feasibility spike was **Windows-only**, and its headline finding — the
`+crt-static` static-CRT exe coexisting with dynamic pdfium — is Windows-specific. That concern does not
exist on Linux/macOS (they link the system libc dynamically as usual; loading a shared pdfium is
standard), so there is no equivalent risk to retire — just a quick per-OS load-smoke before a release.
Windows is *proven*; Linux/macOS are *confident-by-construction* (that is pdfium-render's and bblanchon's
whole purpose) but not literally spiked.

## Architecture decision

Everything in the app is a `Document` in `Vec<Document>` (tabs, `DocId`, panes, and session all assume
this). The clean fit — proven by the HEX view — is a **canvas render-mode flag**, not a new tab type:

- Add `pdf: Option<PdfView>` to `Document` (or a `DocKind`), where
  `PdfView { path, bytes, page_count, zoom, … }`. The rope stays **empty** (no binary in the text
  engine); the tab is read-only, excluded from editing / spell-check / autosave.
- The GUI's canvas render path gets a `pdf` branch — exactly like the existing `hex_view()` branch →
  `render_hex_canvas` — that draws stacked page images instead of text.
- This reuses tabs, switching, close/`AB`, the pane system, `DupView`, and session for free.

**pdfium lifetime wrinkle:** `pdfium-render`'s `PdfDocument` borrows `&Pdfium`, so you can't store both
together without self-referential-lifetime pain. Sidestep it: keep one `Pdfium` in the GUI; store only
the **raw PDF bytes** per tab; each render pass does `pdfium.load_pdf_from_byte_slice(&bytes, None)`,
renders the visible pages, caches the resulting **textures** (keyed by `DocId + page + render-width`),
and drops the `PdfDocument`. Loading is lazy/cheap; the textures are the durable cache.

## Phases

### Phase 0 — Dependency, feature gate, lib loading, packaging (~0.5–1 day)

- Add `pdfium-render` to `xyscript-app` behind a new **`pdf`** cargo feature (peer of `lint`/`email`).
  Dist build becomes `--features lint,email,pdf`.
- Runtime bind: `Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(exe_dir))`, where
  `exe_dir = current_exe().parent()`. Lazy-init once; store `Option<Pdfium>` in the GUI.
- **Graceful degradation:** if the `pdf` feature is off, or `pdfium.dll` isn't found at runtime, `VIEW`
  falls back to `open_in_os` (tier 1) + a status note. A build/deploy missing the lib never hard-fails.
- Packaging: drop `pdfium.dll` into `dist/` and `X:\xys` (like `themes/`). In `release.yml`, fetch the
  matching bblanchon lib per target and include it in each release archive. **Pin the pdfium binary
  version to the `pdfium-render` API level.** Don't vendor the 7 MB binary in git — fetch in CI /
  document for local dev.

### Phase 1 — Host PDF-tab model (~0.5–1 day)

- `PdfView` state on `Document` + accessors (`is_pdf()`, `pdf_path()`, `pdf_bytes()`, `pdf_zoom()`,
  page count).
- A constructor that makes a read-only PDF Document: label = filename, empty rope, `read_only`,
  `styling_dirty=false`.
- Exclude PDF tabs from: editing ops (already read-only), spell/grammar, MDA, find/emph, autosave,
  `is_modified`.

### Phase 2 — the `VIEW` verb (~0.5 day)

- `EditorOp::OpenPdf { path }`; verb `VIEW <path>` (resolve against `$CWD`, like OPEN). Validate: file
  exists and starts with `%PDF`; else trappable error. Missing pdfium/feature → fall back to
  `open_in_os`.
- Add to `BUILTIN_VERBS`, `FOCUS_DOC_VERBS`, reference/manual.
- **Optional auto-route:** `OPEN foo.pdf` → route to `VIEW` (mirrors `DOCTYPE` auto-routing for
  `.html`). Nice, but can be a v1.1 toggle so it never surprises someone who wants the raw bytes.

### Phase 3 — GUI rendering (the meat, ~1.5–2 days)

- New canvas branch: when the active (or paned) doc `is_pdf()`, render pages instead of text.
- **Layout:** pages stacked vertically, centered, with gaps; total height = Σ page heights at current
  zoom. Reuse the existing vertical scroll (wheel + scrollbar) — the scroll offset maps to page +
  intra-page offset.
- **Lazy render + cache:** find the pages intersecting the viewport; for each uncached
  `(DocId, page, render_width)`, render via pdfium → `egui::TextureHandle`; LRU-evict far pages (cap the
  cache, ~a dozen textures). Re-render on width/zoom change (the same trigger the preview pane uses).
- **Threading:** v1 renders visible pages **synchronously on the main thread** with caching (a page is
  ~tens of ms; only uncached pages on scroll cost anything). If scroll jank appears, move to a dedicated
  pdfium thread + channel (pdfium isn't thread-safe — one instance, one thread, or the `thread_safe`
  mutex feature). This is the known perf lever.

### Phase 4 — Zoom & navigation (~0.5 day)

- Zoom levels (fit-width default + explicit steps); zoom changes render-width → cache invalidates. A
  couple of keys/verbs (`VIEWZOOM +/-/fit`, or reuse a generic zoom). Page-up/down + Home/End jump
  pages. **Defer** text-selection, search, and link-clicking.

### Phase 5 — Integration niceties (~0.5 day)

- **Session restore:** reopen PDF tabs by path (read-only, so no shadow-backup needed).
- **AUTOREOPEN synergy:** a PDF tab watches its file like any other; when `TOPDF` re-exports the PDF
  you're viewing, the view refreshes automatically — a live "final PDF" preview loop. The
  `reload_doc_from_disk` path needs a PDF branch that reloads bytes + clears the texture cache.
- `$` reads if wanted: page count / current page (minor; skippable for v1).

### Phase 6 — Docs, tests, packaging, deploy (~1 day)

- Reference `#cmd("VIEW")` (topical + alphabetical) + manual section (Files chapter, near OPEN/READ).
  Regenerate PDFs; rebuild exe (manual baked in).
- Tests (host-side, headless): `VIEW` on a valid PDF creates a read-only PDF tab flagged correctly;
  error on a missing file / non-PDF; the fallback path when the feature is off. Rendering itself is GUI +
  native lib — not headless-testable; covered by the spike + a live smoke test.
- Build `--features lint,email,pdf`; sync `pdfium.dll` to `dist/` + `X:\xys`; deploy.

## Risks & decisions to confirm

- **Two-file distribution** (`xyscript.exe` + `pdfium.dll`, +7 MB) — the accepted concession.
- **Feature-gate default:** ship `pdf` in the dist (`lint,email,pdf`)? Recommended yes, with the OS-open
  fallback as the safety net.
- **v1 scope discipline:** render + scroll + zoom + page nav, read-only. No select/search/links/print —
  punt those explicitly (scope-creep traps).
- **Version coupling:** `pdfium-render` ↔ pdfium binary API level must stay in sync; pin both.

## Rough effort

**~5–6 focused days** end to end. The uncertain part (does the engine work under this build?) is retired
by the spike; what's left is bounded editor work plus a well-understood packaging change. The highest-
value sub-step is Phase 3's texture-cache-on-scroll — that's where "feels like a real viewer" vs. "janky"
is decided.

## Build order

Start with **Phase 0 as its own commit** (dep + feature + lib-loading + OS-open fallback) so the plumbing
is proven before any UI work; then Phase 1–2 (host model + verb) as a second commit; then Phase 3 (the
renderer). Each phase is independently shippable behind the `pdf` feature.
