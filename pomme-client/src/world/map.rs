//! The level's maps as the server has sent them: vanilla
//! `MapItemSavedData` on the client, with `MapColor`'s palette and the
//! built-in marker types.

use std::collections::HashMap;

use pomme_protocol::map::{MAP_SIZE, MapData, Patch};

use crate::ui::text::TextSpan;

/// `MapColor.MATERIAL_COLORS`' RGB by id; `NONE` (0) is see-through.
const BASE_COLORS: [u32; 62] = [
    0x000000, 0x7FB238, 0xF7E9A3, 0xC7C7C7, 0xFF0000, 0xA0A0FF, 0xA7A7A7, 0x007C00, 0xFFFFFF,
    0xA4A8B8, 0x976D4D, 0x707070, 0x4040FF, 0x8F7748, 0xFFFCF5, 0xD87F33, 0xB24CD8, 0x6699D8,
    0xE5E533, 0x7FCC19, 0xF27FA5, 0x4C4C4C, 0x999999, 0x4C7F99, 0x7F3FB2, 0x334CB2, 0x664C33,
    0x667F33, 0x993333, 0x191919, 0xFAEE4D, 0x5CDBD5, 0x4A80FF, 0x00D93A, 0x815631, 0x700200,
    0xD1B1A1, 0x9F5224, 0x95576C, 0x706C8A, 0xBA8524, 0x677535, 0xA04D4E, 0x392923, 0x876B62,
    0x575C5C, 0x7A4958, 0x4C3E5C, 0x4C3223, 0x4C522A, 0x8E3C2E, 0x251610, 0xBD3031, 0x943F61,
    0x5C191D, 0x167E86, 0x3A8E8C, 0x562C3E, 0x14B485, 0x646464, 0xD8AF93, 0x7FA796,
];

/// `MapColor.Brightness`' modifiers by id: low, normal, high, lowest.
const BRIGHTNESS: [u32; 4] = [180, 220, 255, 135];

/// Vanilla `MapColor.getColorFromPackedId` as RGBA: the colour id in the
/// high six bits at the brightness in the low two. `NONE` and ids past the
/// palette are see-through.
pub fn color_rgba(packed: u8) -> [u8; 4] {
    let Some(&rgb) = BASE_COLORS.get(usize::from(packed >> 2)) else {
        return [0; 4];
    };
    if packed >> 2 == 0 {
        return [0; 4];
    }
    let modifier = BRIGHTNESS[usize::from(packed & 3)];
    let scale = |shift: u32| ((rgb >> shift & 0xFF) * modifier / 255) as u8;
    [scale(16), scale(8), scale(0), 255]
}

/// `MapDecorationTypes`, in registry order: the texture under
/// `textures/map/decorations/` and whether it shows on a framed map.
const DECORATION_TYPES: [(&str, bool); 35] = [
    ("player", false),
    ("frame", true),
    ("red_marker", false),
    ("blue_marker", false),
    ("target_x", true),
    ("target_point", true),
    ("player_off_map", false),
    ("player_off_limits", false),
    ("woodland_mansion", true),
    ("ocean_monument", true),
    ("white_banner", true),
    ("orange_banner", true),
    ("magenta_banner", true),
    ("light_blue_banner", true),
    ("yellow_banner", true),
    ("lime_banner", true),
    ("pink_banner", true),
    ("gray_banner", true),
    ("light_gray_banner", true),
    ("cyan_banner", true),
    ("purple_banner", true),
    ("blue_banner", true),
    ("brown_banner", true),
    ("green_banner", true),
    ("red_banner", true),
    ("black_banner", true),
    ("red_x", true),
    ("desert_village", true),
    ("plains_village", true),
    ("savanna_village", true),
    ("snowy_village", true),
    ("taiga_village", true),
    ("jungle_temple", true),
    ("swamp_hut", true),
    ("trial_chambers", true),
];

/// Every marker texture's name, in registry order.
pub fn decoration_textures() -> impl Iterator<Item = &'static str> {
    DECORATION_TYPES.iter().map(|(name, _)| *name)
}

/// A marker on a map (`MapDecoration`).
#[derive(Clone, Debug, PartialEq)]
pub struct MapDecoration {
    /// Index into [`decoration_textures`].
    pub kind: usize,
    /// Half map pixels from the map's centre.
    pub x: i8,
    pub y: i8,
    /// Sixteenths of a turn.
    pub rot: i8,
    pub name: Option<Vec<TextSpan>>,
}

impl MapDecoration {
    /// `MapDecorationType.showOnItemFrame`.
    pub fn shows_on_item_frame(&self) -> bool {
        DECORATION_TYPES[self.kind].1
    }
}

/// One map (`MapItemSavedData`).
pub struct SavedMap {
    /// `MAP_SIZE` rows of packed colours.
    pub colors: Box<[u8]>,
    pub decorations: Vec<MapDecoration>,
    /// How far the map is zoomed out, none to four times.
    pub scale: i8,
    /// A locked map no longer follows the world.
    pub locked: bool,
    /// Changes whenever `colors` does, for whoever caches its picture.
    pub revision: u64,
}

impl SavedMap {
    /// The map's picture as `MAP_SIZE` rows of RGBA.
    pub fn rgba(&self) -> Vec<u8> {
        self.colors.iter().flat_map(|&c| color_rgba(c)).collect()
    }
}

