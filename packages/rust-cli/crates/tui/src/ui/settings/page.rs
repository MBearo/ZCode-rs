//! 设置页的模型（docs/specs/rust-cli/tui-kit.md §4.2「设置」；照 Puffin `shell/settings/page.rs`）：段、段下的页与它们的行，
//! 从读到的数据建。纯函数：页是读到的东西的函数，一处改动处处同时显示。画法是 kit 的 `widgets::settings::view`（行先映射成
//! kit 的中性的 [`kit::Row`]），按键在 `keys.rs`，动作在 `mod.rs`。
//!
//! 两套界面不能互相依赖：行的措辞照抄 Puffin 的同名函数（同画面测试 `crates/cli/tests/same_picture_pages.rs` 比两边的行）。
//! 与 Puffin 的不同（tui-kit.md §4.3 登记）：provider 的页与添加只说这一版还没有；General 的界面语言改了屏幕也换（旧界面
//! 有中文界面），说明照实写。
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use rust_cli_kernel_api::permission::{RuleBehavior, SourcedRule};
use rust_cli_tui_kit::app::widgets::settings::{self as kit, Applies, Opt, Part};
use rust_cli_tui_kit::theme::scheme::ThemeChoice;

use super::data::{self, Data, Section, SectionData};
use super::machines::MachineAct;
use crate::machines::{MachineRow, SshHost};

/// 页在哪里。段自己的页是 [`Page::Top`]；其余压在它上面。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Page {
    Top(&'static str),
    Subagent(String),
    Mcp(String),
    Skill(String),
    Command(String),
    /// 第几个 hook（内核的 hook 没有 id）。
    Hook(usize),
    Machine(String),
    NewMachine,
    /// 一条权限规则的页，按它在段里的行 id。
    Rule(String),
    NewRule,
    /// 内置的家族（Puffin `Page::Family`）。
    Family(String),
    /// 你的一个 provider、新 provider、给它起名与一个模型（Puffin 的同名页）。
    Provider(String),
    NewProvider,
    ProviderForm,
    Model {
        provider: String,
        model: String,
    },
}

/// runner 写的一个改动。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// None 删掉这个键，回到模型上限。
    AutoCompact {
        at: Option<u64>,
    },
    Locale {
        locale: String,
    },
    DefaultModel {
        pick: data::Pick,
    },
    McpEnabled {
        name: String,
        enabled: bool,
    },
    McpReconnect {
        name: String,
    },
    AddRule {
        behavior: RuleBehavior,
        rule: String,
    },
    RemoveRule {
        rule: SourcedRule,
    },
    /// 改种类或规则串：先加新的，再删旧的（内核没有替换）。
    ReplaceRule {
        old: SourcedRule,
        behavior: RuleBehavior,
        rule: String,
    },
    /// 退出 Coding Plan 账号（旧界面的 `/logout`：platform 一次退出所有家族）。
    SignOut,
    /// 换家族的计划（Connection 的选项值）。下面五个两边的 host 都还没有，runner 回答这一版还没有，同 Puffin 的 host
    /// （tui-kit.md §4.5）：经 runner 写，回答晚一步到，与 Puffin 同一帧。
    Connection {
        family: crate::services::Family,
        plan: String,
    },
    /// 开关一个模型。
    Model {
        provider: String,
        model: String,
        enabled: bool,
    },
    /// 改你的 provider 的一项。
    Provider {
        provider: String,
        edit: ProviderEdit,
    },
    NewProvider {
        name: String,
    },
    DeleteProvider {
        provider: String,
    },
}

/// provider 页上改的那一项（Puffin `SettingsChange::Provider` 的各个可选项）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderEdit {
    Name(String),
    /// API 格式的值（`openai-chat-completions` 等）。
    Format(String),
    BaseUrl(String),
    ApiKey(String),
    Enabled(bool),
}

/// 一个改动：`said` 代替生效的那句话，`back` 是之后离开这一页；`swap` 是之后把这一页换成的页，`select` 是之后选中的行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Write {
    pub change: Change,
    pub applies: Applies,
    pub said: String,
    pub back: bool,
    pub swap: Option<Page>,
    pub select: Option<String>,
}

