//! 界面输入的测试：环境变量的取舍、远程 shell 与 shell 名、命令行的启动会话、`theme.json` 等小文件。
use rust_cli_kernel_api::ids::SessionId;

use super::*;

fn pairs(list: &[(&str, &str)]) -> rust_cli_base::secret::EnvSnapshot {
    list.iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn only_terminal_and_locale_variables_reach_the_interface() {
    let env = pairs(&[
        ("LANG", "zh_CN.UTF-8"),
        ("ZCODE_API_KEY", "sk-test"),
        ("TERM", "xterm-256color"),
        ("TMUX", "/tmp/tmux-501/default,1,0"),
        // 滚轮的速度。
        ("ZCODE_SCROLL_SPEED", "5"),
    ]);
    let vars = vars(&env);
    assert_eq!(vars.len(), 4);
    assert!(!vars.contains_key("ZCODE_API_KEY"));
    assert_eq!(
        vars.get("ZCODE_SCROLL_SPEED").map(String::as_str),
        Some("5")
    );
}

/// 远程 shell（Scala `Place.from` 的 `remote`）：SSH 算远端；有图形界面的本机不算。
#[test]
fn ssh_makes_a_remote_shell() {
    let local = remote_shell(&pairs(&[("LANG", "zh_CN.UTF-8"), ("DISPLAY", ":0")]));
    assert!(!local);
    let ssh = remote_shell(&pairs(&[
        ("LANG", "en_US.UTF-8"),
        ("SSH_TTY", "/dev/ttys001"),
    ]));
    assert!(ssh);
}

#[cfg(not(windows))]
#[test]
fn the_shell_name_is_the_file_name_of_shell() {
    assert_eq!(
        shell_name(&pairs(&[("SHELL", "/opt/homebrew/bin/fish")])),
        "fish"
    );
    assert_eq!(shell_name(&pairs(&[("SHELL", "")])), "sh");
    assert_eq!(shell_name(&pairs(&[])), "sh");
}

/// `-c` / `-r [id]` / 不带 id 的 `-r` 的会话。
#[test]
fn the_command_line_session_becomes_the_startup() {
    assert_eq!(startup(&SessionTarget::New, false), Startup::New);
    assert_eq!(startup(&SessionTarget::Continue, false), Startup::Latest);
    assert_eq!(startup(&SessionTarget::New, true), Startup::Pick);
    assert_eq!(
        startup(&SessionTarget::Resume(SessionId::new("s1")), false),
        Startup::Session("s1".to_owned())
    );
}

/// 小文件：没有时 `Ok(None)`，写了读得回（目录不存在时建好）。
#[tokio::test]
async fn the_stores_round_trip_through_their_folder() {
    let temp = tempfile::tempdir().expect("temp dir");
    let stores = FileStores {
        dir: temp.path().join(STORES_DIR),
    };
    assert_eq!(stores.read(StoreFile::Theme).await, Ok(None));
    stores
        .write(StoreFile::Models, "{\"models\":{}}\n".to_owned())
        .await
        .expect("written");
    assert_eq!(
        stores.read(StoreFile::Models).await,
        Ok(Some("{\"models\":{}}\n".to_owned()))
    );
    let written = rust_cli_fs::read_optional(&temp.path().join("puffin-tui/models.json")).await;
    assert_eq!(
        written.ok().flatten().as_deref(),
        Some(b"{\"models\":{}}\n".as_slice())
    );
}
