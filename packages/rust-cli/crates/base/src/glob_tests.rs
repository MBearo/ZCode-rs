//! 从 context 的 `rules_glob_tests.rs` 移来（F58：匹配器收进 base）。
use super::*;

fn globs(patterns: &[&str]) -> Globs {
    Globs::new(&patterns.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>())
}

#[test]
fn names_without_a_slash_match_at_any_depth() {
    let ts = globs(&["*.ts"]);
    assert!(ts.matches("a.ts"));
    assert!(ts.matches("src/deep/a.ts"));
    assert!(!ts.matches("src/a.tsx"));
    // 目录名命中时其下一切都算（所以模式结尾的 `/**` 可以去掉）。
    let src = globs(&["src"]);
    assert!(src.matches("src/a/b.rs"));
    assert!(src.matches("pkg/src/b.rs"));
    assert!(!src.matches("source/b.rs"));
}

#[test]
fn slashes_anchor_the_pattern_and_double_stars_cross_directories() {
    let anchored = globs(&["src/*.ts"]);
    assert!(anchored.matches("src/a.ts"));
    assert!(!anchored.matches("src/x/a.ts"));
    assert!(!anchored.matches("pkg/src/a.ts"));
    let deep = globs(&["src/**/*.ts"]);
    assert!(deep.matches("src/a.ts"));
    assert!(deep.matches("src/x/y/a.ts"));
    let rooted = globs(&["/docs"]);
    assert!(rooted.matches("docs/a.md"));
    assert!(!rooted.matches("x/docs/a.md"));
    let any = globs(&["**/test/*.rs"]);
    assert!(any.matches("test/a.rs"));
    assert!(any.matches("crates/x/test/a.rs"));
}

#[test]
fn directory_patterns_negation_classes_and_case() {
    let dirs = globs(&["build/"]);
    assert!(dirs.matches("build/out.js"));
    assert!(!dirs.matches("build"));
    let negated = globs(&["*.md", "!README.md"]);
    assert!(negated.matches("docs/a.md"));
    assert!(!negated.matches("README.md"));
    let class = globs(&["file[0-9].txt", "?.rs"]);
    assert!(class.matches("file7.txt"));
    assert!(!class.matches("filex.txt"));
    assert!(class.matches("a.rs"));
    assert!(!class.matches("ab.rs"));
    assert!(globs(&["SRC/*.TS"]).matches("src/a.ts"));
}
