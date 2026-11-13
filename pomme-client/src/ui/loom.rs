//! Vanilla `LoomScreen`: a banner, a dye and perhaps a pattern item go in;
//! the patterns they can make are listed four across (four rows showing, a
//! scroller for more), and picking one is the menu's button click. The
//! banner as it would come out is drawn beside them.

use std::time::Instant;

use azalea_inventory::components::BannerPatterns;
use azalea_inventory::{ItemStack, ItemStackData};

use super::chest::build_slots;
use super::common::hit_test;
use super::container::{ContainerInput, ContainerResult, DragState, Panel, push_panel};
use crate::player::inventory::item_resource_name;
use crate::player::menu_click::ContainerKind;
use crate::renderer::pipelines::menu_overlay::{MenuElement, SpriteId, banner_pattern_sprite};

const COLUMNS: usize = 4;
const ROWS: usize = 4;
/// A banner takes six patterns (`LoomScreen.containerChanged`).
const MAX_PATTERNS: usize = 6;

/// Where the pattern list is scrolled.
#[derive(Default)]
pub struct LoomState {
    /// Through the rows off screen, 0 at the top to 1.
    scroll: f32,
    dragging: bool,
    /// What the list was last built for; a change starts it over.
    seen: Option<[ItemStack; 3]>,
}

/// The rows of `count` patterns.
fn total_rows(count: usize) -> usize {
    count.div_ceil(COLUMNS)
}

/// The first row on screen at `scroll` through `count` patterns
/// (`LoomScreen.mouseScrolled`).
fn start_row(scroll: f32, count: usize) -> usize {
    let off_screen = total_rows(count).saturating_sub(ROWS);
    (f64::from(scroll * off_screen as f32) + 0.5) as usize
}

fn rgb(color: u32) -> [f32; 4] {
    [
        ((color >> 16) & 0xFF) as f32 / 255.0,
        ((color >> 8) & 0xFF) as f32 / 255.0,
        (color & 0xFF) as f32 / 255.0,
        1.0,
    ]
}

fn present(stack: Option<&ItemStack>) -> Option<&ItemStackData> {
    stack.and_then(ItemStack::as_present)
}

