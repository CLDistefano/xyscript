//! `xykbd` — the prototype CLI.
//!
//! Subcommands:
//!   check  [--kbd PATH]   Parse + validate a KBD (or the embedded default) and
//!                         print a summary. Headless; runs anywhere.
//!   demo   [--kbd PATH]   Feed a scripted sequence of synthetic key edges through
//!                         the real resolver and print dispositions. Headless;
//!                         exercises strict lookup, dual-role tap, and KF without
//!                         any OS hook — useful where there is no keyboard.
//!   diag   [--kbd PATH]   Live §12 interception diagnostic: install the host's
//!                         non-exclusive hook and print every key edge.
//!   run    [--kbd PATH]   Live: install the hook and "dispatch" assignments
//!                         (printed as a stand-in for the XPL interpreter).

use std::process::ExitCode;
use xyscript_kbd::event::KeyEvent;
use xyscript_kbd::resolver::{Disposition, Resolver};
use xyscript_kbd::{diagnostic, embedded_default, load, Keymap, LoadOutcome};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("help");
    let kbd_path = flag(&args, "--kbd");

    let keymap = match kbd_path {
        Some(p) => match std::fs::read_to_string(&p) {
            Ok(body) => match load(&body, Some(120)) {
                LoadOutcome::Installed(km) => {
                    eprintln!("Loaded KBD: {p}");
                    km
                }
                LoadOutcome::Rejected(e) => {
                    // Visible error; keep the embedded default (§9.2/§9.3).
                    eprintln!("LOADKBD failed: {e}\nFalling back to embedded default.");
                    embedded_default()
                }
            },
            Err(e) => {
                eprintln!("cannot read {p}: {e}\nFalling back to embedded default.");
                embedded_default()
            }
        },
        None => embedded_default(),
    };

    match cmd {
        "check" => {
            print_summary(&keymap);
            ExitCode::SUCCESS
        }
        "demo" => {
            run_demo(keymap);
            ExitCode::SUCCESS
        }
        "diag" => match diagnostic::run(keymap) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("diagnostic unavailable: {e}");
                ExitCode::FAILURE
            }
        },
        "run" => run_live(keymap),
        _ => {
            eprintln!(
                "usage: xykbd <check|demo|diag|run> [--kbd PATH]\n\
                 \n  check  parse+validate and summarize (headless)\
                 \n  demo   scripted synthetic events through the resolver (headless)\
                 \n  diag   live interception diagnostic (the §12 experiment)\
                 \n  run    live: dispatch assignments to a stub interpreter"
            );
            ExitCode::SUCCESS
        }
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn print_summary(km: &Keymap) {
    println!("KEYS (max code): {}", km.keys_max);
    println!("virtual band base: {:?}", km.virtual_base);
    println!("modifiers ({}):", km.modifiers.len());
    for m in &km.modifiers {
        println!(
            "  {:<8} bit={:<5} behavior={:?} triggers={:?}{}",
            m.name,
            m.bit,
            m.behavior,
            m.trigger_keys,
            if m.dual_role { "  [dual-role]" } else { "" }
        );
    }
    // Distinct layers (bitmasks) and total cells.
    let mut masks: Vec<u32> = km.tables.keys().map(|(b, _)| *b).collect();
    masks.sort_unstable();
    masks.dedup();
    let flips = km.tables.values().filter(|a| matches!(a, xyscript_kbd::Assignment::Flip)).count();
    println!("layers (distinct bitmasks): {}", masks.len());
    println!("total cells: {}   KF cells: {}", km.tables.len(), flips);
}

/// Stand-in for the XPL interpreter.
fn dispatch(assignment: &str) {
    println!("    -> INTERPRETER dispatch: {assignment:?}");
}

fn show(label: &str, d: &Disposition) {
    match d {
        Disposition::Swallow => println!("  {label:<28} => swallow"),
        Disposition::Flip => println!("  {label:<28} => FLIP (release to OS)"),
        Disposition::Dispatch(s) => {
            println!("  {label:<28} => dispatch");
            dispatch(s);
        }
    }
}

/// Scripted, hook-free exercise of the resolver (headless).
fn run_demo(keymap: Keymap) {
    println!("== scripted resolver demo ==");
    print_summary(&keymap);
    println!();

    // Identify a couple of useful codes from the loaded map.
    let shift_key = keymap
        .modifiers
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case("SHIFT"))
        .and_then(|m| m.trigger_keys.first().copied());
    let dual = keymap.modifiers.iter().find(|m| m.dual_role).cloned();

    let mut r = Resolver::new(keymap);

    // 1. Plain letter 'q' (code 16) unshifted.
    show("press q (16) unshifted", &r.resolve(KeyEvent::down(16)));
    let _ = r.resolve(KeyEvent::up(16));

    // 2. Shift held + 16 => the SHIFT layer.
    if let Some(sk) = shift_key {
        let _ = r.resolve(KeyEvent::down(sk)); // shift down (swallowed)
        show("Shift+16 (shifted layer)", &r.resolve(KeyEvent::down(16)));
        let _ = r.resolve(KeyEvent::up(16));
        let _ = r.resolve(KeyEvent::up(sk)); // shift up
    }

    // 3. Dual-role key: tap alone (fires tap action) vs. chord (suppressed).
    if let Some(d) = dual {
        let mk = d.trigger_keys[0];
        println!("  -- dual-role key {} ({}) --", d.name, mk);
        // Tap alone: down then up with no chord => tap fires on release.
        let _ = r.resolve(KeyEvent::down(mk));
        show("tap alone (release)", &r.resolve(KeyEvent::up(mk)));
        // Chord: hold modifier, press another key, release => tap suppressed.
        let _ = r.resolve(KeyEvent::down(mk));
        show("  chord: modifier+16", &r.resolve(KeyEvent::down(16)));
        let _ = r.resolve(KeyEvent::up(16));
        show("  release modifier (suppressed)", &r.resolve(KeyEvent::up(mk)));
    }

    // 4. A miss (no such cell) is swallowed (strict lookup).
    show("press 200 (no such cell)", &r.resolve(KeyEvent::down(200u8)));
}

fn run_live(keymap: Keymap) -> ExitCode {
    use xyscript_kbd::normalize::host_table;
    use xyscript_kbd::pipeline::{Capture, Pipeline};
    let vb = keymap.virtual_base;
    let resolver = Resolver::new(keymap);
    let mut pipe = Pipeline::new(resolver, host_table(vb));
    let mut handler = |raw: xyscript_kbd::platform::RawEvent| -> Capture {
        let r = pipe.on_raw(raw.native, raw.phase);
        if let Disposition::Dispatch(s) = &r.disposition {
            dispatch(s);
        }
        r.capture
    };
    match xyscript_kbd::platform::run_capture(&mut handler) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("run unavailable: {e}");
            ExitCode::FAILURE
        }
    }
}
