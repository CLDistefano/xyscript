# XyScript — developer task runner (GNU Make).
#
# On Windows, run these from Git Bash, where `make` and the `cargo` toolchain both
# work. All targets are phony; they always run.

.PHONY: help docs
.DEFAULT_GOAL := help

help:
	@echo "Targets:"
	@echo "  make docs   Regenerate docs/reference.pdf and docs/manual.pdf from their .typ sources"

# Regenerate BOTH authored-Typst PDFs (docs/reference.pdf, docs/manual.pdf) through
# the app's own typst_to_pdf engine — the same fonts and #image/#read resolution the
# GUI TOPDF/preview use, so the output is deterministic and matches an in-app export.
#
# Run this in the SAME commit as any docs/reference.typ or docs/manual.typ edit:
# release.yml bundles the COMMITTED PDFs verbatim (it does not regenerate them), and
# no test guards a PDF against its .typ source — so a skipped regen ships a stale doc.
docs:
	cargo run -p xyscript-app --example build_docs_pdf
