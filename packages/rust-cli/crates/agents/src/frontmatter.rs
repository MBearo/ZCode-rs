//! agent 文件的 frontmatter（agents.md §3）：解析器在 base（与 SKILL.md 共用，F132 的 T8），这里只有 agent 专用的
//! `tools` 读法。嵌套块（`Value::Block`）在 agent 文件里按没写：`Value::text` 不给它，`tools` 也不给。

pub(crate) use rust_cli_base::frontmatter::{Value, split};

/// 工具清单：缺省为 None（全部）；含 `*` 也是全部；逗号分隔或列表。嵌套块按没写。
pub(crate) fn tools(value: Option<&Value>) -> Option<Vec<String>> {
    let items: Vec<String> = match value? {
        Value::Text(text) => text
            .split(',')
            .map(|item| item.trim().to_owned())
            .filter(|item| !item.is_empty())
            .collect(),
        Value::List(items) => items.clone(),
        Value::Block(_) => return None,
    };
    (!items.iter().any(|item| item == "*")).then_some(items)
}

#[cfg(test)]
#[path = "frontmatter_tests.rs"]
mod tests;
