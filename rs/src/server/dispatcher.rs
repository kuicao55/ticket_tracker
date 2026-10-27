//! 薄通知分发器。
//!
//! 触发事件后 fan-out 到 watch 配置的渠道：
//! - Discord / 通用 webhook / 邮件：主服务直接发送
//! - 微信 / 小红书：POST 给外部 Gofer（短超时，202 即完成）
//! - 微信小程序：本期只统计订阅目标，不调用真实微信 API
//!
//! 不持久化、不重试、不阻塞监控轮询。

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Local, NaiveDate};
use serde_json::{json, Value};

use super::{now_secs, AppState};
use crate::db::{ChannelRow, Settings};
use crate::maoyan;
use crate::notify;
use crate::time_spec::TimeSpec;

pub async fn dispatch_trigger(state: Arc<AppState>, event: Value) {
    let settings = match Settings::load(&state.db).await {
        Ok(s) => s,
        Err(e) => {
            state.push_event("notify", None, format!("读取通知设置失败: {}", e));
            return;
        }
    };
    let Some(wid) = event.get("watch_id").and_then(|v| v.as_str()) else {
        return;
    };
    let title = event
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("🎟 新增场次")
        .to_string();
    let message = event
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let buy_url = event
        .get("triggers")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|t| t.get("cinema_id"))
        .and_then(|v| v.as_str())
        .map(maoyan::buy_pc_url_owned);

    let channels = match state.db.watch_channels(wid).await {
        Ok(c) => c,
        Err(e) => {
            state.push_event("notify", Some(wid), format!("读取渠道失败: {}", e));
            return;
        }
    };
    let bindings = state
        .db
        .watch_channel_bindings(wid)
        .await
        .unwrap_or_default();

    let mut set = tokio::task::JoinSet::new();
    for ch in channels.iter().filter(|c| c.channel_type != "autolock") {
        let st = state.clone();
        let ch = ch.clone();
        let cfg = effective_channel_config(&ch, bindings.get(&ch.id));
        let title = title.clone();
        let message = message.clone();
        let buy_url = buy_url.clone();
        let ev = event.clone();
        let settings = settings.clone();
        set.spawn(async move {
            let _ = deliver_channel(
                &st,
                &ch,
                &cfg,
                &title,
                &message,
                buy_url.as_deref(),
                &ev,
                &settings,
            )
            .await;
        });
    }

    // 小程序：本期只统计目标，适配器留接口。
    if channels
        .iter()
        .any(|c| c.channel_type == "wechat_miniprogram")
    {
        match state.db.push_targets_for_watch(wid).await {
            Ok(targets) => {
                let openids = targets.iter().filter(|t| t.openid.is_some()).count();
                state.push_event(
                    "notify",
                    Some(wid),
                    format!(
                        "小程序推送：{} 个订阅目标（适配器未启用，跳过真实发送）",
                        openids
                    ),
                );
            }
            Err(e) => state.push_event("notify", Some(wid), format!("读取小程序订阅失败: {}", e)),
        }
    }

    while set.join_next().await.is_some() {}
}

