//! Armour trims: the server's `trim_material` and `trim_pattern` registries,
//! which a worn piece's `trim` component points into, so the renderer can
//! name the trim sprite it draws.

use std::collections::HashMap;
use std::sync::{PoisonError, RwLock};

use azalea_core::registry_holder::RegistryHolder;
use azalea_inventory::components::Trim;
use azalea_registry::DataRegistry;
use azalea_registry::identifier::Identifier;

/// A trim material's palettes (vanilla `MaterialAssetGroup`).
struct Material {
    /// The palette it draws with (`asset_name`).
    asset_name: String,
    /// Palettes for armour of particular equipment assets, keyed by asset
    /// id (`override_armor_assets`): a darker iron on iron armour.
    overrides: HashMap<String, String>,
}

struct Registries {
    materials: Vec<Material>,
    /// Each pattern's `asset_id`.
    patterns: Vec<String>,
}

static REGISTRIES: RwLock<Registries> = RwLock::new(Registries {
    materials: Vec::new(),
    patterns: Vec::new(),
});

/// Keeps the trim registries from the ones the server synced.
pub fn set_from_registries(holder: &RegistryHolder) {
    let entries = |name: &str| {
        holder
            .extra
            .get(&Identifier::new(name))
            .map(|registry| registry.map.iter().collect::<Vec<_>>())
            .unwrap_or_default()
    };
    let materials = entries("minecraft:trim_material")
        .into_iter()
        .map(|(id, element)| Material {
            asset_name: element
                .string("asset_name")
                .map_or_else(|| id.path().to_owned(), |name| name.to_str().into_owned()),
            overrides: element
                .compound("override_armor_assets")
                .map(|overrides| {
                    overrides
                        .iter()
                        .filter_map(|(asset, palette)| {
                            Some((
                                asset.to_str().into_owned(),
                                palette.string()?.to_str().into_owned(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect();
    let patterns = entries("minecraft:trim_pattern")
        .into_iter()
        .map(|(id, element)| {
            element
                .string("asset_id")
                .map_or_else(|| id.to_string(), |asset| asset.to_str().into_owned())
        })
        .collect();
    *REGISTRIES.write().unwrap_or_else(PoisonError::into_inner) = Registries {
        materials,
        patterns,
    };
}

/// What a trim on armour of equipment asset `equipment_asset` draws: its
/// pattern's asset id and the palette its material picks for that armour
/// (vanilla `TrimMaterial.assets().assetId`).
pub fn sprite_parts(trim: &Trim, equipment_asset: &str) -> Option<(String, String)> {
    let registries = REGISTRIES.read().unwrap_or_else(PoisonError::into_inner);
    let material = registries
        .materials
        .get(trim.material.protocol_id() as usize)?;
    let pattern = registries
        .patterns
        .get(trim.pattern.protocol_id() as usize)?;
    let asset = if equipment_asset.contains(':') {
        equipment_asset.to_owned()
    } else {
        format!("minecraft:{equipment_asset}")
    };
    let palette = material
        .overrides
        .get(&asset)
        .unwrap_or(&material.asset_name);
    Some((pattern.clone(), palette.clone()))
}
