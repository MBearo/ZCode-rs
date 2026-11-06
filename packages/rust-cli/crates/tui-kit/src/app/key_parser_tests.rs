//! 按键解析的测试。
//! 移植自 packages/tui/tui/test/src/zcode/tui/app/KeyParserSuite.scala（feat/tui a2bf878b34）。
//! 与 Scala 的不同只有 `Key` 的形状：高于 U+FFFF 的码点是 `Key::Char`（Scala 是 `Key.Text`），鼠标键用具名字段。
use super::key::Key;
use super::key_parser::KeyParser;
use crate::theme::Rgb;

/// 喂一段字节，收集产出的所有键。
fn parse(bytes: &[u8]) -> Vec<Key> {
    KeyParser::new().feed(bytes)
}

fn parse_str(s: &str) -> Vec<Key> {
    parse(s.as_bytes())
}

fn ch(c: char) -> Key {
    Key::Char(c)
}

#[test]
fn printable_ascii_becomes_char() {
    assert_eq!(parse(b"a"), vec![ch('a')]);
    assert_eq!(parse(b"Q!"), vec![ch('Q'), ch('!')]);
}

#[test]
fn control_bytes_map_to_named_keys() {
    assert_eq!(parse(&[0x0d]), vec![Key::Enter]);
    assert_eq!(parse(&[0x0a]), vec![Key::Newline]);
    assert_eq!(parse(&[0x7f]), vec![Key::Backspace]);
    assert_eq!(parse(&[0x09]), vec![Key::Tab]);
}

#[test]
fn ctrl_chords() {
    assert_eq!(parse(&[0x03]), vec![Key::Ctrl('C')]);
    assert_eq!(parse(&[0x11]), vec![Key::Ctrl('Q')]);
    assert_eq!(parse(&[0x17]), vec![Key::Ctrl('W')]);
    assert_eq!(parse(&[0x01]), vec![Key::Ctrl('A')]);
}

#[test]
fn csi_arrows_home_end_delete() {
    assert_eq!(parse(b"\x1b[A"), vec![Key::Up]);
    assert_eq!(parse(b"\x1b[B"), vec![Key::Down]);
    assert_eq!(parse(b"\x1b[C"), vec![Key::Right]);
    assert_eq!(parse(b"\x1b[D"), vec![Key::Left]);
    assert_eq!(parse(b"\x1b[H"), vec![Key::Home]);
    assert_eq!(parse(b"\x1b[3~"), vec![Key::Delete]);
    assert_eq!(parse(b"\x1b[1~"), vec![Key::Home]);
}

#[test]
fn modified_arrows_keep_the_modifier() {
    // ESC [ 1 ; 5 C（Ctrl+→）：输入框里按词移动（docs/puffin-tui.md §4.3）。
    assert_eq!(parse(b"\x1b[1;5C"), vec![Key::with_ctrl(Key::Right)]);
}

#[test]
fn kitty_arrow_key_releases_are_ignored() {
    assert_eq!(parse(b"\x1b[1;1:3B"), vec![]);
    assert_eq!(parse(b"\x1b[1;5:3A"), vec![]);
}

#[test]
fn ss3_arrows() {
    assert_eq!(parse(b"\x1bOA"), vec![Key::Up]);
    assert_eq!(parse(b"\x1bOF"), vec![Key::End]);
}

#[test]
fn csi_u_enter_and_shift_enter() {
    assert_eq!(parse(b"\x1b[13u"), vec![Key::Enter]);
    assert_eq!(parse(b"\x1b[13;2u"), vec![Key::Newline]);
}

#[test]
fn csi_u_associated_text_inserts_normal_characters() {
    assert_eq!(parse(b"\x1b[97;1;97u"), vec![ch('a')]);
    assert_eq!(parse(b"\x1b[97;2;65u"), vec![ch('A')]);
}

#[test]
fn csi_u_ctrl_letters_remain_control_chords() {
    assert_eq!(parse(b"\x1b[113;5u"), vec![Key::Ctrl('Q')]);
}

