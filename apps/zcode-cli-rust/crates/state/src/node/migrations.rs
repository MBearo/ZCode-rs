//! Node session store migrations (`adapters/.../session-store/migrations.ts`),
//! embedded byte for byte. Spec rust-m11-node-storage §2.2.
use sha2::{Digest, Sha256};

pub struct Migration {
    pub id: &'static str,
    pub app_version: &'static str,
    pub sql: &'static str,
}

macro_rules! migration {
    ($id:literal, $version:literal) => {
        Migration {
            id: $id,
            app_version: $version,
            sql: include_str!(concat!("migrations/", $id, ".sql")),
        }
    };
}

/// `SQLITE_MIGRATIONS` in execution order.
pub const MIGRATIONS: [Migration; 22] = [
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
];

/// ECMAScript `WhiteSpace` and `LineTerminator` (what `String.prototype.trim` strips).
pub(super) fn js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// Node `migrationChecksum`: `sha256(sql.trim())` in hex.
pub fn checksum(sql: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(sql.trim_matches(js_whitespace).as_bytes())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ledger values recorded by Node (and in existing user databases).
    const EXPECTED: [&str; 22] = [
        "60e2d6a38ab36f31417c4f92c02690c96c7dcaaa0b6abe1741117d62a55c6462",
        "22a6ada9325c9ad55a00a1f0ecf72332c63c1e89fc3d91fec8d155bb0058b465",
        "bd880375ba7b948c8bcda847e7eb52bc568d065bcba48190f6c3bfb14d11b7dc",
        "df670752991c78e38e2f25b0a003abc0e9689bc8d4894351358f2f83335a3ae1",
        "6138ed4562dfdd5b571d39d3b62266c55dd9eb8f2f41948e046441a0e4a954cc",
        "a2eab98649d738e15bdae27de7c5c114713f7777c55aa4111b19e03ee5ced54f",
        "0068fc4bcaffe4de4669442a62eee227726e0e8e81b61c7458213a20ac596009",
        "f7ee304e4005c291fb8883cfc180005263e6c6b2f94077487443f2c17a71d3eb",
        "3855cf957177ae6319ae91866cdff59e946ca07a10b36b5b59689818bd13fe00",
        "36918b0a98f465fe844097aa60c65ef73ea9c62cc266f02742bf3fc2cedf860b",
        "2b7723479426a4e7a1ed9901ed817495c1bbf63e9547eb1c63cd9e24bf9305f8",
        "9dcef90998dd00c8ed1b22a2170e180eb15471b93947e65ebe101d41e96bdb60",
        "7ab185540ebb7d26c5403ca52a50de6cf161c79cb93ccd47ff1b43b87415fe1c",
        "66b45c45e4d3a1a60829f193f38d865dcdbd3de2eb78aa79ba954fe7ef1aab08",
        "da3046bf061ebb5ba253bb772f0fc9e4d1f2856dac4cdbc4cc0a65aae00e8511",
        "18d51ae3f5e1425dc1e5c809282129fdc7430cacb6b1ce517b412ddbc34be790",
        "8c2da5985ecdf342438a2df713c9e18114276a0e85ce6f2e4f79bdf1596b52f8",
        "a4d1a7b7c5d4af426b695769ed0f3a031efac8aa7af1f6412a85292c42b5d15b",
        "1c8da5568d2357f25c392be486fae80e1f3ad3ebc830dab1bee941634573e8ba",
        "681180b4fcb497e13289b6970021678f24c975efdf3538777541105c5b2b444e",
        "433a8da454682406e962b043f2e0f412e9801bf7e4641ff25760aa16625ebafe",
        "aee472fe499e9c25d2e71df0fbc68d92e462b30d1d02919ca35d540bf133c99a",
    ];

    #[test]
    fn embedded_sql_matches_the_node_ledger() {
        for (migration, expected) in MIGRATIONS.iter().zip(EXPECTED) {
            assert_eq!(checksum(migration.sql), expected, "{}", migration.id);
        }
        assert_eq!(checksum("\u{feff} a \u{2028}"), checksum("a"));
    }
}
