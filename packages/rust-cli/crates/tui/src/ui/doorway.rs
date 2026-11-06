//! 门道（docs/specs/rust-cli/tui-kit.md §4.2「登录」；照 Puffin docs/puffin-tui.md §4.2「接入模型（门道）」与
//! `shell/doorway/`）：没有模型时或 `/login` 打开，说怎样接入一个模型，接入后自己关上。状态与按键在这里，画法全是 kit
//! `widgets::doorway`；登录与保存密钥经 runner 的账号服务（tui.md §7.2），结果回来时改这里的状态。
//!
//! 不是层：门道开着时草稿、命令架与别的层照常，门道只拿它自己的键（粘贴密钥那一步拿全部的键）。与 Puffin 的不同（tui-kit.md
//! §4.3「门道里旧界面没有的」）：没有「没有套餐」「团队计划」与「别的 provider」的列表，密钥不经检查。
use crossterm::event::{KeyCode, KeyEvent};
use rust_cli_tui_kit::app::element::Element;
use rust_cli_tui_kit::app::widgets::doorway::{
    self as kit, Body, FamilyInfo, Head, KeyStep, family_info,
};
use rust_cli_tui_kit::app::widgets::usage::Family as KitFamily;
use rust_cli_tui_kit::render::Span;

use super::app::{Action, App};
use super::i18n::Lang;
use crate::services::{Family, RedactedString};

/// 家族在 kit 里的名字（kit 收中性的 `usage::Family`）与回到 platform 的。
fn kit_family(f: Family) -> KitFamily {
    match f {
        Family::Zai => KitFamily::Zai,
        Family::Bigmodel => KitFamily::Bigmodel,
    }
}

fn platform_family(f: KitFamily) -> Family {
    match f {
        KitFamily::Zai => Family::Zai,
        KitFamily::Bigmodel => Family::Bigmodel,
    }
}

/// `Another provider` 与旧界面没有的：在槽里说（Puffin 的设置页对没有来源的项的说法）。
const UNAVAILABLE: &str = "This isn't available in this version yet";

/// 门道在哪一步。
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// 三种接入的方式。
    Choose { sel: usize },
    /// 在浏览器里等的登录：地址到了才有 `url`；失败时有原因。链接整段折出与否按远程 shell（`App::remote_shell`）。
    SignIn {
        family: KitFamily,
        url: String,
        failed: Option<String>,
    },
    /// 密钥是给哪个服务的。
    KeyFor { sel: usize },
    /// 板上的密钥；`saving` 时已交给账号服务。esc 回到 `back`。
    Paste {
        family: KitFamily,
        back: Box<Step>,
        key: RedactedString,
        saving: bool,
        refused: Option<String>,
    },
}

/// 打开的门道。`expired` 是登录失效了的家族（选择这一步的头两行）；`note` 是 `c` 或 `Another provider` 说的，留到下一个键。
#[derive(Debug, Clone, PartialEq)]
pub struct Door {
    pub step: Step,
    pub expired: Option<KitFamily>,
    pub note: Option<String>,
}

/// 关上的门道说的：在欢迎块的第 4 行（`in_welcome`），或输入框上方安静的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connected {
    pub says: String,
    pub in_welcome: bool,
}

/// ↑↓ ⏎ 与数字在一组回答上做的（Puffin `keys::answers`）。
pub(super) enum Answer {
    Move(usize),
    Act(usize),
    Pass,
}

/// ↑↓（^P ^N）回绕；⏎ 与 1–9 只在草稿为空时选定。
pub(super) fn answers(key: &KeyEvent, ctrl: bool, n: usize, sel: usize, empty: bool) -> Answer {
    match key.code {
        KeyCode::Up => Answer::Move((sel + n - 1) % n),
        KeyCode::Down => Answer::Move((sel + 1) % n),
        KeyCode::Char('p') if ctrl => Answer::Move((sel + n - 1) % n),
        KeyCode::Char('n') if ctrl => Answer::Move((sel + 1) % n),
        KeyCode::Enter if empty => Answer::Act(sel),
        KeyCode::Char(c @ '1'..='9') if empty && !ctrl => {
            let i = (c as usize) - ('1' as usize);
            if i < n { Answer::Act(i) } else { Answer::Pass }
        }
        _ => Answer::Pass,
    }
}

/// 粘贴那一步留给别人的键：退出、⇧⇥、^O、翻页（Puffin `keys::passes`）。
fn passes(key: &KeyEvent, ctrl: bool) -> bool {
    matches!(
        key.code,
        KeyCode::BackTab | KeyCode::PageUp | KeyCode::PageDown
    ) || (ctrl && matches!(key.code, KeyCode::Char('q' | 'c' | 'd' | 'o')))
}

