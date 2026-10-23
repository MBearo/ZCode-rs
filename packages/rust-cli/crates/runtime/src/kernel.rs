//! 内核会话 API 的本地实现（[`Kernel`]）：建会话、打开会话（冷加载）、列出会话。

use super::*;
use rust_cli_kernel_api::permission::ExecutionState;

fn failed(error: &StoreError) -> KernelError {
    match error {
        StoreError::NotFound { session } => KernelError::NotFound {
            session: session.clone(),
        },
        other => KernelError::Failed {
            message: other.to_string(),
        },
    }
}

#[async_trait]
impl Kernel for LocalKernel {
    async fn create_session(&self, request: CreateSession) -> Result<SessionId, KernelError> {
        self.create_session_as(request, StartSource::Startup).await
    }

    async fn open_session(
        &self,
        session: &SessionId,
    ) -> Result<Arc<dyn SessionHandle>, KernelError> {
        self.open_session_as(session, StartSource::Resume).await
    }

    async fn create_session_as(
        &self,
        request: CreateSession,
        start: StartSource,
    ) -> Result<SessionId, KernelError> {
        let id = request.id.unwrap_or_else(|| self.ports.ids.session());
        let meta = SessionMeta {
            id: id.clone(),
            kind: SessionKind::Interactive,
            parent: None,
            workspace: request.workspace,
            title: String::new(),
            // 顶层会话的链路 ID 取会话 ID；子会话、子代理继承父会话的链路 ID。
            trace: TraceId::new(id.as_str()),
            created_at_ms: self.ports.clock.now_ms(),
        };
        self.ports
            .store
            .create(meta.clone())
            .await
            .map_err(|error| failed(&error))?;
        // 执行状态（修复线 F20）：单值的 `plan` 是“plan 开、default”，另记的 `plan` 打开 plan。
        let execution = ExecutionState::from_parts(request.mode, request.plan);
        let batch = FactBatch {
            owner_epoch: OwnerEpoch::LOCAL,
            command: None,
            facts: vec![
                Fact::ModelSelected {
                    selection: request.selection.clone(),
                },
                Fact::ExecutionChanged { state: execution },
            ],
            turn: None,
        };
        self.ports
            .store
            .commit(&id, batch)
            .await
            .map_err(|error| failed(&error))?;
        self.start(
            SessionState {
                meta,
                messages: Vec::new(),
                selection: request.selection,
                mode: execution,
                checkpoints: Vec::new(),
                start,
                turn: None,
            },
            Opened::Created,
        );
        Ok(id)
    }

    async fn open_session_as(
        &self,
        session: &SessionId,
        start: StartSource,
    ) -> Result<Arc<dyn SessionHandle>, KernelError> {
        // 修复（F33，session-lifecycle.md §3.3.1）：以前表里有这个会话就直接给出句柄，它正在关闭时（actor 已退出、
        // 还没移出表）打开拿到的是死句柄，`view()` 报 `the session is closed`；两个并发的打开还会各自冷加载。现在
        // 关闭中、加载中的等它换掉；没有条目时登记 Loading，由这个调用冷加载（失败或被丢下时守卫移出 Loading）。
        let _loading = match self.claim(session).await {
            table::Claim::Open(open) => return Ok(open),
            table::Claim::Load(loading) => loading,
        };
        let loaded = self
            .ports
            .store
            .load(session)
            .await
            .map_err(|error| failed(&error))?;
        // Node 群协作会话（`permission.scope = session`）的规则只在会话行里：这个会话的判决用它代替本机项目
        // 规则集（修复线 F2，shell-permissions.md §5）。“总是允许”与改模式由存储按同一作用域落库。条目是这个调用
        // 的 Loading，期间不会有同一个会话的关闭插进来 `release` 它。
        if let Some(rules) = self
            .ports
            .store
            .session_rules(session)
            .await
            .map_err(|error| failed(&error))?
        {
            self.gate.scope(session, &rules);
        }
        let state = SessionState {
            meta: loaded.meta,
            messages: loaded.messages,
            selection: loaded
                .model
                .unwrap_or_else(|| self.defaults.selection.clone()),
            mode: loaded.mode.map_or_else(
                || self.defaults.mode.into(),
                |mode| ExecutionState::from_parts(mode, loaded.plan),
            ),
            checkpoints: loaded.checkpoints,
            start,
            turn: loaded.turn,
        };
        Ok(self.start(state, Opened::Loaded))
    }

