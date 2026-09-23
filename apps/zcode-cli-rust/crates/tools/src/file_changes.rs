use super::{checkpoint_blobs as blobs, tool_files::patch};
use crate::domain::file_checkpoint::FileCheckpoint;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

pub(super) async fn details(root: &Path, changes: &[FileCheckpoint]) -> Result<Value> {
    let mut groups: BTreeMap<&str, Vec<&FileCheckpoint>> = BTreeMap::new();
    for c in changes {
        groups.entry(&c.path).or_default().push(c);
    }
    let mut items = vec![];
    let (mut additions, mut deletions) = (0, 0);
    let mut patch_budget = 512 * 1024;
    for (path, group) in groups {
        let before = match &group[0].before {
            Some(key) => blobs::load(root, key).await?,
            None => vec![],
        };
        let after = blobs::load(root, &group.last().unwrap().after).await?;
        // 只读摘要按原始 checkpoint 聚合，不使用当前磁盘内容推测历史 diff。
        let (mut patches, added, removed) = patch(
            std::str::from_utf8(&before)?.trim_start_matches('\u{feff}'),
            std::str::from_utf8(&after)?.trim_start_matches('\u{feff}'),
        );
        let size = serde_json::to_vec(&patches)?.len();
        if size > patch_budget {
            patches = json!([]);
        } else {
            patch_budget -= size;
        }
        additions += added;
        deletions += removed;
        let mut tools = group.iter().map(|c| c.tool.as_str()).collect::<Vec<_>>();
        tools.sort_unstable();
        tools.dedup();
        items.push(json!({"path":path,"additions":added,"deletions":removed,"writeCount":group.len(),"toolNames":tools,"patches":patches}));
    }
    let result = json!({"files":items.len(),"additions":additions,"deletions":deletions,"state":if !changes.is_empty() && changes.iter().all(|c|c.restored){"reverted"}else{"active"},"items":items});
    ensure!(
        serde_json::to_vec(&result)?.len() <= 900 * 1024,
        "File summary exceeds RPC limit"
    );
    Ok(result)
}
