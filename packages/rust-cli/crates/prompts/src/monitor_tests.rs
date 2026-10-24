//! Monitor 文字的逐字检查。

use super::*;

#[test]
fn every_text_file_is_loaded() {
    for (file, text) in ALL {
        assert!(!text().is_empty(), "{file}");
        assert!(!text().ends_with('\n'), "{file}");
    }
}

#[test]
fn started_and_expiry_texts_match_the_recorded_runs() {
    assert_eq!(
        started("bnvso9csm", "3s"),
        "Monitor started (task bnvso9csm, expires in 3s unless the source ends first; you get one notice at expiry — re-arm if you still need the watch). You will be notified on each event. Keep working — do not poll or sleep. Events may arrive while you are waiting for the user — an event is not their reply."
    );
    assert_eq!(
        expired("3s", 1),
        "[Monitor expired after 3s with 1 event delivered. Re-arm it if you still need the watch.]"
    );
    assert_eq!(
        expired("5m", 4),
        "[Monitor expired after 5m with 4 events delivered. Re-arm it if you still need the watch.]"
    );
    assert_eq!(
        expired("1m", 0),
        "[Monitor expired after 1m with no events delivered. Re-arm it if you still need the watch — and widen the filter if silence was unexpected.]"
    );
}

#[test]
fn summaries_follow_the_ending() {
    let summary = |ending, code| terminal_summary("three lines", ending, code);
    assert_eq!(
        summary(Ending::Completed { output: true }, Some(0)),
        "Monitor \"three lines\" stream ended"
    );
    assert_eq!(
        summary(Ending::Completed { output: false }, Some(0)),
        "Monitor \"three lines\" ended without producing output (exit 0)"
    );
    assert_eq!(
        summary(Ending::Failed, Some(2)),
        "Monitor \"three lines\" script failed (exit 2)"
    );
    assert_eq!(
        summary(Ending::Failed, None),
        "Monitor \"three lines\" script failed"
    );
    assert_eq!(
        summary(Ending::Stopped, None),
        "Monitor \"three lines\" stopped"
    );
    assert_eq!(event_summary("ready line"), "Monitor event: \"ready line\"");
    assert_eq!(escape("two <b> & c"), "two &lt;b&gt; &amp; c");
}

#[test]
fn rate_and_socket_texts() {
    assert_eq!(
        suppressed(3),
        "[3 events suppressed — output rate too high. Consider using TaskStop to restart this monitor with a more selective filter.]"
    );
    assert_eq!(
        too_much_output(12, 31),
        "[Monitor stopped — too much output (12 events suppressed over 31s). Restart with a more selective source.]"
    );
    assert_eq!(
        ws_private("10.0.0.1"),
        "Monitor cannot open a WebSocket to 10.0.0.1: the address is in a private, link-local, or cloud-metadata range."
    );
    assert_eq!(
        ws_ask("wss://e.example.com/s", &["v1".to_owned(), "v2".to_owned()]),
        "Monitor will open a WebSocket to wss://e.example.com/s (subprotocols: v1, v2)"
    );
    assert_eq!(
        ws_ask("ws://e.example.com", &[]),
        "Monitor will open a WebSocket to ws://e.example.com"
    );
    assert_eq!(
        ws_closed(1006, "Failed to connect"),
        "[WebSocket closed: 1006 Failed to connect]"
    );
    assert_eq!(ws_closed(1000, ""), "[WebSocket closed: 1000]");
    assert_eq!(ws_error("boom"), "[WebSocket error: boom]");
    assert_eq!(ws_rejected(403), "[WebSocket upgrade rejected: HTTP 403]");
    assert_eq!(
        ws_dropped(2_000_000),
        "[Dropped 2000000-byte frame (exceeds 1048576); closing]"
    );
}

#[test]
fn bash_and_cron_texts() {
    assert_eq!(
        bash_blocked("sleep 30 followed by: echo done"),
        "Blocked: sleep 30 followed by: echo done. To wait for a condition, use Monitor with an until-loop (e.g. `until <check>; do sleep 2; done`). To wait for a command you started, use run_in_background: true. Do not chain shorter sleeps to work around this block."
    );
    assert_eq!(
        bash_sentence(),
        " Foreground `sleep` is blocked; use Monitor with an until-loop to wait on a condition."
    );
    assert!(cron_create_section().starts_with("## Not for live watching\n\nCronCreate re-runs"));
    assert!(cron_create_section().ends_with("cron polls on a schedule."));
}