#[test]
fn xterm_modify_other_keys_shift_enter_becomes_newline() {
    assert_eq!(parse(b"\x1b[27;2;13~"), vec![Key::Newline]);
}

#[test]
fn csi_u_functional_private_use_keys_are_not_inserted_as_text() {
    // kitty 的所有键模式把修饰键的按下报成功能键码。U+E061 是 Shift，不能变成一个私用区字符。
    assert_eq!(parse(b"\x1b[57441;2u"), vec![]);
    // 别的功能键（这里是 F13）仍作为应用忽略的键到达。
    assert_eq!(parse_str("\x1b[57376u"), vec![Key::Unknown]);
}

#[test]
fn csi_u_modifier_and_lock_keys_pressed_alone_are_not_keys() {
    // docs/puffin-tui.md §4.3。一个把 Ctrl 单按映射成 Esc 的键盘，每次 esc 实际送来的字节。
    let ctrl_tap_esc = "\x1b[57442;5u\x1b[57442;1:3u\x1b[27u\x1b[27;1:3u";
    assert_eq!(parse_str(&ctrl_tap_esc.repeat(2)), vec![Key::Esc, Key::Esc]);
    // ⌘、Alt、右 Ctrl、Hyper、Meta、Caps/Scroll/Num Lock：按下、重复、松开都不算键。
    for code in [
        57444, 57443, 57448, 57445, 57446, 57452, 57358, 57359, 57360,
    ] {
        for ev in [1, 2, 3] {
            assert_eq!(
                parse_str(&format!("\x1b[{code};1:{ev}u")),
                vec![],
                "code {code} event {ev}"
            );
        }
    }
    // 组合键照旧：Ctrl 按着时的 c 仍是 Ctrl+C。
    assert_eq!(
        parse_str("\x1b[57442;5u\x1b[99;5u\x1b[99;5:3u\x1b[57442;1:3u"),
        vec![Key::Ctrl('C')]
    );
}

#[test]
fn csi_u_shift_modifier_state_can_make_following_enter_a_newline() {
    assert_eq!(parse_str("\x1b[57441;2:1u\x1b[13;1u"), vec![Key::Newline]);
}

#[test]
fn csi_u_shift_modifier_state_remains_active_across_repeated_enter_presses() {
    assert_eq!(
        parse_str("\x1b[57441;2:1u\x1b[13;1u\x1b[13;1u"),
        vec![Key::Newline, Key::Newline]
    );
}

#[test]
fn csi_u_shift_modifier_state_remains_active_across_repeated_raw_enter_bytes() {
    assert_eq!(
        parse_str("\x1b[57441;2:1u\r\r"),
        vec![Key::Newline, Key::Newline]
    );
}

#[test]
fn csi_u_shift_release_clears_modifier_state() {
    assert_eq!(
        parse_str("\x1b[57441;2:1u\x1b[57441;1:3u\r"),
        vec![Key::Enter]
    );
}

#[test]
fn csi_u_key_release_with_associated_text_is_ignored() {
    assert_eq!(parse_str("\x1b[97;1:3;97u"), vec![]);
}

// ---- Alt、修饰与单独的 Esc（docs/puffin-tui.md §4.3） ----

#[test]
fn esc_then_a_printable_byte_is_alt_key() {
    assert_eq!(parse(b"\x1bb"), vec![Key::alt(ch('b'))]);
    assert_eq!(parse(b"\x1bf"), vec![Key::alt(ch('f'))]);
    assert_eq!(parse(&[0x1b, 0x7f]), vec![Key::alt(Key::Backspace)]);
    assert_eq!(parse(&[0x1b, 0x0d]), vec![Key::alt(Key::Enter)]);
}

#[test]
fn a_lone_esc_waits_and_the_timeout_flushes_it_as_esc() {
    let mut p = KeyParser::new();
    assert_eq!(p.feed(&[0x1b]), vec![]);
    assert!(p.pending_escape());
    assert_eq!(p.flush_escape(), vec![Key::Esc]);
    assert!(!p.pending_escape());
    assert_eq!(p.flush_escape(), vec![], "nothing left to flush");
}

