//! XDG 路径解析 —— 与 py/.../paths.py 1:1 对齐（见 RUST_PORT.md §4.4）。

use std::path::{Path, PathBuf};

const APP_NAME: &str = "ticket-tracker";

/// `XDG_CONFIG_HOME` 或 `~/.config`，叠加 `ticket-tracker/`。
///
/// 注意：故意不使用 `dirs::config_dir()`（macOS 会回到 `~/Library/Application Support`），
/// 强迫走 XDG 路径，与 Python 版 1:1 兼容（已存在的配置在 `~/.config/ticket-tracker/`）。
pub fn config_dir() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("~/.config"));
    let p = expand(base).join(APP_NAME);
    ensure_dir(&p);
    p
}

/// `XDG_STATE_HOME` 或 `~/.local/state`，叠加 `ticket-tracker/`。
pub fn state_dir() -> PathBuf {
    let base = std::env::var("XDG_STATE_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("~/.local/state"));
    let p = expand(base).join(APP_NAME);
    ensure_dir(&p);
    p
}

fn expand(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if s.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(s.trim_start_matches("~/"));
        }
    }
    p
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.json")
}

pub fn log_file() -> PathBuf {
    state_dir().join("ticket-tracker.log")
}

pub fn pid_file() -> PathBuf {
    state_dir().join("ticket-tracker.pid")
}

fn ensure_dir(p: &Path) {
    let _ = std::fs::create_dir_all(p);
}
