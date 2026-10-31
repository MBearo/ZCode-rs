//! `/login` 与 `/logout`（docs/specs/rust-cli/tui.md §7.2，Node `command-center/create.ts`、`login-flow.ts`）：
//! 登录视图的状态、按键与排版。视图只显示；登录是否在途、结果如何由 runner 收到 `Done` 后改这里的状态。
//! 手工 key 只在这个视图里输入（显示 `•`），不经输入框，所以不进输入历史。
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::app::{Action, App, Overlay};
use super::i18n::{self, Strings};
use crate::services::{Family, RedactedString};

/// 登录视图的选项（Node `buildLoginSelection` 的顺序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    OAuth(Family),
    ApiKey(Family),
}

const METHODS: [Method; 4] = [
    Method::OAuth(Family::Zai),
    Method::OAuth(Family::Bigmodel),
    Method::ApiKey(Family::Zai),
    Method::ApiKey(Family::Bigmodel),
];

/// 一次按键之后视图要做的事（先在借用里改状态，再换视图或给出动作）。
enum Next {
    Stay,
    Close,
    Start(Family),
    Input(Family),
    Back(&'static str),
    Act(Action),
}

/// 登录视图。
#[derive(Debug, Clone, PartialEq)]
pub enum LoginView {
    /// 选择方式；`status` 是上一步留下的提示（取消等）。
    Choose {
        selected: usize,
        status: Option<&'static str>,
    },
    /// 等浏览器授权；`cancelling` 为真时取消已经发出、等 platform 停下。
    Waiting { family: Family, cancelling: bool },
    /// 不回显的 key 输入；`saving` 为真时已交给 `Account`，等结果。
    Input {
        family: Family,
        key: RedactedString,
        status: Option<&'static str>,
        saving: bool,
    },
}

impl App {
    /// `/login [方式]`（tui.md §7.2 的命令表）。
    pub(super) fn login_command(&mut self, args: &str) -> Option<Action> {
        let mut words = args.split_whitespace();
        let kind = words.next();
        let key = words.collect::<Vec<_>>().join(" ");
        // 账号体系的名字只在 platform 解析（F58：以前这里另写了一个闭包）。
        let family = Family::parse;
        let Some(kind) = kind else {
            self.overlay = Some(Overlay::Login(LoginView::Choose {
                selected: 0,
                status: None,
            }));
            return None;
        };
        if let Some(name) = kind.strip_suffix("-coding-plan-api-key")
            && let Some(family) = family(name)
        {
            if key.is_empty() {
                self.open_key_input(family, None);
                return None;
            }
            // 带 key 的一行已经进了输入框的历史：去掉（D7）。
            self.composer.forget_last();
            return Some(Action::ConfigureKey {
                family,
                key: RedactedString::new(key),
            });
        }
        if let Some(name) = kind.strip_suffix("-coding-plan")
            && let Some(family) = family(name)
            && key.is_empty()
        {
            return self.start_login(family);
        }
        self.info(i18n::login_usage(self.lang));
        None
    }

    /// `/logout`：带参数时提示用法。
    pub(super) fn logout_command(&mut self, args: &str) -> Option<Action> {
        if args.trim().is_empty() {
            return Some(Action::Logout);
        }
        self.info(i18n::logout_usage(self.lang));
        None
    }

    fn start_login(&mut self, family: Family) -> Option<Action> {
        self.overlay = Some(Overlay::Login(LoginView::Waiting {
            family,
            cancelling: false,
        }));
        Some(Action::Login(family))
    }

    fn open_key_input(&mut self, family: Family, status: Option<&'static str>) {
        self.overlay = Some(Overlay::Login(LoginView::Input {
            family,
            key: RedactedString::new(String::new()),
            status,
            saving: false,
        }));
    }

    /// 回到选项（取消之后），带上提示。
    pub fn login_back(&mut self, status: &'static str) {
        self.overlay = Some(Overlay::Login(LoginView::Choose {
            selected: 0,
            status: Some(status),
        }));
    }

    /// 登录结束：关闭登录视图（用户可能已经关掉或换了别的视图，这时什么都不做）。
    pub fn login_closed(&mut self) {
        if matches!(self.overlay, Some(Overlay::Login(_))) {
            self.overlay = None;
        }
    }

