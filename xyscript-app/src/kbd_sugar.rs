//! KBD literal-text sugar (decision D4): desugar to XPL at keymap-install time.
//!
//! XyScript's headline tenet is **"one syntax — it's all XPL."** So the
//! convenience that lets a KBD cell *type a character* is **syntactic sugar that
//! desugars to a real `insert` command**, not a special case interpreted
//! elsewhere. A cell whose (trimmed) text is either:
//!
//! - a **single bare character** (any UTF-8 scalar except `"`; whitespace is
//!   trimmed away, so use the quoted form for a space), or
//! - a **single quoted-string literal** (`"..."`, internal `"` as `\"`),
//!
//! is rewritten to `insert "<that text>"`. Everything else is left untouched
//! (it is already XPL — a command). This runs in the **app layer**, downstream
//! of the keyboard crate, so that crate stays opaque (it never parses XPL) and
//! its Stage-0 tests are untouched — and the embedded default's bare `q` cells
//! type without any change upstream.
//!
//! Because the result is genuine XPL, at run time every cell flows through the
//! one interpreter on the one execution path. Edge: the only single-char
//! built-in verb is `?` (help), so a bare `?` cell types `?`; use `HELP`.

use xyscript_kbd::{Assignment, Keymap};

/// Rewrite every literal-text cell in `km` to an `insert "..."` command, leaving
/// command cells and `KF` cells untouched. Returns the desugared keymap.
pub fn desugar_keymap(mut km: Keymap) -> Keymap {
    for assignment in km.tables.values_mut() {
        if let Assignment::Xpl(s) = assignment {
            if let Some(desugared) = desugar_cell(s) {
                *assignment = Assignment::Xpl(desugared);
            }
        }
    }
    km
}

/// If `raw` is a literal-text cell, return its `insert "..."` desugaring;
/// otherwise `None` (leave the cell as-is).
fn desugar_cell(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    // (b) a single quoted-string literal -> insert it verbatim.
    if is_single_quoted_string(t) {
        return Some(format!("insert {t}"));
    }
    // (a) a single bare character (not a double quote).
    let mut chars = t.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c != '"' {
            let escaped = match c {
                '\\' => "\\\\".to_string(),
                other => other.to_string(),
            };
            return Some(format!("insert \"{escaped}\""));
        }
    }
    None
}

/// Whether `t` is exactly one quoted-string literal (and nothing else) — reusing
/// the interpreter's own lexer so escape rules are identical and compound
/// expressions like `"a" . "b"` are correctly rejected.
fn is_single_quoted_string(t: &str) -> bool {
    if !t.starts_with('"') {
        return false;
    }
    match xyscript_xpl::lexer::tokenize(t) {
        Ok(toks) => toks.len() == 1 && matches!(toks[0].tok, xyscript_xpl::lexer::Tok::Str(_)),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(raw: &str) -> Option<String> {
        desugar_cell(raw)
    }

    #[test]
    fn single_bare_char_becomes_insert() {
        assert_eq!(d("q").as_deref(), Some("insert \"q\""));
        assert_eq!(d("Q").as_deref(), Some("insert \"Q\""));
        assert_eq!(d("5").as_deref(), Some("insert \"5\""));
        assert_eq!(d("é").as_deref(), Some("insert \"é\"")); // one scalar
        assert_eq!(d("—").as_deref(), Some("insert \"—\""));
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_for_detection() {
        assert_eq!(d("  q  ").as_deref(), Some("insert \"q\""));
    }

    #[test]
    fn backslash_is_escaped() {
        assert_eq!(d("\\").as_deref(), Some("insert \"\\\\\""));
    }

    #[test]
    fn quoted_string_cell_becomes_insert_of_itself() {
        assert_eq!(d("\"hi\"").as_deref(), Some("insert \"hi\""));
        assert_eq!(d("\" \"").as_deref(), Some("insert \" \"")); // space via quotes
        assert_eq!(d("\"a;b\"").as_deref(), Some("insert \"a;b\""));
    }

    #[test]
    fn commands_and_multichar_are_left_alone() {
        assert_eq!(d("call save_buffer"), None);
        assert_eq!(d("insert \"x\""), None); // already an insert
        assert_eq!(d("loadkbd a.kbd"), None);
        assert_eq!(d("bs"), None); // a 2-char command
        assert_eq!(d("\"a\" . \"b\""), None); // compound expr, not one literal
        assert_eq!(d(""), None);
    }

    #[test]
    fn bare_question_mark_types_rather_than_helps() {
        // Documented edge: the only single-char verb (?) self-inserts.
        assert_eq!(d("?").as_deref(), Some("insert \"?\""));
    }

    #[test]
    fn desugar_keymap_rewrites_only_literal_cells() {
        use std::collections::HashMap;
        let mut tables: HashMap<(u32, u8), Assignment> = HashMap::new();
        tables.insert((0, 16), Assignment::Xpl("q".into())); // bare -> insert
        tables.insert((0, 17), Assignment::Xpl("call foo".into())); // command, kept
        tables.insert((0, 1), Assignment::Flip); // KF, kept
        let km = Keymap { modifiers: Vec::new(), tables, keys_max: 104, virtual_base: None };
        let km = desugar_keymap(km);
        assert_eq!(km.tables.get(&(0, 16)), Some(&Assignment::Xpl("insert \"q\"".into())));
        assert_eq!(km.tables.get(&(0, 17)), Some(&Assignment::Xpl("call foo".into())));
        assert_eq!(km.tables.get(&(0, 1)), Some(&Assignment::Flip));
    }
}
