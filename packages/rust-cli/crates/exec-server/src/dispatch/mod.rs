//! 端口的请求交给注入的 `MachineEnv`（remote-exec.md §4：远端的执行逻辑就是 env-local 的同一份代码）。每个端口一个
//! 文件；哪个请求去哪个函数只在 `target.rs` 分一次。

pub(crate) mod background;
pub(crate) mod checkpoints;
pub(crate) mod commands;
pub(crate) mod files;
pub(crate) mod git;
pub(crate) mod info;
pub(crate) mod pdf;
pub(crate) mod search;
pub(crate) mod shell;

use std::future::Future;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::envelope::{ReplyResult, WireError};
use rust_cli_exec_wire::reply::Reply;

/// 端口的一次调用：参数已经绑好，结果是回复与它的二进制段。
pub(crate) type PortCall = BoxFuture<'static, (ReplyResult, Body)>;

fn result(outcome: Result<Reply, WireError>) -> ReplyResult {
    match outcome {
        Ok(reply) => ReplyResult::Ok(reply),
        Err(error) => ReplyResult::Err(error),
    }
}

/// 回复没有二进制段的调用。
pub(crate) fn call(
    work: impl Future<Output = Result<Reply, WireError>> + Send + 'static,
) -> PortCall {
    async move { (result(work.await), Body::empty()) }.boxed()
}

/// 回复带二进制段的调用（读文件、读文本、渲染 PDF）；失败时二进制段是空的。
pub(crate) fn call_with_body(
    work: impl Future<Output = Result<(Reply, Body), WireError>> + Send + 'static,
) -> PortCall {
    async move {
        match work.await {
            Ok((reply, body)) => (ReplyResult::Ok(reply), body),
            Err(error) => (ReplyResult::Err(error), Body::empty()),
        }
    }
    .boxed()
}
