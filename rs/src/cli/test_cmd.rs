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
    match k {
        "all" | "xhs" => {
            // 从 cfg 找第一个配了 xhs_group 的 watch,作为测试目标群
            let cfg = config::load_or_init()?;
            let target_group = cfg
                .get("watches")
                .and_then(|v| v.as_array())
                .and_then(|arr| {
                    arr.iter().find_map(|w| {
                        w.get("xhs_group")
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(String::from)
                    })
                });
            let Some(group) = target_group else {
                println!("✗ 无 watch 配 xhs_group,请先 `tt watch add/edit --xhs-group <群名>`");
                return Ok(());
            };
            println!("→ 小红书群通知（group={}, 真发）…", group);
            let body = "【ticket-tracker 自动化测试通知】本条消息由 ticket-tracker 监控服务自动发出,用于验证小红书群通知通道配置。｜ 收到本消息无需任何操作,正式告警将沿用相同通道发送。｜ 如非本人操作或频繁收到此类消息,请联系管理员。";
            let ok = notify::notify_xhs(&group, "ticket-tracker 自动化测试通知", body, None);
            println!("{}", if ok { "✓ 已发送" } else { "✗ 失败（看 stderr）" });
        }
        _ => {}
    }
    Ok(())
}
