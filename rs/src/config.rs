//! 配置管理 —— XDG 路径下的 JSON 读写、首次创建、迁移旧格式。
//!
//! 与 `py/src/ticket_tracker/config.py` 100% 兼容：
//!   - 同样的 v2 schema
//!   - 同样的 v1→v2 迁移（cinema_id → cinemas[]）
//!   - 同样的旧 state.json 迁移
//!   - 同样的原子写（tmp + rename）
//! 参考：RUST_PORT.md §4

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::paths::{config_file, state_dir};

pub const CONFIG_VERSION: u32 = 2;

/// 当前时段模式（与 Python `current_mode()` 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Quiet,
    PhoneOnly,
    Normal,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Quiet => "quiet",
            Mode::PhoneOnly => "phone_only",
            Mode::Normal => "normal",
        }
    }
}

// ----------------- 默认配置 -----------------

pub fn default_config() -> Value {
    json!({
        "version": CONFIG_VERSION,
        "discord_webhook": null,
        "quiet_window": "01:00-06:00",
        "phone_only_window": "06:00-09:00",
        "check_interval": 90,
        "alert_duration_sec": 60,
        "heartbeat_interval_sec": 3600,
        "cinemas": [],
        "watches": [],
        // 自动锁票行为全局默认（无全局 confirm：watch 开了 auto_lock 就真锁，dry-run 只在测试按钮）
        "lock_headless": true,
        "_runtime": {},
    })
}

// ----------------- 锁票默认值常量 -----------------

/// 每个 watch 的默认锁票票数（booker `--num-seats`）。
pub const DEFAULT_LOCK_NUM_SEATS: u64 = 2;
/// 每个 watch 的默认锁票重试次数（同一影院尽力次数，达到即放弃该影院）。
pub const DEFAULT_LOCK_MAX_RETRIES: u64 = 3;

// ----------------- 时段解析（HH:MM-HH:MM） -----------------

/// `'01:00-06:00'` → `(1, 0, 6, 0)`
pub fn parse_window(s: &str) -> Result<(u32, u32, u32, u32)> {
    let s = s.trim();
    // 拆 "-"
    let (a, b) = s
        .split_once('-')
        .ok_or_else(|| anyhow!("时段格式错误，应像 '01:00-06:00'"))?;
    let parse_hhmm = |t: &str| -> Result<(u32, u32)> {
        let (h, m) = t
            .split_once(':')
            .ok_or_else(|| anyhow!("时段格式错误：'{}'", t))?;
        let h: u32 = h.parse().map_err(|_| anyhow!("时段小时非法"))?;
        let m: u32 = m.parse().map_err(|_| anyhow!("时段分钟非法"))?;
        if h > 23 || m > 59 {
            return Err(anyhow!("时段超出范围"));
        }
        Ok((h, m))
    };
    let (sh, sm) = parse_hhmm(a)?;
    let (eh, em) = parse_hhmm(b)?;
    Ok((sh, sm, eh, em))
}

/// 当前小时属于哪个时段。
/// 公式（与 Python `current_mode` 1:1）：
///   quiet_window.start <= h < quiet_window.end       → Quiet
///   phone_only_window.start <= h < phone_only_window.end → PhoneOnly
///   其他                                              → Normal
/// 注意：不处理跨午夜（永远 start < end，与 Python 行为一致）。
pub fn current_mode(quiet_window: &str, phone_only_window: &str, hour: u32) -> Result<Mode> {
    let (qs, _, qe, _) = parse_window(quiet_window)?;
    let (ps, _, pe, _) = parse_window(phone_only_window)?;
    if (qs..qe).contains(&hour) {
        Ok(Mode::Quiet)
    } else if (ps..pe).contains(&hour) {
        Ok(Mode::PhoneOnly)
    } else {
        Ok(Mode::Normal)
    }
}

// ----------------- 加载 / 保存 -----------------

/// 加载（或首次创建）配置。自动补字段 + 跑迁移。
pub fn load_or_init() -> Result<Value> {
    let p = config_file();
    if !p.exists() {
        let cfg = default_config();
        save(&cfg)?;
        return Ok(cfg);
    }

    let mut cfg: Value = match read_json(&p) {
        Ok(v) => v,
        Err(_) => {
            // 损坏 → 备份 + 重置
            let backup = p.with_extension("broken.json");
            let _ = std::fs::rename(&p, &backup);
            let cfg = default_config();
            save(&cfg)?;
            return Ok(cfg);
        }
    };

    // 补字段（与 Python setdefault 行为一致）
    if cfg.get("version").and_then(|v| v.as_u64()) != Some(CONFIG_VERSION as u64) {
        cfg["version"] = json!(CONFIG_VERSION);
    }
    let defaults = default_config();
    if let Value::Object(dmap) = defaults {
        if let Value::Object(ref mut cmap) = cfg {
            for (k, v) in dmap {
                cmap.entry(k).or_insert(v);
            }
        }
    }
    if cfg.get("_runtime").is_none() {
        cfg["_runtime"] = json!({});
    }

    migrate_legacy_state(&mut cfg)?;
    migrate_watch_schema(&mut cfg)?;
    migrate_per_watch_notify(&mut cfg)?;
    Ok(cfg)
}

/// 原子化写：tmp → rename。
pub fn save(cfg: &Value) -> Result<()> {
    let p = config_file();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let tmp = with_suffix(&p, "json.tmp");
    let body = serde_json::to_string_pretty(cfg)?;
    std::fs::write(&tmp, body).with_context(|| format!("写入临时配置失败: {}", tmp.display()))?;
    std::fs::rename(&tmp, &p).with_context(|| format!("原子 rename 失败: {}", tmp.display()))?;
    Ok(())
}

fn with_suffix(p: &Path, suf: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".");
    s.push(suf);
    PathBuf::from(s)
}

