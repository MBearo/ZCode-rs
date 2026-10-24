//! 事实 → Node 记录：一次 `commit` 在一个 `begin immediate` 事务里提交
//! （docs/specs/rust-cli/node-store.md §4，docs/specs/rust-cli/README.md §5）。
use std::cell::RefCell;

use rusqlite::{Connection, TransactionBehavior};
use rust_cli_kernel_api::ids::{CommandId, OwnerEpoch, SessionId};
use rust_cli_kernel_api::metering::CallStatus;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::{CommitReceipt, Fact, FactBatch, StoreError, TurnAnchor};
use serde_json::{Value, json};

use crate::ids::NodeIds;
use crate::mode::{self, Execution};
use crate::records;
use crate::repo::entries::{self, Entry};
use crate::repo::sessions::{self, SessionRow};
use crate::state::{self, COMMIT_ENTRY, PERMISSION_MODE_ENTRY};
use crate::{OrStorage, storage};

/// 一次提交的上下文：同一个事务连接、同一个提交时间。
pub struct Commit<'a> {
    pub conn: &'a Connection,
    pub session: &'a str,
    pub now: u64,
    pub uuid: &'a dyn Fn() -> String,
    /// 批次的命令 id，写进 user 消息的 `anchor.sourceCommandId`。
    pub command: Option<&'a str>,
    /// 整批事实：assistant 的 `modelId` 取同批 `ModelCalled` 的请求模型（append.rs）。
    pub batch: &'a [Fact],
    /// 批次的回合锚点（会话 actor 给出，node-store.md §4）。
    pub turn: Option<&'a TurnAnchor>,
    /// 当前轮：锚点给出的，或这一批写入的输入；没有时每次从库里推导。
    current: RefCell<Option<CurrentUser>>,
}

/// 当前轮：真实用户输入的 id 与它的 `anchor.turnId`。
pub type CurrentUser = Option<(String, Option<String>)>;

