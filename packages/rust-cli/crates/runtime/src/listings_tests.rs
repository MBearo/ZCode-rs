//! 组合来源把每一类都交给 context（修 bug：skill 提及以前没有转交，TUI 里从来不提醒）。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::listing::{ListingSource, Listings};
use rust_cli_kernel_api::model::{ModelSelection, QuerySource};
use rust_cli_kernel_api::orchestration::{ContextRequest, OrchestrationError};
use rust_cli_kernel_api::permission::PermissionMode;

use super::CombinedListings;

struct Names(&'static [&'static str]);

#[async_trait]
impl ListingSource for Names {
    async fn listings(&self, _request: &ContextRequest) -> Result<Listings, OrchestrationError> {
        Ok(Listings::default())
    }

    async fn skill_mentions(
        &self,
        _request: &ContextRequest,
        _input: &str,
    ) -> Result<Vec<String>, OrchestrationError> {
        Ok(self.0.iter().map(|name| (*name).to_owned()).collect())
    }
}

#[tokio::test]
async fn skill_mentions_are_passed_through() {
    let combined = CombinedListings(vec![Arc::new(Names(&[])), Arc::new(Names(&["greet"]))]);
    let request = ContextRequest::new(
        SessionId::new("s"),
        WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/w"),
        },
        QuerySource::MainThread,
        PermissionMode::Default,
        ModelSelection::new("p".to_owned(), "m".to_owned()),
    );
    assert_eq!(
        combined
            .skill_mentions(&request, "run /greet")
            .await
            .unwrap(),
        ["greet"]
    );
}
