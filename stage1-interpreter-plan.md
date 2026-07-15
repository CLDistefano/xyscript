---
title: stage1-interpreter-plan
tags: [stage-1, xpl, interpreter, implementation-plan, handoff]
created: 2026-06-07
status: implementation plan — ready for Claude Code handoff (no code yet)
companion_to: development-plan.md (Foundational premise + Stage 1), stage1-interpreter-shape.md (DECIDED — AST), keyboard-layer-design.md (§10.1 seam)
---

# Stage 1 — XPL interpreter: module breakdown & implementation plan

*A concrete plan to hand to Claude Code (on-device, where the Rust toolchain and
QN2 live). It assumes the two settled decisions: the **foundational premise**
(QN is a feature floor; its Python/Tkinter structure constrains nothing;
fresh Rust slate) and the **committed interpreter shape** — an **AST rebuild**:
`parse → structured AST → flat IR with resolved jump targets → execute`.*

> **Reading note.** Every "QN does X" below is a *behavior to meet*, not a
> structure to copy. Concrete QN values (the grammar, the function set, the
> special-variable names) are reproduced so the floor is unambiguous — they are
> the **contract with users/scripts**, not QN implementation we are bound to.

---

## 0. Scope of Stage 1

**In:** the *language* — lexer, parser, AST, IR, executor, value model,
expression evaluator, variable substitution, scoping, the two-tier error model,
the verb-dispatch framework, the library system, and the keyboard-layer seam
(`Dispatch(XPL)` → interpreter, `LOADKBD`, `$SC`).

**Out / stubbed:** anything needing the text engine or GUI (cursor, selection,
search, file I/O, display). Editing verbs are registered but resolve to a
**stub host** that records the call (enough to test dispatch + error flow). The
real text engine is Stage 2; the owned regex engine and real concurrency are
later. These are wired through a `Host` trait (see §9) so the interpreter stays
**headless-testable** with no GUI and no OS hooks — mirroring the keyboard
layer's discipline.

**Definition of done:** a keyed assignment runs end-to-end (physical key →
canonical event → resolve → `Dispatch(XPL)` → interpreter executes → observable
effect via the stub host), and XyScript's own headless language test suite is
green — including the block-control-flow coverage QN's suite never had.

---

## 1. Architecture at a glance

```
source text (an assignment string, a CMline line, a .qns file, a .qnu routine)
   │
   ▼  splitter        split on ';' (honor \; ; exempt verbs consume rest verbatim)
   ▼  lexer           text → tokens (substitution is token-level, not textual)
   ▼  parser          tokens → structured AST  (If/While/Sub/Goto are real nodes)
   ▼  lowering        AST → flat IR (Vec<Instr>) with labels resolved to indices
   ▼  executor (VM)   instruction pointer over the IR + a Frame/Scope stack
        │ calls
        ├── Verbs registry  (~200 verbs; editor/text verbs go to the Host stub)
        ├── Expr evaluator  (SET / IF / WHILE conditions)
        ├── Scope/Env       (globals, LOCAL frames, real arrays/maps)
        ├── Specials         (read-only live state, served by the Host)
        └── Host trait      (LOADKBD, text ops, $SC, shell, clock — Stage-2-stubbed)
```

The win of the AST→IR shape over QN's line/PC machine: **block structure and
labels are resolved once, at lowering time, into integer jump targets.** `GOTO`,
`BREAK`, `CONTINUE`, and `ELSE`/`ELSEIF` become plain jumps; there is no runtime
block stack to mis-pop, no physical-line pre-scan, and none of QN's `;`-chain
footguns. There is no `after_idle` loop — `GOTO` re-iteration is just the
instruction pointer moving backward.

---

## 2. Crate / workspace layout

Make the repo a Cargo **workspace** (the existing `xyscript-kbd` becomes one
member). The interpreter is its own library crate so it builds and tests with no
keyboard/OS dependencies.

```
xyscript/                      (workspace root)
├── Cargo.toml                 (workspace)
├── xyscript-kbd/              (existing keyboard layer; produces XPL strings)
├── xyscript-xpl/              (NEW — the interpreter; this plan)
│   ├── src/
│   │   ├── lib.rs             public API: Interpreter, run(), Host trait re-export
│   │   ├── splitter.rs        ';' splitting, \; handling, exempt verbs
│   │   ├── lexer.rs           tokens; token-level $-substitution hooks
│   │   ├── ast.rs             AST node definitions
│   │   ├── parser.rs          tokens → AST (recursive descent)
│   │   ├── ir.rs              Instr enum + Program(Vec<Instr>) + lowering
│   │   ├── value.rs           Value enum + coercion/comparison rules
│   │   ├── expr.rs            expression grammar evaluator + string functions
│   │   ├── subst.rs           variable substitution model ($NAME, ${}, VAR/SEL)
│   │   ├── env.rs             scopes, LOCAL save/restore, arrays/maps
│   │   ├── verbs/             dispatch registry
│   │   │   ├── mod.rs         Verb trait, registry, case-insensitive lookup
│   │   │   ├── core.rs        SET/SV/LOCAL/CLEAR/IF.../WHILE.../GOTO/SUB/CALL...
│   │   │   ├── flow.rs        block-control verbs lowered at parse time
│   │   │   └── editor_stub.rs editor/text verbs → Host (stubbed in Stage 1)
│   │   ├── specials.rs        special-variable names + Host-served resolution
│   │   ├── library.rs         .qnu loader, shadow-rejection, lib merge
│   │   ├── error.rs           Trappable vs Fatal error model
│   │   └── host.rs            Host trait (the seam to editor/OS/keyboard)
│   └── tests/                 headless language conformance suite
└── xyscript-app/              (LATER — wires kbd resolver + xpl + text engine)
```

