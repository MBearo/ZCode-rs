//! 一条你的消息放回输入框时的文字（`/rewind` 的点、fork、esc 撤回共用）。以前这里还有 ← 项目页与 `/resume` 整页的会话行，
//! 两页已删（tui-kit.md §4.3）：列表的行是 kit 的 `resume_list::rows`（sessions_list.rs）。
use rust_cli_kernel_api::message::{ContentBlock, Message};

/// 一条人类输入放回输入框、显示在预览里的样子（从 runner/lifecycle.rs 挪来，rewind 选择器与 `/resume` 页的预览共用）。
pub fn user_text(message: &Message) -> Option<String> {
    let Message::User(user) = message else {
        return None;
    };
    let texts: Vec<&str> = user
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let text = texts.join("\n");
    // 用户调用 skill 的输入：放回输入框的是用户输入的样子（`/<名字> <参数>`，ext.md §2.7）；bash 模式的输入
    // 放回成 `!命令`，输入框据此回到 bash 模式（tui-input.md §2.1）。
    if let Some(command) = rust_cli_kernel_api::user_input::display(&text) {
        return Some(format!("!{}", command.trim_start_matches("! ")));
    }
    Some(rust_cli_kernel_api::user_command::display(&text).unwrap_or(text))
}
