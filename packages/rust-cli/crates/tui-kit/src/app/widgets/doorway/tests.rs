//! 门道的中性画法的用例（docs/specs/rust-cli/tui-kit.md §4.4 K5）：头两行、头下面的块、占着板的两步与键各一个代表的输入。
//! Puffin 的 doorway/tests*.rs 经它的 `DoorStep` 与整屏测同样的规则。
use crate::app::element::Element;
use crate::app::layout::Layout;
use crate::render::{Span, Style};
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

use super::{
    BIGMODEL, Body, Head, KeyStep, ZAI, at_end, body, chunks, fold, head, in_welcome, keys, names,
    paste_rows, providers_rows,
};
use crate::app::widgets::usage::Family;

fn rich() -> Theme {
    Theme::from(Rgb::new(214, 216, 220), Rgb::new(21, 22, 26))
}

fn text(els: &[Element], w: usize) -> Vec<String> {
    els.iter()
        .flat_map(|e| Layout::lay(e, w))
        .map(|l| l.plain().trim_end().to_owned())
        .collect()
}

fn said(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_ref()).collect()
}

#[test]
fn the_head_says_where_the_doorway_is() {
    let t = rich();
    let (a, b) = head(&t, &Head::Choose { expired: None });
    assert_eq!(
        (said(&a), said(&b)),
        ("Connect a model to start.".to_owned(), String::new())
    );
    let (a, b) = head(
        &t,
        &Head::Choose {
            expired: Some(Family::Bigmodel),
        },
    );
    assert_eq!(said(&a), "Your BigModel sign-in has expired.");
    assert_eq!(b[0].style, t.fg3);
    let (a, _) = head(
        &t,
        &Head::Teams {
            family: Family::Zai,
            account: "sam",
            many: true,
        },
    );
    assert_eq!(
        said(&a),
        "Signed in to Z.ai as sam. This account has team plans."
    );
}

#[test]
fn the_body_lists_the_answers_and_the_sign_in_waits_under_a_rail() {
    let t = rich();
    let fams = [&ZAI, &BIGMODEL];
    assert_eq!(
        text(&body(&t, &Body::Choose { sel: 0 }, fams, "   "), 100),
        [
            "   1  Sign in to Z.ai       your z.ai account · Global",
            "   2  Sign in to BigModel   your bigmodel.cn account · China",
            "   3  Use an API key        Z.ai, BigModel, DeepSeek, OpenAI and more",
        ]
    );
    let sign_in = Body::SignIn {
        family: Family::Zai,
        url: "https://z.ai/x",
        failed: None,
        remote: false,
    };
    assert_eq!(
        text(&body(&t, &sign_in, fams, "   "), 100),
        [
            "   │ Opened the sign-in page in your browser. Sign in there.",
            "   │ https://z.ai/x",
            "   ○ Waiting for you to sign in",
        ]
    );
    let failed = Body::SignIn {
        family: Family::Zai,
        url: "",
        failed: Some("ETIMEDOUT"),
        remote: false,
    };
    assert_eq!(
        text(&body(&t, &failed, fams, "   "), 100)[0],
        "   ✗ Could not reach z.ai: ETIMEDOUT"
    );
    // 在 transcript 的末尾：空行、头两行，然后是块；在欢迎块里：块与一个空行。
    let end = at_end(
        (vec![Span::plain("Connect a model to start.")], Vec::new()),
        Vec::new(),
    );
    assert_eq!(text(&[end], 80), ["", "   Connect a model to start.", ""]);
    assert!(in_welcome(Vec::new()).is_none());
}

#[test]
fn the_board_shows_the_key_as_dots_and_the_providers_with_their_hosts() {
    let t = rich();
    assert_eq!(
        text(&paste_rows(&t, "DeepSeek", 3, 80), 80),
        ["   DeepSeek key", " › •••   hidden as you type"]
    );
    let rows = providers_rows(
        &t,
        &[("DeepSeek", "deepseek.com"), ("OpenAI", "openai.com")],
        8,
        1,
        true,
        "",
    );
    assert_eq!(
        text(&rows, 80),
        [
            "   Pick the provider your key is for",
            "   DeepSeek   deepseek.com",
            "   OpenAI     openai.com",
            "   + Custom provider   opens /settings",
            " ›  Search providers",
        ]
    );
    let chosen = Layout::lay(&rows[2], 80);
    assert_eq!(chosen[0].spans[1].style, Style::fg(t.brand));
}

#[test]
fn the_keys_follow_the_step_and_the_words_fold_and_cut() {
    let ks = |s, empty, ready| {
        keys(s, empty, ready)
            .into_iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ks(KeyStep::Choose, true, true),
        ["↑↓ choose", "⏎ connect", "esc close", "^Q quit"]
    );
    assert_eq!(
        ks(KeyStep::Choose, false, false),
        ["↑↓ choose", "⏎ queue message", "^Q quit"]
    );
    assert_eq!(
        ks(KeyStep::Paste { checking: true }, true, true),
        ["esc cancel"]
    );
    assert_eq!(
        names(&["Kimi".into(), "OpenAI".into(), "Qwen".into()], 14),
        "Kimi, OpenAI …"
    );
    assert_eq!(fold("one two three", 7), ["one two", "three"]);
    assert_eq!(chunks("abcdef", 4), ["abcd", "ef"]);
}
