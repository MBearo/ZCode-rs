//! Permission rules: storage shape (Node `PermissionRuleset`), update merge
//! (`permission-rules.ts`) and matching (`service.ts` `matchesProjectRules`,
//! `rule-matching.ts`).
use super::{Context, Resolved, RulePolicy};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Behavior {
    Allow,
    Ask,
    Deny,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_content: Option<String>,
}

/// Node `PermissionUpdate` (`addRules` is the only type that has an effect).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Update {
    #[serde(rename = "type")]
    pub kind: String,
    pub behavior: Behavior,
    pub rules: Vec<Rule>,
}

/// Node `PermissionRuleset`; unknown keys (e.g. `mode`) survive a round trip.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Ruleset {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<Rule>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deny: Option<Vec<Rule>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<Vec<Rule>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Ruleset {
    pub fn rules(&self, behavior: Behavior) -> &[Rule] {
        match behavior {
            Behavior::Allow => self.allow.as_deref(),
            Behavior::Ask => self.ask.as_deref(),
            Behavior::Deny => self.deny.as_deref(),
        }
        .unwrap_or_default()
    }

    fn rules_mut(&mut self, behavior: Behavior) -> &mut Vec<Rule> {
        match behavior {
            Behavior::Allow => self.allow.get_or_insert_with(Vec::new),
            Behavior::Ask => self.ask.get_or_insert_with(Vec::new),
            Behavior::Deny => self.deny.get_or_insert_with(Vec::new),
        }
    }
}

/// Node `applyPermissionUpdates`: sets `version: 1`, appends rules and keeps the
/// first occurrence of each `toolName` + `ruleContent` pair.
pub fn apply_updates(ruleset: &Ruleset, updates: &[Update]) -> Ruleset {
    let mut next = ruleset.clone();
    next.extra.insert("version".into(), 1.into());
    for update in updates.iter().filter(|u| u.kind == "addRules") {
        let rules = next.rules_mut(update.behavior);
        for rule in &update.rules {
            if !rules.iter().any(|r| {
                r.tool_name == rule.tool_name
                    && r.rule_content.as_deref().unwrap_or("")
                        == rule.rule_content.as_deref().unwrap_or("")
            }) {
                rules.push(rule.clone());
            }
        }
    }
    next
}

const OFFICIAL_CUA_RULE_TOOL: &str = "zcode:permission-capability:official_cua";

fn in_scope(rule: &Rule, tool: &str, cap: &Resolved) -> bool {
    if rule.tool_name == OFFICIAL_CUA_RULE_TOOL {
        return cap.capability_group.as_deref() == Some("official_cua");
    }
    rule.tool_name == tool || (tool == "Write" && rule.tool_name == "Edit")
}

/// Node `matchesProjectRules`.
pub(super) fn matches(
    ruleset: Option<&Ruleset>,
    behavior: Behavior,
    ctx: &Context<'_>,
    cap: &Resolved,
    policy: Option<&dyn RulePolicy>,
) -> bool {
    let Some(ruleset) = ruleset else {
        return false;
    };
    let rules: Vec<&Rule> = ruleset
        .rules(behavior)
        .iter()
        .filter(|rule| in_scope(rule, ctx.tool, cap))
        .collect();
    if rules.is_empty() {
        return false;
    }
    if let Some(policy) = policy {
        return policy.evaluate(behavior, &rules);
    }
    let subjects = subjects(ctx.input, ctx.tool);
    rules.iter().any(|rule| match &rule.rule_content {
        None => true,
        Some(content) if content.is_empty() => true,
        Some(content) => subjects.iter().any(|s| matches_content(s, content)),
    })
}

/// Node `ruleSubjects`.
fn subjects(input: &Value, tool: &str) -> Vec<String> {
    if let Some(input) = input.as_str() {
        return vec![input.into()];
    }
    if !input.is_object() {
        return vec![];
    }
    if tool == "WebFetch"
        && let Some(url) = input["url"].as_str()
    {
        return domain_subject(url).into_iter().collect();
    }
    [
        "command",
        "url",
        "file_path",
        "path",
        "pattern",
        "patch_text",
    ]
    .iter()
    .find_map(|key| input[*key].as_str().map(|s| vec![s.to_owned()]))
    .unwrap_or_default()
}

/// Node `domainRuleSubject`.
fn domain_subject(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    let host = parsed.host_str()?.to_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host);
    (!host.is_empty()).then(|| format!("domain:{host}"))
}

/// Node `matchesRuleContent` (`:*` prefix, `*` wildcard, else equality).
pub fn matches_content(subject: &str, content: &str) -> bool {
    if let Some(prefix) = content.strip_suffix(":*") {
        return subject == prefix
            || subject
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with([' ', '\t']));
    }
    if content.contains('*') {
        return wildcard(subject, content);
    }
    subject == content
}

/// Node `wildcardToRegExp(pattern).test(subject)`: an anchored match where `*`
/// is `.*` — JS `.` does not cross line terminators.
fn wildcard(subject: &str, pattern: &str) -> bool {
    let terminator = |c: char| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}');
    let parts: Vec<&str> = pattern.split('*').collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    let Some(mut rest) = subject.strip_prefix(first) else {
        return false;
    };
    for part in &parts[1..parts.len() - 1] {
        let Some(at) = rest.find(part) else {
            return false;
        };
        if rest[..at].contains(terminator) {
            return false;
        }
        rest = &rest[at + part.len()..];
    }
    rest.len() >= last.len()
        && rest.ends_with(last)
        && !rest[..rest.len() - last.len()].contains(terminator)
}

/// Lexical `path.resolve(base, path)`.
fn resolve(base: &str, path: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for component in Path::new(base).join(path).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Node `isPreapprovedWorkflowDraftWrite`: Edit/Write strictly inside
/// `<workingDirectory>/.zcode/workflow-drafts/`, decided on strings only.
pub(super) fn workflow_draft_write(ctx: &Context<'_>) -> bool {
    let (Some(cwd), Some(file)) = (ctx.working_directory, ctx.input["file_path"].as_str()) else {
        return false;
    };
    if !matches!(ctx.tool, "Edit" | "Write") || cwd.is_empty() || file.is_empty() {
        return false;
    }
    let drafts = resolve(cwd, ".zcode/workflow-drafts");
    let target = resolve(cwd, file);
    target
        .strip_prefix(&drafts)
        .is_ok_and(|relative| !relative.as_os_str().is_empty() && !relative.starts_with(".."))
}
