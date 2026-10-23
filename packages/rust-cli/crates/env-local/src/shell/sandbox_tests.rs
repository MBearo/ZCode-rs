//! sandbox 后端的参数（纯字符串，三平台都跑）与 macOS 上的实际拦截。

use std::path::Path;

use rust_cli_kernel_api::env::{EnvError, EnvPath};
use rust_cli_kernel_api::env_shell::{FilesystemPolicy, NetworkPolicy, SandboxPolicy};

use super::*;

fn policy(network: NetworkPolicy, required: bool) -> SandboxPolicy {
    SandboxPolicy {
        filesystem: FilesystemPolicy {
            allow_write: vec![EnvPath::new("/w"), EnvPath::new("/tmp")],
            deny_write: vec![EnvPath::new("/w/secret")],
            deny_read: vec![EnvPath::new("/home/dev/.ssh")],
        },
        network,
        required,
    }
}

#[test]
fn macos_profile_keeps_the_base_and_adds_rules() {
    let profile = macos_profile(
        &policy(NetworkPolicy::Unrestricted, false),
        Path::new("/w"),
        "TAG",
    );
    assert!(
        profile
            .starts_with("(version 1)\n(deny default (with message \"TAG\"))\n\n; LogTag: TAG\n")
    );
    assert!(profile.contains("\n; Network\n(allow network*)\n\n; File read\n(allow file-read*)\n"));
    assert!(
        profile.contains(
            "(deny file-read*\n  (subpath \"/home/dev/.ssh\")\n  (with message \"TAG\"))"
        )
    );
    assert!(profile.contains(
        "(allow file-write*\n  (subpath \"/w\")\n  (subpath \"/tmp\")\n  (with message \"TAG\"))"
    ));
    assert!(profile.contains("  (subpath \"/w/secret\")\n"));
    assert!(profile.contains("  (subpath \"/w/.bashrc\")\n"));
    assert!(profile.contains(r#"  (regex "^/w/(.*/)?\\.git/hooks(/.*)?$")"#));
    assert!(!profile.contains("{log_tag}"));
}

#[test]
fn restricted_network_only_reaches_localhost_when_binding_is_allowed() {
    let blocked = macos_profile(
        &policy(
            NetworkPolicy::LocalOnly {
                allow_local_binding: false,
            },
            false,
        ),
        Path::new("/w"),
        "T",
    );
    assert!(blocked.contains("; Network\n\n; File read"));
    let local = macos_profile(
        &policy(
            NetworkPolicy::LocalOnly {
                allow_local_binding: true,
            },
            false,
        ),
        Path::new("/w"),
        "T",
    );
    assert!(local.contains("(allow network-outbound (remote ip \"localhost:*\"))"));
}

#[test]
fn bwrap_arguments_follow_the_fixed_layout() {
    let (args, _) = bwrap_args(
        &policy(
            NetworkPolicy::LocalOnly {
                allow_local_binding: false,
            },
            false,
        ),
        Path::new("/bin/bash"),
        "eval 'ls'",
        Path::new("/w"),
    );
    assert_eq!(
        &args[..3],
        ["--new-session", "--die-with-parent", "--unshare-net"]
    );
    assert_eq!(&args[3..6], ["--ro-bind", "/", "/"]);
    let tail: Vec<&str> = args
        .iter()
        .rev()
        .take(10)
        .rev()
        .map(String::as_str)
        .collect();
    assert_eq!(
        tail,
        [
            "--dev",
            "/dev",
            "--unshare-pid",
            "--unshare-user",
            "--proc",
            "/proc",
            "--",
            "/bin/bash",
            "-c",
            "eval 'ls'"
        ]
    );
}

#[test]
fn a_required_sandbox_without_a_backend_refuses_to_run() {
    let refused = launch(
        Some(&policy(NetworkPolicy::Unrestricted, true)),
        &Backend::Unavailable,
        Path::new("/bin/bash"),
        "ls",
        Path::new("/w"),
        "T",
    );
    assert!(matches!(refused, Err(EnvError::PermissionDenied { .. })));
    let plain = launch(
        Some(&policy(NetworkPolicy::Unrestricted, false)),
        &Backend::Unavailable,
        Path::new("/bin/bash"),
        "ls",
        Path::new("/w"),
        "T",
    )
    .unwrap();
    assert_eq!(plain.args, ["-c", "ls"]);
}

/// macOS 上真的套一层 sandbox-exec：可写目录里能写，外面不能。
#[cfg(target_os = "macos")]
#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "测试准备目录并直接启动 sandbox-exec"
)]
fn macos_sandbox_blocks_writes_outside_the_allowed_roots() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let inside = base.join("w");
    std::fs::create_dir_all(&inside).unwrap();
    let rules = SandboxPolicy {
        filesystem: FilesystemPolicy {
            allow_write: vec![EnvPath::new(inside.to_string_lossy())],
            ..FilesystemPolicy::default()
        },
        network: NetworkPolicy::Unrestricted,
        required: true,
    };
    let script = format!(
        "touch '{}/ok' && touch '{}/no'",
        inside.display(),
        base.display()
    );
    let launch = launch(
        Some(&rules),
        &Backend::MacOs,
        Path::new("/bin/bash"),
        &script,
        &inside,
        "T",
    )
    .unwrap();
    let status = rust_cli_proc::std_command(&launch.program)
        .args(&launch.args)
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
    assert!(inside.join("ok").exists());
    assert!(!base.join("no").exists());
}

