use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use azalea_block::BlockState;
use azalea_inventory::ItemStackData;
use azalea_inventory::components::{
    ChargedProjectiles, CustomModelData, DyedColor, FireworkExplosion, ItemModel, MapColor,
    PotionContents,
};
use azalea_registry::Registry as AzaleaRegistry;
use azalea_registry::builtin::{ItemKind, MobEffect, Potion};
use serde::{Deserialize, Serialize};

const BLOCK_CACHE_FILE: &str = "block_cache_v4.json";

use super::block_state_table::BlockStateTable;
use super::model;
use super::model::BakedModel;
use crate::assets::AssetIndex;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tint {
    None,
    Grass,
    /// Tall grass and large fern sample the lower block's biome for their upper
    /// half.
    DoubleGrass,
    Foliage,
    DryFoliage,
    Water,
    /// Fixed vanilla block tint color (`0xRRGGBB`).
    Constant(u32),
    /// Power-level color, resolved at mesh time from the state's `power`.
    Redstone,
    /// Age-dependent melon/pumpkin stem color.
    Stem,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct FaceTextures {
    pub top: String,
    pub bottom: String,
    pub north: String,
    pub south: String,
    pub east: String,
    pub west: String,
    pub side_overlay: Option<String>,
    pub tint: Tint,
    /// The model's `particle` texture slot (vanilla `getParticleMaterial`),
    /// used for block-break particles.
    #[serde(default)]
    pub particle: Option<String>,
}

impl FaceTextures {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        top: &str,
        bottom: &str,
        north: &str,
        south: &str,
        east: &str,
        west: &str,
        side_overlay: Option<&str>,
        tint: Tint,
    ) -> Self {
        Self {
            top: top.into(),
            bottom: bottom.into(),
            north: north.into(),
            south: south.into(),
            east: east.into(),
            west: west.into(),
            side_overlay: side_overlay.map(Into::into),
            tint,
            particle: None,
        }
    }

    pub fn uniform(name: &str, tint: Tint) -> Self {
        Self::new(name, name, name, name, name, name, None, tint)
    }
}

/// A block state's model lookups, resolved once per state.
struct StateRender {
    model: Option<Arc<BakedModel>>,
    opaque_full_cube: bool,
    occludes: bool,
    hidden_when_enclosed: bool,
}

#[derive(Default)]
pub struct BlockRenderAssets {
    textures: HashMap<String, FaceTextures>,
    baked: HashMap<String, HashMap<String, Arc<BakedModel>>>,
    /// Per block-state table slot, every state's resolved model lookups,
    /// indexed by state id; built on first use of the slot. The mesher queries
    /// these for every block and neighbour, so they must not hash or parse.
    state_render: [OnceLock<Box<[StateRender]>>; super::TABLE_SLOTS],
    multipart: HashMap<String, Vec<model::MultipartEntry>>,
    item_models: HashMap<String, BakedModel>,
    flat_item_textures: std::collections::HashSet<String>,
    flat_item_texture_keys: HashMap<String, Vec<Option<String>>>,
    item_gui_transforms: HashMap<String, model::DisplayTransform>,
    item_ground_transforms: HashMap<String, glam::Mat4>,
    item_display_transforms: HashMap<String, model::DisplayTransforms>,
    item_tint_sources: HashMap<String, Vec<model::ItemTintSource>>,
    /// Item models with a banner's flag, which take a stack's patterns
    /// ([`crate::banner`]).
    banner_models: std::collections::HashSet<String>,
    /// Item models drawn as a shield (vanilla's plate in its no-pattern
    /// texture), whose stacks draw their base colour and patterns.
    shield_models: std::collections::HashSet<String>,
    /// Item models drawn as a player head, whose stacks draw their own skin
    /// in the GUI.
    player_head_models: std::collections::HashSet<String>,
    /// Every banner pattern texture in the jar and the packs, for the atlas
    /// (stacks name them, no model does).
    banner_pattern_textures: Vec<String>,
}

/// Lightweight view of CPU assets paired with the session's explicitly selected
/// state table.
#[derive(Clone, Copy)]
pub(crate) struct BlockRenderView<'a> {
    assets: &'a BlockRenderAssets,
    block_state_table: BlockStateTable,
}

