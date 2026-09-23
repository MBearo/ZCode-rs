use crate::domain::{history::History, session::Session};
use anyhow::Result;
use rusqlite::{Connection, params};
pub(super) struct Write {
    rewrite: bool,
    inputs: Vec<String>,
    responses: Vec<String>,
    input_start: usize,
    response_start: usize,
}
impl Write {
    pub(super) fn new(s: &Session) -> Result<Self> {
        let input_start = if s.history_rewrite {
            0
        } else {
            s.saved_inputs.min(s.history.inputs.len())
        };
        let response_start = if s.history_rewrite {
            0
        } else {
            s.saved_responses
                .saturating_sub(1)
                .min(s.history.responses.len())
        };
        Ok(Self {
            rewrite: s.history_rewrite,
            input_start,
            response_start,
            inputs: s.history.inputs[input_start..]
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<_, _>>()?,
            responses: s.history.responses[response_start..]
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<_, _>>()?,
        })
    }
    pub(super) fn save(self, tx: &Connection, workspace: &str, session: &str) -> Result<()> {
        if self.rewrite {
            tx.execute(
                "DELETE FROM rust_history WHERE workspace=?1 AND session=?2",
                params![workspace, session],
            )?;
        }
        let mut stmt=tx.prepare_cached("INSERT INTO rust_history VALUES(?1,?2,?3,?4,?5) ON CONFLICT(workspace,session,kind,ordinal) DO UPDATE SET body=excluded.body WHERE body IS NOT excluded.body")?;
        for (kind, start, values) in [
            ("input", self.input_start, self.inputs),
            ("response", self.response_start, self.responses),
        ] {
            for (i, value) in values.into_iter().enumerate() {
                stmt.execute(params![workspace, session, kind, start + i, value])?;
            }
        }
        Ok(())
    }
}
pub(super) fn load(conn: &Connection, workspace: &str, session: &str) -> Result<History> {
    let mut history = History::default();
    let mut stmt=conn.prepare_cached("SELECT kind,body FROM rust_history WHERE workspace=?1 AND session=?2 ORDER BY kind,ordinal")?;
    for row in stmt.query_map(params![workspace, session], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        let (kind, body) = row?;
        if kind == "input" {
            history.inputs.push(serde_json::from_str(&body)?);
        } else {
            history.responses.push(serde_json::from_str(&body)?);
        }
    }
    if let Some(b) = history.inputs.last() {
        history.action_rows.push(b.user_row);
    }
    if let Some(b) = history.responses.last() {
        history.action_rows.push(b.row);
    }
    Ok(history)
}