**Dependency direction (no cycles).** `xyscript-xpl` depends on **nothing** from
`xyscript-kbd`. The seam is a **trait** (`Host`, §9): the interpreter calls *out*
through it for `LOADKBD`, editor ops, specials, shell, and clock. The app crate
implements `Host` and owns the wiring; tests implement a `StubHost`. This is what
keeps Stage 1 fully headless.

---

## 3. Value model (a deliberate superset choice)

QN uses one stringly-typed slot. XyScript starts with a **real tagged value**,
*preserving QN's observable behavior* while removing the stringly-typed ceiling:

```rust
enum Value {
    Str(String),
    Num(f64),            // or a Number(i64|f64) split if integer-exactness matters
    Bool(bool),          // internal; comparisons still surface QN truthiness
    Array(Vec<Value>),   // real arrays (replaces the $arr_0 / $name_0 convention)
    Map(BTreeMap<String, Value>),
}
```

**Must-preserve observable rules (the floor):**

- **Comparison is numeric-first, then string.** `10 < 9` is false; `"10" < "9"`
  is true. Implement by attempting numeric coercion of both operands; fall back
  to string compare if either isn't numeric. (Quoting forces string.)
- **Case-insensitive identifiers, case-preserving values.** Verbs, variable
  names, function names, labels, and sub names fold to upper for lookup; stored
  string *values* keep their original case. Centralize the fold in one helper.
- **Truthiness** matches QN (`IF` treats empty/"0" as false per QN's rules —
  confirm exact predicate against QN before finalizing).

**Superset latitude (decide at foundation, see §12):** arrays/maps as first-class
`Value` variants from day one. The `$arr_0`/`$arr[i]` naming convention is *not*
reproduced; instead expose array/map syntax and keep a thin compatibility shim
only if a real script needs it (none in QN2 do).

---

## 4. Splitter (`splitter.rs`)

Mirrors QN's chain semantics exactly — this is user-facing contract:

- Split a line on `;` into commands.
- `\;` → a literal `;` (not a separator). `\\` → `\`.
- **Windows-path carve-out:** `\;` followed by whitespace or end-of-string is
  *not* an escape (so `CD D:\; DIR` parses sensibly). Whitespace-gated, exactly
  as QN documents.
- **Exempt verbs consume the rest of the line verbatim** (no further splitting or
  `\;` handling): the `LOADKEY` family in QN. In XyScript the retired
  `LOADKEY`/`.qnk` verbs are gone; carry the *mechanism* for the verbs that need
  raw tails (e.g. `XY` shell pass-through). Keep the exempt set tiny and explicit.

The splitter runs **before** lexing/substitution, per the QN order, but note: a
block-form construct must not be split across `;` (the old footgun). Under the
AST model this is a **non-issue** — the parser sees structured blocks and a
`;`-chained `IF ... ; ... ; ENDIF` simply parses as a one-line `if` with those
commands as its body. We *remove* the footgun rather than reproduce it.

---

## 5. Lexer + substitution (`lexer.rs`, `subst.rs`)

**Token-level substitution (a deliberate superset choice).** QN pastes `$NAME`
in as raw text *before* tokenizing — the source of its quoting/escaping rules.
XyScript instead resolves variables at the **token** level: `$NAME` and `${EXPR}`
become value-bearing tokens, so quoting rules fall out of the grammar instead of
from textual splicing.

- Provide the function-call equivalents `VAR(name)` (safe lookup) and `SEL()`
  (live selection) — these must exist because scripts use them; `SEL()` and `$SE`
  both surface the selection (served by the Host).
- Preserve the *observable* result of substitution (what a script sees in a
  string), while dropping the textual-pre-pass mechanism. Where a quoting nuance
  is user-visible, match QN; where it was an artifact of textual splicing, the
  token model is allowed to be cleaner. Record any intentional difference.

---

## 6. AST (`ast.rs`)

Structured nodes — no physical-line semantics:

```
Program        = Vec<Stmt>
Stmt           = Command { verb, raw_args, span }
               | If { cond, then: Vec<Stmt>, elifs: Vec<(Expr, Vec<Stmt>)>, els: Option<Vec<Stmt>> }
               | While { cond, body: Vec<Stmt> }
               | Sub { name, body: Vec<Stmt> }
               | Label(name)
               | Goto(name)
               | Break | Continue | Return(Option<Expr>) | Exit | ExitAll
               | Set { name, expr: Expr }      // SET / SV
               | Local(Vec<name>) | Clear(Vec<name>)
