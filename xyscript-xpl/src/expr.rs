//! Expression sub-language (`stage1-interpreter-plan.md` §7).
//!
//! The grammar `SET`/`IF`/`WHILE` evaluate, ported from QN's `_ExprParser`:
//!
//! ```text
//! expr   := or_e
//! or_e   := and_e ('OR'  and_e)*
//! and_e  := not_e ('AND' not_e)*
//! not_e  := 'NOT' not_e | comp_e
//! comp_e := add_e (('==' | '!=' | '<' | '>' | '<=' | '>=') add_e)?   // at most one
//! add_e  := mul_e (('+' | '-' | '.') mul_e)*                          // '.' = concat
//! mul_e  := unary (('*' | '/' | '%') unary)*
//! unary  := '-' unary | atom
//! atom   := NUMBER | STRING | VAR | '(' expr ')' | NAME '(' args ')' | BARE
//! ```
//!
//! **Two recorded XyScript divergences from QN (§7/§12):**
//!
//! - **No `+-*/%` try/rollback hack.** QN wraps the arithmetic operators in
//!   try/rollback so junk like `IF $a + GOTO done` stops the expression and
//!   treats the rest as a sub-command — a workaround for its textual model.
//!   XyScript parses the condition as a full expression and errors clearly on
//!   garbage (the structured statement parser owns the IF/condition boundary).
//! - **Token-level `$` substitution.** `$NAME` is a [`Expr::Var`] node resolved
//!   at eval time against the [`EvalContext`], not a textual pre-pass.
//!
//! Otherwise the leaf semantics are bug-for-bug faithful (the tested oracle):
//! integer floor division, the 16-function table, truthiness, etc.

use crate::lexer::{tokenize, tokenize_partial, Kw, Op, Tok, Token};
use crate::value::{fold_ident, xpl_cmp, Number, Value};
use std::cmp::Ordering;

/// An expression AST node. Parsed once; evaluated (possibly repeatedly, e.g. a
/// `WHILE` condition) against an [`EvalContext`].
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Num(Number),
    Str(String),
    /// `$NAME` / `${NAME}` / `$1` — resolved at eval time.
    Var(String),
    /// `@X` / `&X` binder reference (`script` = the `&` form), resolved at eval time to
    /// the stored string. `slot` is upper-cased.
    Binder { script: bool, slot: char },
    /// An unquoted identifier used as a string literal.
    Bare(String),
    /// `NAME(args)` — `name` is case-folded.
    Call(String, Vec<Expr>),
    /// `[a, b, c]` — an array literal (possibly empty `[]`; nested allowed). Each element is
    /// evaluated at eval time, so `[$x, LEN($a), 1+2]` works.
    ArrayLit(Vec<Expr>),
    /// `{ k: v, … }` — a map literal (possibly empty `{}`; nested allowed). Each key is a string
    /// (bare identifier or quoted; value-expressions allowed via the `(expr): v` form) and each value
    /// is evaluated at eval time. Keys collapse to strings, matching map indexing.
    MapLit(Vec<(Expr, Expr)>),
    /// `base[index]` — element access into an array (0-based int), map (string
    /// key), or string (char at index). Out-of-range / missing → empty string.
    Index(Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
}

/// Binary operators, grouped by the grammar level they bind at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Add,
    Sub,
    Concat,
    Mul,
    Div,
    Mod,
    /// `^` exponentiation — binds tighter than `* / %`, right-associative.
    Pow,
}

/// An expression error. Mapped to the two-tier error model upstream: most — unknown function, bad
/// arity, non-numeric arithmetic, divide-by-zero — are *fatal* (a programming bug, per §8); a few
/// runtime conditions (a `READFILE` of a missing file) are marked *trappable* so a script can recover
/// via `$ERROR` instead of halting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprError {
    pub msg: String,
    pub trappable: bool,
}

impl ExprError {
    fn new(msg: impl Into<String>) -> ExprError {
        ExprError { msg: msg.into(), trappable: false }
    }
    /// A recoverable error: upstream surfaces it as a trappable `$ERROR`, not a fatal halt.
    fn trappable(msg: impl Into<String>) -> ExprError {
        ExprError { msg: msg.into(), trappable: true }
    }
}

/// The eval-time environment an expression reads from. Kept minimal so `expr` is
/// testable in isolation; the real implementation is the executor's `Env`/`Host`
/// (later steps). Variable-name folding and special-vs-user precedence live in
/// the implementor.
/// The current local date/time, broken into components, for `DATE()`/`TIME()`. The host fills it
/// from the same clock as `$YEAR`/`$MON`/…; `hour` is 24-hour (0–23).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateParts {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