#[test]
fn esc_bracket_or_esc_o_left_alone_is_alt_when_flushed() {
    let mut p = KeyParser::new();
    p.feed(b"\x1b[");
    assert!(p.pending_escape());
    assert_eq!(p.flush_escape(), vec![Key::alt(ch('['))]);
    p.feed(b"\x1bO");
    assert_eq!(p.flush_escape(), vec![Key::alt(ch('O'))]);
}

#[test]
fn esc_esc_is_esc_and_the_second_esc_waits() {
    let mut p = KeyParser::new();
    p.feed(&[0x1b]);
    assert_eq!(p.feed(&[0x1b]), vec![Key::Esc]);
    assert!(p.pending_escape());
}

#[test]
fn esc_then_a_control_byte_is_esc_then_that_key() {
    assert_eq!(parse(&[0x1b, 0x01]), vec![Key::Esc, Key::Ctrl('A')]);
}

#[test]
fn a_partial_csi_is_not_a_pending_escape() {
    let mut p = KeyParser::new();
    p.feed(b"\x1b[1");
    assert!(!p.pending_escape());
    assert_eq!(p.flush_escape(), vec![]);
    assert_eq!(p.feed(b";5D"), vec![Key::with_ctrl(Key::Left)]);
}

#[test]
fn modified_arrows_home_end_and_delete_keep_their_modifier() {
    assert_eq!(parse_str("\x1b[1;3D"), vec![Key::alt(Key::Left)]);
    assert_eq!(parse_str("\x1b[1;3C"), vec![Key::alt(Key::Right)]);
    assert_eq!(parse_str("\x1b[1;5D"), vec![Key::with_ctrl(Key::Left)]);
    assert_eq!(parse_str("\x1b[1;9D"), vec![Key::with_super(Key::Left)]);
    assert_eq!(
        parse_str("\x1b[1;2D"),
        vec![Key::Left],
        "shift alone changes nothing"
    );
    assert_eq!(parse_str("\x1b[1;5H"), vec![Key::with_ctrl(Key::Home)]);
    assert_eq!(parse_str("\x1b[3;5~"), vec![Key::with_ctrl(Key::Delete)]);
    assert_eq!(parse_str("\x1b[3;3~"), vec![Key::alt(Key::Delete)]);
    assert_eq!(
        parse_str("\x1b[1;3:1D"),
        vec![Key::alt(Key::Left)],
        "kitty press event"
    );
    assert_eq!(parse_str("\x1b[1;3:3D"), vec![], "kitty release event");
}

#[test]
fn kitty_alt_ctrl_and_super_chords_on_letters_backspace_and_enter() {
    assert_eq!(parse_str("\x1b[98;3u"), vec![Key::alt(ch('b'))]);
    assert_eq!(
        parse_str("\x1b[98;4u"),
        vec![Key::alt(ch('B'))],
        "Alt+Shift+b"
    );
    assert_eq!(parse_str("\x1b[127;3u"), vec![Key::alt(Key::Backspace)]);
    assert_eq!(
        parse_str("\x1b[127;5u"),
        vec![Key::with_ctrl(Key::Backspace)]
    );
    assert_eq!(
        parse_str("\x1b[127;9u"),
        vec![Key::with_super(Key::Backspace)]
    );
    assert_eq!(parse_str("\x1b[13;3u"), vec![Key::alt(Key::Enter)]);
    assert_eq!(parse_str("\x1b[97;5u"), vec![Key::Ctrl('A')]);
    assert_eq!(
        parse_str("\x1b[97;9u"),
        vec![Key::Unknown],
        "⌘+letter is the terminal's, not ours"
    );
    assert_eq!(parse_str("\x1b[27u"), vec![Key::Esc]);
}

