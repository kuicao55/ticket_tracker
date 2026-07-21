# ticket-tracker Rust 重构指导文档

> 本文档是 2026-07-21 在一次规划会话中敲定的方案，用于在一个 **clean session** 中由 Claude 重建 Rust 版本（保持 Python 版本可继续维护）。
>
> 如果是新会话读这份文档，请通读"决策日志"和"TUI 设计"两节——它们承载了所有不再重新讨论的取舍。

---

## 0. TL;DR

- 项目根：`/Users/kuicao/Applications/ticket_tracker/`
- 把现有 Python/Textual 项目从根目录迁到 `py/`，在 `rs/` 用 Rust 重写 TUI + CLI
- **配置文件完全共用** `~/.config/ticket-tracker/config.json`，Rust 端必须 100% 复刻 v2 schema 与迁移逻辑
- TUI 用 `ratatui` + `crossterm`，**三栏 dashboard + 状态栏**，vim 风键位，**无鼠标**
- Python 版本 = 仅修 bug，不加功能；Rust 版本承接所有新开发
- 颜色限定 16 色 ANSI，无 24-bit RGB，无鼠标，无 tab 栏，无弹窗 modal

---

## 1. 项目背景

`ticket-tracker` 是一个猫眼影城影片预售监测 CLI + TUI：

- 用户添加「watch = 电影 × 影院列表 × 可选日期集合」，脚本每 N 秒抓猫眼 JSON 接口
- 一旦目标电影在某个影院开售，触发：
  - Discord Webhook 推送到手机
  - macOS：弹窗 + 周期性铃声 + 语音 + 自动打开购票页（1 分钟自动停）
- 时段策略：`quiet_window`（完全静默）、`phone_only_window`（只推手机）、其余时段正常报警
- Discord 每小时发运行报告

当前实现：Python 3.8+，依赖 `click` + `rich` + `textual`。
当前版本：`v1.2.0`（git: `cf73902 docs: README 同步 v1.2.0 实际行为 + 修占位 URL`）。

### 为什么重写

用户对 Textual TUI 的视觉观感不满意（"过时、不够极客"），希望用 Rust + ratatui 重写 TUI，但**保留功能完整性 + Python 版本可继续用**。

---

## 2. 决策日志（不要再讨论）

### 2.1 仓库结构：单仓 monorepo（py/ + rs/）

| 候选 | 否决理由 |
|---|---|
| 共享底层代码 + 两套 UI | Rust↔Python 无法共享代码，强行做就是写两遍业务逻辑 |
| worktree / branch | Rust 版本隐藏在分支里，clone 后看不到；发布别扭 |
| **独立仓库** | 同一逻辑项目拆两仓，issue/讨论/release 双份维护，笨重 |
| **monorepo 双 workspace（采用）** | 单一 README 顶部指引，两套独立 CI，Rust 成熟后 `py/` 可改名为 `legacy/` 一键归档 |

### 2.2 Python 版本未来：仅修 bug，不加功能

- 文档顶部横幅标注 "Maintenance only — 新版本请用 Rust"
- v1.2.0 即功能终版；CI 仍跑测试保证能装能跑
- 不发新功能 PR

### 2.3 配置文件：完全共用 `~/.config/ticket-tracker/config.json`

- 走 XDG 规范：`XDG_CONFIG_HOME` 或 `~/.config` 兜底（macOS 兼容）
- Rust 端用 `dirs` crate
- **必须 100% 复刻 v2 schema + 迁移逻辑**（见第 4 节）
- 用户切换版本无感

### 2.4 TUI 后端：crossterm

- 跨 Win/Mac/Linux 一致
- ratatui 官方推荐
- **不**用 termion（Win 不可用）

### 2.5 UI 设计取舍

| 取舍 | 决定 | 理由 |
|---|---|---|
| 鼠标 | **关闭** | Textual 鼠标交互强行做体验差；极客风格核心是键盘 |
| Tab 栏 | **不要** | 所有信息常驻；Tab/h/l 切换焦点 pane 已够 |
| 右栏内容 | **常驻 events** | 开售消息一眼看到，符合 dashboard 哲学 |
| 颜色 | **限 16 色 ANSI** | 24-bit RGB 在老终端/ssh 会丑；极客风 = 任何终端都漂亮 |
| 边框 | `BorderType::Rounded` | 比 Plain 高级，比 Double 含蓄 |
| Pane 焦点指示 | 边框颜色：Cyan+Bold vs DarkGray | 一眼看出当前操作区 |
| 选中行 | `bg(Cyan).fg(Black).bold()` 反色 | 所有终端都能正确显示 |
| Modal 弹窗 | **不用**；用覆盖层 + `Clear` 替代 | 极简 |
| 动画/闪烁 | **不要** | 极客 = 静止 + 高密度信息 |

