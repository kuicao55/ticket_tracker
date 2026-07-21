//! `tt stop`：结束后台 daemon。

use std::process::Command;

use anyhow::{anyhow, Result};

use crate::paths;

pub fn run() -> Result<()> {
    let pid_path = paths::pid_file();
    if !pid_path.exists() {
        println!("无后台 daemon。");
        return Ok(());
    }
    let pid = std::fs::read_to_string(&pid_path)?.trim().to_string();
    let pid_n: u32 = pid.parse().map_err(|_| anyhow!("pid 文件损坏"))?;
    let status = Command::new("kill").arg(&pid).status();
    match status {
        Ok(s) if s.success() => {
            let _ = std::fs::remove_file(&pid_path);
            println!("已停止 daemon pid={}", pid_n);
        }
        _ => {
            // 清理 stale 文件
            let _ = std::fs::remove_file(&pid_path);
            println!("daemon pid={} 已退出，清理 pid 文件", pid_n);
        }
    }
    Ok(())
}
