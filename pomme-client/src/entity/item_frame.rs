//! Item frames (vanilla `ItemFrame` and `GlowItemFrame`): a frame hanging
//! in a block's space against the block behind it, holding an item turned
//! in eighths of a turn, or a map filling the whole face.

use std::collections::HashMap;
use std::sync::Arc;

use azalea_inventory::ItemStackData;
use glam::{DVec3, Mat4, Vec3};

use crate::entity::MetaValue;
use crate::entity::components::Position;
use crate::physics::aabb::Aabb;

/// `Entity.DATA_SHARED_FLAGS_ID` and its invisible bit.
const SHARED_FLAGS: u8 = 0;
const FLAG_INVISIBLE: u8 = 0x20;

/// `ItemFrame.DATA_ROTATION` is the frame's last value: index 9, or 10 on
/// versions that sync the facing before the item. Either way it's the only
/// int past the entity's own.
const FIRST_FRAME_INDEX: u8 = 8;

/// How far the frame's centre sits back from its block's centre, toward the
/// wall (`ItemFrame.calculateBoundingBox`).
const WALL_SHIFT: f64 = 0.46875;

/// `Direction`, in `get3DDataValue` order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Facing {
    Down,
    Up,
    North,
    South,
    West,
    East,
}

impl Facing {
    /// `Direction.from3DDataValue`, which wraps.
    pub fn from_3d_data(data: i32) -> Self {
        [
            Self::Down,
            Self::Up,
            Self::North,
            Self::South,
            Self::West,
            Self::East,
        ][data.rem_euclid(6) as usize]
    }

    /// `Direction.getUnitVec3`.
    pub fn step(self) -> DVec3 {
        match self {
            Self::Down => DVec3::NEG_Y,
            Self::Up => DVec3::Y,
            Self::North => DVec3::NEG_Z,
            Self::South => DVec3::Z,
            Self::West => DVec3::NEG_X,
            Self::East => DVec3::X,
        }
    }

    /// `Direction.toYRot` for the sideways ones.
    fn y_rot(self) -> Option<f32> {
        match self {
            Self::South => Some(0.0),
            Self::West => Some(90.0),
            Self::North => Some(180.0),
            Self::East => Some(270.0),
            Self::Down | Self::Up => None,
        }
    }
}

/// The frame's item ready to draw, as of an asset generation.
pub struct FrameItemRender {
    pub generation: u32,
    pub model: Arc<str>,
    pub tints: Arc<[u32]>,
    /// The model's `fixed` display transform.
    pub transform: Mat4,
    /// The stack has the enchantment glint.
    pub foil: bool,
}

pub struct ItemFrame {
    pub glow: bool,
    /// The block space it hangs in (`BlockAttachedEntity.pos`).
    pub block: (i32, i32, i32),
    pub facing: Facing,
    pub item: Option<ItemStackData>,
    /// Eighths of a turn (`ItemFrame.getRotation`).
    pub rotation: u8,
    pub invisible: bool,
    /// Cached by the renderer's caller; dropped when the item changes.
    pub item_render: Option<FrameItemRender>,
}

impl ItemFrame {
    /// The centre of the block space it hangs in, which its model is built
    /// around.
    pub fn block_center(&self) -> DVec3 {
        DVec3::new(
            f64::from(self.block.0) + 0.5,
            f64::from(self.block.1) + 0.5,
            f64::from(self.block.2) + 0.5,
        )
    }

    /// The entity's own position: the frame's centre, against the wall.
    pub fn position(&self) -> DVec3 {
        self.block_center() - self.facing.step() * WALL_SHIFT
    }

    /// `ItemFrame.calculateBoundingBox`: 12 pixels square, one thick.
    pub fn bounding_box(&self) -> Aabb {
        let half = (DVec3::splat(0.375) - self.facing.step().abs() * 0.34375).abs();
        let center = self.position();
        Aabb::new(center - half, center + half)
    }

    /// The map the frame shows in place of an item.
    pub fn map_id(&self) -> Option<i32> {
        let map = self
            .item
            .as_ref()?
            .get_component::<azalea_inventory::components::MapId>()?;
        Some(map.id)
    }

    /// Vanilla `ItemFrameRenderer.submit`'s turn from the frame's model
    /// space (built facing north at the block's south side) to its facing,
    /// about the block's centre.
    pub fn facing_matrix(&self) -> Mat4 {
        let (x_rot, y_rot) = match self.facing.y_rot() {
            Some(y_rot) => (0.0, 180.0 - y_rot),
            None => (-90.0 * self.facing.step().y as f32, 180.0),
        };
        Mat4::from_rotation_x(x_rot.to_radians()) * Mat4::from_rotation_y(y_rot.to_radians())
    }

    /// Where the frame's contents sit in its model space: just in front of
    /// the backing, or flat against the wall with the frame unseen.
    pub fn contents_offset(&self) -> Mat4 {
        let z = if self.invisible { 0.5 } else { 0.4375 };
        Mat4::from_translation(Vec3::new(0.0, 0.0, z))
    }

    /// The held item's pose in the frame's model space: turned by the
    /// rotation and at half size (the `fixed` display transform follows).
    pub fn item_matrix(&self) -> Mat4 {
        self.contents_offset()
            * Mat4::from_rotation_z((f32::from(self.rotation) * 45.0).to_radians())
            * Mat4::from_scale(Vec3::splat(0.5))
    }

