//! 新会话的缺省模型（docs/specs/rust-cli/performance.md §12.3，工作线 F140）：可以晚到的值，只定一次。
//!
//! 以前入口等模型目录选出缺省模型（每次启动都联网查套餐，约 400 ms）才建内核与 TUI，第一帧跟着等。现在 TUI 模式在后台
//! 选，内核与 TUI 拿到的是这个值：定下之前 [`DefaultModel::current`] 是暂定值（上次启动的结论，§12.4）或空选择，
//! [`DefaultModel::settled`] 等它定下。
//! 内核在建会话、开会话之前等它（唯一的等待点），所以每个回合都在定下之后。

use std::fmt;

use tokio::sync::watch;

use crate::model::ModelSelection;

/// 空选择：还没定下，或目录里选不出模型。
#[must_use]
pub fn unselected() -> ModelSelection {
    ModelSelection::new(String::new(), String::new())
}

/// 缺省模型。克隆共用同一个值。已知的选择用 `From<ModelSelection>` 当场定下。
#[derive(Clone)]
pub struct DefaultModel(watch::Receiver<State>);

/// 定下缺省模型的一方：只能定一次；没定就释放时，等的一方按空选择返回。
pub struct SettleDefault(watch::Sender<State>);

/// 现在的选择（定下之前是暂定值或空选择）与定下了没有。
#[derive(Clone)]
struct State {
    selection: ModelSelection,
    settled: bool,
}

impl DefaultModel {
    /// 还没定下的缺省模型与定它的一方；定下之前 [`Self::current`] 是空选择。
    #[must_use]
    pub fn pending() -> (SettleDefault, Self) {
        Self::guessed(unselected())
    }

    /// 同 [`Self::pending`]，但定下之前 [`Self::current`] 给暂定值 `guess`（上次启动的结论，performance.md §12.4）。
    /// 暂定值只供显示：[`Self::is_settled`] 与 [`Self::settled`] 只看定下的值。
    #[must_use]
    pub fn guessed(guess: ModelSelection) -> (SettleDefault, Self) {
        let (sender, receiver) = watch::channel(State {
            selection: guess,
            settled: false,
        });
        (SettleDefault(sender), Self(receiver))
    }

    /// 现在的值：定下的值；没定下时是暂定值或空选择。
    #[must_use]
    pub fn current(&self) -> ModelSelection {
        self.0.borrow().selection.clone()
    }

    /// 定下了吗。
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.0.borrow().settled
    }

    /// 等它定下。定的一方没定就释放了（取目录的任务结束或被中止）时按空选择返回，不会一直等。
    pub async fn settled(&self) -> ModelSelection {
        let mut receiver = self.0.clone();
        match receiver.wait_for(|state| state.settled).await {
            Ok(state) => state.selection.clone(),
            Err(_) => unselected(),
        }
    }
}

impl From<ModelSelection> for DefaultModel {
    fn from(selection: ModelSelection) -> Self {
        Self(
            watch::channel(State {
                selection,
                settled: true,
            })
            .1,
        )
    }
}

impl fmt::Debug for DefaultModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.0.borrow();
        let name = if state.settled {
            "DefaultModel"
        } else {
            "DefaultModel(pending)"
        };
        f.debug_tuple(name).field(&state.selection).finish()
    }
}

impl SettleDefault {
    /// 定下缺省模型。
    pub fn settle(self, selection: ModelSelection) {
        self.0.send_replace(State {
            selection,
            settled: true,
        });
    }
}

#[cfg(test)]
#[path = "default_model_tests.rs"]
mod tests;
