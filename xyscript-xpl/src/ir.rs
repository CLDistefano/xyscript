//! Flat IR + lowering (`stage1-interpreter-plan.md` §1, §6, §13.6).
//!
//! The committed shape's payoff lives here: **block structure and labels are
//! resolved once, at lowering time, into integer jump targets.** `GOTO`,
//! `BREAK`, `CONTINUE`, and `ELSE`/`ELSEIF` all become plain [`Instr::Jump`] /
//! [`Instr::JumpUnless`] — there is no runtime block stack to mis-pop, no
//! physical-line pre-scan, and none of QN's `;`-chain footguns. `GOTO`
//! re-iteration is just the instruction pointer moving backward.
//!
//! Subroutines are **hoisted**: a `SUB`'s body is emitted *after* the main
//! program (which ends in [`Instr::Exit`]), so normal top-level flow never falls
//! into it. `CALL` (a `Command` the VM handles) looks the entry up in
//! [`Program::subs`] at run time and pushes a return frame; the hoisted body
//! ends in an implicit `RETURN`.

use crate::ast::{self, ForKind, Span, Stmt};
use crate::expr::{BinOp, Expr};
use crate::value::{fold_ident, Number};
use std::collections::HashMap;

/// A single IR instruction.
#[derive(Debug, Clone, PartialEq)]
pub enum Instr {
    /// Dispatch a verb with its raw (un-substituted) argument tail.
    Command {
        verb: String,
        raw_args: String,
        span: Span,
    },
    /// Evaluate `expr` and assign to the (folded) user variable `name`.
    SetVar { name: String, expr: Expr, span: Span },
    /// Evaluate `index` and `expr` and assign into element `name[index]`.
    SetIndexVar {
        name: String,
        index: Expr,
        expr: Expr,
        span: Span,
    },
    /// Evaluate `cond`; if **falsy**, jump to `target`. (IF/ELSEIF/WHILE tests.)
    JumpUnless { cond: Expr, target: usize, span: Span },
    /// Unconditional jump (GOTO, BREAK, CONTINUE, end-of-arm, loop-back).
    Jump(usize),
    /// `LOCAL name…` — save+clear names for the current frame.
    Local { names: Vec<String>, span: Span },
    /// `CLEAR name…`.
    Clear { names: Vec<String>, span: Span },
    /// `RETURN [expr]` — leave the current sub, optionally setting `$RV`.
    Return { value: Option<Expr>, span: Span },
    /// End the current script.
    Exit,
    /// End every running script (single-script here, so same as `Exit`).
    ExitAll,
    /// `CAPTURE var` — begin redirecting command output into a buffer (pushed on a stack so
    /// captures nest). Paired with the next `CaptureEnd`.
    CaptureBegin { var: String, span: Span },
    /// `ENDCAPTURE` — pop the capture buffer and assign it to its variable.
    CaptureEnd,
}

/// A lowered program: a flat instruction vector plus the sub-name → entry map.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub code: Vec<Instr>,
    pub subs: HashMap<String, usize>,
}

/// A lowering error (surfaces as a fatal: unknown GOTO label, BREAK/CONTINUE
/// outside a loop, duplicate sub).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LowerError {
    pub msg: String,
    pub line: usize,
}

impl LowerError {
    fn new(msg: impl Into<String>, line: usize) -> LowerError {
        LowerError { msg: msg.into(), line }
    }
}

/// Lower a parsed AST into flat IR (no external library routines).
pub fn lower(program: &ast::Program) -> Result<Program, LowerError> {
    lower_with(program, &[])
}

/// Lower a parsed AST, additionally hoisting `extra_subs` (library routines) as
/// callable subs. A script's own `SUB` of the same name **wins** (the extra is
/// skipped), and `extra_subs` should be ordered so higher-precedence routines
/// (user library) come before lower (system library) — the first definition of a
/// name wins. Each routine gets its **own label scope** (`GOTO`/`:label` resolve
/// within a routine, never across routines).
pub fn lower_with(
    program: &ast::Program,
    extra_subs: &[(String, Vec<Stmt>)],
) -> Result<Program, LowerError> {
    let mut l = Lowerer::default();
    // Main program is one routine scope; it ends in Exit so flow never falls
    // into the hoisted sub bodies that follow.
    l.lower_routine(&program.stmts)?;
    l.code.push(Instr::Exit);
    l.drain_deferred_subs()?;
    // Library / external routines: only names the script didn't already define.
    for (name, body) in extra_subs {
        if l.subs.contains_key(name) {
            continue;
        }
        let entry = l.code.len();
        l.subs.insert(name.clone(), entry);
        l.lower_routine(body)?;
        l.code.push(Instr::Return { value: None, span: Span::new(0) });
        l.drain_deferred_subs()?;
    }
    Ok(Program { code: l.code, subs: l.subs })
}

