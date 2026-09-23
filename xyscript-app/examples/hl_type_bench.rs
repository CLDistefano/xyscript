//! Simulate typing on a big dense «MD» doc and measure per-keystroke highlight cost through the real
//! edit path (record -> note_edit incremental), plus a scroll.
use std::time::Instant;
use xyscript_app::editor::Editor;
use xyscript_kbd::embedded_default;

fn main() {
    let mut e = Editor::new(embedded_default());
    e.run_command("RUN x:/xys/grammars/xywrite4.xpl");
    let unit = "\u{00AB}MDBO\u{00BB}Lemma\u{00AB}MDNM\u{00BB} \u{00AB}MDIT\u{00BB}n.\u{00AB}MDNM\u{00BB} a definition; ";
    let mut s = String::new();
    while s.len() < 2_000_000 {
        s.push_str(unit);
        s.push('\n');
    }
    e.run_command("NEW bench.xy");
    e.insert_str(&s);
    e.run_command("HIGHLIGHT on");
    let end = e.document().decoded_len();
    let vlo = end.saturating_sub(3000);
    e.ensure_highlights(vlo, end); // initial window at the caret (end of file)
    println!("doc {} KB, {} tokens in window", end / 1024, e.highlights().len());

    // Type 300 chars at the caret (end, inside the covered window) — the hot path.
    let n = 300;
    let t = Instant::now();
    for _ in 0..n {
        e.insert_str("x");
        let end = e.document().decoded_len();
        e.ensure_highlights(end.saturating_sub(3000), end);
    }
    println!("per keystroke: {:.4} ms", t.elapsed().as_secs_f64() * 1000.0 / n as f64);

    // Scroll: jump the window by ~one screen (2 KB) repeatedly; measures extend cost.
    let n = 200;
    let t = Instant::now();
    for i in 0..n {
        let lo = (i * 2000) % (end - 3000);
        e.ensure_highlights(lo, lo + 3000);
    }
    println!("per scroll-step: {:.4} ms", t.elapsed().as_secs_f64() * 1000.0 / n as f64);
}
