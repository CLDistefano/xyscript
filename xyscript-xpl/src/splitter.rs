//! Command splitter (`stage1-interpreter-plan.md` §4).
//!
//! Splits a source line into individual commands on `;`, honoring the exact
//! chain semantics QN documents (this is user-facing contract, ported from
//! QN's `_split_commands` / `_unescape_chain_chars`):
//!
//! - `\;` → a literal `;` (not a separator); `\\` → `\`; `\"` → `"`; `\#` → `#`.
//! - **Quoted-string opacity:** a `;` inside a `"..."` region is not a
//!   separator (`SET x = "a;b"; PR x` → two commands).
//! - **Windows-path carve-out:** `\;` followed by whitespace or end-of-string
//!   is *not* an escape — the `\` belongs to a path (`CD D:\; DIR` parses as two
//!   commands), the `;` separates.
//! - **Inline `#` comments:** a `#` that is unquoted, preceded by whitespace (or
//!   at chunk start), and followed by whitespace/EOL — or by another `#`, so a run
//!   of hashes (`##`, `###`) also comments — starts a comment that runs to end of
//!   line. `\#` is a literal `#`.
//! - **Exempt verbs consume the rest of the line verbatim** (no further
//!   splitting or unescaping): the shell pass-through verbs `X`/`XY`/`XV`. QN's
//!   `LOADKEY` family is *retired* in XyScript (`.KBD`/`LOADKBD` replace it), so it
//!   is intentionally not exempt. (`CLIP` takes a normal quoted-string argument, so it
//!   splits like any other verb — a `;` inside the quotes is protected, one after the
//!   closing quote separates.)
//! - **Bracket-delimited exempt form re-enables chaining.** An exempt verb whose
//!   argument is wrapped in `[...]` (`X [foo; bar]; PR $RC`) is *delimited*: the outer
//!   brackets are stripped, everything between them (including `;`, `(`, `)`, `"`, `#`)
//!   is the verbatim shell command, and `;`-splitting RESUMES after the matching `]`.
//!   Brackets nest (`X [(cd a; make)]` → a real subshell, no doubling needed) and
//!   `\[`/`\]` escape a literal bracket. An unterminated `[`, or trailing content after
//!   `]` that isn't `;`/EOL, falls back to swallowing the whole line (nothing dropped).
//!   The **bare** form (`X foo; bar`, no leading `[`) still swallows the rest, unchanged.
//! - **Search/change verbs are self-delimiting, and chaining RESUMES after the
//!   pattern.** `SE`/`CH`/… take a self-delimiting pattern (`SE "x"`, `SE /;/`,
//!   `CH /a/b/`) whose `;`/`#`/escapes are part of the pattern, but a `;` *after*
//!   the closing delimiter separates commands again (`SE "foo"; PR done` → two
//!   commands). The splitter scans to the pattern's close (and the replacement's,
//!   for `CH`) then splits normally. A **bare** (undelimited) pattern (`SE foo`)
//!   or one with no trailing delimiter (`CH /a/b`) still consumes the rest — use a
//!   delimited form to chain after it. The pattern itself is emitted verbatim.
//!
//! Everything is ASCII-special-driven, so the scan operates on bytes and only
//! ever breaks at ASCII boundaries — UTF-8 multibyte content passes through
//! untouched.
//!
//! Note the block-`IF`-split footgun does **not** exist in XyScript: a
//! `;`-chained `IF ... ; ... ; ENDIF` is just three commands the *parser* (§6)
//! folds into one structured block. The splitter has no knowledge of blocks.

/// Verbs whose tail is a single *command / payload* taken verbatim (no `;` splitting, no unescaping),
/// NOT XyScript statements to split here: the shell pass-throughs (`X`/`XY`/`XV`), and `XLINK` (whose
/// tail is the XPL payload stored on the link, re-parsed only when the link is activated). The tail is
/// swallowed whole UNLESS it is wrapped in `[...]`, in which case the brackets delimit it and
/// `;`-chaining resumes after them (see [`exempt_split`]); the outer brackets are stripped from the
/// emitted chunk. So `XLINK OPEN x;GOTOLINE 5` stores the whole payload, and `XLINK [payload];next`
/// still lets a command follow. (`CLIP` is NOT exempt — it takes a quoted-string argument and splits
/// like any other verb.)
const EXEMPT_VERBS: &[&str] = &["X", "XY", "XV", "XLINK"];