/// Owned, lightweight view of CPU-asset and state-table snapshot for deferred
/// rendering work. Cloning shares the assets; queries borrow the existing
/// render view.
#[derive(Clone)]
pub(crate) struct OwnedBlockRenderView {
    assets: Arc<BlockRenderAssets>,
    block_state_table: BlockStateTable,
}

impl OwnedBlockRenderView {
    pub(crate) fn block_state_table(&self) -> BlockStateTable {
        self.block_state_table
    }

    pub(crate) fn new(assets: Arc<BlockRenderAssets>, block_state_table: BlockStateTable) -> Self {
        Self {
            assets,
            block_state_table,
        }
    }

    pub(crate) fn as_view(&self) -> BlockRenderView<'_> {
        self.assets.for_version(self.block_state_table)
    }
}

impl BlockRenderAssets {
    /// The cached block textures, else each block's resolved from the assets
    /// and packs (cached for next time when no pack applies).
    fn block_textures(
        textures: Option<HashMap<String, FaceTextures>>,
        cache_path: Option<&Path>,
        jar_assets_dir: &Path,
        asset_index: &Option<AssetIndex>,
        packs: Option<&crate::resource_pack::ResourcePackManager>,
    ) -> HashMap<String, FaceTextures> {
        textures.unwrap_or_else(|| {
            let mut textures = model::load_all_block_textures(jar_assets_dir, asset_index, packs);

            textures
                .entry("water".into())
                .or_insert_with(|| FaceTextures::uniform("water_still", Tint::None));
            textures
                .entry("lava".into())
                .or_insert_with(|| FaceTextures::uniform("lava_still", Tint::None));

            if let Some(path) = cache_path {
                save_cache(path, &textures);
            }
            tracing::info!("Block render assets: {} blocks (built)", textures.len());
            textures
        })
    }

    pub fn load(
        jar_assets_dir: &Path,
        asset_index: &Option<AssetIndex>,
        cache_dir: Option<&Path>,
        packs: Option<&crate::resource_pack::ResourcePackManager>,
    ) -> Self {
        // Only vanilla textures are cached, so a cache shared between
        // profiles never carries one profile's resource packs into another.
        let cache_path = cache_dir
            .filter(|_| packs.is_none())
            .map(|dir| dir.join(BLOCK_CACHE_FILE));

        let textures = cache_path.as_deref().and_then(load_cache);
        if let Some(cached) = &textures {
            tracing::info!(
                "Block render assets: {} blocks (cached textures)",
                cached.len()
            );
        }

        // The block textures, block models and item models don't depend on
        // each other; with a pack applied each takes a second or more, so
        // the models bake on their own threads meanwhile.
        let (textures, (baked, multipart), baked_items) = std::thread::scope(|scope| {
            let blocks = scope.spawn(|| model::bake_all_models(jar_assets_dir, asset_index, packs));
            let items = scope.spawn(|| model::bake_item_models(jar_assets_dir, asset_index, packs));
            let textures = Self::block_textures(
                textures,
                cache_path.as_deref(),
                jar_assets_dir,
                asset_index,
                packs,
            );
            (
                textures,
                blocks.join().expect("block models bake"),
                items.join().expect("item models bake"),
            )
        });
        let item_models = baked_items.models;
        let flat_item_textures = baked_items.generated_textures;
        let flat_item_texture_keys = baked_items.flat_texture_keys;
        let item_gui_transforms = baked_items.gui_transforms;
        let item_ground_transforms = baked_items.ground_transforms;
        let item_display_transforms = baked_items.display_transforms;
        let item_tint_sources = baked_items.tint_sources;
        let base_pattern = model::TextureId::intern(model::BANNER_BASE_PATTERN);
        let banner_models = item_models
            .iter()
            .filter(|(_, model)| model.quads.iter().any(|quad| quad.texture == base_pattern))
            .map(|(key, _)| key.clone())
            .collect();
        let banner_pattern_textures = banner_pattern_textures(jar_assets_dir, packs);
        let shield_models = item_models
            .iter()
            .filter(|(_, model)| {
                model
                    .quads
                    .iter()
                    .any(|quad| is_shield_plate_texture(quad.texture))
            })
            .map(|(key, _)| key.clone())
            .collect();
        let player_head = model::TextureId::intern(model::PLAYER_HEAD_TEXTURE);
        let player_head_models = item_models
            .iter()
            .filter(|(_, model)| model.quads.iter().any(|quad| quad.texture == player_head))
            .map(|(key, _)| key.clone())
            .collect();

        Self {
            textures,
            baked,
            state_render: [const { OnceLock::new() }; super::TABLE_SLOTS],
            multipart,
            item_models,
            flat_item_textures,
            flat_item_texture_keys,
            item_gui_transforms,
            item_ground_transforms,
            item_display_transforms,
            item_tint_sources,
            banner_models,
            shield_models,
            player_head_models,
            banner_pattern_textures,
        }
    }

