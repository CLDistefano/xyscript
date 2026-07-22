# xpl.xpl — a flat line-regex syntax-highlight grammar for XyScript's own language, XPL.
#   RUN grammars/xpl.xpl   (then open a .xpl/.cfg/.lib file; HIGHLIGHT on, or HLAUTO on)
#
# Covers the structural pieces: `#` comments, "strings", :labels, $variables /
# @&binders, /SWITCHES, control-flow keywords, expression functions, and numbers. Per-line,
# first-match-wins (so comments/strings are listed first). Ordinary VERBS aren't coloured —
# too many to enumerate — only control-flow keywords and the built-in expression functions.
# Keywords/functions are matched case-insensitively (`(?i)`). `function` is a first-class kind with a
# theme-aware default (see highlight.rs), so it colours itself and follows THEME light/dark — override
# it with HLCOLOR if you want a different colour (see HLKINDS).

HLLANG xpl .xpl,.pm,.pgm,.cfg,.lib

# `#` is a comment only when ws-delimited (so `#4` and `$#` are NOT comments).
HLRULE comment "(^|\s)#(\s.*)?$"
HLRULE string "[^\\]\".*[^\\]\""
# A `:label` (GOTO/GT target) at line start.
HLRULE heading "^\s*:[A-Za-z_][A-Za-z0-9_]*"
# Variables: $NAME, ${NAME}, $1, $*, $#.
HLRULE link "\$\{[^}]*\}|\$[A-Za-z_][A-Za-z0-9_]*|\$[0-9]+|\$[*#]"
# Binders: @X text register, &X script register (X = A-Z / 0-9).
HLRULE link "[@&][A-Za-z0-9]\b"
# Switches: /BG, /HSL, /R:var, … (uppercase, to avoid colliding with /paths).
HLRULE rule    "/[A-Za-z0-9]*(:[A-Za-z0-9_]*)?"
HLRULE number  "\b[0-9][0-9_]*\.?[0-9_]*\b"
# Control-flow keywords: block openers/closers (+ short aliases), loop/branch words, jumps, and the
# expression logical operators AND/OR/NOT. (GT is the focus-text verb, not a GOTO alias — not listed.)
HLRULE keyword "(?i)\b(ifnot|if|elseif|else|endif|ei|while|endwhile|ew|for|endfor|ef|to|in|step|switch|case|default|endswitch|esw|sub|endsub|es|capture|endcapture|ec|break|continue|goto|return|exitall|exit|ex1|ex|set|local|clear|and|or|not)\b"
# Built-in expression functions — the complete set (mirrors FUNCTION_NAMES in expr.rs). Longer names
# precede the shorter ones they contain (sinh|sin, sqrt|sq, splits|split, iterp|iter, atan2|atanh|atan).
HLRULE function "(?i)\b(len|upper|lower|strip|lstrip|rstrip|trimleft|trimright|substr|left|right|index|contains|replace|splits|split|join|chars|words|lines|chr|asc|isdec|ishex|isint|isfloat|isalpha|isalnum|isascii|isutf8|isupper|islower|istitle|isspace|isprint|ispunct|iterp|iter|push|pop|slice|sort|var|sel|abs|int|sqrt|sq|cube|fac|exp|ln|log|pow|base|hex|sinh|sin|cosh|cos|tanh|tan|asinh|asin|acosh|acos|atan2|atanh|atan|pi|e|random|key|getpath|exists|isdir|isfile|readfile|rf|date|time)\b"
