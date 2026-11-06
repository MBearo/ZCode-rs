// 复制自 apps/zcode-host/crates/tui/src/ui/list.rs（feat/rs 79b6691c55）。
//! 通用选择列表（docs/specs/rust-tui.md §7.3、§8.5）：模型、权限、恢复会话、登录方式等选择器共用。
//! 上下移动到头时循环，数字 1–9 直接选中；滚轮移动焦点、到头停住；最后一行是提示。可输入文字过滤
//! （模型、恢复会话）。模型列表每个模型一行，←/→ 调焦点模型的推理档位。
//!
//! 画法：[`ListView::lines`] 在 `list_draw.rs`（[`draw`]），照 Puffin 输入框的板上的那一层画（tui-kit.md §4.2，工作线 TK
//! 的 S3）；这里只有条目、焦点与按键（行为不变）。
use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;

use super::keyboard;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Item {
    pub label: String,
    pub description: String,
    pub current: bool,
    /// 选中后交给调用方的值。
    pub value: Choice,
    /// 推理档位（从低到高）与当前选中的一档；只有模型列表用。
    pub levels: Vec<String>,
    pub level: usize,
    /// 行尾 fg3 的标签（`/mode` 计划模式那一行的 `on` / `off`）；空时在用的一项写 `current`。
    pub tag: String,
}

/// 选中一项后交给调用方的值（架构整理 U4：以前是用分隔符拼的字符串，模型的团队项目先序列化成 JSON，选中后再拆开
/// 解析）。只有下面几种列表用 `ListView`；rewind、`/fork`、`/memory`、`/effort` 各有自己的界面。
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Choice {
    /// 只关闭列表（只读信息的条目）。
    #[default]
    Close,
    /// `/model` 的一个模型；档位由列表的 ←/→ 给出（[`Outcome::Chosen`] 的 `level`，`auto` 也在其中）。
    Model(ModelSelection),
    /// `/mode` 不带参数的选择器（修复线 F26）：选中后同 `/mode <名>`。
    Mode(PermissionMode),
    /// `/theme` 的一个配色（kit 配色的 id）。
    Theme(String),
    /// `/export` 的复制到剪贴板。
    ExportCopy,
    /// `/export` 的存成文件（草稿变成 `/export <缺省文件名>`）。
    ExportFile,
}

/// 列表是哪一个：决定画法（`list_draw.rs`）与跟着草稿的前缀（`draft_layer.rs`）；选中后做什么由 [`Choice`] 决定。
#[derive(Debug, Clone, PartialEq)]
pub enum Purpose {
    Model,
    /// `/mode` 不带参数的选择器（修复线 F26）。工作线 F76：照 App 的菜单每个模式两行（名称、说明），名称用模式的颜色。
    Permissions,
    /// 只读信息：enter 与 esc 都关闭。
    Info,
    /// `/theme`：配色。
    Theme,
    /// `/export` 的两种方式（Puffin `export_rows`）。草稿正好是 `/export ` 时开着。
    Export,
}

/// 早已落定的淡入的 tick 数（同 Puffin 的 `LONG_AGO`）。
const SETTLED: i64 = 1_000_000;

#[derive(Debug, Clone, PartialEq)]
pub struct ListView {
    pub purpose: Purpose,
    pub title: String,
    pub subtitle: Option<String>,
    pub items: Vec<Item>,
    pub selected: usize,
    /// 可过滤时的查询；None 表示不可过滤。
    pub filter: Option<String>,
    pub empty: String,
    /// 模型选择器上一帧显示的档位与它换上的 tick（画之前记，[`ListView::note_level`]；Puffin `Picker::named`）；没画过时
    /// None。`named_age` 是这一帧离它多少 tick：档位的名字从板上淡入。
    pub named: Option<(Option<String>, i64)>,
    pub named_age: i64,
}

pub enum Outcome {
    Pending,
    /// 选中的值与焦点条目的档位（只有带档位的条目有）。
    Chosen {
        choice: Choice,
        level: Option<String>,
    },
    Closed,
}

impl ListView {
    pub fn new(purpose: Purpose, title: String, items: Vec<Item>, empty: String) -> Self {
        let selected = items.iter().position(|item| item.current).unwrap_or(0);
        Self {
            purpose,
            title,
            subtitle: None,
            items,
            selected,
            filter: None,
            empty,
            named: None,
            named_age: SETTLED,
        }
    }

