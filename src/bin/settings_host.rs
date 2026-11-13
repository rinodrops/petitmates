//! Settings window with a live coat preview.
//!
//! Settings stays generic. This binary registers the preview callback and
//! then runs the settings window.

#[path = "../coat.rs"]
mod coat;

fn main() {
    settings::set_on_change(|change| {
        if change.callback != "pet_preview" {
            return None;
        }
        let hue = |key: &str| {
            change
                .values
                .iter()
                .find(|(k, _)| k == key)
                .and_then(|(_, v)| v.parse::<f64>().ok())
        };
        let (species, coat) = if change.record.contains("pond_turtle") {
            (
                "pond_turtle",
                coat::Coat::Turtle {
                    shell: hue("shell_hue").unwrap_or(coat::SHELL_HUE),
                    skin: hue("skin_hue").unwrap_or(coat::SKIN_HUE),
                },
            )
        } else {
            (
                "bearded_dragon",
                coat::Coat::Dragon {
                    hue: hue("hue").unwrap_or(coat::DRAGON_HUE),
                },
            )
        };
        let path = asset_png(species, "s-stand")?;
        let bytes = std::fs::read(path).ok()?;
        let tinted = coat::tint_png(&bytes, "s-stand", coat);
        let img = image::load_from_memory(&tinted).ok()?.to_rgba8();
        let (width, height) = img.dimensions();
        Some(settings::Preview {
            rgba: img.into_raw(),
            width: width as usize,
            height: height as usize,
        })
    });
    settings::run().expect("settings window failed");
}

fn asset_png(species: &str, sprite: &str) -> Option<std::path::PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(dir) = std::env::var("PETITMATES_ASSETS") {
        candidates.push(std::path::PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(macos) = exe.parent() {
            candidates.push(macos.join("../Resources/assets"));
            candidates.push(macos.join("assets"));
        }
    }
    candidates.push(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets"));
    candidates.into_iter().find_map(|root| {
        let path = root
            .join(species)
            .join("sprite")
            .join(format!("{sprite}.png"));
        path.is_file().then_some(path)
    })
}
