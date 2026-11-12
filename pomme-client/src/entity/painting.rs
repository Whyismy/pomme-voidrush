//! Paintings (vanilla `Painting` and `PaintingRenderer`): a board hung on a
//! wall, as wide and tall as its variant, a sixteenth of a block thick.

use std::collections::HashMap;

use glam::DVec3;

use crate::entity::components::Position;

/// The paintings the game ships (`painting_variant`): asset id, the texture
/// to draw, and the size in blocks. A painting draws variant `i` of the
/// painting model for row `i`.
pub const PAINTINGS: &[(&str, &[&[&str]], u32, u32)] = &[
    (
        "minecraft:alban",
        &[&["minecraft/textures/painting/alban.png"]],
        1,
        1,
    ),
    (
        "minecraft:aztec",
        &[&["minecraft/textures/painting/aztec.png"]],
        1,
        1,
    ),
    (
        "minecraft:aztec2",
        &[&["minecraft/textures/painting/aztec2.png"]],
        1,
        1,
    ),
    (
        "minecraft:backyard",
        &[&["minecraft/textures/painting/backyard.png"]],
        3,
        4,
    ),
    (
        "minecraft:baroque",
        &[&["minecraft/textures/painting/baroque.png"]],
        2,
        2,
    ),
    (
        "minecraft:bomb",
        &[&["minecraft/textures/painting/bomb.png"]],
        1,
        1,
    ),
    (
        "minecraft:bouquet",
        &[&["minecraft/textures/painting/bouquet.png"]],
        3,
        3,
    ),
    (
        "minecraft:burning_skull",
        &[&["minecraft/textures/painting/burning_skull.png"]],
        4,
        4,
    ),
    (
        "minecraft:bust",
        &[&["minecraft/textures/painting/bust.png"]],
        2,
        2,
    ),
    (
        "minecraft:cavebird",
        &[&["minecraft/textures/painting/cavebird.png"]],
        3,
        3,
    ),
    (
        "minecraft:changing",
        &[&["minecraft/textures/painting/changing.png"]],
        4,
        2,
    ),
    (
        "minecraft:cotan",
        &[&["minecraft/textures/painting/cotan.png"]],
        3,
        3,
    ),
    (
        "minecraft:courbet",
        &[&["minecraft/textures/painting/courbet.png"]],
        2,
        1,
    ),
    (
        "minecraft:creebet",
        &[&["minecraft/textures/painting/creebet.png"]],
        2,
        1,
    ),
    (
        "minecraft:dennis",
        &[&["minecraft/textures/painting/dennis.png"]],
        3,
        3,
    ),
    (
        "minecraft:donkey_kong",
        &[&["minecraft/textures/painting/donkey_kong.png"]],
        4,
        3,
    ),
    (
        "minecraft:earth",
        &[&["minecraft/textures/painting/earth.png"]],
        2,
        2,
    ),
    (
        "minecraft:endboss",
        &[&["minecraft/textures/painting/endboss.png"]],
        3,
        3,
    ),
    (
        "minecraft:fern",
        &[&["minecraft/textures/painting/fern.png"]],
        3,
        3,
    ),
    (
        "minecraft:fighters",
        &[&["minecraft/textures/painting/fighters.png"]],
        4,
        2,
    ),
    (
        "minecraft:finding",
        &[&["minecraft/textures/painting/finding.png"]],
        4,
        2,
    ),
    (
        "minecraft:fire",
        &[&["minecraft/textures/painting/fire.png"]],
        2,
        2,
    ),
    (
        "minecraft:graham",
        &[&["minecraft/textures/painting/graham.png"]],
        1,
        2,
    ),
    (
        "minecraft:humble",
        &[&["minecraft/textures/painting/humble.png"]],
        2,
        2,
    ),
    (
        "minecraft:kebab",
        &[&["minecraft/textures/painting/kebab.png"]],
        1,
        1,
    ),
    (
        "minecraft:lowmist",
        &[&["minecraft/textures/painting/lowmist.png"]],
        4,
        2,
    ),
    (
        "minecraft:match",
        &[&["minecraft/textures/painting/match.png"]],
        2,
        2,
    ),
    (
        "minecraft:meditative",
        &[&["minecraft/textures/painting/meditative.png"]],
        1,
        1,
    ),
    (
        "minecraft:orb",
        &[&["minecraft/textures/painting/orb.png"]],
        4,
        4,
    ),
    (
        "minecraft:owlemons",
        &[&["minecraft/textures/painting/owlemons.png"]],
        3,
        3,
    ),
    (
        "minecraft:passage",
        &[&["minecraft/textures/painting/passage.png"]],
        4,
        2,
    ),
    (
        "minecraft:pigscene",
        &[&["minecraft/textures/painting/pigscene.png"]],
        4,
        4,
    ),
    (
        "minecraft:plant",
        &[&["minecraft/textures/painting/plant.png"]],
        1,
        1,
    ),
    (
        "minecraft:pointer",
        &[&["minecraft/textures/painting/pointer.png"]],
        4,
        4,
    ),
    (
        "minecraft:pond",
        &[&["minecraft/textures/painting/pond.png"]],
        3,
        4,
    ),
    (
        "minecraft:pool",
        &[&["minecraft/textures/painting/pool.png"]],
        2,
        1,
    ),
    (
        "minecraft:prairie_ride",
        &[&["minecraft/textures/painting/prairie_ride.png"]],
        1,
        2,
    ),
    (
        "minecraft:sea",
        &[&["minecraft/textures/painting/sea.png"]],
        2,
        1,
    ),
    (
        "minecraft:skeleton",
        &[&["minecraft/textures/painting/skeleton.png"]],
        4,
        3,
    ),
    (
        "minecraft:skull_and_roses",
        &[&["minecraft/textures/painting/skull_and_roses.png"]],
        2,
        2,
    ),
    (
        "minecraft:stage",
        &[&["minecraft/textures/painting/stage.png"]],
        2,
        2,
    ),
    (
        "minecraft:sunflowers",
        &[&["minecraft/textures/painting/sunflowers.png"]],
        3,
        3,
    ),
    (
        "minecraft:sunset",
        &[&["minecraft/textures/painting/sunset.png"]],
        2,
        1,
    ),
    (
        "minecraft:tides",
        &[&["minecraft/textures/painting/tides.png"]],
        3,
        3,
    ),
    (
        "minecraft:unpacked",
        &[&["minecraft/textures/painting/unpacked.png"]],
        4,
        4,
    ),
    (
        "minecraft:void",
        &[&["minecraft/textures/painting/void.png"]],
        2,
        2,
    ),
    (
        "minecraft:wanderer",
        &[&["minecraft/textures/painting/wanderer.png"]],
        1,
        2,
    ),
    (
        "minecraft:wasteland",
        &[&["minecraft/textures/painting/wasteland.png"]],
        1,
        1,
    ),
    (
        "minecraft:water",
        &[&["minecraft/textures/painting/water.png"]],
        2,
        2,
    ),
    (
        "minecraft:wind",
        &[&["minecraft/textures/painting/wind.png"]],
        2,
        2,
    ),
    (
        "minecraft:wither",
        &[&["minecraft/textures/painting/wither.png"]],
        2,
        2,
    ),
];

