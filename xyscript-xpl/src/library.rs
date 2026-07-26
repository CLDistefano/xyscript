//! The library system (`stage1-interpreter-plan.md` §11).
//!
//! A library is a flat collection of `SUB…ENDSUB` routines, parsed from `.lib`
//! text, callable like any other sub. There are two layers — a bundled
//! **system** library and a **user** library that overrides it name-for-name —
//! mirroring QN's `_system_lib` / `_user_lib`.
//!
//! Ported behaviors (the floor):
//!
//! - **Strict `.lib` grammar.** Top level is only blank lines, `#` comments, and
//!   `SUB…ENDSUB` blocks. A leading `---`/`---` YAML frontmatter block is
//!   tolerated and skipped. Anything else at top level, a nested `SUB`, an
//!   unclosed `SUB`, a stray `ENDSUB`, or a duplicate routine is a load-time
//!   error — and on **any** error the whole parse fails (`Err`), so a partial /
//!   inconsistent library never goes live.
//! - **`# DOC:` capture.** `# DOC: <text>` lines anywhere in a body are plucked
//!   out (joined with newlines) for `LISTLIB`.
//! - **Shadow-rejection** ([`validate_names`]): a routine may not collide with a
//!   built-in verb, a control-flow keyword, or a special-variable name; such
//!   routines are skipped with a warning.
//! - **Last-wins merge** and **user-over-system** lookup ([`Library::effective`]).
//!
//! XyScript twist (per the foundational premise): routine bodies are parsed into
//! the real [`Stmt`] AST here, so the executor hoists them as ordinary subs
//! (`ir::lower_with`) — no separate library-invocation machine.

use crate::ast::Stmt;
use crate::env::SPECIAL_NAMES;
use crate::value::fold_ident;
use std::collections::BTreeMap;

/// Where a routine came from (for `LISTLIB` tagging and override reporting).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibSource {
    System,
    User,
}

/// One parsed library routine.
#[derive(Debug, Clone, PartialEq)]
pub struct Routine {
    /// Case-folded routine name.
    pub name: String,
    /// Parsed body (a routine is callable exactly like a script `SUB`).
    pub body: Vec<Stmt>,
    /// The routine's ORIGINAL body text (between `SUB`/`ENDSUB`, `# DOC:` lines included), retained
    /// verbatim so `SAVELIB` can round-trip it back to a loadable `.lib` file — lossless, unlike
    /// re-rendering `body`.
    pub body_src: String,
    /// Joined `# DOC:` text (may be empty).
    pub doc: String,
    pub source: LibSource,
}

