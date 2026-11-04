//! Client-side light engine: a port of vanilla's `LevelLightEngine` stack
//! (26.2 `net/minecraft/world/level/lighting/`). Server light packets are
//! queued as tasks and applied per tick, block changes trigger local
//! relights, and changed sections publish into the per-column
//! [`ChunkLightData`] the mesher already snapshots.
//!
//! Tick flow (vanilla `ClientLevel.update`): [`LevelLightEngine::poll_and_run`]
//! drains queued light tasks at vanilla's rate limit, then runs each engine's
//! update passes, publishing between the decrease and increase passes exactly
//! where vanilla's `swapSectionMap` sits.

mod block;
mod data;
mod engine;
mod queue;
mod sky;
mod sources;
mod storage;
mod worker;
mod world;

use std::collections::VecDeque;
use std::sync::Arc;

use azalea_block::BlockState;
use block::BlockLightEngine;
pub(crate) use data::DataLayer;
use data::LAYER_BYTES;
use engine::LightEngine;
use rustc_hash::FxHashSet as HashSet;
use sky::SkyLightEngine;
use sources::ChunkSkyLightSources;
use storage::StorageCore;
pub(crate) use storage::{LightPos, SectionKey};
pub(crate) use worker::LightWorker;
use world::{LightChunks, StoreWorld};

use crate::world::block::{BlockStateData, BlockStateTable};
use crate::world::chunk::ChunkLightData;

/// Vanilla `Lightmap.getBrightness`: a 0..15 light level on the lightmap
/// curve, lifted by the dimension's ambient light.
pub fn lightmap_brightness(level: u8, ambient_light: f32) -> f32 {
    let v = f32::from(level) / 15.0;
    let curved = v / (4.0 - 3.0 * v);
    curved + (1.0 - curved) * ambient_light
}

/// Vanilla `LightEngine.getOpacity`: light always attenuates by at least 1.
pub(crate) fn opacity(block_state_data: &BlockStateData) -> u8 {
    block_state_data.light_props().dampening.max(1)
}

/// Vanilla `LightEngine.hasDifferentLightProperties`: whether a block change
/// requires a relight.
pub(crate) fn has_different_light_properties(
    old: BlockState,
    new: BlockState,
    block_state_table: BlockStateTable,
) -> bool {
    if old == new {
        return false;
    }
    let (o, n) = (
        block_state_table.block_state_data(old).light_props(),
        block_state_table.block_state_data(new).light_props(),
    );
    n.dampening != o.dampening
        || n.emission != o.emission
        || n.use_shape_for_light_occlusion
        || o.use_shape_for_light_occlusion
}

/// The part of [`has_different_light_properties`] sky light depends on: how
/// the two states block light, not what they emit.
fn blocks_light_differently(
    old: BlockState,
    new: BlockState,
    block_state_table: BlockStateTable,
) -> bool {
    let (o, n) = (
        block_state_table.block_state_data(old).light_props(),
        block_state_table.block_state_data(new).light_props(),
    );
    n.dampening != o.dampening || n.use_shape_for_light_occlusion || o.use_shape_for_light_occlusion
}

/// One light section's payload from a chunk-load or light-update packet,
/// mirroring vanilla `readSectionList`: present bytes, explicitly empty, or
/// untouched.
pub(crate) enum SectionEntry {
    Data(Arc<[u8; LAYER_BYTES]>),
    Empty,
    Skip,
}

impl SectionEntry {
    /// The layer to queue, or `None` for untouched sections.
    fn to_layer(&self) -> Option<DataLayer> {
        match self {
            SectionEntry::Data(bytes) => Some(DataLayer::from_bytes(Arc::clone(bytes))),
            SectionEntry::Empty => Some(DataLayer::new()),
            SectionEntry::Skip => None,
        }
    }
}

/// A light packet's section mask, as the `java.util.BitSet` vanilla reads
/// it into: bit `i` is bit `i % 64` of long `i / 64`, and bits past the last
/// long are clear.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BitSet(Box<[u64]>);

impl BitSet {
    pub fn get(&self, i: usize) -> bool {
        self.0
            .get(i / 64)
            .is_some_and(|long| long >> (i % 64) & 1 != 0)
    }
}

impl From<Vec<u64>> for BitSet {
    fn from(longs: Vec<u64>) -> Self {
        Self(longs.into())
    }
}

/// Vanilla `readSectionList`: expands a packet's y-mask pair into one entry
/// per light section. Malformed (non-2048-byte) payloads are skipped rather
/// than disconnecting.
pub(crate) fn section_entries(
    light_section_count: usize,
    y_mask: &BitSet,
    empty_y_mask: &BitSet,
    updates: &[Box<[u8]>],
) -> Vec<SectionEntry> {
    let mut next = 0usize;
    (0..light_section_count)
        .map(|i| {
            if y_mask.get(i) {
                let entry = match updates.get(next) {
                    Some(bytes) if bytes.len() == LAYER_BYTES => {
                        let mut data = [0u8; LAYER_BYTES];
                        data.copy_from_slice(bytes);
                        SectionEntry::Data(Arc::new(data))
                    }
                    _ => SectionEntry::Skip,
                };
                next += 1;
                entry
            } else if empty_y_mask.get(i) {
                SectionEntry::Empty
            } else {
                SectionEntry::Skip
            }
        })
        .collect()
}

/// The block-mutation sites' shared path: vanilla `LevelChunk.setBlockState`'s
/// write plus its light hooks, with the engine on this thread (the game's
/// runs on [`LightWorker`]). Returns the previous state.
#[cfg(test)]
pub(crate) fn set_block_and_light(
    store: &crate::world::chunk::ChunkStore,
    engine: &mut LevelLightEngine,
    block_state_table: BlockStateTable,
    x: i32,
    y: i32,
    z: i32,
    state: BlockState,
) -> BlockState {
    let (old, empty_flip) = store.set_block_state_tracked(x, y, z, state, block_state_table);
    engine.on_block_changed(
        block_state_table,
        LightPos::new(x, y, z),
        old,
        state,
        empty_flip,
        &|column_y| store.get_block_state(x, column_y, z),
    );
    old
}

