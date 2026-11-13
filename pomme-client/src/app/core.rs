use std::collections::{HashMap, HashSet};
use std::ops::Add;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use azalea_protocol::packets::game::s_player_input::ServerboundPlayerInput;
use azalea_protocol::packets::game::{
    ServerboundClientCommand, ServerboundGamePacket, s_client_command, s_client_tick_end,
};
use glam::{FloatExt, dvec3};
use winit::keyboard::KeyCode;
use winit::monitor::MonitorHandle;
use winit::window::{CursorGrabMode, Fullscreen, Window};

use crate::app::input::{Action, InputState, STICK_MOVEMENT_THRESHOLD};
use crate::app::level_load::ReadyInputs;
use crate::app::phases::in_game::GameState;
use crate::app::phases::{ConnectionPhase, Gfx};
use crate::app::{POSITION_SEND_INTERVAL, POSITION_THRESHOLD_SQ};
use crate::assets::AssetIndex;
use crate::attribute::{AttributeInstance, AttributeKind, AttributeMap};
use crate::dirs::DataDirs;
use crate::discord::DiscordPresence;
use crate::entity::components::{LookDirection, Position, Velocity};
use crate::net::connection::ConnectionHandle;
use crate::net::{DimensionInfo, NetworkEvent};
use crate::physics::movement;
use crate::player::LocalPlayer;
use crate::player::tab_list::{PlayerChatValidation, TabList};
use crate::renderer::Renderer;
use crate::renderer::chunk::mesh_source::MeshSource;
use crate::resource_pack::ResourcePackManager;
use crate::ui::menu::{
    CREDITS_KEY_CTRL_L, CREDITS_KEY_CTRL_R, CREDITS_KEY_SPACE, CREDITS_KEY_UP, MainMenu, MenuInput,
};
use crate::ui::text::InlineObject;
use crate::user::UserData;
use crate::world::block::block_registry::BlockDefinitions;
use crate::world::chunk::ChunkStore;
use crate::world::environment::EnvironmentProbe;

pub struct PendingPackDownload {
    pub id: uuid::Uuid,
    pub required: bool,
    pub hash: String,
    pub handle: std::thread::JoinHandle<PackDownloadResult>,
}

pub type PackDownloadResult = Result<std::path::PathBuf, crate::resource_pack::PackError>;

#[derive(Clone, Debug, PartialEq, Eq)]
enum PlayerSkinSource {
    Uuid,
    Textures(String),
    Name(String),
}

/// Fetches the skin a source names; `uuid` is the profile id a `Uuid` source
/// looks up.
async fn fetch_skin(
    source: &PlayerSkinSource,
    uuid: Option<uuid::Uuid>,
) -> Result<crate::renderer::SkinData, String> {
    match source {
        PlayerSkinSource::Textures(textures) => {
            crate::renderer::fetch_skin_texture_from_profile_property(textures).await
        }
        PlayerSkinSource::Name(name) => crate::renderer::fetch_skin_texture_by_name(name).await,
        PlayerSkinSource::Uuid => {
            let uuid = uuid.unwrap_or_default().to_string().replace('-', "");
            crate::renderer::fetch_skin_texture(&uuid).await
        }
    }
}

struct PlayerSkinResult {
    uuid: uuid::Uuid,
    source: PlayerSkinSource,
    result: Result<crate::renderer::SkinData, String>,
}

/// The identity vanilla's `PlayerSkinRenderCache` keys on: a whole
/// `ResolvableProfile`, so two heads with the same id but different textures
/// are different entries.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct HeadProfile {
    id: Option<uuid::Uuid>,
    name: Option<String>,
    textures: Option<String>,
}

impl HeadProfile {
    /// `ResolvableProfile.create`: a profile carrying properties, or both or
    /// neither of id and name, is `Static` and never looks anything up; one
    /// with exactly one of them is `Dynamic` and resolves first.
    fn is_static(&self) -> bool {
        self.textures.is_some() || self.id.is_some() == self.name.is_some()
    }
}

enum HeadState {
    Pending,
    Ready {
        hat: Vec<u8>,
        base: Vec<u8>,
    },
    /// No skin to show: a static profile with no textures, or a lookup that
    /// failed. Cached so it isn't retried (and re-logged) every frame.
    // TODO: vanilla falls back to `DefaultPlayerSkin.get(uuid)` (one of 18
    // skins by `floorMod(uuid.hashCode(), 18)`, see `java_uuid_hash` in
    // world/waypoints.rs); Pomme draws wide Steve everywhere, entities
    // included.
    Fallback,
}

struct HeadEntry {
    state: HeadState,
    last_used: Instant,
}

struct HeadResult {
    profile: HeadProfile,
    result: Result<crate::renderer::SkinData, String>,
}

/// A glyph the text drew: its tile (or an animated sprite's frames), and when
/// it was last drawn (`PlayerSkinRenderCache.CACHE_DURATION`).
struct InlineObjectEntry {
    content: InlineObjectContent,
    last_used: Instant,
}

enum InlineObjectContent {
    /// A stitched sprite's frames, and the one showing now.
    Sprite {
        frames: crate::ui::object_glyph::SpriteFrames,
        frame: usize,
    },
    /// A head: the profile whose skin it shows, and its layer.
    Head { profile: HeadProfile, hat: bool },
}

/// Vanilla `PlayerSkinRenderCache.CACHE_DURATION`.
const GLYPH_CACHE_DURATION: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Applies a server-driven block change: block-entity sync, prediction
/// absorption, the block + light write, and the remesh cascade. The block
/// entity syncs even when a pending prediction absorbs the update (the state
/// is applied later in `acknowledge`), so e.g. a chest placed where a break
/// was just predicted still gets its entry.
fn apply_server_block(
    game: &mut GameState,
    priority_remesh: &mut Vec<(crate::position::ChunkPos, i32)>,
    pos: crate::position::BlockPos,
    state: azalea_block::BlockState,
) {
    sync_block_entities(game, pos, state);
    if game.interaction.update_known_server_state(&pos, state) {
        return;
    }
    write_block(game, priority_remesh, pos, state);
}

/// Vanilla `Level.setBlock` from the client's own code, which no pending
/// prediction absorbs.
fn set_client_block(
    game: &mut GameState,
    priority_remesh: &mut Vec<(crate::position::ChunkPos, i32)>,
    pos: crate::position::BlockPos,
    state: azalea_block::BlockState,
) {
    sync_block_entities(game, pos, state);
    write_block(game, priority_remesh, pos, state);
}

fn sync_block_entities(
    game: &mut GameState,
    pos: crate::position::BlockPos,
    state: azalea_block::BlockState,
) {
    let block_state_data = game.block_registry.block_state_data(state);
    // Vanilla drops a moving block's entity once its cell holds anything else.
    if block_state_data.id() != "moving_piston" {
        game.block_entity_anim.pistons.remove(&pos);
    }
    crate::world::block_entity::sync_block_entity(
        &mut game.chunk_store.block_entities,
        pos,
        block_state_data,
    );
}

/// The block + light write and the remesh cascade.
fn write_block(
    game: &mut GameState,
    priority_remesh: &mut Vec<(crate::position::ChunkPos, i32)>,
    pos: crate::position::BlockPos,
    state: azalea_block::BlockState,
) {
    if is_end_portal(game.block_registry.block_state_table(), state) {
        game.end_portals.insert(pos);
    } else {
        game.end_portals.remove(&pos);
    }
    let (old, empty_flip) = game.chunk_store.set_block_state_tracked(
        pos.x,
        pos.y,
        pos.z,
        state,
        game.block_registry.block_state_table(),
    );
    game.light_engine.on_block_changed(
        crate::world::light::LightPos::new(pos.x, pos.y, pos.z),
        old,
        state,
        empty_flip,
    );
    dirty_sections_for_block(
        priority_remesh,
        pos.x,
        pos.y,
        pos.z,
        game.chunk_store.min_y(),
        game.chunk_store.section_count(),
    );
}

/// The client world as the piston code sees it. Destroyed blocks' effects
/// wait for the caller, which owns the audio and the particle registry.
struct ClientPistonLevel<'a> {
    game: &'a mut GameState,
    remesh: &'a mut Vec<(crate::position::ChunkPos, i32)>,
    destroyed: Vec<(crate::position::BlockPos, azalea_block::BlockState)>,
}

impl<'a> ClientPistonLevel<'a> {
    fn new(game: &'a mut GameState, remesh: &'a mut Vec<(crate::position::ChunkPos, i32)>) -> Self {
        Self {
            game,
            remesh,
            destroyed: Vec::new(),
        }
    }
}

impl crate::world::piston::PistonLevel for ClientPistonLevel<'_> {
    fn block_registry(&self) -> &crate::world::block::block_registry::BlockRegistry {
        &self.game.block_registry
    }

    fn block(&self, pos: crate::position::BlockPos) -> azalea_block::BlockState {
        self.game.chunk_store.get_block_state(pos.x, pos.y, pos.z)
    }

    fn set_block(&mut self, pos: crate::position::BlockPos, state: azalea_block::BlockState) {
        set_client_block(self.game, self.remesh, pos, state);
    }

    fn is_loaded(&self, pos: crate::position::BlockPos) -> bool {
        let chunk = crate::position::ChunkPos::new(pos.x.div_euclid(16), pos.z.div_euclid(16));
        self.game.chunk_store.get_chunk(&chunk).is_some()
    }

    fn min_y(&self) -> i32 {
        self.game.chunk_store.min_y()
    }

    fn max_y(&self) -> i32 {
        self.game.chunk_store.min_y() + self.game.chunk_store.height() as i32 - 1
    }

    fn pistons(&mut self) -> &mut crate::world::piston::MovingPistons {
        &mut self.game.block_entity_anim.pistons
    }

    fn destroy_effect(&mut self, pos: crate::position::BlockPos, state: azalea_block::BlockState) {
        self.destroyed.push((pos, state));
    }
}

/// Vanilla `PistonMovingBlockEntity.tick` for the client's moving blocks, at
/// the block-entity point of the game tick; their landings remesh at once.
pub(crate) fn tick_moving_pistons(game: &mut GameState) {
    let mut remesh = Vec::new();
    let mut level = ClientPistonLevel::new(game, &mut remesh);
    let tick = level.game.tick_count;
    crate::world::piston::tick(&mut level, |level, pos, piston, new_progress| {
        let game = &mut *level.game;
        // A passenger moves with its vehicle, which the server moves.
        if game.position_set && game.riding_vehicle_id.is_none() {
            // TODO: vanilla's client also moves other entities in the way;
            // the server's position updates do it here.
            let world = crate::physics::collision::CollisionWorld::with_block_entities(
                &game.chunk_store,
                game.block_registry.block_state_table(),
                &game.block_entity_anim,
            );
            movement::push_by_moving_piston(
                &mut game.player,
                world,
                pos,
                piston,
                new_progress,
                tick,
            );
        }
    });
    enqueue_edits(game, &remesh);
}

/// Marks edited sections for remeshing as recompiles, ungated by visibility.
fn enqueue_edits(game: &mut GameState, sections: &[(crate::position::ChunkPos, i32)]) {
    for &(col, si) in sections {
        game.enqueue_section_edit(col, si);
    }
}

/// The texture of what `e` holds in its main hand, for its eating crumbs.
fn mouth_item_texture(e: &crate::entity::LivingEntity) -> Option<String> {
    let azalea_inventory::ItemStack::Present(held) =
        e.equipment(azalea_inventory::components::EquipmentSlot::Mainhand)?
    else {
        return None;
    };
    Some(format!(
        "item/{}",
        crate::player::inventory::item_resource_name(held.kind)
    ))
}

/// The client half of `Panda.addEatingParticles`, flagged by the entity tick
/// (the sound is the server's broadcast).
fn panda_eating_particles(game: &mut GameState, camera_pos: glam::DVec3) {
    for e in game.entity_store.living.values() {
        if e.entity_type != azalea_registry::builtin::EntityKind::Panda || !e.panda.eat_particles {
            continue;
        }
        let Some(texture) = mouth_item_texture(e) else {
            continue;
        };
        game.particle_store.add_panda_eating_particles(
            &texture,
            e.position.into(),
            glam::DVec3::from(e.eye_position(1.0)).y,
            e.look_dir.x_rot_deg(),
            e.look_dir.y_rot_deg(),
            e.body_y_rot_deg,
            camera_pos,
        );
    }
}

/// A sound following an entity, skipped while it's silent (vanilla
/// `EntityBoundSoundInstance.canPlaySound`); the playing instance's id.
#[allow(clippy::too_many_arguments)]
pub(super) fn play_entity_sound(
    audio: &mut crate::audio::AudioEngine,
    game: &mut GameState,
    entity_id: i32,
    sound: &crate::audio::SoundRef,
    category: u8,
    volume: f32,
    pitch: f32,
    seed: u64,
) -> Option<u64> {
    if game.silent_entities.contains(&entity_id) {
        return None;
    }
    let pos = (entity_id == game.player.entity_id)
        .then_some(game.player.position)
        .or_else(|| game.entity_positions.get(&entity_id).copied())?;
    #[cfg(feature = "test-harness")]
    game.record_sound(sound);
    audio.play_entity_sound(
        sound,
        category,
        crate::audio::EntitySoundTarget { id: entity_id, pos },
        volume,
        pitch,
        seed,
    )
}

/// Vanilla `LevelEventHandler` case 2001: a broken block's sound and
/// particles.
fn block_break_effect(
    audio: &crate::audio::AudioEngine,
    game: &mut GameState,
    pos: crate::position::BlockPos,
    state: azalea_block::BlockState,
) {
    let block_state_data = game.block_registry.block_state_data(state);
    if !block_state_data.is_air() {
        crate::player::interaction::play_break_sound_for_id(audio, block_state_data.id(), pos);
    }
    game.particle_store.add_destroy_block_effect(
        pos,
        state,
        game.block_registry.render_view(),
        &game.chunk_store,
        &game.biome_climate,
    );
}

/// The local player's box as a push target, gated on `LivingEntity.isPushable`
/// and `EntitySelector.NO_SPECTATORS`.
fn local_push_box(game: &GameState) -> Option<crate::physics::aabb::Aabb> {
    (game.player.health > 0.0
        && !crate::player::is_spectator(game.player.game_mode)
        && !movement::on_climbable(
            &game.player,
            &game.chunk_store,
            game.block_registry.block_state_table(),
            game.block_registry.tags(),
        ))
    .then(|| game.player.bounding_box())
}

/// Starts an entity's hurt animation, with a direction for the packets that
/// carry one. The local player lives outside the entity store.
fn hurt_entity(game: &mut GameState, id: i32, dir: Option<f32>) {
    if id != game.player.entity_id {
        game.entity_store.mark_hurt(id);
    } else if let Some(yaw) = dir {
        game.player.animate_hurt(yaw);
    } else {
        game.player.mark_hurt();
    }
}

/// The level's chunk, light and position updates, shared by the windowed
/// client and the headless chunk benchmark; the caller does any renderer work.
impl GameState {
    pub fn select_protocol(&mut self, protocol: i32) {
        let previous_slot = self.block_registry.block_state_table().table_slot();
        self.block_registry.select_protocol(protocol);
        self.light_engine
            .select_block_state_table(self.block_registry.block_state_table());
        if previous_slot != self.block_registry.block_state_table().table_slot() {
            self.mesh_dispatcher = self
                .mesh_dispatcher
                .recreate_for_version(self.block_registry.owned_render_view());
        }
    }

    pub fn set_biome_climate(
        &mut self,
        colors: HashMap<u32, crate::renderer::chunk::mesher::BiomeClimate>,
    ) {
        tracing::info!("Received {} biome climate entries", colors.len());
        self.biome_climate = Arc::new(colors);
        self.mesh_dispatcher
            .set_biome_climate(Arc::clone(&self.biome_climate));
    }

    /// Resets the level for a new dimension, with a mesh dispatcher for its
    /// shading. The caller clears the old meshes.
    pub fn enter_dimension(
        &mut self,
        dim: DimensionInfo,
        render_distance: u32,
        mesh_source: &impl MeshSource,
    ) {
        let DimensionInfo {
            height,
            min_y,
            has_skylight,
            cardinal_light,
            ambient_light,
            default_clock,
            flat,
            environment,
        } = dim;
        tracing::info!(
            "Dimension: height={height}, min_y={min_y}, skylight={has_skylight}, cardinal_light={cardinal_light:?}"
        );
        self.cardinal_light = cardinal_light;
        self.ambient_light = ambient_light;
        self.default_clock = default_clock;
        self.flat = flat;
        self.environment = *environment;
        // Vanilla `EnvironmentAttributeProbe.reset`: no biomes until the
        // next tick samples the new level's.
        self.biome_weights = Default::default();
        self.environment_probe = EnvironmentProbe::new(self.resolve_environment());
        self.chunk_store = ChunkStore::new_with_dimension(render_distance, height, min_y);
        self.light_engine.reset(height, min_y, has_skylight);
        // The old level's block entities, moving blocks among them.
        self.block_entity_anim = Default::default();
        self.end_portals.clear();
        // `ClientLevel.mapData` goes with the level.
        self.maps.clear();
        self.position_set = false;
        // Login/respawn recreate vanilla's LocalPlayer, resetting
        // the XP display sentinel; waypoints persist.
        self.xp_display_start_tick = i64::MIN;
        self.controlled_vehicle_id = None;
        self.riding_vehicle_id = None;
        self.player.jump_riding_ticks = 0;
        self.player.jump_riding_scale = 0.0;
        self.player.reset_hurt_state();
        self.player.cooldowns.clear();
        // The meshes went with the old level, so their bookkeeping
        // has to go too, or the new level's camera section reads as
        // already meshed.
        self.section_vis.clear();
        self.section_vis_epoch.clear();
        self.compiled.clear();
        self.clear_visibility();
        self.mesh_dispatcher = mesh_source.create_mesh_dispatcher(
            self.block_registry.owned_render_view(),
            Arc::clone(&self.biome_climate),
            None,
            cardinal_light,
        );
        self.reset_mesh_columns();
    }

    pub fn load_chunk(
        &mut self,
        pos: crate::position::ChunkPos,
        data: &[u8],
        heightmaps: &[(crate::world::chunk::HeightmapKind, Box<[u64]>)],
        light: &crate::net::PacketLightData,
    ) {
        let biomes = crate::net::connection::biome_count(&self.registries);
        if let Err(e) = self.chunk_store.load_chunk(
            pos,
            data,
            heightmaps,
            biomes,
            self.block_registry.block_state_table(),
        ) {
            tracing::error!("Failed to load chunk [{}, {}]: {e}", pos.x, pos.z);
            return;
        }
        self.light_engine
            .on_chunk_loaded(&self.chunk_store, (pos.x, pos.z));
        self.find_end_portals(pos);
        // The column meshes once its queued light applies (vanilla
        // schedules the rebuild from enableChunkLight, not here).
        queue_light_apply(self, pos, light, true);
    }

    /// Vanilla `handleChunksBiomes`: the columns' new biomes, then every
    /// loaded column around them remeshes, since biome tints blend across
    /// column edges.
    pub fn replace_biomes(&mut self, chunks: &[(crate::position::ChunkPos, Box<[u8]>)]) {
        let biomes = crate::net::connection::biome_count(&self.registries);
        for (pos, data) in chunks {
            if let Err(e) = self.chunk_store.replace_biomes(*pos, data, biomes) {
                tracing::error!("Failed to replace biomes in [{}, {}]: {e}", pos.x, pos.z);
            }
        }
        for (pos, _) in chunks {
            for column in crate::world::chunk::column_neighborhood(*pos) {
                if self.chunk_store.get_chunk(&column).is_some() {
                    self.mark_column_dirty(column);
                }
            }
        }
    }

    pub fn apply_light_update(
        &mut self,
        pos: crate::position::ChunkPos,
        light: &crate::net::PacketLightData,
    ) {
        queue_light_apply(self, pos, light, false);
    }

    /// Notes a freshly loaded column's end portal and gateway blocks: only
    /// sections whose palette may hold one are read.
    fn find_end_portals(&mut self, pos: crate::position::ChunkPos) {
        self.end_portals
            .retain(|p| p.x.div_euclid(16) != pos.x || p.z.div_euclid(16) != pos.z);
        let table = self.block_registry.block_state_table();
        let wanted = |id: u32| {
            table
                .try_state(id)
                .is_some_and(|state| is_end_portal(table, state))
        };
        let Some(chunk) = self.chunk_store.get_chunk(&pos) else {
            return;
        };
        let min_y = self.chunk_store.min_y();
        let chunk = chunk.read();
        for (index, section) in chunk.sections.iter().enumerate() {
            if section.block_count == 0 || !section.states.may_contain(wanted) {
                continue;
            }
            for y in 0..16 {
                for z in 0..16 {
                    for x in 0..16 {
                        if wanted(section.states.get(x, y, z)) {
                            self.end_portals.insert(crate::position::BlockPos::new(
                                pos.x * 16 + x as i32,
                                min_y + index as i32 * 16 + y as i32,
                                pos.z * 16 + z as i32,
                            ));
                        }
                    }
                }
            }
        }
    }

    /// The caller removes the column's mesh.
    pub fn unload_chunk(&mut self, pos: crate::position::ChunkPos) {
        self.chunk_store.unload_chunk(&pos);
        self.block_entity_anim.drop_chunk(pos.x, pos.z);
        self.light_engine.on_chunk_unloaded((pos.x, pos.z));
        self.end_portals
            .retain(|p| p.x.div_euclid(16) != pos.x || p.z.div_euclid(16) != pos.z);
        self.light_engine
            .queue_task(crate::world::light::LightTask::Remove {
                pos: (pos.x, pos.z),
            });
        self.forget_mesh_column(pos);
        self.compiled.remove(&pos);
        if let Some(mask) = self.in_view_masks.remove(&pos) {
            let sections = self.chunk_store.section_count() as u32;
            self.mesh_gate.remove(mask, sections);
        }
        self.section_vis.retain(|(p, _), _| *p != pos);
        self.section_vis_epoch.retain(|(p, _), _| *p != pos);
    }

    /// Vanilla `handleMovePlayer`. Returns whether the player moved, for the
    /// caller to reset the camera.
    pub fn apply_player_position(
        &mut self,
        connection: &ConnectionHandle,
        id: u32,
        change: &pomme_protocol::entity::PositionMoveRotation,
        relative: &pomme_protocol::entity::Relative,
    ) -> bool {
        let is_passenger = self.riding_vehicle_id.is_some();
        let correction =
            resolve_player_position_correction(&self.player, change, relative, is_passenger);
        if let Some(correction) = &correction {
            self.player.position = correction.position;
            self.player.prev_position = correction.prev_position;
            self.player.velocity = correction.velocity;
            self.player.look_dir = correction.look_dir;
            self.player.prev_look_dir = correction.prev_look_dir;

            let to_chunk_coord = |v: f64| (v.floor() as i32).div_euclid(16);
            self.chunk_store.set_center(crate::position::ChunkPos::new(
                to_chunk_coord(correction.position.x),
                to_chunk_coord(correction.position.z),
            ));

            if !self.position_set {
                self.position_set = true;
                tracing::info!(
                    "Player position set to ({:.1}, {:.1}, {:.1})",
                    correction.position.x,
                    correction.position.y,
                    correction.position.z
                );
            }
        }

        // Vanilla `handleMovePlayer` acks and echoes the pose even
        // when mounted (a 26.3 wire folds the two).
        connection.packet_tx.send(ServerboundGamePacket::AcceptTeleportation(
            azalea_protocol::packets::game::s_accept_teleportation::ServerboundAcceptTeleportation { id },
        ));
        connection
            .packet_tx
            .send(ServerboundGamePacket::MovePlayerPosRot(
            azalea_protocol::packets::game::s_move_player_pos_rot::ServerboundMovePlayerPosRot {
                pos: self.player.position.into(),
                look_direction: self.player.look_dir.into(),
                flags: Default::default(),
            },
        ));
        self.interaction.on_teleport();
        correction.is_some()
    }

    pub fn set_server_view_distance(&mut self, distance: u32) {
        tracing::info!("Server view distance: {distance}");
        if let Some(d) = server_view_distance_update(distance, self.last_render_distance) {
            self.server_render_distance = d;
        }
    }
}

/// Queues a column's packet light for the per-tick apply. Chunk loads enable
/// the column, standalone light updates are corrections.
fn queue_light_apply(
    game: &mut GameState,
    pos: crate::position::ChunkPos,
    light: &crate::net::PacketLightData,
    enable: bool,
) {
    let count = game.light_engine.light_section_count();
    game.light_engine
        .queue_task(crate::world::light::LightTask::ApplyLight {
            pos: (pos.x, pos.z),
            sky: crate::world::light::section_entries(
                count,
                &light.sky_y_mask,
                &light.empty_sky_y_mask,
                &light.sky_updates[..],
            ),
            block: crate::world::light::section_entries(
                count,
                &light.block_y_mask,
                &light.empty_block_y_mask,
                &light.block_updates[..],
            ),
            enable,
        });
}

