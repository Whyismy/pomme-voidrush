//! Vanilla `AdvancementsScreen`: a window with a tab for each tree of
//! advancements, the tree drawn inside over its tab's background (each
//! advancement a framed icon, joined to the one it follows), dragged about
//! with the mouse.
// TODO: vanilla's own title and description box over a hovered advancement
// (a tooltip stands in for it), its progress count, and more tabs than the
// eight that fit above the window.

use std::collections::HashMap;

use super::book::SplitLines;
use super::common::{FONT_SIZE, hit_test, push_button, push_item_glyph, push_tooltip_lines};
use super::text::TextSpan;
use super::toast::{AdvancementData, AdvancementDisplay, AdvancementFrame, AdvancementStore};
use crate::renderer::pipelines::menu_overlay::{
    ADVANCEMENT_BACKGROUNDS, MenuElement, SpriteId, TooltipLine,
};

/// The window, and the part of it the tree shows through.
const WINDOW: (f32, f32) = (252.0, 140.0);
const INSIDE: [f32; 4] = [9.0, 18.0, 234.0, 113.0];
/// The tabs that fit above the window (`AdvancementTabType.ABOVE`).
const MOST_TABS: usize = 8;
/// The order the game's own tabs come in.
const TAB_ORDER: [&str; 5] = [
    "minecraft:story/root",
    "minecraft:nether/root",
    "minecraft:end/root",
    "minecraft:adventure/root",
    "minecraft:husbandry/root",
];

/// The advancements screen's own state.
#[derive(Default)]
pub struct AdvancementsUi {
    /// The pause menu opened it, and has it back when it closes.
    pub from_pause: bool,
    /// The tab shown, by its root's id.
    tab: Option<String>,
    /// Each tab's scroll, once it has been centred.
    scroll: HashMap<String, (f32, f32)>,
    /// The tree is being dragged, from this cursor position.
    dragging: Option<(f32, f32)>,
    /// The tab the server was last told is open.
    told: Option<String>,
    closing: bool,
}

impl AdvancementsUi {
    /// The screen as the pause menu opens it.
    pub fn from_pause() -> Self {
        Self {
            from_pause: true,
            ..Self::default()
        }
    }

    /// Marks the screen closed; the frame then tells the server.
    pub fn close(&mut self) {
        self.closing = true;
    }

    pub fn closing(&self) -> bool {
        self.closing
    }

    /// The tab now shown, where the server has not been told of it yet.
    pub fn take_opened(&mut self) -> Option<String> {
        if self.tab == self.told {
            return None;
        }
        self.told = self.tab.clone();
        self.tab.clone()
    }
}

/// Where an advancement's widget sits in its tree, in GUI pixels
/// (`AdvancementWidget`'s `x` and `y`).
fn widget_at(display: &AdvancementDisplay) -> (f32, f32) {
    (
        (display.position.0 * 28.0).floor(),
        (display.position.1 * 27.0).floor(),
    )
}

/// The root an advancement hangs from.
fn root_of<'a>(store: &'a AdvancementStore, mut id: &'a str) -> &'a str {
    // A tree is not deep; the bound guards against a cycle.
    for _ in 0..256 {
        match store.get(id).and_then(|data| data.parent.as_deref()) {
            Some(parent) => id = parent,
            None => break,
        }
    }
    id
}

/// The tabs: each root that is shown, the game's own in their order and
/// then the rest by name.
fn tabs(store: &AdvancementStore) -> Vec<(&String, &AdvancementDisplay)> {
    let mut roots: Vec<_> = store
        .iter()
        .filter(|(_, data)| data.parent.is_none())
        .filter_map(|(id, data)| Some((id, data.display.as_ref()?)))
        .collect();
    let rank = |id: &str| {
        TAB_ORDER
            .iter()
            .position(|known| *known == id)
            .unwrap_or(TAB_ORDER.len())
    };
    roots.sort_by(|a, b| (rank(a.0), a.0).cmp(&(rank(b.0), b.0)));
    roots.truncate(MOST_TABS);
    roots
}