/// Verbs that take a `[...]`-delimited block of XPL CODE (not a shell command): `RUNCODE`. Unlike
/// the shell pass-throughs, the brackets are KEPT in the emitted chunk (the verb handler strips them
/// and runs the inner RAW, so the code's own `$`-refs resolve when it runs), and the BARE form (no
/// leading `[`) falls through to ordinary `;`-splitting — so `RUNCODE $var; next` is unchanged, and
/// only the new `RUNCODE [ … ; … ]` form protects its inner `;`.
const CODE_EXEMPT_VERBS: &[&str] = &["RUNCODE"];

/// Search / change verbs: their self-delimiting pattern is scanned (not blindly
/// swallowed) so `;`-chaining can resume after it. See [`search_split`].
const SEARCH_VERBS: &[&str] =
    &["SE", "SEB", "SEA", "SEBA", "CH", "CHA", "CI", "CIA", "CV", "CVA"];

/// The single-letter search flags (`/W`, `-t`, grouped `/WT`), matched so a leading
/// flag token is skipped to find the pattern delimiter — mirrors `vm::FLAG_LETTERS`.
const SEARCH_FLAG_LETTERS: &[u8] = b"TWABRStwabrs";

/// Split `s` into its component commands. Empty chunks (e.g. from `a;;b` or a
/// trailing `;`) are dropped. Each returned command is trimmed and unescaped.
pub fn split_commands(s: &str) -> Vec<String> {
    split_commands_indexed(s).into_iter().map(|(_, c)| c).collect()
}

/// Like [`split_commands`], but each chunk is paired with its **0-based byte offset** within `s` —
/// the position of the command's first non-whitespace char. Used for `line:col` diagnostics
/// (e.g. the `BK` breakpoint header).
pub fn split_commands_indexed(s: &str) -> Vec<(usize, String)> {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut out = Vec::new();
    let mut i = 0;

    while i < n {
        // Skip leading whitespace before each chunk.
        while i < n && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        if i >= n {
            break;
        }
        let start = i; // the command's start offset on the line

        // Exempt verbs (`X`/`XY`/`XV`): a bracketed argument `X [cmd]` is delimited, so
        // `;`-chaining resumes after `]`; the bare form swallows the rest of the line.
        match exempt_split(&s[i..]) {
            Some(ExemptSplit::All) => {
                out.push((start, s[i..].to_string()));
                return out;
            }
            Some(ExemptSplit::Upto { chunk, resume }) => {
                if !chunk.is_empty() {
                    out.push((start, chunk));
                }
                match resume {
                    Some(off) => {
                        i += off;
                        continue;
                    }
                    None => return out,
                }
            }
            None => {} // not an exempt verb — fall through to the normal scan
        }

        // Search/change verbs: scan past their self-delimiting pattern (emitted
        // verbatim), then resume `;`-splitting after it.
        match search_split(&s[i..]) {
            Some(SearchSplit::All) => {
                out.push((start, s[i..].to_string()));
                return out;
            }
            Some(SearchSplit::Upto { end, resume }) => {
                let chunk = s[i..i + end].trim();
                if !chunk.is_empty() {
                    out.push((start, chunk.to_string()));
                }
                match resume {
                    Some(off) => {
                        i += off;
                        continue;
                    }
                    None => return out, // pattern ran to EOL (or a trailing comment)
                }
            }
            None => {} // not a search verb — fall through to the normal scan
        }

        // Scan to the next unescaped, unquoted `;` or comment `#`.
        let chunk_start = i;
        let mut j = i;
        let mut in_quote = false;
        let mut comment = false;
        while j < n {
            let ch = bytes[j];
            if ch == b'\\' && j + 1 < n && matches!(bytes[j + 1], b'\\' | b';' | b'"' | b'#') {
                // Windows-path carve-out: `\;<ws>` or `\;<EOS>` is not an
                // escape — step past the `\` and let the `;` separate.
                if bytes[j + 1] == b';'
                    && (j + 2 >= n || bytes[j + 2] == b' ' || bytes[j + 2] == b'\t')
                {
                    j += 1;
                    continue;
                }
                j += 2;
                continue;
            }
            if ch == b'"' {
                in_quote = !in_quote;
                j += 1;
                continue;
            }
            if ch == b';' && !in_quote {
                break;
            }
            // Inline `#` comment: unquoted, preceded by ws (or chunk start), and followed by ws,
            // EOL, or another `#`. The trailing-`#` case makes a RUN of hashes (`##`, `###`, …) a
            // comment too — common for section headers / emphasised comments — which a single `#`
            // (which must be followed by ws/EOL, so `#foo` stays literal) otherwise wouldn't allow.
            // Runs to EOL.
            if ch == b'#'
                && !in_quote
                && (j == chunk_start || bytes[j - 1] == b' ' || bytes[j - 1] == b'\t')
                && (j + 1 >= n
                    || bytes[j + 1] == b' '
                    || bytes[j + 1] == b'\t'
                    || bytes[j + 1] == b'#')
            {
                comment = true;
                break;
            }
            j += 1;
        }

        let chunk = unescape_chain_chars(&s[i..j]);
        let chunk = chunk.trim();
        if !chunk.is_empty() {
            out.push((start, chunk.to_string()));
        }
        if comment {
            return out;
        }
        i = j + 1;
    }

    out
}