/// Mirror of vanilla `LevelExtractor.setBlockDirty`: a block at (x,y,z) dirties
/// its own 16³ section plus any neighbour section it touches when on a boundary
/// (the 3×3×3-block cascade → up to a few sections). Pushes deduped
/// `(column, section_index)` keys.
fn dirty_sections_for_block(
    out: &mut Vec<(crate::position::ChunkPos, i32)>,
    x: i32,
    y: i32,
    z: i32,
    min_y: i32,
    section_count: i32,
) {
    for bz in (z - 1)..=(z + 1) {
        for bx in (x - 1)..=(x + 1) {
            for by in (y - 1)..=(y + 1) {
                let si = (by - min_y).div_euclid(16);
                if si < 0 || si >= section_count {
                    continue;
                }
                let col = crate::position::ChunkPos::new(bx.div_euclid(16), bz.div_euclid(16));
                let key = (col, si);
                if !out.contains(&key) {
                    out.push(key);
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DisplayMode {
    Windowed,
    Borderless,
    Fullscreen,
}

impl DisplayMode {
    pub fn cycle(self) -> Self {
        match self {
            Self::Windowed => Self::Borderless,
            Self::Borderless => Self::Fullscreen,
            Self::Fullscreen => Self::Windowed,
        }
    }

    /// The fullscreen key (vanilla `Window.toggleFullScreen`): out of a
    /// window into `fullscreen`, the kind of fullscreen last used, and out
    /// of either kind of fullscreen back into a window. One press always
    /// changes what is on screen; the options screen's own button is what
    /// steps through all three.
    pub fn toggled(self, fullscreen: Self) -> Self {
        match self {
            Self::Windowed if fullscreen == Self::Windowed => Self::Borderless,
            Self::Windowed => fullscreen,
            Self::Borderless | Self::Fullscreen => Self::Windowed,
        }
    }

    pub fn to_u8(self) -> u8 {
        match self {
            Self::Windowed => 0,
            Self::Borderless => 1,
            Self::Fullscreen => 2,
        }
    }

    pub fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Borderless,
            2 => Self::Fullscreen,
            _ => Self::Windowed,
        }
    }

    /// Shared by window creation and the runtime switch so the two can't drift.
    pub fn fullscreen_for(self, monitor: Option<MonitorHandle>) -> Option<Fullscreen> {
        match self {
            Self::Windowed => None,
            Self::Borderless => Some(Fullscreen::Borderless(None)),
            Self::Fullscreen => {
                let video_mode = monitor.and_then(|m| {
                    m.video_modes().max_by_key(|v| {
                        (v.refresh_rate_millihertz(), v.size().width, v.size().height)
                    })
                });
                Some(match video_mode {
                    Some(mode) => Fullscreen::Exclusive(mode),
                    None => Fullscreen::Borderless(None),
                })
            }
        }
    }
}

#[derive(Default, PartialEq)]
pub struct PlayerInputState {
    forward: bool,
    backward: bool,
    left: bool,
    right: bool,
    jump: bool,
    shift: bool,
    sprint: bool,
}

fn player_input_state(
    input: &InputState,
    analog_move: glam::Vec2,
    sprint_key_down: bool,
) -> PlayerInputState {
    PlayerInputState {
        forward: input.action_held(Action::Forward) || analog_move.y > STICK_MOVEMENT_THRESHOLD,
        backward: input.action_held(Action::Backward) || analog_move.y < -STICK_MOVEMENT_THRESHOLD,
        left: input.action_held(Action::StrafeLeft) || analog_move.x > STICK_MOVEMENT_THRESHOLD,
        right: input.action_held(Action::StrafeRight) || analog_move.x < -STICK_MOVEMENT_THRESHOLD,
        jump: input.performing_action(Action::Jump),
        shift: input.performing_action(Action::Sneak),
        sprint: sprint_key_down,
    }
}

/// `base + value` for a relative field, `value` for an absolute one.
fn resolve<T: Add<Output = T>>(base: T, is_relative: bool, value: T) -> T {
    if is_relative { base + value } else { value }
}

fn resolve_corrected_look(
    current: LookDirection,
    y_rot: f32,
    x_rot: f32,
    relative_y: bool,
    relative_x: bool,
) -> LookDirection {
    // Vanilla `PositionMoveRotation.calculateAbsolute` clamps pitch before
    // `Entity.setXRot` applies its own modulo and clamp.
    LookDirection::new(
        resolve(current.y_rot_deg(), relative_y, y_rot),
        resolve(current.x_rot_deg(), relative_x, x_rot).clamp(-90.0, 90.0),
    )
}

struct ResolvedPlayerCorrection {
    position: Position,
    prev_position: Position,
    velocity: Velocity,
    look_dir: LookDirection,
    prev_look_dir: LookDirection,
}

fn resolve_player_position_correction(
    player: &LocalPlayer,
    change: &pomme_protocol::entity::PositionMoveRotation,
    relative: &pomme_protocol::entity::Relative,
    is_passenger: bool,
) -> Option<ResolvedPlayerCorrection> {
    if is_passenger {
        return None;
    }

    // Vanilla `setValuesFromPositionPacket` resolves the old pose through the
    // same `calculateAbsolute`.
    let resolve_position = |base: Position| {
        Position::new(
            resolve(base.x, relative.x, change.position.x),
            resolve(base.y, relative.y, change.position.y),
            resolve(base.z, relative.z, change.position.z),
        )
    };
    let resolve_look = |base| {
        resolve_corrected_look(
            base,
            change.y_rot,
            change.x_rot,
            relative.y_rot,
            relative.x_rot,
        )
    };
    let position = resolve_position(player.position);
    let prev_position = resolve_position(player.prev_position);
    let look_dir = resolve_look(player.look_dir);
    let prev_look_dir = resolve_look(player.prev_look_dir);

    let mut velocity = player.velocity;
    if relative.rotate_delta {
        let x_rot_delta = player.look_dir.x_rot_deg() - look_dir.x_rot_deg();
        let y_rot_delta = player.look_dir.y_rot_deg() - look_dir.y_rot_deg();
        velocity = velocity
            .x_rot(x_rot_delta.to_radians() as f64)
            .y_rot(y_rot_delta.to_radians() as f64);
    }
    velocity = Velocity::new(
        resolve(velocity.x, relative.delta_x, change.delta.x),
        resolve(velocity.y, relative.delta_y, change.delta.y),
        resolve(velocity.z, relative.delta_z, change.delta.z),
    );

    Some(ResolvedPlayerCorrection {
        position,
        prev_position,
        velocity,
        look_dir,
        prev_look_dir,
    })
}

fn serverbound_player_input(state: &PlayerInputState) -> ServerboundPlayerInput {
    ServerboundPlayerInput {
        forward: state.forward,
        backward: state.backward,
        left: state.left,
        right: state.right,
        jump: state.jump,
        shift: state.shift,
        sprint: state.sprint,
    }
}

// GLFW's key-repeat shape, so a held DPad steps like a held arrow key
// instead of once per rendered frame.
const MENU_REPEAT_DELAY: f32 = 0.4;
const MENU_REPEAT_INTERVAL: f32 = 1.0 / 30.0;

/// Edge-plus-repeat stepping for a held direction. The gamepad exposes level
/// state only, which would step once per frame and tie the rate to the
/// framerate.
#[derive(Default)]
struct RepeatStepper {
    dir: i32,
    countdown: f32,
}

impl RepeatStepper {
    fn step(&mut self, dir: i32, dt: f32) -> i32 {
        if dir != self.dir {
            self.dir = dir;
            self.countdown = MENU_REPEAT_DELAY;
            // The press edge steps immediately; a release returns 0.
            return dir;
        }
        if dir == 0 {
            return 0;
        }
        self.countdown -= dt;
        let mut steps = 0;
        // `dt` is clamped against stalls by the app loop, so this is bounded.
        while self.countdown <= 0.0 {
            self.countdown += MENU_REPEAT_INTERVAL;
            steps += 1;
        }
        steps * dir
    }
}

pub struct AppCore {
    pub user: UserData,
    pub presence: Option<DiscordPresence>,
    pub display_mode: DisplayMode,
    display_mode_override: Option<DisplayMode>,
    /// The kind of fullscreen the fullscreen key goes into: the one last
    /// left, borderless until there is one.
    fullscreen_kind: DisplayMode,
    /// The windowed size the window opens at (`--width`/`--height`).
    pub window_size: winit::dpi::LogicalSize<u32>,
    pub input: InputState,
    pub menu: MainMenu,
    pub tokio_rt: Arc<tokio::runtime::Runtime>,
    pub data_dirs: DataDirs,
    pub version: String,
    pub resource_packs: ResourcePackManager,
    pub(crate) block_definitions: Rc<BlockDefinitions>,
    pub pending_pack_download: Option<PendingPackDownload>,
    /// The server a `transfer` packet named, until the phase acts on it.
    pub pending_transfer: Option<(String, u16)>,
    /// Vanilla `seenInsecureChatWarning`: the unverified-chat toast has
    /// been shown since the menu was last left.
    pub seen_insecure_chat_warning: bool,
    /// A resource pack being downloaded or loaded, for the bar shown
    /// meanwhile; and the share of it the bar shows, eased toward it.
    pub pack_progress: Option<Arc<crate::resource_pack::PackProgress>>,
    pub pack_bar: f32,
    /// Server packs applied whose assets are still preparing off the render
    /// thread; each is acknowledged once they're swapped in.
    pending_pack_acks: Vec<uuid::Uuid>,
    /// The sound events a live pack reload reads alongside its assets.
    pending_sounds: Option<std::thread::JoinHandle<crate::audio::SoundsIndex>>,
    pub asset_index: Option<AssetIndex>,
    pub audio: crate::audio::AudioEngine,
    pub tick_accumulator: f32,
    pub time_tick_accumulator: f32,
    menu_dpad: RepeatStepper,
    /// When the window lost OS focus, for pause-on-lost-focus (vanilla
    /// `pauseIfInactive`); `None` while focused.
    pub unfocused_since: Option<Instant>,
    /// A quick play held back until the required sign-in is done.
    pub deferred_quick_play: Option<crate::app::QuickPlay>,
    /// Vanilla `options.pauseOnLostFocus`.
    pub pause_on_lost_focus: bool,
    /// Vanilla `MouseHandler.mouseGrabbed`.
    mouse_grabbed: bool,
    /// The OS grab may no longer match `mouse_grabbed` (the window manager
    /// can drop a lock on focus change), so the next apply re-issues it.
    os_grab_stale: bool,
    player_skin_tx: crossbeam_channel::Sender<PlayerSkinResult>,
    player_skin_rx: crossbeam_channel::Receiver<PlayerSkinResult>,
    requested_player_skins: HashMap<uuid::Uuid, PlayerSkinSource>,
    /// 8x8 RGBA faces of fetched player skins, for the spectator menu's
    /// face atlas (the GPU-side skins keep no CPU pixels).
    player_faces: HashMap<uuid::Uuid, Vec<u8>>,
    player_faces_dirty: bool,
    /// Inline object glyphs (sprites and heads) the text drew recently,
    /// keyed as the text renderer looks them up.
    inline_objects: HashMap<String, InlineObjectEntry>,
    /// Head skins by profile identity, shared by a profile's hat and base
    /// glyphs and kept apart from the entity skin cache.
    player_heads: HashMap<HeadProfile, HeadEntry>,
    head_tx: crossbeam_channel::Sender<HeadResult>,
    head_rx: crossbeam_channel::Receiver<HeadResult>,
    /// A packed glyph's pixels changed (a head arrived), so the atlas needs
    /// rebuilding even though its key set is the same.
    inline_atlas_dirty: bool,
    /// Whether player faces are in the atlas as packed.
    player_faces_packed: bool,
    /// The inline-object keys currently packed.
    game_dynamic_atlas_keys: HashSet<String>,
    /// The map packed into that atlas for a cartography table to show, by
    /// its id and its picture's revision.
    preview_map_packed: Option<(i32, u64)>,
}

/// Folds the credits roll's keys into a `CREDITS_KEY_*` mask. Both control
/// keys count separately in vanilla's `speedupModifiers`, so this can't go
/// through `InputState::ctrl_held`, which folds them into one winit modifier
/// bit.
fn credits_key_mask(mut is_set: impl FnMut(KeyCode) -> bool) -> u8 {
    [
        (KeyCode::ArrowUp, CREDITS_KEY_UP),
        (KeyCode::Space, CREDITS_KEY_SPACE),
        (KeyCode::ControlLeft, CREDITS_KEY_CTRL_L),
        (KeyCode::ControlRight, CREDITS_KEY_CTRL_R),
    ]
    .into_iter()
    .filter(|&(code, _)| is_set(code))
    .map(|(_, bit)| bit)
    .sum()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorOp {
    Keep,
    Grab,
    Release { center: bool },
}

/// One `MouseHandler.grabMouse`/`releaseMouse` call as a pure transition over
/// `(mouse_grabbed, os_grab_stale)`. Grabbing needs an active window and is a
/// no-op once grabbed; releasing is a no-op once released and only centers on
/// the grabbed -> released edge. A stale OS grab re-issues either call.
fn cursor_step(grabbed: bool, stale: bool, want: bool, focused: bool) -> (CursorOp, bool, bool) {
    if want {
        if !focused || (grabbed && !stale) {
            return (CursorOp::Keep, grabbed, stale);
        }
        (CursorOp::Grab, true, false)
    } else {
        if !grabbed && !stale {
            return (CursorOp::Keep, grabbed, stale);
        }
        (CursorOp::Release { center: grabbed }, false, false)
    }
}

/// The asset index, the menu's splash and the audio engine, which `AppCore`
/// builds from the version's assets.
fn load_assets(
    data_dirs: &DataDirs,
    version: &str,
    resource_packs: &ResourcePackManager,
    menu: &mut MainMenu,
) -> (Option<AssetIndex>, crate::audio::AudioEngine) {
    let asset_index = AssetIndex::load(&data_dirs.indexes_dir, &data_dirs.objects_dir, version);
    menu.load_splash(&data_dirs.jar_assets_dir, &asset_index);
    let audio = crate::audio::AudioEngine::new(
        &data_dirs.jar_assets_dir,
        asset_index.clone(),
        resource_packs,
        menu.category_volumes(),
    );
    (asset_index, audio)
}

/// `LocalPlayerResolver`: a `Dynamic` profile resolves through the tab list
/// first (by name, ignoring case), and only then through the Mojang API. A
/// `Static` profile resolves to itself.
fn resolve_head_profile(profile: HeadProfile, tab_list: &TabList) -> HeadProfile {
    if profile.is_static() {
        return profile;
    }
    let found = match (&profile.id, &profile.name) {
        (Some(id), _) => tab_list.players.get(id),
        (None, Some(name)) => tab_list
            .players
            .values()
            .find(|player| player.name.eq_ignore_ascii_case(name)),
        _ => None,
    };
    match found {
        Some(player) => HeadProfile {
            id: Some(player.uuid),
            name: Some(player.name.clone()),
            textures: player.textures.clone(),
        },
        None => profile,
    }
}

impl AppCore {
    pub fn new(
        version: String,
        data_dirs: DataDirs,
        tokio_rt: Arc<tokio::runtime::Runtime>,
        presence: Option<DiscordPresence>,
        user: UserData,
        window_size: winit::dpi::LogicalSize<u32>,
    ) -> Self {
        let resource_packs = ResourcePackManager::new(&data_dirs.game_dir);

        let mut input = InputState::new();
        let mut menu = MainMenu::new(
            &data_dirs.game_dir,
            data_dirs.bedrock_assets_dir.clone(),
            Arc::clone(&tokio_rt),
            user.username.clone(),
            version.clone(),
            user.access_token.clone(),
            &mut input.key_bindings,
        );

        let display_mode = menu.display_mode;

        let (asset_index, audio) = load_assets(&data_dirs, &version, &resource_packs, &mut menu);
        let (player_skin_tx, player_skin_rx) = crossbeam_channel::unbounded();
        let (head_tx, head_rx) = crossbeam_channel::unbounded();

        Self {
            user,
            presence,
            display_mode,
            display_mode_override: None,
            fullscreen_kind: match display_mode {
                DisplayMode::Windowed => DisplayMode::Borderless,
                fullscreen => fullscreen,
            },
            window_size,
            input,
            menu,
            tokio_rt,
            data_dirs,
            version,
            resource_packs,
            block_definitions: Rc::new(BlockDefinitions::new()),
            pending_pack_download: None,
            pack_progress: None,
            pack_bar: 0.0,
            pending_pack_acks: Vec::new(),
            pending_transfer: None,
            seen_insecure_chat_warning: false,
            pending_sounds: None,
            asset_index,
            audio,
            tick_accumulator: 0.0,
            time_tick_accumulator: 0.0,
            menu_dpad: RepeatStepper::default(),
            unfocused_since: None,
            deferred_quick_play: None,
            pause_on_lost_focus: true,
            mouse_grabbed: false,
            os_grab_stale: false,
            player_skin_tx,
            player_skin_rx,
            requested_player_skins: HashMap::new(),
            player_faces: HashMap::new(),
            player_faces_dirty: false,
            inline_objects: HashMap::new(),
            player_heads: HashMap::new(),
            head_tx,
            head_rx,
            inline_atlas_dirty: false,
            player_faces_packed: false,
            game_dynamic_atlas_keys: HashSet::new(),
            preview_map_packed: None,
        }
    }

    pub fn build_menu_input(&mut self, dt: f32) -> MenuInput {
        if self.input.binding_capture_active() {
            self.menu_dpad.step(0, dt);
            return MenuInput {
                cursor: self.input.cursor_pos(),
                scroll_delta: self.input.consume_menu_scroll(),
                dt,
                binding_capture: true,
                binding_presses: self.input.take_binding_presses(),
                ..MenuInput::default()
            };
        }
        let credits_keys_down = credits_key_mask(|code| self.input.key_pressed(code));
        let credits_keys_pressed = credits_key_mask(|code| self.input.key_just_pressed(code));
        let dpad_dir = i32::from(self.input.gamepad_button_down(gilrs::Button::DPadRight))
            - i32::from(self.input.gamepad_button_down(gilrs::Button::DPadLeft));
        let gamepad_steps = self.menu_dpad.step(dpad_dir, dt);
        MenuInput {
            cursor: self.input.cursor_pos(),
            clicked: self.input.left_just_pressed(),
            mouse_held: self.input.left_held(),
            events: self.input.drain_text_events(),
            shift: self.input.shift_held(),
            enter: self.input.enter_pressed(),
            escape: self.input.escape_pressed(),
            tab: self.input.tab_pressed(),
            f5: self.input.f5_pressed(),
            gamepad_steps,
            scroll_delta: self.input.consume_menu_scroll(),
            dt,
            credits_keys_down,
            credits_keys_pressed,
            ..MenuInput::default()
        }
    }

    /// Uses `mode` for the window and every sync after, whatever the saved
    /// one is.
    pub fn override_display_mode(&mut self, mode: DisplayMode) {
        self.display_mode = mode;
        self.display_mode_override = Some(mode);
    }

    /// The fullscreen key: in or out of fullscreen, saved, unless an
    /// override holds the display mode.
    pub fn toggle_display_mode(&mut self, window: &Window) {
        if self.display_mode_override.is_some() {
            return;
        }
        // The options screen may have picked a kind of fullscreen since.
        if self.menu.display_mode != DisplayMode::Windowed {
            self.fullscreen_kind = self.menu.display_mode;
        }
        let display_mode = self.menu.display_mode.toggled(self.fullscreen_kind);
        self.menu
            .set_display_mode(display_mode, &self.input.key_bindings);
        self.sync_display_mode(window);
    }

    pub fn sync_display_mode(&mut self, window: &Window) {
        let wanted = self.display_mode_override.unwrap_or(self.menu.display_mode);
        if wanted != self.display_mode {
            self.display_mode = wanted;
            self.apply_display_mode(window);
        }
    }

    pub fn apply_display_mode(&mut self, window: &Window) {
        window.set_fullscreen(self.display_mode.fullscreen_for(window.current_monitor()));
        if self.display_mode == DisplayMode::Windowed {
            window.set_decorations(true);
        }
    }

    pub fn invalidate_cursor_grab_state(&mut self) {
        self.os_grab_stale = true;
    }

    pub fn apply_cursor_grab(&mut self, window: &Window, game: Option<&mut GameState>) {
        let captured =
            game.is_some_and(|g| g.input_live() && !g.dead && self.input.is_cursor_captured());
        if captured {
            self.grab_cursor(window);
        } else {
            self.release_cursor(window);
        }
    }

    /// Vanilla `MouseHandler.grabMouse`.
    fn grab_cursor(&mut self, window: &Window) {
        let op = self.step_cursor(true);
        if op == CursorOp::Grab {
            // Vanilla centers on grab too; warp before locking, which
            // freezes the position on some platforms.
            self.center_cursor(window);
            // In background mode the pointer belongs to whoever is at the
            // desk: the game only believes it holds it.
            if crate::background::on() {
                return;
            }
            let _ = window
                .set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined));
            window.set_cursor_visible(false);
        }
    }

    fn step_cursor(&mut self, want: bool) -> CursorOp {
        let (op, grabbed, stale) = cursor_step(
            self.mouse_grabbed,
            self.os_grab_stale,
            want,
            self.unfocused_since.is_none(),
        );
        self.mouse_grabbed = grabbed;
        self.os_grab_stale = stale;
        op
    }

    /// The chunk radius to ask a server for, from the video settings.
    pub const fn view_distance(&self) -> u8 {
        self.menu.render_distance as u8
    }

    /// Every return from a world or server, before the title screen shows.
    /// Vanilla builds a fresh `TitleScreen` here, which rolls a new splash.
    pub fn return_to_menu(&mut self, gfx: &mut Gfx) {
        self.seen_insecure_chat_warning = false;
        self.menu.restore_bench_render_distance();
        self.clear_server_resource_packs(&mut gfx.renderer);
        gfx.renderer.clear_chunk_meshes();
        if let Some(p) = &mut self.presence {
            p.set_in_menu(&self.version);
        }
        self.apply_cursor_grab(&gfx.window, None);
        self.menu
            .load_splash(&self.data_dirs.jar_assets_dir, &self.asset_index);
    }

    /// Releases the cursor, warping it to the window center only when it was
    /// grabbed, like vanilla `MouseHandler.releaseMouse` (a screen opened from
    /// gameplay starts with a centered cursor).
    fn release_cursor(&mut self, window: &Window) {
        if let CursorOp::Release { center } = self.step_cursor(false) {
            if !crate::background::on() {
                let _ = window.set_cursor_grab(CursorGrabMode::None);
                window.set_cursor_visible(true);
            }
            if center {
                self.center_cursor(window);
            }
        }
    }

    /// Never warps the OS pointer while another window has focus.
    fn center_cursor(&mut self, window: &Window) {
        let size = window.inner_size();
        let (x, y) = (size.width as f32 / 2.0, size.height as f32 / 2.0);
        if self.unfocused_since.is_none() && !crate::background::on() {
            let _ = window.set_cursor_position(winit::dpi::PhysicalPosition::new(x, y));
        }
        self.input.on_cursor_moved(x, y);
    }

    pub(crate) fn open_death_screen(
        &mut self,
        connection: &ConnectionHandle,
        window: &Window,
        game: &mut GameState,
        message: Option<String>,
    ) {
        game.interaction
            .stop_destroying_for_screen(&connection.packet_tx);
        // Minecraft.setScreen(DeathScreen) replaces the current screen, which
        // runs the options screen's removed() and saves.
        game.paused = false;
        game.options_from_game = false;
        self.menu.flush_settings(&self.input.key_bindings);
        game.close_menu();
        game.close_creative_inventory();
        game.chat
            .close(crate::ui::chat::ChatExitReason::Interrupted);
        game.game_mode_switcher = None;
        game.sign_edit = None;
        game.book_view = None;
        game.book_edit = None;
        game.advancements_ui = None;
        game.stats_ui = None;
        game.game_master_ui = None;
        game.tracking_emitters.clear();
        game.item_activation = None;
        game.server_dialog = None;

        if let Some(message) = message {
            game.death_message = message;
        }
        game.reset_death_screen();
        game.death_screen_open = true;
        self.release_cursor(window);
    }

    pub fn send_respawn(&mut self, connection: &ConnectionHandle, game: &mut GameState) {
        if game.respawn_sent {
            return;
        }
        connection
            .packet_tx
            .send(ServerboundGamePacket::ClientCommand(
                ServerboundClientCommand {
                    action: s_client_command::Action::PerformRespawn,
                },
            ));

        game.death_confirm = false;
        game.respawn_sent = true;
    }

    pub fn send_chat_message(&self, connection: &ConnectionHandle, msg: String) {
        connection.packet_tx.send_chat(msg);
    }

    /// Queues a tab-list player's entity skin: the textures the server sent,
    /// else a session-server lookup by id.
    fn queue_player_skin(&mut self, uuid: uuid::Uuid, textures: Option<String>) {
        let source = textures
            .map(PlayerSkinSource::Textures)
            .unwrap_or(PlayerSkinSource::Uuid);
        self.queue_skin(uuid, source, uuid);
    }

    /// A `ResolvableProfile`'s skin (a mannequin's, a player head's): the
    /// profile's textures, else the skin of the player it names by id or by
    /// name; with neither, the default skin stays. Kept under `key`.
    pub(crate) fn queue_profile_skin(
        &mut self,
        key: uuid::Uuid,
        profile: &pomme_protocol::entity_data::Profile,
    ) {
        let id = profile.id.map(uuid::Uuid::from_u128);
        let source = match (&profile.textures, id, &profile.name) {
            (Some(textures), ..) => PlayerSkinSource::Textures(textures.clone()),
            (None, Some(id), _) if !id.is_nil() => PlayerSkinSource::Uuid,
            (None, _, Some(name)) if !name.is_empty() => PlayerSkinSource::Name(name.clone()),
            _ => return,
        };
        self.queue_skin(key, source, id.unwrap_or(key));
    }

    /// Fetches `source`'s skin (a `Uuid` source names `profile`) and keeps
    /// it under `uuid`.
    fn queue_skin(&mut self, uuid: uuid::Uuid, source: PlayerSkinSource, profile: uuid::Uuid) {
        if self.requested_player_skins.get(&uuid) == Some(&source) {
            return;
        }
        self.requested_player_skins.insert(uuid, source.clone());

        // Name-derived (v3) UUIDs from offline-mode servers have no Mojang
        // profile to fetch; keep the default skin.
        if matches!(source, PlayerSkinSource::Uuid) && uuid.get_version_num() == 3 {
            return;
        }

        tracing::debug!("Fetching the skin for {uuid} from {source:?}");
        let tx = self.player_skin_tx.clone();
        self.tokio_rt.spawn(async move {
            let result = fetch_skin(&source, Some(profile)).await;
            let _ = tx.send(PlayerSkinResult {
                uuid,
                source,
                result,
            });
        });
    }

    fn drain_player_skin_results(&mut self, renderer: &mut Renderer) {
        while let Ok(skin) = self.player_skin_rx.try_recv() {
            if self.requested_player_skins.get(&skin.uuid) != Some(&skin.source) {
                continue;
            }
            match skin.result {
                Ok(data) => {
                    if let Some(face) = crate::renderer::pipelines::menu_overlay::extract_face_8x8(
                        &data.pixels,
                        data.width,
                        data.height,
                    ) {
                        self.player_faces.insert(skin.uuid, face);
                        self.player_faces_dirty = true;
                    }
                    renderer.update_player_entity_skin(&skin.uuid, &data);
                }
                Err(e) => {
                    tracing::warn!("Failed to load entity player skin for {}: {e}", skin.uuid)
                }
            }
        }
    }

    /// Loads what the text drew last frame into the shared glyph atlas and
    /// rebuilds it when its contents change. Player faces (spectator menu,
    /// tab list) share the same texture binding, so they are packed alongside
    /// while `faces_shown`.
    /// The dynamic atlas holds map `id`'s picture for a cartography table
    /// (as it last was, while a newer one waits for the next sync).
    pub fn preview_map_is_packed(&self, id: i32) -> bool {
        self.preview_map_packed
            .is_some_and(|(packed, _)| packed == id)
    }

    pub fn sync_game_dynamic_atlas(
        &mut self,
        game: &GameState,
        renderer: &mut Renderer,
        faces_shown: bool,
    ) {
        let now = Instant::now();
        self.drain_head_results();

        // Objects drawn last frame are the ones to keep loaded.
        let drawn: Vec<(String, InlineObject)> = renderer.drain_drawn_inline_objects().collect();
        for (key, object) in drawn {
            match self.inline_objects.get_mut(&key) {
                Some(entry) => entry.last_used = now,
                None => {
                    let content = self.load_inline_object(&object, game);
                    self.inline_objects.insert(
                        key,
                        InlineObjectEntry {
                            content,
                            last_used: now,
                        },
                    );
                    self.inline_atlas_dirty = true;
                }
            }
        }

        // `expireAfterAccess`: a glyph the text stopped drawing is dropped
        // once it has been unused for the cache duration.
        self.inline_objects
            .retain(|_, entry| now.duration_since(entry.last_used) < GLYPH_CACHE_DURATION);
        self.player_heads
            .retain(|_, entry| now.duration_since(entry.last_used) < GLYPH_CACHE_DURATION);
        for entry in self.inline_objects.values() {
            if let InlineObjectContent::Head { profile, .. } = &entry.content
                && let Some(head) = self.player_heads.get_mut(profile)
            {
                head.last_used = now;
            }
        }

        // A head with no skin yet draws the default head sprite, so it packs
        // nothing; the arrival sets the dirty flag.
        let head_ready = |profile: &HeadProfile| {
            matches!(
                self.player_heads.get(profile),
                Some(HeadEntry {
                    state: HeadState::Ready { .. },
                    ..
                })
            )
        };
        let packable = |entry: &InlineObjectEntry| match &entry.content {
            InlineObjectContent::Sprite { .. } => true,
            InlineObjectContent::Head { profile, .. } => head_ready(profile),
        };
        // The packed set is compared by its keys, without rebuilding the
        // tiles every frame.
        let wanted = self.inline_objects.values().filter(|e| packable(e)).count();
        let rebuild = self.inline_atlas_dirty
            || wanted != self.game_dynamic_atlas_keys.len()
            || self
                .inline_objects
                .iter()
                .any(|(key, entry)| packable(entry) && !self.game_dynamic_atlas_keys.contains(key))
            || faces_shown != self.player_faces_packed
            || (faces_shown && self.player_faces_dirty)
            || game.previewed_map() != self.preview_map_packed;
        if rebuild {
            let mut entries = Vec::<(String, Vec<u8>, u32)>::new();
            // The map a cartography table works on, under its own key.
            self.preview_map_packed = game.previewed_map();
            if let Some(map) = self
                .preview_map_packed
                .and_then(|(id, _)| game.maps.get(id))
            {
                entries.push((
                    crate::ui::workstations::MAP_PREVIEW_KEY.to_owned(),
                    map.rgba(),
                    pomme_protocol::map::MAP_SIZE as u32,
                ));
            }
            if faces_shown {
                entries.extend(
                    self.player_faces
                        .iter()
                        .map(|(uuid, face)| (uuid.to_string(), face.clone(), 8)),
                );
            }
            self.game_dynamic_atlas_keys.clear();
            for (key, entry) in &self.inline_objects {
                match &entry.content {
                    InlineObjectContent::Sprite { frames, .. } => {
                        // Every frame of an animation is packed; the base key
                        // points at the one showing.
                        if frames.animated() {
                            for frame in 0..frames.frame_count() {
                                entries.push((
                                    format!("{key}#{frame}"),
                                    frames.pixels(frame).to_vec(),
                                    frames.size,
                                ));
                            }
                        }
                        entries.push((key.clone(), frames.pixels(0).to_vec(), frames.size));
                        self.game_dynamic_atlas_keys.insert(key.clone());
                    }
                    InlineObjectContent::Head { profile, hat } => {
                        if let Some(HeadEntry {
                            state:
                                HeadState::Ready {
                                    hat: hat_face,
                                    base,
                                },
                            ..
                        }) = self.player_heads.get(profile)
                        {
                            let face = if *hat { hat_face } else { base };
                            entries.push((key.clone(), face.clone(), 8));
                            self.game_dynamic_atlas_keys.insert(key.clone());
                        }
                    }
                }
            }
            if !entries.is_empty() {
                renderer.update_face_atlas(&entries);
            }
            self.inline_atlas_dirty = false;
            self.player_faces_packed = faces_shown;
            if faces_shown {
                self.player_faces_dirty = false;
            }
        }

        // An animated sprite keeps every frame in the atlas; the key the text
        // draws with points at the one showing this tick.
        for (key, entry) in &mut self.inline_objects {
            if let InlineObjectContent::Sprite { frames, frame } = &mut entry.content
                && frames.animated()
            {
                let next = frames.frame_at(game.tick_count);
                if *frame != next || rebuild {
                    *frame = next;
                    renderer.set_inline_object_frame(key, &format!("{key}#{next}"));
                }
            }
        }
    }

    /// The tile (or frames) behind one inline object, starting a head fetch
    /// where one is needed.
    fn load_inline_object(
        &mut self,
        object: &InlineObject,
        game: &GameState,
    ) -> InlineObjectContent {
        match object {
            InlineObject::AtlasSprite { atlas, sprite } => {
                let frames = crate::ui::object_glyph::load_atlas_sprite(
                    &self.data_dirs.jar_assets_dir,
                    &self.asset_index,
                    &self.resource_packs,
                    atlas,
                    sprite,
                )
                .unwrap_or_else(|| {
                    // A sprite the atlas doesn't have keeps the missing
                    // texture, as `TextureAtlas.missingSprite` does.
                    let (pixels, size) = crate::ui::object_glyph::missing_tile();
                    crate::ui::object_glyph::SpriteFrames::still(pixels, size)
                });
                InlineObjectContent::Sprite { frames, frame: 0 }
            }
            InlineObject::Player {
                uuid,
                name,
                textures,
                hat,
            } => {
                let profile = resolve_head_profile(
                    HeadProfile {
                        id: *uuid,
                        name: name.clone(),
                        textures: textures.clone(),
                    },
                    &game.tab_list,
                );
                self.request_head(profile.clone());
                InlineObjectContent::Head { profile, hat: *hat }
            }
        }
    }

    /// Starts the one fetch a profile needs; its hat and base glyphs share it.
    fn request_head(&mut self, profile: HeadProfile) {
        if self.player_heads.contains_key(&profile) {
            return;
        }
        // A `Static` profile with no textures has nothing to fetch: vanilla's
        // `SkinManager.get` reads the profile's own property and falls back to
        // the default skin without a lookup.
        let source = match (&profile.textures, &profile.id, &profile.name) {
            (Some(textures), _, _) => Some(PlayerSkinSource::Textures(textures.clone())),
            _ if profile.is_static() => None,
            (_, Some(_), _) => Some(PlayerSkinSource::Uuid),
            (_, None, Some(name)) if crate::player::valid_player_name(name) => {
                Some(PlayerSkinSource::Name(name.clone()))
            }
            _ => None,
        };
        let Some(source) = source else {
            self.player_heads.insert(
                profile,
                HeadEntry {
                    state: HeadState::Fallback,
                    last_used: Instant::now(),
                },
            );
            return;
        };
        self.player_heads.insert(
            profile.clone(),
            HeadEntry {
                state: HeadState::Pending,
                last_used: Instant::now(),
            },
        );
        let id = profile.id;
        let tx = self.head_tx.clone();
        self.tokio_rt.spawn(async move {
            let result = fetch_skin(&source, id).await;
            let _ = tx.send(HeadResult { profile, result });
        });
    }

    fn drain_head_results(&mut self) {
        while let Ok(head) = self.head_rx.try_recv() {
            let Some(entry) = self.player_heads.get_mut(&head.profile) else {
                continue;
            };
            entry.state = match head.result {
                Ok(data) => {
                    use crate::renderer::pipelines::menu_overlay::{
                        extract_face_8x8, extract_face_8x8_with_hat,
                    };
                    match (
                        extract_face_8x8(&data.pixels, data.width, data.height),
                        extract_face_8x8_with_hat(&data.pixels, data.width, data.height, false),
                    ) {
                        (Some(hat), Some(base)) => HeadState::Ready { hat, base },
                        _ => HeadState::Fallback,
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to load head skin for {:?}: {e}", head.profile);
                    HeadState::Fallback
                }
            };
            self.inline_atlas_dirty = true;
        }
    }

    fn remove_player_skin(&mut self, renderer: &mut Renderer, uuid: &uuid::Uuid) {
        self.requested_player_skins.remove(uuid);
        let removed = self.player_faces.remove(uuid).is_some();
        if removed {
            self.player_faces_dirty = true;
        }
        renderer.remove_player_entity_skin(uuid);
    }

    /// The resource pack loading bar: what it says and how full it is, 0
    /// to 1, while a pack is being downloaded or loaded. The fill eases
    /// toward where the loading has got over the `dt` seconds since the
    /// last frame, so it never jumps, at any frame rate.
    pub fn pack_loading_bar(
        &mut self,
        renderer: &Renderer,
        dt: f32,
    ) -> Option<(&'static str, f32)> {
        use crate::resource_pack::PackStage;
        let progress = self.pack_progress.as_ref()?;
        let target = progress.fraction(renderer.asset_reload_fraction());
        self.pack_bar += (target - self.pack_bar).max(0.0) * pack_bar_ease(dt);
        let label = match progress.stage() {
            PackStage::Downloading | PackStage::Unpacking => {
                crate::lang::translate("resourcepack.downloading")
                    .unwrap_or("Downloading Resource Pack")
            }
            PackStage::Loading => "Loading Resource Pack",
        };
        Some((label, self.pack_bar))
    }

    /// A server's resource pack, to fetch on a worker thread: what a pack
    /// push starts, in configuration or in play.
    pub fn push_resource_pack(
        &mut self,
        id: uuid::Uuid,
        url: String,
        hash: String,
        required: bool,
    ) {
        tracing::info!("Resource pack push: {id} url={url} required={required}");
        let cache_dir = self.resource_packs.server_cache_dir().to_path_buf();
        let progress = Arc::new(crate::resource_pack::PackProgress::default());
        self.pack_progress = Some(Arc::clone(&progress));
        self.pack_bar = 0.0;
        self.pending_pack_download = Some(PendingPackDownload {
            id,
            required,
            hash: hash.clone(),
            handle: std::thread::spawn(move || {
                ResourcePackManager::download_server_pack(&cache_dir, &url, &hash, &progress)
            }),
        });
    }

    /// Starts rebuilding the assets for a pack change in game: they prepare
    /// on a worker thread while the world keeps running on the old ones, and
    /// [`Self::finish_live_resource_reload`] swaps them in.
    fn begin_live_resource_reload(&mut self, renderer: &mut Renderer) {
        // The bar goes on (or starts, for a pack already on disk) with the
        // assets' rebuilding.
        match &self.pack_progress {
            Some(progress) => progress.set_stage(crate::resource_pack::PackStage::Loading),
            None => {
                self.pack_progress = Some(Arc::new(crate::resource_pack::PackProgress::loading()));
                self.pack_bar = 0.0;
            }
        }
        self.menu.active_packs = self.resource_packs.active_pack_info();
        renderer.begin_asset_reload(&self.resource_packs, self.menu.font_options());
        self.pending_sounds = self.audio.begin_sounds_reload(&self.resource_packs);
        self.menu.reload_assets = false;
        self.menu.reload_fonts = false;
    }

    /// Once the renderer has swapped in a live reload's assets: the rest of
    /// [`Self::reload_pack_assets`], then the world rebuilt on them.
    fn finish_live_resource_reload(&mut self, game: &mut GameState, renderer: &mut Renderer) {
        let start = std::time::Instant::now();
        // The shared atlas bakes UVs into chunk vertices, so the old chunk
        // geometry goes with the old atlas. Recreate the CPU mesher and
        // particle atlas state from the new registry and UV map, then let the
        // normal rescan rebuild loaded columns.
        renderer.clear_chunk_meshes();
        match self
            .pending_sounds
            .take()
            .map(std::thread::JoinHandle::join)
        {
            Some(Ok(sounds)) => self.audio.apply_sounds(sounds),
            // Started without its sounds, or their reading panicked.
            _ => self.audio.reload_assets(&self.resource_packs),
        }
        let audio = start.elapsed();
        // A pack swap changes what the sprites look like, so they reload.
        self.inline_objects
            .retain(|_, entry| matches!(entry.content, InlineObjectContent::Head { .. }));
        self.game_dynamic_atlas_keys.clear();
        game.block_registry
            .replace_render_assets(renderer.shared_render_assets());

        let mesh_dispatcher = renderer.create_mesh_dispatcher(
            game.block_registry.owned_render_view(),
            Arc::clone(&game.biome_climate),
            Some(&self.resource_packs),
            game.cardinal_light,
        );
        let (grass, foliage, dry_foliage) = mesh_dispatcher.colormaps();
        game.mesh_dispatcher = mesh_dispatcher;
        game.particle_store = crate::particle::ParticleStore::new(
            renderer.uv_map().clone(),
            grass,
            foliage,
            dry_foliage,
        );

        game.rebuild_meshes();
        tracing::info!(
            "Pack swap finished in {:.0?} (audio {audio:.0?})",
            start.elapsed()
        );
    }

    fn clear_server_ui(&mut self, game: &mut GameState, renderer: &mut Renderer) {
        game.tab_list.clear();
        game.scoreboard.clear();
        game.player.effects.clear();
        game.boss_bars.clear();
        game.toasts.clear();
        // Vanilla Hud.onDisconnected: clearTitles + resetTitleTimes.
        game.title.clear(true);
        game.subtitles.clear();
        self.requested_player_skins.clear();
        self.player_faces.clear();
        self.player_faces_dirty = false;
        self.inline_objects.clear();
        self.player_heads.clear();
        self.game_dynamic_atlas_keys.clear();
        renderer.clear_player_entity_skins();
    }

    pub fn clear_server_resource_packs(&mut self, renderer: &mut Renderer) {
        if self.resource_packs.clear_server_packs() {
            self.reload_pack_assets(renderer);
        }
    }

    /// Rebuilds every asset a pack can override. Only call this once the
    /// active stack has really changed: it waits for device idle and rebuilds
    /// the texture atlas. Clearing
    /// `menu.reload_assets` and `menu.reload_fonts` is safe here because the
    /// pending toggles they stand for are covered by the reload we just did.
    pub(crate) fn font_sources(&self) -> crate::ui::font::FontSources<'_> {
        crate::ui::font::FontSources {
            jar_assets_dir: &self.data_dirs.jar_assets_dir,
            asset_index: &self.asset_index,
            packs: &self.resource_packs,
            options: self.menu.font_options(),
        }
    }

    /// Rebuilds what was built from assets that didn't exist yet, once the
    /// first-run download has put them on disk.
    pub(crate) fn reload_installed_assets(&mut self) {
        let (asset_index, audio) = load_assets(
            &self.data_dirs,
            &self.version,
            &self.resource_packs,
            &mut self.menu,
        );
        self.asset_index = asset_index;
        self.audio = audio;
    }

    pub(crate) fn reload_pack_assets(&mut self, renderer: &mut Renderer) {
        self.menu.active_packs = self.resource_packs.active_pack_info();
        renderer.reload_assets(&self.resource_packs, self.menu.font_options());
        self.audio.reload_assets(&self.resource_packs);
        // A pack swap changes what the sprites look like, so they reload.
        self.inline_objects
            .retain(|_, entry| matches!(entry.content, InlineObjectContent::Head { .. }));
        self.game_dynamic_atlas_keys.clear();
        self.menu.reload_assets = false;
        self.menu.reload_fonts = false;
    }

    /// Vanilla `Options.updateFontOptions`: a Font Settings toggle applies at
    /// once, from the title screen or in game.
    pub fn apply_font_options(&mut self, renderer: &mut Renderer) {
        if std::mem::take(&mut self.menu.reload_fonts) {
            renderer.reload_fonts(&self.resource_packs, self.menu.font_options());
        }
    }

    pub fn drain_network_events(
        &mut self,
        connection: &ConnectionHandle,
        mut connect_phase: Option<&mut ConnectionPhase>,
        renderer: &mut Renderer,
        window: &Window,
        game: &mut GameState,
    ) -> Option<String> {
        let rx = &connection.event_rx;

        // Phase timers for the chunk-load benchmark's worst-frame breakdown.
        let t_net = std::time::Instant::now();

        // Block edits go on the priority lane so they apply instantly even while
        // chunks stream in, instead of starving behind the load backlog.
        let mut priority_remesh: Vec<(crate::position::ChunkPos, i32)> = Vec::new();
        let mut disconnect_reason: Option<String> = None;
        self.drain_player_skin_results(renderer);

        // Vanilla `PacketProcessor.processQueuedPackets`: drain everything
        // before this frame's ticks.
        while let Ok(event) = rx.try_recv() {
            match event {
                NetworkEvent::ProtocolSelected { protocol } => game.select_protocol(protocol),
                NetworkEvent::Connected => {
                    if let Some(state) = connect_phase.as_deref_mut() {
                        tracing::info!("Connected to server");
                        // Ticks restart from zero with the play state.
                        self.tick_accumulator = 0.0;
                        *state = ConnectionPhase::Loading;
                    } else {
                        tracing::warn!("Unexpected NetworkEvent::Connected, skipping");
                    }
                }
                NetworkEvent::LevelChunksLoadStart => {
                    if let Some(tracker) = &mut game.level_load {
                        tracker.loading_packets_received();
                    }
                }
                NetworkEvent::BiomeColors { colors } => game.set_biome_climate(colors),
                NetworkEvent::DimensionInfo(dim) => {
                    game.enter_dimension(dim, self.menu.render_distance, &*renderer);
                    renderer.clear_chunk_meshes();
                }
                NetworkEvent::DimensionName { name } => {
                    game.dimension = name;
                }
                NetworkEvent::ChunkLoaded {
                    pos,
                    data,
                    heightmaps,
                    light,
                } => game.load_chunk(pos, &data, &heightmaps, &light),
                NetworkEvent::LightUpdate { pos, light } => game.apply_light_update(pos, &light),
                NetworkEvent::ChunkBiomes { chunks } => game.replace_biomes(&chunks),
                NetworkEvent::ChunkUnloaded { pos } => {
                    game.unload_chunk(pos);
                    renderer.remove_chunk_mesh(&pos);
                }
                NetworkEvent::ChunkCacheCenter { x, z } => {
                    tracing::debug!("Chunk cache center: [{x}, {z}]");
                    game.chunk_store
                        .set_center(crate::position::ChunkPos::new(x, z));
                }
                NetworkEvent::Ping { id } => AppCore::send_pong(connection, id),
                NetworkEvent::PlayerPosition {
                    id,
                    change,
                    relative,
                } => {
                    if game.apply_player_position(connection, id, &change, &relative) {
                        // The camera is the eye, as `sync_camera_pos` keeps it
                        // every frame; the feet would seed it a block and a half
                        // low until the first in-game frame.
                        renderer.reset_camera(game.player.eye_pos(), game.player.look_dir);
                    }
                }
                NetworkEvent::WorldBorder(update) => game.world_border.apply(update),
                NetworkEvent::TickingState { tick_rate, frozen } => {
                    game.tick_rate.set_state(tick_rate, frozen);
                }
                NetworkEvent::TickingStep { steps } => game.tick_rate.set_steps(steps),
                NetworkEvent::BlockDestruction { id, pos, progress } => {
                    game.block_breaking.set(id, pos, progress, game.tick_count);
                }
                NetworkEvent::PlayerLookAt {
                    from_eyes,
                    target,
                    entity,
                } => {
                    // `ClientboundPlayerLookAtPacket.getPosition`: the entity's
                    // feet or eyes where it is known here, else the point.
                    let target = entity
                        .and_then(|(id, eyes)| {
                            let entity = game.entity_store.living.get(&id)?;
                            let feet: glam::DVec3 = entity.position.into();
                            let rise = if eyes {
                                crate::entity::dimensions::EntityDimensions::eye_height(
                                    entity.entity_type,
                                )
                            } else {
                                0.0
                            };
                            Some(feet + glam::dvec3(0.0, f64::from(rise), 0.0))
                        })
                        .unwrap_or(target);
                    let from: glam::DVec3 = if from_eyes {
                        game.player.eye_pos().into()
                    } else {
                        game.player.position.into()
                    };
                    // `Entity.lookAt`: the look set, and the old look with it.
                    let look = crate::entity::components::LookDirection::toward(from, target);
                    game.player.look_dir = look;
                    game.player.prev_look_dir = look;
                    renderer.reset_camera(game.player.eye_pos(), look);
                }
                NetworkEvent::PlayerRotation {
                    y_rot,
                    relative_y,
                    x_rot,
                    relative_x,
                } => {
                    let new_look_dir = resolve_corrected_look(
                        game.player.look_dir,
                        y_rot,
                        x_rot,
                        relative_y,
                        relative_x,
                    );

                    // Vanilla `handleRotatePlayer`: set, `setOldRot`, then ack.
                    game.player.look_dir = new_look_dir;
                    game.player.prev_look_dir = new_look_dir;
                    renderer.reset_camera(game.player.eye_pos(), new_look_dir);
                    connection.packet_tx.send(ServerboundGamePacket::MovePlayerRot(
                        azalea_protocol::packets::game::s_move_player_rot::ServerboundMovePlayerRot {
                            look_direction: new_look_dir.into(),
                            flags: Default::default(),
                        },
                    ));
                }
                NetworkEvent::ItemCooldown { group, duration } => {
                    game.player.cooldowns.set(group, duration);
                }
                NetworkEvent::PlayerHealth {
                    health,
                    food,
                    saturation,
                } => {
                    game.player.apply_server_health(health);
                    game.player.food = food;
                    game.player.saturation = saturation;
                    if health > 0.0 && game.dead {
                        game.dead = false;
                        game.player.reset_death_time();
                        game.reset_death_screen();
                        self.apply_cursor_grab(window, Some(game));
                    } else if health <= 0.0 && !game.dead {
                        game.dead = true;
                        game.player.reset_death_time();
                        if !game.death_screen_open {
                            game.death_message.clear();
                        }
                        // Vanilla container screens close themselves as soon as
                        // the local player is no longer alive.
                        game.close_menu();
                        game.close_creative_inventory();
                    }
                }
                NetworkEvent::SetPassengers {
                    vehicle,
                    passengers,
                } => {
                    game.display_store.set_passengers(vehicle, &passengers);
                    game.entity_store.set_passengers(vehicle, &passengers);
                    game.interactions.set_passengers(vehicle, &passengers);
                    game.model_entities.set_passengers(vehicle, &passengers);
                    let me = game.player.entity_id;
                    if passengers.contains(&me) {
                        game.riding_vehicle_id = Some(vehicle);
                        // Only the first passenger controls (vanilla
                        // getControlledVehicle -> null otherwise).
                        game.controlled_vehicle_id =
                            (passengers.first() == Some(&me)).then_some(vehicle);
                    } else {
                        // Removed from this vehicle.
                        if game.riding_vehicle_id == Some(vehicle) {
                            game.riding_vehicle_id = None;
                        }
                        if game.controlled_vehicle_id == Some(vehicle) {
                            game.controlled_vehicle_id = None;
                        }
                    }
                }
                NetworkEvent::EntityEquipment { entity_id, slots } => {
                    if let Some(e) = game.entity_store.living.get_mut(&entity_id) {
                        for (slot, item) in slots {
                            if slot == azalea_inventory::components::EquipmentSlot::Head
                                && let Some(profile) = crate::entity::heads::item_profile(&item)
                            {
                                let key = crate::entity::heads::skin_key(&profile);
                                self.queue_profile_skin(key, &profile);
                            }
                            e.set_equipment(slot, item);
                        }
                    }
                }
                NetworkEvent::PlayerExperience { progress, level } => {
                    if progress != game.player.experience_progress {
                        // Vanilla LocalPlayer.setExperienceValues: the first
                        // change after (re)spawn only arms the sentinel and
                        // doesn't yet prioritize the XP bar.
                        game.xp_display_start_tick = if game.xp_display_start_tick == i64::MIN {
                            i64::MIN + 1
                        } else {
                            game.tick_count as i64
                        };
                    }
                    game.player.experience_progress = progress;
                    game.player.experience_level = level;
                }
                NetworkEvent::Waypoint {
                    operation,
                    waypoint,
                } => {
                    game.waypoints.apply(operation, waypoint);
                }
                NetworkEvent::EntityAttributesUpdate {
                    entity_id,
                    snapshots,
                } => {
                    let dirty = |kind| snapshots.iter().any(|snapshot| snapshot.attribute == kind);
                    if entity_id == game.player.entity_id {
                        for snapshot in &snapshots {
                            if !game.player.attributes.apply_snapshot(snapshot) {
                                tracing::warn!(
                                    "Server tried to update unsupported player attribute {:?}",
                                    snapshot.attribute
                                );
                            }
                        }
                        sync_player_attribute_mirrors(&mut game.player, dirty);
                    }

                    if let Some(entity) = game.entity_store.living.get_mut(&entity_id) {
                        for snapshot in &snapshots {
                            entity.attributes.apply_snapshot_or_insert(snapshot);
                        }
                        sync_living_attribute_mirrors(
                            &entity.attributes,
                            &mut entity.health,
                            &mut entity.max_health,
                            &mut entity.camera_distance,
                            dirty,
                        );
                    }
                }
                NetworkEvent::UpdateMobEffect { entity_id, effect } => {
                    if entity_id == game.player.entity_id {
                        game.player.effects.update(effect);
                    }
                }
                NetworkEvent::RemoveMobEffect {
                    entity_id,
                    effect_id,
                } => {
                    if entity_id == game.player.entity_id {
                        game.player.effects.remove(effect_id);
                    }
                }
                NetworkEvent::ClearMobEffects => {
                    game.player.effects.clear();
                }
                NetworkEvent::ContainerContent {
                    container_id,
                    items,
                    carried,
                    state_id,
                } => {
                    game.drop_bundle_selection(container_id, None);
                    // State ids are per-menu (vanilla scopes them to the menu
                    // the packet addresses); the rendered carried stack is the
                    // open menu's, so an inventory sync must not clobber it.
                    if container_id == 0 {
                        game.player.inventory.set_contents(items);
                        game.sync_container_from_inventory();
                        game.inventory_state_id = state_id;
                        if game.open_container.is_none() {
                            game.cursor_item = carried;
                        }
                    } else if game.open_menu_id() == Some(container_id) {
                        for (i, item) in items.into_iter().enumerate() {
                            game.set_menu_slot(i, item);
                        }
                        game.cursor_item = carried;
                        game.set_container_state_id(state_id);
                    }
                }
                NetworkEvent::CursorItem { item } => {
                    game.cursor_item = item;
                }
                NetworkEvent::Registries(registries) => {
                    crate::trim::set_from_registries(&registries);
                    crate::banner::set_from_registries(&registries);
                    crate::ui::chat::set_enchantment_names(
                        registries
                            .enchantment
                            .map
                            .keys()
                            .map(ToString::to_string)
                            .collect(),
                    );
                    game.registries = registries;
                }
                NetworkEvent::DialogRegistry(registry) => {
                    game.dialog_registry = registry;
                }
                NetworkEvent::ItemTags(tags) => {
                    game.recipe_book.set_item_tags(tags);
                }
                NetworkEvent::BlockTags { tags } => {
                    game.block_registry.replace_tags(tags);
                }
                NetworkEvent::ContainerSlot {
                    container_id,
                    index,
                    item,
                    state_id,
                } => {
                    game.drop_bundle_selection(container_id, Some(index));
                    if container_id == 0 {
                        game.player.inventory.set_slot(index as usize, item);
                        game.sync_container_from_inventory();
                        game.inventory_state_id = state_id;
                    } else if game.open_menu_id() == Some(container_id) {
                        game.set_menu_slot(index as usize, item);
                        game.set_container_state_id(state_id);
                    }
                }
                NetworkEvent::PlayerInventorySlot { index, item } => {
                    if let Some(slot) =
                        crate::player::inventory::menu_slot_for_inventory_index(index)
                    {
                        game.drop_bundle_selection(0, Some(slot as u16));
                        game.player.inventory.set_slot(slot, item);
                        game.sync_container_from_inventory();
                    }
                }
                NetworkEvent::HeldSlot { slot } => {
                    self.input.set_selected_slot(slot);
                }
                NetworkEvent::MountScreenOpen {
                    container_id,
                    columns,
                    entity,
                } => {
                    use crate::app::phases::in_game::ContainerScreen;
                    // `handleMountScreenOpen`: for a mount known here.
                    let mount = game
                        .entity_store
                        .living
                        .get(&entity)
                        .map(|mount| (mount.entity_type, mount.custom_name.clone()));
                    if let Some((kind, name)) = mount
                        && let Some((saddle, armor, nautilus)) = crate::ui::mount::slots_of(kind)
                    {
                        let screen = ContainerScreen::Mount {
                            columns: columns.min(5) as u8,
                            saddle,
                            armor,
                            nautilus,
                        };
                        game.paused = false;
                        if game.inventory_open || game.recipe_book_ui.captures_typing() {
                            game.recipe_book_ui
                                .reset_for_closed_screen(&mut game.recipe_book);
                        }
                        game.inventory_open = false;
                        game.close_creative_inventory();
                        game.inv_drag = None;
                        game.inv_last_click = None;
                        game.open_container = Some(crate::app::phases::in_game::OpenContainer {
                            id: container_id,
                            title: name.unwrap_or_else(|| crate::ui::mount::kind_name(kind)),
                            // No menu type of the registry's is a mount's.
                            #[cfg(feature = "test-harness")]
                            menu: azalea_registry::builtin::MenuKind::Generic9x1,
                            screen,
                            slots: vec![
                                azalea_inventory::ItemStack::Empty;
                                screen.click_kind().slot_count()
                            ],
                            data: [0; 10],
                            anvil: None,
                            enchant: None,
                            state_id: 0,
                        });
                        game.sync_container_from_inventory();
                        game.container_was_open = Some(container_id);
                        self.apply_cursor_grab(window, Some(game));
                    }
                }
                NetworkEvent::OpenBook { off_hand } => {
                    // `handleOpenBook`: the book in that hand, where it is
                    // one.
                    let stack = if off_hand {
                        game.player.inventory.offhand().as_present()
                    } else {
                        game.player.inventory.held_stack(self.input.selected_slot())
                    };
                    if let Some(pages) = stack.and_then(crate::ui::book::pages_of) {
                        game.paused = false;
                        game.close_menu();
                        game.close_creative_inventory();
                        game.book_view = Some(crate::ui::book::BookViewState { pages, page: 0 });
                        self.apply_cursor_grab(window, Some(game));
                    }
                }
                NetworkEvent::OpenSignEditor { pos, front } => {
                    // `handleOpenSignEditor`: only where a sign stands.
                    let state = game.chunk_store.get_block_state(pos.x, pos.y, pos.z);
                    let block = game.block_registry.block_state_data(state).id();
                    if let Some(edit) = crate::ui::sign_edit::SignEditState::open(
                        pos,
                        front,
                        block,
                        game.chunk_store.block_entities.get(&pos),
                    ) {
                        game.paused = false;
                        game.close_menu();
                        game.close_creative_inventory();
                        game.sign_edit = Some(edit);
                        self.apply_cursor_grab(window, Some(game));
                    }
                }
                NetworkEvent::MerchantOffers {
                    container_id,
                    offers,
                    level,
                    xp,
                    show_progress,
                    can_restock,
                } => {
                    // `handleMerchantOffers`: only for the menu that is open.
                    if game
                        .open_container
                        .as_ref()
                        .is_some_and(|open| open.id == container_id)
                    {
                        game.merchant
                            .set_offers(offers, level, xp, show_progress, can_restock);
                    }
                }
                NetworkEvent::ContainerData {
                    container_id,
                    id,
                    value,
                } => {
                    if let Some(c) = &mut game.open_container
                        && c.id == container_id
                        && let Some(d) = c.data.get_mut(id as usize)
                    {
                        *d = value as i16;
                    }
                }
                NetworkEvent::OpenScreen {
                    container_id,
                    menu_type,
                    title,
                } => {
                    use azalea_inventory::ItemStack;
                    use azalea_registry::builtin::MenuKind;

                    use crate::app::phases::in_game::ContainerScreen;
                    use crate::ui::furnace::FurnaceVariant;
                    let screen = match menu_type {
                        MenuKind::Crafting => Some(ContainerScreen::CraftingTable),
                        MenuKind::Furnace => {
                            Some(ContainerScreen::Furnace(FurnaceVariant::Furnace))
                        }
                        MenuKind::BlastFurnace => {
                            Some(ContainerScreen::Furnace(FurnaceVariant::BlastFurnace))
                        }
                        MenuKind::Smoker => Some(ContainerScreen::Furnace(FurnaceVariant::Smoker)),
                        MenuKind::Generic9x1 => Some(ContainerScreen::Chest { rows: 1 }),
                        MenuKind::Generic9x2 => Some(ContainerScreen::Chest { rows: 2 }),
                        MenuKind::Generic9x3 => Some(ContainerScreen::Chest { rows: 3 }),
                        MenuKind::Generic9x4 => Some(ContainerScreen::Chest { rows: 4 }),
                        MenuKind::Generic9x5 => Some(ContainerScreen::Chest { rows: 5 }),
                        MenuKind::Generic9x6 => Some(ContainerScreen::Chest { rows: 6 }),
                        MenuKind::ShulkerBox => Some(ContainerScreen::ShulkerBox),
                        MenuKind::Hopper => Some(ContainerScreen::Hopper),
                        MenuKind::Generic3x3 => Some(ContainerScreen::Dispenser),
                        MenuKind::Grindstone => Some(ContainerScreen::Grindstone),
                        MenuKind::CartographyTable => Some(ContainerScreen::Cartography),
                        MenuKind::Smithing => Some(ContainerScreen::Smithing),
                        MenuKind::BrewingStand => Some(ContainerScreen::BrewingStand),
                        MenuKind::Stonecutter => Some(ContainerScreen::Stonecutter),
                        MenuKind::Merchant => Some(ContainerScreen::Merchant),
                        MenuKind::Crafter3x3 => Some(ContainerScreen::Crafter),
                        MenuKind::Lectern => Some(ContainerScreen::Lectern),
                        MenuKind::Beacon => Some(ContainerScreen::Beacon),
                        MenuKind::Loom => Some(ContainerScreen::Loom),
                        MenuKind::Anvil => Some(ContainerScreen::Anvil),
                        MenuKind::Enchantment => Some(ContainerScreen::Enchantment),
                        // Every menu this version has is drawn; one a later
                        // version adds would fall through to here.
                        #[allow(unreachable_patterns)]
                        _ => None,
                    };
                    if let Some(screen) = screen {
                        // Vanilla setScreen replaces whatever screen is up,
                        // including the pause menu.
                        game.paused = false;
                        if game.inventory_open || game.recipe_book_ui.captures_typing() {
                            game.recipe_book_ui
                                .reset_for_closed_screen(&mut game.recipe_book);
                        }
                        game.inventory_open = false;
                        game.close_creative_inventory();
                        game.inv_drag = None;
                        game.menu_list = Default::default();
                        game.merchant = Default::default();
                        game.beacon = Default::default();
                        game.loom = Default::default();
                        game.inv_last_click = None;
                        game.open_container = Some(crate::app::phases::in_game::OpenContainer {
                            id: container_id,
                            title,
                            #[cfg(feature = "test-harness")]
                            menu: menu_type,
                            screen,
                            slots: vec![ItemStack::Empty; screen.click_kind().slot_count()],
                            data: [0; 10],
                            anvil: (screen == ContainerScreen::Anvil)
                                .then(crate::ui::anvil::AnvilState::new),
                            enchant: (screen == ContainerScreen::Enchantment)
                                .then(crate::ui::enchantment::EnchantState::new),
                            state_id: 0,
                        });
                        game.sync_container_from_inventory();
                        // The new menu replaces any previous one server-side;
                        // don't send a close for the replaced menu (the server
                        // would apply it to this one).
                        game.container_was_open = Some(container_id);
                        self.apply_cursor_grab(window, Some(game));
                    } else {
                        // A menu with no screen here: tell the server it
                        // was closed, so its container state stays
                        // consistent.
                        use azalea_protocol::packets::game::s_container_close::ServerboundContainerClose;
                        connection
                            .packet_tx
                            .send(ServerboundGamePacket::ContainerClose(
                                ServerboundContainerClose { container_id },
                            ));
                    }
                }
                NetworkEvent::ContainerClosed => {
                    // Vanilla closes whatever menu is open regardless of the
                    // packet's container id.
                    game.close_menu();
                    // The server initiated the close; don't echo one back.
                    game.container_was_open = None;
                    self.apply_cursor_grab(window, Some(game));
                }
                NetworkEvent::ChatMessage {
                    component,
                    secure_component,
                    missing_profile_component,
                    signature,
                    sender_uuid,
                    signed_body,
                    source,
                    tag,
                } => {
                    use crate::ui::text::format_component_spans_with_bindings;

                    let only_secure = game.chat.only_secure();
                    let component = if only_secure {
                        secure_component.unwrap_or(component)
                    } else {
                        component
                    };
                    let spans = format_component_spans_with_bindings(
                        &component,
                        [1.0; 4],
                        &self.input.key_bindings,
                    );
                    tracing::info!("Chat: {}", crate::ui::text::span_text(&spans));
                    if let (Some(sender_uuid), Some(body)) = (sender_uuid, signed_body) {
                        let now_ms = crate::net::chat_security::now_ms();
                        let validation =
                            game.tab_list.players.get_mut(&sender_uuid).map(|player| {
                                player.validate_chat_message(
                                    &body,
                                    signature.as_ref(),
                                    game.server_enforces_secure_chat,
                                    now_ms,
                                )
                            });
                        match validation {
                            None | Some(PlayerChatValidation::Invalid) => {
                                if let Some(component) = missing_profile_component {
                                    let spans = format_component_spans_with_bindings(
                                        &component,
                                        [1.0; 4],
                                        &self.input.key_bindings,
                                    );
                                    game.chat.push_validation_error(spans, signature);
                                }
                            }
                            Some(validation) => {
                                // An accepted unsigned message loses its signature
                                // (vanilla `removeSignature`).
                                let signed = validation == PlayerChatValidation::Signed;
                                let signature = signature.filter(|_| signed);
                                if body.fully_filtered {
                                    game.chat.push_fully_filtered(signature);
                                } else {
                                    let tag = accepted_player_chat_tag(
                                        game.singleplayer && sender_uuid == self.user.uuid,
                                        signed,
                                        &body,
                                        now_ms,
                                        only_secure,
                                        &self.input.key_bindings,
                                    );
                                    game.chat
                                        .push_message_with_source(spans, signature, source, tag);
                                }
                            }
                        }
                    } else {
                        game.chat
                            .push_message_with_source(spans, signature, source, tag);
                    }
                }
                NetworkEvent::DeleteChatMessage { signature } => {
                    game.chat.ignore_pending(signature);
                    game.chat.delete_message(signature);
                }
                NetworkEvent::ActionBar { component } => {
                    let spans = crate::ui::text::format_component_spans_with_bindings(
                        &component,
                        [1.0; 4],
                        &self.input.key_bindings,
                    );
                    game.action_bar = Some((spans, game.tick_count));
                }
                NetworkEvent::ServerLinks { links } => {
                    game.server_links = links;
                }
                NetworkEvent::ShowDialog { dialog } => {
                    // The screen under the dialog is vanilla's `previousScreen`:
                    // it keeps its state (an open container included) and comes
                    // back when the dialog closes.
                    if game.open_server_dialog(dialog) {
                        self.apply_cursor_grab(window, Some(game));
                    }
                }
                // `clearDialog` closes a dialog screen only; a
                // `WaitingForResponseScreen` stays up for its own Back button.
                NetworkEvent::ClearDialog => {
                    if game
                        .server_dialog
                        .as_ref()
                        .is_some_and(|dialog| dialog.is_dialog())
                    {
                        game.server_dialog = None;
                        self.apply_cursor_grab(window, Some(game));
                    }
                }
                NetworkEvent::BossBarUpdate { id, op } => {
                    game.boss_bars.apply(id, op);
                }
                NetworkEvent::AdvancementsUpdate(update) => {
                    game.toasts.apply_advancements(*update);
                }
                NetworkEvent::SelectAdvancementsTab(tab) => {
                    game.toasts.select_advancements_tab(tab);
                }
                NetworkEvent::AwardStats(stats) => {
                    game.stats.apply(stats);
                    if let Some(ui) = &mut game.stats_ui {
                        ui.loaded(&game.stats);
                    }
                }
                NetworkEvent::RecipeBookAdd { entries, replace } => {
                    game.recipe_book.apply_add(entries, replace);
                }
                NetworkEvent::RecipeBookRemove { ids } => {
                    game.recipe_book.remove(ids);
                }
                NetworkEvent::RecipeBookSettings(settings) => {
                    game.recipe_book.apply_settings(settings);
                }
                NetworkEvent::RecipeData(data) => {
                    game.recipe_book.data = data;
                }
                NetworkEvent::GhostRecipe(ghost) => {
                    use crate::app::phases::in_game::ContainerScreen;

                    let active_recipe_container = if game.inventory_open {
                        Some(0)
                    } else {
                        game.open_container.as_ref().and_then(|container| {
                            matches!(
                                container.screen,
                                ContainerScreen::CraftingTable | ContainerScreen::Furnace(_)
                            )
                            .then_some(container.id)
                        })
                    };
                    if active_recipe_container == Some(ghost.container_id) {
                        game.recipe_book.ghost_recipe = Some(*ghost);
                    }
                }
                NetworkEvent::RecipeToastAdd { entries } => {
                    game.toasts.add_recipes(entries);
                }
                NetworkEvent::MapData(data) => {
                    game.maps.apply(data, crate::net::chat::component_spans);
                }
                NetworkEvent::TitleText { spans } => {
                    game.title.set_title(spans);
                }
                NetworkEvent::SubtitleText { spans } => {
                    game.title.set_subtitle(spans);
                }
                NetworkEvent::TitlesAnimation {
                    fade_in,
                    stay,
                    fade_out,
                } => {
                    game.title.set_times(fade_in, stay, fade_out);
                }
                NetworkEvent::ClearTitles { reset_times } => {
                    game.title.clear(reset_times);
                }
                NetworkEvent::ScoreboardObjective {
                    name,
                    display,
                    number_format,
                } => {
                    game.scoreboard.set_objective(name, display, number_format);
                }
                NetworkEvent::ScoreboardDisplay { name } => {
                    game.scoreboard.set_display(name);
                }
                NetworkEvent::ScoreboardScore {
                    owner,
                    objective,
                    score,
                    display,
                    number_format,
                } => {
                    game.scoreboard
                        .set_score(owner, objective, score, display, number_format);
                }
                NetworkEvent::ScoreboardReset { owner, objective } => {
                    game.scoreboard.reset_score(&owner, objective.as_deref());
                }
                NetworkEvent::ScoreboardTeam {
                    name,
                    display_name,
                    prefix,
                    suffix,
                    color,
                    fill_color,
                    collision_rule,
                    name_tag_visibility,
                    see_friendly_invisibles,
                    members,
                } => {
                    let team = name.clone();
                    game.scoreboard.set_team(
                        name,
                        display_name,
                        prefix,
                        suffix,
                        color,
                        fill_color,
                        collision_rule,
                        see_friendly_invisibles,
                        members,
                    );
                    game.scoreboard
                        .set_name_tag_visibility(&team, name_tag_visibility);
                }
                NetworkEvent::ScoreboardTeamMembers {
                    name,
                    members,
                    join,
                } => {
                    game.scoreboard.update_team_members(&name, members, join);
                }
                NetworkEvent::ScoreboardTeamRemoved { name } => {
                    game.scoreboard.remove_team(&name);
                }
                NetworkEvent::CommandTree { tree } => {
                    game.command_tree = Some(tree);
                }
                NetworkEvent::CommandSuggestions { id, start, options } => {
                    game.chat.apply_server_suggestions(
                        id,
                        start,
                        options,
                        game.command_tree.as_deref(),
                    );
                }
                NetworkEvent::BlockUpdate { pos, state } => {
                    apply_server_block(game, &mut priority_remesh, pos, state);
                }
                NetworkEvent::SectionBlocksUpdate { updates } => {
                    for (pos, state) in updates {
                        apply_server_block(game, &mut priority_remesh, pos, state);
                    }
                }
                NetworkEvent::BlockEntitySync { chunk_pos, entries } => {
                    game.chunk_store.block_entities.retain(|p, _| {
                        p.x.div_euclid(16) != chunk_pos.x || p.z.div_euclid(16) != chunk_pos.z
                    });
                    game.block_entity_anim.drop_chunk(chunk_pos.x, chunk_pos.z);
                    for (pos, kind, nbt) in entries {
                        if kind == azalea_registry::builtin::BlockEntityKind::Piston {
                            let piston = crate::world::piston::MovingPiston::from_nbt(
                                &nbt,
                                game.block_registry.block_state_table(),
                            );
                            game.block_entity_anim.pistons.insert(pos, piston);
                        }
                        game.chunk_store.block_entities.insert(
                            pos,
                            crate::world::block_entity::StoredBlockEntity::new(kind, nbt),
                        );
                    }
                }
                NetworkEvent::BlockEntityUpdate { pos, kind, nbt } => {
                    let chunk_pos =
                        crate::position::ChunkPos::new(pos.x.div_euclid(16), pos.z.div_euclid(16));
                    // `handleBlockEntityData`: a command block's open screen
                    // takes up what the server sent for it.
                    if let Some(ui) = &mut game.game_master_ui
                        && ui.pos() == pos
                    {
                        let state = game.chunk_store.get_block_state(pos.x, pos.y, pos.z);
                        let data = game.block_registry.block_state_data(state);
                        let conditional = data.properties().get("conditional") == Some("true");
                        ui.updated(data.id(), conditional, &nbt, &|text| {
                            renderer.menu_text_width(text, crate::ui::common::FONT_SIZE)
                        });
                    }
                    if game.chunk_store.get_chunk(&chunk_pos).is_some() {
                        game.chunk_store.block_entities.insert(
                            pos,
                            crate::world::block_entity::StoredBlockEntity::new(kind, nbt),
                        );
                    }
                }
                NetworkEvent::BlockEvent {
                    pos,
                    action_id,
                    action_parameter,
                } => {
                    // Vanilla `Level.blockEvent` hands the event to the block
                    // the client has there.
                    let state = game.chunk_store.get_block_state(pos.x, pos.y, pos.z);
                    if crate::world::piston::is_piston_base(
                        game.block_registry.block_state_table(),
                        state,
                    ) {
                        let mut level = ClientPistonLevel::new(game, &mut priority_remesh);
                        crate::world::piston::trigger_event(
                            &mut level,
                            pos,
                            action_id,
                            action_parameter,
                        );
                        for (pos, state) in std::mem::take(&mut level.destroyed) {
                            block_break_effect(&self.audio, game, pos, state);
                        }
                    } else if action_id == 1
                        && game.block_registry.block_state_data(state).id() == "bell"
                    {
                        game.block_entity_anim.ring_bell(pos, action_parameter);
                    } else if action_id == 1
                        && action_parameter < 2
                        && game.block_registry.block_state_data(state).id() == "decorated_pot"
                    {
                        // `DecoratedPotBlockEntity.triggerEvent`: it wobbles,
                        // gladly or not.
                        game.pot_wobbles
                            .insert(pos, (game.sky_state.game_time, action_parameter));
                    } else if action_id == 1
                        && game.block_registry.block_state_data(state).id() == "spawner"
                    {
                        // `BaseSpawner.onEventTriggered`: it spawned; the
                        // wait for the next begins at its shortest.
                        let wait = game
                            .chunk_store
                            .block_entities
                            .get(&pos)
                            .and_then(|be| be.nbt.short("MinSpawnDelay"))
                            .map_or(200, i32::from);
                        game.spawner_spins.entry(pos).or_insert((0.0, 0.0, 0)).2 = wait;
                    } else if action_id == 1
                        && game.block_registry.block_state_data(state).id() == "end_gateway"
                    {
                        // `TheEndGatewayBlockEntity.triggerEvent`: something
                        // went through; its beam shows for two seconds.
                        game.gateways.entry(pos).or_insert((200, 0)).1 = 40;
                    } else if action_id == 1 {
                        // Chest/shulker: the open-viewer count.
                        game.block_entity_anim.set_open_count(pos, action_parameter);
                    }
                }
                NetworkEvent::PlaySound {
                    sound,
                    category,
                    pos,
                    volume,
                    pitch,
                    seed,
                } => {
                    #[cfg(feature = "test-harness")]
                    game.record_sound(&sound);
                    self.audio
                        .play_world_sound(&sound, category, pos, volume, pitch, seed);
                }
                NetworkEvent::PlayEntitySound {
                    sound,
                    category,
                    entity_id,
                    volume,
                    pitch,
                    seed,
                } => {
                    play_entity_sound(
                        &mut self.audio,
                        game,
                        entity_id,
                        &sound,
                        category,
                        volume,
                        pitch,
                        seed,
                    );
                }
                NetworkEvent::StopSound { sound_id, category } => {
                    self.audio.stop_sounds(sound_id.as_deref(), category);
                }
                NetworkEvent::GameModeChanged {
                    game_mode,
                    previous,
                } => {
                    tracing::info!("Game mode changed to {game_mode}");
                    // Vanilla `setLocalMode`: an in-game change records the
                    // replaced mode; login/respawn set it from the packet.
                    match previous {
                        Some(p) => game.previous_game_mode = p,
                        None if game_mode != game.player.game_mode => {
                            game.previous_game_mode = Some(game.player.game_mode);
                        }
                        None => {}
                    }
                    game.player.game_mode = game_mode;
                    // Vanilla GameType.updatePlayerAbilities, applied locally on
                    // setLocalMode (no packet).
                    // TODO: the instabuild/invulnerable/may_build halves
                    match game_mode {
                        // Creative grants flight but leaves `flying` untouched.
                        1 => game.player.may_fly = true,
                        3 => {
                            game.player.may_fly = true;
                            game.player.flying = true;
                        }
                        _ => {
                            game.player.may_fly = false;
                            game.player.flying = false;
                        }
                    }
                    if game.inventory_open || game.creative_inventory_open {
                        if game.inventory_open && matches!(game_mode, 1 | 3) {
                            game.recipe_book_ui
                                .reset_for_closed_screen(&mut game.recipe_book);
                        }
                        match game_mode {
                            1 => {
                                game.inventory_open = false;
                                if !game.creative_inventory_open {
                                    game.open_creative_inventory();
                                }
                            }
                            3 => {
                                game.inventory_open = false;
                                game.close_creative_inventory();
                                self.apply_cursor_grab(window, Some(game));
                            }
                            _ => {
                                game.inventory_open = true;
                                game.close_creative_inventory();
                            }
                        }
                    }
                }
                NetworkEvent::PlayerAbilitiesChanged {
                    flying,
                    can_fly,
                    flying_speed,
                    walking_speed,
                } => {
                    game.player.flying = flying;
                    game.player.may_fly = can_fly;
                    game.player.fly_speed = flying_speed;
                    game.player.walk_speed = walking_speed;
                }
                NetworkEvent::ServerViewDistance { distance } => {
                    game.set_server_view_distance(distance)
                }
                NetworkEvent::ServerSimulationDistance { distance } => {
                    tracing::info!("Server simulation distance: {distance}");
                    game.server_simulation_distance = distance;
                }
                NetworkEvent::BlockChangedAck { seq } => {
                    let mut ack_dirty: Vec<crate::position::BlockPos> = Vec::new();
                    let player_aabb = game.player.bounding_box();
                    let snap = game.interaction.acknowledge(
                        game.block_registry.block_state_table(),
                        seq,
                        &game.chunk_store,
                        player_aabb,
                        &mut ack_dirty,
                    );
                    if let Some(snap) = snap {
                        game.player.position = snap.into();
                        game.player.prev_position = game.player.position;
                    }
                    let min_y = game.chunk_store.min_y();
                    let n = game.chunk_store.section_count();
                    for b in ack_dirty {
                        game.light_engine.on_block_dirty(b.x, b.y, b.z);
                        dirty_sections_for_block(&mut priority_remesh, b.x, b.y, b.z, min_y, n);
                    }
                }
                NetworkEvent::TimeUpdate { game_time, clocks } => {
                    game.sky_state.game_time = game_time;
                    game.clocks.handle_updates(game_time, clocks);
                    game.sync_day_time();
                }
                NetworkEvent::WeatherUpdate(weather) => {
                    // Mirrors vanilla ClientPacketListener.handleGameEvent: the
                    // server drives the level, the client just applies it.
                    use crate::net::Weather;
                    match weather {
                        Weather::StartRaining => game.sky_state.rain_level = 0.0,
                        Weather::StopRaining => game.sky_state.rain_level = 1.0,
                        Weather::RainLevel(level) => game.sky_state.rain_level = level,
                        Weather::ThunderLevel(level) => game.sky_state.thunder_level = level,
                    }
                }
                NetworkEvent::EntitySpawned {
                    id,
                    uuid,
                    entity_type,
                    position,
                    velocity,
                    y_rot_deg,
                    x_rot_deg,
                    head_y_rot_deg,
                    data,
                } => {
                    game.entity_positions.insert(id, position);
                    if entity_type == azalea_registry::builtin::EntityKind::FallingBlock {
                        game.falling_blocks.spawn(id, data, position, velocity);
                    }
                    if entity_type == azalea_registry::builtin::EntityKind::Tnt {
                        let table = game.block_registry.block_state_table();
                        if let Some(tnt) = table.state_with("tnt", &[("unstable", "false")]) {
                            game.falling_blocks
                                .spawn_tnt(id, tnt.id() as u32, position, velocity);
                        }
                    }
                    if entity_type == azalea_registry::builtin::EntityKind::Interaction {
                        game.interactions.spawn(id, position);
                    }
                    if entity_type == azalea_registry::builtin::EntityKind::EndCrystal {
                        game.end_crystals.spawn(id, position);
                    }
                    if entity_type == azalea_registry::builtin::EntityKind::ExperienceOrb {
                        game.experience_orbs.spawn(id, position, velocity);
                    }
                    if entity_type == azalea_registry::builtin::EntityKind::LightningBolt {
                        game.lightning.spawn(id, position.into());
                    }
                    if let azalea_registry::builtin::EntityKind::ItemFrame
                    | azalea_registry::builtin::EntityKind::GlowItemFrame = entity_type
                    {
                        let glow =
                            entity_type == azalea_registry::builtin::EntityKind::GlowItemFrame;
                        game.item_frames.spawn(id, glow, position, data);
                    }
                    if crate::entity::thrown_item::is_thrown_item(entity_type) {
                        game.thrown_items.spawn(id, entity_type, position, velocity);
                    }
                    if crate::entity::fishing_hook::is_fishing_hook(entity_type) {
                        game.fishing_hooks.spawn(id, data, *position);
                    }
                    if entity_type == azalea_registry::builtin::EntityKind::AreaEffectCloud {
                        game.effect_clouds.spawn(id, *position);
                    }
                    if entity_type == azalea_registry::builtin::EntityKind::Painting {
                        game.paintings.spawn(id, position, data);
                    }
                    if crate::entity::model_entity::is_model_entity(entity_type) {
                        game.model_entities.spawn(
                            id,
                            entity_type,
                            position,
                            velocity,
                            y_rot_deg,
                            x_rot_deg,
                        );
                    }
                    if crate::entity::arrow::is_arrow(entity_type) {
                        game.arrows.spawn(
                            id,
                            entity_type,
                            position,
                            velocity,
                            y_rot_deg,
                            x_rot_deg,
                        );
                    }
                    game.silent_entities.remove(&id);
                    if crate::entity::is_living_mob(&entity_type) {
                        game.entity_store.spawn_living(
                            id,
                            entity_type,
                            position,
                            LookDirection::new(head_y_rot_deg, x_rot_deg),
                            uuid,
                        );
                        if entity_type == azalea_registry::builtin::EntityKind::Player {
                            let textures = game
                                .tab_list
                                .players
                                .get(&uuid)
                                .and_then(|p| p.textures.clone());
                            self.queue_player_skin(uuid, textures);
                        }
                    }
                    if entity_type == azalea_registry::builtin::EntityKind::Item {
                        game.item_entity_store
                            .spawn_item(id, uuid, position, velocity);
                    }
                    if let Some(kind) = crate::entity::display::DisplayKind::of(entity_type) {
                        game.display_store
                            .spawn(id, kind, *position, y_rot_deg, x_rot_deg);
                    }
                    game.name_store.spawn(id, entity_type, uuid);
                }
                NetworkEvent::EntityMoved {
                    id,
                    dx,
                    dy,
                    dz,
                    on_ground,
                } => {
                    if !game.drives_vehicle(id) {
                        game.entity_store
                            .move_living_delta(id, dx, dy, dz, on_ground);
                    }
                    game.item_entity_store.move_delta(id, dx, dy, dz, on_ground);
                    game.thrown_items.move_delta(id, dvec3(dx, dy, dz));
                    game.model_entities.move_delta(id, dvec3(dx, dy, dz));
                    game.effect_clouds.move_delta(id, dvec3(dx, dy, dz));
                    game.arrows.move_delta(id, dvec3(dx, dy, dz));
                    game.fishing_hooks.move_delta(id, dvec3(dx, dy, dz));
                    game.falling_blocks
                        .move_delta(id, dvec3(dx, dy, dz), on_ground);
                    game.interactions.move_delta(id, dvec3(dx, dy, dz));
                    game.experience_orbs.move_delta(id, dvec3(dx, dy, dz));
                    game.end_crystals.move_delta(id, dvec3(dx, dy, dz));
                    game.display_store.move_delta(id, dvec3(dx, dy, dz), None);
                    if let Some(pos) = game.entity_positions.get_mut(&id) {
                        *pos += dvec3(dx, dy, dz);
                        self.audio.update_entity_sound_position(id, *pos);
                    }
                }
                NetworkEvent::EntityMovedRotated {
                    id,
                    dx,
                    dy,
                    dz,
                    y_rot_deg,
                    x_rot_deg,
                    on_ground,
                } => {
                    if !game.drives_vehicle(id) {
                        game.entity_store
                            .move_living_delta(id, dx, dy, dz, on_ground);
                    }
                    if !game.drives_vehicle(id) {
                        game.entity_store
                            .rotate_living(id, y_rot_deg, x_rot_deg, on_ground);
                    }
                    game.arrows.rotate(id, y_rot_deg, x_rot_deg);
                    game.model_entities.rotate(id, y_rot_deg, x_rot_deg);
                    game.item_entity_store.move_delta(id, dx, dy, dz, on_ground);
                    game.thrown_items.move_delta(id, dvec3(dx, dy, dz));
                    game.model_entities.move_delta(id, dvec3(dx, dy, dz));
                    game.effect_clouds.move_delta(id, dvec3(dx, dy, dz));
                    game.arrows.move_delta(id, dvec3(dx, dy, dz));
                    game.fishing_hooks.move_delta(id, dvec3(dx, dy, dz));
                    game.falling_blocks
                        .move_delta(id, dvec3(dx, dy, dz), on_ground);
                    game.interactions.move_delta(id, dvec3(dx, dy, dz));
                    game.experience_orbs.move_delta(id, dvec3(dx, dy, dz));
                    game.display_store.move_delta(
                        id,
                        dvec3(dx, dy, dz),
                        Some((y_rot_deg, x_rot_deg)),
                    );
                    if let Some(pos) = game.entity_positions.get_mut(&id) {
                        *pos += dvec3(dx, dy, dz);
                        self.audio.update_entity_sound_position(id, *pos);
                    }
                }
                NetworkEvent::EntityRotated {
                    id,
                    y_rot_deg,
                    x_rot_deg,
                    on_ground,
                } => {
                    if !game.drives_vehicle(id) {
                        game.entity_store
                            .rotate_living(id, y_rot_deg, x_rot_deg, on_ground);
                    }
                    game.arrows.rotate(id, y_rot_deg, x_rot_deg);
                    game.model_entities.rotate(id, y_rot_deg, x_rot_deg);
                    game.display_store.rotate(id, y_rot_deg, x_rot_deg);
                }
                NetworkEvent::EntityMotion { id, velocity } => {
                    // Vanilla `Entity.lerpMotion` sets velocity; the local
                    // player lives outside the entity stores.
                    if id == game.player.entity_id {
                        game.player.velocity = velocity.into();
                    } else {
                        game.item_entity_store.set_motion(id, velocity);
                        game.experience_orbs.set_motion(id, velocity);
                        game.falling_blocks.set_motion(id, velocity);
                        game.arrows.set_motion(id, velocity);
                        game.thrown_items.set_motion(id, velocity);
                        game.model_entities.set_motion(id, velocity);
                        game.entity_store.set_living_motion(id, velocity);
                    }
                }
                NetworkEvent::PlayerKnockback { delta } => {
                    // Vanilla `handleExplosion`: `LocalPlayer.addDeltaMovement`.
                    *game.player.velocity += delta;
                }
                NetworkEvent::Explosion {
                    center,
                    radius,
                    block_count,
                    particle,
                    sound,
                    block_particles,
                } => {
                    // Vanilla `handleExplosion`: the blast's sound and its
                    // particle at the centre, then the blown blocks' debris.
                    if let Some(sound) = sound {
                        self.audio.play_world_sound(
                            &sound,
                            crate::audio::CATEGORY_BLOCKS,
                            Position::new(center.x, center.y, center.z),
                            4.0,
                            (1.0 + (fastrand::f32() - fastrand::f32()) * 0.2) * 0.7,
                            fastrand::u64(..),
                        );
                    }
                    let camera = renderer.camera_render_position();
                    if let Some((kind, options)) = particle {
                        let look = particle_look(game, options, center);
                        game.particle_store.add_particles_from_packet(
                            kind,
                            options,
                            look,
                            false,
                            center,
                            glam::DVec3::X,
                            1.0,
                            0,
                            camera,
                        );
                    }
                    let block_particles = block_particles
                        .into_iter()
                        .map(|p| (p, particle_look(game, p.options, center)))
                        .collect();
                    game.particle_store.track_explosion(
                        center,
                        radius,
                        block_count,
                        block_particles,
                        camera,
                    );
                }
                NetworkEvent::EntityTeleported {
                    id,
                    position,
                    velocity,
                    y_rot_deg,
                    x_rot_deg,
                    on_ground,
                } => {
                    if !game.drives_vehicle(id) {
                        game.entity_store.teleport_living(id, position, on_ground);
                    }
                    if !game.drives_vehicle(id) {
                        game.entity_store
                            .rotate_living(id, y_rot_deg, x_rot_deg, on_ground);
                    }
                    game.arrows.rotate(id, y_rot_deg, x_rot_deg);
                    if let Some(velocity) = velocity {
                        game.entity_store.set_living_motion(id, velocity);
                    }
                    game.item_entity_store
                        .teleport(id, position, velocity, on_ground);
                    game.falling_blocks.teleport(id, position, velocity);
                    game.interactions.teleport(id, position);
                    game.end_crystals.teleport(id, position);
                    game.experience_orbs.teleport(id, position);
                    game.item_frames.teleport(id, position);
                    game.thrown_items.teleport(id, position);
                    game.arrows.teleport(id, position);
                    game.fishing_hooks.teleport(id, *position);
                    game.arrows.rotate(id, y_rot_deg, x_rot_deg);
                    game.model_entities.teleport(id, position);
                    game.effect_clouds.teleport(id, *position);
                    game.model_entities.rotate(id, y_rot_deg, x_rot_deg);
                    if let Some(velocity) = velocity {
                        game.thrown_items.set_motion(id, velocity);
                        game.arrows.set_motion(id, velocity);
                        game.model_entities.set_motion(id, velocity);
                    }
                    game.display_store
                        .teleport(id, *position, y_rot_deg, x_rot_deg);
                    game.entity_positions.insert(id, position);
                    self.audio.update_entity_sound_position(id, position);
                }
                NetworkEvent::LevelEvent {
                    event_type,
                    pos,
                    data,
                } => {
                    // Vanilla `LevelEventHandler` case 2001 (block break).
                    // The server excludes the breaking player from the
                    // broadcast; the local break's effects come from
                    // `predict_destroy`. TODO: the other level events.
                    // 1010/1011 (jukebox start/stop) only notify nearby
                    // entities so far. TODO: the song itself and the
                    // now-playing HUD line.
                    if event_type == 1010 || event_type == 1011 {
                        game.entity_store
                            .set_record_playing_nearby(pos, event_type == 1010);
                    }
                    if event_type == 2001
                        && let Some(state) = game.block_registry.block_state_table().try_state(data)
                    {
                        block_break_effect(&self.audio, game, pos, state);
                    }
                }
                NetworkEvent::LevelParticles {
                    kind,
                    options,
                    override_limiter,
                    pos,
                    x_dist,
                    y_dist,
                    z_dist,
                    max_speed,
                    count,
                } => {
                    let look = particle_look(game, options, pos);
                    game.particle_store.add_particles_from_packet(
                        kind,
                        options,
                        look,
                        override_limiter,
                        pos,
                        glam::dvec3(x_dist as f64, y_dist as f64, z_dist as f64),
                        max_speed as f64,
                        count,
                        renderer.camera_render_position(),
                    );
                }
                NetworkEvent::MannequinProfile { id, profile } => {
                    if let Some(entity) = game.entity_store.living.get(&id) {
                        let uuid = entity.uuid;
                        self.queue_profile_skin(uuid, &profile);
                    }
                }
                NetworkEvent::MannequinDescription { id, spans } => {
                    game.name_store.set_description(id, spans);
                }
                NetworkEvent::EntitiesRemoved { ids } => {
                    for id in &ids {
                        if let Some(entity) = game.entity_store.remove_living(*id) {
                            if let Some(uuid) = entity.player_uuid
                                && !game.entity_store.has_player_uuid(&uuid)
                            {
                                self.remove_player_skin(renderer, &uuid);
                            }
                            if entity.entity_type == azalea_registry::builtin::EntityKind::Mannequin
                            {
                                self.remove_player_skin(renderer, &entity.uuid);
                            }
                        }
                        game.entity_positions.remove(id);
                        game.silent_entities.remove(id);
                        self.audio.stop_entity_sounds(*id);
                    }
                    game.item_entity_store.remove(&ids);
                    game.falling_blocks.remove(&ids);
                    game.interactions.remove(&ids);
                    game.end_crystals.remove(&ids);
                    game.item_frames.remove(&ids);
                    game.lightning.remove(&ids);
                    game.experience_orbs.remove(&ids);
                    game.thrown_items.remove(&ids);
                    game.model_entities.remove(&ids);
                    game.paintings.remove(&ids);
                    game.effect_clouds.remove(&ids);
                    game.leashes.remove(&ids);
                    game.arrows.remove(&ids);
                    game.fishing_hooks.remove(&ids);
                    game.display_store.remove(&ids);
                    game.name_store.remove(&ids);
                    if game.controlled_vehicle_id.is_some_and(|v| ids.contains(&v)) {
                        game.controlled_vehicle_id = None;
                    }
                    if game.riding_vehicle_id.is_some_and(|v| ids.contains(&v)) {
                        game.riding_vehicle_id = None;
                    }
                }
                NetworkEvent::EntityHeadRotation {
                    id,
                    head_y_rot_deg: head_y_rot,
                } => {
                    game.entity_store.update_head_rotation(id, head_y_rot);
                }
                NetworkEvent::EntityItemData {
                    id,
                    item_name,
                    item_id,
                    damage,
                    count,
                    stack,
                } => {
                    // The stack's own model (a pack's `custom_model_data`
                    // case, an `item_model`), as the screens draw it.
                    let model = renderer.render_assets().stack_model_key(&item_name, &stack);
                    // Dropped and thrown items draw in the GROUND view.
                    let model = renderer.render_assets().context_model_key(&model, 7);
                    renderer.ensure_item_mesh(&model);
                    game.thrown_items.set_item(id, model.clone(), stack.clone());
                    game.item_frames.set_item(id, Some(stack.clone()));
                    game.item_entity_store
                        .set_item_data(id, model, item_id, damage, count, stack);
                }
                NetworkEvent::FrameItemData { id, stack } => {
                    game.item_frames.set_item(id, stack);
                }
                NetworkEvent::EntityData { id, index, value } => {
                    if index == 4
                        && let crate::entity::MetaValue::Bool(silent) = &value
                    {
                        if *silent {
                            game.silent_entities.insert(id);
                            self.audio.stop_entity_sounds(id);
                        } else {
                            game.silent_entities.remove(&id);
                        }
                    }
                    if index == 7
                        && id == game.player.entity_id
                        && let crate::entity::MetaValue::Int(ticks) = value
                    {
                        game.player.ticks_frozen = ticks;
                    }
                    if let crate::entity::MetaValue::Byte(flags) = value {
                        if id == game.player.entity_id {
                            match index {
                                0 => game.player.sync_shared_flags(flags),
                                8 => game.interaction.sync_using_item_flag(flags & 1 != 0),
                                _ => {}
                            }
                        }
                        if index == 0 {
                            game.item_entity_store.set_shared_flags(id, flags);
                        }
                    }
                    game.display_store.apply_entity_data(id, index, value);
                    game.interactions.apply_entity_data(id, index, value);
                    game.end_crystals.apply_entity_data(id, index, value);
                    game.item_frames.apply_entity_data(id, index, value);
                    game.experience_orbs.apply_entity_data(id, index, value);
                    game.arrows.apply_entity_data(id, index, &value);
                    game.model_entities.apply_entity_data(id, index, value);
                    game.effect_clouds.apply_entity_data(id, index, value);
                    game.name_store.apply_entity_data(id, index, value);
                    if index == 10
                        && let crate::entity::MetaValue::Bool(shot_at_angle) = value
                    {
                        game.thrown_items.set_shot_at_angle(id, shot_at_angle);
                    }
                    if index == 5
                        && let crate::entity::MetaValue::Bool(no_gravity) = value
                    {
                        game.falling_blocks.set_no_gravity(id, no_gravity);
                        game.thrown_items.set_no_gravity(id, no_gravity);
                        game.item_entity_store.set_no_gravity(id, no_gravity);
                    }
                    if index == 8
                        && let crate::entity::MetaValue::Int(fuse) = value
                    {
                        game.falling_blocks.set_fuse(id, fuse);
                    }
                    game.entity_store.apply_entity_data(id, index, value);
                }
                NetworkEvent::DisplayData { id, index, value } => {
                    if index == 9
                        && let crate::entity::display::DisplayValue::BlockState(state) = &value
                    {
                        game.falling_blocks.set_tnt_state(id, *state);
                    }
                    game.display_store.apply_display_data(id, index, value);
                }
                NetworkEvent::EntityPose {
                    id,
                    is_crouching,
                    pose,
                } => {
                    game.entity_store.set_crouching(id, is_crouching);
                    game.entity_store.set_pose(id, pose);
                }
                NetworkEvent::VehicleMoved {
                    position,
                    y_rot,
                    x_rot,
                } => {
                    if let Some(id) = game.controlled_vehicle_id
                        && game.drives_vehicle(id)
                        && let Some(horse) = game.entity_store.living.get_mut(&id)
                    {
                        horse.position = position.into();
                        horse.prev_position = horse.position;
                        horse.look_dir =
                            crate::entity::components::LookDirection::new(y_rot, x_rot);
                        horse.velocity = glam::DVec3::ZERO;
                    }
                    if let Some(id) = game.rowed_boat()
                        && let Some(boat) = game.model_entities.entities.get_mut(&id)
                    {
                        boat.put(position.into(), y_rot, x_rot);
                    }
                }
                NetworkEvent::SetCamera { id } => {
                    if id == game.player.entity_id {
                        game.set_camera(None);
                    } else if game.entity_store.living.contains_key(&id) {
                        game.set_camera(Some(id));
                    }
                }
                NetworkEvent::EntitySleepingPos { id, pos } => {
                    game.entity_store.set_sleeping_pos(id, pos);
                    if id == game.player.entity_id {
                        game.player.set_sleeping_pos(pos);
                    }
                }
                NetworkEvent::EntityWakeUp { id } => {
                    // TODO: vanilla `stopSleeping` also stands a remote sleeper
                    // up beside the bed, facing it, with pitch 0.
                    game.entity_store.set_sleeping_pos(id, None);
                    if id == game.player.entity_id {
                        game.player.wake_up();
                    }
                }
                NetworkEvent::SheepEatStart { id } => {
                    game.entity_store.start_sheep_eat(id);
                }
                NetworkEvent::FinishUseItem { id } => {
                    // Vanilla sends event 9 only to the eater; remote players'
                    // eating effects come from each client simulating their use
                    // ticks off entity flags (TODO, with third-person items).
                    if id == game.player.entity_id {
                        game.interaction.complete_using(
                            &self.audio,
                            &mut game.particle_store,
                            game.player.position.into(),
                            game.player.eye_pos().into(),
                            game.player.look_dir,
                        );
                    }
                }
                // `FireworkRocketEntity.handleEntityEvent`: the stars of the
                // rocket's item burst where it is, with their blast.
                NetworkEvent::FireworkExplode { id } => {
                    if let Some(rocket) = game.thrown_items.items.get(&id) {
                        let explosions = firework_explosions(rocket.stack.as_ref());
                        let at = glam::DVec3::from(rocket.position);
                        if !explosions.is_empty() {
                            let large = explosions.len() >= 3
                                || explosions
                                    .iter()
                                    .any(|e| e.shape == crate::particle::FireworkShape::LargeBall);
                            let far =
                                renderer.camera_render_position().distance_squared(at) >= 256.0;
                            let sound = match (large, far) {
                                (true, true) => "entity.firework_rocket.large_blast_far",
                                (true, false) => "entity.firework_rocket.large_blast",
                                (false, true) => "entity.firework_rocket.blast_far",
                                (false, false) => "entity.firework_rocket.blast",
                            };
                            self.audio.play_world_sound(
                                &crate::audio::SoundRef::event(sound),
                                crate::audio::CATEGORY_AMBIENT,
                                Position::new(at.x, at.y, at.z),
                                20.0,
                                0.95 + fastrand::f32() * 0.1,
                                fastrand::u64(..),
                            );
                        }
                        // A twinkling star crackles as the firework ends,
                        // fifteen ticks after its last star has burst.
                        if explosions.iter().any(|e| e.twinkle) {
                            game.firework_twinkles
                                .push((at, explosions.len() as u32 * 2 + 15));
                        }
                        game.particle_store
                            .start_fireworks(at, rocket.velocity, explosions);
                    }
                }
                NetworkEvent::EntityLeash { id, holder } => {
                    game.leashes.set(id, holder);
                }
                NetworkEvent::EntityParticle { id, particle } => {
                    game.effect_clouds.set_particle(id, particle);
                }
                NetworkEvent::RocketTarget { id, target } => {
                    game.thrown_items.set_attached(id, target);
                }
                NetworkEvent::OptionalBlockState { id, index, state } => {
                    if index == 11 {
                        game.model_entities.set_display_block(id, state);
                    }
                    if index == 16 {
                        game.entity_store.set_carried_block(id, state);
                    }
                }
                NetworkEvent::PaintingVariant { id, variant } => {
                    game.paintings.set_variant(id, variant);
                }
                NetworkEvent::EntityVariant { id, kind, variant } => {
                    game.entity_store.set_variant(id, kind, variant);
                }
                NetworkEvent::WolfShaking { id, shaking } => {
                    game.entity_store.set_wolf_shaking(id, shaking);
                }
                NetworkEvent::RabbitJump { id } => {
                    game.entity_store.start_rabbit_jump(id);
                }
                NetworkEvent::SnifferDigging { id } => {
                    super::sniffer::start_digging_sound(&mut self.audio, game, id);
                }
                NetworkEvent::FoxEat { id } => {
                    if let Some(fox) = game.entity_store.living.get(&id)
                        && let Some(texture) = mouth_item_texture(fox)
                    {
                        game.particle_store.add_fox_eating_particles(
                            &texture,
                            fox.position.into(),
                            fox.look_dir,
                            renderer.camera_render_position(),
                        );
                    }
                }
                // `Armadillo.handleEntityEvent`: a local sound at its position.
                NetworkEvent::ArmadilloPeek { id } => {
                    if let Some(pos) = game.entity_store.armadillo_peek(id) {
                        self.audio.play_world_sound(
                            &crate::audio::SoundRef::event("entity.armadillo.peek"),
                            crate::audio::CATEGORY_NEUTRAL,
                            pos,
                            1.0,
                            1.0,
                            fastrand::u64(..),
                        );
                    }
                }
                NetworkEvent::GoatRam { id, lowering } => {
                    game.entity_store.set_goat_lowering_head(id, lowering);
                }
                NetworkEvent::SquidTentacleReset { id } => {
                    game.entity_store.squid_tentacle_reset(id);
                }
                NetworkEvent::GolemPunch { id } => {
                    game.entity_store.golem_punch(id);
                    // The same event starts a pair of evoker fangs' bite
                    // (`EvokerFangs.handleEntityEvent`), with its snap.
                    if let Some(at) = game.model_entities.start_bite(id)
                        && !game.silent_entities.contains(&id)
                    {
                        self.audio.play_world_sound(
                            &crate::audio::SoundRef::event("entity.evoker_fangs.attack"),
                            crate::audio::CATEGORY_NEUTRAL,
                            at,
                            1.0,
                            fastrand::f32() * 0.2 + 0.85,
                            fastrand::u64(..),
                        );
                    }
                }
                NetworkEvent::GolemOfferFlower { id, offering } => {
                    game.entity_store.set_golem_offering_flower(id, offering);
                }
                NetworkEvent::VillagerData {
                    id,
                    kind,
                    profession,
                    level,
                } => {
                    game.entity_store
                        .set_villager_data(id, kind, profession, level);
                }
                NetworkEvent::EntityCustomName { id, name, spans } => {
                    game.entity_store.set_custom_name(id, name);
                    game.name_store.set_name(id, spans);
                }
                NetworkEvent::EntitySwing { id } => {
                    game.entity_store.start_swing(id);
                }
                NetworkEvent::EntityDamaged { id } => hurt_entity(game, id, None),
                NetworkEvent::HurtAnimation { id, yaw } => hurt_entity(game, id, Some(yaw)),
                NetworkEvent::PermissionLevel { id, level } => {
                    if id == game.player.entity_id {
                        game.player.permission_level = level;
                    }
                }
                NetworkEvent::EntityStatus { id, status } => {
                    super::entity_status::apply(
                        &self.audio,
                        game,
                        renderer.camera_render_position(),
                        id,
                        status,
                        self.input.selected_slot(),
                    );
                }
                NetworkEvent::EntityDied { id } => {
                    // Event 3 on a snowball or an egg is its burst.
                    super::entity_status::projectile_break(
                        game,
                        renderer.camera_render_position(),
                        id,
                    );
                    // Event 3 plays `getDeathSound` through `Entity.playSound`,
                    // which on the client sounds only for the local player
                    // (`ClientLevel.playSeededSound`); a mob's death sound is
                    // the server's own broadcast.
                    if id == game.player.entity_id {
                        let pitch = (fastrand::f32() - fastrand::f32()) * 0.2 + 1.0;
                        self.audio.play_world_sound(
                            &crate::audio::SoundRef::event("entity.player.death"),
                            crate::audio::CATEGORY_PLAYERS,
                            game.player.position,
                            1.0,
                            pitch,
                            fastrand::u64(..),
                        );
                    }
                    game.entity_store.mark_dead(id);
                }
                NetworkEvent::ItemPickedUp {
                    item_id,
                    collector_id,
                    amount,
                } => {
                    let target_pos = game
                        .entity_store
                        .living
                        .get(&collector_id)
                        .map(|e| e.position + dvec3(0.0, 0.81, 0.0))
                        .unwrap_or_else(|| {
                            Position::new(
                                game.player.position.x,
                                game.player.position.y + 0.81,
                                game.player.position.z,
                            )
                        });
                    if let Some(at) = game.experience_orbs.take(item_id) {
                        // `handleTakeItemEntity`'s sound for an orb.
                        self.audio.play_world_sound(
                            &crate::audio::SoundRef::event("entity.experience_orb.pickup"),
                            crate::audio::CATEGORY_PLAYERS,
                            Position::new(at.x, at.y, at.z),
                            0.1,
                            (fastrand::f32() - fastrand::f32()) * 0.35 + 0.9,
                            fastrand::u64(..),
                        );
                    }
                    if let Some(item_pos) =
                        game.item_entity_store.pickup(item_id, target_pos, amount)
                    {
                        // Vanilla plays this client-side in handleTakeItemEntity.
                        self.audio.play_world_sound(
                            &crate::audio::SoundRef::event("entity.item.pickup"),
                            crate::audio::CATEGORY_PLAYERS,
                            item_pos,
                            0.2,
                            (fastrand::f32() - fastrand::f32()) * 1.4 + 2.0,
                            fastrand::u64(..),
                        );
                    }
                }
                NetworkEvent::PlayerLogin {
                    entity_id,
                    hardcore,
                    show_death_screen,
                    online_mode,
                } => {
                    connection.packet_tx.chat_login(online_mode);
                    game.recipe_book_ui
                        .reset_for_closed_screen(&mut game.recipe_book);
                    game.recipe_book.reset_for_login();
                    game.player.entity_id = entity_id;
                    game.set_camera(None);
                    game.hardcore = hardcore;
                    game.online_mode = online_mode;
                    game.show_death_screen = show_death_screen;
                    // The fresh LocalPlayer starts from supplier defaults.
                    game.player.attributes = AttributeMap::player();
                    sync_player_attribute_mirrors(&mut game.player, |_| false);
                    // Vanilla `handleLogin` builds a new LocalPlayer on slot 0,
                    // with the server's SetHeldSlot to follow. Ours lives in the
                    // app-wide input state, so a reconnect would otherwise
                    // report the last session's slot.
                    self.input.set_selected_slot(0);
                    game.start_level_load();
                    // `startWaitingForNewLevel` swaps in the level-loading
                    // screen, replacing a configuration-phase dialog.
                    game.configuring = false;
                    game.server_dialog = None;
                    self.apply_cursor_grab(window, Some(game));
                }
                NetworkEvent::SecureChatEnforced { enforced } => {
                    game.server_enforces_secure_chat = enforced;
                    // `handleLogin`: the warning of a server that does not
                    // enforce secure chat, once a connection (and not again
                    // on the server it transfers to).
                    // TODO: 1.20.x gives its word in `server_data`, after a
                    // placeholder false from `login`; it gets no toast.
                    if !enforced
                        && !game.singleplayer
                        && !self.seen_insecure_chat_warning
                        && crate::version::session_protocol() >= 766
                    {
                        self.seen_insecure_chat_warning = true;
                        game.toasts.warn_insecure_chat(
                            &|text, width| toast_lines(renderer, text, width),
                            &|text| renderer.menu_text_width(text, crate::ui::common::FONT_SIZE),
                        );
                    }
                }
                NetworkEvent::LowDiskSpaceWarning => {
                    game.toasts.warn_low_disk_space(&|text| {
                        renderer.menu_text_width(text, crate::ui::common::FONT_SIZE)
                    });
                }
                NetworkEvent::PlayerScore { entity_id, score } => {
                    if entity_id == game.player.entity_id {
                        game.player.score = score;
                    }
                }
                NetworkEvent::PlayerAbsorption {
                    entity_id,
                    absorption,
                } => {
                    if entity_id == game.player.entity_id {
                        game.player.absorption = absorption;
                    }
                }
                NetworkEvent::PlayerRespawned {
                    keep_entity_data,
                    keep_attribute_modifiers,
                } => {
                    if !keep_entity_data {
                        game.player.absorption = 0.0;
                    }
                    game.dead = false;
                    game.set_camera(None);
                    // `startWaitingForNewLevel` replaces an open dialog here too.
                    game.server_dialog = None;
                    game.start_level_load();
                    game.player.reset_for_respawn(keep_entity_data);
                    sync_respawn_attributes(&mut game.player, keep_attribute_modifiers);
                    game.interaction.reset_player_transients_for_respawn();
                    // A fresh LocalPlayer gets a fresh KeyboardInput and packet
                    // baselines even when bit 2 keeps its entity data, so a kept
                    // sprint flag re-sends START_SPRINTING next tick as vanilla does.
                    game.last_sent_input = PlayerInputState::default();
                    game.was_sprinting = false;
                    game.last_sent_pos = Position::default();
                    game.last_sent_look_dir = LookDirection::default();
                    game.last_sent_on_ground = false;
                    game.last_sent_horizontal_collision = false;
                    game.position_send_counter = 0;
                    game.reset_death_screen();
                    self.apply_cursor_grab(window, Some(game));
                }
                NetworkEvent::PlayerDied { player_id, message } => {
                    if player_id != game.player.entity_id {
                        continue;
                    }
                    match death_route(game.show_death_screen) {
                        DeathRoute::ShowDeathScreen => {
                            self.open_death_screen(connection, window, game, Some(message));
                        }
                        DeathRoute::Respawn => {
                            game.death_message = message;
                            game.reset_death_screen();
                            self.send_respawn(connection, game);
                        }
                    }
                }
                NetworkEvent::ResourcePackPush {
                    id,
                    url,
                    hash,
                    required,
                } => self.push_resource_pack(id, url, hash, required),
                NetworkEvent::ResourcePackPop { id } => {
                    // A server may pop an id it already popped, or never pushed.
                    let removed = match id {
                        Some(id) => self.resource_packs.remove_server_pack(&id),
                        None => self.resource_packs.clear_server_packs(),
                    };
                    if removed {
                        self.begin_live_resource_reload(renderer);
                    }
                }
                NetworkEvent::Transfer { host, port } => {
                    tracing::info!("Transferred to {host}:{port}");
                    self.pending_transfer = Some((host, port));
                }
                NetworkEvent::Reconfiguring => {
                    tracing::info!("Server re-entered configuration");
                    game.block_registry.clear_tags();
                    self.audio.stop_all_sounds();
                    game.entity_store = crate::entity::EntityStore::new();
                    game.item_entity_store = crate::entity::ItemEntityStore::new();
                    game.falling_blocks.clear();
                    game.interactions.clear();
                    game.end_crystals.clear();
                    game.item_frames.clear();
                    game.lightning.clear();
                    game.experience_orbs.clear();
                    game.thrown_items.clear();
                    game.model_entities.clear();
                    game.paintings.clear();
                    game.effect_clouds.clear();
                    game.leashes.clear();
                    game.firework_twinkles.clear();
                    game.block_breaking.clear();
                    // A level has its own border, which the server names anew.
                    game.world_border = Default::default();
                    game.tick_rate = Default::default();
                    game.pot_wobbles.clear();
                    game.arrows.clear();
                    game.fishing_hooks.clear();
                    game.display_store.clear();
                    game.name_store.clear();
                    game.entity_positions.clear();
                    game.silent_entities.clear();
                    game.action_bar = None;
                    game.waypoints = crate::world::waypoints::WaypointMap::default();
                    // `ServerReconfigScreen` replaces any dialog; the server
                    // links carry over.
                    game.server_dialog = None;
                    game.configuring = true;
                    game.reset_sleep_for_level_teardown();
                    self.clear_server_ui(game, renderer);
                    self.apply_cursor_grab(window, Some(game));
                }
                NetworkEvent::Disconnected { reason } => {
                    tracing::warn!("Disconnected: {reason}");
                    // Nothing to acknowledge to a server that's gone.
                    self.pending_pack_acks.clear();
                    disconnect_reason = Some(reason);
                    self.clear_server_ui(game, renderer);
                }
                NetworkEvent::PlayerInfoUpdate { actions, entries } => {
                    if actions.add_player {
                        for entry in &entries {
                            self.queue_player_skin(entry.uuid, entry.textures.clone());
                        }
                    } else {
                        for entry in entries.iter().filter(|e| e.textures.is_some()) {
                            self.queue_player_skin(entry.uuid, entry.textures.clone());
                        }
                    }
                    game.tab_list.apply_update(&actions, &entries);
                }
                NetworkEvent::PlayerInfoRemove { uuids } => {
                    for uuid in &uuids {
                        if !game.entity_store.has_player_uuid(uuid) {
                            self.remove_player_skin(renderer, uuid);
                        }
                    }
                    game.tab_list.remove(&uuids);
                }
                NetworkEvent::TabListHeaderFooter { header, footer } => {
                    game.tab_list.set_header_footer(header, footer);
                }
            }
        }

        if let Some(pending) = &self.pending_pack_download
            && pending.handle.is_finished()
        {
            let pending = self.pending_pack_download.take().unwrap();
            let result = pending.handle.join();

            use azalea_protocol::packets::game::s_resource_pack;
            let action: Option<s_resource_pack::Action> = match result {
                Err(_) => {
                    tracing::error!("Resource pack {} thread panicked", pending.id);
                    if pending.required {
                        disconnect_reason = Some(
                            "Required resource pack failed: thread panicked (internal error)"
                                .into(),
                        );
                    }
                    Some(s_resource_pack::Action::FailedDownload)
                }
                Ok(Err(e)) => {
                    tracing::error!("Resource pack {} failed: {e}", pending.id);
                    if pending.required {
                        disconnect_reason = Some(format!("Required resource pack failed: {e}"));
                    }
                    Some(s_resource_pack::Action::FailedDownload)
                }
                Ok(Ok(_path)) => {
                    self.resource_packs
                        .apply_server_pack(pending.id, &pending.hash);
                    // Vanilla acknowledges SuccessfullyLoaded only after the
                    // resources have been applied: the assets prepare off the
                    // render thread and the reply waits for their swap.
                    self.begin_live_resource_reload(renderer);
                    self.pending_pack_acks.push(pending.id);
                    None
                }
            };

            if let Some(action) = action {
                // It failed: nothing is loading any more.
                self.pack_progress = None;
                connection
                    .packet_tx
                    .send(ServerboundGamePacket::ResourcePack(
                        s_resource_pack::ServerboundResourcePack {
                            id: pending.id,
                            action,
                        },
                    ));
            }
            self.menu.active_packs = self.resource_packs.active_pack_info();
        }

        if renderer.finish_asset_reload() {
            self.pack_progress = None;
            self.finish_live_resource_reload(game, renderer);
            use azalea_protocol::packets::game::s_resource_pack;
            for id in self.pending_pack_acks.drain(..) {
                tracing::info!("Resource pack {id} loaded successfully");
                connection
                    .packet_tx
                    .send(ServerboundGamePacket::ResourcePack(
                        s_resource_pack::ServerboundResourcePack {
                            id,
                            action: s_resource_pack::Action::SuccessfullyLoaded,
                        },
                    ));
            }
        }

        let player_chunk = game.player_chunk();
        enqueue_edits(game, &priority_remesh);

        // Refresh the frustum tiers (throttled to camera movement / new loads),
        // then enqueue everything that needs meshing — visible-first, with hidden
        // columns backfilled at a bounded rate so the world still completes.
        // New chunk loads mark themselves dirty when their queued light
        // applies (GameState::update_light), which raises this flag.
        let loads_happened = std::mem::take(&mut game.pending_load_rescan);
        let ms = |t: std::time::Instant| t.elapsed().as_secs_f32() * 1000.0;
        game.last_update_phases.net_decode_ms = ms(t_net);
        crate::frame_log::add("net", ms(t_net));

        let t_vis = std::time::Instant::now();
        game.update_visibility(renderer, player_chunk, loads_happened);
        game.last_update_phases.visibility_ms = ms(t_vis);
        crate::frame_log::add("visibility", ms(t_vis));

        let t_rescan = std::time::Instant::now();
        game.rescan_mesh_jobs(self.menu.chunk_detail);
        game.last_update_phases.rescan_ms = ms(t_rescan);
        crate::frame_log::add("rescan", ms(t_rescan));

        disconnect_reason
    }

    /// Vanilla `ClientPacketListener.tick`'s level-load half: advance the
    /// tracker, and the first tick the level is ready send `player_loaded`
    /// (`notifyPlayerLoaded`) and drop the tracker. Runs at the start of every
    /// client tick, in the loading phase as well as in game, so a respawn or a
    /// dimension change waits and reports again.
    pub fn tick_level_load(
        camera: glam::DVec3,
        connection: &ConnectionHandle,
        game: &mut GameState,
    ) {
        // Taken for the tick so the readiness inputs can borrow `game`; put
        // back below unless the level is ready, which is vanilla clearing
        // `levelLoadTracker`.
        let Some(mut tracker) = game.level_load.take() else {
            return;
        };
        let now = Instant::now();

        let min_y = game.chunk_store.min_y();
        let max_y = min_y + game.chunk_store.height() as i32 - 1;
        let outside_build_height = |y: i32| y < min_y || y > max_y;
        // Vanilla reads `gameRenderer.mainCamera().blockPosition()`.
        let camera_block = camera.floor().as_ivec3();

        tracker.tick_client_load(
            now,
            &ReadyInputs {
                // 26.3's `isLevelReady` checks only the camera's height.
                player_outside_build_height: crate::version::session_protocol() < 777
                    && outside_build_height(game.player.position.y.floor() as i32),
                camera_outside_build_height: outside_build_height(camera_block.y),
                spectator: crate::player::is_spectator(game.player.game_mode),
                alive: !game.dead,
                // Until the server's first position the camera sits at the
                // origin, whose section says nothing about where we spawn.
                player_section_ready: game.position_set && game.camera_section_ready(camera_block),
            },
        );

        if !tracker.is_level_ready(now) {
            game.level_load = Some(tracker);
            return;
        }

        game.client_loaded = true;
        connection
            .packet_tx
            .send(ServerboundGamePacket::PlayerLoaded(
                azalea_protocol::packets::game::s_player_loaded::ServerboundPlayerLoaded,
            ));
    }

    pub fn send_pong(connection: &ConnectionHandle, id: u32) {
        connection.packet_tx.send(ServerboundGamePacket::Pong(
            azalea_protocol::packets::game::s_pong::ServerboundPong { id },
        ));
    }

    /// Marks the end of the client tick (1.21.2+). Must be the last packet of
    /// the tick: servers and anti-cheat batch our movement between these to
    /// tick-align it, so omitting it makes them reject/rubber-band movement.
    /// Vanilla `Minecraft.tick` sends it for every tick a level exists,
    /// including the ones spent on the loading screen.
    pub fn send_client_tick_end(connection: &ConnectionHandle) {
        connection
            .packet_tx
            .send(ServerboundGamePacket::ClientTickEnd(
                s_client_tick_end::ServerboundClientTickEnd,
            ));
    }

    pub fn tick_physics(
        &mut self,
        renderer: &mut Renderer,
        connection: &ConnectionHandle,
        game: &mut GameState,
    ) {
        // `handleLogin` adds the local player before any remote entity, so
        // `ClientLevel.tickEntities` ticks it first and pushes land in next
        // tick's travel.
        // TODO: a same-dimension `handleRespawn` re-adds it after them.
        self.tick_local_player(renderer, connection, game);
        // `TickRateManager.isEntityFrozen`: in a frozen game only players
        // tick. Remote players are held with the rest here.
        if game.tick_rate.runs_normally() {
            self.tick_remote_entities(game);
        }
        // `FireworkParticles.Starter.tick`: a twinkling firework's last
        // sound, the far one from sixteen blocks off.
        let camera = renderer.camera_render_position();
        super::entity_status::tick(game, camera);
        game.block_breaking.tick(game.tick_count);
        game.world_border.tick();
        let audio = &self.audio;
        game.firework_twinkles.retain_mut(|(at, ticks)| {
            *ticks = ticks.saturating_sub(1);
            if *ticks > 0 {
                return true;
            }
            let sound = if camera.distance_squared(*at) >= 256.0 {
                "entity.firework_rocket.twinkle_far"
            } else {
                "entity.firework_rocket.twinkle"
            };
            audio.play_world_sound(
                &crate::audio::SoundRef::event(sound),
                crate::audio::CATEGORY_AMBIENT,
                Position::new(at.x, at.y, at.z),
                20.0,
                0.9 + fastrand::f32() * 0.15,
                fastrand::u64(..),
            );
            false
        });
        super::sniffer::tick(&mut self.audio, game, renderer.camera_render_position());
        super::copper_golem::tick(&self.audio, game);
        panda_eating_particles(game, renderer.camera_render_position());
    }

    /// `LivingEntity.pushEntities` from every remote living entity's tick,
    /// filtered by `EntitySelector.pushableBy` with the local player as target.
    fn tick_remote_entities(&self, game: &mut GameState) {
        let push_box = local_push_box(game);
        let local_name = self.user.username.as_str();
        let vehicle = game.riding_vehicle_id;
        let (scoreboard, tab_list) = (&game.scoreboard, &game.tab_list);
        let pusher_allowed = |id: i32, entity: &crate::entity::LivingEntity| {
            // `isPassengerOfSameVehicle`. TODO: co-passengers.
            if Some(id) == vehicle {
                return false;
            }
            let pusher_name = match entity.player_uuid {
                Some(uuid) => match tab_list.players.get(&uuid) {
                    // `Player.tick` sets `noPhysics` for spectators, so
                    // `Entity.push` returns before any impulse.
                    Some(info) if crate::player::is_spectator(info.game_mode) => return false,
                    Some(info) => info.name.clone(),
                    // TODO: keep the spawn-time profile name; a player
                    // dropped from the tab list falls back to no team.
                    None => return true,
                },
                None => entity.uuid.to_string(),
            };
            scoreboard.pushable_by(&pusher_name, local_name)
        };
        // Vanilla `Level.isThundering`.
        game.entity_store.thundering =
            game.environment.has_weather && f64::from(game.sky_state.thunder()) > 0.9;
        let driven = game
            .controlled_vehicle_id
            .filter(|&id| game.drives_vehicle(id));
        *game.player.velocity += game.entity_store.tick_living(
            &game.chunk_store,
            game.block_registry.block_state_table(),
            game.player.position,
            push_box,
            pusher_allowed,
            game.server_simulation_distance,
            driven,
        );
        let player = (
            game.player.entity_id,
            game.player.position,
            game.player.eye_height,
        );
        let (displays, positions) = (&game.display_store.displays, &game.entity_positions);
        let vehicles = &game.model_entities;
        game.entity_store
            .position_riders(|vehicle, rider, living| match displays.get(&vehicle) {
                Some(display) => Some(display.position),
                None => {
                    let animal = living
                        .get(&rider)
                        .is_some_and(|e| crate::entity::is_animal(e.entity_type));
                    vehicles.seat(vehicle, rider, animal).or_else(|| {
                        super::phases::in_game::display_seat(vehicle, living, positions, player)
                    })
                }
            });
    }

    fn tick_local_player(
        &mut self,
        renderer: &mut Renderer,
        connection: &ConnectionHandle,
        game: &mut GameState,
    ) {
        if game.death_screen_open {
            if game.death_confirm {
                game.death_confirm_ticks = game.death_confirm_ticks.saturating_add(1);
            } else {
                game.death_screen_ticks = game.death_screen_ticks.saturating_add(1);
            }
        }

        // Vanilla ticks the camera FOV interpolation even while dead.
        renderer.set_base_fov(self.menu.fov as f32);
        let fov_effect_scale = self.menu.fov_effect();
        // A bow's draw, in whole ticks; a spyglass only in first person.
        let bow_ticks = (game.interaction.using_kind()
            == Some(azalea_registry::builtin::ItemKind::Bow))
        .then(|| game.interaction.ticks_using(0.0));
        let scoping = game.interaction.is_scoping()
            && crate::app::phases::in_game::renders_first_person_hand(game);
        renderer.update_fov_mod(compute_fov_modifier(
            &game.player,
            bow_ticks,
            scoping,
            fov_effect_scale,
        ));
        // `GameRenderer.getFov`: either fluid at the camera narrows it.
        renderer.set_fluid_fov_factor(if game.player.eyes_in_water || game.player.eyes_in_lava {
            1.0_f32.lerp(0.857_142_87, fov_effect_scale)
        } else {
            1.0
        });

        // Vanilla `LocalPlayer.tick` returns immediately until the client has
        // loaded: no physics, no interaction, and no input, sprint or movement
        // packets while the level is still coming in.
        if !game.client_loaded {
            self.input.clear_click_counts();
            self.input.clear_just_pressed_actions();
            return;
        }

        // LocalPlayer.tickDeath removes the client player at tick 20; from then
        // on ClientLevel.tickEntities skips it entirely.
        if game.dead && game.player.death_animation_finished() {
            self.input.clear_click_counts();
            return;
        }

        // Vanilla ClientLevel snapshots old entity transform before every tick,
        // including dead-player ticks up through the removal tick.
        game.player.snapshot_render_state();
        if game.dead {
            game.player.tick_death();
            let removed_this_tick = game.player.death_animation_finished();
            let hand_stacks = game
                .player
                .inventory
                .hand_stacks(self.input.selected_slot())
                .map(Option::<&_>::cloned);
            game.interaction.tick_using_item(
                hand_stacks.each_ref().map(Option::as_ref),
                &connection.packet_tx,
                &self.audio,
                game.player.position.into(),
                game.player.eye_pos().into(),
                game.player.look_dir,
                &mut crate::player::interaction::BreakEffects {
                    particles: &mut game.particle_store,
                    registry: game.block_registry.render_view(),
                    biome_climate: &game.biome_climate,
                },
            );

            // LocalPlayer.tickDeath marks the player removed at exactly tick 20.
            // LivingEntity.tick then skips aiStep for that removal tick, while
            // LocalPlayer.tick still executes its post-super player state and
            // input/position packet tail once.
            if !removed_this_tick {
                movement::tick_dead(
                    &mut game.player,
                    crate::physics::collision::CollisionWorld::with_block_entities(
                        &game.chunk_store,
                        game.block_registry.block_state_table(),
                        &game.block_entity_anim,
                    ),
                    game.block_registry.tags(),
                    game.riding_vehicle_id.is_some(),
                );
                crate::entity::stop_walk_animation(
                    &mut game.player_walk_pos,
                    &mut game.player_walk_speed,
                    &mut game.player_prev_walk_speed,
                );
                let dx = game.player.position.x - game.player.prev_position.x;
                let dz = game.player.position.z - game.player.prev_position.z;
                game.player.tick_bob(dx, dz, true);
            }
            game.interaction
                .tick_dead_player_state(hand_stacks[0].as_ref(), !removed_this_tick);

            let neutral = InputState::released();
            Self::send_abilities_packet(connection, game);
            Self::send_input_packet(&neutral, connection, game);
            self.send_fall_flying_command(connection, game);
            self.send_sprint_command(connection, game);
            self.send_position_packet(connection, game);

            // Q/F presses queued while dead must not fire on respawn.
            self.input.clear_click_counts();
            return;
        }

        // Open menus only release the keys; the simulation keeps ticking. The
        // chunk-load benchmark also freezes the player so every run measures the
        // same fixed origin.
        let input_live = game.input_live() && game.chunk_load_bench.is_none();

        // Vanilla Minecraft.handleKeybinds: the spectator-effect toggle,
        // offhand-swap then drop consume the queued key presses each tick;
        // spectators consume the last two without acting.
        if input_live {
            let spectator = game.player.game_mode == 3;
            while self
                .input
                .consume_click(Action::ToggleSpectatorShaderEffects)
            {
                game.spectator_effect_active = !game.spectator_effect_active;
            }
            while self.input.consume_click(Action::SwapOffhand) {
                if !spectator {
                    crate::player::interaction::send_swap_offhand(&connection.packet_tx);
                }
            }
            while self.input.consume_click(Action::DropItem) {
                let whole_stack = self.input.ctrl_held();
                if !spectator {
                    // Vanilla `LocalPlayer.drop` always sends the action packet;
                    // only the swing is gated on something actually dropping.
                    crate::player::interaction::send_drop(&connection.packet_tx, whole_stack);
                    if game
                        .player
                        .inventory
                        .remove_from_selected(self.input.selected_slot(), whole_stack)
                    {
                        crate::player::interaction::send_use_swing(
                            &connection.packet_tx,
                            crate::player::interaction::InteractionHand::MainHand,
                        );
                    }
                }
            }
            for slot in self.input.take_spectator_slot_presses() {
                if spectator {
                    game.spectator.on_hotbar_selected(
                        slot,
                        &game.tab_list,
                        &game.scoreboard,
                        &connection.packet_tx,
                    );
                }
            }
            while self.input.consume_click(Action::SpectatorHotbar) {
                if spectator {
                    game.spectator.on_hotbar_action_key(
                        &game.tab_list,
                        &game.scoreboard,
                        &connection.packet_tx,
                    );
                }
            }
        }
        // A press queued while a menu or chat was open must not fire later.
        self.input.clear_click_counts();

        let neutral = InputState::released();
        let input = if input_live { &self.input } else { &neutral };
        // The hotbar slot outlives any screen: the neutral input stands in
        // for keys and clicks only, so a screen or chat opening doesn't
        // carry slot 0 to the server (and swap the held item's attributes).
        let selected_slot = self.input.selected_slot();

        // Vanilla LocalPlayer.aiStep ride-jump charge. `was_jump_pressed`
        // still holds last tick's key here (movement::tick overwrites it
        // below), matching vanilla's wasJumping sampled before input.tick().
        // Equine jump cooldown is always 0, so the cooldown gate collapses
        // into the vehicle check.
        let jump_held = input.performing_action(Action::Jump);
        if game.riding_jumpable_vehicle() {
            let p = &mut game.player;
            if p.jump_riding_ticks < 0 {
                p.jump_riding_ticks += 1;
                if p.jump_riding_ticks == 0 {
                    p.jump_riding_scale = 0.0;
                }
            }
            if p.was_jump_pressed && !jump_held {
                p.jump_riding_ticks = -10;
                // `AbstractHorse.onPlayerJump`: the horse jumps from the
                // client's own physics next time it's on the ground.
                game.ridden_jump_pending = crate::physics::ridden::jump_pending_scale(
                    (p.jump_riding_scale * 100.0).floor() as i32,
                );
                use azalea_protocol::packets::game::s_player_command as cmd;
                connection
                    .packet_tx
                    .send(ServerboundGamePacket::PlayerCommand(
                        cmd::ServerboundPlayerCommand {
                            id: azalea_core::entity_id::MinecraftEntityId(p.entity_id),
                            action: cmd::Action::StartRidingJump,
                            data: (p.jump_riding_scale * 100.0).floor() as u32,
                        },
                    ));
            } else if !p.was_jump_pressed && jump_held {
                p.jump_riding_ticks = 0;
                p.jump_riding_scale = 0.0;
            } else if p.was_jump_pressed {
                p.jump_riding_ticks += 1;
                p.jump_riding_scale = if p.jump_riding_ticks < 10 {
                    p.jump_riding_ticks as f32 * 0.1
                } else {
                    0.8 + 2.0 / (p.jump_riding_ticks - 9) as f32 * 0.1
                };
            }
        } else {
            // Vanilla keeps jumpRidingTicks; only the scale resets.
            game.player.jump_riding_scale = 0.0;
        }

        // What was left of the turn a rowed boat gave its rower last tick.
        let (turn, turned) = std::mem::take(&mut game.rider_turn);
        if turn != turned {
            renderer.turn_camera(turn - turned);
        }
        let camera_look = renderer.camera_look_dir();
        game.player.look_dir = if game.player.is_sleeping() {
            // `LivingEntity.tick` forces xRot to 0 while sleeping.
            LookDirection::new(camera_look.y_rot_deg(), 0.0)
        } else {
            camera_look
        };

        // Vanilla `Minecraft.tick`: pick and keybinds run before the entity
        // tick, so interaction packets precede the movement packet.
        let hand_stacks = game
            .player
            .inventory
            .hand_stacks(selected_slot)
            .map(Option::<&_>::cloned);
        let hand_stacks = hand_stacks.each_ref().map(Option::as_ref);
        let held_stack = hand_stacks[0];
        game.interaction.on_cooldown =
            hand_stacks.map(|stack| stack.is_some_and(|s| game.player.cooldowns.is_on_cooldown(s)));
        let creative = crate::player::is_creative(game.player.game_mode);
        let held_item = held_stack
            .as_ref()
            .map(|data| crate::player::inventory::item_resource_name(data.kind));
        game.interaction.update_target(
            &game.block_registry,
            game.player.eye_pos(),
            game.player.look_dir,
            &game.chunk_store,
            &game.entity_store,
            &pickables(game, held_stack.is_some()),
            &game.player.attributes,
            creative,
            held_item.as_deref(),
        );
        let legacy_mining = crate::tool::legacy_mining(
            held_stack,
            game.player
                .inventory
                .armor_slots()
                .iter()
                .filter_map(|stack| stack.as_present()),
            game.block_registry.protocol(),
        );
        let hands = hand_stacks.map(|stack| crate::player::interaction::HandItem {
            stack,
            place_block: stack.and_then(|s| {
                game.block_registry
                    .placeable_block_for_item(&crate::player::inventory::item_resource_name(s.kind))
            }),
            worn: worn_for(
                stack,
                &game.player.inventory,
                selected_slot,
                creative,
                &game.registries,
            ),
        });
        let player_aabb = game.player.bounding_box();
        let mut effects = crate::player::interaction::BreakEffects {
            particles: &mut game.particle_store,
            registry: game.block_registry.render_view(),
            biome_climate: &game.biome_climate,
        };
        let dirty = game.interaction.tick_actions(
            &game.block_registry,
            input,
            &game.chunk_store,
            &connection.packet_tx,
            &self.audio,
            game.player.position.into(),
            player_aabb,
            game.player.eye_pos().into(),
            game.player.look_dir,
            game.player.on_ground,
            creative,
            game.player.food,
            selected_slot,
            hands,
            &game.player.attributes,
            &game.player.effects,
            game.player.eyes_in_water,
            legacy_mining,
            &mut effects,
        );
        game.interaction.tick_using_item(
            hand_stacks,
            &connection.packet_tx,
            &self.audio,
            game.player.position.into(),
            game.player.eye_pos().into(),
            game.player.look_dir,
            &mut effects,
        );

        if game.chunk_load_bench.is_some() {
            game.player.velocity = crate::entity::components::Velocity::new(0.0, 0.0, 0.0);
        }
        let vehicle = game.riding_vehicle_id.map(|_| movement::Vehicle {
            jumpable: game.riding_jumpable_vehicle(),
        });
        movement::tick(
            &mut game.player,
            input,
            crate::physics::collision::CollisionWorld {
                border: Some(&game.world_border),
                ..crate::physics::collision::CollisionWorld::with_block_entities(
                    &game.chunk_store,
                    game.block_registry.block_state_table(),
                    &game.block_entity_anim,
                )
            },
            game.block_registry.tags(),
            game.interaction.use_speed_multiplier(),
            game.interaction.slow_due_to_using_item(),
            vehicle,
        );
        // A saddled horse under the local player moves by this client's
        // physics (vanilla `travelRidden`), and the server hears where.
        if let Some(id) = game.controlled_vehicle_id
            && game.drives_vehicle(id)
            && let Some(horse) = game.entity_store.living.get_mut(&id)
        {
            crate::physics::ridden::tick(
                horse,
                game.player.look_dir,
                game.player.move_forward,
                game.player.move_strafe,
                &mut game.ridden_jump_pending,
                crate::physics::collision::CollisionWorld::new(
                    &game.chunk_store,
                    game.block_registry.block_state_table(),
                ),
            );
            connection
                .packet_tx
                .send_raw(pomme_protocol::wire::encode_move_vehicle(
                    *horse.position,
                    horse.look_dir.y_rot_deg(),
                    horse.look_dir.x_rot_deg(),
                    horse.on_ground,
                ));
        }
        // A boat the local player rows moves by this client's physics too
        // (`AbstractBoat.tick`), and the server hears where and which
        // paddles are in the water.
        let rowed = game.rowed_boat();
        game.model_entities.driven = rowed;
        // `LocalPlayer.rideTick`: hands on the oars are busy.
        game.interaction.hands_busy =
            rowed.is_some() && (game.player.move_strafe != 0.0 || game.player.move_forward != 0.0);
        if game.boat_drive.vehicle != rowed {
            game.boat_drive = rowed
                .map(crate::physics::boat::Drive::of)
                .unwrap_or_default();
        }
        // What a boat can't pass through (`AbstractBoat.canVehicleCollide`):
        // other vehicles and whatever can be pushed, its own riders aside.
        let obstacles: Vec<crate::physics::aabb::Aabb> =
            match rowed.and_then(|id| Some((id, game.model_entities.entities.get(&id)?))) {
                Some((id, boat)) => {
                    let near = |aabb: &crate::physics::aabb::Aabb| {
                        aabb.min.distance_squared(*boat.position) < 64.0
                    };
                    let riders = game.model_entities.passengers(id);
                    let mobs = game
                        .entity_store
                        .living
                        .iter()
                        .filter(|(mob, entity)| {
                            !riders.contains(mob)
                                && !entity.is_armor_stand_marker()
                                && entity.entity_type != azalea_registry::builtin::EntityKind::Bat
                        })
                        .map(|(_, entity)| {
                            crate::entity::living_entity_dimensions(entity)
                                .make_bounding_box(*entity.position)
                        });
                    let vehicles = game
                        .model_entities
                        .entities
                        .iter()
                        .filter(|(other, _)| **other != id)
                        .filter_map(|(_, vehicle)| Some(vehicle.pick_box()?.0));
                    mobs.chain(vehicles).filter(near).collect()
                }
                None => Vec::new(),
            };
        if let Some(id) = rowed
            && let Some(entity) = game.model_entities.entities.get_mut(&id)
        {
            use crate::physics::boat;
            let mut body = boat::Boat {
                position: *entity.position,
                velocity: entity.velocity,
                y_rot: entity.y_rot,
            };
            // `LocalPlayer.rideTick`: the keys held, for the boat's next tick.
            let keys = boat::Keys {
                left: game.player.move_strafe > 0.0,
                right: game.player.move_strafe < 0.0,
                up: game.player.move_forward > 0.0,
                down: game.player.move_forward < 0.0,
            };
            let paddles = boat::tick(
                &mut body,
                &mut game.boat_drive,
                keys,
                crate::physics::collision::CollisionWorld::new(
                    &game.chunk_store,
                    game.block_registry.block_state_table(),
                ),
                &obstacles,
            );
            entity.rowed(body, paddles);
            // `AbstractBoat.positionRider`: the rower turns with the boat,
            // and looks no further than 105 degrees off its bow. The view
            // makes the turn over the frames to the next tick, and the
            // server hears of it then, as vanilla's does after `sendPosition`.
            let turned = game.player.look_dir.y_rot_deg() + game.boat_drive.delta_rotation;
            let back = boat::rider_turn(body.y_rot, turned);
            game.rider_turn = (game.boat_drive.delta_rotation + back, 0.0);
            connection
                .packet_tx
                .send_raw(pomme_protocol::wire::encode_paddle_boat(
                    paddles[0], paddles[1],
                ));
            connection
                .packet_tx
                .send_raw(pomme_protocol::wire::encode_move_vehicle(
                    body.position,
                    body.y_rot,
                    entity.x_rot,
                    game.boat_drive.on_ground,
                ));
        }
        // `Entity.rideTick`: a passenger moves with its vehicle, not itself.
        if let Some(seat) = game.riding_seat() {
            game.player.position = seat.into();
            game.player.velocity = crate::entity::components::Velocity::new(0.0, 0.0, 0.0);
        }
        let dx = game.player.position.x - game.player.prev_position.x;
        let dz = game.player.position.z - game.player.prev_position.z;
        game.player_prev_body_y_rot = game.player_body_y_rot;
        let rowed_y_rot = rowed
            .and_then(|id| game.model_entities.entities.get(&id))
            .map(|boat| boat.y_rot);
        game.player_body_y_rot = match game
            .riding_vehicle()
            .map(|v| v.body_y_rot_deg)
            .or(rowed_y_rot)
        {
            // `AbstractHorse.positionRider` (and other living vehicles):
            // the rider's body faces the way its mount does.
            Some(vehicle_body) => vehicle_body,
            None => crate::player::body_turn(
                game.player_body_y_rot,
                game.player.look_dir.y_rot_deg(),
                dx,
                dz,
                game.interaction.attack_anim() > 0.0,
            ),
        };
        crate::entity::update_walk_animation(
            dx,
            0.0,
            dz,
            &mut game.player_walk_pos,
            &mut game.player_walk_speed,
            &mut game.player_prev_walk_speed,
        );
        game.player.tick_bob(dx, dz, false);
        game.player.drop_spam.tick();

        game.interaction
            .tick_player_state(input.is_cursor_captured(), held_stack);

        Self::send_abilities_packet(connection, game);
        Self::send_input_packet(input, connection, game);
        self.send_fall_flying_command(connection, game);
        self.send_sprint_command(connection, game);
        self.send_position_packet(connection, game);
        if !dirty.is_empty() {
            let min_y = game.chunk_store.min_y();
            let n = game.chunk_store.section_count();
            let mut sections: Vec<(crate::position::ChunkPos, i32)> = Vec::new();
            for b in dirty {
                // Vanilla relights in this frame's ClientLevel.update; the
                // light worker lands it a frame or so later.
                game.light_engine.on_block_dirty(b.x, b.y, b.z);
                dirty_sections_for_block(&mut sections, b.x, b.y, b.z, min_y, n);
            }
            // One span per column so each column builds its mesh snapshot
            // once (the sections of an edit are contiguous per column).
            let mut spans: Vec<(crate::position::ChunkPos, i32, i32)> = Vec::new();
            for (col, si) in sections {
                match spans.iter_mut().find(|(c, ..)| *c == col) {
                    Some((_, lo, hi)) => {
                        *lo = (*lo).min(si);
                        *hi = (*hi).max(si);
                    }
                    None => spans.push((col, si, si)),
                }
            }
            for (col, lo, hi) in spans {
                game.mesh_sections_edit_now(renderer, col, lo..hi + 1);
            }
        }

        // Menus consume their own clicks later in the frame, so only clear
        // them when the simulation saw the live input.
        if input_live {
            self.input.clear_just_pressed_actions();
        }
    }

    // Vanilla onUpdateAbilities: report a locally toggled `flying` to the
    // server, which gates it on mayfly and corrects us via the clientbound
    // abilities packet if rejected.
    fn send_abilities_packet(connection: &ConnectionHandle, game: &mut GameState) {
        if game.player.abilities_dirty {
            connection
                .packet_tx
                .send(ServerboundGamePacket::PlayerAbilities(
                azalea_protocol::packets::game::s_player_abilities::ServerboundPlayerAbilities {
                    is_flying: game.player.flying,
                },
            ));
            game.player.abilities_dirty = false;
        }
    }

    fn send_input_packet(input: &InputState, connection: &ConnectionHandle, game: &mut GameState) {
        let sender = &connection.packet_tx;

        let analog_move = input.analog_movement_axes().unwrap_or(glam::Vec2::ZERO);

        let current =
            player_input_state(input, analog_move, input.performing_action(Action::Sprint));

        if current != game.last_sent_input {
            sender.send(ServerboundGamePacket::PlayerInput(
                serverbound_player_input(&current),
            ));
            game.last_sent_input = current;
        }
    }

    /// `ServerboundPlayerCommandPacket(player, action)`.
    fn send_player_command(
        &self,
        connection: &ConnectionHandle,
        game: &GameState,
        action: azalea_protocol::packets::game::s_player_command::Action,
    ) {
        connection
            .packet_tx
            .send(ServerboundGamePacket::PlayerCommand(
                azalea_protocol::packets::game::s_player_command::ServerboundPlayerCommand {
                    id: azalea_core::entity_id::MinecraftEntityId(game.player.entity_id),
                    action,
                    data: 0,
                },
            ));
    }

    /// `LocalPlayer.aiStep`: an elytra opened this tick, as the server is
    /// told (`START_FALL_FLYING`).
    pub fn send_fall_flying_command(&self, connection: &ConnectionHandle, game: &mut GameState) {
        if std::mem::take(&mut game.player.start_fall_flying) {
            self.send_player_command(
                connection,
                game,
                azalea_protocol::packets::game::s_player_command::Action::StartFallFlying,
            );
        }
    }

    pub fn send_sprint_command(&self, connection: &ConnectionHandle, game: &mut GameState) {
        let sprinting = game.player.sprinting;
        if sprinting != game.was_sprinting {
            let action = if sprinting {
                azalea_protocol::packets::game::s_player_command::Action::StartSprinting
            } else {
                azalea_protocol::packets::game::s_player_command::Action::StopSprinting
            };
            self.send_player_command(connection, game, action);
            game.was_sprinting = sprinting;
        }
    }

    /// Vanilla InBedChatScreen: leaving bed sends PlayerCommand STOP_SLEEPING.
    pub fn send_stop_sleeping(&self, connection: &ConnectionHandle, game: &GameState) {
        self.send_player_command(
            connection,
            game,
            azalea_protocol::packets::game::s_player_command::Action::StopSleeping,
        );
    }

    pub fn send_position_packet(&self, connection: &ConnectionHandle, game: &mut GameState) {
        let sender = &connection.packet_tx;
        use azalea_protocol::common::movements::MoveFlags;
        use azalea_protocol::packets::game::*;

        let pos = game.player.position;
        let look_dir = game.player.look_dir;

        let dx = pos.x - game.last_sent_pos.x;
        let dy = pos.y - game.last_sent_pos.y;
        let dz = pos.z - game.last_sent_pos.z;
        game.position_send_counter += 1;
        let pos_changed = dx * dx + dy * dy + dz * dz > POSITION_THRESHOLD_SQ
            || game.position_send_counter >= POSITION_SEND_INTERVAL;
        let rot_changed = (look_dir.y_rot_deg() - game.last_sent_look_dir.y_rot_deg()) != 0.0
            || (look_dir.x_rot_deg() - game.last_sent_look_dir.x_rot_deg()) != 0.0;

        let flags = MoveFlags {
            on_ground: game.player.on_ground,
            horizontal_collision: game.player.horizontal_collision,
        };

        if pos_changed && rot_changed {
            sender.send(ServerboundGamePacket::MovePlayerPosRot(
                ServerboundMovePlayerPosRot {
                    pos: pos.into(),
                    look_direction: look_dir.into(),
                    flags,
                },
            ));
        } else if pos_changed {
            sender.send(ServerboundGamePacket::MovePlayerPos(
                ServerboundMovePlayerPos {
                    pos: pos.into(),
                    flags,
                },
            ));
        } else if rot_changed {
            sender.send(ServerboundGamePacket::MovePlayerRot(
                ServerboundMovePlayerRot {
                    look_direction: look_dir.into(),
                    flags,
                },
            ));
        } else if game.player.on_ground != game.last_sent_on_ground
            || game.player.horizontal_collision != game.last_sent_horizontal_collision
        {
            sender.send(ServerboundGamePacket::MovePlayerStatusOnly(
                ServerboundMovePlayerStatusOnly { flags },
            ));
        }

        if pos_changed {
            game.last_sent_pos = pos;
            game.position_send_counter = 0;
        }
        if rot_changed {
            game.last_sent_look_dir = look_dir;
        }
        game.last_sent_on_ground = game.player.on_ground;
        game.last_sent_horizontal_collision = game.player.horizontal_collision;
    }
}

/// Where a dead local player goes: the death screen, or straight to a respawn
/// when the server disabled the screen (`showDeathScreen`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeathRoute {
    ShowDeathScreen,
    Respawn,
}

pub(crate) fn death_route(show_death_screen: bool) -> DeathRoute {
    if show_death_screen {
        DeathRoute::ShowDeathScreen
    } else {
        DeathRoute::Respawn
    }
}

/// Vanilla `ChatListener.evaluateTrustLevel` and `ChatTrustLevel.createTag`.
/// Only the integrated server's own player skips evaluation.
pub(crate) fn accepted_player_chat_tag(
    local_sender: bool,
    signed: bool,
    body: &crate::net::chat_security::SignedChatBody,
    now_ms: u64,
    only_secure: bool,
    bindings: &crate::app::input::KeyBindings,
) -> Option<crate::ui::chat::ChatMessageTag> {
    if local_sender {
        return None;
    }
    let expired = now_ms > body.timestamp_ms.max(0) as u64 + 7 * 60 * 1000;
    if !signed || expired {
        return Some(crate::ui::chat::ChatMessageTag::NotSecure);
    }
    let decorated = if only_secure {
        &body.decorated_signed_content
    } else {
        &body.decorated_content
    };
    let modified = body.modified_style
        || !decorated
            .plain_text_with_bindings(bindings)
            .contains(&body.content);
    modified.then(|| crate::ui::chat::ChatMessageTag::Modified {
        original: body.content.clone(),
    })
}

fn sync_respawn_attributes(player: &mut LocalPlayer, keep_attribute_modifiers: bool) {
    if keep_attribute_modifiers {
        // `assignAllValues` dirties every instance.
        sync_player_attribute_mirrors(player, |_| true);
    } else {
        // Vanilla constructs a fresh player attribute map, then copies only
        // old base values into it; only a base that differs from the
        // supplier default dirties its instance.
        player.attributes.clear_modifiers();
        let dirty: Vec<AttributeKind> = AttributeKind::ALL
            .into_iter()
            .filter(|&kind| {
                player
                    .attributes
                    .instance(kind)
                    .map(AttributeInstance::base_value)
                    != kind.player_base_value()
            })
            .collect();
        sync_player_attribute_mirrors(player, |kind| dirty.contains(&kind));
    }
}

/// Refresh the fields current consumers read from an attribute map. `dirty`
/// gates vanilla `LivingEntity.onAttributeUpdated`'s clamps, which only run
/// for instances that changed.
fn sync_living_attribute_mirrors(
    attributes: &AttributeMap,
    health: &mut f32,
    max_health: &mut f32,
    camera_distance: &mut f32,
    dirty: impl Fn(AttributeKind) -> bool,
) {
    if let Some(value) = attributes.value(AttributeKind::MaxHealth) {
        *max_health = value as f32;
        if dirty(AttributeKind::MaxHealth) {
            *health = health.min(*max_health);
        }
    }
    if let Some(value) = attributes.value(AttributeKind::CameraDistance) {
        *camera_distance = value as f32;
    }
    // TODO: SCALE refreshes dimensions in vanilla once Pomme consumes it.
}

fn sync_player_attribute_mirrors(player: &mut LocalPlayer, dirty: impl Fn(AttributeKind) -> bool) {
    let attributes = &player.attributes;
    sync_living_attribute_mirrors(
        attributes,
        &mut player.health,
        &mut player.max_health,
        &mut player.camera_distance,
        &dirty,
    );
    if let Some(value) = attributes.value(AttributeKind::Armor) {
        player.armor = value.floor() as u32;
    }
    if dirty(AttributeKind::MaxAbsorption)
        && let Some(value) = attributes.value(AttributeKind::MaxAbsorption)
    {
        player.absorption = player.absorption.min(value as f32);
    }
}

/// New `server_render_distance` for a server view-distance announcement, or
/// `None` to keep the current one. Some servers announce min(our request,
/// server max); an echo of our own request carries no cap information and
/// would ratchet the render distance slider down, so only a differing value
/// counts. It can't be an echo above the request: any such value is the
/// server's actual view distance, including later reductions.
fn server_view_distance_update(announced: u32, last_request: u32) -> Option<u32> {
    let announced = announced.min(crate::world::chunk::MAX_VIEW_DISTANCE);
    (announced != last_request).then_some(announced)
}

/// LOD level for a column: full detail within `detail` chunks (the Chunk
/// Detail setting), half resolution to twice that, quarter beyond. Off keeps
/// every column at full detail.
pub(crate) fn chunk_lod(
    pos: crate::position::ChunkPos,
    player: crate::position::ChunkPos,
    detail: u32,
) -> u32 {
    if detail >= crate::ui::menu::CHUNK_DETAIL_OFF {
        return 0;
    }
    let dx = (pos.x - player.x).unsigned_abs();
    let dz = (pos.z - player.z).unsigned_abs();
    let dist = dx.max(dz);
    if dist <= detail {
        0
    } else if dist <= detail * 2 {
        1
    } else {
        2
    }
}

/// The [`Worn`](crate::player::interaction::Worn) for `stack`, if it's a
/// swappable equippable.
fn worn_for<'a>(
    stack: Option<&azalea_inventory::ItemStackData>,
    inventory: &'a crate::player::inventory::Inventory,
    selected: u8,
    creative: bool,
    registries: &azalea_core::registry_holder::RegistryHolder,
) -> crate::player::interaction::Worn<'a> {
    use azalea_core::registry_holder::components::PreventArmorChange;
    use azalea_inventory::components::{Enchantments, Equippable};
    use azalea_registry::DataRegistry;

    let Some(equippable) = stack
        .and_then(|s| s.get_component::<Equippable>())
        .filter(|e| e.swappable)
    else {
        return Default::default();
    };
    let worn = inventory.equipped(equippable.slot, selected);
    let locked = !creative
        && worn
            .and_then(|w| w.get_component::<Enchantments>())
            .is_some_and(|e| {
                e.levels.keys().any(|enchantment| {
                    registries
                        .enchantment
                        .map
                        .get_index(enchantment.protocol_id() as usize)
                        .is_some_and(|(_, data)| data.get::<PreventArmorChange>().is_some())
                })
            });
    crate::player::interaction::Worn {
        stack: worn,
        locked,
    }
}

