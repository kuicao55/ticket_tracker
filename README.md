# ticket-tracker

猫眼影城影片预售监测：**v4 无头主服务 + Web 面板**。

实时盯紧电影 / 影院 / 日期组合，一旦猫眼放票就：

- 通过 Discord / webhook / 邮件直接通知；
- 或把微信 / 小红书 / 自动锁票请求投递给独立的 [`Gofer`](https://github.com/kuicao55/Gofer) 服务。

主服务负责抓取、基线、触发判定和面板；Gofer 负责本机 GUI 动作和锁票。

![Web 面板](docs/assets/dashboard.png)

---

## 当前版本

**v4.8.0** —— 只保留 v4 服务端：

- `tt-server`：无头主服务 + SQLite + Web 面板
- 外部 Gofer：微信 / 小红书 / 锁票（独立项目）；watch 渠道可复用 Settings 里的全局绑定
- iGlance `iglance.service.v1` 服务绑定（状态 / 内存 / 暂停 / 恢复 / 停止 / 一键打开面板）
- Release 二进制内置 Web 资产，可扁平安装（`tt-server` + `start-ticket-tracker.sh`）
- Web 面板密码登录：首次升级引导 `/setup` 设密，Argon2 哈希 + 会话 Cookie
- 每 watch 独立通知渠道参数
- 今日 Poll 健康图
- 监控暂停 / 恢复开关
- GitHub Release 面板自动更新

---

## 功能

- **poll / watch 两层**：影院去重抓取，watch 按电影 / 影院 / 时间 / IMAX 过滤。
- **小时桶差分**：首轮静默建基线；任一桶计数增加才触发，避免重复报警。
- **Web 面板**：KPI、watch leaderboard、poll 列表、poll 健康、用户、事件、渠道配置。
- **每 watch 独立渠道**：
  - Discord / webhook：URL
  - Email：收件人
  - 小红书：群名
  - 微信：发到当前打开的会话
  - 自动锁票：票数 / 重试 / 座位
  - 微信 / 小红书 / 自动锁票的 Gofer：可复用 Settings 里的全局 Gofer，也可单独填写地址和 token
- **服务开关**：Dashboard 顶部暂停/恢复监控；暂停期间面板在线，不轮询、不通知、不锁票。
- **投递解耦**：监控不探活 Gofer / 微信 / 小红书；Gofer 离线只记录一条投递失败，不影响监控健康。
- **Docker**：主服务可容器化；Gofer 因 GUI 依赖必须原生运行在 Mac 上。

---

## 快速开始

### 新 Mac 安装（Release 二进制，无需 clone 仓库）

Web 静态资源（CSS / JS / favicon）已经编进 `tt-server`，安装只需要两个文件：

```bash
mkdir -p ~/Services/ticket_tracker
cd ~/Services/ticket_tracker

# Apple Silicon 用 aarch64；Intel 换成 x86_64
BASE=https://github.com/kuicao55/ticket_tracker/releases/latest/download
curl -fL -o tt-server "$BASE/tt-server-aarch64-apple-darwin"
curl -fL -o start-ticket-tracker.sh "$BASE/start-ticket-tracker.sh"
chmod +x tt-server start-ticket-tracker.sh

./start-ticket-tracker.sh start --manifest
# 面板：http://127.0.0.1:8787
```

- 脚本会自动查找同目录的 `tt-server`，不需要设置 `TT_SERVER_BIN`。
- 不需要仓库里的 `rs/web`；首次启动会把内置资源释放到
  `~/.local/state/ticket-tracker/runtime/v<version>/web`，内容相同不会重写。
- 开发时如需热改 CSS/JS，可显式设置 `TT_WEB_DIR=/path/to/rs/web`。
- `tt-server` 不带参数运行时默认 serve `127.0.0.1:8787`，数据库默认
  `~/.local/state/ticket-tracker/ticket-tracker.db`。

### 1. 编译主服务

```bash
cd rs
cargo build --release --bin tt-server
```

### 2. 启动

```bash
# 仓库根目录
./start-server.sh start --open
# 或 rs/start-server.sh start

./start-server.sh status
./start-server.sh logs
./start-server.sh stop
```

默认面板：<http://127.0.0.1:8787>。  
默认数据库：`~/.local/state/ticket-tracker/ticket-tracker.db`。

### 3. 更新

面板顶部 **Update**：检查 GitHub Release、显示 release notes，
一键下载经 SHA256 校验的新 `tt-server`，替换并重启。

macOS 通过 launchd 部署时，更新后由 KeepAlive 拉起新进程；
通过 `start-server.sh` 部署时，更新会用重启助手接管 PID 和日志。

更新源是 GitHub Release，资产命名：

```text
tt-server-aarch64-apple-darwin
tt-server-x86_64-apple-darwin
start-ticket-tracker.sh
SHA256SUMS
```

发版：

```bash
git tag v4.8.0
git push origin v4.8.0
```

公开仓库通常不需要 token；如果遇到 GitHub 限流，可设置 `TT_UPDATE_TOKEN`。
launchd 模板已设置 `TT_SUPERVISED=launchd`，更新后由 KeepAlive 重启。

### 4. 启动 Gofer（需要微信 / 小红书 / 锁票时）

Gofer 是独立项目：

```bash
cd ../Gofer
cargo build --release
./start-gofer.sh start
```

默认地址：`http://127.0.0.1:8790`，面板同地址。  
然后在主服务 **Settings → Gofer URL / Gofer token** 填对应地址和 token。

> 不需要本机通知/锁票时，可以不运行 Gofer；主服务的 Discord / webhook / 邮件仍然正常。

---

## Web 面板

- `/`：总览、监控开关、watch leaderboard、poll 列表、poll 健康、事件、用户。
- `/watches`：全部 watch。
- `/watches/new`：电影/影院搜索弹窗 + 日期范围 + 小时方块；不需要输入 ID。
- `/watches/:id`：过滤条件、渠道配置、自动锁票、基线图、基线场次表。
- `/users`：用户与订阅。
- `/settings`：检查间隔、静默窗口、Discord 健康告警、Gofer 地址/token、渠道默认值、影院城市。

### 面板登录

- 首次启动或从旧版本升级后，访问 `/` 会跳到 `/setup` 创建面板密码（至少 8 个字符）。
  密码用 Argon2 哈希，只新增 `settings` 表里的 `panel.password_hash` /
  `panel.session_secret` 两行，不改表结构、不影响现有 watch / 数据库数据。
- 之后通过 `/login` 登录，会话 Cookie `tt_session` 有效期 7 天，
  `HttpOnly` + `SameSite=Strict`；右上角「退出」可清除会话。
- 忘记密码：设置 `TT_ADMIN_PASSWORD`（至少 8 个字符）后重启一次，
  新密码会写回数据库并让旧会话失效；之后可移除该环境变量。
- 登录只保护 Web 面板。`/health`、`/static/*`、`/api/v1/*` 保持公开，
  方便 iGlance、Gofer 回调和外部脚本继续工作。
- 通过 HTTPS / 反向代理访问时，设置 `TT_PUBLIC_URL=https://...` 或
  `TT_COOKIE_SECURE=true`，让会话 Cookie 带上 `Secure`。

---

## 配置

配置存在 SQLite `settings` 表，面板修改即时生效。

| key | 说明 | 默认 |
|---|---|---|
| `check_interval` | 轮询间隔秒数 | `90` |
| `silent_window` | 静默时段，如 `01:00-06:00` | `01:00-06:00` |
| `fetch_concurrency` | 影院抓取并发 | `8` |
| `discord_webhook` | 健康告警 webhook | 空 |
| `agent_url` | 全局 Gofer 地址（watch 可复用） | `http://127.0.0.1:8790` |
| `agent_token` | 全局 Gofer token（watch 可复用） | 空 |
| `panel_token` | iGlance / 服务接口 token（空则不鉴权；可用 `TT_SERVICE_TOKEN` 覆盖） | 空 |
| `lock_headless` | 锁票默认 headless | `true` |
| `maoyan_city_id` | 影院搜索城市 | `10` |
| `metrics_retention_days` | 图表保留天数 | `7` |
| `monitor_enabled` | 监控开关 | `true` |

自动更新相关环境变量：

| 变量 | 作用 |
|---|---|
| `TT_UPDATE_TOKEN` | 可选；GitHub 只读 token，避免限流 |
| `TT_UPDATE_API_BASE` | 可选；测试/镜像 API 地址 |
| `TT_SERVICE_TOKEN` | 可选；覆盖 `panel_token` 作为服务接口 token（不写日志） |
| `TT_ADMIN_PASSWORD` | 可选；重置面板登录密码（至少 8 字符，写入数据库后失效） |
| `TT_COOKIE_SECURE` | `true` / `1` 时会话 Cookie 带 `Secure`（HTTPS 反代时使用） |
| `TT_PUBLIC_URL` | 可选；对外面板地址（如 Tailscale / 反代），用于 `urls.display` / `urls.panel` |
| `TT_START_COMMAND` | 可选；覆盖 `controls.start` 返回的启动命令 |
| `TT_START_SCRIPT` | 可选；指定启动脚本路径 |
| `TT_SUPERVISED` | `launchd` 时更新后只退出/停止时 bootout，由 launchd 拉起 |
| `TT_LAUNCHD_LABEL` | launchd label，默认 `com.kuicao.ticket-tracker` |
| `TT_PID_FILE` / `TT_LOG_FILE` | start-server.sh 的 PID / 日志文件 |

---

## API v1

默认 `127.0.0.1:8787`，全部 JSON。

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/health` | 公开健康检查（iGlance manifest 使用） |
| GET | `/api/v1/health` | 服务健康、轮询状态、paused、fetch 统计 |
| GET | `/api/v1/service` | iGlance 服务状态 + 内存 + 控制信息（`/v1/service` 同义） |
| GET | `/api/v1/service/memory` | 仅内存快照 |
| POST | `/api/v1/service/pause` | 暂停监控 |
| POST | `/api/v1/service/resume` | 恢复监控 |
| POST | `/api/v1/service/toggle` | 切换暂停/恢复 |
| POST | `/api/v1/service/stop` | 返回 202 后退出进程 |
| GET | `/api/v1/dashboard` | KPI + watch/poll + events + health |
| GET | `/api/v1/polls` | poll 列表 |
| GET/POST | `/api/v1/watches` | 列表 / 创建或复用 |
| GET/PATCH/DELETE | `/api/v1/watches/:id` | 详情 / 编辑 / 删除 |
| GET/PUT | `/api/v1/watches/:id/channels` | 渠道绑定 |
| GET | `/api/v1/watches/:id/metrics?range=24h\|7d\|30d` | 图表数据 |
| GET/POST | `/api/v1/channels` | 渠道列表 / 新建 |
| GET/PATCH/DELETE | `/api/v1/channels/:id` | 渠道配置 |
| GET/POST | `/api/v1/users` | 用户列表 / 新建 |
| DELETE | `/api/v1/users/:id` | 删除用户 |
| GET | `/api/v1/users/:id/subscriptions` | 用户订阅 |
| GET/POST | `/api/v1/subscriptions` | 订阅列表 / 新建 |
| DELETE | `/api/v1/subscriptions/:id` | 取消订阅 |
| GET/PATCH | `/api/v1/settings` | 全局设置 |
| GET | `/api/v1/movies` | 本地电影缓存 |
| GET | `/api/v1/movies/search?q=` | 搜索任意电影 |
| POST | `/api/v1/movies/refresh` | 刷新猫眼电影列表 |
| GET | `/api/v1/cinemas/search?q=` | 搜索影院 |
| GET | `/api/v1/events` | 内存最近事件 |
| POST | `/api/v1/agent/lock-result` | Gofer 回调锁票结果（best-effort） |

### 创建 watch

```bash
curl -X POST http://127.0.0.1:8787/api/v1/watches \
  -H 'Content-Type: application/json' \
  -d '{
    "movie_id": 1545360,
    "movie_name": "奥德赛",
    "cinemas": ["37534"],
    "time": "2026-11-12 19:00-22:00",
    "imax_only": true
  }'
