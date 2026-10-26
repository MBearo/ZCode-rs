//! 压缩过的大会话冷打开的耗时（docs/specs/rust-cli/compact.md §5.1，工作线 F102 的测量）：几千条消息、三次压缩，
//! 打开会话到拿到视图、视图折成 TUI 的行各用多久。只在手动运行时跑：
//!
//! `cargo test --release -p rust-cli --test compacted_bench -- --ignored --nocapture`
//!
//! 设 `F102_BENCH_DIR=<目录>` 时会话库留在 `<目录>/home`、工作区是 `<目录>/work`（给 PTY 里量首屏的脚本用）。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "手动运行的测量：准备临时目录、打印耗时"
)]

mod compacted;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use compacted::{Keep, SESSION};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::session_api::Kernel;

const TURNS: usize = 780;
const RUNS: usize = 7;

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "手动运行的测量"]
async fn opening_a_large_compacted_session() {
    let keep = tempfile::tempdir().unwrap();
    let base = match std::env::var_os("F102_BENCH_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => std::fs::canonicalize(keep.path()).unwrap(),
    };
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let store = compacted::store(&home);
    let written = compacted::seed(
        &store,
        &work,
        TURNS,
        &[
            (200, Keep::Answer),
            (400, Keep::Result),
            (600, Keep::Answer),
        ],
    )
    .await;
    let mut open = Vec::new();
    let mut fold = Vec::new();
    let mut shown = 0;
    for _ in 0..RUNS {
        let kernel = compacted::kernel(&home, &work).await;
        let started = Instant::now();
        let handle = kernel.open_session(&SessionId::new(SESSION)).await.unwrap();
        let view = handle.view().await.unwrap();
        open.push(started.elapsed());
        shown = view.messages.len();
        let started = Instant::now();
        let transcript = rust_cli_tui::view::Transcript::from_view(view, 0);
        fold.push(started.elapsed());
        std::hint::black_box(&transcript);
        handle
            .close(rust_cli_kernel_api::session_api::EndReason::Other)
            .await;
    }
    println!(
        "F102 bench: {written} messages written, {shown} in the view; open+view median {:?}, fold median {:?}",
        median(open),
        median(fold)
    );
}
