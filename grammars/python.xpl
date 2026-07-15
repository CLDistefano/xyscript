# python.xpl — a simple flat line-regex syntax-highlight grammar for Python (.py).
#   RUN grammars/python.xpl   (then open a .py file; HIGHLIGHT on, or set HLAUTO on)
#
# Per-line only (no triple-quoted strings / multi-line constructs). Rules are
# first-match-wins, so comments and strings are listed before keywords/numbers.

HLLANG python .py,.pyw

HLRULE comment "#.*$"
HLRULE string  "\"[^\"]*\"|'[^']*'"
HLRULE keyword "\b(False|None|True|and|as|assert|async|await|break|class|continue|def|del|elif|else|except|finally|for|from|global|if|import|in|is|lambda|nonlocal|not|or|pass|raise|return|try|while|with|yield)\b"
HLRULE number  "\b[0-9][0-9_]*\.?[0-9_]*\b"
