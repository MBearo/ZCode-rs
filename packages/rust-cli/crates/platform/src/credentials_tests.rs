//! 凭据加解密与读取（platform.md §5.1）：与 Node 的 `credential-cipher.ts` 逐字节兼容。

use super::*;
use crate::test_support::{read_text, write};

fn secret(explicit: Option<&str>) -> CredentialSecret {
    CredentialSecret {
        explicit: explicit.map(RedactedString::new),
        platform: "linux".to_owned(),
        home: "/home/dev".to_owned(),
        username: "dev".to_owned(),
    }
}

/// Node 用同一 secret、IV 01..0c 加密 `sk-fixture-国` 的结果（`crypto.createCipheriv("aes-256-gcm")`）。
const NODE_CIPHERTEXT: &str = "enc:v1:AQIDBAUGBwgJCgsM.ugbwcGz6yNOO7MTsaggOWA.TSZzRstDM_2VG1gJJ9g";

#[test]
fn ciphertexts_match_node() {
    let cipher = Cipher::new(&secret(None));
    assert_eq!(cipher.decrypt(NODE_CIPHERTEXT).unwrap(), "sk-fixture-国");
    let iv = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
    assert_eq!(
        cipher.encrypt_with_iv("sk-fixture-国", iv).unwrap(),
        NODE_CIPHERTEXT
    );
    let other_iv = cipher.encrypt_with_iv("hello", [7; 12]).unwrap();
    assert!(other_iv.starts_with("enc:v1:") && !other_iv.contains('='));
    assert_eq!(cipher.decrypt(&other_iv).unwrap(), "hello");
}

#[test]
fn decrypt_errors_match_node() {
    let cipher = Cipher::new(&secret(None));
    assert_eq!(cipher.decrypt("plain").unwrap(), "plain");
    assert_eq!(
        cipher.decrypt("enc:v1:a.b"),
        Err(CredentialError::Decrypt("密文格式非法"))
    );
    assert_eq!(
        cipher.decrypt("enc:v1:AAAA.AAAA.AAAA"),
        Err(CredentialError::Decrypt("IV 长度非法"))
    );
    assert_eq!(
        cipher.decrypt("enc:v1:AQIDBAUGBwgJCgsM.AAAA.AAAA"),
        Err(CredentialError::Decrypt("AuthTag 长度非法"))
    );
    let other = Cipher::new(&secret(Some("other")));
    assert_eq!(
        other.decrypt(NODE_CIPHERTEXT),
        Err(CredentialError::Decrypt("密钥不匹配或密文已损坏"))
    );
    assert_eq!(
        CredentialError::Decrypt("密文格式非法").to_string(),
        "凭据解密失败：密文格式非法"
    );
}

#[test]
fn the_explicit_secret_wins_and_is_trimmed() {
    assert_eq!(secret(Some(" x ")).derive(), "x");
    assert_eq!(
        secret(Some("  ")).derive(),
        "zcode-credential-fallback:linux:/home/dev:dev"
    );
    assert!(
        ["darwin", "linux", "win32"].contains(&crate::node_platform())
            || !crate::node_platform().is_empty()
    );
}

fn storage(dir: &std::path::Path) -> StoragePath {
    rust_cli_paths::StoragePaths::resolve(&Default::default(), dir, dir).credentials()
}

