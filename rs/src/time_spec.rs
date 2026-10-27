//! 统一的 watch 时间规则。
//!
//! 最小单位是 **1 小时桶**：
//! - `18:00` 表示 `[18:00, 19:00)`，所以 18:10 / 18:59 都算 18 点。
//! - 不支持 18:10、18:25 这类分钟边界。
//!
//! 支持的写法：
//! - 空 / `all`：不限日期、不限时间
//! - `13:00-14:00`：每天 13 点（所有日期）
//! - `2026-11-12`：2026-11-12 整天
//! - `2026-11-12 13:00-14:00`：指定日期 + 小时段
//! - 逗号分隔多个：`2026-11-12,2026-11-13 19:00-22:00`
//!
//! 内部桶 key：`date.num_days_from_ce() * 24 + hour`。

use anyhow::{anyhow, Result};
use chrono::{Datelike, NaiveDate};
use serde_json::{json, Value};

pub type BucketKey = u32;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TimeSpec {
    /// 所有日期、所有小时
    All,
    /// 所有日期，只保留这些小时（0..=23）
    DailyHours(Vec<u8>),
    /// 有限个日期+小时区间
    Intervals(Vec<TimeInterval>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TimeInterval {
    pub start_date: NaiveDate,
    /// 0..=23
    pub start_hour: u8,
    pub end_date: NaiveDate,
    /// 0..=24；24 表示 end_date 当天结束（次日 00:00）
    pub end_hour: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeMask {
    All,
    DailyHours(Vec<u8>),
    /// 排序、去重后的桶 key
    Intervals(Vec<BucketKey>),
}

pub fn date_id(d: NaiveDate) -> u32 {
    d.num_days_from_ce() as u32
}

pub fn date_from_id(id: u32) -> Option<NaiveDate> {
    NaiveDate::from_num_days_from_ce_opt(id as i32)
}

pub fn bucket_key(d: NaiveDate, hour: u8) -> BucketKey {
    date_id(d) * 24 + hour as u32
}

pub fn bucket_key_str(d: NaiveDate, hour: u8) -> String {
    format!("{} {:02}", d.format("%Y-%m-%d"), hour)
}

pub fn parse_bucket_key(s: &str) -> Result<(NaiveDate, u8)> {
    let s = s.trim();
    let (d, h) = s
        .split_once(' ')
        .ok_or_else(|| anyhow!("桶 key 应像 '2026-11-12 19'"))?;
    let date = NaiveDate::parse_from_str(d, "%Y-%m-%d")
        .map_err(|e| anyhow!("日期格式错误 '{}': {}", d, e))?;
    let hour: u8 = h.parse().map_err(|_| anyhow!("小时格式错误 '{}'", h))?;
    if hour > 23 {
        return Err(anyhow!("小时超出范围: {}", hour));
    }
    Ok((date, hour))
}

fn parse_hour_minute(s: &str) -> Result<(u8, u8)> {
    let (h, m) = s
        .split_once(':')
        .ok_or_else(|| anyhow!("时间格式错误，应像 '18:00'"))?;
    let h: u8 = h.parse().map_err(|_| anyhow!("小时非法: {}", h))?;
    let m: u8 = m.parse().map_err(|_| anyhow!("分钟非法: {}", m))?;
    if h > 24 || m > 59 {
        return Err(anyhow!("时间超出范围: {}", s));
    }
    if m != 0 {
        return Err(anyhow!(
            "watch 时间最小单位是整点，不能使用分钟边界: {}（只允许 HH:00）",
            s
        ));
    }
    Ok((h, m))
}

/// 解析 `HH:MM-HH:MM`，要求分钟为 00。
/// 返回 `(start_hour, end_hour)`，end_hour 允许 24。
fn parse_hour_range(s: &str) -> Result<(u8, u8)> {
    let (a, b) = s
        .split_once('-')
        .ok_or_else(|| anyhow!("时间范围格式错误，应像 '18:00-22:00'"))?;
    let (sh, _) = parse_hour_minute(a.trim())?;
    let (eh, _) = parse_hour_minute(b.trim())?;
    if sh >= 24 {
        return Err(anyhow!("起始小时不能是 24:00"));
    }
    if sh == eh {
        return Err(anyhow!("时间范围不能为空: {}", s));
    }
    Ok((sh, eh))
}

/// 解析用户输入。
pub fn parse(s: &str) -> Result<TimeSpec> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("all") || s == "不限" {
        return Ok(TimeSpec::All);
    }

    let parts: Vec<&str> = s
        .split([',', '，', ';', '；'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return Ok(TimeSpec::All);
    }

    let mut daily_hours: Vec<u8> = Vec::new();
    let mut intervals: Vec<TimeInterval> = Vec::new();
    let mut saw_all = false;

    for part in parts {
        if part.eq_ignore_ascii_case("all") || part == "不限" {
            saw_all = true;
            continue;
        }

        // 日期开头：YYYY-MM-DD
        if looks_like_date_prefix(part) {
            let date_str = &part[..10];
            let date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
                .map_err(|e| anyhow!("日期格式错误 '{}': {}", date_str, e))?;
            let rest = part[10..].trim();
            if rest.is_empty() {
                intervals.push(TimeInterval {
                    start_date: date,
                    start_hour: 0,
                    end_date: date,
                    end_hour: 24,
                });
            } else {
                let range = rest.trim_start_matches([' ', 'T', 't']);
                let (sh, eh) = parse_hour_range(range)?;
                let (end_date, end_hour) = if eh == 24 {
                    (date, 24)
                } else if eh <= sh {
                    // 跨午夜
                    (date.succ_opt().ok_or_else(|| anyhow!("日期溢出"))?, eh)
                } else {
                    (date, eh)
                };
                intervals.push(TimeInterval {
                    start_date: date,
                    start_hour: sh,
                    end_date,
                    end_hour,
                });
            }
        } else if part.contains(':') && part.contains('-') {
            // 只有时间：所有日期的这些小时
            let (sh, eh) = parse_hour_range(part)?;
            if eh <= sh {
                // 跨午夜的每日时段：23:00-01:00 -> [23] + [0]
                daily_hours.extend(sh..24);
                daily_hours.extend(0..eh);
            } else {
                daily_hours.extend(sh..eh);
            }
        } else {
            return Err(anyhow!(
                "无法识别的时间规则 '{}'；示例：all / 2026-11-12 / 2026-11-12 19:00-22:00 / 19:00-22:00",
                part
            ));
        }
    }

    if saw_all {
        return Ok(TimeSpec::All);
    }
    if !daily_hours.is_empty() && !intervals.is_empty() {
        return Err(anyhow!(
            "不能同时混用“只有小时”和“指定日期”的规则；请分开成多个 watch"
        ));
    }
    if !daily_hours.is_empty() {
        daily_hours.sort_unstable();
        daily_hours.dedup();
        if daily_hours.len() == 24 {
            return Ok(TimeSpec::All);
        }
        return Ok(TimeSpec::DailyHours(daily_hours));
    }
    Ok(TimeSpec::Intervals(normalize_intervals(intervals)))
}

fn looks_like_date_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..4].iter().all(|c| c.is_ascii_digit())
        && b[5..7].iter().all(|c| c.is_ascii_digit())
        && b[8..10].iter().all(|c| c.is_ascii_digit())
}

fn interval_bounds(i: &TimeInterval) -> (BucketKey, BucketKey) {
    let start = bucket_key(i.start_date, i.start_hour);
    let end = if i.end_hour == 24 {
        bucket_key(i.end_date, 0) + 24
    } else {
        bucket_key(i.end_date, i.end_hour)
    };
    (start, end)
}

fn interval_from_bounds(start: BucketKey, end: BucketKey) -> TimeInterval {
    let start_date =
        date_from_id(start / 24).unwrap_or_else(|| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap());
    let start_hour = (start % 24) as u8;
    let end_date =
        date_from_id(end / 24).unwrap_or_else(|| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap());
    let end_hour = (end % 24) as u8;
    // 归一到 end_hour=24 更易读（当 end 正好是某天 00:00）
    if end_hour == 0 {
        if let Some(prev) = end_date.pred_opt() {
            return TimeInterval {
                start_date,
                start_hour,
                end_date: prev,
                end_hour: 24,
            };
        }
    }
    TimeInterval {
        start_date,
        start_hour,
        end_date,
        end_hour,
    }
}

/// 排序 + 合并重叠/相邻区间。
pub fn normalize_intervals(mut intervals: Vec<TimeInterval>) -> Vec<TimeInterval> {
    if intervals.is_empty() {
        return intervals;
    }
    let mut bounds: Vec<(BucketKey, BucketKey)> = intervals
        .drain(..)
        .map(|i| interval_bounds(&i))
        .filter(|(s, e)| s < e)
        .collect();
    bounds.sort_unstable();
    let mut merged: Vec<(BucketKey, BucketKey)> = Vec::new();
    for (s, e) in bounds {
        if let Some(last) = merged.last_mut() {
            if s <= last.1 {
                if e > last.1 {
                    last.1 = e;
                }
                continue;
            }
        }
        merged.push((s, e));
    }
    merged
        .into_iter()
        .map(|(s, e)| interval_from_bounds(s, e))
        .collect()
}

impl TimeSpec {
    /// 解析用户输入（便捷入口）。
    pub fn parse(s: &str) -> Result<Self> {
        parse(s)
    }

    pub fn normalize(self) -> Self {
        match self {
            TimeSpec::Intervals(v) => TimeSpec::Intervals(normalize_intervals(v)),
            TimeSpec::DailyHours(mut h) => {
                h.sort_unstable();
                h.dedup();
                if h.len() == 24 {
                    TimeSpec::All
                } else {
                    TimeSpec::DailyHours(h)
                }
            }
            TimeSpec::All => TimeSpec::All,
        }
    }

    pub fn key(&self) -> String {
        match self {
            TimeSpec::All => "all".to_string(),
            TimeSpec::DailyHours(h) => {
                let mut h = h.clone();
                h.sort_unstable();
                h.dedup();
                format!(
                    "daily:{}",
                    h.iter()
                        .map(|x| x.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                )
            }
            TimeSpec::Intervals(v) => {
                let mut s = String::from("int:");
                for (idx, i) in v.iter().enumerate() {
                    if idx > 0 {
                        s.push(';');
                    }
                    s.push_str(&format!(
                        "{}T{}:{}-{}T{}:{}",
                        i.start_date.format("%Y-%m-%d"),
                        i.start_hour,
                        0,
                        i.end_date.format("%Y-%m-%d"),
                        i.end_hour,
                        0
                    ));
                }
                s
            }
        }
    }

    pub fn to_json(&self) -> Value {
        match self {
            TimeSpec::All => json!({"type":"all"}),
            TimeSpec::DailyHours(h) => json!({"type":"daily_hours","hours":h}),
            TimeSpec::Intervals(v) => json!({
                "type": "intervals",
                "intervals": v.iter().map(|i| json!({
                    "start_date": i.start_date.format("%Y-%m-%d").to_string(),
                    "start_hour": i.start_hour,
                    "end_date": i.end_date.format("%Y-%m-%d").to_string(),
                    "end_hour": i.end_hour,
                })).collect::<Vec<_>>()
            }),
        }
    }

    pub fn from_json(v: &Value) -> Result<Self> {
        if v.is_null() {
            return Ok(TimeSpec::All);
        }
        if let Some(s) = v.as_str() {
            return parse(s);
        }
        let kind = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        match kind {
            "all" => Ok(TimeSpec::All),
            "daily_hours" => {
                let hours: Vec<u8> = v
                    .get("hours")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_u64().map(|n| n as u8))
                            .filter(|h| *h < 24)
                            .collect()
                    })
                    .unwrap_or_default();
                if hours.is_empty() {
                    return Err(anyhow!("daily_hours.hours 不能为空"));
                }
                let mut t = TimeSpec::DailyHours(hours);
                t = t.normalize();
                Ok(t)
            }
            "intervals" => {
                let mut out = Vec::new();
                if let Some(arr) = v.get("intervals").and_then(|x| x.as_array()) {
                    for item in arr {
                        let sd = item
                            .get("start_date")
                            .and_then(|x| x.as_str())
                            .ok_or_else(|| anyhow!("interval 缺少 start_date"))?;
                        let ed = item
                            .get("end_date")
                            .and_then(|x| x.as_str())
                            .ok_or_else(|| anyhow!("interval 缺少 end_date"))?;
                        let sh = item
                            .get("start_hour")
                            .and_then(|x| x.as_u64())
                            .ok_or_else(|| anyhow!("interval 缺少 start_hour"))?
                            as u8;
                        let eh = item
                            .get("end_hour")
                            .and_then(|x| x.as_u64())
                            .ok_or_else(|| anyhow!("interval 缺少 end_hour"))?
                            as u8;
                        if sh > 23 || eh > 24 {
                            return Err(anyhow!("interval 小时超出范围"));
                        }
                        let start_date = NaiveDate::parse_from_str(sd, "%Y-%m-%d")
                            .map_err(|e| anyhow!("start_date 非法: {}", e))?;
                        let end_date = NaiveDate::parse_from_str(ed, "%Y-%m-%d")
                            .map_err(|e| anyhow!("end_date 非法: {}", e))?;
                        let (s, e2) = interval_bounds(&TimeInterval {
                            start_date,
                            start_hour: sh,
                            end_date,
                            end_hour: eh,
                        });
                        if s >= e2 {
                            return Err(anyhow!("interval 起点必须早于终点"));
                        }
                        out.push(TimeInterval {
                            start_date,
                            start_hour: sh,
                            end_date,
                            end_hour: eh,
                        });
                    }
                }
                if out.is_empty() {
                    return Err(anyhow!("intervals 不能为空"));
                }
                Ok(TimeSpec::Intervals(normalize_intervals(out)))
            }
            _ => Err(anyhow!("未知 time_spec 类型: {}", kind)),
        }
    }

    /// 转回可被 `parse()` 再次解析的紧凑写法。
    pub fn to_input(&self) -> String {
        match self {
            TimeSpec::All => String::new(),
            TimeSpec::DailyHours(h) => {
                let mut h = h.clone();
                h.sort_unstable();
                h.dedup();
                let mut runs: Vec<(u8, u8)> = Vec::new();
                for hour in h {
                    if let Some(last) = runs.last_mut() {
                        if hour == last.1 {
                            last.1 = hour + 1;
                            continue;
                        }
                    }
                    runs.push((hour, hour + 1));
                }
                runs.into_iter()
                    .map(|(s, e)| format!("{:02}:00-{:02}:00", s, e))
                    .collect::<Vec<_>>()
                    .join(",")
            }
            TimeSpec::Intervals(v) => {
                let mut parts = Vec::new();
                let mut seen = std::collections::BTreeSet::new();
                for i in v {
                    let mut d = i.start_date;
                    while d <= i.end_date {
                        if seen.insert(d) {
                            if let Some((lo, hi)) = self.hour_range_for_date(d) {
                                if lo == 0 && hi >= 24 {
                                    parts.push(d.format("%Y-%m-%d").to_string());
                                } else {
                                    parts.push(format!(
                                        "{} {:02}:00-{:02}:00",
                                        d.format("%Y-%m-%d"),
                                        lo,
                                        hi.min(24)
                                    ));
                                }
                            }
                        }
                        let Some(next) = d.succ_opt() else { break };
                        d = next;
                    }
                }
                parts.join(",")
            }
        }
    }

    pub fn summary(&self) -> String {
        match self {
            TimeSpec::All => "不限".to_string(),
            TimeSpec::DailyHours(h) => {
                format!(
                    "每天 {}",
                    h.iter()
                        .map(|x| format!("{:02}:00", x))
                        .collect::<Vec<_>>()
                        .join("、")
                )
            }
            TimeSpec::Intervals(v) => v
                .iter()
                .map(|i| {
                    if i.start_hour == 0 && i.end_hour == 24 && i.start_date == i.end_date {
                        format!("{} 全天", i.start_date.format("%Y-%m-%d"))
                    } else if i.start_date == i.end_date {
                        format!(
                            "{} {:02}:00-{:02}:00",
                            i.start_date.format("%Y-%m-%d"),
                            i.start_hour,
                            i.end_hour
                        )
                    } else {
                        format!(
                            "{} {:02}:00 至 {} {:02}:00",
                            i.start_date.format("%Y-%m-%d"),
                            i.start_hour,
                            i.end_date.format("%Y-%m-%d"),
                            i.end_hour
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join("；"),
        }
    }

    pub fn compile_mask(&self) -> Result<TimeMask> {
        match self {
            TimeSpec::All => Ok(TimeMask::All),
            TimeSpec::DailyHours(h) => {
                let mut h = h.clone();
                h.sort_unstable();
                h.dedup();
                Ok(TimeMask::DailyHours(h))
            }
            TimeSpec::Intervals(v) => {
                let mut keys = Vec::new();
                for i in v {
                    let (s, e) = interval_bounds(i);
                    if e - s > 24 * 366 * 5 {
                        return Err(anyhow!(
                            "时间范围过大（超过 5 年）；请使用“不限日期 + 每天小时”的写法"
                        ));
                    }
                    keys.extend(s..e);
                }
                keys.sort_unstable();
                keys.dedup();
                Ok(TimeMask::Intervals(keys))
            }
        }
    }

    /// 所有有限日期是否都已经过去。
    pub fn all_dates_past(&self, today: NaiveDate) -> bool {
        match self {
            TimeSpec::All | TimeSpec::DailyHours(_) => false,
            TimeSpec::Intervals(v) => v.iter().all(|i| i.end_date < today),
        }
    }

    /// 自动锁票用：给定日期，返回当天的小时范围（start_hour, end_hour）。
    pub fn hour_range_for_date(&self, date: NaiveDate) -> Option<(u8, u8)> {
        match self {
            TimeSpec::All => None,
            TimeSpec::DailyHours(h) => {
                let min = *h.iter().min()?;
                let max = *h.iter().max()?;
                Some((min, max.saturating_add(1)))
            }
            TimeSpec::Intervals(v) => {
                let mut lo = 24u8;
                let mut hi = 0u8;
                let mut found = false;
                for i in v {
                    if date < i.start_date || date > i.end_date {
                        continue;
                    }
                    found = true;
                    let s = if date == i.start_date {
                        i.start_hour
                    } else {
                        0
                    };
                    let e = if i.end_hour == 24 {
                        24
                    } else if date == i.end_date {
                        i.end_hour
                    } else {
                        24
                    };
                    lo = lo.min(s);
                    hi = hi.max(e);
                }
                if found {
                    Some((lo, hi))
                } else {
                    None
                }
            }
        }
    }
}

impl TimeMask {
    pub fn is_all(&self) -> bool {
        matches!(self, TimeMask::All)
    }

    pub fn daily_hours(&self) -> Option<&[u8]> {
        match self {
            TimeMask::DailyHours(h) => Some(h),
            _ => None,
        }
    }

    pub fn interval_keys(&self) -> Option<&[BucketKey]> {
        match self {
            TimeMask::Intervals(k) => Some(k),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn parse_all_and_empty() {
        assert_eq!(parse("").unwrap(), TimeSpec::All);
        assert_eq!(parse("all").unwrap(), TimeSpec::All);
        assert_eq!(parse("不限").unwrap(), TimeSpec::All);
    }

    #[test]
    fn parse_daily_hours() {
        assert_eq!(
            parse("13:00-14:00").unwrap(),
            TimeSpec::DailyHours(vec![13])
        );
        assert_eq!(
            parse("18:00-22:00").unwrap(),
            TimeSpec::DailyHours(vec![18, 19, 20, 21])
        );
        assert_eq!(
            parse("23:00-01:00").unwrap(),
            TimeSpec::DailyHours(vec![0, 23])
        );
        assert_eq!(parse("00:00-24:00").unwrap(), TimeSpec::All);
    }

    #[test]
    fn parse_dates_and_windows() {
        assert_eq!(
            parse("2026-11-12").unwrap(),
            TimeSpec::Intervals(vec![TimeInterval {
                start_date: d("2026-11-12"),
                start_hour: 0,
                end_date: d("2026-11-12"),
                end_hour: 24,
            }])
        );
        assert_eq!(
            parse("2026-11-12 13:00-14:00").unwrap(),
            TimeSpec::Intervals(vec![TimeInterval {
                start_date: d("2026-11-12"),
                start_hour: 13,
                end_date: d("2026-11-12"),
                end_hour: 14,
            }])
        );
    }

    #[test]
    fn rejects_minute_boundaries() {
        assert!(parse("2026-11-12 18:10-19:00").is_err());
        assert!(parse("18:30-19:00").is_err());
    }

    #[test]
    fn merges_adjacent_and_overlapping_intervals() {
        let t = parse("2026-11-12 13:00-14:00,2026-11-12 14:00-15:00").unwrap();
        assert_eq!(
            t,
            TimeSpec::Intervals(vec![TimeInterval {
                start_date: d("2026-11-12"),
                start_hour: 13,
                end_date: d("2026-11-12"),
                end_hour: 15,
            }])
        );
    }

    #[test]
    fn crosses_midnight() {
        let t = parse("2026-11-12 23:00-01:00").unwrap();
        assert_eq!(
            t,
            TimeSpec::Intervals(vec![TimeInterval {
                start_date: d("2026-11-12"),
                start_hour: 23,
                end_date: d("2026-11-13"),
                end_hour: 1,
            }])
        );
        let mask = t.compile_mask().unwrap();
        let keys = mask.interval_keys().unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0], bucket_key(d("2026-11-12"), 23));
        assert_eq!(keys[1], bucket_key(d("2026-11-13"), 0));
    }

    #[test]
    fn json_roundtrip() {
        for s in ["all", "13:00-14:00", "2026-11-12", "2026-11-12 13:00-14:00"] {
            let t = parse(s).unwrap();
            let j = t.to_json();
            let back = TimeSpec::from_json(&j).unwrap();
            assert_eq!(t, back);
        }
    }

    #[test]
    fn all_dates_past_only_for_finite_intervals() {
        assert!(!TimeSpec::All.all_dates_past(d("2026-11-12")));
        assert!(!TimeSpec::DailyHours(vec![18]).all_dates_past(d("2026-11-12")));
        let t = parse("2026-11-10").unwrap();
        assert!(t.all_dates_past(d("2026-11-12")));
        let t = parse("2026-11-12,2026-11-13").unwrap();
        assert!(!t.all_dates_past(d("2026-11-12")));
    }
}
