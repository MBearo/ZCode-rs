//! 部署配置的解析与校验（docs/specs/rust-cli/deployment.md §6）。
#![allow(clippy::disallowed_methods, reason = "测试在临时目录里准备配置文件")]

use super::{Deployment, PrimaryEnv, SshPrimary, VAR, load};

#[test]
fn an_empty_config_is_the_local_default() {
    let deployment = Deployment::parse("{}").unwrap();
    assert_eq!(deployment, Deployment::default());
    assert!(deployment.primary_is_local());
}

#[test]
fn an_ssh_primary_env_parses() {
    let text = r#"{
        "primary_env": {"ssh": {"destination": "dev@sandbox", "workdir": "/srv/proj", "ssh_config": "/tmp/ssh.conf"}}
    }"#;
    let deployment = Deployment::parse(text).unwrap();
    assert_eq!(
        deployment.primary_env,
        PrimaryEnv::Ssh(SshPrimary {
            destination: "dev@sandbox".to_owned(),
            workdir: "/srv/proj".to_owned(),
            ssh_config: Some("/tmp/ssh.conf".to_owned()),
            artifact: None,
        })
    );
    assert!(!deployment.primary_is_local());
    assert_eq!(
        Deployment::parse(r#"{"primary_env": "local"}"#).unwrap(),
        Deployment::default()
    );
}

#[test]
fn unknown_keys_and_bad_values_are_errors() {
    // 拼错的键不能被悄悄忽略：那样会跑在缺省（本机）上，用户以为在沙盒里。会话库只有 SQLite，`store` 也是未知键：
    // 写了别的库不能悄悄落到 SQLite 上（deployment.md §6）。
    for text in [
        r#"{"primary": "local"}"#,
        r#"{"primary_env": {"ssh": {"destination": "h", "workdir": "/w", "port": 22}}}"#,
        r#"{"primary_env": "cloud"}"#,
        r#"{"store": {"postgres": {"url_env": "ZCODE_PG_URL"}}}"#,
    ] {
        assert!(Deployment::parse(text).is_err(), "{text}");
    }
    let relative = r#"{"primary_env": {"ssh": {"destination": "h", "workdir": "proj"}}}"#;
    let error = Deployment::parse(relative).unwrap_err();
    assert!(error.contains("absolute"), "{error}");
    let empty = r#"{"primary_env": {"ssh": {"destination": " ", "workdir": "/w"}}}"#;
    assert!(
        Deployment::parse(empty)
            .unwrap_err()
            .contains("destination")
    );
    let windows = r#"{"primary_env": {"ssh": {"destination": "h", "workdir": "C:\\work"}}}"#;
    assert!(Deployment::parse(windows).is_ok());
}

#[tokio::test]
async fn the_variable_names_the_file_and_errors_name_the_variable() {
    assert_eq!(
        load(&rust_cli_base::secret::EnvSnapshot::default())
            .await
            .unwrap(),
        Deployment::default()
    );
    let blank = rust_cli_base::secret::EnvSnapshot::from(vec![(VAR.to_owned(), " ".to_owned())]);
    assert_eq!(load(&blank).await.unwrap(), Deployment::default());

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("deployment.json");
    std::fs::write(
        &file,
        br#"{"primary_env": {"ssh": {"destination": "h", "workdir": "/w"}}}"#,
    )
    .unwrap();
    let env = rust_cli_base::secret::EnvSnapshot::from(vec![(
        VAR.to_owned(),
        file.to_string_lossy().into_owned(),
    )]);
    assert!(!load(&env).await.unwrap().primary_is_local());

    let missing = rust_cli_base::secret::EnvSnapshot::from(vec![(
        VAR.to_owned(),
        dir.path().join("nope.json").to_string_lossy().into_owned(),
    )]);
    let error = load(&missing).await.unwrap_err();
    assert!(
        error.to_string().starts_with("ZCODE_DEPLOYMENT="),
        "{error}"
    );
    assert!(error.to_string().contains("no such file"), "{error}");
}
