---
title: byte-text-engine-plan
tags: [design, text-engine, byte-oriented, encoding, editpad, supersedes-d1]
created: 2026-06-07
status: in progress — steps 1–5 DONE (encoding + byte Buffer + byte undo + byte file I/O [corpus-verified] + decode-aware/styled viewport); BE1 decided (hand-rolled byte rope); next = step 6. Scope refocused 2026-06-07 to authoring-first / GFM (see steps 7–9). Resolves xywrite-support-plan.md A0 = byte-oriented. Supersedes Stage-3 D1 (ropey) for the DOCUMENT representation.
companion_to: xywrite-support-plan.md (A0), beyond-stage2-plan.md (Stage 3 D1/D2), encoding.txt (EditPad model)
---

# Byte-oriented text engine — design

*The user chose the **EditPad model** (2026-06-07): the document **is the file's
raw bytes**; the **encoding is a switchable view** that never modifies the bytes;
unrepresentable characters show/encode as `?`. This re-architects Stage 3's text
engine (a UTF-8 `ropey` rope) onto bytes — done deliberately now, before XyWrite
reading, to avoid throwaway work and to deliver live encoding-switching from the
start.*

---

## Premise & what carries over

- **"It's all XPL"** and the **headless-testable** discipline are unchanged.
- This **supersedes Stage-3 D1** (rope/`ropey`) for *how the document stores
  text*. The performance bar is unchanged: **edit 100–300 MB without lag.**
- **The `Buffer` public API is the firewall.** The editor loop, verbs,
  multi-document model, file I/O, CMline, and undo were all built *against*
  `Buffer`'s API (insert/delete/motion/selection/line/column/text). If the
  byte engine keeps that API, the blast radius is the engine internals + a few
  semantics (positions become byte offsets; `text()` returns the decoded view) —
  **not** a rewrite of Stage 3's upper layers.

---

## The core model

- **Storage:** the document holds **raw bytes** (`Vec<u8>` to start; a
  byte-rope/piece-table for scale — see decisions). The bytes are never altered
  by viewing.
- **Encoding view:** a current `Encoding` decodes bytes → display characters for
  rendering, and encodes typed characters → bytes for editing. Switching the
  encoding re-decodes the *same bytes* instantly (no reload, no byte change).
- **Positions are byte offsets.** Cursor/selection/edits are byte indices into
  the buffer.

### The key simplification: single-byte encodings first

Almost every encoding XyScript needs near-term is **single-byte** (1 byte = 1
display char): the DOS code pages (**437/850/852/857/866/775**), Windows-125x,
all ISO-8859-*, Mac Roman, KOI8. For these, **byte offset == character offset**,
so cursor/motion/line/column logic is essentially today's logic over byte
indices — minimal new complexity, and it covers **all the XyWrite files** plus
most of EditPad's breadth.

**Multi-byte encodings (UTF-8, UTF-16, EUC) were deferred** to get the single-byte
engine solid first — they break the 1:1 byte↔char mapping (one char = several
bytes), which complicates cursor/column math. **As of the 2026-06-07 scope refocus
they are PROMOTED to step 7** (right after the byte rope), because the *primary*
use is authoring new UTF-8/GFM documents — see the Direction note under
"Incremental build steps." UTF-8 becomes the default view for new documents;
single-byte views remain for opening legacy/encoded files.

---

## How `Buffer` changes (API stable, internals + a little semantics)

- **Internals:** `Vec<char>`/`ropey` → bytes + an `Encoding`.
- **`insert(&str)`**: encode each char via the current encoding (reverse table);
  a char the encoding can't represent inserts `?` (EditPad behavior). Insert the
  resulting bytes at the cursor.
- **`delete_*` / motion / selection**: operate on byte offsets. For single-byte
  encodings these are identical in spirit to today (1 unit = 1 byte = 1 char).
- **`text()` / `selection_text()` / `line_text()`**: **decode** the relevant
  bytes through the current encoding to a `String` for display.
- **lines**: split the byte stream on `0x0A` (handle `0x0D 0x0A`), exactly as the
  current `line_end` logic but over bytes.