```

### 暂停 / 恢复监控

面板顶部开关即可；也可以直接更新设置：

```bash
curl -X PATCH http://127.0.0.1:8787/api/v1/settings \
  -H 'Content-Type: application/json' \
  -d '{"monitor_enabled": false}'
```

暂停后 `/api/v1/health` 中 `paused: true`，下一轮不会再抓取。

---

## iGlance 服务绑定

ticket_tracker 实现了 [`iglance.service.v1`](https://github.com/kuicao55/iGlance/issues/1)，
可以在 iGlance 的 **Services** 里像绑定 Gofer 一样绑定它。

### 绑定

iGlance 新增服务时填写：

- 名称：`Ticket Tracker`
- 启动命令：

  ```bash
  /Users/me/Applications/ticket_tracker/start-ticket-tracker.sh start --manifest
  ```

- API token：可选。已设置 `panel_token`（`PATCH /api/v1/settings`）时填写同一个值；
  也可以启动前设置 `TT_SERVICE_TOKEN` 覆盖。

命令启动成功（或已经在运行）后会在 stdout 输出一行：

```text
IGLANCE_SERVICE_MANIFEST {"schema":"iglance.service.v1","service":{"id":"ticket_tracker",...},"base_url":"http://127.0.0.1:8787",...}
```

iGlance 解析该行后自动获得状态、内存和启停接口。**token 不会出现在 manifest、日志或启动命令输出里。**

### 不启动服务，只打印 manifest

```bash
./start-ticket-tracker.sh manifest
# 或直接用二进制
rs/target/release/tt-server manifest --base-url http://127.0.0.1:8787
```

### 接口

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/health` | 公开健康检查，不需要 token |
| GET | `/` | Web 面板（对应 `endpoints.panel` / `urls.panel`） |
| GET | `/api/v1/service` | `schema` / `state` / `pid` / `memory.groups` / `controls.start` |
| GET | `/api/v1/service/memory` | 仅内存快照 |
| POST | `/api/v1/service/pause` | `{"ok":true,"state":"paused","paused":true}` |
| POST | `/api/v1/service/resume` | `{"ok":true,"state":"running","paused":false}` |
| POST | `/api/v1/service/toggle` | 切换并返回当前状态 |
| POST | `/api/v1/service/stop` | `202`，随后退出进程并清理 PID 文件 |

