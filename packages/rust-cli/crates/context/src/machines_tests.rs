//! 机器清单提醒的时机与文字（remote-exec.md §20.5.3）。

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::env_registry::{MachineFacts, MachineSet, RemoteMachine};
use rust_cli_kernel_api::ids::{EnvironmentId, MessageId};
use rust_cli_kernel_api::message::{AttachmentMessage, Message};

use super::*;

fn machine(name: &str, root: &str) -> RemoteMachine {
    RemoteMachine {
        name: name.to_owned(),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new(name),
            root: EnvPath::new(root),
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

fn set(machines: Vec<RemoteMachine>) -> MachineSet {
    MachineSet {
        version: 1,
        machines,
    }
}

fn sent((text, data): (String, Value)) -> Message {
    Message::Attachment(AttachmentMessage {
        id: MessageId::new("m"),
        source: SOURCE.to_owned(),
        text,
        data,
    })
}

const HEAD: &str = "Remote machines connected to this session. Tools with a `machine` parameter run on the machine you name; omit it to run on the local machine. Paths on a remote machine are that machine's own paths; relative paths resolve against its working directory.";

#[test]
fn nothing_is_said_without_remote_machines() {
    assert_eq!(announce(&[], &MachineSet::default()), None);
}

#[test]
fn a_connection_sends_the_whole_list_once() {
    let now = set(vec![machine("dev-a", "/home/dev/proj")]);
    let (text, data) = announce(&[], &now).unwrap();
    assert_eq!(
        text,
        format!("{HEAD}\n- dev-a: Linux x86_64, shell /bin/bash, working directory /home/dev/proj")
    );
    assert_eq!(
        data,
        json!({"machines": [{"name": "dev-a", "os": "linux", "arch": "x86_64", "shell": "/bin/bash", "workingDirectory": "/home/dev/proj"}]})
    );
    // 已经说过、没有变化：不再发。
    let history = vec![sent((text, data))];
    assert_eq!(announce(&history, &now), None);
}

#[test]
fn a_change_sends_the_list_with_the_disconnected_ones() {
    let first = set(vec![
        machine("dev-a", "/home/dev/proj"),
        machine("gpu-1", "/data"),
    ]);
    let history = vec![sent(announce(&[], &first).unwrap())];
    let (text, _) = announce(&history, &set(vec![machine("dev-a", "/home/dev/proj")])).unwrap();
    assert_eq!(
        text,
        format!(
            "{HEAD}\n- dev-a: Linux x86_64, shell /bin/bash, working directory /home/dev/proj\n- gpu-1: disconnected"
        )
    );
    // 一台都没有了。
    let history = vec![sent(announce(&[], &first).unwrap())];
    let (text, data) = announce(&history, &MachineSet::default()).unwrap();
    assert_eq!(
        text,
        "No remote machines are connected now; every tool runs on the local machine.\n- dev-a: disconnected\n- gpu-1: disconnected"
    );
    assert_eq!(data, json!({"machines": []}));
    // 之后仍然没有：不再发。
    let history = vec![sent((text, data))];
    assert_eq!(announce(&history, &MachineSet::default()), None);
}

#[test]
fn a_changed_working_directory_is_a_change() {
    let history = vec![sent(
        announce(&[], &set(vec![machine("dev-a", "/home/dev/proj")])).unwrap(),
    )];
    assert!(announce(&history, &set(vec![machine("dev-a", "/srv/app")])).is_some());
}
