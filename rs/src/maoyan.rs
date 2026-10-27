//! 猫眼接口客户端。
//!
//! 关键点：
//! - 移动端 `m.maoyan.com` 用 iPhone UA，返回 JSON
//! - PC `www.maoyan.com/films` 用桌面 UA，返回 HTML，需要正则 + cookie
//! - reqwest 关闭 SSL 校验以兼容猫眼站点
//! - 3 次重试，间隔 3s
//!

use std::sync::OnceLock;
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

/// 全进程共享的移动端 Client：连接复用 + gzip（单影院 JSON 92KB → ~6KB）。
static MOBILE_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
/// PC films 列表用的 Client（需要 cookie store）。
static PC_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn mobile_client() -> &'static reqwest::Client {
    MOBILE_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(USER_AGENT_MOBILE)
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(16)
            .tcp_keepalive(Duration::from_secs(30))
            .gzip(true)
            .danger_accept_invalid_certs(true)
            .build()
            .expect("reqwest mobile client")
    })
}

fn pc_client() -> &'static reqwest::Client {
    PC_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(USER_AGENT_PC)
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(8)
            .tcp_keepalive(Duration::from_secs(30))
            .gzip(true)
            .cookie_store(true)
            .danger_accept_invalid_certs(true)
            .build()
            .expect("reqwest pc client")
    })
}