#[derive(Default)]
struct Lowerer {
    code: Vec<Instr>,
    labels: HashMap<String, usize>,
    /// (instruction index of the placeholder Jump, target label, line).
    goto_fixups: Vec<(usize, String, usize)>,
    loops: Vec<LoopCtx>,
    /// Subs to emit after the main program: (name, body, line).
    subs_deferred: Vec<(String, Vec<Stmt>, usize)>,
    subs: HashMap<String, usize>,
    /// Per-`FOR` counter, for unique hidden temp-variable names (`#FOR_END0`, …).
    for_seq: usize,
}

struct LoopCtx {
    /// Indices of placeholder `CONTINUE` jumps, backpatched with the loop's continue point — the
    /// `WHILE` test, or a `FOR`'s increment step (so `CONTINUE` still advances the counter).
    continue_fixups: Vec<usize>,
    /// Indices of placeholder `BREAK` jumps to backpatch with the loop's end.
    break_fixups: Vec<usize>,
}

impl LoopCtx {
    fn new() -> LoopCtx {
        LoopCtx { continue_fixups: Vec::new(), break_fixups: Vec::new() }
    }
}

/// Hidden loop temps are case-folded (uppercased) and use a `#` (not a valid identifier char), so
/// they can never collide with a user variable. Build the step-sign-aware range continue condition:
/// `(step > 0 && v ≤/< end) || (step < 0 && v ≥/> end)` (`<`/`>` when `exclusive`, i.e. `FOR/X`).
fn range_cond(var: &str, end_t: &str, step_t: &str, exclusive: bool) -> Expr {
    let v = || Expr::Var(var.to_string());
    let endv = || Expr::Var(end_t.to_string());
    let stepv = || Expr::Var(step_t.to_string());
    let zero = || Expr::Num(Number::Int(0));
    let bin = |op, a, b| Expr::Binary(op, Box::new(a), Box::new(b));
    let up = if exclusive { BinOp::Lt } else { BinOp::Le };
    let down = if exclusive { BinOp::Gt } else { BinOp::Ge };
    bin(
        BinOp::Or,
        bin(BinOp::And, bin(BinOp::Gt, stepv(), zero()), bin(up, v(), endv())),
        bin(BinOp::And, bin(BinOp::Lt, stepv(), zero()), bin(down, v(), endv())),
    )
}

impl Lowerer {
    /// Lower one routine (the main program or a sub body) with its **own** label
    /// scope: labels and `GOTO`s collected while lowering `stmts` are resolved
    /// against each other and do not leak to or from other routines.
    fn lower_routine(&mut self, stmts: &[Stmt]) -> Result<(), LowerError> {
        let saved_labels = std::mem::take(&mut self.labels);
        let saved_gotos = std::mem::take(&mut self.goto_fixups);
        self.lower_stmts(stmts)?;
        for (idx, label, line) in std::mem::take(&mut self.goto_fixups) {
            match self.labels.get(&label) {
                Some(&addr) => self.code[idx] = Instr::Jump(addr),
                None => {
                    return Err(LowerError::new(
                        format!("GOTO unknown label :{label}"),
                        line,
                    ))
                }
            }
        }
        self.labels = saved_labels;
        self.goto_fixups = saved_gotos;
        Ok(())
    }

    /// Emit any subs deferred during the most recent lowering. A sub body may
    /// defer further subs; the loop drains them all. Each is its own routine.
    fn drain_deferred_subs(&mut self) -> Result<(), LowerError> {
        while let Some((name, body, line)) = self.subs_deferred.pop() {
            if self.subs.contains_key(&name) {
                return Err(LowerError::new(format!("duplicate SUB {name:?}"), line));
            }
            let entry = self.code.len();
            self.subs.insert(name, entry);
            self.lower_routine(&body)?;
            self.code.push(Instr::Return { value: None, span: Span::new(line) });
        }
        Ok(())
    }

