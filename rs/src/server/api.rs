//! /api/v1：面板与外部系统使用的 JSON API。

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{now_secs, AppState};
use crate::db::{NewWatch, Settings, WatchPatch};
use crate::lock::{self, LockResult};
use crate::maoyan;
use crate::service;
use crate::time_spec::TimeSpec;

type ApiError = (StatusCode, Json<Value>);
type ApiResult<T> = Result<T, ApiError>;

fn err(status: StatusCode, msg: impl Into<String>) -> ApiError {
    (status, Json(json!({ "error": msg.into() })))
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

fn bad(e: impl std::fmt::Display) -> ApiError {
    err(StatusCode::BAD_REQUEST, e.to_string())
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        // 公开健康检查：iGlance manifest 里的 endpoints.health。
        .route("/health", get(health))
        .route("/api/v1/health", get(health))
        // iGlance service.v1；/v1/service* 是 /api/v1/service* 的别名。
        .route("/api/v1/service", get(service_status))
        .route("/v1/service", get(service_status))
        .route("/api/v1/service/memory", get(service_memory))
        .route("/v1/service/memory", get(service_memory))
        .route("/api/v1/service/pause", post(service_pause))
        .route("/v1/service/pause", post(service_pause))
        .route("/api/v1/service/resume", post(service_resume))
        .route("/v1/service/resume", post(service_resume))
        .route("/api/v1/service/toggle", post(service_toggle))
        .route("/v1/service/toggle", post(service_toggle))
        .route("/api/v1/service/stop", post(service_stop))
        .route("/v1/service/stop", post(service_stop))
        .route("/api/v1/dashboard", get(dashboard))
        .route("/api/v1/polls", get(list_polls))
        .route("/api/v1/watches", get(list_watches).post(create_watch))
        .route(
            "/api/v1/watches/:id",
            get(get_watch).patch(update_watch).delete(delete_watch),
        )
        .route(
            "/api/v1/watches/:id/channels",
            get(get_watch_channels).put(put_watch_channels),
        )
        .route("/api/v1/watches/:id/metrics", get(watch_metrics))
        .route("/api/v1/channels", get(list_channels).post(create_channel))
        .route(
            "/api/v1/channels/:id",
            patch(update_channel).delete(delete_channel),
        )
        .route("/api/v1/users", get(list_users).post(create_user))
        .route("/api/v1/users/:id", delete(delete_user))
        .route("/api/v1/users/:id/subscriptions", get(user_subscriptions))
        .route(
            "/api/v1/subscriptions",
            get(list_subscriptions).post(create_subscription),
        )
        .route("/api/v1/subscriptions/:id", delete(delete_subscription))
        .route("/api/v1/settings", get(get_settings).patch(patch_settings))
        .route("/api/v1/movies", get(list_movies))
        .route("/api/v1/movies/search", get(search_movies_api))
        .route("/api/v1/movies/refresh", post(refresh_movies))
        .route("/api/v1/cinemas/search", get(search_cinemas_api))
        .route("/api/v1/events", get(list_events))
        .route("/api/v1/agent/lock-result", post(agent_lock_result))
        .route("/api/v1/auth/wx/login", post(wx_login_stub))
        .with_state(state)
}

// ------------------------- health / dashboard -------------------------

async fn health(State(st): State<Arc<AppState>>) -> impl IntoResponse {
    let h = st.health_snapshot();
    let settings = Settings::load(&st.db).await.ok();
    let db_ok = sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&st.db.pool)
        .await
        .is_ok();
    Json(json!({
        "ok": db_ok && h.health_ok,
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_sec": (now_secs() - st.started_at).max(0.0) as u64,
        "last_poll_at": h.last_poll_at,
        "next_poll_at": h.next_poll_at,
        "check_interval": settings.as_ref().map(|s| s.check_interval),
        "fetch_ok": h.fetch_ok,
        "fetch_err": h.fetch_err,
        "silent": h.silent,
        "paused": h.paused,
        "check_count": h.check_count,
        "consecutive_failures": h.consecutive_failures,
        "last_error": h.last_error,
        "db_ok": db_ok,
        "delivery": "fire-and-forget"
    }))
}

async fn dashboard(State(st): State<Arc<AppState>>) -> ApiResult<Json<Value>> {
    let polls = st.db.poll_count().await.map_err(internal)?;
    let watches = st.db.watch_count().await.map_err(internal)?;
    let users = st.db.user_count().await.map_err(internal)?;
    let subscriptions = st.db.subscription_count().await.map_err(internal)?;
    let top_watches = st
        .db
        .list_watches("subscribers", 8, 0)
        .await
        .map_err(internal)?;
    let top_polls = st.db.list_polls(8).await.map_err(internal)?;
    let settings = Settings::load(&st.db).await.map_err(internal)?;
    let health = st.health_snapshot();
    let events = st.events_snapshot(20);
    Ok(Json(json!({
        "counts": {
            "polls": polls, "watches": watches, "users": users,
            "subscriptions": subscriptions,
        },
        "interval": settings.check_interval,
        "silent_window": settings.silent_window,
        "health": health,
        "top_watches": top_watches,
        "top_polls": top_polls,
        "events": events,
    })))
}

