#set document(title: "XyScript — GitHub & Cross-Platform Build Guide")
#set page(paper: "us-letter", margin: (x: 1in, y: 1in), numbering: "1")
#set text(font: "New Computer Modern", size: 11pt)
#set par(justify: true, leading: 0.62em)
#show raw: set text(font: "DejaVu Sans Mono", size: 9.5pt)
#show heading: set block(above: 1.2em, below: 0.6em)
#set heading(numbering: none)

// A light callout box for notes, tips, and gotchas.
#let note(kind: "Note", body) = block(
  width: 100%,
  fill: rgb("#eef3fb"),
  stroke: 0.5pt + rgb("#3b6fb0"),
  radius: 4pt,
  inset: 9pt,
)[#strong[#kind.] #body]

#align(center)[
  #text(size: 20pt, weight: "bold")[XyScript — GitHub & Cross-Platform Build Guide]

  #v(0.2em)
  #text(size: 10pt, fill: rgb("#555"))[Personal reference · set up 16 July 2026]
]

#v(0.5em)

This is a private cheat-sheet for the XyScript repository and its cross-platform build pipeline.
Part 1 records the one-time setup we did (so it can be repeated or understood later). Parts 2 and 3
are the commands to use going forward — pushing changes, building binaries in CI, cutting releases,
and building locally on Windows.

#table(
  columns: (auto, 1fr),
  stroke: none,
  inset: 4pt,
  [*Repository*], [`github.com/CLDistefano/xyscript` (private)],
  [*Local path*], [`C:\Users\cldis\rusted`],
  [*Default branch*], [`main`],
  [*Shipped Windows binary*], [`dist\xyscript.exe`],
  [*CI workflow file*], [`.github\workflows\release.yml`],
)

#note(kind: "Terminal")[All commands below are for PowerShell in the project folder
(`cd C:\Users\cldis\rusted`). `git` and `gh` (the GitHub CLI) must be on the PATH.]

= Part 1 — One-time setup (already done)

== 1. The `.gitignore`

Keeps the huge, regenerable build trees and the shipped binary out of git. Lives at the repo root:

```gitignore
target/        # Cargo build output (every workspace) — large, rebuilt from source
dist/          # the shipped Windows binary — CI rebuilds it; ~78 MB
*.pdb          # debug symbols
Thumbs.db
.DS_Store
```

== 2. Create the local repository

```powershell
cd C:\Users\cldis\rusted

# One-time identity (only if never set on this machine):
git config --global user.name  "Carl L. Distefano"
git config --global user.email "cldistefano@att.net"

git init -b main          # initialize with 'main' as the default branch
git add -A                # stage everything (the .gitignore excludes target/ and dist/)
git status                # confirm files appear under "Changes to be committed:"
git commit -m "Initial commit: XyScript editor + cross-platform CI workflow"
```

#note(kind: "Gotcha")[If `git commit` prints *"Author identity unknown"* it means the
`git config` identity lines above were skipped — set them, then commit again. And always confirm
`git status` shows a *"Changes to be committed"* section (staged) before committing, not just
*"Untracked files"*.]

== 3. Create the GitHub repository and push

```powershell
gh auth login
# Answer: GitHub.com  ->  HTTPS  ->  "Authenticate Git with your GitHub credentials?" Yes
#         ->  Login with a web browser  ->  copy the one-time code into the browser page.

# Create the private repo, wire up 'origin', and push 'main' in one step:
gh repo create xyscript --private --source=. --remote=origin --push

gh repo view --web        # open it in the browser to confirm
```

#note(kind: "Gotcha")[`gh repo create --push` needs at least one commit to already exist — if the
commit in step 2 didn't succeed, the repo gets created but the push fails with *"no commits found."*
Fix the commit, then push manually with `git push -u origin main`.]

== 4. The CI workflow (what `release.yml` does)

Builds native binaries for all three platforms on GitHub's runners (no cross-compiling in CI):

#table(
  columns: (auto, auto, 1fr),
  inset: 5pt,
  align: left,
  stroke: 0.5pt + rgb("#ccc"),
  table.header([*Job*], [*Runner*], [*Produces*]),
  [`linux-x86_64`], [`ubuntu-latest`], [`xyscript-linux-x86_64`],
  [`windows-x86_64`], [`windows-latest`], [`xyscript-windows-x86_64.exe`],
  [`macos-universal`], [`macos-14` (Apple Silicon)],
    [`xyscript-macos-universal` — Intel + Apple Silicon in one binary, built by compiling both
     arches and merging them with `lipo`],
)

- *Triggers:* a manual run (`workflow_dispatch`) or pushing a version tag (`v*`).
- *On a tag*, a fourth job attaches all three binaries to a GitHub *Release*.
- Builds from inside `xyscript-gui/` with `--features lint,email` (the shipped feature set), so the
  crate's own `.cargo\config.toml` applies (static MSVC C-runtime on Windows).

