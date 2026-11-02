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

/// 在执行环境里干活的八个内置工具在点名的机器上执行，其余内置工具不带 `machine` 参数（remote-exec.md §20.5.1；工作线
/// F137：以前是 kernel-api 按名字查的表 `MACHINE_TOOLS`）。
#[test]
fn the_builtin_tools_that_run_on_a_named_machine() {
    use rust_cli_kernel_api::machine_param::MachineParam;
    let mut tools: Vec<Arc<dyn Tool>> =
        crate::builtin(Arc::new(rust_cli_kernel_api::testing::FixedGate::allow())).tools();
    tools.push(Arc::new(crate::BashTool::new(Default::default())));
    tools.push(Arc::new(crate::MonitorTool::new()));
    let mut runs: Vec<&str> = tools
        .iter()
        .filter(|tool| tool.machine_param() == MachineParam::RunsThere)
        .map(|tool| tool.name())
        .collect();
    runs.sort_unstable();
    assert_eq!(
        runs,
        [
            "Bash",
            "Edit",
            "Glob",
            "Grep",
            "Monitor",
            "NotebookEdit",
            "Read",
            "Write"
        ]
    );
    let mut others = crate::worktree_tools(&Arc::default());
    others.extend(crate::interaction_tools(&Arc::default(), true));
    others.push(Arc::new(crate::WebFetchTool::new(
        None,
        Arc::new(rust_cli_kernel_api::testing::FixedClock(0)),
    )));
    for tool in &others {
        assert_eq!(
            tool.machine_param(),
            MachineParam::Absent,
            "{}",
            tool.name()
        );
    }
}
