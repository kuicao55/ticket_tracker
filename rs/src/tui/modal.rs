//! 表单 / 选择器弹窗（modal）系统。
//!
//! 替代原来的命令行输入（`InputMode::Cmd`）：所有添加 / 编辑配置都走居中弹窗。
//!
//! - `Modal::Form`         —— 添加 watch / 编辑 watch / 全局设置（统一字段列表表单）
//! - `Modal::MovieSearch`  —— 猫眼电影列表（正在热映 / 即将上映），点选回填父表单
//! - `Modal::CinemaPicker` —— 影院收藏夹（勾选 / 输 ID 拉取加入）
//!
//! 联网请求（电影 / 影院搜索）在**后台 std::thread** 里跑自建 tokio runtime，
//! 结果经 `mpsc::channel` 回传；主循环每帧 `pump()` 用 `try_recv` 推进加载态。
//! receiver 存在网络 modal 内部：Esc 关 modal → drop receiver → 迟到结果丢弃。

use std::sync::mpsc::{self, Receiver, TryRecvError};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;

use super::{cmd, App};
use crate::config;

// ------------------------- 类型 -------------------------

pub enum Modal {
    Form(FormModal),
    MovieSearch(MovieSearchModal),
    CinemaPicker(CinemaPickerModal),
}

pub enum FormKind {
    AddWatch,
    EditWatch { wid: String },
    GlobalSettings,
}

pub enum FormMode {
    Navigation,
    Editing { original: String },
}

pub struct FormModal {
    pub kind: FormKind,
    pub title: String,
    pub fields: Vec<FormField>,
    pub focus: usize,
    pub mode: FormMode,
    pub error: Option<String>,
}

pub struct FormField {
    pub label: String,
    pub value: String,
    pub kind: FieldKind,
    pub required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Integer,
    OptionalInteger,
    DateList,
    TimeWindow,
    Webhook,
    MovieId,    // Enter → 打开 MovieSearch
    CinemaList, // Enter → 打开 CinemaPicker
    Mode,         // Enter / ←→ 在「开票提醒」「增场监控」之间切换
    WechatNotify, // Enter 在「开」「关」之间切换
    TestNotify,   // 仅 edit_watch：Enter → 给 webhook/邮箱各发一条测试消息
    Submit,
    Cancel,
}

/// 「模式」字段在表单里显示的中文标签。
pub fn mode_label(mode: &str) -> &'static str {
    if mode == config::MODE_INCREMENTAL {
        "增场监控"
    } else {
        "开票提醒"
    }
}

/// 表单里的「模式」文字 → config 里存储的 mode 值。
pub fn label_to_mode(label: &str) -> &'static str {
    if label == "增场监控" {
        config::MODE_INCREMENTAL
    } else {
        config::MODE_PRESALE
    }
}

pub enum SearchState {
    Loading(Receiver<Result<Vec<(String, String)>, String>>),
    Ready(Vec<(String, String)>),
    Error(String),
}

pub struct MovieSearchModal {
    pub show_type: u8, // 1 正在热映 / 2 即将上映
    pub selected: usize,
    pub state: SearchState,
    pub parent: Box<FormModal>,
}

pub struct CinemaChoice {
    pub id: String,
    pub name: String,
    pub builtin: bool,
    pub selected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CinemaMode {
    List,
    AddInput,
}

pub enum CinemaState {
    Ready,
    Loading(Receiver<Result<(String, String), String>>),
    Error(String),
}

pub struct CinemaPickerModal {
    pub selected: usize,
    pub cinemas: Vec<CinemaChoice>,
    pub add_input: String,
    pub mode: CinemaMode,
    pub state: CinemaState,
    pub parent: Box<FormModal>,
}

// ------------------------- 构造器 -------------------------

impl FormField {
    fn with_value(label: &str, kind: FieldKind, required: bool, value: String) -> Self {
        Self {
            label: label.into(),
            value,
            kind,
            required,
        }
    }
    fn button(label: &str, kind: FieldKind) -> Self {
        Self {
            label: label.into(),
            value: String::new(),
            kind,
            required: false,
        }
    }
}

impl FormModal {
    pub fn add_watch() -> Self {
        let fields = vec![
            FormField::with_value("电影 ID", FieldKind::MovieId, true, String::new()),
            FormField::with_value("影院", FieldKind::CinemaList, true, String::new()),
            FormField::with_value("日期", FieldKind::DateList, false, String::new()),
            FormField::with_value("时段", FieldKind::TimeWindow, false, String::new()),
            FormField::with_value("模式", FieldKind::Mode, false, "开票提醒".into()),
            FormField::with_value("电影名", FieldKind::Text, false, String::new()),
            FormField::with_value("独立间隔", FieldKind::OptionalInteger, false, String::new()),
            FormField::with_value("通知 webhook", FieldKind::Webhook, false, String::new()),
            FormField::with_value("通知邮箱", FieldKind::Text, false, String::new()),
            FormField::with_value("通知 xhs 群名", FieldKind::Text, false, String::new()),
            FormField::with_value(
                "通知微信大群",
                FieldKind::WechatNotify,
                false,
                "关".into(),
            ),
            FormField::button("确定", FieldKind::Submit),
            FormField::button("取消", FieldKind::Cancel),
        ];
        FormModal {
            kind: FormKind::AddWatch,
            title: " 添加 watch ".into(),
            fields,
            focus: 0,
            mode: FormMode::Navigation,
            error: None,
        }
    }

