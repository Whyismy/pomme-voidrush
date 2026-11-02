//! The light engine on its own thread. Void Rush rewrites terrain all the
//! time (a few hundred block changes a second), and each relight of a sky
//! section costs a couple of milliseconds, which on the render thread shows
//! up as a stutter every few frames. The worker owns the
//! [`LevelLightEngine`] and reads blocks through the same chunk `Arc`s the
//! game holds, as vanilla's server light engine reads the live chunks off
//! its own thread. The render thread sends it what changed and picks up
//! whole columns of finished light: a publish only ever follows both passes
//! of a run, so the meshers never see a light half moved.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use azalea_block::BlockState;
use parking_lot::RwLock;
use rustc_hash::FxHashSet as HashSet;

use super::world::LightChunks;
use super::{LevelLightEngine, LightDirty, LightPos, LightTask, has_different_light_properties};
use crate::world::block::BlockStateTable;
use crate::world::chunk::{Chunk, ChunkLightData, ChunkStore};

enum Message {
    /// A new level (vanilla recreates the light engine with the level).
    Reset {
        height: u32,
        min_y: i32,
        has_sky: bool,
    },
    ChunkLoaded {
        pos: (i32, i32),
        chunk: Option<Arc<RwLock<Chunk>>>,
    },
    ChunkUnloaded {
        pos: (i32, i32),
    },
    Task(LightTask),
    /// A block written with its old state known (`on_block_changed`).
    BlockChanged {
        pos: LightPos,
        old: BlockState,
        new: BlockState,
        empty_flip: Option<bool>,
    },
    /// A block written without it (`on_block_dirty`).
    BlockDirty {
        pos: LightPos,
    },
}

/// One run's results, tagged with the last message it had seen.
struct Output {
    seq: u64,
    columns: Vec<((i32, i32), Arc<ChunkLightData>)>,
    dirty: LightDirty,
}

/// The render thread's handle on the light worker; dropping it stops the
/// thread.
pub(crate) struct LightWorker {
    tx: Sender<(u64, Message)>,
    rx: Receiver<Output>,
    block_state_table: BlockStateTable,
    /// The sequence number of the last message sent.
    seq: u64,
    /// Output from before the last reset belongs to the old level.
    reset_seq: u64,
    /// Columns unloaded since, and when: output from before then is stale.
    unloaded_at: HashMap<(i32, i32), u64>,
    /// Columns whose light is on (see [`Self::light_on_in_column`]).
    lit: HashSet<(i32, i32)>,
    light_section_count: usize,
}

