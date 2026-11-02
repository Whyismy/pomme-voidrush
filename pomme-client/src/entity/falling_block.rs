//! Falling blocks (vanilla `FallingBlockEntity`): the block state rides in
//! the spawn packet's data, and the client runs the entity's own movement
//! between the server's position updates, which come only once a second.
//! The server places the block when it lands and removes the entity.

use std::collections::HashMap;

use glam::DVec3;

use crate::entity::components::Position;
use crate::physics::aabb::Aabb;
use crate::physics::block_shape::CollisionContext;
use crate::physics::collision::{CollisionWorld, resolve_collision};
use crate::position::ChunkPos;
use crate::world::block::BlockStateTable;
use crate::world::chunk::ChunkStore;

/// Vanilla `FallingBlockEntity.getDefaultGravity`.
const GRAVITY: f64 = 0.04;
/// The `scale(0.98)` at the end of `FallingBlockEntity.tick`.
const DRAG: f64 = 0.98;
/// `EntityType.FALLING_BLOCK` is 0.98 wide and tall.
const HALF_SIZE: f64 = 0.49;

pub struct FallingBlock {
    /// In the wire version's block-state space, like the chunks'.
    pub state: u32,
    pub position: Position,
    pub prev_position: Position,
    velocity: DVec3,
    on_ground: bool,
    /// `Entity.DATA_NO_GRAVITY`: how plugins hang a block in the air.
    no_gravity: bool,
}

#[derive(Default)]
pub struct FallingBlockStore {
    pub blocks: HashMap<i32, FallingBlock>,
}

impl FallingBlockStore {
    /// Vanilla `FallingBlockEntity.recreateFromPacket`; air (an unknown
    /// state) discards itself on its first tick, so it isn't kept.
    pub fn spawn(&mut self, id: i32, state: i32, position: Position, velocity: DVec3) {
        let Ok(state) = u32::try_from(state) else {
            return;
        };
        if state == 0 {
            return;
        }
        self.blocks.insert(
            id,
            FallingBlock {
                state,
                position,
                prev_position: position,
                velocity,
                on_ground: false,
                no_gravity: false,
            },
        );
    }

    pub fn move_delta(&mut self, id: i32, delta: DVec3, on_ground: bool) {
        if let Some(block) = self.blocks.get_mut(&id) {
            block.position += delta;
            block.on_ground = on_ground;
        }
    }

    pub fn teleport(&mut self, id: i32, position: Position, velocity: Option<DVec3>) {
        if let Some(block) = self.blocks.get_mut(&id) {
            if block.position.distance_squared(*position) > 4096.0 {
                block.prev_position = position;
            }
            block.position = position;
            if let Some(velocity) = velocity {
                block.velocity = velocity;
            }
        }
    }

    pub fn set_motion(&mut self, id: i32, velocity: DVec3) {
        if let Some(block) = self.blocks.get_mut(&id) {
            block.velocity = velocity;
        }
    }

    pub fn set_no_gravity(&mut self, id: i32, no_gravity: bool) {
        if let Some(block) = self.blocks.get_mut(&id) {
            block.no_gravity = no_gravity;
        }
    }

    pub fn remove(&mut self, ids: &[i32]) {
        for id in ids {
            self.blocks.remove(id);
        }
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
    }

    /// `FallingBlockEntity.tick` as a client runs it: gravity, a colliding
    /// move, then drag.
    pub fn tick(&mut self, chunks: &ChunkStore, table: BlockStateTable) {
        for block in self.blocks.values_mut() {
            block.prev_position = block.position;
            let at = block.position.floor().as_ivec3();
            if chunks
                .get_chunk(&ChunkPos::new(at.x.div_euclid(16), at.z.div_euclid(16)))
                .is_none()
            {
                continue;
            }
            if !block.no_gravity {
                block.velocity.y -= GRAVITY;
            }
            let aabb = Aabb::from_center(*block.position, HALF_SIZE, HALF_SIZE);
            let ctx = CollisionContext::entity(block.position.y, false, false);
            let (delta, on_ground) = resolve_collision(
                CollisionWorld::new(chunks, table),
                aabb,
                block.velocity.into(),
                0.0,
                block.on_ground,
                &ctx,
            );
            block.position += delta;
            block.on_ground = on_ground;
            if on_ground {
                block.velocity.y = 0.0;
            }
            block.velocity *= DRAG;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn air_never_spawns_and_a_block_keeps_its_state() {
        let mut store = FallingBlockStore::default();
        store.spawn(1, 0, Position::new(0.5, 70.0, 0.5), DVec3::ZERO);
        store.spawn(2, -3, Position::new(0.5, 70.0, 0.5), DVec3::ZERO);
        store.spawn(3, 118, Position::new(0.5, 70.0, 0.5), DVec3::ZERO);
        assert_eq!(store.blocks.len(), 1);
        assert_eq!(store.blocks[&3].state, 118);
        store.move_delta(3, DVec3::new(0.0, -1.0, 0.0), false);
        assert_eq!(*store.blocks[&3].position, DVec3::new(0.5, 69.0, 0.5));
        store.remove(&[3]);
        assert!(store.blocks.is_empty());
    }
}
