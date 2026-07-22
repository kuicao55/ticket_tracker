//! 键盘事件分发。
//!
//! 导航模型：方向键 = 选择；Enter = 进入子内容 / 触发；Esc = 返回上一级
//! - Top 模式：4 个方向键在 4 个区块（Watches / Detail / Logs / Menu）间循环
//!   Enter 进入当前区块（→ In 模式）
//!   Esc / q / Ctrl+C 退出
//! - In 模式：方向键在当前区块的子内容里选择
//!   Enter 触发（或从 Watches 进入 Detail 区块）
//!   Esc 退回 Top 模式
//! - 帮助 / 确认 / 命令行输入模式独立优先

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::actions;
use super::modal;
use super::{App, Focus, FocusMode, InputMode};

pub fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    // 1. help 覆盖层优先生效
    if app.show_help {
        if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?')) {
            app.show_help = false;
        }
        return Ok(());
    }
    // 2. Confirm 提示
    if let Some(c) = app.confirm.clone() {
        if c.created_at.elapsed() > std::time::Duration::from_secs(8) {
            app.confirm = None;
        } else {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    let wid_text = c.text.clone();
                    let wid = extract_wid_from_confirm(&wid_text);
                    app.confirm = None;
                    if let Some(w) = wid {
                        actions::cmd_delete_wid(app, &w);
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    app.confirm = None;
                }
                _ => {}
            }
            return Ok(());
        }
    }
    // 3. modal 表单 / 搜索 / 影院选择器优先于命令行输入
    if app.modal.is_some() {
        modal::handle_key(app, key);
        return Ok(());
    }
    // 4. prompt 期间
    if app.input_mode == InputMode::Cmd {
        return handle_prompt_mode(app, key);
    }
    // 5. 普通导航（Top / In）
    match app.focus_mode {
        FocusMode::Top => handle_top_mode(app, key),
        FocusMode::In => handle_in_mode(app, key),
    }
}

fn extract_wid_from_confirm(text: &str) -> Option<String> {
    let s = text.trim();
    if !s.starts_with("删 watch ") {
        return None;
    }
    let rest = &s["删 watch ".len()..];
    rest.split_whitespace().next().map(String::from)
}

/// Top 模式：4 个方向键都用来选区块（按固定顺序循环）；Enter 进入子内容。
fn handle_top_mode(app: &mut App, key: KeyEvent) -> Result<()> {
    let no_ctrl = !key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => app.request_quit(),
        KeyCode::Char('q') => app.request_quit(),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => app.request_quit(),
        // 4 个方向键都用来在 4 区块间循环选择
        KeyCode::Left | KeyCode::Char('h') if no_ctrl => app.focus = app.focus.prev(),
        KeyCode::Right | KeyCode::Char('l') if no_ctrl => app.focus = app.focus.next(),
        KeyCode::Up | KeyCode::Char('k') if no_ctrl => app.focus = app.focus.prev(),
        KeyCode::Down | KeyCode::Char('j') if no_ctrl => app.focus = app.focus.next(),
        KeyCode::Tab => app.focus = app.focus.next(),
        KeyCode::BackTab => app.focus = app.focus.prev(),
        KeyCode::Enter => {
            // 进入当前区块的子内容
            app.focus_mode = FocusMode::In;
        }
        KeyCode::Char('?') => app.show_help = !app.show_help,
        _ => {}
    }
    Ok(())
}

/// In 模式：方向键在当前区块子内容里操作；Enter 触发；Esc 退回 Top；
/// Tab / BackTab 在两个模式下都能切区块（保留 In 状态）。
fn handle_in_mode(app: &mut App, key: KeyEvent) -> Result<()> {
    let no_ctrl = !key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            // 退回 Top 模式
            app.focus_mode = FocusMode::Top;
            app.status_msg = None;
            return Ok(());
        }
        // Tab / BackTab 在 In 模式也能切区块（这样 user 在 Watches In 也能用 Tab 到 Menu）
        KeyCode::Tab => {
            app.focus = app.focus.next();
            return Ok(());
        }
        KeyCode::BackTab => {
            app.focus = app.focus.prev();
            return Ok(());
        }
        KeyCode::Char('?') => {
            app.show_help = !app.show_help;
            return Ok(());
        }
        _ => {}
    }
    match app.focus {
        Focus::Watches => handle_watches_in(app, key, no_ctrl),
        Focus::Detail => handle_detail_in(app, key, no_ctrl),
        Focus::Events => handle_events_in(app, key, no_ctrl),
        Focus::Actions => handle_actions_in(app, key, no_ctrl),
    }
}

