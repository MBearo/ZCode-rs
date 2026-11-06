//! `/model` 与 meta+p 的选择（docs/specs/rust-cli/workspace-trust.md §3.2，Node `handleModelCommand` +
//! `rememberCurrentModelSelection`）：先换模型；会话 actor 接受之后（没有会话时立即）把模型与档位写成新工作区的
//! 默认模型。写入经入口注入的 `Preferences::save_default_model`，与登录是同一个 platform 函数
//! （同一把目录锁）。写失败只警告，会话的模型不回退。
use futures_util::FutureExt;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::session_api::{Ack, Command};

use super::Loop;
use super::session::Done;
use crate::ui::i18n;

/// Node 在选择没有档位时的报错（`rememberCurrentModelSelection`）。
const INCOMPLETE: &str = "Current model selection is incomplete.";

/// 异步结果。
pub(crate) enum ModelDone {
    /// 会话对 `SetModel` 的回执；被接受后才写默认模型（`remember` 为假时不写）。`args` 是 `/login` 之后按名字切换时的
    /// 模型名（`/model` 的命令记录用，修复线 L-A），选择器与 meta+p 的选择没有。
    Chosen {
        selection: ModelSelection,
        ack: Ack,
        args: Option<String>,
        remember: bool,
    },
    /// 默认模型写完：成功时是文件路径。
    Saved(Result<String, String>),
}

impl Loop<'_> {
    /// 换模型。`remember`：之后写成新会话的默认模型；选择器的 `auto` 一档不写（只对本会话，tui-kit.md §4.2）。
    pub(super) fn choose_model(
        &mut self,
        selection: ModelSelection,
        args: Option<String>,
        remember: bool,
    ) {
        let Some(session) = self.outbox.session().cloned() else {
            // 没有会话：切换只在新会话的草稿里，立即生效，立即写。
            if let Some(args) = &args {
                let saved = self.saves_default(&selection);
                self.record_model(args, &selection.model, saved);
            }
            self.set_model(selection.clone());
            if remember {
                self.remember_model(selection);
            }
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
                    remember,
                })
            }
            .boxed(),
        );
    }

    /// 这次切换会不会存成新会话的缺省（`/model` 的命令记录的说法，修复线 L-A）。
    fn saves_default(&self, selection: &ModelSelection) -> bool {
        selection.reasoning.is_some()
    }

    /// 写成新会话的默认模型。
    fn remember_model(&mut self, selection: ModelSelection) {
        if selection.reasoning.is_none() {
            self.app
                .error(i18n::default_model_unsaved(self.app.lang, INCOMPLETE));
            return;
        }
        let saving = self
            .options
            .services
            .preferences
            .save_default_model(selection);
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
                remember,
            } => match ack {
                Ack::Accepted { .. } => {
                    self.app.info(i18n::model_set(lang, &selection.model));
                    if let Some(args) = &args {
                        let saved = self.saves_default(&selection);
                        self.record_model(args, &selection.model, saved);
                    }
                    if remember {
                        self.remember_model(selection);
                    }
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
