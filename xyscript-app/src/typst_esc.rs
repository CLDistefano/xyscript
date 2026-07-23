//! Typst-markup escaping, shared by every source→typst converter ([`crate::gfm`], [`crate::html`]).
//!
//! Both converters emit typst *markup* built from untrusted document text, so the escaping rules are
//! a correctness boundary rather than a formatting nicety: text content must never be able to inject
//! typst. These live in their own module so neither converter owns the other's escaping.

/// Backslash-escape the typst markup-significant characters in text content.
/// Critically includes `#` (code) and `$` (math) so document text can never
/// inject typst. Over-escaping the line-start markers (`= - +`) is harmless
/// (typst renders `\-` as `-`).
pub(crate) fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(
            ch,
            '\\' | '#' | '$' | '*' | '_' | '`' | '<' | '>' | '@' | '~' | '[' | ']' | '=' | '+' | '-'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Build a typst string literal (`"…"`) with `\`, `"`, newlines, and tabs escaped.
pub(crate) fn typst_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_escaping_blocks_typst_injection() {
        // `#` and `$` are the dangerous ones — they start code and math.
        assert_eq!(escape_text("a #b $c"), r"a \#b \$c");
        assert_eq!(escape_text("x[y]"), r"x\[y\]");
        assert_eq!(escape_text("plain words"), "plain words");
    }

    #[test]
    fn string_literals_escape_quotes_and_control_chars() {
        assert_eq!(typst_str("plain"), "\"plain\"");
        assert_eq!(typst_str(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(typst_str("a\nb\tc"), r#""a\nb\tc""#);
        assert_eq!(typst_str(r"back\slash"), r#""back\\slash""#);
    }
}
