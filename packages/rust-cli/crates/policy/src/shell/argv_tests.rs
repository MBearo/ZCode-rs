use super::*;

fn stripped(argv: &[&str]) -> Vec<String> {
    let argv: Vec<String> = argv.iter().map(|word| (*word).to_owned()).collect();
    strip_wrappers(&argv)
}

#[test]
fn wrappers_and_their_options_go() {
    for (argv, command) in [
        (&["env", "rm", "x"][..], &["rm", "x"][..]),
        (&["env", "-i", "X=1", "rm", "x"], &["rm", "x"]),
        (&["env", "-u", "HOME", "rm", "x"], &["rm", "x"]),
        (&["env", "--", "rm", "x"], &["rm", "x"]),
        (&["sudo", "-u", "root", "--", "rm", "x"], &["rm", "x"]),
        (&["doas", "rm", "x"], &["rm", "x"]),
        (&["watch", "-n", "1", "rm", "x"], &["rm", "x"]),
        (&["ionice", "-c", "3", "rm", "x"], &["rm", "x"]),
        (&["taskset", "1", "rm", "x"], &["rm", "x"]),
        (&["taskset", "0x3", "rm", "x"], &["rm", "x"]),
        (&["chrt", "1", "rm", "x"], &["rm", "x"]),
        (&["flock", "lk", "rm", "x"], &["rm", "x"]),
        (&["exec", "-a", "name", "rm", "x"], &["rm", "x"]),
        (&["nocorrect", "rm", "x"], &["rm", "x"]),
        (&["X=1", "nohup", "sudo", "rm", "x"], &["rm", "x"]),
        // 包装命令之后的赋值是命令名（只在每一轮开头去赋值），停下。
        (
            &["nohup", "FOO=bar", "sudo", "rm", "x"],
            &["FOO=bar", "sudo", "rm", "x"],
        ),
        (&["timeout", "-s", "KILL", "5", "rm", "x"], &["rm", "x"]),
        (&["nice", "-n", "5", "--", "rm", "x"], &["rm", "x"]),
    ] {
        assert_eq!(stripped(argv), command, "{argv:?}");
    }
}

#[test]
fn command_text_options_are_split_again() {
    for (argv, command) in [
        (&["env", "-S", "rm -f x"][..], &["rm", "-f", "x"][..]),
        (&["env", "-Srm -f x"], &["rm", "-f", "x"]),
        (&["env", "--split-string=rm x"], &["rm", "x"]),
        (&["script", "-c", "rm -f x"], &["rm", "-f", "x"]),
        (&["flock", "-c", "rm -f x", "lk"], &["rm", "-f", "x"]),
        (&["env", "-S", "sudo rm x"], &["rm", "x"]),
    ] {
        assert_eq!(stripped(argv), command, "{argv:?}");
    }
}

#[test]
fn queries_and_unknown_shapes_stay() {
    for argv in [
        &["command", "-v", "rm"][..],
        &["sudo"],
        &["sudo", "-u", "root"],
        &["timeout", "--bogus", "5", "rm"],
        &["/usr/bin/env", "rm"],
        &["xargs", "rm"],
    ] {
        assert_eq!(stripped(argv), argv, "{argv:?}");
    }
}