// ------------------------- polls -------------------------

#[derive(Debug, Deserialize)]
struct LimitQuery {
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
    #[serde(default)]
    sort: Option<String>,
}

fn default_limit() -> i64 {
    50
}

async fn list_polls(
    State(st): State<Arc<AppState>>,
    Query(q): Query<LimitQuery>,
) -> ApiResult<Json<Value>> {
    let rows = st
        .db
        .list_polls(q.limit.clamp(1, 500))
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "polls": rows })))
}

// ------------------------- watches -------------------------

async fn list_watches(
    State(st): State<Arc<AppState>>,
    Query(q): Query<LimitQuery>,
) -> ApiResult<Json<Value>> {
    let rows = st
        .db
        .list_watches(
            q.sort.as_deref().unwrap_or("subscribers"),
            q.limit.clamp(1, 500),
            q.offset.max(0),
        )
        .await
        .map_err(internal)?;
    let mut out = Vec::with_capacity(rows.len());
    for w in rows {
        let cinemas = st.db.watch_cinemas(&w.id).await.map_err(internal)?;
        out.push(json!({
            "id": w.id,
            "movie_id": w.movie_id,
            "movie_name": w.movie_name,
            "cinemas": cinemas,
            "time_spec": w.time_spec_value(),
            "imax_only": w.imax_only,
            "enabled": w.enabled,
            "created_by": w.created_by,
            "visibility": w.visibility,
            "auto_lock": w.auto_lock,
            "lock_num_seats": w.lock_num_seats,
            "lock_max_retries": w.lock_max_retries,
            "lock_seats": w.lock_seats_value(),
            "subscriber_count": w.subscriber_count,
            "created_at": w.created_at,
            "updated_at": w.updated_at,
        }));
    }
    Ok(Json(json!({ "watches": out })))
}

#[derive(Debug, Deserialize)]
struct CreateWatchRequest {
    movie_id: i64,
    #[serde(default)]
    movie_name: Option<String>,
    cinemas: Vec<String>,
    #[serde(default)]
    time: Option<String>,
    #[serde(default)]
    time_spec: Option<Value>,
    #[serde(default)]
    imax_only: bool,
    #[serde(default)]
    created_by: Option<String>,
    #[serde(default)]
    visibility: Option<String>,
    #[serde(default)]
    notify: Option<Vec<String>>,
    #[serde(default)]
    auto_lock: bool,
    #[serde(default)]
    lock_num_seats: Option<u64>,
    #[serde(default)]
    lock_max_retries: Option<u64>,
    #[serde(default)]
    lock_seats: Option<Vec<String>>,
}

async fn create_watch(
    State(st): State<Arc<AppState>>,
    Json(req): Json<CreateWatchRequest>,
) -> ApiResult<Json<Value>> {
    let time_spec = match req.time_spec {
        Some(v) => TimeSpec::from_json(&v).map_err(bad)?,
        None => TimeSpec::parse(req.time.as_deref().unwrap_or("")).map_err(bad)?,
    };
    let spec = NewWatch {
        movie_id: req.movie_id,
        movie_name: req.movie_name,
        cinemas: req.cinemas,
        time_spec: time_spec.to_json(),
        imax_only: req.imax_only,
        created_by: req.created_by,
        visibility: req.visibility.unwrap_or_else(|| "public".into()),
        auto_lock: req.auto_lock,
        lock_num_seats: req
            .lock_num_seats
            .unwrap_or(crate::db::DEFAULT_LOCK_NUM_SEATS),
        lock_max_retries: req
            .lock_max_retries
            .unwrap_or(crate::db::DEFAULT_LOCK_MAX_RETRIES),
        lock_seats: req.lock_seats.unwrap_or_default(),
    };
    let (wid, created) = st.db.create_or_reuse_watch(&spec).await.map_err(bad)?;
    if let Some(channels) = req.notify {
        for cid in channels {
            let _ = st.db.set_watch_channel(&wid, &cid, true).await;
        }
    }
    st.push_event(
        "watch",
        Some(&wid),
        format!("{} watch {}", if created { "创建" } else { "复用" }, wid),
    );
    st.wake.notify_one();
    Ok(Json(json!({ "watch_id": wid, "created": created })))
}

