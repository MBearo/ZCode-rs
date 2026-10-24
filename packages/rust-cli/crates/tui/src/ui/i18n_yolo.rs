//! 切到完全访问的确认框的文字（docs/specs/rust-cli/tui.md §7.8，工作线 F87）：App 的原文，键是
//! `packages/ui/src/i18n/locales/en-US.ts` / `zh-CN.ts` 的 `mode.yoloConfirmation.*` 与 `common.cancel`。按键提示的写法
//! 同提问视图。
use super::i18n::Lang;

fn pick(lang: Lang, en: &'static str, zh: &'static str) -> &'static str {
    match lang {
        Lang::En => en,
        Lang::Zh => zh,
    }
}

/// `mode.yoloConfirmation.title`。
pub fn title(lang: Lang) -> &'static str {
    pick(lang, "Enable full access?", "要启用完全访问吗？")
}

/// `mode.yoloConfirmation.description`。
pub fn description(lang: Lang) -> &'static str {
    pick(
        lang,
        "Tasks can perform the following actions without asking for your approval each time.",
        "启用后，任务可以直接执行以下操作，无需逐次请求你确认。",
    )
}

/// 三项：`mode.yoloConfirmation.{files,commands,network}` 与各自的 `.description`，照 App 的顺序。
pub fn items(lang: Lang) -> [(&'static str, &'static str); 3] {
    [
        (
            pick(lang, "Files and folders", "文件和文件夹"),
            pick(
                lang,
                "Read, create, change, and delete accessible files.",
                "读取、创建、修改和删除可访问的文件。",
            ),
        ),
        (
            pick(lang, "Terminal commands", "终端命令"),
            pick(
                lang,
                "Run commands, install software, or change system settings.",
                "运行命令、安装软件或修改系统配置。",
            ),
        ),
        (
            pick(lang, "Network and apps", "网络和应用"),
            pick(
                lang,
                "Access the network, send data, and use connected apps.",
                "访问网络、发送数据，以及调用已连接的应用。",
            ),
        ),
    ]
}

/// `mode.yoloConfirmation.risk`。
pub fn risk(lang: Lang) -> &'static str {
    pick(
        lang,
        "These actions may cause data loss or disclosure. Enable only for tasks you trust. You can switch to another permission mode at any time.",
        "这些操作可能造成数据丢失或泄露。请仅在信任任务内容时启用。你可以随时切回其他权限模式。",
    )
}

/// `mode.yoloConfirmation.acknowledgement`。
pub fn acknowledgement(lang: Lang) -> &'static str {
    pick(
        lang,
        "I understand the risks and take responsibility for my data security.",
        "我已了解风险，并对自己的数据安全负责。",
    )
}

/// `common.cancel`。
pub fn cancel(lang: Lang) -> &'static str {
    pick(lang, "Cancel", "取消")
}

/// `mode.yoloConfirmation.confirm`。
pub fn confirm(lang: Lang) -> &'static str {
    pick(lang, "Confirm", "确认启用")
}

/// 底部的按键提示（`{键}` 模板，tui.md §7.4）。
pub fn hint(lang: Lang) -> &'static str {
    pick(
        lang,
        "{↑/↓} to navigate · {Space} to check · {Enter} to select · {Esc} to cancel",
        "{↑/↓} 移动 · {space} 勾选 · {enter} 选择 · {esc} 取消",
    )
}
