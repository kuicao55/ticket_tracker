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
