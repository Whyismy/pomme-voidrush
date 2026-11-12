//! Leads (vanilla `Leashable` and `EntityRenderer.renderLeash`): which mob
//! is tied to what, and the sagging rope drawn between them.

use std::collections::HashMap;

use glam::DVec3;

/// The rope is drawn in this many pieces.
pub const SEGMENTS: usize = 24;

#[derive(Default)]
pub struct LeashStore {
    /// Each leashed entity and what holds it.
    pub holders: HashMap<i32, i32>,
}

impl LeashStore {
    /// `ClientboundSetEntityLinkPacket`: a holder of 0 lets go.
    pub fn set(&mut self, id: i32, holder: i32) {
        if holder == 0 {
            self.holders.remove(&id);
        } else {
            self.holders.insert(id, holder);
        }
    }

    /// Entities left the world: their own leads go, and those they held.
    pub fn remove(&mut self, ids: &[i32]) {
        self.holders
            .retain(|id, holder| !ids.contains(id) && !ids.contains(holder));
    }

    pub fn clear(&mut self) {
        self.holders.clear();
    }
}

/// The points of a rope from `start` to `end` (`addVertexPair`): straight
/// across, but hanging below the straight line, steepest at its higher end.
pub fn rope_points(start: DVec3, end: DVec3) -> [DVec3; SEGMENTS + 1] {
    let d = end - start;
    std::array::from_fn(|i| {
        let f = i as f64 / SEGMENTS as f64;
        let y = if d.y > 0.0 {
            d.y * f * f
        } else {
            d.y - d.y * (1.0 - f) * (1.0 - f)
        };
        start + DVec3::new(d.x * f, y, d.z * f)
    })
}

/// Where the lead leaves a mob, from its feet (`Entity.getLeashOffset`, as
/// `MobRenderer` turns it): at eye height, four tenths of its width ahead.
pub fn leash_offset(eye_height: f32, width: f32, body_yaw_deg: f32) -> DVec3 {
    let turn = f64::from(body_yaw_deg).to_radians() + std::f64::consts::FRAC_PI_2;
    let ahead = f64::from(width) * 0.4;
    DVec3::new(
        turn.cos() * ahead,
        f64::from(eye_height),
        turn.sin() * ahead,
    )
}

/// `Vec3.yRot`.
fn y_rot(v: DVec3, angle: f64) -> DVec3 {
    let (sin, cos) = angle.sin_cos();
    DVec3::new(v.x * cos + v.z * sin, v.y, v.z * cos - v.x * sin)
}

/// `Vec3.xRot`.
fn x_rot(v: DVec3, angle: f64) -> DVec3 {
    let (sin, cos) = angle.sin_cos();
    DVec3::new(v.x, v.y * cos + v.z * sin, v.z * cos - v.y * sin)
}

/// `Player.getRopeHoldPosition`: at the right hand of a standing player, a
/// block below the top of its box.
// TODO: left-handed, swimming and gliding players hold it elsewhere.
pub fn player_hold(feet: DVec3, height: f64, body_yaw_deg: f32, crouching: bool) -> DVec3 {
    let forward = if crouching { -0.2 } else { 0.07 };
    feet + y_rot(
        DVec3::new(-0.22, height - 1.0, forward),
        -f64::from(body_yaw_deg).to_radians(),
    )
}

/// `LocalPlayer.getRopeHoldPosition` in first person: down and to the right
/// of the eye, turned with the view.
pub fn first_person_hold(eye: DVec3, y_rot_deg: f32, x_rot_deg: f32) -> DVec3 {
    let hand = x_rot(
        DVec3::new(-0.39, -0.6, 0.3),
        -f64::from(x_rot_deg).to_radians(),
    );
    eye + y_rot(hand, -f64::from(y_rot_deg).to_radians())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rope starts and ends where it is tied and hangs below the straight
    /// line between, whichever end is higher.
    #[test]
    fn a_rope_sags_between_its_ends() {
        for (start, end) in [
            (DVec3::new(0.0, 64.0, 0.0), DVec3::new(4.0, 66.0, 2.0)),
            (DVec3::new(0.0, 66.0, 0.0), DVec3::new(4.0, 64.0, 2.0)),
        ] {
            let points = rope_points(start, end);
            assert_eq!(points[0], start);
            assert!((points[SEGMENTS] - end).length() < 1e-9);
            let middle = points[SEGMENTS / 2];
            assert_eq!((middle.x, middle.z), (2.0, 1.0));
            assert!((middle.y - 64.5).abs() < 1e-9, "a quarter of the way up");
        }
    }

    #[test]
    fn leads_are_tied_and_let_go() {
        let mut store = LeashStore::default();
        store.set(1, 10);
        store.set(2, 10);
        store.set(3, 11);
        store.set(3, 0);
        assert_eq!(store.holders.len(), 2);
        // The holder leaving frees both.
        store.remove(&[10]);
        assert!(store.holders.is_empty());
    }

    /// A mob facing south (yaw 0) is tied a little south of its middle; a
    /// player facing south holds the rope at its right hand, to the west.
    #[test]
    fn the_rope_ends_at_the_mob_and_the_hand() {
        let offset = leash_offset(1.3, 0.9, 0.0);
        assert!(offset.x.abs() < 1e-9 && (offset.z - 0.36).abs() < 1e-6);
        assert!((offset.y - 1.3).abs() < 1e-6);
        let hand = player_hold(DVec3::ZERO, 1.8, 0.0, false);
        assert!((hand - DVec3::new(-0.22, 0.8, 0.07)).length() < 1e-9);
        // Turned to face west (yaw 90), the hand is to the north.
        let hand = player_hold(DVec3::ZERO, 1.8, 90.0, false);
        assert!((hand - DVec3::new(-0.07, 0.8, -0.22)).length() < 1e-9);
        let first = first_person_hold(DVec3::new(0.0, 1.62, 0.0), 0.0, 0.0);
        assert!((first - DVec3::new(-0.39, 1.02, 0.3)).length() < 1e-9);
    }
}