    async fn list_sessions(
        &self,
        filter: SessionFilter,
    ) -> Result<Vec<SessionSummary>, KernelError> {
        self.ports
            .store
            .list(filter)
            .await
            .map_err(|error| failed(&error))
    }

    /// `/context`（context-usage.md §2）：有会话时按会话 actor 的当前视图（历史、模型、模式），只读。
    async fn context_usage(
        &self,
        query: rust_cli_kernel_api::context_usage::ContextUsageQuery,
    ) -> Result<rust_cli_kernel_api::context_usage::ContextUsage, KernelError> {
        let Some(session) = &query.session else {
            return self.usage.analyze(query, &[]).await;
        };
        let view = self.open_session(session).await?.view().await?;
        let query = rust_cli_kernel_api::context_usage::ContextUsageQuery {
            selection: view.selection,
            // 判决用的模式（plan 开时是 plan，修复线 F20）。
            mode: ExecutionState::from_parts(view.mode, view.plan).effective(),
            ..query
        };
        self.usage.analyze(query, &view.messages).await
    }

    /// `@` 补全的文件清单（tui-input.md §1.2）：执行环境从工作区根遍历（遵守忽略规则）。
    async fn workspace_files(
        &self,
        workspace: &rust_cli_kernel_api::env::WorkspaceRef,
    ) -> Result<Vec<String>, KernelError> {
        let request = rust_cli_kernel_api::env_files::ListFiles {
            cwd: workspace.root.clone(),
        };
        self.ports
            .env
            .list_files(workspace, request)
            .await
            .map(|list| list.files)
            .map_err(|error| KernelError::Failed {
                message: error.to_string(),
            })
    }

    /// `-p` 的 stream-json 的 system/init：会话可用的工具、agent 与 skill（headless.md §3.3）。
    async fn session_catalog(
        &self,
        query: rust_cli_kernel_api::catalog::CatalogQuery,
    ) -> Result<rust_cli_kernel_api::catalog::SessionCatalog, KernelError> {
        Ok(catalog::catalog(&self.usage, &query).await)
    }

    /// `/mcp`、`/status`：ext 的连接管理给出的状态（tui-display.md §3.4）。
    async fn mcp_servers(&self) -> Vec<rust_cli_kernel_api::manage::McpServerInfo> {
        self.ext.mcp().server_infos()
    }

    /// `/plan open`：计划文件的位置（与 ExitPlanMode 同一个来源）与它有没有内容（tui-manage.md §8）。
    async fn plan_file(
        &self,
        query: rust_cli_kernel_api::manage::PlanFileQuery,
    ) -> Result<rust_cli_kernel_api::manage::PlanFile, KernelError> {
        directory::plan_file(self.ports.env.as_ref(), &query).await
    }

    /// `/cd <path>`：检查路径，有会话时经会话 actor 换工作目录（tui-manage.md §7.1）。
    async fn change_directory(
        &self,
        request: rust_cli_kernel_api::manage::ChangeDirectory,
    ) -> Result<rust_cli_kernel_api::manage::DirectoryChanged, KernelError> {
        directory::change(self, request).await
    }

    /// `/mcp reconnect|enable|disable`：经 ext 的连接管理（tui-manage.md §6.1）。
    async fn mcp_control(
        &self,
        control: rust_cli_kernel_api::manage::McpControl,
    ) -> Result<Vec<rust_cli_kernel_api::manage::McpServerInfo>, KernelError> {
        Ok(self
            .ext
            .mcp()
            .control(control.action, &control.servers)
            .await)
    }

    /// `/usage` 的 API 时长：存储的用量表是唯一来源（docs/specs/rust-cli/usage.md §4）。
    async fn session_metering(
        &self,
        session: &SessionId,
    ) -> Result<rust_cli_kernel_api::metering::SessionMetering, KernelError> {
        self.ports
            .store
            .metering(session)
            .await
            .map_err(|error| failed(&error))
    }

    /// footer 的请求统计（tui-display.md §2.6、usage.md §4.1，修复线 F27）：存储从用量表聚合。
    async fn request_stats(
        &self,
        session: &SessionId,
    ) -> Result<rust_cli_kernel_api::metering::RequestStats, KernelError> {
        self.ports
            .store
            .request_stats(session)
            .await
            .map_err(|error| failed(&error))
    }

