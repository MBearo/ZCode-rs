//! 交互的测试替身：按脚本回答提问、计划审批与权限询问，记录收到的请求与模式切换。

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;

use super::lock;
use crate::ids::{InteractionId, SessionId};
use crate::orchestration::{
    InteractionAnswer, InteractionRequest, Interactions, OrchestrationError,
};
use crate::permission::PermissionMode;

/// 按顺序给出脚本里的回答（`answer` 的 JSON）；脚本用完后返回 `Failed`（等同于交互被取消）。
/// `set_mode` 与 `set_plan` 只记录，不校验。
#[derive(Default)]
pub struct ScriptedInteractions {
    answers: Mutex<VecDeque<Value>>,
    asked: Mutex<Vec<InteractionRequest>>,
    modes: Mutex<Vec<PermissionMode>>,
    plans: Mutex<Vec<bool>>,
}

impl ScriptedInteractions {
    pub fn new(answers: impl IntoIterator<Item = Value>) -> Self {
        Self {
            answers: Mutex::new(answers.into_iter().collect()),
            ..Self::default()
        }
    }

    /// 收到的交互请求，按顺序。
    pub fn asked(&self) -> Vec<InteractionRequest> {
        lock(&self.asked).clone()
    }

    /// 收到的模式切换请求，按顺序。
    pub fn modes(&self) -> Vec<PermissionMode> {
        lock(&self.modes).clone()
    }

    /// 收到的 plan 开关请求，按顺序（修复线 F20）。
    pub fn plans(&self) -> Vec<bool> {
        lock(&self.plans).clone()
    }
}

#[async_trait]
impl Interactions for ScriptedInteractions {
    async fn ask(
        &self,
        _session: &SessionId,
        request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        lock(&self.asked).push(request);
        let answer = lock(&self.answers).pop_front();
        match answer {
            Some(answer) => Ok(InteractionAnswer {
                interaction: InteractionId::new("interaction_scripted"),
                answer,
            }),
            None => Err(OrchestrationError::Failed {
                message: "the interaction ended without an answer".to_owned(),
            }),
        }
    }

    async fn set_mode(
        &self,
        _session: &SessionId,
        mode: PermissionMode,
    ) -> Result<(), OrchestrationError> {
        lock(&self.modes).push(mode);
        Ok(())
    }

    async fn set_plan(
        &self,
        _session: &SessionId,
        enabled: bool,
    ) -> Result<(), OrchestrationError> {
        lock(&self.plans).push(enabled);
        Ok(())
    }
}
