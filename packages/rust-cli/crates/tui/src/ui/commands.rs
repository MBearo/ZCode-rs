// 改写自 apps/zcode-host/crates/tui/src/ui/commands.rs（feat/rs 79b6691c55）。
//! 斜杠命令（docs/specs/rust-cli/tui.md §6）：名称、别名与说明固定，产品名换成
//! ZCode。依赖其他工作线的命令也列出来，执行时提示“未实现”。内置提示词命令（`/init`、`/security-review`，修复线 L-A）
//! 是内核给出的 skill 命令，不在这张表里。
use rust_cli_kernel_api::session_api::Delivery;
use rust_cli_kernel_api::user_command::UserCommand;

use super::app::{Action, App};
use super::i18n::Lang;
use super::manage::Manage;

/// 一条斜杠命令：名字、说明、参数提示、别名与执行，都在 [`COMMANDS`] 的一项里。
///
/// 架构整理 U6：以前名字写在 4 处（说明的宏、参数提示的 `match`、`slash` 与 `slash_more` 的两段分派），漏写分派时
/// 运行时才落到“未实现”；现在每一项必须写 `run`，漏写是编译错误。
pub struct Command {
    pub name: &'static str,
    en: &'static str,
    zh: &'static str,
    /// 参数提示：输入 `/名称 ` 之后在输入框里用 dim 文字显示。
    pub hint: Option<&'static str>,
    /// 别名（`/help` 里列出；弹出列表只显示命令名）。
    pub aliases: &'static [&'static str],
    /// 执行；None 是依赖其他工作线、还没实现的命令（只提示“未实现”）。
    pub run: Option<Run>,
}

/// 命令的执行：返回给 runner 的动作；只改界面状态时返回 None。
pub type Run = fn(&mut App, &Call<'_>) -> Option<Action>;

/// 一次调用：输入的名字（可能是别名）、参数（去掉首尾空白）与整行。
pub struct Call<'a> {
    pub typed: &'a str,
    pub args: &'a str,
    pub text: &'a str,
}

impl Command {
    pub fn description(&self, lang: Lang) -> &'static str {
        match lang {
            Lang::Zh => self.zh,
            Lang::En => self.en,
        }
    }

    /// 本工作线已实现。
    pub fn implemented(&self) -> bool {
        self.run.is_some()
    }
}

/// 有参数时用 `with`，没有时用 `without`（多数命令的“带不带参数”两种形态）。
fn by_args(call: &Call<'_>, without: Action, with: fn(String) -> Action) -> Option<Action> {
    Some(if call.args.is_empty() {
        without
    } else {
        with(call.args.to_owned())
    })
}

fn manage(manage: Manage) -> Option<Action> {
    Some(Action::Manage(manage))
}

