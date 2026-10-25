//! MCP 设置的组装（docs/specs/rust-cli/ext.md §3.1）：配置文件的服务器由 `config` 模块读（Node 的
//! `mcp.servers`），这里加上命令行 `--mcp-config` 与环境变量里的超时，组装成快照交给 ext。
//!
//! `--mcp-config` 是 `mcpServers` 的形状（`${VAR}` 展开）；文件经执行环境端口读。

use rust_cli_ext::mcp::{
    DEFAULT_CONNECT_WAIT_MS, McpScope, McpServer, McpSettings, McpStartup, parse_servers_keeping,
    prewait_ms, required_servers, wait_for_deferrable,
};

use crate::mcp_machines::McpBindings;
use rust_cli_kernel_api::env::{EnvPath, ReadFile, WorkspaceRef};
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::machine::Files;
use serde_json::Value;

/// 命令行的 MCP 参数。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct McpFlags {
    /// `--mcp-config`：JSON 文本或文件路径。
    pub(crate) configs: Vec<String>,
    /// `--strict-mcp-config`：只用 `--mcp-config` 的服务器。
    pub(crate) strict: bool,
    /// `--input-format stream-json`（headless.md §6）。
    pub(crate) streaming_input: bool,
    /// 能用工具搜索（`ENABLE_TOOL_SEARCH` 不是关，乐观判断，工作线 F89；tools-more.md §3.2.1）：入口按设置填。
    pub(crate) tool_search: bool,
}

fn var<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    rust_cli_base::secret::env_var(env, name)
}

fn number(env: &[(String, String)], name: &str) -> Option<u64> {
    var(env, name)
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
}

/// 毫秒数的上限（`Math.min(_, 2147483647)`，JS 定时器的上限，定义在 base）。
const MAX_MS: u64 = rust_cli_base::js::MAX_TIMER_MS;

/// 整数环境变量的读法：去空白；科学计数法（结果须为整数）、千分位写法（`1,000`，分隔符一致）或 `parseInt` 的前导数字。不合法时
/// None。
fn env_int(raw: &str) -> Option<i64> {
    let text = raw.trim();
    if text.len() <= 32 {
        if let Some(value) = scientific(text) {
            return value;
        }
        if let Some(value) = grouped(text) {
            return Some(value);
        }
    }
    // `parseInt(text, 10)`：可选的符号加前导数字。
    let (sign, digits) = split_sign(text);
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    digits[..end].parse::<i64>().ok().map(|value| sign * value)
}

fn split_sign(text: &str) -> (i64, &str) {
    match text.as_bytes().first() {
        Some(b'-') => (-1, &text[1..]),
        Some(b'+') => (1, &text[1..]),
        _ => (1, text),
    }
}

/// 科学计数法（`^[+-]?(\d+(\.\d*)?|\.\d+)[eE][+-]?\d+$`）：匹配时外层 Some，结果不是整数时内层 None
/// （JS 的 NaN）。
fn scientific(text: &str) -> Option<Option<i64>> {
    let (_, body) = split_sign(text);
    let (mantissa, exponent) = body.split_once(['e', 'E'])?;
    let digits = |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit());
    let mantissa_ok = match mantissa.split_once('.') {
        None => digits(mantissa),
        Some((whole, fraction)) => {
            (digits(whole) && (fraction.is_empty() || digits(fraction)))
                || (whole.is_empty() && digits(fraction))
        }
    };
    let (_, exponent) = split_sign(exponent);
    if !mantissa_ok || !digits(exponent) {
        return None;
    }
    let value: f64 = text.parse().ok()?;
    Some(
        (value.is_finite() && value.fract() == 0.0 && value.abs() < 9.0e15).then_some(value as i64),
    )
}

/// 千分位写法（`^[+-]?\d{1,3}([_,\u00A0\u202F ])\d{3}(?:\1\d{3})*$`）。
fn grouped(text: &str) -> Option<i64> {
    let (sign, body) = split_sign(text);
    let separator = body.chars().find(|c| !c.is_ascii_digit())?;
    if !['_', ',', '\u{00A0}', '\u{202F}', ' '].contains(&separator) {
        return None;
    }
    let groups: Vec<&str> = body.split(separator).collect();
    let digits = |group: &str| group.chars().all(|c| c.is_ascii_digit());
    let first = groups.first()?;
    let valid = groups.len() >= 2
        && (1..=3).contains(&first.len())
        && digits(first)
        && groups[1..]
            .iter()
            .all(|group| group.len() == 3 && digits(group));
    valid
        .then(|| groups.concat().parse::<i64>().ok())
        .flatten()
        .map(|value| sign * value)
}

/// 正的毫秒数，缺省 `default`（`_ && _ > 0 ? Math.min(_, 2147483647) : 缺省`）。
fn positive_ms(env: &[(String, String)], name: &str, default: u64) -> u64 {
    var(env, name)
        .and_then(env_int)
        .and_then(|value| u64::try_from(value).ok())
        .filter(|value| *value > 0)
        .map_or(default, |value| value.min(MAX_MS))
}

