//! 纯 shell 分析（F125 的 T6 收进这里）：Bash 解析、只读判定、去包装与前缀赋值、复合结构与替换的拆分、bfs 的参数、
//! guarded 的危险命令识别。不认识权限类型（规则、模式、判决）；判决链在 crate 根下的模块里用它们。
//!
//! 对外（tools）只经 crate 根转出的 `analyze`、`Analysis`、`Invocation`、`is_read_only`、`strip_safe_wrappers`。

pub(crate) mod argv;
pub(crate) mod bfs_args;
pub(crate) mod compound;
pub(crate) mod danger;
pub(crate) mod parse;
pub(crate) mod readonly;
pub(crate) mod safe_env;
pub(crate) mod strip;
pub(crate) mod subst;
