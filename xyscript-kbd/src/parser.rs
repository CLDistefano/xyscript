//! KBD file parser (§3, §9.2).
//!
//! Parsing is the first half of the *transactional* load: it produces a
//! **candidate** `Keymap` or a `ParseError`. The caller (the loader) only swaps
//! a candidate in on success, so a half-parsed file can never reach the runtime
//! (§9.2). The parser does not touch any live state.
//!
//! Format recap (§3):
//!   * Header lines, one declaration each. A line whose first non-space char is
//!     `;` is a comment. Note `;` is *not* a mid-line comment marker — it can be
//!     a literal assignment (e.g. `39=;`), so table values are taken verbatim
//!     after the first `=`.
//!   * `KEYS=<n>` declares the addressable range.
//!   * `<NAME>=<csv key codes>[,T:<attr>]` declares a shifting key or special
//!     key. Whether it is a *modifier* (gets a bit) is decided by whether any
//!     `TABLE=` references its name (§3.1) — that is how `HOTKEY` is a modifier
//!     but `MOUSE` is merely a preserved special key.
//!   * Bit assignment is bottom-up over the modifier list (§2): the last modifier
//!     declared gets 256, the next-from-bottom 512, doubling upward.
//!   * `TABLE=<combo>[,<combo>...]` introduces a layer; the empty name is the
//!     unshifted layer (bitmask 0). Each combo is `+`-joined modifier names in
//!     any order. One table may serve several bitmasks.

use crate::model::*;
use std::collections::HashMap;

/// The on-disk base for the lowest modifier bit (§2). The classic scheme packs
/// modifier bits above the key-code range; 256 sits above the 1..=104 classic
/// keys (and below 256-keyed cells), so codes and bits never collide.
const MOD_BIT_BASE: Bitmask = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub msg: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "KBD parse error (line {}): {}", self.line, self.msg)
    }
}
impl std::error::Error for ParseError {}

fn err(line: usize, msg: impl Into<String>) -> ParseError {
    ParseError { line, msg: msg.into() }
}

/// A header declaration before we know whether it is a modifier or a plain
/// special key. We keep header order so bit assignment can run bottom-up.
struct Decl {
    name: String,
    trigger_keys: Vec<KeyCode>,
    behavior: ModBehavior,
}

/// Parse a whole KBD file body into a candidate `Keymap`.
///
/// `virtual_base` is the editor's policy (not in the file): codes `>=` it are the
/// reserved virtual band (§3.5). Pass `None` for files like `CLD4R.KBD` that use
/// no virtual band.
/// A modifier combo expressed as alternative name-lists (e.g. `[[Ctrl],[Shift]]`).
type ComboNames = Vec<Vec<String>>;
/// One captured table row: `(combo-index, key, raw assignment)`.
type RawRow = (usize, KeyCode, String);
/// A captured `TABLE` block before combos are resolved to bitmasks.
type RawTable = (ComboNames, Vec<RawRow>);

