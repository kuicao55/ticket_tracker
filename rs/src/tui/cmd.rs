//! `:` 命令面板执行器。
//!
//! 支持的命令详见 RUST_PORT.md §7.7。

use shell_words::split;

use super::App;
use crate::{config, maoyan};

pub fn execute(app: &mut App, line: &str) {
    let parts: Vec<String> = match split(line) {
        Ok(p) => p,
        Err(e) => {
            push_event(app, format!("✗ cmd: 解析失败: {}", e));
            return;
        }
    };
    if parts.is_empty() {
        return;
    }
    let cmd = parts[0].as_str();
    let rest: Vec<&str> = parts[1..].iter().map(String::as_str).collect();
    let result = match cmd {
        "run" => cmd_run(app),
        "interval" => cmd_interval(&rest, app),
        "webhook" => cmd_webhook(&rest, app),
        "quiet" => cmd_quiet(&rest, app),
        "phone" => cmd_phone(&rest, app),
        "report" => cmd_report(&rest, app),
        "films" => cmd_films(&rest),
        "log" => cmd_log(&rest),
        "doctor" => cmd_doctor(),
        "add" => cmd_add(&rest, app),
        "rm" => cmd_rm(&rest, app),
        "enable" => cmd_enable(&rest, app, true),
        "disable" => cmd_enable(&rest, app, false),
        "toggle" => cmd_toggle(&rest, app),
        "edit" => cmd_edit(&rest, app),
        "help" | "h" | "?" => Ok("可用的命令见 ? 帮助覆盖层".to_string()),
        "quit" | "q" => {
            app.should_quit = true;
            Ok("".into())
        }
        other => Err(format!("未知命令: {}", other)),
    };
    match result {
        Ok(msg) => {
            if !msg.is_empty() {
                push_event(app, format!("✓ cmd: {}", msg));
            }
        }
        Err(e) => push_event(app, format!("✗ cmd: {}", e)),
    }
}

pub fn push_event(app: &mut App, line: String) {
    let ts = chrono::Local::now().format("%H:%M:%S").to_string();
    let entry = format!("[{}] {}", ts, line);
    // 直接走 events_snapshot 的位置（这里只是状态栏文案提示，monitor 跑在
    // 独立线程）。保留 entry 给状态栏用；
    let entry_for_events = entry.clone();
    app.status_msg = Some(entry);
    app.status_msg_until =
        Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
    // 真实事件线也同步写入 monitor 的 events 队列（同步 lock，不 await）
    {
        let mut q = app.monitor.shared.events.lock().unwrap();
        if q.len() >= 64 {
            q.pop_back();
        }
        q.push_front(entry_for_events);
    }
}

pub fn push_status(app: &mut App, msg: String, secs: u64) {
    app.status_msg = Some(msg);
    app.status_msg_until =
        Some(std::time::Instant::now() + std::time::Duration::from_secs(secs));
}