`/v1/service*` 与 `/api/v1/service*` 等价。暂停/恢复就是面板上的监控开关；
`state` 取值为 `running` / `paused` / `stopping`。

鉴权在设置 token 后生效，接受：

```text
X-Gofer-Token: <token>
X-Agent-Token: <token>
Authorization: Bearer <token>
```

没有 token 时放行；token 错误或缺失返回 `401`。

### 内存分组

`memory.groups` 来自一次 `ps -axo pid=,ppid=,rss=,command=` 快照，单位 bytes：

| 分组 | 含义 |
|---|---|
| `service` | tt-server 主进程 |
| `workers` | tt-server 派生的子进程 |

ticket_tracker 不派生 Playwright / Chromium，也不直接驱动微信，因此不统计
浏览器和微信进程。`total_rss_bytes` 是各组之和，`total_processes` 是去重后的进程数。
RSS 会重复计算共享内存，因此是估算值。`ps` 采集失败时 `available:false` 并带 `error`，
接口本身仍返回 200。

### 停止语义

`POST /api/v1/service/stop` 先返回 `202`，约 350ms 后：

1. `?cleanup=true`（默认）终止 tt-server 的派生进程；
2. 删除指向当前进程的 PID 文件；
3. 如果设置了 `TT_SUPERVISED=launchd`，先 `launchctl bootout`，避免 KeepAlive 立即拉起；
4. 进程退出。