pub fn parse(input: &str, virtual_base: Option<KeyCode>) -> Result<Keymap, ParseError> {
    let mut keys_max: Option<KeyCode> = None;
    let mut decls: Vec<Decl> = Vec::new();
    // Raw table rows captured as (combo names, key, raw assignment) so we can
    // resolve combos to bitmasks only after every modifier name is known.
    let mut raw_tables: Vec<RawTable> = Vec::new();
    let mut current_combos: Option<ComboNames> = None;
    let mut current_rows: Vec<RawRow> = Vec::new();
    let mut in_tables = false;

    for (i, raw_line) in input.lines().enumerate() {
        let lineno = i + 1;
        let trimmed = raw_line.trim_start();
        // Full-line comment or blank.
        if trimmed.is_empty() || trimmed.starts_with(';') {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("TABLE=") {
            in_tables = true;
            // Flush the previous table.
            if let Some(combos) = current_combos.take() {
                raw_tables.push((combos, std::mem::take(&mut current_rows)));
            }
            // The TABLE= line may itself carry a trailing comment via the legacy
            // bullet marker; combos are everything up to the first ';' (none) —
            // we only split on commas and '+'. Trailing inline notes after a
            // space + non-name token are tolerated by name lookup later.
            let combos = parse_table_header(rest);
            current_combos = Some(combos);
            continue;
        }

        // A KBDDOC-annotated table row carries a `<layer> <keyname>  :` documentation column before the
        // key code (`unmod Esc  :1=KF`). Strip it so the `key=value` split below sees the real content:
        // the annotation can itself contain `=` (the `=` key names itself, `unmod =  :13==`), so we
        // can't just split on the first `=`. A plain row starts with its digit key code and is untouched.
        let line = if in_tables && !trimmed.starts_with(|c: char| c.is_ascii_digit()) {
            strip_kbddoc_annotation(raw_line)
        } else {
            raw_line
        };
        // Split on the FIRST '=' only; the remainder is verbatim.
        let eq = match line.find('=') {
            Some(p) => p,
            None => return Err(err(lineno, format!("expected `key=value`: {raw_line:?}"))),
        };
        let key_part = line[..eq].trim();
        let value = &line[eq + 1..];

        if !in_tables {
            // Header region.
            if key_part.eq_ignore_ascii_case("KEYS") {
                keys_max = Some(parse_u8(value.trim(), lineno, "KEYS")?);
            } else {
                // A modifier-or-special declaration: NAME = csv-of-keycodes [,T:x]
                let mut trigger_keys = Vec::new();
                let mut behavior = ModBehavior::Hold;
                for tok in value.split(',') {
                    let tok = tok.trim();
                    if tok.is_empty() {
                        continue;
                    }
                    if let Some(attr) = tok.strip_prefix("T:").or_else(|| tok.strip_prefix("t:")) {
                        if attr.eq_ignore_ascii_case("C") {
                            behavior = ModBehavior::Toggle;
                        }
                        // Unknown attributes are preserved-by-ignoring (§3.1).
                        continue;
                    }
                    // Otherwise it should be a key code.
                    trigger_keys.push(parse_u8(tok, lineno, "trigger key")?);
                }
                decls.push(Decl { name: key_part.to_string(), trigger_keys, behavior });
            }
            continue;
        }

        // Table row (annotation, if any, already stripped above).
        let key = parse_u8(key_part, lineno, "key code")?;
        current_rows.push((lineno, key, value.to_string()));
    }
    // Flush the final table.
    if let Some(combos) = current_combos.take() {
        raw_tables.push((combos, current_rows));
    }

    let keys_max = keys_max.ok_or_else(|| err(0, "missing required KEYS= declaration"))?;

    // ----- Decide which declarations are modifiers (referenced by a TABLE) ----
    let mut referenced: Vec<String> = Vec::new();
    for (combos, _) in &raw_tables {
        for combo in combos {
            for name in combo {
                if !referenced.iter().any(|r| r.eq_ignore_ascii_case(name)) {
                    referenced.push(name.clone());
                }
            }
        }
    }
    // A declaration is a modifier iff its name is referenced by some table.
    // Preserve header order for bit assignment.
    let modifier_indices: Vec<usize> = decls
        .iter()
        .enumerate()
        .filter(|(_, d)| referenced.iter().any(|r| r.eq_ignore_ascii_case(&d.name)))
        .map(|(i, _)| i)
        .collect();

    // Every referenced name must have a header declaration (§9.2: modifiers resolve).
    for name in &referenced {
        if !decls.iter().any(|d| d.name.eq_ignore_ascii_case(name)) {
            return Err(err(0, format!("TABLE references undeclared modifier `{name}`")));
        }
    }

    // ----- Assign bits bottom-up (§2): last modifier -> 256, doubling upward ---
    let mut modifiers: Vec<ModifierDef> = Vec::new();
    let n = modifier_indices.len();
    for (rank, &idx) in modifier_indices.iter().enumerate() {
        // rank 0 is the first (top) modifier; the LAST gets MOD_BIT_BASE.
        let from_bottom = n - 1 - rank;
        let bit = MOD_BIT_BASE << from_bottom as u32;
        let d = &decls[idx];
        modifiers.push(ModifierDef {
            name: d.name.clone(),
            bit,
            trigger_keys: d.trigger_keys.clone(),
            behavior: d.behavior,
            dual_role: false, // filled in after tables are built
        });
    }

    // ----- Build the strict lookup table -------------------------------------
    let mut tables: HashMap<(Bitmask, KeyCode), Assignment> = HashMap::new();
    for (combos, rows) in &raw_tables {
        // Each listed combo points at the SAME assignment set (§3.2).
        let mut masks: Vec<Bitmask> = Vec::new();
        for combo in combos {
            // Empty combo (the bare `TABLE=`) is the unshifted layer, mask 0.
            let lineno = rows.first().map(|r| r.0).unwrap_or(0);
            masks.push(combo_to_mask(&modifiers, combo, lineno)?);
        }
        for (lineno, key, raw) in rows {
            if *key == 0 || *key > keys_max {
                return Err(err(
                    *lineno,
                    format!("key code {key} out of range 1..={keys_max} (KEYS=)"),
                ));
            }
            let assignment = Assignment::parse(raw);
            for &mask in &masks {
                tables.insert((mask, *key), assignment.clone());
            }
        }
    }

    // ----- Mark dual-role modifiers (§5) -------------------------------------
    // A modifier is dual-role if any of its trigger keys also appears as a cell
    // key in any layer (i.e. it has a tap action somewhere).
    let keyed: std::collections::HashSet<KeyCode> = tables.keys().map(|(_, k)| *k).collect();
    for m in modifiers.iter_mut() {
        m.dual_role = m.trigger_keys.iter().any(|k| keyed.contains(k));
    }

    Ok(Keymap { modifiers, tables, keys_max, virtual_base })
}

/// Resolve a combo (list of modifier names) to a summed bitmask (§3.2).
fn combo_to_mask(
    modifiers: &[ModifierDef],
    combo: &[String],
    lineno: usize,
) -> Result<Bitmask, ParseError> {
    let mut mask = 0;
    for name in combo {
        let m = modifiers
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| err(lineno, format!("unknown modifier `{name}` in TABLE combo")))?;
        mask |= m.bit;
    }
    Ok(mask)
}

