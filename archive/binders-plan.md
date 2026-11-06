---
title: binders-build-plan
tags: [design, build-plan, binders, keybinding, xpl]
created: 2026-06-12
---
# Binders — build plan

Turns `binders.md` (the proposal) into a phased, headless-testable build. A **binder** is a
runtime-rebindable register: a key is bound in the KBD file to `@X`/`&X` **once**; the `LDxx`
commands change what those slots *contain* at runtime, so a key's text/script is re-purposed
without reloading the keymap.

## Locked decisions (2026-06-12)
- **Commands:** `LDTX` (load text → `@`slot). `LDSC` (load script → `&`slot), with **`LDPM` as a
  synonym** (XyWrite "LoaD PrograM" muscle memory). Both `LDSC`/`LDPM` dispatch identically.
- **Arg syntax (space-separated): `LDTX <slot> [<source>]`** (same for `LDSC`/`LDPM`). The
  optional source has **three forms, disambiguated by its first non-blank char**:
  - **absent** → the current **selection** (`LDTX h`).
  - **quoted** `"…"` → a **literal** (escapes `\n`/`\t`/… processed, à la `INSERT`/`TXT`):
    `LDTX h "snippet\nline2"`. *(Required by Phase-3 persistence, which emits `LDTX A "…"` lines.)*
  - **bare** → a **file path** read via `host.read_file` (`LDTX p d:\path\my.txt`). Rest-of-tail =
    the path (a path with spaces is fine). To store text that looks like a path, quote it.
- **Slots:** `@A`–`@Z`, `@0`–`@9` (text); `&A`–`&Z`, `&0`–`&9` (script). 36 each. Slot char is
  **case-folded** (`@a` ≡ `@A`).
- **`@X` / `&X` are BOTH values and command-position actions** (position decides — see Semantics).
- **Persistence (future, Phase 3):** a **generated XPL startup script** of `LDTX`/`LDSC` calls
  (no new format/parser; human-editable; folds into the planned `SaveCfg`/`LoadCfg`, B18). NOT JSON.

