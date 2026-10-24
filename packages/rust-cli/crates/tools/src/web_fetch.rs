//! WebFetch（docs/specs/rust-cli/tools-more.md §4）。
//!
//! 取页面经执行环境的 `fetch_url`（一次 GET）；重定向策略、HTML 转换、缓存与旁路模型请求在这里。
//! 权限：工具不给意见，判决链按 `domain:` 规则与预批准域名判断（policy `chain::web_fetch`）。

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::env_web::FetchUrl;
use rust_cli_kernel_api::model::{ModelPort, ModelSelection};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, Validation,
};
use rust_cli_policy::web_preapproved::{host_path, preapproved};
use serde_json::Value;
use tokio::sync::mpsc;
use url::Url;

use crate::common::{Definition, definition, failed, text};
use crate::web_cache::{Page, PageCache};
use crate::web_side::apply_prompt;
use crate::web_texts as texts;

pub const NAME: &str = "WebFetch";

/// URL 最长 2000 个字符。
const MAX_URL: usize = 2000;
/// 重定向的上限。
const MAX_REDIRECTS: usize = 10;
/// 请求超时。
const TIMEOUT_MS: u64 = 60_000;
/// 响应正文的长度上限。
const MAX_BYTES: u64 = 10 * 1024 * 1024;
/// 转换前只取前 1MB 的 HTML。
const HTML_LIMIT: usize = 1_048_576;
/// 交给旁路模型的正文上限，预批准的 markdown 原样返回的上限。
pub(crate) const APPLY_LIMIT: usize = 100_000;

/// 旁路模型请求（用入口给的模型，§7）。
#[derive(Clone)]
pub struct SideModel {
    pub model: Arc<dyn ModelPort>,
    pub selection: ModelSelection,
}

pub struct WebFetchTool {
    definition: Definition,
    side: Option<SideModel>,
    cache: Mutex<PageCache>,
    /// 页面缓存的过期按它算（F7：内核不用 `Instant`）。
    clock: Arc<dyn Clock>,
}

impl WebFetchTool {
    pub fn new(side: Option<SideModel>, clock: Arc<dyn Clock>) -> Self {
        Self {
            definition: definition(NAME),
            side,
            cache: Mutex::default(),
            clock,
        }
    }

    fn cached(&self, url: &str) -> Option<Page> {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(url, self.clock.now_ms())
    }

    fn remember(&self, url: &str, page: &Page) {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .put(url, page.clone(), self.clock.now_ms());
    }
}

/// 一次取页面的结果（三种返回）。
enum Fetched {
    Page(Page),
    Redirect {
        from: String,
        to: String,
        code: u16,
    },
    HttpError {
        code: u16,
        retry_after: Option<String>,
    },
}

/// URL 校验：长度、用户信息、至少两段的主机名。
fn check_url(raw: &str) -> Result<Url, ToolError> {
    let parsed = Url::parse(raw).map_err(|_| failed(texts::INVALID_URL.to_owned()))?;
    let host = parsed.host_str().unwrap_or_default();
    if !host.is_empty() && !host.contains('.') {
        return Err(failed(texts::DOTLESS_HOST.to_owned()));
    }
    let credentials = !parsed.username().is_empty() || parsed.password().is_some();
    if raw.chars().count() > MAX_URL || credentials || host.split('.').count() < 2 {
        return Err(failed(texts::INVALID_URL.to_owned()));
    }
    Ok(parsed)
}

fn bare(host: &str) -> &str {
    host.strip_prefix("www.").unwrap_or(host)
}

/// 可以自动跟随的重定向（同协议、同端口、去掉 `www.` 后同主机、没有用户信息；预批准的路径
/// 不能被重定向到非预批准的路径）。
fn same_site(from: &Url, to: &Url) -> bool {
    if from.scheme() != to.scheme() || from.port() != to.port() {
        return false;
    }
    let host = from.host_str().unwrap_or_default();
    let base = bare(host);
    let variants = [host.to_owned(), base.to_owned(), format!("www.{base}")];
    if variants.iter().any(|v| host_path(v, from.path()))
        && !variants.iter().any(|v| host_path(v, to.path()))
    {
        return false;
    }
    if !to.username().is_empty() || to.password().is_some() {
        return false;
    }
    bare(to.host_str().unwrap_or_default()) == base
}

