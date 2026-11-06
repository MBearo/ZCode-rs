use super::*;

/// 一个空闲、有会话、什么都能做的界面；测试只改要看的那一条。
#[derive(Default, Clone, Copy)]
struct Facts {
    no_session: bool,
    busy: bool,
    sending: bool,
    asking: bool,
    plan: bool,
    no_clipboard: bool,
    empty: bool,
    no_machines: bool,
}

impl CommandFacts for Facts {
    fn session(&self) -> bool {
        !self.no_session
    }
    fn busy(&self) -> bool {
        self.busy
    }
    fn sending(&self) -> bool {
        self.sending
    }
    fn asking(&self) -> bool {
        self.asking
    }
    fn plan(&self) -> bool {
        self.plan
    }
    fn clipboard(&self) -> bool {
        !self.no_clipboard
    }
    fn nothing_to_copy(&self) -> bool {
        self.empty
    }
    fn nothing_to_export(&self) -> bool {
        self.empty
    }
    fn nothing_to_rewind(&self) -> bool {
        self.empty
    }
    fn machines(&self) -> bool {
        !self.no_machines
    }
}

fn why(name: &str, f: Facts) -> Option<&'static str> {
    unavailable(name, &f).map(Reason::en)
}

#[test]
fn an_idle_session_runs_every_command() {
    for name in [
        "compact", "new", "model", "resume", "rewind", "goal", "stats", "copy", "rename",
    ] {
        assert_eq!(why(name, Facts::default()), None, "{name}");
    }
}

#[test]
fn without_a_session_the_session_commands_say_why() {
    let f = Facts {
        no_session: true,
        ..Facts::default()
    };
    assert_eq!(why("compact", f), Some("Nothing to compact yet."));
    assert_eq!(why("new", f), Some("This is already a new session."));
    assert_eq!(why("stats", f), Some("Send a message first."));
    assert_eq!(
        why("rename", f),
        Some("There is no session to name yet. Send a message first.")
    );
}

#[test]
fn the_first_rule_that_matches_speaks() {
    // 没有会话又在跑：`/compact` 说没有可压缩的，`/new` 说先停下（Puffin 的次序）。
    let f = Facts {
        no_session: true,
        busy: true,
        ..Facts::default()
    };
    assert_eq!(why("compact", f), Some("Nothing to compact yet."));
    assert_eq!(why("new", f), Some("Stop the turn first (esc)"));
    let f = Facts {
        sending: true,
        ..Facts::default()
    };
    assert_eq!(why("compact", f), Some("Wait for this turn to end."));
    assert_eq!(why("resume", f), Some("Wait for your message to go out."));
    let f = Facts {
        no_clipboard: true,
        empty: true,
        ..Facts::default()
    };
    assert_eq!(why("copy", f), Some("This terminal can't copy."));
}

#[test]
fn rewind_waits_then_answers_then_needs_a_point() {
    let f = Facts {
        asking: true,
        empty: true,
        ..Facts::default()
    };
    assert_eq!(rewind_blocked(&f), Some(Reason::AnswerFirst));
    let f = Facts {
        empty: true,
        ..Facts::default()
    };
    assert_eq!(why("rewind", f), Some("Nothing to rewind yet."));
    let f = Facts {
        asking: true,
        ..Facts::default()
    };
    assert_eq!(why("settings", f), Some("Answer the request first."));
    assert_eq!(why("model", f), None);
}

#[test]
fn plan_mode_and_machines_are_their_own_rules() {
    assert_eq!(
        why(
            "goal",
            Facts {
                plan: true,
                ..Facts::default()
            }
        ),
        Some("Leave plan mode first (⇧⇥)")
    );
    assert_eq!(
        why(
            "machines",
            Facts {
                no_machines: true,
                ..Facts::default()
            }
        ),
        Some("Remote machines aren't available here.")
    );
}
