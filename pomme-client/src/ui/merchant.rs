//! Vanilla `MerchantScreen`: a villager's or a wandering trader's trades
//! down the left (seven showing, a scroller for more), the two payment
//! slots and the result on the right, and a villager's level and progress
//! over them. Picking a trade tells the server (`ServerboundSelectTrade`),
//! which moves the payment in.

use std::time::Instant;

use azalea_inventory::item::MaxStackSizeExt;
use azalea_inventory::{ItemStack, ItemStackData};

use super::common::{
    FONT_SIZE, hit_test, push_button, push_item_count, push_item_icon, push_tooltip_lines,
};
use super::container::{
    ContainerInput, ContainerResult, DragState, Panel, SlotCtx, push_backdrop, push_clipped_sprite,
    push_cursor_stack, resolve_gesture,
};
use crate::player::menu_click::ContainerKind;
use crate::renderer::pipelines::menu_overlay::{MenuElement, SpriteId, TooltipLine};

/// The trades on screen at once.
const SHOWN: usize = 7;
/// Vanilla `VillagerData.NEXT_LEVEL_XP_THRESHOLDS`.
const LEVEL_XP: [i32; 5] = [0, 10, 70, 150, 250];
/// `merchant.level.1` to `.5`.
const LEVELS: [&str; 5] = ["Novice", "Apprentice", "Journeyman", "Expert", "Master"];

/// One trade (vanilla `MerchantOffer`), as the client is told of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    /// The first cost before demand and discounts.
    pub base_cost_a: ItemStack,
    pub result: ItemStack,
    /// The second cost; empty where there is none.
    pub cost_b: ItemStack,
    pub out_of_stock: bool,
    pub xp: i32,
    pub special_price_diff: i32,
    pub price_multiplier: f32,
    pub demand: i32,
}

impl Offer {
    /// Vanilla `MerchantOffer.getCostA`: the first cost as it stands, up
    /// with demand and by the special price, from one to a stack.
    pub fn cost_a(&self) -> ItemStack {
        let ItemStack::Present(base) = &self.base_cost_a else {
            return ItemStack::Empty;
        };
        let rise =
            (((base.count * self.demand) as f32 * self.price_multiplier).floor() as i32).max(0);
        let mut cost = base.clone();
        cost.count = (base.count + rise + self.special_price_diff)
            .clamp(1, base.kind.max_stack_size().max(1));
        ItemStack::Present(cost)
    }
}

/// The open merchant menu's trades, and where the list stands.
#[derive(Default)]
pub struct MerchantState {
    pub offers: Vec<Offer>,
    pub level: u32,
    pub xp: u32,
    pub show_progress: bool,
    pub can_restock: bool,
    /// The trade picked (vanilla `shopItem`).
    shop_item: usize,
    /// The first trade on screen (`scrollOff`).
    scroll_off: usize,
    dragging: bool,
}

impl MerchantState {
    /// `ClientPacketListener.handleMerchantOffers`.
    pub fn set_offers(
        &mut self,
        offers: Vec<Offer>,
        level: u32,
        xp: u32,
        show_progress: bool,
        can_restock: bool,
    ) {
        self.offers = offers;
        self.level = level;
        self.xp = xp;
        self.show_progress = show_progress;
        self.can_restock = can_restock;
    }

    /// The trade the result slot's stack comes of: the picked one where it
    /// makes that, else the first that does.
    fn active_offer(&self, result: &ItemStack) -> Option<&Offer> {
        let ItemStack::Present(result) = result else {
            return None;
        };
        let makes = |offer: &&Offer| matches!(&offer.result, ItemStack::Present(made) if made.kind == result.kind);
        self.offers
            .get(self.shop_item)
            .filter(makes)
            .or_else(|| self.offers.iter().find(makes))
    }
}

/// Where the scroller sits, in pixels down its 139 pixel track, with
/// `count` trades scrolled to `scroll_off` (`extractScroller`); `None`
/// where they all fit.
fn scroller_offset(count: usize, scroll_off: usize) -> Option<i32> {
    let steps = count as i32 + 1 - SHOWN as i32;
    if steps <= 1 {
        return None;
    }
    let spare = 139 - (27 + (steps - 1) * 139 / steps);
    let step = 1 + spare / steps + 139 / steps;
    Some(if scroll_off as i32 == steps - 1 {
        113
    } else {
        (scroll_off as i32 * step).min(113)
    })
}

