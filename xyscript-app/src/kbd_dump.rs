//! Serialize an operative [`Keymap`] back to KBD-file text (`SAVEKBD`).
//!
//! The inverse of the loader (+ a light re-sugar of typing cells): it walks the
//! resolved keymap and emits a `.kbd` that re-loads to an equivalent layout. The
//! operative keymap is already **desugared** (D4: `16=q` → `insert "q"`), so a
//! single-alphanumeric `insert "X"` is re-sugared back to the terse bare form;
//! every other command is emitted verbatim (always valid, just less terse).
//!
//! Layers are grouped by bitmask and named from the modifier set, so a
//! `TABLE=A,B` shorthand in the source round-trips as two separate `TABLE=A` /
//! `TABLE=B` blocks — more verbose, same result.

use xyscript_kbd::{Assignment, Bitmask, KeyCode, Keymap, ModBehavior};

/// Render `km` as KBD-file text (`SAVEKBD`).
pub fn dump_keymap(km: &Keymap) -> String {
    let mut out = String::new();
    out.push_str(";KB;; Dumped by SAVEKBD — the currently-loaded keymap, as a runnable KBD.\n");
    out.push_str(";;;;; Re-loads with LOADKBD to an equivalent layout. Typing cells are shown in\n");
    out.push_str(";;;;; the terse form where possible; other cells are the literal XPL commands.\n");
    out.push_str(&format!("KEYS={}\n", km.keys_max));

    // Modifier declarations, in the keymap's own order (bit assignment is re-derived on load).
    for m in &km.modifiers {
        let keys =
            m.trigger_keys.iter().map(|k| k.to_string()).collect::<Vec<_>>().join(",");
        match m.behavior {
            ModBehavior::Toggle => out.push_str(&format!("{}={keys},T:C\n", m.name)),
            ModBehavior::Hold => out.push_str(&format!("{}={keys}\n", m.name)),
        }
    }
    out.push('\n');

    // Distinct layer bitmasks present in the tables, sorted (0 / unshifted first).
    let mut masks: Vec<Bitmask> = km.tables.keys().map(|(b, _)| *b).collect();
    masks.sort_unstable();
    masks.dedup();

    // A modifier with no cells in any layer would be dropped on reload (the loader only treats a
    // declaration as a modifier if a TABLE references it). Emit an empty TABLE for it to preserve.
    for m in &km.modifiers {
        if !masks.iter().any(|mask| mask & m.bit != 0) {
            masks.push(m.bit);
        }
    }
    masks.sort_unstable();
    masks.dedup();

    for mask in masks {
        out.push_str(&format!("TABLE={}\n", layer_name(km, mask)));
        let mut cells: Vec<(KeyCode, &Assignment)> = km
            .tables
            .iter()
            .filter(|((b, _), _)| *b == mask)
            .map(|((_, k), a)| (*k, a))
            .collect();
        cells.sort_by_key(|(k, _)| *k);
        for (code, a) in cells {
            out.push_str(&format!("{code}={}\n", cell_value(a)));
        }
        out.push('\n');
    }
    out
}

/// Reconstruct a `TABLE=` combo name from a bitmask: the OR'd modifier names joined by `+`
/// (empty string for the unshifted layer, mask 0).
fn layer_name(km: &Keymap, mask: Bitmask) -> String {
    km.modifiers
        .iter()
        .filter(|m| mask & m.bit != 0)
        .map(|m| m.name.as_str())
        .collect::<Vec<_>>()
        .join("+")
}

/// A cell's right-hand side: `KF` for a flip; for an XPL cell, the terse bare char when it's a
/// re-sugarable single-alphanumeric `insert "X"`, else the command verbatim.
fn cell_value(a: &Assignment) -> String {
    match a {
        Assignment::Flip => "KF".to_string(),
        Assignment::Xpl(s) => resugar(s),
    }
}

/// `insert "X"` where X is a single alphanumeric scalar → `X` (the terse typing form the loader
/// re-desugars). Anything else (snippets, space/punctuation, real commands) is left as-is — always
/// valid, just not terse.
fn resugar(s: &str) -> String {
    if let Some(inner) = s.strip_prefix("insert \"").and_then(|r| r.strip_suffix('"')) {
        let mut it = inner.chars();
        if let Some(c) = it.next() {
            if it.next().is_none() && c.is_alphanumeric() {
                return c.to_string();
            }
        }
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::dump_keymap;
    use xyscript_kbd::{load, Disposition, LoadOutcome, Resolver};

    fn loads(body: &str) -> xyscript_kbd::Keymap {
        match load(body, Some(120)) {
            LoadOutcome::Installed(km) => km,
            LoadOutcome::Rejected(e) => panic!("kbd failed to load: {e:?}"),
        }
    }

    #[test]
    fn dump_round_trips_an_equivalent_keymap() {
        // A small layout: unshifted typing + a command, a Shift layer, a Ctrl command, and KF.
        let src = "KEYS=104\nSHIFT=42,54\nCTRL=29\nCAPS=58,T:C\n\
                   TABLE=\n1=KF\n16=q\n14=BD\n\
                   TABLE=SHIFT\n16=Q\n\
                   TABLE=CTRL\n38=LOADKBD\n";
        let km = loads(src);
        // Re-load the dump and confirm key resolutions match.
        let dumped = dump_keymap(&km);
        let km2 = loads(&dumped);
        let mut r1 = Resolver::new(km);
        let mut r2 = Resolver::new(km2);
        let shift = r2.keymap.bitmask_for_names(&["SHIFT"]).unwrap();
        let ctrl = r2.keymap.bitmask_for_names(&["CTRL"]).unwrap();
        use xyscript_kbd::event::KeyEvent;
        // Unshifted 'q' typing cell survives as the terse bare form.
        assert!(dumped.contains("16=q"));
        assert_eq!(r1.resolve(KeyEvent::down(16)), r2.resolve(KeyEvent::down(16)));
        // KF and a command.
        assert_eq!(r1.resolve(KeyEvent::down(1)), Disposition::Flip);
        assert_eq!(r2.resolve(KeyEvent::down(1)), Disposition::Flip);
        assert_eq!(
            r2.resolve(KeyEvent::inject(38, ctrl)),
            Disposition::Dispatch("LOADKBD".into())
        );
        // Shift layer.
        assert_eq!(
            r2.resolve(KeyEvent::inject(16, shift)),
            Disposition::Dispatch("Q".into())
        );
    }
}
