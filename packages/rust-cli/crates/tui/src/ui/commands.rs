// 改写自 apps/zcode-host/crates/tui/src/ui/commands.rs（feat/rs 79b6691c55）。
//! 斜杠命令（docs/specs/rust-cli/tui.md §6）：名称、别名与说明固定，产品名换成
//! ZCode。依赖其他工作线的命令也列出来，执行时提示“未实现”。内置提示词命令（`/init`、`/security-review`，修复线 L-A）
//! 是内核给出的 skill 命令，不在这张表里。
use rust_cli_kernel_api::session_api::Delivery;
use rust_cli_kernel_api::user_command::UserCommand;

use super::app::{Action, App};
use super::i18n::Lang;
use super::manage::Manage;

/// 一条斜杠命令：名字、说明、参数提示、别名、收不收词与执行，都在 [`COMMANDS`] 的一项里。
///
/// 架构整理 U6：以前名字写在 4 处（说明的宏、参数提示的 `match`、`slash` 与 `slash_more` 的两段分派），漏写分派时
/// 运行时才落到“未实现”；现在每一项必须写 `run`，漏写是编译错误。
pub struct Command {
    pub name: &'static str,
    en: &'static str,
    zh: &'static str,
    /// 参数提示：输入 `/名称 ` 之后在输入框里用 fg3 斜体显示（Puffin `consts::hint` 的说法：小写的词，不带括号）。Puffin 有的
    /// 命令照抄它的词；旧界面才有的命令照同样的写法。Puffin 的命令收旧界面才有的参数时（`/compact 说明`、`/new 名字`、
    /// `/mode 名字`、`/mcp reconnect`）照 Puffin 不写提示。
    pub hint: Option<&'static str>,
    /// 别名：打全了的别名运行这条命令（`/help` 里列出）。命令架只按 [`SHELF_ALIASES`] 列。
    pub aliases: &'static [&'static str],
    /// 补全时名字后面加一个空格：命令收词（Puffin `consts::words` 不是 `Never` 的，加上旧界面才有的 `/cd`、`/locale`、
    /// `/btw`）。
    pub words: bool,
    /// 执行；None 是还没实现的命令（执行只提示“未实现”，命令架上是灰色并说原因，Puffin「现在不能运行的命令」）。
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
    /// 参数提示（[`Command::hint`]）。
    pub fn arg_hint(&self) -> Option<&'static str> {
        self.hint
    }

    /// 补全时名字后面加一个空格（[`Command::words`]）。
    pub fn takes_words(&self) -> bool {
        self.words
    }

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

fn manage(manage: Manage) -> Option<Action> {
    Some(Action::Manage(manage))
}