/// How much of the 102 pixel bar a villager of `level` with `xp` has
/// filled, and how much more `gain` would (`extractProgressBar`); `None`
/// where there is no further level.
fn progress_bar(level: u32, xp: i32, gain: i32) -> Option<(f32, f32)> {
    if !(1..5).contains(&level) {
        return None;
    }
    let (from, to) = (LEVEL_XP[level as usize - 1], LEVEL_XP[level as usize]);
    if xp < from {
        return None;
    }
    let per_xp = 102.0 / (to - from) as f32;
    let filled = (per_xp * (xp - from) as f32).floor().min(102.0);
    let more = if gain > 0 {
        (gain as f32 * per_xp).floor().min(102.0 - filled)
    } else {
        0.0
    };
    Some((filled, more))
}

fn tooltip(elements: &mut Vec<MenuElement>, panel: &Panel, cursor: (f32, f32), stack: &ItemStack) {
    // Nothing there (a trade with one cost), nothing to say.
    if stack.is_present()
        && let Some(value) = crate::ui::chat::stack_tooltip_value(stack)
    {
        let lines = crate::ui::chat::item_tooltip_lines(&value, None, false);
        if !lines.is_empty() {
            push_tooltip_lines(
                elements,
                cursor,
                panel.screen.0,
                panel.screen.1,
                panel.gs,
                lines,
            );
        }
    }
}