/// Vanilla `AbstractClientPlayer.getFieldOfViewModifier`. `effect_scale` is the
/// `fovEffectScale` accessibility value (1.0 = full effect).
///
/// The view widens with the movement speed attribute over the walking speed
/// (sprinting, Speed and Slowness, gear), narrows as a bow is drawn
/// (`bow_ticks` in use), and a spyglass (`scoping`) takes it to a tenth
/// whatever the scale.
fn compute_fov_modifier(
    player: &LocalPlayer,
    bow_ticks: Option<f32>,
    scoping: bool,
    effect_scale: f32,
) -> f32 {
    let mut modifier = 1.0;
    if player.flying {
        modifier *= 1.1;
    }
    if player.walk_speed != 0.0 {
        let speed_factor = crate::physics::movement::movement_speed(player) / player.walk_speed;
        modifier *= (speed_factor + 1.0) / 2.0;
    }
    if let Some(ticks) = bow_ticks {
        let drawn = (ticks / 20.0).min(1.0);
        modifier *= 1.0 - drawn * drawn * 0.15;
    } else if scoping {
        return 0.1;
    }
    1.0_f32.lerp(modifier, effect_scale)
}

/// How a block or item particle looks: the block's terrain sprite and tint at
/// `pos`, or the item's flat sprite. `ParticleLook::None` for other options,
/// and for a state or item with no sprite.
pub(crate) fn particle_look(
    game: &GameState,
    options: crate::particle::ParticleOptions,
    pos: glam::DVec3,
) -> crate::particle::ParticleLook {
    use crate::particle::{ParticleLook, ParticleOptions};
    match options {
        ParticleOptions::Block(state) => {
            let Some(state) = game.block_registry.block_state_table().try_state(state) else {
                return ParticleLook::None;
            };
            game.particle_store
                .terrain_appearance(
                    crate::position::BlockPos::containing(pos),
                    state,
                    game.block_registry.render_view(),
                    &game.chunk_store,
                    &game.biome_climate,
                )
                .map_or(ParticleLook::None, |(region, color)| {
                    ParticleLook::Terrain(region, color)
                })
        }
        ParticleOptions::Item(item) => pomme_protocol::RegistryTable::native()
            .name_of(pomme_protocol::ClientRegistry::Item, item)
            .and_then(|name| {
                let name = name.strip_prefix("minecraft:").unwrap_or(name);
                game.particle_store.item_region(&format!("item/{name}"))
            })
            .map_or(ParticleLook::None, |region| {
                ParticleLook::Terrain(region, [1.0; 3])
            }),
        _ => ParticleLook::None,
    }
}

