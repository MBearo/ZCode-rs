//! 输入能力的按键（docs/specs/rust-cli/tui-input.md §5，Chat 段的按键）：
//! ctrl+x 和弦、立即发送、排队、清空、暂存、模型选择。处理了的键返回 `Some`，其余交给 `compose_key`。
//! 另有排队的插话下面那一行立即发送提示的键与条件（tui.md §5.1.1，修复线 F104）。
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};

use rust_cli_kernel_api::session_api::Delivery;

use super::app::{Action, App};
use super::composer::InputMode;
use super::keyboard;

/// ctrl+x 之后等下一个键的时间。
pub const CHORD: Duration = Duration::from_millis(1000);

impl App {
    /// runner 建好一条输入的信封之后、发出之前（孪生在它的 Submit 里）：消息按键的那一帧就上栈或进尾巴，直到内核认领它
    /// （`view/in_flight.rs`）。`text` 与 `delivery` 是内核将要排上它的样子（`in_flight::input_of`）。
    pub fn sending(
        &mut self,
        command: rust_cli_kernel_api::ids::CommandId,
        text: String,
        delivery: Delivery,
    ) {
        self.view
            .in_flight_mut()
            .send(rust_cli_kernel_api::session_api::QueuedInput {
                command,
                text,
                delivery,
            });
    }

    /// 栈上有没有消息（等这个回合结束的排队输入，与等回合结束的 `/clear`、`/resume`，`bottom::queue` 画的那些）。
    pub fn has_stack(&self) -> bool {
        !self.after_turn.is_empty()
            || self
                .view
                .outgoing()
                .iter()
                .any(|item| item.delivery == Delivery::Queue)
    }

    /// 取回栈上最新的一条：等回合结束的本地命令就地拿掉、命令放回草稿；排队的输入请内核出队（回执之前不再请求）。
    fn take_back(&mut self) -> Option<Action> {
        if let Some((label, _)) = self.after_turn.pop() {
            self.composer.restore_input(&label, Vec::new());
            return None;
        }
        if self.taking.is_some() {
            return None;
        }
        // 还没认领的也能取回（`view/in_flight.rs`）：出队排在它的提交之后进 actor，内核照样出队。
        let item = self
            .view
            .outgoing()
            .into_iter()
            .rev()
            .find(|item| item.delivery == Delivery::Queue)?;
        self.taking = Some(item.command.clone());
        // 放回输入框的是读起来的样子：会话的链接回到 `@标题` 的文字（Puffin 放回打出的字；kit `mention_wire::unlinked`）。
        Some(Action::TakeBack {
            command: item.command,
            text: rust_cli_tui_kit::app::widgets::mention_wire::unlinked(&item.text),
        })
    }

    /// 进出 shell 模式（Puffin `shell_moded`）：这次运行第一次进时输入框上方一行安静的话说 shell 模式做什么；离开时那一行
    /// 还在就拿走。
    fn shell_moded(&mut self, is: bool) {
        let note = super::i18n_input::shell_note(self.lang, &self.shell_name);
        if is {
            if !self.shell_noted {
                self.shell_noted = true;
                self.said.quiet = Some(note);
            }
        } else if self.said.quiet.as_deref() == Some(note.as_str()) {
            self.said.quiet = None;
        }
    }

