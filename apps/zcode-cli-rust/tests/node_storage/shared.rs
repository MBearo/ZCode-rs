//! A shared context import over the Node database (spec
//! rust-m11-node-storage §5.6): Node's import bundle, the context attached
//! by the first input, and the import a resumed session reads back.
use super::harness;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zcode_cli_state::node::{entries, resume, sessions};

const MARKDOWN: &str = "# Shared\n\nUse the parser in a.ts.";

#[tokio::test]
async fn a_shared_context_import_is_nodes_bundle_and_attaches_on_first_input() {
    let mut h = harness::start(Some("shared"), None).await;
    let workspace = h.workspace.clone();
    let session = "sess_shared".to_owned();
    let digest = |text: &str| format!("{:x}", Sha256::digest(text.as_bytes()));
    let created = h
        .request(
            1,
            "session/create",
            json!({"sessionId": session, "workspace": {"workspacePath": workspace,
                "workspaceKey": workspace},
            "importedHistory": {"source": "sharedContext", "title": "Shared notes",
                "createdAt": 1_700_000_000_000u64, "markdown": MARKDOWN,
                "provenance": {"shareId": "share_1", "contextId": "ctx_1",
                    "shareUrl": "https://example.com/cn/share/abc", "status": "pending",
                    "projectionSha256": digest("p"), "artifactSetSha256": digest("a"),
                    "formatterVersion": 1, "markdownSha256": digest(MARKDOWN),
                    "installedArtifacts": []}}}),
        )
        .await;
    assert_eq!(
        created["session"]["sessionId"],
        session.as_str(),
        "{created}"
    );
    let ack = h
        .command(
            2,
            json!({"commandId": "c1", "clientId": "cli", "sessionId": session,
            "type": "sendText", "issuedAt": 1, "payload": {"text": "go on",
                "context_refs": [{"kind": "shared_context_import", "context_id": "ctx_1"}]}}),
        )
        .await;
    assert_eq!(ack["status"], "accepted", "{ack}");
    let conn = h.settled(&session, 1).await;
    let row = sessions::get(&conn, &session).unwrap().unwrap();
    assert_eq!(
        (row.title.as_str(), row.title_source.as_str()),
        ("Shared notes", "custom")
    );
    let entry = &entries::list(&conn, &session, Some("v4/shared_context_import")).unwrap()[0];
    assert_eq!(
        entry.id,
        format!("v4_shared_context_import:{session}:share_1")
    );
    assert_eq!(entry.data["status"], "attached");
    assert!(
        entry.data["attachedMessageId"]
            .as_str()
            .unwrap()
            .starts_with("msg_")
    );
    let context: Value = conn
        .query_row(
            "select data from message where id = ?",
            [format!("msg_{session}_shared_context")],
            |r| r.get::<_, String>(0),
        )
        .map(|d| serde_json::from_str(&d).unwrap())
        .unwrap();
    assert_eq!(context["metadata"]["sharedContextStatus"], "attached");
    assert_eq!(context["visibility"], "model-only");

    let resumed = resume::resume(&conn, &session, &|_| None, None)
        .unwrap()
        .unwrap();
    let shared = resumed.shared.expect("import read back");
    assert_eq!(shared.markdown.as_deref(), Some(MARKDOWN));
    assert_eq!(shared.provenance.context_id.as_deref(), Some("ctx_1"));
    assert!(resumed.history.messages.iter().any(|m| {
        m["content"]
            .as_str()
            .is_some_and(|c| c.contains("Use the parser"))
    }));
    // 冷读取的模型上下文与首轮请求一致（分享上下文在输入之前）。
    h.requests.recv().await.unwrap();
    let live: Vec<Value> = h
        .requests
        .recv()
        .await
        .unwrap()
        .into_iter()
        .filter(|m| m["role"] != "system")
        .filter(|m| {
            !m["content"]
                .as_str()
                .is_some_and(|c| c.starts_with("<system-reminder>"))
        })
        .collect();
    let cold = &resumed.history.messages;
    assert_eq!(cold[..live.len()], live[..], "{cold:#?}");
    harness::dump(&h, &conn, &session);
}
