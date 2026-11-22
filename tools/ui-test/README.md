# UI regression tests

ticket-tracker Web 面板的浏览器级回归测试（**仅开发使用**）。

这些文件不在 `rs/web/` 下，不会被 `include_dir!` 编进 `tt-server`，也不在
Release / Docker 产物里。

## 依赖

- 已构建的 `tt-server`：`cd rs && cargo build --bin tt-server`
- Node.js >= 20（需要内置 `fetch`；`playwright-core` 要求 >= 20）
- Chrome / Chromium
- `sqlite3` CLI（macOS 自带；Ubuntu 用 `apt-get install -y sqlite3`）
- `npm install` 安装 `playwright-core`（不会下载浏览器，直接驱动系统 Chrome）

## 运行

```bash
cd tools/ui-test
npm install
npm test
```

可选环境变量：

| 变量 | 作用 |
|---|---|
| `TT_SERVER_BIN` | 指定 `tt-server` 路径（默认 `rs/target/debug/tt-server`） |
| `TT_CHROME` | 指定 Chrome/Chromium 可执行文件 |

## 隔离性

runner 会：

1. 在系统临时目录创建独立 `XDG_STATE_HOME` 和 SQLite 文件；
2. 用随机空闲端口启动 `tt-server`，并设置 `TT_NO_MONITOR=true`（不轮询猫眼、不发送通知）；
3. 用随机面板密码登录；
4. 播种一条固定的测试 watch（电影 / 影院都是假数据）；
5. 结束后杀掉进程并删除临时目录。

不会读写 `~/.local/state/ticket-tracker`，也不会影响正在运行的生产实例。

## 覆盖范围

- 登录面板；
- 干净页面 5 秒 HTMX 轮询正常换块；
- 修改 IMAX 后，轮询不会覆盖未保存的改动；
- 经历 HTMX 换块后，日期弹窗重新绑定（上/下月、应用）；
- 时间过滤条件保存并在刷新后回显；
- 已选影院的显示、改动保持和保存落库；
- 页面无 JS 报错。

## 注意

测试依赖 `/watches/:id` 详情页里的 `data-*` 选择器和 5 秒轮询节奏。
如果这些交互契约发生变化，请同步更新 `run.mjs`。
