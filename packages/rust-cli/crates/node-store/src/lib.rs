//! 会话存储与 Artifact 的本地实现：Node CLI 的 `db.sqlite`（docs/specs/rust-cli/README.md §5，
//! 细则见 docs/specs/rust-cli/node-store.md）。
//!
//! 把内核提交的事实映射成 Node 的 session / message / part / session_entry 记录，记录格式与 Node
//! 逐字节一致，Node CLI / App 能读、能续写；不新增表、列或迁移。冷加载从 Node 记录构建内核
//! 形状的消息（活动分支、压缩边界）。
//!
//! 并发：每个进程一个连接，首次使用时打开并补齐迁移；所有数据库访问都在阻塞线程里串行执行
//! （`rust_cli_base::blocking`），不在 async 线程上做同步 IO。
mod append;
mod artifacts;
mod branch;
mod clock;
mod compaction;
mod facts;
mod fork;
mod fork_clone;
mod hydrate;
mod ids;
mod load;
mod mode;
mod node_checkpoints;
mod notices;
mod operations;
mod project_mode;
mod project_rules;
mod query;
mod records;
mod repo;
mod rewind;
mod state;
mod task_lists;
mod tool_content;
mod tool_parts;
mod usage;
mod usage_query;
mod usage_stats;
mod workspace_trust;

use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rusqlite::Connection;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::ids::{ArtifactId, SessionId};
use rust_cli_kernel_api::permission::{PermissionMode, ProjectRules};
use rust_cli_kernel_api::store::{
    ArtifactPurpose, ArtifactRef, ArtifactStore, CommitReceipt, FactBatch, ForkReceipt,
    LoadedSession, SessionFilter, SessionFork, SessionKind, SessionMeta, SessionPage, SessionQuery,
    SessionStore, SessionSummary, StoreError,
};
use rust_cli_kernel_api::workspace_trust::TrustKey;
use rust_cli_paths::StoragePaths;
use serde_json::json;

pub use artifacts::uri as artifact_uri;
pub use clock::{FixedOffset, SystemClock, TimeZone};
pub use ids::{NodeIds, RandomUuids, UuidSource};

/// 本机执行环境的 ID（与入口 crate 相同）。只有它的会话写进本地库；`workspace_id` 为 NULL。
pub const LOCAL_ENVIRONMENT: &str = "local";

pub struct NodeStore {
    paths: StoragePaths,
    /// 记录的时间与随机 id：入口建的同一份 `NodeIds`（带着注入的 Clock）也交给内核（`Ports.ids`），记录与内核
    /// 生成的 ID 同源（F7，审查 R2-11、R1-15；以前本 crate 自带 `RecordSource`）。测试注入固定时钟与顺序 uuid。
    ids: Arc<NodeIds>,
    /// 本进程唯一的连接；首次使用时打开。只在阻塞线程里加锁，锁不跨 await。
    connection: Arc<Mutex<Option<Connection>>>,
    /// 会话库所在的目录已经建好（Node 打开会话库前递归建目录）。并发的第一次调用可能都建一次，建目录是
    /// 幂等的。
    database_dir: AtomicBool,
    /// 会话行的 `version`（Node 写 CLI 版本）：入口给的本工程版本（docs/specs/rust-cli/version.md §4），
    /// node-store 不读自己的包版本。没设时为空串（测试与工具）。
    version: String,
}

impl NodeStore {
    pub fn new(paths: StoragePaths, ids: Arc<NodeIds>) -> Self {
        Self {
            paths,
            ids,
            connection: Arc::new(Mutex::new(None)),
            database_dir: AtomicBool::new(false),
            version: String::new(),
        }
    }

    /// 新建会话行写入的版本（入口 crate 的 `VERSION`）。
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    pub fn paths(&self) -> &StoragePaths {
        &self.paths
    }

