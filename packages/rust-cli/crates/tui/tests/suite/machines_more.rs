//! 远端执行 F98（docs/specs/rust-cli/remote-exec.md §25.4.1、§25.4.2）：`/machines` 的立即重试（`r` 与对重连中的机器
//! `/machines connect`）与第一次连接的分步，真实的事件循环与机器服务的替身（`machines.rs` 的 `FakeMachines`）。旧代码没有
//! `r`，`/machines connect` 对重连中的机器回“已经连着”，连接中只写 `connecting`，这些测试都失败。
use crossterm::event::KeyCode;
use rust_cli_tui::machines::{ConnectStep, MachineStatus};

use super::machines::{FakeMachines, idle, ready, row, setup_with, waiting};
use super::support::with_ui;

#[tokio::test(flavor = "multi_thread")]
async fn a_machine_waiting_to_reconnect_is_retried_at_once() {
    for (lang, hint, retried) in [
        ("en_US.UTF-8", "r to retry now", "Retrying gpu-1 now."),
        ("zh_CN.UTF-8", "r 立即重试", "马上重试 gpu-1。"),
    ] {
        let fake = FakeMachines::with(vec![waiting("gpu-1", 60_000)]);
        let calls = fake.clone();
        with_ui(idle(), setup_with(Some(fake), lang), |mut ui| async move {
            ready(&mut ui).await;
            ui.submit("/machines");
            ui.wait_for(hint).await;
            ui.key(KeyCode::Char('r'));
            ui.wait_for(retried).await;
            // 替身马上连上：列表与状态行都跟着变，提示行不再有“立即重试”。
            let screen = ui
                .wait("the machine to be ready", |s| !s.contains(hint))
                .await;
            assert!(screen.contains("● gpu-1"), "{screen}");
        })
        .await;
        assert_eq!(*calls.calls.lock().unwrap(), ["retry gpu-1"]);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn connecting_a_reconnecting_machine_retries_it_now() {
    let fake = FakeMachines::with(vec![waiting("gpu-1", 60_000)]);
    let calls = fake.clone();
    with_ui(
        idle(),
        setup_with(Some(fake), "en_US.UTF-8"),
        |mut ui| async move {
            ready(&mut ui).await;
            ui.submit("/machines connect gpu-1");
            ui.wait_for("Retrying gpu-1 now.").await;
            ui.wait_for("● local  ● gpu-1 12ms").await;
        },
    )
    .await;
    assert_eq!(*calls.calls.lock().unwrap(), ["retry gpu-1"]);
}

/// F98 上传与握手；F100（remote-exec.md §26.6）下载在上传之前。
#[tokio::test(flavor = "multi_thread")]
async fn the_first_connection_shows_the_upload_and_the_handshake() {
    let fake = FakeMachines::with(vec![row(
        "dev-a",
        MachineStatus::Connecting(ConnectStep::Downloading),
    )]);
    let control = fake.clone();
    with_ui(
        idle(),
        setup_with(Some(fake), "zh_CN.UTF-8"),
        |mut ui| async move {
            ui.wait_for("● local  ○ dev-a 连接中 · 下载执行器").await;
            control.set(
                "dev-a",
                MachineStatus::Connecting(ConnectStep::Uploading { bytes: 93_323_264 }),
            );
            ui.wait_for("● local  ○ dev-a 连接中 · 上传执行器（89 MB）")
                .await;
            control.set("dev-a", MachineStatus::Connecting(ConnectStep::Handshaking));
            ui.wait_for("● local  ○ dev-a 连接中 · 握手").await;
            control.set("dev-a", MachineStatus::Ready);
            ui.wait_for("● local  ● dev-a 12ms").await;
        },
    )
    .await;
}
