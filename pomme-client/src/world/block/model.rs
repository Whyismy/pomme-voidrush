use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};

use glam::{Mat3, Mat4, Quat, Vec3};
use serde::Deserialize;

use super::render_assets::{FaceTextures, Tint};
use crate::assets::{AssetId, AssetIndex, resolve_asset_path_with_packs};
use crate::renderer::chunk::mesher::Colormap;
use crate::renderer::entity_model::exported_data;

#[derive(Deserialize)]
struct BlockstateFile {
    variants: Option<HashMap<String, VariantEntry>>,
    multipart: Option<Vec<MultipartCase>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum VariantEntry {
    Single(ModelRef),
    Array(Vec<ModelRef>),
}

impl VariantEntry {
    fn first(&self) -> Option<&ModelRef> {
        match self {
            VariantEntry::Single(r) => Some(r),
            VariantEntry::Array(arr) => arr.first(),
        }
    }
}

#[derive(Deserialize)]
struct MultipartCase {
    apply: MultipartApply,
    #[allow(dead_code)]
    when: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum MultipartApply {
    Single(ModelRef),
    Array(Vec<ModelRef>),
}

impl MultipartApply {
    fn first(&self) -> Option<&ModelRef> {
        match self {
            MultipartApply::Single(r) => Some(r),
            MultipartApply::Array(arr) => arr.first(),
        }
    }
}

#[derive(Deserialize)]
struct ModelRef {
    model: String,
    #[serde(default)]
    x: i32,
    #[serde(default)]
    y: i32,
    #[serde(default)]
    uvlock: bool,
}

#[derive(Deserialize, Default, Clone)]
struct ModelFile {
    parent: Option<String>,
    #[serde(default, deserialize_with = "deserialize_texture_map")]
    textures: HashMap<String, String>,
    #[serde(default)]
    elements: Vec<ElementDef>,
    #[serde(default)]
    display: HashMap<String, serde_json::Value>,
    gui_light: Option<GuiLight>,
    ambientocclusion: Option<bool>,
}

#[derive(Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum GuiLight {
    Front,
    #[default]
    Side,
}

/// The `display` keys each item display context reads, indexed as vanilla's
/// `ItemDisplayContext` ids (0 is `none`): the first a model in the parent
/// chain has, in order, since vanilla's `ItemTransforms` falls back from a
/// missing left-hand entry to the same model's right-hand one.
pub(crate) const DISPLAY_CONTEXT_KEYS: [&[&str]; 10] = [
    &[],
    &["thirdperson_lefthand", "thirdperson_righthand"],
    &["thirdperson_righthand"],
    &["firstperson_lefthand", "firstperson_righthand"],
    &["firstperson_righthand"],
    &["head"],
    &["gui"],
    &["ground"],
    &["fixed"],
    &["on_shelf"],
];

/// The models an item frame draws as its body, by key: `blockstates/
/// item_frame.json`'s and the glow frame's, empty then holding a map. They
/// aren't items or block states, so they bake beside the item models.
pub(crate) const ITEM_FRAME_MODELS: [(&str, &str); 4] = [
    ("pomme:item_frame", "block/item_frame"),
    ("pomme:item_frame_map", "block/item_frame_map"),
    ("pomme:glow_item_frame", "block/glow_item_frame"),
    ("pomme:glow_item_frame_map", "block/glow_item_frame_map"),
];

/// A model's transform per display context ([`DISPLAY_CONTEXT_KEYS`]).
pub(crate) type DisplayTransforms = [Option<DisplayTransform>; 10];

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DisplayTransform {
    pub(crate) rotation: Vec3,
    pub(crate) translation: Vec3,
    pub(crate) scale: Vec3,
}

impl DisplayTransform {
    pub(crate) const IDENTITY: Self = Self {
        rotation: Vec3::ZERO,
        translation: Vec3::ZERO,
        scale: Vec3::ONE,
    };

    /// Vanilla `ItemTransform.apply`'s left-hand fix: X translation and the
    /// Y and Z rotations mirrored.
    pub(crate) fn mirrored(self) -> Self {
        Self {
            rotation: self.rotation * Vec3::new(1.0, -1.0, -1.0),
            translation: self.translation * Vec3::new(-1.0, 1.0, 1.0),
            scale: self.scale,
        }
    }

    pub(crate) fn to_matrix(self) -> Mat4 {
        Mat4::from_translation(self.translation)
            * Mat4::from_rotation_x(self.rotation.x.to_radians())
            * Mat4::from_rotation_y(self.rotation.y.to_radians())
            * Mat4::from_rotation_z(self.rotation.z.to_radians())
            * Mat4::from_scale(self.scale)
    }
}

pub(crate) fn parse_display_transform(json: &serde_json::Value) -> Option<DisplayTransform> {
    let obj = json.as_object()?;
    let rotation = obj
        .get("rotation")
        .map(|value| parse_vec3(value, Vec3::ZERO))
        .unwrap_or(Vec3::ZERO);
    let translation = obj
        .get("translation")
        .map(|value| parse_vec3(value, Vec3::ZERO))
        .unwrap_or(Vec3::ZERO)
        * (1.0 / 16.0);
    let scale = obj
        .get("scale")
        .map(|value| parse_vec3(value, Vec3::ONE))
        .unwrap_or(Vec3::ONE);
    Some(DisplayTransform {
        rotation,
        translation: translation.clamp(Vec3::splat(-5.0), Vec3::splat(5.0)),
        scale: scale.clamp(Vec3::splat(-4.0), Vec3::splat(4.0)),
    })
}

/// The shape vanilla's `display.gui` entries share, which differ only in yaw:
/// `block/block` is 225, `item/template_chest` 45.
#[cfg(test)]
fn gui_transform(y_rot_deg: f32) -> DisplayTransform {
    DisplayTransform {
        rotation: Vec3::new(30.0, y_rot_deg, 0.0),
        translation: Vec3::ZERO,
        scale: Vec3::splat(0.625),
    }
}

pub(crate) fn default_block_ground_transform() -> Mat4 {
    DisplayTransform {
        rotation: Vec3::ZERO,
        translation: Vec3::new(0.0, 3.0 / 16.0, 0.0),
        scale: Vec3::splat(0.25),
    }
    .to_matrix()
}

fn deserialize_texture_map<'de, D>(de: D) -> Result<HashMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let raw: HashMap<String, serde_json::Value> = HashMap::deserialize(de)?;
    let mut out = HashMap::new();
    for (k, v) in raw {
        if let Some(s) = v.as_str() {
            out.insert(k, s.to_string());
        } else if let Some(sprite) = v.get("sprite").and_then(serde_json::Value::as_str) {
            out.insert(k, sprite.to_string());
        }
    }
    Ok(out)
}

#[derive(Deserialize, Clone)]
struct ElementDef {
    from: [f32; 3],
    to: [f32; 3],
    #[serde(default)]
    rotation: Option<ElementRotation>,
    #[serde(default)]
    faces: HashMap<String, FaceDef>,
    #[serde(default = "default_true")]
    shade: bool,
    #[serde(default)]
    light_emission: u8,
}

fn default_true() -> bool {
    true
}

/// Vanilla `BlockElementRotation`: one axis and an angle, or (since 26.1)
/// Euler angles `x`, `y` and `z`, turned in that order.
#[derive(Deserialize, Clone, Default)]
struct ElementRotation {
    origin: [f32; 3],
    axis: Option<String>,
    angle: Option<f32>,
    x: Option<f32>,
    y: Option<f32>,
    z: Option<f32>,
    #[serde(default)]
    rescale: bool,
}

impl ElementRotation {
    /// Vanilla `BlockElementRotation.transformation`, with
    /// `CuboidRotation.computeRescale`'s scale on each basis vector: the
    /// reciprocal of its largest absolute component once turned.
    fn matrix(&self) -> glam::Mat3 {
        let turn = match (self.axis.as_deref(), self.angle) {
            (Some(axis), Some(angle)) => {
                let angle = angle.to_radians();
                match axis {
                    "x" => Mat3::from_rotation_x(angle),
                    "y" => Mat3::from_rotation_y(angle),
                    "z" => Mat3::from_rotation_z(angle),
                    _ => Mat3::IDENTITY,
                }
            }
            _ => Mat3::from_euler(
                glam::EulerRot::ZYX,
                self.z.unwrap_or(0.0).to_radians(),
                self.y.unwrap_or(0.0).to_radians(),
                self.x.unwrap_or(0.0).to_radians(),
            ),
        };
        if !self.rescale {
            return turn;
        }
        let scale = |column: Vec3| 1.0 / column.abs().max_element();
        Mat3::from_cols(
            turn.x_axis * scale(turn.x_axis),
            turn.y_axis * scale(turn.y_axis),
            turn.z_axis * scale(turn.z_axis),
        )
    }
}

#[derive(Deserialize, Clone)]
struct FaceDef {
    uv: Option<[f32; 4]>,
    texture: String,
    cullface: Option<String>,
    #[serde(default)]
    rotation: Option<i32>,
    #[serde(rename = "tintindex")]
    tint_index: Option<i32>,
}

#[derive(Clone, Debug, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Direction {
    Down,
    Up,
    North,
    South,
    West,
    East,
}

impl Direction {
    /// Vanilla `Direction.values()`, indexed by the 3D data value.
    pub const VALUES: [Direction; 6] = [
        Direction::Down,
        Direction::Up,
        Direction::North,
        Direction::South,
        Direction::West,
        Direction::East,
    ];

    pub fn opposite(self) -> Self {
        match self {
            Direction::Down => Direction::Up,
            Direction::Up => Direction::Down,
            Direction::North => Direction::South,
            Direction::South => Direction::North,
            Direction::West => Direction::East,
            Direction::East => Direction::West,
        }
    }

    pub fn offset(&self) -> [i32; 3] {
        match self {
            Direction::Down => [0, -1, 0],
            Direction::Up => [0, 1, 0],
            Direction::North => [0, 0, -1],
            Direction::South => [0, 0, 1],
            Direction::West => [-1, 0, 0],
            Direction::East => [1, 0, 0],
        }
    }

    /// The index of the direction's axis: 0 for X, 1 for Y, 2 for Z.
    pub fn axis(self) -> usize {
        match self {
            Direction::West | Direction::East => 0,
            Direction::Down | Direction::Up => 1,
            Direction::North | Direction::South => 2,
        }
    }

    /// The unit step along the direction.
    pub fn step(self) -> glam::DVec3 {
        let [x, y, z] = self.offset();
        glam::dvec3(x.into(), y.into(), z.into())
    }

    /// The block-state property value, vanilla `getSerializedName`.
    pub fn name(self) -> &'static str {
        match self {
            Direction::Down => "down",
            Direction::Up => "up",
            Direction::North => "north",
            Direction::South => "south",
            Direction::West => "west",
            Direction::East => "east",
        }
    }

    pub(crate) fn from_str(s: &str) -> Option<Self> {
        match s {
            "down" => Some(Direction::Down),
            "up" => Some(Direction::Up),
            "north" => Some(Direction::North),
            "south" => Some(Direction::South),
            "west" => Some(Direction::West),
            "east" => Some(Direction::East),
            _ => None,
        }
    }

    fn rotate_y(self, degrees: i32) -> Self {
        let steps = degrees.rem_euclid(360) / 90;
        let mut d = self;
        for _ in 0..steps {
            d = match d {
                Direction::North => Direction::East,
                Direction::East => Direction::South,
                Direction::South => Direction::West,
                Direction::West => Direction::North,
                other => other,
            };
        }
        d
    }

    fn rotate_x(self, degrees: i32) -> Self {
        let steps = degrees.rem_euclid(360) / 90;
        let mut d = self;
        for _ in 0..steps {
            d = match d {
                Direction::North => Direction::Down,
                Direction::Down => Direction::South,
                Direction::South => Direction::Up,
                Direction::Up => Direction::North,
                other => other,
            };
        }
        d
    }

    /// The default table's shade, for the item paths that bake it in; terrain
    /// looks its dimension's table up at mesh time instead.
    pub(crate) fn shade_light(&self) -> f32 {
        CardinalLighting::DEFAULT.by_face(*self)
    }

    /// Vanilla `Direction.getApproximateNearest`: the direction whose normal
    /// has the largest positive dot with `v`, in float precision; `North`
    /// when none is positive.
    pub fn approximate_nearest(v: glam::DVec3) -> Self {
        let (x, y, z) = (v.x as f32, v.y as f32, v.z as f32);
        let mut result = Direction::North;
        // Java's `Float.MIN_VALUE`: the smallest subnormal.
        let mut highest = f32::from_bits(1);
        for direction in [
            Direction::Down,
            Direction::Up,
            Direction::North,
            Direction::South,
            Direction::West,
            Direction::East,
        ] {
            let [nx, ny, nz] = direction.offset();
            let dot = x * nx as f32 + y * ny as f32 + z * nz as f32;
            if dot > highest {
                highest = dot;
                result = direction;
            }
        }
        result
    }
}

// TODO: drop this once the serverbound packets carrying a face are
// pomme-encoded.
impl From<Direction> for azalea_core::direction::Direction {
    fn from(direction: Direction) -> Self {
        match direction {
            Direction::Down => Self::Down,
            Direction::Up => Self::Up,
            Direction::North => Self::North,
            Direction::South => Self::South,
            Direction::West => Self::West,
            Direction::East => Self::East,
        }
    }
}

/// Vanilla `CardinalLighting`: the per-face brightness a dimension shades
/// block faces with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CardinalLighting {
    pub down: f32,
    pub up: f32,
    pub north: f32,
    pub south: f32,
    pub west: f32,
    pub east: f32,
}

impl CardinalLighting {
    pub const DEFAULT: Self = Self {
        down: 0.5,
        up: 1.0,
        north: 0.8,
        south: 0.8,
        west: 0.6,
        east: 0.6,
    };
    pub const NETHER: Self = Self {
        down: 0.9,
        up: 0.9,
        north: 0.8,
        south: 0.8,
        west: 0.6,
        east: 0.6,
    };

    pub fn by_face(&self, dir: Direction) -> f32 {
        match dir {
            Direction::Down => self.down,
            Direction::Up => self.up,
            Direction::North => self.north,
            Direction::South => self.south,
            Direction::West => self.west,
            Direction::East => self.east,
        }
    }
}

/// A dimension type's `cardinal_light`, vanilla `CardinalLighting.Type`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CardinalLightType {
    #[default]
    Default,
    Nether,
}

impl CardinalLightType {
    pub fn table(self) -> CardinalLighting {
        match self {
            Self::Default => CardinalLighting::DEFAULT,
            Self::Nether => CardinalLighting::NETHER,
        }
    }
}

/// A texture name interned once per process, so a quad holds two bytes
/// instead of its own `String` and a mesh finds its atlas region by index
/// (`AtlasUVMap::region`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureId(u16);

#[derive(Default)]
struct TextureNames {
    names: Vec<&'static str>,
    ids: HashMap<&'static str, u16>,
}

/// Names are leaked: a few thousand distinct textures, for the whole run.
static TEXTURE_NAMES: LazyLock<RwLock<TextureNames>> = LazyLock::new(Default::default);

impl TextureId {
    pub fn intern(name: &str) -> Self {
        if let Some(&id) = TEXTURE_NAMES.read().unwrap().ids.get(name) {
            return Self(id);
        }
        let mut table = TEXTURE_NAMES.write().unwrap();
        if let Some(&id) = table.ids.get(name) {
            return Self(id);
        }
        let id = u16::try_from(table.names.len()).expect("over 65536 texture names");
        let name: &'static str = Box::leak(name.into());
        table.names.push(name);
        table.ids.insert(name, id);
        Self(id)
    }

    pub fn name(self) -> &'static str {
        TEXTURE_NAMES.read().unwrap().names[self.index()]
    }

    pub fn index(self) -> usize {
        usize::from(self.0)
    }

    /// Every name interned so far, by id.
    pub fn all_names() -> Vec<&'static str> {
        TEXTURE_NAMES.read().unwrap().names.clone()
    }
}

#[derive(Clone, Debug)]
pub struct BakedQuad {
    pub positions: [[f32; 3]; 4],
    pub uvs: [[f32; 2]; 4],
    pub texture: TextureId,
    pub cullface: Option<Direction>,
    pub tint: super::render_assets::Tint,
    /// Raw vanilla face `tintindex`. Terrain resolves this through `tint`,
    /// while item models use it to index the `ItemTintSource` palette from
    /// their item definition.
    pub tint_index: Option<u32>,
    /// The default table's shade, for GUI and held items.
    pub shade_light: f32,
    /// The face terrain shades this quad as, `None` for `shade: false`.
    pub shade_face: Option<Direction>,
    /// The element's `light_emission`, 0..15.
    pub emission: u8,
    /// The model's `ambientocclusion`, which smooth lighting needs.
    pub ambient_occlusion: bool,
}

#[derive(Clone, Debug)]
pub struct BakedModel {
    pub quads: Vec<BakedQuad>,
    pub is_full_cube: bool,
}

#[derive(Clone)]
pub struct MultipartEntry {
    pub when: HashMap<String, String>,
    pub model: Arc<BakedModel>,
}

pub fn load_all_block_textures(
    jar_assets_dir: &Path,
    asset_index: &Option<AssetIndex>,
    packs: Option<&crate::resource_pack::ResourcePackManager>,
) -> HashMap<String, FaceTextures> {
    let mut results = HashMap::new();
    let mut model_cache = HashMap::new();

    for_each_blockstate(
        jar_assets_dir,
        asset_index,
        packs,
        |block_name, blockstate| {
            let model_ref = extract_default_model_ref(blockstate)?;
            let resolved = resolve_model(
                &model_ref.model,
                jar_assets_dir,
                asset_index,
                &mut model_cache,
                packs,
            );
            let face_textures = build_face_textures(block_name, &resolved.textures)?;
            results.insert(block_name.to_string(), face_textures);
            Some(())
        },
    );

    tracing::info!(
        "Loaded {} block texture mappings from vanilla assets",
        results.len()
    );
    results
}

type BakedModelMap = HashMap<String, HashMap<String, Arc<BakedModel>>>;
type MultipartMap = HashMap<String, Vec<MultipartEntry>>;