    pub(crate) fn for_version(&self, block_state_table: BlockStateTable) -> BlockRenderView<'_> {
        BlockRenderView {
            assets: self,
            block_state_table,
        }
    }

    pub fn get_item_model(&self, name: &str) -> Option<&BakedModel> {
        self.item_models.get(name)
    }

    /// The model `name` draws with for this stack: the definition its
    /// `item_model` component names, then that definition's variant for the
    /// stack's first custom-model-data string or its loaded charge (vanilla
    /// `CustomModelDataProperty` and `Charge` selects), if one is baked.
    /// The model `key` draws in the GUI: its GUI variant where its
    /// definition has one ([`model::gui_model_key`]), else itself.
    /// `model`'s variant for a stack in use ([`model::using_model_key`]),
    /// or `model` itself where nothing changes while it's used.
    pub fn in_use_model_key(&self, model: &str) -> String {
        // A stack's pattern layers stay on its in-use variant.
        let (base, layers) = model.split_at(model.find('~').unwrap_or(model.len()));
        let key = model::using_model_key(base);
        if self.item_models.contains_key(&key) {
            key + layers
        } else {
            model.to_owned()
        }
    }

    pub fn is_player_head_model(&self, key: &str) -> bool {
        self.player_head_models.contains(key)
    }

    pub fn gui_model_key(&self, key: &str) -> String {
        let (key, layers) = key.split_at(key.find('~').unwrap_or(key.len()));
        let gui = model::gui_model_key(key);
        if !layers.is_empty() {
            let base = if self.banner_models.contains(&gui) {
                gui
            } else {
                key.to_owned()
            };
            return base + layers;
        }
        if self.item_models.contains_key(&gui) || self.flat_item_texture_keys.contains_key(&gui) {
            gui
        } else {
            key.to_owned()
        }
    }

    /// [`Self::stack_model_key_base`], with the stack's banner pattern
    /// layers on a banner's model (`~` and the layers, [`crate::banner`]).
    pub fn stack_model_key(&self, name: &str, stack: &ItemStackData) -> String {
        let key = self.stack_model_key_base(name, stack);
        if self.banner_models.contains(&key)
            && let Some(layers) = stack
                .get_component::<azalea_inventory::components::BannerPatterns>()
                .and_then(|patterns| crate::banner::layers_suffix(&patterns))
        {
            return key + &layers;
        }
        if self.shield_models.contains(&key)
            && let Some(layers) = crate::banner::shield_layers_suffix(
                stack
                    .get_component::<azalea_inventory::components::BaseColor>()
                    .map(|base| base.color as i32),
                stack
                    .get_component::<azalea_inventory::components::BannerPatterns>()
                    .as_deref(),
            )
        {
            return key + &layers;
        }
        key
    }

    fn stack_model_key_base(&self, name: &str, stack: &ItemStackData) -> String {
        let has = |key: &str| {
            self.item_models.contains_key(key) || self.flat_item_texture_keys.contains_key(key)
        };
        let named = stack
            .get_component::<ItemModel>()
            .map(|model| model::item_model_name(&model.resource_location.to_string()))
            .filter(|key| has(key));
        let name = named.as_deref().unwrap_or(name);
        let custom = stack
            .get_component::<CustomModelData>()
            .and_then(|data| {
                let when = data.strings.first()?;
                Some(model::custom_model_key(name, when))
            })
            .filter(|key| has(key));
        if let Some(key) = custom {
            return key;
        }
        let charge = stack
            .get_component::<ChargedProjectiles>()
            .filter(|c| !c.items.is_empty())
            .map(|c| {
                let rocket = c.items.iter().any(|i| i.kind() == ItemKind::FireworkRocket);
                if rocket { "rocket" } else { "arrow" }
            });
        charge
            .map(|charge| model::charge_model_key(name, charge))
            .filter(|key| has(key))
            .unwrap_or_else(|| name.to_string())
    }

    /// Every item with a baked 3D model or a generated flat sprite.
    pub fn item_names(&self) -> impl Iterator<Item = &str> + '_ {
        self.item_models
            .keys()
            .chain(self.flat_item_texture_keys.keys())
            .map(String::as_str)
    }

    pub fn flat_item_textures(&self) -> impl Iterator<Item = &str> + '_ {
        self.flat_item_textures.iter().map(String::as_str)
    }

    /// A flat item's layer textures indexed by layer (`None` = unresolvable),
    /// defaulting to the item's own sprite.
    pub fn flat_item_layers(&self, name: &str) -> Vec<Option<String>> {
        let name = crate::banner::base_key(name);
        self.flat_item_texture_keys
            .get(name)
            .cloned()
            .unwrap_or_else(|| vec![Some(format!("item/{name}"))])
    }

    /// Evaluate the selected item model's tint-source list for this stack.
    /// Vanilla keeps the full list, and model-face `tintindex` values address
    /// it directly, so resource-pack entries must not be truncated or shifted.
    pub fn item_tint_palette(
        &self,
        name: &str,
        stack: Option<&ItemStackData>,
        team_color: Option<u32>,
    ) -> Vec<u32> {
        let mut palette: Vec<u32> = self
            .item_tint_sources
            .get(crate::banner::base_key(name))
            .map(|sources| {
                sources
                    .iter()
                    .map(|source| evaluate_item_tint(source, stack, team_color))
                    .collect()
            })
            .unwrap_or_default();
        // A banner stack's pattern layers take the tints after the model's.
        palette.extend(crate::banner::layers(name).map(|(_, color)| 0xFF00_0000 | color));
        palette
    }

    pub(crate) fn get_item_gui_transform(&self, name: &str) -> model::DisplayTransform {
        self.item_gui_transforms
            .get(crate::banner::base_key(name))
            .copied()
            .unwrap_or(model::DisplayTransform::IDENTITY)
    }

    pub fn get_item_ground_transform(&self, name: &str) -> Option<glam::Mat4> {
        self.item_ground_transforms
            .get(crate::banner::base_key(name))
            .copied()
    }

    /// A baked item model's own transform for display context `context`,
    /// packs included; `None` where its model chain has none.
    pub(crate) fn baked_display_transform(
        &self,
        name: &str,
        context: u8,
    ) -> Option<model::DisplayTransform> {
        self.item_display_transforms
            .get(crate::banner::base_key(name))?
            .get(context as usize)
            .copied()
            .flatten()
    }

    /// A baked item model's transform for display context `context`
    /// ([`model::DISPLAY_CONTEXT_KEYS`]); `None` for models it didn't bake.
    pub(crate) fn get_item_display_transform(
        &self,
        name: &str,
        context: u8,
    ) -> Option<model::DisplayTransform> {
        let display = self
            .item_display_transforms
            .get(crate::banner::base_key(name))?;
        Some(
            display
                .get(context as usize)
                .copied()
                .flatten()
                .unwrap_or(model::DisplayTransform::IDENTITY),
        )
    }

    fn state_render(
        &self,
        block_state_table: BlockStateTable,
        state: BlockState,
    ) -> Option<&StateRender> {
        let slot = block_state_table.table_slot();
        self.state_render[slot]
            .get_or_init(|| {
                block_state_table
                    .table()
                    .iter()
                    .map(|block_state_data| {
                        let model = self
                            .resolve_model(block_state_data.id, &block_state_data.properties)
                            .cloned();
                        // TODO: the occlusion graph reads this for vanilla's
                        // `isSolidRender` (`canOcclude` and a full occlusion shape),
                        // but it still counts the model's quads and ignores
                        // `canOcclude`, so glass counts as solid there.
                        let opaque_full_cube = !block_state_data.is_air
                            && model.as_ref().is_some_and(|m| m.is_full_cube);
                        let occludes = block_state_data.occludes_neighbor_faces();
                        let all_quads_cull = model
                            .as_ref()
                            .is_some_and(|m| m.quads.iter().all(|q| q.cullface.is_some()));
                        StateRender {
                            model,
                            opaque_full_cube,
                            occludes,
                            hidden_when_enclosed: occludes
                                && all_quads_cull
                                && matches!(block_state_data.fluid.kind, super::FluidKind::Empty),
                        }
                    })
                    .collect()
            })
            .get(u32::from(state) as usize)
    }

    fn resolve_model(&self, block: &str, props: &super::PropMap) -> Option<&Arc<BakedModel>> {
        let variants = self.baked.get(block)?;

        if variants.len() == 1 {
            return variants.values().next();
        }

        // Vanilla variant keys only list the properties that affect the model, so
        // match by subset rather than exact string equality (an empty key matches
        // any state, serving as the default variant).
        variants
            .iter()
            .find(|(key, _)| {
                constraints_match(props, key.split(',').filter_map(|p| p.split_once('=')))
            })
            .map(|(_, model)| model)
            .or_else(|| variants.values().next())
    }

    pub fn texture_names(&self) -> impl Iterator<Item = &str> + '_ {
        let face_textures = self.textures.values().flat_map(|ft| {
            let base = [
                &ft.top, &ft.bottom, &ft.north, &ft.south, &ft.east, &ft.west,
            ];
            base.into_iter()
                .map(|s| s.as_str())
                .chain(ft.side_overlay.as_deref())
        });

        let models = self
            .baked
            .values()
            .flat_map(|variants| variants.values().map(|model| &**model))
            .chain(
                self.multipart
                    .values()
                    .flat_map(|entries| entries.iter().map(|entry| &*entry.model)),
            )
            .chain(self.item_models.values());
        let quad_textures =
            models.flat_map(|model| model.quads.iter().map(|quad| quad.texture.name()));

        face_textures
            .chain(quad_textures)
            .chain(self.banner_pattern_textures.iter().map(String::as_str))
    }
}

