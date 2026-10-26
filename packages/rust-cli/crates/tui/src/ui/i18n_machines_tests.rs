//! 远端执行 F98（docs/specs/rust-cli/remote-exec.md §25.4.4，Docker 试用 L7）：连接错误的原因去掉内部的错误链，认得的几种
//! 写成人话；其余留下最里面的原文。
use super::*;

#[test]
fn internal_chains_are_peeled_and_known_reasons_are_said_plainly() {
    let refused = "cannot reach the executor: link io error: executor exited (exit status: 255): ssh: connect to host 127.0.0.1 port 51110: Connection refused";
    assert_eq!(
        explain(Lang::Zh, refused),
        "ssh 连不上 127.0.0.1:51110（Connection refused）"
    );
    assert_eq!(
        explain(Lang::En, refused),
        "ssh can't reach 127.0.0.1:51110 (Connection refused)"
    );
    assert_eq!(
        explain(
            Lang::Zh,
            "cannot reach the executor: link io error: executor exited (exit status: 255): ssh: Could not resolve hostname dev-b: nodename nor servname provided, or not known"
        ),
        "ssh 找不到主机 dev-b（nodename nor servname provided, or not known）"
    );
    assert_eq!(
        explain(
            Lang::Zh,
            "the executor connection was lost: Connection closed by 10.0.0.2 port 22"
        ),
        "10.0.0.2:22 关掉了 ssh 连接"
    );
    assert_eq!(
        explain(Lang::Zh, "no frame from the executor for 30000 ms"),
        "执行器 30000 ms 没有回应"
    );
    assert_eq!(
        explain(
            Lang::Zh,
            "cannot reach the executor: the executor closed the connection"
        ),
        "执行器关掉了连接"
    );
    // 认不出的：留下最里面一行的原文（ssh 的输出）。
    assert_eq!(
        explain(
            Lang::Zh,
            "link io error: executor exited (exit status: 255): Warning: something\nPermission denied (publickey)."
        ),
        "Permission denied (publickey)."
    );
    assert_eq!(
        explain(Lang::En, "connection refused"),
        "connection refused"
    );
}

#[test]
fn connection_failures_use_the_plain_reason() {
    let failure = MachineFailure::Unreachable(
        "executor exited (exit status: 255): ssh: connect to host dev-a port 22: Operation timed out"
            .to_owned(),
    );
    assert_eq!(
        failure_text(Lang::Zh, "dev-a", &failure),
        "无法经 SSH 连到 dev-a：ssh 连不上 dev-a:22（Operation timed out）"
    );
    assert_eq!(
        failure_text(
            Lang::En,
            "dev-a",
            &MachineFailure::Lost(
                "the executor connection was lost: no frame from the executor for 30000 ms"
                    .to_owned()
            )
        ),
        "Lost the connection to dev-a: no reply from the executor for 30000 ms"
    );
}

#[test]
fn the_mcp_reason_for_a_missing_machine_is_translated() {
    assert_eq!(
        mcp_reason(Lang::Zh, "Machine \"dev-a\" is not connected."),
        "机器 dev-a 没有连上。"
    );
    assert_eq!(
        mcp_reason(Lang::En, "Machine \"dev-a\" is not connected."),
        "Machine \"dev-a\" is not connected."
    );
    assert_eq!(mcp_reason(Lang::Zh, "spawn failed"), "spawn failed");
}

#[test]
fn upload_sizes_round_to_whole_megabytes() {
    assert_eq!(
        connect_step(Lang::En, ConnectStep::Uploading { bytes: 93_323_264 }).as_deref(),
        Some(" · uploading the executor (89 MB)")
    );
    assert_eq!(
        connect_step(Lang::Zh, ConnectStep::Uploading { bytes: 10 }).as_deref(),
        Some(" · 上传执行器（1 MB）")
    );
    assert_eq!(connect_step(Lang::Zh, ConnectStep::Checking), None);
}

/// F100（remote-exec.md §26.6）：第一次连接的“下载执行器”一步与三种新的失败。
#[test]
fn downloads_musl_and_old_glibc_are_said_plainly() {
    assert_eq!(
        connect_step(Lang::En, ConnectStep::Downloading).as_deref(),
        Some(" · downloading the executor")
    );
    assert_eq!(
        connect_step(Lang::Zh, ConnectStep::Downloading).as_deref(),
        Some(" · 下载执行器")
    );
    let download = MachineFailure::DownloadFailed {
        version: "0.1.61".to_owned(),
        target: "aarch64-unknown-linux-gnu".to_owned(),
        reason: "GET https://registry.example.com/x: HTTP 404".to_owned(),
    };
    assert_eq!(
        failure_text(Lang::En, "dev-a", &download),
        "Couldn't download the zcode 0.1.61 executor (aarch64-unknown-linux-gnu) to upload to dev-a: GET https://registry.example.com/x: HTTP 404. No compatible executor is installed there either."
    );
    assert_eq!(
        failure_text(Lang::Zh, "dev-a", &download),
        "无法下载要上传到 dev-a 的 zcode 0.1.61 执行器（aarch64-unknown-linux-gnu）：GET https://registry.example.com/x: HTTP 404。那台机器上也没有兼容的执行器。"
    );
    assert_eq!(
        failure_text(Lang::En, "dev-a", &MachineFailure::Musl),
        "dev-a uses musl libc (for example Alpine), and there's no zcode executor package for musl. Use a glibc-based Linux such as Debian or Ubuntu."
    );
    assert_eq!(
        failure_text(Lang::Zh, "dev-a", &MachineFailure::Musl),
        "dev-a 用的是 musl libc（例如 Alpine），没有对应的 zcode 执行器包。请换用基于 glibc 的 Linux（例如 Debian、Ubuntu）。"
    );
    let old = |glibc: Option<&str>| MachineFailure::OldGlibc {
        glibc: glibc.map(str::to_owned),
        needs: "GLIBC_2.34".to_owned(),
    };
    assert_eq!(
        failure_text(Lang::En, "dev-a", &old(Some("2.31"))),
        "The zcode executor can't run on dev-a: it needs GLIBC_2.34, but dev-a has glibc 2.31. Upgrade that machine's system or use a newer Linux."
    );
    assert_eq!(
        failure_text(Lang::Zh, "dev-a", &old(Some("2.31"))),
        "zcode 执行器在 dev-a 上跑不起来：它要 GLIBC_2.34，dev-a 的 glibc 是 2.31。请升级那台机器的系统或换用更新的 Linux。"
    );
    assert!(
        failure_text(Lang::En, "dev-a", &old(None)).contains("but dev-a's glibc is older."),
        "{}",
        failure_text(Lang::En, "dev-a", &old(None))
    );
    assert!(failure_text(Lang::Zh, "dev-a", &old(None)).contains("dev-a 的 glibc 更旧。"));
}