- **NEW `set_encoding(enc)`**: change the view; re-derive display; bytes
  untouched. (The live-switch feature.)
- `render()` and the viewport decode bytes→chars per the encoding.

Because positions stay 1:1 for single-byte encodings, the **Document undo
(inverse-op log)**, the **viewport math**, the **editor loop**, and the **verbs**
need only minor adaptation (they already speak in `Buffer` offsets + `&str`).
Undo records **byte** ops (position, removed bytes, inserted bytes) — the
byte-faithful truth.

---

## File I/O (replaces `read_to_string`)

- **Open:** read **raw bytes** (`std::fs::read`), keep them; pick an encoding
  (detection/override — see decisions). No UTF-8 requirement → the whole legacy
  corpus opens.
- **Save:** write the **bytes verbatim** (byte-exact round-trip is the hard
  rule). Edits already produced bytes via the current encoding, so save is just
  `write(bytes)` — no re-encode pass, nothing to corrupt.
- This makes **byte-exact round-trip trivially true** for untouched content (we
  literally write back the bytes), which is the safety property the real diaries
  demand.

---

## Encoding subsystem

- **Single-byte codepage = a 256-entry table** (byte → Unicode scalar) plus a
  reverse map (scalar → byte, or "unrepresentable"). DOS/OEM pages map all 256
  bytes, so **decoding never needs `?`**; `?` only happens when *encoding* a typed
  char the codepage lacks.
- **Tables source (decision ET1):** `oem_cp`/`codepage` crates cover the DOS/OEM
  set; `encoding_rs` covers WHATWG (Windows-125x/ISO-8859 — but **not** OEM).
  Hand-rolled 256-tables are trivial and dependency-free. **Lean:** hand-rolled
  tables for the DOS set (small, exact, no surprises) + add a crate when we want
  the long tail; revisit if the table count grows large.
- **Detection/override (decision ET2):** BOM for UTF-* (later); otherwise a
  per-document encoding the user sets (an `ENCODING <name>` verb / CMline), with
  a sensible default (e.g. CP437 for `®…¯` files, else the OS code page). Manual
  selection is the EditPad norm.

---

## Load-bearing sub-decisions

