//! 设置页在 runner 里的部分（docs/specs/rust-cli/tui-kit.md §4.2「设置」）：打开一段时按入口注入的服务与内核读出它交给
//! 页，写改动，在编辑器里打开文件，写 `theme.json`。读与写照 Puffin 的设置 host（`settings.rs`）：
//! 读同一些来源、写同一些文件。回执在事件循环的 `FuturesUnordered` 里，不另起任务；页只在回执到时变。
use std::sync::Arc;

use futures_util::FutureExt;
use rust_cli_kernel_api::catalog::AgentOrigin;
use rust_cli_kernel_api::manage::{McpAction, McpControl, McpServerInfo, McpServerState};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::{PermissionRulesChange, PermissionRulesQuery, RuleChange};
use rust_cli_kernel_api::session_api::Kernel;
use rust_cli_tui_kit::theme::scheme::ThemeChoice;

use super::Loop;
use super::session::Done;
use crate::services::{Services, UiConfig, UiLocale};
use crate::ui::settings::data::{self, Scope, Section, SectionData};
use crate::ui::settings::page::{Change, Page, Write};
use crate::ui::settings::{Request, Saved};

/// provider 的改动这一版还没有（同 cli 的 Puffin host）。
const UNAVAILABLE: &str = "This isn't available in this version yet";

/// 设置页的回执。
pub(super) enum SettingsDone {
    Read(Result<SectionData, String>),
    Wrote {
        page: Page,
        key: String,
        write: Write,
        result: Result<Saved, String>,
    },
    Edited {
        path: String,
        result: Result<(), String>,
    },
    Looked(Result<(), String>),
    /// 测试连接的回答：`key` 是那一行，`Ok` 是用了多少秒。
    Tested {
        key: String,
        result: Result<f64, String>,
    },
}

/// 读写要的：都是可以搬进 future 的。
#[derive(Clone)]
struct Sources {
    kernel: Arc<dyn Kernel>,
    services: Services,
    ui: UiConfig,
    models: Option<Arc<dyn rust_cli_kernel_api::provider::ModelDirectory>>,
    workspace: rust_cli_kernel_api::env::WorkspaceRef,
    session: Option<rust_cli_kernel_api::ids::SessionId>,
    selection: ModelSelection,
    locale: String,
}

fn mcp_of(server: McpServerInfo) -> data::McpServer {
    data::McpServer {
        name: server.name,
        enabled: server.state != McpServerState::Disabled,
        state: match server.state {
            McpServerState::Pending => data::McpState::Connecting,
            McpServerState::Connected => data::McpState::Connected,
            McpServerState::Failed => data::McpState::Failed,
            McpServerState::Disabled => data::McpState::Disabled,
        },
        tools: server.tools,
        error: server.reason.unwrap_or_default(),
        machine: server.machine.remote().map(str::to_owned),
        ..data::McpServer::default()
    }
}

impl Sources {
    async fn read(self, section: Section) -> Result<SectionData, String> {
        let kernel = self.kernel.clone();
        Ok(match section {
            Section::General => SectionData::General(data::General {
                auto_compact: self.services.compact_at.as_ref().map(|(at, _)| at.get()),
                locale: self.locale.clone(),
            }),
            Section::Models => SectionData::Models(self.models().await),
            Section::Permissions => SectionData::Permissions(self.permissions().await?),
            Section::Memory => SectionData::Memory(self.memory().await),
            // 子代理与技能只看工作区（同 Puffin 的 host）。
            Section::Subagents => SectionData::Subagents(
                kernel
                    .agent_definitions(&self.workspace)
                    .await
                    .into_iter()
                    .map(|agent| data::Subagent {
                        scope: match agent.origin {
                            AgentOrigin::BuiltIn => Scope::Builtin,
                            AgentOrigin::Plugin => Scope::Plugin,
                            AgentOrigin::User => Scope::User,
                            AgentOrigin::Project => Scope::Project,
                        },
                        name: agent.name,
                        description: agent.description,
                        path: String::new(),
                        tools: agent.tools,
                        model: agent.model,
                    })
                    .collect(),
            ),
            Section::Skills => SectionData::Skills(
                kernel
                    .user_commands(&self.workspace)
                    .await
                    .into_iter()
                    .filter(|command| !command.hidden)
                    .map(|command| data::Skill {
                        scope: if command.name.contains(':') {
                            Scope::Plugin
                        } else {
                            Scope::User
                        },
                        name: command.name,
                        description: command.description,
                        path: String::new(),
                    })
                    .collect(),
            ),
            Section::Commands => SectionData::Commands(
                kernel
                    .user_commands(&self.workspace)
                    .await
                    .into_iter()
                    .map(|command| data::CommandItem {
                        scope: if command.name.contains(':') {
                            Scope::Plugin
                        } else {
                            Scope::User
                        },
                        name: command.name,
                        description: command.description,
                        argument_hint: command.argument_hint.unwrap_or_default(),
                        path: String::new(),
                    })
                    .collect(),
            ),
            Section::Hooks => SectionData::Hooks(
                kernel
                    .hooks()
                    .await
                    .into_iter()
                    .map(|hook| data::Hook {
                        scope: data::scope_of(&hook.source),
                        event: hook.event,
                        matcher: hook.matcher.unwrap_or_default(),
                        command: hook.display,
                        timeout: None,
                        source: hook.source,
                    })
                    .collect(),
            ),
            Section::Mcp => {
                SectionData::Mcp(kernel.mcp_servers().await.into_iter().map(mcp_of).collect())
            }
        })
    }