---

## 3. 仓库结构落地

### 3.1 目标目录

```
ticket_tracker/
├── README.md               ← 顶部"用哪个版本"卡片；默认指向 rs/
├── LICENSE
├── .github/
│   └── workflows/
│       ├── py.yml          # pip build + pytest（mac/linux）
│       └── rs.yml          # cargo test + cross build (mac/linux/win)
├── docs/
│   └── RUST_PORT.md        ← 你正在读的这份
├── py/                     ← 整个旧 Python 项目（git mv）
│   ├── pyproject.toml
│   ├── README.md           ← "Maintenance only" 横幅
│   ├── src/ticket_tracker/
│   │   ├── __init__.py     (v1.2.0)
│   │   ├── __main__.py
│   │   ├── cli.py          (26KB)
│   │   ├── config.py       (7KB)  ← schema 来源
│   │   ├── maoyan.py       (5KB)
│   │   ├── monitor.py      (17KB)
│   │   ├── notify.py       (5KB)
│   │   ├── paths.py        (1KB)
│   │   ├── presets.py      (1KB)
│   │   └── tui.py          (47KB)  ← 仅参考业务交互，不参考视觉
│   └── examples/
│       └── config.example.json
└── rs/                     ← 新 Rust 项目
    ├── Cargo.toml
    ├── Cargo.lock
    ├── README.md
    └── src/
        ├── main.rs         # clap CLI 入口
        ├── lib.rs
        ├── config.rs       # v2 schema + 迁移
        ├── paths.rs        # XDG dirs
        ├── maoyan.rs       # reqwest + serde
        ├── presets.rs      # static const
        ├── notify.rs       # Discord + macOS
        ├── monitor.rs      # tokio 调度
        ├── cli/
        │   ├── mod.rs
        │   ├── start.rs
        │   ├── stop.rs
        │   ├── watch.rs
        │   ├── cinema.rs
        │   ├── films.rs
        │   ├── config_cmd.rs
        │   ├── test_cmd.rs
        │   └── doctor.rs
        └── tui/
            ├── mod.rs      # App struct + event loop + state
            ├── ui.rs       # 主 render(&mut Frame)
            ├── panes.rs    # 左/中/右 pane 渲染
            ├── focus.rs    # 焦点状态机
            ├── input.rs    # 键盘事件分发
            └── cmd.rs      # `:` 命令面板解析与执行
```

### 3.2 迁移步骤

1. `mkdir py && git mv src py/ && git mv pyproject.toml py/ && git mv examples py/`
2. 更新 `py/pyproject.toml` 的 `packages = ["src/ticket_tracker"]` 路径（其实不用变，因为 hatchling 相对 pyproject.toml 解析）
3. 创建 `rs/`：`cd rs && cargo init --lib --vcs none` 然后改 `Cargo.toml`
4. 在 `rs/` 旁边 `cargo init --bin --vcs none` 不必要——直接用 `src/main.rs` + `src/lib.rs` 二合一结构

---

## 4. 配置 Schema（v2，**必须 100% 兼容**）

### 4.1 完整 schema（来自 `py/src/ticket_tracker/config.py`）

```jsonc
{
  "version": 2,
  "discord_webhook": "https://discord.com/api/webhooks/...",   // null = 未配置
  "quiet_window": "01:00-06:00",                              // "HH:MM-HH:MM"
  "phone_only_window": "06:00-09:00",
  "check_interval": 90,                                        // 秒
  "alert_duration_sec": 60,                                    // macOS 报警时长上限
  "heartbeat_interval_sec": 3600,                              // Discord 报告间隔
  "cinemas": [
    { "id": "37534", "name": "MOViE MOViE 影城...", "builtin": false }
  ],
  "watches": [
    {
      "id": "w_a1b2c3",
      "movie_id": 1490607,
      "movie_name": "蜘蛛侠：崭新之日",
      "cinemas": ["37534", "2127"],
      "dates": ["2026-07-29", "2026-07-30"],   // null = 不限
      "interval": null,                        // null = 用全局 check_interval
      "enabled": true,
      "presale_fired": false,
      "created_at": "2026-07-21T17:00:00",
      "last_alert_at": null,
      "fired_cinemas": [],
      "_last_status": "open",                  // 运行期字段，monitor 写
      "_last_payload": { ... }                 // 运行期字段
    }
  ],
  "_runtime": {                                 // 运行期字段
    "started_at": 1721566800
  },
  "_migrated_legacy_state": true,               // 旧 monitor_spiderman.py 迁移标记
  "_watch_schema_migrated": true                // v1→v2 watch.cinema_id→cinemas[] 标记
}
```

