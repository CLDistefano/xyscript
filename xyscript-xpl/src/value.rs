//! The XPL value model (`stage1-interpreter-plan.md` §3, §12).
//!
//! QN uses a single stringly-typed slot; XyScript starts from a **real tagged
//! value**, preserving QN's *observable* behavior while removing the
//! stringly-typed ceiling (arrays/maps are first-class from day one).
//!
//! ## §12 decision — real integers vs. floats (LOCKED)
//!
//! QN's `_expr_to_num` tries `int()` then `float()`, and its `/` operator does
//! **floor division when both operands are integers** but true division
//! otherwise (`a // b` vs `a / b`). The test oracle pins this: `9 / 2 == 4` but
//! `9.0 / 2 == 4.5`. So the int/float distinction is *observable*, and XyScript
//! models it directly with [`Value::Int`] / [`Value::Float`] (and a [`Number`]
//! helper carrying the same distinction through arithmetic). This is the §12
//! "real number type / integer exactness — decide early" item, decided **in**.
//!
//! ## Observable rules ported from QN (`quicknotes.py`)
//!
//! Lifted from QN's tested leaf semantics (`_expr_truthy` / `_expr_to_num` /
//! `_expr_to_str` / `_expr_compare`):
//!
//! - **Coercion** ([`Value::to_number`]): a string coerces if it parses — as an
//!   integer first, else a float; numbers pass through.
//! - **Truthiness** ([`Value::is_truthy`]): numeric nonzero is true; `""` is
//!   false; a string that parses as `0` is false; any other string is true.
//! - **Display** ([`Value::to_str`]): an integer-valued float renders as `5`,
//!   not `5.0`.
//!
//! ## One deliberate divergence from QN (recorded per the plan's mandate)
//!
//! **Comparison is type-aware, so quoting forces string** — the behavior both
//! design docs specify (`10 < 9` false, `"10" < "9"` **true**). QN's actual
//! `_expr_compare` coerces *both* operands to numbers whenever both are
//! numeric-coercible, so in real QN `"10" < "9"` is numeric and therefore
//! *false*. XyScript's real type system lets the value's *type* carry the intent
//! ([`xpl_cmp`]):
//!
//! - both numeric-typed → numeric compare;
//! - both [`Value::Str`] → string compare (quoting forced string);
//! - mixed → **numeric-first**: numeric iff *both* coerce, else string.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{Signed, ToPrimitive, Zero};

/// A number that remembers whether it is an integer or a float — the distinction QN's arithmetic
/// depends on — and, for integers, stays EXACT beyond i64. All numeric arithmetic flows through here so
/// the int/float/bignum promotion rules live in exactly one place.
///
/// `Int` is the fast path; an integer op that overflows i64 auto-promotes to `Big` (arbitrary
/// precision), so `2^256` is the exact 78-digit integer rather than an f64 approximation. **Canonical
/// form:** a value that fits i64 is ALWAYS `Int`, never `Big` (see [`Number::norm`]) — so `Int` and
/// `Big` never denote the same number, keeping equality and comparison consistent. A float anywhere in
/// an expression still collapses the result to `Float` (exactness is an integer-chain property).
#[derive(Debug, Clone, PartialEq)]
pub enum Number {
    Int(i64),
    Big(BigInt),
    Float(f64),
}

// These are deliberate inherent helpers, not the `std::ops` traits: `add`/`sub`/
// `mul` are two-argument associated fns, and `div`/`rem` are *fallible*
// (division by zero) — none match the trait signatures.
#[allow(clippy::should_implement_trait)]
impl Number {
    /// As an `f64` (for comparison and float arithmetic). A `Big` too large for f64 saturates to a
    /// signed infinity — matching what `2.0.powf(256.0)` would have produced before, only now the
    /// EXACT value is still available as the integer itself.
    pub fn as_f64(&self) -> f64 {
        match self {
            Number::Int(i) => *i as f64,
            Number::Big(b) => b
                .to_f64()
                .unwrap_or(if b.is_negative() { f64::NEG_INFINITY } else { f64::INFINITY }),
            Number::Float(f) => *f,
        }
    }

    /// Render like QN's `_expr_to_str`: integers (any size) print in full; integer-valued floats lose
    /// any trailing `.0`.
    pub fn to_str(&self) -> String {
        match self {
            Number::Int(i) => i.to_string(),
            Number::Big(b) => b.to_string(),
            Number::Float(f) => format_float(*f),
        }
    }