async fn get_watch(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let Some(w) = st.db.get_watch(&id).await.map_err(internal)? else {
        return Err(err(StatusCode::NOT_FOUND, "watch 不存在"));
    };
    let cinemas = st.db.watch_cinemas(&id).await.map_err(internal)?;
    let channels = st.db.watch_channels(&id).await.map_err(internal)?;
    let subs = st.db.subscriptions_for_watch(&id).await.map_err(internal)?;
    let metrics = st.db.metrics(&id, "-24 hours").await.map_err(internal)?;
    let lock_states = st.db.lock_states_for_watch(&id).await.map_err(internal)?;
    Ok(Json(json!({
        "id": w.id, "movie_id": w.movie_id, "movie_name": w.movie_name,
        "filter_key": w.filter_key, "cinemas": cinemas,
        "time_spec": w.time_spec_value(), "imax_only": w.imax_only,
        "enabled": w.enabled, "created_by": w.created_by,
        "visibility": w.visibility, "auto_lock": w.auto_lock,
        "lock_num_seats": w.lock_num_seats, "lock_max_retries": w.lock_max_retries,
        "lock_seats": w.lock_seats_value(), "subscriber_count": w.subscriber_count,
        "created_at": w.created_at, "updated_at": w.updated_at,
        "channels": channels, "subscriptions": subs, "metrics": metrics,
        "lock_states": lock_states,
    })))
}

#[derive(Debug, Deserialize)]
struct UpdateWatchRequest {
    #[serde(default)]
    movie_id: Option<i64>,
    #[serde(default)]
    movie_name: Option<String>,
    #[serde(default)]
    cinemas: Option<Vec<String>>,
    #[serde(default)]
    time: Option<String>,
    #[serde(default)]
    time_spec: Option<Value>,
    #[serde(default)]
    imax_only: Option<bool>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    auto_lock: Option<bool>,
    #[serde(default)]
    lock_num_seats: Option<u64>,
    #[serde(default)]
    lock_max_retries: Option<u64>,
    #[serde(default)]
    lock_seats: Option<Vec<String>>,
}

async fn update_watch(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateWatchRequest>,
) -> ApiResult<Json<Value>> {
    let time_spec = match req.time_spec {
        Some(v) => Some(TimeSpec::from_json(&v).map_err(bad)?.to_json()),
        None => match req.time {
            Some(s) => Some(TimeSpec::parse(&s).map_err(bad)?.to_json()),
            None => None,
        },
    };
    let patch = WatchPatch {
        movie_id: req.movie_id,
        movie_name: req.movie_name,
        cinemas: req.cinemas,
        time_spec,
        imax_only: req.imax_only,
        enabled: req.enabled,
        auto_lock: req.auto_lock,
        lock_num_seats: req.lock_num_seats,
        lock_max_retries: req.lock_max_retries,
        lock_seats: req.lock_seats,
    };
    let (new_id, filter_changed) = st.db.update_watch(&id, &patch).await.map_err(bad)?;
    st.push_event(
        "watch",
        Some(&new_id),
        if filter_changed {
            format!("编辑 watch {}；过滤条件变化，基线已清空", new_id)
        } else {
            format!("编辑 watch {}", new_id)
        },
    );
    st.wake.notify_one();
    Ok(Json(
        json!({ "watch_id": new_id, "filter_changed": filter_changed }),
    ))
}

async fn delete_watch(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let ok = st.db.delete_watch(&id).await.map_err(internal)?;
    if !ok {
        return Err(err(StatusCode::NOT_FOUND, "watch 不存在"));
    }
    st.push_event("watch", Some(&id), format!("删除 watch {}", id));
    st.wake.notify_one();
    Ok(Json(json!({ "deleted": true, "watch_id": id })))
}

async fn get_watch_channels(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let channels = st.db.watch_channels(&id).await.map_err(internal)?;
    Ok(Json(json!({ "channels": channels })))
}

#[derive(Debug, Deserialize)]
struct PutChannelsRequest {
    channel_ids: Vec<String>,
}

