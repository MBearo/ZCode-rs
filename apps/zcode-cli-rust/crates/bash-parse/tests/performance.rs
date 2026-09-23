//! 10k 字符以内的病态输入必须保持近线性（release 下均 < 2ms，debug 下 < 5ms）。
//! unbash 的 `scanBraceExpansion` 本身是平方复杂度，这里的预算用来防止回退到逐个扫描。

use std::time::{Duration, Instant};
use zcode_cli_bash_parse::analyze;

const BUDGET: Duration = Duration::from_millis(250);

#[test]
fn pathological_inputs_stay_fast() {
    let cases = [
        ("plain words", format!("echo {}", "abcdefgh ".repeat(1110))),
        (
            "quoted words",
            format!("echo {}", "'a b' \"c $d\" e\\ f ".repeat(500)),
        ),
        ("pipeline", format!("ls{}", " | wc -l".repeat(1100))),
        (
            "heredocs",
            format!("cat {}\n{}", "<<E ".repeat(1200), "x\nE\n".repeat(1200)),
        ),
        ("open braces", format!("echo {}", "{".repeat(9990))),
        ("brace lists", format!("echo {}", "{a,".repeat(3300))),
        (
            "nested substitutions",
            format!("echo {}", "\"$(".repeat(3300)),
        ),
        ("dollar parens", format!("echo {}", "$(".repeat(4990))),
        ("parameter nesting", format!("echo {}", "${".repeat(4990))),
        ("backslashes", format!("echo {}", "\\".repeat(9990))),
        ("extglob", format!("echo {}", "@(".repeat(4990))),
        ("assignments", format!("{}ls", "A=1 ".repeat(2490))),
        ("redirects", format!("ls {}", "2>&1 ".repeat(1990))),
        (
            "shared stale heredoc target",
            format!(
                "cat >\"{}\" >a {}\nE",
                "x".repeat(5000),
                "<<E ".repeat(1000)
            ),
        ),
        (
            "case in substitution",
            format!("echo $({})", "case x in a) ;; esac ".repeat(450)),
        ),
    ];
    for (label, command) in &cases {
        assert!(command.len() <= 10_000, "{label}: input too long");
        let start = Instant::now();
        analyze(command);
        let elapsed = start.elapsed();
        assert!(elapsed < BUDGET, "{label}: took {elapsed:?}");
    }
}
