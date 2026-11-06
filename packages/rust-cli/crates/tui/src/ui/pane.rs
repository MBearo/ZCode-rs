// 改写自 apps/zcode-host/crates/tui/src/ui/pane.rs（feat/rs 79b6691c55）：待处理交互来自视图；去掉登录、
// 后台任务、任务清单与子代理列表。
//! 底部栈的组装，照 Puffin（docs/specs/rust-cli/tui-kit.md §4.0；docs/puffin-tui.md §4.3「底部栈」，`shell/view/frame.rs`
//! 的 `input_stack` 与 `stacks`、`shell/composer.rs` 的 `composer`）：输入框上方的一句（problems）、composer_gap、坞（在变的
//! 机器、标题行：任务的计数与会话名）、**一块**板（请求的行、栈、至多一层、草稿行与它的槽），然后是状态栏。
//!
//! 两摞：没有层的一摞量正文的高度；带层的一摞（层在、坞不在）画出来，比前者多出的行盖在正文最新的几行上（`draw.rs` 调 kit 的
//! `frame::splice`）。请求的行、门道、栈在两摞里都有，所以它们把正文推上去；层不推（tui-kit.md §4.0 第 4 条）。以前每个视图
//! 替换整个底部区域，并自己画一块板与板下面的提示行。
//!
//! 画法（tui-kit.md §3.1、§3.2）：组装 kit 的 [`Element`] 再排成 [`StyledLine`]；输入框的光标格自己带 `attr::CARET`，请求的
//! 光标仍是（行，列），由 `draw.rs` 加。
use std::time::Instant;

use rust_cli_tui_kit::app::element::{Element, stack};
use rust_cli_tui_kit::app::frame as skeleton;
use rust_cli_tui_kit::app::layout::Layout;
use rust_cli_tui_kit::app::widgets::key_hints::on_row;
use rust_cli_tui_kit::render::{Span, StyledLine};

use super::app::{App, Overlay};
use super::bottom;
use super::footer::{self, Slot};
use super::input_box;
use super::interaction::View;
use super::interaction_view::board::Laid;
use super::list::Purpose;
use super::mouse::Target;

/// 底部区域的一行可点击区域：（行，起始列，宽度，目标）；宽度为 None 时占整行。
pub(super) type RowTarget = (usize, usize, Option<usize>, Target);

/// 板上开着的东西在骨架里算什么。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// 请求的行（权限、提问、计划、同伴消息、完全访问的确认）：板的一部分，两摞里都有。
    Ask,
    /// 门道粘贴密钥那一步：代替草稿行，两摞里都有（Puffin `door_slab`）。
    Door,
    /// 一层：只在带层的一摞里，盖住正文。
    Layer,
}

/// 板上开着的一块：它的行（[`Laid`]，还没有放上板）、行上可点击的目标、槽（None：照草稿的槽）与它拿不拿键。
struct Open {
    kind: Kind,
    laid: Laid,
    targets: Vec<(usize, Target)>,
    slot: Option<Vec<Span>>,
    /// 这一块拿键：草稿行不画光标（旧界面的请求与页面自己收字）。
    owns_keys: bool,
    /// 光标（`laid.lines` 里的行，列）。
    cursor: Option<(usize, usize)>,
}

impl Open {
    fn layer(lines: Vec<StyledLine>, slot: Option<Vec<Span>>, owns_keys: bool) -> Open {
        Open {
            kind: Kind::Layer,
            laid: Laid {
                lines,
                ..Laid::default()
            },
            targets: Vec::new(),
            slot,
            owns_keys,
            cursor: None,
        }
    }
}

fn view(app: &App, width: usize) -> View<'_> {
    View {
        ink: &app.ink,
        lang: app.lang,
        width,
    }
}

