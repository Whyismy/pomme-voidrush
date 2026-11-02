//! Names over entities (vanilla `EntityRenderer.submitNameTag`): a custom
//! name an entity shows (`CustomNameVisible`, how servers float hologram
//! text on invisible marker armor stands and name their NPCs), and other
//! players' names, drawn in the world facing the camera at 0.025 a pixel.
//! The store keeps what the entity data says about each entity's name; the
//! positions come from the entity stores.

use std::collections::HashMap;
use std::sync::Arc;

use azalea_registry::builtin::EntityKind;

use crate::entity::MetaValue;
use crate::entity::display::TextLayout;
use crate::ui::text::TextSpan;

/// `Entity.DATA_CUSTOM_NAME_VISIBLE`.
const NAME_VISIBLE: u8 = 3;
/// `ArmorStand.DATA_CLIENT_FLAGS`: small (0x01), marker (0x10).
const ARMOR_STAND_FLAGS: u8 = 15;
const SMALL: u8 = 0x01;
const MARKER: u8 = 0x10;
/// `Entity.FLAG_INVISIBLE` in the shared flags.
const INVISIBLE: u8 = 0x20;

#[derive(Default)]
pub struct Named {
    pub kind: Option<EntityKind>,
    pub name: Option<Vec<TextSpan>>,
    pub name_visible: bool,
    pub invisible: bool,
    armor_stand_flags: u8,
    /// The name laid out, until it changes.
    pub layout: Option<Arc<TextLayout>>,
}

impl Named {
    /// Where the name hangs above the entity's feet: its box's top
    /// (`EntityAttachment.NAME_TAG`), which the renderer lifts 0.5 more.
    pub fn name_height(&self) -> f32 {
        match self.kind {
            Some(EntityKind::ArmorStand) => {
                if self.armor_stand_flags & MARKER != 0 {
                    0.0
                } else if self.armor_stand_flags & SMALL != 0 {
                    0.9875
                } else {
                    1.975
                }
            }
            Some(kind) => crate::entity::dimensions::EntityDimensions::of(kind).height,
            None => 0.0,
        }
    }
}

#[derive(Default)]
pub struct NameStore {
    pub entities: HashMap<i32, Named>,
}

impl NameStore {
    pub fn spawn(&mut self, id: i32, kind: EntityKind) {
        self.entities.insert(
            id,
            Named {
                kind: Some(kind),
                ..Named::default()
            },
        );
    }

    pub fn remove(&mut self, ids: &[i32]) {
        for id in ids {
            self.entities.remove(id);
        }
    }

    pub fn clear(&mut self) {
        self.entities.clear();
    }

    pub fn set_name(&mut self, id: i32, name: Option<Vec<TextSpan>>) {
        let named = self.entities.entry(id).or_default();
        if named.name != name {
            named.name = name;
            named.layout = None;
        }
    }

    pub fn apply_entity_data(&mut self, id: i32, index: u8, value: MetaValue) {
        let Some(named) = self.entities.get_mut(&id) else {
            return;
        };
        match (index, value) {
            (0, MetaValue::Byte(flags)) => named.invisible = flags & INVISIBLE != 0,
            (NAME_VISIBLE, MetaValue::Bool(visible)) => named.name_visible = visible,
            (ARMOR_STAND_FLAGS, MetaValue::Byte(flags))
                if named.kind == Some(EntityKind::ArmorStand) =>
            {
                named.armor_stand_flags = flags;
            }
            _ => {}
        }
    }

    /// The entities whose own name shows: a custom name made visible. An
    /// invisible one still shows it (vanilla hides only the body), which is
    /// what hologram armor stands rely on.
    pub fn shown(&mut self) -> impl Iterator<Item = (&i32, &mut Named)> {
        self.entities
            .iter_mut()
            .filter(|(_, n)| n.name_visible && n.name.as_ref().is_some_and(|s| !s.is_empty()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str) -> TextSpan {
        TextSpan::new(text.into(), [1.0; 4])
    }

    #[test]
    fn a_marker_armor_stand_hologram_shows_its_name_at_its_feet() {
        let mut store = NameStore::default();
        store.spawn(7, EntityKind::ArmorStand);
        store.set_name(7, Some(vec![span("Lvl 3 Station")]));
        store.apply_entity_data(7, 0, MetaValue::Byte(INVISIBLE));
        store.apply_entity_data(7, NAME_VISIBLE, MetaValue::Bool(true));
        store.apply_entity_data(7, ARMOR_STAND_FLAGS, MetaValue::Byte(MARKER));
        let shown: Vec<_> = store
            .shown()
            .map(|(id, n)| (*id, n.name_height()))
            .collect();
        assert_eq!(shown, [(7, 0.0)]);
        // A name not made visible stays hidden, and changing it drops the layout.
        store.spawn(8, EntityKind::Villager);
        store.set_name(8, Some(vec![span("Vendor")]));
        assert_eq!(store.shown().count(), 1);
        store.apply_entity_data(8, NAME_VISIBLE, MetaValue::Bool(true));
        assert_eq!(store.shown().count(), 2);
        assert!((store.entities[&8].name_height() - 1.95).abs() < 1e-6);
    }
}
