//! The map packet, as vanilla 26.2 lays it out
//! (`ClientboundMapItemDataPacket`). Text components stay as NBT.

use crate::codec::{DecodeResult, Reader};

/// A map is 128 pixels a side (`MapItemSavedData.MAP_SIZE`).
pub const MAP_SIZE: usize = 128;

#[derive(Clone, Debug, PartialEq)]
pub struct MapData {
    /// The `map_id` component's id.
    pub id: i32,
    pub scale: i8,
    pub locked: bool,
    /// The map's whole marker list when present; absent leaves the markers
    /// as they are.
    pub decorations: Option<Vec<Decoration>>,
    pub patch: Option<Patch>,
}

/// `MapDecoration`: a marker on the map.
#[derive(Clone, Debug, PartialEq)]
pub struct Decoration {
    /// A `map_decoration_type` registry id.
    pub kind: u32,
    /// Pixels from the map's centre, two to a map pixel.
    pub x: i8,
    pub y: i8,
    /// Sixteenths of a turn.
    pub rot: i8,
    /// A text component as network NBT.
    pub name: Option<Box<[u8]>>,
}

/// `MapItemSavedData.MapPatch`: a rectangle of colours, row by row.
#[derive(Clone, Debug, PartialEq)]
pub struct Patch {
    pub start_x: u8,
    pub start_y: u8,
    pub width: u8,
    pub height: u8,
    pub colors: Box<[u8]>,
}

/// Decodes packet `name`, or `None` if it isn't the map packet.
pub fn read(name: &str, r: &mut Reader) -> Option<DecodeResult<MapData>> {
    (name == "map_item_data").then(|| map_data(r))
}

// ClientboundMapItemDataPacket.java:23-34
fn map_data(r: &mut Reader) -> DecodeResult<MapData> {
    Ok(MapData {
        id: r.var_i32()?,
        scale: r.i8()?,
        locked: r.bool()?,
        decorations: r.optional(|r| r.list(usize::MAX, decoration))?,
        patch: patch(r)?,
    })
}

// MapDecoration.java:15-25
fn decoration(r: &mut Reader) -> DecodeResult<Decoration> {
    Ok(Decoration {
        kind: r.varint()?,
        x: r.i8()?,
        y: r.i8()?,
        // Vanilla keeps the low four bits.
        rot: r.i8()? & 15,
        name: r.optional(|r| Ok(r.nbt()?.into()))?,
    })
}

// MapItemSavedData.MapPatch.read: a zero width is no patch.
fn patch(r: &mut Reader) -> DecodeResult<Option<Patch>> {
    let width = r.u8()?;
    if width == 0 {
        return Ok(None);
    }
    let height = r.u8()?;
    let start_x = r.u8()?;
    let start_y = r.u8()?;
    let colors = r.byte_array(MAP_SIZE * MAP_SIZE)?;
    if colors.len() != usize::from(width) * usize::from(height) {
        return Err(r.error("map patch size and colours disagree"));
    }
    Ok(Some(Patch {
        start_x,
        start_y,
        width,
        height,
        colors: colors.into(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_markers_and_a_patch() {
        let bytes = [
            7, // id
            2, // scale
            1, // locked
            1, // markers present
            2, // two of them
            26, 0x10, 0xF0, 3, 0, // red_x, no name
            0, 0, 0, 0x1F, 0, // player, rotation's high bits dropped
            2, 1, 5, 6, // 2x1 patch at 5,6
            2, 34, 35, // two colours
        ];
        let mut r = Reader::new(&bytes);
        let map = read("map_item_data", &mut r).unwrap().unwrap();
        r.ensure_eof().unwrap();
        assert_eq!((map.id, map.scale, map.locked), (7, 2, true));
        let markers = map.decorations.unwrap();
        assert_eq!((markers[0].kind, markers[0].x, markers[0].y), (26, 16, -16));
        assert_eq!(markers[1].rot, 15);
        let patch = map.patch.unwrap();
        assert_eq!((patch.start_x, patch.start_y), (5, 6));
        assert_eq!(&patch.colors[..], [34, 35]);
    }

    #[test]
    fn a_zero_width_is_no_patch() {
        let bytes = [0, 0, 0, 0, 0];
        let mut r = Reader::new(&bytes);
        let map = read("map_item_data", &mut r).unwrap().unwrap();
        r.ensure_eof().unwrap();
        assert_eq!(map.decorations, None);
        assert_eq!(map.patch, None);
    }

    #[test]
    fn other_packets_are_not_claimed() {
        assert!(read("block_update", &mut Reader::new(&[])).is_none());
    }
}
