The user dismissed the questions — I'll wait for your direction before building.

When you're ready, here's the short version of what I found so you can decide:

- **The VM is well-suited to a continuation approach.** A running script's entire live state is `(program, ip, frames, rv)` — all owned/clonable — and env/library/output already persist across runs. `READKEY`/`INPUT` would park that state, hand control back to the GUI to capture a key/line, then `resume()` rebuilds the VM and injects the value. No threads.
- **One boundary:** `CALL`/`SUB`/loops/`IF` all suspend for free (same VM), but a nested `RUN`-ed file sits on the native Rust stack — so suspending *inside* a `RUN` would have to be a trappable error rather than work. The alternative (thread-based interpreter) removes that limitation but forces `Arc<Mutex>` sharing of the document and a major rework of the single-threaded GUI↔engine model.
- **Surface is already specced** in the parity plan: `READKEY <var>` → `$<var>` (+`$SC`), `INPUT <var>` → `$<var>`, modal entry.

Let me know how you'd like to proceed.

Good question to settle before deciding — it's actually the crux. Let me separate two things that "AST model" could mean, because the current code has both layers and they factor into the decision very differently.

## What's actually built today

XyScript scripting runs through a **two-stage pipeline**, and that was the committed design from the stage-1 interpreter plan:

1. **Front-end: source → AST.** [parser.rs](xyscript-xpl/src/parser.rs) produces an `ast::Program` ([ast.rs](xyscript-xpl/src/ast.rs)), and expressions are kept as trees (`Expr`) evaluated by a tree-walker in [expr.rs](xyscript-xpl/src/expr.rs).
2. **Execution: AST → flat IR.** [ir.rs](xyscript-xpl/src/ir.rs) *lowers* the AST into a flat instruction vector (`Vec<Instr>`) with integer jump targets and a sub-name→entry map. The VM ([vm.rs](xyscript-xpl/src/vm.rs)) then executes that IR with a plain instruction pointer — `while ip < code.len()`. It does **not** tree-walk the AST at runtime.

So the AST is very much still on the table — it's the parsing/representation layer, and expression evaluation is still a tree-walk. What we deliberately *don't* do is interpret control flow (IF/WHILE/GOTO/CALL) by recursively walking AST nodes at runtime. The ir.rs header spells out why: doing it this way means "block structure and labels are resolved once, at lowering time, into integer jump targets — no runtime block stack to mis-pop, no physical-line pre-scan, none of QN's `;`-chain footguns."

## How that factors into the suspend/resume decision

This is the part that matters for your choice, and it's a direct consequence of the lowering decision:

- **If we still tree-walked the AST at runtime** (recursive descent over `If`/`While`/block nodes), a script's execution position would live on the **native Rust call stack** — each nested block is a nested Rust function call. You can't serialize or park a native Rust stack. That would essentially *force* the thread-based approach (Option B), because blocking a thread is the only way to "freeze" a native stack.

- **Because we lowered to flat IR**, control flow is just `ip` movement over a vector. The entire live state of a paused script collapses to `(ip, frames, rv)` — all owned, all clonable. *That* is exactly what makes the clean single-VM continuation (Option A) possible. The lowering decision and the continuation option are the same architectural bet paying off twice.

- **Expression tree-walking is irrelevant to suspend**, because a suspend point only ever falls *between* IR instructions. An `Expr` evaluates atomically inside one instruction; READKEY/INPUT are statement-level commands, so no pause can occur mid-expression. The AST-tree part never spans a suspend boundary.

- **The one nested-RUN limitation maps precisely onto the one place we did *not* flatten.** A nested `RUN <file>` recurses into a second `execute()` — native Rust recursion — so it's the lone bit of runtime state that *isn't* in the flat IP/frames model. That's why, under Option A, suspending inside a RUN-ed file would be a trappable error. If you ever wanted suspend-to-cross-RUN, the AST-faithful fix isn't threads — it's flattening that last recursion too (turn nested RUN into an explicit VM stack the run loop iterates, rather than a Rust call). That's a clean future extension of the *same* IR model, not a departure from it.

**Bottom line:** the AST model is intact and unchanged by this decision. The continuation approach (Option A) is the natural continuation of the original AST→flat-IR design — it leverages exactly the property that lowering bought us. The thread approach would actually be the bigger philosophical break, since its whole appeal is "don't touch the VM, let a native stack hold the state." Nothing here forces you off the AST model either way.

Want me to go ahead on that basis, or is there another angle you want to probe first?