/// The stars a rocket's item carries (its `fireworks` component).
fn firework_explosions(
    stack: Option<&azalea_inventory::ItemStackData>,
) -> Vec<crate::particle::FireworkExplosion> {
    use azalea_inventory::components::{FireworkExplosionShape, Fireworks};

    use crate::particle::{FireworkExplosion, FireworkShape};
    let Some(fireworks) = stack.and_then(|stack| stack.get_component::<Fireworks>()) else {
        return Vec::new();
    };
    let rgb = |colors: &[i32]| colors.iter().map(|c| *c as u32 & 0xFF_FFFF).collect();
    fireworks
        .explosions
        .iter()
        .map(|explosion| FireworkExplosion {
            shape: match explosion.shape {
                FireworkExplosionShape::SmallBall => FireworkShape::SmallBall,
                FireworkExplosionShape::LargeBall => FireworkShape::LargeBall,
                FireworkExplosionShape::Star => FireworkShape::Star,
                FireworkExplosionShape::Creeper => FireworkShape::Creeper,
                FireworkExplosionShape::Burst => FireworkShape::Burst,
            },
            colors: rgb(&explosion.colors),
            fade_colors: rgb(&explosion.fade_colors),
            trail: explosion.has_trail,
            twinkle: explosion.has_twinkle,
        })
        .collect()
}