    pub fn edit_watch(wid: &str, focus_field: usize) -> Option<Self> {
        let cfg = config::load_or_init().ok()?;
        let w = config::find_watch(&cfg, wid)?;
        let join_arr = |key: &str| -> String {
            w.get(key)
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default()
        };
        let opt_str = |key: &str| -> String {
            w.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let cinemas = join_arr("cinemas");
        let dates = join_arr("dates");
        let interval = w
            .get("interval")
            .and_then(|v| v.as_u64())
            .map(|n| n.to_string())
            .unwrap_or_default();
        let fields = vec![
            FormField::with_value("影院", FieldKind::CinemaList, true, cinemas),
            FormField::with_value("日期", FieldKind::DateList, false, dates),
            FormField::with_value("时段", FieldKind::TimeWindow, false, opt_str("time_window")),
            FormField::with_value(
                "模式",
                FieldKind::Mode,
                false,
                mode_label(crate::config::watch_mode(w)).to_string(),
            ),
            FormField::with_value("独立间隔", FieldKind::OptionalInteger, false, interval),
            FormField::with_value(
                "通知 webhook",
                FieldKind::Webhook,
                false,
                opt_str("notify_webhook"),
            ),
            FormField::with_value(
                "通知邮箱",
                FieldKind::Text,
                false,
                opt_str("notify_email_to"),
            ),
            FormField::with_value(
                "通知 xhs 群名",
                FieldKind::Text,
                false,
                opt_str("xhs_group"),
            ),
            FormField::with_value(
                "通知微信大群",
                FieldKind::WechatNotify,
                false,
                if w.get("wechat_notify")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    "开".to_string()
                } else {
                    "关".to_string()
                },
            ),
            FormField::button("测试通知", FieldKind::TestNotify),
            FormField::button("确定", FieldKind::Submit),
            FormField::button("取消", FieldKind::Cancel),
        ];
        let focus = focus_field.min(fields.len() - 1);
        Some(FormModal {
            kind: FormKind::EditWatch {
                wid: wid.to_string(),
            },
            title: format!(" 编辑 {} ", wid),
            fields,
            focus,
            mode: FormMode::Navigation,
            error: None,
        })
    }

    pub fn global_settings(focus_field: usize) -> Self {
        let cfg = config::load_or_init().unwrap_or_else(|_| serde_json::json!({}));
        let s = |key: &str| -> String {
            cfg.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let u = |key: &str| -> String {
            cfg.get(key)
                .and_then(|v| v.as_u64())
                .map(|n| n.to_string())
                .unwrap_or_default()
        };
        let fields = vec![
            FormField::with_value(
                "Discord webhook",
                FieldKind::Webhook,
                false,
                s("discord_webhook"),
            ),
            FormField::with_value(
                "检查间隔(秒)",
                FieldKind::Integer,
                true,
                u("check_interval"),
            ),
            FormField::with_value("静默时段", FieldKind::TimeWindow, false, s("quiet_window")),
            FormField::with_value(
                "只推手机时段",
                FieldKind::TimeWindow,
                false,
                s("phone_only_window"),
            ),
            FormField::with_value(
                "报告间隔(秒)",
                FieldKind::Integer,
                true,
                u("heartbeat_interval_sec"),
            ),
            FormField::button("确定", FieldKind::Submit),
            FormField::button("取消", FieldKind::Cancel),
        ];
        let focus = focus_field.min(fields.len() - 1);
        FormModal {
            kind: FormKind::GlobalSettings,
            title: " 全局设置 ".into(),
            fields,
            focus,
            mode: FormMode::Navigation,
            error: None,
        }
    }

    /// 返回当前聚焦字段的输入提示：`(键盘操作, 输入示例)`。示例供用户对照填写。
    pub fn hint(&self) -> (&'static str, Option<&'static str>) {
        let keys = match self.mode {
            FormMode::Editing { .. } => "输入中：Enter 确认  Esc 取消本项",
            FormMode::Navigation => match self.fields.get(self.focus).map(|f| f.kind) {
                Some(FieldKind::MovieId) => {
                    "↑↓ 选择  Enter 搜索电影  i 手动输入  Esc 关闭"
                }
                Some(FieldKind::CinemaList) => "↑↓ 选择  Enter 影院收藏夹  Esc 关闭",
                Some(FieldKind::TestNotify) => {
                    "↑↓ 选择  Enter 给 webhook/邮箱/小红书/微信各发一条测试  Esc 关闭"
                }
                Some(FieldKind::Submit) | Some(FieldKind::Cancel) => {
                    "↑↓ 选择  Enter 触发  Esc 关闭"
                }
                _ => "↑↓ 选择  Enter 编辑  Esc 关闭",
            },
        };
        let example = self.fields.get(self.focus).and_then(field_example);
        (keys, example)
    }
}

/// 当前聚焦字段的输入示例（显示在 hint 下方）。空/无示例 → None。
fn field_example(field: &FormField) -> Option<&'static str> {
    use FieldKind::*;
    match field.kind {
        MovieId => Some("例如 1545360"),
        CinemaList => Some("例如 37534 12345（多个用空格或逗号分隔）"),
        DateList => Some("例如 2026-08-13 2026-08-14（留空=不限）"),
        TimeWindow => Some("例如 19:00-22:00（留空=不限）"),
        Mode => Some("Enter / ←→ 在「开票提醒」「增场监控」间切换"),
        WechatNotify => Some("Enter 在「开」「关」间切换（开启则发到当前微信会话）"),
        OptionalInteger => Some("例如 60（秒，留空=用全局默认）"),
        Integer => Some("数字"),
        Webhook => Some("例如 https://discord.com/api/webhooks/..."),
        TestNotify | Submit | Cancel => None,
        Text => match field.label.as_str() {
            "电影名" => Some("选填，留空自动从猫眼拉"),
            "通知邮箱" => Some("例如 your@email.com（需本机 msmtp，留空=关）"),
            "通知 xhs 群名" => Some("例如 test（留空=关闭该通道）"),
            _ => None,
        },
    }
}

// ------------------------- 对外入口（actions.rs 调用） -------------------------

pub fn open_add_watch(app: &mut App) {
    app.modal = Some(Modal::Form(FormModal::add_watch()));
}

pub fn open_global_settings(app: &mut App, focus: usize) {
    app.modal = Some(Modal::Form(FormModal::global_settings(focus)));
}

pub fn open_edit_watch(app: &mut App, wid: &str, focus: usize) {
    match FormModal::edit_watch(wid, focus) {
        Some(f) => app.modal = Some(Modal::Form(f)),
        None => cmd::push_status(app, format!("watch 不存在: {}", wid), 3),
    }
}

// ------------------------- 键盘处理 -------------------------

/// input.rs 在 modal 打开时调用。take → 处理 → 放回（owned 转移规避借用冲突）。
pub fn handle_key(app: &mut App, key: KeyEvent) {
    let Some(modal) = app.modal.take() else {
        return;
    };
    app.modal = match modal {
        Modal::Form(f) => handle_form_key(app, f, key),
        Modal::MovieSearch(m) => handle_movie_key(m, key),
        Modal::CinemaPicker(c) => handle_cinema_key(app, c, key),
    };
}

fn is_ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
}