/// Light in the lit columns beside `column`, spread back into it from their
/// edges: each edge cell's level re-enters one step toward `column`.
fn pull_light_across_edges(
    engine: &mut impl LightEngine,
    column: (i32, i32),
    min_light_section_y: i32,
    light_sections: usize,
) {
    use queue::Dir;
    let mut entries = Vec::new();
    let storage = engine.storage();
    let (min_x, min_z) = (column.0 * 16, column.1 * 16);
    for (toward, (dx, dz)) in [
        (Dir::North, (0, -1)),
        (Dir::South, (0, 1)),
        (Dir::West, (-1, 0)),
        (Dir::East, (1, 0)),
    ] {
        let neighbor = (column.0 + dx, column.1 + dz);
        if !storage.light_on_in_column(neighbor) {
            continue;
        }
        for index in 0..light_sections as i32 {
            let section_y = min_light_section_y + index;
            let key = SectionKey::new(neighbor.0, section_y, neighbor.1);
            if !storage.storing_light_for_section(key) {
                continue;
            }
            for y in section_y * 16..section_y * 16 + 16 {
                for t in 0..16 {
                    let (x, z) = match toward {
                        Dir::North => (min_x + t, min_z - 1),
                        Dir::South => (min_x + t, min_z + 16),
                        Dir::West => (min_x - 1, min_z + t),
                        _ => (min_x + 16, min_z + t),
                    };
                    let pos = LightPos::new(x, y, z);
                    let level = storage.get_stored_level(pos);
                    if level > 1 {
                        entries.push((
                            pos,
                            queue::increase_only_one_direction(level, false, toward.opposite()),
                        ));
                    }
                }
            }
        }
    }
    engine.increase_queue().extend(entries);
}

/// A queued light task (vanilla `ClientLevel.lightUpdateQueue` runnables).
pub(crate) enum LightTask {
    /// Server light for a column: chunk-load light (`enable: true`, vanilla
    /// `handleLevelChunkWithLight` + `enableChunkLight`) or a standalone
    /// light-update packet (`enable: false`).
    ApplyLight {
        pos: (i32, i32),
        sky: Vec<SectionEntry>,
        block: Vec<SectionEntry>,
        enable: bool,
    },
    /// Chunk unload (vanilla `queueLightRemoval`).
    Remove { pos: (i32, i32) },
}

/// Remesh requests produced by a light run.
#[derive(Default)]
pub(crate) struct LightDirty {
    /// World-space light sections whose rendered light may have changed.
    pub sections: HashSet<SectionKey>,
    /// Columns whose chunk-load light applied: remesh column + neighbors.
    pub columns: Vec<(i32, i32)>,
    /// Columns whose light was switched on or off, in order
    /// ([`LevelLightEngine::light_on_in_column`]).
    pub lit: Vec<((i32, i32), bool)>,
}

impl LightDirty {
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty() && self.columns.is_empty() && self.lit.is_empty()
    }
}

#[derive(Clone, Copy)]
enum LayerKind {
    Block,
    Sky,
}

pub(crate) struct LevelLightEngine {
    block: BlockLightEngine,
    /// `None` in dimensions without skylight (vanilla constructs the sky
    /// engine only when `dimensionType().hasSkyLight()`).
    sky: Option<SkyLightEngine>,
    tasks: VecDeque<LightTask>,
    /// Lowest block section y (`min_y >> 4`); light sections span one extra
    /// section below and above.
    min_section_y: i32,
    section_count: i32,
}

impl LevelLightEngine {
    pub fn new(height: u32, min_y: i32, has_sky: bool) -> Self {
        Self {
            block: BlockLightEngine::new(),
            sky: has_sky.then(SkyLightEngine::new),
            tasks: VecDeque::new(),
            min_section_y: min_y >> 4,
            section_count: (height / 16) as i32,
        }
    }

    /// Vanilla `LevelLightEngine.lightOnInColumn`: the column's light has been
    /// applied, in both layers the dimension has. Vanilla's renderer requires
    /// it of a section's whole 3x3 neighbourhood before the section may first
    /// compile (`SectionUpdateTracker.hasAllNeighbors`).
    pub fn light_on_in_column(&self, column: (i32, i32)) -> bool {
        self.block.storage.light_on_in_column(column)
            && self
                .sky
                .as_ref()
                .is_none_or(|sky| sky.storage.light_on_in_column(column))
    }

    pub fn light_section_count(&self) -> usize {
        (self.section_count + 2) as usize
    }

    fn light_section_key(&self, pos: (i32, i32), index: usize) -> SectionKey {
        SectionKey::new(pos.0, self.min_section_y - 1 + index as i32, pos.1)
    }

    pub fn queue_task(&mut self, task: LightTask) {
        self.tasks.push_back(task);
    }

    pub fn has_tasks(&self) -> bool {
        !self.tasks.is_empty()
    }

