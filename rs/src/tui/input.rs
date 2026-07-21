//! 键盘事件分发。
//!
//! 详见 RUST_PORT.md §7.6 键位表。

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, ConfirmPrompt, Focus, InputMode};

pub fn handle_key(app: &mut App, key: KeyEvent) -> Result<()> {
    // 1. 模态覆盖层优先
    if app.show_help {
        if key.code == KeyCode::Esc || key.code == KeyCode::Char('?') || key.code == KeyCode::Enter {
            app.show_help = false;
            return Ok(());
        }
        return Ok(());
    }
    // 2. Confirm 提示
    if let Some(c) = app.confirm.clone() {
        if c.created_at.elapsed() > std::time::Duration::from_secs(5) {
            app.confirm = None;
        } else {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    // confirm accept —— 在调用方按上下文决定动作（简化版：清掉 confirm）
                    app.confirm = None;
                    // 真正的删除放在 monitor 里调用方做，这里不实现
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    app.confirm = None;
                }
                _ => {}
            }
            return Ok(());
        }
    }
    // 3. 输入模式（Filter / Cmd）
    match app.input_mode {
        InputMode::Cmd => handle_cmd_mode(app, key),
        InputMode::Filter => handle_filter_mode(app, key),
        InputMode::Normal => handle_normal_mode(app, key),
    }
}

fn handle_normal_mode(app: &mut App, key: KeyEvent) -> Result<()> {
    match key.code {
        // 焦点切换
        KeyCode::Tab => app.focus = app.focus.next(),
        KeyCode::BackTab => app.focus = app.focus.prev(),
        KeyCode::Char('h') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.focus = app.focus.left()
        }
        KeyCode::Char('l') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.focus = app.focus.right()
        }
        // 上下移动（在焦点 pane 内）
        KeyCode::Char('j') | KeyCode::Down => match app.focus {
            Focus::Watches => {
                app.watch_idx = app.watch_idx.saturating_add(1);
            }
            Focus::Events => {
                app.event_idx = app.event_idx.saturating_add(1);
            }
            _ => {}
        },
        KeyCode::Char('k') | KeyCode::Up => match app.focus {
            Focus::Watches => {
                app.watch_idx = app.watch_idx.saturating_sub(1);
            }
            Focus::Events => {
                app.event_idx = app.event_idx.saturating_sub(1);
            }
            _ => {}
        },
        // 首/尾
        KeyCode::Char('g') => {
            if app.focus == Focus::Events {
                app.event_idx = 0;
            } else if app.focus == Focus::Watches {
                app.watch_idx = 0;
            }
        }
        KeyCode::Char('G') => {
            if app.focus == Focus::Events {
                app.event_idx = usize::MAX;
            } else if app.focus == Focus::Watches {
                app.watch_idx = usize::MAX;
            }
        }
        // 过滤 / 命令 / 帮助 / 退出
        KeyCode::Char('/') => {
            if app.focus == Focus::Watches || app.focus == Focus::Events {
                app.input_mode = InputMode::Filter;
                app.input_buf.clear();
            }
        }
        KeyCode::Char(':') => {
            app.input_mode = InputMode::Cmd;
            app.input_buf.clear();
        }
        KeyCode::Char('?') => {
            app.show_help = true;
        }
        KeyCode::Char('a') => {
            // 进 :add 模板
            app.input_mode = InputMode::Cmd;
            app.input_buf = "add ".to_string();
        }
        KeyCode::Char('d') => {
            // confirm 删除
            if app.focus == Focus::Watches {
                app.confirm = Some(ConfirmPrompt {
                    text: "delete current watch? (y/n)".into(),
                    created_at: std::time::Instant::now(),
                });
            }
        }
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true
        }
        _ => {}
    }
    Ok(())
}

fn handle_cmd_mode(app: &mut App, key: KeyEvent) -> Result<()> {
    match key.code {
        KeyCode::Esc => {
            app.input_mode = InputMode::Normal;
            app.input_buf.clear();
        }
        KeyCode::Enter => {
            let cmd = app.input_buf.trim().to_string();
            app.input_mode = InputMode::Normal;
            app.input_buf.clear();
            super::cmd::execute(app, &cmd);
        }
        KeyCode::Backspace => {
            app.input_buf.pop();
        }
        KeyCode::Char(c) => {
            app.input_buf.push(c);
        }
        _ => {}
    }
    Ok(())
}

fn handle_filter_mode(app: &mut App, key: KeyEvent) -> Result<()> {
    match key.code {
        KeyCode::Esc => {
            app.input_mode = InputMode::Normal;
            app.input_buf.clear();
        }
        KeyCode::Enter => {
            // 过滤直接生效（简单版：只在状态栏显示）
            app.status_msg = Some(format!("filter: {}", app.input_buf));
            app.status_msg_until = Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
            app.input_mode = InputMode::Normal;
            app.input_buf.clear();
        }
        KeyCode::Backspace => {
            app.input_buf.pop();
        }
        KeyCode::Char(c) => {
            app.input_buf.push(c);
        }
        _ => {}
    }
    Ok(())
}
