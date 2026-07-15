# typst.xpl — a flat line-regex syntax-highlight grammar for Typst (.typ).
#
# Load it, then open a .typ file and turn highlighting on:
#     RUN grammars/typst.xpl
#     OPEN report.typ
#     HIGHLIGHT on
#
# Or make it automatic every session — put these in your startup.xpl (beside the exe,
# or pass -s <path>), and any .typ you open highlights with no extra step:
#     RUN grammars/typst.xpl
#     HLAUTO on
#
# Each line is highlighted independently (no multi-line constructs — a raw block ```…```
# or a /* … */ comment that spans lines is only coloured line-by-line where a rule hits).
# Rules apply in order with FIRST-MATCH-WINS, and a later match is dropped if it overlaps
# a span an earlier rule already claimed — so the whole-line HEADING rule comes first, then
# the span-claiming literals (comments, strings, raw), then the rest.
#
# `<kind>` is any name. The built-ins (heading/keyword/string/comment/number/strong/…)
# come pre-themed; a custom kind (here, `math`) is coloured explicitly at the bottom.
# Recolour any kind yourself with `HLCOLOR <kind> <r g b>` (see HLKINDS).

HLLANG typst .typ

# Whole-line markup heading: "= Title", "== Section", … (claimed first so the rest of the
# heading line isn't recoloured by the inline rules below).
HLRULE heading  "^\s*=+\s.*$"

# Span-claiming literals first.
HLRULE comment  "//.*$"
HLRULE string   "\"[^\"]*\""
HLRULE code     "`[^`]*`"

# Code mode: a '#' sigil introduces a keyword, function call, or variable reference
# (#let, #set, #show, #import, #table, #figure, …). This is the most Typst-distinctive mark.
HLRULE keyword  "#[A-Za-z][A-Za-z0-9_-]*"

# Inline math: $ … $  (custom kind — coloured at the bottom).
HLRULE math     "\$[^$]*\$"

# Emphasis markers.
HLRULE strong   "\*[^*]+\*"
HLRULE emphasis "_[^_]+_"

# Numbers (covers the leading digits of lengths like 2cm / 12pt too).
HLRULE number   "\b[0-9]+(\.[0-9]+)?\b"

# `math` is a custom kind, so give it a colour (the built-ins above are already themed).
HLCOLOR math 30 130 90
