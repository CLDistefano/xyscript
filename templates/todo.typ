// ─────────────────────────────────────────────────────────────────────────────
// todo.typ — a starter Typst to-do list for XyScript.
//
// Open it, edit the items below, then TOPDF to export a PDF. A .typ file renders
// as Typst automatically (DOCTYPE auto). Set `done: true` on an item to tick it.
// Highlight with:
//     RUN grammars/typst.xpl
//     HIGHLIGHT on
//
// (For a Markdown to-do you can tick in the preview, see todo.md — GFM task lists
// use `- [ ]` / `- [x]`.)
// ─────────────────────────────────────────────────────────────────────────────

// ---- Fill these in -----------------------------------------------------------
#let title = "To-Do"
#let date  = "September 13, 2026"

// Each section: a heading and its items. Per item: (text, done).
#let sections = (
  ("Today", (
    ("Reply to outstanding email", false),
    ("Draft the project update", false),
    ("Review the invoice template", true),
  )),
  ("This week", (
    ("Schedule the planning meeting", false),
    ("Back up the working files", false),
  )),
  ("Someday", (
    ("Learn the Typst math syntax", false),
  )),
)

// ---- Page + text configuration (edit to taste) -------------------------------
#set page(paper: "us-letter", margin: 1in)
#set text(font: "New Computer Modern", size: 11pt)
#set par(leading: 0.65em)

// ---- Title -------------------------------------------------------------------
#text(size: 20pt, weight: "bold")[#title] #h(1fr) #text(fill: luma(120))[#date]
#v(0.4em)
#line(length: 100%, stroke: 0.5pt)
#v(0.8em)

// ---- Checklist ---------------------------------------------------------------
#let item(entry) = {
  let (label, done) = entry
  let box = if done { "☑" } else { "☐" }
  let body = if done { strike[#label] } else { label }
  block(spacing: 0.6em)[#box #h(0.4em) #body]
}

#for (name, entries) in sections [
  == #name
  #for e in entries [#item(e)]
  #v(0.4em)
]
