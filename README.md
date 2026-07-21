# ticket-tracker

> 猫眼影城影片预售监测 CLI + TUI 工具。
> 配合 Discord Webhook 实时推送通知到手机。

帮你在《蜘蛛侠：崭新之日》（或任何电影）一旦在该影城开启预售的**那一刻**通知你，免去每次刷新页面的痛苦。

---

## 特性

- **Textual TUI**：`tt start` 进入漂亮的全屏交互界面（v1.2+），**鼠标 + 键盘双交互**，所有按键显示在底部 `Footer`，新人看一眼就会用
- **后台 daemon**：`tt start --detach` 后台常驻，服务器或无人值守场景适用
- **Discord 推送**：触发开售时立刻推到手机 Discord 应用，含场次数、日期、购票链接
- **macOS 电脑报警**：`caffeinate` 防休眠 + 系统通知 + 玻璃铃声 + 语音播报 + 自动开购票页，1 分钟后自动停止绝不骚扰
- **多影院 × 多电影 × 多日期**：一条 watch = 1 部电影 × N 个影院 × 可选日期集合
- **films 助手**：`tt films` 列出猫眼热映/即将上映电影（最多 20 条），结果可一键加入 watch
- **内置预设**：5 个常用影院（上海前滩太古里 MOViE MOViE 等）一键加入
- **分时段策略**：
  - `01:00-06:00` 完全静默（不抓取、不推送）
  - `06:00-09:00` 只推手机，不在电脑响铃
  - 其他时间正常
- **定期报告**：Discord 每隔 1 小时（可自定义）推送运行报告，标题区分「检测到开售 🎬」vs「例行报告 ✅」；body 列每条 watch 的当前状态、场次数、最早/最晚场次
- **跨平台**：macOS 完整功能；Linux/Windows 仅 Discord 通知（电脑报警自动跳过）
- **可打包**：`pyproject.toml` + console_scripts，`pip install .` 即可

---

## 安装

需要 Python 3.8+。

```bash
pip install -e .
```

或在隔离环境：
```bash
pipx install .
```

升级：
```bash
pip install -U ticket-tracker
```

---

## 快速上手

```bash
tt --version                # 验证安装

tt init                     # 首次：创建配置 + 加入内置预设影院

tt config set discord-webhook https://discord.com/api/webhooks/...

# 添加 watch（新版语法：MOVIE_ID + -c CINEMA_ID + 可选 -d 日期）
tt cinema add-preset 前滩太古里
tt watch add 1490607 -c 37534 --name "蜘蛛侠：崭新之日"

# 不确定 ID？用 films 找
tt films                    # 猫眼在映电影（最多 20 条，含 ID）
tt films 2                  # 即将上映

tt start                    # 前台 TUI（推荐；按 ? 看热键）
# 或
tt start --detach           # 后台

tt status                   # 查状态
tt log -f                   # 实时日志
tt stop                     # 停止

tt doctor                   # 自检
```

获取影院 ID：访问 https://www.maoyan.com/cinema/<ID> 的 URL 或 `tt cinema add-preset 前滩太古里`（内置预设），TUI 添加弹窗里也有"收藏"按钮可点。
获取电影 ID：https://www.maoyan.com/films/<ID>、`tt films` 列出的列表，或 `tt watch add ...` 时省略 `--name` 脚本会**自动从猫眼拉取**；TUI 添加弹窗里也有"选电影"按钮可点。

---

## 监视模型：movie × cinemas × dates

每条 watch 是一个「电影 + 影院列表 + 日期集合」三元组：

```json
{
  "id": "w_a1b2c3",
  "movie_id": 1490607,
  "movie_name": "蜘蛛侠：崭新之日",
  "cinemas": ["37534", "2127"],
  "dates": ["2026-07-29", "2026-07-30"],   // null = 不限
  "interval": null,                        // null = 用全局 check_interval
  "enabled": true,
  "fired_cinemas": []                      // 已触发报警的影院 ID
}
```

含义：**只要 1490607 在 37534 或 2127 的 2026-07-29 或 2026-07-30 任意一天开售场次 > 0，就报警**。
每个 (watch, cinema) 对只报警一次，不会重复打扰。

### 添加示例

```bash
# 单影院 + 不限日期
tt watch add 1490607 -c 37534

# 多影院
tt watch add 1490607 -c 37534 -c 2127

# 限定日期
tt watch add 1490607 -c 37534 -d 2026-07-29 -d 2026-07-30

# 改（只改你指定的字段）
tt watch edit w_a1b2c3 -d 2026-07-30
tt watch edit w_a1b2c3 --interval 30
```

---

## TUI 使用（v1.2+ Textual）

`tt start` 进入交互式 TUI。**所有按键都显示在底部 Footer，一目了然。**

```
┌─ ticket-tracker ────────────────────────────── 17:52 ─┐
│ ⏱ 已运行 12h34m   🔍 检查 478 次   📡 正常   📱 ✓   │
├──────────────────────────────────────────────────────┤
│ ID          电影         影院         日期    状态   │
│ w_a1b2c3 ✓  蜘蛛侠…     MOViE(37…) 07-29  ✓开售    │
│ w_d4e5f6 ×  阿凡达 3    大光明…    不限   待查    │
├──────────────────────────────────────────────────────┤
│ 最近事件（自动滚动，鼠标滚轮翻页）                    │
│ 17:51:22  ✓ w_a1b2c3 ...                             │
├──────────────────────────────────────────────────────┤
│  Add  Delete  Toggle  Browse  Search  Interval  ...  │  ← Footer
└──────────────────────────────────────────────────────┘
```