    /// Vanilla `LevelChunk.setBlockState`'s light lines: section empty-status
    /// first, then (when the light properties changed) the sky-source
    /// heightmap update followed by a relight check. Call with the block
    /// already written to the chunk; `column_state` reads the block column at
    /// the position's x/z by world Y.
    pub fn on_block_changed(
        &mut self,
        block_state_table: BlockStateTable,
        pos: LightPos,
        old: BlockState,
        new: BlockState,
        empty_flip: Option<bool>,
        column_state: &impl Fn(i32) -> BlockState,
    ) {
        if let Some(empty) = empty_flip {
            self.block.update_section_status(pos.section(), empty);
            if let Some(sky) = &mut self.sky {
                sky.update_section_status(pos.section(), empty);
            }
        }
        if has_different_light_properties(old, new, block_state_table) {
            // Sky light reads only how a block blocks light, never what it
            // emits, so a change of emission alone (Void Rush's player light,
            // a light block moved four times a second) leaves it as it was.
            // Vanilla re-checks the sky anyway, and under cover that check
            // empties and refloods ~20 sections back to the same levels.
            let sky_relevant = blocks_light_differently(old, new, block_state_table);
            if sky_relevant
                && let Some(sky) = &mut self.sky
                && let Some(sources) = sky.sources.get_mut(&(pos.x >> 4, pos.z >> 4))
            {
                sources.update(
                    block_state_table,
                    column_state,
                    pos.x & 15,
                    pos.y,
                    pos.z & 15,
                );
            }
            self.block.check_block(pos);
            if sky_relevant && let Some(sky) = &mut self.sky {
                sky.check_block(pos);
            }
        }
    }

    /// Relight hook for positions mutated without old-state tracking (the
    /// player-prediction paths, which apply and roll back edits internally):
    /// the same hooks as [`Self::on_block_changed`] with the
    /// `hasDifferentLightProperties` work-skip dropped — section status
    /// dedups and `checkNode` recomputes idempotently, so results match at a
    /// small recompute cost.
    pub fn on_block_dirty(
        &mut self,
        store: &impl LightChunks,
        block_state_table: BlockStateTable,
        x: i32,
        y: i32,
        z: i32,
    ) {
        let pos = LightPos::new(x, y, z);
        let empty = store.section_is_empty_at((x >> 4, z >> 4), y >> 4);
        self.block.update_section_status(pos.section(), empty);
        if let Some(sky) = &mut self.sky {
            sky.update_section_status(pos.section(), empty);
            if let Some(sources) = sky.sources.get_mut(&(x >> 4, z >> 4)) {
                sources.update(
                    block_state_table,
                    &|cy| store.block_state_at(x, cy, z),
                    x & 15,
                    y,
                    z & 15,
                );
            }
        }
        self.block.check_block(pos);
        if let Some(sky) = &mut self.sky {
            sky.check_block(pos);
        }
    }

    /// Registers a freshly loaded chunk column: builds its sky-source
    /// heightmap (vanilla fills sources at chunk-data apply, before the
    /// queued light task) and creates its light column from any layers the
    /// engine already stores (border sections lit by loaded neighbors), so
    /// later publishes only need to touch changed sections.
    pub fn on_chunk_loaded(
        &mut self,
        store: &mut impl LightChunks,
        block_state_table: BlockStateTable,
        pos: (i32, i32),
    ) {
        let count = self.light_section_count();
        let min_section_y = self.min_section_y;
        let key_at = |index: usize| SectionKey::new(pos.0, min_section_y - 1 + index as i32, pos.1);
        let mut sky_sections = vec![None; count];
        let mut block_sections = vec![None; count];
        for (index, slot) in block_sections.iter_mut().enumerate() {
            *slot = self.block.storage.layer(key_at(index)).cloned();
        }
        let mut sky_top_section = None;
        if let Some(sky) = &mut self.sky {
            for (index, slot) in sky_sections.iter_mut().enumerate() {
                *slot = sky.storage.layer(key_at(index)).cloned();
            }
            sky_top_section = sky
                .sky
                .column_top(pos)
                .map(|top| top - (self.min_section_y - 1));
            if let Some(chunk) = store.chunk_at(pos) {
                sky.sources.insert(
                    pos,
                    ChunkSkyLightSources::fill_from(
                        store.min_y(),
                        &chunk.read(),
                        block_state_table,
                    ),
                );
            }
        }
        store.insert_light_column(
            pos,
            Arc::new(ChunkLightData {
                sky_sections,
                block_sections,
                min_y: store.min_y(),
                has_sky: self.sky.is_some(),
                sky_top_section,
            }),
        );
    }

    /// Forgets a column's sky sources; vanilla's live on the chunk object and
    /// vanish when it drops. The stored light tears down via
    /// [`LightTask::Remove`].
    /// A column the server sent without light, lit as vanilla's server
    /// lights a fresh one (`ThreadedLevelLightEngine.initializeLight`, then
    /// `lightChunk`): its sections' status with the light still off, then
    /// both engines' `propagateLightSources`. Light already in the lit
    /// columns beside it flows in across the edges, as it would have on the
    /// server.
    fn light_column_ourselves(
        &mut self,
        pos: (i32, i32),
        store: &impl LightChunks,
        block_state_table: BlockStateTable,
        out: &mut LightDirty,
    ) {
        for section_y in self.min_section_y..self.min_section_y + self.section_count {
            let key = SectionKey::new(pos.0, section_y, pos.1);
            let empty = store.section_is_empty_at(pos, section_y);
            self.block.update_section_status(key, empty);
            if let Some(engine) = &mut self.sky {
                engine.update_section_status(key, empty);
            }
        }
        match store.chunk_at(pos) {
            Some(chunk) => self.block.propagate_light_sources(
                pos,
                &chunk.read(),
                store.min_y(),
                block_state_table,
            ),
            None => self.block.set_light_enabled(pos, true),
        }
        let sections = self.light_section_count();
        let min_light_section_y = self.min_section_y - 1;
        pull_light_across_edges(&mut self.block, pos, min_light_section_y, sections);
        if let Some(engine) = &mut self.sky {
            engine.propagate_light_sources(pos);
            pull_light_across_edges(engine, pos, min_light_section_y, sections);
        }
        out.lit.push((pos, self.light_on_in_column(pos)));
        out.columns.push(pos);
    }

    pub fn on_chunk_unloaded(&mut self, pos: (i32, i32)) {
        if let Some(sky) = &mut self.sky {
            sky.sources.remove(&pos);
        }
    }

