//! Variable substitution for **verb-argument text** (`stage1-interpreter-plan.md`
//! §5, §12).
//!
//! This is the text-level `$NAME` expansion QN does in `_substitute_vars`, but
//! scoped to where XyScript actually needs it: the arguments of ordinary verbs
//! (`PR $name`, `XY echo $x`, …). The `SET`/`IF`/`WHILE` expression
//! sub-language does **not** go through here — it resolves `$NAME` at the *token*
//! level (`lexer.rs` → `Expr::Var`, evaluated against the same [`EvalContext`]),
//! which is the §12 token-level-substitution decision.
//!
//! Supported forms: `$NAME`, `${NAME}`, positional `$1`/`$2`/…, `$*` (all args),
//! `$#` (arg count). `\$` emits a literal `$`. A `$` not followed by a
//! recognized name is left as a literal `$`. Undefined variables substitute to
//! the empty string. Lookup precedence (specials over user vars) and name
//! folding live in the [`EvalContext`] implementor.
//!
//! ## Recorded divergences from QN (token-level model makes them clean)
//!
//! - **`${NAME}` is supported** (QN's `_substitute_vars` does not support the
//!   brace form — its test documents `${NAME}` surviving literally). XyScript
//!   adds it, matching the lexer.
//! - **No in-string `"`/`\` escaping of substituted values.** QN escaped a
//!   value's quotes/backslashes when the `$NAME` sat inside a `"..."` region,
//!   because QN re-lexed the substituted line. XyScript verb args are taken as
//!   literal text after substitution (no re-lex), so a `"` in a value is just a
//!   `"`. (Expression contexts never reach this code, so nothing relies on the
//!   old escaping.)
//! - **No `$NAME[idx]` → `$NAME_idx` array sugar.** That was QN's stringly-typed
//!   array workaround; XyScript has real arrays (§3/§12).

use crate::expr::EvalContext;
use crate::value::Value;

/// Expand `$`-variables in `text` against `ctx`, returning the substituted
/// string. See the module docs for the supported forms.
pub fn substitute(text: &str, ctx: &dyn EvalContext) -> String {
    if !text.contains('$')
        && !text.contains('\\')
        && !text.contains('@')
        && !text.contains('&')
    {
        return text.to_string();
    }
    let b = text.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut i = 0;

    while i < n {
        let c = b[i];
        // Literal `\$` -> `$` (backslash consumed).
        if c == b'\\' && i + 1 < n && b[i + 1] == b'$' {
            out.push('$');
            i += 2;
            continue;
        }
        if c == b'$' {
            // `$(expr)` — evaluate any expression inline (functions like CHR/ASC/UPPER, arithmetic,
            // nested `$vars`/`$(…)`), substituting its string value. A malformed/unbalanced group
            // yields empty (consistent with an unknown `$var`).
            if i + 1 < n && b[i + 1] == b'(' {
                if let Some((expr_src, end)) = scan_paren_group(text, b, i + 1, n) {
                    out.push_str(&eval_expr_inline(&expr_src, ctx));
                    i = end;
                    continue;
                }
                // Unbalanced `$(` — leave the `$` literal and continue past it.
                out.push('$');
                i += 1;
                continue;
            }
            if let Some((name, end)) = parse_name(text, b, i, n) {
                // Optional `[index]` element access: `$arr[2]`, `$arr[$i]`.
                if end < n && b[end] == b'[' {
                    if let Some((elem, new_end)) = index_subst(text, b, end, n, &name, ctx) {
                        out.push_str(&elem);
                        i = new_end;
                        continue;
                    }
                }
                let value = ctx
                    .get_var(&name)
                    .map(|v| v.to_str_dp(ctx.float_decimals()))
                    .unwrap_or_default();
                out.push_str(&value);
                i = end;
                continue;
            }
            // `$` not followed by a recognized name: leave it literal.
            out.push('$');
            i += 1;
            continue;
        }
        // Binder value reference `@X` / `&X`, but only when bounded by non-alphanumerics,
        // so literal `@`/`&` in text (`a@b.com`, `AT&T`, `@AB`) is left untouched.
        if (c == b'@' || c == b'&') && is_binder_ref(b, i, n) {
            let script = c == b'&';
            let slot = (b[i + 1] as char).to_ascii_uppercase();
            let value = ctx.binder(script, slot).unwrap_or_default();
            out.push_str(&value);
            i += 2;
            continue;
        }
        // Ordinary (possibly multibyte) char.
        let len = utf8_len(c);
        out.push_str(&text[i..i + len]);
        i += len;
    }

    out
}

