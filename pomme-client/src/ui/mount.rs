//! Vanilla `HorseInventoryScreen` and `NautilusInventoryScreen`: a mount's
//! saddle and body armor, where it has the use of them, and beside them
//! three rows of its chest's columns.
// TODO: the mount itself, drawn in the frame beside its slots.

use std::time::Instant;

use azalea_inventory::ItemStack;
use azalea_registry::builtin::EntityKind;

use super::chest::build_slots;
use super::common::SLOT_STRIDE;
use super::container::{
    ContainerInput, ContainerResult, DragState, push_clipped_sprite, push_panel,
};
use crate::player::menu_click::{ContainerKind, MountArmor};
use crate::renderer::pipelines::menu_overlay::{MenuElement, SpriteId};

/// Where a slot this mount has no use of is put: nowhere on screen.
const NOWHERE: (f32, f32) = (-10_000.0, -10_000.0);

/// What a mount of `kind` has in its inventory screen: whether it takes a
/// saddle (`can_equip_saddle`), what it wears, and whether the screen is a
/// nautilus's. `None` for what has no such screen.
pub fn slots_of(kind: EntityKind) -> Option<(bool, MountArmor, bool)> {
    Some(match kind {
        // `can_wear_horse_armor`.
        EntityKind::Horse | EntityKind::ZombieHorse => (true, MountArmor::Horse, false),
        EntityKind::SkeletonHorse
        | EntityKind::Donkey
        | EntityKind::Mule
        | EntityKind::Camel
        | EntityKind::CamelHusk => (true, MountArmor::None, false),
        EntityKind::Llama | EntityKind::TraderLlama => (false, MountArmor::Llama, false),
        EntityKind::Nautilus | EntityKind::ZombieNautilus => (true, MountArmor::Nautilus, true),
        _ => return None,
    })
}

/// A kind's name as a title: "SkeletonHorse" as "Skeleton Horse".
pub fn kind_name(kind: EntityKind) -> String {
    let mut name = String::new();
    for letter in format!("{kind:?}").chars() {
        if letter.is_uppercase() && !name.is_empty() {
            name.push(' ');
        }
        name.push(letter);
    }
    name
}

/// `shape` is the mount's columns, whether it takes a saddle, what it
/// wears, and whether it is a nautilus.
#[allow(clippy::too_many_arguments)]
pub fn build(
    elements: &mut Vec<MenuElement>,
    screen_w: f32,
    screen_h: f32,
    cursor: (f32, f32),
    input: &ContainerInput,
    slots: &[ItemStack],
    title: &str,
    shape: (u8, bool, MountArmor, bool),
    cursor_item: &ItemStack,
    drag: &mut Option<DragState>,
    last_click: &mut Option<(u16, Instant)>,
    gs: f32,
) -> ContainerResult {
    let (columns, saddle, armor, nautilus) = shape;
    let panel = push_panel(
        elements,
        screen_w,
        screen_h,
        gs,
        166.0,
        if nautilus {
            SpriteId::NautilusBackground
        } else {
            SpriteId::HorseBackground
        },
    );
    panel.label(elements, 8.0, 6.0, title);
    panel.label(elements, 8.0, 72.0, "Inventory");

    // The chest's slots, as many columns of the sheet's five as it has.
    if columns > 0 {
        push_clipped_sprite(
            elements,
            &panel,
            SpriteId::HorseChestSlots,
            [79.0, 17.0, 90.0, 54.0],
            [79.0, 17.0, f32::from(columns) * SLOT_STRIDE, 54.0],
        );
    }
    // The saddle's and the armor's slots, each with the picture of what
    // goes in it while it is empty.
    let empty = |slot: usize| slots.get(slot).is_none_or(ItemStack::is_empty);
    if saddle {
        panel.image(elements, SpriteId::MountSlot, 7.0, 17.0, 18.0, 18.0);
        if empty(0) {
            panel.image(elements, SpriteId::MountSlotSaddle, 8.0, 18.0, 16.0, 16.0);
        }
    }
    let armor_icon = match armor {
        MountArmor::None => None,
        MountArmor::Horse => Some(SpriteId::MountSlotHorseArmor),
        MountArmor::Llama => Some(SpriteId::MountSlotLlamaArmor),
        MountArmor::Nautilus => Some(SpriteId::MountSlotNautilusArmor),
    };
    if let Some(icon) = armor_icon {
        panel.image(elements, SpriteId::MountSlot, 7.0, 35.0, 18.0, 18.0);
        if empty(1) {
            panel.image(elements, icon, 8.0, 36.0, 16.0, 16.0);
        }
    }

    build_slots(
        elements,
        &panel,
        cursor,
        input,
        ContainerKind::Mount {
            columns,
            saddle,
            armor,
        },
        84.0,
        &|slot| match slot {
            0 if saddle => (8.0, 18.0),
            1 if armor != MountArmor::None => (8.0, 36.0),
            0 | 1 => NOWHERE,
            _ => {
                let chest = slot - 2;
                let across = u16::from(columns.max(1));
                (
                    80.0 + f32::from(chest % across) * SLOT_STRIDE,
                    18.0 + f32::from(chest / across) * SLOT_STRIDE,
                )
            }
        },
        slots,
        cursor_item,
        drag,
        last_click,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each mount has the slots its kind has the use of, and what is no
    /// mount has no screen.
    #[test]
    fn mounts_have_their_own_slots() {
        assert_eq!(
            slots_of(EntityKind::Horse),
            Some((true, MountArmor::Horse, false))
        );
        assert_eq!(
            slots_of(EntityKind::Donkey),
            Some((true, MountArmor::None, false))
        );
        assert_eq!(
            slots_of(EntityKind::Llama),
            Some((false, MountArmor::Llama, false))
        );
        assert_eq!(
            slots_of(EntityKind::Nautilus),
            Some((true, MountArmor::Nautilus, true))
        );
        assert_eq!(slots_of(EntityKind::Pig), None);
        assert_eq!(kind_name(EntityKind::SkeletonHorse), "Skeleton Horse");
        assert_eq!(kind_name(EntityKind::Donkey), "Donkey");

        // A donkey with its chest: two slots, fifteen of the chest, then the
        // inventory.
        let donkey = ContainerKind::Mount {
            columns: 5,
            saddle: true,
            armor: MountArmor::None,
        };
        assert_eq!((donkey.inv_start(), donkey.slot_count()), (17, 53));
    }
}
