// 改写自 apps/zcode-host/crates/tui/src/ui/keys.rs（feat/rs 79b6691c55）：去掉启动失败页、登录、任务面板、
// 后台任务、子代理与回退浏览；交互的回答换成 `Action::Resolve`。
//! 按键处理：视图（列表、页面、详细视图、快捷键面板）→ 选区 → 待处理交互 → 搜索模式 → 历史搜索 → 弹出列表 →
//! 输入框与全局键。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::model::ModelSelection;

use super::app::{Action, App, Overlay};
use super::composer::Popup;
use super::keyboard;
use super::layer::Layer;
use super::list::{Choice, Outcome};

impl App {
    /// 粘贴给最上面的一层（与按键同一层，`layer.rs`）。
    pub fn on_paste(&mut self, text: &str) -> Option<Action> {
        match self.top_layer() {
            // 打开着的视图：有输入框的收进去，别的丢掉。
            Layer::Overlay => {
                self.overlay_paste(text);
                return None;
            }
            // 询问框：写意见、`Other` 或改“不再询问”的前缀时收进去，别的时候丢掉（不进被盖住的输入框）。
            Layer::Interaction => {
                if let (Some((_, request)), Some(interaction)) = (
                    super::app::pending_of(&self.view, &self.detached),
                    self.interaction.as_mut(),
                ) {
                    interaction.paste(request, text);
                }
                return None;
            }
            Layer::Peer => return None,
            Layer::Composer => {}
        }
        // 快捷键面板：丢掉（同按键，下一个键关闭它）。
        if self.overlay.is_some() {
            self.overlay_paste(text);
            return None;
        }
        // 搜索模式：粘贴进查询（修复线 F114，tui-input.md §2.3.1）。
        if self.port.find.is_some() {
            self.find_paste(text);
            return None;
        }
        // 只读查看子代理时输入框不能输入。
        if self.viewing.is_some() {
            return None;
        }
        // 输入历史的搜索（ctrl+r）：进查询（架构整理 U12；以前进了被盖住的输入框）。
        if let Some((mut query, _)) = self.composer.search.clone() {
            super::line_edit::paste(&mut query, text);
            self.composer.search_update(query);
            return None;
        }
        // 粘贴的是图片文件的路径（拖文件进终端）：先读文件，是图片才当图片（tui-input.md §3.1）。
        if let Some(path) = super::composer::images::image_path(text) {
            return Some(Action::PasteImagePath {
                path,
                text: text.to_owned(),
            });
        }
        self.paste_plain(text);
        None
    }

    /// 文字粘贴（不再识别图片路径）。
    pub fn paste_plain(&mut self, text: &str) {
        // 空输入框里粘贴 `!` 开头的文字：切到 bash 模式并去掉 `!`（tui-input.md §2.1）。
        let text = match text.strip_prefix('!') {
            Some(rest)
                if self.composer.is_empty()
                    && self.composer.mode == super::composer::InputMode::Prompt =>
            {
                self.composer.mode = super::composer::InputMode::Bash;
                rest
            }
            _ => text,
        };
        if self.composer.paste_text(text) == super::composer::Pasted::Folded {
            self.notify(
                super::notice::paste_again(self.lang),
                super::notice::PASTE_FOR,
            );
        }
        // 粘贴替换了盖住占位文字的选区时，图片一起去掉（F26）。
        self.composer.sync_images(false);
        self.composer.refresh_popup();
    }

    /// 一个按键（测试与不经 paste burst 的入口；runner 用 `on_terminal_key`，tui-input.md §9）：松开事件丢掉，Windows
    /// 的 alt 码当作按下、AltGr 归一（`Keyboard::admit`），再交给 [`Self::dispatch`]。
    #[cfg(test)]
    pub fn on_key(&mut self, key: KeyEvent, now: Instant) -> Option<Action> {
        let key = self.keys.keyboard.admit(key)?;
        self.dispatch(key, now)
    }

