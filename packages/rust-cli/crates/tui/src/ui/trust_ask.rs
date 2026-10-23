//! 应用内的工作区信任（修复线 F29，docs/specs/rust-cli/workspace-trust.md §2.8）：切到别的项目的会话或在别的项目新建
//! 会话之前，那个目录未信任时全屏画同一个信任对话框。同意后由 runner 写记录并继续；拒绝回到原来的页面（不退出进程）。
use std::time::Instant;

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::widgets::Paragraph;

use super::app::{Action, App, Overlay};
use super::projects::Request;
use super::trust_view::Dialog;
use crate::trust::{TrustAnswer, TrustPrompt};

#[derive(Debug, Clone, PartialEq)]
pub struct TrustAsk {
    pub prompt: TrustPrompt,
    pub dialog: Dialog,
    /// 同意后要做的事（`Request::Open` 或 `Request::New`）与它的目录。
    pub then: Request,
    pub root: String,
    /// 拒绝时回到的页面。
    pub back: Option<Box<Overlay>>,
}

impl App {
    /// runner 判定未信任：弹对话框，原来的页面留作返回。
    pub fn ask_trust(&mut self, prompt: TrustPrompt, root: String, then: Request) {
        let back = self.overlay.take().map(Box::new);
        self.overlay = Some(Overlay::Trust(Box::new(TrustAsk {
            dialog: Dialog::new(prompt.backstop),
            prompt,
            then,
            root,
            back,
        })));
    }

    pub(super) fn trust_key(&mut self, key: KeyEvent, now: Instant) -> Option<Action> {
        let Some(Overlay::Trust(ask)) = self.overlay.as_mut() else {
            return None;
        };
        match ask.dialog.key(key, now)? {
            TrustAnswer::Trusted => {
                let Some(Overlay::Trust(ask)) = self.overlay.take() else {
                    return None;
                };
                let TrustAsk {
                    then, root, back, ..
                } = *ask;
                self.overlay = back.map(|back| *back);
                Some(Action::Projects(Request::Trusted {
                    root,
                    then: Box::new(then),
                }))
            }
            // `No, exit`、esc、ctrl+c 两次：只取消这次切换（tui-panels.md D13）。
            TrustAnswer::WithoutGrants | TrustAnswer::Exit(_) => {
                let Some(Overlay::Trust(ask)) = self.overlay.take() else {
                    return None;
                };
                self.overlay = ask.back.map(|back| *back);
                None
            }
        }
    }
}

pub fn draw(frame: &mut Frame<'_>, app: &mut App, ask: &TrustAsk) {
    let area = frame.area();
    app.hits = super::mouse::Hits::default();
    let lines = ask
        .dialog
        .lines(&ask.prompt, &app.palette, app.lang, usize::from(area.width));
    frame.render_widget(Paragraph::new(lines), area);
}
