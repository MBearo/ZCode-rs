//! Opening the shared Node database: Node `SqliteSessionStore.openStartup`
//! with `runSqliteSessionMigrationsAsync` (`migration-runner.ts`).
//! Spec rust-m11-node-storage §2.2, §8.
use super::migrations::{MIGRATIONS, checksum};
use super::open_error::Attempt;
pub use super::open_error::StartupError;
use rusqlite::{Connection, ErrorCode, OptionalExtension, params};
use std::path::Path;
use std::time::{Duration, Instant};

/// Business connections wait this long for a lock (Node `DEFAULT_SQLITE_STARTUP_LOCK_TIMEOUT_MS`).
pub const BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);
/// The migration lock budget (Node `DEFAULT_SQLITE_MIGRATION_WAIT_MS`).
pub const MIGRATION_LOCK_WAIT: Duration = Duration::from_secs(60 * 60);
/// While migrating, SQLite only waits briefly; the runner retries with backoff.
const MIGRATION_BUSY_TIMEOUT: Duration = Duration::from_millis(25);
const RETRY_INITIAL_MS: u64 = 10;
const RETRY_MAX_MS: u64 = 200;
const LEDGER_DDL: &str = "create table if not exists schema_migration (
      id text primary key, checksum text not null, app_version text, time_applied integer not null
    )";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Checking,
    WaitingForLock,
    Migrating {
        id: &'static str,
        completed: usize,
        total: usize,
    },
    Committing,
    Ready,
}

/// Node `DatabaseMigrationFacts`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    /// `none`, `initialize` or `upgrade`.
    pub kind: &'static str,
    pub executed: usize,
    pub committed: usize,
    /// `None`: no trusted baseline yet; `Some(None)`: the ledger is empty.
    pub last_applied: Option<Option<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    pub phase: Phase,
    pub facts: Option<Facts>,
    pub elapsed_ms: u64,
}

/// Opens `path` and brings the schema to Node's current migrations.
/// `lock_wait` bounds the wait for other writers (Node waits up to an hour).
pub fn open(
    path: &Path,
    lock_wait: Duration,
    progress: &mut dyn FnMut(&Progress),
) -> Result<Connection, StartupError> {
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
    let started = Instant::now();
    let mut runner = Runner {
        conn: &conn,
        path: &display,
        memory: display == ":memory:",
        deadline: started + lock_wait,
        started,
        facts: None,
        in_transaction: false,
    };
    let migrated = runner.steps(progress);
    if migrated.is_err() && runner.in_transaction {
        // 先回滚，再把失败交给调用方；业务不能拿到仍在事务内的连接。
        let _ = conn.execute_batch("rollback");
    }
    // 迁移结束（含失败）恢复业务连接的 5 秒等待，不继承迁移期间的短超时。
    let restored = conn.busy_timeout(BUSY_TIMEOUT);
    migrated?;
    restored.map_err(|e| StartupError::sqlite(&e, &context))?;
    Ok(conn)
}

struct Runner<'a> {
    conn: &'a Connection,
    path: &'a str,
    memory: bool,
    deadline: Instant,
    started: Instant,
    facts: Option<Facts>,
    in_transaction: bool,
}

