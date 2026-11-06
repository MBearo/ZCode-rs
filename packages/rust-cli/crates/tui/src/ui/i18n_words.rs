// 复制自 apps/zcode-host/crates/tui/src/ui/i18n.rs（feat/rs 79b6691c55），按 400 行上限拆出；权限模式
// 的名称换成内核的模式（docs/specs/rust-cli/tui.md §7）。
//! 带参数的界面文字。
use rust_cli_kernel_api::compaction::ContextNotice;
use rust_cli_kernel_api::permission::PermissionMode;

use super::Lang;

/// 权限模式的显示名称。变更前确认、自主模式、完全访问、自动编辑、计划模式是 App 的 `mode.label.glm.*` 原文（工作线
/// F76 核对过）。
pub fn mode_label(lang: Lang, mode: PermissionMode) -> &'static str {
    match (mode, lang) {
        (PermissionMode::Default, Lang::En) => "Ask before changes",
        (PermissionMode::Default, Lang::Zh) => "变更前确认",
        (PermissionMode::AcceptEdits, Lang::En) => "Edit automatically",
        (PermissionMode::AcceptEdits, Lang::Zh) => "自动编辑",
        (PermissionMode::Plan, Lang::En) => "Plan mode",
        (PermissionMode::Plan, Lang::Zh) => "计划模式",
        // guarded 照 Puffin 是 `Autonomous`（kit `modes::label`；修复线 F20 起是 App 的 `Autonomous mode`）。
        (PermissionMode::Guarded, Lang::En) => "Autonomous",
        (PermissionMode::Guarded, Lang::Zh) => "自主模式",
        (PermissionMode::Auto, Lang::En) => "Auto mode",
        (PermissionMode::Auto, Lang::Zh) => "自动模式",
        (PermissionMode::DontAsk, Lang::En) => "Don't ask",
        (PermissionMode::DontAsk, Lang::Zh) => "不询问",
        (PermissionMode::BypassPermissions, Lang::En) => "Full access",
        (PermissionMode::BypassPermissions, Lang::Zh) => "完全访问",
    }
}

/// `/mode` 列表里模式的说明（Puffin，kit `modes::MODE_ROWS`；以前是 App 的 `mode.description.glm.*`）。三个模式与计划模式
/// （`Plan`）有。
pub fn mode_description(lang: Lang, mode: PermissionMode) -> &'static str {
    match (mode, lang) {
        (PermissionMode::Default, Lang::En) => "Asks before each file change and command",
        (PermissionMode::Default, Lang::Zh) => "每次改文件、跑命令前都先问",
        (PermissionMode::Guarded, Lang::En) => "Asks only before commands that can destroy work",
        (PermissionMode::Guarded, Lang::Zh) => "只在可能毁掉工作的命令前问",
        (PermissionMode::BypassPermissions, Lang::En) => {
            "Runs commands and edits files without asking"
        }
        (PermissionMode::BypassPermissions, Lang::Zh) => "跑命令、改文件都不问",
        (PermissionMode::Plan, Lang::En) => "Reads and plans; changes nothing until approved",
        (PermissionMode::Plan, Lang::Zh) => "只读与计划；批准之前什么都不改",
        _ => "",
    }
}

/// 模式变了时输入框上方安静的一句（Puffin `mode_says`，kit `modes::says`）；以前是 `Mode switched to …`。
pub fn mode_says(lang: Lang, mode: PermissionMode) -> &'static str {
    match (mode, lang) {
        (PermissionMode::Guarded, Lang::En) => {
            "Autonomous: it asks only before commands that can destroy work."
        }
        (PermissionMode::Guarded, Lang::Zh) => "自主：只在可能毁掉工作的命令前问。",
        (PermissionMode::BypassPermissions, Lang::En) => {
            "Full access: it runs commands and edits files without asking."
        }
        (PermissionMode::BypassPermissions, Lang::Zh) => "完全访问：跑命令、改文件都不问。",
        (PermissionMode::AcceptEdits, Lang::En) => {
            "Edit automatically: it edits files without asking, and asks before commands."
        }
        (PermissionMode::AcceptEdits, Lang::Zh) => "自动编辑：改文件不问，跑命令前问。",
        (PermissionMode::Plan, Lang::En) => {
            "Plan mode: it reads and plans, and changes nothing until you approve."
        }
        (PermissionMode::Plan, Lang::Zh) => "计划模式：只读与计划，你批准之前什么都不改。",
        (_, Lang::En) => "Ask before changes: it asks before each file change and each command.",
        (_, Lang::Zh) => "变更前确认：每次改文件、跑命令前都先问。",
    }
}

/// 计划模式关上时的一句（Puffin `PLAN_OFF_SAYS`）。
pub fn plan_off_says(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Plan mode is off.",
        Lang::Zh => "计划模式已关闭。",
    }
}