/// The level's maps by id (`ClientLevel.mapData`).
#[derive(Default)]
pub struct MapStore {
    maps: HashMap<i32, SavedMap>,
    revisions: u64,
}

impl MapStore {
    pub fn get(&self, id: i32) -> Option<&SavedMap> {
        self.maps.get(&id)
    }

    /// A new level starts without maps.
    pub fn clear(&mut self) {
        self.maps.clear();
    }

    /// Vanilla `ClientPacketListener.handleMapItemData`: the map is made on
    /// its first packet, then takes the packet's markers and colour patch.
    /// `name` reads a marker's label.
    pub fn apply(&mut self, data: MapData, name: impl Fn(&[u8]) -> Option<Vec<TextSpan>>) {
        self.revisions += 1;
        let revision = self.revisions;
        let map = self.maps.entry(data.id).or_insert_with(|| SavedMap {
            colors: vec![0; MAP_SIZE * MAP_SIZE].into(),
            decorations: Vec::new(),
            scale: data.scale,
            locked: data.locked,
            revision,
        });
        map.scale = data.scale;
        map.locked = data.locked;
        if let Some(decorations) = data.decorations {
            map.decorations = decorations
                .into_iter()
                .filter(|d| (d.kind as usize) < DECORATION_TYPES.len())
                .map(|d| MapDecoration {
                    kind: d.kind as usize,
                    x: d.x,
                    y: d.y,
                    rot: d.rot,
                    name: d.name.as_deref().and_then(&name),
                })
                .collect();
        }
        if let Some(patch) = data.patch {
            apply_patch(&mut map.colors, &patch);
            map.revision = revision;
        }
    }
}

/// `MapPatch.applyToMap`; a patch reaching past the map is cut at its edge.
fn apply_patch(colors: &mut [u8], patch: &Patch) {
    let width = usize::from(patch.width);
    for (row, line) in patch.colors.chunks_exact(width).enumerate() {
        let y = usize::from(patch.start_y) + row;
        let x = usize::from(patch.start_x);
        if y >= MAP_SIZE || x >= MAP_SIZE {
            continue;
        }
        let n = width.min(MAP_SIZE - x);
        colors[y * MAP_SIZE + x..][..n].copy_from_slice(&line[..n]);
    }
}

#[cfg(test)]
mod tests {
    use pomme_protocol::map::Decoration;

    use super::*;

    fn packet(decorations: Option<Vec<Decoration>>, patch: Option<Patch>) -> MapData {
        MapData {
            id: 3,
            scale: 0,
            locked: true,
            decorations,
            patch,
        }
    }

    /// `MapColor`: grass at each brightness, and the see-through ids.
    #[test]
    fn colors_follow_the_palette_and_brightness() {
        assert_eq!(color_rgba(4), [89, 125, 39, 255]);
        assert_eq!(color_rgba(5), [109, 153, 48, 255]);
        assert_eq!(color_rgba(6), [127, 178, 56, 255]);
        assert_eq!(color_rgba(7), [67, 94, 29, 255]);
        // Snow at full brightness is white; glow lichen is the last colour.
        assert_eq!(color_rgba(8 * 4 + 2), [255, 255, 255, 255]);
        assert_eq!(color_rgba(61 * 4 + 2), [127, 167, 150, 255]);
        for none in [0, 1, 2, 3, 62 * 4, 255] {
            assert_eq!(color_rgba(none), [0; 4]);
        }
    }

    #[test]
    fn a_patch_lands_where_it_says() {
        let mut store = MapStore::default();
        let patch = Patch {
            start_x: 126,
            start_y: 1,
            width: 2,
            height: 2,
            colors: [4, 5, 6, 7].into(),
        };
        store.apply(packet(None, Some(patch)), |_| None);
        let map = store.get(3).unwrap();
        assert_eq!(map.colors[MAP_SIZE + 126..MAP_SIZE + 128], [4, 5]);
        assert_eq!(map.colors[2 * MAP_SIZE + 126..2 * MAP_SIZE + 128], [6, 7]);
        assert_eq!(map.colors.iter().filter(|&&c| c != 0).count(), 4);
        assert_eq!(&map.rgba()[(MAP_SIZE + 126) * 4..][..4], [89, 125, 39, 255]);
    }

    /// A packet's marker list replaces the map's; one without leaves it, and
    /// only a patch moves the picture's revision.
    #[test]
    fn markers_replace_and_only_patches_touch_the_picture() {
        let mut store = MapStore::default();
        let marker = |kind| Decoration {
            kind,
            x: 4,
            y: -4,
            rot: 8,
            name: None,
        };
        store.apply(packet(Some(vec![marker(26), marker(0)]), None), |_| None);
        let first = store.get(3).unwrap().revision;
        assert_eq!(store.get(3).unwrap().decorations.len(), 2);
        assert!(store.get(3).unwrap().decorations[0].shows_on_item_frame());
        assert!(!store.get(3).unwrap().decorations[1].shows_on_item_frame());

        store.apply(packet(None, None), |_| None);
        assert_eq!(store.get(3).unwrap().decorations.len(), 2);
        // An unknown type is left out.
        store.apply(packet(Some(vec![marker(6), marker(99)]), None), |_| None);
        let map = store.get(3).unwrap();
        assert_eq!(map.decorations.len(), 1);
        assert_eq!(
            decoration_textures().nth(map.decorations[0].kind),
            Some("player_off_map")
        );
        assert_eq!(map.revision, first);
    }
}
