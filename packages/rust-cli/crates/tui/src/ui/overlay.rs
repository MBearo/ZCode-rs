//! 替换输入框或整个屏幕的视图（`Overlay`）与它的所有分派：按键、粘贴、滚轮、底部区域的行与全屏的绘制。
//!
//! 架构整理 U1：以前每种分派各是一条 `if matches!(…)` 链，散在 keys、pane、wheel、mouse、draw 几个文件里，加一个
//! 视图要改 3–6 处，漏了不报错（落到别的分支）。现在每种分派是一个穷举 `match`，都在这里。全屏视图的绘制取出来画、
//! 画完放回：以前每帧先复制整个视图（`/resume` 的全屏对话连同整份 transcript，1000 轮对话的会话每帧 0.21 ms，
//! debug 构建），`Overlay` 因此不再实现 `Clone`。
use std::time::Instant;

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::text::Line;

use super::app::{Action, App, Local};
use super::list::ListView;
use super::mouse::Target;
use super::pane::{RowTarget, ViewLines};
use super::transcript::Cache;

/// 替换输入框（底部区域）或整个屏幕的视图。不实现 `Clone`：绘制取出来画、画完放回（不每帧复制）。
#[derive(Debug, PartialEq)]
pub enum Overlay {
    List(ListView),
    /// 独立页面（`/status`、`/help`）：内容不插进 transcript，esc 关闭。
    Page {
        local: Local,
        scroll: usize,
    },
    Shortcuts,
    /// ctrl+o：详细视图。
    Detail(super::detail::Detail),
    /// `/login`：登录视图（tui.md §7.2）。
    Login(super::login::LoginView),
    /// `/permissions`：规则页面（tui-manage.md §1.2）。装箱：比其他变体大得多。
    Permissions(Box<super::permissions::PermissionsView>),
    /// `/export`：导出方式与文件名（tui-manage.md §3）。
    Export(super::export::ExportView),
    /// `/diff`：当前改动与每回合的改动（tui-manage.md §4）。
    Diff(super::diff_view::DiffView),
    /// ← 项目页（修复线 F29，tui-panels.md §2）。装箱：比其他变体大得多。
    Projects(Box<super::projects::ProjectsPage>),
    /// `/resume` 页（修复线 F29，tui-manage.md §10）。
    Resume(Box<super::resume::ResumePage>),
    /// 切到别的项目之前的信任对话框（workspace-trust.md §2.8）。
    Trust(Box<super::trust_ask::TrustAsk>),
    /// Background 面板与 Shell 详情（tui-panels.md §3）：替换底部区域。
    Background(super::background::BackgroundView),
    /// `/usage` 的 Usage / Stats 页面（usage.md §4、§5）。装箱：统计数据比其他变体大得多。
    Usage(Box<super::usage::UsageView>),
    /// 切到完全访问的确认框（tui.md §7.8，工作线 F87）：替换底部区域，不是全屏视图。
    YoloConfirm(super::yolo_confirm::YoloConfirm),
    /// `/machines` 的列表（远端执行 R4，remote-exec.md §21.3）：替换底部区域。
    Machines(super::machines::MachinesView),
}

impl Overlay {
    /// 盖住输入框、收按键与粘贴的视图（最上面的一层，`layer.rs`）。快捷键面板画在输入框上面，不盖住它。
    pub fn covers_input(&self) -> bool {
        match self {
            Self::Shortcuts => false,
            Self::List(_)
            | Self::Page { .. }
            | Self::Detail(_)
            | Self::Login(_)
            | Self::Permissions(_)
            | Self::Export(_)
            | Self::Diff(_)
            | Self::Projects(_)
            | Self::Resume(_)
            | Self::Trust(_)
            | Self::Background(_)
            | Self::Usage(_)
            | Self::YoloConfirm(_)
            | Self::Machines(_) => true,
        }
    }

    /// 占满整个屏幕、不画 transcript 的视图：鼠标在这里按屏幕坐标选择。
    pub fn full_screen(&self) -> bool {
        match self {
            Self::Detail(_)
            | Self::Page { .. }
            | Self::Projects(_)
            | Self::Resume(_)
            | Self::Trust(_)
            | Self::Usage(_) => true,
            Self::List(_)
            | Self::Shortcuts
            | Self::Login(_)
            | Self::Permissions(_)
            | Self::Export(_)
            | Self::Diff(_)
            | Self::Background(_)
            | Self::YoloConfirm(_)
            | Self::Machines(_) => false,
        }
    }
}