/// Split a `TABLE=` header value into combos. `"ALT+CAPS,ALT"` ->
/// `[["ALT","CAPS"], ["ALT"]]`. An empty header yields one empty combo (mask 0).
fn parse_table_header(rest: &str) -> Vec<Vec<String>> {
    let rest = rest.trim();
    if rest.is_empty() {
        return vec![vec![]]; // unshifted layer
    }
    rest.split(',')
        .map(|combo| {
            combo
                .split('+')
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Strip a `KBDDOC` documentation column from a table-row line, returning the real `key=value` tail.
/// The column is `<layer> <keyname>  :` before the key code; its delimiter is the `:` immediately
/// followed by the decimal key code and `=` (`unmod Esc  :1=KF` → `1=KF`). Key/layer names never
/// contain `:`, and this anchors on `:<digits>=`, so it won't trip over a `:` inside a binding value.
/// Returns the whole line unchanged when no such column is present.
fn strip_kbddoc_annotation(line: &str) -> &str {
    for (i, _) in line.match_indices(':') {
        let after = &line[i + 1..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && after.as_bytes().get(digits) == Some(&b'=') {
            return after;
        }
    }
    line
}

fn parse_u8(s: &str, line: usize, what: &str) -> Result<u8, ParseError> {
    s.trim()
        .parse::<u8>()
        .map_err(|_| err(line, format!("invalid {what}: {s:?}")))
}