/// The sprite of a background named like
/// `minecraft:gui/advancements/backgrounds/stone`.
fn background_sprite(background: Option<&str>) -> SpriteId {
    let name = background
        .and_then(|id| id.rsplit('/').next())
        .map(|file| file.trim_end_matches(".png"))
        .unwrap_or("stone");
    let index = ADVANCEMENT_BACKGROUNDS
        .iter()
        .position(|known| *known == name)
        .unwrap_or(0);
    SpriteId::AdvancementBackground(index as u8)
}

fn frame_sprite(frame: AdvancementFrame, obtained: bool) -> SpriteId {
    match (frame, obtained) {
        (AdvancementFrame::Task, true) => SpriteId::AdvancementTaskObtained,
        (AdvancementFrame::Task, false) => SpriteId::AdvancementTaskUnobtained,
        (AdvancementFrame::Challenge, true) => SpriteId::AdvancementChallengeObtained,
        (AdvancementFrame::Challenge, false) => SpriteId::AdvancementChallengeUnobtained,
        (AdvancementFrame::Goal, true) => SpriteId::AdvancementGoalObtained,
        (AdvancementFrame::Goal, false) => SpriteId::AdvancementGoalUnobtained,
    }
}

fn plain(spans: &[TextSpan]) -> String {
    spans.iter().map(|span| span.text.as_str()).collect()
}

/// `AdvancementTab.scroll`: a tree wider or taller than the window moves
/// within it, never past its own edges.
fn clamp_scroll(scroll: f32, moved: f32, low: f32, high: f32, room: f32) -> f32 {
    if high - low > room {
        (scroll + moved).clamp(-(high - room), 0.0)
    } else {
        scroll
    }
}