/// How an exempt verb (`X`/`XY`/`XV`) divides the rest of the line.
enum ExemptSplit {
    /// Swallow the whole remaining line verbatim (the bare form, an unterminated `[`, or
    /// trailing junk after `]`): no chaining.
    All,
    /// Bracketed form `X [cmd]`: `chunk` is the emitted command (verb-token + the bracket
    /// contents, outer brackets stripped). `resume` is the offset just past a separating
    /// `;` to continue splitting from, or `None` if the command ran to end-of-line.
    Upto { chunk: String, resume: Option<usize> },
}

/// If `rest` begins with an exempt verb, decide how much of the line it takes. `None` if
/// `rest` doesn't start with an exempt verb (fall through to the normal scan). The verb
/// token runs to the first whitespace and carries any attached switches (`X/BG`); a
/// leading `[` in the argument selects the delimited (chainable) form.
fn exempt_split(rest: &str) -> Option<ExemptSplit> {
    let b = rest.as_bytes();
    let n = b.len();

    // The verb token runs to the first whitespace (it includes attached switches, e.g. `X/BG`).
    let mut tok_end = 0;
    while tok_end < n && !matches!(b[tok_end], b' ' | b'\t') {
        tok_end += 1;
    }
    let token = &rest[..tok_end];
    // Match an exempt verb alone, or one immediately followed by a switch sigil (`/`/`-`), so
    // `XISTS`/`XYZZY` don't match. Compare BYTES, never `token[..v.len()]`: the token may begin with
    // a multi-byte character (a pasted curly quote, say), and slicing a `&str` mid-character PANICS;
    // the verbs are ASCII, so a byte compare is exactly equivalent and can't panic.
    let matches_list = |list: &[&str]| {
        list.iter().any(|v| {
            token.eq_ignore_ascii_case(v)
                || (token.len() > v.len()
                    && token.as_bytes()[..v.len()].eq_ignore_ascii_case(v.as_bytes())
                    && matches!(token.as_bytes()[v.len()], b'/' | b'-'))
        })
    };
    // Shell verbs (`X`/`XY`/`XV`): STRIP the brackets (the shell wants the bare command) and swallow
    // the bare, no-bracket form whole. Code verbs (`RUNCODE`): KEEP the brackets (the handler runs the
    // inner raw) and let the bare form fall through to normal splitting.
    let (keep_brackets, bare_is_all) = if matches_list(EXEMPT_VERBS) {
        (false, true)
    } else if matches_list(CODE_EXEMPT_VERBS) {
        (true, false)
    } else {
        return None;
    };

    // Skip whitespace to the argument. No leading `[` → bare form.
    let mut j = tok_end;
    skip_ws(b, &mut j);
    if j >= n || b[j] != b'[' {
        return if bare_is_all { Some(ExemptSplit::All) } else { None };
    }

    // Bracketed form: scan to the matching `]` (balanced; `\[`/`\]` escape a literal bracket).
    let open = j;
    let mut depth = 0usize;
    let mut k = j;
    let mut close = None;
    while k < n {
        match b[k] {
            b'\\' if k + 1 < n && matches!(b[k + 1], b'[' | b']') => {
                k += 2;
                continue;
            }
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(k);
                    break;
                }
            }
            _ => {}
        }
        k += 1;
    }
    let Some(close) = close else {
        return Some(ExemptSplit::All); // unterminated `[` — swallow the rest
    };

    // After `]`: whitespace then `;` resumes chaining; EOL ends; anything else is trailing
    // content we don't understand, so swallow the whole line rather than drop it.
    let mut p = close + 1;
    skip_ws(b, &mut p);
    let resume = if p >= n {
        None
    } else if b[p] == b';' {
        Some(p + 1)
    } else {
        return Some(ExemptSplit::All);
    };

    let chunk = if keep_brackets {
        // Code verb: keep the outer brackets verbatim; the verb handler strips them and runs the
        // inner RAW (so its `$`-refs — and nested `[...]` array literals — resolve at run time).
        format!("{} {}", token, &rest[open..=close])
    } else {
        // Shell verb: emit `verb-token <inner>` with the outer brackets stripped and `\[`/`\]` unescaped.
        let inner = unescape_brackets(&rest[open + 1..close]);
        if inner.is_empty() { token.to_string() } else { format!("{token} {inner}") }
    };
    Some(ExemptSplit::Upto { chunk, resume })
}