/// 三值布尔：`1` / `true` / `yes` / `on` 为真，`0` / `false` / `no` / `off`
/// 为假，其余为没设。
fn tri_bool(env: &[(String, String)], name: &str) -> Option<bool> {
    let value = var(env, name)?.trim().to_lowercase();
    match value.as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// `-p` 的启动等待（ext.md §3.3.1，修复线 F36）：`explicit` 是 `--mcp-config` 给出了服务器，`allowed` 是
/// `--allowedTools` 的规则；stream-json 输入时按 `flags` 判断是不是 `lifted`（headless.md §6，修复线 F39）。
fn startup(
    snapshot: &[(String, String)],
    (explicit, flags): (bool, &McpFlags),
    mcp_timeout_ms: u64,
    allowed: &[String],
) -> McpStartup {
    // 启动等待的截止（config.md §2.5）。
    let startup_wait = var(snapshot, "ZCODE_MCP_STARTUP_WAIT_MS")
        .and_then(env_int)
        .and_then(|value| u64::try_from(value).ok())
        .map(|value| value.min(MAX_MS));
    McpStartup {
        // 只有明确设成“假”时阻塞。
        nonblocking: tri_bool(snapshot, "MCP_CONNECTION_NONBLOCKING") != Some(false),
        connect_wait_ms: positive_ms(snapshot, "MCP_CONNECT_TIMEOUT_MS", DEFAULT_CONNECT_WAIT_MS),
        prewait_ms: prewait_ms(startup_wait, explicit, mcp_timeout_ms),
        required: required_servers(allowed.iter().map(String::as_str)),
        wait_for_deferrable: wait_for_deferrable(
            flags.streaming_input,
            flags.tool_search,
            explicit,
            startup_wait,
        ),
    }
}

struct Reader<'a> {
    files: &'a dyn Files,
    workspace: &'a WorkspaceRef,
    snapshot: &'a [(String, String)],
    /// 绑定到远端机器的服务器按原文解析（R5，remote-exec.md §22.7）。
    bindings: &'a McpBindings,
}

impl Reader<'_> {
    async fn json(&self, path: &str) -> Option<Value> {
        let request = ReadFile {
            path: EnvPath::new(path),
        };
        let content = self.files.read_file(self.workspace, request).await.ok()?;
        match serde_json::from_slice(&content.bytes) {
            Ok(value) => Some(value),
            Err(error) => {
                tracing::warn!(path, %error, "MCP configuration is not valid JSON");
                None
            }
        }
    }

    fn servers(&self, table: Option<&Value>, scope: McpScope) -> Vec<McpServer> {
        let Some(table) = table else {
            return Vec::new();
        };
        let lookup = |name: &str| var(self.snapshot, name).map(str::to_owned);
        let keep = |name: &str| self.bindings.machine(name).is_some();
        let (servers, warnings) = parse_servers_keeping(table, scope, &lookup, &keep);
        for warning in warnings {
            tracing::warn!(warning, "MCP configuration");
        }
        servers
    }

    /// `--mcp-config`：JSON 文本或文件（相对 cwd）。
    async fn dynamic(&self, configs: &[String]) -> Vec<McpServer> {
        let mut found = Vec::new();
        for config in configs {
            let text = config.trim();
            let value = if text.starts_with('{') {
                serde_json::from_str(text).ok()
            } else {
                // 相对工作区根解析，规则是 kernel-api `env_path`（R1-9：以前自己判断绝对路径、自己拼接）。
                let root = self.workspace.root.as_str();
                let path = PathStyle::infer(root).resolve(root, text, None);
                self.json(&path).await
            };
            match value {
                Some(value) => {
                    found.extend(self.servers(value.get("mcpServers"), McpScope::Dynamic))
                }
                None => tracing::warn!(config, "--mcp-config could not be read"),
            }
        }
        found
    }
}

/// 组装 MCP 设置：配置文件的服务器（`configured`，已按 Node 规则合并，config.md §2）加上
/// `--mcp-config`（按名覆盖；`--strict-mcp-config` 时只用它）。`enabled` 为 false（`features.mcp`）时
/// 没有服务器。`allowed` 是 `--allowedTools` 的规则（`-p` 启动等待里点名的服务器）。合并之后按 `bindings`
/// 设上服务器所在的远端机器（R5，remote-exec.md §22.2）。
pub(crate) async fn settings(
    files: &dyn Files,
    workspace: &WorkspaceRef,
    (flags, allowed): (&McpFlags, &[String]),
    (snapshot, bindings): (&[(String, String)], &McpBindings),
    configured: Vec<McpServer>,
    enabled: bool,
) -> McpSettings {
    let reader = Reader {
        files,
        workspace,
        snapshot,
        bindings,
    };
    // 显式给了 `--mcp-config` 的服务器：`-p` 的 prewait 缺省等到 `MCP_TIMEOUT`（ext.md §3.3.1）。
    let mut explicit = false;
    let servers = if !enabled {
        Vec::new()
    } else {
        let dynamic = reader.dynamic(&flags.configs).await;
        explicit = !dynamic.is_empty();
        let base = if flags.strict { Vec::new() } else { configured };
        let mut merged = std::collections::BTreeMap::new();
        for server in base.into_iter().chain(dynamic) {
            merged.insert(server.name.clone(), server);
        }
        let mut servers: Vec<McpServer> = merged.into_values().collect();
        bindings.bind(&mut servers);
        servers
    };
    let defaults = McpSettings::default();
    let connect_timeout_ms = positive_ms(snapshot, "MCP_TIMEOUT", defaults.connect_timeout_ms);
    McpSettings {
        servers,
        connect_timeout_ms,
        tool_timeout_ms: number(snapshot, "MCP_TOOL_TIMEOUT").unwrap_or(defaults.tool_timeout_ms),
        max_output_tokens: number(snapshot, "MAX_MCP_OUTPUT_TOKENS")
            .unwrap_or(defaults.max_output_tokens),
        // 握手的 `clientInfo.version`：入口的版本是唯一来源（docs/specs/rust-cli/version.md §4）。
        client_version: crate::VERSION.to_owned(),
        startup: startup(snapshot, (explicit, flags), connect_timeout_ms, allowed),
    }
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