/// Builds the screen; the second value is a trade picked this frame.
#[allow(clippy::too_many_arguments)]
pub fn build(
    elements: &mut Vec<MenuElement>,
    screen_w: f32,
    screen_h: f32,
    cursor: (f32, f32),
    input: &ContainerInput,
    slots: &[ItemStack],
    title: &str,
    state: &mut MerchantState,
    wheel: f32,
    cursor_item: &ItemStack,
    drag: &mut Option<DragState>,
    last_click: &mut Option<(u16, Instant)>,
    gs: f32,
    text_width: &dyn Fn(&str, f32) -> f32,
) -> (ContainerResult, Option<u32>) {
    let panel = push_backdrop(elements, screen_w, screen_h, gs, 276.0, 166.0);
    let scale = panel.scale;
    panel.image(
        elements,
        SpriteId::MerchantBackground,
        0.0,
        0.0,
        276.0,
        166.0,
    );

    // `extractLabels`: the title, with a villager's level, centred over the
    // right part; "Trades" over the left.
    let half_width = |text: &str| (text_width(text, FONT_SIZE * scale) / scale / 2.0).floor();
    let heading = if (1..=5).contains(&state.level) && state.show_progress {
        format!("{title} - {}", LEVELS[state.level as usize - 1])
    } else {
        title.to_owned()
    };
    panel.label(elements, 49.0 + 138.0 - half_width(&heading), 6.0, &heading);
    panel.label(elements, 107.0, 72.0, "Inventory");
    panel.label(elements, 5.0 + 48.0 - half_width("Trades"), 6.0, "Trades");

    let count = state.offers.len();
    let can_scroll = count > SHOWN;
    let mouse = ((cursor.0 - panel.ox) / scale, (cursor.1 - panel.oy) / scale);
    if can_scroll {
        let steps = (count - SHOWN) as i32;
        if input.left_pressed
            && mouse.0 > 94.0
            && mouse.0 < 100.0
            && mouse.1 > 18.0
            && mouse.1 <= 18.0 + 139.0 + 1.0
        {
            state.dragging = true;
        }
        if state.dragging {
            let along = (mouse.1 - 18.0 - 13.5) / (139.0 - 27.0);
            state.scroll_off = ((along * steps as f32 + 0.5) as i32).clamp(0, steps) as usize;
        } else if wheel != 0.0 {
            state.scroll_off =
                ((state.scroll_off as f64 - f64::from(wheel)) as i32).clamp(0, steps) as usize;
        }
    } else {
        state.scroll_off = 0;
    }
    if !input.left_held {
        state.dragging = false;
    }
    state.shop_item = state.shop_item.min(count.saturating_sub(1));

    if state
        .offers
        .get(state.shop_item)
        .is_some_and(|offer| offer.out_of_stock)
    {
        panel.image(
            elements,
            SpriteId::MerchantOutOfStock,
            182.0,
            35.0,
            28.0,
            21.0,
        );
    }

    // The trades' buttons, then what each trades.
    let mut picked = None;
    let mut hovered_row = None;
    for row in 0..SHOWN.min(count) {
        let y = 18.0 + row as f32 * 20.0;
        let over = push_button(
            elements,
            cursor,
            panel.ox + 5.0 * scale,
            panel.oy + y * scale,
            88.0 * scale,
            20.0 * scale,
            scale,
            FONT_SIZE * scale,
            "",
            true,
        );
        if over {
            hovered_row = Some(row);
            if input.left_pressed {
                state.shop_item = row + state.scroll_off;
                picked = Some(state.shop_item as u32);
            }
        }
    }
    if count > 0 {
        match scroller_offset(count, state.scroll_off) {
            Some(offset) => panel.image(
                elements,
                SpriteId::MerchantScroller,
                94.0,
                18.0 + offset as f32,
                6.0,
                27.0,
            ),
            None => panel.image(
                elements,
                SpriteId::MerchantScrollerDisabled,
                94.0,
                18.0,
                6.0,
                27.0,
            ),
        }
    }
    let icon = |elements: &mut Vec<MenuElement>, x: f32, y: f32, stack: &ItemStackData| {
        push_item_icon(
            elements,
            panel.ox + x * scale,
            panel.oy + y * scale,
            16.0 * scale,
            scale,
            stack,
        );
    };
    for (row, offer) in state
        .offers
        .iter()
        .skip(state.scroll_off)
        .take(SHOWN)
        .enumerate()
    {
        let y = 19.0 + row as f32 * 20.0;
        // `extractAndDecorateCostA`: a changed price shows the old count
        // struck through beside the new.
        if let (ItemStack::Present(base), ItemStack::Present(cost)) =
            (&offer.base_cost_a, &offer.cost_a())
        {
            if base.count == cost.count {
                icon(elements, 10.0, y, cost);
            } else {
                icon(elements, 10.0, y, base);
                push_item_count(
                    elements,
                    panel.ox + 24.0 * scale,
                    panel.oy + y * scale,
                    16.0 * scale,
                    scale,
                    cost.count,
                );
                panel.image(
                    elements,
                    SpriteId::MerchantDiscount,
                    17.0,
                    y + 12.0,
                    9.0,
                    2.0,
                );
            }
        }
        if let ItemStack::Present(cost_b) = &offer.cost_b {
            icon(elements, 40.0, y, cost_b);
        }
        panel.image(
            elements,
            if offer.out_of_stock {
                SpriteId::MerchantArrowOutOfStock
            } else {
                SpriteId::MerchantArrow
            },
            60.0,
            y + 3.0,
            10.0,
            9.0,
        );
        if let ItemStack::Present(result) = &offer.result {
            icon(elements, 73.0, y, result);
        }
    }

    // `extractProgressBar`: a villager's way to its next level, and what
    // the trade in hand would add.
    let result_slot = slots.get(2).unwrap_or(&ItemStack::Empty);
    if count > 0 && state.show_progress && state.level < 5 {
        panel.image(
            elements,
            SpriteId::MerchantXpBackground,
            136.0,
            16.0,
            102.0,
            5.0,
        );
        let gain = state.active_offer(result_slot).map_or(0, |offer| offer.xp);
        if let Some((filled, more)) = progress_bar(state.level, state.xp as i32, gain) {
            let bar = [136.0, 16.0, 102.0, 5.0];
            if filled > 0.0 {
                push_clipped_sprite(
                    elements,
                    &panel,
                    SpriteId::MerchantXpCurrent,
                    bar,
                    [136.0, 16.0, filled, 5.0],
                );
            }
            if more > 0.0 {
                push_clipped_sprite(
                    elements,
                    &panel,
                    SpriteId::MerchantXpResult,
                    bar,
                    [136.0 + filled, 16.0, more, 5.0],
                );
            }
        }
    }

    // The menu's own slots sit in the right part, 100 along.
    let slot_panel = Panel {
        scale,
        ox: panel.ox + 100.0 * scale,
        oy: panel.oy,
        w: 176.0 * scale,
        h: panel.h,
        screen: panel.screen,
        gs: panel.gs,
    };
    let kind = ContainerKind::Merchant;
    let mut ctx = SlotCtx::new(
        elements,
        &slot_panel,
        cursor,
        kind,
        slots,
        cursor_item,
        drag,
    );
    ctx.player_rows(slots, 3, 30, 84.0);
    for (num, x) in [(0u16, 36.0), (1, 62.0), (2, 120.0)] {
        let item = slots.get(usize::from(num)).unwrap_or(&ItemStack::Empty);
        ctx.slot(x, 37.0, item, None, num);
    }
    let (hovered, shown_cursor) = ctx.finish(cursor_item);

    // `TradeOfferButton.extractToolTip`: what is under the cursor on a
    // trade's button; and why a villager's trade is shut.
    if let Some(row) = hovered_row
        && let Some(offer) = state.offers.get(row + state.scroll_off)
    {
        if mouse.0 < 25.0 {
            tooltip(elements, &panel, cursor, &offer.cost_a());
        } else if mouse.0 > 35.0 && mouse.0 < 55.0 {
            tooltip(elements, &panel, cursor, &offer.cost_b);
        } else if mouse.0 > 70.0 {
            tooltip(elements, &panel, cursor, &offer.result);
        }
    }
    if state.can_restock
        && state
            .offers
            .get(state.shop_item)
            .is_some_and(|offer| offer.out_of_stock)
        && hit_test(mouse, [186.0, 35.0, 22.0, 21.0])
    {
        push_tooltip_lines(
            elements,
            cursor,
            panel.screen.0,
            panel.screen.1,
            panel.gs,
            vec![TooltipLine::new(
                "Villagers restock up to two times per day.".into(),
                [1.0; 4],
            )],
        );
    }
    push_cursor_stack(elements, cursor, scale, &shown_cursor);

    let (ops, clicked_outside) = resolve_gesture(
        input,
        hovered,
        &panel,
        cursor,
        kind,
        slots,
        cursor_item,
        drag,
        last_click,
    );
    (
        ContainerResult {
            clicked_outside,
            ops,
            button: None,
            hovered,
        },
        picked,
    )
}

