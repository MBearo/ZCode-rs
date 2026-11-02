//! 按剧本回答的模型端口（F126 加了选项）：闸门、按来源分流、能力的回答。以前每种需要各写一个包一层的替身。

use rust_cli_base::sync::LockUnpoisoned;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use futures_util::future::{self, BoxFuture, Either};
use futures_util::stream::{self, BoxStream, StreamExt};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::model::{ModelError, ModelEvent, ModelPort, ModelRequest, ModelSelection, QuerySource};
use crate::provider::ModelCapabilities;

type Events = Vec<Result<ModelEvent, ModelError>>;
/// 按来源分流的一条规则：匹配的请求交给这个剧本。
type Route = (fn(&QuerySource) -> bool, Arc<ScriptedModel>);

/// 按顺序返回预先写好的响应，并记录收到的请求。剧本用完后回一个 `Protocol` 错误。
///
/// 选项（都可以不用）：
/// - [`ScriptedModel::gated`] / [`ScriptedModel::gated_at`]：响应等闸门的一个许可才流出；等待时被取消则只回
///   `ModelError::Cancelled`；
/// - [`ScriptedModel::by_source`]：来源匹配的请求交给另一个剧本（那边记它的请求，这边不记）；
/// - [`ScriptedModel::capabilities`]：`capabilities()` 的回答（缺省答不出）。
#[derive(Default)]
pub struct ScriptedModel {
    responses: Mutex<VecDeque<Events>>,
    requests: Mutex<Vec<ModelRequest>>,
    /// 每个响应都等的闸门。
    gate: Option<Arc<Notify>>,
    /// 第 n 个响应（从 0 数，只数这个剧本回答的请求）等的闸门。
    gates: BTreeMap<usize, Arc<Notify>>,
    routes: Vec<Route>,
    capabilities: Option<ModelCapabilities>,
}

impl ScriptedModel {
    pub fn new(responses: Vec<Events>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            ..Self::default()
        }
    }

    /// 每个响应都等 `gate` 的一个许可。
    #[must_use]
    pub fn gated(mut self, gate: Arc<Notify>) -> Self {
        self.gate = Some(gate);
        self
    }

    /// 第 `index` 个响应（从 0 数）等 `gate` 的一个许可。
    #[must_use]
    pub fn gated_at(mut self, index: usize, gate: Arc<Notify>) -> Self {
        self.gates.insert(index, gate);
        self
    }

    /// `source` 为真的请求交给 `model`；按添加的顺序找第一个匹配的。
    #[must_use]
    pub fn by_source(
        mut self,
        source: fn(&QuerySource) -> bool,
        model: Arc<ScriptedModel>,
    ) -> Self {
        self.routes.push((source, model));
        self
    }

    /// `capabilities()` 回答 `capabilities`（None 是答不出，同缺省）。
    #[must_use]
    pub fn capabilities(mut self, capabilities: Option<ModelCapabilities>) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock_unpoisoned().clone()
    }
}

impl ModelPort for ScriptedModel {
    fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        if let Some((_, model)) = self
            .routes
            .iter()
            .find(|(source, _)| source(&request.source))
        {
            return model.stream(request, cancel);
        }
        let index = {
            let mut requests = self.requests.lock_unpoisoned();
            requests.push(request);
            requests.len() - 1
        };
        let events = self
            .responses
            .lock_unpoisoned()
            .pop_front()
            .unwrap_or_else(|| {
                vec![Err(ModelError::Protocol {
                    message: "no scripted response left".to_owned(),
                })]
            });
        let Some(gate) = self.gates.get(&index).or(self.gate.as_ref()).cloned() else {
            return stream::iter(events).boxed();
        };
        stream::once(async move {
            let open = std::pin::pin!(gate.notified());
            let cancelled = std::pin::pin!(cancel.cancelled());
            match future::select(open, cancelled).await {
                Either::Left(_) => stream::iter(events),
                Either::Right(_) => stream::iter(vec![Err(ModelError::Cancelled)]),
            }
        })
        .flatten()
        .boxed()
    }

    fn capabilities(
        &self,
        _selection: &ModelSelection,
    ) -> BoxFuture<'static, Option<ModelCapabilities>> {
        let capabilities = self.capabilities;
        Box::pin(async move { capabilities })
    }
}