/// Every `textures/entity/banner/*.png` in the jar and the packs, as texture
/// names (`entity/banner/border`, or namespaced outside `minecraft`).
/// A shield's plate texture without patterns (vanilla
/// `NO_PATTERN_SHIELD`, in its 26.1 folder or the older place).
pub(crate) fn is_shield_plate_texture(texture: model::TextureId) -> bool {
    texture.name().ends_with("shield_base_nopattern")
}

fn banner_pattern_textures(
    jar_assets_dir: &Path,
    packs: Option<&crate::resource_pack::ResourcePackManager>,
) -> Vec<String> {
    let mut names = std::collections::BTreeSet::new();
    let roots = std::iter::once(jar_assets_dir.to_path_buf()).chain(
        packs
            .into_iter()
            .flat_map(|packs| packs.active_pack_dirs())
            .map(|pack| pack.join("assets")),
    );
    for root in roots {
        let Ok(namespaces) = std::fs::read_dir(&root) else {
            continue;
        };
        for namespace in namespaces.flatten() {
            let ns = namespace.file_name().to_string_lossy().into_owned();
            // A patterned shield's base before 26.1 moved it into
            // `entity/shield/`.
            if ns == "minecraft"
                && namespace
                    .path()
                    .join("textures/entity/shield_base.png")
                    .is_file()
            {
                names.insert("entity/shield_base".to_owned());
            }
            // Banners' pattern textures and shields' own.
            for folder in ["banner", "shield"] {
                let dir = namespace.path().join("textures/entity").join(folder);
                let Ok(files) = std::fs::read_dir(dir) else {
                    continue;
                };
                for file in files.flatten() {
                    let file = file.file_name().to_string_lossy().into_owned();
                    let Some(stem) = file.strip_suffix(".png") else {
                        continue;
                    };
                    names.insert(if ns == "minecraft" {
                        format!("entity/{folder}/{stem}")
                    } else {
                        format!("{ns}:entity/{folder}/{stem}")
                    });
                }
            }
        }
    }
    names.into_iter().collect()
}