pub fn bake_all_models(
    jar_assets_dir: &Path,
    asset_index: &Option<AssetIndex>,
    packs: Option<&crate::resource_pack::ResourcePackManager>,
) -> (BakedModelMap, MultipartMap) {
    let mut results: BakedModelMap = HashMap::new();
    let mut multipart_results: HashMap<String, Vec<MultipartEntry>> = HashMap::new();
    let mut model_cache = HashMap::new();
    let mut total = 0u32;

    for_each_blockstate(
        jar_assets_dir,
        asset_index,
        packs,
        |block_name, blockstate| {
            total += 1;
            let mut variants_map: HashMap<String, Arc<BakedModel>> = HashMap::new();
            // Variants (and multipart cases) naming the same model, rotation
            // and uvlock share one bake. Per block, since tints are.
            let mut shared: HashMap<(String, i32, i32, bool), Arc<BakedModel>> = HashMap::new();
            let mut bake = |model_ref: &ModelRef| {
                let key = (
                    model_ref.model.clone(),
                    model_ref.x,
                    model_ref.y,
                    model_ref.uvlock,
                );
                if let Some(baked) = shared.get(&key) {
                    return Some(Arc::clone(baked));
                }
                let resolved = resolve_model(
                    &model_ref.model,
                    jar_assets_dir,
                    asset_index,
                    &mut model_cache,
                    packs,
                );
                let mut baked =
                    bake_resolved_model(&resolved, model_ref.x, model_ref.y, Tint::None)?;
                apply_block_tints(&mut baked, block_name);
                let baked = Arc::new(baked);
                shared.insert(key, Arc::clone(&baked));
                Some(baked)
            };

            if let Some(variants) = &blockstate.variants {
                for (variant_key, variant_entry) in variants {
                    if let Some(baked) = bake(variant_entry.first()?) {
                        variants_map.insert(variant_key.clone(), baked);
                    }
                }
            } else if let Some(multipart) = &blockstate.multipart {
                let mut entries = Vec::new();
                for case in multipart {
                    if let Some(baked) = bake(case.apply.first()?) {
                        entries.push(MultipartEntry {
                            when: parse_when_condition(&case.when),
                            model: baked,
                        });
                    }
                }
                if !entries.is_empty() {
                    multipart_results.insert(block_name.to_string(), entries);
                }
            }

            if !variants_map.is_empty() {
                results.insert(block_name.to_string(), variants_map);
            }
            Some(())
        },
    );

    let mut missing_names: Vec<String> = Vec::new();
    for_each_blockstate(jar_assets_dir, asset_index, packs, |block_name, _| {
        if !results.contains_key(block_name)
            && !multipart_results.contains_key(block_name)
            && !crate::world::block_entity::is_block_entity_block(block_name)
            && !crate::world::block_entity::is_fluid_block(block_name)
            && !crate::world::block_entity::is_invisible_block(block_name)
        {
            missing_names.push(block_name.to_string());
        }
        Some(())
    });
    missing_names.sort();
    let baked_count = results.len() + multipart_results.len();
    tracing::info!(
        "Baked models for {}/{} blocks ({} unhandled)",
        baked_count,
        total,
        missing_names.len()
    );
    if !missing_names.is_empty() {
        tracing::warn!("Unhandled baked models: {}", missing_names.join(", "));
    }
    (results, multipart_results)
}

pub struct BakedItemModels {
    pub models: HashMap<String, BakedModel>,
    pub generated_textures: HashSet<String>,
    pub flat_texture_keys: HashMap<String, Vec<Option<String>>>,
    pub(crate) gui_transforms: HashMap<String, DisplayTransform>,
    pub ground_transforms: HashMap<String, Mat4>,
    /// Every display context's transform, from the item's own (pack) models.
    pub(crate) display_transforms: HashMap<String, DisplayTransforms>,
    pub tint_sources: HashMap<String, Vec<ItemTintSource>>,
}

/// Vanilla 26.2 item-model tint sources. Colors keep the codec's full packed
/// ARGB integer because `dye` and `firework` preserve their configured default
/// alpha when no component color is present; sources that Vanilla explicitly
/// opacifies do so during evaluation.
#[derive(Clone, Debug, PartialEq)]
pub enum ItemTintSource {
    Constant(u32),
    Grass { color: u32 },
    Dye { default: u32 },
    Firework { default: u32 },
    Potion { default: u32 },
    MapColor { default: u32 },
    CustomModelData { index: usize, default: u32 },
    Team { default: u32 },
}

/// Every `minecraft/items/*.json` name across the jar and the active packs,
/// so a pack can add an item, not only replace one. Vanilla walks every pack
/// in `FallbackResourceManager`.
fn item_definition_names(
    jar_assets_dir: &Path,
    packs: Option<&crate::resource_pack::ResourcePackManager>,
) -> std::collections::BTreeSet<String> {
    let pack_dirs = packs
        .into_iter()
        .flat_map(|packs| packs.active_pack_dirs())
        .map(|dir| dir.join("assets"));
    std::iter::once(jar_assets_dir.to_path_buf())
        .chain(pack_dirs)
        .filter_map(|root| std::fs::read_dir(root).ok())
        .flatten()
        .flatten()
        .filter_map(|namespace| {
            let ns = namespace.file_name().to_str()?.to_owned();
            let entries = std::fs::read_dir(namespace.path().join("items")).ok()?;
            Some(entries.flatten().filter_map(move |entry| {
                let path = entry
                    .file_name()
                    .to_str()?
                    .strip_suffix(".json")?
                    .to_owned();
                Some(item_definition_name(&ns, &path))
            }))
        })
        .flatten()
        .collect()
}

/// An item definition's key: its path for `minecraft`, else `namespace:path`
/// (the form an `item_model` component names it by, see
/// [`item_model_name`]).
fn item_definition_name(namespace: &str, path: &str) -> String {
    if namespace == "minecraft" {
        path.to_owned()
    } else {
        format!("{namespace}:{path}")
    }
}

/// The asset key of the item definition named `name` (an
/// [`item_definition_name`]).
fn item_definition_asset_key(name: &str) -> String {
    let (namespace, path) = name.split_once(':').unwrap_or(("minecraft", name));
    format!("{namespace}/items/{path}.json")
}

/// The definition key an `item_model` component's id draws with.
pub fn item_model_name(id: &str) -> String {
    let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", id));
    item_definition_name(namespace, path)
}

fn opaque(color: u32) -> u32 {
    color | 0xFF00_0000
}

/// Vanilla `ExtraCodecs.RGB_COLOR_CODEC`: either a signed 32-bit packed color
/// or a three-float vector converted with `ARGB.colorFromFloat(1, r, g, b)`.
fn parse_rgb_color(value: &serde_json::Value) -> Result<u32, String> {
    if let Some(integer) = value.as_i64() {
        let integer =
            i32::try_from(integer).map_err(|_| "color integer is outside i32".to_string())?;
        return Ok(integer as u32);
    }
    let components = value
        .as_array()
        .ok_or_else(|| "color must be an integer or three-float array".to_string())?;
    if components.len() != 3 {
        return Err("color vector must contain exactly three floats".to_string());
    }
    let mut channel = [0_u32; 3];
    for (dst, value) in channel.iter_mut().zip(components) {
        let value = value
            .as_f64()
            .ok_or_else(|| "color vector entries must be numbers".to_string())?
            as f32;
        // Mojang's ARGB.as8BitChannel is floor(value * 255). ARGB.color then
        // keeps the low eight bits of each channel.
        *dst = ((value * 255.0).floor() as i32 as u32) & 0xFF;
    }
    Ok(0xFF00_0000 | channel[0] << 16 | channel[1] << 8 | channel[2])
}

fn required_color(value: &serde_json::Value, field: &str) -> Result<u32, String> {
    parse_rgb_color(
        value
            .get(field)
            .ok_or_else(|| format!("missing required `{field}` color"))?,
    )
}

fn required_unit_float(value: &serde_json::Value, field: &str) -> Result<f32, String> {
    let number = value
        .get(field)
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| format!("missing or invalid `{field}` float"))? as f32;
    if !(0.0..=1.0).contains(&number) {
        return Err(format!("`{field}` must be in [0, 1]"));
    }
    Ok(number)
}

fn parse_item_tint_sources(
    values: &[serde_json::Value],
    grass_colormap: &Colormap,
) -> Result<Vec<ItemTintSource>, String> {
    values
        .iter()
        .map(|value| {
            let ty = value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .map(strip_mc_prefix)
                .ok_or_else(|| "item tint source is missing a string `type`".to_string())?;
            match ty {
                "constant" => Ok(ItemTintSource::Constant(opaque(required_color(
                    value, "value",
                )?))),
                "grass" => Ok(ItemTintSource::Grass {
                    color: opaque(grass_colormap.rgb(
                        required_unit_float(value, "temperature")?,
                        required_unit_float(value, "downfall")?,
                    )),
                }),
                "dye" => Ok(ItemTintSource::Dye {
                    default: required_color(value, "default")?,
                }),
                "firework" => Ok(ItemTintSource::Firework {
                    default: required_color(value, "default")?,
                }),
                "potion" => Ok(ItemTintSource::Potion {
                    default: required_color(value, "default")?,
                }),
                "map_color" => Ok(ItemTintSource::MapColor {
                    default: required_color(value, "default")?,
                }),
                "custom_model_data" => {
                    let index = match value.get("index") {
                        None => 0,
                        Some(index) => {
                            let index = index.as_i64().ok_or_else(|| {
                                "`index` must be a non-negative integer".to_string()
                            })?;
                            usize::try_from(index)
                                .map_err(|_| "`index` must be a non-negative integer".to_string())?
                        }
                    };
                    Ok(ItemTintSource::CustomModelData {
                        index,
                        default: required_color(value, "default")?,
                    })
                }
                "team" => Ok(ItemTintSource::Team {
                    default: required_color(value, "default")?,
                }),
                other => Err(format!("unsupported item tint source type `{other}`")),
            }
        })
        .collect()
}

pub fn bake_item_models(
    jar_assets_dir: &Path,
    asset_index: &Option<AssetIndex>,
    packs: Option<&crate::resource_pack::ResourcePackManager>,
) -> BakedItemModels {
    let mut item_models: HashMap<String, BakedModel> = HashMap::new();
    let mut flat_item_textures: HashSet<String> = HashSet::new();
    let mut flat_keys: HashMap<String, Vec<Option<String>>> = HashMap::new();
    let mut gui_transforms: HashMap<String, DisplayTransform> = HashMap::new();
    let mut ground_transforms: HashMap<String, Mat4> = HashMap::new();
    let mut display_transforms: HashMap<String, DisplayTransforms> = HashMap::new();
    // 26.1 moved the shield's and trident's textures into folders of their
    // own; the jar says which layout it has.
    let jar_texture = |candidates: [&'static str; 2]| {
        candidates
            .into_iter()
            .find(|texture| {
                let key = format!("minecraft/textures/{texture}.png");
                resolve_asset_path_with_packs(jar_assets_dir, asset_index, &key, None).exists()
            })
            .unwrap_or(candidates[1])
    };
    let shield_texture = jar_texture([
        "entity/shield/shield_base_nopattern",
        "entity/shield_base_nopattern",
    ]);
    let trident_texture = jar_texture(["entity/trident/trident", "entity/trident"]);
    let banner_texture = jar_texture(["entity/banner/banner_base", "entity/banner_base"]);
    let mut tint_sources: HashMap<String, Vec<ItemTintSource>> = HashMap::new();
    let grass_colormap = Colormap::load(
        jar_assets_dir,
        asset_index,
        "minecraft/textures/colormap/grass.png",
        packs,
    );
    let mut model_cache: HashMap<String, ModelFile> = HashMap::new();

    let mut bake = |name: &str, parts: &[ModelPart]| {
        let mut merged: Option<BakedModel> = None;
        let mut item_tints = Vec::new();
        // Vanilla applies each composite part's own display transforms. Pomme
        // merges the parts into one mesh, so it can apply only one; no vanilla
        // composite disagrees (beds share `block/template_bed`), so the first
        // part's wins and a disagreement is logged rather than modelled.
        let mut ground_transform: Option<Mat4> = None;
        let mut gui_transform: Option<DisplayTransform> = None;
        let mut gui_light: Option<GuiLight> = None;
        let mut display: Option<DisplayTransforms> = None;
        // Each part's GUI pose, for a GUI variant whose parts disagree.
        let mut part_gui: Vec<(usize, DisplayTransform)> = Vec::new();
        // Each part's display transforms, for the views its parts disagree in.
        let mut part_display: Vec<(usize, DisplayTransforms)> = Vec::new();
        for part in parts {
            let resolved = resolve_model(
                &part.path,
                jar_assets_dir,
                asset_index,
                &mut model_cache,
                packs,
            );
            display.get_or_insert(resolved.display);
            match ground_transform {
                None => ground_transform = Some(resolved.ground_transform),
                Some(existing) if existing.abs_diff_eq(resolved.ground_transform, 1.0e-6) => {}
                Some(_) => tracing::warn!(
                    "{name}: composite part {} has a different ground transform; using the first part's",
                    part.path
                ),
            }
            match gui_transform {
                None => gui_transform = Some(resolved.gui_transform),
                Some(existing) if existing == resolved.gui_transform => {}
                Some(_) => tracing::warn!(
                    "{name}: composite part {} has a different GUI transform; using the first part's",
                    part.path
                ),
            }
            match gui_light {
                None => gui_light = Some(resolved.gui_light),
                Some(existing) if existing == resolved.gui_light => {}
                Some(_) => tracing::warn!(
                    "{name}: composite part {} has a different GUI light; using the first part's",
                    part.path
                ),
            }
            let part_tints = match parse_item_tint_sources(&part.tints, &grass_colormap) {
                Ok(tints) => tints,
                Err(error) => {
                    tracing::error!("Couldn't parse item model '{name}': {error}");
                    return;
                }
            };
            let tint_base = item_tints.len() as u32;
            // A generated flat item may have several layers. Vanilla assigns
            // tint index N to layerN, so keep every layer in order instead of
            // collapsing the item to layer0.
            if parts.len() == 1 && part.special.is_none() && resolved.elements.is_empty() {
                let mut keys = Vec::new();
                for layer in 0..5 {
                    let Some(value) = resolved.textures.get(&format!("layer{layer}")) else {
                        break;
                    };
                    let key = texture_to_name(value);
                    if let Some(key) = &key {
                        flat_item_textures.insert(key.clone());
                    }
                    keys.push(key);
                }
                if keys.iter().any(Option::is_some) {
                    flat_keys.insert(name.to_string(), keys);
                }
                item_tints.extend(part_tints);
                break;
            }
            let baked = match &part.special {
                Some(special) => {
                    let (layer, texture) = match special {
                        SpecialModel::Shield => (exported_data::SHIELD, shield_texture.to_owned()),
                        SpecialModel::Trident => {
                            (exported_data::TRIDENT, trident_texture.to_owned())
                        }
                        SpecialModel::ShulkerBox { texture } => {
                            (exported_data::SHULKER_BOX, texture.clone())
                        }
                        SpecialModel::Conduit => (
                            exported_data::CONDUIT_SHELL,
                            "entity/conduit/base".to_owned(),
                        ),
                        SpecialModel::DecoratedPot => (
                            exported_data::DECORATED_POT_BASE,
                            "entity/decorated_pot/decorated_pot_base".to_owned(),
                        ),
                        SpecialModel::Banner { .. } => {
                            (exported_data::STANDING_BANNER, banner_texture.to_owned())
                        }
                        SpecialModel::Head { kind, texture } => (kind.layer(), texture.clone()),
                    };
                    let mut baked = bake_exported_item(layer, &texture);
                    // `BannerRenderer.renderPatterns`: the flag, then the base
                    // pattern in the base colour (tint 0, the part's own).
                    // The pot's four sides, in the plain side's texture.
                    if matches!(special, SpecialModel::DecoratedPot) {
                        let sides =
                            bake_exported_item(exported_data::DECORATED_POT_SIDES, POT_SIDE);
                        baked.quads.extend(sides.quads);
                    }
                    if matches!(special, SpecialModel::Banner { .. }) {
                        let flag = exported_data::STANDING_BANNER_FLAG;
                        baked
                            .quads
                            .extend(bake_exported_item(flag, banner_texture).quads);
                        let mut base = bake_exported_item(flag, BANNER_BASE_PATTERN).quads;
                        for quad in &mut base {
                            quad.tint_index = Some(0);
                        }
                        baked.quads.extend(base);
                    }
                    Some(baked)
                }
                // A generated sprite among other parts: its layers as flat
                // cards (vanilla's generated faces, less the edges).
                None if resolved.elements.is_empty() => bake_generated_layers(&resolved),
                None => bake_resolved_model(&resolved, 0, 0, Tint::None),
            };
            let Some(mut baked) = baked else {
                continue;
            };
            for quad in &mut baked.quads {
                if let Some(index) = quad.tint_index {
                    quad.tint_index =
                        (index < part_tints.len() as u32).then_some(tint_base + index);
                }
            }
            item_tints.extend(part_tints);
            // A special model's pose: the definition's own transformation
            // (26.1 moved it there), else what its renderer applied itself.
            let transform = part
                .transform
                .or_else(|| part.special.as_ref().map(SpecialModel::legacy_transform));
            if let Some(m) = transform {
                for quad in &mut baked.quads {
                    for p in &mut quad.positions {
                        *p = m.transform_point3(Vec3::from_array(*p)).to_array();
                    }
                }
            }
            let start = merged.as_ref().map_or(0, |model| model.quads.len());
            part_gui.push((start, resolved.gui_transform));
            part_display.push((start, resolved.display));
            merged = Some(match merged.take() {
                None => baked,
                Some(mut model) => {
                    model.quads.extend(baked.quads);
                    model.is_full_cube = false;
                    model
                }
            });
        }
        // Vanilla draws each layer of the stack in its own model's pose; a GUI
        // variant (Void Rush's scrolls: a banner shrunk behind a sprite) whose
        // parts disagree takes each part's pose into its quads instead.
        if name.ends_with("#gui")
            && part_gui.windows(2).any(|pair| pair[0].1 != pair[1].1)
            && let Some(model) = &mut merged
        {
            let ends = part_gui
                .iter()
                .skip(1)
                .map(|(start, _)| *start)
                .chain(std::iter::once(model.quads.len()));
            for ((start, transform), end) in part_gui.iter().zip(ends) {
                let m = Mat4::from_translation(Vec3::splat(0.5))
                    * transform.to_matrix()
                    * Mat4::from_translation(Vec3::splat(-0.5));
                for quad in &mut model.quads[*start..end] {
                    for p in &mut quad.positions {
                        *p = m.transform_point3(Vec3::from_array(*p)).to_array();
                    }
                }
            }
            gui_transform = Some(DisplayTransform::IDENTITY);
        }
        if let Some(transform) = ground_transform {
            ground_transforms.insert(name.to_string(), transform);
        }
        let gui_transform = gui_transform.unwrap_or(DisplayTransform::IDENTITY);
        gui_transforms.insert(name.to_string(), gui_transform);
        if let Some(display) = display {
            display_transforms.insert(name.to_string(), display);
        }
        if !item_tints.is_empty() {
            tint_sources.insert(name.to_string(), item_tints);
        }
        if let Some(mut baked) = merged {
            apply_gui_lambert(
                &mut baked.quads,
                gui_transform,
                gui_light.unwrap_or_default(),
            );
            // Vanilla draws each composite part in its own model's pose;
            // where the parts disagree in a view (Void Rush's shield frames,
            // posed apart from the plate they wrap), that view gets a model
            // of its own with each part's pose folded into its quads, drawn
            // in the first part's.
            if !name.ends_with("#gui") {
                for (context, quads) in context_variants(&baked.quads, &part_display) {
                    let key = context_model_key(name, context);
                    if let Some(display) = display {
                        display_transforms.insert(key.clone(), display);
                    }
                    if let Some(transform) = ground_transforms.get(name).copied() {
                        ground_transforms.insert(key.clone(), transform);
                    }
                    gui_transforms.insert(key.clone(), gui_transform);
                    if let Some(tints) = tint_sources.get(name).cloned() {
                        tint_sources.insert(key.clone(), tints);
                    }
                    item_models.insert(
                        key,
                        BakedModel {
                            quads,
                            is_full_cube: false,
                        },
                    );
                }
            }
            item_models.insert(name.to_string(), baked);
        }
    };

    // Items the special `chest` renderer draws, with its texture.
    let mut chests = Vec::new();
    for item_name in item_definition_names(jar_assets_dir, packs) {
        let item_name = item_name.as_str();
        let item_asset_key = item_definition_asset_key(item_name);
        let item_path =
            resolve_asset_path_with_packs(jar_assets_dir, asset_index, &item_asset_key, packs);
        let Ok(contents) = std::fs::read_to_string(item_path) else {
            continue;
        };
        let Ok(json): Result<serde_json::Value, _> = serde_json::from_str(&contents) else {
            continue;
        };

        if let Some(texture) = chest_special_texture(&json) {
            chests.push((item_name.to_string(), texture));
        }
        let parts = collect_model_parts(&json);
        if parts.is_empty() {
            continue;
        }
        bake(item_name, &parts);
        if let Some(root) = json.get("model")
            && has_gui_variant(root)
        {
            bake(&gui_model_key(item_name), &node_model_parts_in(root, true));
        }
        for (property, suffix) in CONDITION_VARIANTS {
            if let Some(root) = json.get("model")
                && has_condition_variant(component_default_branch(root), property)
            {
                let parts = node_model_parts_holding(root, property);
                if !parts.is_empty() {
                    bake(&format!("{item_name}{suffix}"), &parts);
                }
            }
        }
        for (charge, node) in charge_type_cases(&json) {
            let mut parts = Vec::new();
            collect_parts_from_node(node, None, &mut parts);
            bake(&charge_model_key(item_name, charge), &parts);
        }
        for (when, node) in custom_model_data_cases(&json) {
            let parts = node_model_parts(node);
            if !parts.is_empty() {
                let key = custom_model_key(item_name, when);
                bake(&key, &parts);
                if has_gui_variant(node) {
                    bake(&gui_model_key(&key), &node_model_parts_in(node, true));
                }
                for (property, suffix) in CONDITION_VARIANTS {
                    if has_condition_variant(node, property) {
                        let parts = node_model_parts_holding(node, property);
                        if !parts.is_empty() {
                            bake(&format!("{key}{suffix}"), &parts);
                        }
                    }
                }
            }
        }
        // The bundle's selected-item model composes these around the
        // selected stack at draw time.
        if let Some(layers) = selected_bundle_layers(&json) {
            for (front, parts) in [false, true].into_iter().zip(&layers) {
                bake(&selected_bundle_layer_key(item_name, front), parts);
            }
        }
    }

    for (key, path) in ITEM_FRAME_MODELS {
        let part = ModelPart {
            path: path.to_string(),
            transform: None,
            tints: Vec::new(),
            special: None,
        };
        bake(key, &[part]);
    }

    // The mesh comes from the special renderer, but the chest's display
    // transforms live in `item/template_chest` like any other item's.
    let chest = resolve_model(
        "item/template_chest",
        jar_assets_dir,
        asset_index,
        &mut model_cache,
        packs,
    );
    for (name, texture) in chests {
        item_models.insert(
            name.clone(),
            bake_chest_item_model(&texture, chest.gui_transform, chest.gui_light),
        );
        gui_transforms.insert(name.clone(), chest.gui_transform);
        display_transforms.insert(name.clone(), chest.display);
        ground_transforms.insert(name.clone(), chest.ground_transform);
        flat_keys.remove(&name);
    }

    tracing::info!(
        "Baked {} item models, {} flat items, and registered {} generated-item textures",
        item_models.len(),
        flat_keys.len(),
        flat_item_textures.len()
    );
    BakedItemModels {
        models: item_models,
        generated_textures: flat_item_textures,
        flat_texture_keys: flat_keys,
        gui_transforms,
        ground_transforms,
        display_transforms,
        tint_sources,
    }
}