### 4.2 迁移逻辑（Rust 必须实现）

参考 `py/src/ticket_tracker/config.py:91-135`：

1. **v1 → v2 watch schema 迁移**：把 `watch.cinema_id` 字符串变成 `watch.cinemas: [str]`，并对每个 cinema id 自动注册（用 `add_cinema` 逻辑）
2. **旧 `state.json` 迁移**：如果 repo 根 `state.json` 存在（来自极旧的 `monitor_spiderman.py`），把它里面的 `presale_open: true` 映射到对应 watch 的 `presale_fired: true`，完成后 `.bak` 备份
3. **字段补全**：缺 `dates`/`cinemas`/`_runtime` 时补默认值
4. **写入原子化**：写到 `config.json.tmp` 再 `rename`，避免崩溃中途坏文件

### 4.3 时段解析（`current_mode`）

```rust
// config.py:42-50
// 当前小时 h：
//   quiet_window.start <= h < quiet_window.end       → "quiet"
//   phone_only_window.start <= h < phone_only_window.end → "phone_only"
//   其他                                            → "normal"
// 注意：跨午夜时段不处理（永远 start < end）
```

### 4.4 XDG 路径（来自 `py/src/ticket_tracker/paths.py`）

```rust
config_dir()  = $XDG_CONFIG_HOME/ticket-tracker/  或 ~/.config/ticket-tracker/
state_dir()   = $XDG_STATE_HOME/ticket-tracker/   或 ~/.local/state/ticket-tracker/
config.json   = config_dir() / "config.json"
log file      = state_dir() / "ticket-tracker.log"
pid file      = state_dir() / "ticket-tracker.pid"
```

Rust 用 `dirs` crate 实现：`dirs::config_dir()` / `dirs::state_dir()`，叠加 `ticket-tracker` 子目录。

---

## 5. 后端业务逻辑（Python → Rust 映射）

### 5.1 模块映射

| Python 文件 | 关键函数 | Rust 端 | 关键 crate |
|---|---|---|---|
| `maoyan.py` | `fetch_cinema`, `fetch_films_list`, `fetch_movie_name`, `find_movie`, `movie_dates` | `rs/src/maoyan.rs` | `reqwest`, `serde_json` |
| `config.py` | `load_or_init`, `save`, `_migrate_*`, `add_watch`/`remove_watch`/`mark_presale_fired`/`set_watch_field` | `rs/src/config.rs` | `serde`, `serde_json`, `dirs` |
| `notify.py` | `notify_discord`, `notify_macos`, `caffeinate_start`/`stop`, `is_caffeinated` | `rs/src/notify.rs` | `reqwest` + `std::process::Command` |
| `monitor.py` | `Monitor.run`, `_tick`, `_send_heartbeat`, `check_watch` | `rs/src/monitor.rs` | `tokio`, `std::sync::Arc<Mutex<>>` |
| `paths.py` | XDG 路径 | `rs/src/paths.rs` | `dirs` |
| `presets.py` | `PRESETS` 常量 | `rs/src/presets.rs` | （无，const 字符串） |

### 5.2 猫眼 API（来自 `py/src/ticket_tracker/maoyan.py`）

```
GET https://m.maoyan.com/ajax/cinemaDetail?cinemaId=<id>
   Header: User-Agent (iPhone 模拟), Referer: https://m.maoyan.com/shows/<id>
   返回 JSON: { showData: { cinemaName, movies: [...] } }

GET https://m.maoyan.com/ajax/detailmovie?movieId=<id>
   返回 { detailMovie: { nm: <name> } } 或类似

GET https://www.maoyan.com/films?showType=<1|2|3>     # 1=热映 2=即将 3=经典
   返回 HTML，需要正则提取 /films/<id> 链接
```

- 重试 3 次，每次间隔 3 秒
- SSL 关闭校验（`c.check_hostname=False; verify_mode=CERT_NONE`）—— Rust 端 `reqwest` 同样配置
- PC 端 films 页需要保留 cookie + 跟 302 重定向；移动端 API 不需要

### 5.3 监测逻辑（来自 `py/src/ticket_tracker/monitor.py`）

**单条 watch 检查返回的 status**：
- `open` — 至少有一个 cinema 在限定日期内有场次
- `not_listed` — 所有 cinema 的电影列表里都没有该 movie
- `no_shows` — 列表有但限定日期内没场次
- `error` — HTTP 错误或无 cinema 配置

**tick 节流**：每条 watch 有独立 `interval`，未到时间就跳过；`force=True` 时全跑
**主循环**：
```
while not stop:
    if mode == "quiet":
        wait(60); continue
    if force_check_event: tick(); continue
    tick()
    if elapsed >= heartbeat_interval: send_heartbeat()
    wait(min(interval_per_watch))
```

