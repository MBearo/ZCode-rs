//! 定时回合的哨兵展开（docs/specs/rust-cli/tools-more.md §17.3）：哨兵换成说明，不是哨兵时用原文。
//!
//! `/loop` 的自主默认与 loop.md 模式把哨兵当 prompt 排进任务表；触发时在这里换成说明：第一次（或压缩之后、
//! loop.md 改了）发全文，之后发短提醒。状态只在 actor 里，不持久化；主线程压缩边界提交后清空（压缩后的
//! 清理）。

use rust_cli_kernel_api::env::{EnvPath, ReadFile};
use rust_cli_prompts::loops::{self, LoopFile};
use rust_cli_prompts::texts::render;

use crate::actor::Actor;

/// 自主说明的两个字段，与 noop 折叠的锚点（tools-more.md §18.3）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct LoopTicks {
    autonomous_delivered: bool,
    /// 上次发出的 loop.md 内容，或 [`loops::PREAMBLE_DELIVERED`]。
    last_file: Option<String>,
    /// noop 折叠的锚点与连续次数（压缩后与上面两项一起清空）。
    pub(crate) fold: crate::loop_fold::FoldState,
}

fn with_preamble(tick: &str) -> String {
    format!("{}\n\n---\n\n{tick}", loops::preamble())
}

impl LoopTicks {
    /// 自主默认的哨兵。
    fn autonomous(&mut self, dynamic: bool) -> String {
        let tick = if dynamic {
            loops::tick_dynamic()
        } else {
            loops::tick_fixed()
        };
        if self.autonomous_delivered || self.last_file.is_some() {
            return tick.to_owned();
        }
        self.autonomous_delivered = true;
        with_preamble(tick)
    }

    /// loop.md 的哨兵。
    fn file(&mut self, dynamic: bool, file: Option<LoopFile>) -> String {
        if let Some(file) = file {
            let tick = if dynamic {
                loops::file_tick_dynamic()
            } else {
                loops::file_tick_fixed()
            };
            if self.last_file.as_deref() == Some(file.content.as_str()) {
                return tick.to_owned();
            }
            let text = render(
                loops::file_delivery(),
                &[
                    ("path", &file.path),
                    ("content", &file.content),
                    ("tick", tick),
                ],
            );
            self.last_file = Some(file.content);
            return text;
        }
        let tick = if dynamic {
            loops::file_absent_dynamic()
        } else {
            loops::tick_fixed()
        };
        if self.last_file.as_deref() == Some(loops::PREAMBLE_DELIVERED) || self.autonomous_delivered
        {
            return tick.to_owned();
        }
        self.last_file = Some(loops::PREAMBLE_DELIVERED.to_owned());
        self.autonomous_delivered = true;
        with_preamble(tick)
    }
}

impl Actor {
    /// 定时回合的 prompt：哨兵展开成说明，其余原样。
    pub(crate) async fn resolve_loop_prompt(&mut self, prompt: String) -> String {
        match prompt.as_str() {
            loops::AUTONOMOUS => self.loop_ticks.autonomous(false),
            loops::AUTONOMOUS_DYNAMIC => self.loop_ticks.autonomous(true),
            loops::FILE | loops::FILE_DYNAMIC => {
                let file = self.read_loop_file().await;
                self.loop_ticks.file(prompt == loops::FILE_DYNAMIC, file)
            }
            _ => prompt,
        }
    }

    /// 读 loop.md：先工作区根下的 `.zcode/loop.md`，再 `~/.zcode/loop.md`（tools-more.md §17.1）。
    async fn read_loop_file(&self) -> Option<LoopFile> {
        let workspace = &self.meta.workspace;
        // home 与路径拼法都取执行环境的路径事实（F58：与规则文件、子代理、skill 同一个 `PathStyle::join`）。
        let facts = self.deps.machine.info().path_facts(workspace).await.ok()?;
        let paths = loops::loop_file_paths(
            workspace.root.as_str(),
            facts.home.as_ref().map(EnvPath::as_str),
            |base, parts| facts.style.join(base, parts),
        );
        for path in paths {
            let request = ReadFile {
                path: EnvPath::new(path.clone()),
            };
            if let Ok(content) = self
                .deps
                .machine
                .files()
                .read_file(workspace, request)
                .await
                && let Some(file) =
                    loops::loop_file(&path, &String::from_utf8_lossy(&content.bytes))
            {
                return Some(file);
            }
        }
        None
    }

    /// 主线程压缩之后，下一次触发重新发全文。
    pub(crate) fn reset_loop_ticks(&mut self) {
        self.loop_ticks = LoopTicks::default();
    }
}

#[cfg(test)]
#[path = "loop_tick_tests.rs"]
mod tests;
