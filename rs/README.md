# ticket-tracker (Rust)

Rust + ratatui 重写版。设计稿：[`../docs/RUST_PORT.md`](../docs/RUST_PORT.md)。

## 状态

v1.3.0 · ratatui/crossterm TUI + clap CLI 1:1 对齐 `../py/`。

## 安装

```bash
cargo install --path .
# 或下载 release 二进制
```

`tt --version` 应输出 `tt 1.3.0`。

## 用法

`tt start` —— 进入 TUI（左 watches + 中 detail + 右 events + `:` 命令面板）
`tt watch list/add/show/edit/remove/enable/disable` —— 监视项管理
`tt cinema list/add/remove/presets/add-preset` —— 影院管理
`tt films [1|2|3]` —— 猫眼电影列表（1=热映 2=即将 3=经典）
`tt config show/get/set/unset/path` —— 配置读写
`tt test [all|discord|macos]` —— 通知测试
`tt doctor` —— 自检
`tt init` —— 首次创建配置

所有子命令与 `../py/` 版 1:1 同名同参数。配置文件共用
`~/.config/ticket-tracker/config.json`。

## TUI 键位（详见 RUST_PORT.md §7.6）

- `Tab` / `h` `l` —— 切焦点 pane
- `j` `k` `↑` `↓` —— 焦点 pane 内上下移动
- `g` `G` —— 首/尾
- `/` —— 过滤
- `:` —— 命令面板
- `?` —— 帮助覆盖层
- `r` —— 立即触发一轮检查
- `a` `d` `e` —— 添加/删除/编辑
- `q` / `Ctrl+C` —— 退出

## 设计要点

- 与 Python 版共用同一份 config.json，完全无感切换
- 三栏 + 状态栏 + `:` 命令面板（参考 openapi-tui）
- 限 16 色 ANSI，无鼠标，无嵌套 modal
- vim 风键位
- tokio 调度循环 + ratatui 渲染

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
