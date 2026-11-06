//! 搜索模式的画面（docs/specs/rust-cli/tui-input.md §2.3.2–§2.3.5）：输入框与 footer、计数、高亮、折叠块里的匹配临时
//! 展开、流式追加与改宽度时找回当前匹配、没有匹配时回到原处、查看子代理时搜子会话、中英文、深浅主题、窄终端。
use std::time::Duration;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::ids::{MessageId, ToolUseId, TurnId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::BlockDelta;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent, TurnOutcome};
use rust_cli_kernel_api::tool::ToolOutput;
use rust_cli_tui_kit::render::width::string_width;
use rust_cli_tui_kit::render::{Color, Style, StyledLine, attr};
use serde_json::json;

use super::tests::{app, ctrl, key, typed};
use super::*;
use crate::ui::probe::{ColorLevel, Colors};
use crate::ui::transcript::Cache;

/// 换终端的颜色事实：墨色按新的事实与原来的主题选择重建（同入口探测后建墨色的那条路）。
fn set_colors(app: &mut App, colors: Colors) {
    app.ink = crate::ui::ink::Ink::chosen(
        colors.level,
        colors.foreground,
        colors.background,
        app.ink.choice,
    );
}

/// 直接比 `draw::draw` 排出的一屏 kit 的行（tui-kit.md §3.7）。
struct Screen {
    app: App,
    cache: Cache,
    size: (u16, u16),
    /// 上一次 [`Screen::draw`] 的一屏。
    lines: Vec<StyledLine>,
    seq: u64,
    /// 画了几次：每画一次钟前进 1 秒。流式的回复按播放头逐词显示（tui-kit.md §6「流式的回答」），播放头的 tick 取自
    /// `now - app.started`；钟不走就一直停在笔尖上。
    frames: u32,
}

impl Screen {
    fn new(width: u16, height: u16) -> Self {
        let mut app = app();
        // 报告了前景与背景：kit 的主题是 rich（tui-kit.md §3.4），当前匹配有 `code` 底色。
        set_colors(
            &mut app,
            Colors::new(ColorLevel::TrueColor, Some((0, 0, 0)))
                .with_foreground(Some((255, 255, 255))),
        );
        // 有会话：不画新会话的背景标志。
        app.session = Some("sess_find".into());
        Self {
            app,
            cache: Cache::default(),
            size: (width, height),
            lines: Vec::new(),
            seq: 0,
            frames: 0,
        }
    }

    fn event(&mut self, event: KernelEvent) {
        self.seq += 1;
        self.app.view.apply(
            SequencedEvent {
                seq: self.seq,
                revision: 0,
                event,
            },
            self.seq * 1000,
        );
    }

    /// 排一帧，返回每行的文字（去掉行尾空白）。
    fn draw(&mut self) -> Vec<String> {
        let (width, height) = self.size;
        self.frames += 1;
        let now = self.app.started + Duration::from_secs(u64::from(self.frames));
        let frame = crate::ui::draw::draw(&mut self.app, &mut self.cache, now, 0, width, height);
        assert_eq!(frame.lines.len(), usize::from(height));
        self.lines = frame.lines;
        self.lines
            .iter()
            .map(|line| line.plain().trim_end().to_owned())
            .collect()
    }

    fn resize(&mut self, width: u16, height: u16) {
        self.size = (width, height);
    }

    /// 画面上 `needle` 第一次出现处的（列，行）。
    fn cell(&self, needle: &str) -> Option<(usize, usize)> {
        self.lines.iter().enumerate().find_map(|(y, line)| {
            let row = line.plain();
            let at = row.find(needle)?;
            Some((string_width(&row[..at]), y))
        })
    }

    /// 第 `y` 行第 `x` 列的格子的样式。
    fn style(&self, x: usize, y: usize) -> Option<Style> {
        let mut col = 0;
        for span in &self.lines.get(y)?.spans {
            let w = string_width(&span.text);
            if x < col + w {
                return Some(span.style);
            }
            col += w;
        }
        None
    }

    /// 当前匹配的底色：主题的 `code`（tui-kit.md §4.3）。
    fn current(&self) -> Option<Color> {
        Some(self.app.ink.theme.code)
    }

    fn bg(&self, needle: &str, offset: usize) -> Option<Color> {
        let (x, y) = self.cell(needle)?;
        Some(self.style(x + offset, y)?.bg_color())
    }
}

