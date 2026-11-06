//! `/autocompact`（docs/specs/rust-cli/tui.md §6，Puffin `shell/auto_compact.rs`）：没有词时说在哪里压缩，带 token 数时写
//! `compact.autoThresholdTokens` 并换掉进程里的值，`default` 去掉设置，别的词 ✗ 说要什么，不写。
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_runtime::AutoCompactAt;

use super::support::{GatedModel, Setup, with_ui};

/// 入口给界面的压缩阈值格子（cli `tui_inputs::CompactCell` 的同样写法）：界面只认 tui 的端口，读写进程里的值。
struct Cell(AutoCompactAt);

impl rust_cli_tui::AutoCompactCell for Cell {
    fn get(&self) -> Option<u64> {
        self.0.get()
    }

    fn set(&self, at: Option<u64>) {
        self.0.set(at);
    }
}

/// 记下写入的设置。
#[derive(Default)]
struct Saved(Mutex<Vec<Option<u64>>>);

impl rust_cli_tui::Preferences for Saved {
    fn save_default_model(&self, _: ModelSelection) -> BoxFuture<'static, Result<String, String>> {
        Box::pin(async { Ok(String::new()) })
    }

    fn save_compact_at(&self, at: Option<u64>) -> BoxFuture<'static, Result<String, String>> {
        self.0.lock().unwrap().push(at);
        Box::pin(async { Ok("/Users/dev/.zcode/config.json".to_owned()) })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn autocompact_shows_and_writes_the_setting() {
    let saved = Arc::new(Saved::default());
    let at = AutoCompactAt::new(Some(400_000));
    let mut setup = Setup::default();
    setup.services.preferences = saved.clone();
    setup.services.compact_at = Some((Arc::new(Cell(at.clone())), None));
    let model = GatedModel::scripted(Vec::new());
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/autocompact");
        ui.wait_for("Sessions compact at 400k tokens, or at the model's limit if it is lower.")
            .await;
        ui.submit("/autocompact soon");
        let screen = ui.wait_for("Write a number of tokens, like 400k.").await;
        assert!(screen.contains("✗"), "{screen}");
        ui.clear();
        ui.submit("/autocompact 300k");
        ui.wait_for("Saved. Sessions compact at 300k tokens").await;
        ui.submit("/autocompact default");
        ui.wait_for("Saved. Sessions compact at the model's limit.")
            .await;
    })
    .await;
    assert_eq!(*saved.0.lock().unwrap(), [Some(300_000), None]);
    assert_eq!(at.get(), None);
    assert!(model.requests().is_empty(), "no command reached the model");
}