    /// Collapse a `BigInt` back to `Int` when it fits i64 — keeps the fast path canonical (equality and
    /// the `Int`/`Big` split rely on it).
    fn norm(b: BigInt) -> Number {
        match b.to_i64() {
            Some(i) => Number::Int(i),
            None => Number::Big(b),
        }
    }

    /// The integer as a `BigInt`, for the arbitrary-precision path (only called when neither operand is
    /// a float, so a `Float` here is a bug — hence `unwrap` at the call sites via `expect`-free `?`).
    fn as_big(&self) -> Option<BigInt> {
        match self {
            Number::Int(i) => Some(BigInt::from(*i)),
            Number::Big(b) => Some(b.clone()),
            Number::Float(_) => None,
        }
    }

    fn is_float(&self) -> bool {
        matches!(self, Number::Float(_))
    }

    /// `a + b` (int + int = int, promoting to Big on i64 overflow; any float promotes to float).
    pub fn add(a: &Number, b: &Number) -> Number {
        match (a, b) {
            (Number::Int(x), Number::Int(y)) => match x.checked_add(*y) {
                Some(s) => Number::Int(s),
                None => Number::Big(BigInt::from(*x) + BigInt::from(*y)),
            },
            _ if a.is_float() || b.is_float() => Number::Float(a.as_f64() + b.as_f64()),
            _ => Number::norm(a.as_big().unwrap() + b.as_big().unwrap()),
        }
    }
    /// `a - b`.
    pub fn sub(a: &Number, b: &Number) -> Number {
        match (a, b) {
            (Number::Int(x), Number::Int(y)) => match x.checked_sub(*y) {
                Some(s) => Number::Int(s),
                None => Number::Big(BigInt::from(*x) - BigInt::from(*y)),
            },
            _ if a.is_float() || b.is_float() => Number::Float(a.as_f64() - b.as_f64()),
            _ => Number::norm(a.as_big().unwrap() - b.as_big().unwrap()),
        }
    }
    /// `a * b`.
    pub fn mul(a: &Number, b: &Number) -> Number {
        match (a, b) {
            (Number::Int(x), Number::Int(y)) => match x.checked_mul(*y) {
                Some(p) => Number::Int(p),
                None => Number::Big(BigInt::from(*x) * BigInt::from(*y)),
            },
            _ if a.is_float() || b.is_float() => Number::Float(a.as_f64() * b.as_f64()),
            _ => Number::norm(a.as_big().unwrap() * b.as_big().unwrap()),
        }
    }
    /// `a / b` — **floor division when both are integers** (QN `a // b`), true division otherwise.
    /// `Err` on division by zero (the caller raises a fatal).
    pub fn div(a: &Number, b: &Number) -> Result<Number, DivByZero> {
        match (a, b) {
            (Number::Int(x), Number::Int(y)) => {
                if *y == 0 {
                    return Err(DivByZero);
                }
                // i64::MIN / -1 overflows — promote just that lone case.
                match x.checked_div(*y) {
                    Some(_) => Ok(Number::Int(floor_div(*x, *y))),
                    None => Ok(Number::norm(BigInt::from(*x).div_floor(&BigInt::from(*y)))),
                }
            }
            _ if a.is_float() || b.is_float() => {
                let d = b.as_f64();
                if d == 0.0 {
                    return Err(DivByZero);
                }
                Ok(Number::Float(a.as_f64() / d))
            }
            _ => {
                let (x, y) = (a.as_big().unwrap(), b.as_big().unwrap());
                if y.is_zero() {
                    return Err(DivByZero);
                }
                Ok(Number::norm(x.div_floor(&y)))
            }
        }
    }
    /// `a % b` — Python-style modulo (result takes the sign of the divisor). `Err` on modulo by zero.
    pub fn rem(a: &Number, b: &Number) -> Result<Number, DivByZero> {
        match (a, b) {
            (Number::Int(x), Number::Int(y)) => {
                if *y == 0 {
                    return Err(DivByZero);
                }
                Ok(Number::Int(floor_mod(*x, *y))) // MIN % -1 == 0, so no overflow
            }
            _ if a.is_float() || b.is_float() => {
                let d = b.as_f64();
                if d == 0.0 {
                    return Err(DivByZero);
                }
                Ok(Number::Float(float_mod(a.as_f64(), d)))
            }
            _ => {
                let (x, y) = (a.as_big().unwrap(), b.as_big().unwrap());
                if y.is_zero() {
                    return Err(DivByZero);
                }
                Ok(Number::norm(x.mod_floor(&y)))
            }
        }
    }
    /// Unary negation (promotes `-i64::MIN` to Big so it stays exact).
    pub fn neg(&self) -> Number {
        match self {
            Number::Int(i) => match i.checked_neg() {
                Some(v) => Number::Int(v),
                None => Number::Big(-BigInt::from(*i)),
            },
            Number::Big(b) => Number::norm(-b.clone()),
            Number::Float(f) => Number::Float(-f),
        }
    }
    /// `a^b` for integer operands: exact (`Big` on overflow) when the exponent is a non-negative
    /// integer; otherwise `None` so the caller falls back to `powf`.
    pub fn pow_exact(a: &Number, b: &Number) -> Option<Number> {
        let exp = match b {
            Number::Int(e) if *e >= 0 => u32::try_from(*e).ok()?,
            _ => return None, // negative/huge/float exponent → float path
        };
        match a {
            Number::Int(base) => match base.checked_pow(exp) {
                Some(p) => Some(Number::Int(p)),
                None => Some(Number::norm(num_traits::Pow::pow(BigInt::from(*base), exp))),
            },
            Number::Big(base) => Some(Number::norm(num_traits::Pow::pow(base.clone(), exp))),
            Number::Float(_) => None,
        }
    }
    /// Numeric ordering for [`xpl_cmp`]. Int/Big compare EXACTLY; a float on either side compares as
    /// f64 (as before).
    pub fn cmp_num(a: &Number, b: &Number) -> Ordering {
        match (a, b) {
            (Number::Int(x), Number::Int(y)) => x.cmp(y),
            _ if a.is_float() || b.is_float() => {
                a.as_f64().partial_cmp(&b.as_f64()).unwrap_or(Ordering::Equal)
            }
            _ => a.as_big().unwrap().cmp(&b.as_big().unwrap()),
        }
    }
}

