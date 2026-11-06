//! 替换输入框或盖在板上的视图（`Overlay`，架构整理 U1 把类型放在这里）。各视图的按键、粘贴、滚轮与绘制按 kit 的板与页
//! 分派：跟着草稿的一层在 `draft_layer.rs`，其余在 `keys.rs`、`wheel.rs`、`draw.rs`、`page_frame.rs`；按键与粘贴给同一层
//! （`keys.rs` 的 `on_paste` 与 `dispatch_key` 同一个次序）。
use super::list::ListView;

/// 替换输入框（底部区域）或盖在板上的视图。
#[derive(Debug, Clone, PartialEq)]
pub enum Overlay {
    List(ListView),
    Shortcuts,
    /// `/diff`：当前改动与每回合的改动（tui-manage.md §4）。
    Diff(super::diff_view::DiffView),
    /// `/resume` 的会话列表：板上的一层，跟着草稿 `/resume <词>`（`sessions_list.rs`，tui-kit.md §4.2「`/resume`」）。
    Sessions(Box<super::sessions_list::SessionsList>),
    /// `/rewind` 的点列表：板上的一层，跟着草稿 `/rewind <词>`（`rewind.rs`，tui-kit.md §4.2「`/rewind`」）。
    Points(super::rewind::PointsList),
    /// `/fork` 的点列表：同 `/rewind` 的那一层，跟着草稿 `/fork <词>`（`fork.rs`，tui-kit.md §4.3「`/fork`」）。
    Forks(super::fork::ForkList),
    /// 切到别的项目之前的信任对话框（workspace-trust.md §2.8）。
    Trust(Box<super::trust_ask::TrustAsk>),
    /// 任务卡（tui-tasks.md §3）：子代理、后台命令与待办，^T 打开；板上的一层。
    Tasks(super::task_card::TaskCard),
    /// `/usage` 的 Usage / Stats 页面（usage.md §4、§5）。装箱：统计数据比其他变体大得多。
    Usage(Box<super::usage_local::LocalView>),
    /// 切到完全访问的确认框（tui.md §7.8，工作线 F87）：替换底部区域，不是全屏视图。
    YoloConfirm(super::yolo_confirm::YoloConfirm),
    /// `/machines` 的列表（远端执行 R4，remote-exec.md §21.3）：替换底部区域。
    Machines(super::machines::MachinesView),
    /// 输入层的卡：`/stats` 与 `/goal`（tui-kit.md §4.2，`cards.rs`）；草稿空着时在板上。
    Card(super::cards::Card),
}

impl Overlay {
    /// 占满整个屏幕、不画 transcript 的视图：鼠标在这里按屏幕坐标选择。只有信任询问（照 Puffin 占满整屏）。
    pub fn full_screen(&self) -> bool {
        match self {
            Self::Trust(_) => true,
            Self::List(_)
            | Self::Shortcuts
            | Self::Diff(_)
            | Self::Sessions(_)
            | Self::Points(_)
            | Self::Forks(_)
            | Self::Tasks(_)
            | Self::Usage(_)
            | Self::YoloConfirm(_)
            | Self::Machines(_)
            | Self::Card(_) => false,
        }
    }
}