    fn lower_stmts(&mut self, stmts: &[Stmt]) -> Result<(), LowerError> {
        for s in stmts {
            self.lower_stmt(s)?;
        }
        Ok(())
    }

    fn lower_stmt(&mut self, s: &Stmt) -> Result<(), LowerError> {
        match s {
            Stmt::Command { verb, raw_args, span } => {
                self.code.push(Instr::Command {
                    verb: verb.clone(),
                    raw_args: raw_args.clone(),
                    span: *span,
                });
            }
            Stmt::Set { name, expr, span } => {
                self.code.push(Instr::SetVar {
                    name: name.clone(),
                    expr: expr.clone(),
                    span: *span,
                });
            }
            Stmt::SetIndex { name, index, expr, span } => {
                self.code.push(Instr::SetIndexVar {
                    name: name.clone(),
                    index: index.clone(),
                    expr: expr.clone(),
                    span: *span,
                });
            }
            Stmt::Label { name, span } => {
                // The label's address is the next instruction emitted.
                if self.labels.insert(name.clone(), self.code.len()).is_some() {
                    return Err(LowerError::new(
                        format!("duplicate label :{name}"),
                        span.line,
                    ));
                }
            }
            Stmt::Goto { label, span } => {
                let idx = self.code.len();
                self.code.push(Instr::Jump(usize::MAX)); // placeholder
                self.goto_fixups.push((idx, label.clone(), span.line));
            }
            Stmt::If { arms, els, span } => self.lower_if(arms, els, *span)?,
            Stmt::While { cond, body, span } => self.lower_while(cond, body, *span)?,
            Stmt::For { var, kind, body, span } => self.lower_for(var, kind, body, *span)?,
            Stmt::Switch { scrutinee, arms, default, span } => {
                self.lower_switch(scrutinee, arms, default, *span)?
            }
            Stmt::Sub { name, body, span } => {
                self.subs_deferred.push((name.clone(), body.clone(), span.line));
            }
            Stmt::Capture { var, body, span } => {
                self.code.push(Instr::CaptureBegin { var: var.clone(), span: *span });
                self.lower_stmts(body)?;
                self.code.push(Instr::CaptureEnd);
            }
            Stmt::Break(span) => {
                let idx = self.code.len();
                self.code.push(Instr::Jump(usize::MAX)); // placeholder
                match self.loops.last_mut() {
                    Some(l) => l.break_fixups.push(idx),
                    None => return Err(LowerError::new("BREAK outside WHILE", span.line)),
                }
            }
            Stmt::Continue(span) => {
                let idx = self.code.len();
                self.code.push(Instr::Jump(usize::MAX)); // placeholder, patched at loop end
                match self.loops.last_mut() {
                    Some(l) => l.continue_fixups.push(idx),
                    None => return Err(LowerError::new("CONTINUE outside a loop", span.line)),
                }
            }
            Stmt::Return { value, span } => {
                self.code.push(Instr::Return { value: value.clone(), span: *span });
            }
            Stmt::Exit(_) => self.code.push(Instr::Exit),
            Stmt::ExitAll(_) => self.code.push(Instr::ExitAll),
            Stmt::Local { names, span } => {
                self.code.push(Instr::Local { names: names.clone(), span: *span });
            }
            Stmt::Clear { names, span } => {
                self.code.push(Instr::Clear { names: names.clone(), span: *span });
            }
        }
        Ok(())
    }

    fn lower_if(
        &mut self,
        arms: &[ast::CondArm],
        els: &Option<Vec<Stmt>>,
        _span: Span,
    ) -> Result<(), LowerError> {
        let mut end_jumps = Vec::new();
        for arm in arms {
            // eval cond; if false jump to the next arm/else/end.
            let ju = self.code.len();
            self.code.push(Instr::JumpUnless {
                cond: arm.cond.clone(),
                target: usize::MAX,
                span: _span,
            });
            self.lower_stmts(&arm.body)?;
            // After a taken arm, skip the rest.
            end_jumps.push(self.code.len());
            self.code.push(Instr::Jump(usize::MAX));
            // The false-branch target is the start of whatever comes next.
            let next = self.code.len();
            if let Instr::JumpUnless { target, .. } = &mut self.code[ju] {
                *target = next;
            }
        }
        if let Some(els) = els {
            self.lower_stmts(els)?;
        }
        let end = self.code.len();
        for j in end_jumps {
            self.code[j] = Instr::Jump(end);
        }
        Ok(())
    }

