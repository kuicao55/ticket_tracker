//! tt —— 入口。
//!
//! 用 clap 二级 dispatch；详细子命令见 `cli/mod.rs`。
//! 设计依据：RUST_PORT.md §6。

use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use ticket_tracker::cli::{dispatch, Cli, Cmd};

fn main() -> ExitCode {
    init_log();
    let cli = Cli::parse();
    match cli.cmd {
        Some(cmd) => match dispatch(cmd) {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("✗ {}", e);
                ExitCode::FAILURE
            }
        },
        None => {
            // 默认：进入 TUI（等同 tt start）
            match run_default_tui() {
                Ok(_) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("✗ {}", e);
                    ExitCode::FAILURE
                }
            }
        }
    }
}

fn init_log() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(std::io::stderr)
        .try_init();
}

fn run_default_tui() -> anyhow::Result<()> {
    use ticket_tracker::monitor::Monitor;
    let monitor = Monitor::new(None)?;
    ticket_tracker::tui::run_blocking(monitor)
}

/// 临时入口（Phase 2 doctor / fetch 测试）
#[allow(dead_code)]
fn run_doctor_once() -> anyhow::Result<()> {
    use ticket_tracker::{config, maoyan};
    let cfg = config::load_or_init()?;
    println!("version = {}", cfg.get("version").and_then(|v| v.as_u64()).unwrap_or(0));
    println!("watches = {}", cfg.get("watches").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0));
    println!("discord_webhook = {}", cfg.get("discord_webhook").map(|v| v.to_string()).unwrap_or("null".into()));
    let _ = maoyan::fetch_films_list(2)?;
    println!("films ok");
    Ok(())
}

/// 帮助占位（防止 _ 模块死代码）
#[allow(dead_code)]
const _: Option<Cmd> = None;
