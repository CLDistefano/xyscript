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
    /// `GETKEY(scc)` — the human-readable NAME of a computed scan code (`$SCC` = key code + modifier
    /// bits), like `READKEY/D` writes into its variable (`CTRL+Y`, `Enter`, `Backspace`). `None` when
    /// `scc` isn't a valid key + modifier combination (the function then yields the empty string).
    fn scc_key_name(&self, _scc: i64) -> Option<String> {
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
                let n = n.clone();
                self.eat();
                Ok(Expr::Num(n))
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
        Expr::Num(n) => Ok(Value::from_number(n.clone())),
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
                Ok(crate::value::Number::Big(_)) => i64::MAX, // absurd index → out of bounds
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
                crate::value::Number::Big(_) => i64::MAX, // absurd index → out of bounds
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
    // Integer base ^ non-negative integer exponent stays EXACT (arbitrary precision on overflow) — so
    // `2^256` is the full 78-digit integer. Everything else (negative/fractional exponent, a float
    // operand, an exponent too big for u32) falls back to `powf`.
    if let Some(n) = Number::pow_exact(&a, &b) {
        return Ok(Value::from_number(n));
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
    f: fn(&Number, &Number) -> Number,
) -> Result<Value, ExprError> {
    let a = l.to_number().map_err(|_| non_numeric(l, sym))?;
    let b = r.to_number().map_err(|_| non_numeric(r, sym))?;
    Ok(Value::from_number(f(&a, &b)))
}

fn num_op_checked(
    l: &Value,
    r: &Value,
    sym: &str,
    f: fn(&Number, &Number) -> Result<Number, crate::value::DivByZero>,
) -> Result<Value, ExprError> {
    let a = l.to_number().map_err(|_| non_numeric(l, sym))?;
    let b = r.to_number().map_err(|_| non_numeric(r, sym))?;
    match f(&a, &b) {
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
    "CHR", "ASC", "FORMAT",
    // string classification predicates (all → 1/0; false for an empty string)
    "ISDEC", "ISHEX", "ISINT", "ISFLOAT", "ISALPHA", "ISALNUM", "ISASCII", "ISUTF8",
    "ISUPPER", "ISLOWER", "ISTITLE", "ISSPACE", "ISPRINT", "ISPUNCT",
    // arrays / collections
    "ITER", "ITERP", "PUSH", "POP", "SLICE", "SORT", "VAR", "SEL",
    // math
    "ABS", "INT", "SQRT", "SQ", "CUBE", "FAC", "EXP", "LN", "LOG", "POW", "BASE", "HEX",
    "SIN", "COS", "TAN", "ASIN", "ACOS", "ATAN", "ATAN2", "SINH", "COSH", "TANH", "ASINH", "ACOSH",
    "ATANH", "PI", "E", "RANDOM",
    // editor / environment
    "KEY", "GETKEY", "GETPATH", "EXISTS", "ISDIR", "ISFILE", "READFILE", "RF",
    // date / time (masked)
    "DATE", "TIME",
    // date arithmetic (Julian Day Number pivot, proleptic Gregorian)
    "DATE2JDN", "JDN2DATE", "DOW", "DATEDIFF", "DATEADD", "TODAY", "LEAP", "DAYSINMONTH",
    "DAYOFYEAR", "ISDATE", "DATEPARSE",
];

/// `true` if `chars[i..]` begins with the ASCII `pat`.
fn mask_at(chars: &[char], i: usize, pat: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    i + p.len() <= chars.len() && chars[i..i + p.len()] == p[..]
}

/// Proleptic-Gregorian leap year: divisible by 4, except centuries not divisible by 400.
fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Number of days in `month` (1–12) of `year`, proleptic Gregorian (`0` for an invalid month).
fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => if is_leap_year(year) { 29 } else { 28 },
        _ => 0,
    }
}

/// `true` if `(y, m, d)` is a real proleptic-Gregorian date (month 1–12, day 1–days-in-month). Any
/// year is accepted; the day count uses the Gregorian leap rule for every year (no 1582 switch).
fn valid_ymd(y: i64, m: i64, d: i64) -> bool {
    (1..=12).contains(&m) && d >= 1 && d <= days_in_month(y, m)
}

/// Date → Julian Day Number (Fliegel & Van Flandern), proleptic Gregorian. The result is the standard
/// integer JDN: it advances by 1 each calendar day and `.rem_euclid(7)` gives the weekday (0 = Monday).
/// `div_euclid` (floor division) keeps it correct for negative/ancient years. Assumes a valid date.
fn ymd_to_jdn(y: i64, m: i64, d: i64) -> i64 {
    let a = (14 - m).div_euclid(12);
    let yy = y + 4800 - a;
    let mm = m + 12 * a - 3;
    d + (153 * mm + 2).div_euclid(5) + 365 * yy + yy.div_euclid(4) - yy.div_euclid(100)
        + yy.div_euclid(400)
        - 32045
}

/// Julian Day Number → `(year, month, day)`, proleptic Gregorian. Exact inverse of [`ymd_to_jdn`].
fn jdn_to_ymd(jdn: i64) -> (i64, u32, u32) {
    let a = jdn + 32044;
    let b = (4 * a + 3).div_euclid(146097);
    let c = a - (146097 * b).div_euclid(4);
    let d = (4 * c + 3).div_euclid(1461);
    let e = c - (1461 * d).div_euclid(4);
    let m = (5 * e + 2).div_euclid(153);
    let day = e - (153 * m + 2).div_euclid(5) + 1;
    let month = m + 3 - 12 * m.div_euclid(10);
    let year = 100 * b + d - 4800 + m.div_euclid(10);
    (year, month as u32, day as u32)
}

/// Weekday names indexed by `JDN.rem_euclid(7)` (0 = Monday). Shared by [`format_date`]/[`parse_date`].
const WD_FULL: [&str; 7] =
    ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const WD_ABBR: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
/// Month names indexed by `month - 1`.
const MO_FULL: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October",
    "November", "December",
];
const MO_ABBR: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Read at least `min` and at most `max` ASCII digits from `chars[*i..]`, advancing `*i` past them and
/// returning their value. `Err` (with the offending position) if fewer than `min` digits are present.
fn take_uint(chars: &[char], i: &mut usize, min: usize, max: usize) -> Result<i64, String> {
    let start = *i;
    while *i < chars.len() && *i - start < max && chars[*i].is_ascii_digit() {
        *i += 1;
    }
    if *i - start < min {
        return Err(format!("expected a number at position {start}"));
    }
    chars[start..*i].iter().collect::<String>().parse::<i64>().map_err(|e| e.to_string())
}