/// Parse a variable reference whose `$` is at byte `i`. Returns the name and the
/// byte offset just past the whole reference, or `None` if `$` isn't followed by
/// a recognized name.
fn parse_name(text: &str, b: &[u8], i: usize, n: usize) -> Option<(String, usize)> {
    let after = i + 1;
    if after >= n {
        return None;
    }
    // ${name}
    if b[after] == b'{' {
        let mut j = after + 1;
        while j < n && b[j] != b'}' {
            j += 1;
        }
        if j >= n {
            return None; // unterminated -> leave `${` literal
        }
        let name = text[after + 1..j].trim();
        if name.is_empty() {
            return None;
        }
        return Some((name.to_string(), j + 1));
    }
    let c = b[after];
    // $* / $#
    if c == b'*' || c == b'#' {
        return Some(((c as char).to_string(), after + 1));
    }
    // $<digits>
    if c.is_ascii_digit() {
        let mut j = after;
        while j < n && b[j].is_ascii_digit() {
            j += 1;
        }
        return Some((text[after..j].to_string(), j));
    }
    // $name
    if c == b'_' || c.is_ascii_alphabetic() {
        let mut j = after;
        while j < n && (b[j] == b'_' || b[j].is_ascii_alphanumeric()) {
            j += 1;
        }
        return Some((text[after..j].to_string(), j));
    }
    None
}