/// An end portal or gateway block, which `TheEndPortalRenderer` draws.
fn is_end_portal(
    table: crate::world::block::BlockStateTable,
    state: azalea_block::BlockState,
) -> bool {
    matches!(
        table.block_state_data(state).id(),
        "end_portal" | "end_gateway"
    )
}

/// The non-living entities the crosshair can pick. `hand_holds` is whether
/// the main hand holds anything.
fn pickables(game: &GameState, hand_holds: bool) -> Vec<crate::player::interaction::Pickable> {
    use crate::player::interaction::Pickable;
    let interactions = game.interactions.entities.iter().map(|(&id, e)| Pickable {
        id,
        aabb: e.bounding_box(),
        position: e.position.into(),
        swing_on_use: e.response,
    });
    let crystals = game.end_crystals.crystals.iter().map(|(&id, c)| Pickable {
        id,
        aabb: c.bounding_box(),
        position: c.position.into(),
        swing_on_use: true,
    });
    // `ItemFrame.interact` on the client: a use does something when the
    // frame holds an item (it turns) or the hand does (it goes in).
    let frames = game
        .item_frames
        .frames
        .iter()
        .map(move |(&id, frame)| Pickable {
            id,
            aabb: frame.bounding_box(),
            position: frame.position(),
            swing_on_use: frame.item.is_some() || hand_holds,
        });
    // Boats and minecarts, but for the one the player is in
    // (`ProjectileUtil.getEntityHitResult` passes over its own vehicle).
    let riding = game.riding_vehicle_id;
    let vehicles = game
        .model_entities
        .entities
        .iter()
        .filter(move |(id, _)| Some(**id) != riding)
        .filter_map(|(&id, vehicle)| {
            let (aabb, swing_on_use) = vehicle.pick_box()?;
            Some(Pickable {
                id,
                aabb,
                position: *vehicle.position,
                swing_on_use,
            })
        });
    interactions
        .chain(crystals)
        .chain(frames)
        .chain(vehicles)
        .collect()
}

