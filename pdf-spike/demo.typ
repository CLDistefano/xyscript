// ── Document configuration (the "#set" rules cascade downward) ──
#set page(paper: "us-letter", margin: 1in, numbering: "1")
#set text(font: "New Computer Modern", size: 11pt)
#set par(justify: true)
#set heading(numbering: "1.1")
#show raw: set text(font: "DejaVu Sans Mono")

// ── Variables and a reusable function (the scripting layer) ──
#let project = "XyScript"
#let note(body) = block(
  fill: luma(240), inset: 8pt, radius: 4pt, width: 100%,
  [*Note:* #body],
)

= Introduction

This is *bold*, this is _italic_, and this is `inline code`.
Markup mode reads almost like Markdown — paragraphs are blank-line
separated, and #project is just a variable spliced inline. This paragraph is
long enough that Typst must perform automatic line breaking and justification
across the page width, which is exactly the layout work this backend owns.

- Bullet lists use a leading hyphen
- Nested items indent
  - like this
+ Numbered lists use `+`

#note[Anything in square brackets is *content* — it can hold more markup,
not just a string.]

== Math

Inline math like $a^2 + b^2 = c^2$ sits in `$...$`. Display math gets
its own block:

$ sum_(k=1)^n k = (n (n+1)) / 2 $

== A table

#table(
  columns: 3,
  align: (left, center, right),
  table.header[*Feature*][*Status*][*Version*],
  [FOR loops], [done], [0.9],
  [Map literals], [done], [0.9],
  [Typst export], [planned], [---],
)

== Scripting in the document

Content can be *computed*. A loop that emits markup:

#for lang in ("GFM", "Djot", "Typst") [
  / #lang: a candidate markup language. \
]

The total is #(2 + 40) features and counting.
