//! 插件的 agent 与 hooks 接线（ext.md §4，L12）。

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_ext::{Plugin, PluginAgentFile as ExtAgentFile, Plugins};
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{CommandId, EnvironmentId};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::session_api::{
    Command, CommandEnvelope, CreateSession, Delivery, Kernel, KernelEvent,
};
use rust_cli_kernel_api::testing::{
    FixedClock, MemoryEnvironment, MemoryStore, ScriptedModel, SequentialIds, environments,
    selection, text_response,
};
use serde_json::json;

use super::*;
use crate::{CompactConfig, ContextConfig, Defaults, LocalKernel, Ports, Surface};
use rust_cli_agents::MemoryTaskLists;
use rust_cli_kernel_api::permission::PermissionMode;

const KIT: &str = "/Users/dev/plugins/kit";

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/work"),
    }
}

#[test]
fn plugin_parts_carry_the_plugin_name_and_root() {
    let plugins = Plugins(vec![Plugin {
        name: "kit".to_owned(),
        dir: KIT.to_owned(),
        manifest: json!({}),
        skill_dirs: Vec::new(),
        mcp_servers: Vec::new(),
        agent_files: vec![ExtAgentFile {
            path: format!("{KIT}/agents/team/lead.md"),
            namespace: vec!["team".to_owned()],
        }],
        hooks: vec![
            json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "a.sh"}]}]}}),
        ],
    }]);
    let (agents, hooks) = plugin_parts(&plugins);
    assert_eq!(
        agents,
        [rust_cli_agents::PluginAgentFile {
            plugin: "kit".to_owned(),
            root: KIT.to_owned(),
            path: format!("{KIT}/agents/team/lead.md"),
            namespace: vec!["team".to_owned()],
        }]
    );
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0].root, KIT);
}

/// 插件 agent 经 `start_extensions` 进入主线程的 agent 清单（context 与 Agent 工具共用一个目录）。
#[tokio::test]
async fn plugin_agents_reach_the_agent_listing() {
    let env = MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/Users/dev/work"),
        platform: "darwin".to_owned(),
        os_version: "Darwin 24".to_owned(),
        shell: "zsh".to_owned(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    })
    .with_file(
        &format!("{KIT}/agents/helper.md"),
        "---\ndescription: Plugin helper.\n---\nHelp.",
    );
    let store = Arc::new(MemoryStore::default());
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let kernel = LocalKernel::with_extensions(
        Ports {
            model,
            environments: environments(Arc::new(env)),
            store: store.clone(),
            artifacts: store,
            clock: Arc::new(FixedClock(1_000)),
            ids: Arc::new(SequentialIds::default()),
        },
        Defaults {
            selection: selection(),
            mode: PermissionMode::Default,
            max_turns: None,
            search_tools: false,
            task_tools: None,
            limits: Default::default(),
        },
        ContextConfig::new(Surface::Print),
        CompactConfig::default(),
        Arc::new(MemoryTaskLists::default()),
        Extensions {
            config: ExtConfig {
                plugin_dirs: vec![EnvPath::new(KIT)],
                workspace: Some(workspace()),
                ..ExtConfig::default()
            },
            cron_store: None,
            peers: rust_cli_session::PeerDeps::off(),
            streaming_input: false,
            monitor: false,
        },
    );
    kernel.start_extensions().await;
    let id = kernel
        .create_session(CreateSession {
            id: None,
            workspace: workspace(),
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
        })
        .await
        .unwrap();
    let session = kernel.open_session(&id).await.unwrap();
    let mut events = session.subscribe(Some(0));
    session
        .send(CommandEnvelope {
            id: CommandId::new("c1"),
            command: Command::Submit {
                text: "hello".to_owned(),
                attachments: Vec::new(),
                delivery: Delivery::Queue,
            },
        })
        .await;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
            .unwrap();
        if matches!(event.event, KernelEvent::TurnFinished { .. }) {
            break;
        }
    }
    let listing = session
        .view()
        .await
        .unwrap()
        .messages
        .into_iter()
        .find_map(|message| match message {
            Message::Attachment(attachment) if attachment.source == "agent_listing_delta" => {
                Some(attachment.text)
            }
            _ => None,
        })
        .unwrap();
    assert!(
        listing.contains("- kit:helper: Plugin helper. (Tools: All tools)"),
        "{listing}"
    );
}
