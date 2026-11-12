//! Entities drawn as a small model of their own that are neither mobs nor
//! items: shulker bullets, wither skulls, llama spit, evoker fangs and lead
//! knots. The projectiles fly on the client between the server's position
//! updates, as vanilla's do, and leave their trails.

use std::collections::HashMap;

use azalea_registry::builtin::EntityKind;
use glam::DVec3;

use crate::entity::components::Position;

/// `EvokerFangs.lifeTicks` when the bite starts.
const FANGS_LIFE_TICKS: i32 = 22;
/// `AbstractHurtingProjectile.accelerationPower`'s default, which the spawn
/// packet doesn't carry.
const SKULL_ACCELERATION: f64 = 0.1;

pub struct ModelEntity {
    pub kind: EntityKind,
    pub position: Position,
    pub prev_position: Position,
    /// Where the server last put it, which its relative moves build on.
    server_position: Position,
    pub velocity: DVec3,
    pub y_rot: f32,
    pub x_rot: f32,
    pub prev_y_rot: f32,
    pub prev_x_rot: f32,
    /// Ticks since it spawned (vanilla `tickCount`).
    pub age: u32,
    /// `WitherSkull.DATA_DANGEROUS`: a blue skull.
    pub dangerous: bool,
    /// Evoker fangs: ticks of the bite left, once entity event 4 starts it.
    fangs_life: Option<i32>,
}

impl ModelEntity {
    /// `EvokerFangs.getAnimationProgress`: 0 until the bite starts, 1 once
    /// it is done.
    pub fn bite_progress(&self, partial_tick: f32) -> f32 {
        match self.fangs_life {
            None => 0.0,
            Some(life) if life - 2 <= 0 => 1.0,
            Some(life) => 1.0 - ((life - 2) as f32 - partial_tick) / 20.0,
        }
    }
}

/// The kinds [`ModelEntityStore`] tracks.
pub fn is_model_entity(kind: EntityKind) -> bool {
    matches!(
        kind,
        EntityKind::ShulkerBullet
            | EntityKind::WitherSkull
            | EntityKind::LlamaSpit
            | EntityKind::EvokerFangs
            | EntityKind::LeashKnot
    )
}

/// Where an entity put at `position` is: a lead knot is sent as the block
/// it is tied in, and sits in the middle of it, three eighths up
/// (`LeashFenceKnotEntity.recalculateBoundingBox`).
fn settled(kind: EntityKind, position: Position) -> Position {
    if kind == EntityKind::LeashKnot {
        (position.floor() + DVec3::new(0.5, 0.375, 0.5)).into()
    } else {
        position
    }
}

/// What a tick asks the particle engine for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Trail {
    /// A shulker bullet's end rod sparkle.
    EndRod(DVec3),
    /// A wither skull's smoke.
    Smoke(DVec3),
    /// One of the crits a pair of fangs throws as it snaps shut.
    Crit(DVec3, DVec3),
}

#[derive(Default)]
pub struct ModelEntityStore {
    pub entities: HashMap<i32, ModelEntity>,
}

impl ModelEntityStore {
    pub fn spawn(
        &mut self,
        id: i32,
        kind: EntityKind,
        position: Position,
        velocity: DVec3,
        y_rot: f32,
        x_rot: f32,
    ) {
        let position = settled(kind, position);
        self.entities.insert(
            id,
            ModelEntity {
                kind,
                position,
                prev_position: position,
                server_position: position,
                velocity,
                y_rot,
                x_rot,
                prev_y_rot: y_rot,
                prev_x_rot: x_rot,
                age: 0,
                dangerous: false,
                fangs_life: None,
            },
        );
    }

    pub fn move_delta(&mut self, id: i32, delta: DVec3) {
        if let Some(entity) = self.entities.get_mut(&id) {
            entity.server_position = (*entity.server_position + delta).into();
            entity.position = entity.server_position;
        }
    }

    pub fn teleport(&mut self, id: i32, position: Position) {
        if let Some(entity) = self.entities.get_mut(&id) {
            let position = settled(entity.kind, position);
            entity.server_position = position;
            entity.position = position;
        }
    }

    pub fn rotate(&mut self, id: i32, y_rot: f32, x_rot: f32) {
        if let Some(entity) = self.entities.get_mut(&id) {
            entity.y_rot = y_rot;
            entity.x_rot = x_rot;
        }
    }

