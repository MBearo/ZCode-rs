//! 驱动（remote-exec.md §19.10）对着内存的 Endpoint 与假执行器：部署失败、握手被拒绝、连上、断线重连、次数用完、断开。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_exec_client::{ClientOptions, Reconnect};
use rust_cli_exec_transport::fault::{Faults, Switch, inject};
use rust_cli_exec_transport::memory::{MemoryConnector, MemoryListener, network};
use rust_cli_exec_transport::{
    Connector, Link, LinkError, Listener, ProvisionError, Provisioned, Provisioner,
};
use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::dto::core::PathStyle;
use rust_cli_exec_wire::envelope::Beat;
use rust_cli_exec_wire::frame::{Frame, Kind, Reassembler, Received};
use rust_cli_exec_wire::handshake::{Capabilities, PROTOCOL, Refuse, RefuseCode, Welcome};
use tokio::sync::watch;

use crate::{Endpoint, Machine, MachineError, MachineState};

/// 部署：按顺序给出结果（用完之后都成功）。
struct Steps(Mutex<Vec<Result<Provisioned, ProvisionError>>>);

#[async_trait]
impl Provisioner for Steps {
    async fn provision(&self) -> Result<Provisioned, ProvisionError> {
        let mut steps = self.0.lock().unwrap();
        if steps.is_empty() {
            return Ok(provisioned());
        }
        steps.remove(0)
    }
}

fn provisioned() -> Provisioned {
    Provisioned {
        program: "/remote/zcode-rs".to_owned(),
        args: vec!["executor".to_owned(), "--stdio".to_owned()],
    }
}

/// 连接器：内存网络，每条连接可以被测试切断。
struct Cuttable {
    /// 测试可以换成另一个“网络”（执行器回来了）。
    inner: Mutex<Arc<MemoryConnector>>,
    switches: Mutex<Vec<Switch>>,
}

#[async_trait]
impl Connector for Cuttable {
    async fn connect(&self) -> Result<Box<dyn Link>, LinkError> {
        let inner = self.inner.lock().unwrap().clone();
        let (link, switch) = inject(inner.connect().await?, Faults::new());
        self.switches.lock().unwrap().push(switch);
        Ok(link)
    }

    fn describe(&self) -> String {
        "memory".to_owned()
    }
}

struct TestEndpoint {
    steps: Steps,
    connector: Arc<Cuttable>,
}

impl Endpoint for TestEndpoint {
    fn describe(&self) -> String {
        "test".to_owned()
    }

    fn provisioner(&self) -> &dyn Provisioner {
        &self.steps
    }

    fn connector(&self, provisioned: &Provisioned) -> Result<Arc<dyn Connector>, LinkError> {
        assert_eq!(provisioned, &self::provisioned());
        Ok(self.connector.clone())
    }
}

fn welcome(instance: &str) -> Welcome {
    Welcome {
        protocol: PROTOCOL,
        server_version: "0.1.0".to_owned(),
        server_instance: instance.to_owned(),
        os: "linux".to_owned(),
        arch: "x86_64".to_owned(),
        path_style: PathStyle::Posix,
        home: None,
        shell: None,
        capabilities: Capabilities::ports(),
        max_message: rust_cli_exec_wire::frame::MAX_MESSAGE,
        ledger: None,
    }
}

/// 假执行器：每条连接收 hello、回 welcome（或拒绝），然后读到连接结束。
fn serve(listener: MemoryListener, refuse: bool) -> rust_cli_base::OwnedTask<()> {
    rust_cli_base::owned(async move {
        // 每条连接一个任务，随服务端的任务一起结束。
        let mut connections = Vec::new();
        while let Ok(link) = listener.accept().await {
            let (mut sink, mut source) = link.split();
            connections.push(rust_cli_base::owned(async move {
                let mut reassembler = Reassembler::default();
                let _hello = source.recv().await;
                let answer = if refuse {
                    let refuse = Refuse {
                        code: RefuseCode::Auth,
                        message: "not allowed".to_owned(),
                    };
                    Frame::encode(Kind::Refuse, &refuse, &Body::empty()).unwrap()
                } else {
                    Frame::encode(Kind::Welcome, &welcome("exec-1"), &Body::empty()).unwrap()
                };
                let _ = sink.send(answer).await;
                while let Ok(Some(bytes)) = source.recv().await {
                    if let Ok(Received::Frame(frame)) = reassembler.push(bytes)
                        && frame.kind == Kind::Ping
                    {
                        let beat: Beat = frame.header().unwrap();
                        let pong = Frame::encode(Kind::Pong, &beat, &Body::empty()).unwrap();
                        let _ = sink.send(pong).await;
                    }
                }
            }));
        }
    })
}