#[test]
fn kitty_a_chord_with_associated_text_types_that_text() {
    // macOS 的 Option 组字：Option 不当 Alt 时，终端说它打出 ∫（U+222B）。
    assert_eq!(parse_str("\x1b[98;3;8747u"), vec![ch('∫')]);
}

#[test]
fn characters_above_u_ffff_arrive_whole() {
    assert_eq!(parse_str("👍"), vec![ch('👍')]);
    assert_eq!(parse_str("a😀b"), vec![ch('a'), ch('😀'), ch('b')]);
    assert_eq!(
        parse_str("\x1b[128077u"),
        vec![ch('👍')],
        "kitty code point"
    );
    assert_eq!(
        parse_str("\x1b[97;2;128077u"),
        vec![ch('👍')],
        "kitty associated text"
    );
}

#[test]
fn multi_byte_utf8_decodes_to_a_char() {
    // "é" = U+00E9 = 0xC3 0xA9
    assert_eq!(parse(&[0xc3, 0xa9]), vec![ch('é')]);
    // "›" = U+203A = 0xE2 0x80 0xBA
    assert_eq!(parse(&[0xe2, 0x80, 0xba]), vec![ch('›')]);
}

#[test]
fn sgr_mouse_wheel_decodes_to_wheel_keys_with_1_based_cells() {
    assert_eq!(
        parse_str("\x1b[<64;12;5M"),
        vec![Key::WheelUp { col: 12, row: 5 }]
    );
    assert_eq!(
        parse_str("\x1b[<65;1;1M"),
        vec![Key::WheelDown { col: 1, row: 1 }]
    );
}

fn press(button: u8, col: usize, row: usize, mods: u8) -> Key {
    Key::MousePress {
        button,
        col,
        row,
        mods,
    }
}

fn release(button: u8, col: usize, row: usize, mods: u8) -> Key {
    Key::MouseRelease {
        button,
        col,
        row,
        mods,
    }
}

#[test]
fn sgr_mouse_button_press_release() {
    assert_eq!(parse_str("\x1b[<0;3;4M"), vec![press(0, 3, 4, 0)]);
    assert_eq!(parse_str("\x1b[<0;3;4m"), vec![release(0, 3, 4, 0)]);
}

#[test]
fn sgr_press_release_keep_the_modifier_bits() {
    // shift 4、meta 8、ctrl 16。
    assert_eq!(parse_str("\x1b[<16;3;4M"), vec![press(0, 3, 4, 16)]);
    assert_eq!(parse_str("\x1b[<24;3;4m"), vec![release(0, 3, 4, 24)]);
    assert_eq!(parse_str("\x1b[<6;3;4M"), vec![press(2, 3, 4, 4)]);
}

#[test]
fn sgr_button_held_motion_decodes_to_mouse_drag_with_1_based_cells() {
    let drag = |button, col, row| Key::MouseDrag { button, col, row };
    assert_eq!(parse_str("\x1b[<32;5;6M"), vec![drag(0, 5, 6)]); // 左键拖动
    assert_eq!(parse_str("\x1b[<34;2;2M"), vec![drag(2, 2, 2)]); // 右键拖动
}

#[test]
fn sgr_motion_release_buttonless_hover_motion_and_horizontal_wheel_are_dropped() {
    assert_eq!(parse_str("\x1b[<32;5;6m"), vec![]); // 移动松开（结尾 `m`）
    assert_eq!(parse_str("\x1b[<35;5;6M"), vec![]); // 悬停移动，没有按键
    assert_eq!(parse_str("\x1b[<66;2;2M"), vec![]); // 水平滚轮
}

#[test]
fn an_sgr_wheel_sequence_delivered_one_byte_at_a_time_still_decodes() {
    let bytes = b"\x1b[<64;12;5M";
    let mut p = KeyParser::new();
    let out: Vec<Key> = bytes.iter().flat_map(|&b| p.feed(&[b])).collect();
    assert_eq!(out, vec![Key::WheelUp { col: 12, row: 5 }]);
    // 结尾 `M` 之前的每个字节都不完成任何东西。
    let mut q = KeyParser::new();
    assert!(
        bytes[..bytes.len() - 1]
            .iter()
            .all(|&b| q.feed(&[b]).is_empty())
    );
}

