//! 时间与 ID 的注入端口（docs/specs/rust-cli/README.md §6 规则 12）。
//!
//! 内核不直接读系统时钟、不直接生成 uuid：测试里注入固定值，云端统一时间来源。ID 格式与 Node
//! 记录一致，本机实现见 node-store（`SystemClock`、`NodeIds`，README §4）。

use crate::ids::{
    AgentId, CommandId, Generation, InteractionId, MessageId, OperationId, RunId, SessionId, TurnId,
};

pub trait Clock: Send + Sync {
    /// Unix 毫秒。
    fn now_ms(&self) -> u64;

    /// IANA 时区名，用于提示词里的日期与按日统计。
    fn time_zone(&self) -> String;

    /// 用户时区里的今天，`YYYY-MM-DD`（提示词里的 `Today's date is …`）。
    ///
    /// 默认实现按 UTC 计算，只在 `time_zone` 是 UTC 时正确；能解析时区的实现应覆盖它。
    fn today(&self) -> String {
        civil_date(self.now_ms())
    }

    /// `time_zone` 在 `at_ms` 这一刻相对 UTC 的偏移（分钟，东正西负）。cron 按它换算用户的本地时间
    /// （docs/specs/rust-cli/tools-more.md §10.3）。默认按 UTC；能解析时区的实现应覆盖它。
    fn utc_offset_minutes(&self, _at_ms: u64) -> i32 {
        0
    }
}

/// Unix 毫秒 → UTC 日期 `YYYY-MM-DD`（Howard Hinnant 的 civil_from_days 算法）。
pub fn civil_date(now_ms: u64) -> String {
    let days = (now_ms / 86_400_000) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

pub trait Ids: Send + Sync {
    fn session(&self) -> SessionId;
    /// 子代理的子会话（Node `sess_subagent_agent_<uuid>`），与 `agent` 共用同一个 uuid。
    fn subagent(&self) -> (AgentId, SessionId);
    fn message(&self) -> MessageId;
    fn turn(&self) -> TurnId;
    fn run(&self) -> RunId;
    fn operation(&self) -> OperationId;
    fn interaction(&self) -> InteractionId;

    /// 前端发出的一条命令（落库为 user 消息的 `anchor.sourceCommandId`；本机实现是不带前缀的 uuid，与以前
    /// TUI 自己生成的相同）。缺省用一个新的操作 ID 的文字（测试替身）。F7 新增：前端不再直接调用 uuid。
    fn command(&self) -> CommandId {
        CommandId::new(self.operation().as_str())
    }

    /// 本地执行环境的实例（随进程变化，spec §6 规则 3；本机实现是不带前缀的 uuid）。缺省同上。F7 新增：
    /// runtime 不再直接调用 uuid。
    fn generation(&self) -> Generation {
        Generation::new(self.operation().as_str())
    }
}