impl Write {
    pub fn new(change: Change, applies: Applies) -> Write {
        Write {
            change,
            applies,
            said: String::new(),
            back: false,
            swap: None,
            select: None,
        }
    }

    pub fn saying(mut self, said: impl Into<String>) -> Write {
        self.said = said.into();
        self
    }

    pub fn going_back(mut self) -> Write {
        self.back = true;
        self
    }

    pub fn swapping(mut self, page: Page) -> Write {
        self.swap = Some(page);
        self
    }

    pub fn selecting(mut self, row: impl Into<String>) -> Write {
        self.select = Some(row.into());
        self
    }
}

/// 在一行上动作时做什么。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    Write(Write),
    Push(Page),
    /// 先问：`label` 是动作自己的那一行，`note` 说它意味着什么。
    Confirm {
        label: String,
        note: String,
        next: Box<Act>,
    },
    /// 在你的编辑器里打开文件。
    Edit(String),
    /// 关上设置，把 `$skill-creator ` 放进输入框（同 Puffin）。
    NewSkill,
    /// 在页上留一个值（表单）。
    SetForm {
        key: String,
        value: String,
    },
    /// 什么也不做，只在输入框上方说为什么。
    Refuse(String),
    /// 样子存在 `theme.json`。
    Look(ThemeChoice),
    Machine(MachineAct),
    /// 登录这个家族：关上设置，走旧界面的登录（`/login`）。
    SignIn(crate::services::Family),
    /// 测试这个模型的连接（Puffin `Act::Test`）；结果画在这一行下面。
    Test {
        provider: String,
        model: String,
    },
    /// 按表单添加 provider（Puffin `Act::SubmitProvider`）。
    SubmitProvider,
    Nothing,
}

pub type SetFn = Arc<dyn Fn(bool) -> Act + Send + Sync>;
pub type PickFn = Arc<dyn Fn(&str) -> Act + Send + Sync>;

/// 文字的值（Puffin `TextRow`）。
#[derive(Clone)]
pub struct TextRow {
    pub key: String,
    pub label: String,
    pub value: String,
    pub desc: String,
    pub applies: Applies,
    pub save: PickFn,
    pub unit: String,
    pub placeholder: String,
    pub empty: String,
    pub secret: bool,
    pub hint: String,
    pub note: String,
    pub stuck: bool,
}

impl TextRow {
    pub fn new(
        key: &str,
        label: &str,
        value: impl Into<String>,
        desc: &str,
        applies: Applies,
        save: impl Fn(&str) -> Act + Send + Sync + 'static,
    ) -> TextRow {
        TextRow {
            key: key.to_owned(),
            label: label.to_owned(),
            value: value.into(),
            desc: desc.to_owned(),
            applies,
            save: Arc::new(save),
            unit: String::new(),
            placeholder: String::new(),
            empty: "not set".to_owned(),
            secret: false,
            hint: String::new(),
            note: String::new(),
            stuck: false,
        }
    }
}

/// 清单里的一项（Puffin `ItemRow`）：⏎ 做 `open`，空格做 `flip`，⌫ 做 `remove`。
#[derive(Clone, Default)]
pub struct ItemRow {
    pub key: String,
    pub name: String,
    pub toggle: Option<bool>,
    pub scope: String,
    pub status: Vec<Part>,
    pub desc: String,
    pub open: Option<Act>,
    pub flip: Option<Act>,
    pub detail: Vec<Part>,
    pub remove: Option<Act>,
    pub enter: String,
    pub words: String,
}

