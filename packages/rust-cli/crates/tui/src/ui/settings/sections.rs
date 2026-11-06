//! 设置页的几个段自己的行：Appearance、General、Models 与 Memory（docs/specs/rust-cli/tui-kit.md §4.2「设置」；照抄 Puffin
//! `shell/settings/page.rs` 的同名函数）。从 `page.rs` 分出（文件行数的上限）。
use std::sync::Arc;

use rust_cli_tui_kit::app::widgets::settings::{self as kit, Applies, Ink, Opt, Part};
use rust_cli_tui_kit::theme::scheme::{Ground, Scheme, ThemeChoice, ThemeMode};
use rust_cli_tui_kit::theme::schemes;

use super::data;
use super::page::{Act, Change, Ctx, ItemRow, Page, Row, TextRow, View, Write};
use crate::services::{FamilyStatus, PlanConnection, TeamStatus};

/// TUI 自己的段：三个选择，存在 `theme.json`（Puffin `appearance`）。
pub(super) fn appearance(look: ThemeChoice) -> Vec<Row> {
    let d = ThemeChoice::default();
    let themes = schemes::ids()
        .iter()
        .map(|id| {
            let sides = schemes::by_id(id)
                .map_or_else(|| "Follows your terminal".to_owned(), Scheme::sides);
            Opt::new(*id, schemes::name(id)).desc(sides)
        })
        .collect();
    vec![
        Row::Choice {
            key: "theme".to_owned(),
            label: "Theme".to_owned(),
            options: themes,
            current: look.theme.to_owned(),
            default: d.theme.to_owned(),
            desc: "The colours of the screen.".to_owned(),
            applies: Applies::Look,
            pick: Arc::new(move |v| {
                let id = schemes::ids().iter().copied().find(|id| *id == v);
                id.map_or(Act::Nothing, |theme| {
                    Act::Look(ThemeChoice { theme, ..look })
                })
            }),
        },
        Row::Choice {
            key: "themeMode".to_owned(),
            label: "Light or dark".to_owned(),
            options: vec![
                Opt::new("auto", "Auto").desc("Follows your terminal"),
                Opt::new("light", "Light"),
                Opt::new("dark", "Dark"),
            ],
            current: look.mode.as_str().to_owned(),
            default: d.mode.as_str().to_owned(),
            desc: "Which side of the theme shows.".to_owned(),
            applies: Applies::Look,
            pick: Arc::new(move |v| {
                let mode = ThemeMode::parse(v).unwrap_or(d.mode);
                Act::Look(ThemeChoice { mode, ..look })
            }),
        },
        Row::Choice {
            key: "themeGround".to_owned(),
            label: "Background".to_owned(),
            options: vec![
                Opt::new("theme", "Theme's")
                    .desc("The theme fills the screen and the terminal's margin"),
                Opt::new("terminal", "Terminal's")
                    .desc("Only the theme's inks; the ground and text are the terminal's"),
            ],
            current: look.ground.as_str().to_owned(),
            default: d.ground.as_str().to_owned(),
            desc: "Who paints the screen under a named theme.".to_owned(),
            applies: Applies::Look,
            pick: Arc::new(move |v| {
                let ground = Ground::ALL
                    .into_iter()
                    .find(|g| g.as_str() == v)
                    .unwrap_or(d.ground);
                Act::Look(ThemeChoice { ground, ..look })
            }),
        },
    ]
}

