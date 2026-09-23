use super::Engine;
use crate::{
    contract::{Event, EventSink, ToolOutput},
    domain::todo::{self, TodoItem},
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub(super) async fn execute(
    name: &str,
    call: &str,
    args: Value,
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<ToolOutput> {
    let write = todo::parse(name, args)?;
    let (reply, receipt) = oneshot::channel();
    sink.send(Event::Todos {
        call_id: call.into(),
        write,
        reply,
    })
    .await?;
    tokio::select! {biased;
        _=cancel.cancelled()=>bail!("Todo operation cancelled"),
        result=receipt=>result.context("Todo owner stopped before commit"),
    }
}
impl Engine {
    pub(super) async fn todo_tool(
        &mut self,
        id: &str,
        call: &str,
        write: Option<Vec<TodoItem>>,
        reply: oneshot::Sender<ToolOutput>,
    ) -> Result<()> {
        let s = self.sessions.get_mut(id).unwrap();
        let now = self.clock.now();
        let data = todo::result(&s.todos, write.as_deref());
        let content = todo::model_content(&data);
        if let Some(todos) = write {
            s.todos = todos;
            s.todos_updated_at = now;
            s.revision += 1;
        }
        let turn = &self.active[id].turn_id;
        let row = s
            .rows
            .iter_mut()
            .find(|r| r["turnId"] == *turn && r["toolCallId"] == call)
            .context("Todo tool row missing")?;
        // 清单和真实结果同事务；崩溃恢复不能把已提交写入当成未知结果再执行。
        row["status"] = "success".into();
        row["endedAt"] = now.into();
        row["output"] = json!({"text":content});
        let row = row.clone();
        s.updated_at = now;
        self.persist(id, None).await?;
        self.publish(id, vec![json!({"op":"row.upserted","row":row})])?;
        let _ = reply.send(ToolOutput::new(content, data));
        Ok(())
    }
    pub(super) async fn todo_reminder(
        &mut self,
        id: &str,
        reply: oneshot::Sender<Value>,
    ) -> Result<()> {
        let s = self.sessions.get_mut(id).unwrap();
        let message = json!({"role":"user","content":format!("<system-reminder>\n{}\n</system-reminder>",todo::reminder(&s.todos)),"_zcode_source":"todo_reminder"});
        s.append_message(message.clone());
        self.persist(id, None).await?;
        let _ = reply.send(message);
        Ok(())
    }
}