    /// 在阻塞线程里用连接执行 `work`；连接不存在时先打开并补齐迁移。
    async fn with_db<T, F>(&self, work: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection, &NodeIds) -> Result<T, StoreError> + Send + 'static,
    {
        let connection = self.connection.clone();
        let ids = self.ids.clone();
        let path = self.paths.database().as_path().to_path_buf();
        // 缺陷修复 R2-8：目录原来在阻塞线程里直接用 std::fs 建，豁免理由“rust-cli-fs 没有建目录的函数”已经
        // 过时；现在经 fs 的唯一入口，打开连接之前建一次。
        if !self.database_dir.load(Ordering::Acquire) {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                rust_cli_fs::create_dir_all(parent).await.map_err(storage)?;
            }
            self.database_dir.store(true, Ordering::Release);
        }
        rust_cli_base::blocking(move || {
            // 上一次在持锁时 panic：rusqlite 的事务在展开时已回滚，连接仍可用。
            let mut slot = connection.lock().unwrap_or_else(PoisonError::into_inner);
            let conn = match &mut *slot {
                Some(conn) => conn,
                empty => empty.insert(open_database(&path, ids.now_ms())?),
            };
            work(conn, ids.as_ref())
        })
        .await
        .map_err(storage)?
    }
}

fn open_database(path: &Path, now_ms: u64) -> Result<Connection, StoreError> {
    let (conn, _) =
        repo::open::open(path, repo::open::MIGRATION_LOCK_WAIT, now_ms).map_err(storage)?;
    Ok(conn)
}

pub(crate) fn storage(error: impl Display) -> StoreError {
    StoreError::Storage {
        message: error.to_string(),
    }
}

/// 仓储错误（rusqlite / anyhow）统一成 `StoreError::Storage`。
pub(crate) trait OrStorage<T> {
    fn storage(self) -> Result<T, StoreError>;
}

impl<T, E: Display> OrStorage<T> for Result<T, E> {
    fn storage(self) -> Result<T, StoreError> {
        self.map_err(storage)
    }
}

fn create_input(meta: &SessionMeta, version: &str) -> Result<repo::sessions::Create, StoreError> {
    if meta.workspace.environment.as_str() != LOCAL_ENVIRONMENT {
        return Err(facts::unsupported("sessions of a remote environment"));
    }
    let directory = meta.workspace.root.as_str();
    let task_type = match meta.kind {
        SessionKind::Interactive => None,
        SessionKind::Subagent => Some("subagent_child"),
        SessionKind::Fork => Some("fork"),
    };
    // fork 的标题是 `Fork of <原会话标题>`，来源同 Node `buildForkedSessionInput`：generated。其余会话
    // 标题为空时记为 default；首条真实输入按 Node `titleFromInput` 补上（append）。
    let title_source = if meta.kind == SessionKind::Fork {
        "generated"
    } else if meta.title.is_empty() {
        "default"
    } else {
        "first_input"
    };
    Ok(repo::sessions::Create {
        id: meta.id.as_str().to_owned(),
        project_id: ids::project_id(directory),
        workspace_id: None,
        parent_id: meta.parent.as_ref().map(|p| p.as_str().to_owned()),
        trace_id: Some(meta.trace.as_str().to_owned()),
        task_type: task_type.map(str::to_owned),
        slug: ids::slugify(meta.id.as_str()),
        directory: directory.to_owned(),
        path: Some(directory.to_owned()),
        title: meta.title.clone(),
        title_source: Some(title_source.to_owned()),
        version: version.to_owned(),
        // Node 写 `config.mode ?? "build"`；内核随后提交的 ModeChanged 会更新它。
        permission: Some(json!({"mode": "build"})),
        time_created: Some(i64::try_from(meta.created_at_ms).unwrap_or(i64::MAX)),
    })
}

#[async_trait]
impl SessionStore for NodeStore {
    async fn create(&self, meta: SessionMeta) -> Result<(), StoreError> {
        let input = create_input(&meta, &self.version)?;
        self.with_db(move |conn, ids| {
            let now = i64::try_from(ids.now_ms()).unwrap_or(i64::MAX);
            repo::sessions::create(conn, &input, now).storage()
        })
        .await
    }

    async fn commit(
        &self,
        session: &SessionId,
        batch: FactBatch,
    ) -> Result<CommitReceipt, StoreError> {
        let session = session.clone();
        self.with_db(move |conn, ids| facts::commit(conn, &session, &batch, ids))
            .await
    }

    async fn load(&self, session: &SessionId) -> Result<LoadedSession, StoreError> {
        let session = session.clone();
        let (mut loaded, checkpoints) = self
            .with_db(move |conn, _| load::load(conn, &session))
            .await?;
        loaded.checkpoints = node_checkpoints::resolve(self.paths.artifacts(), checkpoints).await;
        Ok(loaded)
    }

    async fn list(&self, filter: SessionFilter) -> Result<Vec<SessionSummary>, StoreError> {
        self.with_db(move |conn, _| load::list(conn, &filter)).await
    }