/// 命令表：顺序就是补全与 `/help` 的顺序。别名里有 Node CLI 的（`language`，tui.md §7.1）与 L28 的（`cost`、`stats`、
/// `variant`，tui-display.md §4.4、§4.5）。
pub const COMMANDS: &[Command] = &[
    Command {
        name: "help",
        en: "Show help and available commands",
        zh: "显示帮助与可用命令",
        hint: None,
        aliases: &[],
        run: Some(|app, _| app.help()),
    },
    Command {
        name: "clear",
        en: "Start a new session with empty context; previous session stays on disk (resumable with /resume)",
        zh: "开始一个上下文为空的新会话；之前的会话留在磁盘上（可用 /resume 恢复）",
        hint: Some("[name]"),
        aliases: &["reset", "new"],
        // 带名字且有会话时先给当前会话改名（tui-display.md）。
        run: Some(
            |app, call| match (call.args.is_empty(), app.session.is_some()) {
                (false, true) => manage(Manage::ClearNamed(call.args.to_owned())),
                (true, _) | (false, false) => Some(Action::Clear),
            },
        ),
    },
    Command {
        name: "model",
        en: "Set the AI model for ZCode",
        zh: "设置 ZCode 使用的模型",
        hint: Some("[model]"),
        aliases: &[],
        run: Some(|_, call| by_args(call, Action::OpenModels, Action::PickModel)),
    },
    Command {
        name: "plan",
        en: "Enable plan mode or view the current session plan",
        zh: "打开计划模式或查看当前会话的计划",
        hint: Some("[open|<description>]"),
        aliases: &[],
        // `open` 在外部编辑器打开计划文件（tui-manage.md §8）。
        run: Some(|app, call| match call.args {
            "" => app.toggle_plan(),
            "open" => manage(Manage::PlanOpen),
            args => manage(Manage::PlanPrompt(args.to_owned())),
        }),
    },
    Command {
        name: "status",
        en: "Show ZCode status including version, model and session",
        zh: "查看 ZCode 状态：版本、模型与会话",
        hint: None,
        aliases: &[],
        run: Some(|_, _| manage(Manage::Status)),
    },
    Command {
        name: "copy",
        en: "Copy ZCode's last response to clipboard (or /copy N for the Nth-latest)",
        zh: "复制 ZCode 的最后一条回复（/copy N 复制倒数第 N 条）",
        hint: Some("[N]"),
        aliases: &[],
        run: Some(|app, call| app.copy_nth(call.args)),
    },
    Command {
        name: "compact",
        en: "Free up context by summarizing the conversation so far",
        zh: "总结目前的对话，腾出上下文",
        hint: Some("<optional custom summarization instructions>"),
        aliases: &[],
        run: Some(|_, call| {
            Some(Action::Compact(
                (!call.args.is_empty()).then(|| call.args.to_owned()),
            ))
        }),
    },
    Command {
        name: "resume",
        en: "Resume a previous conversation",
        zh: "恢复之前的会话",
        hint: Some("[conversation id or search term]"),
        aliases: &["continue"],
        run: Some(|_, call| by_args(call, Action::OpenResume, Action::Resume)),
    },
    Command {
        name: "rewind",
        en: "Restore the code and/or conversation to a previous point",
        zh: "把代码或对话恢复到之前的位置",
        hint: None,
        aliases: &["checkpoint", "undo"],
        run: Some(|_, _| Some(Action::OpenRewind)),
    },
    Command {
        name: "rename",
        en: "Rename the current conversation",
        zh: "重命名当前会话",
        hint: Some("[name]"),
        aliases: &["name"],
        // 不带名字时用模型起名（titles.md §8）。
        run: Some(|_, call| by_args(call, Action::GenerateName, Action::Rename)),
    },
    Command {
        name: "goal",
        en: "Set a goal ZCode checks before stopping",
        zh: "设置 ZCode 停止前要检查的目标",
        hint: Some("[<condition> | clear]"),
        aliases: &[],
        // 不带参数：只读面板；其余整行发给内核（会话 actor 是目标的唯一 owner，goal.md §2）。
        run: Some(|_, call| {
            Some(match call.args {
                "" => Action::ShowGoal,
                _ => Action::Submit {
                    text: call.text.trim().to_owned(),
                    delivery: Delivery::Queue,
                },
            })
        }),
    },
    Command {
        name: "fork",
        en: "Fork the conversation after one of your messages into a new session",
        zh: "从某条输入之后分出新会话",
        hint: None,
        aliases: &[],
        // 按用户输入分叉（session-lifecycle.md §6.5.3，F75）。参数忽略、照样打开选择器（用户决定，F78；F75 时报错）。
        run: Some(|_, _| Some(Action::OpenFork)),
    },
    Command {
        name: "export",
        en: "Export the current conversation to a file or clipboard",
        zh: "把当前会话导出到文件或剪贴板",
        hint: Some("[filename]"),
        aliases: &[],
        run: Some(|app, call| app.export_command(call.args)),
    },
    Command {
        name: "memory",
        en: "Edit AGENTS.md files and memory settings",
        zh: "编辑 AGENTS.md 与记忆设置",
        hint: None,
        aliases: &[],
        run: Some(|app, _| app.memory()),
    },
    Command {
        name: "context",
        en: "Visualize current context usage as a colored grid",
        zh: "用彩色方格查看上下文用量",
        hint: None,
        aliases: &[],
        run: Some(|_, call| {
            Some(Action::Context {
                all: call.args.eq_ignore_ascii_case("all"),
            })
        }),
    },
    Command {
        name: "permissions",
        en: "Manage allow and deny tool permission rules",
        zh: "管理工具的允许与拒绝规则",
        hint: None,
        aliases: &["allowed-tools"],
        run: Some(|_, _| manage(Manage::Permissions)),
    },
    Command {
        name: "tasks",
        en: "View and manage everything running in the background",
        zh: "查看与管理后台运行的内容",
        hint: None,
        aliases: &["bashes"],
        run: Some(|_, _| manage(Manage::Tasks)),
    },
    Command {
        name: "skills",
        en: "List available skills",
        zh: "列出可用技能",
        hint: None,
        aliases: &[],
        run: Some(|app, _| app.skills_page()),
    },
    Command {
        name: "hooks",
        en: "View hook configurations for tool events",
        zh: "查看工具事件的 hook 配置",
        hint: None,
        aliases: &[],
        run: Some(|_, _| manage(Manage::Hooks)),
    },
    Command {
        name: "mcp",
        en: "Manage MCP servers",
        zh: "管理 MCP 服务",
        hint: Some("[reconnect|enable|disable [<server>|all]]"),
        aliases: &[],
        // reconnect / enable / disable（tui-manage.md §6）：先取服务器快照再判断。
        run: Some(|_, call| match call.args {
            "" => manage(Manage::Mcp),
            args => manage(Manage::McpControl(args.to_owned())),
        }),
    },
    Command {
        name: "diff",
        en: "View uncommitted changes and per-turn diffs",
        zh: "查看未提交的改动与每轮的 diff",
        hint: None,
        aliases: &[],
        run: Some(|app, _| app.open_diff()),
    },
    Command {
        name: "login",
        en: "Sign in to your account",
        zh: "登录账号",
        hint: None,
        aliases: &[],
        run: Some(|app, call| app.login_command(call.args)),
    },
    Command {
        name: "logout",
        en: "Sign out from your account",
        zh: "退出登录",
        hint: None,
        aliases: &[],
        run: Some(|app, call| app.logout_command(call.args)),
    },
    Command {
        name: "btw",
        en: "Ask a quick side question without interrupting the main conversation",
        zh: "旁路提问，不打断主对话",
        hint: Some("[question]"),
        aliases: &[],
        run: None,
    },
    Command {
        name: "locale",
        en: "Show or switch the interface language",
        zh: "查看或切换界面语言",
        hint: Some("[auto|en-US|zh-CN]"),
        aliases: &["language"],
        run: Some(|app, call| app.locale_command(call.args)),
    },
    Command {
        name: "effort",
        en: "Set effort level for model usage",
        zh: "设置模型的推理档位",
        hint: Some("[low|medium|high|xhigh|max|auto]"),
        aliases: &["variant"],
        run: Some(|_, call| manage(Manage::Effort(call.args.to_owned()))),
    },
    Command {
        name: "mode",
        en: "Show or switch the current permission mode",
        zh: "查看或切换当前权限模式",
        // 工作线 F76：照 App 只留三个模式（`plan` 照旧能用，同 `/plan`，不列出）。
        hint: Some("[build|guarded|yolo]"),
        aliases: &[],
        run: Some(|app, call| app.mode_command(call.args)),
    },
    Command {
        name: "usage",
        en: "Show session cost, plan usage, and activity stats",
        zh: "查看会话用量与活动统计",
        hint: None,
        aliases: &["cost", "stats"],
        // API 时长与统计要问内核（usage.md §4、§5）；`/stats` 开 Stats 标签页（按输入的名字选）。
        run: Some(|_, call| {
            manage(Manage::Usage {
                stats: call.typed == "stats",
            })
        }),
    },
    Command {
        name: "theme",
        en: "Change the theme",
        zh: "切换主题",
        hint: None,
        aliases: &[],
        run: Some(|app, _| app.theme_list()),
    },
    Command {
        name: "cd",
        en: "Move this session to a new working directory",
        zh: "把会话移到新的工作目录",
        hint: Some("<path>"),
        aliases: &[],
        run: Some(|app, call| app.cd_command(call.args)),
    },
    Command {
        name: "machines",
        en: "Manage remote machines that tools can run on",
        zh: "管理工具可以在上面执行的远端机器",
        hint: Some("[add <name> <ssh target> [dir] | connect | disconnect | remove <name>]"),
        aliases: &[],
        // 远端机器（remote-exec.md §21.3）。
        run: Some(|app, call| app.machines_command(call.args)),
    },
    Command {
        name: "exit",
        en: "Exit the CLI",
        zh: "退出",
        hint: None,
        aliases: &["quit"],
        run: Some(|_, _| Some(Action::Quit)),
    },
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

fn alias(name: &str) -> Option<&'static Command> {
    COMMANDS
        .iter()
        .find(|command| command.aliases.contains(&name))
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
        assert_eq!(names("m"), ["model", "memory", "mcp", "mode", "machines"]);
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
        assert_eq!(
            find("clear").map(|c| c.aliases),
            Some(&["reset", "new"][..])
        );
    }
}
