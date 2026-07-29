#!/usr/bin/env python3
"""Generate a pseudolocalization catalog from the English string template.

For each `key =   # English` entry, fill the value with a pseudo-translation of
the English label:
  * letters get safe Latin-1 accents (untranslated raw ASCII then stands out,
    and it exercises non-ASCII font coverage),
  * the text is padded ~40% longer to surface layout overflow,
  * it's wrapped in [ ... ] so a clipped end is obvious,
  * {placeholders} are preserved verbatim.
Section dividers and comments pass through; the intro header is replaced.
"""
import re, sys

SRC, DST = sys.argv[1], sys.argv[2]

# Safe, universally-available Latin-1 accents (readable, no tofu).
ACCENT = str.maketrans({
    'a': 'à', 'e': 'è', 'i': 'ì', 'o': 'ò', 'u': 'ù', 'y': 'ý', 'n': 'ñ', 'c': 'ç',
    'A': 'À', 'E': 'È', 'I': 'Ì', 'O': 'Ò', 'U': 'Ù', 'Y': 'Ý', 'N': 'Ñ', 'C': 'Ç',
})

def pseudo(label: str) -> str:
    # Accent text but keep {placeholders} untouched.
    body = ''.join(seg if (seg.startswith('{') and seg.endswith('}'))
                   else seg.translate(ACCENT)
                   for seg in re.split(r'(\{[^}]*\})', label))
    pad = max(2, round(len(label) * 0.4))
    return f"[{body} {'·' * pad}]"

HEADER = """\
# =============================================================================
# XyScript UI strings  —  PSEUDOLOCALIZATION  (generated; do not translate)
# =============================================================================
# A fake locale for LAYOUT TESTING, generated from xyscript-strings.template.
# Load it like any language file and eyeball every menu / dialog:
#   * If a string is NOT bracketed+accented (plain ASCII), it was NOT routed
#     through the string table — a localization gap to fix.
#   * If you can't see the closing "]", the text is being CLIPPED.
#   * Each value is ~40% longer than English to shake out overflow (the tight
#     menu bar and any hard-coded widths like the clipboard menu's min_width).
#   * {placeholders} are preserved — they must survive translation intact.
# Regenerate with: gen_pseudo.py xyscript-strings.template xyscript-strings-pseudo.txt
# =============================================================================

"""

ENTRY = re.compile(r'^([A-Za-z][\w.]*)\s*=\s*#\s*(.*)$')

out = [HEADER]
started = False
for raw in open(SRC, encoding='utf-8'):
    line = raw.rstrip('\n')
    # Drop the original intro header; begin at the MENUS section divider.
    if not started:
        if line.strip().endswith('1. MENUS'):
            out.append('# =============================================================================')
            out.append(line)
            started = True
        continue
    m = ENTRY.match(line)
    if m:
        key, comment = m.group(1), m.group(2)
        label = re.split(r'\s{2,}', comment, maxsplit=1)[0].strip()  # strip my (submenu)-style notes
        out.append(f"{key} = {pseudo(label)}   # {comment}")
    else:
        out.append(line)  # section dividers, comments, blanks

open(DST, 'w', encoding='utf-8').write('\n'.join(out) + '\n')
print(f"wrote {DST}: {sum(1 for l in out if ENTRY.match(l))} entries")
