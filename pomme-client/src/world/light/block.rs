//! Vanilla `BlockLightEngine`; the generic drive loop lives in
//! [`LightEngine`].

use std::collections::VecDeque;

use azalea_block::BlockState;
use rustc_hash::FxHashSet as HashSet;

use super::data::DataLayer;
use super::engine::LightEngine;
use super::opacity;
use super::queue::{self as entry, Dir};
use super::storage::{LightPos, NoHooks, SectionKey, StorageCore};
use super::world::LightBlockGetter;
use crate::world::block;

pub(crate) struct BlockLightEngine {
    nodes_to_check: HashSet<LightPos>,
    decrease_queue: VecDeque<(LightPos, u64)>,
    increase_queue: VecDeque<(LightPos, u64)>,
    pub(crate) storage: StorageCore,
}

impl BlockLightEngine {
    pub fn new() -> Self {
        Self {
            nodes_to_check: HashSet::default(),
            decrease_queue: VecDeque::new(),
            increase_queue: VecDeque::new(),
            storage: StorageCore::new(),
        }
    }

    pub fn check_block(&mut self, pos: LightPos) {
        self.nodes_to_check.insert(pos);
    }

    pub fn queue_section_data(&mut self, key: SectionKey, data: Option<DataLayer>) {
        self.storage.queue_section_data(key, data);
    }

    pub fn update_section_status(&mut self, key: SectionKey, section_empty: bool) {
        self.storage
            .update_section_status(key, section_empty, &mut NoHooks);
    }

    pub fn set_light_enabled(&mut self, column: (i32, i32), enable: bool) {
        self.storage.set_light_enabled(column, enable);
    }

    /// Vanilla `BlockLightEngine.propagateLightSources`, the server's
    /// lighting of a fresh column: light on, and every emitting block
    /// spreads its light.
    pub fn propagate_light_sources(
        &mut self,
        column: (i32, i32),
        chunk: &crate::world::chunk::Chunk,
        min_y: i32,
        block_state_table: block::BlockStateTable,
    ) {
        self.storage.set_light_enabled(column, true);
        let state_of = |id: u32| block_state_table.try_state(id);
        let emits = |id: u32| {
            state_of(id).is_some_and(|state| {
                block_state_table
                    .block_state_data(state)
                    .light_props()
                    .emission
                    > 0
            })
        };
        for (index, section) in chunk.sections.iter().enumerate() {
            if section.block_count == 0 || !section.states.may_contain(emits) {
                continue;
            }
            for y in 0..16 {
                for z in 0..16 {
                    for x in 0..16 {
                        let Some(state) = state_of(section.states.get(x, y, z)) else {
                            continue;
                        };
                        let data = block_state_table.block_state_data(state);
                        let emission = data.light_props().emission;
                        if emission == 0 {
                            continue;
                        }
                        let pos = LightPos::new(
                            column.0 * 16 + x as i32,
                            min_y + index as i32 * 16 + y as i32,
                            column.1 * 16 + z as i32,
                        );
                        self.increase_queue.push_back((
                            pos,
                            entry::increase_light_from_emission(emission, data.is_empty_shape()),
                        ));
                    }
                }
            }
        }
    }

    fn emission(&self, pos: LightPos, block_state_data: &block::BlockStateData) -> u8 {
        let emission = block_state_data.light_props().emission;
        if emission > 0 && self.storage.light_on_in_section(pos.section()) {
            emission
        } else {
            0
        }
    }
}

impl LightEngine for BlockLightEngine {
    fn storage(&mut self) -> &mut StorageCore {
        &mut self.storage
    }

    fn nodes_to_check(&mut self) -> &mut HashSet<LightPos> {
        &mut self.nodes_to_check
    }

    fn increase_queue(&mut self) -> &mut VecDeque<(LightPos, u64)> {
        &mut self.increase_queue
    }

    fn decrease_queue(&mut self) -> &mut VecDeque<(LightPos, u64)> {
        &mut self.decrease_queue
    }