    pub(super) fn input_key(&mut self, key: KeyEvent, now: Instant) -> Option<Option<Action>> {
        let ctrl = keyboard::ctrl(&key);
        let alt = keyboard::alt(&key);
        // 和弦的第二个键；不是已知的和弦时这个键照常处理。
        if let Some(at) = self.keys.chord.take()
            && now.duration_since(at) < CHORD
        {
            match key.code {
                // ctrl+x enter：与 enter 相同（只多一个 `wait`，工具边界照样并入，tui.md §5.1）。
                // 缺陷修复（修复线 F13）：原来按 `Queue` 排到回合结束后，多出一个回合。
                KeyCode::Enter if !ctrl && !alt => return Some(self.submit()),
                KeyCode::Char('s') if ctrl => return Some(self.send_now()),
                KeyCode::Char('e') if ctrl => return Some(Some(Action::ExternalEditor)),
                // ctrl+x ctrl+b：在跑的前台 Bash 转到后台（修复线 L-A，tui-input.md §11.2；对照实现是 ctrl+b，本工程 ctrl+b
                // 是光标左移；键位暂定）。空闲时什么都不做。
                KeyCode::Char('b') if ctrl => {
                    return Some(self.running().then_some(Action::BackgroundForeground));
                }
                _ => {}
            }
        }
        // 搜索模式（修复线 F114，tui-input.md §2.3.1）：输入框里只有 `/`（斜杠列表开着）、光标在末尾时手打第二个 `/`。
        // 粘贴进来的 `//` 不走这里（不切模式）。
        if key.code == KeyCode::Char('/')
            && keyboard::is_plain_text(&key)
            && self.composer.mode == InputMode::Prompt
            && self.composer.area.text() == "/"
            && self.composer.area.cursor() == 1
        {
            self.composer.clear();
            self.open_find(false);
            return Some(None);
        }
        // ⇥：回合进行中把草稿排到这个回合结束之后，其他时候同 ⏎；空草稿什么也不做（Puffin「发出的消息」的「两条车道」）。
        // 面板开着时的 ⇥ 是补全，在这之前处理（`keys.rs`）。
        if key.code == KeyCode::Tab && !ctrl && !alt {
            // `/rename ` 后面的 ghost 是现在的名字：⇥ 把草稿换成 `/rename 名字`，接着改（Puffin `rename_ghost`）。
            let text = self.composer.area.text().to_owned();
            if self.composer.area.cursor() == text.len()
                && let Some(name) = super::input_box::rename_ghost(self, &text)
            {
                let draft = format!("/rename {name}");
                self.composer.set_text(&draft);
                return Some(None);
            }
            return Some(if self.composer.area.text().trim().is_empty() {
                None
            } else {
                self.submit_in(true)
            });
        }
        // 输入框为空、栈上有消息：⌫ 取回最新的一条（Puffin「发出的消息」的「取回」）。
        if matches!(key.code, KeyCode::Backspace)
            && self.composer.mode == InputMode::Prompt
            && self.composer.is_empty()
            && self.has_stack()
        {
            return Some(self.take_back());
        }
        // bash 模式（tui-input.md §2.1）：开头输入 `!` 进入；开头按 backspace / delete / ctrl+u / esc 退出
        // （esc 退出后照常处理）。
        let at_start = self.composer.area.cursor() == 0;
        match (self.composer.mode, key.code) {
            // 挑选回退的点时 `!` 是普通的字（Puffin `shell_mode` 的 `rewind.is_none()`）。
            (InputMode::Prompt, KeyCode::Char('!'))
                if at_start && !ctrl && !alt && self.picking.is_none() =>
            {
                self.composer.mode = InputMode::Bash;
                self.shell_moded(true);
                return Some(None);
            }
            (InputMode::Bash, KeyCode::Backspace | KeyCode::Delete) if at_start => {
                self.composer.mode = InputMode::Prompt;
                self.shell_moded(false);
                return Some(None);
            }
            (InputMode::Bash, KeyCode::Char('u')) if at_start && ctrl => {
                self.composer.mode = InputMode::Prompt;
                self.shell_moded(false);
                return Some(None);
            }
            (InputMode::Bash, KeyCode::Esc) if at_start => {
                self.composer.mode = InputMode::Prompt;
                self.shell_moded(false);
            }
            _ => {}
        }
        match key.code {
            KeyCode::Char('x') if ctrl => {
                self.keys.chord = Some(now);
                Some(None)
            }
            KeyCode::Enter if ctrl => Some(self.send_now()),
            // meta+return 换行（原来是排队，D13）。
            KeyCode::Enter if alt => {
                self.composer.area.checkpoint(now, false);
                self.composer.area.insert('\n');
                Some(None)
            }
            // 清空（记入历史，↑ 可以取回）并重画。
            KeyCode::Char('l') if ctrl => {
                self.composer.discard();
                Some(Some(Action::Redraw))
            }
            KeyCode::Char('s') if ctrl => {
                self.composer.toggle_stash();
                Some(None)
            }
            KeyCode::Char('p') if alt => Some(self.request_models()),
            // 外部编辑器（ctrl+x ctrl+e 见上面的和弦，tui-input.md §4.1）。
            KeyCode::Char('g') if ctrl => Some(Some(Action::ExternalEditor)),
            // 粘贴图片：Windows 是 alt+v，其他平台是 ctrl+v（tui-input.md §3.1）。
            KeyCode::Char('v') if (cfg!(windows) && alt) || (!cfg!(windows) && ctrl) => {
                Some(Some(Action::PasteImage))
            }
            _ => None,
        }
    }

    /// 立即发送的提示里写哪个键（修复线 F104，tui-input.md §11.1.1）：终端分辨得出增强按键时 ctrl+enter
    /// （经 `CSI u` 到达，与回车分得开），分辨不出时 ctrl+x ctrl+s（和弦，任何终端都按得出来）。
    pub(super) fn send_now_key(&self) -> &'static str {
        if self.keys.keyboard_enhanced {
            "^⏎"
        } else {
            "^X ^S"
        }
    }

    /// 输入框为空时立即发送的键能投递排着的插话（tui.md §5.1.1）：回合在跑、队列里有用户插话、没有只读查看子代理。
    /// 以前排队的插话下面另有一行 `ctrl+enter to send now`；照 Puffin 删掉（栈与尾巴只有消息，tui-kit.md §4.0），键在
    /// `?` 按键表里，有草稿时在槽里（`^⏎ send now`）。
    pub(super) fn can_deliver_queued(&self) -> bool {
        self.running() && self.viewing.is_none() && !self.view.outgoing().is_empty()
    }

    /// 立即发送：运行中是低延迟投递（修复线 L-A，tui-input.md §11.1）：runner 先入队（插话），再把前台 Bash 转到后台，
    /// 不中断当前回合；空闲时同 enter。
    ///
    /// 修复线 F105（用户决定 2026-10-27）：以前输入框为空时什么都不做，可提示那一行正让用户按这个键。现在输入框为空时
    /// 只投递排着的插话（不入队新文字）。队列为空时仍什么都不做，不中断。
    fn send_now(&mut self) -> Option<Action> {
        if self.composer.is_empty() {
            return self.can_deliver_queued().then_some(Action::DeliverQueued);
        }
        let running = self.running();
        match self.submit()? {
            Action::Submit { text, delivery } if running => {
                Some(Action::SendNow { text, delivery })
            }
            other => Some(other),
        }
    }
}
