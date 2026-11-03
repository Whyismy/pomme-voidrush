//! Display entities (`Display` and its block, item and text subclasses): their
//! synched data, vanilla's client-side render state with its transformation
//! interpolation, and the per-frame placement the renderers draw from.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;

use azalea_inventory::ItemStackData;
use azalea_registry::builtin::EntityKind;
use glam::{DVec3, EulerRot, Mat4, Quat, Vec3};

use crate::entity::MetaValue;
use crate::ui::text::TextSpan;

/// A display entity's non-scalar synched value.
#[derive(Clone, Debug)]
pub enum DisplayValue {
    Vector3(Vec3),
    Quaternion(Quat),
    /// A block state id in the session's id space.
    BlockState(u32),
    Text(Vec<TextSpan>),
    Item(Option<Box<ItemStackData>>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayKind {
    Block,
    Item,
    Text,
}

impl DisplayKind {
    pub fn of(kind: EntityKind) -> Option<Self> {
        match kind {
            EntityKind::BlockDisplay => Some(Self::Block),
            EntityKind::ItemDisplay => Some(Self::Item),
            EntityKind::TextDisplay => Some(Self::Text),
            _ => None,
        }
    }
}

/// `Display.BillboardConstraints`, by id.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Billboard {
    #[default]
    Fixed,
    Vertical,
    Horizontal,
    Center,
}

impl Billboard {
    /// `BY_ID` with `OutOfBoundsStrategy.ZERO`.
    fn from_id(id: u8) -> Self {
        match id {
            1 => Self::Vertical,
            2 => Self::Horizontal,
            3 => Self::Center,
            _ => Self::Fixed,
        }
    }
}

/// `com.mojang.math.Transformation`: translation, left rotation, scale and
/// right rotation, applied in that order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transformation {
    pub translation: Vec3,
    pub left_rotation: Quat,
    pub scale: Vec3,
    pub right_rotation: Quat,
}

impl Transformation {
    pub const IDENTITY: Self = Self {
        translation: Vec3::ZERO,
        left_rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
        right_rotation: Quat::IDENTITY,
    };

    /// `Transformation.slerp`.
    fn slerp(&self, to: &Self, t: f32) -> Self {
        Self {
            translation: self.translation.lerp(to.translation, t),
            left_rotation: self.left_rotation.slerp(to.left_rotation, t),
            scale: self.scale.lerp(to.scale, t),
            right_rotation: self.right_rotation.slerp(to.right_rotation, t),
        }
    }

    /// `Transformation.getMatrix`.
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_translation(self.translation)
            * Mat4::from_quat(self.left_rotation.normalize())
            * Mat4::from_scale(self.scale)
            * Mat4::from_quat(self.right_rotation.normalize())
    }
}

/// The synched values, `Display.defineSynchedData` defaults.
#[derive(Clone, Debug)]
struct Synced {
    interpolation_delay: i32,
    interpolation_duration: i32,
    pos_rot_interpolation_duration: i32,
    transformation: Transformation,
    billboard: Billboard,
    /// `Brightness.pack`, -1 for none.
    brightness: i32,
    view_range: f32,
    width: f32,
    height: f32,
    glow_color: i32,
    block_state: u32,
    item: Option<Box<ItemStackData>>,
    item_context: u8,
    text: Vec<TextSpan>,
    line_width: i32,
    background: i32,
    /// The signed byte, as vanilla keeps it.
    text_opacity: i32,
    text_flags: u8,
}

impl Default for Synced {
    fn default() -> Self {
        Self {
            interpolation_delay: 0,
            interpolation_duration: 0,
            pos_rot_interpolation_duration: 0,
            transformation: Transformation::IDENTITY,
            billboard: Billboard::Fixed,
            brightness: -1,
            view_range: 1.0,
            width: 0.0,
            height: 0.0,
            glow_color: -1,
            block_state: 0,
            item: None,
            item_context: 0,
            text: Vec::new(),
            line_width: 200,
            background: 0x4000_0000,
            text_opacity: -1,
            text_flags: 0,
        }
    }
}

/// `Display.RenderState`: the transformation and the values that interpolate
/// with it.
#[derive(Clone, Debug)]
struct RenderState {
    from: Transformation,
    to: Transformation,
    billboard: Billboard,
    brightness: i32,
}

