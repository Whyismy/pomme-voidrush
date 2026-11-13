//! The workstations whose screens are their slots over a picture: the
//! grindstone (`GrindstoneScreen`), the cartography table
//! (`CartographyTableScreen`), the smithing table (`SmithingScreen`) and
//! the brewing stand (`BrewingStandScreen`, with its fuel and its
//! progress). What each makes is the server's to work out; its result slot
//! shows what the server put there.
// TODO: the markers on the cartography table's map preview, and the
// smithing table's armor stand.

use std::time::Instant;

use azalea_inventory::ItemStack;
use azalea_registry::builtin::ItemKind;

use super::chest::build_slots;
use super::common::FONT_SIZE;
use super::container::{
    ContainerInput, ContainerResult, DragState, Panel, push_clipped_sprite, push_panel,
};
use crate::player::menu_click::ContainerKind;
use crate::renderer::pipelines::menu_overlay::{MenuElement, SpriteId};

/// Where each of a workstation's own slots sits, in GUI units.
fn slot_at(kind: ContainerKind, slot: u16) -> (f32, f32) {
    let places: &[(f32, f32)] = match kind {
        ContainerKind::Grindstone => &[(49.0, 19.0), (49.0, 40.0), (129.0, 34.0)],
        ContainerKind::Cartography => &[(15.0, 15.0), (15.0, 52.0), (145.0, 39.0)],
        ContainerKind::Smithing => &[(8.0, 48.0), (26.0, 48.0), (44.0, 48.0), (98.0, 48.0)],
        // The three bottles, the ingredient over them, the fuel.
        _ => &[
            (56.0, 51.0),
            (79.0, 58.0),
            (102.0, 51.0),
            (79.0, 17.0),
            (17.0, 17.0),
        ],
    };
    places[usize::from(slot).min(places.len() - 1)]
}

/// The dynamic atlas's key for the map a cartography table shows.
pub const MAP_PREVIEW_KEY: &str = "pomme:cartography_map";

/// The map in a cartography table's first slot, as the client knows it.
#[derive(Clone, Copy)]
pub struct MapView {
    pub scale: i8,
    pub locked: bool,
    /// Its picture is in the atlas, under [`MAP_PREVIEW_KEY`].
    pub drawn: bool,
}

/// What the cartography table will make of its map and the item beside it
/// (`CartographyTableScreen.extractBackground`): which picture frames the
/// preview, where the map goes in it and how large, and whether the arrow
/// is crossed out.
#[derive(Debug, PartialEq)]
struct CartographyPreview {
    frames: Vec<MapFrame>,
    locked: bool,
    error: bool,
}

/// One of the preview's pictures: its sprite and box, then where the map
/// is drawn over it and at what scale.
#[derive(Debug, PartialEq)]
struct MapFrame {
    sprite: SpriteId,
    at: [f32; 4],
    map_at: (f32, f32),
    scale: f32,
}

