//! 旧界面的 `@` 面板（docs/puffin-tui.md §4.2「提及（@）」；照 Puffin `shell/at/handlers.rs` 的 `at_panel`、`at_chosen`、
//! `at_key`、`settle_at` 与 `shell/at/rows.rs`）：从输入框的状态（`composer_files.rs`）与这个对话的工具行建组，键、画之前的
//! 淡入与要读的预览、槽的键，行由 kit 的 `at_panel::rows` 画。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_tui_kit::app::element::Element;
use rust_cli_tui_kit::app::widgets::at_panel::rows::{self as kit, Ctx, PATH_ROWS, WIDE};
use rust_cli_tui_kit::app::widgets::at_panel::{AtItem, AtPanel, Touch};
use rust_cli_tui_kit::app::widgets::key_hints;
use rust_cli_tui_kit::app::widgets::lists::window;

use super::app::{Action, App};
use super::at_panel::{self, MAX_ROWS, NARROW_MAX_ROWS, Sources};
use super::composer::Popup;
use super::i18n::Lang;

impl App {
    /// 这个对话的工具行读过或改过的文件，新的在前。
    fn at_touched(&self) -> Vec<(String, Touch)> {
        let tools = self
            .view
            .rows()
            .iter()
            .filter_map(|row| row.tool())
            .map(|tool| (tool.name.as_str(), &tool.input));
        at_panel::touched(tools, self.view.workspace().root.as_str())
    }

    /// `@` 面板开着时它的组（终端宽 `width`）。
    pub fn at_panel(&self, width: usize) -> Option<AtPanel> {
        if !matches!(self.composer.popup, Some(Popup::Files { .. })) {
            return None;
        }
        let touched = self.at_touched();
        let files = &self.composer.files;
        let sources = Sources {
            files: files.found.as_ref(),
            touched: &touched,
            changed: &files.changed,
            conversations: files.conversations.as_deref(),
            current: self.session.as_deref(),
            lang: self.lang,
        };
        let max = if width >= WIDE {
            MAX_ROWS
        } else {
            NARROW_MAX_ROWS
        };
        Some(at_panel::panel(
            files.word.as_deref().unwrap_or(""),
            &sources,
            max,
        ))
    }

    fn at_choice(&self) -> usize {
        match self.composer.popup {
            Some(Popup::Files { selected }) => selected,
            _ => 0,
        }
    }

    /// 开着的面板选中的项。
    fn at_chosen(&self, width: usize) -> Option<AtItem> {
        let p = self.at_panel(width)?;
        let es = p.entries();
        let last = es.len().checked_sub(1)?;
        es.get(self.at_choice().min(last)).map(|it| (*it).clone())
    }

    /// 开着的 `@` 面板拿的键；`None` 把键传下去（没有可选的项时 ⏎ 照常发送）。
    pub fn at_key(&mut self, key: KeyEvent) -> Option<Option<Action>> {
        let p = self.at_panel(self.composer.files.width)?;
        let es: Vec<AtItem> = p.entries().into_iter().cloned().collect();
        let n = es.len();
        let chosen = self.at_choice().min(n.saturating_sub(1));
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let pick = |choice: usize, app: &mut App| {
            app.composer.popup = Some(Popup::Files { selected: choice });
        };
        match key.code {
            KeyCode::Esc => self.composer.close_files(),
            KeyCode::Up if n > 0 => pick((chosen + n - 1) % n, self),
            KeyCode::Char('p') if ctrl && n > 0 => pick((chosen + n - 1) % n, self),
            KeyCode::Down if n > 0 => pick((chosen + 1) % n, self),
            KeyCode::Char('n') if ctrl && n > 0 => pick((chosen + 1) % n, self),
            KeyCode::Tab | KeyCode::Enter if n > 0 => self.composer.insert_at(&es[chosen]),
            _ => return None,
        }
        Some(None)
    }

    /// 画之前（`pane.rs`）：面板这一帧打开了 → 整块从这个 tick 淡入、预览重读、runner 重读 git 的改动；选中的项换了 → 右栏
    /// 淡入；晚到的文件 → 组名下淡入；选中的路径还没读 → 等 runner 读（Puffin `settle_at` 与 `at_answered`）。
    pub fn note_at(&mut self, width: usize, tick: i64) {
        let chosen = self.at_chosen(width);
        let open = matches!(self.composer.popup, Some(Popup::Files { .. }));
        let files = &mut self.composer.files;
        if open && !files.seen.0 {
            files.opened_at = tick;
            files.peeks.clear();
            files.peeking.clear();
            files.reopened = true;
        }
        if std::mem::take(&mut files.late) {
            files.files_at = tick;
        }
        if chosen.is_some() && chosen != files.seen.1 {
            files.shown_at = tick;
        }
        if let Some(AtItem::Path { path, .. }) = &chosen
            && !files.peeks.contains_key(path)
            && !files.peeking.contains(path)
        {
            files.peeking.insert(path.clone());
            files.peek = Some(path.clone());
        }
        files.seen = (open, chosen);
        files.width = width;
    }

