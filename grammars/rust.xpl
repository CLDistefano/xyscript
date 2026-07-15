# rust.xpl — a simple flat line-regex syntax-highlight grammar for Rust (.rs).
#
# Load it, then open a .rs file and turn highlighting on:
#     RUN grammars/rust.xpl
#     OPEN src/main.rs
#     HIGHLIGHT on
#
# Or make it automatic every session — put these in your startup.xpl (beside the exe,
# or pass -s <path>), and any .rs you open highlights with no extra step:
#     RUN grammars/rust.xpl
#     HLAUTO on
#
# Each line is highlighted independently (no multi-line constructs). Rules apply in
# order with FIRST-MATCH-WINS, so list the "claiming" rules (comments, strings)
# BEFORE keywords/numbers — once a span is coloured a later rule can't recolour it.
#
# `<kind>` is any name. The built-ins (keyword/string/comment/number/…) come pre-
# themed; colour any kind yourself with `HLCOLOR <kind> <r g b>` (see HLKINDS).

HLLANG rust .rs

HLRULE comment "//.*$"
HLRULE string  "\"[^\"]*\""
HLRULE keyword "\b(as|async|await|break|const|continue|crate|dyn|else|enum|fn|for|if|impl|in|let|loop|match|mod|move|mut|pub|ref|return|self|Self|static|struct|trait|type|unsafe|use|where|while)\b"
HLRULE number  "\b[0-9][0-9_]*\b"