/// 请求的行或门道：`/login`、完全访问的确认、扣住的同伴消息（没有待回答的交互时）、待回答的交互。`rows` 是终端的行数（带
/// 预览的题按它限制预览框，工作线 F72）。
/// `below`：正文下面还有行（请求的预览超出屏幕），槽是 `PgDn read on`（Puffin `ask_slot(below)`）。
fn asked(app: &App, width: usize, rows: usize, now: Instant, below: bool) -> Option<Open> {
    let open = |kind, laid: Laid, targets: Vec<(usize, Target)>, cursor| {
        let slot = Some(laid.slot.clone());
        Some(Open {
            kind,
            laid,
            targets,
            slot,
            owns_keys: true,
            cursor,
        })
    };
    // 门道粘贴密钥那一步占着板（Puffin `door_slab`）：名字一行与一字一个 `•` 的输入行，槽是它的键。
    if app.overlay.is_none()
        && let Some(rows) = app.door_slab(width)
    {
        let laid = Laid {
            lines: rows
                .iter()
                .flat_map(|row| Layout::lay(row, width))
                .collect(),
            slot: super::footer::slot(app).spans,
            ..Laid::default()
        };
        return open(Kind::Door, laid, Vec::new(), None);
    }
    if let Some(Overlay::YoloConfirm(confirm)) = &app.overlay {
        let laid = super::yolo_confirm::lines(confirm, &view(app, width), rows);
        return open(Kind::Ask, laid, Vec::new(), None);
    }
    // 扣住的同伴消息（cross-session.md §2.6）：没有待回答的交互时显示批准框。
    // 照 Puffin 的请求画（tui-asks，`peer_view.rs` 的 `peer_laid`）。
    if let Some((laid, options, cursor)) = app.peer_laid(width, now) {
        let targets = options
            .into_iter()
            .map(|(row, index)| (row, Target::Choice(index)))
            .collect();
        return open(Kind::Ask, laid, targets, cursor);
    }
    // 请求的行（tui-asks，`interaction_view.rs` 的 `ask_laid`）：板上的行；草稿行照常，占位与槽是请求的。
    let (laid, options, cursor) = app.ask_laid(width, now, below)?;
    let targets = options
        .into_iter()
        .map(|(row, index)| (row, Target::Choice(index)))
        .collect();
    open(Kind::Ask, laid, targets, cursor)
}

