//! Prompt attachments as Node data URL artifacts and the Node resolution of
//! local path attachments.
use super::*;

fn backing(dir: &tempfile::TempDir) -> Backing {
    Backing::Artifacts(dir.path().join("artifacts"))
}

#[tokio::test]
async fn uploads_are_node_data_url_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let store = backing(&dir);
    let bytes: Vec<u8> = (0u8..=250).collect();
    let (uri, asset) = store
        .put(
            "sess_1",
            "prompt-attachment-upload-x-y",
            &[bytes[..100].to_vec(), bytes[100..].to_vec()],
            "image/png",
        )
        .await
        .unwrap();
    assert!(
        uri.starts_with("zcode-artifact://sess_1/tool-result-"),
        "{uri}"
    );
    assert!(asset.data_url && asset.total_bytes == 251);
    let name = std::path::Path::new(&asset.path)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        name.starts_with("prompt-attachment-upload-x-y-tool-result-") && name.ends_with(".txt")
    );
    // Node 的产物读取器读回同一 data URL。
    let root = dir.path().join("artifacts");
    let content = crate::node::artifacts::read(&root, &uri).unwrap();
    assert!(content.starts_with("data:image/png;base64,"));
    let found = store.attachment_of(&uri).await.unwrap().unwrap();
    assert_eq!(
        (found.total_bytes, found.media_type.as_str()),
        (251, "image/png")
    );
    assert!(found.data_url);
    for (offset, limit) in [(0, 5), (1, 7), (248, 100), (250, 1), (251, 3), (4, 0)] {
        let read = read_attachment(&found, offset, limit).await.unwrap();
        let end = (offset as usize + limit).min(251);
        let start = (offset as usize).min(251);
        assert_eq!(read, bytes[start..end], "{offset}+{limit}");
    }
    assert!(
        store
            .attachment_of("zcode-artifact://sess_1/tool-result-missing")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .attachment_of("zcode-artifact://sess_2/x")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn local_attachments_resolve_like_node() {
    let dir = tempfile::tempdir().unwrap();
    let store = backing(&dir);
    let work = dir.path().join("w");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("a.rs"), "fn a() {}\r\nfn b() {}\n").unwrap();
    std::fs::write(work.join("a.bin"), [0u8, 1, 2]).unwrap();
    std::fs::write(work.join("shot.png"), [137u8, 80, 78, 71]).unwrap();
    let path = |name: &str| work.join(name).to_string_lossy().into_owned();

    let (reference, text) = store
        .local("sess_1", 0, ("a.rs", &path("a.rs")), "text/x-rust")
        .await
        .unwrap();
    assert_eq!(reference, "a.rs");
    let node = text.node.unwrap();
    assert_eq!(
        node.part["metadata"]["preview"]["text"],
        "fn a() {}\nfn b() {}"
    );
    assert_eq!(node.part["metadata"]["preview"]["totalLines"], 2);
    assert_eq!(node.part["metadata"]["storageKind"], "inline");

    let (_, binary) = store
        .local(
            "sess_1",
            1,
            ("a.bin", &path("a.bin")),
            "application/octet-stream",
        )
        .await
        .unwrap();
    assert_eq!(
        binary.node.unwrap().part["metadata"]["storageKind"],
        "local_ref"
    );

    let (uri, image) = store
        .local("sess_1", 2, ("shot.png", &path("shot.png")), "image/png")
        .await
        .unwrap();
    assert!(uri.starts_with("zcode-artifact://sess_1/tool-result-"));
    assert!(image.data_url);
    let node = image.node.unwrap();
    assert_eq!(node.part["url"], uri.as_str());
    assert_eq!(node.part["metadata"]["sizeBytes"], 4);
    assert!(
        node.part["metadata"]["sha256"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    let artifact = std::path::Path::new(&image.path)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        artifact.starts_with("attachment-3-tool-result-"),
        "{artifact}"
    );

    let (_, missing) = store
        .local("sess_1", 3, ("gone.txt", &path("gone.txt")), "text/plain")
        .await
        .unwrap();
    let node = missing.node.unwrap();
    assert_eq!(node.part["metadata"]["errorCode"], "attachment_read_failed");
    assert_eq!(
        node.placement(),
        crate::domain::node_journal::files::Placement::Skip
    );

    std::fs::write(work.join("bad.pdf"), b"nope").unwrap();
    let (_, pdf) = store
        .local(
            "sess_1",
            4,
            ("bad.pdf", &path("bad.pdf")),
            "application/pdf",
        )
        .await
        .unwrap();
    assert_eq!(
        pdf.node.unwrap().part["metadata"]["errorCode"],
        "attachment_pdf_invalid"
    );
}
