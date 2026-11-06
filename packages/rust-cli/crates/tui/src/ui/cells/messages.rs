// 改写自 apps/zcode-host/crates/tui/src/ui/cells/messages.rs（feat/rs 79b6691c55）：输入换成视图行的字段。
//! 消息类的块：用户消息、回复、推理、时间线标记、错误与启动头。
//!
//! 样子照 Puffin（docs/specs/rust-cli/tui-kit.md §4.2，工作线 TK 的 S1；docs/puffin-tui.md §4.2）：用户消息是整宽的 panel 板
//! （`▄` / `▀` 两条边，第 1 列 brand 粗体的 `›`，文字从第 3 列起，Puffin `shell/view/entries.rs` 的 `user_turn`）；回复是
//! kit 的回答，从第 3 列起、折行宽度 `min(宽 − 2, 99)`（同 `entries::answer`）；展开的推理是 `Thought · 6s` 的标题加竖线上的
//! 全文（同 `entries::thought`）；回合失败是第 1 列 err 的 `✗` 加 fg2 的句子（同 `shell/failures.rs` 的 `block`）。
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::NoticeLevel;
use rust_cli_tui_kit::app::element::{Element, Wrap, single_line, slab, stack, wrap_styled};
use rust_cli_tui_kit::render::{Span, Style, StyledLine, attr};

use super::look::{self, LEAD, lay, measured};
use super::{Ctx, words};
use crate::ui::i18n::Lang;
use crate::ui::i18n_kit;
use crate::ui::markdown;

/// 板：rich 主题里是整宽的 panel 底色块（上 `▄` 下 `▀`）；plain 没有板，原样（Puffin `slab`）。
fn on_slab(body: Element, ctx: &Ctx<'_>) -> Element {
    match ctx.ink.theme.panel {
        Some(panel) => slab(body, panel),
        None => body,
    }
}

/// 用户消息（Puffin `entries::user_turn`，kit `user_slab::turn`）：板上第 1 列 brand 粗体的 `›`，文字从第 3 列起按词折行，
/// 正文宽 `min(宽 − 2, 99)`；开头是认得的命令（`/compact` 之类）时命令名是 brand。高过 12 行时只显示开头 6 行与末尾 3 行，
/// 中间一行 fg3 的 `… N more lines`，除非打开（^O）。plain 没有板，文字是 fg2。以前不剪，命令名不上墨。
pub fn user(text: &str, ctx: &Ctx<'_>) -> Vec<StyledLine> {
    use rust_cli_tui_kit::app::widgets::user_slab::{self, Piece};
    let theme = &ctx.ink.theme;
    let cut = command_name(text).map_or(0, |name| name.len() + 1);
    let (name, rest) = text.split_at(cut);
    // 发出的回合里的链接（会话 `[#标题](#sess_…)`、技能 `[$名字](路径)`、文件）读成提及，与 Puffin 的用户行同一份读法
    // （kit `mention_wire::parts`）；之前整段当平常的字，链接原样露出来。旧界面没有 `@子代理` 提及。
    let pieces: Vec<Piece> = rust_cli_tui_kit::app::widgets::mention_wire::parts(
        rest,
        &|s| ctx.skills.iter().any(|k| k.name == s),
        &|_| false,
    )
    .into_iter()
    .map(|(t, k)| {
        if k.is_empty() {
            Piece::Text(t)
        } else {
            Piece::Mention(t, k)
        }
    })
    .collect();
    let words = user_slab::words(theme, name, &pieces);
    lay(
        &user_slab::turn_said(
            theme,
            Some(words),
            Vec::new(),
            ctx.expanded,
            Vec::new(),
            i18n_kit::more(ctx.lang),
        ),
        ctx.width,
    )
}

/// 开头的命令名（Puffin `consts::command_name`）：`/` 之后到第一个空白，是认得的命令（或它的别名）时。
fn command_name(text: &str) -> Option<&str> {
    let body = text.strip_prefix('/')?;
    let end = body.find(char::is_whitespace).unwrap_or(body.len());
    let name = &body[..end];
    (!name.is_empty() && crate::ui::commands::find(&name.to_lowercase()).is_some()).then_some(name)
}

