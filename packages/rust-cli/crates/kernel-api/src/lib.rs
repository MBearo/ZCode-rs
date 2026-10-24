//! 内核的端口与公共类型（docs/specs/rust-cli/README.md §3）。v1 由 M0 冻结。
//!
//! 只有类型与 trait，没有 IO 与实现。依赖方向：内核能力 crate 只依赖本 crate（与 base），
//! 适配器实现这里的端口，runtime 组装。修改本 crate 的接口要单独提交、先合入，并更新 spec。
//!
//! | 模块 | 内容 |
//! | --- | --- |
//! | [`abort`] | 带原因的中止信号 |
//! | [`agents`] | 子代理与任务清单的端口（子循环工厂、会话信号、任务清单存储） |
//! | [`checkpoint`] | 文件 checkpoint 与工具结果的配对规则 |
//! | [`ids`] | 类型化 ID |
//! | [`message`] | 会话消息与内容块 |
//! | [`metering`] | 用量的计量事实（模型请求、工具调用、回合）与 `/usage` 的查询 |
//! | [`model`] | 模型端口，canonical 请求是 Anthropic Messages |
//! | [`env`] | 执行环境的公共类型（路径、工作区、操作上下文、终态、文件读写、错误） |
//! | [`machine`] | 一台机器上的执行端口（环境事实、文件、搜索、命令、shell、后台命令、回退、PDF）与 `MachineEnv` |
//! | [`local_services`] | 只在本机的服务（Git 与 worktree、取网页）与 `LocalServices` |
//! | [`env_registry`] | 执行环境的注册表（机器名 → `MachineEnv`，本机的 `LocalServices`） |
//! | [`machine_param`] | 工具的 `machine` 参数：哪些工具带它、发出时的 schema、从输入里取出（R3） |
//! | [`machine_notices`] | 本机与连上的远端机器的后台命令通知合成一个流（R3） |
//! | [`env_shell`] | shell 执行的 DTO（shell 选择、cwd 延续、sandbox、后台通知） |
//! | [`env_command`] | 执行环境里跑命令的 DTO（`env` 重新导出） |
//! | [`env_files`] | 执行环境端口里文件与搜索操作的 DTO |
//! | [`env_path`] | 执行环境路径的字符串规则（按 `PathStyle`，唯一来源） |
//! | [`env_git`] | 执行环境端口里 git 状态操作的 DTO |
//! | [`env_rewind`] | 代码回退的 token 事务 DTO |
//! | [`env_web`] | 执行环境端口里取网页（WebFetch）的 DTO |
//! | [`env_worktree`] | 执行环境端口里 git worktree 操作的 DTO |
//! | [`goal`] | `/goal` 的目标与 `goal_status` 数据 |
//! | [`cron`] | 会话内定时任务：唤醒与到期任务（定时任务 → 会话 actor） |
//! | [`cron_store`] | 持久定时任务的存储端口（任务文件、调度锁、文件变化） |
//! | [`listing`] | 上下文清单（延迟工具、agent、skill、MCP instructions）的来源 |
//! | [`mcp`] | MCP 的传输端口与工具来源 |
//! | [`mcp_state`] | MCP 连接状态的视图与等待（连接中的服务器，F36） |
//! | [`monitor`] | Monitor 交给会话 actor 的登记（F83） |
//! | [`monitor_watch`] | 一个 monitor 的切行、批与限流（纯状态，会话 actor 与 agents 共用，F83） |
//! | [`store`] | 会话存储与 Artifact 端口 |
//! | [`permission`] | 权限模式、规则与判定 |
//! | [`peers`] | 本机跨会话通信的端口、地址、ref 与信封（docs/specs/rust-cli/cross-session.md） |
//! | [`persisted_output`] | 大内容落盘的唯一入口与 `<persisted-output>` 预览（工具结果与 hook 输出共用） |
//! | [`plan`] | 计划文件的位置（ExitPlanMode、计划模式 attachment 与判决链共用） |
//! | [`provider`] | 模型目录与按选择解析的端点（平台层实现） |
//! | [`reminder_fold`] | 提醒并进 user 消息末尾的工具结果（normalize 与 system 消息降级共用） |
//! | [`tool`] | 工具接口 |
//! | [`tool_search`] | 延迟工具的公共规则（哪些延迟、哪些已被 `tool_reference` 引用） |
//! | [`orchestration`] | 能力之间的编排 trait |
//! | [`interaction`] | 交互的回答形状（`orchestration` 重新导出） |
//! | [`compaction`] | 自动压缩的运行状态与结果 |
//! | [`context_usage`] | 上下文用量（`/context`）的查询、结果与数字写法 |
//! | [`history`] | 活动历史（最近一次压缩边界之后，含保留段） |
//! | [`hooks`] | hook 事件、合并结果与 http hook 的出口端口 |
//! | [`session_api`] | 前端使用的会话 API 与内核事件 |
//! | [`clock`] | 时间与 ID 的注入端口 |
//! | [`user_command`] | 用户输入的斜杠命令（skill 的用户调用） |
//! | [`workspace_trust`] | 工作区信任记录的键 |

pub mod abort;
pub mod agents;
pub mod catalog;
pub mod checkpoint;
pub mod clock;
pub mod compaction;
pub mod context_usage;
pub mod cron;
pub mod cron_store;
pub mod env;
pub mod env_command;
pub mod env_files;
pub mod env_git;
pub mod env_path;
pub mod env_pdf;
pub mod env_registry;
pub mod env_rewind;
pub mod env_shell;
pub mod env_web;
pub mod env_worktree;
pub mod goal;
pub mod history;
pub mod hooks;
pub mod ids;
pub mod image;
pub mod interaction;
pub mod listing;
pub mod local_services;
pub mod machine;
pub mod machine_notices;
pub mod machine_param;
pub mod manage;
pub mod mcp;
pub mod mcp_state;
pub mod message;
pub mod metering;
pub mod model;
pub mod monitor;
pub mod monitor_watch;
pub mod orchestration;
pub mod panels;
pub mod peers;
pub mod permission;
pub mod persisted_output;
pub mod plan;
pub mod provider;
pub mod reminder_fold;
pub mod session_api;
pub mod store;
pub mod title;
pub mod tool;
pub mod tool_search;
pub mod unparsed_input;
pub mod user_command;
pub mod user_input;
pub mod workspace_trust;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