#[test]
fn a_bare_x10_mouse_intro_swallows_its_3_coordinate_bytes() {
    // ESC [ M，再是 3 个原始坐标字节（0x20 0x21 0x22，即空格 ! "），再是 'a'。只有 'a' 留下；坐标不能作为字符漏出。
    assert_eq!(parse(b"\x1b[M\x20\x21\x22a"), vec![ch('a')]);
}

#[test]
fn csi_page_up_page_down() {
    assert_eq!(parse(b"\x1b[5~"), vec![Key::PageUp]);
    assert_eq!(parse(b"\x1b[6~"), vec![Key::PageDown]);
    // 带修饰的形式解成同一个键。
    assert_eq!(parse_str("\x1b[5;2~"), vec![Key::PageUp]);
}

// ---- 括号粘贴 ----

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_STOP: &[u8] = b"\x1b[201~";

/// 开始标记、`body` 的字节、结束标记。
fn pasted(body: &[u8]) -> Vec<u8> {
    [PASTE_START, body, PASTE_STOP].concat()
}

fn paste(s: &str) -> Vec<Key> {
    parse(&pasted(s.as_bytes()))
}

#[test]
fn a_bracketed_paste_emits_one_paste_key_newlines_included() {
    assert_eq!(
        paste("line one\nline two"),
        vec![Key::Paste("line one\nline two".into())]
    );
}

#[test]
fn a_pasted_newline_never_becomes_enter_and_keys_after_the_paste_decode_normally() {
    let mut bytes = pasted(b"a\nb");
    bytes.push(0x0d);
    assert_eq!(parse(&bytes), vec![Key::Paste("a\nb".into()), Key::Enter]);
}

#[test]
fn crlf_and_lone_cr_in_a_paste_normalize_to_lf() {
    assert_eq!(paste("a\r\nb\rc"), vec![Key::Paste("a\nb\nc".into())]);
}

#[test]
fn tabs_survive_a_paste_and_other_control_bytes_are_dropped() {
    assert_eq!(
        parse(&pasted(b"a\tb\x07c")),
        vec![Key::Paste("a\tbc".into())]
    );
}

#[test]
fn an_esc_sequence_inside_a_paste_cannot_replay_as_a_key() {
    // 作为内容粘贴的 ESC [ A（↑）：ESC 丢掉，可打印的尾巴留作文字。
    assert_eq!(parse(&pasted(b"\x1b[Ax")), vec![Key::Paste("[Ax".into())]);
}

#[test]
fn a_broken_end_marker_prefix_inside_a_paste_stays_content() {
    // ESC [ 2 0 1 再 'Z'：终究不是结束标记；只有真的那个结束它。
    assert_eq!(
        parse(&pasted(b"x\x1b[201Z")),
        vec![Key::Paste("x[201Z".into())]
    );
}

#[test]
fn an_empty_paste_is_one_empty_paste() {
    // 剪贴板里是图片时终端的 ⌘V（docs/puffin-tui.md §4.2「图片」）。
    assert_eq!(parse(&pasted(b"")), vec![Key::Paste(String::new())]);
}

#[test]
fn a_paste_of_control_bytes_only_emits_nothing() {
    assert_eq!(parse(&pasted(&[0x07, 0x7f])), vec![]);
}

#[test]
fn a_paste_emits_nothing_until_the_end_marker_completes() {
    let bytes = pasted(b"hi");
    let mut p = KeyParser::new();
    let (last, init) = bytes.split_last().unwrap();
    assert_eq!(p.feed(init), vec![]);
    assert_eq!(p.feed(&[*last]), vec![Key::Paste("hi".into())]);
}

#[test]
fn multi_byte_utf8_inside_a_paste_decodes_intact() {
    assert_eq!(paste("héllo ✓"), vec![Key::Paste("héllo ✓".into())]);
}

