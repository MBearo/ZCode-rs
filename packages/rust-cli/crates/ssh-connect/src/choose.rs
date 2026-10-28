//! 选用哪个执行器（remote-exec.md §19.9 的表）。纯函数：探测结果、要的版本与协议、本机有没有可以上传的产物 → 计划。
//!
//! F107（§28.2）：第一行按目标平台判（Linux x86_64 / aarch64 与 macOS arm64 / x86_64 之外报 `Unsupported`）。

use rust_cli_exec_transport::ProvisionError;

use crate::artifact::Platform;
use crate::probe::Probe;

/// 本机要的版本与协议主版本（组装根给）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    pub version: String,
    pub protocol: u32,
}

/// 选用的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// 用远端已有的这个二进制。
    Use(String),
    /// 上传本机这个平台的产物到 `bin/<版本>/`，再用它。
    Upload(Platform),
}

/// 按 §19.9 的表选。`artifact`：本机有没有远端平台的产物。
pub fn choose(probe: &Probe, wanted: &Wanted, artifact: bool) -> Result<Plan, ProvisionError> {
    let Some(platform) = probe.platform() else {
        return Err(ProvisionError::Unsupported {
            os: probe.os.clone(),
            arch: probe.arch.clone(),
        });
    };
    let reported = || {
        probe
            .candidates
            .iter()
            .filter_map(|candidate| Some((candidate.path.as_str(), candidate.report.as_ref()?)))
    };
    let exact = reported()
        .find(|(_, report)| report.version == wanted.version && report.protocol == wanted.protocol)
        .map(|(path, _)| path.to_owned());
    let compatible = reported()
        .find(|(_, report)| report.protocol == wanted.protocol)
        .map(|(path, _)| path.to_owned());
    // 同版本的：已有就用，没有就上传，都不行时退到 `fallback`。
    let ours = |fallback: Option<String>| -> Option<Plan> {
        exact
            .clone()
            .map(Plan::Use)
            .or_else(|| artifact.then_some(Plan::Upload(platform)))
            .or_else(|| fallback.map(Plan::Use))
    };
    match probe.resident() {
        Some(resident) => {
            // 常驻进程自己的二进制；没报告时找同版本的候选。
            let own = resident.executable.clone().or_else(|| {
                reported()
                    .find(|(_, report)| report.version == resident.version)
                    .map(|(path, _)| path.to_owned())
            });
            if resident.version == wanted.version
                && let Some(own) = own.clone().or_else(|| exact.clone())
            {
                return Ok(Plan::Use(own));
            }
            let same_protocol = resident.protocol == wanted.protocol;
            if !resident.idle {
                // 忙：协议相同就继续用旧的（§17.1）；不同就不能用，也不能换。
                return match (same_protocol, own) {
                    (true, Some(own)) => Ok(Plan::Use(own)),
                    (true, None) => ours(compatible).ok_or_else(|| no_artifact(wanted, platform)),
                    (false, _) => Err(ProvisionError::Incompatible {
                        version: resident.version.clone(),
                        protocol: resident.protocol,
                        busy: true,
                    }),
                };
            }
            // 空闲：换成同版本的（代理会把旧的停掉，§19.5）；没有本机产物时协议相同就继续用它。
            let fallback = if same_protocol { own } else { compatible };
            ours(fallback).ok_or_else(|| ProvisionError::Incompatible {
                version: resident.version.clone(),
                protocol: resident.protocol,
                busy: false,
            })
        }
        None => ours(compatible).ok_or_else(|| no_artifact(wanted, platform)),
    }
}

fn no_artifact(wanted: &Wanted, platform: Platform) -> ProvisionError {
    ProvisionError::NoArtifact {
        version: wanted.version.clone(),
        target: platform.missing_triple().to_owned(),
    }
}
