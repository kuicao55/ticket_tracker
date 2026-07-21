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
        "_runtime": {},
    })
}

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
) -> Result<String> {
    let cinemas_v: Vec<String> = cinemas.iter().map(|s| s.to_string()).collect();
    for cid in &cinemas_v {
        if find_cinema(cfg, cid).is_none() {
            add_cinema(cfg, cid, None)?;
        }
    }
    let watch_id = format!("w_{}", &Uuid::new_v4().to_string()[..6]);
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
        "enabled": true,
        "presale_fired": false,
        "created_at": chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
    });
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
