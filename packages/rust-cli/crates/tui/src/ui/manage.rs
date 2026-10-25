//! 管理命令的界面部分（docs/specs/rust-cli/tui-display.md §2.3、§3、§4）：打开页面与列表、生成提示文字。
//! 数据来自内核（runner 取回后交给这里）；这里不保存业务状态，只有页面与列表的界面状态。
use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::manage::{BackgroundEntry, BackgroundKind, McpServerInfo, McpServerState};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::provider::CatalogModel;

use super::app::{Action, App, Local, Overlay};
use super::i18n::Lang;

/// 需要 runner 去问内核的管理命令。
#[derive(Debug, Clone, PartialEq)]
pub enum Manage {
    /// `/tasks`：打开 Background 面板（与 footer 的 pill 同一个，tui-panels.md §3.1）。
    Tasks,
    /// 在 Background 面板或子代理列表里按 `x`：停止这一项（`Command::StopTask`）。
    StopTask { id: String, label: String },
    /// `/effort 参数`：取模型目录里当前模型的档位后执行。
    Effort(String),
    /// `/mcp`：取服务器状态后打开页面。
    Mcp,
    /// `/clear 名字`：先给旧会话改名，再离开。
    ClearNamed(String),
    /// `/plan 描述`：切到 plan 后发出这条输入。
    PlanPrompt(String),
    /// `/status`：取 MCP 状态后打开状态页。
    Status,
    /// `/usage`（`/cost`、`/stats`）：取会话的 API 时长与全时段的统计后打开页面（docs/specs/rust-cli/usage.md
    /// §4、§5）；`stats` 是 `/stats`，开 Stats 标签页。
    Usage { stats: bool },
    /// Stats 标签页按 `r` 换到这个范围：`seq` 是页面的请求序号，回执按它丢旧。
    UsageRange {
        range: rust_cli_kernel_api::metering::ActivityRange,
        seq: u64,
    },
    /// `/theme` 换了调色板：清掉排版缓存并重画。
    Repaint,
    /// `/hooks`：取 hook 配置后打开只读页面。
    Hooks,
    /// `/permissions`：取规则列表后打开页面（tui-manage.md §1）。
    Permissions,
    /// `/permissions` 里的一次修改。
    ChangeRule(rust_cli_kernel_api::permission::RuleChange),
    /// `/plan open`（tui-manage.md §8）。
    PlanOpen,
    /// `/cd <path>`（tui-manage.md §7）：参数原文。
    ChangeDirectory(String),
    /// `/mcp reconnect|enable|disable …`（tui-manage.md §6）：参数原文。
    McpControl(String),
    /// `/diff`：页面已打开，现取未提交的改动（tui-manage.md §4）。
    Diff,
    /// `/export <文件名>`：渲染会话文字后经内核写文件（tui-manage.md §3）。
    Export(String),
    /// `/export` 选了复制到剪贴板。
    ExportCopy,
    /// `/export` 选了写文件：入口按注入的时钟拼缺省文件名，再打开文件名输入框（F7）。
    ExportName,
    /// `/theme` 选中：先写回 `ui.theme`，成功后换调色板（tui-manage.md §2.1）。
    SetTheme(crate::services::UiTheme),
    /// `/effort` 换档位：先 `SetModel`，再存成新会话的缺省（tui-manage.md §2.2）。`level` 为 None 是 auto。
    EffortChosen {
        selection: rust_cli_kernel_api::model::ModelSelection,
        level: Option<String>,
    },
}

/// 还在跑的后台任务数（footer 的 pill 只数这些）。
pub fn running(entries: &[BackgroundEntry]) -> usize {
    entries
        .iter()
        .filter(|entry| entry.status == BackgroundStatus::Running)
        .count()
}

/// 在跑的子代理里有缺省机器在远端的（F98，remote-exec.md §25.4.5）：这时不写“本地”。
pub fn remote_agents(entries: &[BackgroundEntry]) -> bool {
    entries.iter().any(|entry| {
        entry.kind == BackgroundKind::Agent
            && entry.status == BackgroundStatus::Running
            && entry.machine.is_some()
    })
}