/// 板上的一层：`/diff`、`/export`、`/permissions`、任务卡、`/machines`、列表、`?` 按键表、斜杠命令或 `@` 文件的面板。
/// `height` 是层可用的高度。
/// `now` 是这一帧的动画时钟（同 `draw` 的 `now`；测试与孪生按它走，不读墙上的 `Instant::now()`）。
fn layer(app: &App, width: usize, height: usize, now: Instant, now_ms: u64) -> Option<Open> {
    let open = match &app.overlay {
        Some(Overlay::Diff(diff)) => {
            let (lines, slot) = super::diff_view_lines::lines(app, diff, width, height, now_ms);
            Open::layer(lines, Some(slot), true)
        }
        // 任务卡（task_card.rs、task_card_rows.rs，tui-tasks.md §3）：键由 keys.rs 交给它；草稿行照常。
        Some(Overlay::Tasks(_)) => {
            let card = app.tasks_layer(width)?;
            Open {
                kind: Kind::Layer,
                laid: Laid {
                    lines: card.lines,
                    ..Laid::default()
                },
                targets: card
                    .targets
                    .into_iter()
                    .map(|(row, index)| (row, Target::Task(index)))
                    .collect(),
                slot: Some(card.slot),
                owns_keys: false,
                cursor: None,
            }
        }
        // `/machines` 的列表（machines_list.rs）：键由 keys.rs 交给它；草稿行照 Puffin 有光标格与占位字。
        Some(Overlay::Machines(_)) => Open::layer(
            app.machines_lines(width, now_ms),
            Some(app.machines_slot()),
            false,
        ),
        Some(Overlay::List(list)) => {
            // 列表照 Puffin 的层：高度不看屏幕（只看自己的窗口），比底部栈能占的高时由 `splice` 从上面截（Puffin 的层不收
            // 高度）。以前与别的层一样只给 2/3 屏，矮的终端里窗口还缩到 3 项。
            let (lines, items) = list.lines(width, app.lang, &app.ink);
            // 槽：模型的列表是 `←→ effort`（Puffin `picker_open` 的槽），其余的列表空着（Puffin 的这几层没有键的提示）。
            let slot = if list.purpose == Purpose::Model {
                super::interaction_view::board::slot(
                    &app.ink.theme,
                    &[(
                        "←→",
                        match app.lang {
                            super::i18n::Lang::En => "effort",
                            super::i18n::Lang::Zh => "推理档位",
                        },
                    )],
                )
            } else {
                Vec::new()
            };
            Open {
                kind: Kind::Layer,
                laid: Laid {
                    lines,
                    ..Laid::default()
                },
                targets: items
                    .into_iter()
                    .map(|(row, index)| (row, Target::Item(index)))
                    .collect(),
                slot: Some(slot),
                owns_keys: false,
                cursor: None,
            }
        }
        // 输入层的卡（`/stats`、`/goal`，cards.rs）；有请求在等时不画。
        Some(Overlay::Card(_)) => {
            let tick = super::status_motion::tick_at(app, now);
            let (lines, slot) = app.card_layer(width, tick, now_ms)?;
            Open::layer(lines, Some(slot), false)
        }
        // `/resume` 的会话列表（sessions_list.rs）：槽空着（Puffin `resume_open`）。
        // ⌃t 读的页开着时板上没有层（页在 page_frame.rs）；改名与删除这一步的槽是它们的键（sessions_steps.rs）。
        Some(Overlay::Sessions(list)) if list.reading() => return None,
        // 驾驶舱的列表是页（cockpit_view.rs）；板上只有这一步的问与 `?`（tui-cockpit.md §3.2）。
        Some(Overlay::Sessions(list))
            if list.cockpit.is_some() && !App::cockpit_step_on_board(&list.step) =>
        {
            return None;
        }
        Some(Overlay::Sessions(list)) => Open::layer(
            super::pick::layer(app.sessions_rows(width, now, now_ms), &app.ink, width),
            Some(app.sessions_step_slot(list)),
            matches!(
                list.step,
                super::sessions_steps::Step::Delete { .. }
                    | super::sessions_steps::Step::Folder { .. }
                    | super::sessions_steps::Step::Elsewhere { .. }
                    | super::sessions_steps::Step::Keys
            ),
        ),
        // `/rewind` 的点列表（rewind.rs）：槽空着（Puffin `rewind_list_open`）；原地回退那一问拿键，槽是 `esc back`。
        Some(Overlay::Points(list)) => Open::layer(
            super::pick::layer(app.points_rows(now_ms), &app.ink, width),
            Some(if list.asking() {
                rust_cli_tui_kit::app::widgets::key_hints::spans(
                    &app.ink.theme,
                    &rust_cli_tui_kit::app::widgets::key_hints::pick(&[(
                        "esc".to_owned(),
                        "back".to_owned(),
                    )]),
                )
            } else {
                Vec::new()
            }),
            list.asking(),
        ),
        // `/fork` 的点列表（fork.rs）：同 `/rewind` 的列表，槽空着。
        Some(Overlay::Forks(_)) => Open::layer(
            super::pick::layer(app.forks_rows(now_ms), &app.ink, width),
            Some(Vec::new()),
            false,
        ),
        // `?` 按键表（tui.md §7.4）：槽空着（Puffin `sheet_open`）。
        Some(Overlay::Shortcuts) => Open::layer(
            Layout::lay(&stack(footer::sheet(app, width)), width),
            Some(Vec::new()),
            false,
        ),
        // ^T 时没有任务：卡的位置一行 quiet 的说明，到下一个键（tui-tasks.md §3.1）。
        _ if app.no_tasks => Open::layer(
            Layout::lay(
                &super::pick::quiet_row(super::i18n_panels::no_tasks(app.lang), &app.ink),
                width,
            ),
            Some(Vec::new()),
            false,
        ),
        // 命令架、`@` 面板与粘贴的预览（`input_box::peek_open`，tui-bottom）。
        _ if app.composer.popup.is_some() || super::input_box::peek_open(app) => {
            let mut lines = Vec::new();
            let mut targets = Vec::new();
            for (row, item) in bottom::popup(app, width) {
                let laid = Layout::lay(&row, width);
                if let Some(item) = item {
                    targets.push((lines.len(), Target::Popup(item)));
                }
                lines.extend(laid);
            }
            Open {
                kind: Kind::Layer,
                laid: Laid {
                    lines,
                    ..Laid::default()
                },
                targets,
                slot: None,
                owns_keys: false,
                cursor: None,
            }
        }
        _ => return None,
    };
    Some(open)
}

/// 板上开着一层或一块请求、门道：会话名不写（Puffin `caption_hidden`）。
fn caption_hidden(app: &App) -> bool {
    app.overlay.as_ref().is_some_and(|o| !o.full_screen())
        || app.composer.popup.is_some()
        || super::input_box::peek_open(app)
        || app.pending().is_some()
        || app.view.peer_held().is_some()
}

