// 只有 Unix 的用例；Windows 上 `use super::*` 会成为未使用的导入（交叉 clippy 报错，F7 顺手修）。
#![cfg(unix)]

use super::*;

/// 本进程有启动时间，两次读到的相同；不存在的 PID 没有。
#[tokio::test]
async fn own_start_time_is_stable_and_missing_pids_have_none() {
    let own = process_started(std::process::id()).await;
    assert!(own.as_deref().is_some_and(|text| !text.is_empty()));
    assert_eq!(process_started(std::process::id()).await, own);
    assert_eq!(process_started(u32::MAX / 2).await, None);
}
