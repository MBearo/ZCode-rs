//! `$` 提及技能的规则（docs/puffin-tui.md §4.2「提及技能」；tui-input.md §1.4）：开启面板的词、目录与排序、插入、发出去的
//! 写法与认识的提及在哪里。纯的；照 Puffin `shell/mentions.rs`（技能是旧界面自己的 [`MentionSkill`]，草稿是文字与字节光标）。
//! 面板的画法在 kit 的 `skill_panel`，状态与按键在 `composer_skills.rs`、`skill_view.rs`。
use std::collections::BTreeMap;

use rust_cli_kernel_api::user_command::UserCommand;
use rust_cli_tui_kit::app::widgets::fit::java_space;
use rust_cli_tui_kit::app::widgets::mention_wire;
use rust_cli_tui_kit::app::widgets::skill_panel::{Skill, SkillFrom};

/// 开始提及的符号：`$`，与中文输入法的 `¥` `￥`（kit `mention_wire::SIGNS`，Puffin 同一份）。
pub use rust_cli_tui_kit::app::widgets::mention_wire::SIGNS;

/// 技能从哪里来（Puffin host 的 `SkillScope`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum From {
    User,
    Plugin(String),
}

/// 目录里的一个技能：名字（不带 `$`）、说明、SKILL.md 的路径（内核的命令没有，为空）与来处。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionSkill {
    pub name: String,
    pub description: String,
    pub path: String,
    pub from: From,
}

impl MentionSkill {
    /// 在 kit 面板上的样子。
    pub fn kit(&self) -> Skill<'_> {
        Skill {
            name: &self.name,
            description: &self.description,
            from: match &self.from {
                From::Plugin(p) => SkillFrom::Plugin(p),
                From::User => SkillFrom::User,
            },
        }
    }
}

/// 用户能调用的技能（不隐藏的命令）：插件的名字是 `插件:名字`；内核的命令没有 SKILL.md 的路径与范围，非插件的按你的
/// （与 Puffin host 的 `skills_of` 同一条规则），再按 [`catalog`] 折叠、排序。
pub fn of_commands(commands: &[UserCommand]) -> Vec<MentionSkill> {
    catalog(
        commands
            .iter()
            .filter(|c| !c.hidden)
            .map(|c| MentionSkill {
                name: c.name.clone(),
                description: c.description.clone(),
                path: String::new(),
                from: match c.name.split_once(':') {
                    Some((plugin, _)) => From::Plugin(plugin.to_owned()),
                    None => From::User,
                },
            })
            .collect(),
    )
}

fn priority(from: &From) -> u8 {
    match from {
        From::Plugin(_) => 0,
        From::User => 1,
    }
}

/// 每个名字（不分大小写）一个技能：插件的先于你的（内核的命令没有项目的范围）；按名字排序（Puffin `catalog`）。
pub fn catalog(skills: Vec<MentionSkill>) -> Vec<MentionSkill> {
    let mut by: BTreeMap<String, MentionSkill> = BTreeMap::new();
    for k in skills {
        let key = k.name.trim().to_lowercase();
        match by.get(&key) {
            Some(had) if priority(&had.from) <= priority(&k.from) => {}
            _ => {
                by.insert(key, k);
            }
        }
    }
    let mut out: Vec<MentionSkill> = by.into_values().collect();
    out.sort_by_key(|k| k.name.to_lowercase());
    out
}

/// 光标之前的 `$` 词：符号在哪里（字节）与符号之后打的字。它在草稿开头或跟在空白之后；里面有第二个符号的词，或符号后紧跟
/// 数字的（`$5`）不是（Puffin `token`）。
pub fn token(text: &str, cursor: usize) -> Option<(usize, String)> {
    let before = &text[..cursor.min(text.len())];
    let s = before
        .char_indices()
        .rev()
        .find(|(_, c)| java_space(*c))
        .map_or(0, |(k, c)| k + c.len_utf8());
    let sign = before[s..].chars().next()?;
    if !SIGNS.contains(&sign) {
        return None;
    }
    let q = &before[s + sign.len_utf8()..];
    if q.chars().any(|c| SIGNS.contains(&c)) || q.chars().next().is_some_and(|c| c.is_ascii_digit())
    {
        None
    } else {
        Some((s, q.to_owned()))
    }
}

