//! Per-document counters `C1`..`C9` (XyWrite-style numbered registers).
//!
//! Each counter holds a *kind* (number / letter / Roman numeral), a *case* (for letters and Roman
//! numerals), a *decorator* (literal text on either side of the value), and the *last value* it
//! emitted. The three invocation shapes:
//!
//! - **`Cn <seed>`** configures the counter and emits nothing. The seed gives the type + start value;
//!   any surrounding text is the decorator (quote to keep leading/trailing spaces): `C1 "1. "`,
//!   `C2 "(A) "`, `C3 ivr`, `C4 Ir`. Roman numerals carry a trailing `r`/`R` marker so they don't
//!   collide with letter counters (`ir`→i, `Ir`→I, `ivr`→iv); the marker is dropped from output.
//! - **bare `Cn`** emits the current value wrapped in the decorator, then advances by one.
//! - **`Cn +n` / `Cn -[n]` / `Cn =`** emit relative to the last value: `+n` n greater, `-[n]` n less
//!   (`-` ≡ `-1`), `=` the same value again (no advance).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Number,
    Letter,
    Roman,
}

/// One `Cn` register. `last` is the most recently emitted value; a bare put emits `last + 1`, so a
/// freshly configured counter stores `start - 1` and the first bare put yields `start`.
#[derive(Debug, Clone)]
pub struct Counter {
    kind: Kind,
    upper: bool,
    last: i64,
    prefix: String,
    suffix: String,
}

impl Default for Counter {
    /// An unconfigured counter behaves as a plain decimal counter: a bare put emits 1, 2, 3, … with
    /// no decorator.
    fn default() -> Self {
        Counter { kind: Kind::Number, upper: false, last: 0, prefix: String::new(), suffix: String::new() }
    }
}

impl Counter {
    /// Run one `Cn` invocation with the (already `$`-substituted) argument. Returns `Ok(Some(text))`
    /// for an emit (insert it at the caret), `Ok(None)` for a configure-only `Cn <seed>`, or
    /// `Err(msg)` for a malformed argument.
    pub fn run(&mut self, arg: &str) -> Result<Option<String>, String> {
        let t = arg.trim();
        // Emit forms.
        if t.is_empty() {
            return Ok(Some(self.emit(self.last + 1)));
        }
        if t == "=" {
            return Ok(Some(self.emit(self.last)));
        }
        if let Some(rest) = t.strip_prefix('+') {
            let n = parse_step(rest).ok_or_else(|| format!("C: expected +<number>, got '{t}'"))?;
            return Ok(Some(self.emit(self.last + n)));
        }
        if let Some(rest) = t.strip_prefix('-') {
            // `-` ≡ `-1`; `-n` steps back by n.
            let n = parse_step(rest).ok_or_else(|| format!("C: expected -<number>, got '{t}'"))?;
            return Ok(Some(self.emit(self.last - n)));
        }
        // Otherwise: (re)configure from a seed, optionally wrapped in decorator text.
        let decorator = unquote(arg);
        let seed = parse_seed(&decorator)?;
        self.kind = seed.kind;
        self.upper = seed.upper;
        self.prefix = seed.prefix;
        self.suffix = seed.suffix;
        self.last = seed.start - 1; // first bare put emits `start`
        Ok(None)
    }

    /// The rendered current value (`last + 1`, what a bare put would emit) WITHOUT the decorator —
    /// what `$Cn` reads. Does not advance the counter.
    pub fn peek(&self) -> String {
        render(self.kind, self.upper, self.last + 1)
    }

    /// Record `value` as the last emitted and render it with the decorator.
    fn emit(&mut self, value: i64) -> String {
        self.last = value;
        format!("{}{}{}", self.prefix, render(self.kind, self.upper, value), self.suffix)
    }
}

/// Parse the step count after `+`/`-`: empty ⇒ 1, else a non-negative integer.
fn parse_step(rest: &str) -> Option<i64> {
    if rest.is_empty() {
        Some(1)
    } else {
        rest.parse::<i64>().ok().filter(|&n| n >= 0)
    }
}