    /// Lower a `SWITCH` by evaluating the scrutinee ONCE into a hidden `#SW{n}` temp, then reusing the
    /// `IF`/`ELSEIF` machinery: each `CASE v1,v2,…` becomes an arm whose condition is
    /// `(#SW == v1) OR (#SW == v2) …`, and `DEFAULT` becomes the `else`. No fall-through.
    fn lower_switch(
        &mut self,
        scrutinee: &Expr,
        arms: &[ast::SwitchArm],
        default: &Option<Vec<Stmt>>,
        span: Span,
    ) -> Result<(), LowerError> {
        let n = self.for_seq;
        self.for_seq += 1;
        let tmp = format!("#SW{n}");
        self.code.push(Instr::SetVar { name: tmp.clone(), expr: scrutinee.clone(), span });
        let eq = |v: &Expr| {
            Expr::Binary(BinOp::Eq, Box::new(Expr::Var(tmp.clone())), Box::new(v.clone()))
        };
        let cond_arms: Vec<ast::CondArm> = arms
            .iter()
            .map(|a| {
                let cond = a
                    .values
                    .iter()
                    .map(eq)
                    .reduce(|acc, e| Expr::Binary(BinOp::Or, Box::new(acc), Box::new(e)))
                    .expect("a CASE always has at least one value");
                ast::CondArm { cond, body: a.body.clone() }
            })
            .collect();
        self.lower_if(&cond_arms, default, span)
    }

    fn lower_while(&mut self, cond: &Expr, body: &[Stmt], span: Span) -> Result<(), LowerError> {
        let start = self.code.len();
        let ju = self.code.len();
        self.code.push(Instr::JumpUnless {
            cond: cond.clone(),
            target: usize::MAX,
            span,
        });
        self.loops.push(LoopCtx::new());
        self.lower_stmts(body)?;
        self.code.push(Instr::Jump(start)); // loop back to the test
        let end = self.code.len();
        if let Instr::JumpUnless { target, .. } = &mut self.code[ju] {
            *target = end;
        }
        let ctx = self.loops.pop().expect("loop ctx present");
        for idx in ctx.break_fixups {
            self.code[idx] = Instr::Jump(end);
        }
        for idx in ctx.continue_fixups {
            self.code[idx] = Instr::Jump(start); // WHILE: CONTINUE → re-test
        }
        Ok(())
    }

