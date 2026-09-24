use super::*;
use crate::http::{Get, Http, Response};
use crate::ports::Ports;
use sha2::Digest;
use std::io::Write;
use zip::write::SimpleFileOptions;

pub(crate) fn archive(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(vec![]));
    for (name, body) in entries {
        if name.ends_with('/') {
            writer
                .add_directory(*name, SimpleFileOptions::default())
                .unwrap();
        } else if let Some(link) = name.strip_prefix("link:") {
            writer
                .add_symlink(link, *body, SimpleFileOptions::default())
                .unwrap();
        } else {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(body.as_bytes()).unwrap();
        }
    }
    writer.finish().unwrap().into_inner()
}

fn extract_bytes(bytes: &[u8]) -> (tempfile::TempDir, Result<Vec<String>>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.zip");
    std::fs::write(&path, bytes).unwrap();
    let result = extract(&path, &dir.path().join("out"), &CancellationToken::new());
    (dir, result)
}

#[test]
fn archives_extract_with_yauzl_name_rules() {
    let bytes = archive(&[
        ("root/", ""),
        ("root/.zcode-plugin/plugin.json", r#"{"name":"p"}"#),
        ("root/skills/a/SKILL.md", "x"),
    ]);
    let (dir, result) = extract_bytes(&bytes);
    assert_eq!(result.unwrap(), vec!["root"]);
    let out = dir.path().join("out/root");
    assert_eq!(
        std::fs::read_to_string(out.join("skills/a/SKILL.md")).unwrap(),
        "x"
    );
    let error =
        |entries: &[(&str, &str)]| extract_bytes(&archive(entries)).1.unwrap_err().to_string();
    assert_eq!(error(&[("../x", "")]), "invalid relative path: ../x");
    assert_eq!(error(&[("a\\..\\b", "")]), "invalid relative path: a/../b");
    assert_eq!(error(&[("/abs", "")]), "absolute path: /abs");
    assert_eq!(error(&[("a/./b", "")]), "Unsafe plugin zip path: a/./b");
    assert_eq!(
        error(&[("link:l", "target")]),
        "Plugin zip entry symlinks are not supported: l"
    );
    assert_eq!(normalize("a/b//").unwrap(), "a/b");
    assert!(normalize("").is_err() && normalize("C:x").is_err());
}

struct Serve(Vec<u8>);

#[async_trait::async_trait]
impl Http for Serve {
    async fn get(&self, request: Get<'_>, _: &CancellationToken) -> Result<Response> {
        if request.url.ends_with("/moved") {
            return Ok(Response {
                status: 302,
                status_text: "Found".into(),
                location: Some("/p.zip".into()),
                body: vec![],
            });
        }
        if request.url.ends_with("/missing") {
            return Ok(Response {
                status: 404,
                status_text: "Not Found".into(),
                location: None,
                body: vec![],
            });
        }
        Ok(Response {
            status: 200,
            status_text: "OK".into(),
            location: None,
            body: self.0.clone(),
        })
    }
}

#[tokio::test]
async fn zip_sources_verify_the_digest_and_pick_the_plugin_root() {
    let bytes = archive(&[
        ("pkg/.claude-plugin/plugin.json", r#"{"name":"p"}"#),
        ("pkg/README.md", "r"),
    ]);
    let sha = format!("{:x}", sha2::Sha256::digest(&bytes));
    let http = Serve(bytes);
    let dir = tempfile::tempdir().unwrap();
    let git = crate::git::Git {
        binary: "git".into(),
        env: vec![],
    };
    let cancel = CancellationToken::new();
    let ports = Ports {
        storage: dir.path(),
        cancel: &cancel,
        http: &http,
        git: &git,
    };
    let source = |url: &str, sha: &str| {
        serde_json::json!({"source":"url","type":"zip","url":url,"sha256":sha})
            .as_object()
            .unwrap()
            .clone()
    };
    let url = "https://zip.test/moved";
    let resolved =
        crate::zip_source::resolve_plugin(&ports, &source(url, &sha.to_uppercase()), url)
            .await
            .unwrap();
    assert!(
        resolved.path.ends_with("extract/pkg"),
        "single top-level directory is stripped"
    );
    crate::store::cleanup(resolved.cleanup.as_deref()).await;
    let wrong = "0".repeat(64);
    let error = crate::zip_source::resolve_plugin(&ports, &source(url, &wrong), url)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("Plugin zip sha256 mismatch: expected=000")
    );
    let plain = "http://zip.test/p.zip";
    let error = crate::zip_source::resolve_plugin(&ports, &source(plain, &sha), plain)
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Plugin zip source URL must be HTTPS: http://zip.test/p.zip"
    );
    let missing = "https://zip.test/missing";
    let error = crate::zip_source::resolve_plugin(&ports, &source(missing, &sha), missing)
        .await
        .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<crate::zip_source::DownloadFailure>()
            .map(|f| f.status),
        Some(404)
    );
    assert!(crate::zip_source::is_zip(Some(
        &serde_json::json!({"source":"url","type":"zip","url":"u","sha256":"s"})
    )));
}