impl LightWorker {
    pub fn new(height: u32, min_y: i32, has_sky: bool, block_state_table: BlockStateTable) -> Self {
        let (tx, worker_rx) = mpsc::channel();
        let (worker_tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("light".into())
            .spawn(move || run(worker_rx, worker_tx, block_state_table))
            .expect("spawn the light thread");
        let mut worker = Self {
            tx,
            rx,
            block_state_table,
            seq: 0,
            reset_seq: 0,
            unloaded_at: HashMap::new(),
            lit: HashSet::default(),
            light_section_count: 0,
        };
        worker.reset(height, min_y, has_sky);
        worker
    }

    fn send(&mut self, message: Message) {
        self.seq += 1;
        // The worker only stops when this handle drops, or when it panicked,
        // which the light simply stops updating after.
        let _ = self.tx.send((self.seq, message));
    }

    /// Starts over for a new level.
    pub fn reset(&mut self, height: u32, min_y: i32, has_sky: bool) {
        self.send(Message::Reset {
            height,
            min_y,
            has_sky,
        });
        self.reset_seq = self.seq;
        self.unloaded_at.clear();
        self.lit.clear();
        self.light_section_count = (height / 16) as usize + 2;
    }

    /// See [`LevelLightEngine::light_on_in_column`], as of the last poll.
    pub fn light_on_in_column(&self, column: (i32, i32)) -> bool {
        self.lit.contains(&column)
    }

    pub fn light_section_count(&self) -> usize {
        self.light_section_count
    }

    pub fn queue_task(&mut self, task: LightTask) {
        self.send(Message::Task(task));
    }

    /// See [`LevelLightEngine::on_chunk_loaded`]; call once the chunk is in
    /// the store.
    pub fn on_chunk_loaded(&mut self, store: &ChunkStore, pos: (i32, i32)) {
        let chunk = store.chunk_at(pos);
        self.send(Message::ChunkLoaded { pos, chunk });
    }

    /// See [`LevelLightEngine::on_chunk_unloaded`]. The column goes dark
    /// here at once, as the store has just dropped its light.
    pub fn on_chunk_unloaded(&mut self, pos: (i32, i32)) {
        self.send(Message::ChunkUnloaded { pos });
        self.unloaded_at.insert(pos, self.seq);
        self.lit.remove(&pos);
    }

    /// See [`LevelLightEngine::on_block_changed`]; call with the block
    /// already written.
    pub fn on_block_changed(
        &mut self,
        pos: LightPos,
        old: BlockState,
        new: BlockState,
        empty_flip: Option<bool>,
    ) {
        if empty_flip.is_none() && !has_different_light_properties(old, new, self.block_state_table)
        {
            return;
        }
        self.send(Message::BlockChanged {
            pos,
            old,
            new,
            empty_flip,
        });
    }

    /// See [`LevelLightEngine::on_block_dirty`].
    pub fn on_block_dirty(&mut self, x: i32, y: i32, z: i32) {
        self.send(Message::BlockDirty {
            pos: LightPos::new(x, y, z),
        });
    }

    /// Takes the light the worker has finished into `store` and its remesh
    /// requests into `out`. Call once a frame.
    pub fn poll(&mut self, store: &mut ChunkStore, out: &mut LightDirty) {
        while let Ok(output) = self.rx.try_recv() {
            if output.seq < self.reset_seq {
                continue;
            }
            let fresh = |unloaded_at: &HashMap<(i32, i32), u64>, pos: &(i32, i32)| {
                unloaded_at.get(pos).is_none_or(|&at| output.seq >= at)
            };
            for (pos, column) in output.columns {
                if fresh(&self.unloaded_at, &pos) {
                    store.light_data.insert(pos, column);
                }
            }
            for (pos, on) in output.dirty.lit {
                if !fresh(&self.unloaded_at, &pos) {
                    continue;
                }
                if on {
                    self.lit.insert(pos);
                } else {
                    self.lit.remove(&pos);
                }
            }
            out.sections.extend(output.dirty.sections);
            out.columns.extend(
                output
                    .dirty
                    .columns
                    .into_iter()
                    .filter(|pos| fresh(&self.unloaded_at, pos)),
            );
            self.unloaded_at.retain(|_, at| *at > output.seq);
        }
    }
}

/// The worker's view of the level: the game's chunks, shared, and its own
/// copy of the published light, whose changed columns go back each run.
#[derive(Default)]
struct SharedChunks {
    chunks: HashMap<(i32, i32), Arc<RwLock<Chunk>>>,
    light: HashMap<(i32, i32), Arc<ChunkLightData>>,
    touched: HashSet<(i32, i32)>,
    min_y: i32,
}

impl LightChunks for SharedChunks {
    fn chunk_at(&self, pos: (i32, i32)) -> Option<Arc<RwLock<Chunk>>> {
        self.chunks.get(&pos).cloned()
    }

    fn min_y(&self) -> i32 {
        self.min_y
    }

    fn light_column_mut(&mut self, pos: (i32, i32)) -> Option<&mut Arc<ChunkLightData>> {
        let column = self.light.get_mut(&pos)?;
        self.touched.insert(pos);
        Some(column)
    }

