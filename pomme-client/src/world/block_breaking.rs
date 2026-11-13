//! Blocks others are breaking (vanilla `LevelRenderer.destroyingBlocks`):
//! the server names who has which block broken how far, and each shows the
//! cracks of the furthest along.

use std::collections::HashMap;

use crate::position::BlockPos;

/// How long a block keeps its cracks with no word of it, in ticks
/// (`LevelRenderer.tick`).
const STALE_TICKS: u64 = 400;

struct Breaking {
    pos: BlockPos,
    stage: u8,
    updated: u64,
}

#[derive(Default)]
pub struct BlockBreaking {
    /// By the entity breaking, which breaks one block at a time.
    by_breaker: HashMap<i32, Breaking>,
}

impl BlockBreaking {
    /// `LevelRenderer.destroyBlockProgress`: `progress` 0 to 9 is a stage,
    /// anything else ends it.
    pub fn set(&mut self, id: i32, pos: BlockPos, progress: u8, tick: u64) {
        if progress < 10 {
            self.by_breaker.insert(
                id,
                Breaking {
                    pos,
                    stage: progress,
                    updated: tick,
                },
            );
        } else {
            self.by_breaker.remove(&id);
        }
    }

    /// Drops what has gone unsaid for too long.
    pub fn tick(&mut self, tick: u64) {
        self.by_breaker
            .retain(|_, breaking| tick.saturating_sub(breaking.updated) <= STALE_TICKS);
    }

    pub fn clear(&mut self) {
        self.by_breaker.clear();
    }

    /// Each block being broken, at the furthest stage anyone has it.
    pub fn stages(&self) -> HashMap<BlockPos, u32> {
        let mut stages = HashMap::new();
        for breaking in self.by_breaker.values() {
            let stage = stages.entry(breaking.pos).or_insert(0);
            *stage = (*stage).max(u32::from(breaking.stage));
        }
        stages
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block shows its furthest stage, a breaker has one block at a time,
    /// a progress past nine ends it, and silence does too.
    #[test]
    fn blocks_show_the_furthest_stage_until_ended_or_stale() {
        let (near, far) = (BlockPos { x: 1, y: 2, z: 3 }, BlockPos { x: 9, y: 2, z: 3 });
        let mut breaking = BlockBreaking::default();
        breaking.set(7, near, 3, 0);
        breaking.set(8, near, 6, 0);
        assert_eq!(breaking.stages().get(&near), Some(&6));

        breaking.set(8, far, 1, 10);
        let stages = breaking.stages();
        assert_eq!((stages.get(&near), stages.get(&far)), (Some(&3), Some(&1)));

        breaking.set(7, near, 255, 20);
        assert_eq!(breaking.stages().get(&near), None);

        breaking.tick(410);
        assert_eq!(breaking.stages().len(), 1);
        breaking.tick(411);
        assert!(breaking.stages().is_empty());
    }
}
