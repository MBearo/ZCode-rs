//! Actor-owned registry of everything waiting on an external answer.
//!
//! Permission prompts, user questions and host requests are registered here
//! under their owner (a session, or an auxiliary job id). Terminal paths
//! (run finished, session closed, runtime stopped) release an owner through
//! [`Waiters::release`] only, so no path can forget one kind of waiter.
//! Dropping a reply sender resolves the waiting task as cancelled.
use serde_json::Value;
use std::collections::BTreeMap;
use tokio::sync::oneshot;

use crate::domain::question::QuestionAnswer;

pub(super) struct WaitingQuestion {
    pub session: String,
    pub run: String,
    pub eligible: bool,
    pub reply: oneshot::Sender<QuestionAnswer>,
}

pub(super) struct HostWait {
    pub owner: String,
    pub workspace: Value,
    pub reply: oneshot::Sender<Value>,
}

/// A permission prompt: `owner` runs the tool, `host` shows the prompt (the root
/// session for subagents).
pub(super) struct PermissionWait {
    pub owner: String,
    pub host: String,
    pub tool: String,
    pub call: String,
    pub suggestions: Vec<crate::domain::permission::Update>,
    pub reply: oneshot::Sender<crate::contract::PermissionAnswer>,
}

#[derive(Default)]
pub(super) struct Waiters {
    permissions: BTreeMap<String, PermissionWait>,
    questions: BTreeMap<String, WaitingQuestion>,
    host: BTreeMap<String, HostWait>,
}

impl Waiters {
    pub fn add_permission(&mut self, interaction: String, wait: PermissionWait) {
        self.permissions.insert(interaction, wait);
    }

    pub fn permission(&self, interaction: &str) -> Option<&PermissionWait> {
        self.permissions.get(interaction)
    }

    /// The pending prompt of `owner`'s tool call, if any.
    pub fn permission_for_call(&self, owner: &str, call: &str) -> Option<String> {
        self.permissions
            .iter()
            .find(|(_, p)| p.owner == owner && p.call == call)
            .map(|(id, _)| id.clone())
    }

    pub fn permission_hosted_by(&self, interaction: &str, host: &str) -> bool {
        self.permissions
            .get(interaction)
            .is_some_and(|p| p.host == host)
    }

    pub fn take_permission(&mut self, interaction: &str) -> Option<PermissionWait> {
        self.permissions.remove(interaction)
    }

    /// Prompts of `owner` shown on another session, which must drop them on release.
    pub fn hosted_elsewhere(&self, owner: &str) -> Vec<(String, String)> {
        self.permissions
            .iter()
            .filter(|(_, p)| p.owner == owner && p.host != owner)
            .map(|(id, p)| (id.clone(), p.host.clone()))
            .collect()
    }

    pub fn add_question(&mut self, interaction: String, question: WaitingQuestion) {
        self.questions.insert(interaction, question);
    }

    pub fn question(&self, interaction: &str) -> Option<&WaitingQuestion> {
        self.questions.get(interaction)
    }

    pub fn take_question(&mut self, interaction: &str) -> Option<WaitingQuestion> {
        self.questions.remove(interaction)
    }

    /// Preference turned off: no waiting question may auto-resolve; returns `(session, interaction)`.
    pub fn disable_auto_resolution(&mut self) -> Vec<(String, String)> {
        self.questions
            .iter_mut()
            .map(|(key, q)| {
                q.eligible = false;
                (q.session.clone(), key.clone())
            })
            .collect()
    }

    pub fn add_host(&mut self, id: String, wait: HostWait) {
        self.host.insert(id, wait);
    }

    pub fn take_host(&mut self, id: &str) -> Option<HostWait> {
        self.host.remove(id)
    }

    pub fn host_ids<'a>(&'a self, owner: &'a str) -> impl Iterator<Item = &'a String> {
        self.host
            .iter()
            .filter(move |(_, w)| w.owner == owner)
            .map(|(id, _)| id)
    }

    /// Cancel only host requests of `owner` (e.g. stop while awaiting credentials).
    /// Returned requests must be announced to the host as cancelled.
    pub fn release_host(&mut self, owner: &str) -> Vec<(String, HostWait)> {
        let ids = self.host_ids(owner).cloned().collect::<Vec<_>>();
        ids.into_iter()
            .filter_map(|id| self.host.remove(&id).map(|w| (id, w)))
            .collect()
    }

    /// Release every waiter of `owner`; pending answers resolve as cancelled.
    pub fn release(&mut self, owner: &str) -> Vec<(String, HostWait)> {
        self.permissions.retain(|_, p| p.owner != owner);
        self.questions.retain(|_, q| q.session != owner);
        self.release_host(owner)
    }

    pub fn holds(&self, owner: &str) -> bool {
        self.permissions.values().any(|p| p.owner == owner)
            || self.questions.values().any(|q| q.session == owner)
            || self.host.values().any(|w| w.owner == owner)
    }

    pub fn clear(&mut self) {
        self.permissions.clear();
        self.questions.clear();
        self.host.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_resolves_every_waiter_kind_of_one_owner_only() {
        let mut waiters = Waiters::default();
        let wait = |owner: &str, reply| PermissionWait {
            owner: owner.into(),
            host: owner.into(),
            tool: "Write".into(),
            call: "c".into(),
            suggestions: vec![],
            reply,
        };
        let (permission, permission_rx) = oneshot::channel();
        waiters.add_permission("p".into(), wait("s1", permission));
        let (question, mut question_rx) = oneshot::channel();
        waiters.add_question(
            "q".into(),
            WaitingQuestion {
                session: "s1".into(),
                run: "r".into(),
                eligible: true,
                reply: question,
            },
        );
        let (host, _host_rx) = oneshot::channel();
        waiters.add_host(
            "h".into(),
            HostWait {
                owner: "s1".into(),
                workspace: Value::Null,
                reply: host,
            },
        );
        let (other, _other_rx) = oneshot::channel();
        waiters.add_permission("o".into(), wait("s2", other));

        let announced = waiters.release("s1");
        assert_eq!(
            announced
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            ["h"]
        );
        assert!(!waiters.holds("s1"));
        assert!(waiters.holds("s2"));
        // 发送端被丢弃：等待中的工具以取消收口，不会永久挂起。
        assert!(permission_rx.blocking_recv().is_err());
        assert!(question_rx.try_recv().is_err());
    }
}
