//! 合成的通知流（remote-exec.md §20.7.2）：本机一开始就订阅，新连上的机器开流，重连的机器重开，ack 发给所在的机器。

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;

use super::*;
use crate::testing::{NoticeEnvironment, TestMachines, shell_notice};

async fn next(stream: &mut BoxStream<'static, MachineNotice>) -> MachineNotice {
    tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("a notice in time")
        .expect("the stream never ends")
}

#[tokio::test]
async fn notices_from_every_connected_machine_arrive_tagged() {
    let local = Arc::new(NoticeEnvironment::default());
    let registry = TestMachines::new(local.clone());
    let session = SessionId::new("sess_1");
    let mut stream = all_shell_notices(registry.clone(), session.clone());
    local.finish(shell_notice(1, "b0000000l"));
    let first = next(&mut stream).await;
    assert_eq!(first.machine, "local");
    assert_eq!(first.notice.seq, 1);

    // 会话开始之后才连上的机器：changes() 出项时开流。
    let remote = Arc::new(NoticeEnvironment::default());
    registry.connect("dev-a", remote.clone(), "/home/dev/proj");
    remote.finish(shell_notice(1, "b0000000r"));
    let second = next(&mut stream).await;
    assert_eq!(second.machine, "dev-a");
    assert_eq!(second.notice.task.as_str(), "b0000000r");

    // ack 发给它所在的机器。
    ack(registry.as_ref(), &second, &session).await.unwrap();
    assert_eq!(remote.acked(), vec![1]);
    assert!(local.acked().is_empty());
    ack(registry.as_ref(), &first, &session).await.unwrap();
    assert_eq!(local.acked(), vec![1]);
}

#[tokio::test]
async fn a_disconnected_machine_cannot_be_acknowledged() {
    let local = Arc::new(NoticeEnvironment::default());
    let registry = TestMachines::new(local);
    let notice = MachineNotice {
        machine: "dev-a".to_owned(),
        notice: shell_notice(3, "b0000000r"),
    };
    let error = ack(registry.as_ref(), &notice, &SessionId::new("sess_1"))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        EnvError::Io {
            message: "Machine \"dev-a\" is not connected.".to_owned()
        }
    );
}

#[tokio::test]
async fn a_machine_that_reconnects_gets_a_new_stream() {
    let local = Arc::new(crate::testing::UnusedEnvironment);
    let registry = TestMachines::new(local);
    let session = SessionId::new("sess_1");
    let mut stream = all_shell_notices(registry.clone(), session);
    let first = Arc::new(NoticeEnvironment::default());
    registry.connect("dev-a", first.clone(), "/home/dev/proj");
    first.finish(shell_notice(1, "b00000001"));
    assert_eq!(next(&mut stream).await.notice.seq, 1);
    // 断开后重连：新的执行器（新的替身）。旧连接的通知流在连接丢掉时结束（替身被丢掉、发送端关闭），结束时注册表已经
    // 变过，所以马上给这台机器重开流，新执行器上的通知照样到。
    registry.disconnect("dev-a");
    let second = Arc::new(NoticeEnvironment::default());
    registry.connect("dev-a", second.clone(), "/home/dev/proj");
    drop(first);
    second.finish(shell_notice(1, "b00000002"));
    let again = next(&mut stream).await;
    assert_eq!(again.machine, "dev-a");
    assert_eq!(again.notice.task.as_str(), "b00000002");
}

#[tokio::test]
async fn a_machine_without_notices_is_dropped_and_not_reopened() {
    // 替身不提供通知（流里是错误）：丢掉它，注册表没变就不重开（不会空转）。
    let local = Arc::new(crate::testing::UnusedEnvironment);
    let registry = TestMachines::new(local);
    let mut stream = all_shell_notices(registry, SessionId::new("sess_1"));
    let waited = tokio::time::timeout(Duration::from_millis(100), stream.next()).await;
    assert!(waited.is_err(), "nothing arrives and the stream stays open");
}