**自动停用**：一条 watch 的**所有 cinema 都触发过开售报警后**，`enabled=false`，不再抓取

**触发开售时的副作用**：
1. Discord 推送（始终）
2. macOS 报警（仅 `mode == "normal"`）
3. `mark_presale_fired` 写回 config

### 5.4 时段策略判断

```
quiet_window.start <= now.hour < quiet_window.end  → quiet
phone_only_window.start <= now.hour < ...            → phone_only
其他                                                → normal
```

### 5.5 Discord 通知 payload

```rust
notify_discord(webhook, title, message, url=None) ->
  if !webhook.starts_with("https://discord.com/api/webhooks/"): return false
  POST application/json
  body = { "content": "**{title}**\n{message}\n👉 {url}".filter(|x| x.is_some()) }
  重试 3 次，间隔 3 秒
```

### 5.6 macOS 通知子进程

```rust
// 弹窗
osascript -e 'display notification "..." with title "..." sound name "Glass"'
// 周期性响铃（duration 秒内）
afplay /System/Library/Sounds/Glass.aiff   // 每 3 秒一次
say "蜘蛛侠预售已开启，快去抢票"            // 仅一次
// 自动打开购票页（仅一次）
open https://www.maoyan.com/cinema/<id>
// 防休眠
caffeinate -i -s -w <pid>
```

非 macOS 平台：Discord 仍正常，macOS 特有路径全部跳过。

---

## 6. CLI 子命令（必须 1:1 对齐）

参考 `py/src/ticket_tracker/cli.py`，所有子命令在 Rust 用 `clap` 重新实现：

```
tt --version                                       版本
tt help [command [subcommand ...]]                  帮助（嵌套）
tt init                                            首次配置
tt start [--detach] [--interval N] [--watch ID]    启动（前台 TUI / 后台）
tt stop                                            停止后台
tt restart                                          重启
tt status                                          一行状态
tt log [-n N] [-f]                                  日志

tt watch list
tt watch add MOVIE_ID [-c CID]... [-d YYYY-MM-DD]... [--name ...] [--interval N]
tt watch show WATCH_ID
tt watch edit WATCH_ID [-c CID]... [-d DATE]... [--interval N]
tt watch remove WATCH_ID
tt watch enable / disable WATCH_ID

tt cinema list
tt cinema add CINEMA_ID [--name ...]
tt cinema remove CINEMA_ID
tt cinema presets
tt cinema add-preset NAME

tt films [SHOW_TYPE]                                # 1=热映 2=即将 3=经典

tt config show | get KEY | set KEY VALUE | unset KEY | path

tt test [all|discord|macos]
tt doctor
```

别名（`config get/set`）：`discord-webhook` ↔ `discord_webhook`，`webhook` ↔ `discord_webhook`，`quiet` ↔ `quiet_window`，`phone-only` ↔ `phone_only_window`，`interval` ↔ `check_interval`。

---

## 7. TUI 设计（核心）

### 7.1 整体布局

```
        1         2         3         4         5         6
1234567890123456789012345678901234567890123456789012345678901234567
┌─ ticket-tracker ────── 17:52 ─ 12h34m ── normal ── 3 active ─┐  ← 顶 (Length 1)
│ ┌─ watches (3) ─┐ ┌─ detail · w_a1b2c3 ────┐ ┌─ events (last 64) ─┐ │
│ │>w_a1b2c3 ✓   │ │ 蜘蛛侠：崭新之日 (1490607)│ │ [17:51] ✓ 预售开启! │ │
│ │ w_d4e5f6 ×   │ │ ─────────────────────  │ │ [17:45] · 沙丘 3 未  │ │
│ │ w_xxxxxx ×   │ │ cinemas : MOViE…(37534) │ │ [17:00] · 阿凡达 3  │ │
│ │              │ │ dates   : 07-29→07-30  │ │ [16:30] · 影院列表中│ │
│ │              │ │ interval: 60s  ✓ enabled│ │ ...                  │ │
│ │              │ │ fired   : 1/2          │ │                      │ │
│ │              │ │ ─────────────────────  │ │                      │ │
│ │              │ │ cinema · shows · range │ │                      │ │
│ │              │ │ MOViE  ·  8     · 07-29│ │                      │ │
│ │              │ │ 大光明 ·  4     · 07-30│ │                      │ │
│ └──────────────┘ └────────────────────────┘ └──────────────────────┘ │
├─────────────────────────────────────────────────────────────────────┤
│▮watches ⠂detail ⠂events   a add · d del · r run · / filter · : cmd │  ← 状态栏
└─────────────────────────────────────────────────────────────────────┘
  ↑ : cmd 模式时，底部多出 1 行输入
```