async fn put_watch_channels(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<PutChannelsRequest>,
) -> ApiResult<Json<Value>> {
    let all = st.db.list_channels().await.map_err(internal)?;
    for c in all {
        let on = req.channel_ids.iter().any(|x| x == &c.id);
        if c.channel_type == "autolock" {
            let patch = crate::db::WatchPatch {
                auto_lock: Some(on),
                ..Default::default()
            };
            st.db.update_watch(&id, &patch).await.map_err(internal)?;
        }
        st.db
            .set_watch_channel(&id, &c.id, on)
            .await
            .map_err(internal)?;
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
struct RangeQuery {
    #[serde(default)]
    range: Option<String>,
}

async fn watch_metrics(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<RangeQuery>,
) -> ApiResult<Json<Value>> {
    let modifier = match q.range.as_deref() {
        Some("7d") => "-8 days",
        Some("30d") => "-31 days",
        _ => "-2 days",
    };
    let rows = st.db.metrics(&id, modifier).await.map_err(internal)?;
    let points: Vec<Value> = rows
        .iter()
        .map(|m| {
            json!({
                "t": m.recorded_at,
                "total": m.total_shows,
                "imax": m.imax_shows,
                "buckets": m.bucket_count,
                "added": m.added_shows,
            })
        })
        .collect();
    Ok(Json(json!({ "watch_id": id, "points": points })))
}

// ------------------------- channels -------------------------

async fn list_channels(State(st): State<Arc<AppState>>) -> ApiResult<Json<Value>> {
    Ok(Json(
        json!({ "channels": st.db.list_channels().await.map_err(internal)? }),
    ))
}

#[derive(Debug, Deserialize)]
struct CreateChannelRequest {
    name: String,
    #[serde(rename = "type")]
    channel_type: String,
    #[serde(default)]
    config: Value,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

async fn create_channel(
    State(st): State<Arc<AppState>>,
    Json(req): Json<CreateChannelRequest>,
) -> ApiResult<Json<Value>> {
    if !valid_channel_type(&req.channel_type) {
        return Err(bad(format!("未知渠道类型: {}", req.channel_type)));
    }
    let id = st
        .db
        .create_channel(&req.name, &req.channel_type, &req.config)
        .await
        .map_err(internal)?;
    if !req.enabled {
        st.db
            .update_channel(&id, None, None, Some(false))
            .await
            .map_err(internal)?;
    }
    Ok(Json(json!({ "channel_id": id })))
}

fn valid_channel_type(t: &str) -> bool {
    matches!(
        t,
        "discord" | "email" | "webhook" | "wechat_local" | "xhs_local" | "wechat_miniprogram"
    )
}

#[derive(Debug, Deserialize)]
struct UpdateChannelRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    config: Option<Value>,
    #[serde(default)]
    enabled: Option<bool>,
}

async fn update_channel(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateChannelRequest>,
) -> ApiResult<Json<Value>> {
    st.db
        .update_channel(&id, req.name.as_deref(), req.config.as_ref(), req.enabled)
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_channel(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let ok = st.db.delete_channel(&id).await.map_err(internal)?;
    if !ok {
        return Err(err(StatusCode::NOT_FOUND, "channel 不存在"));
    }
    Ok(Json(json!({ "deleted": true })))
}

// ------------------------- users / subscriptions -------------------------

async fn list_users(
    State(st): State<Arc<AppState>>,
    Query(q): Query<LimitQuery>,
) -> ApiResult<Json<Value>> {
    let rows = st
        .db
        .list_users(q.limit.clamp(1, 500), q.offset.max(0))
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "users": rows })))
}

#[derive(Debug, Deserialize)]
struct CreateUserRequest {
    #[serde(default)]
    openid: Option<String>,
    #[serde(default)]
    nickname: Option<String>,
}

async fn create_user(
    State(st): State<Arc<AppState>>,
    Json(req): Json<CreateUserRequest>,
) -> ApiResult<Json<Value>> {
    let id = st
        .db
        .create_user(req.openid.as_deref(), req.nickname.as_deref().unwrap_or(""))
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "user_id": id })))
}

async fn delete_user(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let ok = st.db.delete_user(&id).await.map_err(internal)?;
    if !ok {
        return Err(err(StatusCode::NOT_FOUND, "user 不存在"));
    }
    Ok(Json(json!({ "deleted": true })))
}

async fn user_subscriptions(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let rows = st.db.subscriptions_for_user(&id).await.map_err(internal)?;
    Ok(Json(json!({ "subscriptions": rows })))
}

async fn list_subscriptions(
    State(st): State<Arc<AppState>>,
    Query(q): Query<LimitQuery>,
) -> ApiResult<Json<Value>> {
    let rows = st
        .db
        .list_subscriptions(q.limit.clamp(1, 500))
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "subscriptions": rows })))
}

#[derive(Debug, Deserialize)]
struct CreateSubscriptionRequest {
    user_id: String,
    watch_id: String,
    #[serde(default)]
    channel_id: Option<String>,
}

async fn create_subscription(
    State(st): State<Arc<AppState>>,
    Json(req): Json<CreateSubscriptionRequest>,
) -> ApiResult<Json<Value>> {
    let id = st
        .db
        .subscribe(&req.user_id, &req.watch_id, req.channel_id.as_deref())
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "subscription_id": id })))
}

