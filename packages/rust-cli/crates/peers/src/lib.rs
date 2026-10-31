//! 本机跨会话通信的适配器（docs/specs/rust-cli/cross-session.md §2）：实现 kernel-api 的 [`PeerMessaging`]。
//!
//! 进程级的事实在这里：自己的名字与地址、登记（`<peers>/<pid>.json`）、收件箱 socket、绑定的主会话、还没 ack 的入站、
//! 发出去还没终态的 msg_id。会话级的事实（收到的消息、扣住、解决、回执）只在会话 actor，先提交再 ack。
//! IO：登记文件经 `rust-cli-fs`，socket 经 `rust-cli-net::uds`；路径由入口从 `StoragePaths::peers` 给出。

mod frames;
mod inbox;
mod registry;
mod server;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use rust_cli_base::{Spawner, Task};
use rust_cli_kernel_api::clock::{Clock, Ids};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::peers::{
    PeerAddress, PeerIdentity, PeerMessaging, PeerOutbound, PeerRecord, PeerSendError, PeerSignal,
    PeerStatus, ReceiptStatus, name_prefix,
};
use tokio::sync::watch;

use crate::frames::{Frame, PEER_PROTOCOL, Record, Reply, UserBody, line};
use crate::inbox::Inbox;

/// Unix 域套接字路径的上限（字节）；超过时退回私有临时目录。
const SOCKET_PATH_LIMIT: usize = 103;
/// 名字后缀的随机十六进制位数（1 字节）。
const NAME_SUFFIX_HEX_DIGITS: usize = 2;

/// 入口给出的位置与能力。
pub struct PeerConfig {
    /// 登记目录（`StoragePaths::peers`）。
    pub registry: PathBuf,
    /// socket 路径太长时依次退回的目录（`<tmp>/zcode-<uid>/peers`、`/tmp/zcode-<uid>/peers`）；入口已建好并核对过
    /// 属主（proc 的私有目录）。
    pub fallback_sockets: Vec<PathBuf>,
    pub pid: u32,
    /// 进程启动时的工作目录（登记的 `cwd`，名字取它的目录名）。
    pub cwd: String,
    pub version: String,
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn Ids>,
    pub spawner: Spawner,
}

/// 本机实现；绑定失败或平台不支持时整体关闭（同 [`rust_cli_kernel_api::peers::NoPeers`]）。
pub struct LocalPeers {
    live: Option<Arc<Live>>,
}

struct Live {
    registry: PathBuf,
    socket: PathBuf,
    pid: u32,
    identity: PeerIdentity,
    inbox: Arc<Inbox>,
    record: watch::Sender<Option<Record>>,
    clock: Arc<dyn Clock>,
    spawner: Spawner,
    writer: std::sync::Mutex<Option<Task<Option<()>>>>,
    _server: Task<Option<()>>,
}

/// 第一个放得下的 socket 路径（Unix 的 `sun_path` 上限）；都放不下时 None（跨会话关闭）。
fn socket_path(config: &PeerConfig) -> Option<PathBuf> {
    let file = format!("{}.sock", config.pid);
    std::iter::once(&config.registry)
        .chain(&config.fallback_sockets)
        .map(|dir| dir.join(&file))
        .find(|path| path.as_os_str().len() <= SOCKET_PATH_LIMIT)
}

impl LocalPeers {
    /// 关闭的实现（测试、不支持的平台）。
    pub fn off() -> Self {
        Self { live: None }
    }

    /// 建登记目录、绑定收件箱、写登记、开服务与写登记的任务。任何一步失败都记日志并关闭跨会话。
    pub async fn start(config: PeerConfig) -> Self {
        if !rust_cli_net::uds::supported() {
            return Self::off();
        }
        match Self::bind(config).await {
            Ok(live) => Self {
                live: Some(Arc::new(live)),
            },
            Err(error) => {
                tracing::warn!(%error, "cross-session messaging is off for this process");
                Self::off()
            }
        }
    }

