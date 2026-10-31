//! 压缩过的会话重新打开（docs/specs/rust-cli/compact.md §5.1，工作线 F102）：发给模型的历史仍从最后一个压缩点开始，
//! 与改动之前逐条相同；界面拿到应用回退之后的完整历史。
//!
//! 记录的期望（`fixtures/compacted-*.json`）是改动之前的代码在同一个库上的结果；设 `F102_RECORD=1` 重写。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、读写期望文件"
)]

mod compacted;
mod sse;

use std::path::{Path, PathBuf};
use std::process::Stdio;

use compacted::{Keep, SESSION};
use rust_cli_base::models::GLM_5;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::store::SessionStore;
use serde_json::Value;

/// 两种最后一次压缩：保留段以正文结尾、以工具结果结尾。前面还有两次压缩（其中一次没有保留段）。
const SHAPES: [(&str, [(usize, Keep); 3]); 2] = [
    (
        "answer",
        [(3, Keep::Result), (6, Keep::Nothing), (9, Keep::Answer)],
    ),
    (
        "result",
        [(3, Keep::Answer), (6, Keep::Nothing), (9, Keep::Result)],
    ),
];

const TURNS: usize = 11;

struct Sandbox {
    _dir: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
}

fn sandbox() -> Sandbox {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    Sandbox {
        _dir: dir,
        home,
        work,
    }
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("compacted-{name}.json"))
}

/// 与记录的期望比较；`F102_RECORD=1` 时改写期望。
fn expect_recorded(name: &str, actual: &Value) {
    let path = fixture_path(name);
    if std::env::var_os("F102_RECORD").is_some() {
        let text = serde_json::to_string_pretty(actual).unwrap() + "\n";
        std::fs::write(&path, text).unwrap();
        return;
    }
    let expected: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        *actual == expected,
        "{name} differs from {}:\n{}",
        path.display(),
        serde_json::to_string_pretty(actual).unwrap()
    );
}

async fn seeded(keep: &[(usize, Keep)]) -> Sandbox {
    let sandbox = sandbox();
    let store = compacted::store(&sandbox.home);
    compacted::seed(&store, &sandbox.work, TURNS, keep).await;
    sandbox
}

/// 冷读取给模型侧的消息（会话 actor 的历史从它开始）与改动之前相同。
#[tokio::test]
async fn the_cold_history_for_the_model_is_unchanged() {
    for (name, keep) in SHAPES {
        let sandbox = seeded(&keep).await;
        let store = compacted::store(&sandbox.home);
        let loaded = store.load(&SessionId::new(SESSION)).await.unwrap();
        let messages = serde_json::to_value(&loaded.messages).unwrap();
        expect_recorded(&format!("cold-{name}"), &messages);
    }
}

/// 请求的消息：路径与日期换成占位；`<system-reminder>` 段只留第一行（段的种类与顺序随历史变化，正文随机器、提示词
/// 文本变化，不在这里比）。
fn normalized(request: &Value, sandbox: &Sandbox) -> Value {
    let text = request["messages"].to_string();
    let text = text
        .replace(sandbox.work.to_str().unwrap(), "<WORK>")
        .replace(sandbox.home.to_str().unwrap(), "<HOME>");
    let mut messages: Value = serde_json::from_str(&without_dates(&text)).unwrap();
    for message in messages.as_array_mut().unwrap() {
        let Some(blocks) = message["content"].as_array_mut() else {
            continue;
        };
        for block in blocks {
            if let Some(text) = block["text"].as_str()
                && let Some(body) = text.strip_prefix("<system-reminder>\n")
            {
                let first = body.lines().next().unwrap_or_default();
                block["text"] = Value::from(format!("<system-reminder>{first}"));
            }
        }
    }
    messages
}