impl RenderState {
    fn transformation(&self, progress: f32) -> Transformation {
        if self.from == self.to {
            self.to
        } else {
            self.from.slerp(&self.to, progress)
        }
    }
}

/// `Display.TextDisplay.TextRenderState`.
#[derive(Clone, Debug)]
struct TextState {
    text: Vec<TextSpan>,
    line_width: i32,
    opacity: (i32, i32),
    background: (i32, i32),
    flags: u8,
}

/// `Display.TextDisplay.FLAG_*`.
pub const TEXT_SHADOW: u8 = 1;
pub const TEXT_SEE_THROUGH: u8 = 2;
pub const TEXT_DEFAULT_BACKGROUND: u8 = 4;
const TEXT_ALIGN_LEFT: u8 = 8;
const TEXT_ALIGN_RIGHT: u8 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAlign {
    Center,
    Left,
    Right,
}

/// A text display's wrapped lines, `Display.TextDisplay.CachedInfo`.
#[derive(Debug, Default)]
pub struct TextLayout {
    pub lines: Vec<(Vec<TextSpan>, f32)>,
    pub width: f32,
}

/// An item display's resolved model, tints, display-context transform and
/// mesh bounds, kept until its item changes or the meshes are rebuilt.
#[derive(Debug)]
pub struct ItemDisplayRender {
    pub generation: u32,
    pub model: Arc<str>,
    pub tints: Arc<[u32]>,
    pub transform: Mat4,
    pub bounds: (Vec3, Vec3),
}

/// The entity's own position and rotation lerp, `InterpolationHandler`.
#[derive(Clone, Copy, Debug)]
struct Lerp {
    position: DVec3,
    y_rot: f32,
    x_rot: f32,
    steps: i32,
}

#[derive(Debug)]
pub struct DisplayEntity {
    pub kind: DisplayKind,
    pub position: DVec3,
    pub prev_position: DVec3,
    pub y_rot: f32,
    pub x_rot: f32,
    pub prev_y_rot: f32,
    pub prev_x_rot: f32,
    /// Where the server last put it; move deltas apply to this.
    server_position: DVec3,
    lerp: Option<Lerp>,
    pub vehicle: Option<i32>,
    shared_flags: u8,
    tick_count: i64,
    synced: Synced,
    update_start_tick: bool,
    update_duration: bool,
    update_render_state: bool,
    interpolation_start: i64,
    interpolation_duration: i32,
    last_progress: Cell<f32>,
    render: Option<RenderState>,
    block_state: Option<u32>,
    item: Option<(Box<ItemStackData>, u8)>,
    text: Option<TextState>,
    /// Filled by the renderer the first time the text is drawn.
    pub text_layout: Option<Arc<TextLayout>>,
    /// Filled by the renderer the first time the item is drawn.
    pub item_render: Option<ItemDisplayRender>,
}

impl DisplayEntity {
    fn new(kind: DisplayKind, position: DVec3, y_rot: f32, x_rot: f32) -> Self {
        Self {
            kind,
            position,
            prev_position: position,
            y_rot,
            x_rot,
            prev_y_rot: y_rot,
            prev_x_rot: x_rot,
            server_position: position,
            lerp: None,
            vehicle: None,
            shared_flags: 0,
            tick_count: 0,
            synced: Synced::default(),
            update_start_tick: false,
            update_duration: false,
            // A spawn's data arrives in the same tick; the first tick builds
            // the render state from it.
            update_render_state: true,
            interpolation_start: i64::from(i32::MIN),
            interpolation_duration: 0,
            last_progress: Cell::new(0.0),
            render: None,
            block_state: None,
            item: None,
            text: None,
            text_layout: None,
            item_render: None,
        }
    }

