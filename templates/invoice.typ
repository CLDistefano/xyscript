// ─────────────────────────────────────────────────────────────────────────────
// invoice.typ — a starter Typst invoice for XyScript.
//
// Open it, fill in the fields and line items below, then TOPDF to export a PDF.
// A .typ file renders as Typst automatically (DOCTYPE auto). The totals compute
// themselves from the `items` list — edit the rows and they re-add. Highlight with:
//     RUN grammars/typst.xpl
//     HIGHLIGHT on
// ─────────────────────────────────────────────────────────────────────────────

// ---- Fill these in -----------------------------------------------------------
#let from = (
  name:    "Your Company",
  address: "123 Main Street, Suite 100",
  city:    "City, ST 00000",
  email:   "billing@example.com",
)
#let bill_to = (
  name:    "Client Name",
  company: "Client Company",
  address: "456 Market Avenue",
  city:    "City, ST 00000",
)
#let invoice_no = "2026-001"
#let issued     = "September 13, 2026"
#let due        = "October 13, 2026"
#let currency   = "$"
#let tax_rate   = 0.00            // e.g. 0.08 for 8% — set to 0 for none

// Line items: (description, quantity, unit price)
#let items = (
  ("Consulting — discovery and planning", 8,  150.00),
  ("Design and implementation",           20, 150.00),
  ("Revisions",                           4,  120.00),
)

// ---- Page + text configuration (edit to taste) -------------------------------
#set page(paper: "us-letter", margin: 1in)
#set text(font: "New Computer Modern", size: 10.5pt)

// ---- Money helper ------------------------------------------------------------
#let money(n) = {
  let cents = calc.round(n * 100)
  let dollars = calc.trunc(cents / 100)
  let rem = calc.rem(cents, 100)
  let frac = if rem < 10 { "0" + str(rem) } else { str(rem) }
  currency + str(dollars) + "." + frac
}
#let subtotal = items.map(it => it.at(1) * it.at(2)).sum()
#let tax      = subtotal * tax_rate
#let total    = subtotal + tax

// ---- Masthead ----------------------------------------------------------------
#grid(
  columns: (1fr, auto),
  align: (left, right),
  [
    #text(size: 16pt, weight: "bold")[#from.name] \
    #from.address \
    #from.city \
    #from.email
  ],
  [
    #text(size: 22pt, weight: "bold", fill: luma(90))[INVOICE] \
    #v(0.4em)
    *Invoice \##invoice_no* \
    Issued: #issued \
    Due: #due
  ],
)
#v(1.2em)

// ---- Bill-to -----------------------------------------------------------------
#text(weight: "bold")[Bill to] \
#bill_to.name \
#bill_to.company \
#bill_to.address \
#bill_to.city
#v(1em)

// ---- Line items --------------------------------------------------------------
#table(
  columns: (1fr, auto, auto, auto),
  align: (left, center, right, right),
  stroke: none,
  inset: (x: 8pt, y: 6pt),
  fill: (_, row) => if row == 0 { luma(230) },
  table.header[*Description*][*Qty*][*Unit price*][*Amount*],
  table.hline(stroke: 0.5pt),
  ..items
    .map(it => (it.at(0), str(it.at(1)), money(it.at(2)), money(it.at(1) * it.at(2))))
    .flatten(),
  table.hline(stroke: 0.5pt),
)
#v(0.4em)

// ---- Totals (right-aligned) --------------------------------------------------
#align(right)[
  #grid(
    columns: (auto, auto),
    column-gutter: 1.5em,
    row-gutter: 0.5em,
    align: (right, right),
    [Subtotal], [#money(subtotal)],
    ..if tax_rate > 0 {
      ([Tax (#calc.round(tax_rate * 100)%)], money(tax))
    } else { () },
    text(weight: "bold")[Total], text(weight: "bold")[#money(total)],
  )
]
#v(2em)

#text(size: 9pt, fill: luma(120))[
  Thank you for your business. Payment is due by #due. \
  Make payment to #from.name.
]
