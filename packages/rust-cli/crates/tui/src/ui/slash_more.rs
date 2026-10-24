//! L28 的斜杠命令（docs/specs/rust-cli/tui-display.md §3、§4）：解析参数，本地能完成的直接完成（`/skills`、
//! `/mode`、`/copy N`、`/theme`），要问内核的交给 runner（[`Manage`]；`/usage` 见 `usage.rs`）。`/agents` 已按用户
//! 决定删除（§3.2，L33），走未知命令。
use rust_cli_kernel_api::permission::PermissionMode;

use super::app::{Action, App, Local, Overlay};
use super::i18n::Lang;
use super::list::{Item, ListView, Purpose};
use super::manage::Manage;

/// 整行是这些词时退出（不在 bash 模式，tui-display.md §4.1）。
const EXIT_WORDS: [&str; 6] = ["exit", "quit", ":q", ":q!", ":wq", ":wq!"];

pub fn is_exit_word(text: &str) -> bool {
    EXIT_WORDS.contains(&text.trim())
}

/// `/mode` 收的 Node 模式名（README §5 的映射）。工作线 F76（用户决定 2026-10-23）：照 App 只有 build / guarded / yolo
/// 三个（以前还收 edit；`auto` 一直不收）；`plan` 照旧打开 plan 开关（不列在选项里，同 `/plan`）。
fn node_mode(name: &str) -> Option<PermissionMode> {
    match name {
        "build" | "guarded" | "yolo" | "plan" => PermissionMode::from_node_name(name),
        _ => None,
    }
}

/// 可用模式的说法（“不支持的模式”提示与命令表的参数提示同一组）。
const NODE_MODES: &str = "build, guarded, yolo";
/// `/mode` 选择器的三个模式，照 App 菜单的顺序（`packages/shared/src/zcode-agent-model-state.ts` 去掉 plan）。
const MENU: [PermissionMode; 3] = [
    PermissionMode::Default,
    PermissionMode::Guarded,
    PermissionMode::BypassPermissions,
];

impl App {
    /// L28 的命令；不是 L28 的命令返回 None（由 `slash` 继续处理）。
    pub(super) fn slash_more(&mut self, name: &str, args: &str) -> Option<Option<Action>> {
        Some(match name {
            "skills" => self.skills_page(),
            "mode" => self.mode_command(args),
            "effort" => Some(Action::Manage(Manage::Effort(args.to_owned()))),
            "copy" => self.copy_nth(args),
            "theme" => self.theme_list(),
            "tasks" => Some(Action::Manage(Manage::Tasks)),
            "hooks" => Some(Action::Manage(Manage::Hooks)),
            "permissions" => Some(Action::Manage(Manage::Permissions)),
            "export" => self.export_command(args),
            "diff" => self.open_diff(),
            // `/cd <path>`（tui-manage.md §7）：内核检查路径并经会话 actor 换目录。
            "cd" if args.is_empty() => {
                self.info(match self.lang {
                    Lang::En => "Usage: /cd <path>",
                    Lang::Zh => "用法：/cd <路径>",
                });
                None
            }
            "cd" => Some(Action::Manage(Manage::ChangeDirectory(args.to_owned()))),
            "mcp" if args.is_empty() => Some(Action::Manage(Manage::Mcp)),
            // reconnect / enable / disable（tui-manage.md §6）：先取服务器快照再判断。
            "mcp" => Some(Action::Manage(Manage::McpControl(args.to_owned()))),
            "clear" if !args.is_empty() && self.session.is_some() => {
                Some(Action::Manage(Manage::ClearNamed(args.to_owned())))
            }
            // 在外部编辑器打开计划文件（tui-manage.md §8；编辑器是 L27 的端口）。
            "plan" if args == "open" => Some(Action::Manage(Manage::PlanOpen)),
            "plan" if !args.is_empty() => Some(Action::Manage(Manage::PlanPrompt(args.to_owned()))),
            "status" => Some(Action::Manage(Manage::Status)),
            // 远端机器（remote-exec.md §21.3）。
            "machines" => self.machines_command(args),
            _ => return None,
        })
    }

