//! 8b validation helper: run the GFM→typst transform on a deliberately tricky
//! document and write a complete typst source (preamble + body) to the spike dir,
//! so `pdf-spike` can compile it and we can confirm the markup is well-formed
//! (escaping in particular). Throwaway — the real compile-test lives in 8c.

use xyscript_app::to_typst;

const SAMPLE: &str = r###"# XyScript GFM → typst (8b)

A paragraph with **bold**, *italic*, ~~strike~~, `inline code`, and a [link](https://example.com).

Tricky characters that must be escaped or typst would choke: a # hash, a $ dollar,
an * asterisk, an _underscore_, <angle> brackets, a ~tilde~, and an @at sign.

## A bullet list
- First item
- Second item with `code` and **bold**
- Third

## An ordered list
1. one
2. two
3. three

## A quote
> To be, or not to be — that is the question.

## A table
| Name | Role     |
|------|----------|
| Ada  | Pioneer  |
| Alan | Theorist |

## A code block
```rust
fn main() {
    let x = "#not code"; // a hash and $dollar inside a string
    println!("{}", x);
}
```

---
Done.
"###;

fn main() {
    let body = to_typst(SAMPLE);
    let preamble = "#set page(margin: 18mm)\n#set text(font: \"Arial\", size: 11pt)\n\
                    #show raw: set text(font: \"Consolas\")\n\n";
    let full = format!("{preamble}{body}\n");
    let path = r"C:\Users\cldis\rusted\pdf-spike\from_gfm.typ";
    std::fs::write(path, &full).unwrap();
    println!("wrote {path} ({} body bytes)\n", body.len());
    println!("----- typst body -----\n{body}");
}
