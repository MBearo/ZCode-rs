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

/// Node 的模式名（`/mode`，README §5 的映射；修复线 F20：guarded 是 guarded，`plan` 只打开 plan 开关）。
/// 名字表在 kernel-api（F58）；`/mode` 照 Node 不收 `auto`（Node `/mode` 只有 plan / build / edit / guarded / yolo）。
fn node_mode(name: &str) -> Option<PermissionMode> {
    PermissionMode::from_node_name(name).filter(|mode| *mode != PermissionMode::Auto)
}

fn node_name(mode: PermissionMode) -> &'static str {
    mode.node_name()
}

/// Node `formatAvailableCommandCenterModes`：菜单里不再列 edit。
const NODE_MODES: &str = "plan, build, guarded, yolo";
/// `/mode` 选择器的条目（与 [`NODE_MODES`] 同序）。
const NODE_MENU: [&str; 4] = ["plan", "build", "guarded", "yolo"];

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
    /// 直接切换（以 `ModeChanged` / `PlanChanged` 为准）。当前模式是权限模式（同 Node `getMode()`）。
    pub(super) fn mode_command(&mut self, args: &str) -> Option<Action> {
        let current = node_name(self.view.mode());
        let zh = self.lang == Lang::Zh;
        if args.is_empty() {
            let items = NODE_MENU
                .iter()
                .map(|name| Item {
                    label: (*name).to_owned(),
                    description: node_mode(name)
                        .map(|mode| super::i18n::mode_label(self.lang, mode).to_owned())
                        .unwrap_or_default(),
                    current: *name == current,
                    value: (*name).to_owned(),
                    ..Item::default()
                })
                .collect();
            let (title, subtitle) = if zh {
                ("模式", format!("当前模式：{current}。选择要切换的模式。"))
            } else {
                (
                    "Mode",
                    format!("Current mode: {current}. Choose a mode to switch to."),
                )
            };
            self.overlay = Some(Overlay::List(
                ListView::new(Purpose::Permissions, title.into(), items, String::new())
                    .with_subtitle(subtitle),
            ));
            return None;
        }
        let requested = args.to_ascii_lowercase();
        let Some(mode) = node_mode(&requested) else {
            self.info(if zh {
                format!("不支持的模式：{args}。可用模式：{NODE_MODES}。")
            } else {
                format!("Unsupported mode: {args}. Available modes: {NODE_MODES}.")
            });
            return None;
        };
        self.info(if zh {
            format!("模式已切换为 {requested}。")
        } else {
            format!("Mode switched to {requested}.")
        });
        Some(Action::SetMode(mode))
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
