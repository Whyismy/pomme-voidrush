//! Banner patterns: the server's `banner_pattern` registry, which a stack's
//! `banner_patterns` component points into, and the dye colours its layers
//! are tinted with, so items can draw a banner's pattern layers.

use std::sync::{PoisonError, RwLock};

use azalea_core::registry_holder::RegistryHolder;
use azalea_inventory::components::BannerPatterns;
use azalea_registry::identifier::Identifier;

/// Each pattern's `asset_id`, by protocol id.
static PATTERNS: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// Keeps the pattern registry from the registries the server synced.
pub fn set_from_registries(holder: &RegistryHolder) {
    let patterns = holder
        .extra
        .get(&Identifier::new("minecraft:banner_pattern"))
        .map(|registry| {
            registry
                .map
                .iter()
                .map(|(id, element)| {
                    element
                        .string("asset_id")
                        .map_or_else(|| id.to_string(), |asset| asset.to_str().into_owned())
                })
                .collect()
        })
        .unwrap_or_default();
    *PATTERNS.write().unwrap_or_else(PoisonError::into_inner) = patterns;
}

/// Vanilla `DyeColor.getTextureDiffuseColor`, by dye id.
pub fn dye_rgb(dye: i32) -> u32 {
    const COLORS: [u32; 16] = [
        0xF9FFFE, 0xF9801D, 0xC74EBD, 0x3AB3DA, 0xFED83D, 0x80C71F, 0xF38BAA, 0x474F52, 0x9D9D97,
        0x169C9C, 0x8932B8, 0x3C44AA, 0x835432, 0x5E7C16, 0xB02E26, 0x1D1D21,
    ];
    COLORS
        .get(usize::try_from(dye).unwrap_or(0))
        .copied()
        .unwrap_or(COLORS[0])
}

/// A dye's colour by name (a banner item's `color`), vanilla's diffuse.
pub fn dye_rgb_named(name: &str) -> u32 {
    const NAMES: [&str; 16] = [
        "white",
        "orange",
        "magenta",
        "light_blue",
        "yellow",
        "lime",
        "pink",
        "gray",
        "light_gray",
        "cyan",
        "purple",
        "blue",
        "brown",
        "green",
        "red",
        "black",
    ];
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    dye_rgb(NAMES.iter().position(|n| *n == name).unwrap_or(0) as i32)
}

/// The suffix a stack's model key takes for its pattern layers (see
/// [`layers`]): each layer's pattern asset and colour, in order.
pub fn layers_suffix(patterns: &BannerPatterns) -> Option<String> {
    if patterns.patterns.is_empty() {
        return None;
    }
    let names = PATTERNS.read().unwrap_or_else(PoisonError::into_inner);
    let layers: Vec<String> = patterns
        .patterns
        .iter()
        .filter_map(|layer| {
            let asset = names.get(usize::try_from(layer.pattern).ok()?)?;
            Some(format!("{asset}={:06x}", dye_rgb(layer.color)))
        })
        .collect();
    (!layers.is_empty()).then(|| format!("~{}", layers.join(",")))
}

/// A model key's pattern layers (see [`layers_suffix`]): each layer's
/// texture (`entity/banner/<pattern>`, namespaced outside `minecraft`) and
/// colour.
pub fn layers(key: &str) -> impl Iterator<Item = (String, u32)> + '_ {
    key.split_once('~')
        .map(|(_, layers)| layers)
        .unwrap_or_default()
        .split(',')
        .filter_map(|layer| {
            let (asset, color) = layer.split_once('=')?;
            let color = u32::from_str_radix(color, 16).ok()?;
            let texture = match asset.split_once(':') {
                Some(("minecraft", _)) | None => format!("entity/banner/{}", path_of(asset)),
                Some((namespace, path)) => format!("{namespace}:entity/banner/{path}"),
            };
            Some((texture, color))
        })
}

fn path_of(asset: &str) -> &str {
    asset.split_once(':').map_or(asset, |(_, path)| path)
}

/// A key without its pattern layers: the baked model it builds on.
pub fn base_key(key: &str) -> &str {
    key.split_once('~').map_or(key, |(base, _)| base)
}
