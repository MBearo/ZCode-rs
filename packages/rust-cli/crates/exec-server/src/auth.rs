//! 握手的鉴权（remote-exec.md §15.5、§18.6）：SSH 与 loopback 下连接本身已经是本用户，放行；云端换成校验令牌的实现。

use async_trait::async_trait;
use rust_cli_exec_wire::handshake::Hello;

/// 通过鉴权的一方。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// 客户端实例（账本按它分开）。
    pub client_instance: String,
}

#[async_trait]
pub trait Authenticator: Send + Sync {
    /// 看 `hello`（含不透明的凭据）决定放不放行；不放行时给出原因（写进 `refuse`）。
    async fn authenticate(&self, hello: &Hello) -> Result<Principal, String>;
}

/// 放行（SSH、loopback）。
pub struct AllowAll;

#[async_trait]
impl Authenticator for AllowAll {
    async fn authenticate(&self, hello: &Hello) -> Result<Principal, String> {
        Ok(Principal {
            client_instance: hello.client_instance.clone(),
        })
    }
}
