//! `Checkpoints`：prepare 全部通过才登记；apply 写回 / 删除、按 token 重放；冲突与 prepare 之后的改动补偿回去。

use rust_cli_env_local::LocalEnvironment;
use rust_cli_kernel_api::env_rewind::{
    RestoreAction, RestoreFile, RewindPlan, RewindPrepare, RewindStatus, content_hash,
};
use rust_cli_kernel_api::ids::OperationId;

use crate::contract;
use crate::support::{Kind, context, env_path, machine, workspace};

fn restore(path: &std::path::Path, expected: &[u8], restore: Option<&[u8]>) -> RestoreFile {
    RestoreFile {
        path: env_path(path),
        expected: Some(content_hash(expected)),
        restore: restore.map(<[u8]>::to_vec),
    }
}

async fn a_prepared_rewind_restores_and_deletes_files(kind: Kind) {
    let dir = tempfile::tempdir().unwrap();
    let edited = dir.path().join("a.txt");
    let created = dir.path().join("new.txt");
    std::fs::write(&edited, "after").unwrap();
    std::fs::write(&created, "made").unwrap();
    let env = machine(kind, LocalEnvironment::new(None)).await;
    let checkpoints = env.checkpoints();
    let request = RewindPrepare {
        files: vec![
            restore(&edited, b"after", Some(b"before")),
            restore(&created, b"made", None),
        ],
    };
    let plan = checkpoints
        .rewind_prepare(&workspace(dir.path()), context("op_1"), request)
        .await
        .unwrap();
    assert_eq!(plan, RewindPlan::Ready);
    let token = OperationId::new("op_1");
    assert_eq!(
        checkpoints.rewind_status(&token).await.unwrap(),
        RewindStatus::Prepared
    );
    let status = checkpoints.rewind_apply(&token).await.unwrap();
    let RewindStatus::Applied { files } = &status else {
        panic!("{status:?}")
    };
    assert_eq!(files[0].action, RestoreAction::Restore);
    assert_eq!(files[1].action, RestoreAction::Delete);
    assert_eq!(std::fs::read_to_string(&edited).unwrap(), "before");
    assert!(!created.exists());
    assert_eq!(checkpoints.rewind_apply(&token).await.unwrap(), status);
    checkpoints.rewind_finish(&token).await.unwrap();
    assert_eq!(
        checkpoints.rewind_status(&token).await.unwrap(),
        RewindStatus::Unknown
    );
}

async fn conflicts_are_reported_and_a_late_change_is_compensated(kind: Kind) {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("1.txt");
    let second = dir.path().join("2.txt");
    std::fs::write(&first, "one").unwrap();
    std::fs::write(&second, "two").unwrap();
    let env = machine(kind, LocalEnvironment::new(None)).await;
    let checkpoints = env.checkpoints();
    let request = RewindPrepare {
        files: vec![restore(&first, b"not this", Some(b"x"))],
    };
    let plan = checkpoints
        .rewind_prepare(&workspace(dir.path()), context("op_1"), request)
        .await
        .unwrap();
    assert!(
        matches!(plan, RewindPlan::Conflicts { files } if files[0].current == Some(content_hash(b"one")))
    );
    assert_eq!(
        checkpoints
            .rewind_status(&OperationId::new("op_1"))
            .await
            .unwrap(),
        RewindStatus::Unknown
    );
    let request = RewindPrepare {
        files: vec![
            restore(&first, b"one", Some(b"zero")),
            restore(&second, b"two", Some(b"zero")),
        ],
    };
    let plan = checkpoints
        .rewind_prepare(&workspace(dir.path()), context("op_2"), request)
        .await
        .unwrap();
    assert_eq!(plan, RewindPlan::Ready);
    std::fs::write(&second, "changed meanwhile").unwrap();
    let status = checkpoints
        .rewind_apply(&OperationId::new("op_2"))
        .await
        .unwrap();
    assert!(matches!(&status, RewindStatus::Failed { conflicts, .. } if conflicts.len() == 1));
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "one");
    assert_eq!(
        std::fs::read_to_string(&second).unwrap(),
        "changed meanwhile"
    );
}

contract!(
    a_prepared_rewind_restores_and_deletes_files,
    conflicts_are_reported_and_a_late_change_is_compensated,
);
