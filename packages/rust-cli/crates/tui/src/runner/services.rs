//! 入口注入的服务与只读查询的执行（docs/specs/rust-cli/tui.md §7.1、§7.2，context-usage.md §6）：界面语言的
//! 写回、登录与退出、上下文的计数。每个操作是一个 future，结果作为 [`ServiceDone`] 回到事件循环；状态只在
//! 事件循环里改。
use futures_util::FutureExt;
use tokio::sync::oneshot;

use super::Loop;
use super::session::Done;
use crate::services::{
    Authorize, CancellationToken, Configured, Family, LoginFailure, RedactedString, UiLocale,
};
use crate::ui::doorway;
use crate::ui::i18n;
use crate::ui::session_page::SessionFacts;
use rust_cli_kernel_api::context_usage::{ContextUsage, ContextUsageQuery};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::session_api::KernelError;

/// 服务操作的结果。
pub(crate) enum ServiceDone {
    /// 界面语言写回完成：成功时是配置文件的路径。
    Locale {
        locale: UiLocale,
        result: Result<String, String>,
    },
    /// 浏览器授权的地址到了（通道被关掉时为 None：登录在拿到地址之前就结束了）。
    LoginUrl {
        family: Family,
        authorize: Option<Authorize>,
    },
    /// 浏览器授权登录结束。
    Login(Result<Configured, LoginFailure>),
    /// 手工 key 保存结束。
    ApiKey(Result<Configured, String>),
    /// 上下文的计数与它问的会话（装箱：比其他变体大得多）。
    Context {
        usage: Box<Result<ContextUsage, KernelError>>,
        session: Option<SessionId>,
    },
    /// 统计的页 Session 标签的事实（装箱，同上）。
    SessionFacts(Box<SessionFacts>),
}

/// 只属于事件循环的服务状态。
#[derive(Default)]
pub(super) struct ServiceState {
    /// 界面语言的写回：同时只有一次在途，在途期间又输入的只留最后一个（tui.md §7.1）。
    locale: super::latest::Latest<UiLocale>,
    /// 在途的浏览器授权登录的取消令牌：登录状态的唯一 owner（tui.md §7.2）。
    login: Option<CancellationToken>,
    /// 上下文的计数：同时只有一次在途，在途时又要问的，答案回来后再问一次（tui-panels.md §4.1）。
    context: super::latest::Latest<()>,
    /// Session 标签的事实：同上，同时只有一次在途（tui-panels.md §4.5）。
    facts: super::latest::Latest<()>,
}

