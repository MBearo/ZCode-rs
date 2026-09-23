use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTask {
    pub id: String,
    pub run_id: String,
    pub title: String,
    pub status: String,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub output_file: String,
}
impl BackgroundTask {
    pub fn projection(&self) -> Value {
        let mut value = json!({"workId":self.id,"kind":"bash","title":self.title,"status":"running","startedAt":self.started_at,"cancellable":true,"anchorRowId":null});
        if self.status != "running" {
            value["status"] = if self.status == "failed" {
                "failed"
            } else {
                "cancelled"
            }
            .into();
            value["cancellable"] = false.into();
            if let Some(at) = self.ended_at {
                value["endedAt"] = at.into();
            }
        }
        value
    }
}
