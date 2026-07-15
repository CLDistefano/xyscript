---
title: stage1-interpreter-shape
tags: [decision, stage-1, xpl, interpreter]
created: 2026-06-07
status: DECIDED & COMMITTED (2026-06-07) — AST rebuild (parse → structured AST → flat IR → execute). Not revisitable on QN-compatibility grounds. No code yet.
companion_to: development-plan.md (Foundational premise + Stage 1), keyboard-layer-design.md (§10.1 seam)
---

# Stage 1 — Interpreter shape: DECIDED — AST rebuild

*This document originally framed the choice as "AST rebuild vs. faithful port of
QN's line/PC machine." **That framing is obsolete and the decision is made: it is
an AST rebuild, all the way** — a fresh Rust-native pipeline (**parse → structured
AST → flat IR with resolved jump targets → execute**). Under the project's
foundational premise (`development-plan.md` → "Foundational premise"), QN's
Python/Tkinter structure does not factor into XyScript's architecture at all, so
"faithfully port QN's interpreter machine" was never on the table. The options
analysis and the "against" points below are retained **only as a record of what
was weighed** — none of them carry any weight against XyScript's development arc,
and the decision is not to be reopened on QN-compatibility grounds. **No
interpreter code exists yet; this records the committed direction.***

> **Primary basis — the premise (decisive).** QN's line/PC machine, its
> physical-line pre-scans, its `after_idle` run loop, and its textual
> `$`-substitution are *QN implementation structure*. The premise says that
> structure is descriptive trivia, never a constraint. So XyScript designs control
> flow fresh: **parse → structured AST → flat IR with resolved jump targets →
> execute.** This is a stance, not a finding.
>
> **Corroborating basis — the measurement (2026-06-07, see below).** Even setting
> the premise aside, the data points the same way: there is *nothing to preserve*
> by copying QN's machine — 0 of 221 tests depend on its layout quirks, no script
> relies on them, and the manual warns against them. So the premise costs us
> nothing here; principle and evidence agree.

---

## Measurement results — decision resolved (2026-06-07)

**The deciding question was:** how much of `test_quicknotes.py` actually depends
on QN's physical-line / `;`-chain layout quirks (the behavior an AST would
reproduce *differently*)? Measured directly against `C:\Users\cldis\QN2`:

| What | Count | Bearing on AST-vs-port |
|------|-------|------------------------|
| Total test methods in `test_quicknotes.py` | **221** | — |
| Tests depending on **physical-line / `;`-chain layout quirks** | **0** | The whole supposed cost of AST. It's **zero**. |
| Tests presupposing the **runtime block-stack dispatch mechanism** (single-command `_dispatch_command` stack ops: ENDIF/ELSE/ELSEIF/WHILE skip-pop) | **~16 (~7%)** | Would be **re-expressed**, not literally ported, under a structured model. They test an implementation artifact (the `_qpl_block_stack`), not user-facing layout. |
| Everything else — expression evaluator, `$`-substitution, command splitter (incl. `\;` + exempt verbs), `.qnu`/`.qnk` parsers, scoping/arrays/`LOCAL`, error model, editor-command leaves | **~205** | **Model-independent.** Ports identically whether control flow is line/PC or AST. |

Three corroborating facts from the source:

1. **The PC machine and its pre-scans are untested.** `_run_script`,
   `_prescan_label_depths`, `_prescan_subs`, `_prescan_whiles` (the exact code
   where the layout quirks live) have **zero references** from
   `test_quicknotes.py`. The suite is a unit suite of *pure-logic leaves*, not a
   script-level block-execution oracle. So the plan's premise that the suite is a
   strong **control-flow** oracle is overstated — for block control flow it is
   essentially silent, and *new* tests are needed regardless of which model wins.
2. **No real script depends on the quirks.** The only `.qns` in the tree
   (`scripts/stamp-date.qns`) is linear; there are **no `.qnu` libraries** at
   all. Nothing in the codebase relies on `;`-chained blocks or layout-sensitive
   GOTO.