    pub fn set_motion(&mut self, id: i32, velocity: DVec3) {
        if let Some(entity) = self.entities.get_mut(&id) {
            entity.velocity = velocity;
        }
    }

    /// `WitherSkull.DATA_DANGEROUS` (index 8).
    pub fn apply_entity_data(&mut self, id: i32, index: u8, value: super::MetaValue) {
        if let Some(entity) = self.entities.get_mut(&id)
            && entity.kind == EntityKind::WitherSkull
            && index == 8
            && let super::MetaValue::Bool(dangerous) = value
        {
            entity.dangerous = dangerous;
        }
    }

    /// Entity event 4 on a pair of evoker fangs
    /// (`EvokerFangs.handleEntityEvent`): the bite starts. Where they are,
    /// for the snap's sound.
    pub fn start_bite(&mut self, id: i32) -> Option<Position> {
        let fangs = self.entities.get_mut(&id)?;
        if fangs.kind != EntityKind::EvokerFangs {
            return None;
        }
        fangs.fangs_life = Some(FANGS_LIFE_TICKS);
        Some(fangs.position)
    }

    /// Each kind's client `tick`; the particles they leave this tick.
    pub fn tick(&mut self) -> Vec<Trail> {
        let mut trails = Vec::new();
        for entity in self.entities.values_mut() {
            entity.prev_position = entity.position;
            entity.prev_y_rot = entity.y_rot;
            entity.prev_x_rot = entity.x_rot;
            entity.age = entity.age.saturating_add(1);
            match entity.kind {
                // `ShulkerBullet.tick`: straight on, sparkling behind.
                EntityKind::ShulkerBullet => {
                    entity.position = (*entity.position + entity.velocity).into();
                    rotate_towards_movement(entity, 0.5);
                    trails.push(Trail::EndRod(
                        *entity.position - entity.velocity + DVec3::new(0.0, 0.15, 0.0),
                    ));
                }
                // `AbstractHurtingProjectile.tick`: it speeds up along its
                // flight and sheds some of that speed, then moves.
                EntityKind::WitherSkull => {
                    let inertia = if entity.dangerous { 0.73f32 } else { 0.95f32 };
                    let v = entity.velocity;
                    entity.velocity =
                        (v + v.normalize_or_zero() * SKULL_ACCELERATION) * f64::from(inertia);
                    entity.position = (*entity.position + entity.velocity).into();
                    rotate_towards_movement(entity, 0.2);
                    trails.push(Trail::Smoke(*entity.position + DVec3::new(0.0, 0.5, 0.0)));
                }
                // `LlamaSpit.tick`: an arc, turning to face it.
                EntityKind::LlamaSpit => {
                    let v = entity.velocity;
                    let flat = (v.x * v.x + v.z * v.z).sqrt();
                    let target_y = v.x.atan2(v.z).to_degrees() as f32;
                    let target_x = v.y.atan2(flat).to_degrees() as f32;
                    entity.y_rot = super::lerp_angle(entity.y_rot, target_y, 0.2);
                    entity.x_rot = super::lerp_angle(entity.x_rot, target_x, 0.2);
                    entity.position = (*entity.position + v).into();
                    entity.velocity = v * f64::from(0.99f32);
                    entity.velocity.y -= 0.06;
                }
                // `EvokerFangs.tick`: twelve crits as the jaws meet.
                EntityKind::EvokerFangs => {
                    if let Some(life) = &mut entity.fangs_life {
                        *life -= 1;
                        if *life == 14 {
                            let spread = || fastrand::f64() * 2.0 - 1.0;
                            for _ in 0..12 {
                                let at = *entity.position
                                    + DVec3::new(
                                        spread() * 0.25,
                                        0.05 + fastrand::f64(),
                                        spread() * 0.25,
                                    );
                                let speed = DVec3::new(
                                    spread() * 0.3,
                                    0.3 + fastrand::f64() * 0.3,
                                    spread() * 0.3,
                                );
                                trails.push(Trail::Crit(at, speed));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        trails
    }

    pub fn remove(&mut self, ids: &[i32]) {
        for id in ids {
            self.entities.remove(id);
        }
    }

    pub fn clear(&mut self) {
        self.entities.clear();
    }
}

/// `ProjectileUtil.rotateTowardsMovement`: `share` of the way from where it
/// faced to where it flies.
fn rotate_towards_movement(entity: &mut ModelEntity, share: f32) {
    let v = entity.velocity;
    if v.length_squared() == 0.0 {
        return;
    }
    let flat = (v.x * v.x + v.z * v.z).sqrt();
    let target_y = v.z.atan2(v.x).to_degrees() as f32 + 90.0;
    let target_x = flat.atan2(v.y).to_degrees() as f32 - 90.0;
    entity.y_rot = super::lerp_angle(entity.prev_y_rot, target_y, share);
    entity.x_rot = super::lerp_angle(entity.prev_x_rot, target_x, share);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(kind: EntityKind, velocity: DVec3) -> ModelEntityStore {
        let mut store = ModelEntityStore::default();
        store.spawn(1, kind, Position::new(0.0, 64.0, 0.0), velocity, 0.0, 0.0);
        store
    }

    #[test]
    fn bullets_fly_straight_and_sparkle_behind() {
        let mut store = store_with(EntityKind::ShulkerBullet, DVec3::new(0.15, 0.0, 0.0));
        let trails = store.tick();
        let bullet = &store.entities[&1];
        assert_eq!(*bullet.position, DVec3::new(0.15, 64.0, 0.0));
        assert_eq!(trails, [Trail::EndRod(DVec3::new(0.0, 64.15, 0.0))]);
        // Flying +x is facing yaw 90 in a projectile's terms; it turns
        // half the way there each tick.
        assert_eq!(bullet.y_rot, 45.0);
        assert!(is_model_entity(bullet.kind) && !is_model_entity(EntityKind::Arrow));
    }

    #[test]
    fn skulls_speed_up_and_a_blue_one_drags() {
        let mut store = store_with(EntityKind::WitherSkull, DVec3::new(1.0, 0.0, 0.0));
        store.tick();
        let speed = store.entities[&1].velocity.x;
        assert!((speed - 1.1 * f64::from(0.95f32)).abs() < 1e-9);
        assert_eq!(store.entities[&1].position.x, speed);

        let mut store = store_with(EntityKind::WitherSkull, DVec3::new(1.0, 0.0, 0.0));
        store.apply_entity_data(1, 8, crate::entity::MetaValue::Bool(true));
        let trails = store.tick();
        assert!((store.entities[&1].velocity.x - 1.1 * f64::from(0.73f32)).abs() < 1e-9);
        assert!(matches!(trails[..], [Trail::Smoke(at)] if at.y == 64.5));
    }

    #[test]
    fn spit_arcs() {
        let mut store = store_with(EntityKind::LlamaSpit, DVec3::new(0.5, 0.0, 0.0));
        store.tick();
        let spit = &store.entities[&1];
        assert_eq!(*spit.position, DVec3::new(0.5, 64.0, 0.0));
        assert!((spit.velocity.x - 0.5 * f64::from(0.99f32)).abs() < 1e-12);
        assert_eq!(spit.velocity.y, -0.06);
    }

    /// The bite runs 20 ticks from the event, throwing its crits 8 ticks in,
    /// and holds shut after.
    #[test]
    fn fangs_bite_when_told_to() {
        let mut store = store_with(EntityKind::EvokerFangs, DVec3::ZERO);
        store.tick();
        assert_eq!(store.entities[&1].bite_progress(0.5), 0.0);
        assert!(store.start_bite(1).is_some());
        assert_eq!(store.entities[&1].bite_progress(0.0), 0.0);
        let mut crits = Vec::new();
        for _ in 0..10 {
            crits.push(store.tick().len());
        }
        assert_eq!(crits, [0, 0, 0, 0, 0, 0, 0, 12, 0, 0]);
        assert!((store.entities[&1].bite_progress(0.0) - 0.5).abs() < 1e-6);
        for _ in 0..30 {
            store.tick();
        }
        assert_eq!(store.entities[&1].bite_progress(0.0), 1.0);

        // Only fangs bite.
        let mut store = store_with(EntityKind::LeashKnot, DVec3::ZERO);
        assert!(store.start_bite(1).is_none());
        // A knot sits in the middle of its block, three eighths up.
        assert_eq!(*store.entities[&1].position, DVec3::new(0.5, 64.375, 0.5));
    }
}
