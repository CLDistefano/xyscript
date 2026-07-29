#!/usr/bin/env python3
"""
i18n-sync — keep the XyScript UI-string catalogs in sync with the code.

The code is the single source of truth: every `tr("key", "English")` /
`tr_msg` / `tr_fmt` call in the Rust sources carries both the string id and its
English text. This tool:

  1. EXTRACTS every (key, English) pair from the sources    -> writes en.lang
  2. MERGES en.lang into each <xx>.lang (gettext msgmerge-style):
       - new keys are added with a blank value (English fallback until translated)
       - keys removed from the code are dropped
       - a key whose English text CHANGED is re-flagged `#, fuzzy` (the old
         translation is kept but marked for review) and stays fuzzy until a
         translator edits the value and deletes the marker
     Existing good translations are never touched.
  3. Regenerates pseudo.lang (a layout smoke-test locale).

Usage:
  python i18n-sync.py                 # regenerate en.lang + pseudo.lang, merge every <xx>.lang in place
  python i18n-sync.py --check         # verify the checked-in files are up to date (CI gate; nonzero on drift)
  python i18n-sync.py --new sv "SVENSKA" "Swedish"   # seed a fresh sv.lang from en.lang (blank values)

Only .lang files are the deliverables; this script is the only thing that writes them.
"""
import io
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", ".."))
SOURCES = [
    os.path.join(ROOT, "xyscript-gui", "src", "main.rs"),   # menu + dialog labels (tr)
    os.path.join(ROOT, "xyscript-app", "src", "host.rs"),   # status/error messages (tr_msg/tr_fmt)
]

# Known sample languages: code -> (native name, English name). A .lang file for a code not
# listed here still merges fine; its header just uses the bare code.
LANGS = {
    "fr": ("FRANÇAIS", "French"),
    "de": ("DEUTSCH", "German"),
    "es": ("ESPAÑOL", "Spanish"),
    "it": ("ITALIANO", "Italian"),
    "nl": ("NEDERLANDS", "Dutch"),
    "fi": ("SUOMI", "Finnish"),
}

STR = r'"((?:\\.|[^"\\])*)"'  # a Rust string literal (captures its contents)
# `.tr("id", "English"...)`, `.tr_msg("id", "English")`, `.tr_fmt("id", "English", ...)`.
TR = re.compile(r'\.tr(?:_msg|_fmt)?\(\s*' + STR + r'\s*,\s*' + STR)
# `.tr_plural("base", "English one", "English other", n)` → keys "base.one" and "base.other".
PLURAL = re.compile(r'\.tr_plural\(\s*' + STR + r'\s*,\s*' + STR + r'\s*,\s*' + STR)
KEY = re.compile(r'^[a-z][a-zA-Z0-9._]*$')
DELIM = "   # "  # separates a translation (value) from its English anchor on an entry line
# A template-era alignment hint like `Foo            (submenu)` — 2+ spaces then a paren group at
# end. Stripped only for the first-run fuzzy comparison; the real English never has that shape.
HINT = re.compile(r"\s{2,}\([^)]*\)\s*$")


def read(path):
    return io.open(path, encoding="utf-8").read()


def unesc(s):
    # Rust string-literal escapes that appear in these labels (quotes/backslash). The labels use
    # real Unicode (…, ⏵, ·) directly, so no \u handling is needed.
    return s.replace('\\"', '"').replace("\\\\", "\\")


def extract():
    """Ordered [(key, english)] as first seen across the sources (menus, then messages). Plain
    tr()/tr_msg()/tr_fmt() contribute one key each; tr_plural() contributes both `.one` and `.other`.
    Matches from both patterns are merged in source order so en.lang stays UI-ordered."""
    order, seen = [], {}

    def add(key, english):
        if KEY.match(key) and key not in seen:
            seen[key] = english
            order.append(key)

    for path in SOURCES:
        text = read(path)
        events = [(m.start(), [(m.group(1), unesc(m.group(2)))]) for m in TR.finditer(text)]
        for m in PLURAL.finditer(text):
            base = m.group(1)
            events.append((m.start(), [
                (f"{base}.one", unesc(m.group(2))),
                (f"{base}.other", unesc(m.group(3))),
            ]))
        events.sort(key=lambda e: e[0])
        for _, pairs in events:
            for key, english in pairs:
                add(key, english)
    return [(k, seen[k]) for k in order]


