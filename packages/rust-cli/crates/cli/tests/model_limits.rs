//! 修复线 F61（docs/specs/rust-cli/compact.md §2、model.md §11、context-usage.md §4）：压缩窗口与输出上限只从模型配置取。
//! 真实 `zcode -p` 连本地 SSE 夹具，两种模型来源都看：
//!
//! - 显式覆盖（`ZCODE_API_KEY` + `ZCODE_MODEL_BASE_URL` + `ZCODE_MODEL`，跑分用）：窗口与输出上限按内置配置的通用规则，
//!   与能力同一份规则。glm-5.3 是 `contextWindow: 1000000`、`maxOutputTokens.max: 128000`。
//! - Provider 配置（平台层的目录）：自定义 provider 上的 glm-4.5，内置规则给 `contextWindow: 131072`。
//!
//! 修前：显式覆盖没有目录，压缩与 `/context` 都按默认窗口 200,000——glm-5.3 在约 167k 就自动压缩；平台层的目录只在
//! `auto_compact` 里用，同步的把关 `should_auto_compact` 按 200,000，窗口小于它的模型到了配置的阈值也不压缩。
//!
//! 工作线 F95（tools-more.md §5 表后）：目录模型上 WebSearch 旁路请求的输出上限经 Provider 补丁发出。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具"
)]

mod system_env;
use system_env::KeepSystem as _;
mod sse;

use std::path::PathBuf;
use std::process::{Output, Stdio};

use serde_json::{Value, json};

/// 摘要请求的开头（`prompts/compact/control.txt`，压缩提示词）。
const SUMMARY_PROMPT: &str = "Your task is to create a detailed summary of the conversation so far";

/// 一次运行的临时 home 与工作区（工作区里有 `a.txt`，模型用 Read 读它）。
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
        std::fs::write(work.join("a.txt"), "alpha\n").unwrap();
        Self {
            _dir: dir,
            home,
            work,
        }
    }

    /// 个人 Provider 配置：一个 anthropic-messages 的自定义 provider，地址是夹具。
    fn provider(&self, url: &str, model: &str) {
        self.provider_with_rules(url, model, &[]);
    }

    /// 同 [`Self::provider`]，另给这个 provider 的模型配置规则（`providerModelRules`，每项是 `{modelId, config}`）。
    fn provider_with_rules(&self, url: &str, model: &str, rules: &[Value]) {
        let rules: Vec<Value> = rules
            .iter()
            .map(|rule| {
                let mut rule = rule.clone();
                rule["providerId"] = json!("personal:gateway");
                rule
            })
            .collect();
        let config = json!({
            "schemaVersion": 1,
            "config": {
                "providerConfigRules": {"providerRules": [{
                    "providerId": "personal:gateway",
                    "providerName": "Gateway",
                    "enabled": true,
                    "config": {
                        "group": "standard-personal",
                        "access": {"type": "api-key", "apiKey": "sk-test"},
                        "api": {"type": "anthropic-messages", "baseUrl": url},
                        "personalModelIds": [model]
                    }
                }]},
                "modelConfigRules": {"providerModelRules": rules, "manualProviderModelRules": []}
            }
        });
        std::fs::write(
            self.home.join(".zcode/v2/provider_config.json"),
            config.to_string(),
        )
        .unwrap();
    }

    /// 跑一次 `zcode -p <prompt>`（不继承父进程的环境）。
    fn run(&self, prompt: &str, vars: &[(&str, &str)], args: &[&str]) -> Output {
        let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
        command
            .args(["-p", prompt])
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
            command.env(name, value);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    /// 剧本：前两个回复各调用一次 Read（两个 API 轮，压缩才有可摘要的组），第二个的用量里输入是 `input_tokens`
    /// （第三个请求之前按它判定自动压缩）；之后回 `rest`。
    fn script(&self, input_tokens: u64, rest: &[&str]) -> Vec<String> {
        let input = json!({"file_path": self.work.join("a.txt")}).to_string();
        let read = |id: &str| sse::tool_block(0, id, "Read", &input);
        let mut script = vec![
            sse::tools_reply(&[read("toolu_first")]),
            sse::tools_reply_with_usage(&[read("toolu_read")], input_tokens),
        ];
        script.extend(rest.iter().map(|text| sse::text_reply(text)));
        script
    }
}