impl<'a> Commit<'a> {
    pub fn new(
        conn: &'a Connection,
        session: &'a str,
        (now, uuid): (u64, &'a dyn Fn() -> String),
        command: Option<&'a str>,
        batch: &'a [Fact],
        turn: Option<&'a TurnAnchor>,
    ) -> Self {
        // 锚点所指的输入在这一批里：写到它之前的事实仍按库推导（与没有锚点时相同），写到之后才用锚点。
        let pending = turn.is_some_and(|anchor| {
            batch.iter().any(|fact| {
                matches!(fact, Fact::MessageAppended { message } if *message.id() == anchor.prompt)
            })
        });
        let current = turn.filter(|_| !pending).map(|anchor| {
            Some((
                anchor.prompt.as_str().to_owned(),
                anchor.turn.as_ref().map(|turn| turn.as_str().to_owned()),
            ))
        });
        Self {
            conn,
            session,
            now,
            uuid,
            command,
            batch,
            turn,
            current: RefCell::new(current),
        }
    }

    /// 当前轮（assistant 的 `parentID`、合成消息与用量行的 `turnId`）。
    ///
    /// 性能修复（修复线 F6，审查 R5-3）：以前 assistant、合成消息与每个用量行各调一次 `state::current_user`，
    /// 那条查询要倒着扫本提示的全部消息、逐条解析 JSON，单提示 600 步时占落库 CPU 的六成。现在会话 actor 随批次给出
    /// 当前轮（它本来就是会话状态的 owner，Node 也把它当运行时状态），这里直接用；没有锚点的批次（fork 复制历史、
    /// 夹具）照旧每次按库推导，结果不变。
    pub fn current_user(&self) -> Result<CurrentUser, StoreError> {
        if let Some(known) = self.current.borrow().as_ref() {
            return Ok(known.clone());
        }
        state::current_user(self.conn, self.session).storage()
    }

    /// 这条输入的 `anchor.turnId`：锚点指向它时用锚点的轮次，否则为 None（调用方新生成）。
    pub fn anchored_turn(&self, prompt: &str) -> Option<String> {
        self.turn
            .filter(|anchor| anchor.prompt.as_str() == prompt)
            .and_then(|anchor| anchor.turn.as_ref())
            .map(|turn| turn.as_str().to_owned())
    }

    /// 写入了一条开启新回合的输入：有锚点的批次之后都用它（没有锚点的批次照旧按库推导）。
    pub fn opened(&self, prompt: &str, turn: &str) {
        if self.turn.is_some() {
            *self.current.borrow_mut() = Some(Some((prompt.to_owned(), Some(turn.to_owned()))));
        }
    }
}

/// Node `GENERATED_TITLE_EXPECTED_SOURCES`。
const GENERATED_TITLE_EXPECTED_SOURCES: [&str; 3] = ["default", "first_input", "generated"];

pub fn unsupported(what: &str) -> StoreError {
    StoreError::Unsupported {
        what: format!("node-store {what}"),
    }
}

/// 未删除的会话行；不存在或已软删除时为 `NotFound`（Node `SessionNotFound`）。
pub fn live_session(conn: &Connection, session: &SessionId) -> Result<SessionRow, StoreError> {
    sessions::get(conn, session.as_str())
        .storage()?
        .filter(|row| row.time_deleted.is_none())
        .ok_or_else(|| StoreError::NotFound {
            session: session.clone(),
        })
}

/// 在一个 `begin immediate` 事务里应用整批事实；任一事实失败（含不支持）则整批回滚。用量事实例外：写不进去时只回退
/// 它自己（[`crate::usage::observe`]）。
pub fn commit(
    conn: &mut Connection,
    session: &SessionId,
    batch: &FactBatch,
    ids: &NodeIds,
) -> Result<CommitReceipt, StoreError> {
    // 本地只有一个 owner（OwnerEpoch::LOCAL）；其他值来自过期的租约。
    if batch.owner_epoch != OwnerEpoch::LOCAL {
        return Err(StoreError::StaleOwner {
            session: session.clone(),
        });
    }
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .storage()?;
    live_session(&tx, session)?;
    let uuid = || ids.uuid();
    let ctx = Commit::new(
        &tx,
        session.as_str(),
        (ids.now_ms(), &uuid),
        batch.command.as_ref().map(CommandId::as_str),
        &batch.facts,
        batch.turn.as_ref(),
    );
    for fact in &batch.facts {
        apply(&ctx, fact)?;
    }
    // 写过用量行：同一事务里按 Node 的保留期删旧行（usage.md §2.5）；删不掉只记日志（工作线 F77）。
    if crate::usage::touches_usage(&batch.facts) {
        crate::usage::observe(&ctx, "usage.prune.failed", || crate::usage::prune(&ctx))?;
    }
    let sequence = next_sequence(&ctx)?;
    tx.commit().storage()?;
    Ok(CommitReceipt { sequence })
}

pub(crate) fn apply(ctx: &Commit<'_>, fact: &Fact) -> Result<(), StoreError> {
    match fact {
        Fact::MessageAppended { message } => crate::append::append(ctx, message),
        Fact::TitleChanged { title } => {
            // Node `setCustomSessionTitle`。
            let update = sessions::Update {
                id: ctx.session.to_owned(),
                title: Some(title.clone()),
                title_source: Some("custom".to_owned()),
                ..Default::default()
            };
            sessions::update(ctx.conn, &update, ctx.now as i64).storage()?;
            Ok(())
        }
        Fact::ModelSelected { selection } => {
            let entry =
                records::model_selection_entry(ctx.session, ctx.now, node_selection(selection));
            save_entry(ctx, &entry)?;
            // 团队项目另存（Node 的选择 schema 是 strict，工作线 F49）；同一事务。
            match records::model_connection_entry(ctx.session, ctx.now, selection) {
                Some(entry) => save_entry(ctx, &entry),
                None => Ok(()),
            }
        }
        Fact::ModeChanged { mode } => change_mode(ctx, *mode),
        Fact::ExecutionChanged { state } => write_execution(ctx, *state),
        Fact::RevertSet { cursor } => {
            // Node `rewindConversationToMessage` → `setRevert`；游标按库里的行算（rewind.rs）。
            let update = sessions::Update {
                id: ctx.session.to_owned(),
                revert: Some(crate::rewind::revert(ctx, cursor)?),
                ..Default::default()
            };
            sessions::update(ctx.conn, &update, ctx.now as i64).storage()?;
            Ok(())
        }
        Fact::OperationIntent {
            operation,
            fingerprint,
        } => crate::operations::record_intent(ctx, operation, fingerprint),
        Fact::Entry { kind, id, data } => {
            if kind.is_empty() || data.is_null() {
                return Err(storage("a rust-cli entry needs a kind and non-null data"));
            }
            let kind = format!("rust-cli/{kind}");
            let entry = records::entry(
                format!("{}:{kind}:{id}", ctx.session),
                ctx.session,
                &kind,
                ctx.now,
                data.clone(),
                false,
            );
            save_entry(ctx, &entry)
        }
        Fact::CheckpointCreated { checkpoint } => crate::rewind::checkpoint(ctx, checkpoint),
        Fact::CheckpointsRestored { checkpoints } => crate::rewind::restored(ctx, checkpoints),
        Fact::TitleGenerated { title, message } => {
            // Node `persistGeneratedSessionTitle`：来源仍在 GENERATED_TITLE_EXPECTED_SOURCES 里才写；
            // 不满足时整批失败（会话 actor 不发事件，titles.md §4）。
            let update = sessions::Update {
                id: ctx.session.to_owned(),
                title: Some(title.clone()),
                expected_title_sources: GENERATED_TITLE_EXPECTED_SOURCES
                    .iter()
                    .map(|source| (*source).to_owned())
                    .collect(),
                title_source: Some("generated".to_owned()),
                title_message_id: message.as_ref().map(|id| id.as_str().to_owned()),
                ..Default::default()
            };
            match sessions::update(ctx.conn, &update, ctx.now as i64).storage()? {
                0 => Err(storage(
                    "the session title was changed; the generated title is dropped",
                )),
                _ => Ok(()),
            }
        }
        Fact::PermissionRulesAdded { behavior, rules } => {
            crate::project_rules::add(ctx, *behavior, rules)
        }
        Fact::ProjectModeSet { mode } => crate::project_mode::write(ctx, *mode),
        // 用量行是观测：写不进去只回退它自己（usage.md §2.5，工作线 F77）。
        Fact::ModelCalled { call } => {
            crate::usage::observe(ctx, "usage.model.write.failed", || {
                crate::usage::model_called(ctx, call)
            })
        }
        Fact::ToolCalled { call } => crate::usage::observe(ctx, "usage.tool.write.failed", || {
            crate::usage::tool_called(ctx, call)
        }),
        Fact::TurnEnded { status } => {
            // `lastOutcome` 是业务事实，在 savepoint 之外。
            last_outcome(ctx, *status)?;
            crate::usage::observe(ctx, "usage.turn.write.failed", || {
                crate::usage::turn_ended(ctx, *status)
            })
        }
        Fact::SubagentLinked {
            tool_use,
            agent,
            child,
            agent_type,
        } => crate::tool_parts::link(ctx, tool_use, (agent.as_str(), child.as_str(), agent_type)),
    }
}

/// 内核的模型选择 → Node `ModelSelection`（`{providerId, modelId, options?: {reasoningLevel}}`）。
pub fn node_selection(selection: &ModelSelection) -> Value {
    let mut out = json!({"providerId": selection.provider, "modelId": selection.model});
    if let Some(level) = selection.reasoning.as_deref().filter(|l| !l.is_empty()) {
        out["options"] = json!({"reasoningLevel": level});
    }
    out
}

/// 旧的单值模式（子代理的子会话、测试）：按 Node `resolveExecutionState` 换成执行状态再写；`plan` 只打开 plan，
/// 不改 `rust-cli/permission_mode`（它只记权限模式）。
fn change_mode(ctx: &Commit<'_>, mode: PermissionMode) -> Result<(), StoreError> {
    let row = sessions::get(ctx.conn, ctx.session)
        .storage()?
        .ok_or_else(|| storage(format!("session {} disappeared", ctx.session)))?;
    let current = state::execution(ctx.conn, &row)
        .storage()?
        .unwrap_or_else(Execution::default);
    let next = mode::resolve(Some(mode::permission_value(mode)), &current);
    let named = (mode != PermissionMode::Plan).then_some(mode);
    write(ctx, &row, &next, named)
}

/// 执行状态（修复线 F20）：权限模式 × plan 开关整体写入。
fn write_execution(
    ctx: &Commit<'_>,
    state: rust_cli_kernel_api::permission::ExecutionState,
) -> Result<(), StoreError> {
    let row = sessions::get(ctx.conn, ctx.session)
        .storage()?
        .ok_or_else(|| storage(format!("session {} disappeared", ctx.session)))?;
    write(ctx, &row, &mode::to_node(state), Some(state.mode))
}

/// 三条记录（node-store.md §4.3）：会话行 `permission.mode`（plan 开时是 `plan`，Node 旧值的写法）、Node 恢复时
/// 优先读的 `runtime/execution_state`、记权限模式名的 `rust-cli/permission_mode`（dontAsk 在 Node 里没有对应值；
/// `named` 为空时不写）。
fn write(
    ctx: &Commit<'_>,
    row: &sessions::SessionRow,
    next: &Execution,
    named: Option<PermissionMode>,
) -> Result<(), StoreError> {
    let value = if next.plan {
        "plan"
    } else {
        next.mode.as_str()
    };
    // Node `setMode`：`{...session.permission, mode}`，保留其余键。修复线 F2：以前整列换成 `{mode}`，续写 Node
    // 群协作会话时丢掉 `scope` 与会话规则。
    let mut permission = row
        .permission
        .as_ref()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    permission.insert("mode".to_owned(), value.into());
    let update = sessions::Update {
        id: ctx.session.to_owned(),
        permission: Some(Value::Object(permission)),
        ..Default::default()
    };
    sessions::update(ctx.conn, &update, ctx.now as i64).storage()?;
    let execution = records::execution_state_entry(ctx.session, ctx.now, &next.mode, next.plan);
    save_entry(ctx, &execution)?;
    let Some(named) = named else {
        return Ok(());
    };
    let named = records::entry(
        format!("{}:{PERMISSION_MODE_ENTRY}", ctx.session),
        ctx.session,
        PERMISSION_MODE_ENTRY,
        ctx.now,
        json!({"mode": mode::entry_name(named)}),
        false,
    );
    save_entry(ctx, &named)
}

/// Node `persistDurableSessionEvent` 在 TurnComplete / TurnError 时写会话的 `lastOutcome`（App 侧边栏的
/// phase 读它）。修复线 F2：以前不写，App 冷启动把本工程的会话都当成没有结果。
fn last_outcome(ctx: &Commit<'_>, status: CallStatus) -> Result<(), StoreError> {
    let outcome = match status {
        CallStatus::Completed => "completed",
        CallStatus::Cancelled => "interrupted",
        CallStatus::Error => "error",
    };
    let update = sessions::Update {
        id: ctx.session.to_owned(),
        last_outcome: Some(outcome.to_owned()),
        time_updated: Some(ctx.now as i64),
        ..Default::default()
    };
    sessions::update(ctx.conn, &update, ctx.now as i64).storage()?;
    Ok(())
}

/// 回执序号：每个会话一条 `rust-cli/commit` entry，每次提交加一（与事实在同一事务里）。
pub(crate) fn next_sequence(ctx: &Commit<'_>) -> Result<u64, StoreError> {
    let previous = state::last_entry(ctx.conn, ctx.session, COMMIT_ENTRY)
        .storage()?
        .and_then(|data| data["sequence"].as_u64())
        .unwrap_or(0);
    let sequence = previous + 1;
    let entry = records::entry(
        format!("{}:{COMMIT_ENTRY}", ctx.session),
        ctx.session,
        COMMIT_ENTRY,
        ctx.now,
        json!({"sequence": sequence}),
        false,
    );
    save_entry(ctx, &entry)?;
    Ok(sequence)
}

/// 把 `records::entry` 形状的值交给 Node `saveSessionEntry`。
pub fn save_entry(ctx: &Commit<'_>, value: &Value) -> Result<(), StoreError> {
    let text = |key: &str| value[key].as_str().unwrap_or_default().to_owned();
    let time = |key: &str| value["time"][key].as_i64().unwrap_or(ctx.now as i64);
    let entry = Entry {
        id: text("id"),
        session_id: text("sessionID"),
        kind: text("type"),
        time_created: time("created"),
        time_updated: time("updated"),
        data: value["data"].clone(),
        touch_session: value["touchSession"] != false,
    };
    entries::save(ctx.conn, &entry).storage()
}

#[cfg(test)]
#[path = "turn_anchor_tests.rs"]
mod turn_anchor_tests;
