//! The map packet, decoded by pomme for every session whose wire carries
//! the native layout (1.20.3 on; older ones drop it in translation).

use crossbeam_channel::Sender;
use pomme_protocol::codec::DecodeResult;
use pomme_protocol::map;

use super::NetworkEvent;
use super::handler::send_ordered;

/// Decodes the map packet; `None` for any other packet.
pub(super) fn handle_raw_map_packet(
    packet_id: u32,
    payload: &[u8],
    event_tx: &Sender<NetworkEvent>,
) -> Option<DecodeResult<()>> {
    Some(
        super::read_raw_packet(packet_id, payload, map::read)?
            .map(|data| send_ordered(event_tx, NetworkEvent::MapData(data))),
    )
}
