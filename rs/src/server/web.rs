//! Web 管理面板（Askama + HTMX + uPlot）。
//!
//! 设计：warm parchment + editorial serif + monospaced data + clay/ember accents。
//! HTMX 刷新整页后用 `hx-select` 只替换片段，避免维护两套模板。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use askama::Template;
use axum::extract::{Form, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use chrono::{TimeZone, Timelike};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::services::ServeDir;

use super::{now_secs, AppState};
use crate::auth;
use crate::db::{ChannelRow, MetricRow, PollHealthRow, Settings, WatchRow};
use crate::engine::WatchCinemaState;
use crate::time_spec::{date_from_id, TimeSpec};

fn web_dir() -> String {
    if let Ok(dir) = std::env::var("TT_WEB_DIR") {
        if !dir.trim().is_empty() {
            return dir;
        }
    }
    let candidates = [
        "web".to_string(),
        format!("{}/web", env!("CARGO_MANIFEST_DIR")),
        "/app/web".to_string(),
    ];
    candidates
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .cloned()
        .unwrap_or_else(|| "web".to_string())
}

pub fn router(state: Arc<AppState>) -> Router {
    let protected = Router::new()
        .route("/", get(dashboard))
        .route("/watches", get(watches_page))
        .route("/watches/new", get(new_watch_page))
        .route("/watches/:id", get(watch_detail))
        .route("/users", get(users_page))
        .route("/settings", get(settings_page))
        .route("/actions/settings/interval", post(action_interval))
        .route("/actions/settings", post(action_settings))
        .route("/actions/monitor/toggle", post(action_monitor_toggle))
        .route("/update", get(update_page))
        .route("/actions/update/install", post(action_update_install))
        .route("/actions/watches", post(action_create_watch))
        .route("/actions/movies/refresh", post(action_refresh_movies))
        .route("/actions/watches/:id/edit", post(action_edit_watch))
        .route("/actions/watches/:id/lock", post(action_update_lock))
        .route("/actions/subscriptions", post(action_subscribe))
        .route("/actions/watches/:id/toggle", post(action_toggle_watch))
        .route(
            "/actions/watches/:id/toggle-detail",
            post(action_toggle_watch_detail),
        )
        .route(
            "/actions/watches/:id/channels/:cid/toggle",
            post(action_toggle_channel),
        )
        .route(
            "/actions/watches/:id/channels/:cid/config",
            post(action_watch_channel_config),
        )
        .route("/actions/channels/:cid/test", post(action_test_channel))
        .route("/actions/channels/:cid/config", post(action_channel_config))
        .route("/actions/users", post(action_create_user))
        .route("/logout", post(logout))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_panel,
        ));

    Router::new()
        .merge(protected)
        .route("/login", get(login_page).post(login_submit))
        .route("/setup", get(setup_page).post(setup_submit))
        .nest_service("/static", ServeDir::new(web_dir()))
        .with_state(state)
}

// ------------------------- 视图模型 -------------------------

#[derive(Debug, Clone)]
pub struct CountsView {
    pub polls: i64,
    pub watches: i64,
    pub users: i64,
    pub subscriptions: i64,
    pub active_watches: i64,
}

#[derive(Debug, Clone)]
pub struct HealthView {
    pub ok: bool,
    pub status_text: String,
    pub status_class: String,
    pub fetch_ok: usize,
    pub fetch_err: usize,
    pub fetch_total: usize,
    pub check_count: u64,
    pub silent: bool,
    pub last_poll: String,
    pub last_error: String,
    pub has_error: bool,
    pub consecutive_failures: u32,
    pub paused: bool,
}

#[derive(Debug, Clone)]
pub struct WatchItemView {
    pub href: String,
    pub id: String,
    pub name: String,
    pub movie_id: i64,
    pub subs: i64,
    pub status_text: String,
    pub status_class: String,
    pub enabled: bool,
    pub last_trigger: String,
    pub cinemas: usize,
    pub bar_pct: i64,
    pub baseline_total: i64,
    pub sparkline: String,
}

#[derive(Debug, Clone)]
pub struct PollItemView {
    pub cinema_id: String,
    pub name: String,
    pub watch_count: i64,
    pub status_text: String,
    pub status_class: String,
    pub last_polled: String,
    pub duration: String,
    pub shows: i64,
}

