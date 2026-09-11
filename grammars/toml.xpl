# toml.xpl — a simple flat line-regex grammar for TOML config files (.toml).
#   RUN grammars/toml.xpl   (then open a .toml file; HIGHLIGHT on, or HLAUTO on)
#
# Rules are first-match-wins. Section headers reuse the built-in `heading` kind so
# they colour out of the box. The built-in kinds follow THEME light/dark; to recolour
# one, UNCOMMENT its `# HLCOLOR` line below and edit the RGB (shown at the light-theme
# default); see HLKINDS.

HLLANG toml .toml

HLRULE comment "#.*$"
# HLCOLOR comment 30 150 160
HLRULE heading "^\s*\[.*\]\s*$"
# HLCOLOR heading 30 150 160
HLRULE string  "\"[^\"]*\"|'[^']*'"
# HLCOLOR string 40 150 70
HLRULE keyword "\b(true|false)\b"
# HLCOLOR keyword 170 40 180
HLRULE number  "\b[0-9][0-9_]*\.?[0-9_]*\b"
# HLCOLOR number 200 60 60
