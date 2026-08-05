//! 猫眼接口客户端 —— 与 py/.../maoyan.py 1:1 对齐。
//!
//! 关键点：
//! - 移动端 `m.maoyan.com` 用 iPhone UA，返回 JSON
//! - PC `www.maoyan.com/films` 用桌面 UA，返回 HTML，需要正则 + cookie
//! - reqwest 关掉 SSL 校验（与 Python `ssl.CERT_NONE` 同）
//! - 3 次重试，间隔 3s
//! 参考：RUST_PORT.md §5.2

use std::time::Duration;

use anyhow::{anyhow, Result};
use regex::Regex;
use serde_json::{json, Value};

const USER_AGENT_MOBILE: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 15_0 like Mac OS X) \
    AppleWebKit/605.1.15 (KHTML, like Gecko) Version/15.0 Mobile/15E148 Safari/604.1";
const USER_AGENT_PC: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120 Safari/537.36";

const API_TEMPLATE: &str = "https://m.maoyan.com/ajax/cinemaDetail?cinemaId={cinema_id}";
const FILM_API: &str = "https://m.maoyan.com/ajax/detailmovie?movieId={movie_id}";
const BUY_MOBILE: &str = "https://m.maoyan.com/shows/{cinema_id}";
const BUY_PC: &str = "https://www.maoyan.com/cinema/{cinema_id}";
const FILMS_LIST_URL: &str = "https://www.maoyan.com/films?showType={show_type}";

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(USER_AGENT_MOBILE)
        .timeout(Duration::from_secs(15))
        .danger_accept_invalid_certs(true)
        .build()
        .expect("reqwest client")
}

async fn _get_json(url: &str, referer: Option<&str>, retries: u32) -> Result<Value> {
    let cli = client();
    let mut last_err: Option<String> = None;
    for i in 0..retries {
        let mut req = cli.get(url);
        if let Some(r) = referer {
            req = req.header("Referer", r);
        }
        req = req.header("Accept", "application/json, text/plain, */*");
        match req.send().await {
            Ok(resp) => {
                if !resp.status().is_success() {
                    last_err = Some(format!("HTTP {}", resp.status()));
                } else {
                    match resp.json::<Value>().await {
                        Ok(v) => return Ok(v),
                        Err(e) => last_err = Some(format!("json decode: {}", e)),
                    }
                }
            }
            Err(e) => last_err = Some(format!("request: {}", e)),
        }
        if i + 1 < retries {
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }
    Err(anyhow!("猫眼接口请求失败 {}: {:?}", url, last_err))
}

// ----------------- 移动端 JSON -----------------

pub async fn fetch_cinema_async(cinema_id: &str) -> Result<Value> {
    let url = API_TEMPLATE.replace("{cinema_id}", cinema_id);
    let referer = BUY_MOBILE.replace("{cinema_id}", cinema_id);
    let data = _get_json(&url, Some(&referer), 3).await?;
    let show = data.get("showData").cloned().unwrap_or(Value::Null);
    let cinema_name = show
        .get("cinemaName")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("影城 {}", cinema_id));
    let movies = show
        .get("movies")
        .cloned()
        .unwrap_or_else(|| Value::Array(vec![]));
    Ok(json!({
        "cinema_id": cinema_id,
        "cinema_name": cinema_name,
        "movies": movies,
    }))
}

/// 同步包装：阻塞当前线程执行一次 fetch_cinema。
pub fn fetch_cinema(cinema_id: &str) -> Result<Value> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(fetch_cinema_async(cinema_id))
}

pub fn movie_dates(movie: &Value) -> Vec<String> {
    let mut ds = std::collections::BTreeSet::new();
    if let Some(shows) = movie.get("shows").and_then(|v| v.as_array()) {
        for s in shows {
            if let Some(plist) = s.get("plist").and_then(|v| v.as_array()) {
                for p in plist {
                    if let Some(dt) = p.get("dt").and_then(|v| v.as_str()) {
                        ds.insert(dt.to_string());
                    }
                }
            }
        }
    }
    ds.into_iter().collect()
}

