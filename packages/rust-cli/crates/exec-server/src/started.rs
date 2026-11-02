//! 命令从启动到线上第一个 `watch_command` 之间的事件流（工作线 F119，remote-exec.md §18.5）。
//!
//! 端口的 `start_command` 直接给这条命令的事件流（执行侧只登记还在跑的命令，结束后不能再按 ID 查）；线上仍是
//! `start_command` 加 `watch_command(op, 0)` 两步（wire 不随内部 API 变）。执行器拿着端口给的流，同一个客户端实例的
//! 第一个 `watch_command(op, 0)` 接走它：命令很快结束（例如 `echo` 的 hook）时登记已删，流还在这里，两步之间没有空档。
//! 之后的 `watch_command`（monitor 流出错后续读）经端口接回还在跑的命令。没被接走的随客户端实例一起清掉（§19.7），
//! 同 MCP 连接表拿着传输的事件流。

use rust_cli_base::sync::LockUnpoisoned;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use rust_cli_exec_wire::envelope::ReplyResult;
use rust_cli_exec_wire::reply::Reply;
use rust_cli_exec_wire::{Body, envelope::WireError};
use rust_cli_kernel_api::env_command::EnvOverrides;
use rust_cli_kernel_api::machine::CommandEvents;

use crate::convert_in as input;
use crate::convert_out as output;
use crate::server::Inner;
use crate::target::Start;

/// 还没被接走的流：（客户端实例，operationId）→ 流。
#[derive(Default)]
pub(crate) struct Started(Mutex<HashMap<(String, String), CommandEvents>>);

impl Started {
    fn table(&self) -> MutexGuard<'_, HashMap<(String, String), CommandEvents>> {
        self.0.lock_unpoisoned()
    }

    /// 同一个客户端实例的第一个 `watch_command(op, 0)` 接走 `start_command` 留下的流；其余的经端口。
    pub(crate) fn claim(
        &self,
        instance: &str,
        operation: &str,
        cursor: u64,
    ) -> Option<CommandEvents> {
        if cursor != 0 {
            return None;
        }
        self.table()
            .remove(&(instance.to_owned(), operation.to_owned()))
    }

    /// 客户端实例被清掉（断开超过期限，§19.7）时一起丢掉。
    pub(crate) fn forget(&self, instance: &str) {
        self.table().retain(|(owner, _), _| owner != instance);
    }
}

/// 线上的 `start_command`：起命令，拿着端口给的流等 `watch_command` 接走。`env` 是这个客户端实例推来的环境变量（F96）。
pub(crate) async fn start(
    inner: Arc<Inner>,
    instance: String,
    start: Start,
    env: EnvOverrides,
) -> (ReplyResult, Body) {
    let result = match start_command(&inner, instance, start, env).await {
        Ok(()) => ReplyResult::Ok(Reply::StartCommand),
        Err(error) => ReplyResult::Err(error),
    };
    (result, Body::empty())
}

async fn start_command(
    inner: &Inner,
    instance: String,
    start: Start,
    env: EnvOverrides,
) -> Result<(), WireError> {
    let Start {
        workspace,
        context,
        request,
        body,
    } = start;
    let operation = context.operation.clone();
    let start = input::start_command(*request, &body)?;
    let events = inner
        .launcher
        .start_command(
            &input::workspace(workspace),
            input::context(context),
            start,
            &env,
        )
        .await
        .map_err(output::error)?;
    inner.started.table().insert((instance, operation), events);
    Ok(())
}

#[cfg(test)]
#[path = "started_tests.rs"]
mod tests;