async fn delete_subscription(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let ok = st.db.unsubscribe(&id).await.map_err(internal)?;
    if !ok {
        return Err(err(StatusCode::NOT_FOUND, "subscription 不存在"));
    }
    Ok(Json(json!({ "deleted": true })))
}

// ------------------------- settings / events -------------------------

#[derive(Debug, Deserialize)]
struct MovieQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

async fn list_movies(
    State(st): State<Arc<AppState>>,
    Query(q): Query<MovieQuery>,
) -> ApiResult<Json<Value>> {
    let rows = st
        .db
        .list_movies(q.q.as_deref().unwrap_or(""), q.limit.unwrap_or(500))
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "movies": rows })))
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    city: Option<u32>,
}

async fn search_movies_api(
    State(st): State<Arc<AppState>>,
    Query(q): Query<SearchQuery>,
) -> ApiResult<Json<Value>> {
    let keyword = q.q.unwrap_or_default().trim().to_string();
    let settings = Settings::load(&st.db).await.map_err(internal)?;
    let mut out: Vec<Value> = Vec::new();
    let mut seen: HashSet<i64> = HashSet::new();
    for m in st.db.list_movies(&keyword, 30).await.map_err(internal)? {
        seen.insert(m.id);
        out.push(json!({"id": m.id, "name": m.name, "show_type": m.show_type}));
    }
    let mut remote_error = false;
    if !keyword.is_empty() {
        match maoyan::search_movies_async(&keyword, settings.maoyan_city_id).await {
            Ok(rows) => {
                let _ = st.db.upsert_search_movies(&rows).await;
                for (id, name) in rows {
                    if let Ok(id) = id.parse::<i64>() {
                        if seen.insert(id) {
                            out.push(json!({"id": id, "name": name, "show_type": 4}));
                        }
                    }
                }
            }
            Err(_) => remote_error = true,
        }
        if out.is_empty() && keyword.parse::<i64>().is_ok() {
            if let Ok((id, name)) = maoyan::fetch_movie_detail_async(&keyword).await {
                let _ = st
                    .db
                    .upsert_search_movies(&[(id.clone(), name.clone())])
                    .await;
                if let Ok(id) = id.parse::<i64>() {
                    if seen.insert(id) {
                        out.push(json!({"id": id, "name": name, "show_type": 4}));
                    }
                }
            } else {
                remote_error = true;
            }
        }
        let rank = |v: &Value| -> u8 {
            let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("");
            if name.eq_ignore_ascii_case(&keyword) {
                0
            } else if name.starts_with(&keyword) {
                1
            } else if name.contains(&keyword) {
                2
            } else {
                3
            }
        };
        out.sort_by(|a, b| {
            rank(a).cmp(&rank(b)).then_with(|| {
                a.get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .cmp(b.get("name").and_then(|x| x.as_str()).unwrap_or(""))
            })
        });
    }
    Ok(Json(json!({
        "movies": out,
        "remote_error": remote_error,
    })))
}

async fn search_cinemas_api(
    State(st): State<Arc<AppState>>,
    Query(q): Query<SearchQuery>,
) -> ApiResult<Json<Value>> {
    let keyword = q.q.unwrap_or_default().trim().to_string();
    let settings = Settings::load(&st.db).await.map_err(internal)?;
    let city_id = q.city.unwrap_or(settings.maoyan_city_id).max(1);
    let mut out: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (id, name) in st.db.search_cinemas(&keyword, 30).await.map_err(internal)? {
        seen.insert(id.clone());
        out.push(json!({"id": id, "name": name, "addr": ""}));
    }
    let mut remote_error = false;
    if !keyword.is_empty() {
        let numeric = keyword.parse::<i64>().is_ok();
        if numeric {
            if let Ok(payload) = maoyan::fetch_cinema_async(&keyword).await {
                let name = payload
                    .get("cinema_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !name.is_empty() && !name.starts_with("影城 ") {
                    let _ = st.db.upsert_cinema(&keyword, &name, false).await;
                    out.push(json!({"id": keyword, "name": name, "addr": ""}));
                }
            } else {
                remote_error = true;
            }
        } else {
            match maoyan::search_cinemas_async(&keyword, city_id).await {
                Ok(rows) => {
                    for (id, name, addr) in rows {
                        let _ = st.db.upsert_cinema(&id, &name, false).await;
                        if seen.insert(id.clone()) {
                            out.push(json!({"id": id, "name": name, "addr": addr}));
                        }
                    }
                }
                Err(_) => remote_error = true,
            }
        }
        let rank = |v: &Value| -> u8 {
            let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("");
            if name.eq_ignore_ascii_case(&keyword) {
                0
            } else if name.starts_with(&keyword) {
                1
            } else if name.contains(&keyword) {
                2
            } else {
                3
            }
        };
        out.sort_by(|a, b| {
            rank(a).cmp(&rank(b)).then_with(|| {
                a.get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .cmp(b.get("name").and_then(|x| x.as_str()).unwrap_or(""))
            })
        });
    }
    Ok(Json(json!({
        "cinemas": out,
        "remote_error": remote_error,
    })))
}

