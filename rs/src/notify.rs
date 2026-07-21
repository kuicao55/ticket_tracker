//! 通知层：Discord Webhook + macOS 电脑通知（带 caffeinate 防休眠）。
//!
//! 与 py/.../notify.py 1:1 对齐。参考：RUST_PORT.md §5.5、§5.6。

use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::Result;

const USER_AGENT: &str = "ticket-tracker/1.0 (+Rust reqwest)";
const DISCORD_HOST: &str = "https://discord.com/api/webhooks/";

#[cfg(target_os = "macos")]
pub const IS_MAC: bool = true;
#[cfg(not(target_os = "macos"))]
pub const IS_MAC: bool = false;

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

    let cli = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(15))
        .danger_accept_invalid_certs(true)
        .build()?;

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
    let _ = last; // 同 Python：warn 即可
    Ok(false)
}

pub fn notify_discord(
    webhook: Option<&str>,
    title: &str,
    message: &str,
    url: Option<&str>,
) -> Result<bool> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(notify_discord_async(webhook, title, message, url))
}

// ----------------- macOS 电脑通知 -----------------

/// 在 macOS 上发系统通知 + 周期性响铃 + 语音 + 自动打开购票页。
/// 非 macOS 平台静默返回。
pub fn notify_macos(title: &str, message: &str, sound: bool, open_url: Option<&str>, duration_secs: u64) {
    if !IS_MAC {
        return;
    }
    // 弹窗
    let script = format!(
        "display notification \"{}\" with title \"{}\" sound name \"Glass\"",
        message.replace('"', "'"),
        title.replace('"', "'")
    );
    let _ = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    // 自动打开购票页（只开一次）
    if let Some(u) = open_url {
        let _ = Command::new("open").arg(u).spawn();
    }
    // 周期性响铃 + 一次性语音
    if !sound {
        return;
    }
    let start = std::time::Instant::now();
    let mut said = false;
    while start.elapsed() < Duration::from_secs(duration_secs.max(1)) {
        print!("\x07");
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let _ = Command::new("afplay")
            .arg("/System/Library/Sounds/Glass.aiff")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !said {
            let _ = Command::new("say")
                .arg("预售已开启，快去抢票")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            said = true;
        }
        std::thread::sleep(Duration::from_secs(3));
    }
}

// ----------------- caffeinate 防休眠 -----------------

static CAFFEINATE: OnceLock<Mutex<Option<Child>>> = OnceLock::new();

fn cell() -> &'static Mutex<Option<Child>> {
    CAFFEINATE.get_or_init(|| Mutex::new(None))
}

pub fn caffeinate_start(child_pid: u32) -> Option<u32> {
    if !IS_MAC {
        return None;
    }
    let mut guard = cell().lock().ok()?;
    // 若已有，先 terminate
    if let Some(mut c) = guard.take() {
        let _ = c.kill();
    }
    match Command::new("caffeinate")
        .args(["-i", "-s", "-w", &child_pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => {
            let pid = c.id();
            *guard = Some(c);
            Some(pid)
        }
        Err(_) => None,
    }
}

pub fn caffeinate_stop() {
    if !IS_MAC {
        return;
    }
    if let Ok(mut g) = cell().lock() {
        if let Some(mut c) = g.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

pub fn is_caffeinated() -> Option<bool> {
    if !IS_MAC {
        return None;
    }
    let mut g = cell().lock().ok()?;
    match g.as_mut() {
        Some(c) => {
            // try_wait 不卡死
            match c.try_wait() {
                Ok(None) => Some(true),
                _ => Some(false),
            }
        }
        None => Some(false),
    }
}
