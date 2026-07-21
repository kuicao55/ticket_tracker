//! 主 render。

use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, BorderType, Clear, Paragraph, Wrap};
use ratatui::Terminal;
use serde_json::Value;
use std::io::Stdout;

use super::{panes, App, InputMode};

pub fn render(app: &mut App, f: &mut ratatui::Frame) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(3),    // body
            Constraint::Length(1), // status
        ])
        .split(area);

    draw_header(app, f, chunks[0]);

    // body 三栏
    let body_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(22),
            Constraint::Min(40),
            Constraint::Length(38),
        ])
        .split(chunks[1]);
    panes::draw_watches(app, f, body_cols[0]);
    panes::draw_detail(app, f, body_cols[1]);
    panes::draw_events(app, f, body_cols[2]);

    draw_statusbar(app, f, chunks[2]);

    // Cmd/Focus 模式下的输入行（叠加在状态栏上方）
    if app.input_mode == InputMode::Cmd || app.input_mode == InputMode::Filter {
        let row = chunks[2].y.saturating_sub(1);
        let input_area = Rect::new(area.x, row, area.width, 1);
        f.render_widget(Clear, input_area);
        let prefix = match app.input_mode {
            InputMode::Cmd => ":",
            InputMode::Filter => "/",
            _ => "",
        };
        let line = Paragraph::new(Line::from(vec![
            Span::styled(prefix, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw(&app.input_buf),
            Span::styled("▮", Style::default().fg(Color::Cyan)),
        ]));
        f.render_widget(line, input_area);
    }
    // Help 覆盖层
    if app.show_help {
        draw_help(f, area);
    }
    // Confirm 提示（占用输入行之上）
    if let Some(c) = &app.confirm {
        let row = chunks[2].y.saturating_sub(1);
        let confirm_area = Rect::new(area.x, row, area.width, 1);
        let line = Paragraph::new(Line::from(Span::styled(
            format!(" {}", c.text),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )));
        f.render_widget(line, confirm_area);
    }
}

fn draw_header(app: &mut App, f: &mut ratatui::Frame, area: Rect) {
    let now = chrono::Local::now().format("%H:%M").to_string();
    let elapsed = chrono::Utc::now().timestamp() as f64 - app.cached_started_at;
    let uptime = crate::monitor::format_uptime(elapsed.max(0.0) as u64);
    let line = Line::from(vec![
        Span::styled("ticket-tracker", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw("   "),
        Span::raw(now),
        Span::raw("  "),
        Span::raw(format!("up {}", uptime)),
        Span::raw("   "),
        Span::styled(app.cached_mode.as_str(), Style::default().fg(Color::Yellow)),
        Span::raw("   "),
        Span::raw(format!("{} active", app.cached_active)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_statusbar(_app: &mut App, f: &mut ratatui::Frame, area: Rect) {
    let tips = if let Some(msg) = &_app.status_msg {
        if let Some(until) = _app.status_msg_until {
            if std::time::Instant::now() < until {
                msg.clone()
            } else {
                default_tips()
            }
        } else {
            msg.clone()
        }
    } else {
        default_tips()
    };
    let line = Line::from(Span::styled(format!(" {}", tips), Style::default().fg(Color::DarkGray)));
    f.render_widget(Paragraph::new(line), area);
}

fn default_tips() -> String {
    "Tab/h/l 切焦点  j/k 移动  / 过滤  : 命令  ? 帮助  q 退出".into()
}

fn draw_help(f: &mut ratatui::Frame, area: Rect) {
    let popup = centered_rect(80, 80, area);
    f.render_widget(Clear, popup);
    let text = vec![
        Line::from(Span::styled("ticket-tracker 帮助", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from("焦点: Tab / Shift+Tab / h / l"),
        Line::from("移动: j/k/↑/↓   首/尾: g/G"),
        Line::from("过滤: /   命令面板: :   帮助: ?"),
        Line::from("立即检查: r   添加: a   删除: d   编辑: e"),
        Line::from(""),
        Line::from(Span::styled("命令:", Style::default().add_modifier(Modifier::BOLD))),
        Line::from(":interval <s>      设置全局间隔"),
        Line::from(":webhook <url|clear>"),
        Line::from(":quiet <HH:MM-HH:MM>   静默时段"),
        Line::from(":phone <HH:MM-HH:MM>   phone-only 时段"),
        Line::from(":films [1|2|3]      拉猫眼列表"),
        Line::from(":add <mid> -c <cid>... -d <date>..."),
        Line::from(":rm <wid>   :enable <wid>   :disable <wid>"),
        Line::from(":doctor    :quit"),
        Line::from(""),
        Line::from(Span::styled("（按任意键关闭）", Style::default().fg(Color::DarkGray))),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(" Help ");
    let p = Paragraph::new(text).block(block).wrap(Wrap { trim: false });
    f.render_widget(p, popup);
}

pub(crate) fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[allow(dead_code)]
fn render_silent(_t: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    Ok(())
}

#[allow(dead_code)]
const _: Option<Value> = None;
