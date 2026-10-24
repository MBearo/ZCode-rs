// 改写自 apps/zcode-host/crates/tui/src/ui/keys.rs（feat/rs 79b6691c55）：去掉启动失败页、登录、任务面板、
// 后台任务、子代理与回退浏览；交互的回答换成 `Action::Resolve`。
//! 按键处理：视图（列表、页面、详细视图、快捷键面板）→ 选区 → 待处理交互 → 历史搜索 → 弹出列表 →
//! 输入框与全局键。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::model::ModelSelection;

use super::app::{Action, App, Overlay};
use super::composer::Popup;
use super::list::{Outcome, Purpose, SEP};

impl App {
    pub fn on_paste(&mut self, text: &str) -> Option<Action> {
        if let (Some((_, request)), Some(interaction)) = (
            super::app::pending_of(&self.view, &self.detached),
            self.interaction.as_mut(),
        ) && interaction.typing
        {
            interaction.paste(request, text);
            return None;
        }
        if matches!(self.overlay, Some(Overlay::Login(_))) {
            self.login_paste(text);
            return None;
        }
        // `/resume` 页：粘贴进查询或改名输入（tui-manage.md §10.4）。
        if matches!(self.overlay, Some(Overlay::Resume(_))) {
            self.resume_paste(text);
            return None;
        }
        // `/export` 的文件名输入框收粘贴（只取第一行）。
        if let Some(Overlay::Export(super::export::ExportView::Name(name))) = self.overlay.as_mut()
        {
            name.push_str(text.lines().next().unwrap_or_default());
            return None;
        }
        // `/permissions` 的规则输入框收粘贴（只取第一行）。
        if let Some(Overlay::Permissions(view)) = self.overlay.as_mut()
            && let super::permissions::Stage::Input(typed) = &mut view.stage
        {
            typed.push_str(text.lines().next().unwrap_or_default());
            return None;
        }
        // 只读查看子代理时输入框不能输入。
        if self.overlay.is_some() || self.viewing.is_some() {
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
    /// 的 alt 码当作按下（`Releases::admit`），再交给 [`Self::dispatch`]。
    pub fn on_key(&mut self, key: KeyEvent, now: Instant) -> Option<Action> {
        let key = self.releases.admit(key)?;
        self.dispatch(key, now)
    }

    /// 进了界面的按键：视图 → 选区 → 待处理交互 → 历史搜索 → 弹出列表 → 输入框与全局键。ctrl 不算 Windows 的
    /// AltGr（`Typing`）。
    pub(crate) fn dispatch(&mut self, key: KeyEvent, now: Instant) -> Option<Action> {
        let ctrl = self.typing.ctrl(&key);
        // 有鼠标选区时的按键（tui-select.md §3.1）：在视图之前处理（独立页面的 ctrl+c 是关闭）。
        if self.has_selection()
            && let Some(action) = self.selection_key(&key, now)
        {
            return action;
        }
        if self.overlay.is_some() {
            return self.overlay_key(key);
        }
        if let (Some((id, request)), Some(ui)) = (
            super::app::pending_of(&self.view, &self.detached),
            self.interaction.as_mut(),
        ) && !(ctrl && key.code == KeyCode::Char('c'))
        {
            let answer = ui.on_key(request, key, now, self.typing)?;
            return Some(Action::Resolve {
                interaction: id.clone(),
                answer,
            });
        }
        // 扣住的同伴消息的批准框（cross-session.md §2.6）：待回答的交互优先；↑ 拒绝、↓ 投递，enter 确认，esc 拒绝。
        if self.pending().is_none()
            && let Some(held) = self.view.peer_held()
            && !(ctrl && key.code == KeyCode::Char('c'))
        {
            let message = held.msg_id.clone();
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
            return None;
        }
        // footer 下面的子代理列表获得焦点、或正在只读查看子代理（tui-panels.md §1.3、§1.4）。
        match self.focus {
            Some(super::agents::Focus::Agents(index)) => return self.agents_key(index, key, now),
            Some(super::agents::Focus::Pill) => return self.pill_key(key, now),
            None => {}
        }
        if self.viewing.is_some() {
            return self.viewing_key(key, now);
        }
        if self.composer.search.is_some() {
            return self.search_key(key);
        }
        // 输入框有选区时：ctrl+c 复制，esc 与 ↑/↓ 只取消选区，enter 取消后照常发送；其他键交给编辑区。
        // 只认上一帧可见的输入框：审批等视图替换了输入框时，ctrl+c 仍是中断。
        if self.composer.area.selection().is_some() && self.hits.composer.is_some() {
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

    /// 有鼠标选区时的一个按键。返回 `Some` 时按键到此为止。
    ///
    /// 缺陷修复（F25）：原来 enter 是“复制并回到底部”、esc 清掉选区，footer 写着这两个键；其实不需要这两个键，复制在松开
    /// 鼠标时已经做了。现在：ctrl+c 在复制过时只清掉选区（再提示一次），没复制过时复制并清掉，都不中断、不退出；
    /// ctrl+shift+c、cmd+c 复制；esc、翻页、shift / alt 加方向键不动选区；其余按键清掉选区后照常处理。
    fn selection_key(&mut self, key: &KeyEvent, now: Instant) -> Option<Option<Action>> {
        let mods = key.modifiers;
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        let shift = mods.contains(KeyModifiers::SHIFT);
        let alt = mods.contains(KeyModifiers::ALT);
        let sup = mods.contains(KeyModifiers::SUPER);
        if matches!(key.code, KeyCode::Char('c' | 'C')) && !alt {
            if ctrl && !shift && !sup {
                let copied = self.selection.is_some_and(|s| s.copied)
                    || self.screen_selection.is_some_and(|s| s.copied);
                if copied {
                    self.clear_selection();
                    if let Some((_, at)) = self.copied.as_mut() {
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
        let scrolls_screen = self.screen_selection.is_some()
            && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown);
        if !keeps || scrolls_screen {
            self.clear_selection();
        }
        None
    }

    fn overlay_key(&mut self, key: KeyEvent) -> Option<Action> {
        if matches!(self.overlay, Some(Overlay::Detail(_))) {
            return self.detail_key(key);
        }
        if matches!(self.overlay, Some(Overlay::Login(_))) {
            return self.login_key(key);
        }
        if matches!(self.overlay, Some(Overlay::Permissions(_))) {
            return self.permissions_key(key);
        }
        if matches!(self.overlay, Some(Overlay::Export(_))) {
            return self.export_key(key);
        }
        if matches!(self.overlay, Some(Overlay::Diff(_))) {
            return self.diff_key(key);
        }
        if matches!(self.overlay, Some(Overlay::Projects(_))) {
            return self.projects_key(key);
        }
        if matches!(self.overlay, Some(Overlay::Resume(_))) {
            return self.resume_key(key);
        }
        if matches!(self.overlay, Some(Overlay::Trust(_))) {
            return self.trust_key(key, Instant::now());
        }
        if matches!(self.overlay, Some(Overlay::Background(_))) {
            return self.background_key(key, Instant::now());
        }
        if matches!(self.overlay, Some(Overlay::Usage(_))) {
            return self.usage_key(key);
        }
        if matches!(self.overlay, Some(Overlay::YoloConfirm(_))) {
            return self.yolo_confirm_key(key);
        }
        let ctrl = self.typing.ctrl(&key);
        match self.overlay.as_mut()? {
            Overlay::List(list) => match list.on_key(key, self.typing) {
                Outcome::Pending => None,
                Outcome::Closed => {
                    // `/memory` 按 esc：给出提示（tui-input.md §4.2）。
                    if list.purpose == Purpose::Memory {
                        self.info(super::i18n_input::memory_cancelled(self.lang).to_owned());
                    }
                    self.overlay = None;
                    None
                }
                Outcome::Chosen(value) => {
                    let purpose = list.purpose.clone();
                    self.overlay = None;
                    self.chosen(&purpose, &value)
                }
            },
            Overlay::Page { scroll, .. } => {
                let page = self.viewport.max(1);
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
                    KeyCode::Char('c') if ctrl => self.overlay = None,
                    KeyCode::Down | KeyCode::Char('j') => *scroll += 1,
                    KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                    KeyCode::PageDown | KeyCode::Char(' ') => *scroll += page,
                    KeyCode::PageUp => *scroll = scroll.saturating_sub(page),
                    KeyCode::Home => *scroll = 0,
                    _ => {}
                }
                None
            }
            Overlay::Shortcuts => {
                self.overlay = None;
                None
            }
            Overlay::Detail(_)
            | Overlay::Login(_)
            | Overlay::Permissions(_)
            | Overlay::Export(_)
            | Overlay::Diff(_)
            | Overlay::Projects(_)
            | Overlay::Resume(_)
            | Overlay::Trust(_)
            | Overlay::Background(_)
            | Overlay::Usage(_)
            | Overlay::YoloConfirm(_) => None,
        }
    }

    /// 选择器选中一项后的动作。模型列表的值是 `provider SEP model SEP 团队项目 SEP 档位`（团队项目是 JSON，可为空）。
    fn chosen(&mut self, purpose: &Purpose, value: &str) -> Option<Action> {
        match purpose {
            Purpose::Model => {
                let mut parts = value.split(SEP);
                let provider = parts.next()?.to_owned();
                let model = parts.next()?.to_owned();
                let connection = parts
                    .next()
                    .filter(|c| !c.is_empty())
                    .and_then(|c| serde_json::from_str(c).ok())
                    .map(Box::new);
                let reasoning = parts.next().filter(|r| !r.is_empty()).map(str::to_owned);
                Some(Action::ChooseModel(ModelSelection {
                    provider,
                    model,
                    reasoning,
                    connection,
                }))
            }
            Purpose::Rewind => self.rewind_chosen(value),
            Purpose::RewindConfirm => self.rewind_confirmed(value),
            Purpose::Memory => {
                let file = self.memory_files.get(value.parse::<usize>().ok()?)?;
                Some(Action::OpenMemory {
                    path: file.path.clone(),
                    display: file.display.clone(),
                })
            }
            Purpose::Fork => self.fork_chosen(value),
            Purpose::Theme => self.set_theme(value),
            // 与带参数的命令走同一条路径（tui-display.md §4.3、§4.4）。
            Purpose::Permissions => self.mode_command(value),
            Purpose::Effort => Some(Action::Manage(super::manage::Manage::Effort(
                value.to_owned(),
            ))),
            Purpose::Info => None,
        }
    }

    fn search_key(&mut self, key: KeyEvent) -> Option<Action> {
        let ctrl = self.typing.ctrl(&key);
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
                query.pop();
                self.composer.search_update(query);
            }
            KeyCode::Char(c) if self.typing.is_text(&key) => {
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
