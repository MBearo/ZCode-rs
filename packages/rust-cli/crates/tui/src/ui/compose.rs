// 改写自 apps/zcode-host/crates/tui/src/ui/compose.rs（feat/rs 79b6691c55）：投递方式
// （docs/specs/rust-cli/tui.md §5.1）；去掉任务面板、子代理与回退编辑。输入能力的键在 `compose_input.rs`
// （tui-input.md）。
//! 输入框与全局键：编辑键与撤销、发送（运行中 enter 插话）、esc / ctrl+c / ctrl+d 的连按、输入历史、
//! 权限模式切换。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::Delivery;

use super::app::{Action, App};
use super::keyboard;
use super::notice::{self, Armed};

/// 连按 ↑ 这么多次后提示 ctrl+r 搜索历史。
const HISTORY_HINT_PRESSES: usize = 2;

/// shift+tab 的循环（修复线 F20，用户决定，参照 Node `app-mode.ts` 去掉 plan）：build → guarded → yolo → build。
/// 不在循环里的模式（acceptEdits、dontAsk、auto）回到 build；plan 是独立开关，shift+tab 不动它。
/// （原来是 default → acceptEdits → plan → default。）
pub(super) fn next_mode(mode: PermissionMode) -> PermissionMode {
    match mode {
        PermissionMode::Default => PermissionMode::Guarded,
        PermissionMode::Guarded => PermissionMode::BypassPermissions,
        PermissionMode::BypassPermissions
        | PermissionMode::AcceptEdits
        | PermissionMode::Plan
        | PermissionMode::DontAsk
        | PermissionMode::Auto => PermissionMode::Default,
    }
}