    /// `Display.onSyncedDataUpdated` for one scalar.
    fn apply_scalar(&mut self, index: u8, value: MetaValue) {
        let s = &mut self.synced;
        match (self.kind, index, value) {
            (_, 0, MetaValue::Byte(flags)) => self.shared_flags = flags,
            (_, 8, MetaValue::Int(v)) => {
                s.interpolation_delay = v;
                self.update_start_tick = true;
            }
            (_, 9, MetaValue::Int(v)) => {
                s.interpolation_duration = v;
                self.update_duration = true;
            }
            (_, 10, MetaValue::Int(v)) => s.pos_rot_interpolation_duration = v,
            (_, 15, MetaValue::Byte(v)) => {
                s.billboard = Billboard::from_id(v);
                self.update_render_state = true;
            }
            (_, 16, MetaValue::Int(v)) => {
                s.brightness = v;
                self.update_render_state = true;
            }
            (_, 17, MetaValue::Float(v)) => s.view_range = v,
            // Shadow radius and strength: drawn as part of the render state
            // in vanilla; no shadow here yet.
            (_, 18 | 19, MetaValue::Float(_)) => self.update_render_state = true,
            (_, 20, MetaValue::Float(v)) => s.width = v,
            (_, 21, MetaValue::Float(v)) => s.height = v,
            (_, 22, MetaValue::Int(v)) => s.glow_color = v,
            (DisplayKind::Item, 24, MetaValue::Byte(v)) => {
                s.item_context = v;
                self.update_render_state = true;
            }
            (DisplayKind::Text, 24, MetaValue::Int(v)) => {
                s.line_width = v;
                self.update_render_state = true;
            }
            (DisplayKind::Text, 25, MetaValue::Int(v)) => {
                s.background = v;
                self.update_render_state = true;
            }
            (DisplayKind::Text, 26, MetaValue::Byte(v)) => {
                s.text_opacity = i32::from(v as i8);
                self.update_render_state = true;
            }
            (DisplayKind::Text, 27, MetaValue::Byte(v)) => {
                s.text_flags = v;
                self.update_render_state = true;
            }
            _ => {}
        }
    }

    fn apply_value(&mut self, index: u8, value: DisplayValue) {
        let s = &mut self.synced;
        let t = &mut s.transformation;
        match (self.kind, index, value) {
            (_, 11, DisplayValue::Vector3(v)) => t.translation = v,
            (_, 12, DisplayValue::Vector3(v)) => t.scale = v,
            (_, 13, DisplayValue::Quaternion(q)) => t.left_rotation = q,
            (_, 14, DisplayValue::Quaternion(q)) => t.right_rotation = q,
            (DisplayKind::Block, 23, DisplayValue::BlockState(state)) => s.block_state = state,
            (DisplayKind::Item, 23, DisplayValue::Item(item)) => s.item = item,
            (DisplayKind::Text, 23, DisplayValue::Text(text)) => s.text = text,
            _ => return,
        }
        self.update_render_state = true;
    }

    /// `ClientLevel.tickNonPassenger` then `Display.tick`.
    fn tick(&mut self) {
        self.prev_position = self.position;
        self.prev_y_rot = self.y_rot;
        self.prev_x_rot = self.x_rot;
        self.tick_count += 1;

        if self.update_start_tick {
            self.update_start_tick = false;
            self.interpolation_start = self.tick_count + i64::from(self.synced.interpolation_delay);
        }
        if self.update_duration {
            self.update_duration = false;
            self.interpolation_duration = self.synced.interpolation_duration;
        }
        if self.update_render_state {
            self.update_render_state = false;
            let interpolating = self.interpolation_duration != 0;
            let progress = self.last_progress.get();
            let current = self.synced.transformation;
            let from = match (&self.render, interpolating) {
                (Some(render), true) => render.transformation(progress),
                _ => current,
            };
            self.render = Some(RenderState {
                from,
                to: current,
                billboard: self.synced.billboard,
                brightness: self.synced.brightness,
            });
            self.update_sub_state(interpolating, progress);
        }

        if let Some(lerp) = &mut self.lerp {
            // `InterpolationHandler.interpolate`.
            let step = 1.0 / f64::from(lerp.steps);
            self.position = self.position.lerp(lerp.position, step);
            self.y_rot = rot_lerp(step as f32, self.y_rot, lerp.y_rot);
            self.x_rot += (lerp.x_rot - self.x_rot) * step as f32;
            lerp.steps -= 1;
            if lerp.steps <= 0 {
                self.lerp = None;
            }
        }
    }

