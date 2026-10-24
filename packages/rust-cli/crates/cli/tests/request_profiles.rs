//! 修复线 F23（docs/specs/rust-cli/model.md §3.2、§6、§11）：真实 `zcode -p` 经显式覆盖（`ZCODE_MODEL_BASE_URL`）连本地
//! 夹具。夹具不是 Anthropic 官方 API，落到“其他 Anthropic 兼容”：第三方形状的 system、没有官方专有项，WebSearch
//! 不发（内置规则只对 `api.anthropic.com/v1` 上匹配 `.*claude-.*` 的模型打开 `supportsNativeWebSearch`）。只给测试用的
//! `_ZCODE_ASSUME_FIRST_PARTY_BASE_URL=1` 让同一个夹具按 Anthropic 官方处理。
//!
//! 工作线 F52（tools-more.md §3.2、model.md §3.3、platform.md §10）：每个请求带 `metadata.user_id`，设备身份与 App / Node
//! 共用 `telemetry-state.json`。工作线 F84（tools-more.md §3.2，推翻 F52 的“其他的不开”）：ToolSearch 所有端点都开——
//! z.ai / bigmodel 的端点原生（`defer_loading`、占位工具、`advanced-tool-use`），网关与官方地址本地（都不带）；只给测试用
//! 的 `_ZCODE_ASSUME_TOOL_SEARCH=1` 让夹具按原生处理。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具"
)]

mod system_env;
use system_env::KeepSystem as _;
mod sse;

use std::path::PathBuf;
use std::process::Stdio;

use serde_json::Value;