/// 一个视图的按键处理。
type KeyHandler = fn(&mut App, KeyEvent) -> Option<Action>;

impl App {
    /// 打开着的视图的按键（tui.md §7）。
    pub(super) fn overlay_key(&mut self, key: KeyEvent) -> Option<Action> {
        let handler: KeyHandler = match self.overlay.as_ref()? {
            Overlay::List(_) => App::list_key,
            Overlay::Page { .. } => App::page_key,
            // 快捷键面板：任何键都关闭。
            Overlay::Shortcuts => |app, _| {
                app.overlay = None;
                None
            },
            Overlay::Detail(_) => App::detail_key,
            Overlay::Login(_) => App::login_key,
            Overlay::Permissions(_) => App::permissions_key,
            Overlay::Export(_) => App::export_key,
            Overlay::Diff(_) => App::diff_key,
            Overlay::Projects(_) => App::projects_key,
            Overlay::Resume(_) => App::resume_key,
            Overlay::Trust(_) => |app, key| app.trust_key(key, Instant::now()),
            Overlay::Background(_) => |app, key| app.background_key(key, Instant::now()),
            Overlay::Usage(_) => App::usage_key,
            Overlay::YoloConfirm(_) => App::yolo_confirm_key,
            Overlay::Machines(_) => App::machines_key,
        };
        handler(self, key)
    }

    /// 打开着的视图收粘贴：有输入框的视图粘进去，别的视图丢掉（只读查看子代理时同样不收）。
    pub(super) fn overlay_paste(&mut self, text: &str) {
        let first_line = || text.lines().next().unwrap_or_default();
        match self.overlay.as_mut() {
            Some(Overlay::Login(_)) => self.login_paste(text),
            // `/resume` 页：粘贴进查询或改名输入（tui-manage.md §10.4）。
            Some(Overlay::Resume(_)) => self.resume_paste(text),
            // `/export` 的文件名输入框收粘贴（只取第一行）。
            Some(Overlay::Export(view)) => {
                if let super::export::ExportView::Name(name) = view {
                    name.push_str(first_line());
                }
            }
            // `/permissions` 的规则输入框收粘贴（只取第一行）；列表阶段进过滤。
            Some(Overlay::Permissions(view)) => match &mut view.stage {
                super::permissions::Stage::Input(typed) => typed.push_str(first_line()),
                super::permissions::Stage::List => view.paste_filter(text),
                super::permissions::Stage::Destination(_)
                | super::permissions::Stage::Details { .. }
                | super::permissions::Stage::Saving => {}
            },
            // 单行输入都收粘贴（架构整理 U12，用户决定 Q8）：列表的过滤、详细视图的搜索、← 项目页的改名与搜索。
            Some(Overlay::List(list)) => list.paste(text),
            Some(Overlay::Detail(detail)) => detail.paste(text),
            Some(Overlay::Projects(_)) => self.projects_paste(text),
            Some(
                Overlay::Page { .. }
                | Overlay::Shortcuts
                | Overlay::Diff(_)
                | Overlay::Trust(_)
                | Overlay::Background(_)
                | Overlay::Usage(_)
                | Overlay::YoloConfirm(_)
                | Overlay::Machines(_),
            )
            | None => {}
        }
    }

    /// 一格滚轮给打开着的视图（tui-select.md §4.1）：列表类每格 1 项（`step`），滚动类按加速的行数（`lines`）。返回
    /// false 时视图不滚，滚轮滚 transcript。
    pub(super) fn overlay_wheel(&mut self, step: isize, lines: isize, now: Instant) -> bool {
        let Some(overlay) = self.overlay.as_mut() else {
            return false;
        };
        match overlay {
            // 详细视图的滚动量是离底部的行数。
            Overlay::Detail(_) => self.detail_wheel(-lines),
            Overlay::Page { scroll, .. } => *scroll = scroll.saturating_add_signed(lines),
            Overlay::Usage(view) => view.wheel(lines),
            Overlay::Diff(_) => self.diff_wheel(lines),
            Overlay::List(list) => list.wheel(step),
            Overlay::Permissions(view) => view.wheel(step),
            Overlay::Projects(_) => self.projects_wheel(step),
            Overlay::Resume(_) => self.resume_wheel(step),
            Overlay::Trust(_) => {}
            Overlay::Background(_) => self.background_wheel(step, now),
            Overlay::Login(_)
            | Overlay::Export(_)
            | Overlay::Shortcuts
            | Overlay::YoloConfirm(_)
            | Overlay::Machines(_) => return false,
        }
        true
    }

