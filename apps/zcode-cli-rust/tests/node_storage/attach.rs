//! Prompt attachments over the Node database (spec rust-m11-node-storage
//! §5.3): uploads are Node data URL artifacts, the user message carries a
//! `file` part per attachment, and a restarted runtime sends them again from
//! the stored transcript.
use super::harness;
use base64::Engine as _;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const IMAGE: &[u8] = &[137, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3];

async fn upload(
    h: &mut harness::Harness,
    id: u64,
    session: &str,
    name: &str,
    mime: &str,
    bytes: &[u8],
) -> String {
    let identity = json!({"connectionId": "cli", "sessionId": session, "uploadId": name});
    let mut begin = identity.clone();
    begin["fileName"] = name.into();
    begin["mime"] = mime.into();
    begin["totalBytes"] = bytes.len().into();
    begin["totalChunks"] = 1.into();
    begin["checksum"] = format!("sha256:{:x}", Sha256::digest(bytes)).into();
    h.request(id, "v4/attachment/begin", begin).await;
    let mut chunk = identity.clone();
    chunk["chunkIndex"] = 0.into();
    chunk["dataBase64"] = base64::engine::general_purpose::STANDARD
        .encode(bytes)
        .into();
    h.request(id + 1, "v4/attachment/chunk", chunk).await;
    let committed = h.request(id + 2, "v4/attachment/commit", identity).await;
    committed["ref"].as_str().expect("ref").to_owned()
}

fn user_with_image(request: &[Value]) -> Option<&Value> {
    request.iter().find(|m| {
        m["role"] == "user"
            && m["content"]
                .as_array()
                .is_some_and(|blocks| blocks.iter().any(|b| b["type"] != "text"))
    })
}

#[tokio::test]
async fn uploaded_attachments_are_node_artifacts_and_file_parts() {
    let mut h = harness::start(Some("attach"), None).await;
    let session = h.create("c1", "Fix it").await;
    h.settled(&session, 1).await;
    let image = upload(&mut h, 10, &session, "shot.png", "image/png", IMAGE).await;
    let notes = upload(
        &mut h,
        20,
        &session,
        "notes.txt",
        "text/plain",
        b"remember this",
    )
    .await;
    assert!(
        image.starts_with(&format!("zcode-artifact://{session}/tool-result-")),
        "{image}"
    );
    let attachments = json!([
        {"ref": image, "fileName": "shot.png", "mime": "image/png", "bytes": IMAGE.len()},
        {"ref": notes, "fileName": "notes.txt", "mime": "text/plain", "bytes": 13}]);
    let ack = h
        .command(30, json!({"commandId": "c2", "clientId": "cli", "sessionId": session,
            "type": "sendText", "issuedAt": 1, "payload": {"text": "look", "attachments": attachments}}))
        .await;
    assert_eq!(ack["status"], "accepted", "{ack}");
    let conn = h.settled(&session, 2).await;
    for _ in 0..2 {
        h.requests.recv().await.unwrap();
    }
    let live = h.requests.recv().await.unwrap();
    // Node 布局：正文后是粘贴图片；文本附件是 user 消息之后的 prompt_attachment 提醒。
    let user = user_with_image(&live).expect("image in the request");
    assert_eq!(user["content"][0], json!({"type": "text", "text": "look"}));
    assert_eq!(user["content"][1]["type"], "_zcode_attachment");
    let at = live.iter().position(|m| std::ptr::eq(m, user)).unwrap();
    assert_eq!(live[at + 1]["_zcode_source"], "prompt_attachment");
    assert!(
        live[at + 1]["content"]
            .as_str()
            .unwrap()
            .contains("Attached inline text: notes.txt\nremember this"),
        "{:#}",
        live[at + 1]
    );

    let parts: Vec<Value> = conn
        .prepare(
            "select p.data from part p join message m on m.id = p.message_id
            where m.session_id = ? and json_extract(m.data, '$.role') = 'user' order by p.id",
        )
        .unwrap()
        .query_map([&session], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|d| serde_json::from_str(&d.unwrap()).unwrap())
        .filter(|p: &Value| p["type"] == "file")
        .collect();
    assert_eq!(parts.len(), 2, "{parts:#?}");
    assert_eq!(parts[0]["url"], image.as_str());
    assert_eq!(parts[0]["metadata"]["storageKind"], "artifact");
    assert_eq!(parts[1]["metadata"]["preview"]["text"], "remember this");
    let artifact =
        zcode_cli_state::node::artifacts::read(&h.root.join("cli/artifacts"), &image).unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(IMAGE);
    assert_eq!(artifact, format!("data:image/png;base64,{encoded}"));

    // 重启后：预览从产物读取，继续对话时图片由存储的 transcript 重新发送。
    drop(conn);
    let mut h = harness::restart(&h).await;
    let read = h
        .request(
            40,
            "v4/attachment/read",
            json!({"sessionId": session, "ref": image,
            "offset": 1, "limit": 4}),
        )
        .await;
    assert_eq!(
        read["dataBase64"],
        base64::engine::general_purpose::STANDARD.encode(&IMAGE[1..5])
    );
    assert_eq!(read["totalBytes"], IMAGE.len());
    h.send_text(41, &session, "c3", "again").await;
    let conn = h.settled(&session, 3).await;
    let cold = h.requests.recv().await.unwrap();
    let user = user_with_image(&cold).expect("image after the restart");
    let block = user["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["type"] == "image")
        .unwrap();
    assert_eq!(block["dataUrl"], format!("data:image/png;base64,{encoded}"));
    assert!(
        cold.iter()
            .any(|m| m["_zcode_source"] == "prompt_attachment")
    );
    harness::dump(&h, &conn, &session);
}
