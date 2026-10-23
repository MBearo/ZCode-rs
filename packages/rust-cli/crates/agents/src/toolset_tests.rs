//! 工具集的过滤（agents.md §4.2）。

use std::sync::Arc;

use rust_cli_kernel_api::orchestration::ToolRegistry;
use rust_cli_kernel_api::testing::{FakeTool, ListTools};
use rust_cli_kernel_api::tool::Tool;

use super::{FilteredTools, ToolRule};
use crate::definitions::{EXPLORE, GENERAL_PURPOSE, builtins};

/// 主线程的工具（内联与延迟加载合在一起）。
const MAIN: &[&str] = &[
    "Agent",
    "Bash",
    "Edit",
    "ListAgents",
    "Read",
    "ReportFindings",
    "ScheduleWakeup",
    "Skill",
    "ToolSearch",
    "Workflow",
    "Write",
    "CronCreate",
    "EnterWorktree",
    "NotebookEdit",
    "SendMessage",
    "TaskStop",
    "WebFetch",
    "mcp__docs__search",
];

fn registry() -> Arc<dyn ToolRegistry> {
    let tools: Vec<Arc<dyn Tool>> = MAIN
        .iter()
        .map(|name| Arc::new(FakeTool::new(name, true, "")) as Arc<dyn Tool>)
        .collect();
    Arc::new(ListTools(tools))
}

fn names(agent: &str, background: bool) -> Vec<String> {
    let definition = builtins()
        .into_iter()
        .find(|definition| definition.agent_type == agent)
        .unwrap();
    let filtered = FilteredTools {
        inner: registry(),
        rule: ToolRule::new(&definition, background, 1),
    };
    filtered
        .tools()
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect()
}

#[test]
fn general_purpose_drops_only_the_main_thread_tools() {
    assert_eq!(
        names(GENERAL_PURPOSE, false),
        [
            "Agent",
            "Bash",
            "Edit",
            "ListAgents",
            "Read",
            "ReportFindings",
            "Skill",
            "ToolSearch",
            "Write",
            "CronCreate",
            "EnterWorktree",
            "NotebookEdit",
            "SendMessage",
            "TaskStop",
            "WebFetch",
            "mcp__docs__search"
        ]
    );
}

#[test]
fn explore_is_read_only_and_background_keeps_the_async_set() {
    assert_eq!(
        names(EXPLORE, false),
        [
            "Bash",
            "ListAgents",
            "Read",
            "ReportFindings",
            "Skill",
            "ToolSearch",
            "CronCreate",
            "EnterWorktree",
            "SendMessage",
            "TaskStop",
            "WebFetch",
            "mcp__docs__search"
        ]
    );
    // A2：后台 Explore 只有 Bash、Read、Skill、ToolSearch 与延迟的 EnterWorktree、SendMessage、
    // TaskStop、WebFetch。
    assert_eq!(
        names(EXPLORE, true),
        [
            "Bash",
            "Read",
            "Skill",
            "ToolSearch",
            "EnterWorktree",
            "SendMessage",
            "TaskStop",
            "WebFetch",
            "mcp__docs__search"
        ]
    );
}

#[test]
fn find_respects_the_rule() {
    let definition = builtins().remove(1);
    let filtered = FilteredTools {
        inner: registry(),
        rule: ToolRule::new(&definition, false, 1),
    };
    assert!(filtered.find("Read").is_some());
    assert!(filtered.find("Edit").is_none());
    assert!(filtered.find("Workflow").is_none());
}

/// Agent 只看深度：后台 general-purpose 在深度 1、2 有它，深度 3 没有；前台同样。
#[test]
fn the_agent_tool_depends_on_the_depth_only() {
    let definition = builtins()
        .into_iter()
        .find(|definition| definition.agent_type == "general-purpose")
        .unwrap();
    let has_agent = |background, depth| {
        FilteredTools {
            inner: registry(),
            rule: ToolRule::new(&definition, background, depth),
        }
        .find("Agent")
        .is_some()
    };
    assert!(has_agent(true, 1) && has_agent(true, 2) && has_agent(false, 2));
    assert!(!has_agent(true, 3) && !has_agent(false, 3));
}

/// 主注册表：有 Bash，Glob / Grep 被它藏起来（`tools()` 里没有，`withheld` 能取回）。
struct HidingSearch(Vec<Arc<dyn Tool>>);

impl ToolRegistry for HidingSearch {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.0
            .iter()
            .filter(|tool| !matches!(tool.name(), "Glob" | "Grep"))
            .cloned()
            .collect()
    }

    fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools().into_iter().find(|tool| tool.name() == name)
    }

    fn withheld(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.0
            .iter()
            .find(|tool| tool.name() == name && matches!(name, "Glob" | "Grep"))
            .cloned()
    }
}

fn custom(tools: &[&str], disallowed: &[&str]) -> FilteredTools {
    let mut definition = builtins().remove(0);
    definition.tools = Some(tools.iter().map(|name| (*name).to_owned()).collect());
    definition.disallowed_tools = disallowed.iter().map(|name| (*name).to_owned()).collect();
    let all: Vec<Arc<dyn Tool>> = ["Agent", "Bash", "Glob", "Grep", "Read", "Write"]
        .iter()
        .map(|name| Arc::new(FakeTool::new(name, true, "")) as Arc<dyn Tool>)
        .collect();
    FilteredTools {
        inner: Arc::new(HidingSearch(all)),
        rule: ToolRule::new(&definition, false, 1),
    }
}

fn listed(filtered: &FilteredTools) -> Vec<String> {
    filtered
        .tools()
        .iter()
        .map(|tool| tool.name().to_owned())
        .collect()
}

/// 自定义 agent 点名工具清单（agents.md §4.2 第 4 条）：
/// 子代理的工具按清单的顺序；清单点名 Glob / Grep、解析出的工具里没有 Bash 时，补回被 Bash 藏起来的它们，排在
/// 清单其余工具之后。旧代码从主注册表过滤：没有 Grep，顺序跟主注册表。
#[test]
fn a_named_tool_list_keeps_its_order_and_gets_the_hidden_search_tools_back() {
    let reviewer = custom(&["Read", "Grep"], &[]);
    assert_eq!(listed(&reviewer), ["Read", "Grep"]);
    assert!(reviewer.keeps_order());
    assert!(reviewer.find("Grep").is_some());
    assert!(reviewer.find("Glob").is_none());
    // 补回的排在清单其余工具之后（解析完清单后才补）。
    assert_eq!(
        listed(&custom(&["Grep", "Write", "Read"], &[])),
        ["Write", "Read", "Grep"]
    );
    // 清单里有 Bash：搜索交给 Bash，不补。
    let with_bash = custom(&["Grep", "Bash", "Read"], &[]);
    assert_eq!(listed(&with_bash), ["Bash", "Read"]);
    assert!(with_bash.find("Grep").is_none());
    // disallowedTools 去掉的不补。
    assert_eq!(
        listed(&custom(&["Read", "Grep", "Glob"], &["Grep"])),
        ["Read", "Glob"]
    );
}

/// 没有点名清单（`*` 或缺省）：仍是主注册表的过滤，发给模型时按名字排序。
#[test]
fn a_wildcard_tool_list_is_sorted_by_name_and_gets_nothing_back() {
    let all = custom(&["*"], &[]);
    assert!(!all.keeps_order());
    assert!(all.find("Grep").is_none());
}
