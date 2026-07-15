# toml.xpl — a simple flat line-regex grammar for TOML config files (.toml).
#   RUN grammars/toml.xpl   (then open a .toml file; HIGHLIGHT on, or HLAUTO on)
#
# Rules are first-match-wins. Section headers reuse the built-in `heading` kind so
# they colour out of the box (theme any kind with HLCOLOR — see HLKINDS).

HLLANG toml .toml

HLRULE comment "#.*$"
HLRULE heading "^\s*\[.*\]\s*$"
HLRULE string  "\"[^\"]*\"|'[^']*'"
HLRULE keyword "\b(true|false)\b"
HLRULE number  "\b[0-9][0-9_]*\.?[0-9_]*\b"