impl<'a> BlockRenderView<'a> {
    pub(crate) fn block_state_table(self) -> BlockStateTable {
        self.block_state_table
    }

    pub(crate) fn get_textures(self, state: BlockState) -> Option<&'a FaceTextures> {
        self.assets
            .textures
            .get(self.block_state_table.block_state_data(state).id)
    }

    pub(crate) fn get_baked_model(self, state: BlockState) -> Option<&'a BakedModel> {
        self.assets
            .state_render(self.block_state_table, state)?
            .model
            .as_deref()
    }

    pub(crate) fn get_multipart_quads(
        self,
        state: BlockState,
    ) -> Option<Vec<&'a model::BakedQuad>> {
        let block_state_data = self.block_state_table.block_state_data(state);
        let entries = self.assets.multipart.get(block_state_data.id)?;
        let mut quads = Vec::new();
        for entry in entries {
            let when = entry.when.iter().map(|(k, v)| (k.as_str(), v.as_str()));
            if constraints_match(&block_state_data.properties, when) {
                quads.extend(entry.model.quads.iter());
            }
        }
        if quads.is_empty() { None } else { Some(quads) }
    }

    pub(crate) fn is_opaque_full_cube(self, state: BlockState) -> bool {
        self.assets
            .state_render(self.block_state_table, state)
            .is_some_and(|r| r.opaque_full_cube)
    }

    /// Full cubes that can occlude, excluding glass and other no-occlusion
    /// cubes.
    pub(crate) fn occludes_neighbor(self, state: BlockState) -> bool {
        self.assets
            .state_render(self.block_state_table, state)
            .is_some_and(|r| r.occludes)
    }

    /// Non-fluid occluders whose every quad has a cullface.
    pub(crate) fn hidden_when_enclosed(self, state: BlockState) -> bool {
        self.assets
            .state_render(self.block_state_table, state)
            .is_some_and(|r| r.hidden_when_enclosed)
    }
}

