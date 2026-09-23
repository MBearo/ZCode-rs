use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCatalog {
    pub enabled: bool,
    pub include_instructions: bool,
    pub metadata_budget: usize,
    pub skills: Vec<Skill>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: String,
    pub scope: String,
    pub plugin_name: Option<String>,
    pub when_to_use: Option<String>,
    pub plugin_root: Option<String>,
}
impl Skill {
    pub fn qualified_name(&self) -> String {
        self.plugin_name
            .as_ref()
            .map_or_else(|| self.name.clone(), |p| format!("{p}:{}", self.name))
    }
    pub fn entry(&self) -> Value {
        let mut value = json!({"id":format!("glm:{}:{}",self.scope,self.path),"name":self.name,"description":self.description,"path":self.path,"scope":self.scope,"enabled":true});
        if let Some(plugin) = &self.plugin_name {
            value["pluginName"] = plugin.clone().into();
        }
        value
    }
}
impl SkillCatalog {
    pub fn response(&self, authority: &str) -> Value {
        json!({"authority":authority,"skills":self.skills.iter().map(Skill::entry).collect::<Vec<_>>()})
    }
    pub fn reminder(&self) -> Option<Value> {
        if !self.include_instructions || self.skills.is_empty() {
            return None;
        }
        let mut skills = self.skills.iter().collect::<Vec<_>>();
        skills.sort_by_key(|s| s.qualified_name());
        let header = "The following skills are available for use with the Skill tool:\n\n";
        let lines = |descriptions: bool| {
            skills
                .iter()
                .map(|s| {
                    let alias = s
                        .plugin_name
                        .as_ref()
                        .map(|_| format!(" (also loadable as {})", s.name))
                        .unwrap_or_default();
                    let description = s.when_to_use.as_ref().map_or_else(
                        || s.description.clone(),
                        |when| format!("{} - {when}", s.description),
                    );
                    let description = if description.chars().count() > 250 {
                        format!("{}...", description.chars().take(249).collect::<String>())
                    } else {
                        description
                    };
                    format!(
                        "- {}{}{alias} (file: {})",
                        s.qualified_name(),
                        if descriptions {
                            format!(": {description}")
                        } else {
                            String::new()
                        },
                        s.path
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut content = format!("{header}{}", lines(true));
        if content.encode_utf16().count() > self.metadata_budget {
            content = format!("{header}{}", lines(false));
        }
        Some(
            json!({"role":"user","content":format!("<system-reminder>\n{content}\n</system-reminder>")}),
        )
    }
}

/// 与 TS flat YAML 保持一致：只解析顶层 scalar，嵌套 metadata 不参与名称解析。
pub fn frontmatter(content: &str) -> (bool, BTreeMap<String, String>, String) {
    let content = content.trim_start_matches('\u{feff}');
    let lines = content.lines().collect::<Vec<_>>();
    let Some(end) = lines
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, l)| l.trim() == "---")
        .map(|(i, _)| i)
        .filter(|_| lines.first().is_some_and(|l| l.trim() == "---"))
    else {
        return (false, BTreeMap::new(), content.trim().into());
    };
    let mut values = BTreeMap::new();
    let mut i = 1;
    while i < end {
        let line = lines[i];
        i += 1;
        if line.starts_with(char::is_whitespace) || line.trim_start().starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let mut value = value.trim().to_owned();
        if [">", ">-", ">+", "|", "|-", "|+"].contains(&value.as_str()) {
            let folded = value.starts_with('>');
            let mut block = vec![];
            while i < end
                && (lines[i].trim().is_empty() || lines[i].starts_with(char::is_whitespace))
            {
                block.push(lines[i]);
                i += 1;
            }
            let indent = block
                .iter()
                .filter(|l| !l.trim().is_empty())
                .map(|l| l.len() - l.trim_start().len())
                .min()
                .unwrap_or(0);
            value = if folded {
                block
                    .split(|l| l.trim().is_empty())
                    .filter(|p| !p.is_empty())
                    .map(|p| p.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" "))
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                block
                    .iter()
                    .map(|l| l.get(indent..).unwrap_or(""))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
        } else if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            value = value[1..value.len() - 1].into();
        }
        if !value.trim().is_empty() {
            values.insert(key.trim().into(), value.trim().into());
        }
    }
    (true, values, lines[end + 1..].join("\n").trim().into())
}
