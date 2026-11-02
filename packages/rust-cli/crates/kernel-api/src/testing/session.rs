//! 会话操作的测试替身（工作线 F134，审查 K6）：不持有会话状态的 [`NoSession`]，与“全委托、只覆盖关心的方法”的
//! [`OpsOverride`]（实现它的类型就是 [`SessionOps`]）。生产实现直接实现 [`SessionOps`]，漏写一个方法是编译错误。

use async_trait::async_trait;

use crate::env::EnvPath;
use crate::ids::SessionId;
use crate::machine::CommandEvents;
use crate::message::Message;
use crate::monitor::MonitorArmed;
use crate::orchestration::OrchestrationError;
use crate::session_ops::{RunView, SessionOps};

fn unsupported(what: &str) -> OrchestrationError {
    OrchestrationError::Unsupported {
        what: what.to_owned(),
    }
}

/// 不持有会话状态：没有视图与对话，owner 是会话自己，命令一律 `Unsupported`。
pub struct NoSession;

#[async_trait]
impl SessionOps for NoSession {
    async fn view(&self) -> Option<RunView> {
        None
    }

    async fn owner_session(&self, session: &SessionId) -> SessionId {
        session.clone()
    }

    async fn conversation(&self) -> Option<Vec<Message>> {
        None
    }

    async fn set_plan(&self, _enabled: bool) -> Result<(), OrchestrationError> {
        Err(unsupported("changing plan mode"))
    }

    async fn set_workspace(&self, _root: EnvPath) -> Result<(), OrchestrationError> {
        Err(unsupported("changing the working directory"))
    }

    async fn arm_monitor(
        &self,
        _monitor: MonitorArmed,
        _events: CommandEvents,
    ) -> Result<(), OrchestrationError> {
        Err(unsupported("monitors"))
    }
}

/// 会话操作的测试代理：每个方法缺省转给 [`OpsOverride::ops`]（缺省是 [`NoSession`]），替身只覆盖关心的方法。
#[async_trait]
pub trait OpsOverride: Send + Sync {
    fn ops(&self) -> &dyn SessionOps {
        &NoSession
    }

    async fn view(&self) -> Option<RunView> {
        self.ops().view().await
    }

    async fn owner_session(&self, session: &SessionId) -> SessionId {
        self.ops().owner_session(session).await
    }

    async fn conversation(&self) -> Option<Vec<Message>> {
        self.ops().conversation().await
    }

    async fn set_plan(&self, enabled: bool) -> Result<(), OrchestrationError> {
        self.ops().set_plan(enabled).await
    }

    async fn set_workspace(&self, root: EnvPath) -> Result<(), OrchestrationError> {
        self.ops().set_workspace(root).await
    }

    async fn arm_monitor(
        &self,
        monitor: MonitorArmed,
        events: CommandEvents,
    ) -> Result<(), OrchestrationError> {
        self.ops().arm_monitor(monitor, events).await
    }
}

#[async_trait]
impl<T: OpsOverride> SessionOps for T {
    async fn view(&self) -> Option<RunView> {
        OpsOverride::view(self).await
    }

    async fn owner_session(&self, session: &SessionId) -> SessionId {
        OpsOverride::owner_session(self, session).await
    }

    async fn conversation(&self) -> Option<Vec<Message>> {
        OpsOverride::conversation(self).await
    }

    async fn set_plan(&self, enabled: bool) -> Result<(), OrchestrationError> {
        OpsOverride::set_plan(self, enabled).await
    }

    async fn set_workspace(&self, root: EnvPath) -> Result<(), OrchestrationError> {
        OpsOverride::set_workspace(self, root).await
    }

    async fn arm_monitor(
        &self,
        monitor: MonitorArmed,
        events: CommandEvents,
    ) -> Result<(), OrchestrationError> {
        OpsOverride::arm_monitor(self, monitor, events).await
    }
}