/// 回复显示的文字：流式中最后一行没写完（没有换行）时先不显示这一行，免得半个词、半个 markdown 标记闪来闪去。只在没有
/// 播放头的时候用（流式的回复由 transcript 的播放头按词显示，`transcript_lines.rs`）。
pub(crate) fn visible(text: &str, streaming: bool) -> &str {
    if streaming {
        text.rfind('\n').map_or("", |end| &text[..end])
    } else {
        text
    }
}

/// 回复：kit 的回答，从第 3 列起（Puffin `entries::answer`）；以前首行是 `⏺ `、其余缩进 2 列。
pub fn agent(text: &str, streaming: bool, ctx: &Ctx<'_>) -> Vec<StyledLine> {
    let text = visible(text, streaming);
    if text.trim().is_empty() {
        return Vec::new();
    }
    // 流式中增量排版，只重排开放的末尾（修复线 F6，审查 R5-5；tui-kit.md §3.5）。
    match ctx.markdown.filter(|_| streaming) {
        Some(memo) => memo.answer(text, None, ctx.width, ctx.ink),
        None => markdown::answer(text, ctx.width, ctx.ink),
    }
}

/// 压缩（Puffin `entries::compact_row`，docs/puffin-tui.md §4.2「命令的结果」的 `/compact`）：写得像一个名为 `Compact` 的
/// 工具调用，在跑时 `○ Compact  summarising the conversation`，成功时 `✓ Compact  52.3k → 7.9k tokens`（两个数都知道时）。
/// 中文是 kit 同一个画法里的译文（`i18n_kit`）。以前开始与做完各是一行，摘要是旧界面的句子。
pub fn compact(compact: &crate::view::Compact, ctx: &Ctx<'_>) -> Vec<StyledLine> {
    use rust_cli_tui_kit::app::widgets::entries;
    let element = entries::compact_row_said(
        &ctx.ink.theme,
        compact.status,
        compact.before,
        compact.after,
        i18n_kit::entries(ctx.lang),
    );
    lay(&element, ctx.width)
}

/// 通知（Puffin `notices::entry`，docs/puffin-tui.md §4.2「通知」）：记号、fg2 粗体的种类词、标题与发生了什么，下面轨道上
/// 是回来的话（至多四行）。`p` 是记号与种类从背景淡入到哪里（0 刚到，1 落定）。英文的字同 Puffin，中文是译文
/// （`i18n_kit`）；标题与正文按语言。
/// 定时任务的触发（tools-more.md §18.3）是回来的命令（`• Command  Running scheduled task (…)  finished`，Puffin `fire`）；
/// 后台子代理的结果回到主会话是 `• Subagent  描述  finished`；系统提示（L18）是 `• Notice` / 琥珀的 `● Warning`，文字在轨道上；同伴的消息（cross-session.md §2.6）是
/// `• Peer  名字  says`，消息在轨道上；回执是 `Sent`。以前触发行是 fg3 的一行字，系统提示与同伴消息没有种类词。
pub fn notice_entry(body: &crate::view::Body, ctx: &Ctx<'_>, p: f64) -> Option<Vec<StyledLine>> {
    use rust_cli_tui_kit::app::widgets::notices::{self, Notice, Receipt, Work};
    let title;
    let n = match body {
        crate::view::Body::Fire(fire) => {
            let wakeup = fire.kind == rust_cli_kernel_api::cron::CronFireKind::Loop;
            let fold = fire
                .fold
                .as_ref()
                .map(|fold| (fold.streak, fold.since.as_str()));
            title = words::fire(ctx.lang, &fire.at, wakeup, fold);
            Notice::Finished {
                work: Work::Command,
                title: &title,
            }
        }
        // 后台子代理回来了（Puffin `NoticeSource::Subagent`）。
        crate::view::Body::Returned { title } => Notice::Finished {
            work: Work::Subagent,
            title,
        },
        crate::view::Body::Notice { level, text } => Notice::System {
            warning: *level == NoticeLevel::Warning,
            text,
        },
        crate::view::Body::Peer(peer) => match peer.as_ref() {
            crate::view::PeerRow::Message { name, body } => Notice::Peer { name, body },
            crate::view::PeerRow::Receipt { status } => Notice::Sent {
                name: "",
                receipt: match status.as_str() {
                    "held" => Receipt::Held,
                    "denied" => Receipt::Denied,
                    "expired" => Receipt::Expired,
                    "delivered" => Receipt::Delivered,
                    _ => Receipt::Refused,
                },
            },
        },
        _ => return None,
    };
    let said = notices::entry_said(&ctx.ink.theme, &n, p, i18n_kit::notices(ctx.lang));
    Some(lay(&said, ctx.width))
}

