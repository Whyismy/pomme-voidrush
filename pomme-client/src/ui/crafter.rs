//! Vanilla `CrafterScreen`: the crafter's nine slots, any of which a click
//! on it empty shuts or opens again, its redstone arrow and what it would
//! make, which is only shown.

use std::time::Instant;

use azalea_inventory::ItemStack;

use super::chest::build_slots;
use super::common::{FONT_SIZE, SLOT_STRIDE, push_item_icon, push_tooltip_lines};
use super::container::{ContainerInput, ContainerResult, DragState, push_panel};
use crate::player::menu_click::ContainerKind;
use crate::renderer::pipelines::menu_overlay::{MenuElement, SpriteId, TooltipLine};

/// The slot the result is shown from, after the crafter's nine and the
/// inventory's thirty-six.
const RESULT_SLOT: usize = 45;

/// Where a crafter slot sits, in GUI units.
fn slot_at(slot: u16) -> (f32, f32) {
    (
        26.0 + f32::from(slot % 3) * SLOT_STRIDE,
        17.0 + f32::from(slot / 3) * SLOT_STRIDE,
    )
}

/// `CrafterScreen.slotClicked`: what a click, or a hotbar key, on the empty
/// crafter slot `disabled` does to its state before it acts as a click:
/// open it again, or with nothing carried shut it.
fn toggle(disabled: bool, clicked: bool, carrying: bool, swapping_in: bool) -> Option<bool> {
    if clicked {
        if disabled {
            Some(true)
        } else {
            (!carrying).then_some(false)
        }
    } else {
        (swapping_in && disabled).then_some(true)
    }
}

/// Builds the screen; the second value is a slot whose state the player
/// set this frame, and what to.
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
    spectator: bool,
    cursor_item: &ItemStack,
    drag: &mut Option<DragState>,
    last_click: &mut Option<(u16, Instant)>,
    gs: f32,
    text_width: &dyn Fn(&str, f32) -> f32,
) -> (ContainerResult, Option<(u32, bool)>) {
    let panel = push_panel(
        elements,
        screen_w,
        screen_h,
        gs,
        166.0,
        SpriteId::CrafterBackground,
    );
    let title_w = text_width(title, FONT_SIZE * panel.scale) / panel.scale;
    panel.label(elements, ((176.0 - title_w) / 2.0).floor(), 6.0, title);
    panel.label(elements, 8.0, 72.0, "Inventory");

    let disabled = |slot: u16| data.get(usize::from(slot)).copied() == Some(1);
    let powered = data.get(9).copied() == Some(1);
    panel.image(
        elements,
        if powered {
            SpriteId::CrafterPowered
        } else {
            SpriteId::CrafterUnpowered
        },
        97.0,
        35.0,
        16.0,
        16.0,
    );
    for slot in (0..9).filter(|slot| disabled(*slot)) {
        let (x, y) = slot_at(slot);
        panel.image(
            elements,
            SpriteId::CrafterDisabledSlot,
            x - 1.0,
            y - 1.0,
            18.0,
            18.0,
        );
    }
    if let Some(ItemStack::Present(made)) = slots.get(RESULT_SLOT) {
        push_item_icon(
            elements,
            panel.ox + 134.0 * panel.scale,
            panel.oy + 35.0 * panel.scale,
            16.0 * panel.scale,
            panel.scale,
            made,
        );
    }

    let result = build_slots(
        elements,
        &panel,
        cursor,
        input,
        ContainerKind::Crafter,
        84.0,
        &slot_at,
        slots,
        cursor_item,
        drag,
        last_click,
    );

    let mut toggled = None;
    if !spectator
        && let Some(slot) = result.hovered.filter(|slot| *slot < 9)
        && slots
            .get(usize::from(slot))
            .is_some_and(ItemStack::is_empty)
    {
        // A hotbar key that would bring a stack in opens the slot for it.
        let swapping_in = input.hotbar_swap.is_some_and(|key| {
            slots
                .get(36 + usize::from(key))
                .is_some_and(ItemStack::is_present)
        });
        let clicked = (input.left_pressed || input.right_pressed) && !input.shift;
        toggled = toggle(
            disabled(slot),
            clicked,
            cursor_item.is_present(),
            swapping_in,
        )
        .map(|enabled| (u32::from(slot), enabled));
        if !disabled(slot) && cursor_item.is_empty() {
            push_tooltip_lines(
                elements,
                cursor,
                screen_w,
                screen_h,
                gs,
                vec![TooltipLine::new("Click to disable slot".into(), [1.0; 4])],
            );
        }
    }
    (result, toggled)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A click opens a shut slot whatever is carried, shuts an open one
    /// only with nothing carried, and a hotbar key only ever opens.
    #[test]
    fn a_click_on_an_empty_slot_sets_its_state() {
        assert_eq!(toggle(true, true, false, false), Some(true));
        assert_eq!(toggle(true, true, true, false), Some(true));
        assert_eq!(toggle(false, true, false, false), Some(false));
        assert_eq!(toggle(false, true, true, false), None);
        assert_eq!(toggle(true, false, false, true), Some(true));
        assert_eq!(toggle(false, false, false, true), None);
        assert_eq!(toggle(true, false, false, false), None);
        assert_eq!(slot_at(4), (44.0, 35.0));
        assert_eq!(slot_at(8), (62.0, 53.0));
    }
}