    /// `updateRenderSubState` of each subclass.
    fn update_sub_state(&mut self, interpolating: bool, progress: f32) {
        match self.kind {
            DisplayKind::Block => self.block_state = Some(self.synced.block_state),
            DisplayKind::Item => {
                self.item = self
                    .synced
                    .item
                    .clone()
                    .map(|item| (item, self.synced.item_context));
                self.item_render = None;
            }
            DisplayKind::Text => {
                let s = &self.synced;
                let (opacity, background) = match (&self.text, interpolating) {
                    (Some(text), true) => (
                        lerp_int(progress, text.opacity.0, text.opacity.1),
                        lerp_argb(progress, text.background.0, text.background.1),
                    ),
                    _ => (s.text_opacity, s.background),
                };
                self.text = Some(TextState {
                    text: s.text.clone(),
                    line_width: s.line_width,
                    opacity: (opacity, s.text_opacity),
                    background: (background, s.background),
                    flags: s.text_flags,
                });
                self.text_layout = None;
            }
        }
    }

    /// `InterpolationHandler.interpolateTo`, or a snap with no duration.
    fn move_to(&mut self, position: DVec3, rotation: Option<(f32, f32)>) {
        self.server_position = position;
        let (y_rot, x_rot) = rotation.unwrap_or((self.y_rot, self.x_rot));
        // A passenger sits where its vehicle puts it every tick
        // (`positionRider`), so where the server has it never shows: a snap
        // here would draw it sliding back from there until the next tick
        // (worn wings, seated by the server's packet but moved only to stay
        // tracked).
        if self.vehicle.is_some() {
            self.y_rot = y_rot;
            self.x_rot = x_rot;
            return;
        }
        let steps = self.synced.pos_rot_interpolation_duration;
        if steps > 0 {
            self.lerp = Some(Lerp {
                position,
                y_rot,
                x_rot,
                steps,
            });
        } else {
            // `Entity.snapTo` also resets the old position and rotation.
            self.lerp = None;
            self.position = position;
            self.prev_position = position;
            self.y_rot = y_rot;
            self.x_rot = x_rot;
            self.prev_y_rot = y_rot;
            self.prev_x_rot = x_rot;
        }
    }

    /// `Display.calculateInterpolationProgress`.
    pub fn interpolation_progress(&self, partial_tick: f32) -> f32 {
        if self.interpolation_duration <= 0 {
            return 1.0;
        }
        let elapsed = (self.tick_count - self.interpolation_start) as f32 + partial_tick;
        let progress = (elapsed / self.interpolation_duration as f32).clamp(0.0, 1.0);
        self.last_progress.set(progress);
        progress
    }

    pub fn is_glowing(&self) -> bool {
        crate::entity::shared_flags_glowing(self.shared_flags)
    }

    /// `Display.getTeamColor`'s override, `None` for the team colour.
    pub fn glow_color_override(&self) -> Option<u32> {
        (self.synced.glow_color != -1).then_some(self.synced.glow_color as u32 & 0xFF_FFFF)
    }

    /// `Display.shouldRenderAtSqrDistance` with the default entity distance
    /// scaling, and `getBoundingBoxForCulling` when it has a size.
    pub fn in_view_range(&self, camera: DVec3, lerped: DVec3) -> bool {
        let range = f64::from(self.synced.view_range) * 64.0;
        lerped.distance_squared(camera) < range * range
    }

    /// The culling box, `None` when vanilla never frustum-culls it.
    pub fn culling_box(&self, lerped: DVec3) -> Option<(DVec3, DVec3)> {
        let (w, h) = (f64::from(self.synced.width), f64::from(self.synced.height));
        if w == 0.0 || h == 0.0 {
            return None;
        }
        Some((
            lerped - DVec3::new(w / 2.0, 0.0, w / 2.0),
            lerped + DVec3::new(w / 2.0, h, w / 2.0),
        ))
    }

    /// `Display.getPackedBrightnessOverride` as (sky, block) levels.
    pub fn brightness_override(&self) -> Option<(u8, u8)> {
        let packed = self.render.as_ref()?.brightness;
        (packed != -1).then_some((((packed >> 20) & 0xF) as u8, ((packed >> 4) & 0xF) as u8))
    }

