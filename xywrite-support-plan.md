---
title: xywrite-support-plan
tags: [plan, xywrite, file-format, encoding, function-codes, design]
created: 2026-06-07
status: design plan — no code yet. Corpus surveyed (6,303 files). Central OPEN decision = A0 (document model: UTF-8 rope vs byte-oriented/EditPad). Domain questions OPEN (need the user/XyWrite expert).
companion_to: beyond-stage2-plan.md (Stage 3 editor + the deferred "encoding" item), development-plan.md (Foundational premise)
---

# XyScript — XyWrite file support: design plan

*Planned deliberately, before building, like Stage 3. XyScript is a XyWrite
successor and the user has real native XyWrite data files (e.g. `x:\wk26\days.26`,
~88 KB) they want to open — so reading/round-tripping XyWrite files is a genuine
project requirement, not a side quest. This document captures what the format
**empirically is**, splits the work into layers, surfaces the load-bearing
decisions, and lists the **domain questions only the user can answer** before the
function-code layer can be designed.*

---

## Foundational premise — still governing

- **"One syntax — it's all XPL."** Whatever editing/authoring of XyWrite codes we
  expose stays expressible as XPL; no parallel command language.
- **QN is the feature floor, fresh Rust slate.** QN (`C:\Users\cldis\QN2`) is
  "XyWrite-*style*" in its UX and supports a set of **text encodings** (its
  `convert_text_encoding` lists CP437 (DOS US), CP850, Windows-125x, Mac Roman,
  UTF-*). That encoding set is the **floor for the encoding layer**. But QN does
  **not** parse the native `®…¯` function-code format — so the **function-code
  layer is fresh design**, with the user as the domain authority.
- **The keyboard-capture model is untouched** by this work.

---

## What the format empirically is — measured across a real corpus (2026-06-07)

Scanned the user's library `C:\Users\cldis\Dropbox\Copy\FandH` (**6,303 files,
~34.5 MB, back to 1985**, legacy extensions like `.LTR`/`.AFF`/`.l1`/`.f1`/`.nts`):

- **~97% are native XyWrite** (6,127 of 6,303 contain the `0xAE` marker); ~176
  are plain text. So native-XyWrite support is the dominant need, not an edge.
- **Embedded function codes: `®CODE¯`** — begin `0xAE` (`®`), end `0xAF` (`¯`),
  ASCII code + optional args between; markers are **balanced**. A code = `®` +
  letters + optional args + `¯` (e.g. `®Sz12¯`, `®UFdutch¯`, `®DAMmmm¯`).
- **The vocabulary is huge: 1,337 distinct code families** corpus-wide. Top by
  frequency: `MDNM` (48k, normal), `IP` (41k), `LS` (31k, line spacing), `MDUL`/
  `MDBO`/`MDIT`/`MDRV` (underline/bold/italic/reverse modes), `RM`/`LM` (margins),
  `C` (center?), `FL`/`FC`/`LB`, `Sz` (point size), `TS` (tab set), `UF…`
  (fonts: `UFdutch`/`UFSWISS`/`UFSTANDA`), `PL`/`PG`/`PN` (page length/page/page
  number), `HY` (hyphenation), `JU` (justify), `DAMmmm` (date format), `LD`/`DC`,
  … — a full word-processor's worth of formatting/layout/font codes. **Exact
  semantics are the user's domain knowledge** (see open questions).
  → **Implication:** XyScript must **preserve all codes verbatim** (round-trip)
  and *interpret* only a growing subset; it cannot and need not understand 1,337.
- **Encoding: DOS OEM code pages, CP437 baseline.** `days.26` is CP437 (its
  horizontal rule is `0xCD` = `═` in CP437, vs `Í` in Latin-1). Per the user:
  **XyWrite 4 natively switches CP437 ↔ CP850**, and user/PostScript extensions
  add **CP775, CP852, CP857, and (modified) CP866**. Files may use any of these.
- **Line endings: CRLF.**

So a XyWrite file = **DOS-codepage bytes + inline `®CODE¯` tokens + CRLF lines**,
and the corpus is overwhelmingly this format.

---

## The work splits into two layers

### Layer A — encoding, the EditPad way: bytes are the truth, encoding is a *view*

The user's stated **ultimate model is EditPad Pro** (`encoding.txt`): the editor
**keeps the file's actual bytes in memory** and treats the encoding as a
*switchable interpretation* — you can re-interpret the same bytes through any
encoding instantly (no reload), **without modifying the file's bytes**, with `?`
shown for bytes/characters the chosen encoding can't represent. Typed characters
are converted to the file's encoding on the fly; characters the encoding can't
hold become `?`. The requirement, minimally: **read and (ideally) display** the
DOS code pages (437/850 native; 775/852/857/866 via extensions) and, eventually,
**switch display among a wide range of encodings — all without changing the
underlying bytes.**

This is the **opposite** of "decode to Unicode on load, re-encode on save." It is
a **byte-oriented document model**, which is in direct tension with the Stage-3
text engine (a UTF-8 rope; see the architecture fork below). The non-negotiable
properties it buys: byte-exact preservation (incl. bytes invalid in the current
view), instant encoding re-interpretation, and no data loss from a wrong-encoding
guess.

