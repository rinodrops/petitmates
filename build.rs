fn main() {
    // Embed Windows resources only when targeting Windows.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let base = std::path::Path::new(&manifest_dir).join("assets");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", base.join("trayicon.ico").display());
    println!("cargo:rerun-if-changed={}", base.join("trayicon-white.ico").display());

    // Copy .ico files to /tmp to avoid issues with spaces in paths (e.g.
    // "Petit Mates" in the workspace path).
    std::fs::copy(base.join("trayicon.ico"),       "/tmp/pm_trayicon.ico").unwrap();
    std::fs::copy(base.join("trayicon-white.ico"), "/tmp/pm_trayicon_white.ico").unwrap();

    // Resource IDs:
    //   1 = window-class / Explorer icon (reuse tray icon as placeholder)
    //   2 = tray icon dark silhouette (shown on light taskbar)
    //   3 = tray icon white silhouette (shown on dark taskbar)
    let rc_src = "#pragma code_page(65001)\n\
                  1 ICON \"/tmp/pm_trayicon.ico\"\n\
                  2 ICON \"/tmp/pm_trayicon.ico\"\n\
                  3 ICON \"/tmp/pm_trayicon_white.ico\"\n";
    std::fs::write("/tmp/pm_resource.rc", rc_src).unwrap();

    let windres = if cfg!(target_os = "windows") { "windres" } else { "x86_64-w64-mingw32-windres" };
    let status  = std::process::Command::new(windres)
        .args(["-i", "/tmp/pm_resource.rc", "-o", "/tmp/pm_resource.o", "--output-format=coff"])
        .status()
        .expect("windres not found — install mingw-w64 (brew install mingw-w64)");
    assert!(status.success(), "windres failed");

    println!("cargo:rustc-link-arg=/tmp/pm_resource.o");
}
