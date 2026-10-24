// 改写自 apps/zcode-host/crates/tui/src/ui/compose.rs（feat/rs 79b6691c55）：投递方式
// （docs/specs/rust-cli/tui.md §5.1）；去掉任务面板、子代理与回退编辑。输入能力的键在 `compose_input.rs`
// （tui-input.md）。
//! 输入框与全局键：编辑键与撤销、发送（运行中 enter 插话）、esc / ctrl+c / ctrl+d 的连按、输入历史、
//! 权限模式切换。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::permission::PermissionMode;

use super::app::{Action, App, Overlay};
use super::notice::{self, Armed};

/// 删掉这么多字符以上时提示 ctrl+y 可以粘回。
const KILL_HINT_CHARS: usize = 3;

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
        let ctrl = self.typing.ctrl(&key);
        let alt = self.typing.alt(&key);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let width = self.composer_width;
        // 翻页：每次可视高度减 1 行。
        let page = self.viewport.saturating_sub(1).max(1) as isize;
        // 任何键都让等待第二次按下的提示失效（连按的键自己会重新判断）。
        let double =
            key.code == KeyCode::Esc || (ctrl && matches!(key.code, KeyCode::Char('c' | 'd')));
        if !double {
            self.notice = self.notice.take().filter(|n| n.armed.is_none());
        }
        if !matches!(key.code, KeyCode::Up) {
            self.history_presses = 0;
        }
        // 编辑前记撤销点：普通输入 1 秒内的连续按键算一步。
        let typed = self.typing.is_plain_text(&key);
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
        let text = self.typing.is_text(&key);
        if let KeyCode::Char(c) = key.code
            && text
        {
            self.composer.before_typing(c, alt);
        }
        let area = &mut self.composer.area;
        let mut killed = 0;
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
            KeyCode::Enter if area.is_empty() && self.scroll > 0 => {
                self.scroll = 0;
                self.new_activity = false;
                None
            }
            KeyCode::Enter => return self.submit(),
            KeyCode::Esc => return self.escape(now),
            KeyCode::Char('c') if ctrl => return self.ctrl_c(now),
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
            // 展开或收起任务清单（tui-display.md §2.2）。
            KeyCode::Char('t') if ctrl => {
                self.tasks.expanded = !self.tasks.expanded;
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
                killed = area.kill_to_end();
                None
            }
            KeyCode::Char('u') if ctrl => {
                killed = area.kill_to_start();
                None
            }
            KeyCode::Char('w') if ctrl => {
                killed = area.kill_word();
                None
            }
            KeyCode::Char('y') if ctrl => {
                area.yank();
                None
            }
            KeyCode::Char('d') if alt => {
                killed = area.kill_word_forward();
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
            KeyCode::Home if ctrl => {
                let top = self.transcript_lines;
                return self.scroll_by(top as isize);
            }
            KeyCode::End if ctrl => {
                self.scroll = 0;
                self.new_activity = false;
                None
            }
            // 循环到完全访问时先弹确认框（tui.md §7.8，工作线 F87）。
            KeyCode::BackTab => return self.switch_mode(next_mode(self.view.mode()), false),
            KeyCode::Char('?') if area.is_empty() => {
                self.overlay = Some(Overlay::Shortcuts);
                return None;
            }
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
            // 输入框为空时 ← 打开项目页（修复线 F29，tui-panels.md §2；原来是 `← for agents` 的会话面板）。
            KeyCode::Left if area.is_empty() => return self.open_projects(),
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
                // 输入框为空：先选中后台任务的 pill，没有时进入子代理列表
                // （tui-panels.md §1.3、§3.1）。
                if self.composer.is_empty() && (self.focus_pill() || self.focus_agents(now)) {
                    return None;
                }
                self.composer.area.vertical(width, true);
                None
            }
            KeyCode::PageUp => return self.scroll_by(page),
            KeyCode::PageDown => return self.scroll_by(-page),
            _ => None,
        };
        if killed >= KILL_HINT_CHARS {
            self.notify_keys(notice::yank_hint(self.lang), notice::HINT_FOR);
        }
        // 删掉了 `[Image #N]` 的图片一起去掉；撤销时找回（tui-input.md §3.1，F26）。
        let undo = ctrl && matches!(key.code, KeyCode::Char('_' | '7'));
        self.composer.sync_images(undo);
        self.composer.refresh_popup();
        action
    }

    /// ↑：光标在第一行时取更早的历史（连按两次后提示 ctrl+r）；否则上移一行。
    fn up(&mut self) -> Option<Action> {
        let width = self.composer_width;
        self.history_presses += 1;
        if self.composer.area.at_first_row(width) && self.composer.history_prev() {
            if self.history_presses >= 2 && !self.history_hinted {
                self.history_hinted = true;
                self.notify_keys(notice::search_history(self.lang), notice::HINT_FOR);
            }
        } else {
            self.composer.area.vertical(width, false);
        }
        None
    }

    /// ctrl+c：运行中中断 → 输入框有内容时清空（记入历史）→ 空闲且为空时连按两次退出。
    fn ctrl_c(&mut self, now: Instant) -> Option<Action> {
        if self.running() {
            return Some(Action::Interrupt);
        }
        if !self.composer.is_empty() {
            self.composer.discard();
            return None;
        }
        self.second_press(Armed::ExitC, now).then_some(Action::Quit)
    }

    /// esc：上滚时回到底部 → 运行中中断 → 输入框有内容时连按两次清空。
    fn escape(&mut self, now: Instant) -> Option<Action> {
        if self.scroll > 0 {
            self.scroll = 0;
            self.new_activity = false;
            return None;
        }
        if self.running() {
            return Some(Action::Interrupt);
        }
        if !self.composer.is_empty() && self.second_press(Armed::Clear, now) {
            self.composer.discard();
        }
        // 输入为空、空闲、有消息时连按两次 esc 打开 rewind 选择器。
        if self.composer.is_empty()
            && self.session.is_some()
            && !self.view.rows().is_empty()
            && self.second_press(Armed::Edit, now)
        {
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
        // R5-9：创建会话期间也发送（runner 先留在 `held` 里，会话打开后按顺序发出），不再忽略。
        if self.composer.is_empty() {
            return None;
        }
        // bash 模式：不经斜杠命令，整行交给内核执行（tui-input.md §2）。
        if self.composer.mode == super::composer::InputMode::Bash {
            let command = self.composer.submit();
            if command.is_empty() {
                return None;
            }
            self.scroll = 0;
            return Some(Action::Bash {
                command,
                delivery: self.delivery(),
            });
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
        let delivery = self.delivery();
        // 粘贴的图片：占位文字还在的那些排在文字块之后（tui-input.md §3.1）。
        let images = self.composer.image_blocks(self.composer.area.text());
        let text = self.composer.submit();
        if text.is_empty() && images.is_empty() {
            return None;
        }
        self.scroll = 0;
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

#[cfg(test)]
#[path = "compose_tests.rs"]
mod tests;
