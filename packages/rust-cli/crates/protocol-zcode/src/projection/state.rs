//! 会话状态的 A 区字段（`snapshot.ts`）：control、availability、inputRouting、meta、config、usage、queue、
//! pendingInteractions。每个函数给出整段的值（`state.updated` 是字段级整体替换，不深合并）。

use rust_cli_kernel_api::message::Usage;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use serde_json::{Value, json};

/// 会话的阶段（`sessionPhaseSchema`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Draft,
    Running,
    CompletedSuccess,
    CompletedInterrupted,
    Error,
}

impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Running => "running",
            Self::CompletedSuccess => "completedSuccess",
            Self::CompletedInterrupted => "completedInterrupted",
            Self::Error => "error",
        }
    }

    pub fn ended(self) -> bool {
        matches!(
            self,
            Self::CompletedSuccess | Self::CompletedInterrupted | Self::Error
        )
    }
}

/// 在跑的前台工作（`activeWorkSummarySchema`，P1 只有主回合）。
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveWork {
    pub execution: String,
    pub command: Option<String>,
    pub started_at: u64,
}

/// 回合失败的说明（`sessionErrorInfoSchema`）。
#[derive(Debug, Clone, PartialEq)]
pub struct LastError {
    pub message: String,
    pub at: u64,
}

pub fn control(phase: Phase, active: Option<&ActiveWork>, error: Option<&LastError>) -> Value {
    let running = phase == Phase::Running;
    let works: Vec<Value> = active
        .filter(|_| running)
        .map(|work| {
            let mut value = json!({
                "kind": "primaryTurn",
                "foregroundExecutionId": work.execution,
                "startedAt": work.started_at,
            });
            if let Some(command) = &work.command {
                value["sourceCommandId"] = json!(command);
            }
            value
        })
        .into_iter()
        .collect();
    json!({
        "phase": phase.name(),
        "sessionEnded": phase.ended(),
        "canStop": running,
        "stopState": if running { "stoppable" } else { "idle" },
        "stopTargetKind": if running { "assistant" } else { "unknown" },
        "activeWorks": works,
        "lastError": error.map(|error| json!({
            "code": "fault.turn.failed",
            "message": error.message,
            "recoverable": true,
            "at": error.at,
            "source": "runtime",
        })),
        "apiRetry": null,
    })
}

fn allowed() -> Value {
    json!({"allowed": true})
}

fn denied(reason: &str) -> Value {
    json!({"allowed": false, "reasonCode": reason})
}

/// 操作是否可用。P1 不支持的命令（fork、compact、立即发送排队项）标成不可用，原因是 `proto.unsupportedCommand`。
pub fn availability() -> Value {
    let unsupported = "proto.unsupportedCommand";
    json!({
        "fork": denied(unsupported),
        "compact": denied(unsupported),
        "switchModelConfig": allowed(),
        "setFollowupMode": allowed(),
        "queueEdit": allowed(),
        "sendQueuedNow": denied(unsupported),
        "pauseGoal": denied("noGoalToPause"),
        "resumeGoal": denied("noGoalToResume"),
    })
}

/// 跟进的方式（`followupMode`）：运行中输入入队还是插话。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Followup {
    Queue,
    Guide,
}

impl Followup {
    pub fn name(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Guide => "guide",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "queue" => Some(Self::Queue),
            "guide" => Some(Self::Guide),
            _ => None,
        }
    }
}

pub fn input_routing(phase: Phase, followup: Followup) -> Value {
    match (phase, followup) {
        (Phase::Running, Followup::Queue) => json!({"mode": "enqueue"}),
        (Phase::Running, Followup::Guide) => json!({"mode": "guide"}),
        _ => json!({"mode": "startNow"}),
    }
}

pub fn meta(title: &str, generated: bool) -> Value {
    json!({
        "title": title,
        "titleSource": if title.is_empty() { "default" } else if generated { "generated" } else { "custom" },
    })
}

/// App 的模型选择（`modelSelectionSchema`）。
pub fn selection(selection: &ModelSelection) -> Value {
    let mut value = json!({"providerId": selection.provider, "modelId": selection.model});
    if let Some(level) = &selection.reasoning {
        value["options"] = json!({"reasoningLevel": level});
    }
    value
}

/// App 的模式名（Node 的写法：default → build 等，README §5）。
pub fn mode_name(mode: PermissionMode) -> &'static str {
    mode.node_name()
}

/// 会话配置（`sessionConfigStateSchema`）。
pub fn config(
    current: &ModelSelection,
    mode: PermissionMode,
    plan: bool,
    followup: Followup,
    levels: &[String],
) -> Value {
    json!({
        "modelSelection": selection(current),
        "provider": current.provider,
        "model": current.model,
        "thought": current.reasoning.clone().unwrap_or_default(),
        "thoughtLevels": levels,
        "followupMode": followup.name(),
        "mode": mode_name(mode),
        "planEnabled": plan,
    })
}

pub fn usage(context: Option<(u64, u64)>, total: &Usage) -> Value {
    json!({
        "contextWindow": context.map(|(used, max)| json!({
            "usedTokens": used,
            "maxTokens": max,
            "autoCompactThresholdTokens": null,
        })),
        "cumulative": {
            "inputTokens": total.input_tokens,
            "outputTokens": total.output_tokens,
            "cacheReadTokens": total.cache_read_input_tokens,
            "cacheWriteTokens": total.cache_creation_input_tokens,
        },
    })
}

pub fn add_usage(total: &mut Usage, usage: &Usage) {
    total.input_tokens += usage.input_tokens;
    total.output_tokens += usage.output_tokens;
    total.cache_creation_input_tokens += usage.cache_creation_input_tokens;
    total.cache_read_input_tokens += usage.cache_read_input_tokens;
}

/// 权限询问的选项（与 Node 的 V4 投影同一组）：允许一次、本项目一直允许、拒绝。
pub fn permission_options() -> Value {
    json!([
        {"optionId": "allowOnce", "label": "Allow once", "kind": "allowOnce",
            "response": {"decision": "allow", "reason": "Approved once"}},
        {"optionId": "allowAlways", "label": "Always allow in this project", "kind": "allowAlways",
            "response": {"decision": "allow", "reason": "Approved for this project"}},
        {"optionId": "deny", "label": "Deny", "kind": "deny",
            "response": {"decision": "deny", "reason": "The user doesn't want to proceed with this tool use."}},
    ])
}

/// “完全访问”选项（与 Node 的 V4 投影相同）：选它时会话切到 yolo（bypassPermissions）并放行这一次。
pub fn full_access_option() -> Value {
    json!({"optionId": "fullAccess", "label": "Full access", "kind": "custom",
        "response": {"decision": "deny", "reason": "Full access requires V4 approval"}})
}
