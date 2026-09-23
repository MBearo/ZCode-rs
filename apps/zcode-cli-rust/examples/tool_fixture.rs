//! Test-only direct adapter driver; the shipped Agent still only accepts app-server --stdio.
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use zcode_cli_rust::{adapters::tools::WorkspaceTools, contract::ToolPort};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cwd = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let tools = WorkspaceTools::new(cwd.clone(), cwd.join(".artifacts"));
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        let input: Value = serde_json::from_str(&line)?;
        let result = if input["definitions"] == true {
            json!({"definitions":tools.definitions()})
        } else {
            match tools
                .call(
                    input["session"].as_str().unwrap_or("fixture"),
                    input["name"].as_str().unwrap(),
                    &input["args"],
                    &tokio_util::sync::CancellationToken::new(),
                )
                .await
            {
                Ok(result) => {
                    json!({"data":result.data,"content":result.content,"display":result.display})
                }
                Err(e) => json!({"error":e.to_string()}),
            }
        };
        out.write_all(format!("{result}\n").as_bytes()).await?;
        out.flush().await?;
    }
    tools.shutdown().await
}
