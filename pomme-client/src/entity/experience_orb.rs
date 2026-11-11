//! Experience orbs (vanilla `ExperienceOrb`): glowing sprites that fall,
//! bounce and drift toward a nearby player. The server owns them, but the
//! client runs the same movement between its updates.

use std::collections::HashMap;

use glam::DVec3;

use crate::entity::MetaValue;
use crate::entity::components::Position;
use crate::physics::aabb::Aabb;
use crate::physics::block_shape::CollisionContext;
use crate::physics::collision::resolve_collision;
use crate::position::ChunkPos;
use crate::world::block::FluidKind;
use crate::world::chunk::ChunkStore;

/// `ExperienceOrb.DATA_VALUE`.
const VALUE: u8 = 8;

/// `ExperienceOrb.getDefaultGravity`.
const GRAVITY: f64 = 0.03;

/// `EntityType.EXPERIENCE_ORB`'s box is half a block cubed.
const HALF_WIDTH: f64 = 0.25;

/// How near a player draws an orb in (`followNearbyPlayer`).
const FOLLOW_RANGE: f64 = 8.0;

/// The orb's sprite sheet, in its 26.1 folder or the older place.
pub(crate) const TEXTURES: [&str; 2] =
    ["entity/experience/experience_orb", "entity/experience_orb"];

pub struct ExperienceOrb {
    pub position: DVec3,
    pub prev_position: DVec3,
    pub velocity: DVec3,
    on_ground: bool,
    /// The experience it holds, which picks its sprite.
    pub value: i32,
    /// Ticks since it appeared, which its colour pulses with.
    pub age: u32,
}

impl ExperienceOrb {
    /// `ExperienceOrb.getIcon`: which of the sheet's sprites, by value.
    pub fn icon(&self) -> u32 {
        const AT_LEAST: [i32; 10] = [3, 7, 17, 37, 73, 149, 307, 617, 1237, 2477];
        AT_LEAST.iter().filter(|&&v| self.value >= v).count() as u32
    }

    /// `ExperienceOrbRenderer`'s colour `partial_tick` into the tick: green
    /// pulsing toward yellow, half clear, as RGBA.
    pub fn color(&self, partial_tick: f32) -> [u8; 4] {
        let phase = (self.age as f32 + partial_tick) / 2.0;
        let red = (phase.sin() + 1.0) * 0.5 * 255.0;
        let blue = ((phase + std::f32::consts::PI * 4.0 / 3.0).sin() + 1.0) * 0.1 * 255.0;
        [red as u8, 255, blue as u8, 128]
    }
}

#[derive(Default)]
pub struct ExperienceOrbStore {
    pub orbs: HashMap<i32, ExperienceOrb>,
}

impl ExperienceOrbStore {
    pub fn spawn(&mut self, id: i32, position: Position, velocity: DVec3) {
        let position = DVec3::from(position);
        self.orbs.insert(
            id,
            ExperienceOrb {
                position,
                prev_position: position,
                velocity,
                on_ground: false,
                value: 0,
                age: 0,
            },
        );
    }

    pub fn move_delta(&mut self, id: i32, delta: DVec3) {
        if let Some(orb) = self.orbs.get_mut(&id) {
            orb.position += delta;
        }
    }

    pub fn teleport(&mut self, id: i32, position: Position) {
        if let Some(orb) = self.orbs.get_mut(&id) {
            orb.position = position.into();
        }
    }

    pub fn set_motion(&mut self, id: i32, velocity: DVec3) {
        if let Some(orb) = self.orbs.get_mut(&id) {
            orb.velocity = velocity;
        }
    }

    pub fn apply_entity_data(&mut self, id: i32, index: u8, value: MetaValue) {
        if let (Some(orb), VALUE, MetaValue::Int(value)) = (self.orbs.get_mut(&id), index, value) {
            orb.value = value;
        }
    }

    /// Takes the orb out as it's collected, returning where it was.
    pub fn take(&mut self, id: i32) -> Option<DVec3> {
        self.orbs.remove(&id).map(|orb| orb.position)
    }

    pub fn remove(&mut self, ids: &[i32]) {
        for id in ids {
            self.orbs.remove(id);
        }
    }

    pub fn clear(&mut self) {
        self.orbs.clear();
    }

