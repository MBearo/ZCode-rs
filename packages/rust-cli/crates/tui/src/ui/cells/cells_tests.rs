// 改写自 apps/zcode-host/crates/tui/src/ui/cells/cells_tests.rs（feat/rs 79b6691c55）：行换成 `view::Row`。
use std::time::Duration;

use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::permission::PermissionMode;
use serde_json::{Value, json};

use super::*;
use crate::ui::i18n::Lang;
use crate::ui::lines::plain;
use crate::ui::probe::{ColorLevel, Colors};
use crate::view::{Compact, Failure, ToolRow, ToolStatus};
use rust_cli_tui_kit::app::widgets::entries::Compaction;

pub(crate) fn ctx<'a>(colors: &'a Colors, expanded: bool) -> Ctx<'a> {
    Ctx {
        // 测试的块要一个活得一样久的墨色；测试进程里泄漏这一点无妨。
        ink: Box::leak(Box::new(colors.ink())),
        lang: Lang::En,
        width: 60,
        expanded,
        elapsed: Duration::ZERO,
        now_ms: 0,
        workspace: "/w",
        workspace_label: "~/w",
        version: "1.0.0",
        mcp_machines: &[],
        skills: &[],
        machines: &[],
        markdown: None,
    }
}

pub(crate) fn row(id: u64, body: Body) -> Row {
    Row {
        id,
        version: 0,
        created_ms: 0,
        body,
    }
}

/// 逐块排版（块之间的空行由 transcript 加，这里没有）。
pub(crate) fn text(rows: &[Row], expanded: bool) -> Vec<String> {
    let colors = Colors::new(ColorLevel::Ansi16, None);
    let ctx = ctx(&colors, expanded);
    build(rows)
        .iter()
        .flat_map(|cell| lines(cell, &ctx))
        .map(|line| plain(&line))
        .collect()
}

pub(crate) fn tool(id: u64, name: &str, status: ToolStatus, input: Value) -> Row {
    row(
        id,
        Body::Tool(Box::new(ToolRow::new(
            ToolUseId::new(format!("tu{id}")),
            name.into(),
            input,
            status,
        ))),
    )
}

pub(crate) fn done(id: u64, name: &str, input: Value, output: &str) -> Row {
    let mut row = tool(id, name, ToolStatus::Success, input);
    if let Body::Tool(tool) = &mut row.body {
        tool.output = output.into();
    }
    row
}

fn agent(text: &str, streaming: bool) -> Body {
    Body::Agent {
        text: text.into(),
        streaming,
    }
}

fn failure(message: &str) -> Body {
    Body::Failure(Box::new(Failure {
        message: message.into(),
        reason: "",
        http_status: 0,
        retryable: false,
        tries: 0,
        provider: String::new(),
        model: String::new(),
        resend: String::new(),
        at: 0,
    }))
}

/// 结束的回合（Puffin 的条目）：你的板、收拢成一句的一段工作、回答、像一个调用的压缩、失败块。
#[test]
fn a_finished_turn_reads_like_puffin() {
    let rows = [
        row(1, Body::Turn(TurnState::Success)),
        row(
            2,
            Body::User {
                text: "fix it".into(),
            },
        ),
        row(
            3,
            Body::Reasoning {
                text: "**Planning**\n\nsteps".into(),
                streaming: false,
                duration_ms: Some(1000),
            },
        ),
        done(
            4,
            "mcp__search__find_docs",
            json!({"query": "x"}),
            "Found docs",
        ),
        row(5, agent("All **set**", false)),
        row(
            6,
            Body::Compact(Compact {
                status: Compaction::Success,
                before: Some(52_300),
                after: Some(7_900),
            }),
        ),
        row(7, failure("busy")),
    ];
    let shown = text(&rows, false);
    assert_eq!(
        shown[..3],
        [" › fix it", GATHERED, "   All set"],
        "{shown:#?}"
    );
    assert_eq!(shown[3], " ✓ Compact  52.3k → 7.9k tokens", "{shown:#?}");
    assert!(shown[4].starts_with(" ✗ "), "{shown:#?}");
}

/// 上面那段工作收拢成的一句（Puffin `gather::line`：思考的用时、MCP 工具的短名）。
const GATHERED: &str = " ✓ Thought for 1s, called find_docs";