#[derive(Clone)]
pub enum Row {
    Heading(String),
    Note(String),
    /// 开关（Puffin `Row::Switch`）：⏎ 与空格都做 `set(!on)`。
    Switch {
        key: String,
        label: String,
        on: bool,
        desc: String,
        applies: Applies,
        set: SetFn,
    },
    Choice {
        key: String,
        label: String,
        options: Vec<Opt>,
        current: String,
        default: String,
        desc: String,
        applies: Applies,
        pick: PickFn,
    },
    Text(TextRow),
    /// 缺省模型的选择：（provider，模型，档位）。
    Picker {
        key: String,
        label: String,
        value: Option<data::Pick>,
        desc: String,
    },
    Editor {
        key: String,
        label: String,
        path: String,
        desc: String,
    },
    Info {
        label: String,
        value: Vec<Part>,
    },
    Item(Box<ItemRow>),
    Action {
        key: String,
        label: String,
        desc: String,
        act: Act,
        plus: bool,
        danger: bool,
        aside: String,
        off: bool,
    },
}

impl Row {
    pub fn item(it: ItemRow) -> Row {
        Row::Item(Box::new(it))
    }

    pub fn action(key: &str, label: impl Into<String>, desc: &str, act: Act) -> Row {
        Row::Action {
            key: key.to_owned(),
            label: label.into(),
            desc: desc.to_owned(),
            act,
            plus: false,
            danger: false,
            aside: String::new(),
            off: false,
        }
    }

    pub fn plus(key: &str, label: &str, desc: &str, act: Act) -> Row {
        match Row::action(key, label, desc, act) {
            Row::Action {
                key,
                label,
                desc,
                act,
                ..
            } => Row::Action {
                key,
                label,
                desc,
                act,
                plus: true,
                danger: false,
                aside: String::new(),
                off: false,
            },
            other => other,
        }
    }

    pub fn danger(key: &str, label: &str, act: Act) -> Row {
        Row::Action {
            key: key.to_owned(),
            label: label.to_owned(),
            desc: String::new(),
            act,
            plus: false,
            danger: true,
            aside: String::new(),
            off: false,
        }
    }

    pub fn info(label: &str, value: Vec<Part>) -> Row {
        Row::Info {
            label: label.to_owned(),
            value,
        }
    }

    pub fn selectable(&self) -> bool {
        !matches!(self, Row::Heading(_) | Row::Note(_) | Row::Info { .. })
    }

    /// 页重建之后行的身份。
    pub fn id(&self) -> String {
        match self {
            Row::Switch { key, .. }
            | Row::Choice { key, .. }
            | Row::Picker { key, .. }
            | Row::Editor { key, .. }
            | Row::Action { key, .. } => key.clone(),
            Row::Text(t) => t.key.clone(),
            Row::Item(it) => it.key.clone(),
            Row::Info { label, .. } => format!("info:{label}"),
            Row::Heading(text) => format!("heading:{text}"),
            Row::Note(text) => format!("note:{text}"),
        }
    }

    /// 搜索匹配的：名字、说明与选项。
    pub fn words(&self) -> String {
        match self {
            Row::Switch { label, desc, .. }
            | Row::Picker { label, desc, .. }
            | Row::Editor { label, desc, .. }
            | Row::Action { label, desc, .. } => format!("{label} {desc}"),
            Row::Choice {
                label,
                desc,
                options,
                ..
            } => {
                let mut all = vec![label.clone(), desc.clone()];
                all.extend(options.iter().map(|o| o.label.clone()));
                all.join(" ")
            }
            Row::Text(t) => format!("{} {}", t.label, t.desc),
            Row::Item(it) => format!("{} {} {}", it.name, it.desc, it.words),
            _ => String::new(),
        }
    }

