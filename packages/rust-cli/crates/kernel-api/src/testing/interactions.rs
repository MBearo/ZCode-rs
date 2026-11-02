//! 交互的测试替身（`Interactions` 的方法都是必需的，工作线 F137）：按脚本回答的 [`ScriptedInteractions`]，没有人能回答的
//! [`NoOneToAsk`]，与“全委托、只覆盖关心的方法”的代理 [`AskOverride`]（实现它的类型就是 [`Interactions`]）。

use rust_cli_base::sync::LockUnpoisoned;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::OpsOverride;
use crate::ids::{InteractionId, ToolUseId};
use crate::orchestration::{
    Answer, InteractionAnswer, InteractionRequest, Interactions, OrchestrationError,
    PermissionAnswer,
};

/// 按顺序给出脚本里的回答；脚本用完后给 [`ScriptedInteractions::always`] 的回答，没有时返回 `Failed`（等同于交互被
/// 取消）。记下收到的请求与撤回的询问；没有会话级的入口。它也是会话操作的替身（[`OpsOverride`]）：`set_plan` 只记录、
/// 不校验，其余同 [`super::NoSession`]。工具不改权限模式（F134 删掉了没有调用方的 `set_mode`）。
#[derive(Default)]
pub struct ScriptedInteractions {
    answers: Mutex<VecDeque<Answer>>,
    /// 脚本用完之后的回答（每次都是它）。
    then: Option<Answer>,
    asked: Mutex<Vec<InteractionRequest>>,
    withdrawn: Mutex<Vec<ToolUseId>>,
    plans: Mutex<Vec<bool>>,
}

impl ScriptedInteractions {
    pub fn new(answers: impl IntoIterator<Item = Answer>) -> Self {
        Self {
            answers: Mutex::new(answers.into_iter().collect()),
            ..Self::default()
        }
    }

    /// 每次询问都给同一个回答（不按询问的种类）。
    pub fn always(answer: impl Into<Answer>) -> Self {
        Self {
            then: Some(answer.into()),
            ..Self::default()
        }
    }

    /// 权限询问一律允许（不加规则）。
    pub fn allowing() -> Self {
        Self::always(PermissionAnswer::Allow {
            remember: Vec::new(),
        })
    }

    /// 收到的交互请求，按顺序。
    pub fn asked(&self) -> Vec<InteractionRequest> {
        self.asked.lock_unpoisoned().clone()
    }

    /// 撤回的询问，按顺序。
    pub fn withdrawn(&self) -> Vec<ToolUseId> {
        self.withdrawn.lock_unpoisoned().clone()
    }

    /// 收到的 plan 开关请求，按顺序（修复线 F20）。
    pub fn plans(&self) -> Vec<bool> {
        self.plans.lock_unpoisoned().clone()
    }
}

#[async_trait]
impl Interactions for ScriptedInteractions {
    async fn ask(
        &self,
        request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        self.asked.lock_unpoisoned().push(request);
        let answer = self
            .answers
            .lock_unpoisoned()
            .pop_front()
            .or_else(|| self.then.clone());
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

    async fn cancel_ask(&self, tool_use: &ToolUseId) {
        self.withdrawn.lock_unpoisoned().push(tool_use.clone());
    }

    fn lasting(&self) -> Option<Arc<dyn Interactions>> {
        None
    }
}

#[async_trait]
impl OpsOverride for ScriptedInteractions {
    async fn set_plan(&self, enabled: bool) -> Result<(), OrchestrationError> {
        self.plans.lock_unpoisoned().push(enabled);
        Ok(())
    }
}

/// 没有人能回答（同 `-p`）：按询问的种类回答（`Answer::nobody`，权限询问是 `Unavailable`，询问本身的文字成为结果），
/// 撤回什么都不做，没有会话级的入口。不关心询问的测试用它，也是 [`AskOverride`] 的缺省委托。
pub struct NoOneToAsk;

#[async_trait]
impl Interactions for NoOneToAsk {
    async fn ask(
        &self,
        request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        Ok(InteractionAnswer {
            interaction: InteractionId::new("interaction_nobody"),
            answer: Answer::nobody(&request, PermissionAnswer::Unavailable),
        })
    }

    async fn cancel_ask(&self, _tool_use: &ToolUseId) {}

    fn lasting(&self) -> Option<Arc<dyn Interactions>> {
        None
    }
}

/// 交互的测试代理：每个方法缺省转给 [`AskOverride::asks`]（缺省是 [`NoOneToAsk`]），替身只覆盖关心的方法（例如只记下
/// 撤回）。生产实现直接实现 [`Interactions`]，漏写一个方法是编译错误。
#[async_trait]
pub trait AskOverride: Send + Sync {
    fn asks(&self) -> &dyn Interactions {
        &NoOneToAsk
    }

    async fn ask(
        &self,
        request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        self.asks().ask(request).await
    }

    async fn cancel_ask(&self, tool_use: &ToolUseId) {
        self.asks().cancel_ask(tool_use).await;
    }

    fn lasting(&self) -> Option<Arc<dyn Interactions>> {
        self.asks().lasting()
    }
}

#[async_trait]
impl<T: AskOverride> Interactions for T {
    async fn ask(
        &self,
        request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        AskOverride::ask(self, request).await
    }

    async fn cancel_ask(&self, tool_use: &ToolUseId) {
        AskOverride::cancel_ask(self, tool_use).await;
    }

    fn lasting(&self) -> Option<Arc<dyn Interactions>> {
        AskOverride::lasting(self)
    }
}
