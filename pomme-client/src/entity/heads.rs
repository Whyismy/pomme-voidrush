//! Player heads worn as helmets (vanilla `CustomHeadLayer`) and placed as
//! blocks: the head's profile names a skin, which is fetched and kept under
//! a key of its own, never a live player's id, so a player leaving doesn't
//! take the skin of their head items with them.

use std::hash::{Hash, Hasher};

use azalea_inventory::ItemStack;
use azalea_inventory::components::{PartialOrFullProfile, Profile as ItemProfile};
use pomme_protocol::entity_data::Profile;

/// The profile a player head item carries.
pub fn item_profile(stack: &ItemStack) -> Option<Profile> {
    let ItemStack::Present(data) = stack else {
        return None;
    };
    if data.kind != azalea_registry::builtin::ItemKind::PlayerHead {
        return None;
    }
    let profile = data.component_patch.get::<ItemProfile>()?;
    let (name, id, properties) = match &*profile.unpack {
        PartialOrFullProfile::Partial(p) => (p.name.clone(), p.id, &p.properties),
        PartialOrFullProfile::Full(p) => (Some(p.name.clone()), Some(p.uuid), &*p.properties),
    };
    Some(Profile {
        name,
        id: id.map(|id| id.as_u128()),
        textures: properties.map.get("textures").map(|t| t.value.clone()),
        body: None,
        slim: None,
    })
}

/// The profile a placed player head's block entity carries (`profile`: a
/// name, or a compound of id, name and properties).
pub fn block_profile(nbt: &simdnbt::owned::NbtCompound) -> Option<Profile> {
    if let Some(name) = nbt.string("profile") {
        return Some(Profile {
            name: Some(name.to_str().into_owned()),
            ..Profile::default()
        });
    }
    let profile = nbt.compound("profile")?;
    let id = profile
        .int_array("id")
        .filter(|id| id.len() == 4)
        .map(|id| {
            id.iter()
                .fold(0u128, |acc, &part| acc << 32 | u128::from(part as u32))
        });
    let textures = profile
        .list("properties")
        .and_then(|list| list.compounds())
        .and_then(|properties| {
            properties
                .iter()
                .find(|p| p.string("name").is_some_and(|n| n.to_str() == "textures"))
        })
        .and_then(|p| p.string("value"))
        .map(|value| value.to_str().into_owned());
    Some(Profile {
        name: profile.string("name").map(|n| n.to_str().into_owned()),
        id,
        textures,
        body: None,
        slim: None,
    })
}

/// Vanilla `SkullBlockRenderer` for a placed head: where its bottom centre
/// sits within the block (a wall head 0.25 back toward the wall and 0.25
/// up) and its yaw (a floor head's 16 rotation steps, or the wall facing).
pub fn placed_skull(rotation: Option<&str>, facing: Option<&str>) -> Option<(glam::DVec3, f32)> {
    if let Some(rotation) = rotation {
        let steps: f32 = rotation.parse().ok()?;
        return Some((glam::DVec3::new(0.5, 0.0, 0.5), steps * 22.5));
    }
    let (dx, dz, yaw) = match facing? {
        "south" => (0.0, 1.0, 0.0),
        "west" => (-1.0, 0.0, 90.0),
        "north" => (0.0, -1.0, 180.0),
        "east" => (1.0, 0.0, 270.0),
        _ => return None,
    };
    Some((
        glam::DVec3::new(0.5 - dx * 0.25, 0.25, 0.5 - dz * 0.25),
        yaw,
    ))
}

/// The key a head's skin is kept under: a hash of what names the skin.
pub fn skin_key(profile: &Profile) -> uuid::Uuid {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    "player head".hash(&mut hasher);
    (&profile.textures, profile.id, &profile.name).hash(&mut hasher);
    let low = hasher.finish();
    low.hash(&mut hasher);
    uuid::Uuid::from_u64_pair(hasher.finish(), low)
}

/// Vanilla `CustomHeadLayer` with `SkullBlockRenderer` on an armor stand:
/// the skull is 1.1875 times a head cube, standing on the head's pivot, with
/// the head part's scale. Returns the pivot's height above the stand's feet
/// and the skull's size, both in blocks.
pub fn armor_stand_skull(small: bool) -> (f64, f32) {
    // `armor_stand` / `armor_stand_small` head poses: y 1 or 12.75 pixels
    // down from the model's 24-pixel top, scale 1 or 0.75.
    let (pivot_y, scale) = if small { (12.75, 0.75) } else { (1.0, 1.0) };
    ((24.0 - pivot_y) / 16.0, 8.0 * 1.1875 / 16.0 * scale)
}

/// Where a player model drawn head-only puts a skull `size` blocks across
/// with its bottom `pivot` blocks above the origin: the model's own scale
/// and its height offset. The player model's head cube is 8 pixels at
/// `PlayerRenderer`'s 0.9375, its pivot 1.501 blocks up (the model's
/// 24.016-pixel rebase).
pub fn player_head_placement(pivot: f64, size: f32) -> (f32, f64) {
    const HEAD: f32 = 0.5 * 0.9375;
    const PIVOT: f64 = 1.501 * 0.9375;
    let scale = size / HEAD;
    (scale, pivot - PIVOT * f64::from(scale))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_placed_head_reads_its_profile_and_placement() {
        use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
        let property = NbtCompound::from_values(vec![
            ("name".into(), NbtTag::String("textures".into())),
            ("value".into(), NbtTag::String("e30=".into())),
        ]);
        let profile = NbtCompound::from_values(vec![
            ("id".into(), NbtTag::IntArray(vec![0, 0, 0, 7])),
            ("name".into(), NbtTag::String("Notch".into())),
            (
                "properties".into(),
                NbtTag::List(NbtList::Compound(vec![property])),
            ),
        ]);
        let nbt = NbtCompound::from_values(vec![("profile".into(), NbtTag::Compound(profile))]);
        assert_eq!(
            block_profile(&nbt),
            Some(Profile {
                name: Some("Notch".into()),
                id: Some(7),
                textures: Some("e30=".into()),
                body: None,
                slim: None,
            })
        );
        let by_name =
            NbtCompound::from_values(vec![("profile".into(), NbtTag::String("jeb_".into()))]);
        assert_eq!(
            block_profile(&by_name).unwrap().name.as_deref(),
            Some("jeb_")
        );

        assert_eq!(
            placed_skull(Some("4"), None),
            Some((glam::DVec3::new(0.5, 0.0, 0.5), 90.0))
        );
        // A head on a wall facing north hangs against the wall to its south.
        assert_eq!(
            placed_skull(None, Some("north")),
            Some((glam::DVec3::new(0.5, 0.25, 0.75), 180.0))
        );
    }

    #[test]
    fn heads_key_their_skins_apart_from_players() {
        let profile = Profile {
            id: Some(7),
            ..Profile::default()
        };
        let key = skin_key(&profile);
        assert_ne!(key, uuid::Uuid::from_u128(7));
        assert_eq!(key, skin_key(&profile));
        let other = Profile {
            textures: Some("e30=".into()),
            ..profile
        };
        assert_ne!(skin_key(&other), key);
    }
}
