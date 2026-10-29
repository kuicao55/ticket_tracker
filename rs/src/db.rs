//! SQLite 数据层（v4 无头主服务）。
//!
//! 通知投递不持久化；这里存 settings / cinemas / polls / watches / watch_state /
//! watch_metrics / users / channels / subscriptions。

use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{FromRow, SqlitePool};
use uuid::Uuid;

use crate::time_spec::TimeSpec;

pub const DEFAULT_CHECK_INTERVAL: u64 = 90;
pub const DEFAULT_FETCH_CONCURRENCY: usize = 8;
pub const DEFAULT_SILENT_WINDOW: &str = "01:00-06:00";
pub const DEFAULT_AGENT_URL: &str = "http://127.0.0.1:8790";
pub const DEFAULT_METRICS_RETENTION_DAYS: u32 = 7;
pub const DEFAULT_LOCK_NUM_SEATS: u64 = 2;
pub const DEFAULT_LOCK_MAX_RETRIES: u64 = 3;

#[derive(Clone)]
pub struct Db {
    pub pool: SqlitePool,
}

// ------------------------- 行类型 -------------------------

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct PollRow {
    pub cinema_id: String,
    pub cinema_name: String,
    pub watch_count: i64,
    pub last_polled_at: Option<String>,
    pub last_ok: Option<bool>,
    pub last_error: Option<String>,
    pub shows_count: i64,
    pub last_duration_ms: i64,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct WatchRow {
    pub id: String,
    pub filter_key: String,
    pub movie_id: i64,
    pub movie_name: String,
    pub time_spec: String,
    pub imax_only: bool,
    pub enabled: bool,
    pub created_by: Option<String>,
    pub visibility: String,
    pub auto_lock: bool,
    pub lock_num_seats: i64,
    pub lock_max_retries: i64,
    pub lock_seats: String,
    pub created_at: String,
    pub updated_at: String,
    #[sqlx(default)]
    pub subscriber_count: i64,
}

impl WatchRow {
    pub fn time_spec_value(&self) -> Value {
        serde_json::from_str(&self.time_spec).unwrap_or_else(|_| json!({"type":"all"}))
    }

    pub fn lock_seats_value(&self) -> Vec<String> {
        serde_json::from_str(&self.lock_seats).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ChannelRow {
    pub id: String,
    pub name: String,
    #[sqlx(rename = "type")]
    #[serde(rename = "type")]
    pub channel_type: String,
    pub config_json: String,
    pub enabled: bool,
    pub created_at: String,
}

impl ChannelRow {
    pub fn config(&self) -> Value {
        serde_json::from_str(&self.config_json).unwrap_or_else(|_| json!({}))
    }
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct UserRow {
    pub id: String,
    pub openid: Option<String>,
    pub unionid: Option<String>,
    pub nickname: String,
    pub avatar_url: Option<String>,
    pub status: String,
    pub created_at: String,
    #[sqlx(default)]
    pub subscription_count: i64,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct SubscriptionRow {
    pub id: String,
    pub user_id: String,
    pub watch_id: String,
    pub channel_id: Option<String>,
    pub enabled: bool,
    pub created_at: String,
    #[sqlx(default)]
    pub user_name: String,
    #[sqlx(default)]
    pub watch_name: String,
    #[sqlx(default)]
    pub channel_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct MetricRow {
    pub recorded_at: String,
    pub total_shows: i64,
    pub imax_shows: i64,
    pub bucket_count: i64,
    pub added_shows: i64,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct PollHealthRow {
    pub id: i64,
    pub recorded_at: String,
    pub recorded_ts: i64,
    pub cinemas_total: i64,
    pub cinemas_ok: i64,
    pub cinemas_err: i64,
    pub blocked_cinemas: i64,
    pub duration_ms: i64,
    pub error_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct PushTarget {
    pub user_id: String,
    pub openid: Option<String>,
    pub nickname: String,
    pub channel_id: Option<String>,
    pub channel_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct MovieRow {
    pub id: i64,
    pub name: String,
    pub show_type: i64,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct LockStateRow {
    pub watch_id: String,
    pub cinema_id: String,
    pub status: String,
    pub tries: i64,
    pub last_result: Option<String>,
    pub last_error: Option<String>,
    pub last_event_id: Option<String>,
    pub updated_at: String,
}

/// monitor 一轮需要的 watch 数据：DB 行 + 影院 + 上次 state。
#[derive(Debug, Clone)]
pub struct MonitorWatch {
    pub row: WatchRow,
    pub cinemas: Vec<String>,
    pub state: HashMap<String, Value>,
}

impl MonitorWatch {
    /// 转成 engine 兼容的 Value（engine 目前全部基于 Value）。
    pub fn to_engine_value(&self) -> Value {
        json!({
            "id": self.row.id,
            "movie_id": self.row.movie_id,
            "movie_name": self.row.movie_name,
            "cinemas": self.cinemas,
            "time_spec": self.row.time_spec_value(),
            "imax_only": self.row.imax_only,
            "enabled": self.row.enabled,
            "state": self.state,
        })
    }
}

/// 新建 watch 的输入。
#[derive(Debug, Clone)]
pub struct NewWatch {
    pub movie_id: i64,
    pub movie_name: Option<String>,
    pub cinemas: Vec<String>,
    pub time_spec: Value,
    pub imax_only: bool,
    pub created_by: Option<String>,
    pub visibility: String,
    pub auto_lock: bool,
    pub lock_num_seats: u64,
    pub lock_max_retries: u64,
    pub lock_seats: Vec<String>,
}

/// 编辑 watch 的输入（None = 不改）。
#[derive(Debug, Clone, Default)]
pub struct WatchPatch {
    pub movie_id: Option<i64>,
    pub movie_name: Option<String>,
    pub cinemas: Option<Vec<String>>,
    pub time_spec: Option<Value>,
    pub imax_only: Option<bool>,
    pub enabled: Option<bool>,
    pub auto_lock: Option<bool>,
    pub lock_num_seats: Option<u64>,
    pub lock_max_retries: Option<u64>,
    pub lock_seats: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub check_interval: u64,
    pub silent_window: Option<String>,
    pub fetch_concurrency: usize,
    pub discord_webhook: Option<String>,
    pub agent_url: String,
    pub agent_token: Option<String>,
    pub panel_token: Option<String>,
    pub wx_appid: Option<String>,
    pub wx_secret: Option<String>,
    pub wx_template_id: Option<String>,
    pub metrics_retention_days: u32,
    pub lock_headless: bool,
    pub maoyan_city_id: u32,
    pub monitor_enabled: bool,
}

impl Settings {
    pub async fn load(db: &Db) -> Result<Self> {
        let map = db.settings_map().await?;
        let s = |k: &str| map.get(k).cloned().filter(|v| !v.trim().is_empty());
        let u = |k: &str, default: u64| {
            map.get(k)
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(default)
        };
        Ok(Self {
            check_interval: u("check_interval", DEFAULT_CHECK_INTERVAL).max(1),
            silent_window: s("silent_window"),
            fetch_concurrency: u("fetch_concurrency", DEFAULT_FETCH_CONCURRENCY as u64).clamp(1, 64)
                as usize,
            discord_webhook: s("discord_webhook"),
            agent_url: s("agent_url").unwrap_or_else(|| DEFAULT_AGENT_URL.to_string()),
            agent_token: s("agent_token"),
            panel_token: s("panel_token"),
            wx_appid: s("wx_appid"),
            wx_secret: s("wx_secret"),
            wx_template_id: s("wx_template_id"),
            metrics_retention_days: u(
                "metrics_retention_days",
                DEFAULT_METRICS_RETENTION_DAYS as u64,
            )
            .clamp(1, 365) as u32,
            lock_headless: map
                .get("lock_headless")
                .map(|v| v != "false" && v != "0")
                .unwrap_or(true),
            maoyan_city_id: u("maoyan_city_id", 10).clamp(1, 9999) as u32,
            monitor_enabled: map
                .get("monitor_enabled")
                .map(|v| v != "false" && v != "0")
                .unwrap_or(true),
        })
    }

    pub fn silent(&self) -> Option<crate::silent::SilentWindow> {
        self.silent_window
            .as_deref()
            .and_then(|s| crate::silent::SilentWindow::parse(s).ok())
    }
}

// ------------------------- 初始化 -------------------------

fn watch_filter_key(movie_id: i64, cinemas: &[String], time_spec: &Value, imax: bool) -> String {
    let time_key = TimeSpec::from_json(time_spec)
        .map(|t| t.key())
        .unwrap_or_else(|_| "all".into());
    format!("{}|{}|{}|{}", movie_id, cinemas.join(","), time_key, imax)
}

impl Db {
    pub async fn connect(path: &str) -> Result<Self> {
        if let Some(dir) = std::path::Path::new(path).parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).ok();
            }
        }
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(5))
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await
            .with_context(|| format!("打开 SQLite 失败: {}", path))?;
        let db = Self { pool };
        db.migrate().await?;
        db.seed_defaults().await?;
        Ok(db)
    }

    pub async fn connect_memory() -> Result<Self> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        let db = Self { pool };
        db.migrate().await?;
        db.seed_defaults().await?;
        Ok(db)
    }

    async fn migrate(&self) -> Result<()> {
        sqlx::migrate!("./migrations")
            .run(&self.pool)
            .await
            .context("SQLite migration 失败")?;
        Ok(())
    }

    async fn seed_defaults(&self) -> Result<()> {
        let defaults = [
            ("check_interval", DEFAULT_CHECK_INTERVAL.to_string()),
            ("silent_window", DEFAULT_SILENT_WINDOW.to_string()),
            ("fetch_concurrency", DEFAULT_FETCH_CONCURRENCY.to_string()),
            ("agent_url", DEFAULT_AGENT_URL.to_string()),
            (
                "metrics_retention_days",
                DEFAULT_METRICS_RETENTION_DAYS.to_string(),
            ),
            ("lock_headless", "true".to_string()),
            ("maoyan_city_id", "10".to_string()),
        ];
        let mut tx = self.pool.begin().await?;
        for (k, v) in defaults {
            sqlx::query("INSERT OR IGNORE INTO settings(key, value) VALUES (?, ?)")
                .bind(k)
                .bind(v)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;

        // 内置影院预设进 DB，保证 Web 选择器有基础列表。
        for (_, p) in crate::presets::PRESETS {
            let _ = self.upsert_cinema(p.id, p.name, false).await;
        }
        // 已有 watch 的电影进缓存，保证即使猫眼列表接口失败也能选择。
        sqlx::query(
            "INSERT OR IGNORE INTO movies(id, name, show_type, updated_at)
             SELECT movie_id, movie_name, 0, datetime('now')
             FROM watches WHERE movie_id > 0 AND movie_name <> ''",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ------------------------- settings -------------------------

    pub async fn settings_map(&self) -> Result<HashMap<String, String>> {
        let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM settings")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().collect())
    }

    pub async fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.0))
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO settings(key, value, updated_at) VALUES (?, ?, datetime('now'))
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = datetime('now')",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 仅当 key 不存在时写入；返回 true 表示本次写入成功。
    /// 用于首次设密，避免并发 setup 覆盖已有密码。
    pub async fn set_setting_if_absent(&self, key: &str, value: &str) -> Result<bool> {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO settings(key, value, updated_at)
             VALUES (?, ?, datetime('now'))",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn set_settings(&self, pairs: &[(String, String)]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for (k, v) in pairs {
            sqlx::query(
                "INSERT INTO settings(key, value, updated_at) VALUES (?, ?, datetime('now'))
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = datetime('now')",
            )
            .bind(k)
            .bind(v)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    // ------------------------- cinemas / polls -------------------------

    pub async fn upsert_cinema(&self, id: &str, name: &str, favorite: bool) -> Result<()> {
        sqlx::query(
            "INSERT INTO cinemas(id, name, favorite) VALUES (?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                name = CASE WHEN excluded.name <> '' THEN excluded.name ELSE cinemas.name END,
                favorite = MAX(cinemas.favorite, excluded.favorite)",
        )
        .bind(id)
        .bind(name)
        .bind(if favorite { 1 } else { 0 })
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_cinemas(&self) -> Result<Vec<(String, String, bool)>> {
        let rows: Vec<(String, String, i64)> =
            sqlx::query_as("SELECT id, name, favorite FROM cinemas ORDER BY favorite DESC, id")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .map(|(id, name, fav)| (id, name, fav != 0))
            .collect())
    }

    /// 每轮开始前同步 poll 列表；不在本轮 plan 里的旧 poll 标记 watch_count=0。
    pub async fn sync_polls(&self, plan: &[(String, i64)]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE polls SET watch_count = 0")
            .execute(&mut *tx)
            .await?;
        for (cid, count) in plan {
            sqlx::query(
                "INSERT INTO polls(cinema_id, cinema_name, watch_count)
                 VALUES (?, COALESCE((SELECT name FROM cinemas WHERE id = ?), ''), ?)
                 ON CONFLICT(cinema_id) DO UPDATE SET
                    watch_count = excluded.watch_count,
                    cinema_name = CASE WHEN excluded.cinema_name <> '' THEN excluded.cinema_name ELSE polls.cinema_name END",
            )
            .bind(cid)
            .bind(cid)
            .bind(count)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn record_poll(
        &self,
        cinema_id: &str,
        cinema_name: &str,
        ok: bool,
        error: Option<&str>,
        shows_count: usize,
        duration_ms: u128,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE polls SET
                last_polled_at = datetime('now'),
                last_ok = ?,
                last_error = ?,
                shows_count = ?,
                last_duration_ms = ?,
                cinema_name = CASE WHEN ? <> '' THEN ? ELSE cinema_name END
             WHERE cinema_id = ?",
        )
        .bind(if ok { 1 } else { 0 })
        .bind(error)
        .bind(shows_count as i64)
        .bind(duration_ms as i64)
        .bind(cinema_name)
        .bind(cinema_name)
        .bind(cinema_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_polls(&self, limit: i64) -> Result<Vec<PollRow>> {
        Ok(sqlx::query_as::<_, PollRow>(
            "SELECT cinema_id, cinema_name, watch_count, last_polled_at, last_ok,
                    last_error, shows_count, last_duration_ms
             FROM polls
             WHERE watch_count > 0
             ORDER BY watch_count DESC, last_polled_at DESC
             LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn poll_count(&self) -> Result<i64> {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM polls WHERE watch_count > 0")
            .fetch_one(&self.pool)
            .await?;
        Ok(n)
    }

    // ------------------------- watches -------------------------

    pub async fn list_watches(&self, sort: &str, limit: i64, offset: i64) -> Result<Vec<WatchRow>> {
        let order = match sort {
            "recent" => "w.created_at DESC",
            "name" => "w.movie_name ASC",
            _ => "subscriber_count DESC, w.created_at DESC",
        };
        let sql = format!(
            "SELECT w.*, (SELECT COUNT(*) FROM subscriptions s WHERE s.watch_id = w.id) AS subscriber_count
             FROM watches w ORDER BY {} LIMIT ? OFFSET ?",
            order
        );
        Ok(sqlx::query_as::<_, WatchRow>(&sql)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn watch_count(&self) -> Result<i64> {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM watches")
            .fetch_one(&self.pool)
            .await?;
        Ok(n)
    }

    pub async fn get_watch(&self, id: &str) -> Result<Option<WatchRow>> {
        Ok(sqlx::query_as::<_, WatchRow>(
            "SELECT w.*, (SELECT COUNT(*) FROM subscriptions s WHERE s.watch_id = w.id) AS subscriber_count
             FROM watches w WHERE w.id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn watch_cinemas(&self, id: &str) -> Result<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT cinema_id FROM watch_cinemas WHERE watch_id = ? ORDER BY cinema_id",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.0).collect())
    }

    /// 一次取出所有 watch 的影院绑定，供列表页批量渲染，避免 N+1 查询。
    pub async fn all_watch_cinemas(&self) -> Result<HashMap<String, Vec<String>>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT watch_id, cinema_id FROM watch_cinemas ORDER BY watch_id, cinema_id",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        for (watch_id, cinema_id) in rows {
            out.entry(watch_id).or_default().push(cinema_id);
        }
        Ok(out)
    }

    pub async fn find_by_filter_key(&self, key: &str) -> Result<Option<WatchRow>> {
        Ok(sqlx::query_as::<_, WatchRow>(
            "SELECT w.*, (SELECT COUNT(*) FROM subscriptions s WHERE s.watch_id = w.id) AS subscriber_count
             FROM watches w WHERE w.filter_key = ?",
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn create_or_reuse_watch(&self, spec: &NewWatch) -> Result<(String, bool)> {
        if spec.cinemas.is_empty() {
            anyhow::bail!("至少需要一个影院");
        }
        let mut cinemas: Vec<String> = spec
            .cinemas
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        cinemas.sort();
        cinemas.dedup();

        let key = watch_filter_key(spec.movie_id, &cinemas, &spec.time_spec, spec.imax_only);

        if let Some(existing) = self.find_by_filter_key(&key).await? {
            if existing.movie_name.is_empty() {
                if let Some(name) = &spec.movie_name {
                    let _ = sqlx::query("UPDATE watches SET movie_name = ? WHERE id = ?")
                        .bind(name)
                        .bind(&existing.id)
                        .execute(&self.pool)
                        .await;
                }
            }
            // 重复 POST 只允许“开启/调整”锁票，不会把已有 true 意外关掉。
            if spec.auto_lock {
                let _ = sqlx::query(
                    "UPDATE watches SET auto_lock = 1, lock_num_seats = ?,
                        lock_max_retries = ?, lock_seats = ?, updated_at = datetime('now')
                     WHERE id = ?",
                )
                .bind(spec.lock_num_seats as i64)
                .bind(spec.lock_max_retries as i64)
                .bind(json!(spec.lock_seats).to_string())
                .bind(&existing.id)
                .execute(&self.pool)
                .await;
                let _ = self
                    .set_watch_channel(&existing.id, "autolock-local", true)
                    .await;
            }
            return Ok((existing.id, false));
        }

        let wid = format!("w_{}", &Uuid::new_v4().to_string()[..6]);
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO watches(id, filter_key, movie_id, movie_name, time_spec,
                                 imax_only, enabled, created_by, visibility,
                                 auto_lock, lock_num_seats, lock_max_retries, lock_seats)
             VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&wid)
        .bind(&key)
        .bind(spec.movie_id)
        .bind(spec.movie_name.clone().unwrap_or_default())
        .bind(spec.time_spec.to_string())
        .bind(if spec.imax_only { 1 } else { 0 })
        .bind(&spec.created_by)
        .bind(&spec.visibility)
        .bind(if spec.auto_lock { 1 } else { 0 })
        .bind(spec.lock_num_seats as i64)
        .bind(spec.lock_max_retries as i64)
        .bind(json!(spec.lock_seats).to_string())
        .execute(&mut *tx)
        .await?;

        for cid in &cinemas {
            sqlx::query("INSERT OR IGNORE INTO cinemas(id, name) VALUES (?, '')")
                .bind(cid)
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT INTO watch_cinemas(watch_id, cinema_id) VALUES (?, ?)")
                .bind(&wid)
                .bind(cid)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        if spec.auto_lock {
            let _ = self.set_watch_channel(&wid, "autolock-local", true).await;
        }
        Ok((wid, true))
    }

    pub async fn update_watch(&self, id: &str, patch: &WatchPatch) -> Result<(String, bool)> {
        let Some(old) = self.get_watch(id).await? else {
            anyhow::bail!("watch 不存在: {}", id);
        };
        let old_cinemas = self.watch_cinemas(id).await?;
        let movie_id = patch.movie_id.unwrap_or(old.movie_id);
        let movie_name = patch
            .movie_name
            .clone()
            .unwrap_or_else(|| old.movie_name.clone());
        let mut cinemas = patch.cinemas.clone().unwrap_or(old_cinemas);
        cinemas.sort();
        cinemas.dedup();
        let time_spec = patch
            .time_spec
            .clone()
            .unwrap_or_else(|| old.time_spec_value());
        let imax_only = patch.imax_only.unwrap_or(old.imax_only);
        let enabled = patch.enabled.unwrap_or(old.enabled);

        let key = watch_filter_key(movie_id, &cinemas, &time_spec, imax_only);
        let filter_changed = key != old.filter_key;

        if filter_changed {
            if let Some(target) = self.find_by_filter_key(&key).await? {
                if target.id != id {
                    self.merge_watch_into(id, &target.id).await?;
                    return Ok((target.id, true));
                }
            }
        }

        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE watches SET movie_id = ?, movie_name = ?, time_spec = ?,
                imax_only = ?, enabled = ?, filter_key = ?,
                auto_lock = COALESCE(?, auto_lock),
                lock_num_seats = COALESCE(?, lock_num_seats),
                lock_max_retries = COALESCE(?, lock_max_retries),
                lock_seats = COALESCE(?, lock_seats),
                updated_at = datetime('now')
             WHERE id = ?",
        )
        .bind(movie_id)
        .bind(&movie_name)
        .bind(time_spec.to_string())
        .bind(if imax_only { 1 } else { 0 })
        .bind(if enabled { 1 } else { 0 })
        .bind(&key)
        .bind(patch.auto_lock.map(|b| if b { 1 } else { 0 }))
        .bind(patch.lock_num_seats.map(|n| n as i64))
        .bind(patch.lock_max_retries.map(|n| n as i64))
        .bind(patch.lock_seats.as_ref().map(|s| json!(s).to_string()))
        .bind(id)
        .execute(&mut *tx)
        .await?;

        if filter_changed {
            sqlx::query("DELETE FROM watch_cinemas WHERE watch_id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            for cid in &cinemas {
                sqlx::query("INSERT OR IGNORE INTO cinemas(id, name) VALUES (?, '')")
                    .bind(cid)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("INSERT INTO watch_cinemas(watch_id, cinema_id) VALUES (?, ?)")
                    .bind(id)
                    .bind(cid)
                    .execute(&mut *tx)
                    .await?;
            }
            sqlx::query("DELETE FROM watch_state WHERE watch_id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM lock_state WHERE watch_id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        if let Some(on) = patch.auto_lock {
            let _ = self.set_watch_channel(id, "autolock-local", on).await;
        }
        Ok((id.to_string(), filter_changed))
    }

    /// 编辑后与已有 watch 过滤条件冲突：把订阅/渠道迁移过去，删除旧 watch。
    async fn merge_watch_into(&self, from: &str, to: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT OR IGNORE INTO subscriptions(id, user_id, watch_id, channel_id, enabled)
             SELECT lower(hex(randomblob(8))), user_id, ?, channel_id, enabled
             FROM subscriptions WHERE watch_id = ?",
        )
        .bind(to)
        .bind(from)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT OR IGNORE INTO watch_channels(watch_id, channel_id, enabled)
             SELECT ?, channel_id, enabled FROM watch_channels WHERE watch_id = ?",
        )
        .bind(to)
        .bind(from)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM watches WHERE id = ?")
            .bind(from)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn delete_watch(&self, id: &str) -> Result<bool> {
        let r = sqlx::query("DELETE FROM watches WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn set_watch_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        sqlx::query("UPDATE watches SET enabled = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(if enabled { 1 } else { 0 })
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn touch_watch_trigger(&self, id: &str) -> Result<()> {
        sqlx::query("UPDATE watches SET updated_at = datetime('now') WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn upsert_watch_state(
        &self,
        watch_id: &str,
        cinema_id: &str,
        state: &Value,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO watch_state(watch_id, cinema_id, state_json, updated_at)
             VALUES (?, ?, ?, datetime('now'))
             ON CONFLICT(watch_id, cinema_id) DO UPDATE SET
                state_json = excluded.state_json, updated_at = datetime('now')",
        )
        .bind(watch_id)
        .bind(cinema_id)
        .bind(state.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn watch_states(&self, watch_id: &str) -> Result<Vec<(String, String)>> {
        Ok(sqlx::query_as(
            "SELECT cinema_id, state_json FROM watch_state WHERE watch_id = ? ORDER BY cinema_id",
        )
        .bind(watch_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn save_watch_states(
        &self,
        watch_id: &str,
        states: &[(String, Value)],
    ) -> Result<()> {
        if states.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for (cid, state) in states {
            sqlx::query(
                "INSERT INTO watch_state(watch_id, cinema_id, state_json, updated_at)
                 VALUES (?, ?, ?, datetime('now'))
                 ON CONFLICT(watch_id, cinema_id) DO UPDATE SET
                    state_json = excluded.state_json, updated_at = datetime('now')",
            )
            .bind(watch_id)
            .bind(cid)
            .bind(state.to_string())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    // ------------------------- auto-lock state -------------------------

    pub async fn lock_state(
        &self,
        watch_id: &str,
        cinema_id: &str,
    ) -> Result<Option<LockStateRow>> {
        Ok(sqlx::query_as::<_, LockStateRow>(
            "SELECT watch_id, cinema_id, status, tries, last_result, last_error,
                    last_event_id, updated_at
             FROM lock_state WHERE watch_id = ? AND cinema_id = ?",
        )
        .bind(watch_id)
        .bind(cinema_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn lock_states_for_watch(&self, watch_id: &str) -> Result<Vec<LockStateRow>> {
        Ok(sqlx::query_as::<_, LockStateRow>(
            "SELECT watch_id, cinema_id, status, tries, last_result, last_error,
                    last_event_id, updated_at
             FROM lock_state WHERE watch_id = ? ORDER BY cinema_id",
        )
        .bind(watch_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn mark_lock_ok(
        &self,
        watch_id: &str,
        cinema_id: &str,
        event_id: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO lock_state(watch_id, cinema_id, status, tries, last_result,
                                    last_event_id, updated_at)
             VALUES (?, ?, 'ok', 0, 'Ok', ?, datetime('now'))
             ON CONFLICT(watch_id, cinema_id) DO UPDATE SET
                status = 'ok',
                last_result = 'Ok',
                last_error = NULL,
                last_event_id = excluded.last_event_id,
                updated_at = datetime('now')",
        )
        .bind(watch_id)
        .bind(cinema_id)
        .bind(event_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_lock_failure(
        &self,
        watch_id: &str,
        cinema_id: &str,
        result: &str,
        error: Option<&str>,
        event_id: Option<&str>,
        max_retries: i64,
    ) -> Result<LockStateRow> {
        sqlx::query(
            "INSERT INTO lock_state(watch_id, cinema_id, status, tries, last_result,
                                    last_error, last_event_id, updated_at)
             VALUES (?, ?, 'failed', 1, ?, ?, ?, datetime('now'))
             ON CONFLICT(watch_id, cinema_id) DO UPDATE SET
                tries = lock_state.tries + 1,
                status = CASE WHEN lock_state.tries + 1 >= ? THEN 'exhausted' ELSE 'failed' END,
                last_result = excluded.last_result,
                last_error = excluded.last_error,
                last_event_id = excluded.last_event_id,
                updated_at = datetime('now')",
        )
        .bind(watch_id)
        .bind(cinema_id)
        .bind(result)
        .bind(error)
        .bind(event_id)
        .bind(max_retries.max(1))
        .execute(&self.pool)
        .await?;
        self.lock_state(watch_id, cinema_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("lock_state 写入后读取失败"))
    }

    // ------------------------- movies -------------------------

    pub async fn list_movies(&self, q: &str, limit: i64) -> Result<Vec<MovieRow>> {
        let q = q.trim();
        Ok(sqlx::query_as::<_, MovieRow>(
            "SELECT id, name, show_type, updated_at FROM movies
             WHERE ? = '' OR name LIKE '%' || ? || '%' OR CAST(id AS TEXT) LIKE '%' || ? || '%'
             ORDER BY show_type ASC, name ASC LIMIT ?",
        )
        .bind(q)
        .bind(q)
        .bind(q)
        .bind(limit.clamp(1, 2000))
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn upsert_movies(&self, rows: &[(String, String)], show_type: u8) -> Result<usize> {
        let mut tx = self.pool.begin().await?;
        let mut n = 0usize;
        for (id, name) in rows {
            let Ok(id) = id.parse::<i64>() else {
                continue;
            };
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            sqlx::query(
                "INSERT INTO movies(id, name, show_type, updated_at)
                 VALUES (?, ?, ?, datetime('now'))
                 ON CONFLICT(id) DO UPDATE SET
                    name = excluded.name,
                    show_type = excluded.show_type,
                    updated_at = datetime('now')",
            )
            .bind(id)
            .bind(name)
            .bind(show_type as i64)
            .execute(&mut *tx)
            .await?;
            n += 1;
        }
        tx.commit().await?;
        Ok(n)
    }

    pub async fn search_cinemas(&self, q: &str, limit: i64) -> Result<Vec<(String, String)>> {
        let q = q.trim();
        Ok(sqlx::query_as(
            "SELECT id, name FROM cinemas
             WHERE ? = '' OR name LIKE '%' || ? || '%' OR id LIKE '%' || ? || '%'
             ORDER BY favorite DESC, name ASC LIMIT ?",
        )
        .bind(q)
        .bind(q)
        .bind(q)
        .bind(limit.clamp(1, 200))
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn upsert_search_movies(&self, rows: &[(String, String)]) -> Result<usize> {
        let mut tx = self.pool.begin().await?;
        let mut n = 0usize;
        for (id, name) in rows {
            let Ok(id) = id.parse::<i64>() else { continue };
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            sqlx::query(
                "INSERT INTO movies(id, name, show_type, updated_at)
                 VALUES (?, ?, 4, datetime('now'))
                 ON CONFLICT(id) DO UPDATE SET
                    name = excluded.name, updated_at = datetime('now')",
            )
            .bind(id)
            .bind(name)
            .execute(&mut *tx)
            .await?;
            n += 1;
        }
        tx.commit().await?;
        Ok(n)
    }

    pub async fn movie_name(&self, id: i64) -> Result<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT name FROM movies WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|(n,)| n))
    }

    pub async fn movie_count(&self) -> Result<i64> {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM movies")
            .fetch_one(&self.pool)
            .await?;
        Ok(n)
    }

    // ------------------------- monitor load -------------------------

    pub async fn load_monitor_watches(&self) -> Result<Vec<MonitorWatch>> {
        let rows = sqlx::query_as::<_, WatchRow>(
            "SELECT w.*, 0 AS subscriber_count FROM watches w WHERE w.enabled = 1 ORDER BY w.id",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let cinemas = self.watch_cinemas(&row.id).await?;
            let state_rows: Vec<(String, String)> =
                sqlx::query_as("SELECT cinema_id, state_json FROM watch_state WHERE watch_id = ?")
                    .bind(&row.id)
                    .fetch_all(&self.pool)
                    .await?;
            let mut state = HashMap::new();
            for (cid, s) in state_rows {
                if let Ok(v) = serde_json::from_str::<Value>(&s) {
                    state.insert(cid, v);
                }
            }
            out.push(MonitorWatch {
                row,
                cinemas,
                state,
            });
        }
        Ok(out)
    }

    // ------------------------- poll health -------------------------

    pub async fn insert_poll_health(
        &self,
        cinemas_ok: usize,
        cinemas_err: usize,
        blocked_cinemas: usize,
        duration_ms: u128,
        error_summary: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO poll_health(cinemas_total, cinemas_ok, cinemas_err,
                                     blocked_cinemas, duration_ms, error_summary)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind((cinemas_ok + cinemas_err) as i64)
        .bind(cinemas_ok as i64)
        .bind(cinemas_err as i64)
        .bind(blocked_cinemas as i64)
        .bind(duration_ms as i64)
        .bind(error_summary)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn poll_health_since(&self, hours: u32) -> Result<Vec<PollHealthRow>> {
        let modifier = format!("-{} hours", hours.max(1));
        Ok(sqlx::query_as::<_, PollHealthRow>(
            "SELECT id, recorded_at,
                    CAST(strftime('%s', recorded_at) AS INTEGER) AS recorded_ts,
                    cinemas_total, cinemas_ok, cinemas_err,
                    blocked_cinemas, duration_ms, error_summary
             FROM poll_health
             WHERE recorded_at >= datetime('now', ?)
             ORDER BY recorded_at ASC",
        )
        .bind(modifier)
        .fetch_all(&self.pool)
        .await?)
    }

    /// 自然日窗口 [start_ts, end_ts)；用于面板的 00:00–24:00 健康图。
    pub async fn poll_health_between(
        &self,
        start_ts: i64,
        end_ts: i64,
    ) -> Result<Vec<PollHealthRow>> {
        Ok(sqlx::query_as::<_, PollHealthRow>(
            "SELECT id, recorded_at,
                    CAST(strftime('%s', recorded_at) AS INTEGER) AS recorded_ts,
                    cinemas_total, cinemas_ok, cinemas_err,
                    blocked_cinemas, duration_ms, error_summary
             FROM poll_health
             WHERE recorded_at >= datetime(?, 'unixepoch')
               AND recorded_at < datetime(?, 'unixepoch')
             ORDER BY recorded_at ASC",
        )
        .bind(start_ts)
        .bind(end_ts)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn poll_health_recent(&self, limit: i64) -> Result<Vec<PollHealthRow>> {
        Ok(sqlx::query_as::<_, PollHealthRow>(
            "SELECT id, recorded_at,
                    CAST(strftime('%s', recorded_at) AS INTEGER) AS recorded_ts,
                    cinemas_total, cinemas_ok, cinemas_err,
                    blocked_cinemas, duration_ms, error_summary
             FROM poll_health
             ORDER BY recorded_at DESC, id DESC LIMIT ?",
        )
        .bind(limit.clamp(1, 200))
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn prune_poll_health(&self, days: u32) -> Result<u64> {
        let modifier = format!("-{} days", days.max(1));
        let r = sqlx::query("DELETE FROM poll_health WHERE recorded_at < datetime('now', ?)")
            .bind(modifier)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    // ------------------------- metrics -------------------------

    pub async fn insert_metric(
        &self,
        watch_id: &str,
        total_shows: usize,
        imax_shows: usize,
        bucket_count: usize,
        added_shows: usize,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO watch_metrics(watch_id, recorded_at, total_shows, imax_shows,
                                       bucket_count, added_shows)
             VALUES (?, datetime('now'), ?, ?, ?, ?)",
        )
        .bind(watch_id)
        .bind(total_shows as i64)
        .bind(imax_shows as i64)
        .bind(bucket_count as i64)
        .bind(added_shows as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn metrics(&self, watch_id: &str, since_modifier: &str) -> Result<Vec<MetricRow>> {
        let sql = "SELECT recorded_at, total_shows, imax_shows, bucket_count, added_shows
             FROM watch_metrics
             WHERE watch_id = ? AND recorded_at >= datetime('now', ?)
             ORDER BY recorded_at ASC";
        Ok(sqlx::query_as::<_, MetricRow>(sql)
            .bind(watch_id)
            .bind(since_modifier)
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn prune_metrics(&self, days: u32) -> Result<u64> {
        let modifier = format!("-{} days", days.max(1));
        let r = sqlx::query("DELETE FROM watch_metrics WHERE recorded_at < datetime('now', ?)")
            .bind(modifier)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    // ------------------------- channels -------------------------

    pub async fn list_channels(&self) -> Result<Vec<ChannelRow>> {
        Ok(sqlx::query_as::<_, ChannelRow>(
            "SELECT id, name, type, config_json, enabled, created_at
             FROM channels ORDER BY created_at ASC, id ASC",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn get_channel(&self, id: &str) -> Result<Option<ChannelRow>> {
        Ok(sqlx::query_as::<_, ChannelRow>(
            "SELECT id, name, type, config_json, enabled, created_at FROM channels WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn create_channel(
        &self,
        name: &str,
        channel_type: &str,
        config: &Value,
    ) -> Result<String> {
        let id = format!("ch_{}", &Uuid::new_v4().to_string()[..8]);
        self.create_channel_with_id(&id, name, channel_type, config, true)
            .await?;
        Ok(id)
    }

    pub async fn create_channel_with_id(
        &self,
        id: &str,
        name: &str,
        channel_type: &str,
        config: &Value,
        enabled: bool,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO channels(id, name, type, config_json, enabled)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(name)
        .bind(channel_type)
        .bind(config.to_string())
        .bind(if enabled { 1 } else { 0 })
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_channel(
        &self,
        id: &str,
        name: Option<&str>,
        config: Option<&Value>,
        enabled: Option<bool>,
    ) -> Result<()> {
        if let Some(name) = name {
            sqlx::query("UPDATE channels SET name = ? WHERE id = ?")
                .bind(name)
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        if let Some(config) = config {
            sqlx::query("UPDATE channels SET config_json = ? WHERE id = ?")
                .bind(config.to_string())
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        if let Some(enabled) = enabled {
            sqlx::query("UPDATE channels SET enabled = ? WHERE id = ?")
                .bind(if enabled { 1 } else { 0 })
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    pub async fn delete_channel(&self, id: &str) -> Result<bool> {
        let r = sqlx::query("DELETE FROM channels WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn watch_channels(&self, watch_id: &str) -> Result<Vec<ChannelRow>> {
        Ok(sqlx::query_as::<_, ChannelRow>(
            "SELECT c.id, c.name, c.type, c.config_json, c.enabled, c.created_at
             FROM channels c
             JOIN watch_channels wc ON wc.channel_id = c.id
             WHERE wc.watch_id = ? AND wc.enabled = 1 AND c.enabled = 1
             ORDER BY c.created_at ASC, c.id ASC",
        )
        .bind(watch_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn set_watch_channel(
        &self,
        watch_id: &str,
        channel_id: &str,
        enabled: bool,
    ) -> Result<()> {
        if enabled {
            sqlx::query(
                "INSERT INTO watch_channels(watch_id, channel_id, enabled) VALUES (?, ?, 1)
                 ON CONFLICT(watch_id, channel_id) DO UPDATE SET enabled = 1",
            )
            .bind(watch_id)
            .bind(channel_id)
            .execute(&self.pool)
            .await?;
        } else {
            // 保留 config_json，重新绑定时不用重填群名/URL/agent 地址。
            sqlx::query(
                "UPDATE watch_channels SET enabled = 0
                 WHERE watch_id = ? AND channel_id = ?",
            )
            .bind(watch_id)
            .bind(channel_id)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    /// 保存绑定并写入该 watch 自己的渠道参数（群名 / agent 地址 / 锁票参数等）。
    pub async fn bind_watch_channel(
        &self,
        watch_id: &str,
        channel_id: &str,
        config: &Value,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO watch_channels(watch_id, channel_id, enabled, config_json)
             VALUES (?, ?, 1, ?)
             ON CONFLICT(watch_id, channel_id)
             DO UPDATE SET enabled = 1, config_json = excluded.config_json",
        )
        .bind(watch_id)
        .bind(channel_id)
        .bind(config.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn watch_channel_config(&self, watch_id: &str, channel_id: &str) -> Result<Value> {
        let raw: Option<String> = sqlx::query_scalar(
            "SELECT config_json FROM watch_channels
             WHERE watch_id = ? AND channel_id = ? AND enabled = 1",
        )
        .bind(watch_id)
        .bind(channel_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(raw
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or_else(|| json!({})))
    }

    /// 所有 watch × channel 的参数，包含已解绑行（用于重新绑定时预填）。
    /// fan-out 仍只遍历 `watch_channels()` 返回的启用渠道。
    pub async fn watch_channel_bindings(&self, watch_id: &str) -> Result<HashMap<String, Value>> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT channel_id, config_json FROM watch_channels WHERE watch_id = ?")
                .bind(watch_id)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, raw)| serde_json::from_str::<Value>(&raw).ok().map(|v| (id, v)))
            .collect())
    }

    // ------------------------- users / subscriptions -------------------------

    pub async fn list_users(&self, limit: i64, offset: i64) -> Result<Vec<UserRow>> {
        Ok(sqlx::query_as::<_, UserRow>(
            "SELECT u.*, (SELECT COUNT(*) FROM subscriptions s WHERE s.user_id = u.id) AS subscription_count
             FROM users u ORDER BY u.created_at DESC LIMIT ? OFFSET ?",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn user_count(&self) -> Result<i64> {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
            .fetch_one(&self.pool)
            .await?;
        Ok(n)
    }

    pub async fn subscription_count(&self) -> Result<i64> {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM subscriptions WHERE enabled = 1")
            .fetch_one(&self.pool)
            .await?;
        Ok(n)
    }

    pub async fn create_user(&self, openid: Option<&str>, nickname: &str) -> Result<String> {
        let id = format!("u_{}", &Uuid::new_v4().to_string()[..8]);
        sqlx::query("INSERT INTO users(id, openid, nickname) VALUES (?, ?, ?)")
            .bind(&id)
            .bind(openid)
            .bind(nickname)
            .execute(&self.pool)
            .await?;
        Ok(id)
    }

    pub async fn delete_user(&self, id: &str) -> Result<bool> {
        let r = sqlx::query("DELETE FROM users WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn list_subscriptions(&self, limit: i64) -> Result<Vec<SubscriptionRow>> {
        Ok(sqlx::query_as::<_, SubscriptionRow>(
            "SELECT s.id, s.user_id, s.watch_id, s.channel_id, s.enabled, s.created_at,
                    u.nickname AS user_name,
                    COALESCE(w.movie_name, w.id) AS watch_name,
                    c.name AS channel_name
             FROM subscriptions s
             JOIN users u ON u.id = s.user_id
             JOIN watches w ON w.id = s.watch_id
             LEFT JOIN channels c ON c.id = s.channel_id
             ORDER BY s.created_at DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn subscriptions_for_watch(&self, watch_id: &str) -> Result<Vec<SubscriptionRow>> {
        Ok(sqlx::query_as::<_, SubscriptionRow>(
            "SELECT s.id, s.user_id, s.watch_id, s.channel_id, s.enabled, s.created_at,
                    u.nickname AS user_name,
                    COALESCE(w.movie_name, w.id) AS watch_name,
                    c.name AS channel_name
             FROM subscriptions s
             JOIN users u ON u.id = s.user_id
             JOIN watches w ON w.id = s.watch_id
             LEFT JOIN channels c ON c.id = s.channel_id
             WHERE s.watch_id = ? ORDER BY s.created_at DESC",
        )
        .bind(watch_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn subscriptions_for_user(&self, user_id: &str) -> Result<Vec<SubscriptionRow>> {
        Ok(sqlx::query_as::<_, SubscriptionRow>(
            "SELECT s.id, s.user_id, s.watch_id, s.channel_id, s.enabled, s.created_at,
                    u.nickname AS user_name,
                    COALESCE(w.movie_name, w.id) AS watch_name,
                    c.name AS channel_name
             FROM subscriptions s
             JOIN users u ON u.id = s.user_id
             JOIN watches w ON w.id = s.watch_id
             LEFT JOIN channels c ON c.id = s.channel_id
             WHERE s.user_id = ? ORDER BY s.created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?)
    }

    /// 用户订阅 watch；channel_id=None 表示小程序默认推送。
    pub async fn subscribe(
        &self,
        user_id: &str,
        watch_id: &str,
        channel_id: Option<&str>,
    ) -> Result<String> {
        let id = format!("s_{}", &Uuid::new_v4().to_string()[..8]);
        sqlx::query(
            "INSERT INTO subscriptions(id, user_id, watch_id, channel_id, enabled)
             VALUES (?, ?, ?, ?, 1)
             ON CONFLICT DO NOTHING",
        )
        .bind(&id)
        .bind(user_id)
        .bind(watch_id)
        .bind(channel_id)
        .execute(&self.pool)
        .await?;
        let (existing,): (String,) = sqlx::query_as(
            "SELECT id FROM subscriptions
             WHERE user_id = ? AND watch_id = ? AND COALESCE(channel_id,'') = COALESCE(?,'')",
        )
        .bind(user_id)
        .bind(watch_id)
        .bind(channel_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(existing)
    }

    pub async fn unsubscribe(&self, id: &str) -> Result<bool> {
        let r = sqlx::query("DELETE FROM subscriptions WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    /// 小程序推送 fan-out 的目标：该 watch 的所有 enabled 订阅用户。
    pub async fn push_targets_for_watch(&self, watch_id: &str) -> Result<Vec<PushTarget>> {
        Ok(sqlx::query_as::<_, PushTarget>(
            "SELECT u.id AS user_id, u.openid, u.nickname,
                    s.channel_id, c.type AS channel_type
             FROM subscriptions s
             JOIN users u ON u.id = s.user_id
             LEFT JOIN channels c ON c.id = s.channel_id
             WHERE s.watch_id = ? AND s.enabled = 1 AND u.status = 'active'
               AND (s.channel_id IS NULL OR c.type = 'wechat_miniprogram')
             ORDER BY s.created_at ASC",
        )
        .bind(watch_id)
        .fetch_all(&self.pool)
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time_spec::TimeSpec;

    fn spec(movie_id: i64, cinemas: &[&str], time: &str) -> NewWatch {
        NewWatch {
            movie_id,
            movie_name: Some("测试电影".into()),
            cinemas: cinemas.iter().map(|s| s.to_string()).collect(),
            time_spec: TimeSpec::parse(time).unwrap().to_json(),
            imax_only: false,
            created_by: None,
            visibility: "public".into(),
            auto_lock: false,
            lock_num_seats: DEFAULT_LOCK_NUM_SEATS,
            lock_max_retries: DEFAULT_LOCK_MAX_RETRIES,
            lock_seats: Vec::new(),
        }
    }

    #[tokio::test]
    async fn watch_filter_key_reuses_existing_row() {
        let db = Db::connect_memory().await.unwrap();
        let (a, created_a) = db
            .create_or_reuse_watch(&spec(1545360, &["37534"], "2026-11-12 19:00-22:00"))
            .await
            .unwrap();
        let (b, created_b) = db
            .create_or_reuse_watch(&spec(1545360, &["37534"], "2026-11-12 19:00-22:00"))
            .await
            .unwrap();
        assert!(created_a);
        assert!(!created_b);
        assert_eq!(a, b);
        assert_eq!(db.watch_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn subscriptions_are_idempotent_and_counted() {
        let db = Db::connect_memory().await.unwrap();
        let (wid, _) = db
            .create_or_reuse_watch(&spec(1, &["2"], "all"))
            .await
            .unwrap();
        let uid = db.create_user(Some("o_test"), "测试用户").await.unwrap();
        let s1 = db.subscribe(&uid, &wid, None).await.unwrap();
        let s2 = db.subscribe(&uid, &wid, None).await.unwrap();
        assert_eq!(s1, s2);
        assert_eq!(db.subscription_count().await.unwrap(), 1);
        let rows = db.list_watches("subscribers", 10, 0).await.unwrap();
        assert_eq!(rows[0].subscriber_count, 1);
        assert_eq!(db.push_targets_for_watch(&wid).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn metrics_roundtrip_and_prune() {
        let db = Db::connect_memory().await.unwrap();
        let (wid, _) = db
            .create_or_reuse_watch(&spec(1, &["2"], "all"))
            .await
            .unwrap();
        db.insert_metric(&wid, 12, 3, 4, 1).await.unwrap();
        let rows = db.metrics(&wid, "-24 hours").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].total_shows, 12);
        assert_eq!(rows[0].imax_shows, 3);
        let pruned = db.prune_metrics(7).await.unwrap();
        assert_eq!(pruned, 0);
    }

    #[tokio::test]
    async fn poll_health_between_uses_epoch_window() {
        let db = Db::connect_memory().await.unwrap();
        db.insert_poll_health(3, 0, 0, 120, None).await.unwrap();
        let now = chrono::Utc::now().timestamp();
        let rows = db.poll_health_between(now - 60, now + 60).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cinemas_total, 3);
        assert!(db
            .poll_health_between(now + 120, now + 180)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn channels_bind_unbind() {
        let db = Db::connect_memory().await.unwrap();
        let (wid, _) = db
            .create_or_reuse_watch(&spec(1, &["2"], "all"))
            .await
            .unwrap();
        db.set_watch_channel(&wid, "discord-default", true)
            .await
            .unwrap();
        assert_eq!(db.watch_channels(&wid).await.unwrap().len(), 1);
        db.set_watch_channel(&wid, "discord-default", false)
            .await
            .unwrap();
        assert_eq!(db.watch_channels(&wid).await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn watch_channel_config_is_per_watch_and_survives_unbind() {
        let db = Db::connect_memory().await.unwrap();
        let (w1, _) = db
            .create_or_reuse_watch(&spec(1, &["2"], "all"))
            .await
            .unwrap();
        let (w2, _) = db
            .create_or_reuse_watch(&spec(2, &["2"], "all"))
            .await
            .unwrap();
        db.bind_watch_channel(&w1, "xhs-local", &json!({"group": "group-a"}))
            .await
            .unwrap();
        db.bind_watch_channel(&w2, "xhs-local", &json!({"group": "group-b"}))
            .await
            .unwrap();
        assert_eq!(
            db.watch_channel_config(&w1, "xhs-local").await.unwrap()["group"],
            "group-a"
        );
        assert_eq!(
            db.watch_channel_config(&w2, "xhs-local").await.unwrap()["group"],
            "group-b"
        );

        db.set_watch_channel(&w1, "xhs-local", false).await.unwrap();
        assert_eq!(
            db.watch_channel_config(&w1, "xhs-local").await.unwrap(),
            json!({})
        );
        assert_eq!(
            db.watch_channel_bindings(&w1).await.unwrap()["xhs-local"]["group"],
            "group-a"
        );
        db.set_watch_channel(&w1, "xhs-local", true).await.unwrap();
        assert_eq!(
            db.watch_channel_config(&w1, "xhs-local").await.unwrap()["group"],
            "group-a"
        );
    }

    #[tokio::test]
    async fn autolock_channel_exists_and_binds_for_auto_lock_watch() {
        let db = Db::connect_memory().await.unwrap();
        assert!(db
            .list_channels()
            .await
            .unwrap()
            .iter()
            .any(|c| c.id == "autolock-local" && c.channel_type == "autolock"));

        let mut s = spec(3, &["2"], "all");
        s.auto_lock = true;
        let (wid, _) = db.create_or_reuse_watch(&s).await.unwrap();
        assert!(db
            .watch_channels(&wid)
            .await
            .unwrap()
            .iter()
            .any(|c| c.id == "autolock-local"));
    }
}

#[cfg(test)]
mod lock_tests {
    use super::*;
    use crate::time_spec::TimeSpec;

    async fn watch(db: &Db) -> String {
        db.create_or_reuse_watch(&NewWatch {
            movie_id: 1,
            movie_name: Some("锁票测试".into()),
            cinemas: vec!["2".into()],
            time_spec: TimeSpec::parse("all").unwrap().to_json(),
            imax_only: false,
            created_by: None,
            visibility: "public".into(),
            auto_lock: true,
            lock_num_seats: 2,
            lock_max_retries: 2,
            lock_seats: vec![],
        })
        .await
        .unwrap()
        .0
    }

    #[tokio::test]
    async fn lock_failures_exhaust_and_success_wins() {
        let db = Db::connect_memory().await.unwrap();
        let wid = watch(&db).await;
        let r1 = db
            .record_lock_failure(&wid, "2", "ShowGone", Some("gone"), Some("e1"), 2)
            .await
            .unwrap();
        assert_eq!(r1.status, "failed");
        assert_eq!(r1.tries, 1);
        let dup = db
            .record_lock_failure(&wid, "2", "ShowGone", Some("gone"), Some("e1"), 2)
            .await
            .unwrap();
        assert_eq!(dup.tries, 2);
        assert_eq!(dup.status, "exhausted");
        db.mark_lock_ok(&wid, "2", Some("e2")).await.unwrap();
        let ok = db.lock_state(&wid, "2").await.unwrap().unwrap();
        assert_eq!(ok.status, "ok");
        assert_eq!(ok.last_event_id.as_deref(), Some("e2"));
    }

    #[tokio::test]
    async fn filter_change_clears_lock_state() {
        let db = Db::connect_memory().await.unwrap();
        let wid = watch(&db).await;
        db.mark_lock_ok(&wid, "2", None).await.unwrap();
        let patch = WatchPatch {
            time_spec: Some(TimeSpec::parse("2027-01-01").unwrap().to_json()),
            ..Default::default()
        };
        db.update_watch(&wid, &patch).await.unwrap();
        assert!(db.lock_state(&wid, "2").await.unwrap().is_none());
    }
}

#[cfg(test)]
mod movie_tests {
    use super::*;

    #[tokio::test]
    async fn movies_upsert_and_search() {
        let db = Db::connect_memory().await.unwrap();
        let n = db
            .upsert_movies(
                &[
                    ("101".to_string(), "奥德赛".to_string()),
                    ("102".to_string(), "Dune Part Three".to_string()),
                ],
                1,
            )
            .await
            .unwrap();
        assert_eq!(n, 2);
        assert_eq!(db.movie_count().await.unwrap(), 2);
        assert_eq!(db.list_movies("奥德", 10).await.unwrap().len(), 1);
        assert_eq!(db.list_movies("dune", 10).await.unwrap().len(), 1);
        assert_eq!(
            db.list_movies("102", 10).await.unwrap()[0].name,
            "Dune Part Three"
        );
        assert_eq!(db.movie_name(101).await.unwrap().as_deref(), Some("奥德赛"));
    }
}
