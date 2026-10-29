//! iGlance `iglance.service.v1` 协议支持。
//!
//! ticket_tracker 是常驻 headless 服务：没有窗口，进程本身只占少量内存，
//! 真正的工作是 HTTP 抓取和面板。这里提供：
//!
//! - 一行 manifest（启动脚本输出，iGlance 解析后自动绑定）
//! - 状态 / 内存快照 / 控制命令的元数据
//! - 进程分组内存统计（`ps` RSS 快照）
//! - 停止时的清理（PID 文件、launchd job）
//!
//! 协议只依赖本机回环 HTTP，不引入新的监听端口。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;

use axum::http::HeaderMap;
use chrono::TimeZone;
use serde::Serialize;
use serde_json::{json, Value};

use crate::db::{Db, Settings};

pub const MANIFEST_PREFIX: &str = "IGLANCE_SERVICE_MANIFEST ";
pub const MANIFEST_SCHEMA: &str = "iglance.service.v1";
pub const SERVICE_ID: &str = "ticket_tracker";
pub const SERVICE_NAME: &str = "Ticket Tracker";
pub const TOKEN_HEADER: &str = "X-Gofer-Token";
pub const DEFAULT_LAUNCHD_LABEL: &str = "com.kuicao.ticket-tracker";

const MEMORY_SOURCE: &str = "ps-rss";

/// 当前二进制版本（Cargo.toml）。
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 把监听地址换算成客户端可访问的 base URL。
///
/// `0.0.0.0` / `::` 对客户端没有意义，统一显示为 `127.0.0.1`。
pub fn base_url_from_bind(bind: &str) -> String {
    let bind = bind.trim();
    let (host, port) = match bind.rsplit_once(':') {
        Some((h, p)) => (h, p),
        None => ("127.0.0.1", "8787"),
    };
    let host = host.trim_matches(['[', ']']);
    let display_host = match host {
        "" | "0.0.0.0" | "::" => "127.0.0.1",
        other => other,
    };
    format!("http://{}:{}", display_host, port)
}

/// iGlance 启动命令读取的一行握手。**不包含 token。**
pub fn manifest_line(base_url: &str) -> String {
    let manifest = json!({
        "schema": MANIFEST_SCHEMA,
        "service": {
            "id": SERVICE_ID,
            "name": SERVICE_NAME,
            "version": version(),
        },
        "base_url": base_url.trim_end_matches('/'),
        "auth": {
            "type": "header",
            "header": TOKEN_HEADER,
        },
        "endpoints": {
            "health": "/health",
            "status": "/api/v1/service",
            "memory": "/api/v1/service/memory",
            "pause": "/api/v1/service/pause",
            "resume": "/api/v1/service/resume",
            "toggle": "/api/v1/service/toggle",
            "stop": "/api/v1/service/stop",
        },
    });
    format!("{MANIFEST_PREFIX}{manifest}")
}

/// `GET /api/v1/service` 里的 `controls`。
pub fn controls(base_url: &str) -> Value {
    json!({
        "protocol": {
            "manifest_prefix": MANIFEST_PREFIX.trim_end(),
            "schema": MANIFEST_SCHEMA,
        },
        "status": {"method": "GET", "path": "/api/v1/service"},
        "memory": {"method": "GET", "path": "/api/v1/service/memory"},
        "pause": {"method": "POST", "path": "/api/v1/service/pause"},
        "resume": {"method": "POST", "path": "/api/v1/service/resume"},
        "toggle": {"method": "POST", "path": "/api/v1/service/toggle"},
        "stop": {
            "method": "POST",
            "path": "/api/v1/service/stop",
            "query": {
                "cleanup": "bool, default true: 终止 tt-server 的子进程",
                "close_browser": "bool, default cleanup value: ticket_tracker 不直接驱动浏览器，保留兼容字段"
            }
        },
        "start": start_spec(base_url),
    })
}

