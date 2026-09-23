use super::Engine;
use crate::domain::question::QuestionInput;
use anyhow::{Context, Result};
use serde_json::{Value, json};

impl Engine {
    pub fn with_question_timing(mut self, hidden_ms: u64, deadline_ms: u64) -> Self {
        self.question_timing = (hidden_ms, deadline_ms);
        self
    }
    pub(super) fn activate_question_head(&mut self, id: &str) {
        let Some(p) = self
            .sessions
            .get_mut(id)
            .and_then(|s| s.pending.first_mut())
        else {
            return;
        };
        if p.get("autoResolution").is_some() {
            return;
        }
        let key = p["interactionId"].as_str().unwrap();
        if self.questions.get(key).is_none_or(|q| !q.eligible) {
            return;
        }
        let now = self.clock.now();
        p["autoResolution"] = json!({"state":"hiddenGrace","startedAt":now,"visibleAt":now+self.question_timing.0,"deadlineAt":now+self.question_timing.1});
    }
    pub(super) fn snooze_question(&mut self, id: &str, interaction: &str) -> bool {
        let Some(p) = self.sessions.get_mut(id).and_then(|s| {
            s.pending
                .iter_mut()
                .find(|p| p["interactionId"] == interaction)
        }) else {
            return false;
        };
        let a = &p["autoResolution"];
        if a.is_null() || a["state"] == "snoozed" {
            return false;
        }
        p["autoResolution"] =
            json!({"state":"snoozed","startedAt":a["startedAt"],"snoozedAt":self.clock.now()});
        true
    }
    pub(super) fn question_delay(&self) -> Option<std::time::Duration> {
        self.sessions
            .iter()
            .filter(|(id, _)| {
                self.active
                    .get(*id)
                    .is_some_and(|a| !a.cancel.is_cancelled())
            })
            .filter_map(|(_, s)| s.pending.first())
            .filter_map(|p| {
                let a = &p["autoResolution"];
                match a["state"].as_str() {
                    Some("hiddenGrace") => a["visibleAt"].as_u64(),
                    Some("visibleCountdown") => a["deadlineAt"].as_u64(),
                    _ => None,
                }
            })
            .min()
            .map(|at| std::time::Duration::from_millis(at.saturating_sub(self.clock.now())))
    }
    pub(super) async fn advance_questions(&mut self) -> Result<()> {
        let now = self.clock.now();
        let due = self
            .sessions
            .iter()
            .filter(|(id, _)| {
                self.active
                    .get(*id)
                    .is_some_and(|a| !a.cancel.is_cancelled())
            })
            .filter_map(|(id, s)| {
                let p = s.pending.first()?;
                let a = &p["autoResolution"];
                let at = match a["state"].as_str() {
                    Some("hiddenGrace") => a["visibleAt"].as_u64()?,
                    Some("visibleCountdown") => a["deadlineAt"].as_u64()?,
                    _ => return None,
                };
                (now >= at).then(|| (id.clone(), p["interactionId"].as_str().unwrap().to_owned()))
            })
            .collect::<Vec<_>>();
        for (id, key) in due {
            let p = &mut self.sessions.get_mut(&id).unwrap().pending[0];
            let mut answered = None;
            let deltas = if now >= p["autoResolution"]["deadlineAt"].as_u64().unwrap() {
                let answer = QuestionInput::parse(p["payload"]["input"].clone())?
                    .answer(json!({"action":"accept","content":{"answers":{}}}))?;
                let deltas = self.settle_question(&id, &key, &answer)?;
                answered = Some(answer);
                deltas
            } else {
                p["autoResolution"]["state"] = "visibleCountdown".into();
                vec![]
            };
            let s = self.sessions.get_mut(&id).unwrap();
            s.revision += 1;
            s.updated_at = now;
            self.publish(&id, deltas)?;
            self.persist(&id, None).await?;
            if let Some(answer) = answered
                && let Some(q) = self.questions.remove(&key)
            {
                let _ = q.reply.send(answer);
            }
        }
        Ok(())
    }
    pub(super) async fn interaction_preferences(&mut self, p: &Value) -> Result<Value> {
        self.validate_workspace(p)?;
        let enabled = p["preferences"]["askUserQuestionAutoResolutionEnabled"]
            .as_bool()
            .context("Invalid interaction preferences")?;
        self.auto_resolution_preference = enabled;
        let mut count = 0;
        if !enabled {
            let mut changed = std::collections::BTreeSet::new();
            let targets = self
                .questions
                .iter_mut()
                .map(|(key, q)| {
                    q.eligible = false;
                    (q.session.clone(), key.clone())
                })
                .collect::<Vec<_>>();
            for (id, key) in targets {
                if self.snooze_question(&id, &key) {
                    count += 1;
                    changed.insert(id);
                }
            }
            for id in changed {
                let s = self.sessions.get_mut(&id).unwrap();
                s.revision += 1;
                s.updated_at = self.clock.now();
                self.publish(&id, vec![])?;
                self.persist(&id, None).await?;
            }
        }
        Ok(
            json!({"workspace":p["workspace"],"askUserQuestionAutoResolutionEnabled":enabled,"snoozedInteractionCount":count}),
        )
    }
}
