// 复制自 apps/zcode-cli-rust/crates/state/src/node/open.rs（feat/rs 79b6691c55）。
//! 打开 Node 会话库：Node `SqliteSessionStore.openStartup` 与 `runSqliteSessionMigrationsAsync`
//! （`migration-runner.ts`）、`sqlite-migrations/runner.ts` 的 `migrateExclusive`
//! （docs/specs/rust-m11-node-storage.md §2.2、§8）。
//!
//! 与复制来源的差异：
//! - 不报告启动进度（rust-cli 没有 `--prepare-storage` 握手）；结果里只给迁移事实。
//! - 账本按 Node `previousIds` 接管旧 id（只补记账本，不重跑 SQL），不再写死 0023 的特例预检。
//! - 迁移前备份没有复制：已有业务数据的库若还缺已知迁移，拒绝打开（`backup_required`），
//!   由 Node CLI / App 先升级。全新库没有要保护的表，照常补齐（docs/specs/rust-cli/node-store.md §2）。
use super::migrations::{MIGRATIONS, Migration, checksum};
use super::open_error::Attempt;
pub use super::open_error::StartupError;
use rusqlite::{Connection, ErrorCode, OptionalExtension, params};
use std::path::Path;
use std::time::{Duration, Instant};

/// 业务连接等锁的时长（Node `DEFAULT_SQLITE_STARTUP_LOCK_TIMEOUT_MS`）。
pub const BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);
/// 迁移等写锁的总预算（Node `DEFAULT_SQLITE_MIGRATION_WAIT_MS`）。
pub const MIGRATION_LOCK_WAIT: Duration = Duration::from_secs(60 * 60);
/// 迁移期间 SQLite 只短暂等待，由外层退避重试。
const MIGRATION_BUSY_TIMEOUT: Duration = Duration::from_millis(25);
const RETRY_INITIAL_MS: u64 = 10;
const RETRY_MAX_MS: u64 = 200;
const LEDGER_DDL: &str = "create table if not exists schema_migration (
      id text primary key, checksum text not null, app_version text, time_applied integer not null
    )";

/// Node `DatabaseMigrationFacts`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    /// `none`、`initialize` 或 `upgrade`。
    pub kind: &'static str,
    pub executed: usize,
}

/// 打开 `path` 并补齐 Node 的已知迁移。`lock_wait` 是等待其他写者的上限；`now_ms` 写进账本。
pub fn open(
    path: &Path,
    lock_wait: Duration,
    now_ms: u64,
) -> Result<(Connection, Facts), StartupError> {
    let display = path.display().to_string();
    let conn = Connection::open(path).map_err(|e| {
        let mut error = StartupError::sqlite(
            &e,
            &format!("Failed to open SQLite session database at {display}"),
        );
        error.code = "open_failed";
        error
    })?;
    let context = format!("SQLite setup failed for {display}");
    conn.busy_timeout(MIGRATION_BUSY_TIMEOUT)
        .map_err(|e| StartupError::sqlite(&e, &context))?;
    let runner = Runner {
        conn: &conn,
        path: &display,
        memory: display == ":memory:",
        deadline: Instant::now() + lock_wait,
        now_ms,
    };
    let migrated = runner.steps();
    if !conn.is_autocommit() {
        // 先回滚，再把失败交给调用方；业务不能拿到仍在事务内的连接。
        let _ = conn.execute_batch("rollback");
    }
    // 迁移结束（含失败）恢复业务连接的 5 秒等待，不继承迁移期间的短超时。
    let restored = conn.busy_timeout(BUSY_TIMEOUT);
    let facts = migrated?;
    restored.map_err(|e| StartupError::sqlite(&e, &context))?;
    Ok((conn, facts))
}

struct Runner<'a> {
    conn: &'a Connection,
    path: &'a str,
    memory: bool,
    deadline: Instant,
    now_ms: u64,
}

