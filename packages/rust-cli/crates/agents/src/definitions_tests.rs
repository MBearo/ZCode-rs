//! 内置 agent 的清单行逐字照写（agents.md §3）。

use super::{AgentSource, builtins, merge, tools_description};

/// 主线程请求的 agent 清单里的三行（产品名替换后）。
const LISTED_LINES: &[&str] = &[
    "- Explore: Read-only search agent for broad fan-out searches — when answering means sweeping many files, directories, or naming conventions and you only need the conclusion, not the file dumps. It reads excerpts rather than whole files, so it locates code; it doesn't review or audit it. Specify search breadth: \"medium\" for moderate exploration, \"very thorough\" for multiple locations and naming conventions. (Tools: All tools except Agent, Artifact, ArtifactComments, ArtifactData, ArtifactCheck, ExitPlanMode, Edit, Write, NotebookEdit)",
    "- general-purpose: General-purpose agent for researching complex questions, searching for code, and executing multi-step tasks. When you are searching for a keyword or file and are not confident that you will find the right match in the first few tries use this agent to perform the search for you. (Tools: *)",
    "- Plan: Software architect agent for designing implementation plans. Use this when you need to plan the implementation strategy for a task. Returns step-by-step plans, identifies critical files, and considers architectural trade-offs. (Tools: All tools except Agent, Artifact, ArtifactComments, ArtifactData, ArtifactCheck, ExitPlanMode, Edit, Write, NotebookEdit)",
];

#[test]
fn builtin_listing_lines_are_verbatim() {
    let lines: Vec<String> = builtins()
        .iter()
        .map(super::AgentDefinition::listing_line)
        .collect();
    for expected in LISTED_LINES {
        assert!(lines.iter().any(|line| line == expected), "{expected}");
    }
    assert_eq!(lines.len(), 3);
}

#[test]
fn only_explore_and_plan_are_one_shot() {
    let agents = builtins();
    let one_shot: Vec<&str> = agents
        .iter()
        .filter(|agent| agent.one_shot())
        .map(|agent| agent.agent_type.as_str())
        .collect();
    assert_eq!(one_shot, ["Explore", "Plan"]);
}

#[test]
fn tools_description_lists_the_usable_tools() {
    let own = |names: &[&str]| {
        names
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(tools_description(None, &[]), "All tools");
    assert_eq!(
        tools_description(Some(&own(&["Read", "Edit"])), &[]),
        "Read, Edit"
    );
    assert_eq!(
        tools_description(Some(&own(&["Read", "Edit"])), &own(&["Edit"])),
        "Read"
    );
    assert_eq!(
        tools_description(Some(&own(&["Edit"])), &own(&["Edit"])),
        "None"
    );
    assert_eq!(
        tools_description(None, &own(&["Bash"])),
        "All tools except Bash"
    );
    assert_eq!(tools_description(Some(&[]), &[]), "All tools");
}

#[test]
fn later_sources_override_by_type_and_keep_the_position() {
    let mut custom = builtins()[0].clone();
    custom.source = AgentSource::Project;
    custom.when_to_use = "Project override.".to_owned();
    let mut extra = builtins()[0].clone();
    extra.agent_type = "reviewer".to_owned();
    let merged = merge(vec![builtins(), vec![custom, extra]]);
    let types: Vec<&str> = merged
        .iter()
        .map(|agent| agent.agent_type.as_str())
        .collect();
    assert_eq!(types, ["general-purpose", "Explore", "Plan", "reviewer"]);
    assert_eq!(merged[0].when_to_use, "Project override.");
    assert_eq!(merged[0].source, AgentSource::Project);
}
