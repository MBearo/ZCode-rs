//! 执行器产物的另一个来源（F100，remote-exec.md §26.2、§26.7）：本机没有远端平台的静态产物时，部署器到这里拿产物。
//! 组装根注入；产品路径是 npm 的平台包（cli 的 `executor_dist`）。ssh-connect 只认这个 trait：不联网、不认识 npm。
//!
//! F107（§28.8）：参数是目标平台（系统 + 架构），不再只是架构。

use std::path::PathBuf;

use async_trait::async_trait;

use crate::artifact::Platform;

/// 下载失败的原因（F103，remote-exec.md §27.3）：只有“注册表里没有这个版本”时部署器才退到注册表的 latest；网络错误、
/// 校验不对不退。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    /// 注册表里没有这个版本（packument 里没有、包或 tarball 404）。`latest`：注册表的最新版本（包从来没发过、或取不到
    /// 合规则的值时 None）。
    Unpublished {
        reason: String,
        latest: Option<String>,
    },
    /// 别的失败（连不上、超时、校验不对、写缓存失败）。
    Failed(String),
}

impl DownloadError {
    /// 一句简短的原因（给人看）。
    pub fn reason(&self) -> &str {
        match self {
            Self::Unpublished { reason, .. } | Self::Failed(reason) => reason,
        }
    }
}

#[async_trait]
pub trait ArtifactSource: Send + Sync {
    /// 本机缓存里已有的 zcode `version` 的 `platform` 产物；不联网。
    async fn cached(&self, version: &str, platform: Platform) -> Option<PathBuf>;

    /// 下载、校验，原子地放进本机缓存，返回文件路径。同一个（版本，平台）同时只下载一次，一起等的拿到同一个结果。
    async fn download(&self, version: &str, platform: Platform) -> Result<PathBuf, DownloadError>;
}
