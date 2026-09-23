//! Test fixture: hold the cross-process import lock until stdin closes.
use std::io::{Read, Write};
fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("lock path");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    file.lock()?;
    std::io::stdout().write_all(b"locked\n")?;
    std::io::stdout().flush()?;
    let _ = std::io::stdin().read(&mut [0u8; 1])?;
    Ok(())
}
