//! 设置页（docs/specs/rust-cli/tui-kit.md §4.2「设置」；照 Puffin `shell/settings/mod.rs`，两套界面不能互相依赖）：正文换成
//! 设置的一页，输入框是搜索与键入值的地方，状态栏右边是这一页的键。只有界面状态：设置本身属于服务与内核，打开一段时由
//! runner 读进来（`runner/settings.rs`），页是读到的东西的纯函数。按键在 `keys.rs`，页在 `page.rs`、`sections.rs`、
//! `lists.rs`、`permissions.rs` 与 `machines.rs`，画法（kit `widgets::settings`）在 `frame.rs`。
//!
//! 与 Puffin 的不同（tui-kit.md §4.3 登记）：家族的登录走旧界面的门道；值淡入按墙上时间折成 kit 的 tick；设置页同 Puffin
//! 只有英文。Models 的改动两边的 host 都还没有（tui-kit.md §4.5）：说的话同 Puffin 的 host 回答之后（`providers.rs`）。
use std::collections::BTreeMap;
use std::time::Instant;

use rust_cli_tui_kit::app::widgets::fade;
use rust_cli_tui_kit::app::widgets::playhead::TICK_MS;
use rust_cli_tui_kit::app::widgets::settings::{Applies, SECTIONS, section, section_index};
use rust_cli_tui_kit::theme::scheme::ThemeChoice;

use super::app::{Action, App};
use crate::machines::SshHost;

pub mod data;
pub mod field;
pub mod frame;
mod keys;
pub mod lists;
pub mod machine_words;
pub mod machines;
pub mod page;
pub mod permissions;
pub mod providers;
pub mod sections;
pub mod tokens;

use data::{Section, SectionData};
use field::Field;
use page::{Act, Change, Ctx, Page, Row, View, Write};

/// 设置页除了显示行之外在做的。
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Mode {
    #[default]
    Browse,
    /// 行 `key` 的选项在它下面打开；`at` 是选中的选项。
    Choosing { key: String, at: usize },
    /// 先问：0 是动作，1 是 `Keep it`。
    Confirming {
        key: String,
        at: usize,
        label: String,
        note: String,
        /// 装箱：`Page::Model` 带两个名字之后 `Act` 变大，模式的各项差得太多（clippy `large_enum_variant`）。
        act: Box<Act>,
    },
    /// 输入框里的一个值。
    Editing { key: String, draft: Field },
    /// 输入框里的缺省模型选择器。
    Picking { at: usize, wish: String },
}

/// 打开的设置页。
///
/// - `in_list`：焦点在段的目录上，不在页上；在没有目录的宽度下没有意义。
/// - `chosen`：每一页选中的可选行，按行的 id。
/// - `typed`：每一行的值上次变的时候，从那时淡入；变了还没画过的是 `None`，画的那一帧记下它的现在（动效与帧同一个时钟）。
/// - `said`：输入框上方的一行：`Ok` 说改动什么时候生效，`Err` 说为什么失败。
/// - `editor`：编辑器的名字（状态栏的 `⏎ open in vim`），runner 读段时填。
/// - `width`：上一帧的屏宽（`frame.rs` 画的时候记下）。
#[derive(Clone, Debug)]
pub struct Settings {
    pub section: usize,
    pub in_list: bool,
    pub stack: Vec<Page>,
    pub chosen: BTreeMap<Page, String>,
    pub search: Field,
    pub search_chosen: usize,
    pub mode: Mode,
    pub data: data::Data,
    pub form: BTreeMap<String, String>,
    pub said: Option<Result<String, String>>,
    pub typed: BTreeMap<String, Option<Instant>>,
    pub ssh_hosts: Vec<SshHost>,
    pub editor: String,
    /// 上一帧的屏宽：窄（没有目录）时 ← → 换段。
    pub width: usize,
    /// `/logout` 打开的：Models 读到时进登录了的家族的页，选中 Sign out（等数据，不猜）。
    pub sign_out: bool,
    /// 测试连接的那一行（按 key）与结果：`None` 是还在测，`Ok` 是用了多少秒（Puffin `SettingsUi.test`）。
    pub test: Option<(String, Option<Result<f64, String>>)>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            section: 0,
            in_list: true,
            stack: Vec::new(),
            chosen: BTreeMap::new(),
            search: Field::default(),
            search_chosen: 0,
            mode: Mode::Browse,
            data: BTreeMap::new(),
            form: BTreeMap::new(),
            said: None,
            typed: BTreeMap::new(),
            ssh_hosts: Vec::new(),
            editor: String::new(),
            width: 120,
            sign_out: false,
            test: None,
        }
    }
}

