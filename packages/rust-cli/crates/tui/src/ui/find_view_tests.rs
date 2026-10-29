//! 搜索模式的画面（docs/specs/rust-cli/tui-input.md §2.3.2–§2.3.5）：输入框与 footer、计数、高亮、折叠块里的匹配临时
//! 展开、流式追加与改宽度时找回当前匹配、没有匹配时回到原处、查看子代理时搜子会话、中英文、深浅主题、窄终端。
use std::time::Instant;

use crossterm::event::KeyCode;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use rust_cli_kernel_api::ids::{MessageId, ToolUseId, TurnId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::BlockDelta;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent, TurnOutcome};
use rust_cli_kernel_api::tool::ToolOutput;
use serde_json::json;

use super::tests::{app, ctrl, key, typed};
use super::*;
use crate::ui::palette::{ColorLevel, Palette};
use crate::ui::transcript::Cache;

/// 当前匹配的底色：深色背景、浅色背景。
const CURRENT_DARK: Color = Color::Rgb(240, 177, 0);
const CURRENT_LIGHT: Color = Color::Rgb(208, 135, 0);

struct Screen {
    app: App,
    cache: Cache,
    terminal: Terminal<TestBackend>,
    seq: u64,
}

impl Screen {
    fn new(width: u16, height: u16) -> Self {
        let mut app = app();
        // 有会话：不画新会话的背景标志。
        app.session = Some("sess_find".into());
        Self {
            app,
            cache: Cache::default(),
            terminal: Terminal::new(TestBackend::new(width, height)).unwrap(),
            seq: 0,
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

    /// 画一帧，返回每行的文字（宽字符的续格跳过、去掉行尾空白）。
    fn draw(&mut self) -> Vec<String> {
        let (app, cache) = (&mut self.app, &mut self.cache);
        self.terminal
            .draw(|frame| crate::ui::draw::draw(frame, app, cache, Instant::now(), 0))
            .unwrap();
        let buffer = self.buffer();
        (0..buffer.area.height)
            .map(|y| {
                let row: String = cells(&buffer, y).into_iter().map(|(_, s)| s).collect();
                row.trim_end().to_owned()
            })
            .collect()
    }

    fn buffer(&self) -> Buffer {
        self.terminal.backend().buffer().clone()
    }

    fn resize(&mut self, width: u16, height: u16) {
        self.terminal.backend_mut().resize(width, height);
        self.terminal.autoresize().unwrap();
    }

    /// 画面上 `needle` 第一次出现处的（列，行）。
    fn cell(&self, needle: &str) -> Option<(u16, u16)> {
        let buffer = self.buffer();
        (0..buffer.area.height).find_map(|y| {
            let cells = cells(&buffer, y);
            let row: String = cells.iter().map(|(_, s)| s.as_str()).collect();
            let at = row.find(needle)?;
            Some((cells[row[..at].chars().count()].0, y))
        })
    }

    fn bg(&self, needle: &str, offset: u16) -> Option<Color> {
        let (x, y) = self.cell(needle)?;
        Some(self.buffer()[(x + offset, y)].bg)
    }
}

/// 一行的格子：（列，字）；宽字符后面的续格跳过（测试终端只更新变了的格子，续格里可能是上一帧的字）。
fn cells(buffer: &Buffer, y: u16) -> Vec<(u16, String)> {
    let mut out = Vec::new();
    let mut x = 0;
    while x < buffer.area.width {
        let symbol = buffer[(x, y)].symbol();
        out.push((x, symbol.to_owned()));
        x += unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
    }
    out
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

fn input_row(lines: &[String]) -> Option<&String> {
    lines.iter().find(|line| line.starts_with("⌕ "))
}

#[test]
fn the_search_box_counts_and_highlights_and_esc_restores() {
    let mut screen = Screen::new(80, 24);
    conversation(&mut screen);
    let before = screen.draw();
    // Bash 并进工作段，折成一行摘要。
    assert!(
        before.iter().any(|l| l.contains("Ran 1 shell command")),
        "{before:#?}"
    );
    assert!(
        !before.iter().any(|l| l.contains("needle six")),
        "{before:#?}"
    );
    typed(&mut screen.app, "//");
    let lines = screen.draw();
    assert_eq!(
        input_row(&lines).map(String::as_str),
        Some("⌕ Search the transcript")
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("↑↓ previous/next · enter stop here · esc exit")),
        "{lines:#?}"
    );
    // 边线是强调色。
    let buffer = screen.buffer();
    let (_, row) = screen.cell("⌕ Search").unwrap();
    assert_eq!(buffer[(0, row - 1)].fg, screen.app.palette.accent);
    assert_eq!(buffer[(0, row)].fg, screen.app.palette.accent);
    // 输入：先停在离底部最近的一处（回复里），计数在右侧；回复里的那一处是当前匹配的颜色。
    typed(&mut screen.app, "needle");
    let lines = screen.draw();
    let row = input_row(&lines).unwrap();
    assert!(row.starts_with("⌕ needle") && row.ends_with("2/2"), "{row}");
    assert_eq!(screen.bg("NEEDLE is", 0), Some(CURRENT_DARK));
    assert!(
        !lines.iter().any(|l| l.contains("needle six")),
        "{lines:#?}"
    );
    // ↑：上一处在折起来的输出里——Bash 那一块临时展开；回复里的那一处只剩反色。
    key(&mut screen.app, KeyCode::Up);
    let lines = screen.draw();
    assert!(input_row(&lines).unwrap().ends_with("1/2"), "{lines:#?}");
    assert!(lines.iter().any(|l| l.contains("needle six")), "{lines:#?}");
    assert_eq!(screen.bg("needle six", 0), Some(CURRENT_DARK));
    let (x, y) = screen.cell("NEEDLE is").unwrap();
    let cell = &screen.buffer()[(x, y)];
    assert!(cell.modifier.contains(Modifier::REVERSED));
    assert_ne!(cell.bg, CURRENT_DARK);
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
    assert_ne!(screen.bg("six", 0), Some(CURRENT_DARK));
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
    assert_eq!(screen.bg("first alpha", 6), Some(CURRENT_DARK));
    let (x, y) = screen.cell("third alpha").unwrap();
    assert!(
        screen.buffer()[(x + 6, y)]
            .modifier
            .contains(Modifier::REVERSED)
    );
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
    screen.app.scroll = 0;
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
    assert_eq!(screen.bg("needle at", 0), Some(CURRENT_DARK));
    // esc 回到进入前的位置（跟随末尾）。
    key(&mut screen.app, KeyCode::Esc);
    let lines = screen.draw();
    assert!(
        !lines.iter().any(|l| l.contains("needle at the top")),
        "{lines:#?}"
    );
    assert_eq!(screen.app.scroll, 0);
}

#[test]
fn light_theme_and_chinese() {
    let mut screen = Screen::new(80, 24);
    screen.app.palette = Palette::new(ColorLevel::TrueColor, Some((255, 255, 255)));
    screen.app.set_lang(Lang::Zh);
    conversation(&mut screen);
    screen.draw();
    typed(&mut screen.app, "//");
    let lines = screen.draw();
    assert_eq!(
        input_row(&lines).map(String::as_str),
        Some("⌕ 搜索对话记录")
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("↑↓ 上一个/下一个 · enter 停在这里 · esc 退出")),
        "{lines:#?}"
    );
    typed(&mut screen.app, "NEEDLE");
    let lines = screen.draw();
    assert!(input_row(&lines).unwrap().ends_with("2/2"), "{lines:#?}");
    assert_eq!(screen.bg("NEEDLE is", 0), Some(CURRENT_LIGHT));
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
    let expected = format!("⌕ needle{}2/2", " ".repeat(12));
    assert_eq!(input_row(&lines), Some(&expected));
    // 查询比一行还长：只显示末尾，计数让位；光标在查询末尾。
    typed(&mut screen.app, " and a long tail");
    let lines = screen.draw();
    let row = input_row(&lines).unwrap();
    assert!(row.ends_with("a long tail"), "{row}");
    assert!(
        unicode_width::UnicodeWidthStr::width(row.as_str()) <= 23,
        "{row}"
    );
    // footer 的提示截在宽度里。
    assert!(
        lines
            .iter()
            .all(|l| unicode_width::UnicodeWidthStr::width(l.as_str()) <= 24)
    );
}

#[test]
fn the_detail_view_finds_lines_whose_lowercase_is_longer() {
    // 修复线 F114：以前详细视图跳过“转小写后变长”的行（`İ`），这样的行里什么都搜不到。
    let mut screen = Screen::new(80, 24);
    screen.event(KernelEvent::MessageDone {
        message: user("u1", "İstanbul and needle"),
    });
    screen.draw();
    ctrl(&mut screen.app, 'o');
    typed(&mut screen.app, "/needle");
    key(&mut screen.app, KeyCode::Enter);
    let lines = screen.draw();
    assert!(lines.last().unwrap().contains("1/1"), "{lines:#?}");
}
