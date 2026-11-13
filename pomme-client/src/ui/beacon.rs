//! Vanilla `BeaconScreen`: the powers a beacon of its height offers, the
//! primary ones by tier down the left and the secondary to the right, the
//! payment slot, and the buttons that set the powers or leave them.

use std::time::Instant;

use azalea_inventory::{ItemStack, ItemStackData};
use azalea_registry::builtin::ItemKind;

use super::common::{FONT_SIZE, hit_test, push_item_icon, push_tooltip_lines};
use super::container::{
    ContainerInput, ContainerResult, DragState, Panel, SlotCtx, push_backdrop, push_cursor_stack,
    resolve_gesture,
};
use crate::player::menu_click::ContainerKind;
use crate::renderer::pipelines::menu_overlay::{MenuElement, SpriteId, TooltipLine};

/// The mob effects a beacon gives, by registry id.
const SPEED: u32 = 0;
const HASTE: u32 = 2;
const STRENGTH: u32 = 4;
const JUMP_BOOST: u32 = 7;
const REGENERATION: u32 = 9;
const RESISTANCE: u32 = 10;

/// `BeaconScreen.init`: the power buttons, as where each sits, its effect,
/// whether it sets the primary power, and the tier it needs. A tier's
/// buttons are centred about 76 (the secondary ones about 167), 24 apart.
const POWER_BUTTONS: [(f32, f32, u32, bool, i16); 6] = [
    (53.0, 22.0, SPEED, true, 0),
    (77.0, 22.0, HASTE, true, 0),
    (53.0, 47.0, RESISTANCE, true, 1),
    (77.0, 47.0, JUMP_BOOST, true, 1),
    (65.0, 72.0, STRENGTH, true, 2),
    (144.0, 47.0, REGENERATION, false, 3),
];
/// The button beside regeneration that makes the primary power stronger.
const UPGRADE_BUTTON: (f32, f32) = (168.0, 47.0);
const CONFIRM_BUTTON: (f32, f32) = (164.0, 107.0);
const CANCEL_BUTTON: (f32, f32) = (190.0, 107.0);

/// The powers picked on the screen, which the server's take the place of
/// whenever it names them anew.
#[derive(Default)]
pub struct BeaconState {
    primary: Option<u32>,
    secondary: Option<u32>,
    /// The menu's power data as last taken.
    seen: Option<(i16, i16)>,
}

/// What a click on the screen's buttons did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeaconAction {
    /// A power was picked; the screen stays.
    Picked,
    Confirm {
        primary: Option<u32>,
        secondary: Option<u32>,
    },
    Cancel,
}

/// `BeaconMenu.decodeEffect`: none, or the effect one less than the value.
fn decode_effect(value: i16) -> Option<u32> {
    u32::try_from(i32::from(value) - 1).ok()
}

