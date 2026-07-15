//! Recursive-descent statement parser (`stage1-interpreter-plan.md` §6, §13.5).
//!
//! Turns source text into a structured [`Program`]. Pipeline: the source is
//! split into physical lines, each line is run through the command
//! [`splitter`](crate::splitter), and the resulting flat command list is parsed
//! into nested [`Stmt`] nodes.
//!
//! Because blocks are structural, the `;`-chain footgun is gone: a
//! `;`-chained `IF cond ; body ; ENDIF` flattens to three commands that pair up
//! into one [`Stmt::If`] exactly like the multi-line form. **Single-line**
//! `IF cond cmd` is detected by greedily parsing the condition
//! ([`expr::parse_prefix`](crate::expr::parse_prefix)) and checking whether any
//! trailing command text remains — no rollback hack (§7).
//!
//! Structural errors (`ENDIF` with no `IF`, an unclosed `SUB`, `ELSE` with a
//! stray argument) are reported here, at parse time, with a line number.

use crate::ast::{CondArm, ForKind, Program, Span, Stmt};
use crate::expr::{self, BinOp, Expr};
use crate::splitter::split_commands_indexed;
use crate::value::fold_ident;

/// A parse error with the offending source line (1-based).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub msg: String,
    pub line: usize,
}

impl ParseError {
    fn new(msg: impl Into<String>, line: usize) -> ParseError {
        ParseError { msg: msg.into(), line }
    }
}

/// One flattened command with its source line and 1-based column.
#[derive(Debug, Clone)]
struct RawCmd {
    text: String,
    line: usize,
    col: usize,
}

/// Parse a whole script (or a single line) into a [`Program`].
pub fn parse(source: &str) -> Result<Program, ParseError> {
    let cmds = flatten(source);
    let mut p = Parser { cmds, pos: 0 };
    let (stmts, term) = p.parse_block()?;
    // At the top level, the only legal terminator is end-of-input.
    match term {
        Term::End => Ok(Program { stmts }),
        Term::EndIf => Err(ParseError::new("ENDIF without matching IF", p.last_line())),
        Term::Else => Err(ParseError::new("ELSE without matching IF", p.last_line())),
        Term::ElseIf(_) => {
            Err(ParseError::new("ELSEIF without matching IF", p.last_line()))
        }
        Term::EndWhile => {
            Err(ParseError::new("ENDWHILE without matching WHILE", p.last_line()))
        }
        Term::EndFor => Err(ParseError::new("ENDFOR without matching FOR", p.last_line())),
        Term::EndSub => Err(ParseError::new("ENDSUB without matching SUB", p.last_line())),
        Term::EndCapture => {
            Err(ParseError::new("ENDCAPTURE without matching CAPTURE", p.last_line()))
        }
        Term::Case(_) => Err(ParseError::new("CASE without matching SWITCH", p.last_line())),
        Term::Default => Err(ParseError::new("DEFAULT without matching SWITCH", p.last_line())),
        Term::EndSwitch => {
            Err(ParseError::new("ENDSWITCH without matching SWITCH", p.last_line()))
        }
    }
}

/// Split source into logical lines (joining line-continuation `\`), each into `;`-separated commands.
fn flatten(source: &str) -> Vec<RawCmd> {
    // Tolerate a leading UTF-8 BOM. Windows editors (Notepad, etc.) add one when saving a script
    // file; without this the first command's verb would be `\u{feff}NAME` — an unknown command that
    // fatally aborts the whole script (the classic "my startup file's first line silently fails, but
    // works when I type it" bug). A BOM only ever appears at the very start, so strip just the prefix.
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    // Strip ALL carriage returns first — `\r` is never meaningful in XPL source. This tolerates
    // CRLF, lone/old-Mac CR, and MANGLED files with stray mid-line `\r` (e.g. `msg\r $a`, which
    // would otherwise parse the verb as `MSG\r`). Newlines are `\n`-only after this.
    let source: String = source.chars().filter(|&c| c != '\r').collect();
    let lines: Vec<&str> = source.split('\n').collect();
    let mut out = Vec::new();
    let mut idx = 0;
    while idx < lines.len() {
        // A continued logical line is numbered (and column-anchored) from its FIRST physical line.
        let line_no = idx + 1;
        // Join physical lines linked by a trailing line-continuation backslash.
        let mut logical = String::new();
        loop {
            match continuation(lines[idx]) {
                Some(head) => {
                    logical.push_str(head);
                    idx += 1;
                    if idx >= lines.len() {
                        break; // a dangling `\` at end of source: drop it, nothing to join
                    }
                }
                None => {
                    logical.push_str(lines[idx]);
                    idx += 1;
                    break;
                }
            }
        }
        for (off, cmd) in split_commands_indexed(&logical) {
            out.push(RawCmd { text: cmd, line: line_no, col: off + 1 }); // 1-based column
        }
    }
    out
}

/// If `line` ends with a line-continuation backslash, return it WITHOUT that backslash (the caller
/// then appends the next physical line); otherwise `None`.
///
/// A trailing `\` is a continuation marker only when **(a)** it is preceded by whitespace (or starts
/// the line) and **(b)** the run of trailing backslashes is **odd**. Rule (a) keeps Windows path
/// tails literal — a `\` hugging non-space content (`CD C:\`) belongs to the path, not a continuation
/// — and rule (b) preserves `\\` as an escaped literal backslash (handled later by the splitter). So
/// you continue a line by ending it with *a space and a backslash*; `… \\` stays a literal backslash.
fn continuation(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    if bytes.last() != Some(&b'\\') {
        return None;
    }
    let run = bytes.iter().rev().take_while(|&&b| b == b'\\').count();
    if run % 2 == 0 {
        return None; // even run = escaped literal backslash(es), not a continuation
    }
    // The byte just before the run of trailing backslashes (None = the line is all backslashes).
    let before = if run >= bytes.len() { None } else { Some(bytes[bytes.len() - run - 1]) };
    match before {
        None | Some(b' ') | Some(b'\t') => Some(&line[..line.len() - 1]),
        Some(_) => None, // backslash hugs non-whitespace (likely a path) — keep it literal
    }
}

