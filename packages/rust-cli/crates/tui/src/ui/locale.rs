//! `/locale`（别名 `/language`，docs/specs/rust-cli/tui.md §7.1，Node `handleLocaleCommand`）：查看或切换
//! 界面语言。切换先写回配置文件，由 runner 在写回成功后改语言（Node 先写后改）。
use super::app::{Action, App};
use super::i18n;
use crate::services::UiLocale;

impl App {
    pub(super) fn locale_command(&mut self, args: &str) -> Option<Action> {
        let args = args.trim();
        if args.is_empty() || args == "status" || args == "list" {
            self.info(i18n::locale_status(self.lang, self.lang.code()));
            return None;
        }
        match UiLocale::parse(args) {
            Some(locale) => Some(Action::SetLocale(locale)),
            None => {
                self.info(i18n::locale_unsupported(self.lang, args));
                None
            }
        }
    }
}
