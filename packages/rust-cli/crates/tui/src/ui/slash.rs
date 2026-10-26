//! 斜杠命令的执行（docs/specs/rust-cli/tui.md §6）。命令表在 `commands.rs`；依赖其他工作线的命令只
//! 提示“未实现”，不发命令。
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::provider::CatalogModel;
use rust_cli_platform::model_name::{self, LookupError};

use super::app::{Action, App, Local, Overlay};
use super::commands::{self, COMMANDS};
use super::i18n;
use super::list::{Item, ListView, Purpose, SEP};

impl App {
    /// 执行一条斜杠命令（`text` 以 `/` 开头）。
    pub(super) fn slash(&mut self, text: &str) -> Option<Action> {
        let rest = text.trim_start_matches('/');
        let (name, args) = rest
            .split_once(char::is_whitespace)
            .map_or((rest, ""), |(name, args)| (name, args.trim()));
        let Some(command) = commands::find(name) else {
            self.info(i18n::unrecognized(self.lang, name));
            return None;
        };
        if !command.implemented {
            self.info(i18n::not_implemented(self.lang, command.name));
            return None;
        }
        // `/usage`：API 时长与统计要问内核（usage.md §4、§5）；`/stats` 开 Stats 标签页（按输入的名字选）。
        if command.name == "usage" {
            return Some(Action::Manage(super::manage::Manage::Usage {
                stats: name == "stats",
            }));
        }
        // L28 的命令与带参数的 `/clear`、`/plan`、`/copy`、`/status`（tui-display.md）。
        if let Some(action) = self.slash_more(command.name, args) {
            return action;
        }
        match command.name {
            "help" => self.help(),
            "memory" => self.memory(),
            "clear" => Some(Action::Clear),
            "model" if args.is_empty() => Some(Action::OpenModels),
            "model" => Some(Action::PickModel(args.to_owned())),
            "plan" => self.toggle_plan(),
            "compact" => Some(Action::Compact((!args.is_empty()).then(|| args.to_owned()))),
            "rewind" => Some(Action::OpenRewind),
            "resume" if args.is_empty() => Some(Action::OpenResume),
            "resume" => Some(Action::Resume(args.to_owned())),
            // 按用户输入分叉（session-lifecycle.md §6.5.3，F75）。参数忽略、照样打开选择器（用户决定，F78；F75 时报错）。
            "fork" => Some(Action::OpenFork),
            // 不带名字时用模型起名（titles.md §8）。
            "rename" if args.is_empty() => Some(Action::GenerateName),
            "rename" => Some(Action::Rename(args.to_owned())),
            "context" => Some(Action::Context {
                all: args.eq_ignore_ascii_case("all"),
            }),
            // 不带参数：只读面板；其余整行发给内核（会话 actor 是目标的唯一 owner，goal.md §2）。
            "goal" if args.is_empty() => Some(Action::ShowGoal),
            "goal" => Some(Action::Submit {
                text: text.trim().to_owned(),
                delivery: rust_cli_kernel_api::session_api::Delivery::Queue,
            }),
            "locale" => self.locale_command(args),
            "login" => self.login_command(args),
            "logout" => self.logout_command(args),
            "exit" => Some(Action::Quit),
            _ => {
                self.info(i18n::not_implemented(self.lang, command.name));
                None
            }
        }
    }