/// 离线时的启动方式。优先级：`TT_START_COMMAND` → launchd → 启动脚本。
fn start_spec(base_url: &str) -> Value {
    let manifest_command = manifest_command(base_url);
    let shell = "/bin/zsh";
    let manifest_prefix = MANIFEST_PREFIX.trim_end();

    if let Some(command) = env_nonempty("TT_START_COMMAND") {
        let binding_command = match &manifest_command {
            Some(manifest) => format!("{command} && {manifest}"),
            None => command.clone(),
        };
        return json!({
            "type": "command",
            "shell": shell,
            "command": command,
            "binding_command": binding_command,
            "manifest_command": manifest_command,
            "manifest_prefix": manifest_prefix,
            "source": "env",
            "hint": "binding_command 会先执行 TT_START_COMMAND，再输出 manifest。",
        });
    }

    if supervised_launchd() {
        let label = launchd_label();
        let plist = launchd_plist(&label);
        let bootstrap = if plist.is_file() {
            format!(
                "launchctl bootstrap gui/$(id -u) {}",
                shell_quote(&plist.to_string_lossy())
            )
        } else {
            format!(
                "launchctl kickstart -k gui/$(id -u)/{}",
                shell_quote(&label)
            )
        };
        let command = match &manifest_command {
            Some(manifest) => format!("{bootstrap} && {manifest}"),
            None => bootstrap,
        };
        return json!({
            "type": "command",
            "shell": shell,
            "command": command,
            "binding_command": command,
            "manifest_command": manifest_command,
            "manifest_prefix": manifest_prefix,
            "source": "launchd",
            "launchd_label": label,
            "plist": plist.to_string_lossy(),
            "hint": "launchd 模式；停止会先 bootout，避免 KeepAlive 立即拉起。",
        });
    }

    if let Some(script) = find_start_script() {
        let command = format!(
            "{} start --manifest",
            shell_quote(&script.to_string_lossy())
        );
        return json!({
            "type": "command",
            "shell": shell,
            "command": command,
            "binding_command": command,
            "manifest_command": manifest_command,
            "manifest_prefix": manifest_prefix,
            "cwd": script.parent().map(|p| p.to_string_lossy().to_string()),
            "source": "start-script",
            "hint": "启动脚本会在健康检查通过后输出 IGLANCE_SERVICE_MANIFEST 行。",
        });
    }

    json!({
        "type": "command",
        "shell": shell,
        "command": Value::Null,
        "binding_command": Value::Null,
        "manifest_command": manifest_command,
        "manifest_prefix": manifest_prefix,
        "source": "unknown",
        "hint": "未找到启动脚本；请设置 TT_START_COMMAND，例如 'cd ~/Applications/ticket_tracker && ./start-ticket-tracker.sh start --manifest'。",
    })
}

fn manifest_command(base_url: &str) -> Option<String> {
    std::env::current_exe().ok().map(|exe| {
        format!(
            "{} manifest --base-url {}",
            shell_quote(&exe.to_string_lossy()),
            shell_quote(base_url.trim_end_matches('/'))
        )
    })
}

pub fn supervised_launchd() -> bool {
    env_nonempty("TT_SUPERVISED")
        .map(|v| v.eq_ignore_ascii_case("launchd"))
        .unwrap_or(false)
}

fn launchd_label() -> String {
    env_nonempty("TT_LAUNCHD_LABEL").unwrap_or_else(|| DEFAULT_LAUNCHD_LABEL.to_string())
}