pub trait EvalContext {
    /// Look up a variable (`$NAME` / `VAR(name)`). `None` if undefined (which
    /// evaluates to the empty string, per QN).
    fn get_var(&self, name: &str) -> Option<Value>;
    /// The current local date/time for `DATE()`/`TIME()`. `None` in contexts without a clock
    /// (the bare evaluator), where those functions report an error.
    fn now(&self) -> Option<DateParts> {
        None
    }
    /// The current selection text (`SEL()`); `""` if none.
    fn sel(&self) -> String {
        String::new()
    }
    /// Tab `n`'s selection text (`SEL(n)`, 1-based); `""` if out of range or no selection there.
    /// Default `""` (the bare context has no buffers).
    fn sel_at(&self, _n: i64) -> String {
        String::new()
    }
    /// A binder's stored content for value access (`@X`/`&X` as a value). `script=false`
    /// → text binder, `true` → script binder. `slot` upper-cased. `None` → empty string.
    fn binder(&self, _script: bool, _slot: char) -> Option<String> {
        None
    }
    /// The fixed number of decimal places to render floats with (the `DECIMALS` setting), applied
    /// when values are stringified for display (substitution / `$(…)`). `None` = default precision.
    fn float_decimals(&self) -> Option<u8> {
        None
    }
    /// `KEY(scan)` — the current binding of key code `scan` (unshifted layer), or `None` if unbound /
    /// out of range / no keymap.
    fn key_binding(&self, _scan: i64) -> Option<String> {
        None
    }
    /// Whether `path` is an existing directory (host-resolved against `$CWD`). For `GETPATH()`/
    /// `ISDIR()`. Default `false` (no filesystem in the bare context).
    fn path_is_dir(&self, _path: &str) -> bool {
        false
    }
    /// Whether `path` exists at all (file or directory; host-resolved). For `EXIST()`/`ISFILE()`.
    /// Default `false`.
    fn path_exists(&self, _path: &str) -> bool {
        false
    }
    /// Read a text file's contents as a string (for `READFILE()`/`RF()`), host-resolved against
    /// `$CWD`. `Err(message)` on a missing/unreadable/non-UTF-8 file — surfaced to the script as a
    /// trappable error. Default: no filesystem (the bare evaluator).
    fn read_file(&self, _path: &str) -> Result<String, String> {
        Err("READFILE: no filesystem in this context".to_string())
    }
    /// A random integer in the inclusive range `[min, max]` (for `RANDOM(min, max)`), drawn from the
    /// host's seedable generator (see the `SRANDOM` verb). `None` in the bare evaluator, which has no
    /// generator — there `RANDOM` reports an error. Args swapped if `min > max`.
    fn random_int(&self, _min: i64, _max: i64) -> Option<i64> {
        None
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse `s` as a complete expression. Trailing tokens are an error.
pub fn parse(s: &str) -> Result<Expr, ExprError> {
    let tokens = tokenize(s).map_err(|e| ExprError::new(e.0))?;
    if tokens.is_empty() {
        return Err(ExprError::new("empty expression"));
    }
    let mut p = Parser { tokens: &tokens, pos: 0, src: s };
    let e = p.parse_or()?;
    if p.pos < tokens.len() {
        return Err(ExprError::new(format!(
            "trailing token after expression: {:?}",
            p.tokens[p.pos].tok
        )));
    }
    Ok(e)
}

/// Parse a **greedy prefix** of `s` as an expression, returning the parsed
/// `Expr` and the byte offset in `s` just past the last token it consumed. The
/// remainder (`s[offset..]`) is whatever the expression didn't claim — used by
/// the statement parser to split a single-line `IF cond cmd` into its condition
/// and its trailing command. No tokens consumed (or a leading parse failure) is
/// an error. This is the §7 "no rollback" boundary: the prefix stops at the
/// first token that can't extend the expression, and never backtracks.
pub fn parse_prefix(s: &str) -> Result<(Expr, usize), ExprError> {
    // Use the partial tokenizer: the trailing command may contain characters the
    // expression lexer rejects, but the condition prefix is still valid.
    let (tokens, _lex_err) = tokenize_partial(s);
    if tokens.is_empty() {
        return Err(ExprError::new("empty expression"));
    }
    let mut p = Parser { tokens: &tokens, pos: 0, src: s };
    let e = p.parse_or()?;
    if p.pos == 0 {
        return Err(ExprError::new("expression consumed no tokens"));
    }
    let end = tokens[p.pos - 1].end;
    Ok((e, end))
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    /// The original source, so `DATE()`/`TIME()` can capture their raw mask text between the parens
    /// (token spans index into this) rather than have it parsed as an expression.
    src: &'a str,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos).map(|t| &t.tok)
    }
    fn peek_tok(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }
    fn eat(&mut self) -> &Token {
        let t = &self.tokens[self.pos];
        self.pos += 1;
        t
    }

    fn parse_or(&mut self) -> Result<Expr, ExprError> {
        let mut v = self.parse_and()?;
        while matches!(self.peek(), Some(Tok::Kw(Kw::Or))) {
            self.eat();
            let r = self.parse_and()?;
            v = Expr::Binary(BinOp::Or, Box::new(v), Box::new(r));
        }
        Ok(v)
    }

    fn parse_and(&mut self) -> Result<Expr, ExprError> {
        let mut v = self.parse_not()?;
        while matches!(self.peek(), Some(Tok::Kw(Kw::And))) {
            self.eat();
            let r = self.parse_not()?;
            v = Expr::Binary(BinOp::And, Box::new(v), Box::new(r));
        }
        Ok(v)
    }

    fn parse_not(&mut self) -> Result<Expr, ExprError> {
        if matches!(self.peek(), Some(Tok::Kw(Kw::Not))) {
            self.eat();
            let v = self.parse_not()?;
            return Ok(Expr::Not(Box::new(v)));
        }
        self.parse_comp()
    }

    fn parse_comp(&mut self) -> Result<Expr, ExprError> {
        let v = self.parse_add()?;
        let op = match self.peek() {
            Some(Tok::Op(Op::EqEq)) => Some(BinOp::Eq),
            Some(Tok::Op(Op::Ne)) => Some(BinOp::Ne),
            Some(Tok::Op(Op::Lt)) => Some(BinOp::Lt),
            Some(Tok::Op(Op::Gt)) => Some(BinOp::Gt),
            Some(Tok::Op(Op::Le)) => Some(BinOp::Le),
            Some(Tok::Op(Op::Ge)) => Some(BinOp::Ge),
            _ => None,
        };
        if let Some(op) = op {
            self.eat();
            let r = self.parse_add()?;
            return Ok(Expr::Binary(op, Box::new(v), Box::new(r)));
        }
        Ok(v)
    }

    fn parse_add(&mut self) -> Result<Expr, ExprError> {
        let mut v = self.parse_mul()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op(Op::Plus)) => BinOp::Add,
                Some(Tok::Op(Op::Minus)) => BinOp::Sub,
                Some(Tok::Op(Op::Dot)) => BinOp::Concat,
                _ => break,
            };
            self.eat();
            let r = self.parse_mul()?;
            v = Expr::Binary(op, Box::new(v), Box::new(r));
        }
        Ok(v)
    }

    fn parse_mul(&mut self) -> Result<Expr, ExprError> {
        let mut v = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op(Op::Star)) => BinOp::Mul,
                Some(Tok::Op(Op::Slash)) => BinOp::Div,
                Some(Tok::Op(Op::Percent)) => BinOp::Mod,
                _ => break,
            };
            self.eat();
            let r = self.parse_unary()?;
            v = Expr::Binary(op, Box::new(v), Box::new(r));
        }
        Ok(v)
    }

    fn parse_unary(&mut self) -> Result<Expr, ExprError> {
        if matches!(self.peek(), Some(Tok::Op(Op::Minus))) {
            self.eat();
            let v = self.parse_unary()?;
            return Ok(Expr::Neg(Box::new(v)));
        }
        self.parse_pow()
    }

    /// `^` exponentiation: binds tighter than `* / %` (its base is a postfix atom), and is
    /// RIGHT-associative — the exponent recurses through `parse_unary`, so `2^3^2` = `2^(3^2)`
    /// and a unary minus in the exponent (`2^-3`) parses. `-2^2` is `-(2^2)` because the leading
    /// `-` is handled one level up in `parse_unary`.
    fn parse_pow(&mut self) -> Result<Expr, ExprError> {
        let base = self.parse_postfix()?;
        if matches!(self.peek(), Some(Tok::Op(Op::Caret))) {
            self.eat();
            let exp = self.parse_unary()?;
            return Ok(Expr::Binary(BinOp::Pow, Box::new(base), Box::new(exp)));
        }
        Ok(base)
    }

    /// `atom ('[' expr ']')*` — postfix element access, chainable (`$a[0][1]`).
    fn parse_postfix(&mut self) -> Result<Expr, ExprError> {
        let mut base = self.parse_atom()?;
        while matches!(self.peek(), Some(Tok::Op(Op::LBracket))) {
            self.eat();
            let index = self.parse_or()?;
            match self.peek() {
                Some(Tok::Op(Op::RBracket)) => {
                    self.eat();
                }
                _ => return Err(ExprError::new("missing ']' to close index")),
            }
            base = Expr::Index(Box::new(base), Box::new(index));
        }
        Ok(base)
    }

    fn parse_atom(&mut self) -> Result<Expr, ExprError> {
        let t = match self.peek_tok() {
            Some(t) => t.clone(),
            None => return Err(ExprError::new("unexpected end of expression")),
        };
        match &t.tok {
            Tok::Num(n) => {
                self.eat();
                Ok(Expr::Num(*n))
            }
            Tok::Str(s) => {
                self.eat();
                Ok(Expr::Str(s.clone()))
            }
            Tok::Var(name) => {
                self.eat();
                Ok(Expr::Var(name.clone()))
            }
            Tok::Binder { script, slot } => {
                let (script, slot) = (*script, *slot);
                self.eat();
                Ok(Expr::Binder { script, slot })
            }
            Tok::Op(Op::LParen) => {
                self.eat();
                let v = self.parse_or()?;
                match self.peek() {
                    Some(Tok::Op(Op::RParen)) => {
                        self.eat();
                        Ok(v)
                    }
                    _ => Err(ExprError::new("missing ')'")),
                }
            }
            // Array literal `[a, b, c]` (or empty `[]`; a trailing comma is allowed). Postfix
            // indexing still applies, so `[1,2,3][0]` works.
            Tok::Op(Op::LBracket) => {
                self.eat(); // [
                let mut items = Vec::new();
                if !matches!(self.peek(), Some(Tok::Op(Op::RBracket))) {
                    loop {
                        items.push(self.parse_or()?);
                        match self.peek() {
                            Some(Tok::Op(Op::Comma)) => {
                                self.eat();
                                if matches!(self.peek(), Some(Tok::Op(Op::RBracket))) {
                                    break; // trailing comma
                                }
                            }
                            _ => break,
                        }
                    }
                }
                match self.peek() {
                    Some(Tok::Op(Op::RBracket)) => {
                        self.eat();
                        Ok(Expr::ArrayLit(items))
                    }
                    _ => Err(ExprError::new("missing ']' to close array literal")),
                }
            }
            // Map literal `{ k: v, … }` (or empty `{}`; trailing comma allowed). Keys are
            // expressions collapsed to strings (a bare word `a` → `"a"`, a quoted string, or a
            // parenthesised expression `($k)`); values are full expressions. Postfix indexing applies.
            Tok::Op(Op::LBrace) => {
                self.eat(); // {
                let mut pairs = Vec::new();
                if !matches!(self.peek(), Some(Tok::Op(Op::RBrace))) {
                    loop {
                        let key = self.parse_or()?;
                        match self.peek() {
                            Some(Tok::Op(Op::Colon)) => {
                                self.eat();
                            }
                            _ => return Err(ExprError::new("map literal: expected ':' after a key")),
                        }
                        let val = self.parse_or()?;
                        pairs.push((key, val));
                        match self.peek() {
                            Some(Tok::Op(Op::Comma)) => {
                                self.eat();
                                if matches!(self.peek(), Some(Tok::Op(Op::RBrace))) {
                                    break; // trailing comma
                                }
                            }
                            _ => break,
                        }
                    }
                }
                match self.peek() {
                    Some(Tok::Op(Op::RBrace)) => {
                        self.eat();
                        Ok(Expr::MapLit(pairs))
                    }
                    _ => Err(ExprError::new("missing '}' to close map literal")),
                }
            }
            Tok::Bare(name) => {
                // Function call iff the next token is `(` *adjacent* to the bare
                // identifier (no whitespace between). Otherwise a bare string.
                let bare_end = t.end;
                let next = self.tokens.get(self.pos + 1);
                let is_call = matches!(next.map(|n| &n.tok), Some(Tok::Op(Op::LParen)))
                    && next.map(|n| n.end - 1) == Some(bare_end);
                if is_call {
                    let fname = fold_ident(name);
                    // `DATE`/`TIME` take a RAW format mask, not an expression — `DATE(m-d-yyyy)`
                    // must not parse as `m - d - yyyy`. Capture the source text between the parens
                    // (token spans index into `self.src`) and hand it over as one string literal.
                    if fname == "DATE" || fname == "TIME" {
                        self.eat(); // BARE (DATE/TIME)
                        let mask_start = self.eat().end; // byte just past '('
                        let mut depth = 1usize;
                        let mask_end;
                        loop {
                            let Some(tk) = self.peek_tok() else {
                                return Err(ExprError::new(format!("{fname}: missing ')' to close call")));
                            };
                            let end = tk.end;
                            let (is_l, is_r) = (
                                matches!(tk.tok, Tok::Op(Op::LParen)),
                                matches!(tk.tok, Tok::Op(Op::RParen)),
                            );
                            self.eat();
                            if is_l {
                                depth += 1;
                            } else if is_r {
                                depth -= 1;
                                if depth == 0 {
                                    mask_end = end - 1; // the ')' byte
                                    break;
                                }
                            }
                        }
                        let raw = self.src[mask_start..mask_end].trim();
                        // Allow an optional quoted form, `DATE("m.d.yy")`, for masks with characters
                        // the bare lexer would reject (e.g. `.`).
                        let mask = raw
                            .strip_prefix('"')
                            .and_then(|r| r.strip_suffix('"'))
                            .unwrap_or(raw);
                        return Ok(Expr::Call(fname, vec![Expr::Str(mask.to_string())]));
                    }
                    self.eat(); // BARE
                    self.eat(); // (
                    let args = self.parse_arg_list()?;
                    match self.peek() {
                        Some(Tok::Op(Op::RParen)) => {
                            self.eat();
                        }
                        _ => {
                            return Err(ExprError::new(format!(
                                "{fname}: missing ')' to close call"
                            )))
                        }
                    }
                    Ok(Expr::Call(fname, args))
                } else {
                    self.eat();
                    Ok(Expr::Bare(name.clone()))
                }
            }
            other => Err(ExprError::new(format!("unexpected token {other:?}"))),
        }
    }

    fn parse_arg_list(&mut self) -> Result<Vec<Expr>, ExprError> {
        let mut args = Vec::new();
        // Empty arg list: next is `)`.
        if matches!(self.peek(), Some(Tok::Op(Op::RParen))) {
            return Ok(args);
        }
        loop {
            args.push(self.parse_or()?);
            match self.peek() {
                Some(Tok::Op(Op::Comma)) => {
                    self.eat();
                }
                _ => break,
            }
        }
        Ok(args)
    }
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

/// Evaluate an expression AST against `ctx`.
pub fn eval(e: &Expr, ctx: &dyn EvalContext) -> Result<Value, ExprError> {
    match e {
        Expr::Num(n) => Ok(Value::from_number(*n)),
        Expr::Str(s) => Ok(Value::str(s.clone())),
        Expr::Bare(s) => Ok(Value::str(s.clone())),
        Expr::ArrayLit(items) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                out.push(eval(it, ctx)?);
            }
            Ok(Value::Array(out))
        }
        Expr::MapLit(pairs) => {
            let mut m = std::collections::BTreeMap::new();
            for (k, v) in pairs {
                // Later duplicate keys win, matching object-literal conventions.
                m.insert(eval(k, ctx)?.to_str(), eval(v, ctx)?);
            }
            Ok(Value::Map(m))
        }
        Expr::Var(name) => Ok(ctx.get_var(name).unwrap_or_else(|| Value::str(""))),
        Expr::Binder { script, slot } => {
            Ok(Value::str(ctx.binder(*script, *slot).unwrap_or_default()))
        }
        Expr::Neg(inner) => {
            let v = eval(inner, ctx)?;
            let n = v.to_number().map_err(|_| {
                ExprError::new(format!("can't negate non-numeric value {:?}", v.to_str()))
            })?;
            Ok(Value::from_number(n.neg()))
        }
        Expr::Not(inner) => {
            let v = eval(inner, ctx)?;
            Ok(Value::Int(if v.is_truthy() { 0 } else { 1 }))
        }
        Expr::Binary(op, l, r) => eval_binary(*op, l, r, ctx),
        Expr::Call(name, args) => eval_call(name, args, ctx),
        Expr::Index(base, index) => {
            let b = eval(base, ctx)?;
            let i = eval(index, ctx)?;
            index_value(&b, &i)
        }
    }
}

/// Element access into an array / map / string. Out-of-range or missing keys
/// yield the empty string (the same lenient convention as an undefined variable);
/// indexing a plain number/bool is a (fatal) error.
pub fn index_value(base: &Value, index: &Value) -> Result<Value, ExprError> {
    match base {
        Value::Array(items) => {
            let n = match index.to_number() {
                Ok(crate::value::Number::Int(i)) => i,
                Ok(crate::value::Number::Float(f)) => f as i64,
                Err(_) => {
                    return Err(ExprError::new(format!(
                        "array index must be a number, got {:?}",
                        index.to_str()
                    )))
                }
            };
            if n < 0 {
                return Ok(Value::str(""));
            }
            Ok(items.get(n as usize).cloned().unwrap_or_else(|| Value::str("")))
        }
        Value::Map(m) => Ok(m.get(&index.to_str()).cloned().unwrap_or_else(|| Value::str(""))),
        Value::Str(s) => {
            let n = index.to_number().map_err(|_| {
                ExprError::new(format!("string index must be a number, got {:?}", index.to_str()))
            })?;
            let idx = match n {
                crate::value::Number::Int(i) => i,
                crate::value::Number::Float(f) => f as i64,
            };
            if idx < 0 {
                return Ok(Value::str(""));
            }
            Ok(s
                .chars()
                .nth(idx as usize)
                .map(|c| Value::str(c.to_string()))
                .unwrap_or_else(|| Value::str("")))
        }
        other => Err(ExprError::new(format!(
            "cannot index a {} value",
            match other {
                Value::Int(_) | Value::Float(_) => "numeric",
                Value::Bool(_) => "boolean",
                _ => "non-indexable",
            }
        ))),
    }
}

