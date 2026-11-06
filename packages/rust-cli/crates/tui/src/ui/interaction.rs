// 改写自 apps/zcode-host/crates/tui/src/ui/interaction.rs（feat/rs 79b6691c55）：输入换成
// `InteractionRequest`，回答换成 kernel-api 的回答类型（docs/specs/rust-cli/tui.md §5.2）。
//! 审批、提问与计划审批的界面状态，照 Puffin 的请求（docs/puffin-tui.md §4.2「请求」「提问」「计划」，壳的
//! `state::AskUi` 与 `shell/asks.rs`；docs/specs/rust-cli/tui-kit.md §4.2）：请求本身在视图里（旧会话的询问在
//! `App::detached` 里），这里只有在回答的那一个的选中的回答、提问的页、逐页选中的回答与你自己的话。回答在真的输入框里打
//! （输入框有字时 ⏎ 是 Deny / Keep planning，提问页上的字是这一页自己的回答）。打开之后 [`ARM_TICKS`] 个 tick 内回答不能选，
//! 回答的行从 line 色淡入（以前是 150 ms 内丢掉全部按键，没有样子）。
//!
//! 按键在 `interaction_keys.rs`，画法在 `interaction_view.rs`，请求与回答的形状在 `interaction_ask.rs`，正文里的记号与预览
//! 在 `ask_body.rs`。
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

use rust_cli_kernel_api::ids::{InteractionId, MachineName};
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::permission::PermissionMode;
pub use rust_cli_tui_kit::app::widgets::asks::ARM_TICKS;
use rust_cli_tui_kit::app::widgets::playhead::TICK_MS;

pub use super::interaction_view::View;

/// 在回答的请求的界面状态（Puffin `AskUi`）。
pub struct Interaction {
    pub id: InteractionId,
    /// 选中的回答（提问：这一页上选中的回答）。
    pub choice: usize,
    /// 提问的当前页。
    pub page: usize,
    /// 提问逐页选中的回答（下标）。
    pub picks: BTreeMap<usize, Vec<usize>>,
    /// 提问逐页你自己的话。
    pub own: BTreeMap<usize, String>,
    /// 弹出时会话的权限模式：计划审批的 `Approve{mode}` 填它（只作记录，退出 plan 不改权限模式，修复线 F20）。
    pub mode: PermissionMode,
    /// Bash 询问“不再询问”改过的前缀（工作线 F72，shell-permissions.md §4.10）：⇥ 把前缀放进输入框，输入框里改的就是它
    /// （`editing`）；没改过时为 None，Always allow 记住全部建议规则（Puffin）。
    pub seed: Option<String>,
    /// 输入框里是“不再询问”的前缀（⇥ 放进去的）：⏎ 回答 Always allow，不是 Deny。
    pub editing: bool,
    /// 当前工作目录：远端的调用是那台机器的工作目录（F98）；Always allow 的规则按它写相对路径。
    pub workspace: String,
    /// MCP 工具绑定的机器（F98，remote-exec.md §25.4.3；没有绑定是主 env）：询问里没有 `machine` 时标题照它写
    /// “在 dev-a 上执行”。
    pub bound_machine: MachineName,
    /// 旧会话的询问（父会话已关闭，agents.md §4.1.2，工作线 F72）：那个会话的标题（可能为空）；当前会话的询问为 None。
    pub origin: Option<String>,
    /// 提问的空闲倒计时（秒）：快到期时槽里显示；值来自 `App::afk`（tools-more.md §1.1）。
    pub countdown: Option<u64>,
    /// 成为在回答的那个的时刻：回答从这一刻点亮（Puffin `opened_at`）。
    pub shown: Instant,
    /// 视图钉在问的那一行（Puffin `AskUi.pinned`）：请求打开时为真，滚动一次就放开；预览比正文高时跟随停在问的行，不在
    /// 末尾（frame 的跟随）。
    pub pinned: bool,
}

impl Interaction {
    pub fn new(id: InteractionId, _request: &InteractionRequest, now: Instant) -> Self {
        Self {
            id,
            choice: 0,
            page: 0,
            picks: BTreeMap::new(),
            own: BTreeMap::new(),
            mode: PermissionMode::Default,
            seed: None,
            editing: false,
            workspace: String::new(),
            bound_machine: MachineName::primary(),
            origin: None,
            countdown: None,
            shown: now,
            pinned: true,
        }
    }

    /// 回答点亮了多少：[`ARM_TICKS`] 内从 0 到 1（Puffin `armedness`）。
    pub fn armedness(&self, now: Instant) -> f64 {
        let ticks = ticks_since(self.shown, now);
        (ticks as f64 / ARM_TICKS as f64).min(1.0)
    }

    pub fn armed(&self, now: Instant) -> bool {
        self.armedness(now) >= 1.0
    }
}

/// `at` 到 `now` 走过的 tick（33 ms 一个，向下取整）。
pub fn ticks_since(at: Instant, now: Instant) -> i64 {
    let ms = now.saturating_duration_since(at).as_millis();
    i64::try_from(ms / u128::from(TICK_MS)).unwrap_or(i64::MAX)
}

/// 请求的界面状态里不跟着在回答的那一个走的部分（Puffin `ShellState` 的 `answered`、`ask_seen`）。
#[derive(Debug, Default)]
pub struct Asks {
    /// 已经回答、等它离开视图的请求：回答的按键不起作用，选中的行一直亮着。
    pub answered: HashSet<InteractionId>,
    /// 每个开着的请求第一次出现的时刻：它的行的 `●` 从这一刻淡入（Puffin `ask_seen`）。
    pub seen: HashMap<InteractionId, Instant>,
    /// 刚到、还没响铃的请求（终端通知取走，notify.rs）。
    pub fresh: Vec<InteractionId>,
    /// esc 停下了回合：请求离开时不说“别处答了”（Puffin `stop_pending`）；没有请求时清掉。
    pub stopping: bool,
    /// 在批准的同伴消息与它第一次在批准的时刻：回答从这一刻点亮（`peer_view.rs`）。
    pub peer: Option<(String, Instant)>,
}

#[path = "interaction_remember.rs"]
pub(super) mod remember;

#[path = "interaction_keys.rs"]
mod keys;

#[cfg(test)]
#[path = "interaction_tests.rs"]
mod tests;