fn subsequence(q: &str, within: &str) -> bool {
    let mut qs = q.chars().peekable();
    for c in within.chars() {
        if qs.peek() == Some(&c) {
            qs.next();
        }
    }
    qs.peek().is_none()
}

/// `q` 找到的技能，好的在前（不分大小写）：名字以它开头；名字的一段，或各段的首字母，以它开头；名字含它；名字按次序含它的
/// 字母；说明含它。同一档里按目录的次序（Puffin `matches`）。
pub fn matches<'a>(skills: &'a [MentionSkill], q: &str) -> Vec<&'a MentionSkill> {
    let q = q.to_lowercase();
    if q.is_empty() {
        return skills.iter().collect();
    }
    let rank = |k: &MentionSkill| {
        let n = k.name.to_lowercase();
        let parts: Vec<&str> = n
            .split(['-', ':', '_', '.'])
            .filter(|p| !p.is_empty())
            .collect();
        let initials: String = parts.iter().filter_map(|p| p.chars().next()).collect();
        if n.starts_with(&q) {
            0
        } else if parts.iter().any(|p| p.starts_with(&q)) || initials.starts_with(&q) {
            1
        } else if n.contains(&q) {
            2
        } else if subsequence(&q, &n) {
            3
        } else if k.description.to_lowercase().contains(&q) {
            4
        } else {
            5
        }
    };
    let mut ranked: Vec<(usize, &MentionSkill)> = skills
        .iter()
        .map(|k| (rank(k), k))
        .filter(|(r, _)| *r < 5)
        .collect();
    ranked.sort_by_key(|(r, _)| *r);
    ranked.into_iter().map(|(_, k)| k).collect()
}

/// 从 `start` 起的 `$` 词（光标之后的也算）换成 `$名字`，后面没有空白时加一个空格：换掉的范围、换上的文字，与插入的提及
/// 在新草稿里的范围（淡入用）。光标停在那个空格之后（Puffin `insert`）。
pub fn insert(
    text: &str,
    cursor: usize,
    start: usize,
    name: &str,
) -> ((usize, usize), String, (usize, usize)) {
    let rest = &text[cursor.min(text.len())..];
    let end = cursor + rest.find(java_space).unwrap_or(rest.len());
    let mention = format!("${name}");
    let spaced = end < text.len();
    let stop = start + mention.len();
    let with = if spaced {
        mention
    } else {
        format!("{mention} ")
    };
    (
        (start, end),
        with,
        (start, if spaced { stop } else { stop + 1 }),
    )
}

/// 光秃的 `$` 提及（kit `mention_wire::bares`，Puffin 同一份）：符号的位置、末尾、名字。
pub fn bares(text: &str) -> Vec<(usize, usize, &str)> {
    mention_wire::bares(text, '$', &SIGNS)
}

/// 草稿里提到 `known` 认识的技能的地方，`[start, end)`（画成技能的墨色）。
pub fn ranges(text: &str, known: &dyn Fn(&str) -> bool) -> Vec<(usize, usize)> {
    bares(text)
        .into_iter()
        .filter(|(_, _, name)| known(name))
        .map(|(s, e, _)| (s, e))
        .collect()
}

/// 发出的文字（kit `mention_wire::to_wire`，Puffin 的 `to_wire` 调同一份）：目录里有 SKILL.md 路径的技能的提及写成桌面端的链接
/// `[$名字](路径)`；没有路径的（内核的命令都没有）与不认识的 `$词` 照打的发出，模型看到 `$名字` 自己调用 `Skill`。
pub fn to_wire(text: &str, skills: &[MentionSkill]) -> String {
    let by_name: BTreeMap<&str, &str> = skills
        .iter()
        .map(|k| (k.name.as_str(), k.path.as_str()))
        .collect();
    mention_wire::to_wire(text, &SIGNS, &|n| by_name.get(n).copied())
}

#[cfg(test)]
#[path = "skill_mention_tests.rs"]
mod tests;