/// General：Auto-compact at（读得到配置时）与界面语言（Puffin `general`）。旧界面的界面语言连屏幕一起换，说明照实写。
pub(super) fn general(g: &data::General) -> Vec<Row> {
    let mut rows = Vec::new();
    if let Some(at) = g.auto_compact {
        let mut t = TextRow::new(
            "autoCompact",
            "Auto-compact at",
            at.map(super::tokens::exact).unwrap_or_default(),
            "Compact the conversation once it holds this many tokens. A model with a smaller window compacts at its own limit.",
            Applies::Now,
            |v| {
                if v.trim().is_empty() {
                    Act::Write(Write::new(Change::AutoCompact { at: None }, Applies::Now))
                } else {
                    super::tokens::parse(v).map_or_else(
                        || Act::Refuse("Write a number of tokens, like 400k.".to_owned()),
                        |n| {
                            Act::Write(Write::new(
                                Change::AutoCompact { at: Some(n) },
                                Applies::Now,
                            ))
                        },
                    )
                }
            },
        );
        t.unit = "tokens".to_owned();
        t.placeholder = "400k".to_owned();
        t.empty = "model limit".to_owned();
        rows.push(Row::Text(t));
    }
    rows.push(Row::Choice {
        key: "locale".to_owned(),
        label: "Language".to_owned(),
        options: vec![
            Opt::new("auto", "Auto").desc("Follows your system"),
            Opt::new("en-US", "English"),
            Opt::new("zh-CN", "简体中文"),
        ],
        current: g.locale.clone(),
        default: "auto".to_owned(),
        // 旧界面有中文界面：屏幕跟着换（Puffin 写 “The screen stays in English for now.”）。
        desc: "The language of the screen and of the agent's reminders.".to_owned(),
        applies: Applies::Now,
        pick: Arc::new(|v| {
            Act::Write(Write::new(
                Change::Locale {
                    locale: v.to_owned(),
                },
                Applies::Now,
            ))
        }),
    });
    rows
}

/// Models：缺省模型、内置的家族（账号端口给的）、模型目录里的 provider 与添加（Puffin `models`）。provider 的页、新
/// provider 与模型页在 `providers.rs`；开关 provider 经 runner 写，runner 回答这一版还没有，同 Puffin 的 host
/// （tui-kit.md §4.5）。
pub(super) fn models(m: &data::Models) -> Vec<Row> {
    let mut rows = vec![
        Row::Picker {
            key: "defaultModel".to_owned(),
            label: "Default model".to_owned(),
            value: m.default_model.clone(),
            desc: "The model new sessions start with. /model changes only one session.".to_owned(),
        },
        Row::Heading("Providers".to_owned()),
    ];
    rows.extend(m.families.iter().map(|f| {
        Row::item(ItemRow {
            key: format!("family:{}", f.family.as_str()),
            name: f.name.clone(),
            status: family_status(f),
            desc: format!("Built in. Sign in with your {} account.", f.name),
            open: Some(Act::Push(Page::Family(f.family.as_str().to_owned()))),
            ..ItemRow::default()
        })
    }));
    rows.push(Row::Heading("Your providers".to_owned()));
    rows.extend(m.providers.iter().map(|p| {
        Row::item(ItemRow {
            key: format!("provider:{}", p.id),
            name: p.name.clone(),
            toggle: Some(true),
            status: vec![Part::new(kit::plural(p.models.len(), "model"), Ink::Fg3)],
            open: Some(Act::Push(Page::Provider(p.id.clone()))),
            flip: Some(super::providers::edit(
                &p.id,
                super::page::ProviderEdit::Enabled(false),
            )),
            ..ItemRow::default()
        })
    }));
    rows.push(Row::plus(
        "addProvider",
        "Add provider",
        "Any service that speaks one of three APIs: chat completions, responses, or Anthropic messages.",
        Act::Push(Page::NewProvider),
    ));
    rows
}

/// Memory：规则文件，⏎ 在编辑器里打开；不存在的打开时先建空文件（Puffin `memory`）。
pub(super) fn memory(files: &[data::MemoryFile], ctx: &Ctx<'_>) -> Vec<Row> {
    if files.is_empty() {
        return vec![Row::Note("No instruction files are known here.".to_owned())];
    }
    files
        .iter()
        .map(|f| {
            let status = if f.exists {
                kit::size(f.size)
            } else {
                "not created yet".to_owned()
            };
            Row::item(ItemRow {
                key: format!("mem:{}", f.path),
                name: kit::file_name(&f.path).to_owned(),
                scope: f.scope.clone(),
                status: vec![Part::new(status, Ink::Fg3)],
                desc: ctx.tilde(&f.path),
                open: Some(Act::Edit(f.path.clone())),
                ..ItemRow::default()
            })
        })
        .collect()
}

