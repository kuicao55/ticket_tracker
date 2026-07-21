//! TUI 入口 —— ratatui + crossterm，三栏 + 状态栏 + `:` 命令面板。
//!
//! 详见 RUST_PORT.md §7。

pub mod cmd;
pub mod focus;
pub mod input;
pub mod panes;
pub mod ui;

use std::sync::Arc;

use anyhow::Result;
use crossterm::event::DisableMouseCapture;
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::Mutex;

use crate::monitor::Monitor;

pub use focus::Focus;

/// 全局应用状态 —— 持有所有可变状态，TUI 各模块通过 `&mut App` 修改。
pub struct App {
    pub monitor: Arc<Mutex<Monitor>>,
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
    /// 主循环已经在的 tokio handle
    pub rt: Option<tokio::runtime::Handle>,
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

impl App {
    pub fn new(monitor: Monitor) -> Self {
        let started_at = chrono::Utc::now().timestamp() as f64;
        Self {
            monitor: Arc::new(Mutex::new(monitor)),
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
            rt: None,
        }
    }
    /// 每帧刷新缓存（cheap）
    pub fn refresh_caches(&mut self) {
        let rt = match tokio::runtime::Handle::try_current() {
            Ok(h) => h,
            Err(_) => return,
        };
        let mon = self.monitor.clone();
        let stats_snap: crate::monitor::Stats = rt.block_on(async {
            let g = mon.lock().await;
            let s = g.stats.lock().await.clone();
            s
        });
        self.cached_started_at = stats_snap.started_at;
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
    let rt = tokio::runtime::Runtime::new()?;
    let _guard = rt.enter();
    app.rt = Some(rt.handle().clone());

    // 启动 monitor
    let mon = app.monitor.clone();
    rt.spawn(async move {
        let mut m = mon.lock().await;
        m.run().await;
    });

    // 主循环
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
                if matches!(code, crossterm::event::KeyCode::Char('r')) && app.input_mode == InputMode::Normal {
                    let mon = app.monitor.clone();
                    rt.spawn(async move {
                        let m = mon.lock().await;
                        m.force_check();
                    });
                }
            }
        }
    }
    // 通知 monitor 停止
    rt.block_on(async {
        let m = app.monitor.lock().await;
        m.stop();
    });
    Ok(())
}