fn read_json(p: &Path) -> Result<Value> {
    let s = std::fs::read_to_string(p)?;
    Ok(serde_json::from_str(&s)?)
}

// ----------------- 迁移 -----------------

/// 旧 `monitor_spiderman.py` 的 state.json 迁移。
fn migrate_legacy_state(cfg: &mut Value) -> Result<()> {
    if cfg.get("_migrated_legacy_state") == Some(&json!(true)) {
        return Ok(());
    }
    // legacy path 与 Python 一致：当前 config.py 所在包的父亲的父亲 = 项目根
    // 但 Rust 端没有"项目根"概念，使用 state_dir 下方做兜底，避免在 root 找。
    let legacy = state_dir().parent().unwrap_or(Path::new(".")).join("state.json");
    if !legacy.exists() {
        cfg["_migrated_legacy_state"] = json!(true);
        return Ok(());
    }
    let Ok(old) = read_json(&legacy) else {
        cfg["_migrated_legacy_state"] = json!(true);
        return Ok(());
    };
    if let Some(watches) = cfg.get_mut("watches").and_then(|v| v.as_array_mut()) {
        for watch in watches.iter_mut() {
            let key = format!("movie_{}", watch.get("movie_id").and_then(|v| v.as_i64()).unwrap_or(0));
            let presale = old
                .get(&key)
                .and_then(|v| v.get("presale_open"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if presale {
                watch["presale_fired"] = json!(true);
                if watch.get("last_alert_at").is_none() {
                    if let Some(d) = old.get(&key).and_then(|v| v.get("detected_at")) {
                        watch["last_alert_at"] = d.clone();
                    }
                }
            }
        }
    }
    let backup = with_suffix(&legacy, "json.bak");
    if !backup.exists() {
        let _ = std::fs::rename(&legacy, &backup);
    }
    cfg["_migrated_legacy_state"] = json!(true);
    save(cfg)?;
    Ok(())
}

/// v1 → v2：`watch.cinema_id` 变成 `watch.cinemas[]`，并对每个 id 调 add_cinema。
fn migrate_watch_schema(cfg: &mut Value) -> Result<()> {
    if cfg.get("_watch_schema_migrated") == Some(&json!(true)) {
        return Ok(());
    }
    let mut to_register: Vec<String> = Vec::new();
    if let Some(watches) = cfg.get_mut("watches").and_then(|v| v.as_array_mut()) {
        for w in watches.iter_mut() {
            // cinema_id → cinemas
            if w.get("cinemas").is_none() {
                if let Some(cid) = w.get("cinema_id").cloned() {
                    w["cinemas"] = json!([cid]);
                    w.as_object_mut().unwrap().remove("cinema_id");
                    // 收集待注册 cinema
                    if let Some(s) = cid.as_str() {
                        if !s.is_empty() {
                            to_register.push(s.to_string());
                        }
                    }
                } else {
                    w["cinemas"] = json!([]);
                }
            }
            if w.get("dates").is_none() {
                w["dates"] = Value::Null;
            }
            // movie_name 自动填补（与 Python 一样尽力尝试一次；失败置 None）
            if matches!(w.get("movie_name"), None | Some(Value::Null)) {
                if let Some(mid) = w.get("movie_id").and_then(|v| v.as_i64()) {
                    let name = crate::maoyan::fetch_movie_name(mid as i64).ok().flatten();
                    w["movie_name"] = name.map(Value::String).unwrap_or(Value::Null);
                }
            }
            // wechat_notify：老 watch 默认为 false（不开启微信大群通知）
            if w.get("wechat_notify").is_none() {
                w["wechat_notify"] = json!(false);
            }
            // 锁票相关老 watch 默认：不开锁票，其余走全局默认值（向后兼容）。
            settle_lock_defaults(w);
        }
    }
    // 第二遍：注册 cinemas（不再持有 watches 的可变借用）
    for cid in to_register {
        add_cinema(cfg, &cid, None)?;
    }
    cfg["_watch_schema_migrated"] = json!(true);
    save(cfg)?;
    Ok(())
}

/// v1 全局通知 → v2 per-watch 通知迁移：
/// - 老的 `notify_webhook` / `notify_email_to` 在 cfg 顶层
/// - 现在搬到每个 watch 上，没填的默认继承全局值
/// - 迁移完删掉全局键
fn migrate_per_watch_notify(cfg: &mut Value) -> Result<()> {
    if cfg.get("_per_watch_notify_migrated") == Some(&json!(true)) {
        return Ok(());
    }
    let global_wh = cfg
        .get("notify_webhook")
        .and_then(|v| v.as_str())
        .map(String::from)
        .filter(|s| !s.is_empty());
    let global_email = cfg
        .get("notify_email_to")
        .and_then(|v| v.as_str())
        .map(String::from)
        .filter(|s| !s.is_empty());
    let needs_save = global_wh.is_some() || global_email.is_some();
    if let Some(watches) = cfg.get_mut("watches").and_then(|v| v.as_array_mut()) {
        for w in watches.iter_mut() {
            // 没有 per-watch 字段、或显式 null，都用全局兜底
            let has_wh = w.get("notify_webhook").map(|v| !v.is_null()).unwrap_or(false);
            let has_email = w.get("notify_email_to").map(|v| !v.is_null()).unwrap_or(false);
            if !has_wh {
                if let Some(ref g) = global_wh {
                    w["notify_webhook"] = json!(g);
                }
            }
            if !has_email {
                if let Some(ref g) = global_email {
                    w["notify_email_to"] = json!(g);
                }
            }
        }
    }
    if let Some(obj) = cfg.as_object_mut() {
        obj.remove("notify_webhook");
        obj.remove("notify_email_to");
    }
    cfg["_per_watch_notify_migrated"] = json!(true);
    if needs_save {
        save(cfg)?;
    }
    Ok(())
}

/// 给单个 watch 补齐全部锁票字段默认值（幂等，已存在的字段不动）。
/// 供两条路径共用：老配置迁移 + 新 watch 直建。
fn settle_lock_defaults(w: &mut Value) {
    let _ = w.get("auto_lock").is_none().then(|| w["auto_lock"] = json!(false));
    let _ = w.get("imax_only").is_none().then(|| w["imax_only"] = json!(false));
    let _ = w
        .get("lock_num_seats")
        .is_none()
        .then(|| w["lock_num_seats"] = json!(DEFAULT_LOCK_NUM_SEATS));
    let _ = w
        .get("lock_max_retries")
        .is_none()
        .then(|| w["lock_max_retries"] = json!(DEFAULT_LOCK_MAX_RETRIES));
    let _ = w.get("lock_seats").is_none().then(|| w["lock_seats"] = json!([]));
    // 锁票运行状态（不进入 add_watch 的初始 JSON，由 mark/incr 写入）
    let _ = w.get("lock_ok_cinemas").is_none().then(|| w["lock_ok_cinemas"] = json!([]));
    let _ = w.get("lock_tries").is_none().then(|| w["lock_tries"] = json!({}));
}

// ----------------- 影院操作 -----------------

pub fn find_cinema<'a>(cfg: &'a Value, cinema_id: &str) -> Option<&'a Value> {
    cfg.get("cinemas")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find(|c| c.get("id").and_then(|v| v.as_str()) == Some(cinema_id))
        })
}

