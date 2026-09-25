//! MCP process telemetry (Node `adapters/src/mcp/telemetry.ts`; spec
//! rust-m9-usage-logs §6): stdio server starts and crashes, the first MCP
//! snapshot of each session, and the tracked processes the resource sampler
//! and `process/childProcesses` read. Server names leave the process only for
//! built-in servers; others are an HMAC keyed by a per-process salt.
use super::mcp_config::Server;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

/// Where process notifications go (`(method, params)`), drained by the engine.
pub type ProcessSink = tokio::sync::mpsc::UnboundedSender<(&'static str, Value)>;

/// The pseudo-session of `mcp/list` (Node's `protocol-settings` lease has no session).
pub(super) const STATUS_SESSION: &str = "mcp-status";
const PLUGIN_PREFIX: &str = "plugin:";

/// Node's `process.platform` of this build.
pub(super) fn platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        "solaris" => "sunos",
        os => os,
    }
}

/// Node's `process.arch` of this build.
pub(super) fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        "loongarch64" => "loong64",
        "powerpc" => "ppc",
        "powerpc64" => "ppc64",
        arch => arch,
    }
}

pub(super) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// HMAC-SHA256 (RFC 2104).
fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| block.iter().map(|b| b ^ byte).collect::<Vec<_>>();
    let inner = Sha256::new()
        .chain_update(pad(0x36))
        .chain_update(data)
        .finalize();
    Sha256::new()
        .chain_update(pad(0x5c))
        .chain_update(inner)
        .finalize()
        .to_vec()
}

/// Node `encodeMcpIdSegment`: UTF-8 bytes, unreserved characters kept, the rest `%HH`.
fn encode_segment(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'~' | b'-' => {
                (b as char).to_string()
            }
            b => format!("%{b:02X}"),
        })
        .collect()
}

/// Node `resolveMcpSource`: the host-attached source, else by name.
fn source(server: &Server) -> &'static str {
    match server.raw["source"]["kind"].as_str() {
        Some("builtin") => "builtin",
        Some("plugin") => "plugin",
        _ if server.name == "node_repl" => "builtin",
        _ if server.name.starts_with(PLUGIN_PREFIX) => "plugin",
        _ => "custom",
    }
}

