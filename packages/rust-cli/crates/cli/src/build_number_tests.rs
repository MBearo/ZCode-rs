//! 编译期的版本号（docs/specs/rust-cli/version.md §1–§3）：计数文件的读、加 1、原子写，坏内容报错，正式发版的
//! 校验与只读，debug 不读不写，给 cargo 的 rerun 指令。全部用临时目录与注入的路径，不碰仓库里的计数文件
//! （最后一项只读它）。
#![allow(
    clippy::disallowed_methods,
    reason = "测试在临时目录里准备与读回计数文件"
)]

use std::path::{Path, PathBuf};

use super::*;

/// 临时的 workspace：`<dir>/crates/cli` 是 cli 目录，计数文件在 `<dir>/build-number`。
struct Workspace {
    _dir: tempfile::TempDir,
    cli: PathBuf,
    counter: PathBuf,
}

impl Workspace {
    fn new(counter: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join("crates").join("cli");
        std::fs::create_dir_all(&cli).unwrap();
        let counter_file = dir.path().join(COUNTER_FILE);
        if let Some(text) = counter {
            std::fs::write(&counter_file, text).unwrap();
        }
        Self {
            _dir: dir,
            cli,
            counter: counter_file,
        }
    }

    fn inputs<'a>(&'a self, profile: &'a str, publish: Option<&'a str>) -> Inputs<'a> {
        Inputs {
            profile,
            manifest_dir: &self.cli,
            package_version: "0.1.0",
            major: "0",
            minor: "1",
            publish,
        }
    }

    fn counter_text(&self) -> String {
        std::fs::read_to_string(&self.counter).unwrap()
    }

    fn leftovers(&self) -> bool {
        self.counter.with_extension("tmp").exists()
    }
}

fn version_directive(version: &str) -> String {
    format!("rustc-env=ZCODE_BUILD_VERSION={version}")
}

#[test]
fn a_local_release_build_adds_one_and_writes_back_atomically() {
    let workspace = Workspace::new(Some("51\n"));
    // 上一次编译在 rename 之前被打断，留下的临时文件被覆盖掉。
    std::fs::write(workspace.counter.with_extension("tmp"), "9").unwrap();
    assert_eq!(
        directives(&workspace.inputs("release", None)),
        Ok(vec![
            format!("rerun-if-changed={SENTINEL}"),
            version_directive("0.1.52"),
        ])
    );
    assert_eq!(workspace.counter_text(), "52\n");
    assert!(!workspace.leftovers());
    // 每次 release 编译都加 1。
    let again = directives(&workspace.inputs("release", None)).unwrap();
    assert_eq!(again.last(), Some(&version_directive("0.1.53")));
    assert_eq!(workspace.counter_text(), "53\n");
}

#[test]
fn the_count_is_one_decimal_integer() {
    assert_eq!(parse_count("51\n"), Ok(51));
    assert_eq!(parse_count(" 41 "), Ok(41));
    // Windows 检出成 CRLF 时也认。
    assert_eq!(parse_count("51\r\n"), Ok(51));
    let conflict = "<<<<<<< HEAD\n53\n=======\n55\n>>>>>>> lane\n";
    for text in ["", "abc", "-1", "1.5", "0x10", conflict] {
        let error = parse_count(text).unwrap_err();
        assert!(error.contains("keep the larger"), "{text:?}: {error}");
    }
}

#[test]
fn a_missing_or_broken_counter_fails_and_is_left_alone() {
    let missing = Workspace::new(None);
    let error = directives(&missing.inputs("release", None)).unwrap_err();
    assert!(error.contains("build-number"), "{error}");
    assert!(error.contains("missing"), "{error}");
    assert!(!missing.counter.exists() && !missing.leftovers());

    let max = u64::MAX.to_string();
    for text in [
        "",
        "abc\n",
        "<<<<<<< HEAD\n53\n=======\n55\n>>>>>>> lane\n",
        &max,
    ] {
        let workspace = Workspace::new(Some(text));
        let error = directives(&workspace.inputs("release", None)).unwrap_err();
        assert!(error.contains("build-number"), "{text:?}: {error}");
        assert_eq!(workspace.counter_text(), text);
        assert!(!workspace.leftovers());
    }
}

#[test]
fn a_publication_build_checks_the_version_and_does_not_write() {
    let workspace = Workspace::new(Some("52\n"));
    let shown = workspace.counter.display().to_string();
    assert_eq!(
        directives(&workspace.inputs("release", Some("0.1.52"))),
        Ok(vec![
            "rerun-if-changed=build.rs".to_owned(),
            format!("rerun-if-changed={shown}"),
            format!("rerun-if-env-changed={RELEASE_ENV}"),
            version_directive("0.1.52"),
        ])
    );
    assert_eq!(workspace.counter_text(), "52\n");
    assert!(!workspace.leftovers());
    // 逐字比较：别的号、空串、带空白或前缀的都不等，编译失败，也不写文件。
    for value in [
        "0.1.51", "0.1.53", "", " 0.1.52", "0.1.52\n", "v0.1.52", "0.2.52",
    ] {
        let error = directives(&workspace.inputs("release", Some(value))).unwrap_err();
        assert!(error.contains(RELEASE_ENV), "{value:?}: {error}");
        assert!(error.contains("0.1.52"), "{value:?}: {error}");
        assert_eq!(workspace.counter_text(), "52\n");
    }
    // 发版时文件缺失：同样失败。
    let missing = Workspace::new(None);
    let error = directives(&missing.inputs("release", Some("0.1.52"))).unwrap_err();
    assert!(error.contains("missing"), "{error}");
}

#[test]
fn a_debug_build_neither_reads_nor_writes_the_counter() {
    let expected = Ok(vec![
        "rerun-if-changed=build.rs".to_owned(),
        version_directive("0.1.0"),
    ]);
    // 没有文件、文件坏了、设了发版变量：debug 的结果都一样，文件不动。
    let missing = Workspace::new(None);
    assert_eq!(directives(&missing.inputs("debug", None)), expected);
    assert!(!missing.counter.exists());
    for text in ["51\n", "abc"] {
        let workspace = Workspace::new(Some(text));
        assert_eq!(directives(&workspace.inputs("debug", None)), expected);
        assert_eq!(
            directives(&workspace.inputs("debug", Some("9.9.9"))),
            expected
        );
        assert_eq!(workspace.counter_text(), text);
        assert!(!workspace.leftovers());
    }
}

#[test]
fn the_release_version_keeps_major_and_minor() {
    assert_eq!(release_version("0", "1", 7), "0.1.7");
    assert_eq!(release_version("2", "10", 1), "2.10.1");
}

#[test]
fn the_counter_sits_in_the_workspace_directory() {
    assert_eq!(
        counter_path(Path::new("/w/packages/rust-cli/crates/cli")),
        Ok(PathBuf::from("/w/packages/rust-cli/build-number"))
    );
    // 与 target 目录无关：只看 cli 目录。
    assert!(counter_path(Path::new("cli")).is_err());
}

#[test]
fn the_committed_build_number_is_an_integer() {
    // 只读仓库里的文件：合并后留下冲突标记时，debug 的测试也会报出来（version.md §2）。
    let path = counter_path(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    assert!(read_count(&path).is_ok(), "{}", path.display());
}
