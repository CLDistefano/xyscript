# shell.xpl — a simple flat line-regex grammar for shell scripts (.sh/.bash/.zsh).
#   RUN grammars/shell.xpl   (then open a .sh file; HIGHLIGHT on, or HLAUTO on)
#
# Per-line only. Rules are first-match-wins (comments/strings before keywords).

HLLANG shell .sh,.bash,.zsh,.ksh

HLRULE comment "#.*$"
HLRULE string  "\"[^\"]*\"|'[^']*'"
HLRULE keyword "\b(if|then|elif|else|fi|for|while|until|do|done|case|esac|in|function|select|return|break|continue|local|export|readonly|declare|typeset|eval|exec|exit|shift|source|unset|trap)\b"
HLRULE link    "\$[A-Za-z_][A-Za-z0-9_]*|\$\{[^}]*\}"
HLRULE number  "\b[0-9]+\b"
