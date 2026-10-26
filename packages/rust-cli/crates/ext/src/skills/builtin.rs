//! 内置 skill：`/loop`（bundled skill，docs/specs/rust-cli/tools-more.md §17.1）与内置提示词命令 `/init`、
//! `/security-review`（修复线 L-A，ext.md §2.9）。
//!
//! 正文是生成的提示，没有 `Base directory for this skill:` 前缀，不做参数替换与变量。`/loop` 参数为空或只有间隔时先找
//! loop.md；`/security-review` 在 git 仓库里时正文的 `!` 命令照 skill 的规则执行（只放行它自己的 allowed-tools）。

use std::sync::{Arc, OnceLock};

use regex::Regex;
use rust_cli_kernel_api::env::{EnvPath, ReadFile};
use rust_cli_kernel_api::tool::ToolUseContext;
use rust_cli_prompts::loops::{self, LoopFile};
use rust_cli_prompts::texts::render;

use super::{Skill, SkillCatalog, SkillShell, SkillSource};

/// 内置 skill 的名字。
pub(crate) const LOOP: &str = "loop";
/// 内置提示词命令（对照实现里内置的 prompt 命令，排在 bundled skill 之后）。
pub(crate) const INIT: &str = "init";
pub(crate) const SECURITY_REVIEW: &str = "security-review";
const INIT_DESCRIPTION: &str = "Initialize a new AGENTS.md file with codebase documentation";
const SECURITY_REVIEW_DESCRIPTION: &str =
    "Complete a security review of the pending changes on the current branch";
/// `/security-review` 的 `!` 命令与它要的工具（对照实现 frontmatter 的 `allowed-tools`，`Task` 是 Agent 的旧名）。
const SECURITY_REVIEW_TOOLS: [&str; 15] = [
    "Bash(git diff *)",
    "PowerShell(git diff *)",
    "Bash(git status *)",
    "PowerShell(git status *)",
    "Bash(git log *)",
    "PowerShell(git log *)",
    "Bash(git show *)",
    "PowerShell(git show *)",
    "Bash(git remote show *)",
    "PowerShell(git remote show *)",
    "Read",
    "Glob",
    "Grep",
    "LS",
    "Task",
];
const DESCRIPTION: &str = "Run a prompt or slash command on a recurring interval (e.g. /loop 5m /foo). Omit the interval to let the model self-pace.";
const WHEN_TO_USE: &str = "When the user wants to set up a recurring task, poll for status, or run something repeatedly on an interval (e.g. \"check the deploy every 5 minutes\", \"keep running /babysit-prs\"). Do NOT invoke for one-off tasks.";
/// 没有给间隔时的缺省。
const DEFAULT_INTERVAL: &str = "10m";

fn builtin(name: &str, description: &str) -> Skill {
    Skill {
        name: name.to_owned(),
        display: None,
        description: description.to_owned(),
        when_to_use: None,
        argument_hint: None,
        arg_names: Vec::new(),
        allowed_tools: Vec::new(),
        disallowed_tools: Vec::new(),
        disable_model_invocation: false,
        user_invocable: true,
        paths: None,
        shell: SkillShell::Bash,
        has_hooks: false,
        model: None,
        effort: None,
        fork: false,
        agent: None,
        dir: String::new(),
        body: String::new(),
        source: SkillSource::Builtin,
    }
}

/// 内置 skill 的清单，排在文件与插件 skill 之后：`loop`，再是内置提示词命令（对照实现的清单里它们排在 bundled skill 之后）。
pub(crate) fn skills() -> Vec<Arc<Skill>> {
    let looping = Skill {
        when_to_use: Some(WHEN_TO_USE.to_owned()),
        argument_hint: Some("[interval] [prompt]".to_owned()),
        ..builtin(LOOP, DESCRIPTION)
    };
    let review = Skill {
        allowed_tools: SECURITY_REVIEW_TOOLS
            .iter()
            .map(|tool| (*tool).to_owned())
            .collect(),
        ..builtin(SECURITY_REVIEW, SECURITY_REVIEW_DESCRIPTION)
    };
    vec![
        Arc::new(looping),
        Arc::new(builtin(INIT, INIT_DESCRIPTION)),
        Arc::new(review),
    ]
}