impl Runner<'_> {
    fn sql<T>(&self, result: rusqlite::Result<T>, context: &str) -> Result<T, Attempt> {
        result.map_err(|e| Attempt {
            busy: e.sqlite_error_code() == Some(ErrorCode::DatabaseBusy),
            error: StartupError::sqlite(&e, &format!("{context} for {}", self.path)),
        })
    }

    /// Node `acquire`：只对其他写者造成的 `SQLITE_BUSY` 退避重试。
    fn acquire<T>(
        &self,
        mut operation: impl FnMut(&Self) -> Result<T, Attempt>,
    ) -> Result<T, StartupError> {
        let mut delay = RETRY_INITIAL_MS;
        loop {
            let attempt = match operation(self) {
                Ok(value) => return Ok(value),
                Err(attempt) if !attempt.busy => return Err(attempt.error),
                Err(attempt) => attempt,
            };
            let now = Instant::now();
            if now >= self.deadline {
                let mut timeout = StartupError::new(
                    "lock_timeout",
                    format!(
                        "Timed out waiting for SQLite migration lock at {}",
                        self.path
                    ),
                );
                timeout.sqlite_code = attempt.error.sqlite_code;
                return Err(timeout);
            }
            // 本函数只在存储的阻塞线程里运行（docs/specs/rust-cli/node-store.md §3）。
            std::thread::sleep((self.deadline - now).min(Duration::from_millis(delay)));
            delay = (delay * 2).min(RETRY_MAX_MS);
        }
    }

    fn steps(&self) -> Result<Facts, StartupError> {
        self.sql(
            self.conn.execute_batch("pragma foreign_keys = on"),
            "SQLite setup failed",
        )
        .map_err(|a| a.error)?;
        self.acquire(Self::journal_mode)?;
        let kind = self.acquire(Self::preflight)?;
        self.acquire(|r| {
            r.sql(
                r.conn.execute_batch("begin immediate"),
                "SQLite migration lock failed",
            )
        })?;
        self.sql(
            self.conn.execute_batch(LEDGER_DDL),
            "SQLite migration initialization failed",
        )
        .map_err(|a| a.error)?;
        let pending = self.pending()?;
        // 缺陷修复（修复线 F12，双进程迁移竞争）：原来按预检的 `kind == "upgrade"` 拒绝。预检在等写锁之前读库，等锁
        // 期间旧版 Node 可能已建表、补了它知道的迁移并提交，预检的结论过期，结果在已有业务表的库上直接补迁移。Node
        // 在 `migrateExclusive` 的事务里按当时的表决定备份（`createMigrationBackup`）；这里同样在事务里重新判定。
        if !pending.is_empty() && self.has_data().map_err(|a| a.error)? {
            let ids: Vec<&str> = pending.iter().map(|m| m.id).collect();
            return Err(StartupError::new(
                "backup_required",
                format!(
                    "SQLite session database {} needs migrations {} that rust-cli cannot back up yet; open it once with the Node CLI or App first",
                    self.path,
                    ids.join(", ")
                ),
            ));
        }
        for migration in &pending {
            let context = format!("SQLite migration {} failed", migration.id);
            let tag = |attempt: Attempt| {
                let mut error = attempt.error;
                error.migration_id = Some(migration.id);
                error
            };
            self.sql(self.apply(migration), &context).map_err(tag)?;
            let ledger = self.conn.execute(
                "insert into schema_migration (id,checksum,app_version,time_applied) values (?,?,?,?)",
                params![migration.id, checksum(migration.sql), migration.app_version, self.now_ms as i64],
            );
            self.sql(ledger, &context).map_err(tag)?;
        }
        self.sql(
            self.conn.execute_batch("commit"),
            "SQLite migration commit failed",
        )
        .map_err(|a| a.error)?;
        Ok(Facts {
            kind,
            executed: pending.len(),
        })
    }

    /// Node `migrateExclusive` 的账本核对：已应用的 id（含 `previousIds`）逐个比校验和；只有旧 id
    /// 时补记新 id。返回还要执行的迁移。
    fn pending(&self) -> Result<Vec<&'static Migration>, StartupError> {
        let mut pending = vec![];
        for migration in &MIGRATIONS {
            let sum = checksum(migration.sql);
            let mut applied = vec![];
            for &id in std::iter::once(&migration.id).chain(migration.previous_ids) {
                if let Some(stored) = self.applied(id).map_err(|a| a.error)? {
                    ensure_checksum(id, &stored, &sum, self.path)?;
                    applied.push(id);
                }
            }
            match applied.first() {
                None => pending.push(migration),
                Some(first) if !applied.contains(&migration.id) => {
                    // 改号只沿用旧结果，不能重新执行 SQL。
                    let adopt = self.conn.execute(
                        "insert into schema_migration(id,checksum,app_version,time_applied)
          select ?,checksum,app_version,time_applied from schema_migration where id=?",
                        params![migration.id, first],
                    );
                    self.sql(adopt, "SQLite migration ledger update failed")
                        .map_err(|a| a.error)?;
                }
                Some(_) => {}
            }
        }
        Ok(pending)
    }

    /// 必须是 WAL（`:memory:` 库报告 `memory`）。
    fn journal_mode(&self) -> Result<(), Attempt> {
        let read = |sql: &str| {
            self.sql(
                self.conn.query_row(sql, [], |r| r.get::<_, String>(0)),
                "SQLite journal mode check failed",
            )
            .map(|mode| mode.to_lowercase())
        };
        let accepted = |mode: &str| mode == "wal" || (self.memory && mode == "memory");
        if accepted(&read("pragma journal_mode")?) {
            return Ok(());
        }
        let mode = read("pragma journal_mode = wal")?;
        if accepted(&mode) {
            return Ok(());
        }
        Err(StartupError::new(
            "sql_failed",
            format!(
                "SQLite refused WAL journal mode for {}; received {mode}",
                self.path
            ),
        )
        .into())
    }

    /// Node `inspectMigrationKind`：已应用的已知迁移逐个比校验和，再判定种类。
    fn preflight(&self) -> Result<&'static str, Attempt> {
        let context = "SQLite migration preflight failed";
        let ledger: bool = self.sql(
            self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migration')",
                [],
                |r| r.get(0),
            ),
            context,
        )?;
        let mut pending = false;
        for migration in &MIGRATIONS {
            let applied = if ledger {
                self.applied(migration.id)?
            } else {
                None
            };
            match applied {
                Some(applied) => {
                    ensure_checksum(migration.id, &applied, &checksum(migration.sql), self.path)?;
                }
                None => pending = true,
            }
        }
        if !pending {
            return Ok("none");
        }
        Ok(if self.has_data()? {
            "upgrade"
        } else {
            "initialize"
        })
    }

    /// 库里有账本以外的表（Node `inspectMigrationKind` 的 `hasDataTables`）。
    fn has_data(&self) -> Result<bool, Attempt> {
        self.sql(
            self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name NOT IN ('schema_migration', 'sqlite_sequence'))",
                [],
                |r| r.get(0),
            ),
            "SQLite migration preflight failed",
        )
    }

    /// 0023 用 Node 的 `apply`：列已存在时不重复 ADD COLUMN（旧版本曾以 0020 提前发布）。
    fn apply(&self, migration: &Migration) -> rusqlite::Result<()> {
        if migration.id != "0023_session_soft_delete_outcome" {
            return self.conn.execute_batch(migration.sql);
        }
        for (name, kind) in [("time_deleted", "integer"), ("last_outcome", "text")] {
            let exists: bool = self.conn.query_row(
                "select exists(select 1 from pragma_table_info('session') where name=?)",
                [name],
                |r| r.get(0),
            )?;
            if !exists {
                self.conn
                    .execute_batch(&format!("alter table session add column {name} {kind}"))?;
            }
        }
        Ok(())
    }

    fn applied(&self, id: &str) -> Result<Option<String>, Attempt> {
        self.sql(
            self.conn
                .query_row(
                    "select checksum from schema_migration where id = ?",
                    [id],
                    |r| r.get(0),
                )
                .optional(),
            "SQLite migration ledger read failed",
        )
    }
}

fn ensure_checksum(
    id: &'static str,
    applied: &str,
    current: &str,
    path: &str,
) -> Result<(), StartupError> {
    if applied == current {
        return Ok(());
    }
    let mut error = StartupError::new(
        "checksum_mismatch",
        format!(
            "SQLite migration checksum mismatch for {id}. Historical migrations are immutable; add a new migration instead. ({path})"
        ),
    );
    error.migration_id = Some(id);
    Err(error)
}

#[cfg(test)]
#[path = "open_tests.rs"]
mod tests;
