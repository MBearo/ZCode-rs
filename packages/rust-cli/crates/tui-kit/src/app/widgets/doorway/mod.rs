//! 门道（docs/puffin-tui.md §4.2「接入模型（门道）」的「位置」「回答」「登录」「密钥」「按键」；docs/specs/rust-cli/tui-kit.md
//! §4.4 K5）：两个能登录的家族的事实、头两行、头下面的块（从第 3 列或欢迎块的文字列起；回答、登录、套餐、团队、密钥的
//! 行）、占着板的两步（粘贴密钥、别的 provider 的列表）的行与它们的键。入参是中性的 [`Head`]、[`Body`] 与 [`KeyStep`]；
//! 壳把自己的 `DoorStep` 与模板映射进来，状态与按键留在壳。
//! 移植自 Puffin 的 shell/doorway/view.rs（Doorway.scala 第 406–602 行；ShellApp.scala 第 3220–3234 行）与
//! doorway.rs 的 `FamilyInfo`。折与切按码点（Scala 的 `grouped` 按 UTF-16 单元）。
use crate::app::element::{Element, br, by_width, single_line, stack};
use crate::markdown::render::INDENT;
use crate::render::width::string_width;
use crate::render::{Color, Span, Style, attr};
use crate::theme::theme::Theme;

use super::fit::{clip, pad};
use super::usage::Family;

pub mod board;

pub use board::{KeyStep, keys, paste_rows, providers_rows};

#[cfg(test)]
mod tests;

/// 列表最多显示这么多模板（§4.2「密钥：其他 provider」）。
pub const PROVIDER_ROWS: usize = 7;
/// 比这窄的描述不画（§4.2「回答」）。
pub const DESC_MIN: usize = 9;

/// 能登录的家族与它的密钥用的模板（Doorway.scala 第 25 行的 `Family`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FamilyInfo {
    pub family: Family,
    /// `/login` 认的名字。
    pub id: &'static str,
    pub name: &'static str,
    pub host: &'static str,
    pub region: &'static str,
    pub site: &'static str,
    pub key_template: &'static str,
    pub pay_template: &'static str,
}

pub const ZAI: FamilyInfo = FamilyInfo {
    family: Family::Zai,
    id: "zai",
    name: "Z.ai",
    host: "z.ai",
    region: "Global",
    site: "https://z.ai",
    key_template: "zai-api",
    pay_template: "zai-standard-api",
};

pub const BIGMODEL: FamilyInfo = FamilyInfo {
    family: Family::Bigmodel,
    id: "bigmodel",
    name: "BigModel",
    host: "bigmodel.cn",
    region: "China",
    site: "https://bigmodel.cn",
    key_template: "bigmodel-api",
    pay_template: "bigmodel-standard-api",
};

/// 家族 `f` 的事实。
pub fn family_info(f: Family) -> &'static FamilyInfo {
    match f {
        Family::Zai => &ZAI,
        Family::Bigmodel => &BIGMODEL,
    }
}

/// 一个回答：名字与描述；`list` 是名字的描述，整个整个地截、带 ` …`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub label: String,
    pub desc: String,
    pub list: Vec<String>,
}

impl Answer {
    pub fn new(label: impl Into<String>, desc: impl Into<String>) -> Answer {
        Answer {
            label: label.into(),
            desc: desc.into(),
            list: Vec::new(),
        }
    }
}

/// 选中的回答的墨：brand（plain 模式加粗）。同 Asks.scala 第 86 行。
fn chosen_style(theme: &Theme) -> Style {
    if theme.rich {
        Style::fg(theme.brand)
    } else {
        Style::new(theme.brand, Color::Default, attr::BOLD)
    }
}

fn span(text: impl Into<String>, style: Style) -> Span {
    Span::new(text.into(), style)
}

