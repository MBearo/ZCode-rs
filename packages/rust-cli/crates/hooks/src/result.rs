//! 单个 hook 的结果（HookResult）与执行器的原始结果。

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Outcome {
    #[default]
    Success,
    Blocking,
    NonBlockingError,
    Cancelled,
}

/// hook 给出的权限行为（`permissionDecision` / `decision`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Behavior {
    Allow,
    Deny,
    Ask,
    /// 只在 print 模式生效；这里按没有决定处理（D11）。
    Defer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Blocking {
    pub error: String,
    pub command: String,
}

/// PermissionRequest 的 `decision`。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PermissionRequestDecision {
    Allow { updated_input: Option<Value> },
    Deny { message: Option<String> },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct HookResult {
    pub outcome: Outcome,
    pub blocking: Option<Blocking>,
    pub prevent: bool,
    pub stop_reason: Option<String>,
    pub behavior: Option<Behavior>,
    /// `hookPermissionDecisionReason`。
    pub reason: Option<String>,
    pub additional_context: Option<String>,
    pub updated_input: Option<Value>,
    pub updated_tool_output: Option<Value>,
    pub system_message: Option<String>,
    /// `hook_success` 的 content：纯文本输出去空白后的 stdout；JSON 输出为空。
    pub success_content: Option<String>,
    pub initial_user_message: Option<String>,
    pub session_title: Option<String>,
    pub suppress_original_prompt: bool,
    pub watch_paths: Vec<String>,
    pub permission_request: Option<PermissionRequestDecision>,
    pub retry: bool,
    /// 非阻塞错误的说明（只进日志）。
    pub error: Option<String>,
}

impl HookResult {
    pub(crate) fn non_blocking(error: impl Into<String>) -> Self {
        Self {
            outcome: Outcome::NonBlockingError,
            error: Some(error.into()),
            ..Self::default()
        }
    }

    pub(crate) fn cancelled() -> Self {
        Self {
            outcome: Outcome::Cancelled,
            ..Self::default()
        }
    }

    pub(crate) fn blocking(error: String, command: String) -> Self {
        Self {
            outcome: Outcome::Blocking,
            blocking: Some(Blocking { error, command }),
            ..Self::default()
        }
    }
}

/// command / http 执行器的原始结果；REPL 与“外部”两条路径各自解释。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Raw {
    Command {
        stdout: String,
        stderr: String,
        exit: i32,
    },
    Http {
        status: u16,
        body: String,
    },
    /// 取消或超时。
    Cancelled,
    /// `async`：启动后不等待。
    Backgrounded,
    /// 没能运行；文本是错误说明。
    Failed(String),
}

/// 一个 hook 执行完的产物。prompt hook 直接给出结果。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Executed {
    Raw(Raw),
    Result(Box<HookResult>),
}

/// “外部”路径的单个结果（HookOutsideReplResult）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Summary {
    pub command: String,
    pub succeeded: bool,
    pub output: String,
    pub blocked: bool,
    pub cancelled: bool,
}
