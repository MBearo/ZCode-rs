// 改写自 apps/zcode-host/crates/tui/src/ui/footer.rs（feat/rs 79b6691c55）：权限模式换成内核的模式；
// 去掉子代理、后台 shell、任务清单与回退浏览的提示；上下文提示来自 `ContextNotice` 事件。
//! 以前输入框下面的一行（docs/specs/rust-tui.md §8.4）按 Puffin 拆成两处（docs/specs/rust-cli/tui-kit.md §4.2、§4.3；
//! docs/puffin-tui.md §4.2「状态栏」「按键提示（槽）」「按键表（?）」）：
//!
//! - **槽**（[`slot`]）：以前 footer 左侧的提示（按状态给的一条、临时提示、搜索与选区的提示），画在输入行的右端；键 `fg2`、
//!   说明 `fg3`（`key_hint.rs`）。文字与中英文照旧（tui-kit.md §4.1 第 4 条）。
//! - **状态栏**（[`status`]）：权限模式与 `⇧⇥`、模型与档位、cwd；右边是上下文的表、数与量、速度；旧界面多出的缓存命中率、
//!   上下文快满的提示排在右边（tui-kit.md §4.3）。排版在 `status_bar.rs`。
//! - **按键表**（[`sheet`]）：`?` 面板，kit 的 `sheet_of` 排旧界面的按键（tui.md §7.4 的三组）。
//!
//! 画法：只产出 kit 的行与片段，墨色经 `Ink`。
use rust_cli_kernel_api::compaction::ContextNotice;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_tui_kit::app::element::Element;
use rust_cli_tui_kit::app::widgets::key_hints::{self, Group};
use rust_cli_tui_kit::render::width::string_width;
use rust_cli_tui_kit::render::{Span, Style, StyledLine};
use rust_cli_tui_kit::theme::theme::Theme;

use self::status_bar::{Part, Window};
use super::app::App;
use super::i18n::{self, Lang};
use super::mouse::Target;

/// 状态栏里模式的墨色，照 Puffin `shell/modes.rs` 的 `mode_style`：变更前确认与自动编辑 `fg3`，自主模式 brand，完全访问
/// amber（`code`）；Puffin 没有的自动模式与不询问沿用旧界面的 `code`。
pub fn bar_mode_style(mode: PermissionMode, theme: &Theme) -> Style {
    match mode {
        PermissionMode::Guarded => Style::fg(theme.brand),
        PermissionMode::BypassPermissions | PermissionMode::Auto | PermissionMode::DontAsk => {
            Style::fg(theme.code)
        }
        PermissionMode::Default | PermissionMode::AcceptEdits | PermissionMode::Plan => theme.fg3,
    }
}

/// 状态栏的模式：plan 开着时先是 fg2 的计划模式与 line 色的 ` · `（Puffin `mode_parts`），再是权限模式。没有旧的
/// `⏸` / `⏵⏵` 图标（tui-kit.md §6）。查看子代理时是它的模式（修复线 F112，tui-panels.md §1.7）。
pub fn mode_words(
    lang: Lang,
    mode: PermissionMode,
    plan: bool,
    theme: &Theme,
) -> Vec<(String, Style)> {
    let mut out = Vec::new();
    if plan {
        // 状态栏里计划模式写 `Plan`（Puffin kit `modes::parts`）；`Plan mode` 是 `/mode` 列表那一行的名字。
        let word = if lang == Lang::Zh { "计划" } else { "Plan" };
        out.push((word.to_owned(), theme.fg2));
        out.push((" · ".to_owned(), theme.line));
    }
    out.push((
        i18n::mode_label(lang, mode).to_owned(),
        bar_mode_style(mode, theme),
    ));
    out
}

/// 槽里的东西：键与说明的段；`sentence` 为真时是一句说明（刚发生的事），输入行放不下时改在状态栏右边说（Puffin
/// 「槽的说明句」），不丢。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Slot {
    pub spans: Vec<Span>,
    pub sentence: bool,
}

impl Slot {
    fn keyed(spans: Vec<Span>) -> Slot {
        Slot {
            spans,
            sentence: false,
        }
    }
}

