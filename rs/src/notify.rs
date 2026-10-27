//! 通知层：只负责**远程**通道。
//!
//! - Discord Webhook / 通用结果 webhook
//! - 邮件（本地 msmtp）
//!
//! 微信 / 小红书 / 锁票由独立 Gofer 服务执行，不在主服务内。

use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::Result;

const USER_AGENT: &str = "ticket-tracker/2.0 (+Rust reqwest)";
const DISCORD_HOST: &str = "https://discord.com/api/webhooks/";

static NOTIFY_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn http_client() -> &'static reqwest::Client {
    NOTIFY_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(8)
            .gzip(true)
            .danger_accept_invalid_certs(true)
            .build()
            .expect("reqwest notify client")
    })
}

// ----------------- Discord -----------------

pub async fn notify_discord_async(
    webhook: Option<&str>,
    title: &str,
    message: &str,
    url: Option<&str>,
) -> Result<bool> {
    let Some(wh) = webhook else {
        return Ok(false);
    };
    if !wh.starts_with(DISCORD_HOST) {
        return Ok(false);
    }
    let mut content = format!("**{}**\n{}", title, message);
    if let Some(u) = url {
        content.push_str(&format!("\n👉 {}", u));
    }
    let payload = serde_json::json!({ "content": content });

    let cli = http_client();
    let mut last: Option<String> = None;
    for i in 0..3 {
        match cli.post(wh).json(&payload).send().await {
            Ok(r) if r.status().is_success() => return Ok(true),
            Ok(r) => last = Some(format!("HTTP {}", r.status())),
            Err(e) => last = Some(format!("req: {}", e)),
        }
        if i + 1 < 3 {
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }
    let _ = last; // 失败只记录，不阻塞调用方
    Ok(false)
}

// ----------------- 结果通知（仅告警，不发心跳） -----------------

/// 通过 webhook 发「结果通知」。与 `discord_webhook` 的关键区别：
/// 这条通道**只**在告警事件里被调（开票提醒 / 增场新增），不会发每小时的例行报告。
///
/// URL 形如 `https://discord.com/api/webhooks/...` 时走 Discord 格式；
/// 其他 URL（飞书 / Slack / Bark / 自建服务）按通用 JSON `{title, content, url}` 发。
/// 返回 `Ok(true)` 表示至少一次请求成功。
pub async fn notify_results_webhook_async(
    url: Option<&str>,
    title: &str,
    message: &str,
    click_url: Option<&str>,
) -> Result<bool> {
    let Some(u) = url else {
        return Ok(false);
    };
    if u.trim().is_empty() {
        return Ok(false);
    }

    let payload: serde_json::Value = if u.starts_with(DISCORD_HOST) {
        let mut content = format!("**{}**\n{}", title, message);
        if let Some(link) = click_url {
            content.push_str(&format!("\n👉 {}", link));
        }
        serde_json::json!({ "content": content })
    } else {
        serde_json::json!({
            "title": title,
            "content": message,
            "url": click_url,
        })
    };

    let cli = http_client();
    let mut last: Option<String> = None;
    for i in 0..3 {
        match cli.post(u).json(&payload).send().await {
            Ok(r) if r.status().is_success() => return Ok(true),
            Ok(r) => last = Some(format!("HTTP {}", r.status())),
            Err(e) => last = Some(format!("req: {}", e)),
        }
        if i + 1 < 3 {
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }
    let _ = last;
    Ok(false)
}

/// 通过本地 `msmtp` 子进程发「结果通知」邮件。
/// 收件人从 config 的 `notify_email_to` 读，SMTP 凭证 / 发件人由用户在自己
/// 的 `~/.msmtprc` 里配置（`brew install msmtp` 一次，永久免维护）。
/// `msmtp` 不在 PATH / 未安装时静默失败（不抛错），避免告警被吞。
pub async fn notify_results_email_async(
    to_addr: Option<&str>,
    title: &str,
    message: &str,
    click_url: Option<&str>,
) -> bool {
    let to_addr = to_addr.map(String::from);
    let title = title.to_string();
    let message = message.to_string();
    let click_url = click_url.map(String::from);
    tokio::task::spawn_blocking(move || {
        notify_results_email(to_addr.as_deref(), &title, &message, click_url.as_deref())
    })
    .await
    .unwrap_or(false)
}

pub fn notify_results_email(
    to_addr: Option<&str>,
    title: &str,
    message: &str,
    click_url: Option<&str>,
) -> bool {
    let Some(addr) = to_addr else {
        return false;
    };
    if addr.trim().is_empty() {
        return false;
    }
    let mut body = format!(
        "From: ticket-tracker <ticket-tracker@localhost>\n\
         To: {}\n\
         Subject: {}\n\
         Content-Type: text/plain; charset=UTF-8\n\
         \n\
         {}",
        addr, title, message
    );
    if let Some(u) = click_url {
        body.push_str(&format!("\n\n👉 {}", u));
    }
    let result = Command::new("msmtp")
        .arg(addr)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match result {
        Ok(mut child) => {
            if let Some(stdin) = child.stdin.as_mut() {
                use std::io::Write;
                let _ = stdin.write_all(body.as_bytes());
            }
            matches!(child.wait(), Ok(s) if s.success())
        }
        Err(_) => false,
    }
}
