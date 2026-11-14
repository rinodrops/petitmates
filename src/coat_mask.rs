//! Build-time masks for the built-in sprites. A replaced PNG uses live inference.

use image::RgbaImage;

use crate::coat::{self, Coat, TintRegion};

struct Mask {
    dimensions: (u32, u32),
    png_len: usize,
    crc: u32,
    start: usize,
    end: usize,
}

const MASKS: &[((bool, &str), Mask)] = include!(concat!(env!("OUT_DIR"), "/coat_masks.rs"));
const DATA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/coat_masks.bin"));

fn lookup(image: &RgbaImage, png: &[u8], sprite: &str, turtle: bool) -> Option<&'static [u8]> {
    let index = MASKS
        .binary_search_by(|(key, _)| key.cmp(&(turtle, sprite)))
        .ok()?;
    let mask = &MASKS[index].1;
    if mask.dimensions != image.dimensions()
        || mask.png_len != png.len()
        || mask.crc != crc32fast::hash(png)
    {
        return None;
    }
    Some(&DATA[mask.start..mask.end])
}

fn runs(mask: &[u8]) -> impl Iterator<Item = (TintRegion, usize)> + '_ {
    mask.iter().map(|&byte| {
        let region = match byte >> 6 {
            0 => TintRegion::Protected,
            1 => TintRegion::Primary,
            2 => TintRegion::Secondary,
            _ => unreachable!("Invalid generated coat mask"),
        };
        (region, usize::from(byte & 63) + 1)
    })
}

#[allow(dead_code)] // Used by the desktop app, not the Settings host.
pub fn tint_builtin(image: &mut RgbaImage, png: &[u8], sprite: &str, coat: Coat) {
    if coat.is_original() {
        return;
    }
    if let Some(mask) = lookup(image, png, sprite, coat::is_turtle(coat)) {
        coat::apply_tint(image, runs(mask), coat);
    } else {
        coat::tint_image(image, sprite, coat);
    }
}

/// Prepared standing sprite for repeated edits in the Settings preview.
#[allow(dead_code)]
pub struct TintPreview {
    image: RgbaImage,
    mask: Option<&'static [u8]>,
}

#[allow(dead_code)]
impl TintPreview {
    pub fn new(png: &[u8], coat: Option<Coat>) -> Option<Self> {
        let image = image::load_from_memory(png).ok()?.into_rgba8();
        let mask = coat.and_then(|coat| lookup(&image, png, "s-stand", coat::is_turtle(coat)));
        Some(Self { image, mask })
    }

    pub fn render(&self, coat: Option<Coat>) -> RgbaImage {
        let mut image = self.image.clone();
        if let Some(coat) = coat.filter(|coat| !coat.is_original()) {
            if let Some(mask) = self.mask {
                coat::apply_tint(&mut image, runs(mask), coat);
            } else {
                coat::tint_image(&mut image, "s-stand", coat);
            }
        }
        image
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coat::RegionColor::{Hsv, Original};

    #[test]
    fn every_built_in_mask_matches_inference_and_preserves_color_output() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        for (species, turtle) in [("bearded_dragon", false), ("pond_turtle", true)] {
            let sprite_dir = root.join(species).join("sprite");
            let mut count = 0;
            for entry in std::fs::read_dir(sprite_dir).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_none_or(|ext| ext != "png") {
                    continue;
                }
                let name = path.file_stem().unwrap().to_str().unwrap();
                let png = std::fs::read(&path).unwrap();
                let source = image::load_from_memory(&png).unwrap().into_rgba8();
                let mask = lookup(&source, &png, name, turtle)
                    .unwrap_or_else(|| panic!("Missing mask for {}", path.display()));
                let inferred = coat::tint_regions(&source, name, turtle);
                assert!(
                    runs(mask)
                        .flat_map(|(region, len)| std::iter::repeat_n(region, len))
                        .eq(inferred.iter().copied()),
                    "Incorrect mask for {}",
                    path.display()
                );

                let coats = if turtle {
                    let shell = Hsv {
                        base: [42, 48, 42],
                        target: [192, 61, 70],
                    };
                    let skin = Hsv {
                        base: [46, 58, 52],
                        target: [308, 79, 60],
                    };
                    vec![
                        Coat::TurtleColor {
                            shell: Original,
                            skin: Original,
                        },
                        crate::coat_palette::turtle(Some("walnut"), None, Some("olive"), None),
                        Coat::TurtleColor { shell, skin },
                        Coat::TurtleColor {
                            shell,
                            skin: Original,
                        },
                        Coat::TurtleColor {
                            shell: Original,
                            skin,
                        },
                    ]
                } else {
                    vec![
                        Coat::DragonColor(Original),
                        crate::coat_palette::dragon(Some("apricot"), None),
                        crate::coat_palette::dragon(Some("custom:230:80:90"), None),
                    ]
                };
                for coat in coats {
                    let mut expected = source.clone();
                    if !coat.is_original() {
                        coat::apply_tint(&mut expected, coat::region_runs(&inferred), coat);
                    }
                    let mut actual = source.clone();
                    tint_builtin(&mut actual, &png, name, coat);
                    assert_eq!(
                        actual.as_raw(),
                        expected.as_raw(),
                        "Color mismatch for {}",
                        path.display()
                    );
                    if name == "s-stand" {
                        let preview = TintPreview::new(&png, Some(coat)).unwrap();
                        assert!(preview.mask.is_some());
                        assert_eq!(preview.render(Some(coat)).as_raw(), expected.as_raw());
                    }
                }
                count += 1;
            }
            assert_eq!(
                count,
                MASKS
                    .iter()
                    .filter(|((kind, _), _)| *kind == turtle)
                    .count()
            );
        }
    }

    #[test]
    fn a_replaced_sprite_of_the_same_size_uses_inference() {
        let sprite = "s-stand";
        let original = include_bytes!("../assets/bearded_dragon/sprite/s-stand.png");
        let replacement = include_bytes!("../assets/pond_turtle/sprite/s-stand.png");
        let original_image = image::load_from_memory(original).unwrap().into_rgba8();
        assert!(lookup(&original_image, original, sprite, false).is_some());

        let mut changed = original.to_vec();
        changed[40] ^= 1;
        assert!(lookup(&original_image, &changed, sprite, false).is_none());

        let coat = crate::coat_palette::dragon(Some("apricot"), None);
        let mut actual = image::load_from_memory(replacement).unwrap().into_rgba8();
        assert!(lookup(&actual, replacement, sprite, false).is_none());
        let mut expected = actual.clone();
        coat::tint_image(&mut expected, sprite, coat);
        tint_builtin(&mut actual, replacement, sprite, coat);
        assert_eq!(actual.as_raw(), expected.as_raw());
    }
}
