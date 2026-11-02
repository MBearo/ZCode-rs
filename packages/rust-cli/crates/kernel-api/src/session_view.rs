//! [`SessionView`] 的构造（F126，单独一个文件：session_api.rs 接近行数上限）。

use crate::message::Message;
use crate::model::ModelSelection;
use crate::permission::PermissionMode;
use crate::session_api::SessionView;
use crate::store::SessionMeta;

impl SessionView {
    /// 空闲、没有待回答的交互与排队输入的视图（`seq` 与 `revision` 为 0）；会话 actor 用结构更新填上运行时的状态。
    pub fn new(
        meta: SessionMeta,
        messages: Vec<Message>,
        selection: ModelSelection,
        mode: PermissionMode,
    ) -> Self {
        Self {
            meta,
            messages,
            selection,
            mode,
            plan: false,
            pending: Vec::new(),
            queue: Vec::new(),
            last_seq: 0,
            revision: 0,
            running: false,
            background: Vec::new(),
            peer_held: Vec::new(),
            context_usage: None,
            context_notice: crate::compaction::ContextNotice::default(),
            output_speed: None,
        }
    }
}
