//! 修复线 F18 的真实进程（docs/specs/rust-cli/tools-files.md §1.4）：`zcode -p` 用真实的 NodeStore 落盘，
//! 模型再用 Read 读预览里的 `zcode-artifact://` 位置。
//!
//! - SessionStart hook 的纯文本 stdout 超过 10000 字符，落盘（hooks.md §4.5）；
//! - 第一个请求的回复是 Grep，结果超过 20000 字符，落盘（tools-files.md D5）；
//! - 第二个请求的回复按请求体里的两个位置各读一次全文、一页；第三个请求里的四个 Read 结果都是原文。
//!
//! 修前：Read 自己造 `size: 0` 的引用，NodeStore 的 `get` 核对大小，四个结果都是
//! `artifact … has N bytes, expected 0` 的报错。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具"
)]

mod sse;

use std::process::Stdio;

use serde_json::{Value, json};

const MARKER: &str = "Full output saved to: ";

/// 请求体里模型可见的文字（消息的字符串、文本块、tool_result 的内容），按出现顺序。
fn texts(body: &Value) -> Vec<String> {
    fn collect(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(text) => out.push(text.clone()),
            Value::Array(items) => items.iter().for_each(|item| collect(item, out)),
            Value::Object(block) => {
                if let Some(text) = block.get("text") {
                    collect(text, out);
                }
                if block.get("type") == Some(&json!("tool_result")) {
                    collect(&block["content"], out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for message in body["messages"].as_array().into_iter().flatten() {
        collect(&message["content"], &mut out);
    }
    out
}

/// 请求体里的落盘位置，按出现顺序（SessionStart 的在第一条人类消息里，Grep 的在 tool_result 里）。
fn locations(body: &Value) -> Vec<String> {
    texts(body)
        .iter()
        .flat_map(|text| {
            text.split(MARKER)
                .skip(1)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .map(|rest| rest.split('\n').next().unwrap().to_owned())
        .collect()
}

/// 请求里 tool_result 的 (tool_use_id, 文本, is_error)。
fn tool_results(body: &Value) -> Vec<(String, String, bool)> {
    let mut found = Vec::new();
    for message in body["messages"].as_array().into_iter().flatten() {
        for block in message["content"].as_array().into_iter().flatten() {
            if block["type"] != "tool_result" {
                continue;
            }
            let text = match &block["content"] {
                Value::String(text) => text.clone(),
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect(),
                _ => String::new(),
            };
            found.push((
                block["tool_use_id"].as_str().unwrap().to_owned(),
                text,
                block["is_error"] == true,
            ));
        }
    }
    found
}

fn read(index: usize, id: &str, location: &str, page: Option<(u64, u64)>) -> String {
    let mut input = json!({"file_path": location});
    if let Some((offset, limit)) = page {
        input["offset"] = json!(offset);
        input["limit"] = json!(limit);
    }
    sse::tool_block(index, id, "Read", &input.to_string())
}

#[tokio::test]
async fn read_returns_persisted_tool_and_hook_output_from_the_real_store() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(home.join(".zcode/cli")).unwrap();
    // 600 行、每行约 60 字符：Grep 的结果超过 20000 字符（落盘），全文约 9000 token（整读不超限）。
    let big: String = (1..=600)
        .map(|i| format!("foo line {i} {}\n", "x".repeat(40)))
        .collect();
    std::fs::write(work.join("big.txt"), big).unwrap();
    // SessionStart 的纯文本 stdout：1500 行、约 20000 字符，超过 10000 落盘。
    let hook = "awk 'BEGIN{for(i=1;i<=1500;i++) print \"hook line \" i}'";
    let config = json!({"hooks": {"enabled": true, "events": {
        "SessionStart": [{"hooks": [{"type": "command", "command": hook}]}],
    }}});
    std::fs::write(home.join(".zcode/cli/config.json"), config.to_string()).unwrap();

    let grep = json!({
        "pattern": "foo",
        "path": work.join("big.txt"),
        "output_mode": "content",
        "head_limit": 0,
    })
    .to_string();
    let fixture = sse::serve_with(vec![
        sse::Reply::Fixed(sse::tools_reply(&[sse::tool_block(
            0,
            "toolu_grep",
            "Grep",
            &grep,
        )])),
        sse::Reply::From(Box::new(|body: &Value| {
            // 不在夹具线程里 panic（进程会一直等回复）：位置不对时回一段文字，由下面的断言报出来。
            let found = locations(body);
            let [hook, tool] = found.as_slice() else {
                return sse::text_reply("missing locations");
            };
            sse::tools_reply(&[
                read(0, "toolu_tool_all", tool, None),
                read(1, "toolu_tool_page", tool, Some((300, 2))),
                read(2, "toolu_hook_all", hook, None),
                read(3, "toolu_hook_page", hook, Some((1499, 5))),
            ])
        })),
        sse::Reply::Fixed(sse::text_reply("read back")),
    ]);
    let output = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        // 点名 Grep 才暴露它（tools-files.md §1.1）。
        .args(["-p", "go", "--permission-mode", "bypassPermissions"])
        .args(["--allowedTools", "Grep,Read"])
        .current_dir(&work)
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/bash")
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", "claude-opus-5-5")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");

    let requests = fixture.main_requests();
    let found = locations(&requests[1]);
    assert_eq!(
        found.len(),
        2,
        "{found:?}\n{}",
        texts(&requests[1]).join("\n---\n")
    );
    assert!(
        found.iter().all(|at| at.starts_with("zcode-artifact://")),
        "{found:?}"
    );
    assert_eq!(requests.len(), 3, "{stderr}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "read back");
    let results = tool_results(&requests[2]);
    let by_id = |id: &str| {
        let (_, text, error) = results.iter().find(|(at, ..)| at == id).unwrap();
        assert!(!error, "{id}: {text}");
        text.clone()
    };

    // Grep 的全文：600 行，行号从 1 起，每行是 Grep 的一行（以 `foo line <行号> x…` 结尾）。
    let tool_all = by_id("toolu_tool_all");
    let lines: Vec<&str> = tool_all.split('\n').collect();
    assert_eq!(lines.len(), 600, "{tool_all}");
    for (index, line) in lines.iter().enumerate() {
        let n = index + 1;
        assert!(
            line.starts_with(&format!("{n}\t"))
                && line.ends_with(&format!("foo line {n} {}", "x".repeat(40))),
            "line {n}: {line}"
        );
    }
    // 一页：offset 300、limit 2。
    assert_eq!(by_id("toolu_tool_page"), lines[299..301].join("\n"));

    // hook 输出的全文（落盘的是去掉首尾空白的 stdout）与到结尾的一页。
    let hook_all: Vec<String> = (1..=1500).map(|i| format!("{i}\thook line {i}")).collect();
    assert_eq!(by_id("toolu_hook_all"), hook_all.join("\n"));
    assert_eq!(
        by_id("toolu_hook_page"),
        "1499\thook line 1499\n1500\thook line 1500"
    );
}
