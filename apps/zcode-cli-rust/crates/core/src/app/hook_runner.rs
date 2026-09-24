//! Hooks of one run (Node `InMemoryHookRunner` bound to a runtime): processes
//! through the tool port, lifecycle events to the engine, `async` hooks in the
//! background. Subagent runs have none.
use crate::contract::{Clock, Event, EventSink, HookProcess, ToolPort};
use crate::domain::hooks::{
    HookEvent, Program, Registration, input,
    output::{self, Callback, Failure, RunResult},
    runner::{self, Admission, Dispatch, Driver, Kind, Lifecycle},
};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

pub(super) struct Hooks {
    pub registrations: Arc<[Registration]>,
    pub tools: Arc<dyn ToolPort>,
    pub clock: Arc<dyn Clock>,
    /// Runtime working directory (hook `cwd` and process directory).
    pub cwd: String,
    pub turn_id: String,
}

impl Hooks {
    pub fn handles(&self, event: HookEvent) -> bool {
        self.registrations.iter().any(|r| r.event == event)
    }

    /// Fields every input shares; `timestamp` is filled per call.
    pub fn base<'a>(&'a self, sink: &'a EventSink, mode: &'a str, now: &'a str) -> input::Base<'a> {
        input::Base {
            agent_name: None,
            cwd: &self.cwd,
            mode,
            session_id: &sink.session_id,
            timestamp: now,
            trace_id: &sink.origin.trace_id,
            turn_id: Some(&self.turn_id),
        }
    }

    pub fn timestamp(&self) -> String {
        input::iso_timestamp(self.clock.now())
    }

    /// Runs every matching hook of `input`'s event (Node `hookRunner.run`).
    pub async fn run(
        &self,
        input: Value,
        values: &[String],
        sink: &EventSink,
        cancel: &CancellationToken,
    ) -> RunResult {
        let event = input["hookEventName"].as_str().and_then(HookEvent::parse);
        if !event.is_some_and(|e| self.handles(e)) {
            return RunResult::default();
        }
        let mut driver = RunDriver {
            hooks: self,
            sink,
            cancel,
        };
        runner::run(&mut driver, &self.registrations, &input, values).await
    }
}

struct RunDriver<'a> {
    hooks: &'a Hooks,
    sink: &'a EventSink,
    cancel: &'a CancellationToken,
}

impl Driver for RunDriver<'_> {
    fn admission(&mut self, _hook: &Registration) -> Admission {
        Admission::ALLOWED
    }
    fn now(&self) -> u64 {
        self.hooks.clock.now()
    }
    fn id(&mut self) -> String {
        self.hooks.clock.id()
    }
    async fn emit(&mut self, event: Lifecycle) {
        emit(self.sink, event).await;
    }
    async fn execute(&mut self, dispatch: &Dispatch) -> Result<Callback, Failure> {
        execute(self.hooks.tools.as_ref(), dispatch, self.cancel).await
    }
    fn spawn(&mut self, dispatch: Dispatch) {
        let tools = self.hooks.tools.clone();
        let clock = self.hooks.clock.clone();
        let sink = self.sink.clone();
        let cancel = self.cancel.clone();
        // async hook 的生命周期独立于本轮：输出不合并，结束时单独上报；仍受本轮取消约束。
        tokio::spawn(async move {
            let event = match execute(tools.as_ref(), &dispatch, &cancel).await {
                Ok(_) => dispatch.completed(clock.now()),
                Err(failure) => dispatch.failed(clock.now(), &failure),
            };
            emit(&sink, event).await;
        });
    }
}

async fn emit(sink: &EventSink, event: Lifecycle) {
    if event.kind == Kind::Failed {
        tracing::warn!(
            target: "zcode::hooks",
            event = "hook.run.failed",
            hook_event = event.payload["hookEventName"].as_str(),
            source = event.payload["hookSource"].as_str(),
            outcome = event.payload["outcome"].as_str(),
            "Hook execution failed"
        );
    }
    // Engine 停止时 run 也会结束；生命周期事件丢失不影响 hook 结果本身。
    let _ = sink.send(Event::Hook(event)).await;
}

/// One foreground hook to its end: variables, the process, then Node
/// `processHookExecutionResult`. Timeouts and cancellation win over the
/// process result like Node's runner timer.
async fn execute(
    tools: &dyn ToolPort,
    dispatch: &Dispatch,
    cancel: &CancellationToken,
) -> Result<Callback, Failure> {
    if cancel.is_cancelled() {
        return Err(Failure::cancelled());
    }
    let hook = &dispatch.hook;
    let hook_input = &dispatch.input;
    let cwd = hook_input["cwd"].as_str().unwrap_or("");
    let expand = |value: &str| {
        input::expand(value, hook.plugin.as_ref(), hook_input, cwd).map_err(Failure::configuration)
    };
    let program = match &hook.program {
        Program::Command {
            command,
            shell,
            background,
        } => Program::Command {
            command: expand(command)?,
            shell: shell.clone(),
            background: *background,
        },
        Program::Process { command, args } => Program::Process {
            command: expand(command)?,
            args: args.iter().map(|a| expand(a)).collect::<Result<_, _>>()?,
        },
    };
    let request = HookProcess {
        program,
        cwd: cwd.into(),
        env: input::env(hook.plugin.as_ref(), hook_input, cwd),
        input: hook_input.clone(),
        timeout: Duration::from_millis(hook.timeout_ms),
        max_output_bytes: hook.max_output_bytes,
    };
    let event = hook_input["hookEventName"]
        .as_str()
        .and_then(HookEvent::parse)
        .ok_or_else(|| Failure::other("Unknown hook event".into()))?;
    match tools.run_hook(request, cancel).await {
        Ok(exec) if exec.status == "timed_out" => Err(Failure::timeout(hook.timeout_ms)),
        Ok(exec) if exec.status == "cancelled" => Err(Failure::cancelled()),
        Ok(exec) => output::callback(event, &exec),
        Err(error) => {
            tracing::warn!(
                target: "zcode::hooks",
                event = "hook.process.failed",
                source = hook.source.as_str(),
                error = %format!("{error:#}"),
                "Hook process could not run"
            );
            Err(Failure::other(format!("{error:#}")))
        }
    }
}
