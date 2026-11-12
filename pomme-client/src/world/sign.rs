//! What signs say (vanilla `SignBlockEntity`, `SignRenderer` and
//! `HangingSignRenderer`): each side's four lines, read from the block
//! entity, and where each line is written on the board.

use glam::{Mat4, Vec3};
use simdnbt::owned::{NbtCompound, NbtTag};

use crate::chat_component::Component;
use crate::ui::text::TextSpan;

/// One side of a sign: its lines, already in the colour they are written
/// in, and whether the text glows (drawn full bright).
#[derive(Clone, Debug, PartialEq)]
pub struct SignSide {
    pub lines: [Vec<TextSpan>; 4],
    pub glowing: bool,
}

/// How a sign hangs, which decides where its text goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SignKind {
    Standing,
    Wall,
    /// Under a block, on chains.
    Hanging,
    /// From a bracket on a wall.
    WallHanging,
}

impl SignKind {
    pub fn of(block: &str) -> Option<Self> {
        if block.ends_with("_wall_hanging_sign") {
            Some(Self::WallHanging)
        } else if block.ends_with("_hanging_sign") {
            Some(Self::Hanging)
        } else if block.ends_with("_wall_sign") {
            Some(Self::Wall)
        } else if block.ends_with("_sign") {
            Some(Self::Standing)
        } else {
            None
        }
    }

    fn hangs(self) -> bool {
        matches!(self, Self::Hanging | Self::WallHanging)
    }

    /// The widest a line may be, in text pixels (`getMaxTextLineWidth`).
    pub fn max_line_width(self) -> i32 {
        if self.hangs() { 60 } else { 90 }
    }

    /// `getTextLineHeight`.
    fn line_height(self) -> f32 {
        if self.hangs() { 9.0 } else { 10.0 }
    }
}

/// `DyeColor.getTextColor`, by the dye's name.
fn dye_text_color(name: &str) -> [u8; 3] {
    let rgb: u32 = match name.strip_prefix("minecraft:").unwrap_or(name) {
        "white" => 0xFFFFFF,
        "orange" => 0xFF681F,
        "magenta" => 0xFF00FF,
        "light_blue" => 0x9AC0CD,
        "yellow" => 0xFFFF00,
        "lime" => 0xBFFF00,
        "pink" => 0xFF69B4,
        "gray" => 0x808080,
        "light_gray" => 0xD3D3D3,
        "cyan" => 0x00FFFF,
        "purple" => 0xA020F0,
        "blue" => 0x0000FF,
        "brown" => 0x8B4513,
        "green" => 0x00FF00,
        "red" => 0xFF0000,
        _ => 0x000000,
    };
    [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8]
}

/// The colour a side's text is written in (`AbstractSignRenderer`): the
/// dye's own when it glows, four tenths of it otherwise.
// TODO: glowing text's outline.
fn text_color(dye: &str, glowing: bool) -> [f32; 4] {
    let [r, g, b] = dye_text_color(dye);
    let channel = |c: u8| {
        if glowing {
            f32::from(c) / 255.0
        } else {
            f32::from((f64::from(c) * 0.4) as u8) / 255.0
        }
    };
    [channel(r), channel(g), channel(b), 1.0]
}

/// One message: a text component as NBT, or, before 1.21.5, as JSON in a
/// string.
fn message(tag: NbtTag, color: [f32; 4]) -> Vec<TextSpan> {
    let json = match &tag {
        NbtTag::String(text) => {
            let text = text.to_str();
            (text.starts_with(['{', '[', '"']))
                .then(|| serde_json::from_str::<serde_json::Value>(&text).ok())
                .flatten()
        }
        _ => None,
    };
    let component = match json {
        Some(json) => Component::from_value(&json),
        None => Component::from_nbt_tag(&tag),
    };
    component
        .map(|component| crate::ui::text::format_component_spans(&component, color))
        .unwrap_or_default()
}