Expr           = (the §7 grammar)
```

Notes:

- `IF cond <cmd>` (single-line) and the block form are **both** just `If` nodes;
  the distinction that mattered in QN (push a frame or not) disappears.
- Verb arguments stay **raw text** (`raw_args`) except inside `SET`/`IF`/`WHILE`,
  matching QN: only those three contexts evaluate the expression sub-language;
  everywhere else args are passed to the verb as text (after substitution).
- Keep a `span` (source offsets) on every node for good error messages — the
  AST model lets us report structural errors at parse time (e.g. `ENDIF` with no
  `IF`, unbalanced `SUB`) instead of QN's runtime stack-underflow surfacing.

---

## 7. Expression evaluator (`expr.rs`)

Implement QN's grammar exactly (this is the contract for `SET`/`IF`/`WHILE`):

```
expr   := or_e
or_e   := and_e ('OR'  and_e)*
and_e  := not_e ('AND' not_e)*
not_e  := 'NOT' not_e | comp_e
comp_e := add_e (('==' | '!=' | '<' | '>' | '<=' | '>=') add_e)?
add_e  := mul_e (('+' | '-' | '.') mul_e)*        // '.' = string concat
mul_e  := unary (('*' | '/' | '%') unary)*
unary  := '-' unary | atom
atom   := NUMBER | STRING | '(' expr ')' | NAME '(' args ')' | BARE
```

**String functions (the floor set — replicate names + arity):**
`LEN, UPPER, LOWER, STRIP, LSTRIP, RSTRIP, TRIMLEFT(s,n), TRIMRIGHT(s,n),
SUBSTR(s,start,len), INDEX(s,sub), CONTAINS(s,sub), REPLACE(s,from,to),
SEL()`, `VAR(name)`, `CHR(n)` (clamps 0..255, "" outside), `ASC(s)` ("0" for
empty). Division/modulo by zero is a **fatal** error (§8).

**Fresh-slate cleanup to decide (see §12):** QN wraps `+ - * / %` in
try/rollback so junk like `IF $a + GOTO done` stops the expression and treats the
rest as a sub-command. That hack exists because QN can't cleanly separate
"expression" from "trailing command" in its textual model. XyScript's parser
*can* — `IF` takes a delimited condition then a statement/block — so we should
**not** reproduce the rollback; we parse the condition as a full expression and
error clearly on garbage. Flag and confirm no real script depends on the hack.

**Superset backlog (not Stage 1 unless cheap):** user-defined expression
functions; an XyScript-owned (PCRE-class) regex behind a function rather than the
borrowed Tcl/Tk `/R`.

---

## 8. Error model (`error.rs`)

Two tiers, exactly QN's *behavior*:

```rust
enum XplError {
    Trappable(String),   // sets $ERROR = 1; script keeps running
    Fatal(String),       // aborts the whole script; surfaced visibly; untrappable
}
type VmResult<T> = Result<T, XplError>;
```

- **Trappable** (e.g. a verb's soft failure): sets `$ERROR=1`, execution
  continues. `$ERROR` **auto-clears at each action dispatch**, except for an
  explicit exempt-verb set so `<action>; IF $ERROR ...` works. `SETERR`/`CLERR`
  manage it manually.
- **Fatal** (`_QplFatal` in QN): unknown command/label/sub, divide-by-zero, write
  to a read-only special. Aborts the script and is shown to the user. This is
  what keeps a bad key-cell from wedging the input loop.
- **Superset backlog:** *trappable fatals* — a `TRY`/catch mechanism scripts can
  opt into. Design the error enum so this is additive (e.g. a catch boundary in
  the executor), but don't build it in Stage 1.

---

## 9. The Host seam (`host.rs`) — keyboard layer & editor

The interpreter never touches the OS, the keyboard crate, or a GUI directly. It
calls a trait the app implements (and tests stub):

```rust
trait Host {
    // Keyboard-layer seam (design §10.1/§10.2/§10.3)
    fn load_kbd(&mut self, path: &str) -> VmResult<()>;   // LOADKBD → transactional loader
    fn last_scan_code(&self) -> i64;                      // $SC / READKEY
    // Editor/text ops — STUBBED in Stage 1 (record calls), real in Stage 2
    fn editor_op(&mut self, op: EditorOp) -> VmResult<Value>;
    // Live state for specials (§10)
    fn special(&self, name: &str) -> Option<Value>;       // $SE, $CB, $CP, $OS, ...
    // Environment
    fn shell(&mut self, cmd: &str) -> VmResult<(String, i64)>;  // sets $RC/$LS
    fn now(&self) -> (/* date */ String, /* time */ String);
}
```

Key rules from the design spec:

- **`KF` never reaches the interpreter.** It's a reserved input-layer directive
  decided synchronously in the keyboard layer's event callback. The interpreter
  only ever sees ordinary assignment strings. (Keep the reserved set minimal;
  `KF` is the first member.)
- **`LOADKBD` is an ordinary XPL verb** that calls `host.load_kbd()`, which
  invokes the kbd crate's transactional/last-known-good loader. This is the first
  command that closes a loop: a key runs XPL that reloads the keymap.
- **`$SC` is preserved** as the canonical key code queryable from scripts; the
  retired `LOADKEY`/`.qnk` binding verbs are **not** ported.

---

## 10. Variables, scoping, specials (`env.rs`, `specials.rs`)

- **Scope:** script-global by default. `LOCAL name...` saves current values
  (array/map-aware), clears them for the routine's duration, and restores on
  exit — for subroutine and library-routine scope. `CALL`/`SUB`/`RETURN` frames.
- **Positional args:** `$1..$N`, `$#` (count), `$*` (all), and `$RV` (sub return
  value) — served from the call frame.
