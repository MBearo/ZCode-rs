//! 找 shell（docs/specs/rust-cli/shell-permissions.md §2.5，工作线 F44）：找法、探测与 Git Bash 的位置
//! 按表逐条核对。本机事实（能不能执行、PATH 里有没有）换成表里的假数据，所以 Windows 的分支在任何主机上都能跑。
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use super::*;

/// 假的本机：能执行的路径、`which` 的结果、存在的文件；记下 `executable` 被问过的路径。
#[derive(Default)]
struct Fake {
    executable: HashSet<&'static str>,
    which: HashMap<&'static str, &'static str>,
    exists: HashSet<&'static str>,
    asked: Mutex<Vec<String>>,
}

impl Fake {
    fn new(executable: &[&'static str], which: &[(&'static str, &'static str)]) -> Self {
        Self {
            executable: executable.iter().copied().collect(),
            which: which.iter().copied().collect(),
            ..Self::default()
        }
    }

    fn files(exists: &[&'static str]) -> Self {
        Self {
            exists: exists.iter().copied().collect(),
            ..Self::default()
        }
    }
}

impl Host for Fake {
    async fn executable(&self, path: &str) -> bool {
        self.asked.lock().unwrap().push(path.to_owned());
        self.executable.contains(path)
    }

    fn which(&self, name: &str) -> Option<String> {
        self.which.get(name).map(|path| (*path).to_owned())
    }

    fn exists(&self, path: &str) -> bool {
        self.exists.contains(path)
    }
}

fn env(vars: &[(&str, &str)]) -> Vec<(String, String)> {
    vars.iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

/// 一行：说明、环境变量、能执行的路径、`which` 的结果、期望。
struct Case {
    name: &'static str,
    env: &'static [(&'static str, &'static str)],
    executable: &'static [&'static str],
    which: &'static [(&'static str, &'static str)],
    expected: Option<&'static str>,
}

const CASES: &[Case] = &[
    Case {
        name: "SHELL 是 bash 且能执行：用它",
        env: &[("SHELL", "/bin/bash")],
        executable: &["/bin/bash", "/bin/zsh", "/usr/bin/zsh"],
        which: &[("zsh", "/usr/bin/zsh"), ("bash", "/usr/bin/bash")],
        expected: Some("/bin/bash"),
    },
    Case {
        name: "SHELL 是 zsh 且能执行：用它",
        env: &[("SHELL", "/bin/zsh")],
        executable: &["/bin/bash", "/bin/zsh"],
        which: &[("zsh", "/usr/bin/zsh"), ("bash", "/usr/bin/bash")],
        expected: Some("/bin/zsh"),
    },
    Case {
        name: "没有 SHELL：zsh 在前，which 的结果在固定目录之前",
        env: &[],
        executable: &["/bin/zsh", "/usr/local/bin/zsh", "/bin/bash"],
        which: &[("zsh", "/usr/local/bin/zsh"), ("bash", "/bin/bash")],
        expected: Some("/usr/local/bin/zsh"),
    },
    Case {
        name: "没有 SHELL、只有 bash（debian）：固定目录的 /bin/bash 在 which bash 之前",
        env: &[],
        executable: &["/bin/bash", "/usr/bin/bash"],
        which: &[("bash", "/usr/bin/bash")],
        expected: Some("/bin/bash"),
    },
    Case {
        name: "没有 SHELL、bash 只在 PATH 里（NixOS）：用 which bash",
        env: &[],
        executable: &["/run/current-system/sw/bin/bash"],
        which: &[("bash", "/run/current-system/sw/bin/bash")],
        expected: Some("/run/current-system/sw/bin/bash"),
    },
    Case {
        name: "SHELL 是 sh（不支持）：当作没有，zsh 在前",
        env: &[("SHELL", "/bin/sh")],
        executable: &["/bin/sh", "/bin/zsh", "/bin/bash"],
        which: &[],
        expected: Some("/bin/zsh"),
    },
    Case {
        name: "SHELL 指向不存在的 bash：仍然 bash 在前，用 which bash",
        env: &[("SHELL", "/usr/local/bin/bash")],
        executable: &["/usr/bin/bash", "/bin/zsh"],
        which: &[("zsh", "/bin/zsh"), ("bash", "/usr/bin/bash")],
        expected: Some("/usr/bin/bash"),
    },
    Case {
        name: "SHELL 指向不存在的路径、只有 zsh：bash 的候选都不行，用 /bin/zsh",
        env: &[("SHELL", "/nonexistent/bash")],
        executable: &["/bin/zsh"],
        which: &[],
        expected: Some("/bin/zsh"),
    },
    Case {
        name: "SHELL 不存在、bash 在前、zsh 只有 which 的结果：排在最后也能用",
        env: &[("SHELL", "/nonexistent/bash")],
        executable: &["/home/dev/.local/bin/zsh"],
        which: &[("zsh", "/home/dev/.local/bin/zsh")],
        expected: Some("/home/dev/.local/bin/zsh"),
    },
    Case {
        name: "只有 sh（alpine）：找不到",
        env: &[("SHELL", "/bin/sh")],
        executable: &["/bin/sh", "/bin/ash"],
        which: &[],
        expected: None,
    },
    Case {
        name: "什么都没有：找不到",
        env: &[],
        executable: &[],
        which: &[],
        expected: None,
    },
    Case {
        name: "/opt/homebrew/bin 也是候选目录",
        env: &[],
        executable: &["/opt/homebrew/bin/bash"],
        which: &[],
        expected: Some("/opt/homebrew/bin/bash"),
    },
    Case {
        name: "ZCODE_SHELL 能执行：盖过 SHELL",
        env: &[("ZCODE_SHELL", "/opt/tools/zsh"), ("SHELL", "/bin/bash")],
        executable: &["/opt/tools/zsh", "/bin/bash"],
        which: &[],
        expected: Some("/opt/tools/zsh"),
    },
    Case {
        name: "ZCODE_SHELL 不是 bash / zsh：不用，接着找",
        env: &[("ZCODE_SHELL", "/usr/bin/fish"), ("SHELL", "/bin/bash")],
        executable: &["/usr/bin/fish", "/bin/bash"],
        which: &[],
        expected: Some("/bin/bash"),
    },
    Case {
        name: "ZCODE_SHELL 指向不存在的 bash：不用，接着找",
        env: &[("ZCODE_SHELL", "/missing/bash"), ("SHELL", "/bin/zsh")],
        executable: &["/bin/zsh"],
        which: &[],
        expected: Some("/bin/zsh"),
    },
    Case {
        name: "空的 ZCODE_SHELL 与 SHELL 等于没有",
        env: &[("ZCODE_SHELL", ""), ("SHELL", "")],
        executable: &["/bin/bash", "/bin/zsh"],
        which: &[],
        expected: Some("/bin/zsh"),
    },
    Case {
        name: "“含 bash”是子串判断：bash5 也算",
        env: &[("SHELL", "/usr/local/bin/bash5")],
        executable: &["/usr/local/bin/bash5", "/bin/zsh"],
        which: &[],
        expected: Some("/usr/local/bin/bash5"),
    },
    Case {
        name: "裸名字的 SHELL：能执行（--version 探测）时原样用",
        env: &[("SHELL", "bash")],
        executable: &["bash", "/bin/zsh"],
        which: &[("bash", "/usr/bin/bash")],
        expected: Some("bash"),
    },
];

#[tokio::test]
async fn the_shell_is_chosen_by_the_fixed_rules() {
    for case in CASES {
        let host = Fake::new(case.executable, case.which);
        let found = bash(&env(case.env), false, &host).await;
        assert_eq!(
            found.as_deref(),
            case.expected.map(Path::new),
            "{}",
            case.name
        );
    }
}

#[tokio::test]
async fn the_candidates_are_probed_in_order() {
    // SHELL 不存在、没有 which 的结果、什么都不能执行：问过的就是完整的候选表。
    let host = Fake::new(&[], &[]);
    assert_eq!(
        bash(&env(&[("SHELL", "/missing/bash")]), false, &host).await,
        None
    );
    assert_eq!(
        *host.asked.lock().unwrap(),
        [
            "/missing/bash",
            "/bin/bash",
            "/usr/bin/bash",
            "/usr/local/bin/bash",
            "/opt/homebrew/bin/bash",
            "/bin/zsh",
            "/usr/bin/zsh",
            "/usr/local/bin/zsh",
            "/opt/homebrew/bin/zsh",
        ]
    );
}

/// Git Bash 的一行：说明、`ZCODE_GIT_BASH_PATH`、存在的文件、`which` 的结果、期望。
type GitCase = (
    &'static str,
    Option<&'static str>,
    &'static [&'static str],
    &'static [(&'static str, &'static str)],
    Option<&'static str>,
);

#[test]
fn git_bash_is_found_in_the_fixed_places() {
    const DEFAULT: &str = r"C:\Program Files\Git\bin\bash.exe";
    const X86: &str = r"C:\Program Files (x86)\Git\bin\bash.exe";
    let cases: &[GitCase] = &[
        (
            "ZCODE_GIT_BASH_PATH 是存在的 bash.exe",
            Some(r"D:\tools\Git\bin\bash.exe"),
            &[r"D:\tools\Git\bin\bash.exe", DEFAULT],
            &[],
            Some(r"D:\tools\Git\bin\bash.exe"),
        ),
        (
            "文件名不分大小写，sh.exe 也算",
            Some(r"D:\msys\usr\bin\SH.EXE"),
            &[r"D:\msys\usr\bin\SH.EXE"],
            &[],
            Some(r"D:\msys\usr\bin\SH.EXE"),
        ),
        (
            "ZCODE_GIT_BASH_PATH 不是 bash / sh：不用，接着找",
            Some(r"D:\tools\python.exe"),
            &[r"D:\tools\python.exe", DEFAULT],
            &[],
            Some(DEFAULT),
        ),
        (
            "ZCODE_GIT_BASH_PATH 不存在：接着找",
            Some(r"D:\missing\bash.exe"),
            &[X86],
            &[],
            Some(X86),
        ),
        ("缺省位置", None, &[DEFAULT, X86], &[], Some(DEFAULT)),
        ("只有 (x86) 的位置", None, &[X86], &[], Some(X86)),
        (
            "PATH 里的 git.exe 往上两级的 bin\\bash.exe",
            None,
            &[r"D:\Git\bin\bash.exe"],
            &[("git", r"D:\Git\cmd\git.exe")],
            Some(r"D:\Git\bin\bash.exe"),
        ),
        (
            "git.exe 旁边没有 bash.exe：找不到",
            None,
            &[],
            &[("git", r"D:\Git\cmd\git.exe")],
            None,
        ),
        ("什么都没有", None, &[], &[], None),
    ];
    for (name, configured, exists, which, expected) in cases {
        let mut host = Fake::files(exists);
        host.which = which.iter().copied().collect();
        assert_eq!(git_bash(*configured, &host).as_deref(), *expected, "{name}");
    }
}

#[tokio::test]
async fn windows_uses_git_bash_as_the_shell_and_zcode_shell_still_wins() {
    let git = r"C:\Program Files\Git\bin\bash.exe";
    // 找到 Git Bash：当作 SHELL，排在候选表最前。
    let mut host = Fake::new(&[git, "/bin/zsh"], &[]);
    host.exists = [git].into_iter().collect();
    assert_eq!(bash(&env(&[]), true, &host).await, Some(PathBuf::from(git)));
    // 用户自己的 SHELL 不算：启动时用 Git Bash 盖掉它。
    assert_eq!(
        bash(&env(&[("SHELL", "/usr/bin/zsh")]), true, &host).await,
        Some(PathBuf::from(git))
    );
    // ZCODE_SHELL 仍然在最前。
    let custom = r"D:\msys\usr\bin\bash.exe";
    host.executable.insert(custom);
    assert_eq!(
        bash(&env(&[("ZCODE_SHELL", custom)]), true, &host).await,
        Some(PathBuf::from(custom))
    );
    // 没有 Git Bash：Bash 工具不可用，ZCODE_SHELL 也不看。
    let host = Fake::new(&[custom], &[]);
    assert_eq!(
        bash(&env(&[("ZCODE_SHELL", custom)]), true, &host).await,
        None
    );
}

#[cfg(unix)]
#[allow(
    clippy::disallowed_methods,
    reason = "测试在临时目录里准备假的 shell 文件"
)]
mod system {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn file(dir: &Path, name: &str, mode: u32) -> String {
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn host(path: &str) -> System {
        System::new(&env(&[("PATH", path)]), false)
    }

