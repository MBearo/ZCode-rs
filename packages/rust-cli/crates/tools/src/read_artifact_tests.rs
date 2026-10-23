//! Read 读落盘位置（修复线 F18，tools-files.md §1.4）：引用由存储按会话与 artifact 找回（测试替身与 NodeStore
//! 一样核对大小），结果按读文件的规则给出。真实 NodeStore 上的读法见 cli 的 `artifact_read.rs`。
use std::sync::Arc;

use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::testing::{MemoryArtifacts, ScriptedInteractions};
use rust_cli_kernel_api::tool::ToolOutput;

use super::*;
use crate::test_more::{Ctx, output_text};

fn context(artifacts: Arc<MemoryArtifacts>) -> ToolUseContext {
    ToolUseContext {
        artifacts,
        ..Ctx::new(Arc::new(ScriptedInteractions::default())).build()
    }
}

fn input(file_path: &str, offset: Option<u64>, limit: Option<u64>) -> Input {
    Input {
        file_path: file_path.to_owned(),
        offset,
        limit,
        pages: None,
    }
}

/// 经结果预算落盘，返回预览里的位置。
async fn persist(artifacts: &MemoryArtifacts, blocks: &[String]) -> String {
    let output = ToolOutput {
        content: blocks
            .iter()
            .map(|text| ToolResultContent::Text { text: text.clone() })
            .collect(),
        is_error: false,
        display: Value::Null,
    };
    let preview = budget::apply(
        output,
        "Grep",
        Some(100),
        &SessionId::new("sess_1"),
        artifacts,
    )
    .await;
    let text = output_text(&preview);
    text.split("Full output saved to: ")
        .nth(1)
        .and_then(|rest| rest.split('\n').next())
        .unwrap()
        .to_owned()
}

async fn read(
    artifacts: &Arc<MemoryArtifacts>,
    location: &str,
    page: Option<(u64, u64)>,
) -> Result<String, String> {
    let rest = location.strip_prefix(budget::ARTIFACT_SCHEME).unwrap();
    let request = input(
        location,
        page.map(|(offset, _)| offset),
        page.map(|(_, limit)| limit),
    );
    read_artifact(&context(artifacts.clone()), rest, &request)
        .await
        .map(|output| output_text(&output))
        .map_err(|error| error.to_string())
}

#[tokio::test]
async fn a_multi_block_result_is_read_back_from_its_json_artifact() {
    let artifacts = Arc::new(MemoryArtifacts::default());
    let location = persist(&artifacts, &["a".repeat(80), "b".repeat(80)]).await;
    // 多段结果存 JSON（application/json）；修前 Read 按 text/plain、size 0 造引用，读不回。
    let whole = read(&artifacts, &location, None).await.unwrap();
    let json = format!(
        "[\n  {{\n    \"type\": \"text\",\n    \"text\": \"{}\"\n  }},\n  {{\n    \"type\": \"text\",\n    \"text\": \"{}\"\n  }}\n]",
        "a".repeat(80),
        "b".repeat(80)
    );
    assert_eq!(whole, number_lines(&json, 1));
    assert_eq!(
        read(&artifacts, &location, Some((3, 2))).await.unwrap(),
        "3\t    \"type\": \"text\",\n4\t    \"text\": \"".to_owned() + &"a".repeat(80) + "\""
    );
    assert_eq!(
        read(&artifacts, &location, Some((20, 1))).await.unwrap(),
        texts::shorter_than_offset(20, 10)
    );
}

#[tokio::test]
async fn whole_reads_keep_the_file_limits() {
    let artifacts = Arc::new(MemoryArtifacts::default());
    // 60000 个字符：按 .txt 估算 15000 token（可读），按 .json 估算 30000 token（超过 25000）。
    let text = persist(&artifacts, &["x".repeat(60_000)]).await;
    assert_eq!(
        read(&artifacts, &text, None).await.unwrap(),
        format!("1\t{}", "x".repeat(60_000))
    );
    let json = persist(&artifacts, &["y".repeat(30_000), "z".repeat(30_000)]).await;
    let error = read(&artifacts, &json, None).await.unwrap_err();
    assert!(
        error.contains("exceeds maximum allowed tokens (25000)"),
        "{error}"
    );
    // 超过 256KB：不加 limit 时报错，加了 limit 照常读。
    let big = persist(&artifacts, &[format!("{}\n", "w".repeat(99)).repeat(3000)]).await;
    let error = read(&artifacts, &big, None).await.unwrap_err();
    assert!(
        error.contains("exceeds maximum allowed size (256KB)"),
        "{error}"
    );
    assert_eq!(
        read(&artifacts, &big, Some((2, 1))).await.unwrap(),
        format!("2\t{}", "w".repeat(99))
    );
}

#[tokio::test]
async fn bad_or_unknown_locations_are_errors() {
    let artifacts = Arc::new(MemoryArtifacts::default());
    let location = persist(&artifacts, &["v".repeat(200)]).await;
    let other = location.replace("sess_1", "sess_2");
    for (location, wanted) in [
        (
            "zcode-artifact://sess_1".to_owned(),
            "Invalid artifact location",
        ),
        (
            "zcode-artifact://sess_1/".to_owned(),
            "Invalid artifact location",
        ),
        // 第一个 `/` 之后都是 id（地址不按路径规范化，F14）：存储里没有这个 id。
        (
            "zcode-artifact://sess_1/a/b".to_owned(),
            "artifact not found",
        ),
        (
            "zcode-artifact://sess_1/tool-result-9".to_owned(),
            "artifact not found",
        ),
        (other, "artifact not found"),
    ] {
        let error = read(&artifacts, &location, None).await.unwrap_err();
        assert!(error.contains(wanted), "{location}: {error}");
    }
}