/// 一次运行的临时 home 与工作区。
struct Sandbox {
    _dir: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let (home, work) = (base.join("home"), base.join("work"));
        std::fs::create_dir_all(work.join(".git")).unwrap();
        std::fs::create_dir_all(home.join(".zcode/v2")).unwrap();
        Self {
            _dir: dir,
            home,
            work,
        }
    }

    fn telemetry_state(&self) -> PathBuf {
        self.home.join(".zcode/v2/telemetry-state.json")
    }

    /// 跑一次 `zcode -p hi`（不继承父进程的环境），返回第一个主请求的请求体与 `anthropic-beta`。
    fn first_request(&self, vars: &[(&str, &str)], args: &[&str]) -> (Value, Option<String>) {
        let fixture = sse::serve(vec![sse::text_reply("hello")]);
        let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
        command
            .args(["-p", "hi"])
            .args(args)
            .current_dir(&self.work)
            .env_clear()
            .keep_system()
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .env("ZCODE_DATA_BASE_DIR", &self.home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in vars {
            command.env(name, value.replace("<fixture>", &fixture.url));
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        (
            fixture.main_requests().remove(0),
            fixture.main_betas().remove(0),
        )
    }
}

/// 显式覆盖连夹具：`flags` 是只给测试用的声明。
fn first_request(flags: &[&str]) -> (Value, Option<String>) {
    let mut vars = vec![
        ("ZCODE_MODEL_BASE_URL", "<fixture>"),
        ("ZCODE_API_KEY", "test-key"),
        ("ZCODE_MODEL", "claude-opus-5-5"),
    ];
    vars.extend(flags.iter().map(|flag| (*flag, "1")));
    Sandbox::new().first_request(&vars, &[])
}

/// 请求里 ToolSearch 的形式（tools-more.md §3.2.2）：没有 ToolSearch 是关；有 ToolSearch、没有 `defer_loading`、占位工具与
/// `advanced-tool-use` 是本地；三样都有是原生。三样不一致时测试失败。
#[derive(Debug, PartialEq, Eq)]
enum Search {
    Off,
    Local,
    Native,
}

fn tool_search(body: &Value, beta: Option<&str>) -> Search {
    let tools = body["tools"].as_array().unwrap();
    let defer = tools.iter().any(|tool| tool.get("defer_loading").is_some());
    let placeholder = tools
        .iter()
        .any(|tool| tool["name"] == "DeferredToolPlaceholder");
    let search = tools.iter().any(|tool| tool["name"] == "ToolSearch");
    let advanced = beta.is_some_and(|beta| beta.contains("advanced-tool-use"));
    assert_eq!(defer, placeholder, "{body}");
    assert_eq!(defer, advanced, "{body} {beta:?}");
    match (search, defer) {
        (false, false) => Search::Off,
        (true, false) => Search::Local,
        (true, true) => Search::Native,
        (false, true) => panic!("deferred tools without ToolSearch: {body}"),
    }
}

/// 名字出现在消息里的延迟工具清单（每行一个名字），不在 `tools` 里。
fn listed_as_deferred(body: &Value, name: &str) -> bool {
    let text = body.to_string();
    text.contains(&format!("\\n{name}\\n")) && !text.contains(&format!("\"name\":\"{name}\""))
}

/// WebSearch 是延迟工具：出现在消息里的延迟工具清单（每行一个名字），不在 `tools` 里。
fn offers_web_search(body: &Value) -> bool {
    let text = body.to_string();
    text.contains("\\nWebSearch\\n") || text.contains("\"name\":\"WebSearch\"")
}

#[tokio::test]
async fn a_local_gateway_gets_the_third_party_shape() {
    let (body, beta) = first_request(&[]);
    let text = body.to_string();
    // 用户报的 Bedrock 网关错误就是 `system.1.cache_control.ephemeral.scope`。
    for field in [
        "scope",
        "diagnostics",
        "context_management",
        "thinking",
        "output_config",
    ] {
        assert!(!text.contains(&format!("\"{field}\"")), "{field}: {text}");
    }
    let system = body["system"].as_array().unwrap();
    assert_eq!(system.len(), 2, "{system:?}");
    for block in system {
        assert_eq!(
            block["cache_control"],
            serde_json::json!({"type": "ephemeral"})
        );
    }
    assert!(!offers_web_search(&body), "{text}");
    // 工作线 F84：网关上是本地 ToolSearch——WebFetch 在延迟清单里，没有 `defer_loading`、占位工具与 `advanced-tool-use`。
    // 修改：F52 到 F84 之间网关上不开 ToolSearch、WebFetch 内联；F52 之前按卡片开原生、请求带 `advanced-tool-use`。
    assert_eq!(tool_search(&body, beta.as_deref()), Search::Local, "{text}");
    assert!(listed_as_deferred(&body, "WebFetch"), "{text}");
    // 工作线 F64：兼容端点照 Node 发 beta——请求里有对话中间的 system 消息（环境段等），带 `mid-conversation-system`。
    assert_eq!(beta.as_deref(), Some("mid-conversation-system-2026-04-07"));
}

#[tokio::test]
#[cfg_attr(
    not(debug_assertions),
    ignore = "测试声明只在 debug 构建生效（工作线 F52b）"
)]
async fn the_test_declaration_gets_the_first_party_shape() {
    let (body, beta) = first_request(&[
        "_ZCODE_ASSUME_FIRST_PARTY_BASE_URL",
        "_ZCODE_ASSUME_TOOL_SEARCH",
    ]);
    let system = body["system"].as_array().unwrap();
    assert_eq!(system.len(), 3, "{system:?}");
    assert_eq!(system[1]["cache_control"]["scope"], "global");
    assert_eq!(body["diagnostics"]["previous_message_id"], Value::Null);
    assert!(body.get("context_management").is_some());
    assert_eq!(body["thinking"]["type"], "adaptive");
    assert!(offers_web_search(&body));
    assert_eq!(tool_search(&body, beta.as_deref()), Search::Native);
    // 官方地址本身是本地 ToolSearch（工作线 F84；F52 时不开），测试声明让它按原生发。
    let (body, beta) = first_request(&["_ZCODE_ASSUME_FIRST_PARTY_BASE_URL"]);
    assert_eq!(tool_search(&body, beta.as_deref()), Search::Local);
    assert!(offers_web_search(&body));
}

/// 工作线 F52b（model.md §6，用户决定 2026-10-22）：release 构建不读两个测试声明，设了也按网关发——第三方形状、本地
/// ToolSearch（没有 `defer_loading` 与 `advanced-tool-use`，发布版不留在网关上发原生的后门；F84 之前是不开）。只在 release 下编译：在 packages/rust-cli 下
/// `cargo test --release --locked -p rust-cli --test request_profiles`；debug 下的对照是上一项。
#[cfg(not(debug_assertions))]
#[tokio::test]
async fn a_release_build_ignores_the_test_declarations() {
    let (body, beta) = first_request(&[
        "_ZCODE_ASSUME_FIRST_PARTY_BASE_URL",
        "_ZCODE_ASSUME_TOOL_SEARCH",
    ]);
    assert_eq!(tool_search(&body, beta.as_deref()), Search::Local, "{body}");
    // 同 debug 下的网关形状：对话中间的 system 消息带来的 `mid-conversation-system`（工作线 F64）。
    assert_eq!(beta.as_deref(), Some("mid-conversation-system-2026-04-07"));
    assert!(body.get("diagnostics").is_none(), "{body}");
    assert_eq!(body["system"].as_array().unwrap().len(), 2, "{body}");
    assert!(!offers_web_search(&body), "{body}");
}