fn side(nbt: &NbtCompound, key: &str) -> Option<SignSide> {
    let text = nbt.compound(key)?;
    let glowing = text.byte("has_glowing_text").is_some_and(|b| b != 0);
    let dye = text.string("color").map(|c| c.to_str().into_owned());
    let color = text_color(dye.as_deref().unwrap_or("black"), glowing);
    let messages = text.list("messages")?;
    let tags: Vec<NbtTag> = if let Some(strings) = messages.strings() {
        strings.iter().cloned().map(NbtTag::String).collect()
    } else {
        // Components, each bare or (in a list of mixed kinds) wrapped
        // under an empty key.
        messages
            .compounds()?
            .iter()
            .map(|c| match c.get("") {
                Some(inner) if c.len() == 1 => inner.clone(),
                _ => NbtTag::Compound(c.clone()),
            })
            .collect()
    };
    let mut lines: [Vec<TextSpan>; 4] = Default::default();
    for (line, tag) in lines.iter_mut().zip(tags) {
        *line = message(tag, color);
    }
    lines
        .iter()
        .any(|line| line.iter().any(|span| !span.text.is_empty()))
        .then_some(SignSide { lines, glowing })
}

/// A sign's front and back, each `None` when blank.
pub fn sign_sides(nbt: &NbtCompound) -> [Option<SignSide>; 2] {
    [side(nbt, "front_text"), side(nbt, "back_text")]
}

/// `Direction.toYRot` of a horizontal facing.
fn facing_degrees(facing: &str) -> f32 {
    match facing {
        "west" => 90.0,
        "north" => 180.0,
        "east" => 270.0,
        _ => 0.0,
    }
}

