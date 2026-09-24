//! Startup failures of the shared Node database with Node's
//! `DatabaseStartupErrorCode` (`classifyDatabaseStartupError`).
use rusqlite::ErrorCode;

/// A startup failure with Node's `DatabaseStartupErrorCode`.
#[derive(Debug)]
pub struct StartupError {
    pub code: &'static str,
    pub sqlite_code: Option<i32>,
    pub migration_id: Option<&'static str>,
    pub message: String,
}

impl std::fmt::Display for StartupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
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

    /// Node `classifyDatabaseStartupError` on the SQLite primary result code.
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

/// A failed step; `busy` failures (another writer holds the lock) are retried.
pub(super) struct Attempt {
    pub(super) error: StartupError,
    pub(super) busy: bool,
}

impl From<StartupError> for Attempt {
    fn from(error: StartupError) -> Self {
        Self { error, busy: false }
    }
}
