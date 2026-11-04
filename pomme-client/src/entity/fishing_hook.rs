//! Fishing bobbers (vanilla `FishingHook` and `FishingHookRenderer`): where
//! each is, eased between the server's updates, and whose rod its line runs
//! to.

use std::collections::HashMap;

use azalea_registry::builtin::EntityKind;
use glam::DVec3;

/// Ticks a server update eases over (vanilla `InterpolationHandler`'s
/// default).
const LERP_STEPS: u32 = 3;

pub struct FishingHook {
    /// The angler's entity id (the hook's spawn data).
    pub owner: i32,
    pub position: DVec3,
    pub prev_position: DVec3,
    /// Where the server last put it: relative moves build on this.
    server_position: DVec3,
    steps: u32,
}

impl FishingHook {
    /// The position `partial_tick` of the way into this tick.
    pub fn lerped(&self, partial_tick: f32) -> DVec3 {
        self.prev_position
            .lerp(self.position, f64::from(partial_tick))
    }
}

pub fn is_fishing_hook(kind: EntityKind) -> bool {
    kind == EntityKind::FishingBobber
}

#[derive(Default)]
pub struct FishingHookStore {
    pub hooks: HashMap<i32, FishingHook>,
}

impl FishingHookStore {
    pub fn spawn(&mut self, id: i32, owner: i32, position: DVec3) {
        self.hooks.insert(
            id,
            FishingHook {
                owner,
                position,
                prev_position: position,
                server_position: position,
                steps: 0,
            },
        );
    }

    pub fn move_delta(&mut self, id: i32, delta: DVec3) {
        if let Some(hook) = self.hooks.get_mut(&id) {
            hook.server_position += delta;
            hook.steps = LERP_STEPS;
        }
    }

    pub fn teleport(&mut self, id: i32, position: DVec3) {
        if let Some(hook) = self.hooks.get_mut(&id) {
            hook.server_position = position;
            hook.steps = LERP_STEPS;
        }
    }

    /// Eases each hook a step toward where the server has it.
    pub fn tick(&mut self) {
        for hook in self.hooks.values_mut() {
            hook.prev_position = hook.position;
            if hook.steps > 0 {
                hook.position += (hook.server_position - hook.position) / f64::from(hook.steps);
                hook.steps -= 1;
            }
        }
    }

    /// Whether `owner` has a hook out (vanilla `Player.fishing != null`).
    pub fn has_hook(&self, owner: i32) -> bool {
        self.hooks.values().any(|hook| hook.owner == owner)
    }

    pub fn remove(&mut self, ids: &[i32]) {
        for id in ids {
            self.hooks.remove(id);
        }
    }

    pub fn clear(&mut self) {
        self.hooks.clear();
    }
}

/// Vanilla `FishingHookRenderer`'s line: 16 segments from 0.25 above the
/// hook to the rod tip, sagging as `(a * a + a) / 2` of the drop.
pub fn line_segments(hook: DVec3, rod_tip: DVec3) -> Vec<(DVec3, DVec3)> {
    let origin = hook + DVec3::new(0.0, 0.25, 0.0);
    let offset = rod_tip - origin;
    let point =
        |a: f64| origin + DVec3::new(offset.x * a, offset.y * (a * a + a) * 0.5, offset.z * a);
    (0..16)
        .map(|i| (point(f64::from(i) / 16.0), point(f64::from(i + 1) / 16.0)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_runs_from_above_the_hook_to_the_rod() {
        let hook = DVec3::new(0.0, 60.0, 0.0);
        let tip = DVec3::new(4.0, 63.0, 2.0);
        let segments = line_segments(hook, tip);
        assert_eq!(segments.len(), 16);
        assert!((segments[0].0 - DVec3::new(0.0, 60.25, 0.0)).length() < 1e-9);
        assert!((segments[15].1 - tip).length() < 1e-9);
        // It sags: halfway along, below the straight line's midpoint.
        let mid = segments[8].0;
        assert!(mid.y < (60.25 + 63.0) / 2.0);
    }

    #[test]
    fn a_hook_eases_to_the_server_position() {
        let mut store = FishingHookStore::default();
        store.spawn(1, 7, DVec3::ZERO);
        store.move_delta(1, DVec3::new(3.0, 0.0, 0.0));
        for _ in 0..3 {
            store.tick();
        }
        assert!((store.hooks[&1].position.x - 3.0).abs() < 1e-9);
        assert!(store.has_hook(7));
        assert!(!store.has_hook(8));
    }
}
