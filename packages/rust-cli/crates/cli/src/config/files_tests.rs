//! 项目文件的发现（config.md §1）。
use rust_cli_kernel_api::env::EnvFacts;
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::testing::MemoryEnvironment;

use super::*;

fn env(files: &[(&str, &str)]) -> MemoryEnvironment {
    let facts = EnvFacts {
        cwd: EnvPath::new("/Users/dev/repo/app"),
        platform: "linux".to_owned(),
        os_version: "Linux".to_owned(),
        shell: "bash".to_owned(),
        is_git_repo: true,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    };
    files
        .iter()
        .fold(MemoryEnvironment::new(facts), |env, (path, text)| {
            env.with_file(path, text)
        })
}

fn workspace(root: &str) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root),
    }
}

#[tokio::test]
async fn files_are_found_from_the_git_root_down_to_the_workspace() {
    let env = env(&[
        ("/Users/dev/zcode.json", "{}"),
        ("/Users/dev/repo/.git/HEAD", "ref"),
        ("/Users/dev/repo/zcode.json", r#"{"a":1}"#),
        ("/Users/dev/repo/.zcode/config.json", "{"),
        ("/Users/dev/repo/app/.zcode/config.json", r#"{"b":2}"#),
    ]);
    let files = discover(&env, &workspace("/Users/dev/repo/app")).await;
    let found: Vec<_> = files
        .iter()
        .map(|file| (file.path.as_str(), file.kind, file.order, file.editable))
        .collect();
    assert_eq!(
        found,
        [
            (
                "/Users/dev/repo/zcode.json",
                ProjectKind::ZcodeJson,
                0,
                false
            ),
            (
                "/Users/dev/repo/.zcode/config.json",
                ProjectKind::DotZcodeConfig,
                1,
                false
            ),
            (
                "/Users/dev/repo/app/.zcode/config.json",
                ProjectKind::DotZcodeConfig,
                2,
                true
            ),
        ]
    );
    // 读坏的文件也占序号。
    assert!(files[1].file.object().is_none());
    assert_eq!(files[2].file.object().unwrap()["b"], 2);
}

#[tokio::test]
async fn without_a_git_marker_only_the_workspace_is_read() {
    let env = env(&[
        ("/Users/dev/zcode.json", "{}"),
        ("/Users/dev/repo/app/zcode.json", "{}"),
        // `.git` 是文件（worktree）也算。
        ("/Users/dev/other/.git", "gitdir: x"),
        ("/Users/dev/other/zcode.json", "{}"),
    ]);
    let files = discover(&env, &workspace("/Users/dev/repo/app")).await;
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "/Users/dev/repo/app/zcode.json");
    let files = discover(&env, &workspace("/Users/dev/other")).await;
    assert_eq!(files[0].path, "/Users/dev/other/zcode.json");
}

#[test]
fn relative_paths_follow_node() {
    assert_eq!(relative("/w/app", "/w/app/zcode.json"), "zcode.json");
    assert_eq!(
        relative("/w/app", "/w/.zcode/config.json"),
        "../.zcode/config.json"
    );
    assert_eq!(relative(r"C:\w\app", r"C:\w\zcode.json"), "../zcode.json");
    assert_eq!(relative("/w/app", "/w/app"), "app");
}

#[test]
fn the_kind_names_keep_their_values() {
    // F55：名字取共用的定义，值不变（进 hooks 的 digest，与 Node 逐字节一致）。
    assert_eq!(ProjectKind::ZcodeJson.as_str(), "zcode.json");
    assert_eq!(ProjectKind::DotZcodeConfig.as_str(), ".zcode/config.json");
    assert_eq!(DOT_ZCODE_CONFIG, format!("{DOT_DIR}/{CONFIG_JSON}"));
}

#[tokio::test]
async fn a_dot_git_that_is_neither_file_nor_directory_is_not_a_marker() {
    // Node `hasWorktreeMarker`：`isDirectory() || isFile()`；FIFO 等别的种类不算（F58：以前任何种类都算）。
    let env = env(&[
        ("/Users/dev/repo/.git/HEAD", "ref"),
        ("/Users/dev/repo/zcode.json", "{}"),
    ])
    .with_special("/Users/dev/repo/app/.git");
    let files = discover(&env, &workspace("/Users/dev/repo/app")).await;
    let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["/Users/dev/repo/zcode.json"]);
}
