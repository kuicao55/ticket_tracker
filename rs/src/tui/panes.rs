//! 左 / 中 / 右 pane 渲染。
//!
//! 所有 monitor 数据通过 `cfg_snapshot()` / `events_snapshot()` 拿 cheap 副本，
//! 完全不持有 monitor 锁，渲染零阻塞。
//!
//! 布局（与 py 版一致）：
//! - watches（左侧满高）
//! - detail（右上：watch 全部信息 + per-watch 按钮行）
//! - logs（右下：最近 12 条事件）

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;
use serde_json::Value;

use super::{actions, App, Focus, FocusMode};
use crate::monitor::{S_ERROR, S_NO_SHOWS, S_NOT_LISTED, S_OPEN};

fn border(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn status_icon(code: Option<&str>) -> &'static str {
    match code {
        Some(S_OPEN) => "✓",
        Some(S_NOT_LISTED) => "-",
        Some(S_NO_SHOWS) => "⠂",
        Some(S_ERROR) => "!",
        _ => "?",
    }
}

fn status_color(code: Option<&str>) -> Color {
    match code {
        Some(S_OPEN) => Color::Green,
        Some(S_NOT_LISTED) => Color::DarkGray,
        Some(S_NO_SHOWS) => Color::Yellow,
        Some(S_ERROR) => Color::Red,
        _ => Color::DarkGray,
    }
}

pub fn draw_watches(app: &mut App, f: &mut Frame, area: Rect) {
    let focused = app.focus == Focus::Watches;
    let cfg = app.monitor.cfg_snapshot();
    let n = cfg
        .get("watches")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let title = format!(" watches ({}) ", n);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border(focused))
        .title(Span::styled(title, border(focused)));
    let items: Vec<ListItem> = cfg
        .get("watches")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|w| {
                    let id = w.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                    let en = w.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
                    let code = w.get("_last_status").and_then(|v| v.as_str());
                    let mark = if en { "×" } else { " " };
                    let icon = status_icon(code);
                    let line = Line::from(vec![
                        Span::styled(icon.to_string(), Style::default().fg(status_color(code))),
                        Span::raw(" "),
                        Span::styled(
                            id.to_string(),
                            Style::default().fg(if en { Color::White } else { Color::DarkGray }),
                        ),
                        Span::raw(" "),
                        Span::raw(mark),
                    ]);
                    ListItem::new(line)
                })
                .collect()
        })
        .unwrap_or_default();
    if items.is_empty() {
        let p = Paragraph::new(Line::from(Span::styled(
            "（无 watch — 按 [A] 添加）",
            Style::default().fg(Color::DarkGray),
        )))
        .block(block)
        .wrap(Wrap { trim: false });
        f.render_widget(p, area);
        return;
    }
    let mut state = ratatui::widgets::ListState::default();
    state.select(Some(app.watch_idx.min(items.len().saturating_sub(1))));
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    f.render_stateful_widget(list, area, &mut state);
}