    /// A held map's 128-unit space in the frame's model space: maps turn in
    /// quarters, and fill the block's face.
    pub fn map_matrix(&self) -> Mat4 {
        let quarter_turns = f32::from(self.rotation % 4);
        self.contents_offset()
            * Mat4::from_rotation_z((quarter_turns * 90.0 + 180.0).to_radians())
            * Mat4::from_scale(Vec3::splat(0.0078125))
            * Mat4::from_translation(Vec3::new(-64.0, -64.0, -1.0))
    }
}

#[derive(Default)]
pub struct ItemFrameStore {
    pub frames: HashMap<i32, ItemFrame>,
}

/// `BlockPos.containing`.
fn block_of(position: Position) -> (i32, i32, i32) {
    let p = DVec3::from(position);
    (p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32)
}

impl ItemFrameStore {
    /// `data` is the spawn packet's: the facing
    /// (`ItemFrame.recreateFromPacket`).
    pub fn spawn(&mut self, id: i32, glow: bool, position: Position, data: i32) {
        self.frames.insert(
            id,
            ItemFrame {
                glow,
                block: block_of(position),
                facing: Facing::from_3d_data(data),
                item: None,
                rotation: 0,
                invisible: false,
                item_render: None,
            },
        );
    }

    /// `BlockAttachedEntity.setPos`: the frame snaps to the block it's in.
    pub fn teleport(&mut self, id: i32, position: Position) {
        if let Some(frame) = self.frames.get_mut(&id) {
            frame.block = block_of(position);
        }
    }

    pub fn set_item(&mut self, id: i32, item: Option<ItemStackData>) {
        if let Some(frame) = self.frames.get_mut(&id) {
            frame.item = item;
            frame.item_render = None;
        }
    }

    pub fn apply_entity_data(&mut self, id: i32, index: u8, value: MetaValue) {
        let Some(frame) = self.frames.get_mut(&id) else {
            return;
        };
        match (index, value) {
            (SHARED_FLAGS, MetaValue::Byte(flags)) => frame.invisible = flags & FLAG_INVISIBLE != 0,
            (index, MetaValue::Int(rotation)) if index > FIRST_FRAME_INDEX => {
                frame.rotation = rotation.rem_euclid(8) as u8;
            }
            _ => {}
        }
    }

    pub fn remove(&mut self, ids: &[i32]) {
        for id in ids {
            self.frames.remove(id);
        }
    }

    pub fn clear(&mut self) {
        self.frames.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(position: [f64; 3], data: i32) -> ItemFrame {
        let mut store = ItemFrameStore::default();
        store.spawn(
            1,
            false,
            Position::new(position[0], position[1], position[2]),
            data,
        );
        store.frames.remove(&1).unwrap()
    }

    /// The server sends the frame's centre against the wall; the client
    /// works back to the block and out to the same place.
    #[test]
    fn a_frame_snaps_to_its_block_and_wall() {
        // On the north face of a block (facing north), in block (4, 64, -3).
        let north = frame([4.5, 64.5, -2.03125], 2);
        assert_eq!(north.block, (4, 64, -3));
        assert_eq!(north.facing, Facing::North);
        assert!((north.position() - DVec3::new(4.5, 64.5, -2.03125)).length() < 1e-9);
        let aabb = north.bounding_box();
        assert!((aabb.min - DVec3::new(4.125, 64.125, -2.0625)).length() < 1e-9);
        assert!((aabb.max - DVec3::new(4.875, 64.875, -2.0)).length() < 1e-9);

        // On a floor, facing up: thin in y.
        let up = frame([0.5, 10.03125, 0.5], 1);
        assert_eq!(up.block, (0, 10, 0));
        let aabb = up.bounding_box();
        assert!((aabb.min - DVec3::new(0.125, 10.0, 0.125)).length() < 1e-9);
        assert!((aabb.max - DVec3::new(0.875, 10.0625, 0.875)).length() < 1e-9);
    }

    /// The model is built against the block's south side facing north; the
    /// facing turn carries its front out the way the frame faces.
    #[test]
    fn the_model_turns_to_the_facing() {
        let front = Vec3::NEG_Z;
        for (data, toward) in [
            (2, Vec3::NEG_Z),
            (3, Vec3::Z),
            (4, Vec3::NEG_X),
            (5, Vec3::X),
            (1, Vec3::Y),
            (0, Vec3::NEG_Y),
        ] {
            let turned = frame([0.5; 3], data)
                .facing_matrix()
                .transform_vector3(front);
            assert!((turned - toward).length() < 1e-5, "{data}: {turned}");
        }
    }

    /// Rotation is the int after the frame's item, wherever the version
    /// puts it; the entity's own ints (air, frozen ticks) aren't it.
    #[test]
    fn rotation_and_invisibility_come_from_the_data() {
        let mut store = ItemFrameStore::default();
        store.spawn(1, true, Position::new(0.5, 0.5, 0.5), 3);
        store.apply_entity_data(1, 1, MetaValue::Int(300));
        store.apply_entity_data(1, 7, MetaValue::Int(5));
        assert_eq!(store.frames[&1].rotation, 0);
        store.apply_entity_data(1, 9, MetaValue::Int(3));
        assert_eq!(store.frames[&1].rotation, 3);
        store.apply_entity_data(1, 10, MetaValue::Int(13));
        assert_eq!(store.frames[&1].rotation, 5);
        store.apply_entity_data(1, 0, MetaValue::Byte(0x20));
        assert!(store.frames[&1].invisible);
        // A map sits a quarter turn per step, upside up at none.
        let map = store.frames[&1].map_matrix();
        assert!(
            map.transform_point3(Vec3::new(64.0, 64.0, 1.0))
                .abs_diff_eq(Vec3::new(0.0, 0.0, 0.5), 1e-6)
        );
    }
}
