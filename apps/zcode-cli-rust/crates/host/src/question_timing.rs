use anyhow::{Context, Result};

pub fn question_timing() -> Result<(u64, u64)> {
    let mut scale = 1.0;
    if std::env::var("ZCODE_ENV").as_deref() == Ok("test")
        && let Ok(raw) = std::env::var("ZCODE_E2E_ASK_USER_QUESTION_CLOCK_SCALE")
        && !raw.trim().is_empty()
    {
        scale = raw
            .trim()
            .parse::<f64>()
            .context("Invalid AskUserQuestion clock scale")?;
        anyhow::ensure!(
            scale.is_finite() && (1.0..=1000.0).contains(&scale),
            "AskUserQuestion clock scale must be between 1 and 1000"
        );
    }
    Ok((
        (60_000.0 / scale).round().max(1.0) as u64,
        (300_000.0 / scale).round().max(1.0) as u64,
    ))
}

