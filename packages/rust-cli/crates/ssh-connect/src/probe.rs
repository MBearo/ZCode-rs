//! 探测远端（remote-exec.md §19.9、§28.3）：一次 ssh，脚本经 stdin 交给远端的 `sh -s`。输出逐行：
//!
//! ```text
//! os Linux                 （F107：`Darwin` 是 macOS）
//! arch x86_64
//! home /home/u
//! hash sha256sum           （macOS 一般只有 `shasum`）
//! libc glibc 2.36          （F100：`getconf GNU_LIBC_VERSION`，有 glibc 时；F107：只在 Linux 上问）
//! libc musl                （F100：有 `/lib/ld-musl-*` 时；F107：只在 Linux 上问）
//! candidate <路径>\t<`<路径> executor status --json` 的第一行，可以为空>
//! ```
//!
//! 只执行名字是 `zcode-rs` 的文件：`PATH` 上的 `zcode` 可能是 Node 版 CLI，不能拿它跑 `executor …`。老版本的 `zcode-rs`
//! 不认 `executor status --json`，按未知选项报错退出（不做任何事），输出为空，算不兼容。

use rust_cli_base::names::{DOT_DIR, EXECUTOR_HOME, UNIX_EXECUTABLE};
use serde::Deserialize;

use crate::artifact::Platform;

/// 探测脚本（POSIX sh）。
///
/// F107（§28.3）：libc 两行只在 Linux 上问（macOS 上 `getconf GNU_LIBC_VERSION` 没有意义）；`readlink -f` 换成
/// `resolve()`（macOS 12.3 之前自带的 `readlink` 不认 `-f`，解析不出 npm 的包目录就找不到平台包里的执行器）。
pub(crate) fn script() -> String {
    format!(
        r#"d="$HOME/{DOT_DIR}/{EXECUTOR_HOME}"
os=$(uname -s 2>/dev/null)
printf 'os %s\n' "$os"
printf 'arch %s\n' "$(uname -m 2>/dev/null)"
printf 'home %s\n' "$HOME"
for h in sha256sum shasum openssl; do command -v "$h" >/dev/null 2>&1 && printf 'hash %s\n' "$h"; done
if [ "$os" = Linux ]; then
  g=$(getconf GNU_LIBC_VERSION 2>/dev/null) && printf 'libc %s\n' "$g"
  for m in /lib/ld-musl-*; do [ -e "$m" ] && {{ printf 'libc musl\n'; break; }}; done
fi
resolve() {{
  p=$1; n=0
  while [ -L "$p" ] && [ "$n" -lt 16 ]; do
    l=$(readlink "$p") || break
    case $l in /*) p=$l ;; *) p=$(dirname "$p")/$l ;; esac
    n=$((n+1))
  done
  printf '%s' "$p"
}}
probe() {{
  [ -f "$1" ] && [ -x "$1" ] || return 0
  r=$("$1" executor status --json </dev/null 2>/dev/null | head -n 1)
  printf 'candidate %s\t%s\n' "$1" "$r"
}}
for f in "$d"/bin/*/{UNIX_EXECUTABLE}; do probe "$f"; done
p=$(command -v {UNIX_EXECUTABLE} 2>/dev/null) && probe "$p"
z=$(command -v zcode 2>/dev/null) && {{
  r=$(resolve "$z")
  pkg=$(dirname "$(dirname "$r")")
  for f in "$pkg"/node_modules/*/bin/{UNIX_EXECUTABLE} "$pkg"/node_modules/@*/*/bin/{UNIX_EXECUTABLE}; do probe "$f"; done
}}
exit 0
"#
    )
}

/// `executor status --json` 的一行（cli 写，remote-exec.md §19.6）；只取部署要的字段，不认识的忽略。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Report {
    pub version: String,
    pub protocol: u32,
    #[serde(default)]
    pub executable: Option<String>,
    #[serde(default)]
    pub resident: Option<Resident>,
}

/// 远端在跑的常驻执行器。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Resident {
    pub version: String,
    pub protocol: u32,
    pub idle: bool,
    #[serde(default)]
    pub executable: Option<String>,
}

/// 一个候选的二进制与它的回答（不认 `status --json` 的为 None）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: String,
    pub report: Option<Report>,
}

/// 远端的 C 库（F100，remote-exec.md §26.5）：npm 的 Linux 包要 glibc。F107：macOS 的远端不问这个，一律 `Unknown`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Libc {
    /// 探测不出来（没有 `getconf`、也没有 musl 的加载器）：照常下载。
    #[default]
    Unknown,
    /// glibc 与它的版本（`getconf GNU_LIBC_VERSION` 的第二段，例如 `2.36`）。
    Glibc(String),
    /// musl（例如 Alpine）。
    Musl,
}

/// 探测的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probe {
    pub os: String,
    pub arch: String,
    pub home: String,
    /// 远端有的 sha256 程序。
    pub hashes: Vec<String>,
    /// 远端的 C 库（F100）。
    pub libc: Libc,
    pub candidates: Vec<Candidate>,
}

impl Probe {
    /// 解析探测的输出。少了 `os` / `arch` / `home` 时报错（多半不是 POSIX shell 在回答）。
    pub fn parse(output: &str) -> Result<Self, String> {
        let mut probe = Self::default();
        let mut musl = false;
        for line in output.lines() {
            let Some((key, value)) = line.split_once(' ') else {
                continue;
            };
            match key {
                "os" => probe.os = value.trim().to_owned(),
                "arch" => probe.arch = value.trim().to_owned(),
                "home" => probe.home = value.to_owned(),
                "hash" => probe.hashes.push(value.trim().to_owned()),
                // 两种都有时算 glibc（glibc 的系统上装了 musl 包）。
                "libc" => match value.trim().split_once(' ') {
                    Some(("glibc", version)) => probe.libc = Libc::Glibc(version.trim().to_owned()),
                    _ => musl |= value.trim() == "musl",
                },
                "candidate" => {
                    let (path, json) = value.split_once('\t').unwrap_or((value, ""));
                    if probe.candidates.iter().any(|known| known.path == path) {
                        continue;
                    }
                    probe.candidates.push(Candidate {
                        path: path.to_owned(),
                        report: serde_json::from_str(json.trim()).ok(),
                    });
                }
                _ => {}
            }
        }
        if musl && probe.libc == Libc::Unknown {
            probe.libc = Libc::Musl;
        }
        if probe.os.is_empty() || probe.arch.is_empty() || probe.home.is_empty() {
            return Err(format!(
                "the probe printed no system facts: {}",
                output.chars().take(200).collect::<String>()
            ));
        }
        Ok(probe)
    }

    /// 远端的目标平台（F107，§28.2）：`uname -s` 与 `uname -m` 一起定；认不出来时 None（报 `Unsupported`）。
    pub fn platform(&self) -> Option<Platform> {
        Platform::of(&self.os, &self.arch)
    }

    /// 在跑的常驻执行器（任何一个候选报告的都一样：一个用户一个）。
    pub fn resident(&self) -> Option<&Resident> {
        self.candidates
            .iter()
            .filter_map(|candidate| candidate.report.as_ref()?.resident.as_ref())
            .next()
    }

    /// 远端上同版本的执行器在哪里（上传的位置）。
    pub fn binary_path(&self, version: &str) -> String {
        format!(
            "{}/{DOT_DIR}/{EXECUTOR_HOME}/bin/{version}/{UNIX_EXECUTABLE}",
            self.home.trim_end_matches('/')
        )
    }
}
