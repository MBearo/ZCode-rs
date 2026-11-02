use super::*;

#[test]
fn the_command_name_loses_its_quotes() {
    for (command, stripped) in [
        ("\\rm -f x", "rm -f x"),
        ("\"rm\" -f x", "rm -f x"),
        ("'rm' -f x", "rm -f x"),
        ("r\\m -f x", "rm -f x"),
        ("\"r\\$m\" x", "r$m x"),
        // 引号没闭合、以反斜杠结尾、开头是空白：不变。
        ("\"rm -f x", "\"rm -f x"),
        ("rm\\", "rm\\"),
    ] {
        assert_eq!(strip_safe_wrappers(command), stripped, "{command:?}");
    }
}

#[test]
fn command_builtin_and_noglob_are_wrappers() {
    for (command, stripped) in [
        ("command rm -f x", "rm -f x"),
        ("command -p rm -f x", "rm -f x"),
        ("command -pp -- rm -f x", "rm -f x"),
        ("builtin rm -f x", "rm -f x"),
        ("builtin -- rm -f x", "rm -f x"),
        ("noglob rm -f x", "rm -f x"),
        ("command builtin noglob rm", "rm"),
        ("nohup command \"rm\" x", "rm x"),
        // 后面紧接 `-` 时不去（`command -v` 是查询）；两个以上空白时照 JS 的回溯去掉。
        ("command -v rm", "command -v rm"),
        ("command  -v rm", "-v rm"),
        ("commandx rm", "commandx rm"),
    ] {
        assert_eq!(strip_safe_wrappers(command), stripped, "{command:?}");
    }
}

#[test]
fn comment_lines_go_but_blank_lines_stay() {
    assert_eq!(strip_comment_lines("# note\nls\n\nls"), "ls\n\nls");
    assert_eq!(strip_comment_lines("# only"), "# only");
}

#[test]
fn every_leading_assignment_goes_for_deny_rules() {
    for (command, stripped) in [
        ("X=1 rm -f x", "rm -f x"),
        ("X=1 Y=2 rm", "rm"),
        ("X='a b' rm", "rm"),
        ("X=\"a b\" rm", "rm"),
        ("X=\"a\\\"b\" rm", "rm"),
        ("X=a=b rm", "rm"),
        ("X+=1 rm", "rm"),
        ("X[0]=1 rm", "rm"),
        ("X=a\\ b rm", "rm"),
        ("X='a'b\"c\" rm", "rm"),
        ("X= rm", "rm"),
        // 值里有展开或命令分隔符时不去。
        ("X=$Y rm", "X=$Y rm"),
        ("X=$(id) rm", "X=$(id) rm"),
        ("X=\"$Y\" rm", "X=\"$Y\" rm"),
        ("X=a;b rm", "X=a;b rm"),
        // 赋值后面是换行：不是前缀。
        ("X=1\nrm", "X=1\nrm"),
    ] {
        assert_eq!(strip_all_assignments(command), stripped, "{command:?}");
    }
}
