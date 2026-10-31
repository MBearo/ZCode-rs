//! 数字写法与超限说明（context-usage.md §4、§5.1）。
use super::*;

#[test]
fn numbers_use_the_compact_form() {
    let cases = [
        (0, "0"),
        (21, "21"),
        (999, "999"),
        (1000, "1k"),
        (1647, "1.6k"),
        (2000, "2k"),
        (8357, "8.4k"),
        (33_000, "33k"),
        (958_643, "958.6k"),
        (999_960, "1m"),
        (1_000_000, "1m"),
        (1_500_000, "1.5m"),
        (200_000, "200k"),
    ];
    for (value, text) in cases {
        assert_eq!(format_tokens(value), text, "{value}");
    }
}

#[test]
fn estimates_round_to_tens() {
    assert_eq!(format_estimate(19), "< 20");
    assert_eq!(format_estimate(20), "~20");
    assert_eq!(format_estimate(124), "~120");
    assert_eq!(format_estimate(125), "~130");
    assert_eq!(format_estimate(2034), "~2k");
}

fn usage(total: u64, window: u64, source: WindowSource, auto: bool) -> ContextUsage {
    ContextUsage {
        model: rust_cli_base::models::GLM_5_3.into(),
        total_tokens: total,
        raw_max_tokens: window,
        percentage: 0,
        window_source: source,
        auto_compact: auto,
        categories: Vec::new(),
        mcp_tools: Vec::new(),
        memory_files: Vec::new(),
        agents: Vec::new(),
        skills: Vec::new(),
    }
}

#[test]
fn the_over_limit_note_follows_the_window_source() {
    assert_eq!(
        usage(10, 200_000, WindowSource::Auto, true).over_limit(),
        None
    );
    assert_eq!(
        usage(201_500, 200_000, WindowSource::Auto, true)
            .over_limit()
            .unwrap(),
        "Context exceeds the 200k-token limit by 1.5k tokens \u{2014} run /compact or /clear to continue."
    );
    assert_eq!(
        usage(1_002_000, 1_000_000, WindowSource::ModelDefault, false)
            .over_limit()
            .unwrap(),
        "Context is 2k tokens past the 1m-token compaction window \u{2014} run /clear to reduce usage."
    );
}