/// footer 的 pill：都是命令 `N shell(s)`，都是子代理 `N local agent(s)`，混合 `N background task(s)`。
///
/// 缺陷修复（Docker 试用 L9，F98）：缺省机器在 dev-a 的子代理也写成“本地代理”，与子代理列表的 `@dev-a` 冲突；有这样的
/// 子代理时写 `N agent(s)` / `N 个代理`。
pub fn pill(entries: &[BackgroundEntry], lang: Lang) -> Option<String> {
    let live: Vec<&BackgroundEntry> = entries
        .iter()
        .filter(|entry| entry.status == BackgroundStatus::Running)
        .collect();
    let first = live.first()?;
    let n = live.len();
    let same = live.iter().all(|entry| entry.kind == first.kind);
    if same && first.kind == BackgroundKind::Agent && remote_agents(entries) {
        return Some(match (lang, n) {
            (Lang::En, 1) => "1 agent".to_owned(),
            (Lang::En, n) => format!("{n} agents"),
            (Lang::Zh, n) => format!("{n} 个代理"),
        });
    }
    Some(match (lang, same.then_some(first.kind)) {
        (Lang::En, Some(BackgroundKind::Shell)) if n == 1 => "1 shell".to_owned(),
        (Lang::En, Some(BackgroundKind::Shell)) => format!("{n} shells"),
        (Lang::En, Some(BackgroundKind::Agent)) if n == 1 => "1 local agent".to_owned(),
        (Lang::En, Some(BackgroundKind::Agent)) => format!("{n} local agents"),
        (Lang::En, None) if n == 1 => "1 background task".to_owned(),
        (Lang::En, None) => format!("{n} background tasks"),
        (Lang::Zh, Some(BackgroundKind::Shell)) => format!("{n} 个 shell"),
        (Lang::Zh, Some(BackgroundKind::Agent)) => format!("{n} 个本地代理"),
        (Lang::Zh, None) => format!("{n} 个后台任务"),
    })
}

/// 档位的说明；其他档位没有。
fn effort_description(level: &str) -> Option<&'static str> {
    Some(match level {
        "low" => "Quick, straightforward implementation with minimal overhead",
        "medium" => "Balanced approach with standard implementation and testing",
        "high" => "Comprehensive implementation with extensive testing and documentation",
        "xhigh" => "Deeper reasoning than high, just below maximum (on supported models)",
        "max" => "Maximum capability with deepest reasoning.",
        _ => return None,
    })
}

fn with_description(level: &str) -> String {
    match effort_description(level) {
        Some(text) => format!("{level} ({text})"),
        None => level.to_owned(),
    }
}

impl App {
    /// `/effort` 的执行（模型目录到达后）：查看、换档位、auto（tui-display.md §4.4）。
    pub fn effort(&mut self, models: &[CatalogModel], args: &str) -> Option<Action> {
        let current = self.view.selection().clone();
        let levels: Vec<String> = models
            .iter()
            .find(|m| m.provider == current.provider && m.model == current.model)
            .map(|m| m.reasoning_levels.clone())
            .unwrap_or_default();
        let args = args.trim();
        // 不带参数：档位的选择器（修复线 F26，用户决定：与 `/model` 同一种列表，当前值打 ✔，不用横向的滑块）。
        // 模型没有档位时没有可选的，照旧只说当前档位。
        if args.is_empty() && !levels.is_empty() {
            self.effort_list(&levels, current.reasoning.as_deref());
            return None;
        }
        if args.is_empty() || args == "current" || args == "status" {
            let text = match &current.reasoning {
                Some(level) => format!("Current effort level: {}", with_description(level)),
                None => "Effort level: auto".to_owned(),
            };
            self.info(text);
            return None;
        }
        let selection = |reasoning: Option<String>| ModelSelection {
            reasoning,
            ..current.clone()
        };
        // 换档位：runner 先 `SetModel`，再存成新会话的缺省，按结果提示（tui-manage.md §2.2）。
        if args == "auto" {
            return Some(Action::Manage(Manage::EffortChosen {
                selection: selection(None),
                level: None,
            }));
        }
        if let Some(level) = levels.iter().find(|l| l.eq_ignore_ascii_case(args)) {
            return Some(Action::Manage(Manage::EffortChosen {
                selection: selection(Some(level.clone())),
                level: Some(level.clone()),
            }));
        }
        let mut options = levels.clone();
        options.push("auto".to_owned());
        let mut usage = format!("Usage: /effort [{}]", options.join("|"));
        if !levels.is_empty() {
            usage.push_str("\n\nEffort levels:");
            for level in &levels {
                match effort_description(level) {
                    Some(text) => usage.push_str(&format!("\n- {level}: {text}")),
                    None => usage.push_str(&format!("\n- {level}")),
                }
            }
        }
        self.info(usage);
        None
    }

