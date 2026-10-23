//! 权限：模式、规则语法、判决链、Bash 只读判定（docs/specs/rust-cli/shell-permissions.md §4）。
//!
//! 判决顺序（[`chain`]）：deny 规则 → 工具自身的检查 → ask 规则 → 模式
//! （bypassPermissions 也遵守 deny、ask 规则与安全路径）→ allow 规则 → 询问；外层再按 dontAsk /
//! auto / plan 改写询问。工具没有自己的检查时，Bash、文件工具、WebFetch 用这里的内置检查。
//!
//! 规则只来自配置快照（[`PermissionRules`]，由入口层组装后交进来，“总是允许”落库后由 runtime 换上新的
//! LocalSettings 层；进程里所有会话共用一份权限上下文）。这里不做 IO：cwd、home 与命令
//! 输出目录都从执行环境的端口取。Bash 解析与只读判定复制自 `apps/zcode-cli-rust/crates/{bash-parse,bash}`。

mod bash;
mod bash_paths;
mod bfs_args;
pub mod chain;
pub mod check;
pub mod classifier;
mod files;
pub mod guarded;
mod parse;
pub mod paths;
pub mod readonly;
pub mod rules;
mod safe_env;
pub mod shell_rules;
mod skill;
pub mod texts;
pub mod web_preapproved;

#[cfg(test)]
mod test_support;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use async_trait::async_trait;
use rust_cli_kernel_api::env_files::PathStyle;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::orchestration::PermissionGate;
use rust_cli_kernel_api::permission::{
    PermissionDecision, PermissionMode, PermissionRule, PermissionRules,
};
use rust_cli_kernel_api::plan::PlanFiles;
use rust_cli_kernel_api::tool::{Tool, ToolUseContext};
use serde_json::Value;

pub use bash::suggestion as bash_suggestion;
pub use parse::{Analysis, Invocation, analyze, is_permission_safe};

use crate::chain::{Call, Outer};
use crate::check::Inputs;
use crate::classifier::{AutoClassifier, Unavailable};
use crate::paths::PathContext;
use crate::rules::Rules;

/// 判决链的开关。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyOptions {
    /// plan 模式按自动模式的语义走分类器（缺省开启）。
    pub auto_in_plan: bool,
}

impl Default for PolicyOptions {
    fn default() -> Self {
        Self { auto_in_plan: true }
    }
}

/// 通用权限判决链。
///
/// 修复（R1-2）：以前“总是允许”在这里另存一份按会话的规则（`session_rules`），库里还有一份，LocalSettings 层不刷新：
/// 子代理与 `/clear` 之后的新会话（别的会话 ID）又问一次，`/permissions` 也要去重补偿。现在规则只有快照一个来源，
/// 回答落库后 runtime 按 `/permissions` 的路径读回库值换上新快照。
pub struct PolicyGate {
    rules: RwLock<Arc<PermissionRules>>,
    /// Node 群协作会话（`permission.scope = session`）自己的规则集：判决时代替快照里的本机项目规则集
    /// （LocalSettings）。会话 actor 打开这种会话时由 runtime 放进来（修复线 F2）。
    scoped: Mutex<HashMap<SessionId, Vec<rust_cli_kernel_api::permission::SourcedRule>>>,
    classifier: Arc<dyn AutoClassifier>,
    options: PolicyOptions,
}

impl Default for PolicyGate {
    fn default() -> Self {
        Self::new(PermissionRules::default())
    }
}

impl PolicyGate {
    pub fn new(rules: PermissionRules) -> Self {
        Self {
            rules: RwLock::new(Arc::new(rules)),
            scoped: Mutex::default(),
            classifier: Arc::new(Unavailable),
            options: PolicyOptions::default(),
        }
    }

    #[must_use]
    pub fn with_classifier(mut self, classifier: Arc<dyn AutoClassifier>) -> Self {
        self.classifier = classifier;
        self
    }

