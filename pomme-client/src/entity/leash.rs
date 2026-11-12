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
/// across, and, when `slack`, hanging below the straight line, steepest at
/// its higher end. A taut one (one of the four a happy ghast tows by) runs
/// straight.
pub fn rope_points(start: DVec3, end: DVec3, slack: bool) -> [DVec3; SEGMENTS + 1] {
    let d = end - start;
    std::array::from_fn(|i| {
        let f = i as f64 / SEGMENTS as f64;
        let y = if !slack {
            d.y * f
        } else if d.y > 0.0 {
            d.y * f * f
        } else {
            d.y - d.y * (1.0 - f) * (1.0 - f)
        };
        start + DVec3::new(d.x * f, y, d.z * f)
    })
}

/// `Vec3.yRot(-yaw)`: an offset in an entity's own frame, turned to face
/// the way its body does.
pub fn turned(offset: DVec3, body_yaw_deg: f32) -> DVec3 {
    y_rot(offset, -f64::from(body_yaw_deg).to_radians())
}

/// `Leashable.createQuadLeashOffsets`: the four corners something is
/// towed by (or tows by), from shares of its width and of its height.
pub fn quad_offsets(width: f32, height: f32, [forward, across, side, up]: [f64; 4]) -> [DVec3; 4] {
    let (w, h) = (f64::from(width), f64::from(height));
    let (forward, across, side, up) = (forward * w, across * w, side * w, up * h);
    [
        DVec3::new(-side, up, across + forward),
        DVec3::new(-side, up, -across + forward),
        DVec3::new(side, up, -across + forward),
        DVec3::new(side, up, across + forward),
    ]
}

/// A happy ghast's shares for [`quad_offsets`] as what tows
/// (`HappyGhast.getQuadLeashHolderOffsets`).
pub const GHAST_HOLDS: [f64; 4] = [-0.03125, 0.4375, 0.46875, 0.03125];
/// What is towed by four ropes: a boat's shares, a camel's, a sniffer's
/// and any other horse's (`getQuadLeashOffsets`).
pub const BOAT_TOWED: [f64; 4] = [0.0, 0.64, 0.382, 0.88];
pub const CAMEL_TOWED: [f64; 4] = [0.02, 0.48, 0.25, 0.82];
pub const SNIFFER_TOWED: [f64; 4] = [-0.01, 0.63, 0.38, 1.15];
pub const HORSE_TOWED: [f64; 4] = [0.04, 0.52, 0.23, 0.87];

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
            let points = rope_points(start, end, true);
            assert_eq!(points[0], start);
            assert!((points[SEGMENTS] - end).length() < 1e-9);
            let middle = points[SEGMENTS / 2];
            assert_eq!((middle.x, middle.z), (2.0, 1.0));
            assert!((middle.y - 64.5).abs() < 1e-9, "a quarter of the way up");
        }
    }

    /// A taut rope runs straight, and the four a boat is towed by leave
    /// its corners: 0.382 of its width to each side, 0.64 fore and aft,
    /// 0.88 of its height up.
    #[test]
    fn a_towed_boat_hangs_by_four_straight_ropes() {
        let points = rope_points(DVec3::ZERO, DVec3::new(0.0, 8.0, 4.0), false);
        assert!((points[SEGMENTS / 2] - DVec3::new(0.0, 4.0, 2.0)).length() < 1e-9);
        let corners = quad_offsets(1.375, 0.5625, BOAT_TOWED);
        assert!((corners[0] - DVec3::new(-0.52525, 0.495, 0.88)).length() < 1e-6);
        assert!((corners[2] - DVec3::new(0.52525, 0.495, -0.88)).length() < 1e-6);
        // Turned a quarter round, forward (+z) is toward -x.
        let ahead = turned(DVec3::Z, 90.0);
        assert!((ahead - DVec3::NEG_X).length() < 1e-9);
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
        // (`Leashable.getLeashOffset`: at eye height, four tenths of its
        // width ahead.)
        let offset = turned(DVec3::new(0.0, 1.3, 0.9 * 0.4), 0.0);
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
