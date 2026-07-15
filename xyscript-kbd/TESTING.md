# XyScript keyboard prototype — tester instructions

Thanks for helping test. This is an early prototype of a keyboard-interception
layer for a text editor. We need to confirm one thing on your machine: that the
app can see **clean, distinguishable key-down and key-up events** for every key —
especially the awkward ones (Caps Lock, the function keys, and Numpad 5) — using
a *non-exclusive* hook that still lets other keys reach the operating system.

You do **not** need to know Rust. The whole test is: install Rust, build, run two
or three commands, and tell us what you saw. Budget ~20 minutes.

There are three sections — **Linux**, **macOS**, **Windows**. Do the one for your
OS. A short **report-back template** is at the end.

---

## Results to date — §12 interception passed on all three platforms (2026-06-07)

| Platform | Build + test | `diag` on hardware | Notes |
|----------|--------------|--------------------|-------|
| **macOS** | OK | ✅ **Passed** — Sequoia (15), Apple Silicon (ARM) | The high-risk path: `FlagsChanged`→clean down/up incl. Caps Lock, non-exclusive `CGEventTap`. Reported running correctly by an experienced tester. |
| **Windows** | OK | ✅ **Passed** | Primary developer's machine. Clean edges incl. `E0` folding (right Ctrl→99 / right Alt→98) and swallow-on-handle. |
| **Linux** | OK | ✅ **Passed** (observe-only) | Clean down/up at a physical console. Selective consume (`uinput`) still unbuilt — that's an engineering item, not an interception-correctness question. |

> The §12 interception question is fully answered: clean, distinguishable edges
> for the hard keys on macOS, Windows, and Linux. This retires the project's
> single load-bearing risk (design §12). The one remaining keyboard-layer build
> item is the Linux `uinput` selective-consume path.

---

## What Rust you need

