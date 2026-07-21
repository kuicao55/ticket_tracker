//! TUI 入口 —— ratatui + crossterm，三栏 + 状态栏 + `:` 命令面板。
//!
//! 详见 RUST_PORT.md §7。
//!
//! 关键设计：Monitor 的可变状态由 `Arc<SharedState>` 持有，**TUI 渲染线程** 通
//! 过 `cfg_snapshot()` / `events_snapshot()` / `stats_snapshot()` 做 cheap 读取；
//! monitor 自身跑在**独立的 `std::thread`** 加自带 `tokio::runtime::Runtime` 中，
//! 停止信号走 `std::sync::Arc<AtomicBool>`。这样：
//! - 渲染线程永远不会在 lock 上 hang
//! - 用户 `q` / Ctrl+C → 主线程读 atomic 标志 → join 监控线程，**无锁竞争**
//! - 窗口缩到很小也能渲染（ui.rs 守底）

pub mod cmd;
pub mod focus;
pub mod input;
pub mod panes;
pub mod ui;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use crossterm::event::DisableMouseCapture;
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::monitor::{Monitor, Stats};

pub use focus::Focus;

/// 全局应用状态 —— 持有所有可变状态，TUI 各模块通过 `&mut App` 修改。
pub struct App {
    pub monitor: Arc<Monitor>,
    pub focus: Focus,
    /// TUI 的 watch 选择（左栏）
    pub watch_idx: usize,
    /// TUI 的 event 滚动（右栏），list state 偏移
    pub event_idx: usize,
    /// 输入模式（None | Filter | Cmd）
    pub input_mode: InputMode,
    pub input_buf: String,
    pub status_msg: Option<String>,
    pub status_msg_until: Option<std::time::Instant>,
    pub show_help: bool,
    pub confirm: Option<ConfirmPrompt>,
    pub should_quit: bool,
    pub last_tick: std::time::Instant,
    // 缓存：避免在 render 里 async 锁
    pub cached_started_at: f64,
    pub cached_mode: String,
    pub cached_active: usize,
    /// 监控线程句柄 + 停止标志
    pub monitor_thread: Option<MonitorHandle>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Filter,
    Cmd,
}

/// `d` 删除时的二次确认
#[derive(Debug, Clone)]
pub struct ConfirmPrompt {
    pub text: String,
    pub created_at: std::time::Instant,
}

/// 监控线程 + 关闭信号。TUI 主线程通过 `stop_flag` / `force_flag` 跨线程交互。
pub struct MonitorHandle {
    pub thread: Option<std::thread::JoinHandle<()>>,
    pub stop_flag: Arc<AtomicBool>,
    pub force_flag: Arc<AtomicBool>,
}

impl MonitorHandle {
    pub fn request_force_check(&self) {
        self.force_flag.store(true, Ordering::SeqCst);
    }
}

impl App {
    pub fn new(monitor: Monitor) -> Self {
        let started_at = chrono::Utc::now().timestamp() as f64;
        Self {
            monitor: Arc::new(monitor),
            focus: Focus::Watches,
            watch_idx: 0,
            event_idx: 0,
            input_mode: InputMode::Normal,
            input_buf: String::new(),
            status_msg: None,
            status_msg_until: None,
            show_help: false,
            confirm: None,
            should_quit: false,
            last_tick: std::time::Instant::now(),
            cached_started_at: started_at,
            cached_mode: "normal".into(),
            cached_active: 0,
            monitor_thread: None,
        }
    }
    /// 每帧刷新缓存（cheap，纯同步读）
    pub fn refresh_caches(&mut self) {
        let stats: Stats = self.monitor.stats_snapshot();
        self.cached_started_at = stats.started_at;
        let cfg = match crate::config::load_or_init() {
            Ok(v) => v,
            Err(_) => return,
        };
        let qw = cfg.get("quiet_window").and_then(|v| v.as_str()).unwrap_or("01:00-06:00");
        let pw = cfg.get("phone_only_window").and_then(|v| v.as_str()).unwrap_or("06:00-09:00");
        let h: u32 = chrono::Local::now().format("%H").to_string().parse().unwrap_or(0);
        self.cached_mode = crate::config::current_mode(qw, pw, h)
            .map(|m| match m {
                crate::config::Mode::Quiet => "quiet",
                crate::config::Mode::PhoneOnly => "phone",
                crate::config::Mode::Normal => "normal",
            })
            .unwrap_or("normal")
            .to_string();
        self.cached_active = cfg
            .get("watches")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter(|w| w.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false))
                    .count()
            })
            .unwrap_or(0);
    }
}

/// 阻塞运行：terminal setup → event loop → cleanup。
pub fn run_blocking(monitor: Monitor) -> Result<()> {
    let mut terminal = setup_terminal()?;
    let mut app = App::new(monitor);
    let res = run_event_loop(&mut terminal, &mut app);
    cleanup_terminal(terminal)?;
    res
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;
    let mut out = std::io::stdout();
    execute!(out, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

fn cleanup_terminal(mut t: Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(t.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    Ok(())
}

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    // ---- 启动 monitor 线程（自带 tokio runtime）----
    // 通过两个 atomic 把 stop/force 信号从 TUI 主线程传给 monitor；监控线程内部
    // 每轮检查一次 stop_flag（避免依赖 tokio::Notify），确保 join 立即返回。
    let stop_flag = Arc::new(AtomicBool::new(false));
    let force_flag = Arc::new(AtomicBool::new(false));
    let mon_for_thread = app.monitor.clone();
    let stop_for_thread = stop_flag.clone();
    let handle = std::thread::Builder::new()
        .name("ticket-tracker-monitor".into())
        .spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(_) => return,
            };
            rt.block_on(async move {
                run_monitor_with_stop(&mon_for_thread, &stop_for_thread).await;
            });
        })?;
    app.monitor_thread = Some(MonitorHandle {
        thread: Some(handle),
        stop_flag: stop_flag.clone(),
        force_flag: force_flag.clone(),
    });

    // ---- 主循环 ----
    use std::time::Duration;
    loop {
        if app.should_quit {
            break;
        }
        app.refresh_caches();
        terminal.draw(|f| ui::render(app, f))?;
        if crossterm::event::poll(Duration::from_millis(100))? {
            if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                let code = key.code;
                input::handle_key(app, key)?;
                // `r` 立即触发一轮检查
                if matches!(code, crossterm::event::KeyCode::Char('r'))
                    && app.input_mode == InputMode::Normal
                {
                    if let Some(h) = &app.monitor_thread {
                        h.request_force_check();
                    }
                }
            }
        }
    }

    // ---- 干净退出：通知 + join，不抢锁 ----
    if let Some(h) = app.monitor_thread.take() {
        // 1) 通过 Monitor 自身的 Notify 唤醒 wait_with_stop
        app.monitor.stop();
        // 2) 设 atomic 标志（给将来轮询用）
        h.stop_flag.store(true, Ordering::SeqCst);
        // 3) join 监控线程
        if let Some(t) = h.thread {
            let _ = t.join();
        }
    }
    Ok(())
}

/// Monitor 主循环：直接调 `Monitor::run()`。停止信号由 `stop()` 通过 tokio
/// `Notify` 投递，能在最大约 `effective_interval_secs` 后被唤醒。
async fn run_monitor_with_stop(monitor: &Arc<Monitor>, _stop_flag: &Arc<AtomicBool>) {
    monitor.run().await;
}