impl Settings {
    pub fn page(&self) -> Page {
        self.stack
            .last()
            .cloned()
            .unwrap_or(Page::Top(SECTIONS[self.section.min(SECTIONS.len() - 1)].id))
    }

    pub fn searching(&self) -> bool {
        !self.search.text.is_empty()
    }

    /// 焦点在段的目录上：目录在（不窄），也没有搜索占着页。
    pub fn list_focused(&self, narrow: bool) -> bool {
        self.in_list && !narrow && !self.searching()
    }
}

/// 交给 runner 的事。
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// 读这些段（打开一段时读它，搜索时读还没读过的每一段）。
    Read(Vec<Section>),
    /// 写一个改动；回答带回 `page` 与 `key`，值在那一行淡入。
    Write {
        page: Page,
        key: String,
        write: Box<Write>,
    },
    /// 在编辑器里打开文件（交出终端）。
    Edit { path: String },
    /// 样子已经换上，写 `theme.json`。
    Look(ThemeChoice),
    /// 测试 `provider` 的 `model` 的连接；回答带回 `key`（那一行）。
    Test {
        key: String,
        provider: String,
        model: String,
    },
    /// 读一次 `~/.ssh/config` 的主机（Machines 段）。
    Hosts,
}

/// 一页的一行的身份，给它的值的动效。
pub fn row_key(page: &Page, id: &str) -> String {
    format!("{page:?}/{id}")
}

/// 写成了的回答：`Some` 是文件写了而这次运行没能用上它的原因（Puffin `Saved.reload_error`）。
pub type Saved = Option<String>;

impl App {
    /// 页从什么建。
    pub fn settings_ctx<'a>(&'a self, ui: &'a Settings, now_ms: u64) -> Ctx<'a> {
        Ctx {
            data: &ui.data,
            form: &ui.form,
            home: self.home.as_deref(),
            look: self.ink.choice,
            machines: self.machines_enabled.then_some(&self.machines[..]),
            ssh_hosts: &ui.ssh_hosts,
            connecting: &self.machines_since,
            now_ms,
            editing: match &ui.mode {
                Mode::Editing { key, draft } => Some((key.as_str(), draft.text.as_str())),
                _ => None,
            },
        }
    }

    /// 显示的页：搜索时是搜索的页。
    pub fn settings_view(&self, ui: &Settings, now_ms: u64) -> View {
        let ctx = self.settings_ctx(ui, now_ms);
        if !ui.searching() {
            return page::view(&ui.page(), &ctx);
        }
        let hits = page::search(&ui.search.text, &ctx);
        let rows = if hits.is_empty() {
            vec![Row::Note(format!(
                "Nothing matches “{}”.",
                ui.search.text.trim()
            ))]
        } else {
            let mut rows = Vec::new();
            let mut last = "";
            for (s, r) in hits {
                if s != last {
                    rows.push(Row::Heading(section(s).name.to_owned()));
                    last = s;
                }
                rows.push(r);
            }
            rows
        };
        View::new(Vec::new(), "", rows)
    }

    /// 搜索找到的（段，行）。
    pub fn settings_hits(&self, ui: &Settings, now_ms: u64) -> Vec<(&'static str, Row)> {
        page::search(&ui.search.text, &self.settings_ctx(ui, now_ms))
    }