    fn insert_light_column(&mut self, pos: (i32, i32), column: Arc<ChunkLightData>) {
        self.light.insert(pos, column);
        self.touched.insert(pos);
    }
}

fn run(rx: Receiver<(u64, Message)>, tx: Sender<Output>, block_state_table: BlockStateTable) {
    crate::frame_log::mute_this_thread();
    let mut engine = LevelLightEngine::new(0, 0, false);
    let mut world = SharedChunks::default();
    let mut seq = 0;
    loop {
        // Sleep until there's news, unless chunk light is still queued.
        if !engine.has_tasks() {
            let Ok((at, message)) = rx.recv() else {
                return;
            };
            seq = at;
            handle(message, &mut engine, &mut world, block_state_table);
        }
        loop {
            match rx.try_recv() {
                Ok((at, message)) => {
                    seq = at;
                    handle(message, &mut engine, &mut world, block_state_table);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        let mut dirty = LightDirty::default();
        engine.poll_and_run(&mut world, block_state_table, &mut dirty);
        let columns: Vec<_> = world
            .touched
            .drain()
            .filter_map(|pos| Some((pos, world.light.get(&pos)?.clone())))
            .collect();
        if (!columns.is_empty() || !dirty.is_empty())
            && tx
                .send(Output {
                    seq,
                    columns,
                    dirty,
                })
                .is_err()
        {
            return;
        }
    }
}

fn handle(
    message: Message,
    engine: &mut LevelLightEngine,
    world: &mut SharedChunks,
    block_state_table: BlockStateTable,
) {
    match message {
        Message::Reset {
            height,
            min_y,
            has_sky,
        } => {
            *engine = LevelLightEngine::new(height, min_y, has_sky);
            *world = SharedChunks {
                min_y,
                ..Default::default()
            };
        }
        Message::ChunkLoaded { pos, chunk } => {
            if let Some(chunk) = chunk {
                world.chunks.insert(pos, chunk);
            }
            engine.on_chunk_loaded(world, block_state_table, pos);
        }
        Message::ChunkUnloaded { pos } => {
            // As `ChunkStore::unload_chunk` drops the chunk and its light.
            world.chunks.remove(&pos);
            world.light.remove(&pos);
            world.touched.remove(&pos);
            engine.on_chunk_unloaded(pos);
        }
        Message::Task(task) => engine.queue_task(task),
        Message::BlockChanged {
            pos,
            old,
            new,
            empty_flip,
        } => engine.on_block_changed(block_state_table, pos, old, new, empty_flip, &|y| {
            world.block_state_at(pos.x, y, pos.z)
        }),
        Message::BlockDirty { pos } => {
            engine.on_block_dirty(&*world, block_state_table, pos.x, pos.y, pos.z);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::position::ChunkPos;

    fn light_level_15(table: BlockStateTable) -> BlockState {
        table.find_state("light", &[("level", "15"), ("waterlogged", "false")])
    }

    /// Polls until `done` holds, for at most a few seconds.
    fn poll_until(
        worker: &mut LightWorker,
        store: &mut ChunkStore,
        done: impl Fn(&ChunkStore) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done(store) {
            assert!(Instant::now() < deadline, "the light worker never finished");
            worker.poll(store, &mut LightDirty::default());
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn loaded_column() -> (ChunkStore, LightWorker, BlockStateTable) {
        let table = BlockStateTable::native();
        let mut store = ChunkStore::new(4);
        let sections = store.section_count() as usize;
        store.insert_chunk(ChunkPos::new(0, 0), Chunk::empty(sections));
        let mut worker = LightWorker::new(store.height(), store.min_y(), true, table);
        worker.on_chunk_loaded(&store, (0, 0));
        worker.queue_task(LightTask::ApplyLight {
            pos: (0, 0),
            sky: Vec::new(),
            block: Vec::new(),
            enable: true,
        });
        (store, worker, table)
    }

    #[test]
    fn a_placed_light_comes_back_lit() {
        let (mut store, mut worker, table) = loaded_column();
        let light = light_level_15(table);
        let (old, flip) = store.set_block_state_tracked(8, 70, 8, light, table);
        worker.on_block_changed(LightPos::new(8, 70, 8), old, light, flip);
        poll_until(&mut worker, &mut store, |store| {
            store.get_block_light(8, 70, 8) == 15 && store.get_block_light(11, 70, 8) == 12
        });
        assert!(worker.light_on_in_column((0, 0)));
    }

    #[test]
    fn light_from_before_an_unload_stays_out() {
        let (mut store, mut worker, table) = loaded_column();
        poll_until(&mut worker, &mut store, |store| {
            store.light_data.contains_key(&(0, 0))
        });
        let light = light_level_15(table);
        let (old, flip) = store.set_block_state_tracked(8, 70, 8, light, table);
        worker.on_block_changed(LightPos::new(8, 70, 8), old, light, flip);
        store.unload_chunk(&ChunkPos::new(0, 0));
        worker.on_chunk_unloaded((0, 0));
        let deadline = Instant::now() + Duration::from_millis(200);
        while Instant::now() < deadline {
            worker.poll(&mut store, &mut LightDirty::default());
            assert!(!store.light_data.contains_key(&(0, 0)));
            assert!(!worker.light_on_in_column((0, 0)));
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