/// Built-in verb names a library routine may not shadow.
pub const BUILTIN_VERBS: &[&str] = &[
    "LOADKBD", "CLEARKBD", "SAVEKBD", "KBDDOC", "INSERT", "WRITE", "WR", "WRITELN", "WRL", "PUT", "WRITEFILE", "WF", "PRINTEX", "MERGE", "ME", "PR", "MSG", "MESSAGE", "CHECK", "SPELLING", "SP", "GRAMMAR", "GR", "SPELLNEXT", "SPELLPREV", "GRAMMARNEXT", "GRAMMARPREV", "LINTFIX", "DIALECT", "LEARN", "UNLEARN", "SV", "SETERR", "CLERR", "QUIET",
    "NOP", "CALL", "EXIT", "EX", "EXITALL", "EX1", "LOADLIB", "ADDLIB", "RELOADLIB",
    "UNLOADLIB", "SAVELIB", "LISTLIB", "LISTSPECIALS", "KBCHART", "RUN", "RUNCODE", "HELP", "?", "MENU",
    // Rich motion (B2): word / sentence / paragraph / line-start + char/line
    // synonyms + JMP + viewport (page / screen-edge) + wrap-aware char (CL/CR/LL/LR).
    "NW", "PW", "NS", "PS", "NP", "PP", "NL", "PL", "NC", "PC", "LU", "LD", "JMP",
    "CL", "CR", "LL", "LR", "PD", "PU", "HM", "BS",
    // Editor verbs (Stage 2).
    "BS", "DEL", "DELSEL", "LEFT", "RIGHT", "UP", "DOWN", "HOME", "END", "EL", "ER", "LB", "LE",
    "TOP",
    "BOTTOM", "TF", "BF", "UNDO", "REDO", "NEW", "NE", "OPEN", "READ", "CA", "ED", "EDIT", "BEDIT", "BCLOSE", "SAVE", "SA",
    "BUFNEXT", "BUFPREV", "BUFALT", "BUFGO", "GOFILE", "GOF", "LISTTABS",
    "BUFLIST", "CMLINE", "SUBMIT", "CANCEL", "QUIT", "QQ",
    // Split panes: split (THS/TVS), focus (TW), un-split (TUS), swap (TRV), zoom (TZ),
    // release focused pane to full screen (FS). `NEW/H` / `NEW/V` also split.
    "THS", "TVS", "TW", "TUS", "TRV", "TZ", "FS",
    // Defined-text selection (DF model).
    "DF", "DM", "DZ", "DFA", "XD", "YD", "DB", "DE",
    // Define a unit (word / line / sentence / paragraph) at the cursor; the `+`
    // forms include the trailing separator.
    "DW", "DL", "DS", "DP", "DW+", "DS+", "DP+",
    // Persistent selection.
    "SPS", "CPS", "TPS",
    // Deletion buffer, strict delete, case; char + whole-unit deletes.
    "RD", "UD", "DN", "LC", "UC", "CC", "BD", "RC", "RW", "RL", "RS", "RP", "RE",
    // In-document copy / move selection.
    "CP", "MV",
    // Clipboard ($CB): cut/copy/paste + set/append/prepend.
    "CUT", "COPY", "PASTE", "CLIP", "APPEND", "PREPEND",
    // Search / replace-all (self-delimiting; B=backward, A=absolute suffixes).
    // CI/CIA = XyWrite synonyms for CH/CHA.
    "SE", "SEB", "SEA", "SEBA", "CH", "CHA", "CI", "CIA", "FIND",
    // Interactive change-verify.
    "CV", "CVA",
    // Counts (B8): word / character count of the selection or whole document.
    "WORDCOUNT", "CHARCOUNT",
    // Seed the random generator for the `RANDOM(min, max)` function.
    "SRANDOM",
    // Sort (B11): reorder the selected lines.
    "SORT",
    // Filesystem (B10): working directory + directory listing browser + file ops.
    // ERNV = "erase, no verify" — synonym for DEL/NV (DEL itself is in the editing group above).
    "CD", "DIR", "LS", "DIRNW", "DIRWNW", "DIRWCLOSE", "DIRRT", "DIRSET", "RENAME", "REN", "ERNV",
    "GREP", "GJ", // ripgrep-style recursive search into a results tab + point-and-shoot jump
    // Shell-out (B14, trust-gated): run / insert-stdout / capture-to-var; TRUST gate.
    // `JOBS` lists background jobs launched with `X/BG`; `KILL <id>` terminates one.
    "X", "XY", "XV", "TRUST", "JOBS", "KILL",
    // Literate scripting (B22): run the fenced code block under the caret.
    "RUNHERE",
    // Date / time / stopwatch (B12). `$ET` reports elapsed; report it with `PR $ET`.
    "DA", "ZT", "ET",
    // Tab width in spaces (`TB <n>`; read via `$TB`).
    "TB",
    // Fixed decimal places for float display (`DECIMALS <n>`/`off`; read via `$DECIMALS`).
    "DECIMALS",
    // PDF/preview render kind for the active doc (`DOCTYPE typst|gfm|auto`; read via `$DOCTYPE`).
    "DOCTYPE",
    // Viewport scroll by one visual line, caret unmoved (`MD` down / `MU` up).
    "MD", "MU",
    // Sequencing (B13): pause execution.
    "WAIT",
    // Interactive input (B13/B21): pause for a keystroke / a typed line, suspend-resume.
    "READKEY", "RK", "INPUT", "PICK", "CHOOSE",
    // Dialogs (B21): scrollable text viewer.
    "TXT",
    // CMline focus & dual-cursor memory (B23).
    "GH", "GT", "TH", "BC", "XC",
    // OS-aware newline (the Enter key inserts the doc's line ending); TAB inserts a tab char.
    "ENTER", "TAB", "TABSPACES", "TABINDENT",
    // Per-document counters C1..C9 (number / letter / Roman, with text decorators).
    "C1", "C2", "C3", "C4", "C5", "C6", "C7", "C8", "C9",
    // Inline breakpoint (debugging): BK off|on|debug — pause/continue/abort the script.
    "BK",
    // Save/file lifecycle (B18): save-as, save-selection, revert, read-only, close-tab,
    // save-all, save+close-all.
    "ST", "SAAS", "SAVEAS", "SAD", "REC", "RECALL", "RO", "AB", "ABD", "SL", "SF", "SAVECFG", "LOADCFG",
    "AUTOCFG", "AUTOSAVE",
    // Lock keys: set/clear/toggle Caps / Num / Scroll.
    "SCL", "CCL", "TCL", "SNL", "CNL", "TNL", "SSL", "CSL", "TSL",
    // Command history: open the picker (HISTORY/HI), load a saved list (LoadCM), stash the
    // current cmline (CMH).
    "HISTORY", "HI", "LOADCM", "SAVECM", "CMH", "LOADCLOSED", "SAVECLOSED", "REOPEN",
    "CLEARCLOSED",
    // Global-variable persistence: save/reload/list all globals and their values.
    "SAVEVARS", "LOADVARS", "LISTVARS", "LISTVAR",
    // GUI view toggles (B15): menu bar / visible-¶ / visible-whitespace / line-number
    // gutter / pop-up command line, each S=on C=off T=toggle.
    "SMB", "CMB", "TMB", "SVR", "CVR", "TVR", "SWS", "CWS", "TWS", "SLN", "CLN", "TLN",
    "SGC", "CGC", "TGC", "SW", "SHELLWIN", "SH", "SHELL", "PV", "PREVIEW", "AR", "LOADAR", "SAVEAR", "CLEARAR",
    // GUI sizing knobs (B16): UI zoom / chrome+editor font pt / line-spacing / margins,
    // plus reset.
    "UISCALE", "UIFONT", "EDFONT", "LEADING", "LSP", "OF", "OFL", "OFR", "UIRESET",
    "TXTFONT", "TXTSCALE", "EDFAMILY", "UIFAMILY", "TXTFAMILY", "EDFONTS", "UIFONTS", "TXTFONTS",
    "EDSTYLE", "UISTYLE", "TXTSTYLE",
    // DIR/W listing window appearance (DW…): font/scale/family/style + reset + picker.
    "DWFONT", "DWSCALE", "DWFAMILY", "DWSTYLE", "DWRESET", "DWFONTS",
    // Markdown Preview / TOPDF fonts (PV…): body family/size + code family + pickers.
    "PVFAMILY", "PVSIZE", "PVCODE", "PVFONTS", "PVCODES",
    "PVMARGIN", "PVMARGINL", "PVMARGINR", "PVMARGINT", "PVMARGINB", // Preview/TOPDF page margins
    // Colours: editor canvas (FG/BG) + UI chrome (UIFG/UIBG) + caret colour/shape.
    "FGCOLOR", "BGCOLOR", "UIFGCOLOR", "UIBGCOLOR", "CRCOLOR", "CMCRCOLOR", "SEPCOLOR", "CRSHAPE", "CMCRSHAPE", "CMPLACE",
    "CMBLANK", "SHOWONETAB", "UNTITLEDTAB", "WRAP", "WRAPHANG", "PAPLACE", "PATHLEN", "PAFGCOLOR", "PAFGCOLORS", "PABGCOLOR", "PABGCOLORS",
    "TXTFGCOLOR", "TXTBGCOLOR", "DWFGCOLOR", "DWBGCOLOR",
    // Encoding / syntax highlighting / PDF export + print.
    "ENCODING", "NEWLINE", "TITLEBAR", "WV", "WINGEO", "SESSION", "HIGHLIGHT", "HLAUTO", "HLCOLOR", "HLRESET", "HLKINDS", "HLLANG", "HLRULE", "THEME", "TABBGCOLOR", "TABBGCOLORS", "TABXCOLOR", "TABXCOLORS", "TOPDF", "PRINT",
    "FGCOLORS", "BGCOLORS", "UIFGCOLORS", "UIBGCOLORS", "CRCOLORS", "CMCRCOLORS", "SEPCOLORS", "TXTFGCOLORS",
    "TXTBGCOLORS", "DWFGCOLORS", "DWBGCOLORS", "HLCOLORS", "SELBGCOLOR", "SELBGCOLORS", "SELFGCOLOR", "SELFGCOLORS",
    "FINDBGCOLOR", "FINDBGCOLORS", "FINDFGCOLOR", "FINDFGCOLORS",
    "SPELLCOLOR", "SPELLCOLORS", "GRAMMARCOLOR", "GRAMMARCOLORS",
    "SPELLSTYLE", "GRAMMARSTYLE", // live-check underline style: dotted|solid|squiggle
    "PANECOLOR", "PANECOLORS", // active-pane border colour (split panes)
    "BALPAIRS", "BALPAIRSCOLOR", "BALPAIRSCOLORS", // matching-bracket highlight
    "EMPH", "EMPHFGCOLOR", "EMPHFGCOLORS", "EMPHBGCOLOR", "EMPHBGCOLORS", // EMPH emphasis highlight
    "TYPEPAIRS", // auto-close typed brackets/quotes
    "WRAPINDENT", // hanging indent for soft-wrapped lines
    // Binder loaders: text binder `@slot` (LDTX), script binder `&slot` (LDSC/LDPM).
    "LDTX", "LDSC", "LDPM", "SAVELDTX", "SAVELDSC", "SAVELDPM",
    // Email: SMTP send config + SENDMAIL (+ ATTACH/CLEARATTACH/MAILDEBUG) and IMAP receive
    // (IMAPURL/MAILBOX config + LISTMAIL/SAVEMAIL/DELMAIL). Names are always listed; the
    // implementation is behind the `email` cargo feature (a slim build reports "needs the email build").
    "SMTPURL", "MAILUSER", "MAILFROM", "MAILPASS", "MAILDEBUG", "SENDMAIL", "ATTACH", "CLEARATTACH",
    "IMAPURL", "MAILBOX", "LISTMAIL", "SAVEMAIL", "DELMAIL",
];

