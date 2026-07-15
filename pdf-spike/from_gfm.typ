#set page(margin: 18mm)
#set text(font: "Arial", size: 11pt)
#show raw: set text(font: "Consolas")

= XyScript GFM → typst (8b)

A paragraph with #strong[bold], #emph[italic], #strike[strike], #raw("inline code"), and a #link("https://example.com")[link].

Tricky characters that must be escaped or typst would choke: a \# hash, a \$ dollar, an \* asterisk, an #emph[underscore], \<angle\> brackets, a #strike[tilde], and an \@at sign.

== A bullet list

#list([First item], [Second item with #raw("code") and #strong[bold]], [Third], )

== An ordered list

#enum([one], [two], [three], )

== A quote

#quote(block: true)[To be, or not to be — that is the question.]

== A table

#table(columns: 2, [#strong[Name]], [#strong[Role]], [Ada], [Pioneer], [Alan], [Theorist], )

== A code block

#raw("fn main() {\n    let x = \"#not code\"; // a hash and $dollar inside a string\n    println!(\"{}\", x);\n}\n", block: true, lang: "rust")

#line(length: 100%)

Done.
