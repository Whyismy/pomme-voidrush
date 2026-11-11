//! Lightning bolts (vanilla `LightningBolt` on the client): a bolt lives a
//! couple of ticks, thunders once, lights the sky while it's live, and
//! strikes again a few times in a new shape before it goes.

use std::collections::HashMap;

use glam::DVec3;

/// `LightningBolt.START_LIFE`.
const START_LIFE: i32 = 2;

/// The ticks the sky stays lit after a live bolt's tick
/// (`ClientLevel.setSkyFlashTime(2)`).
pub const SKY_FLASH_TICKS: u32 = 2;

pub struct LightningBolt {
    pub position: DVec3,
    life: i32,
    /// Strikes left after this one.
    flashes: u32,
    /// The bolt's shape, new with each strike.
    pub seed: u64,
}

/// What a tick of the bolts did.
#[derive(Default)]
pub struct LightningTick {
    /// Bolts that thundered this tick: the thunder and the impact play there.
    pub thunder: Vec<DVec3>,
    /// A bolt was live, which lights the sky.
    pub flash: bool,
}

#[derive(Default)]
pub struct LightningStore {
    pub bolts: HashMap<i32, LightningBolt>,
}

impl LightningStore {
    pub fn spawn(&mut self, id: i32, position: DVec3) {
        self.bolts.insert(
            id,
            LightningBolt {
                position,
                life: START_LIFE,
                flashes: fastrand::u32(1..=3),
                seed: fastrand::u64(..),
            },
        );
    }

    /// `LightningBolt.tick`.
    pub fn tick(&mut self) -> LightningTick {
        let mut tick = LightningTick::default();
        self.bolts.retain(|_, bolt| {
            if bolt.life == START_LIFE {
                tick.thunder.push(bolt.position);
            }
            bolt.life -= 1;
            if bolt.life < 0 {
                if bolt.flashes == 0 {
                    return false;
                }
                if bolt.life < -fastrand::i32(0..10) {
                    bolt.flashes -= 1;
                    bolt.life = 1;
                    bolt.seed = fastrand::u64(..);
                }
            }
            tick.flash |= bolt.life >= 0;
            true
        });
        tick
    }

    pub fn remove(&mut self, ids: &[i32]) {
        for id in ids {
            self.bolts.remove(id);
        }
    }

    pub fn clear(&mut self) {
        self.bolts.clear();
    }
}

/// `java.util.Random`, which vanilla shapes a bolt with.
struct JavaRandom(u64);

impl JavaRandom {
    const MASK: u64 = (1 << 48) - 1;

    fn new(seed: u64) -> Self {
        Self((seed ^ 0x5_DEEC_E66D) & Self::MASK)
    }

    fn next(&mut self, bits: u32) -> i32 {
        self.0 = (self.0.wrapping_mul(0x5_DEEC_E66D).wrapping_add(0xB)) & Self::MASK;
        (self.0 >> (48 - bits)) as i32
    }

    /// `nextInt(bound)` for a bound that isn't a power of two.
    fn next_int(&mut self, bound: i32) -> i32 {
        loop {
            let bits = self.next(31);
            let value = bits % bound;
            if bits.wrapping_sub(value).wrapping_add(bound - 1) >= 0 {
                return value;
            }
        }
    }
}

/// The glow's colour and how much of it each layer adds.
pub const BOLT_COLOR: [f32; 4] = [0.45, 0.45, 0.5, 0.3];