# Top-level ordering for the generated files, so they read top-down like the UI. Within a rank,
# the original source order is preserved (a stable sort), which keeps menu items in menu order.
RANK = {"menubar": 0, "menu": 1, "dialog": 2, "button": 3, "msg": 4}


def rank(key):
    return RANK.get(key.split(".")[0], 9)


def section(key):
    """A readable group label for `key`, so en.lang stays organized by UI area."""
    p = key.split(".")
    if p[0] == "menubar":
        return "menu bar"
    if p[0] in ("menu", "dialog") and len(p) >= 2:
        return f"{p[0]}.{p[1]}"
    return p[0]  # button, msg, …


def render_en(pairs):
    out = [
        "# =============================================================================",
        "# XyScript UI strings — ENGLISH (source of truth)",
        "# =============================================================================",
        "# GENERATED by i18n-sync.py from the tr()/tr_msg()/tr_fmt() calls in the Rust",
        "# sources — do NOT edit by hand. Each <xx>.lang is merged against this file.",
        "# =============================================================================",
        "",
    ]
    last = None
    for key, english in pairs:
        sec = section(key)
        if sec != last:
            out += ["", f"# ---- {sec} " + "-" * max(0, 60 - len(sec))]
            last = sec
        out.append(f"{key} = {english}")
    return "\n".join(out) + "\n"


def parse_lang(path):
    """Existing <xx>.lang -> {key: (value, anchor_english, fuzzy)}. Comments/markers drive `fuzzy`."""
    if not os.path.exists(path):
        return {}
    entries, pending_fuzzy = {}, False
    for raw in io.open(path, encoding="utf-8"):
        line = raw.rstrip("\n")
        s = line.strip()
        if s.startswith("#"):
            if s.startswith("#, fuzzy"):
                pending_fuzzy = True
            continue
        if "=" not in line:
            continue
        i = line.index("=")
        key = line[:i].strip()
        rest = line[i + 1:]
        value, anchor = (rest.split(DELIM, 1) + [""])[:2] if DELIM in rest else (rest, "")
        entries[key] = (value.strip(), anchor.strip(), pending_fuzzy)
        pending_fuzzy = False
    return entries


def lang_header(code, untranslated, fuzzy):
    native, eng = LANGS.get(code, (code.upper(), code))
    return [
        "# =============================================================================",
        f"# XyScript UI strings — {native} ({eng})",
        "# =============================================================================",
        "# Translators: edit the value to the RIGHT of '='. The trailing '# English' is the",
        "# source string; if it changes, i18n-sync re-flags the entry '#, fuzzy'. A blank",
        "# value falls back to English. Keep every {placeholder} intact. Load with:",
        f"#   LANG <path>\\{code}.lang",
        f"# Merged from en.lang by i18n-sync.py — {untranslated} untranslated, {fuzzy} fuzzy.",
        "# =============================================================================",
        "",
    ]


def merge(pairs, code, path):
    """Return (text, untranslated, fuzzy) for <code>.lang, preserving existing translations."""
    old = parse_lang(path)
    body, untranslated, fuzzy = [], 0, 0
    last = None
    for key, english in pairs:
        value, was_fuzzy = "", False
        if key in old:
            value, anchor, was_fuzzy = old[key]
            english_changed = HINT.sub("", anchor).strip() != english
            was_fuzzy = was_fuzzy or (english_changed and value != "")
        if value == "":
            untranslated += 1
            was_fuzzy = False  # nothing to review yet
        sec = section(key)
        if sec != last:
            body += ["", f"# ---- {sec} " + "-" * max(0, 60 - len(sec))]
            last = sec
        if was_fuzzy:
            fuzzy += 1
            body.append("#, fuzzy")
        body.append(f"{key} = {value}{DELIM}{english}")
    text = "\n".join(lang_header(code, untranslated, fuzzy) + body) + "\n"
    return text, untranslated, fuzzy


# ---- pseudolocalization (layout smoke test) --------------------------------------------------
ACCENT = str.maketrans({
    "a": "à", "b": "ƀ", "c": "ç", "d": "đ", "e": "è", "f": "ƒ", "g": "ĝ", "h": "ĥ", "i": "ì",
    "j": "ĵ", "k": "ķ", "l": "ĺ", "m": "ɱ", "n": "ñ", "o": "ò", "p": "þ", "q": "ɋ", "r": "ŕ",
    "s": "š", "t": "ţ", "u": "ù", "v": "ṽ", "w": "ŵ", "x": "ẋ", "y": "ý", "z": "ž",
    "A": "À", "B": "Ɓ", "C": "Ç", "D": "Đ", "E": "È", "F": "Ƒ", "G": "Ĝ", "H": "Ĥ", "I": "Ì",
    "J": "Ĵ", "K": "Ķ", "L": "Ĺ", "M": "Ɱ", "N": "Ñ", "O": "Ò", "P": "Þ", "Q": "Ɋ", "R": "Ŕ",
    "S": "Š", "T": "Ţ", "U": "Ù", "V": "Ṽ", "W": "Ŵ", "X": "Ẋ", "Y": "Ý", "Z": "Ž",
})
PLACEHOLDER = re.compile(r"\{[^}]*\}")


