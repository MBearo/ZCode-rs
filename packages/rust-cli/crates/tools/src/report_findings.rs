//! ReportFindings（docs/specs/rust-cli/tools-more.md §12）。
//!
//! 把代码评审的 findings 以结构化列表交给宿主 UI：`call` 原样返回（放在 `display` 里），给模型的结果
//! 只是条数。描述与 schema 在 `prompts/tools/`。

use async_trait::async_trait;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::common::{Definition, definition, text};

/// ReportFindings 的结果落盘上限（字符）：与通用的 `tool::DEFAULT_MAX_RESULT_CHARS` 不同，是这个工具自己的值（F122）。
const MAX_RESULT_CHARS: usize = 256;

pub const NAME: &str = "ReportFindings";

pub struct ReportFindingsTool {
    definition: Definition,
}

impl ReportFindingsTool {
    pub fn new() -> Self {
        Self {
            definition: definition(NAME),
        }
    }
}

impl Default for ReportFindingsTool {
    fn default() -> Self {
        Self::new()
    }
}

/// 结果文本：`No findings reported.` 或 `<n> finding(s) reported.`。
fn summary(count: usize) -> String {
    match count {
        0 => "No findings reported.".to_owned(),
        1 => "1 finding reported.".to_owned(),
        _ => format!("{count} findings reported."),
    }
}

#[async_trait]
impl Tool for ReportFindingsTool {
    fn name(&self) -> &str {
        NAME
    }

    fn input_schema(&self) -> Value {
        self.definition.schema.clone()
    }

    fn description(&self, _context: &PromptContext) -> String {
        self.definition.description.clone()
    }

    fn search_hint(&self) -> Option<&str> {
        Some("report code-review findings as a structured list")
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(MAX_RESULT_CHARS)
    }

    async fn call(
        &self,
        input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let findings = input.get("findings").cloned().unwrap_or(Value::Null);
        let count = findings.as_array().map_or(0, Vec::len);
        let mut output = text(summary(count));
        output.display = json!({
            "count": count,
            "level": input.get("level").cloned().unwrap_or(Value::Null),
            "findings": findings,
        });
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rust_cli_kernel_api::permission::PermissionMode;
    use rust_cli_kernel_api::testing::ScriptedInteractions;

    use super::*;
    use crate::test_more::{Ctx, output_text};

    #[tokio::test]
    async fn the_result_counts_the_findings() {
        let tool = ReportFindingsTool::new();
        let prompt = PromptContext {
            tool_names: Vec::new(),
            mode: PermissionMode::Default,
        };
        assert!(
            tool.description(&prompt)
                .starts_with("Report code-review findings")
        );
        assert!(!tool.should_defer());
        let finding = json!({"file": "a.rs", "summary": "s", "failure_scenario": "f"});
        for (findings, expected) in [
            (json!([]), "No findings reported."),
            (json!([finding.clone()]), "1 finding reported."),
            (json!([finding.clone(), finding]), "2 findings reported."),
        ] {
            let (progress, _rx) = mpsc::channel(1);
            let context = Ctx::new(Arc::new(ScriptedInteractions::default())).build();
            let output = tool
                .call(
                    json!({"findings": findings, "level": "high"}),
                    context,
                    progress,
                )
                .await
                .unwrap_or_else(|error| panic!("{error}"));
            assert_eq!(output_text(&output), expected);
            assert_eq!(output.display["level"], json!("high"));
        }
    }
}