pub fn add_cinema(cfg: &mut Value, cinema_id: &str, name: Option<&str>) -> Result<bool> {
    if find_cinema(cfg, cinema_id).is_some() {
        return Ok(false);
    }
    let cinemas = cfg
        .get_mut("cinemas")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| anyhow!("config 缺少 cinemas[]"))?;
    let name_value = match name {
        Some(n) => n.to_string(),
        None => format!("影城 {}", cinema_id),
    };
    cinemas.push(json!({
        "id": cinema_id,
        "name": name_value,
        "builtin": false,
    }));
    save(cfg)?;
    Ok(true)
}

pub fn remove_cinema(cfg: &mut Value, cinema_id: &str) -> Result<bool> {
    let before = cfg
        .get("cinemas")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    if let Some(arr) = cfg.get_mut("cinemas").and_then(|v| v.as_array_mut()) {
        arr.retain(|c| c.get("id").and_then(|v| v.as_str()) != Some(cinema_id));
    }
    let after = cfg
        .get("cinemas")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    save(cfg)?;
    Ok(after < before)
}

// ----------------- 监视项操作 -----------------

/// watch 模式：开票提醒（首次开售即报，报完自动停用）。缺省值。
pub const MODE_PRESALE: &str = "presale";
/// watch 模式：增场监控（首轮静默存基线，之后每次出现新场次就报，不自动停用）。
pub const MODE_INCREMENTAL: &str = "incremental";

/// 读 watch 的模式，缺省 / 非法值一律按 `presale` 处理（向后兼容旧配置）。
pub fn watch_mode(watch: &Value) -> &'static str {
    match watch.get("mode").and_then(|v| v.as_str()) {
        Some(MODE_INCREMENTAL) => MODE_INCREMENTAL,
        _ => MODE_PRESALE,
    }
}

/// 读 watch 的时段窗口（如 `"19:00-22:00"`）。null / 空 / 非法 → None（不限）。
pub fn watch_time_window(watch: &Value) -> Option<(u32, u32, u32, u32)> {
    watch
        .get("time_window")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| parse_window(s).ok())
}

/// 给定场次的 `"HH:MM"` 时间，判断是否落在 watch 的时段窗口内。
/// 无窗口配置 → 一律 true（向后兼容旧 watch）。
/// `tm` 解析失败也按 true 处理，宁可多报不要漏报。
pub fn in_time_window(watch: &Value, tm: &str) -> bool {
    let Some((sh, sm, eh, em)) = watch_time_window(watch) else {
        return true;
    };
    let Some((hh, mm)) = tm.split_once(':') else {
        return true;
    };
    let (Ok(h), Ok(m)) = (hh.parse::<u32>(), mm.parse::<u32>()) else {
        return true;
    };
    let t = h * 60 + m;
    let lo = sh * 60 + sm;
    let hi = eh * 60 + em;
    (lo..hi).contains(&t)
}

pub fn list_watches(cfg: &Value) -> Vec<Value> {
    cfg.get("watches")
        .and_then(|v| v.as_array())
        .map(|a| a.clone())
        .unwrap_or_default()
}

pub fn find_watch<'a>(cfg: &'a Value, watch_id: &str) -> Option<&'a Value> {
    cfg.get("watches")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.iter().find(|w| w.get("id").and_then(|v| v.as_str()) == Some(watch_id)))
}

pub fn find_watch_mut<'a>(cfg: &'a mut Value, watch_id: &str) -> Option<&'a mut Value> {
    cfg.get_mut("watches")
        .and_then(|v| v.as_array_mut())
        .and_then(|arr| {
            arr.iter_mut()
                .find(|w| w.get("id").and_then(|v| v.as_str()) == Some(watch_id))
        })
}