    fn mark_new_inconsistencies(&mut self) {
        self.storage.mark_new_inconsistencies(&mut NoHooks);
    }

    fn check_node(&mut self, world: &impl LightBlockGetter, pos: LightPos) {
        if !self.storage.storing_light_for_section(pos.section()) {
            return;
        }
        let state = world.state(pos);
        let block_state_data = world.block_state_table().block_state_data(state);
        let emission = self.emission(pos, block_state_data);
        let old_level = self.storage.get_stored_level(pos);
        if emission < old_level {
            self.storage.set_stored_level(pos, 0);
            self.decrease_queue
                .push_back((pos, entry::decrease_all_directions(old_level)));
        } else {
            self.decrease_queue
                .push_back((pos, entry::PULL_LIGHT_IN_ENTRY));
        }
        if emission > 0 {
            self.increase_queue.push_back((
                pos,
                entry::increase_light_from_emission(emission, block_state_data.is_empty_shape()),
            ));
        }
    }

    fn propagate_increase(
        &mut self,
        world: &impl LightBlockGetter,
        from: LightPos,
        data: u64,
        from_level: u8,
    ) {
        let block_state_table = world.block_state_table();
        let mut from_state: Option<BlockState> = None;
        for dir in Dir::ALL {
            if !entry::should_propagate_in_direction(data, dir) {
                continue;
            }
            let to = from.offset(dir);
            if !self.storage.storing_light_for_section(to.section()) {
                continue;
            }
            let to_level = self.storage.get_stored_level(to) as i32;
            if from_level as i32 - 1 <= to_level {
                continue;
            }
            let to_state = world.state(to);
            let new_to_level =
                from_level as i32 - opacity(block_state_table.block_state_data(to_state)) as i32;
            if new_to_level <= to_level {
                continue;
            }
            let from_state = *from_state.get_or_insert_with(|| {
                if entry::is_from_empty_shape(data) {
                    BlockState::AIR
                } else {
                    world.state(from)
                }
            });
            if block::shape_occludes_data(
                block_state_table.block_state_data(from_state),
                block_state_table.block_state_data(to_state),
                dir as usize,
            ) {
                continue;
            }
            self.storage.set_stored_level(to, new_to_level as u8);
            if new_to_level <= 1 {
                continue;
            }
            self.increase_queue.push_back((
                to,
                entry::increase_skip_one_direction(
                    new_to_level as u8,
                    block_state_table
                        .block_state_data(to_state)
                        .is_empty_shape(),
                    dir.opposite(),
                ),
            ));
        }
    }

    fn propagate_decrease(&mut self, world: &impl LightBlockGetter, from: LightPos, data: u64) {
        let old_from_level = entry::get_from_level(data) as i32;
        for dir in Dir::ALL {
            if !entry::should_propagate_in_direction(data, dir) {
                continue;
            }
            let to = from.offset(dir);
            if !self.storage.storing_light_for_section(to.section()) {
                continue;
            }
            let to_level = self.storage.get_stored_level(to);
            if to_level == 0 {
                continue;
            }
            if (to_level as i32) < old_from_level {
                let to_state = world.state(to);
                let to_block_state_data = world.block_state_table().block_state_data(to_state);
                let to_emission = self.emission(to, to_block_state_data);
                self.storage.set_stored_level(to, 0);
                if to_emission < to_level {
                    self.decrease_queue.push_back((
                        to,
                        entry::decrease_skip_one_direction(to_level, dir.opposite()),
                    ));
                }
                if to_emission > 0 {
                    self.increase_queue.push_back((
                        to,
                        entry::increase_light_from_emission(
                            to_emission,
                            to_block_state_data.is_empty_shape(),
                        ),
                    ));
                }
            } else {
                // A brighter independent neighbor: re-inject its light back
                // toward the darkened area.
                self.increase_queue.push_back((
                    to,
                    entry::increase_only_one_direction(to_level, false, dir.opposite()),
                ));
            }
        }
    }
}
