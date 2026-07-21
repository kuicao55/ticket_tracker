//! 键盘事件分发。
//!
//! 导航模型（Top ↔ In）：
//! - **Top**：方向键在 4 区块间循环；`↓` 或 `Enter` 进入当前区块（→ In）；`Esc` / `q` / `Ctrl+C` 退出
//! - **In**：方向键在当前区块内操作；`Enter` 触发；`Esc` 退回 Top；`Tab` 循环到下一区块并保持 In

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::actions;
use super::{App, Focus, FocusMode, InputMode};

pub fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    // 1. help 覆盖层优先生效
    if app.show_help {
        if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?'))
        {
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
                    // 确认执行删除
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
    // 3. prompt 期间
    if app.input_mode == InputMode::Cmd {
        return handle_prompt_mode(app, key);
    }
    // 4. 普通焦点模式（Top / In 分支）
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

/// 顶层模式：方向键循环 4 区块；`↓` 或 `Enter` 进当前区块；`Esc` 退出。
fn handle_top_mode(app: &mut App, key: KeyEvent) -> Result<()> {
    let no_ctrl = !key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => app.request_quit(),
        KeyCode::Char('q') => app.request_quit(),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.request_quit()
        }
        KeyCode::Left | KeyCode::Char('h') if no_ctrl => {
            app.focus = app.focus.prev();
        }
        KeyCode::Right | KeyCode::Char('l') if no_ctrl => {
            app.focus = app.focus.next();
        }
        KeyCode::Tab => {
            app.focus = app.focus.next();
            app.focus_mode = FocusMode::In;
        }
        KeyCode::BackTab => {
            app.focus = app.focus.prev();
            app.focus_mode = FocusMode::In;
        }
        KeyCode::Down | KeyCode::Char('j') if no_ctrl => {
            app.focus_mode = FocusMode::In;
        }
        KeyCode::Enter => {
            app.focus_mode = FocusMode::In;
        }
        KeyCode::Char('?') => app.show_help = !app.show_help,
        _ => {}
    }
    Ok(())
}

/// In 区块模式：方向键在当前区块内操作；Enter 触发；Esc 退回 Top；Tab 跳下一区块。
fn handle_in_mode(app: &mut App, key: KeyEvent) -> Result<()> {
    let no_ctrl = !key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            // 退回 Top
            app.focus_mode = FocusMode::Top;
            // 关闭状态信息（保留旧语义）
            app.status_msg = None;
            return Ok(());
        }
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
            // 已在 In，Enter 不触发额外动作
        }
        KeyCode::Char('q') => app.request_quit(),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.request_quit()
        }
        _ => {}
    }
    Ok(())
}

fn handle_detail_in(app: &mut App, key: KeyEvent, no_ctrl: bool) -> Result<()> {
    let n = actions::DETAIL_BUTTONS.len();
    match key.code {
        KeyCode::Up | KeyCode::Char('k') if no_ctrl => {
            // 上滚（暂用作按钮循环，可改 cinema sub-table 滚动）
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
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.request_quit()
        }
        _ => {}
    }
    Ok(())
}

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
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.request_quit()
        }
        _ => {}
    }
    Ok(())
}

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
        KeyCode::Home if no_ctrl => app.action_idx = 0,
        KeyCode::End if no_ctrl => {
            if n_buttons > 0 {
                app.action_idx = n_buttons - 1;
            }
        }
        KeyCode::Enter => {
            actions::dispatch(app);
        }
        KeyCode::Char('q') => app.request_quit(),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.request_quit()
        }
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
            // 不允许 Ctrl 组合键写到 buf
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