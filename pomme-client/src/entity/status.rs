//! Entity status events the client answers on its own with particles, a
//! sound or a picture on screen (vanilla `Entity.handleEntityEvent` and its
//! overrides), and the state two of them leave behind.

use azalea_inventory::ItemStackData;
use azalea_inventory::components::EquipmentSlot;

/// A status event, by what it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntityStatus {
    /// 60, `LivingEntity.makePoofParticles`: a dead mob's puff of smoke.
    DeathPoof,
    /// 20, `Mob.spawnAnim`: the same puff about a mob a spawner made.
    SpawnPoof,
    /// 18, `Animal`: hearts over an animal in love.
    LoveHearts,
    /// 7 and 6, `TamableAnimal.spawnTamingParticles`: hearts when tamed,
    /// smoke when not; 41 and 40 are an ocelot's trust, shown the same.
    TameHearts,
    TameSmoke,
    /// 38, `Dolphin`: happy sparkles about a fed dolphin.
    DolphinHappy,
    /// 15, `Witch`: the magic over a witch's head.
    WitchMagic,
    /// 16, `ZombieVillager`: the sound of a cure beginning.
    CureSound,
    /// 32, `ArmorStand`: the sound of one being hit.
    ArmorStandHit,
    /// 12, 13, 14 and 42, `Villager`: hearts, anger, happiness, a splash.
    VillagerHearts,
    VillagerAngry,
    VillagerHappy,
    VillagerSplash,
    /// 35: a totem of undying saved it.
    Totem,
    /// 46: it teleported (a chorus fruit, an enderman), leaving a trail.
    Teleport,
    /// 47 to 52, `LivingEntity.breakItem`: what it had in a slot broke.
    ItemBreak(EquipmentSlot),
}

impl EntityStatus {
    /// The status of event `event`, where it is one of these.
    pub fn from_event(event: i8) -> Option<Self> {
        Some(match event {
            6 => Self::TameSmoke,
            7 => Self::TameHearts,
            12 => Self::VillagerHearts,
            13 => Self::VillagerAngry,
            14 => Self::VillagerHappy,
            15 => Self::WitchMagic,
            16 => Self::CureSound,
            18 => Self::LoveHearts,
            20 => Self::SpawnPoof,
            32 => Self::ArmorStandHit,
            35 => Self::Totem,
            38 => Self::DolphinHappy,
            40 => Self::TameSmoke,
            41 => Self::TameHearts,
            42 => Self::VillagerSplash,
            46 => Self::Teleport,
            // `LivingEntity.entityEventForEquipmentBreak`.
            47 => Self::ItemBreak(EquipmentSlot::Mainhand),
            48 => Self::ItemBreak(EquipmentSlot::Offhand),
            49 => Self::ItemBreak(EquipmentSlot::Head),
            50 => Self::ItemBreak(EquipmentSlot::Chest),
            51 => Self::ItemBreak(EquipmentSlot::Legs),
            52 => Self::ItemBreak(EquipmentSlot::Feet),
            60 => Self::DeathPoof,
            _ => return None,
        })
    }
}

/// Vanilla `TrackingEmitter`: totem particles thrown from about an entity
/// for thirty ticks.
#[derive(Clone, Copy, Debug)]
pub struct TrackingEmitter {
    pub entity: i32,
    pub life: u32,
}

impl TrackingEmitter {
    pub const LIFETIME: u32 = 30;
}

/// Vanilla `GameRenderer.displayItemActivation`: the item that swells and
/// spins in the middle of the screen when a totem saves the player.
#[derive(Clone, Debug)]
pub struct ItemActivation {
    pub stack: ItemStackData,
    /// Ticks left of its forty.
    pub ticks: i32,
    /// Which way it drifts from the middle, each -1 to 1.
    pub offset: (f32, f32),
}

impl ItemActivation {
    pub fn new(stack: ItemStackData) -> Self {
        Self {
            stack,
            ticks: 40,
            offset: (fastrand::f32() * 2.0 - 1.0, fastrand::f32() * 2.0 - 1.0),
        }
    }

    /// `renderItemActivationAnimation` at `partial_tick`: how far along its
    /// drift the item is (0 to 1), its size in GUI pixels, and the cosine
    /// of its turn about the upright, which narrows a flat item.
    pub fn frame(&self, partial_tick: f32) -> (f32, f32, f32) {
        let f = ((40 - self.ticks) as f32 + partial_tick) / 40.0;
        let g = f * f;
        let h = f * g;
        let j = 10.25 * h * g - 24.95 * g * g + 25.5 * h - 13.8 * g + 4.0 * f;
        let k = j * std::f32::consts::PI;
        let drift = (k * 2.0).sin().abs();
        let size = 50.0 + 175.0 * k.sin();
        let turn = (900.0 * k.sin().abs()).to_radians();
        (drift, size, turn.cos())
    }
}

#[cfg(test)]
mod tests {
    use azalea_registry::builtin::ItemKind;

    use super::*;

    /// The events map to what they show, and the rest are left alone.
    #[test]
    fn events_map_to_their_status() {
        assert_eq!(EntityStatus::from_event(60), Some(EntityStatus::DeathPoof));
        assert_eq!(EntityStatus::from_event(35), Some(EntityStatus::Totem));
        assert_eq!(
            EntityStatus::from_event(50),
            Some(EntityStatus::ItemBreak(EquipmentSlot::Chest))
        );
        assert_eq!(
            EntityStatus::from_event(52),
            Some(EntityStatus::ItemBreak(EquipmentSlot::Feet))
        );
        assert_eq!(EntityStatus::from_event(41), Some(EntityStatus::TameHearts));
        // The death itself, and events other code answers.
        for event in [3, 4, 9, 17, 45] {
            assert_eq!(EntityStatus::from_event(event), None);
        }
    }

    /// The totem starts small in the middle, swells well past its first
    /// size as it spins, and is back near the middle at the end.
    #[test]
    fn the_totem_swells_and_spins() {
        let mut item = ItemActivation::new(ItemStackData::new(ItemKind::TotemOfUndying, 1));
        let (drift, size, facing) = item.frame(0.0);
        assert_eq!((drift, size, facing), (0.0, 50.0, 1.0));
        item.ticks = 20;
        let (_, size, _) = item.frame(0.0);
        assert!(size > 150.0, "{size}");
        item.ticks = 1;
        let (drift, _, _) = item.frame(1.0);
        assert!(drift < 0.2, "{drift}");
    }
}
