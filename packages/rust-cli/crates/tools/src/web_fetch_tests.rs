use std::sync::Arc;

use rust_cli_kernel_api::env_web::FetchedUrl;
use rust_cli_kernel_api::model::{ApiRole, QuerySource};
use rust_cli_kernel_api::testing::{
    FixedClock, ScriptedInteractions, ScriptedModel, selection, text_response,
};
use rust_cli_kernel_api::tool::{Tool, ToolError, Validation};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::*;
use crate::test_more::{Ctx, TextEnv, output_text};

fn page(status: u16, content_type: &str, body: &str) -> FetchedUrl {
    FetchedUrl {
        status,
        content_type: content_type.to_owned(),
        location: None,
        retry_after: None,
        body: body.as_bytes().to_vec(),
    }
}

fn moved(status: u16, location: &str) -> FetchedUrl {
    FetchedUrl {
        location: Some(location.to_owned()),
        ..page(status, "", "")
    }
}

struct Run {
    env: Arc<TextEnv>,
    model: Arc<ScriptedModel>,
    tool: WebFetchTool,
}

fn setup(pages: Vec<(&str, FetchedUrl)>, replies: &[&str]) -> Run {
    let model = Arc::new(ScriptedModel::new(
        replies
            .iter()
            .map(|reply| text_response(&[reply]))
            .collect(),
    ));
    let side = SideModel {
        model: model.clone(),
        selection: selection(),
    };
    Run {
        env: Arc::new(TextEnv::web(pages)),
        tool: WebFetchTool::new(Some(side), Arc::new(FixedClock(0))),
        model,
    }
}

async fn call(run: &Run, input: Value) -> Result<String, ToolError> {
    let mut context = Ctx::new(Arc::new(ScriptedInteractions::default()));
    context.set_env(run.env.clone());
    let (progress, _) = mpsc::channel(1);
    run.tool
        .call(input, context.build(), progress)
        .await
        .map(|output| output_text(&output))
}

#[tokio::test]
async fn html_is_converted_and_answered_by_the_side_model() {
    let run = setup(
        vec![(
            "https://example.com/doc",
            page(
                200,
                "text/html; charset=utf-8",
                "<h3>Install</h3><p>Run <code>make</code>.</p>",
            ),
        )],
        &["Run make.", "Again."],
    );
    let answer = call(
        &run,
        json!({"url": "http://example.com/doc", "prompt": "How?"}),
    )
    .await
    .unwrap();
    assert_eq!(answer, "Run make.");
    // http 升级为 https；旁路请求的来源与正文固定。
    assert_eq!(run.env.fetched(), ["https://example.com/doc"]);
    let request = &run.model.requests()[0];
    assert_eq!(
        request.source,
        QuerySource::Side("web_fetch_apply".to_owned())
    );
    // 工作线 F52：带上工具所在的会话（`metadata.user_id` 的 `session_id`，model.md §3.3）。
    assert_eq!(
        request.options.session_id,
        Some(rust_cli_kernel_api::ids::SessionId::new("sess_1"))
    );
    // 工作线 F64：工具所在的回合（每请求头的 `x-query-id`，model.md §2.4）。
    assert!(request.options.run.is_some());
    assert!(request.system.is_empty() && request.tools.is_empty());
    assert_eq!(request.messages[0].role, ApiRole::User);
    let sent = format!("{:?}", request.messages[0].content);
    assert!(
        sent.contains("Web page content:\\n---\\n### Install\\n\\nRun `make`.\\n---\\n\\nHow?")
    );
    assert!(sent.contains("Enforce a strict 125-character maximum"));
    // 第二次在缓存期内：不再取页面，仍按 prompt 走旁路模型。
    let again = call(
        &run,
        json!({"url": "http://example.com/doc", "prompt": "How?"}),
    )
    .await
    .unwrap();
    assert_eq!(again, "Again.");
    assert_eq!(run.env.fetched().len(), 1);
}

#[tokio::test]
async fn same_site_redirects_follow_and_cross_site_redirects_return() {
    let run = setup(
        vec![
            (
                "https://example.com/a",
                moved(301, "https://www.example.com/b"),
            ),
            ("https://www.example.com/b", page(200, "text/plain", "body")),
            (
                "https://example.com/c",
                moved(302, "https://other.example.org/d"),
            ),
        ],
        &["ok"],
    );
    assert_eq!(
        call(&run, json!({"url": "https://example.com/a", "prompt": "p"}))
            .await
            .unwrap(),
        "ok"
    );
    let redirect = call(&run, json!({"url": "https://example.com/c", "prompt": "p"}))
        .await
        .unwrap();
    assert!(redirect.starts_with(
        "REDIRECT DETECTED: The URL redirects to a location that was not fetched automatically."
    ));
    assert!(redirect.contains("Status: 302 Found"));
    assert!(redirect.contains("- url: \"https://other.example.org/d\""));
}

#[tokio::test]
async fn http_errors_and_bad_urls_are_reported() {
    let run = setup(
        vec![(
            "https://example.com/private",
            FetchedUrl {
                retry_after: Some("30".to_owned()),
                ..page(429, "text/html", "slow down")
            },
        )],
        &[],
    );
    let error = call(
        &run,
        json!({"url": "https://example.com/private", "prompt": "p"}),
    )
    .await
    .unwrap();
    assert!(
        error.starts_with("The server returned HTTP 429 Too Many Requests.\nRetry-After: 30\n\n")
    );
    let local = call(
        &run,
        json!({"url": "http://localhost:3000/x", "prompt": "p"}),
    )
    .await;
    assert_eq!(
        local,
        Err(ToolError::Failed {
            message: texts::DOTLESS_HOST.to_owned()
        })
    );
    let context = Ctx::new(Arc::new(ScriptedInteractions::default())).build();
    assert_eq!(
        run.tool
            .validate_input(&json!({"url": "not a url", "prompt": "p"}), &context)
            .await,
        Validation::Invalid {
            message: texts::invalid_url("not a url"),
            error_code: Some(1),
        }
    );
}

#[tokio::test]
async fn preapproved_markdown_is_returned_verbatim_and_offsets_page_through() {
    let run = setup(
        vec![
            (
                "https://docs.python.org/3/a.md",
                page(200, "text/markdown", "# Title\nbody"),
            ),
            (
                "https://example.com/long",
                page(200, "text/plain", "0123456789"),
            ),
        ],
        &["tail"],
    );
    assert_eq!(
        call(
            &run,
            json!({"url": "https://docs.python.org/3/a.md", "prompt": "p"})
        )
        .await
        .unwrap(),
        "# Title\nbody"
    );
    assert_eq!(
        call(
            &run,
            json!({"url": "https://example.com/long", "prompt": "p", "offset": 20})
        )
        .await
        .unwrap(),
        texts::past_end(20, 10)
    );
    assert_eq!(
        call(
            &run,
            json!({"url": "https://example.com/long", "prompt": "p", "offset": 4})
        )
        .await
        .unwrap(),
        "tail"
    );
    let sent = format!("{:?}", run.model.requests()[0].messages[0].content);
    assert!(sent.contains("it starts 4 characters into the page's 10."));
    assert!(sent.contains("---\\n456789\\n---"));
}

#[test]
fn web_fetch_is_deferred_and_read_only() {
    let tool = WebFetchTool::new(None, Arc::new(FixedClock(0)));
    assert!(tool.should_defer());
    assert!(tool.is_read_only(&json!({})));
    assert_eq!(
        tool.search_hint(),
        Some("fetch and extract content from a URL")
    );
}