pub fn add_watch(
    cfg: &mut Value,
    movie_id: i64,
    cinemas: &[&str],
    dates: Option<&[String]>,
    name: Option<&str>,
    interval: Option<u64>,
    mode: &str,
    time_window: Option<&str>,
    notify_webhook: Option<&str>,
    notify_email_to: Option<&str>,
    xhs_group: Option<&str>,
    wechat_notify: bool,
    // 锁票相关（默认值见 DEFAULT_LOCK_* 常量）
    auto_lock: bool,
    imax_only: bool,
    lock_num_seats: u64,
    lock_max_retries: u64,
    lock_seats: &[String],
) -> Result<String> {
    let cinemas_v: Vec<String> = cinemas.iter().map(|s| s.to_string()).collect();
    for cid in &cinemas_v {
        if find_cinema(cfg, cid).is_none() {
            add_cinema(cfg, cid, None)?;
        }
    }
    if let Some(tw) = time_window {
        if !tw.trim().is_empty() {
            parse_window(tw).map_err(|e| anyhow!("time_window 格式错误: {}", e))?;
        }
    }
    let watch_id = format!("w_{}", &Uuid::new_v4().to_string()[..6]);
    let tw_str = time_window.map(str::trim).filter(|s| !s.is_empty());
    let watch = json!({
        "id": watch_id,
        "movie_id": movie_id,
        "movie_name": name,
        "cinemas": cinemas_v,
        "dates": dates.map(|ds| {
            let mut v: Vec<String> = ds.iter().cloned().collect();
            v.sort();
            v.dedup();
            Value::Array(v.into_iter().map(Value::String).collect())
        }).unwrap_or(Value::Null),
        "interval": interval.map(|n| json!(n)).unwrap_or(Value::Null),
        "mode": if mode == MODE_INCREMENTAL { MODE_INCREMENTAL } else { MODE_PRESALE },
        "time_window": tw_str.map(|s| Value::String(s.to_string())).unwrap_or(Value::Null),
        "notify_webhook": notify_webhook.map(String::from).map(Value::String).unwrap_or(Value::Null),
        "notify_email_to": notify_email_to.map(String::from).map(Value::String).unwrap_or(Value::Null),
        "xhs_group": xhs_group.map(str::trim).filter(|s| !s.is_empty()).map(String::from).map(Value::String).unwrap_or(Value::Null),
        "wechat_notify": wechat_notify,
        // 锁票
        "auto_lock": auto_lock,
        "imax_only": imax_only,
        "lock_num_seats": lock_num_seats,
        "lock_max_retries": lock_max_retries,
        "lock_seats": lock_seats.to_vec(),
        "enabled": true,
        "presale_fired": false,
        "created_at": chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
    });
    // 幂等兜底：即便漏字段也补齐（含 lock_ok_cinemas/lock_tries 运行状态）
    let mut watch = watch;
    settle_lock_defaults(&mut watch);
    cfg.get_mut("watches")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| anyhow!("config 缺少 watches[]"))?
        .push(watch);
    save(cfg)?;
    Ok(watch_id)
}

pub fn remove_watch(cfg: &mut Value, watch_id: &str) -> Result<bool> {
    let before = cfg
        .get("watches")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    if let Some(arr) = cfg.get_mut("watches").and_then(|v| v.as_array_mut()) {
        arr.retain(|w| w.get("id").and_then(|v| v.as_str()) != Some(watch_id));
    }
    let after = cfg
        .get("watches")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    save(cfg)?;
    Ok(after < before)
}

pub fn mark_presale_fired(cfg: &mut Value, watch_id: &str, cinema_id: &str) -> Result<()> {
    if let Some(w) = find_watch_mut(cfg, watch_id) {
        w["presale_fired"] = json!(true);
        // fired_cinemas 数组：去重 push
        if w.get("fired_cinemas").is_none() {
            w["fired_cinemas"] = json!([]);
        }
        let arr = w
            .get_mut("fired_cinemas")
            .and_then(|v| v.as_array_mut())
            .expect("fired_cinemas 数组");
        let already = arr.iter().any(|x| x.as_str() == Some(cinema_id));
        if !already {
            arr.push(Value::String(cinema_id.to_string()));
        }
        w["last_alert_at"] = json!(chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string());
    }
    save(cfg)?;
    Ok(())
}

// ----------------- 增场监控基线 -----------------

/// 读某 watch 在某影院已记录的场次 `seqNo` 基线。
///
/// 返回 `None` 表示**从未建立过基线**（该 watch 刚建/刚切到增场模式），调用方应当
/// 静默建线而不是把当前所有场次都当成"新增"。返回 `Some(空集)` 表示建过线但当时
/// 一场都没有，此时任何场次都是真新增。
pub fn seen_shows(watch: &Value, cinema_id: &str) -> Option<std::collections::BTreeSet<String>> {
    let arr = watch
        .get("seen_shows")
        .and_then(|v| v.as_object())?
        .get(cinema_id)?
        .as_array()?;
    Some(
        arr.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
    )
}

/// 覆写某 watch 某影院的 `seqNo` 基线。返回是否真的发生了变化 —— 调用方据此决定
/// 是否落盘，避免每轮 tick（默认 90s）都无谓地重写 config.json。本函数**不** save。
pub fn set_seen_shows(
    cfg: &mut Value,
    watch_id: &str,
    cinema_id: &str,
    seqs: &std::collections::BTreeSet<String>,
) -> bool {
    let Some(w) = find_watch_mut(cfg, watch_id) else {
        return false;
    };
    let changed = match seen_shows(w, cinema_id) {
        Some(old) => &old != seqs,
        None => true,
    };
    if !changed {
        return false;
    }
    if !w.get("seen_shows").map(|v| v.is_object()).unwrap_or(false) {
        w["seen_shows"] = json!({});
    }
    let list: Vec<Value> = seqs.iter().cloned().map(Value::String).collect();
    w["seen_shows"][cinema_id] = Value::Array(list);
    true
}

/// 增场提醒发出后打时间戳（增场模式不写 `fired_cinemas`，也不自动停用）。
pub fn touch_last_alert(cfg: &mut Value, watch_id: &str) {
    if let Some(w) = find_watch_mut(cfg, watch_id) {
        w["last_alert_at"] = json!(chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string());
    }
}

// ----------------- 自动锁票：读档 -----------------

/// 全局：锁票时浏览器是否无头。默认 true（无人值守时无头稳定）。
pub fn global_lock_headless(cfg: &Value) -> bool {
    cfg.get("lock_headless").and_then(|v| v.as_bool()).unwrap_or(true)
}

