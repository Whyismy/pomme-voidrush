use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::ui::text::{TextSpan, format_text_spans};

#[derive(Clone, Serialize, Deserialize)]
pub struct ServerEntry {
    pub name: String,
    pub address: String,
    /// The protocol from this server's last successful ping, so a join
    /// before the current ping completes still skips the wire-version probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<i32>,
    /// Fields other tools own (the launcher's category, ...), passed through
    /// untouched so a client save doesn't strip them.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// How the client can speak to a pinged server.
#[derive(Clone, Copy, PartialEq)]
pub enum Compat {
    /// The version the client speaks natively: joined without translation.
    Native,
    /// Another protocol with embedded translation data: joinable, with the
    /// wire translated on the fly.
    Translated,
    /// A protocol without translation data: a join would be refused.
    Incompatible,
}

#[derive(Clone)]
pub enum PingState {
    Pinging,
    Success {
        motd: Vec<TextSpan>,
        online: i32,
        max: i32,
        latency_ms: u64,
        version: String,
        /// The server's protocol number, reused at join time to skip the
        /// wire-version probe.
        protocol: i32,
        compat: Compat,
        favicon_rgba: Option<Vec<u8>>,
        player_names: Vec<String>,
    },
    Failed(String),
}

pub struct ServerList {
    pub servers: Vec<ServerEntry>,
    path: PathBuf,
}

pub type PingResults = Arc<RwLock<HashMap<String, PingState>>>;

/// Monotonic refresh counter. A ping discards its result if this advanced while
/// it was in flight (mirrors vanilla's `pinger.removeAll()` on refresh).
pub type PingGeneration = Arc<AtomicU64>;

/// Void Rush's own server, always in this build's list so a player who
/// disconnects can get back in.
const OFFICIAL_NAME: &str = "VoidRush Official";
const OFFICIAL_ADDRESS: &str = "play.voidrush.org";

impl ServerList {
    pub fn load(game_dir: &Path) -> Self {
        let path = game_dir.join("servers.json");
        let servers = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let mut list = Self { servers, path };
        if list.add_official() {
            list.save();
        }
        list
    }

    /// Puts Void Rush's server at the top of the list when no entry already
    /// points at it (any name, with or without the default port); whether
    /// it added it.
    fn add_official(&mut self) -> bool {
        let official = with_default_port(OFFICIAL_ADDRESS);
        if self
            .servers
            .iter()
            .any(|s| with_default_port(s.address.trim()).eq_ignore_ascii_case(&official))
        {
            return false;
        }
        self.servers.insert(
            0,
            ServerEntry {
                name: OFFICIAL_NAME.into(),
                address: OFFICIAL_ADDRESS.into(),
                protocol: None,
                extra: serde_json::Map::new(),
            },
        );
        true
    }

    pub fn save(&self) {
        // TODO: use crate::util::write_atomic (vanilla ServerList.save goes
        // through a temp file + Util.safeReplaceFile).
        if let Ok(json) = serde_json::to_string_pretty(&self.servers)
            && let Err(e) = std::fs::write(&self.path, json)
        {
            tracing::warn!("Failed to save server list: {e}");
        }
    }

    pub fn add(&mut self, entry: ServerEntry) {
        self.servers.push(entry);
        self.save();
    }

    pub fn update(&mut self, index: usize, entry: ServerEntry) {
        if index < self.servers.len() {
            self.servers[index] = entry;
            self.save();
        }
    }