    /// Drains queued light tasks at vanilla's per-tick rate limit, then runs
    /// the light updates. Call once per game tick.
    pub fn poll_and_run(
        &mut self,
        store: &mut impl LightChunks,
        block_state_table: BlockStateTable,
        out: &mut LightDirty,
    ) {
        // Vanilla ClientLevel.pollLightUpdates: size < 1000 ? max(10, size/10)
        // : size tasks per tick.
        let size = self.tasks.len();
        let quota = if size < 1000 {
            (size / 10).max(10)
        } else {
            size
        };
        let tasks_start = std::time::Instant::now();
        for _ in 0..quota {
            let Some(task) = self.tasks.pop_front() else {
                break;
            };
            self.apply_task(task, store, block_state_table, out);
        }
        crate::frame_log::add("light_tasks", tasks_start.elapsed().as_secs_f32() * 1000.0);
        self.run_light_updates(store, block_state_table, out);
    }

    fn apply_task(
        &mut self,
        task: LightTask,
        store: &impl LightChunks,
        block_state_table: BlockStateTable,
        out: &mut LightDirty,
    ) {
        match task {
            LightTask::ApplyLight {
                pos,
                sky,
                block,
                enable,
            } => {
                // Paper sends most columns loaded after the join with no
                // light at all (every mask empty) and no light update after,
                // which leaves their caves black on vanilla but where the
                // sky reaches. Light them as the server would have.
                let unlit = sky
                    .iter()
                    .chain(&block)
                    .all(|entry| matches!(entry, SectionEntry::Skip));
                if enable && unlit {
                    self.light_column_ourselves(pos, store, block_state_table, out);
                    return;
                }
                // Vanilla readSectionList order: sky layer first, then block.
                for (index, entry) in sky.iter().enumerate() {
                    let key = self.light_section_key(pos, index);
                    if let Some(engine) = &mut self.sky
                        && let Some(layer) = entry.to_layer()
                    {
                        engine.queue_section_data(key, Some(layer));
                    }
                    if !enable && !matches!(entry, SectionEntry::Skip) {
                        // Vanilla `setSectionDirtyWithNeighbors`.
                        out.sections.extend(key.with_neighbors());
                    }
                }
                for (index, entry) in block.iter().enumerate() {
                    let key = self.light_section_key(pos, index);
                    if let Some(layer) = entry.to_layer() {
                        self.block.queue_section_data(key, Some(layer));
                    }
                    if !enable && !matches!(entry, SectionEntry::Skip) {
                        // Vanilla `setSectionDirtyWithNeighbors`.
                        out.sections.extend(key.with_neighbors());
                    }
                }
                // Vanilla applyLightData tail: sources on for both task kinds.
                // The enable-before-status order is load-bearing: fresh
                // above-terrain sky sections initialize to 15 only while the
                // column's light is already on.
                self.block.set_light_enabled(pos, true);
                if let Some(engine) = &mut self.sky {
                    engine.set_light_enabled(pos, true);
                }
                out.lit.push((pos, self.light_on_in_column(pos)));
                if enable {
                    // Vanilla enableChunkLight: report each block section's
                    // emptiness, then schedule the column's meshes.
                    for section_y in self.min_section_y..self.min_section_y + self.section_count {
                        let key = SectionKey::new(pos.0, section_y, pos.1);
                        let empty = store.section_is_empty_at(pos, section_y);
                        self.block.update_section_status(key, empty);
                        if let Some(engine) = &mut self.sky {
                            engine.update_section_status(key, empty);
                        }
                    }
                    out.columns.push(pos);
                }
            }
            LightTask::Remove { pos } => {
                // Vanilla queueLightRemoval order: sources off, null out every
                // light section, then mark every block section empty.
                self.block.set_light_enabled(pos, false);
                if let Some(engine) = &mut self.sky {
                    engine.set_light_enabled(pos, false);
                }
                out.lit.push((pos, self.light_on_in_column(pos)));
                for index in 0..self.light_section_count() {
                    let key = self.light_section_key(pos, index);
                    self.block.queue_section_data(key, None);
                    if let Some(engine) = &mut self.sky {
                        engine.queue_section_data(key, None);
                    }
                }
                for section_y in self.min_section_y..self.min_section_y + self.section_count {
                    let key = SectionKey::new(pos.0, section_y, pos.1);
                    self.block.update_section_status(key, true);
                    if let Some(engine) = &mut self.sky {
                        engine.update_section_status(key, true);
                    }
                }
            }
        }
    }

    /// Vanilla `runLightUpdates` per engine (block first, then sky), with the
    /// publish step sitting where `swapSectionMap` does: after the decrease
    /// pass, before the increase pass.
    fn run_light_updates(
        &mut self,
        store: &mut impl LightChunks,
        block_state_table: BlockStateTable,
        out: &mut LightDirty,
    ) {
        let (min_section_y, section_count) = (self.min_section_y, self.section_count);
        let block_start = std::time::Instant::now();
        // Both passes run before the publish, as vanilla's `swapSectionMap`
        // ends `runLightUpdates`: publishing between them would hand the
        // meshers a world with a moved light's old glow gone and its new
        // one not yet spread, which shows as a dark flicker.
        {
            let world = StoreWorld::new(store, block_state_table);
            self.block.begin_updates(&world);
            self.block.finish_updates(&world);
        }
        Self::publish(
            &mut self.block.storage,
            None,
            store,
            LayerKind::Block,
            min_section_y,
            section_count,
            out,
        );
        crate::frame_log::add("light_block", block_start.elapsed().as_secs_f32() * 1000.0);
        let sky_start = std::time::Instant::now();
        if let Some(sky) = &mut self.sky {
            {
                let world = StoreWorld::new(store, block_state_table);
                let lap = std::time::Instant::now();
                sky.begin_updates(&world);
                crate::frame_log::add("light_sky_begin", lap.elapsed().as_secs_f32() * 1000.0);
                let lap = std::time::Instant::now();
                sky.finish_updates(&world);
                crate::frame_log::add("light_sky_finish", lap.elapsed().as_secs_f32() * 1000.0);
            }
            let lap = std::time::Instant::now();
            Self::publish(
                &mut sky.storage,
                Some(&sky.sky),
                store,
                LayerKind::Sky,
                min_section_y,
                section_count,
                out,
            );
            crate::frame_log::add("light_sky_publish", lap.elapsed().as_secs_f32() * 1000.0);
        }
        crate::frame_log::add("light_sky", sky_start.elapsed().as_secs_f32() * 1000.0);
    }

