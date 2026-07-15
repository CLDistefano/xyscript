//! Transactional loading and the embedded default keymap (§9).
//!
//! `LOADKBD` is all-or-nothing: parse + validate into a candidate, then either
//! atomically swap it in (success) or discard it and keep the operative keymap
//! (any error). The operative keymap *is* the last-known-good, by construction —
//! no separate copy is kept (§9.3).

use crate::model::*;
use crate::parser;
use std::collections::HashMap;

/// Build the embedded default keymap entirely in code (§9.1) — it never passes
/// through the file parser, so it is the one keymap guaranteed always valid. It
/// is the floor a user KBD upgrades over.
///
/// The default is intentionally tiny but **self-sufficient** (§9.1): it can type
/// the basics, it can reach `LOADKBD` (here bound to a key as a dispatched XPL
/// command), and it `KF`s a baseline of OS keys so the user is never locked out
/// of the operating system even before customizing a KBD (§6.1).
pub fn embedded_default() -> Keymap {
    let mut tables: HashMap<(Bitmask, KeyCode), Assignment> = HashMap::new();

    // Standard modifiers (Shift/Ctrl/Alt/Caps) with the classic doubling bits.
    // Bottom-up: CTRL=256, ALT=512, SHIFT=1024, CAPS=2048.
    let modifiers = vec![
        ModifierDef { name: "CAPS".into(),  bit: 2048, trigger_keys: vec![58],     behavior: ModBehavior::Toggle, dual_role: false },
        ModifierDef { name: "SHIFT".into(), bit: 1024, trigger_keys: vec![42, 54], behavior: ModBehavior::Hold,   dual_role: false },
        ModifierDef { name: "ALT".into(),   bit: 512,  trigger_keys: vec![56, 98], behavior: ModBehavior::Hold,   dual_role: false },
        ModifierDef { name: "CTRL".into(),  bit: 256,  trigger_keys: vec![29, 99], behavior: ModBehavior::Hold,   dual_role: false },
    ];

    // Minimal unshifted typing: letters a..z on their classic codes, space, enter.
    let unshifted: &[(KeyCode, &str)] = &[
        (16, "q"), (17, "w"), (18, "e"), (19, "r"), (20, "t"), (21, "y"),
        (22, "u"), (23, "i"), (24, "o"), (25, "p"),
        (30, "a"), (31, "s"), (32, "d"), (33, "f"), (34, "g"), (35, "h"),
        (36, "j"), (37, "k"), (38, "l"),
        (44, "z"), (45, "x"), (46, "c"), (47, "v"), (48, "b"), (49, "n"), (50, "m"),
        (57, " "),
    ];
    for &(k, v) in unshifted {
        tables.insert((0, k), Assignment::Xpl(v.into()));
    }
    // A way to reach LOADKBD from the bare default (§9.1): Ctrl+L dispatches the
    // XPL command that opens the loader. There MUST be a path back to the load
    // command; a test asserts it (see tests/).
    tables.insert((256, 38), Assignment::Xpl("LOADKBD".into())); // Ctrl + L(38)

    // KF a baseline of OS keys so the user keeps the operating system (§6.1) —
    // Esc(1), Backspace(14), Tab(15). Because lookup is strict with NO fallthrough
    // (§6), a cell only exists in the layer it's written to. So we replicate the
    // baseline into EVERY reachable layer (all subsets of the modifier bits);
    // otherwise latching Caps or holding a modifier would leave the user with no
    // OS escape hatch. `insert` here runs before the letter/LOADKBD cells above
    // would matter, and the baseline codes never collide with them.
    const OS_BASELINE: [KeyCode; 3] = [1, 14, 15];
    for mask in all_layer_masks(&modifiers) {
        for k in OS_BASELINE {
            tables.entry((mask, k)).or_insert(Assignment::Flip);
        }
    }

    Keymap { modifiers, tables, keys_max: 104, virtual_base: Some(120) }
}

/// Every reachable layer bitmask: all 2^n OR-combinations of the modifier bits.
/// Used to replicate a self-contained baseline across layers under strict lookup.
fn all_layer_masks(modifiers: &[ModifierDef]) -> Vec<Bitmask> {
    let mut masks = vec![0];
    for m in modifiers {
        let mut more = Vec::with_capacity(masks.len());
        for &existing in &masks {
            more.push(existing | m.bit);
        }
        masks.extend(more);
    }
    masks
}

/// Outcome of a `LOADKBD` attempt.
pub enum LoadOutcome {
    /// The candidate was installed; this is the new operative keymap.
    Installed(Keymap),
    /// The load failed; the operative keymap is unchanged. Carries the visible
    /// error to surface to the user (§9.2).
    Rejected(parser::ParseError),
}

/// Parse + validate a KBD file body into a candidate. On success the caller
/// installs it (atomic swap); on failure the caller keeps the current keymap and
/// surfaces the error (§9.2, §9.3).
pub fn load(body: &str, virtual_base: Option<KeyCode>) -> LoadOutcome {
    match parser::parse(body, virtual_base) {
        Ok(km) => LoadOutcome::Installed(km),
        Err(e) => LoadOutcome::Rejected(e),
    }
}