/// 家族的计划在 Connection 行里的名字（Puffin `connection_name`、`team_name`）。
pub(crate) fn plan_name(f: &FamilyStatus) -> String {
    match f.connection {
        PlanConnection::Start => "Start Plan".to_owned(),
        PlanConnection::Individual => "Individual Plan".to_owned(),
        PlanConnection::Team(i) => f
            .teams
            .get(i)
            .map_or_else(|| "Team Plan".to_owned(), team_name),
    }
}

fn team_name(t: &TeamStatus) -> String {
    if t.name.is_empty() {
        "Team Plan".to_owned()
    } else {
        format!("Team Plan · {}", t.name)
    }
}

/// 家族行的状态（Puffin `family_status`）。
fn family_status(f: &FamilyStatus) -> Vec<Part> {
    if !f.signed_in {
        return vec![Part::new("not signed in", Ink::Fg3)];
    }
    vec![
        Part::new("signed in", Ink::Ok),
        Part::new(format!(" · {}", plan_name(f)), Ink::Fg3),
    ]
}

/// 一个家族的页：Connection、账号与它的模型（Puffin `family`）。换计划经 runner 写，两边的 host 都回答这一版还没有
/// （tui-kit.md §4.5）；Sign out 是旧界面的 `/logout`。
pub(super) fn family(id: &str, ctx: &Ctx<'_>) -> View {
    let Some(f) = ctx
        .models()
        .and_then(|m| m.families.iter().find(|f| f.family.as_str() == id))
    else {
        return View::loading(id);
    };
    let s = f;
    let teams: Vec<Opt> = if s.teams.is_empty() {
        let why = if !s.signed_in {
            "Sign in to see your teams".to_owned()
        } else if !s.teams_error.is_empty() {
            format!("Could not read your teams: {}", s.teams_error)
        } else {
            "This account has no team plan".to_owned()
        };
        vec![Opt::new("team-coding-plan", "Team Plan").disabled(why)]
    } else {
        s.teams
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let why = if t.usable {
                    String::new()
                } else if !t.why.is_empty() {
                    t.why.clone()
                } else {
                    "Its key is not available yet".to_owned()
                };
                Opt::new(format!("team:{i}"), team_name(t)).disabled(why)
            })
            .collect()
    };
    let mut options = vec![
        Opt::new("start-plan", "Start Plan").desc("Free"),
        Opt::new("individual-coding-plan", "Individual Plan"),
    ];
    options.extend(teams);
    let current = match s.connection {
        PlanConnection::Start => "start-plan".to_owned(),
        PlanConnection::Individual => "individual-coding-plan".to_owned(),
        PlanConnection::Team(i) => format!("team:{i}"),
    };
    let family = s.family;
    let mut rows = vec![Row::Choice {
        key: "connection".to_owned(),
        label: "Connection".to_owned(),
        options,
        current,
        default: String::new(),
        desc: "How ZCode pays for your requests.".to_owned(),
        applies: Applies::Next,
        pick: Arc::new(move |v| {
            super::providers::next(Change::Connection {
                family,
                plan: v.to_owned(),
            })
        }),
    }];
    if s.signed_in {
        let who = if s.account.is_empty() {
            "signed in"
        } else {
            s.account.as_str()
        };
        rows.push(Row::info("Account", vec![Part::new(who, Ink::Fg2)]));
        rows.push(Row::action(
            "signOut",
            "Sign out",
            "The desktop app is signed out too: they share one account.",
            Act::Write(
                Write::new(Change::SignOut, Applies::Next)
                    .saying("Signed out. The desktop app is signed out too."),
            ),
        ));
    } else {
        rows.push(Row::action(
            "signIn",
            "Sign in",
            &format!(
                "Opens {} in your browser. Sign in there; this page changes by itself.",
                s.name
            ),
            Act::SignIn(s.family),
        ));
    }
    rows.push(Row::Heading("Models".to_owned()));
    if f.models.is_empty() {
        let note = if s.signed_in {
            "No models."
        } else {
            "Sign in to see your models."
        };
        rows.push(Row::Note(note.to_owned()));
    } else {
        rows.extend(f.models.iter().map(|m| {
            super::providers::model_item(
                &s.provider,
                m,
                "Turned-off models stay out of /model and the default model list.",
            )
        }));
    }
    View::new(
        vec![s.name.clone()],
        "Built-in provider. Your account decides which models you can use.",
        rows,
    )
}