/// An effect's name as it is shown: "jump_boost" as "Jump Boost".
fn effect_name(effect: u32) -> String {
    let name = crate::mob_effect::MOB_EFFECTS
        .get(effect as usize)
        .map_or("", |info| info.name);
    name.split('_')
        .map(|word| {
            let mut letters = word.chars();
            letters.next().map_or(String::new(), |first| {
                first.to_uppercase().collect::<String>() + letters.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Builds the screen; the second value is what a button's click did.
#[allow(clippy::too_many_arguments)]
pub fn build(
    elements: &mut Vec<MenuElement>,
    screen_w: f32,
    screen_h: f32,
    cursor: (f32, f32),
    input: &ContainerInput,
    slots: &[ItemStack],
    data: &[i16],
    state: &mut BeaconState,
    cursor_item: &ItemStack,
    drag: &mut Option<DragState>,
    last_click: &mut Option<(u16, Instant)>,
    gs: f32,
) -> (ContainerResult, Option<BeaconAction>) {
    let value = |index: usize| data.get(index).copied().unwrap_or(0);
    // The menu's powers, when the server sends them, are the screen's.
    let sent = (value(1), value(2));
    if state.seen != Some(sent) {
        state.seen = Some(sent);
        state.primary = decode_effect(sent.0);
        state.secondary = decode_effect(sent.1);
    }
    let levels = value(0);

    let panel = push_backdrop(elements, screen_w, screen_h, gs, 230.0, 219.0);
    let scale = panel.scale;
    panel.image(elements, SpriteId::BeaconBackground, 0.0, 0.0, 230.0, 219.0);
    for (x, text) in [(62.0, "Primary Power"), (169.0, "Secondary Power")] {
        elements.push(MenuElement::Text {
            x: panel.ox + x * scale,
            y: panel.oy + 10.0 * scale,
            text: text.into(),
            scale: FONT_SIZE * scale,
            // 0xE0E0E0.
            color: [224.0 / 255.0, 224.0 / 255.0, 224.0 / 255.0, 1.0],
            centered: true,
        });
    }
    // What a beacon takes, beside the payment slot.
    for (x, kind) in [
        (20.0, ItemKind::NetheriteIngot),
        (41.0, ItemKind::Emerald),
        (63.0, ItemKind::Diamond),
        (86.0, ItemKind::GoldIngot),
        (108.0, ItemKind::IronIngot),
    ] {
        push_item_icon(
            elements,
            panel.ox + x * scale,
            panel.oy + 109.0 * scale,
            16.0 * scale,
            scale,
            &ItemStackData::new(kind, 1),
        );
    }

    let mouse = ((cursor.0 - panel.ox) / scale, (cursor.1 - panel.oy) / scale);
    let clicked = input.left_pressed;
    let mut action = None;
    let mut tooltip = None;
    // A 22 by 22 button with an 18 by 18 picture on it
    // (`BeaconScreenButton`): shut, picked, under the cursor, or plain.
    let button = |elements: &mut Vec<MenuElement>,
                  (x, y): (f32, f32),
                  icon: SpriteId,
                  active: bool,
                  selected: bool|
     -> bool {
        let over = hit_test(mouse, [x, y, 22.0, 22.0]);
        let sprite = if !active {
            SpriteId::BeaconButtonDisabled
        } else if selected {
            SpriteId::BeaconButtonSelected
        } else if over {
            SpriteId::BeaconButtonHighlighted
        } else {
            SpriteId::BeaconButton
        };
        panel.image(elements, sprite, x, y, 22.0, 22.0);
        panel.image(elements, icon, x + 2.0, y + 2.0, 18.0, 18.0);
        over
    };

    for (x, y, effect, primary, tier) in POWER_BUTTONS {
        let picked = if primary {
            state.primary
        } else {
            state.secondary
        };
        let active = tier < levels;
        let selected = picked == Some(effect);
        if button(
            elements,
            (x, y),
            SpriteId::MobEffect(effect as u8),
            active,
            selected,
        ) {
            tooltip = Some(effect_name(effect));
            if clicked && active && !selected {
                if primary {
                    state.primary = Some(effect);
                } else {
                    state.secondary = Some(effect);
                }
                action = Some(BeaconAction::Picked);
            }
        }
    }
    // `BeaconUpgradePowerButton`: the primary power a level higher, once
    // there is one.
    if let Some(primary) = state.primary {
        let active = levels > 3;
        let selected = state.secondary == Some(primary);
        if button(
            elements,
            UPGRADE_BUTTON,
            SpriteId::MobEffect(primary as u8),
            active,
            selected,
        ) {
            tooltip = Some(format!("{} II", effect_name(primary)));
            if clicked && active && !selected {
                state.secondary = Some(primary);
                action = Some(BeaconAction::Picked);
            }
        }
    }
    // `BeaconConfirmButton`: with a payment in and a primary power picked.
    let paid = slots.first().is_some_and(ItemStack::is_present);
    let can_confirm = paid && state.primary.is_some();
    if button(
        elements,
        CONFIRM_BUTTON,
        SpriteId::BeaconConfirm,
        can_confirm,
        false,
    ) {
        tooltip = Some("Done".to_owned());
        if clicked && can_confirm {
            action = Some(BeaconAction::Confirm {
                primary: state.primary,
                secondary: state.secondary,
            });
        }
    }
    if button(elements, CANCEL_BUTTON, SpriteId::BeaconCancel, true, false) {
        tooltip = Some("Cancel".to_owned());
        if clicked {
            action = Some(BeaconAction::Cancel);
        }
    }

    // The payment slot and the inventory, which sits 28 further along than
    // most screens' (`BeaconMenu`).
    let slot_panel = Panel {
        scale,
        ox: panel.ox + 28.0 * scale,
        oy: panel.oy,
        w: 176.0 * scale,
        h: panel.h,
        screen: panel.screen,
        gs: panel.gs,
    };
    let kind = ContainerKind::Beacon;
    let mut ctx = SlotCtx::new(
        elements,
        &slot_panel,
        cursor,
        kind,
        slots,
        cursor_item,
        drag,
    );
    ctx.player_rows(slots, 1, 28, 137.0);
    ctx.slot(
        108.0,
        110.0,
        slots.first().unwrap_or(&ItemStack::Empty),
        None,
        0,
    );
    let (hovered, shown_cursor) = ctx.finish(cursor_item);
    if let Some(text) = tooltip {
        push_tooltip_lines(
            elements,
            cursor,
            panel.screen.0,
            panel.screen.1,
            panel.gs,
            vec![TooltipLine::new(text, [1.0; 4])],
        );
    }
    push_cursor_stack(elements, cursor, scale, &shown_cursor);

    let (ops, clicked_outside) = resolve_gesture(
        input,
        hovered,
        &panel,
        cursor,
        kind,
        slots,
        cursor_item,
        drag,
        last_click,
    );
    (
        ContainerResult {
            clicked_outside,
            ops,
            button: None,
            hovered,
        },
        action,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The menu's power values are an effect's id and one, zero for none,
    /// and the effects read as their names.
    #[test]
    fn powers_decode_and_are_named() {
        assert_eq!(decode_effect(0), None);
        assert_eq!(decode_effect(1), Some(SPEED));
        assert_eq!(decode_effect(11), Some(RESISTANCE));
        assert_eq!(effect_name(SPEED), "Speed");
        assert_eq!(effect_name(HASTE), "Haste");
        assert_eq!(effect_name(JUMP_BOOST), "Jump Boost");
        assert_eq!(effect_name(REGENERATION), "Regeneration");
        assert_eq!(effect_name(STRENGTH), "Strength");
        assert_eq!(effect_name(RESISTANCE), "Resistance");
    }
}