/// Builds the screen; true while the cursor is over its Done button and
/// the mouse was pressed. `mouse` is the left button's press this frame and
/// whether it is held.
#[allow(clippy::too_many_arguments)]
pub fn build(
    elements: &mut Vec<MenuElement>,
    ui: &mut AdvancementsUi,
    store: &AdvancementStore,
    screen_w: f32,
    screen_h: f32,
    cursor: (f32, f32),
    mouse: (bool, bool),
    gs: f32,
    text_width: &dyn Fn(&str, f32) -> f32,
    split: SplitLines<'_>,
) -> bool {
    let (pressed, held) = mouse;
    // `Screen.extractBackground`: the world blurred, under the menu's dimming.
    elements.push(MenuElement::BlurBackground);
    super::common::push_menu_background(elements, screen_w, screen_h);
    let scale = FONT_SIZE * gs;
    let (gui_w, gui_h) = (screen_w / gs, screen_h / gs);
    let left = ((gui_w - WINDOW.0) / 2.0).floor();
    let top = ((gui_h - WINDOW.1) / 2.0).floor();
    let px = |v: f32| v * gs;
    let translate =
        |key: &'static str, fallback: &'static str| crate::lang::translate(key).unwrap_or(fallback);
    let title = translate("gui.advancements", "Advancements");
    elements.push(MenuElement::Text {
        x: screen_w / 2.0,
        y: px(12.0),
        text: title.to_owned(),
        scale,
        color: [1.0; 4],
        centered: true,
    });

    // Which tab is shown: the one picked, else the server's, else the first.
    let tabs = tabs(store);
    let shown = ui
        .tab
        .as_deref()
        .filter(|tab| tabs.iter().any(|(id, _)| id.as_str() == *tab))
        .or_else(|| {
            store
                .selected_tab()
                .filter(|tab| tabs.iter().any(|(id, _)| id.as_str() == *tab))
        })
        .or_else(|| tabs.first().map(|(id, _)| id.as_str()))
        .map(str::to_owned);
    ui.tab = shown.clone();

    let inside = [
        px(left + INSIDE[0]),
        px(top + INSIDE[1]),
        px(INSIDE[2]),
        px(INSIDE[3]),
    ];
    let over_inside = hit_test(cursor, inside);
    let mut hovered: Option<&AdvancementDisplay> = None;

    elements.push(MenuElement::ScissorPush {
        x: inside[0],
        y: inside[1],
        w: inside[2],
        h: inside[3],
    });
    let rect = |elements: &mut Vec<MenuElement>, at: [f32; 4], color: [f32; 4]| {
        elements.push(MenuElement::Rect {
            x: px(at[0]),
            y: px(at[1]),
            w: px(at[2]),
            h: px(at[3]),
            corner_radius: 0.0,
            color,
        });
    };
    if let Some(tab) = &shown {
        // The tree: every shown advancement hanging from this root.
        let tree: Vec<(&String, &AdvancementData, &AdvancementDisplay)> = store
            .iter()
            .filter(|(id, _)| root_of(store, id) == tab.as_str())
            .filter_map(|(id, data)| Some((id, data, data.display.as_ref()?)))
            .collect();
        let (mut min, mut max) = ((f32::MAX, f32::MAX), (f32::MIN, f32::MIN));
        for (_, _, display) in &tree {
            let (x, y) = widget_at(display);
            min = (min.0.min(x), min.1.min(y));
            max = (max.0.max(x + 28.0), max.1.max(y + 27.0));
        }
        // `extractContents`: a tab starts with its tree centred.
        let scroll = ui
            .scroll
            .entry(tab.clone())
            .or_insert((117.0 - (max.0 + min.0) / 2.0, 56.0 - (max.1 + min.1) / 2.0));
        if pressed && over_inside {
            ui.dragging = Some(cursor);
        }
        if !held {
            ui.dragging = None;
        }
        if let Some(from) = ui.dragging {
            let moved = ((cursor.0 - from.0) / gs, (cursor.1 - from.1) / gs);
            scroll.0 = clamp_scroll(scroll.0, moved.0, min.0, max.0, INSIDE[2]);
            scroll.1 = clamp_scroll(scroll.1, moved.1, min.1, max.1, INSIDE[3]);
            ui.dragging = Some(cursor);
        }
        let origin = (
            left + INSIDE[0] + scroll.0.floor(),
            top + INSIDE[1] + scroll.1.floor(),
        );

        // The background, tiled from a tile's width before the corner.
        let root_display = tabs
            .iter()
            .find(|(id, _)| id.as_str() == tab.as_str())
            .map(|(_, display)| *display);
        elements.push(MenuElement::TiledImage {
            x: px(left + INSIDE[0] + scroll.0.floor().rem_euclid(16.0) - 16.0),
            y: px(top + INSIDE[1] + scroll.1.floor().rem_euclid(16.0) - 16.0),
            w: px(INSIDE[2] + 32.0),
            h: px(INSIDE[3] + 32.0),
            sprite: background_sprite(root_display.and_then(|d| d.background.as_deref())),
            tile_size: px(16.0),
            tint: [1.0; 4],
        });

        // `extractConnectivity`: each advancement joined to the one it
        // follows, out from that one's side, across, and in to this one;
        // first in black a pixel wider, then in white.
        for shadow in [true, false] {
            let color = if shadow {
                [0.0, 0.0, 0.0, 1.0]
            } else {
                [1.0; 4]
            };
            for (_, data, display) in &tree {
                let Some(parent) = data
                    .parent
                    .as_deref()
                    .and_then(|parent| store.get(parent))
                    .and_then(|parent| parent.display.as_ref())
                else {
                    continue;
                };
                let (parent_x, parent_y) = widget_at(parent);
                let (x, y) = widget_at(display);
                let from_x = origin.0 + parent_x + 13.0;
                let bend = origin.0 + parent_x + 30.0;
                let from_y = origin.1 + parent_y + 13.0;
                let to_x = origin.0 + x + 13.0;
                let to_y = origin.1 + y + 13.0;
                let (low_y, high_y) = (from_y.min(to_y), from_y.max(to_y));
                let grow = if shadow { 1.0 } else { 0.0 };
                // Out from the parent to the bend.
                rect(
                    elements,
                    [
                        from_x,
                        from_y - grow,
                        bend - from_x + 1.0 + grow,
                        1.0 + 2.0 * grow,
                    ],
                    color,
                );
                // Down or up the bend.
                rect(
                    elements,
                    [
                        bend - grow,
                        low_y - grow,
                        1.0 + 2.0 * grow,
                        high_y - low_y + 1.0 + 2.0 * grow,
                    ],
                    color,
                );
                // And in to this one.
                rect(
                    elements,
                    [
                        bend - grow,
                        to_y - grow,
                        to_x - bend + 1.0 + grow,
                        1.0 + 2.0 * grow,
                    ],
                    color,
                );
            }
        }

        // The widgets: a frame by its kind and whether it is done, and its
        // icon.
        for (id, _, display) in &tree {
            let (x, y) = widget_at(display);
            let (x, y) = (origin.0 + x, origin.1 + y);
            elements.push(MenuElement::Image {
                x: px(x + 3.0),
                y: px(y),
                w: px(26.0),
                h: px(26.0),
                sprite: frame_sprite(display.frame, store.is_done(id)),
                tint: [1.0; 4],
            });
            if let Some(icon) = &display.icon_item {
                push_item_glyph(
                    elements,
                    [px(x + 8.0), px(y + 5.0), px(16.0), px(16.0)],
                    icon.clone(),
                    None,
                    false,
                );
            }
            if over_inside
                && ui.dragging.is_none()
                && hit_test(cursor, [px(x), px(y), px(27.0), px(27.0)])
            {
                hovered = Some(display);
            }
        }
    } else {
        // `extractInside` with no tab: nothing to show, sadly.
        rect(
            elements,
            [left + INSIDE[0], top + INSIDE[1], INSIDE[2], INSIDE[3]],
            [0.0, 0.0, 0.0, 1.0],
        );
        for (y, key, fallback) in [
            (
                top + INSIDE[1] + 56.0 - 4.0,
                "advancements.empty",
                "There doesn't seem to be anything here...",
            ),
            (
                top + INSIDE[1] + INSIDE[3] - 9.0,
                "advancements.sad_label",
                ":(",
            ),
        ] {
            elements.push(MenuElement::Text {
                x: px(left + INSIDE[0] + 117.0),
                y: px(y),
                text: translate(key, fallback).to_owned(),
                scale,
                color: [1.0; 4],
                centered: true,
            });
        }
    }
    elements.push(MenuElement::ScissorPop);

    // `extractWindow`: the window over the tree, its tabs where there is
    // more than one, and the shown tab's name.
    elements.push(MenuElement::Image {
        x: px(left),
        y: px(top),
        w: px(WINDOW.0),
        h: px(WINDOW.1),
        sprite: SpriteId::AdvancementsWindow,
        tint: [1.0; 4],
    });
    let mut tab_hovered = None;
    if tabs.len() > 1 {
        for (index, (id, display)) in tabs.iter().enumerate() {
            let selected = shown.as_deref() == Some(id.as_str());
            let x = left + 32.0 * index as f32;
            let y = top - 28.0;
            let sprite = match (index, selected) {
                (0, false) => SpriteId::AdvancementTabLeft,
                (0, true) => SpriteId::AdvancementTabLeftSelected,
                (7, false) => SpriteId::AdvancementTabRight,
                (7, true) => SpriteId::AdvancementTabRightSelected,
                (_, false) => SpriteId::AdvancementTabMiddle,
                (_, true) => SpriteId::AdvancementTabMiddleSelected,
            };
            let at = [px(x), px(y), px(28.0), px(32.0)];
            elements.push(MenuElement::Image {
                x: at[0],
                y: at[1],
                w: at[2],
                h: at[3],
                sprite,
                tint: [1.0; 4],
            });
            if let Some(icon) = &display.icon_item {
                push_item_glyph(
                    elements,
                    [px(x + 6.0), px(y + 9.0), px(16.0), px(16.0)],
                    icon.clone(),
                    None,
                    false,
                );
            }
            if hit_test(cursor, at) {
                tab_hovered = Some(*display);
                if pressed {
                    ui.tab = Some((*id).clone());
                }
            }
        }
    }
    let heading = tabs
        .iter()
        .find(|(id, _)| shown.as_deref() == Some(id.as_str()))
        .map_or_else(|| title.to_owned(), |(_, display)| plain(&display.title));
    elements.push(MenuElement::TextFlat {
        x: px(left + 8.0),
        y: px(top + 6.0),
        text: heading,
        scale,
        // 0x404040.
        color: [64.0 / 255.0, 64.0 / 255.0, 64.0 / 255.0, 1.0],
    });

    // What is under the cursor: an advancement's name and what it asks
    // for, or a tab's name.
    let mut lines = Vec::new();
    if let Some(display) = hovered {
        lines.push(TooltipLine::from_spans(display.title.clone()));
        let green: Vec<TextSpan> = display
            .description
            .iter()
            .cloned()
            .map(|mut span| {
                span.color = [0.0, 1.0, 0.0, 1.0];
                span
            })
            .collect();
        lines.extend(split(&green, 163).into_iter().map(TooltipLine::from_spans));
    } else if let Some(display) = tab_hovered {
        lines.push(TooltipLine::from_spans(display.title.clone()));
    }
    if !lines.is_empty() {
        push_tooltip_lines(elements, cursor, screen_w, screen_h, gs, lines);
    }

    // The footer's Done.
    let done = push_button(
        elements,
        cursor,
        screen_w / 2.0 - px(100.0),
        px(gui_h - 27.0),
        px(200.0),
        px(20.0),
        gs,
        scale,
        translate("gui.done", "Done"),
        true,
    );
    let _ = text_width;
    done && pressed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::toast::AdvancementsUpdate;

    fn advancement(parent: Option<&str>, at: (f32, f32)) -> AdvancementData {
        AdvancementData {
            parent: parent.map(str::to_owned),
            display: Some(AdvancementDisplay {
                title: vec![TextSpan::new("Title".into(), [1.0; 4])],
                description: vec![TextSpan::new("What to do".into(), [1.0; 4])],
                frame: AdvancementFrame::Task,
                show_toast: false,
                icon_item: Some("stone".into()),
                background: Some("minecraft:gui/advancements/backgrounds/adventure".into()),
                position: at,
            }),
            requirements: vec![vec!["done".into()]],
        }
    }

    fn store() -> crate::ui::toast::ToastState {
        let mut toasts = crate::ui::toast::ToastState::default();
        toasts.apply_advancements(AdvancementsUpdate {
            reset: true,
            added: vec![
                ("minecraft:story/root".into(), advancement(None, (0.0, 0.0))),
                (
                    "minecraft:story/mine_stone".into(),
                    advancement(Some("minecraft:story/root"), (1.0, 2.0)),
                ),
                (
                    "minecraft:adventure/root".into(),
                    advancement(None, (0.0, 0.0)),
                ),
                ("mod:other/root".into(), advancement(None, (0.0, 0.0))),
            ],
            removed: Vec::new(),
            progress: vec![(
                "minecraft:story/root".into(),
                [("done".to_owned(), true)].into_iter().collect(),
            )],
            show_advancements: false,
        });
        toasts
    }

    /// Roots are the tabs, the game's own in their order; an advancement
    /// hangs from its root, sits by its position, and is done by its
    /// criteria.
    #[test]
    fn advancements_hang_from_their_tabs() {
        let toasts = store();
        let store = toasts.advancements();
        let ids: Vec<&str> = tabs(store).iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "minecraft:story/root",
                "minecraft:adventure/root",
                "mod:other/root"
            ]
        );
        assert_eq!(
            root_of(store, "minecraft:story/mine_stone"),
            "minecraft:story/root"
        );
        let stone = store.get("minecraft:story/mine_stone").unwrap();
        assert_eq!(widget_at(stone.display.as_ref().unwrap()), (28.0, 54.0));
        assert!(store.is_done("minecraft:story/root"));
        assert!(!store.is_done("minecraft:story/mine_stone"));
        assert_eq!(
            background_sprite(Some("minecraft:gui/advancements/backgrounds/adventure")),
            SpriteId::AdvancementBackground(1)
        );
        assert_eq!(background_sprite(None), SpriteId::AdvancementBackground(0));
    }

    /// A tree no bigger than the window stays put; a bigger one moves
    /// between its edges.
    #[test]
    fn a_tree_scrolls_within_its_edges() {
        assert_eq!(clamp_scroll(10.0, 50.0, 0.0, 200.0, 234.0), 10.0);
        assert_eq!(clamp_scroll(0.0, 50.0, 0.0, 400.0, 234.0), 0.0);
        assert_eq!(clamp_scroll(0.0, -50.0, 0.0, 400.0, 234.0), -50.0);
        assert_eq!(clamp_scroll(0.0, -500.0, 0.0, 400.0, 234.0), -166.0);
    }

    /// The server hears of a tab once, when it is first shown.
    #[test]
    fn the_server_is_told_of_a_tab_once() {
        let mut ui = AdvancementsUi::default();
        assert_eq!(ui.take_opened(), None);
        ui.tab = Some("minecraft:story/root".into());
        assert_eq!(ui.take_opened().as_deref(), Some("minecraft:story/root"));
        assert_eq!(ui.take_opened(), None);
    }
}
