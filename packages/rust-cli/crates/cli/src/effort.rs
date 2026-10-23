//! `--effort <level>`（docs/specs/rust-cli/cli-args.md §1.4）：取值、警告与新会话的档位。
//!
//! 档位在解析时就定，不认识的值不报错：写一行警告、按缺省档位继续。会话的档位是 `ModelSelection.reasoning`，
//! `--effort` 是它的初值；会话里的 `/effort` 之后照旧换它。

use rust_cli_kernel_api::model::ModelSelection;

/// 认的档位，按从低到高（警告与帮助里的顺序）。
pub(crate) const LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// 别名：`med` 是 `medium`，`ultracode` 是 `xhigh`（没有单独的 ultracode 模式，cli-args.md D20）。
const ALIASES: [(&str, &str); 2] = [("med", "medium"), ("ultracode", "xhigh")];

/// 解析 `--effort` 的值：去掉首尾空白、不分大小写。不认识时返回写到 stderr 的警告（不带换行）。
pub(crate) fn parse(value: &str) -> Result<&'static str, String> {
    let wanted = value.trim().to_lowercase();
    let wanted = ALIASES
        .iter()
        .find(|(alias, _)| *alias == wanted)
        .map_or(wanted.as_str(), |(_, level)| level);
    LEVELS
        .iter()
        .find(|level| **level == wanted)
        .copied()
        .ok_or_else(|| {
            format!(
                "Warning: Unknown --effort value '{value}' \u{2014} ignoring it and using the default effort. Valid values: {}.",
                LEVELS.join(", ")
            )
        })
}

/// 把 `--effort` 的档位写进新会话的选择。`offered` 是模型目录里这个模型的档位（显式覆盖没有目录时为 None）：
///
/// - None：原样写（请求里的 effort 只由模型卡片决定，没有卡片的模型不发）；
/// - 有目录：同名的档位（不分大小写）；没有同名、档位是 `xhigh` / `max` 而模型有 `high` 时用 `high`；
///   仍没有时不改，按缺省档位（模型的档位不一定是这五档）。
pub(crate) fn apply(selection: &mut ModelSelection, level: &str, offered: Option<&[String]>) {
    let Some(offered) = offered else {
        selection.reasoning = Some(level.to_owned());
        return;
    };
    let named = |name: &str| {
        offered
            .iter()
            .find(|known| known.eq_ignore_ascii_case(name))
            .cloned()
    };
    let chosen = named(level).or_else(|| {
        matches!(level, "xhigh" | "max")
            .then(|| named("high"))
            .flatten()
    });
    if let Some(chosen) = chosen {
        selection.reasoning = Some(chosen);
    }
}

#[cfg(test)]
#[path = "effort_tests.rs"]
mod tests;
