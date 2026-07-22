# ticket-tracker

> 猫眼影城影片预售监测工具 · CLI + TUI · 配合 Discord Webhook 实时推送通知到手机。

📽️ 你关注的电影一旦在影城开启预售的**那一刻**通知你，免去每次刷新页面的痛苦。

---

## ⚡ 30 秒上手

### 下载安装

前往 [**GitHub Releases**](https://github.com/kuicao55/ticket_tracker/releases)
下载与你的平台对应的压缩包，解压即可：

```bash
# macOS (Apple Silicon)
curl -L -o tt.tar.gz https://github.com/kuicao55/ticket_tracker/releases/latest/download/tt-aarch64-apple-darwin.tar.gz
tar -xzf tt.tar.gz && sudo mv tt /usr/local/bin/

# macOS (Intel) — 把 aarch64 换成 x86_64 即可
# Linux (x86_64) — 把 darwin 换成 unknown-linux-gnu 即可
```

### 第一次运行

```bash
tt doctor     # 自检：网络 / 配置 / 猫眼 API
tt init       # 创建 ~/.config/ticket-tracker/config.json
tt start      # 进入 TUI 开始监控
```

想从源码编译？见 [`rs/README.md`](rs/README.md#安装)。

---

## ✨ 特性

- 🎬 **盯预售**：自定义电影 / 影院 / 日期组合，猫眼一放票就推送
- 🔔 **多渠道通知**：Discord Webhook（推荐，手机秒收）+ macOS 系统通知 + 响铃 + 语音播报 + 自动打开购票页
- ⏱ **灵活频率**：全局默认 90s/次，可对单个 watch 设更短间隔；静默时段自动暂停
- 🎯 **智能去重**：同场次不会重复推送；全影院触发自动停用
- 💻 **原生 TUI**：vim 风键位，三栏布局，所有配置表单化（弹窗输入，无需记命令）
- 🔍 **电影搜索 / 影院收藏**：TUI 里直接搜热映/即将上映电影；常用影院加入收藏夹一键勾选
- 📊 **运行统计**：每次检查的影院状态、触发数、累计检查次数全部可见
- 🪶 **低资源**：Rust 编译产物 ~10MB，常驻内存 < 20MB

---

## 📸 TUI 预览

```
┌─ ticket-tracker ────────────────────────────────────────────────────────────┐
│ ◆ Watches          │ detail · w_abc                │ Events                  │
│ ▶ ▣  蜘蛛侠 崭新… │   名称    蜘蛛侠：崭新之日 (1342)│ [10:42] ✓ w_abc 预售    │
│   ▣  银翼杀手     │   影院    金逸中关村 (5678)    │ [10:41] · w_def 列表有 │
│   ▣  长空之王     │   日期    2026-07-25, 2026-07-26│ [10:40] ✗ w_ghi 网络不 │
│                   │   间隔    60s ✓ enabled        │                          │
│                   │   触发    1/2                  │                          │
│                   │   检查    14 次                │                          │
├───────────────────┴─────────────────────────────┴──────────────────────────┤
│ [a]添加 [r]立即检查 [d]删除 [⚙]配置 [?]帮助 [q]退出    mode=normal  2/3 启用│
└────────────────────────────────────────────────────────────────────────────┘
```

实际渲染按终端宽度自适应缩放。

---

## 🚀 常用命令

| 命令 | 作用 |
|------|------|
| `tt start` | 进入 TUI，开始监控 |
| `tt stop` | 停止后台运行的 TUI |
| `tt doctor` | 环境自检（依赖 / 网络 / 配置） |
| `tt init` | 首次创建配置文件 |
| `tt watch add <movie_id> -c <cinema>` | 命令行添加一条 watch |
| `tt watch list` | 查看所有监视项 |
| `tt config set discord-webhook <url>` | 设置 Discord 通知 |
| `tt films 1` | 查看猫眼热映电影列表 |

完整命令与 TUI 键位见 [`rs/README.md`](rs/README.md)。

---

## 🤝 反馈与贡献

- 🐛 **Bug 报告 / 💡 功能建议**：到 [Issues](https://github.com/kuicao55/ticket_tracker/issues) 提
- 🔧 **提 PR**：欢迎，先开个 issue 讨论最佳
- 📜 **许可证**：MIT
