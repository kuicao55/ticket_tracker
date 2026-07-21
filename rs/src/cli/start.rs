//! `tt start` / `start --detach` / `start --watch ID`：前台 TUI 或后台 daemon。

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use tracing::info;

use crate::{paths, tui};

pub fn run(detach: bool, interval: Option<u64>, _watch: Vec<String>) -> Result<()> {
    if detach {
        return run_detach(interval);
    }
    // 前台：跑 TUI
    let monitor = crate::monitor::Monitor::new(None)?;
    let _ = interval;
    tui::run_blocking(monitor)
}

fn run_detach(interval: Option<u64>) -> Result<()> {
    let pid_path = paths::pid_file();
    if pid_path.exists() {
        let pid = std::fs::read_to_string(&pid_path).ok();
        if let Some(pid) = pid {
            return Err(anyhow!("daemon 已在运行（pid={}），先 tt stop", pid.trim()));
        }
    }
    let mut cfg = crate::config::load_or_init()?;
    if let Some(i) = interval {
        cfg["check_interval"] = serde_json::json!(i);
        crate::config::save(&cfg)?;
    }
    info!("daemon 模式：pid 文件 = {}", pid_path.display());
    Err(anyhow!(
        "Rust 后台 daemon 暂未实现，使用 `tt start` 进入前台 TUI。"
    ))
}

pub fn status() -> Result<()> {
    let pid_path = paths::pid_file();
    if pid_path.exists() {
        let pid = std::fs::read_to_string(&pid_path)?;
        let running = Path::new(&format!("/proc/{}", pid.trim())).exists();
        println!(
            "daemon pid {} ({})",
            pid.trim(),
            if running { "running" } else { "stale" }
        );
    } else {
        println!("无后台 daemon。前台模式：tt start");
    }
    Ok(())
}

pub fn log(n: usize, follow: bool) -> Result<()> {
    let lf = paths::log_file();
    if !lf.exists() {
        println!("(无日志文件 {})", lf.display());
        return Ok(());
    }
    let content = std::fs::read_to_string(&lf).context("读 log 失败")?;
    let mut lines: Vec<&str> = content.lines().collect();
    if lines.len() > n {
        lines = lines.split_off(lines.len() - n);
    }
    for l in lines {
        println!("{}", l);
    }
    if follow {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(&lf)?;
        let mut pos = f.metadata()?.len();
        f.seek(SeekFrom::Start(pos))?;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let mut buf = String::new();
            f.seek(SeekFrom::Start(pos))?;
            f.read_to_string(&mut buf)?;
            if !buf.is_empty() {
                print!("{}", buf);
                pos += buf.len() as u64;
            }
        }
    }
    Ok(())
}