/// footer 右侧“上下文快满”的提示（四种文字，compact.md §2.1）；不显示时为 None。
pub fn context_notice(lang: Lang, notice: ContextNotice) -> Option<String> {
    Some(match (notice, lang) {
        (ContextNotice::Hidden, _) => return None,
        (ContextNotice::Used { percent }, Lang::En) => format!("{percent}% context used"),
        (ContextNotice::Used { percent }, Lang::Zh) => format!("上下文已用 {percent}%"),
        (ContextNotice::UntilAutoCompact { percent }, Lang::En) => {
            format!("{percent}% until auto-compact")
        }
        (ContextNotice::UntilAutoCompact { percent }, Lang::Zh) => {
            format!("距自动压缩还剩 {percent}%")
        }
        (
            ContextNotice::Low {
                percent,
                compact_hint,
            },
            Lang::En,
        ) => {
            let hint = if compact_hint {
                " \u{b7} Run /compact to compact & continue"
            } else {
                ""
            };
            format!("Context low ({percent}% remaining){hint}")
        }
        (
            ContextNotice::Low {
                percent,
                compact_hint,
            },
            Lang::Zh,
        ) => {
            let hint = if compact_hint {
                " \u{b7} 运行 /compact 压缩后继续"
            } else {
                ""
            };
            format!("上下文不足（剩余 {percent}%）{hint}")
        }
    })
}

pub fn unrecognized(lang: Lang, name: &str) -> String {
    match lang {
        Lang::Zh => format!("没有命令 '/{name}'。输入 \"/\" 查看全部命令。"),
        Lang::En => {
            format!("Unrecognized command '/{name}'. Type \"/\" for a list of supported commands.")
        }
    }
}

/// 依赖其他工作线、还没实现的命令（docs/specs/rust-cli/tui.md §6）。
pub fn not_implemented(lang: Lang, name: &str) -> String {
    match lang {
        Lang::Zh => format!("/{name} 还没有实现。"),
        Lang::En => format!("/{name} is not implemented yet."),
    }
}

/// 切换会话期间（旧会话已交出、新会话还没打开）的会话命令（titles.md §4）。
pub fn session_opening(lang: Lang) -> &'static str {
    match lang {
        Lang::Zh => "会话还在打开，请稍后再试。",
        Lang::En => "The session is still opening. Try again in a moment.",
    }
}

/// 模型不在目录里（`/model 名称`）。
pub fn unknown_model(lang: Lang, name: &str) -> String {
    match lang {
        Lang::Zh => format!("没有配置模型 {name}。不带参数输入 /model 查看可用的模型。"),
        Lang::En => {
            format!("Model {name} is not configured. Run /model to see the available models.")
        }
    }
}

/// `/model 名称` 说不清是哪个团队项目（docs/specs/rust-cli/cli-args.md §1.3，工作线 F49）：要写 `[<团队>]`。
pub fn ambiguous_model(lang: Lang, error: &rust_cli_kernel_api::model_name::LookupError) -> String {
    use rust_cli_kernel_api::model_name::LookupError;
    match (lang, error) {
        (Lang::Zh, LookupError::SeveralTeams { name, choices }) => {
            format!(
                "模型 {name} 对应多个团队套餐，请指定其中一个：{}",
                choices.join("、")
            )
        }
        (
            Lang::Zh,
            LookupError::SeveralProjects {
                team,
                model,
                choices,
            },
        ) => format!(
            "团队 {team} 在 {model} 下有多个团队项目，请指定其中一个：{}",
            choices.join("、")
        ),
        (_, error) => error.to_string(),
    }
}

/// 模型列表里团队套餐的供应商名（docs/specs/rust-cli/tui.md `/model`，工作线 F49）：`<供应商> Team Plan · <团队>`，
/// “Team Plan / 团队套餐”取 App i18n 的 `settings.modelProvider.connectionMode.teamPlan`。
pub fn team_plan(lang: Lang, vendor: &str, team: &str) -> String {
    match lang {
        Lang::Zh => format!("{vendor} 团队套餐 · {team}"),
        Lang::En => format!("{vendor} Team Plan · {team}"),
    }
}

pub fn model_set(lang: Lang, model: &str) -> String {
    match lang {
        Lang::Zh => format!("模型已设为 {model}"),
        Lang::En => format!("Set model to {model}"),
    }
}

/// 切换已生效、默认模型没写成（Node `rememberCurrentModelSelection` 的警告，workspace-trust.md §3.2）。
pub fn default_model_unsaved(lang: Lang, reason: &str) -> String {
    match lang {
        Lang::Zh => format!("警告：当前选择已生效，但没能保存为新会话的默认模型：{reason}"),
        Lang::En => format!(
            "Warning: The current selection is active, but could not be saved as the default for new sessions: {reason}"
        ),
    }
}

/// 命令被内核拒绝。
pub fn rejected(lang: Lang, reason: &str) -> String {
    match lang {
        Lang::Zh => format!("没有执行：{reason}"),
        Lang::En => format!("Not done: {reason}"),
    }
}
