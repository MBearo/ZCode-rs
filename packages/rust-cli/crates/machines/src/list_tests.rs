//! 机器清单的读写（remote-exec.md §20.3.1）。
#![allow(clippy::disallowed_methods, reason = "测试：准备与读回清单文件")]

use serde_json::json;

use super::*;

fn list() -> (tempfile::TempDir, MachineList) {
    let dir = tempfile::tempdir().unwrap();
    let list = MachineList::at(&dir.path().join("rust-cli/machines.json"));
    (dir, list)
}

#[tokio::test]
async fn a_missing_file_is_an_empty_list() {
    let (_dir, list) = list();
    assert!(list.load().await.unwrap().is_empty());
}

#[tokio::test]
async fn machines_round_trip_and_keep_unknown_fields() {
    let (dir, list) = list();
    list.add(MachineEntry::new("dev-a", "dev-a", Some("/home/dev/proj")))
        .await
        .unwrap();
    list.add(MachineEntry::new("gpu-1", "user@gpu.example.com", None))
        .await
        .unwrap();
    list.set_connected("dev-a", true).await.unwrap();
    let path = dir.path().join("rust-cli/machines.json");
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        written,
        json!({"version": 1, "machines": [
            {"name": "dev-a", "ssh": "dev-a", "workingDirectory": "/home/dev/proj", "connected": true},
            {"name": "gpu-1", "ssh": "user@gpu.example.com", "connected": false}
        ]})
    );
    // 以后的字段（这里写进文件）读进来、写回去都还在。
    let mut edited = written.clone();
    edited["machines"][0]["port"] = json!(2222);
    edited["note"] = json!("kept");
    std::fs::write(&path, edited.to_string()).unwrap();
    list.set_connected("dev-a", false).await.unwrap();
    let again: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(again["machines"][0]["port"], 2222);
    assert_eq!(again["note"], "kept");
    assert_eq!(again["machines"][0]["connected"], false);
    let loaded = list.load().await.unwrap();
    assert_eq!(loaded[1].working_directory, None);
    list.remove("gpu-1").await.unwrap();
    assert_eq!(list.load().await.unwrap().len(), 1);
}

#[tokio::test]
async fn names_are_checked() {
    let (_dir, list) = list();
    for bad in ["", "local", "a b", "x/y", &"n".repeat(33)] {
        assert!(matches!(
            list.add(MachineEntry::new(bad, "h", None)).await,
            Err(MachinesError::Name(_))
        ));
    }
    list.add(MachineEntry::new("dev-a", "h", None))
        .await
        .unwrap();
    assert!(matches!(
        list.add(MachineEntry::new("dev-a", "h", None)).await,
        Err(MachinesError::Exists(_))
    ));
    assert!(matches!(
        list.remove("nope").await,
        Err(MachinesError::Missing(_))
    ));
}

#[tokio::test]
async fn a_broken_file_is_an_error_and_is_not_rewritten() {
    let (dir, list) = list();
    let path = dir.path().join("rust-cli/machines.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{not json").unwrap();
    assert!(matches!(
        list.load().await,
        Err(MachinesError::Invalid { .. })
    ));
    assert!(list.set_connected("dev-a", true).await.is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");
    std::fs::write(&path, r#"{"machines":[{"name":"local","ssh":"x"}]}"#).unwrap();
    assert!(matches!(
        list.load().await,
        Err(MachinesError::Invalid { .. })
    ));
}
