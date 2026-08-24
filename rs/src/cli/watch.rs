//! `tt watch ...`：watch 子命令（list / add / show / edit / remove / enable / disable）。

use anyhow::{anyhow, Result};
use clap::Subcommand;

use crate::{config, presets};

#[derive(Subcommand, Debug)]
pub enum WatchAction {
    List,
    Add {
        movie_id: i64,
        #[arg(short = 'c', long = "cinema")]
        cinema: Vec<String>,
        #[arg(short = 'd', long = "date")]
        date: Vec<String>,
        #[arg(long = "name")]
        name: Option<String>,
        #[arg(long = "interval")]
        interval: Option<u64>,
        /// presale=首次开售即报（默认）；incremental=持续监控新增场次
        #[arg(long = "mode", default_value = config::MODE_PRESALE)]
        mode: String,
        /// 时段窗口（HH:MM-HH:MM），只监控此时间段内的场次
        #[arg(long = "time-window")]
        time_window: Option<String>,
        /// 仅告警时触发的结果通知 webhook（区别于全局 discord_webhook：不含心跳）
        #[arg(long = "notify-webhook")]
        notify_webhook: Option<String>,
        /// 仅告警时触发的结果通知邮箱（通过本地 msmtp 发）
        #[arg(long = "notify-email")]
        notify_email: Option<String>,
        /// 仅告警时触发的小红书群聊通知（群名，留空 = 关闭）
        #[arg(long = "xhs-group")]
        xhs_group: Option<String>,
        /// 仅告警时触发微信大群通知（发到当前打开的微信会话，开/关）
        #[arg(long = "wechat-notify", default_missing_value = "true", num_args = 0..=1)]
        wechat_notify: Option<bool>,
        /// 本 watch 启用自动锁票（无全局总闸，只由本 watch 开关控制）
        #[arg(long = "auto-lock")]
        auto_lock: bool,
        /// 只监控 IMAX 厅（监测与锁票共用一个开关：过滤监控场次，锁票候选随之）
        #[arg(long = "imax-only")]
        imax_only: bool,
        /// 锁票票数（booker --num-seats；1-6）
        #[arg(long = "lock-num-seats", default_value_t = config::DEFAULT_LOCK_NUM_SEATS)]
        lock_num_seats: u64,
        /// 同一影院锁票重试上限
        #[arg(long = "lock-max-retries", default_value_t = config::DEFAULT_LOCK_MAX_RETRIES)]
        lock_max_retries: u64,
        /// 手动指定座位（booker --seat，可多次，如 "5排6座"）；设置后覆盖智能选座
        #[arg(long = "lock-seat")]
        lock_seats: Vec<String>,
    },
    Show { id: String },
    Edit {
        id: String,
        #[arg(short = 'c', long = "cinema")]
        cinema: Vec<String>,
        #[arg(short = 'd', long = "date")]
        date: Vec<String>,
        #[arg(long = "interval")]
        interval: Option<u64>,
        #[arg(long = "name")]
        name: Option<String>,
        /// presale / incremental
        #[arg(long = "mode")]
        mode: Option<String>,
        /// 时段窗口；留空字符串 `""` 表示清空；不传则不改
        #[arg(long = "time-window", allow_hyphen_values = true)]
        time_window: Option<String>,
        /// 留空字符串 `""` 表示清空；不传则不改
        #[arg(long = "notify-webhook", allow_hyphen_values = true)]
        notify_webhook: Option<String>,
        /// 留空字符串 `""` 表示清空；不传则不改
        #[arg(long = "notify-email", allow_hyphen_values = true)]
        notify_email: Option<String>,
        /// 留空字符串 `""` 表示清空；不传则不改
        #[arg(long = "xhs-group", allow_hyphen_values = true)]
        xhs_group: Option<String>,
        /// 微信大群通知开关：`--wechat-notify` 启用 / `--wechat-notify false` 关闭 / 不传则不改
        #[arg(long = "wechat-notify", default_missing_value = "true", num_args = 0..=1)]
        wechat_notify: Option<bool>,
        /// 自动锁票开关：`--auto-lock` 启用 / `--auto-lock false` 关闭 / 不传则不改
        #[arg(long = "auto-lock", default_missing_value = "true", num_args = 0..=1)]
        auto_lock: Option<bool>,
        /// 只监控 IMAX 厅（监测与锁票共用一个开关）
        #[arg(long = "imax-only", default_missing_value = "true", num_args = 0..=1)]
        imax_only: Option<bool>,
        /// 锁票票数
        #[arg(long = "lock-num-seats")]
        lock_num_seats: Option<u64>,
        /// 同一影院锁票重试上限
        #[arg(long = "lock-max-retries")]
        lock_max_retries: Option<u64>,
        /// 手动指定座位（可多次，覆盖智能选座）；传 `--lock-seat ""` 清空
        #[arg(long = "lock-seat", allow_hyphen_values = true)]
        lock_seats: Vec<String>,
    },
    Remove { id: String },
    Enable { id: String },
    Disable { id: String },
}

