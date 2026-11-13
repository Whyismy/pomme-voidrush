//! Vanilla `SignEditScreen` and `HangingSignEditScreen`: the sign drawn
//! large with its four lines, the one being written marked by a blinking
//! cursor, and a Done button. Closing the screen, however it is closed,
//! sends the lines (`ServerboundSignUpdatePacket`).
// TODO: `TextFieldHelper`'s selection, with its copy and cut.

use std::time::Instant;

use winit::keyboard::KeyCode;

use super::common::{FONT_SIZE, push_button};
use super::container::push_transparent_background;
use super::text_edit::{ClipboardAccess, SystemClipboard, TextInputEvent};
use crate::position::BlockPos;
use crate::renderer::pipelines::menu_overlay::{MenuElement, SIGN_WOODS, SpriteId};
use crate::world::block_entity::StoredBlockEntity;
use crate::world::sign::{SignKind, sign_sides};

/// A model pixel of the sign on screen, in GUI units
/// (`SignEditScreen.MAGIC_SCALE_NUMBER` over the sixteen to a block).
const MODEL_PIXEL: f32 = 62.500004 / 16.0;
/// Where the sign model's origin sits down the screen: the board is the
/// twelve pixels from fourteen above it, the stick the fourteen from two
/// above it, and the whole fills 66 to 168.
const MODEL_ORIGIN_Y: f32 = 66.0 + 14.0 * MODEL_PIXEL;
/// `SignEditScreen.MAGIC_TEXT_SCALE`.
const TEXT_SCALE: f32 = 0.9765628;

/// A sign's side being written on.
pub struct SignEditState {
    pub pos: BlockPos,
    pub front: bool,
    kind: SignKind,
    /// The wood, by its place in [`SIGN_WOODS`].
    wood: u8,
    lines: [String; 4],
    /// The line being written.
    line: usize,
    /// The cursor, in characters along that line.
    cursor: usize,
    color: [f32; 4],
    /// When the cursor's blink began.
    opened: Instant,
    done: bool,
}

/// The wood a sign block is made of, by its place in [`SIGN_WOODS`]; oak
/// for one not known.
fn wood_of(block: &str) -> u8 {
    let wood = ["_wall_hanging_sign", "_hanging_sign", "_wall_sign", "_sign"]
        .iter()
        .find_map(|suffix| block.strip_suffix(suffix))
        .unwrap_or("oak");
    SIGN_WOODS
        .iter()
        .position(|name| *name == wood)
        .unwrap_or(0) as u8
}

impl SignEditState {
    /// The editor for a side of the sign `block` at `pos`, starting from
    /// what that side says; `None` where no sign stands.
    pub fn open(
        pos: BlockPos,
        front: bool,
        block: &str,
        entity: Option<&StoredBlockEntity>,
    ) -> Option<Self> {
        let kind = SignKind::of(block)?;
        let mut lines: [String; 4] = Default::default();
        let mut color = [0.0, 0.0, 0.0, 1.0];
        let written = entity.and_then(|entity| {
            let [written_front, written_back] = sign_sides(&entity.nbt);
            if front { written_front } else { written_back }
        });
        if let Some(side) = written {
            for (line, spans) in lines.iter_mut().zip(&side.lines) {
                *line = spans.iter().map(|span| span.text.as_str()).collect();
            }
            if let Some(span) = side.lines.iter().flatten().next() {
                color = span.color;
            }
        }
        let cursor = lines[0].chars().count();
        Some(Self {
            pos,
            front,
            kind,
            wood: wood_of(block),
            lines,
            line: 0,
            cursor,
            color,
            opened: Instant::now(),
            done: false,
        })
    }

    /// Marks the screen closed; the lines are then sent.
    pub fn finish(&mut self) {
        self.done = true;
    }

    pub fn finished(&self) -> bool {
        self.done
    }

    pub fn into_lines(self) -> [String; 4] {
        self.lines
    }

    fn hangs(&self) -> bool {
        matches!(self.kind, SignKind::Hanging | SignKind::WallHanging)
    }

    /// The byte a character place falls on in the line being written.
    fn byte_at(&self, chars: usize) -> usize {
        let line = &self.lines[self.line];
        line.char_indices()
            .nth(chars)
            .map_or(line.len(), |(byte, _)| byte)
    }

