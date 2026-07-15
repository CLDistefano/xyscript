//! G3 PDF-backend spike — typeset a representative GFM-ish document to PDF with
//! each candidate backend, reporting output size and (by construction) the
//! integration effort. See `gfm-track-plan.md` decision G3.

mod genpdf_spike;
mod typst_spike;

fn main() {
    println!("== G3 PDF-backend spike ==\n");

    let (gp, gs) = genpdf_spike::run("out_genpdf.pdf");
    println!("genpdf : wrote {gp} ({gs} bytes)");

    let (tp, ts) = typst_spike::run("out_typst.pdf");
    println!("typst  : wrote {tp} ({ts} bytes)");

    // 8b validation: compile the GFM→typst output if gfm_dump wrote it.
    if std::path::Path::new("from_gfm.typ").exists() {
        let (fp, fs) = typst_spike::run_file("from_gfm.typ", "out_from_gfm.pdf");
        println!("gfm→typst: compiled from_gfm.typ → {fp} ({fs} bytes)");
    }

    // Feature demo: render demo.typ with typst's bundled fonts (math/default font).
    if std::path::Path::new("demo.typ").exists() {
        let (dp, ds) = typst_spike::run_demo("demo.typ", "out_demo.pdf");
        println!("demo   : compiled demo.typ → {dp} ({ds} bytes)");
    }

    println!("\nOpen the PDF(s) to judge fidelity. Integration notes are in the source.");
}
