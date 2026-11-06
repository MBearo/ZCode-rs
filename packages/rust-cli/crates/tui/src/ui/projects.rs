//! `/resume` 的会话列表（`sessions_list.rs`、`sessions_steps.rs`）要 runner 做的事：问内核取会话、搜索、改名、删除、读一个
//! 会话的对话，切到别的目录的会话之前做信任检查。以前的 ← 项目页与 `/resume` 整页已删（tui-kit.md §4.3）；← 现在是代理
//! 驾驶舱（tui-cockpit.md），它也经这里停止、打开与记下分组。
use rust_cli_kernel_api::store::SessionOrder;

/// 列表一次最多取的会话数（tui-panels.md §2.1）。
pub const LIMIT: usize = 200;

/// 要 runner 去做的事（问内核、切换会话、信任检查）。
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// 取会话：`root` 是目录（None 是所有文件夹）。
    LoadResume {
        root: Option<String>,
        order: SessionOrder,
    },
    /// 列表搜这个词：`root` 的会话（None 是所有文件夹）按 Puffin host 的规则筛出、带上命中的那段，按 `order` 排（列表的 ⌃e）。
    Search {
        query: String,
        root: Option<String>,
        order: SessionOrder,
    },
    /// 打开会话：别的目录的先做信任检查；`cd` 是打开后要换到的目录。`startup`：`--resume` 不带参数时的列表选中的，打开时
    /// 不写续写占位，同 `--resume <id>`（session-lifecycle.md §6.3.1，工作线 F79）。
    Open {
        id: String,
        root: String,
        cd: Option<String>,
        startup: bool,
    },
    /// 列表上的 ⌃b：在 `root` 新建会话（别的目录先做信任检查，再同 `/new`，tui-panels.md §2.4）。
    New {
        root: String,
    },
    Rename {
        id: String,
        title: String,
    },
    Delete(String),
    /// 信任对话框里同意了：写信任记录，再做 `then`（不再检查）。
    Trusted {
        root: String,
        then: Box<Request>,
    },
    /// ⌃t 读一个会话的对话要的只读快照。
    Transcript(String),
    /// 驾驶舱的 ⌃x（tui-cockpit.md §1.3）：`address` 是别的进程，None 是现在的会话。
    Stop {
        address: Option<rust_cli_kernel_api::peers::PeerAddress>,
    },
    /// 去那个 tmux 窗格（§3.3）。
    GoTo(rust_cli_kernel_api::peers::TmuxPlace),
    /// 第一次有词时读这些会话最后的回复（§2.3）。
    Replies(Vec<String>),
    /// 这个会话算看过（§1.2）。
    Seen(String),
    /// ⇥ 选的分组（`puffin-tui/cockpit.json`）。
    RememberGrouping(bool),
}
