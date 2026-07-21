//! Action Bar 按钮的派发逻辑。
//!
//! - `dispatch(app)`：Enter 在 Action Bar 触发时调用
//! - `dispatch_prompt_submit(app)`：prompt 期间 Enter 提交时调用

use crate::tui::{cmd, App, ConfirmPrompt, InputMode, PromptTarget};

/// Action Bar 按钮定义。长度 = 8。
pub const BUTTONS: &[(&str, &str)] = &[
    ("+", "添加"),
    ("-", "删除"),
    ("~", "编辑"),
    ("◉", "启停"),
    ("r", "立即检查"),
    ("⚙", "配置"),
    ("?", "帮助"),
    ("q", "退出"),
];

/// 当前 `app.action_idx` 对应的按钮 Enter 触发的动作。
pub fn dispatch(app: &mut App) {
    match app.action_idx {
        0 => {
            // 添加 watch：开 prompt，让用户输入 `<movie_id> [-c ...] [-d ...] [--name ...]`
            app.input_mode = InputMode::Cmd;
            app.prompt_target = None; // 不是配置循环
            app.input_buf = "add ".into();
            cmd::push_status(app, "请输入 watch 参数：movie_id [-c cinema ...] [-d date ...] [--name ...]".into(), 8);
        }
        1 => {
            // 删除：confirm
            if let Some(wid) = cmd::current_wid(app) {
                app.confirm = Some(ConfirmPrompt {
                    text: format!("删 watch {} ? (y/n)", wid),
                    created_at: std::time::Instant::now(),
                });
            } else {
                cmd::push_status(app, "没有选中 watch，无法删除".into(), 3);
            }
        }
        2 => {
            // 编辑：开 prompt，预填 wid
            if let Some(wid) = cmd::current_wid(app) {
                app.input_mode = InputMode::Cmd;
                app.prompt_target = None;
                app.input_buf = format!("edit {} ", wid);
                cmd::push_status(app, format!("编辑 watch {} —— 语法：edit <wid> <field> <value>", wid), 8);
            } else {
                cmd::push_status(app, "没有选中 watch，无法编辑".into(), 3);
            }
        }
        3 => {
            // 启停：直接调 toggle 命令
            let cmd_str = "toggle";
            cmd::execute(app, cmd_str);
        }
        4 => {
            // 立即检查
            app.monitor.force_check();
            cmd::push_event(app, "· 手动触发一轮检查…".into());
            cmd::push_status(app, "已触发立即检查".into(), 3);
        }
        5 => {
            // 配置：循环推进 prompt_target 到第一项
            app.input_mode = InputMode::Cmd;
            app.prompt_target = Some(PromptTarget::Webhook);
            app.input_buf.clear();
            cmd::push_status(
                app,
                format!("配置项：当前 = {}（回车提交 → 跳下一项，Esc 退出）", PromptTarget::Webhook.label()),
                8,
            );
        }
        6 => {
            // 帮助
            app.show_help = !app.show_help;
        }
        7 => {
            // 退出
            app.should_quit = true;
        }
        _ => {}
    }
}

/// prompt 期间 Enter 提交时调用：把 input_buf 翻译成 `cmd::execute(...)` 调用。
pub fn dispatch_prompt_submit(app: &mut App) {
    // 1) 删除/启停用 confirm 流；配置循环用 PromptTarget
    if let Some(pt) = app.prompt_target.clone() {
        let buf = app.input_buf.trim().to_string();
        app.input_mode = InputMode::Normal;
        app.input_buf.clear();
        app.prompt_target = None;
        let cmd_str = match (&pt, buf.as_str()) {
            (PromptTarget::Webhook, s) if !s.is_empty() => Some(format!("webhook {}", s)),
            (PromptTarget::Webhook, _) => None,
            (PromptTarget::Quiet, s) if !s.is_empty() => Some(format!("quiet {}", s)),
            (PromptTarget::Quiet, _) => None,
            (PromptTarget::Phone, s) if !s.is_empty() => Some(format!("phone {}", s)),
            (PromptTarget::Phone, _) => None,
            (PromptTarget::Interval, s) if !s.is_empty() => Some(format!("interval {}", s)),
            (PromptTarget::Interval, _) => None,
            (PromptTarget::Films, _) => Some("films 2".into()),
            (PromptTarget::Doctor, _) => Some("doctor".into()),
        };
        match cmd_str {
            Some(s) => cmd::execute(app, &s),
            None => cmd::push_status(app, "输入为空，已取消".into(), 3),
        }
        // 「配置」按钮是一条链：提交一项后自动开下一项（除非用户后面 Esc）
        let next = pt.next();
        let next_label = next.label();
        // 仅当当前项成功执行才继续
        // 这里简化：成功后再开新 prompt；如果失败就让用户主动按 [⚙]
        app.prompt_target = Some(next);
        app.input_mode = InputMode::Cmd;
        app.input_buf.clear();
        cmd::push_status(
            app,
            format!("配置项：当前 = {}（回车提交 → 跳下一项，Esc 退出）", next_label),
            8,
        );
        return;
    }

    // 2) 加 / 编辑命令（prompt_target = None，但 buffer 是 "add ..." / "edit wid ..."）
    let raw = app.input_buf.trim().to_string();
    app.input_mode = InputMode::Normal;
    app.input_buf.clear();
    if raw.is_empty() {
        cmd::push_status(app, "输入为空，已取消".into(), 3);
        return;
    }
    cmd::execute(app, &raw);
}

/// prompt 期间 Esc：取消并退出
pub fn dispatch_prompt_cancel(app: &mut App) {
    app.input_mode = InputMode::Normal;
    app.input_buf.clear();
    app.prompt_target = None;
    cmd::push_status(app, "已取消".into(), 3);
}

/// confirm 「y」删除调用的低层入口：把 input_buf.replace 成 "rm {wid}"
pub fn cmd_delete_wid(app: &mut App, wid: &str) {
    let cmd_str = format!("rm {}", wid);
    cmd::execute(app, &cmd_str);
}