    async fn models(&self) -> data::Models {
        let selection = &self.selection;
        let default_model = (!selection.model.is_empty()).then(|| data::Pick {
            provider: selection.provider.clone(),
            model: selection.model.clone(),
            effort: selection.reasoning.clone().unwrap_or_default(),
        });
        let catalog = match &self.models {
            Some(directory) => directory.catalog().await.models,
            None => Vec::new(),
        };
        // 内置的家族：账号端口给的登录状态与它列出的全部模型，关着的也在（platform `family_accounts`，Puffin 的 host 读同
        // 一份，tui-kit.md §4.5）。以前这里从目录里筛：目录只列能用的模型，关着的不见了。
        let families = self.services.account.families().await;
        let mut providers: Vec<data::Provider> = Vec::new();
        // 家族的 provider 在家族页上，不在「Your providers」里（Puffin 的两张表分开）。
        for model in catalog
            .iter()
            .filter(|m| !families.iter().any(|f| f.provider == m.provider))
        {
            let entry = data::FamilyModel {
                id: model.model.clone(),
                enabled: true,
                context_window: Some(model.context_window),
            };
            match providers.iter_mut().find(|p| p.id == model.provider) {
                Some(provider) => provider.models.push(entry),
                None => providers.push(data::Provider {
                    id: model.provider.clone(),
                    name: model.provider_name.clone(),
                    models: vec![entry],
                }),
            }
        }
        data::Models {
            default_model,
            families,
            providers,
            catalog,
        }
    }

    async fn memory(&self) -> Vec<data::MemoryFile> {
        let mut files = Vec::new();
        for file in &self.ui.memory_files {
            let size = match &self.services.file_len {
                Some(len) => len(file.path.clone()).await,
                None => None,
            };
            files.push(data::MemoryFile {
                scope: file.label.clone(),
                path: file.path.clone(),
                exists: size.is_some(),
                size: size.unwrap_or(0),
            });
        }
        files
    }

    async fn permissions(&self) -> Result<data::Permissions, String> {
        let rules = self
            .kernel
            .permission_rules(PermissionRulesQuery {
                workspace: self.workspace.clone(),
                session: self.session.clone(),
            })
            .await
            .map_err(|error| error.to_string())?;
        let mut folders = vec![data::Folder {
            path: self.workspace.root.as_str().to_owned(),
            added: false,
        }];
        folders.extend(self.ui.add_dirs.iter().map(|path| data::Folder {
            path: path.clone(),
            added: true,
        }));
        Ok(data::Permissions {
            rules,
            files: self
                .ui
                .rule_files
                .iter()
                .map(|(source, behavior, path)| data::RuleFile {
                    source: *source,
                    behavior: *behavior,
                    path: path.clone(),
                })
                .collect(),
            folders,
        })
    }

    async fn change_rules(&self, change: RuleChange) -> Result<(), String> {
        self.kernel
            .change_permission_rules(PermissionRulesChange {
                workspace: self.workspace.clone(),
                session: self.session.clone(),
                change,
            })
            .await
            .map(drop)
            .map_err(|error| error.to_string())
    }

