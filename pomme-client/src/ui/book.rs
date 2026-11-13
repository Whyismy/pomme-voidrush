//! Vanilla `BookViewScreen`: a book's pages one at a time on the open
//! book's picture, with its page count, the buttons that turn the page and
//! Done. `LecternScreen` is the same over a lectern's menu, with a button
//! to take the book.
// TODO: a page's click events, and writing in a book and quill
// (`BookEditScreen`).

use azalea_inventory::ItemStackData;
use azalea_inventory::components::{WritableBookContent, WrittenBookContent};

use super::common::{FONT_SIZE, hit_test, push_button};
use super::container::push_transparent_background;
use super::text::{TextSpan, format_text_spans};
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
}