`cleanup=false` 只退出主进程。停止不会改变暂停状态。

### 打开网页面板

manifest 的 `endpoints.panel` 和 `GET /api/v1/service` 的 `urls.panel` 都指向
ticket_tracker 的 Web 面板（默认 `/`），iGlance 可以用它在浏览器里一键打开。
默认使用本地监听地址；设置 `TT_PUBLIC_URL`（如 `https://tt.example.com`）后，
`urls.display` / `urls.panel` 会改用对外地址。面板 URL 始终以 `/` 结尾，
不包含 token 或 query，需要登录时由浏览器处理。

---

## 通知与 Gofer

主服务对每个 watch 的渠道 fan-out：

```text
watch 触发
  ├─ Discord / webhook / email：主服务直接发送
  ├─ 微信 / 小红书：POST Gofer /v1/notify
  └─ 自动锁票：POST Gofer /v1/lock
```

- 微信 / 小红书 / 锁票的实际执行、请求历史、能力状态都在 Gofer 面板查看。
- 主服务不探活 Gofer；Gofer 离线只会记录一条投递失败。
- 锁票结果可以通过 Gofer 的 `callback_url` best-effort 回调到主服务，更新 `lock_state`。

详见 [`docs/SERVER.md`](docs/SERVER.md)。

---

