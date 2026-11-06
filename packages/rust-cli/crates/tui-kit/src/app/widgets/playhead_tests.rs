//! 流式的播放头（docs/puffin-tui.md §4.2「流式节奏」）。
//! 移植自 packages/tui/tui/test/src/zcode/tui/shell/PlayheadSuite.scala（feat/tui a2bf878b34）。
//!
//! 与 Scala 的不同：位置是字节偏移（spec §3）。精确位置 `pos` 拆成 `at`（字节）与 `frac`；这里的文字除中文外都是 ASCII，
//! `pos.toInt` 就是 `at`。中文的词是自带的切分（一个汉字一个词，片假名一串一个词），不是 ICU 的词典分词（spec §15.1）：
//! Scala 的「我们」是一个词，这里是两个，所以「在词里」的断言改用片假名。
use crate::app::widgets::playhead::{Playhead, cap, landing, word_end};
use crate::theme::ease;

/// 一个 tick 一步，在固定的文字上走 `ticks` 步；交回每步之后的精确位置（节奏；显示的按整词跟着它）。
fn run(p0: Playhead, text: &str, fin: bool, ticks: i64, from: i64) -> (Playhead, Vec<usize>) {
    let mut p = p0;
    let mut out = Vec::new();
    for t in 0..ticks {
        p = p.step(text, fin, from + t);
        out.push(p.at);
    }
    (p, out)
}

/// 相邻两个位置的差（从 0 起）。
fn steps(shown: &[usize]) -> Vec<i64> {
    std::iter::once(0)
        .chain(shown.iter().copied())
        .collect::<Vec<usize>>()
        .windows(2)
        .map(|w| w[1] as i64 - w[0] as i64)
        .collect()
}

fn words() -> String {
    (1..=60)
        .map(|i| format!("word{i}"))
        .collect::<Vec<_>>()
        .join(" ")
        + " "
}

/// 前 `n` 个字，截到最后一个空格（Scala 的 `take(n).reverse.dropWhile(_ != ' ').reverse`）。
fn up_to_space(s: &str, n: usize) -> String {
    let t = &s[..n.min(s.len())];
    t.rfind(' ').map_or(String::new(), |i| t[..=i].to_owned())
}

#[test]
fn a_burst_is_a_speed_up_the_steps_grow_gradually_none_is_a_jump() {
    let text = up_to_space(&words(), 190);
    let (_, shown) = run(Playhead::start(0), &text, false, 40, 0);
    let st = steps(&shown);
    // 第一个 tick 只显示几个字，之后逐渐加速：不会一下子出现一大块。
    assert!(st[0] <= 3, "{st:?}");
    let max = *st.iter().max().unwrap_or(&0);
    assert!(max <= 30, "{st:?}");
    let top = st.iter().position(|x| *x == max).unwrap_or(0);
    assert!(st[..=top].windows(2).all(|w| w[1] >= w[0] - 1), "{st:?}");
    assert_eq!(shown.last().copied(), Some(text.len()));
}

#[test]
fn a_steady_stream_is_shown_about_0_3_s_behind_it() {
    // 每 33 ms 到 8 个字符（约 240 字/秒），持续 3 s。
    let words = (1..=300)
        .map(|i| format!("word{i}"))
        .collect::<Vec<_>>()
        .join(" ")
        + " ";
    let mut p = Playhead::start(0);
    let mut arrived = 0usize;
    let mut lags = Vec::new();
    for t in 0..90 {
        arrived = words.len().min(arrived + 8);
        p = p.step(&words[..arrived], false, t);
        if t > 45 {
            lags.push((arrived - p.shown) as f64);
        }
    }
    let mean = lags.iter().sum::<f64>() / lags.len() as f64;
    // 0.3 s ≈ 9 个 tick ≈ 72 个字符（加上单词边界的余量）。
    assert!(mean > 50.0 && mean < 110.0, "mean lag {mean} chars");
}

