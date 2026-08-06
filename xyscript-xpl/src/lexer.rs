//! Expression lexer (`stage1-interpreter-plan.md` §5).
//!
//! Tokenizes expression text for the `SET`/`IF`/`WHILE` sub-language. Ported
//! from QN's `_expr_tokenize`, with **two deliberate XyScript additions** (§12):
//!
//! - **Token-level `$` substitution.** QN pasted `$NAME` in as raw text *before*
//!   tokenizing. XyScript instead emits a [`Tok::Var`] token, resolved to a
//!   typed value at eval time — so quoting rules fall out of the grammar rather
//!   than from textual splicing. `$NAME`, `${NAME}`, positional `$1`/`$*`/`$#`.
//! - The string-escape set (`\n \t \r \0 \xHH \dNNN \uHHHH \\ \"`) is ported
//!   verbatim; unknown escapes pass through as `\X`.
//!
//! Each [`Token`] carries `end` (the byte offset just past it) so the parser can
//! reproduce QN's function-call adjacency rule (`NAME(` is a call, `NAME (` is
//! not — see `parser`/`expr`).

use crate::value::Number;

/// An operator / punctuation token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    Dot,
    Lt,
    Gt,
    Le,
    Ge,
    EqEq,
    Ne,
    LParen,
    RParen,
    Comma,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Colon,
}

/// A logical keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kw {
    And,
    Or,
    Not,
}

/// A lexical token kind.
#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Num(Number),
    /// A string literal (quotes stripped, escapes resolved).
    Str(String),
    /// A bare identifier that isn't a keyword — a function name or an unquoted
    /// string literal, disambiguated by the parser via the adjacency rule.
    Bare(String),
    /// A variable reference (`$NAME` / `${NAME}` / `$1` / `$*` / `$#`), resolved
    /// at eval time. The stored name is the raw identifier (case folded by the
    /// resolver, not here).
    Var(String),
    /// A binder reference (`@X` / `&X`, X = A-Z/0-9), resolved at eval time to the stored
    /// string. `script` = the `&` form. `slot` is upper-cased here.
    Binder { script: bool, slot: char },
    Kw(Kw),
    Op(Op),
}

/// A token plus the byte offset just past it (for the adjacency rule).
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub end: usize,
}

/// A lexing error (always surfaces as a fatal expression error upstream).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError(pub String);

/// Tokenize `s` (an entire expression string). Whitespace is discarded. Errors
/// on the first unrecognized input.
pub fn tokenize(s: &str) -> Result<Vec<Token>, LexError> {
    let (toks, err) = tokenize_partial(s);
    match err {
        Some(e) => Err(e),
        None => Ok(toks),
    }
}

