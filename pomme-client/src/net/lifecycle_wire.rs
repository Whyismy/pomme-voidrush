//! Connection lifecycle packets (join, respawn, keep-alive, teleports, time),
//! decoded by pomme for every session: translated frames already carry the
//! native layout, and their registry ids index the session's own synced
//! registries.

use azalea_core::registry_holder::RegistryHolder;
use azalea_core::registry_holder::dimension_type::DimensionKindElement;
use azalea_protocol::packets::game::ServerboundGamePacket;
use azalea_protocol::packets::game::s_keep_alive::ServerboundKeepAlive;
use azalea_registry::identifier::Identifier;
use crossbeam_channel::Sender;
use pomme_protocol::codec::{DecodeResult, Reader};
use pomme_protocol::lifecycle::{self, LifecyclePacket, Login, SpawnInfo};
use pomme_protocol::{Direction, PacketTable, Phase};

use super::chat_security::ProfileKeyServices;
use super::handler::send_ordered;
use super::sender::PacketSender;
use super::{DimensionInfo, NetworkEvent};
use crate::world::block::model::CardinalLightType;
use crate::world::border::BorderUpdate;

/// Decodes a lifecycle packet other than `login`, which the game loop reads
/// itself; `None` for any other packet.
pub(super) fn handle_raw_lifecycle_packet(
    packet_id: u32,
    payload: &[u8],
    registry_holder: &RegistryHolder,
    sender: &PacketSender,
    event_tx: &Sender<NetworkEvent>,
) -> Option<DecodeResult<()>> {
    let read: fn(&str, &mut Reader) -> Option<DecodeResult<LifecyclePacket>> = |name, r| match name
    {
        "login" => None,
        _ => lifecycle::read(name, r),
    };
    Some(
        super::read_raw_packet(packet_id, payload, read)?
            .map(|packet| handle(packet, registry_holder, sender, event_tx)),
    )
}

/// A native `login` frame, for the game loop; `None` for any other packet.
pub(super) fn read_login(raw: &[u8]) -> Option<DecodeResult<Login>> {
    Some(read_native(raw, "login")?.map(|packet| match packet {
        LifecyclePacket::Login(login) => login,
        _ => unreachable!("a login frame decodes as Login"),
    }))
}

/// The reason of a native `disconnect` frame; `None` for any other packet.
pub(super) fn read_disconnect(raw: &[u8]) -> Option<String> {
    match read_native(raw, "disconnect")? {
        Ok(LifecyclePacket::Disconnect(reason)) => Some(disconnect_reason(&reason)),
        _ => None,
    }
}

/// A native game frame of packet `name`; `None` for any other packet.
fn read_native(raw: &[u8], name: &str) -> Option<DecodeResult<LifecyclePacket>> {
    let mut r = Reader::new(raw);
    let id = r.varint().ok()?;
    if PacketTable::native().name_of(Phase::Game, Direction::Clientbound, id)? != name {
        return None;
    }
    super::read_raw_packet(id, &raw[r.pos()..], lifecycle::read)
}

/// Hands the game thread the server's Disconnect `reason`.
pub(super) fn disconnected(event_tx: &Sender<NetworkEvent>, reason: String) {
    tracing::warn!("Disconnected: {reason}");
    send_ordered(event_tx, NetworkEvent::Disconnected { reason });
}

fn disconnect_reason(nbt: &[u8]) -> String {
    super::chat::read_component(nbt, &mut 0)
        .map(|reason| reason.plain_text())
        .unwrap_or_default()
}

/// The events a join sends, `online_mode` already settled by the game loop.
pub(super) fn login_events(
    login: &Login,
    registry_holder: &RegistryHolder,
    event_tx: &Sender<NetworkEvent>,
) {
    spawn_events(&login.spawn, registry_holder, event_tx);
    send_ordered(
        event_tx,
        NetworkEvent::ServerViewDistance {
            distance: login.chunk_radius,
        },
    );
    send_ordered(
        event_tx,
        NetworkEvent::ServerSimulationDistance {
            distance: login.simulation_distance,
        },
    );
    send_ordered(
        event_tx,
        NetworkEvent::PlayerLogin {
            entity_id: login.player_id,
            hardcore: login.hardcore,
            show_death_screen: login.show_death_screen,
            online_mode: login.online_mode,
        },
    );
    send_secure_chat(login.enforces_secure_chat, event_tx);
}

