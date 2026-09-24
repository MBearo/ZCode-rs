//! Runner replay against Node's `InMemoryHookRunner`, row projection and
//! registration order.
use super::output::{Callback, Diagnostics, Failure};
use super::runner::{Admission, Dispatch, Driver, Kind, Lifecycle, run};
use super::tests::fixtures;
use super::*;
use serde_json::{Value, json};

/// Replays a fixture's hook specs.
struct Replay {
    specs: Vec<Value>,
    hooks: Vec<Registration>,
    ids: usize,
    events: Vec<Lifecycle>,
    spawned: Vec<Dispatch>,
}

impl Replay {
    fn spec(&self, hook: &Registration) -> &Value {
        let index = self
            .hooks
            .iter()
            .position(|h| h.source == hook.source)
            .unwrap();
        &self.specs[index]
    }
}

impl Driver for Replay {
    fn admission(&mut self, hook: &Registration) -> Admission {
        let spec = &self.spec(hook)["admission"];
        if spec.is_null() {
            return Admission::ALLOWED;
        }
        Admission {
            allowed: spec["allowed"] == true,
            reason_code: spec["reasonCode"].as_str().map(str::to_owned),
            skip_lifecycle: spec["skipLifecycle"] == true,
        }
    }
    fn now(&self) -> u64 {
        0
    }
    fn id(&mut self) -> String {
        self.ids += 1;
        format!("id-{}", self.ids)
    }
    async fn emit(&mut self, event: Lifecycle) {
        self.events.push(event);
    }
    async fn execute(&mut self, dispatch: &Dispatch) -> Result<Callback, Failure> {
        let spec = self.spec(&dispatch.hook).clone();
        if let Some(failure) = spec["failure"].as_str() {
            let (kind, message) = failure.split_once(':').unwrap();
            return Err(match kind {
                "timeout" => Failure::timeout(1000),
                "execution" => Failure {
                    code: Some("TOOL_EXECUTION_FAILED"),
                    outcome: "failed",
                    message: message.into(),
                },
                "configuration" => Failure::configuration(message.into()),
                _ => Failure::other(message.into()),
            });
        }
        let d = &spec["diagnostics"];
        Ok(Callback {
            output: spec.get("output").cloned(),
            diagnostics: Diagnostics {
                error_message: d["errorMessage"].as_str().map(str::to_owned),
                stderr_preview: d["stderrPreview"].as_str().map(str::to_owned),
                stdout_preview: d["stdoutPreview"].as_str().map(str::to_owned),
            },
        })
    }
    fn spawn(&mut self, dispatch: Dispatch) {
        self.spawned.push(dispatch);
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
    }
}

/// Node ids are random: number runs by first appearance like the generator.
fn normalize(events: &[Lifecycle]) -> Vec<Value> {
    let mut runs: Vec<String> = vec![];
    events
        .iter()
        .map(|e| {
            let mut payload = e.payload.clone();
            let run = payload["hookRunId"].as_str().unwrap().to_owned();
            if !runs.contains(&run) {
                runs.push(run.clone());
            }
            payload["hookInvocationId"] = "inv".into();
            payload["hookRunId"] =
                format!("run-{}", runs.iter().position(|r| *r == run).unwrap()).into();
            json!({"type":e.kind.as_str(),"payload":payload})
        })
        .collect()
}

