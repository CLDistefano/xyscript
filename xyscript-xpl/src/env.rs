//! Variables, scoping, and special-variable identity (`stage1-interpreter-plan.md`
//! §10).
//!
//! - **Scope:** script-global by default. User variables live in [`Env::vars`],
//!   keyed by case-folded name, and persist across `run()` calls on one
//!   [`crate::vm::Interpreter`] (the app-global behavior QN has).
//! - **`LOCAL`** save/clear/restore is driven by the VM's call frames (see
//!   `vm.rs`); the storage it manipulates is this `Env`.
//! - **Special (read-only) variables** are *names* `SET` refuses to write. Their
//!   live values are served by the [`crate::host::Host`] (or computed by the VM,
//!   for `$ERROR`/`$#`/`$*`/`$RV`/positional args). Here we own only the *name
//!   set* and the write-protection check — the contract with scripts.

use crate::value::{fold_ident, Value};
use std::collections::HashMap;

/// The user-variable store plus the trappable-error flag.
#[derive(Default)]
pub struct Env {
    /// User variables, keyed by case-folded name.
    pub vars: HashMap<String, Value>,
    /// `$ERROR` — the trappable-error code. `0` = success/no error; any
    /// non-zero value is an [`crate::error::ErrCode`] (see `error.rs`).
    pub error: u32,
    /// Active `CAPTURE` buffers, innermost last (so captures nest). Each entry is
    /// `(folded var name, accumulated output)`. Lives here (not on the transient `Vm`) so a
    /// capture survives a suspend/resume or a sliced background run. Empty = not capturing.
    pub capture: Vec<(String, String)>,
}

impl Env {
    pub fn new() -> Env {
        Env::default()
    }

    /// Get a user variable by (unfolded) name.
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.vars.get(&fold_ident(name))
    }

    /// Set a user variable (folds the name).
    pub fn set(&mut self, name: &str, value: Value) {
        self.vars.insert(fold_ident(name), value);
    }

    /// Remove a user variable (folds the name).
    pub fn remove(&mut self, name: &str) -> Option<Value> {
        self.vars.remove(&fold_ident(name))
    }
}

