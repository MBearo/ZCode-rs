//! 单测用的脚本化执行器：在内存网络的另一头逐帧收发；连接器可以给每次连接注入故障。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_exec_transport::fault::{Faults, Switch, inject};
use rust_cli_exec_transport::memory::{MemoryConnector, MemoryListener, network};
use rust_cli_exec_transport::{Connector, FrameSink, FrameSource, Link, LinkError, Listener};
use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::dto::core::{PathStyle, Workspace};
use rust_cli_exec_wire::envelope::{
    EventHeader, ReplyHeader, ReplyResult, RequestHeader, StreamItem,
};
use rust_cli_exec_wire::frame::{Frame, Kind, Reassembler, Received};
use rust_cli_exec_wire::handshake::{Capabilities, Hello, PROTOCOL, Welcome};
use rust_cli_exec_wire::reply::Reply;
use serde::Serialize;

use crate::{ClientOptions, Reconnect};

pub(crate) fn options() -> ClientOptions {
    let mut options = ClientOptions::new("0.1.0-test", "client-1");
    options.heartbeat = Duration::from_secs(30);
    options
}

pub(crate) fn reconnecting(heartbeat_ms: u64) -> ClientOptions {
    let mut options = options();
    options.heartbeat = Duration::from_millis(heartbeat_ms);
    options.reconnect = Reconnect {
        attempts: 2,
        delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(5),
    };
    options
}

pub(crate) fn workspace() -> Workspace {
    Workspace {
        environment: "local".to_owned(),
        root: "/w".to_owned(),
    }
}

pub(crate) fn welcome(instance: &str, capabilities: Capabilities) -> Welcome {
    Welcome {
        protocol: PROTOCOL,
        server_version: "0.1.0-test".to_owned(),
        server_instance: instance.to_owned(),
        os: "linux".to_owned(),
        arch: "x86_64".to_owned(),
        path_style: PathStyle::Posix,
        home: Some("/home/dev".to_owned()),
        shell: Some("bash".to_owned()),
        capabilities,
        max_message: rust_cli_exec_wire::frame::MAX_MESSAGE,
        ledger: None,
    }
}

/// 连接器：每次连接按顺序取一份故障（没有了就不注入），记下开关。
pub(crate) struct FaultyConnector {
    inner: MemoryConnector,
    faults: Mutex<VecDeque<Faults>>,
    pub switches: Mutex<Vec<Switch>>,
}

impl FaultyConnector {
    pub fn switch(&self, index: usize) -> Switch {
        self.switches.lock().unwrap()[index].clone()
    }
}

#[async_trait]
impl Connector for FaultyConnector {
    async fn connect(&self) -> Result<Box<dyn Link>, LinkError> {
        let link = self.inner.connect().await?;
        let faults = self.faults.lock().unwrap().pop_front().unwrap_or_default();
        let (link, switch) = inject(link, faults);
        self.switches.lock().unwrap().push(switch);
        Ok(link)
    }

    fn describe(&self) -> String {
        self.inner.describe()
    }
}

pub(crate) fn network_with(faults: Vec<Faults>) -> (Arc<FaultyConnector>, MemoryListener) {
    let (connector, listener) = network("exec");
    (
        Arc::new(FaultyConnector {
            inner: connector,
            faults: Mutex::new(faults.into()),
            switches: Mutex::new(Vec::new()),
        }),
        listener,
    )
}

/// 执行器这一头的一条连接。
pub(crate) struct Peer {
    sink: Box<dyn FrameSink>,
    source: Box<dyn FrameSource>,
    reassembler: Reassembler,
}

impl Peer {
    pub async fn accept(listener: &MemoryListener) -> Self {
        let (sink, source) = listener.accept().await.unwrap().split();
        Self {
            sink,
            source,
            reassembler: Reassembler::default(),
        }
    }

    /// 下一段原始字节（不拼装分片）。
    pub async fn raw(&mut self) -> bytes::Bytes {
        self.source.recv().await.unwrap().expect("connection ended")
    }

    /// 下一帧（`None`：连接结束）。
    pub async fn try_frame(&mut self) -> Option<Frame> {
        loop {
            let bytes = self.source.recv().await.ok()??;
            match self.reassembler.push(bytes).unwrap() {
                Received::Frame(frame) => return Some(frame),
                Received::Pending | Received::Unknown(_) => {}
            }
        }
    }

    /// 下一个这种类型的帧（跳过别的）。
    pub async fn next(&mut self, kind: Kind) -> Frame {
        loop {
            let frame = self.try_frame().await.expect("connection ended");
            if frame.kind == kind {
                return frame;
            }
        }
    }

    pub async fn send<T: Serialize>(&mut self, kind: Kind, header: &T, body: &Body) {
        self.sink
            .send(Frame::encode(kind, header, body).unwrap())
            .await
            .unwrap();
    }

    /// 收 hello，回 welcome。
    pub async fn handshake(&mut self, instance: &str, capabilities: Capabilities) -> Hello {
        self.handshake_with(welcome(instance, capabilities)).await
    }

    /// 收 hello，回给定的 welcome（例如带账本身份，remote-exec.md §19.8）。
    pub async fn handshake_with(&mut self, welcome: Welcome) -> Hello {
        let hello: Hello = self.next(Kind::Hello).await.header().unwrap();
        self.send(Kind::Welcome, &welcome, &Body::empty()).await;
        hello
    }

    pub async fn request(&mut self) -> (RequestHeader, Body) {
        let frame = self.next(Kind::Request).await;
        (frame.header().unwrap(), frame.body)
    }

    pub async fn reply(&mut self, call: u64, reply: Reply, body: &Body) {
        let header = ReplyHeader {
            call,
            result: ReplyResult::Ok(reply),
        };
        self.send(Kind::Reply, &header, body).await;
    }

    pub async fn event(&mut self, call: u64, seq: u64, item: StreamItem, body: &Body) {
        self.send(Kind::Event, &EventHeader { call, seq, item }, body)
            .await;
    }
}
