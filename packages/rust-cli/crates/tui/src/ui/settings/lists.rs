//! 设置页的只读清单：子代理、MCP、技能、命令与 hook（docs/specs/rust-cli/tui-kit.md §4.2「设置」）。照抄 Puffin
//! `shell/settings/lists.rs`（两套界面不能互相依赖），类型换成旧界面自己读的（`data.rs`）。
//!
//! 与 Puffin 的不同：内核的 MCP 快照没有登录页，服务器的行不会说要你登录。
use rust_cli_tui_kit::app::widgets::settings::{Applies, Ink, Part, file_name, plural};

use super::data::{CommandItem, Hook, McpKind, McpServer, McpState, Scope, Skill, Subagent};
use super::page::{Act, Change, Ctx, ItemRow, Page, Row, View, Write};

/// 下一个会话起生效的改动。
pub(super) fn next(change: Change) -> Act {
    Act::Write(Write::new(change, Applies::Next))
}

/// 值为空时说 `none` 的信息行。
pub(super) fn info_text(label: &str, value: &str) -> Row {
    if value.is_empty() {
        Row::info(label, vec![Part::italic("none", Ink::Fg3)])
    } else {
        Row::info(label, vec![Part::new(value, Ink::Fg2)])
    }
}

/// 定义它的文件：⏎ 在编辑器里打开。
pub(super) fn editor(key: &str, label: &str, path: &str) -> Row {
    Row::Editor {
        key: key.to_owned(),
        label: label.to_owned(),
        path: path.to_owned(),
        desc: String::new(),
    }
}

/// 服务器的状态，行上的说法；措辞在 kit。
pub(super) fn mcp_status_parts(s: &McpServer) -> Vec<Part> {
    use rust_cli_tui_kit::app::widgets::settings::{McpStatus, mcp_status_parts};
    let status = if !s.enabled {
        McpStatus::Off
    } else {
        match s.state {
            McpState::Connected => McpStatus::Connected { tools: s.tools },
            McpState::Failed => McpStatus::Failed,
            McpState::Disabled => McpStatus::Off,
            McpState::Connecting => McpStatus::Connecting,
        }
    };
    mcp_status_parts(status)
}

fn grouped_items<T>(rows: &mut Vec<Row>, title: &str, xs: &[&T], item: impl Fn(&T) -> Row) {
    if !xs.is_empty() {
        rows.push(Row::Heading(title.to_owned()));
        rows.extend(xs.iter().map(|x| item(x)));
    }
}

fn subagent_item(a: &Subagent) -> Row {
    let status = a
        .model
        .clone()
        .unwrap_or_else(|| "inherits the model".to_owned());
    Row::item(ItemRow {
        key: format!("agent:{}", a.name),
        name: a.name.clone(),
        toggle: None,
        scope: a.scope.name().to_owned(),
        status: vec![Part::new(status, Ink::Fg3)],
        desc: a.description.clone(),
        open: Some(Act::Push(Page::Subagent(a.name.clone()))),
        flip: None,
        detail: Vec::new(),
        remove: None,
        enter: String::new(),
        words: String::new(),
    })
}

pub fn subagents(all: &[Subagent]) -> Vec<Row> {
    let of = |f: fn(Scope) -> bool| all.iter().filter(|a| f(a.scope)).collect::<Vec<_>>();
    let mine = of(|s| matches!(s, Scope::User | Scope::Project));
    let mut rows = vec![Row::Heading("Yours".to_owned())];
    if mine.is_empty() {
        rows.push(Row::Note(
            "None yet. Each is a Markdown file in your agents folder.".to_owned(),
        ));
    }
    rows.extend(mine.iter().map(|a| subagent_item(a)));
    grouped_items(
        &mut rows,
        "From plugins",
        &of(|s| s == Scope::Plugin),
        subagent_item,
    );
    grouped_items(
        &mut rows,
        "Built in",
        &of(|s| s == Scope::Builtin),
        subagent_item,
    );
    rows
}