/// 回答按请求的画法（§4.2「回答」）：号 fg3、两格、名字 fg2 补到最长、三格、描述 fg3；选中的号与名字 brand（plain
/// 模式加粗）、描述 fg2。`dim` 全部画成线的颜色，没有选中的。
pub fn answer_rows(
    theme: &Theme,
    answers: &[Answer],
    sel: Option<usize>,
    dim: bool,
    lead: &str,
) -> Vec<Element> {
    let lw = answers
        .iter()
        .map(|a| string_width(&a.label))
        .max()
        .unwrap_or(0);
    answers
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let theme = *theme;
            let a = a.clone();
            let lead = lead.to_owned();
            let on = sel == Some(i) && !dim;
            by_width(move |w| {
                let chosen = chosen_style(&theme);
                let ink = |other: Style| {
                    if on {
                        chosen
                    } else if dim {
                        theme.line
                    } else {
                        other
                    }
                };
                let num = format!("{}", i + 1);
                let room = w as isize
                    - string_width(&lead) as isize
                    - string_width(&num) as isize
                    - 2
                    - lw as isize
                    - 3;
                let room_u = usize::try_from(room).unwrap_or(0);
                let text = if a.list.is_empty() {
                    if room > 0 {
                        clip(&a.desc, room_u)
                    } else {
                        String::new()
                    }
                } else {
                    names(&a.list, room_u)
                };
                let mut spans = vec![
                    span(lead.clone(), Style::DEFAULT),
                    span(num, ink(theme.fg3)),
                    span("  ", Style::DEFAULT),
                    span(pad(&a.label, lw), ink(theme.fg2)),
                ];
                if room >= DESC_MIN as isize && !text.is_empty() {
                    let desc = if on {
                        theme.fg2
                    } else if dim {
                        theme.line
                    } else {
                        theme.fg3
                    };
                    spans.push(span("   ", Style::DEFAULT));
                    spans.push(span(text, desc));
                }
                single_line(spans, false)
            })
        })
        .collect()
}

/// 用 `, ` 连起来的名字放进 `room` 格；放不下时放得下的那些加 ` …`。
pub fn names(all: &[String], room: usize) -> String {
    let whole = all.join(", ");
    if string_width(&whole) <= room {
        return whole;
    }
    (0..all.len())
        .rev()
        .map(|n| format!("{} …", all[..n].join(", ")))
        .find(|s| string_width(s) <= room)
        .unwrap_or_default()
}

/// 失败的登录说的与它下面的提示（§4.2「登录失败」）。`host` 是家族的主机。
pub fn failure(host: &str, reason: &str) -> (String, String) {
    const NETWORK: [&str; 6] = [
        "ETIMEDOUT",
        "ENOTFOUND",
        "ECONNREFUSED",
        "ECONNRESET",
        "EAI_AGAIN",
        "fetch failed",
    ];
    if reason.to_lowercase().contains("expire") {
        (
            "The sign-in link expired before you finished.".to_owned(),
            "⏎ opens a new one.".to_owned(),
        )
    } else if NETWORK.iter().any(|n| reason.contains(n)) {
        (
            format!("Could not reach {host}: {reason}"),
            "Behind a proxy? Set HTTPS_PROXY and start ZCode again. ⏎ tries again now.".to_owned(),
        )
    } else {
        (
            format!("Could not sign in: {reason}"),
            "⏎ tries again.".to_owned(),
        )
    }
}

/// `text` 在空格处折成至多 `room` 格的行（Scala `split(" ")` 丢掉末尾的空串）。
pub fn fold(text: &str, room: usize) -> Vec<String> {
    let mut words: Vec<&str> = text.split(' ').collect();
    while words.len() > 1 && words.last().is_some_and(|w| w.is_empty()) {
        words.pop();
    }
    let mut rows: Vec<String> = Vec::new();
    for word in words {
        match rows.last_mut() {
            Some(last) if string_width(&format!("{last} {word}")) <= room => {
                last.push(' ');
                last.push_str(word);
            }
            _ => rows.push(word.to_owned()),
        }
    }
    rows
}

/// `text` 切成 `room` 个码点一段。
pub fn chunks(text: &str, room: usize) -> Vec<String> {
    let cps: Vec<char> = text.chars().collect();
    cps.chunks(room.max(1))
        .map(|c| c.iter().collect())
        .collect()
}

// ---- 回答 ----

/// 三种接入的方式：登录两个家族，或用 API key。
pub fn choose_answers(families: [&FamilyInfo; 2]) -> Vec<Answer> {
    let mut out: Vec<Answer> = families
        .iter()
        .map(|f| {
            Answer::new(
                format!("Sign in to {}", f.name),
                format!("your {} account · {}", f.host, f.region),
            )
        })
        .collect();
    out.push(Answer::new(
        "Use an API key",
        "Z.ai, BigModel, DeepSeek, OpenAI and more",
    ));
    out
}