/// 单个场次的明细。`seq_no` 是猫眼给每个场次的唯一编号（如 `202608090008709`），
/// 增场监控靠它做 diff。
#[derive(Debug, Clone)]
pub struct ShowInfo {
    pub seq_no: String,
    /// 日期 YYYY-MM-DD
    pub dt: String,
    /// 开场时间 HH:MM
    pub tm: String,
    /// 影厅，如「IMAX 激光厅」
    pub th: String,
    /// 制式，如「IMAX2D」
    pub tp: String,
}

impl ShowInfo {
    /// 通知里用的一行描述：`08-09 19:30 IMAX 激光厅 IMAX2D`
    pub fn label(&self) -> String {
        let date = self.dt.get(5..).unwrap_or(self.dt.as_str());
        let mut s = format!("{} {}", date, self.tm);
        if !self.th.is_empty() {
            s.push(' ');
            s.push_str(&self.th);
        }
        if !self.tp.is_empty() {
            s.push(' ');
            s.push_str(&self.tp);
        }
        s
    }
}

/// 摊平 `shows[].plist[]`，返回全部场次明细。没有 `seqNo` 的条目会被跳过
/// （无法参与 diff）。结果按 (日期, 时间) 排序。
pub fn movie_shows(movie: &Value) -> Vec<ShowInfo> {
    let mut out = Vec::new();
    let Some(shows) = movie.get("shows").and_then(|v| v.as_array()) else {
        return out;
    };
    for s in shows {
        let Some(plist) = s.get("plist").and_then(|v| v.as_array()) else {
            continue;
        };
        for p in plist {
            // seqNo 可能是字符串或数字，两种都收
            let seq_no = match p.get("seqNo") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Number(n)) => n.to_string(),
                _ => continue,
            };
            if seq_no.is_empty() {
                continue;
            }
            let get = |k: &str| {
                p.get(k)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            out.push(ShowInfo {
                seq_no,
                dt: get("dt"),
                tm: get("tm"),
                th: get("th"),
                tp: get("tp"),
            });
        }
    }
    out.sort_by(|a, b| (&a.dt, &a.tm).cmp(&(&b.dt, &b.tm)));
    out
}

pub fn find_movie<'a>(cinema_payload: &'a Value, movie_id: i64, keywords: &[&str]) -> Option<&'a Value> {
    let movies = cinema_payload.get("movies")?.as_array()?;
    // 精确 id 匹配
    for m in movies {
        if m.get("id").and_then(|v| v.as_i64()) == Some(movie_id) {
            return Some(m);
        }
    }
    // 关键词模糊
    for m in movies {
        let nm = m.get("nm").and_then(|v| v.as_str()).unwrap_or("");
        for kw in keywords {
            if !kw.is_empty() && nm.contains(kw) {
                return Some(m);
            }
        }
    }
    None
}

// ----------------- 影片详情 / 名字 -----------------

pub async fn fetch_movie_name_async(movie_id: i64) -> Result<Option<String>> {
    let url = FILM_API.replace("{movie_id}", &movie_id.to_string());
    match _get_json(&url, None, 3).await {
        Ok(data) => {
            let mv = data
                .get("detailMovie")
                .or_else(|| data.get("movie"))
                .unwrap_or(&data);
            Ok(mv.get("nm").and_then(|v| v.as_str()).map(String::from))
        }
        Err(_) => Ok(None),
    }
}

pub fn fetch_movie_name(movie_id: i64) -> Result<Option<String>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(fetch_movie_name_async(movie_id))
}

// ----------------- PC 端 films 列表 -----------------