    /// Lower a `FOR` loop onto the same jump machinery as `WHILE`. `CONTINUE` targets the increment
    /// step (so the counter still advances), `BREAK` the loop end. Bounds/step (range) and the
    /// collection (`IN`) are snapshotted into hidden `#FOR_*` temps at entry → evaluated once.
    fn lower_for(
        &mut self,
        var: &str,
        kind: &ForKind,
        body: &[Stmt],
        span: Span,
    ) -> Result<(), LowerError> {
        let n = self.for_seq;
        self.for_seq += 1;
        let set = |this: &mut Self, name: String, expr: Expr| {
            this.code.push(Instr::SetVar { name, expr, span });
        };
        match kind {
            ForKind::Range { start, end, step, exclusive } => {
                let end_t = format!("#FOR_END{n}");
                let step_t = format!("#FOR_STEP{n}");
                // init: evaluate start / end / step ONCE
                set(self, var.to_string(), start.clone());
                set(self, end_t.clone(), end.clone());
                let step_expr = step.clone().unwrap_or(Expr::Num(Number::Int(1)));
                set(self, step_t.clone(), step_expr);
                // test
                let ju = self.code.len();
                self.code.push(Instr::JumpUnless {
                    cond: range_cond(var, &end_t, &step_t, *exclusive),
                    target: usize::MAX,
                    span,
                });
                self.loops.push(LoopCtx::new());
                self.lower_stmts(body)?;
                // increment (CONTINUE lands here)
                let incr = self.code.len();
                set(
                    self,
                    var.to_string(),
                    Expr::Binary(
                        BinOp::Add,
                        Box::new(Expr::Var(var.to_string())),
                        Box::new(Expr::Var(step_t)),
                    ),
                );
                self.code.push(Instr::Jump(ju)); // back to the test
                self.close_loop(ju, incr);
            }
            ForKind::In { coll, value } => {
                let seq_t = format!("#FOR_SEQ{n}");
                let idx_t = format!("#FOR_IDX{n}");
                // Snapshot the iteration sequence + index 0. The tuple form (`k, v IN …`) walks
                // `ITERP` (an array of `[key, value]` pairs); the single form walks `ITER` (elements
                // / keys / chars).
                let iter_fn = if value.is_some() { "ITERP" } else { "ITER" };
                set(self, seq_t.clone(), Expr::Call(iter_fn.into(), vec![coll.clone()]));
                set(self, idx_t.clone(), Expr::Num(Number::Int(0)));
                let ju = self.code.len();
                self.code.push(Instr::JumpUnless {
                    cond: Expr::Binary(
                        BinOp::Lt,
                        Box::new(Expr::Var(idx_t.clone())),
                        Box::new(Expr::Call("LEN".into(), vec![Expr::Var(seq_t.clone())])),
                    ),
                    target: usize::MAX,
                    span,
                });
                // Bind the loop variable(s) each iteration, before the body. Single: `var = seq[idx]`.
                // Tuple: `var = seq[idx][0]` (key/index), `value = seq[idx][1]` (value/element/char).
                let elem = || Expr::Index(Box::new(Expr::Var(seq_t.clone())), Box::new(Expr::Var(idx_t.clone())));
                match value {
                    None => set(self, var.to_string(), elem()),
                    Some(val_var) => {
                        set(
                            self,
                            var.to_string(),
                            Expr::Index(Box::new(elem()), Box::new(Expr::Num(Number::Int(0)))),
                        );
                        set(
                            self,
                            val_var.to_string(),
                            Expr::Index(Box::new(elem()), Box::new(Expr::Num(Number::Int(1)))),
                        );
                    }
                }
                self.loops.push(LoopCtx::new());
                self.lower_stmts(body)?;
                let incr = self.code.len();
                set(
                    self,
                    idx_t.clone(),
                    Expr::Binary(
                        BinOp::Add,
                        Box::new(Expr::Var(idx_t)),
                        Box::new(Expr::Num(Number::Int(1))),
                    ),
                );
                self.code.push(Instr::Jump(ju));
                self.close_loop(ju, incr);
            }
        }
        Ok(())
    }

    /// Backpatch a `FOR` loop: the test's false-jump → the end, `BREAK`s → the end, `CONTINUE`s →
    /// the increment (`incr`). `ju` is the test (`JumpUnless`) instruction index.
    fn close_loop(&mut self, ju: usize, incr: usize) {
        let end = self.code.len();
        if let Instr::JumpUnless { target, .. } = &mut self.code[ju] {
            *target = end;
        }
        let ctx = self.loops.pop().expect("loop ctx present");
        for idx in ctx.break_fixups {
            self.code[idx] = Instr::Jump(end);
        }
        for idx in ctx.continue_fixups {
            self.code[idx] = Instr::Jump(incr);
        }
    }
}

/// Convenience: parse + lower a source string, used by tests and the VM.
pub fn compile(source: &str) -> Result<Program, String> {
    compile_with_library(source, &[])
}

/// Parse + lower a source string, hoisting `extra_subs` (library routines).
pub fn compile_with_library(
    source: &str,
    extra_subs: &[(String, Vec<Stmt>)],
) -> Result<Program, String> {
    let ast = crate::parser::parse(source).map_err(|e| format!("line {}: {}", e.line, e.msg))?;
    lower_with(&ast, extra_subs).map_err(|e| format!("line {}: {}", e.line, e.msg))
}

