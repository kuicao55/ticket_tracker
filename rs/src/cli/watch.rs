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
        } => add(movie_id, &cinema, &date, name.as_deref(), interval),
        WatchAction::Show { id } => show(&id),
        WatchAction::Edit {
            id,
            cinema,
            date,
            interval,
            name,
        } => edit(&id, &cinema, &date, interval, name.as_deref()),
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
) -> Result<()> {
    let mut cfg = config::load_or_init()?;
    let cinemas_ref: Vec<&str> = cinemas.iter().map(|s| s.as_str()).collect();
    let id = config::add_watch(
        &mut cfg,
        movie_id,
        &cinemas_ref,
        if dates.is_empty() { None } else { Some(dates) },
        name,
        interval,
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

fn edit(id: &str, cinemas: &[String], dates: &[String], interval: Option<u64>, name: Option<&str>) -> Result<()> {
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
