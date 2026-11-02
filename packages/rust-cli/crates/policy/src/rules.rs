//! 一次判决看到的规则与查找（docs/specs/rust-cli/shell-permissions.md §4.2）。规则串的语法（解析、写回、工具名的
//! 匹配）在 kernel-api 的 `permission::syntax`，与 hooks 的 `if` 条件共用（F125）。

use rust_cli_kernel_api::permission::syntax::{format_rule, tool_matches};
use rust_cli_kernel_api::permission::{
    PermissionRule, PermissionRules, RuleBehavior, RuleSource, SourcedRule,
};

/// 一次判决看到的规则：配置快照（含“总是允许”落库后的 LocalSettings 层），加上只对这一次判决生效的允许规则
/// （skill 的 `allowed-tools`）。
pub struct Rules<'a> {
    pub base: &'a PermissionRules,
    pub session: &'a [PermissionRule],
}

/// 找到的规则与它的来源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found<'a> {
    pub rule: &'a PermissionRule,
    pub source: RuleSource,
}

impl Found<'_> {
    pub fn text(&self) -> String {
        format_rule(self.rule)
    }
}

impl<'a> Rules<'a> {
    fn all(&self) -> impl Iterator<Item = (RuleBehavior, Found<'a>)> + '_ {
        let base = self.base.rules.iter().map(
            |SourcedRule {
                 source,
                 behavior,
                 rule,
             }| {
                (
                    *behavior,
                    Found {
                        rule,
                        source: *source,
                    },
                )
            },
        );
        let session = self.session.iter().map(|rule| {
            (
                RuleBehavior::Allow,
                Found {
                    rule,
                    source: RuleSource::Session,
                },
            )
        });
        base.chain(session)
    }

    /// 针对整个工具的规则（没有 content）：先查快照（与工具清单同一个查找，kernel-api `PermissionRules::whole_tool`），
    /// 再查这一次判决的允许规则。
    pub fn whole_tool(&self, tool: &str, behavior: RuleBehavior) -> Option<Found<'a>> {
        if let Some(sourced) = self.base.whole_tool(tool, behavior) {
            return Some(Found {
                rule: &sourced.rule,
                source: sourced.source,
            });
        }
        if behavior != RuleBehavior::Allow {
            return None;
        }
        self.session
            .iter()
            .find(|rule| rule.content.is_none() && tool_matches(&rule.tool, tool))
            .map(|rule| Found {
                rule,
                source: RuleSource::Session,
            })
    }

    /// 某个工具名下带 content 的规则。
    pub fn contents(&self, tool: &str, behavior: RuleBehavior) -> Vec<(&'a str, Found<'a>)> {
        self.all()
            .filter(|(kind, found)| *kind == behavior && found.rule.tool == tool)
            .filter_map(|(_, found)| {
                found
                    .rule
                    .content
                    .as_deref()
                    .map(|content| (content, found))
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
