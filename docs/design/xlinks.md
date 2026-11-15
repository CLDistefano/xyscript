---
title: xlinks
tags: [design, xlinks, hyperlinks, sidecar, xymeta, bookmarks, security, navigation, xpl]
created: 2026-11-11
status: phase 1 BUILT (2026-11-11, merged to main). Phase 2 TRUST/ACTION TIER BUILT (2026-11-11): action payloads run once a document is trusted, with a Run once / Trust / Cancel confirm dialog, a persisted path+payload-hash ledger (xyscript-trust.txt), and XLTRUST/XLUNTRUST/$XLTRUSTED. Remaining phase-2 threads (jumplist, authoring niceties, named xlinks) not yet built. Supersedes the "Xyperlinks" half of archive/links-and-xyperlinks-plan.md (the URL-highlighting half already shipped as URLHIGHLIGHT).
---

# xlinks — XyScript hyperlinks (design)

*An **xlink** is a span of highlighted text carrying a block of XPL that runs when
you activate the span — jump to a preset spot in another document, or anything
else XPL can do. xlinks live in the document's `.xymeta` sidecar, survive editing,
and can be navigated forward and back like a browser's history.*

This is the XyWrite "embedded-action" idea reimagined on XyScript's own
machinery. It was earlier called a **Xyperlink** (Xy + hyperlink); "xlink" is the
name we're shipping under.

---

## Relationship to prior work

`archive/links-and-xyperlinks-plan.md` (2026-06-08) unified two requests: live
URL highlighting and code-bearing "Xyperlinks." Since then:

- **The URL half shipped** — `URLHIGHLIGHT on|off`, Ctrl+click to open,
  `URLFGCOLOR`/`URLBGCOLOR`. Its click-hit-testing and open plumbing are reused
  here for xlink activation.