#[cfg(test)]
mod tests {
    use azalea_registry::builtin::ItemKind;

    use super::*;

    fn offer(count: i32, demand: i32, special: i32) -> Offer {
        Offer {
            base_cost_a: ItemStack::Present(ItemStackData::new(ItemKind::Emerald, count)),
            result: ItemStack::Present(ItemStackData::new(ItemKind::Bread, 6)),
            cost_b: ItemStack::Empty,
            out_of_stock: false,
            xp: 2,
            special_price_diff: special,
            price_multiplier: 0.05,
            demand,
        }
    }

    /// A price rises with demand and moves by the special price, but stays
    /// between one and a stack.
    #[test]
    fn a_price_follows_demand_and_discounts() {
        assert_eq!(offer(4, 0, 0).cost_a().count(), 4);
        assert_eq!(offer(20, 10, 0).cost_a().count(), 30);
        assert_eq!(offer(20, -10, 0).cost_a().count(), 20);
        assert_eq!(offer(4, 0, -9).cost_a().count(), 1);
        assert_eq!(offer(60, 0, 30).cost_a().count(), 64);
    }

    /// The scroller steps down its track and ends at the bottom.
    #[test]
    fn the_scroller_reaches_the_bottom_at_the_last_trade() {
        assert_eq!(scroller_offset(7, 0), None);
        assert_eq!(scroller_offset(8, 0), Some(0));
        assert_eq!(scroller_offset(8, 1), Some(113));
        assert_eq!(scroller_offset(9, 2), Some(113));
        assert_eq!(scroller_offset(12, 1), Some(24));
        assert_eq!(scroller_offset(12, 5), Some(113));
    }

    /// The bar fills from a level's first experience to the next level's,
    /// and a master has none.
    #[test]
    fn the_progress_bar_fills_to_the_next_level() {
        assert_eq!(progress_bar(1, 0, 0), Some((0.0, 0.0)));
        assert_eq!(progress_bar(1, 5, 2), Some((51.0, 20.0)));
        assert_eq!(progress_bar(2, 40, 0), Some((51.0, 0.0)));
        assert_eq!(progress_bar(2, 69, 50), Some((100.0, 2.0)));
        assert_eq!(progress_bar(5, 300, 0), None);
        assert_eq!(progress_bar(2, 3, 0), None);
    }
}