fn opaque_rgb(color: u32) -> u32 {
    color | 0xFF00_0000
}

fn evaluate_item_tint(
    source: &model::ItemTintSource,
    stack: Option<&ItemStackData>,
    team_color: Option<u32>,
) -> u32 {
    match *source {
        model::ItemTintSource::Constant(color) | model::ItemTintSource::Grass { color } => color,
        // Vanilla preserves the configured default alpha for dye/firework, but
        // opacifies component colors.
        model::ItemTintSource::Dye { default } => stack
            .and_then(|stack| stack.get_component::<DyedColor>())
            .map(|color| opaque_rgb(color.rgb as u32))
            .unwrap_or(default),
        model::ItemTintSource::Firework { default } => stack
            .and_then(|stack| stack.get_component::<FireworkExplosion>())
            .and_then(|explosion| average_rgb(&explosion.colors))
            .unwrap_or(default),
        model::ItemTintSource::Potion { default } => stack
            .and_then(|stack| stack.get_component::<PotionContents>())
            .map(|contents| potion_contents_color(&contents, default))
            .unwrap_or_else(|| opaque_rgb(default)),
        model::ItemTintSource::MapColor { default } => stack
            .and_then(|stack| stack.get_component::<MapColor>())
            .map(|color| opaque_rgb(color.color as u32))
            .unwrap_or_else(|| opaque_rgb(default)),
        model::ItemTintSource::CustomModelData { index, default } => stack
            .and_then(|stack| stack.get_component::<CustomModelData>())
            .and_then(|data| data.colors.get(index).copied())
            .map(|color| opaque_rgb(color as u32))
            .unwrap_or_else(|| opaque_rgb(default)),
        model::ItemTintSource::Team { default } => opaque_rgb(team_color.unwrap_or(default)),
    }
}