#[derive(Debug, Clone)]
pub struct EventView {
    pub ts: String,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct UserItemView {
    pub id: String,
    pub name: String,
    pub openid: String,
    pub subs: i64,
    pub created: String,
}

#[derive(Debug, Clone)]
pub struct ChartView {
    pub enabled: bool,
    pub id: String,
    pub name: String,
    pub subtitle: String,
    pub total: usize,
    pub imax: usize,
    pub buckets: usize,
}

#[derive(Debug, Clone)]
pub struct PollHealthRowView {
    pub time: String,
    pub ok: i64,
    pub total: i64,
    pub speed: String,
    pub status_text: String,
    pub status_class: String,
    pub error: String,
}

#[derive(Debug, Clone, Default)]
pub struct PollHealthView {
    pub has_data: bool,
    pub svg: String,
    pub day_label: String,
    pub axis: Vec<String>,
    pub rows: Vec<PollHealthRowView>,
    pub rounds: usize,
    pub success_rate: String,
    pub avg_speed: String,
    pub blocked_rounds: usize,
    pub failed_rounds: usize,
    pub last_time: String,
    pub last_status_text: String,
    pub last_status_class: String,
    pub last_detail: String,
    pub last_error: String,
    pub last_error_time: String,
}

#[derive(Debug, Clone)]
pub struct LabelView {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct CinemaChoiceView {
    pub id: String,
    pub name: String,
    pub selected: bool,
}

#[derive(Debug, Clone)]
pub struct ChannelView {
    pub id: String,
    pub name: String,
    pub type_label: String,
    pub type_class: String,
    pub config_summary: String,
    pub linked: bool,
    pub enabled: bool,
    pub testable: bool,
    pub local: bool,
    pub status_text: String,
    pub status_class: String,
    pub url: String,
    pub to: String,
    pub group: String,
    pub agent_url: String,
    pub agent_token: String,
    /// `global` = 复用 Settings 里的 Gofer；`custom` = 使用本 watch 的地址。
    pub agent_source: String,
    pub global_agent_url: String,
    pub global_agent_token_set: bool,
    pub lock_num_seats: String,
    pub lock_max_retries: String,
    pub lock_seats_text: String,
}

#[derive(Debug, Clone)]
pub struct SubscriberView {
    pub id: String,
    pub user_id: String,
    pub user_name: String,
    pub channel_name: String,
    pub created: String,
}

#[derive(Debug, Clone)]
pub struct LockStateView {
    pub cinema_id: String,
    pub cinema_name: String,
    pub status_text: String,
    pub status_class: String,
    pub tries: i64,
    pub last_error: String,
}

#[derive(Debug, Clone)]
pub struct BaselineShowView {
    pub time: String,
    pub hall: String,
    pub format: String,
    pub cinema: String,
    pub imax: bool,
}

#[derive(Debug, Clone)]
pub struct BaselineDayView {
    pub date: String,
    pub count: usize,
    pub shows: Vec<BaselineShowView>,
}

#[derive(Debug, Clone)]
pub struct ChannelConfigView {
    pub id: String,
    pub name: String,
    pub channel_type: String,
    pub type_label: String,
    pub url: String,
    pub to: String,
    pub group: String,
    pub enabled: bool,
}

// ------------------------- templates -------------------------

#[derive(Template)]
#[template(path = "dashboard.html")]
struct DashboardTemplate {
    version: String,
    now: String,
    page: String,
    uptime: String,
    interval: u64,
    last_poll_ts: f64,
    next_poll_ts: f64,
    counts: CountsView,
    health: HealthView,
    top_watches: Vec<WatchItemView>,
    polls: Vec<PollItemView>,
    events: Vec<EventView>,
    users: Vec<UserItemView>,
    user_total: i64,
    poll_health: PollHealthView,
}

#[derive(Template)]
#[template(path = "watches.html")]
struct WatchesTemplate {
    version: String,
    now: String,
    page: String,
    watches: Vec<WatchItemView>,
    count: i64,
}

#[derive(Template)]
#[template(path = "new_watch.html")]
struct NewWatchTemplate {
    version: String,
    now: String,
    page: String,
    error: String,
    movie_error: bool,
    cinemas: Vec<CinemaChoiceView>,
    time_spec_json: String,
    city_id: u32,
}

#[derive(Template)]
#[template(path = "detail.html")]
struct DetailTemplate {
    version: String,
    now: String,
    page: String,
    id: String,
    movie_id: i64,
    name: String,
    enabled: bool,
    enabled_text: String,
    status_text: String,
    status_class: String,
    time_spec: String,
    imax_text: String,
    imax_only: bool,
    cinema_count: usize,
    subscriber_count: i64,
    total: usize,
    imax: usize,
    buckets: usize,
    created_at: String,
    time_spec_json: String,
    lock_rows: Vec<LockStateView>,
    cinemas: Vec<LabelView>,
    cinema_choices: Vec<CinemaChoiceView>,
    channels: Vec<ChannelView>,
    subscribers: Vec<SubscriberView>,
    users: Vec<LabelView>,
    chart: ChartView,
    baseline_days: Vec<BaselineDayView>,
    city_id: u32,
}

#[derive(Template)]
#[template(path = "users.html")]
struct UsersTemplate {
    version: String,
    now: String,
    page: String,
    total: i64,
    subscriptions: i64,
    users: Vec<UserItemView>,
}

#[derive(Template)]
#[template(path = "update.html")]
struct UpdateTemplate {
    version: String,
    now: String,
    page: String,
    info: crate::update::UpdateInfo,
}

#[derive(Template)]
#[template(path = "update_result.html")]
struct UpdateResultTemplate {
    version: String,
    now: String,
    page: String,
    ok: bool,
    title: String,
    message: String,
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate {
    version: String,
    now: String,
    error: String,
}

#[derive(Template)]
#[template(path = "setup.html")]
struct SetupTemplate {
    version: String,
    now: String,
    error: String,
}

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTemplate {
    version: String,
    now: String,
    page: String,
    interval: String,
    silent: String,
    discord_webhook: String,
    agent_url: String,
    agent_token: String,
    metrics_retention_days: String,
    lock_headless: bool,
    maoyan_city_id: String,
    wx_appid: String,
    wx_secret: String,
    wx_template_id: String,
    channels: Vec<ChannelConfigView>,
}

fn render<T: Template>(t: &T) -> Html<String> {
    match t.render() {
        Ok(s) => Html(s),
        Err(e) => Html(format!(
            "<pre style=\"padding:24px;font-family:monospace\">template error: {}</pre>",
            e
        )),
    }
}

fn htmx(headers: &HeaderMap) -> bool {
    headers
        .get("hx-request")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "true")
        .unwrap_or(false)
}

fn meta() -> (String, String) {
    (
        env!("CARGO_PKG_VERSION").to_string(),
        chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
    )
}

fn fmt_uptime(sec: u64) -> String {
    format_uptime(sec)
}

fn format_uptime(sec: u64) -> String {
    let d = sec / 86_400;
    let h = (sec % 86_400) / 3_600;
    let m = (sec % 3_600) / 60;
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

fn fmt_ts(ts: Option<f64>) -> String {
    let Some(ts) = ts else {
        return "—".into();
    };
    chrono::Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .map(|d| d.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "—".into())
}

fn fmt_since(ts: Option<f64>) -> String {
    let Some(ts) = ts else {
        return "—".into();
    };
    let secs = (now_secs() - ts).max(0.0) as u64;
    if secs < 60 {
        format!("{}s 前", secs)
    } else if secs < 3600 {
        format!("{}m 前", secs / 60)
    } else if secs < 86400 {
        format!("{}h 前", secs / 3600)
    } else {
        format!("{}d 前", secs / 86400)
    }
}

fn status_label(status: &str) -> (&'static str, &'static str) {
    match status {
        "triggered" => ("有新增", "warn"),
        "monitoring" => ("监控中", "on"),
        "initializing" => ("建基线", "flat"),
        "no_movie" => ("未上架", "flat"),
        "error" => ("出错", "bad"),
        "disabled" => ("已停用", "flat"),
        _ => ("等待", "flat"),
    }
}

fn health_view(h: &super::HealthState) -> HealthView {
    let ok = h.health_ok;
    HealthView {
        ok,
        status_text: if ok { "正常" } else { "异常" }.into(),
        status_class: if ok { "ok" } else { "bad" }.into(),
        fetch_ok: h.fetch_ok,
        fetch_err: h.fetch_err,
        fetch_total: h.fetch_ok + h.fetch_err,
        check_count: h.check_count,
        silent: h.silent,
        last_poll: fmt_ts(h.last_poll_at),
        last_error: h.last_error.clone().unwrap_or_default(),
        has_error: h.last_error.is_some(),
        consecutive_failures: h.consecutive_failures,
        paused: h.paused,
    }
}

fn watch_status_text(st: Option<&super::WatchStatus>, enabled: bool) -> (String, String) {
    if !enabled {
        return ("已停用".into(), "flat".into());
    }
    match st.map(|s| s.status.as_str()).unwrap_or("") {
        "" => ("等待首轮".into(), "flat".into()),
        s => {
            let (t, c) = status_label(s);
            (t.into(), c.into())
        }
    }
}

fn watch_items(
    rows: &[WatchRow],
    status: &HashMap<String, super::WatchStatus>,
    max_subs: i64,
) -> Vec<WatchItemView> {
    rows.iter()
        .map(|w| {
            let st = status.get(&w.id);
            let (status_text, status_class) = watch_status_text(st, w.enabled);
            let subs = w.subscriber_count.max(0);
            let last_trigger = if st.and_then(|s| s.last_trigger_at).is_some() {
                fmt_since(st.and_then(|s| s.last_trigger_at))
            } else {
                "未触发".into()
            };
            WatchItemView {
                href: format!("/watches/{}", w.id),
                id: w.id.clone(),
                name: if w.movie_name.is_empty() {
                    w.id.clone()
                } else {
                    w.movie_name.clone()
                },
                movie_id: w.movie_id,
                subs,
                status_text,
                status_class,
                enabled: w.enabled,
                last_trigger,
                cinemas: 0,
                bar_pct: if max_subs <= 0 {
                    0
                } else {
                    ((subs * 100) / max_subs).clamp(0, 100)
                },
                baseline_total: 0,
                sparkline: String::new(),
            }
        })
        .collect()
}

fn time_summary(w: &WatchRow) -> String {
    TimeSpec::from_json(&w.time_spec_value())
        .map(|t| t.summary())
        .unwrap_or_else(|_| "不限".into())
}

fn baseline_stats(states: &[(String, String)], imax_only: bool) -> (usize, usize, usize) {
    let mut total = 0usize;
    let mut imax = 0usize;
    let mut buckets = 0usize;
    for (_, raw) in states {
        let v = serde_json::from_str::<Value>(raw).unwrap_or(Value::Null);
        let st = WatchCinemaState::from_json(&v);
        for b in st.buckets.values() {
            total += b.count as usize;
            if imax_only {
                imax += b.count as usize;
            } else {
                imax += b.shows.iter().filter(|s| s.is_imax).count();
            }
            buckets += 1;
        }
    }
    (total, imax, buckets)
}

fn sparkline_svg(rows: &[MetricRow], fallback: usize) -> String {
    let mut vals: Vec<f64> = rows.iter().map(|m| m.total_shows.max(0) as f64).collect();
    if vals.is_empty() {
        vals.push(fallback as f64);
    }
    if vals.len() == 1 {
        let v = vals[0];
        vals.push(v);
    }
    let max = vals.iter().copied().fold(0.0_f64, f64::max);
    let min = vals.iter().copied().fold(max, f64::min);
    let span = (max - min).max(1.0);
    let w = 100.0_f64;
    let h = 24.0_f64;
    let pad = 2.0_f64;
    let n = vals.len();
    let points = vals
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = if n <= 1 {
                0.0
            } else {
                i as f64 * w / (n - 1) as f64
            };
            let y = h - pad - (v - min) / span * (h - pad * 2.0);
            format!("{:.1},{:.1}", x, y)
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "<svg class=\"sparkline\" viewBox=\"0 0 100 24\" preserveAspectRatio=\"none\" aria-hidden=\"true\"><polyline points=\"{}\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.6\" stroke-linejoin=\"round\" stroke-linecap=\"round\"/></svg>",
        points
    )
}

// ------------------------- poll health -------------------------

#[derive(Debug, Clone)]
struct HealthPoint {
    ts: i64,
    ok: i64,
    total: i64,
    blocked: i64,
    err: i64,
}

fn health_points(rows: &[PollHealthRow]) -> Vec<HealthPoint> {
    // 自然日内每 90s 一轮最多约 960 个点；按 5 分钟聚合，最多 288 个点。
    let mut buckets: BTreeMap<i64, HealthPoint> = BTreeMap::new();
    for row in rows {
        let key = row.recorded_ts.div_euclid(300);
        let point = buckets.entry(key).or_insert(HealthPoint {
            ts: row.recorded_ts,
            ok: 0,
            total: 0,
            blocked: 0,
            err: 0,
        });
        point.ts = point.ts.max(row.recorded_ts);
        point.ok += row.cinemas_ok;
        point.total += row.cinemas_total;
        point.blocked += row.blocked_cinemas;
        point.err += row.cinemas_err;
    }
    buckets.into_values().collect()
}

fn poll_health_svg(points: &[HealthPoint], start_ts: i64, end_ts: i64) -> String {
    if points.is_empty() {
        return String::new();
    }
    const W: f64 = 640.0;
    const H: f64 = 158.0;
    const LEFT: f64 = 38.0;
    const RIGHT: f64 = 8.0;
    const TOP: f64 = 10.0;
    const BOTTOM: f64 = 10.0;
    let span = (end_ts - start_ts).max(1) as f64;
    let px = |ts: i64| LEFT + ((ts - start_ts) as f64 / span).clamp(0.0, 1.0) * (W - LEFT - RIGHT);
    let py = |rate: f64| TOP + (1.0 - rate.clamp(0.0, 1.0)) * (H - TOP - BOTTOM);
    let rate = |p: &HealthPoint| {
        if p.total > 0 {
            p.ok as f64 / p.total as f64
        } else {
            0.0
        }
    };
    let color = |p: &HealthPoint| {
        if p.blocked > 0 {
            "#a64535"
        } else if p.err > 0 {
            "#9a6a22"
        } else {
            "#3f6b4f"
        }
    };

    let mut svg = format!(
        "<svg class=\"poll-health-svg\" viewBox=\"0 0 {W:.0} {H:.0}\" role=\"img\" aria-label=\"poll 成功率\">"
    );
    for (label, value) in [("100%", 1.0), ("50%", 0.5), ("0%", 0.0)] {
        let y = py(value);
        svg.push_str(&format!(
            "<line x1=\"{LEFT:.1}\" y1=\"{y:.1}\" x2=\"{:.1}\" y2=\"{y:.1}\" stroke=\"#e3dacc\" stroke-width=\"1\" stroke-dasharray=\"2 4\"/>",
            W - RIGHT
        ));
        svg.push_str(&format!(
            "<text x=\"0\" y=\"{:.1}\" fill=\"#87867f\" font-family=\"'JetBrains Mono','SF Mono',monospace\" font-size=\"9\">{label}</text>",
            y + 3.0
        ));
    }
    for pair in points.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let stroke = if a.blocked > 0 || b.blocked > 0 {
            "#a64535"
        } else if a.err > 0 || b.err > 0 {
            "#9a6a22"
        } else {
            "#3f6b4f"
        };
        svg.push_str(&format!(
            "<line x1=\"{:.1}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"{stroke}\" stroke-width=\"2\" stroke-linecap=\"round\"/>",
            px(a.ts),
            py(rate(a)),
            px(b.ts),
            py(rate(b))
        ));
    }
    for point in points {
        let ts = chrono::Local
            .timestamp_opt(point.ts, 0)
            .single()
            .map(|d| d.format("%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "—".into());
        svg.push_str(&format!(
            "<circle cx=\"{:.1}\" cy=\"{:.1}\" r=\"2.4\" fill=\"{}\"><title>{ts} · {}/{} 成功{}</title></circle>",
            px(point.ts),
            py(rate(point)),
            color(point),
            point.ok,
            point.total,
            if point.blocked > 0 {
                format!(" · 风控 {}", point.blocked)
            } else if point.err > 0 {
                format!(" · 失败 {}", point.err)
            } else {
                String::new()
            }
        ));
    }
    svg.push_str("</svg>");
    svg
}

fn poll_health_status(row: &PollHealthRow) -> (&'static str, &'static str) {
    if row.blocked_cinemas > 0 {
        ("风控", "bad")
    } else if row.cinemas_err > 0 {
        ("部分失败", "warn")
    } else {
        ("正常", "ok")
    }
}

fn fmt_ms(ms: i64) -> String {
    if ms <= 0 {
        "—".into()
    } else if ms < 1000 {
        format!("{}ms", ms)
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

fn fmt_health_clock(ts: i64) -> String {
    chrono::Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|d| d.format("%H:%M").to_string())
        .unwrap_or_else(|| "—".into())
}

fn local_day_bounds() -> (i64, i64, String) {
    let now = chrono::Local::now();
    let today = now.date_naive();
    let start = chrono::Local
        .from_local_datetime(&today.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .map(|d| d.timestamp())
        .unwrap_or_else(|| now.timestamp() - i64::from(now.time().num_seconds_from_midnight()));
    let tomorrow = today.succ_opt().unwrap_or(today);
    let end = chrono::Local
        .from_local_datetime(&tomorrow.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .map(|d| d.timestamp())
        .unwrap_or(start + 86_400);
    (start, end, today.format("%m-%d").to_string())
}

fn poll_health_view(
    rows: &[PollHealthRow],
    start: i64,
    end: i64,
    day_label: &str,
) -> PollHealthView {
    let points = health_points(rows);
    let has_data = !points.is_empty();
    let svg = if has_data {
        poll_health_svg(&points, start, end)
    } else {
        String::new()
    };

    let total: i64 = rows.iter().map(|r| r.cinemas_total).sum();
    let ok: i64 = rows.iter().map(|r| r.cinemas_ok).sum();
    let success_rate = if total > 0 {
        format!("{:.1}%", ok as f64 * 100.0 / total as f64)
    } else {
        "—".into()
    };
    let avg_speed = if rows.is_empty() {
        "—".into()
    } else {
        fmt_ms(rows.iter().map(|r| r.duration_ms).sum::<i64>() / rows.len() as i64)
    };
    let latest = rows.last();
    let (last_status_text, last_status_class) = latest
        .map(poll_health_status)
        .map(|(t, c)| (t.to_string(), c.to_string()))
        .unwrap_or_else(|| ("等待".into(), "flat".into()));
    let last_time = latest
        .map(|r| fmt_health_clock(r.recorded_ts))
        .unwrap_or_else(|| "—".into());
    let last_detail = latest
        .map(|r| {
            format!(
                "{}/{} 影院成功 · {}",
                r.cinemas_ok,
                r.cinemas_total,
                fmt_ms(r.duration_ms)
            )
        })
        .unwrap_or_default();
    let last_error_row = rows
        .iter()
        .rev()
        .find(|r| r.error_summary.as_deref().is_some_and(|s| !s.is_empty()));

    let recent = rows
        .iter()
        .rev()
        .take(10)
        .map(|r| {
            let (status_text, status_class) = poll_health_status(r);
            PollHealthRowView {
                time: fmt_health_clock(r.recorded_ts),
                ok: r.cinemas_ok,
                total: r.cinemas_total,
                speed: fmt_ms(r.duration_ms),
                status_text: status_text.into(),
                status_class: status_class.into(),
                error: r.error_summary.clone().unwrap_or_default(),
            }
        })
        .collect();
    let axis = ["00:00", "06:00", "12:00", "18:00", "24:00"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    PollHealthView {
        has_data,
        svg,
        day_label: day_label.to_string(),
        axis,
        rows: recent,
        rounds: rows.len(),
        success_rate,
        avg_speed,
        blocked_rounds: rows.iter().filter(|r| r.blocked_cinemas > 0).count(),
        failed_rounds: rows.iter().filter(|r| r.cinemas_err > 0).count(),
        last_time,
        last_status_text,
        last_status_class,
        last_detail,
        last_error: last_error_row
            .and_then(|r| r.error_summary.clone())
            .unwrap_or_default(),
        last_error_time: last_error_row
            .map(|r| fmt_health_clock(r.recorded_ts))
            .unwrap_or_default(),
    }
}

fn baseline_days_from_states(
    states: &[(String, String)],
    names: &HashMap<String, String>,
) -> Vec<BaselineDayView> {
    let mut grouped: BTreeMap<String, Vec<BaselineShowView>> = BTreeMap::new();
    for (cid, raw) in states {
        let Ok(v) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        let st = WatchCinemaState::from_json(&v);
        let cinema = names
            .get(cid)
            .cloned()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| cid.clone());
        for (key, bucket) in st.buckets {
            let Some(date) = date_from_id(key / 24) else {
                continue;
            };
            let _hour = key % 24;
            let date = date.format("%Y-%m-%d").to_string();
            for show in bucket.shows {
                grouped
                    .entry(date.clone())
                    .or_default()
                    .push(BaselineShowView {
                        time: show.tm,
                        hall: show.th,
                        format: show.tp,
                        cinema: cinema.clone(),
                        imax: show.is_imax,
                    });
            }
        }
    }
    grouped
        .into_iter()
        .map(|(date, mut shows)| {
            shows.sort_by(|a, b| (&a.time, &a.cinema).cmp(&(&b.time, &b.cinema)));
            BaselineDayView {
                date,
                count: shows.len(),
                shows,
            }
        })
        .collect()
}

fn channel_label(t: &str) -> &'static str {
    match t {
        "discord" => "DISCORD",
        "email" => "EMAIL",
        "webhook" => "WEBHOOK",
        "wechat_local" => "WECHAT · LOCAL",
        "xhs_local" => "XHS · LOCAL",
        "autolock" => "AUTO LOCK",
        "wechat_miniprogram" => "MINI PROGRAM",
        _ => "CHANNEL",
    }
}

fn channel_config_view(ch: &ChannelRow) -> ChannelConfigView {
    let cfg = ch.config();
    let s = |k: &str| {
        cfg.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let pick = |a: &str, b: &str| {
        let v = s(a);
        if v.is_empty() {
            s(b)
        } else {
            v
        }
    };
    ChannelConfigView {
        id: ch.id.clone(),
        name: ch.name.clone(),
        channel_type: ch.channel_type.clone(),
        type_label: channel_label(&ch.channel_type).into(),
        url: pick("url", "webhook"),
        to: s("to"),
        group: pick("group", "xhs"),
        enabled: ch.enabled,
    }
}

// ------------------------- dashboard -------------------------

#[derive(Debug, Deserialize, Default)]
struct DashboardQuery {
    #[serde(default)]
    watch: Option<String>,
}

async fn dashboard(State(st): State<Arc<AppState>>, Query(q): Query<DashboardQuery>) -> Response {
    match build_dashboard(&st, q.watch.as_deref()).await {
        Ok(t) => render(&t).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn build_dashboard(
    st: &Arc<AppState>,
    _watch_id: Option<&str>,
) -> anyhow::Result<DashboardTemplate> {
    let settings = Settings::load(&st.db).await?;
    let (version, now) = meta();
    let h = st.health_snapshot();
    let status = st.watch_status_snapshot();

    let top_watches = st.db.list_watches("subscribers", 8, 0).await?;
    let max_subs = top_watches
        .iter()
        .map(|w| w.subscriber_count)
        .max()
        .unwrap_or(0);
    let mut items = watch_items(&top_watches, &status, max_subs);
    for item in &mut items {
        item.cinemas = st.db.watch_cinemas(&item.id).await?.len();
        let states = st.db.watch_states(&item.id).await?;
        let base = baseline_stats(&states, false).0 as i64;
        let metrics = st.db.metrics(&item.id, "-24 hours").await?;
        item.baseline_total = base;
        item.sparkline = sparkline_svg(&metrics, base as usize);
    }

    let polls = st
        .db
        .list_polls(8)
        .await?
        .into_iter()
        .map(|p| {
            let (status_text, status_class) = match p.last_ok {
                Some(true) => ("正常", "ok"),
                Some(false) => ("失败", "bad"),
                None => ("等待", "flat"),
            };
            PollItemView {
                cinema_id: p.cinema_id,
                name: if p.cinema_name.is_empty() {
                    "—".into()
                } else {
                    p.cinema_name
                },
                watch_count: p.watch_count,
                status_text: status_text.into(),
                status_class: status_class.into(),
                last_polled: p.last_polled_at.unwrap_or_else(|| "—".into()),
                duration: format!("{}ms", p.last_duration_ms),
                shows: p.shows_count,
            }
        })
        .collect();

    let events = st
        .events_snapshot(12)
        .into_iter()
        .map(|e| EventView {
            ts: e.ts,
            kind: e.kind,
            message: e.message,
        })
        .collect();

    let users = st
        .db
        .list_users(5, 0)
        .await?
        .into_iter()
        .map(|u| UserItemView {
            id: u.id.clone(),
            name: if u.nickname.is_empty() {
                u.id.clone()
            } else {
                u.nickname
            },
            openid: u.openid.unwrap_or_default(),
            subs: u.subscription_count,
            created: u.created_at,
        })
        .collect();

    let user_total = st.db.user_count().await?;
    let (day_start, day_end, day_label) = local_day_bounds();
    let health_today = st
        .db
        .poll_health_between(day_start, day_end)
        .await
        .unwrap_or_default();
    let poll_health = poll_health_view(&health_today, day_start, day_end, &day_label);

    Ok(DashboardTemplate {
        version,
        now,
        page: "dashboard".into(),
        uptime: fmt_uptime((now_secs() - st.started_at).max(0.0) as u64),
        interval: settings.check_interval,
        last_poll_ts: h.last_poll_at.unwrap_or(0.0),
        next_poll_ts: h.next_poll_at.unwrap_or(0.0),
        counts: CountsView {
            polls: st.db.poll_count().await?,
            watches: st.db.watch_count().await?,
            users: user_total,
            subscriptions: st.db.subscription_count().await?,
            active_watches: top_watches.iter().filter(|w| w.enabled).count() as i64,
        },
        health: health_view(&h),
        top_watches: items,
        polls,
        events,
        users,
        user_total,
        poll_health,
    })
}

// ------------------------- watches / detail -------------------------

async fn watches_page(State(st): State<Arc<AppState>>) -> Response {
    let (version, now) = meta();
    let status = st.watch_status_snapshot();
    let rows = st
        .db
        .list_watches("subscribers", 200, 0)
        .await
        .unwrap_or_default();
    let max_subs = rows.iter().map(|w| w.subscriber_count).max().unwrap_or(0);
    let mut items = watch_items(&rows, &status, max_subs);
    for item in &mut items {
        item.cinemas = st
            .db
            .watch_cinemas(&item.id)
            .await
            .map(|v| v.len())
            .unwrap_or(0);
    }
    let t = WatchesTemplate {
        version,
        now,
        page: "watches".into(),
        count: rows.len() as i64,
        watches: items,
    };
    render(&t).into_response()
}

async fn watch_detail(State(st): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    match build_detail(&st, &id).await {
        Ok(t) => render(&t).into_response(),
        Err(_) => Redirect::to("/watches").into_response(),
    }
}

fn merge_channel_config(global: &Value, binding: Option<&Value>) -> Value {
    let mut out = global.as_object().cloned().unwrap_or_default();
    if let Some(binding) = binding.and_then(|v| v.as_object()) {
        for (k, v) in binding {
            if !v.is_null() {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(out)
}

fn cfg_str(cfg: &Value, key: &str) -> String {
    cfg.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn channel_views(
    all: &[ChannelRow],
    linked: &HashSet<String>,
    bindings: &HashMap<String, Value>,
    watch: &WatchRow,
    global_agent_url: &str,
    global_agent_token_set: bool,
) -> Vec<ChannelView> {
    all.iter()
        .map(|ch| {
            let cfg = merge_channel_config(&ch.config(), bindings.get(&ch.id));
            let is_autolock = ch.channel_type == "autolock";
            let on = linked.contains(&ch.id) || (is_autolock && watch.auto_lock);
            let local = matches!(
                ch.channel_type.as_str(),
                "wechat_local" | "xhs_local" | "autolock"
            );
            let group = cfg_str(&cfg, "group");
            let url = cfg_str(&cfg, "url");
            let to = cfg_str(&cfg, "to");
            let agent_url = cfg_str(&cfg, "agent_url");
            let agent_token = cfg_str(&cfg, "agent_token");
            let agent_source = if agent_url.is_empty() {
                "global"
            } else {
                "custom"
            };
            let gofer_note = if local {
                if agent_url.is_empty() {
                    "Gofer 全局"
                } else {
                    "Gofer 自定义"
                }
            } else {
                ""
            };
            let lock_num_seats = cfg
                .get("lock_num_seats")
                .and_then(|v| v.as_i64())
                .unwrap_or(watch.lock_num_seats)
                .max(1)
                .to_string();
            let lock_max_retries = cfg
                .get("lock_max_retries")
                .and_then(|v| v.as_i64())
                .unwrap_or(watch.lock_max_retries)
                .max(1)
                .to_string();
            let lock_seats_text = match cfg.get("lock_seats") {
                Some(Value::Array(a)) => a
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                Some(Value::String(s)) => s.clone(),
                _ => watch.lock_seats_value().join(" "),
            };
            let (mut status_text, mut status_class) = if !ch.enabled {
                ("全局停用", "flat")
            } else if is_autolock {
                if on {
                    ("已开启", "warn")
                } else {
                    ("未开启", "flat")
                }
            } else if on {
                ("已绑定", "on")
            } else {
                ("未绑定", "flat")
            };
            if ch.enabled && ch.channel_type == "xhs_local" && group.is_empty() {
                (status_text, status_class) = ("未配置群名", "warn");
            }

            let config_summary = match ch.channel_type.as_str() {
                "discord" | "webhook" => {
                    if url.is_empty() {
                        "未配置 URL".into()
                    } else {
                        url.clone()
                    }
                }
                "email" => {
                    if to.is_empty() {
                        "未配置收件人".into()
                    } else {
                        to.clone()
                    }
                }
                "xhs_local" => {
                    if group.is_empty() {
                        format!("群：未配置 · {}", gofer_note)
                    } else {
                        format!("群：{} · {}", group, gofer_note)
                    }
                }
                "wechat_local" => format!("发到当前打开的微信会话 · {}", gofer_note),
                "autolock" => {
                    if on {
                        format!(
                            "{} 座 · 重试 {} · {}",
                            lock_num_seats, lock_max_retries, gofer_note
                        )
                    } else {
                        format!("开启后命中即真锁 · {}", gofer_note)
                    }
                }
                "wechat_miniprogram" => "微信订阅消息（本期未启用）".into(),
                _ => "—".into(),
            };

            ChannelView {
                id: ch.id.clone(),
                name: ch.name.clone(),
                type_label: channel_label(&ch.channel_type).into(),
                type_class: ch.channel_type.clone(),
                config_summary,
                linked: on,
                enabled: ch.enabled,
                testable: ch.channel_type != "wechat_miniprogram" && !is_autolock,
                local,
                status_text: status_text.into(),
                status_class: status_class.into(),
                url,
                to,
                group,
                agent_url,
                agent_token,
                agent_source: agent_source.into(),
                global_agent_url: global_agent_url.to_string(),
                global_agent_token_set,
                lock_num_seats,
                lock_max_retries,
                lock_seats_text,
            }
        })
        .collect()
}

async fn build_detail(st: &Arc<AppState>, id: &str) -> anyhow::Result<DetailTemplate> {
    let w = st
        .db
        .get_watch(id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("watch 不存在"))?;
    let (version, now) = meta();
    let settings = Settings::load(&st.db).await.ok();
    let city_id = settings.as_ref().map(|s| s.maoyan_city_id).unwrap_or(10);
    let global_agent_url = settings
        .as_ref()
        .map(|s| s.agent_url.clone())
        .unwrap_or_else(|| crate::db::DEFAULT_AGENT_URL.to_string());
    let global_agent_token_set = settings
        .as_ref()
        .and_then(|s| s.agent_token.as_ref())
        .is_some();
    let status = st.watch_status_snapshot();
    let stt = status.get(id);
    let (status_text, status_class) = watch_status_text(stt, w.enabled);
    let cinemas = st.db.watch_cinemas(id).await?;
    let selected_set: HashSet<String> = cinemas.iter().cloned().collect();
    let all_cinemas = st.db.list_cinemas().await?;
    let names: HashMap<String, String> = all_cinemas
        .iter()
        .map(|(id, name, _)| (id.clone(), name.clone()))
        .collect();
    let cinema_choices: Vec<CinemaChoiceView> = all_cinemas
        .iter()
        .map(|(cid, name, _)| CinemaChoiceView {
            id: cid.clone(),
            name: if name.is_empty() {
                cid.clone()
            } else {
                name.clone()
            },
            selected: selected_set.contains(cid),
        })
        .collect();
    let cinema_views = cinemas
        .iter()
        .map(|cid| LabelView {
            id: cid.clone(),
            label: {
                let n = names.get(cid).cloned().unwrap_or_default();
                if n.is_empty() {
                    cid.clone()
                } else {
                    format!("{} · {}", n, cid)
                }
            },
        })
        .collect();

    let all_channels = st.db.list_channels().await?;
    let linked: HashSet<String> = st
        .db
        .watch_channels(id)
        .await?
        .into_iter()
        .map(|c| c.id)
        .collect();
    let bindings = st.db.watch_channel_bindings(id).await?;
    let channels = channel_views(
        &all_channels,
        &linked,
        &bindings,
        &w,
        &global_agent_url,
        global_agent_token_set,
    );

    let subscribers = st
        .db
        .subscriptions_for_watch(id)
        .await?
        .into_iter()
        .map(|s| SubscriberView {
            id: s.id,
            user_id: s.user_id,
            user_name: if s.user_name.is_empty() {
                "—".into()
            } else {
                s.user_name
            },
            channel_name: s.channel_name.unwrap_or_else(|| "小程序".into()),
            created: s.created_at,
        })
        .collect();

    let all_users = st
        .db
        .list_users(500, 0)
        .await?
        .into_iter()
        .map(|u| LabelView {
            id: u.id.clone(),
            label: if u.nickname.is_empty() {
                u.id
            } else {
                u.nickname
            },
        })
        .collect();

    let states = st.db.watch_states(id).await?;
    let (base_total, base_imax, base_buckets) = baseline_stats(&states, w.imax_only);
    let baseline_days = baseline_days_from_states(&states, &names);
    let lock_rows = st
        .db
        .lock_states_for_watch(id)
        .await?
        .into_iter()
        .map(|ls| {
            let (status_text, status_class) = match ls.status.as_str() {
                "ok" => ("已锁", "on"),
                "exhausted" => ("重试耗尽", "bad"),
                "failed" => ("失败", "warn"),
                _ => ("待尝试", "flat"),
            };
            LockStateView {
                cinema_name: names
                    .get(&ls.cinema_id)
                    .cloned()
                    .unwrap_or_else(|| ls.cinema_id.clone()),
                cinema_id: ls.cinema_id,
                status_text: status_text.into(),
                status_class: status_class.into(),
                tries: ls.tries,
                last_error: ls.last_error.unwrap_or_default(),
            }
        })
        .collect();

    let chart = ChartView {
        enabled: true,
        id: w.id.clone(),
        name: if w.movie_name.is_empty() {
            w.id.clone()
        } else {
            w.movie_name.clone()
        },
        subtitle: format!("{} · {} 个影院", time_summary(&w), cinemas.len()),
        total: base_total,
        imax: base_imax,
        buckets: base_buckets,
    };

    Ok(DetailTemplate {
        version,
        now,
        page: "watches".into(),
        id: w.id.clone(),
        movie_id: w.movie_id,
        name: if w.movie_name.is_empty() {
            "未命名 watch".into()
        } else {
            w.movie_name.clone()
        },
        enabled: w.enabled,
        enabled_text: if w.enabled { "已启用" } else { "已停用" }.into(),
        status_text,
        status_class,
        time_spec: time_summary(&w),
        imax_text: if w.imax_only {
            "只看 IMAX"
        } else {
            "全部制式"
        }
        .into(),
        imax_only: w.imax_only,
        cinema_count: cinemas.len(),
        time_spec_json: TimeSpec::from_json(&w.time_spec_value())
            .map(|t| t.to_json().to_string())
            .unwrap_or_else(|_| json!({"type":"all"}).to_string()),
        lock_rows,
        cinema_choices,
        subscriber_count: w.subscriber_count,
        total: base_total,
        imax: base_imax,
        buckets: base_buckets,
        created_at: w.created_at.clone(),
        cinemas: cinema_views,
        channels,
        subscribers,
        users: all_users,
        chart,
        baseline_days,
        city_id,
    })
}

// ------------------------- users / settings -------------------------

async fn users_page(State(st): State<Arc<AppState>>) -> Response {
    let (version, now) = meta();
    let users = st
        .db
        .list_users(200, 0)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|u| UserItemView {
            id: u.id.clone(),
            name: if u.nickname.is_empty() {
                u.id.clone()
            } else {
                u.nickname
            },
            openid: u.openid.unwrap_or_default(),
            subs: u.subscription_count,
            created: u.created_at,
        })
        .collect::<Vec<_>>();
    let total = st.db.user_count().await.unwrap_or(0);
    let subscriptions = st.db.subscription_count().await.unwrap_or(0);
    let t = UsersTemplate {
        version,
        now,
        page: "users".into(),
        total,
        subscriptions,
        users,
    };
    render(&t).into_response()
}

// ------------------------- 面板登录 -------------------------

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn render_no_store<T: Template>(t: &T) -> Response {
    no_store(render(t).into_response())
}

fn render_no_store_status<T: Template>(t: &T, status: StatusCode) -> Response {
    let mut response = render(t).into_response();
    *response.status_mut() = status;
    no_store(response)
}

fn session_redirect(cookie: String, location: &str) -> Response {
    let mut response = Redirect::to(location).into_response();
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    no_store(response)
}

#[derive(Debug, Deserialize)]
struct SetupForm {
    password: String,
    confirm: String,
}

#[derive(Debug, Deserialize)]
struct LoginForm {
    password: String,
}

async fn setup_page(State(st): State<Arc<AppState>>) -> Response {
    let Some(auth) = st.auth.as_ref() else {
        return Redirect::to("/").into_response();
    };
    if auth.has_admin_password() {
        return Redirect::to("/login").into_response();
    }
    let (version, now) = meta();
    render_no_store(&SetupTemplate {
        version,
        now,
        error: String::new(),
    })
}

async fn setup_submit(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<SetupForm>,
) -> Response {
    let Some(auth_state) = st.auth.as_ref() else {
        return Redirect::to("/").into_response();
    };
    if !auth::same_origin_ok(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin request").into_response();
    }
    if auth_state.has_admin_password() {
        return Redirect::to("/login").into_response();
    }

    let len = form.password.chars().count();
    let error = if len < 8 {
        "面板密码至少需要 8 个字符"
    } else if len > 256 {
        "面板密码过长"
    } else if form.password != form.confirm {
        "两次输入的密码不一致"
    } else {
        ""
    };
    if !error.is_empty() {
        let (version, now) = meta();
        return render_no_store_status(
            &SetupTemplate {
                version,
                now,
                error: error.into(),
            },
            StatusCode::BAD_REQUEST,
        );
    }

    let _guard = auth_state.setup_lock.lock().await;
    if auth_state.has_admin_password() {
        return Redirect::to("/login").into_response();
    }

    let hash = match auth::hash_password_async(form.password).await {
        Ok(hash) => hash,
        Err(e) => {
            let (version, now) = meta();
            return render_no_store_status(
                &SetupTemplate {
                    version,
                    now,
                    error: format!("创建面板密码失败: {e}"),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    match st
        .db
        .set_setting_if_absent(auth::ADMIN_HASH_KEY, &hash)
        .await
    {
        Ok(true) => {}
        Ok(false) => return Redirect::to("/login").into_response(),
        Err(e) => {
            let (version, now) = meta();
            return render_no_store_status(
                &SetupTemplate {
                    version,
                    now,
                    error: format!("保存面板密码失败: {e}"),
                },
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    }

    auth_state.set_admin_hash(Some(hash));
    let secret = auth_state.rotate_session_secret();
    if let Err(e) = st
        .db
        .set_setting(auth::SESSION_SECRET_KEY, &auth::hex_encode(&secret))
        .await
    {
        tracing::warn!("持久化 panel session secret 失败: {e}");
    }

    let cookie = auth::session_cookie(&auth_state.issue_session(), auth_state.cookie_secure());
    session_redirect(cookie, "/")
}

async fn login_page(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let Some(auth_state) = st.auth.as_ref() else {
        return Redirect::to("/").into_response();
    };
    if auth::session_from_headers(&headers).is_some_and(|value| auth_state.valid_session(&value)) {
        return Redirect::to("/").into_response();
    }
    if !auth_state.has_admin_password() {
        return Redirect::to("/setup").into_response();
    }
    let (version, now) = meta();
    render_no_store(&LoginTemplate {
        version,
        now,
        error: String::new(),
    })
}

async fn login_submit(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Response {
    let Some(auth_state) = st.auth.as_ref() else {
        return Redirect::to("/").into_response();
    };
    if !auth::same_origin_ok(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin request").into_response();
    }
    if !auth_state.has_admin_password() {
        return Redirect::to("/setup").into_response();
    }
    if form.password.is_empty() {
        let (version, now) = meta();
        return render_no_store_status(
            &LoginTemplate {
                version,
                now,
                error: "请输入面板密码".into(),
            },
            StatusCode::BAD_REQUEST,
        );
    }

    if auth_state.verify_admin_password(form.password).await {
        let cookie = auth::session_cookie(&auth_state.issue_session(), auth_state.cookie_secure());
        session_redirect(cookie, "/")
    } else {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let (version, now) = meta();
        render_no_store_status(
            &LoginTemplate {
                version,
                now,
                error: "密码错误".into(),
            },
            StatusCode::UNAUTHORIZED,
        )
    }
}

async fn logout(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !auth::same_origin_ok(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin request").into_response();
    }
    let secure = st.auth.as_ref().map(|a| a.cookie_secure()).unwrap_or(false);
    session_redirect(auth::clear_session_cookie(secure), "/login")
}

async fn settings_page(State(st): State<Arc<AppState>>) -> Response {
    match build_settings(&st).await {
        Ok(t) => render(&t).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn build_settings(st: &Arc<AppState>) -> anyhow::Result<SettingsTemplate> {
    let map = st.db.settings_map().await?;
    let get = |k: &str, d: &str| map.get(k).cloned().unwrap_or_else(|| d.to_string());
    let (version, now) = meta();
    let channels = st
        .db
        .list_channels()
        .await?
        .iter()
        .filter(|c| c.channel_type != "autolock")
        .map(channel_config_view)
        .collect();
    Ok(SettingsTemplate {
        version,
        now,
        page: "settings".into(),
        interval: get("check_interval", "90"),
        silent: get("silent_window", ""),
        discord_webhook: get("discord_webhook", ""),
        agent_url: get("agent_url", crate::db::DEFAULT_AGENT_URL),
        agent_token: get("agent_token", ""),
        metrics_retention_days: get("metrics_retention_days", "7"),
        lock_headless: get("lock_headless", "true") != "false",
        maoyan_city_id: get("maoyan_city_id", "10"),
        wx_appid: get("wx_appid", ""),
        wx_secret: get("wx_secret", ""),
        wx_template_id: get("wx_template_id", ""),
        channels,
    })
}

// ------------------------- actions -------------------------

#[derive(Debug, Deserialize)]
struct IntervalForm {
    interval: u64,
}

async fn update_page() -> Response {
    let (version, now) = meta();
    let info = crate::update::check().await;
    render(&UpdateTemplate {
        version,
        now,
        page: "update".into(),
        info,
    })
    .into_response()
}

async fn action_update_install() -> Response {
    let (version, now) = meta();
    match crate::update::install_latest().await {
        Ok(out) => {
            crate::update::schedule_exit();
            render(&UpdateResultTemplate {
                version,
                now,
                page: "update".into(),
                ok: true,
                title: format!("已更新到 {}", out.version),
                message: if out.supervised {
                    "服务正在由 launchd 重启；几秒后刷新页面。".into()
                } else {
                    "已下载并替换二进制，重启助手会在当前进程退出后拉起新版本；几秒后刷新页面。"
                        .into()
                },
            })
            .into_response()
        }
        Err(e) => render(&UpdateResultTemplate {
            version,
            now,
            page: "update".into(),
            ok: false,
            title: "更新失败".into(),
            message: e.to_string(),
        })
        .into_response(),
    }
}

async fn action_monitor_toggle(State(st): State<Arc<AppState>>) -> Response {
    let enabled = Settings::load(&st.db)
        .await
        .map(|s| s.monitor_enabled)
        .unwrap_or(true);
    let next = !enabled;
    let _ = st
        .db
        .set_setting("monitor_enabled", if next { "true" } else { "false" })
        .await;
    if let Ok(mut h) = st.health.lock() {
        h.paused = !next;
        if !next {
            h.next_poll_at = None;
        }
    }
    st.push_event(
        "settings",
        None,
        format!("监控服务{}", if next { "已恢复" } else { "已暂停" }),
    );
    st.wake.notify_one();
    Redirect::to("/").into_response()
}

async fn action_interval(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<IntervalForm>,
) -> Response {
    let interval = f.interval.max(1);
    if st
        .db
        .set_setting("check_interval", &interval.to_string())
        .await
        .is_err()
    {
        return Redirect::to("/").into_response();
    }
    st.wake.notify_one();
    st.push_event("settings", None, format!("检查间隔改为 {}s", interval));
    if htmx(&headers) {
        match build_dashboard(&st, None).await {
            Ok(t) => render(&t).into_response(),
            Err(e) => {
                (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
            }
        }
    } else {
        Redirect::to("/").into_response()
    }
}

#[derive(Debug, Deserialize)]
struct SettingsForm {
    #[serde(default)]
    interval: Option<u64>,
    #[serde(default)]
    silent: Option<String>,
    #[serde(default)]
    discord_webhook: Option<String>,
    #[serde(default)]
    agent_url: Option<String>,
    #[serde(default)]
    agent_token: Option<String>,
    #[serde(default)]
    metrics_retention_days: Option<u32>,
    #[serde(default)]
    lock_headless: Option<String>,
    #[serde(default)]
    maoyan_city_id: Option<u32>,
    #[serde(default)]
    wx_appid: Option<String>,
    #[serde(default)]
    wx_secret: Option<String>,
    #[serde(default)]
    wx_template_id: Option<String>,
}

async fn action_settings(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(f): Form<SettingsForm>,
) -> Response {
    let mut pairs: Vec<(String, String)> = Vec::new();
    if let Some(v) = f.interval {
        pairs.push(("check_interval".into(), v.max(1).to_string()));
    }
    if let Some(v) = f.silent {
        let v = v.trim().to_string();
        if !v.is_empty() && crate::silent::SilentWindow::parse(&v).is_err() {
            return Redirect::to("/settings").into_response();
        }
        pairs.push(("silent_window".into(), v));
    }
    for (k, v) in [
        ("discord_webhook", f.discord_webhook),
        ("agent_url", f.agent_url),
        ("agent_token", f.agent_token),
        ("wx_appid", f.wx_appid),
        ("wx_secret", f.wx_secret),
        ("wx_template_id", f.wx_template_id),
    ] {
        if let Some(v) = v {
            pairs.push((k.into(), v.trim().to_string()));
        }
    }
    if let Some(v) = f.metrics_retention_days {
        pairs.push(("metrics_retention_days".into(), v.clamp(1, 365).to_string()));
    }
    if let Some(v) = f.lock_headless {
        pairs.push(("lock_headless".into(), v));
    }
    if let Some(v) = f.maoyan_city_id {
        pairs.push(("maoyan_city_id".into(), v.max(1).to_string()));
    }
    if st.db.set_settings(&pairs).await.is_err() {
        return Redirect::to("/settings").into_response();
    }
    st.wake.notify_one();
    st.push_event("settings", None, "全局设置已保存");
    if htmx(&headers) {
        match build_settings(&st).await {
            Ok(t) => render(&t).into_response(),
            Err(e) => {
                (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
            }
        }
    } else {
        Redirect::to("/settings").into_response()
    }
}

fn split_ids(s: &str) -> Vec<String> {
    s.split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(String::from)
        .collect()
}

#[derive(Debug, Deserialize, Default)]
struct NewWatchQuery {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    movie_error: Option<u8>,
}

async fn build_new_watch(
    st: &Arc<AppState>,
    error: String,
    movie_error: bool,
) -> anyhow::Result<NewWatchTemplate> {
    let (version, now) = meta();
    let city_id = Settings::load(&st.db)
        .await
        .map(|s| s.maoyan_city_id)
        .unwrap_or(10);
    let cinemas = st
        .db
        .list_cinemas()
        .await?
        .into_iter()
        .map(|(id, name, _)| CinemaChoiceView {
            id,
            name,
            selected: false,
        })
        .collect();
    Ok(NewWatchTemplate {
        version,
        now,
        page: "watches".into(),
        error,
        movie_error,
        cinemas,
        time_spec_json: json!({"type":"all"}).to_string(),
        city_id,
    })
}

async fn new_watch_page(
    State(st): State<Arc<AppState>>,
    Query(q): Query<NewWatchQuery>,
) -> Response {
    match build_new_watch(
        &st,
        q.error.unwrap_or_default(),
        q.movie_error.unwrap_or(0) != 0,
    )
    .await
    {
        Ok(t) => render(&t).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

fn parse_time_spec_form(s: &str) -> Result<TimeSpec, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(TimeSpec::All);
    }
    let v: Value = serde_json::from_str(s).map_err(|e| format!("时间选择器数据错误: {}", e))?;
    TimeSpec::from_json(&v).map_err(|e| e.to_string())
}

#[derive(Debug, Deserialize)]
struct CreateWatchForm {
    movie_id: i64,
    cinemas: String,
    #[serde(default)]
    time_spec: String,
    #[serde(default)]
    imax_only: Option<String>,
}

async fn action_create_watch(
    State(st): State<Arc<AppState>>,
    Form(f): Form<CreateWatchForm>,
) -> Response {
    let error_page = |st: &Arc<AppState>, msg: String| {
        let st = st.clone();
        async move {
            match build_new_watch(&st, msg, false).await {
                Ok(t) => render(&t).into_response(),
                Err(e) => {
                    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
                }
            }
        }
    };
    let cinemas = split_ids(&f.cinemas);
    if cinemas.is_empty() {
        return error_page(&st, "请至少选择一家影院".into()).await;
    }
    let time_spec = match parse_time_spec_form(&f.time_spec) {
        Ok(t) => t,
        Err(e) => return error_page(&st, format!("时间选择错误：{}", e)).await,
    };
    let movie_name = match st.db.movie_name(f.movie_id).await {
        Ok(Some(n)) if !n.trim().is_empty() => Some(n),
        _ => return error_page(&st, "请选择一部电影".into()).await,
    };
    let spec = crate::db::NewWatch {
        movie_id: f.movie_id,
        movie_name,
        cinemas,
        time_spec: time_spec.to_json(),
        imax_only: f.imax_only.is_some(),
        created_by: None,
        visibility: "public".into(),
        auto_lock: false,
        lock_num_seats: crate::db::DEFAULT_LOCK_NUM_SEATS,
        lock_max_retries: crate::db::DEFAULT_LOCK_MAX_RETRIES,
        lock_seats: Vec::new(),
    };
    match st.db.create_or_reuse_watch(&spec).await {
        Ok((wid, created)) => {
            st.wake.notify_one();
            st.push_event(
                "watch",
                Some(&wid),
                format!("{} watch {}", if created { "创建" } else { "复用" }, wid),
            );
            Redirect::to(&format!("/watches/{}", wid)).into_response()
        }
        Err(e) => error_page(&st, format!("创建失败：{}", e)).await,
    }
}

#[derive(Debug, Deserialize)]
struct EditWatchForm {
    #[serde(default)]
    movie_id: Option<i64>,
    cinemas: String,
    #[serde(default)]
    time_spec: String,
    #[serde(default)]
    imax_only: Option<String>,
}

async fn action_edit_watch(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
    Form(f): Form<EditWatchForm>,
) -> Response {
    let cinemas = split_ids(&f.cinemas);
    if cinemas.is_empty() {
        return Redirect::to(&format!("/watches/{}", id)).into_response();
    }
    let time_spec = match parse_time_spec_form(&f.time_spec) {
        Ok(t) => t,
        Err(_) => return Redirect::to(&format!("/watches/{}", id)).into_response(),
    };
    let movie_name = match f.movie_id {
        Some(mid) => match st.db.movie_name(mid).await {
            Ok(Some(name)) => Some(name),
            _ => None,
        },
        None => None,
    };
    let patch = crate::db::WatchPatch {
        movie_id: f.movie_id,
        movie_name,
        cinemas: Some(cinemas),
        time_spec: Some(time_spec.to_json()),
        imax_only: Some(f.imax_only.is_some()),
        ..Default::default()
    };
    match st.db.update_watch(&id, &patch).await {
        Ok((new_id, changed)) => {
            st.wake.notify_one();
            st.push_event(
                "watch",
                Some(&new_id),
                if changed {
                    format!("编辑 {} 过滤条件；基线已清空", new_id)
                } else {
                    format!("编辑 {}", new_id)
                },
            );
            Redirect::to(&format!("/watches/{}", new_id)).into_response()
        }
        Err(_) => Redirect::to(&format!("/watches/{}", id)).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct RefreshMoviesForm {
    #[serde(default)]
    next: Option<String>,
}

async fn action_refresh_movies(
    State(st): State<Arc<AppState>>,
    Form(f): Form<RefreshMoviesForm>,
) -> Response {
    let mut total = 0usize;
    let mut failed = false;
    for (show_type, _) in [(1u8, "热映"), (2u8, "即将"), (3u8, "经典")] {
        match crate::maoyan::fetch_films_list_async(show_type).await {
            Ok(rows) => {
                if let Ok(n) = st.db.upsert_movies(&rows, show_type).await {
                    total += n;
                } else {
                    failed = true;
                }
            }
            Err(_) => failed = true,
        }
    }
    st.push_event("movies", None, format!("电影列表刷新：{} 条", total));
    let next = f.next.unwrap_or_else(|| "/watches/new".into());
    let next = if next.starts_with('/') {
        next
    } else {
        "/watches/new".into()
    };
    if failed {
        Redirect::to(&format!("{}?movie_error=1", next)).into_response()
    } else {
        Redirect::to(&next).into_response()
    }
}

#[derive(Debug, Deserialize)]
struct SubscribeForm {
    user_id: String,
    watch_id: String,
}

async fn action_subscribe(
    State(st): State<Arc<AppState>>,
    Form(f): Form<SubscribeForm>,
) -> Response {
    let _ = st.db.subscribe(&f.user_id, &f.watch_id, None).await;
    st.push_event(
        "subscription",
        Some(&f.watch_id),
        format!("用户 {} 订阅 watch", f.user_id),
    );
    Redirect::to(&format!("/watches/{}", f.watch_id)).into_response()
}

#[derive(Debug, Deserialize)]
struct LockForm {
    #[serde(default)]
    auto_lock: Option<String>,
    #[serde(default)]
    lock_num_seats: Option<u64>,
    #[serde(default)]
    lock_max_retries: Option<u64>,
    #[serde(default)]
    lock_seats: String,
}

async fn action_update_lock(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
    Form(f): Form<LockForm>,
) -> Response {
    let patch = crate::db::WatchPatch {
        auto_lock: Some(f.auto_lock.is_some()),
        lock_num_seats: f.lock_num_seats.map(|n| n.max(1)),
        lock_max_retries: f.lock_max_retries.map(|n| n.max(1)),
        lock_seats: Some(split_ids(&f.lock_seats)),
        ..Default::default()
    };
    if st.db.update_watch(&id, &patch).await.is_ok() {
        st.push_event(
            "lock",
            Some(&id),
            format!(
                "自动锁票{}；{} 座 ×{} 次重试",
                if f.auto_lock.is_some() {
                    "已开启"
                } else {
                    "已关闭"
                },
                f.lock_num_seats.unwrap_or(2),
                f.lock_max_retries.unwrap_or(3)
            ),
        );
    }
    Redirect::to(&format!("/watches/{}", id)).into_response()
}

async fn action_toggle_watch(State(st): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    if let Ok(Some(w)) = st.db.get_watch(&id).await {
        let _ = st.db.set_watch_enabled(&id, !w.enabled).await;
        st.wake.notify_one();
        st.push_event(
            "watch",
            Some(&id),
            format!("{} watch {}", if w.enabled { "停用" } else { "启用" }, id),
        );
    }
    match build_dashboard(&st, None).await {
        Ok(t) => render(&t).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn action_toggle_watch_detail(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    if let Ok(Some(w)) = st.db.get_watch(&id).await {
        let _ = st.db.set_watch_enabled(&id, !w.enabled).await;
        st.wake.notify_one();
        st.push_event(
            "watch",
            Some(&id),
            format!("{} watch {}", if w.enabled { "停用" } else { "启用" }, id),
        );
    }
    match build_detail(&st, &id).await {
        Ok(t) => render(&t).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn action_toggle_channel(
    State(st): State<Arc<AppState>>,
    Path((wid, cid)): Path<(String, String)>,
) -> Response {
    let linked = st
        .db
        .watch_channels(&wid)
        .await
        .map(|v| v.iter().any(|c| c.id == cid))
        .unwrap_or(false);
    let next = !linked;
    if cid == "autolock-local" {
        let patch = crate::db::WatchPatch {
            auto_lock: Some(next),
            ..Default::default()
        };
        let _ = st.db.update_watch(&wid, &patch).await;
    }
    let _ = st.db.set_watch_channel(&wid, &cid, next).await;
    st.push_event(
        "watch",
        Some(&wid),
        format!("{} 渠道 {}", if linked { "解绑" } else { "绑定" }, cid),
    );
    match build_detail(&st, &wid).await {
        Ok(t) => render(&t).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct WatchChannelConfigForm {
    #[serde(default)]
    url: String,
    #[serde(default)]
    to: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    agent_url: String,
    #[serde(default)]
    agent_token: String,
    /// `global` = 复用 Settings；`custom` = 使用本 watch 地址。
    /// 旧表单没有该字段时按 agent_url 是否为空推断。
    #[serde(default)]
    agent_source: Option<String>,
    #[serde(default)]
    lock_num_seats: Option<i64>,
    #[serde(default)]
    lock_max_retries: Option<i64>,
    #[serde(default)]
    lock_seats: String,
}

async fn action_watch_channel_config(
    State(st): State<Arc<AppState>>,
    Path((wid, cid)): Path<(String, String)>,
    Form(f): Form<WatchChannelConfigForm>,
) -> Response {
    let Ok(Some(ch)) = st.db.get_channel(&cid).await else {
        return Redirect::to(&format!("/watches/{}", wid)).into_response();
    };
    let mut cfg = json!({});
    if let Some(obj) = cfg.as_object_mut() {
        let agent_url = f.agent_url.trim();
        let agent_token = f.agent_token.trim();
        let custom_agent = match f.agent_source.as_deref().map(str::trim) {
            Some("global") => false,
            Some("custom") => true,
            // 兼容旧表单 / API：填了地址就视为自定义。
            _ => !agent_url.is_empty(),
        };
        if custom_agent {
            if !agent_url.is_empty() {
                obj.insert("agent_url".into(), json!(agent_url));
            }
            if !agent_token.is_empty() {
                obj.insert("agent_token".into(), json!(agent_token));
            }
        }
        match ch.channel_type.as_str() {
            "discord" | "webhook" => {
                obj.insert("url".into(), json!(f.url.trim()));
            }
            "email" => {
                obj.insert("to".into(), json!(f.to.trim()));
            }
            "xhs_local" => {
                obj.insert("group".into(), json!(f.group.trim()));
            }
            "autolock" => {
                let seats = f.lock_num_seats.unwrap_or(2).max(1) as u64;
                let retries = f.lock_max_retries.unwrap_or(3).max(1) as u64;
                obj.insert("lock_num_seats".into(), json!(seats));
                obj.insert("lock_max_retries".into(), json!(retries));
                obj.insert("lock_seats".into(), json!(split_ids(&f.lock_seats)));
                let patch = crate::db::WatchPatch {
                    auto_lock: Some(true),
                    lock_num_seats: Some(seats),
                    lock_max_retries: Some(retries),
                    lock_seats: Some(split_ids(&f.lock_seats)),
                    ..Default::default()
                };
                let _ = st.db.update_watch(&wid, &patch).await;
            }
            _ => {}
        }
    }
    let _ = st.db.bind_watch_channel(&wid, &cid, &cfg).await;
    st.push_event(
        "watch",
        Some(&wid),
        format!("渠道 {} 已保存并绑定", ch.name),
    );
    Redirect::to(&format!("/watches/{}", wid)).into_response()
}

#[derive(Debug, Deserialize)]
struct TestChannelForm {
    watch_id: String,
}

async fn action_test_channel(
    State(st): State<Arc<AppState>>,
    Path(cid): Path<String>,
    Form(f): Form<TestChannelForm>,
) -> Response {
    let _ = super::dispatcher::test_channel(st.clone(), &cid, &f.watch_id).await;
    match build_detail(&st, &f.watch_id).await {
        Ok(t) => render(&t).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct ChannelConfigForm {
    name: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    to: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    enabled: Option<String>,
}

async fn action_channel_config(
    State(st): State<Arc<AppState>>,
    Path(cid): Path<String>,
    Form(f): Form<ChannelConfigForm>,
) -> Response {
    if let Ok(Some(ch)) = st.db.get_channel(&cid).await {
        let cfg = match ch.channel_type.as_str() {
            "discord" | "webhook" => json!({"url": f.url.trim()}),
            "email" => json!({"to": f.to.trim()}),
            "xhs_local" => json!({"group": f.group.trim()}),
            _ => ch.config(),
        };
        let enabled = f.enabled.as_deref().map(|v| v == "on" || v == "true");
        let _ = st
            .db
            .update_channel(&cid, Some(f.name.trim()), Some(&cfg), enabled)
            .await;
        st.push_event("settings", None, format!("渠道 {} 配置已保存", ch.name));
    }
    Redirect::to("/settings").into_response()
}

#[derive(Debug, Deserialize)]
struct CreateUserForm {
    nickname: String,
    #[serde(default)]
    openid: String,
}

async fn action_create_user(
    State(st): State<Arc<AppState>>,
    Form(f): Form<CreateUserForm>,
) -> Response {
    let openid = f.openid.trim();
    let _ = st
        .db
        .create_user(
            if openid.is_empty() {
                None
            } else {
                Some(openid)
            },
            f.nickname.trim(),
        )
        .await;
    Redirect::to("/users").into_response()
}

// Keep `Value` used for future channel-specific forms.
#[allow(dead_code)]
fn _unused(_: Value) {}
