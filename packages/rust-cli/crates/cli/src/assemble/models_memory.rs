//! 上次启动定下的缺省模型（docs/specs/rust-cli/performance.md §12.4，工作线 F140）：`~/.zcode/rust-cli/default-model.json`。
//!
//! 只记选择与当时的 `--model`、`--effort`，没有令牌或套餐细节。下次启动时参数相同才拿来当暂定值（状态栏先显示它）；
//! 会话照旧等目录核对之后的定下值。读写经 fs；位置由 paths 给出（`StoragePaths::default_model`）。

use std::path::Path;

use rust_cli_fs::{FileMode, ParentDir, TempName, WriteOptions};
use rust_cli_kernel_api::model::ModelSelection;
use serde_json::{Value, json};

/// 选模型的参数：`--model` 与 `--effort`（同样的参数在同样的目录里选出同样的模型）。
pub(crate) type Flags = (Option<String>, Option<&'static str>);

/// 文件的内容：选择与当时的参数。
fn record((flag, effort): &Flags, selection: &ModelSelection) -> Value {
    json!({ "modelFlag": flag, "effortFlag": effort, "selection": selection })
}

async fn read(path: &Path) -> Option<Value> {
    match rust_cli_fs::read_optional(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes?).ok(),
        Err(error) => {
            tracing::warn!(%error, "the remembered default model could not be read");
            None
        }
    }
}

/// 上次的结论：参数与这次相同、选出了模型时才有。
pub(crate) async fn recall(path: &Path, flags: &Flags) -> Option<ModelSelection> {
    let remembered = read(path).await?;
    let selection: ModelSelection = serde_json::from_value(remembered["selection"].clone()).ok()?;
    (remembered == record(flags, &selection) && !selection.provider.is_empty()).then_some(selection)
}

/// 记下这次定下的值：选出了模型时写（与文件相同时不写），选不出时删掉（下次不先显示失效的模型）。失败只记 warn。
pub(crate) async fn remember(path: &Path, flags: &Flags, selection: &ModelSelection) {
    let written = if selection.provider.is_empty() {
        rust_cli_fs::remove_optional(path).await
    } else {
        let record = record(flags, selection);
        if read(path).await.as_ref() == Some(&record) {
            return;
        }
        let options = WriteOptions {
            mode: FileMode::Default,
            temp_name: TempName::Hidden,
            parent: ParentDir::Create,
            rename_retry: &[],
        };
        rust_cli_fs::atomic_write(path, format!("{record}\n").as_bytes(), options).await
    };
    if let Err(error) = written {
        tracing::warn!(%error, "the default model could not be remembered");
    }
}

#[cfg(test)]
#[path = "models_memory_tests.rs"]
mod tests;
