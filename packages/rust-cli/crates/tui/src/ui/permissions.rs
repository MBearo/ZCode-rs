//! `/permissions`（docs/specs/rust-cli/tui-manage.md §1.2）：标签页、规则列表、输入新规则、保存位置与删除确认。
//! 规则来自内核的查询（[`PermissionsView::rules`] 是最后一次列表或修改的回执）；这里只有页面的界面状态。
//! 修改经 [`Outcome::Change`] 交给 runner，回执到了才回到列表（同时只有一次修改在途）。
use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_kernel_api::permission::{RuleBehavior, RuleChange, RuleSource, SourcedRule};
use rust_cli_runtime::format_permission_rule;

use super::app::{Action, App, Overlay};
use super::i18n::Lang;
use super::keyboard::Typing;
use super::manage::Manage;

/// 标签页（Allow / Ask / Deny / Workspace；没有 Recently denied 与 Auto mode，D3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Allow,
    Ask,
    Deny,
    Workspace,
}

pub(crate) const TABS: [Tab; 4] = [Tab::Allow, Tab::Ask, Tab::Deny, Tab::Workspace];

impl Tab {
    pub(crate) fn behavior(self) -> Option<RuleBehavior> {
        match self {
            Self::Allow => Some(RuleBehavior::Allow),
            Self::Ask => Some(RuleBehavior::Ask),
            Self::Deny => Some(RuleBehavior::Deny),
            Self::Workspace => None,
        }
    }
}

/// 页面的阶段。
#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    List,
    /// 输入新规则。
    Input(String),
    /// 保存位置：只有 `Project settings (local)`（D1）。
    Destination(String),
    /// 规则详情；`yes` 是删除确认的焦点（缺省在 `No`）。
    Details {
        rule: SourcedRule,
        yes: bool,
    },
    /// 修改在途：等回执。
    Saving,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionsView {
    pub tab: Tab,
    pub rules: Vec<SourcedRule>,
    /// 工作区根（Workspace 标签页）。
    pub workspace: String,
    pub selected: usize,
    /// 过滤的文字；None 表示没在过滤。
    pub filter: Option<String>,
    pub stage: Stage,
    /// 这次打开期间的结果（关闭时合成一条提示）。
    pub results: Vec<String>,
}

pub enum Outcome {
    Pending,
    Change(RuleChange),
    Close,
}

/// 规则文字。
pub(crate) fn rule_text(rule: &SourcedRule) -> String {
    format_permission_rule(&rule.rule)
}

/// 来源名。
pub(crate) fn source_name(source: RuleSource) -> &'static str {
    match source {
        RuleSource::UserSettings => "user settings",
        RuleSource::ProjectSettings => "shared project settings",
        RuleSource::LocalSettings => "project local settings",
        RuleSource::CliArg => "CLI argument",
        RuleSource::Session => "current session",
    }
}

pub(crate) fn behavior_name(behavior: RuleBehavior) -> &'static str {
    match behavior {
        RuleBehavior::Allow => "allow",
        RuleBehavior::Ask => "ask",
        RuleBehavior::Deny => "deny",
    }
}

/// 可以在页面里删除的来源（配置文件只读，D2）。
pub(crate) fn deletable(source: RuleSource) -> bool {
    matches!(
        source,
        RuleSource::LocalSettings | RuleSource::CliArg | RuleSource::Session
    )
}

impl PermissionsView {
    pub fn new(rules: Vec<SourcedRule>, workspace: String) -> Self {
        Self {
            tab: Tab::Allow,
            rules,
            workspace,
            selected: 0,
            filter: None,
            stage: Stage::List,
            results: Vec::new(),
        }
    }

    /// 这个标签页的规则，按规则文字的小写排序（`localeCompare`），过滤时只留包含过滤文字的。
    pub(crate) fn tab_rules(&self) -> Vec<&SourcedRule> {
        let Some(behavior) = self.tab.behavior() else {
            return Vec::new();
        };
        let query = self.filter.as_deref().unwrap_or("").to_lowercase();
        let mut rules: Vec<&SourcedRule> = self
            .rules
            .iter()
            .filter(|rule| rule.behavior == behavior)
            .filter(|rule| query.is_empty() || rule_text(rule).to_lowercase().contains(&query))
            .collect();
        rules.sort_by_key(|rule| rule_text(rule).to_lowercase());
        rules
    }