#[tokio::test]
async fn records_are_read_from_disk_every_time() {
    let dir = tempfile::tempdir().unwrap();
    let store = CredentialStore::new(storage(dir.path()), &secret(None));
    assert_eq!(store.read().await.unwrap().get("a").unwrap(), None);
    let file = store.file().as_path().to_path_buf();
    write(
        &file,
        format!(r#"{{"a":"{NODE_CIPHERTEXT}","legacy":"plain","blank":"  ","bad":"enc:v1:x"}}"#),
    )
    .await;
    let credentials = store.read().await.unwrap();
    assert_eq!(credentials.present("a").unwrap().expose(), "sk-fixture-国");
    assert_eq!(credentials.present("legacy").unwrap().expose(), "plain");
    assert!(credentials.present("blank").is_none());
    // 坏密文按“没有”处理，不当作已登录。
    assert!(credentials.present("bad").is_none());
    assert!(credentials.get("bad").is_err());

    write(&file, r#"{"a": 1}"#).await;
    assert!(matches!(
        store.read().await,
        Err(CredentialError::Corrupt(_))
    ));
}

#[tokio::test]
async fn writes_are_encrypted_pretty_printed_and_private() {
    let dir = tempfile::tempdir().unwrap();
    let store = CredentialStore::new(storage(dir.path()), &secret(None));
    let file = store.file().as_path().to_path_buf();
    write(
        &file,
        format!(r#"{{"keep":"{NODE_CIPHERTEXT}","drop":"x"}}"#),
    )
    .await;
    store
        .mutate(|credentials| {
            credentials.set("oauth:zai:access_token", "token-1")?;
            credentials.remove("drop");
            Ok(())
        })
        .await
        .unwrap();
    let text = read_text(&file).await.unwrap();
    // Node `JSON.stringify(value, null, 2) + "\n"`：键序保持，新键在末尾。
    let record: serde_json::Map<String, Value> = serde_json::from_str(&text).unwrap();
    let keys: Vec<&String> = record.keys().collect();
    assert_eq!(keys, ["keep", "oauth:zai:access_token"]);
    let encrypted = record["oauth:zai:access_token"].as_str().unwrap();
    assert!(encrypted.starts_with("enc:v1:") && !encrypted.contains("token-1"));
    assert_eq!(
        text,
        format!(
            "{{\n  \"keep\": \"{NODE_CIPHERTEXT}\",\n  \"oauth:zai:access_token\": \"{encrypted}\"\n}}\n"
        )
    );
    let credentials = store.read().await.unwrap();
    assert_eq!(
        credentials
            .present("oauth:zai:access_token")
            .unwrap()
            .expose(),
        "token-1"
    );
    #[cfg(unix)]
    #[expect(clippy::disallowed_methods, reason = "测试检查文件权限")]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let lock = dir.path().join(".zcode/v2/credentials.json.lock");
    // 锁目录已释放（还在的话读目录会报错）。
    assert!(matches!(rust_cli_fs::read_optional(&lock).await, Ok(None)));
}

#[tokio::test]
async fn corrupt_files_and_failed_changes_are_not_written() {
    let dir = tempfile::tempdir().unwrap();
    let store = CredentialStore::new(storage(dir.path()), &secret(None));
    let file = store.file().as_path().to_path_buf();
    write(&file, "not json").await;
    let corrupt = store.mutate(|credentials| credentials.set("a", "b")).await;
    assert!(matches!(corrupt, Err(CredentialError::Corrupt(_))));
    assert_eq!(read_text(&file).await.unwrap(), "not json");

    write(&file, "{}").await;
    let failed: Result<(), CredentialError> = store
        .mutate(|credentials| {
            credentials.set("a", "b")?;
            Err(CredentialError::Write("refused".to_owned()))
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(read_text(&file).await.unwrap(), "{}");
}

/// 同一进程里并发的读改写经目录锁互斥：每个任务写的键都在。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writers_do_not_lose_keys() {
    let dir = tempfile::tempdir().unwrap();
    let store = CredentialStore::new(storage(dir.path()), &secret(None));
    let mut tasks = Vec::new();
    for index in 0..8 {
        let store = store.clone();
        tasks.push(tokio::spawn(async move {
            store
                .mutate(|credentials| credentials.set(&format!("key-{index}"), "v"))
                .await
                .unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let credentials = store.read().await.unwrap();
    for index in 0..8 {
        assert!(credentials.present(&format!("key-{index}")).is_some());
    }
}
