//! Vanilla `StatsScreen`: what the server has counted for the player, as
//! three lists behind a tab bar (general, items, mobs).
// TODO: keyboard navigation between tabs and rows, and a translated
// session's block and custom-stat ids (they are read as the native ones).

use std::collections::HashMap;
use std::time::Instant;

use azalea_registry::Registry;
use azalea_registry::builtin::{BlockKind, CustomStat, EntityKind, ItemKind};

use super::common::{
    BTN_H, FONT_SIZE, WHITE, hit_test, push_button, push_item_glyph, push_tooltip_lines,
};
use crate::renderer::pipelines::menu_overlay::{MenuElement, SpriteId, TooltipLine};

/// `TabNavigationBar`: its height, widest span, and the margin either side.
const TAB_BAR_H: f32 = 24.0;
const TAB_BAR_MAX_W: f32 = 400.0;
const TAB_BAR_MARGIN: f32 = 28.0;
/// `HeaderAndFooterLayout`'s footer.
const FOOTER_H: f32 = 33.0;
/// Each list's `getRowWidth`.
const ROW_W: f32 = 280.0;
/// The lists' row heights: general, items, mobs (four lines of text).
const ROW_H: [f32; 3] = [14.0, 22.0, 36.0];
/// A row's text on an even row and an odd one (`-1` and `-4539718`).
const GREY: [f32; 4] = [186.0 / 255.0, 186.0 / 255.0, 186.0 / 255.0, 1.0];
/// A mob's line for a count of none (`-8355712`).
const DARK_GREY: [f32; 4] = [128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0, 1.0];
/// A tab that cannot be opened (`-6250336`).
const INACTIVE: [f32; 4] = [160.0 / 255.0, 160.0 / 255.0, 160.0 / 255.0, 1.0];

/// Vanilla's stat types, in the `stat_type` registry's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatType {
    Mined,
    Crafted,
    Used,
    Broken,
    PickedUp,
    Dropped,
    Killed,
    KilledBy,
    Custom,
}

impl StatType {
    pub fn from_id(id: u32) -> Option<Self> {
        Some(match id {
            0 => Self::Mined,
            1 => Self::Crafted,
            2 => Self::Used,
            3 => Self::Broken,
            4 => Self::PickedUp,
            5 => Self::Dropped,
            6 => Self::Killed,
            7 => Self::KilledBy,
            8 => Self::Custom,
            _ => return None,
        })
    }
}

/// The items list's columns, left to right, with their headers' icons and
/// the names their tooltips give.
const COLUMNS: [(StatType, SpriteId, &str, &str); 6] = [
    (
        StatType::Mined,
        SpriteId::StatBlockMined,
        "stat_type.minecraft.mined",
        "Times Mined",
    ),
    (
        StatType::Broken,
        SpriteId::StatItemBroken,
        "stat_type.minecraft.broken",
        "Times Broken",
    ),
    (
        StatType::Crafted,
        SpriteId::StatItemCrafted,
        "stat_type.minecraft.crafted",
        "Times Crafted",
    ),
    (
        StatType::Used,
        SpriteId::StatItemUsed,
        "stat_type.minecraft.used",
        "Times Used",
    ),
    (
        StatType::PickedUp,
        SpriteId::StatItemPickedUp,
        "stat_type.minecraft.picked_up",
        "Picked Up",
    ),
    (
        StatType::Dropped,
        SpriteId::StatItemDropped,
        "stat_type.minecraft.dropped",
        "Dropped",
    ),
];

/// Vanilla `StatsCounter` on the client: the last value the server sent for
/// each stat, by its type and its value's id in that type's registry.
#[derive(Default)]
pub struct StatsCounter {
    values: HashMap<(StatType, u32), i32>,
}

impl StatsCounter {
    /// `ClientPacketListener.handleAwardStats`: each stat sent takes its value.
    pub fn apply(&mut self, stats: impl IntoIterator<Item = (StatType, u32, i32)>) {
        for (kind, id, value) in stats {
            self.values.insert((kind, id), value);
        }
    }

    pub fn get(&self, kind: StatType, id: u32) -> i32 {
        self.values.get(&(kind, id)).copied().unwrap_or(0)
    }
}

/// A registry entry's name without its namespace.
fn bare(name: String) -> String {
    name.strip_prefix("minecraft:")
        .map(str::to_owned)
        .unwrap_or(name)
}

