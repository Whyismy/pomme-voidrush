//! Container, inventory and equipment packets, as vanilla 26.2 lays them out
//! (`net/minecraft/network/protocol/game/Clientbound*Packet`). Stacks use the
//! native item grammar; `None` is an empty stack.

use crate::codec::{DecodeResult, Reader};
use crate::item::{ComponentPatch, ItemCodec, ItemStack};

#[derive(Clone, Debug, PartialEq)]
pub enum ContainerPacket {
    OpenScreen {
        container_id: i32,
        /// A `menu` registry id.
        menu: u32,
        /// The title, a text component as NBT.
        title: Box<[u8]>,
    },
    Content {
        container_id: i32,
        state_id: u32,
        items: Vec<Option<ItemStack>>,
        carried: Option<ItemStack>,
    },
    Slot {
        container_id: i32,
        state_id: u32,
        slot: i16,
        item: Option<ItemStack>,
    },
    Data {
        container_id: i32,
        id: i16,
        value: i16,
    },
    Close {
        container_id: i32,
    },
    Cursor {
        item: Option<ItemStack>,
    },
    PlayerInventory {
        slot: u32,
        item: Option<ItemStack>,
    },
    Equipment {
        entity: i32,
        /// `EquipmentSlot` ordinals and their stacks.
        slots: Vec<(u8, Option<ItemStack>)>,
    },
    /// The server has the client read the book in a hand.
    OpenBook {
        off_hand: bool,
    },
    /// The open merchant menu's trades.
    MerchantOffers {
        container_id: i32,
        offers: Vec<MerchantOffer>,
        level: u32,
        xp: u32,
        show_progress: bool,
        can_restock: bool,
    },
}

/// `MerchantOffer.STREAM_CODEC`. A cost is an `ItemCost`: its item, its
/// count and the components it must have exactly, kept here as a stack
/// whose patch adds them.
#[derive(Clone, Debug, PartialEq)]
pub struct MerchantOffer {
    pub base_cost_a: ItemStack,
    pub result: ItemStack,
    pub cost_b: Option<ItemStack>,
    pub out_of_stock: bool,
    pub uses: i32,
    pub max_uses: i32,
    pub xp: i32,
    pub special_price_diff: i32,
    pub price_multiplier: f32,
    pub demand: i32,
}

/// Decodes packet `name`, or `None` if it isn't a container packet.
pub fn read(name: &str, r: &mut Reader) -> Option<DecodeResult<ContainerPacket>> {
    let read: fn(&mut Reader) -> DecodeResult<ContainerPacket> = match name {
        // ClientboundOpenScreenPacket.java:20
        "open_screen" => |r| {
            Ok(ContainerPacket::OpenScreen {
                container_id: r.var_i32()?,
                menu: r.varint()?,
                title: r.nbt()?.into(),
            })
        },
        // ClientboundContainerSetContentPacket.java:18
        "container_set_content" => |r| {
            Ok(ContainerPacket::Content {
                container_id: r.var_i32()?,
                state_id: r.varint()?,
                items: r.list(usize::MAX, stack)?,
                carried: stack(r)?,
            })
        },
        // ClientboundContainerSetSlotPacket.java:29-34
        "container_set_slot" => |r| {
            Ok(ContainerPacket::Slot {
                container_id: r.var_i32()?,
                state_id: r.varint()?,
                slot: r.i16()?,
                item: stack(r)?,
            })
        },
        // ClientboundContainerSetDataPacket.java:26-30
        "container_set_data" => |r| {
            Ok(ContainerPacket::Data {
                container_id: r.var_i32()?,
                id: r.i16()?,
                value: r.i16()?,
            })
        },
        // ClientboundContainerClosePacket.java:22-24
        "container_close" => |r| {
            Ok(ContainerPacket::Close {
                container_id: r.var_i32()?,
            })
        },
        // ClientboundSetCursorItemPacket.java:16
        "set_cursor_item" => |r| Ok(ContainerPacket::Cursor { item: stack(r)? }),
        // ClientboundSetPlayerInventoryPacket.java:17
        "set_player_inventory" => |r| {
            Ok(ContainerPacket::PlayerInventory {
                slot: r.varint()?,
                item: stack(r)?,
            })
        },
        // ClientboundSetEquipmentPacket.java:34-44: the high bit marks
        // another entry.
        "set_equipment" => |r| {
            let entity = r.var_i32()?;
            let mut slots = Vec::new();
            loop {
                let byte = r.u8()?;
                slots.push((byte & 0x7F, stack(r)?));
                if byte & 0x80 == 0 {
                    break;
                }
            }
            Ok(ContainerPacket::Equipment { entity, slots })
        },
        // ClientboundOpenBookPacket.java:18: the `InteractionHand`.
        "open_book" => |r| {
            Ok(ContainerPacket::OpenBook {
                off_hand: r.varint()? == 1,
            })
        },
        // ClientboundMerchantOffersPacket.java:22-29
        "merchant_offers" => |r| {
            Ok(ContainerPacket::MerchantOffers {
                container_id: r.var_i32()?,
                offers: r.list(usize::MAX, offer)?,
                level: r.varint()?,
                xp: r.varint()?,
                show_progress: r.bool()?,
                can_restock: r.bool()?,
            })
        },
        _ => return None,
    };
    Some(read(r))
}

