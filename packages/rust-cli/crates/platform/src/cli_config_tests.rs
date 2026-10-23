//! 用户级配置的读取（config.md §1）。
use super::*;
use crate::test_support::write;

#[tokio::test]
async fn the_user_config_is_read_from_the_cli_directory() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StoragePaths::resolve(&Default::default(), dir.path(), dir.path());
    assert_eq!(read_cli_config(&paths).await, ConfigFile::Missing);
    let file = dir.path().join(".zcode/cli/config.json");
    write(&file, r#"{"permission":{"mode":"plan"}}"#).await;
    let read = read_cli_config(&paths).await;
    assert_eq!(read.object().unwrap()["permission"]["mode"], "plan");
    write(&file, "[1]").await;
    assert!(matches!(
        read_cli_config(&paths).await,
        ConfigFile::Invalid { .. }
    ));
    write(&file, "{").await;
    assert!(read_cli_config(&paths).await.object().is_none());
}

/// 界面语言的写回（tui.md §7.1，Node `updateUiLocaleInFileConfig` + `atomicWriteJson`）。
mod ui_locale {
    use super::*;
    use crate::test_support::read_text;

    fn setup() -> (tempfile::TempDir, StoragePaths, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let paths = StoragePaths::resolve(&Default::default(), dir.path(), dir.path());
        let file = dir.path().join(".zcode/cli/config.json");
        (dir, paths, file)
    }

    #[test]
    fn only_the_three_node_values_parse() {
        for value in ["auto", "en-US", "zh-CN"] {
            assert_eq!(UiLocale::parse(value).unwrap().as_str(), value);
        }
        for value in ["zh", "zh_CN", "EN-us", ""] {
            assert_eq!(UiLocale::parse(value), None, "{value}");
        }
    }

