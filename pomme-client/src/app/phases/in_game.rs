use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use azalea_inventory::components::EquipmentSlot;
use azalea_protocol::packets::game::{
    ServerboundClientInformation, ServerboundCommandSuggestion, ServerboundGamePacket,
};
use azalea_registry::builtin::{BlockEntityKind, EntityKind};
use glam::FloatExt as _;

use crate::app::core::{AppCore, PlayerInputState};
use crate::app::input::{Action, InputKey, KeyBindings};
use crate::app::level_load::LevelLoadTracker;
use crate::app::phases::Gfx;
use crate::app::{TICK_RATE, input};
use crate::audio::{CATEGORY_AMBIENT, CATEGORY_PLAYERS, SoundRef};
use crate::benchmark::{
    BenchMode, Benchmark, BenchmarkResult, ChunkLoadBench, ChunkLoadResult, ChunkLoadStep,
    UploadHandle, UploadStatus, upload_result,
};
use crate::entity::components::{LookDirection, Position};
use crate::entity::light::entity_light;
use crate::entity::{EntityStore, ItemEntityStore, lerp_angle};
use crate::net::connection::ConnectionHandle;
use crate::player::LocalPlayer;
use crate::player::interaction::{
    HANDS, HitResult, InteractionHand, InteractionState, send_use_swing,
};
use crate::player::menu_click::ContainerKind;
use crate::player::tab_list::TabList;
use crate::position::{BlockPos, ChunkPos};
use crate::renderer::chunk::block_lighter::{self, LightLevels, WorldLight};
use crate::renderer::chunk::buffer::column_is_near;
use crate::renderer::chunk::mesh_columns::MeshColumns;
use crate::renderer::chunk::mesh_source::MeshSource;
use crate::renderer::chunk::mesher::{BiomeClimate, ChunkMeshData, MeshDispatcher};
use crate::renderer::chunk::occlusion_graph::{self, VisibilitySet};
use crate::renderer::entity_model::{lerp, triangle_wave};
use crate::renderer::pipelines::block_entity;
use crate::renderer::pipelines::entity_renderer::{
    EntityRenderInfo, MAX_OVERLAYS, PackShadow, WHITE_TINT, armor_render_infos, dye_color_tint,
    jeb_sheep_tint, standing_eye_height, wool_color_tint,
};
use crate::renderer::pipelines::entity_shadow::{ShadowCaster, ShadowScene};
use crate::renderer::pipelines::lightmap::LightmapInfo;
use crate::renderer::pipelines::menu_overlay::MenuElement;
use crate::renderer::pipelines::post_effect::PostEffect;
use crate::renderer::pipelines::shaderpack::PackWorld;
use crate::renderer::{Renderer, SkyState};
use crate::resource_pack::ResourcePackManager;
use crate::ui::chat::{ChatState, ChatUiAction};
use crate::ui::death::{self, DeathAction};
use crate::ui::pause::{self, PauseAction, PauseScreen};
use crate::ui::{common, hud};
use crate::world::block::BedDirection;
use crate::world::block::block_registry::{BlockDefinitions, BlockRegistry};
use crate::world::block::model::CardinalLightType;
use crate::world::block_entity_anim::BlockEntityAnimStore;
use crate::world::chunk::ChunkStore;

/// The F3 "Mesh gate" line: among in-frustum columns, the sections drawn and
/// those skipped as occluded (the per-section occlusion win). Kept as each
/// walk lands and as columns unload, so F3 doesn't walk every column per
/// frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MeshGate {
    pub visible: u32,
    pub hidden: u32,
}

impl MeshGate {
    /// An in-frustum column's share: its `mask` out of `sections`.
    fn column(mask: u32, sections: u32) -> Self {
        let visible = mask.count_ones();
        Self {
            visible,
            hidden: sections.saturating_sub(visible),
        }
    }

    fn add(&mut self, mask: u32, sections: u32) {
        let column = Self::column(mask, sections);
        self.visible += column.visible;
        self.hidden += column.hidden;
    }

    /// Takes back what [`Self::add`] counted for the same column.
    pub(crate) fn remove(&mut self, mask: u32, sections: u32) {
        let column = Self::column(mask, sections);
        self.visible -= column.visible;
        self.hidden -= column.hidden;
    }
}

/// Which screen a server-opened container renders as.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ContainerScreen {
    CraftingTable,
    Furnace(crate::ui::furnace::FurnaceVariant),
    Chest { rows: u8 },
    ShulkerBox,
    Anvil,
    Enchantment,
}

impl ContainerScreen {
    /// The click-prediction menu kind backing this screen.
    pub fn click_kind(self) -> ContainerKind {
        match self {
            Self::CraftingTable => ContainerKind::CraftingTable,
            Self::Furnace(_) => ContainerKind::Furnace,
            Self::Chest { rows } => ContainerKind::Chest { rows },
            Self::ShulkerBox => ContainerKind::ShulkerBox,
            Self::Anvil => ContainerKind::Anvil,
            Self::Enchantment => ContainerKind::Enchantment,
        }
    }
}

/// A server-opened container screen.
pub struct OpenContainer {
    pub id: i32,
    pub title: String,
    #[cfg(feature = "test-harness")]
    pub menu: azalea_registry::builtin::MenuKind,
    pub screen: ContainerScreen,
    /// Menu slots in container indices; slots from `inv_start()` on are backed
    /// by the player inventory.
    pub slots: Vec<azalea_inventory::ItemStack>,
    /// The menu's data values (`ClientboundContainerSetData`), e.g. furnace
    /// lit/cook progress or the anvil repair cost. Vanilla data slots are
    /// shorts; the enchanting table uses all 10 (costs, seed, clues) with -1
    /// sentinels, so values are kept sign-extended.
    pub data: [i16; 10],
    /// The anvil rename field's state; Some only for the anvil screen.
    pub anvil: Option<crate::ui::anvil::AnvilState>,
    /// The book animation's state; Some only for the enchantment screen.
    pub enchant: Option<crate::ui::enchantment::EnchantState>,
    /// This menu's latest server state id, echoed in container clicks.
    pub state_id: u32,
}

impl OpenContainer {
    /// First container slot backed by the player inventory; container slot `i`
    /// maps to player inventory slot `i - inv_start() + 9` from here on.
    fn inv_start(&self) -> usize {
        self.screen.click_kind().inv_start()
    }
}

pub struct GameState {
    pub chunk_store: ChunkStore,
    /// Client-side light engine (vanilla `LevelLightEngine`), on its own
    /// thread; reset with the chunk store on dimension changes, polled once a
    /// frame.
    pub light_engine: crate::world::light::LightWorker,
    /// Set by [`Self::update_light`] when chunk-load light marked columns
    /// dirty; consumed by the visibility refresh as its new-loads signal.
    pub pending_load_rescan: bool,
    pub entity_store: EntityStore,
    /// Last server position for every spawned entity, including entity kinds
    /// Pomme does not otherwise render. Used by packet-driven entity-bound
    /// sounds.
    pub entity_positions: HashMap<i32, Position>,
    /// Entity ids whose shared `DATA_SILENT` flag is currently true.
    pub silent_entities: HashSet<i32>,
    pub position_set: bool,
    /// Vanilla `ClientPacketListener.levelLoadTracker`: present from login or
    /// respawn until the level is ready and `player_loaded` has been sent.
    pub level_load: Option<LevelLoadTracker>,
    /// Vanilla `ClientPacketListener.clientLoaded`. While false, the local
    /// player doesn't tick and sends no movement.
    pub client_loaded: bool,
    pub player: LocalPlayer,
    /// Bubble index the pop sound last played for, so each pop fires once.
    pub last_bubble_pop_sound_played: i32,
    pub biome_climate: Arc<HashMap<u32, BiomeClimate>>,
    /// The biome registry's names by id, built when a shader pack needs them.
    pack_biome_names: Arc<[String]>,
    pub player_walk_pos: f32,
    /// Vanilla `yBodyRot` for the local player, and last tick's: the body
    /// turns after where it walks, within 50 degrees of the head.
    pub player_body_y_rot: f32,
    pub player_prev_body_y_rot: f32,
    pub player_walk_speed: f32,
    pub player_prev_walk_speed: f32,
    pub mesh_dispatcher: MeshDispatcher,
    /// Whether the server is this process, which the pause menu labels.
    pub singleplayer: bool,
    pub paused: bool,
    pub dead: bool,
    pub death_screen_open: bool,
    pub show_death_screen: bool,
    pub hardcore: bool,
    pub heart_blink: hud::HeartBlink,
    /// The login packet's `onlineMode`: the tab list shows player heads.
    pub online_mode: bool,
    pub death_message: String,
    pub death_screen_ticks: u32,
    pub death_confirm: bool,
    pub death_confirm_ticks: u32,
    pub respawn_sent: bool,
    pub inventory_open: bool,
    pub creative_inventory_open: bool,
    pub creative_state: crate::ui::creative_inventory::CreativeState,
    /// The inventory menu's (container 0) latest server state id, echoed in
    /// container clicks; an open container keeps its own.
    pub inventory_state_id: u32,
    /// Carried (cursor) stack for container screens, driven by the server.
    pub cursor_item: azalea_inventory::ItemStack,
    /// The server-opened container screen (crafting table), if any.
    pub open_container: Option<OpenContainer>,
    /// Which container menu was open last frame (0 = survival inventory), to
    /// detect the close transition and send a container-close packet.
    pub container_was_open: Option<i32>,
    /// Active survival click-drag (button + slots covered), if any.
    pub inv_drag: Option<(azalea_inventory::operations::QuickCraftKind, Vec<u16>)>,
    /// Last survival left click (slot, time) for double-click detection.
    pub inv_last_click: Option<(u16, Instant)>,
    /// The selected entry of a bundle in (menu id, slot). Vanilla keeps it on
    /// the stack but never sends it, so a server update of the slot drops it.
    pub bundle_selection: Option<(i32, u16, i32)>,
    /// The slot the container screen hovered last frame (menu id, slot, and
    /// whether it held a bundle), for vanilla `onStopHovering`.
    pub bundle_hovered: Option<(i32, u16, bool)>,
    /// `BundleMouseActions`' `ScrollWheelHandler` accumulators (x, y).
    pub bundle_scroll: (f32, f32),
    /// Server registries, for hashing predicted container clicks.
    pub registries: Arc<azalea_core::registry_holder::RegistryHolder>,
    /// Session metadata and state bindings to the app-owned block definitions.
    pub(crate) block_registry: BlockRegistry,
    /// Per-session recipe book, recipe container, ghost placement, and item
    /// tags.
    pub recipe_book: crate::recipe::RecipeBookState,
    pub recipe_book_ui: crate::ui::recipe_book::RecipeBookUiState,
    pub chat: ChatState,
    pub server_dialog: Option<crate::ui::server_dialog::ServerDialogState>,
    pub server_links: Vec<crate::ui::server_dialog::ServerLink>,
    pub dialog_registry: Arc<crate::ui::server_dialog::DialogRegistry>,
    /// The connection is in the configuration phase (the join, or a
    /// reconfiguration), where dialogs can't run commands.
    pub configuring: bool,
    /// UI actions that request vanilla STOP_SLEEPING on the next update pass.
    pub stop_sleeping_requested: bool,
    pub command_tree: Option<Arc<crate::net::commands::CommandTree>>,
    pub tab_list: TabList,
    pub server_enforces_secure_chat: bool,
    /// Locator bar waypoints tracked by the server.
    pub waypoints: crate::world::waypoints::WaypointMap,
    /// Vanilla `Hud.toolHighlightTimer` / `lastToolHighlight` (see
    /// `tick_tool_highlight`).
    pub tool_highlight_timer: u32,
    pub last_tool_highlight: azalea_inventory::ItemStack,
    pub action_bar: Option<(Vec<crate::ui::text::TextSpan>, u64)>,
    pub title: crate::ui::title::TitleState,
    pub scoreboard: crate::ui::hud::Scoreboard,
    pub boss_bars: crate::ui::boss_bar::BossBarState,
    pub toasts: crate::ui::toast::ToastState,
    pub subtitles: crate::ui::subtitles::SubtitleOverlayState,
    /// The sound events most recently played, oldest first.
    #[cfg(feature = "test-harness")]
    pub recent_sounds: std::collections::VecDeque<String>,
    /// Client tick counter (vanilla `player.tickCount`).
    pub tick_count: u64,
    /// Vanilla `Hud.autosaveIndicatorValue` / `lastAutosaveIndicatorValue`;
    /// driven by in-flight screenshot writes (pomme saves no worlds).
    pub saving_indicator_value: f32,
    pub last_saving_indicator_value: f32,
    /// Tick of the last XP progress change; the XP bar outprioritizes the
    /// locator bar for 100 ticks after it (vanilla
    /// `experienceDisplayStartTick`; `i64::MIN` = untouched since (re)spawn,
    /// so the first change after joining never takes priority).
    pub xp_display_start_tick: i64,
    /// Vehicle we are the controlling (first) passenger of, from
    /// `SetPassengers` (vanilla `getControlledVehicle`).
    pub controlled_vehicle_id: Option<i32>,
    /// A released ride jump waiting for the horse to touch the ground
    /// (`AbstractHorse.playerJumpPendingScale`).
    pub ridden_jump_pending: f32,
    /// Vehicle we are any passenger of (vanilla `getVehicle`).
    pub riding_vehicle_id: Option<i32>,
    /// Smoothed vignette darkness (vanilla `Hud.vignetteBrightness`).
    pub vignette_brightness: f32,
    pub interaction: InteractionState,
    pub sky_state: crate::renderer::SkyState,
    pub show_debug: bool,
    pub show_chunk_borders: bool,
    pub advanced_item_tooltips: bool,
    /// F1 (vanilla `hideGui`): the HUD, chat, and overlays don't render.
    pub hide_gui: bool,
    /// A chord fired while F3 was held, so releasing F3 must not toggle the
    /// overlay (vanilla `usedDebugKeyAsModifier`).
    pub f3_chord_consumed: bool,
    /// An overlay input was released while another debug modifier remained
    /// held.
    pub debug_overlay_pending: bool,
    /// Set by F3+A; consumed by `update_game` to re-mesh every loaded chunk.
    pub pending_chunk_reload: bool,
    /// Game mode before the last change (vanilla `previousLocalPlayerMode`),
    /// the F3+N return target.
    pub previous_game_mode: Option<u8>,
    /// The entity a spectator views the world from (vanilla
    /// `Minecraft.cameraEntity`); `None` is the local player.
    pub camera_entity: Option<i32>,
    /// Vanilla `spectatedEntityPostEffect`: the camera entity's view effect.
    pub spectator_effect: Option<PostEffect>,
    /// Vanilla `spectatedEntityEffectActive`, flipped by its toggle key.
    pub spectator_effect_active: bool,
    /// Current dimension identifier (e.g. "minecraft:overworld"), for F3+C.
    pub dimension: String,
    /// Dimension-type `cardinal_light`, which picks the terrain shade table
    /// and the item entity light directions.
    pub cardinal_light: CardinalLightType,
    /// Dimension-type `ambient_light`: 0 in the overworld, 0.1 in the
    /// nether, 0.25 in the end.
    pub ambient_light: f32,
    /// The current dimension's `default_clock`, whose ticks are its day time.
    pub default_clock: Option<String>,
    /// The level is superflat (`ClientLevelData.isFlat`).
    pub flat: bool,
    pub clocks: crate::world::clock::ClientClocks,
    /// The current dimension's environment attribute layers, and the
    /// attributes they resolve to each tick.
    pub environment: crate::world::environment::Environment,
    pub environment_probe: crate::world::environment::EnvironmentProbe,
    /// The biomes around the camera, as of the latest tick.
    pub biome_weights: crate::world::environment::BiomeWeights,
    /// Vanilla `LightmapRenderStateExtractor.blockLightFlicker`.
    pub block_light_flicker: f32,
    /// Set by the test harness's `deterministic` step.
    pub deterministic: bool,
    /// F3+F4 game-mode switcher overlay, while open.
    pub game_mode_switcher: Option<crate::ui::game_mode_switcher::GameModeSwitcherState>,
    /// Spectator hotbar menu (vanilla `SpectatorGui`). Not a GUI screen: the
    /// cursor stays grabbed and mouse look stays live while it is open.
    pub spectator: crate::ui::spectator_menu::SpectatorGuiState,
    /// Last frame's switcher presence, to re-apply the cursor grab on change.
    switcher_was_open: bool,
    pub last_sent_input: PlayerInputState,
    pub last_sent_pos: Position,
    pub last_sent_look_dir: LookDirection,
    pub last_sent_on_ground: bool,
    pub last_sent_horizontal_collision: bool,
    pub was_sprinting: bool,
    pub position_send_counter: u32,
    pub options_from_game: bool,
    pub last_render_distance: u32,
    pub last_chat_visibility: crate::ui::chat::ChatVisibilitySetting,
    pub last_chat_colors: bool,
    pub last_skin_options: crate::ui::menu::SkinOptions,
    pub server_render_distance: u32,
    pub server_simulation_distance: u32,
    pub item_entity_store: ItemEntityStore,
    pub display_store: crate::entity::display::DisplayStore,
    /// Entity names and whether they show (`entity::name_tags`).
    pub name_store: crate::entity::name_tags::NameStore,
    pub falling_blocks: crate::entity::falling_block::FallingBlockStore,
    pub interactions: crate::entity::interaction::InteractionStore,
    /// The loaded end portal and gateway blocks (`TheEndPortalRenderer`'s),
    /// found when their chunk loads and kept up as blocks change.
    pub end_portals: std::collections::HashSet<BlockPos>,
    /// The level's maps, as the server has sent them.
    pub maps: crate::world::map::MapStore,
    pub end_crystals: crate::entity::end_crystal::EndCrystalStore,
    pub item_frames: crate::entity::item_frame::ItemFrameStore,
    pub lightning: crate::entity::lightning::LightningStore,
    pub experience_orbs: crate::entity::experience_orb::ExperienceOrbStore,
    /// `ClientLevel.skyFlashTime`: ticks the sky stays lit by lightning.
    pub sky_flash_time: u32,
    /// The first-person hands' heights and the stacks they show.
    pub hand_equip: crate::player::hand_equip::HandEquip,
    pub thrown_items: crate::entity::thrown_item::ThrownItemStore,
    pub arrows: crate::entity::arrow::ArrowStore,
    pub model_entities: crate::entity::model_entity::ModelEntityStore,
    pub paintings: crate::entity::painting::PaintingStore,
    pub effect_clouds: crate::entity::effect_cloud::EffectCloudStore,
    pub leashes: crate::entity::leash::LeashStore,
    /// Each nearby spawner's spin, last tick's and this one's, in the
    /// tenths of a degree vanilla counts it in (`BaseSpawner.spin`).
    pub spawner_spins: HashMap<BlockPos, (f32, f32)>,
    /// The books over the enchanting tables near the player.
    pub enchanting_books: HashMap<BlockPos, crate::world::book::BookAnim>,
    /// Each beacon's beam as last worked out, and the tick it was.
    pub beacon_beams: HashMap<BlockPos, (u64, Vec<crate::world::beacon::BeamSection>)>,
    /// The laid-out labels of named map markers, by their text, so a label
    /// drawn every frame keeps its glyphs.
    pub map_labels: HashMap<String, Arc<crate::entity::display::TextLayout>>,
    pub fishing_hooks: crate::entity::fishing_hook::FishingHookStore,
    pub particle_store: crate::particle::ParticleStore,
    /// `WeatherEffectRenderer.rainSoundTime`: ticks since the last rain sound.
    pub rain_sound_time: i32,
    pub level_runtime: crate::world::level::LevelRuntime,
    pub block_entity_anim: BlockEntityAnimStore,
    pub benchmark: Option<Benchmark>,
    pub benchmark_result: Option<BenchmarkResult>,
    /// In-flight/finished upload of the FPS result, while its overlay is shown.
    pub benchmark_upload: Option<UploadHandle>,
    /// Which pause screen is showing (main / benchmark submenu / chunk loader).
    pub pause_screen: PauseScreen,
    pub chunk_load_bench: Option<ChunkLoadBench>,
    pub chunk_load_result: Option<ChunkLoadResult>,
    /// Set by Esc while a chunk-load benchmark runs; consumed next frame to
    /// cancel it.
    pub chunk_load_abort: bool,
    /// In-flight/finished upload of the chunk-load result, while its overlay is
    /// shown.
    pub chunk_load_upload: Option<UploadHandle>,
    /// Last frame's `update_game` CPU phase timings, for the chunk-load
    /// benchmark's worst-frame breakdown.
    pub last_update_phases: crate::benchmark::UpdatePhases,
    /// Which loaded columns need (re)meshing.
    mesh_columns: MeshColumns,
    /// The last walk's in-frustum columns and their visible-section masks, so
    /// an unloading column can leave `mesh_gate`.
    pub in_view_masks: HashMap<ChunkPos, u32>,
    /// Per-section cave-cull visibility (vanilla `VisibilitySet`). Fed by mesh
    /// results; consumed by the occlusion walk.
    pub section_vis: HashMap<(ChunkPos, i32), VisibilitySet>,
    /// Per-column bitmask of sections whose mesh is finished and, if it had
    /// any geometry, uploaded — vanilla's "not `UNCOMPILED`", where an empty
    /// mesh counts too. The level load gate waits on the camera's bit.
    pub compiled: HashMap<ChunkPos, u32>,
    /// Highest upload epoch each `section_vis` entry was set from; mirrors the
    /// buffer's per-section geometry gate so a stale bulk can't re-stale an
    /// edited section's visibility.
    pub section_vis_epoch: HashMap<(ChunkPos, i32), u64>,
    pub mesh_gate: MeshGate,
    pub vis_valid: bool,
    /// Camera 8-block bucket that last triggered an occlusion walk — movement,
    /// not rotation, drives recomputes (vanilla's cadence).
    pub last_vis_cam: (i32, i32, i32),
    /// In-flight async occlusion walk; its result is applied a few frames
    /// later.
    pub vis_task: Option<crossbeam_channel::Receiver<HashMap<ChunkPos, u32>>>,
    /// Runtime toggle for graph-driven chunk occlusion culling (F3+O). When
    /// off, only frustum culling applies (full masks pushed to the
    /// renderer).
    pub chunk_occlusion_enabled: bool,
}

impl GameState {
    /// The day time is the dimension's default clock.
    pub fn sync_day_time(&mut self) {
        self.sky_state.day_time = self.clocks.total_ticks(self.default_clock.as_deref());
    }

    /// The mob a spectator views the world from, if it's one the client knows.
    pub fn spectated(&self) -> Option<&crate::entity::LivingEntity> {
        self.entity_store.living.get(&self.camera_entity?)
    }

    /// Vanilla `Minecraft.setCameraEntity`; `None` is the local player.
    pub fn set_camera(&mut self, id: Option<i32>) {
        self.camera_entity = id;
        self.check_spectator_effect(self.spectated().map(|e| e.entity_type));
    }

    /// Vanilla `GameRenderer.checkEntityPostEffect`: the effect of viewing
    /// the world as `camera`, `None` for no entity.
    pub fn check_spectator_effect(&mut self, camera: Option<EntityKind>) {
        self.spectator_effect = match camera {
            Some(EntityKind::Creeper) => Some(PostEffect::Creeper),
            Some(EntityKind::Spider | EntityKind::CaveSpider) => Some(PostEffect::Spider),
            Some(EntityKind::Enderman) => Some(PostEffect::Invert),
            _ => None,
        };
        self.spectator_effect_active = self.spectator_effect.is_some();
    }

    /// The spectator effect the frame draws: vanilla requests it only while
    /// it's toggled on.
    pub fn drawn_spectator_effect(&self) -> Option<PostEffect> {
        self.spectator_effect
            .filter(|_| self.spectator_effect_active)
    }

    /// This dimension's `skyDarken` as of the latest tick.
    pub fn sky_darken(&self) -> u8 {
        self.environment_probe.current().sky_darken()
    }

    /// The environment attributes at the current clock, biomes and weather.
    pub fn resolve_environment(&self) -> crate::world::environment::EnvAttributes {
        let sky = &self.sky_state;
        self.environment.resolve(
            sky.day_time as i64,
            &self.biome_weights,
            sky.rain(),
            sky.thunder(),
        )
    }

    /// Vanilla `EnvironmentAttributeProbe.tick`'s biome sampling around the
    /// camera, in quart cells.
    fn sample_biomes(&mut self, camera: glam::DVec3) {
        let plains = self.environment.plains;
        let chunks = &self.chunk_store;
        self.biome_weights =
            crate::world::environment::BiomeWeights::sample(camera * 0.25, |x, y, z| {
                chunks.noise_biome(x, y, z).unwrap_or(plains)
            });
    }

    /// Notes a played sound for the harness, keeping the last 64.
    #[cfg(feature = "test-harness")]
    pub fn record_sound(&mut self, sound: &crate::audio::SoundRef) {
        if self.recent_sounds.len() == 64 {
            self.recent_sounds.pop_front();
        }
        self.recent_sounds.push_back(sound.event_name().to_owned());
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        mesh_source: &impl MeshSource,
        resource_packs: Option<&ResourcePackManager>,
        render_distance: u32,
        singleplayer: bool,
        chat_options: crate::ui::chat::ChatOptions,
        skin_options: crate::ui::menu::SkinOptions,
        block_definitions: Rc<BlockDefinitions>,
        protocol: i32,
    ) -> Self {
        let block_registry = BlockRegistry::new(
            block_definitions,
            mesh_source.shared_render_assets(),
            protocol,
        );
        let biome_climate = Arc::new(HashMap::new());
        // The dimension's shade table arrives with `DimensionInfo`, which
        // builds a fresh dispatcher.
        let mesh_dispatcher = mesh_source.create_mesh_dispatcher(
            block_registry.owned_render_view(),
            biome_climate,
            resource_packs,
            Default::default(),
        );

        let chunk_store = ChunkStore::new(render_distance);
        Self {
            light_engine: crate::world::light::LightWorker::new(
                chunk_store.height(),
                chunk_store.min_y(),
                true,
                block_registry.block_state_table(),
            ),
            pending_load_rescan: false,
            chunk_store,
            entity_store: EntityStore::new(),
            entity_positions: HashMap::new(),
            silent_entities: HashSet::new(),
            position_set: false,
            level_load: None,
            client_loaded: false,
            options_from_game: false,
            last_render_distance: render_distance,
            last_chat_visibility: chat_options.visibility,
            last_chat_colors: chat_options.colors,
            last_skin_options: skin_options,
            server_render_distance: 0,
            server_simulation_distance: 0,
            item_entity_store: ItemEntityStore::new(),
            display_store: crate::entity::display::DisplayStore::default(),
            name_store: crate::entity::name_tags::NameStore::default(),
            falling_blocks: Default::default(),
            interactions: Default::default(),
            end_portals: Default::default(),
            maps: Default::default(),
            end_crystals: Default::default(),
            item_frames: Default::default(),
            lightning: Default::default(),
            experience_orbs: Default::default(),
            sky_flash_time: 0,
            hand_equip: Default::default(),
            thrown_items: Default::default(),
            arrows: Default::default(),
            model_entities: Default::default(),
            paintings: Default::default(),
            effect_clouds: Default::default(),
            leashes: Default::default(),
            beacon_beams: HashMap::new(),
            enchanting_books: HashMap::new(),
            spawner_spins: HashMap::new(),
            map_labels: HashMap::new(),
            fishing_hooks: Default::default(),
            rain_sound_time: 0,
            particle_store: {
                let (grass, foliage, dry_foliage) = mesh_dispatcher.colormaps();
                crate::particle::ParticleStore::new(
                    mesh_source.uv_map().clone(),
                    grass,
                    foliage,
                    dry_foliage,
                )
            },
            block_entity_anim: BlockEntityAnimStore::default(),
            level_runtime: crate::world::level::LevelRuntime::new(),
            player: LocalPlayer::new(),
            last_bubble_pop_sound_played: 0,
            biome_climate: Arc::new(HashMap::new()),
            pack_biome_names: Arc::default(),
            player_walk_pos: 0.0,
            player_body_y_rot: 0.0,
            player_prev_body_y_rot: 0.0,
            player_walk_speed: 0.0,
            player_prev_walk_speed: 0.0,
            mesh_dispatcher,
            singleplayer,
            paused: false,
            dead: false,
            death_screen_open: false,
            show_death_screen: true,
            hardcore: false,
            heart_blink: Default::default(),
            online_mode: false,
            death_message: String::new(),
            death_screen_ticks: 0,
            death_confirm: false,
            death_confirm_ticks: 0,
            respawn_sent: false,
            inventory_open: false,
            creative_inventory_open: false,
            creative_state: crate::ui::creative_inventory::CreativeState::new(),
            inventory_state_id: 0,
            cursor_item: azalea_inventory::ItemStack::Empty,
            open_container: None,
            container_was_open: None,
            inv_drag: None,
            inv_last_click: None,
            bundle_selection: None,
            bundle_hovered: None,
            bundle_scroll: (0.0, 0.0),
            registries: Arc::new(azalea_core::registry_holder::RegistryHolder::default()),
            block_registry,
            recipe_book: crate::recipe::RecipeBookState::default(),
            recipe_book_ui: crate::ui::recipe_book::RecipeBookUiState::new(),
            chat: {
                let mut chat = ChatState::new();
                chat.set_options(chat_options);
                chat
            },
            server_dialog: None,
            server_links: Vec::new(),
            dialog_registry: Arc::default(),
            configuring: true,
            stop_sleeping_requested: false,
            command_tree: None,
            tab_list: TabList::new(),
            server_enforces_secure_chat: false,
            waypoints: crate::world::waypoints::WaypointMap::default(),
            tool_highlight_timer: 0,
            last_tool_highlight: azalea_inventory::ItemStack::Empty,
            action_bar: None,
            title: crate::ui::title::TitleState::default(),
            scoreboard: crate::ui::hud::Scoreboard::default(),
            boss_bars: crate::ui::boss_bar::BossBarState::default(),
            toasts: crate::ui::toast::ToastState::default(),
            subtitles: crate::ui::subtitles::SubtitleOverlayState::default(),
            #[cfg(feature = "test-harness")]
            recent_sounds: std::collections::VecDeque::new(),
            tick_count: 0,
            saving_indicator_value: 0.0,
            last_saving_indicator_value: 0.0,
            xp_display_start_tick: i64::MIN,
            controlled_vehicle_id: None,
            ridden_jump_pending: 0.0,
            riding_vehicle_id: None,
            vignette_brightness: 1.0,
            interaction: InteractionState::new(),
            sky_state: SkyState::default_day(),
            show_debug: false,
            show_chunk_borders: false,
            advanced_item_tooltips: false,
            hide_gui: false,
            f3_chord_consumed: false,
            debug_overlay_pending: false,
            pending_chunk_reload: false,
            previous_game_mode: None,
            camera_entity: None,
            spectator_effect: None,
            spectator_effect_active: false,
            dimension: String::new(),
            cardinal_light: Default::default(),
            ambient_light: 0.0,
            default_clock: None,
            flat: false,
            clocks: Default::default(),
            environment: Default::default(),
            environment_probe: Default::default(),
            biome_weights: Default::default(),
            block_light_flicker: 0.0,
            deterministic: false,
            game_mode_switcher: None,
            spectator: Default::default(),
            switcher_was_open: false,
            last_sent_input: PlayerInputState::default(),
            last_sent_pos: Position::default(),
            last_sent_look_dir: LookDirection::default(),
            last_sent_on_ground: false,
            last_sent_horizontal_collision: false,
            was_sprinting: false,
            position_send_counter: 0,
            benchmark: None,
            benchmark_result: None,
            benchmark_upload: None,
            pause_screen: PauseScreen::Main,
            chunk_load_bench: None,
            chunk_load_result: None,
            chunk_load_abort: false,
            chunk_load_upload: None,
            last_update_phases: crate::benchmark::UpdatePhases::default(),
            mesh_columns: MeshColumns::default(),
            in_view_masks: HashMap::new(),
            section_vis: HashMap::new(),
            compiled: HashMap::new(),
            section_vis_epoch: HashMap::new(),
            mesh_gate: MeshGate::default(),
            vis_valid: false,
            last_vis_cam: (i32::MIN, i32::MIN, i32::MIN),
            vis_task: None,
            chunk_occlusion_enabled: true,
        }
    }

    /// Whether this client drives entity `id` (vanilla
    /// `isLocalInstanceAuthoritative` for a vehicle): a saddled equine the
    /// local player controls. The server's moves for it are ignored.
    pub fn drives_vehicle(&self, id: i32) -> bool {
        self.controlled_vehicle_id == Some(id) && self.riding_jumpable_vehicle()
    }

    /// Vanilla `LocalPlayer.jumpableVehicle() != null`: controlling a saddled
    /// equine. Equine `getJumpCooldown()` is always 0; camels/nautilus (dash
    /// cooldown) aren't tracked by the entity store yet.
    pub fn riding_jumpable_vehicle(&self) -> bool {
        self.controlled_vehicle_id
            .and_then(|id| self.entity_store.living.get(&id))
            .is_some_and(|e| {
                crate::entity::is_equine(&e.entity_type)
                    && e.equipment(EquipmentSlot::Saddle).is_some()
            })
    }

    /// Vanilla `Entity.positionRider` for the local player: the vehicle's
    /// passenger riding position (a display's own position, its box being
    /// 0 x 0; otherwise the top of the vehicle's box) less the player's
    /// vehicle attachment, `Player.DEFAULT_VEHICLE_ATTACHMENT`'s 0.6.
    // TODO: per-kind passenger attachments (boats, minecarts, horses sit
    // lower than the top of their box).
    pub fn riding_seat(&self) -> Option<glam::DVec3> {
        let vehicle = self.riding_vehicle_id?;
        let seat = match self.display_store.displays.get(&vehicle) {
            Some(display) => display.position,
            None if self.model_entities.seat(vehicle).is_some() => {
                self.model_entities.seat(vehicle)?
            }
            None => display_seat(
                vehicle,
                &self.entity_store.living,
                &self.entity_positions,
                (
                    self.player.entity_id,
                    self.player.position,
                    self.player.eye_height,
                ),
            )?,
        };
        Some(seat - glam::DVec3::new(0.0, 0.6, 0.0))
    }

    /// The ridden vehicle when it is living; the living-store lookup is
    /// vanilla's `instanceof LivingEntity` gate.
    pub fn riding_vehicle(&self) -> Option<&crate::entity::LivingEntity> {
        self.riding_vehicle_id
            .and_then(|id| self.entity_store.living.get(&id))
    }

    /// Vanilla `Hud.getPlayerVehicleWithHealth`: (health, max health) of the
    /// ridden vehicle when it is living (`Entity.showVehicleHealth`).
    pub fn vehicle_health(&self) -> Option<(f32, f32)> {
        self.riding_vehicle().map(|e| (e.health, e.max_health))
    }

    // TODO: vanilla scales each distance by `getScale()` (the `scale`
    // attribute, not read yet) and reads the camera entity's, which differs
    // while spectating a mob.
    pub fn third_person_distance(&self) -> f32 {
        detached_distance(
            self.player.camera_distance,
            self.riding_vehicle().map(|vehicle| vehicle.camera_distance),
        )
    }

    /// A server dialog, or the confirm screen one raised, is the top screen.
    /// Vanilla runs no key mapping while a screen is up, and the screens under
    /// it neither draw nor take input.
    pub fn dialog_open(&self) -> bool {
        self.server_dialog.is_some() || self.chat.has_pending_modal_prompt()
    }

    /// The world, not a screen, chat or the pause menu, takes pointer input.
    pub fn world_has_pointer(&self) -> bool {
        !self.paused
            && !self.dead
            && !self.death_screen_open
            && !self.gui_open()
            && !self.chat.is_open()
    }

    pub fn gui_open(&self) -> bool {
        self.inventory_open
            || self.creative_inventory_open
            || self.open_container.is_some()
            || self.dialog_open()
            || self.game_mode_switcher.is_some()
    }

    /// The container menu the player currently has open (0 = survival
    /// inventory), if any.
    pub fn open_menu_id(&self) -> Option<i32> {
        if let Some(c) = &self.open_container {
            Some(c.id)
        } else if self.inventory_open {
            Some(0)
        } else {
            None
        }
    }

    /// The currently open menu's slots: the open container's, else the player
    /// inventory's.
    pub fn menu_slots(&self) -> &[azalea_inventory::ItemStack] {
        match &self.open_container {
            Some(c) => &c.slots,
            None => self.player.inventory.slots(),
        }
    }

    /// Set a slot of the currently open menu. Container slots backing the
    /// player inventory mirror into it, so the hotbar and a reopened
    /// inventory stay in sync.
    pub fn set_menu_slot(&mut self, index: usize, item: azalea_inventory::ItemStack) {
        match &mut self.open_container {
            Some(c) => {
                let Some(s) = c.slots.get_mut(index) else {
                    return;
                };
                *s = item.clone();
                let inv_start = c.inv_start();
                if index >= inv_start {
                    self.player.inventory.set_slot(index - inv_start + 9, item);
                }
            }
            None => self.player.inventory.set_slot(index, item),
        }
    }

    /// Re-mirror the inventory-backed slots into the open container after a
    /// direct player-inventory update.
    pub fn sync_container_from_inventory(&mut self) {
        let Some(c) = &mut self.open_container else {
            return;
        };
        let inv_start = c.inv_start();
        for (i, slot) in c.slots.iter_mut().enumerate().skip(inv_start) {
            *slot = self.player.inventory.slot(i - inv_start + 9).clone();
        }
    }

    /// Record the open container's latest server state id.
    pub fn set_container_state_id(&mut self, state_id: u32) {
        if let Some(c) = &mut self.open_container {
            c.state_id = state_id;
        }
    }

    /// Opens the player's inventory, the creative one in creative mode.
    pub fn open_inventory(&mut self) {
        if crate::player::is_creative(self.player.game_mode) {
            self.open_creative_inventory();
        } else {
            self.inventory_open = true;
        }
    }

    pub fn open_creative_inventory(&mut self) {
        self.creative_inventory_open = true;
        self.creative_state.reopen();
    }

    /// Closes whichever inventory or container menu is open.
    pub fn close_inventory(&mut self) {
        if self.creative_inventory_open {
            self.close_creative_inventory();
        } else {
            self.close_menu();
        }
    }

    pub fn close_creative_inventory(&mut self) {
        self.creative_inventory_open = false;
        self.creative_state.reset_interaction();
    }

    /// Close whichever container menu is open. Clears the carried stack
    /// (vanilla switches to the inventory menu, whose carried stack is empty;
    /// the server returns the items via inventory sync) and any in-flight
    /// gesture so a stale drag can't commit on reopen.
    pub fn close_menu(&mut self) {
        self.inventory_open = false;
        self.open_container = None;
        self.cursor_item = azalea_inventory::ItemStack::Empty;
        self.inv_drag = None;
        self.inv_last_click = None;
        self.bundle_selection = None;
        self.bundle_hovered = None;
        self.bundle_scroll = (0.0, 0.0);
        self.recipe_book_ui
            .reset_for_closed_screen(&mut self.recipe_book);
    }

    pub fn recipe_book_screen_active(&self) -> bool {
        self.inventory_open
            || matches!(
                self.open_container.as_ref(),
                Some(container)
                    if matches!(
                        container.screen,
                        ContainerScreen::CraftingTable | ContainerScreen::Furnace(_)
                    )
            )
    }

    /// A server update replaced the stack in `slot` of `menu` (every slot when
    /// `None`), dropping any bundle selection it carried.
    pub fn drop_bundle_selection(&mut self, menu: i32, slot: Option<u16>) {
        if self
            .bundle_selection
            .is_some_and(|(m, s, _)| m == menu && slot.is_none_or(|slot| slot == s))
        {
            self.bundle_selection = None;
        }
    }

    /// Replaces any open server dialog; false (logged) when `reference`
    /// doesn't resolve to a dialog.
    pub fn open_server_dialog(
        &mut self,
        reference: crate::ui::server_dialog::DialogReference,
    ) -> bool {
        match crate::ui::server_dialog::ServerDialogState::open(
            reference,
            &self.dialog_registry,
            &self.server_links,
        ) {
            Ok(dialog) => {
                self.server_dialog = Some(dialog);
                true
            }
            Err(error) => {
                tracing::warn!("Could not open server dialog: {error}");
                false
            }
        }
    }

    /// A focused text field (anvil rename, creative search) is capturing
    /// keyboard input: letter/digit keys must type instead of acting as
    /// hotkeys. The anvil field is editable only while its input slot is
    /// filled, matching vanilla.
    pub fn wants_text_input(&self) -> bool {
        if self
            .server_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.wants_text_input())
        {
            return true;
        }
        if self.creative_inventory_open {
            return self.creative_state.tab.captures_typing();
        }
        if self.recipe_book_screen_active() && self.recipe_book_ui.captures_typing() {
            return true;
        }
        matches!(
            &self.open_container,
            Some(c) if c.screen == ContainerScreen::Anvil
                && c.slots.first().is_some_and(|s| s.is_present())
        )
    }

    /// Escape on the chat screen; true when it closed chat and the cursor
    /// should be recaptured.
    pub fn escape_chat(&mut self) -> bool {
        use crate::ui::chat::ChatEscape;
        match self.chat.handle_escape() {
            ChatEscape::Closed => true,
            ChatEscape::WakeUp => {
                self.stop_sleeping_requested = true;
                false
            }
            ChatEscape::Handled => false,
        }
    }

    /// Clears the bed state and screen a reconfiguration would destroy.
    pub(crate) fn reset_sleep_for_level_teardown(&mut self) {
        self.player.reset_sleep_for_level_teardown();
        self.stop_sleeping_requested = false;
        if self.chat.is_in_bed() {
            self.chat
                .close(crate::ui::chat::ChatExitReason::Interrupted);
        }
    }

    /// Closes the death screen and its confirm, and re-arms the respawn send.
    pub fn reset_death_screen(&mut self) {
        self.death_screen_open = false;
        self.death_screen_ticks = 0;
        self.death_confirm = false;
        self.death_confirm_ticks = 0;
        self.respawn_sent = false;
    }

    /// No menu (pause, inventory, chat) is capturing input.
    pub fn input_live(&self) -> bool {
        !self.paused
            && !self.death_screen_open
            && !self.gui_open()
            && !self.chat.is_open()
            && self.benchmark_result.is_none()
            && self.chunk_load_result.is_none()
    }

    /// F3-family debug chords; these fire even while a menu is open, matching
    /// vanilla KeyboardHandler. Returns true if handled. The overlay itself
    /// toggles in [`Self::handle_f3_release`], not here.
    // TODO: vanilla gates hitbox/border/copy chords on the server's
    // reducedDebugInfo flag, which pomme doesn't track yet.
    pub fn handle_debug_key(
        &mut self,
        code: winit::keyboard::KeyCode,
        f3_held: bool,
        bindings: &KeyBindings,
        connection: &ConnectionHandle,
    ) -> bool {
        let input = InputKey::Keyboard(code);
        if bindings.matches(Action::DebugModifier, &input)
            || bindings.matches(Action::ToggleOverlay, &input)
        {
            // Consumed, but acts on release so chords can suppress it.
            return true;
        }
        if !f3_held {
            return false;
        }
        let handled = match code {
            _ if bindings.matches(Action::DebugReloadChunks, &input) => {
                self.pending_chunk_reload = true;
                self.debug_feedback("Reloading all chunks");
                true
            }
            // TODO: F3+B show hitboxes (no entity hitbox renderer yet)
            _ if bindings.matches(Action::DebugCopyLocation, &input) => {
                // Vanilla also crashes the game when held for 10s; not ported.
                let p = &self.player;
                let cmd = format!(
                    "/execute in {} run tp @s {:.2} {:.2} {:.2} {:.2} {:.2}",
                    self.dimension,
                    p.position.x,
                    p.position.y,
                    p.position.z,
                    p.look_dir.y_rot_deg(),
                    p.look_dir.x_rot_deg(),
                );
                if common::set_clipboard(&cmd) {
                    self.debug_feedback("Copied location to clipboard");
                }
                true
            }
            _ if bindings.matches(Action::DebugClearChat, &input) => {
                self.chat.clear_messages();
                true
            }
            _ if bindings.matches(Action::DebugChunkBorders, &input) => {
                self.show_chunk_borders = !self.show_chunk_borders;
                self.debug_feedback(if self.show_chunk_borders {
                    "Chunk borders: shown"
                } else {
                    "Chunk borders: hidden"
                });
                true
            }
            _ if bindings.matches(Action::DebugAdvancedTooltips, &input) => {
                self.advanced_item_tooltips = !self.advanced_item_tooltips;
                self.debug_feedback(if self.advanced_item_tooltips {
                    "Advanced tooltips: shown"
                } else {
                    "Advanced tooltips: hidden"
                });
                true
            }
            _ if bindings.matches(Action::DebugCopyData, &input) => {
                // TODO: entity variant and server-side NBT query (vanilla
                // copyRecreateCommand with addNbt/pullFromServer)
                if let Some(HitResult::Block(t)) = self.interaction.target {
                    let state = self.chunk_store.get_block_state(
                        t.block_pos.x,
                        t.block_pos.y,
                        t.block_pos.z,
                    );
                    let block_state_data = self.block_registry.block_state_data(state);
                    let props = block_state_data
                        .properties()
                        .entries()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join(",");
                    let block = block_state_data.id();
                    let desc = if props.is_empty() {
                        block.to_string()
                    } else {
                        format!("{block}[{props}]")
                    };
                    let cmd = format!(
                        "/setblock {} {} {} {desc}",
                        t.block_pos.x, t.block_pos.y, t.block_pos.z
                    );
                    if common::set_clipboard(&cmd) {
                        self.debug_feedback("Copied client-side block data to clipboard");
                    }
                }
                true
            }
            _ if bindings.matches(Action::DebugGameModeSwitcher, &input) => {
                if let Some(switcher) = &mut self.game_mode_switcher {
                    switcher.cycle();
                } else if self.input_live() {
                    // Sent unconditionally on apply; the server refuses
                    // without permission.
                    // TODO: gate on permission level once pomme tracks it
                    // (vanilla canSwitchGameMode / debug.gamemodes.error).
                    self.game_mode_switcher =
                        Some(crate::ui::game_mode_switcher::GameModeSwitcherState::open(
                            self.player.game_mode,
                            self.previous_game_mode,
                        ));
                }
                true
            }
            _ if bindings.matches(Action::DebugSpectate, &input) => {
                // Sent unconditionally; the server refuses without permission.
                use azalea_core::game_type::GameMode;
                let target = if self.player.game_mode != 3 {
                    GameMode::Spectator
                } else {
                    self.previous_game_mode
                        .and_then(GameMode::from_id)
                        .unwrap_or(GameMode::Creative)
                };
                connection
                    .packet_tx
                    .send(ServerboundGamePacket::ChangeGameMode(
                    azalea_protocol::packets::game::s_change_game_mode::ServerboundChangeGameMode {
                        mode: target,
                    },
                ));
                true
            }
            _ if bindings.matches(Action::DebugChunkOcclusion, &input) => {
                // Pomme-specific: chunk occlusion culling toggle.
                self.chunk_occlusion_enabled = !self.chunk_occlusion_enabled;
                // Force the throttled recompute to run next frame so the
                // toggle takes effect.
                self.vis_valid = false;
                tracing::info!("Chunk occlusion: {}", self.chunk_occlusion_enabled);
                true
            }
            _ if bindings.matches(Action::DebugDumpVersion, &input) => {
                self.debug_feedback("Client version info:");
                self.chat.push_message(vec![crate::ui::text::TextSpan::new(
                    format!("Pomme Client {}", env!("CARGO_PKG_VERSION")),
                    [1.0, 1.0, 1.0, 1.0],
                )]);
                true
            }
            // TODO: F3+F6 debug options screen, F3+P pause-on-lost-focus,
            // F3+S dump dynamic textures, F3+T resource pack reload,
            // F3+L profiler, F3+1..4 debug charts (no backing features)
            _ => false,
        };
        self.f3_chord_consumed |= handled;
        handled
    }

    /// A key's release in game: the overlay toggles on F3 release, unless a
    /// chord consumed it, in every in-game sub-state.
    pub fn on_key_released(
        &mut self,
        code: winit::keyboard::KeyCode,
        input: &crate::app::input::InputState,
        connection: &ConnectionHandle,
    ) {
        let overlay = input.key_bindings.matches_key(Action::ToggleOverlay, code);
        let modifier = input.key_bindings.matches_key(Action::DebugModifier, code);
        if overlay || modifier {
            let f3_held = input.action_held(Action::DebugModifier);
            self.handle_f3_release(overlay, modifier, f3_held, connection);
        }
    }

    /// Settle debug releases once all modifiers are up. Chords suppress the
    /// overlay toggle; releasing a modifier applies an open game-mode switcher.
    pub fn handle_f3_release(
        &mut self,
        overlay: bool,
        modifier: bool,
        modifier_held: bool,
        connection: &ConnectionHandle,
    ) {
        let Some(overlay) =
            debug_overlay_release(&mut self.debug_overlay_pending, overlay, modifier_held)
        else {
            return;
        };
        if modifier && let Some(switcher) = self.game_mode_switcher.take() {
            use azalea_core::game_type::GameMode;
            if switcher.selected != self.player.game_mode
                && let Some(mode) = GameMode::from_id(switcher.selected)
            {
                connection
                    .packet_tx
                    .send(ServerboundGamePacket::ChangeGameMode(
                    azalea_protocol::packets::game::s_change_game_mode::ServerboundChangeGameMode {
                        mode,
                    },
                ));
            }
            self.f3_chord_consumed = false;
            return;
        }
        if overlay && !self.f3_chord_consumed {
            self.show_debug = !self.show_debug;
        }
        if modifier {
            self.f3_chord_consumed = false;
        }
    }

    /// Yellow bold "[Debug]:" prefix plus a plain message, vanilla
    /// `debugFeedback`.
    fn debug_feedback(&mut self, message: &str) {
        use crate::ui::text::TextSpan;
        let yellow = [1.0, 1.0, 85.0 / 255.0, 1.0];
        let mut prefix = TextSpan::new("[Debug]:".into(), yellow);
        prefix.bold = true;
        self.chat.push_message(vec![
            prefix,
            TextSpan::new(" ".into(), [1.0, 1.0, 1.0, 1.0]),
            TextSpan::new(message.into(), [1.0, 1.0, 1.0, 1.0]),
        ]);
    }

    /// Whether the chat or skin options `ClientInformation` carries differ
    /// from the last ones sent.
    fn client_options_changed(
        &self,
        chat_options: crate::ui::chat::ChatOptions,
        skin_options: crate::ui::menu::SkinOptions,
    ) -> bool {
        self.last_chat_visibility != chat_options.visibility
            || self.last_chat_colors != chat_options.colors
            || self.last_skin_options != skin_options
    }

    pub fn sync_client_information(
        &mut self,
        connection: &ConnectionHandle,
        render_distance: u32,
        chat_options: crate::ui::chat::ChatOptions,
        skin_options: crate::ui::menu::SkinOptions,
    ) {
        let render_changed = self.last_render_distance != render_distance;
        let options_changed = self.client_options_changed(chat_options, skin_options);
        self.last_render_distance = render_distance;
        self.last_chat_visibility = chat_options.visibility;
        self.last_chat_colors = chat_options.colors;
        self.last_skin_options = skin_options;
        if render_changed {
            tracing::info!("Render distance changed to {render_distance}");
        }
        if options_changed {
            tracing::info!(
                visibility = ?chat_options.visibility,
                colors = chat_options.colors,
                skin = ?skin_options,
                "Client information options changed"
            );
        }

        connection
            .packet_tx
            .send(ServerboundGamePacket::ClientInformation(
                ServerboundClientInformation {
                    client_information: crate::net::client_information(
                        render_distance as u8,
                        chat_options,
                        skin_options,
                    ),
                },
            ));
    }

    /// Marks a whole column for remeshing as new content.
    pub(crate) fn mark_column_dirty(&mut self, pos: ChunkPos) {
        let full = section_mask(self.chunk_store.section_count());
        self.mesh_columns.mark(pos, full, false);
    }

    /// Drops `pos`'s scheduling state when it unloads, retiring its queued
    /// work.
    pub fn forget_mesh_column(&mut self, pos: ChunkPos) {
        if let Some(retired) = self.mesh_columns.forget(pos) {
            self.mesh_dispatcher.cancel(pos, retired);
        }
    }

    /// [`MeshColumns::first_unmeshed`] over the loaded columns.
    #[cfg(feature = "test-harness")]
    pub fn first_unmeshed_column(&self) -> Option<ChunkPos> {
        self.mesh_columns
            .first_unmeshed(self.chunk_store.loaded_positions())
    }

    /// Forgets every column's scheduling state, for a new level or dispatcher.
    pub fn reset_mesh_columns(&mut self) {
        self.mesh_columns.reset();
    }

    /// Meshes every loaded column again from scratch, after the renderer
    /// dropped its meshes.
    pub fn rebuild_meshes(&mut self) {
        self.reset_mesh_columns();
        self.remesh_all_columns();
        self.section_vis.clear();
        self.section_vis_epoch.clear();
        self.clear_visibility();
        self.vis_task = None;
        self.vis_valid = false;
        self.pending_load_rescan = true;
    }

    /// Marks every loaded column for remeshing (F3+A).
    pub fn remesh_all_columns(&mut self) {
        let loaded: Vec<ChunkPos> = self.chunk_store.loaded_positions().collect();
        for pos in loaded {
            self.mark_column_dirty(pos);
        }
    }

    /// The chunk column the player stands in.
    pub fn player_chunk(&self) -> ChunkPos {
        ChunkPos::new(
            (self.player.position.x as i32).div_euclid(16),
            (self.player.position.z as i32).div_euclid(16),
        )
    }

    /// Takes the light worker's finished light (vanilla `ClientLevel.update`,
    /// called per frame from `Minecraft.runTick`: drain queued light tasks,
    /// then `runLightUpdates`) and turns the resulting dirty scope into remesh
    /// work: columns whose chunk-load light applied remesh whole like chunk
    /// loads, individual lit sections remesh as edits.
    pub fn update_light(&mut self) {
        let mut dirty = crate::world::light::LightDirty::default();
        self.light_engine.poll(&mut self.chunk_store, &mut dirty);
        if dirty.columns.is_empty() && dirty.sections.is_empty() {
            return;
        }
        let mut bumped: Vec<ChunkPos> = Vec::new();
        for &(x, z) in &dirty.columns {
            for p in crate::world::chunk::mesh_neighborhood(ChunkPos::new(x, z)) {
                if self.chunk_store.get_chunk(&p).is_some() && !bumped.contains(&p) {
                    bumped.push(p);
                }
            }
        }
        for &pos in &bumped {
            self.mark_column_dirty(pos);
        }
        if !bumped.is_empty() {
            self.pending_load_rescan = true;
        }
        let min_section_y = self.chunk_store.min_y() >> 4;
        let section_count = self.chunk_store.section_count();
        for key in &dirty.sections {
            let si = key.y - min_section_y;
            let col = ChunkPos::new(key.x, key.z);
            // Padding/out-of-range sections have no mesh; columns already
            // bumped above remesh wholesale anyway.
            if si < 0 || si >= section_count || bumped.contains(&col) {
                continue;
            }
            if self.chunk_store.get_chunk(&col).is_none() {
                continue;
            }
            self.enqueue_section_edit(col, si);
        }
    }

    /// Mark an edited section for remeshing as a recompile, ungated by
    /// visibility.
    pub fn enqueue_section_edit(&mut self, col: ChunkPos, si: i32) {
        self.mesh_columns.mark(col, section_bit(si), true);
    }

    /// Vanilla `compileSync` under `PrioritizeChunkUpdates.PLAYER_AFFECTED`:
    /// mesh and upload a column's player-edited sections on the spot so the
    /// edit shows the same frame. (Vanilla defaults to NONE/async, but
    /// pomme's async round-trip is several frames, which leaves a broken
    /// block visibly lingering after its crack overlay completes.)
    pub fn mesh_sections_edit_now(
        &mut self,
        renderer: &mut Renderer,
        col: ChunkPos,
        sections: std::ops::Range<i32>,
    ) {
        // Older async results for these sections are rejected by the
        // buffer's per-section epoch gate (`ChunkMeshData::upload_epoch`).
        let mesh = self
            .mesh_dispatcher
            .mesh_sections_now(&self.chunk_store, col, sections, 0);
        self.apply_mesh_upload(renderer, mesh);
    }

    /// Collect the frame's ready meshes, apply their CPU-side bookkeeping, then
    /// upload them in one coalesced GPU transfer (one fence wait, not one per
    /// mesh) to avoid the streaming stutter from per-mesh `queue.wait_idle`.
    /// Shared with the loading phase, which streams the spawn chunks in before
    /// the game phase takes over.
    pub fn drain_and_upload_meshes(&mut self, renderer: &mut Renderer) {
        let batch = self.drain_meshes();
        let upload_start = std::time::Instant::now();
        let dropped = renderer.upload_chunk_meshes(&batch);
        self.last_update_phases.upload_ms = upload_start.elapsed().as_secs_f32() * 1000.0;
        crate::frame_log::add("mesh_upload", self.last_update_phases.upload_ms);
        self.clear_dropped_meshed(dropped);
    }

    /// The frame's ready meshes, stale ones dropped and the rest's CPU-side
    /// bookkeeping applied.
    pub fn drain_meshes(&mut self) -> Vec<ChunkMeshData> {
        let drain_start = std::time::Instant::now();
        let results: Vec<_> = self.mesh_dispatcher.drain_results().collect();
        let mut batch = Vec::with_capacity(results.len());
        for mut mesh in results {
            // Stale meshes count too: worker time spent is worker time spent.
            if let Some(bench) = &mut self.chunk_load_bench {
                bench.record_mesh(mesh.queue_ms, mesh.mesh_ms);
            }
            // Drop a mesh for a load of the chunk that has since unloaded
            // (uploading it would resurrect a column nothing cleans up). One
            // from an older generation still uploads: the buffer's epoch gate
            // keeps it from replacing anything newer, and its sections stay
            // dirty for the newer job.
            let stale = self.chunk_store.get_chunk(&mesh.pos).is_none()
                || self.mesh_columns.incarnation(mesh.pos) != Some(mesh.incarnation);
            if stale {
                continue;
            }
            if let Some(t) = &mesh.timing {
                let ms = |d: std::time::Duration| d.as_secs_f32() * 1000.0;
                tracing::trace!(
                    "edit remesh [{}, {}]: queue {:.1}ms + mesh {:.1}ms + drain {:.1}ms = {:.1}ms",
                    mesh.pos.x,
                    mesh.pos.z,
                    ms(t.started_at - t.enqueued_at),
                    ms(t.meshed_at - t.started_at),
                    ms(t.meshed_at.elapsed()),
                    ms(t.enqueued_at.elapsed()),
                );
            }
            // Taken before the upload so the mesh can move into the batch; the
            // upload reports back what it had to drop.
            self.apply_mesh_bookkeeping(&mut mesh);
            batch.push(mesh);
        }
        self.last_update_phases.mesh_drain_ms = drain_start.elapsed().as_secs_f32() * 1000.0;
        crate::frame_log::add("mesh_drain", self.last_update_phases.mesh_drain_ms);
        batch
    }

    /// Vanilla `SectionUpdateTracker.hasAllNeighbors` plus
    /// `LevelRenderer.isSectionCompiledAndVisible`: the section holding
    /// `camera_block` may only have compiled once its column's whole 3x3
    /// neighbourhood was loaded and lit, and it must have a mesh — an empty one
    /// counts, as vanilla's empty `CompiledSectionMesh` does.
    pub fn camera_section_ready(&self, camera_block: glam::IVec3) -> bool {
        let column = ChunkPos::new(camera_block.x >> 4, camera_block.z >> 4);
        let neighbourhood_lit = crate::world::chunk::column_neighborhood(column).all(|p| {
            self.chunk_store.get_chunk(&p).is_some()
                && self.light_engine.light_on_in_column((p.x, p.z))
        });
        let section = (camera_block.y - self.chunk_store.min_y()) >> 4;
        let compiled = self
            .compiled
            .get(&column)
            .is_some_and(|mask| mask & section_bit(section) != 0);
        neighbourhood_lit && compiled
    }

    /// Vanilla `ClientPacketListener.handleLogin`/`handleRespawn`: clear the
    /// loaded flag and start waiting for the new level.
    pub fn start_level_load(&mut self) {
        self.client_loaded = false;
        // TODO: vanilla gives a newly created singleplayer world a 500ms close
        // delay (`Minecraft.doWorldLoad`); Pomme can't tell a fresh world from
        // an opened one yet.
        let mut tracker = LevelLoadTracker::start_client_load(
            std::time::Duration::ZERO,
            std::time::Instant::now(),
        );
        // 1.20.1 and 1.20.2 have no LEVEL_CHUNKS_LOAD_START game event (1.20.4
        // added it), so nothing would ever move the tracker on; those clients
        // had the level from the login packet.
        if crate::version::session_protocol() < 765 {
            tracker.loading_packets_received();
        }
        self.level_load = Some(tracker);
    }

    /// Adopt a finished mesh's CPU-side state: its per-section visibility sets,
    /// epoch-guarded so a stale result can't overwrite a newer edit's
    /// visibility, and the sections it compiled. The upload can still drop a
    /// section afterwards, which `clear_dropped_meshed` takes back out.
    fn apply_mesh_bookkeeping(&mut self, mesh: &mut ChunkMeshData) {
        let pos = mesh.pos;
        for (si, vis) in std::mem::take(&mut mesh.visibility) {
            let e = self.section_vis_epoch.entry((pos, si)).or_insert(0);
            if mesh.upload_epoch >= *e {
                *e = mesh.upload_epoch;
                self.section_vis.insert((pos, si), vis);
            }
        }
        let meshed = section_bits(mesh.replaced.clone());
        self.mesh_columns.meshed(pos, mesh.content_gen, meshed);
        *self.compiled.entry(pos).or_default() |= meshed;
    }

    /// Sections dropped on pool exhaustion were retired from the buffer; mark
    /// them dirty again so the next rescan re-enqueues them, and clear their
    /// compiled bit, since nothing of them reached the GPU.
    fn clear_dropped_meshed(&mut self, dropped: Vec<(ChunkPos, Vec<i32>)>) {
        for (pos, sections) in dropped {
            let retired = section_bits(sections);
            if let Some(mask) = self.compiled.get_mut(&pos) {
                *mask &= !retired;
            }
            self.mesh_columns.remark(pos, retired);
        }
    }

    /// Upload a finished mesh and apply its bookkeeping. The sync edit path;
    /// the frame drain batches uploads instead.
    fn apply_mesh_upload(&mut self, renderer: &mut Renderer, mut mesh: ChunkMeshData) {
        self.apply_mesh_bookkeeping(&mut mesh);
        let dropped = renderer.upload_chunk_meshes(std::slice::from_ref(&mesh));
        self.clear_dropped_meshed(dropped);
    }

    /// Drive the cave-cull occlusion walk: apply a finished async walk to the
    /// per-column draw masks, then schedule the next one on 8-block camera
    /// movement or chunk loads (one at a time, off the main thread — vanilla's
    /// async, movement-gated cadence). The walk is rotation-independent;
    /// frustum culling runs per-frame on the GPU.
    pub fn update_visibility(
        &mut self,
        renderer: &mut Renderer,
        player_chunk: ChunkPos,
        loads_happened: bool,
    ) {
        // Before the camera is placed the frustum is meaningless, so trust
        // nothing and let the queue mesh everything nearest-first.
        if !self.position_set {
            if self.vis_valid {
                self.vis_valid = false;
                self.clear_visibility();
            }
            return;
        }

        // Apply a finished walk (its result lags a few frames, like vanilla's).
        let finished = self.vis_task.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(bfs) = finished {
            self.vis_task = None;
            self.apply_visibility(renderer, &bfs);
        }

        // Schedule the next walk on 8-block movement, chunk loads, or an
        // invalidated result (`!vis_valid`, e.g. the F3+O toggle forcing a
        // recompute while stationary), one in flight.
        let eye = renderer.camera_render_position();
        let cam_bucket = (
            (eye.x / 8.0).floor() as i32,
            (eye.y / 8.0).floor() as i32,
            (eye.z / 8.0).floor() as i32,
        );
        if self.vis_task.is_none()
            && (!self.vis_valid || cam_bucket != self.last_vis_cam || loads_happened)
        {
            self.last_vis_cam = cam_bucket;
            let section_vis = self.section_vis.clone();
            let min_y = self.chunk_store.min_y();
            let n = self.chunk_store.section_count();
            let cam_si = ((eye.y - min_y as f64) / 16.0).floor() as i32;
            // Bound the walk by the actual loaded radius (a server can stream
            // terrain past the client render distance).
            let rd = self
                .chunk_store
                .loaded_positions()
                .map(|p| {
                    (p.x - player_chunk.x)
                        .abs()
                        .max((p.z - player_chunk.z).abs())
                })
                .max()
                .unwrap_or(0);
            let (tx, rx) = crossbeam_channel::bounded(1);
            std::thread::spawn(move || {
                let bfs = occlusion_graph::compute_visible_mask(
                    &section_vis,
                    player_chunk,
                    cam_si,
                    eye,
                    min_y,
                    n,
                    rd,
                );
                let _ = tx.send(bfs);
            });
            self.vis_task = Some(rx);
        }
    }

    /// Drops the last walk's in-view masks and the totals made from them.
    pub(crate) fn clear_visibility(&mut self) {
        self.in_view_masks.clear();
        self.mesh_gate = MeshGate::default();
    }

    /// Combine a finished walk with the current camera frustum into per-column
    /// draw masks (occluded sections omitted), and push them to the GPU cull.
    fn apply_visibility(&mut self, renderer: &mut Renderer, bfs: &HashMap<ChunkPos, u32>) {
        let planes = renderer.frustum_planes();
        let planes_wide = renderer.frustum_planes_dilated(VIS_MARGIN_RADIANS);
        let eye_f = renderer.camera_render_position();
        let min_y = self.chunk_store.min_y() as f32;
        let max_y = min_y + self.chunk_store.height() as f32;
        let full = section_mask(self.chunk_store.section_count());
        let sections = self.chunk_store.section_count() as u32;

        let mut masks = HashMap::new();
        let mut in_view = HashMap::new();
        let mut gate = MeshGate::default();
        for pos in self.chunk_store.loaded_positions() {
            let near = column_is_near(pos, eye_f);
            // Near columns always draw fully; otherwise a column draws only the
            // sections the graph proved occlusion-visible (none => fully hidden).
            let mask = if near {
                full
            } else {
                bfs.get(&pos).copied().unwrap_or(0)
            };
            if mask != 0
                && (near
                    || column_frustum_tier(pos, eye_f, &planes, &planes_wide, min_y, max_y) == 0)
            {
                gate.add(mask, sections);
                in_view.insert(pos, mask);
            }
            masks.insert(pos, mask);
        }
        self.in_view_masks = in_view;
        self.mesh_gate = gate;
        self.vis_valid = true;

        // With occlusion off, push full masks (frustum still applies on the GPU).
        if !self.chunk_occlusion_enabled {
            for m in masks.values_mut() {
                *m = full;
            }
        }
        renderer.set_chunk_visibility(masks);
    }

    /// Publishes every loaded column's dirty sections to the mesh workers,
    /// re-meshing a whole column when the player's movement changes its LOD.
    /// Like vanilla, every section in render distance meshes regardless of
    /// visibility (occlusion gates only drawing), and the queue orders the
    /// backlog nearest-first. Runs every frame, publishing only when the work
    /// or the camera's 8-block bucket changed.
    pub fn rescan_mesh_jobs(&mut self, chunk_detail: u32) {
        // TODO: vanilla won't schedule a section's first compile until its
        // 3x3 column neighbourhood is loaded and lit
        // (`LevelExtractor.java:155` / `SectionUpdateTracker.hasAllNeighbors`);
        // we mesh against missing neighbours as air and repair the borders
        // when their light marks the column dirty.
        let loaded: Vec<ChunkPos> = self.chunk_store.loaded_positions().collect();
        let player_chunk = self.player_chunk();
        let camera = *self.player.position;
        let dispatcher = &self.mesh_dispatcher;
        let requests = self.mesh_columns.requests(
            &loaded,
            camera,
            section_mask(self.chunk_store.section_count()),
            |pos| crate::app::core::chunk_lod(pos, player_chunk, chunk_detail),
            |pos| dispatcher.claimed_generation(pos),
        );
        if let Some(requests) = requests {
            self.mesh_dispatcher
                .publish(&self.chunk_store, camera, requests);
        }
    }
}

/// Extra FOV (radians) for the tier-1 "about to be seen" margin frustum, so
/// small camera turns reveal already-meshed terrain instead of a meshing
/// curtain.
const VIS_MARGIN_RADIANS: f32 = 0.6;

/// Frustum tier for a column: 0 in view, 1 in the dilated margin, 2 behind the
/// camera. (Nearby columns are forced to 0 by the caller.)
fn column_frustum_tier(
    pos: ChunkPos,
    eye: glam::DVec3,
    planes: &[[f32; 4]; 6],
    planes_wide: &[[f32; 4]; 6],
    min_y: f32,
    max_y: f32,
) -> u8 {
    // Camera-relative full-height column box, matching how the GPU cull
    // subtracts the eye before its plane test (cull.comp); f64 first for
    // precision at extreme coordinates.
    let dx = (pos.x as f64 * 16.0 - eye.x) as f32;
    let dz = (pos.z as f64 * 16.0 - eye.z) as f32;
    let mn = [dx, (min_y as f64 - eye.y) as f32, dz];
    let mx = [dx + 16.0, (max_y as f64 - eye.y) as f32, dz + 16.0];
    if aabb_in_frustum(&mn, &mx, planes) {
        0
    } else if aabb_in_frustum(&mn, &mx, planes_wide) {
        1
    } else {
        2
    }
}

/// Bit for one section index, 0 outside a column's 32 addressable sections (a
/// camera outside build height resolves to such an index).
fn section_bit(si: i32) -> u32 {
    if (0..32).contains(&si) { 1u32 << si } else { 0 }
}

/// The bits for several section indices.
pub(crate) fn section_bits(indices: impl IntoIterator<Item = i32>) -> u32 {
    indices
        .into_iter()
        .fold(0u32, |mask, si| mask | section_bit(si))
}

/// Full mask for an `n`-section column (bits `0..n` set).
fn section_mask(n: i32) -> u32 {
    if n >= 32 { u32::MAX } else { (1u32 << n) - 1 }
}

/// Conservative AABB-vs-frustum test (the dominant-corner max-dot used by
/// `cull.comp`): true unless the box is fully behind some plane.
fn aabb_in_frustum(mn: &[f32; 3], mx: &[f32; 3], planes: &[[f32; 4]; 6]) -> bool {
    for p in planes {
        let d = p[0] * if p[0] >= 0.0 { mx[0] } else { mn[0] }
            + p[1] * if p[1] >= 0.0 { mx[1] } else { mn[1] }
            + p[2] * if p[2] >= 0.0 { mx[2] } else { mn[2] }
            + p[3];
        if d < 0.0 {
            return false;
        }
    }
    true
}

pub enum GameUpdateResult {
    None,
    ManualDisconnect,
    Disconnected { reason: String },
}

enum ResultKind {
    Fps,
    ChunkLoad,
}

fn handle_chat_ui_action(
    action: ChatUiAction,
    core: &mut AppCore,
    connection: &ConnectionHandle,
    game: &mut GameState,
) {
    match action {
        ChatUiAction::OpenUrl(url) => {
            let Ok(url) = crate::chat_component::parse_untrusted_url(url) else {
                return;
            };
            crate::util::open_url(&url);
        }
        ChatUiAction::OpenChatSettings => {
            game.chat.close_for_settings();
            core.menu.open_chat_settings(&core.input.key_bindings);
            game.options_from_game = true;
            game.paused = true;
        }
        ChatUiAction::RunCommand(command) => {
            handle_unattended_command(&command, connection, game);
        }
        ChatUiAction::RunCommandUnsigned(command) => {
            connection
                .packet_tx
                .send_raw(crate::net::chat::encode_outbound_command(&command));
        }
        ChatUiAction::Custom { id, payload } => connection.packet_tx.send_custom_click(id, payload),
        // The chat screen stays as the dialog's `previousScreen`.
        ChatUiAction::ShowDialog(dialog) => {
            game.open_server_dialog(crate::ui::server_dialog::DialogReference::Holder(dialog));
        }
    }
}

fn handle_unattended_command(command: &str, connection: &ConnectionHandle, game: &mut GameState) {
    use crate::net::commands::UnattendedCommandCheck;
    use crate::ui::chat::CommandConfirmationKind;

    let command = command.strip_prefix('/').unwrap_or(command);
    let check = game
        .command_tree
        .as_ref()
        .map_or(UnattendedCommandCheck::ParseErrors, |tree| {
            tree.verify_unattended(command)
        });
    match CommandConfirmationKind::for_check(check) {
        Some(kind) => game
            .chat
            .request_command_confirmation(command.to_owned(), kind),
        None => {
            connection
                .packet_tx
                .send_raw(crate::net::chat::encode_outbound_command(command));
            // `setScreen(screenAfterCommand)` re-adds ChatScreen, whose
            // `removed` resets the scroll.
            game.chat.reset_chat_scroll();
        }
    }
}

/// Drops the dialog if the input just handled finished it, then carries out
/// the action it reported.
pub(crate) fn settle_server_dialog(
    action: Option<crate::ui::server_dialog::ServerDialogAction>,
    core: &mut AppCore,
    connection: &ConnectionHandle,
    game: &mut GameState,
) {
    use crate::ui::server_dialog::ServerDialogAction;

    // Vanilla swaps in the after-action screen only where the click event
    // reaches `setScreen` (`DialogScreen.runAction`).
    let (activate, follow_up) = match action {
        None => (false, None),
        // `Screen.clickUrlAction`: the confirm screen replaces the dialog,
        // while opening the link straight away (or chat links being off)
        // leaves the screen alone.
        Some(ServerDialogAction::OpenUrl(url)) => match game.chat.request_open_url(url) {
            Some(action) => (false, Some(action)),
            None => (game.chat.has_pending_modal_prompt(), None),
        },
        // `ClientConfigurationPacketListenerImpl.createDialogAccess`.
        Some(ServerDialogAction::RunCommand(command)) if game.configuring => {
            tracing::warn!(
                "Commands are not supported in configuration phase, trying to run '{command}'"
            );
            (false, None)
        }
        Some(ServerDialogAction::RunCommand(command)) => {
            (true, Some(ChatUiAction::RunCommand(command)))
        }
        Some(ServerDialogAction::Custom { id, payload }) => {
            (true, Some(ChatUiAction::Custom { id, payload }))
        }
        // `showDialog` only warns when the dialog doesn't resolve, leaving the
        // current one up.
        Some(ServerDialogAction::ShowDialog(reference)) => {
            game.open_server_dialog(reference);
            (false, None)
        }
    };
    if activate && let Some(dialog) = game.server_dialog.as_mut() {
        dialog.activate();
    }
    if game
        .server_dialog
        .as_ref()
        .is_some_and(|dialog| dialog.is_finished())
    {
        game.server_dialog = None;
    }
    if let Some(action) = follow_up {
        handle_chat_ui_action(action, core, connection, game);
    }
}

/// The server dialog and the confirm screen a chat or dialog link opens over
/// it, with their clicks settled. The connecting screen shares it for
/// configuration-phase dialogs.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_server_screens(
    elements: &mut Vec<MenuElement>,
    sw: f32,
    sh: f32,
    gs: f32,
    core: &mut AppCore,
    gfx: &Gfx,
    connection: &ConnectionHandle,
    game: &mut GameState,
    // The client tick count, or `None` where the phase runs no game ticks.
    tick: Option<u64>,
    text_events: &[crate::ui::text_edit::TextInputEvent],
) {
    let modal_open = game.chat.has_pending_modal_prompt();
    if let Some(dialog) = game.server_dialog.as_mut() {
        // The dialog types while it is the top screen; a confirm screen over
        // it takes the keyboard instead.
        if !modal_open {
            let fs = common::FONT_SIZE * gs;
            dialog.handle_text_input(text_events, gs, &|s| gfx.renderer.menu_text_width(s, fs));
        }
        let scroll = core.input.consume_menu_scroll();
        if scroll != 0.0 && !modal_open {
            dialog.handle_scroll(scroll);
        }
        // Vanilla's `DialogScreen` keeps `Screen`'s blurred background.
        elements.push(MenuElement::BlurBackground);
        let action = dialog.build(
            elements,
            sw,
            sh,
            gs,
            crate::ui::server_dialog::WidgetInput {
                cursor: core.input.cursor_pos(),
                clicked: core.input.left_just_pressed() && !modal_open,
                held: core.input.left_held() && !modal_open,
                shift: core.input.shift_held(),
                activate: !modal_open
                    && (core.input.enter_pressed()
                        || core.input.key_just_pressed(winit::keyboard::KeyCode::Space)),
                arrow_steps: i32::from(
                    core.input
                        .key_just_pressed(winit::keyboard::KeyCode::ArrowRight),
                ) - i32::from(
                    core.input
                        .key_just_pressed(winit::keyboard::KeyCode::ArrowLeft),
                ),
                tick,
                advanced_tooltips: game.advanced_item_tooltips,
            },
            &|t, s| gfx.renderer.menu_text_width(t, s),
            &|spans, s| gfx.renderer.menu_spans_width(spans, s),
        );
        if dialog.take_click_sound() {
            core.audio.play_ui_click();
        }
        settle_server_dialog(action, core, connection, game);
        core.input.clear_just_pressed_actions();
        core.apply_cursor_grab(&gfx.window, Some(game));
    }

    if game.chat.has_pending_modal_prompt() {
        let cursor = core.input.cursor_pos();
        let clicked = core.input.left_just_pressed();
        // `AbstractButton.onClick` plays the click; this is the same
        // last-frame hit test the modal presses with.
        if clicked && game.chat.hovering_clickable(cursor, false) {
            core.audio.play_ui_click();
        }
        if let Some(action) =
            game.chat
                .build_modal_prompt(elements, sw, sh, gs, cursor, clicked, &|spans, s| {
                    gfx.renderer.menu_spans_width(spans, s)
                })
        {
            handle_chat_ui_action(action, core, connection, game);
        }
        core.input.clear_just_pressed_actions();
        core.apply_cursor_grab(&gfx.window, Some(game));
    }
}

/// A key press while a server dialog is the top screen: Escape cancels it,
/// Tab cycles its text fields, and anything else types. A confirm screen the
/// dialog raised sits above it and answers Escape first.
pub(crate) fn server_dialog_key(
    code: winit::keyboard::KeyCode,
    event: &winit::event::KeyEvent,
    core: &mut AppCore,
    window: &winit::window::Window,
    connection: &ConnectionHandle,
    game: &mut GameState,
) {
    use winit::keyboard::KeyCode;

    if game.chat.has_pending_modal_prompt() {
        // `ConfirmScreen` answers Escape with `accept(false)`, which returns
        // to the screen under it.
        if core.input.key_bindings.matches_key(Action::Close, code) {
            game.escape_chat();
            core.input.clear_action(input::Action::OpenMenu);
            core.apply_cursor_grab(window, Some(game));
        } else {
            core.input.on_menu_key_event(event);
        }
        return;
    }
    match code {
        _ if core.input.key_bindings.matches_key(Action::Close, code) => {
            let action = game
                .server_dialog
                .as_mut()
                .and_then(|dialog| dialog.handle_escape());
            settle_server_dialog(action, core, connection, game);
            core.input.clear_action(input::Action::OpenMenu);
            core.apply_cursor_grab(window, Some(game));
        }
        KeyCode::Tab => {
            if let Some(dialog) = game.server_dialog.as_mut() {
                dialog.handle_tab(core.input.shift_held());
            }
        }
        _ => core.input.on_menu_key_event(event),
    }
}

/// Carry out the button/dismiss action a benchmark result overlay reported,
/// targeting the matching benchmark's result/upload fields.
fn apply_result_action(
    action: common::ResultAction,
    kind: ResultKind,
    status: Option<UploadStatus>,
    json: String,
    core: &mut AppCore,
    gfx: &Gfx,
    game: &mut GameState,
) {
    match action {
        common::ResultAction::StartUpload => {
            let handle = Some(upload_result(&core.tokio_rt, json));
            match kind {
                ResultKind::Fps => game.benchmark_upload = handle,
                ResultKind::ChunkLoad => game.chunk_load_upload = handle,
            }
        }
        common::ResultAction::Recopy => {
            if let Some(UploadStatus::Done { url, .. }) = status {
                common::set_clipboard(&url);
            }
        }
        common::ResultAction::Dismiss => {
            match kind {
                ResultKind::Fps => {
                    game.benchmark_result = None;
                    game.benchmark_upload = None;
                }
                ResultKind::ChunkLoad => {
                    game.chunk_load_result = None;
                    game.chunk_load_upload = None;
                }
            }
            core.apply_cursor_grab(&gfx.window, Some(game));
        }
        common::ResultAction::None => {}
    }
}

/// Starts the chunk-load benchmark at `rd`, from the pause menu or
/// `--chunk-bench`.
pub fn start_chunk_load(
    core: &mut AppCore,
    gfx: &Gfx,
    connection: &ConnectionHandle,
    game: &mut GameState,
    rd: u32,
) {
    game.chunk_load_bench = Some(ChunkLoadBench::new(
        rd,
        game.server_render_distance,
        core.menu.chunk_detail,
        BenchMode::Windowed {
            gpu: gfx.renderer.gpu_name().to_owned(),
            vulkan: gfx.renderer.api_version().to_owned(),
            resolution: (gfx.renderer.screen_width(), gfx.renderer.screen_height()),
        },
        *game.player.position,
    ));
    game.chunk_load_result = None;
    // Drop to the minimum render distance so the server unloads the far
    // chunks; the driver raises it to the target once the reset settles.
    apply_render_distance(core, game, connection, crate::benchmark::CHUNK_LOAD_MIN_RD);
}

/// Set the chunk-load benchmark's render distance and push it to the server
/// as it ramps the distance up and down.
fn apply_render_distance(
    core: &mut AppCore,
    game: &mut GameState,
    connection: &ConnectionHandle,
    rd: u32,
) {
    core.menu.set_bench_render_distance(rd);
    game.sync_client_information(
        connection,
        rd,
        core.menu.chat_options,
        core.menu.skin_options(),
    );
}

/// Puts back the render distance the chunk-load benchmark replaced, on the
/// server too.
fn end_chunk_load(core: &mut AppCore, game: &mut GameState, connection: &ConnectionHandle) {
    if let Some(rd) = core.menu.restore_bench_render_distance() {
        game.sync_client_information(
            connection,
            rd,
            core.menu.chat_options,
            core.menu.skin_options(),
        );
    }
}

/// Vanilla `ScrollWheelHandler.onMouseScroll` on one frame's (x, y) scroll,
/// folded to `BundleMouseActions`' wheel: vertical, else negated horizontal.
fn scroll_wheel(accumulated: &mut (f32, f32), (x, y): (f32, f32)) -> i32 {
    let step = |acc: &mut f32, delta: f32| {
        // `Math.signum`, which is 0 for 0 (Rust's `signum` isn't).
        if *acc != 0.0 && delta.partial_cmp(&0.0) != (*acc).partial_cmp(&0.0) {
            *acc = 0.0;
        }
        *acc += delta;
        let whole = acc.trunc();
        *acc -= whole;
        whole as i32
    };
    let wheel_x = step(&mut accumulated.0, x);
    let wheel_y = step(&mut accumulated.1, y);
    if wheel_y == 0 { -wheel_x } else { wheel_y }
}

/// Vanilla `BundleMouseActions.toggleSelectedBundleItem`: the stack's
/// selection follows `Mutable.toggleSelectedItem`, and the packet always goes.
fn toggle_bundle_selection(
    game: &mut GameState,
    sender: &crate::net::sender::PacketSender,
    menu: i32,
    slot: u16,
    selected: i32,
) {
    let current = game
        .bundle_selection
        .filter(|(m, s, _)| *m == menu && *s == slot)
        .map_or(crate::ui::bundle::NO_SELECTION, |(_, _, i)| i);
    game.bundle_selection =
        (selected >= 0 && selected != current).then_some((menu, slot, selected));
    send_bundle_selection(sender, slot, selected);
}

fn send_bundle_selection(sender: &crate::net::sender::PacketSender, slot: u16, selected: i32) {
    use azalea_protocol::packets::game::s_bundle_item_selected::ServerboundBundleItemSelected;
    sender.send(ServerboundGamePacket::BundleItemSelected(
        ServerboundBundleItemSelected {
            slot_id: i32::from(slot),
            selected_item_index: selected as u32,
        },
    ));
}

/// Vanilla `MultiPlayerGameMode.handleCreativeModeItemAdd`: the creative
/// screen sets an inventory slot directly. Returns whether it was sent, which
/// it never is outside creative.
pub(crate) fn set_creative_slot(
    game: &mut GameState,
    connection: &ConnectionHandle,
    slot_num: u16,
    item: azalea_inventory::ItemStack,
) -> bool {
    if !send_creative_slot(game, connection, slot_num, item.clone()) {
        return false;
    }
    // Optimistic local update; the server echoes via ContainerSetSlot.
    game.player.inventory.set_slot(slot_num as usize, item);
    true
}

/// Vanilla `MultiPlayerGameMode.handleCreativeModeItemDrop`: the creative
/// screen drops a stack into the world through slot -1.
pub(crate) fn drop_creative_item(
    game: &mut GameState,
    connection: &ConnectionHandle,
    item: azalea_inventory::ItemStack,
) -> bool {
    let sent = send_creative_slot(game, connection, u16::MAX, item);
    if sent {
        game.player.drop_spam.increment();
    }
    sent
}

/// `ServerboundSetCreativeModeSlotPacket` for `slot_num`, only in creative;
/// the slot is a short on the wire, so `u16::MAX` is vanilla's -1.
fn send_creative_slot(
    game: &GameState,
    connection: &ConnectionHandle,
    slot_num: u16,
    item: azalea_inventory::ItemStack,
) -> bool {
    use azalea_protocol::packets::game::s_set_creative_mode_slot::ServerboundSetCreativeModeSlot;

    if !crate::player::is_creative(game.player.game_mode) {
        return false;
    }
    connection
        .packet_tx
        .send(ServerboundGamePacket::SetCreativeModeSlot(
            ServerboundSetCreativeModeSlot {
                slot_num,
                item_stack: item,
            },
        ));
    true
}

/// Predict each container click locally (instant UI + drag preview), then send
/// the predicted diff as `HashedStack`es so the server suppresses corrections
/// when the prediction is right (vanilla lockstep).
pub(crate) fn send_container_clicks(
    game: &mut GameState,
    connection: &ConnectionHandle,
    audio: &crate::audio::AudioEngine,
    ops: Vec<azalea_inventory::operations::ClickOperation>,
) {
    use azalea_inventory::ItemStack;
    use azalea_inventory::operations::{
        ClickOperation, QuickCraftClick, QuickCraftKind, QuickCraftStatus,
    };
    use azalea_protocol::packets::game::s_container_click::{
        HashedStack, ServerboundContainerClick,
    };

    use crate::player::menu_click;

    let (container_id, kind, state_id) = match &game.open_container {
        Some(c) => (c.id, c.screen.click_kind(), c.state_id),
        None => (0, ContainerKind::Player, game.inventory_state_id),
    };

    let mut drag_kind = QuickCraftKind::Left;
    let mut drag_slots: Vec<u16> = Vec::new();
    for op in &ops {
        let (changed, carried): (Vec<(u16, ItemStack)>, ItemStack) = match op {
            ClickOperation::QuickCraft(QuickCraftClick {
                kind: qc_kind,
                status,
            }) => match status {
                QuickCraftStatus::Start => {
                    drag_kind = qc_kind.clone();
                    drag_slots.clear();
                    (Vec::new(), game.cursor_item.clone())
                }
                QuickCraftStatus::Add { slot } => {
                    drag_slots.push(*slot);
                    (Vec::new(), game.cursor_item.clone())
                }
                QuickCraftStatus::End => {
                    let (changed, remainder) = menu_click::drag_distribution(
                        kind,
                        game.menu_slots(),
                        &game.cursor_item,
                        &drag_kind,
                        &drag_slots,
                    );
                    for (s, item) in &changed {
                        game.set_menu_slot(*s as usize, item.clone());
                    }
                    game.cursor_item = remainder.clone();
                    (changed, remainder)
                }
            },
            other => {
                let mut cursor = std::mem::take(&mut game.cursor_item);
                let mut bundle = menu_click::BundleClick {
                    selection: game
                        .bundle_selection
                        .filter(|(menu, _, _)| *menu == container_id)
                        .map(|(_, slot, selected)| (slot, selected)),
                    sound: None,
                };
                let had_selection = bundle.selection.is_some();
                let changed = menu_click::apply_click(
                    kind,
                    game.menu_slots(),
                    &mut cursor,
                    other,
                    crate::player::is_creative(game.player.game_mode),
                    &mut bundle,
                );
                if had_selection && bundle.selection.is_none() {
                    game.bundle_selection = None;
                }
                if let Some(sound) = bundle.sound {
                    sound.play(audio, game.player.position);
                }
                game.cursor_item = cursor;
                for (s, item) in &changed {
                    game.set_menu_slot(*s as usize, item.clone());
                }
                (changed, game.cursor_item.clone())
            }
        };

        let mut click = ServerboundContainerClick {
            container_id,
            state_id,
            slot_num: op.slot_num().map(|s| s as i16).unwrap_or(-999),
            button_num: op.button_num(),
            click_type: op.click_type(),
            changed_slots: Default::default(),
            carried_item: HashedStack::from_item_stack(&carried, &game.registries),
        };
        for (s, item) in &changed {
            click
                .changed_slots
                .insert(*s, HashedStack::from_item_stack(item, &game.registries));
        }
        connection
            .packet_tx
            .send(ServerboundGamePacket::ContainerClick(click));
    }
}

fn sleeping_head_pitch_deg(is_sleeping: bool, head_x_rot_deg: f32) -> f32 {
    if is_sleeping { 0.0 } else { head_x_rot_deg }
}

/// What a shader pack reads about the player's surroundings (Iris's
/// `BiomeUniforms` and `blindness`).
fn pack_world(game: &mut GameState) -> PackWorld {
    use crate::renderer::pipelines::weather::{Precip, precipitation_for};
    let registry = crate::net::connection::biome_registry(&game.registries);
    if registry.is_some_and(|r| r.map.len() != game.pack_biome_names.len()) {
        game.pack_biome_names = registry
            .into_iter()
            .flat_map(|r| r.map.keys())
            .map(ToString::to_string)
            .collect();
    }
    // Iris reads them at `blockPosition()`, the feet.
    let feet = game.player.position;
    let [x, y, z] = [feet.x, feet.y, feet.z].map(|c| c.floor() as i32);
    let biome = game.chunk_store.biome_id(x, y, z);
    let climate = game.biome_climate.get(&biome).copied().unwrap_or_default();
    // Iris: a second of fade-out at the end, full while infinite.
    let blindness = game.player.effects.duration("blindness").map_or(0.0, |d| {
        if d < 0 {
            1.0
        } else {
            (d as f32 / 20.0).clamp(0.0, 1.0)
        }
    });
    PackWorld {
        biome: biome as i32,
        biome_names: Arc::clone(&game.pack_biome_names),
        precipitation: match precipitation_for(&climate, y) {
            Precip::None => 0,
            Precip::Rain => 1,
            Precip::Snow => 2,
        },
        temperature: climate.temperature,
        rainfall: climate.downfall,
        blindness,
    }
}

/// Vanilla `GameRenderer.renderItemInHand`: not while asleep, with the HUD
/// hidden (F1), or in spectator.
pub(crate) fn renders_first_person_hand(game: &GameState) -> bool {
    hand_visible(
        game.player.is_sleeping(),
        game.hide_gui,
        game.player.game_mode,
    )
}

fn hand_visible(sleeping: bool, hide_gui: bool, game_mode: u8) -> bool {
    !sleeping && !hide_gui && game_mode != 3
}

fn should_render_local_player(
    benchmark_running: bool,
    first_person: bool,
    sleeping: bool,
    death_animation_finished: bool,
) -> bool {
    !benchmark_running && (!first_person || sleeping) && !death_animation_finished
}

fn eye_lightmap_brightness(game: &GameState) -> f32 {
    let eye = game.player.eye_pos();
    let level = game.chunk_store.max_local_raw_brightness(
        eye.x.floor() as i32,
        eye.y.floor() as i32,
        eye.z.floor() as i32,
        game.sky_darken(),
    );
    crate::world::light::lightmap_brightness(level, game.ambient_light)
}

/// Approximates vanilla's data-driven `equippable.camera_overlay` component
/// check (item components aren't tracked): a carved pumpkin in the head slot.
fn head_is_carved_pumpkin(player: &LocalPlayer) -> bool {
    match player.inventory.slot(crate::player::inventory::ARMOR_START) {
        azalea_inventory::ItemStack::Present(d) => {
            crate::player::inventory::item_resource_name(d.kind) == "carved_pumpkin"
        }
        _ => false,
    }
}

/// Vanilla `Camera.setup`: the third-person camera backs off by the larger of
/// the player's and a living mount's `camera_distance`.
fn detached_distance(own: f32, mount: Option<f32>) -> f32 {
    mount.map_or(own, |mount| own.max(mount))
}

/// Vanilla `LivingEntity.getBedOrientation` for a sleeper's bed.
fn bed_orientation(
    game: &GameState,
    sleeping_pos: Option<crate::position::BlockPos>,
) -> Option<BedDirection> {
    let pos = sleeping_pos?;
    game.block_registry
        .block_state_data(game.chunk_store.get_block_state(pos.x, pos.y, pos.z))
        .bed_direction()
}

/// Vanilla `LivingEntityRenderer`: hurt or dying entities take the red overlay.
fn has_red_overlay(hurt_time: u8, death_time: u32) -> bool {
    hurt_time > 0 || death_time > 0
}

/// Vanilla `LivingEntityRenderState.deathTime`: the clock plus the partial
/// tick, or zero while alive.
fn render_death_time(death_time: u32, partial_tick: f32) -> f32 {
    if death_time > 0 {
        death_time as f32 + partial_tick
    } else {
        0.0
    }
}

/// Vanilla `Hud.tick`: the held-item tooltip timer resets to 40 when the
/// selected item's type or hover name changes, clears when the slot empties,
/// and otherwise counts down.
/// Vanilla `WeatherEffectRenderer.tickRainParticles`: up to 100 splashes a
/// tick (scaled by the rain level squared) on the surface within 10 blocks
/// of the camera where it rains, smoke where it falls on lava, magma or a
/// lit campfire, and every few ticks the rain sound from the last of them.
/// Flies the shulker bullets, wither skulls and llama spit, runs the evoker
/// fangs' bites, and spawns the particles they leave.
fn tick_model_entities(game: &mut GameState, camera: glam::DVec3) {
    use crate::entity::model_entity::Trail;
    use crate::particle::{ParticleLook, ParticleOptions, ServerParticleKind};
    for trail in game.model_entities.tick() {
        let (kind, at, speed) = match trail {
            Trail::EndRod(at) => (ServerParticleKind::EndRod, at, glam::DVec3::ZERO),
            Trail::Smoke(at) => (ServerParticleKind::Smoke, at, glam::DVec3::ZERO),
            Trail::DragonBreath(at) => (ServerParticleKind::DragonBreath, at, glam::DVec3::ZERO),
            Trail::Crit(at, speed) => (ServerParticleKind::Crit, at, speed),
        };
        // A count of zero spawns one, moving at the spread times the speed.
        game.particle_store.add_particles_from_packet(
            kind,
            ParticleOptions::None,
            ParticleLook::None,
            false,
            at,
            speed,
            1.0,
            0,
            camera,
        );
    }
}

fn tick_rain_particles(core: &AppCore, game: &mut GameState, camera: glam::DVec3) {
    use crate::renderer::pipelines::weather::{Precip, precipitation_for};
    let rain = game.sky_state.rain();
    if rain <= 0.0 {
        return;
    }
    let table = game.block_registry.block_state_table();
    let [cx, cy, cz] = [camera.x, camera.y, camera.z].map(|c| c.floor() as i32);
    let min_y = game.chunk_store.min_y();
    let mut last = None;
    for _ in 0..(100.0 * rain * rain) as i32 {
        let x = cx + fastrand::i32(0..21) - 10;
        let z = cz + fastrand::i32(0..21) - 10;
        let surface = game.chunk_store.motion_blocking_height(x, z);
        if surface <= min_y || (surface - cy).abs() > 10 {
            continue;
        }
        let climate = game
            .biome_climate
            .get(&game.chunk_store.biome_id(x, surface, z))
            .copied()
            .unwrap_or_default();
        if precipitation_for(&climate, surface) != Precip::Rain {
            continue;
        }
        let y = surface - 1;
        last = Some((x, y, z));
        let (fx, fz) = (fastrand::f64(), fastrand::f64());
        let data = table.block_state_data(game.chunk_store.get_block_state(x, y, z));
        let shape_top = match crate::physics::block_shape::collision_shape(
            data,
            y,
            &crate::physics::block_shape::CollisionContext::EMPTY,
        ) {
            None => 1.0,
            Some(boxes) => boxes
                .iter()
                .filter(|b| (b[0]..=b[3]).contains(&fx) && (b[2]..=b[5]).contains(&fz))
                .map(|b| b[4])
                .fold(0.0, f64::max),
        };
        let fluid = data.fluid();
        let top = shape_top.max(f64::from(fluid.height()));
        let lit_campfire =
            data.id().ends_with("campfire") && data.properties().get("lit") == Some("true");
        let smoke = fluid.kind == crate::world::block::FluidKind::Lava
            || data.id() == "magma_block"
            || lit_campfire;
        let at = glam::DVec3::new(f64::from(x) + fx, f64::from(y) + top, f64::from(z) + fz);
        game.particle_store.add_rain_splash(at, smoke);
    }
    let Some((x, y, z)) = last else {
        return;
    };
    let ticks = game.rain_sound_time;
    game.rain_sound_time += 1;
    if fastrand::i32(0..3) >= ticks {
        return;
    }
    game.rain_sound_time = 0;
    // Under cover: the muffled sound from above.
    let (event, volume, pitch) =
        if y > cy + 1 && game.chunk_store.motion_blocking_height(cx, cz) > cy {
            ("weather.rain.above", 0.1, 0.5)
        } else {
            ("weather.rain", 0.2, 1.0)
        };
    core.audio.play_world_sound(
        &crate::audio::SoundRef::event(event),
        crate::audio::SoundCategory::Weather as u8,
        Position::new(f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(z) + 0.5),
        volume,
        pitch,
        fastrand::u64(..),
    );
}

fn tick_tool_highlight(core: &AppCore, game: &mut GameState) {
    use azalea_inventory::ItemStack;
    let selected = game
        .player
        .inventory
        .hotbar_slots()
        .get(core.input.selected_slot() as usize)
        .cloned()
        .unwrap_or(ItemStack::Empty);
    match (&selected, &game.last_tool_highlight) {
        (ItemStack::Empty, _) => game.tool_highlight_timer = 0,
        (ItemStack::Present(new), ItemStack::Present(old))
            if new.kind == old.kind
                && crate::ui::common::item_display_name(new)
                    == crate::ui::common::item_display_name(old) =>
        {
            game.tool_highlight_timer = game.tool_highlight_timer.saturating_sub(1);
        }
        _ => game.tool_highlight_timer = 40,
    }
    game.last_tool_highlight = selected;
}

pub fn update_game(
    core: &mut AppCore,
    dt: f32,
    raw_dt: f32,
    gfx: &mut Gfx,
    connection: &ConnectionHandle,
    game: &mut GameState,
) -> GameUpdateResult {
    // Snapshot last frame's phase timings before this frame overwrites them: they
    // align with `raw_dt`, which measures the previous frame's full duration.
    let frame_start = std::time::Instant::now();
    let prev_phases = game.last_update_phases;

    // Position the audio listener at the player's head and push current
    // volumes before draining sound packets this frame.
    let listener_pos = game.player.eye_pos();
    core.audio.set_listener(
        listener_pos,
        game.player.look_dir.y_rot_deg(),
        game.player.look_dir.x_rot_deg(),
    );
    core.audio
        .update_entity_sound_position(game.player.entity_id, game.player.position);
    if core.input.mute_toggle_pressed() {
        let muted = core.menu.toggle_sound_muted(&core.input.key_bindings);
        let text = if muted {
            "Sound muted (Ctrl+M to unmute)"
        } else {
            "Sound on"
        };
        game.action_bar = Some((
            vec![crate::ui::text::TextSpan::new(text.into(), [1.0; 4])],
            game.tick_count,
        ));
    }
    core.audio.set_volumes(core.menu.category_volumes());
    core.audio.set_subtitles_enabled(core.menu.show_subtitles);

    gfx.renderer.set_vsync(core.menu.vsync);
    gfx.renderer
        .set_menu_blur_radius(core.menu.menu_background_blurriness);
    game.chat.set_options(core.menu.chat_options);

    // Vanilla pauseIfInactive: losing OS focus for more than half a second
    // with no screen open pauses the game, which also releases the cursor
    // (otherwise a system overlay like Win-key search opens over a still
    // captured cursor). TODO: F3+P toggle (options.pauseOnLostFocus).
    if core.pause_on_lost_focus
        && core
            .unfocused_since
            .is_some_and(|t| t.elapsed().as_millis() > 500)
        && game.input_live()
        && !game.dead
    {
        game.paused = true;
        game.pause_screen = PauseScreen::Main;
        core.apply_cursor_grab(&gfx.window, Some(game));
    }

    #[cfg(feature = "test-harness")]
    if let Some(code) = core.input.take_harness_release() {
        game.on_key_released(code, &core.input, connection);
    }

    let disconnect_reason =
        core.drain_network_events(connection, None, &mut gfx.renderer, &gfx.window, game);
    if let Some(reason) = disconnect_reason {
        return GameUpdateResult::Disconnected { reason };
    }

    game.chat.tick();
    for mark in game.chat.take_chat_marks() {
        connection.packet_tx.mark_chat(mark);
    }

    game.drain_and_upload_meshes(&mut gfx.renderer);

    // Sky time ticks unconditionally so it keeps flowing in menus.
    core.time_tick_accumulator = (core.time_tick_accumulator + dt).min(1.0);
    while core.time_tick_accumulator >= TICK_RATE {
        game.sky_state.game_time = game.sky_state.game_time.wrapping_add(1);
        game.clocks.tick(game.sky_state.game_time);
        game.sync_day_time();
        game.sample_biomes(gfx.renderer.camera_render_position());
        game.environment_probe.tick(game.resolve_environment());
        core.time_tick_accumulator -= TICK_RATE;
    }

    if game.input_live() && game.chunk_load_bench.is_none() {
        gfx.renderer.update_camera(
            &mut core.input,
            dt,
            core.menu.sensitivity,
            core.menu.invert_mouse_x,
            core.menu.invert_mouse_y,
        );
    }

    // Menus never pause the simulation; tick_physics substitutes neutral input.
    // The whole frame counts, not the clamped `dt`, so a slow frame doesn't
    // lose ticks.
    let ticks = advance_ticks(&mut core.tick_accumulator, raw_dt);
    // Vanilla `Minecraft.runTick` ticks textures once per frame that runs any
    // ticks, before them.
    // TODO: vanilla also skips this while singleplayer is paused and under
    // `/tick freeze` (`isLevelRunningNormally`); pomme tracks neither yet.
    let ticked = ticks > 0;
    let ticks_start = std::time::Instant::now();
    if ticked {
        if game.deterministic {
            gfx.renderer.hold_texture_animations();
        } else {
            gfx.renderer.tick_texture_animations();
        }
    }
    for _ in 0..ticks {
        game.tick_count = game.tick_count.wrapping_add(1);
        if !game.deterministic {
            game.block_light_flicker = tick_block_light_flicker(game.block_light_flicker);
        }
        // Vanilla `Minecraft.tick` order: `gameMode.tick` drives the connection
        // tick (and so the level load tracker) before the level's entities,
        // i.e. before the local player moves or sends anything.
        AppCore::tick_level_load(gfx.renderer.camera_render_position(), connection, game);
        // Vanilla Gui.tick falls back from dead health alone when no screen is
        // open, so death UI/auto-respawn must not depend on PlayerCombatKill.
        let has_screen = game.death_screen_open
            || game.paused
            || game.options_from_game
            || game.gui_open()
            || game.chat.is_open();
        if game.dead && !has_screen {
            match crate::app::core::death_route(game.show_death_screen) {
                crate::app::core::DeathRoute::ShowDeathScreen => {
                    core.open_death_screen(connection, &gfx.window, game, None);
                }
                crate::app::core::DeathRoute::Respawn => core.send_respawn(connection, game),
            }
        }
        let local_player_was_removed = game.dead && game.player.death_animation_finished();
        core.tick_physics(&mut gfx.renderer, connection, game);
        // `LocalPlayer.tick` returns before `super.tick()` until the client has
        // loaded, so the player's own baseTick state waits with it.
        if game.client_loaded && !local_player_was_removed {
            // LivingEntity.baseTick hurt/effects and Player.tick sleep state still
            // run on the tick-20 removal tick, then stop with future entity ticks.
            game.player.tick_hurt();
            game.player.cooldowns.tick();
            let delay = crate::player::interaction::attack_strength_delay(&game.player.attributes);
            let inventory = &game.player.inventory;
            game.hand_equip.tick(
                [
                    &inventory.hotbar_slots()[core.input.selected_slot() as usize],
                    inventory.offhand(),
                ],
                game.interaction.item_swap_scale(delay),
            );
            game.player.effects.tick();
            game.player.tick_confusion();
            game.player.tick_sleep();
        }
        game.item_entity_store
            .tick(&game.chunk_store, game.block_registry.block_state_table());
        game.falling_blocks
            .tick(&game.chunk_store, game.block_registry.block_state_table());
        {
            let living = &game.entity_store.living;
            let positions = &game.entity_positions;
            // The top of the local player's box in its pose and at its
            // scale (vanilla seats passengers there).
            let player = (
                game.player.entity_id,
                game.player.position,
                (game.player.height()
                    * game
                        .player
                        .attributes
                        .value(crate::attribute::AttributeKind::Scale)
                        .unwrap_or(1.0)) as f32,
            );
            game.display_store
                .tick(|vehicle| display_seat(vehicle, living, positions, player));
            // A display's passenger attachment is its 0 x 0 box's top.
            let displays = &game.display_store.displays;
            game.interactions
                .tick(|vehicle| match displays.get(&vehicle) {
                    Some(display) => Some(display.position),
                    None => display_seat(vehicle, living, positions, player),
                });
        }
        if !game.deterministic {
            game.level_runtime.animate_tick(
                &game.block_registry,
                &game.chunk_store,
                *game.player.position,
            );
        }
        if !game.deterministic {
            tick_rain_particles(core, game, gfx.renderer.camera_render_position());
        }
        game.particle_store
            .tick(&game.chunk_store, game.block_registry.block_state_table());
        game.end_crystals.tick();
        // `ClientLevel.tick` runs the sky flash down, then a live bolt's
        // tick lights it again.
        game.sky_flash_time = game.sky_flash_time.saturating_sub(1);
        let lightning = game.lightning.tick();
        if lightning.flash {
            game.sky_flash_time = crate::entity::lightning::SKY_FLASH_TICKS;
        }
        for at in lightning.thunder {
            // `LightningBolt.tick`: thunder heard from afar, and the strike.
            let weather = crate::audio::SoundCategory::Weather as u8;
            let at = Position::new(at.x, at.y, at.z);
            core.audio.play_world_sound(
                &SoundRef::event("entity.lightning_bolt.thunder"),
                weather,
                at,
                10000.0,
                0.8 + fastrand::f32() * 0.2,
                fastrand::u64(..),
            );
            core.audio.play_world_sound(
                &SoundRef::event("entity.lightning_bolt.impact"),
                weather,
                at,
                2.0,
                0.5 + fastrand::f32() * 0.2,
                fastrand::u64(..),
            );
        }
        // A rocket's trail of sparks (`FireworkRocketEntity.tick`).
        for (at, speed) in game.thrown_items.tick() {
            game.particle_store.add_particles_from_packet(
                crate::particle::ServerParticleKind::Firework,
                crate::particle::ParticleOptions::None,
                crate::particle::ParticleLook::None,
                false,
                at,
                speed,
                1.0,
                0,
                gfx.renderer.camera_render_position(),
            );
        }
        tick_model_entities(game, gfx.renderer.camera_render_position());
        update_beacon_beams(game);
        tick_enchanting_books(game);
        tick_spawners(game, gfx.renderer.camera_render_position());
        // `AreaEffectCloud.clientTick`: its particles, shown from any
        // distance (`addAlwaysVisibleParticle`).
        for particle in game.effect_clouds.tick() {
            let look = crate::app::core::particle_look(game, particle.options, particle.position);
            game.particle_store.add_particles_from_packet(
                particle.kind,
                particle.options,
                look,
                true,
                particle.position,
                particle.velocity,
                1.0,
                0,
                gfx.renderer.camera_render_position(),
            );
        }
        game.fishing_hooks.tick();
        // `ExperienceOrb.followNearbyPlayer`: orbs make for the middle of
        // the eye height of a player who isn't spectating.
        let draws_orbs = !crate::player::is_spectator(game.player.game_mode) && !game.dead;
        let orb_target = draws_orbs.then(|| {
            glam::DVec3::from(game.player.position)
                + glam::DVec3::new(0.0, f64::from(game.player.eye_height) / 2.0, 0.0)
        });
        game.experience_orbs.tick(
            &game.chunk_store,
            game.block_registry.block_state_table(),
            orb_target,
        );
        game.arrows
            .tick(crate::physics::collision::CollisionWorld::new(
                &game.chunk_store,
                game.block_registry.block_state_table(),
            ));
        game.block_entity_anim.tick();
        crate::app::core::tick_moving_pistons(game);
        game.title.tick();
        tick_tool_highlight(core, game);
        // LocalPlayer.handlePortalTransitionEffect belongs to aiStep, which is
        // skipped once tickDeath removes the player at exactly death tick 20.
        let local_player_ai_step_ran = !game.dead || !game.player.death_animation_finished();
        let inside_portal = game
            .player
            .is_inside_nether_portal(&game.chunk_store, game.block_registry.block_state_table());
        if local_player_ai_step_ran && game.player.tick_portal_effect(inside_portal) {
            // Vanilla forLocalAmbience: AMBIENT category at the listener,
            // volume 0.25, pitch 0.8..1.2.
            core.audio.play_world_sound(
                &SoundRef::event("block.portal.trigger"),
                CATEGORY_AMBIENT,
                game.player.position,
                0.25,
                fastrand::f32() * 0.4 + 0.8,
                fastrand::u64(..),
            );
        }
        // Vanilla Hud.updateVignetteBrightness: 1%-per-tick smoothing toward
        // the darkness of the eye block's light level.
        let target = (1.0 - eye_lightmap_brightness(game)).clamp(0.0, 1.0);
        game.vignette_brightness += (target - game.vignette_brightness) * 0.01;
        // Vanilla `Hud.tickAutosaveIndicator`.
        game.last_saving_indicator_value = game.saving_indicator_value;
        let target = if gfx.renderer.screenshot_saving() {
            1.0
        } else {
            0.0
        };
        game.saving_indicator_value = game.saving_indicator_value.lerp(target, 0.2);
        if let Some(c) = &mut game.open_container
            && let Some(state) = &mut c.enchant
        {
            state.tick(&c.slots, &c.data);
            // Vanilla `EnchantmentScreen.containerTick` keeps the XP bar
            // prioritized while the screen is open.
            game.xp_display_start_tick = game.tick_count as i64;
        }
        AppCore::send_client_tick_end(connection);
    }
    if ticked {
        gfx.renderer.tick_lightmap(lightmap_info(
            game,
            core.menu.brightness,
            sky_partial_tick(core),
        ));
    }

    crate::frame_log::add("ticks", ticks_start.elapsed().as_secs_f32() * 1000.0);

    // Once per frame after the frame's ticks, where vanilla `Minecraft.runTick`
    // calls `level.update()`.
    let light_start = std::time::Instant::now();
    game.update_light();
    crate::frame_log::add("light", light_start.elapsed().as_secs_f32() * 1000.0);

    // F1 (vanilla keyToggleGui); only while no screen or chat is open.
    if core.input.action_input_just_pressed(Action::ToggleGui) && game.input_live() {
        game.hide_gui = !game.hide_gui;
    }
    if std::mem::take(&mut game.stop_sleeping_requested) {
        core.send_stop_sleeping(connection, game);
    }

    // Vanilla `Gui.tick`: the bed chat follows the sleeping state.
    if !game.player.is_sleeping() && game.chat.on_player_woke_up(game.command_tree.as_deref()) {
        core.apply_cursor_grab(&gfx.window, Some(game));
    }
    if game.player.is_sleeping() && game.input_live() {
        game.chat.open_in_bed(game.command_tree.as_deref());
        core.apply_cursor_grab(&gfx.window, Some(game));
    }
    // TODO: remaining vanilla keybinds with no backing feature yet:
    // L advancements, P social interactions, O friends overlay (in-game),
    // G quick actions, C/X creative saved hotbars, spectator hotbar select.

    // Finished F2 captures announce in chat (vanilla screenshot.success: bare
    // filename, underlined).
    // TODO: vanilla makes the filename a clickable open-file link; pomme chat
    // has no click handling yet.
    use crate::ui::text::TextSpan;
    if crate::frame_log::screenshot_due() {
        gfx.renderer.request_screenshot();
    }
    for result in gfx.renderer.take_screenshot_messages() {
        let spans = match result {
            Ok(name) => {
                let mut file = TextSpan::new(name, common::WHITE);
                file.underline = true;
                vec![
                    TextSpan::new("Saved screenshot as ".into(), common::WHITE),
                    file,
                ]
            }
            Err(err) => vec![TextSpan::new(
                format!("Couldn't save screenshot: {err}"),
                common::WHITE,
            )],
        };
        game.chat.push_message(spans);
    }

    // F3+A: drop every mesh and re-enqueue all loaded columns.
    if game.pending_chunk_reload {
        game.pending_chunk_reload = false;
        game.remesh_all_columns();
        gfx.renderer.clear_chunk_meshes();
        game.vis_valid = false;
        game.pending_load_rescan = true;
    }

    let partial_tick = core.tick_accumulator / TICK_RATE;

    let enter = core.input.enter_pressed();
    let tab = core.input.tab_pressed();
    let shift = core.input.shift_held();
    let up = core.input.up_pressed();
    let down = core.input.down_pressed();
    let page_up = core.input.page_up_pressed();
    let page_down = core.input.page_down_pressed();
    // The ordered key/char stream goes to whichever text consumer owns this
    // frame; menus (in-game options) drain it themselves in build_menu_input.
    let text_events = if game.chat.is_open() || game.wants_text_input() {
        core.input.drain_text_events()
    } else {
        Vec::new()
    };
    let text_sw = gfx.renderer.screen_width() as f32;
    let text_gs = core
        .menu
        .gui_scale(text_sw, gfx.renderer.screen_height() as f32);
    let text_fs = common::FONT_SIZE * text_gs;
    let chat_was_open = game.chat.is_open();
    if game.dialog_open() {
        // The dialog, or the ConfirmScreen over it, replaces ChatScreen and
        // takes its input; `build_server_screens` hands the typing on.
    } else if let Some(msg) = game.chat.handle_key_input(
        &text_events,
        enter,
        tab,
        shift,
        up,
        down,
        page_up,
        page_down,
        text_sw - 4.0 * text_gs,
        &|s| gfx.renderer.menu_text_width(s, text_fs),
        game.command_tree.as_deref(),
    ) {
        core.send_chat_message(connection, msg);
    }
    // Enter closes chat even when there was nothing to send.
    if chat_was_open && !game.chat.is_open() {
        core.apply_cursor_grab(&gfx.window, Some(game));
    }
    if game.server_dialog.is_none() && game.chat.is_open() {
        let scroll = core.input.consume_menu_scroll();
        if scroll != 0.0 {
            game.chat
                .handle_scroll(core.input.cursor_pos(), scroll, shift);
        }
    }
    if let Some((id, command)) = game.chat.take_suggestion_request() {
        connection
            .packet_tx
            .send(ServerboundGamePacket::CommandSuggestion(
                ServerboundCommandSuggestion { id, command },
            ));
    }

    // Chat counts as text capture too, so digits/E/Q/F type instead of acting
    // as game keys (vanilla suppresses KeyMappings while any screen is open).
    core.input.text_capture = game.wants_text_input() || game.chat.is_open();
    core.input.menu_capture = game.gui_open() || game.death_screen_open;
    core.input.spectator = crate::player::is_spectator(game.player.game_mode);
    let faces_shown = (core.input.spectator && game.spectator.is_menu_active())
        || (game.online_mode && core.input.performing_action(input::Action::ViewPlayerList));
    core.sync_game_dynamic_atlas(game, &mut gfx.renderer, faces_shown);

    // The F3+F4 switcher shows the mouse cursor while open.
    let switcher_open = game.game_mode_switcher.is_some();
    if switcher_open != game.switcher_was_open {
        game.switcher_was_open = switcher_open;
        core.apply_cursor_grab(&gfx.window, Some(game));
    }

    let mut close_inventory = false;
    let mut pause_action = PauseAction::None;
    let mut death_action = DeathAction::None;

    let sleeping_direction = bed_orientation(game, game.player.sleeping_pos);
    let is_sleeping = game.player.is_sleeping();
    // `Camera.alignWithEntity`, then the first-person sleeping `move(0, 0.15, 0)`.
    let first_person_lift = if is_sleeping && gfx.renderer.is_first_person() {
        0.15
    } else {
        0.0
    };
    // Vanilla `Camera.setup` follows the camera entity: a spectated mob's
    // eye and view rotation, else the local player's.
    let spectated = game.spectated();
    let camera_pivot = match spectated {
        Some(e) => e.eye_position(partial_tick),
        None => {
            game.player
                .prev_eye_pos()
                .lerp(game.player.eye_pos(), partial_tick as f64)
                + glam::dvec3(0.0, first_person_lift, 0.0)
        }
    };
    gfx.renderer.sync_camera_pos(camera_pivot);
    gfx.renderer
        .set_entity_camera_look(spectated.map(|e| e.view_look(partial_tick)));
    gfx.renderer.set_sleeping_camera_look(is_sleeping.then(|| {
        sleeping_direction
            .map(BedDirection::camera_yaw_deg)
            .unwrap_or(0.0)
    }));
    // Per-frame FOV interpolation; set before the frustum/view-projection reads.
    gfx.renderer.set_render_partial_tick(partial_tick);
    crate::ui::common::set_item_cooldowns(game.player.cooldowns.view(partial_tick));
    gfx.renderer.set_death_time(if game.dead {
        game.player.death_time as f32 + partial_tick
    } else {
        0.0
    });
    gfx.renderer.set_hurt(
        game.player.hurt_time,
        game.player.hurt_dir,
        core.menu.damage_tilt_strength,
    );
    // `GameRenderer.renderLevel`'s spin, at the default screen-effect scale;
    // `confusionAnimationTick` counts client ticks as the game clock does.
    let spin = game
        .player
        .prev_spinning_intensity
        .lerp(game.player.spinning_intensity, partial_tick);
    gfx.renderer.set_confusion(
        spin,
        game.sky_state.game_time as f32 + partial_tick,
        game.player.effects.has_nausea(),
    );
    // Plain lerp (vanilla getInterpolatedWalkDistance); the forward-extrapolating
    // camera variant judders across tick boundaries when per-tick speed varies.
    let bob_walk = game
        .player
        .prev_walk_dist
        .lerp(game.player.walk_dist, partial_tick);
    let bob_amount = game.player.prev_bob.lerp(game.player.bob, partial_tick);
    gfx.renderer.set_view_bob(
        bob_walk,
        bob_amount,
        core.menu.view_bobbing && !game.deterministic,
    );
    gfx.renderer.set_post_effect(game.drawn_spectator_effect());
    gfx.renderer.update_third_person_distance(
        game.block_registry.render_view(),
        camera_pivot,
        &game.chunk_store,
        game.third_person_distance(),
    );
    // Esc cancels a running benchmark.
    if std::mem::take(&mut game.chunk_load_abort) && game.chunk_load_bench.take().is_some() {
        end_chunk_load(core, game, connection);
    }
    // Watch the chunk-load benchmark from straight above, framed to its load
    // radius.
    match &game.chunk_load_bench {
        Some(bench) => {
            let radius = bench.effective_rd().max(1) as f32 * 16.0;
            gfx.renderer.set_top_down_radius(radius);
        }
        None => gfx.renderer.clear_top_down(),
    }

    let sw = gfx.renderer.screen_width() as f32;
    let sh = gfx.renderer.screen_height() as f32;
    let gs = core.menu.gui_scale(sw, sh);
    gfx.renderer.set_gui_scale(gs);

    let mut elements: Vec<MenuElement> = Vec::new();

    let debug = if game.show_debug {
        Some(hud::DebugInfo {
            fps: gfx.fps_counter.display_fps(),
            position: *game.player.position,
            y_rot_deg: gfx.renderer.camera_look_dir().y_rot_deg(),
            x_rot_deg: gfx.renderer.camera_look_dir().x_rot_deg(),
            target_block: game.interaction.target.and_then(|t| {
                let HitResult::Block(t) = t else {
                    return None;
                };
                let state =
                    game.chunk_store
                        .get_block_state(t.block_pos.x, t.block_pos.y, t.block_pos.z);
                let block_state_data = game.block_registry.block_state_data(state);
                let props = block_state_data
                    .properties()
                    .entries()
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect();
                Some((
                    t.block_pos,
                    t.face,
                    block_state_data.id().to_string(),
                    props,
                ))
            }),
            chunk_count: gfx.renderer.loaded_chunk_count(),
            sections_drawn: gfx.renderer.sections_drawn(),
            occlusion_on: game.chunk_occlusion_enabled,
            // The middle slot is unused.
            mesh_gate: game
                .vis_valid
                .then_some((game.mesh_gate.visible, 0, game.mesh_gate.hidden)),
            gpu_name: gfx.renderer.gpu_name(),
            vulkan_version: gfx.renderer.api_version(),
            screen_w: gfx.renderer.screen_width(),
            screen_h: gfx.renderer.screen_height(),
            timings: Some(hud::FrameTimings {
                pace_ms: gfx.renderer.last_timings().pace_ms,
                frame_ms: gfx.renderer.last_timings().frame_ms,
                fence_ms: gfx.renderer.last_timings().fence_ms,
                acquire_ms: gfx.renderer.last_timings().acquire_ms,
                cull_ms: gfx.renderer.last_timings().cull_ms,
                draw_ms: gfx.renderer.last_timings().draw_ms,
                present_ms: gfx.renderer.last_timings().present_ms,
            }),
        })
    } else {
        None
    };
    // The chunk-load benchmark renders a clean top-down view: only terrain, no HUD,
    // entities/player, held item, clouds, or weather — and skipping them also keeps
    // the measured frame times honest.
    let benchmark_running = game.chunk_load_bench.is_some();
    // Underwater overlay (vanilla ScreenEffectRenderer.submitWater): part of
    // the 3D pass in vanilla, so it shows even with the GUI hidden, but not
    // while sleeping.
    if !benchmark_running
        && gfx.renderer.is_first_person()
        && !crate::player::is_spectator(game.player.game_mode)
        && !game.player.is_sleeping()
        && game.player.eyes_in_water
    {
        hud::build_underwater_overlay(
            &mut elements,
            sw,
            sh,
            eye_lightmap_brightness(game),
            game.player.look_dir.y_rot_deg(),
            game.player.look_dir.x_rot_deg(),
        );
    }
    if !benchmark_running && game.hide_gui {
        // F1: vanilla still renders the debug overlay with the GUI hidden.
        if let Some(info) = debug.as_ref() {
            hud::build_debug_overlay(&mut elements, info, gs, &|t, s| {
                gfx.renderer.menu_text_width(t, s)
            });
        }
    } else if !benchmark_running {
        // Vanilla Hud.extractCameraOverlays: vignette, pumpkin, and portal
        // draw under everything else in the HUD.
        let portal_intensity = game
            .player
            .prev_portal_effect_intensity
            .lerp(game.player.portal_effect_intensity, partial_tick);
        hud::build_camera_overlays(
            &mut elements,
            sw,
            sh,
            core.menu.vignette.then_some(game.vignette_brightness),
            gfx.renderer.is_first_person() && head_is_carved_pumpkin(&game.player),
            portal_intensity,
        );
        let is_survival = crate::player::is_survival(game.player.game_mode);
        let air_bubbles = hud::air_bubbles(game.player.air_supply, game.player.eyes_in_water)
            .filter(|_| is_survival);
        // The pop sound only plays while the bubbles render (HUD visible).
        if let Some(bubbles) = &air_bubbles {
            if !game.player.eyes_in_water {
                game.last_bubble_pop_sound_played = 0;
            } else if bubbles.is_popping && game.last_bubble_pop_sound_played != bubbles.popping_pos
            {
                let volume = 0.5 + 0.1 * (bubbles.empty - 3 + 1).max(0) as f32;
                let pitch = 1.0 + 0.1 * (bubbles.empty - 5 + 1).max(0) as f32;
                core.audio.play_world_sound(
                    &SoundRef::event("ui.hud.bubble_pop"),
                    CATEGORY_PLAYERS,
                    game.player.position,
                    volume,
                    pitch,
                    fastrand::u64(..),
                );
                game.last_bubble_pop_sound_played = bubbles.popping_pos;
            }
        }
        // Contextual bar choice (vanilla Hud.nextContextualInfoState): the
        // jump bar takes the slot while controlling a saddled mount, the
        // locator bar while waypoints are tracked; an active jump charge or
        // an XP change within 100 ticks outprioritizes the locator.
        enum BarChoice {
            Jump,
            Xp,
            Locator,
            Empty,
        }
        let can_jump_bar = game.riding_jumpable_vehicle();
        let jump_charge = game.player.jump_riding_scale;
        let xp_prioritized =
            is_survival && game.xp_display_start_tick + 100 > game.tick_count as i64;
        let bar_choice = if game.waypoints.has_waypoints() {
            if can_jump_bar && jump_charge > 0.0 {
                BarChoice::Jump
            } else if xp_prioritized {
                BarChoice::Xp
            } else {
                BarChoice::Locator
            }
        } else if can_jump_bar {
            BarChoice::Jump
        } else if is_survival {
            BarChoice::Xp
        } else {
            BarChoice::Empty
        };
        let show_locator = matches!(bar_choice, BarChoice::Locator);
        let locator_dots = if show_locator {
            let (yaw_deg, pitch_deg) = gfx.renderer.camera_effective_look_deg();
            let cam = crate::world::waypoints::WaypointCamera {
                position: gfx.renderer.camera_render_position(),
                yaw_deg,
                pitch_deg,
                view_rot_proj: gfx.renderer.locator_projection(),
                fov_y_deg: gfx.renderer.camera_fov_degrees(),
            };
            let store = &game.entity_store;
            let entity_eye_pos = |uuid: &uuid::Uuid| {
                store.player_by_uuid(uuid).map(|e| {
                    let feet = e.prev_position.lerp(e.position, partial_tick as f64);
                    // TODO: swimming/gliding eye height needs entity pose data.
                    let eye_height = if e.is_crouching {
                        crate::player::CROUCH_EYE_HEIGHT
                    } else {
                        crate::player::STANDING_EYE_HEIGHT
                    };
                    let block_pos = glam::IVec3::new(
                        e.position.x.floor() as i32,
                        e.position.y.floor() as i32,
                        e.position.z.floor() as i32,
                    );
                    (
                        block_pos,
                        *feet + glam::DVec3::new(0.0, f64::from(eye_height), 0.0),
                    )
                })
            };
            game.waypoints.extract_dots(
                &cam,
                *game.player.position,
                core.user.uuid,
                &entity_eye_pos,
            )
        } else {
            Vec::new()
        };
        let bar = match bar_choice {
            BarChoice::Jump => hud::ContextualBarKind::JumpableVehicle {
                charge: jump_charge,
            },
            BarChoice::Xp => hud::ContextualBarKind::Experience,
            BarChoice::Locator => hud::ContextualBarKind::Locator {
                dots: &locator_dots,
                arrow_frame_1: game.tick_count % 14 >= 10,
            },
            BarChoice::Empty => hud::ContextualBarKind::Empty,
        };
        // Vanilla `renderMaxAttackIndicator`: the picked entity is living (not
        // an interaction or end crystal) and alive, at full charge, and the weapon is
        // slow enough to matter (delay > 5). TODO: vanilla also skips it when
        // the active item's ATTACK_RANGE component says the hit is out of range
        // (spears).
        let delay = crate::player::interaction::attack_strength_delay(&game.player.attributes);
        let scale = game.interaction.attack_strength_scale(delay);
        let show_full = scale >= 1.0
            && delay > 5.0
            && matches!(game.interaction.target, Some(HitResult::Entity(hit))
                if game
                    .entity_store
                    .living
                    .get(&hit.entity_id)
                    .is_some_and(|e| e.health > 0.0));
        let attack = hud::AttackIndicatorState {
            mode: core.menu.attack_indicator,
            scale,
            show_full,
        };
        if crate::player::is_spectator(game.player.game_mode) {
            crate::ui::spectator_menu::build_spectator_menu(
                &mut elements,
                &mut game.spectator,
                &game.tab_list,
                sw,
                sh,
                gs,
                &|t, s| gfx.renderer.menu_text_width(t, s),
            );
        }
        let flash = game.heart_blink.update(
            game.player.health,
            game.tick_count,
            game.player.invulnerable_time > 0,
        );
        let hearts = hud::HeartLook {
            kind: hud::HeartLook::for_player(&game.player.effects, game.player.fully_frozen()),
            hardcore: game.hardcore,
            flash,
            regen_index: game.player.effects.has("regeneration").then(|| {
                let max = game
                    .player
                    .max_health
                    .max(game.player.health.ceil())
                    .max(flash.display_health as f32);
                (game.tick_count % (max + 5.0).ceil() as u64) as i32
            }),
        };
        hud::build_hud(
            &mut elements,
            sw,
            sh,
            core.input.selected_slot(),
            game.player.health,
            game.player.absorption,
            game.player.max_health,
            game.player.food,
            game.player.armor,
            air_bubbles,
            game.player.eyes_in_water,
            game.vehicle_health(),
            game.tick_count,
            game.player.experience_level,
            game.player.experience_progress,
            bar,
            game.player.game_mode,
            game.player.inventory.hotbar_slots(),
            game.player.inventory.offhand(),
            core.menu.main_hand_right(),
            game.tool_highlight_timer,
            game.action_bar
                .as_ref()
                .map(|(spans, tick)| (spans.as_slice(), game.tick_count.wrapping_sub(*tick))),
            &|spans, s| gfx.renderer.menu_spans_width(spans, s),
            &game.scoreboard,
            &game.player.effects,
            &hearts,
            &game.boss_bars,
            gfx.renderer.is_first_person(),
            debug.as_ref(),
            gs,
            &attack,
            &|t, s| gfx.renderer.menu_text_width(t, s),
        );
        if core.input.touch().seen() && game.world_has_pointer() {
            let layout = input::touch::TouchLayout {
                width: sw,
                height: sh,
                gs,
            };
            crate::ui::touch_controls::build_touch_controls(
                &mut elements,
                &layout,
                core.input.touch(),
            );
        }
    }

    // Vanilla Hud.extractSleepOverlay sits outside the isHidden gate: above
    // the hotbar/effects/boss bar, below chat and the tab list.
    // TODO: vanilla draws the scoreboard sidebar, action bar, and nameplates
    // above the fade; here they dim under it (build_hud bundles the first two).
    if !benchmark_running {
        hud::build_sleep_overlay(&mut elements, sw, sh, game.player.sleep_counter);
    }

    if core.input.performing_action(input::Action::ViewPlayerList)
        && !game.hide_gui
        && !game.paused
        && !game.gui_open()
        && !game.chat.is_open()
        && !game.dead
        && !game.death_screen_open
    {
        let r = &gfx.renderer;
        crate::ui::player_tab::build_player_tab_overlay(
            &mut elements,
            sw,
            &game.tab_list,
            &game.scoreboard,
            game.online_mode,
            gs,
            &|spans, s| r.menu_spans_width(spans, s),
        );
    }

    if let Some(switcher) = &mut game.game_mode_switcher {
        crate::ui::game_mode_switcher::build_game_mode_switcher(
            &mut elements,
            switcher,
            sw,
            sh,
            core.input.cursor_pos(),
            gs,
        );
    }

    if let Some(ref mut bench) = game.benchmark {
        let entity_count = game.entity_store.living.len() as u32;
        let done = bench.record_frame(
            raw_dt * 1000.0,
            gfx.renderer.last_timings(),
            gfx.renderer.loaded_chunk_count(),
            entity_count,
        );
        let progress = bench.progress();
        elements.push(MenuElement::Rect {
            x: sw * 0.25,
            y: 16.0,
            w: sw * 0.5,
            h: 8.0,
            corner_radius: 4.0,
            color: [1.0, 1.0, 1.0, 0.1],
        });
        elements.push(MenuElement::Rect {
            x: sw * 0.25,
            y: 16.0,
            w: sw * 0.5 * progress,
            h: 8.0,
            corner_radius: 4.0,
            color: [0.294, 0.871, 0.498, 0.8],
        });
        elements.push(MenuElement::Text {
            x: sw / 2.0,
            y: 28.0,
            text: format!("Benchmarking... {:.0}%", progress * 100.0),
            scale: 8.0 * gs,
            color: [1.0, 1.0, 1.0, 1.0],
            centered: true,
        });
        if done {
            let bench = game.benchmark.take().unwrap();
            game.benchmark_result = Some(bench.finish(&core.data_dirs.game_dir));
            game.benchmark_upload = None;
            core.apply_cursor_grab(&gfx.window, Some(game));
        }
    }

    if let Some(ref result) = game.benchmark_result {
        let lines = [
            format!("GPU: {}", result.gpu),
            format!(
                "{}x{} / RD {} / {} chunks / {} entities",
                result.resolution[0],
                result.resolution[1],
                result.render_distance,
                result.peak_chunk_count,
                result.peak_entity_count,
            ),
            format!("Avg FPS: {:.0}", result.avg_fps),
            format!("Min: {:.0} / Max: {:.0}", result.min_fps, result.max_fps),
            format!(
                "Frame: {:.2}ms / P1: {:.2}ms / P99: {:.2}ms",
                result.avg_frame_ms, result.p1_frame_ms, result.p99_frame_ms
            ),
            format!(
                "Fence: {:.2}ms / Cull: {:.2}ms / Draw: {:.2}ms",
                result.avg_fence_ms, result.avg_cull_ms, result.avg_draw_ms
            ),
            format!(
                "{} spikes (>{:.0}ms) - Saved to benchmark.json",
                result.spike_count, 8.0
            ),
        ];
        let json = serde_json::to_string_pretty(result).unwrap_or_default();
        let status = game
            .benchmark_upload
            .as_ref()
            .map(|h| h.lock().unwrap().clone());
        let action = common::push_results_overlay(
            &mut elements,
            sw,
            sh,
            gs,
            sh / 2.0 - 90.0,
            "Benchmark Complete",
            &lines,
            status.as_ref(),
            core.input.cursor_pos(),
            core.input.left_just_pressed(),
            core.input.escape_pressed(),
        );
        apply_result_action(action, ResultKind::Fps, status, json, core, gfx, game);
    }

    if let Some(mut bench) = game.chunk_load_bench.take() {
        let count = gfx.renderer.loaded_chunk_count();
        match bench.update(
            count,
            raw_dt * 1000.0,
            gfx.renderer.last_timings(),
            prev_phases,
        ) {
            ChunkLoadStep::Wait => {
                game.chunk_load_bench = Some(bench);
            }
            ChunkLoadStep::Load(rd) => {
                apply_render_distance(core, game, connection, rd);
                game.chunk_load_bench = Some(bench);
            }
            ChunkLoadStep::Done(result) => {
                end_chunk_load(core, game, connection);
                result.save(&core.data_dirs.game_dir);
                game.chunk_load_result = Some(*result);
                game.chunk_load_upload = None;
                core.apply_cursor_grab(&gfx.window, Some(game));
            }
        }
    }

    if let Some(ref bench) = game.chunk_load_bench {
        let progress = format!("run {}/{}", bench.current_run(), bench.total_runs());
        let label = if bench.resetting() {
            format!("Resetting world... ({progress})")
        } else {
            format!(
                "Loading RD {}... {} chunks ({progress})",
                bench.target_rd(),
                bench.loaded()
            )
        };
        elements.push(MenuElement::Text {
            x: sw / 2.0,
            y: 28.0,
            text: label,
            scale: 8.0 * gs,
            color: [1.0, 1.0, 1.0, 1.0],
            centered: true,
        });
    }

    if let Some(ref result) = game.chunk_load_result {
        let rd_line = if result.effective_rd != result.target_rd {
            format!(
                "Render Distance: {} (server-capped to {})",
                result.target_rd, result.effective_rd
            )
        } else if result.achieved_rd < result.target_rd {
            format!(
                "Render Distance: {} (server loaded ~{})",
                result.target_rd, result.achieved_rd
            )
        } else {
            format!("Render Distance: {}", result.target_rd)
        };
        let mut lines = vec![
            rd_line,
            format!(
                "Loaded {} chunks in {:.2}s (avg of {} runs)",
                result.chunk_count, result.load_secs, result.runs
            ),
            format!(
                "{:.0} chunks/sec - first chunk in {:.2}s",
                result.chunks_per_sec, result.time_to_first_secs
            ),
            format!(
                "Frame while loading: avg {:.1}ms / worst {:.1}ms",
                result.avg_frame_ms, result.worst_frame_ms
            ),
            format!("GPU: {} / Vulkan {}", result.gpu, result.vulkan),
            format!(
                "{} {} / {} threads / v{} / {}x{}",
                result.os,
                result.arch,
                result.cpu_threads,
                result.version,
                result.resolution[0],
                result.resolution[1],
            ),
            "Saved to chunk_load.json".to_string(),
        ];
        if crate::benchmark::is_debug_build() {
            lines.push("Debug build - frame times are not representative".to_string());
        }
        let json = serde_json::to_string_pretty(result).unwrap_or_default();
        let status = game
            .chunk_load_upload
            .as_ref()
            .map(|h| h.lock().unwrap().clone());
        let action = common::push_results_overlay(
            &mut elements,
            sw,
            sh,
            gs,
            sh / 2.0 - 100.0,
            "Chunk Load Complete",
            &lines,
            status.as_ref(),
            core.input.cursor_pos(),
            core.input.left_just_pressed(),
            core.input.escape_pressed(),
        );
        apply_result_action(action, ResultKind::ChunkLoad, status, json, core, gfx, game);
    }

    // A dialog is the top screen: the screens under it keep their state
    // (vanilla's `previousScreen`) but neither draw nor take input. The Hud
    // still draws, so chat keeps its unfocused backlog.
    let dialog_open = game.dialog_open();
    if game.options_from_game && !dialog_open {
        core.menu.server_render_distance = game.server_render_distance;
        let mut menu_input = core.build_menu_input(dt);
        // Chat consumed the enter/tab latches earlier this frame; hand them on.
        if !menu_input.binding_capture {
            menu_input.enter = enter;
            menu_input.tab = tab;
        }
        let r = &gfx.renderer;
        let result = core
            .menu
            .build(sw, sh, &menu_input, &mut core.input.key_bindings, |t, s| {
                r.menu_text_width(t, s)
            });
        elements.extend(result.elements);
        core.input.clear_just_pressed_actions();
        core.sync_display_mode(&gfx.window);
    } else if game.death_screen_open && !dialog_open {
        let cursor = core.input.cursor_pos();
        let clicked = core.input.left_just_pressed() && !game.respawn_sent;
        death_action = if game.death_confirm {
            death::build_death_confirm(
                &mut elements,
                sw,
                sh,
                cursor,
                clicked,
                gs,
                death::buttons_ready(game.death_confirm_ticks),
            )
        } else {
            let buttons_enabled =
                !game.respawn_sent && death::buttons_ready(game.death_screen_ticks);
            let r = &gfx.renderer;
            death::build_death_screen(
                &mut elements,
                sw,
                sh,
                cursor,
                clicked,
                gs,
                &game.death_message,
                game.player.score,
                game.hardcore,
                buttons_enabled,
                &|t, s| r.menu_text_width(t, s),
            )
        };
        core.input.clear_just_pressed_actions();
    } else if game.paused && !matches!(game.pause_screen, PauseScreen::Hidden) && !dialog_open {
        let cursor = core.input.cursor_pos();
        let clicked = core.input.left_just_pressed();
        // Vanilla `PauseScreen.extractBackground`: blurred while the menu
        // shows; F3+Esc's hidden pause draws nothing.
        elements.push(MenuElement::BlurBackground);
        pause_action = pause::build_pause_menu(
            &mut elements,
            sw,
            sh,
            cursor,
            clicked,
            gs,
            game.pause_screen,
            game.server_render_distance,
            game.singleplayer,
        );
        core.input.clear_just_pressed_actions();
    }

    let mut player_preview = None;
    let mut book_preview = None;
    if (game.inventory_open || game.open_container.is_some()) && !dialog_open {
        let cursor = core.input.cursor_pos();
        let raw_left = core.input.left_just_pressed();
        let raw_right = core.input.right_just_pressed();
        let recipe_context = if game.inventory_open {
            Some((
                crate::ui::recipe_book::RecipeBookScreenSpec::player(0),
                game.player.inventory.slots().to_vec(),
            ))
        } else {
            game.open_container
                .as_ref()
                .and_then(|container| match container.screen {
                    ContainerScreen::CraftingTable => Some((
                        crate::ui::recipe_book::RecipeBookScreenSpec::crafting_table(container.id),
                        container.slots.clone(),
                    )),
                    ContainerScreen::Furnace(variant) => Some((
                        crate::ui::recipe_book::RecipeBookScreenSpec::furnace(
                            container.id,
                            variant.recipe_book_type(),
                        ),
                        container.slots.clone(),
                    )),
                    _ => None,
                })
        };
        if recipe_context.is_none() && game.recipe_book_ui.has_screen() {
            game.recipe_book_ui
                .reset_for_closed_screen(&mut game.recipe_book);
        }
        let recipe_selection_key = !game.recipe_book_ui.captures_typing()
            && (core.input.key_just_pressed(winit::keyboard::KeyCode::Space)
                || core.input.key_just_pressed(winit::keyboard::KeyCode::Enter)
                || core
                    .input
                    .key_just_pressed(winit::keyboard::KeyCode::NumpadEnter));
        let recipe_frame = recipe_context.as_ref().map(|(spec, slots)| {
            crate::ui::recipe_book::handle_input(
                &mut game.recipe_book_ui,
                &mut game.recipe_book,
                &connection.packet_tx,
                *spec,
                sw,
                sh,
                gs,
                cursor,
                raw_left,
                raw_right,
                core.input.shift_held(),
                recipe_selection_key,
                &text_events,
                slots,
                &|t, s| gfx.renderer.menu_text_width(t, s),
            )
        });
        let book_blocks_pointer = recipe_context
            .as_ref()
            .zip(recipe_frame.as_ref())
            .is_some_and(|((spec, _), frame)| {
                frame.consumed_left_click
                    || frame.consumed_right_click
                    || crate::ui::recipe_book::click_hits_book(
                        &game.recipe_book,
                        spec.kind,
                        sw,
                        sh,
                        gs,
                        cursor,
                    )
            });
        // Key shortcuts stay quiet while a text field (anvil rename or recipe
        // search) types.
        let keys_live = !game.wants_text_input();
        let menu_scroll = core.input.consume_menu_scroll_xy();
        let input = crate::ui::container::ContainerInput {
            left_pressed: raw_left && !book_blocks_pointer,
            right_pressed: raw_right && !book_blocks_pointer,
            left_held: core.input.left_held() && !book_blocks_pointer,
            right_held: core.input.right_held() && !book_blocks_pointer,
            middle_pressed: core
                .input
                .key_bindings
                .inputs_for_action(Action::PickBlock)
                .any(|key| {
                    (keys_live || !matches!(key, InputKey::Keyboard(_)))
                        && core.input.input_just_pressed(key)
                })
                && !book_blocks_pointer,
            shift: core.input.shift_held(),
            hotbar_swap: keys_live
                .then(|| core.input.hotbar_key_just_pressed())
                .flatten(),
            swap_offhand: keys_live && core.input.action_input_just_pressed(Action::SwapOffhand),
            throw: keys_live && core.input.action_input_just_pressed(Action::DropItem),
            throw_all: core.input.ctrl_held(),
        };
        // The anvil rename field consumes this frame's typing; a changed
        // accepted name goes to the server (vanilla `onNameChanged`).
        if let Some(c) = &mut game.open_container
            && let Some(state) = &mut c.anvil
            && let Some(name) =
                crate::ui::anvil::update_rename(state, &c.slots, &text_events, &|s| {
                    gfx.renderer.menu_text_width(s, common::FONT_SIZE)
                })
        {
            use azalea_protocol::packets::game::s_rename_item::ServerboundRenameItem;
            connection
                .packet_tx
                .send(ServerboundGamePacket::RenameItem(ServerboundRenameItem {
                    name,
                }));
        }
        let main_x_offset = recipe_frame.map_or(0.0, |frame| frame.main_x_offset);
        let recipe_book_visible = recipe_frame.is_some_and(|frame| frame.visible);
        let hide_main_for_book = recipe_frame.is_some_and(|frame| frame.visible && frame.narrow);
        let mut deferred_recipe_cursor = azalea_inventory::ItemStack::Empty;
        let (clicked_outside, ops, hovered) = if hide_main_for_book {
            // `AbstractRecipeBookScreen`: a narrow screen with the book open
            // shows only the screen's background behind it, and the book
            // takes every click.
            let cursor = core.input.cursor_pos();
            match game.open_container.as_ref().map(|c| (c.screen, &c.data)) {
                Some((ContainerScreen::CraftingTable, _)) => {
                    crate::ui::crafting_table::push_crafting_table_background(
                        &mut elements,
                        sw,
                        sh,
                        gs,
                        0.0,
                    );
                }
                Some((ContainerScreen::Furnace(variant), data)) => {
                    crate::ui::furnace::push_furnace_background(
                        &mut elements,
                        sw,
                        sh,
                        gs,
                        variant,
                        data,
                        0.0,
                    );
                }
                _ => {
                    let (_, preview) = crate::ui::inventory::push_inventory_background(
                        &mut elements,
                        sw,
                        sh,
                        gs,
                        0.0,
                        cursor,
                        &game.player.inventory,
                    );
                    player_preview = Some(preview);
                }
            }
            (false, Vec::new(), None)
        } else if let Some(container) = &game.open_container {
            let result = match container.screen {
                ContainerScreen::CraftingTable => crate::ui::crafting_table::build_crafting_table(
                    &mut elements,
                    sw,
                    sh,
                    core.input.cursor_pos(),
                    &input,
                    &container.slots,
                    &container.title,
                    &game.cursor_item,
                    &mut game.inv_drag,
                    &mut game.inv_last_click,
                    gs,
                    main_x_offset,
                    recipe_book_visible.then_some(&mut deferred_recipe_cursor),
                ),
                ContainerScreen::Furnace(variant) => crate::ui::furnace::build_furnace(
                    &mut elements,
                    sw,
                    sh,
                    core.input.cursor_pos(),
                    &input,
                    variant,
                    &container.slots,
                    &container.data,
                    &container.title,
                    &game.cursor_item,
                    &mut game.inv_drag,
                    &mut game.inv_last_click,
                    gs,
                    main_x_offset,
                    recipe_book_visible.then_some(&mut deferred_recipe_cursor),
                    &|t, s| gfx.renderer.menu_text_width(t, s),
                ),
                ContainerScreen::Chest { rows } => crate::ui::chest::build_chest(
                    &mut elements,
                    sw,
                    sh,
                    core.input.cursor_pos(),
                    &input,
                    rows,
                    &container.slots,
                    &container.title,
                    &game.cursor_item,
                    &mut game.inv_drag,
                    &mut game.inv_last_click,
                    gs,
                ),
                ContainerScreen::ShulkerBox => crate::ui::chest::build_shulker_box(
                    &mut elements,
                    sw,
                    sh,
                    core.input.cursor_pos(),
                    &input,
                    &container.slots,
                    &container.title,
                    &game.cursor_item,
                    &mut game.inv_drag,
                    &mut game.inv_last_click,
                    gs,
                ),
                ContainerScreen::Anvil => crate::ui::anvil::build_anvil(
                    &mut elements,
                    sw,
                    sh,
                    core.input.cursor_pos(),
                    &input,
                    &container.slots,
                    &container.data,
                    &container.title,
                    container.anvil.as_ref().expect("anvil screen has state"),
                    game.player.experience_level,
                    crate::player::is_creative(game.player.game_mode),
                    &game.cursor_item,
                    &mut game.inv_drag,
                    &mut game.inv_last_click,
                    gs,
                    &|t, s| gfx.renderer.menu_text_width(t, s),
                ),
                ContainerScreen::Enchantment => {
                    let result = crate::ui::enchantment::build_enchantment(
                        &mut elements,
                        sw,
                        sh,
                        core.input.cursor_pos(),
                        &input,
                        &container.slots,
                        &container.data,
                        &container.title,
                        container
                            .enchant
                            .as_ref()
                            .expect("enchantment screen has state"),
                        partial_tick,
                        &game.registries,
                        game.player.experience_level,
                        crate::player::is_creative(game.player.game_mode),
                        &game.cursor_item,
                        &mut game.inv_drag,
                        &mut game.inv_last_click,
                        gs,
                        &|t, s| gfx.renderer.menu_text_width(t, s),
                        &|t, s| gfx.renderer.menu_text_width_sga(t, s),
                    );
                    book_preview = Some(result.book);
                    result.container
                }
            };
            if let Some(button_id) = result.button {
                use azalea_protocol::packets::game::s_container_button_click::ServerboundContainerButtonClick;
                connection
                    .packet_tx
                    .send(ServerboundGamePacket::ContainerButtonClick(
                        ServerboundContainerButtonClick {
                            container_id: container.id,
                            button_id,
                        },
                    ));
            }
            (result.clicked_outside, result.ops, result.hovered)
        } else {
            let result = crate::ui::inventory::build_inventory(
                &mut elements,
                sw,
                sh,
                core.input.cursor_pos(),
                &input,
                &game.player.inventory,
                &game.cursor_item,
                &mut game.inv_drag,
                &mut game.inv_last_click,
                gs,
                main_x_offset,
                recipe_book_visible.then_some(&mut deferred_recipe_cursor),
            );
            player_preview = Some(result.player_preview);
            (result.clicked_outside, result.ops, result.hovered)
        };

        if let Some((spec, _)) = recipe_context.as_ref()
            && ops
                .iter()
                .filter_map(|op| op.slot_num())
                .any(|slot| spec.crafting_slots.contains(&(slot as usize)))
        {
            game.recipe_book_ui
                .crafting_slot_clicked(&mut game.recipe_book);
        }

        if let (Some((spec, _)), Some(frame)) = (recipe_context.as_ref(), recipe_frame) {
            crate::ui::recipe_book::render(
                &mut elements,
                &mut game.recipe_book_ui,
                &game.recipe_book,
                *spec,
                frame,
                sw,
                sh,
                gs,
                cursor,
                core.input.ctrl_held(),
                &|t, s| gfx.renderer.menu_text_width(t, s),
            );
            if frame.visible {
                let shown_cursor = if hide_main_for_book {
                    &game.cursor_item
                } else {
                    &deferred_recipe_cursor
                };
                crate::ui::container::push_cursor_stack(
                    &mut elements,
                    cursor,
                    frame.panel_scale,
                    shown_cursor,
                );
            }
        }

        close_inventory = clicked_outside;

        let menu_id = game.open_container.as_ref().map_or(0, |c| c.id);
        let is_bundle_slot = |game: &GameState, slot: u16| {
            game.menu_slots()
                .get(usize::from(slot))
                .and_then(azalea_inventory::ItemStack::as_present)
                .is_some_and(crate::ui::bundle::is_bundle)
        };
        // Vanilla `onMouseClickAction`: a shift-click or number-key swap on a
        // bundle unselects it before the click goes out.
        for op in &ops {
            use azalea_inventory::operations::{ClickOperation, QuickMoveClick};
            let slot = match op {
                ClickOperation::QuickMove(
                    QuickMoveClick::Left { slot } | QuickMoveClick::Right { slot },
                ) => *slot,
                ClickOperation::Swap(click) => click.source_slot,
                _ => continue,
            };
            if is_bundle_slot(game, slot) {
                toggle_bundle_selection(
                    game,
                    &connection.packet_tx,
                    menu_id,
                    slot,
                    crate::ui::bundle::NO_SELECTION,
                );
            }
        }
        send_container_clicks(game, connection, &core.audio, ops);

        // Vanilla `extractContents`: after the clicks, leaving a slot that
        // holds a bundle unselects it (`onStopHovering`).
        if let Some((menu, slot, _)) = game.bundle_hovered
            && menu == menu_id
            && Some(slot) != hovered
            && is_bundle_slot(game, slot)
        {
            toggle_bundle_selection(
                game,
                &connection.packet_tx,
                menu_id,
                slot,
                crate::ui::bundle::NO_SELECTION,
            );
        }
        game.bundle_hovered = hovered.map(|slot| (menu_id, slot, is_bundle_slot(game, slot)));

        if let Some((_, slot, true)) = game.bundle_hovered {
            let data = game.menu_slots()[usize::from(slot)]
                .as_present()
                .cloned()
                .expect("a bundle slot holds a stack");
            let shown = crate::ui::bundle::contents(&data)
                .map_or(0, |c| crate::ui::bundle::shown_count(c.items.len()));
            let current = game
                .bundle_selection
                .filter(|(m, s, _)| *m == menu_id && *s == slot)
                .map_or(crate::ui::bundle::NO_SELECTION, |(_, _, i)| i);
            // `BundleMouseActions.onMouseScrolled`.
            // TODO: vanilla steps once per wheel event, scaled by
            // `mouseWheelSensitivity`; the input layer sums a frame's events.
            if shown > 0 && menu_scroll != (0.0, 0.0) {
                let wheel = scroll_wheel(&mut game.bundle_scroll, menu_scroll);
                let next = crate::ui::bundle::next_selection(wheel, current, shown);
                if wheel != 0 && next != current {
                    toggle_bundle_selection(game, &connection.packet_tx, menu_id, slot, next);
                }
            }
            let selected = game
                .bundle_selection
                .filter(|(m, s, _)| *m == menu_id && *s == slot)
                .map_or(crate::ui::bundle::NO_SELECTION, |(_, _, i)| i);
            crate::ui::bundle::push_selected_icon(&mut elements, &data, selected);
            crate::ui::bundle::push_tooltip(
                &mut elements,
                &data,
                selected,
                core.input.cursor_pos(),
                sw,
                sh,
                gs,
            );
        }
        core.input.clear_just_pressed_actions();
    }

    // TODO: vanilla's creative screen inherits `BundleMouseActions` (scroll
    // selection, the bundle tooltip); only survival containers have them here.
    if game.creative_inventory_open && !dialog_open {
        let cursor = core.input.cursor_pos();
        let clicked = core.input.left_just_pressed();
        let middle_clicked = core
            .input
            .key_bindings
            .inputs_for_action(Action::PickBlock)
            .any(|key| {
                (!game.wants_text_input() || !matches!(key, InputKey::Keyboard(_)))
                    && core.input.input_just_pressed(key)
            });
        let right_clicked = core.input.right_just_pressed();
        let scroll_delta = core.input.consume_menu_scroll();
        // `typed`/`backspace` come from the frame's single drain up top; a
        // second drain here would always read empty.
        // Vanilla's focused search box takes every key, the drop key included.
        let drop = (!game.wants_text_input() && core.input.drop_key_just_pressed())
            .then(|| core.input.ctrl_held());
        let action = crate::ui::creative_inventory::build_creative_inventory(
            &mut elements,
            &mut game.creative_state,
            sw,
            sh,
            cursor,
            clicked,
            middle_clicked,
            right_clicked,
            scroll_delta,
            &text_events,
            core.input.action_input_just_pressed(Action::OpenChat),
            core.input.hotbar_key_just_pressed(),
            core.input.action_input_just_pressed(Action::SwapOffhand),
            drop,
            game.player.drop_spam.can_drop(),
            &game.player.inventory,
            gs,
            game.advanced_item_tooltips,
            core.input.left_held(),
            core.input.right_held(),
            &|t, s| gfx.renderer.menu_text_width(t, s),
        );
        player_preview = crate::ui::creative_inventory::player_preview(
            &game.creative_state,
            sw,
            sh,
            cursor,
            &game.player.inventory,
            gs,
        );
        match action {
            crate::ui::creative_inventory::CreativeAction::SetSlot(slot_num, item) => {
                set_creative_slot(game, connection, slot_num, item);
            }
            crate::ui::creative_inventory::CreativeAction::SetSlots(items) => {
                for (slot_num, item) in items {
                    set_creative_slot(game, connection, slot_num, item);
                }
            }
            // TODO: vanilla's client `LivingEntity.drop` also starts the local
            // arm swing; only the packet is sent here, as for the world drop.
            crate::ui::creative_inventory::CreativeAction::DropItem(item) => {
                drop_creative_item(game, connection, item);
                send_use_swing(&connection.packet_tx, InteractionHand::MainHand);
            }
            crate::ui::creative_inventory::CreativeAction::DropSlot {
                slot,
                dropped,
                remaining,
            } => {
                set_creative_slot(game, connection, slot, remaining);
                drop_creative_item(game, connection, dropped);
                send_use_swing(&connection.packet_tx, InteractionHand::MainHand);
            }
            crate::ui::creative_inventory::CreativeAction::None => {}
        }
        core.input.clear_just_pressed_actions();
    }

    // Before chat so chat draws over it (vanilla extract order).
    if !benchmark_running && !game.hide_gui {
        game.title.build(&mut elements, sw, sh, gs, partial_tick);
    }

    // F1 hides the closed-chat overlay; an open chat is a screen and renders
    // regardless (vanilla Hud.extractChat vs ChatScreen).
    // TODO: the closed chat (and titles) build after screens, so they draw
    // sharp over the menu blur; vanilla's HUD extracts them before the screen.
    if !game.hide_gui || game.chat.is_focused() {
        let command_tree = game.command_tree.clone();
        // Vanilla `ChatScreen.mouseClicked` offers the click to the chat's own
        // targets before its widgets.
        let chat_takes_click = game
            .chat
            .hovering_clickable(core.input.cursor_pos(), core.input.shift_held());
        let chat_action = game.chat.build(
            &mut elements,
            crate::ui::chat::ChatBuildContext {
                screen_w: sw,
                screen_h: sh,
                gui_scale: gs,
                cursor: core.input.cursor_pos(),
                covered: dialog_open,
                clicked: core.input.left_just_pressed(),
                shift: core.input.shift_held(),
                command_tree: command_tree.as_deref(),
                advanced_item_tooltips: game.advanced_item_tooltips,
                key_bindings: &core.input.key_bindings,
                text_width_fn: &|t, s| gfx.renderer.menu_text_width(t, s),
                spans_width_fn: &|spans, s| gfx.renderer.menu_spans_width(spans, s),
            },
        );
        if let Some(action) = chat_action {
            handle_chat_ui_action(action, core, connection, game);
        }
        if game.chat.is_in_bed() && !dialog_open && !game.chat.has_pending_modal_prompt() {
            let hovered = common::push_button(
                &mut elements,
                core.input.cursor_pos(),
                sw / 2.0 - 100.0 * gs,
                sh - 40.0 * gs,
                200.0 * gs,
                20.0 * gs,
                gs,
                common::FONT_SIZE * gs,
                crate::lang::translate("multiplayer.stopSleeping").unwrap_or("Leave Bed"),
                true,
            );
            if hovered && core.input.left_just_pressed() && !chat_takes_click {
                core.audio.play_ui_click();
                game.stop_sleeping_requested = true;
            }
        }
    }

    // Subtitles draw above chat and the tab list; toasts stay on top
    // (vanilla extract order). The queue is empty while the option is off.
    let subtitle_now = std::time::Instant::now();
    for ev in core.audio.take_subtitle_events() {
        game.subtitles
            .on_play_sound(&ev.key, *ev.pos, ev.range, subtitle_now);
    }
    if core.menu.show_subtitles && !benchmark_running && !game.hide_gui {
        let (yaw_deg, pitch_deg) = gfx.renderer.camera_effective_look_deg();
        game.subtitles.build(
            &mut elements,
            sw,
            sh,
            gs,
            gfx.renderer.camera_render_position(),
            yaw_deg,
            pitch_deg,
            subtitle_now,
            &|t, s| gfx.renderer.menu_text_width(t, s),
        );
    }

    // Vanilla Gui.update() runs the toast manager every frame regardless of
    // screens or F1; only rendering is gated (ToastManager.extractRenderState).
    for event in game.toasts.update() {
        core.audio.play_ui_sound(event, 1.0, 1.0);
    }
    if !benchmark_running && !game.hide_gui {
        game.toasts.build(&mut elements, sw, gs, &|spans, s| {
            gfx.renderer.menu_spans_width(spans, s)
        });
    }

    build_server_screens(
        &mut elements,
        sw,
        sh,
        gs,
        core,
        gfx,
        connection,
        game,
        Some(game.tick_count),
        &text_events,
    );

    if game.chat.is_open() && !dialog_open && core.input.cursor_moved_this_frame() {
        let icon = if game
            .chat
            .hovering_clickable(core.input.cursor_pos(), core.input.shift_held())
        {
            winit::window::CursorIcon::Pointer
        } else {
            winit::window::CursorIcon::Default
        };
        gfx.window.set_cursor(icon);
    }

    // Chat consumes keys, not clicks; nothing else clears them while only chat
    // is open, so drop them here to keep stray clicks out of the live sim.
    if game.chat.is_open() {
        core.input.clear_just_pressed_actions();
    }

    let swing_progress = HANDS.map(|hand| game.interaction.swing_progress(hand, partial_tick));
    let use_anim = HANDS.map(|hand| game.interaction.use_animation(hand, partial_tick));
    let destroy_info = game.interaction.destroy_stage().map(|(pos, stage)| {
        let state = game.chunk_store.get_block_state(pos.x, pos.y, pos.z);
        (pos, stage, state)
    });

    use crate::entity::glowing::{appears_glowing, outline_color, scoreboard_name};
    use crate::renderer::pipelines::entity_renderer::Visibility;
    let spectating = crate::player::is_spectator(game.player.game_mode);
    let spectator_outlines = spectating && core.input.performing_action(Action::SpectatorOutlines);
    let viewer = scoreboard_name(&game.tab_list, core.user.uuid, Some(core.user.uuid));
    // Both take the entity's scoreboard name lazily: it's only needed when the
    // entity glows or is invisible.
    let outline = |glowing: bool, is_player: bool, name: &dyn Fn() -> String| {
        appears_glowing(glowing, is_player, spectator_outlines)
            .then(|| outline_color(&game.scoreboard, &name()))
    };
    // Vanilla `Entity.isInvisibleTo`, against the local player.
    let visibility = |invisible: bool, name: &dyn Fn() -> String| {
        if !invisible {
            Visibility::Visible
        } else if spectating || game.scoreboard.sees_friendly_invisible(&viewer, &name()) {
            Visibility::Seen
        } else {
            Visibility::Hidden
        }
    };

    let world_light = WorldLight {
        chunks: &game.chunk_store,
        table: game.block_registry.block_state_table(),
    };
    let player_light = entity_light(
        &world_light,
        EntityKind::Player,
        game.player
            .prev_position
            .lerp(game.player.position, partial_tick as f64)
            + glam::dvec3(0.0, f64::from(game.player.target_eye_height()), 0.0),
        game.player.on_fire,
        0,
    );

    let mut entity_renders: Vec<EntityRenderInfo> = if benchmark_running {
        Vec::new()
    } else {
        // Vanilla `LevelExtractor` skips the camera entity unless the camera
        // is detached.
        let first_person = gfx.renderer.is_first_person();
        game.entity_store
            .living
            .iter()
            .filter(|&(&id, _)| !(first_person && game.camera_entity == Some(id)))
            .map(|(&entity_id, e)| {
                let interp_pos = e.prev_position.lerp(e.position, partial_tick as f64);
                let extras =
                    entity_extras(entity_id, e, partial_tick, game.sky_state.game_time as i64);
                let is_sleeping = e.sleeping_pos.is_some();
                let (radius, strength) = crate::entity::shadow::of_living(e);

                let name = || scoreboard_name(&game.tab_list, e.uuid, e.player_uuid);
                let seen = visibility(e.is_invisible, &name);
                // `ArmorStandRenderer.getRenderType`: a hidden marker has no
                // render type, so not even its glowing outline draws.
                let drawn = !(e.is_armor_stand_marker() && seen == Visibility::Hidden);
                EntityRenderInfo {
                    position: interp_pos + extras.render_offset,
                    outline_color: outline(
                        e.is_glowing && drawn,
                        e.entity_type == EntityKind::Player,
                        &name,
                    ),
                    visibility: seen,
                    head_only: false,
                    armor_stand_pose: e.armor_stand_pose,
                    pack_shadow: if game.riding_vehicle_id == Some(entity_id) {
                        PackShadow::Player
                    } else if e
                        .player_uuid
                        .and_then(|id| game.tab_list.players.get(&id))
                        .is_some_and(|p| crate::player::is_spectator(p.game_mode))
                    {
                        PackShadow::Spectator
                    } else {
                        PackShadow::Entity
                    },
                    light: entity_light(
                        &world_light,
                        e.entity_type,
                        e.eye_position(partial_tick),
                        e.is_on_fire,
                        e.dark_ticks,
                    ),
                    head_y_rot_deg: lerp_angle(
                        e.prev_head_y_rot_deg,
                        e.head_y_rot_deg,
                        partial_tick,
                    ),
                    head_x_rot_deg: sleeping_head_pitch_deg(
                        is_sleeping,
                        e.prev_look_dir
                            .x_rot_deg()
                            .lerp(e.look_dir.x_rot_deg(), partial_tick),
                    ),
                    body_y_rot_deg: extras.body_y_rot_deg.unwrap_or_else(|| {
                        lerp_angle(e.prev_body_y_rot_deg, e.body_y_rot_deg, partial_tick)
                    }),
                    is_sleeping,
                    sleeping_direction: bed_orientation(game, e.sleeping_pos),
                    sleeping_eye_height: standing_eye_height(e.entity_type, e.is_baby),
                    is_baby: e.is_baby,
                    is_crouching: e.is_crouching,
                    is_passenger: e.vehicle.is_some(),
                    walk_anim_pos: e.walk_pos(partial_tick),
                    walk_anim_speed: e.walk_speed(partial_tick),
                    // Vanilla `MannequinRenderer` is an `AvatarRenderer`: the
                    // player model, wearing the skin kept under its own id.
                    entity_kind: if e.entity_type == EntityKind::Mannequin {
                        EntityKind::Player
                    } else {
                        e.entity_type
                    },
                    player_uuid: if e.entity_type == EntityKind::Mannequin {
                        Some(e.uuid)
                    } else {
                        e.player_uuid
                    },
                    armor: e.armor.clone(),
                    entity_id: Some(entity_id),
                    variant_index: extras.variant_index,
                    overlay_tints: extras.overlay_tints,
                    overlay_variants: extras.overlay_variants,
                    is_unhappy: e.unhappy_counter > 0,
                    head_y_offset: extras.head_y_offset,
                    head_x_rot_deg_override: extras.head_x_rot_deg_override,
                    has_red_overlay: has_red_overlay(e.hurt_time, e.death_time),
                    // `CreeperRenderer.getWhiteOverlayProgress`: every other
                    // tenth of the fuse flashes white.
                    white_overlay: if e.entity_type == EntityKind::Creeper {
                        let swelling = crate::entity::creeper_swelling(e, partial_tick);
                        if (swelling * 10.0) as i32 % 2 == 0 {
                            0.0
                        } else {
                            swelling.clamp(0.5, 1.0)
                        }
                    } else {
                        0.0
                    },
                    // The dragon dies hovering, not falling over.
                    death_time: if e.entity_type == EntityKind::EnderDragon {
                        0.0
                    } else {
                        render_death_time(e.death_time, partial_tick)
                    },
                    aggressive: e.aggressive,
                    flap: extras.flap,
                    flap_speed: extras.flap_speed,
                    is_creepy: e.is_creepy,
                    is_converting: e.is_converting,
                    is_holding_item: e.equipment(EquipmentSlot::Mainhand).is_some(),
                    nose_wobble_speed: extras.nose_wobble_speed,
                    is_sitting: e.is_sitting,
                    is_sprinting: e.is_sprinting,
                    is_angry: extras.is_angry,
                    tail_angle: extras.tail_angle,
                    head_roll_angle: extras.head_roll_angle,
                    shake_anim: extras.shake_anim,
                    lie_down_amount: extras.lie_down_amount,
                    lie_down_amount_tail: extras.lie_down_amount_tail,
                    relax_state_one_amount: extras.relax_state_one_amount,
                    hop_elapsed_secs: extras.hop_elapsed_secs,
                    base_tint: extras.base_tint.unwrap_or(WHITE_TINT),
                    eat_anim: extras.eat_anim,
                    stand_anim: extras.stand_anim,
                    feeding_anim: extras.feeding_anim,
                    animate_tail: extras.animate_tail,
                    is_in_water: e.is_in_water,
                    tentacle_angle: extras.tentacle_angle,
                    bat_resting: e.bat_resting,
                    bat_elapsed_secs: extras.bat_elapsed_secs,
                    sniffer: extras.sniffer,
                    allay: extras.allay,
                    copper_golem: extras.copper_golem,
                    llama_decor: extras.llama_decor,
                    bee: extras.bee,
                    fox: extras.fox,
                    axolotl: extras.axolotl,
                    panda: extras.panda,
                    armadillo: extras.armadillo,
                    goat: extras.goat,
                    parrot: extras.parrot,
                    dolphin_moving: extras.dolphin_moving,
                    turtle: extras.turtle,
                    golem_attack_ticks: extras.golem_attack_ticks,
                    golem_offer_flower_ticks: extras.golem_offer_flower_ticks,
                    body_transform: extras.body_transform,
                    scale: crate::entity::entity_scale(e),
                    dragon: extras.dragon.clone(),
                    age_in_ticks: e.age_in_ticks as f32 + partial_tick,
                    attack_time: e.swing_progress(partial_tick),
                    attack_stabs: crate::player::interaction::swing_animation(
                        e.equipment(azalea_inventory::components::EquipmentSlot::Mainhand)
                            .and_then(|stack| stack.as_present()),
                    )
                    .0,
                    blocking_arm: e
                        .using_hand
                        .filter(|&hand| {
                            let slot = if hand == 0 {
                                azalea_inventory::components::EquipmentSlot::Mainhand
                            } else {
                                azalea_inventory::components::EquipmentSlot::Offhand
                            };
                            e.equipment(slot)
                                .and_then(|stack| stack.as_present())
                                .is_some_and(crate::player::interaction::blocks_attacks)
                        })
                        .map(|hand| hand == 0),
                    spear: spear_pose(
                        e.entity_type,
                        e.aggressive,
                        [
                            azalea_inventory::components::EquipmentSlot::Mainhand,
                            azalea_inventory::components::EquipmentSlot::Offhand,
                        ]
                        .map(|slot| e.equipment(slot).and_then(|stack| stack.as_present())),
                        e.using_hand,
                        e.use_ticks as f32 + partial_tick,
                        e.swing_time > 0,
                    ),
                    crossbow: crossbow_pose(
                        e.entity_type,
                        [
                            azalea_inventory::components::EquipmentSlot::Mainhand,
                            azalea_inventory::components::EquipmentSlot::Offhand,
                        ]
                        .map(|slot| e.equipment(slot).and_then(|stack| stack.as_present())),
                        e.using_hand,
                        e.use_ticks as f32 + partial_tick,
                        e.charging_crossbow,
                    ),
                    skip_cull: false,
                    shadow: ShadowCaster {
                        position: *interp_pos,
                        offset: extras.render_offset,
                        radius,
                        strength,
                    },
                }
            })
            .collect()
    };
    // Placed heads and skulls within name-tag range (vanilla
    // `SkullBlockRenderer`), as their model's head; a player head in the
    // head's skin.
    if !benchmark_running {
        let eye = gfx.renderer.camera_render_position();
        for (pos, be) in &game.chunk_store.block_entities {
            if be.kind != BlockEntityKind::Skull {
                continue;
            }
            let block = glam::DVec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
            if (block - eye).length_squared() > 64.0 * 64.0 {
                continue;
            }
            let state = game.chunk_store.get_block_state(pos.x, pos.y, pos.z);
            let data = game.block_registry.block_state_data(state);
            let Some(model) = crate::entity::heads::skull_model(data.id()) else {
                continue;
            };
            let props = data.properties();
            let Some((offset, yaw)) =
                crate::entity::heads::placed_skull(props.get("rotation"), props.get("facing"))
            else {
                continue;
            };
            let mut skin = None;
            if model == EntityKind::Player {
                let Some(profile) = crate::entity::heads::block_profile(&be.nbt) else {
                    continue;
                };
                let key = crate::entity::heads::skin_key(&profile);
                core.queue_profile_skin(key, &profile);
                skin = Some(key);
            }
            let (scale, lift) = crate::entity::heads::head_placement(model, offset.y, 0.5);
            entity_renders.push(EntityRenderInfo {
                position: Position::new(block.x + offset.x, block.y + lift, block.z + offset.z),
                head_y_rot_deg: yaw,
                body_y_rot_deg: yaw,
                entity_kind: model,
                // A player head without a skin draws vanilla's wide Steve.
                variant_index: crate::renderer::pipelines::entity_renderer::WIDE_STEVE_SKIN,
                player_uuid: skin,
                head_only: true,
                body_transform: Some(glam::Mat4::from_scale(glam::Vec3::splat(scale))),
                light: block_lighter::light_coords(&world_light, [pos.x, pos.y, pos.z]),
                ..Default::default()
            });
        }
    }

    // Vanilla `CustomHeadLayer`: a head or skull worn by an armor stand (how
    // servers float pet heads), even an invisible one, drawn as its model's
    // head; a player head in the head's skin. Humanoid mobs and other
    // players wear theirs on their head part too.
    if !benchmark_running {
        for e in game.entity_store.living.values() {
            let wearer = if e.entity_type == EntityKind::ArmorStand {
                None
            } else {
                match crate::entity::heads::humanoid_skull(e.entity_type, e.is_baby, e.is_crouching)
                {
                    Some(placement) => Some(placement),
                    None => continue,
                }
            };
            let Some(helmet) = e
                .equipment(azalea_inventory::components::EquipmentSlot::Head)
                .and_then(|stack| stack.as_present())
            else {
                continue;
            };
            let Some(model) = crate::entity::heads::skull_model(
                &crate::player::inventory::item_resource_name(helmet.kind),
            ) else {
                continue;
            };
            let skin = if model == EntityKind::Player {
                let Some(profile) = e
                    .equipment(azalea_inventory::components::EquipmentSlot::Head)
                    .and_then(crate::entity::heads::item_profile)
                else {
                    continue;
                };
                Some(crate::entity::heads::skin_key(&profile))
            } else {
                None
            };
            let (pivot, size) = wearer.unwrap_or_else(|| {
                crate::entity::heads::armor_stand_skull(e.armor_stand_flags & 0x01 != 0)
            });
            let (scale, lift) = crate::entity::heads::head_placement(model, pivot, size);
            let body_y = lerp_angle(e.prev_body_y_rot_deg, e.body_y_rot_deg, partial_tick);
            let feet = e.prev_position.lerp(e.position, partial_tick as f64);
            // A stand's skull turns with its head pose (its roll aside); a
            // mob's with where it looks.
            let (head_x, head_y) = if wearer.is_some() {
                (
                    e.prev_look_dir
                        .x_rot_deg()
                        .lerp(e.look_dir.x_rot_deg(), partial_tick),
                    lerp_angle(e.prev_head_y_rot_deg, e.head_y_rot_deg, partial_tick),
                )
            } else {
                let [x, y, _] = e.armor_stand_pose[0];
                (x, body_y + y)
            };
            entity_renders.push(EntityRenderInfo {
                position: feet + glam::DVec3::new(0.0, lift, 0.0),
                head_x_rot_deg: head_x,
                head_y_rot_deg: head_y,
                body_y_rot_deg: body_y,
                entity_kind: model,
                // A player head without a skin draws vanilla's wide Steve.
                variant_index: crate::renderer::pipelines::entity_renderer::WIDE_STEVE_SKIN,
                player_uuid: skin,
                head_only: true,
                body_transform: Some(glam::Mat4::from_scale(glam::Vec3::splat(scale))),
                light: entity_light(
                    &world_light,
                    e.entity_type,
                    e.eye_position(partial_tick),
                    false,
                    0,
                ),
                ..Default::default()
            });
        }
    }

    // Arrows (`ArrowRenderer`): turned to their flight, from vanilla's own
    // model layer.
    if !benchmark_running {
        for arrow in game.arrows.arrows.values() {
            let feet = arrow
                .prev_position
                .lerp(arrow.position, partial_tick as f64);
            let y_rot = lerp_angle(arrow.prev_y_rot, arrow.y_rot, partial_tick);
            let x_rot = arrow.prev_x_rot + (arrow.x_rot - arrow.prev_x_rot) * partial_tick;
            // `ThrownTridentRenderer`: the model stands on its butt, so a
            // quarter turn more lays it along the flight.
            let x_rot = if arrow.kind == EntityKind::Trident {
                x_rot + 90.0
            } else {
                x_rot
            };
            entity_renders.push(EntityRenderInfo {
                position: feet,
                entity_kind: arrow.kind,
                head_y_rot_deg: 180.0,
                body_y_rot_deg: 180.0,
                variant_index: u32::from(arrow.tipped),
                body_transform: Some(
                    glam::Mat4::from_rotation_y((y_rot - 90.0).to_radians())
                        * glam::Mat4::from_rotation_z(x_rot.to_radians())
                        * crate::renderer::pipelines::entity_renderer::unliving_model_frame(),
                ),
                light: entity_light(&world_light, arrow.kind, feet, false, 0),
                ..Default::default()
            });
        }
    }

    // Shulker bullets, wither skulls, llama spit, evoker fangs, lead knots,
    // minecarts and boats, each from vanilla's own model layer.
    if !benchmark_running {
        let table = game.block_registry.block_state_table();
        let rails = rails_in(&game.chunk_store, table);
        for entity in game.model_entities.entities.values() {
            let feet = entity
                .prev_position
                .lerp(entity.position, partial_tick as f64);
            let light = entity_light(&world_light, entity.kind, feet, false, 0);
            if let Some(info) = model_entity_render(entity, feet, partial_tick, light, &rails) {
                entity_renders.push(info);
            }
        }
    }

    // Paintings (`PaintingRenderer`): the picture and the board's back,
    // turned to face out of the wall.
    // TODO: vanilla lights each block of a painting on its own.
    if !benchmark_running {
        let eye = gfx.renderer.camera_render_position();
        let unliving = crate::renderer::pipelines::entity_renderer::unliving_model_frame();
        for painting in game.paintings.paintings.values() {
            let (Some(center), Some(variants)) = (painting.center(), painting.model_variants())
            else {
                continue;
            };
            if center.distance_squared(eye) > 128.0 * 128.0 {
                continue;
            }
            let position = Position::new(center.x, center.y, center.z);
            let turn = (180.0 - f32::from(painting.direction) * 90.0).to_radians();
            let light = entity_light(&world_light, EntityKind::Painting, position, false, 0);
            for variant_index in variants {
                entity_renders.push(EntityRenderInfo {
                    position,
                    entity_kind: EntityKind::Painting,
                    head_y_rot_deg: 180.0,
                    body_y_rot_deg: 180.0,
                    variant_index,
                    body_transform: Some(glam::Mat4::from_rotation_y(turn) * unliving),
                    light,
                    // A board is far bigger than the entity's own box.
                    skip_cull: true,
                    ..Default::default()
                });
            }
        }
    }

    // Beacon beams (`BeaconRenderer`): each coloured section as the beam
    // turning slowly inside its faint glow, the texture running upward.
    if !benchmark_running {
        build_beacon_beams(game, partial_tick, &mut entity_renders);
        build_books(game, &world_light, partial_tick, &mut entity_renders);
        build_spawner_mobs(game, &world_light, partial_tick, &mut entity_renders);
    }

    // Wind charges (`WindChargeRenderer`), from vanilla's own model layer:
    // drawn about their feet, and like thrown items not for their first two
    // ticks while still at the thrower's eye.
    if !benchmark_running {
        let eye = gfx.renderer.camera_render_position();
        for charge in game.thrown_items.items.values() {
            if !crate::entity::thrown_item::is_wind_charge(charge.kind) {
                continue;
            }
            let feet = charge
                .prev_position
                .lerp(charge.position, partial_tick as f64);
            if charge.age < 2 && (*feet - eye).length_squared() < 12.25 {
                continue;
            }
            entity_renders.push(EntityRenderInfo {
                position: feet,
                entity_kind: charge.kind,
                head_y_rot_deg: 180.0,
                body_y_rot_deg: 180.0,
                age_in_ticks: charge.age as f32 + partial_tick,
                body_transform: Some(
                    crate::renderer::pipelines::entity_renderer::unliving_model_frame(),
                ),
                light: entity_light(&world_light, charge.kind, feet, false, 0),
                ..Default::default()
            });
        }
    }

    // End crystals (`EndCrystalRenderer`), from vanilla's own model layer.
    if !benchmark_running {
        for crystal in game.end_crystals.crystals.values() {
            let feet = crystal
                .prev_position
                .lerp(crystal.position, partial_tick as f64);
            entity_renders.push(EntityRenderInfo {
                position: feet,
                entity_kind: EntityKind::EndCrystal,
                age_in_ticks: crystal.age as f32 + partial_tick,
                variant_index: u32::from(!crystal.shows_bottom),
                body_transform: Some(
                    crate::renderer::pipelines::entity_renderer::end_crystal_transform(),
                ),
                light: entity_light(&world_light, EntityKind::EndCrystal, feet, false, 0),
                ..Default::default()
            });
        }
    }

    // Vanilla only extracts the local player while it's the camera entity;
    // Iris's shadow pass draws it in first person too.
    let finished = game.player.death_animation_finished();
    let shown = should_render_local_player(
        benchmark_running,
        gfx.renderer.is_first_person(),
        game.player.is_sleeping(),
        finished,
    );
    if game.camera_entity.is_none() && (shown || !benchmark_running && !finished && !spectating) {
        let interp_pos = game
            .player
            .prev_position
            .lerp(game.player.position, partial_tick as f64);

        let interp_y_rot_deg = lerp_angle(
            game.player.prev_look_dir.y_rot_deg(),
            game.player.look_dir.y_rot_deg(),
            partial_tick,
        );

        let (radius, strength) =
            crate::entity::shadow::of_player(&game.player.attributes, game.player.invisible);
        entity_renders.push(EntityRenderInfo {
            position: interp_pos,
            head_y_rot_deg: interp_y_rot_deg,
            head_x_rot_deg: sleeping_head_pitch_deg(
                game.player.is_sleeping(),
                gfx.renderer.camera_look_dir().x_rot_deg(),
            ),
            body_y_rot_deg: lerp_angle(
                game.player_prev_body_y_rot,
                game.player_body_y_rot,
                partial_tick,
            ),
            is_sleeping: game.player.is_sleeping(),
            sleeping_direction,
            sleeping_eye_height: crate::player::STANDING_EYE_HEIGHT,
            is_crouching: game.player.crouching && (!game.dead || game.player.death_time > 0),
            is_passenger: game.riding_vehicle_id.is_some(),
            walk_anim_pos: game.player_walk_pos - game.player_walk_speed * (1.0 - partial_tick),
            walk_anim_speed: (game.player_prev_walk_speed
                + (game.player_walk_speed - game.player_prev_walk_speed) * partial_tick)
                .min(1.0),
            entity_kind: EntityKind::Player,
            player_uuid: Some(core.user.uuid),
            is_holding_item: game.player.inventory.hotbar_slots()
                [core.input.selected_slot() as usize]
                .is_present(),
            variant_index: crate::renderer::pipelines::entity_renderer::default_skin_variant(
                &core.user.uuid,
            ),
            outline_color: outline(game.player.glowing, true, &|| viewer.clone()),
            visibility: visibility(game.player.invisible, &|| viewer.clone()),
            pack_shadow: if spectating {
                PackShadow::Spectator
            } else if shown {
                PackShadow::Player
            } else {
                PackShadow::FirstPersonPlayer
            },
            light: player_light,
            armor: armor_render_infos(game.player.inventory.armor_slots()),
            scale: game
                .player
                .attributes
                .value(crate::attribute::AttributeKind::Scale)
                .unwrap_or(1.0) as f32,
            has_red_overlay: has_red_overlay(game.player.hurt_time, game.player.death_time),
            death_time: render_death_time(game.player.death_time, partial_tick),
            // Vanilla `attackTime`: the local swing shows on its own model
            // in third person, as it does in first.
            attack_time: swing_progress[0].max(swing_progress[1]),
            attack_stabs: crate::player::interaction::swing_animation(
                game.player.inventory.hotbar_slots()[core.input.selected_slot() as usize]
                    .as_present(),
            )
            .0,
            blocking_arm: game
                .interaction
                .blocking()
                .then(|| game.interaction.using_hand())
                .flatten()
                .map(|hand| hand == crate::player::interaction::HANDS[0]),
            spear: spear_pose(
                EntityKind::Player,
                false,
                [
                    game.player.inventory.hotbar_slots()[core.input.selected_slot() as usize]
                        .as_present(),
                    game.player.inventory.offhand().as_present(),
                ],
                game.interaction
                    .using_hand()
                    .map(|hand| u8::from(hand != crate::player::interaction::HANDS[0])),
                game.interaction.ticks_using(partial_tick),
                swing_progress[0].max(swing_progress[1]) > 0.0,
            ),
            crossbow: crossbow_pose(
                EntityKind::Player,
                [
                    game.player.inventory.hotbar_slots()[core.input.selected_slot() as usize]
                        .as_present(),
                    game.player.inventory.offhand().as_present(),
                ],
                game.interaction
                    .using_hand()
                    .map(|hand| u8::from(hand != crate::player::interaction::HANDS[0])),
                game.interaction.ticks_using(partial_tick),
                false,
            ),
            skip_cull: true,
            // Players drop their render offset before the shadow.
            shadow: ShadowCaster {
                position: *interp_pos,
                radius,
                strength,
                ..Default::default()
            },
            ..Default::default()
        });
    }

    let clock_partial_tick = sky_partial_tick(core);
    let min_y = game.chunk_store.min_y();
    // `ClientLevelData.getHorizonHeight`.
    let horizon = if game.flat { f64::from(min_y) } else { 63.0 };
    let sky = crate::renderer::SkyState {
        partial_tick: clock_partial_tick,
        attributes: sky_flash(game, game.environment_probe.at(clock_partial_tick)),
        skybox: game.environment.skybox,
        void_darkness: crate::renderer::fog::void_darkness(
            gfx.renderer.camera_render_position().y,
            min_y,
            game.flat,
        ),
        // `Entity.getEyePosition(partialTicks)`: the position between ticks,
        // the eye height as of now.
        dark_disc: game
            .player
            .prev_position
            .y
            .lerp(game.player.position.y, f64::from(partial_tick))
            + f64::from(game.player.eye_height)
            < horizon
            && !game.player.eyes_in_water,
        cardinal_light: game.cardinal_light,
        effect_fog: crate::renderer::pipelines::sky::EffectFog::new(
            game.player.effects.duration("blindness"),
            game.player.effects.darkness_factor(partial_tick),
            effective_render_distance(core, game) as f32 * 16.0,
        ),
        ..game.sky_state.clone()
    };
    if game.show_chunk_borders {
        gfx.renderer.update_chunk_borders(
            game.chunk_store.min_y(),
            game.chunk_store.min_y() + game.chunk_store.height() as i32,
        );
    }

    let mut item_renders = if benchmark_running {
        Vec::new()
    } else {
        build_item_render_infos(
            &game.item_entity_store,
            &world_light,
            &gfx.renderer,
            *gfx.renderer.camera_pivot_position(),
            gfx.renderer.camera_anchor(),
            partial_tick,
            |item| {
                outline(item.is_glowing, false, &|| {
                    scoreboard_name(&game.tab_list, item.uuid, None)
                })
            },
        )
    };

    game.mesh_dispatcher
        .set_low_chunk_builders(core.menu.low_chunk_builders);
    for (key, profile) in gfx.renderer.take_wanted_head_skins() {
        core.queue_profile_skin(key, &profile);
    }
    let display_start = Instant::now();
    let DisplayRenders {
        items: display_items,
        heads: mut display_heads,
        mut head_skins,
        texts: mut world_texts,
    } = if benchmark_running {
        DisplayRenders::default()
    } else {
        build_display_renders(
            &mut game.display_store,
            &world_light,
            &mut gfx.renderer,
            game.block_registry.block_state_table(),
            partial_tick,
        )
    };
    if !benchmark_running && !game.hide_gui {
        build_name_tags(
            NameTagSources {
                names: &mut game.name_store,
                living: &game.entity_store,
                items: &game.item_entity_store,
                falling_blocks: &game.falling_blocks,
                positions: &game.entity_positions,
                tab_list: &game.tab_list,
                scoreboard: &game.scoreboard,
                local_uuid: core.user.uuid,
                local_entity: game.player.entity_id,
            },
            &world_light,
            &mut gfx.renderer,
            partial_tick,
            &mut world_texts,
        );
    }
    crate::frame_log::add("displays", display_start.elapsed().as_secs_f32() * 1000.0);
    item_renders.extend(display_items);
    let framed_maps = if benchmark_running {
        Vec::new()
    } else {
        build_item_frame_renders(
            &mut game.item_frames,
            &world_light,
            &mut gfx.renderer,
            &mut item_renders,
            &mut display_heads,
            &mut head_skins,
        )
    };
    for (key, profile) in &head_skins {
        core.queue_profile_skin(*key, profile);
    }
    entity_renders.extend(display_heads);
    if !benchmark_running {
        build_held_items(
            game,
            &mut gfx.renderer,
            &entity_renders,
            &mut item_renders,
            core.input.selected_slot() as usize,
        );
        build_thrown_items(
            game,
            &mut gfx.renderer,
            &world_light,
            partial_tick,
            &mut item_renders,
        );
    }
    if !benchmark_running {
        build_falling_blocks(
            &game.falling_blocks,
            &world_light,
            &mut gfx.renderer,
            game.block_registry.block_state_table(),
            partial_tick,
            &mut item_renders,
        );
        build_minecart_contents(
            &game.model_entities,
            &game.chunk_store,
            &world_light,
            &mut gfx.renderer,
            game.block_registry.block_state_table(),
            partial_tick,
            &mut item_renders,
        );
        build_banners(game, &world_light, &mut gfx.renderer, &mut item_renders);
        build_dragon_heads(game, &world_light, &mut gfx.renderer, &mut item_renders);
    }
    // What signs within 64 blocks say (vanilla `SignRenderer`): each line
    // of each side as its own one-line text, on the board.
    if !benchmark_running {
        use azalea_registry::builtin::BlockEntityKind;

        use crate::world::block_entity::SignLayout;
        use crate::world::sign;
        let eye = gfx.renderer.camera_render_position();
        let anchor = gfx.renderer.camera_anchor();
        for (pos, be) in &game.chunk_store.block_entities {
            if !matches!(
                be.kind,
                BlockEntityKind::Sign | BlockEntityKind::HangingSign
            ) {
                continue;
            }
            let corner = glam::DVec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
            let distance = (corner + glam::DVec3::splat(0.5)).distance_squared(eye);
            if distance > 64.0 * 64.0 {
                continue;
            }
            let state = game.chunk_store.get_block_state(pos.x, pos.y, pos.z);
            let data = game.block_registry.block_state_data(state);
            let Some(kind) = sign::SignKind::of(data.id()) else {
                continue;
            };
            let sides = be.sign.get_or_init(|| {
                sign::sign_sides(&be.nbt).map(|side| {
                    side.map(|side| SignLayout {
                        lines: side.lines.map(|line| {
                            // `Font.split` at the board's width, first line.
                            let mut layout = gfx
                                .renderer
                                .layout_display_text(&line, kind.max_line_width());
                            layout.lines.truncate(1);
                            layout.width = layout.lines.first().map_or(0.0, |line| line.1);
                            (layout.width > 0.0).then(|| Arc::new(layout))
                        }),
                        glowing: side.glowing,
                    })
                })
            });
            let props = data.properties();
            let world = block_lighter::light_coords(&world_light, [pos.x, pos.y, pos.z]);
            let base = glam::Mat4::from_translation((corner - anchor).as_vec3());
            for (back, side) in sides.iter().enumerate() {
                let Some(side) = side else {
                    continue;
                };
                let light = if side.glowing {
                    LightLevels::FULL_BRIGHT
                } else {
                    world
                };
                for (line, layout) in side.lines.iter().enumerate() {
                    let Some(layout) = layout else {
                        continue;
                    };
                    let pose = sign::line_pose(
                        kind,
                        props.get("rotation"),
                        props.get("facing"),
                        back == 1,
                        line,
                    );
                    world_texts.push((
                        distance,
                        crate::renderer::pipelines::menu_overlay::WorldTextRender {
                            layout: layout.clone(),
                            pose: base * pose,
                            align: crate::entity::display::TextAlign::Center,
                            color: 0xFFFF_FFFF,
                            background: 0,
                            shadow: false,
                            see_through: false,
                            light: light.lane(),
                        },
                    ));
                }
            }
        }
    }
    // A named marker's label under it on a framed map (vanilla
    // `MapRenderer`): white on a dark band, shrunk to fit 25 map pixels,
    // at most two thirds of the font's size.
    // TODO: the same labels on a map held in the hand.
    for &(id, pose, light) in &framed_maps {
        let Some(map) = game.maps.get(id) else {
            continue;
        };
        let eye = gfx.renderer.camera_render_position() - gfx.renderer.camera_anchor();
        let distance = f64::from(pose.w_axis.truncate().distance_squared(eye.as_vec3()));
        for marker in map.decorations.iter().filter(|d| d.shows_on_item_frame()) {
            let Some(name) = &marker.name else {
                continue;
            };
            let layout = game
                .map_labels
                .entry(format!("{name:?}"))
                .or_insert_with(|| Arc::new(gfx.renderer.layout_display_text(name, i32::MAX)))
                .clone();
            if layout.width <= 0.0 {
                continue;
            }
            let scale = (25.0 / layout.width).clamp(0.0, 6.0 / 9.0);
            // From a text display's space (y up, facing +z, 40 text pixels a
            // unit, its one line just above the origin) into the map's (y
            // down, facing -z): half a turn about x. The line's top sits 6
            // text pixels under a point 4 map pixels below the marker.
            let label = glam::Mat4::from_translation(glam::Vec3::new(
                f32::from(marker.x) / 2.0 + 64.0,
                f32::from(marker.y) / 2.0 + 68.0 + 16.0 * scale,
                -0.1,
            )) * glam::Mat4::from_scale(glam::Vec3::new(40.0, -40.0, -40.0) * scale);
            world_texts.push((
                distance,
                crate::renderer::pipelines::menu_overlay::WorldTextRender {
                    layout,
                    pose: pose * label,
                    align: crate::entity::display::TextAlign::Center,
                    color: 0xFFFF_FFFF,
                    background: 0x8000_0000,
                    shadow: false,
                    see_through: false,
                    light: light.lane(),
                },
            ));
        }
    }
    // Nothing writes depth, so overlapping texts layer back to front.
    world_texts.sort_by(|a, b| b.0.total_cmp(&a.0));
    gfx.renderer
        .set_world_texts(world_texts.into_iter().map(|(_, text)| text).collect());

    let moving_block_renders =
        moving_block_renders(game, gfx.renderer.camera_render_position(), partial_tick);

    let shadow_casters = if core.menu.entity_shadows && !benchmark_running {
        shadow_casters(
            &entity_renders,
            &game.item_entity_store,
            *gfx.renderer.camera_pivot_position(),
            partial_tick,
        )
    } else {
        Vec::new()
    };

    let block_entity_renders: Vec<crate::renderer::BlockEntityRenderInfo> = if benchmark_running {
        Vec::new()
    } else {
        game.chunk_store
            .block_entities
            .iter()
            .filter_map(|(pos, be)| {
                let state = game.chunk_store.get_block_state(pos.x, pos.y, pos.z);
                let block_state_data = game.block_registry.block_state_data(state);
                let id = block_state_data.id();
                // A predicted break leaves a stale entry until the server
                // confirms; don't render entries whose block is gone.
                if !crate::world::block_entity::is_block_entity_block(id) {
                    return None;
                }
                // Since 26.1 a sign's board is a block model, meshed with its
                // chunk; drawing the block entity's own too would stand a
                // second, standing, sign in every wall sign.
                if matches!(
                    be.kind,
                    BlockEntityKind::Sign | BlockEntityKind::HangingSign
                ) && game
                    .block_registry
                    .render_view()
                    .get_baked_model(state)
                    .is_some_and(|model| !model.quads.is_empty())
                {
                    return None;
                }
                let props = block_state_data.properties();
                let variant = block_entity::variant_for_block(be.kind, id, props);
                let yaw = block_entity::yaw_for_block(be.kind, props);
                let openness_at = |p: &BlockPos| {
                    game.block_entity_anim
                        .container(p)
                        .map(|a| a.openness(partial_tick))
                        .unwrap_or(0.0)
                };
                let light_at =
                    |p: &BlockPos| block_lighter::light_coords(&world_light, [p.x, p.y, p.z]);
                let mut lid_open = openness_at(pos);
                let mut light = light_at(pos);
                // A double chest's lids follow the max openness of both halves
                // (vanilla opennessCombiner), and its light the max of both
                // (`BrightnessCombiner`); the open block event only arrives at
                // the interacted half's position.
                if matches!(
                    be.kind,
                    BlockEntityKind::Chest | BlockEntityKind::TrappedChest
                ) && let Some((dx, dz)) = block_entity::chest_partner_offset(
                    props.get("facing").unwrap_or("north"),
                    props.get("type").unwrap_or("single"),
                ) {
                    let partner = BlockPos::new(pos.x + dx, pos.y, pos.z + dz);
                    lid_open = lid_open.max(openness_at(&partner));
                    light = light.max(light_at(&partner));
                }
                Some(crate::renderer::BlockEntityRenderInfo {
                    pos: *pos,
                    kind: be.kind,
                    yaw,
                    variant,
                    lid_open,
                    light,
                    bell_shake: game
                        .block_entity_anim
                        .bell(pos)
                        .map(|bell| bell.shake(partial_tick)),
                })
            })
            .collect()
    };

    gfx.renderer
        .set_hit_outline(hit_outline(game, core.input.selected_slot() as usize));
    gfx.renderer.set_end_portals(
        if benchmark_running {
            Vec::new()
        } else {
            end_portal_blocks(game)
        },
        (game.sky_state.game_time % 24000) as f32 + partial_tick,
    );

    let weather_columns = if benchmark_running {
        Vec::new()
    } else {
        build_weather_columns(
            &world_light,
            &game.biome_climate,
            gfx.renderer.camera_render_position(),
            sky.rain(),
        )
    };

    if game.deterministic {
        game.particle_store.clear();
    }
    // Benchmarks and deterministic tests draw neither particles nor clouds.
    let no_ambience = benchmark_running || game.deterministic;
    let extract_start = Instant::now();
    let mut particle_quads = if no_ambience {
        Vec::new()
    } else {
        game.particle_store
            .extract(partial_tick, gfx.renderer.camera_anchor(), &world_light)
    };
    if !no_ambience {
        add_entity_flames(game, &gfx.renderer, partial_tick, &mut particle_quads);
    }
    let fishing_lines = if benchmark_running {
        Vec::new()
    } else {
        add_experience_orbs(game, &gfx.renderer, partial_tick, &mut particle_quads);
        add_dragon_fireballs(game, &gfx.renderer, partial_tick, &mut particle_quads);
        add_fishing_hooks(game, core, &gfx.renderer, partial_tick, &mut particle_quads)
    };
    gfx.renderer.set_fishing_lines(fishing_lines);
    let ropes = if benchmark_running {
        Vec::new()
    } else {
        build_leashes(game, &gfx.renderer, partial_tick)
    };
    gfx.renderer.set_ropes(ropes);
    crate::frame_log::add(
        "particle_extract",
        extract_start.elapsed().as_secs_f32() * 1000.0,
    );
    crate::frame_log::add("particle_count", particle_quads.len() as f32);

    let effective_rd = effective_render_distance(core, game);
    let inventory = &game.player.inventory;
    let main_stack = &inventory.hotbar_slots()[core.input.selected_slot() as usize];
    let shown_hands = game
        .interaction
        .hands_to_render(main_stack.as_present(), inventory.offhand().as_present());
    let held_items = if benchmark_running {
        [None, None]
    } else {
        let using = game.interaction.using_hand();
        let cast = cast_rod_hand(
            game.fishing_hooks.has_hook(game.player.entity_id),
            main_stack.as_present(),
            inventory.offhand().as_present(),
        );
        let held = |stack: &azalea_inventory::ItemStack, hand: u8| match stack {
            azalea_inventory::ItemStack::Present(data) => {
                let name = crate::player::inventory::item_resource_name(data.kind);
                let hand_id = crate::player::interaction::HANDS[usize::from(hand)];
                (name != "air").then(|| {
                    (
                        name,
                        data.clone(),
                        using == Some(hand_id),
                        cast == Some(hand),
                    )
                })
            }
            _ => None,
        };
        // The hands show their stacks a moment behind a change of them
        // (`ItemInHandRenderer.mainHandItem`).
        let [main, off] = game.hand_equip.visible();
        [held(main, 0), held(off, 1)]
    };
    // Vanilla `submitArmWithItem`: a stack with a map id shows as its map,
    // blank until the server has sent it.
    let held_map = |stack: &azalea_inventory::ItemStack| {
        let map = stack.get_component::<azalea_inventory::components::MapId>()?;
        Some(map.id)
    };
    let held_maps = if benchmark_running {
        [None, None]
    } else {
        game.hand_equip.visible().map(held_map)
    };
    // Last element pushed: vanilla draws the saving indicator on its own
    // stratum above screens, with the GUI hidden (F1) included.
    if !benchmark_running && !game.deterministic && core.menu.show_autosave_indicator {
        let alpha = game
            .last_saving_indicator_value
            .lerp(game.saving_indicator_value, partial_tick)
            .clamp(0.0, 1.0);
        if (alpha * 255.0).floor() > 0.0 {
            hud::build_saving_indicator(&mut elements, sw, sh, gs, alpha, &|t, s| {
                gfx.renderer.menu_text_width(t, s)
            });
        }
    }

    // Recompute after this frame's state changes (a finished benchmark releases
    // the cursor mid-frame), so the renderer doesn't re-hide it from a stale value.
    let hide_cursor = game.input_live() && !game.dead && core.input.is_cursor_captured();
    let shader_pack = core
        .menu
        .shader_pack
        .as_ref()
        .map(|name| core.menu.shader_packs_dir.join(name));
    if shader_pack.is_some() {
        gfx.renderer.set_pack_world(pack_world(game));
    }
    if let Err(e) = gfx.renderer.set_shader_pack(shader_pack.as_deref()) {
        tracing::error!(
            "Shader pack {}: {e}",
            shader_pack.unwrap_or_default().display()
        );
    }
    let block_ids = gfx.renderer.pack_block_ids();
    if !game.mesh_dispatcher.meshes_for(block_ids.as_ref()) {
        // TODO: rebuild after a datapack reload replaces the block tags.
        let pack = block_ids.map(|ids| {
            Arc::new(crate::shaderpack::block_ids::PackBlockIds::new(
                ids,
                game.block_registry.block_state_table(),
                game.block_registry.tags(),
            ))
        });
        game.mesh_dispatcher = game.mesh_dispatcher.with_pack(pack);
        gfx.renderer
            .set_terrain_format(game.mesh_dispatcher.format());
        game.rebuild_meshes();
    }
    gfx.renderer.set_screen_fire(
        game.player.on_fire && !crate::player::is_spectator(game.player.game_mode),
    );
    let render_start = Instant::now();
    gfx.renderer.set_lightning(
        game.lightning
            .bolts
            .values()
            .map(|bolt| (bolt.position, bolt.seed))
            .collect(),
    );
    let rendered = gfx.renderer.render_world(
        game.block_registry.render_view(),
        &gfx.window,
        hide_cursor,
        elements,
        swing_progress,
        game.hand_equip.lowered(partial_tick),
        use_anim,
        held_items,
        held_maps.map(|held| held.map(|id| (id, game.maps.get(id)))),
        &framed_maps
            .iter()
            .map(
                |&(id, pose, light)| crate::renderer::pipelines::map::FramedMap {
                    id,
                    map: game.maps.get(id),
                    pose,
                    light,
                },
            )
            .collect::<Vec<_>>(),
        core.menu.main_hand_right(),
        player_light,
        shown_hands,
        game.scoreboard.member_team_color(&core.user.username),
        renders_first_person_hand(game),
        destroy_info,
        game.show_chunk_borders,
        sky,
        ShadowScene {
            casters: &shadow_casters,
            chunks: &game.chunk_store,
            block_state_table: game.block_registry.block_state_table(),
            sky_darken: game.sky_darken(),
            ambient_light: game.ambient_light,
        },
        &moving_block_renders,
        &entity_renders,
        &item_renders,
        &block_entity_renders,
        &particle_quads,
        &weather_columns,
        if no_ambience {
            crate::renderer::CloudMode::Off
        } else {
            core.menu.cloud_mode
        },
        effective_rd,
        player_preview,
        book_preview,
        game.player.eyes_in_water,
    );
    {
        let t = gfx.renderer.last_timings();
        crate::frame_log::add("render_call", render_start.elapsed().as_secs_f32() * 1000.0);
        crate::frame_log::add("fence_wait", t.fence_ms);
        crate::frame_log::add("acquire", t.acquire_ms);
        crate::frame_log::add("draw_record", t.draw_ms);
        crate::frame_log::add("present", t.present_ms);
        crate::frame_log::add("pace", t.pace_ms);
    }
    if let Err(e) = rendered {
        tracing::error!("Render error: {e}");
    }
    // Whole-frame wall time (incl. render), read next frame to align with `raw_dt`.
    game.last_update_phases.update_ms = frame_start.elapsed().as_secs_f32() * 1000.0;
    crate::frame_log::add("frame", game.last_update_phases.update_ms);

    // Vanilla `onClose`: `closeContainer`, then `onStopHovering`.
    let stop_hovering = close_inventory
        .then(|| game.bundle_hovered.filter(|(_, _, bundle)| *bundle))
        .flatten();
    if close_inventory {
        game.close_menu();
        game.close_creative_inventory();
        core.apply_cursor_grab(&gfx.window, Some(game));
    }

    // Tell the server when a container menu closes so it returns/drops the
    // cursor stack (and a crafting grid's contents).
    let open_menu = game.open_menu_id();
    if let Some(prev) = game.container_was_open
        && open_menu != Some(prev)
    {
        use azalea_protocol::packets::game::s_container_close::ServerboundContainerClose;
        connection
            .packet_tx
            .send(ServerboundGamePacket::ContainerClose(
                ServerboundContainerClose { container_id: prev },
            ));
    }
    game.container_was_open = open_menu;
    if let Some((_, slot, _)) = stop_hovering {
        send_bundle_selection(&connection.packet_tx, slot, crate::ui::bundle::NO_SELECTION);
    }

    match death_action {
        DeathAction::Respawn => {
            game.death_confirm = false;
            core.send_respawn(connection, game);
        }
        DeathAction::TitleScreen => {
            return GameUpdateResult::ManualDisconnect;
        }
        DeathAction::ShowConfirm => {
            game.death_confirm = true;
            game.death_confirm_ticks = 0;
        }
        DeathAction::None => {}
    }

    match pause_action {
        PauseAction::Resume => {
            game.paused = false;
            core.apply_cursor_grab(&gfx.window, Some(game));
        }
        PauseAction::Options => {
            // TODO: back from these options, the pause menu ignores Escape and
            // clicks (tests/ingame/ui/menu_blur.toml stops before it).
            core.menu.open_options(&core.input.key_bindings);
            game.options_from_game = true;
            core.apply_cursor_grab(&gfx.window, Some(game));
        }
        PauseAction::Disconnect => {
            return GameUpdateResult::ManualDisconnect;
        }
        PauseAction::OpenBenchmark => {
            game.pause_screen = PauseScreen::Benchmark;
        }
        PauseAction::OpenChunkLoader => {
            game.pause_screen = PauseScreen::ChunkLoader;
        }
        PauseAction::Back => {
            game.pause_screen = match game.pause_screen {
                PauseScreen::ChunkLoader => PauseScreen::Benchmark,
                _ => PauseScreen::Main,
            };
        }
        PauseAction::StartFpsBenchmark => {
            game.benchmark = Some(Benchmark::new(
                gfx.renderer.gpu_name(),
                gfx.renderer.screen_width(),
                gfx.renderer.screen_height(),
                core.menu.render_distance,
            ));
            game.benchmark_result = None;
            game.pause_screen = PauseScreen::Main;
            game.paused = false;
            core.apply_cursor_grab(&gfx.window, Some(game));
        }
        PauseAction::StartChunkLoad(rd) => {
            start_chunk_load(core, gfx, connection, game, rd);
            game.pause_screen = PauseScreen::Main;
            game.paused = false;
            core.apply_cursor_grab(&gfx.window, Some(game));
        }
        PauseAction::ReportBugs => {
            crate::util::open_url("https://github.com/PommeMC/Client/issues");
        }
        PauseAction::None => {}
    }

    // Vanilla `Options.broadcastOptions`, which sends only what changed.
    let (chat_options, skin_options) = (core.menu.chat_options, core.menu.skin_options());
    if core.menu.take_options_saved()
        && (core.menu.render_distance != game.last_render_distance
            || game.client_options_changed(chat_options, skin_options))
    {
        game.sync_client_information(
            connection,
            core.menu.render_distance,
            chat_options,
            skin_options,
        );
    }
    if game.options_from_game {
        // TODO: a resource-pack toggle's `menu.reload_assets` is only applied
        // once back on the title screen.
        core.apply_font_options(&mut gfx.renderer);
        if !core.menu.is_options_screen() {
            game.options_from_game = false;
            // Chat Settings opened from chat return to it, unpaused.
            game.paused = !game.chat.return_from_settings(game.command_tree.as_deref());
            core.apply_cursor_grab(&gfx.window, Some(game));
        }
    }

    GameUpdateResult::None
}

fn stack_render_count(count: i32) -> usize {
    if count <= 1 {
        1
    } else if count <= 16 {
        2
    } else if count <= 32 {
        3
    } else if count <= 48 {
        4
    } else {
        5
    }
}

/// Builds the rain/snow columns in a square around the camera (vanilla
/// WeatherEffectRenderer.extractRenderState). Returns empty when it is not
/// raining or when no precipitation biomes are nearby.
fn build_weather_columns(
    world_light: &WorldLight,
    biome_climate: &HashMap<u32, BiomeClimate>,
    cam: glam::DVec3,
    rain: f32,
) -> Vec<crate::renderer::WeatherColumn> {
    use crate::renderer::WeatherColumn;
    use crate::renderer::pipelines::weather::{Precip, WEATHER_RADIUS, precipitation_for};

    if rain <= 0.0 {
        return Vec::new();
    }

    let chunk_store = world_light.chunks;
    let cam_x = cam.x.floor() as i32;
    let cam_y = cam.y.floor() as i32;
    let cam_z = cam.z.floor() as i32;

    let mut columns = Vec::new();
    for dz in -WEATHER_RADIUS..=WEATHER_RADIUS {
        for dx in -WEATHER_RADIUS..=WEATHER_RADIUS {
            let wx = cam_x + dx;
            let wz = cam_z + dz;
            let terrain = chunk_store.motion_blocking_height(wx, wz);
            let y0 = (cam_y - WEATHER_RADIUS).max(terrain);
            let y1 = (cam_y + WEATHER_RADIUS).max(terrain);
            if y1 - y0 == 0 {
                continue;
            }
            let climate = biome_climate
                .get(&chunk_store.biome_id(wx, cam_y, wz))
                .copied()
                .unwrap_or_default();
            let precip = precipitation_for(&climate, cam_y);
            if precip == Precip::None {
                continue;
            }
            let light = block_lighter::light_coords(world_light, [wx, cam_y.max(terrain), wz]);
            columns.push(WeatherColumn {
                x: wx,
                z: wz,
                bottom_y: y0 as f32,
                top_y: y1 as f32,
                precip,
                light,
            });
        }
    }
    columns
}

fn item_stack_seed(item_id: u32, damage: i32) -> i64 {
    (item_id as i32).wrapping_add(damage) as i64
}

fn transform_item_bounds(
    min: glam::Vec3,
    max: glam::Vec3,
    transform: glam::Mat4,
) -> (glam::Vec3, glam::Vec3) {
    let mut out_min = glam::Vec3::splat(f32::INFINITY);
    let mut out_max = glam::Vec3::splat(f32::NEG_INFINITY);
    for x in [min.x, max.x] {
        for y in [min.y, max.y] {
            for z in [min.z, max.z] {
                let point = transform.transform_point3(glam::Vec3::new(x, y, z));
                out_min = out_min.min(point);
                out_max = out_max.max(point);
            }
        }
    }
    (out_min, out_max)
}

#[cfg(test)]
mod dropped_item_tests {
    use azalea_registry::builtin::EntityKind;

    use super::{
        hand_visible, item_stack_seed, should_render_local_player, sleeping_head_pitch_deg,
        standing_eye_height, transform_item_bounds,
    };

    #[test]
    fn dropped_item_scatter_seed_includes_damage() {
        assert_eq!(item_stack_seed(42, 0), 42);
        assert_eq!(item_stack_seed(42, 7), 49);
        assert_eq!(item_stack_seed(u32::MAX, 2), 1);
    }

    #[test]
    fn sleeping_head_pitch_is_zero_like_vanilla_living_entity_tick() {
        assert_eq!(sleeping_head_pitch_deg(true, -43.0), 0.0);
        assert_eq!(sleeping_head_pitch_deg(true, 61.5), 0.0);
        assert_eq!(sleeping_head_pitch_deg(false, -17.25), -17.25);
    }

    #[test]
    fn first_person_sleeping_player_remains_rendered_like_vanilla() {
        assert!(!should_render_local_player(false, true, false, false));
        assert!(should_render_local_player(false, true, true, false));
        assert!(should_render_local_player(false, false, false, false));
        assert!(!should_render_local_player(true, false, true, false));
        assert!(!should_render_local_player(false, false, true, true));
    }

    #[test]
    fn hand_hides_asleep_with_f1_and_in_spectator() {
        assert!(hand_visible(false, false, 0));
        assert!(hand_visible(false, false, 1));
        assert!(!hand_visible(true, false, 0));
        assert!(!hand_visible(false, true, 0));
        assert!(!hand_visible(false, false, 3));
    }

    #[test]
    fn villager_sleeping_eye_height_uses_explicit_baby_dimensions() {
        assert_eq!(standing_eye_height(EntityKind::Villager, false), 1.62);
        assert_eq!(standing_eye_height(EntityKind::Villager, true), 0.63);
    }

    #[test]
    fn transformed_bounds_follow_ground_display_transform() {
        let transform = glam::Mat4::from_translation(glam::Vec3::new(0.0, 3.0 / 16.0, 0.0))
            * glam::Mat4::from_scale(glam::Vec3::splat(0.5));
        let (min, max) =
            transform_item_bounds(glam::Vec3::splat(-0.5), glam::Vec3::splat(0.5), transform);

        assert!((min - glam::Vec3::new(-0.25, -0.0625, -0.25)).length() < 1.0e-6);
        assert!((max - glam::Vec3::new(0.25, 0.4375, 0.25)).length() < 1.0e-6);
    }
}

fn dropped_item_geometry(renderer: &Renderer, item_name: &str) -> (glam::Mat4, f32, f32) {
    let mesh = renderer.item_mesh_info(item_name);
    let is_block_model = mesh
        .map(|mesh| mesh.is_block_model)
        .unwrap_or_else(|| renderer.render_assets().get_item_model(item_name).is_some());
    let fallback_transform = if is_block_model {
        crate::world::block::model::default_block_ground_transform()
    } else {
        glam::Mat4::from_scale(glam::Vec3::splat(0.5))
    };
    let ground_transform = renderer
        .render_assets()
        .get_item_ground_transform(item_name)
        .unwrap_or(fallback_transform);
    let (bounds_min, bounds_max) = mesh
        .map(|mesh| (mesh.bounds_min, mesh.bounds_max))
        .unwrap_or_else(|| {
            if is_block_model {
                (glam::Vec3::splat(-0.5), glam::Vec3::splat(0.5))
            } else {
                (
                    glam::Vec3::new(-0.5, -0.5, -1.0 / 32.0),
                    glam::Vec3::new(0.5, 0.5, 1.0 / 32.0),
                )
            }
        });
    let (min, max) = transform_item_bounds(bounds_min, bounds_max, ground_transform);
    (ground_transform, min.y, max.z - min.z)
}

/// Emits the hovering, spinning, multi-copy cluster for one dropped item,
/// shared by resting items and the pickup fly-animation. `ground_transform`
/// is the model's resolved GROUND display transform; `min_y` and `z_size` are
/// the bounds after that transform, matching `ItemStackRenderState`.
#[allow(clippy::too_many_arguments)]
fn emit_item_copies(
    infos: &mut Vec<crate::renderer::pipelines::item_entity::ItemRenderInfo>,
    render_assets: &crate::world::block::render_assets::BlockRenderAssets,
    item_name: &str,
    item_id: u32,
    damage: i32,
    count: i32,
    item_stack: Option<&azalea_inventory::ItemStackData>,
    anchor_rel_pos: glam::Vec3,
    age_f: f32,
    bob_offset: f32,
    ground_transform: glam::Mat4,
    min_y: f32,
    z_size: f32,
    light: LightLevels,
    outline_color: Option<u32>,
) {
    use crate::renderer::pipelines::item_entity::ItemRenderInfo;
    use crate::util::JavaRandom;

    let bob = (age_f / 10.0 + bob_offset).sin() * 0.1 + 0.1;
    let spin = age_f / 20.0 + bob_offset;
    let copies = stack_render_count(count);
    // hover = bob + (-modelBoundingBox.minY) + 0.0625
    let hover_y = bob - min_y + 0.0625;

    let base = glam::Mat4::from_translation(anchor_rel_pos + glam::Vec3::new(0.0, hover_y, 0.0))
        * glam::Mat4::from_rotation_y(spin);
    // Dropped item entities are not LivingEntity owners in vanilla, so a
    // `team` tint source falls back to the JSON default here.
    let tints: std::sync::Arc<[u32]> = render_assets
        .item_tint_palette(item_name, item_stack, None)
        .into();
    let name: std::sync::Arc<str> = item_name.into();
    let mut push = |copy_offset: glam::Mat4| {
        infos.push(ItemRenderInfo {
            item_name: name.clone(),
            outline_color,
            tints: tints.clone(),
            model_matrix: base * copy_offset * ground_transform,
            light,
            white_flash: false,
        });
    };

    // ItemClusterRenderState.getSeedForItemStack: registry id + damage value.
    let mut rng = JavaRandom::new(item_stack_seed(item_id, damage));
    let mut jitter = |spread: f32| (rng.next_float() * 2.0 - 1.0) * spread;

    if z_size > 0.0625 {
        push(glam::Mat4::IDENTITY);
        for _ in 1..copies {
            let off = glam::Vec3::new(jitter(0.15), jitter(0.15), jitter(0.15));
            push(glam::Mat4::from_translation(off));
        }
    } else {
        let z_step = z_size * 1.5;
        let z_start = -(z_step * (copies - 1) as f32 / 2.0);
        push(glam::Mat4::from_translation(glam::Vec3::new(
            0.0, 0.0, z_start,
        )));
        for i in 1..copies {
            let z = z_start + z_step * i as f32;
            let off = glam::Vec3::new(jitter(0.15 * 0.5), jitter(0.15 * 0.5), z);
            push(glam::Mat4::from_translation(off));
        }
    }
}

/// Vanilla `PistonHeadRenderer.extractRenderState`: each moving block, meshed
/// as terrain at its offset from the moving piston's cell and lit from the
/// cell it left. A retracting piston also draws its base, extended, in place.
fn moving_block_renders(
    game: &GameState,
    camera_pos: glam::DVec3,
    partial_tick: f32,
) -> Vec<crate::renderer::chunk::moving_blocks::MovingBlockRender> {
    use crate::world::piston;
    /// Vanilla `PistonHeadRenderer.getViewDistance`.
    const VIEW_DISTANCE: f64 = 68.0;
    let blocks = game.block_registry.block_state_table();
    let mut renders = Vec::new();
    for (pos, moving) in game.block_entity_anim.pistons.iter() {
        let state = moving.moved_state;
        let data = blocks.block_state_data(state);
        let center = glam::DVec3::new(pos.x as f64, pos.y as f64, pos.z as f64) + 0.5;
        if data.is_air() || center.distance_squared(camera_pos) >= VIEW_DISTANCE * VIEW_DISTANCE {
            continue;
        }
        let origin = [pos.x, pos.y, pos.z];
        let cell = piston::relative(*pos, moving.movement_direction().opposite(), 1);
        let cell = [cell.x, cell.y, cell.z];
        let mut mesh = |state, cell, base| {
            renders.push(crate::renderer::chunk::moving_blocks::MovingBlockRender {
                origin,
                mesh: game
                    .mesh_dispatcher
                    .mesh_moving_block(&game.chunk_store, state, cell, base),
            });
        };
        let progress = moving.progress(partial_tick);
        let offset = moving.offset(partial_tick);
        if data.id() == "piston_head" && progress <= 4.0 {
            mesh(
                piston::with_short(blocks, state, progress <= 0.5),
                cell,
                offset,
            );
        } else if moving.source && !moving.extending {
            let Some(head) = piston::head_of(blocks, state, progress >= 0.5) else {
                continue;
            };
            mesh(head, cell, offset);
            let base = blocks
                .with_property(state, "extended", "true")
                .unwrap_or(state);
            mesh(base, origin, [0.0; 3]);
        } else {
            mesh(state, cell, offset);
        }
    }
    renders
}

/// Vanilla `extractShadow`'s casters: every rendered entity, and each resting
/// item entity once, at its own position rather than its bobbing copies'.
/// Items being picked up are gone in vanilla, so they cast none, and the
/// first-person player is only in the list for shader pack shadows.
fn shadow_casters(
    entities: &[EntityRenderInfo],
    items: &crate::entity::ItemEntityStore,
    camera_pos: glam::DVec3,
    partial_tick: f32,
) -> Vec<ShadowCaster> {
    let (item_radius, item_strength) = crate::entity::shadow::ITEM;
    let entities = entities
        .iter()
        .filter(|e| e.pack_shadow != PackShadow::FirstPersonPlayer)
        .map(|e| e.shadow);
    // A full-strength shadow fades out 16 blocks from the camera.
    let items = items
        .visible_items(camera_pos, 16.0)
        .into_iter()
        .map(|item| ShadowCaster {
            position: *item.prev_position.lerp(item.position, partial_tick as f64),
            radius: item_radius,
            strength: item_strength,
            ..Default::default()
        });
    entities.chain(items).collect()
}

#[allow(clippy::too_many_arguments)]
fn build_item_render_infos(
    entity_store: &crate::entity::ItemEntityStore,
    world_light: &WorldLight,
    renderer: &Renderer,
    camera_pos: glam::DVec3,
    anchor: glam::DVec3,
    partial_tick: f32,
    outline: impl Fn(&crate::entity::ItemEntity) -> Option<u32>,
) -> Vec<crate::renderer::pipelines::item_entity::ItemRenderInfo> {
    // TODO: an item's fire flag, and its fire immunity.
    let light = |pos: Position| {
        let eye_height = crate::entity::dimensions::EntityDimensions::eye_height(EntityKind::Item);
        let eye = pos + glam::dvec3(0.0, f64::from(eye_height), 0.0);
        entity_light(world_light, EntityKind::Item, eye, false, 0)
    };
    let mut infos = Vec::new();
    for item in entity_store.visible_items(camera_pos, 64.0) {
        let age_f = item.age as f32 + partial_tick;
        let lerped = item.prev_position.lerp(item.position, partial_tick as f64);
        let (ground_transform, min_y, z_size) = dropped_item_geometry(renderer, &item.item_name);
        emit_item_copies(
            &mut infos,
            renderer.render_assets(),
            &item.item_name,
            item.item_id,
            item.damage,
            item.count,
            item.stack.as_ref(),
            (*lerped - anchor).as_vec3(),
            age_f,
            item.bob_offset,
            ground_transform,
            min_y,
            z_size,
            light(lerped),
            outline(item),
        );
    }

    // Pickup fly-animation: the cluster at the lerped position, age frozen at
    // pickup.
    for pickup in entity_store.active_pickups(partial_tick) {
        let age_f = pickup.age as f32 + partial_tick;
        let (ground_transform, min_y, z_size) = dropped_item_geometry(renderer, &pickup.item_name);
        emit_item_copies(
            &mut infos,
            renderer.render_assets(),
            &pickup.item_name,
            pickup.item_id,
            pickup.damage,
            pickup.count,
            pickup.stack.as_ref(),
            (*pickup.position - anchor).as_vec3(),
            age_f,
            pickup.bob_offset,
            ground_transform,
            min_y,
            z_size,
            // TODO: vanilla's `ItemPickupParticle` keeps the item's light from
            // the pickup.
            light(pickup.position),
            // Vanilla's `ItemPickupParticle` clears its outline.
            None,
        );
    }

    infos
}

/// Vanilla `DisplayRenderer`: each display entity in view range, posed at its
/// lerped position. Block and item displays draw through the item pipeline;
/// text displays come back separately, back to front.
/// What [`build_display_renders`] draws: item and block displays as items,
/// skull items as heads (with the skins to fetch), and the texts back to
/// front.
#[derive(Default)]
struct DisplayRenders {
    items: Vec<crate::renderer::pipelines::item_entity::ItemRenderInfo>,
    heads: Vec<EntityRenderInfo>,
    head_skins: Vec<(uuid::Uuid, pomme_protocol::entity_data::Profile)>,
    texts: Vec<(
        f64,
        crate::renderer::pipelines::menu_overlay::WorldTextRender,
    )>,
}

/// Item frames (vanilla `ItemFrameRenderer.submit`): the frame's body, then
/// the item it holds in the `fixed` view, pushed onto `items` (a head onto
/// `heads`, with the skin a player's head wants onto `head_skins`); or the
/// map it holds, returned as (map id, pose, light) for the map pipeline.
fn build_item_frame_renders(
    store: &mut crate::entity::item_frame::ItemFrameStore,
    world_light: &WorldLight,
    renderer: &mut Renderer,
    items: &mut Vec<crate::renderer::pipelines::item_entity::ItemRenderInfo>,
    heads: &mut Vec<EntityRenderInfo>,
    head_skins: &mut Vec<(uuid::Uuid, pomme_protocol::entity_data::Profile)>,
) -> Vec<(i32, glam::Mat4, LightLevels)> {
    use crate::renderer::pipelines::item_entity::ItemRenderInfo;
    use crate::world::block::model::ITEM_FRAME_MODELS;

    /// `ItemDisplayContext.FIXED`.
    const FIXED: u8 = 8;
    let eye = renderer.camera_render_position();
    let anchor = renderer.camera_anchor();
    let frustum = renderer.frustum_planes();
    let generation = renderer.display_assets_generation();
    let bodies: [Arc<str>; 4] = ITEM_FRAME_MODELS.map(|(key, _)| Arc::from(key));
    let no_tints: Arc<[u32]> = Arc::from([]);
    let mut maps = Vec::new();
    for frame in store.frames.values_mut() {
        // The frame and what it holds stay inside its block's space.
        let center = frame.block_center();
        let q = (center - eye).as_vec3();
        if frustum
            .iter()
            .any(|p| p[0] * q.x + p[1] * q.y + p[2] * q.z + p[3] < -0.87)
        {
            continue;
        }
        let base =
            glam::Mat4::from_translation((center - anchor).as_vec3()) * frame.facing_matrix();
        let position = frame.position();
        let world = entity_light(
            world_light,
            EntityKind::ItemFrame,
            Position::new(position.x, position.y, position.z),
            false,
            0,
        );
        // `ItemFrameRenderer.getLightCoords`: a glow frame's contents are
        // full bright (a map a little less), and its body lit at 5 or more.
        let lit = |sky: u8, block: u8| {
            if frame.glow {
                LightLevels::new(sky, block)
            } else {
                world
            }
        };
        let map = frame.map_id();
        if !frame.invisible {
            let body = usize::from(frame.glow) * 2 + usize::from(map.is_some());
            items.push(ItemRenderInfo {
                item_name: bodies[body].clone(),
                outline_color: None,
                tints: no_tints.clone(),
                model_matrix: base,
                light: lit(world.sky(), world.block().max(5)),
                white_flash: false,
            });
        }
        if let Some(id) = map {
            maps.push((id, base * frame.map_matrix(), lit(15, 13)));
            continue;
        }
        let Some(stack) = &frame.item else {
            continue;
        };
        // Vanilla `SkullSpecialRenderer`: a head item is the head itself,
        // as an item display's is: the floor skull standing on the item's
        // origin, posed by the item's `fixed` display, which turns its face
        // out of the frame.
        let name = crate::player::inventory::item_resource_name(stack.kind);
        if renderer.render_assets().stack_model_key(&name, stack) == name
            && let Some(model) = crate::entity::heads::skull_model(&name)
            && let Some((offset, yaw)) = crate::entity::heads::placed_skull(Some("8"), None)
        {
            let skin = if model == EntityKind::Player {
                let stack = azalea_inventory::ItemStack::Present(stack.clone());
                crate::entity::heads::item_profile(&stack).map(|profile| {
                    let key = crate::entity::heads::skin_key(&profile);
                    head_skins.push((key, profile));
                    key
                })
            } else {
                None
            };
            let (scale, lift) = crate::entity::heads::head_placement(model, offset.y, 0.5);
            let full = base
                * frame.item_matrix()
                * renderer.item_display_transform(&name, FIXED)
                * glam::Mat4::from_translation(glam::Vec3::new(
                    offset.x as f32 - 0.5,
                    lift as f32 - 0.5,
                    offset.z as f32 - 0.5,
                ))
                * glam::Mat4::from_rotation_y((180.0 - yaw).to_radians())
                * glam::Mat4::from_scale(glam::Vec3::splat(scale));
            let origin = full.w_axis.truncate();
            heads.push(EntityRenderInfo {
                position: Position::from(anchor + origin.as_dvec3()),
                head_y_rot_deg: 180.0,
                body_y_rot_deg: 180.0,
                entity_kind: model,
                // A player head without a skin draws vanilla's wide Steve.
                variant_index: crate::renderer::pipelines::entity_renderer::WIDE_STEVE_SKIN,
                player_uuid: skin,
                head_only: true,
                body_transform: Some(glam::Mat4::from_translation(-origin) * full),
                light: lit(15, 15),
                skip_cull: true,
                ..Default::default()
            });
            continue;
        }
        if frame
            .item_render
            .as_ref()
            .is_none_or(|cached| cached.generation != generation)
        {
            let model = renderer.render_assets().stack_model_key(&name, stack);
            let model = renderer.render_assets().context_model_key(&model, FIXED);
            renderer.ensure_item_mesh(&model);
            let tints = renderer
                .render_assets()
                .item_tint_palette(&model, Some(stack), None)
                .into();
            let transform = renderer.item_display_transform(&model, FIXED);
            frame.item_render = Some(crate::entity::item_frame::FrameItemRender {
                generation,
                model: model.into(),
                tints,
                transform,
            });
        }
        let Some(cached) = &frame.item_render else {
            continue;
        };
        items.push(ItemRenderInfo {
            item_name: cached.model.clone(),
            outline_color: None,
            tints: cached.tints.clone(),
            model_matrix: base * frame.item_matrix() * cached.transform,
            light: lit(15, 15),
            white_flash: false,
        });
    }
    maps
}

fn build_display_renders(
    store: &mut crate::entity::display::DisplayStore,
    world_light: &WorldLight,
    renderer: &mut Renderer,
    table: crate::world::block::BlockStateTable,
    partial_tick: f32,
) -> DisplayRenders {
    use crate::entity::display::{DisplayKind, TEXT_SEE_THROUGH, TEXT_SHADOW};
    use crate::renderer::pipelines::item_entity::ItemRenderInfo;
    use crate::renderer::pipelines::menu_overlay::WorldTextRender;

    let eye = renderer.camera_render_position();
    let anchor = renderer.camera_anchor();
    let camera_rot = renderer.camera_look_deg();
    let frustum = renderer.frustum_planes();
    let generation = renderer.display_assets_generation();
    // The camera-relative frustum test for a sphere around anchor-relative
    // `center`. Vanilla leaves sizeless displays unculled; skipping one that
    // is wholly off screen draws the same picture.
    let eye_rel = (eye - anchor).as_vec3();
    let on_screen = |center: glam::Vec3, radius: f32| {
        let q = center - eye_rel;
        !frustum
            .iter()
            .any(|p| p[0] * q.x + p[1] * q.y + p[2] * q.z + p[3] < -radius)
    };
    let max_scale = |m: &glam::Mat4| {
        m.x_axis
            .truncate()
            .length()
            .max(m.y_axis.truncate().length())
            .max(m.z_axis.truncate().length())
    };
    let mut items = Vec::new();
    let mut texts = Vec::new();
    let mut head_renders = Vec::new();
    let mut heads = Vec::new();
    for display in store.displays.values_mut() {
        let lerped = display
            .prev_position
            .lerp(display.position, f64::from(partial_tick));
        if !display.in_view_range(eye, lerped) {
            continue;
        }
        // `getBoundingBoxForCulling`, tested as its bounding sphere.
        if let Some((min, max)) = display.culling_box(lerped) {
            let center = ((min + max) * 0.5 - eye).as_vec3();
            let radius = ((max - min) * 0.5).length() as f32;
            if frustum
                .iter()
                .any(|p| p[0] * center.x + p[1] * center.y + p[2] * center.z + p[3] < -radius)
            {
                continue;
            }
        }
        let Some(pose) = display.pose(partial_tick, camera_rot) else {
            continue;
        };
        let base = glam::Mat4::from_translation((lerped - anchor).as_vec3()) * pose;
        let light = match display.brightness_override() {
            Some((sky, block)) => LightLevels::new(sky, block),
            None => entity_light(
                world_light,
                EntityKind::BlockDisplay,
                Position::new(lerped.x, lerped.y, lerped.z),
                false,
                0,
            ),
        };
        // `Display.getTeamColor`: the override, else the (white) team colour.
        let outline_color = display
            .is_glowing()
            .then(|| display.glow_color_override().unwrap_or(0xFF_FFFF));
        match display.kind {
            DisplayKind::Block => {
                let Some(state) = display.block_state() else {
                    continue;
                };
                // Air draws nothing (servers seat riders on air displays).
                if table
                    .try_state(state)
                    .is_some_and(|state| table.block_state_data(state).is_air())
                {
                    continue;
                }
                let Some((item_name, tints)) = renderer.ensure_block_display_mesh(table, state)
                else {
                    continue;
                };
                // The baked meshes are centred; vanilla's block model spans
                // 0..1 from the entity's origin.
                let model_matrix = base * glam::Mat4::from_translation(glam::Vec3::splat(0.5));
                let radius = 0.87 * max_scale(&model_matrix);
                if !on_screen(model_matrix.w_axis.truncate(), radius) {
                    continue;
                }
                items.push(ItemRenderInfo {
                    item_name,
                    outline_color,
                    tints,
                    model_matrix,
                    light,
                    white_flash: false,
                });
            }
            DisplayKind::Item => {
                // Vanilla `SkullSpecialRenderer`: a head item is the head
                // itself, the floor skull turned about (rotation 8) standing
                // on the item's origin, posed by the item's display.
                if let Some((stack, context)) = display.item()
                    && let name = crate::player::inventory::item_resource_name(stack.kind)
                    && renderer.render_assets().stack_model_key(&name, stack) == name
                    && let Some(model) = crate::entity::heads::skull_model(&name)
                {
                    let skin = if model == EntityKind::Player {
                        let stack = azalea_inventory::ItemStack::Present(stack.clone());
                        crate::entity::heads::item_profile(&stack).map(|profile| {
                            let key = crate::entity::heads::skin_key(&profile);
                            heads.push((key, profile));
                            key
                        })
                    } else {
                        None
                    };
                    let Some((offset, yaw)) = crate::entity::heads::placed_skull(Some("8"), None)
                    else {
                        continue;
                    };
                    let (scale, lift) = crate::entity::heads::head_placement(model, offset.y, 0.5);
                    let full = base
                        * glam::Mat4::from_rotation_y(std::f32::consts::PI)
                        * renderer.item_display_transform(&name, context)
                        * glam::Mat4::from_translation(glam::Vec3::new(
                            offset.x as f32 - 0.5,
                            lift as f32 - 0.5,
                            offset.z as f32 - 0.5,
                        ))
                        * glam::Mat4::from_rotation_y((180.0 - yaw).to_radians())
                        * glam::Mat4::from_scale(glam::Vec3::splat(scale));
                    let origin = full.w_axis.truncate();
                    if !on_screen(origin, max_scale(&full)) {
                        continue;
                    }
                    head_renders.push(EntityRenderInfo {
                        position: Position::from(anchor + origin.as_dvec3()),
                        head_y_rot_deg: 180.0,
                        body_y_rot_deg: 180.0,
                        entity_kind: model,
                        // A player head without a skin draws vanilla's wide Steve.
                        variant_index: crate::renderer::pipelines::entity_renderer::WIDE_STEVE_SKIN,
                        player_uuid: skin,
                        head_only: true,
                        body_transform: Some(glam::Mat4::from_translation(-origin) * full),
                        light,
                        outline_color,
                        skip_cull: true,
                        ..Default::default()
                    });
                    continue;
                }
                if display
                    .item_render
                    .as_ref()
                    .is_none_or(|cached| cached.generation != generation)
                {
                    let Some((stack, context)) = display.item() else {
                        continue;
                    };
                    let name = crate::player::inventory::item_resource_name(stack.kind);
                    let model = renderer.render_assets().stack_model_key(&name, stack);
                    let model = renderer.render_assets().context_model_key(&model, context);
                    let info = renderer.ensure_item_mesh(&model);
                    let tints: Arc<[u32]> = renderer
                        .render_assets()
                        .item_tint_palette(&model, Some(stack), None)
                        .into();
                    let transform = renderer.item_display_transform(&model, context);
                    display.item_render = Some(crate::entity::display::ItemDisplayRender {
                        generation,
                        model: model.into(),
                        tints,
                        transform,
                        bounds: (info.bounds_min, info.bounds_max),
                    });
                }
                let Some(cached) = &display.item_render else {
                    continue;
                };
                // `ItemDisplayRenderer.submitInner` turns the item around.
                let model_matrix =
                    base * glam::Mat4::from_rotation_y(std::f32::consts::PI) * cached.transform;
                let (min, max) = cached.bounds;
                let center = model_matrix.transform_point3((min + max) * 0.5);
                let radius = ((max - min) * 0.5).length() * max_scale(&model_matrix);
                if !on_screen(center, radius) {
                    continue;
                }
                items.push(ItemRenderInfo {
                    item_name: cached.model.clone(),
                    outline_color,
                    tints: cached.tints.clone(),
                    model_matrix,
                    light,
                    white_flash: false,
                });
            }
            DisplayKind::Text => {
                let layout = match &display.text_layout {
                    Some(layout) => layout.clone(),
                    None => {
                        let Some((spans, line_width)) = display.text() else {
                            continue;
                        };
                        let layout = Arc::new(renderer.layout_display_text(spans, line_width));
                        display.text_layout = Some(layout.clone());
                        layout
                    }
                };
                if layout.lines.is_empty() {
                    continue;
                }
                // The text's rectangle, scaled by `submitInner`'s 0.025.
                let half =
                    glam::vec2(layout.width + 2.0, layout.lines.len() as f32 * 10.0 + 1.0) * 0.025;
                if !on_screen(base.w_axis.truncate(), half.length() * max_scale(&base)) {
                    continue;
                }
                let flags = display.text_flags();
                texts.push((
                    lerped.distance_squared(eye),
                    WorldTextRender {
                        layout,
                        pose: base,
                        align: display.text_align(),
                        color: display.text_color(partial_tick),
                        // Vanilla's `getBackgroundOpacity(0.25)` with text
                        // backgrounds for chat only, the default.
                        background: display.text_background(partial_tick, 0.25),
                        shadow: flags & TEXT_SHADOW != 0,
                        see_through: flags & TEXT_SEE_THROUGH != 0,
                        light: light.lane(),
                    },
                ));
            }
        }
    }
    // Copies of one mesh draw as one instanced call only when consecutive.
    items.sort_unstable_by(|a, b| a.item_name.cmp(&b.item_name));
    DisplayRenders {
        items,
        heads: head_renders,
        head_skins: heads,
        texts,
    }
}

/// Placed banners within 64 blocks (vanilla `BannerRenderer`): the banner
/// item's own model, with the block entity's pattern layers, standing in
/// the block turned to its sixteenth or hung against its wall.
// TODO: the flag's sway, and a wall banner's model, which has a bar and no
// pole (this draws the standing one, its pole behind the cloth).
fn build_banners(
    game: &GameState,
    world_light: &WorldLight,
    renderer: &mut Renderer,
    out: &mut Vec<crate::renderer::pipelines::item_entity::ItemRenderInfo>,
) {
    use azalea_registry::builtin::BlockEntityKind;
    use glam::{Mat4, Vec3};

    use crate::renderer::pipelines::item_entity::ItemRenderInfo;
    let eye = renderer.camera_render_position();
    let anchor = renderer.camera_anchor();
    for (pos, be) in &game.chunk_store.block_entities {
        if be.kind != BlockEntityKind::Banner {
            continue;
        }
        let corner = glam::DVec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
        if (corner + glam::DVec3::splat(0.5)).distance_squared(eye) > 64.0 * 64.0 {
            continue;
        }
        let state = game.chunk_store.get_block_state(pos.x, pos.y, pos.z);
        let data = game.block_registry.block_state_data(state);
        let id = data.id();
        let Some(color) = id.strip_suffix("_banner") else {
            continue;
        };
        let (color, wall) = match color.strip_suffix("_wall") {
            Some(color) => (color, true),
            None => (color, false),
        };
        // The model key a stack of this banner takes: its item, then each
        // layer's pattern and colour (`crate::banner::layers`).
        let mut key = format!("{color}_banner");
        let layers = banner_layers(&be.nbt);
        if !layers.is_empty() {
            key.push('~');
            key.push_str(&layers.join(","));
        }
        renderer.ensure_item_mesh(&key);
        let tints: Arc<[u32]> = renderer
            .render_assets()
            .item_tint_palette(&key, None, None)
            .into();
        let props = data.properties();
        // The mesh is centred on its block; the banner turns about the
        // pole's foot, at the middle of the block's floor.
        let centre = Vec3::splat(0.5);
        let foot = Vec3::new(0.0, 0.5, 0.0);
        let placement = if wall {
            let degrees = match props.get("facing") {
                Some("west") => 90.0,
                Some("north") => 180.0,
                Some("east") => 270.0,
                _ => 0.0_f32,
            };
            Mat4::from_translation(Vec3::new(0.5, -1.0 / 6.0, 0.5))
                * Mat4::from_rotation_y((-degrees).to_radians())
                * Mat4::from_translation(Vec3::new(0.0, -0.3125, -0.4375))
                * Mat4::from_translation(foot)
        } else {
            let sixteenths = props
                .get("rotation")
                .and_then(|r| r.parse::<f32>().ok())
                .unwrap_or(0.0);
            Mat4::from_translation(centre)
                * Mat4::from_rotation_y((-sixteenths * 22.5).to_radians())
        };
        out.push(ItemRenderInfo {
            item_name: key.into(),
            outline_color: None,
            tints,
            model_matrix: Mat4::from_translation((corner - anchor).as_vec3()) * placement,
            light: block_lighter::light_coords(world_light, [pos.x, pos.y, pos.z]),
            white_flash: false,
        });
    }
}

/// Placed dragon heads within 64 blocks (vanilla `SkullBlockRenderer` with
/// `DragonHeadModel`): the head item's own model, on the floor at its
/// sixteenth or on its wall. Decorated pots (`DecoratedPotRenderer`) and
/// conduits (`ConduitRenderer`) likewise, as their items' models.
// TODO: the head's jaw working while it is powered, a pot's wobble and
// sherds, and an active conduit's cage, wind and eye.
fn build_dragon_heads(
    game: &GameState,
    world_light: &WorldLight,
    renderer: &mut Renderer,
    out: &mut Vec<crate::renderer::pipelines::item_entity::ItemRenderInfo>,
) {
    use azalea_registry::builtin::BlockEntityKind;
    use glam::{Mat4, Vec3};

    use crate::renderer::pipelines::item_entity::ItemRenderInfo;
    let eye = renderer.camera_render_position();
    let anchor = renderer.camera_anchor();
    for (pos, be) in &game.chunk_store.block_entities {
        if !matches!(
            be.kind,
            BlockEntityKind::Skull | BlockEntityKind::DecoratedPot | BlockEntityKind::Conduit
        ) {
            continue;
        }
        let corner = glam::DVec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
        if (corner + glam::DVec3::splat(0.5)).distance_squared(eye) > 64.0 * 64.0 {
            continue;
        }
        let state = game.chunk_store.get_block_state(pos.x, pos.y, pos.z);
        let data = game.block_registry.block_state_data(state);
        let props = data.properties();
        // Each item's mesh is centred on its block.
        let centre = Mat4::from_translation(Vec3::splat(0.5));
        let (item, placement) = match data.id() {
            "dragon_head" | "dragon_wall_head" => {
                let Some((offset, yaw)) =
                    crate::entity::heads::placed_skull(props.get("rotation"), props.get("facing"))
                else {
                    continue;
                };
                // The item's head stands on the middle of the floor, facing
                // the other way from a placed one.
                let placement = Mat4::from_translation(offset.as_vec3())
                    * Mat4::from_rotation_y((-yaw).to_radians())
                    * Mat4::from_translation(Vec3::new(0.0, 0.5, 0.0));
                ("dragon_head", placement)
            }
            // Turned to its facing; the item's faces north.
            "decorated_pot" => {
                let degrees = match props.get("facing") {
                    Some("south") => 180.0,
                    Some("west") => 90.0,
                    Some("east") => 270.0,
                    _ => 0.0_f32,
                };
                (
                    "decorated_pot",
                    centre * Mat4::from_rotation_y(degrees.to_radians()),
                )
            }
            "conduit" => ("conduit", centre),
            _ => continue,
        };
        renderer.ensure_item_mesh(item);
        out.push(ItemRenderInfo {
            item_name: Arc::from(item),
            outline_color: None,
            tints: Arc::from([]),
            model_matrix: Mat4::from_translation((corner - anchor).as_vec3()) * placement,
            light: block_lighter::light_coords(world_light, [pos.x, pos.y, pos.z]),
            white_flash: false,
        });
    }
}

/// A banner block entity's pattern layers as [`crate::banner::layers`]
/// reads them: each `pattern` (a registry id, whose asset shares its name,
/// or one spelled out) with its dye's colour.
fn banner_layers(nbt: &simdnbt::owned::NbtCompound) -> Vec<String> {
    let Some(patterns) = nbt.list("patterns").and_then(|list| list.compounds()) else {
        return Vec::new();
    };
    patterns
        .iter()
        .filter_map(|layer| {
            let asset = match layer.string("pattern") {
                Some(id) => id.to_str().into_owned(),
                None => layer
                    .compound("pattern")?
                    .string("asset_id")?
                    .to_str()
                    .into_owned(),
            };
            let asset = if asset.contains(':') {
                asset
            } else {
                format!("minecraft:{asset}")
            };
            let color = layer.string("color")?.to_str();
            Some(format!(
                "{asset}={:06x}",
                crate::banner::dye_rgb_named(&color)
            ))
        })
        .collect()
}

/// Runs the books of the enchanting tables within 64 blocks
/// (`bookAnimationTick`): each opens to the nearest player within three
/// blocks of its table's middle.
fn tick_enchanting_books(game: &mut GameState) {
    use azalea_registry::builtin::BlockEntityKind;
    let me = *game.player.position;
    // A spectator doesn't count as near (`EntitySelector.NO_SPECTATORS`).
    let watching = crate::player::is_spectator(game.player.game_mode);
    let players: Vec<glam::DVec3> = (!watching)
        .then_some(me)
        .into_iter()
        .chain(
            game.entity_store
                .living
                .values()
                .filter(|e| e.entity_type == EntityKind::Player)
                .map(|e| *e.position),
        )
        .collect();
    let mut near = Vec::new();
    for (pos, be) in &game.chunk_store.block_entities {
        if be.kind != BlockEntityKind::EnchantingTable {
            continue;
        }
        let middle = glam::DVec3::new(
            f64::from(pos.x) + 0.5,
            f64::from(pos.y) + 0.5,
            f64::from(pos.z) + 0.5,
        );
        if middle.distance_squared(me) > 64.0 * 64.0 {
            continue;
        }
        let reader = players
            .iter()
            .map(|p| (p.distance_squared(middle), *p))
            .filter(|(distance, _)| *distance < 9.0)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, p)| (p.x - middle.x, p.z - middle.z));
        game.enchanting_books.entry(*pos).or_default().tick(reader);
        near.push(*pos);
    }
    game.enchanting_books.retain(|pos, _| near.contains(pos));
}

/// `BaseSpawner.clientTick`: a spawner with a player within 16 blocks
/// throws a wisp of smoke and a flame inside its cage each tick, and its
/// mob spins.
// TODO: the spin speeds up as the spawn nears (the synced delay); this
// holds the delay at its 20 ticks.
fn tick_spawners(game: &mut GameState, camera: glam::DVec3) {
    use azalea_registry::builtin::BlockEntityKind;

    use crate::particle::{ParticleLook, ParticleOptions, ServerParticleKind};
    let me = *game.player.position;
    let watching = crate::player::is_spectator(game.player.game_mode);
    let players: Vec<glam::DVec3> = (!watching)
        .then_some(me)
        .into_iter()
        .chain(
            game.entity_store
                .living
                .values()
                .filter(|e| e.entity_type == EntityKind::Player)
                .map(|e| *e.position),
        )
        .collect();
    let mut active = Vec::new();
    for (pos, be) in &game.chunk_store.block_entities {
        if be.kind != BlockEntityKind::MobSpawner {
            continue;
        }
        let corner = glam::DVec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
        let middle = corner + glam::DVec3::splat(0.5);
        if !players.iter().any(|p| p.distance_squared(middle) < 256.0) {
            continue;
        }
        active.push((*pos, corner));
    }
    for (pos, corner) in &active {
        let at = *corner + glam::DVec3::new(fastrand::f64(), fastrand::f64(), fastrand::f64());
        for kind in [ServerParticleKind::Smoke, ServerParticleKind::Flame] {
            game.particle_store.add_particles_from_packet(
                kind,
                ParticleOptions::None,
                ParticleLook::None,
                false,
                at,
                glam::DVec3::ZERO,
                1.0,
                0,
                camera,
            );
        }
        let spin = game.spawner_spins.entry(*pos).or_insert((0.0, 0.0));
        *spin = (spin.1, (spin.1 + 1000.0 / 220.0) % 360.0);
    }
    game.spawner_spins
        .retain(|pos, _| active.iter().any(|(p, _)| p == pos));
}

/// The mob a spawner is set to, small and spinning in its cage
/// (`SpawnerRenderer`).
fn build_spawner_mobs(
    game: &GameState,
    world_light: &WorldLight,
    partial_tick: f32,
    out: &mut Vec<EntityRenderInfo>,
) {
    use azalea_registry::builtin::BlockEntityKind;
    use glam::{Mat4, Vec3};

    use crate::entity::dimensions::EntityDimensions;
    let eye = *game.player.position;
    for (pos, be) in &game.chunk_store.block_entities {
        if be.kind != BlockEntityKind::MobSpawner {
            continue;
        }
        let corner = glam::DVec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
        if corner.distance_squared(eye) > 64.0 * 64.0 {
            continue;
        }
        let Some(kind) = be
            .nbt
            .compound("SpawnData")
            .and_then(|data| data.compound("entity"))
            .and_then(|entity| entity.string("id"))
            .and_then(|id| id.to_str().parse::<EntityKind>().ok())
            .filter(|kind| crate::entity::is_living_mob(kind) && *kind != EntityKind::Player)
        else {
            continue;
        };
        let size = EntityDimensions::of(kind);
        let largest = size.width.max(size.height);
        let scale = if largest > 1.0 {
            0.53125 / largest
        } else {
            0.53125
        };
        let (before, now) = game.spawner_spins.get(pos).copied().unwrap_or_default();
        // The spin never wraps mid-tick the long way: it only grows.
        let spin = if now < before {
            now
        } else {
            before + (now - before) * partial_tick
        };
        // The mob faces south as its renderer draws it; the cage's pose
        // goes round that.
        let facing = Mat4::from_rotation_y(std::f32::consts::PI);
        let cage = Mat4::from_rotation_y((spin * 10.0).to_radians())
            * Mat4::from_translation(Vec3::new(0.0, -0.2, 0.0))
            * Mat4::from_rotation_x((-30.0_f32).to_radians())
            * Mat4::from_scale(Vec3::splat(scale));
        out.push(EntityRenderInfo {
            position: Position::new(corner.x + 0.5, corner.y + 0.4, corner.z + 0.5),
            entity_kind: kind,
            body_transform: Some(facing.inverse() * cage * facing),
            light: block_lighter::light_coords(world_light, [pos.x, pos.y, pos.z]),
            ..Default::default()
        });
    }
}

/// The book floating over each enchanting table (`EnchantTableRenderer`)
/// and lying open on each lectern that holds one (`LecternRenderer`).
fn build_books(
    game: &GameState,
    world_light: &WorldLight,
    partial_tick: f32,
    out: &mut Vec<EntityRenderInfo>,
) {
    use azalea_registry::builtin::BlockEntityKind;
    use glam::{Mat4, Vec3};

    use crate::renderer::pipelines::entity_renderer::{BOOK_KIND, unliving_model_frame};
    let unliving = unliving_model_frame();
    let book = |at: glam::DVec3, pose: Mat4, pos: &BlockPos| EntityRenderInfo {
        position: Position::new(at.x, at.y, at.z),
        entity_kind: BOOK_KIND,
        head_y_rot_deg: 180.0,
        body_y_rot_deg: 180.0,
        body_transform: Some(pose * unliving),
        light: block_lighter::light_coords(world_light, [pos.x, pos.y, pos.z]),
        ..Default::default()
    };
    for (pos, anim) in &game.enchanting_books {
        let time = anim.time as f32 + partial_tick;
        let corner = glam::DVec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
        let bob = f64::from(0.1 + (time * 0.1).sin() * 0.01);
        let pose = Mat4::from_rotation_y(-anim.rot_at(partial_tick))
            * Mat4::from_rotation_z(80.0_f32.to_radians());
        let [right, left] = anim.page_flips(partial_tick);
        out.push(EntityRenderInfo {
            age_in_ticks: time,
            attack_time: anim.prev_open + (anim.open - anim.prev_open) * partial_tick,
            flap: right,
            flap_speed: left,
            ..book(corner + glam::DVec3::new(0.5, 0.75 + bob, 0.5), pose, pos)
        });
    }
    let eye = game.player.position;
    for (pos, be) in &game.chunk_store.block_entities {
        if be.kind != BlockEntityKind::Lectern {
            continue;
        }
        let corner = glam::DVec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
        if corner.distance_squared(*eye) > 64.0 * 64.0 {
            continue;
        }
        let state = game.chunk_store.get_block_state(pos.x, pos.y, pos.z);
        let data = game.block_registry.block_state_data(state);
        let props = data.properties();
        if data.id() != "lectern" || props.get("has_book") != Some("true") {
            continue;
        }
        // Turned to the facing's clockwise side, tipped onto the slope.
        let degrees = match props.get("facing") {
            Some("south") => 90.0,
            Some("west") => 180.0,
            Some("east") => 0.0,
            _ => 270.0_f32,
        };
        let pose = Mat4::from_rotation_y((-degrees).to_radians())
            * Mat4::from_rotation_z(67.5_f32.to_radians())
            * Mat4::from_translation(Vec3::new(0.0, -0.125, 0.0));
        out.push(EntityRenderInfo {
            attack_time: 1.2,
            flap: 0.1,
            flap_speed: 0.9,
            ..book(corner + glam::DVec3::new(0.5, 1.0625, 0.5), pose, pos)
        });
    }
}

/// How far up the last section of a beam is drawn. Vanilla draws 2048
/// blocks of it; past a few hundred nothing of it is seen.
const BEAM_TOP: u32 = 512;
/// Vanilla's height for the last section, which sets where its texture is.
const VANILLA_BEAM_TOP: f32 = 2048.0;

/// Works out each loaded beacon's beam again once a second (vanilla scans
/// ten blocks a tick, and checks the base every four seconds).
fn update_beacon_beams(game: &mut GameState) {
    use azalea_registry::builtin::BlockEntityKind;

    use crate::world::beacon;
    let now = game.tick_count;
    let top = game.chunk_store.min_y() + game.chunk_store.section_count() * 16;
    let block = |x: i32, y: i32, z: i32| {
        game.block_registry
            .block_state_data(game.chunk_store.get_block_state(x, y, z))
    };
    let mut seen = Vec::new();
    for (pos, be) in &game.chunk_store.block_entities {
        if be.kind != BlockEntityKind::Beacon || block(pos.x, pos.y, pos.z).id() != "beacon" {
            continue;
        }
        seen.push(*pos);
        // Spread the beacons' scans over the second.
        let due = game
            .beacon_beams
            .get(pos)
            .is_none_or(|(at, _)| now.saturating_sub(*at) >= 20);
        if due {
            // `updateBase`: a full layer of base blocks under it.
            let based = (-1..=1).all(|dx| {
                (-1..=1)
                    .all(|dz| beacon::is_base_block(block(pos.x + dx, pos.y - 1, pos.z + dz).id()))
            });
            let sections = if based {
                beacon::beam_sections((pos.y..top).map(|y| {
                    let data = block(pos.x, y, pos.z);
                    let opaque = game
                        .block_registry
                        .render_view()
                        .is_opaque_full_cube(game.chunk_store.get_block_state(pos.x, y, pos.z));
                    beacon::beam_block(data.id(), opaque)
                }))
            } else {
                Vec::new()
            };
            game.beacon_beams.insert(*pos, (now, sections));
        }
    }
    game.beacon_beams.retain(|pos, _| seen.contains(pos));
}

/// The beacons' beams as last worked out ([`update_beacon_beams`]).
fn build_beacon_beams(game: &GameState, partial_tick: f32, out: &mut Vec<EntityRenderInfo>) {
    use glam::Mat4;

    use crate::renderer::entity_model::beam_pieces;
    use crate::renderer::pipelines::entity_renderer::{BEAM_KIND, unliving_model_frame};

    // `renderBeaconBeam`: the beam turns 2.25 degrees a tick, and the
    // texture runs up a fifth of a repeat a tick.
    let time = (game.sky_state.game_time.rem_euclid(40)) as f32 + partial_tick;
    let run = (-time * 0.2 - (-time * 0.1).floor()).rem_euclid(1.0);
    let unliving = unliving_model_frame();
    let turn = Mat4::from_rotation_y((time * 2.25 - 45.0).to_radians());
    for (pos, (_, sections)) in &game.beacon_beams {
        let mut foot = 0;
        for (i, section) in sections.iter().enumerate() {
            let last = i + 1 == sections.len();
            let (height, vanilla_height) = if last {
                (BEAM_TOP, VANILLA_BEAM_TOP)
            } else {
                (section.height, section.height as f32)
            };
            let [r, g, b] =
                [16, 8, 0].map(|shift| ((section.color >> shift) & 0xFF) as f32 / 255.0);
            // The texture's place at the section's foot, as vanilla has it.
            let slide = (vanilla_height * 2.5 + run).rem_euclid(1.0);
            for (offset, length) in beam_pieces(height) {
                let position = Position::new(
                    f64::from(pos.x) + 0.5,
                    f64::from(pos.y) + f64::from(foot + offset),
                    f64::from(pos.z) + 0.5,
                );
                // The beam, then the glow round it at an eighth.
                for (glow, tint, pose) in [
                    (0, [r, g, b, 1.0], turn * unliving),
                    (1, [r, g, b, 32.0 / 255.0], unliving),
                ] {
                    out.push(EntityRenderInfo {
                        position,
                        entity_kind: BEAM_KIND,
                        head_y_rot_deg: 180.0,
                        body_y_rot_deg: 180.0,
                        variant_index: (length * 2 + glow) as u32,
                        base_tint: tint,
                        body_transform: Some(pose),
                        flap: slide,
                        skip_cull: true,
                        light: LightLevels::FULL_BRIGHT,
                        ..Default::default()
                    });
                }
            }
            foot += height;
        }
    }
}

/// The rails of the loaded world, for a minecart's place on them.
fn rails_in<'a>(
    chunks: &'a crate::world::chunk::ChunkStore,
    table: crate::world::block::BlockStateTable,
) -> impl Fn([i32; 3]) -> Option<crate::entity::model_entity::Exits> + 'a {
    move |[x, y, z]| {
        let data = table.block_state_data(chunks.get_block_state(x, y, z));
        if !data.id().ends_with("rail") {
            return None;
        }
        crate::entity::model_entity::rail_exits(data.properties().get("shape")?)
    }
}

/// `AbstractMinecartRenderer.oldRender`'s pose before the cart and what it
/// carries, from the cart's feet: on rails the cart sits on them and turns
/// and tips to the line between the points 0.3 ahead of and behind it;
/// off them it keeps its own facing. Then up 0.375 and turned.
fn minecart_pose(
    cart: &crate::entity::model_entity::ModelEntity,
    feet: Position,
    partial_tick: f32,
    rails: crate::entity::model_entity::Rails,
) -> glam::Mat4 {
    use glam::{Mat4, Vec3};

    use crate::entity::model_entity::{pos_on_rail, pos_on_rail_offset};
    let at = *feet;
    let mut y_rot = lerp_angle(cart.prev_y_rot, cart.y_rot, partial_tick);
    let mut x_rot = cart.prev_x_rot + (cart.x_rot - cart.prev_x_rot) * partial_tick;
    let mut offset = glam::DVec3::ZERO;
    if let Some(on) = pos_on_rail(rails, at) {
        let front = pos_on_rail_offset(rails, at, 0.3).unwrap_or(on);
        let back = pos_on_rail_offset(rails, at, -0.3).unwrap_or(on);
        offset = glam::dvec3(on.x - at.x, (front.y + back.y) / 2.0 - at.y, on.z - at.z);
        if let Some(along) = (back - front).try_normalize() {
            y_rot = along.z.atan2(along.x).to_degrees() as f32;
            x_rot = (along.y.atan() * 73.0) as f32;
        }
    }
    Mat4::from_translation(offset.as_vec3() + Vec3::new(0.0, 0.375, 0.0))
        * Mat4::from_rotation_y((180.0 - y_rot).to_radians())
        * Mat4::from_rotation_z((-x_rot).to_radians())
}

/// What a minecart carries (`AbstractMinecartRenderer.submitMinecartContents`):
/// its block at three quarters of its size, riding in the cart.
fn build_minecart_contents(
    carts: &crate::entity::model_entity::ModelEntityStore,
    chunks: &crate::world::chunk::ChunkStore,
    world_light: &WorldLight,
    renderer: &mut Renderer,
    table: crate::world::block::BlockStateTable,
    partial_tick: f32,
    out: &mut Vec<crate::renderer::pipelines::item_entity::ItemRenderInfo>,
) {
    use glam::{Mat4, Vec3};

    use crate::renderer::pipelines::item_entity::ItemRenderInfo;
    let anchor = renderer.camera_anchor();
    let rails = rails_in(chunks, table);
    for cart in carts.entities.values() {
        let Some(Some(contents)) = crate::entity::model_entity::minecart(cart.kind) else {
            continue;
        };
        // A chest is a block entity's model, not a block's: the chest
        // item's mesh is that model.
        let (item_name, tints): (Arc<str>, Arc<[u32]>) = if contents.block == "chest" {
            renderer.ensure_item_mesh("chest");
            (Arc::from("chest"), Arc::from([]))
        } else {
            let Some(state) = table.state_with(contents.block, contents.props) else {
                continue;
            };
            let Some(mesh) = renderer.ensure_block_display_mesh(table, state.id() as u32) else {
                continue;
            };
            mesh
        };
        let feet = cart
            .prev_position
            .lerp(cart.position, f64::from(partial_tick));
        // The baked meshes are centred on the block; vanilla draws it from
        // its corner.
        let block = Mat4::from_scale(Vec3::splat(0.75))
            * Mat4::from_translation(Vec3::new(-0.5, (contents.offset - 8) as f32 / 16.0, 0.5))
            * Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2)
            * Mat4::from_translation(Vec3::splat(0.5));
        out.push(ItemRenderInfo {
            item_name,
            outline_color: None,
            tints,
            model_matrix: Mat4::from_translation((*feet - anchor).as_vec3())
                * minecart_pose(cart, feet, partial_tick, &rails)
                * block,
            light: entity_light(world_light, cart.kind, feet, false, 0),
            white_flash: false,
        });
    }
}

/// Vanilla `FallingBlockRenderer`: the block's model, its corner at the
/// entity's feet less half a block, lit where the entity is.
fn build_falling_blocks(
    blocks: &crate::entity::falling_block::FallingBlockStore,
    world_light: &WorldLight,
    renderer: &mut Renderer,
    table: crate::world::block::BlockStateTable,
    partial_tick: f32,
    out: &mut Vec<crate::renderer::pipelines::item_entity::ItemRenderInfo>,
) {
    use crate::renderer::pipelines::item_entity::ItemRenderInfo;
    let anchor = renderer.camera_anchor();
    for block in blocks.blocks.values() {
        let feet = block
            .prev_position
            .lerp(block.position, f64::from(partial_tick));
        let Some((item_name, tints)) = renderer.ensure_block_display_mesh(table, block.state)
        else {
            continue;
        };
        // The baked meshes are centred on the block.
        let centre = *feet + glam::DVec3::Y * 0.5;
        // `TntRenderer`: primed TNT swells over its last 10 ticks, and
        // flashes white for five ticks in every ten (`isLit`).
        let left = block.fuse.map(|fuse| fuse as f32 - partial_tick + 1.0);
        let swell = left.map_or(1.0, |left| {
            let g = (1.0 - left / 10.0).clamp(0.0, 1.0).powi(4);
            1.0 + g * 0.3
        });
        let white_flash = left.is_some_and(|left| left >= 0.0 && (left / 5.0) as i32 % 2 == 0);
        out.push(ItemRenderInfo {
            item_name,
            outline_color: None,
            tints,
            model_matrix: glam::Mat4::from_translation((centre - anchor).as_vec3())
                * glam::Mat4::from_scale(glam::Vec3::splat(swell)),
            light: entity_light(
                world_light,
                EntityKind::FallingBlock,
                Position::new(centre.x, centre.y, centre.z),
                false,
                0,
            ),
            white_flash,
        });
    }
}

/// Vanilla `ItemInHandLayer`: what a mob or another player holds, in the
/// hand of its arm part: the arm's pose, turned and pushed out to the fist
/// (`renderArmWithItem`), then the item's third-person display transform.
// TODO: left-handed mobs and players, the illagers' crossed-arm hiding, the
// witch, fox, panda and dolphin's own placements, and the local player in
// third person.
fn build_held_items(
    game: &GameState,
    renderer: &mut Renderer,
    entities: &[EntityRenderInfo],
    out: &mut Vec<crate::renderer::pipelines::item_entity::ItemRenderInfo>,
    selected_slot: usize,
) {
    use azalea_inventory::components::EquipmentSlot;

    use crate::renderer::pipelines::item_entity::ItemRenderInfo;
    let third_person = !renderer.is_first_person();
    for info in entities {
        // The local player is the one player with no entity id; its hands
        // are its selected hotbar slot and offhand, drawn on its model only
        // in third person (first person draws the hand itself).
        let hands: [Option<&azalea_inventory::ItemStackData>; 2] = match info
            .entity_id
            .and_then(|id| game.entity_store.living.get(&id))
        {
            Some(entity) if holds_items(entity.entity_type) => {
                [EquipmentSlot::Mainhand, EquipmentSlot::Offhand]
                    .map(|slot| entity.equipment(slot).and_then(|stack| stack.as_present()))
            }
            None if info.entity_id.is_none()
                && info.entity_kind == EntityKind::Player
                && third_person =>
            {
                let inventory = &game.player.inventory;
                [
                    inventory
                        .hotbar_slots()
                        .get(selected_slot)
                        .and_then(|stack| stack.as_present()),
                    inventory.offhand().as_present(),
                ]
            }
            _ => continue,
        };
        // The hand in use (0 main, 1 off): the local player's own use, or
        // what the server says another entity is using.
        let using = match info.entity_id {
            Some(id) => game.entity_store.living.get(&id).and_then(|e| e.using_hand),
            None => game
                .interaction
                .using_hand()
                .map(|hand| u8::from(hand != crate::player::interaction::HANDS[0])),
        };
        let angler = info.entity_id.unwrap_or(game.player.entity_id);
        let cast = cast_rod_hand(game.fishing_hooks.has_hook(angler), hands[0], hands[1]);
        for ((stack, right), hand) in hands.into_iter().zip([true, false]).zip([0u8, 1]) {
            let Some(stack) = stack else {
                continue;
            };
            let Some(arm) = renderer.entity_arm_pose(info, right) else {
                continue;
            };
            let name = crate::player::inventory::item_resource_name(stack.kind);
            let model = renderer.render_assets().stack_model_key(&name, stack);
            let model = if using == Some(hand) {
                renderer.render_assets().in_use_model_key(&model)
            } else if cast == Some(hand) {
                renderer.render_assets().cast_model_key(&model)
            } else {
                model
            };
            // THIRD_PERSON_RIGHT_HAND / THIRD_PERSON_LEFT_HAND's own pose.
            let model = renderer
                .render_assets()
                .context_model_key(&model, if right { 2 } else { 1 });
            renderer.ensure_item_mesh(&model);
            let tints: Arc<[u32]> = renderer
                .render_assets()
                .item_tint_palette(&model, Some(stack), None)
                .into();
            // THIRD_PERSON_RIGHT_HAND / THIRD_PERSON_LEFT_HAND.
            let transform = renderer.item_display_transform(&model, if right { 2 } else { 1 });
            let side = if right { 1.0 } else { -1.0 };
            let model_matrix = arm
                * ARM_FRAME
                * glam::Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2)
                * glam::Mat4::from_rotation_y(std::f32::consts::PI)
                * glam::Mat4::from_translation(glam::Vec3::new(side / 16.0, 0.125, -0.625))
                * spear_item_matrix(info, hands[0], stack, right)
                * transform;
            out.push(ItemRenderInfo {
                item_name: model.into(),
                outline_color: info.outline_color,
                tints,
                model_matrix,
                light: info.light,
                white_flash: false,
            });
        }
        // Vanilla `CustomHeadLayer`: a worn item that isn't armour (with an
        // equipment asset) or a skull (drawn as a head above) sits on the
        // head in its `head` display pose, as a training dummy's target
        // block or a carved pumpkin does.
        let Some(entity) = info
            .entity_id
            .and_then(|id| game.entity_store.living.get(&id))
        else {
            continue;
        };
        let Some(helmet) = entity.equipment(EquipmentSlot::Head) else {
            continue;
        };
        let Some(stack) = helmet.as_present() else {
            continue;
        };
        let name = crate::player::inventory::item_resource_name(stack.kind);
        if crate::entity::heads::skull_model(&name).is_some()
            || crate::renderer::pipelines::entity_renderer::equipment_render_info(helmet).is_some()
        {
            continue;
        }
        let Some(head) = renderer.entity_part_pose(info, "head") else {
            continue;
        };
        let model = renderer.render_assets().stack_model_key(&name, stack);
        // HEAD's own pose.
        let model = renderer.render_assets().context_model_key(&model, 5);
        renderer.ensure_item_mesh(&model);
        let tints: Arc<[u32]> = renderer
            .render_assets()
            .item_tint_palette(&model, Some(stack), None)
            .into();
        // HEAD.
        let transform = renderer.item_display_transform(&model, 5);
        out.push(ItemRenderInfo {
            item_name: model.into(),
            outline_color: info.outline_color,
            tints,
            model_matrix: head
                * ARM_FRAME
                * glam::Mat4::from_translation(glam::Vec3::new(0.0, -0.25, 0.0))
                * glam::Mat4::from_rotation_y(std::f32::consts::PI)
                * glam::Mat4::from_scale(glam::Vec3::new(0.625, -0.625, -0.625))
                * transform,
            light: info.light,
            white_flash: false,
        });
    }
}

/// Vanilla `ThrownItemRenderer`: each projectile's item in its ground pose,
/// scaled and turned to face the camera; not for its first two ticks while
/// it's still at the thrower's eye.
fn build_thrown_items(
    game: &GameState,
    renderer: &mut Renderer,
    world_light: &crate::renderer::chunk::block_lighter::WorldLight<'_>,
    partial_tick: f32,
    out: &mut Vec<crate::renderer::pipelines::item_entity::ItemRenderInfo>,
) {
    use crate::entity::thrown_item::ThrownItem;
    use crate::renderer::pipelines::item_entity::ItemRenderInfo;
    let anchor = renderer.camera_anchor();
    let eye = renderer.camera_render_position();
    let (right, up) = renderer.billboard_axes();
    let facing = glam::Mat4::from_cols(
        right.extend(0.0),
        up.extend(0.0),
        right.cross(up).extend(0.0),
        glam::Vec4::W,
    );
    for item in game.thrown_items.items.values() {
        if crate::entity::thrown_item::is_wind_charge(item.kind) {
            continue;
        }
        let position = item
            .prev_position
            .lerp(item.position, f64::from(partial_tick));
        if item.age < 2 && (*position - eye).length_squared() < 12.25 {
            continue;
        }
        let model = item
            .model
            .clone()
            .unwrap_or_else(|| ThrownItem::default_item(item.kind).to_owned());
        renderer.ensure_item_mesh(&model);
        let ground = renderer
            .render_assets()
            .get_item_ground_transform(&model)
            .unwrap_or_else(|| glam::Mat4::from_scale(glam::Vec3::splat(0.5)));
        let tints: Arc<[u32]> = renderer
            .render_assets()
            .item_tint_palette(&model, item.stack.as_ref(), None)
            .into();
        out.push(ItemRenderInfo {
            item_name: model.into(),
            outline_color: None,
            tints,
            model_matrix: glam::Mat4::from_translation((*position - anchor).as_vec3())
                * glam::Mat4::from_scale(glam::Vec3::splat(item.scale()))
                * facing
                * ground,
            light: entity_light(world_light, item.kind, position, false, 0),
            white_flash: false,
        });
    }
}

/// Vanilla `ItemInHandLayer.submitArmWithItem`'s spear turns for the item
/// `stack` in the `right` (or left) hand: thrust through a stab
/// (`thirdPersonAttackItem`, the main hand's weapon) and levelled through a
/// charge (`ArmPose.SPEAR.animateUseItem`).
fn spear_item_matrix(
    info: &EntityRenderInfo,
    main: Option<&azalea_inventory::ItemStackData>,
    stack: &azalea_inventory::ItemStackData,
    right: bool,
) -> glam::Mat4 {
    use crate::renderer::entity_model::spear;
    let mut m = glam::Mat4::IDENTITY;
    if right && info.attack_stabs && info.attack_time > 0.0 {
        let forward = main
            .and_then(crate::player::interaction::kinetic_weapon)
            .map_or(0.0, |weapon| weapon.forward_movement);
        m *= spear::attack_item_matrix(info.attack_time, forward);
    }
    if info.spear.arms[usize::from(!right)]
        && let Some((use_right, ticks, _)) = info.spear.using
        && use_right == right
        && let Some(weapon) = crate::player::interaction::kinetic_weapon(stack)
    {
        m *= spear::use_item_matrix(
            &spear::KineticTimings::from(&weapon),
            ticks,
            info.attack_time,
            right,
        );
    }
    m
}

/// From a model part's frame as pomme poses it to vanilla's (y down), which
/// `ItemInHandLayer`'s offsets are in.
const ARM_FRAME: glam::Mat4 = glam::Mat4::from_cols(
    glam::Vec4::new(1.0, 0.0, 0.0, 0.0),
    glam::Vec4::new(0.0, -1.0, 0.0, 0.0),
    glam::Vec4::new(0.0, 0.0, 1.0, 0.0),
    glam::Vec4::W,
);

/// How one of [`crate::entity::model_entity`]'s kinds is drawn: its
/// renderer's pose before the model, over pomme's living-model frame.
fn model_entity_render(
    entity: &crate::entity::model_entity::ModelEntity,
    feet: Position,
    partial_tick: f32,
    light: crate::renderer::chunk::block_lighter::LightLevels,
    rails: crate::entity::model_entity::Rails,
) -> Option<EntityRenderInfo> {
    use glam::{Mat4, Vec3};
    let unliving = crate::renderer::pipelines::entity_renderer::unliving_model_frame();
    let age = entity.age as f32 + partial_tick;
    let yaw = lerp_angle(entity.prev_y_rot, entity.y_rot, partial_tick).to_radians();
    let pitch =
        (entity.prev_x_rot + (entity.x_rot - entity.prev_x_rot) * partial_tick).to_radians();
    let flipped = Mat4::from_scale(Vec3::new(-1.0, -1.0, 1.0));
    let lift = Mat4::from_translation(Vec3::new(0.0, 0.15, 0.0));
    // The model's own turn to face its flight (`yRot`, then `xRot`).
    let facing = Mat4::from_rotation_y(yaw) * Mat4::from_rotation_x(pitch);
    let mut info = EntityRenderInfo {
        position: feet,
        entity_kind: entity.kind,
        head_y_rot_deg: 180.0,
        body_y_rot_deg: 180.0,
        age_in_ticks: age,
        light,
        ..Default::default()
    };
    match entity.kind {
        // `ShulkerBulletRenderer`: tumbling at half size, inside a faint
        // shell half as big again.
        EntityKind::ShulkerBullet => {
            let tumble = Mat4::from_rotation_y(((age * 0.1).sin() * 180.0).to_radians())
                * Mat4::from_rotation_x(((age * 0.1).cos() * 180.0).to_radians())
                * Mat4::from_rotation_z(((age * 0.15).sin() * 360.0).to_radians());
            let half = Mat4::from_scale(Vec3::new(-0.5, -0.5, 0.5));
            info.body_transform = Some(lift * tumble * half * facing * unliving);
            info.overlay_tints[0] = Some([1.0, 1.0, 1.0, 0.15]);
        }
        // `WitherSkullRenderer`: variant 1 is the blue skull.
        EntityKind::WitherSkull => {
            info.body_transform = Some(flipped * facing * unliving);
            info.variant_index = u32::from(entity.dangerous);
        }
        // `LlamaSpitRenderer`.
        EntityKind::LlamaSpit => {
            info.body_transform = Some(
                lift * Mat4::from_rotation_y(yaw - std::f32::consts::FRAC_PI_2)
                    * Mat4::from_rotation_z(pitch)
                    * unliving,
            );
        }
        // `LeashKnotRenderer`.
        EntityKind::LeashKnot => info.body_transform = Some(flipped * unliving),
        // `EvokerFangsRenderer`: nothing until the bite starts; then turned
        // a quarter from its facing, with the model root's rise and final
        // shrink (`EvokerFangsModel.setupAnim`).
        EntityKind::EvokerFangs => {
            let bite = entity.bite_progress(partial_tick);
            if bite == 0.0 {
                return None;
            }
            let size = if bite > 0.9 { (1.0 - bite) / 0.1 } else { 1.0 };
            let root = Mat4::from_translation(Vec3::new(0.0, (24.0 - 20.0 * size) / 16.0, 0.0))
                * Mat4::from_scale(Vec3::splat(size));
            info.body_y_rot_deg = entity.y_rot + 90.0;
            info.head_y_rot_deg = info.body_y_rot_deg;
            info.body_transform = Some(unliving.inverse() * root * unliving);
            info.attack_time = bite;
        }
        // `AbstractBoatRenderer`: up 0.375, turned to its facing, with the
        // model a quarter turn round; the paddles at their strokes.
        kind if crate::entity::model_entity::boat(kind).is_some() => {
            info.body_transform = Some(
                Mat4::from_translation(Vec3::new(0.0, 0.375, 0.0))
                    * Mat4::from_rotation_y(std::f32::consts::PI - yaw)
                    * flipped
                    * Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2)
                    * unliving,
            );
            [info.flap, info.flap_speed] = entity.rowing_time(partial_tick);
        }
        kind if crate::entity::model_entity::minecart(kind).is_some() => {
            info.body_transform =
                Some(minecart_pose(entity, feet, partial_tick, rails) * flipped * unliving);
        }
        _ => return None,
    }
    Some(info)
}

/// The kinds `ItemInHandLayer` draws held items for (humanoid arms).
fn holds_items(kind: EntityKind) -> bool {
    matches!(
        kind,
        EntityKind::Player
            | EntityKind::Mannequin
            | EntityKind::Zombie
            | EntityKind::Husk
            | EntityKind::Drowned
            | EntityKind::Skeleton
            | EntityKind::Stray
            | EntityKind::Bogged
            | EntityKind::WitherSkeleton
            | EntityKind::Piglin
            | EntityKind::PiglinBrute
            | EntityKind::ZombifiedPiglin
            | EntityKind::Vindicator
            | EntityKind::Pillager
            | EntityKind::Evoker
            | EntityKind::Illusioner
            | EntityKind::Vex
            | EntityKind::ArmorStand
    )
}

/// Vanilla `EntityRenderDispatcher.renderFlame`: a burning entity wears a
/// stack of fire quads, 1.4 times its width, climbing its height 0.45 at a
/// time, each a little narrower and nearer the camera, alternating the two
/// fire frames (mirrored every other pair), at full brightness.
// TODO: vanilla's quads are 1.4 times taller than wide and turn only about
// y; particle quads are square and face the camera fully.
/// How a humanoid of `kind` holds a spear (`hands`: main, then off): the
/// arms its renderer's `getArmPose` puts in `ArmPose.SPEAR`, and the charge
/// the hand in `using` (0 main, 1 off) is `use_ticks` into.
fn spear_pose(
    kind: EntityKind,
    aggressive: bool,
    hands: [Option<&azalea_inventory::ItemStackData>; 2],
    using: Option<u8>,
    use_ticks: f32,
    swinging: bool,
) -> crate::renderer::entity_model::spear::SpearPose {
    use crate::renderer::entity_model::spear;
    let held = hands.map(|stack| {
        stack.map(|stack| {
            (
                spear::is_spear(stack.kind),
                crate::player::interaction::swing_animation(Some(stack)).0,
            )
        })
    });
    let using = using.and_then(|hand| {
        let weapon = crate::player::interaction::kinetic_weapon(hands[usize::from(hand)]?)?;
        Some((hand == 0, use_ticks, spear::KineticTimings::from(&weapon)))
    });
    let (arms, hanging) = spear::spear_arms(kind, aggressive, held, swinging);
    spear::SpearPose {
        arms,
        hanging,
        using,
    }
}

/// How a humanoid of `kind` holds a crossbow (`hands`: main, then off), as
/// its renderer's `getArmPose` has it: drawn back while it loads one (the
/// hand in `using`, `use_ticks` in, or a mob's own `charging` flag), and
/// levelled once the one in its main hand is loaded. Pillagers and piglins
/// level theirs loaded or not.
fn crossbow_pose(
    kind: EntityKind,
    hands: [Option<&azalea_inventory::ItemStackData>; 2],
    using: Option<u8>,
    use_ticks: f32,
    charging: bool,
) -> crate::renderer::entity_model::crossbow::CrossbowPose {
    use azalea_inventory::components::ChargedProjectiles;
    use azalea_registry::builtin::ItemKind;

    use crate::renderer::entity_model::crossbow::{CHARGE_TICKS, CrossbowPose};
    if !holds_items(kind) {
        return CrossbowPose::None;
    }
    fn crossbow(
        stack: Option<&azalea_inventory::ItemStackData>,
    ) -> Option<&azalea_inventory::ItemStackData> {
        stack.filter(|stack| stack.kind == ItemKind::Crossbow)
    }
    let loading = using.is_some_and(|hand| crossbow(hands[usize::from(hand)]).is_some());
    if loading || charging && crossbow(hands[0]).is_some() {
        return CrossbowPose::Charge(use_ticks / CHARGE_TICKS);
    }
    let Some(held) = crossbow(hands[0]) else {
        return CrossbowPose::None;
    };
    let loaded = held
        .get_component::<ChargedProjectiles>()
        .is_some_and(|charged| !charged.items.is_empty());
    let always = matches!(
        kind,
        EntityKind::Pillager | EntityKind::Piglin | EntityKind::PiglinBrute
    );
    if loaded || always {
        CrossbowPose::Hold
    } else {
        CrossbowPose::None
    }
}

/// Vanilla `FishingRodCast`: the hand (0 main, 1 off) whose rod draws cast
/// while its angler has a line out; a rod in the main hand wins.
fn cast_rod_hand(
    has_hook: bool,
    main: Option<&azalea_inventory::ItemStackData>,
    off: Option<&azalea_inventory::ItemStackData>,
) -> Option<u8> {
    use azalea_registry::builtin::ItemKind;
    let rod = |stack: Option<&azalea_inventory::ItemStackData>| {
        stack.is_some_and(|stack| stack.kind == ItemKind::FishingRod)
    };
    if !has_hook {
        None
    } else if rod(main) {
        Some(0)
    } else if rod(off) {
        Some(1)
    } else {
        None
    }
}

/// Vanilla `ExperienceOrbRenderer`: each orb is a camera-facing sprite from
/// its sheet of sixteen, 0.3 of a block across and lifted a little, tinted
/// by its pulse and half clear, lit seven levels brighter than its place.
fn add_experience_orbs(
    game: &GameState,
    renderer: &Renderer,
    partial_tick: f32,
    out: &mut Vec<crate::renderer::pipelines::particle::ParticleQuad>,
) {
    use crate::renderer::pipelines::particle::ParticleQuad;
    if game.experience_orbs.orbs.is_empty() {
        return;
    }
    let Some(sheet) = crate::entity::experience_orb::TEXTURES
        .iter()
        .find_map(|texture| game.particle_store.item_region(texture))
    else {
        return;
    };
    let anchor = renderer.camera_anchor();
    let world_light = WorldLight {
        chunks: &game.chunk_store,
        table: game.block_registry.block_state_table(),
    };
    let (width, height) = (sheet.u_max - sheet.u_min, sheet.v_max - sheet.v_min);
    for orb in game.experience_orbs.orbs.values() {
        let pos = orb
            .prev_position
            .lerp(orb.position, f64::from(partial_tick));
        let icon = orb.icon();
        let (column, row) = ((icon % 4) as f32, (icon / 4) as f32);
        let light = entity_light(
            &world_light,
            EntityKind::ExperienceOrb,
            Position::new(pos.x, pos.y, pos.z),
            false,
            0,
        );
        out.push(ParticleQuad {
            // The quad spans y -0.25 to 0.75 at 0.3 scale, 0.1 up.
            pos: (pos + glam::DVec3::new(0.0, 0.175, 0.0) - anchor)
                .as_vec3()
                .into(),
            size: 0.15,
            u0: sheet.u_min + width * column / 4.0,
            u1: sheet.u_min + width * (column + 1.0) / 4.0,
            v0: sheet.v_min + height * row / 4.0,
            v1: sheet.v_min + height * (row + 1.0) / 4.0,
            color: u32::from_le_bytes(orb.color(partial_tick)),
            light: LightLevels::new(light.sky(), (light.block() + 7).min(15)).lane(),
            translucent: true,
        });
    }
}

/// Vanilla `FishingHookRenderer`: each bobber, a camera-facing half-block
/// sprite at full size, and its line's segments to the angler's rod tip.
/// Vanilla `DragonFireballRenderer`: its sprite facing the viewer, two
/// blocks across and full bright, a quarter of that above its feet.
fn add_dragon_fireballs(
    game: &GameState,
    renderer: &Renderer,
    partial_tick: f32,
    out: &mut Vec<crate::renderer::pipelines::particle::ParticleQuad>,
) {
    use crate::renderer::chunk::block_lighter::LightLevels;
    use crate::renderer::pipelines::particle::ParticleQuad;
    let texture = crate::entity::model_entity::DRAGON_FIREBALL_TEXTURE;
    let Some(region) = game.particle_store.item_region(texture) else {
        return;
    };
    let anchor = renderer.camera_anchor();
    for fireball in game.model_entities.entities.values() {
        if fireball.kind != EntityKind::DragonFireball {
            continue;
        }
        let feet = *fireball
            .prev_position
            .lerp(fireball.position, f64::from(partial_tick));
        let centre = feet + glam::DVec3::new(0.0, 0.5, 0.0);
        out.push(ParticleQuad {
            pos: (centre - anchor).as_vec3().into(),
            size: 1.0,
            u0: region.u_min,
            u1: region.u_max,
            v0: region.v_min,
            v1: region.v_max,
            color: u32::MAX,
            light: LightLevels::FULL_BRIGHT.lane(),
            translucent: false,
        });
    }
}

/// The leads between mobs and what holds them (`EntityRenderer.renderLeash`),
/// as the pieces of each rope: its ends, and whether it is one of the
/// darker pieces the rope alternates.
// TODO: vanilla's rope is a lit ribbon a fortieth of a block wide; this is
// a line of constant width, unlit.
fn build_leashes(
    game: &GameState,
    renderer: &Renderer,
    partial_tick: f32,
) -> Vec<(glam::DVec3, glam::DVec3, bool)> {
    use crate::entity::dimensions::EntityDimensions;
    use crate::entity::leash;
    let mut ropes = Vec::new();
    let partial = f64::from(partial_tick);
    for (&id, &holder) in &game.leashes.holders {
        let Some(mob) = game.entity_store.living.get(&id) else {
            continue;
        };
        let feet = *mob.prev_position.lerp(mob.position, partial);
        let yaw = lerp_angle(mob.prev_body_y_rot_deg, mob.body_y_rot_deg, partial_tick);
        let start = feet
            + leash::leash_offset(
                EntityDimensions::eye_height(mob.entity_type),
                EntityDimensions::of(mob.entity_type).width,
                yaw,
            );
        let end = if holder == game.player.entity_id {
            if renderer.is_first_person() {
                let look = renderer.camera_look_dir();
                leash::first_person_hold(
                    renderer.camera_render_position(),
                    look.y_rot_deg(),
                    look.x_rot_deg(),
                )
            } else {
                leash::player_hold(
                    *game
                        .player
                        .prev_position
                        .lerp(game.player.position, partial),
                    game.player.height(),
                    lerp_angle(
                        game.player_prev_body_y_rot,
                        game.player_body_y_rot,
                        partial_tick,
                    ),
                    game.player.crouching,
                )
            }
        } else if let Some(holder) = game.entity_store.living.get(&holder) {
            let feet = *holder.prev_position.lerp(holder.position, partial);
            let yaw = lerp_angle(
                holder.prev_body_y_rot_deg,
                holder.body_y_rot_deg,
                partial_tick,
            );
            if holder.entity_type == EntityKind::Player {
                let height = f64::from(EntityDimensions::of(EntityKind::Player).height);
                leash::player_hold(feet, height, yaw, false)
            } else {
                // `Entity.getRopeHoldPosition`.
                let eye = f64::from(EntityDimensions::eye_height(holder.entity_type));
                feet + glam::DVec3::new(0.0, eye * 0.7, 0.0)
            }
        } else if let Some(knot) = game.model_entities.entities.get(&holder) {
            // `LeashFenceKnotEntity.getRopeHoldPosition`.
            *knot.position + glam::DVec3::new(0.0, 0.2, 0.0)
        } else {
            continue;
        };
        let points = leash::rope_points(start, end);
        for (i, pair) in points.windows(2).enumerate() {
            ropes.push((pair[0], pair[1], i % 2 == 1));
        }
    }
    ropes
}

fn add_fishing_hooks(
    game: &GameState,
    core: &AppCore,
    renderer: &Renderer,
    partial_tick: f32,
    out: &mut Vec<crate::renderer::pipelines::particle::ParticleQuad>,
) -> Vec<(glam::DVec3, glam::DVec3)> {
    use crate::renderer::pipelines::particle::ParticleQuad;
    let mut lines = Vec::new();
    if game.fishing_hooks.hooks.is_empty() {
        return lines;
    }
    let region = crate::world::block::render_assets::FISHING_HOOK_TEXTURES
        .iter()
        .find_map(|texture| game.particle_store.item_region(texture));
    let anchor = renderer.camera_anchor();
    let world_light = WorldLight {
        chunks: &game.chunk_store,
        table: game.block_registry.block_state_table(),
    };
    for hook in game.fishing_hooks.hooks.values() {
        let pos = hook.lerped(partial_tick);
        if let Some(region) = region {
            out.push(ParticleQuad {
                pos: (pos - anchor).as_vec3().into(),
                size: 0.25,
                u0: region.u_min,
                u1: region.u_max,
                v0: region.v_min,
                v1: region.v_max,
                color: u32::MAX,
                light: entity_light(
                    &world_light,
                    EntityKind::FishingBobber,
                    pos.into(),
                    false,
                    0,
                )
                .lane(),
                translucent: false,
            });
        }
        if let Some(tip) = rod_tip(game, core, renderer, hook.owner, partial_tick) {
            lines.extend(crate::entity::fishing_hook::line_segments(pos, tip));
        }
    }
    lines
}

/// Vanilla `FishingHookRenderer.getPlayerHandPos`: the angler's rod tip. In
/// first person it sits on the near plane beside the view (scaled by 960 /
/// fov); otherwise in front of the body, beside the eyes, on the side of
/// the hand holding the rod.
fn rod_tip(
    game: &GameState,
    core: &AppCore,
    renderer: &Renderer,
    owner: i32,
    partial_tick: f32,
) -> Option<glam::DVec3> {
    use azalea_inventory::components::EquipmentSlot;
    use azalea_registry::builtin::ItemKind;
    let main_arm_right = core.menu.main_hand_right();
    if owner == game.player.entity_id {
        let main_rod = game.player.inventory.hotbar_slots()[core.input.selected_slot() as usize]
            .as_present()
            .is_some_and(|stack| stack.kind == ItemKind::FishingRod);
        let invert = if main_rod == main_arm_right {
            1.0
        } else {
            -1.0
        };
        let feet = game
            .player
            .prev_position
            .lerp(game.player.position, f64::from(partial_tick));
        let eye = *feet + glam::DVec3::new(0.0, f64::from(game.player.target_eye_height()), 0.0);
        if renderer.is_first_person() && game.camera_entity.is_none() {
            let look = game.player.look_dir;
            let (yaw, pitch) = (
                f64::from(look.y_rot_deg()).to_radians(),
                f64::from(look.x_rot_deg()).to_radians(),
            );
            let forward = glam::DVec3::new(
                -yaw.sin() * pitch.cos(),
                -pitch.sin(),
                yaw.cos() * pitch.cos(),
            );
            let left = glam::DVec3::new(yaw.cos(), 0.0, yaw.sin());
            let up = forward.cross(left);
            let fov = f64::from(core.menu.fov);
            let aspect =
                f64::from(renderer.screen_width()) / f64::from(renderer.screen_height().max(1));
            let plane_h = (fov.to_radians() / 2.0).tan() * 0.05;
            let plane_w = plane_h * aspect;
            let on_plane =
                forward * 0.05 + up * (-0.1 * plane_h) - left * (invert * 0.525 * plane_w);
            return Some(eye + on_plane * (960.0 / fov));
        }
        let body = lerp_angle(
            game.player_prev_body_y_rot,
            game.player_body_y_rot,
            partial_tick,
        );
        let crouch = if game.player.crouching { -0.1875 } else { 0.0 };
        return Some(hand_offset(eye, body, invert, 1.0, crouch));
    }
    let e = game.entity_store.living.get(&owner)?;
    let main_rod = e
        .equipment(EquipmentSlot::Mainhand)
        .and_then(|stack| stack.as_present())
        .is_some_and(|stack| stack.kind == ItemKind::FishingRod);
    let invert = if main_rod { 1.0 } else { -1.0 };
    let body = lerp_angle(e.prev_body_y_rot_deg, e.body_y_rot_deg, partial_tick);
    let scale = f64::from(crate::entity::entity_scale(e));
    let crouch = if e.is_crouching { -0.1875 } else { 0.0 };
    Some(hand_offset(
        *e.eye_position(partial_tick),
        body,
        invert,
        scale,
        crouch,
    ))
}

/// The third-person half of [`rod_tip`]: 0.35 to the rod's side, 0.8 ahead
/// and 0.45 below the eyes, turned with the body.
fn hand_offset(
    eye: glam::DVec3,
    body_deg: f32,
    invert: f64,
    scale: f64,
    crouch: f64,
) -> glam::DVec3 {
    let (sin, cos) = f64::from(body_deg).to_radians().sin_cos();
    let right = invert * 0.35 * scale;
    let ahead = 0.8 * scale;
    eye + glam::DVec3::new(
        -cos * right - sin * ahead,
        crouch - 0.45 * scale,
        -sin * right + cos * ahead,
    )
}

fn add_entity_flames(
    game: &GameState,
    renderer: &Renderer,
    partial_tick: f32,
    out: &mut Vec<crate::renderer::pipelines::particle::ParticleQuad>,
) {
    use crate::renderer::pipelines::particle::ParticleQuad;
    let frames = [
        game.particle_store.item_region("fire_0"),
        game.particle_store.item_region("fire_1"),
    ];
    let [Some(fire_0), Some(fire_1)] = frames else {
        return;
    };
    let camera = renderer.camera_render_position();
    let anchor = renderer.camera_anchor();
    let full_bright = LightLevels::new(15, 15).lane();
    // Vanilla `Entity.displayFireAnimation`: on fire and not a spectator,
    // invisible or not; the local player too while the camera shows it.
    let first_person = renderer.is_first_person();
    let living = game
        .entity_store
        .living
        .iter()
        .filter(|&(&id, e)| {
            e.is_on_fire
                && !fire_immune(e.entity_type)
                && !(first_person && game.camera_entity == Some(id))
        })
        .map(|(_, e)| {
            let dims = crate::entity::living_entity_dimensions(e);
            (
                *e.prev_position.lerp(e.position, partial_tick as f64),
                dims.width,
                dims.height,
            )
        });
    let local = (game.player.on_fire
        && !first_person
        && game.camera_entity.is_none()
        && !crate::player::is_spectator(game.player.game_mode))
    .then(|| {
        (
            *game
                .player
                .prev_position
                .lerp(game.player.position, partial_tick as f64),
            // `EntityType.PLAYER`'s 0.6 width.
            0.6,
            game.player.height() as f32,
        )
    });
    for (feet, width, height) in living.chain(local) {
        let scale = width * 1.4;
        let mut toward = camera - feet;
        toward.y = 0.0;
        let toward = toward.try_normalize().unwrap_or(glam::DVec3::Z);
        let mut left = height / scale;
        let (mut bottom, mut half_width, mut depth) = (0.0f32, 0.5f32, 0.3 - left.floor() * 0.02);
        let mut i = 0;
        while left > 0.0 {
            let frame = if i % 2 == 0 { fire_0 } else { fire_1 };
            let (u0, u1) = if i / 2 % 2 == 0 {
                (frame.u_max, frame.u_min)
            } else {
                (frame.u_min, frame.u_max)
            };
            let centre = feet
                + glam::DVec3::Y * f64::from((bottom + 0.7) * scale)
                + toward * f64::from(depth * scale);
            out.push(ParticleQuad {
                pos: (centre - anchor).as_vec3().into(),
                size: 0.7 * scale * (half_width / 0.5).sqrt(),
                u0,
                u1,
                v0: frame.v_min,
                v1: frame.v_max,
                color: u32::MAX,
                light: full_bright,
                translucent: false,
            });
            left -= 0.45;
            bottom += 0.45;
            half_width *= 0.9;
            depth -= 0.03;
            i += 1;
        }
    }
}

/// Vanilla `EntityType.fireImmune()` among the mobs pomme draws: they never
/// show flames (`Entity.isOnFire` is false for them).
fn fire_immune(kind: EntityKind) -> bool {
    matches!(
        kind,
        EntityKind::Blaze
            | EntityKind::MagmaCube
            | EntityKind::WitherSkeleton
            | EntityKind::Ghast
            | EntityKind::Strider
            | EntityKind::Zoglin
            | EntityKind::ZombifiedPiglin
            | EntityKind::Wither
            | EntityKind::EnderDragon
            | EntityKind::Warden
    )
}

/// What [`build_name_tags`] reads.
struct NameTagSources<'a> {
    names: &'a mut crate::entity::name_tags::NameStore,
    living: &'a crate::entity::EntityStore,
    /// Items and falling blocks move between the server's updates, which
    /// come only once a second, so their names follow the simulation.
    items: &'a crate::entity::ItemEntityStore,
    falling_blocks: &'a crate::entity::falling_block::FallingBlockStore,
    positions: &'a HashMap<i32, crate::entity::components::Position>,
    tab_list: &'a crate::player::tab_list::TabList,
    scoreboard: &'a crate::ui::hud::Scoreboard,
    local_uuid: uuid::Uuid,
    local_entity: i32,
}

/// Vanilla `EntityRenderer.submitNameTag` for every name that shows: other
/// players' (team-formatted) and entities' visible custom names (hologram
/// armor stands, named NPCs), 0.5 above the entity's box, facing the
/// camera, within 64 blocks (32 for a sneaking player). A name draws twice:
/// faded (0x80) over a quarter-black background through walls, then fully
/// in the world; a sneaking player's only once, faded, on its background,
/// without the see-through pass.
// TODO: teams' nameTagVisibility, and "deadmau5"'s raised tag.
fn build_name_tags(
    sources: NameTagSources<'_>,
    world_light: &WorldLight,
    renderer: &mut Renderer,
    partial_tick: f32,
    out: &mut Vec<(
        f64,
        crate::renderer::pipelines::menu_overlay::WorldTextRender,
    )>,
) {
    use crate::entity::display::TextAlign;
    use crate::renderer::pipelines::menu_overlay::WorldTextRender;
    const FADED: u32 = 0x80FF_FFFF;
    const BACKGROUND: u32 = 0x4000_0000;

    let eye = renderer.camera_render_position();
    let anchor = renderer.camera_anchor();
    let (camera_y, camera_x) = renderer.camera_look_deg();
    let frustum = renderer.frustum_planes();
    let facing = glam::Mat4::from_quat(glam::Quat::from_euler(
        glam::EulerRot::YXZ,
        (-(camera_y - 180.0)).to_radians(),
        (-camera_x).to_radians(),
        0.0,
    ));
    let push = |at: glam::DVec3,
                layout: Arc<crate::entity::display::TextLayout>,
                discrete: bool,
                out: &mut Vec<(f64, WorldTextRender)>| {
        let distance = at.distance_squared(eye);
        let max = if discrete { 32.0 } else { 64.0 };
        if distance > max * max || layout.lines.is_empty() {
            return;
        }
        let rel = (at - eye).as_vec3();
        let radius = (layout.width * 0.5 + 2.0) * 0.025;
        if frustum
            .iter()
            .any(|p| p[0] * rel.x + p[1] * rel.y + p[2] * rel.z + p[3] < -radius)
        {
            return;
        }
        // The text pipeline puts a text's bottom on its anchor, 1 pixel right
        // of centre (`TextDisplayRenderer`); a name tag hangs from it, centred.
        let height = layout.lines.len() as f32 * 10.0 - 1.0;
        let pose = glam::Mat4::from_translation((at - anchor).as_vec3())
            * facing
            * glam::Mat4::from_translation(glam::vec3(-0.025, -0.025 * height, 0.0));
        use crate::renderer::chunk::block_lighter::LightSource;
        let levels = world_light.levels(at.floor().as_ivec3().to_array());
        let light =
            crate::renderer::chunk::block_lighter::LightLevels::new(levels.sky(), levels.block());
        // `LightCoordsUtil.lightCoordsWithEmission(light, 2)` for the full pass.
        let lit = crate::renderer::chunk::block_lighter::LightLevels::new(
            levels.sky().max(2),
            levels.block().max(2),
        );
        out.push((
            distance,
            WorldTextRender {
                layout: layout.clone(),
                pose,
                align: TextAlign::Center,
                color: FADED,
                background: BACKGROUND,
                shadow: false,
                see_through: !discrete,
                light: light.lane(),
            },
        ));
        if !discrete {
            out.push((
                distance,
                WorldTextRender {
                    layout,
                    pose,
                    align: TextAlign::Center,
                    color: 0xFFFF_FFFF,
                    background: 0,
                    shadow: false,
                    see_through: false,
                    light: lit.lane(),
                },
            ));
        }
    };

    // Other players (and NPC bots, which are players): their profile name
    // from the player list, unlisted entries too, in their team's style.
    for (id, entity) in &sources.living.living {
        let Some(uuid) = entity.player_uuid else {
            continue;
        };
        if uuid == sources.local_uuid || *id == sources.local_entity || entity.is_invisible {
            continue;
        }
        let Some(player) = sources.tab_list.players.get(&uuid) else {
            continue;
        };
        let feet = *entity
            .prev_position
            .lerp(entity.position, f64::from(partial_tick));
        let height = if entity.is_crouching { 1.5 } else { 1.8 };
        let at = feet + glam::DVec3::Y * (height + 0.5);
        let max = if entity.is_crouching { 32.0 } else { 64.0 };
        if at.distance_squared(eye) > max * max {
            continue;
        }
        let spans = sources.scoreboard.player_name(&player.name, None);
        let layout = sources.names.player_layout(*id, spans, |spans| {
            renderer.layout_display_text(spans, i32::MAX)
        });
        push(at, layout, entity.is_crouching, out);
    }

    // Entities showing their custom name.
    for (id, named) in sources.names.shown() {
        let feet = match sources.living.living.get(id) {
            Some(entity) => {
                if entity.player_uuid.is_some() {
                    continue;
                }
                *entity
                    .prev_position
                    .lerp(entity.position, f64::from(partial_tick))
            }
            None => {
                let lerp = |prev: Position, now: Position| *prev.lerp(now, f64::from(partial_tick));
                if let Some(item) = sources.items.get(*id) {
                    lerp(item.prev_position, item.position)
                } else if let Some(block) = sources.falling_blocks.blocks.get(id) {
                    lerp(block.prev_position, block.position)
                } else if let Some(at) = sources.positions.get(id) {
                    **at
                } else {
                    continue;
                }
            }
        };
        let at = feet + glam::DVec3::Y * (f64::from(named.name_height()) + 0.5);
        if at.distance_squared(eye) > 64.0 * 64.0 {
            continue;
        }
        let Some(name) = &named.name else { continue };
        let spans = match named.uuid {
            Some(uuid) => sources.scoreboard.entity_name(&uuid.to_string(), name),
            None => name.clone(),
        };
        let layout = named.layout(spans, |spans| renderer.layout_display_text(spans, i32::MAX));
        // A mannequin's description takes the name's place, and the name
        // moves up a line (`AvatarRenderer`'s below-name text, 9 * 1.15
        // pixels at 0.025 a pixel).
        match named.description_layout(|spans| renderer.layout_display_text(spans, i32::MAX)) {
            Some(description) => {
                push(at, description, false, out);
                push(at + glam::DVec3::Y * 0.25875, layout, false, out);
            }
            None => push(at, layout, false, out),
        }
    }
}

/// Where a display riding `vehicle` sits: `Entity.getPassengerRidingPosition`
/// with the default attachment at the top of the vehicle's box.
pub(crate) fn display_seat(
    vehicle: i32,
    living: &HashMap<i32, crate::entity::LivingEntity>,
    positions: &HashMap<i32, Position>,
    player: (i32, Position, f32),
) -> Option<glam::DVec3> {
    let (player_id, player_position, height) = player;
    if vehicle == player_id {
        return Some(*player_position + glam::dvec3(0.0, f64::from(height), 0.0));
    }
    if let Some(entity) = living.get(&vehicle) {
        let height = crate::entity::passenger_attachment_height(entity);
        return Some(*entity.position + glam::dvec3(0.0, height, 0.0));
    }
    positions.get(&vehicle).map(|position| **position)
}

#[derive(Default)]
struct EntityExtras {
    variant_index: u32,
    overlay_tints: [Option<[f32; 4]>; MAX_OVERLAYS],
    overlay_variants: [u32; MAX_OVERLAYS],
    head_y_offset: f32,
    head_x_rot_deg_override: Option<f32>,
    flap: f32,
    flap_speed: f32,
    body_transform: Option<glam::Mat4>,
    render_offset: glam::DVec3,
    nose_wobble_speed: f32,
    is_angry: bool,
    tail_angle: f32,
    head_roll_angle: f32,
    shake_anim: f32,
    lie_down_amount: f32,
    lie_down_amount_tail: f32,
    relax_state_one_amount: f32,
    hop_elapsed_secs: Option<f32>,
    /// Base-model tint (wolf wet shade, glow squid dimming, tropical fish
    /// base dye); `None` = white.
    base_tint: Option<[f32; 4]>,
    eat_anim: f32,
    stand_anim: f32,
    feeding_anim: f32,
    animate_tail: bool,
    tentacle_angle: f32,
    bat_elapsed_secs: Option<f32>,
    sniffer: crate::renderer::entity_model::SnifferAnims,
    allay: crate::renderer::entity_model::AllayAnims,
    copper_golem: crate::renderer::entity_model::CopperGolemAnims,
    llama_decor: Option<crate::renderer::pipelines::entity_renderer::LlamaDecor>,
    bee: crate::renderer::entity_model::BeeAnims,
    fox: crate::renderer::entity_model::FoxAnims,
    axolotl: crate::renderer::entity_model::AxolotlAnims,
    panda: crate::renderer::entity_model::PandaAnims,
    armadillo: crate::renderer::entity_model::ArmadilloAnims,
    goat: crate::renderer::entity_model::GoatAnims,
    parrot: crate::renderer::entity_model::ParrotAnims,
    dolphin_moving: bool,
    turtle: crate::renderer::entity_model::TurtleAnims,
    golem_attack_ticks: f32,
    golem_offer_flower_ticks: u32,
    /// Replaces the body yaw (`EnderDragonRenderer` turns by its flight
    /// history instead).
    body_y_rot_deg: Option<f32>,
    dragon: Option<Box<crate::renderer::entity_model::mob_anims::DragonPose>>,
}

/// `EnderDragonRenderer.submit`'s pose before the model: turned to where it
/// flew 7 ticks ago, pitched by how it climbed, pushed a block forward, and
/// riding the model root's flap (`EnderDragonModel.setupAnim`'s `root`).
fn dragon_extras(e: &crate::entity::LivingEntity, alpha: f32) -> EntityExtras {
    use crate::renderer::entity_model::mob_anims::{DragonPose, dragon_root};
    // A dragon that never recorded (no AI) reads an all-zero history, as
    // vanilla's does, so it faces its default way.
    let dragon = &e.dragon;
    let flap = dragon.prev_flap_time.lerp(dragon.flap_time, alpha);
    let samples = std::array::from_fn(|i| dragon.sample(i, alpha));
    let (root_y, root_z, root_x_rot) = dragon_root(flap);
    let climb = (samples[5].0 - samples[10].0) as f32;
    // The root's pose in pomme's part frame (the y-down bake conjugated,
    // the X flip and the 1.501 rebase outside it), so it sits between the
    // rebase and every top-level part as vanilla's root does.
    let rebase = glam::Vec3::new(0.0, 1.501, 0.0);
    let flip = glam::Mat4::from_scale(glam::Vec3::new(-1.0, 1.0, 1.0));
    let root = glam::Mat4::from_translation(glam::Vec3::new(0.0, -root_y, root_z) / 16.0)
        * glam::Mat4::from_rotation_x(-root_x_rot);
    let body_transform = glam::Mat4::from_rotation_x((climb * 10.0).to_radians())
        * glam::Mat4::from_translation(glam::Vec3::new(0.0, 0.0, 1.0))
        * flip
        * glam::Mat4::from_translation(rebase)
        * root
        * glam::Mat4::from_translation(-rebase)
        * flip;
    EntityExtras {
        overlay_tints: SLOT0_TINTS,
        body_transform: Some(body_transform),
        // `rotY(-yRot)`, which the entity matrix's `180 - bodyRot` gives.
        body_y_rot_deg: Some(180.0 + samples[7].1),
        dragon: Some(Box::new(DragonPose {
            flap,
            samples,
            sitting: dragon.is_sitting(),
            landing_or_taking_off: dragon.is_landing_or_taking_off(),
            // TODO: vanilla measures to the exit portal's top block.
            distance_to_egg: (e.position.x * e.position.x + e.position.z * e.position.z).sqrt(),
        })),
        ..Default::default()
    }
}

/// Only the first overlay slot visible, untinted.
const SLOT0_TINTS: [Option<[f32; 4]>; MAX_OVERLAYS] = {
    let mut tints = [None; MAX_OVERLAYS];
    tints[0] = Some(WHITE_TINT);
    tints
};

/// An `AbstractSkullBlock` item, which `LivingEntityRenderer` wears as a head.
fn is_skull(item: &azalea_inventory::ItemStack) -> bool {
    item.as_present().is_some_and(|d| {
        matches!(
            crate::player::inventory::item_resource_name(d.kind).as_str(),
            "skeleton_skull"
                | "wither_skeleton_skull"
                | "zombie_head"
                | "player_head"
                | "creeper_head"
                | "dragon_head"
                | "piglin_head"
        )
    })
}

/// Slot-0 overlay picked by a 1-based id (0 draws nothing), as
/// (`overlay_tints`, `overlay_variants`).
fn slot0_overlay(id: u32) -> ([Option<[f32; 4]>; MAX_OVERLAYS], [u32; MAX_OVERLAYS]) {
    let tints = if id != 0 {
        SLOT0_TINTS
    } else {
        [None; MAX_OVERLAYS]
    };
    (tints, [id.saturating_sub(1), 0, 0, 0])
}

fn entity_extras(
    entity_id: i32,
    e: &crate::entity::LivingEntity,
    alpha: f32,
    game_time: i64,
) -> EntityExtras {
    match e.entity_type {
        EntityKind::Cow | EntityKind::Mooshroom => EntityExtras {
            variant_index: e.variant,
            ..Default::default()
        },
        EntityKind::Chicken => EntityExtras {
            variant_index: e.variant,
            flap: e.prev_flap.lerp(e.flap, alpha),
            flap_speed: e.prev_flap_speed.lerp(e.flap_speed, alpha),
            ..Default::default()
        },
        EntityKind::Sheep => sheep_extras(entity_id, e, alpha),
        EntityKind::Villager => villager_like_extras(e, &VILLAGER_TYPE_HAT),
        EntityKind::ZombieVillager => villager_like_extras(e, &ZOMBIE_VILLAGER_TYPE_HAT),
        EntityKind::Bogged => EntityExtras {
            overlay_tints: SLOT0_TINTS,
            variant_index: e.is_sheared as u32,
            ..Default::default()
        },
        // Always-visible slot-0 overlay (spider eyes, drowned/stray clothing).
        EntityKind::Spider | EntityKind::Drowned | EntityKind::Stray => EntityExtras {
            overlay_tints: SLOT0_TINTS,
            ..Default::default()
        },
        // The wind and the eyes, both always shown.
        EntityKind::Breeze => {
            let mut overlay_tints = SLOT0_TINTS;
            overlay_tints[1] = Some(WHITE_TINT);
            EntityExtras {
                overlay_tints,
                ..Default::default()
            }
        }
        // `CaveSpiderRenderer.SCALE`.
        EntityKind::CaveSpider => EntityExtras {
            overlay_tints: SLOT0_TINTS,
            body_transform: Some(glam::Mat4::from_scale(glam::Vec3::splat(0.7))),
            ..Default::default()
        },
        EntityKind::Enderman => EntityExtras {
            overlay_tints: SLOT0_TINTS,
            // Vanilla `EndermanRenderer.getRenderOffset`: per-frame gaussian
            // x/z shake while screaming.
            render_offset: if e.is_creepy {
                glam::DVec3::new(
                    crate::particle::next_gaussian() * 0.02,
                    0.0,
                    crate::particle::next_gaussian() * 0.02,
                )
            } else {
                glam::DVec3::ZERO
            },
            ..Default::default()
        },
        EntityKind::Slime => EntityExtras {
            overlay_tints: SLOT0_TINTS,
            body_transform: Some(cube_body_transform(e, alpha, true)),
            ..Default::default()
        },
        EntityKind::SulfurCube => sulfur_cube_extras(e, alpha),
        // Without a skin of its own, one of vanilla's default skins.
        EntityKind::Player => EntityExtras {
            variant_index: crate::renderer::pipelines::entity_renderer::default_skin_variant(
                &e.uuid,
            ),
            ..Default::default()
        },
        // `ClientMannequin.DEFAULT_SKIN`: a mannequin without a profile skin
        // wears the default skin of the empty profile (the nil UUID), not one
        // picked by its own id.
        EntityKind::Mannequin => EntityExtras {
            variant_index: crate::renderer::pipelines::entity_renderer::default_skin_variant(
                &uuid::Uuid::nil(),
            ),
            ..Default::default()
        },
        // Arms (`ShowArms`) and no base plate, as the stand's model variant.
        EntityKind::ArmorStand => EntityExtras {
            variant_index: u32::from(e.armor_stand_flags & 0x04 != 0)
                | u32::from(e.armor_stand_flags & 0x08 != 0) << 1,
            ..Default::default()
        },
        // `MagmaCubeRenderer.scale`: a slime's size and squish.
        EntityKind::MagmaCube => EntityExtras {
            body_transform: Some(cube_body_transform(e, alpha, true)),
            ..Default::default()
        },
        // The renderers' fixed `scale`: `WitherSkeletonRenderer`,
        // `GhastRenderer`, `ElderGuardianRenderer`, `GiantMobRenderer`,
        // `HappyGhastRenderer`.
        EntityKind::WitherSkeleton
        | EntityKind::Ghast
        | EntityKind::ElderGuardian
        | EntityKind::Giant
        | EntityKind::HappyGhast => EntityExtras {
            body_transform: Some(glam::Mat4::from_scale(glam::Vec3::splat(
                match e.entity_type {
                    EntityKind::WitherSkeleton => 1.2,
                    EntityKind::Ghast => 4.5,
                    EntityKind::Giant => 6.0,
                    EntityKind::HappyGhast if e.is_baby => 0.95,
                    EntityKind::HappyGhast => 4.0,
                    _ => 2.35,
                },
            ))),
            ..Default::default()
        },
        // `WitherBossRenderer`: twice the model's size, less while it's
        // still charging up, when its skin flickers to the invulnerable one.
        EntityKind::Wither => {
            let ticks = e.wither_invulnerable_ticks;
            let left = (ticks as f32 - alpha).max(0.0);
            let scale = if ticks > 0 {
                2.0 - left / 220.0 * 0.5
            } else {
                2.0
            };
            EntityExtras {
                variant_index: u32::from(ticks > 0 && (ticks > 80 || ticks / 5 % 2 != 1)),
                body_transform: Some(glam::Mat4::from_scale(glam::Vec3::splat(scale))),
                ..Default::default()
            }
        }
        EntityKind::EnderDragon => dragon_extras(e, alpha),
        EntityKind::Allay => EntityExtras {
            allay: crate::renderer::entity_model::AllayAnims {
                dancing: e.allay.dancing,
                spinning: e.allay.is_spinning(),
                spinning_progress: e
                    .allay
                    .prev_spinning_ticks
                    .lerp(e.allay.spinning_ticks, alpha)
                    / 15.0,
                holding_progress: e
                    .allay
                    .prev_holding_ticks
                    .lerp(e.allay.holding_ticks, alpha)
                    / 5.0,
            },
            ..Default::default()
        },
        EntityKind::CopperGolem => copper_golem_extras(e, alpha),
        EntityKind::Witch => EntityExtras {
            nose_wobble_speed: 0.01 * (entity_id % 10) as f32,
            ..Default::default()
        },
        EntityKind::Wolf => wolf_extras(e, alpha, game_time),
        EntityKind::Cat => cat_extras(e, alpha),
        EntityKind::Horse => {
            // Markings overlay; id 0 = NONE.
            let (overlay_tints, overlay_variants) = slot0_overlay((e.variant >> 8) & 0xFF);
            EntityExtras {
                variant_index: e.variant & 0xFF,
                overlay_tints,
                overlay_variants,
                ..equine_extras(e, alpha)
            }
        }
        EntityKind::Donkey | EntityKind::Mule => EntityExtras {
            variant_index: e.has_chest as u32,
            ..equine_extras(e, alpha)
        },
        EntityKind::SkeletonHorse | EntityKind::ZombieHorse => equine_extras(e, alpha),
        EntityKind::Llama | EntityKind::TraderLlama => llama_extras(e),
        EntityKind::Squid | EntityKind::GlowSquid => squid_extras(e, alpha),
        EntityKind::Bat => EntityExtras {
            bat_elapsed_secs: e.bat_anim_start.map(|s| anim_clock_secs(e, s, alpha)),
            ..Default::default()
        },
        EntityKind::Cod
        | EntityKind::Salmon
        | EntityKind::TropicalFish
        | EntityKind::Pufferfish => fish_extras(e, alpha),
        EntityKind::IronGolem => golem_extras(e, alpha),
        EntityKind::Armadillo => EntityExtras {
            armadillo: armadillo_anims(e, alpha),
            ..Default::default()
        },
        // `GoatRenderer.extractRenderState`.
        EntityKind::Goat => EntityExtras {
            goat: crate::renderer::entity_model::GoatAnims {
                has_left_horn: e.goat.has_left_horn,
                has_right_horn: e.goat.has_right_horn,
                ramming_x_head_rot: e.goat.ramming_x_head_rot(e.is_baby),
            },
            ..Default::default()
        },
        // `DolphinRenderer.extractRenderState`.
        EntityKind::Dolphin => EntityExtras {
            dolphin_moving: e.velocity.x * e.velocity.x + e.velocity.z * e.velocity.z > 1.0e-7,
            ..Default::default()
        },
        // `TurtleRenderer.extractRenderState`.
        EntityKind::Turtle => EntityExtras {
            turtle: crate::renderer::entity_model::TurtleAnims {
                on_land: !e.is_in_water && e.on_ground,
                laying_egg: e.laying_egg,
                has_egg: !e.is_baby && e.has_egg,
            },
            ..Default::default()
        },
        EntityKind::Bee => bee_extras(e, alpha, game_time),
        EntityKind::Fox => fox_extras(e, alpha),
        EntityKind::Axolotl => EntityExtras {
            variant_index: e.variant,
            axolotl: axolotl_anims(e, alpha),
            ..Default::default()
        },
        // `PolarBear.getStandingAnimationScale`.
        EntityKind::PolarBear => EntityExtras {
            stand_anim: lerp(alpha, e.prev_stand_anim, e.stand_anim) / 6.0,
            ..Default::default()
        },
        EntityKind::Panda => panda_extras(e, alpha),
        EntityKind::Parrot => parrot_extras(e, alpha),
        EntityKind::Sniffer => EntityExtras {
            sniffer: sniffer_anims(e, alpha),
            ..Default::default()
        },
        EntityKind::Rabbit => EntityExtras {
            // "Toast" overrides the variant texture (slot 7).
            variant_index: if e.custom_name.as_deref() == Some("Toast") {
                7
            } else {
                e.variant
            },
            hop_elapsed_secs: e.hop_anim_start.map(|s| anim_clock_secs(e, s, alpha)),
            ..Default::default()
        },
        // `CreeperRenderer.scale`: a swelling creeper fattens with a
        // shudder; its charged aura (slot 0) shows only when powered.
        EntityKind::Creeper => {
            let swelling = crate::entity::creeper_swelling(e, alpha);
            let wobble = 1.0 + (swelling * 100.0).sin() * swelling * 0.01;
            let g = swelling.clamp(0.0, 1.0).powi(4);
            let wide = (1.0 + g * 0.4) * wobble;
            let tall = (1.0 + g * 0.1) / wobble;
            EntityExtras {
                overlay_tints: if e.powered {
                    SLOT0_TINTS
                } else {
                    Default::default()
                },
                body_transform: (swelling > 0.0)
                    .then(|| glam::Mat4::from_scale(glam::Vec3::new(wide, tall, wide))),
                ..Default::default()
            }
        }
        _ => EntityExtras::default(),
    }
}

/// Vanilla `LlamaRenderer.extractRenderState` and `LlamaDecorLayer.submit`:
/// babies show neither a chest nor a carpet, and a body item with an
/// equipment asset replaces the trader decor.
fn llama_extras(e: &crate::entity::LivingEntity) -> EntityExtras {
    use crate::renderer::pipelines::entity_renderer::{
        ArmorRenderInfo, LlamaDecor, equipment_render_info,
    };
    let has_chest = !e.is_baby && e.has_chest;
    let body = e
        .equipment(EquipmentSlot::Body)
        .and_then(equipment_render_info);
    let equipment = match body {
        Some(body) if !e.is_baby => Some(body),
        // `EquipmentAssets.TRADER_LLAMA(_BABY)` with an empty stack.
        _ if e.entity_type == EntityKind::TraderLlama => Some(ArmorRenderInfo {
            asset_id: if e.is_baby {
                "minecraft:trader_llama_baby".into()
            } else {
                "minecraft:trader_llama".into()
            },
            dyed_color: None,
            trim: None,
        }),
        _ => None,
    };
    EntityExtras {
        variant_index: e.variant + if has_chest { 4 } else { 0 },
        llama_decor: equipment.map(|equipment| LlamaDecor {
            equipment,
            has_chest,
        }),
        ..Default::default()
    }
}

/// Vanilla `FoxRenderer.extractRenderState` and `setupRotations` (the
/// pounce / faceplant pitch). `variant_index = variant * 2 + sleeping`.
// TODO: `FoxHeldItemLayer` (the item in the fox's mouth).
fn fox_extras(e: &crate::entity::LivingEntity, alpha: f32) -> EntityExtras {
    let f = &e.fox;
    let fox = crate::renderer::entity_model::FoxAnims {
        head_roll_angle: e.interested_angle(alpha) * 0.11 * std::f32::consts::PI,
        crouch_amount: lerp(alpha, f.crouch_amount_o, f.crouch_amount),
        is_crouching: f.is_crouching(),
        is_sleeping: f.is_sleeping(),
        is_sitting: e.is_sitting,
        is_faceplanted: f.is_faceplanted(),
        is_pouncing: f.is_pouncing(),
        leg_motion_pos: if f.is_faceplanted() {
            crate::renderer::entity_model::advance_fox_leg_motion_pos(e.is_baby)
        } else {
            0.0
        },
    };
    let x_rot = e.view_look(alpha).x_rot_deg();
    EntityExtras {
        variant_index: e.variant * 2 + u32::from(fox.is_sleeping),
        body_transform: (fox.is_pouncing || fox.is_faceplanted)
            .then(|| glam::Mat4::from_rotation_x((-x_rot).to_radians())),
        fox,
        ..Default::default()
    }
}

/// Vanilla `PandaRenderer.extractRenderState` and `setupRotations`;
/// `variant_index` is the shown gene.
// TODO: `PandaHoldsItemLayer` (the held bamboo/cake while sitting).
fn panda_extras(e: &crate::entity::LivingEntity, alpha: f32) -> EntityExtras {
    let p = &e.panda;
    let panda = crate::renderer::entity_model::PandaAnims {
        is_unhappy: e.unhappy_counter > 0,
        is_sneezing: p.is_sneezing(),
        sneeze_time: p.sneeze_counter,
        is_eating: p.is_eating(),
        is_scared: p.is_scared,
        sit_amount: lerp(alpha, p.sit_amount_o, p.sit_amount),
        lie_on_back_amount: lerp(alpha, p.on_back_amount_o, p.on_back_amount),
        roll_amount: if e.is_baby {
            0.0
        } else {
            lerp(alpha, p.roll_amount_o, p.roll_amount)
        },
        roll_time: if p.roll_counter > 0 {
            p.roll_counter as f32 + alpha
        } else {
            0.0
        },
    };
    let x_rot = e.view_look(alpha).x_rot_deg();
    EntityExtras {
        variant_index: p.variant(),
        body_transform: crate::renderer::entity_model::panda_body_transform(
            &panda,
            e.is_baby,
            x_rot,
            e.age_in_ticks as f32 + alpha,
        ),
        panda,
        ..Default::default()
    }
}

/// Vanilla `BeeRenderer.extractRenderState` and `getTextureLocation`
/// (variants: plain, nectar, angry, angry with nectar).
fn bee_extras(e: &crate::entity::LivingEntity, alpha: f32, game_time: i64) -> EntityExtras {
    // `NeutralMob.isAngry`.
    let is_angry = e.anger_end_time > 0 && e.anger_end_time - game_time > 0;
    let has_nectar = e.bee.has_nectar();
    EntityExtras {
        variant_index: u32::from(is_angry) * 2 + u32::from(has_nectar),
        bee: crate::renderer::entity_model::BeeAnims {
            roll_amount: lerp(alpha, e.bee.prev_roll_amount, e.bee.roll_amount),
            has_stinger: !e.bee.has_stung(),
            is_on_ground: e.on_ground && e.velocity.length_squared() < 1.0e-7,
            is_angry,
        },
        ..Default::default()
    }
}

/// Vanilla `ParrotRenderer.extractRenderState` (`ParrotModel.getPose`).
fn parrot_extras(e: &crate::entity::LivingEntity, alpha: f32) -> EntityExtras {
    use crate::renderer::entity_model::ParrotPose;
    let flap = lerp(alpha, e.prev_flap, e.flap);
    let flap_speed = lerp(alpha, e.prev_flap_speed, e.flap_speed);
    let pose = if e.party_parrot {
        ParrotPose::Party
    } else if e.is_sitting {
        ParrotPose::Sitting
    } else if !e.on_ground {
        ParrotPose::Flying
    } else {
        ParrotPose::Standing
    };
    EntityExtras {
        variant_index: e.variant,
        parrot: crate::renderer::entity_model::ParrotAnims {
            flap_angle: (flap.sin() + 1.0) * flap_speed,
            pose,
        },
        ..Default::default()
    }
}

/// Vanilla `AxolotlRenderer.extractRenderState`.
fn axolotl_anims(
    e: &crate::entity::LivingEntity,
    alpha: f32,
) -> crate::renderer::entity_model::AxolotlAnims {
    use crate::entity::BabyAxolotlAnim as A;
    let sim = &e.axolotl;
    let mut anims = crate::renderer::entity_model::AxolotlAnims {
        playing_dead_factor: sim.playing_dead_animator.factor(alpha),
        in_water_factor: sim.in_water_animator.factor(alpha),
        on_ground_factor: sim.on_ground_animator.factor(alpha),
        moving_factor: sim.moving_animator.factor(alpha),
        ..Default::default()
    };
    if let Some((anim, start)) = sim.baby_anim {
        let clock = Some(anim_clock_secs(e, start, alpha));
        match anim {
            A::Swim => anims.swim = clock,
            A::Walk => anims.walk = clock,
            A::IdleUnderWater => anims.idle_under_water = clock,
            A::IdleUnderWaterOnGround => anims.idle_under_water_on_ground = clock,
            A::IdleOnGround => anims.idle_on_ground = clock,
            A::PlayDead => anims.play_dead = clock,
            // `BabyAxolotlModel` never reads this state.
            A::WalkUnderWater => {}
        }
    }
    anims
}

/// Vanilla `ArmadilloRenderer.extractRenderState`.
fn armadillo_anims(
    e: &crate::entity::LivingEntity,
    alpha: f32,
) -> crate::renderer::entity_model::ArmadilloAnims {
    let a = &e.armadillo;
    // `AnimationState.getTimeInMillis`, whose start may sit before tick 0
    // after a `fastForward`.
    let clock = |start: Option<i64>| {
        start.map(|s| ((e.age_in_ticks as f32 + alpha - s as f32) * 50.0) as i64 as f32 / 1000.0)
    };
    crate::renderer::entity_model::ArmadilloAnims {
        hiding_in_shell: a.hiding_in_shell(),
        roll_out: clock(a.roll_out_start),
        roll_up: clock(a.roll_up_start),
        peek: clock(a.peek_start),
    }
}

/// Vanilla `SnifferRenderer.extractRenderState`.
fn sniffer_anims(
    e: &crate::entity::LivingEntity,
    alpha: f32,
) -> crate::renderer::entity_model::SnifferAnims {
    use crate::entity::SnifferState as S;
    let clock = e.sniffer_anim_start.map(|s| anim_clock_secs(e, s, alpha));
    let mut anims = crate::renderer::entity_model::SnifferAnims {
        searching: e.sniffer_state == S::Searching,
        ..Default::default()
    };
    match e.sniffer_state {
        S::Digging => anims.digging = clock,
        S::Sniffing => anims.sniffing = clock,
        S::Rising => anims.rising = clock,
        S::FeelingHappy => anims.feeling_happy = clock,
        S::Scenting => anims.scenting = clock,
        S::Idling | S::Searching => {}
    }
    anims
}

/// Seconds on a vanilla `AnimationState` clock started at tick `start`
/// (clocks start one tick ahead of the current age, so clamp at 0).
fn anim_clock_secs(e: &crate::entity::LivingEntity, start: u32, alpha: f32) -> f32 {
    (e.age_in_ticks as f32 - start as f32 + alpha).max(0.0) * 0.05
}

/// Copper golem: weathering picks the body and eyes textures.
fn copper_golem_extras(e: &crate::entity::LivingEntity, alpha: f32) -> EntityExtras {
    let g = &e.copper_golem;
    let clock = |start| Some(anim_clock_secs(e, start, alpha));
    let interaction = |state| match g.interaction {
        Some((s, start)) if s == state => clock(start),
        _ => None,
    };
    let (overlay_tints, overlay_variants) = slot0_overlay(g.weather + 1);
    EntityExtras {
        variant_index: g.weather,
        overlay_tints,
        overlay_variants,
        copper_golem: crate::renderer::entity_model::CopperGolemAnims {
            holding_item: [EquipmentSlot::Mainhand, EquipmentSlot::Offhand]
                .into_iter()
                .any(|slot| e.equipment(slot).is_some()),
            idle: g.idle_anim_start.and_then(clock),
            get_item: interaction(1),
            get_no_item: interaction(2),
            drop_item: interaction(3),
            drop_no_item: interaction(4),
        },
        ..Default::default()
    }
}

/// Vanilla `SulfurCubeRenderer.scale`: a carried block stops the squish and
/// hides the inner cube, and a lit fuse swells it like primed TNT. A skull in
/// the body or head slot only hides the inner cube.
fn sulfur_cube_extras(e: &crate::entity::LivingEntity, alpha: f32) -> EntityExtras {
    let body = e.equipment(EquipmentSlot::Body);
    // TODO: a non-block body item resolves to air in vanilla, which still
    // squishes and draws the inner cube.
    let carries_block = body.is_some_and(|item| !is_skull(item));
    let hides_inner = body.is_some() || e.equipment(EquipmentSlot::Head).is_some_and(is_skull);
    let fuse = if e.sulfur_fuse >= 0 {
        e.sulfur_fuse as f32 - alpha + 1.0
    } else {
        0.0
    };
    // `TntRenderer.getSwellAmount`.
    let swell = if fuse > 0.0 && fuse < 10.0 {
        1.0 + (1.0 - fuse / 10.0).clamp(0.0, 1.0).powi(4) * 0.3
    } else {
        1.0
    };
    let (extra_downscale, v_offset) = if e.is_baby { (1.0, 1.24) } else { (0.5, 0.98) };
    let one_pixel_up = if e.is_invisible { 0.0 } else { 1.0 / 16.0 };
    EntityExtras {
        overlay_tints: if hides_inner {
            [None; MAX_OVERLAYS]
        } else {
            SLOT0_TINTS
        },
        body_transform: Some(
            cube_body_transform(e, alpha, !carries_block)
                * glam::Mat4::from_scale(glam::Vec3::splat(swell * extra_downscale))
                * glam::Mat4::from_translation(glam::Vec3::new(0.0, one_pixel_up - v_offset, 0.0)),
        ),
        ..Default::default()
    }
}

/// Vanilla `AbstractCubeMobRenderer.applySizeAndSquish` plus
/// `downscaleSlightly` (0.999 shrink + a 0.001 drop that tucks the inner body
/// under the shell surface; vanilla's +0.001 is in flipped space = down).
fn cube_body_transform(e: &crate::entity::LivingEntity, alpha: f32, squishes: bool) -> glam::Mat4 {
    let squish = if squishes {
        e.prev_squish + (e.squish - e.prev_squish) * alpha
    } else {
        0.0
    };
    let size = e.slime_size as f32;
    let ss = squish / (size * 0.5 + 1.0);
    let w = 1.0 / (ss + 1.0);
    glam::Mat4::from_scale(glam::Vec3::splat(0.999))
        * glam::Mat4::from_translation(glam::Vec3::new(0.0, -0.001, 0.0))
        * glam::Mat4::from_scale(glam::Vec3::new(w * size, size / w, w * size))
}

/// Iron golem: `IronGolemRenderer.setupRotations` body sway, the punch /
/// flower countdowns, and the `IronGolemCrackinessLayer` health overlay.
fn golem_extras(e: &crate::entity::LivingEntity, alpha: f32) -> EntityExtras {
    // `Crackiness.GOLEM` thresholds over max health 100 (attributes aren't
    // parsed; vanilla never modifies the golem's).
    let crack_level = match e.health / 100.0 {
        f if f < 0.25 => 3,
        f if f < 0.5 => 2,
        f if f < 0.75 => 1,
        _ => 0,
    };
    let (overlay_tints, overlay_variants) = slot0_overlay(crack_level);
    EntityExtras {
        golem_attack_ticks: if e.golem_attack_ticks > 0 {
            e.golem_attack_ticks as f32 - alpha
        } else {
            0.0
        },
        golem_offer_flower_ticks: e.golem_offer_flower_ticks as u32,
        // +-6.5 degree roll in step with the walk cycle.
        body_transform: (e.walk_speed(alpha) >= 0.01).then(|| {
            let sway = 6.5 * triangle_wave(e.walk_pos(alpha) + 6.0, 13.0);
            glam::Mat4::from_rotation_z(sway.to_radians())
        }),
        overlay_tints,
        overlay_variants,
        ..Default::default()
    }
}

/// Squid tentacle stroke + the `SquidRenderer.setupRotations` body pitch and
/// axial spin; glow squid adds the post-hurt dimming.
fn squid_extras(e: &crate::entity::LivingEntity, alpha: f32) -> EntityExtras {
    let x_rot = e.prev_x_body_rot + (e.x_body_rot - e.prev_x_body_rot) * alpha;
    // z_body_rot grows without bound; wrap only here, after the lerp.
    let z_rot = (e.prev_z_body_rot + (e.z_body_rot - e.prev_z_body_rot) * alpha).rem_euclid(360.0);
    let (up, down) = if e.is_baby { (0.25, -0.6) } else { (0.5, -1.2) };
    EntityExtras {
        tentacle_angle: e.prev_tentacle_angle + (e.tentacle_angle - e.prev_tentacle_angle) * alpha,
        // The axial spin is applied about Y, after the pitch (vanilla).
        body_transform: Some(
            glam::Mat4::from_translation(glam::Vec3::new(0.0, up, 0.0))
                * glam::Mat4::from_rotation_x(x_rot.to_radians())
                * glam::Mat4::from_rotation_y(z_rot.to_radians())
                * glam::Mat4::from_translation(glam::Vec3::new(0.0, down, 0.0)),
        ),
        ..Default::default()
    }
}

/// The four fish renderers' `setupRotations`: body wobble about Y, the
/// on-land 90 degree flop roll, and the pufferfish bob; plus per-kind variant
/// and tint selection.
fn fish_extras(e: &crate::entity::LivingEntity, alpha: f32) -> EntityExtras {
    use std::f32::consts::FRAC_PI_2;
    let age = e.age_in_ticks as f32 + alpha;
    if e.entity_type == EntityKind::Pufferfish {
        return EntityExtras {
            variant_index: e.puff_state as u32,
            render_offset: glam::DVec3::new(0.0, ((age * 0.05).cos() * 0.08) as f64, 0.0),
            ..Default::default()
        };
    }
    // Only the salmon scales its wobble when out of water.
    let (amp, ang) = if e.entity_type == EntityKind::Salmon && !e.is_in_water {
        (1.3, 1.7)
    } else {
        (1.0, 1.0)
    };
    let wobble = (amp * 4.3 * (ang * 0.6 * age).sin()).to_radians();
    let mut m = glam::Mat4::from_rotation_y(wobble);
    if !e.is_in_water {
        let t = if e.entity_type == EntityKind::Cod {
            glam::Vec3::new(0.1, 0.1, -0.1)
        } else {
            glam::Vec3::new(0.2, 0.1, 0.0)
        };
        m *= glam::Mat4::from_translation(t) * glam::Mat4::from_rotation_z(FRAC_PI_2);
    }
    let mut extras = EntityExtras {
        body_transform: Some(m),
        ..Default::default()
    };
    match e.entity_type {
        EntityKind::Salmon => extras.variant_index = e.variant,
        EntityKind::TropicalFish => {
            // Packed variant: b0 shape, b1 pattern, b2 base dye, b3 pattern
            // dye. An unknown shape/pattern pair falls back to KOB (small,
            // pattern 0) like vanilla's sparse id map.
            let v = e.variant as i32;
            let (shape, pattern) = match ((v & 0xFF) as usize, ((v >> 8) & 0xFF) as u32) {
                (shape @ 0..=1, pattern @ 0..=5) => (shape, pattern),
                _ => (0, 0),
            };
            extras.variant_index = shape as u32;
            extras.base_tint = Some(dye_color_tint(((v >> 16) & 0xFF) as u8));
            extras.overlay_tints[shape] = Some(dye_color_tint(((v >> 24) & 0xFF) as u8));
            extras.overlay_variants = [pattern, pattern, 0, 0];
        }
        _ => {}
    }
    extras
}

fn equine_extras(e: &crate::entity::LivingEntity, alpha: f32) -> EntityExtras {
    EntityExtras {
        eat_anim: e.prev_eat_anim + (e.eat_anim - e.prev_eat_anim) * alpha,
        stand_anim: e.prev_stand_anim + (e.stand_anim - e.prev_stand_anim) * alpha,
        feeding_anim: e.prev_mouth_anim + (e.mouth_anim - e.prev_mouth_anim) * alpha,
        animate_tail: e.tail_swishing(),
        ..Default::default()
    }
}

/// Wolf texture state (`variant_index = variant * 3 + state`, tame > angry >
/// wild priority), collar tint, tail angle, and the beg/shake/wet values.
fn wolf_extras(e: &crate::entity::LivingEntity, alpha: f32, game_time: i64) -> EntityExtras {
    use std::f32::consts::PI;
    let is_angry = e.anger_end_time > 0 && e.anger_end_time > game_time;
    let state = if e.is_tame {
        1
    } else if is_angry {
        2
    } else {
        0
    };
    let tail_angle = if is_angry {
        1.5393804
    } else if e.is_tame {
        // Tame wolves carry their health in the tail; tame max health is a
        // fixed 40 (`applyTamingSideEffects`), attributes aren't parsed.
        let max_health = 40.0;
        (0.55 - (max_health - e.health) / max_health * 0.4) * PI
    } else {
        0.62831855
    };
    let mut overlay_tints = [None; MAX_OVERLAYS];
    if e.is_tame {
        overlay_tints[0] = Some(dye_color_tint(e.collar_color));
    }
    let wet = e.wet_shade(alpha);
    EntityExtras {
        variant_index: e.variant * 3 + state,
        overlay_tints,
        is_angry,
        tail_angle,
        head_roll_angle: e.interested_angle(alpha) * 0.15 * PI,
        shake_anim: e.prev_shake_anim + (e.shake_anim - e.prev_shake_anim) * alpha,
        base_tint: Some([wet, wet, wet, 1.0]),
        ..Default::default()
    }
}

/// Cat collar, pose springs, and the lie-down whole-body roll (vanilla
/// `CatRenderer.setupRotations`).
// TODO: the extra 0.15 offset while lying on a sleeping player.
fn cat_extras(e: &crate::entity::LivingEntity, alpha: f32) -> EntityExtras {
    let mut overlay_tints = [None; MAX_OVERLAYS];
    if e.is_tame {
        overlay_tints[0] = Some(dye_color_tint(e.collar_color));
    }
    let lie = e.prev_lie_down_amount + (e.lie_down_amount - e.prev_lie_down_amount) * alpha;
    let body_transform = (lie > 0.0).then(|| {
        glam::Mat4::from_translation(glam::Vec3::new(0.4 * lie, 0.15 * lie, 0.1 * lie))
            * glam::Mat4::from_rotation_z((90.0 * lie).to_radians())
    });
    EntityExtras {
        variant_index: e.variant,
        overlay_tints,
        lie_down_amount: lie,
        lie_down_amount_tail: e.prev_lie_down_amount_tail
            + (e.lie_down_amount_tail - e.prev_lie_down_amount_tail) * alpha,
        relax_state_one_amount: e.prev_relax_state_one_amount
            + (e.relax_state_one_amount - e.prev_relax_state_one_amount) * alpha,
        body_transform,
        ..Default::default()
    }
}

fn sheep_extras(entity_id: i32, e: &crate::entity::LivingEntity, alpha: f32) -> EntityExtras {
    let is_jeb = e.custom_name.as_deref() == Some("jeb_");
    let tint = if is_jeb {
        jeb_sheep_tint(entity_id, e.age_in_ticks)
    } else if let Some(c) = e.wool_color {
        wool_color_tint(c)
    } else {
        WHITE_TINT
    };

    let mut overlay_tints = [None; MAX_OVERLAYS];
    if !e.is_sheared {
        if e.is_baby {
            overlay_tints[0] = Some(tint);
        } else {
            let undercoat_visible = is_jeb || e.wool_color.is_some_and(|c| c != 0);
            overlay_tints[0] = if undercoat_visible { Some(tint) } else { None };
            overlay_tints[1] = Some(tint);
        }
    }

    let (pos_scale, angle_scale) = sheep_eat_scales(e.eat_anim_tick, e.prev_eat_anim_tick, alpha);
    let age_scale = if e.is_baby { 0.5 } else { 1.0 };
    let head_y_offset = pos_scale * 9.0 * age_scale;
    let head_x_rot_deg_override = if e.eat_anim_tick > 0 || e.prev_eat_anim_tick > 0 {
        Some(angle_scale)
    } else {
        None
    };

    EntityExtras {
        overlay_tints,
        head_y_offset,
        head_x_rot_deg_override,
        ..Default::default()
    }
}

/// Whether the type texture's built-in hat is fully or partially covered by
/// the profession texture's own hat, per the `villager` sections of the
/// `.png.mcmeta` files under `textures/entity/villager/` (hardcoded — no
/// resource-pack support). 0 = none, 1 = partial, 2 = full.
const VILLAGER_TYPE_HAT: [u8; 7] = [2, 0, 0, 0, 2, 0, 0]; // desert, snow = full
// `zombie_villager/type/` ships no `.mcmeta` files at all.
const ZOMBIE_VILLAGER_TYPE_HAT: [u8; 7] = [0; 7];
const VILLAGER_PROFESSION_HAT: [u8; 15] = [
    0, // none
    0, // armorer
    1, // butcher (partial)
    0, // cartographer
    0, // cleric
    2, // farmer
    2, // fisherman
    2, // fletcher
    0, // leatherworker
    2, // librarian
    0, // mason
    0, // nitwit
    2, // shepherd
    0, // toolsmith
    0, // weaponsmith
];

/// Overlay slots: 0 = biome type (full model), 1 = biome type (no-hat model),
/// 2 = profession, 3 = profession level. Mirrors vanilla
/// `VillagerProfessionLayer.submit`, shared by villager and zombie villager
/// (which differ only in their type-hat `.mcmeta` tables).
fn villager_like_extras(e: &crate::entity::LivingEntity, type_hat_table: &[u8; 7]) -> EntityExtras {
    use crate::entity::villager::VillagerProfession;

    let kind = e.villager_kind as usize;
    let profession = e.villager_profession as usize;

    let type_hat = type_hat_table[kind];
    let prof_hat = VILLAGER_PROFESSION_HAT[profession];
    let type_hat_visible = prof_hat == 0 || (prof_hat == 1 && type_hat != 2);

    let mut overlay_tints = [None; MAX_OVERLAYS];
    overlay_tints[if type_hat_visible { 0 } else { 1 }] = Some(WHITE_TINT);
    // Profession and level layers are adult-only; nitwits have no level badge.
    if !e.is_baby && e.villager_profession != VillagerProfession::None {
        overlay_tints[2] = Some(WHITE_TINT);
        if e.villager_profession != VillagerProfession::Nitwit {
            overlay_tints[3] = Some(WHITE_TINT);
        }
    }

    EntityExtras {
        overlay_tints,
        overlay_variants: [
            kind as u32,
            kind as u32,
            (profession as u32).saturating_sub(1),
            e.villager_level.clamp(1, 5) - 1,
        ],
        ..Default::default()
    }
}

fn sheep_eat_scales(eat_tick: u8, prev_eat_tick: u8, alpha: f32) -> (f32, f32) {
    use std::f32::consts::PI;

    // Mirrors vanilla Sheep.java:127-149. Linear-blend previous and current tick
    // first so the head dip is smooth between server ticks.
    let interp = prev_eat_tick as f32 + (eat_tick as f32 - prev_eat_tick as f32) * alpha;
    let pos_scale = if interp <= 0.0 {
        0.0
    } else if (4.0..=36.0).contains(&interp) {
        1.0
    } else if interp < 4.0 {
        interp / 4.0
    } else {
        -(interp - 40.0) / 4.0
    };

    let angle_scale = if (4.0..36.0).contains(&interp) {
        let s = (interp - 4.0) / 32.0;
        PI / 5.0 + (PI * 7.0 / 100.0) * (s * 28.7).sin()
    } else if interp > 0.0 {
        PI / 5.0
    } else {
        0.0
    };

    (pos_scale, angle_scale)
}

/// Vanilla `LightmapRenderStateExtractor.tick`: the block light's random
/// walk.
fn tick_block_light_flicker(flicker: f32) -> f32 {
    let r = fastrand::f32;
    (flicker + (r() - r()) * r() * r() * 0.1) * 0.9
}

/// Vanilla `GameRenderer.getNightVisionScale`: full while the effect has
/// more than 200 ticks left (or no end), then flickering as it runs out.
// TODO: conduit power's underwater vision.
fn night_vision_scale(duration: Option<i32>, partial_tick: f32) -> f32 {
    match duration {
        None => 0.0,
        Some(d) if !(0..=200).contains(&d) => 1.0,
        Some(d) => 0.7 + ((d as f32 - partial_tick) * std::f32::consts::PI * 0.2).sin() * 0.3,
    }
}

/// Vanilla `GameRenderer.shouldRenderBlockOutline` and
/// `LevelRenderer.renderHitOutline`'s shape: the targeted block's outline
/// (`getShape` in the player's context), unless the HUD is hidden or a
/// spectator looks at anything but a container.
// TODO: adventure mode's `canBreak`/`canPlaceOn` check.
fn hit_outline(
    game: &GameState,
    selected_slot: usize,
) -> Option<(
    glam::DVec3,
    &'static [crate::physics::block_shape::LocalBox],
)> {
    let Some(HitResult::Block(hit)) = game.interaction.target else {
        return None;
    };
    if game.hide_gui {
        return None;
    }
    let pos = hit.block_pos;
    let data = game
        .block_registry
        .block_state_data(game.chunk_store.get_block_state(pos.x, pos.y, pos.z));
    if data.is_air() {
        return None;
    }
    if crate::player::is_spectator(game.player.game_mode)
        && !crate::world::block_entity::is_block_entity_block(data.id())
    {
        return None;
    }
    let held = game.player.inventory.hotbar_slots()[selected_slot]
        .as_present()
        .map(|stack| crate::player::inventory::item_resource_name(stack.kind));
    let boxes = crate::physics::block_shape::outline_shape_holding_data(data, held.as_deref());
    Some((data.outline_shape_position(pos.x, pos.y, pos.z), boxes))
}

/// The end portal and gateway blocks to draw, with the faces each shows: a
/// gateway's where its neighbour doesn't hide them (`Block.shouldRenderFace`).
fn end_portal_blocks(
    game: &GameState,
) -> Vec<crate::renderer::pipelines::end_portal::EndPortalBlock> {
    use crate::renderer::pipelines::end_portal::EndPortalBlock;
    const NEIGHBOURS: [[i32; 3]; 6] = [
        [0, -1, 0],
        [0, 1, 0],
        [0, 0, -1],
        [0, 0, 1],
        [-1, 0, 0],
        [1, 0, 0],
    ];
    let block_at = |x, y, z| {
        game.block_registry
            .block_state_data(game.chunk_store.get_block_state(x, y, z))
    };
    game.end_portals
        .iter()
        .filter_map(|pos| {
            let gateway = match block_at(pos.x, pos.y, pos.z).id() {
                "end_portal" => false,
                "end_gateway" => true,
                _ => return None,
            };
            let faces = if gateway {
                NEIGHBOURS
                    .iter()
                    .enumerate()
                    .filter(|(_, [dx, dy, dz])| {
                        !block_at(pos.x + dx, pos.y + dy, pos.z + dz).occludes_neighbor_faces()
                    })
                    .fold(0, |faces, (i, _)| faces | 1 << i)
            } else {
                EndPortalBlock::PORTAL_FACES
            };
            Some(EndPortalBlock {
                pos: *pos,
                gateway,
                faces,
            })
        })
        .collect()
}

/// The render distance in chunks: the client's, capped by the server's.
fn effective_render_distance(core: &AppCore, game: &GameState) -> u32 {
    if game.server_render_distance > 0 {
        core.menu.render_distance.min(game.server_render_distance)
    } else {
        core.menu.render_distance
    }
}

/// Vanilla `Minecraft.runTick`'s cap on the ticks one frame catches up.
const MAX_TICKS_PER_FRAME: u32 = 10;

/// The ticks a frame of `frame_secs` runs, as vanilla `DeltaTracker.Timer`
/// counts them: the frame's whole time joins the leftover fraction, every
/// whole tick in it is taken out, and `Minecraft.runTick` runs at most
/// [`MAX_TICKS_PER_FRAME`] of them (`Math.min(10, i)`), dropping the rest.
fn advance_ticks(accumulator: &mut f32, frame_secs: f32) -> u32 {
    *accumulator += frame_secs.max(0.0);
    let due = (*accumulator / TICK_RATE).floor();
    *accumulator -= due * TICK_RATE;
    (due as u32).min(MAX_TICKS_PER_FRAME)
}

/// How far the clock is between its last tick and the next.
fn sky_partial_tick(core: &AppCore) -> f32 {
    (core.time_tick_accumulator / TICK_RATE).clamp(0.0, 1.0)
}

/// Vanilla `LightmapRenderStateExtractor.extract`, which samples the
/// attribute probe at the frame's partial tick.
/// TODO: boss fog and the end flashes.
fn lightmap_info(game: &GameState, brightness: f32, partial_tick: f32) -> LightmapInfo {
    let attributes = sky_flash(game, game.environment_probe.at(partial_tick));
    let with = |rgb: glam::Vec3, w: f32| rgb.extend(w).to_array();
    // Darkness (at the default darkness-effect scale of 1): the gamma it
    // takes off the brightness, and its pulse.
    let darkness_gamma = game.player.effects.darkness_factor(partial_tick);
    LightmapInfo {
        block_light_tint: with(attributes.block_light_tint, game.block_light_flicker + 1.4),
        sky_light_color: with(attributes.sky_light_color, attributes.sky_light_factor),
        ambient_color: with(
            attributes.ambient_light_color,
            (brightness - darkness_gamma).max(0.0),
        ),
        night_vision_color: with(
            attributes.night_vision_color,
            night_vision_scale(game.player.effects.duration("night_vision"), partial_tick),
        ),
        darkness_scale: darkness_scale(game.sky_state.game_time, darkness_gamma, partial_tick),
        ..bytemuck::Zeroable::zeroed()
    }
}

/// The client's lightning layer over the environment attributes
/// (`ClientLevel`'s sky flash): while a bolt lights the sky, the sky colour
/// lifts toward white and sky light is at full strength.
fn sky_flash(
    game: &GameState,
    mut attributes: crate::world::environment::EnvAttributes,
) -> crate::world::environment::EnvAttributes {
    if game.sky_flash_time > 0 {
        attributes.sky_color = attributes.sky_color.lerp(glam::Vec3::ONE, 0.22);
        attributes.sky_light_factor = 1.0;
    }
    attributes
}

/// Vanilla `calculateDarknessScale`: Darkness's pulse, four seconds a beat
/// (timed off the game clock rather than the player's age).
fn darkness_scale(game_time: u64, gamma: f32, partial_tick: f32) -> f32 {
    let t = (game_time % 80) as f32 - partial_tick;
    ((t * std::f32::consts::PI * 0.025).cos() * 0.45 * gamma).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_TICKS_PER_FRAME, MeshGate, advance_ticks, detached_distance, has_red_overlay,
        night_vision_scale, scroll_wheel, section_bit, section_bits, tick_block_light_flicker,
    };
    use crate::app::TICK_RATE;

    #[test]
    fn ten_fps_runs_two_ticks_a_frame_and_keeps_the_fraction() {
        let mut accumulator = 0.0;
        let frames: Vec<u32> = (0..10)
            .map(|_| advance_ticks(&mut accumulator, 0.1))
            .collect();
        assert_eq!(frames, [2; 10]);
        assert!(accumulator.abs() < 1e-4);

        // A 130 ms frame runs its two ticks and carries 30 ms on.
        let mut accumulator = 0.0;
        assert_eq!(advance_ticks(&mut accumulator, 0.13), 2);
        assert!((accumulator - 0.03).abs() < 1e-4);
        assert_eq!(advance_ticks(&mut accumulator, 0.025), 1);
    }

    #[test]
    fn a_slow_frame_catches_up_at_most_ten_ticks() {
        // A 300 ms hitch is six ticks; a clamped frame time ran only two.
        let mut accumulator = 0.0;
        assert_eq!(advance_ticks(&mut accumulator, 0.3), 6);

        // Vanilla drops whatever is past ten, keeping only the fraction.
        let mut accumulator = 0.0;
        assert_eq!(
            advance_ticks(&mut accumulator, 2.0 + TICK_RATE / 2.0),
            MAX_TICKS_PER_FRAME
        );
        assert!((accumulator - TICK_RATE / 2.0).abs() < 1e-4);
        assert_eq!(advance_ticks(&mut accumulator, 0.0), 0);
    }

    #[test]
    fn an_unspent_tick_from_the_loading_screen_runs_in_the_first_frame() {
        let mut accumulator = TICK_RATE;
        assert_eq!(advance_ticks(&mut accumulator, 0.001), 1);
    }

    #[test]
    fn mesh_gate_counts_masks_and_takes_them_back() {
        let mut gate = MeshGate::default();
        gate.add(0b1011, 24);
        gate.add(0, 24);
        assert_eq!(
            gate,
            MeshGate {
                visible: 3,
                hidden: 45
            }
        );
        gate.remove(0b1011, 24);
        assert_eq!(
            gate,
            MeshGate {
                visible: 0,
                hidden: 24
            }
        );
    }

    #[test]
    fn night_vision_is_full_until_its_last_ten_seconds() {
        assert_eq!(night_vision_scale(None, 0.0), 0.0);
        assert_eq!(night_vision_scale(Some(-1), 0.0), 1.0);
        assert_eq!(night_vision_scale(Some(201), 0.5), 1.0);
        let flicker = night_vision_scale(Some(100), 0.25);
        assert!((0.4..=1.0).contains(&flicker), "{flicker}");
    }

    #[test]
    fn block_light_flicker_stays_within_its_steady_bound() {
        // Each step adds under 0.1 before the 0.9 decay, so it never passes 0.9.
        let mut flicker = 0.0;
        for _ in 0..100_000 {
            flicker = tick_block_light_flicker(flicker);
            assert!(flicker.abs() < 0.9, "{flicker}");
        }
    }

    #[test]
    fn third_person_distance_takes_the_further_of_player_and_mount() {
        assert_eq!(detached_distance(4.0, None), 4.0);
        assert_eq!(detached_distance(4.0, Some(8.0)), 8.0);
        assert_eq!(detached_distance(10.0, Some(8.0)), 10.0);
    }

    #[test]
    fn scroll_wheel_matches_scroll_wheel_handler() {
        let mut acc = (0.0, 0.0);
        // Fractions accumulate into whole steps; vertical wins over horizontal.
        assert_eq!(scroll_wheel(&mut acc, (0.0, 0.6)), 0);
        assert_eq!(scroll_wheel(&mut acc, (0.0, 0.6)), 1);
        // A sign flip drops the leftover, and horizontal steps are negated.
        assert_eq!(scroll_wheel(&mut acc, (0.0, -1.0)), -1);
        assert_eq!(scroll_wheel(&mut acc, (1.0, 0.0)), -1);
        // `Math.signum(0)` is 0, so an idle axis clears its leftover.
        let mut acc = (0.5, 0.0);
        assert_eq!(scroll_wheel(&mut acc, (0.0, 0.0)), 0);
        assert_eq!(acc, (0.0, 0.0));
    }

    #[test]
    fn section_bits_cover_the_indices_and_ignore_the_rest() {
        assert_eq!(section_bits(0..3), 0b111);
        assert_eq!(section_bits(0..0), 0);
        assert_eq!(section_bits([2, 5]), 0b100100);
        // A camera outside build height resolves to a section index no column
        // has; it must read as "not compiled", not shift out of range.
        assert_eq!(section_bit(-1), 0);
        assert_eq!(section_bit(32), 0);
        assert_eq!(section_bits(-3..-2), 0);
    }

    #[test]
    fn red_overlay_matches_vanilla_hurt_and_death_timers() {
        assert!(has_red_overlay(1, 0));
        assert!(has_red_overlay(0, 1));
        assert!(!has_red_overlay(0, 0));
    }
}

/// Retain overlay releases until all debug modifiers are up; `None` defers
/// settlement.
fn debug_overlay_release(pending: &mut bool, overlay: bool, modifier_held: bool) -> Option<bool> {
    *pending |= overlay;
    if modifier_held {
        None
    } else {
        Some(std::mem::take(pending))
    }
}

#[cfg(test)]
mod debug_release_tests {
    use winit::keyboard::KeyCode;

    use super::*;
    use crate::app::input::{InputState, KeyBindings};

    #[test]
    fn overlay_release_survives_both_modifier_release_orders() {
        let f3 = InputKey::Keyboard(KeyCode::F3);
        let f6 = InputKey::Keyboard(KeyCode::F6);
        let mut bindings = KeyBindings::default();
        bindings.set(Action::DebugModifier, vec![f3, f6]);
        for order in [[KeyCode::F3, KeyCode::F6], [KeyCode::F6, KeyCode::F3]] {
            let mut input = InputState::released();
            input.set_key_pressed_for_test(KeyCode::F3, true);
            input.set_key_pressed_for_test(KeyCode::F6, true);
            let mut pending = false;
            for (index, code) in order.into_iter().enumerate() {
                input.set_key_pressed_for_test(code, false);
                let result = debug_overlay_release(
                    &mut pending,
                    bindings.matches_key(Action::ToggleOverlay, code),
                    bindings
                        .inputs_for_action(Action::DebugModifier)
                        .any(|key| input.input_pressed(key)),
                );
                assert_eq!(result, if index == 0 { None } else { Some(true) });
            }
            assert!(!pending);
            assert_eq!(
                debug_overlay_release(&mut pending, false, false),
                Some(false)
            );
        }
    }
}