fn launchd_plist(label: &str) -> PathBuf {
    if let Some(path) = env_nonempty("TT_LAUNCHD_PLIST") {
        return PathBuf::from(path);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist"))
}

fn find_start_script() -> Option<PathBuf> {
    if let Some(path) = env_nonempty("TT_START_SCRIPT") {
        return Some(PathBuf::from(path));
    }

    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for ancestor in dir.ancestors().take(6) {
                candidates.push(ancestor.join("start-ticket-tracker.sh"));
                candidates.push(ancestor.join("start-server.sh"));
                candidates.push(ancestor.join("rs/start-server.sh"));
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("start-ticket-tracker.sh"));
        candidates.push(cwd.join("start-server.sh"));
        candidates.push(cwd.join("rs/start-server.sh"));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join("Applications/ticket_tracker/start-ticket-tracker.sh"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

// ------------------------- 鉴权 -------------------------

/// 服务 token：优先 `TT_SERVICE_TOKEN`，否则复用面板设置里的 `panel_token`。
///
/// 两者都没有时返回 `None`，协议请求不做鉴权。
pub async fn expected_token(db: &Db) -> anyhow::Result<Option<String>> {
    if let Some(token) = env_nonempty("TT_SERVICE_TOKEN") {
        return Ok(Some(token));
    }
    let settings = Settings::load(db).await?;
    Ok(settings
        .panel_token
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty()))
}

/// 读取 iGlance / Gofer 兼容的 token header。
pub fn token_from_headers(headers: &HeaderMap) -> Option<String> {
    for name in ["x-gofer-token", "x-agent-token"] {
        if let Some(value) = headers.get(name).and_then(|v| v.to_str().ok()) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    let auth = headers.get("authorization")?.to_str().ok()?.trim();
    let (scheme, value) = auth.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") && !value.trim().is_empty() {
        return Some(value.trim().to_string());
    }
    None
}

pub fn token_matches(expected: &str, provided: Option<&str>) -> bool {
    let Some(provided) = provided else {
        return false;
    };
    // 常量时间比较，避免明显的时序差异。
    let a = expected.as_bytes();
    let b = provided.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ------------------------- 时间 -------------------------

pub fn now_iso() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false)
}

pub fn started_at_iso(started_at: f64) -> String {
    let secs = started_at.max(0.0) as i64;
    chrono::Local
        .timestamp_opt(secs, 0)
        .single()
        .unwrap_or_else(chrono::Local::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

// ------------------------- 内存 -------------------------

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ProcessGroup {
    /// RSS 之和，单位 bytes。
    pub rss_bytes: u64,
    pub processes: usize,
    pub pids: Vec<u32>,
}

impl ProcessGroup {
    fn add(&mut self, proc: &ProcInfo) {
        self.rss_bytes = self.rss_bytes.saturating_add(proc.rss_bytes);
        self.processes += 1;
        self.pids.push(proc.pid);
    }

    fn sort(&mut self) {
        self.pids.sort_unstable();
        self.pids.dedup();
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryGroup {
    pub id: String,
    pub label: String,
    #[serde(flatten)]
    pub usage: ProcessGroup,
}

impl MemoryGroup {
    fn new(id: &str, label: &str, usage: &ProcessGroup) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            usage: usage.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MemorySnapshot {
    pub available: bool,
    pub source: String,
    pub observed_at: String,
    /// 各分组之和（估算）。
    pub total_rss_bytes: u64,
    /// 各分组进程数之和（分组互不重叠）。
    pub total_processes: usize,
    /// iGlance 通用渲染入口。
    pub groups: Vec<MemoryGroup>,
    /// ticket_tracker 兼容命名分组。
    pub service: ProcessGroup,
    pub workers: ProcessGroup,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl MemorySnapshot {
    pub fn unavailable(error: impl Into<String>) -> Self {
        let mut snapshot = Self {
            available: false,
            source: MEMORY_SOURCE.to_string(),
            observed_at: now_iso(),
            error: Some(error.into()),
            ..Self::default()
        };
        snapshot.fill_groups();
        snapshot
    }

    fn from_processes(procs: &[ProcInfo], root: u32) -> Self {
        let descendants = descendants_of(root, procs);
        let mut snapshot = Self {
            available: true,
            source: MEMORY_SOURCE.to_string(),
            observed_at: now_iso(),
            ..Self::default()
        };

        for proc in procs {
            match classify(proc, root, &descendants) {
                Some(Group::Service) => snapshot.service.add(proc),
                Some(Group::Workers) => snapshot.workers.add(proc),
                None => {}
            }
        }

        for group in [&mut snapshot.service, &mut snapshot.workers] {
            group.sort();
        }
        snapshot.fill_groups();
        snapshot
    }

    fn fill_groups(&mut self) {
        self.total_rss_bytes = self
            .service
            .rss_bytes
            .saturating_add(self.workers.rss_bytes);
        self.total_processes = self.service.processes + self.workers.processes;
        self.groups = vec![
            MemoryGroup::new("service", SERVICE_NAME, &self.service),
            MemoryGroup::new("workers", "Workers", &self.workers),
        ];
    }
}

/// 采集一次内存快照。失败写入 `available=false`，不影响 HTTP 接口。
pub fn collect_memory() -> MemorySnapshot {
    match read_processes() {
        Ok(procs) => MemorySnapshot::from_processes(&procs, std::process::id()),
        Err(e) => MemorySnapshot::unavailable(format!("ps 采集失败: {e}")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    Service,
    Workers,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcInfo {
    pid: u32,
    ppid: u32,
    rss_bytes: u64,
    command: String,
}

fn read_processes() -> std::io::Result<Vec<ProcInfo>> {
    let output = std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,rss=,command="])
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "ps exited with {}",
            output.status
        )));
    }
    Ok(parse_ps(&String::from_utf8_lossy(&output.stdout)))
}

fn parse_ps(text: &str) -> Vec<ProcInfo> {
    text.lines().filter_map(parse_ps_line).collect()
}

fn parse_ps_line(line: &str) -> Option<ProcInfo> {
    let mut fields = line.split_whitespace();
    let pid = fields.next()?.parse().ok()?;
    let ppid = fields.next()?.parse().ok()?;
    let rss_kb: u64 = fields.next()?.parse().ok()?;
    let command = fields.collect::<Vec<_>>().join(" ");
    if command.is_empty() {
        return None;
    }
    Some(ProcInfo {
        pid,
        ppid,
        rss_bytes: rss_kb.saturating_mul(1024),
        command,
    })
}

fn descendants_of(root: u32, procs: &[ProcInfo]) -> HashSet<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for proc in procs {
        children.entry(proc.ppid).or_default().push(proc.pid);
    }
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([root]);
    while let Some(pid) = queue.pop_front() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(kids) = children.get(&pid) {
            queue.extend(kids.iter().copied());
        }
    }
    seen
}

fn classify(proc: &ProcInfo, root: u32, descendants: &HashSet<u32>) -> Option<Group> {
    if proc.pid == root {
        return Some(Group::Service);
    }
    // 采集用的 ps 本身是 tt-server 的子进程，不能计入结果。
    if is_measurement_process(&proc.command) {
        return None;
    }
    // ticket_tracker 不派生浏览器、也不直接驱动微信；只统计自己的子进程。
    descendants.contains(&proc.pid).then_some(Group::Workers)
}

fn is_measurement_process(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    executable_basename(command) == "ps" && lower.contains("-axo") && lower.contains("command=")
}

fn executable_basename(command: &str) -> String {
    command
        .split_whitespace()
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

// ------------------------- 停止 / 清理 -------------------------

/// 停止前清理。`cleanup=true` 时终止 tt-server 的直属子进程（通常没有）。
pub async fn stop_cleanup(cleanup: bool) {
    if cleanup {
        terminate_descendants().await;
    }
}

async fn terminate_descendants() {
    let root = std::process::id();
    let Ok(procs) = read_processes() else {
        return;
    };
    let pids: Vec<u32> = descendants_of(root, &procs)
        .into_iter()
        .filter(|pid| *pid != root)
        .filter(|pid| {
            procs
                .iter()
                .find(|p| p.pid == *pid)
                .map(|p| !is_measurement_process(&p.command))
                .unwrap_or(false)
        })
        .collect();
    if pids.is_empty() {
        return;
    }
    signal(&pids, "-TERM").await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    signal(&pids, "-KILL").await;
}

async fn signal(pids: &[u32], signal: &str) {
    let mut cmd = tokio::process::Command::new("/bin/kill");
    cmd.arg(signal);
    for pid in pids {
        cmd.arg(pid.to_string());
    }
    let _ = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

/// 删除启动脚本写入的 PID 文件（只删指向当前进程的那个）。
pub fn remove_pid_file() {
    let mut paths = Vec::new();
    if let Some(path) = env_nonempty("TT_PID_FILE") {
        paths.push(PathBuf::from(path));
    }
    paths.push(crate::paths::state_dir().join("tt-server.pid"));
    paths.push(crate::paths::pid_file());
    for path in paths {
        remove_pid_at(&path);
    }
}

fn remove_pid_at(path: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    if text.trim().parse::<u32>().ok() == Some(std::process::id()) {
        let _ = std::fs::remove_file(path);
    }
}

/// launchd 管理时先移除 job，避免 KeepAlive 立即把刚停止的服务拉起来。
pub async fn bootout_launchd() {
    if !supervised_launchd() {
        return;
    }
    let label = launchd_label();
    let target = format!("gui/{}/{}", current_uid(), label);
    let _ = tokio::process::Command::new("launchctl")
        .args(["bootout", &target])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

fn current_uid() -> String {
    std::process::Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "501".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_has_prefix_and_no_token() {
        let line = manifest_line("http://127.0.0.1:8787/");
        assert!(line.starts_with(MANIFEST_PREFIX));
        let value: Value = serde_json::from_str(line.trim_start_matches(MANIFEST_PREFIX)).unwrap();
        assert_eq!(value["schema"], MANIFEST_SCHEMA);
        assert_eq!(value["base_url"], "http://127.0.0.1:8787");
        assert_eq!(value["service"]["id"], SERVICE_ID);
        assert!(value.get("token").is_none());
        assert!(!line.contains("token\":\""));
    }

    #[test]
    fn base_url_normalizes_wildcard_hosts() {
        assert_eq!(
            base_url_from_bind("127.0.0.1:8787"),
            "http://127.0.0.1:8787"
        );
        assert_eq!(base_url_from_bind("0.0.0.0:9000"), "http://127.0.0.1:9000");
        assert_eq!(base_url_from_bind("[::]:9000"), "http://127.0.0.1:9000");
    }

    #[test]
    fn parse_ps_line_reads_rss_bytes() {
        let p = parse_ps_line("  123   1  2048 /usr/bin/foo --bar").unwrap();
        assert_eq!(p.pid, 123);
        assert_eq!(p.ppid, 1);
        assert_eq!(p.rss_bytes, 2 * 1024 * 1024);
        assert_eq!(p.command, "/usr/bin/foo --bar");
    }

    #[test]
    fn token_matching_is_exact() {
        assert!(token_matches("abc", Some("abc")));
        assert!(!token_matches("abc", Some("abd")));
        assert!(!token_matches("abc", Some("abcd")));
        assert!(!token_matches("abc", None));
    }

    #[test]
    fn memory_groups_only_cover_service_and_workers() {
        let snapshot = MemorySnapshot::unavailable("test");
        assert!(!snapshot.available);
        let ids: Vec<&str> = snapshot.groups.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["service", "workers"]);
    }

    #[test]
    fn unrelated_processes_are_not_counted() {
        let procs = vec![
            ProcInfo {
                pid: 100,
                ppid: 1,
                rss_bytes: 1024,
                command: "tt-server".into(),
            },
            ProcInfo {
                pid: 101,
                ppid: 100,
                rss_bytes: 2048,
                command: "worker".into(),
            },
            ProcInfo {
                pid: 102,
                ppid: 1,
                rss_bytes: 4096,
                command: "/Applications/WeChat.app/Contents/MacOS/WeChat".into(),
            },
            ProcInfo {
                pid: 103,
                ppid: 1,
                rss_bytes: 8192,
                command: "/usr/bin/ps -axo pid=,ppid=,rss=,command=".into(),
            },
        ];
        let snapshot = MemorySnapshot::from_processes(&procs, 100);
        assert_eq!(snapshot.service.rss_bytes, 1024);
        assert_eq!(snapshot.workers.rss_bytes, 2048);
        assert_eq!(snapshot.total_rss_bytes, 3072);
        assert_eq!(snapshot.total_processes, 2);
    }
}
