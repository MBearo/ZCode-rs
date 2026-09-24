//! Goal state transitions shared by commands and run events (spec
//! rust-m11-node-storage §5.4): Node `pauseActiveGoal`, the goal accounting
//! at a run's end (`finishTargetTurnAccounting`) and the cancelled
//! verification.
use super::{Engine, Event};
use crate::domain::goal::Verdict;
use crate::domain::node_journal as nj;

impl Engine {
    /// Node `pauseActiveGoal` (stop, preemption, `pauseGoal`): an active goal
    /// pauses with a `goal_state_change` notice; the open run closes when the
    /// cancelled turn ends.
    pub(super) fn pause_active_goal(&mut self, id: &str) -> bool {
        let now = self.clock.now();
        let ids = (
            nj::message_id(now, &self.clock.id()),
            nj::part_id(now, &self.clock.id()),
        );
        let tools = self.tool_names();
        let Some(s) = self.sessions.get_mut(id) else {
            return false;
        };
        let Some(goal) = s.goal.as_mut().filter(|g| g.target_status == "active") else {
            return false;
        };
        goal.set_status("paused", now);
        s.node_goal_state(now, ids, "paused", &tools);
        true
    }

    /// Node `recoverInterruptedSessionTargetRun`: a run left open by an ended
    /// process, recovered when the goal is next used while nothing runs.
    pub(super) fn recover_goal(&mut self, id: &str) {
        if self.active.contains_key(id) {
            return;
        }
        let now = self.clock.now();
        if let Some(goal) = self.sessions.get_mut(id).and_then(|s| s.goal.as_mut()) {
            goal.recover(now);
        }
    }

    /// Node `finishTargetTurnAccounting` at a run's end, before its records:
    /// success adds the run's tokens, a failure only its time, a
    /// cancellation pauses the goal (and settles a running verification).
    pub(super) fn goal_turn_end(&mut self, id: &str, event: &Event) {
        let Event::Finished {
            error, cancelled, ..
        } = event
        else {
            return;
        };
        let cancelled = *cancelled || self.active.get(id).is_some_and(|a| a.cancel.is_cancelled());
        let now = self.clock.now();
        let ids = (self.clock.id(), self.clock.id());
        let Some(s) = self.sessions.get_mut(id) else {
            return;
        };
        let Some(goal) = s.goal.as_mut() else {
            return;
        };
        if goal.verifying.is_some() {
            // Node：verifier 被取消时写 cancelled 生命周期并暂停目标。
            let verdict = Verdict::cancelled();
            let trace = s.trace_id.clone().unwrap_or(ids.1);
            s.node_goal_verification(now, (ids.0, trace), Some(&verdict));
            let goal = s.goal.as_mut().unwrap();
            goal.verifying = None;
            if goal.target_status == "active" {
                goal.set_status("paused", now);
            }
            goal.status = verdict.goal_status().into();
            return;
        }
        if cancelled {
            goal.finish_run(now, false, Some("paused"));
        } else {
            goal.finish_run(now, error.is_none(), None);
        }
    }
}