/// Watches 子内容：方向键选 watch；Enter = 触发（自动更新 detail 显示并把焦点切到 Detail）。
fn handle_watches_in(app: &mut App, key: KeyEvent, no_ctrl: bool) -> Result<()> {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') if no_ctrl => {
            app.watch_idx = app.watch_idx.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') if no_ctrl => {
            let max = snapshot_watches_len(app).saturating_sub(1);
            if max != usize::MAX {
                app.watch_idx = (app.watch_idx + 1).min(max);
            }
        }
        KeyCode::Home | KeyCode::Char('g') if no_ctrl => app.watch_idx = 0,
        KeyCode::End | KeyCode::Char('G') if no_ctrl => {
            let n = snapshot_watches_len(app);
            if n > 0 {
                app.watch_idx = n - 1;
            }
        }
        KeyCode::Enter => {
            // 触发：detail 显示由 watch_idx 同步；这里把焦点切到 Detail 区块
            // （用户下一步可直接用方向键操控 detail 的操作按钮）
            app.focus = Focus::Detail;
        }
        KeyCode::Char('q') => app.request_quit(),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => app.request_quit(),
        _ => {}
    }
    Ok(())
}

/// Detail 子内容：方向键选 per-watch 按钮；Enter 触发当前按钮。
fn handle_detail_in(app: &mut App, key: KeyEvent, no_ctrl: bool) -> Result<()> {
    let n = actions::DETAIL_BUTTONS.len();
    match key.code {
        KeyCode::Up | KeyCode::Char('k') if no_ctrl => {
            if n > 0 {
                app.detail_btn_idx = (app.detail_btn_idx + n - 1) % n;
            }
        }
        KeyCode::Down | KeyCode::Char('j') if no_ctrl => {
            if n > 0 {
                app.detail_btn_idx = (app.detail_btn_idx + 1) % n;
            }
        }
        KeyCode::Left | KeyCode::Char('h') if no_ctrl => {
            if n > 0 {
                app.detail_btn_idx = (app.detail_btn_idx + n - 1) % n;
            }
        }
        KeyCode::Right | KeyCode::Char('l') if no_ctrl => {
            if n > 0 {
                app.detail_btn_idx = (app.detail_btn_idx + 1) % n;
            }
        }
        KeyCode::Enter => {
            actions::dispatch_detail_action(app, app.detail_btn_idx);
        }
        KeyCode::Char('q') => app.request_quit(),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => app.request_quit(),
        _ => {}
    }
    Ok(())
}

/// Logs 子内容：方向键滚动事件列表；Enter 无操作（事件只读）。
fn handle_events_in(app: &mut App, key: KeyEvent, no_ctrl: bool) -> Result<()> {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') if no_ctrl => {
            app.event_idx = app.event_idx.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') if no_ctrl => {
            let max = snapshot_events_len(app).saturating_sub(1);
            if max != usize::MAX {
                app.event_idx = (app.event_idx + 1).min(max);
            }
        }
        KeyCode::Home | KeyCode::Char('g') if no_ctrl => app.event_idx = 0,
        KeyCode::End | KeyCode::Char('G') if no_ctrl => {
            let n = snapshot_events_len(app);
            if n > 0 {
                app.event_idx = n - 1;
            }
        }
        KeyCode::Char('q') => app.request_quit(),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => app.request_quit(),
        _ => {}
    }
    Ok(())
}

/// Menu 子内容：方向键选全局按钮；Enter 触发当前按钮。
fn handle_actions_in(app: &mut App, key: KeyEvent, no_ctrl: bool) -> Result<()> {
    let n_buttons = actions::BUTTONS.len();
    match key.code {
        KeyCode::Left | KeyCode::Char('h') if no_ctrl => {
            if n_buttons > 0 {
                app.action_idx = (app.action_idx + n_buttons - 1) % n_buttons;
            }
        }
        KeyCode::Right | KeyCode::Char('l') if no_ctrl => {
            if n_buttons > 0 {
                app.action_idx = (app.action_idx + 1) % n_buttons;
            }
        }
        KeyCode::Up | KeyCode::Char('k') if no_ctrl => {
            if n_buttons > 0 {
                app.action_idx = (app.action_idx + n_buttons - 1) % n_buttons;
            }
        }
        KeyCode::Down | KeyCode::Char('j') if no_ctrl => {
            if n_buttons > 0 {
                app.action_idx = (app.action_idx + 1) % n_buttons;
            }
        }
        KeyCode::Enter => {
            actions::dispatch(app);
        }
        KeyCode::Char('q') => app.request_quit(),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => app.request_quit(),
        _ => {}
    }
    Ok(())
}

fn handle_prompt_mode(app: &mut App, key: KeyEvent) -> Result<()> {
    match key.code {
        KeyCode::Esc => {
            actions::dispatch_prompt_cancel(app);
        }
        KeyCode::Enter => {
            actions::dispatch_prompt_submit(app);
        }
        KeyCode::Backspace => {
            app.input_buf.pop();
        }
        KeyCode::Char(c) => {
            if !key.modifiers.contains(KeyModifiers::CONTROL) {
                app.input_buf.push(c);
            }
        }
        _ => {}
    }
    Ok(())
}

fn snapshot_watches_len(app: &App) -> usize {
    app.monitor
        .cfg_snapshot()
        .get("watches")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0)
}

fn snapshot_events_len(app: &App) -> usize {
    app.monitor.events_snapshot().len()
}
