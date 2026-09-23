//! Differential-test driver; never linked into the production binary.
use serde_json::{Value, json};
use std::io::{BufRead, Write};
fn main() -> anyhow::Result<()> {
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    for line in std::io::stdin().lock().lines() {
        let v: Value = serde_json::from_str(&line?)?;
        let result = zcode_cli_domain::option_map::evaluate(
            v["source"].as_str().unwrap(),
            v["variable"].as_str().unwrap(),
            &v["input"],
        );
        writeln!(
            out,
            "{}",
            match result {
                Ok(value) => json!({"value":value}),
                Err(_) => json!({"error":true}),
            }
        )?;
    }
    Ok(())
}
