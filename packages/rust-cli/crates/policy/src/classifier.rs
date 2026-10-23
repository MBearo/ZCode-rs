//! 自动模式的分类器端口（判决接口；shell-permissions.md §4.7）。
//!
//! 分类器判断一个需要询问的动作能否在自动模式（以及开启自动语义的 plan 模式）下直接执行。
//! 分类器是一次旁路模型请求（安全监视器 system prompt 加对话摘录，`<severity>` 两阶段打分）；
//! 本工程的实现经 ModelPort 发同样的请求，在 runtime 组装时注入。还没有实现时用
//! [`Unavailable`]：判决链按“分类器不可用”的分支拦下动作。

use async_trait::async_trait;
use rust_cli_kernel_api::ids::SessionId;
use serde_json::Value;

/// 交给分类器的动作。
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifierRequest {
    pub session: SessionId,
    pub tool: String,
    pub input: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassifierVerdict {
    Allow,
    Block {
        reason: String,
    },
    /// 请求失败、回复无法解析或没有配置分类器：失败即拦截。
    Unavailable,
}

#[async_trait]
pub trait AutoClassifier: Send + Sync {
    async fn classify(&self, request: ClassifierRequest) -> ClassifierVerdict;
}

/// 没有分类器。
pub struct Unavailable;

#[async_trait]
impl AutoClassifier for Unavailable {
    async fn classify(&self, _request: ClassifierRequest) -> ClassifierVerdict {
        ClassifierVerdict::Unavailable
    }
}
