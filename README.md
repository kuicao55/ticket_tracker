# ticket-tracker

猫眼影城影片预售监测工具 · Rust + ratatui TUI + clap CLI。

实时盯紧你关注的电影/影院/日期组合，一旦猫眼放票就推 Discord 或本机通知，
并把每次检查结果、触发影院数、检查次数等汇总在终端 TUI 中查看。

> 设计稿：[`docs/RUST_PORT.md`](docs/RUST_PORT.md) ·
> Python 参考实现：[`py/`](py/)

---

## 当前版本

**v1.5.3** —— 编辑 watch 自动释放锁状态：
- **匹配范围变化 → 清 `lock_ok_cinemas` + `lock_tries`**：编辑表单保存时，若 cinemas / dates / time_window / imax_only / movie_id 任一改变，自动清空该 watch 的锁票运行状态（旧锁可能落在新范围外），下一轮 tick 重新评估。事件栏推 `🔓 {watch} 匹配范围变化，释放 N 个影院锁票状态`。
- **不影响**：auto_lock 开关本身、锁参数（票数/座位/重试上限）、mode（开售提醒 vs 增场监控）、通知字段、`interval` —— 这些改了只影响下一场，不重置历史。详细副作用表见下方[编辑 watch 的副作用](#编辑-watch-的副作用)。

**v1.5.2** —— 自动锁票语义简化 + 防误锁（在 v1.5.1 基础上）：
- **锁定时段跟随监控时段**：删掉独立的锁票时段输入，`--time-window` 内所有满足条件的场次就是要锁的场次。
- **IMAX 单一开关**：`--imax-only` 同时作用于监测过滤与锁票候选（TUI 表单改标「只看IMAX」，可单独开，不需要同时开自动锁票）。
- **增场首轮不锁票**：影院本轮刚建立/重建基线时跳过锁票（事件栏有提示），修复重启/编辑保存清基线后第一轮误锁下单的雷；TUI 编辑 watch 不再无条件清基线，只有影院/日期/时段/模式真正变化才重建。

**v1.5.1** —— 锁票联调体验修复（在 v1.5.0 自动锁票基础上）：
- **选好座立即通知**：booker stdout 流式读取，`[seat] 选中:`/`[seat] 手动选中:` 一出现就发结果通知，
  不再等浏览器关闭；dry-run 走通正确显示「🧪 锁票测试：脚本走通」而非「失败」。
- **锁票测试即时响应**：TUI 点 `◈ 锁票测试` 立刻唤醒 monitor，不再等满一个检查间隔。
- **通知带实际座位**：结果消息直接带选中的座位号；booker 被自动化驱动时不再滞留浏览器 30s。

**v1.5.0** —— 在 v1.3.0（配置表单化 + 影院收藏夹）基础上新增：
- **微信大群通知**：watch 级 `wechat_notify`，开售/增场时推送到当前打开的微信会话（本地 Python 脚本，不经服务器）。
- **购票链接恢复**：Discord 告警重新带上猫眼购票直达链接。
- **自动锁票**：与本机 [`maoyan-booker`](https://github.com/kuicao55/maoyan-booker) 联动，开售/增场命中时自动用 Playwright 锁座（详见下方[自动锁票](#自动锁票v15)一节）。

---

## 安装

### 方式一：下载预编译二进制（推荐，最省事）

前往 [GitHub Releases](https://github.com/kuicao55/ticket_tracker/releases)，
下载与你的平台对应的压缩包（macOS / Linux / Windows），解压即可：

```bash
# macOS (Apple Silicon)
curl -L -o tt.tar.gz https://github.com/kuicao55/ticket_tracker/releases/latest/download/tt-aarch64-apple-darwin.tar.gz
tar -xzf tt.tar.gz
sudo mv tt /usr/local/bin/

# macOS (Intel)
curl -L -o tt.tar.gz https://github.com/kuicao55/ticket_tracker/releases/latest/download/tt-x86_64-apple-darwin.tar.gz
tar -xzf tt.tar.gz
sudo mv tt /usr/local/bin/

# Linux (x86_64)
curl -L -o tt.tar.gz https://github.com/kuicao55/ticket_tracker/releases/latest/download/tt-x86_64-unknown-linux-gnu.tar.gz
tar -xzf tt.tar.gz
sudo mv tt /usr/local/bin/
```

Windows 用户解压后把 `tt.exe` 放到 `PATH` 任意目录即可。

### 方式二：`cargo install` 从源码编译

需要 [Rust 1.75+](https://rustup.rs/)。

```bash
# 直接装最新版（默认安装到 ~/.cargo/bin/tt）
cargo install --git https://github.com/kuicao55/ticket_tracker.git --bin tt

# 或者克隆后从本地路径安装
git clone https://github.com/kuicao55/ticket_tracker.git
cd ticket_tracker/rs
cargo install --path .
```

> 首次编译大约需要 3–5 分钟（要编译 tokio / ratatui / reqwest 等依赖）。

### 方式三：手动编译 release 二进制

```bash
git clone https://github.com/kuicao55/ticket_tracker.git
cd ticket_tracker/rs
cargo build --release
./target/release/tt --version
```

---

## 验证安装

```bash
tt --version      # → tt 1.5.3
tt doctor         # 自检：配置、网络、猫眼 API
tt init           # 首次运行：创建 ~/.config/ticket-tracker/config.json
```

配置文件位置（遵循 XDG 规范）：

| 平台 | 路径 |
|------|------|
| macOS / Linux | `~/.config/ticket-tracker/config.json` |
| Windows | `%APPDATA%\ticket-tracker\config.json` |

---

## 快速上手

```bash
# 1. 进入 TUI（主界面）
tt start

# 2. 在 TUI 里：
#    - 底部 Action Bar 按 [a] 添加一条 watch → 弹出表单
#    - 表单里：
#        电影 ID：按 Enter 搜索（热映/即将上映，←→ 切换），或按 i 手动输入
#        影院：   按 Enter 打开影院收藏夹，Space 勾选、d 删除收藏
#        日期：   留空 = 不限，或填 YYYY-MM-DD（可多个，空格分隔）
#        名称：   选填
#        间隔：   留空 = 用全局间隔
#    - 添加后立即看到该 watch 在左栏出现，监控线程同步在跑
#    - 中栏 Detail 显示：名称、影院、日期、间隔、启用状态、触发数、检查次数
#    - 右栏 Events 滚动显示最近的检查/推送事件
#    - 全局 Action Bar 按 [r] 立即检查、按 [⚙] 改全局设置、按 [?] 看帮助
```

如果只在终端用命令行（不进入 TUI），也可以：

```bash
tt watch add 1234 --name "你的电影名" -c 5678 -d 2026-07-25
tt start                       # 进入 TUI 开始监控
tt watch list                  # 查看所有监视项
tt watch enable <wid>          # 启用
tt cinema add-preset 海淀       # 从内置影院预设加入收藏夹
```

---

## CLI 命令一览

| 命令 | 说明 |
|------|------|
| `tt start [--detach] [--interval <sec>] [--watch <wid>...]` | 进入 TUI；可选后台运行 |
| `tt stop` | 停止后台 TUI |
| `tt watch list / add / show / edit / remove / enable / disable / toggle` | 监视项 CRUD |
| `tt cinema list / add / remove / presets / add-preset` | 影院管理 |
| `tt films [1\|2\|3]` | 拉取猫眼电影列表（1=热映 2=即将 3=经典） |
| `tt config show / get / set / unset / path` | 配置读写 |
| `tt test [all\|discord\|macos]` | 通知测试 |
| `tt doctor` | 自检（环境 + 网络 + 配置 + API） |
| `tt init` | 首次创建配置文件 |
| `tt log` | 查看最近日志 |

所有子命令与 Python 版 1:1 同名同参数；配置文件互相兼容，可在两个版本之间无缝切换。

---

## TUI 键位（完整版）

### 区块导航

| 按键 | 作用 |
|------|------|
| `Tab` / `Shift+Tab` / `←` / `→` | 切换左中右三栏 + 底部 Action Bar |
| `Enter` (在 Top 模式) | 进入当前栏的子内容 |
| `Esc` (在 In 模式) | 退回 Top 模式 |
| `?` | 弹出/关闭帮助覆盖层 |
| `q` / `Ctrl+C` | 干净退出（自动发 Discord「已停止」通知） |

### Watches（左栏）

| 按键 | 作用 |
|------|------|
| `j` / `k` / `↑` / `↓` | 选上/下一条 watch |
| `g` / `G` | 跳到第一条/最后一条 |
| `Enter` | 进入 Detail 栏 |

### Detail（中栏）

| 按键 | 作用 |
|------|------|
| `←` / `→` / `h` / `l` | 在底部 per-watch 按钮间移动 |
| `Enter` | 触发当前按钮（启停 / 立即检查 / 删除 / 编辑影院·日期·间隔） |

### Events（右栏）

| 按键 | 作用 |
|------|------|
| `j` / `k` / `↑` / `↓` | 上下滚动事件 |
| `g` / `G` | 首/尾 |

### 全局 Action Bar（底栏）

| 按钮 | 作用 |
|------|------|
| `[a]` 添加 | 弹出表单：搜索电影 / 选影院 / 设日期 / 命名 / 间隔 |
| `[r]` 立即检查 | 强制跑一轮所有启用 watch |
| `[d]` 删除 | 删除当前 watch（二次确认） |
| `[⚙]` 配置 | 弹全局设置表单：webhook / check_interval / quiet_window / phone_only_window / heartbeat / 自动锁票(全局) / 真锁 / 无头 |
| `[?]` 帮助 | 弹键位说明 |
| `[q]` 退出 | 干净关闭 TUI |

### 表单弹窗

| 按键 | 作用 |
|------|------|
| `↑` / `↓` / `j` / `k` | 在字段之间切换 |
| `Enter` | 编辑当前字段 / 触发按钮 / 搜索电影 |
| `i` | 电影 ID 字段：手动输入数字 |
| `Esc` | 关闭弹窗 |
| 字符 / Backspace | 编辑中输入或删除 |

### 电影搜索弹窗（表单里按 Enter 触发）

| 按键 | 作用 |
|------|------|
| `←` / `→` / `h` / `l` | 切换 热映 / 即将上映 |
| `↑` / `↓` / `j` / `k` | 选上一/下一部 |
| `Enter` | 选中回填到电影 ID + 名称 |
| `Esc` | 返回表单 |
| `r` | 加载失败时重试 |

### 影院收藏夹弹窗（表单里 Enter 触发）

| 按键 | 作用 |
|------|------|
| `↑` / `↓` / `j` / `k` | 选择 |
| `Space` | 勾选/取消当前 |
| `d` / `Delete` | 删除当前收藏（内置影院不可删） |
| `Tab` | 进入「输入影院 ID」模式 |
| `Enter`（在列表模式） | 确认选择回填表单 |
| `Enter`（在输入模式） | 按 ID 拉取影院名，加载成功后自动加入收藏夹 |
| `Esc` | 返回表单 |

---

## v1.3.0 新增功能

- **配置表单化**：所有 `add` / `edit` / `webhook` / `interval` 等操作全部从命令面板改为弹窗表单，键盘即可完成；不再需要记住命令语法。
- **电影搜索**：在「添加 watch」表单里直接按 Enter 搜索猫眼热映/即将上映；←→ 切换分类；加载异步、不阻塞 UI、Esc 可取消。
- **手动输入电影 ID**：表单中按 `i` 进入编辑模式，直接键入数字。
- **影院收藏夹**：常用影院一次勾选多次使用；`d` 删除普通收藏；`★` 标识的内置影院不可删。
- **新影院自动持久化**：通过 ID 查到的新影院立即加入收藏夹，下次直接复用。
- **Detail 累计检查次数**：每次非错误的检查 +1（与全局计数语义一致）；interval 跳过、禁用、错误均不计。
- **TUI 状态实时同步**：添加 / 删除 / 编辑 / 切换启停都直接改 Monitor 内存配置，UI 立即刷新，无需重启。

完整变更历史见 [CHANGELOG](https://github.com/kuicao55/ticket_tracker/commits/main)。

---

## 自动锁票

> ⚠️ 依赖本机 [`maoyan-booker`](https://github.com/kuicao55/maoyan-booker)（Playwright PoC），
> 通过 `uv run python booker.py ...` 调用。booker 目录默认 `~/Applications/maoyan-booker`，
> 可用环境变量 `MAOYAN_BOOKER_DIR` 覆盖（booker 自带登录态 session，可复用）。

**自动锁票只看 watch 级开关**：该 watch 的 `--auto-lock`（CLI 或 TUI 表单里的「自动锁票」）
为 true 才会触发；没有全局总闸，每个 watch 独立决定，不开启的 watch 完全不碰锁票。

### 触发时机

- **开票提醒（presale）**：某影院首报开售时；
- **增场监控（incremental）**：每轮发现新增场次时。
- 命中场次会同时注入 booker 的 `--show-date`（最早场次日期）与
  `--show-time-range`（即监测的时段窗口 `--time-window`）。

通知与锁票**互不干扰**：通知走 watch 既有的通道（Discord / 邮箱 / 小红书 / 微信 / macOS），
锁票独立触发；同一影院锁成功后本 watch 不会再重复锁。

### CLI

```bash
# watch 级：给某个 watch 开自动锁票 → 命中就真锁（15 分钟内去 App 付款），不开就不锁
tt watch add 1234 -c 37534 --auto-lock \
    --time-window "19:00-22:00" \              # 监控时段即锁票时段，无需单独设定
    --imax-only \                              # 监测与锁票共用一个 IMAX 开关
    --lock-num-seats 2 \
    --lock-max-retries 3 \
    --lock-seat "5排6座" --lock-seat "6排7座"   # 可多次；不传=智能选座
tt watch edit <wid> --auto-lock true --lock-num-seats 2
```

全局配置别名：`lock-headless`（默认 true 无头，false 带浏览器调试）；watch 级字段：`auto-lock`。
> 从 v1.5.1 起没有「dry-run vs 真锁」开关——watch 开了 `auto-lock` 就**真锁**；
> dry-run 只存在于 TUI 的「◈ 锁票测试」按钮（强制不真锁）。

### 关键细节

- **开了就真锁**：watch 级 `auto_lock` 打开后，命中场次会传 `--confirm` 真锁
  （booker 点到确认、票锁到账号，15 分钟内去 App 付款）；不想要锁票的 watch 直接不开启。
  状态（`lock_ok_cinemas[]` / `lock_tries{}`）只在真锁成功或重试耗尽时落盘。
- **锁票范围跟随监控时段**：不再有单独的锁票时段输入。监测时段（`--time-window`）内
  所有满足条件的场次就是你要锁的场次，锁票自动取同一窗口传给 booker。
- **IMAX 单一开关**：`--imax-only` 同时作用于**监测过滤与锁票候选** —— 开了 IMAX，
  非 IMAX 场次既不会报也不会锁；关了则 IMAX 与非 IMAX 一起监控、一起锁。
- **增场首轮不锁票**：增场监控的影院本轮刚建立/重建基线时，该轮**只建基线不锁票**
  （事件栏提示「首轮建基线，本轮跳过锁票」）。防止基线清零/重启/编辑保存后，
  下一轮把所有已知场次当"新增"直接跑去真锁下单。
- **重试**：单影院默认最多 `--lock-max-retries 3` 次；`场次没了/流程异常/超时/未知` 各消耗一次。
  同轮并发锁票串行执行（booker 全局一把 inflight 锁），忙时不占重试额度。
- **停用**：锁票 watch 在「全部影院锁成 或 重试耗尽」后才自动停用；否则保持 enabled 继续盯。
- **锁成功通知**：沿该 watch 既有通道（Discord/邮箱/小红书/微信/macOS）发「🔒 自动锁票成功」，
  带「⚠️ 15 分钟内到猫眼 App 完成支付」提示；booker 的锁票截图存
  `~/Applications/maoyan-booker/screenshots/05_locked.png`。

### 编辑 watch 的副作用

> TUI 编辑表单 / `tt watch edit` 保存时，会按下方表格清理运行期状态。改之前
> 想清楚：清掉 `seen_shows` → 下一轮所有已知场次当"新增"；清掉 `lock_ok_cinemas`
> → 该影院可以重新触发锁票（**会真锁**，别手滑）。

| 改动字段 | `seen_shows` 基线 | `lock_ok_cinemas` + `lock_tries` |
|---|---|---|
| `cinemas` / `dates` / `time_window` | ✅ 清空 | ✅ 清空 |
| `imax_only` / `movie_id` | ✅ 清空 | ✅ 清空 |
| `mode`（开售提醒 ↔ 增场监控） | ✅ 清空 | ❌ 保留（mode 只影响通知，不影响锁范围） |
| `auto_lock` 开关本身（true↔false） | ❌ 保留 | ❌ 保留（设计原则：开关即真锁） |
| `lock_num_seats` / `lock_seats` / `lock_max_retries` | ❌ 保留 | ❌ 保留（锁是历史事实，下场用新参数锁） |
| 通知字段 / `interval` | ❌ 保留 | ❌ 保留 |

锁状态清空时，事件栏会推一条 `🔓 {watch} 匹配范围变化，释放 N 个影院锁票状态`
让你知道。如果不想要被释放，编辑时只改通知/参数就行，别碰 cinemas/dates/
time_window/imax_only/movie_id。

### 手动锁票测试（TUI）

不想等开售，也可以随时在 TUI 里过一遍锁票链路：Detail 栏按钮行按 `→` 选中
**「◈ 锁票测试」** 再按 Enter。

- 走一次完整检查拿到当前匹配场次，挑**最早开场的一个影院**跑 booker；
- **强制有头浏览器**（`headless=false`）+ **dry-run**（`confirm=false`），只演示不真锁，
  你可以亲眼看到 Playwright 打开浏览器选座、停在确认前；
- 结果会沿该 watch 既有的全部通道**发一条测试通知**（标题带🧪），顺便验证通知链路；
- **不改任何锁状态**：不记锁成功、不耗重试额度，随时可以放心点。
- 若 watch 当前没有匹配场次（没开售/已过期/IMAX 筛选后无场次）会提示「跳过我」。

---

## 系统要求

- **macOS**：10.15+（Apple Silicon 与 Intel 均原生支持）；系统通知、响铃、语音播报、自动打开浏览器仅在 macOS 生效。
- **Linux**：主流发行版（需要终端支持 ANSI 转义和 raw mode，xterm / gnome-terminal / alacritty / kitty 等均可）。
- **Windows**：Windows 10+（PowerShell / Windows Terminal）。
- **网络**：能访问 `m.maoyan.com`；可选配 Discord Webhook 推送到手机。

依赖（自动拉取，无需手动装）：

- `tokio` · `reqwest` · `serde_json` · `ratatui` · `crossterm` · `chrono` · `regex` · `anyhow`

---

## 设计要点

- **与 Python 版共用同一份 config.json**：完全无感切换
- **三栏 + 状态栏 + 表单弹窗**（受 openapi-tui 启发）
- **限 16 色 ANSI**：兼容老终端，无鼠标依赖
- **vim 风键位**：`j/k/h/l/g/G/?`
- **tokio 调度循环 + ratatui 渲染**：检查与渲染完全解耦，UI 永不阻塞

## 文件结构

```
rs/src/
├── main.rs         # clap 入口
├── lib.rs          # 库导出
├── paths.rs        # XDG 路径
├── config.rs       # v2 schema + 迁移 + 所有 watch/cinema 操作
├── presets.rs      # 5 个内置影院
├── maoyan.rs       # reqwest 客户端
├── notify.rs       # Discord + macOS + caffeinate
├── monitor.rs      # tokio 调度循环 + check_watch + heartbeat
├── cli/            # clap 二级子命令
└── tui/            # ratatui TUI
```

---

## 反馈与贡献

- **Bug / 建议**：在 [Issues](https://github.com/kuicao55/ticket_tracker/issues) 提
- **PR**：欢迎，先开 issue 讨论再发 PR 最佳
- **许可证**：MIT
