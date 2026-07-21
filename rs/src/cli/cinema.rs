//! `tt cinema ...`：cinema 子命令（list / add / remove / presets / add-preset）。

use anyhow::{anyhow, Result};
use clap::Subcommand;

use crate::{config, presets};

#[derive(Subcommand, Debug)]
pub enum CinemaAction {
    List,
    Add {
        cinema_id: String,
        #[arg(long = "name")]
        name: Option<String>,
    },
    Remove { cinema_id: String },
    Presets,
    AddPreset { name: String },
}

pub fn dispatch(a: CinemaAction) -> Result<()> {
    match a {
        CinemaAction::List => list(),
        CinemaAction::Add { cinema_id, name } => add(&cinema_id, name.as_deref()),
        CinemaAction::Remove { cinema_id } => remove(&cinema_id),
        CinemaAction::Presets => presets_list(),
        CinemaAction::AddPreset { name } => add_preset(&name),
    }
}

fn list() -> Result<()> {
    let cfg = config::load_or_init()?;
    let arr = cfg.get("cinemas").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if arr.is_empty() {
        println!("(无影院)");
        return Ok(());
    }
    println!("{:<10} {}", "ID", "NAME");
    for c in arr {
        let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        println!("{:<10} {}", id, name);
    }
    Ok(())
}

fn add(cinema_id: &str, name: Option<&str>) -> Result<()> {
    let mut cfg = config::load_or_init()?;
    if config::add_cinema(&mut cfg, cinema_id, name)? {
        println!("✓ 已添加影院 {} ({})", cinema_id, name.unwrap_or(""));
    } else {
        println!("已存在: {}", cinema_id);
    }
    Ok(())
}

fn remove(cinema_id: &str) -> Result<()> {
    let mut cfg = config::load_or_init()?;
    if config::remove_cinema(&mut cfg, cinema_id)? {
        println!("✓ 已删除: {}", cinema_id);
    } else {
        return Err(anyhow!("影院不存在: {}", cinema_id));
    }
    Ok(())
}

fn presets_list() -> Result<()> {
    println!("{:<20} {:<10} {:<6} {}", "名称", "ID", "城市", "说明");
    for (k, v) in presets::list_presets() {
        println!("{:<20} {:<10} {:<6} {}", k, v.id, v.city, v.note);
    }
    Ok(())
}

fn add_preset(name: &str) -> Result<()> {
    let p = presets::get_preset(name).ok_or_else(|| anyhow!("预设不存在: {}", name))?;
    let mut cfg = config::load_or_init()?;
    if config::add_cinema(&mut cfg, p.id, Some(p.name))? {
        println!("✓ 已添加预设: {} ({})", name, p.id);
    } else {
        println!("已存在: {}", p.id);
    }
    Ok(())
}
