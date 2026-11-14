//! Shared palette lookup for the live preview and the desktop runtime.

use std::sync::OnceLock;

use serde::Deserialize;

use crate::coat::{Coat, RegionColor};

#[derive(Clone, Copy)]
pub enum Region {
    Dragon,
    Shell,
    Skin,
}

#[derive(Deserialize)]
struct PaletteColor {
    id: String,
    hsv: [u16; 3],
}

#[derive(Deserialize)]
struct Schema {
    tabs: Vec<Tab>,
}

#[derive(Deserialize)]
struct Tab {
    #[serde(default)]
    fields: Vec<Field>,
}

#[derive(Deserialize)]
struct Field {
    #[serde(default)]
    key: String,
    #[serde(default)]
    item_fields: Vec<Field>,
    #[serde(default)]
    colors: Vec<PaletteColor>,
}

fn catalog(region: Region) -> &'static [PaletteColor] {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    let schema = SCHEMA.get_or_init(|| {
        toml::from_str(include_str!("../schema.toml")).expect("Invalid Settings schema")
    });
    let (list, key) = match region {
        Region::Dragon => ("pets.bearded_dragon", "body_color"),
        Region::Shell => ("pets.pond_turtle", "shell_color"),
        Region::Skin => ("pets.pond_turtle", "skin_color"),
    };
    &schema
        .tabs
        .iter()
        .flat_map(|tab| &tab.fields)
        .find(|field| field.key == list)
        .and_then(|field| field.item_fields.iter().find(|item| item.key == key))
        .expect("Missing built-in coat palette")
        .colors
}

fn custom(value: &str) -> Option<[u16; 3]> {
    let mut values = value.strip_prefix("custom:")?.split(':');
    let hsv = [
        values.next()?.parse().ok()?,
        values.next()?.parse().ok()?,
        values.next()?.parse().ok()?,
    ];
    (values.next().is_none() && hsv[0] < 360 && hsv[1] <= 100 && hsv[2] <= 100).then_some(hsv)
}

impl Region {
    fn color(self, value: Option<&str>, hue: Option<f64>) -> RegionColor {
        let palette = catalog(self);
        let base = palette[0].hsv;
        match value {
            Some(id) if id == palette[0].id => RegionColor::Original,
            Some(id) => palette
                .iter()
                .find(|color| color.id == id)
                .map(|color| color.hsv)
                .or_else(|| custom(id))
                .map_or_else(
                    || legacy(hue, base),
                    |target| RegionColor::Hsv { base, target },
                ),
            None => legacy(hue, base),
        }
    }

    #[allow(dead_code)] // Used by the desktop app, not the Settings host.
    pub fn random(self, rng: &mut impl rand::Rng) -> RegionColor {
        let palette = catalog(self);
        let color = &palette[rng.random_range(0..palette.len())];
        self.color(Some(&color.id), None)
    }
}

fn legacy(hue: Option<f64>, base: [u16; 3]) -> RegionColor {
    match hue.filter(|hue| hue.is_finite()) {
        Some(hue) => RegionColor::Legacy {
            hue,
            base: f64::from(base[0]),
        },
        None => RegionColor::Original,
    }
}

pub fn dragon(value: Option<&str>, hue: Option<f64>) -> Coat {
    Coat::DragonColor(Region::Dragon.color(value, hue))
}

pub fn turtle(
    shell: Option<&str>,
    shell_hue: Option<f64>,
    skin: Option<&str>,
    skin_hue: Option<f64>,
) -> Coat {
    Coat::TurtleColor {
        shell: Region::Shell.color(shell, shell_hue),
        skin: Region::Skin.color(skin, skin_hue),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_colors_come_from_the_same_ten_nearby_presets() {
        use rand::SeedableRng;

        let mut rng = rand::rngs::SmallRng::seed_from_u64(3);
        for region in [Region::Dragon, Region::Shell, Region::Skin] {
            let catalog = catalog(region);
            assert_eq!(catalog.len(), 10);
            let base = catalog[0].hsv[0];
            assert!(
                catalog
                    .iter()
                    .all(|color| base.abs_diff(color.hsv[0]) <= 20)
            );
            let picks: Vec<_> = catalog
                .iter()
                .map(|color| region.color(Some(&color.id), None))
                .collect();
            for _ in 0..100 {
                assert!(picks.contains(&region.random(&mut rng)));
            }
        }
    }

    #[test]
    fn old_hues_are_used_until_a_palette_or_custom_color_is_selected() {
        let region = Region::Shell;
        assert_eq!(
            region.color(None, Some(200.0)),
            RegionColor::Legacy {
                hue: 200.0,
                base: 42.0
            }
        );
        assert_eq!(
            region.color(Some("original"), Some(200.0)),
            RegionColor::Original
        );
        assert!(matches!(
            region.color(Some("custom:0:80:90"), None),
            RegionColor::Hsv {
                target: [0, 80, 90],
                ..
            }
        ));
        assert_eq!(
            region.color(Some("custom:360:100:100"), None),
            RegionColor::Original
        );
    }
}
