//! agent 定义 → 给前端的事实（`Kernel::agent_definitions`，docs/specs/rust-cli/agents.md §8 的 PT 增量）。
use super::*;

/// 内置的 agent 换过去：名字、描述与来源照抄，`inherit` 的模型记作没有。
#[test]
fn a_definition_becomes_agent_info_and_inherit_means_no_model() {
    let builtin = rust_cli_agents::builtins()
        .into_iter()
        .next()
        .expect("there is a built-in agent");
    let info = agent_info(&builtin);
    assert_eq!(info.name, builtin.agent_type);
    assert_eq!(info.description, builtin.when_to_use);
    assert_eq!(info.origin, AgentOrigin::BuiltIn);
    assert_eq!(info.tools, builtin.tools);
    let inherit = AgentDefinition {
        model: Some("inherit".to_owned()),
        source: AgentSource::Project,
        ..builtin.clone()
    };
    assert_eq!(agent_info(&inherit).model, None);
    assert_eq!(agent_info(&inherit).origin, AgentOrigin::Project);
    let pinned = AgentDefinition {
        model: Some(rust_cli_base::models::GLM_5.to_owned()),
        ..builtin
    };
    assert_eq!(
        agent_info(&pinned).model.as_deref(),
        Some(rust_cli_base::models::GLM_5)
    );
}