/// What a [`Parser::parse_block`] call stopped on.
enum Term {
    /// End of input.
    End,
    /// `ENDIF` / `EI`.
    EndIf,
    /// `ELSE`.
    Else,
    /// `ELSEIF` / `ELSE IF` (carrying its parsed condition).
    ElseIf(Expr),
    /// `ENDWHILE` / `EW`.
    EndWhile,
    /// `ENDFOR` / `EF`.
    EndFor,
    /// `ENDSUB` / `ES`.
    EndSub,
    /// `ENDCAPTURE` / `EC`.
    EndCapture,
    /// `CASE <values>` (carrying its parsed value list).
    Case(Vec<Expr>),
    /// `DEFAULT`.
    Default,
    /// `ENDSWITCH` / `ESW`.
    EndSwitch,
}

struct Parser {
    cmds: Vec<RawCmd>,
    pos: usize,
}

impl Parser {
    fn last_line(&self) -> usize {
        self.cmds
            .get(self.pos.min(self.cmds.len().saturating_sub(1)))
            .map(|c| c.line)
            .unwrap_or(0)
    }

    /// Parse statements until a block terminator or end-of-input. Returns the
    /// collected statements and the terminator that stopped the block (consumed).
    fn parse_block(&mut self) -> Result<(Vec<Stmt>, Term), ParseError> {
        let mut stmts = Vec::new();
        loop {
            let (text, line, col) = match self.cmds.get(self.pos) {
                Some(c) => (c.text.clone(), c.line, c.col),
                None => return Ok((stmts, Term::End)),
            };

            // A `:label` is its own statement.
            if text.trim_start().starts_with(':') {
                self.pos += 1;
                let name = fold_ident(text.trim_start()[1..].trim());
                if name.is_empty() {
                    return Err(ParseError::new("empty label", line));
                }
                stmts.push(Stmt::Label { name, span: Span::new(line) });
                continue;
            }

            // A bare quoted-string command (`"x"`) inserts its text — handle before `split_verb`,
            // which would otherwise mangle a literal containing spaces (`" "`).
            if let Some(stmt) = quoted_literal_as_insert(&text, line) {
                self.pos += 1;
                stmts.push(stmt);
                continue;
            }

            // A bare drive-letter path (`D:`, `X:\dir`, `X:rel`) is a drive-change command — rewrite
            // it to `CD <path>`. Handled before `split_verb` so the path keeps its original case (the
            // fold would upper-case it) and any spaces. No XPL verb begins with `<letter>:`, so this
            // is unambiguous; it's XyWrite muscle memory (type `D:` to switch drives).
            if let Some(stmt) = drive_letter_as_cd(&text, line) {
                self.pos += 1;
                stmts.push(stmt);
                continue;
            }

            let (verb, rest) = split_verb(&text);
            match verb.as_str() {
                "ENDIF" | "EI" => {
                    self.pos += 1;
                    return Ok((stmts, Term::EndIf));
                }
                "ENDWHILE" | "EW" => {
                    self.pos += 1;
                    return Ok((stmts, Term::EndWhile));
                }
                "ENDFOR" | "EF" => {
                    self.pos += 1;
                    return Ok((stmts, Term::EndFor));
                }
                "ENDSUB" | "ES" => {
                    self.pos += 1;
                    return Ok((stmts, Term::EndSub));
                }
                "ENDCAPTURE" | "EC" => {
                    self.pos += 1;
                    return Ok((stmts, Term::EndCapture));
                }
                "ENDSWITCH" | "ESW" => {
                    self.pos += 1;
                    return Ok((stmts, Term::EndSwitch));
                }
                "CASE" => {
                    self.pos += 1;
                    let values = parse_case_values(rest, line)?;
                    return Ok((stmts, Term::Case(values)));
                }
                "DEFAULT" => {
                    self.pos += 1;
                    if !rest.trim().is_empty() {
                        return Err(ParseError::new("DEFAULT does not take an argument", line));
                    }
                    return Ok((stmts, Term::Default));
                }
                "ELSE" => {
                    let r = rest.trim();
                    self.pos += 1;
                    if r.is_empty() {
                        return Ok((stmts, Term::Else));
                    }
                    // `ELSE IF …` is an `ELSEIF`. Any other trailing text is an error.
                    let (w2, rest2) = split_verb(r);
                    if w2 == "IF" {
                        let cond = parse_full_cond(rest2, line)?;
                        return Ok((stmts, Term::ElseIf(cond)));
                    }
                    return Err(ParseError::new(
                        format!("ELSE does not take an argument: {r:?}"),
                        line,
                    ));
                }
                "ELSEIF" => {
                    self.pos += 1;
                    let cond = parse_full_cond(rest, line)?;
                    return Ok((stmts, Term::ElseIf(cond)));
                }
                _ => {
                    let stmt = self.parse_one(&verb, rest, line, col)?;
                    stmts.push(stmt);
                }
            }
        }
    }

    /// Parse the command at `self.pos` (already split into `verb`/`rest`),
    /// consuming it and — for block openers — its body. Advances `self.pos`.
    fn parse_one(&mut self, verb: &str, rest: &str, line: usize, col: usize) -> Result<Stmt, ParseError> {
        // `FOR` carries a `/X` switch (exclusive upper bound), so peel it before the plain match.
        let (base, switches) = crate::switches::peel_verb(verb);
        if base == "FOR" {
            self.pos += 1;
            let exclusive = switches.iter().any(|s| s.eq_ignore_ascii_case("X"));
            return self.parse_for(rest, exclusive, line);
        }
        // Block openers need the command stream; advance past the opener first.
        match verb {
            "IF" => {
                self.pos += 1;
                self.parse_if(rest, line, false)
            }
            "IFNOT" => {
                self.pos += 1;
                self.parse_if(rest, line, true)
            }
            "WHILE" => {
                self.pos += 1;
                self.parse_while(rest, line)
            }
            "SWITCH" => {
                self.pos += 1;
                self.parse_switch(rest, line)
            }
            "SUB" => {
                self.pos += 1;
                self.parse_sub(rest, line)
            }
            "CAPTURE" => {
                self.pos += 1;
                self.parse_capture(rest, line)
            }
            _ => {
                // Simple (non-block) statement; consume just this command.
                self.pos += 1;
                parse_simple(verb, rest, line, col)
            }
        }
    }

