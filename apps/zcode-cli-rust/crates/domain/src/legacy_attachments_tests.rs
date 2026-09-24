use super::*;
use serde_json::json;

fn mapped(item: Value) -> Option<Mapped> {
    map(&item)
}

#[test]
fn maps_host_attachments_like_node() {
    let png = general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nrest");
    assert_eq!(
        mapped(json!({"kind":"image","filename":"a\nb.png","mimeType":"image/*","dataBase64":png})),
        Some(Mapped {
            file_name: "ab.png".into(),
            mime: "image/png".into(),
            source: Source::Bytes(b"\x89PNG\r\n\x1a\nrest".to_vec()),
        })
    );
    // PDF 由 MIME 判定，优先于 kind；本地路径优先于内联数据。
    assert_eq!(
        mapped(
            json!({"kind":"file","filename":"r.pdf","mimeType":"Application/PDF; x=1","localPath":"/tmp/r.pdf","dataBase64":"eA=="})
        ),
        Some(Mapped {
            file_name: "r.pdf".into(),
            mime: "application/pdf".into(),
            source: Source::Path("/tmp/r.pdf".into()),
        })
    );
    assert_eq!(
        mapped(
            json!({"kind":"file","filename":"n.txt","mimeType":"text/plain; charset=utf-8","textContent":"hi"})
        ),
        Some(Mapped {
            file_name: "n.txt".into(),
            mime: "text/plain".into(),
            source: Source::Bytes(b"hi".to_vec()),
        })
    );
    assert_eq!(
        mapped(
            json!({"kind":"audio","filename":"v.m4a","mimeType":"audio/mp4","dataBase64":"aGk"})
        ),
        Some(Mapped {
            file_name: "v.m4a".into(),
            mime: "application/octet-stream".into(),
            source: Source::Bytes(b"hi".to_vec()),
        })
    );
    let video = mapped(json!({"kind":"video","dataBase64":"aGk="})).unwrap();
    assert_eq!(
        (video.file_name.as_str(), video.mime.as_str()),
        ("attachment", "video/mp4")
    );
    let binary =
        mapped(json!({"kind":"file","filename":"b","dataBase64":"aGk=","sizeBytes":2})).unwrap();
    assert_eq!(binary.mime, "application/octet-stream");
}

#[test]
fn drops_what_node_drops() {
    // 超过 64 KiB 的内联文件、无内容、未知 kind、无法识别的图片、非法 base64。
    for item in [
        json!({"kind":"file","filename":"big","dataBase64":"aGk=","sizeBytes":65_537}),
        json!({"kind":"file","filename":"none"}),
        json!({"kind":"image","filename":"x","dataBase64":""}),
        json!({"kind":"url","filename":"x","localPath":"/x"}),
        json!({"filename":"x","localPath":"/x"}),
        json!({"kind":"image","filename":"x.bin","mimeType":"image/*","dataBase64":"aGk="}),
        json!({"kind":"pdf","filename":"x.pdf","dataBase64":"!!"}),
    ] {
        assert_eq!(map(&item), None, "{item}");
    }
}
