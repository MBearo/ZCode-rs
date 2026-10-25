//! system prompt 段落与上下文 reminder 的文本（docs/specs/rust-cli/context.md §6）。
//!
//! 每个函数对应 `packages/rust-cli/prompts/` 下的一个 `.txt` 文件：一部分由
//! `scripts/rust-cli/prompts/system.mjs` 生成并 `--check`，其余手工维护。
//! 模板变量写成 `{{name}}`，用 [`render`] 填入。

use rust_cli_base::include_lf;

use crate::trim;

macro_rules! texts {
    ($($name:ident => $path:literal,)*) => {
        $(
            #[doc = concat!("`prompts/", $path, ".txt`")]
            pub fn $name() -> &'static str {
                trim(include_lf!(concat!("../../../prompts/", $path, ".txt")))
            }
        )*

        #[cfg(test)]
        pub(crate) const ALL: &[(&str, fn() -> &'static str)] = &[$(($path, $name)),*];
    };
}

texts! {
    identity => "system/identity",
    lean_body => "system/lean_body",
    intro => "system/intro",
    intro_output_style => "system/intro_output_style",
    security => "system/security",
    reminders_system_turns => "system/reminders_system_turns",
    reminders_tags => "system/reminders_tags",
    communication_lean => "system/communication_lean",
    pronouns => "system/pronouns",
    action_caution => "system/action_caution",
    guidance => "system/guidance",
    guidance_shell => "system/guidance_shell",
    guidance_skills => "system/guidance_skills",
    memory_lean => "system/memory_lean",
    context_management => "system/context_management",
    act_dont_rederive => "system/act_dont_rederive",
    total_tokens => "context/total_tokens",
    environment => "context/environment",
    environment_cwd => "context/environment_cwd",
    environment_cwd_was => "context/environment_cwd_was",
    environment_git => "context/environment_git",
    environment_git_was => "context/environment_git_was",
    environment_additional => "context/environment_additional",
    environment_additional_added => "context/environment_additional_added",
    environment_additional_removed => "context/environment_additional_removed",
    environment_platform => "context/environment_platform",
    environment_shell => "context/environment_shell",
    environment_os => "context/environment_os",
    environment_untrusted => "context/environment_untrusted",
    environment_update => "context/environment_update",
    model => "context/model",
    model_unnamed => "context/model_unnamed",
    model_cutoff => "context/model_cutoff",
    deferred_tools => "context/deferred_tools",
    deferred_tools_removed => "context/deferred_tools_removed",
    deferred_tools_pending => "context/deferred_tools_pending",
    deferred_tools_removed_mcp => "context/deferred_tools_removed_mcp",
    deferred_tools_removed_mcp_many => "context/deferred_tools_removed_mcp_many",
    deferred_tools_readded_mcp => "context/deferred_tools_readded_mcp",
    deferred_tools_readded => "context/deferred_tools_readded",
    ultrathink_effort => "context/ultrathink_effort",
    skill_mention => "context/skill_mention",
    agents => "context/agents",
    agents_new => "context/agents_new",
    agents_removed => "context/agents_removed",
    ambient => "context/ambient",
    agents_concurrency => "context/agents_concurrency",
    skills => "context/skills",
    mcp_instructions => "context/mcp_instructions",
    mcp_instructions_removed => "context/mcp_instructions_removed",
    date => "context/date",
    date_changed => "context/date_changed",
    silent_turn => "context/silent_turn",
    attribution => "context/attribution",
    attribution_commit => "context/attribution_commit",
    attribution_pr => "context/attribution_pr",
    attribution_commit_value => "context/attribution_commit_value",
    attribution_pr_value => "context/attribution_pr_value",
    attribution_none => "context/attribution_none",
    instructions => "context/instructions",
    instructions_file => "context/instructions_file",
    instructions_project => "context/instructions_project",
    instructions_local => "context/instructions_local",
    instructions_automem => "context/instructions_automem",
    instructions_user => "context/instructions_user",
    instructions_managed => "context/instructions_managed",
    instructions_changed => "context/instructions_changed",
    instructions_reread => "context/instructions_reread",
    instructions_reread_only => "context/instructions_reread_only",
    instructions_removed => "context/instructions_removed",
    refresh_session_start => "context/refresh_session_start",
    refresh_compaction => "context/refresh_compaction",
    session_context => "context/session_context",
    session_context_section => "context/session_context_section",
    git_status => "context/git_status",
    git_status_branch => "context/git_status_branch",
    git_status_main => "context/git_status_main",
    git_status_user => "context/git_status_user",
    git_status_status => "context/git_status_status",
    git_status_clean => "context/git_status_clean",
    git_status_commits => "context/git_status_commits",
    git_status_truncated => "context/git_status_truncated",
    // 计划模式（context.md §13，工作线 L11c）。
    plan_mode_full => "context/plan_mode_full",
    plan_mode_file_new => "context/plan_mode_file_new",
    plan_mode_file_exists => "context/plan_mode_file_exists",
    plan_mode_sparse => "context/plan_mode_sparse",
    plan_mode_subagent => "context/plan_mode_subagent",
    plan_mode_subagent_file_new => "context/plan_mode_subagent_file_new",
    plan_mode_subagent_file_exists => "context/plan_mode_subagent_file_exists",
    plan_mode_reentry => "context/plan_mode_reentry",
    plan_mode_exit => "context/plan_mode_exit",
    plan_mode_exit_file => "context/plan_mode_exit_file",
    // bypass 模式的宣布（shell-permissions.md §4.9，修复线 F43）。
    auto_mode_bypass => "context/auto_mode_bypass",
    // 读过的文件在磁盘上被改动（tools-files.md §4.1，工作线 F70）。
    edited_text_file => "context/edited_text_file",
    edited_text_file_changes => "context/edited_text_file_changes",
    edited_text_file_hidden => "context/edited_text_file_hidden",
    // 远端机器：工具的 `machine` 参数与机器清单提醒（remote-exec.md §20.5，R3）。
    machine_param => "context/machine_param",
    machine_param_default => "context/machine_param_default",
    machine_param_agent => "context/machine_param_agent",
    machine_param_agent_default => "context/machine_param_agent_default",
    machines => "context/machines",
    machines_none => "context/machines_none",
    // 子代理的缺省机器在远端时的提醒（F98，remote-exec.md §25.2.1）。
    machines_default => "context/machines_default",
    machines_none_default => "context/machines_none_default",
    machine_line => "context/machine_line",
    machine_gone => "context/machine_gone",
    // 远端调用失败的说明：有没有执行、能不能重试、下一步做什么（remote-exec.md §23.6，F96）。
    remote_not_connected => "remote/not_connected",
    remote_ended_unsent => "remote/ended_unsent",
    remote_ended_read => "remote/ended_read",
    remote_ended_unknown => "remote/ended_unknown",
    remote_ended_command => "remote/ended_command",
    remote_restarted => "remote/restarted",
    remote_restarted_command => "remote/restarted_command",
    // 远端命令的输出文件在那台机器上（F98，remote-exec.md §25.2.3）。
    remote_output_file => "remote/output_file",
    // 任务工具的周期提醒（agents.md §7.4，工作线 F70）。
    task_reminder => "context/task_reminder",
    task_reminder_tasks => "context/task_reminder_tasks",
}

/// 填模板变量：`{{name}}` 换成 `vars` 里同名的值。只扫描模板本身一遍，值里出现的 `{{…}}`
/// 不会再被替换；模板里没有给值的变量原样保留（由各模板的测试保证不会发生）。
pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let key = &after[..end];
                match vars.iter().find(|(name, _)| *name == key) {
                    Some((_, value)) => out.push_str(value),
                    None => out.push_str(&rest[start..start + 2 + end + 2]),
                }
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// 模板里出现的变量名（测试用来核对调用方给全了变量）。
pub fn variables(template: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        names.push(&after[..end]);
        rest = &after[end + 2..];
    }
    names
}