    /// 画之前（`pane.rs`）：模型选择器显示的档位换了（换了模型或 ← →）时它的名字从这个 tick 淡入；打开时已经落定
    /// （Puffin `renamed`）。
    pub fn note_level(&mut self, tick: i64) {
        if self.purpose != Purpose::Model {
            return;
        }
        let shown = self.visible().get(self.selected).and_then(|item| {
            let last = item.levels.len().checked_sub(1)?;
            item.levels.get(item.level.min(last)).cloned()
        });
        let named = match self.named.take() {
            Some((was, at)) if was == shown => (was, at),
            Some(_) => (shown, tick),
            None => (shown, tick - SETTLED),
        };
        self.named_age = tick - named.1;
        self.named = Some(named);
    }

    pub fn with_subtitle(mut self, subtitle: String) -> Self {
        self.subtitle = Some(subtitle);
        self
    }

    /// 可过滤：焦点仍在当前值上，输入后回到第一条。
    pub fn filterable(mut self) -> Self {
        self.filter = Some(String::new());
        self
    }

    /// 滚轮：焦点移动 `rows` 行（正数向下），到头停住、不循环。
    pub fn wheel(&mut self, rows: isize) {
        let last = self.visible().len().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + rows).clamp(0, last) as usize;
    }

    pub fn visible(&self) -> Vec<&Item> {
        let query = self.filter.as_deref().unwrap_or("").to_lowercase();
        self.items
            .iter()
            .filter(|item| {
                query.is_empty()
                    || item.label.to_lowercase().contains(&query)
                    || item.description.to_lowercase().contains(&query)
            })
            .collect()
    }

    /// 选中这个条目：值与焦点的档位。
    fn chosen(item: &Item) -> Outcome {
        Outcome::Chosen {
            choice: item.value.clone(),
            level: item.levels.get(item.level).cloned(),
        }
    }

    /// 焦点条目在 `items` 里的下标。
    fn focused(&self) -> Option<usize> {
        let item = *self.visible().get(self.selected)?;
        self.items.iter().position(|i| std::ptr::eq(i, item))
    }

    /// 一个按键（入口已归一 Windows 的 AltGr，tui-input.md §9.1）。
    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        let count = self.visible().len();
        let ctrl = keyboard::ctrl(&key);
        let step = |selected: usize, down: bool| super::pager::cycle(selected, count, down);
        match key.code {
            KeyCode::Esc => return Outcome::Closed,
            KeyCode::Char('c') if ctrl => return Outcome::Closed,
            KeyCode::Up => self.selected = step(self.selected, false),
            KeyCode::Char('p') if ctrl => self.selected = step(self.selected, false),
            KeyCode::Down => self.selected = step(self.selected, true),
            KeyCode::Char('n') if ctrl => self.selected = step(self.selected, true),
            // 带档位的条目（模型）：←/→ 调焦点条目的档位。
            KeyCode::Left | KeyCode::Right => {
                if let Some(index) = self.focused()
                    && !self.items[index].levels.is_empty()
                {
                    let item = &mut self.items[index];
                    let last = item.levels.len().saturating_sub(1);
                    item.level = if key.code == KeyCode::Left {
                        item.level.saturating_sub(1)
                    } else {
                        (item.level + 1).min(last)
                    };
                }
            }
            KeyCode::Enter => {
                if self.purpose == Purpose::Info {
                    return Outcome::Closed;
                }
                return self
                    .visible()
                    .get(self.selected)
                    .map_or(Outcome::Pending, |item| Self::chosen(item));
            }
            KeyCode::Backspace => {
                if let Some(filter) = self.filter.as_mut() {
                    crate::ui::line_edit::backspace(filter);
                    self.selected = 0;
                }
            }
            KeyCode::Char(c) if keyboard::is_text(&key) => {
                if let Some(filter) = self.filter.as_mut() {
                    filter.push(c);
                    self.selected = 0;
                } else if ('1'..='9').contains(&c) {
                    let index = usize::from(c as u8 - b'1');
                    if let Some(item) = self.visible().get(index)
                        && self.purpose != Purpose::Info
                    {
                        return Self::chosen(item);
                    }
                }
            }
            _ => {}
        }
        Outcome::Pending
    }

    /// 粘贴进过滤（不可过滤的列表丢掉）；焦点回到第一条。
    pub fn paste(&mut self, text: &str) {
        if let Some(filter) = self.filter.as_mut() {
            super::line_edit::paste(filter, text);
            self.selected = 0;
        }
    }
}

#[path = "list_draw.rs"]
pub mod draw;

#[cfg(test)]
#[path = "list_tests.rs"]
mod tests;