3. **The quirks are documented footguns, not features.** `qnmanual.md` actively
   warns *against* `IF cond ; cmd ; ENDIF`, against chaining `IF ; GOTO ; ENDIF`
   on one line, etc. ("the pre-scan can't see inside semicolon-chained
   commands"). These are the manual telling users to avoid the line/PC model's
   sharp edges — exactly what a structured model removes by construction.
4. **The PC machine is GUI-event-loop-shaped.** `_run_script` drives an async
   `after_idle` per-line loop so `GOTO` can re-iterate without blowing the Python
   stack and so Tk stays responsive. A faithful Rust port would be replicating a
   *Tkinter-driven control structure* that XyScript's core has no reason to want.

**Applying this document's own decision rule** ("if the layout-dependent cases
are negligible → the AST/structured path becomes attractive, compatibility cost
near-zero"): the layout-dependent cases aren't merely negligible, they are
**zero in the tests, zero in the scripts, and explicitly discouraged in the
manual.** The rule fires cleanly toward the structured model.

### Updated recommendation

**Resolve control flow structurally from day one — but keep the leaves bug-for-bug
faithful.** Concretely:

1. **Faithfully port the model-independent core** (the real, tested oracle): the
   expression grammar `or→and→not→comp→add→mul→unary→atom`, the `$`-substitution
   textual pre-pass and its quoting/escaping rules, the command splitter
   (`\;`, exempt verbs `X`/`XY`/`LOADKEY`…), stringly-typed values + numeric/string
   coercion, case-insensitive identifiers, scoping/arrays/`LOCAL`, the two-tier
   error model, and the library system. These pass `test_quicknotes.py` 1:1 and
   are where the de-risking value actually is.
2. **Parse control flow to a structured AST, then lower to a flat IR with
   resolved jump targets**, and execute that IR. A flat IR with real jump targets
   handles `GOTO`/labels and `WHILE`/`BREAK`/`CONTINUE` cleanly *without* physical-
   line fragility, and is a better base for the QN-beyond goals (real arrays/maps,
   user-defined functions, token-level substitution). No `after_idle` loop; a
   plain instruction-pointer over the IR.
3. **Re-express the ~16 block-stack tests** as structural-equivalent behavior
   tests (e.g. "`ENDIF` with no open `IF` is an error" becomes a parse-time
   structural error rather than a runtime stack-underflow), and **add the block-
   control-flow tests the QN suite never had** — the one place new coverage is
   required either way.

**Net effect on the QN-compatibility consequence the plan asks us to record:**
XyScript will *not* reproduce QN's layout footguns (`IF cond ; cmd ; ENDIF` and
friends). Since no test and no script depends on them and the manual already
tells users not to write them, the compatibility break is **on discouraged
behavior only** — an acceptable, arguably desirable, divergence. Everything users
actually rely on (expressions, substitution, verbs, scoping, errors, libraries)
stays faithful.

**Why this reverses the original lean:** the pre-measurement recommendation
favored the faithful line/PC port on the assumption that `test_quicknotes.py`
made it a clean 1:1 control-flow oracle worth preserving. The measurement shows
that oracle **doesn't cover block control flow at all**, so the port's headline
advantage is largely illusory there, while its costs (replicating an
`after_idle`-shaped PC machine + documented footguns, in Rust, forever) are real.

---

## What the decision actually is

It is tempting to frame this as "tree-walking interpreter vs. program-counter
machine" — an implementation-style preference. It is not. In QN the execution
model and the *observable language semantics* are welded together:

- QN runs **line-at-a-time**: a program counter over a `ctx` dict (`pc`,
  `labels`, `label_depths`, an `IF`-block stack, call frames). Block structure
  (`IF`/`WHILE`/`SUB`) is recovered by **static pre-scans of physical lines**
  plus runtime stacks — not by parsing nested structure.
- Because blocks are resolved by physical-line scanning, **how a script is laid
  out across lines and `;`-separators changes behavior.** This is the origin of
  manual caveats like "don't chain a block `IF` with `;` on one line." The quirk
  is not a bug bolted on; it is a *direct consequence* of the line/PC model.

An **AST** front-end resolves nesting structurally, so it would *automatically
"fix"* those quirks — and in doing so become **not bug-compatible** with any QN
script that (knowingly or not) relies on them. So the real fork is:

