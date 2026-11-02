//! 一条 Node 消息与它的 part（Node `MessageWithParts`）：仓储读出来的行，纯函数的输入。

use serde_json::Value;

/// Node `MessageWithParts`。
#[derive(Clone, Debug, PartialEq)]
pub struct WithParts {
    pub info: Value,
    pub parts: Vec<Value>,
}

impl WithParts {
    pub fn id(&self) -> &str {
        self.info["id"].as_str().unwrap_or("")
    }
}
