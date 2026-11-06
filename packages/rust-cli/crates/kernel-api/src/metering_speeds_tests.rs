//! 会话的输出速度（spec §10.6）：每次调用结束都换上量到的数，回合按它的调用收尾，短的调用不算，补发与合并的查询不重算。
use super::SpeedHistory;
use crate::metering::{LastRequest, RequestStats};

/// 第 `requests` 个请求：输出 `output` 个 token，第一个字之后用了 `generation_ms`。
fn stats(requests: u64, output: u64, generation_ms: u64) -> RequestStats {
    RequestStats {
        requests,
        last: Some(LastRequest {
            output_tokens: Some(output),
            duration_ms: Some(generation_ms + 300),
            first_token_ms: Some(300),
        }),
        ..RequestStats::default()
    }
}

fn empty() -> RequestStats {
    RequestStats::default()
}

/// 新会话的回合：长的工具调用之后一句短回答。工具调用一结束就有量到的数（以前要等回合结束，而短回答不算，所以一直
/// 没有：状态栏的速度在调用之间消失）；短回答不改它；回合的数只算长的那次。
#[test]
fn a_long_call_in_the_middle_of_a_turn_is_measured_and_a_short_ending_does_not_erase_it() {
    let mut speeds = SpeedHistory::default();
    assert_eq!(speeds.record(&empty(), 0).last, None, "a new session");
    let after_tool = speeds.record(&stats(1, 200, 2000), 0);
    assert_eq!(after_tool.last, Some(100.0));
    assert!(after_tool.turns.is_empty(), "the turn has not ended");
    let after_reply = speeds.record(&stats(2, 12, 100), 0);
    assert_eq!(
        after_reply.last,
        Some(100.0),
        "a 12-token reply does not count"
    );
    let ended = speeds.record(&stats(2, 12, 100), 1);
    assert_eq!((ended.last, ended.turns), (Some(100.0), vec![100.0]));
}

/// 回合的数是它的调用的输出之和 ÷ 生成时长之和，不是最后一次调用的速度。
#[test]
fn a_turn_is_the_sum_of_its_outputs_over_the_sum_of_its_generation_times() {
    let mut speeds = SpeedHistory::default();
    speeds.record(&empty(), 0);
    speeds.record(&stats(1, 100, 1000), 0);
    let last = speeds.record(&stats(2, 300, 1000), 0);
    assert_eq!(last.last, Some(300.0));
    let ended = speeds.record(&stats(2, 300, 1000), 1);
    assert_eq!(ended.turns, vec![200.0]);
    // 下一个回合从零数起；没有量到的调用的回合不记数。
    let quiet = speeds.record(&stats(3, 10, 100), 2);
    assert_eq!((quiet.last, quiet.turns), (Some(300.0), vec![200.0]));
}

/// 回合结束之前发出的查询不收尾；同一个请求再查一次不重算（请求数没变）。
#[test]
fn only_a_query_sent_after_the_turn_ended_closes_it_and_a_repeat_counts_nothing() {
    let mut speeds = SpeedHistory::default();
    speeds.record(&empty(), 0);
    speeds.record(&stats(1, 200, 1000), 0);
    speeds.record(&stats(1, 200, 1000), 0);
    let ended = speeds.record(&stats(1, 200, 1000), 1);
    assert_eq!(ended.turns, vec![200.0], "counted once");
}

/// 冷加载：最近一次请求给出 `last` 与一个回合的数，不计入当前回合。
#[test]
fn a_cold_load_takes_the_latest_request_once() {
    let mut speeds = SpeedHistory::default();
    let cold = speeds.record(&stats(7, 120, 1000), 0);
    assert_eq!((cold.last, cold.turns), (Some(120.0), vec![120.0]));
    let ended = speeds.record(&stats(7, 120, 1000), 1);
    assert_eq!(
        ended.turns,
        vec![120.0],
        "the cold request is not counted again"
    );
}