/// Vanilla `ClientPacketListener.enforcesSecureChat`: the server's flag
/// counts only while profile keys can be validated.
pub(super) fn send_secure_chat(server_enforces: bool, event_tx: &Sender<NetworkEvent>) {
    send_ordered(
        event_tx,
        NetworkEvent::SecureChatEnforced {
            enforced: ProfileKeyServices::get().is_some() && server_enforces,
        },
    );
}

fn handle(
    packet: LifecyclePacket,
    registry_holder: &RegistryHolder,
    sender: &PacketSender,
    event_tx: &Sender<NetworkEvent>,
) {
    match packet {
        LifecyclePacket::Login(_) => unreachable!("the game loop reads login"),
        LifecyclePacket::Respawn {
            spawn,
            data_to_keep,
        } => {
            send_ordered(
                event_tx,
                NetworkEvent::PlayerRespawned {
                    keep_entity_data: data_to_keep & 2 != 0,
                    keep_attribute_modifiers: data_to_keep & 1 != 0,
                },
            );
            spawn_events(&spawn, registry_holder, event_tx);
            // Vanilla recreates the player on respawn; the server re-sends any
            // effects kept across it.
            send_ordered(event_tx, NetworkEvent::ClearMobEffects);
        }
        LifecyclePacket::KeepAlive(id) => {
            sender.send(ServerboundGamePacket::KeepAlive(ServerboundKeepAlive {
                id: id as u64,
            }));
        }
        // Vanilla `handlePing` pongs from the main thread, in order with the
        // other main-thread packets.
        LifecyclePacket::Ping(id) => send_ordered(event_tx, NetworkEvent::Ping { id: id as u32 }),
        LifecyclePacket::Disconnect(reason) => disconnected(event_tx, disconnect_reason(&reason)),
        // Acknowledged on the game thread once the correction is applied.
        LifecyclePacket::PlayerPosition {
            id,
            change,
            relative,
        } => send_ordered(
            event_tx,
            NetworkEvent::PlayerPosition {
                id,
                change,
                relative,
            },
        ),
        LifecyclePacket::SelectAdvancementsTab { tab } => {
            send_ordered(event_tx, NetworkEvent::SelectAdvancementsTab(tab))
        }
        LifecyclePacket::TickingState { tick_rate, frozen } => {
            send_ordered(event_tx, NetworkEvent::TickingState { tick_rate, frozen })
        }
        LifecyclePacket::TickingStep { steps } => {
            send_ordered(event_tx, NetworkEvent::TickingStep { steps })
        }
        LifecyclePacket::BorderInit {
            x,
            z,
            old_size,
            new_size,
            lerp_time,
            absolute_max,
            warning_blocks,
            warning_time,
        } => send_ordered(
            event_tx,
            NetworkEvent::WorldBorder(BorderUpdate::Init {
                x,
                z,
                old_size,
                new_size,
                lerp_time,
                absolute_max,
                warning_blocks,
                warning_time,
            }),
        ),
        LifecyclePacket::BorderCenter { x, z } => send_ordered(
            event_tx,
            NetworkEvent::WorldBorder(BorderUpdate::Center { x, z }),
        ),
        LifecyclePacket::BorderLerpSize {
            old_size,
            new_size,
            lerp_time,
        } => send_ordered(
            event_tx,
            NetworkEvent::WorldBorder(BorderUpdate::LerpSize {
                old_size,
                new_size,
                lerp_time,
            }),
        ),
        LifecyclePacket::BorderSize { size } => send_ordered(
            event_tx,
            NetworkEvent::WorldBorder(BorderUpdate::Size { size }),
        ),
        LifecyclePacket::BorderWarningDelay { time } => send_ordered(
            event_tx,
            NetworkEvent::WorldBorder(BorderUpdate::WarningTime { time }),
        ),
        LifecyclePacket::BorderWarningDistance { blocks } => send_ordered(
            event_tx,
            NetworkEvent::WorldBorder(BorderUpdate::WarningBlocks { blocks }),
        ),
        LifecyclePacket::PlayerLookAt {
            from_eyes,
            target,
            entity,
        } => send_ordered(
            event_tx,
            NetworkEvent::PlayerLookAt {
                from_eyes,
                target: glam::DVec3::from_array(target),
                entity,
            },
        ),
        LifecyclePacket::PlayerRotation {
            y_rot,
            relative_y,
            x_rot,
            relative_x,
        } => send_ordered(
            event_tx,
            NetworkEvent::PlayerRotation {
                y_rot,
                relative_y,
                x_rot,
                relative_x,
            },
        ),
        LifecyclePacket::SetTime { game_time, clocks } => {
            let names = registry_holder.extra.get(&Identifier::new(WORLD_CLOCK));
            let clocks = clocks
                .into_iter()
                .filter_map(|(id, state)| {
                    let name = match names {
                        Some(names) => names.map.get_index(id as usize)?.0.to_string(),
                        // A server older than world clocks (26.1) sends the
                        // level's day time, which the translation puts on id 0.
                        None if id == 0 => OLD_DAY_CLOCK.to_owned(),
                        None => return None,
                    };
                    Some((name, state))
                })
                .collect();
            send_ordered(
                event_tx,
                NetworkEvent::TimeUpdate {
                    game_time: game_time as u64,
                    clocks,
                },
            );
        }
        LifecyclePacket::SimulationDistance(distance) => {
            send_ordered(
                event_tx,
                NetworkEvent::ServerSimulationDistance { distance },
            );
        }
    }
}

