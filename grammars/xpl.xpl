# xpl.xpl — a flat line-regex syntax-highlight grammar for XyScript's own language, XPL.
#   RUN grammars/xpl.xpl   (then open a .xpl/.cfg/.lib file; HIGHLIGHT on, or HLAUTO on)
#
# Covers the structural pieces: `#` comments, "strings", :labels, $variables /
# @&binders, /SWITCHES, control-flow keywords, and numbers. Per-line, first-match-wins
# (so comments/strings are listed first). Verbs themselves aren't coloured — too many to
# enumerate — only control flow. Theme any kind with HLCOLOR (see HLKINDS).

HLLANG xpl .xpl,.pm,.pgm,.cfg,.lib

# `#` is a comment only when ws-delimited (so `#4` and `$#` are NOT comments).
HLRULE comment "(^|\s)#(\s.*)?$"
HLRULE string  "\"[^\"]*\""
# A `:label` (GOTO/GT target) at line start.
HLRULE heading "^\s*:[A-Za-z_][A-Za-z0-9_]*"
# Variables: $NAME, ${NAME}, $1, $*, $#.
HLRULE link    "\$\{[^}]*\}|\$[A-Za-z_][A-Za-z0-9_]*|\$[0-9]+|\$[*#]"
# Binders: @X text register, &X script register (X = A-Z / 0-9).
HLRULE link    "[@&][A-Za-z0-9]\b"
# Switches: /BG, /HSL, /R:var, … (uppercase, to avoid colliding with /paths).
HLRULE rule    "/[A-Z][A-Z0-9]*(:[A-Za-z0-9_]*)?"
HLRULE keyword "\b(IF|IFNOT|ELSE|ELSEIF|ENDIF|EI|WHILE|ENDWHILE|EW|SUB|ENDSUB|ES|CAPTURE|ENDCAPTURE|EC|BREAK|CONTINUE|GOTO|GT|RETURN|SET|LOCAL|CLEAR|AND|OR|NOT)\b"
HLRULE number  "\b[0-9][0-9_]*\.?[0-9_]*\b"
