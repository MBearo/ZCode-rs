//! 测试夹具：经 rust-cli-fs 写临时目录里的文件（不直接用 std::fs，与 §4 的 IO 规则一致）；账号服务的
//! 替身（时钟、指向不可达地址的出口）。

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rust_cli_fs::{FileMode, ParentDir, TempName, WriteOptions};
use rust_cli_kernel_api::clock::Clock;
use rust_cli_net::{Egress, NetworkPolicy};

use crate::{AccountOrigins, AccountServices};

/// registry 的个人订阅与团队列表、权益：连不上任何服务。
pub(crate) async fn offline_account(
    credentials: crate::credentials::CredentialStore,
) -> crate::providers::Plans {
    let services = offline_services().await;
    let tokens = Arc::new(crate::token::ProjectTokens::new(
        services.egress.clone(),
        services.clock.clone(),
        services.origins.clone(),
    ));
    crate::providers::Plans {
        personal: Arc::new(crate::personal::PersonalPlans::new(
            services.egress.clone(),
            services.origins.clone(),
            tokens,
            credentials,
        )),
        teams: Arc::new(crate::team::TeamPlans::new(
            services.egress,
            services.origins,
        )),
    }
}

pub(crate) async fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    let options = WriteOptions {
        mode: FileMode::Default,
        temp_name: TempName::Hidden,
        parent: ParentDir::Create,
        rename_retry: &[],
    };
    rust_cli_fs::atomic_write(path, bytes.as_ref(), options)
        .await
        .unwrap();
}

/// 读文本文件；不存在时为空。
pub(crate) async fn read_text(path: &Path) -> Option<String> {
    rust_cli_fs::read_optional(path)
        .await
        .unwrap()
        .map(|bytes| String::from_utf8(bytes).unwrap())
}

/// 测试设定的时钟。
pub(crate) struct FixedClock(pub AtomicU64);

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    fn time_zone(&self) -> String {
        "UTC".to_owned()
    }
}

/// 不连任何服务的账号服务（地址指向不可达的本地端口）。
pub(crate) async fn offline_services() -> AccountServices {
    let origin = "http://127.0.0.1:9".to_owned();
    AccountServices {
        egress: Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap()),
        clock: Arc::new(FixedClock(AtomicU64::new(1_700_000_000_000))),
        origins: AccountOrigins {
            zcode_api: format!("{origin}/api/v1"),
            zai_api: origin.clone(),
            bigmodel_team_plans: format!("{origin}/coding-plan/team/plans"),
            bigmodel_api: origin,
            client_version: "0.0.0-test".to_owned(),
        },
    }
}

/// App 对照夹具（`scripts/rust-cli/team-plan-golden.mts` 生成，platform.md §7、§9）。
pub(crate) fn golden() -> serde_json::Value {
    serde_json::from_str(include_str!("../tests/fixtures/team-plan-golden.json")).unwrap()
}