    pub fn swap(&mut self, a: usize, b: usize) {
        if a < self.servers.len() && b < self.servers.len() {
            self.servers.swap(a, b);
            self.save();
        }
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.servers.len() {
            self.servers.remove(index);
            self.save();
        }
    }
}

pub fn ping_all_servers(
    rt: &tokio::runtime::Runtime,
    servers: &[ServerEntry],
    results: &PingResults,
    generation: &PingGeneration,
) {
    let spawned_gen = generation.load(Ordering::Acquire);
    for server in servers {
        let address = server.address.clone();
        results.write().insert(address.clone(), PingState::Pinging);
        rt.spawn(ping_server(
            address,
            Arc::clone(results),
            Arc::clone(generation),
            spawned_gen,
        ));
    }
}

async fn ping_server(
    address: String,
    results: PingResults,
    generation: PingGeneration,
    spawned_gen: u64,
) {
    use azalea_protocol::packets::status::ClientboundStatusPacket;
    use azalea_protocol::packets::status::s_ping_request::ServerboundPingRequest;

    let result = async {
        use azalea_protocol::address::ServerAddr;

        let server_addr: ServerAddr = address
            .as_str()
            .try_into()
            .map_err(|_| format!("Invalid address: {address}"))?;
        let (status, mut conn) = crate::net::resolve::request_status(&server_addr)
            .await
            .map_err(|e| format!("{address}: {e}"))?;

        let ping_start = Instant::now();
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        conn.write_packet(ServerboundPingRequest { time })
            .await
            .map_err(|e| format!("Ping request failed: {e}"))?;

        let _ = conn
            .read_packet::<ClientboundStatusPacket>()
            .await
            .map_err(|e| format!("Pong failed: {e}"))?;
        let latency_ms = ping_start.elapsed().as_millis() as u64;

        // Vanilla MOTD base color: 0x808080.
        let motd = format_text_spans(&status.description, [0.5, 0.5, 0.5, 1.0]);
        let version = status.version.name.clone();
        // Keyed to the native version, not the launched one.
        let compat = if status.version.protocol == pomme_protocol::version::NATIVE.protocol {
            Compat::Native
        } else if crate::net::translate::joinable(status.version.protocol) {
            let protocol = status.version.protocol;
            tokio::task::spawn_blocking(move || crate::net::translate::prewarm(protocol));
            Compat::Translated
        } else {
            Compat::Incompatible
        };
        let (online, max) = (status.players.online, status.players.max);

        let favicon_rgba = status.favicon.as_deref().and_then(decode_favicon);
        let player_names: Vec<String> = status
            .players
            .sample
            .iter()
            .map(|p| p.name.clone())
            .collect();

        Ok(PingState::Success {
            motd,
            online,
            max,
            latency_ms,
            version,
            protocol: status.version.protocol,
            compat,
            favicon_rgba,
            player_names,
        })
    }
    .await;

    let state = match result {
        Ok(s) => s,
        Err(e) => PingState::Failed(e),
    };
    if generation.load(Ordering::Acquire) == spawned_gen {
        results.write().insert(address, state);
    }
}

fn decode_favicon(data: &str) -> Option<Vec<u8>> {
    let b64 = data.strip_prefix("data:image/png;base64,").unwrap_or(data);
    let png_bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64).ok()?;
    let img = image::load_from_memory(&png_bytes).ok()?.to_rgba8();
    Some(img.into_raw())
}

fn with_default_port(address: &str) -> String {
    if address.contains(':') {
        address.to_string()
    } else {
        format!("{address}:25565")
    }
}

pub fn is_valid_address(address: &str) -> bool {
    if address.is_empty() {
        return false;
    }
    let with_port = with_default_port(address);
    with_port.parse::<std::net::SocketAddr>().is_ok()
        || with_port
            .split(':')
            .next()
            .is_some_and(|host| !host.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The launcher writes fields the client doesn't model (category); a
    /// client save must pass them through, not strip them.
    #[test]
    fn entry_round_trip_keeps_unknown_fields() {
        let json = r#"{"name":"a","address":"b:25565","protocol":775,"category":"Modded"}"#;
        let entry: ServerEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.protocol, Some(775));
        assert_eq!(
            entry.extra.get("category").and_then(|v| v.as_str()),
            Some("Modded")
        );
        let back: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(serde_json::to_value(&entry).unwrap(), back);
    }

    /// A new install starts with Void Rush's server; a list that has it
    /// (under any name, with the port spelled out) keeps its own entry.
    #[test]
    fn the_official_server_is_always_listed() {
        let dir = std::env::temp_dir().join(format!("pomme-servers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join("servers.json"));
        let list = ServerList::load(&dir);
        assert_eq!(list.servers.len(), 1);
        assert_eq!(list.servers[0].name, "VoidRush Official");
        assert_eq!(list.servers[0].address, "play.voidrush.org");
        // Saved, so the next start finds it and adds nothing.
        assert_eq!(ServerList::load(&dir).servers.len(), 1);

        std::fs::write(
            dir.join("servers.json"),
            r#"[{"name":"a","address":"x.net"},{"name":"VR","address":"PLAY.voidrush.org:25565"}]"#,
        )
        .unwrap();
        let list = ServerList::load(&dir);
        assert_eq!(list.servers.len(), 2);
        assert_eq!(list.servers[1].name, "VR");

        std::fs::write(
            dir.join("servers.json"),
            r#"[{"name":"a","address":"x.net"}]"#,
        )
        .unwrap();
        let list = ServerList::load(&dir);
        assert_eq!(list.servers.len(), 2);
        assert_eq!(list.servers[0].address, "play.voidrush.org");
        assert_eq!(list.servers[1].name, "a");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Entries without the optional fields load and save without gaining any.
    #[test]
    fn entry_round_trip_minimal() {
        let json = r#"{"name":"a","address":"b"}"#;
        let entry: ServerEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.protocol, None);
        assert_eq!(serde_json::to_string(&entry).unwrap(), json);
    }
}