    /// 列表的条目数：没在过滤时第一项是 `Add a new rule…`。
    pub(crate) fn has_add(&self) -> bool {
        self.tab.behavior().is_some() && self.filter.is_none()
    }

    fn count(&self) -> usize {
        self.tab_rules().len() + usize::from(self.has_add())
    }

    /// 滚轮（tui-select.md §4.1）：列表里每格移动选中项 1 项，到头停住（↑ / ↓ 会绕回，滚轮不绕）。
    pub fn wheel(&mut self, step: isize) {
        if self.stage != Stage::List {
            return;
        }
        let last = self.count().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + step).clamp(0, last) as usize;
    }

    fn switch(&mut self, forward: bool) {
        let index = TABS.iter().position(|tab| *tab == self.tab).unwrap_or(0);
        let next = if forward {
            (index + 1) % TABS.len()
        } else {
            (index + TABS.len() - 1) % TABS.len()
        };
        self.tab = TABS[next];
        self.selected = 0;
        self.filter = None;
    }

    /// 一个按键；`typing` 按平台判断文字输入（Windows 的 AltGr，tui-input.md §9.1）。
    pub fn on_key(&mut self, key: KeyEvent, typing: Typing) -> Outcome {
        let ctrl = typing.ctrl(&key);
        if ctrl && key.code == KeyCode::Char('c') {
            return Outcome::Close;
        }
        let typed = typing.is_text(&key);
        match self.stage.clone() {
            Stage::List => self.list_key(key, typed),
            Stage::Input(mut text) => {
                match key.code {
                    KeyCode::Esc => self.stage = Stage::List,
                    KeyCode::Enter if !text.trim().is_empty() => {
                        self.stage = Stage::Destination(text.trim().to_owned());
                    }
                    KeyCode::Backspace => {
                        text.pop();
                        self.stage = Stage::Input(text);
                    }
                    KeyCode::Char(c) if typing.is_text(&key) => {
                        text.push(c);
                        self.stage = Stage::Input(text);
                    }
                    _ => {}
                }
                Outcome::Pending
            }
            Stage::Destination(text) => match key.code {
                KeyCode::Esc => {
                    self.stage = Stage::List;
                    Outcome::Pending
                }
                KeyCode::Enter | KeyCode::Char('1') => {
                    let Some(behavior) = self.tab.behavior() else {
                        return Outcome::Pending;
                    };
                    self.stage = Stage::Saving;
                    Outcome::Change(RuleChange::Add {
                        behavior,
                        rules: vec![text],
                    })
                }
                _ => Outcome::Pending,
            },
            Stage::Details { rule, yes } => {
                if !deletable(rule.source) {
                    if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                        self.stage = Stage::List;
                    }
                    return Outcome::Pending;
                }
                match key.code {
                    KeyCode::Esc | KeyCode::Char('n') => self.stage = Stage::List,
                    KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Tab
                    | KeyCode::BackTab => self.stage = Stage::Details { rule, yes: !yes },
                    KeyCode::Char('y') => {
                        self.stage = Stage::Saving;
                        return Outcome::Change(RuleChange::Remove { rule });
                    }
                    KeyCode::Enter if yes => {
                        self.stage = Stage::Saving;
                        return Outcome::Change(RuleChange::Remove { rule });
                    }
                    KeyCode::Enter => self.stage = Stage::List,
                    _ => {}
                }
                Outcome::Pending
            }
            Stage::Saving => Outcome::Pending,
        }
    }

    /// `typed`：这个键是文字输入（`Typing::is_text`）。
    fn list_key(&mut self, key: KeyEvent, typed: bool) -> Outcome {
        let count = self.count();
        match key.code {
            KeyCode::Esc if self.filter.is_some() => {
                self.filter = None;
                self.selected = 0;
            }
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Left | KeyCode::BackTab => self.switch(false),
            KeyCode::Right | KeyCode::Tab => self.switch(true),
            KeyCode::Up | KeyCode::Char('k') if count > 0 => {
                self.selected = (self.selected + count - 1) % count;
            }
            KeyCode::Down | KeyCode::Char('j') if count > 0 => {
                self.selected = (self.selected + 1) % count;
            }
            KeyCode::Enter => {
                if self.has_add() && self.selected == 0 {
                    self.stage = Stage::Input(String::new());
                } else if let Some(rule) = self
                    .tab_rules()
                    .get(self.selected - usize::from(self.has_add()))
                {
                    self.stage = Stage::Details {
                        rule: (*rule).clone(),
                        yes: false,
                    };
                }
            }
            KeyCode::Backspace => {
                if let Some(filter) = self.filter.as_mut() {
                    filter.pop();
                    if filter.is_empty() {
                        self.filter = None;
                    }
                    self.selected = 0;
                }
            }
            // 可见字符（j、k 除外）开始过滤。
            KeyCode::Char(c) if typed && self.tab.behavior().is_some() => {
                self.filter.get_or_insert_with(String::new).push(c);
                self.selected = 0;
            }
            _ => {}
        }
        Outcome::Pending
    }

    /// 修改的回执：成功时换上新列表并记下结果，失败时记下原因；都回到列表。
    pub fn changed(
        &mut self,
        change: &RuleChange,
        result: Result<Vec<SourcedRule>, String>,
        lang: Lang,
    ) {
        let text = match change {
            RuleChange::Add { rules, .. } => rules
                .first()
                .map(|text| format_permission_rule(&rust_cli_runtime::parse_permission_rule(text)))
                .unwrap_or_default(),
            RuleChange::Remove { rule } => rule_text(rule),
        };
        let line = match (change, &result) {
            (RuleChange::Add { behavior, .. }, Ok(_)) => {
                super::permissions_lines::added(lang, behavior_name(*behavior), &text)
            }
            (RuleChange::Remove { rule }, Ok(_)) => {
                super::permissions_lines::deleted(lang, behavior_name(rule.behavior), &text)
            }
            (RuleChange::Add { .. }, Err(reason)) => {
                super::permissions_lines::add_failed(lang, &text, reason)
            }
            (RuleChange::Remove { .. }, Err(_)) => {
                super::permissions_lines::delete_failed(lang, &text)
            }
        };
        self.results.push(line);
        if let Ok(rules) = result {
            self.rules = rules;
        }
        self.stage = Stage::List;
        self.selected = self.selected.min(self.count().saturating_sub(1));
    }
}

