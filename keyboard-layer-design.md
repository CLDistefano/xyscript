---
title: keyboard-layer-design
tags: 
created: 2026-06-01T04:20:15
---
# Keyboard Layer — Design Specification

*Cross-platform XyWrite-style editor (Rust). Front-end input layer feeding the XPL command interpreter.*

> **Foundational premise (see `development-plan.md` → "Foundational premise").**
> This keyboard layer **is** XyScript's *sine qua non* — the configurable capture
> model that owns the keyboard and can release keys to the OS (`KF`) is the one
> non-negotiable, and a deliberate departure from QN. Everywhere QuickNotes (QN)
> is referenced below, it is a **feature floor** — a reference for *what behavior
> to meet or exceed* — never a structural template. QN's Python/Tkinter internals
> do not constrain any XyScript design choice; this layer is designed fresh in
> Rust, as is everything downstream of it.

---

## 1. Purpose and scope

This document specifies the **keyboard layer** of the new editor: the subsystem that turns raw operating-system key events into editor *assignments* (command strings) for execution. It is written to be handed to whoever implements the layer in Rust.

It covers the keyboard layer in full — the KBD file format, the modifier model, scancode normalization, lookup, and loading — plus the two seams where the layer meets the rest of the system: the **XPL interpreter** (which executes assignments) and the **startup script / `LOADKBD`** mechanism (which decides what gets loaded). It does **not** specify the XPL interpreter itself, the text engine, or the GUI.

### Context in one paragraph

The editor is modeled on the classic DOS word processor XyWrite. A prototype, *QuickNotes* (QN), implements a capable surrounding system in Python/Tkinter: a command language with variables, arrays, expressions, `IF`/`WHILE`/`GOTO`, subroutines, scoping, an error model, a command line (CMline), user libraries (`ADDLIB`/`.qnu`), and a user-editable `startup.qns`. QN establishes the **feature floor** — *what* the command-and-scripting system does for a user — that XyScript must meet and ultimately exceed as a substantial superset. XyScript does **not** reimplement QN's structure: its command language, **XPL** (the *XyScript Programming Language*), is built fresh in Rust — a proper interpreter, an owned text engine, real types where useful — with QN's Python/Tkinter internals treated as non-binding. The new editor pairs this fresh design with a **classic XyWrite `.KBD`** keyboard layer. QN's Tkinter-based `.qnk`/`LOADKEY` binding model is **retired** — Tkinter could not intercept raw scan codes or synthesize new modifier keys, which is itself a primary reason for the move to Rust. The reference file `CLD4R.KBD` (a real XyWrite 4 keyboard file) is used throughout as a structural example *of the KBD format*.

---

## 2. Core model: shift state as a bitmask

The single most important idea: a keyboard layout is a function

```
(modifier_bitmask, key_code) -> assignment
```

XyWrite's KBD "scan code" arithmetic is exactly a bitmask over modifier state. Each shifting key owns one bit. The classic rule — *"add 256 to the last shifting key in the table, 512 to the next from the bottom, doubling each time"* — is simply assigning successive bit positions from the bottom of the modifier list upward.

### Worked example (from `CLD4R.KBD`)

The header declares the shifting keys, in order:

```
CAPS=58,T:C
SHIFT=42,54
ALT=56,98
CTRL=29,99
HOTKEY=76
```

Assigning bits from the bottom up:

| Modifier | Trigger key code(s) | Bit value |
|----------|--------------------|-----------|
| CAPS     | 58                 | 4096 (2¹²) |
| SHIFT    | 42, 54             | 2048 (2¹¹) |
| ALT      | 56, 98             | 1024 (2¹⁰) |
| CTRL     | 29, 99             | 512 (2⁹)  |
| HOTKEY   | 76                 | 256 (2⁸)  |

`HOTKEY` is key code **76 = Numpad 5** — a *custom* shifting key invented purely for its convenient location. This is the feature the fixed-modifier model of any GUI toolkit cannot express, and the reason the layer must be data-driven.