    /// 写一个改动。`Ok(Some(why))`：文件写了而这次运行没能用上它。
    /// 测试一个模型的连接（Puffin host 的 `test_model`）：platform 还不试密钥，两边的 host 都回答这一版还没有
    /// （tui-kit.md §4.5）。回答经 runner 回来，页上先画在测的样子，同 Puffin。
    async fn test(self, _provider: String, _model: String) -> Result<f64, String> {
        Err(UNAVAILABLE.to_owned())
    }

    async fn write(self, change: Change) -> Result<Saved, String> {
        let services = &self.services;
        match change {
            // 两边的 host 都还没有（tui-kit.md §4.5，同 Puffin host 的 `change`）。
            Change::Connection { .. }
            | Change::Model { .. }
            | Change::Provider { .. }
            | Change::NewProvider { .. }
            | Change::DeleteProvider { .. } => Err(UNAVAILABLE.to_owned()),
            Change::Locale { locale } => {
                let parsed = UiLocale::parse(&locale)
                    .ok_or_else(|| format!("unknown language: {locale}"))?;
                services.locale_store.save(parsed).await?;
                Ok(None)
            }
            Change::DefaultModel { pick } => {
                let connection = match &self.models {
                    Some(directory) => directory
                        .catalog()
                        .await
                        .models
                        .into_iter()
                        .find(|m| m.provider == pick.provider && m.model == pick.model)
                        .and_then(|m| m.connection)
                        .map(|c| Box::new(c.id)),
                    None => None,
                };
                let selection = ModelSelection {
                    reasoning: Some(pick.effort).filter(|e| !e.is_empty()),
                    connection,
                    ..ModelSelection::new(pick.provider, pick.model)
                };
                services.preferences.save_default_model(selection).await?;
                Ok(None)
            }
            Change::McpEnabled { name, enabled } => {
                let action = if enabled {
                    McpAction::Enable
                } else {
                    McpAction::Disable
                };
                services
                    .preferences
                    .save_mcp_enabled(name.clone(), enabled)
                    .await?;
                let control = McpControl {
                    action,
                    servers: vec![name],
                };
                let reload = self.kernel.mcp_control(control).await;
                Ok(reload.err().map(|error| error.to_string()))
            }
            Change::McpReconnect { name } => {
                let control = McpControl {
                    action: McpAction::Reconnect,
                    servers: vec![name],
                };
                self.kernel
                    .mcp_control(control)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(None)
            }
            Change::AddRule { behavior, rule } => {
                self.change_rules(RuleChange::Add {
                    behavior,
                    rules: vec![rule],
                })
                .await?;
                Ok(None)
            }
            Change::RemoveRule { rule } => {
                self.change_rules(RuleChange::Remove { rule }).await?;
                Ok(None)
            }
            // 内核没有替换：先加新的，再删旧的。加没成时什么也没变；删没成时新的已经在，经回答说旧的还在。
            Change::ReplaceRule {
                old,
                behavior,
                rule,
            } => {
                self.change_rules(RuleChange::Add {
                    behavior,
                    rules: vec![rule],
                })
                .await?;
                Ok(self
                    .change_rules(RuleChange::Remove { rule: old })
                    .await
                    .err())
            }
            Change::SignOut => {
                services.account.logout().await?;
                Ok(None)
            }
            // compact.md §2.3：写用户级配置，再换掉进程里的值，正在运行的会话从下一次判定起按它压缩；项目或环境钉住了
            // 它时写了也不生效。
            Change::AutoCompact { at } => {
                let (cell, pinned) = services.compact_at.clone().ok_or(UNAVAILABLE)?;
                services.preferences.save_compact_at(at).await?;
                if let Some(file) = pinned {
                    return Ok(Some(format!("{file} sets compact.autoThresholdTokens")));
                }
                cell.set(at);
                Ok(None)
            }
        }
    }
}

