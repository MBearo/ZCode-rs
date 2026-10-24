//! 工作目录围栏（remote-exec.md §18.6）：只许请求里的路径落在给定的根目录里。路径按字面规范化（`.`、`..`、重复的
//! 分隔符），不解析符号链接。R1 的执行器不设围栏（同一个用户，Bash 本来就能访问一切）；云端多租户时打开。

use rust_cli_exec_wire::envelope::WireError;
use rust_cli_exec_wire::request::Request;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Fence {
    /// 不检查。
    #[default]
    Open,
    /// 只许这些根目录（绝对路径）里的路径。
    Roots(Vec<String>),
}

/// 按字面规范化：分隔符认 `/` 与 `\`，去掉 `.`，`..` 退一层（不能退出根）；不是绝对路径时 None。
fn normalize(path: &str) -> Option<Vec<String>> {
    let absolute =
        path.starts_with('/') || path.starts_with('\\') || path.as_bytes().get(1) == Some(&b':');
    if !absolute {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other.to_owned()),
        }
    }
    Some(parts)
}

impl Fence {
    fn allows(&self, path: &str) -> bool {
        let Self::Roots(roots) = self else {
            return true;
        };
        let Some(path) = normalize(path) else {
            return false;
        };
        roots.iter().any(|root| {
            normalize(root)
                .is_some_and(|root| path.len() >= root.len() && path[..root.len()] == root)
        })
    }

    /// 检查一个请求里的全部路径。
    pub fn check(&self, request: &Request) -> Result<(), WireError> {
        if matches!(self, Self::Open) {
            return Ok(());
        }
        match paths(request).into_iter().find(|path| !self.allows(path)) {
            Some(path) => Err(WireError::PermissionDenied {
                message: format!("{path} is outside the executor's working directories"),
            }),
            None => Ok(()),
        }
    }
}

/// 请求里会被访问的路径：工作区根、文件与搜索的路径、命令的 cwd、回退的文件、PDF。
fn paths(request: &Request) -> Vec<&str> {
    match request {
        Request::Facts { workspace }
        | Request::PathFacts { workspace }
        | Request::ShellState { workspace, .. } => vec![&workspace.root],
        Request::ReadFile { workspace, path }
        | Request::Stat { workspace, path }
        | Request::ListDir { workspace, path }
        | Request::WriteFile {
            workspace, path, ..
        }
        | Request::PdfPageCount { workspace, path } => vec![&workspace.root, path],
        Request::ReadText { workspace, request } => vec![&workspace.root, &request.path],
        Request::StatMany { workspace, paths } => std::iter::once(workspace.root.as_str())
            .chain(paths.iter().map(String::as_str))
            .collect(),
        Request::PutFile {
            workspace, request, ..
        } => vec![&workspace.root, &request.path],
        Request::Glob { workspace, request } => vec![&workspace.root, &request.root],
        Request::ListFiles { workspace, cwd } => vec![&workspace.root, cwd],
        Request::Grep { workspace, request } => vec![&workspace.root, &request.root],
        Request::StartCommand {
            workspace, request, ..
        } => vec![&workspace.root, &request.cwd],
        Request::SetShellCwd { cwd, .. } => vec![cwd],
        Request::RewindPrepare {
            workspace, files, ..
        } => std::iter::once(workspace.root.as_str())
            .chain(files.iter().map(|file| file.path.as_str()))
            .collect(),
        Request::RenderPdfPages { workspace, request } => vec![&workspace.root, &request.path],
        Request::WatchCommand { .. }
        | Request::Cancel { .. }
        | Request::OperationStatus { .. }
        | Request::CloseSession { .. }
        | Request::ShellNotices { .. }
        | Request::AckShellNotice { .. }
        | Request::BackgroundCommand { .. }
        | Request::BackgroundElapsed { .. }
        | Request::StopBackgroundCommand { .. }
        | Request::RewindApply { .. }
        | Request::RewindStatus { .. }
        | Request::RewindFinish { .. } => Vec::new(),
    }
}