    /// 活动统计（usage.md §5）：今天与本地日的偏移取自注入的时钟，聚合在存储。
    async fn activity(
        &self,
        range: rust_cli_kernel_api::metering::ActivityRange,
    ) -> Result<rust_cli_kernel_api::metering::ActivityStats, KernelError> {
        let clock = &self.ports.clock;
        let query = rust_cli_kernel_api::metering::ActivityQuery {
            range,
            today: clock.today(),
            utc_offset_minutes: clock.utc_offset_minutes(clock.now_ms()),
        };
        self.ports
            .store
            .activity(query)
            .await
            .map_err(|error| failed(&error))
    }

    /// ← 会话面板：库里的会话合并本进程里打开着的会话的实时状态（tui-panels.md §2.1）。
    async fn workspace_sessions(
        &self,
        filter: SessionFilter,
    ) -> Result<Vec<rust_cli_kernel_api::panels::WorkspaceSession>, KernelError> {
        self.overview(filter).await
    }

    /// ← 项目页与 `/resume` 页（修复线 F29，tui-panels.md §2.1）。
    async fn project_sessions(
        &self,
        query: rust_cli_kernel_api::store::SessionQuery,
    ) -> Result<rust_cli_kernel_api::panels::ProjectSessions, KernelError> {
        self.projects(query).await
    }

    async fn rename_session(&self, session: &SessionId, title: String) -> Result<(), KernelError> {
        self.rename(session, title).await
    }

    async fn delete_session(&self, session: &SessionId) -> Result<(), KernelError> {
        self.delete(session).await
    }

    async fn session_snapshot(
        &self,
        session: &SessionId,
    ) -> Result<rust_cli_kernel_api::session_api::SessionView, KernelError> {
        self.snapshot(session).await
    }

    /// `/hooks`：hooks 引擎当前快照里的配置（tui-display.md §3.5）。
    async fn hooks(&self) -> Vec<rust_cli_kernel_api::manage::HookEntry> {
        self.hooks.entries()
    }

    /// `/diff` 的未提交改动：执行环境里 git 的原始输出（tui-manage.md §4.1）。
    async fn uncommitted_diff(
        &self,
        workspace: &rust_cli_kernel_api::env::WorkspaceRef,
    ) -> Result<rust_cli_kernel_api::env_git::GitDiff, KernelError> {
        let request = rust_cli_kernel_api::env_git::GitDiffRequest {
            cwd: workspace.root.clone(),
        };
        self.ports
            .env
            .git_diff(workspace, request)
            .await
            .map_err(|error| KernelError::Failed {
                message: error.to_string(),
            })
    }

    /// `/export <文件名>`：经执行环境写文件（tui-manage.md §3）。
    async fn write_export(
        &self,
        file: rust_cli_kernel_api::manage::ExportFile,
    ) -> Result<String, KernelError> {
        export::write(
            self.ports.env.as_ref(),
            self.ports.ids.as_ref(),
            &self.generation,
            file,
        )
        .await
    }

    /// `/permissions` 的列表（tui-manage.md §1.1）。
    async fn permission_rules(
        &self,
        query: rust_cli_kernel_api::permission::PermissionRulesQuery,
    ) -> Result<Vec<rust_cli_kernel_api::permission::SourcedRule>, KernelError> {
        permissions::listing(&self.gate, self.ports.store.as_ref(), &query).await
    }

    /// `/permissions` 的修改：落库后换上新快照（tui-manage.md §1.1）。
    async fn change_permission_rules(
        &self,
        change: rust_cli_kernel_api::permission::PermissionRulesChange,
    ) -> Result<Vec<rust_cli_kernel_api::permission::SourcedRule>, KernelError> {
        permissions::change(&self.gate, self.ports.store.as_ref(), change).await
    }

    /// TUI 的斜杠补全：ext 给出的可调用 skill（ext.md §2.7）。
    async fn user_commands(
        &self,
        workspace: &rust_cli_kernel_api::env::WorkspaceRef,
    ) -> Vec<rust_cli_kernel_api::user_command::UserCommand> {
        self.ext.user_commands().commands(workspace).await
    }
}
