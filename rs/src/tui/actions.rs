//! Action Bar 按钮的派发逻辑。
//!
//! - `dispatch(app)`：Enter 在 Action Bar 触发时调用
//! - `dispatch_prompt_submit(app)`：prompt 期间 Enter 提交时调用

use crate::tui::{cmd, modal, App, ConfirmPrompt, InputMode, PromptTarget};

/// 底部 Menu Bar：全部是**全局动作**。per-watch 动作（如启停单条、删除单条）
/// 全部放在 Detail 列内的 per-watch 按钮行；这里不放。
pub const BUTTONS: &[(&str, &str)] = &[
    ("A", "添加"),     // add watch (global)
    ("D", "删除"),     // delete current watch (or dialog if none selected)
    ("R", "立即检查"), // force check all
    ("I", "间隔"),     // global check_interval
    ("W", "webhook"),  // global discord_webhook
    ("Q", "静默"),     // global quiet_window
    ("P", "手机"),     // global phone_only_window
    ("H", "报告"),     // global heartbeat_interval_sec
    ("?", "帮助"),
    ("q", "退出"),
];

/// Detail 列内的 per-watch 按钮（仅 Detail In 模式生效）。长度 = 6。
pub const DETAIL_BUTTONS: &[(&str, &str)] = &[
    ("◉", "启停"),
    ("~", "影院"),
    ("~", "日期"),
    ("~", "间隔"),
    ("r", "立即检查"),
    ("-", "删除"),
];

/// 当前 `app.action_idx` 对应的按钮 Enter 触发的动作。
pub fn dispatch(app: &mut App) {
    match app.action_idx {
        0 => {
            // 添加 watch：打开表单，可从电影搜索与影院收藏夹中选择。
            modal::open_add_watch(app);
        }
        1 => {
            // 删除当前 watch（走 confirm）
            if let Some(wid) = cmd::current_wid(app) {
                app.confirm = Some(ConfirmPrompt {
                    text: format!("删 watch {} ? (y/n)", wid),
                    created_at: std::time::Instant::now(),
                });
            } else {
                cmd::push_status(app, "没有选中 watch，请先在左栏选一条".into(), 3);
            }
        }
        2 => {
            // 立即检查（全局 force_check_all）
            app.monitor.force_check_all();
            cmd::push_event(app, "· 手动触发一轮检查…".into());
            cmd::push_status(app, "已触发立即检查".into(), 3);
        }
        3 => {
            // 全局检查间隔
            modal::open_global_settings(app, 1);
        }
        4 => {
            // 全局 Discord webhook
            modal::open_global_settings(app, 0);
        }
        5 => {
            // 全局静默时段
            modal::open_global_settings(app, 2);
        }
        6 => {
            // 全局只推手机时段
            modal::open_global_settings(app, 3);
        }
        7 => {
            // 全局报告间隔
            modal::open_global_settings(app, 4);
        }
        8 => {
            // 帮助
            app.show_help = !app.show_help;
        }
        9 => {
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
            format!(
                "配置项：当前 = {}（回车提交 → 跳下一项，Esc 退出）",
                next_label
            ),
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

/// Detail In 模式下按 Enter 触发当前按钮。
/// 顺序与 `DETAIL_BUTTONS` 一致：0=启停, 1=影院, 2=日期, 3=间隔, 4=立即检查, 5=删除。
pub fn dispatch_detail_action(app: &mut App, btn_idx: usize) {
    let wid = match cmd::current_wid(app) {
        Some(w) => w,
        None => {
            cmd::push_status(app, "没有选中 watch".into(), 3);
            return;
        }
    };
    match btn_idx {
        0 => {
            // 启停
            cmd::execute(app, "toggle");
        }
        1 => {
            // 编辑影院：打开 watch 表单并聚焦影院字段。
            modal::open_edit_watch(app, &wid, 0);
        }
        2 => {
            // 编辑日期：打开 watch 表单并聚焦日期字段。
            modal::open_edit_watch(app, &wid, 1);
        }
        3 => {
            // 编辑间隔：打开 watch 表单并聚焦间隔字段。
            modal::open_edit_watch(app, &wid, 2);
        }
        4 => {
            // per-watch 立即检查
            app.monitor.force_check_wid(wid.clone());
            cmd::push_event(app, format!("· 手动检查 watch {} …", wid));
            cmd::push_status(app, format!("已触发 {} 立即检查", wid), 3);
        }
        5 => {
            // 删除（走 confirm）
            app.confirm = Some(ConfirmPrompt {
                text: format!("删 watch {} ? (y/n)", wid),
                created_at: std::time::Instant::now(),
            });
        }
        _ => {}
    }
}
