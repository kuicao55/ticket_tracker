//! ticket-tracker (Rust 重写版) —— 库入口。
//!
//! 模块划分（见 `docs/RUST_PORT.md §3.1`）：
//! - [`paths`]  —— XDG 路径
//! - [`config`] —— v2 配置 schema + 迁移
//! - [`presets`] —— 内置影院
//! - [`maoyan`] —— 猫眼 HTTP 客户端
//! - [`notify`] —— Discord + macOS 通知
//! - [`monitor`] —— tokio 监测循环
//! - [`cli`]   —— clap CLI 子命令
//! - [`tui`]   —— ratatui TUI
//!
//! 两端共享同一份 `~/.config/ticket-tracker/config.json`。

pub mod paths;
pub mod presets;
pub mod config;
pub mod maoyan;
pub mod notify;
pub mod monitor;
pub mod cli;
pub mod tui;

pub use config::Config;
pub use monitor::Monitor;
pub use paths::{config_dir, config_file, log_file, pid_file, state_dir};
pub use presets::{list_presets, get_preset, PRESETS};
