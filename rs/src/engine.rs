//! 小时桶计数引擎。
//!
//! 设计要点：
//! - poll 以影院为单位抓取；每部电影预计算 `(date, hour) -> count(total, imax)`；
//! - watch 只看自己时间规则覆盖的小时桶；
//! - watch 按 `(watch, cinema)` 存上次的桶计数 + 桶内场次小列表；
//! - 只比较计数：任一桶计数增加 -> 报警；减少/不变 -> 不报警但更新基线；
//! - 同一小时换场（计数不变）不报警，但小列表要静默更新；
//! - 不依赖 `seqNo`。

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use serde_json::{json, Value};

use crate::maoyan;
use crate::time_spec::{bucket_key, bucket_key_str, date_id, parse_bucket_key, TimeMask, TimeSpec};

// ----------------- 场次 / 桶 -----------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShowMini {
    pub tm: String,
    pub th: String,
    pub tp: String,
    pub is_imax: bool,
}

impl ShowMini {
    fn sort_key(&self) -> (&str, &str, &str, bool) {
        (&self.tm, &self.th, &self.tp, self.is_imax)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "tm": self.tm,
            "th": self.th,
            "tp": self.tp,
            "is_imax": self.is_imax,
        })
    }

    fn from_json(v: &Value) -> Option<Self> {
        Some(Self {
            tm: v.get("tm")?.as_str()?.to_string(),
            th: v
                .get("th")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            tp: v
                .get("tp")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            is_imax: v.get("is_imax").and_then(|x| x.as_bool()).unwrap_or(false),
        })
    }

    pub fn label(&self) -> String {
        let mut s = self.tm.clone();
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

#[derive(Debug, Clone)]
pub struct BucketCount {
    pub key: u32,
    pub total: u16,
    pub imax: u16,
    /// 已按 (tm, th, tp, is_imax) 排序
    pub total_shows: Vec<ShowMini>,
    pub imax_shows: Vec<ShowMini>,
}

impl BucketCount {
    pub fn shows(&self, imax_only: bool) -> &[ShowMini] {
        if imax_only {
            &self.imax_shows
        } else {
            &self.total_shows
        }
    }

    pub fn count(&self, imax_only: bool) -> u16 {
        if imax_only {
            self.imax
        } else {
            self.total
        }
    }
}

#[derive(Debug, Clone)]
pub struct MovieSnapshot {
    pub id: i64,
    pub name: String,
    pub buckets: Vec<BucketCount>,
}

#[derive(Debug, Clone)]
pub struct CinemaSnapshot {
    pub cinema_id: String,
    pub cinema_name: String,
    pub movies: Vec<MovieSnapshot>,
    movie_index: HashMap<i64, usize>,
}

impl CinemaSnapshot {
    pub fn from_payload(cinema_id: &str, payload: &Value) -> Self {
        let cinema_name = payload
            .get("cinema_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let mut movies = Vec::new();
        if let Some(arr) = payload.get("movies").and_then(|v| v.as_array()) {
            for m in arr {
                let id = m.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                let name = m
                    .get("nm")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut by_bucket: BTreeMap<u32, BucketBuild> = BTreeMap::new();
                if let Some(shows) = m.get("shows").and_then(|v| v.as_array()) {
                    for s in shows {
                        let Some(plist) = s.get("plist").and_then(|v| v.as_array()) else {
                            continue;
                        };
                        for p in plist {
                            let Some((dt, hour, tm)) = parse_show_time(p) else {
                                continue;
                            };
                            let th = p
                                .get("th")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let tp = p
                                .get("tp")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let is_imax = format!("{} {}", th, tp)
                                .to_ascii_uppercase()
                                .contains("IMAX");
                            let mini = ShowMini {
                                tm,
                                th,
                                tp,
                                is_imax,
                            };
                            let key = bucket_key(dt, hour);
                            let e = by_bucket.entry(key).or_default();
                            e.total += 1;
                            e.total_shows.push(mini.clone());
                            if is_imax {
                                e.imax += 1;
                                e.imax_shows.push(mini);
                            }
                        }
                    }
                }
                let mut buckets: Vec<BucketCount> = by_bucket
                    .into_iter()
                    .map(|(key, mut b)| {
                        b.total_shows
                            .sort_by(|a, c| a.sort_key().cmp(&c.sort_key()));
                        b.imax_shows.sort_by(|a, c| a.sort_key().cmp(&c.sort_key()));
                        BucketCount {
                            key,
                            total: b.total,
                            imax: b.imax,
                            total_shows: b.total_shows,
                            imax_shows: b.imax_shows,
                        }
                    })
                    .collect();
                buckets.sort_by_key(|b| b.key);
                movies.push(MovieSnapshot { id, name, buckets });
            }
        }
        let movie_index = movies.iter().enumerate().map(|(i, m)| (m.id, i)).collect();
        Self {
            cinema_id: cinema_id.to_string(),
            cinema_name,
            movies,
            movie_index,
        }
    }

    pub fn movie(&self, movie_id: i64) -> Option<&MovieSnapshot> {
        self.movie_index.get(&movie_id).map(|&i| &self.movies[i])
    }
}

#[derive(Default)]
struct BucketBuild {
    total: u16,
    imax: u16,
    total_shows: Vec<ShowMini>,
    imax_shows: Vec<ShowMini>,
}

fn parse_show_time(p: &Value) -> Option<(NaiveDate, u8, String)> {
    let ds = p.get("dt").and_then(|v| v.as_str())?;
    let ts = p.get("tm").and_then(|v| v.as_str())?;
    let dt = NaiveDate::parse_from_str(ds, "%Y-%m-%d").ok()?;
    let (h, m) = ts.split_once(':')?;
    let h: u8 = h.parse().ok()?;
    let m: u8 = m.parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some((dt, h, ts.to_string()))
}

// ----------------- 基线状态 -----------------

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BucketState {
    pub count: u16,
    /// 已排序
    pub shows: Vec<ShowMini>,
}

impl BucketState {
    fn to_json(&self, key: u32) -> Value {
        json!({
            "key": bucket_key_to_json(key),
            "count": self.count,
            "shows": self.shows.iter().map(|s| s.to_json()).collect::<Vec<_>>(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WatchCinemaState {
    pub initialized: bool,
    /// 只保存 count > 0 的桶
    pub buckets: HashMap<u32, BucketState>,
}

impl WatchCinemaState {
    pub fn from_json(v: &Value) -> Self {
        if v.is_null() || !v.is_object() {
            return Self::default();
        }
        let initialized = v
            .get("initialized")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let mut buckets = HashMap::new();
        if let Some(arr) = v.get("buckets").and_then(|x| x.as_array()) {
            for item in arr {
                let Some(key_str) = item.get("key").and_then(|x| x.as_str()) else {
                    continue;
                };
                let Ok((date, hour)) = parse_bucket_key(key_str) else {
                    continue;
                };
                let key = bucket_key(date, hour);
                let shows = item
                    .get("shows")
                    .and_then(|x| x.as_array())
                    .map(|a| a.iter().filter_map(ShowMini::from_json).collect::<Vec<_>>())
                    .unwrap_or_default();
                let count = item
                    .get("count")
                    .and_then(|x| x.as_u64())
                    .map(|n| n.min(u16::MAX as u64) as u16)
                    .unwrap_or(shows.len() as u16);
                if count > 0 {
                    buckets.insert(key, BucketState { count, shows });
                }
            }
        }
        Self {
            initialized,
            buckets,
        }
    }

    pub fn to_json(&self) -> Value {
        let mut keys: Vec<u32> = self.buckets.keys().copied().collect();
        keys.sort_unstable();
        json!({
            "initialized": self.initialized,
            "buckets": keys
                .into_iter()
                .filter_map(|k| self.buckets.get(&k).map(|s| s.to_json(k)))
                .collect::<Vec<_>>(),
        })
    }
}

fn bucket_key_to_json(key: u32) -> String {
    let id = key / 24;
    let hour = (key % 24) as u8;
    match crate::time_spec::date_from_id(id) {
        Some(d) => bucket_key_str(d, hour),
        None => format!("{} {}", id, hour),
    }
}

// ----------------- 编译后的 watch -----------------

#[derive(Debug, Clone)]
pub struct CompiledWatch {
    pub watch_id: String,
    pub movie_id: i64,
    pub mask: TimeMask,
    pub imax_only: bool,
}

pub fn compile_watch(watch: &Value) -> Result<CompiledWatch> {
    let watch_id = watch
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("watch 缺少 id"))?
        .to_string();
    let movie_id = watch.get("movie_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let time_spec = watch
        .get("time_spec")
        .map(TimeSpec::from_json)
        .transpose()?
        .unwrap_or(TimeSpec::All);
    let mask = time_spec.compile_mask()?;
    let imax_only = watch
        .get("imax_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Ok(CompiledWatch {
        watch_id,
        movie_id,
        mask,
        imax_only,
    })
}

// ----------------- PollPlan -----------------

#[derive(Debug, Clone, Default)]
pub struct PollNode {
    pub cinema_id: String,
    pub watches: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PollPlan {
    pub polls: HashMap<String, PollNode>,
    pub watch_cinemas: HashMap<String, Vec<String>>,
}

impl PollPlan {
    pub fn cinemas(&self) -> Vec<String> {
        let mut v: Vec<String> = self.polls.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn watches_for(&self, cinema_id: &str) -> &[String] {
        self.polls
            .get(cinema_id)
            .map(|p| p.watches.as_slice())
            .unwrap_or(&[])
    }
}

/// 从 watch 列表构建 poll 订阅图。输入应已过滤为 enabled 且至少有一个订阅者。
pub fn build_poll_plan(watches: &[Value]) -> PollPlan {
    let mut plan = PollPlan::default();
    for w in watches {
        let Some(wid) = w.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let mut seen = std::collections::HashSet::new();
        let cinemas: Vec<String> = w
            .get("cinemas")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .filter(|c| seen.insert(c.clone()))
                    .collect()
            })
            .unwrap_or_default();
        for cid in &cinemas {
            plan.polls
                .entry(cid.clone())
                .or_insert_with(|| PollNode {
                    cinema_id: cid.clone(),
                    watches: Vec::new(),
                })
                .watches
                .push(wid.to_string());
        }
        plan.watch_cinemas.insert(wid.to_string(), cinemas);
    }
    for p in plan.polls.values_mut() {
        p.watches.sort();
        p.watches.dedup();
    }
    plan
}

// ----------------- 抓取 -----------------

#[derive(Debug, Default)]
pub struct FetchOutcome {
    pub snapshots: HashMap<String, Arc<CinemaSnapshot>>,
    pub errors: HashMap<String, String>,
}

pub async fn fetch_many<F, Fut>(
    cinemas: Vec<String>,
    concurrency: usize,
    fetch_one: F,
) -> FetchOutcome
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = (String, Result<Value>)> + Send + 'static,
{
    use std::sync::Arc as StdArc;
    use tokio::sync::Semaphore;
    use tokio::task::JoinSet;

    let mut outcome = FetchOutcome::default();
    if cinemas.is_empty() {
        return outcome;
    }
    let sem = StdArc::new(Semaphore::new(concurrency.clamp(1, 64)));
    let fetcher = StdArc::new(fetch_one);
    let mut set = JoinSet::new();
    for cid in cinemas {
        let sem = StdArc::clone(&sem);
        let fetcher = StdArc::clone(&fetcher);
        set.spawn(async move {
            let permit = sem.acquire_owned().await.expect("semaphore closed");
            tracing::debug!("抓取影院 {}", cid);
            let (cid, res) = fetcher(cid).await;
            drop(permit);
            (cid, res)
        });
    }
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((cid, Ok(payload))) => {
                let snap = CinemaSnapshot::from_payload(&cid, &payload);
                outcome.snapshots.insert(cid, Arc::new(snap));
            }
            Ok((cid, Err(e))) => {
                outcome.errors.insert(cid, e.to_string());
            }
            Err(e) => {
                outcome
                    .errors
                    .insert("?".into(), format!("抓取任务失败: {}", e));
            }
        }
    }
    outcome
}

pub async fn fetch_plan_live(plan: &PollPlan, concurrency: usize) -> FetchOutcome {
    let cinemas = plan.cinemas();
    fetch_many(cinemas, concurrency, |cid| async move {
        let res = maoyan::fetch_cinema_async(&cid).await;
        (cid, res)
    })
    .await
}

// ----------------- 评估 -----------------

#[derive(Debug, Clone)]
pub struct BucketDelta {
    pub key: u32,
    pub date: String,
    pub hour: u8,
    pub added: Vec<ShowMini>,
}

#[derive(Debug, Clone, Default)]
pub struct CinemaOutcome {
    pub initialized: bool,
    pub added: Vec<BucketDelta>,
    pub state_changed: bool,
    pub movie_present: bool,
}

impl CinemaOutcome {
    pub fn triggered(&self) -> bool {
        !self.added.is_empty()
    }

    pub fn added_count(&self) -> usize {
        self.added.iter().map(|d| d.added.len()).sum()
    }
}

/// 评估一个 watch 在一个影院上的变化，并返回新的影院状态 JSON。
pub fn evaluate_cinema(
    compiled: &CompiledWatch,
    snapshot: &CinemaSnapshot,
    previous_state: &Value,
) -> (Value, CinemaOutcome) {
    let mut state = WatchCinemaState::from_json(previous_state);
    let movie = snapshot.movie(compiled.movie_id);
    let mut current: HashMap<u32, BucketState> = HashMap::new();

    if let Some(movie) = movie {
        match &compiled.mask {
            TimeMask::All => {
                for b in &movie.buckets {
                    let count = b.count(compiled.imax_only);
                    if count > 0 {
                        current.insert(
                            b.key,
                            BucketState {
                                count,
                                shows: b.shows(compiled.imax_only).to_vec(),
                            },
                        );
                    }
                }
            }
            TimeMask::DailyHours(hours) => {
                let set: std::collections::HashSet<u8> = hours.iter().copied().collect();
                for b in &movie.buckets {
                    let hour = (b.key % 24) as u8;
                    if !set.contains(&hour) {
                        continue;
                    }
                    let count = b.count(compiled.imax_only);
                    if count > 0 {
                        current.insert(
                            b.key,
                            BucketState {
                                count,
                                shows: b.shows(compiled.imax_only).to_vec(),
                            },
                        );
                    }
                }
            }
            TimeMask::Intervals(keys) => {
                let by_key: HashMap<u32, &BucketCount> =
                    movie.buckets.iter().map(|b| (b.key, b)).collect();
                for key in keys {
                    let Some(b) = by_key.get(key) else {
                        continue;
                    };
                    let count = b.count(compiled.imax_only);
                    if count > 0 {
                        current.insert(
                            *key,
                            BucketState {
                                count,
                                shows: b.shows(compiled.imax_only).to_vec(),
                            },
                        );
                    }
                }
            }
        }
    }

    let was_initialized = state.initialized;
    let mut added = Vec::new();
    if was_initialized {
        for (key, cur) in &current {
            let old = state.buckets.get(key);
            let old_count = old.map(|s| s.count).unwrap_or(0);
            if cur.count > old_count {
                let prev_shows = old.map(|s| s.shows.as_slice()).unwrap_or(&[]);
                let added_shows = multiset_added(&cur.shows, prev_shows);
                if !added_shows.is_empty() {
                    added.push(BucketDelta {
                        key: *key,
                        date: crate::time_spec::date_from_id(*key / 24)
                            .map(|d| d.format("%Y-%m-%d").to_string())
                            .unwrap_or_default(),
                        hour: (*key % 24) as u8,
                        added: added_shows,
                    });
                }
            }
        }
        added.sort_by_key(|d| d.key);
    }

    let state_changed = !was_initialized || state.buckets != current;
    state.initialized = true;
    state.buckets = current;
    let new_state = state.to_json();

    (
        new_state,
        CinemaOutcome {
            initialized: !was_initialized,
            added,
            state_changed,
            movie_present: movie.is_some(),
        },
    )
}

/// 多重集合差：current - previous。
fn multiset_added(current: &[ShowMini], previous: &[ShowMini]) -> Vec<ShowMini> {
    if previous.is_empty() {
        return current.to_vec();
    }
    let mut counts: HashMap<(&str, &str, &str, bool), u16> = HashMap::new();
    for s in previous {
        *counts.entry(s.sort_key()).or_insert(0) += 1;
    }
    let mut out = Vec::new();
    for s in current {
        let key = s.sort_key();
        match counts.get_mut(&key) {
            Some(n) if *n > 0 => {
                *n -= 1;
            }
            _ => out.push(s.clone()),
        }
    }
    out
}

// ----------------- 工具 -----------------

pub fn bucket_summary(delta: &BucketDelta) -> String {
    let shows = delta
        .added
        .iter()
        .map(|s| s.label())
        .collect::<Vec<_>>()
        .join("、");
    format!(
        "{} {:02}:00 新增 {} 场：{}",
        delta.date,
        delta.hour,
        delta.added.len(),
        shows
    )
}

pub fn date_id_for_key(key: u32) -> u32 {
    date_id(
        crate::time_spec::date_from_id(key / 24)
            .unwrap_or_else(|| NaiveDate::from_ymd_opt(1970, 1, 1).expect("valid date")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time_spec::TimeSpec;
    use serde_json::json;

    fn payload(plist: &Value) -> Value {
        json!({
            "cinema_id": "37534",
            "cinema_name": "测试影院",
            "movies": [{
                "id": 100,
                "nm": "测试电影",
                "showCount": 0,
                "shows": [{ "showDate": "2026-11-12", "plist": plist }],
            }]
        })
    }

    fn show(seq: &str, dt: &str, tm: &str, th: &str, tp: &str) -> Value {
        json!({
            "seqNo": seq,
            "dt": dt,
            "tm": tm,
            "th": th,
            "tp": tp,
        })
    }

    fn compiled(watch: &Value) -> CompiledWatch {
        compile_watch(watch).unwrap()
    }

    fn watch(time_spec: Value, imax_only: bool) -> Value {
        json!({
            "id": "w1",
            "movie_id": 100,
            "time_spec": time_spec,
            "imax_only": imax_only,
        })
    }

    #[test]
    fn builds_hour_buckets_with_hall_and_imax() {
        let p = payload(&json!([
            show("a", "2026-11-12", "18:10", "5号厅", "2D"),
            show("b", "2026-11-12", "18:40", "IMAX 激光厅", "IMAX2D"),
            show("c", "2026-11-12", "19:00", "2号厅", "2D"),
            show("d", "2026-11-13", "18:20", "4号厅", "2D"),
        ]));
        let snap = CinemaSnapshot::from_payload("37534", &p);
        let m = snap.movie(100).unwrap();
        assert_eq!(m.buckets.len(), 3); // 11-12 18, 11-12 19, 11-13 18
        let b = &m.buckets[0];
        assert_eq!(b.total, 2);
        assert_eq!(b.imax, 1);
        assert_eq!(b.total_shows.len(), 2);
        assert_eq!(b.imax_shows.len(), 1);
        assert_eq!(b.imax_shows[0].th, "IMAX 激光厅");
    }

    #[test]
    fn first_run_initializes_without_alert() {
        let p = payload(&json!([show("a", "2026-11-12", "18:10", "5号厅", "2D")]));
        let snap = CinemaSnapshot::from_payload("37534", &p);
        let w = watch(TimeSpec::All.to_json(), false);
        let (state, out) = evaluate_cinema(&compiled(&w), &snap, &Value::Null);
        assert!(out.initialized);
        assert!(!out.triggered());
        assert!(out.state_changed);
        assert!(state["initialized"].as_bool().unwrap());
    }

    #[test]
    fn count_increase_alerts_with_specific_shows() {
        let p1 = payload(&json!([show("a", "2026-11-12", "18:10", "5号厅", "2D")]));
        let p2 = payload(&json!([
            show("a", "2026-11-12", "18:10", "5号厅", "2D"),
            show("b", "2026-11-12", "18:40", "2号厅", "2D"),
        ]));
        let w = watch(TimeSpec::All.to_json(), false);
        let c = compiled(&w);
        let (state, out1) = evaluate_cinema(
            &c,
            &CinemaSnapshot::from_payload("37534", &p1),
            &Value::Null,
        );
        assert!(!out1.triggered());
        let (_, out2) = evaluate_cinema(&c, &CinemaSnapshot::from_payload("37534", &p2), &state);
        assert!(out2.triggered());
        assert_eq!(out2.added[0].added.len(), 1);
        assert_eq!(out2.added[0].added[0].tm, "18:40");
    }

    #[test]
    fn decrease_does_not_alert_but_updates_baseline() {
        let p1 = payload(&json!([
            show("a", "2026-11-12", "18:10", "5号厅", "2D"),
            show("b", "2026-11-12", "18:40", "2号厅", "2D"),
        ]));
        let p2 = payload(&json!([show("a", "2026-11-12", "18:10", "5号厅", "2D")]));
        let w = watch(TimeSpec::All.to_json(), false);
        let c = compiled(&w);
        let (state, _) = evaluate_cinema(
            &c,
            &CinemaSnapshot::from_payload("37534", &p1),
            &Value::Null,
        );
        let (state2, out) =
            evaluate_cinema(&c, &CinemaSnapshot::from_payload("37534", &p2), &state);
        assert!(!out.triggered());
        assert!(out.state_changed);
        let s = WatchCinemaState::from_json(&state2);
        let key = bucket_key(NaiveDate::from_ymd_opt(2026, 11, 12).unwrap(), 18);
        assert_eq!(s.buckets[&key].count, 1);
    }

    #[test]
    fn same_hour_swap_does_not_alert_but_updates_shows() {
        let p1 = payload(&json!([show("a", "2026-11-12", "18:10", "5号厅", "2D")]));
        let p2 = payload(&json!([show("b", "2026-11-12", "18:40", "2号厅", "2D")]));
        let w = watch(TimeSpec::All.to_json(), false);
        let c = compiled(&w);
        let (state, _) = evaluate_cinema(
            &c,
            &CinemaSnapshot::from_payload("37534", &p1),
            &Value::Null,
        );
        let (state2, out) =
            evaluate_cinema(&c, &CinemaSnapshot::from_payload("37534", &p2), &state);
        assert!(!out.triggered());
        assert!(out.state_changed);
        let s = WatchCinemaState::from_json(&state2);
        let key = bucket_key(NaiveDate::from_ymd_opt(2026, 11, 12).unwrap(), 18);
        assert_eq!(s.buckets[&key].shows[0].tm, "18:40");
    }

    #[test]
    fn cross_hour_move_alerts_in_new_bucket() {
        let p1 = payload(&json!([show("a", "2026-11-12", "18:10", "5号厅", "2D")]));
        let p2 = payload(&json!([show("a", "2026-11-12", "19:10", "5号厅", "2D")]));
        let w = watch(TimeSpec::All.to_json(), false);
        let c = compiled(&w);
        let (state, _) = evaluate_cinema(
            &c,
            &CinemaSnapshot::from_payload("37534", &p1),
            &Value::Null,
        );
        let (_, out) = evaluate_cinema(&c, &CinemaSnapshot::from_payload("37534", &p2), &state);
        assert!(out.triggered());
        assert_eq!(out.added[0].hour, 19);
    }

    #[test]
    fn imax_only_uses_imax_counts() {
        let p1 = payload(&json!([show("a", "2026-11-12", "18:10", "5号厅", "2D")]));
        let p2 = payload(&json!([
            show("a", "2026-11-12", "18:10", "5号厅", "2D"),
            show("b", "2026-11-12", "18:40", "IMAX 激光厅", "IMAX2D"),
        ]));
        let w = watch(TimeSpec::All.to_json(), true);
        let c = compiled(&w);
        let (state, _) = evaluate_cinema(
            &c,
            &CinemaSnapshot::from_payload("37534", &p1),
            &Value::Null,
        );
        let (_, out) = evaluate_cinema(&c, &CinemaSnapshot::from_payload("37534", &p2), &state);
        assert!(out.triggered());
        assert_eq!(out.added[0].added.len(), 1);
        assert!(out.added[0].added[0].is_imax);
    }

    #[test]
    fn interval_mask_filters_hours() {
        let p1 = payload(&json!([show("a", "2026-11-12", "18:10", "5号厅", "2D")]));
        let p2 = payload(&json!([
            show("a", "2026-11-12", "18:10", "5号厅", "2D"),
            show("b", "2026-11-12", "20:10", "5号厅", "2D"),
        ]));
        let w = watch(
            TimeSpec::parse("2026-11-12 18:00-19:00").unwrap().to_json(),
            false,
        );
        let c = compiled(&w);
        let (state, _) = evaluate_cinema(
            &c,
            &CinemaSnapshot::from_payload("37534", &p1),
            &Value::Null,
        );
        let (_, out) = evaluate_cinema(&c, &CinemaSnapshot::from_payload("37534", &p2), &state);
        assert!(!out.triggered(), "20点不在18-19窗口内");
    }

    #[test]
    fn daily_hours_filters_hours_across_dates() {
        let p1 = payload(&json!([show("a", "2026-11-12", "18:10", "5号厅", "2D")]));
        let p2 = payload(&json!([
            show("a", "2026-11-12", "18:10", "5号厅", "2D"),
            show("b", "2026-11-13", "18:20", "5号厅", "2D"),
            show("c", "2026-11-13", "20:20", "5号厅", "2D"),
        ]));
        let w = watch(TimeSpec::parse("18:00-19:00").unwrap().to_json(), false);
        let c = compiled(&w);
        let (state, _) = evaluate_cinema(
            &c,
            &CinemaSnapshot::from_payload("37534", &p1),
            &Value::Null,
        );
        let (_, out) = evaluate_cinema(&c, &CinemaSnapshot::from_payload("37534", &p2), &state);
        assert!(out.triggered());
        assert_eq!(out.added.len(), 1);
        assert_eq!(out.added[0].date, "2026-11-13");
        assert_eq!(out.added[0].hour, 18);
    }

    #[test]
    fn poll_plan_dedupes_cinemas_and_watches() {
        let watches = vec![
            json!({"id":"w1","enabled":true,"cinemas":["a","b"]}),
            json!({"id":"w2","enabled":true,"cinemas":["b","c"]}),
        ];
        let plan = build_poll_plan(&watches);
        assert_eq!(plan.cinemas(), vec!["a", "b", "c"]);
        assert_eq!(plan.watches_for("b"), &["w1", "w2"]);
        assert_eq!(plan.watch_cinemas["w1"], vec!["a", "b"]);
    }

    #[tokio::test]
    async fn fetch_many_isolates_errors() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let now = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let cinemas: Vec<String> = (0..10).map(|i| format!("c{}", i)).collect();
        let now2 = Arc::clone(&now);
        let peak2 = Arc::clone(&peak);
        let outcome = fetch_many(cinemas, 3, move |cid| {
            let now = Arc::clone(&now2);
            let peak = Arc::clone(&peak2);
            async move {
                let cur = now.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(cur, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                now.fetch_sub(1, Ordering::SeqCst);
                if cid == "c3" {
                    (cid, Err(anyhow::anyhow!("boom")))
                } else {
                    (cid, Ok(payload(&json!([]))))
                }
            }
        })
        .await;
        assert!(peak.load(Ordering::SeqCst) <= 3);
        assert_eq!(outcome.snapshots.len(), 9);
        assert_eq!(outcome.errors.len(), 1);
    }
}
