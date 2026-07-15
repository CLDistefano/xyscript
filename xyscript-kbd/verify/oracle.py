#!/usr/bin/env python3
"""
oracle.py — an independent reference implementation of the XyScript keyboard
engine's *semantics*, used to verify the design (and the Rust port) against the
REAL reference file CLD4R.KBD.

This is deliberately a second, from-scratch implementation in a different
language: if the Rust engine and this oracle agree on bit assignment, combo->
bitmask, strict lookup, dual-role detection, and KF disposition for the actual
reference file, the load-bearing logic is corroborated rather than just
self-asserted.

It mirrors src/parser.rs and src/resolver.rs. Run: python3 verify/oracle.py
Exits non-zero if any assertion fails.
"""
import os
import sys

MOD_BIT_BASE = 256


class ParseError(Exception):
    pass


def parse(text, virtual_base=None):
    keys_max = None
    decls = []            # [(name, [trigger_keys], behavior)]
    raw_tables = []       # [(combos:[[name,...]], rows:[(line,key,raw)])]
    cur_combos = None
    cur_rows = []
    in_tables = False

    for i, line in enumerate(text.split("\n")):
        lineno = i + 1
        stripped = line.lstrip()
        if not stripped or stripped.startswith(";"):
            continue

        if stripped.startswith("TABLE="):
            in_tables = True
            if cur_combos is not None:
                raw_tables.append((cur_combos, cur_rows))
                cur_rows = []
            rest = stripped[len("TABLE="):].strip()
            if rest == "":
                cur_combos = [[]]                      # unshifted, mask 0
            else:
                cur_combos = [
                    [n.strip() for n in combo.split("+") if n.strip()]
                    for combo in rest.split(",")
                ]
            continue

        if "=" not in line:
            raise ParseError(f"line {lineno}: expected key=value")
        eq = line.index("=")
        key_part = line[:eq].strip()
        value = line[eq + 1:]

        if not in_tables:
            if key_part.upper() == "KEYS":
                keys_max = int(value.strip())
            else:
                triggers, behavior = [], "Hold"
                for tok in value.split(","):
                    tok = tok.strip()
                    if not tok:
                        continue
                    if tok.upper().startswith("T:"):
                        if tok[2:].upper() == "C":
                            behavior = "Toggle"
                        continue
                    triggers.append(int(tok))
                decls.append((key_part, triggers, behavior))
            continue

        # table row
        key = int(key_part)
        cur_rows.append((lineno, key, value))

    if cur_combos is not None:
        raw_tables.append((cur_combos, cur_rows))
    if keys_max is None:
        raise ParseError("missing KEYS=")

    # Which decls are modifiers? Those referenced by some TABLE combo.
    referenced = []
    for combos, _ in raw_tables:
        for combo in combos:
            for name in combo:
                if name.upper() not in (r.upper() for r in referenced):
                    referenced.append(name)
    for name in referenced:
        if not any(d[0].upper() == name.upper() for d in decls):
            raise ParseError(f"TABLE references undeclared modifier {name}")

    mod_decls = [d for d in decls
                 if any(d[0].upper() == r.upper() for r in referenced)]

    # Bottom-up bit assignment: last modifier -> 256, doubling upward.
    n = len(mod_decls)
    modifiers = []  # dict(name,bit,triggers,behavior,dual_role)
    for rank, d in enumerate(mod_decls):
        from_bottom = n - 1 - rank
        bit = MOD_BIT_BASE << from_bottom
        modifiers.append(dict(name=d[0], bit=bit, triggers=d[1],
                              behavior=d[2], dual_role=False))

    def combo_mask(combo):
        mask = 0
        for name in combo:
            m = next(m for m in modifiers if m["name"].upper() == name.upper())
            mask |= m["bit"]
        return mask

    tables = {}  # (mask,key) -> raw assignment
    for combos, rows in raw_tables:
        masks = [combo_mask(c) for c in combos]
        for lineno, key, raw in rows:
            if key < 1 or key > keys_max:
                raise ParseError(f"line {lineno}: key {key} out of range")
            for mask in masks:
                tables[(mask, key)] = raw

    keyed = {k for (_, k) in tables}
    for m in modifiers:
        m["dual_role"] = any(k in keyed for k in m["triggers"])

    return dict(keys_max=keys_max, modifiers=modifiers, tables=tables,
                virtual_base=virtual_base)


def is_kf(raw):
    return raw.strip().upper() == "KF"


def disposition(km, mask, key):
    a = km["tables"].get((mask, key))
    if a is None:
        return ("swallow", None)
    if is_kf(a):
        return ("flip", None)
    return ("dispatch", a)


