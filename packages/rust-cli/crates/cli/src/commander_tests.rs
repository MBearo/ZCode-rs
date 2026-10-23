use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Print,
    Model,
    Resume,
    Tools,
    Continue,
}

const SPECS: &[OptionSpec<Key>] = &[
    OptionSpec {
        key: Key::Print,
        flags: "-p, --print",
        short: Some("-p"),
        longs: &["--print"],
        arity: Arity::Flag,
    },
    OptionSpec {
        key: Key::Model,
        flags: "--model <model>",
        short: None,
        longs: &["--model"],
        arity: Arity::Required,
    },
    OptionSpec {
        key: Key::Resume,
        flags: "-r, --resume [value]",
        short: Some("-r"),
        longs: &["--resume"],
        arity: Arity::Optional,
    },
    OptionSpec {
        key: Key::Tools,
        flags: "--allowedTools, --allowed-tools <tools...>",
        short: None,
        longs: &["--allowedTools", "--allowed-tools"],
        arity: Arity::Variadic,
    },
    OptionSpec {
        key: Key::Continue,
        flags: "-c, --continue",
        short: Some("-c"),
        longs: &["--continue"],
        arity: Arity::Flag,
    },
];

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

/// 切分并记下每个选项事件。
/// 选项事件：哪个选项、带的值。
type Events = Vec<(Key, Option<String>)>;

fn run(values: &[&str]) -> Result<(Events, Parsed), String> {
    let mut events = Vec::new();
    let parsed = split(
        &strings(values),
        SPECS,
        |spec, value| {
            events.push((spec.key, value.map(str::to_owned)));
            Ok::<(), String>(())
        },
        |message| message,
    )?;
    Ok((events, parsed))
}

fn event(key: Key, value: &str) -> (Key, Option<String>) {
    (key, Some(value.to_owned()))
}

#[test]
fn print_is_a_flag_and_the_prompt_is_an_operand() {
    let (events, parsed) = run(&["-p", "hello", "--model", "m"]).unwrap();
    assert_eq!(events, [(Key::Print, None), event(Key::Model, "m")]);
    assert_eq!(parsed.operands, ["hello"]);
    // 位置参数可以在前面（commander 不要求顺序）。
    let (_, parsed) = run(&["hello", "-p"]).unwrap();
    assert_eq!(parsed.operands, ["hello"]);
}

#[test]
fn a_required_value_is_the_next_argument_even_when_it_looks_like_an_option() {
    let (events, _) = run(&["--model", "-p"]).unwrap();
    assert_eq!(events, [event(Key::Model, "-p")]);
    let error = run(&["-p", "--model"]).unwrap_err();
    assert_eq!(error, "error: option '--model <model>' argument missing");
}

#[test]
fn an_optional_value_is_taken_only_when_it_does_not_look_like_an_option() {
    let (events, _) = run(&["-r", "-p"]).unwrap();
    assert_eq!(events, [(Key::Resume, None), (Key::Print, None)]);
    let (events, _) = run(&["-r", "abc"]).unwrap();
    assert_eq!(events, [event(Key::Resume, "abc")]);
    // 单独的 `-` 不像选项（commander `maybeOption` 要求长度大于 1）。
    let (events, _) = run(&["-r", "-"]).unwrap();
    assert_eq!(events, [event(Key::Resume, "-")]);
}

/// Terminal-Bench 的写法：`--allowedTools` 在最后，后面的工具名都归它（cli-args.md §1.2）。
#[test]
fn a_variadic_option_takes_the_following_arguments_until_the_next_option() {
    let (events, parsed) = run(&["-p", "do it", "--allowedTools", "Bash", "Edit", "Read"]).unwrap();
    assert_eq!(
        events,
        [
            (Key::Print, None),
            event(Key::Tools, "Bash"),
            event(Key::Tools, "Edit"),
            event(Key::Tools, "Read"),
        ]
    );
    assert_eq!(parsed.operands, ["do it"]);
    // 放在 prompt 前面时会吞掉 prompt（commander 的切分如此）。
    let (_, parsed) = run(&["-p", "--allowed-tools", "Bash", "do it"]).unwrap();
    assert!(parsed.operands.is_empty());
    // 下一个选项结束它。
    let (_, parsed) = run(&["--allowedTools", "Bash", "-c", "do it"]).unwrap();
    assert_eq!(parsed.operands, ["do it"]);
}

