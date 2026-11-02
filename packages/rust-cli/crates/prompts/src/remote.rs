//! 远端调用失败的说明里工具能懂的动作名（remote-exec.md §23.6，F96）：线上的消息名 → `the file write` 这样的说法，存在
//! `prompts/remote/actions.json`（`default` 是没列的）。文字本身在 [`crate::texts`] 的 `remote_*`。

use std::collections::BTreeMap;
use std::sync::OnceLock;

use rust_cli_base::include_lf;

const ACTIONS: &str = include_lf!("../../../prompts/remote/actions.json");

fn actions() -> &'static BTreeMap<String, String> {
    static ACTIONS_MAP: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    ACTIONS_MAP.get_or_init(|| serde_json::from_str(ACTIONS).unwrap_or_default())
}

/// 失败说明里 `{{instead}}` 的值：失败的是额外机器时建议不写 `machine`、改在缺省机器上跑；失败的就是主 env 时没有
/// 这半句（再建议它自己没有意义，deployment.md §23）。
pub fn instead(primary: bool) -> &'static str {
    if primary {
        ""
    } else {
        crate::texts::remote_instead()
    }
}

/// 消息 `op`（例如 `files.write_file`）在说明里的写法；没列的是 `default`（单测保证有）。
pub fn action(op: &str) -> &'static str {
    let actions = actions();
    actions
        .get(op)
        .or_else(|| actions.get("default"))
        .map_or("the request", String::as_str)
}