async fn _get_json(url: &str, referer: Option<&str>, retries: u32) -> Result<Value> {
    let cli = mobile_client();
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

/// 猫眼风控（captcha / 406）会返回 `{code, msg, customData}` 而不是 `showData`。
/// 必须显式报错，否则监控会把「抓取失败」误当成「影院没有场次」并覆盖基线。
fn show_data(data: &Value) -> Result<&Value> {
    if let Some(show) = data.get("showData") {
        return Ok(show);
    }
    let code = data.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
    let msg = data.get("msg").and_then(|v| v.as_str()).unwrap_or("");
    Err(anyhow!("猫眼接口异常（code={}）：{}", code, msg))
}

pub async fn fetch_cinema_async(cinema_id: &str) -> Result<Value> {
    let url = API_TEMPLATE.replace("{cinema_id}", cinema_id);
    let referer = BUY_MOBILE.replace("{cinema_id}", cinema_id);
    let data = _get_json(&url, Some(&referer), 3).await?;
    let show = show_data(&data)?.clone();
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

/// 是否 IMAX 场次：影厅（th）或制式（tp）字段里带「IMAX」（大小写不敏感）。
/// 与 booker `--imax-only` 语义对齐：booker 读 `<span class="hall">` 判定，这里也基于影厅文本。
pub fn is_imax_show(s: &ShowInfo) -> bool {
    let hay = format!("{} {}", s.th, s.tp);
    hay.to_ascii_uppercase().contains("IMAX")
}

/// 过滤出 IMAX 场次（保序）。
pub fn imax_shows(shows: &[ShowInfo]) -> Vec<ShowInfo> {
    shows.iter().filter(|s| is_imax_show(s)).cloned().collect()
}

impl ShowInfo {
    /// 是否 IMAX 场次（见 [`is_imax_show`]）。
    pub fn is_imax(&self) -> bool {
        is_imax_show(self)
    }

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
            let get = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
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

pub fn find_movie<'a>(
    cinema_payload: &'a Value,
    movie_id: i64,
    keywords: &[&str],
) -> Option<&'a Value> {
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
    let cli = pc_client();
    let mut last_err: Option<String> = None;
    let re = Regex::new(r#"<a href="/films/(\d+)"[^>]*>([^<]+)</a>"#).unwrap();
    for i in 0..3u32 {
        let req = cli
            .get(&url)
            .header("Referer", "https://www.maoyan.com/")
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            );
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

/// 按关键词搜索电影。猫眼 `/ajax/search` 需要 `cityId`，否则会返回风控页。
pub async fn search_movies_async(keyword: &str, city_id: u32) -> Result<Vec<(String, String)>> {
    let keyword = keyword.trim();
    if keyword.is_empty() {
        return Ok(Vec::new());
    }
    let mut url = reqwest::Url::parse("https://m.maoyan.com/ajax/search")?;
    url.query_pairs_mut()
        .append_pair("kw", keyword)
        .append_pair("cityId", &city_id.to_string())
        .append_pair("stype", "-1");
    let data = _get_json(url.as_str(), Some("https://m.maoyan.com/"), 2).await?;
    let Some(list) = data
        .get("movies")
        .and_then(|v| v.get("list"))
        .and_then(|v| v.as_array())
    else {
        return Ok(Vec::new());
    };
    Ok(list
        .iter()
        .filter_map(|m| {
            Some((
                m.get("id")?.as_i64()?.to_string(),
                m.get("nm")?.as_str()?.trim().to_string(),
            ))
        })
        .collect())
}

/// 按 movieId 查电影详情（用于搜索一个不在榜单里的 ID）。
pub async fn fetch_movie_detail_async(movie_id: &str) -> Result<(String, String)> {
    let url = FILM_API.replace("{movie_id}", movie_id);
    let data = _get_json(&url, Some("https://m.maoyan.com/"), 2).await?;
    let dm = data
        .get("detailMovie")
        .ok_or_else(|| anyhow!("电影详情接口没有 detailMovie"))?;
    let id = dm
        .get("id")
        .and_then(|v| v.as_i64())
        .map(|v| v.to_string())
        .unwrap_or_else(|| movie_id.to_string());
    let name = dm
        .get("nm")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if name.is_empty() {
        return Err(anyhow!("电影详情缺少片名"));
    }
    Ok((id, name))
}

/// 按关键词搜索影院（猫眼 `stype=2`）。
pub async fn search_cinemas_async(
    keyword: &str,
    city_id: u32,
) -> Result<Vec<(String, String, String)>> {
    let keyword = keyword.trim();
    if keyword.is_empty() {
        return Ok(Vec::new());
    }
    let mut url = reqwest::Url::parse("https://m.maoyan.com/ajax/search")?;
    url.query_pairs_mut()
        .append_pair("kw", keyword)
        .append_pair("cityId", &city_id.to_string())
        .append_pair("stype", "2");
    let data = _get_json(url.as_str(), Some("https://m.maoyan.com/"), 2).await?;
    let Some(list) = data
        .get("cinemas")
        .and_then(|v| v.get("list"))
        .and_then(|v| v.as_array())
    else {
        return Ok(Vec::new());
    };
    Ok(list
        .iter()
        .filter_map(|c| {
            Some((
                c.get("id")?.as_i64()?.to_string(),
                c.get("nm")?.as_str()?.trim().to_string(),
                c.get("addr")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string(),
            ))
        })
        .collect())
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

    #[test]
    fn imax_detection_matches_hall_or_format_case_insensitive() {
        let imax_hall = ShowInfo {
            seq_no: "1".into(),
            dt: "2026-08-09".into(),
            tm: "15:40".into(),
            th: "IMAX 激光厅".into(),
            tp: "IMAX2D".into(),
        };
        assert!(imax_hall.is_imax());
        // 只有制式带 imax（小写）也算
        let fmt_only = ShowInfo {
            seq_no: "2".into(),
            dt: "2026-08-09".into(),
            tm: "19:00".into(),
            th: "1号激光厅".into(),
            tp: "imax2d".into(),
        };
        assert!(fmt_only.is_imax());
        // 普通厅不算
        let normal = ShowInfo {
            seq_no: "3".into(),
            dt: "2026-08-09".into(),
            tm: "20:00".into(),
            th: "5号厅".into(),
            tp: "2D".into(),
        };
        assert!(!normal.is_imax());
        // 空字段不算（别把空字符串误判）
        let empty = ShowInfo {
            seq_no: "4".into(),
            dt: "2026-08-09".into(),
            tm: "20:00".into(),
            th: "".into(),
            tp: "".into(),
        };
        assert!(!empty.is_imax());
    }

    #[test]
    fn imax_shows_filters_and_preserves_order() {
        let shows = movie_shows(&sample_movie());
        assert_eq!(shows.len(), 2);
        let filtered = imax_shows(&shows);
        assert_eq!(filtered.len(), 2);
        // 加一个普通厅再过滤
        let mut mixed = shows;
        mixed.push(ShowInfo {
            seq_no: "x".into(),
            dt: "2026-08-10".into(),
            tm: "10:00".into(),
            th: "4号厅".into(),
            tp: "2D".into(),
        });
        let filtered = imax_shows(&mixed);
        assert_eq!(filtered.len(), 2);
    }
}

#[cfg(test)]
mod error_payload_tests {
    use super::*;

    #[test]
    fn captcha_payload_is_an_error_not_empty_cinema() {
        let v = serde_json::json!({
            "code": 406,
            "msg": "您的网络好像不太给力，请稍后再试",
            "customData": {"riskLevel": "199"}
        });
        let err = show_data(&v).unwrap_err().to_string();
        assert!(err.contains("406"));
    }

    #[test]
    fn normal_payload_has_show_data() {
        let v = serde_json::json!({"showData": {"cinemaName": "x", "movies": []}});
        assert!(show_data(&v).is_ok());
    }
}
