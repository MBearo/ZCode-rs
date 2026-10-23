use super::*;

fn ids() -> Arc<NodeIds> {
    Arc::new(NodeIds::new(Arc::new(
        rust_cli_node_store::SystemClock::utc(),
    )))
}

fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

/// 临时文件放在入口给的临时目录下本用户的私有目录（修复线 F58，`zcode-prompt-<uuid>.md`，
/// 0700 并核对属主）。以前自己按 `TMPDIR` → `TEMP` → `TMP` → `/tmp` 找目录，文件直接放在公共的临时目录里。
#[test]
fn the_prompt_file_goes_to_the_private_temp_dir() {
    let temp = Path::new("/var/t");
    let editor = ProcessEditor::from_env(&env(&[("TMPDIR", "/elsewhere")]), temp, ids());
    assert_eq!(editor.temp_dir, rust_cli_proc::private::user_dir(temp));
}

#[test]
fn visual_wins_over_editor_and_blank_values_do_not_count() {
    let editor = ProcessEditor::from_env(
        &env(&[("VISUAL", " code "), ("EDITOR", "vim")]),
        Path::new("/tmp"),
        ids(),
    );
    assert_eq!(editor.candidates(), ["code"]);
    assert_eq!(
        editor.source(),
        Some(("$VISUAL".to_owned(), "code".to_owned()))
    );
    let editor = ProcessEditor::from_env(
        &env(&[("VISUAL", "  "), ("EDITOR", "vim")]),
        Path::new("/tmp"),
        ids(),
    );
    assert_eq!(editor.candidates(), ["vim"]);
    let none = ProcessEditor::from_env(&[], Path::new("/tmp"), ids());
    assert_eq!(none.source(), None);
    if cfg!(windows) {
        assert_eq!(none.candidates(), ["start /wait notepad"]);
    } else {
        assert_eq!(none.candidates(), ["code", "vi", "nano"]);
    }
}

#[test]
fn gui_editors_wait_for_the_file() {
    let path = Path::new("/tmp/zcode-prompt-1.md");
    let quoted = if cfg!(windows) {
        "\"/tmp/zcode-prompt-1.md\""
    } else {
        "'/tmp/zcode-prompt-1.md'"
    };
    assert_eq!(command_line("code", path), format!("code -w {quoted}"));
    assert_eq!(command_line("subl", path), format!("subl --wait {quoted}"));
    assert_eq!(
        command_line("vim -u NONE", path),
        format!("vim -u NONE {quoted}")
    );
    assert_eq!(display_name("/usr/bin/vim -u NONE"), "vim");
}

/// 修复线 F65 的 T15：cmd 找不到命令时退出码是 9009（POSIX shell 是 127）。Windows 上两者都当作“找不到”，试下一个
/// 候选，最后提示 `No external editor was found…`；以前提示 `<名字> exited with code 9009`。其他平台的 9009 照旧是
/// 编辑器的退出码。
#[test]
fn not_found_follows_the_shell_of_the_platform() {
    assert!(not_found(127, false));
    assert!(not_found(127, true));
    assert!(not_found(9009, true));
    assert!(!not_found(9009, false));
    assert!(!not_found(1, true));
    assert!(!not_found(0, false));
}

/// 缺陷 R2-19：路径原来用双引号拼进 bash，`$(…)`、反引号与 `$变量` 在双引号里照样展开，路径里的 `"`
/// 会提前结束引号。Unix 上改用单引号字面量（`'` 写成 `'\''`）。
#[cfg(unix)]
#[test]
fn the_path_is_a_single_quoted_literal() {
    let path = Path::new("/tmp/a b/$(touch x)`id`\"q'.md");
    assert_eq!(
        command_line("vim", path),
        r#"vim '/tmp/a b/$(touch x)`id`"q'\''.md'"#
    );
}

/// 真实的编辑器进程：`sh` 脚本当编辑器，把文件改写成固定内容（Unix 才有 `/bin/bash`）。
#[cfg(unix)]
#[tokio::test]
#[expect(clippy::disallowed_methods, reason = "测试检查临时目录里剩下的文件")]
async fn the_edited_file_comes_back_and_the_temp_file_is_removed() {
    let root = tempfile::tempdir().unwrap();
    // 私有目录还不存在：编辑前按 0700 建好（F58）。
    let dir = root.path().join("zcode-private");
    let script = "printf 'edited\\n' >";
    let editor = ProcessEditor {
        visual: Some(script.to_owned()),
        editor: None,
        temp_dir: dir.clone(),
        ids: ids(),
    };
    let text = editor.edit("draft".to_owned()).await.unwrap();
    assert_eq!(text, "edited\n");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(
        std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let failing = ProcessEditor {
        visual: Some("exit 3;".to_owned()),
        editor: None,
        temp_dir: dir.clone(),
        ids: ids(),
    };
    let error = failing.edit("x".to_owned()).await.unwrap_err();
    assert_eq!(error, "exit exited with code 3");
}