/// 「Signed in to Z.ai as sam.」（kit 的句子，与 Puffin 同一个函数）。
pub fn signed_in_says(f: Family, account: Option<&str>) -> String {
    kit::signed_in_says(kit_family(f), account.unwrap_or(""))
}

/// 手工 key 接好了（kit 的句子，与 Puffin 同一个函数）：主机是家族名（两边都没有模板时的说法），模型是登录写下的默认
/// 模型去掉 provider（同 cli 的 Puffin host `connect_key`），用时是 0：platform 不试密钥，Puffin host 也给 0。
///
/// 缺陷修复（2026-10-31，与 Puffin 不同）：以前只说「Connected to Z.ai with your key.」，少了 Puffin 的模型与用时。
pub fn keyed_says(f: Family, model: &str) -> String {
    let model_id = model.split_once('/').map_or(model, |(_, m)| m);
    kit::key_says(family_info(kit_family(f)).name, model_id, 0)
}

impl App {
    /// 按界面语言排的两个家族：中文时 BigModel 在前（Puffin 按系统语言）。
    fn door_families(&self) -> [&'static FamilyInfo; 2] {
        if self.lang == Lang::Zh {
            [&kit::BIGMODEL, &kit::ZAI]
        } else {
            [&kit::ZAI, &kit::BIGMODEL]
        }
    }

    fn door_family_at(&self, f: KitFamily) -> usize {
        self.door_families()
            .iter()
            .position(|i| i.family == f)
            .unwrap_or(0)
    }

    /// 已经有模型：选择这一步 esc 关上门道。
    fn has_model(&self) -> bool {
        !self.view.selection().model.is_empty()
    }

    /// 门道在欢迎块里：transcript 只有欢迎块（没有回合、本地块，不是恢复的会话）。
    pub fn door_in_welcome(&self) -> bool {
        self.resumed.is_none()
            && self.viewing.is_none()
            && self.view.rows().is_empty()
            && self.local.is_empty()
    }

    /// 打开门道（启动时没有模型、`/login`）：停在选择；`expired` 时选中那个家族的登录。已经开着时回到选择。
    pub fn open_door(&mut self, expired: Option<Family>) {
        let expired = expired.map(kit_family);
        let sel = expired.map_or(0, |f| self.door_family_at(f));
        self.door = Some(Door {
            step: Step::Choose { sel },
            expired,
            note: None,
        });
    }

    /// 门道开始登录 `family`（选定，或 `/login 家族`）。
    pub(super) fn door_sign_in(&mut self, family: Family) -> Option<Action> {
        if self.door.is_none() {
            self.open_door(None);
        }
        let door = self.door.as_mut()?;
        door.step = Step::SignIn {
            family: kit_family(family),
            url: String::new(),
            failed: None,
        };
        Some(Action::Login(family))
    }

    /// 门道到粘贴 `family` 的密钥（选定，或 `/login 家族-api-key`）；esc 回到密钥给哪个服务。
    pub(super) fn door_paste(&mut self, family: Family) {
        if self.door.is_none() {
            self.open_door(None);
        }
        let family = kit_family(family);
        let sel = self.door_family_at(family);
        if let Some(door) = self.door.as_mut() {
            door.step = Step::Paste {
                family,
                back: Box::new(Step::KeyFor { sel }),
                key: RedactedString::new(String::new()),
                saving: false,
                refused: None,
            };
        }
    }

    /// 授权地址到了。浏览器打没打开都不改画法（Puffin 不理会打开的结果：链接就在屏幕上）。
    ///
    /// 缺陷修复（2026-10-31，与 Puffin 不同）：以前浏览器打不开时才把链接整段折出，远程 shell 里要等打开失败；Puffin 按
    /// 入口的 `place.remote` 一开始就折出。现在同 Puffin：看 `remote_shell`（入口经同一个 `puffin::place` 算）。
    pub fn door_url(&mut self, family: Family, url: String) {
        if let Some(Door {
            step:
                Step::SignIn {
                    family: f,
                    url: u,
                    failed: None,
                },
            ..
        }) = self.door.as_mut()
            && *f == kit_family(family)
        {
            *u = url;
        }
    }

    /// 登录失败：原因留在登录这一步，⏎ 再试。
    pub fn door_failed(&mut self, reason: String) {
        match self.door.as_mut().map(|d| &mut d.step) {
            Some(Step::SignIn { failed, .. }) => *failed = Some(reason),
            Some(Step::Paste {
                saving, refused, ..
            }) => {
                *saving = false;
                *refused = Some(reason);
            }
            // 门道关着（`/login …-api-key <key>` 直接保存）：照旧说在输入框上方。
            _ => self.error(reason),
        }
    }

