// ─────────────────────────────────────────────────────────────────────────────
// business-letter.typ — a starter Typst business letter for XyScript.
//
// Open it, fill in the fields below, then TOPDF to export a PDF. A .typ file
// renders as Typst automatically (DOCTYPE auto); the document's own #set rules
// control the page, so what you write is what you get. Syntax-highlight it with:
//     RUN grammars/typst.xpl
//     HIGHLIGHT on
//
// Layout: modified block — the letterhead and date are centred, the body is
// left-aligned, and the closing sits just right of centre.
// ─────────────────────────────────────────────────────────────────────────────

// ---- Fill these in -----------------------------------------------------------
#let sender = (
  name:    "Your Name",
  title:   "Your Title",
  company: "Company Name",
  address: "123 Main Street, Suite 100",
  city:    "City, ST 00000",
  phone:   "(555) 555-0100",
  email:   "you@example.com",
)
#let date      = "September 13, 2026"
#let recipient = (
  name:    "Recipient Name",
  title:   "Their Title",
  company: "Their Company",
  address: "456 Market Avenue",
  city:    "City, ST 00000",
)
#let salutation = "Dear " + recipient.name + ":"
#let closing    = "Sincerely,"

// ---- Page + text configuration (edit to taste) -------------------------------
#set page(paper: "us-letter", margin: 1in)
#set text(font: "New Computer Modern", size: 11pt)
#set par(justify: false, leading: 0.65em, spacing: 1.1em)

// ---- Letterhead (centred, no rule) -------------------------------------------
#align(center)[
  #text(size: 15pt, weight: "bold")[#sender.company] \
  #sender.address \
  #sender.city \
  #sender.phone #h(1em) · #h(1em) #sender.email
]
#v(1.4em)

// ---- Date (centred) ----------------------------------------------------------
#align(center)[#date]
#v(1.4em)

// ---- Inside address ----------------------------------------------------------
#recipient.name \
#recipient.title \
#recipient.company \
#recipient.address \
#recipient.city
#v(1.2em)

// ---- Salutation --------------------------------------------------------------
#salutation
#v(0.6em)

// ---- Body — replace with your letter -----------------------------------------
Thank you for taking the time to consider this letter. Replace this paragraph
with your opening: state your purpose in the first sentence so the reader knows
at once why you are writing.

Use the middle paragraphs for the details — background, the request or proposal,
and anything the reader needs in order to act. Keep each paragraph to a single
idea, and *emphasise* only what matters.

Close by making the next step clear: what you would like the reader to do, and how
to reach you. Thank them again for their time.

// ---- Closing (just right of centre) + signature ------------------------------
#v(1.2em)
#pad(left: 50%)[
  #closing
  #v(3em) // space for a handwritten signature
  #sender.name \
  #sender.title
]
