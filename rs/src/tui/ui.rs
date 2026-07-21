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

use super::{actions, panes, App, Focus, FocusMode, InputMode};

pub fn render(app: &mut App, f: &mut ratatui::Frame) {
    let area = f.area();
    // 极小窗口 fallback
    if area.width < 60 || area.height < 8 {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(area);
        draw_header(app, f, chunks[0]);
        let body_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(if area.height >= 6 { 3 } else { 1 }),
                Constraint::Length(0),
                Constraint::Min(1),
            ])
            .split(chunks[1]);
        panes::draw_watches(app, f, body_chunks[0]);
        panes::draw_detail(app, f, body_chunks[1]);
        panes::draw_events(app, f, body_chunks[2]);
        draw_statusbar(app, f, chunks[2]);
        draw_input_line(app, f, chunks[2]);
        if app.show_help {
            draw_help(f, area);
        }
        if let Some(c) = &app.confirm {
            let row = chunks[2].y.saturating_sub(1);
            let confirm_area = Rect::new(area.x, row, area.width, 1);
            let line = Paragraph::new(Line::from(Span::styled(
                format!(" {}", c.text),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )));
            f.render_widget(line, confirm_area);
        }
        return;
    }
    // 正常布局：[header / body / actions / status]
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(5),    // body
            Constraint::Length(2), // actions（标题 + 按钮行）
            Constraint::Length(1), // status
        ])
        .split(area);

    draw_header(app, f, chunks[0]);

    // body 三栏：左 ≤ 18，中 ≥ 60，右 = remaining
    let mid_w = 60u16.min(area.width.saturating_sub(50));
    let left_w = 18u16.min(area.width.saturating_sub(mid_w + 24));
    let right_w = area.width.saturating_sub(mid_w + left_w);
    let body_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(left_w),
            Constraint::Length(mid_w),
            Constraint::Length(right_w),
        ])
        .split(chunks[1]);
    panes::draw_watches(app, f, body_cols[0]);
    panes::draw_detail(app, f, body_cols[1]);
    panes::draw_events(app, f, body_cols[2]);

    // actions bar（2 行：标题 + 按钮）
    draw_actions(app, f, chunks[2]);

    // status 栏 + input line 叠加
    draw_statusbar(app, f, chunks[3]);
    draw_input_line(app, f, chunks[3]);

    // Help 覆盖层
    if app.show_help {
        draw_help(f, area);
    }
    // Confirm 提示
    if let Some(c) = &app.confirm {
        let row = chunks[3].y.saturating_sub(1);
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
    let block_label = match app.focus {
        Focus::Watches => "Watches",
        Focus::Detail => "Detail",
        Focus::Events => "Events",
        Focus::Actions => "Actions",
    };
    let mode_label = match app.focus_mode {
        FocusMode::Top => "TOP",
        FocusMode::In => "IN",
    };
    let mode_color = match app.focus_mode {
        FocusMode::Top => Color::Yellow,
        FocusMode::In => Color::Cyan,
    };
    let line = Line::from(vec![
        Span::styled(
            "ticket-tracker",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::raw(now),
        Span::raw("  "),
        Span::raw(format!("up {}", uptime)),
        Span::raw("   "),
        Span::styled(app.cached_mode.as_str(), Style::default().fg(Color::Yellow)),
        Span::raw("   "),
        Span::raw(format!("{} active", app.cached_active)),
        Span::raw("   "),
        Span::styled(
            format!("[{}] {}", mode_label, block_label),
            Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_actions(app: &mut App, f: &mut ratatui::Frame, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);
    // 第 1 行：标题
    let title = Line::from(vec![
        Span::styled("─ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "actions",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            "←/→ 切换 · Enter 触发",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(" ─", Style::default().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(title), rows[0]);

    // 第 2 行：按钮一字排开，按 app.action_idx 高亮
    let mut spans: Vec<Span> = Vec::new();
    let mut used = 0usize;
    let max_w = area.width as usize;
    for (i, (icon, label)) in actions::BUTTONS.iter().enumerate() {
        let text = format!(" [{}] {} ", icon, label);
        let w = text.chars().count();
        if used + w > max_w {
            spans.push(Span::styled("…", Style::default().fg(Color::DarkGray)));
            break;
        }
        let style = if i == app.action_idx {
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
    f.render_widget(Paragraph::new(Line::from(spans)), rows[1]);
}

fn draw_statusbar(app: &mut App, f: &mut ratatui::Frame, area: Rect) {
    let tips = if let Some(msg) = &app.status_msg {
        if let Some(until) = app.status_msg_until {
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
    let line = Line::from(Span::styled(
        format!(" {}", tips),
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(Paragraph::new(line), area);
}

fn default_tips() -> String {
    "←/→ 选区块 (Top)   ↓ 进区块 (In)   Enter 触发   Esc 退回上一级   ? 帮助   q 退出".into()
}

fn draw_input_line(app: &App, f: &mut ratatui::Frame, status: Rect) {
    if app.input_mode != InputMode::Cmd {
        return;
    }
    // prefix：add / edit / config 循环
    let prefix = match (&app.prompt_target, &app.input_buf) {
        (Some(t), _) => format!("{}> ", t.label()),
        (None, s) if s.starts_with("add ") || s.starts_with("edit ") => {
            let cmd = if s.starts_with("add ") { "add> " } else { "edit> " };
            cmd.to_string()
        }
        (None, _) => "input> ".into(),
    };
    let row = status.y.saturating_sub(1);
    let input_area = Rect::new(f.area().x, row, f.area().width, 1);
    f.render_widget(Clear, input_area);
    let buf_display = match &app.prompt_target {
        Some(_) => String::new(),
        None => app.input_buf.clone(),
    };
    let line = Paragraph::new(Line::from(vec![
        Span::styled(
            prefix,
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::raw(buf_display),
        Span::styled("▮", Style::default().fg(Color::Cyan)),
    ]));
    f.render_widget(line, input_area);
}

fn draw_help(f: &mut ratatui::Frame, area: Rect) {
    let popup = centered_rect(70, 70, area);
    f.render_widget(Clear, popup);
    let text = vec![
        Line::from(Span::styled(
            "ticket-tracker 帮助",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Top 模式（标题栏写 TOP）：",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  ←/→ 选区块（Watches/Detail/Events/Actions）"),
        Line::from("  ↓ 或 Enter 进入当前区块（→ In 模式）"),
        Line::from("  Esc / q / Ctrl+C 退出"),
        Line::from(""),
        Line::from(Span::styled(
            "In 模式（标题栏写 IN）：",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  Watches: ↑/↓ 切 watch；Enter 无操作（已 In）"),
        Line::from("  Detail:  ↑/↓ / ←/→ 切 per-watch 按钮；Enter 触发"),
        Line::from("          [◉ 启停] [~ 影院] [~ 日期] [~ 间隔] [r 检查] [- 删除]"),
        Line::from("  Events:  ↑/↓ 滚事件"),
        Line::from("  Actions: ←/→/↑/↓ 切按钮；Enter 触发"),
        Line::from("  Esc 退回 Top；Tab 跳下一区块并保持 In"),
        Line::from(""),
        Line::from(Span::styled("其它：", Style::default().add_modifier(Modifier::BOLD))),
        Line::from("  ? 切换本覆盖层"),
        Line::from("  q / Ctrl+C 干净退出（Discord 收到「已停止 🛑」）"),
        Line::from(Span::styled(
            "（按 ? 或任意键关闭）",
            Style::default().fg(Color::DarkGray),
        )),
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
