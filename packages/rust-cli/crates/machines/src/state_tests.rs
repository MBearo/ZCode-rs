//! 转移表（remote-exec.md §19.10）：合法的每一条与不合法的组合。

use rust_cli_exec_client::ClientError;
use rust_cli_exec_transport::ProvisionError;
use rust_cli_exec_wire::handshake::RefuseCode;

use crate::{MachineError, MachineEvent as E, MachineState as S, Retry, Welcomed, transition};

fn welcomed(instance: &str) -> Welcomed {
    Welcomed {
        server_version: "0.1.0".to_owned(),
        server_instance: instance.to_owned(),
        os: "linux".to_owned(),
        arch: "x86_64".to_owned(),
    }
}

fn lost() -> MachineError {
    MachineError::Lost("gone".to_owned())
}

/// 重连中（F96，remote-exec.md §23.4）：第几次、下一次的时刻（等着时有）、最近一次的原因。
fn retrying(attempt: u32, waiting: bool, reason: &str) -> Retry {
    Retry {
        attempt,
        retry_at: waiting.then(|| tokio::time::Instant::now() + std::time::Duration::from_secs(60)),
        reason: reason.to_owned(),
    }
}

fn reconnecting(retry: &Retry) -> S {
    S::Reconnecting {
        attempt: retry.attempt,
        retry_at: retry.retry_at,
        reason: retry.reason.clone(),
    }
}

#[test]
fn the_happy_path_and_reconnecting() {
    let mut state = S::Unconnected;
    let (first, second, third) = (
        retrying(0, true, "link closed"),
        retrying(1, false, "link closed"),
        retrying(1, true, "cannot connect: refused"),
    );
    for (event, expected) in [
        (E::Connect, S::Provisioning),
        (E::Provisioned, S::Handshaking),
        (E::Welcomed(welcomed("a")), S::Ready(welcomed("a"))),
        (E::LinkLost, reconnecting(&retrying(0, false, ""))),
        (E::Retrying(first.clone()), reconnecting(&first)),
        (E::Retrying(second.clone()), reconnecting(&second)),
        (E::Retrying(third.clone()), reconnecting(&third)),
        (E::Reattached(welcomed("b")), S::Ready(welcomed("b"))),
        (E::Reattached(welcomed("c")), S::Ready(welcomed("c"))),
        (E::GaveUp(lost()), S::Failed { error: lost() }),
        (E::Connect, S::Provisioning),
        (E::Disconnect, S::Unconnected),
    ] {
        state = transition(&state, event).unwrap();
        assert_eq!(state, expected);
    }
}

#[test]
fn failures_lead_to_failed_and_disconnect_always_resets() {
    let unsupported = MachineError::Unsupported {
        os: "Darwin".to_owned(),
        arch: "arm64".to_owned(),
    };
    assert_eq!(
        transition(&S::Provisioning, E::ProvisionFailed(unsupported.clone())),
        Some(S::Failed { error: unsupported })
    );
    assert_eq!(
        transition(&S::Handshaking, E::HandshakeFailed(lost())),
        Some(S::Failed { error: lost() })
    );
    for state in [
        S::Unconnected,
        S::Provisioning,
        S::Handshaking,
        S::Ready(welcomed("a")),
        reconnecting(&retrying(3, true, "x")),
        S::Failed { error: lost() },
    ] {
        assert_eq!(transition(&state, E::Disconnect), Some(S::Unconnected));
    }
}

#[test]
fn other_combinations_are_ignored() {
    for (state, event) in [
        (S::Unconnected, E::Provisioned),
        (S::Unconnected, E::LinkLost),
        (S::Provisioning, E::Connect),
        (S::Provisioning, E::Welcomed(welcomed("a"))),
        (S::Handshaking, E::Provisioned),
        (S::Ready(welcomed("a")), E::Connect),
        (S::Ready(welcomed("a")), E::Retrying(retrying(1, true, "x"))),
        (
            reconnecting(&retrying(1, false, "x")),
            E::Welcomed(welcomed("a")),
        ),
        (S::Failed { error: lost() }, E::LinkLost),
        (S::Unconnected, E::GaveUp(lost())),
    ] {
        assert_eq!(
            transition(&state, event.clone()),
            None,
            "{state:?} + {event:?}"
        );
    }
}

#[test]
fn errors_are_converted() {
    assert_eq!(
        MachineError::from(ProvisionError::Incompatible {
            version: "0.9".to_owned(),
            protocol: 2,
            busy: true
        }),
        MachineError::Incompatible {
            version: "0.9".to_owned(),
            protocol: 2,
            busy: true
        }
    );
    assert_eq!(
        MachineError::from(ProvisionError::Unreachable {
            target: "dev-a".to_owned(),
            message: "Permission denied".to_owned()
        }),
        MachineError::Unreachable("Permission denied".to_owned())
    );
    assert_eq!(
        MachineError::from(ClientError::Refused {
            code: RefuseCode::Protocol,
            message: "protocol 2".to_owned()
        }),
        MachineError::Refused("protocol 2".to_owned())
    );
    assert!(matches!(
        MachineError::from(ClientError::Disconnected("x".to_owned())),
        MachineError::Lost(_)
    ));
    // F96（remote-exec.md §23.4）：重连被目标拒绝（ssh 认证失败等）同部署时的“到不了”。
    assert_eq!(
        MachineError::from(ClientError::Rejected(
            "Permission denied (publickey).".to_owned()
        )),
        MachineError::Unreachable("Permission denied (publickey).".to_owned())
    );
}

/// F98（remote-exec.md §25.4.2）：部署时的上传是单独一步：探测之后、握手之前；上传失败同部署失败，断开随时回到未连。
#[test]
fn uploading_is_a_step_between_probing_and_the_handshake() {
    let uploading = S::Uploading { bytes: 93_000_000 };
    assert_eq!(
        transition(&S::Provisioning, E::Uploading { bytes: 93_000_000 }),
        Some(uploading.clone())
    );
    assert_eq!(transition(&uploading, E::Provisioned), Some(S::Handshaking));
    let failed = MachineError::Upload("sha256 mismatch".to_owned());
    assert_eq!(
        transition(&uploading, E::ProvisionFailed(failed.clone())),
        Some(S::Failed { error: failed })
    );
    assert_eq!(transition(&uploading, E::Disconnect), Some(S::Unconnected));
    for (state, event) in [
        (uploading, E::Welcomed(welcomed("a"))),
        (S::Handshaking, E::Uploading { bytes: 1 }),
        (S::Ready(welcomed("a")), E::Uploading { bytes: 1 }),
    ] {
        assert_eq!(transition(&state, event), None);
    }
}