> **Numpad 5 is only an example.** The design goal is fully general: **any key may be designated a shifting key** — a function key, a bracket, the numpad, anything the user finds convenient — and several may be defined at once. The standard `SHIFT`/`CTRL`/`ALT`/`CAPS` declarations and a custom `HOTKEY` are not privileged categories; they are all just modifier definitions. Throughout this document, references to Numpad 5 stand in for "whatever key(s) the user chose."

Because the largest key code (104 on a classic layout) sits well below the smallest modifier bit (256), key codes and modifier bits never collide; the "effective scan code" is just `key_code + bitmask`. The implementation does **not** need to literally add them — it should key its tables on the pair `(bitmask, key_code)`. The arithmetic is only a description of the on-disk numbering, and decoupling the pair internally is also what lets the key-code space grow past 104 without disturbing the modifier model (§3.4).

### Bitmask sizing (consequence of "any key")

Because the number of definable shift keys is open-ended, **size the internal bitmask as `u32`.** The classic on-disk scheme packs modifier bits *above* the key-code range (256, 512, …), which in a 16-bit value leaves room for only about eight modifiers. Internally there is no need to honor that packing: tables are keyed on the `(bitmask, key_code)` *pair*, so a modifier can simply be bit 0, 1, 2, … decoupled from key codes entirely. A `u32` makes the count of user-defined shift keys a non-issue. The on-disk "doubling" numbering is retained only as a compatibility detail of the file format, not as an internal constraint.

---

## 3. The KBD file format (as adopted)

The editor adopts the classic KBD *format and mechanics*. A KBD file has two parts.

### 3.1 Header

Plain-text lines, one declaration each. Comments begin with `;`.

- `KEYS=<n>` — number of physical keys the layout addresses.
- One line per shifting key: `CAPS=`, `SHIFT=`, `ALT=`, `CTRL=`, plus any **custom** shift keys (e.g. `HOTKEY=`). The value is a comma-separated list of the **key codes** that trigger that modifier (so Shift can be both 42 and 54). The *order of these lines* determines bit assignment (§2).
- Per-modifier **attributes** follow the key code(s) as `T:<x>`. In `CLD4R.KBD`, `CAPS=58,T:C` marks Caps as a **toggle** (latching) modifier; modifiers without the attribute are **hold** (momentary). See §4.
- Other header keys (e.g. `MOUSE=105`, `HOTKEY=76`) register additional special keys. The parser should preserve unrecognized header keys rather than error on them.

### 3.2 Tables

Each table is introduced by a `TABLE=` line naming one or more modifier combinations, followed by `key_code=assignment` lines.

```
TABLE=                      ; the empty name is the UNSHIFTED layer (bitmask 0)
1=ES
16=q
...
TABLE=SHIFT
16=Q
...
TABLE=ALT+CAPS,ALT          ; one table serves two bitmasks
...
```

Key points:

- A `TABLE=` header may list **several combinations separated by commas** (`ALT+CAPS,ALT`). All listed combinations resolve to the same assignment set — i.e. multiple bitmasks point at one table. The parser computes each combination's bitmask from the modifier registry and registers the table under each.
- The empty header `TABLE=` is the **unshifted** layer, bitmask `0`.
- Combination names are modifier names joined by `+` in any order; the parser maps names → bits → summed bitmask. Order within a combination is irrelevant.

### 3.3 Assignments are opaque XPL strings

The right-hand side of each `key_code=…` line is an **assignment**: the text to feed the interpreter when that key resolves. The keyboard layer treats assignments as **opaque strings**. It does not parse, validate, or execute them — that is the interpreter's job (§7).

