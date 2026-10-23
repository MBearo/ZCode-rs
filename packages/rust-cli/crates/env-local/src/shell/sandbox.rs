//! sandbox 后端（docs/specs/rust-cli/shell-permissions.md §2.4）：macOS 的 sandbox profile 与 Linux 的 bwrap 参数。
//!
//! - macOS：`/usr/bin/sandbox-exec -p <profile> <bash> -c <script>`。profile 的基段是固定文本
//!   （`macos-base.sb`），后接网络段、读段、写段；
//! - Linux：`bwrap --new-session --die-with-parent [--unshare-net] --ro-bind / / --bind <可写> …
//!   --dev /dev --unshare-pid --unshare-user --proc /proc -- <bash> -c <script>`；
//! - 其他平台没有后端：`required` 时拒绝执行，否则不套 sandbox。

use std::path::{Path, PathBuf};

use rust_cli_base::include_lf;
use rust_cli_kernel_api::env::EnvError;
use rust_cli_kernel_api::env_shell::{NetworkPolicy, SandboxPolicy};

/// profile 的基段（`; Network` 之前的部分），`{log_tag}` 是占位符。
const MACOS_BASE: &str = include_lf!("macos-base.sb");

/// 工作目录里始终禁止写的文件。
const PROTECTED_FILES: &[&str] = &[
    ".gitconfig",
    ".gitmodules",
    ".bashrc",
    ".bash_profile",
    ".zshrc",
    ".zprofile",
    ".profile",
    ".ripgreprc",
    ".mcp.json",
];
/// 工作目录里始终禁止写的目录（产品目录是 `.zcode`）。`const` 里只能用
/// `dot_dir!()`；`agents` 与子代理目录 `names::AGENTS_DIR` 相同，测试核对（F55）。
const PROTECTED_DIRS: &[&str] = &[
    ".vscode",
    ".idea",
    concat!(rust_cli_base::dot_dir!(), "/commands"),
    concat!(rust_cli_base::dot_dir!(), "/agents"),
];

/// 可用的后端。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Backend {
    MacOs,
    Linux { bwrap: PathBuf },
    Unavailable,
}

/// 实际启动的程序与参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// 套了 Linux sandbox 时为 Some：bwrap 会在宿主上建的挂载点（保护表里原来不存在的路径），命令结束后经
    /// `mounts` 清理（可能为空）。其他情况为 None。
    pub mounts: Option<Vec<PathBuf>>,
}

fn quote(value: &str) -> String {
    serde_json::Value::String(value.to_owned()).to_string()
}

fn ruleset(behavior: &str, operations: &str, filters: &[String], tag: &str) -> Vec<String> {
    if filters.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![format!("({behavior} {operations}")];
    lines.extend(filters.iter().map(|filter| format!("  {filter}")));
    lines.push(format!("  (with message \"{tag}\"))"));
    lines
}

fn subpath(path: &Path) -> String {
    format!("(subpath {})", quote(&path.to_string_lossy()))
}