    /// `/effort` 的选择器：模型的档位（从低到高）与 `auto`，说明用档位说明的文字。
    fn effort_list(&mut self, levels: &[String], current: Option<&str>) {
        let zh = self.lang == Lang::Zh;
        let mut items: Vec<super::list::Item> = levels
            .iter()
            .map(|level| super::list::Item {
                label: level.clone(),
                description: effort_description(level).unwrap_or_default().to_owned(),
                current: current == Some(level.as_str()),
                value: level.clone(),
                ..super::list::Item::default()
            })
            .collect();
        items.push(super::list::Item {
            label: "auto".to_owned(),
            // 缺省档位的说明。
            description: if zh {
                "这个模型的缺省档位"
            } else {
                "The default effort for this model"
            }
            .to_owned(),
            current: current.is_none(),
            value: "auto".to_owned(),
            ..super::list::Item::default()
        });
        let (title, subtitle) = if zh {
            ("推理档位", "设置模型的推理档位")
        } else {
            ("Effort", "Set effort level for model usage")
        };
        self.overlay = Some(Overlay::List(
            super::list::ListView::new(
                super::list::Purpose::Effort,
                title.into(),
                items,
                String::new(),
            )
            .with_subtitle(subtitle.into()),
        ));
    }

    /// `/effort` 的提示：`saved` 为 None 时没有存储（只对本会话有效），否则是保存的结果。
    pub fn effort_done(&mut self, level: Option<&str>, saved: Option<Result<String, String>>) {
        let suffix = match &saved {
            None => " (this session only)",
            Some(Ok(_)) => " (saved as your default for new sessions)",
            Some(Err(reason)) => {
                self.error(format!("Failed to set effort level: {reason}"));
                return;
            }
        };
        let text = match level {
            None if saved.is_some() => "Effort level set to auto".to_owned(),
            None => format!("Effort level set to auto{suffix}"),
            Some(level) => match effort_description(level) {
                Some(d) => format!("Set effort level to {level}{suffix}: {d}"),
                None => format!("Set effort level to {level}{suffix}"),
            },
        };
        self.info(text);
    }

