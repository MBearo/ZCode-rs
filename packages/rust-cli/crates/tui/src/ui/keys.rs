// 改写自 apps/zcode-host/crates/tui/src/ui/keys.rs（feat/rs 79b6691c55）：去掉启动失败页、登录、任务面板、
// 后台任务、子代理与回退浏览；交互的回答换成 `Action::Resolve`。
//! 按键处理：视图（列表、页面、详细视图、快捷键面板）→ 选区 → 待处理交互 → 搜索模式 → 历史搜索 → 弹出列表 →
//! 输入框与全局键。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_tui_kit::app::widgets::model_picker::AUTO;

use super::app::{Action, App, Overlay};
use super::composer::Popup;
use super::keyboard;
use super::list::{Choice, Outcome, Purpose};

impl App {
    /// 粘贴给收按键的那一层（同 [`Self::dispatch_key`] 的次序）：设置页、门道的密钥、搜索模式、历史搜索，否则是草稿。
    pub fn on_paste(&mut self, text: &str) -> Option<Action> {
        // 同按键：粘贴之后没有悬停，直到鼠标再动（tui-select.md §9）。
        self.clear_hover();
        self.note_slot();
        // 设置页开着：粘贴进它的输入框（搜索或键入的值）。
        if self.settings.is_some() {
            return self.settings_paste(text);
        }
        // 门道的粘贴密钥那一步：粘贴进密钥（不进草稿）。
        if self.overlay.is_none() && self.door_paste_text(text) {
            return None;
        }
        // 搜索模式：粘贴进查询（修复线 F114，tui-input.md §2.3.1）。
        if self.port.find.is_some() && self.overlay.is_none() {
            self.find_paste(text);
            return None;
        }
        // 不跟着草稿的列表：粘贴进它的过滤，同按键（架构整理 U12）。
        // 缺陷修复：以前列表开着时粘贴被丢掉，键入的字却进过滤。
        if !self.drafted()
            && let Some(Overlay::List(list)) = self.overlay.as_mut()
        {
            list.paste(text);
            return None;
        }
        // 只读查看子代理时输入框不能输入；跟着草稿的一层开着时粘贴进草稿（draft_layer.rs）。
        if (self.overlay.is_some() && !self.drafted()) || self.viewing.is_some() {
            return None;
        }
        // 输入历史的搜索（ctrl+r）：进查询，同按键（架构整理 U12）。
        // 缺陷修复：以前粘贴进了被盖住的输入框，按键却进查询。
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
        let before = self.composer.area.text().to_owned();
        self.paste_plain(text);
        self.settle_drafted(&before, false)
    }

