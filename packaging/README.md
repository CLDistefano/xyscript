# Packaging & icons

The XyScript icon is delivered three different ways because the three OSes handle app icons
differently. The source art is `../xyscript.ico` / `../icon-final-1024.png`; regenerate the
platform assets with `scratchpad/make_platform_icons.ps1` if the art changes.

| Platform | Executable / bundle icon | Window (titlebar/taskbar) icon |
|----------|--------------------------|--------------------------------|
| Windows  | Embedded `.ico` resource (build.rs) | winit `with_window_icon` **+** the embedded resource |
| Linux/X11 | — (use the `.desktop` launcher) | winit `with_window_icon` (`_NET_WM_ICON`) |
| Linux/Wayland | — | `.desktop` + hicolor PNG, matched by app-id `xyscript` |
| macOS    | `.icns` inside `XyScript.app` | n/a (macOS has no per-window icon) |

## Runtime kit (`runtime/`)

`runtime/` holds the files that ship **beside the binary** in every platform's release bundle (the
`release.yml` package steps copy `runtime/*` and `themes/` into the folder next to the executable).
Because a GUI boot sets `$CWD` to the program's own folder (`$XSDIR`), the working `startup.xpl` loads
this kit with plain relative paths, so an unzipped release runs out of the box:

| File | Purpose |
|------|---------|
| `startup.xpl` | Working startup script; loads the keymap, config, a theme, auto-replace pairs, and the library. |
| `startup.xpl.template` | Documented, all-commented starting point to copy into your own `startup.xpl`. |
| `xyscript-internal.kbd` | The default keyboard map, as a file (`loadkbd`). |
| `xyscript-default.cfg` | Every setting at its default value — **regenerate with `SAVECFG xyscript-default.cfg`** from a fresh instance. |
| `xyscript.ar` | Default auto-replace pairs (accented characters, symbols). |
| `xyscript.lib` | The bundled script library (`LISTLIB` to view routines). |
| `user-variables.template` | Blank template of the variables the library uses; the user copies it to `user-variables.xpl`. |

Keep everything here ASCII (`SAVECFG` now emits ASCII comments) so the files edit cleanly in any editor.
The release bundles also carry the repo's `themes/`, `grammars/` (syntax-highlight grammars you `RUN`),
`examples/` (runnable sample `.xpl`/`.kbd`/`.lst`), `lang/` catalogs, and a `docs/` subfolder
(`manual.pdf`, `reference.pdf`, the `keyboard.png` chart, and `email.md`/`regex.md`/`run-commands.md`)
beside the binary. The library's `kbd`, `man`, and `ref` routines open `docs/keyboard.png`,
`docs/manual.pdf`, and `docs/reference.pdf`. A drift guard (`shipped_default_cfg_matches_config_script`)
fails if `xyscript-default.cfg` no longer matches a fresh instance.

## Windows

Nothing to do — `cargo build --release -p xyscript-gui` embeds `assets/xyscript.ico` into the
`.exe` (via `build.rs` + the `embed-resource` crate). Explorer, the taskbar, and Alt-Tab pick it up.

## Linux

### Runtime dependencies

The Linux binary is dynamically linked against system libraries (winit for X11/Wayland + keyboard,
wgpu for GL/Vulkan, rfd/GTK for file dialogs, fontconfig for text). A full desktop usually has them,
but a **minimal or fresh install** (e.g. a new VM) often does not — the one most commonly missing is
`libxkbcommon-x11-0` (winit's X11 keyboard integration), whose absence stops the app from starting.
These are the runtime `.so` packages (the counterparts of the `-dev` packages the CI build installs);
on Debian/Ubuntu:

```sh
sudo apt install \
  libxkbcommon0 libxkbcommon-x11-0 \
  libx11-6 libxcb1 libxcb-render0 libxcb-shape0 libxcb-xfixes0 \
  libxrandr2 libxi6 libxcursor1 \
  libwayland-client0 libwayland-cursor0 libwayland-egl1 \
  libgtk-3-0 libgl1 libfontconfig1
```

If XyScript fails to start with a missing-library error, install the named library (most often
`sudo apt install libxkbcommon-x11-0`). A `.deb` built with `cargo bundle` declares these as
dependencies, so `apt`/`dpkg` pulls them in automatically.

### Launcher & icons

The app sets its X11 `WM_CLASS` and Wayland app-id to `xyscript` at runtime, matching
`linux/xyscript.desktop` (`StartupWMClass=xyscript`). To install the launcher + icons per-user:

```sh
cargo build --release -p xyscript-gui        # produces target/release/xyscript-gui
cd packaging/linux
./install.sh                                  # finds the release binary automatically
```

This drops the binary at `~/.local/bin/xyscript`, the launcher in
`~/.local/share/applications/`, and PNGs into `~/.local/share/icons/hicolor/*/apps/xyscript.png`.
On X11 the window already carries the icon at runtime; on Wayland the compositor reads it from the
installed `.desktop` + hicolor PNGs.

To build a `.deb` instead:

```sh
cargo install cargo-bundle
cargo bundle --release                        # reads [package.metadata.bundle] in Cargo.toml
```

## macOS

### First run (Gatekeeper)

The released macOS binary is **unsigned and un-notarized**, so Gatekeeper blocks it on first launch
("cannot be opened because it is from an unidentified developer", or "is damaged" for a quarantined
download). To run it anyway, either right-click the binary (or `XyScript.app`) in Finder and choose
**Open** (then confirm), or clear the quarantine flag from a terminal:

```sh
xattr -dr com.apple.quarantine /path/to/xyscript        # or .../XyScript.app
```

Signing + notarization (a paid Apple Developer step) would remove this prompt; see the notes below.

### Icon & `.app` bundle

`assets/xyscript.icns` is a multi-resolution icon (16–1024 px, PNG-backed). Build a `.app` bundle:

```sh
cargo install cargo-bundle
cargo bundle --release        # -> target/release/bundle/osx/XyScript.app (icns = Dock/Finder icon)
```

Before publishing, change `identifier` in `[package.metadata.bundle]` (currently the placeholder
`net.xyscript.editor`) to a reverse-DNS you control, and code-sign/notarize as needed.

If you'd rather not use `cargo-bundle`, drop `xyscript.icns` into any hand-assembled
`XyScript.app/Contents/Resources/` and reference it from `Info.plist` via `CFBundleIconFile`.
