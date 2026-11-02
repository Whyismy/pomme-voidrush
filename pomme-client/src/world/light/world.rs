//! Block-state access for light propagation: vanilla `LightEngine.getState`
//! plus its 2-entry chunk cache. Positions in unloaded chunks read as bedrock
//! so propagation stops at loaded-area borders exactly like vanilla.

use std::cell::RefCell;
use std::sync::Arc;

use azalea_block::BlockState;
use parking_lot::RwLock;

use super::storage::LightPos;
use crate::world::block::BlockStateTable;
use crate::world::chunk::{Chunk, ChunkLightData, ChunkStore};

/// Where the light engine reads blocks and keeps the light it publishes: the
/// game's [`ChunkStore`], or the light worker's view of the same chunks.
pub(crate) trait LightChunks {
    fn chunk_at(&self, pos: (i32, i32)) -> Option<Arc<RwLock<Chunk>>>;

    fn min_y(&self) -> i32;

    fn light_column_mut(&mut self, pos: (i32, i32)) -> Option<&mut Arc<ChunkLightData>>;

    fn insert_light_column(&mut self, pos: (i32, i32), column: Arc<ChunkLightData>);

    /// The block at a position; air outside loaded chunks.
    fn block_state_at(&self, x: i32, y: i32, z: i32) -> BlockState {
        self.chunk_at((x >> 4, z >> 4))
            .map_or(BlockState::AIR, |chunk| {
                chunk.read().block_state(x, y, z, self.min_y())
            })
    }

    /// See [`ChunkStore::section_is_empty`].
    fn section_is_empty_at(&self, pos: (i32, i32), section_y: i32) -> bool {
        let Some(chunk) = self.chunk_at(pos) else {
            return true;
        };
        let index = section_y - (self.min_y() >> 4);
        let chunk = chunk.read();
        usize::try_from(index)
            .ok()
            .and_then(|i| chunk.sections.get(i))
            .is_none_or(|section| section.block_count == 0)
    }
}

impl LightChunks for ChunkStore {
    fn chunk_at(&self, pos: (i32, i32)) -> Option<Arc<RwLock<Chunk>>> {
        self.get_chunk(&crate::position::ChunkPos::new(pos.0, pos.1))
    }

    fn min_y(&self) -> i32 {
        ChunkStore::min_y(self)
    }

    fn light_column_mut(&mut self, pos: (i32, i32)) -> Option<&mut Arc<ChunkLightData>> {
        self.light_data.get_mut(&pos)
    }

    fn insert_light_column(&mut self, pos: (i32, i32), column: Arc<ChunkLightData>) {
        self.light_data.insert(pos, column);
    }

    fn section_is_empty_at(&self, pos: (i32, i32), section_y: i32) -> bool {
        self.section_is_empty(pos, section_y)
    }
}

pub(crate) trait LightBlockGetter {
    fn block_state_table(&self) -> BlockStateTable;

    fn state(&self, pos: LightPos) -> BlockState;

    /// Vanilla clears the engine's chunk cache between the decrease and
    /// increase passes; getters without a cache ignore this.
    fn clear_cache(&self) {}
}

type CacheEntry = ((i32, i32), Option<Arc<RwLock<Chunk>>>);

/// [`LightChunks`]-backed getter, constructed fresh per light-update run.
pub(crate) struct StoreWorld<'a> {
    block_state_table: BlockStateTable,
    store: &'a dyn LightChunks,
    bedrock: BlockState,
    min_y: i32,
    cache: RefCell<[CacheEntry; 2]>,
}

const INVALID_CHUNK: (i32, i32) = (i32::MAX, i32::MAX);

impl<'a> StoreWorld<'a> {
    pub fn new(store: &'a dyn LightChunks, block_state_table: BlockStateTable) -> Self {
        Self {
            block_state_table,
            store,
            bedrock: block_state_table
                .first_state_of("bedrock")
                .unwrap_or(BlockState::AIR),
            min_y: store.min_y(),
            cache: RefCell::new([(INVALID_CHUNK, None), (INVALID_CHUNK, None)]),
        }
    }

    fn chunk(&self, cx: i32, cz: i32) -> Option<Arc<RwLock<Chunk>>> {
        let mut cache = self.cache.borrow_mut();
        for (pos, chunk) in cache.iter() {
            if *pos == (cx, cz) {
                return chunk.clone();
            }
        }
        let chunk = self.store.chunk_at((cx, cz));
        cache[1] = cache[0].clone();
        cache[0] = ((cx, cz), chunk.clone());
        chunk
    }
}

impl LightBlockGetter for StoreWorld<'_> {
    fn block_state_table(&self) -> BlockStateTable {
        self.block_state_table
    }

    fn state(&self, pos: LightPos) -> BlockState {
        let Some(chunk) = self.chunk(pos.x >> 4, pos.z >> 4) else {
            return self.bedrock;
        };
        let chunk = chunk.read();
        chunk.block_state(pos.x, pos.y, pos.z, self.min_y)
    }

    fn clear_cache(&self) {
        *self.cache.borrow_mut() = [(INVALID_CHUNK, None), (INVALID_CHUNK, None)];
    }
}