/// Control-flow keyword names a library routine may not shadow.
pub const CONTROL_KEYWORDS: &[&str] = &[
    "IF", "IFNOT", "ELSE", "ELSEIF", "ENDIF", "EI", "WHILE", "ENDWHILE", "EW", "SUB",
    "ENDSUB", "ES", "FOR", "ENDFOR", "EF", "BREAK", "CONTINUE", "GOTO", "RETURN", "SET",
    "LOCAL", "CLEAR", "CAPTURE", "ENDCAPTURE", "EC",
    "SWITCH", "CASE", "DEFAULT", "ENDSWITCH", "ESW",
];

/// Parse `.lib` text into routines. On **any** error, returns `Err(messages)`
/// and no routines (atomic — a partial library never goes live).
pub fn parse_lib(text: &str, source: LibSource) -> Result<Vec<Routine>, Vec<String>> {
    let lines: Vec<&str> = text.lines().collect();
    let n = lines.len();
    let mut routines: Vec<Routine> = Vec::new();
    let mut i = skip_frontmatter(&lines);

    while i < n {
        let raw = lines[i];
        let stripped = raw.trim();
        if stripped.is_empty() || stripped.starts_with('#') {
            i += 1;
            continue;
        }
        if let Some(name) = sub_header_name(raw) {
            let header_line = i;
            let mut body_lines: Vec<&str> = Vec::new();
            let mut doc_parts: Vec<String> = Vec::new();
            let mut j = i + 1;
            let mut closed = false;
            while j < n {
                let lj = lines[j];
                if sub_header_name(lj).is_some() {
                    return Err(vec![format!(
                        "line {}: nested SUB inside '{}' (top-level only)",
                        j + 1,
                        name
                    )]);
                }
                if is_endsub(lj) {
                    closed = true;
                    break;
                }
                if let Some(doc) = doc_text(lj) {
                    doc_parts.push(doc);
                }
                body_lines.push(lj);
                j += 1;
            }
            if !closed {
                return Err(vec![format!(
                    "line {}: SUB '{}' has no matching ENDSUB",
                    header_line + 1,
                    name
                )]);
            }
            if routines.iter().any(|r| r.name == name) {
                return Err(vec![format!(
                    "line {}: duplicate routine '{}'",
                    header_line + 1,
                    name
                )]);
            }
            // Parse the body as its own mini-program.
            let body_src = body_lines.join("\n");
            let body = match crate::parser::parse(&body_src) {
                Ok(p) => p.stmts,
                Err(e) => {
                    return Err(vec![format!(
                        "line {}: in SUB '{}': {}",
                        header_line + 1 + e.line,
                        name,
                        e.msg
                    )])
                }
            };
            routines.push(Routine { name, body, body_src, doc: doc_parts.join("\n"), source });
            i = j + 1;
            continue;
        }
        if is_endsub(raw) {
            return Err(vec![format!("line {}: ENDSUB without matching SUB", i + 1)]);
        }
        let preview = if stripped.len() <= 60 {
            stripped.to_string()
        } else {
            format!("{}...", &stripped[..57])
        };
        return Err(vec![format!(
            "line {}: top-level content not allowed in .lib (only SUB blocks): {preview:?}",
            i + 1
        )]);
    }

    Ok(routines)
}