| # | Decision | Lean |
|---|---|---|
| **BE1** | Byte storage structure for 300 MB | ✅ **DECIDED (2026-06-07): rope family, hand-roll a byte rope.** Re-spike (`examples/byte_storage_spike.rs`, 256 MB) confirmed both candidates crush `Vec<u8>` (edits/nav ms → µs): chunked/rope 23–27 µs/edit, piece-table 51–254 µs (its insert cost is the flat-`Vec<Piece>` artifact; a tree fixes it), nav ~8–10 µs both; piece-table best on memory (256 vs 416 MB). Chose **rope** because undo already lives at the Document layer (piece-table's undo elegance is moot for us) and rope is the more mature path to multi-byte. The chunked buffer in the spike is the seed; grow to a tree if find-latency ever needs it. |
| **ET1** | Encoding tables: crate vs hand-rolled | Hand-rolled 256-tables for the DOS set first; crate for the long tail later. |
| **ET2** | Encoding detection & per-doc override | Manual selection + sensible default now; BOM/auto-detect later. |
| **BE2** | Multi-byte encodings (UTF-8/16) | Deferred; design the `Encoding` trait so byte↔char is N:1-capable from day one (so adding them later doesn't reshape the API). |

---

## What gets reused vs. replaced (migration map)

- **Reused (mostly untouched):** `editor.rs` (the loop), `host.rs` (verbs,
  multi-doc, CMline, status), the verb set, file-verb wiring — they speak the
  `Buffer`/`Document` API.
- **Replaced/reworked:** `text.rs` (rope → bytes + encoding), `document.rs` undo
  (char-ops → byte-ops), `viewport.rs` (decode-aware rendering), file read (bytes
  not `read_to_string`), and `ropey` is dropped (or retained only if the byte
  structure reuses it — it can't, being UTF-8). `crossterm` stays.

---

## Incremental build steps (each headless-tested green)

> **Direction (2026-06-07; see memory `xyscript-product-scope`).** The PRIMARY use
> is **authoring new documents** in a XyWrite-like operating environment, with
> **GitHub Flavored Markdown** as the formatting model (screen preview +
> print-to-PDF) — a text editor with a *modicum* of formatting, **not** a word
> processor. Editing legacy XyWrite files is **secondary** (open + byte-exact
> preserve + encoding views; **no** code-rendering engine). This promoted
> **UTF-8/multi-byte** to step 7 and reshaped the tail of this list (steps 7–9):
> byte rope → UTF-8 → GFM track, with legacy support held at read/preserve only.

1. ✅ **DONE (2026-06-07)** — **`Encoding` abstraction + DOS single-byte tables**
   (`xyscript-app/src/encoding.rs`): `Encoding` trait (slice-based
   `decode`/`encode`, so N:1-capable per BE2) + `SingleByteEncoding` (256-entry
   table + reverse map) + `cp437()` (hand-rolled full table per ET1) + `latin1()`
   + `by_name()` alias lookup (for the future `ENCODING` verb). 5 headless tests
   green: all-256-byte round-trip, `?` on unrepresentable encode, Latin-1
   identity, alias resolution. **Validation:** in CP437 `0xAE`/`0xAF` decode to
   `«`/`»`, so XyWrite's function-code delimiters are guillemets (`«FC»`,
   `«MDBO»`) — confirms CP437 is the right view (Latin-1 mis-rendered them as
   `®`/`¯`).
2. ✅ **DONE (2026-06-07)** — **Byte `Buffer` over `Vec<u8>` + a single-byte
   encoding** (`text.rs` rewritten off `ropey`): `bytes: Vec<u8>` + `enc:
   SingleByteEncoding` (default **Latin-1**) + byte-offset `cursor`/`anchor`.
   **Public API unchanged** → `editor.rs`/`host.rs`/`viewport.rs` untouched, and
   `document.rs` undo works as-is (its `String` ops + `.chars().count()` equal
   byte counts under single-byte, and `encode(decode(b))==b` keeps it byte-exact).
   New: `from_bytes(bytes, enc)`, `bytes()` accessor. Line geometry reimplemented
   over bytes (`byte_to_line`/`line_to_byte`/`line_start`/`line_end` with CRLF
   trim); `render()` uses decoded-char index == byte offset. Behavioral change:
   a typed char the view can't represent inserts `?` (deferred-multibyte
   tradeoff). Two UTF-8-specific tests ported to byte semantics; +2 new tests
   (`?`-on-unrepresentable, CP437 `from_bytes`). Full workspace green (app lib
   67 tests). `set_encoding`/live-switch is step 3.
3. ✅ **DONE (2026-06-07)** — **Byte undo** + **`set_encoding`** live switch.
   `document.rs` `Op` now stores `removed`/`inserted` as `Vec<u8>` (byte-faithful;
   ready for multi-byte); `insert` derives inserted bytes from `slice_bytes(at,
   cursor_after)`; undo/redo use `Buffer::replace_range_bytes` (verbatim splice,
   no re-encode); coalesce/merge use `.len()`/`extend_from_slice`. New `Buffer`
   primitives: `slice_bytes`, `replace_range_bytes` (replaced the old str-based
   `replace_range`), `set_encoding`, `encoding_name`. `set_encoding` (Buffer +
   Document) is a live re-decode — bytes/cursor untouched, **not an edit / not
   undoable**. +2 tests (in-place reinterpret Latin-1→CP437; switch-is-not-an-edit).
   Workspace green (app lib 69).
4. ✅ **DONE (2026-06-07)** — **File I/O on bytes** + per-doc encoding +
   `ENCODING` verb, **corpus-verified**. `Document::load(bytes, path)` stores bytes
   **verbatim** (no LF normalization) and picks a provisional view via
   `default_encoding_for` (0xAE/0xAF → CP437, else Latin-1; ET2 full detection
   later); `save_bytes()` returns the bytes untouched (re-expansion dropped).
   `AppHost::read_doc_bytes`/`write_doc_bytes` use `std::fs::read`/atomic write
   (the String `files` overlay stays for the ASCII test fixtures; `read_file`/
   `write_file` remain UTF-8 for kbd/lib). New `EditorOp::SetEncoding` +
   `ENCODING <name>` verb (live reinterpret; unknown name trappable) + `$EN`
   special + encoding shown in the open-status message. **Behavioral change:**
   `text()`/`$TX` now include `\r` for CRLF files (the byte truth); the viewport
   still trims it per line. Verified via `examples/corpus_roundtrip.rs` over the
   real `FandH` corpus: **6303/6303 files byte-exact, 0 read errors, 0
   mismatches** (6131 CP437, 172 Latin-1). Workspace green (app lib 70).
5. ✅ **DONE (2026-06-07)** — **Viewport decode-aware + live switch visible +
   styled-span pipeline.** The viewport already renders via `Buffer::line_text`
   (decode-aware), so a `set_encoding` switch shows on the next redraw — now
   proven by a test (`rows_reflect_the_current_encoding_view`: same bytes →
   `«»` under CP437, `®¯` under Latin-1). New `style.rs` (`Color`/`Style`/`Span`/
   `StyledLine`) + `Viewport::styled_rows` (one plain span per line today) so
   syntax highlighting / Xy-MoDe color / GFM can attach styles later without
   reshaping the render path (the feature-note forward-compat). `main.rs` renders
   via `styled_rows` + `queue_span` (applies fg/bg/bold/italic/underline, no-op
   for plain) and the status line now shows the encoding name (e.g. `cp437`),
   making the live switch legible. Workspace green (app lib 74); zero warnings in
   touched files.
6. **Scale — byte rope** (BE1 ✅ decided: hand-roll a byte rope; seed =
   `examples/byte_storage_spike.rs`). Swap `Vec<u8>` for it **behind the stable
   `Buffer` API**, so the editor loop, verbs, undo, and viewport are untouched.
   Hold the 100–300 MB edit-without-lag bar (spike: ~µs edits/nav vs `Vec<u8>`'s
   ms). The rope stores **bytes** and is encoding-agnostic — unaffected by the
   scope refocus; it's the foundation both the legacy and authoring uses ride on.
7. **Multi-byte / UTF-8** (PRIMARY-use-critical; promoted from "later"). Make
   cursor / column / line / selection math **char-boundary aware** when the active
   view is multi-byte (single-byte views stay 1:1); add UTF-8 (and UTF-16). The
   `Encoding` trait is already N:1-capable (BE2), so no API reshape. **The default
   view for NEW documents becomes UTF-8** (single-byte views remain for opening
   legacy/encoded files). This is what makes em-dashes, smart quotes, accents, and
   emoji typeable for the authoring workflow — no more `?` on unrepresentable input.
8. **GFM formatting track** (the formatting model — a text-editor *modicum*, not a
   word processor; target **100% GitHub Flavored Markdown** compatibility):
   (a) **Markdown syntax highlighting** emitted through the step-5 styled-span
   pipeline; (b) **on-screen preview** of rendered GFM; (c) **print-to-PDF**.
9. **Legacy XyWrite support — reduced scope (formerly "Layer B")**: open, **preserve
   bytes exactly**, and offer **encoding views** (CP437/850/852/857/866/775,
   Windows-125x, ISO-8859, Mac, …). Explicitly **not** an engine that interprets or
   renders the `«…»` function codes — XyScript does not reproduce XyWrite's
   formatting machinery.

---

## Risks

- **Byte-exact round-trip** is sacred (real, irreplaceable documents). Test
  read→write→compare against the actual `FandH` corpus before trusting save.
- **Redo cost:** this redoes Stage-3 steps 1–2 on bytes. The `Buffer` API
  firewall keeps the upper layers; resist re-touching them beyond the offset/
  decode semantics.
- **Performance:** keep the 300 MB bar; don't ship the `Vec<u8>` stage as final
  (step 6 restores scale). The `byte_storage_spike` numbers set the target
  (~µs edits/nav at 256 MB).
- **Sequencing:** the byte foundation (steps 1–6) must be solid and corpus-verified
  before UTF-8 (step 7). The XyWrite `«…»` codes are **out of scope** as a render
  target (step 9 is read/preserve only) — don't chase the 1,337-code interpretation
  engine.