    /// 登录取消了：还在登录这一步时回到选择（esc 已经回去了时不动）。
    pub fn door_cancelled(&mut self) {
        let family = match self.door.as_ref().map(|d| &d.step) {
            Some(Step::SignIn { family, .. }) => *family,
            _ => return,
        };
        let sel = self.door_family_at(family);
        if let Some(door) = self.door.as_mut() {
            door.step = Step::Choose { sel };
        }
    }

    /// 接好了：门道关上，说接上了什么。
    pub fn door_connected(&mut self, says: String) {
        let in_welcome = self.door_in_welcome();
        self.door = None;
        if !in_welcome {
            self.said.quiet = Some(format!("✓ {says}"));
        }
        self.connected = Some(Connected { says, in_welcome });
    }

    /// 门道的键；`None` 把键交给后面（草稿、滚动）。
    pub(super) fn door_key(&mut self, key: KeyEvent) -> Option<Option<Action>> {
        let ctrl = super::keyboard::ctrl(&key);
        let typed = super::keyboard::is_text(&key);
        let empty = self.composer.is_empty();
        let has_model = self.has_model();
        let families = self.door_families();
        let door = self.door.as_mut()?;
        door.note = None;
        match &mut door.step {
            Step::Choose { sel } => {
                if key.code == KeyCode::Esc && has_model {
                    self.door = None;
                    return Some(None);
                }
                match answers(&key, ctrl, families.len() + 1, *sel, empty) {
                    Answer::Move(i) => *sel = i,
                    Answer::Act(i) if i < families.len() => {
                        return Some(self.door_sign_in(platform_family(families[i].family)));
                    }
                    Answer::Act(_) => door.step = Step::KeyFor { sel: 0 },
                    Answer::Pass => return None,
                }
                Some(None)
            }
            Step::SignIn {
                family,
                url,
                failed,
                ..
            } => match key.code {
                KeyCode::Esc => {
                    let waiting = failed.is_none();
                    let sel = families.iter().position(|f| f.family == *family);
                    door.step = Step::Choose {
                        sel: sel.unwrap_or(0),
                    };
                    Some(waiting.then_some(Action::CancelLogin))
                }
                KeyCode::Char('c') if empty && !ctrl && !url.is_empty() && failed.is_none() => {
                    let link = url.clone();
                    door.note = Some("Copied the link".to_owned());
                    Some(Some(Action::Copy(link)))
                }
                KeyCode::Enter if empty && failed.is_some() => {
                    let family = platform_family(*family);
                    Some(self.door_sign_in(family))
                }
                _ => None,
            },
            Step::KeyFor { sel } => {
                if key.code == KeyCode::Esc {
                    door.step = Step::Choose {
                        sel: families.len(),
                    };
                    return Some(None);
                }
                match answers(&key, ctrl, families.len() + 1, *sel, empty) {
                    Answer::Move(i) => *sel = i,
                    Answer::Act(i) if i < families.len() => {
                        let family = platform_family(families[i].family);
                        self.door_paste(family);
                    }
                    Answer::Act(_) => door.note = Some(UNAVAILABLE.to_owned()),
                    Answer::Pass => return None,
                }
                Some(None)
            }
            // 保存中：键不改密钥（旧界面的保存很快，没有 Puffin 检查时的 esc 取消）。
            Step::Paste { saving: true, .. } => {
                if passes(&key, ctrl) {
                    None
                } else {
                    Some(None)
                }
            }
            Step::Paste {
                family,
                back,
                key: secret,
                saving,
                refused,
            } => match key.code {
                KeyCode::Esc => {
                    door.step = (**back).clone();
                    Some(None)
                }
                KeyCode::Enter if !secret.expose().trim().is_empty() => {
                    *saving = true;
                    *refused = None;
                    Some(Some(Action::ConfigureKey {
                        family: platform_family(*family),
                        key: RedactedString::new(secret.expose().trim().to_owned()),
                    }))
                }
                KeyCode::Char('u') if ctrl => {
                    *secret = RedactedString::new(String::new());
                    Some(None)
                }
                KeyCode::Char('c') if ctrl && !secret.expose().is_empty() => {
                    *secret = RedactedString::new(String::new());
                    Some(None)
                }
                KeyCode::Backspace => {
                    let mut text = secret.expose().to_owned();
                    super::line_edit::backspace(&mut text);
                    *secret = RedactedString::new(text);
                    Some(None)
                }
                KeyCode::Char(c) if typed => {
                    *secret = RedactedString::new(format!("{}{c}", secret.expose()));
                    Some(None)
                }
                _ if passes(&key, ctrl) => None,
                _ => Some(None),
            },
        }
    }

