// 第一帧不等模型目录（工作线 F140，docs/specs/rust-cli/performance.md §12.3）：入口给出还没定下的缺省模型；第一帧照样
// 画，门道不开；这期间发的消息留着、说等 agent；定下后草稿接上它、留下的消息发出；定为空时打开门道。
use rust_cli_kernel_api::default_model::{DefaultModel, unselected};
use rust_cli_kernel_api::testing::{selection, text_response};

use super::support::{GatedModel, Setup, with_ui};

#[tokio::test(flavor = "multi_thread")]
async fn a_message_sent_before_the_default_model_settles_goes_out_after() {
    let model = GatedModel::scripted(vec![text_response(&["answered"])]);
    let (settle, default) = DefaultModel::pending();
    let setup = Setup {
        selection: Some(default),
        ..Setup::default()
    };
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("Send a message").await;
        ui.submit("hello there");
        let screen = ui
            .wait_for("Queued. It goes out as soon as the agent is ready.")
            .await;
        assert!(!screen.contains("Connect a model to start."), "{screen}");
        assert!(model.requests().is_empty());
        settle.settle(selection());
        ui.wait_for("answered").await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_default_model_opens_the_doorway_when_it_settles() {
    let model = GatedModel::scripted(Vec::new());
    let (settle, default) = DefaultModel::pending();
    let setup = Setup {
        selection: Some(default),
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        let screen = ui.wait_for("Send a message").await;
        assert!(!screen.contains("Connect a model to start."), "{screen}");
        settle.settle(unselected());
        ui.wait_for("Connect a model to start.").await;
    })
    .await;
}

fn remembered() -> rust_cli_kernel_api::model::ModelSelection {
    rust_cli_kernel_api::model::ModelSelection {
        model: "remembered-model".into(),
        ..selection()
    }
}

/// §12.4：状态栏先显示上次的结论；消息照样等定下，定下后草稿换成定下的值再发。
#[tokio::test(flavor = "multi_thread")]
async fn the_status_bar_shows_the_last_choice_until_the_default_settles() {
    let model = GatedModel::scripted(vec![text_response(&["answered"])]);
    let (settle, default) = DefaultModel::guessed(remembered());
    let setup = Setup {
        selection: Some(default),
        ..Setup::default()
    };
    let seen = model.clone();
    with_ui(model.clone(), setup, |mut ui| async move {
        let screen = ui.wait_for("remembered-model").await;
        assert!(!screen.contains("Connect a model to start."), "{screen}");
        ui.submit("hello there");
        ui.wait_for("Queued. It goes out as soon as the agent is ready.")
            .await;
        assert!(model.requests().is_empty());
        settle.settle(selection());
        let screen = ui.wait_for("answered").await;
        assert!(!screen.contains("remembered-model"), "{screen}");
    })
    .await;
    assert_eq!(seen.requests()[0].selection, selection());
}

/// §12.4：上次的模型这次选不出（套餐过期等）：草稿清空、打开门道。
#[tokio::test(flavor = "multi_thread")]
async fn a_last_choice_that_no_longer_holds_opens_the_doorway() {
    let model = GatedModel::scripted(Vec::new());
    let (settle, default) = DefaultModel::guessed(remembered());
    let setup = Setup {
        selection: Some(default),
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.wait_for("remembered-model").await;
        settle.settle(unselected());
        let screen = ui.wait_for("Connect a model to start.").await;
        assert!(!screen.contains("remembered-model"), "{screen}");
    })
    .await;
}
