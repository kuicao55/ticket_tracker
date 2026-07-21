//! `tt config show | get | set | unset | path`：配置子命令 + 别名。

use anyhow::{anyhow, Result};
use clap::Subcommand;
use serde_json::Value;

use crate::{config, paths};

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    Show,
    Get { key: String },
    Set { key: String, value: String },
    Unset { key: String },
    Path,
}

pub fn dispatch(a: ConfigAction) -> Result<()> {
    match a {
        ConfigAction::Show => show(),
        ConfigAction::Get { key } => get(&key),
        ConfigAction::Set { key, value } => set(&key, &value),
        ConfigAction::Unset { key } => unset(&key),
        ConfigAction::Path => {
            println!("{}", paths::config_file().display());
            Ok(())
        }
    }
}

fn resolve_key(k: &str) -> &str {
    match k {
        "discord-webhook" | "webhook" => "discord_webhook",
        "quiet" => "quiet_window",
        "phone-only" => "phone_only_window",
        "interval" => "check_interval",
        other => other,
    }
}

fn show() -> Result<()> {
    let cfg = config::load_or_init()?;
    println!("{}", serde_json::to_string_pretty(&cfg)?);
    Ok(())
}

fn get(key: &str) -> Result<()> {
    let cfg = config::load_or_init()?;
    let k = resolve_key(key);
    match cfg.get(k) {
        Some(v) => {
            println!("{}", render(v));
            Ok(())
        }
        None => Err(anyhow!("未配置: {}", k)),
    }
}

fn render(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

fn parse_scalar(v: &str) -> Value {
    match v.to_lowercase().as_str() {
        "null" | "none" | "" => Value::Null,
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => {
            if let Ok(n) = v.parse::<i64>() {
                Value::Number(n.into())
            } else {
                Value::String(v.to_string())
            }
        }
    }
}

fn set(key: &str, value: &str) -> Result<()> {
    let mut cfg = config::load_or_init()?;
    let k = resolve_key(key);
    cfg[k] = parse_scalar(value);
    config::save(&cfg)?;
    println!("✓ {} = {}", k, value);
    Ok(())
}

fn unset(key: &str) -> Result<()> {
    let mut cfg = config::load_or_init()?;
    let k = resolve_key(key);
    if let Some(obj) = cfg.as_object_mut() {
        obj.remove(k);
    }
    config::save(&cfg)?;
    println!("✓ 已删除: {}", k);
    Ok(())
}