### 7.2 Layout 约束值

```rust
// 顶层 vertical
let chunks = Layout::vertical([
    Length(1),       // header
    Min(0),          // body
    Length(1),       // status bar
]).split(frame.area());

// body 横向三栏
let cols = Layout::horizontal([
    Length(22),      // 左：watches（窄固定）
    Min(40),         // 中：detail（自适应，最小 40）
    Length(38),      // 右：events（窄固定）
]).split(chunks[1]);

// 中栏内部再拆
let mid = Layout::vertical([
    Length(8),       // 详情字段区
    Min(0),          // 影院/场次子表
]).split(cols[1]);
```

### 7.3 Widget 选择（扔掉 Textual 的 grid/button）

| 用途 | widget |
|---|---|
| 监视列表 | `widgets::Table` + `TableState`（stateful） |
| 详情字段 | `widgets::Paragraph` 拼多 `Line`，字段对齐用 `Span::styled` |
| 影院子表 | `widgets::Table` |
| 事件流 | `widgets::List` + `ListState`（自带滚动） |
| 命令输入 | `widgets::Paragraph` 单行 + cursor |
| 状态栏 | `widgets::Paragraph` |
| 边框 | `widgets::Block::bordered().border_type(BorderType::Rounded).title(...)` |
| 模态覆盖 | 先 `frame.render_widget(Clear, area)` 再渲染内容 |

### 7.4 视觉规范

```rust
use ratatui::style::{Color, Modifier, Style};

// 焦点 pane
let focused = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
// 非焦点 pane
let unfocused = Style::default().fg(Color::DarkGray);
// 选中行
let selected = Style::default().bg(Color::Cyan).fg(Color::Black).add_modifier(Modifier::BOLD);
// 状态图标
// ✓ 开售 → Green
// × 停用 → DarkGray
// ⠂ 等待 → Yellow
// ! 出错 → Red
// — 无数据 → DarkGray
```

### 7.5 焦点管理

- 状态机 enum `Focus { Watches, Detail, Events }`
- `Tab` / `Shift+Tab` 循环；`h` / `l` 左右；`j` / `k` 上下移动（取决于焦点 pane）
- 焦点 pane 边框 `Cyan+Bold`，非焦点 `DarkGray`
- 焦点 pane 内的 `j/k/↑↓/g/G/Enter` 作用于该 pane 的 stateful widget

### 7.6 键位（状态栏常驻显示）

| 键 | 作用 |
|---|---|
| `Tab` / `Shift+Tab` | 切换焦点 pane |
| `h` / `l` | 左右切 pane |
| `j` / `k` / `↑` / `↓` | 焦点 pane 内上下移动 |
| `g` / `G` | 跳到首/尾 |
| `/` | 在焦点 pane 内过滤（watches 过滤电影名；events 过滤文本） |
| `Enter` | 执行默认动作（events = 无；watches = 选中+跳 detail 焦点） |
| `a` | 添加 watch（弹 `:` 命令面板，自动填充 `:add`） |
| `d` | 删除（焦点 pane 上的当前项；confirm via y/n） |
| `r` | 立即触发一轮检查（force tick） |
| `e` | 编辑当前 watch（dates / interval / enable toggle） |
| `:` | 命令面板 |
| `?` | 帮助覆盖层 |
| `q` / `Ctrl+C` | 退出 |

### 7.7 `:` 命令面板（取代所有弹窗）

- 底部覆盖一行 `Clear` + `Paragraph`：``: `` + 输入字符 + cursor
- **上下文感知**：根据当前焦点 pane 给候选补全
  - watches 焦点 → `:add <mid> [-c <cid>]... [-d <date>]...`, `:rm <wid>`, `:enable <wid>`, `:disable <wid>`, `:edit dates ...`, `:edit interval ...`
  - events 焦点 → 无 pane 命令
  - 全局（任意焦点都可）：
    - `:run` — 立即检查
    - `:interval <sec>` — 改全局间隔
    - `:webhook <url>` / `:webhook clear`
    - `:quiet <HH:MM-HH:MM>` / `:phone <HH:MM-HH:MM>` / `:report <sec>`
    - `:films [1|2|3]` — 拉取猫眼电影列表，输出到 events pane
    - `:log [N]` / `:log -f` — 查看日志
    - `:doctor` — 自检
    - `:help`
    - `:quit` / `:q`