    /// 文字粘贴（不再识别图片路径）。
    pub fn paste_plain(&mut self, text: &str) {
        // 空输入框里粘贴 `!` 开头的文字：切到 bash 模式并去掉 `!`（tui-input.md §2.1）。
        let text = match text.strip_prefix('!') {
            // 请求开着时 `!` 是普通的字（Puffin `shell_mode` 要没有请求开着）。
            Some(rest)
                if self.composer.is_empty()
                    && self.composer.mode == super::composer::InputMode::Prompt
                    && !self.asking() =>
            {
                self.composer.mode = super::composer::InputMode::Bash;
                rest
            }
            _ => text,
        };
        // 折成帽时不再提示 `paste again to expand`（Puffin 没有这句；再粘贴一次照旧展开）。
        let _ = self.composer.paste_text(text, self.composer_width);
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
        self.note_slot();
        // 页的槽里的 `Copied the page` 留到下一个键（page_copy.rs）。
        self.page_copied = None;
        // ^T 的 `No tasks in this session. …` 同样留到下一个键（tui-tasks.md §3.1）。
        self.no_tasks = false;
        let before = self.composer.area.text().to_owned();
        let mut action = self.dispatch_key(key, now);
        // 打出命令后面的空格、草稿正好成了 `/model `、`/theme `：不等 ⏎，打开它的列表（draft_layer.rs `open_by_draft`）。
        // 用户报告（2026-10-31）：以前这两个列表只在 ⏎ 之后才出来，Puffin 在空格时就打开。
        if action.is_none()
            && let Some(name) = self.spaced_command(&before, &key)
            && let Some(opened) = self.open_by_draft(&name)
        {
            action = opened;
        }
        // 跟着草稿的一层按这个键之后的草稿开关、过滤（draft_layer.rs）；会话列表的词变了时请 runner 搜。
        let settled = self.settle_drafted(&before, matches!(key.code, KeyCode::Up | KeyCode::Down));
        // 输入层的卡：打了字就关上（cards.rs）。
        self.settle_card();
        // 请求开着：打进空输入框的字选中 Deny / Keep planning；命令架不开（Puffin `settle_ask`、`asking` 的门）。
        if self.asking() {
            self.settle_ask(&before);
            self.composer.popup = None;
        }
        // 挑选回退的点时命令面板不打开（Puffin「回退（/rewind）」的「按键」）。
        if self.picking.is_some() {
            self.composer.popup = None;
        }
        match (action, settled) {
            (Some(action), Some(search)) => Some(Action::Many(vec![action, search])),
            (action, settled) => action.or(settled),
        }
    }

    /// 这个键是打进草稿末尾的空格、草稿成了 `/model ` 或 `/theme `（之前没有这个空格）：命令名。粘贴与历史调回不经这里。别名不算
    /// （tui-kit.md §4.0「板」）：`/effort low` 打到空格就打开的话，`low` 会去过滤模型，⏎ 时看到的是没有匹配。
    fn spaced_command(&self, before: &str, key: &KeyEvent) -> Option<String> {
        if key.code != KeyCode::Char(' ')
            || keyboard::ctrl(key)
            || key.modifiers.contains(KeyModifiers::ALT)
            || self.overlay.is_some()
            || self.settings.is_some()
        {
            return None;
        }
        let text = self.composer.area.text();
        let name = text.strip_prefix('/')?.strip_suffix(' ')?;
        let typed =
            text.strip_suffix(' ') == Some(before) && self.composer.area.cursor() == text.len();
        (typed && (name.eq_ignore_ascii_case("model") || name.eq_ignore_ascii_case("theme")))
            .then(|| name.to_ascii_lowercase())
    }

