//! bfs 的谓词与标志组（工作线 F53 阶段 0，guarded 的 `find-delete` 用）。

use super::*;

#[test]
fn flags_and_groups() {
    assert_eq!(classify("-H"), Some(Arg::Flag { values: 0 }));
    assert_eq!(classify("-S"), Some(Arg::Flag { values: 1 }));
    assert_eq!(classify("-Sdfs"), Some(Arg::Flag { values: 0 }));
    assert_eq!(classify("-j"), None);
    assert_eq!(classify("-HLX"), Some(Arg::Flag { values: 0 }));
    assert_eq!(classify("-Hf"), Some(Arg::RootFlag));
    assert_eq!(classify("-fH"), None);
    // 组里要有大写字母，否则是谓词（bfs 不认识就报错）。
    assert_eq!(classify("-dx"), None);
    assert_eq!(classify("-rm"), Some(Arg::Primary { values: 0 }));
    assert_eq!(classify("-newerBt"), Some(Arg::Primary { values: 1 }));
}
