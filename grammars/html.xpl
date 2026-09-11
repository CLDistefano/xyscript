# html.xpl — a flat line-regex syntax-highlight grammar for HTML.
#   RUN grammars/html.xpl   (then open an .html/.htm/.xhtml file; HIGHLIGHT on, or HLAUTO on)
#
# Per-line, first-match-wins (so comments/strings are listed before tags). Covers: `<!-- -->`
# comments, tag delimiters + names, quoted attribute values, attribute names, `<!DOCTYPE …>`
# declarations, and `&…;` entities. A single-line comment (or the OPENING line of a multi-line
# one) is coloured; the interior lines of a multi-line comment are NOT — the grammar keeps no
# cross-line state (the same limitation the shipped JS/Rust grammars note for block comments).
# Embedded CSS/JS inside <style>/<script> isn't separately highlighted. The built-in kinds follow THEME
# light/dark; to recolour one, UNCOMMENT its `# HLCOLOR` line below and edit the RGB (shown at the
# light-theme default). Where a kind drives two rules (comment, keyword), one HLCOLOR covers both. See HLKINDS.

HLLANG html .html,.htm,.xhtml

# Comments — two rules, in order. First a complete single-line `<!-- … -->` (LAZY, so it stops at the
# first `-->` and doesn't swallow following tags, and two comments on a line stay separate). Then, on a
# line that OPENS a comment with no close, from `<!--` to the end of the line. (First-match-wins keeps
# the second rule off a line the first already claimed.)
HLRULE comment "<!--.*?-->"
HLRULE comment "<!--.*$"
# HLCOLOR comment 30 150 160
# Quoted attribute values (double or single quoted).
HLRULE string  "\"[^\"]*\"|'[^']*'"
# HLCOLOR string 40 150 70
# Doctype / markup declarations: `<!DOCTYPE …>`, `<!ENTITY …>` (word after `<!`, so NOT a comment).
HLRULE keyword "<!\w[^>]*>"
# Tag delimiters + tag name: `<div`  `</div`  `<br/`  plus the closing `>` and `/>`.
HLRULE keyword "</?[a-zA-Z][a-zA-Z0-9:-]*|/?>"
# HLCOLOR keyword 170 40 180
# Attribute names, through the `=` (no lookahead in this regex engine).
HLRULE link    "[a-zA-Z_:][a-zA-Z0-9_:.-]*="
# HLCOLOR link 50 90 200
# Entities: `&amp;`  `&#123;`  `&#xAB;`
HLRULE number  "&#?[a-zA-Z0-9]+;"
# HLCOLOR number 200 60 60
