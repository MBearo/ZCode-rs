//! Subagent rows and the `subagents` summary (Node
//! `ProductProjection.onSubagentSpawned`, `onSubagentStopped` and
//! `materializeSubagentProjection`).
use super::events::Event;
use super::facts::{text, truthy};
use super::projection::{Delta, Projection, row_id};
use crate::js_json::stringify;
use serde_json::{Map, Value, json};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

/// Node `mapSubagentStatus`.
fn row_status(status: Option<&str>) -> &'static str {
    match status {
        Some("completed" | "success") => "success",
        Some("cancelled" | "stopped") => "cancelled",
        _ => "failed",
    }
}

fn member(payload: &Value, key: &str) -> Option<Value> {
    text(&payload[key]).map(Value::from)
}

/// Node `latestRowByChildId`: `Map.set` replaces a known child in place and
/// appends a new one.
#[derive(Default)]
struct LatestByChild<'a> {
    rows: Vec<(String, Cow<'a, Value>)>,
    positions: HashMap<String, usize>,
}

impl<'a> LatestByChild<'a> {
    fn collect(&mut self, row: Option<Cow<'a, Value>>) {
        let Some(row) = row else { return };
        let Some(child) = text(&row["childSessionId"]).map(str::to_owned) else {
            return;
        };
        match self.positions.get(&child) {
            Some(&index) => self.rows[index].1 = row,
            None => {
                self.positions.insert(child.clone(), self.rows.len());
                self.rows.push((child, row));
            }
        }
    }
}

impl Projection {
    /// Node `subagentAgentId`.
    fn agent_id(payload: &Value, event: &Event) -> String {
        ["agentId", "childSessionId", "parentToolCallId"]
            .iter()
            .find_map(|key| text(&payload[*key]))
            .map(str::to_owned)
            .unwrap_or_else(|| format!("subagent-{}", event.seq))
    }

    /// Node `findSubagentLifecycleRow`: the agent's row, else the row its
    /// parent tool call created in this turn.
    fn lifecycle_row(&self, agent: &str, payload: &Value, event: &Event) -> Option<Value> {
        let exact = self
            .subagent_rows
            .get(agent)
            .and_then(|id| self.find_row(*id));
        if let Some(row) = exact.filter(|row| row["kind"] == "subagent") {
            return Some(row.clone());
        }
        let parent = text(&payload["parentToolCallId"])?;
        let turn = self.turn_of(event);
        // Node 扫描整个 rows 窗口；这里只按行序扫描 subagent 行（结果相同），长会话冷回放
        // 每个 lifecycle 事件不再遍历数万行。
        self.subagent_rows_in_order()
            .find(|row| {
                row["kind"] == "subagent"
                    && row["turnId"] == turn.as_str()
                    && row["parentToolCallId"] == parent
            })
            .cloned()
    }

    /// Node `onSubagentSpawned` (the cold synthesis never resumes a background child).
    pub fn on_subagent_spawned(&mut self, event: &Event) -> Vec<Delta> {
        let payload = &event.payload;
        let agent = Self::agent_id(payload, event);
        let existing = self.lifecycle_row(&agent, payload, event);
        if let Some(row) = &existing {
            self.subagent_rows.insert(agent.clone(), row_id(row));
        }
        let child = member(payload, "childSessionId");
        let parent = member(payload, "parentToolCallId");
        let background = payload["background"] == true;
        if let Some(existing) = &existing
            && existing["status"] == "running"
            && child
                .as_ref()
                .is_none_or(|c| *c == existing["childSessionId"])
            && parent
                .as_ref()
                .is_none_or(|p| *p == existing["parentToolCallId"])
            && (!background || existing["backgrounded"] == true)
        {
            return Vec::new();
        }
        if let Some(mut row) = existing {
            row["status"] = "running".into();
            row["summaryText"] = member(payload, "description")
                .or_else(|| member(payload, "prompt"))
                .unwrap_or_else(|| row["summaryText"].clone());
            if let Some(parent) = parent.filter(|_| !truthy(&row["parentToolCallId"])) {
                row["parentToolCallId"] = parent;
            }
            if let Some(child) = child {
                row["childSessionId"] = child;
            }
            if background {
                row["backgrounded"] = true.into();
                row["workId"] = agent.into();
            }
            row["startedAt"] = event.at.into();
            row.as_object_mut()
                .expect("rows are objects")
                .shift_remove("endedAt");
            return vec![Delta::Upsert(row)];
        }
        let turn = self.turn_of(event);
        let mut row = self.row_base(event, &turn, &agent);
        row["kind"] = "subagent".into();
        if let Some(parent) = parent {
            row["parentToolCallId"] = parent;
        }
        row["subagentType"] = member(payload, "agentType").unwrap_or_else(|| "subagent".into());
        row["status"] = "running".into();
        row["summaryText"] = ["description", "summaryText", "prompt"]
            .iter()
            .find_map(|key| member(payload, key))
            .unwrap_or_else(|| "".into());
        if let Some(child) = child {
            row["childSessionId"] = child;
        }
        if background {
            row["backgrounded"] = true.into();
            row["workId"] = agent.clone().into();
        }
        row["startedAt"] = event.at.into();
        self.subagent_rows.insert(agent, row_id(&row));
        vec![Delta::Append(row)]
    }

