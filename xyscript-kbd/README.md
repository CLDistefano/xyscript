# xyscript-kbd — keyboard layer prototype

A Rust prototype of the XyScript keyboard layer described in
`../keyboard-layer-design.md`. It implements the data-driven `.KBD` modifier
model `(bitmask, key_code) -> assignment`, wires it end-to-end through
non-exclusive OS interception on all three target platforms, and is verified
against the real reference file `../CLD4R.KBD`.

This directly addresses the design's **§12 recommended first experiment**: prove
the app can intercept clean, distinguishable down/up events — for the keys users
are most likely to make modifiers (Caps Lock, function keys, Numpad 5) — using
non-exclusive capture so `KF` can still hand keys back to the OS.

## What's here

```
src/
  model.rs        Types: KeyCode(u8), Bitmask(u32), ModifierDef, Assignment, Keymap (§2,§3,§11)
  parser.rs       KBD parser: header, bottom-up bit assignment, combos, T:C, KF (§3,§9.2)
  input_state.rs  Held/latched bits + tap-vs-hold dual-role state machine (§4,§5)
  resolver.rs     Shared resolver -> Swallow / Dispatch(XPL) / Flip (§6,§8,§10.1)
  event.rs        Canonical KeyEvent + EventSource trait (multi-source front) (§3.5,§8)
  normalize.rs    Per-OS native->canonical tables, virtual-band guard (§7,§3.5)
  inject.rs       Access-controlled injection channel for virtual keys (§3.5)
  loader.rs       Embedded default keymap + transactional LOADKBD (§9)
  pipeline.rs     Toolkit-independent normalize->resolve->capture glue (§8)
  diagnostic.rs   Live §12 interception logger
  platform/
    linux.rs      evdev, no EVIOCGRAB (TESTED-design path)
    windows.rs    WH_KEYBOARD_LL low-level hook
    macos.rs      CGEventTap (session, Default = swallow-capable)
  main.rs         CLI: check / demo / diag / run
tests/engine_tests.rs   Headless unit tests (no GUI, no hooks) — §10.1
verify/oracle.py        Independent parser/lookup oracle vs. real CLD4R.KBD
verify/runtime_sim.py   Independent runtime simulation (dual-role, toggle, KF, inject)
kbd/xyscript-internal.kbd A KBD authored in XPL (not XyWrite verbs), per §3.3
```

## Build & run

The headless engine has no OS dependencies; only `src/platform/*` pulls in
per-OS crates, each gated to its `target_os`, so the other platforms never build
it.

```sh
cargo build
cargo test                       # headless engine tests (all platforms)

# Headless, no keyboard needed:
cargo run -- check --kbd ../CLD4R.KBD          # parse + validate + summarize
cargo run -- demo  --kbd kbd/xyscript-internal.kbd   # scripted events through the resolver

# Live interception (needs a real keyboard + permissions):
cargo run -- diag  --kbd kbd/xyscript-internal.kbd   # the §12 experiment
cargo run -- run   --kbd kbd/xyscript-internal.kbd   # dispatch assignments to a stub
```

### Per-platform permissions (live `diag`/`run`)

- **Linux:** read access to `/dev/input/event*` — run as root or join the
  `input` group. Reading without `EVIOCGRAB` is non-exclusive (§7).
- **macOS:** grant **Input Monitoring / Accessibility** to the terminal or the
  built binary. Modifier and Caps Lock changes arrive as `FlagsChanged`; the tap
  reconstructs clean down/up. This is the **highest-risk** platform (§7,§12) and
  the one to validate first on real hardware.
- **Windows:** no special privilege for `WH_KEYBOARD_LL`; the process must pump a
  message loop (it does). Extended keys (right Ctrl/Alt, nav, numpad Enter/slash)
  are folded in via the `E0` flag.

The `diag` output has one line per key edge: native code, canonical code (with
`*MOD` if it's a modifier trigger), phase (DOWN/UP), live bitmask, capture
(consume/passthru), and disposition. Watching Caps Lock, F-keys and Numpad 5
produce clean DOWN/UP lines is the proof §12 asks for.

## Verification performed

Because this sandbox has no Rust toolchain and no real keyboard, two independent
Python checks corroborate the engine semantics (run: `python3 verify/oracle.py`
and `python3 verify/runtime_sim.py`):

- **oracle.py** parses the *actual* `CLD4R.KBD` and asserts 22 facts: bottom-up
  bit assignment exactly matches the design's worked example
  (CAPS=4096, SHIFT=2048, ALT=1024, CTRL=512, HOTKEY=256); `MOUSE` is not a
  modifier; multi-trigger Shift; the Caps toggle; strict lookup across layers;
  one TABLE serving two bitmasks (`ALT+CAPS,ALT`); HOTKEY dual-role; an absent
  combination misses entirely; and KF/miss/dispatch disposition.
- **runtime_sim.py** re-implements the resolver and checks 14 behaviors: dual-role
  tap-vs-chord, toggle latching, KF/miss/dispatch, single-modifier bit base, and
  injection determinism. These mirror `tests/engine_tests.rs` 1:1.

`tests/engine_tests.rs` is the Rust equivalent; run `cargo test` on any platform.

## Known limitations / open work

- **Not compiled here.** The sandbox lacks a Rust toolchain (network-restricted),
  so the Rust was verified by structural review plus the two Python oracles, not
  by `cargo build`. Run `cargo build && cargo test` on each target to confirm.
- **Platform hooks are untested on-device.** `windows.rs`/`macos.rs` use the real
  `windows` / `core-graphics` APIs but have not run on those OSes. The macOS
  `FlagsChanged` reconstruction and the Windows `E0` folding are the bits most
  worth scrutinizing first.
- **Linux consume vs. observe.** A non-grabbing evdev read *observes* clean edges
  (enough for §12) but cannot *swallow* events. Real selective consumption on
  Linux needs `device.grab()` plus re-injecting the `KF` keys through a `uinput`
  virtual device — the remaining Linux work, flagged in `platform/linux.rs`.
- **Normalization tables are partial.** They cover the §12 offenders plus the
  main blocks faithfully; the full canonical chart past 104 (§3.4) is the
  maintained artifact still to be filled in for all three platforms.

## Two design sharp-edges surfaced while building

1. **Structural dual-role detection.** A modifier is dual-role if any trigger key
   also appears as a cell. In `CLD4R.KBD` that makes Shift/Ctrl/Caps dual-role
   too, because they carry `29=NO`, `42=NO`, `58=NO` cells. The layer treats
   assignments opaquely (§10.1), so it can't know `NO` is a no-op — it just
   routes those keys through the tap-vs-hold machine and dispatches the cell on a
   clean tap. This is consistent with the design but worth a conscious decision:
   either accept it (the interpreter no-ops `NO`) or let the parser treat a
   reserved no-op token as "no tap."
2. **Declared-but-unreferenced shift keys vanish silently.** Under "a modifier is
   whatever a TABLE references," declaring `ALT=56,98` with no Alt table means Alt
   is not a modifier — *and* it shifts the bottom-up bit assignment of the others.
   Consider warning on a declared shift key that no table uses.
