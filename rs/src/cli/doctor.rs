//! `tt doctor`：自检。`tt init`：首次创建配置。

use std::time::Instant;

use anyhow::Result;

use crate::{config, notify, paths};

pub fn run_init() -> Result<()> {
    let p = paths::config_file();
    let existed = p.exists();
    let _ = config::load_or_init()?; // create if missing
    if existed {
        println!("✓ 配置已存在：{}", p.display());
    } else {
        println!("✓ 已创建配置：{}", p.display());
    }
    println!("  编辑：tt config show");
    Ok(())
}

pub fn run_full() -> Result<()> {
    println!("ticket-tracker 自检");
    println!("  版本: {} ({})", env!("CARGO_PKG_VERSION"), std::env::consts::OS);

    // 配置
    let cfg = config::load_or_init()?;
    let watches = cfg.get("watches").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
    let wh = cfg.get("discord_webhook").and_then(|v| v.as_str());
    println!("✓ 配置文件：{}", paths::config_file().display());
    println!("✓ watches: {} 条", watches);
    println!(
        "{}",
        if wh.is_some() {
            "✓ Discord webhook: 已配置"
        } else {
            "✗ Discord webhook: 未配置"
        }
    );
    println!(
        "{}",
        if notify::IS_MAC {
            "✓ caffeinate（防休眠）: 可用"
        } else {
            "○ caffeinate（防休眠）: 当前平台不可用"
        }
    );

    // 网络
    let start = Instant::now();
    let rt = tokio::runtime::Runtime::new()?;
    let ok = rt.block_on(async {
        match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .danger_accept_invalid_certs(true)
            .build()
        {
            Ok(c) => c.get("https://m.maoyan.com/").send().await.is_ok(),
            Err(_) => false,
        }
    });
    println!(
        "{} m.maoyan.com ({} ms)",
        if ok { "✓ 网络可达" } else { "✗ 网络不通" },
        start.elapsed().as_millis()
    );
    Ok(())
}
