//! Read-only policy tables exported from Node (`bash-readonly-policy-*.ts`).
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    sync::OnceLock,
};

/// Node `SafeFlagValue`.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub(crate) enum Kind {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "number")]
    Number,
    #[serde(rename = "string")]
    String,
    #[serde(rename = "optionalString")]
    OptionalString,
    #[serde(rename = "char")]
    Char,
    #[serde(rename = "{}")]
    Braces,
    #[serde(rename = "EOF")]
    Eof,
}

/// A dangerous-argument check; receives the words after the matched prefix.
pub(crate) type Callback = fn(&[String]) -> bool;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawPolicy {
    prefix: String,
    #[serde(default)]
    allow_any_args: bool,
    #[serde(default)]
    command_only: bool,
    #[serde(default)]
    allow_compact_numeric_count_flag: bool,
    #[serde(default = "yes")]
    respects_double_dash: bool,
    safe_flags: Option<HashMap<String, Kind>>,
    regex: Option<String>,
    callback: Option<String>,
}

fn yes() -> bool {
    true
}

/// Node `BashReadonlyCommandPolicy`.
pub(crate) struct Policy {
    pub words: Vec<String>,
    pub allow_any_args: bool,
    pub command_only: bool,
    pub allow_compact_numeric_count_flag: bool,
    pub respects_double_dash: bool,
    pub safe_flags: Option<HashMap<String, Kind>>,
    pub regex: Option<regex::Regex>,
    pub callback: Option<Callback>,
}

impl From<RawPolicy> for Policy {
    fn from(raw: RawPolicy) -> Self {
        Self {
            words: raw.prefix.split(' ').map(str::to_owned).collect(),
            allow_any_args: raw.allow_any_args,
            command_only: raw.command_only,
            allow_compact_numeric_count_flag: raw.allow_compact_numeric_count_flag,
            respects_double_dash: raw.respects_double_dash,
            safe_flags: raw.safe_flags,
            regex: raw.regex.as_deref().map(super::js::regex),
            callback: raw.callback.as_deref().map(|name| {
                super::callbacks::by_name(name)
                    .unwrap_or_else(|| panic!("no Rust callback for {name}"))
            }),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawTables {
    commands: Vec<RawPolicy>,
    git: Vec<RawPolicy>,
    multiword: Vec<RawPolicy>,
    allow_any_arg_commands: Vec<String>,
    allow_any_arg_command_prefixes: Vec<Vec<String>>,
    git_global_no_value_flags: Vec<String>,
    git_global_value_flags: Vec<String>,
    git_global_dangerous_flags: Vec<String>,
}

pub(crate) struct Tables {
    pub commands: HashMap<String, Policy>,
    /// Longest prefix first; equal lengths keep table order (JS stable sort).
    pub git: Vec<Policy>,
    pub multiword: Vec<Policy>,
    pub allow_any: HashSet<String>,
    pub allow_any_prefixes: Vec<Vec<String>>,
    pub git_no_value: HashSet<String>,
    pub git_value: HashSet<String>,
    pub git_dangerous: Vec<String>,
}

fn by_length(policies: Vec<RawPolicy>) -> Vec<Policy> {
    let mut policies: Vec<Policy> = policies.into_iter().map(Policy::from).collect();
    policies.sort_by_key(|p| std::cmp::Reverse(p.words.len()));
    policies
}

pub(crate) fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let raw: RawTables = serde_json::from_str(include_str!("../schema/readonly-policy.json"))
            .expect("read-only policy tables");
        Tables {
            commands: raw
                .commands
                .into_iter()
                .map(|p| (p.prefix.clone(), Policy::from(p)))
                .collect(),
            git: by_length(raw.git),
            multiword: by_length(raw.multiword),
            allow_any: raw.allow_any_arg_commands.into_iter().collect(),
            allow_any_prefixes: raw.allow_any_arg_command_prefixes,
            git_no_value: raw.git_global_no_value_flags.into_iter().collect(),
            git_value: raw.git_global_value_flags.into_iter().collect(),
            git_dangerous: raw.git_global_dangerous_flags,
        }
    })
}