fn user(id: &str, text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text { text: text.into() }],
        origin: UserOrigin::Human,
    })
}

fn assistant(id: &str, content: Vec<ContentBlock>) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content,
        model: "scripted".into(),
        stop_reason: None,
        usage: None,
    })
}

fn started() -> KernelEvent {
    KernelEvent::TurnStarted {
        turn: TurnId::new("t1"),
        command: None,
    }
}

fn finished() -> KernelEvent {
    KernelEvent::TurnFinished {
        turn: TurnId::new("t1"),
        outcome: TurnOutcome::Success,
        usage: None,
    }
}

/// 一个回合：用户输入、一次 Bash（6 行输出，`needle` 在第 6 行；普通视图里并进工作段，折成一行摘要）、一段回复。
fn conversation(screen: &mut Screen) {
    screen.event(started());
    screen.event(KernelEvent::MessageDone {
        message: user("u1", "show the files"),
    });
    screen.event(KernelEvent::ToolStarted {
        tool_use: ToolUseId::new("tu1"),
        name: "Bash".into(),
        input: json!({"command": "ls"}),
    });
    screen.event(KernelEvent::MessageDone {
        message: assistant(
            "a1",
            vec![ContentBlock::ToolUse {
                id: ToolUseId::new("tu1"),
                name: "Bash".into(),
                input: json!({"command": "ls"}),
            }],
        ),
    });
    screen.event(KernelEvent::ToolDone {
        tool_use: ToolUseId::new("tu1"),
        output: ToolOutput {
            content: vec![ToolResultContent::Text {
                text: "one\ntwo\nthree\nfour\nfive\nneedle six".into(),
            }],
            is_error: false,
            display: serde_json::Value::Null,
        },
    });
    screen.event(KernelEvent::MessageDone {
        message: Message::User(UserMessage {
            id: MessageId::new("r1"),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new("tu1"),
                content: vec![ToolResultContent::Text {
                    text: "one\ntwo\nthree\nfour\nfive\nneedle six".into(),
                }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new("a1"),
            },
        }),
    });
    screen.event(KernelEvent::MessageDone {
        message: assistant(
            "a2",
            vec![ContentBlock::Text {
                text: "The NEEDLE is the last file.".into(),
            }],
        ),
    });
    screen.event(finished());
}

/// 搜索模式的输入行：第 1 列是 `⌕`（Puffin 输入行 `›` 的位置，tui-kit.md §4.3）。
fn input_row(lines: &[String]) -> Option<&str> {
    lines
        .iter()
        .find(|line| line.starts_with(" ⌕ "))
        .map(String::as_str)
}

#[test]
fn the_search_box_counts_and_highlights_and_esc_restores() {
    let mut screen = Screen::new(80, 24);
    conversation(&mut screen);
    let before = screen.draw();
    // Bash 并进一段工作，收成一句（Puffin 的收拢）。
    assert!(
        before.iter().any(|l| l.contains("✓ Ran 1 command")),
        "{before:#?}"
    );
    assert!(
        !before.iter().any(|l| l.contains("needle six")),
        "{before:#?}"
    );
    typed(&mut screen.app, "//");
    let lines = screen.draw();
    assert_eq!(input_row(&lines), Some(" ⌕  Search the transcript"));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("↑↓ previous/next · ⏎ stop here · esc exit")),
        "{lines:#?}"
    );
    // `⌕` 是强调色（brand）；plain 主题时板的两条线也是。
    let (x, row) = screen.cell("⌕").unwrap();
    let accent = screen.app.ink.accent();
    assert_eq!(screen.style(x, row).map(Style::fg_color), Some(accent));
    if !screen.app.ink.theme.rich {
        assert_eq!(screen.style(0, row - 1).map(Style::fg_color), Some(accent));
        assert_eq!(screen.style(0, row + 1).map(Style::fg_color), Some(accent));
    }
    // 输入：先停在离底部最近的一处（回复里），计数在右侧；回复里的那一处是当前匹配的颜色。
    typed(&mut screen.app, "needle");
    let lines = screen.draw();
    let row = input_row(&lines).unwrap();
    assert!(
        row.starts_with(" ⌕ needle") && row.ends_with("2/2"),
        "{row}"
    );
    assert_eq!(screen.bg("NEEDLE is", 0), screen.current());
    assert!(
        !lines.iter().any(|l| l.contains("needle six")),
        "{lines:#?}"
    );
    // ↑：上一处在折起来的输出里——Bash 那一块临时展开；回复里的那一处只剩反色。
    key(&mut screen.app, KeyCode::Up);
    let lines = screen.draw();
    assert!(input_row(&lines).unwrap().ends_with("1/2"), "{lines:#?}");
    assert!(lines.iter().any(|l| l.contains("needle six")), "{lines:#?}");
    assert_eq!(screen.bg("needle six", 0), screen.current());
    let (x, y) = screen.cell("NEEDLE is").unwrap();
    let cell = screen.style(x, y).unwrap();
    assert!(cell.has_attr(attr::REVERSE));
    assert_ne!(Some(cell.bg_color()), screen.current());
    // 回绕：↑ 到最后一个，↓ 回到第一个。
    key(&mut screen.app, KeyCode::Up);
    assert!(input_row(&screen.draw()).unwrap().ends_with("2/2"));
    key(&mut screen.app, KeyCode::Down);
    assert!(input_row(&screen.draw()).unwrap().ends_with("1/2"));
    // esc：退出，块收起，画面与进入之前相同。
    key(&mut screen.app, KeyCode::Esc);
    assert_eq!(screen.draw(), before);
}