> **Important divergence from the reference file.** `CLD4R.KBD`'s assignments are *XyWrite* verbs (`NO`, `JM(...)`, `$X`, `BD`, `�0D` byte escapes, etc.). These are **not** valid XPL and will **not** run verbatim in the new editor. `CLD4R.KBD` is a *structural* reference — it demonstrates the header, the shift-key declarations, the bitmask scheme, and the table layout. A real keyboard file for the new editor carries the same keys and layers but with **XPL** on the right-hand side. Because the keyboard layer stores assignments opaquely, this requires no special handling in the layer; it simply means authored KBD files will contain XPL.

### 3.4 Key-code space, `KEYS=`, and keyboards beyond 104 keys

The classic 1–104 numbering is not a ceiling. `KEYS=<n>` conceptually configures larger keyboards, and the model accommodates this almost entirely through *numbering and normalization* rather than core logic — because key codes and modifier bits are fully decoupled internally (§2). Growing the key-code space does not touch the bitmask math, the lookup, or the loader.

- **`KEYS=` is a range declaration, not an enumeration.** It tells the parser that this layout addresses key codes `1..N`. The loader uses it for validation (reject assignments to out-of-range codes) and bounds-checking. It does not itself describe any physical key; `KEYS=126` simply makes codes 105–126 legal cells.
- **Codes above 104 extend the editor's own canonical numbering.** The 1–104 range is fixed by history (IBM set 1); codes 105+ are defined by the editor. Lighting them up is work in the **per-OS normalization table** (§7): each platform must map the extra physical keys (F13–F24, media keys, extra navigation/numpad, international keys) to those canonical codes. If a platform does not deliver a given key, that code is simply never produced there and the cell is inert on that platform — consistent with the strict rule (§6). A `KEYS=126` file is therefore portable; keys light up on the platforms whose normalization tables reach them.
- **This implies a maintained artifact: a stable canonical key-code chart extended past 104.** KBD files and all three normalization tables must agree on what codes 105+ mean, so that chart is the contract. `KEYS=` just says how much of the chart a given file uses. (Any of these extended keys may also be designated a shifting key, per §4 — e.g. F13 as a modifier.)
- **Integer width.** `KeyCode = u8` already covers `1..=255`, far beyond 126, so no change is needed for realistic large keyboards. The on-disk packing (modifier bits starting at 256, above the key range) also keeps working as long as the maximum code stays below 256. Only to exceed **255** keys would `KeyCode` widen to `u16` and the on-disk modifier-bit base lift above the new maximum — and even then the internal pair-keyed lookup is unaffected.

> The `;; KEYS=126 doesn't work in vDos(Plus)` note in `CLD4R.KBD` was a limitation of that DOS host, not of the KBD format or of this model.

### 3.5 Virtual keys and programmatic injection

Historically, XyWrite customization used key codes above 104 as **virtual keys** — addresses that need not correspond to any physical key. They let an external program "poke" keystrokes into the editor without interfering with the user's physical keyboard. The model accommodates this directly, because a virtual key is simply **a key code that no normalization table ever emits**: the lookup does not distinguish physical from virtual; both resolve through the same `(bitmask, key_code)` path.

**Reserve a virtual band.** Physical keys occupy the codes the per-OS normalization tables produce (1–104 classic, plus any extended physical keys, §3.4). Codes **above** that range are designated virtual: they exist only in the keymap tables and, by rule, are never mapped by any normalization table. That single rule is the non-interference guarantee at the numbering level — a physical keystroke can never produce a virtual code, and a virtual poke never collides with a physical key.

**Multiple event sources feed one resolver.** Generalize the front of the input pipeline (§8): define an internal canonical `KeyEvent` and treat the OS normalizer as *one* event source. A programmatic **injection channel** (IPC / API / scripting hook) is a second source that submits canonical events into the *same* resolution stage. Resolution and dispatch are shared; only the entry point differs. A poke therefore lands in the keymap exactly as a physical key would, without ever touching the OS or the physical keyboard.

