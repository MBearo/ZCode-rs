//! `<system-reminder>` 包装：唯一定义在 base 的 `reminder`（F58：model 与 node-store 不依赖 prompts，定义移到
//! 那里），这里重新导出，原来的调用方不变。

pub use rust_cli_base::reminder::{is_wrapped, unwrap, wrap};
