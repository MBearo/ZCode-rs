// 改写自 apps/zcode-host/crates/tui/src/ui/commands.rs（feat/rs 79b6691c55）。
//! 斜杠命令（docs/specs/rust-cli/tui.md §6）：名称、别名与说明固定，产品名换成
//! ZCode。依赖其他工作线的命令也列出来，执行时提示“未实现”。
use rust_cli_kernel_api::user_command::UserCommand;

use super::i18n::Lang;

pub struct Command {
    pub name: &'static str,
    en: &'static str,
    zh: &'static str,
    /// 本工作线已实现；为假时执行只提示“未实现”。
    pub implemented: bool,
}

impl Command {
    /// 参数提示：输入 `/名称 ` 之后在输入框里用 dim 文字显示。
    pub fn arg_hint(&self) -> Option<&'static str> {
        match self.name {
            "model" => Some("[model]"),
            "compact" => Some("<optional custom summarization instructions>"),
            "resume" => Some("[conversation id or search term]"),
            "export" => Some("[filename]"),
            "btw" => Some("[question]"),
            "goal" => Some("[<condition> | clear]"),
            "rename" => Some("[name]"),
            "locale" => Some("[auto|en-US|zh-CN]"),
            // L28（tui-display.md）。
            "clear" => Some("[name]"),
            "plan" => Some("[open|<description>]"),
            "effort" => Some("[low|medium|high|xhigh|max|auto]"),
            // 工作线 F76：照 App 只留三个模式（`plan` 照旧能用，同 `/plan`，不列出）。
            "mode" => Some("[build|guarded|yolo]"),
            "mcp" => Some("[reconnect|enable|disable [<server>|all]]"),
            "copy" => Some("[N]"),
            "cd" => Some("<path>"),
            _ => None,
        }
    }

    pub fn description(&self, lang: Lang) -> &'static str {
        match lang {
            Lang::Zh => self.zh,
            Lang::En => self.en,
        }
    }
}

macro_rules! commands {
    ($($name:literal, $implemented:literal, $en:literal, $zh:literal;)*) => {
        pub const COMMANDS: &[Command] = &[$(Command {
            name: $name, en: $en, zh: $zh, implemented: $implemented,
        },)*];
    };
}

commands! {
    "help", true, "Show help and available commands", "显示帮助与可用命令";
    "clear", true, "Start a new session with empty context; previous session stays on disk (resumable with /resume)", "开始一个上下文为空的新会话；之前的会话留在磁盘上（可用 /resume 恢复）";
    "model", true, "Set the AI model for ZCode", "设置 ZCode 使用的模型";
    "plan", true, "Enable plan mode or view the current session plan", "打开计划模式或查看当前会话的计划";
    "status", true, "Show ZCode status including version, model and session", "查看 ZCode 状态：版本、模型与会话";
    "copy", true, "Copy ZCode's last response to clipboard (or /copy N for the Nth-latest)", "复制 ZCode 的最后一条回复（/copy N 复制倒数第 N 条）";
    "compact", true, "Free up context by summarizing the conversation so far", "总结目前的对话，腾出上下文";
    "resume", true, "Resume a previous conversation", "恢复之前的会话";
    "rewind", true, "Restore the code and/or conversation to a previous point", "把代码或对话恢复到之前的位置";
    "rename", true, "Rename the current conversation", "重命名当前会话";
    "goal", true, "Set a goal ZCode checks before stopping", "设置 ZCode 停止前要检查的目标";
    "fork", true, "Fork the conversation after one of your messages into a new session", "从某条输入之后分出新会话";
    "export", true, "Export the current conversation to a file or clipboard", "把当前会话导出到文件或剪贴板";
    "init", false, "Initialize a new AGENTS.md file with codebase documentation", "为代码库创建 AGENTS.md";
    "memory", true, "Edit AGENTS.md files and memory settings", "编辑 AGENTS.md 与记忆设置";
    "context", true, "Visualize current context usage as a colored grid", "用彩色方格查看上下文用量";
    "permissions", true, "Manage allow and deny tool permission rules", "管理工具的允许与拒绝规则";
    "tasks", true, "View and manage everything running in the background", "查看与管理后台运行的内容";
    "skills", true, "List available skills", "列出可用技能";
    "hooks", true, "View hook configurations for tool events", "查看工具事件的 hook 配置";
    "mcp", true, "Manage MCP servers", "管理 MCP 服务";
    "diff", true, "View uncommitted changes and per-turn diffs", "查看未提交的改动与每轮的 diff";
    "login", true, "Sign in to your account", "登录账号";
    "logout", true, "Sign out from your account", "退出登录";
    "btw", false, "Ask a quick side question without interrupting the main conversation", "旁路提问，不打断主对话";
    "locale", true, "Show or switch the interface language", "查看或切换界面语言";
    "effort", true, "Set effort level for model usage", "设置模型的推理档位";
    "mode", true, "Show or switch the current permission mode", "查看或切换当前权限模式";
    "usage", true, "Show session cost, plan usage, and activity stats", "查看会话用量与活动统计";
    "theme", true, "Change the theme", "切换主题";
    "cd", true, "Move this session to a new working directory", "把会话移到新的工作目录";
    "exit", true, "Exit the CLI", "退出";
}

/// 别名 → 命令名；弹出列表只显示命令名。
const ALIASES: &[(&str, &str)] = &[
    ("reset", "clear"),
    ("new", "clear"),
    ("quit", "exit"),
    ("continue", "resume"),
    ("checkpoint", "rewind"),
    ("undo", "rewind"),
    ("allowed-tools", "permissions"),
    ("bashes", "tasks"),
    ("name", "rename"),
    // Node CLI 的别名（tui.md §7.1）。
    ("language", "locale"),
    // L28：`/usage` 的别名与 Node 的 `/variant`（tui-display.md §4.4、§4.5）。
    ("cost", "usage"),
    ("stats", "usage"),
    ("variant", "effort"),
];

