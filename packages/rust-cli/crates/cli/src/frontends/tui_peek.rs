//! `@` 面板的文件预览（界面的 `Services::peek_file`；迁自已删除的 Puffin host 的 `mentions.rs`，规则照 feat/tui 的
//! packages/tui/host/src/mentionsHost.ts）：只读本机工作区里的文件，经 rust-cli-fs 读（数到 8 MB 的行、前 4 行、是否
//! 二进制）；远端的工作区没有读文件的接缝，回答 `Missing`。
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_tui_kit::app::widgets::at_panel::data::PathPeek;
use tokio::io::AsyncReadExt;

/// 预览只数这么多字节里的行。
const PEEK_MAX_BYTES: u64 = 8 * 1024 * 1024;
const PEEK_SLICE_BYTES: usize = 256 * 1024;
const PEEK_HEAD_LINES: usize = 4;
/// 开头几行最多攒这么多字节。
const PEEK_HEAD_BYTES: usize = 64 * 1024;

/// 工作区里相对它的 `path` 的预览：本机工作区读文件，远端的是 `Missing`。
pub(crate) async fn peek(workspace: WorkspaceRef, path: String) -> PathPeek {
    if workspace.environment.as_str() != rust_cli_node_store::LOCAL_ENVIRONMENT {
        return PathPeek::Missing;
    }
    peek_file(&std::path::Path::new(workspace.root.as_str()).join(&path)).await
}

/// 本机文件的预览（TS `peekFile`）：按片读，只数换行；第一片决定是不是二进制。
async fn peek_file(path: &std::path::Path) -> PathPeek {
    let Ok(Some(bytes)) = rust_cli_fs::file_len(path).await else {
        return PathPeek::Missing;
    };
    let Ok(mut file) = rust_cli_fs::open_read(path).await else {
        return PathPeek::Missing;
    };
    let mut slice = vec![0u8; PEEK_SLICE_BYTES];
    let (mut offset, mut newlines, mut last) = (0u64, 0u64, None::<u8>);
    let mut head: Vec<u8> = Vec::new();
    loop {
        let read = match file.read(&mut slice).await {
            Ok(read) => read,
            Err(_) => return PathPeek::Missing,
        };
        let chunk = &slice[..read];
        if offset == 0 && chunk.contains(&0) {
            return PathPeek::File {
                lines: 0,
                more: false,
                binary: true,
                bytes,
                head: Vec::new(),
            };
        }
        if newlines < PEEK_HEAD_LINES as u64 && head.len() < PEEK_HEAD_BYTES {
            head.extend_from_slice(chunk);
        }
        newlines += chunk.iter().filter(|b| **b == b'\n').count() as u64;
        if let Some(byte) = chunk.last() {
            last = Some(*byte);
        }
        offset += read as u64;
        if read == 0 || offset >= bytes || offset >= PEEK_MAX_BYTES {
            break;
        }
    }
    // 最后一行没有换行时它也算一行。
    let lines = newlines + u64::from(last.is_some_and(|byte| byte != b'\n'));
    let head = String::from_utf8_lossy(&head)
        .lines()
        .take(PEEK_HEAD_LINES.min(lines as usize))
        .map(str::to_owned)
        .collect();
    PathPeek::File {
        lines,
        more: offset < bytes,
        binary: false,
        bytes,
        head,
    }
}

#[cfg(test)]
mod tests {
    use rust_cli_kernel_api::env::EnvPath;
    use rust_cli_kernel_api::ids::EnvironmentId;

    use super::*;

    fn workspace(environment: &str, root: &std::path::Path) -> WorkspaceRef {
        WorkspaceRef {
            environment: EnvironmentId::new(environment),
            root: EnvPath::new(root.display().to_string()),
        }
    }

    /// 本机文件数行、给前 4 行；二进制只说是；没有的文件与远端的工作区是 `Missing`。
    #[tokio::test]
    async fn a_peek_counts_lines_of_a_local_file() {
        let dir = tempfile::tempdir().unwrap();
        rust_cli_fs::append(
            &dir.path().join("notes.txt"),
            b"one\ntwo\nthree\nfour\nfive",
        )
        .await
        .unwrap();
        rust_cli_fs::append(&dir.path().join("blob.bin"), b"\x00\x01\x02")
            .await
            .unwrap();
        let local = || workspace(rust_cli_node_store::LOCAL_ENVIRONMENT, dir.path());
        assert_eq!(
            peek(local(), "notes.txt".to_owned()).await,
            PathPeek::File {
                lines: 5,
                more: false,
                binary: false,
                bytes: 23,
                head: ["one", "two", "three", "four"].map(str::to_owned).to_vec(),
            }
        );
        assert!(matches!(
            peek(local(), "blob.bin".to_owned()).await,
            PathPeek::File { binary: true, .. }
        ));
        assert_eq!(
            peek(local(), "gone.txt".to_owned()).await,
            PathPeek::Missing
        );
        assert_eq!(
            peek(workspace("dev-box", dir.path()), "notes.txt".to_owned()).await,
            PathPeek::Missing
        );
    }
}
