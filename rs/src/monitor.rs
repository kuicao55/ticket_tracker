//! 监测循环 —— 与 py/.../monitor.py 1:1 对齐。
//!
//! 设计要点（参考 RUST_PORT.md §5.3）：
//! - 异步：tokio 调度循环
//! - 单条 watch 返回 status ∈ {open, not_listed, no_shows, error}
//! - tick 节流：每条 watch 独立 interval，未到时间跳过
//! - 时段策略：quiet → 60s 等；normal/phone_only → 正常
//! - 触发顺序：Discord → macOS（仅 normal）→ mark_presale_fired
//! - 自动停用：所有 cinema 都触发过 → enabled=false
//! - heartbeat：每 heartbeat_interval_sec 发 Discord 报告

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde_json::{json, Value};
use tokio::sync::Notify;

use crate::config;
use crate::maoyan;
use crate::notify;

// 单 status 字符串
pub type Status = String;
pub const S_OPEN: &str = "open";
pub const S_NOT_LISTED: &str = "not_listed";
pub const S_NO_SHOWS: &str = "no_shows";
pub const S_ERROR: &str = "error";

#[derive(Debug, Clone)]
pub struct Match {
    pub cinema_id: String,
    pub cinema_name: String,
    pub show_count: i64,
    pub earliest: String,
    pub latest: String,
}

#[derive(Debug, Clone)]
pub struct WatchInfo {
    pub name: String,
    pub matches: Vec<Match>,
    /// 所有本次扫到的 cinema（含未开售的）—— 用于未触发时也能填子表
    pub all_cinemas: Vec<Match>,
    pub cinema_names: HashMap<String, String>,
    pub show_dates: HashMap<String, Vec<String>>,
    pub errors: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub enum WatchStatus {
    Open(WatchInfo),
    NotListed(WatchInfo),
    NoShows(WatchInfo),
    Error(WatchInfo),
}

impl WatchStatus {
    pub fn code(&self) -> &'static str {
        match self {
            WatchStatus::Open(_) => S_OPEN,
            WatchStatus::NotListed(_) => S_NOT_LISTED,
            WatchStatus::NoShows(_) => S_NO_SHOWS,
            WatchStatus::Error(_) => S_ERROR,
        }
    }
    pub fn info(&self) -> &WatchInfo {
        match self {
            WatchStatus::Open(i)
            | WatchStatus::NotListed(i)
            | WatchStatus::NoShows(i)
            | WatchStatus::Error(i) => i,
        }
    }
}

// ----------------- 单个 watch 检查 -----------------

