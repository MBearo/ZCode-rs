//! L28 的斜杠命令（docs/specs/rust-cli/tui-display.md §3、§4）：解析参数，本地能完成的直接完成（`/skills`、
//! `/mode`、`/copy N`、`/theme`），要问内核的交给 runner（[`Manage`]；`/usage` 见 `usage.rs`）。`/agents` 已按用户
//! 决定删除（§3.2，L33），走未知命令。
use rust_cli_kernel_api::permission::PermissionMode;

use super::app::{Action, App};
use super::i18n::Lang;
use super::list::{Choice, Item, ListView, Purpose};
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
    /// `/cd <path>`（tui-manage.md §7）：内核检查路径并经会话 actor 换目录；不带路径时提示用法。
    pub(super) fn cd_command(&mut self, args: &str) -> Option<Action> {
        if !args.is_empty() {
            return Some(Action::Manage(Manage::ChangeDirectory(args.to_owned())));
        }
        self.info(match self.lang {
            Lang::En => "Usage: /cd <path>",
            Lang::Zh => "用法：/cd <路径>",
        });
        // 对照实现记这一句（缺省显示，加固定说明；修复线 L-A，tui-manage.md §11.2）。
        self.records
            .push(rust_cli_kernel_api::user_command::LocalCommandRecord {
                name: "cd".to_owned(),
                args: String::new(),
                output: rust_cli_kernel_api::user_command::LocalCommandOutput::Stdout(
                    "Usage: /cd <path>".to_owned(),
                ),
                caveat: true,
                meta: Vec::new(),
            });
        None
    }

    /// Node `/mode`：不带参数时打开列表，带参数时直接切换（以 `ModeChanged` / `PlanChanged` 为准）。列表照 Puffin（kit
    /// `modes::MODE_ROWS`）：没有头，四行——三个模式与计划模式，名字与说明是 Puffin 的，在用的模式说 `current`，计划模式说
    /// `on` / `off`；选中计划模式那一行开关计划模式。以前（工作线 F76）只有三个模式，名字与说明是 App 的，有头与副标题。
    /// Node 写的 edit 会话不在列表里，谁都不说 `current`。
    pub(super) fn mode_command(&mut self, args: &str) -> Option<Action> {
        let zh = self.lang == Lang::Zh;
        if args.is_empty() {
            let current = self.view.mode();
            let plan = self.view.plan();
            let mut items: Vec<Item> = MENU
                .iter()
                .map(|mode| Item {
                    label: super::i18n::mode_label(self.lang, *mode).to_owned(),
                    description: super::i18n::mode_description(self.lang, *mode).to_owned(),
                    current: *mode == current,
                    value: Choice::Mode(*mode),
                    ..Item::default()
                })
                .collect();
            items.push(Item {
                label: super::i18n::mode_label(self.lang, PermissionMode::Plan).to_owned(),
                description: super::i18n::mode_description(self.lang, PermissionMode::Plan)
                    .to_owned(),
                // 选中这一行开关计划模式（keys.rs）。输入框上方的一句在视图里的开关变了时说（status_motion.rs，Puffin `asked`）。
                value: Choice::Mode(PermissionMode::Plan),
                tag: if plan { "on" } else { "off" }.to_owned(),
                ..Item::default()
            });
            let title = if zh { "模式" } else { "Mode" };
            self.show_list(ListView::new(
                Purpose::Permissions,
                title.into(),
                items,
                String::new(),
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
        // 切到完全访问先弹确认框，确认后才切换（tui.md §7.8，工作线 F87）。模式的一句在视图里的模式变了时说
        // （status_motion.rs，Puffin `asked`：不论从哪里变）；以前 `/mode` 在命令发出之前就说了，⇧⇥ 与请求的回答不说。
        self.switch_mode(mode, false)
    }

    /// `/mode` 切换时输入框上方安静的一句（Puffin `mode_says`；以前是 `Mode switched to …`）。
    pub(super) fn mode_switched(&mut self, mode: PermissionMode) {
        self.info(super::i18n::mode_says(self.lang, mode));
    }

    /// `/copy [N]`：第 N 新的回复（1 是最后一条）。
    pub(super) fn copy_nth(&mut self, args: &str) -> Option<Action> {
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
            let reply = reply.to_owned();
            self.info(self.strings.copied);
            return Some(Action::Copy(reply));
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