/// The texture (`entity/chest/<name>`) of the special `chest` model a plain
/// stack of this item definition draws: `chest`'s and `trapped_chest`'s
/// outside Christmas (the `local_time` select's fallback), `ender_chest`'s,
/// the copper chests'.
// TODO: the Christmas texture from 24 to 26 December.
fn chest_special_texture(json: &serde_json::Value) -> Option<String> {
    let mut node = json.get("model")?;
    while node_type(node) == Some("select") {
        node = node.get("fallback")?;
    }
    let special = node
        .get("model")
        .filter(|_| node_type(node) == Some("special"))?;
    if node_type(special) != Some("chest") {
        return None;
    }
    let texture = strip_mc_prefix(special.get("texture")?.as_str()?);
    Some(format!("entity/chest/{texture}"))
}

fn bake_chest_item_model(tex: &str, gui: DisplayTransform, gui_light: GuiLight) -> BakedModel {
    let mut quads = Vec::new();
    let shades = vanilla_gui_face_shades(gui, gui_light);
    add_chest_cube(
        &mut quads,
        1.0 / 16.0,
        0.0,
        1.0 / 16.0,
        15.0 / 16.0,
        10.0 / 16.0,
        15.0 / 16.0,
        0.0,
        19.0,
        14.0,
        10.0,
        14.0,
        tex,
        shades,
    );
    add_chest_cube(
        &mut quads,
        1.0 / 16.0,
        9.0 / 16.0,
        1.0 / 16.0,
        15.0 / 16.0,
        14.0 / 16.0,
        15.0 / 16.0,
        0.0,
        0.0,
        14.0,
        5.0,
        14.0,
        tex,
        shades,
    );
    add_chest_cube(
        &mut quads,
        7.0 / 16.0,
        7.0 / 16.0,
        15.0 / 16.0,
        9.0 / 16.0,
        11.0 / 16.0,
        1.0,
        0.0,
        0.0,
        2.0,
        4.0,
        1.0,
        tex,
        shades,
    );
    BakedModel {
        quads,
        is_full_cube: false,
    }
}

/// The special item models pomme draws (vanilla `SpecialModelRenderer`s),
/// each from vanilla's own model layer.
// TODO: the shield's patterns and copper golem statues.
#[derive(Clone, Debug, PartialEq)]
enum SpecialModel {
    Shield,
    Trident,
    /// Its texture, `entity/shulker/<name>`.
    ShulkerBox {
        texture: String,
    },
    /// `ConduitSpecialRenderer`: the conduit's shell.
    Conduit,
    /// `DecoratedPotSpecialRenderer`: the pot, its four sides plain.
    // TODO: a pot's sherds, each side's own pattern.
    DecoratedPot,
    /// `BannerSpecialRenderer`: the standing banner, its flag in its base
    /// colour (`0xRRGGBB`); a stack's pattern layers go on per stack
    /// ([`crate::banner`]).
    Banner {
        color: u32,
    },
    /// `SkullSpecialRenderer` and `PlayerHeadSpecialRenderer`: a head or
    /// skull in its texture (`entity/...`); a player head's stack draws its
    /// own skin in the GUI ([`PLAYER_HEAD_TEXTURE`] marks the model).
    Head {
        kind: HeadKind,
        texture: String,
    },
}

/// The default skin a player head draws (vanilla `DefaultPlayerSkin`), and
/// the texture that marks a player-head item model.
pub const PLAYER_HEAD_TEXTURE: &str = "entity/player/wide/steve";

/// Vanilla `SkullBlock.Types`, each with its model layer and texture
/// (`SkullBlockRenderer.SKIN_BY_TYPE`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum HeadKind {
    Skeleton,
    WitherSkeleton,
    Zombie,
    Creeper,
    Piglin,
    Dragon,
    Player,
}

impl HeadKind {
    fn of(name: &str) -> Option<Self> {
        Some(match strip_mc_prefix(name) {
            "skeleton" => Self::Skeleton,
            "wither_skeleton" => Self::WitherSkeleton,
            "zombie" => Self::Zombie,
            "creeper" => Self::Creeper,
            "piglin" => Self::Piglin,
            "dragon" => Self::Dragon,
            "player" => Self::Player,
            _ => return None,
        })
    }

    fn layer(self) -> &'static [crate::renderer::entity_model::exported::ExportedPart] {
        match self {
            Self::Skeleton => exported_data::SKELETON_SKULL,
            Self::WitherSkeleton => exported_data::WITHER_SKELETON_SKULL,
            Self::Zombie => exported_data::ZOMBIE_HEAD,
            Self::Creeper => exported_data::CREEPER_HEAD,
            Self::Piglin => exported_data::PIGLIN_HEAD,
            Self::Dragon => exported_data::DRAGON_SKULL,
            Self::Player => exported_data::PLAYER_HEAD,
        }
    }

    fn texture(self) -> &'static str {
        match self {
            Self::Skeleton => "entity/skeleton/skeleton",
            Self::WitherSkeleton => "entity/skeleton/wither_skeleton",
            Self::Zombie => "entity/zombie/zombie",
            Self::Creeper => "entity/creeper/creeper",
            Self::Piglin => "entity/piglin/piglin",
            Self::Dragon => "entity/enderdragon/dragon",
            Self::Player => PLAYER_HEAD_TEXTURE,
        }
    }
}

/// The banner pattern every flag draws first, in the banner's base colour;
/// a stack's pattern layers copy its quads.
pub const BANNER_BASE_PATTERN: &str = "entity/banner/base";

/// A decorated pot's plain side.
pub const POT_SIDE: &str = "entity/decorated_pot/decorated_pot_side";
/// What starts the sides of a decorated pot's model key: after it, the
/// textures' names for its back, left, right and front
/// (`PotDecorations`' order), each empty for a plain side.
pub const POT_SIDES_KEY: &str = "decorated_pot~sides:";

/// The model key of a decorated pot with these sherds (item ids, back,
/// left, right, front), or the plain pot's when it has none
/// (`DecoratedPotPatterns`: a sherd's pattern is named after it, and a
/// brick is a plain side).
pub fn decorated_pot_key<'a>(sherds: impl Iterator<Item = &'a str>) -> String {
    let sides: Vec<&str> = sherds
        .map(|sherd| {
            let sherd = sherd.strip_prefix("minecraft:").unwrap_or(sherd);
            sherd.strip_suffix("_pottery_sherd").unwrap_or("")
        })
        .chain(std::iter::repeat(""))
        .take(4)
        .collect();
    if sides.iter().all(|side| side.is_empty()) {
        "decorated_pot".to_owned()
    } else {
        format!("{POT_SIDES_KEY}{}", sides.join(","))
    }
}

impl SpecialModel {
    /// A `minecraft:special` node's model, if pomme draws it.
    fn of(model: &serde_json::Value) -> Option<Self> {
        match node_type(model)? {
            "shield" => Some(Self::Shield),
            "trident" => Some(Self::Trident),
            "conduit" => Some(Self::Conduit),
            "decorated_pot" => Some(Self::DecoratedPot),
            "player_head" => Some(Self::Head {
                kind: HeadKind::Player,
                texture: PLAYER_HEAD_TEXTURE.into(),
            }),
            "head" => {
                let kind = HeadKind::of(model.get("kind")?.as_str()?)?;
                // `SkullSpecialRenderer.Unbaked`'s texture override, under
                // `textures/entity/`.
                let texture = match model.get("texture").and_then(serde_json::Value::as_str) {
                    Some(path) => format!("entity/{}", strip_mc_prefix(path)),
                    None => kind.texture().to_owned(),
                };
                Some(Self::Head { kind, texture })
            }
            "banner" => Some(Self::Banner {
                color: crate::banner::dye_rgb_named(
                    model
                        .get("color")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("white"),
                ),
            }),
            "shulker_box" => {
                let name = model
                    .get("texture")
                    .and_then(serde_json::Value::as_str)
                    .map_or("shulker", strip_mc_prefix);
                Some(Self::ShulkerBox {
                    texture: format!("entity/shulker/{name}"),
                })
            }
            _ => None,
        }
    }

    /// What the renderer did to the model itself before 26.1 moved it into
    /// the item definition's `transformation`, for definitions in the older
    /// format (packs made for 1.21): `ShieldSpecialRenderer` and
    /// `TridentSpecialRenderer` flip it, `ShulkerBoxRenderer` stands it in the
    /// block, flipped, a hair inside, `ConduitSpecialRenderer` centres it and
    /// `BannerRenderer.renderInHand` stands it in the block at two thirds,
    /// flipped.
    fn legacy_transform(&self) -> Mat4 {
        let flip = Mat4::from_scale(Vec3::new(1.0, -1.0, -1.0));
        match self {
            Self::Shield | Self::Trident => flip,
            Self::Conduit => Mat4::from_translation(Vec3::splat(0.5)),
            // The pot's layers are built in the block already.
            Self::DecoratedPot => Mat4::IDENTITY,
            Self::Banner { .. } => {
                Mat4::from_translation(Vec3::new(0.5, 0.0, 0.5))
                    * Mat4::from_scale(Vec3::new(2.0, -2.0, -2.0) / 3.0)
            }
            Self::ShulkerBox { .. } => {
                Mat4::from_translation(Vec3::new(0.5, 1.4995, 0.5))
                    * flip
                    * Mat4::from_scale(Vec3::splat(0.9995))
            }
            // `SkullBlockRenderer.renderSkull` with no facing.
            Self::Head { .. } => {
                Mat4::from_translation(Vec3::new(0.5, 0.0, 0.5))
                    * Mat4::from_scale(Vec3::new(-1.0, -1.0, 1.0))
            }
        }
    }
}

/// A generated item's layers (`layer0`, `layer1`, ...) as two-sided flat
/// cards a pixel thick, each taking its layer's tint index: vanilla
/// `ItemModelGenerator`'s front and back faces.
fn bake_generated_layers(resolved: &ResolvedModel) -> Option<BakedModel> {
    let mut quads = Vec::new();
    for layer in 0..5u32 {
        let Some(value) = resolved.textures.get(&format!("layer{layer}")) else {
            break;
        };
        let Some(texture) = texture_to_name(value) else {
            continue;
        };
        let texture = TextureId::intern(&texture);
        let card = |z: f32, front: bool| {
            let corners = [
                ([0.0, 1.0, z], [0.0, 0.0]),
                ([0.0, 0.0, z], [0.0, 1.0]),
                ([1.0, 0.0, z], [1.0, 1.0]),
                ([1.0, 1.0, z], [1.0, 0.0]),
            ];
            let order = if front { [0, 1, 2, 3] } else { [3, 2, 1, 0] };
            BakedQuad {
                positions: order.map(|i| corners[i].0),
                uvs: order.map(|i| corners[i].1),
                texture,
                cullface: None,
                tint: Tint::None,
                tint_index: Some(layer),
                shade_light: 1.0,
                shade_face: None,
                emission: 0,
                ambient_occlusion: false,
            }
        };
        quads.push(card(8.5 / 16.0, true));
        quads.push(card(7.5 / 16.0, false));
    }
    (!quads.is_empty()).then_some(BakedModel {
        quads,
        is_full_cube: false,
    })
}

/// A special model's geometry from vanilla's model layer, as its renderer
/// draws it: each part through its `translateAndRotate` chain, in the item
/// model's frame (blocks, y down as the layer is) before the special's own
/// transform.
fn bake_exported_item(
    parts: &[crate::renderer::entity_model::exported::ExportedPart],
    texture: &str,
) -> BakedModel {
    let mut matrices: Vec<Mat4> = Vec::with_capacity(parts.len());
    let mut quads = Vec::new();
    for part in parts {
        let [x, y, z, x_rot, y_rot, z_rot, x_scale, y_scale, z_scale] = part.pose;
        let local = Mat4::from_translation(Vec3::new(x, y, z) / 16.0)
            * Mat4::from_euler(glam::EulerRot::ZYX, z_rot, y_rot, x_rot)
            * Mat4::from_scale(Vec3::new(x_scale, y_scale, z_scale));
        let matrix = match part.parent {
            Some(parent) => matrices[usize::from(parent)] * local,
            None => local,
        };
        matrices.push(matrix);
        for quad in part.quads {
            quads.push(BakedQuad {
                positions: quad.corners.map(|corner| {
                    matrix
                        .transform_point3(Vec3::new(corner[0], corner[1], corner[2]) / 16.0)
                        .to_array()
                }),
                uvs: quad.corners.map(|corner| [corner[3], corner[4]]),
                texture: TextureId::intern(texture),
                cullface: None,
                tint: Tint::None,
                tint_index: None,
                shade_light: 1.0,
                shade_face: None,
                emission: 0,
                ambient_occlusion: true,
            });
        }
    }
    BakedModel {
        quads,
        is_full_cube: false,
    }
}

fn items_3d_lights() -> ([f32; 3], [f32; 3]) {
    let pose = Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0))
        * Mat4::from_rotation_y(1.0821041)
        * Mat4::from_rotation_x(3.2375858)
        * Mat4::from_rotation_y(-std::f32::consts::PI / 8.0)
        * Mat4::from_rotation_x(2.3561945);
    let transform = |x: f32, y: f32, z: f32| {
        pose.transform_vector3(Vec3::new(x, y, z).normalize())
            .to_array()
    };
    (transform(0.2, 1.0, -0.7), transform(-0.2, 1.0, 0.7))
}

fn items_flat_lights() -> ([f32; 3], [f32; 3]) {
    let pose =
        Mat4::from_rotation_y(-std::f32::consts::PI / 8.0) * Mat4::from_rotation_x(2.3561945);
    let transform = |x: f32, y: f32, z: f32| {
        pose.transform_vector3(Vec3::new(x, y, z).normalize())
            .to_array()
    };
    (transform(0.2, 1.0, -0.7), transform(-0.2, 1.0, 0.7))
}

fn gui_lights(gui_light: GuiLight) -> ([f32; 3], [f32; 3]) {
    match gui_light {
        GuiLight::Front => items_flat_lights(),
        GuiLight::Side => items_3d_lights(),
    }
}

fn lambert_shade(world_normal: [f32; 3], l0: [f32; 3], l1: [f32; 3]) -> f32 {
    let d0 = (l0[0] * world_normal[0] + l0[1] * world_normal[1] + l0[2] * world_normal[2]).max(0.0);
    let d1 = (l1[0] * world_normal[0] + l1[1] * world_normal[1] + l1[2] * world_normal[2]).max(0.0);
    ((d0 + d1) * 0.6 + 0.4).min(1.0)
}

fn transform_gui_normal(n_mesh: [f32; 3], display: DisplayTransform) -> [f32; 3] {
    // GuiItemAtlas applies (slot, -slot, slot) before ItemTransform. Vanilla's
    // PoseStack transforms normals by the inverse-transpose of that composed
    // pose, including each model's actual GUI rotation/scale.
    let pose = Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0)) * display.to_matrix();
    let normal = Mat3::from_mat4(pose).inverse().transpose() * Vec3::from_array(n_mesh);
    normal.normalize_or_zero().to_array()
}

