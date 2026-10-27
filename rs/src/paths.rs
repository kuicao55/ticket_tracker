//! XDG 路径解析。

use std::path::{Path, PathBuf};

const APP_NAME: &str = "ticket-tracker";

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

pub fn log_file() -> PathBuf {
    state_dir().join("ticket-tracker.log")
}

pub fn pid_file() -> PathBuf {
    state_dir().join("ticket-tracker.pid")
}

fn ensure_dir(p: &Path) {
    let _ = std::fs::create_dir_all(p);
}