Encodings to support (growing): the DOS/OEM set first (**437, 850, 852, 857,
866, 775**), then the broad EditPad-style range (Windows-125x, ISO-8859-*, Mac,
KOI8, UTF-8/16, …). The list in `encoding.txt` is the north-star breadth.

### Layer B — XyWrite function-code awareness *(the real feature)*

On top of Layer A, understand the `®CODE¯` tokens rather than treating them as
plain text:

- **Parse** the inline `®CODE¯` markers into structured tokens (code + args),
  distinct from ordinary text.
- **Represent** them in the buffer so they are atomic (you can't accidentally
  edit half a code), while preserving exact bytes for round-trip.
- **Render** per a mode: *revealed* (show `®FC¯`), *hidden* (codes invisible), or
  *interpreted* (apply `MDBO`→bold, etc. in the display) — XyWrite had a
  reveal-codes toggle.
- **Edit / author** codes (insert/delete a code as a unit) — later.

---

## The architectural fork: byte-oriented document vs. the Stage-3 UTF-8 rope

This is the load-bearing decision the EditPad requirement forces, and it reaches
back into Stage 3. Two models:

- **(R) UTF-8 rope + tracked encoding (current Stage-3 engine).** Decode bytes to
  Unicode on open, hold a `ropey` rope, re-encode on save. For the DOS code pages
  (437/850/852/857/866/775) this is **lossless round-trip** *because those OEM
  mappings are bijective* (every byte ↔ a distinct Unicode char) — so opening a
  XyWrite file and saving it back is byte-exact, and the `®…¯` codes ride along as
  `®` text. **But** it cannot switch the display encoding without **re-reading the
  file** (the rope holds decoded text, not the original bytes), and it can't
  preserve bytes that are invalid in the chosen encoding. It keeps the validated
  Stage-3 wins (300 MB perf, undo) untouched.
- **(B) Byte-oriented document (the EditPad model).** Hold the raw bytes; decode
  a *view* through the current encoding for display; switching encoding is instant
  and never touches bytes; typed chars encode to bytes (unrepresentable → `?`).
  This is what the user named as the ultimate model. **Cost:** it replaces the
  UTF-8 rope as the document representation — a byte-rope/piece-table over bytes,
  with undo and the 300 MB performance **re-established** on it; the `Buffer` API
  and viewport math would sit on bytes-decoded-to-a-line-view.

**The tension:** (R) is what's built and is lossless for the immediate
DOS-codepage need, but it does **not** deliver live encoding-switching — the
user's explicit eventual goal. (B) delivers the full model but is a foundational
re-architecture of the text engine (redoing parts of Stage 3 steps 1–2).

**Lean / proposed path (small steps, per the user):**
1. **Near term (R+):** keep the rope, add **tracked per-document encoding** with
   the bijective DOS code pages (437/850/852/857/866/775) so XyWrite files open,
   *display in the right code page*, edit, and round-trip **byte-exact**.
   Encoding choice is per-open (re-open to change it) — not yet live-switchable.
   This opens the user's whole library safely and soon.
2. **Then decide (B) deliberately:** when live encoding-switching / the broad
   EditPad range / invalid-byte preservation become the priority, design the
   byte-oriented engine as its own effort (it's a text-engine re-architecture, so
   it gets the same care D1 got — possibly a spike comparing a byte-rope to
   keeping bytes-alongside-the-rope). **Flag now so we don't over-invest in the
   rope in a way that's costly to migrate.**