/// 每个 watch 是否启用自动锁票（自动锁票只看 watch 级开关，无全局总闸）。
pub fn watch_auto_lock(watch: &Value) -> bool {
    watch.get("auto_lock").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// 只看 IMAX 厅（纯监测过滤；同时约束锁票候选，无需先开 auto_lock）。
pub fn watch_imax_only(watch: &Value) -> bool {
    watch.get("imax_only").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// 锁票票数（booker `--num-seats`）。
pub fn watch_lock_num_seats(watch: &Value) -> u64 {
    watch
        .get("lock_num_seats")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_LOCK_NUM_SEATS)
}

/// 同一影院的锁票重试上限。
pub fn watch_lock_max_retries(watch: &Value) -> u64 {
    watch
        .get("lock_max_retries")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_LOCK_MAX_RETRIES)
}

/// 手动指定的座位列表（booker `--seat "X排Y座"`，可多条）。空 = 用 num_seats 让 booker 智能选座。
pub fn watch_lock_seats(watch: &Value) -> Vec<String> {
    watch
        .get("lock_seats")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

// ----------------- 自动锁票：状态（运行期写入） -----------------

/// 已锁成功的影院列表（按 `LockResult::Ok` 记录）。
pub fn lock_ok_cinemas(watch: &Value) -> Vec<String> {
    watch
        .get("lock_ok_cinemas")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

pub fn cinema_lock_ok(watch: &Value, cinema_id: &str) -> bool {
    lock_ok_cinemas(watch).iter().any(|c| c == cinema_id)
}

/// 某影院已尝试锁票的次数。
pub fn cinema_lock_tries(watch: &Value, cinema_id: &str) -> u64 {
    watch
        .get("lock_tries")
        .and_then(|v| v.get(cinema_id))
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}

/// 某影院的锁票尝试是否已耗尽重试上限。
pub fn cinema_lock_exhausted(watch: &Value, cinema_id: &str) -> bool {
    cinema_lock_tries(watch, cinema_id) >= watch_lock_max_retries(watch)
}

/// 锁票成功后记录：加入 lock_ok_cinemas（去重），并清掉该影院的失败计数。落盘。
pub fn mark_lock_ok(cfg: &mut Value, watch_id: &str, cinema_id: &str) -> Result<()> {
    mark_lock_ok_inner(cfg, watch_id, cinema_id);
    save(cfg)?;
    Ok(())
}

fn mark_lock_ok_inner(cfg: &mut Value, watch_id: &str, cinema_id: &str) {
    if let Some(w) = find_watch_mut(cfg, watch_id) {
        if !cinema_lock_ok(w, cinema_id) {
            let arr = w
                .get_mut("lock_ok_cinemas")
                .and_then(|v| v.as_array_mut())
                .expect("lock_ok_cinemas 数组");
            arr.push(Value::String(cinema_id.to_string()));
        }
        if let Some(tries) = w.get_mut("lock_tries").and_then(|v| v.as_object_mut()) {
            tries.remove(cinema_id);
        }
    }
}

/// 一次锁票失败后记录（不论失败原因），达上限后该影院不再尝试。落盘。
pub fn incr_lock_tries(cfg: &mut Value, watch_id: &str, cinema_id: &str) -> Result<()> {
    incr_lock_tries_inner(cfg, watch_id, cinema_id);
    save(cfg)?;
    Ok(())
}

fn incr_lock_tries_inner(cfg: &mut Value, watch_id: &str, cinema_id: &str) {
    if let Some(w) = find_watch_mut(cfg, watch_id) {
        let cur = cinema_lock_tries(w, cinema_id);
        if !w.get("lock_tries").map(|v| v.is_object()).unwrap_or(false) {
            w["lock_tries"] = json!({});
        }
        w["lock_tries"][cinema_id] = json!(cur + 1);
    }
}

/// 判定编辑 watch 时，"匹配范围"相关字段是否发生变化。
///
/// 一旦匹配范围变了，旧的 `lock_ok_cinemas` / `lock_tries` 就不再可信——
/// 上次锁的那场可能根本不在新范围里（比如日期改了），或新范围里出现了
/// 之前没考虑的影院。调用方应据此清空锁票状态让 monitor 重新评估。
///
/// 触发字段（任一变化即返回 true）：
///   - `cinemas`     影院列表
///   - `dates`       限定日期（null/[] 都视为无过滤，归一化后比较）
///   - `time_window` 监控时段窗口
///   - `imax_only`   是否只看 IMAX
///   - `movie_id`    监控的电影本身变了，旧锁完全无关
///
/// 不触发的字段（保留状态）：
///   - `mode`                仅影响通知逻辑（开售提醒 vs 增场监控），不影响锁范围
///   - `auto_lock` 开关本身   toggle 不重锁（设计原则：开关即真锁）
///   - 锁参数 (`lock_num_seats` / `lock_seats` / `lock_max_retries`)
///                          锁是历史事实，参数改了只影响下一场
///   - 通知字段 / `interval`
pub fn lock_keys_changed(old: &Value, new: &Value) -> bool {
    fn str_list(v: Option<&Value>) -> Vec<String> {
        v.and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }
    if str_list(old.get("cinemas")) != str_list(new.get("cinemas")) {
        return true;
    }
    if str_list(old.get("dates")) != str_list(new.get("dates")) {
        return true;
    }
    let tw_old = old
        .get("time_window")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tw_new = new
        .get("time_window")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if tw_old != tw_new {
        return true;
    }
    let imax_old = old.get("imax_only").and_then(|v| v.as_bool()).unwrap_or(false);
    let imax_new = new.get("imax_only").and_then(|v| v.as_bool()).unwrap_or(false);
    if imax_old != imax_new {
        return true;
    }
    let movie_old = old.get("movie_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let movie_new = new.get("movie_id").and_then(|v| v.as_i64()).unwrap_or(0);
    if movie_old != movie_new {
        return true;
    }
    false
}

/// 清空 watch 的锁票运行状态（`lock_ok_cinemas` + `lock_tries`）。
/// 返回清掉前 `lock_ok_cinemas` 的长度，供 UI 在事件栏 push「释放 N 个影院」文案。
///
/// 注意：本函数不 save，由调用方（编辑表单提交）统一落盘。
pub fn clear_lock_state(watch: &mut Value) -> usize {
    let cleared = lock_ok_cinemas(watch).len();
    if let Some(o) = watch.as_object_mut() {
        o.insert("lock_ok_cinemas".into(), json!([]));
        o.insert("lock_tries".into(), json!({}));
    }
    cleared
}

/// 一个 watch 是否所有影院都已「锁成功」或「重试耗尽」。空影院列表返回 false，
/// 避免误停用只有空列表的 watch。
pub fn all_cinemas_lock_settled(watch: &Value, cinemas: &[String]) -> bool {
    !cinemas.is_empty()
        && cinemas
            .iter()
            .all(|c| cinema_lock_ok(watch, c) || cinema_lock_exhausted(watch, c))
}

// ----------------- 运行期 -----------------

pub fn set_runtime(cfg: &mut Value, started_at: f64) {
    let obj = cfg
        .get_mut("_runtime")
        .and_then(|v| v.as_object_mut())
        .unwrap();
    obj.insert("started_at".into(), json!(started_at));
    let _ = save(cfg);
}

// ----------------- 类型别名（外部使用便捷） -----------------

/// `Config` 实际就是 `serde_json::Value`；类型别名仅为语义可读。
pub type Config = Value;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn cfg_with_watch(watch: Value) -> Value {
        json!({ "watches": [watch] })
    }

    #[test]
    fn watch_mode_defaults_to_presale_for_legacy_watches() {
        // 老配置里根本没有 mode 字段，必须按开票提醒处理，行为不能变
        assert_eq!(watch_mode(&json!({ "id": "w_1" })), MODE_PRESALE);
        assert_eq!(watch_mode(&json!({ "mode": "乱写" })), MODE_PRESALE);
        assert_eq!(
            watch_mode(&json!({ "mode": "incremental" })),
            MODE_INCREMENTAL
        );
    }

    /// 这是整个增场逻辑最关键的区分：
    /// "从没建过线"（None）要静默建线，"建过线但当时 0 场"（空集）要照常报新增。
    #[test]
    fn seen_shows_distinguishes_never_baselined_from_empty_baseline() {
        assert_eq!(seen_shows(&json!({ "id": "w_1" }), "37534"), None);
        // seen_shows 存在但没有这个影院 → 同样算没建过线
        assert_eq!(
            seen_shows(&json!({ "seen_shows": { "999": [] } }), "37534"),
            None
        );
        assert_eq!(
            seen_shows(&json!({ "seen_shows": { "37534": [] } }), "37534"),
            Some(BTreeSet::new())
        );
        assert_eq!(
            seen_shows(&json!({ "seen_shows": { "37534": ["a"] } }), "37534"),
            Some(set(&["a"]))
        );
    }

    #[test]
    fn set_seen_shows_reports_no_change_when_identical() {
        let mut cfg = cfg_with_watch(json!({
            "id": "w_1",
            "seen_shows": { "37534": ["202608090008709"] }
        }));
        assert!(!set_seen_shows(
            &mut cfg,
            "w_1",
            "37534",
            &set(&["202608090008709"])
        ));
    }

    #[test]
    fn set_seen_shows_creates_baseline_on_first_call() {
        let mut cfg = cfg_with_watch(json!({ "id": "w_1" }));
        assert!(set_seen_shows(&mut cfg, "w_1", "37534", &set(&["a", "b"])));
        let w = find_watch(&cfg, "w_1").unwrap();
        assert_eq!(seen_shows(w, "37534"), Some(set(&["a", "b"])));
    }

    /// 场次撤掉后要同步移出基线，否则同一个 seqNo 复排时会被当成老场次而漏报。
    #[test]
    fn set_seen_shows_prunes_disappeared_showtimes() {
        let mut cfg = cfg_with_watch(json!({
            "id": "w_1",
            "seen_shows": { "37534": ["a", "b"] }
        }));
        assert!(set_seen_shows(&mut cfg, "w_1", "37534", &set(&["b"])));
        let w = find_watch(&cfg, "w_1").unwrap();
        assert_eq!(seen_shows(w, "37534"), Some(set(&["b"])));
    }

    #[test]
    fn set_seen_shows_keeps_other_cinemas_untouched() {
        let mut cfg = cfg_with_watch(json!({
            "id": "w_1",
            "seen_shows": { "37534": ["a"], "42020": ["z"] }
        }));
        set_seen_shows(&mut cfg, "w_1", "37534", &set(&["a", "new"]));
        let w = find_watch(&cfg, "w_1").unwrap();
        assert_eq!(seen_shows(w, "42020"), Some(set(&["z"])));
    }

    fn watch_with_tw(tw: &str) -> Value {
        json!({ "id": "w_1", "time_window": tw })
    }

    #[test]
    fn watch_time_window_parses_or_returns_none() {
        assert_eq!(watch_time_window(&watch_with_tw("19:00-22:00")), Some((19, 0, 22, 0)));
        assert_eq!(watch_time_window(&watch_with_tw("")), None);
        assert_eq!(watch_time_window(&watch_with_tw("  ")), None);
        assert_eq!(watch_time_window(&json!({})), None);
        assert_eq!(watch_time_window(&json!({"time_window": null})), None);
        assert_eq!(watch_time_window(&watch_with_tw("乱写")), None); // 解析失败容错
    }

    /// `in_time_window` 是真正的过滤函数：无窗口 / 解析失败 → 全放行（向后兼容）
    #[test]
    fn in_time_window_open_when_unset_or_malformed() {
        let w_no = json!({});
        for tm in ["00:00", "12:00", "23:59"] {
            assert!(in_time_window(&w_no, tm));
        }
        let w_bad = watch_with_tw("乱写");
        assert!(in_time_window(&w_bad, "12:00"));
        let w_bad_tm = json!({});
        assert!(in_time_window(&w_bad_tm, "not-a-time"));
    }

    #[test]
    fn in_time_window_inclusive_lo_exclusive_hi() {
        let w = watch_with_tw("19:00-22:00");
        assert!(in_time_window(&w, "19:00"));  // 起点包含
        assert!(in_time_window(&w, "21:59"));
        assert!(!in_time_window(&w, "22:00")); // 终点不含（与 Python range 一致）
        assert!(!in_time_window(&w, "18:59"));
        assert!(!in_time_window(&w, "23:00"));
    }

    // ---------- 自动锁票 ----------

    #[test]
    fn global_lock_defaults_headless() {
        assert!(global_lock_headless(&json!({})));
        assert!(global_lock_headless(&json!({ "lock_headless": false })) == false);
        assert!(global_lock_headless(&json!({ "lock_headless": true })));
    }

    #[test]
    fn watch_lock_defaults_for_legacy_or_out_of_range() {
        assert!(!watch_auto_lock(&json!({})));
        assert!(!watch_imax_only(&json!({})));
        assert_eq!(watch_lock_num_seats(&json!({})), DEFAULT_LOCK_NUM_SEATS);
        assert_eq!(watch_lock_num_seats(&json!({ "lock_num_seats": 0 })), 0);
        assert_eq!(watch_lock_max_retries(&json!({})), DEFAULT_LOCK_MAX_RETRIES);
        assert_eq!(watch_lock_seats(&json!({})), Vec::<String>::new());
        assert_eq!(watch_lock_seats(&json!({ "lock_seats": ["5排6座", "5排7座"] })), vec!["5排6座", "5排7座"]);
    }

    #[test]
    fn lock_state_tracks_ok_and_tries() {
        let mut cfg = cfg_with_watch(json!({
            "id": "w_1",
            "lock_max_retries": 3,
            "lock_ok_cinemas": [],
            "lock_tries": {},
        }));
        assert!(!cinema_lock_ok(find_watch(&cfg, "w_1").unwrap(), "37534"));
        assert_eq!(cinema_lock_tries(find_watch(&cfg, "w_1").unwrap(), "37534"), 0);

        incr_lock_tries_inner(&mut cfg, "w_1", "37534");
        incr_lock_tries_inner(&mut cfg, "w_1", "37534");
        let w = find_watch(&cfg, "w_1").unwrap();
        assert_eq!(cinema_lock_tries(w, "37534"), 2);
        assert!(!cinema_lock_exhausted(w, "37534"));

        incr_lock_tries_inner(&mut cfg, "w_1", "37534");
        let w = find_watch(&cfg, "w_1").unwrap();
        assert!(cinema_lock_exhausted(w, "37534"));

        // 成功后：标记 ok + 清掉计数
        mark_lock_ok_inner(&mut cfg, "w_1", "37534");
        let w = find_watch(&cfg, "w_1").unwrap();
        assert!(cinema_lock_ok(w, "37534"));
        assert!(cinema_lock_exhausted(w, "37534") == false); // 清掉计数后不再 exhausted
        assert_eq!(cinema_lock_tries(w, "37534"), 0);
    }

    #[test]
    fn all_cinemas_lock_settled_requires_all_done() {
        let mut cfg = cfg_with_watch(json!({
            "id": "w_1",
            "lock_max_retries": 2,
            "lock_ok_cinemas": [],
            "lock_tries": {},
        }));
        let cinemas = vec!["37534".to_string(), "42020".to_string()];
        // 全未处理
        assert!(!all_cinemas_lock_settled(find_watch(&cfg, "w_1").unwrap(), &cinemas));
        // 空列表永不 settled（防误停用）
        assert!(!all_cinemas_lock_settled(find_watch(&cfg, "w_1").unwrap(), &[]));
        // 一家 ok、一家耗尽 → settled
        mark_lock_ok_inner(&mut cfg, "w_1", "37534");
        incr_lock_tries_inner(&mut cfg, "w_1", "42020");
        incr_lock_tries_inner(&mut cfg, "w_1", "42020");
        assert!(all_cinemas_lock_settled(find_watch(&cfg, "w_1").unwrap(), &cinemas));
    }

    #[test]
    fn settle_lock_defaults_fills_missing_fields_idempotently() {
        let mut w = json!({ "id": "w_1" });
        settle_lock_defaults(&mut w);
        assert_eq!(w["auto_lock"], json!(false));
        assert_eq!(w["imax_only"], json!(false));
        assert_eq!(w["lock_num_seats"], json!(DEFAULT_LOCK_NUM_SEATS));
        assert_eq!(w["lock_max_retries"], json!(DEFAULT_LOCK_MAX_RETRIES));
        assert_eq!(w["lock_seats"], json!([]));
        assert_eq!(w["lock_ok_cinemas"], json!([]));
        assert_eq!(w["lock_tries"], json!({}));
        // 已存在的值不被覆盖
        let mut w2 = json!({ "id": "w_2", "lock_num_seats": 5, "auto_lock": true });
        settle_lock_defaults(&mut w2);
        assert_eq!(w2["lock_num_seats"], json!(5));
        assert_eq!(w2["auto_lock"], json!(true));
    }

    /// 编辑 watch 时，"匹配范围"变化应当触发锁状态清空。
    /// 一次覆盖五个触发字段：cinemas / dates / time_window / imax_only / movie_id。
    fn w(cinemas: &[&str], dates: Option<&[&str]>, tw: Option<&str>, imax: bool, movie: i64) -> Value {
        let mut v = json!({
            "id": "w_1",
            "cinemas": cinemas,
            "imax_only": imax,
            "movie_id": movie,
        });
        if let Some(d) = dates {
            v["dates"] = json!(d);
        }
        if let Some(t) = tw {
            v["time_window"] = json!(t);
        }
        v
    }

    #[test]
    fn lock_keys_changed_triggers_on_each_range_field() {
        let base = w(&["37534"], Some(&["2026-08-31"]), Some("19:00-22:00"), true, 1545360);

        // cinemas 增/减/换 都触发
        assert!(lock_keys_changed(&base, &w(&["42020"], Some(&["2026-08-31"]), Some("19:00-22:00"), true, 1545360)));
        assert!(lock_keys_changed(&base, &w(&["37534", "42020"], Some(&["2026-08-31"]), Some("19:00-22:00"), true, 1545360)));

        // dates 改：换日期、清空（[]）、从 null 改成有限定
        assert!(lock_keys_changed(&base, &w(&["37534"], Some(&["2026-09-01"]), Some("19:00-22:00"), true, 1545360)));
        assert!(lock_keys_changed(&base, &w(&["37534"], Some(&[]), Some("19:00-22:00"), true, 1545360)));
        assert!(lock_keys_changed(&base, &w(&["37534"], None, Some("19:00-22:00"), true, 1545360)));

        // time_window 改
        assert!(lock_keys_changed(&base, &w(&["37534"], Some(&["2026-08-31"]), Some("20:00-22:00"), true, 1545360)));
        assert!(lock_keys_changed(&base, &w(&["37534"], Some(&["2026-08-31"]), None, true, 1545360)));

        // imax_only toggle
        assert!(lock_keys_changed(&base, &w(&["37534"], Some(&["2026-08-31"]), Some("19:00-22:00"), false, 1545360)));

        // movie_id 改
        assert!(lock_keys_changed(&base, &w(&["37534"], Some(&["2026-08-31"]), Some("19:00-22:00"), true, 9999999)));
    }

    #[test]
    fn lock_keys_changed_ignores_unrelated_fields() {
        let base = w(&["37534"], Some(&["2026-08-31"]), Some("19:00-22:00"), true, 1545360);
        let mut same = base.clone();

        // 通知字段：全不算
        same["wechat_notify"] = json!(true);
        same["xhs_group"] = json!("another");
        same["notify_webhook"] = json!("https://example/hook");
        same["notify_email_to"] = json!("a@b.com");
        // 锁参数：toggle 不算
        same["auto_lock"] = json!(false);
        same["lock_num_seats"] = json!(4);
        same["lock_max_retries"] = json!(5);
        same["lock_seats"] = json!(["5排6座", "7排8座"]);
        // 节奏字段
        same["mode"] = json!("presale");
        same["interval"] = json!(30);
        assert!(!lock_keys_changed(&base, &same), "无关字段全部改了也应返回 false");

        // dates null vs [] 视为相同（都表示"不限"）
        let null_dates = json!({
            "id": "w_1", "cinemas": ["37534"], "dates": null, "time_window": "19:00-22:00",
            "imax_only": true, "movie_id": 1545360,
        });
        let empty_dates = json!({
            "id": "w_1", "cinemas": ["37534"], "dates": [], "time_window": "19:00-22:00",
            "imax_only": true, "movie_id": 1545360,
        });
        assert!(!lock_keys_changed(&null_dates, &empty_dates));
    }

    #[test]
    fn clear_lock_state_empties_arrays_and_reports_count() {
        let mut w = json!({
            "id": "w_1",
            "lock_ok_cinemas": ["37534", "42020"],
            "lock_tries": { "37534": 2, "42020": 3 },
        });
        let n = clear_lock_state(&mut w);
        assert_eq!(n, 2);
        assert_eq!(w["lock_ok_cinemas"], json!([]));
        assert_eq!(w["lock_tries"], json!({}));
    }

    #[test]
    fn clear_lock_state_handles_missing_fields_gracefully() {
        let mut w = json!({ "id": "w_1" });
        let n = clear_lock_state(&mut w);
        assert_eq!(n, 0);
        assert_eq!(w["lock_ok_cinemas"], json!([]));
        assert_eq!(w["lock_tries"], json!({}));
    }

    /// 端到端集成：旧 watch 已锁成功 → 编辑匹配范围 → 锁状态被清，
    /// 下一次 tick 不会因 cinema_lock_ok 跳过。
    #[test]
    fn edit_clears_lock_state_when_range_changes() {
        let mut cfg = cfg_with_watch(json!({
            "id": "w_1",
            "cinemas": ["37534"],
            "dates": ["2026-08-31"],
            "time_window": "19:00-22:00",
            "imax_only": true,
            "movie_id": 1545360,
            "lock_ok_cinemas": ["37534"],
            "lock_tries": {},
            "seen_shows": { "37534": ["202608310023070"] },
        }));

        // 模拟"编辑把 dates 从 8/31 改成 9/1"——匹配范围变化
        let new_dates = json!(["2026-09-01"]);
        let w_ref = find_watch(&cfg, "w_1").unwrap();
        let new_snapshot = {
            let mut snap = w_ref.clone();
            snap["dates"] = new_dates;
            snap
        };
        assert!(lock_keys_changed(w_ref, &new_snapshot));

        // 真实写回 cfg（按 submit_edit_watch 的做法）
        let w = find_watch_mut(&mut cfg, "w_1").unwrap();
        w["dates"] = json!(["2026-09-01"]);
        let cleared = clear_lock_state(w);
        assert_eq!(cleared, 1);

        let w = find_watch(&cfg, "w_1").unwrap();
        assert!(!cinema_lock_ok(w, "37534"), "锁状态必须被清空");
        assert_eq!(w["lock_ok_cinemas"], json!([]));
    }
}