    /// 进了界面的按键（入口已归一，AltGr 不带 ctrl）：视图 → 选区 → 待处理交互 → 历史搜索 → 弹出列表 → 输入框与全局键。
    pub(crate) fn dispatch(&mut self, key: KeyEvent, now: Instant) -> Option<Action> {
        let ctrl = keyboard::ctrl(&key);
        // 有鼠标选区时的按键（tui-select.md §3.1）：在视图之前处理（独立页面的 ctrl+c 是关闭）。
        if self.has_selection()
            && let Some(action) = self.selection_key(&key, now)
        {
            return action;
        }
        // ctrl+c 越过询问框与批准框，照常中断与退出。
        let ctrl_c = ctrl && key.code == KeyCode::Char('c');
        match self.top_layer() {
            Layer::Overlay => return self.overlay_key(key),
            Layer::Interaction if !ctrl_c => return self.interaction_key(key, now),
            Layer::Peer if !ctrl_c => return self.peer_key(key),
            Layer::Interaction | Layer::Peer | Layer::Composer => {}
        }
        // 快捷键面板（输入框这一层）：任何键都关闭。
        if self.overlay.is_some() {
            return self.overlay_key(key);
        }
        // footer 下面的子代理列表获得焦点、或正在只读查看子代理（tui-panels.md §1.3、§1.4）。
        match self.focus {
            Some(super::agents::Focus::Agents(index)) => return self.agents_key(index, key, now),
            Some(super::agents::Focus::Pill) => return self.pill_key(key, now),
            None => {}
        }
        // 搜索模式（修复线 F114，tui-input.md §2.3.1）：查看子代理时也在这里，先于查看的按键。
        if self.port.find.is_some() {
            return self.find_key(key);
        }
        if self.viewing.is_some() {
            return self.viewing_key(key, now);
        }
        if self.composer.search.is_some() {
            return self.search_key(key);
        }
        // 输入框有选区时：ctrl+c 复制，esc 与 ↑/↓ 只取消选区，enter 取消后照常发送；其他键交给编辑区。
        // 只认上一帧可见的输入框：审批等视图替换了输入框时，ctrl+c 仍是中断。
        if self.composer.area.selection().is_some() && self.pointer.hits.composer.is_some() {
            match key.code {
                KeyCode::Char('c') if ctrl => return self.copy_selection(false),
                KeyCode::Esc | KeyCode::Up | KeyCode::Down => {
                    self.composer.area.clear_selection();
                    return None;
                }
                KeyCode::Enter => self.composer.area.clear_selection(),
                _ => {}
            }
        }
        if self.composer.popup.is_some()
            && let Some(action) = self.popup_key(key)
        {
            return action;
        }
        self.compose_key(key, now)
    }

    /// 待回答的交互的按键：作答时发回答。
    fn interaction_key(&mut self, key: KeyEvent, now: Instant) -> Option<Action> {
        let (Some((id, request)), Some(ui)) = (
            super::app::pending_of(&self.view, &self.detached),
            self.interaction.as_mut(),
        ) else {
            return None;
        };
        let answer = ui.on_key(request, key, now)?;
        Some(Action::Resolve {
            interaction: id.clone(),
            answer,
        })
    }

    /// 扣住的同伴消息的批准框（cross-session.md §2.6）：↑ 拒绝、↓ 投递，enter 确认，esc 拒绝；别的键不生效。
    fn peer_key(&mut self, key: KeyEvent) -> Option<Action> {
        let message = self.view.peer_held()?.msg_id.clone();
        match key.code {
            KeyCode::Up | KeyCode::Char('1') => self.peer_choice = 0,
            KeyCode::Down | KeyCode::Char('2') => self.peer_choice = 1,
            KeyCode::Tab | KeyCode::BackTab => self.peer_choice = 1 - self.peer_choice.min(1),
            KeyCode::Enter => {
                let approve = self.peer_choice == 1;
                self.peer_choice = 0;
                return Some(Action::ResolvePeer { message, approve });
            }
            KeyCode::Esc => {
                self.peer_choice = 0;
                return Some(Action::ResolvePeer {
                    message,
                    approve: false,
                });
            }
            _ => {}
        }
        None
    }