fn vanilla_gui_face_shades(display: DisplayTransform, gui_light: GuiLight) -> [f32; 6] {
    let (l0, l1) = gui_lights(gui_light);
    let normals = [
        [0.0, 1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 0.0, -1.0],
        [0.0, 0.0, 1.0],
        [-1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
    ];
    let mut shades = [0.0; 6];
    for (i, &n) in normals.iter().enumerate() {
        shades[i] = lambert_shade(transform_gui_normal(n, display), l0, l1);
    }
    shades
}

/// The GUI's lambert, baked for `item_entity.vert`; held and dropped items
/// light by their normals instead (`item_entity_world.vert`).
fn apply_gui_lambert(quads: &mut [BakedQuad], display: DisplayTransform, gui_light: GuiLight) {
    let (l0, l1) = gui_lights(gui_light);
    for quad in quads {
        // ItemFeatureRenderer sends BakedQuad.direction() through the pose's
        // normal matrix. It does not apply BlockModelLighter's cardinal face
        // brightness in GUI rendering, so this replaces (rather than
        // multiplies) the terrain-oriented shade byte from bake_resolved_model.
        //
        // Not `quad.shade_face`: that is `None` for `shade: false` elements,
        // and `putBakedQuad` takes `direction()` regardless of `shade()`.
        let direction = direction_from_positions(&quad.positions).unwrap_or(Direction::Up);
        let n_mesh = direction.offset().map(|component| component as f32);
        let n_gui = transform_gui_normal(n_mesh, display);
        quad.shade_light = lambert_shade(n_gui, l0, l1);
    }
}

#[allow(clippy::too_many_arguments)]
fn add_chest_cube(
    quads: &mut Vec<BakedQuad>,
    x0: f32,
    y0: f32,
    z0: f32,
    x1: f32,
    y1: f32,
    z1: f32,
    u: f32,
    v: f32,
    w: f32,
    h: f32,
    d: f32,
    texture: &str,
    shades: [f32; 6],
) {
    const TEX_SIZE: f32 = 64.0;

    // Vanilla ModelPart.Cube names the z=min vertices `t*` and z=max
    // vertices `l*`, then builds faces in DOWN/UP/WEST/NORTH/EAST/SOUTH
    // order. Keep that layout exactly: chest items use the special entity
    // model rather than a normal block/item JSON mesh.
    let t0 = [x0, y0, z0];
    let t1 = [x1, y0, z0];
    let t2 = [x1, y1, z0];
    let t3 = [x0, y1, z0];
    let l0 = [x0, y0, z1];
    let l1 = [x1, y0, z1];
    let l2 = [x1, y1, z1];
    let l3 = [x0, y1, z1];

    let u0 = u;
    let u1 = u + d;
    let u2 = u + d + w;
    let u22 = u + d + w + w;
    let u3 = u + d + w + d;
    let u4 = u + d + w + d + w;
    let v0 = v;
    let v1 = v + d;
    let v2 = v + d + h;

    let face_specs = [
        FaceSpec {
            positions: [l1, l0, t0, t1],
            uv_pixels: [[u2, v0], [u1, v0], [u1, v1], [u2, v1]],
            shade: shades[1],
        },
        FaceSpec {
            positions: [t2, t3, l3, l2],
            uv_pixels: [[u22, v1], [u2, v1], [u2, v0], [u22, v0]],
            shade: shades[0],
        },
        FaceSpec {
            positions: [t0, l0, l3, t3],
            uv_pixels: [[u1, v1], [u0, v1], [u0, v2], [u1, v2]],
            shade: shades[4],
        },
        FaceSpec {
            positions: [t1, t0, t3, t2],
            uv_pixels: [[u2, v1], [u1, v1], [u1, v2], [u2, v2]],
            shade: shades[2],
        },
        FaceSpec {
            positions: [l1, t1, t2, l2],
            uv_pixels: [[u3, v1], [u2, v1], [u2, v2], [u3, v2]],
            shade: shades[5],
        },
        FaceSpec {
            positions: [l0, l1, l2, l3],
            uv_pixels: [[u4, v1], [u3, v1], [u3, v2], [u4, v2]],
            shade: shades[3],
        },
    ];

    for spec in face_specs {
        quads.push(BakedQuad {
            positions: spec.positions,
            uvs: spec
                .uv_pixels
                .map(|[u_px, v_px]| [u_px / TEX_SIZE, v_px / TEX_SIZE]),
            texture: TextureId::intern(texture),
            cullface: None,
            tint: super::render_assets::Tint::None,
            tint_index: None,
            shade_light: spec.shade,
            shade_face: None,
            emission: 0,
            ambient_occlusion: true,
        });
    }
}

struct FaceSpec {
    positions: [[f32; 3]; 4],
    uv_pixels: [[f32; 2]; 4],
    shade: f32,
}

struct ModelPart {
    path: String,
    transform: Option<Mat4>,
    tints: Vec<serde_json::Value>,
    /// A special model: `path` is its `base`, which carries the display
    /// transforms, and the mesh is the special's ([`SpecialModel`]).
    special: Option<SpecialModel>,
}

/// Model references to bake for one item. `minecraft:composite` contributes
/// every child, with the children's `transformation`s composed parent-to-child
/// (vanilla `CompositeModel.Unbaked.bake`); anything else keeps the old
/// first-model-string heuristic, which for select/condition trees picks one
/// representative state.
fn collect_model_parts(json: &serde_json::Value) -> Vec<ModelPart> {
    json.get("model").map_or_else(Vec::new, node_model_parts)
}

/// The parts a plain stack draws for item-model node `node`.
fn node_model_parts(node: &serde_json::Value) -> Vec<ModelPart> {
    node_model_parts_in(node, false)
}

/// [`node_model_parts`] in the GUI (`gui`) or anywhere else.
fn node_model_parts_in(node: &serde_json::Value, gui: bool) -> Vec<ModelPart> {
    let mut parts = Vec::new();
    let root = display_branch(node, gui);
    collect_parts_from_node(root, None, &mut parts);
    if !parts.is_empty() {
        return parts;
    }
    if let Some(node) = find_first_item_model_node(root)
        && let Some(path) = node.get("model").and_then(|value| value.as_str())
    {
        parts.push(ModelPart {
            path: strip_mc_prefix(path).to_string(),
            transform: None,
            tints: node
                .get("tints")
                .and_then(|value| value.as_array())
                .cloned()
                .unwrap_or_default(),
            special: None,
        });
    } else if let Some(path) = first_model_ref(root) {
        // A special renderer's `base` carries its transforms but no tints.
        parts.push(ModelPart {
            path,
            transform: None,
            tints: Vec::new(),
            special: None,
        });
    }
    parts
}

fn find_first_item_model_node(json: &serde_json::Value) -> Option<&serde_json::Value> {
    match json {
        serde_json::Value::Object(map) => {
            if map
                .get("type")
                .and_then(serde_json::Value::as_str)
                .map(strip_mc_prefix)
                == Some("model")
                && map
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .is_some()
            {
                return Some(json);
            }
            map.values().find_map(find_first_item_model_node)
        }
        serde_json::Value::Array(values) => values.iter().find_map(find_first_item_model_node),
        _ => None,
    }
}

/// The branch of an item definition's `model` node that a stack without
/// the components its `select`s read resolves to. Vanilla `SelectItemModel`
/// takes the `fallback` when no case matches, and `TrimMaterialProperty`
/// (no `trim`), `Charge` (no `charged_projectiles`, `NONE`),
/// `ItemBlockState` (no `block_state`) and `CustomModelDataProperty` (no
/// `custom_model_data`) never match for a plain stack.
///
/// A loaded crossbow's `charge_type` cases and a root `custom_model_data`
/// select's cases bake separately (see [`charge_model_key`] and
/// [`custom_model_key`]).
///
/// TODO: this branch still draws for loaded crossbows outside first person
/// and GUI icons (dropped items, third person, the bundle tooltip, chat
/// hovers), and for trimmed armor, which also needs the
/// `paletted_permutations` atlas source for its trim textures.
fn default_stack_branch(node: &serde_json::Value) -> &serde_json::Value {
    display_branch(node, false)
}

/// The key a composite model's variant for display context `context`
/// ([`DISPLAY_CONTEXT_KEYS`]) bakes under, where its parts' poses disagree
/// there.
pub fn context_model_key(key: &str, context: u8) -> String {
    format!("{key}@{context}")
}

/// Each display context in which a composite's parts (their quads from
/// each `part_display` start on) disagree, with the quads posed for it:
/// each part's quads carried from its own transform into the first part's,
/// which the view applies (`D_first⁻¹ · D_part` about the item's centre,
/// both mirrored in the left-hand views as they're drawn).
fn context_variants(
    quads: &[BakedQuad],
    part_display: &[(usize, DisplayTransforms)],
) -> Vec<(u8, Vec<BakedQuad>)> {
    let Some((_, first)) = part_display.first() else {
        return Vec::new();
    };
    let pose = |display: &DisplayTransforms, context: usize| {
        let transform = display[context].unwrap_or(DisplayTransform::IDENTITY);
        if matches!(context, 1 | 3) {
            transform.mirrored()
        } else {
            transform
        }
    };
    let ends: Vec<usize> = part_display
        .iter()
        .skip(1)
        .map(|(start, _)| *start)
        .chain(std::iter::once(quads.len()))
        .collect();
    let mut variants = Vec::new();
    for context in 1..DISPLAY_CONTEXT_KEYS.len() {
        let base = pose(first, context);
        if part_display
            .iter()
            .all(|(_, display)| pose(display, context) == base)
        {
            continue;
        }
        let Some(base_inverse) = Some(base.to_matrix()).filter(|m| m.determinant().abs() > 1e-9)
        else {
            continue;
        };
        let base_inverse = base_inverse.inverse();
        let mut posed = quads.to_vec();
        for ((start, display), end) in part_display.iter().zip(&ends) {
            let own = pose(display, context);
            if own == base {
                continue;
            }
            let m = Mat4::from_translation(Vec3::splat(0.5))
                * base_inverse
                * own.to_matrix()
                * Mat4::from_translation(Vec3::splat(-0.5));
            for quad in &mut posed[*start..*end] {
                for p in &mut quad.positions {
                    *p = m.transform_point3(Vec3::from_array(*p)).to_array();
                }
            }
        }
        variants.push((context as u8, posed));
    }
    variants
}

/// The conditions whose true side bakes a variant of its own: each
/// property and the suffix its key takes.
pub const CONDITION_VARIANTS: [(&str, &str); 2] = [
    // A stack in use: a raised shield, a drawn bow.
    ("using_item", "#using"),
    // A fishing rod whose angler has a line out.
    ("fishing_rod/cast", "#cast"),
];

/// Whether item-model node `node` draws differently while condition
/// `property` holds.
fn has_condition_variant(node: &serde_json::Value, property: &str) -> bool {
    match node {
        serde_json::Value::Object(map) => {
            (node_type(node) == Some("condition") && node_property(node) == Some(property))
                || map
                    .values()
                    .any(|child| has_condition_variant(child, property))
        }
        serde_json::Value::Array(items) => items
            .iter()
            .any(|child| has_condition_variant(child, property)),
        _ => false,
    }
}

/// The parts item-model node `node` draws while condition `property` holds.
fn node_model_parts_holding(node: &serde_json::Value, property: &str) -> Vec<ModelPart> {
    let mut parts = Vec::new();
    collect_parts_from_node_in(
        display_branch(node, false),
        None,
        &mut parts,
        Some(property),
    );
    parts
}

/// The key a model's GUI variant bakes under, where its definition draws
/// something else in the GUI (a `display_context` select's `gui` case).
pub fn gui_model_key(key: &str) -> String {
    format!("{key}#gui")
}

/// [`default_stack_branch`], also through `display_context` selects: their
/// `gui` case for the GUI (`gui`), their fallback everywhere else (held,
/// worn, dropped, displayed), as Void Rush's weapons draw a flat icon in
/// the inventory and a 3D model in the hand.
fn display_branch(mut node: &serde_json::Value, gui: bool) -> &serde_json::Value {
    loop {
        node = component_default_branch(node);
        if node_type(node) != Some("select") || node_property(node) != Some("display_context") {
            return node;
        }
        let gui_case = gui
            .then(|| {
                select_cases(node)
                    .into_iter()
                    .find(|(when, _)| strip_mc_prefix(when) == "gui")
                    .map(|(_, model)| model)
            })
            .flatten();
        match gui_case.or_else(|| node.get("fallback")) {
            Some(next) => node = next,
            None => return node,
        }
    }
}

fn component_default_branch(mut node: &serde_json::Value) -> &serde_json::Value {
    const COMPONENT_PROPERTIES: [&str; 4] = [
        "trim_material",
        "charge_type",
        "block_state",
        "custom_model_data",
    ];
    while node_type(node) == Some("select")
        && node_property(node).is_some_and(|property| COMPONENT_PROPERTIES.contains(&property))
        && let Some(fallback) = node.get("fallback")
    {
        node = fallback;
    }
    node
}

/// First `model` reference in an item definition, falling back to a special
/// renderer's `base`.
pub fn first_item_model_ref(json: &serde_json::Value) -> Option<String> {
    first_model_ref(json.get("model").map_or(json, default_stack_branch))
}

fn first_model_ref(node: &serde_json::Value) -> Option<String> {
    find_first_model_string(node)
        .or_else(|| find_first_string_for_key(node, "base"))
        .map(|path| strip_mc_prefix(&path).to_string())
}

/// An item-model node's `type`, without the `minecraft:` prefix.
fn node_type(node: &serde_json::Value) -> Option<&str> {
    node.get("type")
        .and_then(|t| t.as_str())
        .map(strip_mc_prefix)
}

/// The item-model key a root `minecraft:charge_type` select's case bakes
/// under, for a stack loaded with `charge` (vanilla `Charge`: `arrow` or
/// `rocket`).
pub fn charge_model_key(item_name: &str, charge: &str) -> String {
    format!("__pomme_{item_name}_charge_{charge}")
}

/// Each `when` of an item definition's root `minecraft:charge_type` select,
/// with its model node.
fn charge_type_cases(json: &serde_json::Value) -> Vec<(&str, &serde_json::Value)> {
    json.get("model")
        .filter(|root| {
            node_type(root) == Some("select") && node_property(root) == Some("charge_type")
        })
        .map_or_else(Vec::new, select_cases)
}

/// The item-model key a root `minecraft:custom_model_data` select's case
/// bakes under, for a stack whose first custom-model-data string is `when`.
pub fn custom_model_key(item_name: &str, when: &str) -> String {
    format!("__pomme_{item_name}_cmd_{when}")
}

/// Each `when` of an item definition's root `minecraft:custom_model_data`
/// select over the first string (vanilla `CustomModelDataProperty`, `index`
/// 0), with its model node.
fn custom_model_data_cases(json: &serde_json::Value) -> Vec<(&str, &serde_json::Value)> {
    json.get("model")
        .filter(|root| {
            node_type(root) == Some("select")
                && node_property(root) == Some("custom_model_data")
                && root
                    .get("index")
                    .is_none_or(|index| index.as_u64() == Some(0))
        })
        .map_or_else(Vec::new, select_cases)
}

/// Each `when` of select node `root` (a list `when` counts once per entry),
/// with its model node.
fn select_cases(root: &serde_json::Value) -> Vec<(&str, &serde_json::Value)> {
    let cases = root.get("cases").and_then(serde_json::Value::as_array);
    cases
        .into_iter()
        .flatten()
        .filter_map(|case| Some((case.get("when")?, case.get("model")?)))
        .flat_map(|(when, model)| {
            let whens = match when {
                serde_json::Value::Array(whens) => whens.iter().collect(),
                when => vec![when],
            };
            whens
                .into_iter()
                .filter_map(serde_json::Value::as_str)
                .map(move |when| (when, model))
        })
        .collect()
}

/// A select or condition node's `property`, without the `minecraft:` prefix.
/// Whether `node` draws a different model in the GUI than elsewhere.
fn has_gui_variant(node: &serde_json::Value) -> bool {
    !std::ptr::eq(display_branch(node, true), display_branch(node, false))
}

fn node_property(node: &serde_json::Value) -> Option<&str> {
    node.get("property")
        .and_then(serde_json::Value::as_str)
        .map(strip_mc_prefix)
}

/// The item-model key the bundle's selected layers bake under: behind the
/// selected stack, or in front of it.
pub fn selected_bundle_layer_key(item_name: &str, front: bool) -> String {
    let side = if front { "front" } else { "back" };
    format!("__pomme_{item_name}_selected_{side}")
}

/// The `on_true` of a `minecraft:bundle/has_selected_item` condition in an
/// item definition, split around its `minecraft:bundle/selected_item`
/// placeholder into the layers drawn behind and in front of the selected
/// stack (vanilla `BundleSelectedItemSpecialRenderer`).
fn selected_bundle_layers(json: &serde_json::Value) -> Option<[Vec<ModelPart>; 2]> {
    fn find_condition(node: &serde_json::Value) -> Option<&serde_json::Value> {
        let is_condition = node_type(node) == Some("condition")
            && node_property(node) == Some("bundle/has_selected_item");
        if is_condition {
            return Some(node);
        }
        match node {
            serde_json::Value::Object(map) => map.values().find_map(find_condition),
            serde_json::Value::Array(items) => items.iter().find_map(find_condition),
            _ => None,
        }
    }
    let on_true = find_condition(json.get("model")?)?.get("on_true")?;
    let children: Vec<&serde_json::Value> = match node_type(on_true) {
        Some("composite") => on_true.get("models")?.as_array()?.iter().collect(),
        _ => vec![on_true],
    };
    let split = children
        .iter()
        .position(|child| node_type(child) == Some("bundle/selected_item"))?;
    let transform = on_true
        .get("transformation")
        .and_then(parse_item_transformation);
    let mut layers = [Vec::new(), Vec::new()];
    for (i, child) in children.iter().enumerate() {
        if i != split {
            collect_parts_from_node(child, transform, &mut layers[usize::from(i > split)]);
        }
    }
    Some(layers)
}

fn collect_parts_from_node(
    node: &serde_json::Value,
    parent_transform: Option<Mat4>,
    parts: &mut Vec<ModelPart>,
) {
    collect_parts_from_node_in(node, parent_transform, parts, None);
}

/// [`collect_parts_from_node`] for a stack whose condition `holds` (a
/// property such as `using_item`) is true: it draws that condition's
/// `on_true` (a raised shield, a cast rod).
fn collect_parts_from_node_in(
    node: &serde_json::Value,
    parent_transform: Option<Mat4>,
    parts: &mut Vec<ModelPart>,
    holds: Option<&str>,
) {
    let own_transform = match node.get("transformation") {
        Some(value) => match parse_item_transformation(value) {
            Some(transform) => Some(transform),
            None => {
                tracing::warn!("Skipping item-model part with unsupported transformation encoding");
                return;
            }
        },
        None => None,
    };
    let transform = match (parent_transform, own_transform) {
        (Some(parent), Some(own)) => Some(parent * own),
        (parent, own) => parent.or(own),
    };
    match node_type(node) {
        Some("composite") => {
            if let Some(models) = node.get("models").and_then(|m| m.as_array()) {
                for child in models {
                    collect_parts_from_node_in(child, transform, parts, holds);
                }
            }
        }
        Some("model") => {
            if let Some(path) = node.get("model").and_then(|m| m.as_str()) {
                parts.push(ModelPart {
                    path: strip_mc_prefix(path).to_string(),
                    transform,
                    tints: node
                        .get("tints")
                        .and_then(|value| value.as_array())
                        .cloned()
                        .unwrap_or_default(),
                    special: None,
                });
            }
        }
        // A plain stack is in no condition (not in use, not cast, no
        // component), so it draws the `on_false` branch; one in use draws
        // a `using_item` condition's `on_true`.
        Some("condition") => {
            let side = if holds.is_some() && node_property(node) == holds {
                "on_true"
            } else {
                "on_false"
            };
            if let Some(branch) = node.get(side) {
                collect_parts_from_node_in(branch, transform, parts, holds);
            }
        }
        Some("special") => {
            if let Some(special) = node.get("model").and_then(SpecialModel::of)
                && let Some(base) = node.get("base").and_then(|b| b.as_str())
            {
                // A banner's base colour, as the tint its base pattern takes.
                let tints = match special {
                    SpecialModel::Banner { color } => {
                        vec![serde_json::json!({"type": "minecraft:constant", "value": color})]
                    }
                    _ => Vec::new(),
                };
                parts.push(ModelPart {
                    path: strip_mc_prefix(base).to_string(),
                    transform,
                    tints,
                    special: Some(special),
                });
            }
        }
        // Other node types are left for the caller's whole-file fallback.
        _ => {}
    }
}

/// Vanilla `Transformation.compose` (Transformation.java:103): `translation ·
/// leftRotation · scale · rightRotation`, translation in block units. Pomme
/// currently supports the stock quaternion-array encoding. Other valid codec
/// forms are rejected by the caller instead of silently becoming identity.
fn parse_item_transformation(json: &serde_json::Value) -> Option<Mat4> {
    let object = json.as_object()?;
    let quat = |key: &str| -> Option<Quat> {
        let Some(value) = object.get(key) else {
            return Some(Quat::IDENTITY);
        };
        let arr = value.as_array()?;
        if arr.len() != 4 {
            return None;
        }
        let get = |i: usize| arr.get(i)?.as_f64().map(|v| v as f32);
        Some(Quat::from_xyzw(get(0)?, get(1)?, get(2)?, get(3)?))
    };
    let vec3 = |key: &str, default: Vec3| {
        object.get(key).map_or(Some(default), |v| {
            v.as_array().and_then(|arr| {
                if arr.len() != 3 {
                    return None;
                }
                Some(Vec3::new(
                    arr[0].as_f64()? as f32,
                    arr[1].as_f64()? as f32,
                    arr[2].as_f64()? as f32,
                ))
            })
        })
    };
    Some(
        Mat4::from_translation(vec3("translation", Vec3::ZERO)?)
            * Mat4::from_quat(quat("left_rotation")?)
            * Mat4::from_scale(vec3("scale", Vec3::ONE)?)
            * Mat4::from_quat(quat("right_rotation")?),
    )
}

pub fn parse_vec3(value: &serde_json::Value, default: Vec3) -> Vec3 {
    let Some(arr) = value.as_array() else {
        return default;
    };
    let get = |i: usize| arr.get(i).and_then(|v| v.as_f64()).map(|v| v as f32);
    Vec3::new(
        get(0).unwrap_or(default.x),
        get(1).unwrap_or(default.y),
        get(2).unwrap_or(default.z),
    )
}

pub fn strip_mc_prefix(s: &str) -> &str {
    s.strip_prefix("minecraft:").unwrap_or(s)
}

pub fn find_first_model_string(json: &serde_json::Value) -> Option<String> {
    find_first_string_for_key(json, "model")
}

pub fn find_first_string_for_key(json: &serde_json::Value, key: &str) -> Option<String> {
    match json {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(s)) = map.get(key) {
                return Some(s.clone());
            }
            for v in map.values() {
                if let Some(r) = find_first_string_for_key(v, key) {
                    return Some(r);
                }
            }
            None
        }
        serde_json::Value::Array(arr) => arr.iter().find_map(|v| find_first_string_for_key(v, key)),
        _ => None,
    }
}