    fn skills_page(&mut self) -> Option<Action> {
        let lang = self.lang;
        let model_only = match lang {
            Lang::En => "(model only)",
            Lang::Zh => "（仅模型可调用）",
        };
        let rows = self
            .composer
            .skills
            .iter()
            .map(|skill| {
                let extra = if skill.hidden {
                    model_only.to_owned()
                } else {
                    String::new()
                };
                (format!("/{}", skill.name), skill.description.clone(), extra)
            })
            .collect();
        let (title, empty) = match lang {
            Lang::En => ("Skills", "No skills found"),
            Lang::Zh => ("技能", "没有找到技能"),
        };
        self.overlay = Some(Overlay::Page {
            local: Local::Table {
                title: title.into(),
                rows,
                empty: empty.into(),
            },
            scroll: 0,
        });
        None
    }

    /// Node `/mode`：不带参数时打开选择器（修复线 F26，用户决定：与 `/model` 同一种列表，当前值打 ✔），带参数时
    /// 直接切换（以 `ModeChanged` / `PlanChanged` 为准）。工作线 F76：选择器照 App 的菜单只列三个模式，名称与说明是
    /// App 的原文（名称一行、说明在下一行）；提示里的模式名也用 App 的名称。Node 写的 edit 会话不在选择器里，没有打勾。
    pub(super) fn mode_command(&mut self, args: &str) -> Option<Action> {
        let zh = self.lang == Lang::Zh;
        if args.is_empty() {
            let current = self.view.mode();
            let items = MENU
                .iter()
                .map(|mode| Item {
                    label: super::i18n::mode_label(self.lang, *mode).to_owned(),
                    description: super::i18n::mode_description(self.lang, *mode).to_owned(),
                    current: *mode == current,
                    value: mode.node_name().to_owned(),
                    ..Item::default()
                })
                .collect();
            // 副标题写当前模式（App 的名称；Node 写的 edit 会话不在选项里，靠这一行看出来）。
            let label = super::i18n::mode_label(self.lang, current);
            let (title, subtitle) = if zh {
                ("模式", format!("当前模式：{label}。选择要切换的模式。"))
            } else {
                (
                    "Mode",
                    format!("Current mode: {label}. Choose a mode to switch to."),
                )
            };
            self.overlay = Some(Overlay::List(
                ListView::new(Purpose::Permissions, title.into(), items, String::new())
                    .with_subtitle(subtitle),
            ));
            return None;
        }
        let Some(mode) = node_mode(&args.to_ascii_lowercase()) else {
            self.info(if zh {
                format!("不支持的模式：{args}。可用模式：{NODE_MODES}。")
            } else {
                format!("Unsupported mode: {args}. Available modes: {NODE_MODES}.")
            });
            return None;
        };
        // 切到完全访问先弹确认框，确认后才提示与切换（tui.md §7.8，工作线 F87）。
        self.switch_mode(mode, true)
    }

    /// `/mode` 切换时的提示（模式名是 App 的名称）。
    pub(super) fn mode_switched(&mut self, mode: PermissionMode) {
        let label = super::i18n::mode_label(self.lang, mode);
        self.info(if self.lang == Lang::Zh {
            format!("模式已切换为 {label}。")
        } else {
            format!("Mode switched to {label}.")
        });
    }

    /// `/copy [N]`：第 N 新的回复（1 是最后一条）。
    fn copy_nth(&mut self, args: &str) -> Option<Action> {
        let zh = self.lang == Lang::Zh;
        let n = if args.is_empty() {
            1
        } else {
            match args.parse::<usize>() {
                Ok(n) if n > 0 => n,
                _ => {
                    self.info("Usage: /copy [N]");
                    return None;
                }
            }
        };
        if let Some(reply) = self.view.nth_reply(n) {
            return Some(Action::Copy(reply.to_owned()));
        }
        let count = self.view.reply_count();
        if count == 0 {
            self.info(self.strings.nothing_to_copy);
        } else {
            self.info(if zh {
                format!("只有 {count} 条回复")
            } else {
                format!("Only {count} responses available")
            });
        }
        None
    }
}

#[cfg(test)]
#[path = "slash_more_tests.rs"]
mod tests;