/// Unescape only the bracket-delimiter escapes (`\[`, `\]`) inside an exempt verb's
/// `[...]` argument. Every other backslash sequence is left verbatim for the shell (so
/// Windows paths and UNC `\\server` shares round-trip untouched).
pub(crate) fn unescape_brackets(s: &str) -> String {
    let b = s.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut i = 0;
    while i < n {
        if b[i] == b'\\' && i + 1 < n && matches!(b[i + 1], b'[' | b']') {
            out.push(b[i + 1] as char);
            i += 2;
        } else {
            let cl = utf8_len(b[i]);
            out.push_str(&s[i..i + cl]);
            i += cl;
        }
    }
    out
}

/// How a search/change command divides the rest of the line.
enum SearchSplit {
    /// The whole remaining line is the search command (bare/unterminated pattern,
    /// or trailing non-`;` content): consume it all, no chaining.
    All,
    /// The search command occupies `..end` bytes (emitted verbatim). `resume` is the
    /// offset just past a separating `;` to continue splitting from, or `None` if the
    /// command ran to end-of-line (or a trailing comment).
    Upto { end: usize, resume: Option<usize> },
}

/// If `rest` begins with a search/change verb, scan past its self-delimiting pattern
/// (and the replacement, for `CH`/`CV`) and report where `;`-chaining may resume.
/// `None` if `rest` doesn't start with a search verb. Mirrors `vm::parse_pattern`:
/// a delimited pattern (`"x"`, `/x/`) closes at its delimiter; a bare alphanumeric
/// pattern, or a replace with no trailing delimiter, consumes the rest.
fn search_split(rest: &str) -> Option<SearchSplit> {
    let b = rest.as_bytes();
    let n = b.len();

    // 1. A search verb at the start, terminated by EOS / whitespace / a switch sigil
    //    (so `SET`, `CHANGE`, `SEEK` don't match). Length of the base verb token.
    let base_len = SEARCH_VERBS.iter().find_map(|v| {
        let l = v.len();
        // Compare BYTES, never `rest[..l]`: `rest` may begin with a multi-byte character, and
        // slicing a `&str` mid-character PANICS — a real crash report was a command starting with a
        // curly quote (`se |“|;BD;‘;…`). The verbs are ASCII, so a byte compare is equivalent.
        if n >= l && b[..l].eq_ignore_ascii_case(v.as_bytes()) {
            match b.get(l) {
                None => Some(l),
                Some(&c) if matches!(c, b' ' | b'\t' | b'/' | b'-') => Some(l),
                _ => None,
            }
        } else {
            None
        }
    })?;
    let upper = rest[..base_len].to_ascii_uppercase();
    let is_replace = upper.starts_with("CH") || upper.starts_with("CV");

    let mut i = base_len;
    // Attached flag switches (no space): `SE/W`, `CH-w`, `SE/WT`.
    while i < n && matches!(b[i], b'/' | b'-') {
        i += 1;
        while i < n && b[i].is_ascii_alphanumeric() {
            i += 1;
        }
    }
    skip_ws(b, &mut i);
    // Leading flag tokens before the pattern (`SE /W "x"`): a `/`- or `-`-token whose
    // chars are all flag letters. A token that isn't (e.g. `/foo/`) is the pattern.
    while i < n && matches!(b[i], b'/' | b'-') {
        let mut j = i + 1;
        while j < n && !matches!(b[j], b' ' | b'\t') {
            j += 1;
        }
        let tok = &b[i + 1..j];
        if tok.is_empty() || !tok.iter().all(|c| SEARCH_FLAG_LETTERS.contains(c)) {
            break; // not a flag group — this is the pattern delimiter
        }
        i = j;
        skip_ws(b, &mut i);
    }

    // 2. The pattern.
    if i >= n || b[i] == b'#' {
        // Empty pattern (bare `SE`, or `SE` then a comment): ends here, no chaining.
        return Some(SearchSplit::Upto { end: i, resume: None });
    }
    if b[i] == b';' {
        // Empty pattern then a separator (`SE; PR x`): repeat-search, then chain.
        return Some(SearchSplit::Upto { end: i, resume: Some(i + 1) });
    }
    let delim = rest[i..].chars().next().unwrap();
    if delim.is_alphanumeric() {
        return Some(SearchSplit::All); // bare pattern: consume the rest
    }

    // 3. Delimited: scan past the closing delimiter(s) — one for find, two for a
    //    replace (`/find/replace/`). `\<any>` escapes (an escaped delimiter or `\\`).
    let closers_needed = if is_replace { 2 } else { 1 };
    let dl = delim.len_utf8();
    let mut k = i + dl;
    let mut closed = 0;
    while k < n && closed < closers_needed {
        let c = rest[k..].chars().next().unwrap();
        if c == '\\' {
            k += c.len_utf8();
            if k < n {
                k += rest[k..].chars().next().unwrap().len_utf8();
            }
            continue;
        }
        k += c.len_utf8();
        if c == delim {
            closed += 1;
        }
    }
    if closed < closers_needed {
        return Some(SearchSplit::All); // unterminated pattern/replacement
    }

    // After the pattern: only a `;` (skipping whitespace) resumes chaining; EOL stops;
    // anything else is trailing content the search verb absorbs (consume the rest).
    let mut p = k;
    skip_ws(b, &mut p);
    if p >= n {
        Some(SearchSplit::Upto { end: k, resume: None })
    } else if b[p] == b';' {
        Some(SearchSplit::Upto { end: k, resume: Some(p + 1) })
    } else {
        Some(SearchSplit::All)
    }
}