async fn fetch(context: &ToolUseContext, start: Url) -> Result<Fetched, ToolError> {
    let mut url = start;
    for _ in 0..=MAX_REDIRECTS {
        let request = FetchUrl {
            url: url.to_string(),
            headers: vec![(
                // 小写，同 net 的写法（HTTP 不分大小写，线上字节不变，F58）。
                "accept".to_owned(),
                "text/markdown, text/html, */*".to_owned(),
            )],
            timeout_ms: TIMEOUT_MS,
            max_bytes: MAX_BYTES,
        };
        let reply = context
            .local
            .fetch()
            .fetch_url(&context.workspace, request)
            .await
            .map_err(|error| failed(error.to_string()))?;
        let code = reply.status;
        if (200..300).contains(&code) {
            let raw = String::from_utf8_lossy(&reply.body).into_owned();
            let content = if reply.content_type.contains("text/html") {
                let truncated = raw.chars().count() > HTML_LIMIT;
                let head: String = raw.chars().take(HTML_LIMIT).collect();
                let mut markdown = crate::html_markdown::to_markdown(&head);
                if truncated {
                    markdown.push_str(texts::CONTENT_TRUNCATED);
                }
                markdown
            } else {
                raw
            };
            return Ok(Fetched::Page(Page {
                code,
                content,
                content_type: reply.content_type,
            }));
        }
        if matches!(code, 301 | 302 | 303 | 307 | 308) {
            let target = reply
                .location
                .filter(|location| !location.trim().is_empty())
                .and_then(|location| url.join(&location).ok());
            let Some(target) = target else {
                return Ok(Fetched::HttpError {
                    code,
                    retry_after: None,
                });
            };
            if !same_site(&url, &target) {
                return Ok(Fetched::Redirect {
                    from: url.to_string(),
                    to: target.to_string(),
                    code,
                });
            }
            url = target;
            continue;
        }
        let retry_after = reply.retry_after.filter(|value| {
            (1..=6).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit())
        });
        return Ok(Fetched::HttpError { code, retry_after });
    }
    Err(failed(texts::too_many_redirects(MAX_REDIRECTS)))
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        NAME
    }

    fn input_schema(&self) -> Value {
        self.definition.schema.clone()
    }

    fn description(&self, _context: &PromptContext) -> String {
        self.definition.description.clone()
    }

    fn should_defer(&self) -> bool {
        true
    }

    fn search_hint(&self) -> Option<&str> {
        Some("fetch and extract content from a URL")
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    /// 工具结果的缺省上限。
    fn max_result_size_chars(&self) -> Option<usize> {
        Some(50_000)
    }

    async fn validate_input(&self, input: &Value, _context: &ToolUseContext) -> Validation {
        let url = input.get("url").and_then(Value::as_str).unwrap_or_default();
        if Url::parse(url).is_err() {
            return Validation::Invalid {
                message: texts::invalid_url(url),
                error_code: Some(1),
            };
        }
        Validation::Ok
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let raw = input.get("url").and_then(Value::as_str).unwrap_or_default();
        let prompt = input
            .get("prompt")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let offset = input.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let mut url = check_url(raw)?;
        let page = match self.cached(raw) {
            Some(page) => page,
            None => {
                if url.scheme() == "http" {
                    let _ = url.set_scheme("https");
                }
                let fetched = tokio::select! {
                    fetched = fetch(&context, url) => fetched?,
                    () = context.cancel.cancelled() => return Err(ToolError::Cancelled),
                };
                match fetched {
                    Fetched::Page(page) => {
                        self.remember(raw, &page);
                        page
                    }
                    Fetched::Redirect { from, to, code } => {
                        return Ok(text(texts::redirect(&from, &to, code, prompt, offset)));
                    }
                    Fetched::HttpError { code, retry_after } => {
                        return Ok(text(texts::http_error(code, retry_after.as_deref())));
                    }
                }
            }
        };
        let content: Vec<char> = page.content.chars().collect();
        let skip = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(content.len());
        let rest: String = content[skip..].iter().collect();
        if offset > 0 && rest.is_empty() {
            return Ok(text(texts::past_end(offset, content.len())));
        }
        let approved = preapproved(raw);
        let rest_length = content.len() - skip;
        if approved && page.content_type.contains("text/markdown") && rest_length < APPLY_LIMIT {
            return Ok(text(rest));
        }
        let lead = if offset > 0 {
            texts::content_lead(offset, content.len())
        } else {
            String::new()
        };
        let mut answer =
            apply_prompt(self.side.as_ref(), prompt, &rest, approved, &lead, &context).await?;
        let end = skip + rest_length.min(APPLY_LIMIT);
        if end < content.len() {
            answer.push_str(&texts::partial_note(content.len(), skip, end));
        }
        Ok(text(answer))
    }
}

#[cfg(test)]
#[path = "web_fetch_tests.rs"]
mod tests;