    /// 有鼠标选区时的一个按键。返回 `Some` 时按键到此为止。
    ///
    /// 缺陷修复（F25）：原来 enter 是“复制并回到底部”、esc 清掉选区，footer 写着这两个键；其实不需要这两个键，复制在松开
    /// 鼠标时已经做了。现在：ctrl+c 在复制过时只清掉选区（再提示一次），没复制过时复制并清掉，都不中断、不退出；
    /// ctrl+shift+c、cmd+c 复制；esc、翻页、shift / alt 加方向键不动选区；其余按键清掉选区后照常处理。
    fn selection_key(&mut self, key: &KeyEvent, now: Instant) -> Option<Option<Action>> {
        let mods = key.modifiers;
        let (ctrl, alt) = (keyboard::ctrl(key), keyboard::alt(key));
        let shift = mods.contains(KeyModifiers::SHIFT);
        let sup = mods.contains(KeyModifiers::SUPER);
        if matches!(key.code, KeyCode::Char('c' | 'C')) && !alt {
            if ctrl && !shift && !sup {
                let copied = self.pointer.selection.is_some_and(|s| s.copied)
                    || self.pointer.screen_selection.is_some_and(|s| s.copied);
                if copied {
                    self.pointer.clear_selection();
                    if let Some((_, at)) = self.pointer.copied.as_mut() {
                        *at = now;
                    }
                    return Some(None);
                }
                return Some(self.copy_selection(true));
            }
            if ctrl || sup {
                return Some(self.copy_selection(true));
            }
        }
        // 这些键不清选区。
        let arrows = matches!(
            key.code,
            KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Home
                | KeyCode::End
        );
        let keeps = matches!(key.code, KeyCode::Esc | KeyCode::PageUp | KeyCode::PageDown)
            || (matches!(key.code, KeyCode::Home | KeyCode::End) && ctrl)
            || (arrows && (shift || alt || sup));
        // 全屏视图的选区按屏幕坐标记：翻页之后不再对应原来的文字，清掉（tui-select.md D3）。
        let scrolls_screen = self.pointer.screen_selection.is_some()
            && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown);
        if !keeps || scrolls_screen {
            self.pointer.clear_selection();
        }
        None
    }

    /// 选择列表的按键（`overlay.rs` 分派）。
    pub(super) fn list_key(&mut self, key: KeyEvent) -> Option<Action> {
        let Some(Overlay::List(list)) = self.overlay.as_mut() else {
            return None;
        };
        match list.on_key(key) {
            Outcome::Pending => None,
            Outcome::Closed => {
                // `/memory` 按 esc：给出提示（tui-input.md §4.2）。
                let cancelled = list.cancelled.take();
                self.overlay = None;
                if let Some(text) = cancelled {
                    self.info(text);
                }
                None
            }
            Outcome::Chosen { choice, level } => {
                self.overlay = None;
                self.chosen(choice, level)
            }
        }
    }

    /// 选择器选中一项后的动作；`level` 是带档位的条目（模型）的焦点档位。
    fn chosen(&mut self, choice: Choice, level: Option<String>) -> Option<Action> {
        match choice {
            Choice::Close => None,
            Choice::Model(selection) => Some(Action::ChooseModel(ModelSelection {
                reasoning: level,
                ..selection
            })),
            // 与带参数的命令走同一条路径（tui-display.md §4.3、§4.4）。
            Choice::Mode(mode) => self.switch_mode(mode, true),
            Choice::Effort(level) => Some(Action::Manage(super::manage::Manage::Effort(level))),
            Choice::Rewind(message) => self.rewind_chosen(&message),
            Choice::RewindTo { scope, target } => self.rewind_confirmed(scope, target),
            Choice::RewindBack => {
                let entries = std::mem::take(&mut self.rewind);
                self.open_rewind(entries);
                None
            }
            Choice::Memory(file) => Some(Action::OpenMemory {
                path: file.path,
                display: file.display,
            }),
            Choice::Fork(input) => Some(Action::Fork { input }),
            Choice::Theme(theme) => Some(Action::Manage(super::manage::Manage::SetTheme(theme))),
        }
    }

    fn search_key(&mut self, key: KeyEvent) -> Option<Action> {
        let ctrl = keyboard::ctrl(&key);
        let (mut query, _) = self.composer.search.clone()?;
        match key.code {
            // esc / tab 采用；enter 采用并发送；ctrl+c 取消，输入框保持原样。
            KeyCode::Esc | KeyCode::Tab => self.composer.search_accept(),
            KeyCode::Enter => {
                self.composer.search_accept();
                return self.submit();
            }
            KeyCode::Char('c') if ctrl => self.composer.search = None,
            KeyCode::Char('r') if ctrl => self.composer.search_older(),
            KeyCode::Backspace => {
                crate::ui::line_edit::backspace(&mut query);
                self.composer.search_update(query);
            }
            KeyCode::Char(c) if keyboard::is_text(&key) => {
                query.push(c);
                self.composer.search_update(query);
            }
            _ => {}
        }
        None
    }

    /// 弹出列表的按键；不处理的键返回 None，交给输入框。
    fn popup_key(&mut self, key: KeyEvent) -> Option<Option<Action>> {
        match key.code {
            KeyCode::Up => self.composer.popup_move(false),
            KeyCode::Down => self.composer.popup_move(true),
            KeyCode::Esc => self.composer.popup = None,
            KeyCode::Tab | KeyCode::Enter => {
                return Some(match self.composer.popup {
                    Some(Popup::Slash { .. }) => {
                        self.composer.accept_slash()?;
                        // tab 只补全；enter 补全并执行（本工作线的命令参数都是可选的，tui.md §6）：
                        // TUI 命令在本地执行，skill 整行发给内核展开（与直接回车同一条路径）。
                        if key.code == KeyCode::Tab {
                            None
                        } else {
                            self.submit()
                        }
                    }
                    // `@` 文件：tab 先补公共前缀，enter 直接接受（tui-input.md §1.2）。
                    Some(Popup::Files { .. }) => {
                        self.composer.accept_file(key.code == KeyCode::Tab);
                        None
                    }
                    None => None,
                });
            }
            _ => return None,
        }
        Some(None)
    }
}
