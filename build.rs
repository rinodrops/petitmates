fn main() {
    // Embed Windows resources only when targeting Windows.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let base = std::path::Path::new(&manifest_dir).join("assets");

    let chars = ["bearded_dragon", "pond_turtle"];

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", base.join("appicon.ico").display());

    for char_name in &chars {
        let char_base  = base.join(char_name);
        let sprite_dir = char_base.join("sprite");
        println!("cargo:rerun-if-changed={}", char_base.join("manifest.toml").display());
        for entry in std::fs::read_dir(&sprite_dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().map_or(false, |e| e == "png") {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }

    // ----------------------------------------------------------------
    // Generate OUT_DIR/embedded_assets.rs
    // Contains compile-time include_bytes! references for every sprite
    // and the manifest.toml for each character, so the Windows exe is
    // fully self-contained.
    // ----------------------------------------------------------------
    let out_dir  = std::env::var("OUT_DIR").unwrap();
    let out_path = std::path::Path::new(&out_dir).join("embedded_assets.rs");

    let mut code = String::new();

    for char_name in &chars {
        let char_base  = base.join(char_name);
        let sprite_dir = char_base.join("sprite");

        code.push_str(&format!("pub mod {char_name} {{\n"));

        // manifest.toml
        code.push_str(&format!(
            "    pub const MANIFEST_TOML: &[u8] = include_bytes!({:?});\n",
            char_base.join("manifest.toml"),
        ));

        // All sprite PNGs, sorted for deterministic output.
        let mut sprites: Vec<(String, std::path::PathBuf)> = std::fs::read_dir(&sprite_dir)
            .unwrap()
            .filter_map(|e| {
                let path = e.ok()?.path();
                if path.extension()? == "png" {
                    let name = path.file_stem()?.to_string_lossy().into_owned();
                    Some((name, path))
                } else {
                    None
                }
            })
            .collect();
        sprites.sort_by(|a, b| a.0.cmp(&b.0));

        code.push_str("    pub const SPRITES: &[(&str, &[u8])] = &[\n");
        for (name, path) in &sprites {
            code.push_str(&format!("        ({:?}, include_bytes!({:?})),\n", name, path));
        }
        code.push_str("    ];\n");

        code.push_str("}\n\n");
    }

    std::fs::write(out_path, code).unwrap();

    // ----------------------------------------------------------------
    // Windows resources (.ico files embedded via windres)
    // appicon.ico already contains 16 – 256 px layers, so it works as
    // the exe/Explorer icon AND as the system-tray icon at all sizes.
    // ----------------------------------------------------------------
    std::fs::copy(base.join("appicon.ico"), "/tmp/pm_appicon.ico").unwrap();

    // All three resource IDs point to the same appicon so that the
    // application icon is consistent at every size.
    //   1 = window-class / Explorer icon
    //   2 = tray icon (taskbar dark mode – white/light icon)
    //   3 = tray icon (taskbar light mode – coloured icon)
    let rc_src = "#pragma code_page(65001)\n\
                  1 ICON \"/tmp/pm_appicon.ico\"\n\
                  2 ICON \"/tmp/pm_appicon.ico\"\n\
                  3 ICON \"/tmp/pm_appicon.ico\"\n";
    std::fs::write("/tmp/pm_resource.rc", rc_src).unwrap();

    let windres = if cfg!(target_os = "windows") { "windres" } else { "x86_64-w64-mingw32-windres" };
    let status  = std::process::Command::new(windres)
        .args(["-i", "/tmp/pm_resource.rc", "-o", "/tmp/pm_resource.o", "--output-format=coff"])
        .status()
        .expect("windres not found — install mingw-w64 (brew install mingw-w64)");
    assert!(status.success(), "windres failed");

    println!("cargo:rustc-link-arg=/tmp/pm_resource.o");
}
