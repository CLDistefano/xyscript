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
`examples/` (runnable sample `.xpl`/`.kbd`/`.lst`), and `lang/` catalogs beside the binary. A drift
guard (`shipped_default_cfg_matches_config_script`) fails if `xyscript-default.cfg` no longer matches a
fresh instance.

## Windows

Nothing to do — `cargo build --release -p xyscript-gui` embeds `assets/xyscript.ico` into the
`.exe` (via `build.rs` + the `embed-resource` crate). Explorer, the taskbar, and Alt-Tab pick it up.

## Linux

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

`assets/xyscript.icns` is a multi-resolution icon (16–1024 px, PNG-backed). Build a `.app` bundle:

```sh
cargo install cargo-bundle
cargo bundle --release        # -> target/release/bundle/osx/XyScript.app (icns = Dock/Finder icon)
```

Before publishing, change `identifier` in `[package.metadata.bundle]` (currently the placeholder
`net.xyscript.editor`) to a reverse-DNS you control, and code-sign/notarize as needed.

If you'd rather not use `cargo-bundle`, drop `xyscript.icns` into any hand-assembled
`XyScript.app/Contents/Resources/` and reference it from `Info.plist` via `CFBundleIconFile`.