/// 缺陷 R2-16：bwrap 原来没有 macOS 那张保护表，工作目录可写时 sandbox 里的命令能改 `.git/hooks`、
/// `.bashrc` 等。现在可写的工作目录里：存在的只读绑定，不存在的在第一个不存在的那一级绑 /dev/null。
#[cfg(unix)]
#[test]
#[expect(clippy::disallowed_methods, reason = "测试准备工作目录")]
fn bwrap_protects_the_same_paths_as_macos() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::create_dir_all(cwd.join(".git/hooks")).unwrap();
    std::fs::write(cwd.join(".bashrc"), "").unwrap();
    let root = cwd.to_string_lossy().into_owned();
    let rules = SandboxPolicy {
        filesystem: FilesystemPolicy {
            allow_write: vec![EnvPath::new(&root)],
            ..FilesystemPolicy::default()
        },
        network: NetworkPolicy::Unrestricted,
        required: true,
    };
    let backend = Backend::Linux {
        bwrap: PathBuf::from("/usr/bin/bwrap"),
    };
    let launched = launch(
        Some(&rules),
        &backend,
        Path::new("/bin/bash"),
        "ls",
        &cwd,
        "T",
    )
    .unwrap();
    let args = launched.args;
    let binds: Vec<[&str; 3]> = args
        .windows(3)
        .filter(|w| w[0] == "--ro-bind" && w[2] != "/")
        .map(|w| [w[0].as_str(), w[1].as_str(), w[2].as_str()])
        .collect();
    let at = |rel: &str| format!("{root}/{rel}");
    let (bashrc, hooks, vscode, zcode, config) = (
        at(".bashrc"),
        at(".git/hooks"),
        at(".vscode"),
        at(".zcode"),
        at(".git/config"),
    );
    for expected in [
        ["--ro-bind", bashrc.as_str(), bashrc.as_str()],
        ["--ro-bind", hooks.as_str(), hooks.as_str()],
        ["--ro-bind", "/dev/null", vscode.as_str()],
        ["--ro-bind", "/dev/null", config.as_str()],
    ] {
        assert!(binds.contains(&expected), "{expected:?} in {binds:?}");
    }
    // `.zcode/commands` 与 `.zcode/agents` 都缺 `.zcode`：只绑一次。
    let zcode_binds = binds.iter().filter(|b| b[2] == zcode).count();
    assert_eq!(zcode_binds, 1);
    // 可写绑定在前，保护在后（后面的挂载覆盖前面的）。
    let writable = args.iter().position(|a| a == "--bind").unwrap();
    let protected = args.iter().position(|a| a == &hooks).unwrap();
    assert!(writable < protected);
    // 绑 /dev/null 的路径 bwrap 会在宿主上建出空文件，交给命令结束后清理（mounts.rs）。
    let created = launched.mounts.unwrap();
    for path in [&vscode, &zcode, &config] {
        assert!(
            created.contains(&PathBuf::from(path)),
            "{path} in {created:?}"
        );
    }
    assert_eq!(
        created.len(),
        created
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    );
    assert!(!created.contains(&PathBuf::from(&hooks)));
}

/// 工作目录不可写时根本来就是只读绑定：不加保护（bwrap 也建不出挂载点）。
#[test]
fn bwrap_adds_no_protection_outside_writable_roots() {
    let rules = SandboxPolicy {
        filesystem: FilesystemPolicy::default(),
        network: NetworkPolicy::Unrestricted,
        required: true,
    };
    let backend = Backend::Linux {
        bwrap: PathBuf::from("/usr/bin/bwrap"),
    };
    let args = launch(
        Some(&rules),
        &backend,
        Path::new("/bin/bash"),
        "ls",
        Path::new("/w"),
        "T",
    )
    .unwrap()
    .args;
    assert!(!args.iter().any(|a| a == "/dev/null"));
}

/// git worktree 里 `.git` 是文件：它下面建不出挂载点，跳过“祖先是文件”的路径。
#[cfg(unix)]
#[test]
#[expect(clippy::disallowed_methods, reason = "测试准备工作目录")]
fn bwrap_skips_paths_under_a_git_file() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(cwd.join(".git"), "gitdir: /elsewhere\n").unwrap();
    let rules = SandboxPolicy {
        filesystem: FilesystemPolicy {
            allow_write: vec![EnvPath::new(cwd.to_string_lossy())],
            ..FilesystemPolicy::default()
        },
        network: NetworkPolicy::Unrestricted,
        required: true,
    };
    let (args, created) = bwrap_args(&rules, Path::new("/bin/bash"), "ls", &cwd);
    assert!(
        !created
            .iter()
            .any(|path| path.starts_with(cwd.join(".git")))
    );
    let under = format!("{}/.git/", cwd.display());
    assert!(!args.iter().any(|a| a.starts_with(&under)), "{args:?}");
}

#[test]
fn the_protected_directories_keep_their_names() {
    // F55：名字取 base 的定义；值是固定的目录表（产品目录是 `.zcode`）。
    use rust_cli_base::names::{AGENTS_DIR, DOT_DIR};
    assert_eq!(
        PROTECTED_DIRS,
        [".vscode", ".idea", ".zcode/commands", ".zcode/agents"]
    );
    assert!(PROTECTED_DIRS.contains(&format!("{DOT_DIR}/{AGENTS_DIR}").as_str()));
}