fn parse_when_condition(when: &Option<serde_json::Value>) -> HashMap<String, String> {
    let mut result = HashMap::new();
    if let Some(serde_json::Value::Object(map)) = when {
        for (key, value) in map {
            if let serde_json::Value::String(s) = value {
                result.insert(key.clone(), s.clone());
            } else if let serde_json::Value::Bool(b) = value {
                result.insert(key.clone(), b.to_string());
            }
        }
    }
    result
}

fn for_each_blockstate(
    jar_assets_dir: &Path,
    asset_index: &Option<AssetIndex>,
    packs: Option<&crate::resource_pack::ResourcePackManager>,
    mut callback: impl FnMut(&str, &BlockstateFile) -> Option<()>,
) {
    let Some(blockstates_dir) = resolve_blockstates_dir(jar_assets_dir, asset_index, packs) else {
        tracing::warn!("Blockstates directory not found");
        return;
    };

    let entries = match std::fs::read_dir(&blockstates_dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("Failed to read blockstates dir: {e}");
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }

        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(blockstate) = serde_json::from_str::<BlockstateFile>(&contents) else {
            continue;
        };

        callback(name, &blockstate);
    }
}

fn resolve_blockstates_dir(
    jar_assets_dir: &Path,
    asset_index: &Option<AssetIndex>,
    packs: Option<&crate::resource_pack::ResourcePackManager>,
) -> Option<PathBuf> {
    let candidates = [
        jar_assets_dir.join("assets/minecraft/blockstates"),
        jar_assets_dir.join("jar/assets/minecraft/blockstates"),
        PathBuf::from("reference/assets/assets/minecraft/blockstates"),
    ];

    for c in &candidates {
        if c.is_dir() {
            return Some(c.clone());
        }
    }

    // Also check the original simple path
    let path = jar_assets_dir.join("minecraft/blockstates");
    if path.is_dir() {
        return Some(path);
    }

    if asset_index.is_some() {
        let test_path = resolve_asset_path_with_packs(
            jar_assets_dir,
            asset_index,
            "minecraft/blockstates/stone.json",
            packs,
        );
        if test_path.exists() {
            return test_path.parent().map(|p| p.to_path_buf());
        }
    }

    None
}

fn extract_default_model_ref(blockstate: &BlockstateFile) -> Option<ModelRef> {
    if let Some(variants) = &blockstate.variants {
        let entry = variants.get("").or_else(|| variants.values().next())?;
        let r = entry.first()?;
        Some(ModelRef {
            model: r.model.clone(),
            x: r.x,
            y: r.y,
            uvlock: r.uvlock,
        })
    } else if let Some(multipart) = &blockstate.multipart {
        let r = multipart.first()?.apply.first()?;
        Some(ModelRef {
            model: r.model.clone(),
            x: r.x,
            y: r.y,
            uvlock: r.uvlock,
        })
    } else {
        None
    }
}

struct ResolvedModel {
    textures: HashMap<String, String>,
    elements: Vec<ElementDef>,
    /// Vanilla `BlockModel.getAmbientOcclusion`: the nearest model in the
    /// parent chain that sets it, else true.
    ambient_occlusion: bool,
    gui_transform: DisplayTransform,
    gui_light: GuiLight,
    ground_transform: Mat4,
    display: DisplayTransforms,
}

fn resolve_model(
    model_id: &str,
    jar_assets_dir: &Path,
    asset_index: &Option<AssetIndex>,
    cache: &mut HashMap<String, ModelFile>,
    packs: Option<&crate::resource_pack::ResourcePackManager>,
) -> ResolvedModel {
    let mut texture_map: HashMap<String, String> = HashMap::new();
    let mut elements: Option<Vec<ElementDef>> = None;
    let mut gui_transform: Option<DisplayTransform> = None;
    let mut gui_light: Option<GuiLight> = None;
    let mut ground_transform: Option<Mat4> = None;
    let mut ambient_occlusion: Option<bool> = None;
    let mut display: DisplayTransforms = [None; 10];
    let mut current_id = model_id.to_string();

    for _ in 0..20 {
        let Some(model) = load_model(&current_id, jar_assets_dir, asset_index, cache, packs) else {
            break;
        };

        for (key, value) in &model.textures {
            texture_map
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }

        if elements.is_none() && !model.elements.is_empty() {
            elements = Some(model.elements.clone());
        }
        if gui_transform.is_none()
            && let Some(transform) = model.display.get("gui").and_then(parse_display_transform)
        {
            gui_transform = Some(transform);
        }
        if gui_light.is_none() {
            gui_light = model.gui_light;
        }
        for (slot, keys) in display.iter_mut().zip(DISPLAY_CONTEXT_KEYS) {
            if slot.is_none() {
                *slot = keys
                    .iter()
                    .find_map(|key| model.display.get(*key).and_then(parse_display_transform));
            }
        }
        if ambient_occlusion.is_none() {
            ambient_occlusion = model.ambientocclusion;
        }
        if ground_transform.is_none()
            && let Some(transform) = model
                .display
                .get("ground")
                .and_then(parse_display_transform)
        {
            ground_transform = Some(transform.to_matrix());
        }

        match &model.parent {
            Some(parent) => current_id = parent.clone(),
            None => break,
        }
    }

    let mut resolved_textures = HashMap::new();
    for (key, value) in &texture_map {
        resolved_textures.insert(key.clone(), resolve_ref(value, &texture_map, 0));
    }

    ResolvedModel {
        textures: resolved_textures,
        elements: elements.unwrap_or_default(),
        ambient_occlusion: ambient_occlusion.unwrap_or(true),
        gui_transform: gui_transform.unwrap_or(DisplayTransform::IDENTITY),
        gui_light: gui_light.unwrap_or_default(),
        ground_transform: ground_transform.unwrap_or(Mat4::IDENTITY),
        display,
    }
}

fn resolve_ref(value: &str, map: &HashMap<String, String>, depth: u32) -> String {
    if depth > 10 {
        return value.to_string();
    }
    if let Some(ref_name) = value.strip_prefix('#')
        && let Some(target) = map.get(ref_name)
    {
        return resolve_ref(target, map, depth + 1);
    }
    value.to_string()
}

fn load_model<'a>(
    model_id: &str,
    jar_assets_dir: &Path,
    asset_index: &Option<AssetIndex>,
    cache: &'a mut HashMap<String, ModelFile>,
    packs: Option<&crate::resource_pack::ResourcePackManager>,
) -> Option<&'a ModelFile> {
    if cache.contains_key(model_id) {
        return cache.get(model_id);
    }

    let asset_key = model_id_to_asset_key(model_id);
    let file_path = resolve_model_path(jar_assets_dir, asset_index, &asset_key, packs)?;

    let contents = std::fs::read_to_string(&file_path).ok()?;
    let model: ModelFile = match serde_json::from_str(&contents) {
        Ok(model) => model,
        Err(error) => {
            // Vanilla rejects the model too, but it says so rather than going quiet.
            tracing::warn!("{model_id}: model JSON is invalid ({error}); skipping");
            return None;
        }
    };
    cache.insert(model_id.to_string(), model);
    cache.get(model_id)
}

fn resolve_model_path(
    jar_assets_dir: &Path,
    asset_index: &Option<AssetIndex>,
    asset_key: &str,
    packs: Option<&crate::resource_pack::ResourcePackManager>,
) -> Option<PathBuf> {
    let primary = resolve_asset_path_with_packs(jar_assets_dir, asset_index, asset_key, packs);
    if primary.exists() {
        return Some(primary);
    }

    let ref_path = Path::new("reference/assets/assets")
        .join(asset_key.strip_prefix("minecraft/").unwrap_or(asset_key));
    if ref_path.exists() {
        return Some(ref_path);
    }

    None
}

fn model_id_to_asset_key(model_id: &str) -> String {
    AssetId::parse(model_id).asset_key("models", ".json")
}

fn texture_to_name(texture_ref: &str) -> Option<String> {
    if texture_ref.starts_with('#') {
        return None;
    }
    let id = AssetId::parse(texture_ref);
    if id.namespace == "minecraft" {
        if let Some(block_path) = id.path.strip_prefix("block/") {
            Some(block_path.to_string())
        } else if id.path.starts_with("item/")
            || id.path.starts_with("entity/")
            || id.path.starts_with("particle/")
        {
            Some(id.path.to_string())
        } else {
            Some(format!("minecraft:{}", id.path))
        }
    } else {
        Some(id.canonical())
    }
}

fn resolve_face_texture<'a>(
    reference: &str,
    textures: &'a HashMap<String, String>,
) -> Option<&'a str> {
    let slot = reference.strip_prefix('#').unwrap_or(reference);
    textures.get(slot).map(String::as_str)
}

fn bake_resolved_model(
    resolved: &ResolvedModel,
    rot_x: i32,
    rot_y: i32,
    tint: super::render_assets::Tint,
) -> Option<BakedModel> {
    if resolved.elements.is_empty() {
        return None;
    }

    let mut quads = Vec::new();

    for element in &resolved.elements {
        let from = [
            element.from[0] / 16.0,
            element.from[1] / 16.0,
            element.from[2] / 16.0,
        ];
        let to = [
            element.to[0] / 16.0,
            element.to[1] / 16.0,
            element.to[2] / 16.0,
        ];

        for (face_name, face_def) in &element.faces {
            let Some(dir) = Direction::from_str(face_name) else {
                continue;
            };

            let Some(texture_ref) = resolve_face_texture(&face_def.texture, &resolved.textures)
            else {
                continue;
            };
            let Some(texture_name) = texture_to_name(texture_ref) else {
                continue;
            };

            let positions = face_positions(dir, from, to);
            let mut uvs = face_uvs(dir, from, to, face_def.uv.as_ref(), face_def.rotation);

            let mut positions = apply_element_rotation(positions, &element.rotation);

            let mut cullface = face_def.cullface.as_deref().and_then(Direction::from_str);
            let quad_tint = if face_def.tint_index.is_some() {
                tint
            } else {
                super::render_assets::Tint::None
            };

            if rot_x != 0 || rot_y != 0 {
                positions = rotate_positions(positions, rot_x, rot_y);
                cullface = cullface.map(|d| d.rotate_x(rot_x).rotate_y(rot_y));
            }
            if element.rotation.is_none()
                && let Some(facing) = direction_from_positions(&positions)
            {
                recalculate_winding(&mut positions, &mut uvs, facing);
            }

            // Vanilla `FaceBakery.bakeQuad`: the shade direction is the
            // rotated quad's nearest cardinal, `UP` for a degenerate quad.
            let shade_face = element
                .shade
                .then(|| direction_from_positions(&positions).unwrap_or(Direction::Up));

            quads.push(BakedQuad {
                positions,
                uvs,
                texture: TextureId::intern(&texture_name),
                cullface,
                tint: quad_tint,
                tint_index: face_def
                    .tint_index
                    .and_then(|index| u32::try_from(index).ok()),
                shade_light: shade_face.map_or(1.0, |face| face.shade_light()),
                shade_face,
                emission: element.light_emission,
                ambient_occlusion: resolved.ambient_occlusion,
            });
        }
    }

    if quads.is_empty() {
        return None;
    }

    Some(BakedModel {
        is_full_cube: check_full_cube(&quads),
        quads,
    })
}

fn check_full_cube(quads: &[BakedQuad]) -> bool {
    if quads.len() != 6 {
        return false;
    }
    let mut dirs = [false; 6];
    for q in quads {
        match q.cullface {
            Some(Direction::Down) => dirs[0] = true,
            Some(Direction::Up) => dirs[1] = true,
            Some(Direction::North) => dirs[2] = true,
            Some(Direction::South) => dirs[3] = true,
            Some(Direction::West) => dirs[4] = true,
            Some(Direction::East) => dirs[5] = true,
            None => return false,
        }
    }
    dirs.iter().all(|&d| d)
}

/// Vanilla `FaceInfo`: the per-face vertex order all the UV rules are defined
/// against. Every face winds CCW viewed from outside, which the chunk
/// pipeline's backface culling needs.
pub(crate) fn face_positions(dir: Direction, from: [f32; 3], to: [f32; 3]) -> [[f32; 3]; 4] {
    let [x0, y0, z0] = from;
    let [x1, y1, z1] = to;
    match dir {
        Direction::Down => [[x0, y0, z1], [x0, y0, z0], [x1, y0, z0], [x1, y0, z1]],
        Direction::Up => [[x0, y1, z0], [x0, y1, z1], [x1, y1, z1], [x1, y1, z0]],
        Direction::North => [[x1, y1, z0], [x1, y0, z0], [x0, y0, z0], [x0, y1, z0]],
        Direction::South => [[x0, y1, z1], [x0, y0, z1], [x1, y0, z1], [x1, y1, z1]],
        Direction::West => [[x0, y1, z0], [x0, y0, z0], [x0, y0, z1], [x0, y1, z1]],
        Direction::East => [[x1, y1, z1], [x1, y0, z1], [x1, y0, z0], [x1, y1, z0]],
    }
}

pub(crate) fn face_uvs(
    dir: Direction,
    from: [f32; 3],
    to: [f32; 3],
    explicit_uv: Option<&[f32; 4]>,
    rotation: Option<i32>,
) -> [[f32; 2]; 4] {
    // Vanilla `FaceBakery.defaultFaceUV`, normalized to 0..1: some faces
    // sample a window reflected about the texture center.
    let (u1, v1, u2, v2) = if let Some(uv) = explicit_uv {
        (uv[0] / 16.0, uv[1] / 16.0, uv[2] / 16.0, uv[3] / 16.0)
    } else {
        match dir {
            Direction::Down => (from[0], 1.0 - to[2], to[0], 1.0 - from[2]),
            Direction::Up => (from[0], from[2], to[0], to[2]),
            Direction::North => (1.0 - to[0], 1.0 - to[1], 1.0 - from[0], 1.0 - from[1]),
            Direction::South => (from[0], 1.0 - to[1], to[0], 1.0 - from[1]),
            Direction::West => (from[2], 1.0 - to[1], to[2], 1.0 - from[1]),
            Direction::East => (1.0 - to[2], 1.0 - to[1], 1.0 - from[2], 1.0 - from[1]),
        }
    };

    // Vanilla `CuboidFace.UVs` corner cycle, identical for every face (the
    // per-face variation lives in `face_positions`' vertex order).
    // `Quadrant.rotateVertexIndex` shifts each vertex forward through the
    // cycle, spinning the texture clockwise per 90 degrees viewed from
    // outside the block.
    let cycle = [[u1, v1], [u1, v2], [u2, v2], [u2, v1]];
    let shift = rotation.map_or(0, |r| {
        if r % 90 != 0 {
            // Vanilla `Quadrant.parseJson` rejects the whole model instead.
            tracing::warn!("face uv rotation {r} is not a multiple of 90, truncating");
        }
        r.rem_euclid(360) / 90
    }) as usize;
    std::array::from_fn(|i| cycle[(i + shift) % 4])
}

fn apply_element_rotation(
    mut positions: [[f32; 3]; 4],
    rotation: &Option<ElementRotation>,
) -> [[f32; 3]; 4] {
    let Some(rot) = rotation else {
        return positions;
    };
    let origin = Vec3::from_array(rot.origin) / 16.0;
    let matrix = rot.matrix();
    for pos in &mut positions {
        *pos = (origin + matrix * (Vec3::from_array(*pos) - origin)).to_array();
    }
    positions
}

/// The quad's winding normal, or `None` when it is degenerate.
pub(crate) fn quad_normal(positions: &[[f32; 3]; 4]) -> Option<Vec3> {
    let p0 = Vec3::from_array(positions[0]);
    let p1 = Vec3::from_array(positions[1]);
    let p2 = Vec3::from_array(positions[2]);
    (p1 - p0).cross(p2 - p0).try_normalize()
}

/// Vanilla `FaceBakery.findClosestDirection`: the cardinal with the largest
/// positive dot product, first in `Direction` order on a tie; `None` when the
/// normal points nowhere.
pub(crate) fn nearest_cardinal_direction(normal: Vec3) -> Option<Direction> {
    let mut best = None;
    let mut closest_product = 0.0f32;
    for candidate in [
        Direction::Down,
        Direction::Up,
        Direction::North,
        Direction::South,
        Direction::West,
        Direction::East,
    ] {
        let product = normal.dot(Vec3::from_array(candidate.offset().map(|v| v as f32)));
        if product >= 0.0 && product > closest_product {
            closest_product = product;
            best = Some(candidate);
        }
    }
    best
}

pub(crate) fn direction_from_positions(positions: &[[f32; 3]; 4]) -> Option<Direction> {
    nearest_cardinal_direction(quad_normal(positions)?)
}

/// A quad's smallest and largest corner coordinates, per axis.
pub(crate) fn quad_bounds(positions: &[[f32; 3]; 4]) -> ([f32; 3], [f32; 3]) {
    let mut min = [f32::MAX; 3];
    let mut max = [f32::MIN; 3];
    for p in positions {
        for i in 0..3 {
            min[i] = min[i].min(p[i]);
            max[i] = max[i].max(p[i]);
        }
    }
    (min, max)
}