pub fn subagent(name: &str, ctx: &Ctx<'_>) -> View {
    let Some(a) = ctx
        .subagents()
        .and_then(|xs| xs.iter().find(|a| a.name == name))
    else {
        return View::loading("Subagent");
    };
    let tools = a
        .tools
        .as_ref()
        .map_or_else(|| "All tools".to_owned(), |ts| plural(ts.len(), "tool"));
    let mut rows = vec![
        info_text("Description", &a.description),
        Row::info(
            "Model",
            vec![Part::new(
                a.model
                    .clone()
                    .unwrap_or_else(|| "Inherit default".to_owned()),
                Ink::Fg2,
            )],
        ),
        Row::info("Allowed tools", vec![Part::new(tools, Ink::Fg2)]),
    ];
    if !a.path.is_empty() {
        rows.push(editor("prompt", "System prompt", &a.path));
    }
    let desc = if a.path.is_empty() {
        "Built in: it can't be changed here.".to_owned()
    } else {
        ctx.tilde(&a.path)
    };
    View::new(vec![a.name.clone()], desc, rows)
}

pub fn mcp(servers: &[McpServer]) -> Vec<Row> {
    if servers.is_empty() {
        return vec![Row::Note("No servers are set up.".to_owned())];
    }
    servers
        .iter()
        .map(|s| {
            Row::item(ItemRow {
                key: format!("mcp:{}", s.name),
                name: s.name.clone(),
                toggle: Some(s.enabled),
                scope: s.scope.name().to_owned(),
                status: mcp_status_parts(s),
                desc: s.target.clone(),
                open: Some(Act::Push(Page::Mcp(s.name.clone()))),
                detail: Vec::new(),
                remove: None,
                enter: String::new(),
                words: String::new(),
                flip: Some(next(Change::McpEnabled {
                    name: s.name.clone(),
                    enabled: !s.enabled,
                })),
            })
        })
        .collect()
}

pub fn mcp_server(name: &str, ctx: &Ctx<'_>) -> View {
    let Some(s) = ctx.mcp().and_then(|xs| xs.iter().find(|s| s.name == name)) else {
        return View::loading(name);
    };
    let mut status = mcp_status_parts(s);
    if !s.error.is_empty() {
        status.push(Part::new(format!(" · {}", s.error), Ink::Fg3));
    }
    let mut rows = vec![Row::info("Status", status)];
    let (kind, where_) = match s.kind {
        McpKind::Stdio => ("stdio", "Command"),
    };
    rows.push(Row::info("Type", vec![Part::new(kind, Ink::Fg2)]));
    rows.push(info_text(where_, &s.target));
    if let Some(m) = &s.machine {
        rows.push(Row::info("Machine", vec![Part::new(m.clone(), Ink::Fg2)]));
    }
    rows.push(Row::action(
        "reconnect",
        "Reconnect",
        "Starts the server again and reads its tools.",
        Act::Write(
            Write::new(
                Change::McpReconnect {
                    name: s.name.clone(),
                },
                Applies::Now,
            )
            .saying(format!("Reconnecting {}.", s.name)),
        ),
    ));
    let desc = match s.scope {
        Scope::Project => concat!(
            "In this project's ",
            rust_cli_base::dot_dir!(),
            "/config.json."
        ),
        Scope::Plugin => "From a plugin.",
        _ => "In your user config, for every project.",
    };
    View::new(vec![s.name.clone()], desc, rows)
}

pub fn skills(all: &[Skill]) -> Vec<Row> {
    let mut rows: Vec<Row> = all
        .iter()
        .map(|s| {
            Row::item(ItemRow {
                key: format!("skill:{}", s.name),
                name: s.name.clone(),
                toggle: None,
                scope: s.scope.name().to_owned(),
                status: vec![Part::new(s.description.clone(), Ink::Fg3)],
                desc: String::new(),
                open: Some(Act::Push(Page::Skill(s.name.clone()))),
                flip: None,
                detail: Vec::new(),
                remove: None,
                enter: String::new(),
                words: String::new(),
            })
        })
        .collect();
    rows.push(Row::plus(
        "newSkill",
        "New skill",
        "Closes settings and asks the agent to write one with $skill-creator.",
        Act::NewSkill,
    ));
    rows
}

pub fn skill(name: &str, ctx: &Ctx<'_>) -> View {
    let Some(s) = ctx
        .skills()
        .and_then(|xs| xs.iter().find(|s| s.name == name))
    else {
        return View::loading("Skill");
    };
    let mut rows = vec![info_text("Description", &s.description)];
    if !s.path.is_empty() {
        rows.push(editor("file", file_name(&s.path), &s.path));
    }
    View::new(vec![s.name.clone()], ctx.tilde(&s.path), rows)
}