- **Themes/RGB color shipped** — `FGCOLOR`/`BGCOLOR`, `THEME`, `Color::Rgb`
  (resolving the old plan's TH1). xlink styling clones these knobs.
- **The `.xymeta` sidecar now exists** — holding bookmarks and style spans. This
  is the decisive change: the old plan recommended an *inline* GFM-scheme embedding
  (`[label](xpl:…)`) only because there was nowhere else to put the action. With a
  reconciling sidecar, we store the action out-of-text without polluting the bytes
  — cleaner than the inline scheme and than the XyWrite in-stream codes that plan
  ruled out.

The old plan's **security model (XL2)** — tiered, never-auto-execute, confirm
showing the code, per-document trust — still holds and is carried forward below,
with one refinement (permissive capability + provenance trust, rather than a
capability sandbox).

---

## The model

> **An xlink = an anchored text span + an XPL command line, stored in the sidecar.**

That reuses four subsystems already in the codebase:

| xlink needs | reuses |
|---|---|
| a position that survives editing | **MARK bookmarks** — self-anchoring, reconciled on every edit, in `.xymeta` |
| a *span* with visible styling | **MDA block styling** (`set_spans_bulk`) + FGCOLOR-on-selection, persisted in `.xymeta` |
| click-to-activate | **URLHIGHLIGHT** — Ctrl+click hit-testing + open, already wired |
| running arbitrary behavior | the VM — `exec_command`; "it's all XPL" |

So an xlink is, almost exactly, a bookmark that (a) spans a range and (b) carries
a command line.

---

## Phase 1 — as built (2026-11-11)

Phase 1 shipped as designed, with **one change forced by the codebase**: the
design assumed a `GO <line>` verb and *named* bookmarks (`GOMARK budget-q3`), but
XyScript had neither — bookmarks are **positional** (`MARKNEXT`/`MARKPREV`), not
named, and nothing exposed a line jump. So:

- **Added `GOTOLINE <n>`** — a first-class navigation verb (jump to a 1-based line,
  centred). It's generally useful, and it's what nav xlinks compile to.
- **`/L` replaces `/M`.** The nav-target sugar is **`XLINK/L [file:]<line>`**, which
  compiles to `GOTOLINE <n>` (same doc) or `OPEN <file>;GOTOLINE <n>` (another).
  Line targets are more fragile across edits than a bookmark would be, but they
  work today. **Named-bookmark targets (`/M`) are deferred** until the `MARK`
  system grows names (tracked with named xlinks, S4 → phase 2).

What landed: the `[xlinks]` sidecar record (two self-anchoring endpoints + a
verbatim payload, reconciled on reopen); the verbs **`XLINK`** (raw payload) /
**`XLINK/L`** / **`XLDEL`**\[`/A`\] / **`XLGO`** plus **`GOTOLINE`**; the
deny-by-default **navigation-only classifier** gating `XLGO`; `$XLINK` / `$XLINKS`;
and the GUI layer — spans rendered in **`XLFGCOLOR`** (default teal) / **`XLBGCOLOR`**,
**Ctrl+click** to activate, **`XLINKHIGHLIGHT`** on by default. The multi-line
payload editor (bare `XLINK`), `XLHERE`/`XLINK/H`, `/X` (expand-now), named xlinks,
and all of **Navigate** (`XLNEXT`/`XLPREV`/`XLBACK`/`XLFWD`/`XLINKS`) remain for
later phases. The verb surface below is the *full* design; items not in the list
above are not yet built.

---

## Settled decisions

| # | Decision | Call |
|---|---|---|
| **S1** | Storage | **Sidecar-anchored span** in `.xymeta` (not inline markup). Keeps the file pure text; reuses bookmark/MDA reconcile. Trade-offs (copy/paste and external-editor edits) are the same known limits bookmarks already have. |
| **S2** | First tier | **Navigation-only first.** Phase 1 ships nav payloads only — safe by construction, so it needs no trust machinery. |
| **S3** | Capability | **Permissive — any XPL is fair game.** Safety is enforced at the *provenance/trust* layer, not by restricting what you can author. The real hazard is a code-link you *didn't write* (a received doc), not what XPL can do. |
| **S4** | Named xlinks | **Deferred to phase 2.** Phase 1 xlinks are anonymous (addressed by position). `/N <name>` + `XLGO <name>` comes later. |
| **S5** | Delete verb | **`XLDEL`** (parallels `DEL`/`DELMAIL`). *Not* `XLKILL` — `KILL` already means "kill a background job." |
| **S6** | Nav targets | **A target sugar ships in phase 1.** Intended as `/M` (named-bookmark targets, survive editing), but named bookmarks don't exist — shipped as **`/L` (line targets)** via a new `GOTOLINE` verb. `/M` deferred until `MARK` gains names. See "Phase 1 — as built". |

---

## Authoring verb surface

The `XL*` namespace is clean — no `XL*` verb exists yet (`XSW` is windows, `HL*`
is highlighting). `XLINK` (singular) acts; `XLINKS` (plural) lists.

### Create — the daily driver

**`XLINK <payload>`** wraps the current selection in an xlink whose activation
payload is the rest of the line.

```
; select "see the budget", then:
XLINK OPEN budget.txt;GOTOLINE 4200
```

- **Payload stored raw — no `$`-expansion at author time** (the `BC` model, literal
  `raw_args`). You want `XLINK OPEN $CWD\notes.txt;GOTOLINE 500` to resolve `$CWD`
  *when clicked*, not freeze it now. `XLINK/X …` (expand-now) is a later phase.
- **`XLINK` is an exempt verb** (added to `EXEMPT_VERBS` with `X`/`XY`/`XV`, 2026-11-12):
  the splitter takes the *rest of the line* as the payload, so a multi-command `;`
  payload needs no escaping (`XLINK OPEN x;GOTOLINE 5` stores both). `XLINK [payload];next`
  delimits the payload and resumes `;`-chaining after `]`. (`XLINK*` longer verbs —
  `XLINKHIGHLIGHT`, `XLDEL`, … — don't match the exempt `XLINK`.)
- **Selection required** — the span *is* the link (its text is the label; no
  separate alias in v1). Bare **`XLINK`** with a selection opens a payload editor
  (reuse `TXT`/`SCRATCH`/`BEDIT`) for multi-line code; here-strings (`@'…'@`) also
  carry literal `;`/newlines.
- **Overlap rejected, not merged** — creating over an existing xlink errors
  ("xlink already here — `XLINK/E` to edit, `XLDEL` to remove").

### Line targets (`/L`) — the nav sugar, as shipped

The common nav case shouldn't make you hand-write `OPEN …;GOTOLINE …`, so **`/L`**
builds it:

```
XLINK/L 4200                   ; same doc  → GOTOLINE 4200
XLINK/L budget.txt:4200        ; another   → OPEN budget.txt;GOTOLINE 4200
```

The file text is kept verbatim (expanded at click time, not now); the file/line
split is on the *last* `:`, so a Windows path (`C:\docs\budget.txt:17`) works. The
xlink's own *source* span still reconciles across edits via the sidecar record;
the *destination* is a line number, which does **not** track edits — that's the
fragility a named bookmark would fix.

> **Deferred (needs named bookmarks):** the originally-designed `/M` — destinations
> that are **bookmarks**, so both ends survive editing (`XLINK/M budget-q3` →
> `OPEN …;GOMARK budget-q3` at click time) — plus the `XLHERE` / `XLINK/H`
> "copy link to this location" capture. These wait until the `MARK` system gains
> names.

### The full family

| | Verb | Does |
|---|---|---|
| **Create** | `XLINK <payload>` | wrap selection, inline literal payload — **phase 1** |
| | `XLINK/L <[file:]line>` | nav link to a line via `GOTOLINE` (sugar) — **phase 1** |
| | `XLINK` (bare) | open editor for a multi-line payload |
| | `XLINK/M <[file:]mark>` | nav link to a bookmark (sugar) — deferred (needs named marks) |
| | `XLINK/H` | link to the location stashed by `XLHERE` |
| | `XLINK/X …` | expand `$`-vars now instead of at click |
| | `XLINK/N <name> …` | name the xlink — **phase 2** |
| **Edit** | `XLINK/E` | edit payload of xlink at caret |
| **Delete** | `XLDEL` / `XLDEL/A` | remove at caret / all (all confirms) |
| **Activate** | `XLGO [name]` | run the xlink at caret (or a named one) |
| **Navigate** | `XLNEXT` / `XLPREV` | hop between xlink spans |
| | `XLBACK` / `XLFWD` | walk the jumplist |
| | `XLINKS` | `PICK`-style modal of all xlinks → jump |
| **Appearance** | `XLINKHIGHLIGHT on\|off` | show/hide xlink styling (clone of `URLHIGHLIGHT`) |
| | `XLFGCOLOR` / `XLBGCOLOR` / `XLMODE` | nav-link style (clones of `URL*`/`HL*`) |
| | `XLACTIONMODE` | distinct style/badge for non-nav (action) links |
| **Specials** | `$XLINK` | payload of xlink at caret |
| | `$XLINKS` | count / list in the doc |
| | `$XLINKHIGHLIGHT` | current toggle state |

Every row is either a clone of an existing verb (`URL*`/`HL*`/`PICK`) or thin
logic over the sidecar record.

---

## Storage — the `.xymeta` record

The real sidecar format ([xyscript-app/src/host.rs:248](../../xyscript-app/src/host.rs)):

```
xymeta 1
len 2048
hash 9f3c1a07b2e5d4c6
[colors]
fg 120 126 200 40 40
[attrs]
at 120 126 1
[marks]
m 512 3 2e62756467657473
```

Two properties decide the xlink shape:

- **Unknown `[sections]` are silently skipped** (host.rs:367) → adding `[xlinks]`
  is backward-safe.
- **Colors/attrs are byte-guarded** (`len`+`hash`; trusted only if bytes
  unchanged), but **marks are always restored because each self-anchors**:
  `m <off> <split> <ctx-hex>` → `Bookmark { off, ctx, split }`, where `ctx` is a
  window of raw bytes and `split` is the mark's position within it. On reload
  against changed bytes it relocates by finding `ctx`. (ctx is **hex** so it can't
  collide with field separators, whatever bytes it holds — host.rs:278.)

### Key insight: an xlink is two bookmarks and a payload

Each endpoint anchors *exactly* like a `Bookmark` — same `(off, ctx, split)`, same
relocate routine, run twice. This is the correct model, not a shortcut: when text
*inside* the span is edited, the start anchor stays and the end anchor slides by
the edit delta — which independent point-anchors give for free. (Storing end as
`start+len` breaks on any intra-span edit.) xlinks inherit the whole bookmark
reconcile story with no new anchoring code.

### The `[xlinks]` section

```
[xlinks]
x 120 0 627564676574 126 1 2e747874 4f50454e20…
  └start─┘ └─ctx ─┘ └─end─┘ └ctx ─┘ └ payload hex ┘
   off split          off split

x <s_off> <s_split> <s_ctx_hex>  <e_off> <e_split> <e_ctx_hex>  <payload_hex>
```

One line per xlink, whitespace-delimited into fixed arity (every field is hex or
an integer — no embedded whitespace — so a plain split works, like the `m` line).
Three deliberate choices:

1. **Payload is hex-encoded**, per the file's own rule. XPL payloads hold spaces,
   `;`, `[`, and newlines (the multi-line editor path) — hex round-trips all of
   them verbatim. Unreadable raw, but the sidecar is hidden and payloads are viewed
   via `XLINK/E` / `$XLINK`.
2. **Kind (nav vs. action) is NOT stored — it's derived from the payload.** A
   stored `kind=nav` flag could let a hostile sidecar label an action payload
   "safe" and dodge the trust gate. Classifying the payload itself means **the file
   can't lie about what the link does.** The record stores only the irreducible:
   two anchors + the code. The style badge is likewise derived.
3. **xlinks relocate like marks, not colors** — parsed and re-anchored regardless
   of `guard_ok`, even against externally-edited bytes. If both ends relocate to
   the same point (spanned text deleted) → empty span → **flag broken, show it,
   don't fire** (the orphaned-link rule).

### Code touch-points

Small and localized; rides existing seams:

- `xymeta_serialize` / `xymeta_parse` / `XymetaRestore` gain an `xlinks` field and
  a `[xlinks]` arm (host.rs:248, :327, :346) — parallel to the `marks` plumbing.
- The relocate call reuses the `Bookmark` anchor logic, per endpoint.
- **Easy to miss:** the empty-check at host.rs:255 (`if fg.is_empty() && … &&
  marks.is_empty()`) must also test `xlinks.is_empty()`, or a doc whose *only*
  metadata is xlinks won't get a sidecar.
- `Document` grows `xlinks: Vec<Xlink>` beside its marks, reconciled in the same
  edit hook that already slides marks (document.rs:4324, "marks after an insert
  must slide").

---

## Security / trust model

The hazard is executing code that travels *inside a document* you may have
received. Safety is a property of **provenance, not payload** — your own
`XY [rm -rf …]` is your foot; a stranger's identical link is an attack.

**Foundational rules (all tiers, always):**

- **Never auto-execute.** Opening a doc only *displays* xlinks. Actions run only on
  explicit activation (Ctrl+click or `XLGO`) — never on open, never on render.
- Activation is a **verb bound to a key** — auditable, rebindable, itself XPL.
- **Runaway is already handled** — xlinks run through the VM, which has
  time-sliced execution, Ctrl+Break abort, and the XC/SUBMIT recursion cap. No new
  guard needed.

**Tiering (friction, not capability):**

- **Nav tier → runs free.** A payload whose every statement is pure navigation
  (see classifier) fires with no prompt. This is the phase-1 world.
- **Action tier → provenance-gated.** Anything else runs only once the *document*
  is trusted. First activation of an untrusted doc shows the code and offers
  *Run once / Trust this document / Cancel* (reuse the `TRUST` modal machinery).

**The classifier (nav vs. action):** a payload is **nav-safe** only if *every*
flattened statement is on a conservative allowlist of pure-navigation verbs —
`OPEN`, `GOTOLINE`, caret motion (`TF`/`BF`, `LU`/`LD`, `UP`/`DOWN`, `PU`/`PD`,
`HOME`/`END`, …), `MARKNEXT`/`MARKPREV`, `BUFALT`, `DUPVIEW`. **Deny by default:**
any shell-out (`X`/`XY`/`XV`), file write
(`WRITEFILE`/`SAVE`/`DEL`), `RUN`/`BG`, `TRUST`, or *unrecognized* verb pushes the
payload to the action tier. Critically, the classifier walks the **same flattened
statement list the VM will execute**, so an encoded payload can't look nav and run
as action.

**The trust ledger:** per-document trust lives *outside* the sidecar (an attacker
would just set a "trusted" flag in their own) — a user-global file (proposed
`xyscript-trust.txt`), one line per trusted doc: `<path>\t<payload-set-hash>`.

- Keyed by path, like `known_hosts`.
- **Hash the payload set only** (decoded payloads, sorted — not positions or
  styles). So editing text or restyling never re-prompts; a **new or changed code
  payload** invalidates trust and re-confirms. You're asked exactly when new code
  appears, never when you move a link.

**On `.lib` indirection:** calling lib routines or `&`-binders from a
payload is good *ergonomics* (short, readable payloads) but **not a security
layer** — `CALL foo` is only as safe as whoever defined `foo`. The trust gate is
the control. (A foreign doc can't auto-load its own `.lib`: `LOADLIB` is explicit
and validated, so the indirection stays safe-by-default anyway.)

---

## Navigation internals

- **`XLNEXT`/`XLPREV`** — hop between xlink spans in document order (the
  `SPELLNEXT`/MARK-navigation pattern).
- **Jumplist** — following an xlink that jumps away pushes a return-location;
  `XLBACK`/`XLFWD` walk it like a browser's back/forward (Vim's jumplist; Obsidian's
  back arrow). **Session-only**, like REOPEN's caret history and command history —
  not persisted to the sidecar.
- **`XLINKS`** — a `PICK`/`LISTTABS`-style modal of every xlink in the doc (label +
  payload preview), jump to selected.

---

## Models it draws on

- **org-mode `elisp:` links** — the direct precedent for code-bearing links gated
  by a confirm-function. The security model is lifted from here.
- **HyperCard buttons** — a region with a script that runs on click. That's an
  xlink, nearly verbatim.
- **Obsidian / browser** — the back/forward navigation and the "a link to a
  not-yet-existing target is fine" tolerance. (Obsidian's links are
  navigation-only; the *code* dimension is org-mode/HyperCard.)

---

## Phasing

- **Phase 1 — DONE (2026-11-11)** — sidecar `[xlinks]` record; `XLINK` create
  (inline one-liner) + `XLINK/L` line targets (via the new `GOTOLINE`); Ctrl+click
  + `XLGO` activation; the nav-only classifier (so phase 1 is safe by construction,
  no trust machinery yet); `XLINKHIGHLIGHT` + `XLFGCOLOR`/`XLBGCOLOR` style knobs;
  `XLDEL`. (`/M` bookmark targets fell to a later phase — see "Phase 1 — as built".)
- **Phase 2, trust/action tier — DONE (2026-11-11)** — action payloads run once a
  document is trusted; the `XLGO`/Ctrl+click confirm dialog (Run once / Trust this
  document / Cancel) suspends like `RUNMENU` and runs the payload on resume; a
  persisted ledger (`xyscript-trust.txt`, keyed by path → payload-set hash, session
  trust for path-less buffers); `XLTRUST`/`XLUNTRUST`/`$XLTRUSTED`. Trust
  auto-invalidates when the payload set changes. The dialog shows a plain-English
  summary DERIVED from the payload's verbs (`describe_xlink_payload`) above the raw
  code — never author-supplied text, which could lie over hostile code.
- **Navigation & discovery — DONE (2026-11-13)** — `XLNEXT`/`XLPREV`, the `XLINKS`
  picker, the `XLBACK`/`XLFWD` jumplist.
- **Authoring polish — DONE (2026-11-15)** — `XLINK/E` (edit in place),
  `XLFGCOLORS`/`XLBGCOLORS` pickers.
- **Naming — DONE (2026-11-15)** — `MARKSET <name>` + `GOMARK <name>` (named
  bookmarks, `.xymeta` `[marks]` gains an optional name field); `XLINK/M [file:]<name>`
  (bookmark targets — both ends survive edits); `XLINK/N <name> <payload>` + `XLGO
  <name>` (named xlinks, `[xlinks]` gains an optional name field).
- **Still remaining** — multi-line payload editor (bare `XLINK` → editable modal);
  `/X` expand-now; `XLHERE`/`XLINK/H` capture.
- **Phase 3** (maybe never) — cross-document link index / backlinks (that's a wiki,
  a separate feature).

---

## Still open (to sketch before coding)

- ~~**Activation dispatch**~~ — DONE. `XLGO` (vm) classifies, then: nav or trusted →
  `run_source_sync`; untrusted action → suspend `XlinkConfirm`, run on resume. Ctrl+click
  routes through the same `XLGO`.
- ~~**Trust-ledger location/format**~~ — DONE. `xyscript-trust.txt` in `$XSDIR`, one
  `<payload-set-hash>\t<abspath>` per trusted doc; loaded at GUI startup; session trust
  (a `DocId → hash` map) for path-less buffers.
- **Jumplist depth & semantics** — size cap, de-duping, whether `XLNEXT` and
  activation share one list. (Jumplist is a remaining phase-2 thread.)
- **Broken-xlink UX** — how a relocated-to-empty or unresolved-target xlink is
  shown (distinct style) and what activation does (report, offer to edit/delete).

---

*Supersedes the Xyperlink half of `archive/links-and-xyperlinks-plan.md`.
Related shipped work: URLHIGHLIGHT, bookmarks (MARK*), MDA styling, TRUST, REOPEN
(jumplist analog), binders (`&A-Z`).*