/// L18：插话被 UserPromptSubmit 阻塞时的提示，多行原样显示，从第 3 列起（Puffin 的系统提示 `notices::entry`）。
#[test]
fn system_notices_show_every_line() {
    let rows = [row(
        1,
        Body::Notice {
            level: rust_cli_kernel_api::session_api::NoticeLevel::Warning,
            text: "UserPromptSubmit operation blocked by hook:\nno\n\nOriginal prompt: also b"
                .into(),
        },
    )];
    let shown = text(&rows, false);
    // 头一行是种类，正文在 `│` 上，空行不画（kit `notices::entry`）。
    assert_eq!(
        shown,
        [
            " ● Warning",
            "   │ UserPromptSubmit operation blocked by hook:",
            "   │ no",
            "   │ Original prompt: also b",
        ]
    );
}

/// 停下的回合：「Stopped.」在它的最后一行之后（Puffin `settle_stop`），不在你的消息上面；下一个回合在它下面。
#[test]
fn a_stopped_turn_ends_with_stopped_after_its_rows() {
    let rows = [
        row(1, Body::Turn(TurnState::Interrupted)),
        row(2, Body::User { text: "go".into() }),
        row(3, agent("half", false)),
        row(4, Body::Turn(TurnState::Success)),
        row(
            5,
            Body::User {
                text: "again".into(),
            },
        ),
    ];
    assert_eq!(
        text(&rows, false),
        [" › go", "   half", "   Stopped.", " › again"]
    );
}

