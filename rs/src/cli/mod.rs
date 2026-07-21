//! CLI 子命令入口 —— clap derive。

pub mod start;
pub mod stop;
pub mod watch;
pub mod cinema;
pub mod films;
pub mod config_cmd;
pub mod test_cmd;
pub mod doctor;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "tt",
    version,
    about = "猫眼影城影片预售监测 CLI（Rust 重写版）"
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// 首次配置（交互或非交互）
    Init,
    /// 启动监测（TUI 或后台）
    Start {
        #[arg(long)]
        detach: bool,
        #[arg(long)]
        interval: Option<u64>,
        #[arg(long = "watch")]
        watch: Vec<String>,
    },
    /// 停止后台
    Stop,
    /// 重启
    Restart,
    /// 一行状态
    Status,
    /// 查看日志
    Log {
        #[arg(short = 'n', long, default_value_t = 30)]
        n: usize,
        #[arg(short, long)]
        follow: bool,
    },
    /// watch 子命令
    Watch {
        #[command(subcommand)]
        action: watch::WatchAction,
    },
    /// cinema 子命令
    Cinema {
        #[command(subcommand)]
        action: cinema::CinemaAction,
    },
    /// films 列表
    Films {
        #[arg(default_value_t = 2)]
        show_type: u8,
    },
    /// config 子命令
    Config {
        #[command(subcommand)]
        action: config_cmd::ConfigAction,
    },
    /// 通知测试
    Test {
        kind: Option<String>,
    },
    /// 自检
    Doctor,
}

pub fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Init => {
            doctor::run_init()?;
        }
        Cmd::Start { detach, interval, watch } => {
            start::run(detach, interval, watch)?;
        }
        Cmd::Stop => stop::run()?,
        Cmd::Restart => {
            stop::run().ok();
            start::run(false, None, vec![])?;
        }
        Cmd::Status => start::status()?,
        Cmd::Log { n, follow } => start::log(n, follow)?,
        Cmd::Watch { action } => watch::dispatch(action)?,
        Cmd::Cinema { action } => cinema::dispatch(action)?,
        Cmd::Films { show_type } => films::run(show_type)?,
        Cmd::Config { action } => config_cmd::dispatch(action)?,
        Cmd::Test { kind } => test_cmd::run(kind.as_deref())?,
        Cmd::Doctor => doctor::run_full()?,
    }
    Ok(())
}
