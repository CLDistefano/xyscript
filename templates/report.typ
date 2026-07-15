// ─────────────────────────────────────────────────────────────────────────────
// report.typ — a starter Typst document for XyScript.
//
// Open it, edit the fields below, then TOPDF to export a PDF. A .typ file renders
// as Typst automatically (DOCTYPE auto); the document's own #set rules control the
// page, so what you write is what you get. Syntax-highlight it with:
//     RUN grammars/typst.xpl
//     HIGHLIGHT on
// ─────────────────────────────────────────────────────────────────────────────

// ---- Fill these in -----------------------------------------------------------
#let title  = "Project Report"
#let author = "Your Name"
#let date   = "2026-06-23"

// ---- Page + text configuration (edit to taste) -------------------------------
#set page(paper: "us-letter", margin: 1in, numbering: "1")
#set text(font: "New Computer Modern", size: 11pt)
#set par(justify: true, leading: 0.65em)
#set heading(numbering: "1.1")

// ---- A small component library (reusable #let helpers) -----------------------
#let note(body) = block(
  fill: luma(240), stroke: 0.5pt + luma(180), radius: 4pt, inset: 8pt, width: 100%,
  [*Note.* #body],
)
#let todo(body) = text(fill: red)[*TODO:* #body]

// ---- Title block -------------------------------------------------------------
#align(center)[
  #text(size: 20pt, weight: "bold")[#title] \
  #v(4pt)
  #author #h(1fr) #date
]
#v(6pt)
#line(length: 100%, stroke: 0.5pt)
#v(8pt)

// ---- Body --------------------------------------------------------------------
= Introduction

Replace this with your opening. Body text is justified, and *bold* / _italic_ work
as usual. Inline code looks like `let x = 1`.

#note[Anything in square brackets is _content_ — it can hold more markup, not just a
string, so components like this can wrap whole paragraphs.]

= Method

Inline math such as $a^2 + b^2 = c^2$ sits in dollar signs; a display equation gets
its own block:

$ sum_(k = 1)^n k = (n (n + 1)) / 2 $

= Results

#figure(
  table(
    columns: 3,
    align: (left, center, right),
    table.header[*Item*][*Status*][*Value*],
    [Alpha], [done],    [12.0],
    [Beta],  [planned], [3.5],
  ),
  caption: [Example results table.],
)

= Discussion

#todo[write this section]
