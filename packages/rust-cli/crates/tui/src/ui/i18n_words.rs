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
        // 修复线 F20：guarded 用 App 的名称（`mode.label.glm.guarded`），auto 改用固定的名称（原来是 Autonomous mode）。
        (PermissionMode::Guarded, Lang::En) => "Autonomous mode",
        (PermissionMode::Guarded, Lang::Zh) => "自主模式",
        (PermissionMode::Auto, Lang::En) => "Auto mode",
        (PermissionMode::Auto, Lang::Zh) => "自动模式",
        (PermissionMode::DontAsk, Lang::En) => "Don't ask",
        (PermissionMode::DontAsk, Lang::Zh) => "不询问",
        (PermissionMode::BypassPermissions, Lang::En) => "Full access",
        (PermissionMode::BypassPermissions, Lang::Zh) => "完全访问",
    }
}

/// `/mode` 选择器里模式的说明（工作线 F76）：App 的 `mode.description.glm.{build,guarded,yolo}` 原文
/// （`packages/ui/src/i18n/locales/en-US.ts` / `zh-CN.ts`；自主模式那句没有句点，照原文）。只有选择器里的三个模式有。
pub fn mode_description(lang: Lang, mode: PermissionMode) -> &'static str {
    match (mode, lang) {
        (PermissionMode::Default, Lang::En) => "Ask before file changes.",
        (PermissionMode::Default, Lang::Zh) => "改文件前先问我。",
        (PermissionMode::Guarded, Lang::En) => "Ask when there’s risk",
        (PermissionMode::Guarded, Lang::Zh) => "在有风险时询问",
        (PermissionMode::BypassPermissions, Lang::En) => "Run with fewer confirmations.",
        (PermissionMode::BypassPermissions, Lang::Zh) => "减少确认次数。",
        _ => "",
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
pub fn ambiguous_model(lang: Lang, error: &rust_cli_platform::model_name::LookupError) -> String {
    use rust_cli_platform::model_name::LookupError;
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

/// 回到底部控件的文字，从长到短；调用方取第一个放得下的。
pub fn follow_labels(lang: Lang, unseen: bool) -> [&'static str; 5] {
    match (lang, unseen) {
        (Lang::En, false) => [
            " ↓ Back to bottom · esc ",
            " ↓ Back to bottom ",
            " ↓ Bottom · esc ",
            " ↓ Bottom ",
            " ↓ ",
        ],
        (Lang::En, true) => [
            " New activity · ↓ Back to bottom · esc ",
            " New activity · ↓ Bottom ",
            " New · ↓ Bottom ",
            " ↓ Bottom ",
            " ↓ ",
        ],
        (Lang::Zh, false) => [
            " ↓ 回到底部 · esc ",
            " ↓ 回到底部 ",
            " ↓ 底部 · esc ",
            " ↓ 底部 ",
            " ↓ ",
        ],
        (Lang::Zh, true) => [
            " 有新内容 · ↓ 回到底部 · esc ",
            " 有新内容 · ↓ 底部 ",
            " 新 · ↓ 底部 ",
            " ↓ 底部 ",
            " ↓ ",
        ],
    }
}

/// 不补零的用时：`11s`、`1m 5s`、`1h 2m 3s`。
pub fn elapsed_plain(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    match (h, m) {
        (0, 0) => format!("{s}s"),
        (0, _) => format!("{m}m {s}s"),
        _ => format!("{h}h {m}m {s}s"),
    }
}

/// `0s`、`1m 05s`、`1h 02m 03s`。
pub fn elapsed(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    match (h, m) {
        (0, 0) => format!("{s}s"),
        (0, _) => format!("{m}m {s:02}s"),
        _ => format!("{h}h {m:02}m {s:02}s"),
    }
}

/// 子代理发起的询问：对话框标题后的来源（docs/specs/rust-cli/agents.md §4.1.1）。名字是 agent 类型；为空时只说是子代理。
pub fn asked_by(lang: Lang, name: &str) -> String {
    match (lang, name.trim()) {
        (Lang::En, "") => "from a subagent".to_owned(),
        (Lang::Zh, "") => "来自子代理".to_owned(),
        (Lang::En, name) => format!("from the {name} agent"),
        (Lang::Zh, name) => format!("来自 {name} 子代理"),
    }
}

/// 已关闭的会话里后台子代理的询问（docs/specs/rust-cli/agents.md §4.1.2，工作线 F72）：另标出会话的标题。
pub fn asked_by_session(lang: Lang, name: &str, session: &str) -> String {
    let agent = match (lang, name.trim()) {
        (Lang::En, "") => "a subagent".to_owned(),
        (Lang::En, name) => format!("the {name} agent"),
        (Lang::Zh, "") => "子代理".to_owned(),
        (Lang::Zh, name) => format!("{name} 子代理"),
    };
    match (lang, session.trim()) {
        (Lang::En, "") => format!("from {agent} in an earlier session"),
        (Lang::En, title) => format!("from {agent} in session \"{title}\""),
        (Lang::Zh, "") => format!("来自之前的会话的 {agent}"),
        (Lang::Zh, title) => format!("来自会话「{title}」的 {agent}"),
    }
}
