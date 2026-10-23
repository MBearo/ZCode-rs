//! `--model` 与 TUI `/model <名称>` 的取值（docs/specs/rust-cli/cli-args.md §1.3，工作线 F49）：`<provider>/<model>`
//! 或模型 ID，取目录里第一个匹配的项；团队套餐共用一个 provider ID，用 `[<团队>]` 后缀（项目 ID 或团队显示名）
//! 指定团队项目。后缀放在最后，与上下文档位的 `[1m]` 后缀同一种习惯。
//!
//! 第一个匹配是团队项、而这个团队 provider 下同一个模型有多个可用的团队项目时报错，不默认取第一个：`-p` 没有
//! 界面，取错团队会用错团队的额度。

use std::fmt;

use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::provider::CatalogModel;

/// 找不到或说不清是哪一项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupError {
    /// 目录里没有。
    Missing,
    /// 第一个匹配是团队项，同一 provider 与模型有多个团队项目：`choices` 是每一个的写法。
    SeveralTeams { name: String, choices: Vec<String> },
    /// `[<团队>]` 的显示名对上同一 provider 与模型的多个团队项目：`choices` 是按项目 ID 的写法。
    SeveralProjects {
        team: String,
        model: String,
        choices: Vec<String>,
    },
}

impl fmt::Display for LookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("model is not configured"),
            Self::SeveralTeams { name, choices } => write!(
                f,
                "model {name} matches several team plans; pass one of: {}",
                choices.join(", ")
            ),
            Self::SeveralProjects {
                team,
                model,
                choices,
            } => write!(
                f,
                "team {team} matches several team projects of {model}; pass one of: {}",
                choices.join(", ")
            ),
        }
    }
}

fn same_model(a: &CatalogModel, b: &CatalogModel) -> bool {
    a.provider == b.provider && a.model == b.model
}

fn named(entry: &CatalogModel, name: &str) -> bool {
    format!("{}/{}", entry.provider, entry.model) == name || entry.model == name
}

/// 一项的写法：`<provider>/<model>`，团队项加 `[<团队显示名>]`（`by_project` 时用项目 ID）。
pub fn written(entry: &CatalogModel, by_project: bool) -> String {
    let base = format!("{}/{}", entry.provider, entry.model);
    match &entry.connection {
        Some(connection) if by_project => format!("{base}[{}]", connection.id.project_id),
        Some(connection) => format!("{base}[{}]", connection.name),
        None => base,
    }
}

/// 同一 provider 与模型的团队项的写法；显示名有重复时都用项目 ID。
fn choices<'a>(entries: impl Iterator<Item = &'a CatalogModel> + Clone) -> Vec<String> {
    let names: Vec<&str> = entries
        .clone()
        .filter_map(|entry| entry.connection.as_ref().map(|c| c.name.as_str()))
        .collect();
    let unique = names
        .iter()
        .enumerate()
        .all(|(i, name)| !names[..i].contains(name));
    entries.map(|entry| written(entry, !unique)).collect()
}

/// 按名称在目录里找一项（cli-args.md §1.3）。
pub fn lookup<'a>(models: &'a [CatalogModel], name: &str) -> Result<&'a CatalogModel, LookupError> {
    if let Some(first) = models.iter().find(|entry| named(entry, name)) {
        if first.connection.is_none() {
            return Ok(first);
        }
        let teams = models.iter().filter(|entry| same_model(entry, first));
        if teams.clone().count() > 1 {
            return Err(LookupError::SeveralTeams {
                name: name.to_owned(),
                choices: choices(teams),
            });
        }
        return Ok(first);
    }
    // 整个名称找不到才拆方括号（模型 ID 里本来就有 `[` 的模型照上面找到）。
    let Some((base, team)) = name
        .strip_suffix(']')
        .and_then(|rest| rest.rsplit_once('['))
    else {
        return Err(LookupError::Missing);
    };
    let team = rust_cli_base::js::string::trim(team);
    let teams = || {
        models
            .iter()
            .filter(move |entry| entry.connection.is_some() && named(entry, base))
    };
    if let Some(found) = teams().find(|entry| {
        entry
            .connection
            .as_ref()
            .is_some_and(|connection| connection.id.project_id == team)
    }) {
        return Ok(found);
    }
    let by_name = || {
        teams().filter(move |entry| {
            entry
                .connection
                .as_ref()
                .is_some_and(|connection| connection.name == team)
        })
    };
    let first = by_name().next().ok_or(LookupError::Missing)?;
    let same = by_name().filter(|entry| same_model(entry, first));
    if same.clone().count() > 1 {
        return Err(LookupError::SeveralProjects {
            team: team.to_owned(),
            model: format!("{}/{}", first.provider, first.model),
            choices: same.map(|entry| written(entry, true)).collect(),
        });
    }
    Ok(first)
}

/// 目录项 → 模型选择：档位取最后一个（平台层的缺省），团队项带上团队项目。
pub fn selection(entry: &CatalogModel) -> ModelSelection {
    ModelSelection {
        provider: entry.provider.clone(),
        model: entry.model.clone(),
        reasoning: entry.reasoning_levels.last().cloned(),
        connection: entry
            .connection
            .as_ref()
            .map(|connection| Box::new(connection.id.clone())),
    }
}

/// 选择与目录项是不是同一项（provider、模型与团队项目）。
pub fn is_entry(selection: &ModelSelection, entry: &CatalogModel) -> bool {
    selection.provider == entry.provider
        && selection.model == entry.model
        && selection.connection.as_deref() == entry.connection.as_ref().map(|c| &c.id)
}

#[cfg(test)]
#[path = "model_name_tests.rs"]
mod tests;