*(Open question for the user: is live, no-reload encoding switching needed soon —
pushing us to (B) sooner — or is "open in the correct code page, lossless
round-trip" enough for now, deferring (B)?)*

## Goals, non-goals, and phasing

**Goal:** open, read, edit, and **losslessly round-trip** native XyWrite files,
progressing toward XyWrite-faithful display/editing of the embedded codes.

**Non-goals (at least initially):** a full XyWrite formatting engine (page
layout, footnotes, printing); converting XyWrite documents to a different format;
emulating every one of XyWrite's hundreds of codes.

**Phasing (each lands with tests, like Stage 3):**

- **Phase 1 — Encoding (Layer A).** Open/save non-UTF-8 (incl. CP437) with
  lossless round-trip; XyWrite files open as raw CP437 text (`®…¯` visible).
  Unblocks "open my files" safely.
- **Phase 2 — Function-code model (Layer B core).** Parse/represent `®CODE¯` as
  atomic tokens; a reveal/hide toggle; guarantee byte-exact round-trip of
  untouched content.
- **Phase 3 — Interpreted display + authoring.** Apply formatting modes in the
  display; insert/delete codes; grow the supported vocabulary as needed.

---

## Load-bearing decisions (resolve before the relevant phase)

| # | Decision | Phase | Notes / lean |
|---|---|---|---|
| **A0 (central)** | **Document model: UTF-8 rope (R) vs byte-oriented (B)** | A | **DECIDED (2026-06-07): (B) byte-oriented** — the EditPad model (raw bytes = truth, encoding = switchable view). Supersedes Stage-3 D1 (`ropey`) for the document representation. Designed in its own doc: **`byte-text-engine-plan.md`**. The rest of this plan's Layer A is realized by that engine. |
| **EA1** | Encoding tables: hand-rolled vs. a crate | A | The DOS/OEM code pages (437/850/852/857/866/775) are fixed 256-entry, mostly **bijective** tables. The WHATWG crate `encoding_rs` **excludes** OEM code pages — so either hand-roll OEM tables or add an OEM crate (`oem_cp`/`codepage`). **Lean:** start with `oem_cp` (or hand-rolled tables) for the DOS set; add `encoding_rs` for the broad EditPad range (Windows-125x/ISO-8859/UTF-16) later. |
| **EA2** | Encoding detection & override | A | BOM for UTF-*; default + explicit override (e.g. `OPEN`/`SAVE` honoring a document encoding the user can set). XyWrite/`®…¯` ⇒ CP437 default. |
| **EB1** | **How function codes live in the buffer** | B | The crux. (i) **Inline text** (Phase 1: codes are just `®…¯` characters in the rope — simplest, lossless, but editable byte-by-byte). (ii) **Atomic tokens** (Phase 2: a model where each code is one indivisible unit alongside text). **Lean:** inline for Phase 1; design an atomic-token representation for Phase 2 (likely a parallel structure or a sentinel the rope+a side-table track). This is its own decision doc when we get there. |
| **EB2** | Render modes (revealed / hidden / interpreted) | B | Match XyWrite's reveal-codes; default to *revealed* (honest) first, *interpreted* later. |
| **EB3** | Round-trip fidelity contract | A/B | **Byte-exact** for any content the user didn't touch (these are real diaries — corruption is unacceptable). Save re-encodes only changed runs; unknown codes preserved verbatim. |
| **EB4** | Vocabulary coverage | B/C | Which codes we *interpret* vs. *preserve-opaquely*. Preserve all; interpret a growing subset (start with the `MD*` formatting modes seen in real files). |

---

## Open domain questions — for the user (the XyWrite expert)

The function-code layer can't be designed without these. (Answers will set EB1–EB4
and likely add detail to Layer A.)

1. **Code semantics.** What do the frequent codes mean — especially `MD` (750×),
   `LD` (425×), `DC` (366×)? Are `MD*` all formatting-mode toggles
   (bold/italic/underline/normal/reverse)? What are `FC`, `FL`, `LM`, `RM`, `TS`,
   `RT`, `SS`, `NM`/`NT`, `US`, `LBLink`?
2. **File structure.** Is there a header or sections beyond inline codes? The
   file starts `®FC¯®MDBO¯…` — is `FC` a file/format marker, a footnote, a
   "frame," something else? Do XyWrite files carry multiple "save-get"
   frames/windows?
3. **Encoding reality.** Code pages confirmed by you: 437/850 native, plus
   775/852/857/866 via extensions. Which appear in *this* corpus, and how is a
   file's code page known (a `®`-code in the file, an out-of-band convention, or
   just "you know which language")? Any way to auto-detect, or always
   user-selected?
4. **Live encoding-switching urgency (drives decision A0).** Is no-reload,
   switch-the-view-instantly encoding (full EditPad model → byte-oriented engine)
   needed *soon*, or is "open in the correct code page with byte-exact
   round-trip" (rope + tracked encoding) enough for now?
5. **The end goal.** Should XyScript be a **faithful XyWrite-file editor** (open,
   edit with codes intact, save byte-faithfully — even author new codes), or
   primarily an **importer/reader/converter**? This sets how far Layer B goes.
6. **Display intent.** Do you want codes shown *revealed* (`®FC¯`), *hidden*, or
   *interpreted as formatting* — and is a reveal-codes toggle (XyWrite-style) the
   model to match?
7. **Reference material.** Is there a XyWrite function-code reference (a manual /
   the `CLD4R.KBD` / `reference/CLD4R.xyw.KBD`) we should mine for the full
   vocabulary and argument syntax?

---

## Recommended next actions

1. **User answers the domain questions above** — especially Q4 (live-switching
   urgency), which decides **A0** (rope vs byte-oriented), and Q1/Q5 (code
   semantics + display), which shape Layer B.
2. **Near term, regardless of A0:** build **Phase 1 (encoding on the rope, R+)** —
   tracked per-document DOS code page (437/850/852/857/866/775) so the whole
   library opens, displays correctly, and round-trips byte-exact. Well-defined,
   safe, unblocks reading the corpus, and discharges the deferred Stage-3
   "encoding" item. (If Q4 says live-switching is needed soon, instead front-load
   the **A0 / byte-oriented** decision doc + spike before building further.)
3. **Write the EB1 decision doc** (buffer representation of codes) once the domain
   answers are in, then build Phase 2.

**Risk to hold above all:** these are **irreplaceable real documents**
(diaries/calendars). Round-trip must be byte-exact for untouched content from day
one; we test against real files (read → write → `fc`/hash-compare) before trusting
any save path.