/// 密钥是给哪个服务的：两个家族的模板（名字与主机，顺序同 `families`），然后是别的 provider 的名字。
pub fn key_answers(
    families: [&FamilyInfo; 2],
    keyed: [(&str, &str); 2],
    others: Vec<String>,
) -> Vec<Answer> {
    let mut out: Vec<Answer> = families
        .iter()
        .zip(keyed)
        .map(|(f, (name, host))| {
            let desc = if host.is_empty() {
                f.region.to_owned()
            } else {
                format!("{host} · {}", f.region)
            };
            Answer::new(name, desc)
        })
        .collect();
    out.push(Answer {
        label: "Another provider".to_owned(),
        desc: String::new(),
        list: others,
    });
    out
}

/// 登录了、没有套餐的账号的回答。
pub fn no_plan_answers(f: &FamilyInfo, account: &str) -> Vec<Answer> {
    vec![
        Answer::new(
            "Get a Coding Plan",
            format!("opens {} in your browser", f.host),
        ),
        Answer::new("Use an API key", "pay for what you use instead"),
        Answer::new(
            "Use another account",
            if account.is_empty() {
                "signs you out first".to_owned()
            } else {
                format!("signs {account} out first")
            },
        ),
    ]
}

/// 团队按名字（组织的，没有时 ""），之后是密钥与另一个账号。
pub fn team_answers(f: &FamilyInfo, account: &str, teams: &[&str]) -> Vec<Answer> {
    let mut out: Vec<Answer> = teams
        .iter()
        .map(|t| {
            if t.is_empty() {
                Answer::new("Team Plan", "")
            } else {
                Answer::new(*t, "team plan")
            }
        })
        .collect();
    out.extend(no_plan_answers(f, account).into_iter().skip(1));
    out
}

/// 「Signed in to Z.ai as sam.」（Puffin 与旧界面都用这一句；`account` 空时不写 `as`）。
pub fn signed_in_says(f: Family, account: &str) -> String {
    let who = if account.is_empty() {
        String::new()
    } else {
        format!(" as {account}")
    };
    format!("Signed in to {}{who}.", family_info(f).name)
}

/// 密钥接好了：「Connected to Z.ai with your key. glm-4.6 answered in 1.3 s.」。`host` 是模板的主机（没有时是它的名字），
/// `ms` 是试那个模型的用时（Puffin 与旧界面都用这一句）。
pub fn key_says(host: &str, model_id: &str, ms: u64) -> String {
    format!(
        "Connected to {host} with your key. {model_id} answered in {:.1} s.",
        ms as f64 / 1000.0
    )
}

/// 在切换的团队的说法。
pub fn team_says(name: &str) -> String {
    if name.is_empty() {
        "the team plan".to_owned()
    } else {
        format!("{name}'s team plan")
    }
}

// ---- 头两行（§4.2「位置」） ----

/// 门道在哪一步，头两行要的。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Head<'a> {
    /// 三种方式；`expired` 是登录失效了的家族。
    Choose {
        expired: Option<Family>,
    },
    SignIn {
        family: Family,
    },
    NoPlan {
        family: Family,
        account: &'a str,
        waiting: bool,
    },
    Teams {
        family: Family,
        account: &'a str,
        many: bool,
    },
    /// 密钥是给哪个服务的（也是别的 provider 的列表那一步）。
    KeyFor,
    Paste {
        name: &'a str,
    },
}

