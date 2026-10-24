//! `/export` 的路径解析与写入（tui-manage.md §3.1）。
use rust_cli_kernel_api::env::{EnvFacts, ReadFile, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::testing::{MemoryEnvironment, SequentialIds};

use super::*;

fn root() -> EnvPath {
    EnvPath::new("/Users/dev/project")
}

#[test]
fn paths_get_a_txt_extension_and_resolve_from_the_workspace() {
    let home = EnvPath::new("/Users/dev");
    assert_eq!(target("chat", &root(), None), "/Users/dev/project/chat.txt");
    assert_eq!(
        target(" notes/chat.md ", &root(), None),
        "/Users/dev/project/notes/chat.md"
    );
    assert_eq!(
        target("~/out/chat", &root(), Some(&home)),
        "/Users/dev/out/chat.txt"
    );
    assert_eq!(target("/tmp/a.log", &root(), None), "/tmp/a.log");
    assert_eq!(target("../up", &root(), None), "/Users/dev/up.txt");
}

#[tokio::test]
async fn the_text_is_written_through_the_environment() {
    let env = MemoryEnvironment::new(EnvFacts {
        cwd: root(),
        platform: "darwin".to_owned(),
        os_version: String::new(),
        shell: "zsh".to_owned(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    });
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: root(),
    };
    let path = write(
        &env,
        &env,
        &SequentialIds::default(),
        &Generation::new("gen_1"),
        ExportFile {
            workspace: workspace.clone(),
            path: "~/exports/chat".to_owned(),
            text: "❯ hi\n".to_owned(),
        },
    )
    .await
    .unwrap();
    assert_eq!(path, "/Users/dev/exports/chat.txt");
    let read = env
        .read_file(
            &workspace,
            ReadFile {
                path: EnvPath::new(path.as_str()),
            },
        )
        .await
        .unwrap();
    assert_eq!(read.bytes, "❯ hi\n".as_bytes());
}