async fn refresh_movies(State(st): State<Arc<AppState>>) -> ApiResult<Json<Value>> {
    let mut total = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for (show_type, label) in [(1u8, "热映"), (2u8, "即将上映"), (3u8, "经典")] {
        match maoyan::fetch_films_list_async(show_type).await {
            Ok(rows) => match st.db.upsert_movies(&rows, show_type).await {
                Ok(n) => total += n,
                Err(e) => errors.push(format!("{} 写入失败: {}", label, e)),
            },
            Err(e) => errors.push(format!("{} 拉取失败: {}", label, e)),
        }
    }
    st.push_event("movies", None, format!("电影列表刷新：{} 条", total));
    Ok(Json(json!({
        "ok": errors.is_empty(),
        "upserted": total,
        "count": st.db.movie_count().await.unwrap_or(0),
        "errors": errors,
    })))
}

async fn get_settings(State(st): State<Arc<AppState>>) -> ApiResult<Json<Value>> {
    let map = st.db.settings_map().await.map_err(internal)?;
    Ok(Json(json!({ "settings": map })))
}

async fn patch_settings(
    State(st): State<Arc<AppState>>,
    Json(mut body): Json<serde_json::Map<String, Value>>,
) -> ApiResult<Json<Value>> {
    for key in [
        "check_interval",
        "silent_window",
        "fetch_concurrency",
        "discord_webhook",
        "agent_url",
        "agent_token",
        "panel_token",
        "wx_appid",
        "wx_secret",
        "wx_template_id",
        "metrics_retention_days",
        "lock_headless",
        "maoyan_city_id",
        "monitor_enabled",
    ] {
        if let Some(v) = body.remove(key) {
            let s = match v {
                Value::String(s) => s,
                Value::Null => String::new(),
                other => other.to_string(),
            };
            if key == "check_interval" {
                let n: u64 = s.parse().map_err(|_| bad("check_interval 必须是数字"))?;
                if n < 1 {
                    return Err(bad("check_interval 必须 >= 1"));
                }
            }
            if key == "silent_window" && !s.trim().is_empty() {
                crate::silent::SilentWindow::parse(&s).map_err(bad)?;
            }
            st.db.set_setting(key, &s).await.map_err(internal)?;
        }
    }
    st.wake.notify_one();
    Ok(Json(json!({ "ok": true })))
}

async fn list_events(
    State(st): State<Arc<AppState>>,
    Query(q): Query<LimitQuery>,
) -> ApiResult<Json<Value>> {
    Ok(Json(json!({
        "events": st.events_snapshot(q.limit.clamp(1, 200) as usize)
    })))
}

#[derive(Debug, Deserialize)]
struct LockResultRequest {
    #[serde(default)]
    event_id: Option<String>,
    watch_id: String,
    #[serde(default)]
    movie_name: String,
    cinema_id: String,
    #[serde(default)]
    cinema_name: String,
    #[serde(default)]
    show_date: String,
    result: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    elapsed_sec: f64,
}

fn parse_lock_result(s: &str) -> Option<LockResult> {
    Some(match s {
        "Ok" => LockResult::Ok,
        "DryRun" => LockResult::DryRun,
        "ShowGone" => LockResult::ShowGone,
        "Error" => LockResult::Error,
        "Timeout" => LockResult::Timeout,
        "Unknown" => LockResult::Unknown,
        _ => return None,
    })
}

