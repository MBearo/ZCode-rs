//! `update` 之后运行时执行的效果。
//! 移植自 packages/tui/tui/src/zcode/tui/app/Cmd.scala（feat/tui a2bf878b34）：IO 变成 future，`toMsg` 已在 future 里应用，
//! 所以 Task 的类型参数只有消息。
use futures_util::future::BoxFuture;

/// `Fire` 的同步副作用。
pub type Effect = Box<dyn FnOnce() + Send + 'static>;

pub enum Cmd<M> {
    None,
    Quit,
    /// 从头重画：运行时丢掉渲染器的 diff 基线，终端上真实的格子与模型不一致时（终端的 bug、线路噪声、别的进程写了 tty）
    /// 重新对齐。
    Refresh,
    Batch(Vec<Cmd<M>>),
    /// 没有结果消息的副作用。
    Fire(Effect),
    /// 在运行时的 scope 里跑，结果作为消息送回；运行时退出时取消，不送消息。错误已经在 future 里变成消息
    /// （Scala 的 `Either[String, A] => Msg`）。
    Task(BoxFuture<'static, M>),
    /// 把终端交给别的程序（编辑器，docs/puffin-tui.md §4.2 设置）：离开备用屏与原始模式、停读输入；future 结束时屏幕整个
    /// 回来，结果作为消息送回。
    Handoff(BoxFuture<'static, M>),
}

impl<M> Cmd<M> {
    pub fn batch(cmds: impl IntoIterator<Item = Cmd<M>>) -> Cmd<M> {
        Cmd::Batch(cmds.into_iter().collect())
    }

    pub fn fire(effect: impl FnOnce() + Send + 'static) -> Cmd<M> {
        Cmd::Fire(Box::new(effect))
    }

    pub fn task(work: impl std::future::Future<Output = M> + Send + 'static) -> Cmd<M> {
        Cmd::Task(Box::pin(work))
    }

    pub fn handoff(work: impl std::future::Future<Output = M> + Send + 'static) -> Cmd<M> {
        Cmd::Handoff(Box::pin(work))
    }

    pub fn is_none(&self) -> bool {
        matches!(self, Cmd::None)
    }

    /// 把 `f` 套在每条结果消息上（子组件的消息包进壳的消息）。
    pub fn map<N: Send + 'static>(
        self,
        f: impl Fn(M) -> N + Send + Sync + Clone + 'static,
    ) -> Cmd<N>
    where
        M: Send + 'static,
    {
        use futures_util::FutureExt;
        match self {
            Cmd::None => Cmd::None,
            Cmd::Quit => Cmd::Quit,
            Cmd::Refresh => Cmd::Refresh,
            Cmd::Batch(cs) => Cmd::Batch(cs.into_iter().map(|c| c.map(f.clone())).collect()),
            Cmd::Fire(e) => Cmd::Fire(e),
            Cmd::Task(w) => Cmd::Task(w.map(f).boxed()),
            Cmd::Handoff(w) => Cmd::Handoff(w.map(f).boxed()),
        }
    }
}

impl<M> std::fmt::Debug for Cmd<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Cmd::None => f.write_str("None"),
            Cmd::Quit => f.write_str("Quit"),
            Cmd::Refresh => f.write_str("Refresh"),
            Cmd::Batch(cs) => f.debug_list().entries(cs).finish(),
            Cmd::Fire(_) => f.write_str("Fire(..)"),
            Cmd::Task(_) => f.write_str("Task(..)"),
            Cmd::Handoff(_) => f.write_str("Handoff(..)"),
        }
    }
}