/// `NumberFormat.getIntegerInstance(Locale.US)`: thousands set off by commas.
fn grouped(value: i32) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    if value < 0 {
        out.push('-');
    }
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Vanilla `StatFormatter`, by the custom stat's name: times, distances and
/// tenths each have their own.
fn format_custom(name: &str, value: i32) -> String {
    let v = f64::from(value);
    match name {
        "play_time" | "total_world_time" | "time_since_death" | "time_since_rest"
        | "sneak_time" => {
            let seconds = v / 20.0;
            let minutes = seconds / 60.0;
            let hours = minutes / 60.0;
            let days = hours / 24.0;
            let years = days / 365.0;
            if years > 0.5 {
                format!("{years:.2} y")
            } else if days > 0.5 {
                format!("{days:.2} d")
            } else if hours > 0.5 {
                format!("{hours:.2} h")
            } else if minutes > 0.5 {
                format!("{minutes:.2} min")
            } else {
                // Java's `Double.toString` keeps a whole number's `.0`.
                format!("{seconds:?} s")
            }
        }
        _ if name.ends_with("_one_cm") => {
            let meters = v / 100.0;
            let kilometers = meters / 1000.0;
            if kilometers > 0.5 {
                format!("{kilometers:.2} km")
            } else if meters > 0.5 {
                format!("{meters:.2} m")
            } else {
                format!("{value} cm")
            }
        }
        _ if name.starts_with("damage_") => format!("{:.2}", v * 0.1),
        _ => grouped(value),
    }
}

/// The block an item places, as `BlockItem.getBlock` has it: the block of
/// its own name, or the one a seed or the like plants.
fn block_of_item(item: &str) -> &str {
    match item {
        "wheat_seeds" => "wheat",
        "carrot" => "carrots",
        "potato" => "potatoes",
        "beetroot_seeds" => "beetroots",
        "cocoa_beans" => "cocoa",
        "sweet_berries" => "sweet_berry_bush",
        "redstone" => "redstone_wire",
        "string" => "tripwire",
        "melon_seeds" => "melon_stem",
        "pumpkin_seeds" => "pumpkin_stem",
        "glow_berries" => "cave_vines",
        "pitcher_pod" => "pitcher_crop",
        "torchflower_seeds" => "torchflower_crop",
        other => other,
    }
}

/// The item a block is (`Block.asItem`), where it has one: the item of its
/// own name, the item that plants it, or a wall-mounted block's standing
/// form.
// TODO: this follows the names; vanilla reads `Item.BY_BLOCK`.
fn item_of_block(block: &str, is_item: &dyn Fn(&str) -> bool) -> Option<String> {
    if is_item(block) {
        return Some(block.to_owned());
    }
    let named = match block {
        "wheat" => "wheat_seeds",
        "carrots" => "carrot",
        "potatoes" => "potato",
        "beetroots" => "beetroot_seeds",
        "cocoa" => "cocoa_beans",
        "sweet_berry_bush" => "sweet_berries",
        "redstone_wire" => "redstone",
        "tripwire" => "string",
        "melon_stem" | "attached_melon_stem" => "melon_seeds",
        "pumpkin_stem" | "attached_pumpkin_stem" => "pumpkin_seeds",
        "cave_vines" | "cave_vines_plant" => "glow_berries",
        "pitcher_crop" => "pitcher_pod",
        "torchflower_crop" => "torchflower_seeds",
        "kelp_plant" => "kelp",
        "bamboo_sapling" => "bamboo",
        "weeping_vines_plant" => "weeping_vines",
        "twisting_vines_plant" => "twisting_vines",
        "big_dripleaf_stem" => "big_dripleaf",
        _ => "",
    };
    if !named.is_empty() {
        return Some(named.to_owned());
    }
    let standing = block.strip_prefix("wall_").map(str::to_owned).or_else(|| {
        block
            .contains("_wall_")
            .then(|| block.replacen("_wall_", "_", 1))
    })?;
    is_item(&standing).then_some(standing)
}

struct GeneralRow {
    name: String,
    value: String,
}

struct ItemRow {
    id: u32,
    /// The item's name in the registry, for its icon.
    resource: String,
    display: String,
    /// A count for each of [`COLUMNS`]; the first only where the item is a
    /// block's.
    values: [Option<i32>; 6],
}

