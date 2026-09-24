use super::*;
use std::sync::mpsc;

fn open_at(path: &Path, wait: Duration) -> (Result<Connection, StartupError>, Vec<Progress>) {
    let mut seen = vec![];
    let result = open(path, wait, &mut |p| seen.push(p.clone()));
    (result, seen)
}

fn ledger(conn: &Connection) -> Vec<(String, String, String)> {
    conn.prepare("select id, checksum, app_version from schema_migration order by id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn a_new_database_gets_every_node_migration_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db.sqlite");
    let (conn, progress) = open_at(&path, MIGRATION_LOCK_WAIT);
    let conn = conn.unwrap();
    let rows = ledger(&conn);
    assert_eq!(rows.len(), MIGRATIONS.len());
    for (row, migration) in rows.iter().zip(&MIGRATIONS) {
        assert_eq!(row.0, migration.id);
        assert_eq!(row.1, checksum(migration.sql));
        assert_eq!(row.2, migration.app_version);
    }
    let mode: String = conn
        .query_row("pragma journal_mode", [], |r| r.get(0))
        .unwrap();
    let keys: i64 = conn
        .query_row("pragma foreign_keys", [], |r| r.get(0))
        .unwrap();
    assert_eq!((mode.as_str(), keys), ("wal", 1));
    let phases: Vec<&Phase> = progress.iter().map(|p| &p.phase).collect();
    assert_eq!(phases.first(), Some(&&Phase::Checking));
    assert_eq!(
        phases
            .iter()
            .filter(|p| matches!(p, Phase::Migrating { .. }))
            .count(),
        MIGRATIONS.len()
    );
    let last = progress.last().unwrap();
    assert_eq!(last.phase, Phase::Ready);
    let facts = last.facts.clone().unwrap();
    assert_eq!(
        (
            facts.kind,
            facts.executed,
            facts.committed,
            facts.last_applied
        ),
        ("initialize", 22, 22, Some(None))
    );
    drop(conn);

    let (conn, progress) = open_at(&path, MIGRATION_LOCK_WAIT);
    conn.unwrap();
    let facts = progress.last().unwrap().facts.clone().unwrap();
    assert_eq!((facts.kind, facts.executed), ("none", 0));
    assert_eq!(
        facts.last_applied,
        Some(Some("0022_backfilled_session_reasoning".into()))
    );
}

#[test]
fn foreign_migrations_tables_and_columns_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db.sqlite");
    open_at(&path, MIGRATION_LOCK_WAIT).0.unwrap();
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "insert into schema_migration values ('0023_session_soft_delete_outcome','x','3.12.0',1);
             insert into schema_migration values ('0017_todo_state_revision','y','0.15.2',1);
             alter table session add column time_deleted integer;
             alter table session add column last_outcome text;
             create table todo_state(session_id text primary key, todo_revision integer not null, time_updated integer not null);",
        )
        .unwrap();
    }
    let (conn, progress) = open_at(&path, MIGRATION_LOCK_WAIT);
    let conn = conn.unwrap();
    assert_eq!(progress.last().unwrap().facts.clone().unwrap().kind, "none");
    let columns: i64 = conn
        .query_row(
            "select count(*) from pragma_table_info('session') where name in ('time_deleted','last_outcome')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(columns, 2);
}

#[test]
fn a_changed_checksum_stops_startup_and_a_gap_is_filled() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db.sqlite");
    open_at(&path, MIGRATION_LOCK_WAIT).0.unwrap();
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "delete from schema_migration where id = '0022_backfilled_session_reasoning'",
        [],
    )
    .unwrap();
    let (reopened, progress) = open_at(&path, MIGRATION_LOCK_WAIT);
    reopened.unwrap();
    let facts = progress.last().unwrap().facts.clone().unwrap();
    assert_eq!((facts.kind, facts.executed), ("upgrade", 1));

    conn.execute(
        "update schema_migration set checksum = 'x' where id = '0005_session_target_accounting'",
        [],
    )
    .unwrap();
    let error = open_at(&path, MIGRATION_LOCK_WAIT).0.unwrap_err();
    assert_eq!(error.code, "checksum_mismatch");
    assert_eq!(error.migration_id, Some("0005_session_target_accounting"));
}

#[test]
fn another_writer_makes_startup_wait_or_time_out() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db.sqlite");
    open_at(&path, MIGRATION_LOCK_WAIT).0.unwrap();
    let holder = Connection::open(&path).unwrap();
    holder.execute_batch("begin immediate").unwrap();
    let (result, progress) = open_at(&path, Duration::from_millis(150));
    let error = result.unwrap_err();
    assert_eq!(error.code, "lock_timeout");
    assert!(progress.iter().any(|p| p.phase == Phase::WaitingForLock));

    let (release, released) = mpsc::channel();
    let waiter = {
        let path = path.clone();
        std::thread::spawn(move || {
            let (result, _) = open_at(&path, Duration::from_secs(5));
            release.send(()).ok();
            result.map(drop)
        })
    };
    std::thread::sleep(Duration::from_millis(100));
    holder.execute_batch("commit").unwrap();
    waiter.join().unwrap().unwrap();
    released.recv().unwrap();
}
