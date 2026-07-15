# Regular expressions in XyScript

XyScript's regular expressions are powered by Rust's [`regex`](https://docs.rs/regex/1) crate
(version 1.x). It is a **linear-time, RE2-style** engine: it never backtracks catastrophically, so
patterns are fast and safe even on hostile input — but, as a deliberate trade-off, it does **not**
support backreferences or look-around (see [Not supported](#not-supported)).

## Where regex is used

| Context | How regex is selected | Default case |
|---|---|---|
| **Search** — `SE`/`SEB`/`SEA`/… | the `/R` switch (`SE/R <pattern>`) | **insensitive** (`/A` = case-sensitive) |
| **Search-and-replace** — `CH`/`CHA` (= `CI`/`CIA`) | the `/R` switch; the *replacement* may use capture refs | **insensitive** (`/A` = case-sensitive) |
| **Syntax-highlight grammars** — `HLRULE <kind> <regex>` | the rule's pattern is always a regex | **sensitive** (use `(?i)` to fold) |

Two switches refine a search regex:

- **`/A`** — case-sensitive (regex is otherwise matched case-insensitively).
- **`/W`** — whole word: the pattern is wrapped as `\b(?:…)\b`.

`HLRULE` patterns take no switches — fold case with an inline `(?i)` flag instead, and remember they
match **one line at a time** (so `^` and `$` are the start/end of each line).

## Supported syntax

### Characters & escapes
| Pattern | Matches |
|---|---|
| `a`, `%`, ` ` | the literal character |
| `.` | any character **except newline** (see `(?s)` to include it) |
| `\n` `\r` `\t` | newline, carriage return, tab |
| `\.` `\*` `\(` `\\` … | an escaped metacharacter, literally |
| `\x7F`, `\x{1F600}` | a character by hex code point |

### Character classes
| Pattern | Matches |
|---|---|
| `[abc]` | any one of `a`, `b`, `c` |
| `[^abc]` | any character *except* those |
| `[a-z0-9]` | a range |
| `\d` `\D` | a digit / non-digit |
| `\w` `\W` | a word char `[0-9A-Za-z_]` / non-word |
| `\s` `\S` | whitespace / non-whitespace |
| `[[:alpha:]]` `[[:digit:]]` … | POSIX classes |
| `\p{L}` `\p{Lu}` `\P{N}` | Unicode property classes (Unicode is on by default) |

### Anchors & boundaries
| Pattern | Matches |
|---|---|
| `^` `$` | start / end of text — or of each **line** under `(?m)` (and in `HLRULE`, always per line) |
| `\b` `\B` | a word boundary / non-boundary |
| `\A` `\z` | start / very end of the text |

### Repetition
| Pattern | Repeats the preceding item |
|---|---|
| `*` `+` `?` | zero+, one+, optional (**greedy**) |
| `*?` `+?` `??` | the same, **lazy** (as few as possible) |
| `{3}` `{2,}` `{2,5}` | exactly / at-least / between *n* and *m* times |
| `{2,5}?` | bounded, lazy |

### Grouping & alternation
| Pattern | Meaning |
|---|---|
| `(…)` | a **capturing** group (numbered left-to-right: `$1`, `$2`, …) |
| `(?:…)` | a **non-capturing** group |
| `(?<name>…)` or `(?P<name>…)` | a **named** capturing group |
| `a|b` | `a` **or** `b` |

### Inline flags
Set flags for the rest of the pattern with `(?flags)`, or for one group with `(?flags:…)`:

| Flag | Effect |
|---|---|
| `i` | case-insensitive |
| `m` | multi-line: `^`/`$` match at line breaks |
| `s` | dot matches newline too |
| `x` | verbose: ignore whitespace in the pattern, allow `#` comments |
| `u` | Unicode mode (**on by default**; `-u` for byte/ASCII semantics) |

Example: `(?i)\bfn\b` matches `fn`, `Fn`, `FN`. `(?is)…` combines flags.

## Replacement (search-and-replace)

In a `/R` change, the replacement string may reference captures from the match:

| In the replacement | Inserts |
|---|---|
| `$0` | the whole match |
| `$1`, `$2`, … | the *n*-th capturing group |
| `${name}` | a named group (use the braces to delimit it from following text) |
| `$$` | a literal `$` |

Bare `$1abc` reads the group named `1abc`; write `${1}abc` when a digit/name is followed by more
word characters. (For a **literal**, non-`/R` replace, the replacement text is inserted verbatim — no
`$` expansion.)

## Not supported

The linear-time engine intentionally omits features that require backtracking:

- **Backreferences** in the *pattern* — e.g. `(\w)\1`. (Capture refs in the *replacement*, like `$1`,
  are fine — that's a different mechanism.)
- **Look-around** — lookahead `(?=…)`/`(?!…)` and lookbehind `(?<=…)`/`(?<!…)`.
- **Atomic groups / possessive quantifiers** — `(?>…)`, `a++`.

If a pattern needs one of these, restructure it (often a capturing group + a replacement, or splitting
into two commands, does the job).

## XyScript-specific notes

- **`HLRULE` quoting.** Wrap a grammar regex in `"…"` if it contains spaces. The command splitter
  unescapes only `\\`, `\"`, `\#`, and `\;` before the regex is compiled — every other backslash
  escape (`\b`, `\d`, `\s`, `\w`, `\.`, `\$`, …) passes through untouched. So **avoid a literal `\\`**
  in an `HLRULE` pattern (it collapses to a single `\`); `"\"[^\"]*\""` is the idiom for matching a
  `"…"` string (it becomes `"[^"]*"` after unescaping). See [grammars/](../grammars/) for worked
  examples.
- **`HLRULE` is per line.** Each source line is matched independently, so `^`/`$` are the line edges
  and no pattern can span a line break (no multi-line block comments or triple-quoted strings).
- **Case.** Search regex is case-insensitive unless you add `/A`; `HLRULE` is case-sensitive — fold it
  with an inline `(?i)`.

## See also

- [overview.md](overview.md) — the full command + variable reference (`SE`, `CH`, `HLLANG`, `HLRULE`).
- [grammars/](../grammars/) — the bundled syntax grammars (`rust`, `python`, `xpl`, …) as examples.