fn handle_form_key(app: &mut App, mut f: FormModal, key: KeyEvent) -> Option<Modal> {
    if let FormMode::Editing { original } = &f.mode {
        let original = original.clone();
        match key.code {
            KeyCode::Enter => f.mode = FormMode::Navigation,
            KeyCode::Esc => {
                f.fields[f.focus].value = original;
                f.mode = FormMode::Navigation;
            }
            KeyCode::Backspace => {
                f.fields[f.focus].value.pop();
            }
            KeyCode::Char(c) if !is_ctrl(&key) => f.fields[f.focus].value.push(c),
            _ => {}
        }
        return Some(Modal::Form(f));
    }

    // Navigation 模式
    let n = f.fields.len();
    match key.code {
        KeyCode::Esc => None, // 关闭弹窗
        KeyCode::Up | KeyCode::Char('k') => {
            f.focus = (f.focus + n - 1) % n;
            Some(Modal::Form(f))
        }
        KeyCode::Down | KeyCode::Char('j') => {
            f.focus = (f.focus + 1) % n;
            Some(Modal::Form(f))
        }
        KeyCode::Char('i') if f.fields[f.focus].kind == FieldKind::MovieId => {
            let original = f.fields[f.focus].value.clone();
            f.mode = FormMode::Editing { original };
            Some(Modal::Form(f))
        }
        KeyCode::Enter => match f.fields[f.focus].kind {
            FieldKind::Submit => submit_form(app, f),
            FieldKind::Cancel => None,
            FieldKind::MovieId => Some(open_movie_search(f)),
            FieldKind::CinemaList => Some(open_cinema_picker(f)),
            FieldKind::Mode => {
                // Enter 在「开票提醒 / 增场监控」间切换
                let cur = &f.fields[f.focus].value;
                f.fields[f.focus].value = if cur == "增场监控" {
                    "开票提醒".to_string()
                } else {
                    "增场监控".to_string()
                };
                Some(Modal::Form(f))
            }
            FieldKind::WechatNotify => {
                // Enter 在「开 / 关」间切换
                let cur = &f.fields[f.focus].value;
                f.fields[f.focus].value = if cur == "开" {
                    "关".to_string()
                } else {
                    "开".to_string()
                };
                Some(Modal::Form(f))
            }
            FieldKind::TestNotify => {
                let msg = trigger_test_notify(&f);
                cmd::push_status(app, msg, 6);
                Some(Modal::Form(f))
            }
            _ => {
                let original = f.fields[f.focus].value.clone();
                f.mode = FormMode::Editing { original };
                Some(Modal::Form(f))
            }
        },
        _ => Some(Modal::Form(f)),
    }
}