/// `YYYY-MM-DD` 换成 `<DATE>`（不引入正则依赖）。
fn without_dates(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        let window = &bytes[index..bytes.len().min(index + 10)];
        let is_date = window.len() == 10
            && window.iter().enumerate().all(|(at, byte)| match at {
                4 | 7 => *byte == b'-',
                _ => byte.is_ascii_digit(),
            });
        if is_date {
            out.push_str("<DATE>");
            index += 10;
        } else {
            let ch = text[index..].chars().next().unwrap();
            out.push(ch);
            index += ch.len_utf8();
        }
    }
    out
}

/// 续写之后的第一个请求：消息与改动之前相同（system 与工具定义不受冷读取影响，不比）。
#[tokio::test]
async fn the_first_request_after_resuming_is_unchanged() {
    for (name, keep) in SHAPES {
        let sandbox = seeded(&keep).await;
        let fixture = sse::serve(vec![sse::text_reply("resumed answer")]);
        let output = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
            .args(["-p", "--resume", SESSION, "next question"])
            .current_dir(&sandbox.work)
            .env_clear()
            .env("HOME", &sandbox.home)
            .env("USERPROFILE", &sandbox.home)
            .env("PATH", "/usr/bin:/bin")
            .env("TZ", "UTC")
            .env("ZCODE_DATA_BASE_DIR", &sandbox.home)
            .env("ZCODE_MODEL_BASE_URL", &fixture.url)
            .env("ZCODE_API_KEY", "test-key")
            .env("ZCODE_MODEL", GLM_5)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let requests = fixture.main_requests();
        assert_eq!(requests.len(), 1);
        expect_recorded(
            &format!("request-{name}"),
            &normalized(&requests[0], &sandbox),
        );
    }
}

/// 视图里人类输入的文字，按顺序。
fn inputs(messages: &[rust_cli_kernel_api::message::Message]) -> Vec<String> {
    use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) if user.origin == UserOrigin::Human => match user.content.first() {
                Some(ContentBlock::Text { text }) => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// 打开压缩过的会话：视图里有完整的历史（每条消息一次、每个压缩点一个边界），模型侧的那一段与冷读取的历史相同；
/// rewind 只能选最后一个压缩点之后的输入。修 bug（工作线 F102）：以前视图只有最后一个压缩点之后的消息。
#[tokio::test(flavor = "multi_thread")]
async fn the_view_after_opening_has_the_whole_history() {
    use rust_cli_kernel_api::message::{Message, SystemMessage};
    use rust_cli_kernel_api::session_api::{EndReason, Kernel};

    for (name, keep) in SHAPES {
        let sandbox = seeded(&keep).await;
        let kernel = compacted::kernel(&sandbox.home, &sandbox.work).await;
        let handle = kernel.open_session(&SessionId::new(SESSION)).await.unwrap();
        let view = handle.view().await.unwrap();
        let expected: Vec<String> = (1..=TURNS).map(|n| format!("question {n}")).collect();
        assert_eq!(inputs(&view.messages), expected, "{name}");
        let boundaries = view
            .messages
            .iter()
            .filter(|m| matches!(m, Message::System(SystemMessage::CompactBoundary(_))))
            .count();
        assert_eq!(boundaries, 3, "{name}");
        let mut ids: Vec<&str> = view.messages.iter().map(|m| m.id().as_str()).collect();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total, "{name}: a message is shown twice");
        // 模型侧：视图上取活动历史，与冷读取给模型侧的消息相同。
        let store = compacted::store(&sandbox.home);
        let loaded = store.load(&SessionId::new(SESSION)).await.unwrap();
        let active: Vec<Message> = rust_cli_kernel_api::history::active(&view.messages)
            .into_iter()
            .map(std::borrow::Cow::into_owned)
            .collect();
        assert_eq!(active, loaded.messages, "{name}");
        let options: Vec<String> = handle
            .rewind_options()
            .await
            .unwrap()
            .into_iter()
            .map(|option| option.message.as_str().to_owned())
            .collect();
        assert_eq!(options, ["msg_u00010", "msg_u00011"], "{name}");
        handle.close(EndReason::Other).await;
    }
}