// 命令架的次序与说明照 Puffin（docs/puffin-tui.md §4.2「命令与顺序」，`shell/consts.rs` 的 `AGENT_COMMANDS` 与
// `OWN_COMMANDS`；docs/specs/rust-cli/tui.md §6）：agent 的内置命令（`goal`、`compact`、内核给出的 `init`、`plan`，
// 说明同 Puffin 的 host）在前，然后是 TUI 自己的命令，最后是内核给出的其余 skill。旧界面才有的命令（`/cd`、`/fork`、
// `/locale`、`/logout`、`/tasks`、`/diff`、`/btw`）照 Puffin 的写法写说明，排在 Puffin 的命令之后（Puffin 把用户的
// 命令接在自己的命令之后，同一个位置；tui.md §6）。`init` 不在这张表里：它是内核的 skill，命令架把它排在 `compact` 之后
// （`composer_popup.rs`）。别名里 Puffin 的七个（`compress`、`target`、`context`、`status`、`clear`、`effort`、`quit`）之外是
// 旧界面原有的，另有 Node CLI 的 `language`（tui.md §7.1）与 L28 的 `cost`、`variant`（tui-display.md §4.4）。
pub const COMMANDS: &[Command] = &[
    Command {
        name: "goal",
        en: "Show or set the current session goal.",
        zh: "查看或设置当前会话的目标。",
        hint: Some("objective"),
        aliases: &["target"],
        words: true,
        // `/goal`、`/goal show`：目标的卡（Puffin 的 `"" | "show"`，cards.rs）；其余整行发给内核（会话 actor 是目标的唯一
        // owner，goal.md §2）。
        run: Some(|_, call| {
            Some(
                if call.args.is_empty() || call.args.eq_ignore_ascii_case("show") {
                    Action::ShowGoal
                } else {
                    Action::Submit {
                        text: call.text.trim().to_owned(),
                        delivery: Delivery::Queue,
                    }
                },
            )
        }),
    },
    Command {
        name: "compact",
        en: "Compact the current conversation with optional instructions.",
        zh: "压缩当前对话，可以附上说明。",
        hint: None,
        aliases: &["compress"],
        words: false,
        run: Some(|_, call| {
            Some(Action::Compact(
                (!call.args.is_empty()).then(|| call.args.to_owned()),
            ))
        }),
    },
    Command {
        name: "plan",
        en: "Switch to Plan mode and optionally send a task.",
        zh: "切到计划模式，可以顺带发出任务。",
        hint: Some("task"),
        aliases: &[],
        words: true,
        // `open` 在外部编辑器打开计划文件（tui-manage.md §8；编辑器是 L27 的端口）。
        run: Some(|app, call| match call.args {
            "" => app.toggle_plan(),
            "open" => manage(Manage::PlanOpen),
            args => manage(Manage::PlanPrompt(args.to_owned())),
        }),
    },
    Command {
        name: "new",
        en: "Start a new session.",
        zh: "开始一个新会话。",
        hint: None,
        aliases: &["clear", "reset"],
        words: false,
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
        en: "Switch the model and its effort.",
        zh: "切换模型与推理档位。",
        hint: None,
        aliases: &["effort", "variant"],
        words: true,
        run: Some(|app, call| app.list_by_words("model", call)),
    },
    Command {
        name: "resume",
        en: "Open an earlier session in this folder.",
        zh: "打开这个文件夹里之前的会话。",
        hint: Some("title or words"),
        aliases: &["continue"],
        words: true,
        run: Some(|_, call| {
            Some(match call.args {
                "" => Action::OpenResume,
                args => Action::Resume(args.to_owned()),
            })
        }),
    },
    Command {
        name: "cockpit",
        en: "See every agent in every folder.",
        zh: "查看所有文件夹的所有代理。",
        hint: None,
        aliases: &[],
        words: false,
        run: Some(|app, _| app.open_cockpit()),
    },
    Command {
        name: "rename",
        en: "Name this session.",
        zh: "给这个会话起名。",
        hint: Some("name"),
        aliases: &["name"],
        words: true,
        // 不带名字时用模型起名（titles.md §8）。
        run: Some(|_, call| {
            Some(match call.args {
                "" => Action::GenerateName,
                args => Action::Rename(args.to_owned()),
            })
        }),
    },
    Command {
        name: "rewind",
        en: "Go back to an earlier message.",
        zh: "回到之前的一条消息。",
        hint: Some("words"),
        aliases: &["checkpoint", "undo"],
        words: true,
        run: Some(|_, _| Some(Action::OpenRewind)),
    },
    Command {
        name: "mode",
        en: "Change the permission mode.",
        zh: "切换权限模式。",
        hint: None,
        aliases: &[],
        words: true,
        run: Some(|app, call| app.mode_command(call.args)),
    },
    // 设置与它的一段的捷径（Puffin `SETTINGS_SHORTCUTS`，tui-kit.md §4.2「设置」）：`/permissions` 同
    // `/settings permissions`，`/skills`、`/hooks`、`/memory`、不带词的 `/mcp` 同理。
    Command {
        name: "permissions",
        en: "See what the agent may do without asking.",
        zh: "查看代理不问就能做的事。",
        hint: None,
        aliases: &["allowed-tools"],
        words: false,
        run: Some(|app, _| app.settings_open("permissions")),
    },
    Command {
        name: "theme",
        en: "Change the colours.",
        zh: "切换配色。",
        hint: Some("name"),
        aliases: &[],
        words: true,
        run: Some(|app, call| app.list_by_words("theme", call)),
    },
    Command {
        name: "settings",
        en: "Change ZCode's settings.",
        zh: "修改 ZCode 的设置。",
        hint: Some("section or words"),
        aliases: &[],
        words: true,
        run: Some(|app, call| app.settings_open(call.args)),
    },
    Command {
        name: "memory",
        en: "See the instruction files the agent reads.",
        zh: "查看代理读取的指令文件。",
        hint: None,
        aliases: &[],
        words: false,
        run: Some(|app, _| app.settings_open("memory")),
    },
    Command {
        name: "mcp",
        en: "See the MCP servers and turn them on or off.",
        zh: "查看 MCP 服务器，打开或关闭它们。",
        hint: None,
        aliases: &[],
        words: false,
        // 带词时照旧是旧界面的 reconnect / enable / disable（tui-manage.md §6）：先取服务器快照再判断。
        run: Some(|app, call| match call.args {
            "" => app.settings_open("mcp"),
            args => manage(Manage::McpControl(args.to_owned())),
        }),
    },
    Command {
        name: "skills",
        en: "See the skills the agent can use.",
        zh: "查看代理能用的技能。",
        hint: None,
        aliases: &[],
        words: false,
        run: Some(|app, _| app.settings_open("skills")),
    },
    Command {
        name: "hooks",
        en: "See the hooks that run on tool events.",
        zh: "查看工具事件触发的 hook。",
        hint: None,
        aliases: &[],
        words: false,
        run: Some(|app, _| app.settings_open("hooks")),
    },
    Command {
        name: "machines",
        en: "Manage the remote machines tools can run on.",
        zh: "管理工具可以在上面运行的远端机器。",
        hint: Some("add, connect, disconnect, retry, remove"),
        aliases: &[],
        words: true,
        // 远端机器（remote-exec.md §21.3）。
        run: Some(|app, call| app.machines_command(call.args)),
    },
    Command {
        name: "usage",
        en: "See how much of your plan is left.",
        zh: "查看套餐还剩多少。",
        hint: None,
        aliases: &["cost"],
        words: false,
        // 照 Puffin：`/usage` 是计划的卡（usage_plans.rs）；`/usage all` 直接打开详情页之后多出的「This computer」页
        // （会话的 API 时长与本机的活动统计，旧界面的能力，usage_local.rs，tui-kit.md §4.3）。
        run: Some(|app, call| {
            if call.args.eq_ignore_ascii_case("all") {
                manage(Manage::Usage)
            } else {
                app.open_plan_usage()
            }
        }),
    },
    Command {
        name: "login",
        en: "Sign in, or add an API key.",
        zh: "登录，或添加 API key。",
        hint: None,
        aliases: &[],
        words: true,
        run: Some(|app, call| app.login_command(call.args)),
    },
    Command {
        name: "stats",
        en: "See this session's context, cache, speed and tokens.",
        zh: "查看这个会话的上下文、缓存、速度与 token。",
        hint: None,
        aliases: &["context", "status"],
        words: false,
        // 草稿清空，数字的卡在板上（Puffin，cards.rs）；卡上 ⏎ 是页的 Context 标签，⇥ 是 Session 标签。`/stats all`
        // （`/context all`）打开卡并直接打开 Context，`/status` 打开卡并直接打开 Session（Puffin 没有，tui-panels.md §4.1）。
        // 都要上下文的计数；Session 另要它的事实（session_page.rs），runner 在问计数之后按页问。
        run: Some(|app, call| {
            let count = app.open_stats();
            let tab = if call.typed.eq_ignore_ascii_case("status") {
                Some(super::stats_page::Tab::Session)
            } else if call.args.eq_ignore_ascii_case("all") {
                Some(super::stats_page::Tab::Context)
            } else {
                None
            };
            if let Some(tab) = tab {
                let _ = app.open_stats_page(tab);
            }
            Some(count)
        }),
    },
    Command {
        name: "autocompact",
        en: "See or set when the conversation is compacted.",
        zh: "查看或设置对话什么时候压缩。",
        hint: Some("tokens"),
        aliases: &[],
        words: true,
        run: Some(|app, call| app.auto_compact_command(call.args)),
    },
    Command {
        name: "copy",
        en: "Copy a response. /copy 2 copies the one before the last.",
        zh: "复制一个回答。/copy 2 复制倒数第二个。",
        hint: Some("N"),
        aliases: &[],
        words: true,
        run: Some(|app, call| app.copy_nth(call.args)),
    },
    Command {
        name: "export",
        en: "Save or copy the conversation as text.",
        zh: "把对话存成文字或复制。",
        hint: Some("file name"),
        aliases: &[],
        words: true,
        run: Some(|app, call| app.export_command(call.args)),
    },
    Command {
        name: "keys",
        en: "Show the keys.",
        zh: "查看按键。",
        hint: None,
        aliases: &["help"],
        words: false,
        // 草稿变成 `?`，按键表开着（Puffin `/keys`）。
        run: Some(|app, _| {
            app.composer.set_text("?");
            app.keys_closed = false;
            None
        }),
    },
    Command {
        name: "exit",
        en: "Quit ZCode.",
        zh: "退出 ZCode。",
        hint: None,
        aliases: &["quit"],
        words: false,
        run: Some(|_, _| Some(Action::Quit)),
    },
    Command {
        name: "cd",
        en: "Move this session to another folder.",
        zh: "把这个会话移到另一个文件夹。",
        hint: Some("folder"),
        aliases: &[],
        words: true,
        run: Some(|app, call| app.cd_command(call.args)),
    },
    Command {
        name: "fork",
        en: "Start a new session from one of your messages.",
        zh: "从你的一条消息开始一个新会话。",
        hint: None,
        aliases: &[],
        words: false,
        // 按用户输入分叉（session-lifecycle.md §6.5.3，F75）。参数忽略、照样打开选择器（用户决定，F78；F75 时报错）。
        run: Some(|_, _| Some(Action::OpenFork)),
    },
    Command {
        name: "locale",
        en: "Change the interface language.",
        zh: "切换界面语言。",
        hint: Some("auto, en-US or zh-CN"),
        aliases: &["language"],
        words: true,
        run: Some(|app, call| app.locale_command(call.args)),
    },
    Command {
        name: "logout",
        en: "Sign out.",
        zh: "退出登录。",
        hint: None,
        aliases: &[],
        words: false,
        run: Some(|app, call| app.logout_command(call.args)),
    },
    Command {
        name: "tasks",
        en: "See subagents, background commands and todos.",
        zh: "查看子代理、后台命令与待办。",
        hint: None,
        aliases: &["task", "bashes"],
        words: false,
        run: Some(|_, _| manage(Manage::Tasks)),
    },
    Command {
        name: "diff",
        en: "Show the changes in this folder.",
        zh: "查看这个文件夹里的改动。",
        hint: None,
        aliases: &[],
        words: false,
        run: Some(|app, _| app.open_diff()),
    },
    Command {
        name: "btw",
        en: "Ask a quick side question.",
        zh: "旁路问一个问题。",
        hint: Some("question"),
        aliases: &[],
        words: true,
        run: None,
    },
];