/// If any entry of `names` matches `chars[*i..]` case-insensitively, advance `*i` past the match and
/// return its index; else `None`. The longest matching name wins, so a name that is a prefix of a
/// longer one never shadows it.
fn match_name(chars: &[char], i: &mut usize, names: &[&str]) -> Option<usize> {
    let mut best: Option<(usize, usize)> = None; // (index, char-length)
    for (idx, nm) in names.iter().enumerate() {
        let nc: Vec<char> = nm.chars().collect();
        let fits = *i + nc.len() <= chars.len()
            && chars[*i..*i + nc.len()].iter().zip(&nc).all(|(a, b)| a.eq_ignore_ascii_case(b));
        if fits && best.is_none_or(|(_, l)| nc.len() > l) {
            best = Some((idx, nc.len()));
        }
    }
    best.map(|(idx, len)| {
        *i += len;
        idx
    })
}

/// Parse `input` against a DATE `mask`, returning `(year, month, day)`. Mask tokens consume the
/// matching text: `yyyy` (up to 4 digits) / `yy` (2 digits, POSIX pivot: 69–99 → 1900s, 00–68 →
/// 2000s); `mmmm`/`mmm` (month name, full / abbreviated, case-insensitive); `mm` (exactly 2 digits) /
/// `m` (1–2 digits) month number; `dddd`/`ddd` (weekday name — matched then IGNORED); `dd` (exactly 2
/// digits) / `d` (1–2 digits) day. Any other mask character must appear literally in the input. `Err`
/// on a mismatch, leftover input, an incomplete mask (missing year, month, or day), or an unreal date.
fn parse_date(mask: &str, input: &str) -> Result<(i64, u32, u32), String> {
    let mc: Vec<char> = mask.chars().collect();
    let ic: Vec<char> = input.chars().collect();
    let (mut mi, mut ii) = (0usize, 0usize);
    let (mut y, mut mo, mut d): (Option<i64>, Option<i64>, Option<i64>) = (None, None, None);
    while mi < mc.len() {
        if mask_at(&mc, mi, "yyyy") {
            y = Some(take_uint(&ic, &mut ii, 1, 4)?);
            mi += 4;
        } else if mask_at(&mc, mi, "yy") {
            let v = take_uint(&ic, &mut ii, 2, 2)?;
            y = Some(if v >= 69 { 1900 + v } else { 2000 + v }); // POSIX two-digit-year window
            mi += 2;
        } else if mask_at(&mc, mi, "mmmm") {
            mo = Some(match_name(&ic, &mut ii, &MO_FULL).ok_or("expected a month name")? as i64 + 1);
            mi += 4;
        } else if mask_at(&mc, mi, "mmm") {
            mo = Some(match_name(&ic, &mut ii, &MO_ABBR).ok_or("expected a month abbreviation")? as i64 + 1);
            mi += 3;
        } else if mask_at(&mc, mi, "mm") {
            mo = Some(take_uint(&ic, &mut ii, 2, 2)?);
            mi += 2;
        } else if mc[mi] == 'm' {
            mo = Some(take_uint(&ic, &mut ii, 1, 2)?);
            mi += 1;
        } else if mask_at(&mc, mi, "dddd") {
            match_name(&ic, &mut ii, &WD_FULL).ok_or("expected a weekday name")?;
            mi += 4;
        } else if mask_at(&mc, mi, "ddd") {
            match_name(&ic, &mut ii, &WD_ABBR).ok_or("expected a weekday abbreviation")?;
            mi += 3;
        } else if mask_at(&mc, mi, "dd") {
            d = Some(take_uint(&ic, &mut ii, 2, 2)?);
            mi += 2;
        } else if mc[mi] == 'd' {
            d = Some(take_uint(&ic, &mut ii, 1, 2)?);
            mi += 1;
        } else {
            if ii >= ic.len() || ic[ii] != mc[mi] {
                return Err(format!("expected {:?} at position {ii}", mc[mi]));
            }
            ii += 1;
            mi += 1;
        }
    }
    if ii != ic.len() {
        return Err(format!("trailing text {:?}", ic[ii..].iter().collect::<String>()));
    }
    let (Some(y), Some(mo), Some(d)) = (y, mo, d) else {
        return Err("mask must specify year, month, and day".to_string());
    };
    if !valid_ymd(y, mo, d) {
        return Err(format!("invalid date {y}-{mo}-{d}"));
    }
    Ok((y, mo as u32, d as u32))
}