    /// Parse an `IF`/`IFNOT` whose opener has already been consumed.
    fn parse_if(&mut self, rest: &str, line: usize, negated: bool) -> Result<Stmt, ParseError> {
        let (cond, consumed) = expr::parse_prefix(rest)
            .map_err(|e| ParseError::new(format!("IF condition: {}", e.msg), line))?;
        let cond = maybe_negate(cond, negated);
        let trailing = rest[consumed..].trim();

        // Single-line form: `IF cond cmd` — body is the one inline command.
        if !trailing.is_empty() {
            let body_stmt = parse_inline(trailing, line)?;
            return Ok(Stmt::If {
                arms: vec![CondArm { cond, body: vec![body_stmt] }],
                els: None,
                span: Span::new(line),
            });
        }

        // Block form: collect the body and any ELSEIF/ELSE arms.
        let (body, mut term) = self.parse_block()?;
        let mut arms = vec![CondArm { cond, body }];
        let mut els = None;
        loop {
            match term {
                Term::EndIf => break,
                Term::ElseIf(c) => {
                    let (eb, t2) = self.parse_block()?;
                    arms.push(CondArm { cond: c, body: eb });
                    term = t2;
                }
                Term::Else => {
                    let (eb, t2) = self.parse_block()?;
                    match t2 {
                        Term::EndIf => {
                            els = Some(eb);
                            break;
                        }
                        Term::End => {
                            return Err(ParseError::new(
                                "unclosed IF block (missing ENDIF after ELSE)",
                                line,
                            ))
                        }
                        _ => {
                            return Err(ParseError::new(
                                "expected ENDIF to close ELSE block",
                                line,
                            ))
                        }
                    }
                }
                Term::End => {
                    return Err(ParseError::new("unclosed IF block (missing ENDIF)", line))
                }
                Term::EndWhile => {
                    return Err(ParseError::new(
                        "ENDWHILE inside IF block (missing ENDIF)",
                        line,
                    ))
                }
                Term::EndFor => {
                    return Err(ParseError::new("ENDFOR inside IF block (missing ENDIF)", line))
                }
                Term::EndSub => {
                    return Err(ParseError::new(
                        "ENDSUB inside IF block (missing ENDIF)",
                        line,
                    ))
                }
                Term::EndCapture => {
                    return Err(ParseError::new(
                        "ENDCAPTURE inside IF block (missing ENDIF)",
                        line,
                    ))
                }
                Term::Case(_) | Term::Default | Term::EndSwitch => {
                    return Err(ParseError::new(
                        "CASE/DEFAULT/ENDSWITCH inside IF block (missing ENDIF)",
                        line,
                    ))
                }
            }
        }
        Ok(Stmt::If { arms, els, span: Span::new(line) })
    }

    /// Parse a `WHILE … ENDWHILE` whose opener has already been consumed.
    fn parse_while(&mut self, rest: &str, line: usize) -> Result<Stmt, ParseError> {
        let cond = parse_full_cond(rest, line)
            .map_err(|e| ParseError::new(format!("WHILE condition: {}", e.msg), line))?;
        let (body, term) = self.parse_block()?;
        match term {
            Term::EndWhile => Ok(Stmt::While { cond, body, span: Span::new(line) }),
            Term::End => Err(ParseError::new("unclosed WHILE (missing ENDWHILE)", line)),
            _ => Err(ParseError::new("mismatched block end; expected ENDWHILE", line)),
        }
    }

    /// Parse a `FOR` header (`v = a TO b [STEP s]` or `v IN expr`) + its body to `ENDFOR`/`EF`.
    /// `self.pos` is already past the opener; `exclusive` reflects the `/X` switch.
    fn parse_for(&mut self, rest: &str, exclusive: bool, line: usize) -> Result<Stmt, ParseError> {
        let (var, kind) = parse_for_header(rest, exclusive, line)?;
        let (body, term) = self.parse_block()?;
        match term {
            Term::EndFor => Ok(Stmt::For { var, kind, body, span: Span::new(line) }),
            Term::End => Err(ParseError::new("unclosed FOR (missing ENDFOR)", line)),
            _ => Err(ParseError::new("mismatched block end; expected ENDFOR", line)),
        }
    }

    /// Parse a `SWITCH expr … ENDSWITCH` whose opener has already been consumed. Body is a series of
    /// `CASE <values>` arms then an optional `DEFAULT`; `DEFAULT` must be last. No statements may
    /// appear before the first `CASE`.
    fn parse_switch(&mut self, rest: &str, line: usize) -> Result<Stmt, ParseError> {
        let scrutinee = expr::parse(rest.trim())
            .map_err(|e| ParseError::new(format!("SWITCH value: {}", e.msg), line))?;
        // Anything before the first CASE/DEFAULT/ENDSWITCH is illegal.
        let (pre, mut term) = self.parse_block()?;
        if !pre.is_empty() {
            return Err(ParseError::new("SWITCH: statements before the first CASE", line));
        }
        let mut arms: Vec<crate::ast::SwitchArm> = Vec::new();
        let mut default = None;
        loop {
            match term {
                Term::Case(values) => {
                    let (body, t2) = self.parse_block()?;
                    arms.push(crate::ast::SwitchArm { values, body });
                    term = t2;
                }
                Term::Default => {
                    let (body, t2) = self.parse_block()?;
                    match t2 {
                        Term::EndSwitch => {
                            default = Some(body);
                            break;
                        }
                        Term::End => {
                            return Err(ParseError::new(
                                "unclosed SWITCH (missing ENDSWITCH after DEFAULT)",
                                line,
                            ))
                        }
                        _ => {
                            return Err(ParseError::new(
                                "DEFAULT must be the last clause of a SWITCH",
                                line,
                            ))
                        }
                    }
                }
                Term::EndSwitch => break,
                Term::End => {
                    return Err(ParseError::new("unclosed SWITCH (missing ENDSWITCH)", line))
                }
                _ => {
                    return Err(ParseError::new(
                        "expected CASE, DEFAULT, or ENDSWITCH inside SWITCH",
                        line,
                    ))
                }
            }
        }
        Ok(Stmt::Switch { scrutinee, arms, default, span: Span::new(line) })
    }

    /// Parse a `SUB name … ENDSUB` whose opener has already been consumed.
    fn parse_sub(&mut self, rest: &str, line: usize) -> Result<Stmt, ParseError> {
        let (name, _extra) = split_verb(rest);
        if name.is_empty() {
            return Err(ParseError::new("SUB requires a name", line));
        }
        let (body, term) = self.parse_block()?;
        match term {
            Term::EndSub => Ok(Stmt::Sub { name, body, span: Span::new(line) }),
            Term::End => Err(ParseError::new("unclosed SUB (missing ENDSUB)", line)),
            _ => Err(ParseError::new("mismatched block end; expected ENDSUB", line)),
        }
    }