pub fn commands(all: &[CommandItem]) -> Vec<Row> {
    if all.is_empty() {
        return vec![Row::Note(
            "No commands yet. Each is a Markdown file in your commands folder.".to_owned(),
        )];
    }
    all.iter()
        .map(|c| {
            let gap = if c.argument_hint.is_empty() { "" } else { "  " };
            let status = vec![
                Part::italic(c.argument_hint.clone(), Ink::Fg3),
                Part::new(format!("{gap}{}", c.description), Ink::Fg3),
            ]
            .into_iter()
            .filter(|p| !p.text.is_empty())
            .collect();
            Row::item(ItemRow {
                key: format!("command:{}", c.name),
                name: format!("/{}", c.name),
                toggle: None,
                scope: c.scope.name().to_owned(),
                status,
                desc: String::new(),
                open: Some(Act::Push(Page::Command(c.name.clone()))),
                flip: None,
                detail: Vec::new(),
                remove: None,
                enter: String::new(),
                words: String::new(),
            })
        })
        .collect()
}

pub fn command(name: &str, ctx: &Ctx<'_>) -> View {
    let Some(c) = ctx
        .commands()
        .and_then(|xs| xs.iter().find(|c| c.name == name))
    else {
        return View::loading("Command");
    };
    let mut rows = vec![
        info_text("Description", &c.description),
        info_text("Argument hint", &c.argument_hint),
    ];
    if !c.path.is_empty() {
        rows.push(editor("prompt", "Prompt", &c.path));
    }
    View::new(vec![format!("/{}", c.name)], ctx.tilde(&c.path), rows)
}

fn hook_item(i: usize, h: &Hook) -> Row {
    let gap = if h.matcher.is_empty() { "" } else { "  " };
    let status = vec![
        Part::new(h.matcher.clone(), Ink::Fg2),
        Part::new(format!("{gap}{}", h.command), Ink::Fg3),
    ]
    .into_iter()
    .filter(|p| !p.text.is_empty())
    .collect();
    Row::item(ItemRow {
        key: format!("hook:{i}"),
        name: h.event.clone(),
        toggle: None,
        scope: h.scope.name().to_owned(),
        status,
        desc: String::new(),
        open: Some(Act::Push(Page::Hook(i))),
        flip: None,
        detail: Vec::new(),
        remove: None,
        enter: String::new(),
        words: String::new(),
    })
}

pub fn hooks(all: &[Hook]) -> Vec<Row> {
    if all.is_empty() {
        return vec![Row::Note("No hooks are set up.".to_owned())];
    }
    let mine: Vec<Row> = all
        .iter()
        .enumerate()
        .filter(|(_, h)| h.scope != Scope::Plugin)
        .map(|(i, h)| hook_item(i, h))
        .collect();
    let plugins: Vec<Row> = all
        .iter()
        .enumerate()
        .filter(|(_, h)| h.scope == Scope::Plugin)
        .map(|(i, h)| hook_item(i, h))
        .collect();
    let mut rows = mine;
    if !plugins.is_empty() {
        rows.push(Row::Heading("From plugins".to_owned()));
        rows.extend(plugins);
    }
    rows
}

pub fn hook(i: usize, ctx: &Ctx<'_>) -> View {
    let Some(h) = ctx.hooks().and_then(|xs| xs.get(i)) else {
        return View::loading("Hook");
    };
    let timeout = h.timeout.map(|t| format!("{t} s")).unwrap_or_default();
    let mut rows = vec![
        Row::info("Event", vec![Part::new(h.event.clone(), Ink::Fg2)]),
        if h.matcher.is_empty() {
            Row::info("Matcher", vec![Part::italic("every tool", Ink::Fg3)])
        } else {
            info_text("Matcher", &h.matcher)
        },
        info_text("Command", &h.command),
        info_text("Timeout", &timeout),
    ];
    if !h.source.is_empty() {
        rows.push(editor("source", "Config file", &h.source));
    }
    let desc = if h.source.is_empty() {
        String::new()
    } else {
        format!("In {}.", ctx.tilde(&h.source))
    };
    View::new(vec![h.event.clone()], desc, rows)
}
