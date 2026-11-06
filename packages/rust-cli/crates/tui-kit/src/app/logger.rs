//! 界面自己的日志去哪（docs/puffin-tui.md §4.5）。一个端口：壳的测试能读到它记了什么，cli 传文件日志。
//! 移植自 packages/tui/tui/src/zcode/tui/app/Logger.scala（feat/tui a2bf878b34）。
use std::sync::{Arc, Mutex};

use rust_cli_base::sync::LockUnpoisoned;

/// 服务层的级别：debug 只在开发时、info 是生命周期事件、warn 是能恢复的失败、error 是不能恢复的。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    pub const fn name(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

pub trait Logger: Send + Sync {
    fn log(&self, level: Level, message: &str);
    /// 今天的行写到哪个文件（有的话）：启动失败时点名它。
    fn file(&self) -> Option<String>;

    fn debug(&self, message: &str) {
        self.log(Level::Debug, message);
    }
    fn info(&self, message: &str) {
        self.log(Level::Info, message);
    }
    fn warn(&self, message: &str) {
        self.log(Level::Warn, message);
    }
    fn error(&self, message: &str) {
        self.log(Level::Error, message);
    }
}

/// 什么都不记。
pub struct NoLogger;

impl Logger for NoLogger {
    fn log(&self, _: Level, _: &str) {}
    fn file(&self) -> Option<String> {
        None
    }
}

/// 记在内存里（测试读它）。
#[derive(Default)]
pub struct MemoryLogger {
    lines: Mutex<Vec<(Level, String)>>,
}

impl MemoryLogger {
    pub fn new() -> Arc<MemoryLogger> {
        Arc::new(MemoryLogger::default())
    }

    pub fn lines(&self) -> Vec<(Level, String)> {
        self.lines.lock_unpoisoned().clone()
    }
}

impl Logger for MemoryLogger {
    fn log(&self, level: Level, message: &str) {
        self.lines
            .lock_unpoisoned()
            .push((level, message.to_string()));
    }
    fn file(&self) -> Option<String> {
        None
    }
}