/// How much of the way to where the loading has got the pack bar's fill
/// goes in `dt` seconds: most of it in a quarter of a second, whatever the
/// frame rate.
fn pack_bar_ease(dt: f32) -> f32 {
    1.0 - (-dt.max(0.0) * 10.0).exp()
}

/// `Font.split` for a toast's message: its lines at `width` GUI pixels.
pub(crate) fn toast_lines(renderer: &Renderer, text: &str, width: i32) -> Vec<String> {
    let spans = [crate::ui::text::TextSpan::new(text.to_owned(), [1.0; 4])];
    renderer
        .layout_display_text(&spans, width)
        .lines
        .into_iter()
        .map(|(line, _)| line.iter().map(|span| span.text.as_str()).collect())
        .collect()
}

#[cfg(test)]
mod tests {
    /// The view is unchanged at a walk, widens with a sprint, narrows by
    /// up to 15% as a bow draws over a second, and a spyglass takes it to
    /// a tenth whatever the accessibility scale says.
    #[test]
    fn the_view_follows_speed_bow_and_spyglass() {
        let mut player = crate::player::LocalPlayer::new();
        let fov = |player: &crate::player::LocalPlayer, bow, scoping, scale| {
            super::compute_fov_modifier(player, bow, scoping, scale)
        };
        assert!((fov(&player, None, false, 1.0) - 1.0).abs() < 1e-6);
        assert!((fov(&player, Some(10.0), false, 1.0) - (1.0 - 0.25 * 0.15)).abs() < 1e-6);
        assert!((fov(&player, Some(40.0), false, 1.0) - 0.85).abs() < 1e-6);
        assert_eq!(fov(&player, None, true, 0.0), 0.1);
        player.sprinting = true;
        assert!((fov(&player, None, false, 1.0) - 1.15).abs() < 1e-5);
        assert!((fov(&player, None, false, 0.0) - 1.0).abs() < 1e-6);
    }