/// 门道的头两行：代替欢迎块的第 4、5 行；第二行可以是空的。
pub fn head(theme: &Theme, h: &Head<'_>) -> (Vec<Span>, Vec<Span>) {
    let fg = |t: String| vec![span(t, Style::DEFAULT)];
    let fg3 = |t: &str| vec![span(t, theme.fg3)];
    let who = |account: &str| {
        if account.is_empty() {
            String::new()
        } else {
            format!(" as {account}")
        }
    };
    match *h {
        Head::Choose {
            expired: Some(family),
        } => (
            fg(format!(
                "Your {} sign-in has expired.",
                family_info(family).name
            )),
            fg3("Sign in again to go on. Your sessions are all still here."),
        ),
        Head::Choose { expired: None } => (fg("Connect a model to start.".to_owned()), Vec::new()),
        Head::SignIn { family } => (
            fg(format!("Sign in to {}", family_info(family).name)),
            Vec::new(),
        ),
        Head::NoPlan {
            family,
            account,
            waiting,
        } => (
            fg(format!(
                "Signed in to {}{}. This account has no plan yet.",
                family_info(family).name,
                who(account)
            )),
            if waiting {
                Vec::new()
            } else {
                fg3("A plan comes with the models. An API key works too.")
            },
        ),
        Head::Teams {
            family,
            account,
            many,
        } => {
            let has = if many { "team plans" } else { "a team plan" };
            (
                fg(format!(
                    "Signed in to {}{}. This account has {has}.",
                    family_info(family).name,
                    who(account)
                )),
                fg3("Pick the one to use. /settings can change it later."),
            )
        }
        Head::KeyFor => (fg("Which service is the key for?".to_owned()), Vec::new()),
        Head::Paste { name } => (fg(format!("Paste your {name} key below.")), Vec::new()),
    }
}

// ---- 头下面的块 ----

/// 门道在哪一步，头下面的行要的。`remote`：在远程 shell 里（网址整段折在 `│` 后面）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body<'a> {
    Choose {
        sel: usize,
    },
    SignIn {
        family: Family,
        url: &'a str,
        failed: Option<&'a str>,
        remote: bool,
    },
    NoPlan {
        family: Family,
        account: &'a str,
        sel: usize,
        waiting: bool,
        remote: bool,
    },
    /// `teams` 是团队的名字（没有时 ""），`switching` 是在切换的那个的名字。
    Teams {
        family: Family,
        account: &'a str,
        teams: Vec<&'a str>,
        sel: usize,
        switching: Option<&'a str>,
        failed: Option<&'a str>,
    },
    /// 密钥是给哪个服务的；`sel` 为 `None` 时全部画成线的颜色（别的 provider 的列表开着）。
    KeyFor {
        sel: Option<usize>,
        answers: Vec<Answer>,
    },
    Paste {
        name: &'a str,
        key_url: &'a str,
        host: &'a str,
        checking: bool,
        refused: Option<&'a str>,
    },
}