    fn dispatch_key(&mut self, key: KeyEvent, now: Instant) -> Option<Action> {
        let ctrl = keyboard::ctrl(&key);
        // 有鼠标选区时的按键（tui-select.md §3.1）：在视图之前处理（独立页面的 ctrl+c 是关闭）。
        if self.has_selection()
            && let Some(action) = self.selection_key(&key, now)
        {
            return action;
        }
        // 复制页的字（tui-kit.md §4.3「页的复制」）：页上的 `c`、设置页的 ⌃y，在页自己的键之前。
        if let Some(action) = self.page_copy_key(&key, now) {
            return action;
        }
        // 设置页开着（tui-kit.md §4.2「设置」）：它拿全部的键，输入框是它的搜索；⇧⇥ 照常切换模式（Puffin `on_key`）。
        if self.settings.is_some() && key.code != KeyCode::BackTab {
            return self.settings_key(key);
        }
        // 任务卡（tui-tasks.md §3.3）：不是卡的键关上卡，照没有卡时处理。
        if let Some(action) = self.tasks_key(key) {
            return action;
        }
        // 跟着草稿的一层（列表、`?` 按键表）不拿的键去编辑草稿（tui-kit.md §4.0「板」）。
        if self.drafted() {
            if let Some(action) = self.drafted_key(key, now) {
                return action;
            }
        } else if self.overlay.is_some() {
            return self.overlay_key(key, now);
        }
        // 门道（doorway.rs）：它的回答与登录、密钥的键；其余传下去编辑草稿、滚动（Puffin 的 `door_key` 在请求之后，
        // 门道开着时没有会话，请求不会同时在）。
        if self.door.is_some()
            && let Some(action) = self.door_key(key)
        {
            return action;
        }
        // 请求开着（tui-kit.md §4.2「请求」，Puffin `ask_key`）：它拿回答的键，其余传下去编辑输入框、滚动、换模式。
        if self.asking()
            && let Some(action) = self.ask_key(key, now)
        {
            return action;
        }
        // 扣住的同伴消息的批准（cross-session.md §2.6，`peer_view.rs`）：待回答的请求优先；照 Puffin 的请求拿键。
        if let Some(action) = self.peer_key(key, now) {
            return action;
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
        // 挑选回退的点（`rewind_pick.rs`）：esc、⏎、在草稿边上的 ↑ ↓ 归它，其余照常编辑草稿。
        if let Some(action) = self.picking_key(&key) {
            return action;
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
        let (ctrl, alt) = (keyboard::ctrl(key), keyboard::alt(key));
        let shift = mods.contains(KeyModifiers::SHIFT);
        let sup = mods.contains(KeyModifiers::SUPER);
        if matches!(key.code, KeyCode::Char('c' | 'C')) && !alt {
            if ctrl && !shift && !sup {
                let copied = self.pointer.selection.is_some_and(|s| s.copied.is_some())
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
            || (matches!(key.code, KeyCode::Home | KeyCode::End | KeyCode::Char('n')) && ctrl)
            || (arrows && (shift || alt || sup));
        // 全屏视图的选区按屏幕坐标记：翻页之后不再对应原来的文字，清掉（tui-select.md D3）。
        let scrolls_screen = self.pointer.screen_selection.is_some()
            && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown);
        if !keeps || scrolls_screen {
            self.pointer.clear_selection();
        }
        None
    }

    /// `now` 是这次按键的时钟（`on_key` 传下来；按键路径不读墙上的 `Instant::now()`，测试与孪生按它走）。
    pub(super) fn overlay_key(&mut self, key: KeyEvent, now: Instant) -> Option<Action> {
        if matches!(self.overlay, Some(Overlay::Diff(_))) {
            return self.diff_key(key);
        }
        if matches!(self.overlay, Some(Overlay::Trust(_))) {
            return self.trust_key(key, now);
        }
        if matches!(self.overlay, Some(Overlay::Usage(_))) {
            return self.usage_key(key);
        }
        if matches!(self.overlay, Some(Overlay::YoloConfirm(_))) {
            return self.yolo_confirm_key(key);
        }
        if matches!(self.overlay, Some(Overlay::Machines(_))) {
            return self.machines_key(key);
        }
        // `/theme` 移动时预览、esc 换回（tui-kit.md §4.2「`/theme`」，slash_theme.rs）。
        if matches!(&self.overlay, Some(Overlay::List(list)) if list.purpose == Purpose::Theme) {
            return self.theme_key(key);
        }
        match self.overlay.as_mut()? {
            Overlay::List(list) => match list.on_key(key) {
                Outcome::Pending => None,
                Outcome::Closed => {
                    self.overlay = None;
                    None
                }
                Outcome::Chosen { choice, level } => {
                    let purpose = list.purpose.clone();
                    self.overlay = None;
                    self.drop_list_draft(&purpose);
                    self.chosen(choice, level)
                }
            },
            Overlay::Shortcuts => {
                self.overlay = None;
                None
            }
            Overlay::Diff(_)
            | Overlay::Trust(_)
            | Overlay::Tasks(_)
            | Overlay::Usage(_)
            | Overlay::YoloConfirm(_)
            | Overlay::Machines(_)
            | Overlay::Card(_)
            | Overlay::Sessions(_)
            | Overlay::Points(_)
            | Overlay::Forks(_) => None,
        }
    }

    /// 选择器选中一项后的动作；`level` 是带档位的条目（模型）的焦点档位，`auto` 一档是去掉档位（slash.rs `open_models`）。
    fn chosen(&mut self, choice: Choice, level: Option<String>) -> Option<Action> {
        match choice {
            Choice::Close => None,
            Choice::Model(selection) => {
                let auto = level.as_deref() == Some(AUTO);
                let reasoning = level.filter(|level| !level.is_empty() && level != AUTO);
                // 状态栏的标签变成选的这个时淡入（Puffin `picked`，status_motion.rs）；选的就是在用的时不淡。
                let picked = (
                    selection.model.clone(),
                    reasoning.clone().unwrap_or_default(),
                );
                let current = self.view.selection();
                let same = current.model == picked.0
                    && current.reasoning.clone().unwrap_or_default() == picked.1;
                self.status_motion.picked = (!same).then_some(picked);
                Some(Action::ChooseModel {
                    selection: ModelSelection {
                        reasoning,
                        ..selection
                    },
                    auto,
                })
            }
            Choice::ExportCopy => Some(Action::Manage(super::manage::Manage::ExportCopy)),
            Choice::ExportFile => Some(Action::Manage(super::manage::Manage::ExportName)),
            Choice::Theme(id) => self.set_theme(&id),
            // 与带参数的命令走同一条路径（tui-display.md §4.3、§4.4）：计划模式那一行开关计划模式，其余切换（完全访问先确认）。
            Choice::Mode(PermissionMode::Plan) => self.toggle_plan(),
            Choice::Mode(mode) => self.switch_mode(mode, false),
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
            // 按字素删（架构整理 U12）：以前 `pop` 只删最后一个字符，组合字符与 emoji 剩下半个。
            KeyCode::Backspace => {
                super::line_edit::backspace(&mut query);
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
        // `@` 面板的键在 `at_view.rs`（照 Puffin `at_key`）。
        if matches!(self.composer.popup, Some(Popup::Files { .. })) {
            return self.at_key(key);
        }
        // `$` 面板的键在 `skill_view.rs`（照 Puffin `mention_key`）。
        if matches!(self.composer.popup, Some(Popup::Skills { .. })) {
            return self.skill_key(key);
        }
        match key.code {
            KeyCode::Up => self.composer.popup_move(false),
            KeyCode::Down => self.composer.popup_move(true),
            // esc 关上命令架、草稿留着，草稿空了之前不再打开（Puffin `shelf_key`）。
            KeyCode::Esc if matches!(self.composer.popup, Some(Popup::Slash { .. })) => {
                self.composer.close_shelf();
            }
            // 现在不能运行的命令：⏎ 什么都不做（Puffin `shelf_key`）。
            KeyCode::Enter
                if matches!(self.composer.popup, Some(Popup::Slash { selected })
                    if self.composer.slash_matches().get(selected)
                        .is_some_and(|e| super::shelf_rows::unavailable(self, e.name()).is_some())) =>
            {
                return Some(None);
            }
            KeyCode::Tab | KeyCode::Enter => {
                return Some(match self.composer.popup {
                    Some(Popup::Slash { .. }) => {
                        let name = self.composer.accept_slash()?;
                        // tab 补全（`/model`、`/theme` 上还打开它的列表，Puffin `shelf_key`；以前只补全，用户报告
                        // 2026-10-31）；enter 补全并执行（本工作线的命令参数都是可选的，tui.md §6）：TUI 命令在本地执行，
                        // skill 整行发给内核展开（与直接回车同一条路径）。
                        if key.code == KeyCode::Tab {
                            self.open_by_draft(&name).flatten()
                        } else {
                            self.submit()
                        }
                    }
                    Some(Popup::Files { .. } | Popup::Skills { .. }) | None => None,
                });
            }
            _ => return None,
        }
        Some(None)
    }
}