fn setup(
    steps: Vec<Result<Provisioned, ProvisionError>>,
) -> (Machine, Arc<Cuttable>, MemoryListener) {
    let (connector, listener) = network("machine");
    let connector = Arc::new(Cuttable {
        inner: Mutex::new(Arc::new(connector)),
        switches: Mutex::default(),
    });
    let endpoint = Arc::new(TestEndpoint {
        steps: Steps(Mutex::new(steps)),
        connector: connector.clone(),
    });
    let mut options = ClientOptions::new("test", "client-1");
    options.reconnect = Reconnect {
        attempts: 3,
        delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(10),
    };
    (Machine::new(endpoint, options), connector, listener)
}

/// 等状态满足条件，记下看到的每一个。
async fn until(
    states: &mut watch::Receiver<MachineState>,
    seen: &mut Vec<MachineState>,
    done: impl Fn(&MachineState) -> bool,
) {
    loop {
        let current = states.borrow_and_update().clone();
        if seen.last() != Some(&current) {
            seen.push(current.clone());
        }
        if done(&current) {
            return;
        }
        tokio::time::timeout(Duration::from_secs(5), states.changed())
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn a_failed_provision_is_reported_and_a_later_connect_succeeds() {
    let unsupported = ProvisionError::Unsupported {
        os: "Darwin".to_owned(),
        arch: "arm64".to_owned(),
    };
    let (machine, _connector, listener) = setup(vec![Err(unsupported)]);
    let _server = serve(listener, false);
    let mut states = machine.states();
    assert_eq!(
        machine.connect().await.map(|_| ()),
        Err(MachineError::Unsupported {
            os: "Darwin".to_owned(),
            arch: "arm64".to_owned()
        })
    );
    assert!(matches!(machine.state(), MachineState::Failed { .. }));
    let client = machine.connect().await.unwrap();
    assert!(client.is_connected());
    let mut seen = Vec::new();
    until(&mut states, &mut seen, |state| {
        matches!(state, MachineState::Ready(_))
    })
    .await;
    let MachineState::Ready(welcomed) = machine.state() else {
        panic!("not ready");
    };
    assert_eq!(welcomed.server_instance, "exec-1");
    // 已经连着：再 connect 给同一个客户端，不再部署。
    assert!(machine.connect().await.is_ok());
    machine.disconnect().await;
    assert_eq!(machine.state(), MachineState::Unconnected);
}

#[tokio::test]
async fn a_refused_handshake_fails_the_machine() {
    let (machine, _connector, listener) = setup(Vec::new());
    let _server = serve(listener, true);
    let result = machine.connect().await.map(|_| ());
    assert!(
        matches!(result, Err(MachineError::Refused(_))),
        "{result:?}"
    );
    assert!(matches!(
        machine.state(),
        MachineState::Failed {
            error: MachineError::Refused(_)
        }
    ));
}

#[tokio::test]
async fn a_lost_link_reconnects_and_the_state_follows() {
    let (machine, connector, listener) = setup(Vec::new());
    let _server = serve(listener, false);
    let _client = machine.connect().await.unwrap();
    let mut states = machine.states();
    let mut seen = Vec::new();
    until(&mut states, &mut seen, |state| {
        matches!(state, MachineState::Ready(_))
    })
    .await;
    connector.switches.lock().unwrap()[0].cut();
    until(&mut states, &mut seen, |state| {
        matches!(state, MachineState::Reconnecting { .. })
    })
    .await;
    until(&mut states, &mut seen, |state| {
        matches!(state, MachineState::Ready(_))
    })
    .await;
    assert!(connector.switches.lock().unwrap().len() >= 2, "{seen:?}");
}

#[tokio::test]
async fn when_reconnecting_gives_up_the_machine_fails() {
    let (machine, connector, listener) = setup(Vec::new());
    let server = serve(listener, false);
    let _client = machine.connect().await.unwrap();
    let mut states = machine.states();
    // 执行器不在了：重连连不上，次数用完后 Failed。
    drop(server);
    connector.switches.lock().unwrap()[0].cut();
    let mut seen = Vec::new();
    until(&mut states, &mut seen, |state| {
        matches!(state, MachineState::Failed { .. })
    })
    .await;
    assert!(
        seen.iter()
            .any(|state| matches!(state, MachineState::Reconnecting { .. })),
        "{seen:?}"
    );
    assert!(matches!(
        machine.state(),
        MachineState::Failed {
            error: MachineError::Lost(_)
        }
    ));
}