/// Signals a division/modulo by zero (a fatal in the error model).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DivByZero;

/// Python-style floor division (rounds toward negative infinity).
fn floor_div(a: i64, b: i64) -> i64 {
    let q = a.wrapping_div(b);
    let r = a.wrapping_rem(b);
    if r != 0 && ((r < 0) != (b < 0)) {
        q - 1
    } else {
        q
    }
}

/// Python-style integer modulo (result has the sign of the divisor).
fn floor_mod(a: i64, b: i64) -> i64 {
    let r = a.wrapping_rem(b);
    if r != 0 && ((r < 0) != (b < 0)) {
        r + b
    } else {
        r
    }
}

/// Python-style float modulo (result has the sign of the divisor).
fn float_mod(a: f64, b: f64) -> f64 {
    let r = a % b;
    if r != 0.0 && ((r < 0.0) != (b < 0.0)) {
        r + b
    } else {
        r
    }
}

/// A runtime XPL value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    /// Arbitrary-precision integer (an integer op that overflowed i64). Canonical: only ever holds a
    /// value that does NOT fit i64, so `Int`/`Big` never represent the same number.
    Big(BigInt),
    Float(f64),
    /// Internal boolean (e.g. the result of a comparison/logical op). Surfaces
    /// QN truthiness and stringifies as `1`/`0`.
    Bool(bool),
    /// Real arrays — replaces QN's `$arr_0` / `$arr[i]` naming convention (§12).
    Array(Vec<Value>),
    /// Real maps (§12).
    Map(BTreeMap<String, Value>),
}

impl Value {
    /// Construct a string value.
    pub fn str(s: impl Into<String>) -> Value {
        Value::Str(s.into())
    }
    /// Construct an integer value.
    pub fn int(i: i64) -> Value {
        Value::Int(i)
    }
    /// Construct a float value.
    pub fn float(f: f64) -> Value {
        Value::Float(f)
    }
    /// Wrap a [`Number`] back into a `Value`.
    pub fn from_number(n: Number) -> Value {
        match n {
            Number::Int(i) => Value::Int(i),
            Number::Big(b) => Value::Big(b),
            Number::Float(f) => Value::Float(f),
        }
    }

    /// True for the numeric-*typed* variants. Used by [`xpl_cmp`] to choose
    /// numeric vs. string comparison — about the *type*, not whether a string
    /// looks numeric (that is [`Value::to_number`]).
    pub fn is_numeric_type(&self) -> bool {
        matches!(self, Value::Int(_) | Value::Big(_) | Value::Float(_) | Value::Bool(_))
    }

