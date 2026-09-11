# shell.xpl — a simple flat line-regex grammar for shell scripts (.sh/.bash/.zsh).
#   RUN grammars/shell.xpl   (then open a .sh file; HIGHLIGHT on, or HLAUTO on)
#
# Per-line only. Rules are first-match-wins (comments/strings before keywords).
#
# `$variables` reuse the built-in `link` kind so they colour out of the box. The built-in
# kinds follow THEME light/dark; to recolour one, UNCOMMENT its `# HLCOLOR` line below and
# edit the RGB (shown at the light-theme default); see HLKINDS.

HLLANG shell .sh,.bash,.zsh,.ksh

HLRULE comment "#.*$"
# HLCOLOR comment 30 150 160
HLRULE string  "\"[^\"]*\"|'[^']*'"
# HLCOLOR string 40 150 70
HLRULE keyword "\b(if|then|elif|else|fi|for|while|until|do|done|case|esac|in|function|select|return|break|continue|local|export|readonly|declare|typeset|eval|exec|exit|shift|source|unset|trap)\b"
# HLCOLOR keyword 170 40 180
HLRULE link    "\$[A-Za-z_][A-Za-z0-9_]*|\$\{[^}]*\}"
# HLCOLOR link 50 90 200
HLRULE number  "\b[0-9]+\b"
# HLCOLOR number 200 60 60
