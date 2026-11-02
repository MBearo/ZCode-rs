//! 工具调用上下文的测试 builder（F126）：测试只写关心的字段，其余用这里的缺省（结构更新
//! `ToolUseContext { 字段: 值, ..tool_use_context() }`）。生产里由执行管线装出上下文。

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::{
    MemoryStore, NoSession, ScriptedInteractions, unsupported_controller, unsupported_machine,
};
use crate::env::{EnvPath, OperationContext, Origin, WorkspaceRef};
use crate::ids::{EnvironmentId, Generation, OperationId, RunId, SessionId, ToolUseId};
use crate::permission::PermissionMode;
use crate::tool::ToolUseContext;

/// 本机工作区 `/Users/dev/work` 里会话 `sess_1`、运行 `run_1` 的调用 `toolu_1`：缺省模式，每个机器端口都返回
/// `Unsupported`，询问按 [`ScriptedInteractions`] 的空脚本（一律没有回答），不持有会话（[`NoSession`]），产物存在内存里，
/// 操作 ID 是 op_1。
pub fn tool_use_context() -> ToolUseContext {
    let session = SessionId::new("sess_1");
    let run = RunId::new("run_1");
    let environment = EnvironmentId::new("local");
    ToolUseContext {
        session: session.clone(),
        run: run.clone(),
        tool_use: ToolUseId::new("toolu_1"),
        agent: None,
        workspace: WorkspaceRef {
            environment: environment.clone(),
            root: EnvPath::new("/Users/dev/work"),
        },
        mode: PermissionMode::Default,
        cancel: CancellationToken::new(),
        machine: unsupported_machine(),
        controller: unsupported_controller(),
        artifacts: Arc::new(MemoryStore::default()),
        interactions: Arc::new(ScriptedInteractions::default()),
        session_ops: Arc::new(NoSession),
        operation: OperationContext::new(
            environment,
            Generation::new("g1"),
            OperationId::new("op_1"),
            Origin::Agent { session, run },
        ),
    }
}