**Injected events carry their full `(bitmask, key_code)` pair** — they do **not** borrow live physical modifier state. This matches the historical mechanism (XyWrite pokes carried an *effective scan code*, which already encodes the table via the modifier addend — exactly our pair). Specifying the full pair (or naming the cell/layer) makes pokes **deterministic and independent of whatever the user is physically holding at that instant**, which is what "without interfering with the physical keyboard" means in both directions. An unspecified injection defaults to the unshifted layer.

**Physical-only mechanics do not apply to virtual keys.** A poke is a discrete "this cell fired" event, not a timed down/up, so hold/toggle (§4), the tap-vs-hold state machine (§5), and `KF` (§6.1) are all N/A for injected events. A virtual key resolving to `KF` is a no-op — there is no OS event to release. The injection path is thus *simpler* than the physical path: it enters at the resolver and produces an assignment for the interpreter.

**Opt-in with access control is a likely `sine qua non`.** Because the channel can poke any cell, and cells execute XPL, the injection surface is effectively remote control of the editor — it can run arbitrary scripted commands. It must therefore be **disabled by default and explicitly enabled**, with access control over *who* may inject (e.g. a local-only endpoint, an authentication token or handshake, and/or an allow-list of permitted clients), never an always-open channel. Treat this as a security requirement of the feature, not optional polish: shipping the injection channel without it would expose every XPL capability to any process that can reach the endpoint.

---

## 4. Modifier semantics: hold vs toggle

Modifiers are **data-driven** — there is no hardcoded notion of Ctrl/Alt/Shift. Each declared modifier carries a behavior:

- **Hold (momentary):** the bit is set only while the trigger key is physically down. Shift, Alt, Ctrl, and custom keys like Numpad 5 are hold modifiers.
- **Toggle (latching):** a tap flips the bit and it stays flipped (Caps Lock behavior). Declared with the `T:C` attribute in the header.

The runtime therefore maintains two sets and OR's them to form the current bitmask:

```
current_bitmask = bits_of(physically_held_modifiers) | bits_of(latched_modifiers)
```

A new shifting key — *any* key the user picks — is *only* a new entry in the modifier registry with a bit and a behavior; nothing else in the layer changes. That is what makes "invent your own shift key" a data operation rather than a code change, and why the capability is general rather than limited to a fixed set of modifier keys.

---

## 5. Tap-vs-hold: dual-role keys

A custom shifting key is frequently *also* a key that does something when pressed alone. In `CLD4R.KBD`, key code 76 is declared `HOTKEY` **and** has table entries (e.g. unshifted `76=NOJM(,2,.,M,O,1,w,a,r,n,i,n,g,)`, and `76=NO` in most layers). So Numpad 5 means "modifier" when held with another key, but firing it alone has its own assignment.

This cannot be resolved on key-down. The runtime needs a small state machine per dual-role modifier:

1. On modifier key **down**: set the bit, but **defer** the tap assignment; record that no other key has yet been pressed.
2. While held: if **any** non-modifier key is pressed (a chord), mark the modifier "consumed."
3. On modifier key **up**: if it was *not* consumed, fire the tap assignment; otherwise suppress it.

Real Shift/Ctrl/Alt have no tap assignment and skip this path entirely. The custom shift keys are precisely the ones that need it, so the state machine is not optional.

Because the design allows **any** key to be a shifting key, this is the *common* path, not an edge case: nearly every key a user would plausibly designate (function keys especially) also has a natural tap action, so dual-role resolution must be robust and fast. Note one UX consequence — if a key that normally *types* (e.g. a letter) is made a hold modifier, its own output must be deferred until the tap-vs-hold decision resolves, which delays that character. Designating non-typing keys (function keys, numpad, brackets) avoids this; it is a guidance point for users, not a code restriction.

---

## 6. Lookup: strict, with swallow

Lookup is **strict** — there is no fallthrough between layers.