    /// 选中的行在可选的行里的位置，与那一行。
    pub fn settings_chosen(&self, ui: &Settings, now_ms: u64) -> Option<(usize, Row)> {
        let mut rows: Vec<Row> = self
            .settings_view(ui, now_ms)
            .rows
            .into_iter()
            .filter(Row::selectable)
            .collect();
        if rows.is_empty() {
            return None;
        }
        let i = if ui.searching() {
            ui.search_chosen.min(rows.len() - 1)
        } else {
            ui.chosen
                .get(&ui.page())
                .and_then(|id| rows.iter().position(|r| r.id() == *id))
                .unwrap_or(0)
        };
        Some((i, rows.swap_remove(i)))
    }

    /// 设置页有值在淡入：按动画节拍重画到它落定。
    pub fn settings_moving(&self) -> bool {
        self.settings.as_ref().is_some_and(|ui| {
            ui.typed.values().any(|t| {
                t.is_none_or(|t| {
                    t.elapsed().as_millis() < u128::from(TICK_MS) * fade::TICKS as u128
                })
            })
        })
    }

    // ---- 打开 ----

    /// `/settings [段或词]`：General、点名的段，或一次搜索。草稿清掉（命令本身）；指名的段直接让页拿到焦点。
    pub(super) fn settings_open(&mut self, words: &str) -> Option<Action> {
        let w = words.trim().to_lowercase();
        let at = SECTIONS
            .iter()
            .position(|s| s.id == w || s.name.to_lowercase() == w);
        let mut ui = Settings {
            section: at.unwrap_or(0),
            in_list: at.is_none(),
            ..Settings::default()
        };
        if at.is_none() && !w.is_empty() {
            ui.search = Field::at_end(words.trim());
        }
        self.composer.clear();
        self.said = Default::default();
        let read = if ui.searching() {
            read_all(&ui)
        } else {
            shown(&ui.page())
        };
        self.settings = Some(ui);
        read
    }

    /// 从 `/machines` 的列表打开设置的 Machines 段，`page` 时再进那一页；`target` 填进添加页的 SSH 目标（Puffin
    /// `settings_machines`：列表上 ⏎ 打开那台的页，`~/.ssh/config` 的主机与 `+ Add a machine` 打开添加页）。列表还开着：
    /// esc 一级一级退回列表。
    pub fn settings_machines(
        &mut self,
        page: Option<Page>,
        target: Option<String>,
    ) -> Option<Action> {
        let read = self.settings_open("machines");
        if let Some(ui) = self.settings.as_mut() {
            if let Some(t) = target {
                ui.form.insert(machines::SSH.to_owned(), t);
            }
            if let Some(p) = page {
                if let Page::Machine(name) = &p {
                    ui.chosen
                        .insert(Page::Top("machines"), format!("machine:{name}"));
                }
                ui.stack.push(p);
            }
        }
        read
    }

    /// 关上设置：对话回到原样。
    pub fn settings_close(&mut self) {
        self.settings = None;
    }

    /// 段 `at` 的页显示出来：换段，读它。
    pub(super) fn settings_section(&mut self, at: usize) -> Option<Action> {
        let ui = self.settings.as_mut()?;
        ui.section = at;
        ui.stack.clear();
        ui.said = None;
        ui.test = None;
        shown(&ui.page())
    }

    /// 搜索命中的第 `i` 行上的 ⏎：到它的段，选中它，清掉搜索。
    pub(super) fn settings_go_to(&mut self, i: usize) -> Option<Action> {
        let ui = self.settings.as_ref()?;
        let (s, row) = self.settings_hits(ui, 0).into_iter().nth(i)?;
        let page = Page::Top(section(s).id);
        let ui = self.settings.as_mut()?;
        ui.section = section_index(s).unwrap_or(0);
        ui.in_list = false;
        ui.stack.clear();
        ui.search = Field::default();
        ui.search_chosen = 0;
        ui.chosen.insert(page.clone(), row.id());
        ui.said = None;
        shown(&page)
    }

    // ---- runner 的回答 ----

