//! 命令行：参数的解析、帮助与 `-p` 的输入，以及会话之外的子命令（账号、信任、更新、桌面端打开文件夹、执行器、内嵌的搜索工具）。

pub(crate) mod args;
// build.rs 的计数与版本规则（读写计数文件、拼接与校验版本），只在单测里引入（docs/specs/rust-cli/version.md §3）。
#[cfg(test)]
pub(crate) mod build_number;
pub(crate) mod commander;
pub(crate) mod desktop_app;
pub(crate) mod effort;
pub(crate) mod embedded;
pub(crate) mod executor;
pub(crate) mod executor_run;
pub(crate) mod flags;
pub(crate) mod help;
pub(crate) mod hooks_trust;
pub(crate) mod login;
pub(crate) mod print_format;
pub(crate) mod stdin_prompt;
pub(crate) mod update;
