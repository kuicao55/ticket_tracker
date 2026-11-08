//! 全城开售 watch 的纯逻辑：解析猫眼城市接口、选最早场次。
//!
//! 这里不碰网络和数据库，方便单测。HTTP 请求在 [`crate::maoyan`]，
//! 监控状态机在 [`crate::server::monitor`]。

use std::collections::HashSet;
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use regex::Regex;
use serde_json::Value;

/// 一个城市接口返回的场次。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CityShow {
    pub cinema_id: String,
    pub cinema_name: String,
    /// YYYY-MM-DD（来自请求的 showDate）
    pub date: String,
    /// 规范化 HH:MM
    pub show_time: String,
    /// 影厅名；城市列表接口通常为空，IMAX cellShows 也可能没有
    pub hall: String,
    /// 制式 / 语言，如 `IMAX2D`、`英语 2D`
    pub format: String,
    /// 猫眼场次唯一号；`showTimes` 摘要里没有
    pub seq_no: Option<String>,
    pub is_imax: bool,
}

impl CityShow {
    pub fn hour(&self) -> Option<u8> {
        hour_of(&self.show_time)
    }

    pub(crate) fn sort_key(&self) -> (&str, &str, &str, &str) {
        (
            self.date.as_str(),
            self.show_time.as_str(),
            self.cinema_id.as_str(),
            self.seq_no.as_deref().unwrap_or(""),
        )
    }
}

/// 一个城市 × 影片 × 日期范围的轮询结果。
#[derive(Debug, Clone, Default)]
pub struct CityPoll {
    /// showdays 返回的可预售日期（升序）。
    pub dates: Vec<String>,
    /// 最早一个非空日期的场次。
    pub shows: Vec<CityShow>,
    /// 本轮成功发出的基础 JSON 请求数（showdays 不计入这里）。
    pub requests: usize,
}

/// 单个 showDate 的抓取结果 + 实际请求页数。
#[derive(Debug, Clone, Default)]
pub struct CityDateFetch {
    pub shows: Vec<CityShow>,
    pub requests: usize,
}

fn time_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(\d{1,2}):(\d{2})").expect("time regex"))
}