### 按键（在 Footer 一目了然）

| 键 | 作用 |
|---|---|
| `a` | 添加 watch（弹出表单；电影 ID 行有"选电影"按钮，影院 ID 行有"收藏"按钮） |
| `d` | 删除 watch（若中部已选 watch，直接删它） |
| `i` | 改全局检查间隔 |
| `w` | 改 Discord webhook URL |
| `q` | 改静默时段 |
| `p` | 改 phone-only 时段 |
| `h` | 改 Discord 报告间隔（默认 1 小时；可设任意秒数） |
| `r` | 立即检查一轮 |
| `Esc` | 收起中部详情面板 |
| `?` | 弹 Markdown 完整帮助 |
| `Ctrl+C` | 退出 |

### 鼠标

| 元素 | 操作 |
|---|---|
| 表格行 | 单击高亮 + 中部显示详情；再点同一行 / 点关闭按钮 / 按 Esc 收起 |
| 详情面板按钮 | 编辑影院 / 日期 / 间隔（弹小窗改字段）、启停、删除、关闭 |
| 弹窗按钮 | 直接点 |
| **底部按钮菜单** | 8 个动作分 3 行排列，鼠标直接点（按键 `[X]` 即对应快捷键） |
| Input 框 | 点击聚焦 |

底部菜单（Grid 流式布局，8 个按钮分 3 行 = 3+3+2，列宽自适应窗口大小）：

```
┌─ [A] 添加     [D] 删除     [R] 立即检查 ──┐
├─ [I] 检查间隔 [W] Discord [Q] 静默时段 ──┤
└─ [P] 只推手机 [?] 帮助                  ┘
```

弹窗（`VerticalScroll` 包裹）：超过 `max-height: 90%` 时自动出滚动条，可用鼠标滚轮 / 方向键滚动。

### Empty State（无 watch）

尚无 watch 时进入 TUI 也 OK，会显示大黄色引导卡片，按 `a` 或点 "+ 添加第一条 watch" 按钮即进入添加流程。

---

## 所有命令

```
tt [--version]                                   版本
tt help [command [subcommand ...]]                帮助（支持嵌套：tt help watch add）
tt init                                          首次配置

tt start [--detach] [--interval N] [--watch ID]  启动（前台 TUI / 后台 / 只跑某条）
tt stop                                          停止后台进程
tt restart                                        重启
tt status                                        一行状态
tt log [-n N] [-f]                               日志

tt watch list                                    列出监视项
tt watch add MOVIE_ID [-c CID]... [-d YYYY-MM-DD]... [--name ...] [--interval N]
tt watch show WATCH_ID                            查看详情
tt watch edit WATCH_ID [-c CID]... [-d DATE]... [--interval N]
tt watch remove WATCH_ID
tt watch enable / disable WATCH_ID

tt cinema list                                   列影院
tt cinema add CINEMA_ID [--name ...]
tt cinema remove CINEMA_ID
tt cinema presets                                内置预设
tt cinema add-preset NAME

tt films [SHOW_TYPE]                            猫眼在映 / 即将上映电影（1=热映 2=即将 3=经典）

tt config show | get KEY | set KEY VALUE | unset KEY | path

tt test [all|discord|macos]                      测试通知
tt doctor                                        自检
```

---

## 配置文件

位置：`~/.config/ticket-tracker/config.json`（XDG 规范）
日志：`~/.local/state/ticket-tracker/log.txt`

用 `tt config show` / `tt config get <key>` / `tt config set <key> <value>` 操作。
特殊 key 别名：`interval` ↔ `check_interval`、`webhook` ↔ `discord_webhook`、`quiet` ↔ `quiet_window`、`phone-only` ↔ `phone_only_window`。

完整示例见 [`examples/config.example.json`](examples/config.example.json)。

---

## 时段策略

默认：
- `01:00-06:00` **静默**：脚本挂着但不抓数据、不推送
- `06:00-09:00` **只推手机**：正常抓取，命中仅推 Discord，电脑不叫
- `09:00-次日01:00` **正常**：电脑报警 + Discord

修改：
```bash
tt config set quiet "00:00-05:00"
tt config set phone-only "05:00-08:00"
```

---

## 核心原理

猫眼移动端公开 JSON 接口，无需登录：
```
GET https://m.maoyan.com/ajax/cinemaDetail?cinemaId=<影城ID>
→ showData.movies[] 是该影城当前【已开预售/已排片】的电影列表
```

只要目标 `movie_id` 出现在某个影院 movies 列表中且 `shows[].plist[]` 非空（即有场次日期），即代表该影院对该电影开放了预售。

---

## 开发

```bash
git clone https://github.com/you/ticket-tracker
cd ticket-tracker
pip install -e .

# 跑测试（如果有）
pytest
```

发布到 PyPI：
```bash
pip install build twine
python -m build
twine upload dist/*
```

---

## License

MIT