//! 与 Node oracle（scripts/zcode-cli-rust-bash-parse-fixtures.mjs）生成的 fixture 逐条对比：
//! 安全判定必须一致；Node 判定安全时整个分析结果的 JSON 必须一致。

use serde_json::Value;
use zcode_cli_bash_parse::{analyze, is_permission_safe};

const HANDWRITTEN: &str = include_str!("../fixtures/analysis.json");
const FUZZ: &str = include_str!("../fixtures/analysis-fuzz.json");

/// 已知且有意保留的差异：接口把 fd 定为 `u32`，Node 的 `Number.parseInt` 结果超出范围时这里饱和。
/// 安全判定不受影响。
const JSON_DIVERGENCES: &[&str] = &["ls 99999999999>f"];

fn check(label: &str, fixtures: &str) -> (usize, usize) {
    let cases: Vec<Value> = serde_json::from_str(fixtures).expect("fixture JSON");
    let mut failures = Vec::new();
    let mut safe_cases = 0;
    for case in &cases {
        let command = case["command"].as_str().expect("command");
        let expected_safe = case["safe"].as_bool().expect("safe");
        let analysis = analyze(command);
        let actual = serde_json::to_value(&analysis).expect("serialize");
        if is_permission_safe(&analysis) != expected_safe {
            failures.push(format!(
                "verdict {command:?}: expected safe={expected_safe}\n  node: {}\n  rust: {actual}",
                case["analysis"]
            ));
            continue;
        }
        if expected_safe {
            safe_cases += 1;
            if actual != case["analysis"] && !JSON_DIVERGENCES.contains(&command) {
                failures.push(format!(
                    "json {command:?}\n  node: {}\n  rust: {actual}",
                    case["analysis"]
                ));
            }
        }
    }
    let shown: Vec<&str> = failures.iter().take(40).map(String::as_str).collect();
    assert!(
        failures.is_empty(),
        "{label}: {} of {} cases differ ({safe_cases} safe matched)\n{}",
        failures.len(),
        cases.len(),
        shown.join("\n")
    );
    (cases.len(), safe_cases)
}

#[test]
fn handwritten_fixtures_match_node() {
    let (total, safe) = check("hand-written", HANDWRITTEN);
    assert!(
        total > 1000 && safe > 700,
        "unexpectedly small corpus: {total} cases, {safe} safe"
    );
}

#[test]
fn fuzz_fixtures_match_node() {
    let (total, safe) = check("fuzz", FUZZ);
    assert!(
        total > 2500 && safe > 1000,
        "unexpectedly small corpus: {total} cases, {safe} safe"
    );
}

/// 大规模差分测试入口：`BASH_PARSE_FIXTURES=<json> cargo test -p zcode-cli-bash-parse -- --ignored`。
#[test]
#[ignore = "needs an external fixture file"]
fn external_fixtures_match_node() {
    let path = std::env::var("BASH_PARSE_FIXTURES").expect("BASH_PARSE_FIXTURES");
    let fixtures = std::fs::read_to_string(&path).expect("read fixtures");
    let (total, safe) = check(&path, &fixtures);
    eprintln!("{path}: {total} cases, {safe} safe, all match");
    // 不安全用例只要求判定一致；这里统计各个标志位与完整 JSON 的一致程度，供参考。
    let cases: Vec<Value> = serde_json::from_str(&fixtures).expect("fixture JSON");
    let mut stats = [0usize; 5];
    for case in cases.iter().filter(|case| case["safe"] == false) {
        let actual = serde_json::to_value(analyze(case["command"].as_str().unwrap())).unwrap();
        let expected = &case["analysis"];
        stats[0] += 1;
        for (slot, key) in [
            (1, "hasParseErrors"),
            (2, "hasUnsupportedSyntax"),
            (3, "hasDynamicWords"),
        ] {
            stats[slot] += usize::from(actual[key] == expected[key]);
        }
        stats[4] += usize::from(&actual == expected);
    }
    eprintln!(
        "unsafe cases: {}; hasParseErrors equal {}, hasUnsupportedSyntax equal {}, hasDynamicWords equal {}, full JSON equal {}",
        stats[0], stats[1], stats[2], stats[3], stats[4]
    );
}

#[test]
fn saturated_fd_is_the_only_json_divergence() {
    let analysis = analyze("ls 99999999999>f");
    assert!(is_permission_safe(&analysis));
    assert_eq!(
        analysis.commands[0].redirects[0].file_descriptor,
        Some(u32::MAX)
    );
}

/// Node 的 `extractBalanced`/`skipDQ` 递归在约 2560 层 `"$(`（冷启动）时栈溢出并报 parse error，
/// JIT 预热后则不会；Rust 在 2000 层（4000 帧）处保守地截断。低于此深度时与 Node 完全一致。
#[test]
fn deep_nesting_follows_node_stack_limits() {
    // Node 在任何 JIT 状态下都不会溢出的深度：复现 Node 的 bug（被丢弃的前缀重定向），判为安全。
    let shallow = format!("> $({}", "\"$(".repeat(1900));
    let analysis = analyze(&shallow);
    assert!(is_permission_safe(&analysis));
    assert_eq!(analysis.commands.len(), 1);
    // 超过保守上限：与 Node 冷启动时的 RangeError 结果一致。
    let deep = format!("> $({}", "\"$(".repeat(2100));
    let analysis = analyze(&deep);
    assert!(analysis.has_parse_errors && analysis.commands.is_empty());
    let deep_array = format!("a=({}{} ls", "\"$(".repeat(2100), ")".repeat(2101));
    assert!(analyze(&deep_array).has_parse_errors);
    // `${x:-"` 每层只占一个 JS 帧，10k 字符内最多 1666 层，Node 从不溢出。
    let params = format!("> {}", "${x:-\"".repeat(1666));
    assert!(is_permission_safe(&analyze(&params)));
}

/// unbash 4.0.1 在这些输入上死循环（ANSI-C 串以孤立反斜杠结尾），Node 拿不到结果；
/// Rust 必须给出结论且判为不安全。
#[test]
fn node_hanging_inputs_are_parse_errors() {
    for command in [
        "echo $'\\",
        "echo \"$'\\",
        "$'A=$'\\",
        "a=($'\\) ls",
        "cat <<E\n$x $'\\",
        "echo ${x:-$'\\}",
        "echo $(echo $'\\)",
    ] {
        let analysis = analyze(command);
        assert!(
            !is_permission_safe(&analysis),
            "{command:?} must not be safe"
        );
    }
    for command in ["echo $'\\", "$'A=$'\\", "a=($'\\) ls", "cat <<E\n$x $'\\"] {
        assert!(
            analyze(command).has_parse_errors,
            "{command:?} must report the hang as a parse error"
        );
    }
}