impl App {
    /// `/permissions` 的列表到了：打开页面，或在页面开着时换上新列表。
    pub fn open_permissions(&mut self, rules: Vec<SourcedRule>) {
        self.overlay = Some(Overlay::Permissions(Box::new(PermissionsView::new(
            rules,
            self.workspace().to_owned(),
        ))));
    }

    /// 修改的回执（runner 调用）。
    pub fn permissions_changed(
        &mut self,
        change: &RuleChange,
        result: Result<Vec<SourcedRule>, String>,
    ) {
        let lang = self.lang;
        if let Some(Overlay::Permissions(view)) = self.overlay.as_mut() {
            view.changed(change, result, lang);
        }
    }

    pub(super) fn permissions_key(&mut self, key: KeyEvent) -> Option<Action> {
        let Some(Overlay::Permissions(view)) = self.overlay.as_mut() else {
            return None;
        };
        match view.on_key(key, self.typing) {
            Outcome::Pending => None,
            Outcome::Change(change) => Some(Action::Manage(Manage::ChangeRule(change))),
            Outcome::Close => {
                let results = std::mem::take(&mut view.results);
                self.overlay = None;
                if results.is_empty() {
                    self.info(super::permissions_lines::dismissed(self.lang));
                } else {
                    self.info(results.join("\n"));
                }
                None
            }
        }
    }
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