    /// 画的时候的样子：kit 的中性的行。
    pub fn kit(&self) -> kit::Row<'_> {
        match self {
            Row::Heading(t) => kit::Row::Heading(t),
            Row::Note(t) => kit::Row::Note(t),
            Row::Switch {
                label,
                on,
                desc,
                applies,
                ..
            } => kit::Row::Switch {
                label,
                on: *on,
                desc,
                applies: *applies,
            },
            Row::Choice {
                label,
                options,
                current,
                default,
                desc,
                applies,
                ..
            } => kit::Row::Choice {
                label,
                options,
                current,
                default,
                desc,
                applies: *applies,
            },
            Row::Text(t) => kit::Row::Text(kit::Text {
                label: &t.label,
                value: &t.value,
                desc: &t.desc,
                applies: t.applies,
                unit: &t.unit,
                empty: &t.empty,
                secret: t.secret,
                note: &t.note,
            }),
            Row::Picker {
                label, value, desc, ..
            } => kit::Row::Picker {
                label,
                value: value
                    .as_ref()
                    .map(|p| (p.model.as_str(), p.effort.as_str())),
                desc,
            },
            Row::Editor {
                label, path, desc, ..
            } => kit::Row::Editor { label, path, desc },
            Row::Info { label, value } => kit::Row::Info { label, value },
            Row::Item(it) => kit::Row::Item(kit::Item {
                name: &it.name,
                toggle: it.toggle,
                scope: &it.scope,
                status: &it.status,
                desc: &it.desc,
                opens: it.open.is_some(),
                detail: &it.detail,
            }),
            Row::Action {
                label,
                desc,
                plus,
                danger,
                aside,
                off,
                ..
            } => kit::Row::Action(kit::Action {
                label,
                desc,
                plus: *plus,
                danger: *danger,
                aside,
                off: *off,
                signs_in: false,
            }),
        }
    }
}

/// 一页：面包屑、说明与行。
#[derive(Clone)]
pub struct View {
    pub crumbs: Vec<String>,
    pub desc: String,
    pub rows: Vec<Row>,
}

impl View {
    pub fn new(crumbs: Vec<String>, desc: impl Into<String>, rows: Vec<Row>) -> View {
        View {
            crumbs,
            desc: desc.into(),
            rows,
        }
    }

    pub fn loading(crumb: impl Into<String>) -> View {
        View::new(vec![crumb.into()], "", vec![loading()])
    }
}

pub fn loading() -> Row {
    Row::Note("Reading…".to_owned())
}

/// 页从什么建：读到的一切，与页自己的状态（表单）。
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub data: &'a Data,
    pub form: &'a BTreeMap<String, String>,
    pub home: Option<&'a str>,
    pub look: ThemeChoice,
    /// 远端机器：没有机器服务时没有。
    pub machines: Option<&'a [MachineRow]>,
    pub ssh_hosts: &'a [SshHost],
    /// 连接中的机器从什么时候起在连（毫秒）。
    pub connecting: &'a HashMap<String, u64>,
    pub now_ms: u64,
    pub editing: Option<(&'a str, &'a str)>,
}

impl Ctx<'_> {
    /// `path` 里的家目录写成 `~`。
    pub fn tilde(&self, path: &str) -> String {
        match self.home.filter(|h| !h.is_empty()) {
            Some(h)
                if path == h
                    || path.starts_with(&format!("{h}/"))
                    || path.starts_with(&format!("{h}\\")) =>
            {
                format!("~{}", &path[h.len()..])
            }
            _ => path.to_owned(),
        }
    }

    pub fn get(&self, s: Section) -> Option<&SectionData> {
        self.data.get(&s)
    }

    pub fn models(&self) -> Option<&data::Models> {
        match self.get(Section::Models) {
            Some(SectionData::Models(m)) => Some(m),
            _ => None,
        }
    }

    pub fn permissions(&self) -> Option<&data::Permissions> {
        match self.get(Section::Permissions) {
            Some(SectionData::Permissions(p)) => Some(p),
            _ => None,
        }
    }

    pub fn subagents(&self) -> Option<&[data::Subagent]> {
        match self.get(Section::Subagents) {
            Some(SectionData::Subagents(x)) => Some(x),
            _ => None,
        }
    }

    pub fn mcp(&self) -> Option<&[data::McpServer]> {
        match self.get(Section::Mcp) {
            Some(SectionData::Mcp(x)) => Some(x),
            _ => None,
        }
    }

    pub fn skills(&self) -> Option<&[data::Skill]> {
        match self.get(Section::Skills) {
            Some(SectionData::Skills(x)) => Some(x),
            _ => None,
        }
    }

    pub fn commands(&self) -> Option<&[data::CommandItem]> {
        match self.get(Section::Commands) {
            Some(SectionData::Commands(x)) => Some(x),
            _ => None,
        }
    }

    pub fn hooks(&self) -> Option<&[data::Hook]> {
        match self.get(Section::Hooks) {
            Some(SectionData::Hooks(x)) => Some(x),
            _ => None,
        }
    }

    pub fn draft(&self, key: &str) -> Option<&str> {
        self.editing.filter(|(k, _)| *k == key).map(|(_, d)| d)
    }

    pub fn field(&self, key: &str, default: &str) -> String {
        self.form
            .get(key)
            .cloned()
            .unwrap_or_else(|| default.to_owned())
    }
}