impl Runner<'_> {
    fn report(&self, progress: &mut dyn FnMut(&Progress), phase: Phase) {
        progress(&Progress {
            phase,
            facts: self.facts.clone(),
            elapsed_ms: self.started.elapsed().as_millis() as u64,
        });
    }

    fn sql<T>(&self, result: rusqlite::Result<T>, context: &str) -> Result<T, Attempt> {
        result.map_err(|e| Attempt {
            busy: e.sqlite_error_code() == Some(ErrorCode::DatabaseBusy),
            error: StartupError::sqlite(&e, &format!("{context} for {}", self.path)),
        })
    }

    /// Node `acquire`: retries only `SQLITE_BUSY` from other writers, with backoff.
    fn acquire<T>(
        &self,
        progress: &mut dyn FnMut(&Progress),
        mut operation: impl FnMut(&Self) -> Result<T, Attempt>,
    ) -> Result<T, StartupError> {
        let mut reported = false;
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
            if !reported {
                reported = true;
                self.report(progress, Phase::WaitingForLock);
            }
            std::thread::sleep((self.deadline - now).min(Duration::from_millis(delay)));
            delay = (delay * 2).min(RETRY_MAX_MS);
        }
    }

    fn steps(&mut self, progress: &mut dyn FnMut(&Progress)) -> Result<(), StartupError> {
        self.report(progress, Phase::Checking);
        self.sql(
            self.conn.execute_batch("pragma foreign_keys = on"),
            "SQLite setup failed",
        )
        .map_err(|a| a.error)?;
        self.acquire(progress, Self::journal_mode)?;
        let kind = self.acquire(progress, Self::preflight)?;
        self.facts = Some(Facts {
            kind,
            executed: 0,
            committed: 0,
            last_applied: None,
        });
        self.acquire(progress, |r| {
            r.sql(
                r.conn.execute_batch("begin immediate"),
                "SQLite migration lock failed",
            )
        })?;
        self.in_transaction = true;
        let init = "SQLite migration initialization failed";
        self.sql(self.conn.execute_batch(LEDGER_DDL), init)
            .map_err(|a| a.error)?;
        let baseline: Option<String> = self
            .sql(
                self.conn
                    .query_row(
                        "SELECT id FROM schema_migration ORDER BY id DESC LIMIT 1",
                        [],
                        |r| r.get(0),
                    )
                    .optional(),
                init,
            )
            .map_err(|a| a.error)?;
        if let Some(facts) = &mut self.facts {
            // 起点在拿锁后读取；非法 id 视为没有可信起点（Node safeParse 失败即缺失）。
            facts.last_applied = match baseline {
                None => Some(None),
                Some(id) if valid_migration_id(&id) => Some(Some(id)),
                Some(_) => None,
            };
        }
        let total = MIGRATIONS.len();
        for (completed, migration) in MIGRATIONS.iter().enumerate() {
            let sum = checksum(migration.sql);
            if let Some(applied) = self.applied(migration.id).map_err(|a| a.error)? {
                ensure_checksum(migration.id, &applied, &sum, self.path)?;
                continue;
            }
            if let Some(facts) = self.facts.as_mut().filter(|f| f.kind == "none") {
                facts.kind = "upgrade";
            }
            self.report(
                progress,
                Phase::Migrating {
                    id: migration.id,
                    completed,
                    total,
                },
            );
            let context = format!("SQLite migration {} failed", migration.id);
            let tag = |attempt: Attempt| {
                let mut error = attempt.error;
                error.migration_id = Some(migration.id);
                error
            };
            self.sql(self.conn.execute_batch(migration.sql), &context)
                .map_err(tag)?;
            if let Some(facts) = &mut self.facts {
                facts.executed += 1;
            }
            let ledger = self.conn.execute(
                "insert into schema_migration (id, checksum, app_version, time_applied) values (?, ?, ?, ?)",
                params![migration.id, sum, migration.app_version, zcode_cli_host::now() as i64],
            );
            self.sql(ledger, &context).map_err(tag)?;
        }
        self.report(progress, Phase::Committing);
        self.sql(
            self.conn.execute_batch("commit"),
            "SQLite migration commit failed",
        )
        .map_err(|a| a.error)?;
        self.in_transaction = false;
        if let Some(facts) = &mut self.facts {
            facts.committed = facts.executed;
        }
        self.report(progress, Phase::Ready);
        Ok(())
    }

    /// WAL is required (a `:memory:` database reports `memory`).
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

    /// Node `inspectMigrationKind`: checksums of applied known migrations, then the kind.
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
            match ledger
                .then(|| self.applied(migration.id))
                .transpose()?
                .flatten()
            {
                Some(applied) => {
                    ensure_checksum(migration.id, &applied, &checksum(migration.sql), self.path)?
                }
                None => pending = true,
            }
        }
        if !pending {
            return Ok("none");
        }
        let data: bool = self.sql(
            self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name NOT IN ('schema_migration', 'sqlite_sequence'))",
                [],
                |r| r.get(0),
            ),
            context,
        )?;
        Ok(if data { "upgrade" } else { "initialize" })
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

/// Node `databaseMigrationIdSchema`: `/^[a-zA-Z_0-9-]{1,128}$/`.
fn valid_migration_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
#[path = "open_tests.rs"]
mod tests;