/// Node `resolveMcpId`.
fn mcp_id(name: &str, source: &str, salt: &str) -> String {
    if source == "builtin" {
        let public = name.strip_prefix(PLUGIN_PREFIX).unwrap_or(name);
        let segments: Vec<String> = public.split(':').map(encode_segment).collect();
        return format!("builtin:{}", segments.join(":"));
    }
    let digest = hmac(salt.as_bytes(), name.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("{source}:{}", &hex[..12])
}

/// A live stdio server process.
#[derive(Clone)]
pub(super) struct Tracked {
    /// The connection key (its owners are the sessions bound to it).
    key: String,
    pub server: String,
    pub mcp_id: String,
    pub source: &'static str,
    isolation: &'static str,
    pub instance: String,
    pub pid: u32,
    pub started_at: u64,
    plugin: Option<String>,
}

#[derive(Default)]
struct State {
    /// Process instance → process: a replacement connection has its own instance.
    processes: BTreeMap<String, Tracked>,
    /// Connection key → the sessions bound to it.
    owners: BTreeMap<String, BTreeSet<String>>,
    /// Sessions whose first snapshot was reported.
    reported: BTreeSet<String>,
}

pub(super) struct Tracker {
    salt: String,
    sink: Option<ProcessSink>,
    state: Mutex<State>,
}

impl Tracker {
    pub fn new(sink: Option<ProcessSink>) -> Self {
        Self {
            salt: uuid::Uuid::new_v4().to_string(),
            sink,
            state: Mutex::default(),
        }
    }

    fn emit(&self, method: &'static str, params: Value) {
        if let Some(sink) = &self.sink {
            let _ = sink.send((method, params));
        }
    }

    /// Node `recordProcessStarted` after a stdio server connected; its instance.
    pub fn started(&self, key: &str, server: &Server, pid: u32) -> String {
        let source = source(server);
        let plugin = server.raw["source"]["pluginName"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| {
                let rest = server.name.strip_prefix(PLUGIN_PREFIX)?;
                rest.split(':')
                    .next()
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                    .map(str::to_owned)
            });
        let tracked = Tracked {
            key: key.into(),
            server: server.name.clone(),
            mcp_id: mcp_id(&server.name, source, &self.salt),
            source,
            isolation: if server.workspace {
                "workspace"
            } else {
                "session"
            },
            instance: uuid::Uuid::new_v4().to_string(),
            pid,
            started_at: now_ms(),
            plugin,
        };
        self.emit(
            "process/mcpTelemetry",
            json!({"arch": arch(), "kind": "process_start", "mcpId": tracked.mcp_id,
                "mcpInstanceId": tracked.instance, "mcpIsolation": tracked.isolation,
                "mcpSource": tracked.source, "occurredAt": tracked.started_at, "platform": platform()}),
        );
        let instance = tracked.instance.clone();
        self.state
            .lock()
            .unwrap()
            .processes
            .insert(instance.clone(), tracked);
        instance
    }

    /// The server process ended on its own (Node `recordProcessCrash`).
    pub fn crashed(&self, instance: &str, code: Option<i32>, signal: Option<&str>) {
        let (tracked, affected) = {
            let mut state = self.state.lock().unwrap();
            let Some(tracked) = state.processes.remove(instance) else {
                return;
            };
            let affected = state.owners.get(&tracked.key).map_or(0, BTreeSet::len);
            (tracked, affected)
        };
        let now = now_ms();
        self.emit(
            "process/mcpTelemetry",
            json!({"affectedSessionCount": affected, "arch": arch(), "exitCode": code,
                "kind": "process_crash", "mcpId": tracked.mcp_id, "mcpInstanceId": tracked.instance,
                "mcpIsolation": tracked.isolation, "mcpSource": tracked.source, "occurredAt": now,
                "platform": platform(), "signal": signal,
                "uptimeMs": now.saturating_sub(tracked.started_at)}),
        );
    }

    /// The connection closed on purpose: no event.
    pub fn closed(&self, instance: &str) {
        self.state.lock().unwrap().processes.remove(instance);
    }

    /// The connection keys `session` is bound to now.
    pub fn bind(&self, session: &str, keys: &BTreeSet<String>) {
        if session == STATUS_SESSION {
            return;
        }
        let mut state = self.state.lock().unwrap();
        for (key, owners) in state.owners.iter_mut() {
            if !keys.contains(key) {
                owners.remove(session);
            }
        }
        for key in keys {
            state
                .owners
                .entry(key.clone())
                .or_default()
                .insert(session.into());
        }
    }

    pub fn unbind(&self, session: &str) {
        let mut state = self.state.lock().unwrap();
        for owners in state.owners.values_mut() {
            owners.remove(session);
        }
        state.reported.remove(session);
    }

    /// Node `session_startup` on a session's first MCP snapshot:
    /// `(configured, connected, processes)`.
    pub fn session_startup(&self, session: &str, counts: (usize, usize, usize)) {
        if session == STATUS_SESSION || !self.state.lock().unwrap().reported.insert(session.into())
        {
            return;
        }
        let (configured, connected, processes) = counts;
        self.emit(
            "process/mcpTelemetry",
            json!({"arch": arch(), "configuredCount": configured, "connectedCount": connected,
                "failedCount": configured.saturating_sub(connected), "kind": "session_startup",
                "occurredAt": now_ms(), "platform": platform(), "processCount": processes,
                "sessionId": session}),
        );
    }

    /// The live processes (Node `listProcesses`, `process/childProcesses`).
    pub fn processes(&self) -> Vec<Value> {
        self.state
            .lock()
            .unwrap()
            .processes
            .values()
            .map(|p| {
                let mut entry =
                    json!({"pid": p.pid, "serverName": p.server, "mcpSource": p.source});
                if let Some(plugin) = &p.plugin {
                    entry["pluginName"] = plugin.clone().into();
                }
                entry
            })
            .collect()
    }

    pub(super) fn tracked(&self) -> Vec<Tracked> {
        self.state
            .lock()
            .unwrap()
            .processes
            .values()
            .cloned()
            .collect()
    }

    pub(super) fn sample(&self, samples: Vec<Value>) {
        if !samples.is_empty() {
            self.emit("process/mcpResourceSamples", Value::Array(samples));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_hide_custom_names() {
        assert_eq!(mcp_id("node_repl", "builtin", "s"), "builtin:node_repl");
        assert_eq!(mcp_id("plugin:a b:x", "builtin", "s"), "builtin:a%20b:x");
        let custom = mcp_id("secret-server", "custom", "salt");
        assert!(
            custom.starts_with("custom:") && custom.len() == 19,
            "{custom}"
        );
        assert_ne!(custom, mcp_id("secret-server", "custom", "other"));
        // RFC 4231 test case 2.
        let digest = hmac(b"Jefe", b"what do ya want for nothing?");
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
}