    async fn bind(config: PeerConfig) -> std::io::Result<Live> {
        rust_cli_fs::create_private_dir_all(&config.registry).await?;
        let socket = socket_path(&config).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "no socket directory is short enough for a Unix domain socket path",
            )
        })?;
        if let Some(parent) = socket.parent() {
            rust_cli_fs::create_private_dir_all(parent).await?;
        }
        // 同一个 pid 的旧 socket 只可能属于已经退出的进程。
        rust_cli_fs::remove_optional(&socket).await?;
        let listener = rust_cli_net::uds::UdsListener::bind(&socket)?;
        rust_cli_fs::restrict_to_owner(&socket).await?;
        let socket_text = socket.to_string_lossy().into_owned();
        let directory = Path::new(&config.cwd)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        // 名字 = 目录名的规整 + 1 字节随机 hex（不固定后缀时的写法）。
        let random: String = config
            .ids
            .command()
            .as_str()
            .chars()
            .filter(char::is_ascii_hexdigit)
            .take(NAME_SUFFIX_HEX_DIGITS)
            .collect();
        let name = format!("{}-{}", name_prefix(&directory), random.to_lowercase());
        let now = config.clock.now_ms();
        let record = Record {
            pid: config.pid,
            session_id: None,
            cwd: config.cwd.clone(),
            started_at: now,
            version: config.version.clone(),
            peer_protocol: PEER_PROTOCOL,
            kind: "interactive".to_owned(),
            messaging_socket_path: socket_text.clone(),
            name: name.clone(),
            name_source: "derived".to_owned(),
            status: Some(PeerStatus::Busy.as_str().to_owned()),
            updated_at: now,
            status_updated_at: Some(now),
        };
        registry::write(&config.registry, &record).await?;
        let inbox = Inbox::new();
        let server = config.spawner.spawn_resident(
            "peer-inbox",
            server::serve(
                listener,
                inbox.clone(),
                config.clock.clone(),
                config.spawner.clone(),
            ),
        );
        let (record, changes) = watch::channel(Some(record));
        let writer = config.spawner.spawn_resident(
            "peer-registry",
            write_records(config.registry.clone(), socket.clone(), config.pid, changes),
        );
        tracing::info!(event = "peers.listening", name = %name, "cross-session inbox listening");
        Ok(Live {
            registry: config.registry,
            identity: PeerIdentity {
                name,
                address: PeerAddress::from_socket(&socket_text),
            },
            socket,
            pid: config.pid,
            inbox,
            record,
            clock: config.clock,
            spawner: config.spawner,
            writer: std::sync::Mutex::new(Some(writer)),
            _server: server,
        })
    }

    /// 退出：不再收入站，删掉登记与 socket（写登记的任务做最后一步，之后结束）。
    pub async fn shutdown(&self) {
        let Some(live) = &self.live else { return };
        live.inbox.close();
        live.record.send_replace(None);
        let writer = live
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(writer) = writer {
            let _ = writer.await;
        }
        // 写登记的任务已不在（作用域关闭）时由这里删。
        registry::remove(&live.registry, live.pid, &live.socket).await;
    }
}

/// 唯一写登记文件的任务：每次变化写最新的一份；变成 None 时删掉登记与 socket 并结束。
async fn write_records(
    registry: PathBuf,
    socket: PathBuf,
    pid: u32,
    mut changes: watch::Receiver<Option<Record>>,
) {
    loop {
        if changes.changed().await.is_err() {
            return;
        }
        let latest = changes.borrow_and_update().clone();
        match latest {
            Some(record) => {
                if let Err(error) = registry::write(&registry, &record).await {
                    tracing::warn!(%error, "the peer registry record could not be updated");
                }
            }
            None => {
                registry::remove(&registry, pid, &socket).await;
                return;
            }
        }
    }
}

