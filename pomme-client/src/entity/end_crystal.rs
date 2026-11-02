//! End crystals (vanilla `EndCrystal`): not living, so outside the living
//! store, but drawn by the entity renderer from vanilla's own model layer.
//! The client only needs where they are, how long they've existed (their
//! spin and bob) and whether they show their bedrock base.

use std::collections::HashMap;

use glam::DVec3;

use crate::entity::MetaValue;
use crate::entity::components::Position;

/// `EndCrystal.DATA_SHOW_BOTTOM`.
const SHOW_BOTTOM: u8 = 9;

pub struct EndCrystal {
    pub position: Position,
    pub prev_position: Position,
    /// `EndCrystal.time`, the ticks it has existed.
    pub age: u32,
    pub shows_bottom: bool,
}

#[derive(Default)]
pub struct EndCrystalStore {
    pub crystals: HashMap<i32, EndCrystal>,
}

impl EndCrystalStore {
    pub fn spawn(&mut self, id: i32, position: Position) {
        self.crystals.insert(
            id,
            EndCrystal {
                position,
                prev_position: position,
                // Vanilla starts each crystal at a random point in its spin.
                age: fastrand::u32(..100_000),
                shows_bottom: true,
            },
        );
    }

    pub fn move_delta(&mut self, id: i32, delta: DVec3) {
        if let Some(c) = self.crystals.get_mut(&id) {
            c.position = (DVec3::from(c.position) + delta).into();
        }
    }

    pub fn teleport(&mut self, id: i32, position: Position) {
        if let Some(c) = self.crystals.get_mut(&id) {
            c.position = position;
        }
    }

    pub fn apply_entity_data(&mut self, id: i32, index: u8, value: MetaValue) {
        if let (Some(c), SHOW_BOTTOM, MetaValue::Bool(shows)) =
            (self.crystals.get_mut(&id), index, value)
        {
            c.shows_bottom = shows;
        }
    }

    pub fn tick(&mut self) {
        for c in self.crystals.values_mut() {
            c.prev_position = c.position;
            c.age = c.age.wrapping_add(1);
        }
    }

    pub fn remove(&mut self, ids: &[i32]) {
        for id in ids {
            self.crystals.remove(id);
        }
    }

    pub fn clear(&mut self) {
        self.crystals.clear();
    }
}
