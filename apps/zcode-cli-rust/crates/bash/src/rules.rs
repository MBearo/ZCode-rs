//! Bash rule policy (Node `createBashPermissionRulePolicy`,
//! `bash-command-rule-evaluator.ts`).
use super::{js, suggest};
use zcode_cli_bash_parse::{Analysis, Invocation, analyze, is_permission_safe};
use zcode_cli_domain::permission::{Behavior, Rule, RulePolicy, Update, matches_content};

const MAX_SUGGESTED_RULES: usize = 5;

/// Rule subjects and suggestions of one Bash command.
#[derive(Debug)]
pub struct BashRules {
    safe: bool,
    exact_commands: Vec<String>,
    all_subjects: Vec<Vec<String>>,
    required_subjects: Vec<Vec<String>>,
    /// "Always allow" updates offered with a prompt.
    pub suggestions: Vec<Update>,
}

/// Node `isAnalysisSafeForPrefix`: no redirects and only static assignments.
fn safe_for_prefix(analysis: &Analysis) -> bool {
    is_permission_safe(analysis)
        && !analysis.has_redirects
        && !analysis.commands.is_empty()
        && analysis.commands.iter().all(|c| {
            !c.has_redirects && !c.has_dynamic_words && suggest::static_assignments(c).is_some()
        })
}

/// Node `buildInvocationRuleSubjects`: the raw words, plus the stable prefix
/// the suggestions save (so `pnpm run lint:*` matches `pnpm --dir x run lint`).
fn subjects(part: &Invocation) -> Vec<String> {
    let mut words = suggest::static_assignments(part).unwrap_or_default();
    words.extend(part.argv.iter().cloned());
    let raw = words.join(" ");
    match suggest::stable_prefix(part) {
        Some(prefix) if prefix != raw => vec![raw, prefix],
        _ => vec![raw],
    }
}

fn exact_update(command: &str) -> Vec<Update> {
    vec![Update {
        kind: "addRules".into(),
        behavior: Behavior::Allow,
        rules: vec![Rule {
            tool_name: "Bash".into(),
            rule_content: Some(command.into()),
        }],
    }]
}

fn suggestions(command: &str, safe: bool, required: &[&Invocation]) -> Vec<Update> {
    if command.is_empty() {
        return vec![];
    }
    if !safe || required.is_empty() || required.len() > MAX_SUGGESTED_RULES {
        return exact_update(command);
    }
    let mut rules: Vec<Rule> = vec![];
    for part in required {
        let Some(prefix) = suggest::stable_prefix(part) else {
            return exact_update(command);
        };
        let content = format!("{prefix}:*");
        if rules
            .iter()
            .all(|r| r.rule_content.as_deref() != Some(&content))
        {
            rules.push(Rule {
                tool_name: "Bash".into(),
                rule_content: Some(content),
            });
        }
    }
    vec![Update {
        kind: "addRules".into(),
        behavior: Behavior::Allow,
        rules,
    }]
}

impl BashRules {
    /// `git_unsafe` is the git runtime context verdict for the session's
    /// working directory (only consulted for git invocations).
    pub fn new(command: &str, git_unsafe: bool) -> Self {
        let trimmed = js::trim(command);
        let exact_commands = if command == trimmed {
            vec![trimmed.to_owned()]
        } else {
            vec![command.to_owned(), trimmed.to_owned()]
        };
        let analysis = analyze(command);
        let safe = safe_for_prefix(&analysis);
        let (all, required): (Vec<&Invocation>, Vec<&Invocation>) = if safe {
            let required = analysis
                .commands
                .iter()
                .filter(|c| !super::is_read_only(&c.command_text, git_unsafe))
                .collect();
            (analysis.commands.iter().collect(), required)
        } else {
            (vec![], vec![])
        };
        Self {
            safe,
            suggestions: suggestions(trimmed, safe, &required),
            all_subjects: all.iter().map(|c| subjects(c)).collect(),
            required_subjects: required.iter().map(|c| subjects(c)).collect(),
            exact_commands,
        }
    }
}

impl RulePolicy for BashRules {
    /// Node `evaluateBashRules`; `rules` are already the Bash rules of one behavior.
    fn evaluate(&self, behavior: Behavior, rules: &[&Rule]) -> bool {
        let content = |r: &Rule| r.rule_content.clone().unwrap_or_default();
        if rules.iter().any(|r| content(r).is_empty()) {
            return true;
        }
        if self.exact_commands.iter().any(|c| !c.is_empty())
            && rules
                .iter()
                .any(|r| self.exact_commands.contains(&content(r)))
        {
            return true;
        }
        if !self.safe {
            return false;
        }
        let groups = if behavior == Behavior::Allow {
            &self.required_subjects
        } else {
            &self.all_subjects
        };
        let covered = |subjects: &Vec<String>| {
            subjects.iter().any(|s| {
                rules
                    .iter()
                    .any(|r| matches_content(s, r.rule_content.as_deref().unwrap_or("")))
            })
        };
        if groups.is_empty() {
            return false;
        }
        if behavior == Behavior::Allow {
            groups.iter().all(covered)
        } else {
            groups.iter().any(covered)
        }
    }
}
