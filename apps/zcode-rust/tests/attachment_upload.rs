use serde_json::json;
use sha2::{Digest, Sha256};
use zcode_rust::domain::attachment_upload::{CHUNK_BYTES, Uploads, key};

fn begin(id: usize) -> serde_json::Value {
    json!({"connectionId":"desktop","sessionId":"session","uploadId":format!("u{id}"),"fileName":"file.txt","mime":"text/plain","totalBytes":1,"totalChunks":1,"checksum":format!("sha256:{:x}",Sha256::digest(b"a"))})
}
#[test]
fn expiry_capacity_and_owner_cleanup_preserve_other_connections() {
    let mut uploads = Uploads::default();
    for i in 0..16 {
        uploads.begin(&begin(i), 10).unwrap();
    }
    assert!(
        uploads
            .begin(&begin(16), 10)
            .unwrap_err()
            .to_string()
            .contains("tooManyUploads")
    );
    uploads.prune(300_010);
    assert!(uploads.0.is_empty());
    let mut mobile = begin(0);
    mobile["connectionId"] = "mobile".into();
    uploads.begin(&mobile, 300_011).unwrap();
    uploads.begin(&begin(0), 300_011).unwrap();
    uploads.clear_connection("desktop");
    assert_eq!(uploads.0.len(), 1);
    assert!(uploads.0.contains_key(&key(&mobile).unwrap()));
}
#[test]
fn staged_byte_budget_and_committed_buffers_are_bounded() {
    use base64::Engine as _;
    let mut uploads = Uploads::default();
    let chunk = base64::engine::general_purpose::STANDARD.encode(vec![b'a'; CHUNK_BYTES]);
    for id in 0..4 {
        let mut p = begin(id);
        p["totalBytes"] = (20 * 1024 * 1024).into();
        p["totalChunks"] = 40.into();
        uploads.begin(&p, 10).unwrap();
        for index in 0..if id == 3 { 8 } else { 40 } {
            uploads.chunk(&json!({"connectionId":"desktop","sessionId":"session","uploadId":format!("u{id}"),"chunkIndex":index,"dataBase64":chunk}), 10).unwrap();
        }
    }
    assert!(uploads.chunk(&json!({"connectionId":"desktop","sessionId":"session","uploadId":"u3","chunkIndex":8,"dataBase64":chunk}),10).unwrap_err().to_string().contains("stagingCapacityExceeded"));
    uploads.clear_connection("desktop");
    let p = begin(0);
    uploads.begin(&p, 10).unwrap();
    uploads.chunk(&json!({"connectionId":"desktop","sessionId":"session","uploadId":"u0","chunkIndex":0,"dataBase64":"YQ=="}),10).unwrap();
    uploads.validated(&key(&p).unwrap()).unwrap();
    uploads.committed(&key(&p).unwrap(), "ref".into(), 10);
    let entry = &uploads.0[&key(&p).unwrap()];
    assert!(entry.chunks.is_empty());
    assert_eq!(entry.bytes, 0);
}