- `Enter` 执行，`Esc` 取消
- 命令执行结果：成功 → events pane 多一条 `[HH:MM:SS] ✓ cmd: <summary>`；失败 → `✗ cmd: <error>`
- 命令解析：空格分隔；引号包住带空格的参数（如 `:add 1490607 --name "蜘蛛侠：崭新之日"`）

### 7.8 模态覆盖（Help / Confirm）

不用嵌套 modal。用 `Clear` 覆盖 + 居中渲染：

```rust
// Help (?) 全屏覆盖
let popup = centered_rect(80, 80, frame.area());
frame.render_widget(Clear, popup);
// Paragraph 里手写 markdown 风格的帮助文本
frame.render_widget(help_paragraph, popup);

// Confirm (y/n)：状态栏变 yellow，底部多 1 行
// "delete w_a1b2c3? (y/n)"  5 秒不答自动取消
```

### 7.9 Empty state

无任何 watch 时：
- 中栏 `Paragraph` 居中大字提示
- 样式：`Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)`
- 内容：
  ```
  
            🎬 ticket-tracker
       
       你还没有添加任何监视项
       
       按 : 然后 :add <movie_id> -c <cinema_id>
                          或
       按 a 直接进入添加
       
  ```
- 按 `:` 自动跳到 add 命令面板；按 `a` 同效

### 7.10 不做清单

- ❌ 鼠标（`crossterm` 不 enable mouse event）
- ❌ 动画 / 闪烁 / 颜色脉冲
- ❌ 嵌套 modal 弹窗（用 Clear 覆盖层）
- ❌ 自适应 pane resize 热键（v1 不做）
- ❌ Emoji 作为关键状态指示（系统差异大；用 ASCII `✓×⠂!`）
- ❌ 24-bit RGB 颜色
- ❌ 主题切换 / 配色自定义
- ❌ 插件系统

---

## 8. Ratatui 学习资源（必读）

### 8.1 关键链接

- **官方站点**：https://ratatui.rs
- **docs.rs**（API 文档）：https://docs.rs/ratatui/latest/ratatui/
- **教程**：https://ratatui.rs/tutorials/
- **应用模式**（Elm / Component / Flux）：https://ratatui.rs/concepts/application-patterns/
- **examples 目录**（必看）：https://github.com/ratatui/ratatui/tree/main/examples
  - **重点研究**：`examples/apps/demo/`、`examples/apps/popup/`、`examples/apps/user_input/`
- **模板项目**（推荐 fork 起点）：https://github.com/ratatui/templates
- **跨平台后端**：https://ratatui.rs/concepts/backends/

### 8.2 推荐学习的 examples

| Example | 学什么 |
|---|---|
| `examples/apps/demo/` | 多 widget 组合、`Table`、`Tabs`、`Chart` 等所有 widget |
| `examples/apps/popup/` | `Clear` + 居中覆盖层 = 模态 |
| `examples/apps/user_input/` | 单行输入 + 光标定位 |
| `examples/widgets/table/` | `Table` + `TableState` 有状态渲染 |
| `examples/widgets/list/` | `List` + `ListState` 滚动 |
| `examples/widgets/paragraph/` | `Paragraph` 多 `Line` `Span` 拼接 |
| `examples/layout/` | `Constraint::{Length,Percentage,Ratio,Min}` 各种搭配 |

### 8.3 推荐架构模式

参考 https://ratatui.rs/concepts/application-patterns/the-elm-architecture/

- **App struct** 持有所有可变状态（`Vec<Watch>`, `Vec<Event>`, `Focus`, `InputMode`...）
- **update(msg) → App** 处理事件，纯函数风格（除了 tokio 任务回写）
- **view(&App, &mut Frame)** 纯渲染
- **event loop**：`crossterm::event::read()` → 分发到 update → view → terminal.draw()

```rust
// 典型结构（伪代码）
fn run() -> Result<()> {
    let mut terminal = setup_terminal()?;
    let mut app = App::new();
    let (tx, rx) = mpsc::channel::<AppEvent>();
    let monitor_handle = tokio::spawn(run_monitor(tx));
    
    loop {
        terminal.draw(|f| ui::render(&mut app, f))?;
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(k) = event::read()? {
                if handle_key(&mut app, k, &tx).is_quit() { break; }
            }
        }
        while let Ok(ev) = rx.try_recv() { app.apply(ev); }
    }
    cleanup_terminal()?;
    Ok(())
}
```

### 8.4 推荐 crate 列表

```toml
[dependencies]
ratatui = "0.29"
ratatui-crossterm = "0.29"
crossterm = "0.28"
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", features = ["json", "rustls-tls"], default-features = false }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
clap = { version = "4", features = ["derive"] }
dirs = "5"
anyhow = "1"
thiserror = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v4"] }

[dev-dependencies]
pretty_assertions = "1"
```