/// The sizes those come in, each with a back and a blank front after the
/// paintings in the model's variants.
pub const SIZES: &[(u32, u32)] = &[
    (1, 1),
    (1, 2),
    (2, 1),
    (2, 2),
    (3, 3),
    (3, 4),
    (4, 2),
    (4, 3),
    (4, 4),
];

/// `painting/back.png`, the board's back and edges.
pub const BACK_TEXTURE: &[&[&str]] = &[&["minecraft/textures/painting/back.png"]];

/// What a painting shows (`PaintingVariant`).
#[derive(Clone, Debug, PartialEq)]
pub struct PaintingVariant {
    /// In blocks.
    pub width: u32,
    pub height: u32,
    pub asset_id: String,
}

pub struct Painting {
    /// The block it hangs in.
    block: DVec3,
    /// The way it faces, as `Direction.get2DDataValue`: south, west, north,
    /// east.
    pub direction: u8,
    pub variant: Option<PaintingVariant>,
}

impl Painting {
    /// The middle of the board (`HangingEntity.calculateBoundingBox`): the
    /// middle of its block pushed back against the wall, and half a block
    /// along and up for an even width or height.
    pub fn center(&self) -> Option<DVec3> {
        let variant = self.variant.as_ref()?;
        let facing = horizontal(self.direction);
        // `Direction.getCounterClockWise`.
        let along = horizontal((self.direction + 3) % 4);
        let offset = |size: u32| if size.is_multiple_of(2) { 0.5 } else { 0.0 };
        Some(
            self.block + DVec3::splat(0.5) - facing * 0.46875
                + along * offset(variant.width)
                + DVec3::Y * offset(variant.height),
        )
    }