pub fn dispatch(a: WatchAction) -> Result<()> {
    match a {
        WatchAction::List => list(),
        WatchAction::Add {
            movie_id,
            cinema,
            date,
            name,
            interval,
            mode,
            time_window,
            notify_webhook,
            notify_email,
            xhs_group,
            wechat_notify,
            auto_lock,
            imax_only,
            lock_num_seats,
            lock_max_retries,
            lock_seats,
        } => add(
            movie_id,
            &cinema,
            &date,
            name.as_deref(),
            interval,
            &mode,
            time_window.as_deref(),
            notify_webhook.as_deref(),
            notify_email.as_deref(),
            xhs_group.as_deref(),
            wechat_notify.unwrap_or(false),
            auto_lock,
            imax_only,
            lock_num_seats,
            lock_max_retries,
            &lock_seats,
        ),
        WatchAction::Show { id } => show(&id),
        WatchAction::Edit {
            id,
            cinema,
            date,
            interval,
            name,
            mode,
            time_window,
            notify_webhook,
            notify_email,
            xhs_group,
            wechat_notify,
            auto_lock,
            imax_only,
            lock_num_seats,
            lock_max_retries,
            lock_seats,
        } => edit(
            &id,
            &cinema,
            &date,
            interval,
            name.as_deref(),
            mode.as_deref(),
            time_window.as_deref(),
            notify_webhook.as_deref(),
            notify_email.as_deref(),
            xhs_group.as_deref(),
            wechat_notify,
            auto_lock,
            imax_only,
            lock_num_seats,
            lock_max_retries,
            &lock_seats,
        ),
        WatchAction::Remove { id } => remove(&id),
        WatchAction::Enable { id } => set_enabled(&id, true),
        WatchAction::Disable { id } => set_enabled(&id, false),
    }
}

fn list() -> Result<()> {
    let cfg = config::load_or_init()?;
    let watches = config::list_watches(&cfg);
    if watches.is_empty() {
        println!("(无 watch)");
        return Ok(());
    }
    println!("{:<10} {:<10} {:<20} {}", "ID", "MOVIE", "NAME", "ENABLED");
    for w in watches {
        let id = w.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let mid = w.get("movie_id").and_then(|v| v.as_i64()).unwrap_or(0);
        let name = w.get("movie_name").and_then(|v| v.as_str()).unwrap_or("?");
        let en = w.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
        println!("{:<10} {:<10} {:<20} {}", id, mid, name, if en { "✓" } else { "×" });
    }
    Ok(())
}

fn add(
    movie_id: i64,
    cinemas: &[String],
    dates: &[String],
    name: Option<&str>,
    interval: Option<u64>,
    mode: &str,
    time_window: Option<&str>,
    notify_webhook: Option<&str>,
    notify_email: Option<&str>,
    xhs_group: Option<&str>,
    wechat_notify: bool,
    auto_lock: bool,
    imax_only: bool,
    lock_num_seats: u64,
    lock_max_retries: u64,
    lock_seats: &[String],
) -> Result<()> {
    if mode != config::MODE_PRESALE && mode != config::MODE_INCREMENTAL {
        return Err(anyhow!(
            "--mode 只能是 {} 或 {}",
            config::MODE_PRESALE,
            config::MODE_INCREMENTAL
        ));
    }
    let mut cfg = config::load_or_init()?;
    let cinemas_ref: Vec<&str> = cinemas.iter().map(|s| s.as_str()).collect();
    let id = config::add_watch(
        &mut cfg,
        movie_id,
        &cinemas_ref,
        if dates.is_empty() { None } else { Some(dates) },
        name,
        interval,
        mode,
        time_window,
        notify_webhook,
        notify_email,
        xhs_group,
        wechat_notify,
        auto_lock,
        imax_only,
        lock_num_seats,
        lock_max_retries,
        lock_seats,
    )?;
    println!("✓ 已添加 watch: {}", id);
    Ok(())
}

fn show(id: &str) -> Result<()> {
    let cfg = config::load_or_init()?;
    let w = config::find_watch(&cfg, id).ok_or_else(|| anyhow!("watch 不存在"))?;
    println!("{}", serde_json::to_string_pretty(w)?);
    Ok(())
}

