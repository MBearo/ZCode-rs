//! Reads every session of a Node session database the way a restarted Rust
//! runtime does (model context, rows and snapshot state) and writes one JSON
//! per session, for `scripts/zcode-cli-rust-node-db-compare.mjs` to compare
//! with Node's own readers (spec rust-m11-node-storage §11 scenario 1).
//! Usage: `node_read <db> <artifact-root> <out-dir>`. Not a release entry.
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [db, artifacts, out] = args.as_slice() else {
        anyhow::bail!("Usage: node_read <db> <artifact-root> <out-dir>");
    };
    let (artifacts, out) = (PathBuf::from(artifacts), PathBuf::from(out));
    std::fs::create_dir_all(&out)?;
    let conn = rusqlite::Connection::open(db)?;
    let ids: Vec<String> = conn
        .prepare("select id from session order by time_updated desc, id desc")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let read = |uri: &str| zcode_cli_state::node::artifacts::read(&artifacts, uri);
    let (mut ok, mut failed, mut timings) = (0usize, vec![], vec![]);
    for (index, id) in ids.iter().enumerate() {
        let started = Instant::now();
        let result = (|| -> anyhow::Result<Option<Value>> {
            let active = zcode_cli_state::node::cold::active(&conn, id)?;
            let history: Vec<Value> = zcode_cli_rust::domain::node_history::hydrate(&active, &read)
                .entries
                .iter()
                .map(|e| e.to_node())
                .collect();
            let Some(resumed) = zcode_cli_state::node::resume::resume(&conn, id, &read, None)?
            else {
                return Ok(None);
            };
            Ok(Some(json!({"sessionId": id, "history": history,
                "rows": resumed.conversation.rows, "state": resumed.conversation.state})))
        })();
        timings.push((started.elapsed().as_secs_f64() * 1000.0, id.clone()));
        match result {
            Ok(Some(read)) => {
                std::fs::write(out.join(format!("{index}.json")), read.to_string())?;
                ok += 1;
            }
            Ok(None) => {}
            // 只记录会话 id 与错误类别，不输出会话内容。
            Err(error) => failed.push(json!({"sessionId": id,
                "error": format!("{error:#}").chars().take(200).collect::<String>()})),
        }
    }
    timings.sort_by(|a, b| a.0.total_cmp(&b.0));
    let pick = |q: f64| timings[((timings.len() as f64 - 1.0) * q) as usize].0;
    let slowest: Vec<Value> = timings
        .iter()
        .rev()
        .take(3)
        .map(|(ms, id)| json!({"sessionId": id, "ms": ms}))
        .collect();
    let summary = json!({"sessions": ids.len(), "read": ok, "failed": failed,
        "coldReadMs": {"p50": pick(0.5), "p95": pick(0.95), "max": pick(1.0)},
        "slowest": slowest});
    std::fs::write(out.join("summary.json"), summary.to_string())?;
    println!("{summary}");
    Ok(())
}