/// Validate routines for the library: reject (with a warning each) any that shadow a built-in verb /
/// control keyword / special variable, OR that **fail to compile** (e.g. a `GOTO` to a missing
/// label). The compile check is essential: a routine that parses but won't lower would otherwise
/// fail compilation of EVERY later command — the whole library is lowered to resolve `CALL`s — so one
/// broken routine would silently break the entire editor. Skipping it keeps the rest of the library
/// usable and surfaces a clear message naming the culprit.
pub fn validate_names(routines: Vec<Routine>) -> (Vec<Routine>, Vec<String>) {
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    for r in routines {
        let n = r.name.as_str();
        if BUILTIN_VERBS.contains(&n) {
            rejected.push(format!("'{n}' shadows a built-in command -- skipped."));
        } else if CONTROL_KEYWORDS.contains(&n) {
            rejected.push(format!("'{n}' shadows a control-flow keyword -- skipped."));
        } else if SPECIAL_NAMES.contains(&n) {
            rejected.push(format!("'{n}' shadows a special variable -- skipped."));
        } else if let Err(e) =
            crate::ir::lower(&crate::ast::Program { stmts: r.body.clone() })
        {
            rejected.push(format!(
                "'{n}' has an error (line {} of the routine) and was skipped: {}",
                e.line, e.msg
            ));
        } else {
            accepted.push(r);
        }
    }
    (accepted, rejected)
}