fn edit(
    id: &str,
    cinemas: &[String],
    dates: &[String],
    interval: Option<u64>,
    name: Option<&str>,
    mode: Option<&str>,
    time_window: Option<&str>,
    notify_webhook: Option<&str>,
    notify_email: Option<&str>,
    xhs_group: Option<&str>,
    wechat_notify: Option<bool>,
    auto_lock: Option<bool>,
    imax_only: Option<bool>,
    lock_num_seats: Option<u64>,
    lock_max_retries: Option<u64>,
    lock_seats: &[String],
) -> Result<()> {
    if let Some(m) = mode {
        if m != config::MODE_PRESALE && m != config::MODE_INCREMENTAL {
            return Err(anyhow!(
                "--mode 只能是 {} 或 {}",
                config::MODE_PRESALE,
                config::MODE_INCREMENTAL
            ));
        }
    }
    let mut cfg = config::load_or_init()?;
    let w = config::find_watch(&cfg, id).ok_or_else(|| anyhow!("watch 不存在"))?.clone();
    drop(w);
    let mut updates = serde_json::Map::new();
    if !cinemas.is_empty() {
        for c in cinemas {
            if !config::find_cinema(&cfg, c).is_some() {
                config::add_cinema(&mut cfg, c, None)?;
            }
        }
        updates.insert("cinemas".into(), serde_json::json!(cinemas));
    }
    if !dates.is_empty() {
        let mut v = dates.to_vec();
        v.sort();
        v.dedup();
        updates.insert("dates".into(), serde_json::json!(v));
    }
    if let Some(i) = interval {
        updates.insert("interval".into(), serde_json::json!(i));
    }
    if let Some(n) = name {
        updates.insert("movie_name".into(), serde_json::json!(n));
    }
    if let Some(m) = mode {
        updates.insert("mode".into(), serde_json::json!(m));
    }
    // time_window：同上，区分「没传」和「传了空字符串」
    if let Some(v) = time_window {
        if v.is_empty() {
            updates.insert("time_window".into(), serde_json::Value::Null);
        } else {
            config::parse_window(v)
                .map_err(|e| anyhow!("--time-window 格式错误: {}", e))?;
            updates.insert("time_window".into(), serde_json::json!(v));
        }
    }
    // notify_webhook / notify_email：区分「没传」和「传了空字符串」
    //   - 没传 → 不动
    //   - 传了 "" → 清空（置 null）
    //   - 传了具体值 → 覆盖
    if let Some(v) = notify_webhook {
        if v.is_empty() {
            updates.insert("notify_webhook".into(), serde_json::Value::Null);
        } else {
            updates.insert("notify_webhook".into(), serde_json::json!(v));
        }
    }
    if let Some(v) = notify_email {
        if v.is_empty() {
            updates.insert("notify_email_to".into(), serde_json::Value::Null);
        } else {
            updates.insert("notify_email_to".into(), serde_json::json!(v));
        }
    }
    // xhs_group: 同上,区分「没传」/「传空」/「传具体值」
    if let Some(v) = xhs_group {
        if v.is_empty() {
            updates.insert("xhs_group".into(), serde_json::Value::Null);
        } else {
            updates.insert("xhs_group".into(), serde_json::json!(v));
        }
    }
    // wechat_notify: None=不改, Some(b)=设为 b
    if let Some(v) = wechat_notify {
        updates.insert("wechat_notify".into(), serde_json::json!(v));
    }
    // 锁票字段：None=不改，Some=覆盖
    if let Some(v) = auto_lock {
        updates.insert("auto_lock".into(), serde_json::json!(v));
    }
    if let Some(v) = imax_only {
        updates.insert("imax_only".into(), serde_json::json!(v));
    }
    if let Some(v) = lock_num_seats {
        updates.insert("lock_num_seats".into(), serde_json::json!(v));
    }
    if let Some(v) = lock_max_retries {
        updates.insert("lock_max_retries".into(), serde_json::json!(v));
    }
    if !lock_seats.is_empty() {
        // 显式传空串 = 清空手动座位（回退智能选座）
        let cleared = lock_seats.iter().all(|s| s.is_empty());
        let v: Vec<String> = if cleared {
            Vec::new()
        } else {
            lock_seats.to_vec()
        };
        updates.insert("lock_seats".into(), serde_json::json!(v));
    }
    // 影院/日期/时段/模式一变，旧的 seqNo 基线就对不上了 —— 清空让下一轮重新静默建线，
    // 否则新窗口里已有的场次会被当成"新增"一次性全报出来。
    if updates.contains_key("cinemas")
        || updates.contains_key("dates")
        || updates.contains_key("time_window")
        || updates.contains_key("mode")
    {
        updates.insert("seen_shows".into(), serde_json::json!({}));
    }
    if let Some(w) = config::find_watch_mut(&mut cfg, id) {
        for (k, v) in updates {
            w[k] = v;
        }
    }
    config::save(&cfg)?;
    println!("✓ 已更新 watch: {}", id);
    Ok(())
}

fn remove(id: &str) -> Result<()> {
    let mut cfg = config::load_or_init()?;
    if config::remove_watch(&mut cfg, id)? {
        println!("✓ 已删除: {}", id);
    } else {
        return Err(anyhow!("watch 不存在: {}", id));
    }
    Ok(())
}

fn set_enabled(id: &str, enabled: bool) -> Result<()> {
    let mut cfg = config::load_or_init()?;
    if let Some(w) = config::find_watch_mut(&mut cfg, id) {
        w["enabled"] = serde_json::json!(enabled);
        config::save(&cfg)?;
        println!("✓ {} {}", id, if enabled { "已启用" } else { "已停用" });
        Ok(())
    } else {
        Err(anyhow!("watch 不存在: {}", id))
    }
}

#[allow(dead_code)]
fn _preset_ref() -> &'static [(&'static str, presets::Preset)] {
    presets::PRESETS
}