    /// Writes `text` at the cursor, where the line stays within the board
    /// (`width` in text pixels).
    fn insert(&mut self, text: &str, width: &dyn Fn(&str) -> f32) {
        let text: String = text
            .chars()
            .filter(|c| *c >= ' ' && *c != '\u{7f}' && *c != '\u{a7}')
            .collect();
        if text.is_empty() {
            return;
        }
        let at = self.byte_at(self.cursor);
        let mut line = self.lines[self.line].clone();
        line.insert_str(at, &text);
        if width(&line) <= self.kind.max_line_width() as f32 {
            self.lines[self.line] = line;
            self.cursor += text.chars().count();
        }
    }

    fn move_to_line(&mut self, line: usize) {
        self.line = line % 4;
        self.cursor = self.lines[self.line].chars().count();
    }

    /// `AbstractSignEditScreen.keyPressed` and `charTyped`: up and down
    /// (and Enter) change line, the rest writes on the line.
    pub fn handle_text_input(&mut self, events: &[TextInputEvent], width: &dyn Fn(&str) -> f32) {
        for event in events {
            self.opened = Instant::now();
            match event {
                TextInputEvent::Char(c) => self.insert(c.encode_utf8(&mut [0; 4]), width),
                TextInputEvent::Key { code, mods } => match code {
                    KeyCode::ArrowUp => self.move_to_line(self.line + 3),
                    KeyCode::ArrowDown | KeyCode::Enter | KeyCode::NumpadEnter => {
                        self.move_to_line(self.line + 1);
                    }
                    KeyCode::Backspace if self.cursor > 0 => {
                        let from = self.byte_at(self.cursor - 1);
                        let to = self.byte_at(self.cursor);
                        self.lines[self.line].replace_range(from..to, "");
                        self.cursor -= 1;
                    }
                    KeyCode::Delete => {
                        let from = self.byte_at(self.cursor);
                        let to = self.byte_at(self.cursor + 1);
                        self.lines[self.line].replace_range(from..to, "");
                    }
                    KeyCode::ArrowLeft => self.cursor = self.cursor.saturating_sub(1),
                    KeyCode::ArrowRight => {
                        self.cursor = (self.cursor + 1).min(self.lines[self.line].chars().count());
                    }
                    KeyCode::Home => self.cursor = 0,
                    KeyCode::End => self.cursor = self.lines[self.line].chars().count(),
                    KeyCode::KeyV if mods.edit_shortcut() => {
                        let pasted = SystemClipboard.get();
                        self.insert(&pasted, width);
                    }
                    _ => {}
                },
            }
        }
    }
}

