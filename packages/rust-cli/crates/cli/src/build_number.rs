//! 编译期的版本号（docs/specs/rust-cli/version.md）：计数文件 `packages/rust-cli/build-number` 的读写、版本的
//! 拼接与校验、给 cargo 的指令。`build.rs` 用 `#[path]` 引入它，只负责读环境变量与打印指令；cli 的单测也引入它
//! （`main.rs` 的 `cfg(test)` 模块），用临时目录与注入的路径测试。
//!
//! F54 起计数只来自仓库里提交的文件（用户决定：本地编译与正式发布共用一个号段）；L33 的
//! `<target 目录>/zcode-build-number` 不再读写。这里的文件读写是编译期行为，不是 `zcode` 运行时的 IO
//! （version.md §3），clippy 的禁令只在读写计数文件的函数上局部放开。

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// 计数文件名，在 workspace 目录（cli 目录往上两级）。
pub const COUNTER_FILE: &str = "build-number";

/// 永远不创建的哨兵文件（相对 cli 目录）。cargo 把不存在的 `rerun-if-changed` 路径当作“已变化”，本地 release
/// 编译因此每次都重跑 build script、计数加 1；不写任何 `rerun-if-*` 时 cargo 只在包内文件变化时重跑。
pub const SENTINEL: &str = ".zcode-build-number-sentinel";

/// 正式发版（ZCode-rs 的 CI）给的版本：只校验，不加 1、不写文件。
pub const RELEASE_ENV: &str = "ZCODE_RELEASE_VERSION";

/// cargo 给 build script 的输入（version.md §3）。
pub struct Inputs<'a> {
    /// `PROFILE`：`release`（含继承 release 的 profile）或 `debug`。
    pub profile: &'a str,
    /// `CARGO_MANIFEST_DIR`：cli 目录。
    pub manifest_dir: &'a Path,
    /// `CARGO_PKG_VERSION`：debug 的版本。
    pub package_version: &'a str,
    pub major: &'a str,
    pub minor: &'a str,
    /// `ZCODE_RELEASE_VERSION`；没设时为 `None`。
    pub publish: Option<&'a str>,
}

/// 这一次编译给 cargo 的指令（不带 `cargo:` 前缀），最后一条是版本（version.md §1、§3）。
///
/// - debug：不读不写计数文件，也不看发版变量；release 编译改了文件之后 debug 不重跑、入口 crate 不重编。
/// - 本地 release：读、加 1、原子写回；哨兵让每次 release 编译都重跑。
/// - 正式发版：变量必须逐字等于 `<major>.<minor>.<文件值>`，只读；文件或变量变了才重跑。
pub fn directives(inputs: &Inputs) -> Result<Vec<String>, String> {
    if inputs.profile != "release" {
        return Ok(vec![
            "rerun-if-changed=build.rs".to_owned(),
            version_directive(inputs.package_version),
        ]);
    }
    let counter = counter_path(inputs.manifest_dir)?;
    let Some(published) = inputs.publish else {
        let count = bump(&counter)?;
        let version = release_version(inputs.major, inputs.minor, count);
        return Ok(vec![
            format!("rerun-if-changed={SENTINEL}"),
            version_directive(&version),
        ]);
    };
    let expected = release_version(inputs.major, inputs.minor, read_count(&counter)?);
    if published != expected {
        return Err(format!(
            "{RELEASE_ENV} {published:?} does not match {expected} (Cargo.toml major.minor and {})",
            counter.display()
        ));
    }
    Ok(vec![
        "rerun-if-changed=build.rs".to_owned(),
        format!("rerun-if-changed={}", counter.display()),
        format!("rerun-if-env-changed={RELEASE_ENV}"),
        version_directive(&expected),
    ])
}

fn version_directive(version: &str) -> String {
    format!("rustc-env=ZCODE_BUILD_VERSION={version}")
}

/// 计数文件的位置：cli 目录往上两级（workspace 目录），与 target 目录无关（version.md §2）。
pub fn counter_path(manifest_dir: &Path) -> Result<PathBuf, String> {
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(COUNTER_FILE))
        .ok_or_else(|| {
            format!(
                "no workspace directory two levels above {}",
                manifest_dir.display()
            )
        })
}

/// 计数：去掉首尾空白后的一个十进制整数。坏内容（多半是合并后留下的冲突标记）报错，不静默回退。
pub fn parse_count(text: &str) -> Result<u64, String> {
    let digits = text.trim();
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!(
            "{text:?} is not a build number; write back one decimal integer (after a merge conflict, keep the larger number)"
        ));
    }
    digits
        .parse::<u64>()
        .map_err(|error| format!("{text:?} is not a build number: {error}"))
}

/// 读计数。文件缺失也是错误：它提交在仓库里（version.md §2）。
#[expect(
    clippy::disallowed_methods,
    reason = "编译期读仓库里的计数文件（version.md §3），不是运行时 IO"
)]
pub fn read_count(path: &Path) -> Result<u64, String> {
    let shown = path.display();
    let text = std::fs::read_to_string(path).map_err(|error| match error.kind() {
        ErrorKind::NotFound => format!(
            "{shown} is missing; it holds the release build number (docs/specs/rust-cli/version.md)"
        ),
        _ => format!("reading {shown}: {error}"),
    })?;
    parse_count(&text).map_err(|error| format!("{shown}: {error}"))
}

/// 读计数、加 1、原子写回（先写同目录的 `build-number.tmp` 再 rename），返回这一次的计数。坏内容与写失败时
/// 文件保持原样。并发的两次编译可能得到同一个号，本地号不要求唯一（version.md §2）。
#[expect(
    clippy::disallowed_methods,
    reason = "编译期写仓库里的计数文件（version.md §3），不是运行时 IO"
)]
pub fn bump(path: &Path) -> Result<u64, String> {
    let shown = path.display();
    let count = read_count(path)?
        .checked_add(1)
        .ok_or_else(|| format!("{shown}: the build number cannot grow any more"))?;
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, format!("{count}\n"))
        .map_err(|error| format!("writing {}: {error}", temp.display()))?;
    std::fs::rename(&temp, path).map_err(|error| format!("replacing {shown}: {error}"))?;
    Ok(count)
}

/// release 的版本：`Cargo.toml` 的 major、minor 加计数。
pub fn release_version(major: &str, minor: &str, count: u64) -> String {
    format!("{major}.{minor}.{count}")
}

#[cfg(test)]
#[path = "build_number_tests.rs"]
mod tests;
