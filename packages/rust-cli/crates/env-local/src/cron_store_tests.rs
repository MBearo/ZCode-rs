use futures_util::StreamExt;
use rust_cli_kernel_api::cron_store::NewCronTask;

use super::*;

/// 注入的时钟：晚于各用例里任务的 `created_at`（与以前读系统时间时的先后相同）。
struct Later;

impl Clock for Later {
    fn now_ms(&self) -> u64 {
        1_800_000_000_000
    }

    fn time_zone(&self) -> String {
        "UTC".to_owned()
    }
}

fn store(dir: &tempfile::TempDir) -> LocalCronStore {
    LocalCronStore::new(&EnvPath::new(dir.path().to_string_lossy()), Arc::new(Later))
}

fn new_task(id: &str, session: &str) -> NewCronTask {
    NewCronTask {
        id: id.to_owned(),
        cron: "*/5 * * * *".to_owned(),
        prompt: "check the deploy".to_owned(),
        created_at: 1_760_000_000_000,
        recurring: true,
        created_by: SessionId::new(session),
    }
}

async fn file(dir: &tempfile::TempDir) -> Vec<FileTask> {
    let bytes = rust_cli_fs::read_optional(&dir.path().join(".zcode/scheduled_tasks.json"))
        .await
        .unwrap()
        .unwrap_or_default();
    cron_file::parse(&String::from_utf8(bytes).unwrap())
}

/// 新建时执行侧补上本进程与项目根；读出时是“本进程建的、在本项目”。
#[tokio::test]
async fn add_stamps_the_creator_and_reads_back() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    assert!(store.tasks().await.unwrap().is_empty());
    store
        .change(CronTasksChange::Add {
            task: new_task("1a2b3c4d", "sess_a"),
        })
        .await
        .unwrap();
    let raw = file(&dir).await;
    assert_eq!(raw[0].created_by_pid, Some(std::process::id()));
    assert_eq!(
        raw[0].created_in_project.as_deref(),
        Some(dir.path().to_string_lossy().as_ref())
    );
    let tasks = store.tasks().await.unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].creator, CronCreator::ThisProcess);
    assert!(tasks[0].in_project && tasks[0].recurring);
    assert_eq!(tasks[0].created_by, Some(SessionId::new("sess_a")));
}

/// 删除、回写 `lastFiredAt`；没有匹配的 ID 时不写文件。
#[tokio::test]
async fn remove_and_mark_fired_rewrite_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    for id in ["a1", "b2"] {
        store
            .change(CronTasksChange::Add {
                task: new_task(id, "sess_a"),
            })
            .await
            .unwrap();
    }
    store
        .change(CronTasksChange::MarkFired {
            ids: vec!["a1".to_owned()],
            at: 99,
        })
        .await
        .unwrap();
    store
        .change(CronTasksChange::Remove {
            ids: vec!["b2".to_owned(), "nope".to_owned()],
        })
        .await
        .unwrap();
    let tasks = store.tasks().await.unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(
        (tasks[0].id.as_str(), tasks[0].last_fired_at),
        ("a1", Some(99))
    );
    // 没有匹配：文件不动（监听不会收到事件）。
    let before = rust_cli_fs::read_optional(&dir.path().join(".zcode/scheduled_tasks.json"))
        .await
        .unwrap();
    store
        .change(CronTasksChange::Remove {
            ids: vec!["nope".to_owned()],
        })
        .await
        .unwrap();
    let after = rust_cli_fs::read_optional(&dir.path().join(".zcode/scheduled_tasks.json"))
        .await
        .unwrap();
    assert_eq!(before, after);
}