/// 用户报的场景（2026-10-22）：自定义 provider（anthropic-messages，第三方网关，后面是 Bedrock）上的 claude-opus-5-5。
/// 修 bug（F52）：以前按模型卡片打开 ToolSearch，工具带 `defer_loading`、header 带 `advanced-tool-use-2025-11-20`，
/// Bedrock 报 `Unexpected value(s) advanced-tool-use-2025-11-20 for the anthropic-beta header`。工作线 F84：这里是本地
/// ToolSearch，延迟工具照样只在清单里宣布，请求仍然没有 `defer_loading`、占位工具与这个 beta。
#[tokio::test]
async fn a_custom_gateway_provider_gets_local_tool_search() {
    let sandbox = Sandbox::new();
    let fixture_url = "<fixture>";
    let config = serde_json::json!({
        "schemaVersion": 1,
        "config": {
            "providerConfigRules": {"providerRules": [{
                "providerId": "personal:gateway",
                "providerName": "Gateway",
                "enabled": true,
                "config": {
                    "group": "standard-personal",
                    "access": {"type": "api-key", "apiKey": "sk-test"},
                    "api": {"type": "anthropic-messages", "baseUrl": fixture_url},
                    "personalModelIds": ["claude-opus-5-5"]
                }
            }]}
        }
    });
    // 夹具地址要等启动后才知道：provider 配置里的地址由 `first_request` 替换前写成占位，这里换成环境变量传不了，
    // 所以先起一个夹具拿地址，再写配置。
    let fixture = sse::serve(vec![sse::text_reply("hello")]);
    let text = config.to_string().replace(fixture_url, &fixture.url);
    std::fs::write(sandbox.home.join(".zcode/v2/provider_config.json"), text).unwrap();
    let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
    let output = command
        .args(["-p", "hi", "--model", "personal:gateway/claude-opus-5-5"])
        .current_dir(&sandbox.work)
        .env_clear()
        .keep_system()
        .env("HOME", &sandbox.home)
        .env("USERPROFILE", &sandbox.home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", &sandbox.home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body = fixture.main_requests().remove(0);
    let beta = fixture.main_betas().remove(0);
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(tool_search(&body, beta.as_deref()), Search::Local, "{body}");
    assert!(listed_as_deferred(&body, "WebFetch"), "{body}");
}

/// 工作线 F84（tools-more.md §3.2.2）：网关上 ToolSearch 选中的工具，下一个请求以普通定义进 tools（不带 `defer_loading`，
/// 没有占位工具与 `advanced-tool-use`），ToolSearch 的结果是文字 `Tool loaded: <name>`，后面照样有 `Tool loaded.`。
/// 修改：F52 到 F84 之间网关上没有 ToolSearch，模型调用它只会得到未知工具。
#[tokio::test]
async fn a_tool_loaded_on_a_gateway_is_sent_as_a_plain_definition() {
    let sandbox = Sandbox::new();
    let fixture = sse::serve(vec![
        sse::tools_reply(&[sse::tool_block(
            0,
            "toolu_search",
            "ToolSearch",
            r#"{"query":"select:WebFetch"}"#,
        )]),
        sse::text_reply("done"),
    ]);
    let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
    let output = command
        .args(["-p", "load it"])
        .current_dir(&sandbox.work)
        .env_clear()
        .keep_system()
        .env("HOME", &sandbox.home)
        .env("USERPROFILE", &sandbox.home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", &sandbox.home)
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", "claude-opus-5-5")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = fixture.main_requests();
    let betas = fixture.main_betas();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(listed_as_deferred(&requests[0], "WebFetch"));
    let body = &requests[1];
    assert_eq!(
        tool_search(body, betas[1].as_deref()),
        Search::Local,
        "{body}"
    );
    let web_fetch = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "WebFetch")
        .unwrap_or_else(|| panic!("WebFetch is not on the wire: {body}"));
    assert!(web_fetch.get("defer_loading").is_none(), "{web_fetch}");
    let carrier = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| {
            message["content"]
                .as_array()
                .is_some_and(|blocks| blocks.iter().any(|block| block["type"] == "tool_result"))
        })
        .unwrap_or_else(|| panic!("no tool result: {body}"));
    let blocks = carrier["content"].as_array().unwrap();
    let result = blocks
        .iter()
        .find(|block| block["type"] == "tool_result")
        .unwrap();
    assert_eq!(
        result["content"],
        serde_json::json!([{"type": "text", "text": "Tool loaded: WebFetch"}]),
        "{carrier}"
    );
    assert!(
        blocks
            .iter()
            .any(|block| block["type"] == "text" && block["text"] == "Tool loaded."),
        "{carrier}"
    );
}

/// 名字出现在 `tools` 里（以完整定义发出）。
fn sent_inline(body: &Value, name: &str) -> bool {
    body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == name)
}

