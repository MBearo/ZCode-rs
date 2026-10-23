use std::sync::Arc;

use rust_cli_kernel_api::testing::FakeTool;

use super::*;

fn names(registry: &BuiltinTools) -> Vec<String> {
    registry
        .tools()
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect()
}

#[test]
fn search_tools_hide_behind_bash_unless_named_at_startup() {
    let base = || -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(crate::glob::GlobTool::new()),
            Arc::new(crate::grep::GrepTool::new()),
        ]
    };
    assert_eq!(names(&BuiltinTools::new(base())), ["Glob", "Grep"]);
    let mut with_bash = base();
    with_bash.push(Arc::new(FakeTool::new("Bash", false, "ok")));
    let hidden = BuiltinTools::new(with_bash.clone());
    assert_eq!(names(&hidden), ["Bash"]);
    assert!(hidden.find("Grep").is_none());
    let opted = BuiltinTools::new(with_bash).with_search_opt_in(true);
    assert_eq!(names(&opted), ["Glob", "Grep", "Bash"]);
}

/// 被 Bash 藏起来的 Glob / Grep 仍能取回（子代理点名时补回，修复线 F16，agents.md §4.2）；暴露着的、不存在的
/// 与不是搜索工具的都没有。旧代码只有缺省实现（总是 None）。
#[test]
fn hidden_search_tools_can_be_withheld_back() {
    let tools = || -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(crate::glob::GlobTool::new()),
            Arc::new(crate::grep::GrepTool::new()),
            Arc::new(FakeTool::new("Bash", false, "ok")),
        ]
    };
    let hidden = BuiltinTools::new(tools());
    assert_eq!(
        hidden
            .withheld("Grep")
            .map(|tool| tool.name().to_owned())
            .as_deref(),
        Some("Grep")
    );
    assert_eq!(
        hidden
            .withheld("Glob")
            .map(|tool| tool.name().to_owned())
            .as_deref(),
        Some("Glob")
    );
    assert!(hidden.withheld("Bash").is_none());
    assert!(hidden.withheld("Read").is_none());
    let opted = BuiltinTools::new(tools()).with_search_opt_in(true);
    assert!(opted.withheld("Grep").is_none());
}
