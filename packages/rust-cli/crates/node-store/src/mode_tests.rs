use super::*;
use serde_json::json;

fn state(mode: &str, plan: bool) -> Execution {
    Execution {
        mode: mode.into(),
        plan,
    }
}

/// Node `resolveExecutionState`：合法模式关掉 plan；`plan` 保留当前模式；其余值不变。
#[test]
fn execution_states_resolve_like_node() {
    let current = state("edit", true);
    assert_eq!(resolve(Some("yolo"), &current), state("yolo", false));
    assert_eq!(
        resolve(Some("plan"), &state("edit", false)),
        state("edit", true)
    );
    assert_eq!(resolve(Some("default"), &current), current);
    assert_eq!(resolve(None, &current), current);
    assert_eq!(
        parse(&json!({"mode": "auto", "planEnabled": false, "x": 1})),
        Some(state("auto", false))
    );
    assert_eq!(parse(&json!({"mode": "plan", "planEnabled": true})), None);
    assert_eq!(parse(&json!({"mode": "build"})), None);
}

/// 修复线 F20：执行状态（权限模式 × plan）写进 Node 的 `{mode, planEnabled}` 再读回不变；plan 开关不影响权限模式。
#[test]
fn every_execution_state_reads_back_from_the_state_it_writes() {
    for mode in [
        PermissionMode::Default,
        PermissionMode::AcceptEdits,
        PermissionMode::BypassPermissions,
        PermissionMode::Guarded,
        PermissionMode::DontAsk,
        PermissionMode::Auto,
    ] {
        for plan in [false, true] {
            let state_ = ExecutionState { mode, plan };
            let written = to_node(state_);
            assert_eq!(written.plan, plan);
            assert!(matches(mode, &written), "{mode:?}");
            assert_eq!(from_entry_name(entry_name(mode)), Some(mode));
            // 有 `rust-cli/permission_mode` 时原样读回；dontAsk 在 Node 里就是 build，单靠执行状态读回 default。
            assert_eq!(read(&written, Some(mode)), state_, "{mode:?}");
            let expected = if mode == PermissionMode::DontAsk {
                PermissionMode::Default
            } else {
                mode
            };
            assert_eq!(
                from_node(&written),
                ExecutionState {
                    mode: expected,
                    plan
                }
            );
        }
    }
    // Node 写的 guarded 读回 guarded（F20 之前读成 bypassPermissions）。
    assert_eq!(
        from_node(&state("guarded", true)),
        ExecutionState {
            mode: PermissionMode::Guarded,
            plan: true
        }
    );
    // 旧行里记的 `plan` 不是权限模式：忽略，按执行状态读。
    assert!(!matches(PermissionMode::Plan, &state("edit", true)));
    assert_eq!(
        read(&state("edit", true), Some(PermissionMode::Plan)),
        ExecutionState {
            mode: PermissionMode::AcceptEdits,
            plan: true
        }
    );
    // Node 只有会话行 `mode: plan` 的旧值：plan 开、build。
    assert_eq!(
        from_node(&resolve(Some("plan"), &Execution::default())),
        ExecutionState::from(PermissionMode::Plan)
    );
    assert!(!matches(PermissionMode::DontAsk, &state("edit", true)));
}
