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
    /// 有值时之后的连接被目标拒绝（F96：ssh 认证失败等，不可恢复）。
    reject: Mutex<Option<String>>,
}

#[async_trait]
impl Connector for Cuttable {
    async fn connect(&self) -> Result<Box<dyn Link>, LinkError> {
        if let Some(message) = self.reject.lock().unwrap().clone() {
            return Err(LinkError::Rejected(message));
        }
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
    serve_recording(listener, refuse, Arc::default())
}

/// 同 [`serve`]，另记下每条连接 hello 里的客户端实例。
fn serve_recording(
    listener: MemoryListener,
    refuse: bool,
    instances: Arc<Mutex<Vec<String>>>,
) -> rust_cli_base::OwnedTask<()> {
    rust_cli_base::owned(async move {
        // 每条连接一个任务，随服务端的任务一起结束。
        let mut connections = Vec::new();
        while let Ok(link) = listener.accept().await {
            let (mut sink, mut source) = link.split();
            let instances = instances.clone();
            connections.push(rust_cli_base::owned(async move {
                let mut reassembler = Reassembler::default();
                if let Ok(Some(bytes)) = source.recv().await
                    && let Ok(Received::Frame(frame)) = reassembler.push(bytes)
                    && let Ok(hello) = frame.header::<rust_cli_exec_wire::handshake::Hello>()
                {
                    instances.lock().unwrap().push(hello.client_instance);
                }
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
        reject: Mutex::default(),
    });
    let endpoint = Arc::new(TestEndpoint {
        steps: Steps(Mutex::new(steps)),
        connector: connector.clone(),
    });
    let mut options = ClientOptions::new("test", "client-1");
    options.reconnect = Reconnect {
        attempts: Some(3),
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
    // F96（remote-exec.md §23.4）：重连中的状态带下一次的时刻（等着的时候）。
    assert!(
        seen.iter().any(|state| matches!(
            state,
            MachineState::Reconnecting {
                retry_at: Some(_),
                ..
            }
        )),
        "{seen:?}"
    );
}

/// F96（remote-exec.md §23.4）：重连被目标拒绝（ssh 认证失败、主机密钥不对、执行器程序不在了）时不再重试，马上
/// `Failed`，原因同部署时的“到不了”。
#[tokio::test]
async fn a_rejected_reconnect_fails_the_machine_at_once() {
    let (machine, connector, listener) = setup(Vec::new());
    let _server = serve(listener, false);
    let _client = machine.connect().await.unwrap();
    let mut states = machine.states();
    *connector.reject.lock().unwrap() = Some("Permission denied (publickey).".to_owned());
    connector.switches.lock().unwrap()[0].cut();
    let mut seen = Vec::new();
    until(&mut states, &mut seen, |state| {
        matches!(state, MachineState::Failed { .. })
    })
    .await;
    assert_eq!(
        machine.state(),
        MachineState::Failed {
            error: MachineError::Unreachable("Permission denied (publickey).".to_owned())
        }
    );
    // 只试了一次（`setup` 给了 3 次）。
    assert!(
        seen.iter().all(|state| !matches!(
            state,
            MachineState::Reconnecting { attempt, .. } if *attempt > 1
        )),
        "{seen:?}"
    );
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

#[tokio::test]
async fn every_new_client_has_its_own_instance() {
    // 缺陷修复（R5 发现）：断开或失败之后再连，新的 ExecClient 沿用了同一个客户端实例，调用 ID 又从 1 起；执行器那边还
    // 留着旧客户端没 ack 的调用与流（键是（实例，调用 ID）），新的调用撞上它们，回 operation_conflict 或接上一个无关的
    // 旧流。现在每个新的客户端一个实例（第一个仍是组装根给的）。
    let (machine, _connector, listener) = setup(Vec::new());
    let instances = Arc::new(Mutex::new(Vec::new()));
    let _server = serve_recording(listener, false, instances.clone());
    machine.connect().await.unwrap();
    machine.disconnect().await;
    machine.connect().await.unwrap();
    let seen = instances.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0], "client-1");
    assert_ne!(seen[1], seen[0]);
}

/// 部署时依次报告这些步（F98 上传、F100 下载），每步之后等测试放行。
struct Uploads(
    Arc<tokio::sync::Notify>,
    Vec<rust_cli_exec_transport::ProvisionStep>,
);

#[async_trait]
impl Provisioner for Uploads {
    async fn provision(&self) -> Result<Provisioned, ProvisionError> {
        self.provision_reporting(&|_| {}).await
    }

    async fn provision_reporting(
        &self,
        progress: &(dyn Fn(rust_cli_exec_transport::ProvisionStep) + Send + Sync),
    ) -> Result<Provisioned, ProvisionError> {
        for step in &self.1 {
            progress(step.clone());
            self.0.notified().await;
        }
        Ok(provisioned())
    }
}

struct UploadingEndpoint {
    uploads: Uploads,
    connector: Arc<Cuttable>,
}

impl Endpoint for UploadingEndpoint {
    fn describe(&self) -> String {
        "test".to_owned()
    }

    fn provisioner(&self) -> &dyn Provisioner {
        &self.uploads
    }

    fn connector(&self, _provisioned: &Provisioned) -> Result<Arc<dyn Connector>, LinkError> {
        Ok(self.connector.clone())
    }
}

/// F98（remote-exec.md §25.4.2、§25.4.1）：第一次连接时上传是单独的状态（带大小），之后握手、就绪；没在等重连时“立即重试”
/// 不做事。旧代码部署时只有 `Provisioning`。
#[tokio::test]
async fn the_upload_shows_as_its_own_state() {
    let (connector, listener) = network("machine");
    let _server = serve(listener, false);
    let release = Arc::new(tokio::sync::Notify::new());
    let endpoint = Arc::new(UploadingEndpoint {
        uploads: Uploads(
            release.clone(),
            vec![rust_cli_exec_transport::ProvisionStep::Uploading { bytes: 93_323_264 }],
        ),
        connector: Arc::new(Cuttable {
            inner: Mutex::new(Arc::new(connector)),
            switches: Mutex::default(),
            reject: Mutex::default(),
        }),
    });
    let machine = Arc::new(Machine::new(
        endpoint,
        ClientOptions::new("test", "client-1"),
    ));
    assert!(!machine.retry_now(), "nothing to retry before connecting");
    let mut states = machine.states();
    let connecting = rust_cli_base::owned({
        let machine = machine.clone();
        async move { machine.connect().await.map(drop) }
    });
    tokio::time::timeout(
        Duration::from_secs(5),
        states.wait_for(|state| *state == MachineState::Uploading { bytes: 93_323_264 }),
    )
    .await
    .unwrap()
    .unwrap();
    release.notify_one();
    connecting.await.unwrap().unwrap();
    assert!(matches!(machine.state(), MachineState::Ready(_)));
    assert!(!machine.retry_now(), "nothing to retry while ready");
    machine.disconnect().await;
}

/// F100（remote-exec.md §26.6）：本机缓存里没有执行器时，第一次连接先是“下载”，再是“上传”，之后握手、就绪。
#[tokio::test]
async fn the_download_shows_as_its_own_state_before_the_upload() {
    use rust_cli_exec_transport::ProvisionStep;
    let (connector, listener) = network("machine");
    let _server = serve(listener, false);
    let release = Arc::new(tokio::sync::Notify::new());
    let endpoint = Arc::new(UploadingEndpoint {
        uploads: Uploads(
            release.clone(),
            vec![
                ProvisionStep::Downloading,
                ProvisionStep::Uploading { bytes: 42 },
            ],
        ),
        connector: Arc::new(Cuttable {
            inner: Mutex::new(Arc::new(connector)),
            switches: Mutex::default(),
            reject: Mutex::default(),
        }),
    });
    let machine = Arc::new(Machine::new(
        endpoint,
        ClientOptions::new("test", "client-1"),
    ));
    let mut states = machine.states();
    let connecting = rust_cli_base::owned({
        let machine = machine.clone();
        async move { machine.connect().await.map(drop) }
    });
    for wanted in [
        MachineState::Downloading,
        MachineState::Uploading { bytes: 42 },
    ] {
        tokio::time::timeout(
            Duration::from_secs(5),
            states.wait_for(|state| *state == wanted),
        )
        .await
        .unwrap()
        .unwrap();
        release.notify_one();
    }
    connecting.await.unwrap().unwrap();
    assert!(matches!(machine.state(), MachineState::Ready(_)));
    machine.disconnect().await;
}
