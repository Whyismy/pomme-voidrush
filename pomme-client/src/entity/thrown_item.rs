//! Projectiles drawn as their item (vanilla `ThrownItemRenderer`): snowballs,
//! eggs, ender pearls, potions, bottles o' enchanting, fireballs and eyes of
//! ender. Servers dress them in any stack, so Void Rush's mage bolts are
//! snowballs carrying a pack model. They fly on the client between the
//! server's position updates, as vanilla's do. Wind charges fly here too,
//! though they're drawn as their own model (`WindChargeRenderer`), and
//! firework rockets, which climb faster as they go and trail sparks.

use std::collections::HashMap;

use azalea_registry::builtin::EntityKind;
use glam::DVec3;

use crate::entity::components::Position;

pub struct ThrownItem {
    pub kind: EntityKind,
    pub position: Position,
    pub prev_position: Position,
    /// Where the server last put it, which its relative moves build on.
    server_position: Position,
    pub velocity: DVec3,
    /// The model key of the stack it carries (`EntityItemData`), else its
    /// kind's default item.
    pub model: Option<String>,
    pub stack: Option<azalea_inventory::ItemStackData>,
    /// Ticks since it spawned (vanilla `tickCount`).
    pub age: u32,
    /// `Entity.DATA_NO_GRAVITY`: Void Rush's mage bolts fly straight.
    no_gravity: bool,
    /// `FireworkRocketEntity.DATA_SHOT_AT_ANGLE`: a rocket from a crossbow,
    /// which keeps its speed.
    shot_at_angle: bool,
}

impl ThrownItem {
    /// The item a projectile shows before the server names one
    /// (`getDefaultItem`).
    pub fn default_item(kind: EntityKind) -> &'static str {
        match kind {
            EntityKind::Snowball => "snowball",
            EntityKind::Egg => "egg",
            EntityKind::EnderPearl => "ender_pearl",
            EntityKind::ExperienceBottle => "experience_bottle",
            EntityKind::SplashPotion => "splash_potion",
            EntityKind::LingeringPotion => "lingering_potion",
            EntityKind::EyeOfEnder => "ender_eye",
            EntityKind::FireworkRocket => "firework_rocket",
            _ => "fire_charge",
        }
    }

    /// `ThrownItemRenderer`'s scale: the ghast's fireball is three times
    /// the size, the blaze's three quarters.
    pub fn scale(&self) -> f32 {
        match self.kind {
            EntityKind::Fireball => 3.0,
            EntityKind::SmallFireball => 0.75,
            _ => 1.0,
        }
    }
}

/// The kinds [`ThrownItemStore`] tracks.
pub fn is_thrown_item(kind: EntityKind) -> bool {
    matches!(
        kind,
        EntityKind::Snowball
            | EntityKind::Egg
            | EntityKind::EnderPearl
            | EntityKind::ExperienceBottle
            | EntityKind::SplashPotion
            | EntityKind::LingeringPotion
            | EntityKind::Fireball
            | EntityKind::SmallFireball
            | EntityKind::EyeOfEnder
            | EntityKind::FireworkRocket
    ) || is_wind_charge(kind)
}

/// A player's wind charge or a breeze's.
pub fn is_wind_charge(kind: EntityKind) -> bool {
    matches!(kind, EntityKind::WindCharge | EntityKind::BreezeWindCharge)
}

/// What a projectile keeps of its speed each tick: a throwable's drag, or a
/// wind charge's inertia of one (`AbstractWindCharge`, which also has no
/// acceleration).
fn drag(kind: EntityKind) -> f64 {
    if is_wind_charge(kind) { 1.0 } else { 0.99 }
}

/// Each kind's `getDefaultGravity`; fireballs and eyes of ender fly level.
fn gravity(kind: EntityKind) -> f64 {
    match kind {
        EntityKind::Snowball | EntityKind::Egg | EntityKind::EnderPearl => 0.03,
        EntityKind::SplashPotion | EntityKind::LingeringPotion => 0.05,
        EntityKind::ExperienceBottle => 0.07,
        _ => 0.0,
    }
}

#[derive(Default)]
pub struct ThrownItemStore {
    pub items: HashMap<i32, ThrownItem>,
}

impl ThrownItemStore {
    pub fn spawn(&mut self, id: i32, kind: EntityKind, position: Position, velocity: DVec3) {
        self.items.insert(
            id,
            ThrownItem {
                kind,
                position,
                prev_position: position,
                server_position: position,
                velocity,
                model: None,
                stack: None,
                age: 0,
                no_gravity: false,
                shot_at_angle: false,
            },
        );
    }

    pub fn set_item(&mut self, id: i32, model: String, stack: azalea_inventory::ItemStackData) {
        if let Some(item) = self.items.get_mut(&id) {
            item.model = Some(model);
            item.stack = Some(stack);
        }
    }

    pub fn set_no_gravity(&mut self, id: i32, no_gravity: bool) {
        if let Some(item) = self.items.get_mut(&id) {
            item.no_gravity = no_gravity;
        }
    }