/// Lower already-folded label sugar — exposed for tests.
pub fn fold(name: &str) -> String {
    fold_ident(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(src: &str) -> Vec<Instr> {
        compile(src).unwrap().code
    }

    #[test]
    fn linear_program_ends_in_exit() {
        let c = code("PR a\nPR b");
        assert_eq!(c.len(), 3); // 2 commands + Exit
        assert!(matches!(c[2], Instr::Exit));
    }

    #[test]
    fn if_lowers_to_jump_unless_and_skip() {
        // IF $x PR yes ELSE PR no ENDIF
        let c = code("IF $x\nPR yes\nELSE\nPR no\nENDIF");
        // [0] JumpUnless -> else, [1] PR yes, [2] Jump -> end, [3] PR no, [4] Exit
        match &c[0] {
            Instr::JumpUnless { target, .. } => assert_eq!(*target, 3),
            other => panic!("expected JumpUnless, got {other:?}"),
        }
        match &c[2] {
            Instr::Jump(t) => assert_eq!(*t, 4),
            other => panic!("expected Jump, got {other:?}"),
        }
    }

    #[test]
    fn while_loops_back_and_break_jumps_to_end() {
        // WHILE 1 \n IF $done BREAK ENDIF \n ENDWHILE
        let c = code("WHILE 1\nIF $done\nBREAK\nENDIF\nENDWHILE");
        // [0] JumpUnless(cond) -> end
        // [1] JumpUnless($done false) -> after-break (the if-end jump)
        // [2] Jump(end)  <- BREAK
        // [3] Jump(skip-after-if-body)  <- end-of-if-arm jump
        // [4] Jump(0)    <- loop back to start
        // [5] Exit
        match c.last().unwrap() {
            Instr::Exit => {}
            other => panic!("expected Exit, got {other:?}"),
        }
        // The WHILE test jumps to the loop-back+1 (end).
        let end = c.len() - 1;
        match &c[0] {
            Instr::JumpUnless { target, .. } => assert_eq!(*target, end),
            other => panic!("expected JumpUnless, got {other:?}"),
        }
        // The BREAK jumps to the same end.
        match &c[2] {
            Instr::Jump(t) => assert_eq!(*t, end),
            other => panic!("expected Jump(BREAK), got {other:?}"),
        }
        // The loop-back jumps to 0.
        match &c[end - 1] {
            Instr::Jump(0) => {}
            other => panic!("expected loop-back Jump(0), got {other:?}"),
        }
    }

    #[test]
    fn goto_resolves_forward_and_back() {
        let c = code(":top\nGOTO :bot\nPR mid\n:bot\nGOTO :top");
        // labels: top -> 0, bot -> 2 (after the GOTO :bot jump + PR mid)
        // [0] Jump(2)  <- GOTO :bot
        // [1] PR mid
        // [2] Jump(0)  <- GOTO :top
        // [3] Exit
        assert!(matches!(c[0], Instr::Jump(2)));
        assert!(matches!(c[2], Instr::Jump(0)));
    }

    #[test]
    fn unknown_goto_label_is_error() {
        assert!(compile("GOTO :nowhere").is_err());
    }

    #[test]
    fn break_outside_loop_is_error() {
        assert!(compile("BREAK").is_err());
    }

    #[test]
    fn labels_are_scoped_per_routine() {
        // The same label name in the main program and in a sub must not collide
        // (each routine has its own label scope).
        let src = ":loop\nPR top\nSUB s\n:loop\nPR insub\nENDSUB";
        assert!(compile(src).is_ok());
    }

    #[test]
    fn goto_cannot_cross_into_a_sub() {
        // A top-level GOTO to a label that only exists inside a sub is unknown.
        let src = "GOTO :inner\nSUB s\n:inner\nPR x\nENDSUB";
        assert!(compile(src).is_err());
    }

    #[test]
    fn sub_is_hoisted_after_exit() {
        let c = code("PR main\nSUB s\nPR insub\nENDSUB");
        // [0] PR main, [1] Exit, [2] PR insub, [3] Return
        assert!(matches!(c[1], Instr::Exit));
        match &c[2] {
            Instr::Command { raw_args, .. } => assert_eq!(raw_args, "insub"),
            other => panic!("expected sub body command, got {other:?}"),
        }
        assert!(matches!(c[3], Instr::Return { value: None, .. }));
        let p = compile("PR main\nSUB s\nPR insub\nENDSUB").unwrap();
        assert_eq!(p.subs.get("S"), Some(&2));
    }
}
