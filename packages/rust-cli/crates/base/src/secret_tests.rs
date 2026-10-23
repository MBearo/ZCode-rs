// 复制自 rust/crates/kit/src/secret_tests.rs（feat/rs 79b6691c55）。
use serde_json::json;

use super::*;

#[test]
fn debug_and_display_hide_the_value() {
    let secret = RedactedString::new("sk-live-value");
    assert_eq!(format!("{secret:?}"), "<redacted>");
    assert_eq!(format!("{secret}"), "<redacted>");
    assert_eq!(
        format!("{:?}", Some(secret.clone())),
        "Some(<redacted>)",
        "nested Debug output stays redacted"
    );
    assert_eq!(secret.expose(), "sk-live-value");
}

#[test]
fn serde_round_trips_the_raw_value() {
    let secret = RedactedString::new("sk-live-value");
    assert_eq!(
        serde_json::to_value(&secret).unwrap(),
        json!("sk-live-value")
    );
    let parsed: RedactedString = serde_json::from_value(json!("token-1")).unwrap();
    assert_eq!(parsed, RedactedString::new("token-1"));
    assert!(serde_json::from_value::<RedactedString>(json!(1)).is_err());
}

#[test]
fn env_snapshot_debug_lists_names_only() {
    let snapshot = EnvSnapshot::from(vec![
        ("PATH".to_owned(), "/bin".to_owned()),
        ("ZCODE_API_KEY".to_owned(), "sk-secret".to_owned()),
    ]);
    assert_eq!(
        format!("{snapshot:?}"),
        r#"EnvSnapshot(["PATH", "ZCODE_API_KEY"])"#
    );
    assert_eq!(snapshot.get("ZCODE_API_KEY"), Some("sk-secret"));
    assert_eq!(snapshot.get("HOME"), None);
    assert_eq!(snapshot.len(), 2);
}

/// 入口改写快照里的一个变量：有同名的就换值，没有就加（F58）。
#[test]
fn with_var_replaces_or_appends() {
    let snapshot = EnvSnapshot::from(vec![("SHELL".to_owned(), "/bin/zsh".to_owned())]);
    let replaced = snapshot.with_var("SHELL", "/bin/bash");
    assert_eq!(replaced.get("SHELL"), Some("/bin/bash"));
    assert_eq!(replaced.len(), 1);
    let added = EnvSnapshot::default().with_var("SHELL", "/bin/bash");
    assert_eq!(added.get("SHELL"), Some("/bin/bash"));
}

/// 名字的比较（F58）：Windows 上不分大小写（Node 的 `process.env`），其他平台精确匹配。
#[test]
fn environment_names_follow_node_on_each_platform() {
    assert!(same_env_name("PATH", "PATH", false));
    assert!(!same_env_name("Path", "PATH", false));
    assert!(same_env_name("Path", "PATH", true));
    assert!(same_env_name("systemroot", "SystemRoot", true));
    assert!(!same_env_name("PATHX", "PATH", true));
    let vars = vec![
        ("Path".to_owned(), "C:\\Windows".to_owned()),
        ("HOME".to_owned(), "/home/dev".to_owned()),
    ];
    assert_eq!(env_var(&vars, "HOME"), Some("/home/dev"));
    assert_eq!(env_var(&vars, "TMPDIR"), None);
    let snapshot = EnvSnapshot::from(vars);
    assert_eq!(snapshot.get("HOME"), Some("/home/dev"));
    assert_eq!(
        snapshot.get("PATH"),
        cfg!(windows).then_some("C:\\Windows"),
        "Windows 上 `Path` 就是 `PATH`"
    );
}