/// 键与说明的对画成槽的段（kit `key_hints::spans`：键 fg2、说明 fg3，两对之间两格）。
pub fn pairs(theme: &Theme, keys: &[(&str, &str)]) -> Vec<Span> {
    key_hints::spans(theme, &key_hints::pairs(keys))
}

/// 槽（docs/puffin-tui.md §4.2「按键提示（槽）」，Puffin `slot_of` 的次序里第一条成立的规则；tui-kit.md §4.2）。键照 Puffin
/// 写成键帽的字形（`? keys`、`esc stop`、`⇧⏎ newline`、`⇥ complete`），不写 `to`、不写一看就懂的键（⏎ 发送、↑↓ 选、esc
/// 关上），两对之间两格，至多两对。旧界面才有的状态照 Puffin 最近的规则说：
///
/// 1. 一层自己的键：`?` 按键表开着时空着；输入框里有选区时 `^C copy  esc clear`；命令架开着时 `⇥ complete`（没有匹配时空着）；`@` 面板开着时 `⏎ insert`。
/// 2. 刚发生的事（连按的提示、粘贴、ctrl+r 等，旧界面的临时提示），到时为止；拖选复制过且选区还在时 fg3 的 `Copied N lines`。
/// 3. shell 命令（`!`）：草稿空着时 `⌫ leave`，否则 `⏎ run in your shell`；回合在跑时再加 `esc stop`。
/// 6. 回合在跑：有草稿时 `⏎ steer  ⇥ after this turn`（两条车道，同 Puffin；立即发送的键在 `?` 按键表里）；栈上有消息时
///    `⌫ take back  esc stop`；否则 `esc stop`。查看子代理时空着（esc 是回到主会话）。回合没在跑、栈上有消息、草稿空着时
///    `⌫ take back`。
/// 7. 一行的草稿：`⇧⏎ newline`；两行以上空着。
/// 9. `? keys`。
///
/// 草稿空时（tui-cockpit.md §3.4），只有一对的 `esc stop`（回合在跑）、`⌫ take back`、`? keys` 多一对驾驶舱的提示
/// `← agent cockpit`，别的代理有消息时说明换成数 `← ● 2 need you`：← 只在草稿空时能打开驾驶舱，提示也只在这时有。
///
/// 以前的 `← for projects`（2026-11-02 起以驾驶舱的名字加回，见上）、`/tasks to see subagents`、`ctrl+t to show tasks`、`↓ to manage` 移进 `?` 按键表。槽照 Puffin 只有
/// 一种写法（键最多两对，放不下时整个让开）；以前按省到哪一级（`Level`）先省 `? for shortcuts`，再省 `/tasks` 的提示（修复线
/// F30），它们现在在 `?` 按键表里（[`sheet_columns`]）。
pub fn slot(app: &App) -> Slot {
    let ink = &app.ink;
    let theme = &ink.theme;
    let keys = |ks: &[(&str, &str)]| Slot::keyed(pairs(theme, ks));
    // 1. 一层自己的键。`?` 按键表开着：空着（Puffin `sheet_open` 的槽）。
    if matches!(app.overlay, Some(super::app::Overlay::Shortcuts)) {
        return Slot::default();
    }
    // 鼠标选区不给提示（F25）；输入框里的选区给复制与取消。
    if app.composer.area.selection().is_some() {
        return keys(&[("^C", copy_word(app.lang)), ("esc", clear_word(app.lang))]);
    }
    // 2. 刚发生的事（旧界面的临时提示）到时之前替换整个槽：按键提示经组件画，其余是一句说明（fg2，Puffin「刚发生的事」）。
    if let Some(notice) = app.live_notice() {
        return if notice.keyed {
            Slot::keyed(super::key_hint::spans(&notice.text, ink))
        } else {
            Slot {
                spans: vec![Span::new(notice.text.clone(), theme.fg2)],
                sentence: true,
            }
        };
    }
    // 命令架：`⇥ complete`，没有匹配时空着（Puffin `shelf_open` 的槽）；`@` 面板：Puffin `at::keys` 去掉一看就懂的（`at_view.rs`）。
    match &app.composer.popup {
        Some(super::composer::Popup::Slash { .. }) => {
            return if app.composer.slash_matches().is_empty() {
                Slot::default()
            } else {
                keys(&[("⇥", complete_word(app.lang))])
            };
        }
        Some(super::composer::Popup::Skills { .. }) => {
            let pairs = app.skill_slot();
            let pairs: Vec<(&str, &str)> = pairs
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            return keys(&pairs);
        }
        Some(super::composer::Popup::Files { .. }) => {
            let pairs = app.at_slot(app.composer.files.width);
            let pairs: Vec<(&str, &str)> = pairs
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            return keys(&pairs);
        }
        None => {}
    }
    // 光标在粘贴的帽上：预览开着，`^D remove`（Puffin `peek_open` 的槽，kit `paste_caps::KEYS`）。
    if super::input_box::peek_open(app) {
        return keys(&[("^D", word(app.lang, "remove", "移除"))]);
    }
    // 复制的反馈（Puffin §4.4「复制的反馈」）：选区复制过且还在时是 fg3 的 `Copied N characters`，选区清掉时一起消失，不设
    // 定时器；复制途径的说明在输入框上方安静的一行（runner 的 `copied`）。
    if let Some(copied) = app.pointer.selection.and_then(|s| s.copied) {
        return Slot {
            spans: vec![Span::new(says_copied(app.lang, copied), theme.fg3)],
            sentence: true,
        };
    }
    // 门道开着（Puffin `door_slot`）：它这一步的键，或 `c` 与 `Another provider` 刚说的（fg2，到下一个键）。
    if let Some(says) = app.door_keys() {
        return match says {
            // 去掉明显的键、至多两个（Puffin 的 `pick`）。
            Ok(ks) => {
                let ks = key_hints::pick(&ks);
                let ks: Vec<(&str, &str)> =
                    ks.iter().map(|(k, w)| (k.as_str(), w.as_str())).collect();
                keys(&ks)
            }
            Err(note) => Slot {
                spans: vec![Span::new(note, theme.fg2)],
                sentence: true,
            },
        };
    }
    let stop = ("esc", stop_word(app.lang));
    // 5. 挑选回退的点（Puffin `rewind_slot`；到头时的一句是第 2 条「刚发生的事」）。挑选时不会是 shell 模式。
    match super::rewind_pick::slot(app) {
        Some(super::rewind_pick::Slot::Says(says)) => {
            return Slot {
                spans: vec![Span::new(says, theme.fg2)],
                sentence: true,
            };
        }
        Some(super::rewind_pick::Slot::Keys(ks)) => return keys(&ks),
        None => {}
    }
    // 3. shell 命令。
    if app.composer.mode == super::composer::InputMode::Bash {
        let mut ks = if app.composer.is_empty() {
            vec![("⌫", leave_word(app.lang))]
        } else {
            vec![("⏎", run_word(app.lang))]
        };
        if app.running() || shelling(app) {
            ks.push(stop);
        }
        return keys(&ks);
    }
    // 查看子代理时（修复线 F112，tui-panels.md §1.7）：esc 是返回主会话、不中断；槽空着。
    if app.viewing.is_some() {
        return Slot::default();
    }
    // 6. 回合在跑。
    if app.running() {
        return if !app.composer.is_empty() {
            keys(&[
                ("⏎", steer_word(app.lang)),
                ("⇥", after_turn_word(app.lang)),
            ])
        } else if app.has_stack() {
            keys(&[("⌫", take_back_word(app.lang)), stop])
        } else {
            with_hint(app, keys(&[stop]), true)
        };
    }
    // 有 `!` 命令在跑、回合还没开始（Puffin `shell_running`）：一行的草稿 `⇧⏎ newline  esc stop`，其余 `esc stop`。
    if shelling(app) {
        if !app.composer.is_empty() && !app.composer.area.text().contains('\n') {
            return keys(&[
                (key_hints::newline_key(false), newline_word(app.lang)),
                stop,
            ]);
        }
        return keys(&[stop]);
    }
    // 回合没有进行、栈上还有消息、输入框为空：取回。
    if app.composer.is_empty() && app.has_stack() {
        return with_hint(app, keys(&[("⌫", take_back_word(app.lang))]), true);
    }
    // 7. 一行的草稿；两行及以上时槽是空的。
    if !app.composer.is_empty() {
        return if app.composer.area.text().contains('\n') {
            Slot::default()
        } else {
            keys(&[(key_hints::newline_key(false), newline_word(app.lang))])
        };
    }
    // 9.
    with_hint(app, keys(&[("?", keys_word(app.lang))]), false)
}