/// detail 列：watch 完整基本信息（名称 / 影院 id+name / 日期 / 间隔 / 启用 / 触发）
/// + per-watch 操作按钮行（仅 In 模式高亮当前按钮）。
///
/// 已移除 cinemas 子表。
pub fn draw_detail(app: &mut App, f: &mut Frame, area: Rect) {
    let focused = app.focus == Focus::Detail;
    let cfg = app.monitor.cfg_snapshot();
    let watch_opt: Option<Value> = cfg
        .get("watches")
        .and_then(|v| v.as_array())
        .and_then(|a| a.get(app.watch_idx).cloned());
    let body_block = match &watch_opt {
        Some(w) => {
            let id = w.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(border(focused))
                .title(Span::styled(format!(" detail · {} ", id), border(focused)))
        }
        None => Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border(focused))
            .title(Span::styled(" detail ", border(focused))),
    };
    if watch_opt.is_none() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "（请在左栏选一条 watch）",
            Style::default().fg(Color::DarkGray),
        )))
        .block(body_block)
        .wrap(Wrap { trim: false });
        f.render_widget(empty, area);
        return;
    }
    let w = watch_opt.unwrap();

    // 两段：info (Min) / 按钮行 (Length 3)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // info 区（含影院列表，按影院数自动撑开）
            Constraint::Length(3), // 操作按钮行（标题 + 2 行按钮）
        ])
        .split(area);

    // ---- 详情文本：影院列表 id+name ----
    let mid = w.get("movie_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let name = w.get("movie_name").and_then(|v| v.as_str()).unwrap_or("");
    let cinemas: Vec<String> = w
        .get("cinemas")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let dates: Vec<String> = w
        .get("dates")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let interval = w.get("interval").and_then(|v| v.as_u64());
    let enabled = w.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
    let fired_n = w.get("fired_cinemas").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
    let total_cinemas = cinemas.len();
    // 影院 id+name：从 _last_payload.cinema_names 兜底
    let payload = w.get("_last_payload").cloned().unwrap_or(serde_json::json!({}));
    let names_map: std::collections::HashMap<String, String> = serde_json::from_value(
        payload.get("cinema_names").cloned().unwrap_or(serde_json::json!({})),
    )
    .unwrap_or_default();
    let dates_str = if dates.is_empty() {
        "不限".to_string()
    } else {
        dates.join(", ")
    };
    let interval_str = interval
        .map(|n| format!("{}s", n))
        .unwrap_or_else(|| "(默认)".into());

    // info lines（含影院列表，每个影院一行 "name (id)" 或纯 id）
    let mut info_lines: Vec<Line> = Vec::new();
    info_lines.push(Line::from(vec![
        Span::styled("名称    ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{} ({})", name, mid),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
    ]));
    info_lines.push(Line::from(vec![Span::styled(
        "影院    ",
        Style::default().fg(Color::DarkGray),
    )]));
    if cinemas.is_empty() {
        info_lines.push(Line::from(Span::styled(
            "  （无）",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for cid in &cinemas {
            let display = match names_map.get(cid) {
                Some(n) if !n.is_empty() => format!("  {} ({})", n, cid),
                _ => format!("  {}", cid),
            };
            info_lines.push(Line::from(Span::styled(
                display,
                Style::default().fg(Color::White),
            )));
        }
    }
    info_lines.push(Line::from(vec![
        Span::styled("日期    ", Style::default().fg(Color::DarkGray)),
        Span::raw(dates_str),
    ]));
    info_lines.push(Line::from(vec![
        Span::styled("间隔    ", Style::default().fg(Color::DarkGray)),
        Span::raw(interval_str),
        Span::raw("    "),
        Span::styled(
            if enabled { "✓ enabled" } else { "× disabled" },
            Style::default().fg(if enabled { Color::Green } else { Color::DarkGray }),
        ),
    ]));
    info_lines.push(Line::from(vec![
        Span::styled("触发    ", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(" {}/{}", fired_n, total_cinemas)),
    ]));
    let info = Paragraph::new(info_lines)
        .block(Block::default().borders(Borders::NONE))
        .wrap(Wrap { trim: false });
    f.render_widget(info, chunks[0]);

    // ---- per-watch 按钮行（仅 Detail In 模式下高亮当前按钮） ----
    draw_detail_buttons(app, f, chunks[1]);
}

/// 渲染 detail 列底部 per-watch 按钮行。
/// 标题 + 两行按钮（3 个 / 行），当前按钮 cyan 黑底加粗。
fn draw_detail_buttons(app: &mut App, f: &mut Frame, area: Rect) {
    let in_detail_focus = app.focus == Focus::Detail && app.focus_mode == FocusMode::In;
    let title_line = Line::from(vec![
        Span::styled("─ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "操作",
            Style::default()
                .fg(if in_detail_focus {
                    Color::Cyan
                } else {
                    Color::DarkGray
                })
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            if in_detail_focus {
                "←/→ 切换 · Enter 触发 · Esc 返回"
            } else {
                "（进入本栏后可操作）"
            },
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(" ─", Style::default().fg(Color::DarkGray)),
    ]);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1)])
        .split(area);
    f.render_widget(Paragraph::new(title_line), rows[0]);

    let n = actions::DETAIL_BUTTONS.len();
    if n == 0 {
        return;
    }
    let buttons_per_row = 3usize;
    let (first_row, second_row) = (rows[1], rows[2]);

    // 第一行
    let mut used = 0usize;
    let max_w = first_row.width as usize;
    let mut spans: Vec<Span> = Vec::new();
    for (i, (icon, label)) in actions::DETAIL_BUTTONS.iter().enumerate() {
        if i >= buttons_per_row {
            break;
        }
        let text = format!(" [{}] {} ", icon, label);
        let w = text.chars().count();
        if used + w > max_w {
            spans.push(Span::styled("…", Style::default().fg(Color::DarkGray)));
            break;
        }
        let style = if in_detail_focus && i == app.detail_btn_idx {
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        spans.push(Span::styled(text, style));
        used += w;
    }
    f.render_widget(Paragraph::new(Line::from(spans)), first_row);

    // 第二行
    let mut used2 = 0usize;
    let max_w2 = second_row.width as usize;
    let mut spans2: Vec<Span> = Vec::new();
    for (i, (icon, label)) in actions::DETAIL_BUTTONS.iter().enumerate().skip(buttons_per_row) {
        let text = format!(" [{}] {} ", icon, label);
        let w = text.chars().count();
        if used2 + w > max_w2 {
            if i < n {
                spans2.push(Span::styled("…", Style::default().fg(Color::DarkGray)));
            }
            break;
        }
        let style = if in_detail_focus && i == app.detail_btn_idx {
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        spans2.push(Span::styled(text, style));
        used2 += w;
    }
    f.render_widget(Paragraph::new(Line::from(spans2)), second_row);
}

/// logs 列：最近 12 条事件（monitor 端已 cap 到 12）。
pub fn draw_logs(app: &mut App, f: &mut Frame, area: Rect) {
    let focused = app.focus == Focus::Events;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border(focused))
        .title(Span::styled(" logs (最近 12 条) ", border(focused)));
    let items: Vec<ListItem> = app
        .monitor
        .events_snapshot()
        .into_iter()
        .map(ListItem::new)
        .collect();
    if items.is_empty() {
        let p = Paragraph::new(Line::from(Span::styled(
            "（暂无事件）",
            Style::default().fg(Color::DarkGray),
        )))
        .block(block)
        .wrap(Wrap { trim: false });
        f.render_widget(p, area);
        return;
    }
    let mut state = ratatui::widgets::ListState::default();
    if !items.is_empty() {
        state.select(Some(app.event_idx.min(items.len() - 1)));
    }
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    f.render_stateful_widget(list, area, &mut state);
}