fn handle_movie_key(mut m: MovieSearchModal, key: KeyEvent) -> Option<Modal> {
    match key.code {
        KeyCode::Esc => return Some(Modal::Form(*m.parent)),
        KeyCode::Left | KeyCode::Char('h') => {
            if m.show_type != 1 {
                m.show_type = 1;
                m.selected = 0;
                m.state = SearchState::Loading(start_movie_search(1));
            }
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if m.show_type != 2 {
                m.show_type = 2;
                m.selected = 0;
                m.state = SearchState::Loading(start_movie_search(2));
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            m.selected = m.selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let SearchState::Ready(list) = &m.state {
                if !list.is_empty() {
                    m.selected = (m.selected + 1).min(list.len() - 1);
                }
            }
        }
        KeyCode::Char('r') => {
            if matches!(m.state, SearchState::Error(_)) {
                m.state = SearchState::Loading(start_movie_search(m.show_type));
            }
        }
        KeyCode::Enter => {
            if let SearchState::Ready(list) = &m.state {
                if let Some((id, name)) = list.get(m.selected).cloned() {
                    let mut parent = *m.parent;
                    if let Some(i) = parent
                        .fields
                        .iter()
                        .position(|x| x.kind == FieldKind::MovieId)
                    {
                        parent.fields[i].value = id;
                    }
                    if let Some(i) = parent.fields.iter().position(|x| x.kind == FieldKind::Text) {
                        parent.fields[i].value = name;
                    }
                    return Some(Modal::Form(parent));
                }
            }
        }
        _ => {}
    }
    Some(Modal::MovieSearch(m))
}

fn handle_cinema_key(app: &mut App, mut c: CinemaPickerModal, key: KeyEvent) -> Option<Modal> {
    if c.mode == CinemaMode::AddInput {
        match key.code {
            KeyCode::Esc => {
                c.mode = CinemaMode::List;
                c.state = CinemaState::Ready;
            }
            KeyCode::Enter => {
                let id = c.add_input.trim().to_string();
                if !id.is_empty() {
                    c.state = CinemaState::Loading(start_cinema_lookup(id));
                }
            }
            KeyCode::Backspace => {
                c.add_input.pop();
            }
            KeyCode::Char(ch) if !is_ctrl(&key) => c.add_input.push(ch),
            _ => {}
        }
        return Some(Modal::CinemaPicker(c));
    }

    // List 模式
    match key.code {
        KeyCode::Esc => return Some(Modal::Form(*c.parent)),
        KeyCode::Tab => c.mode = CinemaMode::AddInput,
        KeyCode::Up | KeyCode::Char('k') => c.selected = c.selected.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            if !c.cinemas.is_empty() {
                c.selected = (c.selected + 1).min(c.cinemas.len() - 1);
            }
        }
        KeyCode::Char(' ') => {
            if let Some(ch) = c.cinemas.get_mut(c.selected) {
                ch.selected = !ch.selected;
            }
        }
        KeyCode::Char('d') | KeyCode::Delete => {
            if let Some((id, builtin)) = c
                .cinemas
                .get(c.selected)
                .map(|ch| (ch.id.clone(), ch.builtin))
            {
                if builtin {
                    c.state = CinemaState::Error("内置影院不能删除".into());
                } else {
                    let result = {
                        let mut cfg = app.monitor.shared.cfg.lock().unwrap();
                        config::remove_cinema(&mut cfg, &id)
                    };
                    match result {
                        Ok(true) => {
                            c.cinemas.remove(c.selected);
                            c.selected = c.selected.min(c.cinemas.len().saturating_sub(1));
                            c.state = CinemaState::Ready;
                        }
                        Ok(false) => c.state = CinemaState::Error("影院收藏不存在".into()),
                        Err(e) => c.state = CinemaState::Error(e.to_string()),
                    }
                }
            }
        }
        KeyCode::Enter => {
            // 确定：把已勾选影院 id 写回父表单 CinemaList 字段
            let ids: Vec<String> = c
                .cinemas
                .iter()
                .filter(|x| x.selected)
                .map(|x| x.id.clone())
                .collect();
            let mut parent = *c.parent;
            if let Some(i) = parent
                .fields
                .iter()
                .position(|x| x.kind == FieldKind::CinemaList)
            {
                parent.fields[i].value = ids.join(" ");
            }
            return Some(Modal::Form(parent));
        }
        _ => {}
    }
    Some(Modal::CinemaPicker(c))
}

// ------------------------- 打开子选择器 -------------------------

fn open_movie_search(parent: FormModal) -> Modal {
    Modal::MovieSearch(MovieSearchModal {
        show_type: 1,
        selected: 0,
        state: SearchState::Loading(start_movie_search(1)),
        parent: Box::new(parent),
    })
}