    /// Coerce to a [`Number`] (QN `_expr_to_num`): integers parse first, then
    /// floats; `Bool` is `1`/`0`. `Err` for non-numeric strings and arrays/maps.
    // `Result<_, ()>` is deliberate: the failure carries no detail (callers raise
    // their own error), but the `Result` shape lets them use `?`.
    #[allow(clippy::result_unit_err)]
    pub fn to_number(&self) -> Result<Number, ()> {
        match self {
            Value::Int(i) => Ok(Number::Int(*i)),
            Value::Big(b) => Ok(Number::Big(b.clone())),
            Value::Float(f) => Ok(Number::Float(*f)),
            Value::Bool(b) => Ok(Number::Int(if *b { 1 } else { 0 })),
            Value::Str(s) => {
                let t = s.trim();
                // Integers first (i64, then arbitrary-precision for a huge literal), then float — so a
                // long digit string coerces EXACTLY rather than through a lossy f64.
                if let Ok(i) = t.parse::<i64>() {
                    Ok(Number::Int(i))
                } else if let Ok(b) = t.parse::<BigInt>() {
                    Ok(Number::Big(b))
                } else if let Ok(f) = t.parse::<f64>() {
                    Ok(Number::Float(f))
                } else {
                    Err(())
                }
            }
            Value::Array(_) | Value::Map(_) => Err(()),
        }
    }

    /// Display string (QN `_expr_to_str`). Integer-valued floats render without
    /// `.0`; `Bool` renders `1`/`0`.
    pub fn to_str(&self) -> String {
        self.to_str_dp(None)
    }

    /// Display string with an optional fixed number of decimal places applied to floats (the
    /// `DECIMALS` setting). `None` = the default rendering (integer-valued floats lose `.0`,
    /// others print at full precision). Only `Float`s are affected — integers/strings are
    /// unchanged; arrays/maps format their float elements the same way.
    pub fn to_str_dp(&self, decimals: Option<u8>) -> String {
        match self {
            Value::Str(s) => s.clone(),
            Value::Int(i) => i.to_string(),
            Value::Big(b) => b.to_string(),
            Value::Float(f) => format_float_dp(*f, decimals),
            Value::Bool(b) => if *b { "1" } else { "0" }.to_string(),
            Value::Array(items) => {
                let inner: Vec<String> = items.iter().map(|v| v.to_str_dp(decimals)).collect();
                format!("[{}]", inner.join(", "))
            }
            Value::Map(m) => {
                let inner: Vec<String> =
                    m.iter().map(|(k, v)| format!("{k}={}", v.to_str_dp(decimals))).collect();
                format!("{{{}}}", inner.join(", "))
            }
        }
    }

    /// Serialize to an expression **literal** that re-parses (via `expr::parse` + `eval`) to an
    /// equal value — the on-disk form written by `SaveVars`. Strings are double-quoted with lexer
    /// escapes (`\\ \" \n \r \t`), floats keep a decimal point, arrays use `[…]`, and maps use the
    /// `{"k": v}` literal syntax the parser accepts. Because `LoadVars` evaluates each literal
    /// directly (no `$`-substitution pass), a `$` inside a string needs no escaping.
    pub fn to_literal(&self) -> String {
        match self {
            Value::Str(s) => format!("\"{}\"", escape_literal(s)),
            Value::Int(i) => i.to_string(),
            // A bare big-integer literal re-parses to `Big` (the lexer promotes on i64 overflow).
            Value::Big(b) => b.to_string(),
            Value::Bool(b) => if *b { "1" } else { "0" }.to_string(),
            Value::Float(f) => {
                if !f.is_finite() {
                    // inf/NaN have no numeric literal; keep the file parseable by storing the text.
                    return format!("\"{}\"", escape_literal(&format_float_dp(*f, None)));
                }
                let s = format_float_dp(*f, None);
                if s.contains(|c| c == '.' || c == 'e' || c == 'E') { s } else { format!("{s}.0") }
            }
            Value::Array(items) => {
                let inner: Vec<String> = items.iter().map(Value::to_literal).collect();
                format!("[{}]", inner.join(", "))
            }
            Value::Map(m) => {
                let inner: Vec<String> = m
                    .iter()
                    .map(|(k, v)| format!("\"{}\": {}", escape_literal(k), v.to_literal()))
                    .collect();
                format!("{{{}}}", inner.join(", "))
            }
        }
    }