    /// ← 项目页与 `/resume` 页的会话（修复线 F29，node-store.md §5.1）。
    async fn query_sessions(&self, query: SessionQuery) -> Result<SessionPage, StoreError> {
        self.with_db(move |conn, _| query::query(conn, &query))
            .await
    }

    /// Node `setCustomSessionTitle`（node-store.md §5.1）。
    async fn rename(&self, session: &SessionId, title: &str) -> Result<(), StoreError> {
        let (session, title) = (session.clone(), title.to_owned());
        self.with_db(move |conn, ids| query::rename(conn, &session, &title, ids.now_ms()))
            .await
    }

    /// Node `softDeleteSession`（node-store.md §5.1）。
    async fn delete(&self, session: &SessionId) -> Result<(), StoreError> {
        let session = session.clone();
        self.with_db(move |conn, ids| query::delete(conn, &session, ids.now_ms()))
            .await
    }

    /// Node `/fork` 的新会话，一个事务写完（docs/specs/rust-cli/node-store.md §4.8）。
    async fn fork(&self, fork: SessionFork) -> Result<ForkReceipt, StoreError> {
        self.with_db(move |conn, ids| fork::fork(conn, &fork, ids))
            .await
    }

    /// 本机项目规则集（Node `getProjectPermission`）；项目 ID 由工作区根按 Node 规则算出。
    async fn project_rules(&self, workspace: &WorkspaceRef) -> Result<ProjectRules, StoreError> {
        let project = ids::project_id(workspace.root.as_str());
        self.with_db(move |conn, _| project_rules::read(conn, &project))
            .await
    }

    /// Node 群协作会话自己的规则集（修复线 F2，`project_rules::session_read`）。
    async fn session_rules(&self, session: &SessionId) -> Result<Option<ProjectRules>, StoreError> {
        let session = session.as_str().to_owned();
        self.with_db(move |conn, _| project_rules::session_read(conn, &session))
            .await
    }

    /// `/permissions` 的修改（docs/specs/rust-cli/tui-manage.md §1.1）：项目 ID 同 [`Self::project_rules`]。
    async fn change_project_rules(
        &self,
        workspace: &WorkspaceRef,
        change: rust_cli_kernel_api::store::ProjectRulesChange,
    ) -> Result<ProjectRules, StoreError> {
        let project = ids::project_id(workspace.root.as_str());
        self.with_db(move |conn, ids| project_rules::change(conn, &project, ids.now_ms(), &change))
            .await
    }

    /// 本机项目的持久模式（Node `getProjectPermissionMode`，docs/specs/rust-cli/config.md §2.2）。
    async fn project_mode(
        &self,
        workspace: &WorkspaceRef,
    ) -> Result<Option<PermissionMode>, StoreError> {
        let project = ids::project_id(workspace.root.as_str());
        self.with_db(move |conn, _| project_mode::read(conn, &project))
            .await
    }

    /// `/usage` 的 API 时长（docs/specs/rust-cli/usage.md §4）。
    async fn metering(
        &self,
        session: &SessionId,
    ) -> Result<rust_cli_kernel_api::metering::SessionMetering, StoreError> {
        let session = session.clone();
        self.with_db(move |conn, _| {
            facts::live_session(conn, &session)?;
            usage_query::metering(conn, session.as_str())
        })
        .await
    }

    /// footer 的请求统计（usage.md §4.1，修复线 F27）：本会话完成的主回合请求的聚合。
    async fn request_stats(
        &self,
        session: &SessionId,
    ) -> Result<rust_cli_kernel_api::metering::RequestStats, StoreError> {
        let session = session.clone();
        self.with_db(move |conn, _| {
            facts::live_session(conn, &session)?;
            usage_stats::request_stats(conn, session.as_str())
        })
        .await
    }

    /// 活动统计的原始聚合（usage.md §5.2）。
    async fn activity(
        &self,
        query: rust_cli_kernel_api::metering::ActivityQuery,
    ) -> Result<rust_cli_kernel_api::metering::ActivityStats, StoreError> {
        self.with_db(move |conn, _| usage_query::activity(conn, &query))
            .await
    }