fn open_cinema_picker(parent: FormModal) -> Modal {
    let preselected: Vec<String> = parent
        .fields
        .iter()
        .find(|x| x.kind == FieldKind::CinemaList)
        .map(|x| split_ids(&x.value))
        .unwrap_or_default();
    let cfg = config::load_or_init().unwrap_or_else(|_| serde_json::json!({ "cinemas": [] }));
    let mut cinemas: Vec<CinemaChoice> = cfg
        .get("cinemas")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|c| {
                    let id = c
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = c
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let builtin = c.get("builtin").and_then(|v| v.as_bool()).unwrap_or(false);
                    let selected = preselected.contains(&id);
                    CinemaChoice {
                        id,
                        name,
                        builtin,
                        selected,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    // 已在表单里但不在收藏夹的 id 也补进来（保持勾选）
    for pid in &preselected {
        if !cinemas.iter().any(|c| &c.id == pid) {
            cinemas.push(CinemaChoice {
                id: pid.clone(),
                name: String::new(),
                builtin: false,
                selected: true,
            });
        }
    }
    Modal::CinemaPicker(CinemaPickerModal {
        selected: 0,
        cinemas,
        add_input: String::new(),
        mode: CinemaMode::List,
        state: CinemaState::Ready,
        parent: Box::new(parent),
    })
}

// ------------------------- 提交（直接调 config::） -------------------------

fn submit_form(app: &mut App, mut f: FormModal) -> Option<Modal> {
    let result = match &f.kind {
        FormKind::AddWatch => submit_add_watch(app, &f),
        FormKind::EditWatch { wid } => submit_edit_watch(app, wid, &f),
        FormKind::GlobalSettings => submit_global(app, &f),
    };
    match result {
        Ok(msg) => {
            cmd::push_status(app, msg, 4);
            None
        }
        Err(e) => {
            f.error = Some(e);
            Some(Modal::Form(f))
        }
    }
}

fn submit_add_watch(app: &App, f: &FormModal) -> Result<String, String> {
    let movie_id: i64 = f.fields[0]
        .value
        .trim()
        .parse()
        .map_err(|_| "电影 ID 必须是数字".to_string())?;
    let cinemas = split_ids(&f.fields[1].value);
    if cinemas.is_empty() {
        return Err("至少填一个影院 ID".into());
    }
    let dates = parse_dates(&f.fields[2].value)?;
    let time_window = {
        let t = f.fields[3].value.trim();
        if t.is_empty() { None } else { Some(t.to_string()) }
    };
    let mode = label_to_mode(f.fields[4].value.trim());
    let name = {
        let t = f.fields[5].value.trim();
        if t.is_empty() { None } else { Some(t.to_string()) }
    };
    let interval = parse_opt_u64(&f.fields[6].value, "间隔")?;
    let notify_webhook = {
        let t = f.fields[7].value.trim();
        if t.is_empty() { None } else { Some(t.to_string()) }
    };
    let notify_email_to = {
        let t = f.fields[8].value.trim();
        if t.is_empty() { None } else { Some(t.to_string()) }
    };
    let xhs_group = {
        let t = f.fields[9].value.trim();
        if t.is_empty() { None } else { Some(t.to_string()) }
    };
    let wechat_notify = f.fields[10].value.trim() == "开";
    let mut cfg = app.monitor.shared.cfg.lock().unwrap();
    let cref: Vec<&str> = cinemas.iter().map(String::as_str).collect();
    let id = config::add_watch(
        &mut cfg,
        movie_id,
        &cref,
        dates.as_deref(),
        name.as_deref(),
        interval,
        mode,
        time_window.as_deref(),
        notify_webhook.as_deref(),
        notify_email_to.as_deref(),
        xhs_group.as_deref(),
        wechat_notify,
    )
    .map_err(|e| e.to_string())?;
    Ok(format!("已添加 watch {}", id))
}

fn submit_edit_watch(app: &App, wid: &str, f: &FormModal) -> Result<String, String> {
    let cinemas = split_ids(&f.fields[0].value);
    let dates = parse_dates(&f.fields[1].value)?;
    let time_window = {
        let t = f.fields[2].value.trim();
        if t.is_empty() { None } else { Some(t.to_string()) }
    };
    let mode = label_to_mode(f.fields[3].value.trim());
    let interval = parse_opt_u64(&f.fields[4].value, "间隔")?;
    let notify_webhook = {
        let t = f.fields[5].value.trim();
        if t.is_empty() { None } else { Some(t.to_string()) }
    };
    let notify_email_to = {
        let t = f.fields[6].value.trim();
        if t.is_empty() { None } else { Some(t.to_string()) }
    };
    let xhs_group = {
        let t = f.fields[7].value.trim();
        if t.is_empty() { None } else { Some(t.to_string()) }
    };
    let wechat_notify = f.fields[8].value.trim() == "开";
    let mut cfg = app.monitor.shared.cfg.lock().unwrap();
    // 注册尚未收藏的影院
    for cid in &cinemas {
        if config::find_cinema(&cfg, cid).is_none() {
            config::add_cinema(&mut cfg, cid, None).map_err(|e| e.to_string())?;
        }
    }
    let w =
        config::find_watch_mut(&mut cfg, wid).ok_or_else(|| format!("watch 不存在: {}", wid))?;
    w["cinemas"] = serde_json::json!(cinemas);
    w["dates"] = match dates {
        Some(d) => serde_json::json!(d),
        None => Value::Null,
    };
    // 时段窗口：空 = 移除配置；非空 = 落盘
    let tw = &time_window;
    match tw {
        Some(s) if !s.trim().is_empty() => {
            config::parse_window(s).map_err(|e| format!("时段格式错误: {}", e))?;
            w["time_window"] = serde_json::Value::String(s.clone());
        }
        _ => {
            if let Some(o) = w.as_object_mut() {
                o.remove("time_window");
            }
        }
    }
    w["mode"] = serde_json::json!(mode);
    match interval {
        Some(secs) => w["interval"] = serde_json::json!(secs),
        None => {
            if let Some(o) = w.as_object_mut() {
                o.remove("interval");
            }
        }
    }
    // 通知通道
    match &notify_webhook {
        Some(s) => w["notify_webhook"] = serde_json::Value::String(s.clone()),
        None => {
            if let Some(o) = w.as_object_mut() {
                o.remove("notify_webhook");
            }
        }
    }
    match &notify_email_to {
        Some(s) => w["notify_email_to"] = serde_json::Value::String(s.clone()),
        None => {
            if let Some(o) = w.as_object_mut() {
                o.remove("notify_email_to");
            }
        }
    }
    match &xhs_group {
        Some(s) => w["xhs_group"] = serde_json::Value::String(s.clone()),
        None => {
            if let Some(o) = w.as_object_mut() {
                o.remove("xhs_group");
            }
        }
    }
    w["wechat_notify"] = serde_json::json!(wechat_notify);
    // 影院/日期/时段/模式变化 → 清 seen_shows（与 cli 行为对齐）
    let _ = w; // suppress unused warning if not used below
    let any_baseline_change = true; // 简化：保存路径上无 seen_shows diff，记住清一次
    if any_baseline_change {
        if let Some(o) = w.as_object_mut() {
            o.remove("seen_shows");
        }
    }
    config::save(&cfg).map_err(|e| e.to_string())?;
    Ok(format!("已更新 {}", wid))
}

fn submit_global(app: &App, f: &FormModal) -> Result<String, String> {
    let webhook = f.fields[0].value.trim().to_string();
    let interval: u64 = f.fields[1]
        .value
        .trim()
        .parse()
        .map_err(|_| "检查间隔必须是数字".to_string())?;
    let quiet = f.fields[2].value.trim().to_string();
    let phone = f.fields[3].value.trim().to_string();
    let hb: u64 = f.fields[4]
        .value
        .trim()
        .parse()
        .map_err(|_| "报告间隔必须是数字".to_string())?;
    if !quiet.is_empty() {
        config::parse_window(&quiet).map_err(|e| e.to_string())?;
    }
    if !phone.is_empty() {
        config::parse_window(&phone).map_err(|e| e.to_string())?;
    }
    let mut cfg = app.monitor.shared.cfg.lock().unwrap();
    cfg["discord_webhook"] = if webhook.is_empty() {
        Value::Null
    } else {
        serde_json::json!(webhook)
    };
    cfg["check_interval"] = serde_json::json!(interval);
    if !quiet.is_empty() {
        cfg["quiet_window"] = serde_json::json!(quiet);
    }
    if !phone.is_empty() {
        cfg["phone_only_window"] = serde_json::json!(phone);
    }
    cfg["heartbeat_interval_sec"] = serde_json::json!(hb);
    config::save(&cfg).map_err(|e| e.to_string())?;
    Ok("全局设置已保存".into())
}

/// 编辑表单里的「测试通知」按钮：给当前表单里配的 webhook / 邮箱 / 小红书 / 微信各发一条
/// 客户可读的测试消息。返回值交给底部 status bar 显示执行结果。
///
/// 表单字段索引（按 `edit_watch` 现在的布局）：
///   0 cinemas | 1 dates | 2 time_window | 3 mode | 4 interval
///   5 notify_webhook | 6 notify_email_to | 7 通知 xhs 群名 | 8 通知微信大群
///   9 测试通知 | 10 确定 | 11 取消
fn trigger_test_notify(f: &FormModal) -> String {
    // 索引 5/6/7/8 才是真的 webhook / email / xhs 群名 / 微信开关
    let webhook = f.fields.get(5).map(|x| x.value.trim().to_string()).unwrap_or_default();
    let email = f.fields.get(6).map(|x| x.value.trim().to_string()).unwrap_or_default();
    let xhs_group = f.fields.get(7).map(|x| x.value.trim().to_string()).unwrap_or_default();
    let wechat_on = f.fields.get(8).map(|x| x.value.trim() == "开").unwrap_or(false);
    let cinemas_in_form = f.fields.get(0).map(|x| x.value.trim().to_string()).unwrap_or_default();
    let dates_in_form = f.fields.get(1).map(|x| x.value.trim().to_string()).unwrap_or_default();
    let tw_in_form = f.fields.get(2).map(|x| x.value.trim().to_string()).unwrap_or_default();
    let mode_label_in_form = f.fields.get(3).map(|x| x.value.trim().to_string()).unwrap_or_default();
    let interval_in_form = f.fields.get(4).map(|x| x.value.trim().to_string()).unwrap_or_default();

    // 从 config 里捞 movie_name / movie_id（form 不显示这两个），
    // 同时拿两层兜底的影院名（注册表 → _last_payload）。
    let (movie_name, movie_id, wid_str, registry_names, payload_names) = match &f.kind {
        FormKind::EditWatch { wid } => {
            if let Ok(cfg) = crate::config::load_or_init() {
                if let Some(w) = crate::config::find_watch(&cfg, wid) {
                    let n = w
                        .get("movie_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(未命名)")
                        .to_string();
                    let id = w.get("movie_id").and_then(|v| v.as_i64()).unwrap_or(0);
                    let reg: std::collections::HashMap<String, String> = cfg
                        .get("cinemas")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|c| {
                                    let id = c.get("id").and_then(|v| v.as_str())?;
                                    let n = c.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                    if n.is_empty() || n.starts_with("影城 ") {
                                        None
                                    } else {
                                        Some((id.to_string(), n.to_string()))
                                    }
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let pay: std::collections::HashMap<String, String> = w
                        .get("_last_payload")
                        .and_then(|p| p.get("cinema_names"))
                        .and_then(|c| c.as_object())
                        .map(|o| {
                            o.iter()
                                .filter_map(|(k, v)| {
                                    v.as_str().map(|s| (k.clone(), s.to_string()))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    (n, id, wid.clone(), reg, pay)
                } else {
                    (wid.clone(), 0, wid.clone(), Default::default(), Default::default())
                }
            } else {
                (wid.clone(), 0, wid.clone(), Default::default(), Default::default())
            }
        }
        _ => (
            "(未知)".into(),
            0,
            "(未知)".into(),
            Default::default(),
            Default::default(),
        ),
    };

    // 把每个影院显示成「名称 (id)」，没拉到名称的 cinema 就只显示 id。
    let cinemas_display = if cinemas_in_form.is_empty() {
        "（未配）".to_string()
    } else {
        cinemas_in_form
            .split_whitespace()
            .map(|cid| {
                let name = registry_names
                    .get(cid)
                    .cloned()
                    .or_else(|| payload_names.get(cid).cloned())
                    .unwrap_or_default();
                if name.is_empty() {
                    cid.to_string()
                } else {
                    format!("{} ({})", name, cid)
                }
            })
            .collect::<Vec<_>>()
            .join("、")
    };
    let dates_display = if dates_in_form.is_empty() {
        "不限".to_string()
    } else {
        dates_in_form
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let tw_display = if tw_in_form.is_empty() {
        "不限".to_string()
    } else {
        tw_in_form.clone()
    };
    let interval_display = if interval_in_form.is_empty() {
        "默认 (90s)".to_string()
    } else {
        format!("{}s", interval_in_form)
    };
    let now_str = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mode_disp = if mode_label_in_form.is_empty() {
        "开票提醒".to_string()
    } else {
        mode_label_in_form.clone()
    };
    let webhook_preview = if webhook.is_empty() {
        "（未填）".to_string()
    } else {
        webhook.clone()
    };
    let email_preview = if email.is_empty() {
        "（未填）".to_string()
    } else {
        email.clone()
    };
    let xhs_preview = if xhs_group.is_empty() {
        "（未填）".to_string()
    } else {
        xhs_group.clone()
    };

    let title = "🎬 ticket-tracker · 测试通知".to_string();
    let msg = format!(
        "你好，这是一条来自 ticket-tracker 的测试通知，用于确认通知通道已配置正确。\n\
         \n\
         ── 监视任务 ──\n\
         任务 ID    : {wid}\n\
         影片       : {movie_name} (ID {movie_id})\n\
         模式       : {mode}\n\
         影院       : {cinemas}\n\
         日期       : {dates}\n\
         时段窗口   : {tw}\n\
         独立间隔   : {interval}\n\
         \n\
         ── 通知通道 ──\n\
         Webhook    : {webhook}\n\
         邮箱       : {email}\n\
         XHS 群     : {xhs}\n\
         \n\
         ── 触发信息 ──\n\
         触发人     : ticket-tracker（手动测试）\n\
         触发时间   : {now}\n\
         \n\
         收到此邮件即说明通知通道已配置正确，正式告警将沿用同一模板。\n\
         如非本人操作，请联系管理员。",
        wid = wid_str,
        movie_name = movie_name,
        movie_id = movie_id,
        mode = mode_disp,
        cinemas = cinemas_display,
        dates = dates_display,
        tw = tw_display,
        interval = interval_display,
        webhook = webhook_preview,
        email = email_preview,
        xhs = xhs_preview,
        now = now_str,
    );

    let mut results: Vec<String> = Vec::new();
    if webhook.is_empty() {
        results.push("webhook 未填（仅发邮箱）".to_string());
    } else {
        let rt = tokio::runtime::Runtime::new();
        let sent = match rt {
            Ok(rt) => rt.block_on(crate::notify::notify_results_webhook_async(
                Some(&webhook),
                &title,
                &msg,
                None,
            )),
            Err(e) => Err(anyhow::anyhow!(e.to_string())),
        };
        results.push(match sent {
            Ok(true) => "webhook ✓ 已送达".to_string(),
            Ok(false) => "webhook ✗ 发送失败（HTTP 错误）".to_string(),
            Err(e) => format!("webhook ✗ 内部错误: {}", e),
        });
    }
    if email.is_empty() {
        results.push("邮箱 未填（仅发 webhook）".to_string());
    } else {
        let ok = crate::notify::notify_results_email(Some(&email), &title, &msg, None);
        results.push(if ok {
            "邮箱 ✓ 已交付 msmtp".to_string()
        } else {
            "邮箱 ✗ msmtp 未安装或配置有误".to_string()
        });
    }
    if xhs_group.is_empty() {
        results.push("XHS 群 未填（不测试）".to_string());
    } else {
        let xhs_body = format!(
            "【ticket-tracker 自动化测试通知】本条消息由 ticket-tracker 监控服务自动发出,用于验证小红书群通知通道配置。｜ 任务 ID: {wid} ｜ 影片: {movie_name} ｜ 影院: {cinemas} ｜ 收到本消息无需任何操作,正式告警将沿用相同通道发送。｜ 如非本人操作或频繁收到此类消息,请联系管理员。",
            wid = wid_str,
            movie_name = movie_name,
            cinemas = cinemas_display
        );
        let ok = crate::notify::notify_xhs(&xhs_group, "ticket-tracker 自动化测试通知", &xhs_body, None);
        results.push(if ok {
            "XHS 群 ✓ 已发送".to_string()
        } else {
            "XHS 群 ✗ 发送失败（看 stderr）".to_string()
        });
    }
    if wechat_on {
        // 微信测试通知：沿用真实告警模板，开头加 🧪 标识这是测试；末尾附购票链接
        // （测试用 https://www.maoyan.com/cinema/37534 ，与正式告警链接同字段可点回猫眼）
        let wechat_msg = format!(
            "🧪 测试通知\n\n🎬 预售开启\n\n🎞 {}\n🏛 {}\n📅 测试场次｜N 场｜HH:MM 至 HH:MM\n\n👉 https://www.maoyan.com/cinema/37534",
            movie_name, cinemas_display
        );
        let ok = crate::notify::notify_wechat(&wechat_msg);
        results.push(if ok {
            "微信大群 ✓ 已发送".to_string()
        } else {
            "微信大群 ✗ 发送失败（看 stderr）".to_string()
        });
    } else {
        results.push("微信大群 关（不测试）".to_string());
    }
    let any_failed = results.iter().any(|s| {
        s.starts_with("webhook ✗")
            || s.starts_with("邮箱 ✗")
            || s.starts_with("XHS 群 ✗")
            || s.starts_with("微信大群 ✗")
    });
    let prefix = if any_failed { "部分通道失败： " } else { "" };
    format!("{prefix}{}", results.join("  "))
}

/// 空白或逗号分隔 → 去空 id 列表。
fn split_ids(s: &str) -> Vec<String> {
    s.split(|c: char| c.is_whitespace() || c == ',')
        .filter(|x| !x.is_empty())
        .map(String::from)
        .collect()
}

fn parse_dates(s: &str) -> Result<Option<Vec<String>>, String> {
    let list = split_ids(s);
    if list.is_empty() {
        return Ok(None);
    }
    for d in &list {
        if d.len() != 10 || d.as_bytes().get(4) != Some(&b'-') {
            return Err(format!("日期格式应为 YYYY-MM-DD: {}", d));
        }
    }
    Ok(Some(list))
}

fn parse_opt_u64(s: &str, label: &str) -> Result<Option<u64>, String> {
    let t = s.trim();
    if t.is_empty() {
        return Ok(None);
    }
    t.parse::<u64>()
        .map(Some)
        .map_err(|_| format!("{} 必须是数字或留空", label))
}

// ------------------------- 后台 worker -------------------------

fn start_movie_search(show_type: u8) -> Receiver<Result<Vec<(String, String)>, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let res = (|| {
            let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
            rt.block_on(crate::maoyan::fetch_films_list_async(show_type))
                .map_err(|e| e.to_string())
        })();
        let _ = tx.send(res);
    });
    rx
}

fn start_cinema_lookup(id: String) -> Receiver<Result<(String, String), String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let res = (|| -> Result<(String, String), String> {
            let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
            let v = rt
                .block_on(crate::maoyan::fetch_cinema_async(&id))
                .map_err(|e| e.to_string())?;
            let cid = v
                .get("cinema_id")
                .and_then(|x| x.as_str())
                .unwrap_or(&id)
                .to_string();
            let name = v
                .get("cinema_name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            Ok((cid, name))
        })();
        let _ = tx.send(res);
    });
    rx
}

// ------------------------- 每帧推进加载态 -------------------------

/// 主循环每帧调用（draw 之前）。用 `try_recv` 推进 Loading → Ready/Error。
pub fn pump(app: &mut App) {
    let Some(modal) = app.modal.take() else {
        return;
    };
    let modal = match modal {
        Modal::MovieSearch(mut m) => {
            m.state = pump_search(m.state);
            Modal::MovieSearch(m)
        }
        Modal::CinemaPicker(mut c) => {
            pump_cinema(app, &mut c);
            Modal::CinemaPicker(c)
        }
        other => other,
    };
    app.modal = Some(modal);
}

fn pump_search(state: SearchState) -> SearchState {
    match state {
        SearchState::Loading(rx) => match rx.try_recv() {
            Ok(Ok(list)) => SearchState::Ready(list),
            Ok(Err(e)) => SearchState::Error(e),
            Err(TryRecvError::Empty) => SearchState::Loading(rx),
            Err(TryRecvError::Disconnected) => SearchState::Error("请求线程中断".into()),
        },
        other => other,
    }
}

fn pump_cinema(app: &App, c: &mut CinemaPickerModal) {
    let state = std::mem::replace(&mut c.state, CinemaState::Ready);
    match state {
        CinemaState::Loading(rx) => match rx.try_recv() {
            Ok(Ok((id, name))) => {
                let save_result = {
                    let mut cfg = app.monitor.shared.cfg.lock().unwrap();
                    config::add_cinema(&mut cfg, &id, Some(&name))
                };
                if let Err(e) = save_result {
                    c.state = CinemaState::Error(e.to_string());
                    return;
                }
                if let Some(ch) = c.cinemas.iter_mut().find(|x| x.id == id) {
                    ch.selected = true;
                    if !name.is_empty() {
                        ch.name = name;
                    }
                } else {
                    c.cinemas.push(CinemaChoice {
                        id,
                        name,
                        builtin: false,
                        selected: true,
                    });
                }
                c.add_input.clear();
                c.mode = CinemaMode::List;
                c.state = CinemaState::Ready;
            }
            Ok(Err(e)) => c.state = CinemaState::Error(e),
            Err(TryRecvError::Empty) => c.state = CinemaState::Loading(rx),
            Err(TryRecvError::Disconnected) => c.state = CinemaState::Error("请求线程中断".into()),
        },
        other => c.state = other,
    }
}
