//! Vanilla `BookViewScreen`: a book's pages one at a time on the open
//! book's picture, with its page count, the buttons that turn the page and
//! Done. `LecternScreen` is the same over a lectern's menu, with a button
//! to take the book.
//!
//! `BookEditScreen` is the same book with a page that can be written on,
//! and `BookSignScreen` names it and closes it for good.
// TODO: a page's click events.

use azalea_inventory::ItemStackData;
use azalea_inventory::components::{WritableBookContent, WrittenBookContent};

use super::common::{FONT_SIZE, hit_test, push_button};
use super::container::push_transparent_background;
use super::text::{TextSpan, format_text_spans};
use super::text_edit::{MultilineField, SystemClipboard, TextInputEvent};
use crate::renderer::pipelines::menu_overlay::{MenuElement, SpriteId};

const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// A page's text is 114 wide and 128 tall, nine to a line.
const TEXT_WIDTH: i32 = 114;
const LINES: usize = 128 / 9;

/// The book being read.
pub struct BookViewState {
    pub pages: Vec<Vec<TextSpan>>,
    pub page: usize,
}

impl BookViewState {
    /// Turns the page as `button` asks; whether it turned.
    pub fn turn(&mut self, button: BookButton) -> bool {
        let to = match button {
            BookButton::Back => self.page.checked_sub(1),
            BookButton::Forward => Some(self.page + 1).filter(|page| *page < self.pages.len()),
            BookButton::Done | BookButton::Take => None,
        };
        if let Some(page) = to {
            self.page = page;
        }
        to.is_some()
    }
}

/// One of the screen's buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookButton {
    Done,
    /// A lectern's "Take Book".
    Take,
    Back,
    Forward,
}

/// Book text is written without a shadow.
fn unshadowed(mut spans: Vec<TextSpan>) -> Vec<TextSpan> {
    for span in &mut spans {
        span.shadow_color = Some([0.0; 4]);
    }
    spans
}

/// A book's pages (`BookViewScreen.BookAccess.fromItem`): a written book's
/// or a book and quill's; `None` for anything else.
pub fn pages_of(stack: &ItemStackData) -> Option<Vec<Vec<TextSpan>>> {
    if let Some(book) = stack.get_component::<WrittenBookContent>() {
        return Some(
            book.pages
                .iter()
                .map(|page| unshadowed(format_text_spans(&page.raw, BLACK)))
                .collect(),
        );
    }
    let book = stack.get_component::<WritableBookContent>()?;
    Some(
        book.pages
            .iter()
            .map(|page| unshadowed(vec![TextSpan::new(page.raw.clone(), BLACK)]))
            .collect(),
    )
}

/// What the frame's input asks of the book: a click on the button under
/// the cursor, or Page Up and Page Down (`BookViewScreen.keyPressed`).
pub fn pressed(
    over: Option<BookButton>,
    clicked: bool,
    page_up: bool,
    page_down: bool,
) -> Option<BookButton> {
    if clicked && over.is_some() {
        over
    } else if page_up {
        Some(BookButton::Back)
    } else if page_down {
        Some(BookButton::Forward)
    } else {
        None
    }
}

/// `Font.split`: a page's text as the lines it wraps to at a width.
pub type SplitLines<'a> = &'a dyn Fn(&[TextSpan], i32) -> Vec<Vec<TextSpan>>;

