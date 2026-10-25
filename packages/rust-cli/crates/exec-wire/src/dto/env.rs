//! 环境变量推送的线上类型（远端执行 F96，docs/specs/rust-cli/remote-exec.md §23.5）：`env.update` 与 `hello.session_env`。
//!
//! 变量按客户端实例生效，只影响之后启动的进程；值是 null 时是“去掉这个变量”。校验（名字、保护名单、上限）在执行器。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 一组变量：名字 → 值（`None`：去掉这个变量）。
pub type EnvVars = BTreeMap<String, Option<String>>;

/// `env.update` 怎么作用到这个客户端实例已有的那组变量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvMode {
    /// 整组替换（被注入的名字也换成这次给的）。
    Replace,
    /// 合并：给出的名字覆盖或加上，别的不动。
    Merge,
}