    /// 读到一段。
    pub fn settings_read(&mut self, result: Result<SectionData, String>) {
        let Some(ui) = self.settings.as_mut() else {
            return;
        };
        match result {
            Ok(data) => {
                ui.data.insert(data.section(), data);
            }
            Err(err) => ui.said = Some(Err(format!("Could not read the settings: {err}"))),
        }
        if ui.sign_out
            && let Some(SectionData::Models(m)) = ui.data.get(&data::Section::Models)
        {
            ui.sign_out = false;
            match m.families.iter().find(|f| f.signed_in) {
                Some(f) => {
                    let id = f.family.as_str();
                    let page = Page::Family(id.to_owned());
                    ui.chosen
                        .insert(Page::Top("models"), format!("family:{id}"));
                    ui.chosen.insert(page.clone(), "signOut".to_owned());
                    ui.stack.push(page);
                }
                None => ui.said = Some(Err("You are not signed in.".to_owned())),
            }
        }
    }

    /// `/logout`：Settings › Models › 登录了的家族，停在 Sign out 行（⏎ 退出）。
    pub(in crate::ui) fn settings_sign_out(&mut self) -> Option<Action> {
        let read = self.settings_open("models");
        if let Some(ui) = self.settings.as_mut() {
            ui.sign_out = true;
        }
        read
    }

    /// 写的回答：值淡入，输入框上方说它什么时候生效；失败时值不变，说为什么。
    pub fn settings_wrote(
        &mut self,
        page: &Page,
        key: &str,
        write: &Write,
        result: Result<Saved, String>,
    ) -> Option<Action> {
        let no_session = self.session.is_none();
        let ui = self.settings.as_mut()?;
        let why = match result {
            // 添加 provider 失败时的说法同 Puffin `SettingsMsg::Created`。
            Err(err) if matches!(write.change, Change::NewProvider { .. }) => {
                ui.said = Some(Err(format!("Could not add it: {err}")));
                return None;
            }
            Err(err) => {
                ui.said = Some(Err(format!("Could not save: {err}")));
                return None;
            }
            Ok(why) => why,
        };
        let text = if !write.said.is_empty() {
            write.said.clone()
        } else if write.applies == Applies::Next && no_session {
            "Saved. Your next session uses it.".to_owned()
        } else {
            write.applies.saved().to_owned()
        };
        if write.back {
            ui.stack.pop();
        }
        if let Some(next) = &write.swap {
            ui.stack.pop();
            // 改规则换了行的 id：段里的光标跟到新规则上，esc 回去时还在它上面。
            if let Page::Rule(id) = next {
                ui.chosen.insert(ui.page(), id.clone());
            }
            ui.stack.push(next.clone());
        }
        if let Some(id) = &write.select {
            ui.chosen.insert(ui.page(), id.clone());
        }
        ui.said = Some(match why {
            // 改规则是先加新的、再删旧的：删没成时新规则已经在，旧的也还在。
            Some(w) if matches!(write.change, Change::ReplaceRule { .. }) => Err(format!(
                "Added the new rule, but the old one is still there: {w}"
            )),
            Some(w) => Err(format!(
                "Saved, but this session keeps compacting where it did: {w}"
            )),
            None => Ok(text),
        });
        ui.typed.insert(row_key(page, key), None);
        Section::of(page::section_of(page)).map(|s| Action::Settings(Request::Read(vec![s])))
    }

    /// 编辑器里改完了文件：重读这一段；权限的配置文件在下次启动时才读。
    pub fn settings_edited(&mut self, path: &str, result: Result<(), String>) -> Option<Action> {
        let ui = self.settings.as_mut()?;
        ui.mode = Mode::Browse;
        match result {
            Ok(()) => {
                let at = page::section_of(&ui.page());
                if at == "permissions" {
                    ui.said = Some(Ok(format!(
                        "Saved {}. ZCode reads it the next time it starts.",
                        rust_cli_tui_kit::app::widgets::settings::file_name(path)
                    )));
                }
                Section::of(at).map(|s| Action::Settings(Request::Read(vec![s])))
            }
            Err(err) => {
                ui.said = Some(Err(format!("Could not edit it: {err}")));
                None
            }
        }
    }