    #[must_use]
    pub fn with_options(mut self, options: PolicyOptions) -> Self {
        self.options = options;
        self
    }

    /// 配置重载后换上新的规则快照；进行中的判决用的仍是旧快照。
    pub fn replace_rules(&self, rules: PermissionRules) {
        *self.rules.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(rules);
    }

    fn snapshot(&self) -> Arc<PermissionRules> {
        self.rules
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// 当前的规则快照（`/permissions` 的列表与修改，docs/specs/rust-cli/tui-manage.md §1.1）。
    pub fn rules(&self) -> Arc<PermissionRules> {
        self.snapshot()
    }

    /// Node 群协作会话的规则集（Node `loadProjectPermissionRuleset`：`scope = session` 时只用会话行的规则）：这个
    /// 会话的判决不用本机项目规则集，改用它。修复线 F2：以前续写这种会话时用的是项目规则。
    pub fn scope(
        &self,
        session: &SessionId,
        rules: &rust_cli_kernel_api::permission::ProjectRules,
    ) {
        self.scoped
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(session.clone(), rules.sourced());
    }

    /// 会话关闭时清掉它的群协作规则集（session-lifecycle.md §3.3 的关闭路径；再打开时 runtime 从会话行重放）。
    pub fn release(&self, session: &SessionId) {
        self.scoped
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(session);
    }

    /// 这个会话判决用的快照：群协作会话把 LocalSettings 换成它自己的规则集，其余会话就是当前快照。
    fn snapshot_for(&self, session: &SessionId) -> Arc<PermissionRules> {
        let base = self.snapshot();
        let scoped = self
            .scoped
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(session)
            .cloned();
        let Some(scoped) = scoped else {
            return base;
        };
        let mut rules = (*base).clone();
        rules.rules.retain(|rule| {
            rule.source != rust_cli_kernel_api::permission::RuleSource::LocalSettings
        });
        rules.rules.extend(scoped);
        Arc::new(rules)
    }

    /// 从执行环境取路径事实：cwd 取会话的 shell cwd（会话的当前目录，取不到时用工作区根；与文件工具的
    /// `env_shell::current_dir` 同一来源，tools-files.md §3.3），命令输出目录取同一份 shell 状态。
    async fn path_context(&self, base: &PermissionRules, context: &ToolUseContext) -> PathContext {
        let workspace = &context.workspace;
        let shell = context
            .env
            .shell_state(workspace, &context.session)
            .await
            .ok();
        let facts = context.env.path_facts(workspace).await.ok();
        // 计划文件与 ExitPlanMode、计划模式 attachment 同一来源（kernel-api `PlanFiles`）：
        // `<工作区>/.zcode/plans/plan-<会话>.md`（tools-more.md §2.2）。
        let plans = facts
            .as_ref()
            .and_then(|facts| PlanFiles::in_workspace(facts, &workspace.root, &context.session));
        let mut roots = vec![workspace.root.as_str().to_owned()];
        roots.extend(
            base.additional_directories
                .iter()
                .map(|dir| dir.as_str().to_owned()),
        );
        PathContext {
            cwd: shell.as_ref().map_or_else(
                || workspace.root.as_str().to_owned(),
                |shell| shell.cwd.as_str().to_owned(),
            ),
            roots,
            home: facts
                .as_ref()
                .and_then(|facts| facts.home.as_ref())
                .map(|home| home.as_str().to_owned()),
            style: facts.map_or(PathStyle::Posix, |facts| facts.style),
            internal: shell
                .and_then(|shell| shell.outputs)
                .map(|outputs| vec![outputs.as_str().to_owned()])
                .unwrap_or_default(),
            plans,
        }
    }
}

impl PolicyGate {
    /// 与 [`PermissionGate::decide`] 相同，另把 `allowed` 当作这一次判决的 allow 规则（skill 的
    /// `allowed-tools`，docs/specs/rust-cli/ext.md §2.5）。
    /// deny、ask 规则仍然优先。
    pub async fn decide_with_allowed(
        &self,
        tool: &dyn Tool,
        input: &Value,
        context: &ToolUseContext,
        allowed: &[PermissionRule],
    ) -> PermissionDecision {
        let base = self.snapshot_for(&context.session);
        let rules = Rules {
            base: &base,
            session: allowed,
        };
        let paths = self.path_context(&base, context).await;
        let root = context.workspace.root.as_str();
        let own = tool.check_permissions(input, context).await;
        let name = tool.name();
        let evaluate = |mode: PermissionMode| {
            let inputs = Inputs {
                mode,
                rules: &rules,
                paths: &paths,
                root,
                dangerous: guarded::applies(mode, name, input, paths.style),
            };
            let (check, updated) = chain::own(name, own.clone(), input, &inputs);
            chain::inner(name, check, updated, &inputs)
        };
        let result = evaluate(context.mode);
        let call = Call {
            tool: name,
            input,
            session: &context.session,
            mode: context.mode,
        };
        let options = Outer {
            auto_in_plan: self.options.auto_in_plan,
            avoid_prompts: base.avoid_prompts,
        };
        chain::outer(
            &call,
            result,
            || evaluate(PermissionMode::AcceptEdits),
            self.classifier.as_ref(),
            options,
        )
        .await
    }
}

#[async_trait]
impl PermissionGate for PolicyGate {
    async fn decide(
        &self,
        tool: &dyn Tool,
        input: &Value,
        context: &ToolUseContext,
    ) -> PermissionDecision {
        self.decide_with_allowed(tool, input, context, &[]).await
    }

