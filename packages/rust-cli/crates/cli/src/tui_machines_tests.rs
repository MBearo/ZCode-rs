//! 换算（remote-exec.md §21.2、§21.4）：状态机的状态、连接错误与清单的错误换成 TUI 的类型。

use std::time::Duration;

use rust_cli_machines::{MachineEntry, MachineError, MachineState, MachinesError, Welcomed};
use rust_cli_tui::machines::{MachineFailure, MachineProblem, MachineStatus};

use super::{problem, row};
use crate::machines::{FleetError, Seen};

fn welcomed() -> Welcomed {
    Welcomed {
        server_version: "0.1.58".to_owned(),
        server_instance: "exec-1".to_owned(),
        os: "linux".to_owned(),
        arch: "x86_64".to_owned(),
    }
}

#[test]
fn states_map_to_the_statuses_the_tui_shows() {
    let cases = [
        (MachineState::Unconnected, MachineStatus::Disconnected),
        (MachineState::Provisioning, MachineStatus::Connecting),
        (MachineState::Handshaking, MachineStatus::Connecting),
        (MachineState::Ready(welcomed()), MachineStatus::Ready),
        (
            MachineState::Reconnecting { attempt: 3 },
            MachineStatus::Reconnecting { attempt: 3 },
        ),
        (
            MachineState::Failed {
                error: MachineError::Incompatible {
                    version: "0.1.40".to_owned(),
                    protocol: 0,
                    busy: true,
                },
            },
            MachineStatus::Failed(MachineFailure::Incompatible {
                version: "0.1.40".to_owned(),
                protocol: 0,
                busy: true,
            }),
        ),
    ];
    for (state, status) in cases {
        let seen = Seen {
            entry: MachineEntry::new("dev-a", "user@dev-a", Some("/srv/proj")),
            state,
            latency: Some(Duration::from_micros(12_400)),
            welcome: None,
        };
        let shown = row(seen);
        assert_eq!(shown.status, status);
        assert_eq!(shown.latency_ms, Some(12));
        assert_eq!(shown.ssh, "user@dev-a");
        assert_eq!(shown.working_directory.as_deref(), Some("/srv/proj"));
        assert!(shown.executor.is_none());
    }
}

#[test]
fn problems_keep_their_kind() {
    let cases = [
        (
            FleetError::Connection(MachineError::Unreachable("refused".to_owned())),
            MachineProblem::Connection(MachineFailure::Unreachable("refused".to_owned())),
        ),
        (
            FleetError::List(MachinesError::Name("local".to_owned())),
            MachineProblem::InvalidName("local".to_owned()),
        ),
        (
            FleetError::List(MachinesError::Exists("a".to_owned())),
            MachineProblem::Exists("a".to_owned()),
        ),
        (
            FleetError::List(MachinesError::Missing("a".to_owned())),
            MachineProblem::Missing("a".to_owned()),
        ),
    ];
    for (error, wanted) in cases {
        assert_eq!(problem(error), wanted);
    }
    let file = problem(FleetError::List(MachinesError::Write {
        path: "/home/dev/.zcode/rust-cli/machines.json".to_owned(),
        message: "read-only".to_owned(),
    }));
    assert!(
        matches!(&file, MachineProblem::File(text) if text.contains("read-only")),
        "{file:?}"
    );
}