/// 驾驶舱的提示（tui-cockpit.md §3.4）：平常是 `← agent cockpit`（同别的键对）；别的代理有消息时是 `←` fg2、组的记号（它的
/// 墨）、数 fg3，同页头一行的数。
fn cockpit_hint(app: &App) -> Vec<Span> {
    let theme = &app.ink.theme;
    let Some((kind, n)) = super::cockpit::news_of(&app.cockpit_news, app.session.as_deref()) else {
        return pairs(theme, &[("←", cockpit_word(app.lang))]);
    };
    vec![
        Span::new("← ", theme.fg2),
        super::cockpit_view::mark(theme, kind),
        Span::new(
            format!(" {}", super::cockpit::count_words(app.lang, kind, n)),
            theme.fg3,
        ),
    ]
}

/// 一对的槽加上驾驶舱的提示：`after` 为真时提示在后（`esc stop  ← …`），否则在前（`← …  ? keys`）。
fn with_hint(app: &App, slot: Slot, after: bool) -> Slot {
    let hint = cockpit_hint(app);
    let gap = Span::new("  ", Style::DEFAULT);
    let spans = if after {
        [slot.spans, vec![gap], hint].concat()
    } else {
        [hint, vec![gap], slot.spans].concat()
    };
    Slot::keyed(spans)
}

