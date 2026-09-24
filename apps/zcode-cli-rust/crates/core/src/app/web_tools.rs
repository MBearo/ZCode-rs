//! WebFetch as a core built-in: the page comes from the tool port, the answer
//! from the run's model (Node passes `context.model`). Spec rust-m5-tools §3.
use crate::contract::{EventSink, ModelPort, ToolError, ToolOutput, ToolPort};
use crate::domain::web::{self, FetchRequest, Fetched, Page};
use crate::domain::{js_string, permission::webfetch_preapproved};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const INVALID_URL: &str = r#"[ { "validation": "url", "code": "invalid_string", "message": "Invalid url", "path": [ "url" ] } ]"#;

/// zod's `received` word for a JSON value.
fn received(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Node `InputValidationError` for WebFetch's string keys (unknown keys are
/// dropped: the schema is not strict), then zod `.url()`.
fn input(args: &Value) -> Result<(&str, &str)> {
    let mut missing = vec![];
    let mut wrong = vec![];
    for key in ["url", "prompt"] {
        match args.get(key) {
            None => missing.push(format!("The required parameter `{key}` is missing")),
            Some(Value::String(_)) => {}
            Some(other) => wrong.push(format!(
                "The parameter `{key}` type is expected as `string` but provided as `{}`",
                received(other)
            )),
        }
    }
    let lines: Vec<String> = missing.into_iter().chain(wrong).collect();
    if !lines.is_empty() {
        let noun = if lines.len() > 1 { "issues" } else { "issue" };
        let body = lines.join("\n");
        return Err(ToolError::Rendered(format!(
            "<tool_use_error>InputValidationError: WebFetch failed due to the following {noun}:\n{body}</tool_use_error>"
        ))
        .into());
    }
    let (url, prompt) = (
        args["url"].as_str().unwrap(),
        args["prompt"].as_str().unwrap(),
    );
    if !web::url::zod_url(url) {
        // 处理器内的 zod 解析失败：模型读到折叠空白后的 ZodError JSON。
        return Err(anyhow::Error::msg(INVALID_URL));
    }
    Ok((url, prompt))
}

/// Node `processFetchedContent`.
async fn process(
    model: &dyn ModelPort,
    page: &Page,
    prompt: &str,
    preapproved: bool,
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<(String, bool)> {
    if web::text::direct_markdown(preapproved, &page.content_type, &page.content) {
        return Ok((page.content.clone(), false));
    }
    let (content, truncated) = web::html::truncate_for_model(&page.content);
    let message = web::text::processing_prompt(&content, prompt, preapproved);
    let auxiliary = model.auxiliary();
    let model = auxiliary.as_deref().unwrap_or(model);
    let messages = vec![json!({"role": "user", "content": message})];
    let output =
        super::context::hidden_request(model, messages, sink, "web_fetch_processing", cancel)
            .await
            .map_err(|error| {
                let message = error.to_string();
                let message = if message.trim().is_empty() {
                    web::text::PROCESSING_FAILED.to_owned()
                } else {
                    message
                };
                anyhow::Error::from(web::WebError::new("webfetch_processing_failed", message))
            })?;
    let text = js_string::trim(output.message["content"].as_str().unwrap_or(""));
    let result = if text.is_empty() {
        web::text::EMPTY_RESULT
    } else {
        text
    };
    Ok((result.to_owned(), truncated))
}

async fn fetch(
    tools: &dyn ToolPort,
    model: &dyn ModelPort,
    (url, prompt): (&str, &str),
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<ToolOutput> {
    let started = Instant::now();
    let request = FetchRequest {
        session: sink.session_id.clone(),
        url: url.to_owned(),
        trace_id: Some(sink.origin.trace_id.clone()),
    };
    let fetched = tools.web_fetch(&request, cancel).await?;
    let elapsed = |started: Instant| started.elapsed().as_millis() as u64;
    let (result, data) = match fetched {
        Fetched::Redirect {
            original_url,
            redirect_url,
            redirects,
            status,
        } => {
            let status_text = web::text::status_text(status, "");
            let result =
                web::text::redirect(&original_url, &redirect_url, status, &status_text, prompt);
            let data = json!({"url": url, "finalUrl": original_url, "status": status,
                "statusText": status_text, "contentType": "text/plain", "bytes": result.len(),
                "durationMs": elapsed(started), "result": result, "cacheHit": false,
                "redirects": redirects, "truncated": false});
            (result, data)
        }
        Fetched::HttpError {
            final_url,
            redirects,
            retry_after,
            status,
        } => {
            let status_text = web::text::status_text(status, "");
            let result = web::text::http_error(status, &status_text, retry_after.as_deref());
            let data = json!({"url": url, "finalUrl": final_url, "status": status,
                "statusText": status_text, "contentType": "text/plain", "bytes": 0,
                "durationMs": elapsed(started), "result": result, "cacheHit": false,
                "redirects": redirects, "truncated": false});
            (result, data)
        }
        Fetched::Page(page) => {
            let preapproved = webfetch_preapproved(url);
            let (result, truncated) =
                process(model, &page, prompt, preapproved, sink, cancel).await?;
            let mut data = json!({"url": url, "finalUrl": page.final_url, "status": page.status,
                "statusText": web::text::status_text(page.status, ""), "contentType": page.content_type,
                "bytes": page.bytes, "durationMs": elapsed(started), "result": result,
                "cacheHit": page.cache_hit, "redirects": page.redirects, "truncated": truncated});
            if let Some(path) = &page.artifact_path {
                data["artifactPath"] = path.clone().into();
            }
            (result, data)
        }
    };
    let mut output = ToolOutput::text(result);
    output.data = data;
    Ok(output)
}

/// Node `webFetchToolEntry`: 60 s for the whole call, a fixed cancel text.
pub(super) async fn web_fetch(
    tools: &dyn ToolPort,
    model: &dyn ModelPort,
    args: &Value,
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<ToolOutput> {
    let input = input(args)?;
    let deadline = Duration::from_millis(web::TIMEOUT_MS);
    tokio::select! {
        _ = cancel.cancelled() => bail!(web::text::CANCELLED),
        _ = tokio::time::sleep(deadline) => bail!("Tool execution timed out after {}ms", web::TIMEOUT_MS),
        result = fetch(tools, model, input, sink, cancel) => result,
    }
}