/// The two-layer library: an optional system layer overlaid by a user library (the user's routines
/// win name-for-name). Nothing populates the system layer by default — it's the override mechanism,
/// available to a host that wants a base library.
#[derive(Default)]
pub struct Library {
    system: BTreeMap<String, Routine>,
    user: BTreeMap<String, Routine>,
}

impl Library {
    pub fn new() -> Library {
        Library::default()
    }

    /// Install the system library from already-parsed, validated routines,
    /// replacing any previous system library.
    pub fn set_system(&mut self, routines: Vec<Routine>) {
        self.system = routines.into_iter().map(|r| (r.name.clone(), r)).collect();
    }

    /// Replace the user library wholesale (LOADLIB success).
    pub fn set_user(&mut self, routines: Vec<Routine>) {
        self.user = routines.into_iter().map(|r| (r.name.clone(), r)).collect();
    }

    /// Merge routines into the user library, last-wins (ADDLIB success).
    pub fn merge_user(&mut self, routines: Vec<Routine>) {
        for r in routines {
            self.user.insert(r.name.clone(), r);
        }
    }

    /// Drop all user routines (UNLOADLIB). The system library is unaffected.
    pub fn unload_user(&mut self) -> usize {
        let n = self.user.len();
        self.user.clear();
        n
    }

    pub fn user_count(&self) -> usize {
        self.user.len()
    }
    pub fn system_count(&self) -> usize {
        self.system.len()
    }

    /// Look up a routine by (folded) name — user wins over system.
    pub fn get(&self, name: &str) -> Option<&Routine> {
        let f = fold_ident(name);
        self.user.get(&f).or_else(|| self.system.get(&f))
    }

    /// The effective routine set (user over system) as `(name, body)` pairs,
    /// **user routines first** so `ir::lower_with` registers them ahead of
    /// system routines of the same name. Cloned so the result is independent of
    /// the library (lets the library be mutated by `LOADLIB` during a run).
    pub fn effective(&self) -> Vec<(String, Vec<Stmt>)> {
        let mut out: Vec<(String, Vec<Stmt>)> = Vec::new();
        for (name, r) in &self.user {
            out.push((name.clone(), r.body.clone()));
        }
        for (name, r) in &self.system {
            if !self.user.contains_key(name) {
                out.push((name.clone(), r.body.clone()));
            }
        }
        out
    }