/// 记号在第 1 列、文字从第 3 列起按词折行（续行缩进 3 列），正文宽。
fn marked_text(mark: Span, text: &str, style: Style, ctx: &Ctx<'_>) -> Vec<StyledLine> {
    let body = wrap_styled(
        look::marked(mark),
        vec![Span::plain(LEAD)],
        vec![Span::new(
            text.replace('\r', "").replace('\t', "    "),
            style,
        )],
        Wrap::Word,
    );
    lay(&measured(body), ctx.width)
}

/// 折叠时说怎样看全文。
fn expand_hint(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "(^O to expand)",
        Lang::Zh => "（^O 展开）",
    }
}

/// 紧凑形式下错误文字的上限。
const ERROR_LIMIT: usize = 1000;

/// 错误行（docs/specs/rust-cli/tui-select.md §1）照 Puffin 的回合失败的块（`failures::block` 的头一行）：第 1 列 err 的 `✗`，
/// 错误文字从第 3 列起、fg2、按词折行（正文宽）；紧凑形式超过 1000 个字符时只显示前 1000 个字符加 `…`，下面第 3 列是 fg3 的
/// `(^O to expand)`（中文另配）；详细形式是全文。本地命令的错误（`Local::Error`）同样画（Puffin「带 ✗ 的提示行」）。
///
/// 缺陷修复（F25）：原来整条错误截到一行宽（`truncate`），Bedrock 这类长的 400 只显示到 `https…`，后半句与请求 ID
/// 看不到也复制不到。
pub fn error_lines(message: &str, ctx: &Ctx<'_>) -> Vec<StyledLine> {
    let message = words::turn_error(ctx.lang, message.trim());
    let text = message.as_str();
    let cut = !ctx.expanded && text.chars().count() > ERROR_LIMIT;
    let shown = if cut {
        let mut shown: String = text.chars().take(ERROR_LIMIT).collect();
        shown.push('…');
        shown
    } else {
        text.to_owned()
    };
    let theme = &ctx.ink.theme;
    let mut lines = marked_text(Span::new("✗", Style::fg(theme.err)), &shown, theme.fg2, ctx);
    if cut {
        lines.push(StyledLine::new(vec![
            Span::plain(LEAD),
            Span::new(expand_hint(ctx.lang), theme.fg3),
        ]));
    }
    lines
}

/// 被 esc 停下的回合（Puffin `entries::stopped_line`，docs/puffin-tui.md §4.2「边缘状态」）：第 3 列起 fg3 斜体的
/// `Stopped.`；中文是它的译文。以前是旧界面的 `Interrupted · What should ZCode do instead?`。
pub fn interrupted(ctx: &Ctx<'_>) -> Vec<StyledLine> {
    let element = rust_cli_tui_kit::app::widgets::entries::stopped_line_said(
        &ctx.ink.theme,
        false,
        i18n_kit::entries(ctx.lang),
    );
    lay(&element, ctx.width)
}