#[test]
fn no_match_says_so_and_snaps_back() {
    let mut screen = Screen::new(80, 24);
    conversation(&mut screen);
    let before = screen.draw();
    typed(&mut screen.app, "//needle");
    key(&mut screen.app, KeyCode::Up);
    screen.draw();
    // 没有匹配：右侧写 `no matches`，临时展开的块收起、回到进入前的样子。
    typed(&mut screen.app, "zzz");
    let lines = screen.draw();
    assert!(
        input_row(&lines).unwrap().ends_with("no matches"),
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("needle six")),
        "{lines:#?}"
    );
    let transcript = |lines: &[String]| lines[..lines.len() - 6].to_vec();
    assert_eq!(transcript(&lines), transcript(&before));
}

#[test]
fn enter_keeps_the_peeked_block_open() {
    let mut screen = Screen::new(80, 24);
    conversation(&mut screen);
    screen.draw();
    typed(&mut screen.app, "//six");
    let lines = screen.draw();
    assert!(input_row(&lines).unwrap().ends_with("1/1"), "{lines:#?}");
    key(&mut screen.app, KeyCode::Enter);
    let lines = screen.draw();
    assert!(input_row(&lines).is_none(), "{lines:#?}");
    assert!(lines.iter().any(|l| l.contains("needle six")), "{lines:#?}");
    // 高亮随搜索模式一起去掉。
    assert_ne!(screen.bg("six", 0), screen.current());
}

#[test]
fn streamed_text_adds_matches_without_moving_the_current_one() {
    let mut screen = Screen::new(80, 24);
    screen.event(started());
    screen.event(KernelEvent::MessageDone {
        message: user("u1", "go"),
    });
    let delta = |text: &str| KernelEvent::StreamDelta {
        turn: TurnId::new("t1"),
        response: 0,
        index: 0,
        delta: BlockDelta::Text(text.into()),
    };
    screen.event(delta("first alpha\n\n"));
    screen.draw();
    typed(&mut screen.app, "//alpha");
    let lines = screen.draw();
    assert!(input_row(&lines).unwrap().ends_with("1/1"), "{lines:#?}");
    screen.event(delta("second alpha\n\nthird alpha\n\n"));
    let lines = screen.draw();
    // 新的两处算进总数，当前匹配还是第一处。
    assert!(input_row(&lines).unwrap().ends_with("1/3"), "{lines:#?}");
    assert_eq!(screen.bg("first alpha", 6), screen.current());
    let (x, y) = screen.cell("third alpha").unwrap();
    assert!(screen.style(x + 6, y).unwrap().has_attr(attr::REVERSE));
}