/// The read-only **special** variable names (the contract from §10 / QN's
/// `_SPECIAL_VAR_NAMES`). `SET` to any of these is a fatal error. Live values
/// are served by the host or computed by the VM; in Stage 1 the host stub
/// returns placeholders for most. Names are stored folded (upper).
pub const SPECIAL_NAMES: &[&str] = &[
    // file/path (PA = synonym of CWD; ED = running executable's path; XSDIR = its directory;
    // DC = OS directory-separator char)
    "FI", "FP", "CWD", "PA", "ED", "XSDIR", "VE", "CF", "DC",
    // directory listing (DIR/LS): D1 = is-a-listing flag; DP = listing path + mask; DR = entry under cursor
    "D1", "DP", "DR",
    // cursor/offset
    "CP", "CX", "CY", "LN", "OF", "LI", "WD", "DOC", "CARETS", "CARETP", "MCP",
    "MSE", "MSL", "MDF", "MDS", "MDN", "MCS", "MCH", "MWD", "MLI", "MLN",
    // selection
    "SE", "SEL", "SL", "DF", "DS", "DN", "CS",
    // editor state/flags (EN = active document's encoding-view name)
    "FB", "FE", "RE", "MO", "EN", "FS", "LC", "TL", "WC", "CC", "CH", "TB", "GC", "MB",
    "AU", "PS", "VR", "TC", "CT", "TX", "DIRNW", "DIRWNW", "DIRWCLOSE", "DIRRT", "DIRSET", "LNS", "WS", "RO", "HLAUTO", "DECIMALS", "SW", "SH", "SHPROMPT", "ANSICOLOR", "AR", "TABSPACES", "TABINDENT", "INDENTED", "BALPAIRS", "BALPAIRSCOLOR", "TYPEPAIRS", "WRAPINDENT",
    "DOCTYPE", "PV", "TITLEBAR", "WV", "WINGEO", "SESSION",
    // lock keys (full names; $CL/$NL freed for other specials)
    "CAPSLOCK", "NUMLOCK", "SCROLLLOCK",
    // command line: CL = last issued command, CM = current content, CMH = full history (OS newlines)
    "CL", "CM", "CMH",
    // CFA = the AUTOCFG tracked-config path (empty if off); CF (above) = last LOADCFG'd path
    "CFA",
    // keyboard seam (SC = last scancode; KB = path of the last LOADKBD'd file)
    "SC", "SCC", "KB",
    // clock — `$DA`/`$TM` strings, `$ET`/`$ETS` stopwatch, and AutoIt-style component fields
    "DA", "TM", "ET", "ETS",
    "YEAR", "MON", "MDAY", "HOUR", "MIN", "SEC", "MSEC", "WDAY", "YDAY",
    // shell
    "RC", "LS", "TR", "XO", "JOBS", "LJ", "CALC", "RUNSUM",
    // language checking (live-on flags + dialect + underline styles + spell engine/dicts/binary)
    "SPELLING", "GRAMMAR", "DIALECT", "SPELLSTYLE", "GRAMMARSTYLE", "SPELLENGINE", "SPELLDICT", "SPELLBIN",
    // UI localization: path of the loaded LANG catalog (empty = built-in English)
    "LANG",
    // split panes: pane count + focused pane's 1-based position + active-pane border colour
    "PANES", "PANE", "PANECOLOR",
    // clipboard / deletion buffer
    "CB", "XB",
    // scratchpad: full text of the most-recently-closed scratchpad window (empty until one closes) +
    // the scratchpad window's fg/bg colours (empty = egui default)
    "SCRATCHTEXT", "SCRATCHFGCOLOR", "SCRATCHBGCOLOR",
    // host/UI
    "OS", "UIFONT", "UISCALE", "EDFONT", "EDFAMILY", "UIFAMILY", "TXTFAMILY", "EDSTYLE", "UISTYLE", "TXTSTYLE", "TXTFONT", "TXTSCALE", "LSP", "OFL", "OFR", "FGCOLOR", "BGCOLOR", "UIFGCOLOR",
    "UIBGCOLOR", "CRCOLOR", "CMCRCOLOR", "SEPCOLOR", "TABXCOLOR", "TXTFGCOLOR", "TXTBGCOLOR", "SELBGCOLOR", "SELFGCOLOR",
    "FINDBGCOLOR", "FINDFGCOLOR", "SPELLCOLOR", "GRAMMARCOLOR", "EMPHFGCOLOR", "EMPHBGCOLOR",
    "TABACTIVE", "TABHOVER", "TABINACTIVE", "CRSHAPE", "CMCRSHAPE", "CMPLACE", "STATUSLINE", "CMBLANK", "SHOWONETAB", "CARETMOTION", "UNTITLEDTAB", "GOTOOPEN", "ULSPACES", "WRAP", "HEX", "MARK", "MARKS", "MARKGUTTER", "MARKFGCOLOR", "MARKBGCOLOR", "LNFGCOLOR", "LNBGCOLOR", "MSGHFGCOLOR", "MSGBGCOLOR", "MSGOKFGCOLOR", "MSGOKBGCOLOR", "URLHIGHLIGHT", "URLFGCOLOR", "URLBGCOLOR", "AUTOREOPEN", "DIMONBLUR", "CLOSEBACK", "WRAPHANG", "REFLOWWIDTH", "PAPLACE", "PATHLEN", "PAFGCOLOR", "PABGCOLOR", "NEWLINE", "NL",
    // DIR/W listing window appearance.
    "DWFGCOLOR", "DWBGCOLOR", "DWFONT", "DWSCALE", "DWFAMILY", "DWSTYLE",
    // Markdown Preview / TOPDF fonts.
    "PVFAMILY", "PVSIZE", "PVCODE",
    "PVMARGIN", "PVMARGINL", "PVMARGINR", "PVMARGINT", "PVMARGINB",
    // counters C1..C9 — peek the current value (no advance), rendered without the decorator
    "C1", "C2", "C3", "C4", "C5", "C6", "C7", "C8", "C9",
    // email (send config + IMAP receive config; masked password reads "set"/"not set")
    "SMTPURL", "MAILUSER", "MAILFROM", "MAILPASS", "MAILDEBUG", "IMAPURL", "MAILBOX", "ATTACH",
    // error code + generic-message catalog ($ERRORMSG[n]); error-log on/off + file path
    "ERROR", "ERRORMSG", "ERRORLOG", "ERRORLOGFILE",
    // args / return
    "RV",
];

/// Is `name` a read-only special (so `SET`/`SV` must refuse it)? Also rejects
/// the positional-arg names (`*`, `#`, and pure digits) — those are managed by
/// the call frame, never assigned directly.
pub fn is_special_name(name: &str) -> bool {
    let f = fold_ident(name);
    if f == "*" || f == "#" {
        return true;
    }
    if !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()) {
        return true; // positional $1, $2, …
    }
    SPECIAL_NAMES.contains(&f.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_vars_fold_for_lookup() {
        let mut e = Env::new();
        e.set("Name", Value::str("Alice"));
        assert_eq!(e.get("NAME"), Some(&Value::str("Alice")));
        assert_eq!(e.get("name"), Some(&Value::str("Alice")));
    }

    #[test]
    fn specials_are_recognized() {
        assert!(is_special_name("ERROR"));
        assert!(is_special_name("se"));
        assert!(is_special_name("OS"));
        assert!(is_special_name("1")); // positional
        assert!(is_special_name("*"));
        assert!(!is_special_name("myvar"));
        assert!(!is_special_name("count"));
    }
}