    /// Parse `CAPTURE var [cmd]` whose opener has already been consumed. With a trailing
    /// command it's the single-line form (body = that one command); alone, it opens a block
    /// collected up to `ENDCAPTURE`.
    fn parse_capture(&mut self, rest: &str, line: usize) -> Result<Stmt, ParseError> {
        let (var, trailing) = split_verb(rest);
        if var.is_empty() {
            return Err(ParseError::new("CAPTURE requires a variable name", line));
        }
        let var = fold_ident(&var);
        let trailing = trailing.trim();
        // Single-line form: `CAPTURE var cmd` — body is the one inline command.
        if !trailing.is_empty() {
            let body = parse_inline(trailing, line)?;
            return Ok(Stmt::Capture { var, body: vec![body], span: Span::new(line) });
        }
        // Block form: collect the body up to ENDCAPTURE.
        let (body, term) = self.parse_block()?;
        match term {
            Term::EndCapture => Ok(Stmt::Capture { var, body, span: Span::new(line) }),
            Term::End => Err(ParseError::new("unclosed CAPTURE (missing ENDCAPTURE)", line)),
            _ => Err(ParseError::new("mismatched block end; expected ENDCAPTURE", line)),
        }
    }
}

/// A bare quoted-string command (`"x"`) is sugar for `INSERT "x"` — it's all XPL, so a string
/// literal at command position inserts its text. This makes a quoted typing cell work not just as a
/// whole KBD cell (desugared in the app layer) but also inside a command chain (`AR;" "`) or a
/// script. Returns the desugared `INSERT` statement when `text` is exactly one quoted-string literal.
fn quoted_literal_as_insert(text: &str, line: usize) -> Option<Stmt> {
    let t = text.trim();
    if !t.starts_with('"') {
        return None;
    }
    match crate::lexer::tokenize(t) {
        Ok(toks) if toks.len() == 1 && matches!(toks[0].tok, crate::lexer::Tok::Str(_)) => {
            Some(Stmt::Command { verb: "INSERT".to_string(), raw_args: t.to_string(), span: Span::new(line) })
        }
        _ => None,
    }
}

/// Parse a `CASE` value list: one or more comma-separated expressions (`CASE 1, 2, "x"`). At least
/// one value is required; a trailing comma is an error.
fn parse_case_values(rest: &str, line: usize) -> Result<Vec<Expr>, ParseError> {
    let mut s = rest.trim();
    if s.is_empty() {
        return Err(ParseError::new("CASE requires at least one value", line));
    }
    let mut values = Vec::new();
    loop {
        let (e, consumed) =
            expr::parse_prefix(s).map_err(|e| ParseError::new(format!("CASE value: {}", e.msg), line))?;
        values.push(e);
        let rem = s[consumed..].trim_start();
        if rem.is_empty() {
            break;
        }
        match rem.strip_prefix(',') {
            Some(next) => {
                s = next.trim_start();
                if s.is_empty() {
                    return Err(ParseError::new("CASE: trailing comma with no value", line));
                }
            }
            None => {
                return Err(ParseError::new(
                    format!("CASE: expected ',' or end of clause, got {rem:?}"),
                    line,
                ))
            }
        }
    }
    Ok(values)
}

/// Parse a `FOR` header into `(loop-var, kind)`: `v = a TO b [STEP s]` or `v IN collection`.
fn parse_for_header(rest: &str, exclusive: bool, line: usize) -> Result<(String, ForKind), ParseError> {
    let t = rest.trim_start();
    // Loop variable = the leading identifier.
    let end = t.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(t.len());
    if end == 0 {
        return Err(ParseError::new("FOR: expected a loop variable", line));
    }
    let var = fold_ident(&t[..end]);
    let mut after = t[end..].trim_start();

    // Tuple form: `k, v IN expr` — a second loop variable after a comma. Only the `IN` form takes
    // a tuple (a counting range has nothing to pair), so a comma here forces collection parsing.
    let mut value_var: Option<String> = None;
    if let Some(rest2) = after.strip_prefix(',') {
        let rest2 = rest2.trim_start();
        let vend = rest2.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(rest2.len());
        if vend == 0 {
            return Err(ParseError::new("FOR: expected a value variable after the comma", line));
        }
        value_var = Some(fold_ident(&rest2[..vend]));
        after = rest2[vend..].trim_start();
    }

    // Range form: `= start TO end [STEP step]`.
    if let Some(r) = after.strip_prefix('=') {
        if value_var.is_some() {
            return Err(ParseError::new("FOR: a `k, v` tuple applies only to `IN`, not a TO range", line));
        }
        let r = r.trim_start();
        let (start, c1) =
            expr::parse_prefix(r).map_err(|e| ParseError::new(format!("FOR start: {}", e.msg), line))?;
        let rem = r[c1..].trim_start();
        let (kw, rem) = split_verb(rem);
        if kw != "TO" {
            return Err(ParseError::new("FOR: expected TO after the start value", line));
        }
        let rem = rem.trim_start();
        let (end_e, c2) =
            expr::parse_prefix(rem).map_err(|e| ParseError::new(format!("FOR end: {}", e.msg), line))?;
        let rem = rem[c2..].trim_start();
        let step = if rem.is_empty() {
            None
        } else {
            let (kw2, rem2) = split_verb(rem);
            if kw2 != "STEP" {
                return Err(ParseError::new("FOR: expected STEP (or end of header) after the end value", line));
            }
            let rem2 = rem2.trim_start();
            let (step_e, c3) = expr::parse_prefix(rem2)
                .map_err(|e| ParseError::new(format!("FOR step: {}", e.msg), line))?;
            if !rem2[c3..].trim().is_empty() {
                return Err(ParseError::new("FOR: trailing text after the STEP value", line));
            }
            if matches!(&step_e, Expr::Num(n) if n.as_f64() == 0.0) {
                return Err(ParseError::new("FOR: STEP cannot be 0", line)); // would never advance
            }
            Some(step_e)
        };
        return Ok((var, ForKind::Range { start, end: end_e, step, exclusive }));
    }

    // Collection form: `IN expr`.
    let (kw, coll) = split_verb(after);
    if kw == "IN" {
        if exclusive {
            return Err(ParseError::new("FOR/X applies only to a `TO` range, not `IN`", line));
        }
        let coll = coll.trim();
        if coll.is_empty() {
            return Err(ParseError::new("FOR … IN: expected a collection expression", line));
        }
        let e = expr::parse(coll).map_err(|e| ParseError::new(format!("FOR IN: {}", e.msg), line))?;
        return Ok((var, ForKind::In { coll: e, value: value_var }));
    }

    Err(ParseError::new("FOR: expected `<var> = a TO b` or `<var> IN collection`", line))
}