/// 弹出列表的一项：TUI 自己的命令，或内核给出的 skill（ext.md §2.7、tui.md §6）。
#[derive(Clone, Copy)]
pub enum Entry<'a> {
    Builtin(&'static Command),
    Skill(&'a UserCommand),
}

impl<'a> Entry<'a> {
    pub fn name(&self) -> &'a str {
        match self {
            Self::Builtin(command) => command.name,
            Self::Skill(skill) => &skill.name,
        }
    }

    pub fn description(&self, lang: Lang) -> &'a str {
        match self {
            Self::Builtin(command) => command.description(lang),
            Self::Skill(skill) => &skill.description,
        }
    }
}

/// 名字是不是内核给出的 skill（含不进补全的 `hidden`）：是的话整行发给内核展开。
pub fn skill<'a>(skills: &'a [UserCommand], name: &str) -> Option<&'a UserCommand> {
    skills.iter().find(|skill| skill.name == name)
}

/// 补全里与输入匹配的 skill：先前缀，没有前缀匹配时子序列（同 [`matching`]）；`hidden` 的不列。
pub fn matching_skills<'a>(skills: &'a [UserCommand], typed: &str) -> Vec<&'a UserCommand> {
    let typed = typed.to_ascii_lowercase();
    let listed = skills.iter().filter(|skill| !skill.hidden);
    let mut prefix: Vec<&UserCommand> = listed
        .clone()
        .filter(|skill| skill.name.to_ascii_lowercase().starts_with(typed.as_str()))
        .collect();
    prefix.sort_by_key(|skill| skill.name != typed);
    if !prefix.is_empty() {
        return prefix;
    }
    listed
        .filter(|skill| {
            let mut name = skill.name.chars();
            typed.chars().all(|c| name.any(|n| n == c))
        })
        .collect()
}

/// 指向 `name` 的别名（`/help` 里列出）。
pub fn aliases_of(name: &str) -> Vec<&'static str> {
    ALIASES
        .iter()
        .filter(|(_, target)| *target == name)
        .map(|(alias, _)| *alias)
        .collect()
}

fn alias(name: &str) -> Option<&'static Command> {
    let (_, target) = ALIASES.iter().find(|(alias, _)| *alias == name)?;
    COMMANDS.iter().find(|command| command.name == *target)
}

pub fn find(name: &str) -> Option<&'static Command> {
    COMMANDS
        .iter()
        .find(|command| command.name == name)
        .or_else(|| alias(name))
}

/// 先按前缀匹配，没有前缀匹配时按子序列（`/mdl` 匹配 `/model`）。
pub fn matching(typed: &str) -> Vec<&'static Command> {
    let typed = typed.to_ascii_lowercase();
    // 完全匹配排在最前，其余前缀匹配保持表里的顺序。
    let mut prefix: Vec<&Command> = COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(typed.as_str()))
        .collect();
    prefix.sort_by_key(|c| c.name != typed);
    // 完整输入了别名（`/quit`）：别名指向的命令排第一。
    if let Some(target) = alias(&typed) {
        prefix.retain(|c| c.name != target.name);
        prefix.insert(0, target);
    }
    if !prefix.is_empty() {
        return prefix;
    }
    COMMANDS
        .iter()
        .filter(|command| {
            let mut name = command.name.chars();
            typed.chars().all(|c| name.any(|n| n == c))
        })
        .collect()
}

/// 名称里与输入匹配的字符下标（弹出列表里加粗）。
pub fn matched_positions(name: &str, typed: &str) -> Vec<usize> {
    if name.starts_with(typed) {
        return (0..typed.chars().count()).collect();
    }
    let mut positions = Vec::new();
    let mut typed = typed.chars().peekable();
    for (index, c) in name.chars().enumerate() {
        if typed.peek() == Some(&c) {
            positions.push(index);
            typed.next();
        }
    }
    positions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_matches_win_over_subsequences() {
        let names = |typed: &str| matching(typed).iter().map(|c| c.name).collect::<Vec<_>>();
        assert_eq!(names("m"), ["model", "memory", "mcp", "mode"]);
        assert_eq!(names("exit"), ["exit"]);
        assert_eq!(names("mdl"), ["model"]);
        assert_eq!(
            names(""),
            COMMANDS.iter().map(|c| c.name).collect::<Vec<_>>()
        );
        assert!(names("zzz").is_empty());
        assert_eq!(matched_positions("model", "mdl"), [0, 2, 4]);
    }

    /// 用户决定删除 `/agents`（docs/specs/rust-cli/tui-display.md §3.2，L33）：命令表、别名与补全里都没有，
    /// 提交时走未知命令。
    #[test]
    fn agents_is_not_a_command() {
        assert!(find("agents").is_none());
        assert!(matching("agents").is_empty());
        assert!(
            matching("ag")
                .iter()
                .all(|command| command.name != "agents")
        );
    }

    #[test]
    fn aliases_find_their_command_and_come_first() {
        let first = |typed: &str| matching(typed).first().map(|c| c.name);
        assert_eq!(first("new"), Some("clear"));
        assert_eq!(first("quit"), Some("exit"));
        assert_eq!(find("reset").map(|c| c.name), Some("clear"));
        assert_eq!(aliases_of("clear"), ["reset", "new"]);
    }
}
