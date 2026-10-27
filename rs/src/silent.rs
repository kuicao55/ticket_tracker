//! 全局静默时段。
//!
//! 与 per-watch 的 `time_window`（只过滤场次时间）不同，这里控制的是**整个进程**：
//! 静默期间不抓取、不发任何通知。
//!
//! 支持分钟精度与跨午夜窗口，例如：
//! - `01:00-06:00`（普通）
//! - `23:30-07:15`（跨午夜）
//! - `null` / `""` / `start == end` → 关闭静默

use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration as ChronoDuration, Local, Timelike};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SilentWindow {
    /// 一天内的分钟数，0..=1439
    pub start_min: u16,
    /// 一天内的分钟数，0..=1439
    pub end_min: u16,
}

impl SilentWindow {
    /// 解析 `HH:MM-HH:MM`。`start == end` 合法但表示空窗口。
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        let (a, b) = s
            .split_once('-')
            .ok_or_else(|| anyhow!("静默时段格式错误，应像 '01:00-06:00'"))?;
        let parse_hhmm = |t: &str| -> Result<u16> {
            let (h, m) = t
                .split_once(':')
                .ok_or_else(|| anyhow!("静默时段格式错误：'{}'", t))?;
            let h: u16 = h.parse().map_err(|_| anyhow!("静默时段小时非法"))?;
            let m: u16 = m.parse().map_err(|_| anyhow!("静默时段分钟非法"))?;
            if h > 23 || m > 59 {
                return Err(anyhow!("静默时段超出范围"));
            }
            Ok(h * 60 + m)
        };
        Ok(Self {
            start_min: parse_hhmm(a.trim())?,
            end_min: parse_hhmm(b.trim())?,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.start_min == self.end_min
    }

    fn minute_of_day(now: DateTime<Local>) -> u16 {
        (now.hour() * 60 + now.minute()) as u16
    }

    /// 当前是否在静默窗口内。起点含、终点不含。
    pub fn contains(&self, now: DateTime<Local>) -> bool {
        if self.is_empty() {
            return false;
        }
        let t = Self::minute_of_day(now);
        if self.start_min < self.end_min {
            self.start_min <= t && t < self.end_min
        } else {
            t >= self.start_min || t < self.end_min
        }
    }

    /// 假设 `now` 在窗口内，返回本次窗口结束的时间点。
    /// 用“距结束还有多少分钟”计算，避免手工构造跨 DST 的本地时间。
    pub fn next_end(&self, now: DateTime<Local>) -> DateTime<Local> {
        if self.is_empty() {
            return now;
        }
        let t = Self::minute_of_day(now);
        let mins_until_end = if self.start_min < self.end_min {
            if t < self.end_min {
                self.end_min - t
            } else {
                (24 * 60 - t) + self.end_min
            }
        } else if t >= self.start_min {
            // 跨午夜，当前在午夜前
            (24 * 60 - t) + self.end_min
        } else {
            // 跨午夜，当前在午夜后、end 前
            self.end_min.saturating_sub(t)
        };
        now + ChronoDuration::minutes(mins_until_end as i64)
    }
}

/// 从 config 读取静默窗口。关闭 / 非法 → None。
pub fn silent_window(cfg: &Value) -> Option<SilentWindow> {
    let raw = cfg.get("silent_window").and_then(|v| v.as_str())?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    match SilentWindow::parse(raw) {
        Ok(w) if !w.is_empty() => Some(w),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!("忽略非法的 silent_window '{}': {}", raw, e);
            None
        }
    }
}

pub fn is_silent_now(cfg: &Value) -> bool {
    silent_window(cfg)
        .map(|w| w.contains(Local::now()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(h: u32, m: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 10, 24, h, m, 0)
            .single()
            .expect("valid local time")
    }

    #[test]
    fn parse_normal_and_cross_midnight() {
        assert_eq!(
            SilentWindow::parse("01:00-06:00").unwrap(),
            SilentWindow {
                start_min: 60,
                end_min: 360
            }
        );
        assert_eq!(
            SilentWindow::parse("23:30-07:15").unwrap(),
            SilentWindow {
                start_min: 1410,
                end_min: 435
            }
        );
        assert!(SilentWindow::parse("25:00-10:00").is_err());
        assert!(SilentWindow::parse("abc").is_err());
        assert!(SilentWindow::parse("").is_err());
    }

    #[test]
    fn contains_normal_window_start_inclusive_end_exclusive() {
        let w = SilentWindow::parse("01:00-06:00").unwrap();
        assert!(!w.contains(at(0, 59)));
        assert!(w.contains(at(1, 0)));
        assert!(w.contains(at(5, 59)));
        assert!(!w.contains(at(6, 0)));
        assert!(!w.contains(at(12, 0)));
    }

    #[test]
    fn contains_cross_midnight_window() {
        let w = SilentWindow::parse("23:30-07:15").unwrap();
        assert!(!w.contains(at(23, 29)));
        assert!(w.contains(at(23, 30)));
        assert!(w.contains(at(0, 0)));
        assert!(w.contains(at(7, 14)));
        assert!(!w.contains(at(7, 15)));
        assert!(!w.contains(at(12, 0)));
    }

    #[test]
    fn equal_start_and_end_means_disabled() {
        let w = SilentWindow::parse("06:00-06:00").unwrap();
        assert!(!w.contains(at(6, 0)));
        assert!(!w.contains(at(0, 0)));
    }

    #[test]
    fn next_end_returns_same_day_end() {
        let w = SilentWindow::parse("01:00-06:00").unwrap();
        assert_eq!(w.next_end(at(2, 30)), at(6, 0));
        // 不在窗口内时也返回“下一次结束”
        assert_eq!(w.next_end(at(12, 0)), at(6, 0) + ChronoDuration::days(1));
    }

    #[test]
    fn next_end_handles_cross_midnight() {
        let w = SilentWindow::parse("23:30-07:15").unwrap();
        assert_eq!(w.next_end(at(23, 45)), at(7, 15) + ChronoDuration::days(1));
        assert_eq!(w.next_end(at(2, 0)), at(7, 15));
    }

    #[test]
    fn cfg_helpers_accept_null_empty_and_invalid_as_disabled() {
        let cfg = serde_json::json!({ "silent_window": null });
        assert!(silent_window(&cfg).is_none());
        let cfg = serde_json::json!({ "silent_window": "" });
        assert!(silent_window(&cfg).is_none());
        let cfg = serde_json::json!({ "silent_window": "garbage" });
        assert!(silent_window(&cfg).is_none());
        let cfg = serde_json::json!({ "silent_window": "01:00-06:00" });
        assert!(silent_window(&cfg).is_some());
    }
}
