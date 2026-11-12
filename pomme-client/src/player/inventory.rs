use azalea_inventory::components::EquipmentSlot;
use azalea_inventory::{ItemStack, ItemStackData};
use azalea_registry::builtin::ItemKind;

pub const PLAYER_SLOTS: usize = 46;
pub const HOTBAR_START: usize = 36;
const HOTBAR_END: usize = 45;
pub const MAIN_START: usize = 9;
const MAIN_END: usize = 36;
pub const ARMOR_START: usize = 5;
const ARMOR_END: usize = 9;
/// The equipment the armor slots hold, in slot order.
pub const ARMOR_EQUIPMENT: [EquipmentSlot; 4] = [
    EquipmentSlot::Head,
    EquipmentSlot::Chest,
    EquipmentSlot::Legs,
    EquipmentSlot::Feet,
];
pub const CRAFT_INPUT_START: usize = 1;
pub const CRAFT_OUTPUT: usize = 0;
pub const OFFHAND: usize = 45;

/// A vanilla `Inventory` index (hotbar 0-8, main 9-35, armor feet to head
/// 36-39, offhand 40) as its `InventoryMenu` slot.
pub fn menu_slot_for_inventory_index(index: u32) -> Option<usize> {
    let index = index as usize;
    Some(match index {
        0..=8 => HOTBAR_START + index,
        9..=35 => index,
        36..=39 => ARMOR_START + (39 - index),
        40 => OFFHAND,
        _ => return None,
    })
}

pub struct Inventory {
    slots: Vec<ItemStack>,
}

impl Inventory {
    pub fn new() -> Self {
        Self {
            slots: vec![ItemStack::Empty; PLAYER_SLOTS],
        }
    }

    pub fn set_contents(&mut self, items: Vec<ItemStack>) {
        self.slots = items;
        self.slots.resize(PLAYER_SLOTS, ItemStack::Empty);
    }

    pub fn set_slot(&mut self, index: usize, item: ItemStack) {
        if index < self.slots.len() {
            self.slots[index] = item;
        }
    }

    pub fn slot(&self, index: usize) -> &ItemStack {
        self.slots.get(index).unwrap_or(&ItemStack::Empty)
    }

    pub fn slots(&self) -> &[ItemStack] {
        &self.slots
    }

    pub fn main_slots(&self) -> &[ItemStack] {
        &self.slots[MAIN_START..MAIN_END]
    }

    pub fn hotbar_slots(&self) -> &[ItemStack] {
        &self.slots[HOTBAR_START..HOTBAR_END]
    }

    pub fn armor_slots(&self) -> &[ItemStack] {
        &self.slots[ARMOR_START..ARMOR_END]
    }

    pub fn craft_output(&self) -> &ItemStack {
        self.slot(CRAFT_OUTPUT)
    }

    /// The `canEntityWalkOnPowderSnow` check: leather boots in the feet slot.
    pub fn wears_leather_boots(&self) -> bool {
        matches!(
            self.slot(ARMOR_END - 1),
            ItemStack::Present(data) if data.kind == ItemKind::LeatherBoots
        )
    }

    pub fn offhand(&self) -> &ItemStack {
        self.slot(OFFHAND)
    }

    /// The non-empty stack in the selected hotbar slot.
    pub fn held_stack(&self, selected: u8) -> Option<&ItemStackData> {
        self.hotbar_slots()
            .get(selected as usize)
            .and_then(non_empty)
    }

    /// The non-empty main-hand then offhand stacks.
    pub fn hand_stacks(&self, selected: u8) -> [Option<&ItemStackData>; 2] {
        [self.held_stack(selected), non_empty(self.offhand())]
    }

    /// The non-empty stack the player has in `slot`; a player's body and
    /// saddle slots are always empty.
    pub fn equipped(&self, slot: EquipmentSlot, selected: u8) -> Option<&ItemStackData> {
        let [main, off] = self.hand_stacks(selected);
        match slot {
            EquipmentSlot::Mainhand => main,
            EquipmentSlot::Offhand => off,
            EquipmentSlot::Body | EquipmentSlot::Saddle => None,
            armor => ARMOR_EQUIPMENT
                .iter()
                .position(|&s| s == armor)
                .and_then(|i| non_empty(&self.armor_slots()[i])),
        }
    }

    /// Remove one item (or the whole stack) from the selected hotbar slot,
    /// vanilla `Inventory.removeFromSelected`. Returns whether anything was
    /// removed; the server spawns the dropped item entity.
    pub fn remove_from_selected(&mut self, selected: u8, whole_stack: bool) -> bool {
        let index = HOTBAR_START + (selected as usize).min(8);
        match &mut self.slots[index] {
            ItemStack::Present(data) if data.count > 0 => {
                if whole_stack || data.count == 1 {
                    self.slots[index] = ItemStack::Empty;
                } else {
                    data.count -= 1;
                }
                true
            }
            _ => false,
        }
    }
}