#[test]
fn when_the_stream_stalls_the_playhead_slows_down_before_it_stops() {
    let text = up_to_space(&words(), 120);
    // 先追到稳定的速度，再停止到达。
    let mut p = Playhead::start(0);
    for t in 0..12 {
        p = p.step(&text[..text.len().min((t as usize + 1) * 10)], false, t);
    }
    let at = p.at;
    let (_, after) = run(p, &text, false, 45, 12);
    let all: Vec<usize> = std::iter::once(at).chain(after.iter().copied()).collect();
    let st: Vec<i64> = steps(&all).into_iter().skip(1).filter(|x| *x > 0).collect();
    assert!(st.len() >= 4, "{st:?}");
    assert!(st.last() < st.first(), "{st:?}");
    assert_eq!(after.last().copied(), Some(text.len()));
}

#[test]
fn it_never_passes_a_word_that_has_not_finished_arriving() {
    let (p, _) = run(Playhead::start(0), "hello wor", false, 60, 0);
    assert_eq!(p.shown, 6);
    // final 之后可以。
    assert_eq!(run(p, "hello wor", true, 20, 60).0.shown, 9);
}

#[test]
fn a_word_longer_than_24_characters_is_shown_as_it_arrives() {
    let long = "x".repeat(40);
    assert_eq!(run(Playhead::start(0), &long, false, 60, 0).0.shown, 40);
}

#[test]
fn after_the_item_is_final_it_never_slows_down_and_the_rest_is_shown_within_about_0_2_s() {
    let w = words();
    let text = &w[..60];
    let mut p = Playhead::start(0);
    for t in 0..30 {
        p = p.step(text, false, t);
    }
    assert!(text.len() > p.shown);
    let at = p.at;
    let (q, after) = run(p, text, true, 8, 30);
    assert_eq!(after.last().copied(), Some(text.len()));
    assert_eq!(q.shown, text.len());
    let all: Vec<usize> = std::iter::once(at).chain(after.iter().copied()).collect();
    let st: Vec<i64> = steps(&all)
        .into_iter()
        .skip(1)
        .take_while(|x| *x > 0)
        .collect();
    assert!(st.windows(2).all(|w| w[1] >= w[0] - 1), "{st:?}");
}

#[test]
fn it_never_goes_back_and_a_snapshot_start_shows_the_text_at_once() {
    let p = Playhead::start(12);
    assert_eq!(p.step("hello", false, 0).shown, 12);
    assert_eq!(Playhead::start(5).shown, 5);
}

#[test]
fn words_land_whole_what_is_shown_is_always_at_a_word_s_end_never_half_a_word() {
    let text = up_to_space(&words(), 190);
    let b = text.as_bytes();
    let mut p = Playhead::start(0);
    let mut seen = std::collections::BTreeSet::new();
    for t in 0..40 {
        p = p.step(&text, false, t);
        seen.insert(p.shown);
        let sh = p.shown;
        assert!(
            sh == 0
                || sh == text.len()
                || !b[sh].is_ascii_alphanumeric()
                || !b[sh - 1].is_ascii_alphanumeric(),
            "half a word at {sh}: {}",
            &text[..sh]
        );
        assert!(
            sh <= p.at,
            "it never shows more than the playhead has passed"
        );
    }
    assert!(seen.len() > 10, "many landings, a word or a few each");
    // 中文：这里一个汉字一个词（Scala 的 ICU 把「我们」当一个词，`landing(zh, 1)` 是 0）。字的边界上就是它自己。
    let zh = "我们先看看页脚为什么会换行。";
    assert_eq!(
        landing(zh, 3),
        3,
        "between 我 and 们: each Han character is a word here"
    );
    assert_eq!(landing(zh, 6), 6, "between words");
    // 片假名一串是一个词：在它里面时回到它的开头。
    assert_eq!(
        landing("カタカナです", 6),
        0,
        "inside カタカナ: back to its start"
    );
    assert_eq!(
        landing(&"x".repeat(40), 30),
        30,
        "a run longer than 24 with no word end shows as it comes"
    );
    assert_eq!(landing("hello world", 8), 6);
    assert_eq!(landing("hello world", 5), 5);
}