    /// Serialize the USER routines back to `.lib` text (`SUB <name>` … body … `ENDSUB` per routine,
    /// sorted by name), reloadable with `LOADLIB`/`ADDLIB`. Uses each routine's retained `body_src`,
    /// so `# DOC:` lines and formatting survive. The system library is never written. Empty (no
    /// routines) → just the leading header comment from the caller.
    pub fn user_lib_text(&self) -> String {
        let mut s = String::new();
        for (name, r) in &self.user {
            s.push_str("SUB ");
            s.push_str(name);
            s.push('\n');
            if !r.body_src.is_empty() {
                s.push_str(&r.body_src);
                if !r.body_src.ends_with('\n') {
                    s.push('\n');
                }
            }
            s.push_str("ENDSUB\n\n");
        }
        s
    }

    /// A human-readable listing (LISTLIB), one routine per line, tagged with its
    /// source and `[overrides]` where a user routine shadows a system one.
    pub fn listing(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (name, r) in &self.user {
            let overrides = if self.system.contains_key(name) {
                " [USER, overrides]"
            } else {
                " [USER]"
            };
            lines.push(format_listing(name, &r.doc, overrides));
        }
        for (name, r) in &self.system {
            if !self.user.contains_key(name) {
                lines.push(format_listing(name, &r.doc, " [SYSTEM]"));
            }
        }
        if lines.is_empty() {
            lines.push("(no library routines loaded)".to_string());
        }
        lines
    }

    /// Same routines as [`listing`](Self::listing), but split into `(name, description)`
    /// pairs for a two-column grid: left = `name [SOURCE]`, right = the routine's docs
    /// (every `# DOC:` line, joined with newlines; the GUI wraps within each column).
    pub fn listing_columns(&self) -> Vec<(String, String)> {
        let mut rows = Vec::new();
        for (name, r) in &self.user {
            let tag = if self.system.contains_key(name) {
                " [USER, overrides]"
            } else {
                " [USER]"
            };
            rows.push((format!("{name}{tag}"), doc_cell(&r.doc)));
        }
        for (name, r) in &self.system {
            if !self.user.contains_key(name) {
                rows.push((format!("{name} [SYSTEM]"), doc_cell(&r.doc)));
            }
        }
        if rows.is_empty() {
            rows.push(("(no library routines loaded)".to_string(), String::new()));
        }
        rows
    }
}

/// The description cell for the LISTLIB grid: the routine's non-empty `# DOC:` lines joined
/// with newlines, or `(no docs)` when it has none.
fn doc_cell(doc: &str) -> String {
    let docs: Vec<&str> = doc.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if docs.is_empty() { "(no docs)".to_string() } else { docs.join("\n") }
}

fn format_listing(name: &str, doc: &str, tag: &str) -> String {
    // Every `# DOC:` line shows: the first on the name line, the rest indented beneath it (so a
    // routine's multi-line docs stay grouped under its name in the LISTLIB dialog).
    let mut docs = doc.lines().map(str::trim).filter(|l| !l.is_empty());
    match docs.next() {
        None => format!("{name}{tag} (no docs)"),
        Some(first) => {
            let mut out = format!("{name}{tag} -- {first}");
            for extra in docs {
                out.push_str("\n    ");
                out.push_str(extra);
            }
            out
        }
    }
}

// ---- low-level .lib scanning helpers ---------------------------------------

/// Skip a leading `---`/`---` YAML frontmatter block; return the first line
/// index to parse. If there is no (closed) frontmatter, returns 0.
fn skip_frontmatter(lines: &[&str]) -> usize {
    let mut j = 0;
    while j < lines.len() && lines[j].trim().is_empty() {
        j += 1;
    }
    if j < lines.len() && lines[j].trim_end() == "---" {
        let mut k = j + 1;
        while k < lines.len() && lines[k].trim_end() != "---" {
            k += 1;
        }
        if k < lines.len() {
            return k + 1; // skip through the closing fence
        }
    }
    0
}

/// If `line` is a `SUB <name>` header, return the folded name.
fn sub_header_name(line: &str) -> Option<String> {
    let mut it = line.split_whitespace();
    let first = it.next()?;
    if first.eq_ignore_ascii_case("SUB") {
        let name = it.next()?;
        return Some(fold_ident(name));
    }
    None
}