impl Live {
    fn update(&self, change: impl FnOnce(&mut Record)) {
        let now = self.clock.now_ms();
        self.record.send_if_modified(|record| {
            let Some(record) = record else { return false };
            change(record);
            record.updated_at = now;
            true
        });
    }
}

fn send_error(error: &std::io::Error) -> PeerSendError {
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => PeerSendError::Gone,
        _ => PeerSendError::Failed {
            message: error.to_string(),
        },
    }
}

#[async_trait]
impl PeerMessaging for LocalPeers {
    async fn identity(&self) -> Option<PeerIdentity> {
        self.live.as_ref().map(|live| live.identity.clone())
    }

    async fn peers(&self) -> Vec<PeerRecord> {
        match &self.live {
            Some(live) => registry::list(&live.registry, live.pid).await,
            None => Vec::new(),
        }
    }

    async fn send(&self, message: PeerOutbound) -> Result<(), PeerSendError> {
        let Some(live) = &self.live else {
            return Err(PeerSendError::Failed {
                message: "cross-session messaging is off in this session".to_owned(),
            });
        };
        live.inbox.record_send(&message.msg_id, &message.to);
        let frame = Frame::User {
            msg_id: message.msg_id.clone(),
            from: Some(live.identity.address.as_str().to_owned()),
            message: UserBody {
                role: "user".to_owned(),
                content: message.envelope,
            },
            priority: Some("next".to_owned()),
        };
        let socket = PathBuf::from(message.to.socket());
        let result = match rust_cli_net::uds::request(&socket, &line(&frame)).await {
            Ok(Some(reply)) => match serde_json::from_str::<Reply>(&reply) {
                Ok(Reply::Accepted) => Ok(()),
                Ok(Reply::Refused { reason }) => Err(PeerSendError::Refused { reason }),
                Ok(Reply::Ignored) | Err(_) => Err(PeerSendError::Failed {
                    message: format!("unexpected reply: {reply}"),
                }),
            },
            // 对方没回就关闭：多半正在退出。
            Ok(None) => Err(PeerSendError::Gone),
            Err(error) => Err(send_error(&error)),
        };
        if result.is_err() {
            live.inbox.forget_send(&message.msg_id);
        }
        result
    }

    async fn receipt(&self, to: PeerAddress, msg_id: String, status: ReceiptStatus) {
        let Some(live) = &self.live else { return };
        let frame = Frame::Control {
            action: "peer_message_status".to_owned(),
            orig_msg_id: Some(msg_id),
            status: Some(status.as_str().to_owned()),
        };
        let text = line(&frame);
        live.spawner.spawn("peer-receipt", async move {
            let socket = PathBuf::from(to.socket());
            if let Err(error) = rust_cli_net::uds::request(&socket, &text).await {
                // 对方已退出：回执丢掉。
                tracing::debug!(%error, to = %to.as_str(), "a cross-session receipt was not delivered");
            }
        });
    }

    async fn bind(&self, session: &SessionId) {
        let Some(live) = &self.live else { return };
        live.inbox.bind(session);
        live.update(|record| record.session_id = Some(session.to_string()));
    }

    async fn unbind(&self, session: &SessionId) {
        let Some(live) = &self.live else { return };
        live.inbox.unbind(session);
    }

    fn signals(&self, session: &SessionId) -> BoxStream<'static, PeerSignal> {
        match &self.live {
            Some(live) => live.inbox.subscribe(session),
            None => Box::pin(futures_util::stream::pending()),
        }
    }

    async fn ack(&self, _session: &SessionId, seq: u64) {
        if let Some(live) = &self.live {
            live.inbox.ack(seq);
        }
    }

    async fn set_status(&self, session: &SessionId, status: PeerStatus) {
        let Some(live) = &self.live else { return };
        if live.inbox.bound().as_ref() != Some(session) {
            return;
        }
        let now = live.clock.now_ms();
        live.update(|record| {
            if record.status.as_deref() != Some(status.as_str()) {
                record.status = Some(status.as_str().to_owned());
                record.status_updated_at = Some(now);
            }
        });
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