async fn agent_lock_result(
    State(st): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(req): Json<LockResultRequest>,
) -> ApiResult<Json<Value>> {
    if let Ok(settings) = Settings::load(&st.db).await {
        if let Some(expected) = settings.agent_token.as_deref() {
            let direct = headers
                .get("x-agent-token")
                .and_then(|v| v.to_str().ok())
                .map(|v| v == expected)
                .unwrap_or(false);
            let bearer = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(|v| v == expected)
                .unwrap_or(false);
            if !direct && !bearer {
                return Err(err(StatusCode::UNAUTHORIZED, "invalid agent token"));
            }
        }
    }
    let Some(result) = parse_lock_result(&req.result) else {
        return Err(bad(format!("未知锁票结果: {}", req.result)));
    };
    let Some(watch) = st.db.get_watch(&req.watch_id).await.map_err(internal)? else {
        return Err(err(StatusCode::NOT_FOUND, "watch 不存在"));
    };
    let key = format!("{}:{}", req.watch_id, req.cinema_id);
    st.lock_inflight.lock().unwrap().remove(&key);

    // agent 可能提前 + 最终各回调一次；同一 event_id 只处理一次。
    if let Ok(Some(ls)) = st.db.lock_state(&req.watch_id, &req.cinema_id).await {
        if req.event_id.is_some() && ls.last_event_id == req.event_id {
            return Ok(Json(json!({
                "ok": true, "duplicate": true, "status": ls.status, "tries": ls.tries
            })));
        }
        // 已经锁成功：忽略迟到的失败/重复回调，避免重试计数回退。
        if ls.status == "ok" && result != LockResult::Ok {
            return Ok(Json(json!({
                "ok": true, "ignored": true, "status": ls.status, "tries": ls.tries
            })));
        }
    }

    let (status, tries, retry_note) = match result {
        LockResult::Ok => {
            st.db
                .mark_lock_ok(&req.watch_id, &req.cinema_id, req.event_id.as_deref())
                .await
                .map_err(internal)?;
            ("ok".to_string(), 0, String::new())
        }
        LockResult::DryRun => ("dry_run".to_string(), 0, String::new()),
        other => {
            let row = st
                .db
                .record_lock_failure(
                    &req.watch_id,
                    &req.cinema_id,
                    other.label(),
                    Some(&req.summary),
                    req.event_id.as_deref(),
                    watch.lock_max_retries.max(1),
                )
                .await
                .map_err(internal)?;
            let note = if row.status == "exhausted" {
                "已耗尽重试额度，不再自动尝试".to_string()
            } else {
                "稍后自动重试".to_string()
            };
            (row.status, row.tries, note)
        }
    };

    let movie = if req.movie_name.is_empty() {
        watch.movie_name.clone()
    } else {
        req.movie_name.clone()
    };
    let cinema = if req.cinema_name.is_empty() {
        req.cinema_id.clone()
    } else {
        req.cinema_name.clone()
    };
    let (title, body) = match result {
        LockResult::Ok => (
            "🔒 自动锁票成功".to_string(),
            format!(
                "🎞 {}
🏛 {}
📅 {} 起｜🕒 用时 {:.1}s
📋 {}

⚠️ {} 分钟内到猫眼 App 完成支付",
                movie,
                cinema,
                req.show_date,
                req.elapsed_sec,
                req.summary,
                lock::PAY_WINDOW_MINUTES
            ),
        ),
        LockResult::DryRun => (
            "🧪 自动锁票 dry-run（未真锁）".to_string(),
            format!(
                "🎞 {}
🏛 {}
📋 {}",
                movie, cinema, req.summary
            ),
        ),
        other => (
            "⚠️ 自动锁票失败".to_string(),
            format!(
                "🎞 {}
🏛 {}
📋 {}

原因：{}
🔁 {}",
                movie,
                cinema,
                req.summary,
                other.label(),
                retry_note
            ),
        ),
    };
    st.push_event(
        "lock",
        Some(&req.watch_id),
        format!("{} {}（{}）", title, cinema, result.label()),
    );
    let st2 = st.clone();
    let wid = req.watch_id.clone();
    let buy_url = maoyan::buy_pc_url_owned(&req.cinema_id);
    tokio::spawn(async move {
        super::dispatcher::dispatch_lock_result(st2, &wid, &title, &body, Some(buy_url)).await;
    });
    Ok(Json(json!({
        "ok": true, "status": status, "tries": tries,
        "result": result.label(), "event_id": req.event_id,
    })))
}

async fn wx_login_stub() -> ApiResult<Json<Value>> {
    Err(err(
        StatusCode::NOT_IMPLEMENTED,
        "微信小程序登录尚未启用：需要配置 wx_appid/wx_secret/template_id，并实现 jscode2session",
    ))
}

// ------------------------- iGlance service.v1 -------------------------

/// 服务接口鉴权：未设置 token 时放行；设置后接受
/// `X-Gofer-Token` / `X-Agent-Token` / `Authorization: Bearer <token>`。
async fn service_auth(st: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let expected = service::expected_token(&st.db)
        .await
        .map_err(|e| internal(format!("读取服务 token 失败: {e}")))?;
    let Some(expected) = expected else {
        return Ok(());
    };
    let provided = service::token_from_headers(headers);
    if service::token_matches(&expected, provided.as_deref()) {
        Ok(())
    } else {
        Err(err(StatusCode::UNAUTHORIZED, "缺少或错误的 API token"))
    }
}