/// Vanilla `LightningBoltRenderer.submit`: the bolt's quads, relative to
/// where it strikes. A jagged trunk 128 blocks tall and two branches, each
/// drawn as four nested square tubes, the trunk widening toward the top.
pub fn bolt_quads(seed: u64) -> Vec<[[f32; 3]; 4]> {
    let mut x_offsets = [0.0f32; 8];
    let mut z_offsets = [0.0f32; 8];
    let (mut x_total, mut z_total) = (0.0f32, 0.0f32);
    let mut random = JavaRandom::new(seed);
    for h in (0..8).rev() {
        x_offsets[h] = x_total;
        z_offsets[h] = z_total;
        x_total += (random.next_int(11) - 5) as f32;
        z_total += (random.next_int(11) - 5) as f32;
    }

    let mut quads = Vec::with_capacity(224);
    for layer in 0..4 {
        let mut random = JavaRandom::new(seed);
        for part in 0..3usize {
            let top = if part > 0 { 7 - part } else { 7 };
            let bottom = if part > 0 { top - 2 } else { 0 };
            let mut x0 = x_offsets[top] - x_total;
            let mut z0 = z_offsets[top] - z_total;
            for h in (bottom..=top).rev() {
                let (x1, z1) = (x0, z0);
                let spread = if part == 0 { 11 } else { 31 };
                x0 += (random.next_int(spread) - spread / 2) as f32;
                z0 += (random.next_int(spread) - spread / 2) as f32;
                let mut upper = 0.1 + layer as f32 * 0.2;
                let mut lower = upper;
                if part == 0 {
                    upper *= h as f32 * 0.1 + 1.0;
                    lower *= (h as f32 - 1.0) * 0.1 + 1.0;
                }
                let (y0, y1) = (h as f32 * 16.0, (h + 1) as f32 * 16.0);
                let side = |plus: bool, radius: f32| if plus { radius } else { -radius };
                for (px1, pz1, px2, pz2) in [
                    (false, false, true, false),
                    (true, false, true, true),
                    (true, true, false, true),
                    (false, true, false, false),
                ] {
                    quads.push([
                        [x0 + side(px1, lower), y0, z0 + side(pz1, lower)],
                        [x1 + side(px1, upper), y1, z1 + side(pz1, upper)],
                        [x1 + side(px2, upper), y1, z1 + side(pz2, upper)],
                        [x0 + side(px2, lower), y0, z0 + side(pz2, lower)],
                    ]);
                }
            }
        }
    }
    quads
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `java.util.Random(42)`'s first draws.
    #[test]
    fn the_shape_random_is_javas() {
        let mut random = JavaRandom::new(42);
        assert_eq!(random.next(32), -1170105035);
        let mut random = JavaRandom::new(42);
        assert_eq!(
            [
                random.next_int(11),
                random.next_int(11),
                random.next_int(31)
            ],
            [7, 4, 3]
        );
    }

    /// The trunk comes down to the strike point and reaches 128 blocks up;
    /// every layer shares the path.
    #[test]
    fn a_bolt_lands_where_it_strikes() {
        let quads = bolt_quads(7);
        // Four layers of a trunk of 8 and two branches of 3, four sides each.
        assert_eq!(quads.len(), 4 * (8 + 3 + 3) * 4);
        let lowest: Vec<_> = quads.iter().filter(|quad| quad[0][1] == 0.0).collect();
        assert_eq!(lowest.len(), 16);
        for quad in &lowest {
            // Within the widest layer's radius of the strike, at its foot.
            assert!(quad[0][0].abs() <= 0.7 + 1e-5 && quad[0][2].abs() <= 0.7 + 1e-5);
        }
        let top = quads.iter().map(|quad| quad[1][1]).fold(0.0, f32::max);
        assert_eq!(top, 128.0);
        // Another seed, another path.
        assert_ne!(quads, bolt_quads(8));
    }

    /// A bolt thunders once, lights the sky while live, strikes one to
    /// three more times and is gone within a few seconds.
    #[test]
    fn a_bolt_thunders_once_and_ends() {
        let mut store = LightningStore::default();
        store.spawn(1, DVec3::new(1.0, 2.0, 3.0));
        let first = store.tick();
        assert_eq!(first.thunder, [DVec3::new(1.0, 2.0, 3.0)]);
        assert!(first.flash);
        let mut ticks = 1;
        while !store.bolts.is_empty() {
            assert!(store.tick().thunder.is_empty());
            ticks += 1;
            assert!(ticks < 60, "the bolt never ended");
        }
    }
}