/// 有 `!` 命令在跑（`Transcript::shell_running`）。
fn shelling(app: &App) -> bool {
    app.view.shell_running()
}

/// 槽里的说明词（Puffin 的英文；中文是它们的译文）。
fn word(lang: Lang, en: &'static str, zh: &'static str) -> &'static str {
    match lang {
        Lang::En => en,
        Lang::Zh => zh,
    }
}

fn keys_word(lang: Lang) -> &'static str {
    word(lang, "keys", "按键")
}

fn cockpit_word(lang: Lang) -> &'static str {
    word(lang, "agent cockpit", "代理驾驶舱")
}

fn stop_word(lang: Lang) -> &'static str {
    word(lang, "stop", "停止")
}

fn steer_word(lang: Lang) -> &'static str {
    word(lang, "steer", "插话")
}

fn after_turn_word(lang: Lang) -> &'static str {
    word(lang, "after this turn", "这轮之后")
}

fn take_back_word(lang: Lang) -> &'static str {
    word(lang, "take back", "取回")
}

fn newline_word(lang: Lang) -> &'static str {
    word(lang, "newline", "换行")
}

fn complete_word(lang: Lang) -> &'static str {
    word(lang, "complete", "补全")
}

fn leave_word(lang: Lang) -> &'static str {
    word(lang, "leave", "退出")
}

fn run_word(lang: Lang) -> &'static str {
    word(lang, "run in your shell", "在你的 shell 里运行")
}

fn copy_word(lang: Lang) -> &'static str {
    word(lang, "copy", "复制")
}

fn clear_word(lang: Lang) -> &'static str {
    word(lang, "clear", "取消选择")
}

/// 复制的反馈的一句（Puffin `input.rs` 的 `copied_says`）：多行时写行数，否则写字数。
pub fn copied_says(lang: Lang, text: &str) -> String {
    says_copied(lang, super::mouse::Copied::of(text))
}

/// [`copied_says`]，按复制那一刻记下的行数与字数。
fn says_copied(lang: Lang, copied: super::mouse::Copied) -> String {
    let (lines, n) = (copied.lines, copied.chars);
    match lang {
        Lang::En if lines > 1 => format!("Copied {lines} lines"),
        Lang::En => format!("Copied {n} character{}", if n == 1 { "" } else { "s" }),
        Lang::Zh if lines > 1 => format!("已复制 {lines} 行"),
        Lang::Zh => format!("已复制 {n} 个字符"),
    }
}