impl Loop<'_> {
    fn settings_sources(&self) -> Sources {
        Sources {
            kernel: self.kernel.clone(),
            services: self.options.services.clone(),
            ui: self.options.ui.clone(),
            models: self.options.models.clone(),
            workspace: self.app.view.workspace().clone(),
            session: self.outbox.session().map(|session| session.id().clone()),
            selection: self.app.view.selection().clone(),
            locale: self
                .options
                .ui
                .locale
                .map_or("auto", UiLocale::as_str)
                .to_owned(),
        }
    }

    /// 编辑器的名字（状态栏的 `⏎ open in vim`）：它的命令的第一个词的文件名；没有编辑器时为 ""。
    fn editor_name(&self) -> String {
        self.options
            .services
            .editor
            .source()
            .and_then(|(_, cmd)| {
                cmd.split_whitespace()
                    .next()
                    .map(|w| rust_cli_tui_kit::app::widgets::settings::file_name(w).to_owned())
            })
            .unwrap_or_default()
    }

    pub(super) fn settings_request(&mut self, request: Request) {
        let editor = self.editor_name();
        if let Some(ui) = self.app.settings.as_mut() {
            ui.editor = editor;
        }
        match request {
            Request::Read(sections) => {
                for section in sections {
                    let reading = self.settings_sources().read(section);
                    self.pending.push(
                        async move { Done::Settings(SettingsDone::Read(reading.await)) }.boxed(),
                    );
                }
            }
            Request::Write { page, key, write } => {
                let writing = self.settings_sources().write(write.change.clone());
                self.pending.push(
                    async move {
                        let result = writing.await;
                        Done::Settings(SettingsDone::Wrote {
                            page,
                            key,
                            write: *write,
                            result,
                        })
                    }
                    .boxed(),
                );
            }
            Request::Edit { path } => {
                // 编辑器总在（没设 $VISUAL / $EDITOR 时入口给缺省编辑器），所以这里不再有“没有编辑器”的提示。
                let editor = self.options.services.editor.clone();
                self.hand_over();
                self.pending.push(
                    async move {
                        let result = editor.open(path.clone()).await;
                        Done::Settings(SettingsDone::Edited { path, result })
                    }
                    .boxed(),
                );
            }
            Request::Test {
                key,
                provider,
                model,
            } => {
                let testing = self.settings_sources().test(provider, model);
                self.pending.push(
                    async move {
                        let result = testing.await;
                        Done::Settings(SettingsDone::Tested { key, result })
                    }
                    .boxed(),
                );
            }
            Request::Look(choice) => self.settings_look(choice),
            Request::Hosts => self.machine_request(crate::ui::machines::MachineRequest::Hosts),
        }
    }

    /// 样子已经换上：重画，写 `theme.json`（没有存储时只对本次运行有效）。
    fn settings_look(&mut self, choice: ThemeChoice) {
        self.cache.clear();
        self.renderer.invalidate();
        let Some(stores) = self.options.services.stores.clone() else {
            return;
        };
        let saving = stores.write(crate::services::StoreFile::Theme, choice.encode());
        self.pending
            .push(async move { Done::Settings(SettingsDone::Looked(saving.await)) }.boxed());
    }

    pub(super) fn on_settings(&mut self, done: SettingsDone) {
        let next = match done {
            SettingsDone::Read(result) => {
                self.app.settings_read(result);
                None
            }
            SettingsDone::Wrote {
                page,
                key,
                write,
                result,
            } => {
                if result.is_ok() {
                    self.settings_applied(&write.change);
                }
                self.app.settings_wrote(&page, &key, &write, result)
            }
            SettingsDone::Edited { path, result } => {
                self.take_back();
                self.app.settings_edited(&path, result)
            }
            SettingsDone::Looked(result) => {
                self.app.settings_looked(result);
                None
            }
            SettingsDone::Tested { key, result } => {
                self.app.settings_tested(&key, result);
                None
            }
        };
        if let Some(crate::ui::app::Action::Settings(request)) = next {
            self.settings_request(request);
        }
    }

    /// 写成了的改动在这次运行里生效：界面语言换了屏幕也换（旧界面有中文界面）；没有会话时新的缺省模型就是草稿的模型。
    fn settings_applied(&mut self, change: &Change) {
        match change {
            Change::Locale { locale } => {
                if let Some(parsed) = UiLocale::parse(locale) {
                    self.options.ui.locale = Some(parsed);
                    let lang = crate::ui::i18n::resolve(Some(parsed.as_str()), &self.options.vars);
                    self.app.set_lang(lang);
                }
            }
            Change::DefaultModel { pick } if self.outbox.session().is_none() => {
                let execution = self.app.view.execution();
                let selection = ModelSelection {
                    reasoning: Some(pick.effort.clone()).filter(|e| !e.is_empty()),
                    ..ModelSelection::new(pick.provider.clone(), pick.model.clone())
                };
                self.app.view.set_draft(selection, execution);
                self.release_for_model();
            }
            _ => {}
        }
    }
}