- **Faithful line/PC port** → exact QN semantics, quirks included, `test_quicknotes.py`
  is a 1:1 conformance oracle, lower port risk — but you inherit and must
  forever support the quirks.
- **AST rebuild** → cleaner semantics, quirks gone, a better base for the
  QN-beyond ambitions — but you diverge from the oracle exactly where scripts
  lean on layout, and you must decide that divergence is acceptable.

Everything else (stringly-typed values, `$`-substitution as a textual pre-pass,
~200 verbs, the `or→and→not→comp→add→mul→unary→atom` expression grammar,
case-insensitive identifiers) is **independent of this choice** and ports the
same way either way.

---

## Option A — Faithful line/PC port

**What it is.** Reimplement QN's machine in Rust as-is: a PC over a vector of
physical lines, the same static pre-scan to build label/block tables, the same
runtime `IF`/`WHILE`/`SUB` stacks, the same per-command `;`-split → `$`-pre-pass
→ tokenize order.

**For.**

- **Bug-compatible with QN.** Existing `.qns`/`.qnu` scripts and, crucially,
  `test_quicknotes.py` run unchanged. The plan's exit criterion — "passes a Rust
  port of `test_quicknotes.py`'s language-level cases" — becomes a clean
  green/red signal rather than a list of intentional diffs to adjudicate.
- **Lowest port risk.** This mirrors the keyboard layer's own de-risking
  discipline: port faithfully against a proven oracle first, bank parity, then
  exceed. The plan is explicit — "hold this stage to parity with QN's language
  core, bank the de-risked port."
- **Smaller Stage 1.** No grammar design for control structures, no decisions
  about how restructured blocks should behave; you are transcribing a known
  machine.

**Against.**

- **You inherit the quirks permanently.** Once XyScript ships the line/PC
  semantics, new scripts will come to depend on them, and the eventual move to
  structured blocks (a stated QN-beyond goal) becomes a second migration with
  its own compatibility break — paid later, with interest.
- **The model fights some of the QN-beyond list.** Token-level substitution and
  `${}`/nested substitution sit awkwardly on a textual `$`-pre-pass; a real
  block structure is more natural on a tree. You would be building the layer you
  plan to replace.

---

## Option B — AST rebuild

**What it is.** Lex → parse to a structured tree (real `If`/`While`/`Sub`
nodes), tree-walk it. Keep QN's *values and verbs* semantics, but resolve
control flow structurally.

**For.**

- **Fixes the `;`-chain class of quirks for free** and gives a clean base for
  the explicitly-flagged upgrades (real arrays/maps, user-defined functions,
  trappable fatals, token-level substitution, an XyScript-owned regex).
- **One rewrite, not two.** If structured blocks are inevitable, doing them now
  avoids building-then-discarding the line/PC machine.
- **Better error locality.** A parser can report structural errors up front
  rather than discovering an unbalanced block mid-run via stack underflow.

**Against — weighed and explicitly carrying NO weight for XyScript.** *Recorded
for completeness; each was considered and rejected as a basis for the decision.
None survives contact with the foundational premise or the measurement.*

- ~~**Not bug-compatible — the testing philosophy is oracle-based.**~~ **No
  weight.** The premise makes bug-compatibility with QN a non-goal, and the
  measurement shows there is nothing to be compatible *with*: 0 tests and 0
  scripts depend on the layout behavior, and the manual warns against it. There
  is no "mechanical conformance pass" to lose because QN's suite never covered
  block control flow.
- ~~**Larger, riskier Stage 1.**~~ **No weight.** Grammar design for the control
  constructs is ordinary, well-understood work, and it is *less* effort than
  faithfully replicating QN's `after_idle`-driven PC machine and its documented
  footguns in Rust. The `$`-substitution ordering is a design choice XyScript
  owns (and should move token-level anyway), not inherited messiness.
- ~~**Defers the bank.**~~ **No weight.** "Banking parity with QN's machine" was
  never the goal — the premise rejects QN structure as the thing to bank. The
  de-risking that matters (the leaf semantics: expressions, substitution,
  splitter, scoping) is captured faithfully regardless, because those are
  model-independent.

---

