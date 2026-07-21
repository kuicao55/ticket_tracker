//! 主 render。
//!
//! 布局（与 py 版一致）：
//!   header (1)
//!   body  ：watches (左, 满高) │ [details (右上) / logs (右下)]
//!   menu  ：标题 (1) + 两行按钮 (2)
//!   status (1)
//!
//! 输入模式（Top / In）见 input.rs。
//! - Top：方向键在 4 区块间循环（Watches/Detail/Logs/Menu）
//! - In ：方向键在当前区块内操作；Enter 触发；Esc 退回 Top

use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Terminal;
use serde_json::Value;
use std::io::Stdout;

use super::{actions, panes, App, Focus, FocusMode, InputMode};

pub fn render(app: &mut App, f: &mut ratatui::Frame) {
    let area = f.area();

    // 极小窗口 fallback —— 全部垂直堆叠
    if area.width < 60 || area.height < 12 {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(if area.height >= 8 { 6 } else { 3 }), // watches
                Constraint::Length(if area.height >= 8 { 4 } else { 2 }), // details
                Constraint::Min(1),                                        // logs
                Constraint::Length(3),                                     // menu
                Constraint::Length(1),                                     // status
            ])
            .split(area);
        draw_header(app, f, chunks[0]);
        panes::draw_watches(app, f, chunks[1]);
        panes::draw_detail(app, f, chunks[2]);
        panes::draw_logs(app, f, chunks[3]);
        draw_menu(app, f, chunks[4]);
        draw_statusbar(app, f, chunks[5]);
        draw_input_line(app, f, chunks[5]);
        if app.show_help {
            draw_help(f, area);
        }
        if let Some(c) = &app.confirm {
            let row = chunks[5].y.saturating_sub(1);
            let confirm_area = Rect::new(area.x, row, area.width, 1);
            let line = Paragraph::new(Line::from(Span::styled(
                format!(" {}", c.text),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )));
            f.render_widget(line, confirm_area);
        }
        return;
    }

    // ---- 正常布局：[header / body / menu / status] ----
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(6),    // body
            Constraint::Length(3), // menu（标题 + 两行按钮）
            Constraint::Length(1), // status
        ])
        .split(area);

    draw_header(app, f, chunks[0]);

    // body：左 watches（满高），右 [details / logs]
    let body_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(22), Constraint::Min(40)])
        .split(chunks[1]);
    panes::draw_watches(app, f, body_cols[0]);
    let right_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(body_cols[1]);
    panes::draw_detail(app, f, right_rows[0]);
    panes::draw_logs(app, f, right_rows[1]);

    // menu
    draw_menu(app, f, chunks[2]);

    // status + input line overlay
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
        Focus::Events => "Logs",
        Focus::Actions => "Menu",
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

/// 底部菜单栏：标题 + 两行按钮（5 个 / 行）。
fn draw_menu(app: &mut App, f: &mut ratatui::Frame, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1)])
        .split(area);
    // 第 1 行：标题
    let title = Line::from(vec![
        Span::styled("─ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "menu (全局)",
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

    // 第 2-3 行：按钮按顺序排，一行 5 个；当前按钮高亮
    let n = actions::BUTTONS.len();
    if n == 0 {
        return;
    }
    let per_row = 5usize;
    let in_menu = app.focus == Focus::Actions && app.focus_mode == FocusMode::In;
    for (row_idx, row_area) in [rows[1], rows[2]].iter().enumerate() {
        let mut spans: Vec<Span> = Vec::new();
        let mut used = 0usize;
        let max_w = row_area.width as usize;
        let start = row_idx * per_row;
        let end = (start + per_row).min(n);
        for i in start..end {
            let (icon, label) = actions::BUTTONS[i];
            let text = format!(" [{}] {} ", icon, label);
            let w = text.chars().count();
            if used + w > max_w {
                spans.push(Span::styled("…", Style::default().fg(Color::DarkGray)));
                break;
            }
            let style = if in_menu && i == app.action_idx {
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
        f.render_widget(Paragraph::new(Line::from(spans)), *row_area);
    }
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
    "方向键 选区块/选项目   Enter 进入/触发   Esc 退回上一级   ? 帮助   q 退出".into()
}

fn draw_input_line(app: &App, f: &mut ratatui::Frame, status: Rect) {
    if app.input_mode != InputMode::Cmd {
        return;
    }
    let prefix = match (&app.prompt_target, &app.input_buf) {
        (Some(t), _) => format!("{}> ", t.label()),
        (None, s) if s.starts_with("add ") => "add> ".into(),
        (None, s) if s.starts_with("edit ") => "edit> ".into(),
        (None, s) if s.starts_with("interval ") => "interval> ".into(),
        (None, s) if s.starts_with("webhook ") => "webhook> ".into(),
        (None, s) if s.starts_with("quiet ") => "quiet> ".into(),
        (None, s) if s.starts_with("phone ") => "phone> ".into(),
        (None, s) if s.starts_with("report ") => "report> ".into(),
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
            "布局：",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  左：watches    右上：details    右下：logs    底：menu (全局按钮)"),
        Line::from(""),
        Line::from(Span::styled(
            "导航规则：",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  · 方向键 = 选择"),
        Line::from("  · Enter = 进入子内容 或 触发"),
        Line::from("  · Esc   = 返回上一级"),
        Line::from(""),
        Line::from(Span::styled(
            "Top 模式（标题栏写 TOP）：",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  ←/→/↑/↓ 在 4 区块间选（Watches/Detail/Logs/Menu）"),
        Line::from("  Enter 进入当前区块的子内容（→ In 模式）"),
        Line::from("  q / Ctrl+C 退出"),
        Line::from(""),
        Line::from(Span::styled(
            "In 模式（标题栏写 IN）：",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  Watches: ↑/↓ 选 watch；Enter = 触发并跳到 Detail"),
        Line::from("  Detail:  ↑/↓/←/→ 选 per-watch 按钮；Enter 触发"),
        Line::from("           [◉ 启停] [~ 影院] [~ 日期] [~ 间隔] [r 检查] [- 删除]"),
        Line::from("  Logs:    ↑/↓ 滚事件（最近 12 条，只读）"),
        Line::from("  Menu:    ↑/↓/←/→ 选全局按钮；Enter 触发"),
        Line::from("  Esc 退回 Top"),
        Line::from(""),
        Line::from(Span::styled(
            "Menu 全局按钮：",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  [A] 添加  [D] 删除当前  [R] 立即检查  [I] 间隔  [W] webhook"),
        Line::from("  [Q] 静默  [P] 手机      [H] 报告      [?] 帮助  [q] 退出"),
        Line::from(""),
        Line::from(Span::styled(
            "其它：",
            Style::default().add_modifier(Modifier::BOLD),
        )),
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
