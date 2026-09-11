# cpp.xpl — a flat line-regex syntax-highlight grammar for C and C++.
#   RUN grammars/cpp.xpl   (then open a .c/.h/.cpp/… file; HIGHLIGHT on, or HLAUTO on)
#
# Per-line only: a `/* … */` block comment that spans lines is coloured only where a rule hits
# (its opening line, and any line carrying a complete `/* … */`), not its interior — the grammar
# keeps no cross-line state (the same limitation the JS/Rust grammars note). Rules are
# first-match-wins, so comments and strings are listed before keywords/numbers.
#
# The built-in kinds follow THEME light/dark. To recolour one, UNCOMMENT its `# HLCOLOR`
# line below and edit the RGB (shown at the light-theme default); see HLKINDS.

HLLANG cpp .c,.h,.cpp,.cc,.cxx,.hpp,.hh,.hxx

# Comments: line `//…`, a complete single-line `/* … */` (LAZY, so two on a line stay separate),
# then a `/* …` that OPENS a block with no close on this line.
HLRULE comment "//.*$"
HLRULE comment "/\*.*?\*/"
HLRULE comment "/\*.*$"
# HLCOLOR comment 30 150 160
# Strings and character literals.
HLRULE string  "\"[^\"]*\"|'[^']*'"
# HLCOLOR string 40 150 70
# Preprocessor directives: #include, #define, #ifdef, #pragma, … (the word after `#`). Shares the
# `keyword` colour with the language keywords below.
HLRULE keyword "^\s*#\s*[A-Za-z_]+"
# Keywords — C plus the common C++ additions.
HLRULE keyword "\b(alignas|alignof|and|asm|auto|bool|break|case|catch|char|char8_t|char16_t|char32_t|class|const|consteval|constexpr|constinit|const_cast|continue|co_await|co_return|co_yield|decltype|default|delete|do|double|dynamic_cast|else|enum|explicit|export|extern|false|final|float|for|friend|goto|if|inline|int|long|mutable|namespace|new|noexcept|nullptr|operator|or|override|private|protected|public|register|reinterpret_cast|requires|restrict|return|short|signed|sizeof|static|static_assert|static_cast|struct|switch|template|this|thread_local|throw|true|try|typedef|typeid|typename|union|unsigned|using|virtual|void|volatile|wchar_t|while)\b"
# HLCOLOR keyword 170 40 180
# Numbers: hex/binary and decimal/float (digit separators `'` allowed, C++14-style).
HLRULE number  "\b(0[xX][0-9a-fA-F']+|0[bB][01']+|[0-9][0-9']*\.?[0-9']*)\b"
# HLCOLOR number 200 60 60