pub async fn check_watch(watch: &Value, cinema_cache: &mut HashMap<String, Value>) -> WatchStatus {
    let movie_id = watch
        .get("movie_id")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let movie_name = watch
        .get("movie_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let cinema_ids: Vec<String> = watch
        .get("cinemas")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    if cinema_ids.is_empty() {
        let info = WatchInfo {
            name: if movie_name.is_empty() {
                movie_id.to_string()
            } else {
                movie_name.clone()
            },
            matches: vec![],
            all_cinemas: vec![],
            cinema_names: HashMap::new(),
            show_dates: HashMap::new(),
            errors: vec![("?".into(), "该 watch 未指定影院".into())],
        };
        return WatchStatus::Error(info);
    }

    let mut matches = Vec::new();
    let mut all_cinemas = Vec::new();
    let mut errors = Vec::new();
    let mut any_listed = false;
    let mut cinema_names = HashMap::new();
    let mut show_dates = HashMap::new();

    for cid in &cinema_ids {
        if !cinema_cache.contains_key(cid) {
            match maoyan::fetch_cinema_async(cid).await {
                Ok(payload) => {
                    cinema_cache.insert(cid.clone(), payload);
                }
                Err(e) => {
                    errors.push((cid.clone(), e.to_string()));
                    continue;
                }
            }
        }
        let payload = cinema_cache.get(cid).unwrap();
        let cinema_name = payload
            .get("cinema_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        cinema_names.insert(cid.clone(), cinema_name.clone());
        let kw: Vec<&str> = if movie_name.is_empty() {
            vec![]
        } else {
            vec![&movie_name]
        };
        let movie = maoyan::find_movie(payload, movie_id, &kw);
        let Some(movie) = movie else {
            // 此 cinema 没出现当前电影，但仍占一行（占位）
            all_cinemas.push(Match {
                cinema_id: cid.clone(),
                cinema_name: cinema_name.clone(),
                show_count: 0,
                earliest: String::new(),
                latest: "—".into(),
            });
            continue;
        };
        any_listed = true;
        let all_dates = maoyan::movie_dates(movie);
        show_dates.insert(cid.clone(), all_dates.clone());
        // 全场次计数
        let total_shows: i64 = movie
            .get("shows")
            .and_then(|v| v.as_array())
            .map(|arr: &Vec<Value>| {
                arr.iter()
                    .map(|s: &Value| {
                        s.get("plist")
                            .and_then(|v| v.as_array())
                            .map(|x: &Vec<Value>| x.len() as i64)
                            .unwrap_or(0)
                    })
                    .sum()
            })
            .unwrap_or(0);
        let mut show_count = movie
            .get("showCount")
            .and_then(|v| v.as_i64())
            .unwrap_or(total_shows);

        // 日期过滤
        let mut dates = all_dates.clone();
        let allowed: Option<BTreeSet<String>> = watch
            .get("dates")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect());
        if let Some(ref allowed) = allowed {
            dates.retain(|d| allowed.contains(d));
            // 重算限定内 show_count
            show_count = movie
                .get("shows")
                .and_then(|v| v.as_array())
                .map(|arr: &Vec<Value>| {
                    arr.iter()
                        .map(|s: &Value| {
                            s.get("plist")
                                .and_then(|v| v.as_array())
                                .map(|plist: &Vec<Value>| {
                                    plist
                                        .iter()
                                        .filter(|p: &&Value| {
                                            p.get("dt")
                                                .and_then(|v| v.as_str())
                                                .map(|d| allowed.contains(d))
                                                .unwrap_or(false)
                                        })
                                        .count() as i64
                                })
                                .unwrap_or(0)
                        })
                        .sum()
                })
                .unwrap_or(0);
        }
        if dates.is_empty() || show_count <= 0 {
            // 影院有但限定日内无场次——同样占位
            all_cinemas.push(Match {
                cinema_id: cid.clone(),
                cinema_name: cinema_name.clone(),
                show_count: 0,
                earliest: all_dates.first().cloned().unwrap_or_default(),
                latest: all_dates.last().cloned().unwrap_or_else(|| "—".into()),
            });
            continue;
        }
        let m = Match {
            cinema_id: cid.clone(),
            cinema_name,
            show_count,
            earliest: dates.first().cloned().unwrap_or_default(),
            latest: dates.last().cloned().unwrap_or_default(),
        };
        matches.push(m.clone());
        all_cinemas.push(m);
    }

    let info = WatchInfo {
        name: if movie_name.is_empty() {
            movie_id.to_string()
        } else {
            movie_name.clone()
        },
        matches: matches.clone(),
        all_cinemas: all_cinemas.clone(),
        cinema_names,
        show_dates,
        errors,
    };
    if !any_listed {
        WatchStatus::NotListed(info)
    } else if matches.is_empty() {
        WatchStatus::NoShows(info)
    } else {
        WatchStatus::Open(info)
    }
}

// ----------------- Monitor 主循环 -----------------

/// Monitor 共享状态：cfg/events/stats 用 `std::sync::Mutex` 包装，并通过
/// `Arc` 让 TUI 的渲染线程可以**零 async** 地读，避免 tokio::sync::Mutex 被
/// run() 永久占用导致 try_lock 永远失败的旧 bug。
pub struct SharedState {
    pub cfg: StdMutex<Value>,
    pub events: StdMutex<VecDeque<String>>,
    pub stats: StdMutex<Stats>,
}

pub struct Monitor {
    pub shared: Arc<SharedState>,
    stop: Arc<Notify>,
    force_flag: Arc<AtomicBool>,
    /// wid 集合：要求「只强制检查这一条」的 watch（per-watch force check）
    force_targets: Arc<StdMutex<std::collections::HashSet<String>>>,
    #[allow(dead_code)]
    watch_filter: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub started_at: f64,
    pub check_count: u64,
    pub per_watch_last: HashMap<String, f64>,
}

impl Monitor {
    pub fn new(watch_filter: Option<Vec<String>>) -> Result<Self> {
        let mut cfg = config::load_or_init()?;
        // 应用 watch_filter：只保留指定的 watch id
        if let Some(ref ids) = watch_filter {
            let id_set: std::collections::HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
            if let Some(arr) = cfg.get_mut("watches").and_then(|v| v.as_array_mut()) {
                arr.retain(|w| {
                    w.get("id")
                        .and_then(|v| v.as_str())
                        .map(|s| id_set.contains(s))
                        .unwrap_or(false)
                });
            }
        }
        let started_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        Ok(Self {
            shared: Arc::new(SharedState {
                cfg: StdMutex::new(cfg),
                events: StdMutex::new(VecDeque::with_capacity(12)),
                stats: StdMutex::new(Stats {
                    started_at,
                    check_count: 0,
                    per_watch_last: HashMap::new(),
                }),
            }),
            stop: Arc::new(Notify::new()),
            force_flag: Arc::new(AtomicBool::new(false)),
            force_targets: Arc::new(StdMutex::new(std::collections::HashSet::new())),
            watch_filter: watch_filter.unwrap_or_default(),
        })
    }

    /// TUI 线程用：拿 cfg 的**快照**（cheap clone）。
    pub fn cfg_snapshot(&self) -> Value {
        self.shared.cfg.lock().unwrap().clone()
    }

    /// TUI 线程用：拿 events 的**快照**。
    pub fn events_snapshot(&self) -> Vec<String> {
        self.shared
            .events
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect()
    }

    /// TUI 线程用：拿 stats 的**快照**。
    pub fn stats_snapshot(&self) -> Stats {
        self.shared.stats.lock().unwrap().clone()
    }

    pub async fn push_event(&self, line: String) {
        let ts = chrono::Local::now().format("%H:%M:%S");
        let entry = format!("[{}] {}", ts, line);
        let mut q = self.shared.events.lock().unwrap();
        // 仅保留最近 12 条，避免高频推送导致队列爆炸
        if q.len() >= 12 {
            q.pop_back();
        }
        q.push_front(entry);
    }

    pub fn stop(&self) {
        self.stop.notify_waiters();
    }

    pub fn force_check(&self) {
        self.force_flag.store(true, Ordering::SeqCst);
    }

    /// 强制只检查一条 watch（跳过 interval 节流；其余 watch 仍按正常节奏跑）
    pub fn force_check_wid(&self, wid: String) {
        if !wid.is_empty() {
            self.force_targets.lock().unwrap().insert(wid);
        }
    }

    /// 全局手动检查：将所有当前启用的 watch 的 wid 都加入 force_targets。
    /// 不直接翻转 force_flag 以避免一过即丢，集合里的 wid 在被消费后自动清出。
    pub fn force_check_all(&self) {
        // 同时保留旧 force_flag（用于兼容），并把 enabled 集合塞入
        self.force_flag.store(true, Ordering::SeqCst);
        let enabled_wids: Vec<String> = {
            let g = self.shared.cfg.lock().unwrap();
            g.get("watches")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter(|w| w.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false))
                        .filter_map(|w| w.get("id").and_then(|v| v.as_str()).map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut ft = self.force_targets.lock().unwrap();
        for w in enabled_wids {
            ft.insert(w);
        }
    }

    /// 读出当前 force_targets 的 snapshot 并清空它（一轮 tick 内消费一次）。
    fn drain_force_targets(&self) -> std::collections::HashSet<String> {
        let mut ft = self.force_targets.lock().unwrap();
        let snap = ft.clone();
        ft.clear();
        snap
    }

    pub async fn run(&self) {
        // 预先快照所有 cfg 字段（owned），避免后续可变借用冲突
        let n = {
            let g = self.shared.cfg.lock().unwrap();
            g.get("watches")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0)
        };
        let interval = {
            let g = self.shared.cfg.lock().unwrap();
            g.get("check_interval")
                .and_then(|v| v.as_u64())
                .unwrap_or(90)
        };
        let quiet_w = {
            let g = self.shared.cfg.lock().unwrap();
            g.get("quiet_window")
                .and_then(|v| v.as_str())
                .unwrap_or("01:00-06:00")
                .to_string()
        };
        let phone_w = {
            let g = self.shared.cfg.lock().unwrap();
            g.get("phone_only_window")
                .and_then(|v| v.as_str())
                .unwrap_or("06:00-09:00")
                .to_string()
        };
        let webhook: Option<String> = {
            let g = self.shared.cfg.lock().unwrap();
            g.get("discord_webhook")
                .and_then(|v| v.as_str())
                .map(String::from)
        };
        let hb_interval = {
            let g = self.shared.cfg.lock().unwrap();
            g.get("heartbeat_interval_sec")
                .and_then(|v| v.as_u64())
                .unwrap_or(3600)
        };

        let msg = if n == 0 {
            "无监视项。请 tt watch add 或在 TUI 里按 a 添加。".to_string()
        } else {
            format!(
                "监视项 {} 条｜间隔 {}s｜时段 quiet={} phone_only={}",
                n, interval, quiet_w, phone_w
            )
        };
        let _ = notify::notify_discord_async(
            webhook.as_deref(),
            "ticket-tracker 已启动 ✅",
            &msg,
            None,
        )
        .await;

        let mut last_heartbeat = std::time::Instant::now();
        loop {
            // 时段判定
            let now_h = chrono::Local::now().format("%H").to_string();
            let hour: u32 = now_h.parse().unwrap_or(0);
            let mode = config::current_mode(&quiet_w, &phone_w, hour).unwrap_or(config::Mode::Normal);
            if mode == config::Mode::Quiet {
                self.push_event("进入静默时段：暂停抓取/推送".into()).await;
                if self.wait_with_stop(Duration::from_secs(60)).await {
                    break;
                }
                continue;
            }

            // 强制检查：force_flag（全局） + force_targets（per-wid 集合）
            let force_all = self.force_flag.swap(false, Ordering::SeqCst);
            let force_set = self.drain_force_targets();
            let force = force_all || !force_set.is_empty();
            if force_all {
                self.push_event("· 手动触发一轮检查…".into()).await;
            }
            if !force_set.is_empty() && !force_all {
                self.push_event(format!("· 手动触发 {} 条 watch 检查…", force_set.len())).await;
            }

            let did = self.tick(mode, force, &force_set).await;
            if did {
                let mut s = self.shared.stats.lock().unwrap();
                s.check_count += 1;
            }

            if force {
                continue;
            }

            // heartbeat
            if last_heartbeat.elapsed() >= Duration::from_secs(hb_interval) {
                let any_enabled = {
                    let g = self.shared.cfg.lock().unwrap();
                    g.get("watches")
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().any(|w| w.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false)))
                        .unwrap_or(false)
                };
                if any_enabled {
                    self.send_heartbeat().await;
                }
                last_heartbeat = std::time::Instant::now();
            }

            // loop 间隔：取 enabled 中最小 interval，否则默认
            let eff = self.effective_interval_secs();
            if self.wait_with_stop(Duration::from_secs(eff.max(1))).await {
                break;
            }
        }

        // 已停止推送
        let started = self.shared.stats.lock().unwrap().started_at;
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
            - started;
        let uptime = format_uptime(elapsed as u64);
        let check_count = self.shared.stats.lock().unwrap().check_count;
        let webhook_final = {
            self.shared
                .cfg
                .lock()
                .unwrap()
                .get("discord_webhook")
                .and_then(|v| v.as_str())
                .map(String::from)
        };
        let _ = notify::notify_discord_async(
            webhook_final.as_deref(),
            "ticket-tracker 已停止 🛑",
            &format!("运行时长 {}｜累计检查 {} 次", uptime, check_count),
            None,
        )
        .await;
    }

    async fn wait_with_stop(&self, d: Duration) -> bool {
        tokio::select! {
            _ = tokio::time::sleep(d) => false,
            _ = self.stop.notified() => true,
        }
    }

    fn effective_interval_secs(&self) -> u64 {
        // 一次性 lock、立刻拷贝所有权，绝不跨 await 持有
        let (default, intervals): (u64, Vec<Option<u64>>) = {
            let g = self.shared.cfg.lock().unwrap();
            let def = g
                .get("check_interval")
                .and_then(|v| v.as_u64())
                .unwrap_or(90);
            let ints: Vec<Option<u64>> = g
                .get("watches")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter(|w| {
                            w.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false)
                        })
                        .filter_map(|w| w.get("interval").and_then(|v| v.as_u64()))
                        .map(Some)
                        .collect()
                })
                .unwrap_or_default();
            (def, ints)
        };
        if intervals.is_empty() {
            return default;
        }
        intervals
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(default)
    }

    /// 主 tick 一轮
    async fn tick(
        &self,
        mode: config::Mode,
        force: bool,
        force_set: &std::collections::HashSet<String>,
    ) -> bool {
        let mut any_done = false;
        let mut cinema_cache: HashMap<String, Value> = HashMap::new();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        // 一次性克隆所有 watch（避免借用 cfg 同时改 cfg）
        let watch_count = {
            let g = self.shared.cfg.lock().unwrap();
            g.get("watches")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0)
        };

        for i in 0..watch_count {
            // 拿 watch 的 clone
            let watch_opt = {
                let g = self.shared.cfg.lock().unwrap();
                g.get("watches")
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.get(i).cloned())
            };
            let Some(watch) = watch_opt else {
                continue;
            };
            if !watch.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false) {
                continue;
            }
            let wid = watch.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();

            // per-watch 强制检查（即使 force 全局为 false）
            let per_wid_force = force_set.contains(&wid);
            let effective_force = force || per_wid_force;

            // 独立 interval 节流
            if !effective_force {
                let w_interval = watch.get("interval").and_then(|v| v.as_u64());
                if let Some(secs) = w_interval {
                    let mut s = self.shared.stats.lock().unwrap();
                    let last = s.per_watch_last.get(&wid).copied().unwrap_or(0.0);
                    if (now - last) < secs as f64 {
                        drop(s);
                        continue;
                    }
                    s.per_watch_last.insert(wid.clone(), now);
                }
            }

            let status = check_watch(&watch, &mut cinema_cache).await;
            let status_code = status.code().to_string();
            let info = status.info();
            let any_status_non_error = status_code != S_ERROR;
            any_done = any_done || any_status_non_error;

            let movie_id = watch.get("movie_id").and_then(|v| v.as_i64()).unwrap_or(0);
            let label = format!("{}({})", info.name, movie_id);

            // 写 _last_status / _last_payload
            {
                let mut g = self.shared.cfg.lock().unwrap();
                if let Some(arr) = g.get_mut("watches").and_then(|v| v.as_array_mut()) {
                    if let Some(w) = arr.get_mut(i) {
                        w["_last_status"] = json!(status_code);
                        // 子表「cinema / shows / range」需要永远至少有 1 行；
                        // info.matches 仅 Open 状态非空；其余情况回退到 all_cinemas（占位）。
                        let display_matches: Vec<Match> = if !info.matches.is_empty() {
                            info.matches.clone()
                        } else {
                            info.all_cinemas.clone()
                        };
                        // _last_payload: 与 Python 字段对齐
                        w["_last_payload"] = json!({
                            "name": info.name,
                            "matches": display_matches.iter().map(|m| json!({
                                "cinema_id": m.cinema_id,
                                "cinema_name": m.cinema_name,
                                "show_count": m.show_count,
                                "earliest": m.earliest,
                                "latest": m.latest,
                            })).collect::<Vec<_>>(),
                            "cinema_names": info.cinema_names,
                            "show_dates": info.show_dates,
                            "errors": info.errors.iter().map(|(a,b)| json!([a,b])).collect::<Vec<_>>(),
                        });
                    }
                }
            }

            if status_code == S_OPEN {
                let lines: Vec<String> = info
                    .matches
                    .iter()
                    .map(|m| {
                        format!(
                            "{}({}场, {} 起)",
                            m.cinema_name, m.show_count, m.earliest
                        )
                    })
                    .collect();
                self.push_event(format!(
                    "✓ {} 预售开启！{}",
                    label,
                    lines.join(" / ")
                ))
                .await;
                for m in &info.matches {
                    let cid = &m.cinema_id;
                    let already = {
                        let g = self.shared.cfg.lock().unwrap();
                        g.get("watches")
                            .and_then(|v| v.as_array())
                            .and_then(|a| a.get(i))
                            .and_then(|w| w.get("fired_cinemas"))
                            .and_then(|v| v.as_array())
                            .map(|arr| arr.iter().any(|x| x.as_str() == Some(cid.as_str())))
                            .unwrap_or(false)
                    };
                    if already {
                        continue;
                    }
                    let buy_url = maoyan::buy_pc_url_owned(cid);
                    let alert = format!(
                        "{}｜{}：{} 场｜{} 至 {}",
                        info.name, m.cinema_name, m.show_count, m.earliest, m.latest
                    );
                    let webhook = {
                        let g = self.shared.cfg.lock().unwrap();
                        g.get("discord_webhook")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                    };
                    let _ = notify::notify_discord_async(
                        webhook.as_deref(),
                        "预售开启 🎬",
                        &alert,
                        Some(&buy_url),
                    )
                    .await;
                    if mode == config::Mode::Normal {
                        let dur = {
                            let g = self.shared.cfg.lock().unwrap();
                            g.get("alert_duration_sec")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(60)
                        };
                        notify::notify_macos("预售开启 🎬", &alert, true, Some(&buy_url), dur);
                    }
                    // 在 cfg 上记 fired
                    {
                        let mut g = self.shared.cfg.lock().unwrap();
                        let _ = config::mark_presale_fired(&mut g, &wid, cid);
                    }
                }
                // 自动停用
                let all_cinemas: std::collections::HashSet<String> = {
                    let g = self.shared.cfg.lock().unwrap();
                    g.get("watches")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.get(i))
                        .and_then(|w| w.get("cinemas"))
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                        .unwrap_or_default()
                };
                let fired_set: std::collections::HashSet<String> = {
                    let g = self.shared.cfg.lock().unwrap();
                    g.get("watches")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.get(i))
                        .and_then(|w| w.get("fired_cinemas"))
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                        .unwrap_or_default()
                };
                if !all_cinemas.is_empty() && all_cinemas.is_subset(&fired_set) {
                    {
                        let mut g = self.shared.cfg.lock().unwrap();
                        if let Some(arr) = g.get_mut("watches").and_then(|v| v.as_array_mut()) {
                            if let Some(w) = arr.get_mut(i) {
                                w["enabled"] = json!(false);
                            }
                        }
                    }
                    self.push_event(format!(
                        "· {} 全 {} 个影院已触发，自动停用",
                        wid,
                        all_cinemas.len()
                    ))
                    .await;
                    self.push_event(format!("✓ {} 已停用（任务完成）", label))
                        .await;
                }
            } else if status_code == S_NOT_LISTED {
                self.push_event(format!("· {} 影院列表中尚未出现", label))
                    .await;
            } else if status_code == S_NO_SHOWS {
                self.push_event(format!(
                    "· {} 列表有但未开售符合条件的场次",
                    label
                ))
                .await;
            } else {
                let errs = info
                    .errors
                    .iter()
                    .map(|(c, e)| format!("{}: {}", c, e))
                    .collect::<Vec<_>>()
                    .join("; ");
                self.push_event(format!(
                    "✗ {} 检查出错: {}",
                    label,
                    if errs.is_empty() { "未知".into() } else { errs }
                ))
                .await;
            }
        }

        let snapshot = {
            let g = self.shared.cfg.lock().unwrap();
            g.clone()
        };
        let _ = config::save(&snapshot);
        any_done
    }

    async fn send_heartbeat(&self) {
        let mut enabled: Vec<Value> = Vec::new();
        {
            let g = self.shared.cfg.lock().unwrap();
            if let Some(arr) = g.get("watches").and_then(|v| v.as_array()) {
                for w in arr {
                    if w.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false) {
                        enabled.push(w.clone());
                    }
                }
            }
        }
        let has_open = enabled
            .iter()
            .any(|w| w.get("_last_status").and_then(|v| v.as_str()) == Some(S_OPEN));
        let title = if has_open {
            "🎬 检测到开售"
        } else {
            "✅ 例行报告（未开售）"
        };
        let started_at = self.shared.stats.lock().unwrap().started_at;
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
            - started_at;
        let uptime = format_uptime(elapsed as u64);
        let mode_h = chrono::Local::now().format("%H").to_string();
        let hour: u32 = mode_h.parse().unwrap_or(0);
        let (qw, pw) = {
            let g = self.shared.cfg.lock().unwrap();
            let qw = g
                .get("quiet_window")
                .and_then(|v| v.as_str())
                .unwrap_or("01:00-06:00")
                .to_string();
            let pw = g
                .get("phone_only_window")
                .and_then(|v| v.as_str())
                .unwrap_or("06:00-09:00")
                .to_string();
            (qw, pw)
        };
        let mode = config::current_mode(&qw, &pw, hour).unwrap_or(config::Mode::Normal);
        let mode_label = match mode {
            config::Mode::Normal => "正常",
            config::Mode::PhoneOnly => "只推手机",
            config::Mode::Quiet => "静默",
        };
        let check_count = self.shared.stats.lock().unwrap().check_count;
        let mut lines = vec![format!(
            "⏱ {}｜🔍 {} 次｜📡 {}｜活跃 {} 条",
            uptime,
            check_count,
            mode_label,
            enabled.len()
        )];
        if enabled.is_empty() {
            lines.push("（无启用中的监视项）".to_string());
        }
        for w in &enabled {
            lines.push(watch_summary_line(w));
        }
        let body = lines.join("\n");
        let webhook = {
            let g = self.shared.cfg.lock().unwrap();
            g.get("discord_webhook")
                .and_then(|v| v.as_str())
                .map(String::from)
        };
        let _ = notify::notify_discord_async(webhook.as_deref(), title, &body, None).await;
    }
}

