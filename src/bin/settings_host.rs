//! Settings window with live previews of the selected individual.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[path = "../coat.rs"]
mod coat;
#[path = "../coat_palette.rs"]
mod coat_palette;

use std::collections::HashMap;

use coat::TintPreview;

fn main() {
    settings::set_legacy_color_resolver(coat::legacy_editor_color);
    let mut previews = HashMap::new();
    settings::set_on_change(move |change| {
        if change.callback != "pet_preview" {
            return None;
        }
        let value = |key: &str| {
            change
                .values
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
        };
        let hue = |key| value(key).and_then(|value| value.parse().ok());
        let (species, bytes, coat) = if change.record.starts_with("pets.pond_turtle[") {
            (
                "pond_turtle",
                &include_bytes!("../../assets/pond_turtle/sprite/s-stand.png")[..],
                Some(coat_palette::turtle(
                    value("shell_color"),
                    hue("shell_hue"),
                    value("skin_color"),
                    hue("skin_hue"),
                )),
            )
        } else if change.record.starts_with("pets.leopard_gecko[") {
            (
                "leopard_gecko",
                &include_bytes!("../../assets/leopard_gecko/sprite/s-stand.png")[..],
                None,
            )
        } else {
            (
                "bearded_dragon",
                &include_bytes!("../../assets/bearded_dragon/sprite/s-stand.png")[..],
                Some(coat_palette::dragon(value("body_color"), hue("hue"))),
            )
        };
        if !previews.contains_key(species) {
            let source = image::load_from_memory(bytes).ok()?.to_rgba8();
            previews.insert(
                species,
                TintPreview::new(source, "s-stand", species == "pond_turtle"),
            );
        }
        let image = previews.get(species)?.render(coat);
        let (width, height) = image.dimensions();
        Some(settings::Preview {
            rgba: image.into_raw(),
            width: width as usize,
            height: height as usize,
        })
    });
    settings::run().expect("settings window failed");
}