#[test]
fn the_nib_s_rest_follows_the_playhead_idle_counts_ticks_since_it_last_moved() {
    let text = "hello wor";
    let mut p = Playhead::start(0);
    for t in 0..60 {
        p = p.step(text, false, t);
    }
    // 播放头停在 `wor` 前面等它到齐：笔尖停住。
    assert!(p.idle(59) > ease::REST_TICKS, "{p:?}");
    let w = words();
    let moving = Playhead::start(0).step(&w, false, 0).step(&w, false, 1);
    assert_eq!(moving.idle(1), 0);
    assert!(
        Playhead::start(5).idle(3) > 1_000_000,
        "never moved: long ago"
    );
}

#[test]
fn each_character_knows_its_age_while_it_settles_the_item_is_done_once_the_settling_is_over() {
    let text = "hi there ";
    let mut p = Playhead::start(0);
    let mut t = 0i64;
    while p.shown < text.len() {
        p = p.step(text, true, t);
        t += 1;
    }
    let last = t - 1;
    assert_eq!(p.age_of(text.len() - 1, last), Some(0));
    assert!(!p.done(text.len(), true, last));
    assert_eq!(p.age_of(0, last + 30), None);
    // 提交要等最后的字落定：130 ms（§4.2「流式节奏」的「提交」）。
    assert!(!p.done(text.len(), true, last + ease::TICKS - 1));
    assert!(p.done(text.len(), true, last + ease::TICKS));
    assert_eq!(ease::TICKS, 4, "a new word settles in 130 ms");
    assert!(!p.done(text.len(), false, last + 100));
}

#[test]
fn text_shown_long_before_it_is_final_waits_for_the_pen_to_lift_done_4_ticks_after_it_is_final() {
    let text = "hi there";
    let p = Playhead::start(text.len()).step(text, false, 0);
    assert_eq!(p.ended_for(50), None, "not final yet");
    let fin = p.step(text, true, 100);
    assert_eq!(fin.ended_for(101), Some(1));
    assert!(!fin.done(text.len(), true, 100 + ease::LIFT_TICKS - 1));
    assert!(fin.done(text.len(), true, 100 + ease::LIFT_TICKS));
}

#[test]
fn chinese_is_shown_word_by_word_it_does_not_wait_for_a_space_or_for_24_characters() {
    // 修复前：中文没有空格，第一个字要等一整行（超过 24 个字）才出现。
    let text = "我们先看看页脚为什么会换行";
    let c = cap(text);
    assert!(
        c > 0 && c < text.len(),
        "cap {c}: the last word may still be arriving, the ones before it are whole"
    );
    assert_eq!(
        cap(&format!("{text}。")),
        text.len() + "。".len(),
        "a mark ends the last word"
    );
    let mut p = Playhead::start(0);
    for t in 0..6 {
        p = p.step(text, false, t);
    }
    assert!(p.shown > 0, "the first characters are shown within 0.2 s");
}

#[test]
fn word_end_stops_at_a_wide_character_nothing_after_it_is_laid_in_advance() {
    // 字节偏移：第 1 个字在 3。
    assert_eq!(word_end("我们先看看", 3), 3);
    assert_eq!(word_end("ab我们", 0), 2);
}

#[test]
fn word_end_the_end_of_the_word_the_playhead_is_in() {
    assert_eq!(word_end("hello world", 7), 11);
    assert_eq!(word_end("hello world", 5), 5);
    assert_eq!(word_end("hello\nworld", 2), 5);
}

/// 新（Rust）：BMP 之外的字按 2 个单元算（Scala 的 `String.length`），节奏与 Scala 相同；显示的从不停在一对代理项中间。
#[test]
fn an_emoji_weighs_two_units_and_what_is_shown_is_always_on_a_character_boundary() {
    let text = "🚀🚀🚀🚀 done 🚀🚀 ok ".repeat(6);
    let mut p = Playhead::start(0);
    for t in 0..80 {
        p = p.step(&text, true, t);
        assert!(
            text.is_char_boundary(p.shown) && text.is_char_boundary(p.at),
            "{p:?}"
        );
    }
    assert_eq!(p.shown, text.len());
}