- **Special (read-only) variables — `SET` refuses to write them.** Reproduce the
  names as the contract; resolve live ones through `Host::special()`. The floor
  set from QN includes (grouped):
  - *file/path:* `FI FP CWD PA ED SF VE QNDIR CF CFL SLASH`
  - *cursor/offset:* `CP CX CY LN OF`
  - *selection:* `SE SL DF DS DN`
  - *editor state/flags:* `FB FE RE MO FS LC WC CC CH TB GC NP MB AU PS VR TC CT TX`
  - *lock keys:* `CL NL RL`
  - *keyboard seam:* `SC` (last scan code)
  - *clock:* `DA TM ET`
  - *shell:* `RC LS`
  - *clipboard:* `CB`
  - *host/UI:* `OS UIFONT UISCALE EDFONT`
  - *error flag:* `ERROR`
  - *args/return:* `* # RV`
  Many of these are editor/Stage-2 state — in Stage 1 the stub `Host` returns
  placeholder values for them, but the **names and the write-protection** are
  enforced now (writing one is a Fatal). Rename `$SC`'s rebranded fork (`QNDIR`
  etc.) to XyScript equivalents as needed — names are a contract, not sacred.

---

## 11. Library system (`library.rs`)

- `.qnu` files: `SUB name ... ENDSUB` routines, each with a `# DOC:` string.
- A bundled **system** library, overlaid by a **user** library; `LOADLIB`,
  `ADDLIB`, `RELOADLIB`, `UNLOADLIB`, `LISTLIB`; **last-wins** merge.
- **Shadow-rejection:** a routine may not collide with a built-in verb or a
  special-variable name — reject at load with a clear error.
- `RUN <script.qns>` executes a script file; `startup.qns` runs at launch (the
  loading model in design §9.4 expects it; full startup wiring is Stage 2).

---

## 12. Foundational superset decisions to lock at the start

These are the §"Where XyScript exceeds QN" items that are cheaper to design in
now than to retrofit. Decide each before/while building the named module:

| Decision | Module | Recommendation |
|---|---|---|
| Real array/map `Value` (vs `$arr_0` convention) | `value.rs`, `env.rs` | **In now** — variants exist from day one. |
| Token-level / `${}` substitution (vs textual pre-pass) | `lexer.rs`, `subst.rs` | **In now** — the model is chosen here regardless. |
| Drop the expr `+-*/%` try/rollback hack | `expr.rs`, `parser.rs` | **In now** — parser makes it unnecessary; cleaner errors. |
| Real number type / integer exactness | `value.rs` | Decide early; affects comparison + arithmetic. |
| Trappable fatals (`TRY`/catch) | `error.rs`, executor | **Backlog** — keep the enum/executor catch-ready, build later. |
| XyScript-owned PCRE regex (vs Tcl/Tk `/R`) | (new) `regex` seam | **Backlog** — Stage 2+, but don't bake in Tcl-isms. |
| User-defined expression functions | `expr.rs` | **Backlog.** |

Hold the line on **scope** (don't let Stage 1 balloon), never on fidelity to QN
internals.

---

## 13. Build & test order (incremental, each step green before the next)

1. **`value.rs` + coercion/comparison** — unit-test numeric-vs-string compare,
   case folding, truthiness against documented QN behavior.
2. **`splitter.rs`** — `;`, `\;`, `\\`, Windows-path carve-out, exempt verbs.
3. **`lexer.rs` + `expr.rs`** — the full grammar + every string function; this is
   where QN's `test_quicknotes.py` leaf cases are a useful behavior reference.
4. **`subst.rs`** — `$NAME`, `${}`, `VAR()`, `SEL()` (SEL via StubHost).
5. **`ast.rs` + `parser.rs`** — parse all control constructs; assert parse-time
   structural errors (unbalanced `IF`/`SUB`).
6. **`ir.rs` + lowering** — blocks/labels → jumps; unit-test that `GOTO`,
   `BREAK`, `CONTINUE`, `ELSE`/`ELSEIF` resolve to correct targets. **Add the
   block-control-flow tests QN never had.**
7. **executor + `env.rs` + `error.rs`** — run IR; `$ERROR` trappable flow; fatal
   aborts; `LOCAL` save/restore; `SUB`/`CALL`/`RETURN`/`$RV`/positional args.
8. **`verbs/` framework + a starter set** — `SET/SV/LOCAL/CLEAR/PR/MSG`, the flow
   verbs, plus editor verbs routed to `StubHost` (record-only).
9. **`library.rs`** — `.qnu` load/merge/shadow-reject; `RUN`.
10. **Seam** — `LOADKBD` verb → `Host::load_kbd`; `$SC`; confirm `KF` never
    arrives. Then the **end-to-end test**: drive a `(bitmask, key_code)` through
    the `xyscript-kbd` resolver, feed the resulting XPL to the interpreter, assert
    the StubHost observed the effect.

Test discipline mirrors the keyboard layer: **headless, no GUI**, XyScript owns
its suite, and control flow gets first-class coverage.

---

## 14. Handoff checklist for Claude Code (on-device)

- [x] Convert repo to a Cargo workspace; add `xyscript-xpl` member. *(done
      2026-06-07 — root `Cargo.toml` workspace; `xyscript-kbd` + `xyscript-xpl`
      members; release profile hoisted to root.)*
- [~] Implement modules in the §13 order; keep each step's tests green. *(steps
      1–3 done + token-level `$` foundation; see progress log below.)*
