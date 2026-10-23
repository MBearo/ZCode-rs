// 复制自 apps/zcode-cli-rust/crates/domain/src/node_ids.rs 的测试（feat/rs 79b6691c55）。
use std::sync::Mutex;

use super::*;

#[test]
fn ids_follow_node_formats() {
    assert_eq!(message_id(1_790_219_323_612, "u"), "msg_mueyd2os_u");
    assert_eq!(part_id(35, "u"), "part_z_u");
    assert_eq!(turn_id("u"), "turn_u");
    assert_eq!(
        message_time("msg_mueycw12_c9269e63-088e-46d9-aa6f-dadc1eebd2e1"),
        Some(1_790_219_314_982)
    );
    assert_eq!(message_time("msg_1"), None);
    assert_eq!(message_time("u1"), None);
}

#[test]
fn slugs_and_project_ids_follow_node() {
    assert_eq!(project_id("/Users/a/My Repo/"), "proj_users-a-my-repo");
    assert_eq!(project_id(""), "proj_session");
    assert_eq!(slugify("sess_ABC-1"), "sess_abc-1");
    assert_eq!(slugify("C:\\Work\\Ünï"), "c-work-n");
    let long = format!("/{}", "a".repeat(100));
    assert_eq!(project_id(&long), format!("proj_{}", "a".repeat(80)));
    // 80 个字符截断发生在去除首尾短横线之后，结果可以以短横线结尾。
    let dashy = format!("{}/b", "a".repeat(79));
    assert_eq!(project_id(&dashy), format!("proj_{}-", "a".repeat(79)));
}

struct FixedClock;

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        1_760_000_000_000
    }

    fn time_zone(&self) -> String {
        "UTC".to_owned()
    }
}

/// 从 runtime 移来（F7）：内核的 ID 用 Node 的格式，消息 ID 的毫秒取自注入的时钟。
#[test]
fn kernel_ids_use_the_node_formats() {
    let ids = NodeIds::new(Arc::new(FixedClock));
    let message = ids.message();
    assert!(message.as_str().starts_with("msg_mgj6k3cw_"), "{message}");
    assert_eq!(message.created_ms(), Some(1_760_000_000_000));
    assert!(ids.session().as_str().starts_with("sess_"));
    let (agent, child) = ids.subagent();
    let uuid = agent.as_str().strip_prefix("agent_").unwrap();
    assert_eq!(child.as_str(), format!("sess_subagent_agent_{uuid}"));
    assert!(ids.turn().as_str().starts_with("turn_"));
    // 命令与执行环境实例是不带前缀的 uuid（以前 TUI 与 runtime 直接用 `Uuid::new_v4` 的格式）。
    for id in [ids.command().to_string(), ids.generation().to_string()] {
        assert_eq!(id.len(), 36, "{id}");
        assert_eq!(id.matches('-').count(), 4, "{id}");
    }
}

/// 顺序 uuid（测试替身）。
struct Sequence(Mutex<u32>);

impl UuidSource for Sequence {
    fn uuid(&self) -> String {
        let mut next = self.0.lock().unwrap();
        *next += 1;
        format!("u{next}")
    }
}

/// F7：内核的 ID 与记录的 ID 只有一个随机源与一个时钟（以前 runtime 的 `NodeIds` 与本 crate 的 `RecordSource`
/// 各自取 uuid 与系统时间）。
#[test]
fn every_id_comes_from_the_one_injected_source() {
    let ids = NodeIds::with_uuids(Arc::new(FixedClock), Arc::new(Sequence(Mutex::new(0))));
    assert_eq!(ids.session().as_str(), "sess_u1");
    assert_eq!(ids.message().as_str(), "msg_mgj6k3cw_u2");
    assert_eq!(ids.turn().as_str(), "turn_u3");
    assert_eq!(ids.command().as_str(), "u4");
    assert_eq!(ids.generation().as_str(), "u5");
    assert_eq!(part_id(ids.now_ms(), &ids.uuid()), "part_mgj6k3cw_u6");
}