- Resolution is a single map lookup on `(current_bitmask, key_code)`.
- A **miss** means the key does nothing **in that layer** — and the event is **swallowed**, not passed through to the OS. The layer consumes every event it processes by default; the host OS must never insert a stray character behind the editor's back. (Modifier-trigger keys are likewise swallowed.)
- "Swallow on miss" is deliberately distinct from a pass-through model. There is no inheritance from the unshifted layer and no *implicit* fallback to the OS character. The **only** way an event reaches the OS is an explicit `KF` assignment (§6.1).
- Because whole combinations may be absent from a file (e.g. `CLD4R.KBD` defines `CTRL+SHIFT` and `CTRL+ALT+SHIFT` but not `CTRL+ALT+SHIFT+CAPS`), pressing an unlisted combination means **every** key code misses and the entire layer is inert until release. This falls out of the strict rule automatically and is the intended behavior.

The redundancy in real KBD files (the same entries repeated across many tables) is a *structural consequence* of strict lookup: with no fallthrough, every layer must be self-contained.

### 6.1 Keyboard Flip (KF): opt-in OS pass-through

Strict swallowing means that, by default, the editor captures *every* key it sees — which would lock the user out of OS-level keys and shortcuts. **`KF` (Keyboard Flip)** is the escape valve, modeled on XyWrite's function of the same name: a cell whose assignment is `KF` is **not** consumed; the keystroke is **released to the OS** for normal handling. It is how the user delegates specific keys (e.g. Alt-Tab, the platform command/meta shortcuts, volume and media keys) back to the operating system while the editor retains everything else.

Key properties:

- **Per-cell and opt-in.** `KF` applies to a specific `(bitmask, key_code)` cell, so the user decides exactly which keys and combinations flip to the OS. Everything not assigned `KF` stays captured.
- **It is a reserved input-layer directive, not opaque XPL.** Unlike ordinary assignments (which the layer passes verbatim to the interpreter), the consume-vs-release decision must be made **synchronously inside the OS event callback**, before default processing is decided — too early to defer to a general interpreter dispatch. The keyboard layer therefore recognizes a *small reserved set* of input-control directives itself (`KF` being the first), short-circuits on them, and treats all other assignments as opaque (§10.1).
- **Disposition table.** Resolution now has three outcomes rather than two:
  - *miss* → **swallow** (consumed, nothing happens);
  - *hit, ordinary assignment* → **consume**, dispatch the assignment string to the interpreter;
  - *hit, `KF`* → **do not consume**, release the event to the OS.
