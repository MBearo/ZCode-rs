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

/// Schema check (Node InputValidationError), then zod `.url()` in the handler.
fn input(args: &Value) -> Result<(&str, &str)> {
    web::fetch_validation(args).map_err(ToolError::Rendered)?;
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
    let output = super::context::hidden_request(
        model,
        (messages, &[]),
        sink,
        "web_fetch_processing",
        cancel,
    )
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

/// Node `ModelUsage` of an internal request (token counts only).
fn model_usage(usage: &Value) -> Value {
    let input = usage["prompt_tokens"].as_u64();
    let output = usage["completion_tokens"].as_u64();
    if input.is_none() && output.is_none() {
        return Value::Null;
    }
    let (input, output) = (input.unwrap_or(0), output.unwrap_or(0));
    json!({"inputTokens": input, "outputTokens": output, "totalTokens": input + output})
}

async fn search(
    model: &dyn ModelPort,
    args: &Value,
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<ToolOutput> {
    let started = Instant::now();
    let query = args["query"].as_str().unwrap_or_default();
    let messages = vec![
        json!({"role": "system", "content": web::search::SYSTEM}),
        json!({"role": "user", "content": web::search::user_message(query)}),
    ];
    let tools = [web::search::provider_tool(args)];
    let auxiliary = model.auxiliary();
    let model = auxiliary.as_deref().unwrap_or(model);
    let output =
        super::context::hidden_request(model, (messages, &tools), sink, "web_search_tool", cancel)
            .await?;
    let text = output.message["content"].as_str().unwrap_or("");
    let elapsed = started.elapsed().as_millis() as u64;
    let data = web::search::output(query, text, model_usage(&output.usage), elapsed);
    let content = crate::domain::persisted_output::truncate(
        &web::search::model_content(&data),
        web::search::MODEL_BYTES,
    );
    let mut result = ToolOutput::text(content);
    result.data = data;
    Ok(result)
}

/// Node `webSearchToolEntry`: a provider-native search through the run's
/// model (lowest reasoning level), 60 s, fixed cancel text.
pub(super) async fn web_search(
    model: &dyn ModelPort,
    args: &Value,
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<ToolOutput> {
    web::search::validate(args).map_err(ToolError::Rendered)?;
    if !model.supports_native_web_search() {
        bail!(web::search::UNSUPPORTED);
    }
    let deadline = Duration::from_millis(web::TIMEOUT_MS);
    tokio::select! {
        _ = cancel.cancelled() => bail!(web::search::CANCELLED),
        _ = tokio::time::sleep(deadline) => bail!("Tool execution timed out after {}ms", web::TIMEOUT_MS),
        result = search(model, args, sink, cancel) => result,
    }
}
