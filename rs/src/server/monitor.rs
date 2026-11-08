//! DB-backed 监控循环。
//!
//! 复用 `engine` 的纯逻辑（compile / poll plan / fetch / evaluate），
//! 状态落在 SQLite，启动时自动执行 migrations。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{Local, TimeZone, Utc};
use serde_json::{json, Value};

use super::{now_secs, AppState, WatchStatus};
use crate::city::{self, CityPoll, CityShow};
use crate::db::{MonitorWatch, Settings, WatchCityStateRow};
use crate::engine::{self, WatchCinemaState};
use crate::notify;

enum CityFetch {
    Fired,
    MissingCityId,
    ShowDatesError {
        msg: String,
        duration_ms: u128,
    },
    Waiting {
        dates: Vec<String>,
        duration_ms: u128,
    },
    Shows {
        dates: Vec<String>,
        result: Result<CityPoll, String>,
        duration_ms: u128,
    },
}

type ShowDaysResult = (Result<Vec<String>, String>, u128);
type ShowDaysMap = HashMap<(u32, i64), ShowDaysResult>;

fn prev_city_stats(mw: &MonitorWatch) -> (Vec<String>, usize, usize, usize, usize) {
    let dates = mw
        .city_state
        .as_ref()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s.last_dates_json).ok())
        .unwrap_or_default();
    let cinema_count = mw
        .city_state
        .as_ref()
        .map(|s| s.last_cinema_count.max(0) as usize)
        .unwrap_or(0);
    let total = mw
        .city_state
        .as_ref()
        .map(|s| s.last_show_count.max(0) as usize)
        .unwrap_or(0);
    let imax = mw
        .city_state
        .as_ref()
        .map(|s| s.last_imax_count.max(0) as usize)
        .unwrap_or(0);
    let buckets = mw
        .city_state
        .as_ref()
        .map(|s| s.last_bucket_count.max(0) as usize)
        .unwrap_or(0);
    (dates, cinema_count, total, imax, buckets)
}

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
        let (city_watches, cinema_watches): (Vec<_>, Vec<_>) =
            monitor_watches.into_iter().partition(|w| w.row.is_city());
        let active: Vec<_> = cinema_watches
            .into_iter()
            .filter(|w| !w.cinemas.is_empty())
            .collect();

        if active.is_empty() && city_watches.is_empty() {
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

        // 每轮采样一次：影院 JSON + 全城扫描 JSON 统一计入。
        let cinema_ok = fetch.snapshots.len();
        let cinema_err = fetch.errors.len();
        let blocked_cinemas = fetch
            .errors
            .values()
            .filter(|err| looks_blocked(err))
            .count();
        let cinema_error_summary = if fetch.errors.is_empty() {
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

        // ------------------------- 全城开售 watch（一个 monitor 任务内并发探测） -------------------------
        // 1) 并发查唯一 (city, movie) 的 showdays；IMAX 变体共享结果。
        // 2) 有日期的 watch 并发查城市影院列表。
        // 3) 结果统一回到当前任务顺序落库/触发，避免共享状态竞态。
        let city_concurrency = settings.fetch_concurrency.clamp(1, 8);
        let mut city_ok = 0usize;
        let mut city_err = 0usize;
        let mut city_blocked = 0usize;
        let mut city_duration_sum_ms = 0u128;
        let mut city_errors: Vec<String> = Vec::new();

        let mut showdays_keys: HashSet<(u32, i64)> = HashSet::new();
        for mw in &city_watches {
            if mw.city_state.as_ref().is_some_and(|s| s.fired) {
                continue;
            }
            let city_id = mw.row.city_id.max(0) as u32;
            if city_id > 0 {
                showdays_keys.insert((city_id, mw.row.movie_id));
            }
        }
        let mut showdays: ShowDaysMap = HashMap::new();
        if !showdays_keys.is_empty() {
            let sem = Arc::new(tokio::sync::Semaphore::new(city_concurrency));
            let mut set = tokio::task::JoinSet::new();
            for (city_id, movie_id) in showdays_keys {
                let sem = sem.clone();
                set.spawn(async move {
                    let _permit = sem.acquire_owned().await.ok();
                    let start = Instant::now();
                    let res = crate::maoyan::fetch_city_show_dates_async(movie_id, city_id)
                        .await
                        .map_err(|e| e.to_string());
                    ((city_id, movie_id), res, start.elapsed().as_millis())
                });
            }
            while let Some(joined) = set.join_next().await {
                if let Ok((key, res, ms)) = joined {
                    showdays.insert(key, (res, ms));
                }
            }
        }
        for ((city_id, _), (res, ms)) in &showdays {
            city_duration_sum_ms += *ms;
            match res {
                Ok(_) => city_ok += 1,
                Err(e) => {
                    city_err += 1;
                    if looks_blocked(e) {
                        city_blocked += 1;
                    }
                    city_errors.push(format!("city {}: {}", city_id, e));
                }
            }
        }

        let mut outcomes: Vec<Option<CityFetch>> = (0..city_watches.len()).map(|_| None).collect();
        let mut list_jobs: Vec<(usize, u32, i64, Vec<String>, bool)> = Vec::new();
        for (idx, mw) in city_watches.iter().enumerate() {
            if mw.city_state.as_ref().is_some_and(|s| s.fired) {
                outcomes[idx] = Some(CityFetch::Fired);
                continue;
            }
            let city_id = mw.row.city_id.max(0) as u32;
            if city_id == 0 {
                outcomes[idx] = Some(CityFetch::MissingCityId);
                continue;
            }
            match showdays.get(&(city_id, mw.row.movie_id)) {
                Some((Ok(dates), ms)) if dates.is_empty() => {
                    outcomes[idx] = Some(CityFetch::Waiting {
                        dates: dates.clone(),
                        duration_ms: *ms,
                    });
                }
                Some((Ok(dates), _)) => {
                    list_jobs.push((
                        idx,
                        city_id,
                        mw.row.movie_id,
                        dates.clone(),
                        mw.row.imax_only,
                    ));
                }
                Some((Err(e), ms)) => {
                    outcomes[idx] = Some(CityFetch::ShowDatesError {
                        msg: e.clone(),
                        duration_ms: *ms,
                    });
                }
                None => {
                    city_err += 1;
                    city_errors.push(format!("city {}: showdays 任务异常", city_id));
                    outcomes[idx] = Some(CityFetch::ShowDatesError {
                        msg: "showdays 任务异常".into(),
                        duration_ms: 0,
                    });
                }
            }
        }

        if !list_jobs.is_empty() {
            let sem = Arc::new(tokio::sync::Semaphore::new(city_concurrency));
            let mut set = tokio::task::JoinSet::new();
            for (idx, city_id, movie_id, dates, imax_only) in list_jobs {
                let sem = sem.clone();
                set.spawn(async move {
                    let _permit = sem.acquire_owned().await.ok();
                    let start = Instant::now();
                    let res =
                        crate::maoyan::fetch_city_shows_async(movie_id, city_id, &dates, imax_only)
                            .await
                            .map_err(|e| e.to_string());
                    (idx, dates, res, start.elapsed().as_millis())
                });
            }
            while let Some(joined) = set.join_next().await {
                if let Ok((idx, dates, res, ms)) = joined {
                    match &res {
                        Ok(poll) => {
                            city_ok += poll.requests.max(1);
                            city_duration_sum_ms += ms;
                        }
                        Err(e) => {
                            city_err += 1;
                            city_duration_sum_ms += ms;
                            if looks_blocked(e) {
                                city_blocked += 1;
                            }
                            city_errors
                                .push(format!("city {}: {}", city_watches[idx].row.city_id, e));
                        }
                    }
                    outcomes[idx] = Some(CityFetch::Shows {
                        dates,
                        result: res,
                        duration_ms: ms,
                    });
                }
            }
        }

        // 3) 顺序处理结果：DB/状态/触发；网络失败只影响自己的 watch。
        for (idx, mw) in city_watches.iter().enumerate() {
            let wid = mw.row.id.clone();
            let now = now_secs();
            let (prev_dates, prev_cinemas, prev_total, prev_imax, prev_buckets) =
                prev_city_stats(mw);
            let outcome = match outcomes[idx].take() {
                Some(o) => o,
                None => {
                    city_err += 1;
                    let msg = "城市影院任务异常".to_string();
                    city_errors.push(format!("city {}: {}", mw.row.city_id, msg));
                    CityFetch::Shows {
                        dates: prev_dates.clone(),
                        result: Err(msg),
                        duration_ms: 0,
                    }
                }
            };

            match outcome {
                CityFetch::Fired => {
                    if let Some(cs) = mw.city_state.as_ref() {
                        state.set_watch_status(&wid, city_fired_status(cs, now));
                    }
                }
                CityFetch::MissingCityId => {
                    let msg = "全城 watch 缺少 city_id";
                    city_errors.push(format!("watch {}: {}", wid, msg));
                    let _ = state
                        .db
                        .record_city_poll(
                            &wid,
                            false,
                            Some(msg),
                            &prev_dates,
                            prev_cinemas,
                            prev_total,
                            prev_imax,
                            prev_buckets,
                            0,
                        )
                        .await;
                    set_city_error(&state, &wid, msg, now);
                }
                CityFetch::ShowDatesError { msg, duration_ms } => {
                    let _ = state
                        .db
                        .record_city_poll(
                            &wid,
                            false,
                            Some(&msg),
                            &prev_dates,
                            prev_cinemas,
                            prev_total,
                            prev_imax,
                            prev_buckets,
                            duration_ms,
                        )
                        .await;
                    set_city_error(&state, &wid, &msg, now);
                }
                CityFetch::Waiting { dates, duration_ms } => {
                    let _ = state
                        .db
                        .record_city_poll(&wid, true, None, &dates, 0, 0, 0, 0, duration_ms)
                        .await;
                    set_city_waiting(&state, &wid, now);
                    any_success = true;
                    let due = now - last_sample.get(&wid).copied().unwrap_or(0.0) >= 300.0;
                    if due {
                        let _ = state.db.insert_metric(&wid, 0, 0, 0, 0).await;
                        last_sample.insert(wid.clone(), now);
                    }
                }
                CityFetch::Shows {
                    dates,
                    result,
                    duration_ms,
                } => match result {
                    Err(msg) => {
                        let _ = state
                            .db
                            .record_city_poll(
                                &wid,
                                false,
                                Some(&msg),
                                &dates,
                                prev_cinemas,
                                prev_total,
                                prev_imax,
                                prev_buckets,
                                duration_ms,
                            )
                            .await;
                        set_city_error(&state, &wid, &msg, now);
                    }
                    Ok(poll) => {
                        let (total, imax, buckets) = city::summarize(&poll.shows);
                        let cinema_count = city_cinema_count(&poll.shows);
                        let per_request_ms = if poll.requests > 0 {
                            duration_ms / poll.requests as u128
                        } else {
                            duration_ms
                        };
                        let _ = state
                            .db
                            .record_city_poll(
                                &wid,
                                true,
                                None,
                                &poll.dates,
                                cinema_count,
                                total,
                                imax,
                                buckets,
                                per_request_ms,
                            )
                            .await;
                        any_success = true;

                        if poll.shows.is_empty() {
                            set_city_waiting(&state, &wid, now);
                            let due = now - last_sample.get(&wid).copied().unwrap_or(0.0) >= 300.0;
                            if due {
                                let _ = state.db.insert_metric(&wid, 0, 0, 0, 0).await;
                                last_sample.insert(wid.clone(), now);
                            }
                            continue;
                        }

                        let Some(best) = city::choose_earliest(&poll.shows) else {
                            continue;
                        };
                        let marked = match state
                            .db
                            .mark_city_watch_fired(
                                &wid,
                                best,
                                &poll.dates,
                                cinema_count,
                                total,
                                imax,
                                buckets,
                            )
                            .await
                        {
                            Ok(v) => v,
                            Err(e) => {
                                let msg = format!("写入开售状态失败：{}", e);
                                city_errors.push(format!("watch {}: {}", wid, msg));
                                set_city_error(&state, &wid, &msg, now);
                                continue;
                            }
                        };
                        if !marked {
                            if let Ok(Some(cs)) = state.db.city_state(&wid).await {
                                state.set_watch_status(&wid, city_fired_status(&cs, now));
                            }
                            continue;
                        }

                        let _ = state
                            .db
                            .insert_metric(&wid, total, imax, buckets, total)
                            .await;
                        last_sample.insert(wid.clone(), now);
                        state.set_watch_status(
                            &wid,
                            WatchStatus {
                                status: "fired".into(),
                                last_checked_at: Some(now),
                                last_trigger_at: Some(now),
                                total_shows: total,
                                imax_shows: imax,
                                error: None,
                            },
                        );
                        state.push_event(
                            "trigger",
                            Some(&wid),
                            format!(
                                "全城开售｜{}｜{}｜{} {}",
                                mw.row.city_name, best.cinema_name, best.date, best.show_time
                            ),
                        );
                        let event = build_city_event(mw, best, &poll.shows);
                        let st = state.clone();
                        tokio::spawn(async move {
                            let ev = event.clone();
                            let lock_st = st.clone();
                            let lock_ev = ev.clone();
                            let lock_task = tokio::spawn(async move {
                                super::dispatcher::maybe_dispatch_autolock(lock_st, &lock_ev).await;
                            });
                            super::dispatcher::dispatch_trigger(st, ev).await;
                            let _ = lock_task.await;
                        });
                    }
                },
            }
        }

        health_issues.extend(city_errors.iter().map(|e| format!("全城扫描：{}", e)));

        let city_json_total = city_ok + city_err;
        let city_avg_ms = if city_json_total > 0 {
            city_duration_sum_ms / city_json_total as u128
        } else {
            0
        };
        let json_total = cinema_ok + cinema_err + city_ok + city_err;
        if json_total > 0 {
            let mut error_parts: Vec<String> = Vec::new();
            if let Some(s) = cinema_error_summary.as_deref() {
                error_parts.push(s.to_string());
            }
            error_parts.extend(city_errors.iter().take(3).cloned());
            let error_summary = if error_parts.is_empty() {
                None
            } else {
                Some(error_parts.join("; "))
            };
            let _ = state
                .db
                .insert_poll_health_full(
                    cinema_ok,
                    cinema_err,
                    blocked_cinemas,
                    per_poll_ms,
                    city_ok,
                    city_err,
                    city_blocked,
                    city_avg_ms,
                    error_summary.as_deref(),
                )
                .await;
        }

        let mut round_issues = health_issues.clone();
        if let Some(e) = state.delivery_error(600.0) {
            round_issues.push(format!("投递失败：{}", e));
        }
        let healthy = round_issues.is_empty();
        {
            let mut h = state.health.lock().unwrap();
            h.last_poll_at = Some(now_secs());
            h.next_poll_at = Some(now_secs() + settings.check_interval as f64);
            h.fetch_ok = cinema_ok + city_ok;
            h.fetch_err = cinema_err + city_err;
            if any_success {
                h.check_count += 1;
            }
            h.health_ok = healthy;
            h.last_error = round_issues.first().cloned();
            if healthy {
                h.consecutive_failures = 0;
            } else {
                h.consecutive_failures = h.consecutive_failures.saturating_add(1);
            }
        }

        if health_prev && !healthy && !round_issues.is_empty() {
            state.push_event(
                "health",
                None,
                format!("⚠ 健康检查异常：{}", round_issues[0]),
            );
            if let Some(webhook) = settings.discord_webhook.as_deref() {
                let mut body = format!(
                    "基础 JSON 获取 {}/{} 成功\n\n",
                    cinema_ok + city_ok,
                    cinema_ok + cinema_err + city_ok + city_err
                );
                for line in round_issues.iter().take(5) {
                    body.push_str(line);
                    body.push('\n');
                }
                if round_issues.len() > 5 {
                    body.push_str(&format!("…另有 {} 条问题\n", round_issues.len() - 5));
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

fn parse_sqlite_ts(s: Option<&str>) -> Option<f64> {
    let s = s?;
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|ndt| Utc.from_utc_datetime(&ndt).timestamp() as f64)
}

fn city_fired_status(cs: &WatchCityStateRow, now: f64) -> WatchStatus {
    WatchStatus {
        status: "fired".into(),
        last_checked_at: parse_sqlite_ts(cs.last_polled_at.as_deref()).or(Some(now)),
        last_trigger_at: parse_sqlite_ts(cs.fired_at.as_deref()),
        total_shows: cs.last_show_count.max(0) as usize,
        imax_shows: cs.last_imax_count.max(0) as usize,
        error: None,
    }
}

fn set_city_waiting(state: &Arc<AppState>, wid: &str, now: f64) {
    let prev = state.watch_status_snapshot();
    let prev = prev.get(wid);
    state.set_watch_status(
        wid,
        WatchStatus {
            status: "waiting".into(),
            last_checked_at: Some(now),
            last_trigger_at: prev.and_then(|s| s.last_trigger_at),
            total_shows: 0,
            imax_shows: 0,
            error: None,
        },
    );
}

fn set_city_error(state: &Arc<AppState>, wid: &str, err: &str, now: f64) {
    let prev = state.watch_status_snapshot();
    let prev = prev.get(wid);
    let was_error = prev.map(|s| s.status == "error").unwrap_or(false);
    let last_trigger = prev.and_then(|s| s.last_trigger_at);
    let total_shows = prev.map(|s| s.total_shows).unwrap_or(0);
    let imax_shows = prev.map(|s| s.imax_shows).unwrap_or(0);
    state.set_watch_status(
        wid,
        WatchStatus {
            status: "error".into(),
            last_checked_at: Some(now),
            last_trigger_at: last_trigger,
            total_shows,
            imax_shows,
            error: Some(err.to_string()),
        },
    );
    if !was_error {
        state.push_event("watch", Some(wid), format!("全城 watch 探测失败：{}", err));
    }
}

fn city_cinema_count(shows: &[CityShow]) -> usize {
    shows
        .iter()
        .map(|s| s.cinema_id.as_str())
        .collect::<HashSet<_>>()
        .len()
}

fn build_city_event(mw: &crate::db::MonitorWatch, best: &CityShow, shows: &[CityShow]) -> Value {
    let movie_name = if mw.row.movie_name.is_empty() {
        mw.row.movie_id.to_string()
    } else {
        mw.row.movie_name.clone()
    };
    let city_name = if mw.row.city_name.trim().is_empty() {
        crate::cities::resolve_name(mw.row.city_id.max(0) as u32)
    } else {
        mw.row.city_name.clone()
    };
    let hour = best.hour().unwrap_or(0);
    let added: Vec<Value> = shows
        .iter()
        .filter(|s| s.cinema_id == best.cinema_id && s.date == best.date)
        .map(|s| {
            json!({
                "tm": s.show_time,
                "th": s.hall,
                "tp": s.format,
                "is_imax": s.is_imax,
                "seq_no": s.seq_no,
            })
        })
        .collect();
    let trigger = json!({
        "cinema_id": best.cinema_id,
        "cinema_name": best.cinema_name,
        "date": best.date,
        "hour": hour,
        "show_time": best.show_time,
        "seq_no": best.seq_no,
        "is_imax": best.is_imax,
        "hall": best.hall,
        "format": best.format,
        "added": added,
    });

    let mut desc = format!(
        "{}｜{}｜{} {}",
        city_name, best.cinema_name, best.date, best.show_time
    );
    if !best.hall.is_empty() {
        desc.push(' ');
        desc.push_str(&best.hall);
    }
    if !best.format.is_empty() {
        desc.push(' ');
        desc.push_str(&best.format);
    }
    if best.is_imax && !desc.to_ascii_uppercase().contains("IMAX") {
        desc.push_str(" [IMAX]");
    }

    json!({
        "schema_version": 1,
        "event_id": format!("evt_{}_{}", &uuid::Uuid::new_v4().to_string()[..8], now_secs() as u64),
        "type": "watch_triggered",
        "scope": "city",
        "trigger_kind": "presale_open",
        "watch_id": mw.row.id,
        "movie_id": mw.row.movie_id,
        "movie_name": movie_name,
        "city_id": mw.row.city_id,
        "city_name": city_name,
        "time_spec": {"type":"all"},
        "imax_only": mw.row.imax_only,
        "title": format!("🎟 {} 全城开售", movie_name),
        "message": desc,
        "first_trigger": trigger.clone(),
        "triggers": [trigger],
        "detected_at": Local::now().to_rfc3339(),
    })
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
