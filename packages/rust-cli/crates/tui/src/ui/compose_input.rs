//! 输入能力的按键（docs/specs/rust-cli/tui-input.md §5，Chat 段的按键）：
//! ctrl+x 和弦、立即发送、排队、清空、暂存、模型选择。处理了的键返回 `Some`，其余交给 `compose_key`。
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};

use super::app::{Action, App};
use super::composer::InputMode;

/// ctrl+x 之后等下一个键的时间。
pub const CHORD: Duration = Duration::from_millis(1000);

impl App {
    pub(super) fn input_key(&mut self, key: KeyEvent, now: Instant) -> Option<Option<Action>> {
        let ctrl = self.typing.ctrl(&key);
        let alt = self.typing.alt(&key);
        // 和弦的第二个键；不是已知的和弦时这个键照常处理。
        if let Some(at) = self.chord.take()
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
        // bash 模式（tui-input.md §2.1）：开头输入 `!` 进入；开头按 backspace / delete / ctrl+u / esc 退出
        // （esc 退出后照常处理）。
        let at_start = self.composer.area.cursor() == 0;
        match (self.composer.mode, key.code) {
            (InputMode::Prompt, KeyCode::Char('!')) if at_start && !ctrl && !alt => {
                self.composer.mode = InputMode::Bash;
                return Some(None);
            }
            (InputMode::Bash, KeyCode::Backspace | KeyCode::Delete) if at_start => {
                self.composer.mode = InputMode::Prompt;
                return Some(None);
            }
            (InputMode::Bash, KeyCode::Char('u')) if at_start && ctrl => {
                self.composer.mode = InputMode::Prompt;
                return Some(None);
            }
            (InputMode::Bash, KeyCode::Esc) if at_start => {
                self.composer.mode = InputMode::Prompt;
            }
            _ => {}
        }
        match key.code {
            KeyCode::Char('x') if ctrl => {
                self.chord = Some(now);
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
            KeyCode::Char('p') if alt => Some(Some(Action::OpenModels)),
            // 外部编辑器（ctrl+x ctrl+e 见上面的和弦，tui-input.md §4.1）。
            KeyCode::Char('g') if ctrl => Some(Some(Action::ExternalEditor)),
            // 粘贴图片：Windows 是 alt+v，其他平台是 ctrl+v（tui-input.md §3.1）。
            KeyCode::Char('v') if (cfg!(windows) && alt) || (!cfg!(windows) && ctrl) => {
                Some(Some(Action::PasteImage))
            }
            _ => None,
        }
    }

    /// 立即发送：运行中是低延迟投递（修复线 L-A，tui-input.md §11.1）：runner 先入队（插话），再把前台 Bash 转到后台，
    /// 不中断当前回合；空闲时同 enter。
    fn send_now(&mut self) -> Option<Action> {
        let running = self.running();
        match self.submit()? {
            Action::Submit { text, delivery } if running => {
                Some(Action::SendNow { text, delivery })
            }
            other => Some(other),
        }
    }
}
