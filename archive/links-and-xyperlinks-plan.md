---
title: links-and-xyperlinks-plan
tags: [design, links, uri, xyperlinks, highlighting, security, actionable-spans]
created: 2026-06-08
status: planning — no code yet. Decisions LK1/LK2/XL1/TH1 OPEN (with leans); XL2 (security) DECIDED 2026-06-08. Companion to gfm-track-plan.md (highlighter seam 8a, render model 8b) and xyscript-product-scope.md.
---

# Live links & Xyperlinks (plan)

> **Note (2026-11-11):** the Xyperlink half of this plan is superseded by [`docs/design/xlinks.md`](../docs/design/xlinks.md) (the URL-highlighting half shipped as `URLHIGHLIGHT`).

*Two user-requested features that converge on one mechanism: (1) highlight URLs/URIs
in any document with a user-chosen color and make them **live** (activatable), and
(2) **Xyperlinks** — the XyWrite idea of highlighted text carrying an embedded XPL
action, run by putting the cursor on it and pressing a key.*

---

## The unifying idea: an "actionable span" + a `FOLLOW` verb

Both features are the same gesture over the same structure:

- An **actionable span** = a highlighted text range that carries an **action
  payload** — a URL, a named routine, or (gated) inline XPL.
- A **`FOLLOW` verb**, bound to a key, finds the actionable span under the cursor
  and **dispatches** by action type: URL → open in the OS default handler;
  routine → `CALL` it; XPL → interpret it (security-gated, see XL2).

This is squarely "it's all XPL": activation is a verb bound to a key; the action
is XPL (or a thing XPL can do). Execution is essentially **already built** — we
have the interpreter and `CALL`. The work is detection, the action payload, the
activation surface, and (the hard part) **security**.

---

## (1) URL/URI highlighting + live links

**Detect & highlight**
- **Markdown:** comrak already yields Link/autolink nodes (autolink extension is
  on), so `[text](url)`, `<http://…>`, and bare URLs tokenize as `TokenKind::Link`
  today — but we currently **discard the target**. Capturing it is the small
  extension under "forward-compat" below.
- **Any buffer:** an always-on **URL-scanner overlay** that finds URIs regardless
  of file type and emits Link tokens into the same span pipeline — its own toggle,
  **independent of `HIGHLIGHT on/off`** (links may be wanted on a quiet screen).

**User-modifiable color** — the first concrete slice of the deferred Xy-MoDe color
work (**TH1**): make the `TokenKind → Style` theme **user-settable** and add a
`Color::Rgb(u8,u8,u8)` arm (already anticipated in `style.rs`).

**Make them live** (by surface):
- **Terminal (now):** emit **OSC 8 hyperlink** escapes — the terminal makes the
  text clickable and opens it (no click-handling on our side). Support varies
  (Windows Terminal/iTerm2 yes; legacy conhost no).
- **Universal (now):** cursor-on-link + key → `FOLLOW` → open via the OS default
  handler (the `open` crate, or `start`/`xdg-open`/`open`). Works anywhere; also
  the foundation for Xyperlinks.
- **GUI (end-state):** native clickable text.

## (2) Xyperlinks

A highlighted span whose action is **XPL** (or a reference to it), run by cursor +
key — i.e. an actionable span whose payload is code. Execution is trivial (we have
the interpreter); the design is all about **format** and **safety**.

**Embedding format (XL1)** — where the action lives:
- **Recommended: a GFM link with a scheme** — `[label](run:my_routine)` or
  `[label](xpl:…)`. Stays valid GFM (renders/degrades as an ordinary link,
  highlight is free); the link *target* is the action.
- **Alt: reference-style indirection** — `[label](#act)` + a definitions block
  mapping `act` → XPL, keeping executable code in one auditable place.
- **Ruled out:** XyWrite-style inline embedded function codes (the
  formatting-machinery that's out of product scope).

---

## XL2 — Security model  ✅ DECIDED (2026-06-08)

Executing code that travels *inside a document* is a real hazard in an
authoring/sharing world. Tiered rollout (the agreed lean):

- **Tier 1 — ship first (lower risk):**
  - **URL links** → open in the OS default handler. (Still worth confirming
    unfamiliar hosts.)
  - **Named-routine links** (`run:<name>`) → `CALL` a routine **from the user's
    own library**. The document chooses *which* of *your* routines runs — not
    arbitrary code.
- **Tier 2 — gated / deferred (higher risk):**
  - **Inline arbitrary XPL** (`xpl:…`) — only behind **explicit per-document
    trust** + a **confirmation prompt that shows the code**, run under a
    **restricted host capability set** (no shell / no file-write unless trusted),
    and **off by default for untrusted documents**.
- **Foundational rules (always, all tiers):**
  - **Never auto-execute.** Actions run only on **explicit cursor+key
    activation** (XyWrite's own model) — never on open, never on render.
  - Activation is a **verb (`FOLLOW`) bound to a key** — auditable, rebindable,
    and itself "just XPL."

---

## Decisions

| # | Decision | Status / lean |
|---|---|---|
| **XL2** | Xyperlink security model | ✅ **DECIDED** — tiered (above): URL + named-routine first; inline XPL gated by trust + confirmation + restricted caps, off by default; never auto-execute. |
| **LK1** | URL detection source | Markdown links via comrak (capture target) **+** an always-on URL-scanner overlay for any buffer. Crate: `linkify` (purpose-built) vs hand-rolled `regex`. *Lean:* both sources; scanner as its own toggle. OPEN (crate). |
| **LK2** | Live-link activation surface | (a) terminal **OSC 8**; (b) universal **cursor+key → `FOLLOW` → `open`**; (c) GUI native click. *Lean:* ship (b) (also powers Xyperlinks) + (a) as a terminal nicety; (c) at the GUI. OPEN. |
| **XL1** | Xyperlink embedding format | GFM link with a scheme (`run:`/`xpl:`) [recommended] vs reference-style indirection vs (ruled out) inline codes. *Lean:* GFM-scheme. OPEN. |
| **TH1** | Configurable theme / Xy-MoDe color | Make `TokenKind → Style` user-settable + add `Color::Rgb`. Scope: minimal (link color only) vs general (whole theme). *Lean:* build the general mechanism, expose incrementally. OPEN. |

---

## Forward-compat to bank now (cheap now, awkward to retrofit)

- **Token + render model carry an optional action/link payload.** 8a's
  `Token { start, end, kind }` should be able to grow a `target`/action for Link
  tokens (capture comrak's URL instead of discarding it); the **render model (8b)**
  should carry link targets too (PDF supports clickable links; the GUI will want
  them). Design this seam into 8b rather than retrofitting.
- **`Color::Rgb` + a settable theme** (TH1) — the hook is already noted in
  `style.rs`.

None of this changes the byte engine, comrak, or the PDF path — **it does not gate
the G3 spike.**

---

## Sequencing

A **later track**, after the GFM core, built incrementally:
1. Capture link targets in tokens + the render model (with 8b).
2. URL highlight (overlay) + `Color::Rgb`/settable link color (TH1 slice).
3. `FOLLOW` verb + `open` (universal) and OSC 8 (terminal) — live URLs.
4. Named-routine Xyperlinks (`run:<name>` → `CALL`) — Tier 1.
5. *(Maybe, much later)* inline-XPL Xyperlinks behind the XL2 trust/confirm/sandbox.
