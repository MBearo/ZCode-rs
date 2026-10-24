//! 转移表（remote-exec.md §19.10）：合法的每一条与不合法的组合。

use rust_cli_exec_client::ClientError;
use rust_cli_exec_transport::ProvisionError;
use rust_cli_exec_wire::handshake::RefuseCode;

use crate::{MachineError, MachineEvent as E, MachineState as S, Welcomed, transition};

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

#[test]
fn the_happy_path_and_reconnecting() {
    let mut state = S::Unconnected;
    for (event, expected) in [
        (E::Connect, S::Provisioning),
        (E::Provisioned, S::Handshaking),
        (E::Welcomed(welcomed("a")), S::Ready(welcomed("a"))),
        (E::LinkLost, S::Reconnecting { attempt: 0 }),
        (E::Retrying(1), S::Reconnecting { attempt: 1 }),
        (E::Retrying(2), S::Reconnecting { attempt: 2 }),
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
        S::Reconnecting { attempt: 3 },
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
        (S::Ready(welcomed("a")), E::Retrying(1)),
        (S::Reconnecting { attempt: 1 }, E::Welcomed(welcomed("a"))),
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
}