    /// The pack bar's fill moves by time, not by frame: two half-length
    /// frames take it as far as one whole one, and it never overshoots.
    #[test]
    fn the_pack_bar_eases_by_time() {
        let whole = super::pack_bar_ease(0.02);
        let half = super::pack_bar_ease(0.01);
        assert!((1.0 - (1.0 - half) * (1.0 - half) - whole).abs() < 1e-6);
        assert_eq!(super::pack_bar_ease(0.0), 0.0);
        assert!(super::pack_bar_ease(10.0) <= 1.0);
        assert!(super::pack_bar_ease(0.25) > 0.9);
    }

    /// The fullscreen key never needs a second press to change the screen:
    /// a window goes fullscreen, in the kind last used, and either kind of
    /// fullscreen goes back to a window.
    #[test]
    fn the_fullscreen_key_toggles() {
        use super::DisplayMode::{Borderless, Fullscreen, Windowed};
        assert_eq!(Windowed.toggled(Borderless), Borderless);
        assert_eq!(Windowed.toggled(Fullscreen), Fullscreen);
        assert_eq!(Windowed.toggled(Windowed), Borderless);
        for kind in [Borderless, Fullscreen] {
            assert_eq!(Borderless.toggled(kind), Windowed);
            assert_eq!(Fullscreen.toggled(kind), Windowed);
        }
    }