/// Builds the screen; true while the cursor is over its Done button.
pub fn build(
    elements: &mut Vec<MenuElement>,
    state: &SignEditState,
    screen_w: f32,
    screen_h: f32,
    cursor: (f32, f32),
    gs: f32,
    text_width: &dyn Fn(&str, f32) -> f32,
) -> bool {
    push_transparent_background(elements, screen_w, screen_h);
    let centre = screen_w / 2.0;
    elements.push(MenuElement::Text {
        x: centre,
        y: 40.0 * gs,
        text: "Edit Sign Message".into(),
        scale: FONT_SIZE * gs,
        color: [1.0; 4],
        centered: true,
    });

    let hangs = state.hangs();
    let image = |elements: &mut Vec<MenuElement>, sprite, x: f32, y: f32, w: f32, h: f32| {
        elements.push(MenuElement::Image {
            x: centre + x * gs,
            y: y * gs,
            w: w * gs,
            h: h * gs,
            sprite,
            tint: [1.0; 4],
        });
    };
    if hangs {
        // `HangingSignEditScreen`: its sixteen pixel picture, four and a
        // half times over, about 125 down.
        image(
            elements,
            SpriteId::HangingSignGui(state.wood),
            -36.0,
            125.0 - 36.0,
            72.0,
            72.0,
        );
    } else {
        image(
            elements,
            SpriteId::SignBoard(state.wood),
            -12.0 * MODEL_PIXEL,
            MODEL_ORIGIN_Y - 14.0 * MODEL_PIXEL,
            24.0 * MODEL_PIXEL,
            12.0 * MODEL_PIXEL,
        );
        if state.kind == SignKind::Standing {
            image(
                elements,
                SpriteId::SignStick(state.wood),
                -MODEL_PIXEL,
                MODEL_ORIGIN_Y - 2.0 * MODEL_PIXEL,
                2.0 * MODEL_PIXEL,
                14.0 * MODEL_PIXEL,
            );
        }
    }

    // `extractSignText`: the lines centred about the board's middle, the
    // one being written with its cursor, which blinks every 300 ms.
    let (text_scale, line_height, middle) = if hangs {
        (1.0, 9.0, 125.0)
    } else {
        (TEXT_SCALE, 10.0, 90.0)
    };
    let scale = FONT_SIZE * gs * text_scale;
    let unit = gs * text_scale;
    let width = |text: &str| text_width(text, FONT_SIZE);
    let blink = (state.opened.elapsed().as_millis() / 300).is_multiple_of(2);
    for (index, line) in state.lines.iter().enumerate() {
        let y = middle * gs + (index as f32 * line_height - 2.0 * line_height) * unit;
        let left = centre - (width(line) / 2.0).floor() * unit;
        if !line.is_empty() {
            elements.push(MenuElement::TextFlat {
                x: left,
                y,
                text: line.clone(),
                scale,
                color: state.color,
            });
        }
        if index != state.line || !blink {
            continue;
        }
        let before: String = line.chars().take(state.cursor).collect();
        let x = left + width(&before) * unit;
        if state.cursor >= line.chars().count() {
            elements.push(MenuElement::TextFlat {
                x,
                y,
                text: "_".into(),
                scale,
                color: state.color,
            });
        } else {
            elements.push(MenuElement::Rect {
                x,
                y: y - unit,
                w: unit,
                h: (line_height + 1.0) * unit,
                corner_radius: 0.0,
                color: state.color,
            });
        }
    }

    push_button(
        elements,
        cursor,
        centre - 100.0 * gs,
        ((screen_h / gs / 4.0).floor() + 144.0) * gs,
        200.0 * gs,
        20.0 * gs,
        gs,
        FONT_SIZE * gs,
        "Done",
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> TextInputEvent {
        TextInputEvent::Key {
            code,
            mods: super::super::text_edit::KeyMods {
                shift: false,
                ctrl: false,
                alt: false,
                super_key: false,
            },
        }
    }

    fn typed(text: &str) -> Vec<TextInputEvent> {
        text.chars().map(TextInputEvent::Char).collect()
    }

    /// Six pixels to a character.
    fn width(text: &str) -> f32 {
        text.chars().count() as f32 * 6.0
    }

    /// Typing writes on the line the cursor is on, Enter and the arrows
    /// change line, and a line takes no more than the board is wide.
    #[test]
    fn typing_fills_the_lines_within_the_board() {
        let mut edit =
            SignEditState::open(BlockPos { x: 0, y: 0, z: 0 }, true, "oak_sign", None).unwrap();
        edit.handle_text_input(&typed("Hello"), &width);
        edit.handle_text_input(&[key(KeyCode::Enter)], &width);
        edit.handle_text_input(&typed("there"), &width);
        edit.handle_text_input(&[key(KeyCode::ArrowUp), key(KeyCode::Backspace)], &width);
        edit.handle_text_input(&[key(KeyCode::Home), key(KeyCode::Delete)], &width);
        edit.handle_text_input(&typed("J"), &width);
        // Fifteen characters are the ninety pixels; a sixteenth is not taken.
        edit.handle_text_input(&[key(KeyCode::ArrowUp)], &width);
        edit.handle_text_input(&typed("0123456789abcdefgh"), &width);
        assert_eq!(
            edit.into_lines(),
            ["Jell", "there", "", "0123456789abcde"].map(String::from)
        );
    }

    /// A hanging sign's board is narrower, and each kind knows its wood.
    #[test]
    fn a_sign_knows_its_wood_and_its_width() {
        assert_eq!(
            SIGN_WOODS[usize::from(wood_of("dark_oak_wall_sign"))],
            "dark_oak"
        );
        assert_eq!(
            SIGN_WOODS[usize::from(wood_of("bamboo_wall_hanging_sign"))],
            "bamboo"
        );
        assert_eq!(
            SIGN_WOODS[usize::from(wood_of("cherry_hanging_sign"))],
            "cherry"
        );
        let pos = BlockPos { x: 0, y: 0, z: 0 };
        assert!(SignEditState::open(pos, true, "stone", None).is_none());
        let mut edit = SignEditState::open(pos, false, "oak_hanging_sign", None).unwrap();
        edit.handle_text_input(&typed("0123456789abcdefgh"), &width);
        assert_eq!(edit.into_lines()[0], "0123456789");
    }
}
