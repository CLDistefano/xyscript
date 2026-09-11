# xpl.xpl — a flat line-regex syntax-highlight grammar for XyScript's own language, XPL.
#   RUN grammars/xpl.xpl   (then open a .xpl/.cfg/.lib file; HIGHLIGHT on, or HLAUTO on)
#
# Covers the structural pieces: `#` comments, "strings", :labels, $variables /
# @&binders, /SWITCHES, control-flow keywords, expression functions, and numbers. Per-line,
# first-match-wins (so comments/strings are listed first). Ordinary VERBS aren't coloured —
# too many to enumerate — only control-flow keywords and the built-in expression functions.
# Keywords/functions are matched case-insensitively (`(?i)`). Every built-in kind here (comment/string/
# heading/link/rule/number/keyword/function) follows THEME light/dark automatically. To recolour one,
# UNCOMMENT its `# HLCOLOR` line below and edit the RGB (shown at the light-theme default; see HLKINDS).

HLLANG xpl .xpl,.pm,.pgm,.cfg,.lib

# `#` is a comment only when ws-delimited (so `#4` and `$#` are NOT comments).
HLRULE comment "(^|\s)#(\s.*)?$"
# HLCOLOR comment 30 150 160
HLRULE string "[^\\]\".*[^\\]\""
# HLCOLOR string 40 150 70
# A `:label` (GOTO/GT target) at line start.
HLRULE heading "^\s*:[A-Za-z_][A-Za-z0-9_]*"
# HLCOLOR heading 30 150 160
# Variables: $NAME, ${NAME}, $1, $*, $#.
HLRULE link "\$\{[^}]*\}|\$[A-Za-z_][A-Za-z0-9_]*|\$[0-9]+|\$[*#]"
# Binders: @X text register, &X script register (X = A-Z / 0-9).
HLRULE link "[@&][A-Za-z0-9]\b"
# HLCOLOR link 50 90 200
# Switches: /BG, /HSL, /R:var, … (uppercase, to avoid colliding with /paths).
HLRULE rule    "/[A-Za-z0-9]*(:[A-Za-z0-9_]*)?"
# HLCOLOR rule 170 130 20
HLRULE number  "\b[0-9][0-9_]*\.?[0-9_]*\b"
# HLCOLOR number 200 60 60
# Control-flow keywords: block openers/closers (+ short aliases), loop/branch words, jumps, and the
# expression logical operators AND/OR/NOT. (GT is the focus-text verb, not a GOTO alias — not listed.)
HLRULE keyword "(?i)\b(ifnot|if|elseif|else|endif|ei|while|endwhile|ew|for|endfor|ef|to|in|step|switch|case|default|endswitch|esw|sub|endsub|es|capture|endcapture|ec|break|continue|goto|return|exitall|exit|ex1|ex|set|local|clear|and|or|not)\b"
# HLCOLOR keyword 170 40 180
# Built-in expression functions — the complete set (mirrors FUNCTION_NAMES in expr.rs). Longer names
# precede the shorter ones they contain (sinh|sin, sqrt|sq, splits|split, iterp|iter, atan2|atanh|atan,
# and the date* names before date).
HLRULE function "(?i)\b(len|upper|lower|reverse|strip|lstrip|rstrip|trimleft|trimright|substr|left|right|index|contains|replace|splits|split|join|chars|words|lines|chr|asc|format|isdec|ishex|isint|isfloat|isalpha|isalnum|isascii|isutf8|isupper|islower|istitle|isspace|isprint|ispunct|iterp|iter|push|pop|slice|sort|var|sel|abs|int|sqrt|sq|cube|fac|exp|ln|log|pow|base|hex|sinh|sin|cosh|cos|tanh|tan|asinh|asin|acosh|acos|atan2|atanh|atan|pi|e|random|key|getkey|getpath|exists|isdir|isfile|readfile|rf|datediff|dateadd|dateparse|date2jdn|jdn2date|date|dow|today|leap|daysinmonth|dayofyear|isdate|time)\b"
# HLCOLOR function 150 150 50