/// Parse a simple (non-block) statement from an already-split command.
fn parse_simple(verb: &str, rest: &str, line: usize, col: usize) -> Result<Stmt, ParseError> {
    let span = Span::at(line, col);
    match verb {
        "GOTO" => {
            let label = fold_ident(rest.trim().trim_start_matches(':').trim());
            if label.is_empty() {
                return Err(ParseError::new("GOTO requires a label", line));
            }
            Ok(Stmt::Goto { label, span })
        }
        "BREAK" => Ok(Stmt::Break(span)),
        "CONTINUE" => Ok(Stmt::Continue(span)),
        "EXIT" | "EX" => Ok(Stmt::Exit(span)),
        "EXITALL" | "EX1" => Ok(Stmt::ExitAll(span)),
        "RETURN" => {
            let r = rest.trim();
            let value = if r.is_empty() {
                None
            } else {
                Some(expr::parse(r).map_err(|e| {
                    ParseError::new(format!("RETURN expression: {}", e.msg), line)
                })?)
            };
            Ok(Stmt::Return { value, span })
        }
        "SET" => parse_set(rest, line),
        "LOCAL" => Ok(Stmt::Local { names: name_list(rest), span }),
        "CLEAR" => Ok(Stmt::Clear { names: name_list(rest), span }),
        // Everything else (including SV, CALL, PR, MSG, editor verbs) dispatches
        // as a Command with its raw argument tail.
        _ => Ok(Stmt::Command {
            verb: verb.to_string(),
            raw_args: rest.to_string(),
            span,
        }),
    }
}

/// Parse a single inline command (the body of a single-line `IF`). Has no access
/// to the command stream, so a block opener cannot be inlined here — except a
/// nested single-line `IF`/`IFNOT`, which is handled string-only.
fn parse_inline(text: &str, line: usize) -> Result<Stmt, ParseError> {
    if text.trim_start().starts_with(':') {
        return Err(ParseError::new(
            "a label cannot be the body of a single-line IF",
            line,
        ));
    }
    if let Some(stmt) = quoted_literal_as_insert(text, line) {
        return Ok(stmt);
    }
    let (verb, rest) = split_verb(text);
    match verb.as_str() {
        "IF" => parse_inline_if(rest, line, false),
        "IFNOT" => parse_inline_if(rest, line, true),
        "WHILE" | "SUB" | "CAPTURE" => Err(ParseError::new(
            format!("{verb} block cannot be the body of a single-line IF"),
            line,
        )),
        _ => parse_simple(&verb, rest, line, 0), // inline (single-line IF) body: column not tracked
    }
}

/// Nested single-line `IF` as the body of another single-line `IF`. The nested
/// IF must itself be single-line (have an inline command).
fn parse_inline_if(rest: &str, line: usize, negated: bool) -> Result<Stmt, ParseError> {
    let (cond, consumed) = expr::parse_prefix(rest)
        .map_err(|e| ParseError::new(format!("IF condition: {}", e.msg), line))?;
    let cond = maybe_negate(cond, negated);
    let trailing = rest[consumed..].trim();
    if trailing.is_empty() {
        return Err(ParseError::new(
            "a block IF cannot be the body of a single-line IF (add an inline command)",
            line,
        ));
    }
    let body = parse_inline(trailing, line)?;
    Ok(Stmt::If {
        arms: vec![CondArm { cond, body: vec![body] }],
        els: None,
        span: Span::new(line),
    })
}

/// `SET name = expr` or `SET name[index] = expr`.
fn parse_set(rest: &str, line: usize) -> Result<Stmt, ParseError> {
    // The assignment `=` is the first `=` at bracket depth 0 (so an index
    // expression containing `==`/`<=` doesn't fool us).
    let eq = assignment_eq(rest)
        .ok_or_else(|| ParseError::new("SET requires 'name = expr'", line))?;
    // Compound assignment: when the byte just before `=` is `+`/`-`/`*`/`/`, this is
    // `name += expr` (etc.), desugared to `name = name <op> (expr)`. The op char is then
    // excluded from the LHS.
    let compound = (eq > 0).then(|| rest.as_bytes()[eq - 1]).and_then(compound_binop);
    let lhs_end = if compound.is_some() { eq - 1 } else { eq };
    let lhs = rest[..lhs_end].trim();
    if lhs.is_empty() {
        return Err(ParseError::new("SET requires a variable name before '='", line));
    }
    // Accept an optional leading `$` sigil on the target, so `SET $x = …` / `SET $x += …`
    // mean the same as `SET x = …` / `SET x += …` (XyWrite muscle memory).
    let lhs = lhs.strip_prefix('$').unwrap_or(lhs).trim();
    if lhs.is_empty() {
        return Err(ParseError::new("SET requires a variable name before '='", line));
    }
    let expr_text = rest[eq + 1..].trim();
    let rhs = expr::parse(expr_text)
        .map_err(|e| ParseError::new(format!("SET expression: {}", e.msg), line))?;

    // Indexed form: NAME[INDEX].
    if let Some(open) = lhs.find('[') {
        let close = lhs
            .rfind(']')
            .filter(|&c| c > open && lhs[c + 1..].trim().is_empty())
            .ok_or_else(|| ParseError::new("SET: malformed 'name[index]'", line))?;
        let name = lhs[..open].trim();
        if name.is_empty() {
            return Err(ParseError::new("SET: missing array/map name before '['", line));
        }
        let index_text = lhs[open + 1..close].trim();
        let index = expr::parse(index_text)
            .map_err(|e| ParseError::new(format!("SET index: {}", e.msg), line))?;
        let name = fold_ident(name);
        // `arr[i] += e` → `arr[i] = arr[i] <op> (e)` (the index is evaluated for both the
        // read and the store; index expressions are side-effect-free).
        let expr = match compound {
            Some(op) => {
                let target = Expr::Index(
                    Box::new(Expr::Var(name.clone())),
                    Box::new(index.clone()),
                );
                Expr::Binary(op, Box::new(target), Box::new(rhs))
            }
            None => rhs,
        };
        return Ok(Stmt::SetIndex { name, index, expr, span: Span::new(line) });
    }

    let name = fold_ident(lhs);
    let expr = match compound {
        Some(op) => Expr::Binary(op, Box::new(Expr::Var(name.clone())), Box::new(rhs)),
        None => rhs,
    };
    Ok(Stmt::Set { name, expr, span: Span::new(line) })
}

