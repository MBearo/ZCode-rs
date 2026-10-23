// 复制自 apps/zcode-cli-rust/crates/state/src/node/migrations.rs（feat/rs 79b6691c55）。
//! Node 会话库的迁移（`adapters/.../session-store/migrations.ts`），SQL 原文逐字节内嵌
//! （docs/specs/rust-m11-node-storage.md §2.2）。
//!
//! 与复制来源的差异：Node 已把 `zcode_database_identity` 迁移改号为 `0024_session_database_identity`
//! （SQL 与校验和不变），这里随 Node；`previous_ids` 对应 Node 的 `previousIds`。
use rust_cli_base::include_lf;
use sha2::{Digest, Sha256};

pub struct Migration {
    pub id: &'static str,
    pub app_version: &'static str,
    pub sql: &'static str,
    /// Node `previousIds`：旧账本 id 已应用时只补记新 id，不重跑 SQL。
    pub previous_ids: &'static [&'static str],
}

macro_rules! migration {
    ($id:literal, $version:literal) => {
        migration!($id, $version, [])
    };
    ($id:literal, $version:literal, [$($previous:literal),*]) => {
        Migration {
            id: $id,
            app_version: $version,
            // 修复线 F56：以前用 `include_str!`，Windows 检出成 CRLF 时 SQL 带 `\r`，校验和与 Node 不同（Node 的 SQL
            // 在 JS 模板字符串里，只有 LF），0.1.52 打不开用户的库。现在 `.gitattributes` 固定 LF，`include_lf!` 在
            // 文本含 `\r` 时编译失败（docs/specs/rust-cli/node-store.md §2.1）。
            sql: include_lf!(concat!("migrations/", $id, ".sql")),
            previous_ids: &[$($previous),*],
        }
    };
}

/// Node `SQLITE_MIGRATIONS`，按执行顺序。
pub const MIGRATIONS: [Migration; 24] = [
    migration!("0001_base_session_store", "0.2.0"),
    migration!("0002_local_setting", "0.2.0"),
    migration!("0003_backfill_permission_local_setting", "0.2.0"),
    migration!("0004_session_target", "0.7.0"),
    migration!("0005_session_target_accounting", "0.7.0"),
    migration!("0006_input_history_attachments", "0.11.0"),
    migration!("0007_workflow_script_runtime", "0.13.0"),
    migration!("0008_workflow_definition_scope", "0.13.0"),
    migration!("0009_session_title_metadata", "0.14.0"),
    migration!("0010_usage_observability", "0.15.0"),
    migration!("0011_session_target_summary_title", "0.15.0"),
    migration!("0012_session_trace_id", "0.15.0"),
    migration!("0013_session_target_active_run_accounting", "0.15.0"),
    migration!("0014_message_part_sequence", "0.15.0"),
    migration!("0015_message_part_sequence_backfill_and_guard", "0.15.2"),
    migration!("0016_session_input_ledger", "0.15.2"),
    migration!("0017_session_input_start_now_delivery", "0.15.2"),
    migration!("0018_session_input_failed_status", "0.15.2"),
    migration!("0019_dwf_journal", "0.16.5"),
    migration!("0020_provider_model_selection", "0.16.5"),
    migration!("0021_official_glm_selection", "0.16.5"),
    migration!("0022_backfilled_session_reasoning", "0.16.5"),
    migration!(
        "0023_session_soft_delete_outcome",
        "0.16.9",
        ["0020_session_soft_delete_outcome"]
    ),
    migration!("0024_session_database_identity", "0.16.9"),
];

/// Node `migrationChecksum`：`sha256(sql.trim())` 的十六进制。
pub fn checksum(sql: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(rust_cli_base::js::string::trim(sql).as_bytes())
    )
}

#[cfg(test)]
#[path = "migrations_tests.rs"]
mod tests;