/// A pattern's flag at a rectangle of the panel, in `tint`.
fn flag(
    elements: &mut Vec<MenuElement>,
    panel: &Panel,
    asset: &str,
    [x, y, w, h]: [f32; 4],
    tint: [f32; 4],
) {
    if let Some(sprite) = banner_pattern_sprite(asset) {
        elements.push(MenuElement::Image {
            x: panel.ox + x * panel.scale,
            y: panel.oy + y * panel.scale,
            w: w * panel.scale,
            h: h * panel.scale,
            sprite,
            tint,
        });
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build(
    elements: &mut Vec<MenuElement>,
    screen_w: f32,
    screen_h: f32,
    cursor: (f32, f32),
    input: &ContainerInput,
    slots: &[ItemStack],
    data: &[i16],
    title: &str,
    state: &mut LoomState,
    wheel: f32,
    cursor_item: &ItemStack,
    drag: &mut Option<DragState>,
    last_click: &mut Option<(u16, Instant)>,
    gs: f32,
) -> ContainerResult {
    let panel = push_panel(
        elements,
        screen_w,
        screen_h,
        gs,
        166.0,
        SpriteId::LoomBackground,
    );
    panel.label(elements, 8.0, 4.0, title);
    panel.label(elements, 8.0, 72.0, "Inventory");

    let (banner, dye, pattern, result) = (
        present(slots.first()),
        present(slots.get(1)),
        present(slots.get(2)),
        present(slots.get(3)),
    );
    // The pictures of what goes in each empty slot.
    for (stack, sprite, x, y) in [
        (banner, SpriteId::LoomSlotBanner, 13.0, 26.0),
        (dye, SpriteId::LoomSlotDye, 33.0, 26.0),
        (pattern, SpriteId::LoomSlotPattern, 23.0, 45.0),
    ] {
        if stack.is_none() {
            panel.image(elements, sprite, x, y, 16.0, 16.0);
        }
    }

    // `containerChanged`: the list starts over when what is in the loom
    // changes.
    let now: [ItemStack; 3] =
        std::array::from_fn(|slot| slots.get(slot).cloned().unwrap_or(ItemStack::Empty));
    if state.seen.as_ref() != Some(&now) {
        *state = LoomState {
            seen: Some(now),
            ..LoomState::default()
        };
    }
    let full = banner
        .and_then(|stack| stack.component_patch.get::<BannerPatterns>())
        .is_some_and(|patterns| patterns.patterns.len() >= MAX_PATTERNS);
    let patterns = if banner.is_some() && dye.is_some() && !full {
        crate::banner::selectable_patterns(pattern)
    } else {
        Vec::new()
    };
    let count = patterns.len();
    let active = total_rows(count) > ROWS;

    let mouse = (
        (cursor.0 - panel.ox) / panel.scale,
        (cursor.1 - panel.oy) / panel.scale,
    );
    if input.left_pressed && hit_test(mouse, [119.0, 9.0, 12.0, 56.0]) {
        state.dragging = true;
    }
    if !input.left_held {
        state.dragging = false;
    }
    if active {
        let off_screen = (total_rows(count) - ROWS) as f32;
        if state.dragging {
            state.scroll = ((mouse.1 - 13.0 - 7.5) / (56.0 - 15.0)).clamp(0.0, 1.0);
        } else if wheel != 0.0 {
            state.scroll = (state.scroll - wheel / off_screen).clamp(0.0, 1.0);
        }
    } else {
        state.scroll = 0.0;
    }
    panel.image(
        elements,
        if active {
            SpriteId::LoomScroller
        } else {
            SpriteId::LoomScrollerDisabled
        },
        119.0,
        13.0 + (41.0 * state.scroll).floor(),
        12.0,
        15.0,
    );

    // The banner as it would come out: its own colour, then each layer.
    if let (Some(made), Some(banner), false) = (result, banner, full) {
        let rect = [141.0, 8.0, 20.0, 40.0];
        let name = item_resource_name(banner.kind);
        let base = crate::banner::dye_rgb_named(name.strip_suffix("_banner").unwrap_or("white"));
        flag(elements, &panel, "base", rect, rgb(base));
        if let Some(layers) = made.component_patch.get::<BannerPatterns>() {
            for (asset, color) in crate::banner::preview_layers(layers) {
                flag(elements, &panel, &asset, rect, rgb(color));
            }
        }
    } else if full {
        // A banner that can take no more.
        panel.image(elements, SpriteId::LoomError, 138.0, 52.0, 26.0, 26.0);
    }

    // The patterns' buttons: each shows its pattern in white on a small
    // grey flag.
    let selected = data.first().copied().map_or(-1, i32::from);
    let first = start_row(state.scroll, count) * COLUMNS;
    let mut button = None;
    for (shown, (_, asset)) in patterns.iter().skip(first).take(COLUMNS * ROWS).enumerate() {
        let index = first + shown;
        let x = 60.0 + (shown % COLUMNS) as f32 * 14.0;
        let y = 13.0 + (shown / COLUMNS) as f32 * 14.0;
        let over = hit_test(mouse, [x, y, 14.0, 14.0]);
        let sprite = if index as i32 == selected {
            SpriteId::LoomPatternSelected
        } else if over {
            SpriteId::LoomPatternHighlighted
        } else {
            SpriteId::LoomPattern
        };
        panel.image(elements, sprite, x, y, 14.0, 14.0);
        let small = [x + 4.0, y + 2.0, 5.0, 10.0];
        elements.push(MenuElement::Rect {
            x: panel.ox + small[0] * panel.scale,
            y: panel.oy + small[1] * panel.scale,
            w: small[2] * panel.scale,
            h: small[3] * panel.scale,
            corner_radius: 0.0,
            color: rgb(crate::banner::dye_rgb_named("gray")),
        });
        flag(elements, &panel, asset, small, [1.0; 4]);
        if over && (input.left_pressed || input.right_pressed) {
            button = Some(index as u32);
        }
    }

    let mut built = build_slots(
        elements,
        &panel,
        cursor,
        input,
        ContainerKind::Loom,
        84.0,
        &|slot| match slot {
            0 => (13.0, 26.0),
            1 => (33.0, 26.0),
            2 => (23.0, 45.0),
            _ => (143.0, 57.0),
        },
        slots,
        cursor_item,
        drag,
        last_click,
    );
    built.button = button;
    built
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sixteen patterns fit; more scroll a row at a time to the last.
    #[test]
    fn the_patterns_scroll_by_rows() {
        assert_eq!(total_rows(16), 4);
        assert_eq!(total_rows(17), 5);
        assert_eq!(start_row(1.0, 16), 0);
        assert_eq!(start_row(0.0, 34), 0);
        assert_eq!(start_row(0.4, 34), 2);
        assert_eq!(start_row(1.0, 34), 5);
        assert_eq!(
            rgb(0x474F52),
            [71.0 / 255.0, 79.0 / 255.0, 82.0 / 255.0, 1.0]
        );
    }
}