    /// Writes changed sections into their columns' [`ChunkLightData`]
    /// (vanilla `swapSectionMap`) and drains the remesh scope (vanilla
    /// `onLightUpdate` fan-out).
    fn publish(
        storage: &mut StorageCore,
        sky: Option<&sky::SkyStorage>,
        store: &mut impl LightChunks,
        layer: LayerKind,
        min_section_y: i32,
        section_count: i32,
        out: &mut LightDirty,
    ) {
        let count = (section_count + 2) as usize;
        let changed: Vec<SectionKey> = storage.changed_sections.drain().collect();
        for key in changed {
            // Columns without chunk data have no ChunkLightData; their layers
            // republish via on_chunk_loaded when the chunk arrives.
            let Some(column) = store.light_column_mut(key.column()) else {
                continue;
            };
            let index = key.y - (min_section_y - 1);
            debug_assert!(
                (0..count as i32).contains(&index),
                "light section {index} outside 0..{count}"
            );
            if !(0..count as i32).contains(&index) {
                continue;
            }
            let column = Arc::make_mut(column);
            let slot = match layer {
                LayerKind::Block => &mut column.block_sections[index as usize],
                LayerKind::Sky => &mut column.sky_sections[index as usize],
            };
            *slot = storage.layer(key).cloned();
            // A column's top only moves when one of its own sections
            // changed, so refreshing it per changed key keeps it current.
            if let Some(sky) = sky {
                column.sky_top_section = sky
                    .column_top(key.column())
                    .map(|top| top - (min_section_y - 1));
            }
        }
        out.sections.extend(storage.affected_sections.drain());
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::world::LightBlockGetter;
    use super::*;
    use crate::world::chunk::ChunkStore;

    fn find_state(name: &str, props: &[(&str, &str)]) -> BlockState {
        BlockStateTable::native().find_state(name, props)
    }

    /// Times a light block moved a step on a void-world island with sky light
    /// (how Void Rush's player light moves, 4 times a second): run with
    /// `--release -- --ignored --nocapture`.
    #[test]
    #[ignore = "a timing, not a check"]
    fn time_a_player_light_move() {
        let table = BlockStateTable::native();
        let mut store = ChunkStore::new(4);
        let sections = store.section_count() as usize;
        let columns: Vec<(i32, i32)> = (-2..=2)
            .flat_map(|x| (-2..=2).map(move |z| (x, z)))
            .collect();
        for &(x, z) in &columns {
            store.insert_chunk(
                crate::position::ChunkPos::new(x, z),
                crate::world::chunk::Chunk::empty(sections),
            );
        }
        let height = (store.section_count() * 16) as u32;
        let mut engine = LevelLightEngine::new(height, store.min_y(), true);
        let stone = find_state("stone", &[]);
        for &(cx, cz) in &columns {
            for x in cx * 16..cx * 16 + 16 {
                for z in cz * 16..cz * 16 + 16 {
                    // A void-world island whose top ends a section, so the
                    // light at the player's feet sits alone in the section
                    // above (its every move flips that section's emptiness).
                    for y in 76..80 {
                        store.set_block_state_tracked(x, y, z, stone, table);
                    }
                }
            }
        }
        for &pos in &columns {
            engine.on_chunk_loaded(&mut store, table, pos);
            engine.queue_task(LightTask::ApplyLight {
                pos,
                sky: Vec::new(),
                block: Vec::new(),
                enable: true,
            });
        }
        for _ in 0..200 {
            engine.poll_and_run(&mut store, table, &mut LightDirty::default());
        }
        let light = find_state("light", &[("level", "15"), ("waterlogged", "false")]);
        let mut at = (0, 80, 0);
        set_block_and_light(&store, &mut engine, table, at.0, at.1, at.2, light);
        engine.poll_and_run(&mut store, table, &mut LightDirty::default());
        let sky_before: Vec<u8> = (-20..20)
            .flat_map(|x| (76..92).map(move |y| (x, y)))
            .map(|(x, y)| store.get_sky_light(x, y, 0))
            .collect();
        let moves = 40;
        let start = std::time::Instant::now();
        let mut dirty_sections = 0;
        for i in 0..moves {
            let next = (if i % 2 == 0 { 1 } else { 0 }, 80, 0);
            set_block_and_light(
                &store,
                &mut engine,
                table,
                at.0,
                at.1,
                at.2,
                BlockState::AIR,
            );
            set_block_and_light(&store, &mut engine, table, next.0, next.1, next.2, light);
            let mut dirty = LightDirty::default();
            engine.poll_and_run(&mut store, table, &mut dirty);
            dirty_sections += dirty.sections.len();
            at = next;
        }
        println!(
            "{:.3?} a move, {} dirty sections a move",
            start.elapsed() / moves,
            dirty_sections / moves as usize
        );
        // Moving a light changes no sky light.
        let sky_after: Vec<u8> = (-20..20)
            .flat_map(|x| (76..92).map(move |y| (x, y)))
            .map(|(x, y)| store.get_sky_light(x, y, 0))
            .collect();
        assert_eq!(sky_after, sky_before);
    }

    /// A light block moved a step in one update (how servers make a player
    /// light) publishes its new glow together with the old one's removal:
    /// the meshers never see the dark world in between.
    #[test]
    fn a_moved_light_publishes_whole() {
        let table = BlockStateTable::native();
        let mut store = ChunkStore::with_origin_chunk();
        let height = (store.section_count() * 16) as u32;
        let mut engine = LevelLightEngine::new(height, store.min_y(), false);
        engine.on_chunk_loaded(&mut store, table, (0, 0));
        engine.queue_task(LightTask::ApplyLight {
            pos: (0, 0),
            sky: Vec::new(),
            block: Vec::new(),
            enable: true,
        });
        let light = find_state("light", &[("level", "15")]);
        let run = |store: &mut ChunkStore, engine: &mut LevelLightEngine| {
            engine.poll_and_run(store, table, &mut LightDirty::default());
        };
        set_block_and_light(&store, &mut engine, table, 8, 70, 8, light);
        run(&mut store, &mut engine);
        assert_eq!(store.get_block_light(11, 70, 8), 12);

        set_block_and_light(&store, &mut engine, table, 8, 70, 8, BlockState::AIR);
        set_block_and_light(&store, &mut engine, table, 9, 70, 8, light);
        run(&mut store, &mut engine);
        assert_eq!(store.get_block_light(9, 70, 8), 15);
        assert_eq!(store.get_block_light(11, 70, 8), 13);
        assert_eq!(store.get_block_light(8, 70, 8), 14);
    }

    struct TestWorld {
        block_state_table: BlockStateTable,
        blocks: HashMap<LightPos, BlockState>,
    }

    impl TestWorld {
        fn new() -> Self {
            Self {
                block_state_table: BlockStateTable::native(),
                blocks: HashMap::new(),
            }
        }

        fn set(&mut self, x: i32, y: i32, z: i32, state: BlockState) {
            self.blocks.insert(LightPos::new(x, y, z), state);
        }

        fn clear(&mut self, x: i32, y: i32, z: i32) {
            self.blocks.remove(&LightPos::new(x, y, z));
        }
    }

    impl LightBlockGetter for TestWorld {
        fn block_state_table(&self) -> BlockStateTable {
            self.block_state_table
        }

        fn state(&self, pos: LightPos) -> BlockState {
            self.blocks.get(&pos).copied().unwrap_or(BlockState::AIR)
        }
    }

    /// Engine storing light for section (0,0,0) and its 26 neighbors, with
    /// sources enabled in the center column.
    fn block_engine() -> BlockLightEngine {
        let mut engine = BlockLightEngine::new();
        engine.update_section_status(SectionKey::new(0, 0, 0), false);
        engine.set_light_enabled((0, 0), true);
        engine
    }

    fn run(engine: &mut impl LightEngine, world: &TestWorld) {
        engine.begin_updates(world);
        engine.finish_updates(world);
    }

    fn level(engine: &BlockLightEngine, x: i32, y: i32, z: i32) -> u8 {
        engine.storage.get_stored_level(LightPos::new(x, y, z))
    }

    #[test]
    fn torch_diamond() {
        let mut world = TestWorld::new();
        let mut engine = block_engine();
        world.set(8, 8, 8, find_state("torch", &[]));
        engine.check_block(LightPos::new(8, 8, 8));
        run(&mut engine, &world);

        // Level = 14 - manhattan distance in open air.
        assert_eq!(level(&engine, 8, 8, 8), 14);
        assert_eq!(level(&engine, 9, 8, 8), 13);
        assert_eq!(level(&engine, 8, 8, 4), 10);
        assert_eq!(level(&engine, 12, 10, 9), 7);
        // The diamond's edge crosses into the neighbor section.
        assert_eq!(level(&engine, 21, 8, 8), 1);
        assert_eq!(level(&engine, 22, 8, 8), 0);
    }

    #[test]
    fn torch_against_wall() {
        let mut world = TestWorld::new();
        let mut engine = block_engine();
        world.set(8, 8, 8, find_state("torch", &[]));
        world.set(9, 8, 8, find_state("stone", &[]));
        engine.check_block(LightPos::new(8, 8, 8));
        engine.check_block(LightPos::new(9, 8, 8));
        run(&mut engine, &world);

        // Opaque cells stay dark; light reaches behind them the long way.
        assert_eq!(level(&engine, 9, 8, 8), 0);
        assert_eq!(level(&engine, 10, 8, 8), 10);
    }

    #[test]
    fn torch_removal_clears_light() {
        let mut world = TestWorld::new();
        let mut engine = block_engine();
        world.set(8, 8, 8, find_state("torch", &[]));
        engine.check_block(LightPos::new(8, 8, 8));
        run(&mut engine, &world);
        assert_eq!(level(&engine, 12, 8, 8), 10);

        world.clear(8, 8, 8);
        engine.check_block(LightPos::new(8, 8, 8));
        run(&mut engine, &world);
        for (x, y, z) in [(8, 8, 8), (9, 8, 8), (12, 8, 8), (8, 12, 8)] {
            assert_eq!(level(&engine, x, y, z), 0, "at {x} {y} {z}");
        }
    }

    #[test]
    fn overlapping_sources_keep_brighter() {
        let mut world = TestWorld::new();
        let mut engine = block_engine();
        let torch = find_state("torch", &[]);
        world.set(4, 8, 8, torch);
        world.set(12, 8, 8, torch);
        engine.check_block(LightPos::new(4, 8, 8));
        engine.check_block(LightPos::new(12, 8, 8));
        run(&mut engine, &world);
        assert_eq!(level(&engine, 8, 8, 8), 10);
        assert_eq!(level(&engine, 6, 8, 8), 12);

        // Removing one source re-floods from the survivor (the decrease
        // pass's brighter-neighbor re-injection).
        world.clear(4, 8, 8);
        engine.check_block(LightPos::new(4, 8, 8));
        run(&mut engine, &world);
        assert_eq!(level(&engine, 6, 8, 8), 8);
        assert_eq!(level(&engine, 4, 8, 8), 6);
        assert_eq!(level(&engine, 12, 8, 8), 14);
    }

    #[test]
    fn emission_off_in_disabled_column() {
        let mut world = TestWorld::new();
        let mut engine = BlockLightEngine::new();
        engine.update_section_status(SectionKey::new(0, 0, 0), false);
        // No set_light_enabled: vanilla getEmission returns 0.
        world.set(8, 8, 8, find_state("torch", &[]));
        engine.check_block(LightPos::new(8, 8, 8));
        run(&mut engine, &world);
        assert_eq!(level(&engine, 8, 8, 8), 0);
        assert_eq!(level(&engine, 9, 8, 8), 0);
    }

    #[test]
    fn queued_packet_data_installs_without_propagating() {
        let world = TestWorld::new();
        let mut engine = BlockLightEngine::new();
        let mut layer = DataLayer::new();
        layer.set(1, 1, 1, 9);
        engine.queue_section_data(SectionKey::new(0, 0, 0), Some(layer));
        engine.update_section_status(SectionKey::new(0, 0, 0), false);
        run(&mut engine, &world);

        // Server data lands verbatim; nothing spreads without a checkBlock.
        assert_eq!(level(&engine, 1, 1, 1), 9);
        assert_eq!(level(&engine, 2, 1, 1), 0);

        // Marking the section empty tears the whole neighborhood down.
        engine.update_section_status(SectionKey::new(0, 0, 0), true);
        run(&mut engine, &world);
        for key in SectionKey::new(0, 0, 0).with_neighbors() {
            assert!(!engine.storage.storing_light_for_section(key), "{key:?}");
        }
    }

    #[test]
    fn slab_occludes_directionally() {
        let mut world = TestWorld::new();
        let mut engine = block_engine();
        let slab = find_state("oak_slab", &[("type", "top"), ("waterlogged", "false")]);
        world.set(8, 8, 8, find_state("torch", &[]));
        world.set(8, 9, 8, slab);
        engine.check_block(LightPos::new(8, 8, 8));
        engine.check_block(LightPos::new(8, 9, 8));
        run(&mut engine, &world);

        // Light enters the slab's own cell from below (its DOWN face is
        // open), but its full UP face blocks the straight path; above the
        // slab arrives the long way around.
        assert_eq!(level(&engine, 8, 9, 8), 13);
        assert_eq!(level(&engine, 9, 9, 8), 12);
        assert_eq!(level(&engine, 8, 10, 8), 10);
    }

    fn sky_level(engine: &SkyLightEngine, x: i32, y: i32, z: i32) -> u8 {
        engine.storage.get_stored_level(LightPos::new(x, y, z))
    }

    /// Sky engine storing light around section (0,0,0), with chunk (0,0)'s
    /// sources built from the world by updating each column at `source_ys`.
    fn sky_engine(world: &TestWorld, source_ys: &[(i32, i32, i32)]) -> SkyLightEngine {
        let mut engine = SkyLightEngine::new();
        engine.update_section_status(SectionKey::new(0, 0, 0), false);
        let mut sources = ChunkSkyLightSources::new(-64);
        for &(x, y, z) in source_ys {
            sources.update(
                world.block_state_table(),
                &|sy| world.state(LightPos::new(x, sy, z)),
                x,
                y,
                z,
            );
        }
        engine.sources.insert((0, 0), sources);
        engine.set_light_enabled((0, 0), true);
        engine
    }

    #[test]
    fn digging_through_a_floor_lets_sky_under_it() {
        let mut world = TestWorld::new();
        let stone = find_state("stone", &[]);
        for x in 0..16 {
            for z in 0..16 {
                world.set(x, 0, z, stone);
            }
        }
        let floor: Vec<(i32, i32, i32)> = (0..16)
            .flat_map(|x| (0..16).map(move |z| (x, 0, z)))
            .collect();
        let mut engine = sky_engine(&world, &floor);
        run(&mut engine, &world);
        // Enabling pre-filled the fully-above-terrain section with 15.
        assert_eq!(sky_level(&engine, 8, 20, 8), 15);
        assert_eq!(sky_level(&engine, 8, -1, 8), 0);

        // Dig one floor block: its column becomes open sky and light floods
        // sideways under the floor.
        world.clear(8, 0, 8);
        engine.sources.get_mut(&(0, 0)).unwrap().update(
            world.block_state_table(),
            &|y| world.state(LightPos::new(8, y, 8)),
            8,
            0,
            8,
        );
        engine.check_block(LightPos::new(8, 0, 8));
        run(&mut engine, &world);
        assert_eq!(sky_level(&engine, 8, 0, 8), 15);
        assert_eq!(sky_level(&engine, 8, -10, 8), 15);
        assert_eq!(sky_level(&engine, 9, 0, 8), 0); // stone floor
        assert_eq!(sky_level(&engine, 9, -1, 8), 14);
        assert_eq!(sky_level(&engine, 12, -1, 8), 11);

        // Cover it again: the source column tears down and the decrease wave
        // re-darkens everything under the floor.
        world.set(8, 0, 8, stone);
        engine.sources.get_mut(&(0, 0)).unwrap().update(
            world.block_state_table(),
            &|y| world.state(LightPos::new(8, y, 8)),
            8,
            0,
            8,
        );
        engine.check_block(LightPos::new(8, 0, 8));
        run(&mut engine, &world);
        for (x, y, z) in [(8, 0, 8), (8, -10, 8), (9, -1, 8), (12, -1, 8)] {
            assert_eq!(sky_level(&engine, x, y, z), 0, "at {x} {y} {z}");
        }
        assert_eq!(sky_level(&engine, 8, 20, 8), 15);
    }

    #[test]
    fn leaves_attenuate_falling_sky_light() {
        let mut world = TestWorld::new();
        world.set(8, 10, 8, find_state("oak_leaves", &[]));
        let mut engine = sky_engine(&world, &[(8, 10, 8)]);
        engine.check_block(LightPos::new(8, 10, 8));
        run(&mut engine, &world);

        // Sources stop above the leaf; below it the light falls with the
        // leaf's opacity of 1 and then keeps losing 1 per non-source step.
        assert_eq!(sky_level(&engine, 8, 12, 8), 15);
        assert_eq!(sky_level(&engine, 9, 11, 8), 14);
        assert_eq!(sky_level(&engine, 8, 10, 8), 14);
        assert_eq!(sky_level(&engine, 8, 9, 8), 13);
        assert_eq!(sky_level(&engine, 8, 5, 8), 9);
    }

    #[test]
    fn light_property_change_detection() {
        let _world = TestWorld::new();
        let air = BlockState::AIR;
        let stone = find_state("stone", &[]);
        let torch = find_state("torch", &[]);
        let glass = find_state("glass", &[]);
        let slab = find_state("oak_slab", &[("type", "bottom"), ("waterlogged", "false")]);

        assert!(!has_different_light_properties(
            stone,
            stone,
            _world.block_state_table()
        ));
        assert!(has_different_light_properties(
            air,
            stone,
            _world.block_state_table()
        ));
        assert!(has_different_light_properties(
            air,
            torch,
            _world.block_state_table()
        ));
        // Glass matches air in every light property: no relight, as vanilla.
        assert!(!has_different_light_properties(
            air,
            glass,
            _world.block_state_table()
        ));
        // Shaped states always relight.
        assert!(has_different_light_properties(
            air,
            slab,
            _world.block_state_table()
        ));
        assert!(has_different_light_properties(
            slab,
            air,
            _world.block_state_table()
        ));
    }

    /// Paper sends columns loaded after the join without light: such a
    /// column lights itself as the server would, a lantern in a sealed cave
    /// lighting the cave and the open sky over it full, and light from the
    /// lit column beside it flows across the edge.
    #[test]
    fn a_column_sent_without_light_lights_itself() {
        use crate::position::ChunkPos;
        use crate::world::chunk::Chunk;
        let table = BlockStateTable::native();
        let mut store = ChunkStore::new(4);
        let sections = store.section_count() as usize;
        let height = store.height();
        let min_y = store.min_y();
        let mut engine = LevelLightEngine::new(height, min_y, true);
        let stone = find_state("stone", &[]);
        let lantern = find_state("lantern", &[("hanging", "false"), ("waterlogged", "false")]);
        let run = |store: &mut ChunkStore, engine: &mut LevelLightEngine| {
            for _ in 0..4 {
                engine.poll_and_run(store, table, &mut LightDirty::default());
            }
        };
        // Two columns, each a stone slab from y 40 to 80 hollowed into one
        // cave from x 2 to 29 at y 50 to 52; a lantern at each end.
        let unlit = |pos: (i32, i32)| LightTask::ApplyLight {
            pos,
            sky: (0..sections + 2).map(|_| SectionEntry::Skip).collect(),
            block: (0..sections + 2).map(|_| SectionEntry::Skip).collect(),
            enable: true,
        };
        for cx in 0..2 {
            store.insert_chunk(ChunkPos::new(cx, 0), Chunk::empty(sections));
            for x in cx * 16..cx * 16 + 16 {
                for z in 0..16 {
                    for y in 40..80 {
                        let cave = (2..30).contains(&x) && z == 8 && (50..53).contains(&y);
                        if !cave {
                            store.set_block_state_tracked(x, y, z, stone, table);
                        }
                    }
                }
            }
        }
        store.set_block_state_tracked(2, 50, 8, lantern, table);
        store.set_block_state_tracked(29, 50, 8, lantern, table);
        // The first column arrives and is lit before the second.
        engine.on_chunk_loaded(&mut store, table, (0, 0));
        engine.queue_task(unlit((0, 0)));
        run(&mut store, &mut engine);
        assert_eq!(store.get_block_light(2, 50, 8), 15);
        assert_eq!(store.get_block_light(10, 50, 8), 7);
        assert_eq!(store.get_sky_light(10, 50, 8), 0);
        assert_eq!(store.get_sky_light(10, 80, 8), 15);
        assert_eq!(store.get_sky_light(10, 81, 8), 15);
        engine.on_chunk_loaded(&mut store, table, (1, 0));
        engine.queue_task(unlit((1, 0)));
        run(&mut store, &mut engine);
        // Each lantern's light runs the length of the cave, both ways
        // across the edge.
        assert_eq!(store.get_block_light(29, 50, 8), 15);
        assert_eq!(store.get_block_light(20, 50, 8), 6);
        assert_eq!(store.get_block_light(15, 50, 8), 2);
        assert_eq!(store.get_block_light(16, 50, 8), 2);
        assert_eq!(store.get_block_light(12, 50, 8), 5);
        assert_eq!(store.get_sky_light(20, 81, 8), 15);
        assert_eq!(store.get_sky_light(20, 50, 8), 0);
    }

    #[test]
    fn lightmap_brightness_follows_vanilla() {
        assert_eq!(super::lightmap_brightness(15, 0.0), 1.0);
        assert_eq!(super::lightmap_brightness(0, 0.0), 0.0);
        assert!((super::lightmap_brightness(0, 0.25) - 0.25).abs() < 1e-6);
        let v = 7.0 / 15.0;
        assert!((super::lightmap_brightness(7, 0.0) - v / (4.0 - 3.0 * v)).abs() < 1e-6);
    }
}