    use super::{
        CursorOp, DeathRoute, HeadProfile, MENU_REPEAT_DELAY, MENU_REPEAT_INTERVAL, RepeatStepper,
        accepted_player_chat_tag, cursor_step, death_route, player_input_state,
        resolve_corrected_look, resolve_head_profile, resolve_player_position_correction,
        server_view_distance_update, serverbound_player_input, sync_living_attribute_mirrors,
        sync_player_attribute_mirrors, sync_respawn_attributes,
    };
    use crate::app::input::{InputState, gamepad_movement_axes};
    use crate::attribute::{
        AttributeKind, AttributeMap, AttributeModifier, AttributeModifierOperation,
        AttributeSnapshot,
    };
    use crate::entity::components::{LookDirection, Position, Velocity};
    use crate::net::chat_security::SignedChatBody;
    use crate::player::tab_list::{PlayerInfoEntry, TabList};
    use crate::player::{LocalPlayer, valid_player_name};
    use crate::ui::chat::ChatMessageTag;

    fn tab_list_with(uuid: uuid::Uuid, name: &str, textures: Option<&str>) -> TabList {
        let mut tab_list = TabList::new();
        tab_list.apply_update(
            &pomme_protocol::player_info::Actions {
                add_player: true,
                ..Default::default()
            },
            &[PlayerInfoEntry {
                uuid,
                name: name.to_owned(),
                textures: textures.map(str::to_owned),
                game_mode: 0,
                listed: true,
                latency: 0,
                display_name: None,
                list_order: 0,
                chat_session: None,
            }],
        );
        tab_list
    }

    #[test]
    fn player_names_follow_the_codec_rules() {
        assert!(valid_player_name("Dinnerbone"));
        assert!(valid_player_name("a_1234567890_bcd"));
        // 17 characters, a space, a control character and a non-ASCII one.
        assert!(!valid_player_name("a_1234567890_bcde"));
        assert!(!valid_player_name("two words"));
        assert!(!valid_player_name("tab	here"));
        assert!(!valid_player_name("héllo"));
    }

    #[test]
    fn profiles_resolve_like_resolvable_profile() {
        let uuid = uuid::Uuid::from_u128(1);
        let tab_list = tab_list_with(uuid, "Alex", Some("packed"));

        // Dynamic: one of name/id and no properties, so the tab list answers
        // first — by name, ignoring case.
        let by_name = HeadProfile {
            id: None,
            name: Some("alex".to_owned()),
            textures: None,
        };
        assert!(!by_name.is_static());
        let resolved = resolve_head_profile(by_name, &tab_list);
        assert_eq!(resolved.id, Some(uuid));
        assert_eq!(resolved.textures.as_deref(), Some("packed"));

        let by_id = HeadProfile {
            id: Some(uuid),
            name: None,
            textures: None,
        };
        assert_eq!(
            resolve_head_profile(by_id, &tab_list).textures.as_deref(),
            Some("packed")
        );

        // A name nobody in the list has keeps its own identity for the
        // Mojang lookup.
        let stranger = HeadProfile {
            id: None,
            name: Some("Notch".to_owned()),
            textures: None,
        };
        assert_eq!(resolve_head_profile(stranger.clone(), &tab_list), stranger);

        // Static: both, neither, or properties present, and never resolved.
        let both = HeadProfile {
            id: Some(uuid),
            name: Some("Alex".to_owned()),
            textures: None,
        };
        assert!(both.is_static());
        assert_eq!(resolve_head_profile(both.clone(), &tab_list), both);
        assert!(
            HeadProfile {
                id: None,
                name: None,
                textures: None,
            }
            .is_static()
        );
        assert!(
            HeadProfile {
                id: None,
                name: Some("Alex".to_owned()),
                textures: Some("packed".to_owned()),
            }
            .is_static()
        );
    }

    /// Total steps from holding `dir` for `seconds` at a fixed frame time.
    fn held_steps(dir: i32, seconds: f32, dt: f32) -> i32 {
        let mut stepper = RepeatStepper::default();
        let mut total = 0;
        let mut elapsed = 0.0;
        while elapsed < seconds {
            total += stepper.step(dir, dt);
            elapsed += dt;
        }
        total
    }

    #[test]
    fn dpad_press_steps_once_then_waits_for_the_repeat_delay() {
        let dt = 1.0 / 60.0;
        // Only the press itself until the delay is up, then repeats.
        assert_eq!(held_steps(1, MENU_REPEAT_DELAY - dt, dt), 1);
        assert!(held_steps(1, MENU_REPEAT_DELAY + MENU_REPEAT_INTERVAL * 2.0, dt) > 1);
    }

    /// The regression this guards: reading the DPad as level state stepped
    /// once per rendered frame, so a second of holding gave 60 steps at 60fps
    /// and 300 at 300fps. Sampling a fixed repeat at different frame times
    /// still lands a step either side of a boundary, hence the tolerance.
    #[test]
    fn dpad_repeat_rate_does_not_depend_on_the_framerate() {
        // One press plus a second minus the delay, repeating at the interval.
        let expected = 1 + ((1.0 - MENU_REPEAT_DELAY) / MENU_REPEAT_INTERVAL) as i32;
        for dt in [1.0 / 30.0, 1.0 / 60.0, 1.0 / 144.0, 1.0 / 300.0] {
            let steps = held_steps(1, 1.0, dt);
            assert!(
                (steps - expected).abs() <= 1,
                "{steps} steps in a second at dt {dt}, expected about {expected}"
            );
        }
    }

    #[test]
    fn releasing_re_arms_the_delay_and_a_flip_steps_immediately() {
        let dt = 1.0 / 60.0;
        let mut stepper = RepeatStepper::default();
        assert_eq!(stepper.step(1, dt), 1);
        assert_eq!(stepper.step(0, dt), 0);
        assert_eq!(stepper.step(1, dt), 1);
        // Right to Left with no neutral frame between.
        assert_eq!(stepper.step(-1, dt), -1);
        assert_eq!(stepper.step(-1, dt), 0);
    }

    #[test]
    fn controller_packet_directions_match_physical_stick_direction() {
        let input = InputState::released();

        let right_state =
            player_input_state(&input, gamepad_movement_axes(glam::vec2(1.0, 0.0)), false);
        let right = serverbound_player_input(&right_state);
        assert!(right.right);
        assert!(!right.left);

        let left_state =
            player_input_state(&input, gamepad_movement_axes(glam::vec2(-1.0, 0.0)), false);
        let left = serverbound_player_input(&left_state);
        assert!(left.left);
        assert!(!left.right);

        let forward_right_state =
            player_input_state(&input, gamepad_movement_axes(glam::vec2(0.8, 0.8)), false);
        let forward_right = serverbound_player_input(&forward_right_state);
        assert!(forward_right.forward);
        assert!(forward_right.right);
        assert!(!forward_right.backward);
        assert!(!forward_right.left);
    }

    #[test]
    fn cursor_never_grabs_while_unfocused() {
        assert_eq!(
            cursor_step(false, false, true, false),
            (CursorOp::Keep, false, false)
        );
        assert_eq!(
            cursor_step(true, true, true, false),
            (CursorOp::Keep, true, true)
        );
    }

    #[test]
    fn cursor_calls_are_idempotent() {
        assert_eq!(
            cursor_step(false, false, false, true),
            (CursorOp::Keep, false, false)
        );
        assert_eq!(
            cursor_step(true, false, true, true),
            (CursorOp::Keep, true, false)
        );
    }

    #[test]
    fn cursor_centers_once_on_grabbed_to_released() {
        let (op, grabbed, stale) = cursor_step(true, false, false, true);
        assert_eq!(op, CursorOp::Release { center: true });
        assert_eq!(
            cursor_step(grabbed, stale, false, true),
            (CursorOp::Keep, false, false)
        );
    }

    #[test]
    fn stale_cursor_reissues_without_warping_a_free_cursor() {
        // Refocus while gameplay holds the grab: the WM may have dropped the
        // lock, so it is re-locked.
        assert_eq!(
            cursor_step(true, true, true, true),
            (CursorOp::Grab, true, false)
        );
        // Refocus on a screen: the release is re-issued without a warp.
        assert_eq!(
            cursor_step(false, true, false, true),
            (CursorOp::Release { center: false }, false, false)
        );
    }

    #[test]
    fn death_route_follows_show_death_screen() {
        assert_eq!(death_route(true), DeathRoute::ShowDeathScreen);
        assert_eq!(death_route(false), DeathRoute::Respawn);
    }

    fn chat_body() -> SignedChatBody {
        SignedChatBody {
            content: "hello".to_owned(),
            timestamp_ms: 1_000,
            salt: 0,
            last_seen: Vec::new(),
            message_index: 0,
            decorated_content: Box::new(crate::chat_component::Component::text("hello")),
            decorated_signed_content: Box::new(crate::chat_component::Component::text("hello")),
            modified_style: true,
            fully_filtered: false,
        }
    }

    #[test]
    fn integrated_server_own_chat_is_always_secure() {
        let body = chat_body();
        let bindings = crate::app::input::KeyBindings::default();
        let expired_now = 1_000 + 8 * 60 * 1000;
        assert_eq!(
            accepted_player_chat_tag(true, false, &body, expired_now, false, &bindings),
            None
        );
        assert_eq!(
            accepted_player_chat_tag(true, true, &body, expired_now, false, &bindings),
            None
        );
        assert_eq!(
            accepted_player_chat_tag(true, true, &body, 1_001, true, &bindings),
            None
        );

        assert_eq!(
            accepted_player_chat_tag(false, false, &body, 1_001, false, &bindings),
            Some(ChatMessageTag::NotSecure)
        );
        assert_eq!(
            accepted_player_chat_tag(false, true, &body, expired_now, false, &bindings),
            Some(ChatMessageTag::NotSecure)
        );
        assert_eq!(
            accepted_player_chat_tag(false, true, &body, 1_001, true, &bindings),
            Some(ChatMessageTag::Modified {
                original: "hello".to_owned(),
            })
        );
    }

    #[test]
    fn correction_pitch_clamps_before_entity_setter_modulo() {
        let current = LookDirection::new(30.0, 80.0);

        let absolute = resolve_corrected_look(current, 720.0, 360.0, false, false);
        assert_eq!(absolute.y_rot_deg(), 720.0);
        assert_eq!(absolute.x_rot_deg(), 90.0);

        let relative = resolve_corrected_look(current, 400.0, 50.0, true, true);
        assert_eq!(relative.y_rot_deg(), 430.0);
        assert_eq!(relative.x_rot_deg(), 90.0);
    }

    #[test]
    fn mounted_player_position_correction_is_acknowledged_without_local_application() {
        use pomme_protocol::entity::{PositionMoveRotation, Relative};

        let mut player = LocalPlayer::new();
        player.position = Position::new(1.0, 2.0, 3.0);
        player.prev_position = Position::new(0.5, 1.5, 2.5);
        player.velocity = Velocity::new(0.1, 0.2, 0.3);
        player.look_dir = LookDirection::new(10.0, 20.0);
        player.prev_look_dir = LookDirection::new(5.0, 15.0);

        let change = PositionMoveRotation {
            position: glam::DVec3::new(100.0, 200.0, 300.0),
            delta: glam::DVec3::new(1.0, 2.0, 3.0),
            y_rot: 90.0,
            x_rot: 45.0,
        };
        let relative = Relative::default();

        let unmounted = resolve_player_position_correction(&player, &change, &relative, false)
            .expect("unmounted correction applies");
        assert_eq!(unmounted.position, Position::new(100.0, 200.0, 300.0));
        assert_eq!(unmounted.velocity, Velocity::new(1.0, 2.0, 3.0));
        assert_eq!(unmounted.look_dir, LookDirection::new(90.0, 45.0));

        assert!(
            resolve_player_position_correction(&player, &change, &relative, true).is_none(),
            "vanilla mounted clients acknowledge the teleport but keep current local PosRot"
        );
    }

    #[test]
    fn player_input_packet_uses_physical_sprint_key_state() {
        let input = InputState::released();
        let pressed = serverbound_player_input(&player_input_state(&input, glam::Vec2::ZERO, true));
        let released =
            serverbound_player_input(&player_input_state(&input, glam::Vec2::ZERO, false));
        assert!(pressed.sprint);
        assert!(!released.sprint);
    }

    #[test]
    fn server_view_distance_updates() {
        // Echo of our own request carries no cap information.
        assert_eq!(server_view_distance_update(12, 12), None);
        // Below the request: min(request, cap) revealed the server cap.
        assert_eq!(server_view_distance_update(10, 12), Some(10));
        // Above the request: the server's actual view distance, including a
        // reduction from an earlier higher announcement.
        assert_eq!(server_view_distance_update(64, 12), Some(64));
        assert_eq!(server_view_distance_update(20, 12), Some(20));
        // Wire values past the chunk grid's extent clamp to it.
        assert_eq!(server_view_distance_update(300, 12), Some(128));
    }

    #[test]
    fn max_health_updates_clamp_current_health() {
        let mut attributes = AttributeMap::player();
        let (mut health, mut max_health, mut camera_distance) = (18.0, 20.0, 4.0);
        let mut set_max_health = |base, dirty: bool| {
            assert!(attributes.apply_snapshot(&AttributeSnapshot {
                attribute: AttributeKind::MaxHealth,
                base,
                modifiers: vec![],
            }));
            sync_living_attribute_mirrors(
                &attributes,
                &mut health,
                &mut max_health,
                &mut camera_distance,
                |_| dirty,
            );
            (health, max_health)
        };
        assert_eq!(set_max_health(10.0, false), (18.0, 10.0));
        assert_eq!(set_max_health(10.0, true), (10.0, 10.0));
        assert_eq!(set_max_health(30.0, true), (10.0, 30.0));
    }

    #[test]
    fn respawn_sync_applies_attributes_after_player_reset() {
        let mut player = LocalPlayer::new();
        assert!(player.attributes.apply_snapshot(&AttributeSnapshot {
            attribute: AttributeKind::Armor,
            base: 6.0,
            modifiers: vec![AttributeModifier {
                id: "minecraft:test_armor".into(),
                amount: 2.0,
                operation: AttributeModifierOperation::Value,
            }],
        }));
        assert!(player.attributes.apply_snapshot(&AttributeSnapshot {
            attribute: AttributeKind::MaxHealth,
            base: 10.0,
            modifiers: vec![AttributeModifier {
                id: "minecraft:test_health".into(),
                amount: 4.0,
                operation: AttributeModifierOperation::Value,
            }],
        }));

        player.reset_for_respawn(false);
        sync_respawn_attributes(&mut player, true);
        assert_eq!(player.armor, 8);
        assert_eq!(player.max_health, 14.0);
        assert_eq!(player.health, 14.0);

        player.reset_for_respawn(false);
        sync_respawn_attributes(&mut player, false);
        assert_eq!(player.armor, 6);
        assert_eq!(player.max_health, 10.0);
        assert_eq!(player.health, 10.0);
    }

    #[test]
    fn max_absorption_update_clamps_absorption() {
        let mut player = LocalPlayer::new();
        player.absorption = 8.0;
        assert!(player.attributes.apply_snapshot(&AttributeSnapshot {
            attribute: AttributeKind::MaxAbsorption,
            base: 4.0,
            modifiers: vec![],
        }));
        sync_player_attribute_mirrors(&mut player, |_| false);
        assert_eq!(player.absorption, 8.0);
        sync_player_attribute_mirrors(&mut player, |kind| kind == AttributeKind::MaxAbsorption);
        assert_eq!(player.absorption, 4.0);
    }

    #[test]
    fn chunk_detail_off_keeps_every_column_at_full_detail() {
        use crate::position::ChunkPos;
        let player = ChunkPos::new(0, 0);
        let far = ChunkPos::new(100, -100);
        assert_eq!(super::chunk_lod(far, player, 8), 2);
        assert_eq!(
            super::chunk_lod(far, player, crate::ui::menu::CHUNK_DETAIL_OFF),
            0
        );
    }
}