/// Where line `line` of a side is written, from the block's corner, as the
/// pose of a one-line text display (which is how the line is drawn): the
/// sign's own turn, the board's place, the side, then vanilla's text
/// offset and scale.
pub fn line_pose(
    kind: SignKind,
    rotation: Option<&str>,
    facing: Option<&str>,
    back: bool,
    line: usize,
) -> Mat4 {
    // A standing sign turns in sixteenths; one on a wall faces out of it.
    let degrees = match kind {
        SignKind::Standing | SignKind::Hanging => {
            rotation.and_then(|r| r.parse::<f32>().ok()).unwrap_or(0.0) * 22.5
        }
        SignKind::Wall | SignKind::WallHanging => facing_degrees(facing.unwrap_or("south")),
    };
    let turn = Mat4::from_rotation_y((-degrees).to_radians());
    // `SignRenderer.translateSign` and `HangingSignRenderer.translateSign`.
    let board = match kind {
        SignKind::Standing => Mat4::from_translation(Vec3::new(0.5, 0.5, 0.5)) * turn,
        SignKind::Wall => {
            Mat4::from_translation(Vec3::new(0.5, 0.5, 0.5))
                * turn
                * Mat4::from_translation(Vec3::new(0.0, -0.3125, -0.4375))
        }
        SignKind::Hanging | SignKind::WallHanging => {
            Mat4::from_translation(Vec3::new(0.5, 0.9375, 0.5))
                * turn
                * Mat4::from_translation(Vec3::new(0.0, -0.3125, 0.0))
        }
    };
    // `translateSignText`: `getTextOffset`, then 1/64 of the board's scale,
    // y down.
    let (offset, scale) = if kind.hangs() {
        (Vec3::new(0.0, -0.32, 0.073), 0.015625)
    } else {
        (
            Vec3::new(0.0, 0.333_333_34, 0.046_666_667),
            0.015625 * 0.666_666_7,
        )
    };
    let side = if back {
        Mat4::from_rotation_y(std::f32::consts::PI)
    } else {
        Mat4::IDENTITY
    };
    // The four lines sit about the text's origin, each a line height down.
    // A text display's one line sits just above its origin, y up, 40 text
    // pixels a unit: flipped into the sign's y-down text pixels.
    let height = kind.line_height();
    let line_top = line as f32 * height - 2.0 * height;
    board
        * side
        * Mat4::from_translation(offset)
        * Mat4::from_scale(Vec3::new(scale, -scale, scale))
        * Mat4::from_translation(Vec3::new(0.0, line_top + 10.0, 0.0))
        * Mat4::from_scale(Vec3::new(40.0, -40.0, 40.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(snbt_lines: [&str; 4], color: Option<&str>, glowing: bool) -> NbtCompound {
        use simdnbt::owned::NbtList;
        let mut text = NbtCompound::new();
        text.insert(
            "messages",
            NbtTag::List(NbtList::String(
                snbt_lines.iter().map(|line| (*line).into()).collect(),
            )),
        );
        if let Some(color) = color {
            text.insert("color", NbtTag::String(color.into()));
        }
        text.insert("has_glowing_text", NbtTag::Byte(i8::from(glowing)));
        let mut nbt = NbtCompound::new();
        nbt.insert("front_text", NbtTag::Compound(text));
        nbt
    }

    /// A side's lines come out in its dye's colour at four tenths, or the
    /// dye's own when it glows; a blank side is no side.
    #[test]
    fn a_side_is_read_in_its_colour() {
        let [front, back] = sign_sides(&sign(["Hello", "", "world", ""], None, false));
        let front = front.unwrap();
        assert!(back.is_none());
        assert_eq!(front.lines[0][0].text, "Hello");
        assert_eq!(front.lines[2][0].text, "world");
        assert_eq!(front.lines[0][0].color, [0.0, 0.0, 0.0, 1.0]);
        assert!(!front.glowing);

        let [front, _] = sign_sides(&sign(["Red", "", "", ""], Some("red"), false));
        assert_eq!(front.unwrap().lines[0][0].color, [0.4, 0.0, 0.0, 1.0]);
        let [front, _] = sign_sides(&sign(["Red", "", "", ""], Some("red"), true));
        let front = front.unwrap();
        assert!(front.glowing);
        assert_eq!(front.lines[0][0].color, [1.0, 0.0, 0.0, 1.0]);

        // Before 1.21.5 a line was JSON in a string.
        let [front, _] = sign_sides(&sign([r#"{"text":"Old"}"#, "", "", ""], None, false));
        assert_eq!(front.unwrap().lines[0][0].text, "Old");
        assert!(sign_sides(&sign(["", "", "", ""], None, false))[0].is_none());
    }

    #[test]
    fn signs_are_told_apart_by_name() {
        assert_eq!(SignKind::of("oak_sign"), Some(SignKind::Standing));
        assert_eq!(SignKind::of("oak_wall_sign"), Some(SignKind::Wall));
        assert_eq!(SignKind::of("oak_hanging_sign"), Some(SignKind::Hanging));
        assert_eq!(
            SignKind::of("oak_wall_hanging_sign"),
            Some(SignKind::WallHanging)
        );
        assert_eq!(SignKind::of("stone"), None);
        assert_eq!(SignKind::Standing.max_line_width(), 90);
        assert_eq!(SignKind::Hanging.max_line_width(), 60);
    }

    /// A standing sign facing south writes its lines just in front of the
    /// board, centred across the block, the second line's top a third of a
    /// block above the board's pivot less ten text pixels; its back is the
    /// same turned about; a wall sign hangs lower, against the wall behind.
    #[test]
    fn lines_sit_on_the_board() {
        // The display's text hangs from y = 10/40 down to 0 above its origin.
        let top = Vec3::new(0.0, 0.25, 0.0);
        let scale = 0.015625 * 0.666_666_7;
        let front = line_pose(SignKind::Standing, Some("0"), None, false, 1);
        let at = front.transform_point3(top);
        assert!((at.x - 0.5).abs() < 1e-6);
        assert!((at.z - (0.5 + 0.046_666_667)).abs() < 1e-6);
        assert!((at.y - (0.5 + 0.333_333_34 + 10.0 * scale)).abs() < 1e-5);
        // Facing +z: the text's right is +x.
        let right = front.transform_vector3(Vec3::X);
        assert!(right.x > 0.0 && right.z.abs() < 1e-6);

        let back = line_pose(SignKind::Standing, Some("0"), None, true, 1);
        assert!((back.transform_point3(top).z - (0.5 - 0.046_666_667)).abs() < 1e-6);
        assert!(back.transform_vector3(Vec3::X).x < 0.0);

        // A wall sign facing north hangs on the block's south side.
        let wall = line_pose(SignKind::Wall, None, Some("north"), false, 1);
        let at = wall.transform_point3(top);
        assert!((at.z - (0.5 + 0.4375 - 0.046_666_667)).abs() < 1e-5);
        assert!((at.y - (0.5 - 0.3125 + 0.333_333_34 + 10.0 * scale)).abs() < 1e-5);

        // Lines step down by the line height.
        let down = |kind, n| {
            line_pose(kind, Some("0"), None, false, n)
                .transform_point3(top)
                .y
        };
        let step = down(SignKind::Standing, 0) - down(SignKind::Standing, 1);
        assert!((step - 10.0 * scale).abs() < 1e-6);
        let step = down(SignKind::Hanging, 0) - down(SignKind::Hanging, 1);
        assert!((step - 9.0 * 0.015625).abs() < 1e-6);
    }
}
