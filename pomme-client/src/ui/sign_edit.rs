//! Vanilla `SignEditScreen` and `HangingSignEditScreen`: the sign drawn
//! large with its four lines, the one being written marked by a blinking
//! cursor, and a Done button. Closing the screen, however it is closed,
//! sends the lines (`ServerboundSignUpdatePacket`).
// TODO: the selection inverts the text it covers in vanilla; here it is a
// blue box behind it.

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
    /// The selection's other end; the cursor's own place when nothing is
    /// selected.
    select: usize,
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
            select: cursor,
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

    /// The selected stretch of the line, as character places: empty when
    /// the cursor and the selection's other end are one.
    fn selection(&self) -> (usize, usize) {
        (self.cursor.min(self.select), self.cursor.max(self.select))
    }

    fn selected(&self) -> &str {
        let (from, to) = self.selection();
        &self.lines[self.line][self.byte_at(from)..self.byte_at(to)]
    }

    /// `TextFieldHelper.insertText`: `text` takes the selection's place, or
    /// goes in at the cursor, where the line stays within the board
    /// (`width` in text pixels).
    fn insert(&mut self, text: &str, width: &dyn Fn(&str) -> f32) {
        let text: String = text
            .chars()
            .filter(|c| *c >= ' ' && *c != '\u{7f}' && *c != '\u{a7}')
            .collect();
        if text.is_empty() {
            return;
        }
        let (from, to) = self.selection();
        let mut line = self.lines[self.line].clone();
        line.replace_range(self.byte_at(from)..self.byte_at(to), &text);
        if width(&line) <= self.kind.max_line_width() as f32 {
            self.lines[self.line] = line;
            self.cursor = from + text.chars().count();
            self.select = self.cursor;
        }
    }

    /// `TextFieldHelper.removeFromCursor`: the selection goes, or else what
    /// lies between the cursor and `to`.
    fn remove_to(&mut self, to: usize) {
        if self.cursor == self.select {
            self.select = to;
        }
        let (from, to) = self.selection();
        let bytes = self.byte_at(from)..self.byte_at(to);
        self.lines[self.line].replace_range(bytes, "");
        self.cursor = from;
        self.select = from;
    }

    /// The place a step from the cursor reaches: a character on, or with
    /// `word` the start of the next word (the previous, going back).
    fn step(&self, forward: bool, word: bool) -> usize {
        let chars: Vec<char> = self.lines[self.line].chars().collect();
        let mut at = self.cursor;
        if !word {
            return if forward {
                (at + 1).min(chars.len())
            } else {
                at.saturating_sub(1)
            };
        }
        if forward {
            while at < chars.len() && chars[at] != ' ' {
                at += 1;
            }
            while at < chars.len() && chars[at] == ' ' {
                at += 1;
            }
        } else {
            while at > 0 && chars[at - 1] == ' ' {
                at -= 1;
            }
            while at > 0 && chars[at - 1] != ' ' {
                at -= 1;
            }
        }
        at
    }

    /// Moves the cursor; the selection's other end follows unless Shift
    /// holds it.
    fn move_cursor(&mut self, to: usize, selecting: bool) {
        self.cursor = to;
        if !selecting {
            self.select = to;
        }
    }

    fn move_to_line(&mut self, line: usize) {
        self.line = line % 4;
        self.cursor = self.lines[self.line].chars().count();
        self.select = self.cursor;
    }

    /// `AbstractSignEditScreen.keyPressed` and `charTyped`: up and down
    /// (and Enter) change line, the rest writes on the line as
    /// `TextFieldHelper` does.
    pub fn handle_text_input(&mut self, events: &[TextInputEvent], width: &dyn Fn(&str) -> f32) {
        self.handle_text_input_with(events, width, &mut SystemClipboard);
    }

    fn handle_text_input_with(
        &mut self,
        events: &[TextInputEvent],
        width: &dyn Fn(&str) -> f32,
        clipboard: &mut dyn ClipboardAccess,
    ) {
        for event in events {
            self.opened = Instant::now();
            let end = self.lines[self.line].chars().count();
            match event {
                TextInputEvent::Char(c) => self.insert(c.encode_utf8(&mut [0; 4]), width),
                TextInputEvent::Key { code, mods } => match code {
                    KeyCode::ArrowUp => self.move_to_line(self.line + 3),
                    KeyCode::ArrowDown | KeyCode::Enter | KeyCode::NumpadEnter => {
                        self.move_to_line(self.line + 1);
                    }
                    KeyCode::KeyA if mods.edit_shortcut() => {
                        self.select = 0;
                        self.cursor = end;
                    }
                    KeyCode::KeyC if mods.edit_shortcut() => clipboard.set(self.selected()),
                    KeyCode::KeyX if mods.edit_shortcut() => {
                        clipboard.set(self.selected());
                        self.remove_to(self.cursor);
                    }
                    KeyCode::KeyV if mods.edit_shortcut() => {
                        let pasted = clipboard.get();
                        self.insert(&pasted, width);
                    }
                    KeyCode::Backspace => self.remove_to(self.step(false, mods.edit_shortcut())),
                    KeyCode::Delete => self.remove_to(self.step(true, mods.edit_shortcut())),
                    KeyCode::ArrowLeft => {
                        self.move_cursor(self.step(false, mods.edit_shortcut()), mods.shift);
                    }
                    KeyCode::ArrowRight => {
                        self.move_cursor(self.step(true, mods.edit_shortcut()), mods.shift);
                    }
                    KeyCode::Home => self.move_cursor(0, mods.shift),
                    KeyCode::End => self.move_cursor(end, mods.shift),
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
        let written = (!line.is_empty()).then(|| MenuElement::TextFlat {
            x: left,
            y,
            text: line.clone(),
            scale,
            color: state.color,
        });
        if index != state.line {
            elements.extend(written);
            continue;
        }
        let along = |chars: usize| {
            let before: String = line.chars().take(chars).collect();
            left + width(&before) * unit
        };
        // The selection, behind what it covers.
        let (from, to) = state.selection();
        if from != to {
            elements.push(MenuElement::Rect {
                x: along(from),
                y,
                w: along(to) - along(from),
                h: line_height * unit,
                corner_radius: 0.0,
                color: super::common::FIELD_SELECTION,
            });
        }
        elements.extend(written);
        if !blink {
            continue;
        }
        let x = along(state.cursor);
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

    /// Shift selects, typing and pasting replace what is selected, and
    /// copy and cut take it to the clipboard.
    #[test]
    fn a_selection_is_copied_cut_and_written_over() {
        struct Clipboard(String);
        impl ClipboardAccess for Clipboard {
            fn get(&mut self) -> String {
                self.0.clone()
            }
            fn set(&mut self, text: &str) {
                self.0 = text.to_owned();
            }
        }
        let mods = |shift: bool, ctrl: bool| super::super::text_edit::KeyMods {
            shift,
            ctrl,
            alt: false,
            super_key: ctrl,
        };
        let press = |code, shift, ctrl| TextInputEvent::Key {
            code,
            mods: mods(shift, ctrl),
        };
        let mut clipboard = Clipboard(String::new());
        let pos = BlockPos { x: 0, y: 0, z: 0 };
        let mut edit = SignEditState::open(pos, true, "oak_sign", None).unwrap();
        edit.handle_text_input_with(&typed("one two"), &width, &mut clipboard);
        // Shift and Ctrl+Left: the last word is selected.
        let back_a_word = [press(KeyCode::ArrowLeft, true, true)];
        edit.handle_text_input_with(&back_a_word, &width, &mut clipboard);
        assert_eq!(edit.selection(), (4, 7));
        edit.handle_text_input_with(&[press(KeyCode::KeyC, false, true)], &width, &mut clipboard);
        assert_eq!(clipboard.0, "two");
        // Typing writes over it.
        edit.handle_text_input_with(&typed("2"), &width, &mut clipboard);
        assert_eq!(edit.lines[0], "one 2");
        // All of it cut, and pasted back twice.
        let all = [
            press(KeyCode::KeyA, false, true),
            press(KeyCode::KeyX, false, true),
        ];
        edit.handle_text_input_with(&all, &width, &mut clipboard);
        assert_eq!(
            (edit.lines[0].as_str(), clipboard.0.as_str()),
            ("", "one 2")
        );
        let paste = [press(KeyCode::KeyV, false, true)];
        edit.handle_text_input_with(&paste, &width, &mut clipboard);
        edit.handle_text_input_with(&paste, &width, &mut clipboard);
        assert_eq!(edit.lines[0], "one 2one 2");
        // Backspace with a selection removes the selection alone.
        let select_two = [
            press(KeyCode::ArrowLeft, true, false),
            press(KeyCode::ArrowLeft, true, false),
            press(KeyCode::Backspace, false, false),
        ];
        edit.handle_text_input_with(&select_two, &width, &mut clipboard);
        assert_eq!(edit.lines[0], "one 2one");
        // And Ctrl+Backspace a word.
        edit.handle_text_input_with(
            &[press(KeyCode::Backspace, false, true)],
            &width,
            &mut clipboard,
        );
        assert_eq!(edit.lines[0], "one ");
    }
}