    /// `/help`：命令、说明与别名；未实现的命令标出来。参数提示在输入命令时显示，这里不列（长的提示会
    /// 把说明挤出屏幕）。
    fn help(&mut self) -> Option<Action> {
        let rows = COMMANDS
            .iter()
            .map(|command| {
                let usage = format!("/{}", command.name);
                let mut extra = commands::aliases_of(command.name)
                    .iter()
                    .map(|alias| format!("/{alias}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                if !command.implemented {
                    if !extra.is_empty() {
                        extra.push(' ');
                    }
                    extra.push_str(self.strings.help_not_implemented);
                }
                (usage, command.description(self.lang).to_owned(), extra)
            })
            .chain(self.shortcut_rows())
            .collect();
        self.overlay = Some(Overlay::Page {
            local: Local::Table {
                title: self.strings.help_title.into(),
                rows,
                empty: String::new(),
            },
            scroll: 0,
        });
        None
    }

    /// `/memory`（tui-input.md §4.2）：列出规则与记忆文件，选中后用外部编辑器打开。
    fn memory(&mut self) -> Option<Action> {
        let items = self
            .memory_files
            .iter()
            .enumerate()
            .map(|(index, file)| super::list::Item {
                label: file.label.clone(),
                description: file.display.clone(),
                value: index.to_string(),
                ..super::list::Item::default()
            })
            .collect();
        let mut list = super::list::ListView::new(
            super::list::Purpose::Memory,
            super::i18n_input::memory_title(self.lang).to_owned(),
            items,
            String::new(),
        );
        list.subtitle = Some(super::i18n_input::memory_subtitle(self.lang).to_owned());
        self.overlay = Some(Overlay::List(list));
        None
    }

    /// `/help` 末尾的“快捷键”一节（tui-input.md §5）：空一行、标题行，再逐行列出按键与说明。
    fn shortcut_rows(&self) -> Vec<(String, String, String)> {
        let mut rows = vec![
            (String::new(), String::new(), String::new()),
            (
                super::i18n_input::shortcuts_title(self.lang).to_owned(),
                String::new(),
                String::new(),
            ),
        ];
        rows.extend(super::shortcuts::shortcuts().iter().map(|entry| {
            (
                entry.0.to_owned(),
                super::shortcuts::describe(self.lang, entry).to_owned(),
                String::new(),
            )
        }));
        rows
    }

    /// `/plan`：打开或关掉 plan 开关，权限模式不变（修复线 F20：以前切到 plan 模式、记下之前的模式，再执行一次
    /// 切回去；进入前的模式现在一直在会话 actor 里）。
    ///
    /// 修复线 L-A（tui-manage.md §11.2）：打开时照对照实现记 `Enabled plan mode`（加固定说明）；关掉时对照实现没有对应的结果
    /// （它的 `/plan` 不关），不记。
    fn toggle_plan(&mut self) -> Option<Action> {
        let enabling = !self.view.plan();
        if enabling {
            self.records
                .push(rust_cli_kernel_api::user_command::LocalCommandRecord {
                    name: "plan".to_owned(),
                    args: String::new(),
                    output: rust_cli_kernel_api::user_command::LocalCommandOutput::Stdout(
                        "Enabled plan mode".to_owned(),
                    ),
                    caveat: true,
                    meta: Vec::new(),
                });
        }
        Some(Action::SetPlan(enabling))
    }

    /// `/status` 的权限一行：权限模式；plan 开着时前面加 `Plan mode · `（修复线 F20，tui.md §7）。
    fn permissions_label(&self) -> String {
        let mode = i18n::mode_label(self.lang, self.view.mode());
        if self.view.plan() {
            let plan = i18n::mode_label(
                self.lang,
                rust_cli_kernel_api::permission::PermissionMode::Plan,
            );
            format!("{plan} · {mode}")
        } else {
            mode.to_owned()
        }
    }

    /// `/status`：版本、模型、目录、权限模式、会话，以及 MCP 的汇总（runner 取回后调用，tui-display.md §4.9）。
    pub fn status(&mut self, mcp: String) -> Option<Action> {
        let strings = self.strings;
        let selection = self.view.selection();
        let model = if selection.provider.is_empty() {
            selection.model.clone()
        } else {
            format!("{}/{}", selection.provider, selection.model)
        };
        let rows = vec![
            (strings.status_version.to_owned(), self.version.clone()),
            (strings.status_model.to_owned(), model),
            (strings.status_directory.to_owned(), self.workspace_label()),
            (
                strings.status_permissions.to_owned(),
                self.permissions_label(),
            ),
            (
                strings.status_session.to_owned(),
                self.session
                    .clone()
                    .unwrap_or_else(|| strings.status_no_session.to_owned()),
            ),
            (strings.status_mcp.to_owned(), mcp),
        ];
        self.overlay = Some(Overlay::Page {
            local: Local::Status(rows),
            scroll: 0,
        });
        None
    }

    /// 模型目录到达：打开模型列表（`/model`）。条目的值是 `provider SEP model SEP 团队项目 SEP 档位`（团队项目是
    /// 它的 JSON，没有时为空）。所有可用的套餐平铺，说明是供应商名；团队套餐每个团队项目一项（工作线 F49）。
    pub fn open_models(&mut self, models: &[CatalogModel]) {
        if models.is_empty() {
            self.info(self.strings.no_models);
            return;
        }
        let current = self.view.selection();
        let items = models
            .iter()
            .map(|model| {
                let is_current = model_name::is_entry(current, model);
                let level = current
                    .reasoning
                    .as_ref()
                    .filter(|_| is_current)
                    .and_then(|r| model.reasoning_levels.iter().position(|l| l == r))
                    .unwrap_or(model.reasoning_levels.len().saturating_sub(1));
                let (description, connection) = match &model.connection {
                    Some(connection) => (
                        i18n::team_plan(self.lang, &connection.vendor, &connection.name),
                        serde_json::to_string(&connection.id).unwrap_or_default(),
                    ),
                    None => (model.provider_name.clone(), String::new()),
                };
                Item {
                    label: model.model.clone(),
                    description,
                    current: is_current,
                    value: format!("{}{SEP}{}{SEP}{connection}", model.provider, model.model),
                    levels: model.reasoning_levels.clone(),
                    level,
                }
            })
            .collect();
        let list = ListView::new(
            Purpose::Model,
            self.strings.select_model.into(),
            items,
            self.strings.no_matches.into(),
        )
        .with_subtitle(self.strings.select_model_subtitle.into())
        .filterable();
        self.overlay = Some(Overlay::List(list));
    }
}

/// `/model 名称`：与命令行的 `--model` 同一个函数（`<provider>/<model>` 或模型 ID，取第一个匹配；团队套餐加
/// `[<团队>]`，docs/specs/rust-cli/cli-args.md §1.3）。
pub fn resolve_model(models: &[CatalogModel], name: &str) -> Result<ModelSelection, LookupError> {
    model_name::lookup(models, name).map(model_name::selection)
}
