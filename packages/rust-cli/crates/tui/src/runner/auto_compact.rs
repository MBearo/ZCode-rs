//! `/autocompact` 在 runner 里的部分（ui/auto_compact.rs；docs/specs/rust-cli/tui.md §6）：读或写
//! `compact.autoThresholdTokens`。写法与设置页的「自动压缩」一行相同（runner/settings.rs 的 `Change::AutoCompact`）：先写用户级
//! 配置，再换掉进程里的值，正在运行的会话从下一次判定起按它压缩；项目或环境钉住了它时写了也不生效，照实说。
use futures_util::FutureExt;

use super::Loop;
use super::manage::ManageDone;
use super::session::Done;
use crate::ui::auto_compact as words;

impl Loop<'_> {
    /// `at` 为 None：说现在在哪里压缩（设置与视图里会话的门槛）；否则写 `at`（`None` 去掉设置）。
    pub(super) fn auto_compact(&mut self, at: Option<Option<u64>>) {
        let lang = self.app.lang;
        let services = &self.options.services;
        let Some(at) = at else {
            match &services.compact_at {
                Some((setting, _)) => {
                    let effective = self.app.view.context_usage().and_then(|u| u.compact_at);
                    self.app.info(words::shown(lang, setting.get(), effective));
                }
                None => self.app.error(words::read_failed(lang, words::UNAVAILABLE)),
            }
            return;
        };
        let compact = services.compact_at.clone();
        let preferences = services.preferences.clone();
        self.pending.push(
            async move {
                let result = async {
                    let (cell, pinned) = compact.ok_or(words::UNAVAILABLE)?;
                    preferences.save_compact_at(at).await?;
                    if let Some(file) = pinned {
                        return Ok(Some(format!("{file} sets compact.autoThresholdTokens")));
                    }
                    cell.set(at);
                    Ok(None)
                }
                .await;
                Done::Manage(ManageDone::AutoCompactSaved { at, result })
            }
            .boxed(),
        );
    }

    /// 写完：存下了说新的位置；没有到达运行的会话时说原因（✗）；没存下说为什么（✗）。
    pub(super) fn auto_compact_saved(
        &mut self,
        at: Option<u64>,
        result: Result<Option<String>, String>,
    ) {
        let lang = self.app.lang;
        match result {
            Ok(None) => self.app.info(words::saved(lang, at)),
            Ok(Some(why)) => {
                tracing::warn!(%why, "autocompact: saved but the session did not reload it");
                self.app.error(words::unreloaded(lang, &why));
            }
            Err(why) => self.app.error(words::save_failed(lang, &why)),
        }
    }
}