    /// `ExperienceOrb.tick`'s movement. `player` is the point orbs make for:
    /// the local player's position raised by half their eye height, or
    /// `None` when they draw none in (a spectator).
    pub fn tick(
        &mut self,
        chunk_store: &ChunkStore,
        block_state_table: crate::world::block::BlockStateTable,
        player: Option<DVec3>,
    ) {
        for orb in self.orbs.values_mut() {
            orb.prev_position = orb.position;
            orb.age += 1;
            let block = orb.position.floor().as_ivec3();
            let chunk = ChunkPos::new(block.x.div_euclid(16), block.z.div_euclid(16));
            if chunk_store.get_chunk(&chunk).is_none() {
                // Don't fall through terrain that hasn't loaded.
                continue;
            }
            let state = chunk_store.get_block_state(block.x, block.y, block.z);
            let fluid = block_state_table.block_state_data(state).fluid();
            let in_water = fluid.kind == FluidKind::Water
                && f64::from(block.y) + f64::from(fluid.height()) > orb.position.y;
            if in_water {
                // `setUnderwaterMovement`: it slows and floats up.
                orb.velocity.x *= 0.99;
                orb.velocity.z *= 0.99;
                orb.velocity.y = (orb.velocity.y + 5.0e-4).min(0.06);
            } else {
                orb.velocity.y -= GRAVITY;
            }
            if let Some(player) = player {
                let toward = player - orb.position;
                let distance_sq = toward.length_squared();
                if distance_sq < FOLLOW_RANGE * FOLLOW_RANGE && distance_sq > 0.0 {
                    let power = 1.0 - distance_sq.sqrt() / FOLLOW_RANGE;
                    orb.velocity += toward.normalize() * power * power * 0.1;
                }
            }
            let aabb = Aabb::from_center(orb.position, HALF_WIDTH, HALF_WIDTH);
            let ctx = CollisionContext::entity(orb.position.y, false, false);
            let (delta, on_ground) = resolve_collision(
                crate::physics::collision::CollisionWorld::new(chunk_store, block_state_table),
                aabb,
                orb.velocity.into(),
                0.0,
                orb.on_ground,
                &ctx,
            );
            orb.position += delta;
            orb.on_ground = on_ground;
            // TODO: the block below's own friction (ice, slime), as for
            // dropped items.
            let friction = if on_ground { 0.6 * 0.98 } else { 0.98 };
            orb.velocity *= DVec3::new(friction, 0.98, friction);
            if on_ground {
                orb.velocity.y *= -0.9;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::block::BlockStateTable;

    fn orb(value: i32) -> ExperienceOrb {
        ExperienceOrb {
            position: DVec3::ZERO,
            prev_position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            on_ground: false,
            value,
            age: 0,
        }
    }

    /// `getIcon`'s thresholds, and the pulse's ends.
    #[test]
    fn value_picks_the_sprite_and_age_the_colour() {
        let icons: Vec<u32> = [0, 2, 3, 6, 7, 37, 2476, 2477, 99999]
            .into_iter()
            .map(|value| orb(value).icon())
            .collect();
        assert_eq!(icons, [0, 0, 1, 1, 2, 4, 9, 10, 10]);
        // sin(0) = 0: half red, green full, and blue from sin(4pi/3).
        assert_eq!(orb(1).color(0.0), [127, 255, 3, 128]);
        let mut later = orb(1);
        later.age = 3;
        // Phase 1.5 plus a little: red near its peak.
        assert!(later.color(0.14)[0] > 250);
    }

    /// An orb beside the player is drawn toward them; one out of range
    /// only falls.
    #[test]
    fn a_near_player_draws_the_orb_in() {
        let table = BlockStateTable::native();
        let mut chunks = ChunkStore::new(4);
        let sections = chunks.section_count() as usize;
        chunks.insert_chunk(
            ChunkPos::new(0, 0),
            crate::world::chunk::Chunk::empty(sections),
        );
        let mut store = ExperienceOrbStore::default();
        store.spawn(1, Position::new(8.0, 80.0, 8.0), DVec3::ZERO);
        store.spawn(2, Position::new(8.0, 80.0, 8.0), DVec3::ZERO);
        store.apply_entity_data(1, 8, MetaValue::Int(11));
        assert_eq!(store.orbs[&1].icon(), 2);

        store.tick(&chunks, table, Some(DVec3::new(12.0, 80.0, 8.0)));
        let near = &store.orbs[&1];
        // Four blocks off: half power squared, a tenth of it, less drag.
        assert!((near.velocity.x - 0.025 * 0.98).abs() < 1e-9);
        assert!(near.position.x > 8.0 && near.position.y < 80.0);

        let mut far = ExperienceOrbStore::default();
        far.spawn(3, Position::new(8.0, 80.0, 8.0), DVec3::ZERO);
        far.tick(&chunks, table, Some(DVec3::new(8.0, 80.0, 0.0)));
        assert_eq!(far.orbs[&3].velocity.z, 0.0);
        assert!((far.orbs[&3].velocity.y + 0.03 * 0.98).abs() < 1e-9);
        let taken = store.take(2).unwrap();
        assert!((taken.x - 8.025).abs() < 1e-9);
        assert!(store.take(2).is_none());
    }
}