    /// hook 放行或询问时仍然生效的检查（不可绕过的权限关卡，
    /// docs/specs/rust-cli/hooks.md §6）：按 bypassPermissions 跑内层判决链，仍然拒绝或询问的就是
    /// deny / ask 规则、工具检查的拒绝与规则 / 安全路径的询问。
    async fn check_rules(
        &self,
        tool: &dyn Tool,
        input: &Value,
        context: &ToolUseContext,
    ) -> Option<PermissionDecision> {
        let base = self.snapshot_for(&context.session);
        let rules = Rules {
            base: &base,
            session: &[],
        };
        let paths = self.path_context(&base, context).await;
        // guarded 按 guarded 跑：hook 放行时危险命令仍然询问（Node user-once 不让 hook 替用户批准，修复线 F20）。
        let mode = match context.mode {
            PermissionMode::Guarded => PermissionMode::Guarded,
            _ => PermissionMode::BypassPermissions,
        };
        let inputs = Inputs {
            mode,
            rules: &rules,
            paths: &paths,
            root: context.workspace.root.as_str(),
            dangerous: guarded::applies(mode, tool.name(), input, paths.style),
        };
        let own = tool.check_permissions(input, context).await;
        let (check, updated) = chain::own(tool.name(), own, input, &inputs);
        match chain::inner(tool.name(), check, updated, &inputs) {
            chain::Inner::Allow { .. } => None,
            chain::Inner::Deny { message } => Some(PermissionDecision::Deny { message }),
            chain::Inner::Ask {
                message,
                suggestions,
                ..
            } => Some(PermissionDecision::Ask {
                message,
                suggestions,
            }),
        }
    }

    /// guarded 的危险命令只批准这一次（Node `approvalMode: "user-once"`，修复线 F20）：执行管线据此不跑
    /// PermissionRequest hooks。路径风格（方言）与判决同一来源（执行环境的 `path_facts`）。
    async fn ask_once(&self, tool: &dyn Tool, input: &Value, context: &ToolUseContext) -> bool {
        if context.mode != PermissionMode::Guarded {
            return false;
        }
        let style = context
            .env
            .path_facts(&context.workspace)
            .await
            .map_or(PathStyle::Posix, |facts| facts.style);
        guarded::applies(context.mode, tool.name(), input, style)
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