/// 解析 `近期场次: 9:05 | 15:40`，返回升序、去重、规范化的 `HH:MM`。
pub fn parse_show_times(s: &str) -> Vec<String> {
    let mut out: Vec<String> = time_re()
        .captures_iter(s)
        .filter_map(|c| {
            let h: u8 = c.get(1)?.as_str().parse().ok()?;
            let m: u8 = c.get(2)?.as_str().parse().ok()?;
            if h > 23 || m > 59 {
                return None;
            }
            Some(format!("{:02}:{:02}", h, m))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// 把 `9:05` / `09:05` / `09:05:00` 规范成 `09:05`。
pub fn normalize_time(s: &str) -> Option<String> {
    let mut parts = s.trim().split(':');
    let h: u8 = parts.next()?.trim().parse().ok()?;
    let m: u8 = parts.next()?.trim().parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some(format!("{:02}:{:02}", h, m))
}

/// 解析 showdays 响应。
///
/// `code=109`（未获取到该城市下影片的上映日期）是“还没开售”的正常状态，
/// 返回空 Vec；其他非 0 code 才是错误。
pub fn parse_show_dates(v: &Value) -> Result<Vec<String>> {
    let code = v.get("code").and_then(Value::as_i64).unwrap_or(0);
    if code == 109 {
        return Ok(Vec::new());
    }
    if code != 0 {
        let msg = v
            .get("errMsg")
            .or_else(|| v.get("msg"))
            .and_then(Value::as_str)
            .unwrap_or("");
        return Err(anyhow!("猫眼日期接口异常（code={}）：{}", code, msg));
    }
    let Some(dates) = v
        .get("data")
        .and_then(|d| d.get("dates"))
        .and_then(Value::as_array)
    else {
        return Ok(Vec::new());
    };
    let mut out: Vec<String> = dates
        .iter()
        .filter_map(|d| d.get("date").and_then(Value::as_str))
        .filter(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").is_ok())
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    Ok(out)
}

/// 只保留今天及以后的日期。
pub fn upcoming_dates(dates: Vec<String>, today: NaiveDate) -> Vec<String> {
    dates
        .into_iter()
        .filter(|d| {
            NaiveDate::parse_from_str(d, "%Y-%m-%d")
                .map(|parsed| parsed >= today)
                .unwrap_or(false)
        })
        .collect()
}

/// 解析某个 showDate 的城市影院列表。
///
/// - `force_imax=true` 时把返回场次都视为 IMAX（请求已带 IMAX 筛选）。
/// - 优先使用 `cellShows`（含精确时间/seqNo）；否则回退到 `showTimes` 摘要。
/// - 非 0 code 必须报错，不能当成“没有场次”覆盖状态。
pub fn parse_city_date(date: &str, v: &Value, force_imax: bool) -> Result<Vec<CityShow>> {
    let code = v.get("code").and_then(Value::as_i64).unwrap_or(0);
    if code != 0 {
        let msg = v
            .get("errMsg")
            .or_else(|| v.get("msg"))
            .and_then(Value::as_str)
            .unwrap_or("");
        return Err(anyhow!("猫眼城市影院接口异常（code={}）：{}", code, msg));
    }
    if NaiveDate::parse_from_str(date, "%Y-%m-%d").is_err() {
        return Err(anyhow!("非法日期: {}", date));
    }
    let Some(cinemas) = v
        .get("data")
        .and_then(|d| d.get("cinemas"))
        .and_then(Value::as_array)
    else {
        return Ok(Vec::new());
    };

    let mut out = Vec::new();
    for c in cinemas {
        let Some(cinema_id) = value_id(c.get("id")) else {
            continue;
        };
        let cinema_name = c
            .get("name")
            .or_else(|| c.get("nm"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();

        let mut seen: HashSet<(String, String, String)> = HashSet::new();
        if let Some(cells) = c.get("cellShows").and_then(Value::as_array) {
            for cell in cells {
                let Some(show_time) = cell
                    .get("showTime")
                    .or_else(|| cell.get("tm"))
                    .and_then(Value::as_str)
                    .and_then(normalize_time)
                else {
                    continue;
                };
                let format = cell
                    .get("langAndType")
                    .or_else(|| cell.get("tp"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let hall = cell
                    .get("hallName")
                    .or_else(|| cell.get("th"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let seq_no = value_id(cell.get("seqNo"));
                let is_imax = force_imax
                    || format!("{} {}", hall, format)
                        .to_ascii_uppercase()
                        .contains("IMAX");
                let key = (
                    show_time.clone(),
                    seq_no.clone().unwrap_or_default(),
                    format.clone(),
                );
                if !seen.insert(key) {
                    continue;
                }
                out.push(CityShow {
                    cinema_id: cinema_id.clone(),
                    cinema_name: cinema_name.clone(),
                    date: date.to_string(),
                    show_time,
                    hall,
                    format,
                    seq_no,
                    is_imax,
                });
            }
        }

        if !out
            .iter()
            .any(|s| s.cinema_id == cinema_id && s.date == date)
        {
            if let Some(times) = c.get("showTimes").and_then(Value::as_str) {
                for show_time in parse_show_times(times) {
                    let key = (show_time.clone(), String::new(), String::new());
                    if !seen.insert(key) {
                        continue;
                    }
                    out.push(CityShow {
                        cinema_id: cinema_id.clone(),
                        cinema_name: cinema_name.clone(),
                        date: date.to_string(),
                        show_time,
                        hall: String::new(),
                        format: String::new(),
                        seq_no: None,
                        is_imax: force_imax,
                    });
                }
            }
        }
    }

    out.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
    Ok(out)
}

fn value_id(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// 选全局最早场次：先比日期，再比时间；同场次用影院/seqNo 稳定排序。
pub fn choose_earliest(shows: &[CityShow]) -> Option<&CityShow> {
    shows.iter().min_by(|a, b| a.sort_key().cmp(&b.sort_key()))
}

/// `(场次总数, IMAX 场次数, 不同 (日期, 小时) 桶数)`。
pub fn summarize(shows: &[CityShow]) -> (usize, usize, usize) {
    let imax = shows.iter().filter(|s| s.is_imax).count();
    let buckets: HashSet<(&str, u8)> = shows
        .iter()
        .filter_map(|s| s.hour().map(|h| (s.date.as_str(), h)))
        .collect();
    (shows.len(), imax, buckets.len())
}

pub fn hour_of(show_time: &str) -> Option<u8> {
    show_time.split(':').next()?.parse::<u8>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn showdays_109_is_not_an_error() {
        let v = json!({
            "code": 109,
            "data": null,
            "errMsg": "未获取到该城市下影片的上映日期",
            "success": false
        });
        assert!(parse_show_dates(&v).unwrap().is_empty());
    }

    #[test]
    fn showdays_parses_sorts_and_dedupes() {
        let v = json!({
            "code": 0,
            "data": {"dates": [
                {"date": "2026-12-31"},
                {"date": "2026-12-30"},
                {"date": "2026-12-31"},
                {"date": "bad"}
            ]}
        });
        assert_eq!(
            parse_show_dates(&v).unwrap(),
            vec!["2026-12-30".to_string(), "2026-12-31".to_string()]
        );
    }

    #[test]
    fn showdays_other_code_is_error() {
        let v = json!({"code": 406, "errMsg": "风控"});
        assert!(parse_show_dates(&v).is_err());
    }

    #[test]
    fn upcoming_dates_drops_past() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 8).unwrap();
        let got = upcoming_dates(
            vec![
                "2026-11-07".into(),
                "2026-11-08".into(),
                "2026-11-09".into(),
            ],
            today,
        );
        assert_eq!(
            got,
            vec!["2026-11-08".to_string(), "2026-11-09".to_string()]
        );
    }

    #[test]
    fn parses_show_times_from_summary() {
        assert_eq!(
            parse_show_times("近期场次: 9:05 | 15:40 | 09:05"),
            vec!["09:05".to_string(), "15:40".to_string()]
        );
        assert!(parse_show_times("暂无场次").is_empty());
        assert!(parse_show_times("24:00 | 10:60").is_empty());
    }

    #[test]
    fn parses_cell_shows_for_imax() {
        let v = json!({
            "code": 0,
            "data": {"cinemas": [{
                "id": 37534,
                "name": "MOViE MOViE",
                "cellShows": [
                    {"showTime": "10:00", "seqNo": "202612310012345", "langAndType": "英语 2D"},
                    {"showTime": "15:40", "seqNo": 202612310012346u64, "langAndType": "IMAX2D"}
                ]
            }]}
        });
        let shows = parse_city_date("2026-12-31", &v, true).unwrap();
        assert_eq!(shows.len(), 2);
        assert_eq!(shows[0].show_time, "10:00");
        assert_eq!(shows[0].seq_no.as_deref(), Some("202612310012345"));
        assert!(shows.iter().all(|s| s.is_imax));
    }

    #[test]
    fn falls_back_to_show_times() {
        let v = json!({
            "code": 0,
            "data": {"cinemas": [{
                "id": "42",
                "nm": "测试影院",
                "showTimes": "近期场次: 19:10 | 10:40",
                "cellShows": []
            }]}
        });
        let shows = parse_city_date("2026-11-08", &v, false).unwrap();
        assert_eq!(
            shows
                .iter()
                .map(|s| s.show_time.as_str())
                .collect::<Vec<_>>(),
            vec!["10:40", "19:10"]
        );
        assert!(shows.iter().all(|s| s.seq_no.is_none() && !s.is_imax));
        assert_eq!(shows[0].cinema_name, "测试影院");
    }

    #[test]
    fn city_date_nonzero_code_is_error() {
        let v = json!({"code": 406, "data": {"cinemas": []}, "errMsg": "风控"});
        assert!(parse_city_date("2026-11-08", &v, false).is_err());
    }

    #[test]
    fn earliest_is_date_first_then_time() {
        let shows = vec![
            CityShow {
                cinema_id: "1".into(),
                cinema_name: "A".into(),
                date: "2026-11-09".into(),
                show_time: "00:05".into(),
                hall: String::new(),
                format: String::new(),
                seq_no: None,
                is_imax: false,
            },
            CityShow {
                cinema_id: "2".into(),
                cinema_name: "B".into(),
                date: "2026-11-08".into(),
                show_time: "23:59".into(),
                hall: String::new(),
                format: String::new(),
                seq_no: None,
                is_imax: false,
            },
        ];
        assert_eq!(choose_earliest(&shows).unwrap().cinema_id, "2");
        assert_eq!(summarize(&shows), (2, 0, 2));
    }
}
