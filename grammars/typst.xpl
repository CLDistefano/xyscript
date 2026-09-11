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
# follow THEME light/dark; to recolour one, UNCOMMENT its `# HLCOLOR` line and edit the RGB
# (shown at the light-theme default). A custom kind (here `math`) has no theme default, so it
# carries a live HLCOLOR. See HLKINDS.

HLLANG typst .typ

# Whole-line markup heading: "= Title", "== Section", … (claimed first so the rest of the
# heading line isn't recoloured by the inline rules below).
HLRULE heading  "^\s*=+\s.*$"
# HLCOLOR heading 30 150 160

# Span-claiming literals first.
HLRULE comment  "//.*$"
# HLCOLOR comment 30 150 160
HLRULE string   "\"[^\"]*\""
# HLCOLOR string 40 150 70
HLRULE code     "`[^`]*`"
# HLCOLOR code 40 150 70

# Code mode: a '#' sigil introduces a keyword, function call, or variable reference
# (#let, #set, #show, #import, #table, #figure, …). This is the most Typst-distinctive mark.
HLRULE keyword  "#[A-Za-z][A-Za-z0-9_-]*"
# HLCOLOR keyword 170 40 180

# Inline math: $ … $  — a custom kind (no theme default), so it carries a LIVE colour.
HLRULE math     "\$[^$]*\$"
HLCOLOR math 30 130 90

# Emphasis markers.
HLRULE strong   "\*[^*]+\*"
# HLCOLOR strong 30 30 30
HLRULE emphasis "_[^_]+_"
# HLCOLOR emphasis 30 30 30

# Numbers (covers the leading digits of lengths like 2cm / 12pt too).
HLRULE number   "\b[0-9]+(\.[0-9]+)?\b"
# HLCOLOR number 200 60 60
