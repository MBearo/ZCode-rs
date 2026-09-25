//! Node `resource-telemetry.ts` and the process probes (spec
//! rust-m9-usage-logs §6): every 5 minutes the memory and CPU of each stdio
//! MCP server's process tree, one sample per MCP identity, as
//! `process/mcpResourceSamples`. Nothing is sent without tracked processes.
use super::mcp_telemetry::{Tracker, arch, now_ms, platform};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Node `ZCODE_MCP_RESOURCE_SAMPLE_INTERVAL_MS`.
const INTERVAL: Duration = Duration::from_secs(300);
/// Node's probe budget per sample.
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// One row of the process table: `(ppid, rss KiB, cpu ms)`.
type Row = (u32, u64, Option<u64>);

/// `[dd-][hh:]mm:ss[.ff]` (BSD and procps `cputime`) in milliseconds.
fn cputime(text: &str) -> Option<u64> {
    let (days, rest) = match text.split_once('-') {
        Some((days, rest)) => (days.parse::<u64>().ok()?, rest),
        None => (0, text),
    };
    let mut parts: Vec<&str> = rest.split(':').collect();
    let seconds: f64 = parts.pop()?.parse().ok()?;
    let minutes: u64 = parts.pop().map_or(Some(0), |m| m.parse().ok())?;
    let hours: u64 = parts.pop().map_or(Some(0), |h| h.parse().ok())?;
    let whole = ((days * 24 + hours) * 60 + minutes) * 60;
    Some(whole * 1000 + (seconds * 1000.0).round() as u64)
}

/// `ps -eo pid=,ppid=,rss=,cputime=`.
fn parse_ps(text: &str) -> HashMap<u32, Row> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid = fields.next()?.parse().ok()?;
            let rss = fields.next()?.parse().ok()?;
            Some((pid, (ppid, rss, fields.next().and_then(cputime))))
        })
        .collect()
}

/// `tasklist /FO CSV /NH`: `"image","pid","session","#","12,345 K"`.
fn parse_tasklist(text: &str) -> HashMap<u32, Row> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split("\",\"").map(|f| f.trim_matches('"')).collect();
            let pid = fields.get(1)?.parse().ok()?;
            let memory: String = fields
                .get(4)?
                .chars()
                .filter(char::is_ascii_digit)
                .collect();
            Some((pid, (0, memory.parse().ok()?, None)))
        })
        .collect()
}

async fn probe() -> Option<HashMap<u32, Row>> {
    let (program, args, windows): (&str, &[&str], bool) = if cfg!(windows) {
        ("tasklist", &["/FO", "CSV", "/NH"], true)
    } else {
        ("ps", &["-eo", "pid=,ppid=,rss=,cputime="], false)
    };
    let output = tokio::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(PROBE_TIMEOUT, output)
        .await
        .ok()?
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    Some(if windows {
        parse_tasklist(&text)
    } else {
        parse_ps(&text)
    })
}

/// The pids of `root`'s tree (Windows lists no parents: only the root).
fn tree(root: u32, table: &HashMap<u32, Row>) -> Vec<u32> {
    let mut out = vec![];
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        if !table.contains_key(&pid) || out.contains(&pid) {
            continue;
        }
        out.push(pid);
        stack.extend(
            table
                .iter()
                .filter(|(child, (ppid, _, _))| *ppid == pid && **child != pid)
                .map(|(child, _)| *child),
        );
    }
    out
}

fn total_memory_gb() -> u64 {
    let bytes = if cfg!(target_os = "linux") {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|m| {
                let line = m.lines().find(|l| l.starts_with("MemTotal:"))?.to_owned();
                line.split_whitespace().nth(1)?.parse::<u64>().ok()
            })
            .map_or(0, |kib| kib * 1024)
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
            .unwrap_or(0)
    } else {
        0
    };
    (bytes as f64 / 1024f64.powi(3)).round() as u64
}

/// Starts the sampler; it stops with `stop`.
pub(super) fn start(tracker: Arc<Tracker>, stop: CancellationToken) {
    let token = uuid::Uuid::new_v4().to_string();
    tokio::spawn(async move {
        let hardware = (
            std::thread::available_parallelism().map_or(1, |n| n.get()),
            tokio::task::spawn_blocking(total_memory_gb)
                .await
                .unwrap_or(0),
        );
        let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + INTERVAL, INTERVAL);
        let mut baseline: HashMap<(String, u32), u64> = HashMap::new();
        let mut previous: Option<u64> = None;
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                _ = ticks.tick() => {}
            }
            let processes = tracker.tracked();
            let table = if processes.is_empty() {
                None
            } else {
                probe().await
            };
            let Some(table) = table else {
                baseline.clear();
                previous = None;
                continue;
            };
            let sampled_at = now_ms();
            let interval = previous.map_or(INTERVAL.as_millis() as u64, |p| {
                sampled_at.saturating_sub(p).max(1)
            });
            previous = Some(sampled_at);
            let mut groups: BTreeMap<String, Value> = BTreeMap::new();
            let mut seen = HashSet::new();
            let mut next = HashMap::new();
            for process in processes {
                let pids: Vec<u32> = tree(process.pid, &table)
                    .into_iter()
                    .filter(|p| seen.insert(*p))
                    .collect();
                if pids.is_empty() {
                    continue;
                }
                let group = groups.entry(process.mcp_id.clone()).or_insert_with(|| {
                    json!({"mcpId": process.mcp_id, "instanceToken": token, "sampledAt": sampled_at,
                        "intervalMs": interval, "processCount": 0, "rssKbTotal": 0,
                        "rssKbMaxProcess": 0, "cpuTimeMsDelta": 0, "uptimeMinutes": 0,
                        "platform": platform(), "arch": arch(), "logicalCpuCount": hardware.0,
                        "totalMemoryGb": hardware.1})
                });
                let add = |group: &mut Value, key: &str, value: u64| {
                    group[key] = (group[key].as_u64().unwrap_or(0) + value).into();
                };
                for pid in pids {
                    let (_, rss, cpu) = table[&pid];
                    add(group, "processCount", 1);
                    add(group, "rssKbTotal", rss);
                    group["rssKbMaxProcess"] = group["rssKbMaxProcess"]
                        .as_u64()
                        .unwrap_or(0)
                        .max(rss)
                        .into();
                    if let Some(cpu) = cpu {
                        let key = (process.instance.clone(), pid);
                        if let Some(before) = baseline.get(&key) {
                            add(group, "cpuTimeMsDelta", cpu.saturating_sub(*before));
                        }
                        next.insert(key, cpu);
                    }
                }
                let uptime = sampled_at.saturating_sub(process.started_at) / 60_000;
                group["uptimeMinutes"] = group["uptimeMinutes"]
                    .as_u64()
                    .unwrap_or(0)
                    .max(uptime)
                    .into();
            }
            baseline = next;
            tracker.sample(groups.into_values().collect());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_tables_parse_like_node() {
        assert_eq!(cputime("01:02.50"), Some(62_500));
        assert_eq!(cputime("1-02:03:04"), Some(((26 * 60 + 3) * 60 + 4) * 1000));
        let table = parse_ps(
            "  10     1  2048  0:01.00\n  11    10  1024  0:00.50\n  12     1   512  0:00.00\n",
        );
        assert_eq!(table[&11], (10, 1024, Some(500)));
        let mut pids = tree(10, &table);
        pids.sort();
        assert_eq!(pids, vec![10, 11]);
        let windows = parse_tasklist("\"node.exe\",\"4242\",\"Console\",\"1\",\"12,345 K\"\n");
        assert_eq!(windows[&4242], (0, 12345, None));
    }
}
