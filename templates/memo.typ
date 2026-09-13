// ─────────────────────────────────────────────────────────────────────────────
// memo.typ — a starter Typst interoffice memo for XyScript.
//
// Open it, fill in the header below, then TOPDF to export a PDF. A .typ file
// renders as Typst automatically (DOCTYPE auto). Syntax-highlight it with:
//     RUN grammars/typst.xpl
//     HIGHLIGHT on
// ─────────────────────────────────────────────────────────────────────────────

// ---- Fill these in -----------------------------------------------------------
#let org  = "Company Name"
#let memo = (
  to:   "All Staff",
  from: "Your Name, Your Title",
  date: "September 13, 2026",
  re:   "Subject of this memo",
  cc:   "",              // leave "" to omit the Cc line
)

// ---- Page + text configuration (edit to taste) -------------------------------
#set page(paper: "us-letter", margin: 1in)
#set text(font: "New Computer Modern", size: 11pt)
#set par(justify: false, leading: 0.65em, spacing: 1.1em)

// ---- Masthead ----------------------------------------------------------------
#align(center)[
  #text(size: 13pt, weight: "bold")[#org] \
  #text(size: 18pt, weight: "bold", tracking: 2pt)[MEMORANDUM]
]
#v(0.6em)
#line(length: 100%, stroke: 0.75pt)
#v(0.8em)

// ---- Header block (aligned labels) -------------------------------------------
#let row(label, value) = (
  text(weight: "bold")[#label], value,
)
#grid(
  columns: (auto, 1fr),
  column-gutter: 1em,
  row-gutter: 0.6em,
  ..row("TO:",   memo.to),
  ..row("FROM:", memo.from),
  ..row("DATE:", memo.date),
  ..row("RE:",   memo.re),
  ..if memo.cc != "" { row("CC:", memo.cc) } else { () },
)
#v(0.6em)
#line(length: 100%, stroke: 0.5pt)
#v(1em)

// ---- Body — replace with your memo -------------------------------------------
Open with the point of the memo in one or two sentences: what this is about and
what, if anything, the reader must do.

Use the body to lay out the details. For several items, a list keeps them scannable:

- First point, with any specifics the reader needs.
- Second point.
- Third point.

Close with the action you want and any deadline — for example, "Please reply by
Friday" — so no one is left wondering what happens next.