    /// Node `onSubagentStopped`.
    pub fn on_subagent_stopped(&mut self, event: &Event) -> Vec<Delta> {
        let payload = &event.payload;
        let agent = Self::agent_id(payload, event);
        let existing = self.lifecycle_row(&agent, payload, event);
        let parent = member(payload, "parentToolCallId");
        let status = row_status(payload["status"].as_str().filter(|s| !s.is_empty()));
        let summary = ["summaryText", "result", "error", "description"]
            .iter()
            .find_map(|key| member(payload, key))
            .or_else(|| existing.as_ref().map(|row| row["summaryText"].clone()))
            .unwrap_or_else(|| "".into());
        let child = member(payload, "childSessionId");
        let (row, appended) = match existing {
            Some(mut row) => {
                row["status"] = status.into();
                row["summaryText"] = summary;
                row["endedAt"] = event.at.into();
                if let Some(parent) = parent.filter(|_| !truthy(&row["parentToolCallId"])) {
                    row["parentToolCallId"] = parent;
                }
                if let Some(child) = child {
                    row["childSessionId"] = child;
                }
                (row, false)
            }
            None => {
                let turn = self.turn_of(event);
                let mut row = self.row_base(event, &turn, &agent);
                row["kind"] = "subagent".into();
                if let Some(parent) = parent {
                    row["parentToolCallId"] = parent;
                }
                row["subagentType"] =
                    member(payload, "agentType").unwrap_or_else(|| "subagent".into());
                row["status"] = status.into();
                row["summaryText"] = summary;
                if let Some(child) = child {
                    row["childSessionId"] = child;
                }
                if payload["background"] == true {
                    row["backgrounded"] = true.into();
                    row["workId"] = agent.clone().into();
                }
                row["endedAt"] = event.at.into();
                (row, true)
            }
        };
        self.subagent_rows.insert(agent, row_id(&row));
        vec![if appended {
            Delta::Append(row)
        } else {
            Delta::Upsert(row)
        }]
    }

    /// Node `shouldMaterializeSubagentProjection` in a batch replay.
    pub fn subagents_touched(&self, deltas: &[Delta]) -> bool {
        deltas.iter().any(|delta| match delta {
            Delta::Append(row) | Delta::Upsert(row) => {
                row["kind"] == "subagent"
                    || self
                        .find_row(row_id(row))
                        .is_some_and(|r| r["kind"] == "subagent")
            }
            Delta::Text { row, .. } => self.find_row(*row).is_some_and(|r| r["kind"] == "subagent"),
            Delta::State(patch) => {
                patch.contains_key("pendingInteractions") || patch.contains_key("backgroundWorks")
            }
        })
    }