    /// 粘贴进密钥（去掉首尾空白与控制字符）；不在粘贴这一步时 `false`（照常粘贴进草稿）。
    pub(super) fn door_paste_text(&mut self, text: &str) -> bool {
        let Some(Door {
            step: Step::Paste {
                key, saving: false, ..
            },
            ..
        }) = self.door.as_mut()
        else {
            return false;
        };
        let pasted: String = text.trim().chars().filter(|c| !c.is_control()).collect();
        *key = RedactedString::new(format!("{}{pasted}", key.expose()));
        true
    }

    /// 门道的头两行（Puffin `door_head`）。
    pub fn door_head(&self) -> (Vec<Span>, Vec<Span>) {
        let theme = &self.ink.theme;
        let Some(d) = self.door.as_ref() else {
            return (Vec::new(), Vec::new());
        };
        let name;
        let h = match &d.step {
            Step::Choose { .. } => Head::Choose { expired: d.expired },
            Step::SignIn { family, .. } => Head::SignIn { family: *family },
            Step::KeyFor { .. } => Head::KeyFor,
            Step::Paste { family, .. } => {
                name = family_info(*family).name;
                Head::Paste { name }
            }
        };
        kit::head(theme, &h)
    }

    /// 头下面的行，每行以 `lead` 开头（Puffin `door_body`）。两个家族的模板名是家族名、没有主机，`Another provider` 没有
    /// 列表（同 cli 的 Puffin host 没给模板时）。
    fn door_body(&self, lead: &str) -> Vec<Element> {
        let Some(d) = self.door.as_ref() else {
            return Vec::new();
        };
        let families = self.door_families();
        let b = match &d.step {
            Step::Choose { sel } => Body::Choose { sel: *sel },
            Step::SignIn {
                family,
                url,
                failed,
            } => Body::SignIn {
                family: *family,
                url,
                failed: failed.as_deref(),
                remote: self.remote_shell,
            },
            Step::KeyFor { sel } => Body::KeyFor {
                sel: Some(*sel),
                answers: kit::key_answers(
                    families,
                    [(families[0].name, ""), (families[1].name, "")],
                    Vec::new(),
                ),
            },
            Step::Paste {
                family,
                saving,
                refused,
                ..
            } => {
                let name = family_info(*family).name;
                Body::Paste {
                    name,
                    key_url: "",
                    host: name,
                    checking: *saving,
                    refused: refused.as_deref(),
                }
            }
        };
        kit::body(&self.ink.theme, &b, families, lead)
    }

    /// 门道在欢迎块下面的块（从欢迎块的文字列起）；不在欢迎块里时为 None。
    pub fn door_below_welcome(&self) -> Option<Element> {
        if self.door.is_none() || !self.door_in_welcome() {
            return None;
        }
        kit::in_welcome(
            self.door_body(&" ".repeat(rust_cli_tui_kit::app::widgets::welcome::TEXT_COL)),
        )
    }

    /// 门道在 transcript 末尾的块（会话有了回合之后）；在欢迎块里或关着时为 None。
    pub fn door_at_end(&self) -> Option<Element> {
        if self.door.is_none() || self.door_in_welcome() {
            return None;
        }
        Some(kit::at_end(
            self.door_head(),
            self.door_body(rust_cli_tui_kit::app::widgets::user_slab::INDENT),
        ))
    }

    /// 粘贴那一步在板上的行（最后一行是输入行）。
    pub fn door_slab(&self, width: usize) -> Option<Vec<Element>> {
        match self.door.as_ref().map(|d| &d.step) {
            Some(Step::Paste { family, key, .. }) => Some(kit::board::paste_rows(
                &self.ink.theme,
                family_info(*family).name,
                key.expose().chars().count(),
                width,
            )),
            _ => None,
        }
    }

    /// 门道的键（kit `board::keys`），或 `c` 与 `Another provider` 刚说的；门道关着时为 None。
    pub fn door_keys(&self) -> Option<Result<Vec<(String, String)>, String>> {
        let d = self.door.as_ref()?;
        if let Some(note) = &d.note {
            return Some(Err(note.clone()));
        }
        let step = match &d.step {
            Step::Choose { .. } => KeyStep::Choose,
            Step::SignIn { failed, .. } => KeyStep::SignIn {
                failed: failed.is_some(),
            },
            Step::KeyFor { .. } => KeyStep::KeyFor,
            Step::Paste { saving, .. } => KeyStep::Paste { checking: *saving },
        };
        Some(Ok(kit::board::keys(
            step,
            self.composer.is_empty(),
            self.has_model(),
        )))
    }
}

#[cfg(test)]
#[path = "doorway_tests.rs"]
mod tests;