/// 你的消息（Puffin `user_slab::turn`）：高过 12 行时只显示开头 6 行、`… N more lines`、末尾 3 行，^O 时全部；开头认得的命令名
/// 是 brand。
#[test]
fn a_tall_message_shows_its_first_six_and_last_three_lines() {
    let text_of = |n: usize| {
        (1..=n)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let rows = [row(1, Body::User { text: text_of(15) })];
    let shown = text(&rows, false);
    assert_eq!(shown.len(), 10, "{shown:#?}");
    assert_eq!(shown[0], " › line 1");
    assert_eq!(shown[5], "   line 6");
    assert_eq!(shown[6].trim(), "… 6 more lines");
    assert_eq!(shown[9], "   line 15");
    assert_eq!(text(&rows, true).len(), 15);
    let colors = Colors::new(ColorLevel::Ansi16, None);
    let ctx = ctx(&colors, false);
    let line = &messages::user("/compact now", &ctx)[0];
    let brand = line
        .spans
        .iter()
        .find(|span| span.text.contains("/compact"))
        .expect("命令名");
    assert_eq!(brand.style.fg_color(), ctx.ink.theme.brand, "{line:?}");
    let plain_line = &messages::user("/nope now", &ctx)[0];
    assert!(
        plain_line
            .spans
            .iter()
            .filter(|span| span.text.contains("/nope"))
            .all(|span| span.style.fg_color() != ctx.ink.theme.brand),
        "{plain_line:?}"
    );
}

/// 用户行里发出的链接读成提及（Puffin 的用户行，kit `mention_wire::parts`）：会话 `@标题` 是 work 的墨色，技能链接与认得的
/// 光秃 `$名字` 是 skill 的墨色；不认得的 `$名字` 是平常的字。
#[test]
fn user_rows_read_mention_links() {
    use crate::ui::skill_mention::{From, MentionSkill};
    let colors = Colors::new(ColorLevel::TrueColor, None);
    let skills = [MentionSkill {
        name: "lint".into(),
        description: String::new(),
        path: String::new(),
        from: From::User,
    }];
    let ctx = Ctx {
        skills: &skills,
        ..ctx(&colors, false)
    };
    let line = &messages::user(
        "see [#Fix login](#sess_a1) and #sess_b2 [$deploy](/s/deploy/SKILL.md) $lint $nope",
        &ctx,
    )[0];
    let shown: String = line.spans.iter().map(|s| &*s.text).collect();
    assert_eq!(
        shown.trim_end(),
        " › see @Fix login and #sess_b2 $deploy $lint $nope",
        "{line:?}"
    );
    let ink = |t: &str| {
        line.spans
            .iter()
            .find(|s| s.text.contains(t))
            .map(|s| s.style.fg_color())
            .expect(t)
    };
    let theme = &ctx.ink.theme;
    assert_eq!(ink("@Fix"), theme.work);
    assert_eq!(ink("$deploy"), theme.skill);
    assert_eq!(ink("$lint"), theme.skill);
    assert_ne!(ink("$nope"), theme.skill);
    // 标签就是会话号的链接（桌面端的写法）也读成 `@sess_…`；光秃的 `#sess_…`（这里没有标题时发的）是平常的字，同 Puffin。
    let bare = &messages::user("[#sess_c3](#sess_c3)", &ctx)[0];
    let shown: String = bare.spans.iter().map(|s| &*s.text).collect();
    assert_eq!(shown.trim_end(), " › @sess_c3");
}

/// 启动头是 Colophon（tui.md §7.3，kit `welcome::block_said`）：第 1 行空，文字从第 3 列起；平时没有第 4 行；
/// bypassPermissions 时第 4 行是权限模式；有新版本时版本行尾说它；恢复的会话在目录下面多一行标题与开始的时刻。
#[test]
fn the_startup_header_is_the_colophon() {
    let colors = Colors::new(ColorLevel::Ansi16, None);
    let ctx = ctx(&colors, false);
    let lines = |mode, newer, resumed| -> Vec<String> {
        header(mode, newer, resumed, Default::default(), &ctx)
            .iter()
            .map(|l| plain(l).trim_end().to_owned())
            .collect()
    };
    let shown = lines(PermissionMode::Default, None, None);
    assert_eq!(shown, ["", "  ZCode  1.0.0", "  ~/w", ""], "{shown:#?}");
    let full = lines(PermissionMode::BypassPermissions, None, None);
    assert_eq!(full[4], "  permissions: Full access", "{full:#?}");
    assert_eq!(full[5], "", "{full:#?}");
    let newer = lines(PermissionMode::Default, Some("2.0.0"), None);
    assert!(
        newer[1].ends_with("ZCode  1.0.0   2.0.0 is out · zcode update"),
        "{newer:#?}"
    );
    let at = crate::ui::app::Resumed {
        created_ms: 0,
        utc_offset: 0,
    };
    assert_eq!(
        lines(PermissionMode::Default, None, Some(("Fix the footer", at))),
        [
            "",
            "  ZCode  1.0.0",
            "  ~/w",
            "  Fix the footer  started Thu 1 Jan, 00:00",
            ""
        ]
    );
    assert_eq!(
        lines(PermissionMode::Default, None, Some(("", at))),
        [
            "",
            "  ZCode  1.0.0",
            "  ~/w",
            "  Untitled  started Thu 1 Jan, 00:00",
            ""
        ]
    );
}

#[test]
fn live_cells_are_the_ones_still_changing() {
    let rows = [
        row(1, Body::Turn(TurnState::Running)),
        tool(2, "Bash", ToolStatus::Running, json!({"command": "x"})),
    ];
    assert!(build(&rows).last().unwrap().live());
    let rows = [
        row(1, Body::Turn(TurnState::Success)),
        tool(2, "Bash", ToolStatus::Success, json!({"command": "x"})),
    ];
    assert!(!build(&rows).last().unwrap().live());
}

/// 运行中的调用的 `○` 不动（Puffin：只有工作光标动，docs/puffin-tui.md §4.2「笔」的「调用」）；以前 `⏺` 每 600 ms 闪一次。
#[test]
fn running_calls_keep_a_still_ring() {
    let colors = Colors::new(ColorLevel::Ansi16, None);
    let rows = [
        row(1, Body::Turn(TurnState::Running)),
        tool(2, "Bash", ToolStatus::Running, json!({"command": "x"})),
    ];
    let built = build(&rows);
    let cell = built.last().unwrap();
    // 手停住时动的是点阵，圈不呼吸（kit `tool_row::call`）：圈的字与墨不随时间变。
    let ring = |ms| {
        let ctx = Ctx {
            elapsed: Duration::from_millis(ms),
            ..ctx(&colors, false)
        };
        let line = lines(cell, &ctx).remove(0);
        let span = line.spans.into_iter().find(|s| s.text == "○").expect("圈");
        assert!(plain(&lines(cell, &ctx)[0]).ends_with("Bash  x"));
        span.style
    };
    assert!([700, 1300].iter().all(|ms| ring(*ms) == ring(0)));
}

#[test]
fn a_streaming_reply_hides_its_unfinished_last_line() {
    assert_eq!(
        text(&[row(1, agent("line one\nline tw", true))], false),
        ["   line one"]
    );
    assert!(text(&[row(1, agent("no newline yet", true))], false).is_empty());
    // 回复结束后全部显示（markdown 的软换行把两行连成一段）。
    assert_eq!(
        text(&[row(1, agent("line one\nline two", false))], false),
        ["   line one line two"]
    );
}

/// 定时任务的触发行（tools-more.md §18.3）：英文原文（产品名换成 ZCode）与中文。
#[test]
fn fire_rows_use_the_fixed_text_in_both_languages() {
    assert_eq!(
        words::fire(Lang::En, "Oct 18 3:04pm", false, None),
        "Running scheduled task (Oct 18 3:04pm)"
    );
    assert_eq!(
        words::fire(Lang::En, "Oct 18 3:04pm", true, None),
        "ZCode resuming /loop wakeup (Oct 18 3:04pm)"
    );
    assert_eq!(
        words::fire(Lang::En, "Oct 18 3:04pm", true, Some((1, "Oct 18 2:44pm"))),
        "ZCode resuming /loop wakeup (Oct 18 3:04pm) \u{b7} 1 no-op tick since Oct 18 2:44pm"
    );
    assert_eq!(
        words::fire(Lang::En, "Oct 18 3:04pm", true, Some((3, "Oct 18 2:44pm"))),
        "ZCode resuming /loop wakeup (Oct 18 3:04pm) \u{b7} 3 no-op ticks since Oct 18 2:44pm"
    );
    assert_eq!(
        words::fire(Lang::Zh, "Oct 18 3:04pm", true, Some((2, "Oct 18 2:44pm"))),
        "ZCode 恢复 /loop 唤醒（Oct 18 3:04pm） \u{b7} 自 Oct 18 2:44pm 起连续 2 次无动作"
    );
    let fire = crate::view::Fire {
        at: "Oct 18 3:04pm".into(),
        kind: rust_cli_kernel_api::cron::CronFireKind::Task,
        fold: None,
    };
    let shown = text(&[row(1, Body::Fire(Box::new(fire)))], false);
    assert!(
        shown[0].contains("Running scheduled task (Oct 18 3:04pm)"),
        "{shown:#?}"
    );
}

/// 本地命令的错误行（Puffin「带 ✗ 的提示行」）：到了阻塞线（compact.md §2.2）时是“上下文满了”的提示；带压缩失败原因的接上
/// `/clear`；其余错误原样。回合失败是失败块，按原因写（`failure_block`）。
#[test]
fn the_blocking_line_error_reads_as_a_full_context() {
    let colors = Colors::new(ColorLevel::Ansi16, None);
    let ctx = ctx(&colors, false);
    assert_eq!(
        error_lines("Prompt is too long", &ctx)
            .iter()
            .map(plain)
            .collect::<String>(),
        " ✗ Context limit reached · /compact or /clear to continue"
    );
    assert_eq!(
        words::turn_error(
            Lang::En,
            "Prompt is too long \u{b7} automatic compaction failed: boom"
        ),
        "Prompt is too long \u{b7} automatic compaction failed: boom \u{b7} /clear to start fresh"
    );
    assert_eq!(
        words::turn_error(Lang::Zh, "Prompt is too long"),
        "上下文已满 \u{b7} 用 /compact 或 /clear 继续"
    );
    assert_eq!(words::turn_error(Lang::En, "busy"), "busy");
}

/// 中文（tui-kit.md §4.2「中文」）：kit 的条目同一个铺法，字是 `i18n_kit` 的译文——剪去的标记、通知的种类、压缩行、
/// `Stopped.`、失败的句子与下一步、收拢的句子、欢迎块的更新提示。
#[test]
fn kit_entries_speak_chinese_in_the_same_layout() {
    let colors = Colors::new(ColorLevel::Ansi16, None);
    let mut zh = ctx(&colors, false);
    zh.lang = Lang::Zh;
    let shown = |rows: &[Row]| -> Vec<String> {
        build(rows)
            .iter()
            .flat_map(|cell| lines(cell, &zh))
            .map(|line| plain(&line).trim_end().to_owned())
            .collect()
    };
    let tall = (1..=15)
        .map(|i| format!("行 {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let user = shown(&[row(1, Body::User { text: tall })]);
    assert_eq!(user.len(), 10, "{user:#?}");
    assert_eq!(user[6].trim(), "… 还有 6 行");
    let notice = shown(&[row(
        1,
        Body::Notice {
            level: rust_cli_kernel_api::session_api::NoticeLevel::Info,
            text: "一\n二\n三\n四\n五\n六".into(),
        },
    )]);
    assert_eq!(notice[0], " • 提示", "{notice:#?}");
    assert_eq!(notice[4], "   │ … 还有 3 行", "{notice:#?}");
    let compact = |status| {
        shown(&[row(
            1,
            Body::Compact(Compact {
                status,
                before: Some(52_300),
                after: Some(7_900),
            }),
        )])
    };
    assert_eq!(compact(Compaction::Running), [" ○ Compact  正在总结对话"]);
    assert_eq!(
        compact(Compaction::Success),
        [" ✓ Compact  52.3k → 7.9k tokens"]
    );
    let stopped = shown(&[
        row(1, Body::Turn(TurnState::Interrupted)),
        row(2, Body::User { text: "go".into() }),
    ]);
    assert_eq!(
        stopped.last().map(String::as_str),
        Some("   已停止。"),
        "{stopped:#?}"
    );
    let limited = Body::Failure(Box::new(Failure {
        message: "slow down".into(),
        reason: "rate_limited",
        http_status: 429,
        retryable: true,
        tries: 3,
        provider: String::new(),
        model: "glm".into(),
        resend: "again".into(),
        at: 0,
    }));
    assert_eq!(
        shown(&[row(1, limited)]),
        [
            " ✗ 服务商 正在限制请求。",
            "   slow down  ·  HTTP 429  ·  试了 3 次",
            "   ⏎ 再试一次。",
        ]
    );
    let read = |id, path: &str| done(id, "Read", json!({ "file_path": path }), "x");
    assert_eq!(
        shown(&[
            row(1, Body::Turn(TurnState::Success)),
            read(2, "/w/a.rs"),
            read(3, "/w/b.rs"),
        ]),
        [" ✓ 浏览了 2 个文件"]
    );
    let head: Vec<String> = header(
        PermissionMode::Default,
        Some("2.0.0"),
        None,
        Default::default(),
        &zh,
    )
    .iter()
    .map(|l| plain(l).trim_end().to_owned())
    .collect();
    assert!(
        head[1].ends_with("ZCode  1.0.0   2.0.0 已发布 · zcode update"),
        "{head:#?}"
    );
}

/// 中文的 ^O（tui-kit.md §4.2「中文」）：单独画的思考的标题与失败块的字段名也是译文，字段按显示宽度补到 10 格。
#[test]
fn expanded_entries_speak_chinese_too() {
    let colors = Colors::new(ColorLevel::Ansi16, None);
    let mut zh = ctx(&colors, true);
    zh.lang = Lang::Zh;
    let shown = |rows: &[Row]| -> Vec<String> {
        build(rows)
            .iter()
            .flat_map(|cell| lines(cell, &zh))
            .map(|l| plain(&l).trim_end().to_owned())
            .collect()
    };
    let thought = shown(&[
        row(1, Body::Turn(TurnState::Success)),
        row(
            2,
            Body::Reasoning {
                text: "steps".into(),
                streaming: false,
                duration_ms: Some(6000),
            },
        ),
    ]);
    assert!(
        thought.iter().any(|l| l == "   思考了 · 6s"),
        "{thought:#?}"
    );
    let failed = Body::Failure(Box::new(Failure {
        message: "slow down".into(),
        reason: "rate_limited",
        http_status: 429,
        retryable: true,
        tries: 0,
        provider: "zai".into(),
        model: "glm".into(),
        resend: String::new(),
        at: 0,
    }));
    let block = shown(&[row(1, failed)]);
    assert!(
        block
            .iter()
            .any(|l| l == "   │ 状态      HTTP 429 · rate_limited"),
        "{block:#?}"
    );
    assert!(
        block.iter().any(|l| l == "   │ 服务商    zai"),
        "{block:#?}"
    );
    assert!(
        block.iter().any(|l| l == "   │ 模型      glm"),
        "{block:#?}"
    );
    // 后台子代理回来了的通知（kit `notices::entry_said`）。
    let back = shown(&[row(
        1,
        Body::Returned {
            title: "Audit dates".into(),
        },
    )]);
    assert_eq!(back, [" • 子代理  Audit dates  完成了"], "{back:#?}");
}