pub async fn fetch_films_list_async(show_type: u8) -> Result<Vec<(String, String)>> {
    let url = FILMS_LIST_URL.replace("{show_type}", &show_type.to_string());
    let cli = reqwest::Client::builder()
        .user_agent(USER_AGENT_PC)
        .timeout(Duration::from_secs(15))
        .danger_accept_invalid_certs(true)
        .cookie_store(true)
        .build()?;
    let mut last_err: Option<String> = None;
    for i in 0..3u32 {
        let req = cli
            .get(&url)
            .header("Referer", "https://www.maoyan.com/")
            .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8");
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                last_err = Some(format!("send: {}", e));
                if i + 1 < 3 {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
                continue;
            }
        };
        if !resp.status().is_success() {
            last_err = Some(format!("HTTP {}", resp.status()));
            if i + 1 < 3 {
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            continue;
        }
        let body = resp.text().await?;
        let re = Regex::new(r#"<a href="/films/(\d+)"[^>]*>([^<]+)</a>"#).unwrap();
        let mut seen = std::collections::BTreeMap::<String, String>::new();
        for cap in re.captures_iter(&body) {
            let mid = cap[1].to_string();
            let name = cap[2].trim().to_string();
            seen.entry(mid).or_insert(name);
        }
        return Ok(seen.into_iter().collect());
    }
    Err(anyhow!("films 列表抓取失败: {:?}", last_err))
}

pub fn fetch_films_list(show_type: u8) -> Result<Vec<(String, String)>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(fetch_films_list_async(show_type))
}

// ----------------- 工具 -----------------

#[allow(dead_code)]
pub fn buy_pc_url(cinema_id: &str) -> String {
    BUY_PC.replace("{cinema_id}", cinema_id)
}

pub fn buy_pc_url_owned(cinema_id: &str) -> String {
    buy_pc_url(cinema_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取自 `cinemaDetail?cinemaId=37534` 的真实响应片段（奥德赛）。
    fn sample_movie() -> Value {
        json!({
            "id": 1545360,
            "nm": "奥德赛",
            "showCount": 3,
            "shows": [
                { "showDate": "2026-08-08", "plist": [
                    { "seqNo": "202608080026845", "dt": "2026-08-08", "tm": "15:40",
                      "th": "IMAX 激光厅", "tp": "IMAX2D" }
                ]},
                { "showDate": "2026-08-09", "plist": [
                    { "seqNo": "202608090008709", "dt": "2026-08-09", "tm": "15:40",
                      "th": "IMAX 激光厅", "tp": "IMAX2D" },
                    // 没有 seqNo 的条目无法参与 diff，必须跳过
                    { "dt": "2026-08-09", "tm": "20:00" }
                ]}
            ]
        })
    }

    #[test]
    fn movie_shows_extracts_seq_no_and_skips_entries_without_it() {
        let shows = movie_shows(&sample_movie());
        let seqs: Vec<&str> = shows.iter().map(|s| s.seq_no.as_str()).collect();
        assert_eq!(seqs, vec!["202608080026845", "202608090008709"]);
    }

    #[test]
    fn movie_shows_sorts_by_date_then_time() {
        let m = json!({ "shows": [{ "plist": [
            { "seqNo": "b", "dt": "2026-08-09", "tm": "22:15" },
            { "seqNo": "c", "dt": "2026-08-10", "tm": "09:00" },
            { "seqNo": "a", "dt": "2026-08-09", "tm": "19:30" },
        ]}]});
        let shows = movie_shows(&m);
        let seqs: Vec<&str> = shows.iter().map(|s| s.seq_no.as_str()).collect();
        assert_eq!(seqs, vec!["a", "b", "c"]);
    }

    #[test]
    fn label_is_human_readable() {
        let shows = movie_shows(&sample_movie());
        assert_eq!(shows[1].label(), "08-09 15:40 IMAX 激光厅 IMAX2D");
    }

    #[test]
    fn movie_shows_on_movie_without_shows_is_empty() {
        assert!(movie_shows(&json!({ "nm": "x" })).is_empty());
    }
}

