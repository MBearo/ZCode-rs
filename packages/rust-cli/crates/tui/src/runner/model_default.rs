//! `/model` 与 meta+p 的选择（docs/specs/rust-cli/workspace-trust.md §3.2，Node `handleModelCommand` +
//! `rememberCurrentModelSelection`）：先换模型；会话 actor 接受之后（没有会话时立即）把模型与档位写成新工作区的
//! 默认模型。写入经入口注入的 `Preferences::save_default_model`，与 `/effort`、登录是同一个 platform 函数
//! （同一把目录锁）。写失败只警告，会话的模型不回退。
use futures_util::FutureExt;
use ratatui::backend::Backend;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::session_api::{Ack, Command};

use super::Loop;
use super::session::Done;
use crate::ui::i18n;

/// Node 在选择没有档位时的报错（`rememberCurrentModelSelection`）。
const INCOMPLETE: &str = "Current model selection is incomplete.";

/// 异步结果。
pub(crate) enum ModelDone {
    /// 会话对 `SetModel` 的回执；被接受后才写默认模型。`args` 是 `/model <args>` 的参数（命令记录用，修复线 L-A），
    /// 选择器与 meta+p 的选择没有。
    Chosen {
        selection: ModelSelection,
        ack: Ack,
        args: Option<String>,
    },
    /// 默认模型写完：成功时是文件路径。
    Saved(Result<String, String>),
}

impl<B: Backend> Loop<'_, B> {
    pub(super) fn choose_model(&mut self, selection: ModelSelection, args: Option<String>) {
        let Some(session) = self.session.clone() else {
            // 没有会话：切换只在新会话的草稿里，立即生效，立即写。
            if let Some(args) = &args {
                let saved = self.saves_default(&selection);
                self.record_model(args, &selection.model, saved);
            }
            self.set_model(selection.clone());
            self.remember_model(selection);
            return;
        };
        let envelope = self.envelope(Command::SetModel {
            selection: selection.clone(),
        });
        self.pending.push(
            async move {
                let ack = session.send(envelope).await;
                Done::Model(ModelDone::Chosen {
                    selection,
                    ack,
                    args,
                })
            }
            .boxed(),
        );
    }

    /// 这次切换会不会存成新会话的缺省（`/model` 的命令记录的说法，修复线 L-A）。
    fn saves_default(&self, selection: &ModelSelection) -> bool {
        self.options.services.preferences.is_some() && selection.reasoning.is_some()
    }

    /// 写成新会话的默认模型。没有注入存储（显式覆盖、测试）时不写，与 Node 没有 registry 时相同。
    fn remember_model(&mut self, selection: ModelSelection) {
        let Some(store) = self.options.services.preferences.clone() else {
            return;
        };
        if selection.reasoning.is_none() {
            self.app
                .error(i18n::default_model_unsaved(self.app.lang, INCOMPLETE));
            return;
        }
        let saving = store.save_default_model(selection);
        self.pending
            .push(async move { Done::Model(ModelDone::Saved(saving.await)) }.boxed());
    }

    pub(super) fn on_model(&mut self, done: ModelDone) {
        let lang = self.app.lang;
        match done {
            ModelDone::Chosen {
                selection,
                ack,
                args,
            } => match ack {
                Ack::Accepted { .. } => {
                    self.app.info(i18n::model_set(lang, &selection.model));
                    if let Some(args) = &args {
                        let saved = self.saves_default(&selection);
                        self.record_model(args, &selection.model, saved);
                    }
                    self.remember_model(selection);
                }
                Ack::Rejected { reason } => self.app.error(i18n::rejected(lang, &reason)),
                // 重复的命令 ID：第一次的结果已经处理过。
                Ack::Duplicate => {}
            },
            ModelDone::Saved(Ok(_)) => {}
            ModelDone::Saved(Err(reason)) => {
                self.app.error(i18n::default_model_unsaved(lang, &reason));
            }
        }
    }
}