/// Tokenize as far as possible, returning the tokens gathered before any error
/// plus the error (if one occurred). Used by the greedy prefix parse, where the
/// expression may end *before* a tail the expression lexer can't tokenize (e.g.
/// `IF $x XY echo {raw}` — the `{` is not expression syntax, but `$x` is a valid
/// condition and `XY echo {raw}` is the trailing command). Mirrors QN's
/// incremental-tokenizer graceful degradation for single-line `IF`.
pub fn tokenize_partial(s: &str) -> (Vec<Token>, Option<LexError>) {
    let b = s.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;

    while i < n {
        let c = b[i];
        match c {
            b' ' | b'\t' => {
                i += 1;
            }
            b'"' => match lex_string(s, b, i, n) {
                Ok((val, end)) => {
                    out.push(Token { tok: Tok::Str(val), end });
                    i = end;
                }
                Err(e) => return (out, Some(e)),
            },
            b'$' if i + 1 < n && b[i + 1] == b'(' => {
                // `$(expr)` — an inline sub-expression. In command-argument text this is
                // expanded during the substitution pass (see `subst.rs`), but that pass never
                // runs on compiled expressions (RETURN/IF/SET/FOR/SWITCH). Here the wrapper is
                // redundant — `$(x)` and `(x)` mean the same thing — so we simply skip the `$`
                // and let the `(` lex as a normal parenthesised group, preserving the typed
                // value rather than stringifying it.
                i += 1;
            }
            b'$' => match lex_var(s, b, i, n) {
                Ok((name, end)) => {
                    out.push(Token { tok: Tok::Var(name), end });
                    i = end;
                }
                Err(e) => return (out, Some(e)),
            },
            b'@' | b'&' => {
                // Binder reference: sigil + a single A-Z/0-9 slot (upper-cased here).
                let after = i + 1;
                if after < n && b[after].is_ascii_alphanumeric() {
                    let script = c == b'&';
                    let slot = (b[after] as char).to_ascii_uppercase();
                    out.push(Token { tok: Tok::Binder { script, slot }, end: after + 1 });
                    i = after + 1;
                } else {
                    return (
                        out,
                        Some(LexError(format!("{:?} with no binder slot (A-Z/0-9)", c as char))),
                    );
                }
            }
            b'0'..=b'9' => match lex_number(s, b, i, n) {
                Ok((num, end)) => {
                    out.push(Token { tok: Tok::Num(num), end });
                    i = end;
                }
                Err(e) => return (out, Some(e)),
            },
            _ if c == b'_' || c.is_ascii_alphabetic() => {
                let start = i;
                let mut j = i + 1;
                while j < n && (b[j] == b'_' || b[j].is_ascii_alphanumeric()) {
                    j += 1;
                }
                let ident = &s[start..j];
                let up = ident.to_ascii_uppercase();
                let tok = match up.as_str() {
                    "AND" => Tok::Kw(Kw::And),
                    "OR" => Tok::Kw(Kw::Or),
                    "NOT" => Tok::Kw(Kw::Not),
                    "TRUE" => Tok::Num(Number::Int(1)),
                    "FALSE" => Tok::Num(Number::Int(0)),
                    _ => Tok::Bare(ident.to_string()),
                };
                out.push(Token { tok, end: j });
                i = j;
            }
            _ => {
                // Two-char operators first.
                if i + 1 < n {
                    let two = &s[i..i + 2];
                    let op = match two {
                        "==" => Some(Op::EqEq),
                        "!=" => Some(Op::Ne),
                        "<=" => Some(Op::Le),
                        ">=" => Some(Op::Ge),
                        _ => None,
                    };
                    if let Some(op) = op {
                        out.push(Token { tok: Tok::Op(op), end: i + 2 });
                        i += 2;
                        continue;
                    }
                }
                let op = match c {
                    b'+' => Op::Plus,
                    b'-' => Op::Minus,
                    b'*' => Op::Star,
                    b'/' => Op::Slash,
                    b'%' => Op::Percent,
                    b'^' => Op::Caret,
                    b'.' => Op::Dot,
                    b'<' => Op::Lt,
                    b'>' => Op::Gt,
                    b'(' => Op::LParen,
                    b')' => Op::RParen,
                    b',' => Op::Comma,
                    b'[' => Op::LBracket,
                    b']' => Op::RBracket,
                    b'{' => Op::LBrace,
                    b'}' => Op::RBrace,
                    b':' => Op::Colon,
                    _ => {
                        return (
                            out,
                            Some(LexError(format!("unexpected character {:?}", c as char))),
                        )
                    }
                };
                out.push(Token { tok: Tok::Op(op), end: i + 1 });
                i += 1;
            }
        }
    }

    (out, None)
}