/// Scan a `$( … )` group whose `(` is at byte `open`, returning the inner expression text and
/// the byte offset just past the matching `)`. Tracks nested parens and skips `"…"` string
/// literals (so a `)` inside a string doesn't close the group), mirroring the lexer's `\`-escapes.
/// Returns `None` if the group is unbalanced (no matching `)`).
fn scan_paren_group(text: &str, b: &[u8], open: usize, n: usize) -> Option<(String, usize)> {
    let mut j = open + 1;
    let mut depth = 1usize;
    let mut in_str = false;
    while j < n {
        let ch = b[j];
        if in_str {
            match ch {
                b'\\' => {
                    j += 2; // skip the escaped char (ASCII escapes: \" \\ \n …)
                    continue;
                }
                b'"' => in_str = false,
                _ => {}
            }
            j += 1;
            continue;
        }
        match ch {
            b'"' => in_str = true,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((text[open + 1..j].to_string(), j + 1));
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// Parse + evaluate an inline `$(…)` expression to its string value; empty on any parse/eval error.
fn eval_expr_inline(src: &str, ctx: &dyn EvalContext) -> String {
    match crate::expr::parse(src) {
        Ok(expr) => crate::expr::eval(&expr, ctx)
            .map(|v| v.to_str_dp(ctx.float_decimals()))
            .unwrap_or_default(),
        Err(_) => String::new(),
    }
}

/// Resolve `$name[index]` element access during substitution. `open` is the byte
/// index of `[`. The index may be a literal (`2`, a map key) or a single `$var`.
/// Returns `(element_text, byte_offset_past_])`, or `None` if there's no closing
/// `]` (then the `[…]` is left as literal text after the substituted name).
fn index_subst(
    text: &str,
    b: &[u8],
    open: usize,
    n: usize,
    name: &str,
    ctx: &dyn EvalContext,
) -> Option<(String, usize)> {
    let mut close = open + 1;
    while close < n && b[close] != b']' {
        close += 1;
    }
    if close >= n {
        return None; // no closing bracket — leave `[` literal
    }
    let idx_text = text[open + 1..close].trim();
    // Index value: a `$var` resolves through the context; otherwise the literal
    // text (which `index_value` coerces to a number for arrays, or uses as a map
    // key).
    let index_value_v = if let Some(stripped) = idx_text.strip_prefix('$') {
        ctx.get_var(stripped).unwrap_or_else(|| Value::str(""))
    } else {
        Value::str(idx_text)
    };
    let base = ctx.get_var(name).unwrap_or_else(|| Value::str(""));
    let elem = crate::expr::index_value(&base, &index_value_v)
        .map(|v| v.to_str_dp(ctx.float_decimals()))
        .unwrap_or_default();
    Some((elem, close + 1))
}

/// Is there a binder reference (`@X`/`&X`, single A-Z/0-9 slot) at byte `i`? To avoid
/// corrupting literal `@`/`&` in text (`a@b.com`, `AT&T`, `R&D`, `@AB`), it counts only
/// when bounded by non-alphanumerics: the preceding source char (if any) and the char
/// after the slot (if any) must not be ASCII-alphanumeric, and the slot must be alphanumeric.
fn is_binder_ref(b: &[u8], i: usize, n: usize) -> bool {
    if i > 0 && b[i - 1].is_ascii_alphanumeric() {
        return false; // glued to a preceding word char (e.g. `a@b`)
    }
    if i + 1 >= n || !b[i + 1].is_ascii_alphanumeric() {
        return false; // no slot char after the sigil
    }
    if i + 2 < n && b[i + 2].is_ascii_alphanumeric() {
        return false; // more than one char (e.g. `@AB`) — not a single-slot ref
    }
    true
}

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
    use crate::value::{fold_ident, Value};
    use std::collections::HashMap;

    struct Ctx {
        vars: HashMap<String, Value>,
        binders: HashMap<(bool, char), String>,
    }
    impl Ctx {
        fn new(pairs: &[(&str, Value)]) -> Ctx {
            let mut vars = HashMap::new();
            for (k, v) in pairs {
                vars.insert(fold_ident(k), v.clone());
            }
            Ctx { vars, binders: HashMap::new() }
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
        fn binder(&self, script: bool, slot: char) -> Option<String> {
            self.binders.get(&(script, slot)).cloned()
        }
    }

    fn sub(text: &str, pairs: &[(&str, Value)]) -> String {
        substitute(text, &Ctx::new(pairs))
    }

    // ---- cases mirrored from QN's test_quicknotes.py (where we agree) -------

    #[test]
    fn simple_var() {
        assert_eq!(sub("PR $NAME", &[("NAME", Value::str("Alice"))]), "PR Alice");
    }

    #[test]
    fn undefined_var_is_empty() {
        assert_eq!(sub("PR $UNKNOWN", &[]), "PR ");
    }

    #[test]
    fn positional_args() {
        assert_eq!(
            sub("PR $1 $2", &[("1", Value::str("first")), ("2", Value::str("second"))]),
            "PR first second"
        );
    }

    #[test]
    fn escaped_dollar_is_literal() {
        assert_eq!(sub(r"PR \$NAME", &[("NAME", Value::str("Alice"))]), "PR $NAME");
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(sub("$name", &[("NAME", Value::str("Alice"))]), "Alice");
    }

    // ---- XyScript divergences (recorded) -----------------------------------

    #[test]
    fn brace_form_is_supported_unlike_qn() {
        // QN leaves `${NAME}` literal; XyScript expands it.
        assert_eq!(sub("${NAME}", &[("NAME", Value::str("Alice"))]), "Alice");
        // Brace form disambiguates against trailing identifier chars.
        assert_eq!(
            sub("${x}_suffix", &[("x", Value::str("VAL"))]),
            "VAL_suffix"
        );
    }

    #[test]
    fn no_in_string_quote_escaping() {
        // A value containing `"` substitutes verbatim (no `\"` escaping).
        assert_eq!(
            sub(r#"PR "$Q""#, &[("Q", Value::str(r#"a"b"#))]),
            r#"PR "a"b""#
        );
    }

    // ---- other behavior -----------------------------------------------------

    #[test]
    fn dollar_without_name_is_literal() {
        assert_eq!(sub("cost is $ 5", &[]), "cost is $ 5");
        assert_eq!(sub("trailing $", &[]), "trailing $");
    }

    #[test]
    fn numeric_value_stringifies() {
        // Int renders without a trailing .0 (value model).
        assert_eq!(sub("n=$n", &[("n", Value::int(42))]), "n=42");
        assert_eq!(sub("f=$f", &[("f", Value::float(2.5))]), "f=2.5");
    }

    #[test]
    fn star_and_count_specials() {
        let pairs = &[("*", Value::str("a b c")), ("#", Value::int(3))];
        assert_eq!(sub("all=$* count=$#", pairs), "all=a b c count=3");
    }

    #[test]
    fn array_element_access() {
        let arr = Value::Array(vec![Value::str("zero"), Value::str("one"), Value::int(42)]);
        assert_eq!(sub("PR $a[1]", &[("a", arr.clone())]), "PR one");
        // $var index.
        assert_eq!(
            sub("PR $a[$i]", &[("a", arr.clone()), ("i", Value::int(2))]),
            "PR 42"
        );
        // Out of range -> empty.
        assert_eq!(sub("PR $a[9]", &[("a", arr)]), "PR ");
        // No closing bracket -> the `[` is left literal after the value.
        assert_eq!(sub("$x[", &[("x", Value::str("V"))]), "V[");
    }

    #[test]
    fn utf8_passthrough() {
        assert_eq!(sub("café $x", &[("x", Value::str("☕"))]), "café ☕");
    }

    // ---- binder value refs in arg text (bounded, collision-safe) -----------

    #[test]
    fn binder_refs_substitute_when_bounded() {
        let ctx = Ctx::new(&[]).with_binder(false, 'A', "hi").with_binder(true, 'Z', "run");
        assert_eq!(substitute("PR @A", &ctx), "PR hi"); // whitespace-bounded
        assert_eq!(substitute("@a done", &ctx), "hi done"); // start + case-fold
        assert_eq!(substitute("(@A)", &ctx), "(hi)"); // punctuation-bounded
        assert_eq!(substitute("&Z it", &ctx), "run it"); // script binder as a value
        assert_eq!(substitute("@Q", &ctx), ""); // unset → empty
    }

    #[test]
    fn literal_at_and_amp_in_text_are_untouched() {
        let ctx = Ctx::new(&[]).with_binder(false, 'B', "X").with_binder(false, 'T', "X");
        assert_eq!(substitute("mail a@b.com", &ctx), "mail a@b.com"); // glued left
        assert_eq!(substitute("AT&T and R&D", &ctx), "AT&T and R&D"); // glued left
        assert_eq!(substitute("@AB", &ctx), "@AB"); // multi-char, not a single slot
        assert_eq!(substitute("x & y", &ctx), "x & y"); // bare & (no slot)
    }

    #[test]
    fn inline_expr_evaluates_functions_and_arithmetic() {
        // $(expr) runs the expression layer inline: functions (CHR/ASC/UPPER), arithmetic, …
        assert_eq!(sub("code $(ASC(\"A\"))", &[]), "code 65");
        assert_eq!(sub("char $(CHR(66))", &[]), "char B");
        assert_eq!(sub("$(1 + 2)", &[]), "3");
        // Nested $vars resolve inside the expression (the expr layer reads the same context).
        assert_eq!(sub("$(UPPER($name))", &[("name", Value::str("ada"))]), "ADA");
        // Round-trip CHR(ASC(x)) and nested parens / a string with a ')' inside.
        assert_eq!(sub("$(CHR(ASC(\"Z\")))", &[]), "Z");
        assert_eq!(sub("$(REPLACE(\"a)b\", \")\", \"-\"))", &[]), "a-b");
    }

    #[test]
    fn inline_expr_malformed_or_unbalanced_is_empty_or_literal() {
        // A parse/eval error yields empty (like an unknown $var).
        assert_eq!(sub("x$(1 +)y", &[]), "xy");
        assert_eq!(sub("x$(NOPE(1))y", &[]), "xy");
        // An unbalanced group leaves the `$` literal and keeps scanning the rest as text.
        assert_eq!(sub("a $(CHR(66) tail", &[]), "a $(CHR(66) tail");
    }

    #[test]
    fn escaped_dollar_paren_passes_through_literally() {
        // `\$(…)` escapes the sigil so a literal `$(` survives — e.g. to hand bash/PowerShell
        // command substitution through a shell-out (`XY echo \$(date)`), like `\$var` already does.
        assert_eq!(sub("echo \\$(date)", &[]), "echo $(date)");
    }
}