/// The arithmetic [`BinOp`] for a compound-assignment sigil byte (`+= -= *= /= ^=`), else `None`.
fn compound_binop(b: u8) -> Option<BinOp> {
    match b {
        b'+' => Some(BinOp::Add),
        b'-' => Some(BinOp::Sub),
        b'*' => Some(BinOp::Mul),
        b'/' => Some(BinOp::Div),
        b'^' => Some(BinOp::Pow),
        _ => None,
    }
}

/// Find the assignment `=` — the first `=` at bracket depth 0.
fn assignment_eq(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            '=' if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// Parse a condition that must consume the entire text (WHILE / ELSEIF).
fn parse_full_cond(text: &str, line: usize) -> Result<Expr, ParseError> {
    expr::parse(text).map_err(|e| ParseError::new(e.msg, line))
}

/// Negate a condition for the `IFNOT` form.
fn maybe_negate(cond: Expr, negated: bool) -> Expr {
    if negated {
        Expr::Not(Box::new(cond))
    } else {
        cond
    }
}

/// Split a command into its leading verb (case-folded) and the trimmed remainder.
/// A command that is a bare drive-qualified path (`D:`, `X:\dir`, `X:rel`, `X:/dir`) → the `CD`
/// verb with the path as its (original-case) argument. `None` for anything else. The test is a
/// single ASCII letter followed by `:` — the Windows drive-letter shape; no XPL verb starts that
/// way. `CD`/`resolve_path` normalize the rest (`D:` → `D:\`, `D:rel` → `D:\rel`).
fn drive_letter_as_cd(text: &str, line: usize) -> Option<Stmt> {
    let t = text.trim();
    let b = t.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return Some(Stmt::Command {
            verb: "CD".to_string(),
            raw_args: t.to_string(),
            span: Span::new(line),
        });
    }
    None
}

fn split_verb(text: &str) -> (String, &str) {
    let t = text.trim_start();
    let end = t.find([' ', '\t']).unwrap_or(t.len());
    (fold_ident(&t[..end]), t[end..].trim_start())
}