/// transcript 有正文（不只欢迎块，Puffin `has_body`）：有你的回合、本地块或尾巴里发出的消息。
pub fn has_body(app: &App) -> bool {
    app.view.has_messages() || !app.local.is_empty() || !super::transcript::outgoing(app).is_empty()
}

/// 输入框上方的一句（Puffin `problems`）：本地命令的结果；没有时什么都没有，有正文时上面空一行。
fn problems(app: &App) -> Element {
    let theme = &app.ink.theme;
    // 安静的话可以有几行（复制的途径说明接在复制的结果下面，Puffin `problems` 的提示与安静的一行各一行）。
    let failed = app
        .said
        .failed
        .iter()
        .map(|words| skeleton::notice_row(theme, words));
    let quiet = app
        .said
        .quiet
        .iter()
        .flat_map(|words| words.lines())
        .map(|line| skeleton::quiet_row(theme, line));
    let rows: Vec<Element> = failed.chain(quiet).collect();
    skeleton::problems(has_body(app), rows)
}

/// 槽的段，从它说的变了的那一刻淡入（Puffin `slot_spans` 的 `fade_in`）。第一次画的槽直接出现。
/// `record`：记下槽换了的时刻。`below` 的第二次排（`PgDn read on`）不记，按第一次的槽的时刻淡（Puffin 的 `slot_at` 按
/// `slot_of(state, false, …)` 记，`below` 换出的槽不重新淡入）。以前两次都记，每帧先记第一次的槽、再记第二次的，
/// `PgDn read on` 每帧从头淡入、一直看不见（孪生 ask-long-preview 第 11 步）。
fn faded(app: &mut App, spans: Vec<Span>, now: Instant, record: bool) -> Vec<Span> {
    let says: String = spans.iter().map(|s| s.text.as_ref()).collect();
    match &app.slot_seen {
        _ if !record => {}
        Some((was, _)) if *was == says => {}
        Some(_) => app.slot_seen = Some((says, now)),
        None => {
            app.slot_seen = Some((
                says,
                now.checked_sub(std::time::Duration::from_secs(1))
                    .unwrap_or(now),
            ))
        }
    }
    let at = app.slot_seen.as_ref().map_or(now, |(_, at)| *at);
    let age = now.saturating_duration_since(at).as_millis()
        / u128::from(rust_cli_tui_kit::app::widgets::playhead::TICK_MS);
    rust_cli_tui_kit::app::widgets::fade::fade_in(
        &app.ink.theme,
        spans,
        i64::try_from(age).unwrap_or(i64::MAX),
        rust_cli_tui_kit::app::widgets::fade::TICKS,
        input_box::ground(app),
    )
}

