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
    Submit,
    Cancel,
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
    fn new(label: &str, kind: FieldKind, required: bool) -> Self {
        Self {
            label: label.into(),
            value: String::new(),
            kind,
            required,
        }
    }
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
            FormField::new("电影 ID", FieldKind::MovieId, true),
            FormField::new("影院", FieldKind::CinemaList, true),
            FormField::new("日期", FieldKind::DateList, false),
            FormField::new("电影名", FieldKind::Text, false),
            FormField::new("独立间隔", FieldKind::OptionalInteger, false),
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
            FormField::with_value("独立间隔", FieldKind::OptionalInteger, false, interval),
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

    /// 返回当前聚焦字段的输入提示（渲染底部用）。
    pub fn hint(&self) -> &'static str {
        match self.mode {
            FormMode::Editing { .. } => "输入中：Enter 确认  Esc 取消本项",
            FormMode::Navigation => match self.fields.get(self.focus).map(|f| f.kind) {
                Some(FieldKind::MovieId) => {
                    "↑↓ 选择  Enter 搜索电影  i 手动输入  Esc 关闭"
                }
                Some(FieldKind::CinemaList) => "↑↓ 选择  Enter 影院收藏夹  Esc 关闭",
                Some(FieldKind::Submit) | Some(FieldKind::Cancel) => {
                    "↑↓ 选择  Enter 触发  Esc 关闭"
                }
                _ => "↑↓ 选择  Enter 编辑  Esc 关闭",
            },
        }
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
    let name = {
        let t = f.fields[3].value.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    };
    let interval = parse_opt_u64(&f.fields[4].value, "间隔")?;
    let mut cfg = app.monitor.shared.cfg.lock().unwrap();
    let cref: Vec<&str> = cinemas.iter().map(String::as_str).collect();
    let id = config::add_watch(
        &mut cfg,
        movie_id,
        &cref,
        dates.as_deref(),
        name.as_deref(),
        interval,
    )
    .map_err(|e| e.to_string())?;
    Ok(format!("已添加 watch {}", id))
}

fn submit_edit_watch(app: &App, wid: &str, f: &FormModal) -> Result<String, String> {
    let cinemas = split_ids(&f.fields[0].value);
    let dates = parse_dates(&f.fields[1].value)?;
    let interval = parse_opt_u64(&f.fields[2].value, "间隔")?;
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
    match interval {
        Some(secs) => w["interval"] = serde_json::json!(secs),
        None => {
            if let Some(o) = w.as_object_mut() {
                o.remove("interval");
            }
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

// ------------------------- 解析工具 -------------------------

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
