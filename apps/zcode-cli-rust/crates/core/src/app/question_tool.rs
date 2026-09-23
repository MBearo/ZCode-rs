use crate::{
    contract::{Event, EventSink, ToolOutput},
    domain::question::QuestionInput,
};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

pub(super) async fn execute(
    call: &str,
    args: Value,
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<ToolOutput> {
    let mut input = QuestionInput::parse(args)?;
    // 模型生成的 answers 不能替代用户回执；yolo 也必须进入 owner 的澄清等待。
    input.answers = None;
    input.annotations = None;
    let (reply, receipt) = tokio::sync::oneshot::channel();
    sink.send(Event::Question {
        call_id: call.into(),
        input: Box::new(input),
        reply,
    })
    .await?;
    let answer = tokio::select! {biased;
        _=cancel.cancelled()=>bail!("AskUserQuestion was cancelled before answers were returned"),
        answer=receipt=>answer.context("Question owner stopped before answer commit")?,
    };
    Ok(ToolOutput {
        content: answer.content,
        data: answer.data,
        failed: answer.failed,
        display: None,
    })
}
