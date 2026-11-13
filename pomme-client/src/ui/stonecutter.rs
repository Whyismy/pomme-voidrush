//! Vanilla `StonecutterScreen`: the input, a grid of what it can be cut
//! into (four across, three rows showing, a scroller for the rest) and the
//! result. Picking one is a menu button click; the server makes the result.

use std::time::Instant;

use azalea_inventory::ItemStack;
use azalea_registry::builtin::ItemKind;

use super::chest::build_slots;
use super::common::{hit_test, push_item_icon, push_tooltip_lines};
use super::container::{ContainerInput, ContainerResult, DragState, push_panel};
use crate::player::menu_click::ContainerKind;
use crate::renderer::pipelines::menu_overlay::{MenuElement, SpriteId};

/// The grid: four across, three rows on screen.
const COLUMNS: i32 = 4;
const ROWS: i32 = 3;
const SHOWN: i32 = COLUMNS * ROWS;

/// Where a menu's own list is scrolled, and what was last picked from it.
#[derive(Default)]
pub struct ListState {
    /// Through the rows off screen, 0 at the top to 1.
    pub scroll: f32,
    /// The scroller is held.
    dragging: bool,
    /// What the list was last built for; another input starts it over
    /// (`StonecutterScreen.containerChanged`).
    input: Option<ItemKind>,
    /// The option picked this visit, ahead of the server's word of it.
    picked: Option<i16>,
}

/// The rows of `count` options that do not fit on screen.
fn offscreen_rows(count: i32) -> i32 {
    (count + COLUMNS - 1) / COLUMNS - ROWS
}

/// The first option on screen at `scroll` through `count` of them.
fn start_index(scroll: f32, count: i32) -> i32 {
    if count <= SHOWN {
        return 0;
    }
    (f64::from(scroll * offscreen_rows(count) as f32) + 0.5) as i32 * COLUMNS
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
    options: &[ItemStack],
    state: &mut ListState,
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
        SpriteId::StonecutterBackground,
    );
    panel.label(elements, 8.0, 5.0, title);
    panel.label(elements, 8.0, 72.0, "Inventory");

    let input_kind = match slots.first() {
        Some(ItemStack::Present(stack)) => Some(stack.kind),
        _ => None,
    };
    if state.input != input_kind {
        *state = ListState {
            input: input_kind,
            ..ListState::default()
        };
    }
    let count = options.len() as i32;
    let active = count > SHOWN;
    // The cursor in the panel's own units.
    let mouse = (
        (cursor.0 - panel.ox) / panel.scale,
        (cursor.1 - panel.oy) / panel.scale,
    );
    if input.left_pressed && hit_test(mouse, [119.0, 9.0, 12.0, 54.0]) {
        state.dragging = true;
    }
    if !input.left_held {
        state.dragging = false;
    }
    if active {
        if state.dragging {
            state.scroll = ((mouse.1 - 14.0 - 7.5) / (54.0 - 15.0)).clamp(0.0, 1.0);
        } else if wheel != 0.0 {
            state.scroll = (state.scroll - wheel / offscreen_rows(count) as f32).clamp(0.0, 1.0);
        }
    } else {
        state.scroll = 0.0;
    }
    let start = start_index(state.scroll, count);

    panel.image(
        elements,
        if active {
            SpriteId::StonecutterScroller
        } else {
            SpriteId::StonecutterScrollerDisabled
        },
        119.0,
        15.0 + (41.0 * state.scroll).floor(),
        12.0,
        15.0,
    );

    let selected = i32::from(state.picked.unwrap_or(data.first().copied().unwrap_or(-1)));
    let mut button = None;
    let mut hovered_option = None;
    for index in start..(start + SHOWN).min(count) {
        let shown = index - start;
        let x = 52.0 + (shown % COLUMNS) as f32 * 16.0;
        let row_y = 14.0 + (shown / COLUMNS) as f32 * 18.0;
        let over = hit_test(mouse, [x, row_y + 2.0, 16.0, 18.0]);
        let sprite = if index == selected {
            SpriteId::StonecutterRecipeSelected
        } else if over {
            SpriteId::StonecutterRecipeHighlighted
        } else {
            SpriteId::StonecutterRecipe
        };
        panel.image(elements, sprite, x, row_y + 1.0, 16.0, 18.0);
        let option = &options[index as usize];
        // The item alone, without its count (`GuiGraphicsExtractor.item`).
        if let ItemStack::Present(stack) = option {
            let mut single = stack.clone();
            single.count = 1;
            push_item_icon(
                elements,
                panel.ox + x * panel.scale,
                panel.oy + (row_y + 2.0) * panel.scale,
                16.0 * panel.scale,
                panel.scale,
                &single,
            );
        }
        if over {
            hovered_option = Some(option);
        }
        // `mouseClicked`: any button, on the row's own 16 by 18, and
        // `clickMenuButton` turns down the one already picked.
        if (input.left_pressed || input.right_pressed)
            && hit_test(mouse, [x, row_y, 16.0, 18.0])
            && index != selected
        {
            button = Some(index as u32);
            state.picked = Some(index as i16);
        }
    }

    let mut result = build_slots(
        elements,
        &panel,
        cursor,
        input,
        ContainerKind::Stonecutter,
        84.0,
        &|slot| {
            if slot == 0 {
                (20.0, 33.0)
            } else {
                (143.0, 33.0)
            }
        },
        slots,
        cursor_item,
        drag,
        last_click,
    );
    result.button = button;

    if cursor_item.is_empty()
        && let Some(option) = hovered_option
        && let Some(value) = crate::ui::chat::stack_tooltip_value(option)
    {
        let lines = crate::ui::chat::item_tooltip_lines(&value, None, false);
        if !lines.is_empty() {
            push_tooltip_lines(elements, cursor, screen_w, screen_h, gs, lines);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Twelve options fit; more scroll a row of four at a time, to the last
    /// row at the bottom.
    #[test]
    fn the_list_scrolls_by_rows() {
        assert_eq!(start_index(1.0, 12), 0);
        assert_eq!(offscreen_rows(13), 1);
        assert_eq!(offscreen_rows(21), 3);
        assert_eq!(start_index(0.0, 21), 0);
        assert_eq!(start_index(0.4, 21), 4);
        assert_eq!(start_index(0.5, 21), 8);
        assert_eq!(start_index(1.0, 21), 12);
    }
}
