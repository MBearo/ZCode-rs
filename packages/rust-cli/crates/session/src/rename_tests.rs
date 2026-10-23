//! 不带参数的 `/rename`（titles.md §8.2）：真实的会话 actor 与主循环，脚本模型给起名的回复。
use std::sync::Arc;

use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::{
    ModelError, ModelEvent, ModelPort, ModelRequest, OutputFormat, QuerySource,
};
use rust_cli_kernel_api::session_api::{Ack, Command, KernelEvent, SessionHandle};
use rust_cli_kernel_api::store::Fact;
use rust_cli_kernel_api::testing::{ScriptedModel, selection, text_response};
use serde_json::json;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::test_support::{Built, Setup, envelope, finished, submit, until};

/// 先问一轮，再把起名模型接上。
async fn after_one_turn(side: Arc<dyn ModelPort>) -> Built {
    let main = Arc::new(ScriptedModel::new(vec![text_response(&["I will look."])]));
    let mut setup = Setup::new(main);
    setup.side_model = Some(side);
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    built
        .session
        .send(submit("c1", "Fix the flaky login test"))
        .await;
    until(&mut events, finished).await;
    built
}

fn renamed(facts: &[Fact]) -> Vec<String> {
    facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::TitleChanged { title } => Some(title.clone()),
            _ => None,
        })
        .collect()
}

fn facts(built: &Built) -> Vec<Fact> {
    built.store.facts(&SessionId::new("sess_1"))
}

#[tokio::test]
async fn the_model_names_the_session() {
    let side = Arc::new(ScriptedModel::new(vec![text_response(&[
        "{\"name\":\"flaky-login-test\"}",
    ])]));
    let built = after_one_turn(side.clone()).await;
    let mut events = built.session.subscribe(None);
    let ack = built
        .session
        .send(envelope("n1", Command::GenerateName))
        .await;
    assert_eq!(
        ack,
        Ack::Accepted {
            result: json!({"name": "flaky-login-test", "message": "Session renamed to: flaky-login-test"})
        }
    );
    let requests = side.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    // 小模型槽位等于当前主模型（model.md §6.1）。
    assert_eq!(request.selection, selection());
    let system: Vec<&str> = request.system.iter().map(|b| b.text.as_str()).collect();
    assert_eq!(
        system,
        [
            rust_cli_prompts::texts::identity(),
            rust_cli_prompts::title::rename_system()
        ]
    );
    assert_eq!(
        request.messages[0].content[0].block,
        ContentBlock::Text {
            text: "<conversation>\nFix the flaky login test\nI will look.\n</conversation>"
                .to_owned()
        }
    );
    assert!(request.tools.is_empty());
    assert!(matches!(
        request.options.output_format,
        Some(OutputFormat::JsonSchema { .. })
    ));
    assert_eq!(
        request.source,
        QuerySource::Side("rename_generate_name".to_owned())
    );
    // 写入按改名（来源 custom）：先提交，再改内存、发事件。
    assert_eq!(renamed(&facts(&built)), ["flaky-login-test"]);
    until(
        &mut events,
        |event| matches!(event, KernelEvent::TitleChanged { title } if title == "flaky-login-test"),
    )
    .await;
    assert_eq!(
        built.session.view().await.unwrap().meta.title,
        "flaky-login-test"
    );
}

const NO_NAME: &str =
    "Could not generate a name: no conversation context yet. Usage: /rename <name>";

#[tokio::test]
async fn without_a_conversation_no_request_is_sent() {
    let side = Arc::new(ScriptedModel::new(Vec::new()));
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(Vec::new())));
    setup.side_model = Some(side.clone());
    let built = setup.build().await;
    let ack = built
        .session
        .send(envelope("n1", Command::GenerateName))
        .await;
    assert_eq!(
        ack,
        Ack::Rejected {
            reason: NO_NAME.to_owned()
        }
    );
    assert!(side.requests().is_empty());
}

#[tokio::test]
async fn an_unusable_reply_keeps_the_title() {
    for reply in ["no json here", "{\"name\": \"\\u200b \"}"] {
        let side = Arc::new(ScriptedModel::new(vec![text_response(&[reply])]));
        let built = after_one_turn(side).await;
        let ack = built
            .session
            .send(envelope("n1", Command::GenerateName))
            .await;
        assert_eq!(
            ack,
            Ack::Rejected {
                reason: NO_NAME.to_owned()
            },
            "{reply}"
        );
        assert!(renamed(&facts(&built)).is_empty(), "{reply}");
    }
}

/// 回复要等 `open`：用来在起名期间插入别的命令。
struct Gated {
    open: Arc<Notify>,
}

impl ModelPort for Gated {
    fn stream(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        let open = self.open.clone();
        stream::once(async move {
            open.notified().await;
            stream::iter(text_response(&["{\"name\":\"generated-name\"}"]))
        })
        .flatten()
        .boxed()
    }
}

#[tokio::test]
async fn a_newer_rename_wins_over_the_generated_name() {
    let open = Arc::new(Notify::new());
    let built = after_one_turn(Arc::new(Gated { open: open.clone() })).await;
    let naming = built.session.send(envelope("n1", Command::GenerateName));
    let others = async {
        // 起名期间：第二个起名被拒，改名照常提交。
        let busy = built
            .session
            .send(envelope("n2", Command::GenerateName))
            .await;
        let rename = Command::Rename {
            title: "mine".to_owned(),
        };
        let renamed = built.session.send(envelope("r1", rename)).await;
        open.notify_one();
        (busy, renamed)
    };
    let (named, (busy, renamed_ack)) = tokio::join!(naming, others);
    assert_eq!(
        busy,
        Ack::Rejected {
            reason: "A session name is being generated.".to_owned()
        }
    );
    assert!(matches!(renamed_ack, Ack::Accepted { .. }));
    // `superseded`：保留先落地的改名，不写生成的名字。
    assert_eq!(
        named,
        Ack::Accepted {
            result: json!({"name": "mine", "message": "Session is named: mine (a newer rename landed first)"})
        }
    );
    assert_eq!(renamed(&facts(&built)), ["mine"]);
}
