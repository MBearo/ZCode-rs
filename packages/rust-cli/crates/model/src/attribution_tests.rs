//! 每请求头的取值（工作线 F64，model.md §2.4 的第二张表与用途表）。

use rust_cli_kernel_api::ids::{AgentId, RunId, SessionId};
use rust_cli_kernel_api::model::{ModelRequest, QuerySource, RequestOptions};

use super::*;

fn request(source: QuerySource, options: RequestOptions) -> ModelRequest {
    ModelRequest {
        selection: rust_cli_kernel_api::model::ModelSelection {
            provider: "p".to_owned(),
            model: "m".to_owned(),
            reasoning: None,
            connection: None,
        },
        system: Vec::new(),
        messages: Vec::new(),
        tools: Vec::new(),
        tool_choice: None,
        max_tokens: 1,
        thinking: None,
        temperature: None,
        betas: Vec::new(),
        source,
        options,
    }
}

fn names(headers: &[(&'static str, String)]) -> Vec<&'static str> {
    headers.iter().map(|(name, _)| *name).collect()
}

/// 用途名：Node 有对应用途的用 Node 的名字。
#[test]
fn query_sources_use_the_node_names() {
    for (source, expected) in [
        (QuerySource::MainThread, "main_turn"),
        (QuerySource::Subagent("Explore".to_owned()), "subagent"),
        (QuerySource::Compact, "compact"),
        (
            QuerySource::Side("generate_session_title".to_owned()),
            "session_title",
        ),
        (
            QuerySource::Side("web_fetch_apply".to_owned()),
            "web_fetch_processing",
        ),
        (
            QuerySource::Side("web_search_tool".to_owned()),
            "web_search_tool",
        ),
        (QuerySource::Side("hook_prompt".to_owned()), "hook_prompt"),
        (
            QuerySource::Side("rename_generate_name".to_owned()),
            "rename_generate_name",
        ),
    ] {
        assert_eq!(source.node_name(), expected);
    }
}

/// 主会话的回合：会话、回合去掉内部前缀，键序同 Node 的对象。
#[test]
fn a_main_turn_has_every_header() {
    let options = RequestOptions {
        session_id: Some(SessionId::new("sess_s1")),
        run: Some(RunId::new("run_r1")),
        ..RequestOptions::default()
    };
    let origin = Origin::of(&request(QuerySource::MainThread, options));
    let headers = headers(
        &origin,
        "https://api.z.ai/api/anthropic",
        "t",
        "q".to_owned(),
    );
    assert_eq!(
        headers,
        [
            (REQUEST_ID, "q".to_owned()),
            (SESSION_TYPE, "main".to_owned()),
            (TRACE_ID, "t".to_owned()),
            (QUERY_ID, "r1".to_owned()),
            (QUERY_SOURCE, "main_turn".to_owned()),
            (SESSION_ID, "s1".to_owned()),
        ]
    );
}

/// 子代理（含子代理里的工具发的旁路请求）是 `subagent`；没有会话是 `other`；没有回合时不发 `x-query-id`；前缀去掉后为空时
/// 不去（Node `stripHeaderInternalPrefixes`）。
#[test]
fn session_types_and_missing_parts() {
    let child = RequestOptions {
        session_id: Some(SessionId::new("sess_subagent_agent_a1")),
        agent: Some(AgentId::new("agent_a1")),
        ..RequestOptions::default()
    };
    let origin = Origin::of(&request(
        QuerySource::Side("web_search_tool".to_owned()),
        child,
    ));
    let headers = headers(&origin, "http://127.0.0.1:9", "t", "q".to_owned());
    assert_eq!(headers[1], (SESSION_TYPE, "subagent".to_owned()));
    assert_eq!(headers.last(), Some(&(SESSION_ID, "a1".to_owned())));
    assert!(!names(&headers).contains(&QUERY_ID));

    let none = headers_of(&Origin::none(), "http://127.0.0.1:9");
    assert_eq!(names(&none), [REQUEST_ID, SESSION_TYPE, TRACE_ID]);
    assert_eq!(none[1].1, "other");

    let bare = RequestOptions {
        session_id: Some(SessionId::new("sess_")),
        run: Some(RunId::new("run_")),
        ..RequestOptions::default()
    };
    let origin = Origin::of(&request(QuerySource::MainThread, bare));
    let headers = headers_of(&origin, "http://127.0.0.1:9");
    assert!(headers.contains(&(SESSION_ID, "sess_".to_owned())));
    assert!(headers.contains(&(QUERY_ID, "run_".to_owned())));
}

fn headers_of(origin: &Origin, base_url: &str) -> Vec<(&'static str, String)> {
    headers(origin, base_url, "t", "q".to_owned())
}

/// 不合 `^[A-Za-z0-9_.:-]{1,128}$` 的用途名不发；OpenCode Go 的端点另带会话头。
#[test]
fn invalid_sources_are_dropped_and_opencode_gets_the_session() {
    for name in ["has space", "", &"a".repeat(129), "中文"] {
        let origin = Origin::of(&request(
            QuerySource::Side(name.to_owned()),
            RequestOptions::default(),
        ));
        assert!(
            !names(&headers_of(&origin, "http://127.0.0.1:9")).contains(&QUERY_SOURCE),
            "{name}"
        );
    }
    let origin = Origin::of(&request(
        QuerySource::Side("a.b:c-d_1".to_owned()),
        RequestOptions::default(),
    ));
    assert!(
        headers_of(&origin, "http://127.0.0.1:9").contains(&(QUERY_SOURCE, "a.b:c-d_1".to_owned()))
    );

    let options = RequestOptions {
        session_id: Some(SessionId::new("sess_s1")),
        ..RequestOptions::default()
    };
    let origin = Origin::of(&request(QuerySource::MainThread, options));
    let headers = headers_of(&origin, "https://opencode.ai/zen/go/v1");
    assert_eq!(headers.last(), Some(&(OPENCODE_SESSION, "s1".to_owned())));
    assert!(!names(&headers_of(&origin, "https://opencode.ai/zen/v1")).contains(&OPENCODE_SESSION));
}

fn golden() -> serde_json::Value {
    serde_json::from_str(include_str!("../tests/fixtures/attribution-golden.json")).unwrap()
}

/// 与 Node 的对照夹具逐项相同（`scripts/rust-cli/headers-golden.mjs` 用 Node 的 `createModelRequestAttributionHeaders`
/// 生成：会话的前缀、回合（`run_<x>` 对应 Node 的 `query_<x>`）、子代理、用途名与它的校验、OpenCode Go 的会话头、键序）。
#[test]
fn request_headers_match_node() {
    let golden = golden();
    let cases = golden["attribution"].as_array().unwrap();
    assert!(cases.len() >= 50);
    for case in cases {
        let text = |key: &str| case[key].as_str().map(str::to_owned);
        let source = match case["source"]["kind"].as_str().unwrap() {
            "main_thread" => QuerySource::MainThread,
            "subagent" => {
                QuerySource::Subagent(case["source"]["name"].as_str().unwrap().to_owned())
            }
            "compact" => QuerySource::Compact,
            _ => QuerySource::Side(case["source"]["name"].as_str().unwrap().to_owned()),
        };
        let options = RequestOptions {
            session_id: text("session").map(SessionId::new),
            run: text("run").map(RunId::new),
            agent: text("agent").map(AgentId::new),
            ..RequestOptions::default()
        };
        let origin = Origin::of(&request(source, options));
        let ours = headers(
            &origin,
            case["baseUrl"].as_str().unwrap(),
            "trace-1",
            "req-1".to_owned(),
        );
        let node: Vec<(&str, String)> = case["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| {
                (
                    pair[0].as_str().unwrap(),
                    pair[1].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        let ours: Vec<(&str, String)> = ours.into_iter().collect();
        assert_eq!(ours, node, "{case}");
    }
}
