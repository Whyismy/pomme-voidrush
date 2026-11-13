//! Arrows (vanilla `AbstractArrow` and `ArrowRenderer`): they fly on the
//! client between the server's rare position updates (every 20 ticks), turn
//! to face where they're going, and stop where they hit a block until the
//! server says they're stuck (`IN_GROUND`).

use std::collections::HashMap;

use azalea_registry::builtin::EntityKind;
use glam::DVec3;

use crate::entity::components::Position;
use crate::physics::collision::CollisionWorld;

pub struct Arrow {
    pub kind: EntityKind,
    pub position: Position,
    pub prev_position: Position,
    /// Where the server last put it: its relative moves build on this, not
    /// on where this client has flown it since (`VecDeltaCodec`).
    server_position: Position,
    pub velocity: DVec3,
    /// Vanilla's arrow rotation: `atan2(dx, dz)` and the climb, in degrees.
    pub y_rot: f32,
    pub x_rot: f32,
    pub prev_y_rot: f32,
    pub prev_x_rot: f32,
    /// `AbstractArrow.IN_GROUND`, or stopped on a block this client hit.
    pub in_ground: bool,
    /// `Arrow.ID_EFFECT_COLOR` isn't -1: a tipped arrow.
    pub tipped: bool,
    /// `ThrownTrident.ID_FOIL`: an enchanted trident, which glints.
    pub foil: bool,
}

/// The kinds [`ArrowStore`] tracks: arrows, and thrown tridents, which fly
/// and stick the same way (`AbstractArrow`).
pub fn is_arrow(kind: EntityKind) -> bool {
    matches!(
        kind,
        EntityKind::Arrow | EntityKind::SpectralArrow | EntityKind::Trident
    )
}

#[derive(Default)]
pub struct ArrowStore {
    pub arrows: HashMap<i32, Arrow>,
}

impl ArrowStore {
    pub fn spawn(
        &mut self,
        id: i32,
        kind: EntityKind,
        position: Position,
        velocity: DVec3,
        y_rot: f32,
        x_rot: f32,
    ) {
        self.arrows.insert(
            id,
            Arrow {
                kind,
                position,
                prev_position: position,
                server_position: position,
                velocity,
                y_rot,
                x_rot,
                prev_y_rot: y_rot,
                prev_x_rot: x_rot,
                in_ground: false,
                tipped: false,
                foil: false,
            },
        );
    }

    pub fn move_delta(&mut self, id: i32, delta: DVec3) {
        if let Some(arrow) = self.arrows.get_mut(&id) {
            arrow.server_position = (*arrow.server_position + delta).into();
            arrow.position = arrow.server_position;
        }
    }

    pub fn teleport(&mut self, id: i32, position: Position) {
        if let Some(arrow) = self.arrows.get_mut(&id) {
            arrow.server_position = position;
            arrow.position = position;
        }
    }

    pub fn rotate(&mut self, id: i32, y_rot: f32, x_rot: f32) {
        if let Some(arrow) = self.arrows.get_mut(&id) {
            arrow.y_rot = y_rot;
            arrow.x_rot = x_rot;
        }
    }

    pub fn set_motion(&mut self, id: i32, velocity: DVec3) {
        if let Some(arrow) = self.arrows.get_mut(&id) {
            arrow.velocity = velocity;
        }
    }

    /// `AbstractArrow.IN_GROUND` (index 10) and `Arrow.ID_EFFECT_COLOR` (11).
    pub fn apply_entity_data(&mut self, id: i32, index: u8, value: &super::MetaValue) {
        let Some(arrow) = self.arrows.get_mut(&id) else {
            return;
        };
        match (index, value) {
            (10, super::MetaValue::Bool(in_ground)) => arrow.in_ground = *in_ground,
            (11, super::MetaValue::Int(color)) if arrow.kind == EntityKind::Arrow => {
                arrow.tipped = *color != -1;
            }
            (12, super::MetaValue::Bool(foil)) if arrow.kind == EntityKind::Trident => {
                arrow.foil = *foil;
            }
            _ => {}
        }
    }

    /// `AbstractArrow.tick` on the client: fly, slow and fall, turn toward
    /// the flight, and stop on the first block in the way.
    pub fn tick(&mut self, world: CollisionWorld<'_>) {
        for arrow in self.arrows.values_mut() {
            arrow.prev_position = arrow.position;
            arrow.prev_y_rot = arrow.y_rot;
            arrow.prev_x_rot = arrow.x_rot;
            if arrow.in_ground {
                continue;
            }
            let from = *arrow.position;
            let to = from + arrow.velocity;
            match first_block_hit(world, from, to) {
                Some(hit) => {
                    arrow.position = hit.into();
                    arrow.velocity = DVec3::ZERO;
                    arrow.in_ground = true;
                    continue;
                }
                None => arrow.position = to.into(),
            }
            // `Projectile.updateRotation`: a fifth of the way toward the
            // flight each tick.
            let v = arrow.velocity;
            let flat = (v.x * v.x + v.z * v.z).sqrt();
            let target_y = v.x.atan2(v.z).to_degrees() as f32;
            let target_x = v.y.atan2(flat).to_degrees() as f32;
            arrow.y_rot = lerp_rotation(arrow.y_rot, target_y);
            arrow.x_rot = lerp_rotation(arrow.x_rot, target_x);
            arrow.velocity *= 0.99;
            arrow.velocity.y -= 0.05;
        }
    }

    pub fn remove(&mut self, ids: &[i32]) {
        for id in ids {
            self.arrows.remove(id);
        }
    }

    pub fn clear(&mut self) {
        self.arrows.clear();
    }
}

/// Vanilla `Projectile.lerpRotation`: a fifth of the way, the short way round.
fn lerp_rotation(from: f32, to: f32) -> f32 {
    let mut from = from;
    while to - from < -180.0 {
        from -= 360.0;
    }
    while to - from >= 180.0 {
        from += 360.0;
    }
    from + (to - from) * 0.2
}

/// Where the segment from `from` to `to` first enters a block's collision
/// shape, sampled a tenth of a block at a time.
fn first_block_hit(world: CollisionWorld<'_>, from: DVec3, to: DVec3) -> Option<DVec3> {
    let length = (to - from).length();
    let steps = (length / 0.1).ceil().max(1.0) as i32;
    for step in 1..=steps {
        let point = from + (to - from) * (f64::from(step) / f64::from(steps));
        let probe = crate::physics::aabb::Aabb {
            min: point,
            max: point,
        };
        let boxes = crate::physics::collision::collect_block_aabbs(
            world,
            &probe,
            &crate::physics::block_shape::CollisionContext::EMPTY,
        );
        if boxes.iter().any(|aabb| aabb.contains(point)) {
            return Some(from + (to - from) * (f64::from(step - 1) / f64::from(steps)));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_turns_a_fifth_of_the_short_way() {
        assert!((lerp_rotation(0.0, 10.0) - 2.0).abs() < 1.0e-6);
        // 170 to -170 crosses 180: 4 degrees on, at 174 (as -186).
        assert!((lerp_rotation(170.0, -170.0).rem_euclid(360.0) - 174.0).abs() < 1.0e-4);
    }
}