impl App {
    pub(super) fn compose_key(&mut self, key: KeyEvent, now: Instant) -> Option<Action> {
        // Windows 的 AltGr（Ctrl+Alt 加字符）是文字，不是 ctrl / alt（修复线 F65，tui-input.md §9.1）。
        let ctrl = keyboard::ctrl(&key);
        let alt = keyboard::alt(&key);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let width = self.composer_width;
        // 翻页：每次跟随时的正文高度减 1 行（Puffin）。
        let page = self.page_rows();
        // 任何键都让等待第二次按下的提示失效（连按的键自己会重新判断）。
        let double =
            key.code == KeyCode::Esc || (ctrl && matches!(key.code, KeyCode::Char('c' | 'd')));
        if !double {
            self.notice = self.notice.take().filter(|n| n.armed.is_none());
        }
        if !matches!(key.code, KeyCode::Up) {
            self.keys.history_presses = 0;
        }
        // 编辑前记撤销点：普通输入 1 秒内的连续按键算一步。
        let typed = keyboard::is_plain_text(&key);
        let edits = typed
            || matches!(key.code, KeyCode::Backspace | KeyCode::Delete)
            || (ctrl && matches!(key.code, KeyCode::Char('k' | 'u' | 'w' | 'y' | 'd' | 'j')))
            || (alt && matches!(key.code, KeyCode::Char('d' | 'y')));
        if edits {
            // 替换选区单独算一步撤销，不和前面的输入合并。
            let typed = typed && self.composer.area.selection().is_none();
            self.composer.area.checkpoint(now, typed);
        }
        if let Some(action) = self.input_key(key, now) {
            self.composer.sync_images(false);
            self.composer.refresh_popup();
            return action;
        }
        // 粘贴图片之后键入的第一个字符前按规则补空格（tui-input.md §3.1）。
        let text = keyboard::is_text(&key);
        if let KeyCode::Char(c) = key.code
            && text
        {
            self.composer.before_typing(c, alt);
        }
        let area = &mut self.composer.area;
        let action = match key.code {
            // 行尾的 `\` 加 enter：换行。
            KeyCode::Enter if area.text()[..area.cursor()].ends_with('\\') => {
                area.checkpoint(now, false);
                area.backspace();
                area.insert('\n');
                None
            }
            KeyCode::Enter if shift => {
                area.checkpoint(now, false);
                area.insert('\n');
                None
            }
            KeyCode::Char('j') if ctrl => {
                area.insert('\n');
                None
            }
            // 上滚时输入框为空，enter 回到底部。
            KeyCode::Enter if area.is_empty() && self.port.scroll > 0 => {
                self.port.scroll = 0;
                self.port.new_activity = false;
                None
            }
            KeyCode::Enter => return self.submit(),
            KeyCode::Esc => return self.escape(now),
            KeyCode::Char('c') if ctrl => return self.ctrl_c(now),
            // ^Q 退出（Puffin：任何时候、不用按两次；`?` 按键表里有它）。
            KeyCode::Char('q') if ctrl => return Some(Action::Quit),
            KeyCode::Char('d') if ctrl && area.is_empty() => {
                return self.second_press(Armed::ExitD, now).then_some(Action::Quit);
            }
            KeyCode::Char('d') if ctrl => {
                area.delete();
                None
            }
            KeyCode::Char('o') if ctrl => {
                self.open_detail();
                return None;
            }
            // 任务卡（tui-tasks.md §3.1）：草稿为空、没有请求时打开，同 Puffin 工作流卡的 ^T。以前 ctrl+t 收起与展开任务
            // 清单，后来打开子代理的层（用户决定 2026-10-31：子代理、后台命令与待办合成一张卡）。
            KeyCode::Char('t') if ctrl => {
                if self.tasks_key_works() {
                    self.open_tasks();
                }
                return None;
            }
            KeyCode::Char('r') if ctrl => {
                self.composer.search_update(String::new());
                return None;
            }
            // ctrl+_：多数终端发成 0x1f，crossterm 解析成 ctrl+7。
            KeyCode::Char('_' | '7') if ctrl => {
                area.undo();
                None
            }
            KeyCode::Char('a') if ctrl => {
                area.home();
                None
            }
            KeyCode::Char('e') if ctrl => {
                area.end();
                None
            }
            KeyCode::Char('b') if ctrl => {
                area.left();
                None
            }
            KeyCode::Char('f') if ctrl => {
                area.right();
                None
            }
            KeyCode::Char('k') if ctrl => {
                area.kill_to_end();
                None
            }
            KeyCode::Char('u') if ctrl => {
                area.kill_to_start();
                None
            }
            KeyCode::Char('w') if ctrl => {
                area.kill_word();
                None
            }
            KeyCode::Char('y') if ctrl => {
                area.yank();
                None
            }
            KeyCode::Char('d') if alt => {
                area.kill_word_forward();
                None
            }
            KeyCode::Char('y') if alt => {
                area.yank_pop();
                None
            }
            KeyCode::Char('b') if alt => {
                area.word_left();
                None
            }
            KeyCode::Char('f') if alt => {
                area.word_right();
                None
            }
            KeyCode::Left if alt => {
                area.word_left();
                None
            }
            KeyCode::Right if alt => {
                area.word_right();
                None
            }
            KeyCode::Home if ctrl => return self.scroll_home(),
            // 回到末尾：屏幕上写 ^N（tui-kit.md §4.0）；ctrl+End 是不写出来的旧键。挑选回退的点时 ctrl+n 是下一个点，
            // 在 `picking_key` 里先处理了，到这里的只有 ctrl+End。
            KeyCode::Char('n') | KeyCode::End if ctrl => {
                self.port.scroll = 0;
                self.port.new_activity = false;
                // 挑选回退的点时 ^End 回到切线（Puffin「回退（/rewind）」）。
                if let Some(picking) = self.picking.as_mut() {
                    picking.at_cut = true;
                }
                None
            }
            // 循环到完全访问时先弹确认框（tui.md §7.8，工作线 F87）。
            KeyCode::BackTab => return self.switch_mode(next_mode(self.view.mode()), false),
            KeyCode::Char(c) if text => {
                area.insert(c);
                None
            }
            KeyCode::Backspace => {
                area.backspace();
                None
            }
            KeyCode::Delete => {
                area.delete();
                None
            }
            // 输入框为空时 ← 打开驾驶舱（tui-cockpit.md §3.1；以前是所有文件夹的会话列表）。
            KeyCode::Left if area.is_empty() => return self.open_cockpit(),
            KeyCode::Left => {
                area.left();
                None
            }
            KeyCode::Right => {
                area.right();
                None
            }
            KeyCode::Home => {
                area.home();
                None
            }
            KeyCode::End => {
                area.end();
                None
            }
            KeyCode::Up => return self.up(),
            KeyCode::Down => {
                // 光标在最后一行：浏览更新的历史；否则下移一行。
                let at_last = self.composer.area.at_last_row(width);
                if at_last && self.composer.history_next() {
                    return None;
                }
                // 输入框为空、有东西可看：打开任务卡（兼容的旧键，tui-tasks.md §3.1）。
                if self.composer.is_empty() && self.has_tasks() {
                    self.open_tasks();
                    return None;
                }
                self.composer.area.vertical(width, true);
                None
            }
            KeyCode::PageUp => return self.scroll_shown(page),
            KeyCode::PageDown => return self.scroll_shown(-page),
            _ => None,
        };
        // 删掉了 `[Image #N]` 的图片一起去掉；撤销时找回（tui-input.md §3.1，F26）。
        let undo = ctrl && matches!(key.code, KeyCode::Char('_' | '7'));
        self.composer.sync_images(undo);
        self.composer.refresh_popup();
        action
    }