/// glob（`**`、`*`）换成锚定的正则：`**/` 可匹配零到多级目录。
fn glob_regex(glob: &str) -> String {
    let mut out = String::from("^");
    let mut rest = glob;
    while let Some(c) = rest.chars().next() {
        if let Some(tail) = rest.strip_prefix("/**/") {
            out.push_str("/(.*/)?");
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix("**/") {
            out.push_str("(.*/)?");
            rest = tail;
        } else if rest == "/**" {
            out.push_str("(/.*)?");
            rest = "";
        } else if c == '*' {
            out.push_str("[^/]*");
            rest = &rest[1..];
        } else {
            if ".+?^${}()|[]\\".contains(c) {
                out.push('\\');
            }
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out.push('$');
    out
}

fn regex(glob: &str) -> String {
    format!("(regex {})", quote(&glob_regex(glob)))
}

/// 工作目录里强制禁止写的路径：（相对工作目录的路径，是不是目录）。macOS 与 Linux 共用
/// 这一张表。
fn protected_entries() -> impl Iterator<Item = (&'static str, bool)> {
    let files = PROTECTED_FILES.iter().map(|file| (*file, false));
    let dirs = PROTECTED_DIRS
        .iter()
        .chain([&".git/hooks"])
        .map(|dir| (*dir, true));
    files.chain(dirs).chain([(".git/config", false)])
}

/// macOS：每项一个字面路径加一个 `**` 形式（目录再加 `/**`）。
fn protected(cwd: &Path) -> Vec<String> {
    let root = cwd.to_string_lossy();
    protected_entries()
        .flat_map(|(entry, dir)| {
            let glob = if dir {
                format!("{root}/**/{entry}/**")
            } else {
                format!("{root}/**/{entry}")
            };
            [subpath(&cwd.join(entry)), regex(&glob)]
        })
        .collect()
}

fn network(policy: &NetworkPolicy) -> Vec<String> {
    match policy {
        NetworkPolicy::Unrestricted => vec!["(allow network*)".to_owned()],
        // 按域名放行要经本地代理过滤（HTTP / SOCKS 代理）；没有代理时按只连本机处理。
        NetworkPolicy::LocalOnly {
            allow_local_binding: false,
        }
        | NetworkPolicy::Domains { .. } => Vec::new(),
        NetworkPolicy::LocalOnly {
            allow_local_binding: true,
        } => vec![
            "(allow network-bind (local ip \"*:*\"))".to_owned(),
            "(allow network-inbound (local ip \"*:*\"))".to_owned(),
            "(allow network-outbound (remote ip \"localhost:*\"))".to_owned(),
        ],
    }
}

/// macOS 的 sandbox profile。
pub(crate) fn macos_profile(policy: &SandboxPolicy, cwd: &Path, tag: &str) -> String {
    let base = MACOS_BASE.strip_suffix('\n').unwrap_or(MACOS_BASE);
    let mut lines = vec![base.replace("{log_tag}", tag), "; Network".to_owned()];
    lines.extend(network(&policy.network));
    lines.push(String::new());
    lines.push("; File read".to_owned());
    lines.push("(allow file-read*)".to_owned());
    let fs = &policy.filesystem;
    let denied: Vec<String> = fs
        .deny_read
        .iter()
        .map(|path| subpath(Path::new(path.as_str())))
        .collect();
    lines.extend(ruleset("deny", "file-read*", &denied, tag));
    if !denied.is_empty() {
        lines.push("(allow file-read-metadata".to_owned());
        lines.push("  (vnode-type DIRECTORY))".to_owned());
    }
    lines.push(String::new());
    lines.push("; File write".to_owned());
    let writable: Vec<String> = fs
        .allow_write
        .iter()
        .map(|path| subpath(Path::new(path.as_str())))
        .collect();
    lines.extend(ruleset("allow", "file-write*", &writable, tag));
    let mut deny: Vec<String> = fs
        .deny_write
        .iter()
        .map(|path| subpath(Path::new(path.as_str())))
        .collect();
    deny.extend(protected(cwd));
    lines.extend(ruleset("deny", "file-write*", &deny, tag));
    lines.join("\n")
}

#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是进程与 sandbox 的入口：bwrap 只能绑定已存在的路径（spec §4）"
)]
fn exists(path: &Path) -> bool {
    path.exists()
}

#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是进程与 sandbox 的入口：读禁止的目录换成 tmpfs，文件换成 /dev/null（spec §4）"
)]
fn is_dir(path: &Path) -> bool {
    path.is_dir()
}

/// `path` 从上往下第一个不存在的一级；都存在时为 None。
fn first_missing(path: &Path) -> Option<&Path> {
    let mut ancestors: Vec<&Path> = path.ancestors().collect();
    ancestors.reverse();
    ancestors.into_iter().find(|ancestor| !exists(ancestor))
}

/// Linux：保护表在 bwrap 里的绑定（deny-write 路径的处理）。
///
/// 缺陷修复 R2-16：原来 bwrap 没有 macOS 那张保护表，工作目录可写时 sandbox 里的命令能改 `.git/hooks`、
/// `.bashrc`、`.zcode/commands` 等。现在只处理落在可写路径里的项（根是只读绑定，其余本来就写不了，bwrap
/// 也建不出挂载点）：存在的只读绑定自身；不存在的在第一个不存在的那一级绑定 `/dev/null`，命令建不出它
/// （缺的是上级目录时改绑一个空目录的做法见 shell-permissions.md D4）；那一级的上级是文件
/// （git worktree 的 `.git`）时跳过。bwrap 不支持通配，`**` 的嵌套位置不做（D4）。返回参数与 bwrap 会在宿主上
/// 建的挂载点（绑 `/dev/null` 的那些，bwrap 在可写的工作目录里留下空文件，命令结束后清理，见 `mounts`）。
fn protected_binds(policy: &SandboxPolicy, cwd: &Path) -> (Vec<String>, Vec<PathBuf>) {
    let writable = |path: &Path| {
        policy
            .filesystem
            .allow_write
            .iter()
            .any(|root| path.starts_with(root.as_str()))
    };
    let mut bound: Vec<PathBuf> = Vec::new();
    let mut created = Vec::new();
    let mut args = Vec::new();
    for (entry, _) in protected_entries() {
        let path = cwd.join(entry);
        if !writable(&path) {
            continue;
        }
        let (source, target) = match first_missing(&path) {
            None => (path.clone(), path),
            Some(missing) if missing.parent().is_some_and(is_dir) => {
                (PathBuf::from("/dev/null"), missing.to_path_buf())
            }
            Some(_) => continue,
        };
        if bound.contains(&target) {
            continue;
        }
        if source != target {
            created.push(target.clone());
        }
        args.extend([
            "--ro-bind".to_owned(),
            source.to_string_lossy().into_owned(),
            target.to_string_lossy().into_owned(),
        ]);
        bound.push(target);
    }
    (args, created)
}