    /// QN truthiness (`_expr_truthy`): numeric nonzero is true; `""` is false; a
    /// string parseable as `0` is false; any other string is true. Empty
    /// arrays/maps are false, non-empty true.
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Int(i) => *i != 0,
            Value::Big(b) => !b.is_zero(), // canonical Big is never 0, but be defensive
            Value::Float(f) => *f != 0.0,
            Value::Bool(b) => *b,
            Value::Str(s) => {
                if s.is_empty() {
                    return false;
                }
                match s.trim().parse::<f64>() {
                    Ok(n) => n != 0.0,
                    Err(_) => true,
                }
            }
            Value::Array(items) => !items.is_empty(),
            Value::Map(m) => !m.is_empty(),
        }
    }

    /// `<` per the type-aware comparison model.
    pub fn lt(&self, other: &Value) -> bool {
        xpl_cmp(self, other) == Ordering::Less
    }
    /// `>` per the type-aware comparison model.
    pub fn gt(&self, other: &Value) -> bool {
        xpl_cmp(self, other) == Ordering::Greater
    }
    /// `==` per the type-aware comparison model (numeric-first for mixed).
    pub fn xpl_eq(&self, other: &Value) -> bool {
        xpl_cmp(self, other) == Ordering::Equal
    }
}

/// Render a float the way QN's `_expr_to_str` does: integer-valued floats lose
/// the `.0`.
fn format_float(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

/// Format a float with an optional fixed decimal-place count (the `DECIMALS` setting). `Some(d)`
/// → exactly `d` places (e.g. `d=2`: `3.14159` → `3.14`, `3.0` → `3.00`); non-finite values
/// (`NaN`/`inf`) ignore the setting. `None` → the default [`format_float`] rendering.
pub fn format_float_dp(f: f64, decimals: Option<u8>) -> String {
    match decimals {
        Some(d) if f.is_finite() => format!("{:.*}", d as usize, f),
        _ => format_float(f),
    }
}

/// Escape a string for a double-quoted expression literal (used by [`Value::to_literal`]). Only the
/// escapes the string lexer decodes are emitted (`\\ \" \n \r \t`); all other characters — including
/// `$` and non-ASCII — pass through unchanged, since `LoadVars` never runs `$`-substitution on them.
fn escape_literal(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            _ => o.push(c),
        }
    }
    o
}

/// The type-aware comparison at the heart of the value model (see module docs):
/// both numeric-typed → numeric; both string-typed → string; mixed → numeric
/// iff *both* coerce, else string. Integer/integer compares exactly; anything
/// else compares as `f64`.
pub fn xpl_cmp(a: &Value, b: &Value) -> Ordering {
    let numeric = match (a, b) {
        (Value::Str(_), Value::Str(_)) => false,
        _ if a.is_numeric_type() && b.is_numeric_type() => true,
        _ => a.to_number().is_ok() && b.to_number().is_ok(),
    };

    if numeric {
        match (a.to_number(), b.to_number()) {
            (Ok(x), Ok(y)) => Number::cmp_num(&x, &y),
            _ => Ordering::Equal, // unreachable: `numeric` implies both coerce
        }
    } else {
        a.to_str().cmp(&b.to_str())
    }
}

