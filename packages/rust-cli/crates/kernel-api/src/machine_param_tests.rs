//! `machine` 参数的形状（remote-exec.md §20.5.1）。

use serde_json::json;

use super::*;
use crate::env::{EnvPath, WorkspaceRef};
use crate::env_files::PathStyle;
use crate::env_registry::{MachineFacts, RemoteMachine};
use crate::ids::EnvironmentId;

fn remote(name: &str) -> RemoteMachine {
    RemoteMachine {
        name: name.to_owned(),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new(name),
            root: EnvPath::new("/home/dev/proj"),
        },
        facts: MachineFacts {
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
            shell: "/bin/bash".to_owned(),
            home: None,
            path_style: PathStyle::Posix,
        },
    }
}

#[test]
fn the_parameter_goes_last_with_local_first() {
    let schema = json!({
        "type": "object",
        "properties": {"command": {"type": "string"}, "timeout": {"type": "number"}},
        "required": ["command"],
        "additionalProperties": false,
    });
    let set = MachineSet {
        version: 2,
        machines: vec![remote("dev-a"), remote("gpu-1")],
    };
    let choices = choices(&set);
    assert_eq!(choices, ["local", "dev-a", "gpu-1"]);
    let extended = with_machine(&schema, &choices, "Machine to run on.");
    let keys: Vec<&String> = extended["properties"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["command", "timeout", "machine"]);
    assert_eq!(
        extended["properties"]["machine"],
        json!({"type": "string", "enum": ["local", "dev-a", "gpu-1"], "description": "Machine to run on."})
    );
    // 其余不变：required 不含它，工具自己的 schema 不被改。
    assert_eq!(extended["required"], json!(["command"]));
    assert!(schema["properties"].get("machine").is_none());
}

#[test]
fn taking_the_parameter_removes_it() {
    let mut input = json!({"command": "pwd", "machine": "dev-a"});
    assert_eq!(take(&mut input), Some(json!("dev-a")));
    assert_eq!(input, json!({"command": "pwd"}));
    assert_eq!(take(&mut input), None);
    assert_eq!(take(&mut json!("not an object")), None);
    assert_eq!(named_in(&json!({"machine": "dev-a"})), Some("dev-a"));
    assert_eq!(named_in(&json!({"machine": "local"})), None);
    assert_eq!(named_in(&json!({})), None);
}

#[test]
fn the_tools_with_the_parameter() {
    for tool in [
        "Bash",
        "Monitor",
        "Read",
        "Edit",
        "Write",
        "NotebookEdit",
        "Grep",
        "Glob",
        "Agent",
    ] {
        assert!(takes_machine(tool), "{tool}");
    }
    for tool in [
        "TaskStop",
        "WebFetch",
        "Skill",
        "EnterWorktree",
        "mcp__s__t",
    ] {
        assert!(!takes_machine(tool), "{tool}");
    }
}