/// Linux 的 bwrap 参数（不含 seccomp 与代理桥）与 bwrap 会建的挂载点。
pub(crate) fn bwrap_args(
    policy: &SandboxPolicy,
    bash: &Path,
    script: &str,
    cwd: &Path,
) -> (Vec<String>, Vec<PathBuf>) {
    let mut args: Vec<String> = ["--new-session", "--die-with-parent"]
        .map(str::to_owned)
        .to_vec();
    if !matches!(policy.network, NetworkPolicy::Unrestricted) {
        args.push("--unshare-net".to_owned());
    }
    let pair = |flag: &str, from: &str, to: &str| [flag.to_owned(), from.to_owned(), to.to_owned()];
    args.extend(pair("--ro-bind", "/", "/"));
    let fs = &policy.filesystem;
    for path in fs
        .allow_write
        .iter()
        .filter(|p| exists(Path::new(p.as_str())))
    {
        args.extend(pair("--bind", path.as_str(), path.as_str()));
    }
    for path in fs
        .deny_write
        .iter()
        .filter(|p| exists(Path::new(p.as_str())))
    {
        args.extend(pair("--ro-bind", path.as_str(), path.as_str()));
    }
    let (protected, created) = protected_binds(policy, cwd);
    args.extend(protected);
    for path in fs
        .deny_read
        .iter()
        .filter(|p| exists(Path::new(p.as_str())))
    {
        if is_dir(Path::new(path.as_str())) {
            args.extend(["--tmpfs".to_owned(), path.as_str().to_owned()]);
        } else {
            args.extend(pair("--ro-bind", "/dev/null", path.as_str()));
        }
    }
    args.extend(
        [
            "--dev",
            "/dev",
            "--unshare-pid",
            "--unshare-user",
            "--proc",
            "/proc",
        ]
        .map(str::to_owned),
    );
    args.extend([
        "--".to_owned(),
        bash.to_string_lossy().into_owned(),
        "-c".to_owned(),
        script.to_owned(),
    ]);
    (args, created)
}

/// 按 policy 与后端决定怎样启动。没有 policy 时直接 `<bash> -c <script>`。
pub(crate) fn launch(
    policy: Option<&SandboxPolicy>,
    backend: &Backend,
    bash: &Path,
    script: &str,
    cwd: &Path,
    tag: &str,
) -> Result<Launch, EnvError> {
    let plain = || Launch {
        program: bash.to_path_buf(),
        args: vec!["-c".to_owned(), script.to_owned()],
        mounts: None,
    };
    let Some(policy) = policy else {
        return Ok(plain());
    };
    match backend {
        Backend::MacOs => Ok(Launch {
            program: PathBuf::from("/usr/bin/sandbox-exec"),
            args: vec![
                "-p".to_owned(),
                macos_profile(policy, cwd, tag),
                bash.to_string_lossy().into_owned(),
                "-c".to_owned(),
                script.to_owned(),
            ],
            mounts: None,
        }),
        Backend::Linux { bwrap } => {
            let (args, created) = bwrap_args(policy, bash, script, cwd);
            Ok(Launch {
                program: bwrap.clone(),
                args,
                mounts: Some(created),
            })
        }
        Backend::Unavailable if policy.required => Err(EnvError::PermissionDenied {
            message: "This call must run inside a fully confining sandbox and none is available here. The command was not run.".to_owned(),
        }),
        Backend::Unavailable => Ok(plain()),
    }
}

#[cfg(test)]
#[path = "sandbox_tests.rs"]
mod tests;
