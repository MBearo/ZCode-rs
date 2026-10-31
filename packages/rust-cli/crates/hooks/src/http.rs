//! http hook（docs/specs/rust-cli/hooks.md §5.2）：执行的语义部分，网络经
//! [`HookHttp`] 端口。

use rust_cli_base::proto::{http, mime};
use rust_cli_kernel_api::hooks::{HookHttp, HookHttpError, HookHttpRequest};
use tokio_util::sync::CancellationToken;

use crate::config::HttpHook;
use crate::result::Raw;

/// 带花括号的变量引用的开头（`${NAME}`；不带花括号是 `$NAME`）。
const BRACED_OPEN: &str = "${";

/// URL 允许表的通配（`*` 任意串，其余按字面）。
fn url_matches(url: &str, pattern: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return url == pattern;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if url.len() < first.len() + last.len() || !url.starts_with(first) || !url.ends_with(last) {
        return false;
    }
    let mut at = first.len();
    let end = url.len() - last.len();
    for part in &parts[1..parts.len() - 1] {
        match url[at..end].find(part) {
            Some(found) => at += found + part.len(),
            None => return false,
        }
    }
    true
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_uppercase() || c == '_'
}

fn is_name(c: char) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'
}

/// 环境变量插值：`${NAME}` / `$NAME`（大写名）只替换允许的变量，其余为空；
/// 结果去掉 CR、LF、NUL。
pub(crate) fn interpolate(value: &str, allowed: &[String], env: &[(String, String)]) -> String {
    let lookup = |name: &str| {
        if !allowed.iter().any(|known| known == name) {
            tracing::warn!(name, "hook header env var is not in allowedEnvVars");
            return String::new();
        }
        rust_cli_base::secret::env_var(env, name)
            .map(str::to_owned)
            .unwrap_or_default()
    };
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '$' {
            let braced = chars.get(index + 1) == Some(&'{');
            let start = index + if braced { BRACED_OPEN.len() } else { 1 };
            if chars.get(start).copied().is_some_and(is_name_start) {
                let mut end = start;
                while chars.get(end).copied().is_some_and(is_name) {
                    end += 1;
                }
                let closed = !braced || chars.get(end) == Some(&'}');
                if closed {
                    let name: String = chars[start..end].iter().collect();
                    out.push_str(&lookup(&name));
                    index = end + usize::from(braced);
                    continue;
                }
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out.retain(|c| !matches!(c, '\r' | '\n' | '\0'));
    out
}

pub(crate) struct HttpPolicy<'a> {
    pub allowed_urls: Option<&'a [String]>,
    pub allowed_env_vars: Option<&'a [String]>,
    pub env: &'a [(String, String)],
}

pub(crate) async fn run(
    port: Option<&dyn HookHttp>,
    hook: &HttpHook,
    input: &str,
    timeout_ms: u64,
    policy: &HttpPolicy<'_>,
    cancel: CancellationToken,
) -> Raw {
    if let Some(patterns) = policy.allowed_urls
        && !patterns
            .iter()
            .any(|pattern| url_matches(&hook.url, pattern))
    {
        return Raw::Failed(format!(
            "HTTP hook blocked: {} does not match any pattern in allowedHttpHookUrls",
            hook.url
        ));
    }
    let Some(port) = port else {
        return Raw::Failed("HTTP hooks are not available".to_owned());
    };
    let allowed: Vec<String> = match policy.allowed_env_vars {
        Some(policy) => hook
            .allowed_env_vars
            .iter()
            .filter(|name| policy.contains(name))
            .cloned()
            .collect(),
        None => hook.allowed_env_vars.clone(),
    };
    // header 名写小写（base 的 `proto` 的写法；HTTP 不分大小写，发送时本来就规范化成小写，F58）。同名（不分大小写）的
    // 配置值盖掉缺省值（例如 `Content-Type`）。
    let mut headers = vec![(http::header::CONTENT_TYPE.to_owned(), mime::JSON.to_owned())];
    for (name, value) in &hook.headers {
        let value = interpolate(value, &allowed, policy.env);
        match headers
            .iter_mut()
            .find(|(known, _)| known.eq_ignore_ascii_case(name))
        {
            Some(slot) => slot.1 = value,
            None => headers.push((name.clone(), value)),
        }
    }
    let request = HookHttpRequest {
        url: hook.url.clone(),
        headers,
        body: input.to_owned(),
        timeout_ms,
    };
    match port.post(request, cancel).await {
        Ok(response) => Raw::Http {
            status: response.status,
            body: response.body,
        },
        Err(HookHttpError::Cancelled | HookHttpError::TimedOut) => Raw::Cancelled,
        Err(HookHttpError::Failed { message }) => Raw::Failed(message),
    }
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