    /// `theme.json` 写不成：样子已经换上，说为什么。
    /// 测试连接的回答：那一行还在测时才记下（换了行、退了页之后回来的不算，Puffin `SettingsMsg::Tested`）。
    pub fn settings_tested(&mut self, key: &str, result: Result<f64, String>) {
        if let Some(ui) = self.settings.as_mut()
            && ui.test.as_ref().is_some_and(|(k, _)| k == key)
        {
            ui.test = Some((key.to_owned(), Some(result)));
        }
    }

    pub fn settings_looked(&mut self, result: Result<(), String>) {
        if let (Some(ui), Err(err)) = (self.settings.as_mut(), result) {
            ui.said = Some(Err(format!("Could not save the theme: {err}")));
        }
    }

    /// `~/.ssh/config` 的主机读到了。
    pub fn settings_hosts(&mut self, hosts: Vec<SshHost>) {
        if let Some(ui) = self.settings.as_mut() {
            ui.ssh_hosts = hosts;
        }
    }

    /// 设置开着时一句结果写在页的输入框上方（Puffin `machines_say`）；关着时为 false，照旧写在输入框上方安静的一行。
    pub fn settings_say(&mut self, said: Result<&str, &str>) -> bool {
        let Some(ui) = self.settings.as_mut() else {
            return false;
        };
        ui.said = Some(said.map(str::to_owned).map_err(str::to_owned));
        true
    }

    /// 添加页加成了：回到段，选中新的那台，说 `Added gpu-2. Connecting…`。设置关着时为 false。
    pub fn settings_machine_added(&mut self, name: &str) -> bool {
        let Some(ui) = self.settings.as_mut() else {
            return false;
        };
        if ui.page() == Page::NewMachine {
            ui.stack.pop();
        }
        for k in [machines::SSH, machines::NAME, machines::DIR] {
            ui.form.remove(k);
        }
        if section_index("machines") == Some(ui.section) {
            ui.chosen
                .insert(Page::Top("machines"), format!("machine:{name}"));
        }
        ui.said = Some(Ok(machine_words::added_short(name)));
        true
    }

    /// 删掉了：在那台的页上时回到段。
    pub fn settings_machine_removed(&mut self, name: &str) {
        if let Some(ui) = self.settings.as_mut()
            && ui.page() == Page::Machine(name.to_owned())
        {
            ui.stack.pop();
        }
    }

    // ---- 在行上动作 ----

