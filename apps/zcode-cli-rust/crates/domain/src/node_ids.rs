//! Node id and slug formats of the shared session database (Node
//! `contracts/src/interfaces/shared.ts`, `core/src/runtime/helpers/project.ts`).
//! Spec rust-m11-node-storage §2.3.

/// `Date.now().toString(36)`.
pub fn base36(mut value: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".into();
    }
    let mut out = vec![];
    while value > 0 {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Node `createSortableIdSegment`: `<ms in base 36>_<uuid>`.
fn sortable(now_ms: u64, uuid: &str) -> String {
    format!("{}_{uuid}", base36(now_ms))
}

/// Node `createMessageId`.
pub fn message_id(now_ms: u64, uuid: &str) -> String {
    format!("msg_{}", sortable(now_ms, uuid))
}

/// Node `createPartId`.
pub fn part_id(now_ms: u64, uuid: &str) -> String {
    format!("part_{}", sortable(now_ms, uuid))
}

/// Node `createSessionId`.
pub fn session_id(uuid: &str) -> String {
    format!("sess_{uuid}")
}

/// Node `createTurnId`.
pub fn turn_id(uuid: &str) -> String {
    format!("turn_{uuid}")
}

/// Node `createStorageTargetId` (`session_target.target_id`).
pub fn target_id(now_ms: u64, uuid: &str) -> String {
    format!("target_{}", sortable(now_ms, uuid))
}

/// Node `createStorageInputHistoryId`.
pub fn input_history_id(now_ms: u64, uuid: &str) -> String {
    format!("input_{}", sortable(now_ms, uuid))
}

/// The `[^a-z0-9._-]+` → `-` replacement with the edge dashes trimmed.
fn dashed(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut dash = false;
    for c in value.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches('-').to_owned()
}

/// Node core `slugify`: empty results become `session`.
pub fn slugify(value: &str) -> String {
    let slug = dashed(value);
    if slug.is_empty() {
        "session".into()
    } else {
        slug
    }
}

/// Node core `projectIdFromDirectory`: `proj_` plus the first 80 slug chars.
/// The slug is ASCII, so byte slicing equals JS `slice`.
pub fn project_id(directory: &str) -> String {
    let slug = slugify(directory);
    format!("proj_{}", &slug[..slug.len().min(80)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_follow_node_formats() {
        assert_eq!(base36(0), "0");
        assert_eq!(base36(1_790_219_323_612), "mueyd2os");
        assert_eq!(
            message_id(1_790_219_314_982, "c9269e63-088e-46d9-aa6f-dadc1eebd2e1"),
            "msg_mueycw12_c9269e63-088e-46d9-aa6f-dadc1eebd2e1"
        );
        assert_eq!(part_id(35, "u"), "part_z_u");
        assert_eq!(session_id("u"), "sess_u");
        assert_eq!(turn_id("u"), "turn_u");
        assert_eq!(target_id(36, "u"), "target_10_u");
        assert_eq!(input_history_id(36, "u"), "input_10_u");
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
}