impl App {
    /// 输入之前槽说的（Puffin `slotted` 的 `before.slot`）：还没画过槽时按现在的样子记下、当作早已落定，这次输入改了的槽就从
    /// 下一帧淡入。缺陷修复（tui-bottom，2026-10-31）：淡入原来只在画的时候比，第一帧之前的输入（启动后马上打 `/`、粘贴）改了的
    /// 槽没有可比的，直接出现；Puffin 每次更新前后都比，同一个输入的槽是淡入的（孪生 palette-filter、typing-100）。
    pub(super) fn note_slot(&mut self) {
        if self.slot_seen.is_some() {
            return;
        }
        let says: String = slot(self).spans.iter().map(|s| s.text.as_ref()).collect();
        let long_ago = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(3600))
            .unwrap_or_else(std::time::Instant::now);
        self.slot_seen = Some((says, long_ago));
    }
}

/// 排好的一行里槽的可点击处（起始列，宽度，目标）：`? keys`（打开按键表）与驾驶舱的提示（同按 ←，tui-cockpit.md §3.4）。
/// 以前还有 `← for projects`；它在按键表里。
pub fn slot_targets(app: &App, line: &StyledLine) -> Vec<(usize, usize, Target)> {
    let plain = line.plain();
    let mut out = Vec::new();
    let keys = format!("? {}", keys_word(app.lang));
    if plain.trim_end().ends_with(&keys) {
        let at = plain.rfind(&keys).unwrap_or(0);
        out.push((
            string_width(&plain[..at]),
            string_width(&keys),
            Target::Shortcuts,
        ));
    }
    let text: String = cockpit_hint(app)
        .iter()
        .map(|span| span.text.as_ref())
        .collect();
    if let Some(at) = plain.rfind(&text) {
        out.push((
            string_width(&plain[..at]),
            string_width(&text),
            Target::OpenCockpit,
        ));
    }
    out
}

/// 右边“上下文快满”的提示（compact.md §2.1）：快满时 `code`，其余 `fg2`。查看子代理时是它的。
fn context_span(app: &App) -> Option<Span> {
    let notice = app.shown().context_notice();
    let text = i18n::context_notice(app.lang, notice)?;
    let style = app
        .ink
        .context_notice(matches!(notice, ContextNotice::Low { .. }));
    Some(Span::new(text, style))
}

/// 状态栏左边与右边的部分。`bash`：shell 模式（Puffin「Shell 命令（!）」的状态栏：`Shell` 与文件夹，没有模型与 `⇧⇥`）。
fn parts(app: &App, bash: bool) -> (Vec<Part>, Vec<Part>) {
    let theme = &app.ink.theme;
    let shown = app.shown();
    let cwd = app.workspace_label();
    // 就绪的远端机器（Puffin `machines_where`）：第一台，不止一台时 ` +N`。
    let ready: Vec<&str> = app
        .machines
        .iter()
        .filter(|row| row.status == crate::machines::MachineStatus::Ready)
        .map(|row| row.name.as_str())
        .collect();
    let machines = rust_cli_tui_kit::app::widgets::machine_rows::where_(&ready);
    let mut left = Vec::new();
    if bash {
        // Puffin 的 shell 模式：`Shell` 与文件夹（kit `shell_left`），没有模型、`⇧⇥` 与机器。
        left.extend(status_bar::shell_left(
            theme,
            app.bottom_motion.shell_age(),
            &cwd,
        ));
    } else {
        // 查看子代理时 shift+tab 不生效（F112）：不写 `⇧⇥`。
        let words = mode_words(app.lang, shown.mode(), shown.plan(), theme);
        let motion = &app.status_motion;
        left.extend(status_bar::mode_parts(
            theme,
            words,
            motion.mode_age(),
            app.viewing.is_none(),
        ));
        // 目标的记号跟着模式（Puffin 状态栏，cards.rs 的会话目标）。
        left.extend(status_bar::goal_parts(
            theme,
            app.goal_status_parts(),
            motion.goal_age(),
        ));
        let selection = shown.selection();
        let model = (!selection.model.is_empty()).then(|| {
            (
                selection.model.as_str(),
                selection.reasoning.as_deref().unwrap_or(""),
                motion.label_age(),
            )
        });
        left.extend(status_bar::rest(theme, model, &cwd, machines.as_deref()));
    }
    let context = shown.context_usage().map(|usage| Window {
        used: usage.used_tokens,
        max: usage.max_tokens,
        compact_at: usage.compact_at,
        breakdown: Vec::new(),
    });
    // 速度：kit 的现场估算（Puffin `sped`；以前只有内核 `OutputSpeed` 量到的数）。查看子代理时（旧界面独有，Puffin 没有）
    // 照旧是那个会话量到的数，同 kit 在调用之间的写法；现场估算只听主会话。
    let speed = if app.viewing.is_some() {
        rust_cli_tui_kit::app::widgets::live_speed::LiveSpeed {
            shown: shown.output_speed().and_then(|s| s.tokens_per_second()),
            ..Default::default()
        }
        .spans(theme, 0, false)
    } else {
        app.status_motion.speed_spans(theme)
    };
    let mut group = Vec::new();
    group.extend(status_bar::group(
        theme,
        context,
        app.status_motion.context_age(),
        speed,
    ));
    if let Some(span) = context_span(app) {
        group.push(Part {
            right: true,
            ..Part::new("notice", vec![span], 80)
        });
    }
    (left, group)
}