/// Builds the screen at `page` of `pages`; `take_book` is a lectern's,
/// and whether the player may take the book from it. The button under the
/// cursor is returned.
#[allow(clippy::too_many_arguments)]
pub fn build(
    elements: &mut Vec<MenuElement>,
    pages: &[Vec<TextSpan>],
    page: usize,
    take_book: Option<bool>,
    screen_w: f32,
    screen_h: f32,
    cursor: (f32, f32),
    gs: f32,
    split: SplitLines<'_>,
    text_width: &dyn Fn(&str, f32) -> f32,
) -> Option<BookButton> {
    push_transparent_background(elements, screen_w, screen_h);
    let left = ((screen_w / gs - 192.0) / 2.0).floor() * gs;
    let top = 2.0 * gs;
    let scale = FONT_SIZE * gs;
    elements.push(MenuElement::Image {
        x: left,
        y: top,
        w: 192.0 * gs,
        h: 192.0 * gs,
        sprite: SpriteId::BookBackground,
        tint: [1.0; 4],
    });

    // "Page 1 of 3", ending 148 along.
    let indicator = format!("Page {} of {}", page + 1, pages.len().max(1));
    elements.push(MenuElement::TextFlat {
        x: left + 148.0 * gs - text_width(&indicator, scale),
        y: top + 16.0 * gs,
        text: indicator,
        scale,
        color: BLACK,
    });
    if let Some(spans) = pages.get(page) {
        for (index, line) in split(spans, TEXT_WIDTH).into_iter().take(LINES).enumerate() {
            elements.push(MenuElement::TextSpans {
                x: left + 36.0 * gs,
                y: top + (30.0 + 9.0 * index as f32) * gs,
                spans: unshadowed(line),
                scale,
                centered: false,
            });
        }
    }

    let mut over = None;
    // `PageButton`: 23 by 13, shown where there is a page to turn to.
    for (button, shown, x, plain, lit) in [
        (
            BookButton::Forward,
            page + 1 < pages.len(),
            116.0,
            SpriteId::PageForward,
            SpriteId::PageForwardHighlighted,
        ),
        (
            BookButton::Back,
            page > 0,
            43.0,
            SpriteId::PageBackward,
            SpriteId::PageBackwardHighlighted,
        ),
    ] {
        if !shown {
            continue;
        }
        let rect = [left + x * gs, top + 157.0 * gs, 23.0 * gs, 13.0 * gs];
        let hovered = hit_test(cursor, rect);
        elements.push(MenuElement::Image {
            x: rect[0],
            y: rect[1],
            w: rect[2],
            h: rect[3],
            sprite: if hovered { lit } else { plain },
            tint: [1.0; 4],
        });
        if hovered {
            over = Some(button);
        }
    }

    // `createMenuControls`: Done under the book; a lectern's shares the
    // row with Take Book where the player may take it.
    let centre = screen_w / 2.0;
    let row = top + 194.0 * gs;
    let button = |elements: &mut Vec<MenuElement>, x: f32, w: f32, label: &str| {
        push_button(
            elements,
            cursor,
            centre + x * gs,
            row,
            w * gs,
            20.0 * gs,
            gs,
            scale,
            label,
            true,
        )
    };
    if take_book == Some(true) {
        if button(elements, -100.0, 98.0, "Done") {
            over = Some(BookButton::Done);
        }
        if button(elements, 2.0, 98.0, "Take Book") {
            over = Some(BookButton::Take);
        }
    } else if button(elements, -100.0, 200.0, "Done") {
        over = Some(BookButton::Done);
    }
    over
}

/// A book may hold a hundred pages (`BookEditScreen.appendPageToBook`),
/// each of 1024 characters on fourteen lines, and a title of fifteen.
const MAX_PAGES: usize = 100;
const PAGE_CHARACTERS: usize = 1024;
const TITLE_CHARACTERS: usize = 15;

/// What a closed book and quill sends (`ServerboundEditBookPacket`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookEdit {
    /// The inventory slot it is held in.
    pub slot: u32,
    pub pages: Vec<String>,
    /// Its title, where it was signed.
    pub title: Option<String>,
}

/// A button of the editing or the signing screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditButton {
    Back,
    Forward,
    /// To the signing screen.
    Sign,
    Done,
    /// "Sign and Close".
    Finalize,
    /// Back from signing to writing.
    Cancel,
}

/// A book and quill being written in.
pub struct BookEditState {
    pages: Vec<String>,
    page: usize,
    field: MultilineField,
    /// The page the field holds, so a turned page is loaded into it.
    loaded: Option<usize>,
    /// The title being typed, while signing.
    title: Option<String>,
    owner: String,
    slot: u32,
    /// The screen was closed by one of its buttons, with this to send.
    result: Option<BookEdit>,
}