    /// ↑：光标在第一行时取更早的历史（连按两次后提示 ctrl+r）；否则上移一行。
    fn up(&mut self) -> Option<Action> {
        let width = self.composer_width;
        self.keys.history_presses += 1;
        if self.composer.area.at_first_row(width) && self.composer.history_prev() {
            if self.keys.history_presses >= HISTORY_HINT_PRESSES && !self.keys.history_hinted {
                self.keys.history_hinted = true;
                self.notify_keys(notice::search_history(self.lang), notice::HINT_FOR);
            }
        } else {
            self.composer.area.vertical(width, false);
        }
        None
    }

    /// 中断这个回合：从现在起回答的光标不再画、不再落定（Puffin `stop_pending`，§4.2「Stopped.」），直到回合结束。
    pub(crate) fn interrupt(&mut self) -> Action {
        self.stop_pending = self.view.running_turn();
        Action::Interrupt
    }

    /// ctrl+c：运行中中断 → 输入框有内容时清空（记入历史）→ 空闲且为空时连按两次退出。
    fn ctrl_c(&mut self, now: Instant) -> Option<Action> {
        if self.running() {
            return Some(self.interrupt());
        }
        if !self.composer.is_empty() {
            self.composer.discard();
            return None;
        }
        self.second_press(Armed::ExitC, now).then_some(Action::Quit)
    }

    /// esc：上滚时回到底部 → 运行中中断 → 输入框为空时连按两次挑选回退的点；有草稿时什么也不做（Puffin 的 esc，
    /// docs/puffin-tui.md「按键」：草稿不会被换掉）。以前有草稿时连按两次清空。
    fn escape(&mut self, now: Instant) -> Option<Action> {
        if self.port.scroll > 0 {
            self.port.scroll = 0;
            self.port.new_activity = false;
            return None;
        }
        if self.running() {
            return Some(self.interrupt());
        }
        // 输入为空、空闲、有消息时：第一次 esc 槽里说 `esc rewind`，第二次在最后一条消息上开始挑选回退的点（Puffin
        // 「回退（/rewind）」的「进入」；点到了再挑，`App::open_rewind`）。以前第二次打开回退的列表。
        if self.composer.is_empty()
            && self.session.is_some()
            && !self.view.rows().is_empty()
            && self.second_press(Armed::Edit, now)
        {
            self.picking_wanted = true;
            return Some(Action::OpenRewind);
        }
        None
    }

    /// skill 清单到了（runner 的 `Done::Commands`，启动时与每个回合结束后）：换上清单；清单第一次到之前提交、
    /// 当时判定不了的命令现在重新提交（tui.md §6），返回它的动作。
    pub fn commands_arrived(
        &mut self,
        commands: Vec<rust_cli_kernel_api::user_command::UserCommand>,
    ) -> Option<Action> {
        // `$` 面板的目录取自同一份清单（与 Puffin host 的 `skills_of` 同一条规则，tui-input.md §1.4）。
        self.composer.mentions.catalog = Some(super::skill_mention::of_commands(&commands));
        self.composer.skills = commands;
        self.composer.skills_loaded = true;
        self.composer.refresh_popup();
        if std::mem::take(&mut self.held_command) {
            self.submit()
        } else {
            None
        }
    }

    /// 发送输入框的内容：斜杠命令在本地执行；其余按投递方式发给内核（enter 与 ctrl+x enter 相同，tui.md §5.1）。
    pub(super) fn submit(&mut self) -> Option<Action> {
        self.submit_in(false)
    }

