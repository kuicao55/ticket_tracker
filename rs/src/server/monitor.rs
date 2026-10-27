//! DB-backed 监控循环。
//!
//! 复用 `engine` 的纯逻辑（compile / poll plan / fetch / evaluate），
//! 状态落在 SQLite，启动时自动执行 migrations。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::Local;
use serde_json::{json, Value};

use super::{now_secs, AppState, WatchStatus};
use crate::db::Settings;
use crate::engine::{self, WatchCinemaState};
use crate::notify;

pub fn spawn(state: Arc<AppState>) {
    tokio::spawn(async move {
        if let Err(e) = run(state.clone()).await {
            state.push_event("error", None, format!("监控循环退出: {}", e));
            tracing::error!("monitor loop exited: {}", e);
        }
    });
}

async fn run(state: Arc<AppState>) -> Result<()> {
    let mut last_sample: HashMap<String, f64> = HashMap::new();
    let mut health_prev = true;
    let mut rounds: u64 = 0;

    loop {
        let settings = Settings::load(&state.db).await?;
        let now = Local::now();
        let paused = !settings.monitor_enabled;
        let silent = settings.silent().map(|w| w.contains(now)).unwrap_or(false);

        {
            let mut h = state.health.lock().unwrap();
            h.paused = paused;
            h.silent = silent;
            if paused {
                h.next_poll_at = None;
            } else if silent {
                h.next_poll_at = Some(now_secs() + 15.0);
            }
        }

        if paused {
            sleep_or_wake(&state, Duration::from_secs(5)).await;
            continue;
        }

        if silent {
            sleep_or_wake(&state, Duration::from_secs(15)).await;
            continue;
        }

        let round_start = Instant::now();
        let monitor_watches = state.db.load_monitor_watches().await?;
        let active: Vec<_> = monitor_watches
            .into_iter()
            .filter(|w| !w.cinemas.is_empty())
            .collect();

        if active.is_empty() {
            {
                let mut h = state.health.lock().unwrap();
                h.last_poll_at = Some(now_secs());
                h.next_poll_at = Some(now_secs() + settings.check_interval as f64);
                h.fetch_ok = 0;
                h.fetch_err = 0;
                h.health_ok = true;
                h.last_error = None;
            }
            sleep_or_wake(&state, Duration::from_secs(settings.check_interval)).await;
            continue;
        }

        let engine_watches: Vec<Value> = active.iter().map(|w| w.to_engine_value()).collect();
        let plan = engine::build_poll_plan(&engine_watches);
        let plan_pairs: Vec<(String, i64)> = plan
            .polls
            .iter()
            .map(|(cid, node)| (cid.clone(), node.watches.len() as i64))
            .collect();
        state.db.sync_polls(&plan_pairs).await?;

        let fetch_start = Instant::now();
        let fetch = engine::fetch_plan_live(&plan, settings.fetch_concurrency).await;
        let per_poll_ms = if plan.polls.is_empty() {
            fetch_start.elapsed().as_millis()
        } else {
            fetch_start.elapsed().as_millis() / plan.polls.len() as u128
        };

        // 每轮 poll 采样一次：成功率、单影院平均速度、疑似风控的影院数。
        if !plan.polls.is_empty() {
            let blocked_cinemas = fetch
                .errors
                .values()
                .filter(|err| looks_blocked(err))
                .count();
            let error_summary = if fetch.errors.is_empty() {
                None
            } else {
                Some(
                    fetch
                        .errors
                        .iter()
                        .take(3)
                        .map(|(cid, err)| format!("{}: {}", cid, err))
                        .collect::<Vec<_>>()
                        .join("; "),
                )
            };
            let _ = state
                .db
                .insert_poll_health(
                    fetch.snapshots.len(),
                    fetch.errors.len(),
                    blocked_cinemas,
                    per_poll_ms,
                    error_summary.as_deref(),
                )
                .await;
        }

        for (cid, snap) in &fetch.snapshots {
            let shows: usize = snap
                .movies
                .iter()
                .flat_map(|m| m.buckets.iter())
                .map(|b| b.total as usize)
                .sum();
            let _ = state
                .db
                .record_poll(cid, &snap.cinema_name, true, None, shows, per_poll_ms)
                .await;
        }
        for (cid, err) in &fetch.errors {
            let _ = state
                .db
                .record_poll(cid, "", false, Some(err), 0, per_poll_ms)
                .await;
        }

        let mut health_issues: Vec<String> = fetch
            .errors
            .iter()
            .map(|(cid, err)| format!("影院 {} 抓取失败：{}", cid, err))
            .collect();
        let mut any_success = false;

        for mw in &active {
            let wid = mw.row.id.clone();
            let compiled = match engine::compile_watch(&mw.to_engine_value()) {
                Ok(c) => c,
                Err(e) => {
                    health_issues.push(format!("watch {} 配置错误：{}", wid, e));
                    state.set_watch_status(
                        &wid,
                        WatchStatus {
                            status: "error".into(),
                            last_checked_at: Some(now_secs()),
                            error: Some(e.to_string()),
                            ..Default::default()
                        },
                    );
                    continue;
                }
            };

            let mut states_to_save: Vec<(String, Value)> = Vec::new();
            let mut trigger_groups: Vec<Value> = Vec::new();
            let mut lines: Vec<String> = Vec::new();
            let mut added_total = 0usize;
            let mut triggered = false;
            let mut success = false;
            let mut first_error: Option<String> = None;
            let mut init_count = 0usize;
            let mut total_shows = 0usize;
            let mut imax_shows = 0usize;
            let mut bucket_count = 0usize;

            for cid in &mw.cinemas {
                let Some(snap) = fetch.snapshots.get(cid) else {
                    if let Some(err) = fetch.errors.get(cid) {
                        first_error = Some(format!("{}: {}", cid, err));
                    }
                    continue;
                };
                success = true;
                let prev = mw.state.get(cid).cloned().unwrap_or(Value::Null);
                let (new_state, out) = engine::evaluate_cinema(&compiled, snap, &prev);

                if out.initialized {
                    init_count += 1;
                }
                if out.state_changed {
                    states_to_save.push((cid.clone(), new_state.clone()));
                }

                let st = WatchCinemaState::from_json(&new_state);
                for b in st.buckets.values() {
                    total_shows += b.count as usize;
                    if mw.row.imax_only {
                        imax_shows += b.count as usize;
                    } else {
                        imax_shows += b.shows.iter().filter(|s| s.is_imax).count();
                    }
                    bucket_count += 1;
                }

                if !out.added.is_empty() {
                    triggered = true;
                    for d in &out.added {
                        added_total += d.added.len();
                        let labels = d
                            .added
                            .iter()
                            .map(|s| s.label())
                            .collect::<Vec<_>>()
                            .join("、");
                        let line = format!(
                            "{}｜{} {:02}:00 新增 {} 场：{}",
                            snap.cinema_name,
                            d.date,
                            d.hour,
                            d.added.len(),
                            labels
                        );
                        lines.push(line.clone());
                        trigger_groups.push(json!({
                            "cinema_id": cid,
                            "cinema_name": snap.cinema_name,
                            "date": d.date,
                            "hour": d.hour,
                            "added": d.added.iter().map(|s| s.to_json()).collect::<Vec<_>>(),
                        }));
                    }
                }
            }

            state.db.save_watch_states(&wid, &states_to_save).await?;

            let now = now_secs();
            let changed = !states_to_save.is_empty();
            let due = now - last_sample.get(&wid).copied().unwrap_or(0.0) >= 300.0;
            if success && (changed || due) {
                let _ = state
                    .db
                    .insert_metric(&wid, total_shows, imax_shows, bucket_count, added_total)
                    .await;
                last_sample.insert(wid.clone(), now);
            }

            let status = if triggered {
                "triggered"
            } else if !success {
                "error"
            } else if total_shows == 0 {
                "no_movie"
            } else {
                "monitoring"
            };
            let prev_trigger = state
                .watch_status_snapshot()
                .get(&wid)
                .and_then(|s| s.last_trigger_at);
            state.set_watch_status(
                &wid,
                WatchStatus {
                    status: status.into(),
                    last_checked_at: Some(now),
                    last_trigger_at: if triggered { Some(now) } else { prev_trigger },
                    total_shows,
                    imax_shows,
                    error: if success { None } else { first_error.clone() },
                },
            );

            if success {
                any_success = true;
            } else if let Some(e) = &first_error {
                health_issues.push(format!("watch {}：{}", wid, e));
            }

            if init_count > 0 && !triggered {
                state.push_event(
                    "watch",
                    Some(&wid),
                    format!("{} 建立基线（{} 个影院）", mw.row.movie_name, init_count),
                );
            }

            if triggered {
                let movie_name = if mw.row.movie_name.is_empty() {
                    mw.row.movie_id.to_string()
                } else {
                    mw.row.movie_name.clone()
                };
                let title = format!("🎟 {} 新增场次", movie_name);
                let message = lines.join("\n");
                state.push_event(
                    "trigger",
                    Some(&wid),
                    format!("{}｜{}", title, lines.join("；")),
                );
                let event_id = format!(
                    "evt_{}_{}",
                    &uuid::Uuid::new_v4().to_string()[..8],
                    now as u64
                );
                let event = json!({
                    "schema_version": 1,
                    "event_id": event_id,
                    "type": "watch_triggered",
                    "watch_id": wid,
                    "movie_id": mw.row.movie_id,
                    "movie_name": movie_name,
                    "time_spec": mw.row.time_spec_value(),
                    "imax_only": mw.row.imax_only,
                    "title": title,
                    "message": message,
                    "triggers": trigger_groups,
                    "detected_at": Local::now().to_rfc3339(),
                });
                let st = state.clone();
                tokio::spawn(async move {
                    let ev = event.clone();
                    // 锁票提交不要排在通知 fan-out 后面：两者并发。
                    let lock_st = st.clone();
                    let lock_ev = ev.clone();
                    let lock_task = tokio::spawn(async move {
                        super::dispatcher::maybe_dispatch_autolock(lock_st, &lock_ev).await;
                    });
                    super::dispatcher::dispatch_trigger(st, ev).await;
                    let _ = lock_task.await;
                });
            }
        }

        let healthy = health_issues.is_empty();
        {
            let mut h = state.health.lock().unwrap();
            h.last_poll_at = Some(now_secs());
            h.next_poll_at = Some(now_secs() + settings.check_interval as f64);
            h.fetch_ok = fetch.snapshots.len();
            h.fetch_err = fetch.errors.len();
            if any_success {
                h.check_count += 1;
            }
            h.health_ok = healthy;
            h.last_error = health_issues.first().cloned();
            if healthy {
                h.consecutive_failures = 0;
            } else {
                h.consecutive_failures = h.consecutive_failures.saturating_add(1);
            }
        }

        if health_prev && !healthy && !health_issues.is_empty() {
            state.push_event(
                "health",
                None,
                format!("⚠ 健康检查异常：{}", health_issues[0]),
            );
            if let Some(webhook) = settings.discord_webhook.as_deref() {
                let mut body = format!(
                    "影院抓取 {}/{} 成功\n\n",
                    fetch.snapshots.len(),
                    fetch.snapshots.len() + fetch.errors.len()
                );
                for line in health_issues.iter().take(5) {
                    body.push_str(line);
                    body.push('\n');
                }
                if health_issues.len() > 5 {
                    body.push_str(&format!("…另有 {} 条问题\n", health_issues.len() - 5));
                }
                body.push_str(&format!(
                    "\n时间：{}",
                    Local::now().format("%Y-%m-%d %H:%M:%S")
                ));
                let _ = notify::notify_discord_async(
                    Some(webhook),
                    "⚠ ticket-tracker 健康检查异常",
                    &body,
                    None,
                )
                .await;
            }
        }
        health_prev = healthy;

        rounds += 1;
        if rounds.is_multiple_of(60) {
            let _ = state
                .db
                .prune_metrics(settings.metrics_retention_days)
                .await;
            let _ = state
                .db
                .prune_poll_health(settings.metrics_retention_days)
                .await;
        }

        let elapsed = round_start.elapsed().as_secs();
        let wait = settings.check_interval.saturating_sub(elapsed).max(1);
        sleep_or_wake(&state, Duration::from_secs(wait)).await;
    }
}

async fn sleep_or_wake(state: &AppState, d: Duration) {
    tokio::select! {
        _ = tokio::time::sleep(d) => {}
        _ = state.wake.notified() => {}
    }
}

/// 猫眼风控/封 IP 的常见表现。只是给面板分类，不影响抓取结果。
fn looks_blocked(err: &str) -> bool {
    let e = err.to_lowercase();
    [
        "406",
        "403",
        "captcha",
        "verify.meituan",
        "spiderindefence",
        "风控",
        "forbidden",
        "访问受限",
        "拒绝访问",
    ]
    .iter()
    .any(|needle| e.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::looks_blocked;

    #[test]
    fn classifies_captcha_and_406_as_blocked() {
        assert!(looks_blocked("猫眼接口异常（code=406）"));
        assert!(looks_blocked("captcha required"));
        assert!(looks_blocked("https://verify.meituan.com/..."));
        assert!(looks_blocked("触发风控"));
        assert!(!looks_blocked("请求超时"));
        assert!(!looks_blocked("connection reset"));
    }
}
