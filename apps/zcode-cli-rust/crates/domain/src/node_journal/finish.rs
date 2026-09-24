//! The end of a run and of a discarded streaming attempt (Node turn
//! completion, `persistCancelledStreamSnapshot`,
//! `recoverPartialAssistantOutputFailure`).
use super::Op;
use super::records as r;
use crate::session::Session;
use serde_json::{Value, json};

/// How a run ended.
pub enum Outcome<'a> {
    Success,
    Failed { name: &'a str, data: Value },
    Cancelled { text: &'a str, reasoning: &'a str },
}

const DISCARDED_MESSAGE: &str = "Partial assistant output was discarded before a streaming retry.";

impl Session {
    /// Node `recoverPartialAssistantOutputFailure`: the failed attempt closes
    /// with `StreamRecoveryDiscarded`; the retry opens a new step.
    pub fn node_stream_discarded(&mut self, now: u64, retry: u32) {
        let Some(mut step) = self.open_step() else {
            return;
        };
        let error = json!({"name": "StreamRecoveryDiscarded",
            "data": {"message": DISCARDED_MESSAGE, "retryNumber": retry}});
        let end = (
            now,
            Some(error),
            r::tokens(None),
            Some("stream_recovery_discarded".into()),
        );
        let message = self.assistant_record(&step, Some(end));
        self.node.push(now, Op::Message(message));
        step.completed = true;
        let turn = self.node.turn.as_mut().unwrap();
        turn.rounds += 1;
        turn.step = Some(step);
    }

    /// Tools the run ended without: error parts carrying the results the
    /// history gives them (`Session::unfinished_tool_results`), as Node
    /// persists its cancelled tool results.
    pub fn node_close_tools(&mut self, now: u64, results: &[(String, Value, bool)]) {
        let Some(mut step) = self.open_step() else {
            return;
        };
        for tool in step.tools.iter_mut().filter(|t| !t.done) {
            let content = results
                .iter()
                .find(|(id, ..)| *id == tool.call)
                .map_or(Value::Null, |(_, content, _)| content.clone());
            let window = (tool.started.unwrap_or(now), now);
            let state = r::error_tool(&tool.input, &content, Some(&content), window);
            tool.done = true;
            let part = r::tool_part(
                &tool.part,
                &self.id,
                &step.assistant,
                (&tool.call, tool.index, &tool.name),
                None,
                state,
            );
            self.node.push(now, Op::Part(part));
        }
        self.node.turn.as_mut().unwrap().step = Some(step);
    }

    /// The run's end: the fork boundary of a successful turn, or the open
    /// step closed with the failure / cancellation.
    pub fn node_finished(&mut self, now: u64, outcome: Outcome, ids: &mut dyn FnMut() -> String) {
        let Some(turn) = self.node.turn.clone() else {
            return;
        };
        // Node：目标运行先结算，稳定边界的 goalBoundary 快照读到的是结算后的行。
        self.node_sync_goal(now);
        match outcome {
            Outcome::Success => {
                if let Some(boundary) = turn.boundary {
                    self.node.push(
                        now,
                        Op::StableBoundary {
                            boundary,
                            start: turn.user,
                            rounds: turn.rounds,
                            turn: turn.runtime,
                        },
                    );
                }
            }
            Outcome::Failed { name, data } => {
                let error = json!({"name": name, "data": data});
                self.complete_step(now, String::new(), Some(error));
            }
            Outcome::Cancelled { .. } if self.open_step().is_some_and(|s| !s.tools.is_empty()) => {
                // 工具阶段取消：Node 为每个调用写入取消结果后仍按工具步骤收口。
                self.complete_step(now, super::part_id(now, &ids()), None);
            }
            Outcome::Cancelled { text, reasoning } => {
                if let Some(step) = self.open_step() {
                    // Node persistCancelledStreamSnapshot：只落已到达的推理与正文。
                    if !reasoning.is_empty() {
                        let part = r::reasoning_part(
                            super::part_id(now, &ids()),
                            &self.id,
                            &step.assistant,
                            reasoning,
                            None,
                            (step.created, now),
                        );
                        self.node.push(now, Op::Part(part));
                    }
                    if !text.is_empty() {
                        let part = r::text_part(
                            super::part_id(now, &ids()),
                            &self.id,
                            &step.assistant,
                            text,
                            step.created,
                            now,
                        );
                        self.node.push(now, Op::Part(part));
                    }
                    let error = json!({"name": "AbortError",
                        "data": {"message": "This operation was aborted", "turnResult": "cancelled"}});
                    self.complete_step(now, String::new(), Some(error));
                }
            }
        }
        // 轮次到此结束：之后的模型步骤只属于下一次输入开启的轮次。
        self.node.turn = None;
    }
}