    /// 登录视图的按键；只在视图是登录视图时调用。
    pub(super) fn login_key(&mut self, key: KeyEvent) -> Option<Action> {
        let (ctrl, typed) = (self.typing.ctrl(&key), self.typing.is_text(&key));
        let strings = self.strings;
        let Some(Overlay::Login(view)) = self.overlay.as_mut() else {
            return None;
        };
        let next = match view {
            LoginView::Choose { selected, status } => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    *selected = selected.checked_sub(1).unwrap_or(METHODS.len() - 1);
                    *status = None;
                    Next::Stay
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    *selected = (*selected + 1) % METHODS.len();
                    *status = None;
                    Next::Stay
                }
                KeyCode::Enter => match METHODS[*selected] {
                    Method::OAuth(family) => Next::Start(family),
                    Method::ApiKey(family) => Next::Input(family),
                },
                KeyCode::Esc => Next::Close,
                KeyCode::Char('c') if ctrl => Next::Close,
                _ => Next::Stay,
            },
            // 等授权：esc / ctrl+c 发取消（一条消息）；结果回来之前视图不变。
            LoginView::Waiting { cancelling, .. } => {
                let cancel = key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c'));
                if cancel && !*cancelling {
                    *cancelling = true;
                    Next::Act(Action::CancelLogin)
                } else {
                    Next::Stay
                }
            }
            LoginView::Input { saving: true, .. } => Next::Stay,
            LoginView::Input {
                family,
                key: secret,
                status,
                saving,
            } => match key.code {
                KeyCode::Enter if secret.expose().trim().is_empty() => {
                    *status = Some(strings.login_key_empty);
                    Next::Stay
                }
                KeyCode::Enter => {
                    *saving = true;
                    Next::Act(Action::ConfigureKey {
                        family: *family,
                        key: secret.clone(),
                    })
                }
                KeyCode::Esc => Next::Back(strings.login_key_cancelled),
                KeyCode::Char('c') if ctrl => Next::Back(strings.login_key_cancelled),
                KeyCode::Char('u') if ctrl => {
                    *secret = RedactedString::new(String::new());
                    *status = Some(strings.login_key_cleared);
                    Next::Stay
                }
                KeyCode::Backspace => {
                    let mut text = secret.expose().to_owned();
                    text.pop();
                    *secret = RedactedString::new(text);
                    Next::Stay
                }
                KeyCode::Char(c) if typed => {
                    *secret = RedactedString::new(format!("{}{c}", secret.expose()));
                    *status = None;
                    Next::Stay
                }
                _ => Next::Stay,
            },
        };
        match next {
            Next::Stay => None,
            Next::Close => {
                self.overlay = None;
                None
            }
            Next::Start(family) => self.start_login(family),
            Next::Input(family) => {
                self.open_key_input(family, None);
                None
            }
            Next::Back(status) => {
                self.login_back(status);
                None
            }
            Next::Act(action) => Some(action),
        }
    }

    /// 粘贴进 key 输入（去掉换行）；其他登录视图里的粘贴忽略。
    pub(super) fn login_paste(&mut self, text: &str) {
        if let Some(Overlay::Login(LoginView::Input {
            key, saving: false, ..
        })) = self.overlay.as_mut()
        {
            let pasted: String = text.chars().filter(|c| !c.is_control()).collect();
            *key = RedactedString::new(format!("{}{pasted}", key.expose()));
        }
    }
}

fn dim(text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {}", text.into()),
        Style::default().add_modifier(Modifier::DIM),
    ))
}

fn title(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {text}"),
        Style::default().add_modifier(Modifier::BOLD),
    ))
}

fn method_labels(strings: &Strings, method: Method) -> (String, &'static str) {
    match method {
        Method::OAuth(family) => (
            format!("{} Coding Plan", family.display_name()),
            match family {
                Family::Zai => strings.login_zai_detail,
                Family::Bigmodel => strings.login_bigmodel_detail,
            },
        ),
        Method::ApiKey(family) => (
            format!("{} Coding Plan API Key", family.display_name()),
            strings.login_key_detail,
        ),
    }
}

/// 登录视图的行（替换输入区，tui.md §7.2）。
pub fn lines(
    view: &LoginView,
    strings: &Strings,
    palette: &super::palette::Palette,
    width: usize,
) -> Vec<Line<'static>> {
    let accent = palette.accent;
    let keys = |template: &str| super::key_hint::line(super::layout::INDENT, template, palette);
    let rule = Line::from(Span::styled("─".repeat(width), Style::default().fg(accent)));
    let mut lines = vec![rule];
    match view {
        LoginView::Choose { selected, status } => {
            lines.push(title(strings.login_title));
            lines.push(dim(strings.login_prompt));
            lines.push(Line::default());
            for (index, method) in METHODS.iter().enumerate() {
                let (label, detail) = method_labels(strings, *method);
                // 选择列表组件（`› ` 加底色条，tui.md §7.4，修复线 F26）；说明的第二行也在底色条里。
                let chosen = index == *selected;
                lines.push(super::pick::paint(
                    Line::from(vec![
                        Span::raw("  "),
                        super::pick::pointer(chosen),
                        Span::raw(label),
                    ]),
                    chosen,
                    palette,
                    width,
                ));
                lines.push(super::pick::paint(
                    Line::from(Span::styled(format!("    {detail}"), palette.secondary())),
                    chosen,
                    palette,
                    width,
                ));
            }
            lines.push(Line::default());
            if let Some(status) = status {
                lines.push(dim(*status));
            }
            lines.push(keys(strings.login_help));
        }
        LoginView::Waiting { family, cancelling } => {
            let (primary, secondary) = match family {
                Family::Zai => (strings.login_wait_zai, strings.login_wait_zai_detail),
                Family::Bigmodel => (
                    strings.login_wait_bigmodel,
                    strings.login_wait_bigmodel_detail,
                ),
            };
            lines.push(title(primary));
            lines.push(dim(secondary));
            lines.push(Line::default());
            lines.push(dim(if *cancelling {
                strings.login_cancelling
            } else {
                strings.login_wait_status
            }));
            lines.push(keys(strings.login_wait_help));
        }
        LoginView::Input {
            family,
            key,
            status,
            saving,
        } => {
            lines.push(title(match family {
                Family::Zai => strings.login_key_zai,
                Family::Bigmodel => strings.login_key_bigmodel,
            }));
            lines.push(dim(strings.login_key_hidden));
            lines.push(Line::default());
            let count = key.expose().chars().count();
            let field = if count == 0 {
                Span::styled(
                    strings.login_key_placeholder,
                    Style::default().add_modifier(Modifier::DIM),
                )
            } else {
                Span::raw("•".repeat(count))
            };
            lines.push(Line::from(vec![
                Span::styled("  ❯ ", Style::default().fg(accent)),
                field,
            ]));
            lines.push(Line::default());
            let shown = match (saving, status) {
                (true, _) => strings.login_key_saving,
                (false, Some(status)) => status,
                (false, None) => strings.login_key_status,
            };
            lines.push(dim(shown));
            lines.push(keys(strings.login_key_help));
        }
    }
    lines
}