/// Strip one layer of surrounding double quotes (processing `\n`/`\t`/`\"`/`\\`) so a decorator can
/// keep leading/trailing spaces (`C1 "1. "`). Unquoted arguments are returned trimmed.
fn unquote(arg: &str) -> String {
    let t = arg.trim();
    let inner = match t.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        Some(i) if t.len() >= 2 => i,
        _ => return t.to_string(),
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

struct Seed {
    prefix: String,
    suffix: String,
    kind: Kind,
    upper: bool,
    start: i64,
}

/// Locate the single counter token inside a decorator and split off the surrounding prefix/suffix.
/// Scanning left to right, the first match wins, trying Roman (numerals + `r` marker) → number →
/// isolated single letter. Errors if no token is found.
fn parse_seed(s: &str) -> Result<Seed, String> {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    for i in 0..n {
        // Roman numerals followed by the `r`/`R` type marker, isolated from surrounding alphanumerics.
        if is_roman_char(chars[i]) {
            let mut j = i;
            while j < n && is_roman_char(chars[j]) {
                j += 1;
            }
            if j < n && (chars[j] == 'r' || chars[j] == 'R') {
                let after = j + 1;
                let prev_ok = i == 0 || !chars[i - 1].is_ascii_alphanumeric();
                let after_ok = after >= n || !chars[after].is_ascii_alphanumeric();
                if prev_ok && after_ok {
                    let numerals: String = chars[i..j].iter().collect();
                    if let Some(start) = roman_to_int(&numerals) {
                        return Ok(Seed {
                            prefix: chars[..i].iter().collect(),
                            suffix: chars[after..].iter().collect(),
                            kind: Kind::Roman,
                            upper: chars[i].is_ascii_uppercase(),
                            start,
                        });
                    }
                }
            }
        }
        // A run of digits.
        if chars[i].is_ascii_digit() && (i == 0 || !chars[i - 1].is_ascii_digit()) {
            let mut j = i;
            while j < n && chars[j].is_ascii_digit() {
                j += 1;
            }
            let num: String = chars[i..j].iter().collect();
            if let Ok(start) = num.parse::<i64>() {
                return Ok(Seed {
                    prefix: chars[..i].iter().collect(),
                    suffix: chars[j..].iter().collect(),
                    kind: Kind::Number,
                    upper: false,
                    start,
                });
            }
        }
        // A single letter not adjacent to other letters (so a word like "Section" isn't a seed).
        if chars[i].is_ascii_alphabetic() {
            let prev_ok = i == 0 || !chars[i - 1].is_ascii_alphabetic();
            let next_ok = i + 1 >= n || !chars[i + 1].is_ascii_alphabetic();
            if prev_ok && next_ok {
                let c = chars[i];
                return Ok(Seed {
                    prefix: chars[..i].iter().collect(),
                    suffix: chars[i + 1..].iter().collect(),
                    kind: Kind::Letter,
                    upper: c.is_ascii_uppercase(),
                    start: (c.to_ascii_lowercase() as i64) - ('a' as i64) + 1,
                });
            }
        }
    }
    Err(format!("C: no counter seed (number, letter, or Roman numeral + 'r') in '{s}'"))
}

fn is_roman_char(c: char) -> bool {
    matches!(c, 'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm' | 'I' | 'V' | 'X' | 'L' | 'C' | 'D' | 'M')
}

/// Render `value` in the counter's kind/case. Out-of-range values for letters (`< 1`) and Roman
/// numerals (`< 1` or `> 3999`) fall back to a decimal so nothing is silently lost.
fn render(kind: Kind, upper: bool, value: i64) -> String {
    match kind {
        Kind::Number => value.to_string(),
        Kind::Letter => {
            if value < 1 {
                value.to_string()
            } else {
                let s = int_to_letters(value);
                if upper {
                    s.to_ascii_uppercase()
                } else {
                    s
                }
            }
        }
        Kind::Roman => match int_to_roman(value) {
            Some(s) if upper => s,
            Some(s) => s.to_ascii_lowercase(),
            None => value.to_string(),
        },
    }
}

/// Bijective base-26: 1→a, 26→z, 27→aa, 28→ab, … (spreadsheet-column style). `value` must be ≥ 1.
fn int_to_letters(mut value: i64) -> String {
    let mut buf = Vec::new();
    while value > 0 {
        value -= 1;
        buf.push((b'a' + (value % 26) as u8) as char);
        value /= 26;
    }
    buf.iter().rev().collect()
}

/// Uppercase Roman numeral for 1..=3999; `None` outside that range.
fn int_to_roman(mut value: i64) -> Option<String> {
    if !(1..=3999).contains(&value) {
        return None;
    }
    const TABLE: &[(i64, &str)] = &[
        (1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"), (50, "L"),
        (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I"),
    ];
    let mut out = String::new();
    for &(v, sym) in TABLE {
        while value >= v {
            out.push_str(sym);
            value -= v;
        }
    }
    Some(out)
}

/// Parse a Roman numeral (case-insensitive) to its value; `None` if it isn't a well-formed numeral
/// (we round-trip through [`int_to_roman`] to reject malformed input like "iix").
fn roman_to_int(s: &str) -> Option<i64> {
    fn digit(c: char) -> Option<i64> {
        Some(match c.to_ascii_lowercase() {
            'i' => 1,
            'v' => 5,
            'x' => 10,
            'l' => 50,
            'c' => 100,
            'd' => 500,
            'm' => 1000,
            _ => return None,
        })
    }
    if s.is_empty() {
        return None;
    }
    let vals: Vec<i64> = s.chars().map(digit).collect::<Option<_>>()?;
    let mut total = 0;
    for i in 0..vals.len() {
        if i + 1 < vals.len() && vals[i] < vals[i + 1] {
            total -= vals[i];
        } else {
            total += vals[i];
        }
    }
    // Canonical-form check: only accept input that re-renders to itself.
    if int_to_roman(total).as_deref() == Some(s.to_ascii_uppercase().as_str()) {
        Some(total)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emit(c: &mut Counter, arg: &str) -> String {
        c.run(arg).unwrap().unwrap()
    }

    #[test]
    fn numeric_set_then_bare_puts_and_advances() {
        let mut c = Counter::default();
        assert_eq!(c.run("\"1. \"").unwrap(), None); // set, no output
        assert_eq!(emit(&mut c, ""), "1. ");
        assert_eq!(emit(&mut c, ""), "2. ");
        assert_eq!(emit(&mut c, ""), "3. ");
    }

    #[test]
    fn letters_and_case_bijective_overflow() {
        let mut c = Counter::default();
        c.run("\"(A) \"").unwrap();
        assert_eq!(emit(&mut c, ""), "(A) ");
        assert_eq!(emit(&mut c, ""), "(B) ");
        // Start near the end of the alphabet to reach the aa overflow quickly.
        let mut z = Counter::default();
        z.run("z").unwrap();
        assert_eq!(emit(&mut z, ""), "z");
        assert_eq!(emit(&mut z, ""), "aa");
        // Uppercase seed → uppercase output.
        let mut u = Counter::default();
        u.run("Y").unwrap();
        assert_eq!(emit(&mut u, ""), "Y");
        assert_eq!(emit(&mut u, ""), "Z");
        assert_eq!(emit(&mut u, ""), "AA");
    }

    #[test]
    fn roman_marker_sets_kind_and_case() {
        let mut lower = Counter::default();
        lower.run("ivr").unwrap(); // start at iv = 4, lowercase
        assert_eq!(emit(&mut lower, ""), "iv");
        assert_eq!(emit(&mut lower, ""), "v");
        let mut upper = Counter::default();
        upper.run("\"Ir) \"").unwrap(); // start at I = 1, uppercase, decorator ") "
        assert_eq!(emit(&mut upper, ""), "I) ");
        assert_eq!(emit(&mut upper, ""), "II) ");
    }

    #[test]
    fn relative_steps_plus_minus_same() {
        let mut c = Counter::default();
        c.run("1").unwrap();
        assert_eq!(emit(&mut c, ""), "1"); // last = 1
        assert_eq!(emit(&mut c, "+2"), "3"); // 1 + 2
        assert_eq!(emit(&mut c, "="), "3"); // same as last
        assert_eq!(emit(&mut c, "-"), "2"); // last - 1
        assert_eq!(emit(&mut c, "-2"), "0"); // last - 2
        assert_eq!(emit(&mut c, ""), "1"); // bare = last + 1
    }

    #[test]
    fn peek_does_not_advance() {
        let mut c = Counter::default();
        c.run("5").unwrap();
        assert_eq!(c.peek(), "5");
        assert_eq!(c.peek(), "5"); // still 5 — no advance
        assert_eq!(emit(&mut c, ""), "5");
        assert_eq!(c.peek(), "6");
    }

    #[test]
    fn single_letter_seed_in_a_word_is_not_mistaken() {
        let mut c = Counter::default();
        // "Section A: " → prefix "Section ", seed A, suffix ": " (the word "Section" is not a seed).
        c.run("\"Section A: \"").unwrap();
        assert_eq!(emit(&mut c, ""), "Section A: ");
        assert_eq!(emit(&mut c, ""), "Section B: ");
    }

    #[test]
    fn no_seed_is_an_error() {
        let mut c = Counter::default();
        assert!(c.run("\"-- \"").is_err());
        assert!(c.run("hello").is_err());
    }
}
