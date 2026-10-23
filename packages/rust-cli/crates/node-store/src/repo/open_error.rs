// 复制自 apps/zcode-cli-rust/crates/state/src/node/open_error.rs（feat/rs 79b6691c55）。
//! 打开 Node 会话库的失败，错误码沿用 Node 的 `DatabaseStartupErrorCode`
//! （`classifyDatabaseStartupError`）。
use rusqlite::ErrorCode;

/// 带 Node `DatabaseStartupErrorCode` 的启动失败。
#[derive(Debug)]
pub struct StartupError {
    pub code: &'static str,
    pub sqlite_code: Option<i32>,
    pub migration_id: Option<&'static str>,
    pub message: String,
}

impl std::fmt::Display for StartupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for StartupError {}

impl StartupError {
    pub(super) fn new(code: &'static str, message: String) -> Self {
        Self {
            code,
            sqlite_code: None,
            migration_id: None,
            message,
        }
    }

    /// Node `classifyDatabaseStartupError`：按 SQLite 主结果码分类。
    pub(super) fn sqlite(error: &rusqlite::Error, context: &str) -> Self {
        let failure = match error {
            rusqlite::Error::SqliteFailure(failure, _) => Some(*failure),
            _ => None,
        };
        let code = match failure.map(|f| f.code) {
            Some(ErrorCode::DiskFull) => "storage_full",
            Some(ErrorCode::PermissionDenied | ErrorCode::ReadOnly) => "permission_denied",
            Some(ErrorCode::SystemIoFailure) => "io_error",
            Some(ErrorCode::OutOfMemory) => "out_of_memory",
            Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => "corrupt",
            Some(ErrorCode::CannotOpen) => "open_failed",
            Some(ErrorCode::DatabaseBusy) => "lock_timeout",
            _ => "sql_failed",
        };
        Self {
            code,
            sqlite_code: failure.map(|f| f.extended_code),
            migration_id: None,
            message: format!("{context}: {error}"),
        }
    }
}

/// 一步失败；`busy`（其他写者持锁）的失败会退避重试。
pub(super) struct Attempt {
    pub(super) error: StartupError,
    pub(super) busy: bool,
}

impl From<StartupError> for Attempt {
    fn from(error: StartupError) -> Self {
        Self { error, busy: false }
    }
}