- [ ] `StubHost` in `tests/` for headless runs; real `Host` deferred to the app.
- [x] Use QN2 (`quicknotes.py` / `test_quicknotes.py` / `qnmanual.md`) only as a
      **behavior reference** for the floor — never copy its structure. *(leaf
      semantics lifted from QN's `_expr_*` and `_split_commands`; oracle values
      cross-checked against `test_quicknotes.py`.)*
- [x] Record each §12 decision (and any intentional QN-behavior divergence) —
      see the progress log below.

---

## 15. Implementation progress log & recorded decisions

*Living record, updated as modules land. Mirrors the §13 build order.*

### Status (2026-06-07, updated — Stage-1 core COMPLETE)

Workspace + interpreter crate stood up; **all §13 steps 1–10 complete** — the
entire Stage-1 language core. All headless, **122 tests green** (120 unit + 2
integration: `cargo test -p xyscript-xpl`; full workspace 137 incl. kbd).
Toolchain on device: Rust 1.86. **The Stage-1 "definition of done" is met** —
the Appendix A.3 north-star passes: a key-shaped event drives the real
`xyscript-kbd` resolver → `Dispatch(XPL)` → interpreter → observable host
effect, with `KF` handled in the layer and never reaching the interpreter.

| §13 step | Module(s) | State |
|---|---|---|
| 1 value model + coercion/comparison | `value.rs` | ✅ done |
| 2 splitter | `splitter.rs` | ✅ done |
| 3 lexer + expression evaluator + string fns | `lexer.rs`, `expr.rs` | ✅ done |
| 4 substitution (`$NAME`/`${}`/`VAR`/`SEL`) | `lexer.rs` (token Var) + `subst.rs` | ✅ done |
| 5 AST + parser | `ast.rs`, `parser.rs` | ✅ done |
| 6 IR + lowering (blocks/labels → jumps) | `ir.rs` | ✅ done |
| 7 executor + env + error model | `vm.rs`, `env.rs`, `error.rs` | ✅ done |
| 8 verbs + starter set | `vm.rs` (inline registry) | ✅ done (starter set; see note) |
| 9 library system (`.qnu`) | `library.rs`, `vm.rs` | ✅ done |
| 10 seam (`LOADKBD`, `$SC`) + end-to-end | `host.rs`, `tests/end_to_end.rs` | ✅ done |

**Library system (step 9) as built.** `.qnu` parsing (strict top-level
`SUB…ENDSUB`, frontmatter tolerated, `# DOC:` captured, atomic on error),
shadow-rejection vs verbs/keywords/specials, system+user layers with
user-over-system, and the verbs `LOADLIB`/`ADDLIB`/`RELOADLIB`/`UNLOADLIB`/
`LISTLIB` + `RUN`. Library routines are **hoisted into the IR as ordinary subs**
(`ir::lower_with`), so `CALL` resolves script subs → user lib → system lib with
no separate invocation machine. File I/O goes through a new `Host::read_file`
(virtual FS in `StubHost`), keeping it headless. **`RUN`** re-enters the executor
(`execute` is a free fn over an `ExecCtx`) sharing env/library/host. **Timing
note (recorded):** a library loaded by `LOADLIB` mid-run takes effect on the
*next* `run()` — the current program was already compiled. **Label scoping fix:**
labels are now resolved **per routine** (`ir::lower_with`), so a library or sub
`:label` never collides with the script's (this also fixed a latent
global-label-collision bug).

**Stage-1 polish — DONE (2026-06-07).** Total **150 tests** (135 xpl: 132 unit +
3 integration; 15 kbd).

