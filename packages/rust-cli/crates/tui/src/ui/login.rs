//! `/login` 与 `/logout`（docs/specs/rust-cli/tui.md §7.2，Node `command-center/create.ts`）：命令的词。`/login` 打开门道
//! （`doorway.rs`，Puffin 的「接入模型」，tui-kit.md §4.2「登录」）或直接到它的登录、粘贴密钥；`/logout` 停在设置的 Sign out
//! 行。2026-10-31 以前是板上的登录视图（四个选项与 key 的输入行），换成门道后删掉。
use super::app::{Action, App};
use super::i18n;
use crate::services::{Family, RedactedString};

impl App {
    /// `/login [方式]`（tui.md §7.2 的命令表）。
    pub(super) fn login_command(&mut self, args: &str) -> Option<Action> {
        let mut words = args.split_whitespace();
        let kind = words.next();
        let key = words.collect::<Vec<_>>().join(" ");
        // 账号体系的名字只在 platform 解析（F58：以前这里另写了一个闭包）。
        let family = Family::parse;
        let Some(kind) = kind else {
            self.open_door(None);
            return None;
        };
        if let Some(name) = kind.strip_suffix("-coding-plan-api-key")
            && let Some(family) = family(name)
        {
            if key.is_empty() {
                self.door_paste(family);
                return None;
            }
            // 带 key 的一行已经进了输入框的历史：去掉（D7）。
            self.composer.forget_last();
            return Some(Action::ConfigureKey {
                family,
                key: RedactedString::new(key),
            });
        }
        if let Some(name) = kind.strip_suffix("-coding-plan")
            && let Some(family) = family(name)
            && key.is_empty()
        {
            return self.door_sign_in(family);
        }
        self.info(i18n::login_usage(self.lang));
        None
    }

    /// `/logout`：带参数时提示用法。
    pub(super) fn logout_command(&mut self, args: &str) -> Option<Action> {
        if args.trim().is_empty() {
            return self.settings_sign_out();
        }
        self.info(i18n::logout_usage(self.lang));
        None
    }
}
