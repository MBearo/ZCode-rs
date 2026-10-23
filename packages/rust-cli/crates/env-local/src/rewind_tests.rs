#![expect(clippy::disallowed_methods, reason = "测试直接准备与检查工作区文件")]

use std::fs;

use rust_cli_kernel_api::env::{EnvPath, Environment, OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::env_rewind::{
    RestoreAction, RestoreFile, RewindPlan, RewindPrepare, RewindStatus, content_hash,
};
use rust_cli_kernel_api::ids::{EnvironmentId, Generation, OperationId, OwnerEpoch, SessionId};

use crate::LocalEnvironment;

fn workspace(root: &std::path::Path) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root.to_string_lossy()),
    }
}

fn context(token: &str) -> OperationContext {
    OperationContext {
        environment: EnvironmentId::new("local"),
        generation: Generation::new("g1"),
        owner_epoch: OwnerEpoch::LOCAL,
        operation: OperationId::new(token),
        origin: Origin::User {
            session: Some(SessionId::new("sess_1")),
        },
    }
}

fn restore(path: &std::path::Path, expected: &[u8], restore: Option<&[u8]>) -> RestoreFile {
    RestoreFile {
        path: EnvPath::new(path.to_string_lossy()),
        expected: Some(content_hash(expected)),
        restore: restore.map(<[u8]>::to_vec),
    }
}

/// prepare 全部通过才登记；apply 写回 / 删除，重放返回第一次的结果；finish 之后是 Unknown。
#[tokio::test]
async fn a_prepared_rewind_restores_and_deletes_files() {
    let dir = tempfile::tempdir().unwrap();
    let edited = dir.path().join("a.txt");
    let created = dir.path().join("new.txt");
    fs::write(&edited, "after").unwrap();
    fs::write(&created, "made").unwrap();
    let env = LocalEnvironment::new(None);
    let request = RewindPrepare {
        files: vec![
            restore(&edited, b"after", Some(b"before")),
            restore(&created, b"made", None),
        ],
    };
    let plan = env
        .rewind_prepare(&workspace(dir.path()), context("op_1"), request)
        .await
        .unwrap();
    assert_eq!(plan, RewindPlan::Ready);
    let token = OperationId::new("op_1");
    assert_eq!(
        env.rewind_status(&token).await.unwrap(),
        RewindStatus::Prepared
    );
    let status = env.rewind_apply(&token).await.unwrap();
    let RewindStatus::Applied { files } = &status else {
        panic!("{status:?}")
    };
    assert_eq!(files[0].action, RestoreAction::Restore);
    assert_eq!(files[1].action, RestoreAction::Delete);
    assert_eq!(fs::read_to_string(&edited).unwrap(), "before");
    assert!(!created.exists());
    assert_eq!(env.rewind_apply(&token).await.unwrap(), status);
    env.rewind_finish(&token).await.unwrap();
    assert_eq!(
        env.rewind_status(&token).await.unwrap(),
        RewindStatus::Unknown
    );
}

/// prepare 发现冲突时不登记；prepare 之后文件又被改：apply 失败，已写的文件补偿回去。
#[tokio::test]
async fn conflicts_are_reported_and_a_late_change_is_compensated() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("1.txt");
    let second = dir.path().join("2.txt");
    fs::write(&first, "one").unwrap();
    fs::write(&second, "two").unwrap();
    let env = LocalEnvironment::new(None);
    let request = RewindPrepare {
        files: vec![restore(&first, b"not this", Some(b"x"))],
    };
    let plan = env
        .rewind_prepare(&workspace(dir.path()), context("op_1"), request)
        .await
        .unwrap();
    assert!(
        matches!(plan, RewindPlan::Conflicts { files } if files[0].current == Some(content_hash(b"one")))
    );
    assert_eq!(
        env.rewind_status(&OperationId::new("op_1")).await.unwrap(),
        RewindStatus::Unknown
    );
    let request = RewindPrepare {
        files: vec![
            restore(&first, b"one", Some(b"zero")),
            restore(&second, b"two", Some(b"zero")),
        ],
    };
    let plan = env
        .rewind_prepare(&workspace(dir.path()), context("op_2"), request)
        .await
        .unwrap();
    assert_eq!(plan, RewindPlan::Ready);
    fs::write(&second, "changed meanwhile").unwrap();
    let status = env.rewind_apply(&OperationId::new("op_2")).await.unwrap();
    assert!(matches!(&status, RewindStatus::Failed { conflicts, .. } if conflicts.len() == 1));
    assert_eq!(fs::read_to_string(&first).unwrap(), "one");
    assert_eq!(fs::read_to_string(&second).unwrap(), "changed meanwhile");
}
