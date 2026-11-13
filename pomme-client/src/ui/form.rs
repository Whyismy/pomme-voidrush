//! Vanilla `EditBox`es as a screen holds them: a few text fields, one of
//! them focused at most, Tab moving between them.
// TODO: a field that cannot be edited is not focused either, so its text
// cannot be scrolled or copied, and it draws in the same colour as one
// that can.

use winit::keyboard::KeyCode;

use super::common::{FONT_SIZE, WHITE, hit_test, push_field_text};
use super::text_edit::{SystemClipboard, TextFieldState, TextInputEvent};
use crate::renderer::pipelines::menu_overlay::{MenuElement, SpriteId};

/// The space either side of an `EditBox`'s text, inside its border.
const PADDING: f32 = 4.0;

pub struct Form {
    fields: Vec<TextFieldState>,
    /// The width each field's text has, in GUI pixels.
    widths: Vec<f32>,
    focus: Option<usize>,
}

impl Form {
    /// A field for each `(most characters, box width in GUI pixels)`.
    pub fn new(fields: &[(usize, f32)]) -> Self {
        Self {
            fields: fields
                .iter()
                .map(|(most, _)| TextFieldState::new(*most))
                .collect(),
            widths: fields.iter().map(|(_, w)| w - PADDING * 2.0).collect(),
            focus: None,
        }
    }

    pub fn value(&self, field: usize) -> &str {
        self.fields[field].value()
    }

    /// `EditBox.setValue`; `width` measures text at the GUI's own size.
    pub fn set(&mut self, field: usize, value: &str, width: &dyn Fn(&str) -> f32) {
        self.fields[field].set_value(value, self.widths[field], width);
    }

    #[cfg(test)]
    pub fn focus(&self) -> Option<usize> {
        self.focus
    }

    pub fn set_focus(&mut self, field: Option<usize>) {
        if self.focus != field
            && let Some(field) = field
        {
            self.fields[field].set_focused(true);
        }
        self.focus = field;
    }

    /// Types into the focused field. Tab moves the focus on through
    /// `order` (back with Shift); whether Enter was pressed.
    pub fn handle_text_input(
        &mut self,
        events: &[TextInputEvent],
        order: &[usize],
        width: &dyn Fn(&str) -> f32,
    ) -> bool {
        let mut entered = false;
        for event in events {
            if let TextInputEvent::Key { code, mods } = event {
                match code {
                    KeyCode::Tab if !order.is_empty() => {
                        let at = self
                            .focus
                            .and_then(|focus| order.iter().position(|field| *field == focus));
                        let next = match at {
                            Some(at) if mods.shift => (at + order.len() - 1) % order.len(),
                            Some(at) => (at + 1) % order.len(),
                            None if mods.shift => order.len() - 1,
                            None => 0,
                        };
                        self.set_focus(Some(order[next]));
                        continue;
                    }
                    KeyCode::Enter | KeyCode::NumpadEnter => {
                        entered = true;
                        continue;
                    }
                    _ => {}
                }
            }
            if let Some(field) = self.focus {
                self.fields[field].handle(event, &mut SystemClipboard, self.widths[field], width);
            }
        }
        entered
    }

    /// Draws a field in its box (GUI pixels). A press on one that can be
    /// edited focuses it and places the cursor; a press anywhere else takes
    /// its focus away.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        elements: &mut Vec<MenuElement>,
        field: usize,
        [x, y, w, h]: [f32; 4],
        editable: bool,
        cursor: (f32, f32),
        pressed: bool,
        gs: f32,
        text_width: &dyn Fn(&str, f32) -> f32,
    ) {
        let fs = FONT_SIZE * gs;
        let rect = [x * gs, y * gs, w * gs, h * gs];
        let inner = self.widths[field];
        if pressed {
            if editable && hit_test(cursor, rect) {
                self.set_focus(Some(field));
                let at = (cursor.0 / gs).floor() - (x + PADDING);
                let width = |text: &str| text_width(text, FONT_SIZE);
                let position = self.fields[field].pos_from_click(at, inner, &width);
                self.fields[field].on_click(position, false, inner, &width);
            } else if self.focus == Some(field) {
                self.focus = None;
            }
        }
        let focused = self.focus == Some(field);
        elements.push(MenuElement::NineSlice {
            x: rect[0],
            y: rect[1],
            w: rect[2],
            h: rect[3],
            sprite: if focused {
                SpriteId::WidgetTextFieldHighlighted
            } else {
                SpriteId::WidgetTextField
            },
            border: gs,
            tint: WHITE,
        });
        let width = |text: &str| text_width(text, fs);
        let info = self.fields[field].render_info(inner * gs, focused, &width);
        let shown = &self.fields[field].value()[info.display_start..info.display_end];
        let text_x = (x + PADDING) * gs;
        elements.push(MenuElement::ScissorPush {
            x: text_x,
            y: rect[1],
            w: inner * gs,
            h: rect[3],
        });
        push_field_text(
            elements,
            &info,
            shown,
            None,
            text_x,
            rect[1] + (rect[3] - fs) / 2.0,
            fs,
            gs,
            gs,
            WHITE,
            None,
            &width,
        );
        elements.push(MenuElement::ScissorPop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::text_edit::KeyMods;

    fn key(code: KeyCode, shift: bool) -> TextInputEvent {
        TextInputEvent::Key {
            code,
            mods: KeyMods {
                shift,
                ctrl: false,
                alt: false,
                super_key: false,
            },
        }
    }

    /// Typing goes to the focused field, Tab moves on through the order
    /// given (and back with Shift), and Enter is reported, not typed.
    #[test]
    fn typing_follows_the_focus() {
        let width = |text: &str| text.len() as f32 * 6.0;
        let mut form = Form::new(&[(32, 100.0), (32, 100.0), (4, 100.0)]);
        let typed = |text: &str| text.chars().map(TextInputEvent::Char).collect::<Vec<_>>();
        // Nothing is focused: nothing is typed.
        assert!(!form.handle_text_input(&typed("x"), &[0, 2], &width));
        assert_eq!(form.value(0), "");
        form.set_focus(Some(0));
        form.handle_text_input(&typed("one"), &[0, 2], &width);
        // The second field is not in the order: Tab passes it by.
        form.handle_text_input(&[key(KeyCode::Tab, false)], &[0, 2], &width);
        assert_eq!(form.focus(), Some(2));
        // A field takes no more than it holds.
        form.handle_text_input(&typed("123456"), &[0, 2], &width);
        assert!(form.handle_text_input(&[key(KeyCode::Enter, false)], &[0, 2], &width));
        form.handle_text_input(&[key(KeyCode::Tab, true)], &[0, 2], &width);
        assert_eq!(form.focus(), Some(0));
        assert_eq!(
            [form.value(0), form.value(1), form.value(2)],
            ["one", "", "1234"]
        );
        form.set(1, "set", &width);
        assert_eq!(form.value(1), "set");
    }
}