## Semantics — position decides
- **Command-position** (the binder-ref is the *head* of a statement, e.g. a KBD cell `30=GT;@A`
  or `&A` alone): perform the ACTION.
  - `@X` → insert the stored text **verbatim** at the **active** caret (text or CMline, per focus).
  - `&X` → **run** the stored XPL (deferred via `post_run`, like `SUBMIT`/`XC`/`RUNHERE`, since the
    interpreter can't re-enter mid-dispatch).
  - Empty/unset slot in command-position → trappable error ("binder @X is empty"). *(sub-decision —
    could be a silent no-op; trappable is the safer default.)*
- **Value-position** (binder-ref used as an argument or inside an expression, e.g. `PR @A`,
  `SE @A`, `SET x = @A`): resolves to the stored **string** — the text for `@X`, the source for
  `&X` (symmetric; running only happens in command-position). Unset → empty string (mirrors `$`).

## Storage architecture
- Live on **`AppHost`**: `text_binders` + `script_binders`, each a 36-slot store
  (`HashMap<char,String>` or `[Option<String>;36]`). Consistent with editor state + persistence;
  value-resolution reaches them the same way host special-vars are served.
- `LDTX`/`LDSC` → new `EditorOp`s that write a slot. `@X` action → an insert `EditorOp`. `&X`
  action → set a `pending_run`-style flag carrying the script (drained in `editor.rs::post_run`).

---

## Phase 1 — storage + `LDTX`/`LDSC`/`LDPM` + command-position action (CORE) — ✅ DONE 2026-06-12
**Built & green** (full workspace 11 suites; GUI builds; RUNHERE intact). As-built notes:
- `EditorOp::SetBinder { script, slot, source: BinderSource }` (one op; host resolves the source)
  + `EditorOp::UseBinder { script, slot }`. `BinderSource` enum (`Selection`/`Literal`/`File`) in
  `xyscript-xpl/src/host.rs`, re-exported from lib.
- AppHost: `text_binders`/`script_binders: HashMap<char,String>`; `pending_run` changed
  `Option<String>` → **`Vec<String>` (FIFO queue)** so `&A;&B` and `&X`→`&Y` nesting all run;
  `editor.rs post_run` drains in a `while` loop with a **1000-iteration cap** (self-referential
  binder guard). `UseBinder` text→`active().insert`, script→`pending_run.push`.
- VM: NO lexer change — `binder_ref(base)` pseudo-verb check (`@`/`&` + alnum, `base` already
  upper-cased by `split_verb`/`fold_ident`) dispatches `UseBinder` before `validate_switches`;
  `LDTX`/`LDSC`/`LDPM` arms call `exec_load_binder` (slot parse + source classify via
  `rest.starts_with('"')` → `literal_text`, bare → `BinderSource::File`, empty → `Selection`).
  Bad slot is **trappable in-place** (set status + `$ERROR`, return Ok) — a direct `Err` from
  `exec_command` is fatal, so it can't just `?`.
- Tests: 4 in `vm.rs` (dispatch/parse/case-fold/bad-slot via StubHost log) + 6 in `editor.rs`
  (literal/selection/file round-trips, `&` runs, LDPM synonym, empty-slot error, queue + nesting).

### (original Phase 1 sketch)
*No lexer change needed — the action is a pseudo-verb pattern check in dispatch.*
- `AppHost`: the two slot stores + helpers `set_text_binder/set_script_binder/text_binder/script_binder`.
- `EditorOp`s: `SetTextBinder{slot,text}`, `SetScriptBinder{slot,src}`, `InsertText(String)` (if not
  already present — reuse the existing insert path), and a `RunBinder(String)` that stashes into a
  `pending_run`-style slot (reuse/extend the RUNHERE `pending_run` queue → make it a **queue** so a
  `&X` whose script invokes `&Y` nests correctly).
- **VM dispatch** (`vm.rs`): before the "unknown verb" error, match a binder-ref verb pattern
  `^[@&][A-Za-z0-9]$` → dispatch the action (`@`→insert stored text; `&`→run stored src). Case-fold slot.
- **Verbs** `LDTX` / `LDSC` (+ `LDPM` alias): parse `<slot> [<source>]`; resolve source by first
  non-blank char — absent → `selection_text()` (trappable "no text is selected" if empty); quoted →
  `literal_text` (the INSERT/TXT helper, escapes processed); bare → `host.read_file(path)`
  (host.rs:1428). Store into the slot.
- **Tests (headless, `xyscript-app`):** load-from-selection, load-from-**quoted-literal** (with
  escapes), load-from-file, `@X` inserts at caret, `&X` runs (observe a side effect), `&X`→`&Y`
  nesting, errors (bad slot, no selection, missing file, empty-slot action). Verb-alias `LDPM`==`LDSC`.

## Phase 2 — value access (`@A`/`&A` as a string anywhere) — ✅ DONE 2026-06-12
**Built & green** (xpl 144, app 194, workspace 11 suites, GUI builds). As-built:
- `Tok::Binder{script,slot}` (lexer `@`/`&` arms, slot upper-cased) → `Expr::Binder{script,slot}`
  (parse_atom + eval) → `EvalContext::binder` → `Host::binder` (AppHost reads the maps). Real
  `Ctx` (vm.rs) forwards to `host.binder`. So `SET x=@A`, `IF @A==…`, `UPPER(@A)` work; unset → "".
- **Arg-tail subst** (`subst.rs`): `@X`/`&X` resolve to the stored string, but ONLY when **bounded
  by non-alphanumerics** (`is_binder_ref`: preceding char + char-after-slot must be non-alnum, slot
  single alnum). So literal `a@b.com`, `AT&T`, `R&D`, `@AB`, bare `&` stay untouched; `PR @A`,
  `(@A)`, `&Z it` resolve. Early-out guard now also checks `@`/`&`.
- **STORE-LITERAL decision (bug fix):** `LDTX`/`LDSC` now take the **RAW** (un-substituted) args, so
  loaded content is stored verbatim — `$x`/`@A`/`&B` inside are NOT expanded at load (was a bug:
  `LDSC c "&D"` had `&D` expanded at load by the new arg-tail subst, storing garbage). Net model:
  TEXT binder = stored literal, inserted VERBATIM (no expansion ever); SCRIPT binder = stored
  literal, RUN at use (so nested `&`/`$` resolve when the script runs). Required for persistence too.
- Tests: expr `binder_refs_resolve_as_values`; subst `binder_refs_substitute_when_bounded` +
  `literal_at_and_amp_in_text_are_untouched`; app `binder_value_access_in_args_and_expressions` +
  `literal_at_in_arg_text_is_not_a_binder`.

### (original Phase 2 sketch)
- **Lexer** (`lexer.rs`): add `b'@'`/`b'&'` arms beside the `b'$'` arm (line ~112) → new tokens
  (e.g. `Tok::Binder{kind,slot}`), single slot char only.
- **Expr** (`expr.rs`): resolve binder tokens to a `Value::Str` (stored string; unset → "").
- **Arg-tail substitution** (`subst.rs`): substitute `@X`/`&X` to the stored string, parallel to
  `$NAME` (same fragility/escaping rules as `$`).
- **Tests:** `PR @A`, `SE @A`, `SET x = @A`, `PR &A` (source), unset → empty, inside expressions.

## Phase 3 — persistence (FUTURE)
- Serialize all non-empty slots → a generated XPL script (`LDTX A …` / `LDSC B …` lines, text
  escaped as a quoted literal). Load at startup (alongside `startup.int` / `TRUST 1`); save on
  demand and/or at exit. Fold into `SaveCfg`/`LoadCfg` (B18) so there's one config mechanism.
- **Tests:** round-trip (set → serialize → fresh editor → load → identical).

## Phase 4 — GUI / default-KBD integration + live verify
- Optional default-`.kbd` example bindings (a tidy convention: `Alt`+letter → `@`letter,
  `Ctrl+Alt`+letter → `&`letter). Document, don't force.
- Live test in `xyscript-gui`: bind keys, `LDTX`/`LDSC` from selection + file, press keys to
  insert/run; confirm rebinding-without-KBD-reload.

## Open sub-decisions (confirm during build, non-blocking)
1. Empty-slot action: trappable error (default) vs silent no-op.
2. `&X` value = its **source** string (planned, symmetric) vs run-and-yield `$RV`. (Source chosen.)
3. File-read leniency: `read_file` is strict UTF-8 (fine for authored snippets/scripts); a
   lenient UTF-8-with-replacement read (C-section, for MERGE/RF) is a later option for `LDTX`.
4. Trust: **no new hole** — a bound `&X` runs XPL exactly as any KBD cell already can, and any
   shell-out inside it stays `TRUST`-gated. (Note in docs; no gate needed.)
5. Insert target for `@X`: the **active** target (text/CMline). Matches the proposal.

See also: [[xyscript-gui-keyboard-restructure]] (keymap/dispatch), the parity plan's B9 (file I/O)
and B23 (cmline focus), and `binders.md` (original proposal).
