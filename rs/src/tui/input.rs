//! 键盘事件分发。
//!
//! 键位仅方向键 + Enter，辅以 ? / q / Esc。详细的 vim 风键位被全面砍掉。

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::actions;
use super::{App, Focus, InputMode};

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
                    // 文本格式: "删 watch {wid} ? (y/n)"
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
    // 4. 普通焦点模式
    handle_normal_mode(app, key)
}

fn extract_wid_from_confirm(text: &str) -> Option<String> {
    // 模式: "删 watch {wid} ? (y/n)"
    let s = text.trim();
    if !s.starts_with("删 watch ") {
        return None;
    }
    let rest = &s["删 watch ".len()..];
    rest.split_whitespace().next().map(String::from)
}

fn handle_normal_mode(app: &mut App, key: KeyEvent) -> Result<()> {
    let n_buttons = actions::BUTTONS.len();
    match key.code {
        KeyCode::Tab => app.focus = app.focus.next(),
        KeyCode::BackTab => app.focus = app.focus.prev(),
        KeyCode::Left | KeyCode::Char('h') => {
            // 仅在没有 Ctrl 修饰时才触发（避免与系统快捷键冲突）
            if !key.modifiers.contains(KeyModifiers::CONTROL) {
                if app.focus == Focus::Actions {
                    app.action_idx = (app.action_idx + n_buttons - 1) % n_buttons;
                } else {
                    app.focus = app.focus.left();
                }
            }
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if !key.modifiers.contains(KeyModifiers::CONTROL) {
                if app.focus == Focus::Actions {
                    app.action_idx = (app.action_idx + 1) % n_buttons;
                } else {
                    app.focus = app.focus.right();
                }
            }
        }
        KeyCode::Up | KeyCode::Char('k') => match app.focus {
            Focus::Watches => {
                app.watch_idx = app.watch_idx.saturating_sub(1);
            }
            Focus::Events => {
                app.event_idx = app.event_idx.saturating_sub(1);
            }
            Focus::Actions => {
                app.action_idx = (app.action_idx + n_buttons - 1) % n_buttons;
            }
            _ => {}
        },
        KeyCode::Down | KeyCode::Char('j') => match app.focus {
            Focus::Watches => {
                let max = snapshot_watches_len(app).saturating_sub(1);
                app.watch_idx = if max == usize::MAX { 0 } else { (app.watch_idx + 1).min(max) };
            }
            Focus::Events => {
                let max = snapshot_events_len(app).saturating_sub(1);
                app.event_idx = if max == usize::MAX { 0 } else { (app.event_idx + 1).min(max) };
            }
            Focus::Actions => {
                app.action_idx = (app.action_idx + 1) % n_buttons;
            }
            _ => {}
        },
        KeyCode::Home | KeyCode::Char('g') => match app.focus {
            Focus::Watches => app.watch_idx = 0,
            Focus::Events => app.event_idx = 0,
            _ => {}
        },
        KeyCode::End | KeyCode::Char('G') => match app.focus {
            Focus::Watches => {
                let n = snapshot_watches_len(app);
                if n > 0 {
                    app.watch_idx = n - 1;
                }
            }
            Focus::Events => {
                let n = snapshot_events_len(app);
                if n > 0 {
                    app.event_idx = n - 1;
                }
            }
            _ => {}
        },
        KeyCode::Enter => {
            if app.focus == Focus::Actions {
                actions::dispatch(app);
            } else {
                // Enter 在 pane 内把焦点跳到 Actions bar（用户的「按 Enter 触发动作」直觉）
                app.focus = Focus::Actions;
            }
        }
        KeyCode::Char('?') => app.show_help = !app.show_help,
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true
        }
        KeyCode::Esc => {
            // 关闭状态信息
            app.show_help = false;
            app.status_msg = None;
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
