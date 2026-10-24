//! 本机与连上的远端机器的后台命令通知合成一个流（docs/specs/rust-cli/remote-exec.md §20.7.2）。
//!
//! 每台机器一个 `shell_notices(session, 0)`：本机从一开始就订阅；注册表的 `changes()` 出项时给新连上的机器开流。
//! 某台机器的流出错或结束就丢掉它；丢掉时如果注册表在它开流之后变过（那台机器重连了），马上重开。合成的流不会结束：
//! 没有可等的机器时一直等下一次连接变化。没有远端机器时它就是本机的通知流。

use std::collections::HashMap;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream, Fuse, SelectAll};

use crate::env::EnvError;
use crate::env_registry::{EnvironmentRegistry, LOCAL_MACHINE};
use crate::env_shell::ShellNotice;
use crate::ids::SessionId;
use crate::machine::not_connected;

/// 一条带机器名的通知：ack 要发给这台机器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineNotice {
    pub machine: String,
    pub notice: ShellNotice,
}

enum Item {
    Notice(MachineNotice),
    Ended(String),
}

struct Merged {
    registry: Arc<dyn EnvironmentRegistry>,
    session: SessionId,
    changes: Fuse<BoxStream<'static, u64>>,
    streams: SelectAll<BoxStream<'static, Item>>,
    /// 开着的流：机器名 → 开流时注册表的 version。
    open: HashMap<String, u64>,
}

impl Merged {
    fn subscribe(&mut self, name: &str, version: u64) {
        let Some(machine) = self.registry.machine(name) else {
            return;
        };
        let ended = name.to_owned();
        // 流里的错误（执行环境不提供通知、连接断了）结束这台机器的流；kernel-api 不记日志，订阅方看不到这条错误。
        let notices = machine
            .shell()
            .shell_notices(&self.session, 0)
            .take_while(|item| std::future::ready(item.is_ok()))
            .filter_map({
                let name = name.to_owned();
                move |item| {
                    let name = name.clone();
                    std::future::ready(item.ok().map(|notice| {
                        Item::Notice(MachineNotice {
                            machine: name,
                            notice,
                        })
                    }))
                }
            })
            .chain(stream::once(std::future::ready(Item::Ended(ended))));
        self.streams.push(notices.boxed());
        self.open.insert(name.to_owned(), version);
    }

    /// 给本机与连上、但还没开流的机器开流。
    fn open_new(&mut self) {
        let set = self.registry.machines();
        let mut names = vec![LOCAL_MACHINE.to_owned()];
        names.extend(set.names());
        for name in names {
            if !self.open.contains_key(&name) {
                self.subscribe(&name, set.version);
            }
        }
    }

    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Option<MachineNotice>> {
        loop {
            while let Poll::Ready(Some(_)) = self.changes.poll_next_unpin(cx) {
                self.open_new();
            }
            match self.streams.poll_next_unpin(cx) {
                Poll::Ready(Some(Item::Notice(notice))) => return Poll::Ready(Some(notice)),
                Poll::Ready(Some(Item::Ended(name))) => {
                    let opened = self.open.remove(&name).unwrap_or(0);
                    if name != LOCAL_MACHINE && self.registry.machines().version > opened {
                        self.open_new();
                    }
                }
                // 没有开着的流：等下一次连接变化（上面已经登记了唤醒）。
                Poll::Ready(None) | Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// 会话 `session` 在本机与全部连上的远端机器上的后台命令通知。
pub fn all_shell_notices(
    registry: Arc<dyn EnvironmentRegistry>,
    session: SessionId,
) -> BoxStream<'static, MachineNotice> {
    let changes = registry.changes().fuse();
    let mut merged = Merged {
        registry,
        session,
        changes,
        streams: SelectAll::new(),
        open: HashMap::new(),
    };
    merged.open_new();
    stream::poll_fn(move |cx| merged.poll(cx)).boxed()
}

/// 把通知 ack 给它所在的机器；那台机器没连上时是“没连上”的 io 错误（重连后通知会再送一次）。
pub async fn ack(
    registry: &dyn EnvironmentRegistry,
    notice: &MachineNotice,
    session: &SessionId,
) -> Result<(), EnvError> {
    let Some(machine) = registry.machine(&notice.machine) else {
        return Err(EnvError::Io {
            message: not_connected(&notice.machine),
        });
    };
    machine
        .shell()
        .ack_shell_notice(session, notice.notice.seq)
        .await
}

#[cfg(test)]
#[path = "machine_notices_tests.rs"]
mod tests;
