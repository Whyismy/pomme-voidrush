//! Vanilla `EnderDragon`'s client side: the wing-flap clock and the flight
//! history (`DragonFlightHistory`) its neck and tail trail along, plus the
//! synced phase that seats it.

use glam::DVec3;

/// `DragonFlightHistory.LENGTH`.
const HISTORY: usize = 64;

/// `EnderDragon.DATA_PHASE`.
pub const PHASE_DATA: u8 = 16;

#[derive(Clone)]
pub struct DragonState {
    pub flap_time: f32,
    pub prev_flap_time: f32,
    /// `(y, yRot)` a tick apart, `head` the newest; `None` until the first
    /// record fills it.
    history: Option<([(f64, f32); HISTORY], usize)>,
    /// The `EnderDragonPhase` id.
    pub phase: i32,
}

impl Default for DragonState {
    fn default() -> Self {
        Self {
            flap_time: 0.0,
            prev_flap_time: 0.0,
            history: None,
            // `HOLDING_PATTERN`.
            phase: 0,
        }
    }
}

impl DragonState {
    /// `AbstractDragonSittingPhase`: flaming, scanning, attacking.
    pub fn is_sitting(&self) -> bool {
        matches!(self.phase, 5..=7)
    }

    /// `LANDING` or `TAKEOFF`.
    pub fn is_landing_or_taking_off(&self) -> bool {
        matches!(self.phase, 3 | 4)
    }

    /// `EnderDragon.aiStep` on the client: the flap advances by how fast it
    /// flies (faster gliding flaps slower, climbing flaps harder), and the
    /// flight history records this tick unless it has no AI.
    pub fn tick(&mut self, y: f64, y_rot: f32, velocity: DVec3, no_ai: bool, dying: bool) {
        self.prev_flap_time = self.flap_time;
        if dying {
            return;
        }
        let horizontal = (velocity.x * velocity.x + velocity.z * velocity.z).sqrt() as f32;
        let mut step = 0.2 / (horizontal * 10.0 + 1.0);
        step *= 2.0_f64.powf(velocity.y) as f32;
        // TODO: half speed while `inWall`.
        self.flap_time += if self.is_sitting() { 0.1 } else { step };
        if no_ai {
            self.flap_time = 0.5;
        } else {
            self.record(y, wrap_degrees(y_rot));
        }
    }

    /// `DragonFlightHistory.record`.
    fn record(&mut self, y: f64, y_rot: f32) {
        let (samples, head) = self
            .history
            .get_or_insert(([(y, y_rot); HISTORY], HISTORY - 1));
        *head = (*head + 1) % HISTORY;
        samples[*head] = (y, y_rot);
    }

    /// `DragonFlightHistory.get(index, partialTick)`: the sample `index`
    /// ticks back, eased from the one before it.
    pub fn sample(&self, index: usize, partial_tick: f32) -> (f64, f32) {
        let Some((samples, head)) = &self.history else {
            return (0.0, 0.0);
        };
        let at = |i: usize| samples[(head + HISTORY - i % HISTORY) % HISTORY];
        let (y, y_rot) = at(index);
        let (y1, y_rot1) = at(index + 1);
        (
            y1 + (y - y1) * f64::from(partial_tick),
            y_rot1 + wrap_degrees(y_rot - y_rot1) * partial_tick,
        )
    }
}

/// `Mth.wrapDegrees`.
pub fn wrap_degrees(degrees: f32) -> f32 {
    let wrapped = degrees % 360.0;
    if wrapped >= 180.0 {
        wrapped - 360.0
    } else if wrapped < -180.0 {
        wrapped + 360.0
    } else {
        wrapped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_starts_full_and_reads_back_in_order() {
        let mut dragon = DragonState::default();
        dragon.tick(10.0, 0.0, DVec3::ZERO, false, false);
        // The first record fills every sample.
        assert_eq!(dragon.sample(40, 1.0), (10.0, 0.0));
        dragon.tick(11.0, 90.0, DVec3::ZERO, false, false);
        dragon.tick(12.0, 180.0, DVec3::ZERO, false, false);
        assert_eq!(dragon.sample(0, 1.0), (12.0, 180.0));
        assert_eq!(dragon.sample(1, 1.0), (11.0, 90.0));
        // Halfway between a tick back and two.
        assert_eq!(dragon.sample(1, 0.5), (10.5, 45.0));
    }

    #[test]
    fn flap_speeds_up_while_slow_and_climbing() {
        let mut gliding = DragonState::default();
        gliding.tick(0.0, 0.0, DVec3::new(1.0, 0.0, 0.0), false, false);
        let mut hovering = DragonState::default();
        hovering.tick(0.0, 0.0, DVec3::ZERO, false, false);
        assert!(hovering.flap_time > gliding.flap_time);
        assert!((hovering.flap_time - 0.2).abs() < 1e-6);
        let mut parked = DragonState::default();
        parked.tick(0.0, 0.0, DVec3::ZERO, true, false);
        assert_eq!(parked.flap_time, 0.5);
    }
}
