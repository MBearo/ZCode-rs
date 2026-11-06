//! 留下的输入只按信封的 id 离开（`in_flight.rs`）。
use rust_cli_kernel_api::ids::{CommandId, TurnId};
use rust_cli_kernel_api::session_api::{Command, Delivery, KernelEvent, QueuedInput};

use super::{InFlight, input_of};

fn item(id: &str, text: &str) -> QueuedInput {
    QueuedInput {
        command: CommandId::new(id),
        text: text.to_owned(),
        delivery: Delivery::Queue,
    }
}

fn texts(list: &InFlight) -> Vec<&str> {
    list.items().map(|item| item.text.as_str()).collect()
}

#[test]
fn an_input_leaves_when_the_kernel_queues_or_starts_it_by_its_id() {
    let mut list = InFlight::default();
    list.send(item("a", "one"));
    list.send(item("b", "two"));
    // 同样的文字、别的 id：不认领。
    list.claim(&KernelEvent::QueueChanged {
        items: vec![item("other", "one")],
    });
    assert_eq!(texts(&list), ["one", "two"]);
    list.claim(&KernelEvent::QueueChanged {
        items: vec![item("a", "one")],
    });
    assert_eq!(texts(&list), ["two"]);
    list.claim(&KernelEvent::TurnStarted {
        turn: TurnId::new("t"),
        command: Some(CommandId::new("b")),
    });
    assert!(list.is_empty());
}

#[test]
fn a_rejected_input_leaves_and_a_snapshot_keeps_only_the_unanswered() {
    let mut list = InFlight::default();
    list.send(item("a", "one"));
    list.send(item("b", "two"));
    list.send(item("c", "three"));
    list.acked(&CommandId::new("a"), false);
    assert_eq!(texts(&list), ["two", "three"]);
    // b 收下了（它的回合或队列在快照里），c 还没回执。
    list.acked(&CommandId::new("b"), true);
    assert_eq!(texts(&list), ["two", "three"]);
    let list = list.carried(&[]);
    assert_eq!(texts(&list), ["three"]);
    let list = list.carried(&[item("c", "three")]);
    assert!(list.is_empty());
}

#[test]
fn inputs_are_kept_as_the_kernel_will_queue_them() {
    let submit = Command::Submit {
        text: "hi".into(),
        attachments: Vec::new(),
        delivery: Delivery::Steer,
    };
    assert_eq!(input_of(&submit), Some(("hi".into(), Delivery::Steer)));
    let bash = Command::Bash {
        command: "ls".into(),
        delivery: Delivery::Steer,
    };
    assert_eq!(
        input_of(&bash),
        Some(("<bash-input>ls</bash-input>".into(), Delivery::Queue))
    );
    assert_eq!(input_of(&Command::Shutdown), None);
}
