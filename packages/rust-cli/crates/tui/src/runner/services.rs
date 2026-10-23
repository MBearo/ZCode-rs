//! 入口注入的服务与只读查询的执行（docs/specs/rust-cli/tui.md §7.1、§7.2，context-usage.md §6）：界面语言的
//! 写回、登录与退出、`/context`。每个操作是一个 future，结果作为 [`ServiceDone`] 回到事件循环；状态只在
//! 事件循环里改。
use futures_util::FutureExt;
use ratatui::backend::Backend;
use tokio::sync::oneshot;

use super::Loop;
use super::session::Done;
use crate::services::{
    Authorize, CancellationToken, Configured, Family, LoginFailure, RedactedString, UiLocale,
};
use crate::ui::app::Local;
use crate::ui::i18n;
use rust_cli_kernel_api::context_usage::{ContextUsage, ContextUsageQuery};
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
    /// 退出结束：成功时是凭据文件的路径。
    Logout(Result<String, String>),
    /// `/context` 的结果（装箱：比其他变体大得多）。
    Context {
        usage: Box<Result<ContextUsage, KernelError>>,
        all: bool,
    },
}

/// 只属于事件循环的服务状态。
#[derive(Default)]
pub(super) struct ServiceState {
    /// 有一次界面语言写回在途。
    locale_writing: bool,
    /// 在途期间又输入的语言：只留最后一个，在途的完成后再写（两次写入不交错，tui.md §7.1）。
    locale_next: Option<UiLocale>,
    /// 在途的浏览器授权登录的取消令牌：登录状态的唯一 owner（tui.md §7.2）。
    login: Option<CancellationToken>,
}

impl<B: Backend> Loop<'_, B> {
    /// `/locale <值>`：先写回配置文件，成功后再切换（Node 先写后改）。
    pub(super) fn save_locale(&mut self, locale: UiLocale) {
        let Some(store) = self.options.services.locale_store.clone() else {
            self.app.info(i18n::locale_unavailable(self.app.lang));
            return;
        };
        if self.services.locale_writing {
            self.services.locale_next = Some(locale);
            return;
        }
        self.services.locale_writing = true;
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
        let lang = self.app.lang;
        let Some(account) = self.options.services.account.clone() else {
            self.app.login_closed();
            self.app.info(i18n::login_unavailable(lang, family, false));
            return;
        };
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

    /// `/context`：内核按会话的当前视图（没有会话时按新会话的模型与模式）算出用量，只读。
    pub(super) fn context_usage(&mut self, all: bool) {
        let kernel = self.kernel.clone();
        let query = ContextUsageQuery {
            session: self.session.as_ref().map(|session| session.id().clone()),
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
                    all,
                })
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
        let Some(account) = self.options.services.account.clone() else {
            self.app.login_closed();
            self.app
                .info(i18n::login_unavailable(self.app.lang, family, true));
            return;
        };
        let saving = account.configure_api_key(family, key);
        self.pending
            .push(async move { Done::Service(ServiceDone::ApiKey(saving.await)) }.boxed());
    }

    pub(super) fn logout(&mut self) {
        let Some(account) = self.options.services.account.clone() else {
            self.app.info(i18n::logout_unavailable(self.app.lang));
            return;
        };
        let logout = account.logout();
        self.pending
            .push(async move { Done::Service(ServiceDone::Logout(logout.await)) }.boxed());
    }

    /// 登录成功：关闭视图、提示结果，再重读模型目录、切到登录给出的模型（tui.md §7.2，D8）。
    fn configured(&mut self, result: Result<Configured, String>) {
        self.app.login_closed();
        match result {
            Ok(configured) => {
                self.app.info(i18n::configured(self.app.lang, &configured));
                if self.options.models.is_some() {
                    self.catalog(Some(configured.model));
                }
            }
            Err(reason) => self.app.error(reason),
        }
    }

    pub(super) fn on_service(&mut self, done: ServiceDone) {
        let lang = self.app.lang;
        match done {
            ServiceDone::LoginUrl { family, authorize } => {
                if let Some(authorize) = authorize {
                    self.app
                        .info(i18n::login_open_url(lang, family, &authorize.url));
                }
            }
            ServiceDone::Login(result) => {
                self.services.login = None;
                match result {
                    Ok(configured) => self.configured(Ok(configured)),
                    Err(LoginFailure::Cancelled) => {
                        let status = self.app.strings.login_cancelled;
                        self.app.login_back(status);
                    }
                    Err(LoginFailure::Failed(reason)) => self.configured(Err(reason)),
                }
            }
            ServiceDone::ApiKey(result) => self.configured(result),
            // 独立页面（L30 起不再插进 transcript，tui-panels.md §4.1）。
            ServiceDone::Context { usage, all } => match *usage {
                Ok(usage) => {
                    self.app.overlay = Some(crate::ui::app::Overlay::Page {
                        local: Local::Context {
                            usage: Box::new(usage),
                            all,
                        },
                        scroll: 0,
                    });
                }
                Err(error) => self.app.error(i18n::rejected(lang, &error.to_string())),
            },
            ServiceDone::Logout(result) => match result {
                Ok(path) => self.app.info(i18n::logged_out(lang, &path)),
                Err(reason) => self.app.error(reason),
            },
            ServiceDone::Locale { locale, result } => {
                self.services.locale_writing = false;
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
                if let Some(next) = self.services.locale_next.take() {
                    self.save_locale(next);
                }
            }
        }
    }
}
