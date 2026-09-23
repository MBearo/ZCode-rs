//! Claude Code history import (Node `persistImportedSessionHistory`): imported
//! messages replace earlier imports of the same session; later chat is kept.
use super::session::Session;
use serde_json::{Value, json};

/// Marker on imported model messages (Node `metadata.migrationSource`).
const MARKER: &str = "_zcode_migration_source";
const SOURCE: &str = "claudeCode";

fn js_trim(s: &str) -> &str {
    super::zod::js_trim(s)
}

/// Import start time: `createdAt ?? messages[0].timestamp ?? now`.
pub fn created_at(history: &Value, now: u64) -> u64 {
    history["createdAt"]
        .as_u64()
        .or_else(|| history["messages"][0]["timestamp"].as_u64())
        .unwrap_or(now)
}

/// Rewrites `session` with `history` (a parsed `claudeCode` import). The caller
/// sets identity, mode and timestamps; the model binding is cleared as in Node.
pub fn apply(session: &mut Session, history: &Value, now: u64) {
    let prefix = format!("msg_{}_import_", session.id);
    let imported_turn = |row: &Value| {
        row["turnId"]
            .as_str()
            .is_some_and(|t| t.starts_with(&prefix))
    };
    let kept_rows: Vec<Value> = std::mem::take(&mut session.rows)
        .into_iter()
        .filter(|r| !imported_turn(r))
        .collect();
    let kept_messages: Vec<Value> = std::mem::take(&mut session.messages)
        .into_iter()
        .filter(|m| m[MARKER] != SOURCE)
        .collect();
    let start = created_at(history, now);
    let mut previous: Option<u64> = None;
    let mut turn: Option<(String, usize)> = None;
    let mode = session.mode.as_str();
    for (index, message) in history["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        // 时间强制严格递增：缺省取 createdAt + i，不大于前一条时取前一条 + 1。
        let mut ts = message["timestamp"]
            .as_u64()
            .unwrap_or(start + index as u64);
        if let Some(prev) = previous
            && ts <= prev
        {
            ts = prev + 1;
        }
        previous = Some(ts);
        let id = format!("{prefix}{index}");
        let content = message["content"].as_str().unwrap_or("");
        if message["role"] == "user" {
            let mut header = session.row("turnHeader", &id, &id, ts);
            header["origin"] = "userInput".into();
            header["state"] = "completedSuccess".into();
            header["startedAt"] = ts.into();
            header["endedAt"] = ts.into();
            session.rows.push(header);
            turn = Some((id.clone(), session.rows.len() - 1));
            let mut row = session.row("userInput", &id, &id, ts);
            row["text"] = content.into();
            row["origin"] = "realUser".into();
            session.rows.push(row);
            session
                .messages
                .push(json!({"role":"user","content":content,MARKER:SOURCE}));
        } else {
            let (turn_id, header) = turn.clone().unwrap_or_else(|| {
                // 没有前置 user 消息时 Node 以占位父 id 挂载这条 assistant 消息。
                let parent = format!("msg_{}_import_parent_{index}", session.id);
                let mut header = session.row("turnHeader", &parent, &parent, ts);
                header["origin"] = "userInput".into();
                header["state"] = "completedSuccess".into();
                header["startedAt"] = ts.into();
                session.rows.push(header);
                (parent, session.rows.len() - 1)
            });
            turn = Some((turn_id.clone(), header));
            session.rows[header]["endedAt"] = ts.into();
            let mut row = session.row("assistantText", &turn_id, &id, ts);
            row["text"] = content.into();
            row["assistantResponseId"] = id.into();
            row["state"] = "complete".into();
            session.rows.push(row);
            session
                .messages
                .push(json!({"role":"assistant","content":content,MARKER:SOURCE}));
            session.last_assistant_mode = Some(mode.into());
        }
    }
    for mut row in kept_rows {
        let fresh = session.row("placeholder", "", "", 0);
        row["rowId"] = fresh["rowId"].clone();
        session.rows.push(row);
    }
    session.messages.extend(kept_messages);
    // 历史整体重写：客户端按新 epoch 重新同步，模型上下文从头计算。
    session.history_rewrite = true;
    session.context = Default::default();
    session.context_tokens = None;
    let title = js_trim(history["title"].as_str().unwrap_or(""));
    session.title = if title.is_empty() {
        "Imported session".into()
    } else {
        title.into()
    };
    session.title_source = "custom".into();
    session.phase = super::execution::Phase::CompletedSuccess;
    session.last_error = None;
    session.provider.clear();
    session.model.clear();
    session.reasoning_level.clear();
    session.thought_levels.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session::new(
            "s".into(),
            "w".into(),
            "p".into(),
            "m".into(),
            "low".into(),
            "e".into(),
            5,
        )
    }

    #[test]
    fn timestamps_increase_and_reimport_keeps_later_chat() {
        let mut s = session();
        let history = json!({"title":" T ","messages":[
            {"role":"user","content":"hi","timestamp":10},
            {"role":"assistant","content":"hello","timestamp":9},
            {"role":"user","content":"again"}]});
        apply(&mut s, &history, 100);
        let times: Vec<u64> = s
            .rows
            .iter()
            .map(|r| r["createdAt"].as_u64().unwrap())
            .collect();
        assert_eq!(times, [10, 10, 11, 12, 12]);
        assert_eq!(s.rows[0]["endedAt"], 11);
        assert_eq!(s.title, "T");
        assert!(s.provider.is_empty() && s.messages.len() == 3);
        s.rows
            .push(json!({"rowId":99,"turnId":"later","kind":"userInput","text":"mine"}));
        s.messages.push(json!({"role":"user","content":"mine"}));
        apply(
            &mut s,
            &json!({"messages":[{"role":"user","content":"only"}]}),
            200,
        );
        assert_eq!(s.messages.len(), 2);
        assert_eq!(s.messages[1]["content"], "mine");
        assert_eq!(s.rows.last().unwrap()["text"], "mine");
        assert_eq!(s.rows[0]["turnId"], "msg_s_import_0");
        assert_eq!(s.title, "Imported session");
    }
}