// ----------------- Discord 单条 watch 报告格式 -----------------

fn status_icon(code: &str) -> &'static str {
    match code {
        S_OPEN => "🟢 已开售",
        S_NOT_LISTED => "⚫ 未上架",
        S_NO_SHOWS => "🟡 排片中",
        S_ERROR => "🔴 出错",
        _ => "⚪ 待查",
    }
}

fn watch_summary_line(w: &Value) -> String {
    let icon = status_icon(w.get("_last_status").and_then(|v| v.as_str()).unwrap_or(""));
    let name = w
        .get("movie_name")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("movie_{}", w.get("movie_id").and_then(|v| v.as_i64()).unwrap_or(0)));
    let wid = w.get("id").and_then(|v| v.as_str()).unwrap_or("?");
    let cinema_ids: Vec<String> = w
        .get("cinemas")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let payload = w.get("_last_payload").cloned().unwrap_or(json!({}));
    let matches: Vec<Value> = payload
        .get("matches")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let cinema_names: HashMap<String, String> = serde_json::from_value(
        payload.get("cinema_names").cloned().unwrap_or(json!({})),
    )
    .unwrap_or_default();
    let show_dates: HashMap<String, Vec<String>> = serde_json::from_value(
        payload.get("show_dates").cloned().unwrap_or(json!({})),
    )
    .unwrap_or_default();
    let allowed: Vec<String> = w
        .get("dates")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let cinema_label = if cinema_ids.is_empty() {
        "?".to_string()
    } else {
        cinema_ids
            .iter()
            .map(|c| format!("{} ({})", cinema_names.get(c).cloned().unwrap_or_else(|| "?".into()), c))
            .collect::<Vec<_>>()
            .join(" + ")
    };

    if !matches.is_empty() {
        let m0 = &matches[0];
        let earliest = m0.get("earliest").and_then(|v| v.as_str()).unwrap_or("?");
        let latest = m0.get("latest").and_then(|v| v.as_str()).unwrap_or("?");
        let show = m0.get("show_count").and_then(|v| v.as_i64()).unwrap_or(0);
        let cinema = m0.get("cinema_name").and_then(|v| v.as_str()).unwrap_or("?");
        let detail = if matches.len() > 1 {
            format!("{} 等 {} 家 · {} 场 · {}~{}", cinema, matches.len(), show, earliest, latest)
        } else {
            format!("{} · {} 场 · {}~{}", cinema, show, earliest, latest)
        };
        return format!("{} {} ({}) [{}] {}", icon, name, wid, cinema_label, detail);
    }

    let status = w.get("_last_status").and_then(|v| v.as_str()).unwrap_or("");
    if status == S_NOT_LISTED {
        return format!(
            "{} {} ({}) [{}] 影院列表中暂无此电影",
            icon, name, wid, cinema_label
        );
    }
    if cinema_ids.is_empty() {
        return format!("{} {} ({}) [{}] 尚未排片", icon, name, wid, cinema_label);
    }
    let allowed_set: std::collections::HashSet<String> = allowed.iter().cloned().collect();
    let single = cinema_ids.len() == 1;
    let mut parts = Vec::new();
    for cid in &cinema_ids {
        let cn = cinema_names.get(cid).cloned().unwrap_or_else(|| "?".into());
        let prefix = if single { "" } else { &format!("{} ", cn) };
        let ds = show_dates.get(cid).cloned().unwrap_or_default();
        if ds.is_empty() {
            parts.push(format!("{}已上架未排片", prefix));
        } else if allowed.is_empty() {
            parts.push(format!(
                "{}已排 {} 天，最早 {}, 但未触发开售",
                prefix,
                ds.len(),
                ds[0]
            ));
        } else {
            let overlap: Vec<&String> = ds.iter().filter(|d| allowed_set.contains(*d)).collect();
            if !overlap.is_empty() {
                let oe = overlap[0];
                parts.push(format!(
                    "{}限定内已有 {} 等 {} 天",
                    prefix,
                    oe,
                    overlap.len()
                ));
            } else {
                parts.push(format!(
                    "{}限定 {} 无场次；最早开售 {}",
                    prefix,
                    allowed.join("/"),
                    ds[0]
                ));
            }
        }
    }
    format!("{} {} ({}) [{}] {}", icon, name, wid, cinema_label, parts.join("；"))
}

// ----------------- 时间格式 -----------------

pub fn format_uptime(sec: u64) -> String {
    let h = sec / 3600;
    let m = (sec % 3600) / 60;
    let s = sec % 60;
    if h > 0 {
        format!("{}小时{}分", h, m)
    } else if m > 0 {
        format!("{}分{}秒", m, s)
    } else {
        format!("{}秒", s)
    }
}