fn cartography_preview(map: Option<MapView>, addition: Option<ItemKind>) -> CartographyPreview {
    let frame = |sprite, at, map_at, scale| MapFrame {
        sprite,
        at,
        map_at,
        scale,
    };
    let paper = addition == Some(ItemKind::Paper);
    let pane = addition == Some(ItemKind::GlassPane);
    let copy = addition == Some(ItemKind::Map);
    // A locked map takes nothing more, nor does one zoomed out all the way.
    let locked_map = map.is_some_and(|map| map.locked);
    let full = locked_map || (paper && map.is_some_and(|map| map.scale >= 4));
    let error = (locked_map && (paper || pane)) || (paper && map.is_some_and(|map| map.scale >= 4));
    let whole = [67.0, 13.0, 66.0, 66.0];
    let frames = if paper && !full {
        vec![frame(
            SpriteId::CartographyScaledMap,
            whole,
            (85.0, 31.0),
            0.226,
        )]
    } else if copy {
        let sprite = SpriteId::CartographyDuplicatedMap;
        vec![
            frame(sprite, [83.0, 13.0, 50.0, 66.0], (86.0, 16.0), 0.34),
            frame(sprite, [67.0, 29.0, 50.0, 66.0], (70.0, 32.0), 0.34),
        ]
    } else {
        vec![frame(SpriteId::CartographyMap, whole, (71.0, 17.0), 0.45)]
    };
    CartographyPreview {
        frames,
        locked: pane,
        error,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build(
    elements: &mut Vec<MenuElement>,
    screen_w: f32,
    screen_h: f32,
    cursor: (f32, f32),
    input: &ContainerInput,
    kind: ContainerKind,
    slots: &[ItemStack],
    map: Option<MapView>,
    data: &[i16],
    title: &str,
    cursor_item: &ItemStack,
    drag: &mut Option<DragState>,
    last_click: &mut Option<(u16, Instant)>,
    gs: f32,
    text_width: &dyn Fn(&str, f32) -> f32,
) -> ContainerResult {
    let background = match kind {
        ContainerKind::Grindstone => SpriteId::GrindstoneBackground,
        ContainerKind::Cartography => SpriteId::CartographyBackground,
        ContainerKind::Smithing => SpriteId::SmithingBackground,
        _ => SpriteId::BrewingStandBackground,
    };
    let panel = push_panel(elements, screen_w, screen_h, gs, 166.0, background);
    let has = |slot: usize| slots.get(slot).is_some_and(ItemStack::is_present);
    match kind {
        ContainerKind::Grindstone => {
            // Something to grind and nothing it can become.
            if (has(0) || has(1)) && !has(2) {
                panel.image(elements, SpriteId::GrindstoneError, 92.0, 31.0, 28.0, 21.0);
            }
            panel.label(elements, 8.0, 6.0, title);
        }
        ContainerKind::Smithing => {
            if has(0) && has(1) && has(2) && !has(3) {
                panel.image(elements, SpriteId::SmithingError, 65.0, 46.0, 28.0, 21.0);
            }
            panel.label(elements, 44.0, 15.0, title);
        }
        ContainerKind::Cartography => {
            let addition = slots.get(1).and_then(ItemStack::as_present);
            let preview = cartography_preview(map, addition.map(|stack| stack.kind));
            if preview.error {
                panel.image(elements, SpriteId::CartographyError, 35.0, 31.0, 28.0, 21.0);
            }
            for MapFrame {
                sprite,
                at: [x, y, w, h],
                map_at: at,
                scale,
            } in preview.frames
            {
                panel.image(elements, sprite, x, y, w, h);
                if map.is_some_and(|map| map.drawn) {
                    elements.push(MenuElement::Favicon {
                        x: panel.ox + at.0 * panel.scale,
                        y: panel.oy + at.1 * panel.scale,
                        size: 128.0 * scale * panel.scale,
                        address: MAP_PREVIEW_KEY.to_owned(),
                    });
                }
            }
            if preview.locked {
                panel.image(
                    elements,
                    SpriteId::CartographyLocked,
                    118.0,
                    60.0,
                    10.0,
                    14.0,
                );
            }
            panel.label(elements, 8.0, 4.0, title);
        }
        _ => {
            push_brewing_progress(elements, &panel, data);
            let title_w = text_width(title, FONT_SIZE * panel.scale) / panel.scale;
            panel.label(elements, ((176.0 - title_w) / 2.0).floor(), 6.0, title);
        }
    }
    panel.label(elements, 8.0, 72.0, "Inventory");

    build_slots(
        elements,
        &panel,
        cursor,
        input,
        kind,
        84.0,
        &|slot| slot_at(kind, slot),
        slots,
        cursor_item,
        drag,
        last_click,
    )
}

/// How far `ticks` of brewing left (of 400) has filled the 28-pixel arrow.
fn brew_arrow(ticks: i16) -> f32 {
    (28.0 * (1.0 - f32::from(ticks) / 400.0)).floor().max(0.0)
}

/// The bubbles' height, of 29 pixels, which rises through seven steps as
/// the brewing runs.
fn brew_bubbles(ticks: i16) -> f32 {
    const LENGTHS: [f32; 7] = [29.0, 24.0, 20.0, 16.0, 11.0, 6.0, 0.0];
    LENGTHS[(ticks / 2).rem_euclid(7) as usize]
}

/// The fuel bar's width, of 18 pixels, for `fuel` charges left of 20.
fn fuel_bar(fuel: i16) -> f32 {
    f32::from(((18 * fuel + 20 - 1) / 20).clamp(0, 18))
}

/// Vanilla `BrewingStandScreen.extractBackground`: the fuel left, and while
/// something brews, the arrow filling downward and the bubbles rising.
fn push_brewing_progress(elements: &mut Vec<MenuElement>, panel: &Panel, data: &[i16]) {
    let ticks = data.first().copied().unwrap_or(0);
    let fuel = fuel_bar(data.get(1).copied().unwrap_or(0));
    if fuel > 0.0 {
        push_clipped_sprite(
            elements,
            panel,
            SpriteId::BrewingFuelLength,
            [60.0, 44.0, 18.0, 4.0],
            [60.0, 44.0, fuel, 4.0],
        );
    }
    if ticks > 0 {
        let arrow = brew_arrow(ticks);
        if arrow > 0.0 {
            push_clipped_sprite(
                elements,
                panel,
                SpriteId::BrewingProgress,
                [97.0, 16.0, 9.0, 28.0],
                [97.0, 16.0, 9.0, arrow],
            );
        }
        let bubbles = brew_bubbles(ticks);
        if bubbles > 0.0 {
            push_clipped_sprite(
                elements,
                panel,
                SpriteId::BrewingBubbles,
                [63.0, 14.0, 12.0, 29.0],
                [63.0, 14.0 + 29.0 - bubbles, 12.0, bubbles],
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The brewing stand's bars follow vanilla's arithmetic: the arrow
    /// fills as the 400 ticks run down, the bubbles step through their
    /// seven heights, and any fuel at all shows at least a pixel.
    #[test]
    fn the_brewing_bars_follow_the_stand() {
        assert_eq!(brew_arrow(400), 0.0);
        assert_eq!(brew_arrow(200), 14.0);
        assert_eq!(brew_arrow(1), 27.0);
        assert_eq!(brew_bubbles(400), 11.0);
        assert_eq!(brew_bubbles(398), 16.0);
        assert_eq!(brew_bubbles(14), 29.0);
        assert_eq!(brew_bubbles(12), 0.0);
        assert_eq!(fuel_bar(0), 0.0);
        assert_eq!(fuel_bar(1), 1.0);
        assert_eq!(fuel_bar(20), 18.0);
        assert_eq!(fuel_bar(40), 18.0);
    }

    /// Each workstation's slots are where its picture has them.
    #[test]
    fn the_slots_sit_on_their_pictures() {
        assert_eq!(slot_at(ContainerKind::Grindstone, 2), (129.0, 34.0));
        assert_eq!(slot_at(ContainerKind::Cartography, 1), (15.0, 52.0));
        assert_eq!(slot_at(ContainerKind::Smithing, 3), (98.0, 48.0));
        assert_eq!(slot_at(ContainerKind::BrewingStand, 3), (79.0, 17.0));
        assert_eq!(slot_at(ContainerKind::BrewingStand, 4), (17.0, 17.0));
    }

    /// What the cartography table shows of a map and the item beside it:
    /// zoomed out with paper, twice over with an empty map, locked with a
    /// pane, and the arrow crossed out where the map takes no more.
    #[test]
    fn the_cartography_table_previews_its_result() {
        let map = |scale: i8, locked: bool| {
            Some(MapView {
                scale,
                locked,
                drawn: true,
            })
        };
        let plain = cartography_preview(map(0, false), None);
        assert_eq!(plain.frames.len(), 1);
        assert_eq!(plain.frames[0].sprite, SpriteId::CartographyMap);
        assert_eq!(
            (plain.frames[0].map_at, plain.frames[0].scale),
            ((71.0, 17.0), 0.45)
        );
        assert!(!plain.locked && !plain.error);

        let scaled = cartography_preview(map(0, false), Some(ItemKind::Paper));
        assert_eq!(scaled.frames[0].sprite, SpriteId::CartographyScaledMap);
        assert_eq!(scaled.frames[0].scale, 0.226);
        assert!(!scaled.error);

        let copied = cartography_preview(map(2, false), Some(ItemKind::Map));
        assert_eq!(copied.frames.len(), 2);
        assert_eq!(copied.frames[1].map_at, (70.0, 32.0));

        let locking = cartography_preview(map(0, false), Some(ItemKind::GlassPane));
        assert!(locking.locked && !locking.error);

        // A map zoomed out four times takes no more paper, and a locked
        // map neither paper nor a pane; either shows as it is.
        let full = cartography_preview(map(4, false), Some(ItemKind::Paper));
        assert!(full.error);
        assert_eq!(full.frames[0].sprite, SpriteId::CartographyMap);
        assert!(cartography_preview(map(0, true), Some(ItemKind::GlassPane)).error);
        assert!(cartography_preview(map(0, true), Some(ItemKind::Paper)).error);
        assert!(!cartography_preview(map(0, true), Some(ItemKind::Map)).error);
        // No map: the empty frame, and nothing wrong.
        assert!(!cartography_preview(None, Some(ItemKind::Paper)).error);
    }
}