#[test]
fn a_width_change_keeps_the_current_match_and_brings_it_into_view() {
    let mut screen = Screen::new(80, 24);
    // 很多行的回复：`needle` 在开头，跟随末尾时不在视口里。
    let mut text = String::from("needle at the top\n\n");
    for index in 0..40 {
        text.push_str(&format!("line {index}\n\n"));
    }
    screen.event(KernelEvent::MessageDone {
        message: user("u1", "go"),
    });
    screen.event(KernelEvent::MessageDone {
        message: assistant("a1", vec![ContentBlock::Text { text }]),
    });
    screen.draw();
    typed(&mut screen.app, "//needle");
    let lines = screen.draw();
    assert!(
        lines.iter().any(|l| l.contains("needle at the top")),
        "{lines:#?}"
    );
    assert!(input_row(&lines).unwrap().ends_with("1/1"));
    // 滚开它（滚轮、翻页不换当前匹配），再改宽度：重新找回并滚回视口。
    screen.app.port.scroll = 0;
    let lines = screen.draw();
    assert!(
        !lines.iter().any(|l| l.contains("needle at the top")),
        "{lines:#?}"
    );
    screen.resize(50, 24);
    let lines = screen.draw();
    assert!(
        lines.iter().any(|l| l.contains("needle at the top")),
        "{lines:#?}"
    );
    assert!(input_row(&lines).unwrap().ends_with("1/1"), "{lines:#?}");
    assert_eq!(screen.bg("needle at", 0), screen.current());
    // esc 回到进入前的位置（跟随末尾）。
    key(&mut screen.app, KeyCode::Esc);
    let lines = screen.draw();
    assert!(
        !lines.iter().any(|l| l.contains("needle at the top")),
        "{lines:#?}"
    );
    assert_eq!(screen.app.port.scroll, 0);
}

#[test]
fn light_theme_and_chinese() {
    let mut screen = Screen::new(80, 24);
    set_colors(
        &mut screen.app,
        Colors::new(ColorLevel::TrueColor, Some((255, 255, 255))).with_foreground(Some((0, 0, 0))),
    );
    screen.app.set_lang(Lang::Zh);
    conversation(&mut screen);
    screen.draw();
    typed(&mut screen.app, "//");
    let lines = screen.draw();
    assert_eq!(input_row(&lines), Some(" ⌕  搜索对话记录"));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("↑↓ 上一个/下一个 · ⏎ 停在这里 · esc 退出")),
        "{lines:#?}"
    );
    typed(&mut screen.app, "NEEDLE");
    let lines = screen.draw();
    assert!(input_row(&lines).unwrap().ends_with("2/2"), "{lines:#?}");
    assert_eq!(screen.bg("NEEDLE is", 0), screen.current());
    typed(&mut screen.app, "zz");
    let lines = screen.draw();
    assert!(
        input_row(&lines).unwrap().ends_with("没有匹配"),
        "{lines:#?}"
    );
}

#[test]
fn a_narrow_terminal_drops_the_count_and_shows_the_end_of_the_query() {
    let mut screen = Screen::new(24, 20);
    conversation(&mut screen);
    screen.draw();
    typed(&mut screen.app, "//needle");
    let lines = screen.draw();
    // 槽的最后一格在第 宽度 − 2 列（kit `key_hints::edge` 是宽度 − 1，不含）。
    let expected = format!(" ⌕ needle{}2/2", " ".repeat(11));
    assert_eq!(input_row(&lines), Some(expected.as_str()));
    // 查询比一行还长：只显示末尾，计数让位；光标在查询末尾。
    typed(&mut screen.app, " and a long tail");
    let lines = screen.draw();
    let row = input_row(&lines).unwrap();
    assert!(row.ends_with("a long tail"), "{row}");
    assert!(string_width(row) <= 23, "{row}");
    // footer 的提示截在宽度里。
    assert!(lines.iter().all(|l| string_width(l) <= 24));
}

#[test]
fn the_detailed_look_finds_lines_whose_lowercase_is_longer() {
    // 修复线 F114：以前详细视图跳过“转小写后变长”的行（`İ`），这样的行里什么都搜不到。详细视图删掉之后（^O 在原地，
    // tui-kit.md §4.2）同样的行在详细的样子里用 `//` 搜。
    let mut screen = Screen::new(80, 24);
    screen.event(KernelEvent::MessageDone {
        message: user("u1", "İstanbul and needle"),
    });
    screen.draw();
    ctrl(&mut screen.app, 'o');
    typed(&mut screen.app, "//needle");
    let lines = screen.draw();
    let row = input_row(&lines).unwrap_or_default();
    assert!(row.ends_with("1/1"), "{lines:#?}");
}
