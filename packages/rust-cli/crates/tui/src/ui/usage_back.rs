//! 失败回合的「回来的时刻」（docs/puffin-tui.md §4.2「用量（/usage）」的「回来的时刻」；Puffin `shell/usage/mod.rs` 的
//! `ask_back`、`usage_back_line`）：套餐的上限停下了回合时，问一次那个 provider 的计划；ZCode 跑着当前模型、第一个计划的
//! 5 小时或每周的池用完时，失败块多一行它什么时候回来（kit `usage::page::comes_back`），从到达的那个 tick 起淡入。
//!
//! 只看刚到的失败（视图最后一行、不是冷加载的）：同 Puffin 的 `fresh_notices(Failure)`。行 id 属于这个视图：换会话时
//! `walls` 清空，晚到的回答按会话丢掉（runner）。现在两个界面的内核都不给「不能再试的限流」，这条路只在内核给出时才走。
use rust_cli_tui_kit::app::widgets::fade;
use rust_cli_tui_kit::app::widgets::failure_block;
use rust_cli_tui_kit::app::widgets::usage::page;
use rust_cli_tui_kit::render::Span;

use super::app::{Action, App};
use crate::view::{Body, Failure};

/// 失败回合的上限说的与到达的 tick（Puffin `Wall`）：`back` 是停住套餐的池与它回来的时刻；没有可说的时为 `None`。
#[derive(Debug, Clone, PartialEq)]
pub struct Wall {
    pub back: Option<(String, f64)>,
    pub arrived: i64,
}

impl App {
    /// 视图刚变了：最后一行是刚到的、被套餐的上限停下的失败时，问一次它什么时候回来（Puffin `ask_back`）。
    pub fn ask_back(&mut self) -> Option<Action> {
        let cold = self.view.cold_until();
        let row = self.view.rows().last().filter(|row| row.id >= cold)?;
        let Body::Failure(f) = &row.body else {
            return None;
        };
        let ui = &mut self.usage_plans;
        if !failure_block::capped(&super::cells::failure::kit_failure(f))
            || ui.walls.contains_key(&row.id)
            || !ui.walls_asking.insert(row.id)
        {
            return None;
        }
        Some(Action::UsageBack {
            row: row.id,
            provider: f.provider.clone(),
        })
    }

    /// 回答到了（Puffin `UsageMsg::Back`）：按状态栏的帧钟记到达；读不到时块不多那一行。
    pub fn usage_back(&mut self, row: u64, back: Option<(String, f64)>) {
        let arrived = self.status_motion.tick();
        let ui = &mut self.usage_plans;
        ui.walls_asking.remove(&row);
        ui.walls.insert(row, Wall { back, arrived });
    }

    /// 失败块的那一行与它的年龄（Puffin `usage_back_line`）；上限没说什么时候回来时为 None。
    pub fn usage_back_line(&self, row: u64, f: &Failure) -> Option<(Vec<Span>, i64)> {
        let w = self.usage_plans.walls.get(&row)?;
        let (pool, at) = w.back.as_ref()?;
        Some((
            page::comes_back(&self.ink.theme, pool, *at, f.at as f64, &self.zone),
            self.status_motion.tick() - w.arrived,
        ))
    }

    /// 那一行在淡入：按动画节拍重画（Puffin `usage_moving` 的 `walls`）。
    pub fn usage_back_moving(&self) -> bool {
        let now = self.status_motion.tick();
        self.usage_plans
            .walls
            .values()
            .any(|w| w.back.is_some() && now - w.arrived < fade::TICKS)
    }
}

#[cfg(test)]
#[path = "usage_back_tests.rs"]
mod tests;