fn non_empty(stack: &ItemStack) -> Option<&ItemStackData> {
    match stack {
        ItemStack::Present(data) if data.count > 0 => Some(data),
        _ => None,
    }
}

/// Vanilla `Item.canFitInsideContainerItems`: false only for shulker boxes.
pub fn can_fit_inside_container_items(kind: ItemKind) -> bool {
    !azalea_registry::tags::items::SHULKER_BOXES.contains(&kind)
}

/// Vanilla `ItemStack.hasFoil`: the stack's `enchantment_glint_override`
/// where it has one (its item's own included, as an enchanted golden
/// apple's), else whether it is enchanted.
pub fn has_foil(stack: &ItemStackData) -> bool {
    use azalea_inventory::components::{EnchantmentGlintOverride, Enchantments};
    match stack.get_component::<EnchantmentGlintOverride>() {
        Some(glint) => glint.show_glint,
        None => stack
            .get_component::<Enchantments>()
            .is_some_and(|enchantments| !enchantments.levels.is_empty()),
    }
}

/// Vanilla `LivingEntity.canGlideUsing`: a glider (an elytra) worn in its
/// own slot, with more than one use left in it.
pub fn can_glide_using(stack: &ItemStackData, slot: EquipmentSlot) -> bool {
    use azalea_inventory::components::{Damage, Equippable, Glider, MaxDamage, Unbreakable};
    if stack.get_component::<Glider>().is_none()
        || stack
            .get_component::<Equippable>()
            .is_none_or(|equippable| equippable.slot != slot)
    {
        return false;
    }
    // `ItemStack.nextDamageWillBreak`.
    let unbreakable = stack.get_component::<Unbreakable>().is_some();
    let worn_out = match (
        stack.get_component::<MaxDamage>(),
        stack.get_component::<Damage>(),
    ) {
        (Some(max), Some(damage)) => !unbreakable && damage.amount >= max.amount - 1,
        _ => false,
    };
    !worn_out
}

pub fn item_resource_name(kind: ItemKind) -> String {
    kind.to_string()
        .strip_prefix("minecraft:")
        .unwrap_or("air")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An elytra glides from the chest slot only, and not on its last use.
    #[test]
    fn an_elytra_glides_from_the_chest_until_its_last_use() {
        use azalea_inventory::components::Damage;
        let glides = |stack: ItemStack, slot| can_glide_using(stack.as_present().unwrap(), slot);
        let elytra = || ItemStack::new(ItemKind::Elytra, 1);
        assert!(glides(elytra(), EquipmentSlot::Chest));
        assert!(!glides(elytra(), EquipmentSlot::Head));
        assert!(!glides(
            ItemStack::new(ItemKind::DiamondChestplate, 1),
            EquipmentSlot::Chest
        ));
        // 432 uses: the 431st point of damage leaves the one it won't spend.
        assert!(glides(
            elytra().with_component(Damage { amount: 430 }),
            EquipmentSlot::Chest
        ));
        assert!(!glides(
            elytra().with_component(Damage { amount: 431 }),
            EquipmentSlot::Chest
        ));
    }

    /// A stack glints by its own `enchantment_glint_override`, else by its
    /// item's (an enchanted golden apple's), else not at all unenchanted.
    #[test]
    fn a_stack_glints_by_its_override_or_its_item() {
        use azalea_inventory::components::EnchantmentGlintOverride;
        let foil = |stack: ItemStack| has_foil(stack.as_present().unwrap());
        assert!(!foil(ItemStack::new(ItemKind::DiamondSword, 1)));
        assert!(foil(ItemStack::new(ItemKind::EnchantedGoldenApple, 1)));
        assert!(foil(ItemStack::new(ItemKind::ExperienceBottle, 1)));
        assert!(foil(
            ItemStack::new(ItemKind::DiamondSword, 1)
                .with_component(EnchantmentGlintOverride { show_glint: true })
        ));
        assert!(!foil(
            ItemStack::new(ItemKind::EnchantedGoldenApple, 1)
                .with_component(EnchantmentGlintOverride { show_glint: false })
        ));
    }

    #[test]
    fn inventory_indices_map_to_inventory_menu_slots() {
        // Vanilla `InventoryMenu`: armor head..feet at 5..8, hotbar last.
        let mapped = |i| menu_slot_for_inventory_index(i);
        assert_eq!(mapped(0), Some(HOTBAR_START));
        assert_eq!(mapped(8), Some(HOTBAR_END - 1));
        assert_eq!(mapped(9), Some(MAIN_START));
        assert_eq!(mapped(35), Some(MAIN_END - 1));
        assert_eq!(mapped(36), Some(ARMOR_END - 1));
        assert_eq!(mapped(39), Some(ARMOR_START));
        assert_eq!(mapped(40), Some(OFFHAND));
        assert_eq!(mapped(41), None);
    }
}
