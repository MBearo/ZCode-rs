use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(version, about = "Headless ZCode Rust core (App stdio)")]
pub struct Args {
    #[arg(value_parser=["app-server"])]
    pub command: String,
    #[arg(long, required = true)]
    pub stdio: bool,
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
    #[arg(long)]
    pub import_ts_db: Option<PathBuf>,
    #[arg(long)]
    pub config: Option<PathBuf>,
    #[arg(long, value_parser=["desktop","terminal"], default_value="terminal")]
    pub surface: String,
    #[arg(long)]
    pub prepare_storage: bool,
}
