//! Prompt attachments as Node data URL artifacts and the Node resolution of
//! local path attachments.
use super::*;

/// A noise PNG (it does not compress).
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut seed = 0x2545_f491_u32;
    let image = image::RgbImage::from_fn(width, height, |_, _| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let [a, b, c, _] = seed.to_le_bytes();
        image::Rgb([a, b, c])
    });
    let mut data = std::io::Cursor::new(vec![]);
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut data, image::ImageFormat::Png)
        .unwrap();
    data.into_inner()
}

fn backing(dir: &tempfile::TempDir) -> Backing {
    Backing {
        root: dir.path().join("artifacts"),
        cache: dir.path().join("cache"),
    }
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
    std::fs::write(work.join("shot.png"), png(1, 1)).unwrap();
    std::fs::write(work.join("broken.png"), [137u8, 80, 78, 71]).unwrap();
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
    assert_eq!(node.part["metadata"]["sizeBytes"], png(1, 1).len());
    assert_eq!(node.part["metadata"]["image"]["resized"], false);
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

    // Node：解码失败的图片是缩放失败占位，带原文件大小。
    let (_, broken) = store
        .local(
            "sess_1",
            5,
            ("broken.png", &path("broken.png")),
            "image/png",
        )
        .await
        .unwrap();
    let node = broken.node.unwrap();
    assert_eq!(
        node.part["metadata"]["errorCode"],
        "attachment_image_resize_failed"
    );
    assert_eq!(node.part["metadata"]["sizeBytes"], 4);

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

#[tokio::test]
async fn prompt_images_are_prepared_like_node() {
    let dir = tempfile::tempdir().unwrap();
    let store = backing(&dir);
    let work = dir.path().join("w");
    std::fs::create_dir_all(&work).unwrap();
    // 噪声 PNG 超过 5 MiB base64 预算：Node 转成 JPEG 并缩小。
    let large = png(1500, 1500);
    assert!(large.len() > 3 * 1024 * 1024 + 800 * 1024);
    let path = work.join("big.jpg");
    std::fs::write(&path, &large).unwrap();
    let (uri, local) = store
        .local(
            "sess_1",
            0,
            ("big.jpg", &path.to_string_lossy()),
            "image/jpeg",
        )
        .await
        .unwrap();
    let node = local.node.unwrap();
    // 本地图片：产物是准备后的字节，sizeBytes 与 sha256 是原文件的。
    assert_eq!(node.part["mime"], "image/jpeg");
    assert_eq!(local.media_type, "image/jpeg");
    assert!(local.total_bytes < large.len() as u64);
    let keys: Vec<&String> = node.part["metadata"].as_object().unwrap().keys().collect();
    assert_eq!(keys[0], "image");
    assert_eq!(node.part["metadata"]["sizeBytes"], large.len());
    assert_eq!(
        node.part["metadata"]["image"]["transformedSizeBytes"],
        local.total_bytes
    );
    assert!(uri.starts_with("zcode-artifact://sess_1/"));

    // 上传图片：产物保留原图，请求用缓存中的准备后字节。
    let (reference, upload) = store
        .put(
            "sess_1",
            "prompt-attachment-upload-a-b",
            std::slice::from_ref(&large),
            "image/png",
        )
        .await
        .unwrap();
    let (file, live) = store
        .uploaded_image(&reference, &upload, "big.png", 1)
        .await
        .unwrap();
    let live = live.expect("a prepared image");
    assert_eq!(
        (file.part["mime"].as_str(), live.media_type.as_str()),
        (Some("image/jpeg"), "image/jpeg")
    );
    assert_eq!(file.block["source"]["mimeType"], "image/png");
    assert!(
        !live.data_url
            && live
                .path
                .starts_with(&*dir.path().join("cache/prompt-images").to_string_lossy())
    );
    assert_eq!(
        std::fs::metadata(&live.path).unwrap().len(),
        live.total_bytes
    );
    assert_eq!(read_attachment(&upload, 0, 8).await.unwrap(), large[..8]);
    // 已在预算内的上传：直接用产物。
    let (small, asset) = store
        .put(
            "sess_1",
            "prompt-attachment-upload-c-d",
            &[png(2, 2)],
            "image/png",
        )
        .await
        .unwrap();
    let (_, live) = store
        .uploaded_image(&small, &asset, "s.png", 2)
        .await
        .unwrap();
    assert!(live.is_none());
    // 处理器拒绝的上传：Node 的占位，url 是整段 data URL。
    let (bad, asset) = store
        .put(
            "sess_1",
            "prompt-attachment-upload-e-f",
            &[b"nope".to_vec()],
            "image/png",
        )
        .await
        .unwrap();
    let (file, _) = store
        .uploaded_image(&bad, &asset, "x.png", 3)
        .await
        .unwrap();
    assert_eq!(
        file.part["metadata"]["errorCode"],
        "attachment_image_resize_failed"
    );
    assert!(
        file.part["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );
}

#[tokio::test]
async fn local_videos_follow_the_node_limit() {
    let dir = tempfile::tempdir().unwrap();
    let store = backing(&dir);
    let path = dir.path().join("clip.mov");
    let file = std::fs::File::create(&path).unwrap();
    // 稀疏文件：25 MiB 超过原先 20 MiB，仍在 Node 的 30 MiB 视频上限内；类型按扩展名推断。
    file.set_len(25 * 1024 * 1024).unwrap();
    let shown = path.to_string_lossy().into_owned();
    let (uri, video) = store
        .local("sess_1", 0, ("clip.mov", &shown), "video/mp4")
        .await
        .unwrap();
    assert!(uri.starts_with("zcode-artifact://"));
    assert_eq!(video.media_type, "video/quicktime");
    file.set_len(31 * 1024 * 1024).unwrap();
    let (_, large) = store
        .local("sess_1", 1, ("clip.mov", &shown), "video/mp4")
        .await
        .unwrap();
    assert_eq!(
        large.node.unwrap().part["metadata"]["storageKind"],
        "local_ref"
    );
}
