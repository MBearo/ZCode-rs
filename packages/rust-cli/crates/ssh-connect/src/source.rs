//! 执行器产物的另一个来源（F100，remote-exec.md §26.2、§26.7）：本机没有远端架构的静态产物时，部署器到这里拿 glibc 的
//! 产物。组装根注入；产品路径是 npm 的 Linux 平台包（cli 的 `executor_dist`）。ssh-connect 只认这个 trait：不联网、
//! 不认识 npm。

use std::path::PathBuf;

use async_trait::async_trait;

use crate::artifact::Arch;

#[async_trait]
pub trait ArtifactSource: Send + Sync {
    /// 本机缓存里已有的 zcode `version` 的 `arch` 产物（glibc 的）；不联网。
    async fn cached(&self, version: &str, arch: Arch) -> Option<PathBuf>;

    /// 下载、校验，原子地放进本机缓存，返回文件路径。同一个（版本，架构）同时只下载一次，一起等的拿到同一个结果。
    /// 失败时是一句简短的原因（给人看）。
    async fn download(&self, version: &str, arch: Arch) -> Result<PathBuf, String>;
}
