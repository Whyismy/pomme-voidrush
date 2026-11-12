//! The book over an enchanting table (vanilla
//! `EnchantingTableBlockEntity.bookAnimationTick`): it turns idly while
//! shut, and opens toward a player who comes within three blocks, riffling
//! its pages.

use std::f32::consts::PI;

/// One table's book.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BookAnim {
    pub time: u32,
    pub rot: f32,
    pub prev_rot: f32,
    target_rot: f32,
    pub open: f32,
    pub prev_open: f32,
    pub flip: f32,
    pub prev_flip: f32,
    flip_target: f32,
    flip_speed: f32,
}

/// An angle wrapped into -pi..pi.
fn wrap(mut angle: f32) -> f32 {
    while angle >= PI {
        angle -= 2.0 * PI;
    }
    while angle < -PI {
        angle += 2.0 * PI;
    }
    angle
}

impl BookAnim {
    /// One tick; `player` is the nearest player within three blocks, as its
    /// offset from the middle of the table (x, z).
    pub fn tick(&mut self, player: Option<(f64, f64)>) {
        self.prev_open = self.open;
        self.prev_rot = self.rot;
        match player {
            Some((dx, dz)) => {
                self.target_rot = (dz as f32).atan2(dx as f32);
                self.open += 0.1;
                if self.open < 0.5 || fastrand::u32(0..40) == 0 {
                    let before = self.flip_target;
                    while self.flip_target == before {
                        self.flip_target += (fastrand::i32(0..4) - fastrand::i32(0..4)) as f32;
                    }
                }
            }
            None => {
                self.target_rot += 0.02;
                self.open -= 0.1;
            }
        }
        self.rot = wrap(self.rot);
        self.target_rot = wrap(self.target_rot);
        self.rot += wrap(self.target_rot - self.rot) * 0.4;
        self.open = self.open.clamp(0.0, 1.0);
        self.time += 1;
        self.prev_flip = self.flip;
        let pull = ((self.flip_target - self.flip) * 0.4).clamp(-0.2, 0.2);
        self.flip_speed += (pull - self.flip_speed) * 0.9;
        self.flip += self.flip_speed;
    }

    /// The book's turn at `partial` into the tick, radians, taking the
    /// short way round.
    pub fn rot_at(&self, partial: f32) -> f32 {
        self.prev_rot + wrap(self.rot - self.prev_rot) * partial
    }

    /// How far each loose page has turned, right then left, 0 to 1
    /// (`EnchantTableRenderer`).
    pub fn page_flips(&self, partial: f32) -> [f32; 2] {
        let flip = self.prev_flip + (self.flip - self.prev_flip) * partial;
        [0.25, 0.75].map(|offset| ((flip + offset).rem_euclid(1.0) * 1.6 - 0.3).clamp(0.0, 1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With no one near the book stays shut and turns; a player to its east
    /// opens it within a second, facing them.
    #[test]
    fn the_book_opens_to_a_player() {
        let mut book = BookAnim::default();
        for _ in 0..10 {
            book.tick(None);
        }
        assert_eq!(book.open, 0.0);
        assert!(book.rot > 0.0 && book.time == 10);
        // Shut, its loose pages lie at a tenth and nine tenths.
        let [right, left] = book.page_flips(1.0);
        assert!((right - 0.1).abs() < 1e-6 && (left - 0.9).abs() < 1e-6);

        for _ in 0..40 {
            book.tick(Some((2.0, 0.0)));
        }
        assert_eq!(book.open, 1.0);
        assert!(book.rot.abs() < 0.01, "{}", book.rot);
        assert!((book.rot_at(0.5) - book.rot).abs() < 0.01);
        for _ in 0..20 {
            book.tick(None);
        }
        assert_eq!(book.open, 0.0);
    }
}
