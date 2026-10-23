// 复制自 rust/crates/proc/src/shell_tests.rs（feat/rs 79b6691c55）。
use super::*;

#[test]
fn cmd_exe_receives_the_text_verbatim_inside_one_pair_of_quotes() {
    // 与 hooks 的现有写法一致（apps/zcode-cli-rust/crates/tools/src/hook_process.rs）：
    // `/s` 让 cmd.exe 只去掉最外层的一对引号，其余字符原样执行，`"` 不能被改写成 `\"`。
    assert_eq!(
        cmd_exe_arguments(r#"echo "a b" > "c d.txt" & type "c d.txt""#),
        r#"/d /s /c "echo "a b" > "c d.txt" & type "c d.txt"""#
    );
    assert_eq!(cmd_exe_arguments(""), r#"/d /s /c """#);
}

#[tokio::test]
async fn quotes_reach_the_shell_unchanged() {
    let text = if cfg!(windows) {
        r#"echo "a b" "c""#
    } else {
        r#"printf '%s\n' "\"a b\" \"c\"""#
    };
    let output = script(text).output().await.unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim_end(),
        r#""a b" "c""#
    );
}
