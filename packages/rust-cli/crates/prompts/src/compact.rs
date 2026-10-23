//! 压缩用的文本（`packages/rust-cli/prompts/compact/`，docs/specs/rust-cli/compact.md §3.2、§3.3）。
//!
//! 文字逐字固定，没有产品名需要替换。模板变量用
//! [`crate::texts::render`] 填入。

use rust_cli_base::include_lf;

use crate::trim;

macro_rules! texts {
    ($($name:ident => $path:literal,)*) => {
        $(
            #[doc = concat!("`prompts/compact/", $path, ".txt`")]
            pub fn $name() -> &'static str {
                trim(include_lf!(concat!("../../../prompts/compact/", $path, ".txt")))
            }
        )*
    };
}

texts! {
    no_tools => "no_tools",
    control => "control",
    additional => "additional",
    no_tools_reminder => "no_tools_reminder",
    reply_shape => "reply_shape",
    summary_header => "summary_header",
    summary_continue => "summary_continue",
    summary_kept_tail => "summary_kept_tail",
    summary_head_truncated => "summary_head_truncated",
    truncated_head => "truncated_head",
    tool_call => "tool_call",
    tool_result => "tool_result",
}
