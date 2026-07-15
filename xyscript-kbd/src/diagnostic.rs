//! Live interception diagnostic — the §12 first experiment, made runnable.
//!
//! Installs the host's non-exclusive hook and prints, for every key edge, the
//! native code, the canonical code, the phase, the live bitmask, and the
//! disposition (swallow / dispatch / flip). Pressing the historically-difficult
//! keys (Caps Lock, function keys, Numpad 5) and watching clean, distinguishable
//! down/up lines appear is the proof the design's load-bearing assumption holds
//! on that platform.

use crate::event::KeyPhase;
use crate::normalize::host_table;
use crate::pipeline::{Capture, Pipeline};
use crate::resolver::{Disposition, Resolver};
use crate::Keymap;

fn phase_str(p: KeyPhase) -> &'static str {
    match p {
        KeyPhase::Down => "DOWN",
        KeyPhase::Up => "UP  ",
        KeyPhase::Inject => "INJ ",
    }
}

fn disp_str(d: &Disposition) -> String {
    match d {
        Disposition::Swallow => "swallow".into(),
        Disposition::Flip => "FLIP->OS".into(),
        Disposition::Dispatch(s) => format!("dispatch {:?}", truncate(s, 40)),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n).collect();
        t.push('…');
        t
    }
}

/// Run the live diagnostic with the given operative keymap.
pub fn run(keymap: Keymap) -> std::io::Result<()> {
    let virtual_base = keymap.virtual_base;
    let modifier_keys: Vec<u8> = keymap
        .modifiers
        .iter()
        .flat_map(|m| m.trigger_keys.clone())
        .collect();

    eprintln!("XyScript keyboard diagnostic (§12). Press keys; Ctrl-C to quit.");
    eprintln!(
        "Modifiers: {}",
        keymap
            .modifiers
            .iter()
            .map(|m| format!("{}=bit{} keys{:?}{}", m.name, m.bit, m.trigger_keys,
                if m.dual_role { "*dual" } else { "" }))
            .collect::<Vec<_>>()
            .join("  ")
    );
    eprintln!("{:<6} {:<9} {:<5} {:<8} {:<10} disposition", "native", "canonical", "phase", "bitmask", "capture");

    let resolver = Resolver::new(keymap);
    let table = host_table(virtual_base);
    let mut pipe = Pipeline::new(resolver, table);

    let mut handler = |raw: crate::platform::RawEvent| -> Capture {
        let r = pipe.on_raw(raw.native, raw.phase);
        let canon = r
            .canonical
            .map(|c| {
                let tag = if modifier_keys.contains(&c) { "*MOD" } else { "" };
                format!("{c}{tag}")
            })
            .unwrap_or_else(|| "—(unmapped)".into());
        let cap = match r.capture {
            Capture::Consume => "consume",
            Capture::PassThrough => "passthru",
        };
        println!(
            "{:<6} {:<9} {:<5} {:<8} {:<10} {}",
            r.native,
            canon,
            phase_str(r.phase),
            r.bitmask,
            cap,
            disp_str(&r.disposition)
        );
        r.capture
    };

    crate::platform::run_capture(&mut handler)
}
