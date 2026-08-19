//! `tt test [all|discord|macos|xhs|wechat]`：通知测试。

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
    match k {
        "all" | "wechat" => {
            // 从 cfg 找第一个 wechat_notify=true 的 watch 作为测试目标
            // （微信发到「当前打开的会话」，所以只要确认有 watch 开启就发）
            let cfg = config::load_or_init()?;
            let any_on = cfg
                .get("watches")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter().any(|w| {
                        w.get("wechat_notify")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);
            if !any_on {
                println!("✗ 无 watch 开启 wechat_notify,请先 `tt watch add/edit --wechat-notify`");
                return Ok(());
            }
            // 拿首个开启的 watch 的 movie_name 做测试消息更直观
            let movie_name = cfg
                .get("watches")
                .and_then(|v| v.as_array())
                .and_then(|arr| {
                    arr.iter()
                        .find(|w| {
                            w.get("wechat_notify")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false)
                        })
                        .and_then(|w| w.get("movie_name").and_then(|v| v.as_str()))
                        .map(String::from)
                })
                .unwrap_or_else(|| "(未命名影片)".to_string());
            println!("→ 微信大群通知（发到当前微信会话，真发）…");
            // 与 TUI 测试通知模板一致：开头加 🧪 标识这是测试，末尾附购票链接
            // （测试用 https://www.maoyan.com/cinema/37534，与正式告警同字段可点回猫眼）
            let msg = format!(
                "🧪 测试通知\n\n🎬 预售开启\n\n🎞 {}\n🏛 测试影院\n📅 测试场次｜N 场｜HH:MM 至 HH:MM\n\n👉 https://www.maoyan.com/cinema/37534",
                movie_name
            );
            let ok = notify::notify_wechat(&msg);
            println!("{}", if ok { "✓ 已发送" } else { "✗ 失败（看 stderr）" });
        }
        _ => {}
    }
    Ok(())
}