- **The embedded default keymap should KF a sensible baseline** of OS keys, so a user is never locked out of the operating system even before customizing a KBD (ties to the default's self-sufficiency, §9.1).

---

## 7. Cross-platform scancode normalization

This is the layer's hardest and most platform-specific work. The 1–104 key numbering is essentially the IBM PC scan-code **set 1** that DOS exposed. None of the three target OSes hands you that numbering, and they disagree with each other:

- **Windows** — raw input / `WM_KEYDOWN` gives a virtual-key code plus a scan code close to set 1, but **extended keys** (right Ctrl/Alt, the navigation-vs-numpad clusters, numpad Enter) carry an `E0` prefix that must be folded in.
- **macOS** — hardware-independent virtual keycodes (`kVK_*`), a completely different numbering; the OS also intercepts some keys (Caps Lock especially) before clean up/down events are visible.
- **Linux** — depends on the stack: evdev (Linux input-event codes), X11 (keycodes offset by 8), or Wayland, each different.

### Design

Maintain a **per-OS translation table**: `native_code -> canonical_key_code` (the editor's canonical numbering — 1–104 classic, extensible past 104 for larger keyboards per §3.4). Make it **data, not code** — a small per-platform file — so a keyboard quirk can be fixed without recompiling. This table is also where keys above code 104 are mapped into existence on each platform. By rule it **never** emits codes in the reserved *virtual* band (§3.5); those codes are reachable only through programmatic injection, which is what keeps virtual keys from interfering with the physical keyboard. Everything *above* this table (bitmask, KBD tables, interpreter) is then identical on all three platforms.

### Capture must be non-exclusive (required by KF)

The chosen input mechanism on each OS **must support selective non-consumption** — the ability to handle some events and let others propagate to the OS unchanged — because `KF` (§6.1) depends on it. An *exclusive* keyboard grab would make `KF` impossible. In practice this means: Windows low-level keyboard hook (`WH_KEYBOARD_LL`) returning the event unhandled to pass it through; macOS `CGEventTap` passing the event rather than swallowing it; Linux evdev read **without** an exclusive `EVIOCGRAB`. This requirement reinforces the event-tap style of interception over any exclusive-capture design.

### Highest-risk item

**Clean down/up events for whatever keys a user designates as modifiers — on macOS first.** Since *any* key may be made a shifting key, the risk is not about one key but about the full set a user might plausibly choose. Some are reliably troublesome: **Caps Lock**, **function keys** bound to OS or media behavior, and the **numpad** (including Numpad 5) are the usual offenders, and macOS is the most likely of the three platforms to remap or swallow them before the app sees clean, distinguishable down/up events. Proving the app can intercept those events for the historically-difficult keys — on macOS, before the parser or runtime — is the single biggest technical risk in the project. If a class of keys cannot be made to work natively as modifiers, that constrains what users can designate, and it needs to be known early.

---

## 8. The input pipeline

End to end, per key event:

1. **Source → canonical event.** Events enter from one of several **sources**, all producing the same internal canonical `KeyEvent`. The OS normalizer is one source: OS raw event → canonical `KeyCode` via the per-OS table (§7), tracking physical **down/up**, not just characters. A programmatic **injection channel** is another source, submitting canonical events directly with their own `(bitmask, key_code)` (§3.5). Injected events skip steps 2 below (no modifier/dual-role bookkeeping) and enter at step 3.
2. **Modifier update.** If the key code is a registered modifier trigger, update the held/latched bit (§4), run the dual-role bookkeeping (§5), and stop. The event is swallowed.
3. **Resolve.** On a non-modifier key **down**, compute `current_bitmask` and look up `(bitmask, key_code)` in the active keymap (§6).
4. **Dispatch, flip, or swallow.** On a hit whose assignment is the reserved `KF` directive, **release the event to the OS** (do not consume — §6.1). On any other hit, **consume** and emit the assignment string to the interpreter (§9). On a miss, **swallow** the event.

The pipeline is toolkit-independent: it depends only on the per-OS normalization table at the very edge, which keeps the door open to swapping the GUI toolkit later without touching input semantics.

---

## 9. Loading model

### 9.1 Embedded default keymap

A **default keymap is compiled into the binary.** It never passes through the file parser — it is constructed directly in code, so it is the one keymap guaranteed always to be valid. It is the *floor*; a user KBD is an attempted *upgrade* over it.

The default must be **self-sufficient**: basic editing plus a way to reach `LOADKBD` (a binding and/or CMline access), so a user whose KBD failed to load is never stranded with no path back to their own layout. There should be a test asserting the default can reach the load command. The default should also **`KF` a sensible baseline of OS keys** (§6.1) so the user retains access to the operating system even on the bare default layout.

### 9.2 Transactional loading

`LOADKBD` is **all-or-nothing**:

1. Parse the file into a **candidate** `Keymap`.
2. Validate it (modifiers resolve, key codes in range, tables well-formed, combinations parse).
3. On success, **atomically swap** the candidate in as the operative keymap.
4. On **any** error, **discard the candidate**; the operative keymap is left untouched. Surface a **visible error** so the user knows the new file did not take and they are still on the previous layout.

The live keymap is never mutated mid-parse, so a half-parsed file can never leave the editor in a partial state.

### 9.3 Fallback policy

The operative keymap **changes only when a load succeeds.** This single rule yields the desired behavior with no extra machinery:

- The currently operative keymap *is* the last-known-good, by construction — no separate "last good" copy is kept.
- A failed load leaves whatever was running in place: a custom layout loaded earlier, or the embedded default if nothing has succeeded yet.
- The embedded default is simply the operative slot's **initial value** at startup, so it is in force in exactly one situation: nothing has ever loaded successfully (no KBD specified, or the startup load failed).

This is forgiving: a bad edit reloaded over a good custom layout keeps the good layout running.

### 9.4 Startup integration

Loading is driven by the user-editable startup script (`startup.qns` in the prototype), exactly as XyWrite worked. The script may call `LOADKBD <path>` to install a layout. **If the startup script never loads a KBD, the embedded default stands.** This matches the prototype's existing behavior and the agreed design.

### 9.5 Optional: cross-session persistence

Within a session, the last-known-good behavior is free (§9.3). *Across* sessions, the in-memory keymap is gone, so on next launch the editor re-attempts the configured KBD path; if that file is still broken, it falls to the default for that session. If it is later deemed desirable that a user never lose a working layout even across a bad edit, the editor could cache the last successfully-compiled keymap to disk and seed from that instead of the default. Treat this as optional polish, not core.

---

## 10. Interfaces (the seams)

### 10.1 To the XPL interpreter

For ordinary assignments, the keyboard layer's responsibility **ends** at producing an assignment string. The interpreter:

- receives the opaque assignment and parses/executes it as XPL;
- owns all validation of assignment content (the keyboard layer never inspects it);
- is the same engine that runs the CMline, `startup.qns`, and user libraries.

The exception is a **small reserved set of input-control directives** that the keyboard layer recognizes and acts on *itself*, because their effect is on event disposition rather than editor state and must be decided synchronously in the OS event callback. **`KF`** (§6.1) is the first such directive. The layer checks for the reserved set *before* dispatching; a match short-circuits to the layer's own handling, and everything else is opaque and goes to the interpreter. The reserved set should be kept deliberately minimal.

This split keeps the keyboard layer fully testable **headless** — no GUI, no OS hooks — by asserting that a given `(bitmask, key_code)` resolves to the expected assignment string.

### 10.2 To the startup script / `LOADKBD`

`LOADKBD` is a XPL command (interpreter-dispatched) that invokes the loader in §9.2. The startup script is the policy layer that decides *which* KBD loads; the keyboard layer provides the *mechanism*. The retired `.qnk`/`LOADKEY`/`UNLOADKEY`/`SAVEKBD` family is replaced by classic `.KBD` files loaded via `LOADKBD`.

### 10.3 Scancode exposure to scripts

The prototype already exposes a key's scan code to scripts via the special variable `$SC`. The new layer should preserve an equivalent: the canonical key code (and/or current bitmask) should be queryable from XPL, so scripts can reason about keys consistently with the keyboard layer's own numbering.

---

## 11. Rust data model sketch

Illustrative, not prescriptive:

```rust
type KeyCode = u8;        // canonical numbering: 1..=104 classic, up to KEYS (≤255 in a u8); see §3.4
type Bitmask = u32;       // OR of active modifier bits; u32 so the number
                          // of user-definable shift keys is effectively unbounded

enum ModBehavior { Hold, Toggle }

struct ModifierDef {
    name: String,             // "CAPS", "SHIFT", "HOTKEY", ...
    bit: Bitmask,             // 256, 512, 1024, ... assigned at parse time
    trigger_keys: Vec<KeyCode>,
    behavior: ModBehavior,    // T:C -> Toggle; otherwise Hold
}

// A resolved cell is either a reserved input-layer directive (acted on by
// the layer itself) or an opaque XPL string (handed to the interpreter).
enum Assignment {
    Flip,            // KF: release the event to the OS (see §6.1)
    // ...other reserved directives may be added, kept minimal...
    Xpl(String),     // opaque; the interpreter validates and executes
}

struct Keymap {
    modifiers: Vec<ModifierDef>,
    // strict lookup; a miss => swallow
    tables: HashMap<(Bitmask, KeyCode), Assignment>,
}

// Runtime state, separate from the (immutable) Keymap:
struct InputState {
    held: Bitmask,        // momentary modifiers currently down
    latched: Bitmask,     // toggled modifiers
    // per dual-role modifier: deferred-tap / consumed tracking (see §5)
}

impl InputState {
    fn current_bitmask(&self) -> Bitmask { self.held | self.latched }
}

// Per-OS edge: native_code -> KeyCode, loaded from a data file.
trait ScancodeMap { fn to_canonical(&self, native: u32) -> Option<KeyCode>; }

// Canonical event fed to the resolver. The OS normalizer is one source;
// the access-controlled injection channel (virtual keys, §3.5) is another.
struct KeyEvent { key_code: KeyCode, bitmask: Bitmask, /* phase: Down/Up for physical */ }
trait EventSource { /* yields KeyEvents into the shared resolver */ }
```

A clean rule of thumb: the `Keymap` is immutable data produced by the parser; `InputState` is the only thing that changes as keys are pressed; the per-OS `ScancodeMap` is the only platform-specific piece.

---

## 12. Recommended first experiment

Before writing the parser or runtime, **prove the app can intercept clean, distinguishable down/up events — on macOS first — for the keys users are most likely to designate as modifiers**, starting with the historically-difficult ones (Caps Lock, function keys, the numpad including Numpad 5) (§7). Everything else in this document is portable and low-risk; the ability to turn arbitrary keys into modifiers is the project's load-bearing assumption.

---

## Appendix A — Decisions captured

- Keyboard model: classic XyWrite **`.KBD`** only; the Tkinter `.qnk`/`LOADKEY` model is **retired**.
- Modifiers are **data-driven** bits; **any key** may be designated a shifting key, and several at once (Numpad 5 = key code 76 is only an example). The standard Shift/Ctrl/Alt/Caps keys are not privileged — they are ordinary modifier definitions.
- The internal bitmask is **`u32`**, decoupling the count of definable shift keys from the key-code range; the on-disk "doubling" numbering is a format-compatibility detail only.
- The key-code space is **not capped at 104**: `KEYS=` declares a layout's addressable range, codes above 104 extend the editor's canonical numbering, and large keyboards are supported by extending the per-OS normalization tables and a stable canonical key-code chart. `KeyCode = u8` covers up to 255 keys with no change (§3.4).
- **Virtual keys** are codes in a reserved band that no normalization table emits; they are reachable only via a **programmatic injection channel** (a second event source feeding the shared resolver), carry their own `(bitmask, key_code)`, and cannot interfere with the physical keyboard. The injection channel is **disabled by default and access-controlled** — a security requirement, since it can execute arbitrary XPL (§3.5).
- Modifier behavior: **hold** (momentary) by default; **toggle** (latching) via `T:C` (Caps).
- Dual-role keys resolved by a **tap-vs-hold** state machine.
- Lookup is **strict**: a miss does nothing *in that layer* and the event is **swallowed** (not passed to the OS, no inheritance).
- **`KF` (Keyboard Flip)** is the sole opt-in pass-through: a cell assigned `KF` releases the keystroke to the OS instead of consuming it, giving the user controlled OS access. It is a **reserved input-layer directive** (not opaque XPL), decided synchronously in the event callback. Its existence requires **non-exclusive** keyboard capture on every platform. The embedded default keymap `KF`s a baseline of OS keys so the user is never locked out.
- Loading is **transactional** over an **embedded default**; the operative keymap **changes only on a successful load**, so the running map is always the last-known-good, with the embedded default as backstop.
- A **visible error** is shown on a failed load.
- Startup script drives loading via `LOADKBD`; if it loads no KBD, the **embedded default** stands.
- Assignments are stored **opaque**; their vocabulary is **XPL** (not literal XyWrite); the interpreter validates and executes them.
- Cross-session caching of the last-good keymap is **optional** future polish.