### 8.5 常见需求代码片段索引

**嵌套 Layout 拆分**（见 §7.2）

**Table stateful 渲染**：
```rust
let mut state = TableState::default();
state.select(Some(0));
let table = Table::new(rows, widths)
    .block(Block::bordered().title("watches"))
    .highlight_style(Style::default().bg(Color::Cyan).fg(Color::Black).bold())
    .highlight_symbol("> ");
frame.render_stateful_widget(table, area, &mut state);
```

**List stateful**：
```rust
let mut state = ListState::default();
state.select(Some(0));
let list = List::new(items)
    .block(Block::bordered().title("events"))
    .highlight_style(...);
frame.render_stateful_widget(list, area, &mut state);
```

**覆盖层（Help / Confirm）**：
```rust
let popup = centered_rect(60, 60, area);
frame.render_widget(Clear, popup);   // 清掉后面的内容
frame.render_widget(help_paragraph, popup);
```

**居中 Rect 工具函数**（来自 popup example）：
```rust
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ]).split(r);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ]).split(popup_layout[1])[1]
}
```

**terminal setup / teardown**（必须配对，否则终端坏掉）：
```rust
fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut out = std::io::stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;   // 注意：不要 EnableMouseCapture！
    Ok(Terminal::new(CrosstermBackend::new(out))?)
}
fn cleanup_terminal() -> Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}
// 任何 panic 都必须 cleanup → 用 std::panic::set_hook 或 Drop guard
```

**tokio + crossterm 共存**：
- `crossterm::event::read()` 阻塞；用 `event::poll(timeout)` 短轮询
- 主循环里 `tokio::select!` 监听多个 channel
- monitor 跑在 `tokio::spawn`；UI 跑在主线程 sync 代码里（不必 async）

---

## 9. openapi-tui 参考（设计灵感来源）

仓库：https://github.com/zaghaghi/openapi-tui

### 9.1 借鉴的设计点

- **多 pane 横向布局**，不同 pane 有不同宽度比例（中心 pane 大，两侧窄）
- **底部状态栏** 永远显示所有可用键位，零隐藏快捷键
- **`:` 命令面板** —— 取代所有 modal 弹窗，输入一行命令执行
- **vim 风格键位**：`h/j/k/l` 切 pane / 移动，`g/G` 首尾，`/` 过滤，`:` 命令，`?` 帮助
- **terminal-native 美学**：无装饰边框 / ANSI 颜色 / 反色高亮
- **状态行** 放关键摘要（uptime / 模式 / 计数）

### 9.2 不借鉴的

- **Tabs**：它有 1-9 数字键切 tab；我们不要 tab 栏（§2.5）
- **jq 集成**：它的 `:jq <expr>` 是针对 JSON 响应；我们用不上
- **请求/响应分离**：它有 request editor + response viewer 双栏；我们是 watches list + detail + events 三栏
- **嵌套浏览（`g` 展开/`b` 收回）**：我们的列表是平铺的，没有树

### 9.3 具体可读的源文件

- `openapi-tui/src/main.rs` —— 整体入口
- `openapi-tui/src/tui.rs` 或 `ui.rs` —— 渲染 + 事件循环
- `openapi-tui/src/components/*.rs` —— 每个 pane 一个文件
- `openapi-tui/src/app_state.rs` 或类似 —— App struct

---

## 10. 实施阶段（建议顺序）

### Phase 1：仓库重组（30 分钟）

- [ ] `mkdir py && git mv src py/`
- [ ] `git mv pyproject.toml py/ && git mv examples py/`
- [ ] 创建 `docs/`（已存在 RUST_PORT.md）
- [ ] 创建 `.github/workflows/py.yml`（保留旧的最小 pytest）
- [ ] 写新顶层 `README.md`，顶部"用哪个版本"卡片

### Phase 2：Rust 骨架（1-2 小时）

- [ ] `rs/Cargo.toml` 加上面 §8.4 的依赖
- [ ] `rs/src/paths.rs` — XDG 路径
- [ ] `rs/src/config.rs` — schema + load_or_init + save + 迁移
- [ ] `rs/src/presets.rs` — 5 个影院 const
- [ ] `rs/src/maoyan.rs` — reqwest + serde，抓 cinema + films + movie name
- [ ] `rs/src/notify.rs` — Discord + macOS 子进程
- [ ] `rs/src/monitor.rs` — tokio 调度循环 + 节流 + 心跳
- [ ] 跑一次真实猫眼抓取验证（无 UI）：`cargo run -- doctor`

### Phase 3：Rust CLI（1-2 小时）