    /// `/mcp`：服务器与状态（tui-display.md §3.4）。
    pub fn open_mcp(&mut self, servers: &[McpServerInfo]) {
        let lang = self.lang;
        let rows = servers
            .iter()
            .map(|server| {
                // 状态词与 `/mcp` 的子命令同一张表（tui-manage.md §6）：中文界面用中文（远端执行 F98，remote-exec.md
                // §25.4.4，Docker 试用 L7：以前这一页写死英文）。
                let state = super::mcp_control::state_word(server.state, lang);
                let extra = match (server.state, &server.reason) {
                    (McpServerState::Failed, Some(reason)) => {
                        super::i18n_machines::mcp_reason(lang, reason)
                    }
                    (McpServerState::Connected, _) => match lang {
                        Lang::En if server.tools == 1 => "1 tool".to_owned(),
                        Lang::En => format!("{} tools", server.tools),
                        Lang::Zh => format!("{} 个工具", server.tools),
                    },
                    _ => String::new(),
                };
                // 绑定到远端机器的服务器名后面接 ` @机器`（R5，remote-exec.md §22.8；表格的单元格不分颜色）。
                let name = match &server.machine {
                    Some(machine) => format!("{} @{machine}", server.name),
                    None => server.name.clone(),
                };
                (name, state.to_owned(), extra)
            })
            .collect();
        let (title, empty) = match lang {
            Lang::En => ("MCP servers", "No MCP servers are configured."),
            Lang::Zh => ("MCP 服务器", "没有配置 MCP 服务器。"),
        };
        self.overlay = Some(Overlay::Page {
            local: Local::Table {
                title: title.into(),
                rows,
                empty: empty.into(),
            },
            scroll: 0,
        });
    }
}

impl App {
    /// `/hooks`：只读页面，每条 hook 一行（事件与 matcher、显示文本、来源与类型，tui-display.md §3.5）。
    pub fn open_hooks(&mut self, entries: &[rust_cli_kernel_api::manage::HookEntry]) {
        let rows = entries
            .iter()
            .map(|entry| {
                let name = match &entry.matcher {
                    Some(matcher) if !matcher.is_empty() => format!("{} ({matcher})", entry.event),
                    _ => entry.event.clone(),
                };
                (
                    name,
                    entry.display.clone(),
                    format!("{} · {}", entry.source, entry.kind),
                )
            })
            .collect();
        let (title, empty) = match self.lang {
            Lang::En => ("Hooks", "No hooks configured"),
            Lang::Zh => ("Hooks", "没有配置 hook"),
        };
        self.overlay = Some(Overlay::Page {
            local: Local::Table {
                title: title.into(),
                rows,
                empty: empty.into(),
            },
            scroll: 0,
        });
    }
}

/// `/plan open` 没有计划时：刚切到 plan 是 `Enabled plan mode`，已经在 plan 是 “还没写计划”。
pub(crate) fn no_plan(lang: Lang, entering: bool) -> &'static str {
    match (lang, entering) {
        (Lang::En, true) => "Enabled plan mode",
        (Lang::En, false) => "Already in plan mode. No plan written yet.",
        (Lang::Zh, true) => "已打开计划模式",
        (Lang::Zh, false) => "已经在计划模式，还没有写计划。",
    }
}

/// `/status` 的 MCP 一行：`N connected, M failed`，没有服务器时 `none`。
pub fn mcp_summary(servers: &[McpServerInfo], lang: Lang) -> String {
    if servers.is_empty() {
        return match lang {
            Lang::En => "none".to_owned(),
            Lang::Zh => "无".to_owned(),
        };
    }
    let count = |state: McpServerState| servers.iter().filter(|s| s.state == state).count();
    let (connected, pending, failed) = (
        count(McpServerState::Connected),
        count(McpServerState::Pending),
        count(McpServerState::Failed),
    );
    let mut parts = match lang {
        Lang::En => vec![format!("{connected} connected")],
        Lang::Zh => vec![format!("{connected} 个已连接")],
    };
    if pending > 0 {
        parts.push(match lang {
            Lang::En => format!("{pending} connecting"),
            Lang::Zh => format!("{pending} 个连接中"),
        });
    }
    if failed > 0 {
        parts.push(match lang {
            Lang::En => format!("{failed} failed"),
            Lang::Zh => format!("{failed} 个失败"),
        });
    }
    let disabled = count(McpServerState::Disabled);
    if disabled > 0 {
        parts.push(match lang {
            Lang::En => format!("{disabled} disabled"),
            Lang::Zh => format!("{disabled} 个已停用"),
        });
    }
    parts.join(", ")
}

#[cfg(test)]
#[path = "manage_tests.rs"]
mod tests;
