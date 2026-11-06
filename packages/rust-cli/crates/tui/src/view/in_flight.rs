//! 发出了、内核还没认领的输入（Puffin `ShellState::outgoing`，docs/specs/rust-cli/tui.md §5.1.2）：按键的那一帧消息就在
//! 栈上或 transcript 的尾巴里，与内核排上之后同一个样子、同一个位置。
//!
//! 唯一的键是信封的 [`CommandId`]；不按文字配对，没有超时。内核收下一条输入时先发出带这个 id 的事实，再回执
//! （session/src/actor.rs：运行中 `QueueChanged` 里有它，空闲时 `TurnStarted { command }` 是它），所以一条留下的输入在下面三种
//! 情况离开，都是同一个折叠里的事实：
//!
//! - `QueueChanged` 里有它：之后由内核的队列画（同一行，不闪）；
//! - `TurnStarted` 带它：它开了回合，用户行在同一次折叠里接替；
//! - 回执是拒绝：它不会出现了。
//!
//! 快照（[`Transcript::from_view`](super::Transcript::from_view)）接替时，快照的队列里有的与已经收下的（回合或队列已在快照里）
//! 离开，还没回执的留着（它的事实在快照之后到）。
use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::session_api::{Command, Delivery, KernelEvent, QueuedInput};

/// 留下的输入，按发出的顺序。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InFlight {
    items: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq)]
struct Entry {
    input: QueuedInput,
    /// 内核已经收下（回执是接受）。
    accepted: bool,
}

impl InFlight {
    /// 发出一条输入（runner 建好信封之后、发出之前；孪生在它的 Submit 里）。
    pub fn send(&mut self, input: QueuedInput) {
        self.items.push(Entry {
            input,
            accepted: false,
        });
    }

    pub fn items(&self) -> impl Iterator<Item = &QueuedInput> {
        self.items.iter().map(|entry| &entry.input)
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 内核的事实认领了它（见模块说明）。
    pub fn claim(&mut self, event: &KernelEvent) {
        match event {
            KernelEvent::QueueChanged { items } => self
                .items
                .retain(|entry| items.iter().all(|item| item.command != entry.input.command)),
            KernelEvent::TurnStarted {
                command: Some(command),
                ..
            } => self.items.retain(|entry| entry.input.command != *command),
            _ => {}
        }
    }

    /// 回执：拒绝时拿掉，接受时记下（快照接替时用）。
    pub fn acked(&mut self, command: &CommandId, accepted: bool) {
        if accepted {
            if let Some(entry) = self.items.iter_mut().find(|e| e.input.command == *command) {
                entry.accepted = true;
            }
        } else {
            self.items.retain(|entry| entry.input.command != *command);
        }
    }

    /// 快照接替：只留下快照的队列里没有、也还没回执的。
    pub fn carried(mut self, queue: &[QueuedInput]) -> Self {
        self.items.retain(|entry| {
            !entry.accepted && queue.iter().all(|item| item.command != entry.input.command)
        });
        self
    }
}

/// 一条输入命令留下时的文字与投递，与内核排上它时相同（session/src/actor.rs 与 bash.rs）：`Submit` 用它自己的投递与文字，
/// `SubmitContent` 是文字块用换行连起来、`Bash` 是 `<bash-input>`，这两种总是 `Queue`。别的命令不是输入，没有。
pub fn input_of(command: &Command) -> Option<(String, Delivery)> {
    match command {
        Command::Submit { text, delivery, .. } => Some((text.clone(), *delivery)),
        Command::SubmitContent { content, .. } => {
            let text = content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            Some((text, Delivery::Queue))
        }
        Command::Bash { command, .. } => Some((
            rust_cli_kernel_api::user_input::bash_input(command),
            Delivery::Queue,
        )),
        _ => None,
    }
}

#[cfg(test)]
#[path = "in_flight_tests.rs"]
mod tests;
