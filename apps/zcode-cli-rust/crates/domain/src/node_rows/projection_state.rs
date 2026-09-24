//! The initial conversation snapshot and the derived action guards (Node
//! `projection-state.ts`).
use serde_json::{Map, Value, json};

/// Node `AvailabilityContext`.
pub struct Context<'a> {
    pub phase: &'a str,
    pub goal_status: Option<&'a str>,
    pub compacting: bool,
    pub goal_verifying: bool,
    pub queue_length: usize,
    pub auto_drain: bool,
}

impl<'a> Context<'a> {
    /// The guard inputs of a snapshot state with `control`, `goal` and `queue`.
    pub fn of(control: &'a Value, goal: &'a Value, queue: &'a Value) -> Self {
        let works = control["activeWorks"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        Self {
            phase: control["phase"].as_str().unwrap_or(""),
            goal_status: goal["status"].as_str(),
            compacting: works.iter().any(|w| w["kind"] == "compact"),
            goal_verifying: works.iter().any(|w| w["kind"] == "goalVerifier"),
            queue_length: queue["items"].as_array().map_or(0, Vec::len),
            auto_drain: queue["autoDrain"] != false,
        }
    }
}

fn allowed() -> Value {
    json!({"allowed": true})
}

fn denied(reason: &str) -> Value {
    json!({"allowed": false, "reasonCode": reason})
}

/// Node `computeAvailability`.
pub fn availability(c: &Context) -> Value {
    let running = matches!(c.phase, "running" | "prewarming");
    let lock = || denied("compactOperationLock");
    let compact = if c.compacting {
        lock()
    } else if c.phase == "draft" {
        denied("idleCannotCompact")
    } else {
        allowed()
    };
    let fork = if c.compacting {
        lock()
    } else if c.phase == "draft" {
        denied("forkTargetNotStable")
    } else {
        allowed()
    };
    let send_now = if c.compacting {
        lock()
    } else if running {
        allowed()
    } else {
        denied("sendQueuedNowRequiresRunning")
    };
    let pause = match c.goal_status {
        Some("active" | "verifying" | "notSatisfied") => allowed(),
        None => denied("noGoalToPause"),
        Some(_) => denied("goalNotActive"),
    };
    let resume = match c.goal_status {
        Some("paused") => allowed(),
        None => denied("noGoalToResume"),
        Some(_) => denied("goalNotPaused"),
    };
    json!({
        "fork": fork,
        "compact": compact,
        "switchModelConfig": allowed(),
        "setFollowupMode": allowed(),
        "queueEdit": allowed(),
        "sendQueuedNow": send_now,
        "pauseGoal": pause,
        "resumeGoal": resume,
    })
}

/// Node `computeInputRouting`.
pub fn input_routing(c: &Context, followup_mode: &str) -> Value {
    if c.compacting {
        return json!({"mode": "enqueue", "reasonCode": "compactingAcceptsFutureInput"});
    }
    if c.goal_verifying {
        return json!({"mode": "enqueue", "reasonCode": "goalVerifierAcceptsFutureInput"});
    }
    if matches!(c.phase, "running" | "prewarming") {
        let mode = if followup_mode == "guide" {
            "guide"
        } else {
            "enqueue"
        };
        return json!({ "mode": mode });
    }
    let completed = matches!(c.phase, "completedSuccess" | "completedInterrupted");
    if completed && c.queue_length > 0 && !c.auto_drain {
        return json!({"mode": "choice", "reasonCode": "heldQueueInputRequiresChoice"});
    }
    json!({"mode": "startNow"})
}

/// Node `createInitialConversationSnapshot` without the rows and the
/// publication counters (`protocolVersion`, `sessionId`, `logEpoch`, `seq`,
/// `revision`).
pub fn initial() -> Map<String, Value> {
    let control = json!({
        "phase": "draft",
        "sessionEnded": false,
        "canStop": false,
        "stopState": "idle",
        "stopTargetKind": "unknown",
        "activeWorks": [],
        "lastError": null,
        "apiRetry": null,
    });
    let queue = json!({"items": [], "autoDrain": true});
    let context = Context::of(&control, &Value::Null, &queue);
    let (availability, routing) = (availability(&context), input_routing(&context, "queue"));
    let zero =
        json!({"inputTokens": 0, "outputTokens": 0, "cacheReadTokens": 0, "cacheWriteTokens": 0});
    let state = json!({
        "control": control,
        "availability": availability,
        "inputRouting": routing,
        "meta": {"title": "", "titleSource": "default"},
        "config": {"provider": "", "model": "", "thought": "", "thoughtLevels": [],
            "followupMode": "queue", "mode": "build"},
        "modelTransition": null,
        "usage": {"contextWindow": null, "cumulative": zero},
        "queue": queue,
        "pendingInteractions": [],
        "pendingCommands": [],
        "backgroundWorks": [],
        "subagents": {"revision": 0, "childSessionIds": [], "running": [], "endedTotal": 0},
        "goal": null,
        "plan": null,
        "workspaceHookAdmission": null,
    });
    match state {
        Value::Object(map) => map,
        _ => unreachable!("the initial snapshot is an object"),
    }
}
