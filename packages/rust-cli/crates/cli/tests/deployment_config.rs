//! 部署配置的错误路径（docs/specs/rust-cli/deployment.md §6）：真实的 `zcode -p`。写错的配置报出变量与原因、退出码 1，
//! 不按缺省（本机）悄悄运行；会话库只有 SQLite，写了 `store`（别的库）同样报错，不落到 SQLite 上。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：写临时的部署配置、拉起 zcode 进程"
)]

use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_zcode-rs");

fn run_with(config: &str) -> Output {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("deployment.json");
    std::fs::write(&file, config).unwrap();
    Command::new(BIN)
        .args(["-p", "hi"])
        .current_dir(home.path())
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", home.path())
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL_BASE_URL", "http://127.0.0.1:9")
        .env("ZCODE_RUNTIME_ENV", "test")
        .env("ZCODE_DEPLOYMENT", &file)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn a_misspelled_key_or_a_store_is_an_error_not_the_local_default() {
    for config in [
        r#"{"primary": "local"}"#,
        r#"{"store": {"postgres": {"url_env": "ZCODE_PG_URL"}}}"#,
    ] {
        let output = run_with(config);
        assert_eq!(output.status.code(), Some(1), "{config}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("ZCODE_DEPLOYMENT="), "{stderr}");
        assert!(stderr.contains("unknown field"), "{stderr}");
    }
}