/// `ItemStack.OPTIONAL_STREAM_CODEC`.
fn stack(r: &mut Reader) -> DecodeResult<Option<ItemStack>> {
    ItemCodec::native().read_stack(r)
}

/// `MerchantOffer.createFromStream`.
fn offer(r: &mut Reader) -> DecodeResult<MerchantOffer> {
    let base_cost_a = cost(r)?;
    let Some(result) = stack(r)? else {
        return Err(r.error("a trade for nothing"));
    };
    Ok(MerchantOffer {
        base_cost_a,
        result,
        cost_b: r.optional(cost)?,
        out_of_stock: r.bool()?,
        uses: r.i32()?,
        max_uses: r.i32()?,
        xp: r.i32()?,
        special_price_diff: r.i32()?,
        price_multiplier: r.f32()?,
        demand: r.i32()?,
    })
}

/// `ItemCost.STREAM_CODEC`: the item, the count, then
/// `DataComponentExactPredicate`'s typed components.
fn cost(r: &mut Reader) -> DecodeResult<ItemStack> {
    let codec = ItemCodec::native();
    let item = r.varint()?;
    let count = r.var_i32()?;
    let added = r.list(usize::MAX, |r| {
        let id = r.varint()?;
        Ok((id, Box::<[u8]>::from(codec.read_component(r, id)?)))
    })?;
    Ok(ItemStack {
        item,
        count,
        patch: ComponentPatch {
            added,
            removed: Vec::new(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ClientRegistry;
    use crate::item::tests::id;
    use crate::wire::write_varint;

    fn decode(name: &str, bytes: &[u8]) -> ContainerPacket {
        let mut r = Reader::new(bytes);
        let packet = read(name, &mut r).expect("a container packet").unwrap();
        r.ensure_eof().unwrap();
        packet
    }

    /// A merchant's offers: a cost is its item and count before its
    /// components, a result is a stack, and the second cost is optional.
    #[test]
    fn merchant_offers_read_their_costs_and_results() {
        let emerald = id(ClientRegistry::Item, "emerald");
        let bread = id(ClientRegistry::Item, "bread");
        let mut bytes = Vec::new();
        write_varint(&mut bytes, 7);
        write_varint(&mut bytes, 1);
        // The cost: three emeralds, no components.
        write_varint(&mut bytes, emerald);
        write_varint(&mut bytes, 3);
        write_varint(&mut bytes, 0);
        // The result: six bread, an empty patch.
        write_varint(&mut bytes, 6);
        write_varint(&mut bytes, bread);
        write_varint(&mut bytes, 0);
        write_varint(&mut bytes, 0);
        // No second cost; in stock; uses, max uses, xp, special price.
        bytes.extend([0, 0]);
        for value in [2i32, 16, 5, -1] {
            bytes.extend(value.to_be_bytes());
        }
        bytes.extend(0.05f32.to_be_bytes());
        bytes.extend(4i32.to_be_bytes());
        // Level, xp, the progress bar, restocking.
        write_varint(&mut bytes, 2);
        write_varint(&mut bytes, 30);
        bytes.extend([1, 0]);

        let ContainerPacket::MerchantOffers {
            container_id,
            offers,
            level,
            xp,
            show_progress,
            can_restock,
        } = decode("merchant_offers", &bytes)
        else {
            panic!("not the offers");
        };
        assert_eq!((container_id, level, xp), (7, 2, 30));
        assert!(show_progress && !can_restock);
        let offer = &offers[0];
        assert_eq!(
            (offer.base_cost_a.item, offer.base_cost_a.count),
            (emerald, 3)
        );
        assert_eq!((offer.result.item, offer.result.count), (bread, 6));
        assert_eq!(offer.cost_b, None);
        assert_eq!((offer.uses, offer.max_uses, offer.xp), (2, 16, 5));
        assert_eq!((offer.special_price_diff, offer.demand), (-1, 4));
        assert_eq!(offer.price_multiplier, 0.05);
    }

    /// `count` of `item` with an empty patch.
    fn stack_bytes(out: &mut Vec<u8>, item: &str, count: u32) {
        write_varint(out, count);
        write_varint(out, id(ClientRegistry::Item, item));
        out.extend([0, 0]);
    }

    fn stack_of(item: &str, count: i32) -> Option<ItemStack> {
        Some(ItemStack {
            item: id(ClientRegistry::Item, item),
            count,
            patch: Default::default(),
        })
    }

    #[test]
    fn other_packets_are_not_read() {
        assert!(read("block_update", &mut Reader::new(&[])).is_none());
    }

    #[test]
    fn content_with_an_empty_slot_and_a_carried_stack() {
        let mut bytes = vec![2, 7, 2];
        stack_bytes(&mut bytes, "diamond", 3);
        bytes.push(0);
        stack_bytes(&mut bytes, "stone", 1);
        assert_eq!(
            decode("container_set_content", &bytes),
            ContainerPacket::Content {
                container_id: 2,
                state_id: 7,
                items: vec![stack_of("diamond", 3), None],
                carried: stack_of("stone", 1),
            }
        );
    }

    #[test]
    fn slot_cursor_and_player_inventory() {
        let mut slot = vec![0, 4];
        slot.extend((-1i16).to_be_bytes());
        slot.push(0);
        assert_eq!(
            decode("container_set_slot", &slot),
            ContainerPacket::Slot {
                container_id: 0,
                state_id: 4,
                slot: -1,
                item: None,
            }
        );
        let mut cursor = Vec::new();
        stack_bytes(&mut cursor, "stone", 64);
        assert_eq!(
            decode("set_cursor_item", &cursor),
            ContainerPacket::Cursor {
                item: stack_of("stone", 64)
            }
        );
        let mut inventory = vec![40];
        stack_bytes(&mut inventory, "shield", 1);
        assert_eq!(
            decode("set_player_inventory", &inventory),
            ContainerPacket::PlayerInventory {
                slot: 40,
                item: stack_of("shield", 1)
            }
        );
    }

    #[test]
    fn equipment_list_continues_on_the_high_bit() {
        // Main hand (0) with a stone sword, then head (5) empty.
        let mut bytes = vec![9, 0x80];
        stack_bytes(&mut bytes, "stone_sword", 1);
        bytes.extend([5, 0]);
        assert_eq!(
            decode("set_equipment", &bytes),
            ContainerPacket::Equipment {
                entity: 9,
                slots: vec![(0, stack_of("stone_sword", 1)), (5, None)],
            }
        );
    }

    #[test]
    fn screen_data_and_close() {
        let title = [8, 0, 5, b'C', b'h', b'e', b's', b't'];
        let bytes = [&[3, 2][..], &title].concat();
        assert_eq!(
            decode("open_screen", &bytes),
            ContainerPacket::OpenScreen {
                container_id: 3,
                menu: 2,
                title: title.into(),
            }
        );
        let data = [&[3][..], &1i16.to_be_bytes(), &(-200i16).to_be_bytes()].concat();
        assert_eq!(
            decode("container_set_data", &data),
            ContainerPacket::Data {
                container_id: 3,
                id: 1,
                value: -200
            }
        );
        assert_eq!(
            decode("container_close", &[3]),
            ContainerPacket::Close { container_id: 3 }
        );
    }
}