/// Lex a `"..."` string literal starting at `i` (the opening quote). Returns the
/// resolved content and the byte offset just past the closing quote.
fn lex_string(s: &str, b: &[u8], i: usize, n: usize) -> Result<(String, usize), LexError> {
    let mut j = i + 1;
    let mut buf = String::new();
    while j < n && b[j] != b'"' {
        if b[j] == b'\\' && j + 1 < n {
            let nxt = b[j + 1];
            match nxt {
                b'\\' | b'"' => {
                    buf.push(nxt as char);
                    j += 2;
                    continue;
                }
                b'n' => {
                    buf.push('\n');
                    j += 2;
                    continue;
                }
                b't' => {
                    buf.push('\t');
                    j += 2;
                    continue;
                }
                b'r' => {
                    buf.push('\r');
                    j += 2;
                    continue;
                }
                b'0' => {
                    buf.push('\0');
                    j += 2;
                    continue;
                }
                b'x' if j + 3 < n => {
                    let hh = &s[j + 2..j + 4];
                    if hh.len() == 2 && hh.bytes().all(|c| c.is_ascii_hexdigit()) {
                        let code = u8::from_str_radix(hh, 16).unwrap();
                        buf.push(code as char);
                        j += 4;
                        continue;
                    }
                    // fall through to verbatim
                    buf.push(b[j] as char);
                    j += 1;
                    continue;
                }
                b'd' if j + 4 < n => {
                    let ddd = &s[j + 2..j + 5];
                    if ddd.len() == 3 && ddd.bytes().all(|c| c.is_ascii_digit()) {
                        if let Ok(code) = ddd.parse::<u32>() {
                            if code <= 255 {
                                buf.push(code as u8 as char);
                                j += 5;
                                continue;
                            }
                        }
                    }
                    buf.push(b[j] as char);
                    j += 1;
                    continue;
                }
                b'u' if j + 5 < n => {
                    let hhhh = &s[j + 2..j + 6];
                    if hhhh.len() == 4 && hhhh.bytes().all(|c| c.is_ascii_hexdigit()) {
                        if let Ok(code) = u32::from_str_radix(hhhh, 16) {
                            if let Some(ch) = char::from_u32(code) {
                                buf.push(ch);
                                j += 6;
                                continue;
                            }
                        }
                    }
                    buf.push(b[j] as char);
                    j += 1;
                    continue;
                }
                _ => {
                    // Unknown/malformed escape: pass the backslash through
                    // verbatim (so `\d` in a regex doesn't blow up).
                    buf.push(b[j] as char);
                    j += 1;
                    continue;
                }
            }
        }
        // Push a whole (possibly multibyte) char.
        let ch_len = utf8_len(b[j]);
        buf.push_str(&s[j..j + ch_len]);
        j += ch_len;
    }
    if j >= n {
        return Err(LexError("unterminated string literal".into()));
    }
    Ok((buf, j + 1))
}

/// Lex a variable reference starting at `i` (the `$`). Supports `$name`,
/// `${name}`, `$<digits>`, `$*`, `$#`.
fn lex_var(s: &str, b: &[u8], i: usize, n: usize) -> Result<(String, usize), LexError> {
    let after = i + 1;
    if after >= n {
        return Err(LexError("'$' with no variable name".into()));
    }
    if b[after] == b'{' {
        // ${name} — read to the closing brace.
        let mut j = after + 1;
        while j < n && b[j] != b'}' {
            j += 1;
        }
        if j >= n {
            return Err(LexError("unterminated ${...} reference".into()));
        }
        let name = s[after + 1..j].trim().to_string();
        if name.is_empty() {
            return Err(LexError("empty ${} reference".into()));
        }
        return Ok((name, j + 1));
    }
    let c = b[after];
    if c == b'*' || c == b'#' {
        return Ok(((c as char).to_string(), after + 1));
    }
    if c.is_ascii_digit() {
        let mut j = after;
        while j < n && b[j].is_ascii_digit() {
            j += 1;
        }
        return Ok((s[after..j].to_string(), j));
    }
    if c == b'_' || c.is_ascii_alphabetic() {
        let mut j = after;
        while j < n && (b[j] == b'_' || b[j].is_ascii_alphanumeric()) {
            j += 1;
        }
        return Ok((s[after..j].to_string(), j));
    }
    Err(LexError(format!(
        "'$' followed by invalid name character {:?}",
        c as char
    )))
}

/// Lex a numeric literal (int unless it contains a `.`).
fn lex_number(s: &str, b: &[u8], i: usize, n: usize) -> Result<(Number, usize), LexError> {
    let mut j = i + 1;
    while j < n && (b[j].is_ascii_digit() || b[j] == b'.') {
        j += 1;
    }
    let tok = &s[i..j];
    let num = if tok.contains('.') {
        tok.parse::<f64>()
            .map(Number::Float)
            .map_err(|_| LexError(format!("bad number {tok:?}")))?
    } else {
        // Integer literal: i64 fast path, else arbitrary precision (so `115792…936` or `2^256`'s
        // printed form re-parses EXACTLY rather than erroring or decaying to a float).
        match tok.parse::<i64>() {
            Ok(i) => Number::Int(i),
            Err(_) => tok
                .parse::<num_bigint::BigInt>()
                .map(Number::Big)
                .map_err(|_| LexError(format!("bad number {tok:?}")))?,
        }
    };
    Ok((num, j))
}

