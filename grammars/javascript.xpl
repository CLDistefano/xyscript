# javascript.xpl — a simple flat line-regex grammar for JavaScript/TypeScript.
#   RUN grammars/javascript.xpl   (then open a .js/.ts/… file; HIGHLIGHT on, or HLAUTO on)
#
# Per-line only (no /* */ block comments or multi-line template literals). Rules are
# first-match-wins, so comments and strings precede keywords/numbers.

HLLANG javascript .js,.mjs,.cjs,.ts,.jsx,.tsx

HLRULE comment "//.*$"
HLRULE string  "\"[^\"]*\"|'[^']*'|`[^`]*`"
HLRULE keyword "\b(var|let|const|function|return|if|else|for|while|do|switch|case|default|break|continue|new|delete|typeof|instanceof|in|of|this|class|extends|super|import|export|from|as|async|await|yield|try|catch|finally|throw|void|null|undefined|true|false|interface|type|enum|implements|readonly)\b"
HLRULE number  "\b[0-9][0-9_]*\.?[0-9_]*\b"