#note(kind: "Why no Intel-Mac runner")[GitHub's Intel `macos-13` runners are being retired and a job
targeting one can sit queued indefinitely. Building a *universal* binary on the Apple-Silicon runner
avoids depending on an Intel runner entirely.]

= Part 2 — Everyday GitHub commands (future use)

== Push code changes to GitHub

The normal edit → commit → push cycle:

```powershell
git add -A
git commit -m "Describe what changed"
git push
```

#note[Pushing to `main` does *not* automatically start a build — the workflow only runs on a manual
trigger or a version tag (see below). Push freely; build when you actually want binaries.]

== Build binaries in CI (without making a release)

```powershell
gh workflow run "Build binaries"     # start a manual run
gh run watch                         # follow it live
gh run list -L 5                     # recent runs + their status
gh run view --log-failed             # if a job goes red, show only the failing logs
```

Download the binaries from a finished run:

```powershell
gh run download                      # pulls the latest run's artifacts into the current folder
```

== Cut a versioned release (build + attach binaries)

Tagging is what publishes a Release with the three binaries attached:

```powershell
git tag v0.1.0                       # use the next version number
git push origin v0.1.0               # triggers the build + release job
```

Then check / manage it:

```powershell
gh release view v0.1.0 --web         # view the release in the browser
gh release list                      # list all releases
```

#note(kind: "Undo a release")[A release is easy to reverse if needed:
`gh release delete v0.1.0` then delete the tag with `git push origin :refs/tags/v0.1.0` and
`git tag -d v0.1.0`.]

#note(kind: "macOS signing")[The macOS binary is *unsigned*. On a Mac, first launch needs
right-click → Open to get past Gatekeeper. Proper signing/notarization is a separate, paid
Apple-developer step.]

= Part 3 — Local builds on your Windows machine

== Build and deploy the Windows binary (`dist\xyscript.exe`)

The same build CI runs, but locally — this is how you refresh the binary you actually use:

```powershell
cargo build --release --features lint,email --manifest-path xyscript-gui\Cargo.toml
copy xyscript-gui\target\release\xyscript-gui.exe dist\xyscript.exe
```

#note[Always build the GUI with `--manifest-path xyscript-gui\Cargo.toml` (it is its own detached
workspace) and with `--features lint,email` (language checker + email verbs — the full edition).
Drop `email` for a mail-free build; drop both for the lean tester build.]

== Cross-build the Linux binary locally (optional)

We verified the whole app cross-compiles from Windows using `zig` as the linker. One-time tooling:

```powershell
rustup target add x86_64-unknown-linux-gnu
cargo install cargo-zigbuild        # requires zig on the PATH
```

Build for Linux:

```powershell
# Engine only (fast portability check):
cargo zigbuild --target x86_64-unknown-linux-gnu -p xyscript-app --features lint,email

# Whole GUI app:
cd xyscript-gui
cargo zigbuild --target x86_64-unknown-linux-gnu --features lint,email
cd ..
```

#note(kind: "macOS")[macOS binaries *cannot* be cross-compiled from Windows (Apple's SDK/frameworks
aren't available there). Build those in CI — that's what the `macos-universal` job is for.]

== Reclaim disk space

The build trees get very large (the local Linux cross-build tree was ~79 GB). Clear a specific
target, or everything:

```powershell
cargo clean --target x86_64-unknown-linux-gnu    # clears the whole target\ tree in this workspace
cargo clean --manifest-path xyscript-gui\Cargo.toml --target x86_64-unknown-linux-gnu
```

#note[After a `cargo clean`, the next build recompiles the dependency tree from scratch (a couple of
minutes). Source code and `dist\xyscript.exe` are never touched by `cargo clean`.]

= Quick reference

#table(
  columns: (auto, 1fr),
  inset: 5pt,
  stroke: 0.5pt + rgb("#ccc"),
  table.header([*Task*], [*Command*]),
  [Push changes], [`git add -A; git commit -m "..."; git push`],
  [Build in CI (no release)], [`gh workflow run "Build binaries"`],
  [Watch a run], [`gh run watch`],
  [Download binaries], [`gh run download`],
  [Release + binaries], [`git tag vX.Y.Z; git push origin vX.Y.Z`],
  [Local Windows build], [`cargo build --release --features lint,email --manifest-path xyscript-gui\Cargo.toml`],
  [Deploy local build], [`copy xyscript-gui\target\release\xyscript-gui.exe dist\xyscript.exe`],
  [Local Linux build], [`cd xyscript-gui; cargo zigbuild --target x86_64-unknown-linux-gnu --features lint,email`],
  [Free disk space], [`cargo clean --target x86_64-unknown-linux-gnu`],
)