fn eval_binary(
    op: BinOp,
    l: &Expr,
    r: &Expr,
    ctx: &dyn EvalContext,
) -> Result<Value, ExprError> {
    // Both sides are always evaluated (QN does not short-circuit — a fatal in an
    // un-taken branch still fires).
    let lv = eval(l, ctx)?;
    let rv = eval(r, ctx)?;
    match op {
        BinOp::Or => Ok(Value::Int(if lv.is_truthy() || rv.is_truthy() { 1 } else { 0 })),
        BinOp::And => Ok(Value::Int(if lv.is_truthy() && rv.is_truthy() { 1 } else { 0 })),
        BinOp::Eq => Ok(Value::Int(if lv.xpl_eq(&rv) { 1 } else { 0 })),
        BinOp::Ne => Ok(Value::Int(if lv.xpl_eq(&rv) { 0 } else { 1 })),
        BinOp::Lt => Ok(cmp_bool(&lv, &rv, &[Ordering::Less])),
        BinOp::Gt => Ok(cmp_bool(&lv, &rv, &[Ordering::Greater])),
        BinOp::Le => Ok(cmp_bool(&lv, &rv, &[Ordering::Less, Ordering::Equal])),
        BinOp::Ge => Ok(cmp_bool(&lv, &rv, &[Ordering::Greater, Ordering::Equal])),
        BinOp::Concat => Ok(Value::str(format!("{}{}", lv.to_str(), rv.to_str()))),
        BinOp::Add => num_op(&lv, &rv, "+", Number::add),
        BinOp::Sub => num_op(&lv, &rv, "-", Number::sub),
        BinOp::Mul => num_op(&lv, &rv, "*", Number::mul),
        BinOp::Div => num_op_checked(&lv, &rv, "/", Number::div),
        BinOp::Mod => num_op_checked(&lv, &rv, "%", Number::rem),
        BinOp::Pow => pow_op(&lv, &rv, "^"),
    }
}

/// `base ^ exponent` (also the `POW(base, exp)` function). Both operands must be numeric. An
/// integer base with a non-negative integer exponent stays an exact integer when it fits
/// (`checked_pow`); otherwise the result is a float via `powf` (negative/fractional exponents,
/// float operands, or i64 overflow). `sym` names the operator/function for error messages.
fn pow_op(l: &Value, r: &Value, sym: &str) -> Result<Value, ExprError> {
    let a = l.to_number().map_err(|_| non_numeric(l, sym))?;
    let b = r.to_number().map_err(|_| non_numeric(r, sym))?;
    if let (Number::Int(base), Number::Int(exp)) = (a, b) {
        if let Ok(e) = u32::try_from(exp) {
            if let Some(p) = base.checked_pow(e) {
                return Ok(Value::Int(p));
            }
        }
    }
    Ok(Value::Float(a.as_f64().powf(b.as_f64())))
}

fn cmp_bool(l: &Value, r: &Value, accept: &[Ordering]) -> Value {
    let ord = xpl_cmp(l, r);
    Value::Int(if accept.contains(&ord) { 1 } else { 0 })
}

fn num_op(
    l: &Value,
    r: &Value,
    sym: &str,
    f: fn(Number, Number) -> Number,
) -> Result<Value, ExprError> {
    let a = l.to_number().map_err(|_| non_numeric(l, sym))?;
    let b = r.to_number().map_err(|_| non_numeric(r, sym))?;
    Ok(Value::from_number(f(a, b)))
}

fn num_op_checked(
    l: &Value,
    r: &Value,
    sym: &str,
    f: fn(Number, Number) -> Result<Number, crate::value::DivByZero>,
) -> Result<Value, ExprError> {
    let a = l.to_number().map_err(|_| non_numeric(l, sym))?;
    let b = r.to_number().map_err(|_| non_numeric(r, sym))?;
    match f(a, b) {
        Ok(n) => Ok(Value::from_number(n)),
        Err(_) => Err(ExprError::new(if sym == "/" {
            "division by zero"
        } else {
            "modulo by zero"
        })),
    }
}

fn non_numeric(v: &Value, sym: &str) -> ExprError {
    ExprError::new(format!("can't apply '{sym}' to non-numeric value {:?}", v.to_str()))
}

// ---- string functions (the §7 table; QN _QPL_FUNCTIONS) --------------------

/// Every function name `eval_call` recognizes — the authoritative surface for documentation/coverage
/// (the functions themselves are `match` arms below). KEEP IN SYNC with `eval_call`; the test
/// `function_names_are_all_callable` checks every entry resolves, and the docs-coverage test requires
/// each to be documented in `docs/reference.typ`.
pub const FUNCTION_NAMES: &[&str] = &[
    // string
    "LEN", "UPPER", "LOWER", "STRIP", "LSTRIP", "RSTRIP", "TRIMLEFT", "TRIMRIGHT", "SUBSTR", "LEFT",
    "RIGHT", "INDEX", "CONTAINS", "REPLACE", "SPLIT", "SPLITS", "JOIN", "CHARS", "WORDS", "LINES",
    "CHR", "ASC",
    // string classification predicates (all → 1/0; false for an empty string)
    "ISDEC", "ISHEX", "ISINT", "ISFLOAT", "ISALPHA", "ISALNUM", "ISASCII", "ISUTF8",
    // arrays / collections
    "ITER", "ITERP", "PUSH", "POP", "SLICE", "SORT", "VAR", "SEL",
    // math
    "ABS", "INT", "SQRT", "SQ", "CUBE", "FAC", "EXP", "LN", "LOG", "POW", "BASE", "HEX",
    "SIN", "COS", "TAN", "ASIN", "ACOS", "ATAN", "ATAN2", "SINH", "COSH", "TANH", "ASINH", "ACOSH",
    "ATANH", "PI", "E", "RANDOM",
    // editor / environment
    "KEY", "GETPATH", "EXISTS", "ISDIR", "ISFILE", "READFILE", "RF",
    // date / time (masked)
    "DATE", "TIME",
];

/// `true` if `chars[i..]` begins with the ASCII `pat`.
fn mask_at(chars: &[char], i: usize, pat: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    i + p.len() <= chars.len() && chars[i..i + p.len()] == p[..]
}

