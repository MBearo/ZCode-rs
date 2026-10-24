// 复制自 apps/zcode-host/crates/tui/src/ui/list.rs（feat/rs 79b6691c55）。
//! 通用选择列表（docs/specs/rust-tui.md §7.3、§8.5）：模型、权限、恢复会话、登录方式等选择器共用。
//! 第一行是强调色的 `─` 线，标题粗体，可选的副标题 dim；焦点行按选择列表组件画（`› ` 加底色条，
//! docs/specs/rust-cli/tui.md §7.4，修复线 F26；原来是强调色的 `❯ `），当前值后面绿色的 ` ✔`；
//! 上下移动到头时循环，数字 1–9 直接选中；滚轮移动焦点、到头停住；最后一行是提示。可输入文字过滤
//! （模型、恢复会话）。模型列表每个模型一行，←/→ 调焦点模型的推理档位。
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use rust_cli_kernel_api::permission::PermissionMode;

use super::i18n::Lang;
use super::key_hint;
use super::keyboard::Typing;
use super::palette::Palette;
use super::pick;
use super::text::truncate_line;

/// 分隔选择值里的 provider / model / 档位（不会出现在 id 里）。
pub const SEP: char = '\u{1f}';

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Item {
    pub label: String,
    pub description: String,
    pub current: bool,
    /// 选中后交给调用方的值。
    pub value: String,
    /// 推理档位（从低到高）与当前选中的一档；只有模型列表用。
    pub levels: Vec<String>,
    pub level: usize,
}

/// 列表的用途：决定选中后做什么。
#[derive(Debug, Clone, PartialEq)]
pub enum Purpose {
    Model,
    /// `/mode` 不带参数的选择器（修复线 F26）：值是 Node 的模式名，选中后同 `/mode <名>`。工作线 F76：照 App 的菜单
    /// 每个模式两行（名称、说明），名称用模式的颜色。
    Permissions,
    /// `/effort` 不带参数的选择器（修复线 F26）：值是档位或 `auto`，选中后同 `/effort <档>`。
    Effort,
    /// rewind 选择器的消息列表：每条两行（消息、这一回合的改动）。
    Rewind,
    /// rewind 的确认选项；值是 `范围 SEP 消息 ID`。
    RewindConfirm,
    /// 只读信息：enter 与 esc 都关闭。
    Info,
    /// `/memory` 的文件（tui-input.md §4.2）；值是文件在列表里的下标。
    Memory,
    /// `/fork` 的用户输入列表（session-lifecycle.md §6.5.3）：每条一行；值是消息 ID。
    Fork,
    /// `/theme`：深色 / 浅色。
    Theme,
}

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
}

pub enum Outcome {
    Pending,
    Chosen(String),
    Closed,
}

