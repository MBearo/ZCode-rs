//! 编译期的版本号（docs/specs/rust-cli/version.md）：本地 release 编译时第三位是仓库里 `build-number` 的计数
//! （每次加 1），正式发版用 `ZCODE_RELEASE_VERSION`（必须等于计数文件给出的版本），debug 编译用 `Cargo.toml`
//! 的版本。结果写进 `ZCODE_BUILD_VERSION`，入口 crate 的 `VERSION` 是它唯一的读者。
//!
//! 本文件只读 cargo 给的环境变量、打印指令；计数文件的读写与规则在 `src/build_number.rs`（单测覆盖）。读环境
//! 变量是编译期行为，不是 `zcode` 运行时的 IO，不受 README §4 的入口规则约束；clippy 的禁令只在下面的函数上
//! 局部放开。

#[path = "src/build_number.rs"]
mod build_number;

use std::path::Path;

fn main() -> Result<(), String> {
    let profile = cargo_env("PROFILE")?;
    let manifest_dir = cargo_env("CARGO_MANIFEST_DIR")?;
    let package_version = cargo_env("CARGO_PKG_VERSION")?;
    let major = cargo_env("CARGO_PKG_VERSION_MAJOR")?;
    let minor = cargo_env("CARGO_PKG_VERSION_MINOR")?;
    let publish = optional_env(build_number::RELEASE_ENV)?;
    let directives = build_number::directives(&build_number::Inputs {
        profile: &profile,
        manifest_dir: Path::new(&manifest_dir),
        package_version: &package_version,
        major: &major,
        minor: &minor,
        publish: publish.as_deref(),
    })?;
    for text in directives {
        // 给 cargo 的指令经 stdout（clippy 的 print_stdout 不检查 build script）。
        println!("cargo:{text}");
    }
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "build script 读 cargo 给的编译期环境变量（version.md §3）"
)]
fn cargo_env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|error| format!("{name}: {error}"))
}

#[expect(
    clippy::disallowed_methods,
    reason = "build script 读正式发版给的 ZCODE_RELEASE_VERSION（version.md §3）"
)]
fn optional_env(name: &str) -> Result<Option<String>, String> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(format!("{name}: {error}")),
    }
}