- **Arrays surface.** `SPLIT(s,sep)` / `SPLITS(s,charset)` / `JOIN(arr,sep)`
  expression functions returning real `Value::Array`s (QN had these as SET-RHS
  special forms; XyScript makes them ordinary functions); `LEN` extended to count
  array/map elements; `[]` element access in expressions (`parse_postfix`,
  chainable) and in substitution (`$arr[N]` / `$arr[$var]`); and `SET arr[i] = v`
  assignment (`Stmt::SetIndex` → `Instr::SetIndexVar`) with auto-extend and
  auto-vivify. **Maps** get a surface too: `SET m[key] = v` creates/updates a
  `Value::Map` keyed by the index's string form. **Recorded divergences:** arrays
  are **0-based** (QN's `$arr_N` convention was **1-based**), out-of-range / missing
  reads yield the **empty string** (the "undefined → empty" convention), and
  indexing a scalar/number is a fatal error. *(There is still no map/array
  **literal** syntax — a map is only created via `SET m[k]=v` — backlog.)*
- **Sample content authored in real XPL.** `kbd/xyscript-sample.kbd` rewritten so
  every cell is runnable XPL (letter cells `insert "x"`, command cells `call
  <routine>`, KF cells, `loadkbd <path>`); bundled `xyscript-xpl/scripts/system.qnu`
  defines the called routines (Stage-1 PR stubs with `# DOC:`). New integration
  test loads both through the **real kbd loader + resolver** and drives a
  keystroke → `Dispatch(XPL)` → interpreter → library routine, with `KF` released.
- **`$SC` wired** to the dedicated `Host::last_scan_code()` (not the generic
  `special()` map). **`INSERT`** now unwraps a single surrounding quoted string
  literal (`insert "q"` → `q`, `insert " "` → space) — *provisional*; the full
  editor typing model (terse self-inserting cells) is a Stage 2 concern.

**Still deferred (genuinely Stage 2 / backlog).** `READKEY` (needs the Stage 2
input loop to block for a key; `$SC` already works); map/array **literal** syntax;
the terse self-inserting-letter KBD convention. None gate the Stage-1 exit
criteria, which are met. **Stage 1 is complete.**

**Module map as built** (differs slightly from §2's sketch): the executor lives
in `vm.rs` (not split into `verbs/`); the starter verb set is an inline match in
`vm.rs::exec_command` rather than a `verbs/` registry — a `Verb` trait/registry
refactor is deferred until the verb count grows enough to need it (scope
discipline). `host.rs` provides both the `Host` trait and a `StubHost`.

**What runs today** (covered by tests): `SET` (with the full expression
sub-language), `IF`/`IFNOT`/`ELSE`/`ELSEIF` (single-line *and* block, incl. the
`;`-chained block that was QN's footgun), `WHILE`/`BREAK`/`CONTINUE`,
`GOTO`+`:labels` (forward and back), `SUB`/`CALL`/`RETURN` with positional args
(`$1..$N`, `$#`, `$*`) and `$RV`, `LOCAL` save/restore + `CLEAR`, the two-tier
error model (`$ERROR` trappable flow with action-verb auto-clear; `SETERR`/
`CLERR`; fatal aborts), and the host-seam verbs `LOADKBD`/`INSERT`/`SV` plus
`PR`/`MSG` output and host-served specials (`$OS`, `$SE`, `SEL()`, …).

**Step-9 scope (next session).** `.qnu` files of `SUB…ENDSUB` with `# DOC:`
strings; bundled **system** lib overlaid by a **user** lib; `LOADLIB`/`ADDLIB`/
`RELOADLIB`/`UNLOADLIB`/`LISTLIB`; last-wins merge; **shadow-rejection** (a
routine may not collide with a built-in verb or special-var name); `RUN
<script.qns>`; `startup.qns` wiring is Stage 2. The executor already hoists subs
and resolves `CALL` by name through `Program::subs`, so the library system mainly
needs a loader that parses `.qnu` into subs and merges them into the callable
table — plus the shadow-reject check against the verb/special name sets.

### Decisions & divergences recorded for steps 4–8/10

- **`subst.rs` scope (step 4).** Verb-argument text substitution is *only* for
  non-expression verbs; `SET`/`IF`/`WHILE` resolve `$NAME` at the token level
  (lexer `Var`). Consequences, all recorded in `subst.rs`: **`${}` is supported**
  (QN doesn't), **no in-string `"`/`\` escaping** of substituted values (a QN
  re-lex artifact that the token model makes unnecessary), and **no
  `$NAME[idx]`→`$NAME_idx` array sugar** (real arrays instead).
- **`SV` is a selection verb, not a `SET` variant.** `SV foo` ≡ `SET foo =
  SEL()` (saves the selection to a var). So only `SET` is a dedicated AST/IR node;
  `SV` and `CALL` are `Command`s. (The §6 AST comment "`Set { } // SET / SV`" was
  corrected against QN source.)
- **Subs are hoisted, `CALL` resolves by name at run time (step 6/7).** `SUB`
  bodies are emitted after the main program (which ends in `Exit`), recorded in
  `Program::subs`; `CALL` pushes a frame and jumps to the entry. This supports
  dynamic call targets and matches QN's name-keyed sub table.
- **GOTO/BREAK/CONTINUE are lowered to integer jumps (step 6).** Labels are
  script-global; unknown `GOTO` label, `BREAK`/`CONTINUE` outside a loop, and
  duplicate `SUB`/label are **fatal** errors caught at lowering time (earlier than
  QN's runtime detection — strictly better, same tier).
- **`$ERROR` model (step 7).** Action verbs auto-clear `$ERROR` on entry; an
  exempt set (`NO_ERROR_TOUCH` in `vm.rs` — `CALL`/`SETERR`/`CLERR`/test+info
  verbs) does not. A host op returning `Trappable` folds into `$ERROR=1` and
  continues; `Fatal` unwinds. `SET`/flow are IR nodes (not `Command`s) so they
  never touch `$ERROR` — consistent with QN's no-touch list.
- **Variable resolution precedence (step 7).** Built-in specials (`$ERROR`,
  `$#`, `$*`, `$RV`, positional `$1`…) → host specials (`$SE`, `$OS`, …) → user
  vars. `SET`/`SV` to any special name (incl. positional/`*`/`#`) is fatal
  (write-protection enforced now, per §10).
- **Numbers compare exactly when both are `Int`** (`xpl_cmp`), else via `f64` —
  a refinement of the §3 model now that the `Int`/`Float` split exists.

### §12 foundational decisions — as built

- **Real array/map `Value`** → **IN.** `Value::{Str,Int,Float,Bool,Array,Map}`
  from day one (`value.rs`). The `$arr_0`/`$name_0` convention is *not*
  reproduced.
- **Token-level / `${}` substitution** → **IN.** The lexer emits a `Var` token
  for `$NAME` / `${NAME}` / `$1` / `$*` / `$#`, resolved to a typed value at eval
  time (`lexer.rs`, `expr.rs`). The textual `$`-pre-pass is dropped. *(QN does
  **not** support `${}`; XyScript adds it. Arbitrary `${EXPR}` inside braces is
  backlog — only a delimited variable **name** is supported today.)*
- **Drop the expr `+-*/%` try/rollback hack** → **IN.** The recursive-descent
  parser parses a full expression and errors clearly on garbage; the IF/condition
  boundary will be owned by the structured statement parser (step 5), so the
  rollback is unnecessary (`expr.rs`).
- **Real number type / integer exactness** → **IN (decided early, as the table
  advised).** `value.rs` carries a `Number::{Int(i64),Float(f64)}` through all
  arithmetic. This is *required* for fidelity: the oracle pins `9 / 2 == 4`
  (integer floor division) but `9.0 / 2 == 4.5` (float). Floor division and
  Python-style modulo (sign-of-divisor) are implemented and tested.
- **Trappable fatals / owned regex / user-defined fns** → **BACKLOG**, untouched
  (as planned).

### Intentional QN-behavior divergences (the §14 mandate)

1. **Comparison is type-aware — "quoting forces string."** Both design docs
   specify `10 < 9` false and `"10" < "9"` **true**. QN's actual `_expr_compare`
   coerces *both* operands to numbers whenever both are numeric-coercible, so in
   real QN `"10" < "9"` is *false*. XyScript's real type system lets the value's
   **type** carry the intent: both numeric-typed → numeric; both `Str` → string;
   mixed → numeric-first (numeric iff both coerce, else string). This honors the
   documented intent and is the cleaner model. (`value.rs::xpl_cmp`.) **Equality
   (`==`/`!=`) follows the same rule**, so `10 == "10"` is true (mixed→numeric)
   but `"10" == "10.0"` is false (both string-typed). Recorded as the one
   semantic the docs assert about QN that QN does not actually exhibit.
2. **`${}` brace substitution** exists in XyScript, not QN (see above).
3. **No `;`-chain block footgun** — a `;`-chained `IF … ; … ; ENDIF` will parse
   as one structured block (the splitter has no block awareness; the parser folds
   it). This is the already-committed AST-rebuild consequence
   (`stage1-interpreter-shape.md`); the splitter is written with no block
   knowledge in anticipation of it.

### Faithful ports worth noting (so they are not re-litigated)

- **Splitter** (`splitter.rs`): `;` split with `\;`/`\\`/`\"`/`\#` unescaping,
  quoted-string opacity, the Windows-path `\;<ws|EOS>` carve-out, inline `#`
  comments (whitespace-gated), and exempt verbs that swallow the tail verbatim.
  Exempt set is **`X`/`XY` only** — QN's `LOADKEY` family is retired in XyScript,
  so it is intentionally not exempt. (Cross-checked against
  `test_quicknotes.py`'s `_split_commands` cases.)
- **Expression grammar** (`expr.rs`): the full `or→and→not→comp→add→mul→unary→
  atom` grammar, single (non-associative) comparison, `.` concat, unary minus,
  the function-call **adjacency rule** (`NAME(` is a call, `NAME (` is not), and
  all 16 string functions (`LEN UPPER LOWER STRIP LSTRIP RSTRIP TRIMLEFT
  TRIMRIGHT SUBSTR INDEX CONTAINS REPLACE SEL VAR CHR ASC`) with QN's arities and
  edge behavior (CHR clamps 0..255→"", ASC of "" →"0", SUBSTR/ TRIM clamps).
- **String escapes** in the lexer: `\n \t \r \0 \xHH \dNNN \uHHHH \\ \"`, unknown
  → verbatim `\X` (QN `_expr_tokenize`).
- **Logical/comparison ops do not short-circuit** — both operands are always
  evaluated, so a fatal in an un-taken branch still fires (matches QN).

### Next concrete step

`subst.rs` — verb-argument text substitution (QN `_substitute_vars`):
`$NAME`/`$1`/`$*`/`$#` → text, `\$` → literal `$`, undefined → `""`, and the
in-string-literal `\`/`"` escaping QN does so a `$SE` value can't break out of a
surrounding quote. Then §13 steps 5–10 (AST → parser → IR → executor → verbs →
library → seam), ending at the Appendix A.3 north-star end-to-end test.

---

## Appendix A — `StubHost` and the first end-to-end target test

*Concrete starting targets for the on-device session. Code is illustrative Rust:
exact constructor/method names should follow whatever `xyscript-kbd` and
`xyscript-xpl` actually expose, but the shapes and the assertions are the
intended contract. Two targets are given: the **north-star** acceptance test
(end-to-end, the §13 step-10 goal) and the **first green milestone** (the very
first PR's target).*

### A.1 Host-side types (`host.rs`)

```rust
/// Editor/text operations the interpreter can request. In Stage 1 these are
/// recorded by the StubHost, not executed; Stage 2 backs them with the real
/// text engine. The set grows as editor verbs are registered (editor_stub.rs).
#[derive(Debug, Clone, PartialEq)]
pub enum EditorOp {
    InsertText(String),
    MoveCursor(i64),
    DeleteSelection,
    Search { pattern: String, flags: String },
}

pub trait Host {
    // Keyboard-layer seam (design §10.1–§10.3)
    fn load_kbd(&mut self, path: &str) -> VmResult<()>;   // LOADKBD verb
    fn last_scan_code(&self) -> i64;                      // $SC / READKEY
    // Editor/text ops — STUBBED in Stage 1, real in Stage 2
    fn editor_op(&mut self, op: EditorOp) -> VmResult<Value>;
    // Live read-only state for specials ($SE, $CB, $OS, ...)
    fn special(&self, name: &str) -> Option<Value>;
    // Environment
    fn shell(&mut self, cmd: &str) -> VmResult<(String, i64)>;  // sets $RC/$LS
    fn now(&self) -> (String, String);                          // ($DA, $TM)
}
```

### A.2 `StubHost` (test support — `tests/support/mod.rs`)

```rust
use std::collections::HashMap;
use xyscript_xpl::{Host, EditorOp, Value, VmResult};

/// Records everything a script does, so tests assert on observable effects
/// without a GUI or OS. `&mut self` methods push to `log`; read-only `special`
/// reads from a preloaded map.
#[derive(Default)]
pub struct StubHost {
    pub log: Vec<String>,
    pub loaded_kbd: Option<String>,
    pub last_scan: i64,
    pub specials: HashMap<String, String>,
}

impl Host for StubHost {
    fn load_kbd(&mut self, path: &str) -> VmResult<()> {
        self.log.push(format!("LOADKBD {path}"));
        self.loaded_kbd = Some(path.to_string());
        Ok(())
    }
    fn last_scan_code(&self) -> i64 { self.last_scan }

    fn editor_op(&mut self, op: EditorOp) -> VmResult<Value> {
        self.log.push(format!("{op:?}"));
        Ok(Value::Str(String::new()))
    }
    fn special(&self, name: &str) -> Option<Value> {
        self.specials.get(name).map(|v| Value::Str(v.clone()))
    }
    fn shell(&mut self, cmd: &str) -> VmResult<(String, i64)> {
        self.log.push(format!("SHELL {cmd}"));
        Ok((String::new(), 0))
    }
    fn now(&self) -> (String, String) {
        ("2026-01-01".into(), "00:00".into())
    }
}
```

### A.3 North-star test — keystroke → XPL → interpreter → host (`tests/end_to_end.rs`)

This is the §13 step-10 acceptance test: it crosses the seam from the existing
keyboard crate into the interpreter and asserts the script's effects landed on
the host.

```rust
use xyscript_kbd::{KeyMap, Assignment};   // existing keyboard layer
use xyscript_xpl::Interpreter;
mod support;
use support::StubHost;

#[test]
fn keystroke_runs_xpl_and_reaches_host() {
    // A tiny keymap:
    //   plain 'q' (bitmask 0, key 16) -> "INSERT q"   (a stubbed editor verb)
    //   Ctrl+L   (bitmask 256, key 38) -> "LOADKBD layout2.kbd"
    // (Build the KeyMap with whatever constructor the kbd crate exposes, or
    //  load kbd/xyscript-sample.kbd; the (bitmask,key) inputs match its tests.)
    let km = KeyMap::from_pairs([
        ((0u32, 16u16), Assignment::Xpl("INSERT q".into())),
        ((256,   38),   Assignment::Xpl("LOADKBD layout2.kbd".into())),
    ]);

    let mut host  = StubHost::default();
    let mut interp = Interpreter::new();

    // Press 'q', then Ctrl+L: resolve -> dispatch string -> interpret.
    for (mask, key) in [(0u32, 16u16), (256, 38)] {
        if let Some(Assignment::Xpl(code)) = km.resolve(mask, key) {
            interp.run(code, &mut host).expect("script ran");
        }
    }

    // The script's effects are observable on the host, in order.
    assert_eq!(host.log, vec![
        r#"InsertText("q")"#.to_string(),   // "INSERT q" -> EditorOp::InsertText
        "LOADKBD layout2.kbd".to_string(),  // LOADKBD verb -> host.load_kbd
    ]);
    assert_eq!(host.loaded_kbd.as_deref(), Some("layout2.kbd"));
}
```

What this proves end-to-end: the resolver hands out an opaque XPL string (`KF` is
never seen here — it's handled in the kbd event callback); the interpreter parses
and runs it; a letter-cell reaches a (stubbed) editor op and a command-cell
reaches `LOADKBD` through the host seam. That is the Stage 1 "definition of done"
in one test.

### A.4 First green milestone (the very first PR)

The north-star test needs most modules. The *first* concrete target is far
smaller — `value.rs` coercion/comparison, which everything else builds on:

```rust
// xyscript-xpl/src/value.rs  (unit tests)
#[test]
fn numeric_then_string_comparison() {
    assert!( Value::lt(&Value::num(10.0), &Value::num(9.0)) == false); // numeric
    assert!( Value::lt(&Value::str("10"),  &Value::str("9")) == true ); // string
}
#[test]
fn identifiers_fold_case_values_do_not() {
    assert_eq!(fold_ident("SetVar"), "SETVAR");      // verbs/vars/labels fold
    assert_eq!(Value::str("KeepCase").as_str(), "KeepCase"); // values preserved
}
```

Build order from here is §13: once `value.rs` is green, add `splitter.rs`, then
`expr.rs` (where QN's `test_quicknotes.py` leaf cases are a useful behavior
reference), and work up to the north-star test at step 10.