/// Vanilla `FaceBakery.recalculateWinding`: a rotated face's corners (and
/// their uvs) back in `facing`'s vertex order, which per-vertex lighting
/// relies on.
fn recalculate_winding(positions: &mut [[f32; 3]; 4], uvs: &mut [[f32; 2]; 4], facing: Direction) {
    let (min, max) = quad_bounds(positions);
    let target = face_positions(facing, min, max);
    for (vertex, &corner) in target.iter().enumerate() {
        let Some(found) = (vertex..4).find(|&i| positions[i] == corner) else {
            return;
        };
        positions.swap(vertex, found);
        uvs.swap(vertex, found);
    }
}

/// A blockstate's `x` then `y` rotation, in exact quarter turns about the
/// block centre (vanilla's are only ever multiples of 90), so corners on the
/// block's edge stay exactly on it.
fn rotate_positions(mut positions: [[f32; 3]; 4], rot_x: i32, rot_y: i32) -> [[f32; 3]; 4] {
    let quarter_turns = |degrees: i32| degrees.rem_euclid(360) / 90;
    for pos in &mut positions {
        for _ in 0..quarter_turns(rot_x) {
            *pos = [pos[0], pos[2], 1.0 - pos[1]];
        }
        for _ in 0..quarter_turns(rot_y) {
            *pos = [1.0 - pos[2], pos[1], pos[0]];
        }
    }
    positions
}

fn build_face_textures(
    block_name: &str,
    textures: &HashMap<String, String>,
) -> Option<FaceTextures> {
    let mut faces = face_textures_base(block_name, textures)?;
    faces.particle = textures.get("particle").and_then(|v| texture_to_name(v));
    Some(faces)
}

fn face_textures_base(
    block_name: &str,
    textures: &HashMap<String, String>,
) -> Option<FaceTextures> {
    let get = |key: &str| -> Option<String> { textures.get(key).and_then(|v| texture_to_name(v)) };

    let (up, down, north, south, east, west) = (
        get("up"),
        get("down"),
        get("north"),
        get("south"),
        get("east"),
        get("west"),
    );

    let tint = determine_tint(block_name, 0);

    if let (Some(up), Some(down), Some(north), Some(south), Some(east), Some(west)) =
        (up, down, north, south, east, west)
    {
        let (side_overlay, tint) = if block_name == "grass_block" {
            (Some("grass_block_side_overlay"), Tint::Grass)
        } else {
            (None, tint)
        };
        return Some(FaceTextures::new(
            &up,
            &down,
            &north,
            &south,
            &east,
            &west,
            side_overlay,
            tint,
        ));
    }

    if let Some(all) = get("all") {
        return Some(FaceTextures::uniform(&all, tint));
    }

    if let (Some(end), Some(side)) = (get("end"), get("side")) {
        return Some(FaceTextures::new(
            &end,
            &end,
            &side,
            &side,
            &side,
            &side,
            None,
            Tint::None,
        ));
    }

    if let (Some(top), Some(side)) = (get("top"), get("side")) {
        let bottom = get("bottom").unwrap_or_else(|| top.clone());
        return Some(FaceTextures::new(
            &top, &bottom, &side, &side, &side, &side, None, tint,
        ));
    }

    if let Some(cross) = get("cross") {
        return Some(FaceTextures::uniform(&cross, tint));
    }

    if let (Some(front), Some(side)) = (get("front"), get("side")) {
        let top = get("top")
            .or_else(|| get("end"))
            .unwrap_or_else(|| side.clone());
        let bottom = get("bottom").unwrap_or_else(|| top.clone());
        return Some(FaceTextures::new(
            &top,
            &bottom,
            &front,
            &side,
            &side,
            &side,
            None,
            Tint::None,
        ));
    }

    if let Some(p) = get("particle") {
        return Some(FaceTextures::uniform(&p, tint));
    }

    None
}

fn apply_block_tints(model: &mut BakedModel, block_name: &str) {
    for quad in &mut model.quads {
        quad.tint = quad
            .tint_index
            .map_or(Tint::None, |index| determine_tint(block_name, index));
    }
}

