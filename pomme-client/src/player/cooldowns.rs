//! Item cooldowns (vanilla `ItemCooldowns`): the server starts one for a
//! group of items, and until it runs out those items show a rising white
//! cover in their slots and their use isn't predicted.

use std::collections::HashMap;

use azalea_inventory::ItemStackData;
use azalea_inventory::components::UseCooldown;

use crate::player::inventory::item_resource_name;

/// `ItemCooldowns.CooldownInstance`, in the tracker's own ticks.
#[derive(Clone, Copy)]
struct Cooldown {
    start: i32,
    end: i32,
}

#[derive(Default)]
pub struct ItemCooldowns {
    cooldowns: HashMap<String, Cooldown>,
    tick_count: i32,
}

/// `ItemCooldowns.getCooldownGroup`: the stack's `use_cooldown` group, else
/// its item's id.
pub fn cooldown_group(stack: &ItemStackData) -> String {
    stack
        .get_component::<UseCooldown>()
        .and_then(|cooldown| cooldown.cooldown_group.as_ref().map(ToString::to_string))
        .unwrap_or_else(|| format!("minecraft:{}", item_resource_name(stack.kind)))
}

impl ItemCooldowns {
    /// `ItemCooldowns.tick`: time passes and finished cooldowns go.
    pub fn tick(&mut self) {
        self.tick_count += 1;
        let now = self.tick_count;
        self.cooldowns.retain(|_, cooldown| cooldown.end > now);
    }

    /// `ClientPacketListener.handleItemCooldown`: no duration ends the
    /// group's cooldown, any other starts it over from now.
    pub fn set(&mut self, group: String, duration: i32) {
        if duration == 0 {
            self.cooldowns.remove(&group);
        } else {
            let start = self.tick_count;
            self.cooldowns.insert(
                group,
                Cooldown {
                    start,
                    end: start + duration,
                },
            );
        }
    }

    /// `getCooldownPercent`: how much of the group's cooldown is left, 1
    /// down to 0, `partial_tick` into the current tick.
    pub fn percent(&self, group: &str, partial_tick: f32) -> f32 {
        let Some(cooldown) = self.cooldowns.get(group) else {
            return 0.0;
        };
        let total = (cooldown.end - cooldown.start) as f32;
        let remaining = cooldown.end as f32 - (self.tick_count as f32 + partial_tick);
        (remaining / total).clamp(0.0, 1.0)
    }

    /// `isOnCooldown`.
    pub fn is_on_cooldown(&self, stack: &ItemStackData) -> bool {
        self.percent(&cooldown_group(stack), 0.0) > 0.0
    }

    /// Every running cooldown's group and what's left of it, for drawing.
    pub fn view(&self, partial_tick: f32) -> Vec<(String, f32)> {
        self.cooldowns
            .keys()
            .map(|group| (group.clone(), self.percent(group, partial_tick)))
            .filter(|(_, percent)| *percent > 0.0)
            .collect()
    }

    pub fn clear(&mut self) {
        self.cooldowns.clear();
    }
}

#[cfg(test)]
mod tests {
    use azalea_registry::builtin::ItemKind;

    use super::*;

    fn stack(kind: ItemKind) -> ItemStackData {
        ItemStackData {
            kind,
            count: 1,
            component_patch: Default::default(),
        }
    }

    /// A cooldown runs down over its duration, covers only its group, and
    /// a zero duration ends it.
    #[test]
    fn a_cooldown_runs_down_and_ends() {
        let mut cooldowns = ItemCooldowns::default();
        let pearl = stack(ItemKind::EnderPearl);
        assert_eq!(cooldown_group(&pearl), "minecraft:ender_pearl");
        cooldowns.set("minecraft:ender_pearl".into(), 20);
        assert_eq!(cooldowns.percent("minecraft:ender_pearl", 0.0), 1.0);
        assert!(cooldowns.is_on_cooldown(&pearl));
        assert!(!cooldowns.is_on_cooldown(&stack(ItemKind::Stick)));
        for _ in 0..5 {
            cooldowns.tick();
        }
        assert_eq!(cooldowns.percent("minecraft:ender_pearl", 0.0), 0.75);
        assert_eq!(cooldowns.percent("minecraft:ender_pearl", 0.5), 0.725);
        assert_eq!(
            cooldowns.view(0.0),
            [("minecraft:ender_pearl".to_string(), 0.75)]
        );
        for _ in 0..15 {
            cooldowns.tick();
        }
        assert!(!cooldowns.is_on_cooldown(&pearl));
        assert!(cooldowns.view(0.0).is_empty());

        cooldowns.set("minecraft:ender_pearl".into(), 100);
        cooldowns.set("minecraft:ender_pearl".into(), 0);
        assert!(!cooldowns.is_on_cooldown(&pearl));
    }
}
