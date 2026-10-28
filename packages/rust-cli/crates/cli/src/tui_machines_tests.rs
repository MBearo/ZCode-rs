//! 换算（remote-exec.md §21.2、§21.4）：状态机的状态、连接错误与清单的错误换成 TUI 的类型。

use std::time::Duration;

use rust_cli_machines::{MachineEntry, MachineError, MachineState, MachinesError, Welcomed};
use rust_cli_tui::machines::{ConnectStep, MachineFailure, MachineProblem, MachineStatus};

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
        // 第一次连接分步（F98，remote-exec.md §25.4.2）。
        (
            MachineState::Provisioning,
            MachineStatus::Connecting(ConnectStep::Checking),
        ),
        // F100（remote-exec.md §26.6）：下载执行器。
        (
            MachineState::Downloading,
            MachineStatus::Connecting(ConnectStep::Downloading),
        ),
        (
            MachineState::Uploading { bytes: 93_323_264 },
            MachineStatus::Connecting(ConnectStep::Uploading { bytes: 93_323_264 }),
        ),
        (
            MachineState::Failed {
                error: MachineError::DownloadFailed {
                    version: "0.1.61".to_owned(),
                    target: "x86_64-unknown-linux-gnu".to_owned(),
                    reason: "HTTP 404".to_owned(),
                },
            },
            MachineStatus::Failed(MachineFailure::DownloadFailed {
                version: "0.1.61".to_owned(),
                target: "x86_64-unknown-linux-gnu".to_owned(),
                reason: "HTTP 404".to_owned(),
            }),
        ),
        (
            MachineState::Failed {
                error: MachineError::Musl,
            },
            MachineStatus::Failed(MachineFailure::Musl),
        ),
        (
            MachineState::Failed {
                error: MachineError::OldGlibc {
                    glibc: Some("2.31".to_owned()),
                    needs: "GLIBC_2.34".to_owned(),
                },
            },
            MachineStatus::Failed(MachineFailure::OldGlibc {
                glibc: Some("2.31".to_owned()),
                needs: "GLIBC_2.34".to_owned(),
            }),
        ),
        // F103（remote-exec.md §27.7）：npm 的 latest 说别的协议。
        (
            MachineState::Failed {
                error: MachineError::LatestIncompatible {
                    version: "0.1.64".to_owned(),
                    protocol: 1,
                    latest: "0.1.63".to_owned(),
                    latest_protocol: 2,
                    target: "x86_64-unknown-linux-gnu".to_owned(),
                },
            },
            MachineStatus::Failed(MachineFailure::LatestIncompatible {
                version: "0.1.64".to_owned(),
                protocol: 1,
                latest: "0.1.63".to_owned(),
                latest_protocol: 2,
                target: "x86_64-unknown-linux-gnu".to_owned(),
            }),
        ),
        (
            MachineState::Handshaking,
            MachineStatus::Connecting(ConnectStep::Handshaking),
        ),
        (MachineState::Ready(welcomed()), MachineStatus::Ready),
        (
            // 第 3 次正在连（F96，remote-exec.md §23.4）。
            MachineState::Reconnecting {
                attempt: 3,
                retry_at: None,
                reason: "link closed".to_owned(),
            },
            MachineStatus::Reconnecting {
                attempt: 3,
                retry_at_ms: None,
                reason: "link closed".to_owned(),
            },
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
        let shown = row(seen, (tokio::time::Instant::now(), 1_000_000));
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

/// F96（remote-exec.md §23.4）：等下一次重连时，界面写下一次是第几次（状态机记的是已经开始了几次），下一次的时刻换成
/// 墙上时间（与界面的时钟同一个）。
#[test]
fn a_waiting_reconnect_shows_the_next_attempt_and_its_wall_time() {
    let now = tokio::time::Instant::now();
    let seen = Seen {
        entry: MachineEntry::new("gpu-1", "gpu-1", None),
        state: MachineState::Reconnecting {
            attempt: 2,
            retry_at: Some(now + Duration::from_millis(12_000)),
            reason: "ssh: connect to host gpu-1 port 22: Connection refused".to_owned(),
        },
        latency: None,
        welcome: None,
    };
    assert_eq!(
        row(seen, (now, 1_000_000)).status,
        MachineStatus::Reconnecting {
            attempt: 3,
            retry_at_ms: Some(1_012_000),
            reason: "ssh: connect to host gpu-1 port 22: Connection refused".to_owned(),
        }
    );
}

/// F98（remote-exec.md §25.4.3）：握手报告的 home 交给 TUI（清单没写工作目录时，询问框的“不再询问”写它）。
#[test]
fn the_remote_home_reaches_the_tui() {
    use rust_cli_exec_wire::dto::core::PathStyle;
    use rust_cli_exec_wire::handshake::{Capabilities, PROTOCOL, Welcome};
    let seen = Seen {
        entry: MachineEntry::new("dev-a", "dev-a", None),
        state: MachineState::Ready(welcomed()),
        latency: None,
        welcome: Some(Welcome {
            protocol: PROTOCOL,
            server_version: "0.1.61".to_owned(),
            server_instance: "exec-1".to_owned(),
            os: "linux".to_owned(),
            arch: "aarch64".to_owned(),
            path_style: PathStyle::Posix,
            home: Some("/home/dev".to_owned()),
            shell: None,
            capabilities: Capabilities::ports(),
            max_message: rust_cli_exec_wire::frame::MAX_MESSAGE,
            ledger: None,
        }),
    };
    let shown = row(seen, (tokio::time::Instant::now(), 0));
    assert_eq!(
        shown.executor.and_then(|executor| executor.home).as_deref(),
        Some("/home/dev")
    );
}