fn explicit(url: &str, model: &str) -> Vec<(String, String)> {
    vec![
        ("ZCODE_MODEL_BASE_URL".to_owned(), url.to_owned()),
        ("ZCODE_API_KEY".to_owned(), "test-key".to_owned()),
        ("ZCODE_MODEL".to_owned(), model.to_owned()),
    ]
}

fn borrowed(vars: &[(String, String)]) -> Vec<(&str, &str)> {
    vars.iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

fn is_summary(body: &Value) -> bool {
    body.to_string().contains(SUMMARY_PROMPT)
}

/// `-p "/context"` 的 markdown 里 `**Tokens:** <总量> / <窗口> (…)` 的窗口。
fn context_window(stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(stdout);
    let line = text
        .lines()
        .find(|line| line.starts_with("**Tokens:**"))
        .unwrap_or_else(|| panic!("{text}"));
    let (_, rest) = line.split_once(" / ").unwrap();
    rest.split(' ').next().unwrap().to_owned()
}

/// `/context` 表格里 `Autocompact buffer` 一行的 token 数（窗口 − 阈值 = 摘要预留 + 13000）。
fn autocompact_buffer(stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(stdout);
    let line = text
        .lines()
        .find(|line| line.starts_with("| Autocompact buffer |"))
        .unwrap_or_else(|| panic!("{text}"));
    line.split('|').nth(2).unwrap().trim().to_owned()
}

/// 显式覆盖的 glm-5.3（配置 1M）：300k token 时不压缩，第三个请求就是带工具结果的续写。
#[tokio::test]
async fn explicit_glm_5_3_does_not_compact_at_300k() {
    let sandbox = Sandbox::new();
    let fixture = sse::serve(sandbox.script(300_000, &["done"]));
    let vars = explicit(&fixture.url, "glm-5.3");
    sandbox.run("go", &borrowed(&vars), &[]);
    let requests = fixture.main_requests();
    assert!(
        !requests.iter().any(is_summary),
        "compacted at 300k: {} requests",
        requests.len()
    );
    assert_eq!(requests.len(), 3);
    assert!(requests[2].to_string().contains("toolu_read"));
}

/// 显式覆盖的 glm-5.3：`/context` 的窗口是配置的 1,000,000（修前 200k）。
#[tokio::test]
async fn explicit_glm_5_3_context_shows_the_configured_window() {
    let sandbox = Sandbox::new();
    let fixture = sse::serve(Vec::new());
    let vars = explicit(&fixture.url, "glm-5.3");
    let output = sandbox.run("/context", &borrowed(&vars), &[]);
    assert_eq!(context_window(&output.stdout), "1m");
    // 预留 min(128000, 20000) + 13000。
    assert_eq!(autocompact_buffer(&output.stdout), "33k");
}

/// 显式覆盖：配置的 `maxOutputTokens.max` 进摘要预留。glm-4-flash-250414 是 `contextWindow: 131072`、`max: 16384`：
/// 缓冲 = 16384 + 13000 = 29384（修前按默认窗口 200k、预留 20000，缓冲 33k）。
#[tokio::test]
async fn explicit_output_limit_sets_the_summary_reserve() {
    let sandbox = Sandbox::new();
    let fixture = sse::serve(Vec::new());
    let vars = explicit(&fixture.url, "glm-4-flash-250414");
    let output = sandbox.run("/context", &borrowed(&vars), &[]);
    assert_eq!(context_window(&output.stdout), "131.1k");
    assert_eq!(autocompact_buffer(&output.stdout), "29.4k");
}

/// 回归：显式覆盖的 claude-opus-5-5（卡片 1M、内置规则 1M）照旧。
#[tokio::test]
async fn an_explicit_carded_model_keeps_its_window() {
    let sandbox = Sandbox::new();
    let fixture = sse::serve(Vec::new());
    let vars = explicit(&fixture.url, "claude-opus-5-5");
    let output = sandbox.run("/context", &borrowed(&vars), &[]);
    assert_eq!(context_window(&output.stdout), "1m");
    assert_eq!(autocompact_buffer(&output.stdout), "33k");
}

/// 回归：`ZCODE_AUTO_COMPACT_WINDOW` 照旧盖过配置的窗口（`min(窗口, 覆盖值)`）。
#[tokio::test]
async fn the_window_override_still_applies() {
    let sandbox = Sandbox::new();
    let fixture = sse::serve(Vec::new());
    let mut vars = explicit(&fixture.url, "glm-5.3");
    vars.push(("ZCODE_AUTO_COMPACT_WINDOW".to_owned(), "300000".to_owned()));
    let output = sandbox.run("/context", &borrowed(&vars), &[]);
    assert_eq!(context_window(&output.stdout), "300k");
}

/// Provider 配置里的 glm-4.5（配置 131072）：100k token 已过配置的阈值 131072 − 20000 − 13000 = 98072，压缩。修前同步的
/// 把关按默认窗口 200k（阈值 167000），不压缩。
#[tokio::test]
async fn a_configured_small_window_compacts_at_its_threshold() {
    let sandbox = Sandbox::new();
    let fixture = sse::serve(sandbox.script(100_000, &["<summary>S</summary>", "done"]));
    sandbox.provider(&fixture.url, "glm-4.5");
    sandbox.run("go", &[], &["--model", "personal:gateway/glm-4.5"]);
    let requests = fixture.main_requests();
    assert!(
        requests.iter().any(is_summary),
        "did not compact: {} requests",
        requests.len()
    );
}

/// Provider 配置里的 glm-5.3（配置 1M）：300k token 不压缩；`/context` 的窗口是 1M。
#[tokio::test]
async fn a_configured_1m_window_does_not_compact_at_300k() {
    let sandbox = Sandbox::new();
    let fixture = sse::serve(sandbox.script(300_000, &["done"]));
    sandbox.provider(&fixture.url, "glm-5.3");
    sandbox.run("go", &[], &["--model", "personal:gateway/glm-5.3"]);
    let requests = fixture.main_requests();
    assert!(!requests.iter().any(is_summary), "{}", requests.len());
    let output = sandbox.run("/context", &[], &["--model", "personal:gateway/glm-5.3"]);
    assert_eq!(context_window(&output.stdout), "1m");
}

/// 工作线 F95（tools-more.md §5 表后）：目录模型上 WebSearch 的旁路请求发 `min(4096, 配置的上限)`，同 Node（Node 的 option
/// map 以请求自己的上限为输入）；主请求照旧发配置的上限（补丁的输入不变）。修前补丁的输入只是配置的上限，旁路请求也
/// 发 128000。
#[tokio::test]
async fn a_catalog_web_search_sends_the_node_output_limit() {
    let sandbox = Sandbox::new();
    let search = sse::tool_block(0, "toolu_search", "WebSearch", r#"{"query":"rust"}"#);
    let fixture = sse::serve(vec![
        sse::tools_reply(&[search]),
        sse::text_reply("Found it."),
        sse::text_reply("done"),
    ]);
    let web_search =
        json!({"modelId": "glm-5.3", "config": {"properties": {"supportsNativeWebSearch": true}}});
    sandbox.provider_with_rules(&fixture.url, "glm-5.3", &[web_search]);
    sandbox.run(
        "search",
        &[("ENABLE_TOOL_SEARCH", "false")],
        &[
            "--model",
            "personal:gateway/glm-5.3",
            "--allowedTools",
            "WebSearch",
        ],
    );
    let requests = fixture.main_requests();
    let (side, main): (Vec<&Value>, Vec<&Value>) = requests
        .iter()
        .partition(|body| body["tools"][0]["type"] == "web_search_20260209");
    assert_eq!(side.len(), 1, "{requests:#?}");
    assert_eq!(side[0]["max_tokens"], 4096);
    assert_eq!(main.len(), 2, "{requests:#?}");
    assert!(main[0].to_string().contains("\"name\":\"WebSearch\""));
    for body in main {
        assert_eq!(body["max_tokens"], 128_000);
    }
}