/// Format `now` by a DATE mask. Tokens: `yyyy`/`yy` (year), `mmmm`/`mmm` (month name, full / abbr),
/// `mm`/`m` (month number, padded / bare), `dddd`/`ddd` (weekday name, full / abbr), `dd`/`d` (day
/// number). Longest token wins, so a name token is tried before the numeric one. Everything else is a
/// literal. The weekday comes from the proleptic-Gregorian JDN, so it is correct for any date.
fn format_date(mask: &str, now: &DateParts) -> String {
    let dow = ymd_to_jdn(now.year, now.month as i64, now.day as i64).rem_euclid(7) as usize; // 0 = Mon
    let mo = (now.month as usize).clamp(1, 12) - 1;
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
        } else if mask_at(&chars, i, "mmmm") {
            out.push_str(MO_FULL[mo]);
            i += 4;
        } else if mask_at(&chars, i, "mmm") {
            out.push_str(MO_ABBR[mo]);
            i += 3;
        } else if mask_at(&chars, i, "mm") {
            out.push_str(&format!("{:02}", now.month));
            i += 2;
        } else if chars[i] == 'm' {
            out.push_str(&now.month.to_string());
            i += 1;
        } else if mask_at(&chars, i, "dddd") {
            out.push_str(WD_FULL[dow]);
            i += 4;
        } else if mask_at(&chars, i, "ddd") {
            out.push_str(WD_ABBR[dow]);
            i += 3;
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
        // `FORMAT(template, …args)` — interpolate the args into the template's `{}` placeholders (v1).
        "FORMAT" => {
            if argc < 1 {
                return Err(ExprError::new(format!(
                    "FORMAT: expected at least 1 arg (the template), got {argc}"
                )));
            }
            Ok(Value::str(format_template(&s(0), &vals[1..])?))
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
        // Date arithmetic (proleptic Gregorian, day resolution). The Julian Day Number is the pivot:
        // `DATE2JDN(y,m,d)` encodes a date to an integer, `JDN2DATE(jdn[,mask])` decodes it back, and
        // everything else is integer math on the JDN. A malformed date (bad month, or a day past the
        // month's length) is a *trappable* error (sets `$ERROR` instead of halting); use `ISDATE` to
        // pre-check. `DOW` numbers the week ISO-style, 1 = Monday … 7 = Sunday.
        "DATE2JDN" => {
            expect(3)?;
            let (y, m, d) =
                (int_arg(&vals[0], "DATE2JDN")?, int_arg(&vals[1], "DATE2JDN")?, int_arg(&vals[2], "DATE2JDN")?);
            if !valid_ymd(y, m, d) {
                return Err(ExprError::trappable(format!("DATE2JDN: invalid date {y}-{m}-{d}")));
            }
            Ok(Value::Int(ymd_to_jdn(y, m, d)))
        }
        "JDN2DATE" => {
            if argc != 1 && argc != 2 {
                return Err(ExprError::new(format!("JDN2DATE: expected 1 or 2 args, got {argc}")));
            }
            let (y, month, day) = jdn_to_ymd(int_arg(&vals[0], "JDN2DATE")?);
            let parts = DateParts { year: y, month, day, hour: 0, minute: 0, second: 0 };
            let mask = if argc == 2 { s(1) } else { "yyyy-mm-dd".to_string() };
            Ok(Value::str(format_date(&mask, &parts)))
        }
        "DOW" => {
            expect(3)?;
            let (y, m, d) = (int_arg(&vals[0], "DOW")?, int_arg(&vals[1], "DOW")?, int_arg(&vals[2], "DOW")?);
            if !valid_ymd(y, m, d) {
                return Err(ExprError::trappable(format!("DOW: invalid date {y}-{m}-{d}")));
            }
            Ok(Value::Int(ymd_to_jdn(y, m, d).rem_euclid(7) + 1)) // 1 = Monday … 7 = Sunday
        }
        "DATEDIFF" => {
            expect(6)?;
            let a = (int_arg(&vals[0], "DATEDIFF")?, int_arg(&vals[1], "DATEDIFF")?, int_arg(&vals[2], "DATEDIFF")?);
            let b = (int_arg(&vals[3], "DATEDIFF")?, int_arg(&vals[4], "DATEDIFF")?, int_arg(&vals[5], "DATEDIFF")?);
            if !valid_ymd(a.0, a.1, a.2) {
                return Err(ExprError::trappable(format!("DATEDIFF: invalid date {}-{}-{}", a.0, a.1, a.2)));
            }
            if !valid_ymd(b.0, b.1, b.2) {
                return Err(ExprError::trappable(format!("DATEDIFF: invalid date {}-{}-{}", b.0, b.1, b.2)));
            }
            Ok(Value::Int(ymd_to_jdn(b.0, b.1, b.2) - ymd_to_jdn(a.0, a.1, a.2))) // days from date1 to date2
        }
        "DATEADD" => {
            if argc != 4 && argc != 5 {
                return Err(ExprError::new(format!("DATEADD: expected 4 or 5 args, got {argc}")));
            }
            let (y, m, d) =
                (int_arg(&vals[0], "DATEADD")?, int_arg(&vals[1], "DATEADD")?, int_arg(&vals[2], "DATEADD")?);
            let n = int_arg(&vals[3], "DATEADD")?;
            if !valid_ymd(y, m, d) {
                return Err(ExprError::trappable(format!("DATEADD: invalid date {y}-{m}-{d}")));
            }
            let (yy, month, day) = jdn_to_ymd(ymd_to_jdn(y, m, d) + n);
            let parts = DateParts { year: yy, month, day, hour: 0, minute: 0, second: 0 };
            let mask = if argc == 5 { s(4) } else { "yyyy-mm-dd".to_string() };
            Ok(Value::str(format_date(&mask, &parts)))
        }
        "TODAY" => {
            expect(0)?;
            let now = ctx.now().ok_or_else(|| ExprError::new("TODAY: no clock in this context"))?;
            Ok(Value::Int(ymd_to_jdn(now.year, now.month as i64, now.day as i64)))
        }
        "LEAP" => {
            expect(1)?;
            Ok(Value::Int(if is_leap_year(int_arg(&vals[0], "LEAP")?) { 1 } else { 0 }))
        }
        "DAYSINMONTH" => {
            expect(2)?;
            let (y, m) = (int_arg(&vals[0], "DAYSINMONTH")?, int_arg(&vals[1], "DAYSINMONTH")?);
            if !(1..=12).contains(&m) {
                return Err(ExprError::trappable(format!("DAYSINMONTH: invalid month {m}")));
            }
            Ok(Value::Int(days_in_month(y, m)))
        }
        "DAYOFYEAR" => {
            expect(3)?;
            let (y, m, d) =
                (int_arg(&vals[0], "DAYOFYEAR")?, int_arg(&vals[1], "DAYOFYEAR")?, int_arg(&vals[2], "DAYOFYEAR")?);
            if !valid_ymd(y, m, d) {
                return Err(ExprError::trappable(format!("DAYOFYEAR: invalid date {y}-{m}-{d}")));
            }
            Ok(Value::Int(ymd_to_jdn(y, m, d) - ymd_to_jdn(y, 1, 1) + 1))
        }
        "ISDATE" => {
            expect(3)?;
            let (y, m, d) = (int_arg(&vals[0], "ISDATE")?, int_arg(&vals[1], "ISDATE")?, int_arg(&vals[2], "ISDATE")?);
            Ok(Value::Int(if valid_ymd(y, m, d) { 1 } else { 0 }))
        }
        // `DATEPARSE(str, mask)` — read a date STRING back into components, the inverse of `JDN2DATE`.
        // Returns a 3-element array `[year, month, day]` (index it, or feed `DATE2JDN`). The `mask` is a
        // normal quoted argument using the same codes as `DATE`; a weekday token (`dddd`/`ddd`) in the
        // mask is matched in the string but does not set a component. A string that doesn't fit the mask
        // — or that spells an unreal date — is a trappable error.
        "DATEPARSE" => {
            expect(2)?;
            let (y, m, d) =
                parse_date(&s(1), &s(0)).map_err(|e| ExprError::trappable(format!("DATEPARSE: {e}")))?;
            Ok(Value::Array(vec![Value::Int(y), Value::Int(m as i64), Value::Int(d as i64)]))
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
        "ISUPPER" => {
            expect(1)?;
            Ok(is_cased(&s(0), true))
        }
        "ISLOWER" => {
            expect(1)?;
            Ok(is_cased(&s(0), false))
        }
        "ISTITLE" => {
            expect(1)?;
            Ok(is_title(&s(0)))
        }
        "ISSPACE" => {
            // All characters are ASCII whitespace (the C `isspace` set: space, tab, LF, VT, FF, CR).
            expect(1)?;
            Ok(all_chars(&s(0), |c| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r')))
        }
        "ISPRINT" => {
            // All characters are printable ASCII (space through `~`, i.e. U+0020–U+007E) — graphic
            // characters plus the space, excluding control characters and non-ASCII.
            expect(1)?;
            Ok(all_chars(&s(0), |c| c.is_ascii_graphic() || c == ' '))
        }
        "ISPUNCT" => {
            // All characters are ASCII punctuation: the 32 marks `!"#$%&'()*+,-./:;<=>?@[\]^_`{|}~`.
            expect(1)?;
            Ok(all_chars(&s(0), |c| c.is_ascii_punctuation()))
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
        "GETKEY" => {
            // `GETKEY(scc)` — the key NAME for a computed scan code (`$SCC`), the string `READKEY/D`
            // would write into its variable (`CTRL+Y`, `Enter`, `Backspace`). Empty when `scc` is not a
            // valid key + modifier-bits combination for the current keymap.
            expect(1)?;
            let scc = int_arg(&vals[0], "GETKEY")?;
            Ok(Value::str(ctx.scc_key_name(scc).unwrap_or_default()))
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
                // `ABS(i64::MIN)` promotes to Big rather than wrapping back to a negative.
                Ok(Number::Int(i)) => Ok(match i.checked_abs() {
                    Some(a) => Value::Int(a),
                    None => Value::Big(-num_bigint::BigInt::from(i)),
                }),
                Ok(Number::Big(b)) => Ok(Value::Big(num_traits::Signed::abs(&b))),
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
            // Arbitrary-precision product, so `FAC(21)` and beyond are EXACT (was: overflowed i64 →
            // f64). Demotes to a plain `Int` when the answer still fits.
            let mut acc = num_bigint::BigInt::from(1u32);
            for k in 2..=n {
                acc *= num_bigint::BigInt::from(k);
            }
            Ok(Value::from_number(crate::value::Number::from_big(acc)))
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
        // EXACT integer of any size (arbitrary precision — `POW(2, 256)` is the full 78-digit value);
        // a float operand, or a negative/fractional exponent, makes it a float (`POW(2, 0.5)` →
        // `1.414…`, `POW(2, -1)` → `0.5`). Out-of-domain (`POW(-1, 0.5)`) → NaN.
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
        // A Big can't fit i64 — saturate (these are counts/indices; a huge value is out of any real
        // range), so callers clamp rather than panic.
        Ok(Number::Big(b)) => Ok(if num_traits::Signed::is_negative(&b) { i64::MIN } else { i64::MAX }),
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

/// `ISUPPER`/`ISLOWER`: true iff `s` has at least one ASCII letter and EVERY ASCII letter is the
/// wanted case — non-letters (spaces, digits, punctuation) are ignored, per the conventional meaning
/// of "is it uppercase". Equivalently: has a letter of the wanted case and none of the other case, so
/// `ISUPPER("HELLO, WORLD!")` and `ISUPPER("ABC1")` are true, `ISUPPER("Hello")`/`ISUPPER("123")` are
/// not. ASCII-only, matching `ISALPHA`.
fn is_cased(s: &str, want_upper: bool) -> Value {
    let has_upper = s.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = s.chars().any(|c| c.is_ascii_lowercase());
    bool_val(if want_upper { has_upper && !has_lower } else { has_lower && !has_upper })
}

/// `ISTITLE`: `s` is title-cased (each run of letters starts upper-case, the rest lower-case), with
/// at least one letter — the standard `istitle` rule: an upper-case letter may only follow a
/// NON-letter (or the start), and a lower-case letter may only follow a LETTER. So `"Hello World"`
/// and `"Hello123World"` are titles; `"Hello world"` (the `w` follows a space), `"HELLO"` (the `E`
/// follows a letter), and `"123"` (no letters) are not. ASCII-only, matching the rest of the suite.
fn is_title(s: &str) -> Value {
    let mut prev_letter = false;
    let mut has_letter = false;
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            if prev_letter {
                return bool_val(false); // an upper-case letter mid-word
            }
            has_letter = true;
            prev_letter = true;
        } else if c.is_ascii_lowercase() {
            if !prev_letter {
                return bool_val(false); // a lower-case letter starting a word
            }
            has_letter = true;
            prev_letter = true;
        } else {
            prev_letter = false; // a non-letter ends the current word
        }
    }
    bool_val(has_letter)
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

// ---- FORMAT (v1): Python/Rust-style `{}` string interpolation ----------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Left,
    Right,
    Center,
}

/// How to show the sign of a number (v2): `-` only when negative (default), `+` always, or a leading
/// space for a non-negative number (so positives and negatives line up in a column).
#[derive(Clone, Copy, PartialEq)]
enum Sign {
    Minus,
    Plus,
    Space,
}

/// A parsed FORMAT field spec. `ty == '\0'` means "auto" — format by the value's own type.
struct FmtSpec {
    fill: char,
    align: Option<Align>,
    sign: Sign,
    alt: bool,   // `#` — radix prefix (0x/0o/0b) for x/X/o/b
    zero: bool,  // `0` — zero-pad (sign/prefix kept in front)
    width: usize,
    group: bool, // `,` — thousands separators
    prec: Option<usize>,
    ty: char,
}

impl Default for FmtSpec {
    fn default() -> Self {
        FmtSpec {
            fill: ' ',
            align: None,
            sign: Sign::Minus,
            alt: false,
            zero: false,
            width: 0,
            group: false,
            prec: None,
            ty: '\0',
        }
    }
}

/// `FORMAT(template, …args)` — interpolate `args` into `template`. `{}` takes the next positional
/// argument, `{n}` an explicit 0-based one; `{:spec}` / `{n:spec}` apply a field spec (see
/// [`parse_fmt_spec`]); `{{` / `}}` are literal braces. Returns the built string.
fn format_template(tmpl: &str, args: &[Value]) -> Result<String, ExprError> {
    let mut out = String::new();
    let mut chars = tmpl.chars().peekable();
    let mut auto = 0usize; // next auto-positional argument
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '}' => {
                return Err(ExprError::new("FORMAT: unmatched '}' in template (use '}}' for a literal)"))
            }
            '{' => {
                let mut body = String::new();
                let mut closed = false;
                for d in chars.by_ref() {
                    if d == '}' {
                        closed = true;
                        break;
                    }
                    body.push(d);
                }
                if !closed {
                    return Err(ExprError::new(
                        "FORMAT: unmatched '{' in template (use '{{' for a literal)",
                    ));
                }
                let (idx_part, spec_part) = match body.split_once(':') {
                    Some((i, sp)) => (i, Some(sp)),
                    None => (body.as_str(), None),
                };
                let idx = if idx_part.trim().is_empty() {
                    let i = auto;
                    auto += 1;
                    i
                } else {
                    idx_part
                        .trim()
                        .parse::<usize>()
                        .map_err(|_| ExprError::new(format!("FORMAT: bad argument index {idx_part:?}")))?
                };
                let val = args.get(idx).ok_or_else(|| {
                    ExprError::new(format!(
                        "FORMAT: {{{idx}}} but only {} argument(s) were given",
                        args.len()
                    ))
                })?;
                out.push_str(&apply_fmt(&parse_fmt_spec(spec_part.unwrap_or(""))?, val)?);
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

/// Parse a FORMAT field spec: `[[fill]align][sign]['#']['0'][width][','][.precision][type]`. `align` is
/// `<` (left), `>` (right), or `^` (centre) with an optional fill char in front; `sign` is `+` (always),
/// `-` (default), or space (leading space for positives); `#` adds a `0x`/`0o`/`0b` radix prefix; `0`
/// zero-pads (keeping the sign/prefix in front); `width` is the minimum field width; `,` inserts
/// thousands separators; `.precision` is decimals (`f`/`e`/`%`/floats) or max characters (strings);
/// `type` is `s` string, `d` integer, `f` fixed, `e`/`E` scientific, `%` percent, or `x`/`X`/`o`/`b`
/// hex/octal/binary. Empty = defaults (auto).
fn parse_fmt_spec(spec: &str) -> Result<FmtSpec, ExprError> {
    let cs: Vec<char> = spec.chars().collect();
    let mut sp = FmtSpec::default();
    let mut i = 0;
    let align_of = |c: char| match c {
        '<' => Align::Left,
        '>' => Align::Right,
        _ => Align::Center,
    };
    let is_align = |c: char| matches!(c, '<' | '>' | '^');
    // [fill]align — a fill char counts only when an align char immediately follows it.
    if cs.len() >= 2 && is_align(cs[1]) {
        sp.fill = cs[0];
        sp.align = Some(align_of(cs[1]));
        i = 2;
    } else if !cs.is_empty() && is_align(cs[0]) {
        sp.align = Some(align_of(cs[0]));
        i = 1;
    }
    // sign
    if i < cs.len() {
        match cs[i] {
            '+' => {
                sp.sign = Sign::Plus;
                i += 1;
            }
            ' ' => {
                sp.sign = Sign::Space;
                i += 1;
            }
            '-' => {
                sp.sign = Sign::Minus; // explicit default
                i += 1;
            }
            _ => {}
        }
    }
    if i < cs.len() && cs[i] == '#' {
        sp.alt = true;
        i += 1;
    }
    if i < cs.len() && cs[i] == '0' {
        sp.zero = true;
        i += 1;
    }
    let w0 = i;
    while i < cs.len() && cs[i].is_ascii_digit() {
        i += 1;
    }
    if i > w0 {
        sp.width = cs[w0..i].iter().collect::<String>().parse().unwrap_or(0);
    }
    if i < cs.len() && cs[i] == ',' {
        sp.group = true;
        i += 1;
    }
    if i < cs.len() && cs[i] == '.' {
        i += 1;
        let p0 = i;
        while i < cs.len() && cs[i].is_ascii_digit() {
            i += 1;
        }
        if i == p0 {
            return Err(ExprError::new(format!("FORMAT: precision '.' needs digits in {spec:?}")));
        }
        sp.prec = Some(cs[p0..i].iter().collect::<String>().parse().unwrap_or(0));
    }
    if i < cs.len() {
        let t = cs[i];
        if !matches!(t, 's' | 'd' | 'f' | 'x' | 'X' | 'o' | 'b' | 'e' | 'E' | '%') {
            return Err(ExprError::new(format!("FORMAT: unknown type {t:?} in spec {spec:?}")));
        }
        sp.ty = t;
        i += 1;
    }
    if i != cs.len() {
        return Err(ExprError::new(format!("FORMAT: bad format spec {spec:?}")));
    }
    Ok(sp)
}

/// Insert thousands separators into the leading run of digits of `s` (the integer part), leaving any
/// fractional/`%`/exponent tail alone. `"1234567" → "1,234,567"`, `"1234.5" → "1,234.5"`.
fn group_thousands(s: &str) -> String {
    let int_len = s.bytes().take_while(u8::is_ascii_digit).count();
    let (digits, rest) = s.split_at(int_len);
    if digits.len() <= 3 {
        return s.to_string();
    }
    let n = digits.len();
    let mut out = String::with_capacity(n + n / 3);
    for (idx, ch) in digits.chars().enumerate() {
        if idx > 0 && (n - idx) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.push_str(rest);
    out
}

/// The sign flag (`neg`) and unsigned MAGNITUDE string for a number, per the effective type. Kept
/// separate from the sign so zero-padding and the `#` prefix can slot in front of the digits.
fn numeric_mag(n: &Number, ty: char, spec: &FmtSpec) -> (bool, String) {
    let et = if ty == '\0' {
        match n {
            Number::Float(_) => 'f',
            _ => 'd',
        }
    } else {
        ty
    };
    let prec = spec.prec.map(|p| p.min(100) as u8);
    match et {
        'd' | 'x' | 'X' | 'o' | 'b' => {
            let big = match n {
                Number::Int(i) => num_bigint::BigInt::from(*i),
                Number::Big(b) => b.clone(),
                Number::Float(f) => num_bigint::BigInt::from(f.trunc() as i64),
            };
            let neg = num_traits::Signed::is_negative(&big);
            let mag = num_traits::Signed::abs(&big);
            let s = match et {
                'x' => mag.to_str_radix(16),
                'X' => mag.to_str_radix(16).to_uppercase(),
                'o' => mag.to_str_radix(8),
                'b' => mag.to_str_radix(2),
                _ => mag.to_str_radix(10),
            };
            (neg, s)
        }
        'e' | 'E' => {
            let f = n.as_f64();
            let s = format!("{:.*e}", spec.prec.unwrap_or(6).min(100), f.abs());
            (f.is_sign_negative() && f != 0.0, if et == 'E' { s.to_uppercase() } else { s })
        }
        '%' => {
            let f = n.as_f64();
            let body = crate::value::format_float_dp(f.abs() * 100.0, prec);
            (f.is_sign_negative() && f != 0.0, format!("{body}%"))
        }
        _ /* 'f' */ => {
            let f = n.as_f64();
            (f.is_sign_negative() && f != 0.0, crate::value::format_float_dp(f.abs(), prec))
        }
    }
}

/// Render one value per `spec`: build the (sign, prefix, magnitude), then pad to `width`.
fn apply_fmt(spec: &FmtSpec, val: &Value) -> Result<String, ExprError> {
    let numeric = matches!(spec.ty, 'd' | 'f' | 'x' | 'X' | 'o' | 'b' | 'e' | 'E' | '%')
        || (spec.ty == '\0' && val.is_numeric_type());

    // The "prefix" (sign + radix marker) stays flush-left of any zero-padding; `body` is what pads.
    let (prefix, body) = if numeric {
        let n = val
            .to_number()
            .map_err(|_| ExprError::new(format!("FORMAT: can't format {:?} as a number", val.to_str())))?;
        let (neg, mut mag) = numeric_mag(&n, spec.ty, spec);
        if spec.group && matches!(effective_ty(spec.ty, &n), 'd' | 'f' | '%') {
            mag = group_thousands(&mag);
        }
        let sign = if neg {
            "-"
        } else {
            match spec.sign {
                Sign::Plus => "+",
                Sign::Space => " ",
                Sign::Minus => "",
            }
        };
        let radix = if spec.alt {
            match spec.ty {
                'x' => "0x",
                'X' => "0X",
                'o' => "0o",
                'b' => "0b",
                _ => "",
            }
        } else {
            ""
        };
        (format!("{sign}{radix}"), mag)
    } else {
        // string / auto-string
        let s = val.to_str();
        let s = match spec.prec {
            Some(p) => s.chars().take(p).collect(),
            None => s,
        };
        (String::new(), s)
    };

    let base = format!("{prefix}{body}");
    let clen = base.chars().count();
    if clen >= spec.width {
        return Ok(base);
    }
    let pad = spec.width - clen;
    let align = spec.align.unwrap_or(if numeric { Align::Right } else { Align::Left });
    let fill = if spec.zero && spec.align.is_none() { '0' } else { spec.fill };
    // Zero-fill goes between the sign/prefix and the digits (so `-3.14` in width 8 is `-0003.14`).
    if fill == '0' && align == Align::Right {
        return Ok(format!("{prefix}{}{body}", "0".repeat(pad)));
    }
    let run = |n: usize| std::iter::repeat(fill).take(n).collect::<String>();
    Ok(match align {
        Align::Left => format!("{base}{}", run(pad)),
        Align::Right => format!("{}{base}", run(pad)),
        Align::Center => {
            let l = pad / 2;
            format!("{}{base}{}", run(l), run(pad - l))
        }
    })
}

/// The effective type once `'\0'` (auto) is resolved against the value.
fn effective_ty(ty: char, n: &Number) -> char {
    if ty == '\0' {
        match n {
            Number::Float(_) => 'f',
            _ => 'd',
        }
    } else {
        ty
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
    fn format_v1_placeholders_and_specs() {
        // Positional, indexed (reusable), and literal braces.
        assert_eq!(ev(r#"FORMAT("{} + {} = {}", 2, 3, 5)"#).to_str(), "2 + 3 = 5");
        assert_eq!(ev(r##"FORMAT("#{0}: {1} ({0})", 7, "Ada")"##).to_str(), "#7: Ada (7)");
        assert_eq!(ev(r#"FORMAT("{{{}}}", 9)"#).to_str(), "{9}");
        // Width + alignment + fill.
        assert_eq!(ev(r#"FORMAT("[{:>6}]", "hi")"#).to_str(), "[    hi]");
        assert_eq!(ev(r#"FORMAT("[{:<6}]", "hi")"#).to_str(), "[hi    ]");
        assert_eq!(ev(r#"FORMAT("[{:^6}]", "hi")"#).to_str(), "[  hi  ]");
        assert_eq!(ev(r#"FORMAT("[{:*>6}]", "hi")"#).to_str(), "[****hi]");
        // Numbers default to right-align; float precision; zero-pad keeps the sign in front.
        assert_eq!(ev(r#"FORMAT("[{:6}]", 42)"#).to_str(), "[    42]");
        assert_eq!(ev(r#"FORMAT("{:.2}", 3.14159)"#).to_str(), "3.14");
        assert_eq!(ev(r#"FORMAT("{:08.2}", 3.14159)"#).to_str(), "00003.14");
        assert_eq!(ev(r#"FORMAT("{:08.2}", 0 - 3.14159)"#).to_str(), "-0003.14");
        // Explicit types: string truncation, integer, hex.
        assert_eq!(ev(r#"FORMAT("{:.3s}", "abcdef")"#).to_str(), "abc");
        assert_eq!(ev(r#"FORMAT("{:d}", 7.9)"#).to_str(), "7"); // d truncates a float
        assert_eq!(ev(r#"FORMAT("{:x}", 255)"#).to_str(), "ff");
        assert_eq!(ev(r#"FORMAT("{:04X}", 255)"#).to_str(), "00FF");
        // Big integers format in full, including as hex.
        assert_eq!(
            ev(r#"FORMAT("{}", 2^256)"#).to_str(),
            "115792089237316195423570985008687907853269984665640564039457584007913129639936"
        );
        assert_eq!(ev(r#"FORMAT("{:x}", 2^64)"#).to_str(), "10000000000000000");
    }

    #[test]
    fn format_v2_grouping_sign_alt_and_radix_types() {
        // Thousands grouping (integers and the integer part of floats).
        assert_eq!(ev(r#"FORMAT("{:,}", 1234567)"#).to_str(), "1,234,567");
        assert_eq!(ev(r#"FORMAT("{:,.2}", 1234567.5)"#).to_str(), "1,234,567.50");
        assert_eq!(ev(r#"FORMAT("{:,}", 2^100)"#).to_str(), "1,267,650,600,228,229,401,496,703,205,376");
        // Sign flags: '+' always, ' ' space for positives (negatives keep '-').
        assert_eq!(ev(r#"FORMAT("{:+d}", 5)"#).to_str(), "+5");
        assert_eq!(ev(r#"FORMAT("{:+d}", 0 - 5)"#).to_str(), "-5");
        assert_eq!(ev(r#"FORMAT("[{: d}]", 5)"#).to_str(), "[ 5]");
        // Zero-pad keeps sign in front: "+000042".
        assert_eq!(ev(r#"FORMAT("{:+08}", 42)"#).to_str(), "+0000042");
        // Radix types + '#' prefix (prefix stays flush-left of zero padding).
        assert_eq!(ev(r#"FORMAT("{:o} {:b}", 8, 5)"#).to_str(), "10 101");
        assert_eq!(ev(r#"FORMAT("{:#x}", 255)"#).to_str(), "0xff");
        assert_eq!(ev(r#"FORMAT("{:#08x}", 255)"#).to_str(), "0x0000ff");
        // Scientific and percent.
        assert_eq!(ev(r#"FORMAT("{:.2e}", 31400)"#).to_str(), "3.14e4");
        assert_eq!(ev(r#"FORMAT("{:.1%}", 0.5)"#).to_str(), "50.0%");
    }

    #[test]
    fn format_v1_errors_are_trappable() {
        // Unmatched braces, an out-of-range argument, and a bogus type all error (not panic).
        assert!(parse(r#"FORMAT("{", 1)"#).is_ok()); // parses; fails at eval
        assert!(eval(&parse(r#"FORMAT("{")"#).unwrap(), &Ctx::new()).is_err());
        assert!(eval(&parse(r#"FORMAT("{1}", 5)"#).unwrap(), &Ctx::new()).is_err());
        assert!(eval(&parse(r#"FORMAT("{:q}", 5)"#).unwrap(), &Ctx::new()).is_err());
        assert!(eval(&parse(r#"FORMAT("}")"#).unwrap(), &Ctx::new()).is_err());
    }

    #[test]
    fn bignum_integers_stay_exact_beyond_i64() {
        // The headline: 2^256 is the EXACT 78-digit integer, not an f64 approximation.
        assert_eq!(
            ev("2^256").to_str(),
            "115792089237316195423570985008687907853269984665640564039457584007913129639936"
        );
        // i64 overflow on +, *, and unary - now PROMOTE (were: silently wrapped).
        assert_eq!(ev("9223372036854775807 + 1").to_str(), "9223372036854775808");
        assert_eq!(ev("2^128 * 2^128 == 2^256").to_str(), "1"); // exact big multiply
        assert_eq!(ev("0 - 2^100").to_str(), "-1267650600228229401496703205376");
        // FAC is exact past i64 too (30! has 33 digits).
        assert_eq!(ev("FAC(30)").to_str(), "265252859812191058636308480000000");
        assert!(matches!(ev("FAC(20)"), Value::Int(2432902008176640000))); // still fits i64 → plain Int
        // A big literal parses, prints in full, and round-trips through the SaveVars literal form.
        let big = "123456789012345678901234567890";
        assert_eq!(ev(big).to_str(), big);
        assert_eq!(ev(&ev(big).to_literal()).to_str(), big);
    }

    #[test]
    fn bignum_demotes_and_floats_still_contaminate() {
        // A big result that lands back inside i64 becomes a plain Int (canonical form).
        assert!(matches!(ev("2^100 - 2^100"), Value::Int(0)));
        assert!(matches!(ev("2^64 / 2^64"), Value::Int(1)));
        // A float ANYWHERE collapses exactness (the float-contamination rule is unchanged).
        assert!(matches!(ev("2^256 * 1.0"), Value::Float(_)));
        assert!(matches!(ev("2 ^ 0.5"), Value::Float(_))); // fractional exponent → float
        assert!(matches!(ev("2 ^ (0 - 1)"), Value::Float(_))); // negative exponent → float (0.5)
        assert_eq!(ev("2 ^ (0 - 1)").to_str(), "0.5");
    }

    #[test]
    fn bignum_div_mod_and_compare_are_exact() {
        assert_eq!(ev("10^24 / 3").to_str(), "3".repeat(24)); // floor division, exact
        assert_eq!(ev("10^24 % 7").to_str(), "1"); // exact modulo (3^24 ≡ 1 mod 7)
        // Floor division keeps its sign semantics for big negatives (rounds toward -∞).
        assert_eq!(ev("(0 - 10^24) / 3").to_str(), format!("-{}4", "3".repeat(23)));
        // Cross-type comparison (Int vs Big) is EXACT, not via a lossy f64.
        assert_eq!(ev("2^256 > 999999999999999999").to_str(), "1");
        assert_eq!(ev("2^256 == 2^256").to_str(), "1");
        assert_eq!(ev("2^256 == 2^256 - 1").to_str(), "0");
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
    fn jdn_conversions_and_roundtrip() {
        // Anchor values (standard integer JDN, proleptic Gregorian).
        assert_eq!(ev("DATE2JDN(2000, 1, 1)"), Value::Int(2451545));
        assert_eq!(ev("DATE2JDN(1, 1, 1)"), Value::Int(1721426)); // proleptic 0001-01-01
        assert_eq!(ev("JDN2DATE(2451545)").to_str(), "2000-01-01"); // default ISO mask
        // Round-trip over a spread of dates, including a leap day and a pre-Gregorian-switch year
        // (proleptic — no 1582 gap) to exercise the floor-division path.
        for (y, m, d) in [(2026, 7, 23), (2000, 2, 29), (1969, 12, 31), (1582, 10, 4), (1, 3, 1)] {
            let j = ymd_to_jdn(y, m, d);
            assert_eq!(jdn_to_ymd(j), (y, m as u32, d as u32), "roundtrip {y}-{m}-{d}");
        }
    }

    #[test]
    fn day_of_week_is_iso_numbered() {
        // 1 = Monday … 7 = Sunday.
        assert_eq!(ev("DOW(2000, 1, 1)"), Value::Int(6)); // a Saturday
        assert_eq!(ev("DOW(2026, 7, 23)"), Value::Int(4)); // a Thursday
        assert_eq!(ev("DOW(2026, 7, 26)"), Value::Int(7)); // a Sunday
        // Named forms via the mask (JDN2DATE reuses the DATE formatter).
        assert_eq!(ev(r#"JDN2DATE(DATE2JDN(2000, 1, 1), "dddd")"#).to_str(), "Saturday");
        assert_eq!(ev(r#"JDN2DATE(DATE2JDN(2026, 7, 23), "ddd mmm d, yyyy")"#).to_str(), "Thu Jul 23, 2026");
        assert_eq!(ev(r#"JDN2DATE(DATE2JDN(2026, 7, 23), "mmmm")"#).to_str(), "July");
    }

    #[test]
    fn date_diff_add_and_calendar_helpers() {
        // DATEDIFF is date2 − date1 in days (2000 is a leap year → 366 days across a full year).
        assert_eq!(ev("DATEDIFF(2000, 1, 1, 2000, 12, 31)"), Value::Int(365));
        assert_eq!(ev("DATEDIFF(2000, 1, 1, 2001, 1, 1)"), Value::Int(366));
        assert_eq!(ev("DATEDIFF(2000, 12, 31, 2000, 1, 1)"), Value::Int(-365)); // negative going backwards
        // DATEADD returns a date string (default ISO, optional mask), n may be negative.
        assert_eq!(ev("DATEADD(2000, 1, 1, 31)").to_str(), "2000-02-01");
        assert_eq!(ev("DATEADD(2000, 1, 1, 365)").to_str(), "2000-12-31");
        assert_eq!(ev(r#"DATEADD(2000, 3, 1, -1, "mmmm d")"#).to_str(), "February 29"); // leap-day step back
        // Leap year, days-in-month, day-of-year.
        assert_eq!(ev("LEAP(2000)"), Value::Int(1));
        assert_eq!(ev("LEAP(1900)"), Value::Int(0));
        assert_eq!(ev("LEAP(2024)"), Value::Int(1));
        assert_eq!(ev("DAYSINMONTH(2000, 2)"), Value::Int(29));
        assert_eq!(ev("DAYSINMONTH(2001, 2)"), Value::Int(28));
        assert_eq!(ev("DAYSINMONTH(2026, 4)"), Value::Int(30));
        assert_eq!(ev("DAYOFYEAR(2000, 3, 1)"), Value::Int(61)); // 31 + 29 + 1
        assert_eq!(ev("DAYOFYEAR(2001, 3, 1)"), Value::Int(60)); // 31 + 28 + 1
    }

    #[test]
    fn is_date_predicate_and_trappable_errors() {
        // ISDATE never traps — it just reports validity.
        assert_eq!(ev("ISDATE(2000, 2, 29)"), Value::Int(1));
        assert_eq!(ev("ISDATE(2001, 2, 29)"), Value::Int(0)); // 2001 not a leap year
        assert_eq!(ev("ISDATE(2026, 13, 1)"), Value::Int(0)); // bad month
        assert_eq!(ev("ISDATE(2026, 4, 31)"), Value::Int(0)); // April has 30 days
        // The arithmetic functions REJECT a malformed date with a trappable error (not a panic).
        for call in ["DATE2JDN(2001, 2, 29)", "DOW(2026, 13, 1)", "DATEADD(2026, 4, 31, 1)",
            "DATEDIFF(2000, 1, 1, 2026, 2, 30)", "DAYSINMONTH(2026, 0)"]
        {
            let e = eval(&parse(call).unwrap(), &Ctx::new());
            assert!(e.is_err(), "{call} should be a trappable error");
            assert!(e.unwrap_err().trappable, "{call} error should be trappable");
        }
    }

    #[test]
    fn today_needs_a_clock() {
        let ctx = Ctx::new()
            .with_now(DateParts { year: 2026, month: 7, day: 23, hour: 0, minute: 0, second: 0 });
        // TODAY() is today's JDN; anchoring "days until" arithmetic.
        assert_eq!(ev_ctx("TODAY()", &ctx), Value::Int(2461245));
        assert_eq!(ev_ctx("DATE2JDN(2026, 12, 25) - TODAY()", &ctx), Value::Int(155)); // days to Christmas
        // No clock → a plain (non-trappable) error, like DATE()/TIME().
        assert!(eval(&parse("TODAY()").unwrap(), &Ctx::new()).is_err());
    }

    #[test]
    fn dateparse_reads_components() {
        // Numeric masks, padded and variable-width. The result is [year, month, day].
        assert_eq!(ev(r#"DATEPARSE("07/23/2026", "mm/dd/yyyy")"#).to_str(), "[2026, 7, 23]");
        assert_eq!(ev(r#"DATEPARSE("2026-07-23", "yyyy-mm-dd")"#).to_str(), "[2026, 7, 23]");
        assert_eq!(ev(r#"DATEPARSE("7/3/2026", "m/d/yyyy")"#).to_str(), "[2026, 7, 3]");
        // Name tokens; a weekday in the string is matched, then ignored.
        assert_eq!(
            ev(r#"DATEPARSE("Thursday, July 23, 2026", "dddd, mmmm d, yyyy")"#).to_str(),
            "[2026, 7, 23]"
        );
        assert_eq!(ev(r#"DATEPARSE("Jul 5, 26", "mmm d, yy")"#).to_str(), "[2026, 7, 5]"); // yy 00–68 → 2000s
        assert_eq!(ev(r#"DATEPARSE("12/25/99", "mm/dd/yy")"#).to_str(), "[1999, 12, 25]"); // yy 69–99 → 1900s
        // A normal array — index a component directly.
        assert_eq!(ev(r#"DATEPARSE("2026-07-23", "yyyy-mm-dd")[1]"#), Value::Int(7));
    }

    #[test]
    fn dateparse_round_trips_with_the_formatter() {
        // format_date then parse_date recovers the same (y, m, d) across assorted dates and masks.
        for &(y, m, d) in &[(2026i64, 7u32, 23u32), (2000, 2, 29), (1969, 12, 31), (2001, 1, 1)] {
            let now = DateParts { year: y, month: m, day: d, hour: 0, minute: 0, second: 0 };
            for mask in ["yyyy-mm-dd", "m/d/yyyy", "dddd, mmmm d, yyyy", "ddd mmm d, yyyy"] {
                let s = format_date(mask, &now);
                assert_eq!(parse_date(mask, &s), Ok((y, m, d)), "round-trip {mask:?} on {s:?}");
            }
        }
    }

    #[test]
    fn dateparse_rejects_bad_input() {
        for call in [
            r#"DATEPARSE("2026-07-23", "mm/dd/yyyy")"#, // separators / order don't match
            r#"DATEPARSE("2026-02-30", "yyyy-mm-dd")"#, // parses, but Feb 30 isn't a real date
            r#"DATEPARSE("07-23", "mm-dd")"#,           // mask has no year
            r#"DATEPARSE("2026-07-23x", "yyyy-mm-dd")"#, // trailing text
            r#"DATEPARSE("13/01/2026", "mm/dd/yyyy")"#, // month 13
        ] {
            let e = eval(&parse(call).unwrap(), &Ctx::new());
            assert!(e.is_err(), "{call} should error");
            assert!(e.unwrap_err().trappable, "{call} error should be trappable");
        }
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
        // ISUPPER / ISLOWER — letters only; non-letters ignored, but at least one letter required.
        assert_eq!(ev(r#"ISUPPER("HELLO, WORLD!")"#), t); // punctuation/space ignored
        assert_eq!(ev(r#"ISUPPER("ABC1")"#), t); // digit ignored
        assert_eq!(ev(r#"ISUPPER("Hello")"#), f);
        assert_eq!(ev(r#"ISUPPER("123")"#), f); // no letters → false
        assert_eq!(ev(r#"ISUPPER("")"#), f);
        assert_eq!(ev(r#"ISLOWER("hello, world!")"#), t);
        assert_eq!(ev(r#"ISLOWER("abc9")"#), t);
        assert_eq!(ev(r#"ISLOWER("Hello")"#), f);
        assert_eq!(ev(r#"ISLOWER("123")"#), f);
        // Non-ASCII letters are uncased here (ASCII-only, like ISALPHA), so an accented-only string
        // has no cased letters → false either way.
        assert_eq!(ev(r#"ISUPPER("É")"#), f);
        // ISTITLE — first letter of each word upper, the rest lower; ≥1 letter.
        assert_eq!(ev(r#"ISTITLE("Hello World")"#), t);
        assert_eq!(ev(r#"ISTITLE("Hello123World")"#), t); // digits end/begin words
        assert_eq!(ev(r#"ISTITLE("Hello world")"#), f); // second word not capitalised
        assert_eq!(ev(r#"ISTITLE("HELLO")"#), f); // all caps isn't title case
        assert_eq!(ev(r#"ISTITLE("hello")"#), f);
        assert_eq!(ev(r#"ISTITLE("123")"#), f); // no letters
        assert_eq!(ev(r#"ISTITLE("")"#), f);
        // ISSPACE — every char is ASCII whitespace; empty is false. CHR builds tab/LF/CR/VT/FF
        // without depending on string-escape handling.
        assert_eq!(ev(r#"ISSPACE("   ")"#), t);
        assert_eq!(ev(r#"ISSPACE(CHR(9) . CHR(10) . " ")"#), t); // tab, LF, space
        assert_eq!(ev(r#"ISSPACE(CHR(11) . CHR(12) . CHR(13))"#), t); // VT, FF, CR
        assert_eq!(ev(r#"ISSPACE("a ")"#), f);
        assert_eq!(ev(r#"ISSPACE("")"#), f);
        // ISPRINT — printable ASCII (space through ~); controls and non-ASCII fail.
        assert_eq!(ev(r#"ISPRINT("Hello, World! 123")"#), t); // space is printable
        assert_eq!(ev(r#"ISPRINT(CHR(9))"#), f); // tab is a control char
        assert_eq!(ev(r#"ISPRINT(CHR(127))"#), f); // DEL
        assert_eq!(ev(r#"ISPRINT("café")"#), f); // é is non-ASCII
        assert_eq!(ev(r#"ISPRINT("")"#), f);
        // ISPUNCT — only ASCII punctuation; letters/digits/space fail.
        assert_eq!(ev(r#"ISPUNCT("!?.,;:-_")"#), t);
        assert_eq!(ev(r#"ISPUNCT("a!")"#), f);
        assert_eq!(ev(r#"ISPUNCT(". .")"#), f); // the space is not punctuation
        assert_eq!(ev(r#"ISPUNCT("")"#), f);
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
