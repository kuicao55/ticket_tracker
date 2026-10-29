//! v4 无头主服务：DB-backed monitor + API + Web 面板 + 薄通知分发。

pub mod api;
pub mod dispatcher;
pub mod monitor;
pub mod web;

use std::collections::{HashMap, VecDeque};
use std::fs::OpenOptions;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use fs2::FileExt;
use serde::Serialize;

use sqlx::SqlitePool;

use crate::db::Db;
use crate::service;

#[derive(Debug, Clone, Default, Serialize)]
pub struct WatchStatus {
    pub status: String,
    pub last_checked_at: Option<f64>,
    pub last_trigger_at: Option<f64>,
    pub total_shows: usize,
    pub imax_shows: usize,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventItem {
    pub id: u64,
    pub ts: String,
    pub watch_id: Option<String>,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct HealthState {
    pub last_poll_at: Option<f64>,
    pub next_poll_at: Option<f64>,
    pub fetch_ok: usize,
    pub fetch_err: usize,
    pub silent: bool,
    pub last_error: Option<String>,
    pub consecutive_failures: u32,
    pub check_count: u64,
    pub health_ok: bool,
    pub paused: bool,
}

pub struct AppState {
    pub db: Db,
    pub started_at: f64,
    /// 实际监听地址（`TT_BIND`），用于生成 iGlance 的 `base_url`。
    pub bind: String,
    /// 可选对外地址（`TT_PUBLIC_URL`），用于 iGlance 的 display / panel。
    pub public_url: Option<String>,
    pub health: StdMutex<HealthState>,
    pub watch_status: StdMutex<HashMap<String, WatchStatus>>,
    pub events: StdMutex<VecDeque<EventItem>>,
    pub event_seq: std::sync::atomic::AtomicU64,
    pub http: reqwest::Client,
    /// 配置/watch 变更时唤醒 monitor，立即重读 settings 并重算下一轮。
    pub wake: tokio::sync::Notify,
    /// 正在等待 Gofer 回调的 lock key（watch_id:cinema_id），避免重复提交。
    pub lock_inflight: StdMutex<HashMap<String, f64>>,
    /// `/api/v1/service/stop` 已受理；避免重复触发退出。
    pub stopping: AtomicBool,
    /// 面板登录状态；`None` 表示看守关闭（库/测试用，正式服务始终启用）。
    pub auth: Option<crate::auth::AuthState>,
}

impl AppState {
    /// 库/测试用：不启用面板登录。
    pub fn new(db: Db) -> Arc<Self> {
        Self::with_bind(db, default_bind())
    }

    pub fn with_bind(db: Db, bind: impl Into<String>) -> Arc<Self> {
        Self::build(db, bind, None)
    }

    /// 正式服务 / 登录测试用。
    pub fn with_bind_auth(
        db: Db,
        bind: impl Into<String>,
        auth: crate::auth::AuthState,
    ) -> Arc<Self> {
        Self::build(db, bind, Some(auth))
    }

    pub fn new_with_auth(db: Db, auth: crate::auth::AuthState) -> Arc<Self> {
        Self::build(db, default_bind(), Some(auth))
    }

    fn build(db: Db, bind: impl Into<String>, auth: Option<crate::auth::AuthState>) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .connect_timeout(std::time::Duration::from_secs(3))
            .build()
            .expect("reqwest client");
        Arc::new(Self {
            db,
            started_at: now_secs(),
            bind: bind.into(),
            public_url: service::env_public_url(),
            health: StdMutex::new(HealthState {
                health_ok: true,
                ..Default::default()
            }),
            watch_status: StdMutex::new(HashMap::new()),
            events: StdMutex::new(VecDeque::new()),
            event_seq: std::sync::atomic::AtomicU64::new(1),
            http,
            wake: tokio::sync::Notify::new(),
            lock_inflight: StdMutex::new(HashMap::new()),
            stopping: AtomicBool::new(false),
            auth,
        })
    }

    /// iGlance 可访问的 base URL。
    pub fn base_url(&self) -> String {
        service::base_url_from_bind(&self.bind)
    }

    /// 面板/展示地址：优先 `TT_PUBLIC_URL`，否则本地 base URL。
    pub fn display_base_url(&self) -> String {
        service::display_base_url(&self.base_url(), self.public_url.as_deref())
    }

    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    pub fn push_event(&self, kind: &str, watch_id: Option<&str>, message: impl Into<String>) {
        let id = self
            .event_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let item = EventItem {
            id,
            ts: chrono::Local::now().format("%H:%M:%S").to_string(),
            watch_id: watch_id.map(String::from),
            kind: kind.to_string(),
            message: message.into(),
        };
        let mut q = self.events.lock().unwrap();
        q.push_front(item);
        while q.len() > 200 {
            q.pop_back();
        }
    }

    pub fn events_snapshot(&self, limit: usize) -> Vec<EventItem> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn health_snapshot(&self) -> HealthState {
        self.health.lock().unwrap().clone()
    }

    pub fn watch_status_snapshot(&self) -> HashMap<String, WatchStatus> {
        self.watch_status.lock().unwrap().clone()
    }

    pub fn set_watch_status(&self, id: &str, status: WatchStatus) {
        self.watch_status
            .lock()
            .unwrap()
            .insert(id.to_string(), status);
    }
}

fn acquire_instance_lock() -> Result<std::fs::File> {
    let path = crate::paths::state_dir().join("server.lock");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("打开单实例锁失败: {}", path.display()))?;
    file.try_lock_exclusive()
        .map_err(|_| anyhow!("另一个 tt-server 正在运行（{} 已被占用）", path.display()))?;
    Ok(file)
}