/// 一个只占一行的元素排成的那一行。
fn one_line(element: &Element, width: usize) -> StyledLine {
    Layout::lay(element, width)
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// 草稿的槽与它是否画在输入行上：放不下时槽让开（Puffin「按键提示（槽）」：草稿离槽不足两格时不画），说明句改在状态栏
/// 右边说。
fn fitted_slot(app: &App, width: usize, max_rows: usize) -> (Slot, bool) {
    // 槽只有一种（Puffin 的槽放不下时整个不画）：以前按 `Level` 一级级缩短再试。
    let slot = footer::slot(app);
    if slot.spans.is_empty() {
        return (slot, false);
    }
    let input = input_box::input_rows(app, width, max_rows, slot.spans.clone(), true, None);
    let last = input.rows.last().map(|r| one_line(r, width).plain());
    let text: String = slot.spans.iter().map(|s| s.text.as_ref()).collect();
    let fits = last.is_some_and(|l| l.trim_end().ends_with(text.trim_end()));
    (slot, fits)
}

/// 底部栈：没有层的一摞的行数、带层的一摞（画出来的，开着层时没有坞）、状态栏与子代理列表的行；可点击的行（相对带层的一摞
/// 接上状态栏的行）、输入框文字区（起始行，行数，第一可见行，相对带层的一摞）与请求的光标（相对带层的一摞）。
pub(super) struct Bottom {
    pub(super) stack0: usize,
    pub(super) shelf: Vec<StyledLine>,
    pub(super) status: Vec<StyledLine>,
    pub(super) cursor: Option<(u16, u16)>,
    pub(super) targets: Vec<RowTarget>,
    pub(super) composer: Option<(usize, usize, usize)>,
    /// 有请求开着（分隔行不画，Puffin `asking`）。
    pub(super) asking: bool,
}

/// `page`：开着一页（正文换成了它，`page_frame`）：底部栈没有坞（Puffin 的页的 `input_stack` 不带坞），槽是页的键。
/// `below`：请求开着、正文下面还有行（`draw.rs` 第二次排），见 [`asked`]。
pub(super) fn bottom(
    app: &mut App,
    width: usize,
    rows: usize,
    now: Instant,
    now_ms: u64,
    page: Option<&super::page_frame::PageBody>,
    below: bool,
) -> Bottom {
    // 输入框里淡入的东西先记下这一帧（`!`、粘贴的帽与预览、状态栏的 Shell，input_motion.rs）。
    super::input_motion::note(app, now, now_ms);
    // `@` 面板这一帧的淡入与要读的预览（`at_view.rs`），模型选择器的档位名的淡入（`list.rs`，tui-bottom）。
    let tick = app.status_motion.tick();
    app.note_at(width, tick);
    app.note_skills(tick);
    if let Some(super::app::Overlay::List(list)) = app.overlay.as_mut() {
        list.note_level(tick);
    }
    let theme = app.ink.theme;
    // 层可用的高度：同以前的底部区域（2/3 屏，至少 8 行）。
    let room = (rows * 2 / 3).max(8).min(rows.saturating_sub(1));
    let ask = asked(app, width, rows, now, below);
    // 会话列表的细节行变了的那一刻（淡入，sessions_list.rs）。
    app.note_sessions(now);
    let shelf_layer = layer(app, width, room, now, now_ms);
    let hidden = caption_hidden(app);
    // 输入框上方的一句与 composer_gap（两摞都有）。
    let said = problems(app);
    let quiet = said.is_blank();
    let gap = skeleton::composer_gap(theme.rich, has_body(app), quiet);
    let head = Layout::lay(&stack([said, gap]), width);
    // 坞：在变的机器与标题行（tui-tasks.md §2：左边数子代理、命令与待办，^T 打开任务卡；右边会话名）；只在没有层的一摞里。
    // 开着页时没有机器与计数，会话名留着。
    let mut dock: Vec<StyledLine> = Vec::new();
    if page.is_none() {
        dock.extend(
            app.machines_row(now_ms)
                .map(|row| Layout::lay(&row, width))
                .unwrap_or_default(),
        );
    }
    let body = has_body(app);
    let name = input_box::title(app).filter(|_| quiet && !hidden && (body || !theme.rich));
    // 有正文时两边都没字也留这一行：任务开始、结束时 transcript 不动（§2.3）。
    let reserved = quiet && body;
    // 标题行左边可点的地方（tui-tasks.md §2.2「点击」）：标题行是坞的最后一行。
    let (title, title_clicks) = match app.title_row(width, now, name, page.is_none()) {
        Some(row) => (Some(row.line), row.clicks),
        None => (None, Vec::new()),
    };
    let title_at = dock.len();
    dock.extend(title.or_else(|| reserved.then(StyledLine::default)));
    // 请求：板上面的预览、板上的行、代替草稿行的输入行。
    let (above, ask_rows, ask_input) = match &ask {
        Some(open) if open.kind == Kind::Ask => {
            let l = &open.laid;
            let end = l.input_at + l.input_rows;
            (
                l.lines[..l.board_at].to_vec(),
                l.lines[l.board_at..l.input_at].to_vec(),
                l.lines[l.input_at..end.min(l.lines.len())].to_vec(),
            )
        }
        Some(open) => (Vec::new(), Vec::new(), open.laid.lines.clone()),
        None => (Vec::new(), Vec::new(), Vec::new()),
    };
    // 栈：等下一轮的消息、建会话期间留下的输入、等回合结束的 `/clear` `/resume`。
    let queue: Vec<StyledLine> = bottom::queue(app, width)
        .iter()
        .flat_map(|row| Layout::lay(row, width))
        .collect();
    // 挑选回退的点：输入行上面是 Also 与 Files 两行（Puffin `rewind_rows`，板里层之下、草稿之上）。
    let rewound: Vec<StyledLine> = super::rewind_pick::rows(app)
        .iter()
        .flat_map(|row| Layout::lay(row, width))
        .collect();
    // 修复：以前栈、层与挑选的行紧贴草稿行，排队的行和任务卡的行读成草稿的一部分（2026-11-01 用户报告）。它们上面的东西
    // 都从这里过，所以在板上统一空一行（tui-kit.md §4.0「板」）；只有层时这一行不算进没有层的一摞，不推正文。
    let stack_gap = usize::from(!queue.is_empty() || !rewound.is_empty());
    // 草稿行最多 max(3, 高度 / 2 − 5) 行，也不超过没有层的一摞剩下的高度（板的两条边与一行状态栏）。
    let fixed = head.len()
        + dock.len()
        + above.len()
        + 2
        + ask_rows.len()
        + queue.len()
        + rewound.len()
        + stack_gap
        + 1;
    let max_rows = input_box::max_rows(rows)
        .min(rows.saturating_sub(1 + fixed))
        .max(1);
    let bash = app.composer.mode == super::composer::InputMode::Bash;
    let owns_keys = ask.as_ref().is_some_and(|o| o.owns_keys)
        || shelf_layer.as_ref().is_some_and(|o| o.owns_keys)
        || page.is_some_and(|p| p.owns_keys);
    // 槽：请求、门道与拿键的层是它们自己的键；其余照草稿的槽（放不下时说明句改在状态栏右边说）。
    let own_slot = ask
        .as_ref()
        .and_then(|o| o.slot.clone())
        .or_else(|| shelf_layer.as_ref().and_then(|o| o.slot.clone()))
        .or_else(|| page.and_then(|p| p.slot.clone()));
    let mut note = Vec::<Span>::new();
    let mut slotted = false;
    // 草稿行：门道或请求的输入行代替它（槽在它的最后一行上）；搜索模式（修复线 F114，tui-input.md §2.3.5）是搜索的输入行，
    // 槽是计数、搜索的按键在状态栏右边；否则是草稿。
    let (draft, composer_rows): (Vec<StyledLine>, Option<(usize, usize)>) = if !ask_input.is_empty()
    {
        let mut lines = ask_input;
        let slot = own_slot.clone().map(|slot| faded(app, slot, now, !below));
        if let (Some(slot), Some(last)) = (slot, lines.pop()) {
            lines.extend(Layout::lay(&on_row(Element::Raw(vec![last]), slot), width));
        }
        (lines, None)
    } else if let Some(find) = &app.port.find {
        note = super::key_hint::spans(super::find::hint(app.lang), &app.ink);
        let row = super::find::input_row(app, find, width);
        (Layout::lay(&row, width), Some((0, 1)))
    } else {
        let slot = match own_slot {
            Some(slot) => slot,
            None => {
                let (slot, fits) = fitted_slot(app, width, max_rows);
                if !fits && slot.sentence {
                    note = slot.spans.clone();
                }
                slotted = fits;
                if fits { slot.spans } else { Vec::new() }
            }
        };
        let slot = faded(app, slot, now, !below);
        // 请求开着时草稿行就是回答的输入框（Puffin `composer`）：有光标；请求给了一句（`Laid::answer`）时占位写它，不然照常。
        let answer = ask.as_ref().and_then(|o| o.laid.answer.as_deref());
        let caret = ask.as_ref().is_some_and(|o| o.kind == Kind::Ask) || !owns_keys;
        let input = input_box::input_rows(app, width, max_rows, slot, caret, answer);
        let first = input.first;
        let text_rows = input.text_rows;
        let lines: Vec<StyledLine> = input
            .rows
            .iter()
            .flat_map(|r| Layout::lay(r, width))
            .collect();
        (lines, Some((text_rows, first)))
    };
    let layer_rows: Vec<StyledLine> = shelf_layer
        .as_ref()
        .map(|o| o.laid.lines.clone())
        .unwrap_or_default();
    let layered = shelf_layer.is_some();
    // 没有层的一摞：head、坞、预览、板（两条边、请求的行、栈、草稿行）。
    let stack0 = head.len()
        + dock.len()
        + above.len()
        + 2
        + ask_rows.len()
        + queue.len()
        + rewound.len()
        + stack_gap
        + draft.len();
    // 状态栏（一或两行）在板下面，没有可点的地方（后台命令的 pill 并进了任务行，tui-tasks.md §4）；任务卡是板上的一层。
    let status = footer::status(app, width, rows, note, bash);
    // 层放不下时空行让给层的字：多这一行，`?` 按键表在 80×24 上就从上面截掉 `Anywhere` 的标题（`splice` 从上面截）。
    let drawn = head.len()
        + above.len()
        + 2
        + ask_rows.len()
        + queue.len()
        + layer_rows.len()
        + rewound.len()
        + draft.len()
        + status.len();
    let layer_fits = !layer_rows.is_empty() && drawn < skeleton::max_bottom(rows);
    // 带层的一摞（画出来的）：开着层时没有坞（Puffin `input_stack` 的 `rows` 为 None）。
    let mut shelf = head;
    // 开着层时没有坞，也就没有标题行可点。
    let title_row = (!layered).then_some(shelf.len() + title_at);
    if !layered {
        shelf.extend(dock);
    }
    shelf.extend(above);
    let board_top = shelf.len() + 1;
    let ask_top = board_top;
    let layer_top = ask_top + ask_rows.len() + queue.len();
    let draft_gap = usize::from(stack_gap > 0 || layer_fits);
    let draft_top = layer_top + layer_rows.len() + rewound.len() + draft_gap;
    let inner: Vec<StyledLine> = ask_rows
        .into_iter()
        .chain(queue)
        .chain(layer_rows)
        .chain(rewound)
        .chain(std::iter::repeat_n(StyledLine::default(), draft_gap))
        .chain(draft)
        .collect();
    shelf.extend(Layout::lay(
        &input_box::board(app, vec![Element::Raw(inner)]),
        width,
    ));
    let mut targets: Vec<RowTarget> = Vec::new();
    let mut cursor = None;
    if let Some(open) = &ask {
        // 请求的行在 `board_at` 起；门道的行是草稿行的位置。
        let base = match open.kind {
            Kind::Door => draft_top,
            _ => ask_top,
        };
        let at = |row: usize| -> Option<usize> {
            let l = &open.laid;
            match open.kind {
                Kind::Door => Some(draft_top + row),
                _ if row < l.board_at => None,
                _ if row < l.input_at => Some(base + row - l.board_at),
                _ => Some(draft_top + row - l.input_at),
            }
        };
        targets.extend(
            open.targets
                .iter()
                .filter_map(|(row, target)| Some((at(*row)?, 0, None, target.clone()))),
        );
        cursor = open
            .cursor
            .and_then(|(row, col)| Some((u16::try_from(at(row)?).ok()?, u16::try_from(col).ok()?)));
    }
    if let Some(open) = &shelf_layer {
        targets.extend(
            open.targets
                .iter()
                .map(|(row, target)| (layer_top + row, 0, None, target.clone())),
        );
    }
    if let Some(row) = title_row {
        targets.extend(
            title_clicks
                .into_iter()
                .map(|(col, w, target)| (row, col, Some(w), target)),
        );
    }
    // 槽里可以点的提示（`? for shortcuts`、`← for projects`）：在草稿的最后一行。
    let composer = composer_rows.map(|(text_rows, first)| (draft_top, text_rows, first));
    if slotted && let Some((top, text_rows, _)) = composer {
        let row = top + text_rows - 1;
        if let Some(line) = shelf.get(row) {
            targets.extend(
                footer::slot_targets(app, line)
                    .into_iter()
                    .map(|(col, w, target)| (row, col, Some(w), target)),
            );
        }
    }
    Bottom {
        stack0,
        shelf,
        status,
        cursor,
        targets,
        composer,
        asking: ask.as_ref().is_some_and(|o| o.kind == Kind::Ask),
    }
}

/// 输入行（最后一行，带放得下的槽）排成的样子：单元测试读槽用。
#[cfg(test)]
pub(crate) fn input_line(app: &App, width: usize) -> StyledLine {
    let rows = input_box::max_rows(30);
    let (slot, fits) = fitted_slot(app, width, rows);
    let shown = if fits { slot.spans } else { Vec::new() };
    let input = input_box::input_rows(app, width, rows, shown, true, None);
    input
        .rows
        .last()
        .map(|row| one_line(row, width))
        .unwrap_or_default()
}