## 数据库

SQLite + WAL，迁移文件在 `rs/migrations/`：

- `0001_init.sql`
- `0002_autolock.sql`
- `0003_movies.sql`
- `0004_poll_health.sql`
- `0005_watch_channel_config.sql`

主要表：

`settings` / `cinemas` / `movies` / `polls` / `watches` / `watch_cinemas` / `watch_state` / `watch_metrics` / `poll_health` / `users` / `channels` / `watch_channels` / `subscriptions` / `lock_state`。

备份：

```bash
sqlite3 ~/.local/state/ticket-tracker/ticket-tracker.db \
  ".backup '$HOME/Backups/ticket-tracker-$(date +%Y%m%d-%H%M%S).db'"
```

---

## Docker（仅主服务）

```bash
cd rs
docker compose up -d --build
```

- 容器内监听 `127.0.0.1:8787`，远程用 SSH 隧道。
- Gofer 不能放进 Docker（需要 macOS 微信/小红书 GUI）。

---

## 开发与测试

```bash
cd rs
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

测试覆盖：

- DB / engine / 时间规则
- Web / API smoke
- 自动锁票回调（mock Gofer）

---

## 项目结构

```text
ticket_tracker/
├── rs/
│   ├── src/
│   │   ├── bin/tt-server.rs   # 主服务入口
│   │   ├── server/            # axum API / Web / DB monitor / dispatcher
│   │   ├── assets.rs          # include_dir! 内置 Web 资产 + 启动释放
│   │   ├── db.rs              # SQLite
│   │   ├── engine.rs          # poll / watch 纯逻辑
│   │   ├── maoyan.rs          # 猫眼客户端
│   │   ├── notify.rs          # Discord / webhook / 邮件
│   │   ├── lock.rs            # 锁票结果类型
│   │   ├── service.rs         # iGlance service.v1 manifest / 内存 / 停止清理
│   │   ├── time_spec.rs
│   │   ├── silent.rs
│   │   ├── paths.rs
│   │   └── presets.rs
│   ├── migrations/
│   ├── templates/
│   ├── web/
│   └── tests/
├── docs/
│   ├── SERVER.md
│   └── assets/
├── start-server.sh            # 兼容入口 → rs/start-server.sh
├── start-ticket-tracker.sh    # 自包含启动入口（扁平安装 / iGlance 绑定）
└── README.md
```

---

## 故障排查

### 面板打不开

```bash
./start-server.sh status
tail -n 100 ~/.local/state/ticket-tracker/tt-server.log
```

### 端口被占用

```bash
lsof -nP -iTCP:8787 -sTCP:LISTEN
```

### 监控一直暂停

Dashboard 顶部开关切回「运行中」，或：

```bash
curl -X PATCH http://127.0.0.1:8787/api/v1/settings \
  -H 'Content-Type: application/json' \
  -d '{"monitor_enabled": true}'
```

### Gofer 发送失败

主服务只负责投递，排查看 Gofer：

```bash
cd ../Gofer
./target/release/gofer health
./target/release/gofer requests --limit 20
tail -f ~/.local/state/gofer/gofer.log
```

---

## 安全

- 默认只监听 `127.0.0.1`；远程访问用 SSH 隧道。
- Web 面板现在需要密码登录；首次升级后访问 `/setup` 创建。密码与 API token 相互独立。
- `/api/v1/*` 仍保持原有公开行为（`/api/v1/service*` 可选 `panel_token`），不要把 8787 直接暴露公网。
- Gofer 的 `agent_token` 与 Gofer 的 `GOFER_TOKEN` 必须一致；token 泄露等于交出本机通知/锁票能力。
- 不要把 8787 / 8790 直接暴露公网。
- 锁票是真锁，生产 watch 的 `auto_lock` 请谨慎开启。
