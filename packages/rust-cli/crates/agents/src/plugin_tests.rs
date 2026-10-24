//! 插件 agent（agents.md §3）。

use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::testing::MemoryEnvironment;

use super::{PluginAgentFile, parse};
use crate::context::AgentCatalog;
use crate::definitions::AgentSource;

const ROOT: &str = "/Users/dev/plugins/kit";

fn file(path: &str, namespace: &[&str]) -> PluginAgentFile {
    PluginAgentFile {
        plugin: "kit".to_owned(),
        root: ROOT.to_owned(),
        path: format!("{ROOT}/{path}"),
        namespace: namespace.iter().map(|part| (*part).to_owned()).collect(),
    }
}

#[test]
fn names_descriptions_and_variables_follow_the_plugin_rules() {
    let raw = "---\nname: lead\nwhen_to_use: Leads the team.\ntools: Read\npermissionMode: bypassPermissions\nmaxTurns: 3\n---\nRun ${CLAUDE_PLUGIN_ROOT}/bin/x in ${ZCODE_PROJECT_DIR}.\n";
    let agent = parse(
        raw,
        &file("agents/team/lead.md", &["team"]),
        "/Users/dev/repo",
    );
    assert_eq!(agent.agent_type, "kit:team:lead");
    assert_eq!(agent.when_to_use, "Leads the team.");
    assert_eq!(agent.source, AgentSource::Plugin);
    assert_eq!(agent.tools, Some(vec!["Read".to_owned()]));
    // permissionMode 对插件 agent 无效（会越过安装时的授权）。
    assert_eq!(agent.permission_mode, None);
    assert_eq!(agent.max_turns, Some(3));
    assert_eq!(
        agent.prompt,
        "Run /Users/dev/plugins/kit/bin/x in /Users/dev/repo."
    );
    // 没有 frontmatter：名字取文件名，描述用缺省句。
    let bare = parse(
        "Just do it.",
        &file("agents/helper.md", &[]),
        "/Users/dev/repo",
    );
    assert_eq!(bare.agent_type, "kit:helper");
    assert_eq!(bare.when_to_use, "Agent from kit plugin");
    assert_eq!(bare.prompt, "Just do it.");
}

#[tokio::test]
async fn plugin_agents_sit_between_built_ins_and_user_agents() {
    let facts = EnvFacts {
        cwd: EnvPath::new("/Users/dev/repo"),
        platform: "darwin".to_owned(),
        os_version: "Darwin 24".to_owned(),
        shell: "zsh".to_owned(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    };
    let env = MemoryEnvironment::new(facts)
        .with_file(
            &format!("{ROOT}/agents/review.md"),
            "---\nname: review\ndescription: Plugin review.\n---\nP.",
        )
        .with_file(
            &format!("{ROOT}/agents/explore.md"),
            "---\ndescription: Plugin explore.\n---\nE.",
        )
        .with_file(
            "/Users/dev/.zcode/agents/kit-review.md",
            "---\nname: kit:review\ndescription: User override.\n---\nU.",
        );
    let env = Arc::new(env);
    let catalog = AgentCatalog::new(env.clone(), env);
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/repo"),
    };
    catalog.set_plugins(vec![
        file("agents/review.md", &[]),
        file("agents/explore.md", &[]),
        file("agents/missing.md", &[]),
    ]);
    let found: Vec<(String, String, AgentSource)> = catalog
        .definitions(&workspace)
        .await
        .into_iter()
        .map(|agent| (agent.agent_type, agent.when_to_use, agent.source))
        .collect();
    let tail = &found[3..];
    assert_eq!(
        tail,
        [
            (
                "kit:review".to_owned(),
                "User override.".to_owned(),
                AgentSource::User
            ),
            (
                "kit:explore".to_owned(),
                "Plugin explore.".to_owned(),
                AgentSource::Plugin
            ),
        ]
    );
}
