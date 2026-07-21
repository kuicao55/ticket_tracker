//! 左 / 中 / 右 pane 渲染。
//!
//! 所有 monitor 数据通过 `cfg_snapshot()` / `events_snapshot()` 拿 cheap 副本，
//! 完全不持有 monitor 锁，渲染零阻塞。

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Cell, List, ListItem, Paragraph, Row, Table, Wrap};
use ratatui::Frame;
use serde_json::Value;

use super::{App, Focus};
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
        let p = Paragraph::new("")
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

pub fn draw_detail(app: &mut App, f: &mut Frame, area: Rect) {
    let focused = app.focus == Focus::Detail;
    let cfg = app.monitor.cfg_snapshot();
    // 选中 watch
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
        let empty = Paragraph::new("")
            .block(body_block)
            .wrap(Wrap { trim: false });
        f.render_widget(empty, area);
        return;
    }
    let w = watch_opt.unwrap();
    // 内部分两段：详情 + 影院子表
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(8), Constraint::Min(1)])
        .split(area);
    // 详情
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
    let cinemas_str = if cinemas.is_empty() {
        "(无)".to_string()
    } else {
        cinemas.join(", ")
    };
    let dates_str = if dates.is_empty() {
        "不限".to_string()
    } else {
        dates.join(", ")
    };
    let interval_str = interval
        .map(|n| format!("{}s", n))
        .unwrap_or_else(|| "(default)".into());
    let detail_lines = vec![
        Line::from(vec![
            Span::styled("名称    ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} ({})", name, mid),
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("cinemas ", Style::default().fg(Color::DarkGray)),
            Span::raw(cinemas_str),
        ]),
        Line::from(vec![
            Span::styled("dates   ", Style::default().fg(Color::DarkGray)),
            Span::raw(dates_str),
        ]),
        Line::from(vec![
            Span::styled("interval", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(" {}", interval_str)),
            Span::raw("  "),
            Span::styled(
                if enabled { "✓ enabled" } else { "× disabled" },
                Style::default().fg(if enabled { Color::Green } else { Color::DarkGray }),
            ),
        ]),
        Line::from(vec![
            Span::styled("fired   ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(" {}/{}", fired_n, total_cinemas)),
        ]),
    ];
    let detail = Paragraph::new(detail_lines)
        .block(Block::default().borders(Borders::NONE))
        .wrap(Wrap { trim: false });
    f.render_widget(detail, chunks[0]);
    // 子表
    let header = Row::new(vec![
        Cell::from("cinema").style(Style::default().fg(Color::DarkGray)),
        Cell::from("shows").style(Style::default().fg(Color::DarkGray)),
        Cell::from("range").style(Style::default().fg(Color::DarkGray)),
    ]);
    let payload = w.get("_last_payload").cloned().unwrap_or(serde_json::json!({}));
    let matches: Vec<Value> = payload
        .get("matches")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let rows: Vec<Row> = matches
        .iter()
        .map(|m| {
            let n = m.get("cinema_name").and_then(|v| v.as_str()).unwrap_or("?");
            let sc = m.get("show_count").and_then(|v| v.as_i64()).unwrap_or(0);
            let e = m.get("earliest").and_then(|v| v.as_str()).unwrap_or("");
            let l = m.get("latest").and_then(|v| v.as_str()).unwrap_or("");
            Row::new(vec![
                Cell::from(n.to_string()),
                Cell::from(sc.to_string()),
                Cell::from(format!("{} → {}", e, l)),
            ])
        })
        .collect();
    let sub_block = Block::default()
        .borders(Borders::TOP)
        .border_style(border(focused))
        .title(Span::styled(" cinemas ", border(focused)));
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(50),
            Constraint::Length(8),
            Constraint::Percentage(40),
        ],
    )
    .header(header)
    .block(sub_block);
    f.render_widget(table, chunks[1]);
}

pub fn draw_events(app: &mut App, f: &mut Frame, area: Rect) {
    let focused = app.focus == Focus::Events;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border(focused))
        .title(Span::styled(" events ", border(focused)));
    let items: Vec<ListItem> = app
        .monitor
        .events_snapshot()
        .into_iter()
        .map(ListItem::new)
        .collect();
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