    /// 做 `act` 说的；`key` 是它来自的行。
    pub(super) fn settings_run(&mut self, key: &str, act: Act) -> Option<Action> {
        let ui = self.settings.as_mut()?;
        ui.mode = Mode::Browse;
        let page = ui.page();
        match act {
            Act::Nothing => None,
            Act::Write(write) => Some(Action::Settings(Request::Write {
                page,
                key: key.to_owned(),
                write: Box::new(write),
            })),
            Act::Push(next) => {
                if next == Page::NewRule {
                    permissions::clear_form(&mut ui.form);
                }
                ui.stack.push(next.clone());
                ui.said = None;
                ui.test = None;
                shown(&next)
            }
            Act::Confirm { label, note, next } => {
                ui.mode = Mode::Confirming {
                    key: key.to_owned(),
                    at: 1,
                    label,
                    note,
                    act: next,
                };
                None
            }
            Act::Edit(path) => Some(Action::Settings(Request::Edit { path })),
            Act::NewSkill => {
                // 关上设置，把 `$skill-creator ` 放进输入框，交给 agent 写（同 Puffin；以前旧界面没有 `$` 的提及，写的是
                // `/skill-creator `）。
                self.settings_close();
                self.composer.set_text("$skill-creator ");
                None
            }
            Act::SetForm { key: k, value } => {
                ui.form.insert(k, value);
                ui.typed.insert(row_key(&page, key), None);
                None
            }
            Act::Refuse(reason) => {
                ui.said = Some(Err(reason));
                None
            }
            Act::Test { provider, model } => {
                // 同 Puffin：先画在测的样子，回答经 runner 回来（`settings_tested`）。以前在这里直接给出回答，比 Puffin
                // 少了在测的那一帧（孪生 settings-models）。
                ui.test = Some((key.to_owned(), None));
                Some(Action::Settings(Request::Test {
                    key: key.to_owned(),
                    provider,
                    model,
                }))
            }
            Act::SubmitProvider => {
                // 同 Puffin `settings_submit_provider`：没名字当场说，有名字交给 runner 写。
                let name = ui
                    .form
                    .get("np.name")
                    .map(|n| n.trim().to_owned())
                    .unwrap_or_default();
                if name.is_empty() {
                    ui.said = Some(Err("Give it a name first.".to_owned()));
                    return None;
                }
                Some(Action::Settings(Request::Write {
                    page,
                    key: key.to_owned(),
                    write: Box::new(Write::new(Change::NewProvider { name }, Applies::Next)),
                }))
            }
            Act::Machine(a) => self.settings_machine_act(a),
            Act::SignIn(family) => {
                self.settings_close();
                self.door_sign_in(family)
            }
            Act::Look(choice) => {
                // 当场换成选的样子，再写 `theme.json`。
                ui.said = Some(Ok(Applies::Look.saved().to_owned()));
                ui.typed.insert(row_key(&page, key), None);
                self.apply_theme(choice);
                Some(Action::Settings(Request::Look(choice)))
            }
        }
    }

    /// 机器的操作：交给 runner 经机器服务做（`/machines` 同一条路）。
    fn settings_machine_act(&mut self, act: machines::MachineAct) -> Option<Action> {
        use super::machines::MachineRequest;
        use machines::{MachineAct, MachineOp};
        let request = match act {
            MachineAct::Op { op, name, .. } => match op {
                MachineOp::Connect => MachineRequest::Connect(name),
                MachineOp::Disconnect => MachineRequest::Disconnect(name),
                MachineOp::Remove => MachineRequest::Remove(name),
                MachineOp::Retry => MachineRequest::Retry(name),
            },
            MachineAct::Edit {
                name,
                ssh,
                directory,
                reconnect,
            } => MachineRequest::Replace {
                name,
                ssh,
                working_directory: directory,
                reconnect,
            },
            MachineAct::Add => {
                let ui = self.settings.as_mut()?;
                let form = |k: &str| {
                    ui.form
                        .get(k)
                        .map(|v| v.trim().to_owned())
                        .unwrap_or_default()
                };
                let (ssh, given, dir) = (
                    form(machines::SSH),
                    form(machines::NAME),
                    form(machines::DIR),
                );
                if ssh.is_empty() {
                    ui.said = Some(Err(machines::NEEDS_TARGET.to_owned()));
                    return None;
                }
                let name = if given.is_empty() {
                    machine_words::name_from(&ssh)
                } else {
                    given
                };
                MachineRequest::Add {
                    name,
                    ssh,
                    working_directory: (!dir.is_empty()).then_some(dir),
                }
            }
        };
        Some(Action::Machines(request))
    }
}

/// 一页显示了：它读的（段的页每次打开都读一次；Machines 读 `~/.ssh/config` 的主机，Appearance 不读）。
fn shown(page: &Page) -> Option<Action> {
    match page {
        Page::Top("machines") => Some(Action::Settings(Request::Hosts)),
        Page::Top(id) => Section::of(id).map(|s| Action::Settings(Request::Read(vec![s]))),
        _ => None,
    }
}

/// 搜索要每一段。
fn read_all(ui: &Settings) -> Option<Action> {
    let missing: Vec<Section> = Section::ALL
        .into_iter()
        .filter(|s| !ui.data.contains_key(s))
        .collect();
    (!missing.is_empty()).then(|| Action::Settings(Request::Read(missing)))
}
