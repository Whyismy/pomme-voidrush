//! How far up each first-person hand holds its item (vanilla
//! `ItemInHandRenderer.tick`): a hand drops out of view when its stack
//! changes, swaps to the new one at the bottom and comes back up, and the
//! main hand also sits low after an attack until the swing recharges.

use azalea_inventory::ItemStack;

/// The most a hand's height moves in a tick.
const STEP: f32 = 0.4;

/// A hand low enough to swap its item unseen.
const SWAP_BELOW: f32 = 0.1;

#[derive(Default)]
pub struct HandEquip {
    /// The main hand, then the off hand.
    hands: [Hand; 2],
}

struct Hand {
    height: f32,
    prev_height: f32,
    /// The stack the hand still shows, which lags a change of the real one
    /// until the hand is down.
    visible: ItemStack,
}

impl Default for Hand {
    fn default() -> Self {
        Self {
            height: 0.0,
            prev_height: 0.0,
            visible: ItemStack::Empty,
        }
    }
}

/// Whether showing `next` in place of `visible` takes no animation. Vanilla
/// compares the stack objects: one the server replaced with an equal one is
/// the same (`ItemStack.matches`), and so is one the client itself counted
/// down (placing, eating, throwing), which it does in place. Anything else,
/// such as another item, a picked-up count or new damage, is a change.
fn replaces_unseen(visible: &ItemStack, next: &ItemStack) -> bool {
    match (visible.as_present(), next.as_present()) {
        (None, None) => true,
        (Some(visible), Some(next)) => {
            visible.is_same_item_and_components(next) && next.count <= visible.count
        }
        _ => false,
    }
}

impl HandEquip {
    /// One tick. `swap_scale` is vanilla `Player.getItemSwapScale(1)`: how
    /// far the main hand's swing has recharged, 0 to 1.
    pub fn tick(&mut self, stacks: [&ItemStack; 2], swap_scale: f32) {
        // TODO: `isHandsBusy` (rowing a boat) lowers both hands.
        let targets = [swap_scale * swap_scale * swap_scale, 1.0];
        for ((hand, next), target) in self.hands.iter_mut().zip(stacks).zip(targets) {
            hand.prev_height = hand.height;
            let changed = !replaces_unseen(&hand.visible, next);
            if !changed {
                hand.visible = next.clone();
            }
            let target = if changed { 0.0 } else { target };
            hand.height += (target - hand.height).clamp(-STEP, STEP);
            if hand.height < SWAP_BELOW {
                hand.visible = next.clone();
            }
        }
    }

    /// The stacks the hands show, main then off.
    pub fn visible(&self) -> [&ItemStack; 2] {
        [&self.hands[0].visible, &self.hands[1].visible]
    }

    /// Vanilla's `inverseArmHeight` for each hand, `partial_tick` into the
    /// tick: 0 fully up, 1 down out of view.
    pub fn lowered(&self, partial_tick: f32) -> [f32; 2] {
        [0, 1].map(|i| {
            let hand = &self.hands[i];
            1.0 - (hand.prev_height + (hand.height - hand.prev_height) * partial_tick)
        })
    }
}

#[cfg(test)]
mod tests {
    use azalea_inventory::ItemStackData;
    use azalea_registry::builtin::ItemKind;

    use super::*;

    fn stack(kind: ItemKind, count: i32) -> ItemStack {
        ItemStack::Present(ItemStackData {
            kind,
            count,
            component_patch: Default::default(),
        })
    }

    fn kind(stack: &ItemStack) -> Option<ItemKind> {
        stack.as_present().map(|data| data.kind)
    }

    /// A new stack drops the hand, swaps at the bottom and rises again.
    #[test]
    fn a_changed_item_dips_and_swaps_at_the_bottom() {
        let mut equip = HandEquip::default();
        let (sword, pick) = (
            stack(ItemKind::IronSword, 1),
            stack(ItemKind::IronPickaxe, 1),
        );
        let empty = ItemStack::Empty;
        for _ in 0..5 {
            equip.tick([&sword, &empty], 1.0);
        }
        assert_eq!(equip.lowered(1.0), [0.0, 0.0]);
        assert_eq!(kind(equip.visible()[0]), Some(ItemKind::IronSword));

        // Down 0.4 a tick: the sword still shows until the hand is under 0.1.
        equip.tick([&pick, &empty], 1.0);
        assert!((equip.lowered(1.0)[0] - 0.4).abs() < 1e-6);
        assert!((equip.lowered(0.5)[0] - 0.2).abs() < 1e-6);
        assert_eq!(kind(equip.visible()[0]), Some(ItemKind::IronSword));
        equip.tick([&pick, &empty], 1.0);
        assert_eq!(kind(equip.visible()[0]), Some(ItemKind::IronSword));
        equip.tick([&pick, &empty], 1.0);
        assert_eq!(equip.lowered(1.0)[0], 1.0);
        assert_eq!(kind(equip.visible()[0]), Some(ItemKind::IronPickaxe));
        for _ in 0..3 {
            equip.tick([&pick, &empty], 1.0);
        }
        assert_eq!(equip.lowered(1.0)[0], 0.0);
    }

    /// The main hand follows the swing's recharge cubed; the off hand
    /// doesn't. A stack the client counts down stays up; one that grows
    /// (picked up) dips.
    #[test]
    fn the_main_hand_follows_the_swing_and_counts_stay_up() {
        let mut equip = HandEquip::default();
        let blocks = stack(ItemKind::Stone, 8);
        let shield = stack(ItemKind::Shield, 1);
        for _ in 0..5 {
            equip.tick([&blocks, &shield], 1.0);
        }
        equip.tick([&blocks, &shield], 0.5);
        // Toward 0.125, 0.4 a tick.
        assert!((equip.lowered(1.0)[0] - 0.4).abs() < 1e-6);
        assert_eq!(equip.lowered(1.0)[1], 0.0);
        for _ in 0..5 {
            equip.tick([&blocks, &shield], 0.5);
        }
        assert!((equip.lowered(1.0)[0] - 0.875).abs() < 1e-6);
        for _ in 0..5 {
            equip.tick([&blocks, &shield], 1.0);
        }

        let placed = stack(ItemKind::Stone, 7);
        equip.tick([&placed, &shield], 1.0);
        assert_eq!(equip.lowered(1.0)[0], 0.0);
        assert_eq!(equip.visible()[0].count(), 7);
        let picked_up = stack(ItemKind::Stone, 9);
        equip.tick([&picked_up, &shield], 1.0);
        assert!((equip.lowered(1.0)[0] - 0.4).abs() < 1e-6);
    }
}
