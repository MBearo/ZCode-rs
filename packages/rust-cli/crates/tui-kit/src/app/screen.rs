//! `view` 每帧产出的东西、渲染时的视口，与 Elm 架构的契约。
//! 移植自 packages/tui/tui/src/zcode/tui/app/App.scala（feat/tui a2bf878b34）：`update` 拿 `&mut State`，效果只以 [`Cmd`]
//! 描述，不做终端 IO——运行时驱动它。
use super::cmd::Cmd;
use super::element::Element;
use super::sub::Sub;
use crate::render::renderer::{Backdrop, Graphic};

/// 整个屏幕（docs/puffin-tui.md §4.4）：`body` 正好一屏的行（渲染器裁或补到终端高度），按视口宽度排版；`graphics` 画在
/// 文字之后，按屏幕格子定位；`backdrop` 是主题涂的地（如果它涂）。
#[derive(Debug, Clone, PartialEq)]
pub struct Screen {
    pub body: Element,
    pub graphics: Vec<Graphic>,
    pub backdrop: Option<Backdrop>,
}

impl Screen {
    pub fn new(body: Element) -> Screen {
        Screen {
            body,
            graphics: Vec::new(),
            backdrop: None,
        }
    }
}

/// 渲染时的终端尺寸，由运行时的轮询采样。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Viewport {
    pub width: usize,
    pub rows: usize,
}

impl Viewport {
    pub const fn new(width: usize, rows: usize) -> Viewport {
        Viewport { width, rows }
    }
}

impl Default for Viewport {
    fn default() -> Viewport {
        Viewport::new(80, 24)
    }
}

pub trait App {
    type State;
    type Msg: Send + 'static;

    fn init(&self) -> (Self::State, Cmd<Self::Msg>);
    fn update(&self, msg: Self::Msg, state: &mut Self::State) -> Cmd<Self::Msg>;
    fn subscriptions(&self, state: &Self::State) -> Vec<Sub<Self::Msg>>;
    fn view(&self, state: &Self::State, viewport: Viewport) -> Screen;
    /// 界面离开备用屏之后写到主屏的文字（留给用户 shell 历史的最后一行）；空串是没有。
    fn exit_text(&self, _state: &Self::State) -> String {
        String::new()
    }
}