fn average_rgb(colors: &[i32]) -> Option<u32> {
    if colors.is_empty() {
        return None;
    }
    if colors.len() == 1 {
        return Some(opaque_rgb(colors[0] as u32));
    }
    let mut red = 0_u32;
    let mut green = 0_u32;
    let mut blue = 0_u32;
    for &color in colors {
        let color = color as u32;
        red += (color >> 16) & 0xFF;
        green += (color >> 8) & 0xFF;
        blue += color & 0xFF;
    }
    let count = colors.len() as u32;
    Some(opaque_rgb(
        ((red / count) << 16) | ((green / count) << 8) | (blue / count),
    ))
}

fn potion_contents_color(contents: &PotionContents, default: u32) -> u32 {
    if let Some(color) = contents.custom_color {
        return opaque_rgb(color as u32);
    }

    let mut effects: Vec<(u32, u32)> = Vec::new();
    if let Some(potion) = contents.potion {
        potion_effect_colors(potion, &mut effects);
    }
    for effect in &contents.custom_effects {
        if effect.details.show_particles {
            effects.push((
                mob_effect_color(effect.id),
                (effect.details.amplifier.max(0) as u32) + 1,
            ));
        }
    }
    opaque_rgb(weighted_effect_color(&effects).unwrap_or(default))
}

fn weighted_effect_color(effects: &[(u32, u32)]) -> Option<u32> {
    let mut red = 0_u64;
    let mut green = 0_u64;
    let mut blue = 0_u64;
    let mut weight_sum = 0_u64;
    for &(color, weight) in effects {
        let weight = weight as u64;
        red += ((color >> 16) & 0xFF) as u64 * weight;
        green += ((color >> 8) & 0xFF) as u64 * weight;
        blue += (color & 0xFF) as u64 * weight;
        weight_sum += weight;
    }
    let red = red.checked_div(weight_sum)? as u32;
    let green = green.checked_div(weight_sum)? as u32;
    let blue = blue.checked_div(weight_sum)? as u32;
    Some((red << 16) | (green << 8) | blue)
}

fn potion_effect_colors(potion: Potion, out: &mut Vec<(u32, u32)>) {
    use Potion::*;
    let one = |out: &mut Vec<(u32, u32)>, effect, amplifier: u32| {
        out.push((mob_effect_color(effect), amplifier + 1));
    };
    match potion {
        Water | Mundane | Thick | Awkward => {}
        NightVision | LongNightVision => one(out, MobEffect::NightVision, 0),
        Invisibility | LongInvisibility => one(out, MobEffect::Invisibility, 0),
        Leaping | LongLeaping => one(out, MobEffect::JumpBoost, 0),
        StrongLeaping => one(out, MobEffect::JumpBoost, 1),
        FireResistance | LongFireResistance => one(out, MobEffect::FireResistance, 0),
        Swiftness | LongSwiftness => one(out, MobEffect::Speed, 0),
        StrongSwiftness => one(out, MobEffect::Speed, 1),
        Slowness | LongSlowness => one(out, MobEffect::Slowness, 0),
        StrongSlowness => one(out, MobEffect::Slowness, 3),
        TurtleMaster | LongTurtleMaster => {
            one(out, MobEffect::Slowness, 3);
            one(out, MobEffect::Resistance, 2);
        }
        StrongTurtleMaster => {
            one(out, MobEffect::Slowness, 5);
            one(out, MobEffect::Resistance, 3);
        }
        WaterBreathing | LongWaterBreathing => one(out, MobEffect::WaterBreathing, 0),
        Healing => one(out, MobEffect::InstantHealth, 0),
        StrongHealing => one(out, MobEffect::InstantHealth, 1),
        Harming => one(out, MobEffect::InstantDamage, 0),
        StrongHarming => one(out, MobEffect::InstantDamage, 1),
        Poison | LongPoison => one(out, MobEffect::Poison, 0),
        StrongPoison => one(out, MobEffect::Poison, 1),
        Regeneration | LongRegeneration => one(out, MobEffect::Regeneration, 0),
        StrongRegeneration => one(out, MobEffect::Regeneration, 1),
        Strength | LongStrength => one(out, MobEffect::Strength, 0),
        StrongStrength => one(out, MobEffect::Strength, 1),
        Weakness | LongWeakness => one(out, MobEffect::Weakness, 0),
        Luck => one(out, MobEffect::Luck, 0),
        SlowFalling | LongSlowFalling => one(out, MobEffect::SlowFalling, 0),
        WindCharged => one(out, MobEffect::WindCharged, 0),
        Weaving => one(out, MobEffect::Weaving, 0),
        Oozing => one(out, MobEffect::Oozing, 0),
        Infested => one(out, MobEffect::Infested, 0),
    }
}