/// Fold an identifier (verb / variable / function / label / sub name) for
/// case-insensitive lookup. Centralized so every site folds identically. Values
/// are never folded — only identifiers (§3 "case-insensitive identifiers,
/// case-preserving values").
pub fn fold_ident(name: &str) -> String {
    name.to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_str_dp_applies_fixed_decimals_to_floats_only() {
        assert_eq!(Value::Float(3.14159).to_str_dp(Some(2)), "3.14");
        assert_eq!(Value::Float(3.0).to_str_dp(Some(2)), "3.00"); // whole float → fixed places
        assert_eq!(Value::Float(3.14159).to_str_dp(None), "3.14159"); // off = default
        assert_eq!(Value::Int(5).to_str_dp(Some(2)), "5"); // integers untouched
        assert_eq!(Value::str("hi").to_str_dp(Some(2)), "hi"); // strings untouched
        assert_eq!(format_float_dp(f64::NAN, Some(2)), "NaN"); // non-finite ignores the setting
    }

    // ---- coercion (QN _expr_to_num: int first, then float) -----------------

    #[test]
    fn coercion_prefers_int_then_float() {
        assert_eq!(Value::str("10").to_number(), Ok(Number::Int(10)));
        assert_eq!(Value::str("10.5").to_number(), Ok(Number::Float(10.5)));
        assert_eq!(Value::str("  -3 ").to_number(), Ok(Number::Int(-3)));
        assert_eq!(Value::int(7).to_number(), Ok(Number::Int(7)));
        assert_eq!(Value::Bool(true).to_number(), Ok(Number::Int(1)));
        assert!(Value::str("abc").to_number().is_err());
        assert!(Value::str("").to_number().is_err());
    }

    // ---- arithmetic (QN _add/_mul: int floor division, float true division) -

    #[test]
    fn integer_floor_division() {
        // The oracle: 9 / 2 == 4, but 9.0 / 2 == 4.5.
        assert_eq!(Number::div(&Number::Int(9), &Number::Int(2)), Ok(Number::Int(4)));
        assert_eq!(
            Number::div(&Number::Float(9.0), &Number::Int(2)),
            Ok(Number::Float(4.5))
        );
        // Floor toward negative infinity (Python //): -7 / 2 == -4.
        assert_eq!(Number::div(&Number::Int(-7), &Number::Int(2)), Ok(Number::Int(-4)));
        assert_eq!(Number::div(&Number::Int(5), &Number::Int(0)), Err(DivByZero));
    }

    #[test]
    fn modulo_takes_sign_of_divisor() {
        assert_eq!(Number::rem(&Number::Int(7), &Number::Int(3)), Ok(Number::Int(1)));
        assert_eq!(Number::rem(&Number::Int(-7), &Number::Int(3)), Ok(Number::Int(2)));
        assert_eq!(Number::rem(&Number::Int(5), &Number::Int(0)), Err(DivByZero));
    }

    #[test]
    fn add_promotes_to_float_when_either_is_float() {
        assert_eq!(Number::add(&Number::Int(2), &Number::Int(3)), Number::Int(5));
        assert_eq!(
            Number::add(&Number::Int(2), &Number::Float(3.5)),
            Number::Float(5.5)
        );
    }

    // ---- display (QN _expr_to_str) -----------------------------------------

    #[test]
    fn integer_valued_floats_drop_the_dot_zero() {
        assert_eq!(Value::float(5.0).to_str(), "5");
        assert_eq!(Value::float(5.5).to_str(), "5.5");
        assert_eq!(Value::int(-12).to_str(), "-12");
        assert_eq!(Value::Bool(true).to_str(), "1");
        assert_eq!(Value::str("KeepCase").to_str(), "KeepCase");
    }

    // ---- truthiness (QN _expr_truthy) --------------------------------------

    #[test]
    fn truthiness_matches_qn() {
        assert!(Value::int(1).is_truthy());
        assert!(!Value::int(0).is_truthy());
        assert!(!Value::str("").is_truthy());
        assert!(!Value::str("0").is_truthy());
        assert!(!Value::str("0.0").is_truthy());
        assert!(!Value::str("00").is_truthy()); // "00" parses as 0
        assert!(Value::str("abc").is_truthy());
        assert!(Value::str("5").is_truthy());
        assert!(!Value::Array(vec![]).is_truthy());
        assert!(Value::Array(vec![Value::int(1)]).is_truthy());
    }

    // ---- comparison (type-aware model; the recorded divergence from QN) -----

    #[test]
    fn numeric_then_string_comparison() {
        assert!(!Value::int(10).lt(&Value::int(9)));
        assert!(Value::str("10").lt(&Value::str("9")));
    }

    #[test]
    fn mixed_comparison_is_numeric_first_when_both_coerce() {
        assert!(Value::str("10").gt(&Value::int(9)));
        assert!(Value::str("abc").gt(&Value::int(9))); // "abc" vs "9" string compare
    }

    #[test]
    fn equality_is_numeric_first_for_mixed() {
        assert!(Value::int(10).xpl_eq(&Value::str("10")));
        assert!(!Value::str("10").xpl_eq(&Value::str("10.0"))); // both string-typed
        assert!(Value::int(10).xpl_eq(&Value::str("10.0"))); // mixed -> numeric
    }

    #[test]
    fn identifiers_fold_case_values_do_not() {
        assert_eq!(fold_ident("SetVar"), "SETVAR");
        assert_eq!(fold_ident("goto"), "GOTO");
        assert_eq!(Value::str("KeepCase").to_str(), "KeepCase");
    }
}