#[test]
fn equals_and_combined_short_forms() {
    let (events, _) = run(&["--model=m1", "--allowedTools=Read,Bash", "-pc"]).unwrap();
    assert_eq!(
        events,
        [
            event(Key::Model, "m1"),
            event(Key::Tools, "Read,Bash"),
            (Key::Print, None),
            (Key::Continue, None),
        ]
    );
    // `=` 后面的参数不再归可变参数选项。
    let (_, parsed) = run(&["--allowedTools=Read", "hello"]).unwrap();
    assert_eq!(parsed.operands, ["hello"]);
    // 短选项接值。
    let (events, _) = run(&["-rabc"]).unwrap();
    assert_eq!(events, [event(Key::Resume, "abc")]);
    // 布尔选项不接 `=`：整个参数不认识。
    let (_, parsed) = run(&["--print=yes"]).unwrap();
    assert_eq!(parsed.unknown, ["--print=yes"]);
}

/// Inspect-SWE 的写法：`-- <prompt>`（cli-args.md §1.2）。
#[test]
fn a_double_dash_ends_the_options() {
    let (events, parsed) = run(&["-p", "--", "-c is not an option"]).unwrap();
    assert_eq!(events, [(Key::Print, None)]);
    assert_eq!(parsed.operands, ["-c is not an option"]);
}

#[test]
fn the_first_unknown_option_and_everything_after_it_are_unknown() {
    let (events, parsed) = run(&["-p", "--nope", "x", "--model", "m", "-h"]).unwrap();
    // 已知的选项照常处理（commander 只把不认识的参数放进 unknown）。
    assert_eq!(events, [(Key::Print, None), event(Key::Model, "m")]);
    assert_eq!(parsed.unknown, ["--nope", "x", "-h"]);
    assert!(parsed.operands.is_empty());
    let (_, parsed) = run(&["--nope", "--", "a"]).unwrap();
    assert_eq!(parsed.unknown, ["--nope", "--", "a"]);
}

#[test]
fn the_callback_error_stops_the_split() {
    let error = split(
        &strings(&["--model", "bad", "--model"]),
        SPECS,
        |_, value| Err(format!("bad value {value:?}")),
        |message| message,
    )
    .unwrap_err();
    assert_eq!(error, "bad value Some(\"bad\")");
}

/// commander `suggestSimilar` 的结果（`--nope` → `--name`）。
#[test]
fn suggestions_follow_commander() {
    let candidates = ["--name", "--model", "--help", "--print", "--resume"];
    assert_eq!(
        suggestion("--nope", &candidates),
        "\n(Did you mean --name?)"
    );
    assert_eq!(
        suggestion("--modle", &candidates),
        "\n(Did you mean --model?)"
    );
    assert_eq!(suggestion("--zzzzzzzz", &candidates), "");
    // 短选项不给建议。
    assert_eq!(suggestion("-x", &candidates), "");
    // 距离相同的几个按名字排序。
    assert_eq!(
        suggestion("--hel", &["--help", "--held", "--name"]),
        "\n(Did you mean one of --held, --help?)"
    );
}

#[test]
fn option_names_sort_like_locale_compare() {
    let mut names = vec![
        "allowedTools",
        "add-dir",
        "allow-dangerously-skip-permissions",
        "Zed",
        "help",
    ];
    names.sort_by(|a, b| locale_compare(a, b));
    assert_eq!(
        names,
        [
            "add-dir",
            "allow-dangerously-skip-permissions",
            "allowedTools",
            "help",
            "Zed"
        ]
    );
}

#[test]
fn edit_distance_counts_transpositions_once() {
    assert_eq!(edit_distance("ab", "ba"), 1);
    assert_eq!(edit_distance("nope", "name"), 2);
    assert_eq!(edit_distance("a", "abcdef"), 6);
}
