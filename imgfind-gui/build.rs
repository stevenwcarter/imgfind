fn main() {
    slint_build::compile("ui/app.slint").expect("Slint build failed");
    #[cfg(windows)]
    embed_windows_icon();
}

// Embed the app icon into the .exe's resources so the GUI window's taskbar entry
// and the binary in Explorer show it. cargo-packager bundles the prebuilt binary
// as-is and cannot add this — it must happen at build time. Gated on the HOST
// being Windows (Windows artifacts are built natively in CI), matching the
// host-gated `winresource` build-dependency.
#[cfg(windows)]
fn embed_windows_icon() {
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../packaging/icon.ico");
    res.compile()
        .expect("failed to embed Windows icon resource");
}