impl BookEditState {
    /// `pages` are the book's own; `slot` the inventory slot it is held
    /// in, and `owner` who is writing.
    pub fn new(mut pages: Vec<String>, slot: u32, owner: String) -> Self {
        if pages.is_empty() {
            pages.push(String::new());
        }
        Self {
            pages,
            page: 0,
            field: MultilineField::new(PAGE_CHARACTERS, Some(LINES)),
            loaded: None,
            title: None,
            owner,
            slot,
            result: None,
        }
    }

    /// The field holds the page shown, wrapped to the page's width.
    fn load(&mut self, width: &dyn Fn(&str) -> f32) {
        if self.loaded != Some(self.page) {
            self.field.set_width(TEXT_WIDTH as f32, width);
            self.field.set_value(&self.pages[self.page], width);
            self.loaded = Some(self.page);
        }
    }

    /// Typing: on the page, or on the title while signing. Page Up and
    /// Page Down turn the page (`BookEditScreen.keyPressed`).
    pub fn handle_text_input(&mut self, events: &[TextInputEvent], width: &dyn Fn(&str) -> f32) {
        use winit::keyboard::KeyCode;
        self.load(width);
        for event in events {
            if let Some(title) = &mut self.title {
                match event {
                    TextInputEvent::Char(c)
                        if *c >= ' '
                            && *c != '\u{7f}'
                            && *c != '\u{a7}'
                            && title.chars().count() < TITLE_CHARACTERS =>
                    {
                        title.push(*c);
                    }
                    TextInputEvent::Key {
                        code: KeyCode::Backspace,
                        ..
                    } => {
                        title.pop();
                    }
                    _ => {}
                }
                continue;
            }
            match event {
                TextInputEvent::Key {
                    code: KeyCode::PageUp,
                    ..
                } => self.press(EditButton::Back),
                TextInputEvent::Key {
                    code: KeyCode::PageDown,
                    ..
                } => self.press(EditButton::Forward),
                _ => {
                    self.field.handle(event, &mut SystemClipboard, width);
                    self.pages[self.page] = self.field.value().to_owned();
                }
            }
            self.load(width);
        }
    }

    /// A button's press.
    pub fn press(&mut self, button: EditButton) {
        match button {
            EditButton::Back => {
                self.page = self.page.saturating_sub(1);
            }
            // `pageForward`: past the last page a new one is added.
            EditButton::Forward => {
                if self.page + 1 >= self.pages.len() && self.pages.len() < MAX_PAGES {
                    self.pages.push(String::new());
                }
                self.page = (self.page + 1).min(self.pages.len() - 1);
            }
            EditButton::Sign => self.title = Some(String::new()),
            EditButton::Cancel => self.title = None,
            EditButton::Done => self.close(None),
            EditButton::Finalize => {
                let title = self.title.as_deref().unwrap_or("").trim().to_owned();
                if !title.is_empty() {
                    self.close(Some(title));
                }
            }
        }
    }

    /// `saveChanges`: the pages, less the empty ones at the end.
    fn close(&mut self, title: Option<String>) {
        let mut pages = self.pages.clone();
        while pages.last().is_some_and(String::is_empty) {
            pages.pop();
        }
        self.result = Some(BookEdit {
            slot: self.slot,
            pages,
            title,
        });
    }

    /// What to send, once a button has closed the screen.
    pub fn take_result(&mut self) -> Option<BookEdit> {
        self.result.take()
    }
}

/// A book and quill's pages (`WritableBookContent`), for writing on.
pub fn writable_pages(stack: &ItemStackData) -> Option<Vec<String>> {
    let book = stack.get_component::<WritableBookContent>()?;
    Some(book.pages.iter().map(|page| page.raw.clone()).collect())
}

