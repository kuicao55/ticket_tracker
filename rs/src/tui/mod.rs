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

pub mod actions;
pub mod cmd;
pub mod focus;
pub mod input;
pub mod modal;
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
    /// 当前聚焦的区块（Watches / Detail / Events / Actions）
    pub focus: Focus,
    /// 层级化导航模式：Top（区块选择）/ In（区块内选择）
    pub focus_mode: FocusMode,
    /// TUI 的 watch 选择（左栏）
    pub watch_idx: usize,
    /// TUI 的 event 滚动（右栏），list state 偏移
    pub event_idx: usize,
    /// Detail 栏内的 per-watch 按钮索引（0..=5）；仅 Detail In 模式下生效
    pub detail_btn_idx: usize,
    /// prompt 期间进入文本输入模式（仅一种：Cmd —— 由 action bar 进入）
    pub input_mode: InputMode,
    pub input_buf: String,
    /// 当前 Action Bar 选中的按钮索引（0..=7）
    pub action_idx: usize,
    /// prompt 期间记录「这条输入是要干什么」
    pub prompt_target: Option<PromptTarget>,
    pub status_msg: Option<String>,
    pub status_msg_until: Option<std::time::Instant>,
    pub show_help: bool,
    pub confirm: Option<ConfirmPrompt>,
    /// 当前打开的表单 / 搜索 / 影院选择弹窗
    pub modal: Option<modal::Modal>,
    pub should_quit: bool,
    pub last_tick: std::time::Instant,
    // 缓存：避免在 render 里 async 锁
    pub cached_started_at: f64,
    pub cached_mode: String,
    pub cached_active: usize,
    /// 监控线程句柄 + 停止标志
    pub monitor_thread: Option<MonitorHandle>,
    /// SIGINT handler 设的标志 —— 主循环检测到即干净退出（让 Monitor::run() 发 Discord 「已停止」）
    sigint_flag: Arc<AtomicBool>,
}

/// 层级化导航模式。
/// - Top：方向键在 4 区块间切；Enter / ↓ 进入当前区块（→ In）
/// - In：方向键在当前区块内操作；Esc 退回 Top；Enter 触发
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusMode {
    Top,
    In,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Cmd,
}

/// Action Bar `[⚙]` 配置按钮触发的循环 prompt 目标。
/// 第一次按 `[⚙]` 进 Webhook；submit 后下一项 Quiet；最末一项后再 [⚙] 关。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptTarget {
    Webhook,
    Quiet,
    Phone,
    Interval,
    Films,
    Doctor,
}

impl PromptTarget {
    pub fn label(&self) -> &'static str {
        match self {
            PromptTarget::Webhook => "webhook",
            PromptTarget::Quiet => "quiet",
            PromptTarget::Phone => "phone",
            PromptTarget::Interval => "interval",
            PromptTarget::Films => "films",
            PromptTarget::Doctor => "doctor",
        }
    }
    /// 循环到下一项
    pub fn next(&self) -> Self {
        match self {
            PromptTarget::Webhook => PromptTarget::Quiet,
            PromptTarget::Quiet => PromptTarget::Phone,
            PromptTarget::Phone => PromptTarget::Interval,
            PromptTarget::Interval => PromptTarget::Films,
            PromptTarget::Films => PromptTarget::Doctor,
            PromptTarget::Doctor => PromptTarget::Webhook,
        }
    }
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
            focus_mode: FocusMode::Top,
            watch_idx: 0,
            event_idx: 0,
            detail_btn_idx: 0,
            input_mode: InputMode::Normal,
            input_buf: String::new(),
            action_idx: 0,
            prompt_target: None,
            status_msg: None,
            status_msg_until: None,
            show_help: false,
            confirm: None,
            modal: None,
            should_quit: false,
            last_tick: std::time::Instant::now(),
            cached_started_at: started_at,
            cached_mode: "normal".into(),
            cached_active: 0,
            monitor_thread: None,
            sigint_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Ctrl+C handler 调用：设置 should_quit 并记录是 SIGINT 触发的（无副作用，目前仅日志需要）。
    pub fn request_quit(&mut self) {
        self.should_quit = true;
    }

    /// 主线程读 sigint_flag
    pub fn sigint_pending(&self) -> bool {
        self.sigint_flag.load(Ordering::SeqCst)
    }

    /// 每帧推进弹窗里的后台请求状态。
    pub fn pump_async(&mut self) {
        modal::pump(self);
    }

    /// 每帧刷新缓存（cheap，纯同步读）
    pub fn refresh_caches(&mut self) {
        let stats: Stats = self.monitor.stats_snapshot();
        self.cached_started_at = stats.started_at;
        let cfg = match crate::config::load_or_init() {
            Ok(v) => v,
            Err(_) => return,
        };
        let qw = cfg
            .get("quiet_window")
            .and_then(|v| v.as_str())
            .unwrap_or("01:00-06:00");
        let pw = cfg
            .get("phone_only_window")
            .and_then(|v| v.as_str())
            .unwrap_or("06:00-09:00");
        let h: u32 = chrono::Local::now()
            .format("%H")
            .to_string()
            .parse()
            .unwrap_or(0);
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
    // ---- 抢注 SIGINT handler：在独立线程里跑一个 mini tokio runtime 监听 Ctrl+C。
    // Unix tty driver 在 raw mode 下仍然会把 Ctrl+C 发成 SIGINT（ISIG 仍启用）；
    // 一旦 Tokio 接管这个信号，默认 handler 不再杀进程，主循环就能干净走 cleanup。
    let sigint_flag = app.sigint_flag.clone();
    let _sigint_thread = std::thread::Builder::new()
        .name("ticket-tracker-sigint".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(_) => return,
            };
            rt.block_on(async move {
                let _ = tokio::signal::ctrl_c().await;
                sigint_flag.store(true, Ordering::SeqCst);
            });
        })?;

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
        if app.should_quit || app.sigint_pending() {
            // sigint 触发的退出也要走 cleanup
            if app.sigint_pending() {
                app.should_quit = true;
            }
            break;
        }
        app.refresh_caches();
        app.pump_async();
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
