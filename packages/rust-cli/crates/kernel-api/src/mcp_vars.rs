//! MCP 服务器配置里的 `${VAR}` 与 `${VAR:-默认}` 展开（docs/specs/rust-cli/ext.md §3.1、remote-exec.md §22.7）。
//!
//! 只有一份规则：入口展开 `--mcp-config` 与插件的服务器，远端的执行器展开绑定到它的服务器（用执行器自己的
//! 环境，本机的值不发到远端）。纯函数，不做 IO；查变量的函数由调用方给。

use crate::mcp::McpTransportConfig;

/// 变量引用的开头。
const OPEN: &str = "${";

/// `${VAR}` 与 `${VAR:-默认}` 展开；没有值也没有默认值的变量原样保留，名字记进 `missing`。
pub fn expand_vars(
    text: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
    missing: &mut Vec<String>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        let after = &rest[start + OPEN.len()..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let inner = &after[..end];
        let (name, default) = match inner.split_once(":-") {
            Some((name, default)) => (name, Some(default)),
            None => (inner, None),
        };
        match lookup(name).or_else(|| default.map(str::to_owned)) {
            Some(value) => out.push_str(&value),
            None => {
                missing.push(name.to_owned());
                out.push_str(&rest[start..start + OPEN.len() + end + 1]);
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// 展开一个服务器的全部字符串字段：stdio 的 command、args、env 的值、cwd；HTTP / SSE 的 url 与 headers 的值。
/// 返回展开后的配置与缺的变量名（去重，按出现的先后）。
pub fn expand_transport_vars(
    config: &McpTransportConfig,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> (McpTransportConfig, Vec<String>) {
    let mut missing = Vec::new();
    let mut expand = |text: &str| expand_vars(text, lookup, &mut missing);
    let expanded = match config {
        McpTransportConfig::Stdio {
            command,
            args,
            env,
            cwd,
        } => McpTransportConfig::Stdio {
            command: expand(command),
            args: args.iter().map(|arg| expand(arg)).collect(),
            env: env
                .iter()
                .map(|(key, value)| (key.clone(), expand(value)))
                .collect(),
            cwd: cwd.as_deref().map(&mut expand),
        },
        McpTransportConfig::Sse { url, headers } => McpTransportConfig::Sse {
            url: expand(url),
            headers: headers
                .iter()
                .map(|(key, value)| (key.clone(), expand(value)))
                .collect(),
        },
        McpTransportConfig::Http { url, headers } => McpTransportConfig::Http {
            url: expand(url),
            headers: headers
                .iter()
                .map(|(key, value)| (key.clone(), expand(value)))
                .collect(),
        },
    };
    let mut seen = std::collections::HashSet::new();
    missing.retain(|name| seen.insert(name.clone()));
    (expanded, missing)
}

#[cfg(test)]
#[path = "mcp_vars_tests.rs"]
mod tests;