/// 在 `page` 的页；数据还没读到的页说出来。
pub fn view(page: &Page, ctx: &Ctx<'_>) -> View {
    use super::{lists, machines, permissions};
    match page {
        Page::Top(id) => View::new(Vec::new(), kit::section(id).desc, top(id, ctx)),
        Page::Subagent(name) => lists::subagent(name, ctx),
        Page::Mcp(name) => lists::mcp_server(name, ctx),
        Page::Skill(name) => lists::skill(name, ctx),
        Page::Command(name) => lists::command(name, ctx),
        Page::Hook(i) => lists::hook(*i, ctx),
        Page::Machine(name) => machines::machine(name, ctx),
        Page::NewMachine => machines::new_machine(ctx),
        Page::Rule(id) => permissions::rule(id, ctx),
        Page::NewRule => permissions::new_rule(ctx),
        Page::Family(id) => super::sections::family(id, ctx),
        Page::Provider(id) => super::providers::provider(id, ctx),
        Page::NewProvider => super::providers::new_provider(),
        Page::ProviderForm => super::providers::provider_form(ctx),
        Page::Model { provider, model } => super::providers::model(provider, model, ctx),
    }
}

/// 一页属于哪一段：它读的与重读的。
pub fn section_of(page: &Page) -> &'static str {
    match page {
        Page::Top(id) => kit::section(id).id,
        Page::Subagent(_) => "subagents",
        Page::Mcp(_) => "mcp",
        Page::Skill(_) => "skills",
        Page::Command(_) => "commands",
        Page::Hook(_) => "hooks",
        Page::Machine(_) | Page::NewMachine => "machines",
        Page::Rule(_) | Page::NewRule => "permissions",
        Page::Family(_)
        | Page::Provider(_)
        | Page::NewProvider
        | Page::ProviderForm
        | Page::Model { .. } => "models",
    }
}

/// 一段自己的行。
pub fn top(id: &str, ctx: &Ctx<'_>) -> Vec<Row> {
    use super::{lists, machines, permissions, sections};
    let section = Section::of(id).and_then(|s| ctx.get(s));
    match (id, section) {
        ("appearance", _) => sections::appearance(ctx.look),
        ("machines", _) => machines::machines(ctx),
        (_, None) => vec![loading()],
        (_, Some(SectionData::General(g))) => sections::general(g),
        (_, Some(SectionData::Permissions(p))) => permissions::section(p, ctx),
        (_, Some(SectionData::Models(m))) => sections::models(m),
        (_, Some(SectionData::Memory(m))) => sections::memory(m, ctx),
        (_, Some(SectionData::Subagents(a))) => lists::subagents(a),
        (_, Some(SectionData::Mcp(s))) => lists::mcp(s),
        (_, Some(SectionData::Skills(s))) => lists::skills(s),
        (_, Some(SectionData::Commands(c))) => lists::commands(c),
        (_, Some(SectionData::Hooks(h))) => lists::hooks(h),
    }
}

/// 搜索找到的每个段级的行，带它的段，按段的次序。
pub fn search(query: &str, ctx: &Ctx<'_>) -> Vec<(&'static str, Row)> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    kit::SECTIONS
        .iter()
        .flat_map(|s| {
            top(s.id, ctx)
                .into_iter()
                .filter(|r| r.selectable() && r.words().to_lowercase().contains(&needle))
                .map(move |r| (s.id, r))
        })
        .collect()
}