    /// Node `prospectiveSubagentRow`: `row` after the deltas from `start`.
    /// 只在文本增量改写摘要时复制行；否则借用快照或增量里的行，避免每次物化复制全部子代理行。
    fn prospective<'a>(
        row: &'a Value,
        deltas: &'a [Delta],
        start: usize,
    ) -> Option<Cow<'a, Value>> {
        let mut current = Cow::Borrowed(row);
        for delta in &deltas[start.min(deltas.len())..] {
            match delta {
                Delta::Upsert(next) if row_id(next) == row_id(&current) => {
                    current = Cow::Borrowed(next)
                }
                Delta::Text { row, path, append }
                    if *row == row_id(&current)
                        && *path == "summaryText"
                        && current["kind"] == "subagent" =>
                {
                    let joined =
                        format!("{}{append}", current["summaryText"].as_str().unwrap_or(""));
                    current.to_mut()["summaryText"] = joined.into();
                }
                _ => {}
            }
        }
        (current["kind"] == "subagent").then_some(current)
    }

    /// Node `materializeSubagentProjection`: child sessions in timeline order,
    /// the running ones summarized.
    pub fn materialize_subagents(&self, deltas: &[Delta]) -> Vec<Delta> {
        let previous = self.state["subagents"].clone();
        let mut ids: HashSet<u64> = self.subagent_rows.values().copied().collect();
        for delta in deltas {
            match delta {
                Delta::Upsert(row)
                    if row["kind"] == "subagent"
                        || self
                            .find_row(row_id(row))
                            .is_some_and(|r| r["kind"] == "subagent") =>
                {
                    ids.insert(row_id(row));
                }
                Delta::Text { row, .. }
                    if self.find_row(*row).is_some_and(|r| r["kind"] == "subagent") =>
                {
                    ids.insert(*row);
                }
                _ => {}
            }
        }
        let mut current: Vec<(usize, &Value)> = ids
            .iter()
            .filter_map(|id| Some((self.row_index(*id)?, self.find_row(*id)?)))
            .collect();
        current.sort_by_key(|(index, _)| *index);
        let mut latest = LatestByChild::default();
        for (_, row) in &current {
            latest.collect(Self::prospective(row, deltas, 0));
        }
        for (index, delta) in deltas.iter().enumerate() {
            if let Delta::Append(row) = delta {
                latest.collect(Self::prospective(row, deltas, index + 1));
            }
        }
        let latest = latest.rows;
        let running = self.running_subagents(&latest, &previous);
        let children: Vec<Value> = latest.iter().map(|(id, _)| id.clone().into()).collect();
        let ended = children.len() - running.len();
        let semantic =
            json!({"childSessionIds": children, "running": running, "endedTotal": ended});
        let before = json!({"childSessionIds": previous["childSessionIds"],
            "running": previous["running"], "endedTotal": previous["endedTotal"]});
        if stringify(&semantic) == stringify(&before) {
            return Vec::new();
        }
        let revision = previous["revision"].as_u64().unwrap_or(0) + 1;
        let mut subagents = json!({ "revision": revision });
        if let (Some(target), Value::Object(fields)) = (subagents.as_object_mut(), semantic) {
            target.extend(fields);
        }
        let mut patch = Map::new();
        patch.insert("subagents".into(), subagents);
        vec![Delta::State(patch)]
    }

    fn running_subagents(&self, latest: &[(String, Cow<Value>)], previous: &Value) -> Vec<Value> {
        let waiting: HashSet<String> = self.state["pendingInteractions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| i["payload"]["origin"]["kind"] == "subagent")
            .filter_map(|i| {
                i["payload"]["origin"]["childSessionId"]
                    .as_str()
                    .map(str::to_owned)
            })
            .collect();
        let blocked: HashSet<String> = self.state["backgroundWorks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|w| w["kind"] == "subagent" && w["status"] == "running" && w["blocked"] == true)
            .filter_map(|w| text(&w["childSessionId"]).map(str::to_owned))
            .collect();
        let mut running: Vec<Value> = latest
            .iter()
            .filter(|(_, row)| row["status"] == "running")
            .map(|(child, row)| {
                let previous_title = previous["running"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|item| item["childSessionId"] == child.as_str())
                    .and_then(|item| text(&item["title"]).map(str::to_owned));
                let summary = row["summaryText"].as_str().unwrap_or("").trim().to_owned();
                let title = previous_title
                    .or((!summary.is_empty()).then_some(summary))
                    .map(Value::from)
                    .unwrap_or_else(|| row["subagentType"].clone());
                let mut item = json!({"childSessionId": child, "agentId": row["entityId"]});
                if super::facts::truthy(&row["parentToolCallId"]) {
                    item["toolCallId"] = row["parentToolCallId"].clone();
                }
                item["subagentType"] = row["subagentType"].clone();
                item["title"] = title;
                item["status"] = if waiting.contains(child) {
                    "waiting"
                } else if blocked.contains(child) {
                    "blocked"
                } else {
                    "running"
                }
                .into();
                if row.get("startedAt").is_some() {
                    item["startedAt"] = row["startedAt"].clone();
                }
                item
            })
            .collect();
        running.sort_by(|left, right| {
            let started = |v: &Value| v["startedAt"].as_f64().unwrap_or(0.0);
            started(right)
                .partial_cmp(&started(left))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    let id = |v: &Value| v["childSessionId"].as_str().unwrap_or("").to_owned();
                    id(right).cmp(&id(left))
                })
        });
        running
    }
}