/// 工作线 F89（tools-more.md §3.2.1、config.md §2.5）：入口读 `ENABLE_TOOL_SEARCH`，只决定开不开，开时是本地。`false`
/// 关：ToolSearch 不发、WebFetch 内联、没有延迟工具清单。`auto:50` 的门槛是延迟工具占上下文窗口的 50%（按字符），内置的
/// 延迟工具远远不够：这一步的请求全部内联，清单照样宣布（ToolSearch 工具仍然启用）。`auto:0` 等于开。修 bug 前不读这个
/// 变量，三种写法都是本地 ToolSearch。
#[tokio::test]
async fn enable_tool_search_decides_whether_tool_search_is_on() {
    let run = |value: &str| {
        Sandbox::new().first_request(
            &[
                ("ZCODE_MODEL_BASE_URL", "<fixture>"),
                ("ZCODE_API_KEY", "test-key"),
                ("ZCODE_MODEL", "claude-opus-5-5"),
                ("ENABLE_TOOL_SEARCH", value),
            ],
            &[],
        )
    };
    let (body, beta) = run("false");
    assert_eq!(tool_search(&body, beta.as_deref()), Search::Off, "{body}");
    assert!(sent_inline(&body, "WebFetch"), "{body}");
    assert!(!body.to_string().contains("\\nWebFetch\\n"), "{body}");
    let (body, beta) = run("auto:50");
    assert_eq!(tool_search(&body, beta.as_deref()), Search::Off, "{body}");
    assert!(sent_inline(&body, "WebFetch"), "{body}");
    assert!(body.to_string().contains("\\nWebFetch\\n"), "{body}");
    let (body, beta) = run("auto:0");
    assert_eq!(tool_search(&body, beta.as_deref()), Search::Local, "{body}");
    assert!(listed_as_deferred(&body, "WebFetch"), "{body}");
}

const SESSION: &str = "2d480878-38af-4bae-a54f-096e2fb0f4d7";

/// 工作线 F52（model.md §3.3）：请求带 `metadata.user_id`。设备身份是 App / Node 已写的 `deviceMid`（文件不变），
/// `session_id` 是会话 ID 去掉 `sess_`。修 bug：以前请求里没有 metadata。
#[tokio::test]
async fn the_request_carries_the_shared_device_id_and_the_session() {
    let sandbox = Sandbox::new();
    let state = r#"{"lastDailyActiveDate":"2026-10-21","deviceMid":"7f1431f0-53e8-41c1-9cf9-22d11aa51d6a"}"#;
    std::fs::write(sandbox.telemetry_state(), state).unwrap();
    let vars = [
        ("ZCODE_MODEL_BASE_URL", "<fixture>"),
        ("ZCODE_API_KEY", "test-key"),
        ("ZCODE_MODEL", "claude-opus-5-5"),
    ];
    let (body, _) = sandbox.first_request(&vars, &["--session-id", SESSION]);
    assert_eq!(
        body["metadata"],
        serde_json::json!({"user_id": format!(
            r#"{{"device_id":"7f1431f0-53e8-41c1-9cf9-22d11aa51d6a","account_uuid":"","session_id":"{SESSION}"}}"#
        )})
    );
    assert_eq!(
        std::fs::read_to_string(sandbox.telemetry_state()).unwrap(),
        state
    );
}

/// 没有 `deviceMid` 时本工程生成并写进同一个文件（保留其他字段），请求里是同一个值。
#[tokio::test]
async fn the_first_run_writes_the_device_id_it_sends() {
    let sandbox = Sandbox::new();
    std::fs::write(
        sandbox.telemetry_state(),
        r#"{"lastDailyActiveDate":"2026-10-21"}"#,
    )
    .unwrap();
    let vars = [
        ("ZCODE_MODEL_BASE_URL", "<fixture>"),
        ("ZCODE_API_KEY", "test-key"),
        ("ZCODE_MODEL", "claude-opus-5-5"),
    ];
    let (body, _) = sandbox.first_request(&vars, &[]);
    let user: Value = serde_json::from_str(body["metadata"]["user_id"].as_str().unwrap()).unwrap();
    let stored: Value =
        serde_json::from_str(&std::fs::read_to_string(sandbox.telemetry_state()).unwrap()).unwrap();
    assert_eq!(stored["lastDailyActiveDate"], "2026-10-21");
    let device = stored["deviceMid"].as_str().unwrap();
    assert_eq!(user["device_id"], device);
    // UUID v4，小写带连字符（Node `crypto.randomUUID()` 的格式）。
    assert_eq!(device.len(), 36, "{device}");
    assert_eq!(&device[14..15], "4", "{device}");
    assert!(
        device
            .chars()
            .all(|c| c == '-' || c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    assert_eq!(user["account_uuid"], "");
    assert_eq!(user["session_id"].as_str().unwrap().len(), 36);
}