- **Rust 1.74 or newer** (latest stable is best). Install with rustup:
  <https://rustup.rs> — on Linux/macOS:
  `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
  then open a new terminal so `cargo` is on your `PATH`.
- Confirm: `cargo --version` prints 1.74.x or higher.

You'll be given the project folder `xyscript-kbd/` (as a zip or a repo). `cd` into
it before running anything.

---

## Step 0 — Build + headless self-test (all platforms, no permissions needed)

Do this first. It proves the code builds and the engine logic works on your
machine, independent of the OS keyboard hook.

```sh
cargo build
cargo test
```

Expect: it compiles, and the tests print something ending in `test result: ok`.
If `cargo test` fails, **stop and report that** — the live test below won't add
anything until the build is healthy.

Then a quick no-permissions sanity run (prints a scripted demo, exits on its own):

```sh
cargo run -- demo --kbd kbd/xyscript-internal.kbd
```

You should see lines like `press q (16) unshifted => dispatch` and a dual-role
section. If you got here, the build is good.

---

## How to read the live output (everyone)

The live command is `diag`. It prints **one line per key event**:

```
native  canonical  phase  bitmask  capture   disposition
58      58*MOD     DOWN   0        consume   swallow
58      58*MOD     UP     4096     consume   swallow
16      16         DOWN   0        consume   dispatch "q"
```

- **native** — the raw code your OS reported.
- **canonical** — the editor's internal code (this is what we're verifying the
  per-OS translation produces). `*MOD` means the key is a modifier (Shift-like).
- **phase** — `DOWN` when you press, `UP` when you release. We need to see *both*,
  cleanly, for every key.
- **bitmask** / **capture** / **disposition** — internal detail; you can ignore
  these. (`bitmask` jumps to a nonzero number while a modifier is held — that's
  expected and good.)

**To quit `diag`:** press **Ctrl-C**. If Ctrl-C doesn't respond (the hook may be
holding the keys), just **close the terminal window**, or from a second terminal
run `pkill xykbd` (Linux/macOS) / close it via Task Manager (Windows).

**The keys we care about** — press and release each one slowly and watch for a
clean `DOWN` then `UP` line, with the **canonical** value shown:

| Key                 | Expected canonical | Notes                                  |
|---------------------|--------------------|----------------------------------------|
| Caps Lock           | 58 (`*MOD`)        | latches: bitmask stays set until you tap it again |
| Left / Right Shift  | 42 / 54 (`*MOD`)   |                                        |
| Left / Right Ctrl   | 29 / 99 (`*MOD`)   |                                        |
| Left / Right Alt    | 56 / 98 (`*MOD`)   | (right Alt = AltGr on some layouts)    |
| Numpad 5            | 76 (`*MOD`)        | tap alone should `dispatch` a warning; held = modifier |
| F1 … F10            | 59 … 68            |                                        |
| q w e / a s d       | 16 17 18 / 30 31 32| should `dispatch` the letter           |
| Esc / Tab           | 1 / 15             | should show `FLIP->OS` (handed back to the OS) |

The single most important result is **Caps Lock, the function keys, and Numpad 5
each producing a clean DOWN and a clean UP** with the right canonical number. If
any of them is missing, doubled, stuck, or never shows `UP`, that's exactly the
finding we need.

---

## Linux

### Build prerequisites
Usually just Rust. If `cargo build` complains about a missing compiler or
`pkg-config`, install the basics (Debian/Ubuntu shown):

```sh
sudo apt-get update && sudo apt-get install -y build-essential pkg-config
```

### Do NOT run `diag`/`run` over SSH (or remote desktop)
The live commands read the **physical** console keyboard via `/dev/input/event*`.
Keystrokes typed in an **SSH** session travel over a pseudo-terminal that never
touches those devices, so `diag` will start, print its banner, and then show
**no per-key lines no matter what you press** — not a bug, just the wrong input
path. The same applies to most VNC/remote-desktop sessions (they inject via the
X server, bypassing evdev).

Run the live commands one of these ways instead:

- At the machine's **physical keyboard / local console**.
- If it's a **VM**, use the hypervisor's **console window** (not SSH): the
  virtual keyboard it injects shows up as a real `/dev/input/eventX`, so evdev
  sees it.

The headless commands — `cargo test`, `cargo run -- check`, `cargo run -- demo`
— work fine over SSH; only `diag` and `run` need local input.

### Permissions
The app reads `/dev/input/event*`, which is normally root-only. Two options:

- **Easiest:** run with `sudo` (note: `sudo` may not have `cargo` on its PATH, so
  build first, then run the built binary):
  ```sh
  cargo build
  sudo ./target/debug/xykbd diag --kbd kbd/xyscript-internal.kbd
  ```
- **Or** add yourself to the `input` group once, then log out and back in:
  ```sh
  sudo usermod -aG input "$USER"
  # log out / log back in, then:
  cargo run -- diag --kbd kbd/xyscript-internal.kbd
  ```

### Important expectation on Linux
This prototype **observes** keys but does **not** swallow them on Linux yet. So
while `diag` runs, your keystrokes will *also* reach the terminal as normal (you'll
see letters appear alongside the diag lines). **That is expected** — please don't
report it as a bug. We only care that the diag lines show clean DOWN/UP with the
right canonical codes.

It works the same under X11 or Wayland (the hook sits below the display server).

---

## macOS

> Heads-up: macOS is the platform we're most worried about. The OS sometimes
> intercepts Caps Lock and the media/function keys before an app can see clean
> events. Finding out whether that happens on your Mac is the whole point.

### Build prerequisites
Install Apple's command-line tools (for the linker/SDK), then build:

```sh
xcode-select --install   # if not already installed
cargo build
```

Works on both Apple Silicon and Intel.

### Permissions (required — the hook won't work without this)
The app uses a `CGEventTap`, which macOS gates behind privacy permissions. Grant
them to **the terminal app you'll run from** (Terminal or iTerm):

1. Open **System Settings → Privacy & Security**.
2. Under **Accessibility**, click **+**, add your terminal app, toggle it on.
3. Under **Input Monitoring**, do the same — add and enable your terminal app.
4. **Quit and reopen** the terminal app (the grant only takes effect on relaunch).

Then run:

```sh
cargo run -- diag --kbd kbd/xyscript-internal.kbd
```

If it prints `failed to create CGEventTap (grant Input Monitoring / Accessibility)`,
the permission didn't take — recheck both lists and that you fully quit/reopened
the terminal.

### Things to try on macOS specifically
- **Caps Lock**: does it show a clean DOWN and UP (canonical 58)? Or nothing /
  only one edge? This is the key risk.
- **Function keys**: in **System Settings → Keyboard**, there's a setting like
  *"Use F1, F2, etc. as standard function keys."* Please test **F1–F10 with that
  setting both OFF and ON**, and note whether the diag lines differ. (With it off,
  the keys are brightness/media and macOS may grab them.)
- **Numpad 5** (canonical 76): only on a keyboard with a real numeric keypad.

---

## Windows

(Included for completeness; the primary developer is testing here.)

### Build + run
```sh
cargo build
cargo run -- diag --kbd kbd/xyscript-internal.kbd
```

No special privileges are needed for the low-level keyboard hook. Note that on
Windows the app **does** swallow keys it handles, so while `diag` runs, most
keystrokes won't reach other apps — that's expected. Use Ctrl-C or close the
window to quit. Test the same key list as above; right Ctrl/Alt and the numpad
arrive as "extended" keys, which the app folds in — confirm right Ctrl shows
canonical 99 and right Alt shows 98.

---

## What to send back

Copy this template into an email / message and fill it in:

```
OS and version:            (e.g. Ubuntu 24.04 / macOS 15.1 / Windows 11 23H2)
Machine / keyboard:        (e.g. MacBook Pro M3, built-in keyboard; or external USB)
Rust version:              (output of: cargo --version)

cargo build:               OK / failed  (paste errors if failed)
cargo test:                OK / failed  (paste the "test result:" line, or errors)
diag launched:             yes / no     (if no, what stopped it?)

For each key, did you see a clean DOWN *and* UP with the expected canonical code?
  Caps Lock (58):          yes / no / weird — describe
  Left/Right Shift (42/54): yes / no / weird
  Left/Right Ctrl (29/99):  yes / no / weird
  Left/Right Alt (56/98):   yes / no / weird
  Numpad 5 (76):            yes / no / n/a (no numpad) / weird
  F1–F10 (59–68):           yes / no / weird   (macOS: note the F-key setting state)
  Letters q/w/e, a/s/d:     yes / no / weird
  Esc / Tab (FLIP->OS):     yes / no / weird

Anything missing, doubled, stuck, or with no UP event:
  (this is the most useful part — describe exactly what you pressed and saw)

Paste ~15 lines of the diag output that show the awkward keys (Caps, an F-key,
Numpad 5):
```

That's everything. Thank you!