    /// 面板的行与每一行对应的项（点它等于选中再按 ⏎）。
    pub fn at_lines(&self, width: usize, tick: i64, now_ms: u64) -> Vec<(Element, Option<usize>)> {
        let Some(p) = self.at_panel(width) else {
            return Vec::new();
        };
        let touched = self.at_touched();
        let files = &self.composer.files;
        let touch = at_panel::touches(&Sources {
            files: files.found.as_ref(),
            touched: &touched,
            changed: &files.changed,
            conversations: None,
            current: None,
            lang: self.lang,
        });
        let ctx = Ctx {
            peeks: &files.peeks,
            touch: &touch,
            choice: self.at_choice(),
            opened_age: tick - files.opened_at,
            files_age: tick - files.opened_at.max(files.files_at),
            shown_age: tick - files.shown_at,
            now: now_ms as f64,
            offset: &|_| 0,
        };
        let words = at_panel::words(self.lang);
        let rows = kit::rows_said(&self.ink.theme, &p, &ctx, width, &words);
        let targets = targets(&p, self.at_choice(), width, |path| {
            files.peeks.contains_key(path)
        });
        rows.into_iter()
            .enumerate()
            .map(|(i, row)| (row, targets.get(i).copied().flatten()))
            .collect()
    }

    /// 槽的键（Puffin `at::keys` 经 `key_hints::pick`）。
    /// 面板、晚到的组或预览还在淡入：按动画节拍重画。
    pub fn at_moving(&self, tick: i64) -> bool {
        let f = &self.composer.files;
        tick - f.opened_at.max(f.shown_at).max(f.files_at)
            < rust_cli_tui_kit::app::widgets::fade::TICKS
    }

    pub fn at_slot(&self, width: usize) -> Vec<(String, String)> {
        let Some(p) = self.at_panel(width) else {
            return Vec::new();
        };
        key_hints::pick(&kit::keys(&p, self.at_choice()))
            .into_iter()
            .map(|(k, v)| (k, slot_word(self.lang, &v)))
            .collect()
    }
}

/// 槽的标签的中文。
pub(super) fn slot_word(lang: Lang, word: &str) -> String {
    let zh = match word {
        "choose" => "选择",
        "insert" => "插入",
        "open folder" => "打开文件夹",
        "close" => "关闭",
        "send" => "发送",
        other => other,
    };
    match lang {
        Lang::En => word.to_owned(),
        Lang::Zh => zh.to_owned(),
    }
}

/// kit 画出的每一行对应的项（与 `rows` 的次序相同：没有匹配的一行、每组的组名、在找、↑、项、↓、多出的；窄的终端在选中的
/// 行下面展开路径与事实）。
fn targets(
    p: &AtPanel,
    choice: usize,
    width: usize,
    peeked: impl Fn(&str) -> bool,
) -> Vec<Option<usize>> {
    let es = p.entries();
    let c = (!es.is_empty()).then(|| choice.min(es.len() - 1));
    let mut out: Vec<Option<usize>> = Vec::new();
    if es.is_empty() && !p.loading() {
        out.push(None);
    }
    let mut k = 0;
    for g in &p.groups {
        out.push(None);
        if g.loading {
            out.push(None);
        }
        let (from, until) = if p.single() {
            window(g.items.len(), c.unwrap_or(0).saturating_sub(k), PATH_ROWS)
        } else {
            (0, g.items.len())
        };
        if from > 0 {
            out.push(None);
        }
        out.extend((from..until).map(|i| Some(k + i)));
        if until < g.items.len() {
            out.push(None);
        }
        if g.more > 0 {
            out.push(None);
        }
        k += g.items.len();
    }
    if width < WIDE
        && let Some(c) = c
        && let Some(at) = out.iter().position(|t| *t == Some(c))
    {
        let extra = match es[c] {
            AtItem::Path { path, .. } => 1 + usize::from(peeked(path)),
            _ => 1,
        };
        for _ in 0..extra {
            out.insert(at + 1, None);
        }
    }
    out
}