/// 头下面的行，每行以 `lead` 开头（§4.2「位置」）。`families` 是两个家族在回答里的次序。
pub fn body(theme: &Theme, b: &Body<'_>, families: [&FamilyInfo; 2], lead: &str) -> Vec<Element> {
    let theme = *theme;
    let lw = string_width(lead);
    let pre = span(lead.to_owned(), Style::DEFAULT);
    let row = move |spans: Vec<Span>| {
        let mut all = vec![pre.clone()];
        all.extend(spans);
        single_line(all, false)
    };
    let rail = {
        let row = row.clone();
        move |spans: Vec<Span>| {
            let mut all = vec![span("│", theme.line), span(" ", Style::DEFAULT)];
            all.extend(spans);
            row(all)
        }
    };
    let waiting = |text: String| {
        row(vec![
            span("○", theme.fg3),
            span(" ", Style::DEFAULT),
            span(text, theme.fg3),
        ])
    };
    let failed = |text: String| {
        row(vec![
            span("✗", Style::fg(theme.err)),
            span(" ", Style::DEFAULT),
            span(text, theme.fg2),
        ])
    };
    // 失败下面的 fg3 的话：从第二格起，按宽度折。
    let under = |text: String| {
        let row = row.clone();
        by_width(move |w| {
            let room = w.saturating_sub(lw + 4).max(10);
            stack(
                fold(&text, room)
                    .into_iter()
                    .map(|l| row(vec![span(format!("  {l}"), theme.fg3)])),
            )
        })
    };
    let link = Style::fg(theme.brand).with_attr(attr::UNDERLINE);
    match b {
        Body::Choose { sel } => {
            answer_rows(&theme, &choose_answers(families), Some(*sel), false, lead)
        }
        Body::SignIn {
            family,
            failed: Some(reason),
            ..
        } => {
            let (says, hint) = failure(family_info(*family).host, reason);
            vec![failed(says), under(hint)]
        }
        Body::SignIn { url, remote, .. } => {
            let mut rows = Vec::new();
            if !url.is_empty() {
                if *remote {
                    // 远程 shell 里网址按行宽完整地折在 `│` 后面，好整段选中。
                    rows.push(rail(vec![span(
                        "Open this link on any device and sign in there.",
                        theme.fg2,
                    )]));
                    let url = (*url).to_owned();
                    rows.push(by_width(move |w| {
                        let room = w.saturating_sub(lw + 2).max(10);
                        stack(
                            chunks(&url, room)
                                .into_iter()
                                .map(|c| rail(vec![span(c, link)])),
                        )
                    }));
                } else {
                    rows.push(rail(vec![span(
                        "Opened the sign-in page in your browser. Sign in there.",
                        theme.fg2,
                    )]));
                    rows.push(rail(vec![span((*url).to_owned(), link)]));
                }
            }
            rows.push(waiting("Waiting for you to sign in".to_owned()));
            rows
        }
        Body::NoPlan {
            family,
            waiting: true,
            remote,
            ..
        } => {
            let fam = family_info(*family);
            let says = if *remote {
                format!("Open {} on any device and get a plan there.", fam.site)
            } else {
                format!("Opened {} in your browser. Get a plan there.", fam.host)
            };
            vec![
                rail(vec![span(says, theme.fg2)]),
                waiting("ZCode looks again when you come back to this window".to_owned()),
            ]
        }
        Body::NoPlan {
            family,
            account,
            sel,
            ..
        } => answer_rows(
            &theme,
            &no_plan_answers(family_info(*family), account),
            Some(*sel),
            false,
            lead,
        ),
        Body::Teams {
            family,
            account,
            teams,
            sel,
            switching,
            failed: why,
        } => {
            let mut rows = answer_rows(
                &theme,
                &team_answers(family_info(*family), account, teams),
                Some(*sel),
                false,
                lead,
            );
            if let Some(t) = switching {
                rows.push(waiting(format!("Switching to {}", team_says(t))));
            }
            if let Some(w) = why {
                rows.push(failed(format!("Could not switch: {w}")));
            }
            rows
        }
        Body::KeyFor { sel, answers } => answer_rows(&theme, answers, *sel, sel.is_none(), lead),
        Body::Paste {
            name,
            key_url,
            host,
            checking,
            refused,
        } => {
            let mut rows = vec![if key_url.is_empty() {
                rail(vec![span(
                    format!("Copy it from your {name} account's API keys page."),
                    theme.fg2,
                )])
            } else {
                let shown = key_url
                    .strip_prefix("https://")
                    .or_else(|| key_url.strip_prefix("http://"))
                    .unwrap_or(key_url)
                    .to_owned();
                rail(vec![
                    span("No key yet? Get one at ", theme.fg2),
                    span(shown, link),
                ])
            }];
            if *checking {
                rows.push(waiting(format!("Checking the key with {host}")));
            }
            if let Some(why) = refused {
                rows.push(failed(format!("{host} refused the key: {why}")));
                rows.push(row(vec![span(
                    "  Check it, then paste it again.",
                    theme.fg3,
                )]));
            }
            rows
        }
    }
}

/// 门道的块在欢迎块下面（从它的文字列起）：没有行时没有块。
pub fn in_welcome(mut rows: Vec<Element>) -> Option<Element> {
    if rows.is_empty() {
        return None;
    }
    rows.push(br());
    Some(stack(rows))
}

/// 门道的块在 transcript 的末尾（从第 3 列起）：空行、头两行（第二行空时是空行），然后是 `body`（以 INDENT 开头的行）。
pub fn at_end(head: (Vec<Span>, Vec<Span>), body: Vec<Element>) -> Element {
    let (a, b) = head;
    let indent = span(INDENT, Style::DEFAULT);
    let line = |spans: Vec<Span>| {
        let mut all = vec![indent.clone()];
        all.extend(spans);
        single_line(all, false)
    };
    let mut rows = vec![br(), line(a), if b.is_empty() { br() } else { line(b) }];
    rows.extend(body);
    stack(rows)
}
