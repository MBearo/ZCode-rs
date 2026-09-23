use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::OnceLock;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptSnapshot {
    pub cwd: String,
    pub platform: String,
    pub shell: String,
    pub os_version: String,
    pub current_date: String,
    pub git: Option<GitSnapshot>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitSnapshot {
    pub branch: String,
    pub main_branch: String,
    pub user: String,
    pub status: String,
    pub recent_commits: String,
}
pub struct InstructionSource {
    pub path: String,
    pub user: bool,
    pub content: String,
    pub truncated: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Templates {
    user_steer: String,
    cli: String,
    identity: String,
    desktop: String,
    behavior: String,
    context_management: String,
}
fn templates() -> &'static Templates {
    static TEMPLATES: OnceLock<Templates> = OnceLock::new();
    TEMPLATES.get_or_init(|| {
        serde_json::from_str(include_str!("prompt_templates.json"))
            .expect("validated prompt assets")
    })
}
pub fn user_steer(text: &str) -> String {
    templates()
        .user_steer
        .replacen("{zcode_input_text}", text, 1)
}
pub fn prefix(
    snapshot: &PromptSnapshot,
    sources: &[InstructionSource],
    model: Option<(&str, &str)>,
    desktop: bool,
) -> Vec<Value> {
    let templates = templates();
    let stable = if desktop {
        format!("{}\n\n{}", templates.identity, templates.desktop)
    } else {
        templates.identity.clone()
    };
    let mut env = format!(
        "# Environment\nYou have been invoked in the following environment:\n- Primary working directory: {}\n- Is a git repository: {}\n- Platform: {}\n- Shell: {}\n- OS Version: {}",
        snapshot.cwd,
        if snapshot.git.is_some() { "yes" } else { "no" },
        snapshot.platform,
        snapshot.shell,
        snapshot.os_version,
    );
    if let Some((provider, model)) = model {
        env.push_str(&format!(
            "\n- You are powered by the model named {provider}/{model}."
        ));
    }
    let mut dynamic = format!(
        "\n\n{}\n\n{env}\n\n{}",
        templates.behavior, templates.context_management
    );
    if let Some(git) = &snapshot.git {
        dynamic.push_str("\n\ngitStatus: This is the git status at the start of the conversation. Note that this status is a snapshot in time, and will not update during the conversation.");
        for (label, value) in [
            ("Current branch", &git.branch),
            (
                "Main branch (you will usually use this for PRs)",
                &git.main_branch,
            ),
            ("Git user", &git.user),
        ] {
            if !value.is_empty() {
                dynamic.push_str(&format!("\n\n{label}: {value}"));
            }
        }
        dynamic.push_str(&format!(
            "\n\nStatus:\n{}\n\nRecent commits:\n{}",
            if git.status.is_empty() {
                "(clean)"
            } else {
                &git.status
            },
            git.recent_commits
        ));
    }
    let mut messages = [&templates.cli, &stable, &dynamic].map(|content| {
        json!({"role":"system","content":content,"_zcode_cache_control":{"type":"ephemeral"}})
    }).to_vec();
    let mut sections = vec![];
    for source in sources {
        let body = if source.truncated {
            format!("{}\n\n[File truncated: AGENTS.md]", source.content)
        } else {
            source.content.clone()
        };
        let body = body.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
        if !body.is_empty() {
            sections.push(format!(
                "Contents of {} ({}):\n\n{body}",
                source.path,
                if source.user {
                    "user default instructions"
                } else {
                    "workspace instructions"
                }
            ));
        }
    }
    let mut context = vec![];
    if !sections.is_empty() {
        context.push(format!("# agentsMd\nCodebase and user instructions are shown below. Be sure to adhere to these instructions. IMPORTANT: These instructions OVERRIDE any default behavior and you MUST follow them exactly as written.\n\n{}", sections.join("\n\n")));
    }
    if !snapshot.current_date.is_empty() {
        context.push(format!(
            "# currentDate\nToday's date is {}.",
            snapshot.current_date
        ));
    }
    if !context.is_empty() {
        let body = format!(
            "As you answer the user's questions, you can use the following context:\n{}\n\n      IMPORTANT: this context may or may not be relevant to your tasks. You should not respond to this context unless it is highly relevant to your task.",
            context.join("\n\n")
        );
        static NESTED: OnceLock<regex::Regex> = OnceLock::new();
        let escaped = NESTED
            .get_or_init(|| regex::Regex::new(r"(?i)</?system-reminder\b").unwrap())
            .replace_all(&body, |caps: &regex::Captures<'_>| {
                format!("&lt;{}", &caps[0][1..])
            });
        messages.push(json!({"role":"user","content":format!("<system-reminder>\n{escaped}\n</system-reminder>\n")}));
    }
    messages
}
