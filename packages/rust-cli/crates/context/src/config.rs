//! 上下文组装的配置（docs/specs/rust-cli/context.md §2）。
//!
//! 由入口 crate 按进程形态与设置构造，经 runtime 传给 [`crate::PromptAssembler`]；不读进程环境。

/// 前端形态：`-p`（非交互）与 TUI 的提示词有差别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// `-p` / stream-json。
    Print,
    /// 终端界面。
    Interactive,
}

/// 输出风格（设置 `outputStyle`）。设置了输出风格时，身份后的第一句换成按风格回答的说法。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputStyle {
    pub name: String,
    pub keep_coding_instructions: bool,
}

/// commit 与 PR 的署名行（attribution 设置）。为空的一行不提。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    pub commit: Option<String>,
    pub pr: Option<String>,
}

impl Default for Attribution {
    /// ZCode 的署名（用户决定，见 context.md §4.6）。
    fn default() -> Self {
        Self {
            commit: Some(rust_cli_prompts::texts::attribution_commit_value().to_owned()),
            pr: Some(rust_cli_prompts::texts::attribution_pr_value().to_owned()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextConfig {
    pub surface: Surface,
    /// 自动记忆目录（展示给模型的路径文本）；None 表示自动记忆关闭，不渲染记忆段。
    /// 目录由 `StoragePaths::memory_dir` 解析，并由记忆能力保证已经存在（提示词说它已存在）。
    pub memory_dir: Option<String>,
    pub output_style: Option<OutputStyle>,
    pub attribution: Attribution,
    /// `--system-prompt`（docs/specs/rust-cli/cli-args.md §4）：替换缺省的 system prompt 段落；这时也不发 git 状态
    /// （systemContext 为空），规则文件与日期照常。
    pub custom_system_prompt: Option<String>,
    /// `--append-system-prompt`：接在 system prompt 最后一段之后（空行连接）。
    pub append_system_prompt: Option<String>,
}

impl ContextConfig {
    pub fn new(surface: Surface) -> Self {
        Self {
            surface,
            memory_dir: None,
            output_style: None,
            attribution: Attribution::default(),
            custom_system_prompt: None,
            append_system_prompt: None,
        }
    }
}
