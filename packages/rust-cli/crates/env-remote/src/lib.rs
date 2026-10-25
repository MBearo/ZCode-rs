//! 执行端口的远端实现（docs/specs/rust-cli/remote-exec.md §4、§15.3、§18）：`MachineEnv` 的 8 个端口的每个方法是一条
//! 请求，经 `exec-client` 发给执行器；执行器那一头是同一份 env-local。工具实现不知道自己在哪台机器上执行。
//!
//! kernel-api 的类型与线上类型的换算（客户端侧）只在这里：[`to_wire`] 与 [`from_wire`]。MCP 的管道（R5，§22）在
//! [`mcp`]：`McpTransport` 的每个方法是一条 `mcp.*` 消息，另有期望集合 [`RemoteEnvironment::declare_mcp`]。

mod failure;
mod from_wire;
mod mcp;
mod ports;
mod to_wire;

use std::sync::Arc;

use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_exec_client::{ClientStream, ExecClient};
use rust_cli_exec_wire::envelope::{ReplyResult, StreamItem};
use rust_cli_exec_wire::reply::Reply;
use rust_cli_exec_wire::request::Request;
use rust_cli_exec_wire::{Body, BodyBuilder};
use rust_cli_kernel_api::env::EnvError;
use rust_cli_kernel_api::machine::MachineEnv;

/// 一台远端机器：一条到执行器的连接实现全部 8 个端口。
pub struct RemoteEnvironment {
    client: ExecClient,
    /// 机器名：只用在给模型的失败说明里（F96，remote-exec.md §23.6）。
    name: String,
}

impl RemoteEnvironment {
    pub fn new(client: ExecClient) -> Self {
        Self {
            client,
            name: rust_cli_kernel_api::env_registry::LOCAL_MACHINE.to_owned(),
        }
    }

    /// 带上机器名（失败说明里写它，F96）。
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn client(&self) -> &ExecClient {
        &self.client
    }

    /// 装成一台机器：8 个端口与 MCP 的管道都经这条连接（MCP 是 R5，§22.6）。
    pub fn machine(self: Arc<Self>) -> MachineEnv {
        MachineEnv::new(self.clone(), self)
    }

    /// 按这个客户端实例推一组环境变量，只影响之后启动的进程（远端执行 F96，remote-exec.md §23.5；留给以后的推送方，
    /// 例如云端的秘密注入）。执行器没报告 `env` 时是 `Unsupported`；不合规则时是执行器的说明（`io`）。
    pub async fn update_env(
        &self,
        mode: rust_cli_exec_wire::dto::env::EnvMode,
        vars: rust_cli_exec_wire::dto::env::EnvVars,
        injected: Vec<String>,
    ) -> Result<(), EnvError> {
        let request = Request::EnvUpdate {
            mode,
            vars,
            injected,
        };
        self.call(request, BodyBuilder::new()).await.map(drop)
    }

    /// 发一条请求，取回对应的回复与二进制段；回复的种类对不上时是协议错误。
    async fn call(&self, request: Request, body: BodyBuilder) -> Result<(Reply, Body), EnvError> {
        let op = request.op();
        let answer = self
            .client
            .call(request, body.finish())
            .await
            .map_err(|failure| failure::client(&self.name, op, failure))?;
        match answer.result {
            ReplyResult::Ok(reply) if reply.op() == op => Ok((reply, answer.body)),
            ReplyResult::Ok(reply) => Err(mismatch(op.as_str(), reply.op().as_str())),
            ReplyResult::Err(wire) => Err(failure::wire(&self.name, op, wire)),
        }
    }

    /// 开一个流；每项交给 `convert`，流里的错误换成 `Err`，连接层的错误交一次之后结束。
    fn stream<T: Send + 'static>(
        &self,
        request: Request,
        convert: fn(StreamItem, &Body) -> Result<T, EnvError>,
    ) -> BoxStream<'static, Result<T, EnvError>> {
        let client = self.client.clone();
        let op = request.op();
        let name: std::sync::Arc<str> = self.name.as_str().into();
        stream::once(async move { client.stream(request, Body::empty()).await })
            .flat_map(move |opened: Result<ClientStream, _>| match opened {
                Ok(items) => {
                    let name = name.clone();
                    items
                        .map(move |item| match item {
                            Ok(event) => match event.item {
                                StreamItem::Error { error: wire } => {
                                    Err(failure::wire(&name, op, wire))
                                }
                                item => convert(item, &event.body),
                            },
                            Err(failure) => Err(failure::client(&name, op, failure)),
                        })
                        .boxed()
                }
                Err(failure) => stream::iter([Err(failure::client(&name, op, failure))]).boxed(),
            })
            .boxed()
    }
}

fn mismatch(asked: &str, answered: &str) -> EnvError {
    EnvError::Io {
        message: format!("executor protocol error: {asked} was answered with {answered}"),
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