/// Whitespace-separated, case-folded name list (`LOCAL`/`CLEAR`).
fn name_list(rest: &str) -> Vec<String> {
    rest.split_whitespace().map(fold_ident).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::Expr;

    fn prog(src: &str) -> Program {
        parse(src).unwrap_or_else(|e| panic!("parse failed at line {}: {}", e.line, e.msg))
    }

    fn cmd(verb: &str, args: &str) -> Stmt {
        Stmt::Command {
            verb: verb.into(),
            raw_args: args.into(),
            span: Span::new(0),
        }
    }

    /// Compare ignoring spans (which carry line numbers).
    fn stmt_shape(s: &Stmt) -> String {
        format!("{:?}", strip_spans(s))
    }
    fn strip_spans(s: &Stmt) -> Stmt {
        // Re-emit with span line 0 so structural equality ignores line numbers.
        match s.clone() {
            Stmt::Command { verb, raw_args, .. } => Stmt::Command {
                verb,
                raw_args,
                span: Span::new(0),
            },
            Stmt::Set { name, expr, .. } => Stmt::Set { name, expr, span: Span::new(0) },
            Stmt::SetIndex { name, index, expr, .. } => Stmt::SetIndex {
                name,
                index,
                expr,
                span: Span::new(0),
            },
            Stmt::If { arms, els, .. } => Stmt::If {
                arms: arms
                    .into_iter()
                    .map(|a| CondArm {
                        cond: a.cond,
                        body: a.body.iter().map(strip_spans).collect(),
                    })
                    .collect(),
                els: els.map(|b| b.iter().map(strip_spans).collect()),
                span: Span::new(0),
            },
            Stmt::While { cond, body, .. } => Stmt::While {
                cond,
                body: body.iter().map(strip_spans).collect(),
                span: Span::new(0),
            },
            Stmt::For { var, kind, body, .. } => Stmt::For {
                var,
                kind,
                body: body.iter().map(strip_spans).collect(),
                span: Span::new(0),
            },
            Stmt::Switch { scrutinee, arms, default, .. } => Stmt::Switch {
                scrutinee,
                arms: arms
                    .into_iter()
                    .map(|a| crate::ast::SwitchArm {
                        values: a.values,
                        body: a.body.iter().map(strip_spans).collect(),
                    })
                    .collect(),
                default: default.map(|b| b.iter().map(strip_spans).collect()),
                span: Span::new(0),
            },
            Stmt::Sub { name, body, .. } => Stmt::Sub {
                name,
                body: body.iter().map(strip_spans).collect(),
                span: Span::new(0),
            },
            Stmt::Capture { var, body, .. } => Stmt::Capture {
                var,
                body: body.iter().map(strip_spans).collect(),
                span: Span::new(0),
            },
            Stmt::Label { name, .. } => Stmt::Label { name, span: Span::new(0) },
            Stmt::Goto { label, .. } => Stmt::Goto { label, span: Span::new(0) },
            Stmt::Break(_) => Stmt::Break(Span::new(0)),
            Stmt::Continue(_) => Stmt::Continue(Span::new(0)),
            Stmt::Return { value, .. } => Stmt::Return { value, span: Span::new(0) },
            Stmt::Exit(_) => Stmt::Exit(Span::new(0)),
            Stmt::ExitAll(_) => Stmt::ExitAll(Span::new(0)),
            Stmt::Local { names, .. } => Stmt::Local { names, span: Span::new(0) },
            Stmt::Clear { names, .. } => Stmt::Clear { names, span: Span::new(0) },
        }
    }

    // ---- simple statements --------------------------------------------------

    #[test]
    fn commands_and_verbs() {
        let p = prog("PR hello world");
        assert_eq!(p.stmts.len(), 1);
        assert_eq!(strip_spans(&p.stmts[0]), cmd("PR", "hello world"));
    }

    #[test]
    fn bare_drive_letter_is_a_cd_command() {
        // `D:` / `X:\dir` / `x:rel` on their own are drive-change commands → `CD <path>` with the
        // ORIGINAL case preserved (a plain verb would be folded to upper-case).
        assert_eq!(strip_spans(&prog("D:").stmts[0]), cmd("CD", "D:"));
        assert_eq!(strip_spans(&prog(r"X:\Proj\Sub").stmts[0]), cmd("CD", r"X:\Proj\Sub"));
        assert_eq!(strip_spans(&prog("x:rel").stmts[0]), cmd("CD", "x:rel"));
        // Chained after another command still works (splitter already separated them).
        let p = prog(r"PR hi; Q:\Docs");
        assert_eq!(strip_spans(&p.stmts[1]), cmd("CD", r"Q:\Docs"));
        // NOT triggered by a normal verb or a multi-letter token before a colon.
        assert_eq!(strip_spans(&prog("PR a:b").stmts[0]), cmd("PR", "a:b"));
        assert_eq!(strip_spans(&prog("foo:bar").stmts[0]), cmd("FOO:BAR", ""));
    }

    #[test]
    fn semicolons_make_separate_commands() {
        let p = prog("PR a; MSG b");
        assert_eq!(p.stmts.len(), 2);
        assert_eq!(strip_spans(&p.stmts[0]), cmd("PR", "a"));
        assert_eq!(strip_spans(&p.stmts[1]), cmd("MSG", "b"));
    }

    #[test]
    fn multi_hash_line_is_a_comment() {
        // A line of two or more `#` (header/emphasis comment) must produce no command, not a bogus
        // `##` verb. The reported bug.
        let p = prog("## just a comment\n### another\nPR hi");
        assert_eq!(p.stmts.len(), 1);
        assert_eq!(strip_spans(&p.stmts[0]), cmd("PR", "hi"));
    }

    #[test]
    fn leading_bom_is_ignored() {
        // A UTF-8 BOM (as Notepad saves) must not mangle the first command's verb.
        let p = prog("\u{feff}LOADAR myfile.ar");
        assert_eq!(p.stmts.len(), 1);
        assert_eq!(strip_spans(&p.stmts[0]), cmd("LOADAR", "myfile.ar"));
    }

    #[test]
    fn line_continuation_joins_with_trailing_backslash() {
        // A trailing " \" (whitespace + backslash) continues onto the next physical line.
        let p = prog("PR hello \\\nworld");
        assert_eq!(p.stmts.len(), 1);
        assert_eq!(strip_spans(&p.stmts[0]), cmd("PR", "hello world"));
    }

    #[test]
    fn line_continuation_chains_multiple_lines() {
        let p = prog("SET x = 1 + \\\n 2 + \\\n 3");
        assert_eq!(p.stmts.len(), 1);
        match strip_spans(&p.stmts[0]) {
            Stmt::Set { name, .. } => assert_eq!(name, "X"),
            other => panic!("expected one Set, got {other:?}"),
        }
    }

    #[test]
    fn line_continuation_leaves_windows_paths_intact() {
        // `C:\` — the backslash hugs the path (no preceding whitespace), so it is NOT a continuation.
        let p = prog("CD C:\\\nDIR");
        assert_eq!(p.stmts.len(), 2);
        assert_eq!(strip_spans(&p.stmts[0]), cmd("CD", "C:\\"));
        assert_eq!(strip_spans(&p.stmts[1]), cmd("DIR", ""));
    }

    #[test]
    fn line_continuation_respects_escaped_backslash() {
        // An even run of trailing backslashes is an escaped literal `\\`, not a continuation.
        let p = prog("PR a \\\\\nb");
        assert_eq!(p.stmts.len(), 2);
    }

    #[test]
    fn line_continuation_dangling_at_eof_is_dropped() {
        let p = prog("PR a \\");
        assert_eq!(p.stmts.len(), 1);
        assert_eq!(strip_spans(&p.stmts[0]), cmd("PR", "a"));
    }

    #[test]
    fn set_parses_expression() {
        let p = prog("SET total = $n + 1");
        match &p.stmts[0] {
            Stmt::Set { name, .. } => assert_eq!(name, "TOTAL"),
            other => panic!("expected Set, got {other:?}"),
        }
    }

    #[test]
    fn labels_and_goto() {
        let p = prog(":loop\nGOTO :loop");
        assert_eq!(strip_spans(&p.stmts[0]), Stmt::Label { name: "LOOP".into(), span: Span::new(0) });
        assert_eq!(strip_spans(&p.stmts[1]), Stmt::Goto { label: "LOOP".into(), span: Span::new(0) });
    }

    #[test]
    fn flow_keywords_and_aliases() {
        let p = prog("BREAK\nCONTINUE\nEXIT\nEX1\nRETURN\nLOCAL a b\nCLEAR x");
        let shapes: Vec<_> = p.stmts.iter().map(strip_spans).collect();
        assert_eq!(shapes[0], Stmt::Break(Span::new(0)));
        assert_eq!(shapes[1], Stmt::Continue(Span::new(0)));
        assert_eq!(shapes[2], Stmt::Exit(Span::new(0)));
        assert_eq!(shapes[3], Stmt::ExitAll(Span::new(0)));
        assert_eq!(shapes[4], Stmt::Return { value: None, span: Span::new(0) });
        assert_eq!(
            shapes[5],
            Stmt::Local { names: vec!["A".into(), "B".into()], span: Span::new(0) }
        );
        assert_eq!(shapes[6], Stmt::Clear { names: vec!["X".into()], span: Span::new(0) });
    }

    // ---- IF: single-line vs block ------------------------------------------

    #[test]
    fn single_line_if() {
        let p = prog("IF $x > 5 PR big");
        match &p.stmts[0] {
            Stmt::If { arms, els, .. } => {
                assert_eq!(arms.len(), 1);
                assert!(els.is_none());
                assert_eq!(arms[0].body.len(), 1);
                assert_eq!(strip_spans(&arms[0].body[0]), cmd("PR", "big"));
            }
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn block_if_with_elseif_and_else() {
        let src = "IF $s >= 90\nPR A\nELSEIF $s >= 70\nPR B\nELSE\nPR C\nENDIF";
        let p = prog(src);
        match &p.stmts[0] {
            Stmt::If { arms, els, .. } => {
                assert_eq!(arms.len(), 2);
                assert_eq!(strip_spans(&arms[0].body[0]), cmd("PR", "A"));
                assert_eq!(strip_spans(&arms[1].body[0]), cmd("PR", "B"));
                let els = els.as_ref().expect("else present");
                assert_eq!(strip_spans(&els[0]), cmd("PR", "C"));
            }
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn else_if_two_words_is_elseif() {
        let p = prog("IF $a\nPR x\nELSE IF $b\nPR y\nENDIF");
        match &p.stmts[0] {
            Stmt::If { arms, .. } => assert_eq!(arms.len(), 2),
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn ifnot_negates() {
        let p = prog("IFNOT $err PR ok");
        match &p.stmts[0] {
            Stmt::If { arms, .. } => {
                assert!(matches!(arms[0].cond, Expr::Not(_)));
            }
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn semicolon_chained_block_if_is_not_a_footgun() {
        // The QN footgun: this parses as ONE block IF, not a broken chain.
        let p = prog("IF $x ; PR yes ; ENDIF");
        assert_eq!(p.stmts.len(), 1);
        match &p.stmts[0] {
            Stmt::If { arms, els, .. } => {
                assert_eq!(arms.len(), 1);
                assert!(els.is_none());
                assert_eq!(strip_spans(&arms[0].body[0]), cmd("PR", "yes"));
            }
            other => panic!("expected If, got {other:?}"),
        }
    }

    // ---- WHILE / SUB / nesting ---------------------------------------------

    #[test]
    fn while_block() {
        let p = prog("WHILE $i < 3\nSET i = $i + 1\nENDWHILE");
        match &p.stmts[0] {
            Stmt::While { body, .. } => assert_eq!(body.len(), 1),
            other => panic!("expected While, got {other:?}"),
        }
    }

    #[test]
    fn sub_block() {
        let p = prog("SUB greet\nPR hi\nENDSUB");
        match &p.stmts[0] {
            Stmt::Sub { name, body, .. } => {
                assert_eq!(name, "GREET");
                assert_eq!(body.len(), 1);
            }
            other => panic!("expected Sub, got {other:?}"),
        }
    }

    #[test]
    fn nested_if_inside_while() {
        let src = "WHILE 1\nIF $done\nBREAK\nENDIF\nENDWHILE";
        let p = prog(src);
        match &p.stmts[0] {
            Stmt::While { body, .. } => {
                assert_eq!(body.len(), 1);
                assert!(matches!(body[0], Stmt::If { .. }));
            }
            other => panic!("expected While, got {other:?}"),
        }
    }

    #[test]
    fn aliases_ei_ew_es() {
        assert!(parse("IF 1\nPR x\nEI").is_ok());
        assert!(parse("WHILE 1\nBREAK\nEW").is_ok());
        assert!(parse("SUB s\nPR x\nES").is_ok());
    }

    // ---- structural errors --------------------------------------------------

    #[test]
    fn endif_without_if_errors() {
        let e = parse("PR a\nENDIF").unwrap_err();
        assert!(e.msg.contains("ENDIF"));
    }

    #[test]
    fn unclosed_if_errors() {
        assert!(parse("IF $x\nPR a").is_err());
    }

    #[test]
    fn unclosed_while_errors() {
        assert!(parse("WHILE 1\nPR a").is_err());
    }

    #[test]
    fn capture_inline_and_block_parse() {
        // Inline: body is the one trailing command.
        let p = prog("CAPTURE x PR hi");
        assert_eq!(
            strip_spans(&p.stmts[0]),
            Stmt::Capture { var: "X".into(), body: vec![cmd("PR", "hi")], span: Span::new(0) }
        );
        // Block: body collects to ENDCAPTURE.
        let p = prog("CAPTURE y\nPR a\nPR b\nENDCAPTURE");
        assert_eq!(
            strip_spans(&p.stmts[0]),
            Stmt::Capture {
                var: "Y".into(),
                body: vec![cmd("PR", "a"), cmd("PR", "b")],
                span: Span::new(0),
            }
        );
    }

    #[test]
    fn capture_structural_errors() {
        assert!(parse("CAPTURE x\nPR a").is_err()); // unclosed
        let e = parse("PR a\nENDCAPTURE").unwrap_err(); // unmatched
        assert!(e.msg.contains("ENDCAPTURE"));
        assert!(parse("CAPTURE").is_err()); // missing var name
    }

    #[test]
    fn unclosed_sub_errors() {
        assert!(parse("SUB s\nPR a").is_err());
    }

    #[test]
    fn switch_parses_and_structural_errors() {
        // Well-formed switch parses to a single Switch with arms + default.
        let p = prog("SWITCH $x\nCASE 1, 2\nPR a\nDEFAULT\nPR z\nENDSWITCH");
        match strip_spans(&p.stmts[0]) {
            Stmt::Switch { arms, default, .. } => {
                assert_eq!(arms.len(), 1);
                assert_eq!(arms[0].values.len(), 2); // CASE 1, 2
                assert!(default.is_some());
            }
            other => panic!("expected Switch, got {other:?}"),
        }
        // ESW is the short close.
        assert!(parse("SWITCH 1\nCASE 1\nPR a\nESW").is_ok());
        // Structural errors.
        assert!(parse("SWITCH 1\nCASE 1\nPR a").is_err()); // unclosed
        assert!(parse("CASE 1\nPR a\nESW").is_err()); // CASE without SWITCH
        assert!(parse("ENDSWITCH").unwrap_err().msg.contains("ENDSWITCH")); // unmatched
        assert!(parse("SWITCH 1\nCASE\nPR a\nESW").is_err()); // CASE with no value
        assert!(parse("SWITCH 1\nPR a\nCASE 1\nPR b\nESW").is_err()); // stmt before first CASE
        assert!(parse("SWITCH 1\nDEFAULT\nPR z\nCASE 1\nPR a\nESW").is_err()); // DEFAULT not last
    }

    #[test]
    fn else_with_argument_errors() {
        assert!(parse("IF $x\nPR a\nELSE foo\nPR b\nENDIF").is_err());
    }

    #[test]
    fn mismatched_block_end_errors() {
        // WHILE closed by ENDIF.
        assert!(parse("WHILE 1\nPR a\nENDIF").is_err());
    }

    // keep helper referenced
    #[test]
    fn shape_helper_smoke() {
        let _ = stmt_shape(&cmd("PR", "x"));
    }
}