pub fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn default_bind() -> String {
    std::env::var("TT_BIND")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "127.0.0.1:8787".to_string())
}

#[derive(Subcommand, Debug)]
pub enum ServerCommand {
    /// 只打印 iGlance manifest，不启动服务
    Manifest {
        /// 服务 HTTP 根地址；省略时用 --bind / TT_BIND
        #[arg(long)]
        base_url: Option<String>,
    },
}

#[derive(Parser, Debug)]
#[command(name = "tt-server", version, about = "ticket-tracker 无头主服务")]
pub struct ServerArgs {
    /// HTTP 监听地址（默认只绑 localhost）
    #[arg(long, env = "TT_BIND", default_value = "127.0.0.1:8787")]
    pub bind: String,
    /// SQLite 文件路径
    #[arg(long, env = "TT_DB", default_value = "")]
    pub db: String,
    /// 只启动 API/面板，不跑监控（测试用）
    #[arg(long, env = "TT_NO_MONITOR", default_value_t = false)]
    pub no_monitor: bool,
    #[command(subcommand)]
    pub command: Option<ServerCommand>,
}

pub fn default_db_path() -> String {
    crate::paths::state_dir()
        .join("ticket-tracker.db")
        .display()
        .to_string()
}

pub async fn run() -> Result<()> {
    let args = ServerArgs::parse();

    // iGlance 兜底命令：不启动服务、不初始化数据库，只输出一行 manifest。
    if let Some(ServerCommand::Manifest { base_url }) = args.command {
        let base = base_url
            .map(|v| v.trim().trim_end_matches('/').to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| service::base_url_from_bind(&args.bind));
        println!("{}", service::manifest_line(&base));
        return Ok(());
    }

    init_tracing();
    // Release 二进制自带 Web 资产；显式 TT_WEB_DIR 时保留外部目录。
    crate::assets::materialize().context("释放内置 Web 资源失败")?;
    let db_path = if args.db.trim().is_empty() {
        default_db_path()
    } else {
        args.db.clone()
    };
    let db = Db::connect(&db_path)
        .await
        .with_context(|| format!("初始化数据库失败: {}", db_path))?;
    tracing::info!("SQLite: {}", db_path);

    // 单实例锁，避免两个 tt-server 同时轮询。
    let _instance_lock = acquire_instance_lock()?;

    let auth = crate::auth::AuthState::load(&db)
        .await
        .context("初始化面板登录失败")?;
    let state = AppState::with_bind_auth(db.clone(), args.bind.clone(), auth);
    // 首次启动时后台预热电影列表，让 Web 弹窗打开就有可选电影。
    spawn_movie_warmup(db.clone());
    if !args.no_monitor {
        monitor::spawn(state.clone());
    }

    let app = api::router(state.clone()).merge(web::router(state.clone()));
    let listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("监听失败: {}", args.bind))?;
    tracing::info!("http://{}  (API + Web 面板)", args.bind);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn spawn_movie_warmup(db: Db) {
    tokio::spawn(async move {
        if db.movie_count().await.unwrap_or(0) > 0 {
            return;
        }
        for (show_type, _) in [(1u8, "热映"), (2u8, "即将")] {
            if let Ok(rows) = crate::maoyan::fetch_films_list_async(show_type).await {
                let _ = db.upsert_movies(&rows, show_type).await;
            }
        }
    });
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        let mut sig = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM");
        sig.recv().await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

/// 给测试/工具用：从一个已有 pool 构造 state。
pub fn state_from_pool(pool: SqlitePool) -> Arc<AppState> {
    AppState::new(Db { pool })
}