    pub fn move_delta(&mut self, id: i32, delta: DVec3) {
        if let Some(item) = self.items.get_mut(&id) {
            item.server_position = (DVec3::from(item.server_position) + delta).into();
            item.position = item.server_position;
        }
    }

    pub fn teleport(&mut self, id: i32, position: Position) {
        if let Some(item) = self.items.get_mut(&id) {
            item.server_position = position;
            item.position = position;
        }
    }

    pub fn set_motion(&mut self, id: i32, velocity: DVec3) {
        if let Some(item) = self.items.get_mut(&id) {
            item.velocity = velocity;
        }
    }

    /// `FireworkRocketEntity.DATA_SHOT_AT_ANGLE` (index 10).
    pub fn set_shot_at_angle(&mut self, id: i32, shot_at_angle: bool) {
        if let Some(item) = self.items.get_mut(&id)
            && item.kind == EntityKind::FireworkRocket
        {
            item.shot_at_angle = shot_at_angle;
        }
    }

    /// `ThrowableProjectile.tick` on the client: move, then drag and fall.
    /// A rocket instead speeds up as it goes (`FireworkRocketEntity.tick`);
    /// returned are where each rocket's spark starts this tick and how it
    /// moves.
    // TODO: water drag (0.8) and the eye of ender's own flight.
    pub fn tick(&mut self) -> Vec<(DVec3, DVec3)> {
        let mut sparks = Vec::new();
        for item in self.items.values_mut() {
            item.prev_position = item.position;
            if item.kind == EntityKind::FireworkRocket {
                if !item.shot_at_angle {
                    item.velocity =
                        item.velocity * DVec3::new(1.15, 1.0, 1.15) + DVec3::new(0.0, 0.04, 0.0);
                }
                item.position = (DVec3::from(item.position) + item.velocity).into();
                item.age = item.age.saturating_add(1);
                let drift = || crate::particle::next_gaussian() * 0.05;
                sparks.push((
                    DVec3::from(item.position),
                    DVec3::new(drift(), -item.velocity.y * 0.5, drift()),
                ));
                continue;
            }
            item.position = (DVec3::from(item.position) + item.velocity).into();
            item.velocity *= drag(item.kind);
            if !item.no_gravity {
                item.velocity.y -= gravity(item.kind);
            }
            item.age = item.age.saturating_add(1);
        }
        sparks
    }

    pub fn remove(&mut self, ids: &[i32]) {
        for id in ids {
            self.items.remove(id);
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snowballs_arc_and_fireballs_fly_level() {
        let mut store = ThrownItemStore::default();
        store.spawn(
            1,
            EntityKind::Snowball,
            Position::new(0.0, 0.0, 0.0),
            DVec3::new(1.0, 0.0, 0.0),
        );
        store.spawn(
            2,
            EntityKind::Fireball,
            Position::new(0.0, 0.0, 0.0),
            DVec3::new(1.0, 0.0, 0.0),
        );
        store.spawn(
            3,
            EntityKind::Snowball,
            Position::new(0.0, 0.0, 0.0),
            DVec3::new(1.0, 0.0, 0.0),
        );
        store.set_no_gravity(3, true);
        for _ in 0..10 {
            store.tick();
        }
        assert!(store.items[&1].position.y < -0.5, "a snowball falls");
        assert_eq!(store.items[&3].position.y, 0.0, "a mage bolt doesn't");
        assert_eq!(store.items[&2].position.y, 0.0, "a fireball doesn't");
        assert_eq!(ThrownItem::default_item(EntityKind::Snowball), "snowball");

        // A rocket speeds up sideways and climbs, sparking as it goes; one
        // shot from a crossbow keeps its speed.
        store.spawn(
            5,
            EntityKind::FireworkRocket,
            Position::new(0.0, 0.0, 0.0),
            DVec3::new(0.1, 0.5, 0.0),
        );
        store.spawn(
            6,
            EntityKind::FireworkRocket,
            Position::new(0.0, 0.0, 0.0),
            DVec3::new(0.1, 0.5, 0.0),
        );
        store.set_shot_at_angle(6, true);
        store.set_shot_at_angle(1, true);
        let sparks = store.tick();
        assert_eq!(sparks.len(), 2);
        let rocket = &store.items[&5];
        assert!((rocket.velocity - DVec3::new(0.115, 0.54, 0.0)).length() < 1e-12);
        assert_eq!(*rocket.position, rocket.velocity);
        assert_eq!(store.items[&6].velocity, DVec3::new(0.1, 0.5, 0.0));
        assert_eq!(
            ThrownItem::default_item(EntityKind::FireworkRocket),
            "firework_rocket"
        );
        store.remove(&[5, 6]);

        // A wind charge keeps its speed and its height.
        store.spawn(
            4,
            EntityKind::WindCharge,
            Position::new(0.0, 0.0, 0.0),
            DVec3::new(1.5, 0.0, 0.0),
        );
        for _ in 0..10 {
            store.tick();
        }
        let charge = &store.items[&4];
        assert_eq!((charge.position.x, charge.position.y), (15.0, 0.0));
        assert!(is_wind_charge(charge.kind) && !is_wind_charge(EntityKind::Fireball));
        assert_eq!(store.items[&2].scale(), 3.0);
    }
}
