# Packaging assets

- `icon.png` — 1024×1024 app icon source. `cargo-packager` derives the macOS
  `.icns` and Windows `.ico` from it (see the `[package.metadata.packager]`
  config in `imgfind-launcher/Cargo.toml`). Replace this file with a branded
  icon (same dimensions) to rebrand; no code change needed.
- `imgfind-launcher.desktop` — Linux XDG desktop entry installed by `install.sh`.
