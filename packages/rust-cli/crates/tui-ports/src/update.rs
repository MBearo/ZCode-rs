//! 自动更新的选择屏与会话提示要显示的内容、用户的选择（docs/specs/rust-cli/update.md §6、§7，工作线 F48）：两套终端界面
//! 共用的类型；要不要弹、读写缓存、执行安装都在入口，画法在各自的界面 crate。

/// 选择屏与会话提示要显示的内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    /// 当前版本。
    pub current: String,
    /// npm 上的最新版本。
    pub latest: String,
    /// 显示用的安装命令行（例如 `npm install -g @scope/zcode`）。
    pub command: String,
    /// release notes 链接（入口包的 npm 页面）。
    pub notes_url: String,
}

/// 用户的选择：立即更新、这次跳过、跳过到下一个版本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateChoice {
    UpdateNow,
    Skip,
    SkipUntilNext,
}