/// 命令架上打出开头时也列出所指命令的别名：照 Puffin，只有它的七个（`shelf::matches`）。旧界面原有的别名（`help`、`undo`、
/// `cost`、`bashes` …）不上架：以前打 `/u` 时 `/rewind` 也上架（经 `undo`），Puffin 只有 `/usage`（twin `usage-page`）；
/// 它们打全了照样能用。
pub const SHELF_ALIASES: &[(&str, &str)] = &[
    ("compress", "compact"),
    ("target", "goal"),
    ("context", "stats"),
    ("status", "stats"),
    ("clear", "new"),
    ("effort", "model"),
    ("quit", "exit"),
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

/// 命令架上与输入匹配的 skill（Puffin `shelf::matches`：名字以打的词开头，不分大小写）；`hidden` 的不列，与 TUI 命令或别名
/// 同名的不列（打它运行 TUI 的那个）。
pub fn matching_skills<'a>(skills: &'a [UserCommand], typed: &str) -> Vec<&'a UserCommand> {
    let typed = typed.to_lowercase();
    skills
        .iter()
        .filter(|skill| !skill.hidden && find(&skill.name).is_none())
        .filter(|skill| skill.name.to_lowercase().starts_with(typed.as_str()))
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

/// 命令架上与输入匹配的命令（Puffin `shelf::matches`）：名字以打的词开头，或打的是指向它的别名（[`SHELF_ALIASES`]）的开头；次序照表，不提前
/// （以前完全匹配与别名提前、没有前缀匹配时按子序列，tui-kit.md §6）。
pub fn matching(typed: &str) -> Vec<&'static Command> {
    let typed = typed.to_lowercase();
    COMMANDS
        .iter()
        .filter(|c| rust_cli_tui_kit::app::widgets::shelf::matches(c.name, SHELF_ALIASES, &typed))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shelf_matches_name_and_alias_prefixes_in_table_order() {
        let names = |typed: &str| matching(typed).iter().map(|c| c.name).collect::<Vec<_>>();
        assert_eq!(names("mo"), ["model", "mode"]);
        assert_eq!(names("exit"), ["exit"]);
        // 别名的开头列出它所指的命令，次序照表。
        assert_eq!(names("cl"), ["new"]);
        assert_eq!(names("st"), ["stats"]);
        assert!(names("mdl").is_empty());
        assert_eq!(
            names(""),
            COMMANDS.iter().map(|c| c.name).collect::<Vec<_>>()
        );
    }

    /// 用户决定删除 `/agents`（docs/specs/rust-cli/tui-display.md §3.2，L33）：命令表、别名与补全里都没有，
    /// 提交时走未知命令。
    #[test]
    fn agents_is_not_a_command() {
        assert!(find("agents").is_none());
        assert!(matching("agents").is_empty());
    }

    #[test]
    fn aliases_find_their_command() {
        assert_eq!(find("clear").map(|c| c.name), Some("new"));
        assert_eq!(find("reset").map(|c| c.name), Some("new"));
        assert_eq!(find("quit").map(|c| c.name), Some("exit"));
        assert_eq!(find("effort").map(|c| c.name), Some("model"));
        assert_eq!(find("status").map(|c| c.name), Some("stats"));
        assert_eq!(find("context").map(|c| c.name), Some("stats"));
        assert_eq!(
            find("new").map(|c| c.aliases),
            Some(&["clear", "reset"][..])
        );
    }

    /// 命令架上的别名都指向表里的命令，并且是那条命令自己的别名（两处不能各写一份）。
    #[test]
    fn shelf_aliases_are_aliases_of_their_command() {
        for (alias, target) in SHELF_ALIASES {
            assert_eq!(find(alias).map(|c| c.name), Some(*target));
        }
    }
}