struct MobRow {
    name: String,
    kills: String,
    has_kills: bool,
    killed_by: String,
    was_killed_by: bool,
}

/// `StatsScreen.GeneralStatisticsList`: every custom stat, by its name.
fn general_rows(stats: &StatsCounter) -> Vec<GeneralRow> {
    let mut rows: Vec<GeneralRow> = (0..)
        .map_while(CustomStat::from_u32)
        .map(|stat| {
            let name = bare(stat.to_string());
            let key = format!("stat.minecraft.{name}");
            GeneralRow {
                value: format_custom(&name, stats.get(StatType::Custom, stat.to_u32())),
                name: crate::lang::translate(&key).map_or(key, str::to_owned),
            }
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

/// `StatsScreen.ItemStatisticsList`: each item with a count of its own, or
/// whose block has been mined.
fn item_rows(stats: &StatsCounter) -> Vec<ItemRow> {
    let items: Vec<(u32, ItemKind, String)> = (0..)
        .map_while(ItemKind::from_u32)
        .map(|kind| (kind.to_u32(), kind, bare(kind.to_string())))
        .collect();
    let item_ids: HashMap<&str, u32> = items
        .iter()
        .map(|(id, _, name)| (name.as_str(), *id))
        .collect();
    let blocks: HashMap<String, u32> = (0..)
        .map_while(BlockKind::from_u32)
        .map(|kind| (bare(kind.to_string()), kind.to_u32()))
        .collect();

    let mut listed: Vec<bool> = items
        .iter()
        .map(|(id, ..)| {
            COLUMNS[1..]
                .iter()
                .any(|(kind, ..)| stats.get(*kind, *id) > 0)
        })
        .collect();
    for (block, id) in &blocks {
        if stats.get(StatType::Mined, *id) <= 0 {
            continue;
        }
        let item = item_of_block(block, &|name| item_ids.contains_key(name));
        if let Some(item) = item.and_then(|name| item_ids.get(name.as_str()).copied()) {
            listed[item as usize] = true;
        }
    }

    items
        .iter()
        .filter(|(id, _, name)| listed[*id as usize] && name != "air")
        .map(|(id, kind, name)| {
            let mut values = [None; 6];
            values[0] = blocks
                .get(block_of_item(name))
                .map(|block| stats.get(StatType::Mined, *block));
            for (column, (stat, ..)) in COLUMNS.iter().enumerate().skip(1) {
                values[column] = Some(stats.get(*stat, *id));
            }
            ItemRow {
                id: *id,
                resource: name.clone(),
                display: crate::lang::item_display_name(*kind),
                values,
            }
        })
        .collect()
}

/// `StatsScreen.MobsStatisticsList`: each kind of entity the player has
/// killed or been killed by.
fn mob_rows(stats: &StatsCounter) -> Vec<MobRow> {
    let text = |key: &str, fallback: &str, args: [&str; 2]| {
        let mut out = crate::lang::translate(key).unwrap_or(fallback).to_owned();
        for arg in args {
            out = out.replacen("%s", arg, 1);
        }
        out
    };
    (0..)
        .map_while(EntityKind::from_u32)
        .filter_map(|kind| {
            let kills = stats.get(StatType::Killed, kind.to_u32());
            let deaths = stats.get(StatType::KilledBy, kind.to_u32());
            if kills <= 0 && deaths <= 0 {
                return None;
            }
            let resource = bare(kind.to_string());
            let name = crate::lang::translate(&format!("entity.minecraft.{resource}"))
                .map_or_else(|| crate::lang::title_case_snake(&resource), str::to_owned);
            Some(MobRow {
                kills: if kills == 0 {
                    text(
                        "stat_type.minecraft.killed.none",
                        "You have never killed %s",
                        [name.as_str(), ""],
                    )
                } else {
                    text(
                        "stat_type.minecraft.killed",
                        "You killed %s %s",
                        [kills.to_string().as_str(), name.as_str()],
                    )
                },
                has_kills: kills != 0,
                killed_by: if deaths == 0 {
                    text(
                        "stat_type.minecraft.killed_by.none",
                        "You have never been killed by %s",
                        [name.as_str(), ""],
                    )
                } else {
                    text(
                        "stat_type.minecraft.killed_by",
                        "%s killed you %s time(s)",
                        [name.as_str(), deaths.to_string().as_str()],
                    )
                },
                was_killed_by: deaths != 0,
                name,
            })
        })
        .collect()
}

/// The statistics screen's own state.
pub struct StatsUi {
    /// The pause menu opened it, and has it back when it closes.
    pub from_pause: bool,
    opened: Instant,
    /// The server has yet to be asked for the stats.
    unasked: bool,
    /// They have yet to arrive (`StatsScreen.isLoading`).
    loading: bool,
    tab: usize,
    scroll: [f32; 3],
    /// The scrollbar is held, with the cursor here last frame.
    dragging: Option<f32>,
    /// The items' sort: a column of [`COLUMNS`], and down (-1) or up (1).
    sort: Option<(usize, i8)>,
    general: Vec<GeneralRow>,
    items: Vec<ItemRow>,
    mobs: Vec<MobRow>,
    closing: bool,
}

/// What a frame of the screen did.
#[derive(Default)]
pub struct StatsResult {
    /// A widget was pressed (the click sound plays).
    pub clicked: bool,
    /// Done was pressed.
    pub done: bool,
}

impl StatsUi {
    pub fn new(from_pause: bool) -> Self {
        Self {
            from_pause,
            opened: Instant::now(),
            unasked: true,
            loading: true,
            tab: 0,
            scroll: [0.0; 3],
            dragging: None,
            sort: None,
            general: Vec::new(),
            items: Vec::new(),
            mobs: Vec::new(),
            closing: false,
        }
    }

    pub fn close(&mut self) {
        self.closing = true;
    }

    pub fn closing(&self) -> bool {
        self.closing
    }

    /// `StatsScreen.init`: the stats are asked for once, as the screen
    /// opens.
    pub fn take_request(&mut self) -> bool {
        std::mem::take(&mut self.unasked)
    }

    /// `StatsScreen.onStatsUpdated`: the lists are made from the first
    /// stats to arrive after the screen opened.
    pub fn loaded(&mut self, stats: &StatsCounter) {
        if !self.loading {
            return;
        }
        self.loading = false;
        self.general = general_rows(stats);
        self.items = item_rows(stats);
        self.mobs = mob_rows(stats);
        self.tab = 0;
    }

    /// The rows a tab's list holds (the items' header among them).
    fn row_count(&self, tab: usize) -> usize {
        match tab {
            0 => self.general.len(),
            1 if self.items.is_empty() => 0,
            1 => self.items.len() + 1,
            _ => self.mobs.len(),
        }
    }

    /// `ItemStatisticsList.sortByColumn`: a column sorts down, then up,
    /// then lets go.
    fn sort_by(&mut self, column: usize) {
        self.sort = match self.sort {
            Some((current, -1)) if current == column => Some((column, 1)),
            Some((current, _)) if current == column => None,
            _ => Some((column, -1)),
        };
        // `ItemRowComparator`: by the column's count, then the item's id; a
        // row with no count for the column comes below any that has one.
        if let Some((column, order)) = self.sort {
            self.items.sort_by(|a, b| {
                let (va, vb) = (
                    a.values[column].unwrap_or(-1),
                    b.values[column].unwrap_or(-1),
                );
                let by = if va == vb {
                    a.id.cmp(&b.id)
                } else {
                    va.cmp(&vb)
                };
                if order < 0 { by.reverse() } else { by }
            });
        }
    }
}

/// `Mth.roundToward`: `value` up to the next multiple of `factor`.
fn round_toward(value: i32, factor: i32) -> i32 {
    (value + factor - 1).div_euclid(factor) * factor
}

/// The tab bar's three tabs: where the first starts and how wide each is,
/// in GUI pixels (`TabNavigationBar.arrangeElements`).
fn tab_bar(width: i32) -> (i32, i32) {
    let tab_w = round_toward(
        (width.min(TAB_BAR_MAX_W as i32) - TAB_BAR_MARGIN as i32) / 3,
        2,
    );
    (round_toward((width - tab_w * 3) / 2, 2), tab_w)
}

/// Draws the screen and takes this frame's input.
#[allow(clippy::too_many_arguments)]
pub fn build(
    elements: &mut Vec<MenuElement>,
    ui: &mut StatsUi,
    screen_w: f32,
    screen_h: f32,
    cursor: (f32, f32),
    (pressed, held): (bool, bool),
    wheel: f32,
    gs: f32,
    text_width: &dyn Fn(&str, f32) -> f32,
) -> StatsResult {
    let mut result = StatsResult::default();
    let fs = FONT_SIZE * gs;
    let px = |v: f32| v * gs;
    // The screen in whole GUI pixels, as vanilla lays it out.
    let width = (screen_w / gs).floor();
    let height = (screen_h / gs).floor();
    let text = |elements: &mut Vec<MenuElement>, x: f32, y: f32, s: &str, color: [f32; 4]| {
        elements.push(MenuElement::Text {
            x: px(x),
            y: px(y),
            text: s.to_owned(),
            scale: fs,
            color,
            centered: false,
        });
    };

    // `Screen.extractBackground`, with `StatsScreen.extractMenuBackground`:
    // the world blurred, the tab header's own background over the header,
    // and the menu's dimming under the rest.
    elements.push(MenuElement::BlurBackground);
    super::common::push_menu_background(elements, screen_w, screen_h);
    elements.push(MenuElement::TiledImage {
        x: 0.0,
        y: 0.0,
        w: screen_w,
        h: px(TAB_BAR_H),
        sprite: SpriteId::TabHeaderBackground,
        tile_size: px(16.0),
        tint: WHITE,
    });

    // The tab bar. A tab whose list is empty cannot be opened.
    let (bar_x, tab_w) = tab_bar(width as i32);
    let (bar_x, tab_w) = (bar_x as f32, tab_w as f32);
    let labels = [
        ("stat.generalButton", "General"),
        ("stat.itemsButton", "Items"),
        ("stat.mobsButton", "Mobs"),
    ];
    let mut tooltip: Option<((f32, f32), String)> = None;
    for (index, (key, fallback)) in labels.into_iter().enumerate() {
        let label = crate::lang::translate(key).unwrap_or(fallback);
        let active = index == 0 || (!ui.loading && ui.row_count(index) > 0);
        let selected = ui.tab == index;
        let x = bar_x + tab_w * index as f32;
        let hovered = hit_test(cursor, [px(x), 0.0, px(tab_w), px(TAB_BAR_H)]);
        let sprite = match (selected, hovered) {
            (true, true) => SpriteId::TabSelectedHighlighted,
            (true, false) => SpriteId::TabSelected,
            (false, true) => SpriteId::TabHighlighted,
            (false, false) => SpriteId::Tab,
        };
        elements.push(MenuElement::NineSlice {
            x: px(x),
            y: 0.0,
            w: px(tab_w),
            h: px(TAB_BAR_H),
            sprite,
            border: px(2.0),
            tint: WHITE,
        });
        // `TabButton`: the selected tab opens onto the page, and its label
        // is underlined; the others' sit three pixels lower.
        let label_top = if selected {
            elements.push(MenuElement::TiledImage {
                x: px(x + 2.0),
                y: px(2.0),
                w: px(tab_w - 4.0),
                h: px(TAB_BAR_H - 2.0),
                sprite: SpriteId::MenuBackground,
                tile_size: px(32.0),
                tint: WHITE,
            });
            0.0
        } else {
            3.0
        };
        elements.push(MenuElement::Text {
            x: px(x + tab_w / 2.0),
            y: px(label_top + (TAB_BAR_H - label_top - FONT_SIZE) / 2.0),
            text: label.to_owned(),
            scale: fs,
            color: if active { WHITE } else { INACTIVE },
            centered: true,
        });
        if selected {
            let under = text_width(label, fs).min(px(tab_w - 4.0));
            elements.push(MenuElement::Rect {
                x: px(x + tab_w / 2.0) - under / 2.0,
                y: px(TAB_BAR_H - 2.0),
                w: under,
                h: gs,
                corner_radius: 0.0,
                color: if active { WHITE } else { INACTIVE },
            });
        }
        if hovered && !active && !ui.loading {
            let none = crate::lang::translate("gui.stats.none_found");
            tooltip = Some((cursor, none.unwrap_or("No statistics found.").to_owned()));
        }
        if hovered && pressed && active && !selected {
            ui.tab = index;
            ui.dragging = None;
            result.clicked = true;
        }
    }
    // `TabNavigationBar.extractRenderState`: the header's separator runs
    // either side of the tabs.
    let bar_right = bar_x + tab_w * 3.0;
    for (x, w) in [(0.0, px(bar_x)), (px(bar_right), screen_w - px(bar_right))] {
        elements.push(MenuElement::Image {
            x,
            y: px(TAB_BAR_H - 2.0),
            w,
            h: px(2.0),
            sprite: SpriteId::HeaderSeparator,
            tint: WHITE,
        });
    }

    let list_top = TAB_BAR_H;
    let list_h = height - TAB_BAR_H - FOOTER_H;
    let over_list = hit_test(cursor, [0.0, px(list_top), screen_w, px(list_h)]);
    if ui.loading {
        // `LoadingTab`: the wait, with `LoadingDotsText` under it.
        let middle = list_top + (list_h / 2.0).floor();
        let waiting = crate::lang::translate("multiplayer.downloadingStats")
            .unwrap_or("Retrieving statistics...");
        elements.push(MenuElement::Text {
            x: screen_w / 2.0,
            y: px(middle - 9.0),
            text: waiting.to_owned(),
            scale: fs,
            color: WHITE,
            centered: true,
        });
        let dots = match (ui.opened.elapsed().as_millis() / 300) % 4 {
            0 => "O o o",
            2 => "o o O",
            _ => "o O o",
        };
        elements.push(MenuElement::Text {
            x: screen_w / 2.0,
            y: px(middle + 9.0),
            text: dots.to_owned(),
            scale: fs,
            color: DARK_GREY,
            centered: true,
        });
    } else {
        let tab = ui.tab;
        let row_h = ROW_H[tab];
        let rows = ui.row_count(tab);
        // `AbstractSelectionList.contentHeight`, and the scroll's reach.
        let content_h = rows as f32 * row_h + 4.0;
        let most = (content_h - list_h).max(0.0);
        // `AbstractScrollArea`: the scroller, and the bar it runs in.
        let thumb_h = (list_h * list_h / content_h)
            .floor()
            .clamp(32.0, (list_h - 8.0).max(32.0));
        let row_left = (width / 2.0).floor() - ROW_W / 2.0;
        let bar_left = row_left + ROW_W + 6.0 + 2.0;
        if most > 0.0 {
            if over_list && wheel != 0.0 {
                ui.scroll[tab] -= wheel * row_h / 2.0;
            }
            let over_bar = hit_test(cursor, [px(bar_left), px(list_top), px(6.0), px(list_h)]);
            if pressed && over_bar {
                ui.dragging = Some(cursor.1);
            }
            if !held {
                ui.dragging = None;
            }
            if let Some(last) = ui.dragging {
                // `AbstractScrollArea.mouseDragged`: the scroller follows the
                // cursor, the content by as much more as it is longer.
                let per_pixel = (most / (list_h - thumb_h)).max(1.0);
                ui.scroll[tab] += (cursor.1 - last) / gs * per_pixel;
                ui.dragging = Some(cursor.1);
            }
        } else {
            ui.dragging = None;
        }
        ui.scroll[tab] = ui.scroll[tab].clamp(0.0, most);
        let scroll = ui.scroll[tab].floor();

        elements.push(MenuElement::ScissorPush {
            x: 0.0,
            y: px(list_top),
            w: screen_w,
            h: px(list_h),
        });
        // An entry's content sits two pixels inside it.
        let content_x = row_left + 2.0;
        let content_right = content_x + ROW_W - 4.0;
        let mut sort_pressed = None;
        for index in 0..rows {
            let top = list_top + 2.0 - scroll + index as f32 * row_h;
            if top + row_h < list_top || top > list_top + list_h {
                continue;
            }
            let content_y = top + 2.0;
            let middle = content_y + ((row_h - 4.0) / 2.0).floor();
            let even = index.is_multiple_of(2);
            let shade = if even { WHITE } else { GREY };
            match tab {
                0 => {
                    let row = &ui.general[index];
                    text(elements, content_x + 2.0, middle - 4.0, &row.name, shade);
                    let w = text_width(&row.value, fs) / gs;
                    text(
                        elements,
                        content_right - w.ceil() - 4.0,
                        middle - 4.0,
                        &row.value,
                        shade,
                    );
                }
                1 if index == 0 => {
                    // `HeaderEntry`: a button for each column, and the sort's
                    // arrow beside the one sorted by.
                    for (column, (_, icon, key, fallback)) in COLUMNS.iter().enumerate() {
                        let x = content_x + 75.0 + 40.0 * column as f32 - 18.0;
                        let rect = [px(x), px(content_y + 1.0), px(18.0), px(18.0)];
                        let hovered = over_list && hit_test(cursor, rect);
                        for sprite in [
                            if hovered {
                                SpriteId::MountSlot
                            } else {
                                SpriteId::StatHeader
                            },
                            *icon,
                        ] {
                            elements.push(MenuElement::Image {
                                x: rect[0],
                                y: rect[1],
                                w: rect[2],
                                h: rect[3],
                                sprite,
                                tint: WHITE,
                            });
                        }
                        if hovered {
                            let name = crate::lang::translate(key).unwrap_or(fallback);
                            tooltip = Some((cursor, name.to_owned()));
                            if pressed {
                                sort_pressed = Some(column);
                            }
                        }
                    }
                    if let Some((column, order)) = ui.sort {
                        elements.push(MenuElement::Image {
                            x: px(content_x + 75.0 + 40.0 * column as f32 - 36.0),
                            y: px(content_y + 1.0),
                            w: px(18.0),
                            h: px(18.0),
                            sprite: if order > 0 {
                                SpriteId::StatSortUp
                            } else {
                                SpriteId::StatSortDown
                            },
                            tint: WHITE,
                        });
                    }
                }
                1 => {
                    // `ItemRow`: the item in a slot, then a count a column.
                    let row = &ui.items[index - 1];
                    let slot = [px(content_x), px(content_y), px(18.0), px(18.0)];
                    elements.push(MenuElement::Image {
                        x: slot[0],
                        y: slot[1],
                        w: slot[2],
                        h: slot[3],
                        sprite: SpriteId::MountSlot,
                        tint: WHITE,
                    });
                    push_item_glyph(
                        elements,
                        [px(content_x + 1.0), px(content_y + 1.0), px(16.0), px(16.0)],
                        row.resource.clone(),
                        None,
                        false,
                    );
                    if over_list && hit_test(cursor, slot) {
                        tooltip = Some((
                            (px(content_x + 18.0), px(content_y + 18.0)),
                            row.display.clone(),
                        ));
                    }
                    for (column, value) in row.values.iter().enumerate() {
                        let shown = match value {
                            Some(value) => grouped(*value),
                            None => crate::lang::translate("stats.none")
                                .unwrap_or("-")
                                .to_owned(),
                        };
                        let w = (text_width(&shown, fs) / gs).ceil();
                        text(
                            elements,
                            content_x + 75.0 + 40.0 * column as f32 - w,
                            middle - 4.0,
                            &shown,
                            shade,
                        );
                    }
                }
                _ => {
                    // `MobRow`: the mob, then what it has and has not done.
                    let row = &ui.mobs[index];
                    let line = |has: bool| if has { GREY } else { DARK_GREY };
                    text(elements, content_x + 2.0, content_y + 1.0, &row.name, WHITE);
                    text(
                        elements,
                        content_x + 12.0,
                        content_y + 10.0,
                        &row.kills,
                        line(row.has_kills),
                    );
                    text(
                        elements,
                        content_x + 12.0,
                        content_y + 19.0,
                        &row.killed_by,
                        line(row.was_killed_by),
                    );
                }
            }
        }
        elements.push(MenuElement::ScissorPop);
        if let Some(column) = sort_pressed {
            ui.sort_by(column);
            result.clicked = true;
        }
        crate::ui::menu::helpers::push_scrollbar(
            elements,
            px(bar_left),
            px(list_top),
            px(list_h),
            px(content_h),
            px(scroll),
            gs,
            px(32.0),
        );
    }

    // The footer: its separator, and Done.
    elements.push(MenuElement::Image {
        x: 0.0,
        y: px(height - FOOTER_H),
        w: screen_w,
        h: px(2.0),
        sprite: SpriteId::FooterSeparator,
        tint: WHITE,
    });
    let done = crate::lang::translate("gui.done").unwrap_or("Done");
    let done_y = height - FOOTER_H + ((FOOTER_H - BTN_H) / 2.0).floor();
    let over_done = push_button(
        elements,
        cursor,
        px((width / 2.0).floor() - 100.0),
        px(done_y),
        px(200.0),
        px(BTN_H),
        gs,
        fs,
        done,
        true,
    );
    if over_done && pressed {
        result.clicked = true;
        result.done = true;
    }

    if let Some((at, line)) = tooltip {
        push_tooltip_lines(
            elements,
            at,
            screen_w,
            screen_h,
            gs,
            vec![TooltipLine::new(line, WHITE)],
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_are_grouped_by_thousands() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1,000");
        assert_eq!(grouped(1234567), "1,234,567");
        assert_eq!(grouped(-12345), "-12,345");
    }

    /// `StatFormatter.TIME` picks the largest unit past a half, and leaves
    /// seconds as Java prints a double.
    #[test]
    fn times_pick_their_unit() {
        assert_eq!(format_custom("play_time", 100), "5.0 s");
        assert_eq!(format_custom("play_time", 1), "0.05 s");
        assert_eq!(format_custom("play_time", 20 * 60), "1.00 min");
        assert_eq!(format_custom("sneak_time", 20 * 60 * 90), "1.50 h");
        assert_eq!(
            format_custom("total_world_time", 20 * 60 * 60 * 36),
            "1.50 d"
        );
    }

    /// `StatFormatter.DISTANCE` and `DIVIDE_BY_TEN`.
    #[test]
    fn distances_and_tenths() {
        assert_eq!(format_custom("walk_one_cm", 40), "40 cm");
        assert_eq!(format_custom("walk_one_cm", 250), "2.50 m");
        assert_eq!(format_custom("fly_one_cm", 123_400), "1.23 km");
        assert_eq!(format_custom("damage_dealt", 125), "12.50");
        assert_eq!(format_custom("jump", 12_000), "12,000");
    }

    /// A wall-mounted block is its standing form's item, a crop its seed's,
    /// and a block with no item of its own none.
    #[test]
    fn blocks_name_their_items() {
        let items = [
            "torch",
            "oak_sign",
            "wheat_seeds",
            "stone",
            "cobblestone_wall",
        ];
        let is_item = |name: &str| items.contains(&name);
        let item = |block: &str| item_of_block(block, &is_item);
        assert_eq!(item("stone").as_deref(), Some("stone"));
        assert_eq!(item("wall_torch").as_deref(), Some("torch"));
        assert_eq!(item("oak_wall_sign").as_deref(), Some("oak_sign"));
        assert_eq!(item("wheat").as_deref(), Some("wheat_seeds"));
        assert_eq!(
            item("cobblestone_wall").as_deref(),
            Some("cobblestone_wall")
        );
        assert_eq!(item("water"), None);
        assert_eq!(block_of_item("wheat_seeds"), "wheat");
        assert_eq!(block_of_item("stone"), "stone");
    }

    /// `ItemStatisticsList.sortByColumn`: down, up, then unsorted, with
    /// ties by the item's id.
    #[test]
    fn a_column_sorts_down_then_up_then_lets_go() {
        let row = |id: u32, used: i32| ItemRow {
            id,
            resource: String::new(),
            display: String::new(),
            values: [None, Some(0), Some(0), Some(used), Some(0), Some(0)],
        };
        let mut ui = StatsUi::new(false);
        ui.items = vec![row(1, 5), row(2, 9), row(3, 5)];
        let ids = |ui: &StatsUi| ui.items.iter().map(|row| row.id).collect::<Vec<_>>();
        ui.sort_by(3);
        assert_eq!(ui.sort, Some((3, -1)));
        assert_eq!(ids(&ui), [2, 3, 1]);
        ui.sort_by(3);
        assert_eq!(ui.sort, Some((3, 1)));
        assert_eq!(ids(&ui), [1, 3, 2]);
        ui.sort_by(3);
        assert_eq!(ui.sort, None);
        assert_eq!(ids(&ui), [1, 3, 2]);
        // Another column starts over, going down.
        ui.sort_by(0);
        assert_eq!(ui.sort, Some((0, -1)));
    }

    /// `TabNavigationBar.arrangeElements` on a 427-wide and a narrow screen.
    #[test]
    fn the_tabs_share_the_bar() {
        assert_eq!(tab_bar(427), (28, 124));
        assert_eq!(tab_bar(320), (14, 98));
    }

    #[test]
    fn stats_take_the_last_value_sent() {
        let mut stats = StatsCounter::default();
        stats.apply([(StatType::Custom, 3, 7), (StatType::Used, 3, 1)]);
        stats.apply([(StatType::Custom, 3, 9)]);
        assert_eq!(stats.get(StatType::Custom, 3), 9);
        assert_eq!(stats.get(StatType::Used, 3), 1);
        assert_eq!(stats.get(StatType::Mined, 3), 0);
    }
}