    /// The entity-to-world rotation and the display's transformation,
    /// `DisplayRenderer.submit`'s pose, below the entity's position.
    /// `camera_rot` is the camera's (yaw, pitch) in degrees.
    pub fn pose(&self, partial_tick: f32, camera_rot: (f32, f32)) -> Option<Mat4> {
        let render = self.render.as_ref()?;
        let progress = self.interpolation_progress(partial_tick);
        let y_rot = rot_lerp(partial_tick, self.prev_y_rot, self.y_rot);
        let x_rot = self.prev_x_rot + (self.x_rot - self.prev_x_rot) * partial_tick;
        let (camera_y, camera_x) = camera_rot;
        let (yaw, pitch) = match render.billboard {
            Billboard::Fixed => (-y_rot, x_rot),
            Billboard::Horizontal => (-y_rot, -camera_x),
            Billboard::Vertical => (-(camera_y - 180.0), x_rot),
            Billboard::Center => (-(camera_y - 180.0), -camera_x),
        };
        let orientation =
            Quat::from_euler(EulerRot::YXZ, yaw.to_radians(), pitch.to_radians(), 0.0);
        Some(Mat4::from_quat(orientation) * render.transformation(progress).matrix())
    }

    pub fn block_state(&self) -> Option<u32> {
        self.block_state
    }

    /// The item and its `ItemDisplayContext` id.
    pub fn item(&self) -> Option<(&ItemStackData, u8)> {
        self.item
            .as_ref()
            .map(|(item, context)| (&**item, *context))
    }

    pub fn text(&self) -> Option<(&[TextSpan], i32)> {
        self.text
            .as_ref()
            .map(|text| (text.text.as_slice(), text.line_width))
    }

    pub fn text_flags(&self) -> u8 {
        self.text.as_ref().map_or(0, |text| text.flags)
    }

    /// `Display.TextDisplay.getAlign`.
    pub fn text_align(&self) -> TextAlign {
        let flags = self.text_flags();
        if flags & TEXT_ALIGN_LEFT != 0 {
            TextAlign::Left
        } else if flags & TEXT_ALIGN_RIGHT != 0 {
            TextAlign::Right
        } else {
            TextAlign::Center
        }
    }

    /// `TextDisplayRenderer.submitInner`'s text colour: the opacity byte over
    /// white, with `Font.adjustColor` making a near-zero alpha opaque.
    pub fn text_color(&self, partial_tick: f32) -> u32 {
        let Some(text) = &self.text else {
            return 0xFFFF_FFFF;
        };
        let progress = self.interpolation_progress(partial_tick);
        let opacity = lerp_int(progress, text.opacity.0, text.opacity.1) as i8;
        let color = (i32::from(opacity) << 24) as u32 | 0xFF_FFFF;
        if color & 0xFC00_0000 == 0 {
            color | 0xFF00_0000
        } else {
            color
        }
    }

    /// The background's ARGB, 0 for none. `default_opacity` is the text
    /// background opacity option.
    pub fn text_background(&self, partial_tick: f32, default_opacity: f32) -> u32 {
        let Some(text) = &self.text else {
            return 0;
        };
        if text.flags & TEXT_DEFAULT_BACKGROUND != 0 {
            return ((default_opacity * 255.0) as u32) << 24;
        }
        let progress = self.interpolation_progress(partial_tick);
        lerp_argb(progress, text.background.0, text.background.1) as u32
    }
}

/// `Mth.lerpInt`.
fn lerp_int(t: f32, from: i32, to: i32) -> i32 {
    from + (t * (to - from) as f32).floor() as i32
}

/// `ARGB.lerp`: each channel by `lerp_int`.
fn lerp_argb(t: f32, from: i32, to: i32) -> i32 {
    let channel = |shift: u32| {
        let a = (from >> shift) & 0xFF;
        let b = (to >> shift) & 0xFF;
        (lerp_int(t, a, b) & 0xFF) << shift
    };
    channel(24) | channel(16) | channel(8) | channel(0)
}

/// `Mth.rotLerp`: the short way round.
fn rot_lerp(t: f32, from: f32, to: f32) -> f32 {
    let delta = (to - from + 180.0).rem_euclid(360.0) - 180.0;
    from + t * delta
}