/// 别的进程建的：进程不在是 Gone，PID 是本进程但启动时间不同也是 Gone；没有 PID 也是 Gone；
/// `Claim` 把本会话建的项改成本进程；别的项目建的 `in_project` 为假。
#[tokio::test]
async fn creators_projects_and_claim() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    let base = FileTask {
        id: String::new(),
        cron: "*/5 * * * *".to_owned(),
        prompt: "p".to_owned(),
        created_at: 1,
        last_fired_at: None,
        recurring: false,
        permanent: false,
        created_by_session: Some("sess_old".to_owned()),
        created_by_pid: None,
        created_by_proc_start: None,
        created_in_project: Some(dir.path().to_string_lossy().into_owned()),
    };
    let reused = FileTask {
        id: "reused".to_owned(),
        created_by_pid: Some(std::process::id()),
        created_by_proc_start: Some("Thu Jan  1 00:00:00 1970".to_owned()),
        ..base.clone()
    };
    let no_pid = FileTask {
        id: "no-pid".to_owned(),
        created_in_project: Some("/Users/dev/elsewhere".to_owned()),
        ..base.clone()
    };
    let legacy = FileTask {
        id: "legacy".to_owned(),
        created_by_session: None,
        created_in_project: None,
        ..base.clone()
    };
    let mine = FileTask {
        id: "mine".to_owned(),
        created_by_session: Some("sess_a".to_owned()),
        created_by_pid: Some(1),
        ..base
    };
    store.write(&[reused, no_pid, legacy, mine]).await.unwrap();
    let tasks = store.tasks().await.unwrap();
    let by_id = |id: &str| tasks.iter().find(|task| task.id == id).unwrap().clone();
    assert_eq!(by_id("reused").creator, CronCreator::Gone);
    assert!(by_id("reused").in_project);
    assert_eq!(by_id("no-pid").creator, CronCreator::Gone);
    assert!(!by_id("no-pid").in_project);
    assert_eq!(by_id("legacy").created_by, None);
    assert!(!by_id("legacy").in_project);
    store
        .change(CronTasksChange::Claim {
            session: SessionId::new("sess_a"),
        })
        .await
        .unwrap();
    let tasks = store.tasks().await.unwrap();
    let mine = tasks.iter().find(|task| task.id == "mine").unwrap();
    assert_eq!(mine.creator, CronCreator::ThisProcess);
}

/// 监听（端到端，系统的监听服务）：本进程写入与“别的进程”直接写文件都产出变化；无关文件不产出。逻辑本身在
/// `cron_watch_tests.rs` 用注入的事件源测。
///
/// macOS 上不在全量里跑（工作线 F71，tools-more.md §16.2.1）：FSEvents 的登记与事件都经系统守护进程 fseventsd，它忙时
/// 一次登记要等几十秒（系统日志实测 24–66 s），测的是守护进程而不是本工程。要验证时
/// `cargo nextest run --run-ignored all -E 'test(changes_follow_writes)'`。
#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    target_os = "macos",
    ignore = "FSEvents waits for fseventsd, which can stall for tens of seconds"
)]
async fn changes_follow_writes_to_the_tasks_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    rust_cli_fs::create_dir_all(&dir.path().join(".zcode"))
        .await
        .unwrap();
    let mut changes = store.changes();
    store
        .change(CronTasksChange::Add {
            task: new_task("a1", "sess_a"),
        })
        .await
        .unwrap();
    let wait = std::time::Duration::from_secs(10);
    tokio::time::timeout(wait, changes.next())
        .await
        .expect("a change after our own write")
        .unwrap();
    // 把已经排着的事件取干净，再验证别的写入。
    while tokio::time::timeout(std::time::Duration::from_millis(300), changes.next())
        .await
        .is_ok()
    {}
    rust_cli_fs::atomic_write(
        &dir.path().join(".zcode/other.json"),
        b"{}",
        write_options(),
    )
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(500), changes.next())
            .await
            .is_err(),
        "unrelated files do not count"
    );
    rust_cli_fs::atomic_write(
        &dir.path().join(".zcode/scheduled_tasks.json"),
        b"{\"tasks\": []}\n",
        write_options(),
    )
    .await
    .unwrap();
    tokio::time::timeout(wait, changes.next())
        .await
        .expect("a change after another writer")
        .unwrap();
}

#[test]
fn project_keys_ignore_slashes_and_trailing_separators() {
    assert_eq!(
        project_key("/Users/dev/repo/"),
        project_key("/Users/dev/repo")
    );
    assert_eq!(project_key("C:\\Users\\dev"), project_key("C:/Users/dev"));
    assert_eq!(project_key("/"), "/");
}
