use std::sync::Arc;

use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::testing::ListTools;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::*;

/// 整理输入（`rewrite` 为真时把输入换成 `{"rewritten": true}`）的工具。
struct Rewriting {
    name: &'static str,
    rewrite: bool,
}

#[async_trait::async_trait]
impl Tool for Rewriting {
    fn name(&self) -> &str {
        self.name
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }
    fn rewrite_input(&self, _input: &Value) -> Option<Value> {
        self.rewrite.then(|| json!({"rewritten": true}))
    }
    async fn call(
        &self,
        _input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        unreachable!("not called")
    }
}

fn block(name: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: ToolUseId::new("t"),
        name: name.to_owned(),
        input: json!({"n": 1}),
    }
}

fn input(block: &ContentBlock) -> &Value {
    let ContentBlock::ToolUse { input, .. } = block else {
        unreachable!()
    };
    input
}

/// 只按工具自己的整理写回（工作线 F131）：整理的工具写回，不整理的与找不到的工具不动。
#[test]
fn only_tools_that_rewrite_change_the_input() {
    let tools = ListTools(vec![
        Arc::new(Rewriting {
            name: "Rewrites",
            rewrite: true,
        }),
        Arc::new(Rewriting {
            name: "Keeps",
            rewrite: false,
        }),
    ]);
    let mut rewritten = block("Rewrites");
    apply(&tools, &mut rewritten);
    assert_eq!(input(&rewritten), &json!({"rewritten": true}));
    for name in ["Keeps", "Missing"] {
        let mut kept = block(name);
        apply(&tools, &mut kept);
        assert_eq!(input(&kept), &json!({"n": 1}), "{name}");
    }
}
