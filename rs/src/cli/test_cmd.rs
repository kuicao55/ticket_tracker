//! `tt test [all|discord|macos]`：通知测试。

use anyhow::Result;

use crate::{config, notify};

pub fn run(kind: Option<&str>) -> Result<()> {
    let k = kind.unwrap_or("all");
    match k {
        "all" | "discord" => {
            let cfg = config::load_or_init()?;
            let url = cfg.get("discord_webhook").and_then(|v| v.as_str());
            if url.is_none() {
                println!("✗ Discord 未配置（tt config set discord-webhook <url>）");
            } else {
                println!("→ Discord 测试推送…");
                let ok = notify::notify_discord(url, "ticket-tracker 测试 🧪", "这是一条测试消息。", None)?;
                println!("{}", if ok { "✓ 推送成功" } else { "✗ 推送失败" });
            }
        }
        _ => {}
    }
    match k {
        "all" | "macos" => {
            if !notify::IS_MAC {
                println!("(macos 通知在非 macOS 平台静默跳过)");
            } else {
                println!("→ macOS 系统通知…");
                notify::notify_macos("ticket-tracker 测试 🧪", "测试通知", true, None, 3);
                println!("✓ 已发送");
            }
        }
        _ => {}
    }
    Ok(())
}
