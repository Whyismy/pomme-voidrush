//! The rate the server ticks at (vanilla `TickRateManager`, the client's
//! side): `/tick rate` slows the client's own ticks with a slower server,
//! `/tick freeze` holds everything but players, and `/tick step` lets a
//! frozen game run a number of ticks.

/// The slowest a server may tick (`TickRateManager.MIN_TICKRATE`).
const MIN_RATE: f32 = 1.0;

#[derive(Clone, Copy, Debug)]
pub struct TickRate {
    rate: f32,
    frozen: bool,
    /// Ticks a frozen game still has to run.
    steps: u32,
    /// Whether the tick now running moves the game on (`runGameElements`).
    running: bool,
}

impl Default for TickRate {
    fn default() -> Self {
        Self {
            rate: 20.0,
            frozen: false,
            steps: 0,
            running: true,
        }
    }
}

impl TickRate {
    /// `ClientPacketListener.handleTickingState`.
    pub fn set_state(&mut self, rate: f32, frozen: bool) {
        self.rate = rate.max(MIN_RATE);
        self.frozen = frozen;
        self.running = !frozen || self.steps > 0;
    }

    /// `handleTickingStep`.
    pub fn set_steps(&mut self, steps: u32) {
        self.steps = steps;
        self.running = !self.frozen || steps > 0;
    }

    /// `TickRateManager.tick`, at the start of a client tick.
    pub fn tick(&mut self) {
        self.running = !self.frozen || self.steps > 0;
        self.steps = self.steps.saturating_sub(1);
    }

    /// Whether the game moves on (`runsNormally`).
    pub fn runs_normally(&self) -> bool {
        self.running
    }

    /// `Minecraft.getTickTargetMillis`: how long a client tick is. A
    /// slower server stretches it; a faster one, or a frozen one, leaves
    /// it at a twentieth of a second.
    pub fn seconds_per_tick(&self) -> f32 {
        if self.running {
            (1.0 / self.rate).max(0.05)
        } else {
            0.05
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A slow server stretches the client's ticks, a fast one does not, a
    /// frozen one holds the game, and steps run it on for a while.
    #[test]
    fn the_client_keeps_a_slow_or_frozen_servers_time() {
        let mut rate = TickRate::default();
        assert!(rate.runs_normally());
        assert_eq!(rate.seconds_per_tick(), 0.05);

        rate.set_state(10.0, false);
        assert_eq!(rate.seconds_per_tick(), 0.1);
        rate.set_state(100.0, false);
        assert_eq!(rate.seconds_per_tick(), 0.05);
        rate.set_state(0.1, false);
        assert_eq!(rate.seconds_per_tick(), 1.0);

        rate.set_state(10.0, true);
        assert!(!rate.runs_normally());
        assert_eq!(rate.seconds_per_tick(), 0.05);
        rate.set_steps(2);
        assert!(rate.runs_normally());
        rate.tick();
        assert!(rate.runs_normally());
        rate.tick();
        assert!(rate.runs_normally());
        rate.tick();
        assert!(!rate.runs_normally());
    }
}
