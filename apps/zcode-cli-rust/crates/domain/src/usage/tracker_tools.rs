//! Tool usage facts of a run (Node `recordToolUsageFromEvent` and
//! `recordToolUsageFromResult`, merged by the same upsert). Spec
//! rust-m9-usage-logs §2.3.
use super::super::{ErrorInfo, Fact, ToolFact, ToolMeta};
use super::{Outcome, RunUsage};
use serde_json::Value;

pub(super) struct ToolProbe {
    name: String,
    meta: Option<ToolMeta>,
    started_at: u64,
    executing_at: Option<u64>,
    first_output_at: Option<u64>,
    requested: bool,
}

/// How one tool call ended.
#[derive(Clone, Copy, Debug, Default)]
pub struct ToolEnd<'a> {
    pub failed: bool,
    /// Denied by the permission policy or the user; the tool never ran.
    pub denied: bool,
    /// The run was cancelled when it ended.
    pub cancelled: bool,
    pub result: &'a str,
    /// Bash's exit code (Node `perf.detail.command.exitCode`).
    pub exit_code: Option<i64>,
    /// The result was cut to its budget.
    pub truncated: bool,
}

fn text(value: &Value) -> Option<String> {
    value.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

impl RunUsage {
    fn tool_fact(&self, id: &str, probe: &ToolProbe, status: &'static str) -> ToolFact {
        ToolFact {
            session_id: self.who.session_id.clone(),
            turn_id: Some(self.who.turn_id.clone()),
            trace_id: Some(self.who.trace_id.clone()),
            tool_call_id: id.into(),
            tool_name: probe.name.clone(),
            meta: probe.meta,
            approval_status: if probe.requested { "requested" } else { "none" },
            status,
            started_at: probe.started_at,
            ..ToolFact::default()
        }
    }

    /// Node `ToolCallScheduled`: running with the registry metadata.
    pub fn on_tool_start(&mut self, call: &Value, meta: Option<ToolMeta>, now: u64) -> Vec<Fact> {
        let Some(id) = call["id"].as_str() else {
            return vec![];
        };
        let probe = ToolProbe {
            name: text(&call["function"]["name"]).unwrap_or_else(|| "unknown".into()),
            meta,
            started_at: now,
            executing_at: None,
            first_output_at: None,
            requested: false,
        };
        let fact = self.tool_fact(id, &probe, "running");
        self.tools.insert(id.into(), probe);
        self.tool_calls += 1;
        vec![Fact::Tool(Box::new(fact))]
    }

    pub fn on_permission(&mut self, call_id: &str) {
        if let Some(probe) = self.tools.get_mut(call_id) {
            probe.requested = true;
        }
    }

    pub fn on_tool_executing(&mut self, call_id: &str, now: u64) {
        if let Some(probe) = self.tools.get_mut(call_id) {
            probe.executing_at.get_or_insert(now);
        }
    }

    /// Node `ToolCallProgress` with output: the first output of a running call.
    pub fn on_tool_output(&mut self, call_id: &str, now: u64) {
        if let Some(probe) = self.tools.get_mut(call_id) {
            probe.first_output_at.get_or_insert(now);
        }
    }

    pub fn on_tool_done(&mut self, call_id: &str, end: ToolEnd, now: u64) -> Vec<Fact> {
        let Some(probe) = self.tools.remove(call_id) else {
            return vec![];
        };
        let broken = end.failed || end.denied;
        let status = match (broken, end.cancelled && !end.denied) {
            (false, _) => "completed",
            (true, true) => "cancelled",
            (true, false) => "error",
        };
        let mut fact = self.tool_fact(call_id, &probe, status);
        // Node 的结果记录器最后写入 approvalStatus "none"，upsert 的 coalesce 让它覆盖之前的
        // requested / allowed / denied；统计与 Node 一致。
        fact.approval_status = "none";
        let executed = probe.executing_at.unwrap_or(probe.started_at);
        fact.completed_at = Some(now);
        fact.duration_ms = Some(now.saturating_sub(executed));
        // Node 结果记录器以完成时刻为首次输出（Bash 的输出增量更早），时长从执行开始算。
        fact.first_output_at = Some(probe.first_output_at.unwrap_or(now));
        fact.time_to_first_output_ms = Some(now.saturating_sub(executed));
        // Node 结果事件对缺失的退出码写 0；失败事件不写，只剩结果记录器的命令退出码。
        fact.exit_code = if broken {
            end.exit_code
        } else {
            Some(end.exit_code.unwrap_or(0))
        };
        // 失败与拒绝的结果没有序列化输出：Node 记 0 字节。
        fact.output_bytes = if broken { 0 } else { end.result.len() as u64 };
        fact.truncated = end.truncated;
        if broken {
            fact.cancelled_by_user = status == "cancelled";
            let kind = match status {
                "cancelled" => "tool_cancelled",
                _ if end.denied => "permission_denied",
                _ => "tool_execution_failed",
            };
            fact.error = ErrorInfo {
                kind: Some(kind.into()),
                // CoreError 的 code 是大写类型；权限拒绝不是 CoreError，没有 code。
                code: (!end.denied).then(|| kind.to_uppercase()),
                message: Some(end.result.to_owned()),
            };
        }
        vec![Fact::Tool(Box::new(fact))]
    }

    /// Calls still open when the run ends, closed with its outcome.
    pub(super) fn close_tools(&mut self, outcome: Outcome, now: u64) -> Vec<Fact> {
        let status = if outcome == Outcome::Cancelled {
            "cancelled"
        } else {
            "error"
        };
        let tools = std::mem::take(&mut self.tools);
        // Node 只把本轮合成的 ToolCallError（被中断的调用）计入 tool_error_count。
        self.tool_errors += tools.len() as u64;
        tools
            .into_iter()
            .map(|(id, probe)| {
                let mut fact = self.tool_fact(&id, &probe, status);
                fact.completed_at = Some(now);
                fact.cancelled_by_user = status == "cancelled";
                Fact::Tool(Box::new(fact))
            })
            .collect()
    }
}