/// 设置页开着时的状态栏（Puffin `status_line` 的设置分支）：左边照旧，右边只有这一页的键（`keys`），没有数字的那组。
pub fn settings_status(app: &App, width: usize, height: usize, keys: Vec<Span>) -> Vec<StyledLine> {
    let (left, _) = parts(app, false);
    let note = vec![Part {
        right: true,
        ..Part::new("note", keys, 95)
    }];
    status_bar::lines(&app.ink.theme, &left, &[], &note, width, height)
}

/// 状态栏（一或两行）。以前右边最前是后台命令的 pill（可点），2026-10-31 起并进任务行（tui-tasks.md §4）。`note` 是放不下输入行的说明句或搜索模式的键，占第 1 行的右端；`height` 是终端的
/// 行数（少于 20 行时总是一行）。
pub fn status(
    app: &App,
    width: usize,
    height: usize,
    note: Vec<Span>,
    bash: bool,
) -> Vec<StyledLine> {
    let (left, group) = parts(app, bash);
    let note: Vec<Part> = if note.is_empty() {
        Vec::new()
    } else {
        vec![Part {
            right: true,
            ..Part::new("note", note, 95)
        }]
    };
    status_bar::lines(&app.ink.theme, &left, &group, &note, width, height)
}

/// `?` 按键表的列（docs/puffin-tui.md §4.2「按键表（?）」，kit `key_hints::columns` 的三列与标题）：Writing、Typed first 与
/// Mouse、Anywhere。Puffin 有的条目照抄它的键与说法；旧界面才有的键排进意思最近的一组，说明
/// 放得进 28 格的一列；只在某一层里有的键（会话列表、回退列表、权限询问、页）各成一个小组（tui-kit.md §4.3、§6「`?` 面板」）。
/// `send_now` 是立即发送的键（[`send_now_glyph`]）。
pub fn sheet_columns(lang: Lang, send_now: &str) -> Vec<Vec<Group>> {
    use key_hints::{entry, gesture, group};
    let zh = lang == Lang::Zh;
    let pick = |en: &'static str, cn: &'static str| if zh { cn } else { en };
    let paste = key_hints::paste_key();
    vec![
        vec![group(
            pick("Writing", "输入"),
            vec![
                entry(key_hints::newline_key(false), pick("new line", "换行")),
                entry(paste, pick("paste an image", "粘贴图片")),
                entry("↑ ↓", pick("earlier messages", "之前的消息")),
                entry("^R", pick("search them", "搜索它们")),
                entry("^W", pick("cut a word", "剪掉一个词")),
                entry("^K", pick("cut to line end", "剪到行尾")),
                entry("^Y", pick("put back", "放回")),
                entry("^_", pick("undo", "撤销")),
                entry("^S", pick("stash the draft", "暂存草稿")),
                entry("^G", pick("open in editor", "在编辑器里打开")),
            ],
        )],
        vec![
            group(
                pick("Typed first", "开头打"),
                vec![
                    entry("/", pick("a command", "命令")),
                    entry("@", pick("a file or session", "文件或会话")),
                    entry("$", pick("a skill", "技能")),
                    entry("!", pick("a shell command", "shell 命令")),
                    entry("//", pick("find in the chat", "在对话里查找")),
                ],
            ),
            group(
                pick("Mouse", "鼠标"),
                vec![
                    gesture(pick("drag", "拖动"), pick("select and copy", "选择并复制")),
                    gesture(pick("click", "点击"), pick("open a link", "打开链接")),
                    gesture(pick("wheel", "滚轮"), pick("scroll", "滚动")),
                ],
            ),
            // `/resume` 列表上旧界面才有的键（sessions_steps.rs）：Puffin 的列表槽空着，这些只写在这里（tui-kit.md §4.3）。
            group(
                pick("Session list", "会话列表"),
                vec![
                    entry("⇥", pick("every folder", "所有文件夹")),
                    entry("^R", pick("rename", "改名")),
                    entry("^D", pick("delete", "删除")),
                    entry("^T", pick("read it", "读对话")),
                    entry("^E", pick("created order", "按创建时间排")),
                    entry("^F", pick("filter by status", "按状态筛")),
                    entry(
                        "^B",
                        pick("new session in its folder", "在它的文件夹新建会话"),
                    ),
                ],
            ),
            // `/rewind` 列表上旧界面才有的键（rewind.rs）：Puffin 的列表槽空着。
            group(
                pick("Rewind list", "回退列表"),
                vec![entry("^R", pick("rewind in place", "原地回退"))],
            ),
        ],
        vec![
            group(
                pick("Anywhere", "任何时候"),
                vec![
                    entry("⇧⇥", pick("permission mode", "权限模式")),
                    entry("esc", pick("stop the turn", "停下这一回合")),
                    entry(send_now, pick("send now", "立即发送")),
                    entry("⌥P", pick("model", "模型")),
                    entry("^O", pick("more details", "更多细节")),
                    // ^T 是任务卡（Puffin 工作流卡的键）；↓ 打开同一张卡，条件也相同（输入框为空），写在同一行，表不多一行
                    // （tui-tasks.md §3.1、§3.3）。
                    entry("^T ↓", pick("tasks", "任务")),
                    entry("←", pick("agent cockpit", "代理驾驶舱")),
                    entry("^N", pick("back to the end", "回到末尾")),
                    entry("^Q", pick("quit", "退出")),
                ],
            ),
            // 任务卡的键（task_card.rs）：槽至多两对，`^X stop` 只写在这里（tui-tasks.md §3.3）。
            group(
                pick("Task card", "任务卡"),
                vec![
                    entry("←→", pick("kind", "种类")),
                    entry("^X", pick("stop", "停止")),
                ],
            ),
            // 权限询问里选中 Always allow 时 ⇥ 改 Bash 的前缀（tui-asks 的能力）：槽照 Puffin 写 `1–N answer  esc stop`，
            // 这个键只写在这里（团队决定 2026-10-31，tui-kit.md §4.3）。
            group(
                pick("Permission ask", "权限询问"),
                vec![entry("⇥", pick("edit the rule", "改规则"))],
            ),
            // 页的复制（page_copy.rs）：Puffin 的页的槽没有这个键，只写在这里（tui-kit.md §4.3「页的复制」）。
            group(
                pick("Page", "页"),
                vec![
                    entry("c", pick("copy the page", "复制这一页")),
                    entry("^Y", pick("copy the settings", "复制设置页")),
                ],
            ),
        ],
    ]
}

/// `?` 按键表（docs/puffin-tui.md §4.2「按键表（?）」）：输入框的板上、输入行之上，每列 28 格、从第 3 列起，键画成键帽（line
/// 底、fg2），说明 fg3；没有标题行与边框。
pub fn sheet(app: &App, width: usize) -> Vec<Element> {
    key_hints::sheet_of(
        &app.ink.theme,
        &sheet_columns(app.lang, app.send_now_key()),
        width,
    )
}

/// 状态栏的排版（Puffin 的 `status_line` 与 `stats` 的移植）。
#[path = "status_bar.rs"]
pub mod status_bar;

#[cfg(test)]
#[path = "footer_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "footer_viewing_tests.rs"]
mod viewing_tests;
