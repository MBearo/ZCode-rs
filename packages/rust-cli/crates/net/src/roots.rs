//! 信任的根证书（docs/specs/rust-cli/config.md §2.4）。读 CA 文件是入口的事，这里只按 PEM 文本取证书。

use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject as _;

use crate::NetError;

/// 一份 PEM 里能用的证书。读法同 Node 给 https agent 的 `ca`（OpenSSL 的 PEM 读法）：按顺序读 `CERTIFICATE`
/// 块，块外的文字与其他类型的块（例如私钥）跳过；遇到解不开的证书块就停，前面的证书照用。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaBundle {
    pub(crate) certificates: Vec<Vec<u8>>,
    /// 读到了解不开的证书块（之后的内容没有读）。
    broken: bool,
}

impl CaBundle {
    pub fn from_pem(pem: &[u8]) -> Self {
        let mut certificates = Vec::new();
        let mut broken = false;
        for item in CertificateDer::pem_slice_iter(pem) {
            match item {
                Ok(der) if usable(&der) => certificates.push(der.to_vec()),
                _ => {
                    broken = true;
                    break;
                }
            }
        }
        Self {
            certificates,
            broken,
        }
    }

    pub fn len(&self) -> usize {
        self.certificates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.certificates.is_empty()
    }

    /// 读法上的问题（入口写警告用）：遇到解不开的证书块，或者一个证书都没有。
    pub fn problem(&self) -> Option<&'static str> {
        if self.broken {
            Some("invalid certificate")
        } else if self.certificates.is_empty() {
            Some("no certificate")
        } else {
            None
        }
    }
}

/// 交给 HTTP 客户端的证书（DER）。
pub(crate) fn certificates(ders: &[Vec<u8>]) -> Result<Vec<reqwest::Certificate>, NetError> {
    ders.iter()
        .map(|der| {
            reqwest::Certificate::from_der(der)
                .map_err(|error| NetError::Policy(format!("invalid CA certificate: {error}")))
        })
        .collect()
}

/// 内置的根证书：Mozilla 的列表（`webpki-root-certs`，CCADB 的 TLS 服务端根证书，编进二进制）。
/// 与 Bun 内置的 `tls.rootCertificates` 作用相同（docs/specs/rust-cli/config.md §2.4，工作线 F44）。
pub(crate) fn bundled() -> &'static [CertificateDer<'static>] {
    webpki_root_certs::TLS_SERVER_ROOT_CERTS
}

/// 交给 HTTP 客户端的信任锚。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Anchors {
    /// 平台根证书（系统的校验或证书库）加上这些（reqwest `tls_certs_merge`）。
    Merge(Vec<Vec<u8>>),
    /// 只信任这些（reqwest `tls_certs_only`）。
    Only(Vec<Vec<u8>>),
}

/// 能作为信任锚（rustls 解析 X.509 的结果；OpenSSL 解不开的证书这里同样解不开）。
fn usable(der: &CertificateDer<'_>) -> bool {
    rustls::RootCertStore::empty()
        .add(der.clone().into_owned())
        .is_ok()
}

/// 信任的根证书。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TrustRoots {
    /// 内置的 Mozilla 根证书加平台根证书（rustls-platform-verifier）：缺省的
    /// `bundled,system`。
    #[default]
    Platform,
    /// 内置与平台根证书加上这些（`NODE_EXTRA_CA_CERTS`）。
    Extra(CaBundle),
    /// 只信任这些（`ZCODE_AGENT_CA_CERT`，Node 的 `ca` 替换根证书，不含内置的）。一个都没有时所有 HTTPS 请求都在
    /// 证书校验时失败（Node 相同）。
    Only(CaBundle),
}

impl TrustRoots {
    /// 合并规则（config.md §2.4）：内置的证书在前，`NODE_EXTRA_CA_CERTS` 接在后面，都作为平台校验器的附加信任锚；
    /// `ZCODE_AGENT_CA_CERT` 替换全部。
    ///
    /// 缺陷修复（F44）：以前 `Platform` / `Extra` 不带内置的。没有 ca-certificates 的 Linux 镜像里平台校验器一张
    /// 证书都没有，构造失败（`No CA certificates were loaded from the system`），`zcode` 启动即退出；带着 Mozilla
    /// 根证书，同样的镜像里能连上。
    pub(crate) fn anchors(&self) -> Anchors {
        let mozilla = || bundled().iter().map(|der| der.to_vec());
        match self {
            Self::Platform => Anchors::Merge(mozilla().collect()),
            Self::Extra(extra) => Anchors::Merge(
                mozilla()
                    .chain(extra.certificates.iter().cloned())
                    .collect(),
            ),
            Self::Only(only) => Anchors::Only(only.certificates.clone()),
        }
    }
}

#[cfg(test)]
#[path = "roots_tests.rs"]
mod tests;