    /// 工作区信任（docs/specs/rust-cli/workspace-trust.md §2.3）：候选里第一个有记录的键。
    async fn trusted_workspace(
        &self,
        candidates: &[TrustKey],
    ) -> Result<Option<TrustKey>, StoreError> {
        let candidates = candidates.to_vec();
        self.with_db(move |conn, _| workspace_trust::find(conn, &candidates))
            .await
    }

    /// 跨会话的入站与回执（cross-session.md §2.2）：只读。
    async fn entries(
        &self,
        session: &SessionId,
        kind: &str,
    ) -> Result<Vec<(String, serde_json::Value)>, StoreError> {
        let (session, kind) = (session.as_str().to_owned(), kind.to_owned());
        self.with_db(move |conn, _| operations::kind_entries(conn, &session, &kind))
            .await
    }

    /// 记下工作区已信任（upsert；不在会话事务里）。
    async fn trust_workspace(&self, key: &TrustKey) -> Result<(), StoreError> {
        let key = key.clone();
        self.with_db(move |conn, ids| workspace_trust::record(conn, &key, ids.now_ms()))
            .await
    }
}

#[async_trait]
impl ArtifactStore for NodeStore {
    async fn put(
        &self,
        session: &SessionId,
        purpose: ArtifactPurpose,
        media_type: String,
        bytes: Vec<u8>,
    ) -> Result<ArtifactRef, StoreError> {
        let artifact = ArtifactRef {
            session: session.clone(),
            id: ArtifactId::new(artifacts::artifact_id(&self.ids.uuid())),
            purpose,
            media_type,
            size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        };
        let path = artifacts::path(self.paths.artifacts(), &artifact);
        let options = rust_cli_fs::WriteOptions {
            mode: rust_cli_fs::FileMode::Default,
            temp_name: rust_cli_fs::TempName::Hidden,
            parent: rust_cli_fs::ParentDir::Create,
            rename_retry: &[],
        };
        // 临时文件写完、fsync、rename 之后才返回引用：拿到引用的一方读到的一定是完整内容。
        rust_cli_fs::atomic_write(path.as_path(), &bytes, options)
            .await
            .map_err(storage)?;
        Ok(artifact)
    }

    async fn get(&self, artifact: &ArtifactRef) -> Result<Vec<u8>, StoreError> {
        let path: PathBuf = artifacts::path(self.paths.artifacts(), artifact)
            .as_path()
            .to_path_buf();
        // 缺陷修复 R2-8：原来在阻塞线程里直接 std::fs::read，豁免理由“rust-cli-fs 还没有读文件的函数”已经
        // 过时；现在经 fs 的 `read_optional`。
        let bytes = match rust_cli_fs::read_optional(&path).await.map_err(storage)? {
            Some(bytes) => Some(bytes),
            // Node 写的产物（checkpoint，修复线 F12）用 Node 的文件名：按 Node 的读法找含 artifact id 的文件。
            None => node_checkpoints::read_node_named(self.paths.artifacts(), artifact).await?,
        }
        .ok_or_else(|| storage(format!("artifact {} is missing", artifact.id)))?;
        if u64::try_from(bytes.len()).ok() != Some(artifact.size) {
            return Err(storage(format!(
                "artifact {} has {} bytes, expected {}",
                artifact.id,
                bytes.len(),
                artifact.size
            )));
        }
        Ok(bytes)
    }

    /// 修复线 F18：Read 读落盘位置时按会话与 id 找回引用（大小、用途、类型取自文件），再经 `get` 核对大小。
    async fn find(
        &self,
        session: &SessionId,
        id: &ArtifactId,
    ) -> Result<Option<ArtifactRef>, StoreError> {
        artifacts::find(self.paths.artifacts(), session, id).await
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "records_tests.rs"]
mod records_tests;

#[cfg(test)]
#[path = "records_tools_tests.rs"]
mod records_tools_tests;

#[cfg(test)]
#[path = "hydrate_tests.rs"]
mod hydrate_tests;

#[cfg(test)]
#[path = "title_tests.rs"]
mod title_tests;

#[cfg(test)]
#[path = "user_image_tests.rs"]
mod user_image_tests;

#[cfg(test)]
#[path = "compat_tests.rs"]
mod compat_tests;

#[cfg(test)]
#[path = "node_checkpoints_tests.rs"]
mod node_checkpoints_tests;

#[cfg(test)]
#[path = "artifacts_find_tests.rs"]
mod artifacts_find_tests;

#[cfg(test)]
#[path = "model_connection_tests.rs"]
mod model_connection_tests;