# --------------------------------------------------------------------------
# Assertions against the design doc's stated facts and the real file.
# --------------------------------------------------------------------------
def main():
    here = os.path.dirname(os.path.abspath(__file__))
    kbd_path = os.path.join(here, "..", "..", "CLD4R.KBD")
    with open(kbd_path, "r", encoding="latin-1") as f:
        text = f.read()

    km = parse(text, virtual_base=None)
    checks = []

    def check(name, cond, got=None):
        checks.append((name, bool(cond), got))

    mods = {m["name"].upper(): m for m in km["modifiers"]}

    # 1. Bit assignment exactly as the design's worked example (§2).
    check("CAPS bit == 4096", mods["CAPS"]["bit"] == 4096, mods["CAPS"]["bit"])
    check("SHIFT bit == 2048", mods["SHIFT"]["bit"] == 2048, mods["SHIFT"]["bit"])
    check("ALT bit == 1024", mods["ALT"]["bit"] == 1024, mods["ALT"]["bit"])
    check("CTRL bit == 512", mods["CTRL"]["bit"] == 512, mods["CTRL"]["bit"])
    check("HOTKEY bit == 256", mods["HOTKEY"]["bit"] == 256, mods["HOTKEY"]["bit"])

    # 2. MOUSE is NOT a modifier (declared but never referenced by a TABLE) (§3.1).
    check("MOUSE is not a modifier", "MOUSE" not in mods,
          sorted(mods.keys()))

    # 3. Multi-trigger Shift (42 and 54) (§3.1).
    check("SHIFT triggers == [42,54]", mods["SHIFT"]["triggers"] == [42, 54],
          mods["SHIFT"]["triggers"])

    # 4. CAPS is the toggle (T:C); others hold (§4).
    check("CAPS behavior Toggle", mods["CAPS"]["behavior"] == "Toggle")
    check("SHIFT behavior Hold", mods["SHIFT"]["behavior"] == "Hold")

    # 5. Strict lookup of representative cells across layers (§6).
    check("unshifted (0,16) == 'q'", km["tables"].get((0, 16)) == "q",
          km["tables"].get((0, 16)))
    check("SHIFT (2048,16) == 'Q'", km["tables"].get((2048, 16)) == "Q",
          km["tables"].get((2048, 16)))
    check("CAPS (4096,16) == 'Q'", km["tables"].get((4096, 16)) == "Q",
          km["tables"].get((4096, 16)))
    check("SHIFT+CAPS (6144,16) == 'q'",
          km["tables"].get((2048 | 4096, 16)) == "q",
          km["tables"].get((2048 | 4096, 16)))

    # 6. One TABLE serving two bitmasks: TABLE=ALT+CAPS,ALT (§3.2).
    #    ALT=1024, ALT+CAPS=1024|4096=5120. Both must have key 2's entry.
    check("ALT layer (1024,2) present", (1024, 2) in km["tables"])
    check("ALT+CAPS layer (5120,2) present", (5120, 2) in km["tables"])
    check("ALT and ALT+CAPS share key 2 assignment",
          km["tables"].get((1024, 2)) == km["tables"].get((5120, 2)),
          (km["tables"].get((1024, 2)), km["tables"].get((5120, 2))))

    # 7. HOTKEY (Numpad 5 = code 76) is dual-role: it has table entries (§5).
    check("HOTKEY dual_role", mods["HOTKEY"]["dual_role"] is True)
    check("HOTKEY tap cell (0,76) exists", (0, 76) in km["tables"],
          km["tables"].get((0, 76)))

    # 8. An absent whole combination misses entirely (§6): the file defines
    #    CTRL+SHIFT but NOT CTRL+ALT+SHIFT+CAPS. That full mask must be empty.
    full = 512 | 1024 | 2048 | 4096  # CTRL+ALT+SHIFT+CAPS
    absent = all((full, k) not in km["tables"] for k in range(1, 105))
    check("CTRL+ALT+SHIFT+CAPS layer is entirely absent (strict miss)", absent)

    # 9. Disposition wiring: a normal cell dispatches; an absent cell swallows;
    #    a synthetic KF cell flips. (KF doesn't appear in CLD4R, so we inject one.)
    d_dispatch = disposition(km, 0, 16)
    d_swallow = disposition(km, full, 16)
    km["tables"][(0, 200)] = "KF"
    d_flip = disposition(km, 0, 200)
    check("normal cell -> dispatch", d_dispatch[0] == "dispatch", d_dispatch)
    check("absent cell -> swallow", d_swallow[0] == "swallow", d_swallow)
    check("KF cell -> flip", d_flip[0] == "flip", d_flip)

    # ---- report ----
    width = max(len(n) for n, _, _ in checks)
    passed = 0
    print("=" * (width + 18))
    print("CLD4R.KBD oracle verification")
    print("=" * (width + 18))
    for name, ok, got in checks:
        status = "PASS" if ok else "FAIL"
        extra = "" if ok else f"   (got: {got!r})"
        print(f"[{status}] {name.ljust(width)}{extra}")
        passed += ok
    print("-" * (width + 18))
    print(f"{passed}/{len(checks)} checks passed")

    # Informational dump.
    print("\nParsed modifiers (header order, bottom-up bits):")
    for m in km["modifiers"]:
        print(f"  {m['name']:<8} bit={m['bit']:<5} {m['behavior']:<6} "
              f"triggers={m['triggers']} dual_role={m['dual_role']}")
    masks = sorted({b for (b, _) in km["tables"]})
    print(f"\nDistinct layers (bitmasks): {len(masks)} -> {masks}")
    print(f"Total cells: {len(km['tables'])}")

    if passed != len(checks):
        sys.exit(1)


if __name__ == "__main__":
    main()
