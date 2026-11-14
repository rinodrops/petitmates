use std::path::Path;

#[path = "src/coat.rs"]
#[allow(dead_code)]
mod coat;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let base = Path::new(&manifest_dir).join("assets");
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let out = Path::new(&out_dir);

    generate_coat_masks(&base, out);
    // Windows also embeds the sprites and executable icon.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let chars = ["bearded_dragon", "pond_turtle", "leopard_gecko"];

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
    let out_path = out.join("embedded_assets.rs");

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
    //
    // Temp artifacts are written under OUT_DIR (a per-build, per-crate
    // private directory) rather than predictable, world-writable paths in
    // /tmp.  Fixed /tmp names are vulnerable to symlink/race attacks on
    // shared hosts and break concurrent builds.  Process args are passed
    // directly (no shell), so spaces in OUT_DIR are handled correctly.
    // ----------------------------------------------------------------
    let ico_path = out.join("pm_appicon.ico");
    let rc_path  = out.join("pm_resource.rc");
    let o_path   = out.join("pm_resource.o");

    std::fs::copy(base.join("appicon.ico"), &ico_path).unwrap();

    // All three resource IDs point to the same appicon so that the
    // application icon is consistent at every size.
    //   1 = window-class / Explorer icon
    //   2 = tray icon (taskbar dark mode – white/light icon)
    //   3 = tray icon (taskbar light mode – coloured icon)
    let rc_src = format!(
        "#pragma code_page(65001)\n\
         1 ICON {0:?}\n\
         2 ICON {0:?}\n\
         3 ICON {0:?}\n",
        ico_path.display().to_string(),
    );
    std::fs::write(&rc_path, rc_src).unwrap();

    let windres = if cfg!(target_os = "windows") { "windres" } else { "x86_64-w64-mingw32-windres" };
    let status  = std::process::Command::new(windres)
        .arg("-i").arg(&rc_path)
        .arg("-o").arg(&o_path)
        .arg("--output-format=coff")
        .status()
        .expect("windres not found — install mingw-w64 (brew install mingw-w64)");
    assert!(status.success(), "windres failed");

    println!("cargo:rustc-link-arg={}", o_path.display());
}

/// Store runs of up to 64 pixels in one byte (two region bits and six length bits).
fn generate_coat_masks(assets: &Path, out: &Path) {
    println!("cargo:rerun-if-changed=src/coat.rs");
    println!("cargo:rerun-if-changed={}", assets.join("pond_turtle/mask").display());

    let mut table = String::from("&[\n");
    let mut masks = Vec::new();
    for (species, turtle) in [("bearded_dragon", false), ("pond_turtle", true)] {
        let sprite_dir = assets.join(species).join("sprite");
        println!("cargo:rerun-if-changed={}", sprite_dir.display());

        let mut paths: Vec<_> = std::fs::read_dir(&sprite_dir)
            .unwrap_or_else(|err| panic!("Could not list {}: {err}", sprite_dir.display()))
            .map(|entry| entry.expect("Could not read sprite directory entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "png"))
            .collect();
        paths.sort_by(|a, b| a.file_stem().cmp(&b.file_stem()));
        for path in paths {
            let name = path.file_stem().and_then(|name| name.to_str()).expect("Invalid sprite name");
            let png = std::fs::read(&path)
                .unwrap_or_else(|err| panic!("Could not read {}: {err}", path.display()));
            let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
                .unwrap_or_else(|err| panic!("Could not decode {}: {err}", path.display()))
                .into_rgba8();
            let start = masks.len();
            let regions = coat::tint_regions(&image, name, turtle);
            for (region, len) in coat::region_runs(&regions) {
                let full = len / 64;
                masks.extend(std::iter::repeat_n(((region as u8) << 6) | 63, full));
                let remainder = len % 64;
                if remainder != 0 {
                    masks.push(((region as u8) << 6) | (remainder - 1) as u8);
                }
            }
            let (width, height) = image.dimensions();
            let key = (turtle, name);
            let end = masks.len();
            let size = png.len();
            let crc = crc32fast::hash(&png);
            use std::fmt::Write;
            writeln!(
                table,
                "    ({key:?}, Mask {{ dimensions: ({width}, {height}), png_len: {size}, crc: {crc}, start: {start}, end: {end} }}),"
            ).unwrap();
        }
    }
    table.push_str("]\n");
    std::fs::write(out.join("coat_masks.rs"), table).expect("Could not write coat mask table");
    std::fs::write(out.join("coat_masks.bin"), masks).expect("Could not write coat mask data");
}
