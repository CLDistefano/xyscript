# zig.xpl — a flat line-regex syntax-highlight grammar for Zig (.zig).
#   RUN grammars/zig.xpl   (then open a .zig file; HIGHLIGHT on, or HLAUTO on)
#
# Per-line, first-match-wins (comments/strings before keywords). Zig has only `//` line comments
# (including `///` doc and `//!` module comments — all covered). Multiline string literals (lines
# beginning `\\`) are NOT highlighted (the grammar avoids `\\` in its patterns).
#
# The built-in kinds follow THEME light/dark. To recolour one, UNCOMMENT its `# HLCOLOR`
# line below and edit the RGB (shown at the light-theme default); see HLKINDS.

HLLANG zig .zig

HLRULE comment "//.*$"
# HLCOLOR comment 30 150 160
# Strings and character literals.
HLRULE string  "\"[^\"]*\"|'[^']*'"
# HLCOLOR string 40 150 70
# Builtin functions: @import, @intCast, @This, … (the `@name` form).
HLRULE function "@[A-Za-z_][A-Za-z0-9_]*"
# HLCOLOR function 150 150 50
# Keywords (incl. the literal words true/false/null/undefined).
HLRULE keyword "\b(addrspace|align|allowzero|and|anyframe|anytype|asm|async|await|break|callconv|catch|comptime|const|continue|defer|else|enum|errdefer|error|export|extern|fn|for|if|inline|linksection|noalias|noinline|nosuspend|null|opaque|or|orelse|packed|pub|resume|return|struct|suspend|switch|test|threadlocal|true|false|try|undefined|union|unreachable|usingnamespace|var|volatile|while)\b"
# HLCOLOR keyword 170 40 180
# Numbers: hex/octal/binary and decimal/float (with optional `_` digit separators).
HLRULE number  "\b(0[xX][0-9a-fA-F_]+|0o[0-7_]+|0b[01_]+|[0-9][0-9_]*\.?[0-9_]*)\b"
# HLCOLOR number 200 60 60