    /// Which variants of the painting model draw it: its picture (or a
    /// blank board for one the game doesn't ship) and its back. `None` for
    /// a size no shipped painting has.
    pub fn model_variants(&self) -> Option<[u32; 2]> {
        let variant = self.variant.as_ref()?;
        let size = SIZES
            .iter()
            .position(|size| *size == (variant.width, variant.height))?;
        let back = (PAINTINGS.len() + size * 2) as u32;
        let front = PAINTINGS
            .iter()
            .position(|(asset, _, width, height)| {
                *asset == variant.asset_id && (*width, *height) == (variant.width, variant.height)
            })
            .map_or(back + 1, |i| i as u32);
        Some([front, back])
    }
}

/// The unit vector of a horizontal direction's 2D data value.
fn horizontal(direction: u8) -> DVec3 {
    match direction % 4 {
        0 => DVec3::Z,
        1 => DVec3::NEG_X,
        2 => DVec3::NEG_Z,
        _ => DVec3::X,
    }
}

#[derive(Default)]
pub struct PaintingStore {
    pub paintings: HashMap<i32, Painting>,
}

impl PaintingStore {
    /// `Painting.recreateFromPacket`: the packet's position is the block,
    /// its data the facing (`Direction.from3DDataValue`).
    pub fn spawn(&mut self, id: i32, position: Position, data: i32) {
        let direction = match data {
            2 => 2,
            4 => 1,
            5 => 3,
            _ => 0,
        };
        self.paintings.insert(
            id,
            Painting {
                block: position.floor(),
                direction,
                variant: None,
            },
        );
    }

    pub fn set_variant(&mut self, id: i32, variant: PaintingVariant) {
        if let Some(painting) = self.paintings.get_mut(&id) {
            painting.variant = Some(variant);
        }
    }

    pub fn remove(&mut self, ids: &[i32]) {
        for id in ids {
            self.paintings.remove(id);
        }
    }

    pub fn clear(&mut self) {
        self.paintings.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hung(data: i32, asset: &str, width: u32, height: u32) -> Painting {
        let mut store = PaintingStore::default();
        store.spawn(1, Position::new(10.0, 64.0, -3.0), data);
        store.set_variant(
            1,
            PaintingVariant {
                width,
                height,
                asset_id: asset.into(),
            },
        );
        store.paintings.remove(&1).unwrap()
    }

    /// A one-block painting sits in the middle of its block against the
    /// wall behind it; an even side moves the middle half a block along
    /// (to the viewer's right) and up.
    #[test]
    fn a_painting_hangs_against_its_wall() {
        // Facing south: the wall is to the north.
        let small = hung(3, "minecraft:kebab", 1, 1);
        assert_eq!(small.direction, 0);
        assert_eq!(small.center(), Some(DVec3::new(10.5, 64.5, -2.96875)));
        let wide = hung(3, "minecraft:pool", 2, 1);
        assert_eq!(wide.center(), Some(DVec3::new(11.0, 64.5, -2.96875)));
        // Facing east, four by four.
        let big = hung(5, "minecraft:pointer", 4, 4);
        assert_eq!(big.direction, 3);
        assert_eq!(big.center(), Some(DVec3::new(10.03125, 65.0, -3.0)));
    }

    #[test]
    fn a_painting_picks_its_picture_and_its_back() {
        let kebab = hung(3, "minecraft:kebab", 1, 1);
        let [front, back] = kebab.model_variants().unwrap();
        assert_eq!(PAINTINGS[front as usize].0, "minecraft:kebab");
        assert_eq!(back as usize, PAINTINGS.len());
        // A pack's own painting of a shipped size hangs as a blank board.
        let custom = hung(3, "voidrush:banner", 2, 2);
        let size = SIZES.iter().position(|s| *s == (2, 2)).unwrap();
        let back = (PAINTINGS.len() + size * 2) as u32;
        assert_eq!(custom.model_variants(), Some([back + 1, back]));
        assert_eq!(hung(3, "voidrush:huge", 8, 8).model_variants(), None);
        // Every shipped painting's size has a back.
        assert!(PAINTINGS.iter().all(|(.., w, h)| SIZES.contains(&(*w, *h))));
    }
}
