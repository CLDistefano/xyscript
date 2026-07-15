//! `SORT` over selected lines — QN's `SORT [/W /R /D /F:n /N:n]` semantics.
//!
//! Default ordering is **letter-by-letter**: each line's key is its alphanumerics only,
//! lowercased (so `New York` → `newyork` sorts *after* `Newark` → `newark`). **/W** keys
//! on the lowercased whitespace-delimited word tokens instead (so `New York` → `[new,
//! york]` sorts *before* `Newark` → `[newark]`, since `new < newark`). **/F:n** drops the
//! first `n` characters of each line and **/N:n** then keeps only the next `n` before the
//! letter/word stripping. **/R** reverses, **/D** drops later lines whose key already
//! appeared. The sort is stable; a trailing newline on the selection is preserved.

use std::collections::HashSet;

use xyscript_xpl::SortSpec;

/// The comparison key for one line: a list of tokens (letter mode yields exactly one
/// token; word mode yields zero or more, so a blank line — empty list — sorts first).
fn key_of(line: &str, spec: &SortSpec) -> Vec<String> {
    // `/F:n` skip then `/N:n` take, both character-based, before any stripping.
    let narrowed: String = if spec.f_skip.is_some() || spec.n_take.is_some() {
        let skipped = line.chars().skip(spec.f_skip.unwrap_or(0));
        match spec.n_take {
            Some(n) => skipped.take(n).collect(),
            None => skipped.collect(),
        }
    } else {
        line.to_string()
    };
    if spec.word {
        narrowed.to_lowercase().split_whitespace().map(str::to_string).collect()
    } else {
        vec![narrowed
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()]
    }
}

/// Sort `text`'s lines under `spec`. Returns `(result, lines_in, lines_out)` — `lines_out`
/// differs from `lines_in` only when `/D` dropped duplicates.
pub fn sort_lines(text: &str, spec: &SortSpec) -> (String, usize, usize) {
    // Preserve the selection's trailing-newline shape: a paragraph keeps its terminator,
    // a partial last line doesn't gain one.
    let trailing = text.ends_with('\n');
    let body = if trailing { &text[..text.len() - 1] } else { text };
    let lines: Vec<&str> = body.split('\n').collect();
    let n_in = lines.len();

    // Decorate–sort–undecorate so each key is computed once, not per comparison.
    let mut keyed: Vec<(Vec<String>, &str)> =
        lines.iter().map(|&l| (key_of(l, spec), l)).collect();
    // Stable sort; `/R` is a stable *descending* sort (equal keys keep input order), which
    // `sort_by` with the reversed comparator gives — not `sort` then `reverse`.
    if spec.reverse {
        keyed.sort_by(|a, b| b.0.cmp(&a.0));
    } else {
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
    }
    if spec.dedup {
        let mut seen: HashSet<Vec<String>> = HashSet::new();
        keyed.retain(|(k, _)| seen.insert(k.clone()));
    }

    let n_out = keyed.len();
    let mut result = keyed.iter().map(|(_, l)| *l).collect::<Vec<_>>().join("\n");
    if trailing {
        result.push('\n');
    }
    (result, n_in, n_out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> SortSpec {
        SortSpec::default()
    }

    #[test]
    fn letter_vs_word_ordering() {
        let t = "Newark\nNew York\n";
        // Letter-by-letter: newark < newyork → Newark first.
        assert_eq!(sort_lines(t, &spec()).0, "Newark\nNew York\n");
        // Word-by-word: [new,york] < [newark] → New York first.
        let s = SortSpec { word: true, ..spec() };
        assert_eq!(sort_lines(t, &s).0, "New York\nNewark\n");
    }

    #[test]
    fn reverse_is_stable_descending() {
        // Two lines share key "boy"; reverse keeps their input order (stable), then "a".
        let t = "boy\nA\nBOY"; // keys: boy, a, boy
        let s = SortSpec { reverse: true, ..spec() };
        assert_eq!(sort_lines(t, &s).0, "boy\nBOY\nA"); // no trailing nl: input had none
    }

    #[test]
    fn dedup_by_key_ignores_case_and_punctuation() {
        let t = "Apple\napple!\nBanana\n";
        let s = SortSpec { dedup: true, ..spec() };
        let (out, n_in, n_out) = sort_lines(t, &s);
        assert_eq!(out, "Apple\nBanana\n"); // "apple!"/"Apple" share key "apple"
        assert_eq!((n_in, n_out), (3, 2));
    }

    #[test]
    fn f_skip_and_n_take_window_the_key() {
        // Sort on chars 3..5 (skip 3, take 2): keys "20","10","30".
        let t = "ID:20 b\nID:10 a\nID:30 c\n";
        let s = SortSpec { f_skip: Some(3), n_take: Some(2), ..spec() };
        assert_eq!(sort_lines(t, &s).0, "ID:10 a\nID:20 b\nID:30 c\n");
    }

    #[test]
    fn no_trailing_newline_preserved() {
        let (out, _, _) = sort_lines("c\nb\na", &spec());
        assert_eq!(out, "a\nb\nc"); // partial last-line selection: no gained newline
    }
}