/// Vanilla 26.2 `BlockColors.createDefault`, indexed by model `tintindex`.
///
/// Most blocks use layer 0. Pink petals and wildflowers intentionally register
/// a blank layer 0 and grass on layer 1, so tint selection must be per quad.
fn determine_tint(block_name: &str, tint_index: u32) -> Tint {
    match (block_name, tint_index) {
        ("redstone_wire", 0) => Tint::Redstone,

        ("large_fern" | "tall_grass", 0) => Tint::DoubleGrass,
        ("fern" | "short_grass" | "potted_fern" | "bush" | "grass_block" | "sugar_cane", 0) => {
            Tint::Grass
        }
        ("pink_petals" | "wildflowers", 1) => Tint::Grass,

        ("spruce_leaves", 0) => Tint::Constant(0x619961),
        ("birch_leaves", 0) => Tint::Constant(0x80A755),
        (
            "oak_leaves" | "jungle_leaves" | "acacia_leaves" | "dark_oak_leaves" | "vine"
            | "mangrove_leaves",
            0,
        ) => Tint::Foliage,
        ("leaf_litter", 0) => Tint::DryFoliage,

        ("water_cauldron", 0) => Tint::Water,
        ("attached_melon_stem" | "attached_pumpkin_stem", 0) => Tint::Constant(0xE0C71C),
        ("melon_stem" | "pumpkin_stem", 0) => Tint::Stem,
        ("lily_pad", 0) => Tint::Constant(0x208030),

        _ => Tint::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_tints(values: &[serde_json::Value]) -> Result<Vec<ItemTintSource>, String> {
        parse_item_tint_sources(values, &Colormap::fallback())
    }

    const DIRS: [Direction; 6] = [
        Direction::Down,
        Direction::Up,
        Direction::North,
        Direction::South,
        Direction::West,
        Direction::East,
    ];

    /// The (right, up) axes of each face viewed from outside the block:
    /// sky-up for the sides, and vanilla's map orientation for up/down.
    /// `right x up` is the outward normal.
    fn face_axes(dir: Direction) -> (Vec3, Vec3) {
        match dir {
            Direction::Down => (Vec3::X, Vec3::Z),
            Direction::Up => (Vec3::X, Vec3::NEG_Z),
            Direction::North => (Vec3::NEG_X, Vec3::Y),
            Direction::South => (Vec3::X, Vec3::Y),
            Direction::West => (Vec3::Z, Vec3::Y),
            Direction::East => (Vec3::NEG_Z, Vec3::Y),
        }
    }

    /// Quads must stay CCW viewed from outside for backface culling.
    #[test]
    fn face_winding_is_ccw_from_outside() {
        for dir in DIRS {
            let p = face_positions(dir, [0.0; 3], [1.0; 3]).map(Vec3::from_array);
            let normal = (p[1] - p[0]).cross(p[2] - p[0]);
            let outward = Vec3::from_array(dir.offset().map(|c| c as f32));
            assert!(normal.dot(outward) > 0.0, "{dir:?} winds the wrong way");
            assert_eq!(
                direction_from_positions(&p.map(|position| position.to_array())),
                Some(dir)
            );
        }
    }

    #[test]
    fn items_3d_lights_match_vanilla_pose_order() {
        let (light0, light1) = items_3d_lights();
        let expected = [
            (light0, [-0.9334393, -0.26269472, -0.24430019]),
            (light1, [-0.10357136, -0.97660685, 0.18844643]),
        ];
        for (actual, expected) in expected {
            for (actual, expected) in actual.into_iter().zip(expected) {
                assert!(
                    (actual - expected).abs() < 1.0e-6,
                    "expected {expected}, got {actual}"
                );
            }
        }
    }

    #[test]
    fn items_flat_lights_match_vanilla_pose_order() {
        let (light0, light1) = items_flat_lights();
        let expected = [
            (light0, [-0.222519, -0.17149863, 0.9597257]),
            (light1, [-0.21501213, -0.97182524, 0.0965678]),
        ];
        for (actual, expected) in expected {
            for (actual, expected) in actual.into_iter().zip(expected) {
                assert!(
                    (actual - expected).abs() < 1.0e-6,
                    "expected {expected}, got {actual}"
                );
            }
        }
    }

    #[test]
    fn gui_face_shades_include_vanilla_atlas_y_flip() {
        let block = vanilla_gui_face_shades(gui_transform(225.0), GuiLight::Side);
        let chest = vanilla_gui_face_shades(gui_transform(45.0), GuiLight::Side);
        let expected_block = [1.0, 0.4, 0.4, 1.0, 0.49398834, 0.6505372];
        let expected_chest = [1.0, 0.4, 1.0, 0.4, 0.6505372, 0.49398834];
        for (actual, expected) in block.into_iter().zip(expected_block) {
            assert!(
                (actual - expected).abs() < 1.0e-6,
                "expected {expected}, got {actual}"
            );
        }
        for (actual, expected) in chest.into_iter().zip(expected_chest) {
            assert!(
                (actual - expected).abs() < 1.0e-6,
                "expected {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn gui_face_shades_follow_model_display_transform() {
        let fence = vanilla_gui_face_shades(gui_transform(135.0), GuiLight::Side);
        let expected = [1.0, 0.4, 0.65053713, 0.49398836, 0.4, 1.0];
        for (actual, expected) in fence.into_iter().zip(expected) {
            assert!(
                (actual - expected).abs() < 1.0e-6,
                "expected {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn gui_lambert_replaces_terrain_cardinal_shade() {
        let mut quad = BakedQuad {
            positions: face_positions(Direction::Up, [0.0; 3], [1.0; 3]),
            uvs: [[0.0; 2]; 4],
            texture: TextureId::intern("block/stone"),
            cullface: None,
            tint: Tint::None,
            tint_index: None,
            shade_light: 0.25,
            shade_face: Some(Direction::Up),
            emission: 0,
            ambient_occlusion: true,
        };
        apply_gui_lambert(
            std::slice::from_mut(&mut quad),
            gui_transform(225.0),
            GuiLight::Side,
        );
        assert!((quad.shade_light - 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn chest_body_faces_match_vanilla_modelpart_cube() {
        let model =
            bake_chest_item_model("entity/chest/normal", gui_transform(45.0), GuiLight::Side);
        assert_eq!(model.quads.len(), 18);

        let x0 = 1.0 / 16.0;
        let x1 = 15.0 / 16.0;
        let y0 = 0.0;
        let y1 = 10.0 / 16.0;
        let z0 = 1.0 / 16.0;
        let z1 = 15.0 / 16.0;
        let t0 = [x0, y0, z0];
        let t1 = [x1, y0, z0];
        let t2 = [x1, y1, z0];
        let t3 = [x0, y1, z0];
        let l0 = [x0, y0, z1];
        let l1 = [x1, y0, z1];
        let l2 = [x1, y1, z1];
        let l3 = [x0, y1, z1];

        let expected = [
            (
                [l1, l0, t0, t1],
                [[28.0, 19.0], [14.0, 19.0], [14.0, 33.0], [28.0, 33.0]],
            ),
            (
                [t2, t3, l3, l2],
                [[42.0, 33.0], [28.0, 33.0], [28.0, 19.0], [42.0, 19.0]],
            ),
            (
                [t0, l0, l3, t3],
                [[14.0, 33.0], [0.0, 33.0], [0.0, 43.0], [14.0, 43.0]],
            ),
            (
                [t1, t0, t3, t2],
                [[28.0, 33.0], [14.0, 33.0], [14.0, 43.0], [28.0, 43.0]],
            ),
            (
                [l1, t1, t2, l2],
                [[42.0, 33.0], [28.0, 33.0], [28.0, 43.0], [42.0, 43.0]],
            ),
            (
                [l0, l1, l2, l3],
                [[56.0, 33.0], [42.0, 33.0], [42.0, 43.0], [56.0, 43.0]],
            ),
        ];

        for (quad, (positions, uv_pixels)) in model.quads.iter().take(6).zip(expected) {
            assert_eq!(quad.positions, positions);
            assert_eq!(quad.uvs, uv_pixels.map(|[u, v]| [u / 64.0, v / 64.0]));
        }
    }

    #[test]
    fn model_rotation_uses_final_face_direction_for_cardinal_shading() {
        let face = FaceDef {
            uv: Some([0.0, 0.0, 16.0, 16.0]),
            texture: "side".to_string(),
            cullface: None,
            rotation: None,
            tint_index: None,
        };
        let resolved = ResolvedModel {
            textures: HashMap::from([("side".to_string(), "block/piston_side".to_string())]),
            elements: vec![ElementDef {
                from: [6.0, 6.0, 4.0],
                to: [10.0, 10.0, 20.0],
                rotation: None,
                faces: HashMap::from([("west".to_string(), face)]),
                shade: true,
                light_emission: 0,
            }],
            ambient_occlusion: true,
            gui_transform: DisplayTransform::IDENTITY,
            gui_light: GuiLight::Side,
            ground_transform: Mat4::IDENTITY,
            display: [None; 10],
        };

        let baked = bake_resolved_model(&resolved, 0, 270, Tint::None).unwrap();
        assert_eq!(baked.quads.len(), 1);
        assert!((baked.quads[0].shade_light - Direction::South.shade_light()).abs() < 1.0e-6);
    }

    #[test]
    fn baked_quad_preserves_raw_tint_index() {
        let face = FaceDef {
            uv: Some([0.0, 0.0, 16.0, 16.0]),
            texture: "all".to_string(),
            cullface: None,
            rotation: None,
            tint_index: Some(1),
        };
        let resolved = ResolvedModel {
            textures: HashMap::from([("all".to_string(), "item/test".to_string())]),
            elements: vec![ElementDef {
                from: [0.0, 0.0, 0.0],
                to: [16.0, 16.0, 16.0],
                rotation: None,
                faces: HashMap::from([("south".to_string(), face)]),
                shade: true,
                light_emission: 0,
            }],
            ambient_occlusion: true,
            gui_transform: DisplayTransform::IDENTITY,
            gui_light: GuiLight::default(),
            ground_transform: Mat4::IDENTITY,
            display: [None; 10],
        };
        let baked = bake_resolved_model(&resolved, 0, 0, Tint::None).unwrap();
        assert_eq!(baked.quads[0].tint_index, Some(1));
    }

    #[test]
    fn euler_rotations_turn_x_then_y_then_z() {
        // 26.x's hanging signs at 22.5-degree steps rotate this way.
        let euler: ElementRotation =
            serde_json::from_str(r#"{"x": 0, "y": 45, "z": 0, "origin": [8, 0, 8]}"#).unwrap();
        let single: ElementRotation =
            serde_json::from_str(r#"{"axis": "y", "angle": 45, "origin": [8, 0, 8]}"#).unwrap();
        let quad = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 0.5],
        ];
        let (a, b) = (
            apply_element_rotation(quad, &Some(euler)),
            apply_element_rotation(quad, &Some(single)),
        );
        for (a, b) in a.iter().zip(b) {
            assert!(Vec3::from_array(*a).abs_diff_eq(Vec3::from_array(b), 1e-6));
        }
        // x 90 then y 90: +y goes to +z, then to +x.
        let both: ElementRotation =
            serde_json::from_str(r#"{"x": 90, "y": 90, "origin": [0, 0, 0]}"#).unwrap();
        let up = apply_element_rotation([[0.0, 1.0, 0.0]; 4], &Some(both))[0];
        assert!(Vec3::from_array(up).abs_diff_eq(Vec3::X, 1e-6), "{up:?}");
    }

    #[test]
    fn element_rescale_preserves_rotation_axis() {
        let positions = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        for (axis, axis_index) in [("x", 0), ("y", 1), ("z", 2)] {
            let rotation = Some(ElementRotation {
                origin: [8.0, 8.0, 8.0],
                axis: Some(axis.to_string()),
                angle: Some(45.0),
                rescale: true,
                ..Default::default()
            });
            let rotated = apply_element_rotation(positions, &rotation);
            for (before, after) in positions.iter().zip(rotated) {
                assert_eq!(
                    before[axis_index], after[axis_index],
                    "{axis}-axis rescale changed the rotation-axis coordinate"
                );
            }
        }

        let quarter_turn = Some(ElementRotation {
            origin: [8.0, 8.0, 8.0],
            axis: Some("y".to_string()),
            angle: Some(90.0),
            rescale: true,
            ..Default::default()
        });
        let rotated = apply_element_rotation([[0.75, 0.5, 0.5]; 4], &quarter_turn);
        assert!((rotated[0][0] - 0.5).abs() < 1.0e-6);
        assert!((rotated[0][1] - 0.5).abs() < 1.0e-6);
        assert!((rotated[0][2] - 0.25).abs() < 1.0e-6);

        let steep_turn = Some(ElementRotation {
            origin: [8.0, 8.0, 8.0],
            axis: Some("y".to_string()),
            angle: Some(67.5),
            rescale: true,
            ..Default::default()
        });
        let rotated = apply_element_rotation([[0.75, 0.5, 0.5]; 4], &steep_turn);
        let expected_x = 0.5 + 0.25 * 67.5_f32.to_radians().tan().recip();
        assert!((rotated[0][0] - expected_x).abs() < 1.0e-6);
        assert!((rotated[0][1] - 0.5).abs() < 1.0e-6);
        assert!((rotated[0][2] - 0.25).abs() < 1.0e-6);
    }

    #[test]
    fn rotated_faces_wind_back_to_their_facings_vertex_order() {
        // A full cube's top turned on its side, as a sideways log's is.
        let mut positions =
            rotate_positions(face_positions(Direction::Up, [0.0; 3], [1.0; 3]), 90, 0);
        let mut uvs = [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];
        let facing = direction_from_positions(&positions).unwrap();
        let before: Vec<([f32; 3], [f32; 2])> = positions.into_iter().zip(uvs).collect();
        recalculate_winding(&mut positions, &mut uvs, facing);
        let target = face_positions(facing, [0.0; 3], [1.0; 3]);
        assert_eq!(positions, target);
        // Each corner keeps its own uv.
        for (p, uv) in positions.iter().zip(uvs) {
            assert!(before.contains(&(*p, uv)));
        }
    }

    /// Every face must show the full-tile texture upright at rotation 0 and
    /// spin it clockwise per 90 degrees, vanilla's `FaceInfo` +
    /// `CuboidFace.UVs` + `Quadrant` behavior (the piston's side faces use
    /// 90/270 and read 180 degrees off when the direction is inverted).
    #[test]
    fn face_uvs_show_upright_clockwise_rotated_texture() {
        // Image corners in clockwise order; screen corner `s` under a
        // clockwise rotation by `steps` shows image corner `(s - steps) % 4`.
        const IMAGE_CORNERS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        for dir in DIRS {
            let (right, up) = face_axes(dir);
            let positions = face_positions(dir, [0.0; 3], [1.0; 3]).map(Vec3::from_array);
            for (rot, steps) in [
                (None, 0),
                (Some(0), 0),
                (Some(90), 1),
                (Some(180), 2),
                (Some(270), 3),
            ] {
                let uvs = face_uvs(dir, [0.0; 3], [1.0; 3], Some(&[0.0, 0.0, 16.0, 16.0]), rot);
                let mid = |axis: Vec3| {
                    let coords = positions.map(|p| p.dot(axis));
                    (coords.iter().copied().fold(f32::INFINITY, f32::min)
                        + coords.iter().copied().fold(f32::NEG_INFINITY, f32::max))
                        / 2.0
                };
                let (right_mid, up_mid) = (mid(right), mid(up));
                for (p, uv) in positions.iter().zip(uvs) {
                    let screen_corner = match (p.dot(right) > right_mid, p.dot(up) > up_mid) {
                        (false, true) => 0,
                        (true, true) => 1,
                        (true, false) => 2,
                        (false, false) => 3,
                    };
                    let expected = IMAGE_CORNERS[(screen_corner + 4 - steps) % 4];
                    assert_eq!(uv, expected, "{dir:?} rotation {rot:?} vertex {p:?}");
                }
            }
        }
    }

    /// Default UV windows per face, vanilla `FaceBakery.defaultFaceUV`: the
    /// down/north/east windows reflect about the texture center.
    #[test]
    fn default_uv_windows_match_vanilla() {
        let from = [2.0 / 16.0, 3.0 / 16.0, 4.0 / 16.0];
        let to = [8.0 / 16.0, 9.0 / 16.0, 10.0 / 16.0];
        let expected = [
            (Direction::Down, (0.125, 0.375, 0.5, 0.75)),
            (Direction::Up, (0.125, 0.25, 0.5, 0.625)),
            (Direction::North, (0.5, 0.4375, 0.875, 0.8125)),
            (Direction::South, (0.125, 0.4375, 0.5, 0.8125)),
            (Direction::West, (0.25, 0.4375, 0.625, 0.8125)),
            (Direction::East, (0.375, 0.4375, 0.75, 0.8125)),
        ];
        for (dir, (u1, v1, u2, v2)) in expected {
            let uvs = face_uvs(dir, from, to, None, None);
            // The cycle assigns vertex 0 the (u1, v1) corner and vertex 2 the
            // (u2, v2) corner.
            assert_eq!(uvs[0], [u1, v1], "{dir:?} window origin");
            assert_eq!(uvs[2], [u2, v2], "{dir:?} window extent");
        }
    }

    /// The bed shape: a composite item model contributes every child, the
    /// foot carrying its one-block translation.
    #[test]
    fn composite_item_collects_all_parts() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
                "model": {
                    "type": "minecraft:composite",
                    "models": [
                        {"type": "minecraft:model", "model": "minecraft:block/red_bed_head"},
                        {
                            "type": "minecraft:model",
                            "model": "minecraft:block/red_bed_foot",
                            "transformation": {"translation": [0.0, 0.0, 1.0]}
                        }
                    ]
                }
            }"#,
        )
        .unwrap();
        let parts = collect_model_parts(&json);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].path, "block/red_bed_head");
        assert!(parts[0].transform.is_none());
        assert_eq!(parts[1].path, "block/red_bed_foot");
        let moved = parts[1].transform.unwrap().transform_point3(Vec3::ZERO);
        assert!((moved - Vec3::new(0.0, 0.0, 1.0)).length() < 1e-6);
    }

    #[test]
    fn special_model_falls_back_to_its_base() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"model":{
                "type":"minecraft:special",
                "base":"minecraft:item/template_shulker_box",
                "model":{"type":"minecraft:shulker_box"}
            }}"#,
        )
        .unwrap();
        let parts = collect_model_parts(&json);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].path, "item/template_shulker_box");
        assert!(parts[0].tints.is_empty());
    }

    #[test]
    fn vanilla_block_tint_sources_match_26_2_registration() {
        assert_eq!(determine_tint("fern", 0), Tint::Grass);
        assert_eq!(determine_tint("bush", 0), Tint::Grass);
        assert_eq!(determine_tint("large_fern", 0), Tint::DoubleGrass);
        assert_eq!(determine_tint("tall_grass", 0), Tint::DoubleGrass);
        assert_eq!(determine_tint("pink_petals", 0), Tint::None);
        assert_eq!(determine_tint("pink_petals", 1), Tint::Grass);
        assert_eq!(determine_tint("spruce_leaves", 0), Tint::Constant(0x619961));
        assert_eq!(determine_tint("birch_leaves", 0), Tint::Constant(0x80A755));
        assert_eq!(determine_tint("oak_leaves", 0), Tint::Foliage);
        assert_eq!(determine_tint("leaf_litter", 0), Tint::DryFoliage);
        assert_eq!(determine_tint("water_cauldron", 0), Tint::Water);
        assert_eq!(determine_tint("redstone_wire", 0), Tint::Redstone);
        assert_eq!(determine_tint("sugar_cane", 0), Tint::Grass);
        assert_eq!(
            determine_tint("attached_melon_stem", 0),
            Tint::Constant(0xE0C71C)
        );
        assert_eq!(determine_tint("melon_stem", 0), Tint::Stem);
        assert_eq!(determine_tint("lily_pad", 0), Tint::Constant(0x208030));
        assert_eq!(determine_tint("stone", 0), Tint::None);
    }

    #[test]
    fn block_tint_is_applied_only_to_tintindexed_quads() {
        let mut model = BakedModel {
            quads: vec![
                BakedQuad {
                    positions: [[0.0; 3]; 4],
                    uvs: [[0.0; 2]; 4],
                    texture: TextureId::intern("fern"),
                    cullface: None,
                    tint: Tint::None,
                    tint_index: Some(0),
                    shade_light: 1.0,
                    shade_face: None,
                    emission: 0,
                    ambient_occlusion: true,
                },
                BakedQuad {
                    positions: [[0.0; 3]; 4],
                    uvs: [[0.0; 2]; 4],
                    texture: TextureId::intern("fern_overlay"),
                    cullface: None,
                    tint: Tint::None,
                    tint_index: None,
                    shade_light: 1.0,
                    shade_face: None,
                    emission: 0,
                    ambient_occlusion: true,
                },
            ],
            is_full_cube: false,
        };

        apply_block_tints(&mut model, "fern");
        assert_eq!(model.quads[0].tint, Tint::Grass);
        assert_eq!(model.quads[1].tint, Tint::None);
    }

    #[test]
    fn item_model_node_preserves_tint_sources() {
        let json = serde_json::json!({
            "model": {
                "type": "minecraft:model",
                "model": "minecraft:item/tinted",
                "tints": [
                    {"type": "minecraft:constant", "value": 0x123456},
                    {"type": "minecraft:dye", "default": 0x654321}
                ]
            }
        });
        let parts = collect_model_parts(&json);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].path, "item/tinted");
        let tints = parse_tints(&parts[0].tints).unwrap();
        assert_eq!(
            tints,
            vec![
                ItemTintSource::Constant(0xFF123456),
                ItemTintSource::Dye { default: 0x654321 }
            ]
        );
    }

    #[test]
    fn item_tint_parser_keeps_arbitrary_palette_length_and_all_26_2_codecs() {
        let values = vec![
            serde_json::json!({"type": "minecraft:constant", "value": 0x010203}),
            serde_json::json!({"type": "minecraft:dye", "default": 0x111213}),
            serde_json::json!({"type": "minecraft:firework", "default": 0x212223}),
            serde_json::json!({"type": "minecraft:potion", "default": 0x313233}),
            serde_json::json!({"type": "minecraft:map_color", "default": 0x414243}),
            serde_json::json!({
                "type": "minecraft:custom_model_data",
                "index": 7,
                "default": 0x515253
            }),
            serde_json::json!({"type": "minecraft:team", "default": 0x616263}),
        ];
        let tints = parse_tints(&values).unwrap();
        assert_eq!(tints.len(), values.len());
        assert_eq!(
            tints[5],
            ItemTintSource::CustomModelData {
                index: 7,
                default: 0x515253
            }
        );
        assert_eq!(tints[6], ItemTintSource::Team { default: 0x616263 });
    }

    #[test]
    fn selected_item_model_keeps_the_selected_nodes_tints() {
        let json = serde_json::json!({
            "model": {
                "type": "minecraft:condition",
                "property": "minecraft:using_item",
                "on_true": {"type": "minecraft:model", "model": "minecraft:item/alternate"},
                "on_false": {
                    "type": "minecraft:model",
                    "model": "minecraft:item/plain",
                    "tints": [{"type": "minecraft:constant", "value": 0xABCDEF}]
                }
            }
        });
        let parts = collect_model_parts(&json);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].path, "item/plain");
        assert_eq!(
            parse_tints(&parts[0].tints).unwrap(),
            vec![ItemTintSource::Constant(0xFFABCDEF)]
        );
    }

    #[test]
    fn grass_item_tint_uses_vanilla_colormap_coordinates() {
        let mut colormap = vec![[0_u8; 3]; 256 * 256];
        colormap[127 * 256 + 127] = [0x91, 0xBD, 0x59];
        let tint = parse_item_tint_sources(
            &[serde_json::json!({
                "type": "minecraft:grass",
                "temperature": 0.5,
                "downfall": 1.0
            })],
            &Colormap::from_pixels(colormap),
        )
        .unwrap();
        assert_eq!(tint, vec![ItemTintSource::Grass { color: 0xFF91BD59 }]);
    }

    #[test]
    fn item_tint_rgb_codec_accepts_vanilla_vector_form_and_preserves_integer_alpha() {
        assert_eq!(
            parse_rgb_color(&serde_json::json!([1.0, 0.5, 0.0])).unwrap(),
            0xFFFF7F00
        );
        assert_eq!(
            parse_rgb_color(&serde_json::json!(-2146360781_i64)).unwrap(),
            0x80112233
        );
    }

    #[test]
    fn item_tint_parser_rejects_missing_required_fields_and_invalid_ranges() {
        assert!(parse_tints(&[serde_json::json!({"type": "minecraft:dye"})]).is_err());
        assert!(
            parse_tints(&[serde_json::json!({
                "type": "minecraft:grass",
                "temperature": 1.1,
                "downfall": 1.0
            })])
            .is_err()
        );
        assert!(
            parse_tints(&[serde_json::json!({
                "type": "minecraft:custom_model_data",
                "index": -1,
                "default": 0xffffff
            })])
            .is_err()
        );
    }

    #[test]
    fn unsupported_item_transformation_encoding_is_rejected() {
        let raw_matrix = serde_json::json!([
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0
        ]);
        let axis_angle = serde_json::json!({
            "left_rotation": {"axis": [0.0, 1.0, 0.0], "angle": 90.0}
        });
        assert!(parse_item_transformation(&raw_matrix).is_none());
        assert!(parse_item_transformation(&axis_angle).is_none());
    }

    /// Non-composite trees (bundles' select/condition) keep the old
    /// first-model-string behavior.
    #[test]
    fn chest_items_name_their_special_texture() {
        let ender = serde_json::json!({"model": {"type": "minecraft:special",
            "base": "minecraft:item/ender_chest",
            "model": {"type": "minecraft:chest", "texture": "minecraft:ender"}}});
        assert_eq!(
            chest_special_texture(&ender).as_deref(),
            Some("entity/chest/ender")
        );
        let trapped = serde_json::json!({"model": {"type": "minecraft:select",
            "property": "minecraft:local_time", "pattern": "MM-dd",
            "cases": [{"when": ["12-24"], "model": {"type": "minecraft:special",
                "base": "minecraft:item/trapped_chest",
                "model": {"type": "minecraft:chest", "texture": "minecraft:christmas"}}}],
            "fallback": {"type": "minecraft:special", "base": "minecraft:item/trapped_chest",
                "model": {"type": "minecraft:chest", "texture": "minecraft:trapped"}}}});
        assert_eq!(
            chest_special_texture(&trapped).as_deref(),
            Some("entity/chest/trapped")
        );
        let bed = serde_json::json!({"model": {"type": "minecraft:special",
            "base": "minecraft:item/red_bed",
            "model": {"type": "minecraft:bed", "texture": "minecraft:red"}}});
        assert_eq!(chest_special_texture(&bed), None);
    }

    #[test]
    fn select_item_falls_back_to_first_model() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
                "model": {
                    "type": "minecraft:select",
                    "cases": [
                        {"model": {"type": "minecraft:model", "model": "minecraft:item/bundle_open"}}
                    ],
                    "fallback": {"type": "minecraft:model", "model": "minecraft:item/bundle"}
                }
            }"#,
        )
        .unwrap();
        let parts = collect_model_parts(&json);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].transform.is_none());
        let legacy = find_first_model_string(&json).unwrap();
        assert_eq!(parts[0].path, strip_mc_prefix(&legacy));
    }

    /// Both resolvers pick `path` for an item definition.
    fn assert_resolves_to(json: &serde_json::Value, path: &str) {
        let parts = collect_model_parts(json);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].path, path);
        assert_eq!(first_item_model_ref(json).as_deref(), Some(path));
    }

    #[test]
    fn trim_select_bakes_the_untrimmed_fallback() {
        let json = serde_json::json!({
            "model": {
                "type": "minecraft:select",
                "property": "minecraft:trim_material",
                "cases": [{
                    "when": "minecraft:quartz",
                    "model": {"type": "minecraft:model", "model": "minecraft:item/iron_chestplate_quartz_trim"}
                }],
                "fallback": {"type": "minecraft:model", "model": "minecraft:item/iron_chestplate"}
            }
        });
        assert_resolves_to(&json, "item/iron_chestplate");
    }

    /// The charge fallback is a `condition`, which the heuristic resolves to
    /// its `on_false`.
    #[test]
    fn charge_select_bakes_the_unloaded_crossbow() {
        let json = serde_json::json!({
            "model": {
                "type": "minecraft:select",
                "property": "minecraft:charge_type",
                "cases": [{
                    "when": "arrow",
                    "model": {"type": "minecraft:model", "model": "minecraft:item/crossbow_arrow"}
                }],
                "fallback": {
                    "type": "minecraft:condition",
                    "property": "minecraft:using_item",
                    "on_false": {"type": "minecraft:model", "model": "minecraft:item/crossbow"},
                    "on_true": {"type": "minecraft:model", "model": "minecraft:item/crossbow_pulling_0"}
                }
            }
        });
        assert_resolves_to(&json, "item/crossbow");
    }

    #[test]
    fn charge_type_cases_list_each_loaded_model() {
        let json = serde_json::json!({"model": {
            "type": "minecraft:select",
            "property": "minecraft:charge_type",
            "cases": [
                {"when": "arrow", "model": {"type": "minecraft:model", "model": "item/a"}},
                {"when": ["rocket", "other"], "model": {"type": "minecraft:model", "model": "item/r"}}
            ],
            "fallback": {"type": "minecraft:model", "model": "item/x"}
        }});
        let cases: Vec<_> = charge_type_cases(&json)
            .into_iter()
            .map(|(when, node)| (when, node["model"].as_str().unwrap()))
            .collect();
        assert_eq!(
            cases,
            [
                ("arrow", "item/a"),
                ("rocket", "item/r"),
                ("other", "item/r")
            ]
        );
        let trim = serde_json::json!({"model": {"type": "minecraft:select", "property": "minecraft:trim_material", "cases": []}});
        assert!(charge_type_cases(&trim).is_empty());
    }

    #[test]
    fn bundle_selected_layers_split_around_the_selected_item() {
        // Vanilla 26.2 `items/bundle.json`.
        let json = serde_json::json!({"model": {
            "type": "minecraft:select",
            "property": "minecraft:display_context",
            "cases": [{"when": "gui", "model": {
                "type": "minecraft:condition",
                "property": "minecraft:bundle/has_selected_item",
                "on_false": {"type": "minecraft:model", "model": "minecraft:item/bundle"},
                "on_true": {"type": "minecraft:composite", "models": [
                    {"type": "minecraft:model", "model": "minecraft:item/bundle_open_back"},
                    {"type": "minecraft:bundle/selected_item"},
                    {"type": "minecraft:model", "model": "minecraft:item/bundle_open_front"}
                ]}
            }}],
            "fallback": {"type": "minecraft:model", "model": "minecraft:item/bundle"}
        }});
        let [back, front] = selected_bundle_layers(&json).unwrap();
        let paths = |parts: &[ModelPart]| parts.iter().map(|p| p.path.clone()).collect::<Vec<_>>();
        assert_eq!(paths(&back), ["item/bundle_open_back"]);
        assert_eq!(paths(&front), ["item/bundle_open_front"]);
        assert!(selected_bundle_layers(&serde_json::json!({"model": {}})).is_none());
    }

    use crate::test_util::test_temp_dir;

    /// A shield (a special `shield` model under `using_item`) bakes vanilla's
    /// plate and handle around the item's center, takes its base model's
    /// display transforms, and a pack's composite case adds its own model.
    /// A composite whose parts are posed apart in a view (Void Rush's
    /// shield frames, raised 3 pixels in third person) bakes that view's
    /// variant with the part moved into the first part's pose; views where
    /// they agree get none.
    #[test]
    fn composite_parts_posed_apart_bake_a_view_variant() {
        let root = test_temp_dir("context_variant");
        let jar = root.join("jar");
        let items = jar.join("minecraft/items");
        let models = jar.join("minecraft/models/item");
        for dir in [&items, &models] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(
            items.join("shield.json"),
            r#"{"model":{"type":"minecraft:composite","models":[
                {"type":"minecraft:special","base":"minecraft:item/shield","model":{"type":"minecraft:shield"}},
                {"type":"minecraft:model","model":"minecraft:item/frame"}]}}"#,
        )
        .unwrap();
        std::fs::write(
            models.join("shield.json"),
            r#"{"display":{"thirdperson_righthand":{"rotation":[0,90,0],"translation":[10,6,-4]},
                "gui":{"translation":[2,3,0],"scale":[0.65,0.65,0.65]}}}"#,
        )
        .unwrap();
        std::fs::write(
            models.join("frame.json"),
            r##"{"textures":{"frame":"block/stone"},
                "display":{"thirdperson_righthand":{"rotation":[0,90,0],"translation":[10,9,-4]},
                "gui":{"translation":[2,3,0],"scale":[0.65,0.65,0.65]}},
                "elements":[{"from":[-7,-12,0.6],"to":[-5.5,12,2.4],"faces":{"south":{"uv":[0,0,1,1],"texture":"#frame"}}}]}"##,
        )
        .unwrap();

        let baked = bake_item_models(&jar, &None, None);
        let frame_y = |key: &str| baked.models[key].quads.last().unwrap().positions[0][1];
        // THIRD_PERSON_RIGHT_HAND (2): the frame 3 pixels up, in the
        // shield's pose.
        let variant = context_model_key("shield", 2);
        assert!(((frame_y(&variant) - frame_y("shield")) - 3.0 / 16.0).abs() < 1e-5);
        assert_eq!(
            baked.models[&variant].quads[0].positions,
            baked.models["shield"].quads[0].positions
        );
        // GUI (6): posed alike, no variant.
        assert!(!baked.models.contains_key(&context_model_key("shield", 6)));
    }

    /// The fishing rod's `fishing_rod/cast` condition bakes its cast sprite
    /// (no line hanging) as a variant of its own, beside the plain one.
    #[test]
    fn the_cast_fishing_rod_bakes_its_own_sprite() {
        let root = test_temp_dir("cast_rod");
        let jar = root.join("jar");
        let items = jar.join("minecraft/items");
        let models = jar.join("minecraft/models/item");
        for dir in [&items, &models] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(
            items.join("fishing_rod.json"),
            r#"{"model":{"type":"minecraft:condition","property":"minecraft:fishing_rod/cast",
                "on_false":{"type":"minecraft:model","model":"minecraft:item/fishing_rod"},
                "on_true":{"type":"minecraft:model","model":"minecraft:item/fishing_rod_cast"}}}"#,
        )
        .unwrap();
        for name in ["fishing_rod", "fishing_rod_cast"] {
            std::fs::write(
                models.join(format!("{name}.json")),
                format!(r#"{{"parent":"minecraft:item/generated","textures":{{"layer0":"minecraft:item/{name}"}}}}"#),
            )
            .unwrap();
        }
        std::fs::write(models.join("generated.json"), r#"{}"#).unwrap();

        let baked = bake_item_models(&jar, &None, None);
        let sprite = |key: &str| baked.flat_texture_keys[key][0].clone();
        assert_eq!(sprite("fishing_rod").as_deref(), Some("item/fishing_rod"));
        assert_eq!(
            sprite("fishing_rod#cast").as_deref(),
            Some("item/fishing_rod_cast")
        );
    }

    #[test]
    fn shields_bake_the_special_model() {
        let root = test_temp_dir("shield_special");
        let jar = root.join("jar");
        let items = jar.join("minecraft/items");
        let models = jar.join("minecraft/models/item");
        for dir in [&items, &models] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let special = |base: &str| {
            format!(
                r#"{{"type":"minecraft:special","base":"minecraft:item/{base}","model":{{"type":"minecraft:shield"}}}}"#
            )
        };
        std::fs::write(
            items.join("shield.json"),
            format!(
                r#"{{"model":{{"type":"minecraft:select","property":"minecraft:custom_model_data",
                "cases":[{{"when":"frame","model":{{"type":"minecraft:condition","property":"minecraft:using_item",
                    "on_false":{{"type":"minecraft:composite","models":[{},{{"type":"minecraft:model","model":"minecraft:item/frame"}}]}},
                    "on_true":{}}}}}],
                "fallback":{{"type":"minecraft:condition","property":"minecraft:using_item","on_false":{},"on_true":{}}}}}}}"#,
                special("shield"),
                special("shield_blocking"),
                special("shield"),
                special("shield_blocking"),
            ),
        )
        .unwrap();
        std::fs::write(
            models.join("shield.json"),
            r#"{"gui_light":"front","display":{"thirdperson_righthand":{"rotation":[0,90,0],"translation":[10,6,-4]},"gui":{"rotation":[15,-25,-5],"translation":[2,3,0],"scale":[0.65,0.65,0.65]}}}"#,
        )
        .unwrap();
        std::fs::write(
            models.join("frame.json"),
            r##"{"parent":"minecraft:item/shield","textures":{"frame":"block/stone"},
                "elements":[{"from":[-7,-12,0.6],"to":[-5.5,15,2.4],"faces":{"south":{"uv":[0,0,1,1],"texture":"#frame"}}}]}"##,
        )
        .unwrap();

        let baked = bake_item_models(&jar, &None, None);
        let bounds = |key: &str| {
            let quads = &baked.models[key].quads;
            let points = quads.iter().flat_map(|quad| quad.positions);
            let min = points.clone().fold([f32::MAX; 3], |a, p| {
                std::array::from_fn(|i| a[i].min(p[i]))
            });
            let max = points.fold([f32::MIN; 3], |a, p| {
                std::array::from_fn(|i| a[i].max(p[i]))
            });
            (quads.len(), min, max)
        };
        // The plate (12x22x1) and handle (2x6x6) around the item model's
        // origin, the handle behind the plate.
        let (quads, min, max) = bounds("shield");
        assert_eq!(quads, 12);
        let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-5);
        assert!(
            close(min, [-6.0 / 16.0, -11.0 / 16.0, -5.0 / 16.0]),
            "{min:?}"
        );
        assert!(close(max, [6.0 / 16.0, 11.0 / 16.0, 2.0 / 16.0]), "{max:?}");
        let display = baked.display_transforms["shield"];
        assert_eq!(display[6].map(|t| t.scale), Some(Vec3::splat(0.65)));
        assert_eq!(
            display[2].map(|t| t.rotation),
            Some(Vec3::new(0.0, 90.0, 0.0))
        );
        // The pack's frame joins the plate, with the same transforms.
        let framed = custom_model_key("shield", "frame");
        assert_eq!(bounds(&framed).0, 13);
        assert_eq!(baked.display_transforms[&framed], display);
    }

    /// Audits a real pack against a real jar: every item definition the pack
    /// overrides and every `custom_model_data` case (and its GUI variant)
    /// must bake to something drawable, and every texture they draw must
    /// exist. Run with `POMME_PACK_INSTANCE` (an instance dir whose
    /// `resourcepacks/vr` is the unzipped pack) and `POMME_JAR_ASSETS`:
    /// `cargo test -p pomme-client --lib pack_items_all_bake -- --ignored
    /// --nocapture`.
    #[test]
    #[ignore = "needs a pack and a jar on disk"]
    fn pack_items_all_bake() {
        let env = |key: &str| std::path::PathBuf::from(std::env::var(key).expect(key));
        let (instance, jar) = (env("POMME_PACK_INSTANCE"), env("POMME_JAR_ASSETS"));
        let mut packs = crate::resource_pack::ResourcePackManager::new(&instance);
        packs.enable_local_pack("vr");
        let baked = bake_item_models(&jar, &None, Some(&packs));
        let drawable = |key: &str| {
            baked.models.get(key).is_some_and(|m| !m.quads.is_empty())
                || baked
                    .flat_texture_keys
                    .get(key)
                    .is_some_and(|layers| layers.iter().any(Option::is_some))
        };
        let mut checked = 0;
        let mut undrawable = Vec::new();
        for name in item_definition_names(&jar, Some(&packs)) {
            let key = item_definition_asset_key(&name);
            let Some(path) = packs.resolve_asset(&key) else {
                continue;
            };
            let json: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            let mut keys = vec![name.clone()];
            if json.get("model").is_some_and(has_gui_variant) {
                keys.push(gui_model_key(&name));
            }
            for (when, node) in custom_model_data_cases(&json) {
                let case = custom_model_key(&name, when);
                if has_gui_variant(node) {
                    keys.push(gui_model_key(&case));
                }
                keys.push(case);
            }
            for key in keys {
                checked += 1;
                if !drawable(&key) {
                    undrawable.push(key);
                }
            }
        }
        let texture_exists = |texture: &str| {
            let key = crate::renderer::chunk::atlas::atlas_asset_path(texture);
            crate::assets::resolve_asset_path_with_packs(&jar, &None, &key, Some(&packs)).exists()
        };
        let mut missing_textures: std::collections::BTreeMap<&str, Vec<String>> =
            Default::default();
        for (key, model) in &baked.models {
            for quad in &model.quads {
                let texture = quad.texture.name();
                if !texture_exists(texture) {
                    missing_textures
                        .entry(texture)
                        .or_default()
                        .push(key.clone());
                }
            }
        }
        for (key, layers) in &baked.flat_texture_keys {
            for texture in layers.iter().flatten() {
                if !texture_exists(texture) {
                    missing_textures
                        .entry(texture.as_str())
                        .or_default()
                        .push(key.clone());
                }
            }
        }
        undrawable.sort();
        println!(
            "checked {checked} item keys; {} draw nothing:",
            undrawable.len()
        );
        for key in &undrawable {
            println!("  UNDRAWABLE {key}");
        }
        println!("{} textures missing:", missing_textures.len());
        for (texture, keys) in &missing_textures {
            let mut keys = keys.clone();
            keys.sort();
            keys.dedup();
            println!(
                "  MISSING {texture} (used by {})",
                keys.iter().take(4).cloned().collect::<Vec<_>>().join(", ")
            );
        }
    }

    /// A `display_context` select (Void Rush's weapons): the 3D model bakes
    /// for the world, the GUI case's flat icon under the GUI key.
    #[test]
    fn display_context_selects_bake_a_gui_variant() {
        let root = test_temp_dir("display_context_variants");
        let jar = root.join("jar");
        let items = jar.join("minecraft/items");
        let models = jar.join("minecraft/models/item");
        for dir in [&items, &models] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(
            items.join("stick.json"),
            r#"{"model":{"type":"minecraft:select","property":"minecraft:custom_model_data",
                "cases":[{"when":"staff_3","model":{"type":"minecraft:select","property":"minecraft:display_context",
                    "cases":[{"when":"gui","model":{"type":"minecraft:model","model":"minecraft:item/staff_icon"}}],
                    "fallback":{"type":"minecraft:model","model":"minecraft:item/staff_3d"}}}],
                "fallback":{"type":"minecraft:model","model":"minecraft:item/stick"}}}"#,
        )
        .unwrap();
        let flat = |texture: &str| {
            format!(
                r#"{{"parent":"minecraft:item/generated","textures":{{"layer0":"{texture}"}}}}"#
            )
        };
        std::fs::write(models.join("stick.json"), flat("minecraft:item/stick")).unwrap();
        std::fs::write(
            models.join("staff_icon.json"),
            flat("minecraft:item/staff_icon"),
        )
        .unwrap();
        std::fs::write(models.join("generated.json"), r#"{}"#).unwrap();
        std::fs::write(
            models.join("staff_3d.json"),
            r##"{"textures":{"wood":"minecraft:block/oak_planks"},
                "elements":[{"from":[7,0,7],"to":[9,16,9],"faces":{"north":{"uv":[0,0,2,16],"texture":"#wood"}}}]}"##,
        )
        .unwrap();

        let baked = bake_item_models(&jar, &None, None);
        let staff = custom_model_key("stick", "staff_3");
        assert!(
            baked.models.contains_key(&staff),
            "the 3D staff for the world"
        );
        assert!(!baked.flat_texture_keys.contains_key(&staff));
        assert_eq!(
            baked
                .flat_texture_keys
                .get(&gui_model_key(&staff))
                .map(|keys| keys[0].clone()),
            Some(Some("item/staff_icon".to_string())),
            "the flat icon for the GUI"
        );
        assert!(
            !baked
                .flat_texture_keys
                .contains_key(&gui_model_key("stick")),
            "no GUI variant without a display_context select"
        );
    }

    /// A resource pack's `custom_model_data` select (Void Rush's layout): each
    /// case bakes under its own key, a plain stack takes the fallback, and a
    /// definition in another namespace bakes under `namespace:path`.
    #[test]
    fn custom_model_data_cases_and_namespaced_definitions_bake() {
        let root = test_temp_dir("custom_model_data_cases");
        let jar = root.join("jar");
        let items = jar.join("minecraft/items");
        let models = jar.join("minecraft/models/item");
        let pack_items = jar.join("pack/items");
        let pack_models = jar.join("pack/models/item");
        for dir in [&items, &models, &pack_items, &pack_models] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(
            items.join("bone.json"),
            r#"{"model":{"type":"minecraft:select","property":"minecraft:custom_model_data",
                "cases":[{"when":"staff","model":{"type":"minecraft:model","model":"pack:item/staff"}},
                         {"when":["a","b"],"model":{"type":"minecraft:model","model":"pack:item/wand"}}],
                "fallback":{"type":"minecraft:model","model":"minecraft:item/bone"}}}"#,
        )
        .unwrap();
        std::fs::write(
            pack_items.join("relic.json"),
            r#"{"model":{"type":"minecraft:model","model":"pack:item/staff"}}"#,
        )
        .unwrap();
        let flat = |texture: &str| {
            format!(
                r#"{{"parent":"minecraft:item/generated","textures":{{"layer0":"{texture}"}}}}"#
            )
        };
        std::fs::write(models.join("bone.json"), flat("minecraft:item/bone")).unwrap();
        std::fs::write(pack_models.join("staff.json"), flat("pack:item/staff")).unwrap();
        std::fs::write(pack_models.join("wand.json"), flat("pack:item/wand")).unwrap();
        std::fs::write(models.join("generated.json"), r#"{}"#).unwrap();

        let baked = bake_item_models(&jar, &None, None);
        let layer0 = |key: &str| baked.flat_texture_keys.get(key).map(|keys| keys[0].clone());
        assert_eq!(layer0("bone"), Some(Some("item/bone".to_string())));
        assert_eq!(
            layer0(&custom_model_key("bone", "staff")),
            Some(Some("pack:item/staff".to_string()))
        );
        for when in ["a", "b"] {
            assert_eq!(
                layer0(&custom_model_key("bone", when)),
                Some(Some("pack:item/wand".to_string()))
            );
        }
        assert_eq!(
            layer0(&item_model_name("pack:relic")),
            Some(Some("pack:item/staff".to_string()))
        );
        assert_eq!(item_model_name("minecraft:bone"), "bone");
    }

    #[test]
    fn generated_item_layers_keep_their_layer_positions() {
        let root = test_temp_dir("generated_layer_gap");
        let jar = root.join("jar");
        let items = jar.join("minecraft/items");
        let models = jar.join("minecraft/models/item");
        std::fs::create_dir_all(&items).unwrap();
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(
            items.join("holey.json"),
            r#"{"model":{"type":"minecraft:model","model":"minecraft:item/holey"}}"#,
        )
        .unwrap();
        std::fs::write(
            models.join("holey.json"),
            r#"{
                "parent":"minecraft:item/generated",
                "textures":{
                    "layer0":"minecraft:item/zero",
                    "layer2":"minecraft:item/two"
                }
            }"#,
        )
        .unwrap();
        std::fs::write(
            items.join("unresolved.json"),
            r#"{"model":{"type":"minecraft:model","model":"minecraft:item/unresolved"}}"#,
        )
        .unwrap();
        std::fs::write(
            models.join("unresolved.json"),
            r##"{
                "parent":"minecraft:item/generated",
                "textures":{
                    "layer0":"minecraft:item/zero",
                    "layer1":"#missing",
                    "layer2":"minecraft:item/two"
                }
            }"##,
        )
        .unwrap();
        std::fs::write(models.join("generated.json"), r#"{}"#).unwrap();

        let baked = bake_item_models(&jar, &None, None);
        assert_eq!(
            baked.flat_texture_keys.get("holey").map(Vec::as_slice),
            Some([Some("item/zero".to_string())].as_slice())
        );
        assert_eq!(
            baked.flat_texture_keys.get("unresolved").map(Vec::as_slice),
            Some(
                [
                    Some("item/zero".to_string()),
                    None,
                    Some("item/two".to_string()),
                ]
                .as_slice()
            )
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resolved_model_inherits_gui_transform() {
        let root = test_temp_dir("item_gui_transform");
        let models = root.join("minecraft/models/block");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(
            models.join("child.json"),
            r#"{"parent":"minecraft:block/parent"}"#,
        )
        .unwrap();
        std::fs::write(
            models.join("parent.json"),
            r#"{"gui_light":"front","display":{"gui":{"rotation":[30,135,0],"scale":[0.625,0.625,0.625]}}}"#,
        )
        .unwrap();

        let mut cache = HashMap::new();
        let resolved = resolve_model("block/child", &root, &None, &mut cache, None);
        assert_eq!(resolved.gui_transform, gui_transform(135.0));
        assert_eq!(resolved.gui_light, GuiLight::Front);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn item_definition_and_display_transforms_follow_resource_pack_override() {
        let root = test_temp_dir("item_model_pack");
        let jar = root.join("jar");
        let instance = root.join("instance");
        let items = jar.join("minecraft/items");
        let models = jar.join("minecraft/models/item");
        let pack_items = instance.join("resourcepacks/test_pack/assets/minecraft/items");
        let pack_models = instance.join("resourcepacks/test_pack/assets/other/models/item");
        std::fs::create_dir_all(&items).unwrap();
        std::fs::create_dir_all(&models).unwrap();
        std::fs::create_dir_all(&pack_items).unwrap();
        std::fs::create_dir_all(&pack_models).unwrap();
        std::fs::write(
            instance.join("resourcepacks/test_pack/pack.mcmeta"),
            r#"{"pack":{"pack_format":84,"description":"test"}}"#,
        )
        .unwrap();
        std::fs::write(
            items.join("test_item.json"),
            r#"{"model":{"type":"minecraft:model","model":"minecraft:item/base"}}"#,
        )
        .unwrap();
        std::fs::write(
            models.join("base.json"),
            r#"{"parent":"minecraft:item/generated","textures":{"layer0":"minecraft:item/base"}}"#,
        )
        .unwrap();
        std::fs::write(
            models.join("generated.json"),
            r#"{"display":{"ground":{"translation":[0,2,0],"scale":[0.5,0.5,0.5]}}}"#,
        )
        .unwrap();
        std::fs::write(
            pack_items.join("test_item.json"),
            r#"{"model":{"type":"minecraft:model","model":"other:item/replacement"}}"#,
        )
        .unwrap();
        std::fs::write(
            pack_models.join("replacement.json"),
            r#"{"parent":"minecraft:item/generated","textures":{"layer0":"other:item/replacement"},"display":{"gui":{"translation":[32,-96,0],"scale":[8,-8,1]}}}"#,
        )
        .unwrap();
        // An item the jar does not define at all.
        std::fs::write(
            pack_items.join("pack_only.json"),
            r#"{"model":{"type":"minecraft:model","model":"other:item/replacement"}}"#,
        )
        .unwrap();

        let mut packs = crate::resource_pack::ResourcePackManager::new(&instance);
        packs.enable_local_pack("test_pack");
        let baked = bake_item_models(&jar, &None, Some(&packs));
        assert_eq!(
            baked.flat_texture_keys.get("test_item").map(Vec::as_slice),
            Some([Some("other:item/replacement".to_string())].as_slice())
        );
        assert_eq!(
            baked.flat_texture_keys.get("pack_only").map(Vec::as_slice),
            Some([Some("other:item/replacement".to_string())].as_slice())
        );
        assert_eq!(
            baked.gui_transforms["test_item"],
            DisplayTransform {
                rotation: Vec3::ZERO,
                translation: Vec3::new(2.0, -5.0, 0.0),
                scale: Vec3::new(4.0, -4.0, 1.0),
            }
        );
        let transform = baked.ground_transforms["test_item"];
        let origin = transform.transform_point3(Vec3::ZERO);
        assert!((origin - Vec3::new(0.0, 2.0 / 16.0, 0.0)).length() < 1.0e-6);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bare_face_texture_slot_matches_vanilla_texture_slots() {
        let root = test_temp_dir("bare_texture_slot");
        let models = root.join("minecraft/models/block");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(
            models.join("test_core.json"),
            r#"{
                "textures":{"all":"block/test_core"},
                "elements":[{
                    "from":[4,0,4],"to":[12,8,12],
                    "faces":{
                        "down":{"texture":"all"},"up":{"texture":"all"},
                        "north":{"texture":"all"},"south":{"texture":"all"},
                        "west":{"texture":"all"},"east":{"texture":"all"}
                    }
                }]
            }"#,
        )
        .unwrap();

        let mut cache = HashMap::new();
        let resolved = resolve_model("block/test_core", &root, &None, &mut cache, None);
        let baked = bake_resolved_model(&resolved, 0, 0, Tint::None).unwrap();
        assert_eq!(baked.quads.len(), 6);
        assert!(
            baked
                .quads
                .iter()
                .all(|quad| quad.texture.name() == "test_core")
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ground_display_transform_matches_vanilla_centered_mesh() {
        let json = serde_json::json!({
            "rotation": [0.0, 0.0, 0.0],
            "translation": [0.0, 3.0, 0.0],
            "scale": [0.5, 0.5, 0.5]
        });
        let transform = parse_display_transform(&json).unwrap().to_matrix();

        // ItemTransform applies translation/rotation/scale and then recenters
        // vanilla's 0..1 model. Pomme stores its item mesh already centered,
        // so the equivalent matrix leaves out only that final -0.5 step.
        let min = transform.transform_point3(Vec3::splat(-0.5));
        let max = transform.transform_point3(Vec3::splat(0.5));
        assert!((min - Vec3::new(-0.25, -0.0625, -0.25)).length() < 1e-6);
        assert!((max - Vec3::new(0.25, 0.4375, 0.25)).length() < 1e-6);
    }

    #[test]
    fn plain_item_is_a_single_part() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"model": {"type": "minecraft:model", "model": "minecraft:block/oak_stairs"}}"#,
        )
        .unwrap();
        let parts = collect_model_parts(&json);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].path, "block/oak_stairs");
        assert!(parts[0].transform.is_none());
    }
}
