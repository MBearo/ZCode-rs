//! `ArtifactStore::find`（修复线 F18，node-store.md §4.4）：按会话与 artifact id 找回 put 写的完整引用（大小、
//! 用途、类型取自文件），Node 命名的产物也找得到；不完整的 id、别的会话、走出产物目录的位置找不到。
#![expect(
    clippy::disallowed_methods,
    reason = "测试直接写 Node 命名的产物文件（模拟 Node 进程）"
)]
use rust_cli_kernel_api::ids::{ArtifactId, SessionId};
use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactRef, ArtifactStore};

use crate::tests::TestStore;

async fn put(t: &TestStore, purpose: ArtifactPurpose, media_type: &str, body: &str) -> ArtifactRef {
    t.store
        .put(
            &SessionId::new("sess_1"),
            purpose,
            media_type.to_owned(),
            body.as_bytes().to_vec(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn find_returns_the_reference_that_put_wrote() {
    let t = TestStore::new();
    // 单段结果（text/plain）、多段结果（application/json，文件是 .json）与别的用途。
    let written = [
        put(&t, ArtifactPurpose::ToolOutput, "text/plain", "one\ntwo").await,
        put(
            &t,
            ArtifactPurpose::ToolOutput,
            "application/json",
            "[\n  {}\n]",
        )
        .await,
        put(&t, ArtifactPurpose::Checkpoint, "application/json", "{}").await,
    ];
    for artifact in written {
        let found = t.store.find(&artifact.session, &artifact.id).await.unwrap();
        assert_eq!(found.as_ref(), Some(&artifact));
        let bytes = t.store.get(&found.unwrap()).await.unwrap();
        assert_eq!(bytes.len() as u64, artifact.size);
    }
}

#[tokio::test]
async fn find_reads_node_named_artifacts_as_tool_output() {
    let t = TestStore::new();
    let id = "tool-result-0b7ea269-b8a7-4850-acf4-7830e6ba7548";
    let dir = t.dir.path().join("cli/artifacts/sess_1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("toolu_node_1-{id}.json")), "[1, 2]").unwrap();
    let found = t
        .store
        .find(&SessionId::new("sess_1"), &ArtifactId::new(id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        found,
        ArtifactRef {
            session: SessionId::new("sess_1"),
            id: ArtifactId::new(id),
            purpose: ArtifactPurpose::ToolOutput,
            media_type: "application/json".to_owned(),
            size: 6,
        }
    );
    assert_eq!(t.store.get(&found).await.unwrap(), b"[1, 2]");
}

#[tokio::test]
async fn find_does_not_match_partial_ids_other_sessions_or_outside_paths() {
    let t = TestStore::new();
    let artifact = put(&t, ArtifactPurpose::ToolOutput, "text/plain", "body").await;
    // 存储根下另放一个文件：`..` 不能走到这里。
    std::fs::write(t.dir.path().join("cli/tool_output-secret.txt"), "outside").unwrap();
    let missing = [
        ("sess_1", "tool-result"),
        ("sess_1", ""),
        ("sess_1", &artifact.id.as_str()[1..]),
        ("sess_2", artifact.id.as_str()),
        ("..", "secret"),
        (".", artifact.id.as_str()),
    ];
    for (session, id) in missing {
        let found = t
            .store
            .find(&SessionId::new(session), &ArtifactId::new(id))
            .await
            .unwrap();
        assert_eq!(found, None, "{session}/{id}");
    }
}