fn mob_effect_color(effect: MobEffect) -> u32 {
    crate::mob_effect::info(effect.to_u32()).map_or(0xFFFFFF, |info| info.color)
}

/// Whether every `key=value` constraint holds for `props`. A value may list
/// alternatives separated by `|`, as vanilla multipart `when` clauses do.
fn constraints_match<'a>(
    props: &super::PropMap,
    mut constraints: impl Iterator<Item = (&'a str, &'a str)>,
) -> bool {
    constraints.all(|(k, v)| {
        props
            .get(k)
            .is_some_and(|pv| v.split('|').any(|opt| opt == pv))
    })
}

fn load_cache(path: &Path) -> Option<HashMap<String, FaceTextures>> {
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Atomic, since every client of a version shares the one cache.
fn save_cache(path: &Path, textures: &HashMap<String, FaceTextures>) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string(textures)
        && let Err(e) = crate::util::write_atomic(path, json.as_bytes())
    {
        tracing::warn!("Failed to write block cache: {e}");
    }
}

#[cfg(test)]
mod tint_tests {
    use azalea_inventory::ItemStack;
    use azalea_inventory::components::CustomModelData;

    use super::*;

    #[test]
    fn custom_model_data_tint_uses_requested_color_index_and_default() {
        let stack = ItemStack::from(ItemKind::Stone).with_component(CustomModelData {
            floats: vec![],
            flags: vec![],
            strings: vec![],
            colors: vec![0x112233, 0x445566, 0x778899],
        });
        let stack = stack.as_present().unwrap();

        assert_eq!(
            evaluate_item_tint(
                &model::ItemTintSource::CustomModelData {
                    index: 1,
                    default: 0xABCDEF,
                },
                Some(stack),
                None,
            ),
            0xFF445566
        );
        assert_eq!(
            evaluate_item_tint(
                &model::ItemTintSource::CustomModelData {
                    index: 9,
                    default: 0xABCDEF,
                },
                Some(stack),
                None,
            ),
            0xFFABCDEF
        );
    }

    #[test]
    fn team_tint_uses_owner_team_color_or_default() {
        let source = model::ItemTintSource::Team { default: 0x123456 };
        assert_eq!(
            evaluate_item_tint(&source, None, Some(0xABCDEF)),
            0xFFABCDEF
        );
        assert_eq!(evaluate_item_tint(&source, None, None), 0xFF123456);
    }

    #[test]
    fn source_specific_alpha_matches_vanilla_fallback_rules() {
        let translucent = 0x80112233;
        assert_eq!(
            evaluate_item_tint(
                &model::ItemTintSource::Dye {
                    default: translucent
                },
                None,
                None,
            ),
            translucent
        );
        assert_eq!(
            evaluate_item_tint(
                &model::ItemTintSource::Firework {
                    default: translucent
                },
                None,
                None,
            ),
            translucent
        );
        for source in [
            model::ItemTintSource::Potion {
                default: translucent,
            },
            model::ItemTintSource::MapColor {
                default: translucent,
            },
            model::ItemTintSource::CustomModelData {
                index: 0,
                default: translucent,
            },
            model::ItemTintSource::Team {
                default: translucent,
            },
        ] {
            assert_eq!(evaluate_item_tint(&source, None, None), 0xFF112233);
        }
    }
}