#[test]
fn runner_merges_and_reports_like_node() {
    for case in fixtures()["runners"].as_array().unwrap() {
        let event = HookEvent::parse(case["event"].as_str().unwrap()).unwrap();
        let specs = case["specs"].as_array().unwrap().clone();
        let hooks: Vec<Registration> = specs
            .iter()
            .enumerate()
            .map(|(index, spec)| Registration {
                event,
                matcher: None,
                program: Program::Command {
                    command: "hook".into(),
                    shell: Shell::Unset,
                    background: spec["async"] == true,
                },
                source: format!("config.{}.0.{index}", event.as_str()),
                source_kind: SourceKind::User,
                source_path: None,
                plugin: None,
                status_message: None,
                timeout_ms: 1000,
                max_output_bytes: 32768,
                review: None,
            })
            .collect();
        let mut input = json!({"hookEventName":event.as_str(),"sessionId":"s","traceId":"t","turnId":"turn","cwd":"/w"});
        for (key, value) in case["input"].as_object().unwrap() {
            input[key] = value.clone();
        }
        let values: Vec<String> = input["toolName"]
            .as_str()
            .map(|t| vec![t.to_owned()])
            .unwrap_or_default();
        let mut replay = Replay {
            specs,
            hooks: hooks.clone(),
            ids: 0,
            events: vec![],
            spawned: vec![],
        };
        let result = block_on(run(&mut replay, &hooks, &input, &values));
        for dispatch in std::mem::take(&mut replay.spawned) {
            replay.events.push(dispatch.completed(0));
        }
        assert_eq!(result.to_value(), case["result"], "{}", case["name"]);
        let mut got = normalize(&replay.events);
        let mut want = case["events"].as_array().unwrap().clone();
        if case["name"] == "async" {
            // 后台 hook 的结束时刻不确定，只比较事件集合。
            let key = |v: &Value| v.to_string();
            got.sort_by_key(key);
            want.sort_by_key(key);
        }
        assert_eq!(json!(got), json!(want), "{}", case["name"]);
    }
}

#[test]
fn display_names_follow_the_projection() {
    let name = |d: Value, i| display::display_name(&d, i);
    assert_eq!(
        name(
            json!({"commandDisplay":"node \"hook script.js\" --x","statusMessage":" Check "}),
            0
        ),
        "Check"
    );
    assert_eq!(
        name(
            json!({"commandDisplay":"/usr/bin/python3 ./hooks/lint.py"}),
            0
        ),
        "python3 · lint.py"
    );
    assert_eq!(name(json!({"commandDisplay":"bash -c 'x'"}), 0), "bash");
    assert_eq!(
        name(
            json!({"commandDisplay":"./run.sh a","pluginName":"Plug"}),
            1
        ),
        "Plug · run.sh"
    );
    assert_eq!(name(json!({"commandDisplay":"   "}), 2), "Hook #3");
    assert_eq!(
        name(json!({"commandDisplay":"\"C:\\\\a b\\\\x.exe\" y"}), 0),
        "x.exe"
    );
}

#[test]
fn registrations_follow_config_order_and_gates() {
    let hooks = json!({"enabled":true,"timeoutMs":1500.4,"maxOutputBytes":10.5,"events":{
        "Stop":[{"hooks":[{"type":"command","command":"a","timeout":2},{"type":"command","command":"b","enabled":false}]}],
        "PreToolUse":[{"matcher":"Bash","hooks":[{"type":"process","command":"c","args":["x"],"timeout":9}]}]}});
    let list = registrations(&hooks, Some("/u/config.json"));
    // 跨事件的注册顺序不影响执行（每次只运行一个事件的 hooks）；同一事件内保持数组顺序。
    let mut summary: Vec<_> = list
        .iter()
        .map(|r| (r.source.as_str(), r.timeout_ms, r.max_output_bytes))
        .collect();
    summary.sort();
    assert_eq!(
        summary,
        [
            ("config.PreToolUse.0.0", 1500, 11),
            ("config.Stop.0.0", 2000, 11)
        ]
    );
    let pre = list
        .iter()
        .find(|r| r.event == HookEvent::PreToolUse)
        .unwrap();
    assert_eq!(pre.matcher.as_deref(), Some("Bash"));
    assert_eq!(pre.source_path.as_deref(), Some("/u/config.json"));
    assert!(registrations(&json!({"enabled":false,"events":hooks["events"]}), None).is_empty());
}

fn lifecycle(run: &str, index: u64, extra: Value) -> Value {
    let mut payload = json!({"descriptor":{"clientVisible":true,"commandDisplay":"node ./lint.js","sourceKind":"user"},
        "hookEventName":"UserPromptSubmit","hookIndex":index,"hookCount":2,"hookInvocationId":"inv","hookRunId":run,"startedAt":10});
    for (key, value) in extra.as_object().unwrap() {
        payload[key] = value.clone();
    }
    payload
}

