//! Interaction entities (vanilla `Interaction`): invisible boxes servers
//! put over display entities and other props so they can be clicked. They
//! draw nothing; the client only needs their box for the crosshair, and
//! whether a use swings the arm.

use std::collections::HashMap;

use glam::DVec3;

use crate::entity::MetaValue;
use crate::entity::components::Position;
use crate::physics::aabb::Aabb;

/// `Interaction.DATA_WIDTH_ID`, `DATA_HEIGHT_ID` and `DATA_RESPONSE_ID`.
const WIDTH: u8 = 8;
const HEIGHT: u8 = 9;
const RESPONSE: u8 = 10;

pub struct Interaction {
    pub position: Position,
    /// `EntityDimensions.scalable(width, height)`: the box is centred on the
    /// position in x and z and stands on it.
    pub width: f32,
    pub height: f32,
    /// `getResponse`: a use swings the arm (`SUCCESS`) rather than only
    /// consuming the click (`CONSUME`).
    pub response: bool,
    /// The entity it rides, if any: furniture plugins seat the box on the
    /// display it covers.
    pub vehicle: Option<i32>,
}

impl Interaction {
    pub fn bounding_box(&self) -> Aabb {
        Aabb::from_center(
            self.position.into(),
            f64::from(self.width) * 0.5,
            f64::from(self.height) * 0.5,
        )
    }
}

#[derive(Default)]
pub struct InteractionStore {
    pub entities: HashMap<i32, Interaction>,
}

impl InteractionStore {
    pub fn spawn(&mut self, id: i32, position: Position) {
        // The entity type's own 1 x 1 until the data says otherwise.
        self.entities.insert(
            id,
            Interaction {
                position,
                width: 1.0,
                height: 1.0,
                response: false,
                vehicle: None,
            },
        );
    }

    pub fn move_delta(&mut self, id: i32, delta: DVec3) {
        if let Some(e) = self.entities.get_mut(&id) {
            e.position = (DVec3::from(e.position) + delta).into();
        }
    }

    pub fn teleport(&mut self, id: i32, position: Position) {
        if let Some(e) = self.entities.get_mut(&id) {
            e.position = position;
        }
    }

    pub fn apply_entity_data(&mut self, id: i32, index: u8, value: MetaValue) {
        let Some(e) = self.entities.get_mut(&id) else {
            return;
        };
        match (index, value) {
            (WIDTH, MetaValue::Float(w)) => e.width = w,
            (HEIGHT, MetaValue::Float(h)) => e.height = h,
            (RESPONSE, MetaValue::Bool(r)) => e.response = r,
            _ => {}
        }
    }

    pub fn set_passengers(&mut self, vehicle: i32, passengers: &[i32]) {
        for (id, e) in &mut self.entities {
            if passengers.contains(id) {
                e.vehicle = Some(vehicle);
            } else if e.vehicle == Some(vehicle) {
                e.vehicle = None;
            }
        }
    }

    /// Riders follow their vehicle's riding position
    /// (`Entity.positionRider`); `seat` gives it for a vehicle id.
    pub fn tick(&mut self, seat: impl Fn(i32) -> Option<DVec3>) {
        for e in self.entities.values_mut() {
            if let Some(position) = e.vehicle.and_then(&seat) {
                e.position = position.into();
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_box_follows_the_synced_size_and_moves() {
        let mut store = InteractionStore::default();
        store.spawn(7, DVec3::new(10.0, 64.0, -3.0).into());
        store.apply_entity_data(7, WIDTH, MetaValue::Float(2.0));
        store.apply_entity_data(7, HEIGHT, MetaValue::Float(0.5));
        store.apply_entity_data(7, RESPONSE, MetaValue::Bool(true));
        store.move_delta(7, DVec3::new(0.0, 1.0, 0.0));
        let e = &store.entities[&7];
        assert!(e.response);
        let aabb = e.bounding_box();
        assert_eq!(aabb.min, DVec3::new(9.0, 65.0, -4.0));
        assert_eq!(aabb.max, DVec3::new(11.0, 65.5, -2.0));

        // Seated on a vehicle, it follows the vehicle's seat.
        store.set_passengers(3, &[7]);
        store.tick(|vehicle| (vehicle == 3).then_some(DVec3::new(1.0, 2.0, 3.0)));
        assert_eq!(
            DVec3::from(store.entities[&7].position),
            DVec3::new(1.0, 2.0, 3.0)
        );
        store.set_passengers(3, &[]);
        assert_eq!(store.entities[&7].vehicle, None);
    }
}
