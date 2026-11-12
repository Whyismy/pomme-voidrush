//! A beacon's beam (vanilla `BeaconBlockEntity`): whether the beacon stands
//! on a base, and the coloured sections the beam passes through on its way
//! up, which the client works out for itself from the blocks.

/// One stretch of a beam in one colour (`BeaconBeamOwner.Section`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeamSection {
    /// `0xRRGGBB`.
    pub color: u32,
    /// In blocks.
    pub height: u32,
}

/// What the beam meets in a block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BeamBlock {
    /// A `BeaconBeamBlock`: the beacon itself or stained glass, of this
    /// colour (`DyeColor.getTextureDiffuseColor`).
    Tint(u32),
    /// Anything the beam passes through.
    Clear,
    /// A block light can't pass (but for bedrock): no beam at all.
    Opaque,
}

/// What a block is to a beam, by its name and whether it blocks all light.
pub fn beam_block(id: &str, blocks_light: bool) -> BeamBlock {
    if id == "beacon" {
        return BeamBlock::Tint(0xFF_FFFF);
    }
    let glass = id
        .strip_suffix("_stained_glass_pane")
        .or_else(|| id.strip_suffix("_stained_glass"));
    match glass {
        Some(dye) => BeamBlock::Tint(crate::banner::dye_rgb_named(dye)),
        None if blocks_light && id != "bedrock" => BeamBlock::Opaque,
        None => BeamBlock::Clear,
    }
}

/// Whether `id` is one of the blocks a beacon's pyramid is built of
/// (`BlockTags.BEACON_BASE_BLOCKS`).
pub fn is_base_block(id: &str) -> bool {
    matches!(
        id,
        "iron_block" | "gold_block" | "diamond_block" | "emerald_block" | "netherite_block"
    )
}

/// `ARGB.average` of two colours.
fn average(a: u32, b: u32) -> u32 {
    let channel = |shift: u32| (((a >> shift) & 0xFF) + ((b >> shift) & 0xFF)) / 2;
    channel(16) << 16 | channel(8) << 8 | channel(0)
}

/// The beam's sections from the beacon up (`BeaconBlockEntity.tick`'s scan),
/// `blocks` being what it meets from the beacon's own block upward; empty
/// when something opaque is in the way.
pub fn beam_sections(blocks: impl Iterator<Item = BeamBlock>) -> Vec<BeamSection> {
    let mut sections: Vec<BeamSection> = Vec::new();
    for block in blocks {
        let count = sections.len();
        match block {
            // The beacon and the first glass over it each start a section;
            // after that a new colour starts one, mixed with the last.
            BeamBlock::Tint(color) => match sections.last_mut() {
                Some(last) if count > 1 && last.color == color => last.height += 1,
                Some(last) if count > 1 => {
                    let color = average(last.color, color);
                    sections.push(BeamSection { color, height: 1 });
                }
                _ => sections.push(BeamSection { color, height: 1 }),
            },
            BeamBlock::Clear => match sections.last_mut() {
                Some(last) => last.height += 1,
                None => return Vec::new(),
            },
            BeamBlock::Opaque => return Vec::new(),
        }
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_beam_takes_the_glass_it_passes() {
        use BeamBlock::{Clear, Opaque, Tint};
        let white = 0xFF_FFFF;
        let red = crate::banner::dye_rgb_named("red");
        let blue = crate::banner::dye_rgb_named("blue");
        assert_eq!(beam_block("beacon", false), Tint(white));
        assert_eq!(beam_block("red_stained_glass", false), Tint(red));
        assert_eq!(beam_block("blue_stained_glass_pane", false), Tint(blue));
        assert_eq!(beam_block("glass", false), Clear);
        assert_eq!(beam_block("stone", true), Opaque);
        assert_eq!(beam_block("bedrock", true), Clear);

        // Open sky: one white section.
        let open = beam_sections([Tint(white), Clear, Clear].into_iter());
        assert_eq!(
            open,
            [BeamSection {
                color: white,
                height: 3
            }]
        );
        // The first glass takes its own colour; a second colour mixes in.
        let tinted = beam_sections(
            [Tint(white), Clear, Tint(red), Tint(red), Clear, Tint(blue)].into_iter(),
        );
        assert_eq!(
            tinted,
            [
                BeamSection {
                    color: white,
                    height: 2
                },
                BeamSection {
                    color: red,
                    height: 3
                },
                BeamSection {
                    color: average(red, blue),
                    height: 1
                },
            ]
        );
        // A roof: no beam.
        assert!(beam_sections([Tint(white), Clear, Opaque, Clear].into_iter()).is_empty());
        assert!(is_base_block("iron_block") && !is_base_block("stone"));
    }
}