## Recommendation (pre-measurement — SUPERSEDED 2026-06-07)

> ⚠️ **Superseded.** This was the provisional lean *before* QN2 was mounted. The
> measurement (see top) found zero layout-quirk dependence in the tests, which
> flips the conclusion toward the structured/AST path. Retained for the reasoning
> trail; the two-layer-seam idea below survives in spirit — the structured model
> simply makes the front-end structured from the start rather than a later swap.

**Port faithfully (Option A) for Stage 1 — but build it as two explicit layers
so an AST front-end can be added later without touching the executor.**

Concretely:

1. **Executor layer:** the PC machine that consumes a flat sequence of
   *resolved commands* against the `ctx` state — values, verbs, expression
   evaluator, error model, scoping, libraries. This is the de-riskable core and
   the bulk of Stage 1.
2. **Front-end layer:** the thing that turns source text into that sequence —
   currently QN's physical-line pre-scan + `;`-split + `$`-pre-pass. Keep this a
   *replaceable seam*, not something fused into the executor.

This buys parity now (Option A's whole upside, and a clean `test_quicknotes.py`
oracle), while making the eventual structured-block front-end an **additive**
change at the seam rather than a rewrite — neutralizing Option A's main
objection. It matches both the plan's de-risking philosophy and its "bank the
port, let Stage 2+ motivate where XyScript exceeds QN" sequencing.

**Why not AST now:** the cost of AST is paid in *divergence from the oracle* and
*semantics design*, both of which the plan explicitly wants Stage 1 to avoid.
The benefit of AST is mostly realized *later* (the QN-beyond features), and the
two-layer port preserves the option to take it then — at a point where the
editor has surfaced concrete requirements to design the new block semantics
*for*, instead of guessing now.

---

## The deciding measurement — ✅ DONE (2026-06-07, see "Measurement results" up top)

> This section described the measurement to run; it has now been run against the
> real QN2 tree and the result (0 layout-dependent tests) is recorded at the top
> of this document. The method below is kept for reproducibility.

The recommendation flips if one number is small. **In `C:\Users\cldis\QN2`,
measure how much of `test_quicknotes.py` actually depends on the line/`;`-chain
layout quirks** (vs. behavior an AST would reproduce identically):

- Grep `test_quicknotes.py` for block `IF`/`WHILE`/`SUB` cases that deliberately
  exercise `;`-chaining, single-line blocks, or label/`pc` layout sensitivity.
- Classify each: *layout-dependent* (would diverge under AST) vs.
  *layout-neutral* (identical either way).

Decision rule:

- **If layout-dependent cases are a meaningful share** (the manual's caveats
  suggest they are non-trivial) → **Option A two-layer port**, as recommended.
  The compatibility cost of AST is real and the oracle stays clean.
- **If they are negligible** (a handful, all arguably testing the bug not the
  feature) → **Option B (AST) becomes attractive**: the compatibility cost is
  near-zero, and you skip building a machine you intend to replace. In that case
  port the values/verbs/expression semantics faithfully but resolve control flow
  structurally from day one.

Either branch keeps the rest of the port (values, `$`-substitution, verbs,
expressions, error model, libraries) identical, so this measurement is the only
thing gating the structural choice.

---

## What this does *not* decide (still Option-independent Stage 1 work)

Per the plan, these proceed the same regardless: the two-tier error model
(trappable `$ERROR` vs. fatal abort), the variable/scoping model (`LOCAL`,
`$arr_0` arrays, read-only special vars), the library system
(`.qnu`/`LOADLIB`/shadow-rejection), the keyboard-layer seam
(`Dispatch(XPL)` → interpreter; `KF` never reaches it), `LOADKBD` as a XPL
command, and authoring `kbd/xyscript-sample.kbd` in real XPL. None of these
depend on the AST-vs-line/PC outcome.

---

## Recommended next action

1. Mount / open `QN2` and run the deciding measurement above.
2. Record the AST-vs-line/PC decision and its QN-compatibility consequences in
   `development-plan.md` (Stage 1 exit criteria call for this written record).
3. Then move to Claude Code on-device to scaffold the executor layer against a
   Rust port of `test_quicknotes.py`.
