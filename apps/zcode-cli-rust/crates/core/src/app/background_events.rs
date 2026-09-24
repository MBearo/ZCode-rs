use super::Engine;
use anyhow::{Result, bail};
impl Engine {
    /// A tool's cleanup failed: fatal only while its run or background task still owns it.
    pub(super) fn cleanup_failed(&self, id: &str, run: &str, message: String) -> Result<()> {
        let owned = self.active.get(id).is_some_and(|a| a.run_id == run)
            || self.sessions.get(id).is_some_and(|s| {
                s.background
                    .values()
                    .any(|task| task.run_id == run && task.status == "running")
            });
        if owned {
            bail!("{message}");
        }
        Ok(())
    }
    pub(super) async fn background_event(
        &mut self,
        id: &str,
        run: &str,
        task: crate::domain::background::BackgroundTask,
        committed: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<()> {
        let Some(session) = self.sessions.get_mut(id) else {
            return Ok(());
        };
        let known = session.background.get(&task.id);
        let valid = if task.status == "running" {
            known.is_none()
                && self
                    .active
                    .get(id)
                    .is_some_and(|a| a.run_id == run && !a.cancel.is_cancelled())
        } else {
            known.is_some_and(|prior| prior.run_id == run && prior.status == "running")
        };
        if !valid || task.run_id != run {
            return Ok(());
        }
        if session.background.len() >= 128
            && known.is_none()
            && let Some(old) = session
                .background
                .values()
                .filter(|t| t.status != "running")
                .min_by_key(|t| t.started_at)
                .map(|t| t.id.clone())
        {
            session.background.remove(&old);
        }
        session.background.insert(task.id.clone(), task);
        session.updated_at = self.clock.now();
        session.revision += 1;
        self.publish(id, vec![])?;
        self.persist(id, None).await?;
        if let Some(receipt) = committed {
            let _ = receipt.send(());
        }
        self.resume_background_goal(id).await?;
        self.finish_child(id).await?;
        Ok(())
    }
}