/// 内置提示词命令的正文（修复线 L-A）；不是它们时为 None。`/security-review` 要知道工作目录是不是 git 仓库：
/// 不是时给对照实现的说明；是时正文里的 `!` 命令交给 `expand`（skill 的展开，放行它的 allowed-tools）。
pub(crate) async fn prompt_command(
    catalog: &SkillCatalog,
    skill: &Skill,
    context: &ToolUseContext,
) -> Option<Result<String, String>> {
    match skill.name.as_str() {
        INIT => Some(Ok(rust_cli_prompts::commands::init().to_owned())),
        SECURITY_REVIEW => {
            let workspace = &context.workspace;
            let git = catalog
                .info
                .facts(workspace)
                .await
                .is_ok_and(|facts| facts.is_git_repo);
            if git {
                return None;
            }
            Some(Ok(rust_cli_prompts::commands::security_review_not_git(
                workspace.root.as_str(),
            )))
        }
        _ => None,
    }
}

/// 整段参数是一个 `<N><s|m|h|d>`。
fn bare_interval() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^\d+[smhd]$").unwrap_or_else(|_| unreachable!()))
}

/// 整段参数是 `every <N> <单位>`。
fn every_interval() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?i)^every\s+(\d+)\s*(s|sec|secs|second|seconds|m|min|mins|minute|minutes|h|hr|hrs|hour|hours|d|day|days)\s*$")
            .unwrap_or_else(|_| unreachable!())
    })
}

/// 只有间隔时的间隔（单位按首字母归一）；不是只有间隔时为 None。
fn interval_only(input: &str) -> Option<String> {
    if bare_interval().is_match(input) {
        return Some(input.to_owned());
    }
    let captures = every_interval().captures(input)?;
    let unit = captures[2].to_lowercase();
    let suffix = ["s", "h", "d"]
        .into_iter()
        .find(|prefix| unit.starts_with(prefix))
        .unwrap_or("m");
    Some(format!("{}{suffix}", &captures[1]))
}

async fn read_loop_file(catalog: &SkillCatalog, context: &ToolUseContext) -> Option<LoopFile> {
    let workspace = &context.workspace;
    // home 与路径拼法都取执行环境的路径事实（F58：与规则文件、子代理、skill 同一个 `PathStyle::join`）。
    let facts = catalog.info.path_facts(workspace).await.ok()?;
    let paths = loops::loop_file_paths(
        workspace.root.as_str(),
        facts.home.as_ref().map(EnvPath::as_str),
        |base, parts| facts.style.join(base, parts),
    );
    for path in paths {
        let request = ReadFile {
            path: EnvPath::new(path.clone()),
        };
        if let Ok(content) = catalog.files.read_file(workspace, request).await
            && let Some(file) = loops::loop_file(&path, &String::from_utf8_lossy(&content.bytes))
        {
            return Some(file);
        }
    }
    None
}

/// Monitor 的超时：单次的 `-p` 是 10 分钟，其余 30 分钟（毫秒、分钟）。
fn monitor(print: bool) -> (&'static str, &'static str) {
    if print {
        ("600000", "10")
    } else {
        ("1800000", "30")
    }
}

/// 正文的四种：自主默认或 loop.md，动态节奏或固定间隔。
fn defaults(file: Option<&LoopFile>, dynamic: bool, interval: &str, print: bool) -> String {
    let (timeout, minutes) = monitor(print);
    match (file, dynamic) {
        (Some(file), true) => render(
            loops::file_dynamic(),
            &[
                ("path", &file.path),
                ("content", &file.content),
                ("monitor_timeout_ms", timeout),
                ("monitor_minutes", minutes),
            ],
        ),
        (Some(file), false) => render(
            loops::file_fixed(),
            &[
                ("path", &file.path),
                ("interval", interval),
                ("content", &file.content),
            ],
        ),
        (None, true) => render(
            loops::autonomous_dynamic(),
            &[
                ("preamble", loops::preamble()),
                ("monitor_timeout_ms", timeout),
                ("monitor_minutes", minutes),
            ],
        ),
        (None, false) => render(
            loops::autonomous_fixed(),
            &[("interval", interval), ("preamble", loops::preamble())],
        ),
    }
}

/// `/loop` 的正文。
pub(crate) async fn prompt(catalog: &SkillCatalog, context: &ToolUseContext, args: &str) -> String {
    let input = args.trim();
    let interval = interval_only(input);
    let print = catalog.print_session();
    if input.is_empty() || interval.is_some() {
        let file = read_loop_file(catalog, context).await;
        let interval = interval.unwrap_or_else(|| DEFAULT_INTERVAL.to_owned());
        return defaults(file.as_ref(), input.is_empty(), &interval, print);
    }
    let (timeout, minutes) = monitor(print);
    render(
        loops::prompt(),
        &[
            ("input", input),
            ("monitor_timeout_ms", timeout),
            ("monitor_minutes", minutes),
        ],
    )
}

#[cfg(test)]
#[path = "builtin_tests.rs"]
mod tests;
