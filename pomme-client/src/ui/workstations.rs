//! The workstations whose screens are their slots over a picture: the
//! grindstone (`GrindstoneScreen`), the cartography table
//! (`CartographyTableScreen`), the smithing table (`SmithingScreen`) and
//! the brewing stand (`BrewingStandScreen`, with its fuel and its
//! progress). What each makes is the server's to work out; its result slot
//! shows what the server put there.
// TODO: the cartography table's map preview, and the smithing table's armor
// stand.

use std::time::Instant;

use azalea_inventory::ItemStack;

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

#[allow(clippy::too_many_arguments)]
pub fn build(
    elements: &mut Vec<MenuElement>,
    screen_w: f32,
    screen_h: f32,
    cursor: (f32, f32),
    input: &ContainerInput,
    kind: ContainerKind,
    slots: &[ItemStack],
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
        ContainerKind::Cartography => panel.label(elements, 8.0, 4.0, title),
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
        assert_eq!(brew_bubbles(400), 24.0);
        assert_eq!(brew_bubbles(398), 20.0);
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
}