def pseudo_value(english):
    parts, out = PLACEHOLDER.split(english), []
    holes = PLACEHOLDER.findall(english)
    for i, seg in enumerate(parts):
        out.append(seg.translate(ACCENT))
        if i < len(holes):
            out.append(holes[i])  # keep {placeholders} verbatim
    body = "".join(out)
    pad = "·" * max(1, round(len("".join(parts)) * 0.4))
    return f"[{body} {pad}]"


def render_pseudo(pairs):
    out = [
        "# =============================================================================",
        "# XyScript UI strings — PSEUDO (layout smoke test)",
        "# =============================================================================",
        "# GENERATED by i18n-sync.py. Accented + padded + bracketed English, {placeholders}",
        "# preserved. Load it (LANG <path>\\pseudo.lang) to spot un-localized text (anything",
        "# plain-ASCII and un-bracketed in the UI is a string NOT routed through tr()) and to",
        "# stress-test layout against ~40% longer strings.",
        "# =============================================================================",
        "",
    ]
    last = None
    for key, english in pairs:
        sec = section(key)
        if sec != last:
            out += ["", f"# ---- {sec} " + "-" * max(0, 60 - len(sec))]
            last = sec
        out.append(f"{key} = {pseudo_value(english)}   # {english}")
    return "\n".join(out) + "\n"


def lang_files():
    return sorted(
        f for f in os.listdir(HERE)
        if re.fullmatch(r"[a-z]{2}\.lang", f) and f != "en.lang"
    )


def main():
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8")  # native language names etc. print on any console
        except AttributeError:
            pass
    args = sys.argv[1:]
    check = "--check" in args
    pairs = extract()
    pairs.sort(key=lambda kv: rank(kv[0]))  # stable: logical section order, source order within

    # --new <code> [native] [english]: seed a blank catalog, then fall through to a normal merge.
    if "--new" in args:
        i = args.index("--new")
        code = args[i + 1]
        if len(args) > i + 3:
            LANGS[code] = (args[i + 2], args[i + 3])
        path = os.path.join(HERE, f"{code}.lang")
        if os.path.exists(path):
            print(f"{code}.lang already exists — merging instead of seeding.")
        else:
            text, unt, fz = merge(pairs, code, path)  # no old file → all blank
            io.open(path, "w", encoding="utf-8", newline="\n").write(text)
            print(f"seeded {code}.lang — {unt} strings to translate")
        return 0

    planned = {"en.lang": render_en(pairs), "pseudo.lang": render_pseudo(pairs)}
    reports = []
    for f in lang_files():
        code = f[:2]
        text, unt, fz = merge(pairs, code, os.path.join(HERE, f))
        planned[f] = text
        reports.append((f, unt, fz))

    drift = []
    for name, text in planned.items():
        path = os.path.join(HERE, name)
        current = read(path) if os.path.exists(path) else None
        if current != text:
            drift.append(name)
        if not check:
            io.open(path, "w", encoding="utf-8", newline="\n").write(text)

    print(f"{len(pairs)} UI strings across {len(lang_files())} language(s)")
    for f, unt, fz in reports:
        flag = f"  !! {fz} FUZZY" if fz else ""
        print(f"  {f}: {unt} untranslated{flag}")
    total_fuzzy = sum(fz for _, _, fz in reports)

    if check:
        if drift:
            print(f"\nOUT OF DATE: {', '.join(drift)} — run `python i18n-sync.py`.", file=sys.stderr)
            return 1
        if total_fuzzy:
            print(f"\n{total_fuzzy} fuzzy entry(ies) need review (see `#, fuzzy`).", file=sys.stderr)
            return 1
        print("in sync.")
        return 0
    if total_fuzzy:
        print(f"\n{total_fuzzy} fuzzy entry(ies) flagged — search `#, fuzzy` in the .lang files.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