/// 档位的图标：从低到高 `○ ◐ ● ◉`。
fn effort_glyph(level: usize, count: usize) -> &'static str {
    const GLYPHS: [&str; 4] = ["○", "◐", "●", "◉"];
    if count <= 1 {
        return GLYPHS[2];
    }
    GLYPHS[((level * 3) as f64 / (count - 1) as f64).round() as usize]
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
        }
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

    /// 选中条目的值；模型列表后面接上选中的档位。
    fn chosen(item: &Item) -> String {
        match item.levels.get(item.level) {
            Some(level) => format!("{}{SEP}{level}", item.value),
            None if item.levels.is_empty() && item.value.contains(SEP) => {
                format!("{}{SEP}", item.value)
            }
            None => item.value.clone(),
        }
    }

    /// 焦点条目在 `items` 里的下标。
    fn focused(&self) -> Option<usize> {
        let item = *self.visible().get(self.selected)?;
        self.items.iter().position(|i| std::ptr::eq(i, item))
    }

    /// 一个按键；`typing` 按平台判断文字输入（Windows 的 AltGr，tui-input.md §9.1）。
    pub fn on_key(&mut self, key: KeyEvent, typing: Typing) -> Outcome {
        let count = self.visible().len();
        let ctrl = typing.ctrl(&key);
        let step = |selected: usize, down: bool| match count {
            0 => 0,
            _ if down => (selected + 1) % count,
            _ => (selected + count - 1) % count,
        };
        match key.code {
            KeyCode::Esc => return Outcome::Closed,
            KeyCode::Char('c') if ctrl => return Outcome::Closed,
            KeyCode::Up => self.selected = step(self.selected, false),
            KeyCode::Char('p') if ctrl => self.selected = step(self.selected, false),
            KeyCode::Down => self.selected = step(self.selected, true),
            KeyCode::Char('n') if ctrl => self.selected = step(self.selected, true),
            KeyCode::Left | KeyCode::Right if self.purpose == Purpose::Model => {
                if let Some(index) = self.focused() {
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
                    .map_or(Outcome::Pending, |item| Outcome::Chosen(Self::chosen(item)));
            }
            KeyCode::Backspace => {
                if let Some(filter) = self.filter.as_mut() {
                    filter.pop();
                    self.selected = 0;
                }
            }
            KeyCode::Char(c) if typing.is_text(&key) => {
                if let Some(filter) = self.filter.as_mut() {
                    filter.push(c);
                    self.selected = 0;
                } else if let Some(digit) = c.to_digit(10).filter(|d| (1..=9).contains(d)) {
                    let index = digit as usize - 1;
                    if let Some(item) = self.visible().get(index)
                        && self.purpose != Purpose::Info
                    {
                        return Outcome::Chosen(Self::chosen(item));
                    }
                }
            }
            _ => {}
        }
        Outcome::Pending
    }

    /// 视图的行与每个条目所在的行；`height` 是可用高度，列表只显示焦点附近的条目。
    pub fn lines(
        &self,
        width: usize,
        height: usize,
        lang: Lang,
        palette: &Palette,
    ) -> (Vec<Line<'static>>, Vec<(usize, usize)>) {
        let accent = palette.accent;
        let dim = Style::default().add_modifier(Modifier::DIM);
        let secondary = palette.secondary();
        let mut rows = Vec::new();
        let mut lines = vec![
            Line::from(Span::styled("─".repeat(width), Style::default().fg(accent))),
            Line::from(Span::styled(
                format!("  {}", self.title),
                Style::default().add_modifier(Modifier::BOLD),
            )),
        ];
        // 副标题可以有多行（rewind 的确认页）。
        for line in self.subtitle.iter().flat_map(|subtitle| subtitle.lines()) {
            lines.push(truncate_line(
                Line::from(Span::styled(format!("  {line}"), dim)),
                width,
            ));
        }
        if let Some(filter) = &self.filter {
            let search = match lang {
                Lang::En => "Search…",
                Lang::Zh => "搜索…",
            };
            let shown = if filter.is_empty() {
                Span::styled(search, dim)
            } else {
                Span::raw(filter.clone())
            };
            lines.push(Line::from(vec![Span::styled("  ⌕ ", dim), shown]));
        }
        lines.push(Line::default());
        let visible = self.visible();
        if visible.is_empty() {
            // 有条目但搜索没有匹配：不用列表为空时的说法（模型的是“去登录”）。
            let empty = match (self.items.is_empty(), lang) {
                (true, _) => self.empty.as_str(),
                (false, Lang::En) => "No matches",
                (false, Lang::Zh) => "没有匹配",
            };
            lines.push(Line::from(Span::styled(format!("  {empty}"), dim)));
        }
        // 每条两行：rewind（消息、改动）与 `/mode`（名称、说明，工作线 F76）；`/mode` 的 ✔ 在名称那一行。
        let modes = self.purpose == Purpose::Permissions;
        let two_lines = self.purpose == Purpose::Rewind || modes;
        let ticked = |item: &Item| item.current && (!two_lines || modes);
        let per_item = if two_lines { 2 } else { 1 };
        let focused = visible.get(self.selected).copied();
        let effort = focused.filter(|item| !item.levels.is_empty());
        // 列表之外还有：空行、档位行与空行（模型）、提示。
        let below = 2 + if effort.is_some() { 2 } else { 0 };
        let room = (height.saturating_sub(lines.len() + below) / per_item).max(3);
        let start = self.selected.saturating_sub(room.saturating_sub(1));
        let end = (start + room).min(visible.len());
        let label_width = visible
            .iter()
            .map(|item| item.label.width() + if ticked(item) { 2 } else { 0 })
            .max()
            .unwrap_or(0)
            .min(40);
        let green = Style::default().fg(Color::Green);
        for (index, item) in visible.iter().enumerate().take(end).skip(start) {
            let chosen = index == self.selected;
            // 指针列：焦点行 `›`（选择列表组件）；列表超出时首尾两行写 `↑` / `↓`。
            let pointer = if chosen {
                pick::pointer(true)
            } else if index == start && start > 0 {
                Span::styled("↑ ", dim)
            } else if index + 1 == end && end < visible.len() {
                Span::styled("↓ ", dim)
            } else {
                pick::pointer(false)
            };
            let mut spans = vec![Span::raw("  "), pointer];
            if self.filter.is_none() {
                spans.push(Span::styled(format!("{}. ", index + 1), secondary));
            }
            let label = if modes {
                let mode =
                    PermissionMode::from_node_name(&item.value).unwrap_or(PermissionMode::Default);
                super::footer::mode_style(mode, palette)
            } else {
                Style::default()
            };
            spans.push(Span::styled(item.label.clone(), label));
            let mut used = item.label.width();
            if ticked(item) {
                spans.push(Span::styled(" ✔", green));
                used += 2;
            }
            if !two_lines && width >= 24 && !item.description.is_empty() {
                spans.push(Span::raw(" ".repeat(label_width.saturating_sub(used) + 3)));
                spans.push(Span::styled(item.description.clone(), secondary));
            }
            rows.push((lines.len(), index));
            lines.push(pick::paint(
                truncate_line(Line::from(spans), width),
                chosen,
                palette,
                width,
            ));
            if two_lines {
                let mut meta = item.description.clone();
                if item.current && !modes {
                    meta.push_str(match lang {
                        Lang::En => " · current",
                        Lang::Zh => " · 当前",
                    });
                }
                let indent = if self.filter.is_none() { 7 } else { 4 };
                lines.push(pick::paint(
                    truncate_line(
                        Line::from(Span::styled(
                            format!("{}{meta}", " ".repeat(indent)),
                            secondary,
                        )),
                        width,
                    ),
                    chosen,
                    palette,
                    width,
                ));
            }
        }
        lines.push(Line::default());
        if let Some(item) = effort {
            let level = &item.levels[item.level.min(item.levels.len() - 1)];
            let glyph = effort_glyph(item.level, item.levels.len());
            let text = match lang {
                Lang::En => format!(" {level} effort  ←/→ to adjust"),
                Lang::Zh => format!(" 推理档位 {level}  ←/→ 调整"),
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(glyph, Style::default().fg(accent)),
                Span::styled(text, dim),
            ]));
            lines.push(Line::default());
        }
        let hint = match lang {
            Lang::En => "{Enter} to select · {Esc} to cancel",
            Lang::Zh => "{enter} 选择 · {esc} 取消",
        };
        lines.push(key_hint::line(2, hint, palette));
        (lines, rows)
    }
}

#[cfg(test)]
#[path = "list_tests.rs"]
mod tests;