/// Format `now` by a DATE mask: `yyyy`/`yy`, `mm`/`m`, `dd`/`d`; everything else is a literal.
fn format_date(mask: &str, now: &DateParts) -> String {
    let chars: Vec<char> = mask.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if mask_at(&chars, i, "yyyy") {
            out.push_str(&format!("{:04}", now.year));
            i += 4;
        } else if mask_at(&chars, i, "yy") {
            out.push_str(&format!("{:02}", now.year.rem_euclid(100)));
            i += 2;
        } else if mask_at(&chars, i, "mm") {
            out.push_str(&format!("{:02}", now.month));
            i += 2;
        } else if chars[i] == 'm' {
            out.push_str(&now.month.to_string());
            i += 1;
        } else if mask_at(&chars, i, "dd") {
            out.push_str(&format!("{:02}", now.day));
            i += 2;
        } else if chars[i] == 'd' {
            out.push_str(&now.day.to_string());
            i += 1;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Convert a 24-hour hour to its 12-hour value (12, 1..=11).
fn to_12_hour(h: u32) -> u32 {
    match h % 12 {
        0 => 12,
        r => r,
    }
}

/// `true` if `chars[i]`,`chars[i+1]` is a meridiem marker (`am`/`pm`, any case); returns whether it's
/// uppercase.
fn meridiem_at(chars: &[char], i: usize) -> Option<bool> {
    if i + 1 >= chars.len() {
        return None;
    }
    let (a, b) = (chars[i], chars[i + 1]);
    if matches!(a, 'a' | 'A' | 'p' | 'P') && matches!(b, 'm' | 'M') {
        Some(a.is_ascii_uppercase())
    } else {
        None
    }
}

/// Format `now` by a TIME mask: `hh`/`h`, `mm`/`m`, `ss`/`s`; an `am`/`pm`/`AM`/`PM` marker forces
/// 12-hour and appends the correct meridiem in the marker's case. Everything else is a literal.
fn format_time(mask: &str, now: &DateParts) -> String {
    let chars: Vec<char> = mask.chars().collect();
    // A meridiem marker anywhere switches the hour display to 12-hour for the whole mask.
    let twelve = (0..chars.len()).any(|i| meridiem_at(&chars, i).is_some());
    let hour = if twelve { to_12_hour(now.hour) } else { now.hour };
    let pm = now.hour >= 12;
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if let Some(upper) = meridiem_at(&chars, i) {
            out.push_str(match (pm, upper) {
                (false, true) => "AM",
                (true, true) => "PM",
                (false, false) => "am",
                (true, false) => "pm",
            });
            i += 2;
        } else if mask_at(&chars, i, "hh") {
            out.push_str(&format!("{hour:02}"));
            i += 2;
        } else if chars[i] == 'h' {
            out.push_str(&hour.to_string());
            i += 1;
        } else if mask_at(&chars, i, "mm") {
            out.push_str(&format!("{:02}", now.minute));
            i += 2;
        } else if chars[i] == 'm' {
            out.push_str(&now.minute.to_string());
            i += 1;
        } else if mask_at(&chars, i, "ss") {
            out.push_str(&format!("{:02}", now.second));
            i += 2;
        } else if chars[i] == 's' {
            out.push_str(&now.second.to_string());
            i += 1;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Trim trailing path separators (`/` or `\`) without emptying the string (so `"/"` stays `"/"`).
/// Used by `GETPATH` to return a directory without a trailing slash.
fn strip_trailing_seps(s: &str) -> &str {
    let t = s.trim_end_matches(['/', '\\']);
    if t.is_empty() {
        s
    } else {
        t
    }
}

fn eval_call(name: &str, args: &[Expr], ctx: &dyn EvalContext) -> Result<Value, ExprError> {
    // (arity, then handle below). -1 means handled with its own arity check.
    let argc = args.len();
    let expect = |want: usize| -> Result<(), ExprError> {
        if argc != want {
            Err(ExprError::new(format!(
                "{name}: expected {want} arg{}, got {argc}",
                if want == 1 { "" } else { "s" }
            )))
        } else {
            Ok(())
        }
    };
    // Evaluate all args up front (the table functions are all eager).
    let mut vals = Vec::with_capacity(argc);
    for a in args {
        vals.push(eval(a, ctx)?);
    }
    let s = |i: usize| vals[i].to_str();

    match name {
        "LEN" => {
            expect(1)?;
            // Extended over QN: LEN of an array/map is its element/entry count;
            // otherwise the string length (char count).
            Ok(Value::Int(match &vals[0] {
                Value::Array(items) => items.len() as i64,
                Value::Map(m) => m.len() as i64,
                _ => s(0).chars().count() as i64,
            }))
        }
        // `ITER(x)` — the sequence a `FOR … IN` walks: an array's elements, a map's keys (sorted),
        // a string's characters, or a single-element list for a scalar. Also useful standalone
        // (e.g. `JOIN(ITER($map), ",")` lists a map's keys).
        "ITER" => {
            expect(1)?;
            Ok(Value::Array(match &vals[0] {
                Value::Array(items) => items.clone(),
                Value::Map(m) => m.keys().map(|k| Value::str(k.clone())).collect(),
                Value::Str(s) => s.chars().map(|c| Value::str(c.to_string())).collect(),
                other => vec![other.clone()],
            }))
        }
        // `ITERP(x)` — like `ITER` but pairs: an array of two-element `[key, value]` lists. Drives the
        // tuple `FOR k, v IN x` form. Array → `[index, element]`; map → `[key, value]` (sorted keys);
        // string → `[index, char]`; scalar → a single `[0, x]`.
        "ITERP" => {
            expect(1)?;
            let pair = |k: Value, v: Value| Value::Array(vec![k, v]);
            Ok(Value::Array(match &vals[0] {
                Value::Array(items) => items
                    .iter()
                    .enumerate()
                    .map(|(i, e)| pair(Value::Int(i as i64), e.clone()))
                    .collect(),
                Value::Map(m) => {
                    m.iter().map(|(k, v)| pair(Value::str(k.clone()), v.clone())).collect()
                }
                Value::Str(s) => s
                    .chars()
                    .enumerate()
                    .map(|(i, c)| pair(Value::Int(i as i64), Value::str(c.to_string())))
                    .collect(),
                other => vec![pair(Value::Int(0), other.clone())],
            }))
        }
        // Array helpers — all PURE (return a NEW array; the source is unchanged), so you mutate via
        // `SET a = PUSH($a, v)`. A non-array argument is coerced leniently (see `as_array`).
        "PUSH" => {
            expect(2)?;
            let mut a = as_array(&vals[0]);
            a.push(vals[1].clone());
            Ok(Value::Array(a))
        }
        "POP" => {
            // Returns the array WITHOUT its last element (empty → empty). Read the popped value
            // first if you need it (`$a[LEN($a)-1]`).
            expect(1)?;
            let mut a = as_array(&vals[0]);
            a.pop();
            Ok(Value::Array(a))
        }
        "SLICE" => {
            // `SLICE(arr, start [, end])` — `end` exclusive (default = length). Negative indices
            // count from the end; both are clamped to the array, so out-of-range is lenient.
            if argc != 2 && argc != 3 {
                return Err(ExprError::new(format!("SLICE: expected 2 or 3 args, got {argc}")));
            }
            let a = as_array(&vals[0]);
            let len = a.len() as i64;
            let resolve = |i: i64| (if i < 0 { len + i } else { i }).clamp(0, len) as usize;
            let start = resolve(int_arg(&vals[1], "SLICE")?);
            let end = if argc == 3 { resolve(int_arg(&vals[2], "SLICE")?) } else { len as usize };
            Ok(Value::Array(a[start..end.max(start)].to_vec()))
        }
        "SORT" => {
            // `SORT(arr [, reverse])` — ascending by the same ordering as `<` (numbers numerically,
            // strings lexically); a truthy second arg sorts descending. Stable.
            if argc != 1 && argc != 2 {
                return Err(ExprError::new(format!("SORT: expected 1 or 2 args, got {argc}")));
            }
            let mut a = as_array(&vals[0]);
            a.sort_by(crate::value::xpl_cmp);
            if argc == 2 && vals[1].is_truthy() {
                a.reverse();
            }
            Ok(Value::Array(a))
        }
        // Array constructors (QN had these as SET-RHS special forms; XyScript
        // makes them ordinary expression functions returning a real array).
        "SPLIT" => {
            expect(2)?;
            Ok(Value::Array(split_on_substring(&s(0), &s(1))))
        }
        "SPLITS" => {
            expect(2)?;
            Ok(Value::Array(split_on_any(&s(0), &s(1))))
        }
        // Sweeteners over SPLIT: plain 0-based arrays (IN-friendly like the rest; count = `LEN(...)`).
        "CHARS" => {
            expect(1)?;
            Ok(Value::Array(s(0).chars().map(|c| Value::str(c.to_string())).collect()))
        }
        "WORDS" => {
            // Words = runs of non-whitespace (Unicode-aware); leading/trailing/repeated whitespace
            // collapses, so no empty items.
            expect(1)?;
            Ok(Value::Array(s(0).split_whitespace().map(Value::str).collect()))
        }
        "LINES" => {
            // Lines split on `\n` with any trailing `\r` removed (CRLF-safe); a final newline does
            // NOT yield a trailing empty line (`str::lines` semantics).
            expect(1)?;
            Ok(Value::Array(s(0).lines().map(Value::str).collect()))
        }
        "JOIN" => {
            expect(2)?;
            let sep = s(1);
            let joined = match &vals[0] {
                Value::Array(items) => {
                    items.iter().map(|v| v.to_str()).collect::<Vec<_>>().join(&sep)
                }
                other => other.to_str(),
            };
            Ok(Value::str(joined))
        }
        "UPPER" => {
            expect(1)?;
            Ok(Value::str(s(0).to_uppercase()))
        }
        // `GETPATH(p)` — split a path into a 4-element array: [directory (no trailing slash),
        // filename+extension, filename only, extension only]. If `p` is an EXISTING DIRECTORY the
        // whole path is the directory and the three filename parts are empty (so a directory's last
        // component isn't returned as a filename). Otherwise `p` is parsed as a file path (works for
        // a non-existent path too). The returned parts preserve the input spelling; only the
        // directory TEST resolves against `$CWD`.
        "GETPATH" => {
            expect(1)?;
            let raw = s(0);
            let (dir, name_ext, name, ext) = if ctx.path_is_dir(&raw) {
                (strip_trailing_seps(&raw).to_string(), String::new(), String::new(), String::new())
            } else {
                let p = std::path::Path::new(&raw);
                let part = |o: Option<&std::ffi::OsStr>| -> String {
                    o.map(|x| x.to_string_lossy().into_owned()).unwrap_or_default()
                };
                (
                    part(p.parent().map(|d| d.as_os_str())),
                    part(p.file_name()),
                    part(p.file_stem()),
                    part(p.extension()),
                )
            };
            Ok(Value::Array(vec![
                Value::str(dir),
                Value::str(name_ext),
                Value::str(name),
                Value::str(ext),
            ]))
        }
        // Filesystem predicates (paths resolve against `$CWD`): `EXIST(p)` = exists at all,
        // `ISDIR(p)` = is a directory, `ISFILE(p)` = exists and is not a directory. Each → 1/0.
        "EXISTS" => {
            expect(1)?;
            Ok(Value::Int(if ctx.path_exists(&s(0)) { 1 } else { 0 }))
        }
        "ISDIR" => {
            expect(1)?;
            Ok(Value::Int(if ctx.path_is_dir(&s(0)) { 1 } else { 0 }))
        }
        "ISFILE" => {
            expect(1)?;
            let p = s(0);
            Ok(Value::Int(if ctx.path_exists(&p) && !ctx.path_is_dir(&p) { 1 } else { 0 }))
        }
        // `READFILE(path)` / `RF(path)` — the file's text as a string (host-resolved against `$CWD`).
        // A missing/unreadable/non-UTF-8 file is a trappable error. Pairs with the `WRITEFILE` verb.
        "READFILE" | "RF" => {
            expect(1)?;
            ctx.read_file(&s(0)).map(|t| Value::str(t)).map_err(ExprError::trappable)
        }
        // `DATE(mask)` / `TIME(mask)` — the current LOCAL date/time formatted by a mask. DATE tokens:
        // `yyyy`/`yy` (year), `mm`/`m` (month), `dd`/`d` (day). TIME tokens: `hh`/`h`, `mm`/`m`,
        // `ss`/`s`; an `am`/`pm` (lowercase) or `AM`/`PM` (uppercase) marker switches the hour to
        // 12-hour and appends the correct meridiem in that case. Anything else is a literal.
        "DATE" => {
            expect(1)?;
            let now = ctx.now().ok_or_else(|| ExprError::new("DATE: no clock in this context"))?;
            Ok(Value::str(format_date(&s(0), &now)))
        }
        "TIME" => {
            expect(1)?;
            let now = ctx.now().ok_or_else(|| ExprError::new("TIME: no clock in this context"))?;
            Ok(Value::str(format_time(&s(0), &now)))
        }
        "LOWER" => {
            expect(1)?;
            Ok(Value::str(s(0).to_lowercase()))
        }
        "STRIP" => {
            expect(1)?;
            Ok(Value::str(s(0).trim().to_string()))
        }
        "LSTRIP" => {
            expect(1)?;
            Ok(Value::str(s(0).trim_start().to_string()))
        }
        "RSTRIP" => {
            expect(1)?;
            Ok(Value::str(s(0).trim_end().to_string()))
        }
        "TRIMLEFT" => {
            expect(2)?;
            let n = int_arg(&vals[1], "TRIMLEFT")?;
            Ok(Value::str(trim_left(&s(0), n)))
        }
        "TRIMRIGHT" => {
            expect(2)?;
            let n = int_arg(&vals[1], "TRIMRIGHT")?;
            Ok(Value::str(trim_right(&s(0), n)))
        }
        "SUBSTR" => {
            expect(3)?;
            let start = int_arg(&vals[1], "SUBSTR")?;
            let len = int_arg(&vals[2], "SUBSTR")?;
            Ok(Value::str(substr(&s(0), start, len)))
        }
        "LEFT" => {
            expect(2)?;
            let n = int_arg(&vals[1], "LEFT")?;
            Ok(Value::str(left(&s(0), n)))
        }
        "RIGHT" => {
            expect(2)?;
            let n = int_arg(&vals[1], "RIGHT")?;
            Ok(Value::str(right(&s(0), n)))
        }
        "INDEX" => {
            expect(2)?;
            Ok(Value::Int(char_index(&s(0), &s(1))))
        }
        "CONTAINS" => {
            expect(2)?;
            Ok(Value::Int(if s(0).contains(&s(1)) { 1 } else { 0 }))
        }
        "REPLACE" => {
            // `REPLACE(s, from, to)` replaces ALL occurrences; the optional 4th arg bounds it:
            // `n > 0` = at most n from the start, `n < 0` = at most |n| from the END, `0` = none.
            if argc != 3 && argc != 4 {
                return Err(ExprError::new(format!("REPLACE: expected 3 or 4 args, got {argc}")));
            }
            if argc == 3 {
                Ok(Value::str(s(0).replace(&s(1), &s(2))))
            } else {
                let n = int_arg(&vals[3], "REPLACE")?;
                Ok(Value::str(replace_bounded(&s(0), &s(1), &s(2), n)))
            }
        }
        // String-classification predicates → 1/0. Each "all characters are …" test is FALSE for an
        // empty string (nothing to classify), matching the usual convention. The character-class
        // tests use ASCII ranges exactly as specified; they do NOT trim, so any stray space fails.
        "ISDEC" => {
            expect(1)?;
            Ok(all_chars(&s(0), |c| c.is_ascii_digit())) // [0-9]
        }
        "ISHEX" => {
            expect(1)?;
            Ok(all_chars(&s(0), |c| c.is_ascii_hexdigit())) // [0-9A-Fa-f]
        }
        "ISALPHA" => {
            expect(1)?;
            Ok(all_chars(&s(0), |c| c.is_ascii_alphabetic())) // [A-Za-z]
        }
        "ISALNUM" => {
            expect(1)?;
            Ok(all_chars(&s(0), |c| c.is_ascii_alphanumeric())) // [0-9A-Za-z]
        }
        "ISASCII" => {
            expect(1)?;
            Ok(all_chars(&s(0), |c| c.is_ascii())) // U+0000–U+007F
        }
        "ISUTF8" => {
            // Every char is in the Basic Multilingual Plane (U+0000–U+FFFF) — i.e. no astral chars
            // like emoji. (The string is already valid UTF-8, so this is the meaningful test.)
            expect(1)?;
            Ok(all_chars(&s(0), |c| (c as u32) <= 0xFFFF))
        }
        "ISINT" => {
            // A whole number, as XPL itself reads it: trimmed and parsing as an integer (value.rs
            // `to_number` tries `i64` before `f64`). `ISINT` and `ISFLOAT` partition numeric strings.
            expect(1)?;
            Ok(bool_val(s(0).trim().parse::<i64>().is_ok()))
        }
        "ISFLOAT" => {
            // A number written in FLOAT form: not an integer, but a finite `f64` — so `4.2`/`1e3`
            // are true, `42` is `ISINT` (false here), and `inf`/`nan` are excluded.
            expect(1)?;
            let t = s(0);
            let t = t.trim();
            Ok(bool_val(t.parse::<i64>().is_err() && t.parse::<f64>().map(f64::is_finite).unwrap_or(false)))
        }
        "SEL" => {
            // `SEL()` = the current selection; `SEL(n)` = tab n's selection (1-based; "" if out of
            // range or that tab has no selection).
            match argc {
                0 => Ok(Value::str(ctx.sel())),
                1 => Ok(Value::str(ctx.sel_at(int_arg(&vals[0], "SEL")?))),
                _ => Err(ExprError::new(format!("SEL: expected 0 or 1 args, got {argc}"))),
            }
        }
        "VAR" => {
            expect(1)?;
            Ok(ctx.get_var(&s(0)).unwrap_or_else(|| Value::str("")))
        }
        "KEY" => {
            // `KEY(scan)` — the current binding of key code `scan` (unshifted layer) as a string;
            // empty when unbound / out of range / no keymap loaded.
            expect(1)?;
            let scan = int_arg(&vals[0], "KEY")?;
            Ok(Value::str(ctx.key_binding(scan).unwrap_or_default()))
        }
        "CHR" => {
            expect(1)?;
            Ok(Value::str(chr(&s(0))))
        }
        "ASC" => {
            expect(1)?;
            let st = s(0);
            Ok(Value::str(match st.chars().next() {
                Some(c) => (c as u32).to_string(),
                None => "0".to_string(),
            }))
        }
        // Unary math functions. Trig/hyperbolic/log/root/power all return a float (rendered as an
        // integer when whole, e.g. `SQRT(4)` → `2`); out-of-domain inputs (e.g. `SQRT(-1)`,
        // `LN(0)`) yield NaN/inf rather than erroring. Angles are in radians. `LOG` = base 10,
        // `LN` = natural.
        "SIN" | "COS" | "TAN" | "ASIN" | "ACOS" | "ATAN" | "SINH" | "COSH" | "TANH" | "ASINH"
        | "ACOSH" | "ATANH" | "LOG" | "LN" | "EXP" | "SQRT" | "SQ" | "CUBE" => {
            expect(1)?;
            let x = num_arg(&vals[0], name)?;
            let r = match name {
                "SIN" => x.sin(),
                "COS" => x.cos(),
                "TAN" => x.tan(),
                "ASIN" => x.asin(),
                "ACOS" => x.acos(),
                "ATAN" => x.atan(),
                "SINH" => x.sinh(),
                "COSH" => x.cosh(),
                "TANH" => x.tanh(),
                "ASINH" => x.asinh(),
                "ACOSH" => x.acosh(),
                "ATANH" => x.atanh(),
                "LOG" => x.log10(),
                "LN" => x.ln(),
                "EXP" => x.exp(), // e^x — the natural exponential (standard `exp()`)
                "SQRT" => x.sqrt(),
                "SQ" => x * x,
                "CUBE" => x * x * x,
                _ => unreachable!(),
            };
            Ok(Value::Float(r))
        }
        // `ABS` preserves int-ness (`ABS(-5)` → `5`, `ABS(-5.5)` → `5.5`).
        "ABS" => {
            expect(1)?;
            match vals[0].to_number() {
                Ok(Number::Int(i)) => Ok(Value::Int(i.wrapping_abs())),
                Ok(Number::Float(f)) => Ok(Value::Float(f.abs())),
                Err(_) => Err(ExprError::new(format!("ABS: can't convert {:?} to number", s(0)))),
            }
        }
        // `INT` truncates toward zero to a whole number (`INT(3.7)` → `3`, `INT(-3.7)` → `-3`).
        "INT" => {
            expect(1)?;
            Ok(Value::Int(num_arg(&vals[0], "INT")?.trunc() as i64))
        }
        // `FAC(n)` — factorial of a non-negative integer; exact (i64) up to 20!, then a float.
        "FAC" => {
            expect(1)?;
            let x = num_arg(&vals[0], "FAC")?;
            if x < 0.0 {
                return Err(ExprError::new("FAC: negative argument"));
            }
            let n = x.trunc() as u64;
            let mut int_acc: Option<i64> = Some(1);
            let mut float_acc = 1.0f64;
            for k in 2..=n {
                float_acc *= k as f64;
                int_acc = int_acc.and_then(|a| a.checked_mul(k as i64));
            }
            Ok(match int_acc {
                Some(i) => Value::Int(i),
                None => Value::Float(float_acc),
            })
        }
        // `ATAN2(y, x)` — the angle (radians) of the point `(x, y)`, quadrant-correct.
        "ATAN2" => {
            expect(2)?;
            let y = num_arg(&vals[0], "ATAN2")?;
            let x = num_arg(&vals[1], "ATAN2")?;
            Ok(Value::Float(y.atan2(x)))
        }
        // `POW(base, exponent)` — `base` raised to `exponent` (the named twin of the `^` operator);
        // both may be int or float. An integer base with a non-negative integer exponent stays an
        // exact integer when it fits (e.g. `POW(2, 10)` → `1024`); otherwise it's a float
        // (`POW(2, 0.5)` → `1.414…`, `POW(2, -1)` → `0.5`). Out-of-domain (`POW(-1, 0.5)`) → NaN.
        "POW" => {
            expect(2)?;
            pow_op(&vals[0], &vals[1], "POW")
        }
        // `RANDOM(min, max)` — a random integer in the INCLUSIVE range `[min, max]`, from the host's
        // seedable generator (`SRANDOM` seeds it; unseeded it starts from an entropy-based value).
        // Non-integer args are truncated toward zero; `min > max` is tolerated (the range is swapped).
        "RANDOM" => {
            expect(2)?;
            let min = num_arg(&vals[0], "RANDOM")?.trunc() as i64;
            let max = num_arg(&vals[1], "RANDOM")?.trunc() as i64;
            ctx.random_int(min, max)
                .map(Value::Int)
                .ok_or_else(|| ExprError::new("RANDOM: no generator in this context"))
        }
        // Mathematical constants — called with empty parens: `PI()`, `E()`.
        "PI" => {
            expect(0)?;
            Ok(Value::Float(std::f64::consts::PI))
        }
        "E" => {
            expect(0)?;
            Ok(Value::Float(std::f64::consts::E))
        }
        // `BASE(number, base_in, base_out)` — reinterpret the integer string `number` (given in
        // `base_in`) as a string in `base_out`. Bases are 2–36; input digits are case-insensitive,
        // output letters are uppercase; an optional leading `-` is allowed. e.g.
        // `BASE("FF", 16, 10)` → `255`, `BASE("255", 10, 2)` → `11111111`.
        "HEX" => {
            // HEX(n) / HEX(n, p): lowercase hexadecimal of integer `n`, zero-padded on the LEFT to at
            // least `p` digits (never truncated — a value wider than `p` keeps all its digits). A
            // negative `n` prints as `-<hex>` with the sign outside the padding.
            if !(1..=2).contains(&argc) {
                return Err(ExprError::new(format!("HEX: expected 1 or 2 args, got {argc}")));
            }
            let n = int_arg(&vals[0], "HEX")?;
            let pad = if argc == 2 { int_arg(&vals[1], "HEX")?.max(0).min(1024) as usize } else { 0 };
            let mag = (n as i128).unsigned_abs();
            let mut hex = format!("{mag:x}"); // lowercase magnitude ("0" for 0)
            if hex.len() < pad {
                hex.insert_str(0, &"0".repeat(pad - hex.len()));
            }
            Ok(Value::str(if n < 0 { format!("-{hex}") } else { hex }))
        }
        "BASE" => {
            expect(3)?;
            let num = s(0);
            let base_in = int_arg(&vals[1], "BASE")?;
            let base_out = int_arg(&vals[2], "BASE")?;
            if !(2..=36).contains(&base_in) || !(2..=36).contains(&base_out) {
                return Err(ExprError::new("BASE: bases must be in 2..=36"));
            }
            let n = i64::from_str_radix(num.trim(), base_in as u32).map_err(|_| {
                ExprError::new(format!("BASE: {num:?} is not a base-{base_in} integer"))
            })?;
            Ok(Value::str(to_radix(n, base_out as u32)))
        }
        _ => Err(ExprError::new(format!("unknown function {name:?}"))),
    }
}

/// Coerce an argument to an integer for the count/index functions, surfacing the
/// error with the function's name (matching QN's wrapped messages).
/// Coerce a value to an array for the array helpers (`PUSH`/`POP`/`SLICE`/`SORT`): an array is used
/// as-is; an empty string (or unset var) → `[]` (so building from nothing works); any other scalar
/// → a one-element array. Returns an owned copy (the helpers are pure).
fn as_array(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(items) => items.clone(),
        Value::Str(s) if s.is_empty() => Vec::new(),
        other => vec![other.clone()],
    }
}

fn int_arg(v: &Value, fname: &str) -> Result<i64, ExprError> {
    match v.to_number() {
        Ok(Number::Int(i)) => Ok(i),
        Ok(Number::Float(f)) => Ok(f as i64),
        Err(_) => Err(ExprError::new(format!(
            "{fname}: can't convert {:?} to number",
            v.to_str()
        ))),
    }
}

/// Format `n` in `radix` (2–36) with uppercase letter-digits and a leading `-` for negatives.
/// Works at i128 width internally so `i64::MIN` is handled.
fn to_radix(n: i64, radix: u32) -> String {
    const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    if n == 0 {
        return "0".to_string();
    }
    let mut v = (n as i128).unsigned_abs();
    let mut buf = Vec::new();
    while v > 0 {
        buf.push(DIGITS[(v % radix as u128) as usize]);
        v /= radix as u128;
    }
    if n < 0 {
        buf.push(b'-');
    }
    buf.reverse();
    String::from_utf8(buf).expect("ASCII digits")
}

/// Coerce an argument to a float for the math functions, surfacing the error with the
/// function's name.
fn num_arg(v: &Value, fname: &str) -> Result<f64, ExprError> {
    match v.to_number() {
        Ok(n) => Ok(n.as_f64()),
        Err(_) => Err(ExprError::new(format!(
            "{fname}: can't convert {:?} to number",
            v.to_str()
        ))),
    }
}

fn trim_left(s: &str, n: i64) -> String {
    if n <= 0 {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    if (n as usize) >= chars.len() {
        return String::new();
    }
    chars[n as usize..].iter().collect()
}

fn trim_right(s: &str, n: i64) -> String {
    if n <= 0 {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    if (n as usize) >= chars.len() {
        return String::new();
    }
    chars[..chars.len() - n as usize].iter().collect()
}

/// The leftmost `n` characters of `s` (whole string if `n` exceeds its length; empty if `n <= 0`).
fn left(s: &str, n: i64) -> String {
    if n <= 0 {
        return String::new();
    }
    s.chars().take(n as usize).collect()
}

/// The rightmost `n` characters of `s` (whole string if `n` exceeds its length; empty if `n <= 0`).
fn right(s: &str, n: i64) -> String {
    if n <= 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    let start = chars.len().saturating_sub(n as usize);
    chars[start..].iter().collect()
}

/// A boolean predicate result in the function library's convention: `1`/`0` (matching
/// `CONTAINS`/`ISDIR`/`ISFILE`), not a `Value::Bool`.
fn bool_val(b: bool) -> Value {
    Value::Int(if b { 1 } else { 0 })
}

/// True iff `s` is non-empty and EVERY character satisfies `pred` — the shared shape of the
/// character-class predicates (`ISDEC`, `ISHEX`, …). Empty is false by decision (nothing to classify).
fn all_chars(s: &str, pred: impl Fn(char) -> bool) -> Value {
    bool_val(!s.is_empty() && s.chars().all(pred))
}

/// Replace up to `n` non-overlapping occurrences of `from` with `to`. `n > 0` takes the FIRST `n`
/// (left to right); `n < 0` takes the LAST `|n|` (right to left); `n == 0` replaces nothing. An
/// empty `from` matches nothing here (returns `s` unchanged) — the unbounded 3-arg `REPLACE` keeps
/// Rust's `str::replace` semantics for that corner. Byte indices come from `find`/`rfind`, so they
/// always land on char boundaries (`from` is a substring), keeping the slicing UTF-8-safe. The
/// direction genuinely matters for self-overlapping patterns: `REPLACE("aaa","aa","X",-1)` → `"aX"`
/// (rightmost match), whereas `+1` → `"Xa"` (leftmost).
fn replace_bounded(s: &str, from: &str, to: &str, n: i64) -> String {
    if from.is_empty() || n == 0 {
        return s.to_string();
    }
    if n > 0 {
        // First `n`, scanning left to right.
        let limit = n as usize;
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        let mut done = 0;
        while done < limit {
            match rest.find(from) {
                Some(pos) => {
                    out.push_str(&rest[..pos]);
                    out.push_str(to);
                    rest = &rest[pos + from.len()..];
                    done += 1;
                }
                None => break,
            }
        }
        out.push_str(rest);
        out
    } else {
        // Last `|n|`, scanning right to left so overlapping matches are picked from the end.
        let limit = n.unsigned_abs() as usize;
        let mut starts: Vec<usize> = Vec::new(); // match starts, collected descending
        let mut end = s.len();
        while starts.len() < limit {
            match s[..end].rfind(from) {
                Some(pos) => {
                    starts.push(pos);
                    end = pos; // next match must not overlap this one
                }
                None => break,
            }
        }
        starts.reverse(); // ascending, to rebuild left to right
        let mut out = String::with_capacity(s.len());
        let mut last = 0;
        for pos in starts {
            out.push_str(&s[last..pos]);
            out.push_str(to);
            last = pos + from.len();
        }
        out.push_str(&s[last..]);
        out
    }
}

fn substr(s: &str, start: i64, len: i64) -> String {
    let start = start.max(0) as usize;
    if len <= 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if start >= chars.len() {
        return String::new();
    }
    let end = (start + len as usize).min(chars.len());
    chars[start..end].iter().collect()
}

/// 0-based char index of `needle` in `haystack`, or -1.
fn char_index(haystack: &str, needle: &str) -> i64 {
    match haystack.find(needle) {
        Some(byte_idx) => haystack[..byte_idx].chars().count() as i64,
        None => -1,
    }
}

/// `SPLIT(s, sep)`: split `s` around the literal substring `sep`. An empty `sep`
/// yields each character as an element (QN `_split_string`).
fn split_on_substring(s: &str, sep: &str) -> Vec<Value> {
    if sep.is_empty() {
        return s.chars().map(|c| Value::str(c.to_string())).collect();
    }
    s.split(sep).map(Value::str).collect()
}

/// `SPLITS(s, charset)`: split `s` around ANY single character in `charset`. An
/// empty `charset` yields each character (QN `_splits_string`).
fn split_on_any(s: &str, charset: &str) -> Vec<Value> {
    if charset.is_empty() {
        return s.chars().map(|c| Value::str(c.to_string())).collect();
    }
    let set: Vec<char> = charset.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if set.contains(&ch) {
            out.push(Value::str(std::mem::take(&mut cur)));
        } else {
            cur.push(ch);
        }
    }
    out.push(Value::str(cur));
    out
}

/// CHR(n): single char for code point 0..=255, else "".
fn chr(arg: &str) -> String {
    match arg.trim().parse::<i64>() {
        Ok(n) if (0..=255).contains(&n) => (n as u8 as char).to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Minimal context: a name->Value map and a fixed selection.
    struct Ctx {
        vars: HashMap<String, Value>,
        sel: String,
        binders: HashMap<(bool, char), String>,
        now: Option<DateParts>,
    }
    impl Ctx {
        fn new() -> Ctx {
            Ctx { vars: HashMap::new(), sel: String::new(), binders: HashMap::new(), now: None }
        }
        fn with_now(mut self, now: DateParts) -> Ctx {
            self.now = Some(now);
            self
        }
        fn with(mut self, k: &str, v: Value) -> Ctx {
            self.vars.insert(fold_ident(k), v);
            self
        }
        fn with_binder(mut self, script: bool, slot: char, text: &str) -> Ctx {
            self.binders.insert((script, slot), text.to_string());
            self
        }
    }
    impl EvalContext for Ctx {
        fn get_var(&self, name: &str) -> Option<Value> {
            self.vars.get(&fold_ident(name)).cloned()
        }
        fn sel(&self) -> String {
            self.sel.clone()
        }
        fn binder(&self, script: bool, slot: char) -> Option<String> {
            self.binders.get(&(script, slot)).cloned()
        }
        fn now(&self) -> Option<DateParts> {
            self.now
        }
    }

    fn ev(s: &str) -> Value {
        eval(&parse(s).unwrap(), &Ctx::new()).unwrap()
    }
    fn ev_ctx(s: &str, ctx: &Ctx) -> Value {
        eval(&parse(s).unwrap(), ctx).unwrap()
    }

    #[test]
    fn date_mask_formats_year_month_day() {
        // 2026-06-07 (single-digit month/day so padding is observable).
        let p = DateParts { year: 2026, month: 6, day: 7, hour: 0, minute: 0, second: 0 };
        assert_eq!(format_date("m-d-yyyy", &p), "6-7-2026");
        assert_eq!(format_date("mm-dd-yy", &p), "06-07-26");
        assert_eq!(format_date("yyyy/mm/dd", &p), "2026/06/07");
        assert_eq!(format_date("(mm.dd.yyyy)", &p), "(06.07.2026)"); // non-letter separators pass through
    }

    #[test]
    fn time_mask_24h_and_12h_meridiem() {
        let pm = DateParts { year: 2026, month: 6, day: 7, hour: 15, minute: 5, second: 9 };
        // 24-hour (no marker).
        assert_eq!(format_time("hh:mm:ss", &pm), "15:05:09");
        assert_eq!(format_time("h:mm", &pm), "15:05");
        // 12-hour, marker case controls the meridiem case.
        assert_eq!(format_time("h:mm PM", &pm), "3:05 PM");
        assert_eq!(format_time("h:mm am", &pm), "3:05 pm");
        // Midnight and noon edges.
        let midnight = DateParts { hour: 0, minute: 0, ..pm };
        assert_eq!(format_time("h:mm am", &midnight), "12:00 am");
        let noon = DateParts { hour: 12, minute: 30, ..pm };
        assert_eq!(format_time("hh:mm PM", &noon), "12:30 PM");
    }

    #[test]
    fn date_time_through_eval_with_unquoted_mask() {
        let ctx = Ctx::new()
            .with_now(DateParts { year: 2026, month: 6, day: 7, hour: 15, minute: 5, second: 9 });
        // The mask is unquoted and would otherwise parse as subtraction.
        assert_eq!(ev_ctx("DATE(m-d-yyyy)", &ctx).to_str(), "6-7-2026");
        assert_eq!(ev_ctx("TIME(h:mm am)", &ctx).to_str(), "3:05 pm");
        // Quoted form also accepted (for masks with characters the lexer rejects).
        assert_eq!(ev_ctx(r#"DATE("mm.dd.yy")"#, &ctx).to_str(), "06.07.26");
        // No clock → a trappable error rather than a panic.
        assert!(eval(&parse("DATE(yyyy)").unwrap(), &Ctx::new()).is_err());
    }

    #[test]
    fn hex_formats_lowercase_padded_not_truncated() {
        assert_eq!(ev("HEX(255)").to_str(), "ff"); // lowercase, no pad
        assert_eq!(ev("HEX(255, 4)").to_str(), "00ff"); // left-padded to 4
        assert_eq!(ev("HEX(255, 1)").to_str(), "ff"); // p < digits → not truncated
        assert_eq!(ev("HEX(4096, 2)").to_str(), "1000"); // wider than p, kept whole
        assert_eq!(ev("HEX(0, 3)").to_str(), "000");
        assert_eq!(ev("HEX(0)").to_str(), "0");
        assert_eq!(ev("HEX(10)").to_str(), "a");
        assert_eq!(ev("HEX(-255, 4)").to_str(), "-00ff"); // sign outside the padding
        // A float argument truncates to an integer first.
        assert_eq!(ev("HEX(255.9)").to_str(), "ff");
        // Wrong arity is an error, not a panic.
        assert!(eval(&parse("HEX(1, 2, 3)").unwrap(), &Ctx::new()).is_err());
    }

    #[test]
    fn array_literal_and_indexing() {
        assert_eq!(ev("[1, 2, 3]").to_str(), "[1, 2, 3]");
        assert_eq!(ev("[]").to_str(), "[]");
        assert_eq!(ev("[1, 2, 3,]").to_str(), "[1, 2, 3]"); // trailing comma ok
        assert_eq!(ev("[10, 20, 30][1]"), Value::Int(20)); // postfix index on a literal
        assert_eq!(ev("LEN([1, 2, 3])"), Value::Int(3));
        assert_eq!(ev("[1, [2, 3], 4][1][0]"), Value::Int(2)); // nested
    }

    #[test]
    fn getpath_splits_a_file_path() {
        // The bare Ctx has no filesystem (path_is_dir = false), so every path is parsed as a file.
        assert_eq!(ev(r#"GETPATH("C:/foo/bar.txt")"#).to_str(), "[C:/foo, bar.txt, bar, txt]");
        assert_eq!(ev(r#"GETPATH("bar.txt")"#).to_str(), "[, bar.txt, bar, txt]"); // no directory
        assert_eq!(ev(r#"GETPATH("a.tar.gz")"#).to_str(), "[, a.tar.gz, a.tar, gz]"); // double ext
        // A leaf with no extension that ISN'T an existing dir is still parsed as a file.
        assert_eq!(ev(r#"GETPATH("C:/foo/sub")"#).to_str(), "[C:/foo, sub, sub, ]");
        // Element access.
        assert_eq!(ev(r#"GETPATH("C:/foo/bar.txt")[3]"#).to_str(), "txt");
    }

    #[test]
    fn array_helpers_push_pop_slice_sort() {
        assert_eq!(ev("PUSH([1, 2], 3)").to_str(), "[1, 2, 3]");
        assert_eq!(ev("POP([1, 2, 3])").to_str(), "[1, 2]");
        assert_eq!(ev("POP([])").to_str(), "[]"); // lenient
        assert_eq!(ev("SLICE([1, 2, 3, 4, 5], 1, 3)").to_str(), "[2, 3]"); // end exclusive
        assert_eq!(ev("SLICE([1, 2, 3, 4], 2)").to_str(), "[3, 4]"); // to end
        assert_eq!(ev("SLICE([1, 2, 3, 4], 0, -1)").to_str(), "[1, 2, 3]"); // negative = from end
        assert_eq!(ev("SLICE([1, 2, 3], 5, 9)").to_str(), "[]"); // out of range clamps
        assert_eq!(ev("SORT([3, 1, 2])").to_str(), "[1, 2, 3]");
        assert_eq!(ev("SORT([3, 1, 2], 1)").to_str(), "[3, 2, 1]"); // reverse
        assert_eq!(ev(r#"SORT(["b", "a", "c"])"#).to_str(), "[a, b, c]"); // strings
        // Coercion: building from an empty/unset value works.
        assert_eq!(ev(r#"PUSH("", 1)"#).to_str(), "[1]");
    }

    #[test]
    fn map_literal_and_indexing() {
        // Bare-word keys, sorted on render (BTreeMap).
        assert_eq!(ev("{b: 2, a: 1}").to_str(), "{a=1, b=2}");
        assert_eq!(ev("{}").to_str(), "{}");
        assert_eq!(ev("{a: 1, b: 2,}").to_str(), "{a=1, b=2}"); // trailing comma
        // Quoted keys + value expressions.
        assert_eq!(ev(r#"{"x": 1 + 2}["x"]"#), Value::Int(3));
        // Mixed value types, nesting, and indexing into a nested map.
        assert_eq!(ev(r#"{name: "Al", nums: [1, 2]}["nums"][1]"#), Value::Int(2));
        assert_eq!(ev(r#"{outer: {inner: 9}}["outer"]["inner"]"#), Value::Int(9));
        assert_eq!(ev("LEN({a: 1, b: 2})"), Value::Int(2));
        // Later duplicate key wins.
        assert_eq!(ev("{a: 1, a: 2}").to_str(), "{a=2}");
    }

    #[test]
    fn map_literal_computed_keys_and_iterp() {
        let ctx = Ctx::new().with("k", Value::str("dyn"));
        // A parenthesised key expression is collapsed to a string.
        assert_eq!(ev_ctx(r#"{($k): 5}["dyn"]"#, &ctx), Value::Int(5));
        // ITERP yields [key, value] pairs (drives the tuple FOR form).
        assert_eq!(ev("ITERP([10, 20])").to_str(), "[[0, 10], [1, 20]]");
        assert_eq!(ev("ITERP({a: 1, b: 2})").to_str(), "[[a, 1], [b, 2]]");
        assert_eq!(ev(r#"ITERP("hi")"#).to_str(), "[[0, h], [1, i]]");
    }

    #[test]
    fn chars_words_lines_are_plain_arrays() {
        // CHARS: plain 0-based array; count = LEN.
        assert_eq!(ev(r#"CHARS("abc")"#).to_str(), "[a, b, c]");
        assert_eq!(ev(r#"CHARS("abc")[0]"#).to_str(), "a");
        assert_eq!(ev(r#"LEN(CHARS("abc"))"#), Value::Int(3));
        assert_eq!(ev(r#"CHARS("")"#).to_str(), "[]"); // empty → empty array
        // WORDS: whitespace-collapsed, no empties.
        assert_eq!(ev(r#"WORDS("  the  quick fox ")"#).to_str(), "[the, quick, fox]");
        assert_eq!(ev(r#"WORDS("")"#).to_str(), "[]");
        // LINES: split on \n, trailing \r stripped, no trailing empty line.
        assert_eq!(ev(r#"LINES("a\nb\nc")"#).to_str(), "[a, b, c]");
        assert_eq!(ev(r#"LINES("a\nb\n")"#).to_str(), "[a, b]"); // final newline ≠ extra line
        assert_eq!(ev("LINES(\"a\r\nb\")").to_str(), "[a, b]"); // CRLF
    }

    // ---- arithmetic (oracle: 9/2==4, 9.0/2==4.5) ---------------------------

    #[test]
    fn arithmetic_matches_oracle() {
        assert_eq!(ev("3 * 4"), Value::Int(12));
        assert_eq!(ev("9 / 2"), Value::Int(4)); // integer floor
        assert_eq!(ev("9.0 / 2"), Value::Float(4.5)); // float
        assert_eq!(ev("7 % 3"), Value::Int(1));
        assert_eq!(ev("2 + 3 * 4"), Value::Int(14)); // precedence
        assert_eq!(ev("-5 + 2"), Value::Int(-3));
    }

    #[test]
    fn math_functions() {
        let f = |s: &str| ev(s).to_number().unwrap().as_f64();
        // Powers / roots (float, but whole values render as integers).
        assert_eq!(ev("SQRT(9)"), Value::Float(3.0));
        assert_eq!(ev("SQ(4)"), Value::Float(16.0));
        assert_eq!(ev("CUBE(3)"), Value::Float(27.0));
        assert_eq!(ev("SQRT(SQ(5))"), Value::Float(5.0));
        // ABS preserves int-ness; INT truncates toward zero.
        assert_eq!(ev("ABS(-5)"), Value::Int(5));
        assert_eq!(ev("ABS(-5.5)"), Value::Float(5.5));
        assert_eq!(ev("INT(3.7)"), Value::Int(3));
        assert_eq!(ev("INT(-3.7)"), Value::Int(-3));
        // Factorial — exact i64.
        assert_eq!(ev("FAC(5)"), Value::Int(120));
        assert_eq!(ev("FAC(0)"), Value::Int(1));
        // Logs: LOG = base 10, LN = natural.
        assert!((f("LOG(1000)") - 3.0).abs() < 1e-12);
        assert_eq!(ev("LN(1)"), Value::Float(0.0));
        // Trig (radians) and inverses.
        assert_eq!(ev("SIN(0)"), Value::Float(0.0));
        assert_eq!(ev("COS(0)"), Value::Float(1.0));
        assert!((f("ATAN(1)") - std::f64::consts::FRAC_PI_4).abs() < 1e-12);
        assert!((f("SIN(ASIN(0.5))") - 0.5).abs() < 1e-12);
        // Hyperbolic and inverses.
        assert_eq!(ev("COSH(0)"), Value::Float(1.0));
        assert_eq!(ev("ACOSH(1)"), Value::Float(0.0));
        assert!((f("TANH(ATANH(0.5))") - 0.5).abs() < 1e-12);
        assert!((f("ASINH(SINH(2))") - 2.0).abs() < 1e-12);
        // Lowercase names fold to the same function; usable in expressions.
        assert_eq!(ev("sqrt(16) + 1"), Value::Float(5.0));
        // Out-of-domain inputs yield NaN rather than erroring.
        assert_eq!(ev("SQRT(-1)").to_str(), "NaN");
        // Constants (zero-arg) and atan2.
        assert!((f("PI()") - std::f64::consts::PI).abs() < 1e-15);
        assert!((f("E()") - std::f64::consts::E).abs() < 1e-15);
        assert!((f("ATAN2(1, 1)") - std::f64::consts::FRAC_PI_4).abs() < 1e-12);
        assert!((f("ATAN2(0, -1)") - std::f64::consts::PI).abs() < 1e-12); // quadrant-correct
        // Composable: a half-circle in radians.
        assert!((f("2 * PI() * 1") - std::f64::consts::TAU).abs() < 1e-12);
        // EXP(x) = e^x — the natural exponential (standard one-argument form), always a float.
        assert_eq!(ev("EXP(0)"), Value::Float(1.0)); // e^0 = 1 (renders "1")
        assert!((f("EXP(1)") - std::f64::consts::E).abs() < 1e-12); // e^1 = e
        assert!((f("EXP(LN(5))") - 5.0).abs() < 1e-12); // inverse of LN
        assert!((f("EXP(-1)") - (1.0 / std::f64::consts::E)).abs() < 1e-12);
        // POW(base, exponent) = base^exponent (the named twin of `^`).
        assert_eq!(ev("POW(2, 10)"), Value::Int(1024)); // int base + non-neg int exp → exact int
        assert_eq!(ev("POW(2, 0)"), Value::Int(1));
        assert_eq!(ev("POW(2, -1)"), Value::Float(0.5)); // negative exp → float
        assert!((f("POW(2, 0.5)") - std::f64::consts::SQRT_2).abs() < 1e-12); // fractional exp
        assert_eq!(ev("POW(2.0, 3)"), Value::Float(8.0)); // float base → float (renders "8")
        assert_eq!(ev("POW(-1, 0.5)").to_str(), "NaN"); // out of domain
    }

    #[test]
    fn base_conversion() {
        assert_eq!(ev(r#"BASE("FF", 16, 10)"#), Value::str("255"));
        assert_eq!(ev(r#"BASE("ff", 16, 10)"#), Value::str("255")); // input case-insensitive
        assert_eq!(ev(r#"BASE("255", 10, 16)"#), Value::str("FF")); // output uppercase
        assert_eq!(ev(r#"BASE("255", 10, 2)"#), Value::str("11111111"));
        assert_eq!(ev(r#"BASE("1010", 2, 10)"#), Value::str("10"));
        assert_eq!(ev(r#"BASE("0", 10, 16)"#), Value::str("0"));
        assert_eq!(ev(r#"BASE("-255", 10, 16)"#), Value::str("-FF")); // negative
        assert_eq!(ev(r#"BASE("z", 36, 10)"#), Value::str("35")); // base 36
        // Errors: out-of-range base, or digits that don't fit the input base.
        assert!(eval(&parse(r#"BASE("10", 1, 10)"#).unwrap(), &Ctx::new()).is_err());
        assert!(eval(&parse(r#"BASE("8", 8, 10)"#).unwrap(), &Ctx::new()).is_err()); // 8 invalid in base 8
    }

    #[test]
    fn power_operator() {
        let f = |s: &str| ev(s).to_number().unwrap().as_f64();
        assert_eq!(ev("2^10"), Value::Int(1024)); // int^int → exact int
        assert_eq!(ev("2^3^2"), Value::Int(512)); // right-associative: 2^(3^2)
        assert_eq!(ev("2*3^2"), Value::Int(18)); // binds tighter than *
        assert_eq!(ev("2+3^2"), Value::Int(11)); // …and tighter than +
        assert_eq!(ev("-2^2"), Value::Int(-4)); // -(2^2), not (-2)^2
        assert_eq!(ev("2^-1"), Value::Float(0.5)); // unary minus in the exponent
        assert!((f("2^0.5") - std::f64::consts::SQRT_2).abs() < 1e-12); // fractional exponent
        assert_eq!(ev("2^10"), ev("POW(2, 10)")); // same engine as POW()
        // Non-numeric operand errors, like the other arithmetic operators.
        assert!(eval(&parse(r#""a" ^ 2"#).unwrap(), &Ctx::new()).is_err());
    }

    #[test]
    fn division_by_zero_is_error() {
        assert!(eval(&parse("5 / 0").unwrap(), &Ctx::new()).is_err());
        assert!(eval(&parse("5 % 0").unwrap(), &Ctx::new()).is_err());
    }

    #[test]
    fn binder_refs_resolve_as_values() {
        // `@X` (text) and `&X` (script source) resolve to their stored strings; the slot
        // is case-folded by the lexer; an unset slot is the empty string.
        let ctx = Ctx::new().with_binder(false, 'A', "hello").with_binder(true, 'B', "DFA");
        assert_eq!(ev_ctx("@a", &ctx), Value::str("hello")); // case-folded slot
        assert_eq!(ev_ctx("&B", &ctx), Value::str("DFA"));
        assert_eq!(ev_ctx("@Z", &ctx), Value::str("")); // unset → empty
        // Usable inside expressions.
        assert_eq!(ev_ctx(r#"@A == "hello""#, &ctx), Value::Int(1));
        assert_eq!(ev_ctx(r#"UPPER(@A)"#, &ctx), Value::str("HELLO"));
    }

    #[test]
    fn string_classification_predicates() {
        let t = Value::Int(1);
        let f = Value::Int(0);
        // Empty is FALSE for every char-class test (and for the number ones).
        for fname in ["ISDEC", "ISHEX", "ISINT", "ISFLOAT", "ISALPHA", "ISALNUM", "ISASCII", "ISUTF8"] {
            assert_eq!(ev(&format!(r#"{fname}("")"#)), f, "{fname}(\"\") should be false");
        }
        // ISDEC — ASCII digits only.
        assert_eq!(ev(r#"ISDEC("0123456789")"#), t);
        assert_eq!(ev(r#"ISDEC("12a")"#), f);
        assert_eq!(ev(r#"ISDEC("-1")"#), f); // the sign isn't a digit
        assert_eq!(ev(r#"ISDEC("1 2")"#), f); // no trimming — the space fails
        // ISHEX — [0-9A-Fa-f], no `0x`.
        assert_eq!(ev(r#"ISHEX("1fF0aA")"#), t);
        assert_eq!(ev(r#"ISHEX("0x1F")"#), f);
        assert_eq!(ev(r#"ISHEX("g1")"#), f);
        // ISINT / ISFLOAT partition numeric strings by FORM (trimmed, like the language).
        assert_eq!(ev(r#"ISINT("42")"#), t);
        assert_eq!(ev(r#"ISINT("-42")"#), t);
        assert_eq!(ev(r#"ISINT(" 42 ")"#), t); // trims, matching to_number
        assert_eq!(ev(r#"ISINT("4.2")"#), f);
        assert_eq!(ev(r#"ISINT("4x")"#), f);
        assert_eq!(ev(r#"ISFLOAT("4.2")"#), t);
        assert_eq!(ev(r#"ISFLOAT("1e3")"#), t);
        assert_eq!(ev(r#"ISFLOAT("-0.5")"#), t);
        assert_eq!(ev(r#"ISFLOAT("42")"#), f); // an integer is ISINT, not ISFLOAT
        assert_eq!(ev(r#"ISFLOAT("inf")"#), f); // non-finite excluded
        assert_eq!(ev(r#"ISFLOAT("nan")"#), f);
        // ISALPHA / ISALNUM — ASCII letters, then letters+digits.
        assert_eq!(ev(r#"ISALPHA("Hello")"#), t);
        assert_eq!(ev(r#"ISALPHA("Hi5")"#), f);
        assert_eq!(ev(r#"ISALPHA("café")"#), f); // é is not ASCII [A-Za-z]
        assert_eq!(ev(r#"ISALNUM("abc123")"#), t);
        assert_eq!(ev(r#"ISALNUM("a b")"#), f);
        // ISASCII — codepoints 0..127.
        assert_eq!(ev(r#"ISASCII("plain~text!")"#), t);
        assert_eq!(ev(r#"ISASCII("café")"#), f);
        // ISUTF8 — all chars in the BMP (U+0000..U+FFFF); astral chars (emoji) fail.
        assert_eq!(ev(r#"ISUTF8("café")"#), t); // é is U+00E9, in the BMP
        assert_eq!(ev(r#"ISUTF8("hi 😀")"#), f); // U+1F600 is above the BMP
    }

    // ---- concat, comparison, logic -----------------------------------------

    #[test]
    fn concat_and_compare() {
        assert_eq!(ev(r#""a" . "b" . "c""#), Value::str("abc"));
        assert_eq!(ev("10 < 9"), Value::Int(0)); // numeric
        assert_eq!(ev(r#""10" < "9""#), Value::Int(1)); // string (quoting forces)
        assert_eq!(ev("5 == 5"), Value::Int(1));
        assert_eq!(ev("5 != 5"), Value::Int(0));
        assert_eq!(ev("3 <= 3 AND 4 >= 5"), Value::Int(0));
    }

    #[test]
    fn logic_and_not() {
        assert_eq!(ev("1 AND 0"), Value::Int(0));
        assert_eq!(ev("1 OR 0"), Value::Int(1));
        assert_eq!(ev("NOT 0"), Value::Int(1));
        assert_eq!(ev("NOT 5"), Value::Int(0));
    }

    // ---- string functions (the §7 table) -----------------------------------

    #[test]
    fn string_functions() {
        assert_eq!(ev(r#"LEN("hello")"#), Value::Int(5));
        assert_eq!(ev(r#"UPPER("aBc")"#), Value::str("ABC"));
        assert_eq!(ev(r#"LOWER("aBc")"#), Value::str("abc"));
        assert_eq!(ev(r#"STRIP("  hi  ")"#), Value::str("hi"));
        assert_eq!(ev(r#"SUBSTR("hello", 1, 3)"#), Value::str("ell"));
        assert_eq!(ev(r#"LEFT("hello", 2)"#), Value::str("he"));
        assert_eq!(ev(r#"RIGHT("hello", 3)"#), Value::str("llo"));
        assert_eq!(ev(r#"LEFT("hi", 9)"#), Value::str("hi")); // n past length → whole string
        assert_eq!(ev(r#"RIGHT("hi", 9)"#), Value::str("hi"));
        assert_eq!(ev(r#"LEFT("hi", 0)"#), Value::str("")); // n <= 0 → empty
        assert_eq!(ev(r#"RIGHT("hi", -1)"#), Value::str(""));
        assert_eq!(ev(r#"TRIMLEFT("hello", 2)"#), Value::str("llo"));
        assert_eq!(ev(r#"TRIMRIGHT("hello", 2)"#), Value::str("hel"));
        assert_eq!(ev(r#"INDEX("hello", "ll")"#), Value::Int(2));
        assert_eq!(ev(r#"INDEX("hello", "z")"#), Value::Int(-1));
        assert_eq!(ev(r#"CONTAINS("hello", "ell")"#), Value::Int(1));
        assert_eq!(ev(r#"REPLACE("a-b-c", "-", "+")"#), Value::str("a+b+c"));
        // Optional 4th arg bounds the replacements: n>0 from the start, n<0 from the end, 0 = none.
        assert_eq!(ev(r#"REPLACE("a-b-c-d", "-", "+", 2)"#), Value::str("a+b+c-d"));
        assert_eq!(ev(r#"REPLACE("a-b-c-d", "-", "+", -2)"#), Value::str("a-b+c+d"));
        assert_eq!(ev(r#"REPLACE("a-b-c-d", "-", "+", 1)"#), Value::str("a+b-c-d"));
        assert_eq!(ev(r#"REPLACE("a-b-c-d", "-", "+", -1)"#), Value::str("a-b-c+d"));
        assert_eq!(ev(r#"REPLACE("a-b", "-", "+", 0)"#), Value::str("a-b")); // 0 → unchanged
        assert_eq!(ev(r#"REPLACE("a-b", "-", "+", 9)"#), Value::str("a+b")); // n past count → all
        // Direction matters for self-overlapping patterns.
        assert_eq!(ev(r#"REPLACE("aaa", "aa", "X", 1)"#), Value::str("Xa"));
        assert_eq!(ev(r#"REPLACE("aaa", "aa", "X", -1)"#), Value::str("aX"));
        // Multi-byte safety: replace only the last accented match.
        assert_eq!(ev(r#"REPLACE("é-é-é", "é", "e", -1)"#), Value::str("é-é-e"));
        assert_eq!(ev("CHR(65)"), Value::str("A"));
        assert_eq!(ev("CHR(70000)"), Value::str("")); // out of 0..255
        assert_eq!(ev(r#"ASC("A")"#), Value::str("65"));
        assert_eq!(ev(r#"ASC("")"#), Value::str("0"));
    }

    #[test]
    fn nested_functions() {
        assert_eq!(ev(r#"UPPER(SUBSTR("hello", 0, 3))"#), Value::str("HEL"));
        assert_eq!(ev(r#"LEN(REPLACE("aaa", "a", "bb"))"#), Value::Int(6));
    }

    #[test]
    fn function_errors() {
        assert!(parse(r#"LEN("a", "b")"#).and_then(|e| eval(&e, &Ctx::new())).is_err()); // arity
        assert!(parse("NOPE(1)").and_then(|e| eval(&e, &Ctx::new())).is_err()); // unknown
    }

    #[test]
    fn function_names_are_all_callable() {
        // Every name in FUNCTION_NAMES (the doc/coverage surface) must be a real `eval_call` arm — a
        // wrong-arity call still proves it exists; only an UNKNOWN name reports "unknown function".
        let ctx = Ctx::new();
        for &name in FUNCTION_NAMES {
            if let Err(ExprError { msg: m, .. }) = eval_call(name, &[], &ctx) {
                assert!(!m.contains("unknown function"), "FUNCTION_NAMES lists {name:?}, not in eval_call");
            }
        }
    }

    // ---- variables (token-level substitution) ------------------------------

    #[test]
    fn variables_resolve_at_eval_time() {
        let ctx = Ctx::new().with("name", Value::str("Alice")).with("n", Value::int(3));
        assert_eq!(ev_ctx(r#""Hi " . $name"#, &ctx), Value::str("Hi Alice"));
        assert_eq!(ev_ctx("$n * 2", &ctx), Value::Int(6));
        // Undefined variable is the empty string.
        assert_eq!(ev_ctx("$missing", &ctx), Value::str(""));
        // VAR() is the function form.
        assert_eq!(ev_ctx(r#"UPPER(VAR("name"))"#, &ctx), Value::str("ALICE"));
    }

    #[test]
    fn array_functions_and_indexing() {
        // SPLIT / SPLITS produce real arrays; LEN counts elements; [] reads.
        assert_eq!(ev(r#"LEN(SPLIT("a,b,c", ","))"#), Value::Int(3));
        assert_eq!(ev(r#"SPLIT("a,b,c", ",")[0]"#), Value::str("a"));
        assert_eq!(ev(r#"SPLIT("a,b,c", ",")[2]"#), Value::str("c"));
        assert_eq!(ev(r#"SPLIT("a,b,c", ",")[9]"#), Value::str("")); // OOB -> ""
        assert_eq!(ev(r#"SPLITS("a-b_c", "-_")[1]"#), Value::str("b"));
        assert_eq!(ev(r#"JOIN(SPLIT("a,b,c", ","), "+")"#), Value::str("a+b+c"));
        // Empty separator -> per-character.
        assert_eq!(ev(r#"LEN(SPLIT("abc", ""))"#), Value::Int(3));
        // String indexing -> char at position.
        assert_eq!(ev(r#""hello"[1]"#), Value::str("e"));
    }

    #[test]
    fn index_into_array_variable() {
        let ctx = Ctx::new().with(
            "items",
            Value::Array(vec![Value::str("x"), Value::int(7)]),
        );
        assert_eq!(ev_ctx("$items[0]", &ctx), Value::str("x"));
        assert_eq!(ev_ctx("$items[1] * 2", &ctx), Value::Int(14));
        assert_eq!(ev_ctx("LEN($items)", &ctx), Value::Int(2));
    }

    #[test]
    fn indexing_a_number_is_an_error() {
        assert!(parse("5[0]").and_then(|e| eval(&e, &Ctx::new())).is_err());
    }

    #[test]
    fn sel_function() {
        let ctx = Ctx { vars: HashMap::new(), sel: "picked".into(), binders: HashMap::new(), now: None };
        assert_eq!(ev_ctx(r#"UPPER(SEL())"#, &ctx), Value::str("PICKED"));
    }

    // ---- parse-level: no rollback hack; clean errors -----------------------

    #[test]
    fn trailing_garbage_is_a_clean_error() {
        // QN's rollback would have stopped at `+`; XyScript errors clearly.
        assert!(parse("1 + ) bad").is_err());
        assert!(parse("1 2").is_err()); // trailing token
    }

    #[test]
    fn bare_word_is_string_but_space_paren_is_not_a_call() {
        // Adjacent: a call.
        assert_eq!(ev(r#"LEN(" abc")"#), Value::Int(4));
        // Non-adjacent `LEN (..)` is bare "LEN" then a stray group -> trailing.
        assert!(parse("LEN (1)").is_err());
    }
}