/// 拿当前选中 watch 的 id（按 `app.watch_idx`）。无选则返回 None。
pub fn current_wid(app: &App) -> Option<String> {
    let cfg = app.monitor.cfg_snapshot();
    cfg.get("watches")
        .and_then(|v| v.as_array())
        .and_then(|a| a.get(app.watch_idx))
        .and_then(|w| w.get("id"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

pub fn current_watch_enabled(app: &App) -> Option<bool> {
    let cfg = app.monitor.cfg_snapshot();
    cfg.get("watches")
        .and_then(|v| v.as_array())
        .and_then(|a| a.get(app.watch_idx))
        .and_then(|w| w.get("enabled"))
        .and_then(|v| v.as_bool())
}

fn cmd_run(_app: &mut App) -> Result<String, String> {
    // 强制 tick 的力量在 input.rs 里通过 force_check 处理
    Err("r 键已经触发；这里不重复".into())
}

fn cmd_interval(rest: &[&str], _app: &mut App) -> Result<String, String> {
    let secs: u64 = rest
        .first()
        .ok_or_else(|| "用法: :interval <sec>".to_string())?
        .parse()
        .map_err(|_| "interval 必须是数字".to_string())?;
    let mut cfg = config::load_or_init().map_err(|e| e.to_string())?;
    cfg["check_interval"] = serde_json::json!(secs);
    config::save(&cfg).map_err(|e| e.to_string())?;
    Ok(format!("interval = {}s", secs))
}

fn cmd_webhook(rest: &[&str], _app: &mut App) -> Result<String, String> {
    let mut cfg = config::load_or_init().map_err(|e| e.to_string())?;
    if rest.first().map(|s| *s == "clear").unwrap_or(false) {
        cfg["discord_webhook"] = serde_json::Value::Null;
        config::save(&cfg).map_err(|e| e.to_string())?;
        return Ok("webhook = (cleared)".into());
    }
    let url = rest.first().ok_or_else(|| "用法: :webhook <url|clear>".to_string())?;
    cfg["discord_webhook"] = serde_json::json!(url);
    config::save(&cfg).map_err(|e| e.to_string())?;
    Ok(format!("webhook = {}", url))
}

fn cmd_quiet(rest: &[&str], _app: &mut App) -> Result<String, String> {
    let v = rest.first().ok_or_else(|| "用法: :quiet HH:MM-HH:MM".to_string())?;
    config::parse_window(v).map_err(|e| e.to_string())?;
    let mut cfg = config::load_or_init().map_err(|e| e.to_string())?;
    cfg["quiet_window"] = serde_json::json!(v);
    config::save(&cfg).map_err(|e| e.to_string())?;
    Ok(format!("quiet_window = {}", v))
}

fn cmd_phone(rest: &[&str], _app: &mut App) -> Result<String, String> {
    let v = rest.first().ok_or_else(|| "用法: :phone HH:MM-HH:MM".to_string())?;
    config::parse_window(v).map_err(|e| e.to_string())?;
    let mut cfg = config::load_or_init().map_err(|e| e.to_string())?;
    cfg["phone_only_window"] = serde_json::json!(v);
    config::save(&cfg).map_err(|e| e.to_string())?;
    Ok(format!("phone_only_window = {}", v))
}

fn cmd_report(rest: &[&str], _app: &mut App) -> Result<String, String> {
    let secs: u64 = rest
        .first()
        .ok_or_else(|| "用法: :report <sec>".to_string())?
        .parse()
        .map_err(|_| "report 必须是数字".to_string())?;
    let mut cfg = config::load_or_init().map_err(|e| e.to_string())?;
    cfg["heartbeat_interval_sec"] = serde_json::json!(secs);
    config::save(&cfg).map_err(|e| e.to_string())?;
    Ok(format!("heartbeat = {}s", secs))
}

fn cmd_films(rest: &[&str]) -> Result<String, String> {
    let show_type: u8 = rest.first().and_then(|s| s.parse().ok()).unwrap_or(2);
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let pairs = rt
        .block_on(maoyan::fetch_films_list_async(show_type))
        .map_err(|e| e.to_string())?;
    let s = pairs
        .iter()
        .map(|(id, n)| format!("{} {}", id, n))
        .collect::<Vec<_>>()
        .join("; ");
    Ok(format!("已拉取 {} 条 → {}", pairs.len(), if s.len() > 80 { &s[..80] } else { &s }))
}

fn cmd_log(_rest: &[&str]) -> Result<String, String> {
    Err("请用 tt log 命令行查看".into())
}

fn cmd_doctor() -> Result<String, String> {
    match crate::cli::doctor::run_full() {
        Ok(_) => Ok("doctor 已运行（输出见 stdout）".into()),
        Err(e) => Err(e.to_string()),
    }
}

fn cmd_add(rest: &[&str], _app: &mut App) -> Result<String, String> {
    if rest.is_empty() {
        return Err("用法: :add <movie_id> [-c <cinema>...] [-d <date>...] [--name ...]".into());
    }
    let movie_id: i64 = rest[0].parse().map_err(|_| "movie_id 必须是数字".to_string())?;
    // 简化解析：剩余参数识别 -c / -d / --name / --interval
    let mut cinemas: Vec<String> = vec![];
    let mut dates: Vec<String> = vec![];
    let mut name: Option<String> = None;
    let mut interval: Option<u64> = None;
    let mut i = 1;
    while i < rest.len() {
        match rest[i] {
            "-c" => {
                if let Some(v) = rest.get(i + 1) {
                    cinemas.push(v.to_string());
                    i += 2;
                } else {
                    return Err("-c 缺参数".into());
                }
            }
            "-d" => {
                if let Some(v) = rest.get(i + 1) {
                    dates.push(v.to_string());
                    i += 2;
                } else {
                    return Err("-d 缺参数".into());
                }
            }
            "--name" => {
                if let Some(v) = rest.get(i + 1) {
                    name = Some(v.to_string());
                    i += 2;
                } else {
                    return Err("--name 缺参数".into());
                }
            }
            "--interval" => {
                if let Some(v) = rest.get(i + 1) {
                    interval = Some(v.parse().map_err(|_| "interval 数字错误")?);
                    i += 2;
                } else {
                    return Err("--interval 缺参数".into());
                }
            }
            other => {
                return Err(format!("未知参数: {}", other));
            }
        }
    }
    let mut cfg = config::load_or_init().map_err(|e| e.to_string())?;
    let cref: Vec<&str> = cinemas.iter().map(String::as_str).collect();
    let id = config::add_watch(
        &mut cfg,
        movie_id,
        &cref,
        if dates.is_empty() { None } else { Some(&dates) },
        name.as_deref(),
        interval,
    )
    .map_err(|e| e.to_string())?;
    Ok(format!("添加 watch {}", id))
}

fn cmd_rm(rest: &[&str], _app: &mut App) -> Result<String, String> {
    let id = rest.first().ok_or_else(|| "用法: :rm <watch_id>".to_string())?;
    let mut cfg = config::load_or_init().map_err(|e| e.to_string())?;
    if config::remove_watch(&mut cfg, id).map_err(|e| e.to_string())? {
        Ok(format!("已删除 {}", id))
    } else {
        Err(format!("watch 不存在: {}", id))
    }
}

fn cmd_enable(rest: &[&str], _app: &mut App, en: bool) -> Result<String, String> {
    let id = rest.first().ok_or_else(|| "用法: :enable|disable <watch_id>".to_string())?;
    let mut cfg = config::load_or_init().map_err(|e| e.to_string())?;
    if let Some(w) = config::find_watch_mut(&mut cfg, id) {
        w["enabled"] = serde_json::json!(en);
        config::save(&cfg).map_err(|e| e.to_string())?;
        Ok(format!("{} {}", id, if en { "已启用" } else { "已停用" }))
    } else {
        Err(format!("watch 不存在: {}", id))
    }
}

fn cmd_edit(_rest: &[&str], _app: &mut App) -> Result<String, String> {
    Err("简化版：未实现，编辑请用 tt watch edit".into())
}

/// 反转当前 watch 的 enabled 状态。无 wid 时报错。
fn cmd_toggle(_rest: &[&str], app: &mut App) -> Result<String, String> {
    let wid = current_wid(app).ok_or_else(|| "当前没有选中的 watch".to_string())?;
    let mut cfg = config::load_or_init().map_err(|e| e.to_string())?;
    let cur = config::find_watch_mut(&mut cfg, &wid)
        .and_then(|w| w.get("enabled").and_then(|v| v.as_bool()))
        .unwrap_or(false);
    if let Some(w) = config::find_watch_mut(&mut cfg, &wid) {
        w["enabled"] = serde_json::json!(!cur);
    }
    config::save(&cfg).map_err(|e| e.to_string())?;
    Ok(format!("{} 已{}", wid, if !cur { "启用" } else { "停用" }))
}
