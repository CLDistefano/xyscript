//! Headless highlight probe: load a grammar, open a file, enable highlighting, and print every
//! syntax token the renderer would draw — its kind, resolved fg/bg colour, and text — so grammar
//! behaviour can be checked without the GUI.
//!
//! Run: `cargo run -p xyscript-app --example dump_highlights -- <grammar.xpl> <file>`

use xyscript_app::editor::Editor;
use xyscript_kbd::embedded_default;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let grammar = args.first().map(String::as_str).unwrap_or("x:/xys/grammars/xywrite3.xpl");
    let file = args.get(1).map(String::as_str).unwrap_or("x:/rusted/xycolors.xy");

    let mut e = Editor::new(embedded_default());
    e.run_command(&format!("RUN {grammar}"));
    match e.open(file) {
        Ok(msg) => println!("open: {msg}"),
        Err(err) => {
            eprintln!("open {file}: {err}");
            std::process::exit(1);
        }
    }
    e.run_command("HIGHLIGHT on");

    println!("highlighter: {:?}", e.highlighter_name());
    let toks = e.highlights();
    println!("{} tokens\n", toks.len());
    for t in toks {
        let text = e.document().decoded_region(t.start, t.end).0;
        let fg = e.highlight_color(&t.kind);
        let bg = e.highlight_bg_color(&t.kind);
        println!(
            "{:>4}..{:<4} {:<12} fg={:<15} bg={:<15} {:?}",
            t.start,
            t.end,
            t.kind.name(),
            format!("{fg:?}"),
            format!("{bg:?}"),
            text,
        );
    }
}