/// Byte length of the UTF-8 character whose lead byte is `b`.
fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(s: &str) -> Vec<Tok> {
        tokenize(s).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn numbers_keep_int_vs_float() {
        assert_eq!(kinds("9"), vec![Tok::Num(Number::Int(9))]);
        assert_eq!(kinds("9.0"), vec![Tok::Num(Number::Float(9.0))]);
    }

    #[test]
    fn keywords_and_bools() {
        assert_eq!(
            kinds("a AND NOT b OR TRUE"),
            vec![
                Tok::Bare("a".into()),
                Tok::Kw(Kw::And),
                Tok::Kw(Kw::Not),
                Tok::Bare("b".into()),
                Tok::Kw(Kw::Or),
                Tok::Num(Number::Int(1)),
            ]
        );
    }

    #[test]
    fn operators_two_char_first() {
        assert_eq!(
            kinds("a == b != c <= d >= e < f > g"),
            vec![
                Tok::Bare("a".into()),
                Tok::Op(Op::EqEq),
                Tok::Bare("b".into()),
                Tok::Op(Op::Ne),
                Tok::Bare("c".into()),
                Tok::Op(Op::Le),
                Tok::Bare("d".into()),
                Tok::Op(Op::Ge),
                Tok::Bare("e".into()),
                Tok::Op(Op::Lt),
                Tok::Bare("f".into()),
                Tok::Op(Op::Gt),
                Tok::Bare("g".into()),
            ]
        );
    }

    #[test]
    fn variables_in_all_forms() {
        assert_eq!(kinds("$name"), vec![Tok::Var("name".into())]);
        assert_eq!(kinds("${full name}"), vec![Tok::Var("full name".into())]);
        assert_eq!(kinds("$1"), vec![Tok::Var("1".into())]);
        assert_eq!(kinds("$*"), vec![Tok::Var("*".into())]);
        assert_eq!(kinds("$#"), vec![Tok::Var("#".into())]);
    }

    #[test]
    fn dollar_paren_lexes_as_bare_group() {
        // `$(expr)` in an expression is redundant with `(expr)`: the `$` is skipped and the
        // parens lex normally, so `$(1 + 1)` and `(1 + 1)` tokenize identically.
        assert_eq!(kinds("$(1 + 1)"), kinds("(1 + 1)"));
        assert_eq!(
            kinds("$(1 + 1)"),
            vec![
                Tok::Op(Op::LParen),
                Tok::Num(Number::Int(1)),
                Tok::Op(Op::Plus),
                Tok::Num(Number::Int(1)),
                Tok::Op(Op::RParen),
            ]
        );
        // A `$var` inside still lexes as a variable.
        assert_eq!(kinds("$($x + 1)"), kinds("($x + 1)"));
    }

    #[test]
    fn string_escapes() {
        assert_eq!(kinds(r#""a\nb""#), vec![Tok::Str("a\nb".into())]);
        assert_eq!(kinds(r#""tab\there""#), vec![Tok::Str("tab\there".into())]);
        assert_eq!(kinds(r#""\x41""#), vec![Tok::Str("A".into())]);
        assert_eq!(kinds(r#""\d065""#), vec![Tok::Str("A".into())]);
        assert_eq!(kinds(r#""quote\"in""#), vec![Tok::Str("quote\"in".into())]);
        // Unknown escape passes through verbatim.
        assert_eq!(kinds(r#""\q""#), vec![Tok::Str("\\q".into())]);
    }

    #[test]
    fn unterminated_string_errors() {
        assert!(tokenize(r#""oops"#).is_err());
    }

    #[test]
    fn token_end_positions_enable_adjacency() {
        // LEN( -> Bare ends at 3, LParen starts at 3 (adjacent => call).
        let toks = tokenize("LEN(x)").unwrap();
        assert_eq!(toks[0].end, 3); // "LEN"
        assert_eq!(toks[1].end, 4); // "("
    }
}