#[test]
fn a_stray_end_marker_with_no_paste_in_progress_is_ignored() {
    assert_eq!(parse(PASTE_STOP), vec![]);
}

#[test]
fn shift_tab_in_the_legacy_form_and_in_the_kitty_protocol() {
    assert_eq!(parse(b"\x1b[Z"), vec![Key::BackTab]);
    assert_eq!(parse_str("\x1b[9;2u"), vec![Key::BackTab]);
    assert_eq!(parse_str("\x1b[9u"), vec![Key::Tab]);
}

#[test]
fn focus_reports_the_terminal_gained_or_lost_focus() {
    assert_eq!(parse_str("\x1b[I"), vec![Key::FocusIn]);
    assert_eq!(parse_str("\x1b[O"), vec![Key::FocusOut]);
}

// ---- 启动之后终端的回答（docs/puffin-tui.md §4.2「主题」） ----

#[test]
fn dec_2031_appearance_reports_1_is_dark_2_is_light() {
    assert_eq!(
        parse_str("\x1b[?997;1n"),
        vec![Key::Appearance { dark: true }]
    );
    assert_eq!(
        parse_str("\x1b[?997;2n"),
        vec![Key::Appearance { dark: false }]
    );
    assert_eq!(parse_str("\x1b[?997;3n"), vec![Key::Unknown]);
}

#[test]
fn an_osc_10_11_answer_ended_by_bel_or_st_is_a_colour_report() {
    let report = |slot, r, g, b| Key::ColorReport {
        slot,
        rgb: Rgb::new(r, g, b),
    };
    assert_eq!(
        parse_str("\x1b]11;rgb:2828/2828/2828\x07"),
        vec![report(11, 0x28, 0x28, 0x28)]
    );
    assert_eq!(
        parse_str("\x1b]10;rgb:eb/db/b2\x1b\\"),
        vec![report(10, 0xeb, 0xdb, 0xb2)]
    );
    // 两个回答连在一起，后面跟着打的字。
    assert_eq!(
        parse_str("\x1b]10;rgb:ff/ff/ff\x07\x1b]11;rgb:00/00/00\x07a"),
        vec![report(10, 255, 255, 255), report(11, 0, 0, 0), ch('a')]
    );
    assert_eq!(parse_str("\x1b]11;nonsense\x07"), vec![Key::Unknown]);
}

#[test]
fn an_esc_bracket_that_is_not_an_answer_is_still_alt_bracket_and_what_follows_is_typed() {
    let mut p = KeyParser::new();
    assert_eq!(p.feed(b"\x1b]"), vec![]);
    assert!(p.pending_escape());
    assert_eq!(p.flush_escape(), vec![Key::alt(ch(']'))]);
    assert_eq!(parse_str("\x1b]x"), vec![Key::alt(ch(']')), ch('x')]);
    assert_eq!(
        parse_str("\x1b]1a"),
        vec![Key::alt(ch(']')), ch('1'), ch('a')]
    );
    assert_eq!(parse_str("\x1b]\x1b[A"), vec![Key::alt(ch(']')), Key::Up]);
}

// ---- Rust 移植补的：码点按 char 处理，不经 UTF-16 ----

#[test]
fn utf8_split_across_reads_still_decodes() {
    let mut p = KeyParser::new();
    let bytes = "😀".as_bytes();
    assert_eq!(p.feed(&bytes[..2]), vec![]);
    assert_eq!(p.feed(&bytes[2..]), vec![ch('😀')]);
}

#[test]
fn a_surrogate_code_point_is_never_a_char() {
    // 代理区不是 char：UTF-8 编出的代理丢掉，kitty 报的代理码是 Unknown，关联文字里的代理跳过。
    assert_eq!(parse(&[0xed, 0xa0, 0x80]), vec![]);
    assert_eq!(parse_str("\x1b[55296u"), vec![Key::Unknown]);
    assert_eq!(parse_str("\x1b[97;1;55296:98u"), vec![ch('b')]);
}
