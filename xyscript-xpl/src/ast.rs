//! The structured AST (`stage1-interpreter-plan.md` §6).
//!
//! Control flow is **structural** — `If`/`While`/`Sub` are real nested nodes, so
//! the `;`-chain and physical-line footguns of QN's line/PC machine cannot
//! exist (`stage1-interpreter-shape.md`). A single-line `IF cond cmd` and a
//! block `IF cond … ENDIF` are *both* just [`Stmt::If`] nodes; the distinction
//! that mattered in QN (push a frame or not) disappears.
//!
//! Verb arguments stay **raw text** ([`Stmt::Command::raw_args`]) — only
//! `SET`/`IF`/`WHILE` evaluate the expression sub-language, matching QN. Every
//! node carries a [`Span`] (source line) so the parser can report structural
//! errors (`ENDIF` with no `IF`, an unclosed `SUB`) at parse time rather than as
//! a runtime stack underflow.

use crate::expr::Expr;

/// What a `FOR` loop iterates over.
#[derive(Debug, Clone, PartialEq)]
pub enum ForKind {
    /// `v = start TO end [STEP step]` — counting loop. `exclusive` (`FOR/X`) drops the upper bound.
    /// `step` defaults to `+1`; the direction is decided at runtime from the step's sign.
    Range { start: Expr, end: Expr, step: Option<Expr>, exclusive: bool },
    /// `v IN expr` — iterate a collection: an array's elements, a map's keys, a string's chars.
    /// With a `value` variable (`k, v IN expr`) the loop var holds the index/key and `value` holds
    /// the element/value/char.
    In { coll: Expr, value: Option<String> },
}

/// Source location for diagnostics. Line numbers are 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub line: usize,
    /// 1-based column (the command's start position on its line); 0 = unknown/not tracked.
    pub col: usize,
}

impl Span {
    pub fn new(line: usize) -> Span {
        Span { line, col: 0 }
    }
    /// A span with a known 1-based column.
    pub fn at(line: usize, col: usize) -> Span {
        Span { line, col }
    }
}

/// A parsed program: a flat sequence of statements (blocks nest inside).
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub stmts: Vec<Stmt>,
}

/// One `IF`/`ELSEIF` arm: a condition and the body run when it is the first
/// truthy arm.
#[derive(Debug, Clone, PartialEq)]
pub struct CondArm {
    pub cond: Expr,
    pub body: Vec<Stmt>,
}

/// One `CASE` arm of a `SWITCH`: the value(s) that select it (the arm runs when the scrutinee equals
/// any of them) and the body. No fall-through — a matched arm runs its body and the switch ends.
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchArm {
    pub values: Vec<Expr>,
    pub body: Vec<Stmt>,
}

/// A statement node.
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// An ordinary verb dispatch. `verb` is case-folded; `raw_args` is the
    /// verbatim tail (substituted, then handed to the verb, at run time).
    Command {
        verb: String,
        raw_args: String,
        span: Span,
    },
    /// `SET name = expr` (the only verb whose RHS is an expression node here;
    /// `SV` is a selection verb and stays a `Command`). `name` is case-folded.
    Set {
        name: String,
        expr: Expr,
        span: Span,
    },
    /// `SET name[index] = expr` — assign into an array element (numeric index) or
    /// map entry (string key). `name` is case-folded.
    SetIndex {
        name: String,
        index: Expr,
        expr: Expr,
        span: Span,
    },
    /// `IF`/`IFNOT` with optional `ELSEIF`/`ELSE`. `arms[0]` is the `IF`; the
    /// rest are `ELSEIF`s, in order. `els` is the optional `ELSE` body.
    If {
        arms: Vec<CondArm>,
        els: Option<Vec<Stmt>>,
        span: Span,
    },
    /// `WHILE cond … ENDWHILE`.
    While {
        cond: Expr,
        body: Vec<Stmt>,
        span: Span,
    },
    /// `FOR[/X] … ENDFOR` — a counting (`v = a TO b [STEP s]`) or collection (`v IN expr`) loop.
    /// `var` is the case-folded loop variable. Lowers onto the `WHILE` jump machinery.
    For {
        var: String,
        kind: ForKind,
        body: Vec<Stmt>,
        span: Span,
    },
    /// `SWITCH expr … CASE … [DEFAULT …] ENDSWITCH/ESW` — multi-way branch on a value. `scrutinee`
    /// is evaluated once; the first `CASE` whose value list contains an equal value runs (no
    /// fall-through), else `default` (if present). Lowers onto the `IF` jump machinery.
    Switch {
        scrutinee: Expr,
        arms: Vec<SwitchArm>,
        default: Option<Vec<Stmt>>,
        span: Span,
    },
    /// `SUB name … ENDSUB`. `name` is case-folded.
    Sub {
        name: String,
        body: Vec<Stmt>,
        span: Span,
    },
    /// `CAPTURE var … ENDCAPTURE` (or inline `CAPTURE var cmd`): run `body` with command
    /// output (`PR`/`MSG`/`TXT`/`HLKINDS`/`DIR`…) redirected into the user variable `var`
    /// instead of displayed. `var` is case-folded.
    Capture {
        var: String,
        body: Vec<Stmt>,
        span: Span,
    },
    /// A `:label` target. `name` is case-folded, colon stripped.
    Label { name: String, span: Span },
    /// `GOTO :label`. `label` is case-folded, colon stripped.
    Goto { label: String, span: Span },
    /// `BREAK` — exit the innermost `WHILE`.
    Break(Span),
    /// `CONTINUE` — restart the innermost `WHILE`.
    Continue(Span),
    /// `RETURN [expr]` — leave the current sub, optionally setting `$RV`.
    Return { value: Option<Expr>, span: Span },
    /// `EXIT` / `EX` — end the current script.
    Exit(Span),
    /// `EXITALL` / `EX1` — end every running script.
    ExitAll(Span),
    /// `LOCAL name…` — save/clear/restore names for the routine's scope.
    Local { names: Vec<String>, span: Span },
    /// `CLEAR name…` — clear names.
    Clear { names: Vec<String>, span: Span },
}

impl Stmt {
    /// The source span of this statement (its opening line for blocks).
    pub fn span(&self) -> Span {
        match self {
            Stmt::Command { span, .. }
            | Stmt::Set { span, .. }
            | Stmt::SetIndex { span, .. }
            | Stmt::If { span, .. }
            | Stmt::While { span, .. }
            | Stmt::For { span, .. }
            | Stmt::Switch { span, .. }
            | Stmt::Sub { span, .. }
            | Stmt::Capture { span, .. }
            | Stmt::Label { span, .. }
            | Stmt::Goto { span, .. }
            | Stmt::Return { span, .. }
            | Stmt::Local { span, .. }
            | Stmt::Clear { span, .. } => *span,
            Stmt::Break(s) | Stmt::Continue(s) | Stmt::Exit(s) | Stmt::ExitAll(s) => *s,
        }
    }
}