/// What login and respawn share: the dimension and the game modes.
fn spawn_events(
    spawn: &SpawnInfo,
    registry_holder: &RegistryHolder,
    event_tx: &Sender<NetworkEvent>,
) {
    match registry_holder
        .dimension_type
        .map
        .get_index(spawn.dimension_type as usize)
    {
        Some((_, dim)) => {
            send_ordered(
                event_tx,
                dimension_info(dim, &spawn.dimension, spawn.is_flat, registry_holder),
            );
        }
        None => tracing::error!("Unknown dimension_type {}", spawn.dimension_type),
    }
    send_ordered(
        event_tx,
        NetworkEvent::DimensionName {
            name: spawn.dimension.clone(),
        },
    );
    send_ordered(
        event_tx,
        NetworkEvent::GameModeChanged {
            game_mode: super::game_type(spawn.game_type.into()),
            previous: Some(
                spawn
                    .previous_game_type
                    .map(|id| super::game_type(id.into())),
            ),
        },
    );
}

const WORLD_CLOCK: &str = "minecraft:world_clock";
/// The clock a server older than world clocks keeps its day time on: every
/// dimension follows it, as the level's `dayTime` did.
const OLD_DAY_CLOCK: &str = "minecraft:overworld";

/// Dimension info from a login/respawn registry entry. Fields that azalea does
/// not model directly live in its flattened extras. Missing `has_skylight`
/// defaults to true; missing `cardinal_light` defaults to vanilla's `default`.
fn dimension_info(
    dim: &DimensionKindElement,
    dimension: &str,
    flat: bool,
    registries: &RegistryHolder,
) -> NetworkEvent {
    let string = |key: &str| {
        dim._extra
            .get(key)
            .and_then(|tag| tag.string())
            .map(|value| value.to_str().into_owned())
    };
    let flag = |key: &str| {
        dim._extra
            .get(key)
            .and_then(|tag| tag.byte())
            .map(|b| b != 0)
    };
    let has_skylight = flag("has_skylight").unwrap_or(true);
    // Vanilla `Level.canHaveWeather`, which names the end.
    let has_weather =
        has_skylight && !flag("has_ceiling").unwrap_or(false) && dimension != "minecraft:the_end";
    NetworkEvent::DimensionInfo(DimensionInfo {
        height: dim.height,
        min_y: dim.min_y,
        has_skylight,
        cardinal_light: match string("cardinal_light").as_deref() {
            Some("nether") => CardinalLightType::Nether,
            _ => CardinalLightType::Default,
        },
        ambient_light: dim
            ._extra
            .get("ambient_light")
            .and_then(|tag| tag.float())
            .unwrap_or(0.0),
        default_clock: string("default_clock").or_else(|| {
            (!registries.extra.contains_key(&Identifier::new(WORLD_CLOCK)))
                .then(|| OLD_DAY_CLOCK.to_owned())
        }),
        flat,
        environment: Box::new(super::environment_wire::environment(
            &dim._extra,
            registries,
            has_weather,
        )),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use simdnbt::owned::NbtTag;

    use super::*;

    fn dimension(extra: &[(&str, NbtTag)]) -> DimensionKindElement {
        DimensionKindElement {
            height: 384,
            min_y: -64,
            ultrawarm: None,
            _extra: extra
                .iter()
                .map(|(key, tag)| (key.to_string(), tag.clone()))
                .collect::<HashMap<_, _>>(),
        }
    }

    #[test]
    fn set_time_names_clocks_by_the_synced_registry() {
        use azalea_core::registry_holder::RegistryType;
        use pomme_protocol::lifecycle::ClockState;
        use simdnbt::owned::NbtCompound;

        let mut registries = RegistryHolder::default();
        let mut clocks = RegistryType::<NbtCompound>::default();
        for name in ["minecraft:overworld", "minecraft:the_end"] {
            clocks.map.insert(Identifier::new(name), NbtCompound::new());
        }
        registries
            .extra
            .insert(Identifier::new("minecraft:world_clock"), clocks);
        let state = |total_ticks| ClockState {
            total_ticks,
            partial_tick: 0.0,
            rate: 1.0,
        };
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (out_tx, _out_rx) = tokio::sync::mpsc::unbounded_channel();
        handle(
            LifecyclePacket::SetTime {
                game_time: 10,
                clocks: vec![(1, state(300)), (0, state(6000)), (7, state(1))],
            },
            &registries,
            &PacketSender::new(out_tx),
            &event_tx,
        );
        let Ok(NetworkEvent::TimeUpdate { game_time, clocks }) = event_rx.try_recv() else {
            panic!("expected TimeUpdate");
        };
        assert_eq!(game_time, 10);
        assert_eq!(
            clocks,
            [
                ("minecraft:the_end".to_owned(), state(300)),
                ("minecraft:overworld".to_owned(), state(6000)),
            ]
        );
    }

    #[test]
    fn set_time_from_a_server_without_world_clocks_drives_the_overworld_clock() {
        use pomme_protocol::lifecycle::ClockState;

        let state = ClockState {
            total_ticks: 18000,
            partial_tick: 0.0,
            rate: 1.0,
        };
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (out_tx, _out_rx) = tokio::sync::mpsc::unbounded_channel();
        handle(
            LifecyclePacket::SetTime {
                game_time: 10,
                clocks: vec![(0, state), (1, state)],
            },
            &RegistryHolder::default(),
            &PacketSender::new(out_tx),
            &event_tx,
        );
        let Ok(NetworkEvent::TimeUpdate { clocks, .. }) = event_rx.try_recv() else {
            panic!("expected TimeUpdate");
        };
        assert_eq!(clocks, [(OLD_DAY_CLOCK.to_owned(), state)]);
    }

    #[test]
    fn dimension_info_reads_vanilla_extras() {
        let NetworkEvent::DimensionInfo(DimensionInfo {
            height,
            min_y,
            has_skylight,
            cardinal_light,
            ambient_light,
            default_clock,
            ..
        }) = dimension_info(
            &dimension(&[
                ("has_skylight", NbtTag::Byte(1)),
                ("cardinal_light", NbtTag::String("nether".into())),
                ("ambient_light", NbtTag::Float(0.1)),
                (
                    "timelines",
                    NbtTag::String("#minecraft:in_overworld".into()),
                ),
                (
                    "default_clock",
                    NbtTag::String("minecraft:overworld".into()),
                ),
            ]),
            "minecraft:overworld",
            false,
            &RegistryHolder::default(),
        )
        else {
            panic!("dimension_info returned the wrong event variant");
        };
        assert_eq!((height, min_y, has_skylight), (384, -64, true));
        assert_eq!(cardinal_light, CardinalLightType::Nether);
        assert_eq!(ambient_light, 0.1);
        assert_eq!(default_clock.as_deref(), Some("minecraft:overworld"));
    }

    #[test]
    fn dimension_info_defaults_like_vanilla() {
        let NetworkEvent::DimensionInfo(DimensionInfo {
            has_skylight,
            cardinal_light,
            ambient_light,
            default_clock,
            ..
        }) = dimension_info(
            &dimension(&[]),
            "minecraft:overworld",
            false,
            &RegistryHolder::default(),
        )
        else {
            panic!("dimension_info returned the wrong event variant");
        };
        assert!(has_skylight);
        assert_eq!(cardinal_light, CardinalLightType::Default);
        assert_eq!(ambient_light, 0.0);
        // A server with no world clocks keeps one day time for every dimension.
        assert_eq!(default_clock.as_deref(), Some(OLD_DAY_CLOCK));

        let mut registries = RegistryHolder::default();
        registries
            .extra
            .insert(Identifier::new(WORLD_CLOCK), Default::default());
        let NetworkEvent::DimensionInfo(DimensionInfo { default_clock, .. }) =
            dimension_info(&dimension(&[]), "minecraft:the_nether", false, &registries)
        else {
            panic!("dimension_info returned the wrong event variant");
        };
        assert_eq!(default_clock, None);
    }
}