impl Loop<'_> {
    /// `/locale <值>`：先写回配置文件，成功后再切换（Node 先写后改）。
    pub(super) fn save_locale(&mut self, locale: UiLocale) {
        let Some(locale) = self.services.locale.start(locale) else {
            return;
        };
        let store = self.options.services.locale_store.clone();
        // 写入在输入时就开始；完成后回到事件循环。
        let saving = store.save(locale);
        self.pending.push(
            async move {
                let result = saving.await;
                Done::Service(ServiceDone::Locale { locale, result })
            }
            .boxed(),
        );
    }

    /// 浏览器授权登录（tui.md §7.2）：同时只有一个；地址与结果分别作为 `Done` 回来。
    pub(super) fn login(&mut self, family: Family) {
        let account = self.options.services.account.clone();
        if self.services.login.is_some() {
            return;
        }
        let cancel = CancellationToken::new();
        self.services.login = Some(cancel.clone());
        let (sender, receiver) = oneshot::channel();
        let login = account.login(family, sender, cancel);
        self.pending.push(
            async move {
                let authorize = receiver.await.ok();
                Done::Service(ServiceDone::LoginUrl { family, authorize })
            }
            .boxed(),
        );
        self.pending
            .push(async move { Done::Service(ServiceDone::Login(login.await)) }.boxed());
    }

    /// 上下文的计数（tui-panels.md §4.1）：数字的卡或上下文的页开着时，内核按主会话的当前视图算出用量，只读。卡打开、
    /// 每次模型请求结束（`refresh_stats`）、看的会话换了时都经这里；不开着时什么也不做。
    pub(super) fn count_context(&mut self) {
        let Some(session) = self.app.context_wanted() else {
            return;
        };
        if self.services.context.start(()).is_none() {
            return;
        }
        let kernel = self.kernel.clone();
        let query = ContextUsageQuery {
            session: session.clone(),
            workspace: self.app.view.workspace().clone(),
            selection: self.app.view.selection().clone(),
            // 判决用的模式（plan 开时是 plan，修复线 F20）。
            mode: self.app.view.execution().effective(),
        };
        self.pending.push(
            async move {
                let usage = kernel.context_usage(query).await;
                Done::Service(ServiceDone::Context {
                    usage: Box::new(usage),
                    session,
                })
            }
            .boxed(),
        );
    }

    /// 统计的页 Session 标签的事实（tui-panels.md §4.5）：显示 Session 时问 MCP 服务器、hooks、内置家族的登录状态、模型目录里的
    /// provider 名字与工作区的分支，一起回来。分支按目录读（`project_sessions` 的每一行都是它目录现在的分支，同 ← 项目页），所以问这个目录的一个会话
    /// 就够了；这个目录还没有会话时没有分支。打开或换到 Session、每次模型请求结束时经这里；不显示 Session 时什么也不做。
    pub(super) fn session_facts(&mut self) {
        if !self.app.session_facts_wanted() || self.services.facts.start(()).is_none() {
            return;
        }
        let kernel = self.kernel.clone();
        let account = self.options.services.account.clone();
        let models = self.options.models.clone();
        let workspace = self.app.view.workspace().clone();
        let query = rust_cli_kernel_api::store::SessionQuery {
            environment: workspace.environment.clone(),
            root: Some(workspace.root),
            order: Default::default(),
            limit: 1,
        };
        self.pending.push(
            async move {
                let catalog = async {
                    match &models {
                        Some(directory) => directory.catalog().await.models,
                        None => Vec::new(),
                    }
                };
                let (mcp, hooks, families, found, catalog) = futures_util::join!(
                    kernel.mcp_servers(),
                    kernel.hooks(),
                    account.families(),
                    kernel.project_sessions(query),
                    catalog,
                );
                let mut providers: Vec<(String, String)> = Vec::new();
                for model in catalog {
                    if !providers.iter().any(|(id, _)| *id == model.provider) {
                        providers.push((model.provider, model.provider_name));
                    }
                }
                let branch = found
                    .ok()
                    .and_then(|found| found.sessions.into_iter().next())
                    .and_then(|session| session.branch);
                Done::Service(ServiceDone::SessionFacts(Box::new(SessionFacts {
                    mcp,
                    hooks,
                    families,
                    providers,
                    branch,
                })))
            }
            .boxed(),
        );
    }

    /// esc：取消是一条消息，结果仍由登录的 future 给出（已经写完就是成功）。
    pub(super) fn cancel_login(&mut self) {
        if let Some(cancel) = &self.services.login {
            cancel.cancel();
        }
    }

    pub(super) fn configure_key(&mut self, family: Family, key: RedactedString) {
        let account = self.options.services.account.clone();
        let saving = account.configure_api_key(family, key);
        self.pending
            .push(async move { Done::Service(ServiceDone::ApiKey(saving.await)) }.boxed());
    }

    /// 登录或密钥的结果：成功时门道关上、说接上了什么，再重读模型目录、切到登录给出的模型（tui.md §7.2，D8）；失败时原因
    /// 留在门道里（门道关着时在输入框上方）。
    fn configured(&mut self, result: Result<Configured, String>) {
        match result {
            Ok(configured) => {
                let says = if configured.oauth {
                    doorway::signed_in_says(configured.family, configured.user.as_deref())
                } else {
                    doorway::keyed_says(configured.family, &configured.model)
                };
                self.app.door_connected(says);
                if self.options.models.is_some() {
                    self.catalog(Some(configured.model));
                }
            }
            Err(reason) => self.app.door_failed(reason),
        }
    }

    pub(super) fn on_service(&mut self, done: ServiceDone) {
        let lang = self.app.lang;
        match done {
            ServiceDone::LoginUrl { family, authorize } => {
                if let Some(authorize) = authorize {
                    self.app.door_url(family, authorize.url);
                }
            }
            ServiceDone::Login(result) => {
                self.services.login = None;
                match result {
                    Ok(configured) => self.configured(Ok(configured)),
                    Err(LoginFailure::Cancelled) => self.app.door_cancelled(),
                    Err(LoginFailure::Failed(reason)) => self.configured(Err(reason)),
                }
            }
            ServiceDone::ApiKey(result) => self.configured(result),
            // 上下文的计数：答案只进 `App.context_count`（卡的组成与页都读它）；问的不是现在看的会话时丢掉再问，在途时又
            // 要问的现在问（tui-panels.md §4.1）。失败不进 transcript（每次请求结束都会再问）：页上代替 `Counting…`。
            ServiceDone::Context { usage, session } => {
                let again = self.services.context.finish().is_some();
                let result = (*usage).map_err(|error| i18n::rejected(lang, &error.to_string()));
                let stale = self.app.context_counted(
                    session,
                    result,
                    std::time::Instant::now(),
                    self.now_ms(),
                );
                if again || stale {
                    self.count_context();
                }
            }
            // Session 标签的事实：替换上一次的；在途时又要问的现在问（§4.5）。
            ServiceDone::SessionFacts(facts) => {
                let again = self.services.facts.finish().is_some();
                self.app.session_facts_arrived(*facts);
                if again {
                    self.session_facts();
                }
            }
            ServiceDone::Locale { locale, result } => {
                let next = self.services.locale.finish();
                match result {
                    Ok(path) => {
                        let lang = i18n::resolve(Some(locale.as_str()), &self.options.vars);
                        self.app.set_lang(lang);
                        self.app.info(i18n::locale_switched(
                            lang,
                            locale.as_str(),
                            lang.code(),
                            &path,
                        ));
                    }
                    Err(reason) => self.app.error(i18n::locale_failed(self.app.lang, &reason)),
                }
                if let Some(next) = next {
                    self.save_locale(next);
                }
            }
        }
    }
}