fn effective_channel_config(ch: &ChannelRow, binding: Option<&Value>) -> Value {
    let mut out = ch.config().as_object().cloned().unwrap_or_default();
    if let Some(binding) = binding.and_then(|v| v.as_object()) {
        for (k, v) in binding {
            if !v.is_null() {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(out)
}

#[allow(clippy::too_many_arguments)]
async fn deliver_channel(
    state: &Arc<AppState>,
    ch: &ChannelRow,
    cfg: &Value,
    title: &str,
    message: &str,
    buy_url: Option<&str>,
    event: &Value,
    settings: &Settings,
) -> Result<String, String> {
    let result: Result<String, String> = match ch.channel_type.as_str() {
        "discord" | "webhook" => {
            let url = cfg
                .get("url")
                .or_else(|| cfg.get("webhook"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let Some(url) = url else {
                let r: Result<String, String> = Err("渠道未配置 url/webhook".to_string());
                log_result(state, ch, event, r.clone());
                return r;
            };
            match notify::notify_results_webhook_async(Some(url), title, message, buy_url).await {
                Ok(true) => Ok("webhook 已发送".into()),
                Ok(false) => Err("webhook 发送失败或返回非 2xx".into()),
                Err(e) => Err(format!("webhook 错误: {}", e)),
            }
        }
        "email" => {
            let to = cfg
                .get("to")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let Some(to) = to else {
                let r: Result<String, String> = Err("渠道未配置邮箱 to".into());
                log_result(state, ch, event, r.clone());
                return r;
            };
            if notify::notify_results_email_async(Some(to), title, message, buy_url).await {
                Ok("邮件已发送".into())
            } else {
                Err("邮件发送失败（检查本机 msmtp）".into())
            }
        }
        "wechat_local" | "xhs_local" => {
            let channel = if ch.channel_type == "wechat_local" {
                "wechat"
            } else {
                "xhs"
            };
            post_agent(
                state, settings, event, channel, title, message, buy_url, cfg,
            )
            .await
        }
        "wechat_miniprogram" => Ok("小程序适配器未启用".into()),
        other => Err(format!("未知渠道类型: {}", other)),
    };

    log_result(state, ch, event, result.clone());
    result
}

fn log_result(
    state: &Arc<AppState>,
    ch: &ChannelRow,
    event: &Value,
    result: Result<String, String>,
) {
    let wid = event.get("watch_id").and_then(|v| v.as_str());
    match result {
        Ok(msg) => state.push_event("notify", wid, format!("{}：{}", ch.name, msg)),
        Err(e) => state.push_event("notify", wid, format!("{}：发送失败（{}）", ch.name, e)),
    }
}

#[allow(clippy::too_many_arguments)]
async fn post_agent(
    state: &Arc<AppState>,
    settings: &Settings,
    event: &Value,
    channel: &str,
    title: &str,
    message: &str,
    url: Option<&str>,
    config: &Value,
) -> Result<String, String> {
    let base = config
        .get("agent_url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| settings.agent_url.trim_end_matches('/'));
    if base.is_empty() {
        return Err("未配置 agent_url".into());
    }
    let token = config
        .get("agent_token")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or(settings.agent_token.as_deref());
    let body = json!({
        "event_id": event.get("event_id"),
        "channel": channel,
        "title": title,
        "message": message,
        "url": url,
        "config": config,
        "source": "ticket-tracker",
    });
    let mut req = state
        .http
        .post(format!("{}/notify", base))
        .timeout(Duration::from_secs(3))
        .json(&body);
    if let Some(token) = token {
        req = req.header("X-Agent-Token", token);
    }
    match req.send().await {
        Ok(r) if r.status().is_success() => Ok(format!("已交给 Gofer（{}）", channel)),
        Ok(r) => Err(format!("Gofer HTTP {}", r.status())),
        Err(e) => Err(format!("Gofer 不可达: {}", e)),
    }
}

/// 面板「测试」按钮：向单个渠道发一条测试消息，返回可读结果。
pub async fn test_channel(state: Arc<AppState>, channel_id: &str, watch_id: &str) -> String {
    let settings = match Settings::load(&state.db).await {
        Ok(s) => s,
        Err(e) => return format!("读取设置失败: {}", e),
    };
    let Some(ch) = state.db.get_channel(channel_id).await.ok().flatten() else {
        return "渠道不存在".into();
    };
    if !ch.enabled {
        return format!("{} 已全局停用", ch.name);
    }
    if ch.channel_type == "autolock" {
        return "自动锁票不是通知渠道，无法发送测试消息".into();
    }
    let binding = state
        .db
        .watch_channel_config(watch_id, channel_id)
        .await
        .ok();
    let cfg = effective_channel_config(&ch, binding.as_ref());
    let event = json!({
        "event_id": format!("test_{}", &uuid::Uuid::new_v4().to_string()[..8]),
        "watch_id": watch_id,
        "type": "channel_test",
    });
    let title = "🧪 ticket-tracker 测试通知";
    let message = "这是一条来自 Web 面板的测试消息；收到即表示渠道链路正常。";
    match deliver_channel(&state, &ch, &cfg, title, message, None, &event, &settings).await {
        Ok(msg) => format!("{}：{}", ch.name, msg),
        Err(e) => format!("{}：测试失败（{}）", ch.name, e),
    }
}

/// 触发后把自动锁票任务 push 给外部 Gofer。
///
/// 安全语义：只有 watch 级 `auto_lock=true` 才提交，且始终 `confirm=true`；
/// 没有全局 dry-run。agent 执行 booker，结果通过 `/api/v1/agent/lock-result` 回调。
pub async fn maybe_dispatch_autolock(state: Arc<AppState>, event: &Value) {
    let Some(wid) = event.get("watch_id").and_then(|v| v.as_str()) else {
        return;
    };
    let watch = match state.db.get_watch(wid).await {
        Ok(Some(w)) => w,
        _ => return,
    };
    if !watch.auto_lock {
        return;
    }
    let settings = match Settings::load(&state.db).await {
        Ok(s) => s,
        Err(e) => {
            state.push_event("lock", Some(wid), format!("读取锁票设置失败: {}", e));
            return;
        }
    };
    let autolock_cfg = state
        .db
        .watch_channel_config(wid, "autolock-local")
        .await
        .unwrap_or_else(|_| json!({}));
    let base = autolock_cfg
        .get("agent_url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| settings.agent_url.trim_end_matches('/').to_string());
    if base.is_empty() {
        state.push_event("lock", Some(wid), "未配置 Gofer URL，跳过自动锁票");
        return;
    }
    let token = autolock_cfg
        .get("agent_token")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or(settings.agent_token.as_deref());
    let callback_url = std::env::var("TT_CALLBACK_URL")
        .ok()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:8787/api/v1/agent/lock-result".into());
    let time_spec = TimeSpec::from_json(&watch.time_spec_value()).ok();
    let event_id = event
        .get("event_id")
        .and_then(|v| v.as_str())
        .unwrap_or("evt");
    let mut seen: HashSet<String> = HashSet::new();

    for t in event
        .get("triggers")
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let Some(cid) = t.get("cinema_id").and_then(|v| v.as_str()) else {
            continue;
        };
        if !seen.insert(cid.to_string()) {
            continue;
        }
        if let Ok(Some(ls)) = state.db.lock_state(wid, cid).await {
            if ls.status == "ok" || ls.status == "exhausted" {
                continue;
            }
            if ls.status == "failed" && ls.tries >= watch.lock_max_retries.max(1) {
                continue;
            }
        }
        let key = format!("{}:{}", wid, cid);
        {
            let mut inflight = state.lock_inflight.lock().unwrap();
            if let Some(ts) = inflight.get(&key) {
                if now_secs() - *ts < 600.0 {
                    continue;
                }
                inflight.remove(&key);
            }
        }
        let date = t.get("date").and_then(|v| v.as_str()).unwrap_or("");
        let time_range = NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .ok()
            .and_then(|d| time_spec.as_ref().and_then(|ts| ts.hour_range_for_date(d)))
            .and_then(|(lo, hi)| {
                if lo == 0 && hi >= 24 {
                    None
                } else {
                    Some(format!("{:02}:00-{:02}:00", lo, hi.min(24)))
                }
            });
        let body = json!({
            "event_id": format!("{}:{}", event_id, cid),
            "watch_id": wid,
            "movie_id": watch.movie_id,
            "movie_name": watch.movie_name,
            "cinema_id": cid,
            "cinema_name": t.get("cinema_name").and_then(|v| v.as_str()).unwrap_or(cid),
            "show_date": date,
            "time_range": time_range,
            "num_seats": watch.lock_num_seats.max(1) as u64,
            "imax_only": watch.imax_only,
            "seats": watch.lock_seats_value(),
            "headless": settings.lock_headless,
            "callback_url": callback_url.as_str(),
            "source": "ticket-tracker",
        });
        state
            .lock_inflight
            .lock()
            .unwrap()
            .insert(key.clone(), now_secs());
        let mut req = state
            .http
            .post(format!("{}/lock", base))
            .timeout(Duration::from_secs(3))
            .json(&body);
        if let Some(token) = token {
            req = req.header("X-Agent-Token", token);
        }
        match req.send().await {
            Ok(r) if r.status().is_success() => {
                state.push_event(
                    "lock",
                    Some(wid),
                    format!("🎯 自动锁票已提交 agent（影院 {}）", cid),
                );
            }
            Ok(r) => {
                state.lock_inflight.lock().unwrap().remove(&key);
                state.push_event(
                    "lock",
                    Some(wid),
                    format!("✗ 自动锁票提交失败（Gofer HTTP {}）", r.status()),
                );
            }
            Err(e) => {
                state.lock_inflight.lock().unwrap().remove(&key);
                state.push_event("lock", Some(wid), format!("✗ 自动锁票提交失败：{}", e));
            }
        }
    }
}

/// agent 回调锁票结果后，沿该 watch 的渠道发结果通知。
pub async fn dispatch_lock_result(
    state: Arc<AppState>,
    watch_id: &str,
    title: &str,
    message: &str,
    buy_url: Option<String>,
) {
    let settings = match Settings::load(&state.db).await {
        Ok(s) => s,
        Err(_) => return,
    };
    if let Some(w) = settings.silent() {
        if w.contains(Local::now()) {
            return;
        }
    }
    let channels = match state.db.watch_channels(watch_id).await {
        Ok(c) => c,
        Err(_) => return,
    };
    let bindings = state
        .db
        .watch_channel_bindings(watch_id)
        .await
        .unwrap_or_default();
    let event = json!({
        "event_id": format!("lock_result_{}", &uuid::Uuid::new_v4().to_string()[..8]),
        "watch_id": watch_id,
        "type": "lock_result",
    });
    let mut set = tokio::task::JoinSet::new();
    for ch in channels.iter().filter(|c| c.channel_type != "autolock") {
        let st = state.clone();
        let ch = ch.clone();
        let cfg = effective_channel_config(&ch, bindings.get(&ch.id));
        let title = title.to_string();
        let message = message.to_string();
        let buy_url = buy_url.clone();
        let ev = event.clone();
        let settings = settings.clone();
        set.spawn(async move {
            let _ = deliver_channel(
                &st,
                &ch,
                &cfg,
                &title,
                &message,
                buy_url.as_deref(),
                &ev,
                &settings,
            )
            .await;
        });
    }
    while set.join_next().await.is_some() {}
}

#[cfg(test)]
mod tests {
    use super::effective_channel_config;
    use crate::db::ChannelRow;
    use serde_json::json;

    #[test]
    fn binding_config_overrides_global_defaults() {
        let ch = ChannelRow {
            id: "xhs-local".into(),
            name: "XHS".into(),
            channel_type: "xhs_local".into(),
            config_json: json!({"group": "default", "agent_url": "http://global:8790"}).to_string(),
            enabled: true,
            created_at: "2026-01-01 00:00:00".into(),
        };
        let cfg = effective_channel_config(&ch, Some(&json!({"group": "per-watch"})));
        assert_eq!(cfg["group"], "per-watch");
        assert_eq!(cfg["agent_url"], "http://global:8790");
    }
}
