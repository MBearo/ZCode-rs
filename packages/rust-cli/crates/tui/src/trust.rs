//! 工作区信任对话框（docs/specs/rust-cli/workspace-trust.md §2.4、§2.5）。
//!
//! 入口在启动扩展与建会话之前调用 [`ask`]：独立进入、离开一次 alternate screen，阻塞到用户回答（§1 的启动时序）。
//! 判定是否要问、写信任记录都在入口；这里只显示与收按键。真实终端与测试（`TestBackend`）走同一个 [`drive`]。
use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::{Result, bail};
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures_util::{Stream, StreamExt};
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::widgets::Paragraph;

use crate::services::{UiLocale, UiTheme};
use crate::ui::i18n::{self, Lang};
use crate::ui::palette::{ColorLevel, Palette};
use crate::ui::trust_view::Dialog;

/// 项目级配置文件里会自动生效的授权（`permission.allowedTools`，workspace-trust.md §2.4）。规则已排好、
/// 写成规则的形式并去重；`count` 是原始条数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grants {
    pub count: usize,
    pub rules: Vec<String>,
    /// 写了这些规则的文件，相对工作区根。
    pub sources: Vec<String>,
}

/// 对话框要显示的内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustPrompt {
    /// 工作区根的绝对路径。
    pub path: String,
    /// 只经父目录信任、项目有授权时的后备询问（§2.5）：取消项是“不带这些权限继续”。
    pub backstop: bool,
    pub grants: Option<Grants>,
}

/// 用户的回答。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustAnswer {
    /// 选了 `Yes, I trust this folder`。
    Trusted,
    /// 后备询问里选了“不带这些权限继续”或按了 esc：继续，但去掉项目级的 allow 规则。
    WithoutGrants,
    /// 退出进程，不打印文字：`No, exit` 与 ctrl+c / ctrl+d 两次是 1，esc 是 0。
    Exit(u8),
}

/// 画对话框、收按键，直到给出回答。给出回答后不再重画；输入结束（终端关掉）当作 `Exit(1)`。
pub async fn drive<B: Backend>(
    prompt: &TrustPrompt,
    palette: Palette,
    lang: Lang,
    terminal: &mut Terminal<B>,
    mut input: impl Stream<Item = Event> + Unpin,
) -> Result<TrustAnswer> {
    let mut dialog = Dialog::new(prompt.backstop);
    loop {
        terminal
            .draw(|frame| {
                let area = frame.area();
                let lines = dialog.lines(prompt, &palette, lang, usize::from(area.width));
                frame.render_widget(Paragraph::new(lines), area);
            })
            .map_err(|error| anyhow::anyhow!("drawing the trust dialog: {error}"))?;
        // 连按的提示到期时重画（界面计时，不同步任何状态）。
        let event = match dialog.hint_until() {
            Some(until) => {
                tokio::select! {
                    event = input.next() => event,
                    () = tokio::time::sleep_until(until.into()) => {
                        dialog.expire(Instant::now());
                        continue;
                    }
                }
            }
            None => input.next().await,
        };
        let Some(event) = event else {
            return Ok(TrustAnswer::Exit(1));
        };
        if let Event::Key(key) = event
            && key.kind != KeyEventKind::Release
            && let Some(answer) = dialog.key(key, Instant::now())
        {
            return Ok(answer);
        }
    }
}

/// 真实终端：进入 alternate screen 与 raw mode，问完恢复。stdin 或 stdout 不是终端时与 TUI 相同地失败。
pub async fn ask(
    prompt: TrustPrompt,
    vars: &BTreeMap<String, String>,
    locale: Option<UiLocale>,
    theme: Option<UiTheme>,
    os: crate::os::Os,
) -> Result<TrustAnswer> {
    let lang = i18n::resolve(locale.map(UiLocale::as_str), vars);
    // 与 TUI 同一个判断（含 Git Bash 的 mintty，工作线 F71）。
    if let Some(refusal) = crate::ui::console::Console::current().refusal(i18n::strings(lang)) {
        bail!("{refusal}");
    }
    let (mut terminal, guard, probed) = crate::ui::terminal::enter()?;
    let palette = Palette::new(
        ColorLevel::detect(vars, os),
        crate::ui::slash_theme::background(theme.unwrap_or(UiTheme::Auto), probed.background),
    )
    .with_foreground(probed.foreground);
    // 读失败的事件跳过（与 TUI 的输入相同）；对话框结束时丢掉事件流，TUI 之后重建自己的。
    let input = EventStream::new().filter_map(|event| std::future::ready(event.ok()));
    let answer = drive(&prompt, palette, lang, &mut terminal, input).await;
    drop(terminal);
    drop(guard);
    answer
}