/// Every display entity, by id.
#[derive(Default)]
pub struct DisplayStore {
    pub displays: HashMap<i32, DisplayEntity>,
}

impl DisplayStore {
    pub fn spawn(&mut self, id: i32, kind: DisplayKind, position: DVec3, y_rot: f32, x_rot: f32) {
        self.displays
            .insert(id, DisplayEntity::new(kind, position, y_rot, x_rot));
    }

    pub fn remove(&mut self, ids: &[i32]) {
        for id in ids {
            self.displays.remove(id);
        }
    }

    pub fn clear(&mut self) {
        self.displays.clear();
    }

    pub fn apply_entity_data(&mut self, id: i32, index: u8, value: MetaValue) {
        if let Some(display) = self.displays.get_mut(&id) {
            display.apply_scalar(index, value);
        }
    }

    pub fn apply_display_data(&mut self, id: i32, index: u8, value: DisplayValue) {
        if let Some(display) = self.displays.get_mut(&id) {
            display.apply_value(index, value);
        }
    }

    pub fn move_delta(&mut self, id: i32, delta: DVec3, rotation: Option<(f32, f32)>) {
        if let Some(display) = self.displays.get_mut(&id) {
            let position = display.server_position + delta;
            display.move_to(position, rotation);
        }
    }

    pub fn rotate(&mut self, id: i32, y_rot: f32, x_rot: f32) {
        if let Some(display) = self.displays.get_mut(&id) {
            display.move_to(display.server_position, Some((y_rot, x_rot)));
        }
    }

    pub fn teleport(&mut self, id: i32, position: DVec3, y_rot: f32, x_rot: f32) {
        if let Some(display) = self.displays.get_mut(&id) {
            display.move_to(position, Some((y_rot, x_rot)));
        }
    }

    /// `ClientboundSetPassengersPacket`: `passengers` now ride `vehicle`.
    pub fn set_passengers(&mut self, vehicle: i32, passengers: &[i32]) {
        for (id, display) in &mut self.displays {
            if passengers.contains(id) {
                display.vehicle = Some(vehicle);
            } else if display.vehicle == Some(vehicle) {
                display.vehicle = None;
            }
        }
    }