/// Is `line` an `ENDSUB` / `ES` closer (alone)?
fn is_endsub(line: &str) -> bool {
    let t = line.trim();
    t.eq_ignore_ascii_case("ENDSUB") || t.eq_ignore_ascii_case("ES")
}

/// Extract `# DOC: <text>` content, if `line` is such a comment.
fn doc_text(line: &str) -> Option<String> {
    let t = line.trim();
    let rest = t.strip_prefix('#')?.trim_start();
    let after = rest.get(..4).filter(|p| p.eq_ignore_ascii_case("DOC:"))?;
    let _ = after;
    Some(rest[4..].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(rs: &[Routine]) -> Vec<&str> {
        rs.iter().map(|r| r.name.as_str()).collect()
    }

    #[test]
    fn parses_routines_and_docs() {
        let src = "\
# a library
SUB greet
    # DOC: say hello
    # DOC: to someone
    PR hello $1
ENDSUB

SUB bye
    PR bye
ES";
        let rs = parse_lib(src, LibSource::User).unwrap();
        assert_eq!(names(&rs), vec!["GREET", "BYE"]);
        assert_eq!(rs[0].doc, "say hello\nto someone");
        assert_eq!(rs[1].doc, "");
    }

    #[test]
    fn frontmatter_is_skipped() {
        let src = "---\ntitle: x\n---\nSUB s\nPR hi\nENDSUB";
        let rs = parse_lib(src, LibSource::User).unwrap();
        assert_eq!(names(&rs), vec!["S"]);
    }

    #[test]
    fn user_lib_text_round_trips() {
        // Two routines, one with a `# DOC:` line and control flow — SAVELIB text must re-parse equal.
        let src = "SUB greet\n# DOC: says hi\nPR hello\nENDSUB\nSUB classify\nIF $1 > 0\nRETURN pos\nENDIF\nENDSUB";
        let mut lib = Library::new();
        lib.set_user(parse_lib(src, LibSource::User).unwrap());
        let dumped = lib.user_lib_text();
        // Re-parse the serialized text and compare the routines to the originals.
        let reparsed = parse_lib(&dumped, LibSource::User).unwrap();
        assert_eq!(reparsed.len(), 2);
        let mut lib2 = Library::new();
        lib2.set_user(reparsed);
        for name in ["GREET", "CLASSIFY"] {
            let a = lib.get(name).unwrap();
            let b = lib2.get(name).unwrap();
            assert_eq!(a.body, b.body, "{name} body");
            assert_eq!(a.doc, b.doc, "{name} doc");
            assert_eq!(a.body_src, b.body_src, "{name} body_src");
        }
        assert!(dumped.contains("# DOC: says hi"), "DOC line preserved");
    }

    #[test]
    fn body_control_flow_parses() {
        let src = "SUB classify\nIF $1 > 0\nRETURN positive\nELSE\nRETURN nonpositive\nENDIF\nENDSUB";
        let rs = parse_lib(src, LibSource::User).unwrap();
        assert_eq!(rs[0].body.len(), 1); // a single If statement
    }

    #[test]
    fn errors_are_atomic() {
        assert!(parse_lib("PR top-level", LibSource::User).is_err());
        assert!(parse_lib("SUB a\nPR x", LibSource::User).is_err()); // unclosed
        assert!(parse_lib("ENDSUB", LibSource::User).is_err()); // stray
        assert!(parse_lib("SUB a\nSUB b\nENDSUB\nENDSUB", LibSource::User).is_err()); // nested
        assert!(parse_lib("SUB a\nPR x\nENDSUB\nSUB a\nPR y\nENDSUB", LibSource::User).is_err()); // dup
    }

    #[test]
    fn shadow_rejection() {
        let src = "SUB IF\nPR x\nENDSUB\nSUB OS\nPR y\nENDSUB\nSUB ok\nPR z\nENDSUB\nSUB PR\nPR w\nENDSUB";
        let rs = parse_lib(src, LibSource::User).unwrap();
        let (accepted, rejected) = validate_names(rs);
        assert_eq!(names(&accepted), vec!["OK"]);
        assert_eq!(rejected.len(), 3); // IF (keyword), OS (special), PR (verb)
    }

    #[test]
    fn listing_shows_every_doc_line() {
        let src = "SUB r\n# DOC: first line\n# DOC: second line\nPR x\nENDSUB";
        let rs = parse_lib(src, LibSource::User).unwrap();
        let (accepted, _) = validate_names(rs);
        let mut lib = Library::new();
        lib.set_user(accepted);
        let listing = lib.listing().join("\n");
        assert!(listing.contains("R [USER] -- first line"), "{listing}");
        assert!(listing.contains("\n    second line"), "second DOC line indented: {listing}");
    }

    #[test]
    fn listing_columns_splits_name_and_doc_for_the_grid() {
        let src = "SUB r\n# DOC: first line\n# DOC: second line\nPR x\nENDSUB\nSUB bare\nPR y\nENDSUB";
        let rs = parse_lib(src, LibSource::User).unwrap();
        let (accepted, _) = validate_names(rs);
        let mut lib = Library::new();
        lib.set_user(accepted);
        let cols = lib.listing_columns();
        let r = cols.iter().find(|(n, _)| n.starts_with("R ")).expect("R row");
        assert_eq!(r.0, "R [USER]");
        assert_eq!(r.1, "first line\nsecond line", "docs joined with newlines, not indented");
        let bare = cols.iter().find(|(n, _)| n.starts_with("BARE")).expect("BARE row");
        assert_eq!(bare.1, "(no docs)", "a routine with no DOC lines shows (no docs)");
    }

    #[test]
    fn routine_that_fails_to_compile_is_rejected_not_poisoning_the_rest() {
        // A routine that PARSES but won't LOWER (here: GOTO to a missing label, the real-world
        // `delar` bug) must be skipped — the good routines still load.
        let src = "SUB bad\nGOTO :nope\nENDSUB\nSUB good\nPR ok\nENDSUB";
        let rs = parse_lib(src, LibSource::User).unwrap();
        let (accepted, rejected) = validate_names(rs);
        assert_eq!(names(&accepted), vec!["GOOD"], "the good routine still loads");
        assert_eq!(rejected.len(), 1);
        assert!(rejected[0].contains("BAD") && rejected[0].contains("unknown label"), "{:?}", rejected[0]);
    }

    #[test]
    fn shadow_rejection_covers_dispatched_verbs() {
        // Verbs dispatched in vm.rs's exec_command must be listed in BUILTIN_VERBS so a
        // same-named routine is rejected (else it parses but is unreachable). Spot-check
        // the groups added in the 2026-06-13 audit.
        for verb in [
            "FGCOLOR", "BGCOLOR", "UIFGCOLOR", "UIBGCOLOR", "SMB", "TGC", "SLN", "UISCALE",
            "EDFONT", "OF", "UIRESET", "ENCODING", "HIGHLIGHT", "TOPDF", "PRINT", "LDTX",
            "LDSC", "LDPM",
        ] {
            let src = format!("SUB {verb}\nPR x\nENDSUB");
            let rs = parse_lib(&src, LibSource::User).unwrap();
            let (accepted, rejected) = validate_names(rs);
            assert!(accepted.is_empty(), "{verb} should be rejected, not accepted");
            assert_eq!(rejected.len(), 1, "{verb} should produce one rejection");
        }
    }

    #[test]
    fn library_user_overrides_system() {
        let mut lib = Library::new();
        lib.set_system(parse_lib("SUB f\nPR system\nENDSUB", LibSource::System).unwrap());
        lib.set_user(parse_lib("SUB f\nPR user\nENDSUB", LibSource::User).unwrap());
        // user wins
        assert_eq!(lib.get("f").unwrap().source, LibSource::User);
        // effective() lists the user version, not the system one.
        let eff = lib.effective();
        assert_eq!(eff.len(), 1);
        assert_eq!(eff[0].0, "F");
    }

    #[test]
    fn merge_and_unload() {
        let mut lib = Library::new();
        lib.set_user(parse_lib("SUB a\nPR a\nENDSUB", LibSource::User).unwrap());
        lib.merge_user(parse_lib("SUB b\nPR b\nENDSUB\nSUB a\nPR a2\nENDSUB", LibSource::User).unwrap());
        assert_eq!(lib.user_count(), 2); // a (overridden) + b
        assert_eq!(lib.unload_user(), 2);
        assert_eq!(lib.user_count(), 0);
    }
}
