use super::subagent::Profile;
use serde_json::{Value, json};

pub fn parse(text: &str, source: &str) -> Option<Profile> {
    let (present, mut fields, body) = super::skills::frontmatter(text);
    if !present {
        return None;
    }
    // Agent 的列表允许 YAML 多行；Skill scalar parser 不读取嵌套字段，单独规范化列表。
    let mut list: Option<(String, Vec<String>)> = None;
    for line in text.lines().skip(1).take_while(|l| l.trim() != "---") {
        if let Some(value) = line.trim().strip_prefix("- ") {
            if let Some((_, values)) = &mut list {
                values.push(unquote(value));
            }
        } else if !line.starts_with(char::is_whitespace) {
            if let Some((key, values)) = list.take() {
                fields.insert(key, serde_json::to_string(&values).ok()?);
            }
            if let Some((key, value)) = line.split_once(':')
                && value.trim().is_empty()
            {
                list = Some((key.into(), vec![]));
            }
        }
    }
    if let Some((key, values)) = list {
        fields.insert(key, serde_json::to_string(&values).ok()?);
    }
    let name = fields.get("name")?.clone();
    let description = fields.get("description")?.clone();
    if name.is_empty() || description.is_empty() {
        return None;
    }
    let tools = fields.get("tools").map(|s| parse_tools(s));
    Some(Profile {
        name,
        description,
        source: source.into(),
        system_prompt: body,
        tools,
        disallowed_tools: fields
            .get("disallowedTools")
            .map(|s| parse_tools(s))
            .unwrap_or_default(),
        max_turns: fields
            .get("maxTurns")
            .and_then(|s| s.parse().ok())
            .filter(|n| *n > 0),
        model_selection: fields
            .get("model")
            .and_then(|model| selection(model, fields.get("thoughtLevel").map(String::as_str))),
        background: fields.get("background").is_some_and(|s| s == "true"),
        inject_agents_md: fields.get("injectAgentsMd").and_then(|s| s.parse().ok()),
        mcp_servers: fields.get("mcpServers").map(|s| parse_list(s)),
        skills: fields
            .get("skills")
            .map(|s| parse_list(s))
            .unwrap_or_default(),
        memory: fields.get("memory").cloned(),
    })
}
fn parse_tools(s: &str) -> Vec<String> {
    let values = if let Ok(values) = serde_json::from_str::<Vec<String>>(s) {
        values
    } else {
        let mut result = Vec::new();
        let mut current = String::new();
        let mut depth = 0usize;
        for c in s.trim_matches(['[', ']']).chars() {
            if c == '(' {
                depth += 1;
            } else if c == ')' {
                depth = depth.saturating_sub(1);
            }
            if depth == 0 && (c == ',' || c.is_whitespace()) {
                if !current.is_empty() {
                    result.push(std::mem::take(&mut current));
                }
            } else {
                current.push(c);
            }
        }
        if !current.is_empty() {
            result.push(current);
        }
        result
    };
    values
        .iter()
        .map(|s| unquote(s).split('(').next().unwrap().trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}
fn parse_list(s: &str) -> Vec<String> {
    if let Ok(list) = serde_json::from_str::<Vec<String>>(s) {
        return list;
    }
    s.trim_matches(['[', ']'])
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(unquote)
        .filter(|s| !s.is_empty())
        .collect()
}
fn unquote(s: &str) -> String {
    s.trim().trim_matches(['\'', '"']).to_owned()
}
fn decode(s: &str) -> String {
    let mut bytes = vec![];
    let mut it = s.as_bytes().iter().copied();
    while let Some(b) = it.next() {
        if b == b'%' {
            let Some(a) = it.next() else { return s.into() };
            let Some(b) = it.next() else { return s.into() };
            let (Some(a), Some(b)) = ((a as char).to_digit(16), (b as char).to_digit(16)) else {
                return s.into();
            };
            bytes.push((16 * a + b) as u8);
        } else {
            bytes.push(b);
        }
    }
    String::from_utf8(bytes).unwrap_or_else(|_| s.into())
}
fn selection(model: &str, thought: Option<&str>) -> Option<Value> {
    if ["inherit", "main", "sonnet", "opus", "haiku"].contains(&model) {
        return None;
    }
    let (provider, model) = if let Some(raw) = model.strip_prefix("custom:") {
        let (p, m) = raw.split_once(':')?;
        (decode(p), decode(m))
    } else {
        let (p, m) = model.split_once('/')?;
        (p.into(), m.into())
    };
    let (model, level) = model
        .split_once('$')
        .map_or((model.as_str(), None), |(m, l)| (m, Some(l)));
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    let mut value = json!({"providerId":provider,"modelId":model});
    if let Some(level) = thought.filter(|s| !s.is_empty()).or(level) {
        value["options"] = json!({"reasoningLevel":level});
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    #[test]
    fn tool_profile_matches_ts_names_without_permission_specifiers() {
        assert_eq!(
            super::parse_tools("Read Bash(git status, git diff) Grep"),
            vec!["Read", "Bash", "Grep"]
        );
        assert_eq!(
            super::parse_tools(r#"["Read", "Bash(git *)"]"#),
            vec!["Read", "Bash"]
        );
    }
}