    #[test]
    fn which_takes_the_first_executable_file_and_skips_relative_entries() {
        let temp = tempfile::tempdir().unwrap();
        let (one, two, three) = (
            temp.path().join("one"),
            temp.path().join("two"),
            temp.path().join("three"),
        );
        for dir in [&one, &two, &three] {
            std::fs::create_dir(dir).unwrap();
        }
        file(&one, "zsh", 0o644);
        std::fs::create_dir(two.join("zsh")).unwrap();
        let wanted = file(&three, "zsh", 0o755);
        let path = format!(
            "relative::{}:{}:{}",
            one.display(),
            two.display(),
            three.display()
        );
        assert_eq!(host(&path).which("zsh"), Some(wanted));
        assert_eq!(host(&path).which("bash"), None);
        assert_eq!(host("").which("zsh"), None);
    }

    #[tokio::test]
    async fn executable_follows_access_then_the_version_probe() {
        let temp = tempfile::tempdir().unwrap();
        let system = host("/usr/bin:/bin");
        assert!(system.executable(&file(temp.path(), "bash", 0o755)).await);
        // 没有执行位：跑 `--version` 也是 EACCES。
        assert!(!system.executable(&file(temp.path(), "zsh", 0o644)).await);
        assert!(
            !system
                .executable(&temp.path().join("missing").to_string_lossy())
                .await
        );
        // 裸名字：access 按 cwd 找不到，`bash --version` 按 PATH 找到、退出码 0。
        assert!(system.executable("bash").await);
        assert!(!system.executable("zcode-no-such-shell").await);
    }
}

/// 内嵌的搜索工具（embedded-search.md §4、§8.1，F53）：本进程二进制的绝对路径；`find_and_grep` 照入口给的值。
#[expect(clippy::disallowed_methods, reason = "测试核对取到的路径是个文件")]
#[tokio::test]
async fn the_config_carries_the_embedded_search_tools() {
    let temp = tempfile::tempdir().unwrap();
    for find_and_grep in [true, false] {
        let config = config(EnvSnapshot::default(), temp.path(), find_and_grep).await;
        let search = config.embedded.expect("测试进程的二进制路径总能取到");
        assert!(search.executable.is_absolute(), "{search:?}");
        assert!(search.executable.is_file(), "{search:?}");
        assert_eq!(search.find_and_grep, find_and_grep);
    }
}

/// `SHELL` 的唯一取值（F58）：Windows 上找到的 Git Bash 写进快照，环境段、git 与 MCP 都读它；没有找到
/// 或不是 Windows 时快照不变。
#[test]
fn the_snapshot_carries_the_git_bash_as_shell() {
    let git = r"C:\Program Files\Git\bin\bash.exe";
    let snapshot = EnvSnapshot::from(env(&[("SHELL", "/usr/bin/zsh")]));
    assert_eq!(
        shell_snapshot(snapshot.clone(), Some(git)).get("SHELL"),
        Some(git)
    );
    assert_eq!(
        shell_snapshot(EnvSnapshot::default(), Some(git)).get("SHELL"),
        Some(git)
    );
    assert_eq!(
        shell_snapshot(snapshot, None).get("SHELL"),
        Some("/usr/bin/zsh")
    );
}

#[tokio::test]
async fn git_bash_is_found_once_for_the_shell_and_the_snapshot() {
    let git = r"C:\Program Files\Git\bin\bash.exe";
    let mut host = Fake::new(&[git], &[]);
    host.exists = [git].into_iter().collect();
    assert_eq!(
        resolve(&env(&[]), true, &host).await,
        (Some(git.to_owned()), Some(PathBuf::from(git)))
    );
    assert_eq!(
        resolve(&env(&[("SHELL", "/bin/zsh")]), false, &host)
            .await
            .0,
        None
    );
}
