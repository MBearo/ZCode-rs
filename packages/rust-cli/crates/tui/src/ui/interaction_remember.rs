//! 权限询问“不再询问”一行（docs/specs/rust-cli/shell-permissions.md §4.10、tui.md §5.2，工作线 F72）：Bash 有种子时
//! 这一行是可编辑的前缀，记住的规则由它得出；没有种子时列出全部建议规则。
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::permission::PermissionRule;

use crate::ui::i18n::Lang;

const BASH: &str = "Bash";

/// 询问框弹出时的种子：Bash 且有建议规则时才有。
pub(in crate::ui) fn seed(request: &InteractionRequest) -> Option<String> {
    let InteractionRequest::Permission {
        tool,
        input,
        suggestions,
        ..
    } = request
    else {
        return None;
    };
    if tool != BASH || suggestions.is_empty() {
        return None;
    }
    let command = input["command"].as_str()?;
    rust_cli_runtime::bash_rule_seed(command, suggestions)
}

/// 种子（可能被用户改过）记住的规则：`*` 是整个 Bash，空的只允许这一次。
pub(in crate::ui) fn rules(seed: &str) -> Vec<PermissionRule> {
    match seed.trim() {
        "" => Vec::new(),
        "*" => vec![PermissionRule {
            tool: BASH.to_owned(),
            content: None,
        }],
        prefix => vec![PermissionRule {
            tool: BASH.to_owned(),
            content: Some(prefix.to_owned()),
        }],
    }
}

/// `a`、`a and b`、`a, b, and c`（中文 `a`、`a 和 b`、`a、b 和 c`）。
fn listed(items: &[&str], lang: Lang) -> String {
    let (pair, last) = match lang {
        Lang::En => (" and ", ", and "),
        Lang::Zh => (" 和 ", " 和 "),
    };
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [a, b] => format!("{a}{pair}{b}"),
        [init @ .., tail] => {
            let separator = match lang {
                Lang::En => ", ",
                Lang::Zh => "、",
            };
            format!("{}{last}{tail}", init.join(separator))
        }
    }
}

/// Bash 没有种子时的一行：列出建议规则（去掉末尾的通配）与工作目录。
pub(in crate::ui) fn commands_label(
    suggestions: &[PermissionRule],
    workspace: &str,
    lang: Lang,
) -> String {
    let commands: Vec<&str> = suggestions
        .iter()
        .filter_map(|rule| rule.content.as_deref())
        .map(rust_cli_runtime::bash_rule_label)
        .collect();
    let commands = listed(&commands, lang);
    match lang {
        Lang::En => format!("Yes, and don't ask again for {commands} commands in {workspace}"),
        Lang::Zh => format!("是，以后在 {workspace} 里 {commands} 命令不再询问"),
    }
}

/// 有种子时这一行的前半句（后面接种子）。
pub(in crate::ui) fn seed_label(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "Yes, and don\u{2019}t ask again for: ",
        Lang::Zh => "是，以后不再询问：",
    }
}

#[cfg(test)]
#[path = "interaction_remember_tests.rs"]
mod tests;