/// 启动头（docs/specs/rust-cli/tui.md §7.3）是 Colophon：一行空行，`ZCode`（Z 是 brand 色）与 fg3 的版本——npm 上有更新的版本
/// （`newer`）时行尾接 fg2 的 `   X is out · ` 与 brand 的 `zcode update`——、fg2 的目录。恢复的会话（`resumed`，与它的标题）
/// 接着是粗体的标题与 fg3 的 `started Thu 1 Jan, 00:00`；否则有话要说时才有第 4 行起，bypassPermissions 时是
/// `permissions: Full access`（err）。最后一行空行。
pub fn header(
    mode: PermissionMode,
    newer: Option<&str>,
    resumed: Option<(&str, crate::ui::app::Resumed)>,
    door: Door<'_>,
    ctx: &Ctx<'_>,
) -> Vec<StyledLine> {
    use rust_cli_tui_kit::app::widgets::{welcome, when};
    let theme = &ctx.ink.theme;
    if let Some((title, at)) = resumed {
        let title = if title.is_empty() { "Untitled" } else { title };
        let started = when::started(at.created_ms as f64, &|_| at.utc_offset);
        let block = welcome::resumed_said(
            theme,
            ctx.version,
            ctx.workspace_label,
            title,
            &started,
            newer,
            i18n_kit::is_out(ctx.lang),
        );
        return lay(&block, ctx.width);
    }
    // 门道 > 刚接好 > 开始（Puffin `welcome_rows`；tui-kit.md §4.2「登录」）：门道在欢迎块里时头两行是它的头。
    let (row4, row5) = match door.rows {
        Some(rows) => rows,
        None => (header_row4(mode, ctx), Vec::new()),
    };
    // 不量宽：同 Puffin，每行单行、按整个宽度截断（长目录保留结尾）。
    let block = welcome::block_said(
        theme,
        ctx.version,
        ctx.workspace_label,
        &row4,
        &row5,
        newer,
        i18n_kit::is_out(ctx.lang),
    );
    let mut lines = lay(&block, ctx.width);
    if let Some(below) = door.below {
        lines.extend(lay(below, ctx.width));
    }
    lines
}

/// 欢迎块里门道说的（`App::door_head` 或接好时的 `✓` 行）与它头下面的块。
#[derive(Default)]
pub struct Door<'a> {
    pub rows: Option<(Vec<Span>, Vec<Span>)>,
    pub below: Option<&'a rust_cli_tui_kit::app::element::Element>,
}

/// 欢迎块平时的第 4 行：只有权限模式要说时才有。
fn header_row4(mode: PermissionMode, ctx: &Ctx<'_>) -> Vec<Span> {
    if mode != PermissionMode::BypassPermissions {
        return Vec::new();
    }
    let theme = &ctx.ink.theme;
    let label = match ctx.lang {
        Lang::En => "permissions: ",
        Lang::Zh => "权限：",
    };
    vec![
        Span::new(label, theme.fg3),
        Span::new(words::full_access(ctx.lang), Style::fg(theme.err)),
    ]
}

/// 板上的 `!` 命令（Puffin `shell_view::command_rows`）：第 1 列琥珀粗体的 `!`，命令从第 3 列起，每行按宽截断（不折行）；
/// rich 主题在板上，plain 没有板、命令是 fg2。
pub(crate) fn shell_command(command: &str, ctx: &Ctx<'_>) -> Vec<StyledLine> {
    let theme = &ctx.ink.theme;
    let ink = if theme.rich {
        Style::DEFAULT
    } else {
        theme.fg2
    };
    let width = ctx.width.max(1);
    let rows: Vec<Element> = command
        .split('\n')
        .enumerate()
        .map(|(i, line)| {
            let mut spans = if i == 0 {
                vec![
                    Span::plain(" "),
                    Span::new("!", Style::fg(theme.code).with_attr(attr::BOLD)),
                    Span::plain(" "),
                ]
            } else {
                vec![Span::plain(LEAD)]
            };
            let text = line.replace('\t', "    ").replace('\r', "");
            spans.push(Span::new(
                look::clip(&text, width.saturating_sub(4).max(1)),
                ink,
            ));
            single_line(spans, false)
        })
        .collect();
    lay(&on_slab(stack(rows), ctx), width)
}