- [ ] `rs/src/main.rs` — clap 入口 + 子命令 dispatch
- [ ] `rs/src/cli/*.rs` — 所有子命令（init / start / stop / watch.* / cinema.* / films / config.* / test / doctor）
- [ ] 验证 `cargo run -- watch list`、`cargo run -- films 2`、`cargo run -- doctor` 输出与 Python 版一致

### Phase 4：TUI（3-4 小时，最大块）

- [ ] `rs/src/tui/mod.rs` — App struct + 状态字段 + event loop
- [ ] `rs/src/tui/ui.rs` — 主 render，三栏 + 顶/底
- [ ] `rs/src/tui/focus.rs` — Focus enum + 切换
- [ ] `rs/src/tui/input.rs` — 键盘事件分发
- [ ] `rs/src/tui/cmd.rs` — `:` 命令面板解析
- [ ] Empty state / Help 覆盖层 / Confirm y/n
- [ ] 跑通：添加 watch → 启动 monitor → 看到 events 实时滚动 → 焦点切换 → 命令面板

### Phase 5：CI（30 分钟）

- [ ] `.github/workflows/rs.yml` — `cargo test` + `cargo clippy` + `cargo build --release`
- [ ] 跨平台 runner：ubuntu + macos（Windows 可选 v2）

### Phase 6：Release（30 分钟）

- [ ] `rs/.github/workflows/release.yml`（或在 rs/ 内独立）— tag 触发，cross build 三平台二进制上传 GitHub release
- [ ] `cargo install --path rs` 一行安装文档

---

## 11. 关键文件清单（grep 时从这里开始）

| 文件 | 行数 | 关键内容 |
|---|---|---|
| `py/src/ticket_tracker/__init__.py` | 3 | `__version__ = "1.2.0"` |
| `py/src/ticket_tracker/paths.py` | 40 | XDG 路径 |
| `py/src/ticket_tracker/config.py` | 240 | v2 schema + 迁移 + 所有 watch/cinema 操作 |
| `py/src/ticket_tracker/presets.py` | 42 | 5 个内置影院（上海/北京/深圳） |
| `py/src/ticket_tracker/maoyan.py` | 140 | 猫眼 API 客户端 + 解析 |
| `py/src/ticket_tracker/notify.py` | 147 | Discord + macOS 报警 + caffeinate |
| `py/src/ticket_tracker/monitor.py` | 385 | 调度循环 + check_watch + 心跳 |
| `py/src/ticket_tracker/cli.py` | 746 | 所有 Click 子命令 |
| `py/src/ticket_tracker/tui.py` | 47KB | Textual TUI（**仅参考业务交互，不参考视觉**） |
| `py/examples/config.example.json` | 25 | v1 配置示例（注意：example 还是 v1，实际配置已 v2） |
| `py/README.md` | 全文 | 用户文档 |

---

## 12. 验证清单（完工时跑一遍）

- [ ] `cargo run -- init` 在空 `~/.config/ticket-tracker/` 创建 v2 config
- [ ] `cargo run -- watch add 1490607 -c 37534` 添加一条 watch
- [ ] `cargo run -- films 2` 拉即将上映（验证猫眼 API + UA + SSL 配置）
- [ ] `cargo run -- cinema add-preset 前滩太古里` 加预设
- [ ] `cargo run -- config get quiet` / `set phone-only "05:00-08:00"`
- [ ] `cargo run -- start` 启动 TUI：左栏看到 watches、中栏选中后看 detail、右栏看到 events 滚动
- [ ] `Tab`/`h`/`l` 切换焦点：边框颜色从灰变青
- [ ] `:` 输入 `:interval 30` 回车：事件栏多一条 `✓ cmd: interval = 30s`
- [ ] `r` 立即检查：事件栏多一条 `· 手动触发一轮检查…`
- [ ] `?` 弹帮助覆盖层，任意键关闭
- [ ] 跨平台：Linux runner 跑 `cargo build --release` 通过；非 macOS 下 `notify_macos` 路径静默跳过
- [ ] Python 版本仍能 `pip install -e py/` 并 `tt watch list` 读同一份 config.json（兼容性证明）

---

## 13. 给 clean session 的执行提示

1. 先读本文件**完整**一遍（不用快读，11KB 不大）
2. 读 `py/src/ticket_tracker/config.py` 一遍确认 schema 与迁移函数签名
3. 读 `py/src/ticket_tracker/monitor.py` 一遍确认调度逻辑
4. 跳过 `py/src/ticket_tracker/tui.py`（47KB 太长，且视觉参考价值低）—— 只在需要确认某个字段怎么呈现时再翻
5. 按 §10 的 Phase 顺序推进
6. 任何与本文件决策冲突的设计，先问用户**而非自行决定**

---

*End of document. v1.0 — 2026-07-21*