/// Builds the editing screen, or the signing one while a title is being
/// typed. The button under the cursor is returned.
#[allow(clippy::too_many_arguments)]
pub fn build_edit(
    elements: &mut Vec<MenuElement>,
    state: &BookEditState,
    screen_w: f32,
    screen_h: f32,
    cursor: (f32, f32),
    gs: f32,
    split: SplitLines<'_>,
    text_width: &dyn Fn(&str, f32) -> f32,
) -> Option<EditButton> {
    push_transparent_background(elements, screen_w, screen_h);
    let left = ((screen_w / gs - 192.0) / 2.0).floor() * gs;
    let top = 2.0 * gs;
    let scale = FONT_SIZE * gs;
    let centre = screen_w / 2.0;
    elements.push(MenuElement::Image {
        x: left,
        y: top,
        w: 192.0 * gs,
        h: 192.0 * gs,
        sprite: SpriteId::BookBackground,
        tint: [1.0; 4],
    });
    let width = |text: &str| text_width(text, scale);
    let flat = |elements: &mut Vec<MenuElement>, x: f32, y: f32, text: &str| {
        elements.push(MenuElement::TextFlat {
            x,
            y,
            text: text.to_owned(),
            scale,
            color: BLACK,
        });
    };
    // The cursor blinks every 300 ms.
    let blink = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis())
        / 300)
        .is_multiple_of(2);
    let mut over = None;
    let button = |elements: &mut Vec<MenuElement>, x: f32, label: &str, enabled: bool| {
        push_button(
            elements,
            cursor,
            centre + x * gs,
            196.0 * gs,
            98.0 * gs,
            20.0 * gs,
            gs,
            scale,
            label,
            enabled,
        )
    };

    if let Some(title) = &state.title {
        // `BookSignScreen`: each line centred over the page's 114.
        let centred = |elements: &mut Vec<MenuElement>, y: f32, text: &str| {
            let x = left + 36.0 * gs + ((114.0 * gs - width(text)) / 2.0).floor();
            flat(elements, x, y * gs, text);
        };
        centred(elements, 34.0, "Enter Book Title:");
        let shown = if blink {
            format!("{title}_")
        } else {
            title.clone()
        };
        let x = left + 36.0 * gs + ((114.0 * gs - width(title)) / 2.0).floor();
        flat(elements, x, 50.0 * gs, &shown);
        centred(elements, 60.0, &format!("by {}", state.owner));
        let warning = [TextSpan::new(
            "Note! When you sign the book, it will no longer be editable.".into(),
            BLACK,
        )];
        for (index, line) in split(&warning, TEXT_WIDTH).into_iter().enumerate() {
            elements.push(MenuElement::TextSpans {
                x: left + 36.0 * gs,
                y: (82.0 + 9.0 * index as f32) * gs,
                spans: unshadowed(line),
                scale,
                centered: false,
            });
        }
        if button(elements, -100.0, "Sign and Close", !title.trim().is_empty()) {
            over = Some(EditButton::Finalize);
        }
        if button(elements, 2.0, "Cancel", true) {
            over = Some(EditButton::Cancel);
        }
        return over;
    }

    let indicator = format!("Page {} of {}", state.page + 1, state.pages.len());
    flat(
        elements,
        left + 148.0 * gs - width(&indicator),
        top + 16.0 * gs,
        &indicator,
    );
    // The page as the field has it wrapped, and the cursor where it is.
    let value = state.field.value();
    let (text_x, text_y) = (left + 35.0 * gs, top + 30.0 * gs);
    let cursor_at = state.field.cursor();
    let cursor_line = state.field.line_at_cursor();
    for (index, (begin, end)) in state.field.lines().iter().enumerate() {
        let y = text_y + 9.0 * index as f32 * gs;
        let line = &value[*begin..*end];
        if !line.is_empty() {
            flat(elements, text_x, y, line);
        }
        if blink && index == cursor_line {
            let x = text_x + width(&value[*begin..cursor_at.clamp(*begin, *end)]);
            if cursor_at >= value.len() {
                flat(elements, x, y, "_");
            } else {
                elements.push(MenuElement::Rect {
                    x,
                    y: y - gs,
                    w: gs,
                    h: 10.0 * gs,
                    corner_radius: 0.0,
                    color: BLACK,
                });
            }
        }
    }

    for (which, shown, x, plain, lit) in [
        (
            EditButton::Forward,
            true,
            116.0,
            SpriteId::PageForward,
            SpriteId::PageForwardHighlighted,
        ),
        (
            EditButton::Back,
            state.page > 0,
            43.0,
            SpriteId::PageBackward,
            SpriteId::PageBackwardHighlighted,
        ),
    ] {
        if !shown {
            continue;
        }
        let rect = [left + x * gs, top + 157.0 * gs, 23.0 * gs, 13.0 * gs];
        let hovered = hit_test(cursor, rect);
        elements.push(MenuElement::Image {
            x: rect[0],
            y: rect[1],
            w: rect[2],
            h: rect[3],
            sprite: if hovered { lit } else { plain },
            tint: [1.0; 4],
        });
        if hovered {
            over = Some(which);
        }
    }
    if button(elements, -100.0, "Sign", true) {
        over = Some(EditButton::Sign);
    }
    if button(elements, 2.0, "Done", true) {
        over = Some(EditButton::Done);
    }
    over
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pages turn within the book, by the buttons or by Page Up and Page
    /// Down, and a click over nothing does nothing.
    #[test]
    fn pages_turn_within_the_book() {
        let page = || vec![TextSpan::new("text".into(), BLACK)];
        let mut view = BookViewState {
            pages: vec![page(), page(), page()],
            page: 0,
        };
        assert!(!view.turn(BookButton::Back));
        assert!(view.turn(BookButton::Forward));
        assert!(view.turn(BookButton::Forward));
        assert!(!view.turn(BookButton::Forward));
        assert_eq!(view.page, 2);
        assert!(view.turn(BookButton::Back));
        assert!(!view.turn(BookButton::Done));
        assert_eq!(view.page, 1);

        assert_eq!(
            pressed(Some(BookButton::Done), true, false, false),
            Some(BookButton::Done)
        );
        assert_eq!(pressed(Some(BookButton::Done), false, false, false), None);
        assert_eq!(pressed(None, true, false, true), Some(BookButton::Forward));
        assert_eq!(pressed(None, false, true, false), Some(BookButton::Back));
    }

    fn typed(text: &str) -> Vec<TextInputEvent> {
        text.chars().map(TextInputEvent::Char).collect()
    }

    /// Writing fills the page shown, a turn past the last adds a page, the
    /// empty pages at the end are not sent, and a blank title does not
    /// sign.
    #[test]
    fn a_book_is_written_and_signed() {
        let width = |text: &str| text.chars().count() as f32 * 6.0;
        let mut book = BookEditState::new(Vec::new(), 36, "Tester".into());
        book.handle_text_input(&typed("Hello"), &width);
        book.press(EditButton::Forward);
        book.handle_text_input(&typed("there"), &width);
        book.press(EditButton::Forward);
        book.press(EditButton::Back);
        book.handle_text_input(&typed("!"), &width);

        book.press(EditButton::Sign);
        book.handle_text_input(&typed("   "), &width);
        book.press(EditButton::Finalize);
        assert_eq!(book.take_result(), None);
        book.handle_text_input(&typed("A very long title indeed"), &width);
        book.press(EditButton::Finalize);
        assert_eq!(
            book.take_result(),
            Some(BookEdit {
                slot: 36,
                pages: vec!["Hello".into(), "there!".into()],
                title: Some("A very long".into()),
            })
        );

        let mut book = BookEditState::new(vec!["kept".into()], 40, "Tester".into());
        book.press(EditButton::Done);
        assert_eq!(
            book.take_result(),
            Some(BookEdit {
                slot: 40,
                pages: vec!["kept".into()],
                title: None,
            })
        );
    }
}
