//! 斜杠命令的执行（docs/specs/rust-cli/tui.md §6）。命令表在 `commands.rs`；依赖其他工作线的命令只
//! 提示“未实现”，不发命令。
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::model_name::{self, LookupError};
use rust_cli_kernel_api::provider::CatalogModel;
use rust_cli_tui_kit::app::widgets::model_picker::AUTO;

use super::app::{Action, App};
use super::commands;
use super::i18n;
use super::list::{Choice, Item, ListView, Purpose};

impl App {
    /// 执行一条斜杠命令（`text` 以 `/` 开头）：按命令表的 `run`（`commands.rs`）。
    pub(super) fn slash(&mut self, text: &str) -> Option<Action> {
        let rest = text.trim_start_matches('/');
        let (name, args) = rest
            .split_once(char::is_whitespace)
            .map_or((rest, ""), |(name, args)| (name, args.trim()));
        let Some(command) = commands::find(name) else {
            self.info(i18n::unrecognized(self.lang, name));
            return None;
        };
        let Some(run) = command.run else {
            self.info(i18n::not_implemented(self.lang, command.name));
            return None;
        };
        run(
            self,
            &commands::Call {
                typed: name,
                args,
                text,
            },
        )
    }

    /// `/model 词`、`/theme 词`（用户决定 2026-10-31，tui-kit.md §4.0「板」）：不直接换，草稿放回、打开列表、按词过滤
    /// （`/effort`、`/variant` 带什么词都只打开模型选择器，草稿是 `/model `）；不带词时直接打开列表。以前 `/model 名字`
    /// 按名字直接换、`/effort 档位` 直接换档位并存成缺省、`/theme 名字` 认得时直接换上。
    pub(super) fn list_by_words(
        &mut self,
        name: &str,
        call: &commands::Call<'_>,
    ) -> Option<Action> {
        if !call.args.is_empty() {
            // 别名 `/effort`、`/variant` 后面的词是档位，不是模型名：不拿它过滤。
            let alias = !call.typed.eq_ignore_ascii_case(name);
            let words = if alias { "" } else { call.args };
            self.composer.forget_submitted();
            self.composer.set_text(&format!("/{name} {words}"));
        }
        match name {
            "model" => self.request_models(),
            _ => self.theme_list(),
        }
    }

    /// `/plan`：打开或关掉 plan 开关，权限模式不变（修复线 F20：以前切到 plan 模式、记下之前的模式，再执行一次
    /// 切回去；进入前的模式现在一直在会话 actor 里）。
    ///
    /// 修复线 L-A（tui-manage.md §11.2）：打开时照对照实现记 `Enabled plan mode`（加固定说明）；关掉时对照实现没有对应的结果
    /// （它的 `/plan` 不关），不记。
    /// 开关计划模式（`/plan` 与 `/mode` 列表的计划模式那一行）。
    pub(super) fn toggle_plan(&mut self) -> Option<Action> {
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

    /// 模型目录到达：打开模型列表（`/model`）。条目的值是这个模型的选择（团队项目的连接原样带上，档位由列表给出）。
    /// 所有可用的套餐平铺，说明是供应商名；团队套餐每个团队项目一项（工作线 F49）。
    pub fn open_models(&mut self, models: &[CatalogModel]) {
        self.models_opening = false;
        if models.is_empty() {
            self.info(self.strings.no_models);
            return;
        }
        let current = self.view.selection();
        let items = models
            .iter()
            .map(|model| {
                let is_current = model_name::is_entry(current, model);
                // 最低一档之前的 `auto`（用户决定 2026-10-31，tui-kit.md §4.2 对应表的 `/model` 一行）：`/effort auto` 删掉
                // 之后，回到自动档位只有这一处。模型自己有 `auto` 一档时不加。
                let mut levels = model.reasoning_levels.clone();
                if !levels.is_empty() && !levels.iter().any(|l| l == AUTO) {
                    levels.insert(0, AUTO.to_owned());
                }
                let level = match current.reasoning.as_ref().filter(|_| is_current) {
                    Some(r) => levels.iter().position(|l| l == r),
                    None if is_current => levels.iter().position(|l| l == AUTO),
                    None => None,
                }
                .unwrap_or(levels.len().saturating_sub(1));
                let description = match &model.connection {
                    Some(connection) => {
                        i18n::team_plan(self.lang, &connection.vendor, &connection.name)
                    }
                    None => model.provider_name.clone(),
                };
                Item {
                    label: model.model.clone(),
                    description,
                    current: is_current,
                    value: Choice::Model(ModelSelection {
                        connection: model
                            .connection
                            .as_ref()
                            .map(|connection| Box::new(connection.id.clone())),
                        ..ModelSelection::new(model.provider.clone(), model.model.clone())
                    }),
                    levels,
                    level,
                    ..Item::default()
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
        self.show_list(list);
    }
}

/// 按名字找模型：与命令行的 `--model` 同一个函数（`<provider>/<model>` 或模型 ID，取第一个匹配；团队套餐加
/// `[<团队>]`，docs/specs/rust-cli/cli-args.md §1.3）。TUI 只在 `/login` 之后切到登录给出的模型时用。
pub fn resolve_model(models: &[CatalogModel], name: &str) -> Result<ModelSelection, LookupError> {
    model_name::lookup(models, name).map(model_name::selection)
}