fn skip_ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && matches!(b[*i], b' ' | b'\t') {
        *i += 1;
    }
}

/// Unescape the chain characters `split_commands` honors — **but only outside
/// string literals**. The splitter's escapes (`\;`→`;`, `\"`→`"`, `\#`→`#`,
/// `\\`→`\`) are for command-level concerns (separators, comments). *Inside* a
/// `"..."` region the escapes belong to the expression lexer / `INSERT`'s
/// literal-text handling, so they are passed through verbatim — otherwise a
/// string literal containing `\"` or `\\` (e.g. the desugaring of a backslash or
/// quote KBD cell, `insert "\\"`) would be corrupted before the lexer sees it. A
/// lone `\` not followed by an escapable char is left alone, so Windows paths
/// round-trip cleanly.
fn unescape_chain_chars(s: &str) -> String {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut out = String::with_capacity(n);
    let mut i = 0;
    let mut in_quote = false;
    while i < n {
        let b = bytes[i];
        if in_quote {
            // Pass `\X` through verbatim (don't even toggle on an escaped quote).
            if b == b'\\' && i + 1 < n {
                out.push('\\');
                let cl = utf8_len(bytes[i + 1]);
                out.push_str(&s[i + 1..i + 1 + cl]);
                i += 1 + cl;
                continue;
            }
            if b == b'"' {
                in_quote = false;
            }
            let cl = utf8_len(b);
            out.push_str(&s[i..i + cl]);
            i += cl;
        } else {
            if b == b'\\' && i + 1 < n && matches!(bytes[i + 1], b'\\' | b';' | b'"' | b'#') {
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if b == b'"' {
                in_quote = true;
            }
            let cl = utf8_len(b);
            out.push_str(&s[i..i + cl]);
            i += cl;
        }
    }
    out
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

    fn split(s: &str) -> Vec<String> {
        split_commands(s)
    }

    // ---- cases mirrored from QN's test_quicknotes.py (the contract) --------

    #[test]
    fn simple_chain() {
        assert_eq!(split("PR a; PR b"), vec!["PR a", "PR b"]);
    }

    #[test]
    fn escaped_semicolon_is_literal() {
        assert_eq!(split(r"PR foo\;bar"), vec!["PR foo;bar"]);
    }

    /// Regression: a chunk STARTING with a multi-byte character used to panic — the verb probes
    /// sliced `&str` at an ASCII verb's byte length, landing inside the character. Reported from the
    /// field as `se |“|;BD;‘;se |”|;BD;’;` (curly quotes), which killed the editor outright. Splitting
    /// must simply produce chunks; whether the verbs are valid is the VM's problem, not the splitter's.
    #[test]
    fn chunks_starting_with_a_multibyte_char_do_not_panic() {
        // The exact reported command.
        let out = split("se |“|;BD;‘;se |”|;BD;’;");
        assert!(out.iter().any(|c| c.contains('‘')), "curly-quote chunk survives: {out:?}");
        // A multi-byte lead-in on its own, and one that shadows a search/exempt verb prefix.
        assert_eq!(split("‘"), vec!["‘"]);
        assert_eq!(split("’; PR ok"), vec!["’", "PR ok"]);
        assert_eq!(split("“SE”; PR ok"), vec!["“SE”", "PR ok"]);
        assert_eq!(split("—X/BG; PR ok"), vec!["—X/BG", "PR ok"]);
    }

    #[test]
    fn semicolon_inside_quotes_is_not_a_separator() {
        assert_eq!(
            split(r#"SET x = "a;b"; PR x"#),
            vec![r#"SET x = "a;b""#, "PR x"]
        );
    }

    #[test]
    fn empty_chunks_are_dropped() {
        assert_eq!(split("PR a;  ;PR b"), vec!["PR a", "PR b"]);
    }

    #[test]
    fn exempt_verb_consumes_rest_verbatim() {
        assert_eq!(split("X foo;bar"), vec!["X foo;bar"]);
        assert_eq!(split("XY git log; echo hi"), vec!["XY git log; echo hi"]);
        // The bare verb form is also exempt.
        assert_eq!(split("X"), vec!["X"]);
    }

    #[test]
    fn clip_is_not_exempt_and_chains_via_a_quoted_arg() {
        // CLIP takes a quoted-string argument now (not exempt): a `;` inside the quotes is
        // protected; one after the closing quote separates commands.
        assert_eq!(split(r#"CLIP "a;b"; PR x"#), vec![r#"CLIP "a;b""#, "PR x"]);
        // A bare (unquoted) argument now splits at `;` like any other verb.
        assert_eq!(split("CLIP hi; PR x"), vec!["CLIP hi", "PR x"]);
    }

    #[test]
    fn x_prefix_does_not_falsely_match_other_verbs() {
        // "XISTS" must not be treated as the exempt verb "X".
        assert_eq!(split("XISTS a; PR b"), vec!["XISTS a", "PR b"]);
    }

    #[test]
    fn xlink_is_exempt_so_its_payload_keeps_semicolons() {
        // Bare form: the whole `;`-containing payload is XLINK's single argument.
        assert_eq!(split("XLINK OPEN x;GOTOLINE 5"), vec!["XLINK OPEN x;GOTOLINE 5"]);
        // The attached-switch form (`XLINK/L`) is still recognised as the exempt verb.
        assert_eq!(split("XLINK/L budget.txt:4200"), vec!["XLINK/L budget.txt:4200"]);
        // Bracketed form delimits the payload and lets a command chain after `]`.
        assert_eq!(split("XLINK [OPEN x;GOTOLINE 5]; TF"), vec!["XLINK OPEN x;GOTOLINE 5", "TF"]);
        // The longer XL* verbs must NOT match the exempt "XLINK" — they split normally.
        assert_eq!(split("XLINKHIGHLIGHT off; PR x"), vec!["XLINKHIGHLIGHT off", "PR x"]);
        assert_eq!(split("XLDEL; PR x"), vec!["XLDEL", "PR x"]);
    }

    // ---- exempt verbs: the bracket-delimited form re-enables `;`-chaining -----------

    #[test]
    fn exempt_bracket_form_chains_after_the_close_bracket() {
        // The outer brackets are stripped; `;`-splitting resumes past `]`.
        assert_eq!(split("X [foo;bar]; PR $RC"), vec!["X foo;bar", "PR $RC"]);
        assert_eq!(split("XY [git log; head]; PR done"), vec!["XY git log; head", "PR done"]);
    }

    #[test]
    fn exempt_bracket_strips_only_the_outer_pair_and_nests() {
        // A subshell passes through untouched — no doubling needed.
        assert_eq!(split("X [(cd a; make)]; PR $RC"), vec!["X (cd a; make)", "PR $RC"]);
        // Balanced inner brackets (glob / test) survive.
        assert_eq!(split("X [ls [a-z]*.txt]; PR $RC"), vec!["X ls [a-z]*.txt", "PR $RC"]);
    }

    #[test]
    fn exempt_bracket_runs_to_eol_without_a_trailing_semicolon() {
        assert_eq!(split("X [foo; bar]"), vec!["X foo; bar"]);
    }

    #[test]
    fn exempt_bracket_carries_attached_switches() {
        assert_eq!(split("X/BG [make; test]; PR $LJ"), vec!["X/BG make; test", "PR $LJ"]);
    }

    #[test]
    fn exempt_bracket_escapes_a_literal_bracket() {
        // `\]` is a literal `]`, so the group closes at the *next* real `]`.
        assert_eq!(split(r"X [echo \]]; PR x"), vec!["X echo ]", "PR x"]);
    }

    #[test]
    fn exempt_unterminated_bracket_swallows_the_rest() {
        // No matching `]`: fall back to swallowing the whole line, brackets intact.
        assert_eq!(split("X [foo; bar; PR x"), vec!["X [foo; bar; PR x"]);
    }

    #[test]
    fn exempt_trailing_junk_after_bracket_swallows_the_rest() {
        // Content after `]` that isn't `;`/EOL → swallow the whole line (nothing dropped).
        assert_eq!(split("X [foo] bar; baz"), vec!["X [foo] bar; baz"]);
    }

    // ---- code verbs (RUNCODE): brackets KEPT, bare form splits normally ----

    #[test]
    fn runcode_bracket_keeps_brackets_and_protects_inner_semicolons() {
        // The point of the feature: `;` inside the block is NOT a separator; chaining resumes after `]`.
        // Unlike X, the brackets are KEPT so the handler can run the inner raw.
        assert_eq!(
            split("RUNCODE [set a=1; pr $a]; PR done"),
            vec!["RUNCODE [set a=1; pr $a]", "PR done"]
        );
        // Runs to EOL without a trailing `;`.
        assert_eq!(split("RUNCODE [set a=1; pr $a]"), vec!["RUNCODE [set a=1; pr $a]"]);
        // Attached switch (`/BG`) is carried; nested array-literal brackets survive (balanced).
        assert_eq!(
            split("RUNCODE/BG [set a=[1,2,3]; pr $a]; PR $LJ"),
            vec!["RUNCODE/BG [set a=[1,2,3]; pr $a]", "PR $LJ"]
        );
    }

    #[test]
    fn runcode_bare_form_splits_like_any_verb() {
        // No leading `[`: the old behavior is unchanged — `;` still chains after `RUNCODE $var`.
        assert_eq!(split("RUNCODE $code; PR done"), vec!["RUNCODE $code", "PR done"]);
    }

    #[test]
    fn exempt_bare_form_without_a_bracket_still_swallows() {
        // Unchanged: a bare argument (no leading `[`) takes the rest verbatim.
        assert_eq!(split("X foo;bar"), vec!["X foo;bar"]);
        assert_eq!(split("X/BG make; test"), vec!["X/BG make; test"]);
    }

    // ---- search/change verbs: chaining resumes after the self-delimiting pattern --

    #[test]
    fn chain_resumes_after_a_delimited_search() {
        assert_eq!(split(r#"SE "foo"; PR done"#), vec![r#"SE "foo""#, "PR done"]);
        assert_eq!(split("SE /foo/; PR done"), vec!["SE /foo/", "PR done"]);
    }

    #[test]
    fn semicolon_inside_a_search_pattern_is_not_a_separator() {
        // The contract case: `;` inside the delimited pattern is the search target.
        assert_eq!(split("SE /;/; PR x"), vec!["SE /;/", "PR x"]);
        assert_eq!(split(r#"SE ";"; PR x"#), vec![r#"SE ";""#, "PR x"]);
        // No trailing command: the lone search still parses as one chunk.
        assert_eq!(split("SE /;/"), vec!["SE /;/"]);
    }

    #[test]
    fn escaped_delimiter_inside_pattern_does_not_close_it() {
        // `\/` is an escaped delimiter, so the pattern closes at the *next* real `/`.
        assert_eq!(split(r"SE /a\/b/; PR x"), vec![r"SE /a\/b/", "PR x"]);
    }

    #[test]
    fn change_verb_chains_after_the_trailing_delimiter() {
        assert_eq!(split("CH /a/b/; PR x"), vec!["CH /a/b/", "PR x"]);
        // No trailing delimiter → the replacement runs to EOL → consume the rest.
        assert_eq!(split("CH /a/b; PR x"), vec!["CH /a/b; PR x"]);
    }

    #[test]
    fn search_switches_are_skipped_to_find_the_delimiter() {
        assert_eq!(split(r#"SE/W "x"; PR y"#), vec![r#"SE/W "x""#, "PR y"]);
        assert_eq!(split(r#"SE /W "x"; PR y"#), vec![r#"SE /W "x""#, "PR y"]);
        assert_eq!(split(r#"CHA /a/b/; PR y"#), vec!["CHA /a/b/", "PR y"]);
    }

    #[test]
    fn bare_pattern_still_consumes_the_rest() {
        // A bare (undelimited) pattern can't chain — `;` is part of the search text.
        assert_eq!(split("SE foo; PR x"), vec!["SE foo; PR x"]);
    }

    #[test]
    fn empty_search_then_chain() {
        // Empty `SE` (repeat last search) followed by a separator chains.
        assert_eq!(split("SE; PR x"), vec!["SE", "PR x"]);
    }

    #[test]
    fn search_verb_prefix_is_not_falsely_matched() {
        // "SET"/"SEEK"/"CHANGE" must not be treated as search verbs.
        assert_eq!(split("SET x = 1; PR $x"), vec!["SET x = 1", "PR $x"]);
        assert_eq!(split("SEEK a; PR b"), vec!["SEEK a", "PR b"]);
    }

    #[test]
    fn two_searches_chain() {
        assert_eq!(
            split(r#"SE "a"; SEB "b"; PR c"#),
            vec![r#"SE "a""#, r#"SEB "b""#, "PR c"]
        );
    }

    // ---- additional contract: escapes, carve-out, comments -----------------

    #[test]
    fn backslash_and_quote_escapes() {
        assert_eq!(split(r"PR a\\b"), vec![r"PR a\b"]);
        assert_eq!(split(r#"PR \"hi\""#), vec![r#"PR "hi""#]);
    }

    #[test]
    fn lone_backslash_in_path_survives() {
        assert_eq!(split(r"CD C:\notes\file"), vec![r"CD C:\notes\file"]);
    }

    #[test]
    fn windows_path_drive_root_carve_out() {
        // `D:\` followed by `;` then space: the `\` is path, `;` separates.
        assert_eq!(split(r"CD D:\; DIR"), vec![r"CD D:\", "DIR"]);
    }

    #[test]
    fn inline_comment_runs_to_eol() {
        assert_eq!(split("PR a # this is a comment"), vec!["PR a"]);
        assert_eq!(split("PR a; PR b # tail"), vec!["PR a", "PR b"]);
    }

    #[test]
    fn hash_without_whitespace_context_is_literal() {
        // `#4` and `#tag` are not comments (no required ws context).
        assert_eq!(split("PR Issue #4"), vec!["PR Issue #4"]);
        assert_eq!(split(r"PR a\#b"), vec!["PR a#b"]);
    }

    #[test]
    fn runs_of_hashes_are_comments() {
        // Two or more `#` start a comment (with or without a following space) — a header/emphasis
        // comment, which a single `#` (needs trailing ws) wouldn't otherwise allow.
        assert!(split("## a header comment").is_empty());
        assert!(split("### section").is_empty());
        assert!(split("##nospace").is_empty());
        assert_eq!(split("PR a ## inline"), vec!["PR a"]);
        // A single `#` hugging text stays literal (`#4` unchanged).
        assert_eq!(split("PR #4"), vec!["PR #4"]);
    }

    #[test]
    fn utf8_content_passes_through() {
        assert_eq!(split("PR café; PR naïve"), vec!["PR café", "PR naïve"]);
    }

    #[test]
    fn escapes_inside_quotes_are_preserved_for_the_lexer() {
        // A string literal's escapes must survive the splitter intact (otherwise
        // `insert "\\"` / `"a\"b"` would be corrupted before the lexer).
        assert_eq!(split(r#"insert "\\""#), vec![r#"insert "\\""#]);
        assert_eq!(split(r#"SET x = "a\"b""#), vec![r#"SET x = "a\"b""#]);
        // But OUTSIDE quotes, chain escapes still unescape.
        assert_eq!(split(r"PR a\\b"), vec![r"PR a\b"]);
    }
}