    #[tokio::test]
    async fn a_missing_file_is_created_with_the_node_text() {
        let (_dir, paths, file) = setup();
        let written = write_ui_locale(&paths, UiLocale::ZhCn).await.unwrap();
        assert_eq!(written.as_path(), file);
        assert_eq!(
            read_text(&file).await.unwrap(),
            "{\n  \"ui\": {\n    \"locale\": \"zh-CN\"\n  }\n}\n"
        );
        #[cfg(unix)]
        #[expect(clippy::disallowed_methods, reason = "测试检查文件权限")]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[tokio::test]
    async fn other_fields_and_key_order_are_kept() {
        let (_dir, paths, file) = setup();
        write(
            &file,
            r#"{"permission":{"mode":"plan"},"ui":{"theme":"dark","locale":"en-US","x":1.0},"hooks":{}}"#,
        )
        .await;
        write_ui_locale(&paths, UiLocale::Auto).await.unwrap();
        assert_eq!(
            read_text(&file).await.unwrap(),
            "{\n  \"permission\": {\n    \"mode\": \"plan\"\n  },\n  \"ui\": {\n    \"theme\": \"dark\",\n    \"locale\": \"auto\",\n    \"x\": 1\n  },\n  \"hooks\": {}\n}\n"
        );
    }

    #[tokio::test]
    async fn a_missing_or_non_object_ui_is_replaced_at_its_place() {
        let (_dir, paths, file) = setup();
        write(&file, r#"{"a":1,"ui":"dark","b":[]}"#).await;
        write_ui_locale(&paths, UiLocale::EnUs).await.unwrap();
        let text = read_text(&file).await.unwrap();
        assert_eq!(
            text,
            "{\n  \"a\": 1,\n  \"ui\": {\n    \"locale\": \"en-US\"\n  },\n  \"b\": []\n}\n"
        );
        write(&file, r#"{"a":1}"#).await;
        write_ui_locale(&paths, UiLocale::EnUs).await.unwrap();
        let text = read_text(&file).await.unwrap();
        assert!(
            text.ends_with("\"a\": 1,\n  \"ui\": {\n    \"locale\": \"en-US\"\n  }\n}\n"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn an_unreadable_config_is_not_overwritten() {
        let (_dir, paths, file) = setup();
        let shown = file.display().to_string();
        write(&file, "{").await;
        assert_eq!(
            write_ui_locale(&paths, UiLocale::ZhCn).await.unwrap_err(),
            ConfigWriteError::Parse(shown.clone())
        );
        assert_eq!(read_text(&file).await.unwrap(), "{");
        write(&file, "[1]").await;
        let error = write_ui_locale(&paths, UiLocale::ZhCn).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("Config file must contain a JSON object: {shown}")
        );
        assert_eq!(read_text(&file).await.unwrap(), "[1]");
    }
}

/// 主题的写回（tui-manage.md §2.1）：与 `ui.locale` 同一套写法。
mod ui_theme {
    use super::*;
    use crate::test_support::read_text;

    #[test]
    fn only_the_three_node_values_parse() {
        for value in ["auto", "dark", "light"] {
            assert_eq!(UiTheme::parse(value).unwrap().as_str(), value);
        }
        for value in ["Dark", "dark-ansi", ""] {
            assert_eq!(UiTheme::parse(value), None, "{value}");
        }
    }

    #[tokio::test]
    async fn the_theme_is_set_in_place_and_bad_files_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let paths = StoragePaths::resolve(&Default::default(), dir.path(), dir.path());
        let file = dir.path().join(".zcode/cli/config.json");
        write_ui_theme(&paths, UiTheme::Light).await.unwrap();
        assert_eq!(
            read_text(&file).await.unwrap(),
            "{\n  \"ui\": {\n    \"theme\": \"light\"\n  }\n}\n"
        );
        write(&file, r#"{"ui":{"theme":"dark","locale":"zh-CN"},"x":1}"#).await;
        write_ui_theme(&paths, UiTheme::Auto).await.unwrap();
        assert_eq!(
            read_text(&file).await.unwrap(),
            "{\n  \"ui\": {\n    \"theme\": \"auto\",\n    \"locale\": \"zh-CN\"\n  },\n  \"x\": 1\n}\n"
        );
        write(&file, "[1]").await;
        assert!(matches!(
            write_ui_theme(&paths, UiTheme::Dark).await,
            Err(ConfigWriteError::NotObject(_))
        ));
        assert_eq!(read_text(&file).await.unwrap(), "[1]");
    }
}

/// `/mcp enable/disable` 的写法（tui-manage.md §6.1）。
#[tokio::test]
async fn mcp_enabled_is_set_in_place_and_the_legacy_field_goes() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StoragePaths::resolve(&Default::default(), dir.path(), dir.path());
    let file = dir.path().join(".zcode/cli/config.json");
    write(
        &file,
        r#"{"mcp":{"servers":{"docs":{"command":"d","enable":false,"enabled":true,"x":1},"other":{"url":"u"}}},"ui":{}}"#,
    )
    .await;
    write_mcp_enabled(&paths, "docs", false).await.unwrap();
    assert_eq!(
        crate::test_support::read_text(&file).await.unwrap(),
        "{\n  \"mcp\": {\n    \"servers\": {\n      \"docs\": {\n        \"command\": \"d\",\n        \"enabled\": false,\n        \"x\": 1\n      },\n      \"other\": {\n        \"url\": \"u\"\n      }\n    }\n  },\n  \"ui\": {}\n}\n"
    );
    write_mcp_enabled(&paths, "other", true).await.unwrap();
    assert!(
        crate::test_support::read_text(&file)
            .await
            .unwrap()
            .contains("\"url\": \"u\",\n        \"enabled\": true")
    );
    let error = write_mcp_enabled(&paths, "nope", true).await.unwrap_err();
    assert!(
        matches!(error, ConfigWriteError::NoMcpServer(..)),
        "{error}"
    );
    assert!(matches!(
        mcp_enabled_text(None, "/x", "docs", true),
        Err(ConfigWriteError::NoMcpServer(..))
    ));
}

/// 缺陷 R2-12：`ui.locale`、`ui.theme` 与 `/mcp enable/disable` 原来各自读改写、不加锁；TUI 里这些写回并发
/// 跑（`/theme` 紧接着 `/mcp disable`）时后写的覆盖先写的。现在都经同一个持锁的写者。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writes_keep_each_other() {
    use crate::test_support::read_text;
    let dir = tempfile::tempdir().unwrap();
    let paths = StoragePaths::resolve(&Default::default(), dir.path(), dir.path());
    let file = dir.path().join(".zcode/cli/config.json");
    write(
        &file,
        r#"{"mcp":{"servers":{"a":{"command":"x","enabled":true}}}}"#,
    )
    .await;
    for round in 0..20 {
        let theme = if round % 2 == 0 {
            UiTheme::Dark
        } else {
            UiTheme::Light
        };
        let locale = if round % 2 == 0 {
            UiLocale::ZhCn
        } else {
            UiLocale::EnUs
        };
        let enabled = round % 2 == 1;
        let (a, b, c) = tokio::join!(
            write_ui_theme(&paths, theme),
            write_mcp_enabled(&paths, "a", enabled),
            write_ui_locale(&paths, locale),
        );
        a.unwrap();
        b.unwrap();
        c.unwrap();
        let text = read_text(&file).await.unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value["ui"]["theme"],
            theme.as_str(),
            "round {round}: {text}"
        );
        assert_eq!(
            value["ui"]["locale"],
            locale.as_str(),
            "round {round}: {text}"
        );
        assert_eq!(
            value["mcp"]["servers"]["a"]["enabled"], enabled,
            "round {round}: {text}"
        );
    }
}