#[test]
fn invocation_rows_follow_the_node_projection() {
    use projection::{block_error, close_running, invocation};
    let started = lifecycle("a", 0, json!({}));
    let row = invocation(None, Kind::Started, &started, 10).unwrap();
    assert_eq!(row["state"], "running");
    assert_eq!(row["lane"], "assistantWork");
    assert_eq!(row["executions"][0]["displayName"], "node · lint.js");
    assert_eq!(row["executions"][0]["didExecute"], true);
    let blocked = lifecycle(
        "a",
        0,
        json!({"durationMs":5,"outcome":"blocked","blockReason":"no","stderrPreview":"bad input"}),
    );
    let row = invocation(Some(&row), Kind::Blocked, &blocked, 15).unwrap();
    // hookCount 为 2，第二个 hook 尚未出现：行仍在运行。
    assert_eq!(row["state"], "running");
    assert_eq!(row["executions"][0]["state"], "completed");
    assert_eq!(row["executions"][0]["blockReason"], "no");
    let error = block_error(Kind::Blocked, &blocked, &row, 15, "trace").unwrap();
    assert_eq!(error["message"], "hooks_prompt_block: bad input");
    assert_eq!(
        error["detail"],
        "Hook block reason: no\nHook error: bad input"
    );
    assert_eq!(error["code"], "fault.runtime.hookBlocked");
    // 准入拒绝（未执行）不写 lastError。
    let refused = lifecycle(
        "b",
        1,
        json!({"durationMs":0,"outcome":"blocked","blockReason":"workspace_hooks_pending_trust"}),
    );
    let done = invocation(Some(&row), Kind::Blocked, &refused, 16).unwrap();
    assert!(block_error(Kind::Blocked, &refused, &done, 16, "t").is_none());
    assert_eq!(done["state"], "completed");
    assert_eq!(done["endedAt"], 16);
    assert_eq!(done["durationMs"], 6);
    let failed = lifecycle("a", 0, json!({"durationMs":1,"outcome":"timed_out"}));
    let failed = invocation(Some(&done), Kind::Failed, &failed, 20).unwrap();
    assert_eq!(failed["state"], "failed");
    // 内部 hook 与缺字段的事件不投影。
    let internal = lifecycle(
        "c",
        0,
        json!({"descriptor":{"clientVisible":false,"sourceKind":"internal"}}),
    );
    assert!(invocation(None, Kind::Started, &internal, 1).is_none());
    assert!(invocation(None, Kind::Started, &json!({"hookRunId":"x"}), 1).is_none());
    let mut running = invocation(None, Kind::Started, &started, 10).unwrap();
    assert!(close_running(&mut running, 30));
    assert_eq!(running["state"], "failed");
    assert_eq!(running["executions"][0]["outcome"], "cancelled");
    assert_eq!(running["executions"][0]["durationMs"], 20);
}

#[test]
fn timestamps_are_js_iso_strings() {
    assert_eq!(input::iso_timestamp(0), "1970-01-01T00:00:00.000Z");
    assert_eq!(
        input::iso_timestamp(1_767_225_600_000),
        "2026-01-01T00:00:00.000Z"
    );
    assert_eq!(
        input::iso_timestamp(951_782_400_123),
        "2000-02-29T00:00:00.123Z"
    );
    assert_eq!(
        input::iso_timestamp(1_790_000_000_999),
        "2026-09-21T14:13:20.999Z"
    );
}

#[test]
fn variables_and_env_match_node() {
    let f = fixtures();
    let plugin = Plugin {
        id: "p".into(),
        name: "Plug".into(),
        root_path: "/root".into(),
        data_path: "/data".into(),
        source_path: None,
    };
    for case in f["expansions"]["cases"].as_array().unwrap() {
        let cwd = case.get("cwd").and_then(Value::as_str).unwrap_or("/work");
        let input = json!({"sessionId":"s-1","cwd":cwd});
        let plugin = (case["plugin"] == true).then_some(&plugin);
        let got = input::expand(case["value"].as_str().unwrap(), plugin, &input, "/fallback");
        match case.get("error") {
            Some(error) => assert_eq!(got.unwrap_err(), error.as_str().unwrap()),
            None => assert_eq!(got.unwrap(), case["result"].as_str().unwrap()),
        }
    }
    let input = json!({"sessionId":"s-1","cwd":"/work"});
    for (index, with) in [None, Some(&plugin)].into_iter().enumerate() {
        let env: serde_json::Map<String, Value> = input::env(with, &input, "/fallback")
            .into_iter()
            .map(|(k, v)| (k, v.into()))
            .collect();
        assert_eq!(
            Value::Object(env).to_string(),
            f["expansions"]["env"][index]["set"].to_string()
        );
    }
}
