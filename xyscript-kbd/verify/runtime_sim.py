#!/usr/bin/env python3
"""
runtime_sim.py — independent simulation of the runtime resolver (src/resolver.rs
+ src/input_state.rs), used to corroborate the behaviors asserted in
tests/engine_tests.rs WITHOUT a Rust toolchain. Mirrors the same logic in
Python and checks: dual-role tap vs. chord, toggle latching, KF/miss/dispatch
disposition, single-modifier bit assignment, and injection determinism.
"""
import sys
from oracle import parse  # reuse the verified parser

DISPATCH, SWALLOW, FLIP = "dispatch", "swallow", "flip"


class Resolver:
    """Mirror of src/resolver.rs + src/input_state.rs."""
    def __init__(self, km):
        self.km = km
        self.held = 0
        self.latched = 0
        self.down = set()
        self.consumed = {}   # mod bit -> bool
        self.mods_by_key = {}
        for m in km["modifiers"]:
            for k in m["triggers"]:
                self.mods_by_key[k] = m

    def mask(self):
        return self.held | self.latched

    def _disp(self, bitmask, key):
        a = self.km["tables"].get((bitmask, key))
        if a is None:
            return (SWALLOW, None)
        if a.strip().upper() == "KF":
            return (FLIP, None)
        return (DISPATCH, a)

    def event(self, key, phase, inject_mask=None):
        # phase in {"down","up","inject"}
        if phase == "inject":
            d = self._disp(inject_mask, key)
            return (SWALLOW, None) if d[0] == FLIP else d  # KF N/A for virtual

        m = self.mods_by_key.get(key)
        if m is not None:
            if phase == "down":
                if key in self.down:
                    return (SWALLOW, None)
                self.down.add(key)
                if m["behavior"] == "Toggle":
                    self.latched ^= m["bit"]
                else:
                    self.held |= m["bit"]
                    if m["dual_role"]:
                        self.consumed[m["bit"]] = False
                return (SWALLOW, None)
            else:  # up
                self.down.discard(key)
                if m["behavior"] == "Toggle":
                    return (SWALLOW, None)
                self.held &= ~m["bit"]
                was = self.consumed.pop(m["bit"], True)
                if m["dual_role"] and not was:
                    return self._disp(self.mask(), key)  # fire tap
                return (SWALLOW, None)
        else:
            if phase == "down":
                for mm in self.km["modifiers"]:
                    if mm["dual_role"] and (self.held & mm["bit"]):
                        self.consumed[mm["bit"]] = True
                self.down.add(key)
                return self._disp(self.mask(), key)
            else:
                self.down.discard(key)
                return (SWALLOW, None)


def main():
    checks = []

    def chk(name, cond, got=None):
        checks.append((name, bool(cond), got))

    # 1. Dual-role tap then chord.
    km = parse("KEYS=104\nHOTKEY=76\nTABLE=\n16=q\n76=TAP\nTABLE=HOTKEY\n16=CHORD16\n")
    r = Resolver(km)
    chk("HOTKEY dual_role", any(m["name"] == "HOTKEY" and m["dual_role"]
                                for m in km["modifiers"]))
    chk("76 down swallows", r.event(76, "down") == (SWALLOW, None))
    chk("76 up fires TAP", r.event(76, "up") == (DISPATCH, "TAP"))
    r.event(76, "down")
    chk("chord 16 -> CHORD16", r.event(16, "down") == (DISPATCH, "CHORD16"))
    r.event(16, "up")
    chk("76 up suppressed after chord", r.event(76, "up") == (SWALLOW, None))

    # 2. Toggle latches and unlatches.
    km = parse("KEYS=104\nCAPS=58,T:C\nTABLE=\n16=q\nTABLE=CAPS\n16=Q\n")
    r = Resolver(km)
    chk("pre-caps q", r.event(16, "down") == (DISPATCH, "q")); r.event(16, "up")
    r.event(58, "down"); r.event(58, "up")
    chk("post-caps Q", r.event(16, "down") == (DISPATCH, "Q")); r.event(16, "up")
    r.event(58, "down"); r.event(58, "up")
    chk("caps off -> q", r.event(16, "down") == (DISPATCH, "q"))

    # 3. KF / miss / dispatch.
    km = parse("KEYS=104\nTABLE=\n1=KF\n16=q\n")
    r = Resolver(km)
    chk("KF flips", r.event(1, "down") == (FLIP, None))
    chk("ordinary dispatch", r.event(16, "down") == (DISPATCH, "q"))
    chk("miss swallows", r.event(99, "down") == (SWALLOW, None))

    # 4. Single modifier gets bit 256 (bottom-up base).
    km = parse("KEYS=140\nSHIFT=42\nTABLE=\n130=plain\nTABLE=SHIFT\n130=shifted\n",
               virtual_base=128)
    shift_bit = next(m["bit"] for m in km["modifiers"] if m["name"] == "SHIFT")
    chk("single modifier bit == 256", shift_bit == 256, shift_bit)

    # 5. Injection determinism: ignores physical Shift, honors carried mask.
    r = Resolver(km)
    r.event(42, "down")  # physically hold shift
    chk("inject mask 0 -> plain", r.event(130, "inject", 0) == (DISPATCH, "plain"))
    chk("inject shift bit -> shifted",
        r.event(130, "inject", shift_bit) == (DISPATCH, "shifted"))

    width = max(len(n) for n, _, _ in checks)
    passed = 0
    print("=" * (width + 12))
    print("runtime resolver simulation")
    print("=" * (width + 12))
    for name, ok, got in checks:
        s = "PASS" if ok else "FAIL"
        extra = "" if ok else f"   (got {got!r})"
        print(f"[{s}] {name.ljust(width)}{extra}")
        passed += ok
    print("-" * (width + 12))
    print(f"{passed}/{len(checks)} checks passed")
    if passed != len(checks):
        sys.exit(1)


if __name__ == "__main__":
    main()