    /// 替换底部区域的视图的行；不替换底部区域的视图（快捷键面板在输入框上面，全屏视图另画）返回 None。
    pub(super) fn overlay_lines(
        &self,
        width: usize,
        height: usize,
        rows: usize,
        now_ms: u64,
    ) -> Option<ViewLines> {
        let plain = |lines: Vec<Line<'static>>| Some((lines, Vec::new(), None));
        match self.overlay.as_ref()? {
            Overlay::Login(view) => plain(super::login::lines(
                view,
                self.strings,
                &self.palette,
                width,
            )),
            Overlay::Diff(view) => plain(super::diff_view_lines::lines(
                self, view, width, height, now_ms,
            )),
            Overlay::Export(view) => {
                plain(super::export::lines(view, self.lang, &self.palette, width))
            }
            Overlay::Permissions(view) => plain(super::permissions_lines::lines(
                view,
                self.lang,
                &self.palette,
                width,
                height,
            )),
            Overlay::Background(_) => {
                let (lines, rows) = self.background_lines(width, height, Instant::now());
                let targets: Vec<RowTarget> = rows
                    .into_iter()
                    .map(|(row, index)| (row, 0, None, Target::Background(index)))
                    .collect();
                Some((lines, targets, None))
            }
            // 切到完全访问的确认框（tui.md §7.8）：局部的框，上面的对话照常显示。
            Overlay::YoloConfirm(confirm) => {
                let view = super::interaction::View {
                    palette: &self.palette,
                    lang: self.lang,
                    width,
                    height: rows,
                    now_ms,
                };
                plain(super::yolo_confirm::lines(confirm, &view, height))
            }
            // `/machines` 的列表（remote-exec.md §21.3）。
            Overlay::Machines(_) => plain(self.machines_lines(width, height, now_ms)),
            Overlay::List(list) => {
                let (lines, items) = list.lines(width, height, self.lang, &self.palette);
                let targets = items
                    .into_iter()
                    .map(|(row, index)| (row, 0, None, Target::Item(index)))
                    .collect();
                Some((lines, targets, None))
            }
            Overlay::Shortcuts
            | Overlay::Page { .. }
            | Overlay::Detail(_)
            | Overlay::Projects(_)
            | Overlay::Resume(_)
            | Overlay::Trust(_)
            | Overlay::Usage(_) => None,
        }
    }

    /// 画全屏视图：取出来画、画完放回（绘制不换视图）。
    pub(super) fn draw_full_screen(
        &mut self,
        frame: &mut Frame<'_>,
        cache: &mut Cache,
        now: Instant,
        now_ms: u64,
    ) {
        let Some(mut overlay) = self.overlay.take() else {
            return;
        };
        match &mut overlay {
            Overlay::Detail(detail) => {
                let elapsed = now.duration_since(self.started);
                super::detail::draw(frame, self, detail, cache, elapsed, now_ms);
            }
            Overlay::Page { local, scroll } => {
                super::page::page(frame, self, local, scroll, now_ms)
            }
            Overlay::Usage(view) => super::usage::draw(frame, self, view),
            Overlay::Projects(page) => super::projects_draw::draw(frame, self, page, now_ms),
            Overlay::Resume(page) => super::resume_draw::draw(frame, self, page, now_ms),
            Overlay::Trust(ask) => super::trust_ask::draw(frame, self, ask),
            Overlay::List(_)
            | Overlay::Shortcuts
            | Overlay::Login(_)
            | Overlay::Permissions(_)
            | Overlay::Export(_)
            | Overlay::Diff(_)
            | Overlay::Background(_)
            | Overlay::YoloConfirm(_)
            | Overlay::Machines(_) => {}
        }
        self.overlay = Some(overlay);
    }
}