    /// Ticks every display; riders follow `vehicle`'s riding position
    /// (`Entity.positionRider`), displays being vehicles too.
    pub fn tick(&mut self, vehicle: impl Fn(i32) -> Option<DVec3>) {
        let riding: Vec<(i32, DVec3)> = self
            .displays
            .iter()
            .filter_map(|(id, display)| {
                let vehicle_id = display.vehicle?;
                let seat = match self.displays.get(&vehicle_id) {
                    // A display's passenger attachment is its 0 x 0 box's top.
                    Some(vehicle) => vehicle.position,
                    None => vehicle(vehicle_id)?,
                };
                Some((*id, seat))
            })
            .collect();
        for display in self.displays.values_mut() {
            display.tick();
        }
        for (id, seat) in riding {
            if let Some(display) = self.displays.get_mut(&id) {
                display.lerp = None;
                display.position = seat;
                display.server_position = seat;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawned(kind: DisplayKind) -> DisplayEntity {
        let mut display = DisplayEntity::new(kind, DVec3::ZERO, 0.0, 0.0);
        display.tick();
        display
    }

    /// The server moving a seated display (to keep it tracked) neither
    /// moves it nor resets where it's drawn from: its seat places it.
    #[test]
    fn a_passenger_ignores_server_moves() {
        let mut store = DisplayStore::default();
        store.displays.insert(7, spawned(DisplayKind::Block));
        store.set_passengers(1, &[7]);
        let seat = |at: f64| move |_| Some(DVec3::new(at, 0.0, 0.0));
        store.tick(seat(1.0));
        store.tick(seat(2.0));
        store.teleport(7, DVec3::new(-3.0, 0.0, 0.0), 0.0, 0.0);
        store.move_delta(7, DVec3::new(0.5, 0.0, 0.0), None);
        let display = &store.displays[&7];
        assert_eq!(display.prev_position, DVec3::new(1.0, 0.0, 0.0));
        assert_eq!(display.position, DVec3::new(2.0, 0.0, 0.0));
    }

    #[test]
    fn transformation_matrix_is_translate_rotate_scale_rotate() {
        let t = Transformation {
            translation: Vec3::new(1.0, 2.0, 3.0),
            left_rotation: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            scale: Vec3::new(2.0, 2.0, 2.0),
            right_rotation: Quat::IDENTITY,
        };
        let p = t.matrix().transform_point3(Vec3::X);
        assert!((p - Vec3::new(1.0, 2.0, 1.0)).length() < 1e-5, "{p}");
    }

    #[test]
    fn transformation_interpolates_from_the_previous_state() {
        let mut display = spawned(DisplayKind::Block);
        display.apply_scalar(9, MetaValue::Int(10));
        display.apply_scalar(8, MetaValue::Int(0));
        display.apply_value(11, DisplayValue::Vector3(Vec3::new(10.0, 0.0, 0.0)));
        display.tick();
        let pose = |d: &DisplayEntity, partial| {
            d.pose(partial, (0.0, 0.0))
                .unwrap()
                .transform_point3(Vec3::ZERO)
        };
        // Start tick: progress 0 at partial 0, halfway five ticks on.
        assert!(pose(&display, 0.0).x.abs() < 1e-5);
        for _ in 0..5 {
            display.tick();
        }
        assert!((pose(&display, 0.0).x - 5.0).abs() < 1e-4);
        for _ in 0..10 {
            display.tick();
        }
        assert!((pose(&display, 0.0).x - 10.0).abs() < 1e-4);
    }

    #[test]
    fn no_interpolation_duration_snaps() {
        let mut display = spawned(DisplayKind::Item);
        display.apply_value(12, DisplayValue::Vector3(Vec3::splat(3.0)));
        display.tick();
        let m = display.pose(0.5, (0.0, 0.0)).unwrap();
        assert!((m.transform_vector3(Vec3::X).x - 3.0).abs() < 1e-5);
    }

    #[test]
    fn center_billboard_faces_the_camera() {
        let mut display = spawned(DisplayKind::Text);
        display.apply_scalar(15, MetaValue::Byte(3));
        display.tick();
        // Camera looking south (yaw 0): the text's +z faces back north.
        let m = display.pose(0.0, (0.0, 0.0)).unwrap();
        let facing = m.transform_vector3(Vec3::Z);
        assert!((facing - Vec3::NEG_Z).length() < 1e-5, "{facing}");
    }

    #[test]
    fn teleport_duration_lerps_and_zero_snaps() {
        let mut display = spawned(DisplayKind::Block);
        display.move_to(DVec3::new(4.0, 0.0, 0.0), None);
        assert_eq!(display.position.x, 4.0);
        assert_eq!(display.prev_position.x, 4.0);

        display.apply_scalar(10, MetaValue::Int(4));
        display.move_to(DVec3::new(8.0, 0.0, 0.0), None);
        display.tick();
        assert_eq!(display.position.x, 5.0);
        for _ in 0..3 {
            display.tick();
        }
        assert_eq!(display.position.x, 8.0);
    }

    #[test]
    fn text_opacity_and_background_follow_vanilla_defaults() {
        let display = spawned(DisplayKind::Text);
        assert_eq!(display.text_color(0.0), 0xFFFF_FFFF);
        assert_eq!(display.text_background(0.0, 0.25), 0x4000_0000);
        let mut faint = spawned(DisplayKind::Text);
        faint.apply_scalar(26, MetaValue::Byte(2));
        faint.tick();
        // Font.adjustColor: alpha under 4 draws opaque.
        assert_eq!(faint.text_color(0.0), 0xFFFF_FFFF);
        faint.apply_scalar(26, MetaValue::Byte(0x80));
        faint.tick();
        assert_eq!(faint.text_color(0.0), 0x80FF_FFFF);
    }

    #[test]
    fn brightness_override_unpacks_block_and_sky() {
        let mut display = spawned(DisplayKind::Block);
        display.apply_scalar(16, MetaValue::Int(15 << 4 | 7 << 20));
        display.tick();
        assert_eq!(display.brightness_override(), Some((7, 15)));
    }
}
