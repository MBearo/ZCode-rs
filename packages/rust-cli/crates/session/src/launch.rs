//! 开一个回合的共同部分（工作线 F130，architecture-review.md C4）：中止信号、提交通道、这次运行的 ID 与主线程的请求，
//! 起好任务之后登记成 [`Running`]。人类输入（input.rs）、bash 模式（bash.rs）、通知与定时任务（turn.rs）、`/compact`
//! （compact.rs）四种回合都经这里；以前各写一份 `Running { .. }` 与三份相同的 `QueryRequest`，F118 给四处各加了同样的
//! 计时字段。
//!
//! 轮次 ID 与 `TurnStarted` 仍在各自的入口：四种回合发它的时机不同（通知回合先提交消息再开始），ID 的生成顺序不变。

use std::sync::Arc;

use rust_cli_base::Task;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::ids::{CommandId, MessageId, RunId, TurnId};
use rust_cli_kernel_api::model::QuerySource;
use rust_cli_kernel_api::orchestration::{QueryError, QueryRequest, QueryStop};
use tokio::sync::mpsc;

use crate::actor::{Actor, Running};
use crate::sink::{ActorSink, SinkRequest};

/// 主循环提交请求的缓冲；满了时主循环等待，不丢请求。人类输入的 Bash 与 hook 的回合任务用同一个值（F55）。
pub(crate) const SINK_REQUESTS: usize = 256;

/// 起任务之前的一个回合：任务要用的输出端、中止信号与运行 ID，加上 actor 留着的提交通道。
pub(crate) struct Opening {
    turn: TurnId,
    /// 这次运行（主循环的回合）；压缩没有（它的子循环自己取）。
    run: Option<RunId>,
    pub(crate) abort: Abort,
    pub(crate) sink: Arc<ActorSink>,
    requests: mpsc::Receiver<SinkRequest>,
}

impl Actor {
    /// 回合的输出端与中止信号；`buffer` 是提交通道的缓冲。主循环的回合用 [`Self::open_main_turn`]。
    pub(crate) fn open_turn(&self, turn: TurnId, run: Option<RunId>, buffer: usize) -> Opening {
        let abort = Abort::new();
        let (sender, requests) = mpsc::channel(buffer);
        Opening {
            turn,
            run,
            abort,
            sink: Arc::new(self.sink(sender)),
            requests,
        }
    }

    /// 主循环的回合：新的运行 ID 与这一回合的请求（[`Self::main_request`]）。
    pub(crate) fn open_main_turn(&self, turn: TurnId) -> (Opening, QueryRequest) {
        let run = self.deps.ids.run();
        let request = self.main_request(run.clone());
        (self.open_turn(turn, Some(run), SINK_REQUESTS), request)
    }

    /// 主线程这一回合的请求：历史、模型与模式取 actor 此刻的值，入口再追加这一回合的输入。
    fn main_request(&self, run: RunId) -> QueryRequest {
        QueryRequest {
            max_turns: self.deps.max_turns,
            ..QueryRequest::new(
                self.meta.id.clone(),
                run,
                self.meta.workspace.clone(),
                self.messages.clone(),
                self.selection.clone(),
                self.mode(),
                QuerySource::MainThread,
            )
        }
    }

    /// 登记起好的回合任务（busy 的唯一槽位）。`prompt` 是开启本回合的 user 消息（checkpoint 的 `messageId`、首个回合
    /// 的标题），`command` 是开启它的命令（提交 `prompt` 时写进批次，Node `anchor.sourceCommandId`）。
    pub(crate) fn launch(
        &mut self,
        opening: Opening,
        task: Task<Result<QueryStop, QueryError>>,
        prompt: Option<MessageId>,
        command: Option<CommandId>,
    ) {
        let Opening {
            turn,
            run,
            abort,
            sink: _,
            requests,
        } = opening;
        self.running = Some(Running {
            turn,
            run,
            abort,
            requests,
            task,
            failure: None,
            prompt,
            command,
            // 回合结束那一行的计时（修复线 F118，duration.rs）。
            timing: crate::duration::TurnTiming::start(
                self.deps.clock.now_ms(),
                !self.pending.is_empty(),
            ),
        });
    }
}