async fn service_status(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(e) = service_auth(&st, &headers).await {
        return e.into_response();
    }
    let settings = match Settings::load(&st.db).await {
        Ok(s) => s,
        Err(e) => return internal(e).into_response(),
    };
    let token_required = match service::expected_token(&st.db).await {
        Ok(t) => t.is_some(),
        Err(e) => return internal(e).into_response(),
    };
    let db_ok = sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&st.db.pool)
        .await
        .is_ok();
    let memory = tokio::task::spawn_blocking(service::collect_memory)
        .await
        .unwrap_or_else(|e| service::MemorySnapshot::unavailable(format!("内存采集任务失败: {e}")));

    let paused = !settings.monitor_enabled;
    let stopping = st.is_stopping();
    let state = if stopping {
        "stopping"
    } else if paused {
        "paused"
    } else {
        "running"
    };
    let base = st.base_url();
    let display = st.display_base_url();
    let panel = service::panel_url(&display);
    let body = json!({
        "schema": service::MANIFEST_SCHEMA,
        "ok": true,
        "service": {
            "id": service::SERVICE_ID,
            "name": service::SERVICE_NAME,
            "version": service::version(),
            "kind": "headless-service",
        },
        "state": state,
        "paused": paused,
        "stopping": stopping,
        "pid": std::process::id(),
        "started_at": service::started_at_iso(st.started_at),
        "uptime_sec": (now_secs() - st.started_at).max(0.0) as u64,
        "urls": {
            "base": base,
            "display": display,
            "health": format!("{}/health", display.trim_end_matches('/')),
            "panel": panel,
        },
        "auth": {
            "header": service::TOKEN_HEADER,
            "token_required": token_required,
        },
        "queue": {},
        "ready": {
            "db": db_ok,
            "monitor": settings.monitor_enabled,
        },
        "memory": memory,
        "controls": service::controls(&st.base_url()),
    });
    Json(body).into_response()
}

async fn service_memory(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(e) = service_auth(&st, &headers).await {
        return e.into_response();
    }
    let memory = tokio::task::spawn_blocking(service::collect_memory)
        .await
        .unwrap_or_else(|e| service::MemorySnapshot::unavailable(format!("内存采集任务失败: {e}")));
    Json(json!({
        "ok": true,
        "service": service::SERVICE_ID,
        "pid": std::process::id(),
        "memory": memory,
    }))
    .into_response()
}

async fn service_pause(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(e) = service_auth(&st, &headers).await {
        return e.into_response();
    }
    if let Err(e) = set_service_paused(&st, true).await {
        return e.into_response();
    }
    Json(json!({"ok": true, "state": "paused", "paused": true})).into_response()
}

async fn service_resume(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(e) = service_auth(&st, &headers).await {
        return e.into_response();
    }
    if let Err(e) = set_service_paused(&st, false).await {
        return e.into_response();
    }
    Json(json!({"ok": true, "state": "running", "paused": false})).into_response()
}

async fn service_toggle(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(e) = service_auth(&st, &headers).await {
        return e.into_response();
    }
    let settings = match Settings::load(&st.db).await {
        Ok(s) => s,
        Err(e) => return internal(e).into_response(),
    };
    let paused = settings.monitor_enabled;
    if let Err(e) = set_service_paused(&st, paused).await {
        return e.into_response();
    }
    let state = if paused { "paused" } else { "running" };
    Json(json!({"ok": true, "state": state, "paused": paused})).into_response()
}

async fn set_service_paused(st: &Arc<AppState>, paused: bool) -> Result<(), ApiError> {
    st.db
        .set_setting("monitor_enabled", if paused { "false" } else { "true" })
        .await
        .map_err(internal)?;
    // 面板 / metrics 立刻看到新状态，不必等 monitor 下一轮。
    st.health.lock().unwrap().paused = paused;
    st.wake.notify_one();
    st.push_event(
        "service",
        None,
        if paused {
            "服务暂停：不再轮询、通知或锁票"
        } else {
            "服务恢复：继续监控"
        },
    );
    Ok(())
}

#[derive(Debug, Deserialize)]
struct StopQuery {
    cleanup: Option<bool>,
    #[allow(dead_code)]
    close_browser: Option<bool>,
}

async fn service_stop(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<StopQuery>,
) -> Response {
    if let Err(e) = service_auth(&st, &headers).await {
        return e.into_response();
    }
    if st.stopping.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return (
            StatusCode::ACCEPTED,
            Json(json!({"ok": true, "state": "stopping", "already_stopping": true})),
        )
            .into_response();
    }

    let cleanup = q.cleanup.unwrap_or(true);
    tokio::spawn(async move {
        // 先让 202 响应发出去。
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        service::stop_cleanup(cleanup).await;
        service::remove_pid_file();
        service::bootout_launchd().await;
        std::process::exit(0);
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "ok": true,
            "state": "stopping",
            "cleanup": cleanup,
            "message": "ticket_tracker 正在停止；完全停止后使用 controls.start 中的命令重新启动。",
        })),
    )
        .into_response()
}