    /// 发送草稿；`after` 为真时（⇥）回合进行中排到这个回合结束之后（Puffin 的第二条车道「after this turn」），
    /// 其他时候同 ⏎。
    pub(super) fn submit_in(&mut self, after: bool) -> Option<Action> {
        let delivery = if after {
            Delivery::Queue
        } else {
            self.delivery()
        };
        // R5-9：创建会话期间也发送（runner 先留在 `held` 里，会话打开后按顺序发出），不再忽略。草稿是空的、最新的失败块
        // 说 `⏎ tries again.` 时，⏎ 照原样再发失败回合的消息（Puffin `try_again`，§4.2「再试一次」）。
        if self.composer.is_empty() {
            return self
                .try_again()
                .map(|text| Action::Submit { text, delivery });
        }
        // 输入框上方的一句到下一次发送为止（Puffin 发送时 `notice = None`，tui-kit.md §4.0「本地命令的结果」）：命令自己说的
        // 话在这之后再写上。
        self.said = super::app::Said::default();
        self.goal_sent();
        // bash 模式：不经斜杠命令，整行交给内核执行（tui-input.md §2）。
        if self.composer.mode == super::composer::InputMode::Bash {
            let command = self.composer.submit();
            if command.is_empty() {
                return None;
            }
            self.port.scroll = 0;
            // 空闲时内核立即运行它：块在 ⏎ 时就画在末尾（Puffin `run_shell`）。回合在跑时内核把它排到回合之后（D9），
            // 照旧在板上的栈里。
            if !self.running() {
                self.view.hold_shell(&command);
            }
            return Some(Action::Bash { command, delivery });
        }
        let text = self.composer.area.text().trim().to_owned();
        // 整行是 exit、quit、:q 等时退出（tui-display.md §4.1）。
        if super::slash_more::is_exit_word(&text) {
            self.composer.submit();
            return Some(Action::Quit);
        }
        if let Some(rest) = text.strip_prefix('/') {
            let name = rest.split_whitespace().next().unwrap_or("");
            if super::commands::find(name).is_some() {
                self.composer.submit();
                return self.slash(&text);
            }
            // 不是命令（`/` 开头的路径等）时照常发送；看起来像命令名时提示，输入保留。skill（内核给出的
            // 清单，含内置 `/loop`）整行发给内核展开（ext.md §2.7）。
            let skill = super::commands::skill(&self.composer.skills, name).is_some();
            if !name.is_empty() && !name.contains('/') && !skill {
                // 缺陷修复（修复线 F13）：清单还没到时原来当场判成未知命令（输入框一就绪就键入
                // skill 命令时必现）。判定只看清单这一个来源：先留着输入，清单到了（`commands_arrived`）再提交。
                if !self.composer.skills_loaded {
                    self.held_command = true;
                    return None;
                }
                self.info(super::i18n::unrecognized(self.lang, name));
                return None;
            }
        }
        // 粘贴的图片：占位文字还在的那些排在文字块之后（tui-input.md §3.1）。
        let images = self.composer.image_blocks(self.composer.area.text());
        let text = self.composer.submit();
        if text.is_empty() && images.is_empty() {
            return None;
        }
        self.port.scroll = 0;
        // 发送普通输入后放回 ctrl+s 暂存的草稿（tui-input.md §5）。
        if self.composer.pop_stash() {
            self.notify(
                super::i18n_input::draft_restored(self.lang),
                super::notice::HINT_FOR,
            );
        }
        if !images.is_empty() {
            let mut content = vec![rust_cli_kernel_api::message::ContentBlock::Text { text }];
            content.extend(images);
            return Some(Action::SubmitContent { content, delivery });
        }
        Some(Action::Submit { text, delivery })
    }
}

impl App {
    /// ⏎ 会再发的失败回合的消息（Puffin `try_again`）：最新的块提供它（你的消息之后的失败块，说 `⏎ tries again.`），回合
    /// 不在跑，没有排着的消息。
    fn try_again(&self) -> Option<String> {
        if self.running() || !self.view.outgoing().is_empty() {
            return None;
        }
        let last = self.view.rows().iter().rev().find(|row| {
            matches!(
                row.body,
                crate::view::Body::Failure(_) | crate::view::Body::User { .. }
            )
        })?;
        let crate::view::Body::Failure(f) = &last.body else {
            return None;
        };
        (super::cells::failure::offers_again(f) && !f.resend.is_empty()).then(|| f.resend.clone())
    }
}

#[cfg(test)]
#[path = "compose_tests.rs"]
mod tests;
