# ticket-tracker

> 猫眼影城影片预售监测 CLI + TUI 工具 · 配合 Discord Webhook 实时推送通知到手机。

《蜘蛛侠：崭新之日》（或任何电影）一旦在影城开启预售的**那一刻**通知你，免去每次刷新页面的痛苦。

---

## 用哪个版本？

本仓为 monorepo，目前两个实现并存：

| 目录 | 状态 | 语言 / UI |
|---|---|---|
| **[`rs/`](rs/)** | 🎯 正在开发 · 设计稿 → [`docs/RUST_PORT.md`](docs/RUST_PORT.md) | Rust + ratatui |
| **[`py/`](py/)** | 🛠 维护中 · bug-fix only（v1.2.0 功能终版） | Python + Textual |

> ⚠️ **当前可用版本是 `py/`**。`rs/` 还处于骨架阶段，design-freeze 已敲定、Phase 2-4 实现进行中。

## 快速上手（推荐先用 py/）

```bash
git clone https://github.com/kuicao55/ticket_tracker.git
cd ticket_tracker/py
pip install -e .
tt --version          # ticket-tracker 1.2.0
tt doctor             # 自检：依赖 / 网络 / 配置 / macOS 通知
tt init               # 首次创建 ~/.config/ticket-tracker/config.json
tt start              # 进入 Textual TUI
```

完整文档 → [`py/README.md`](py/README.md)。

## Rust 版本（v1.3.0 · TUI 已就绪）

完整设计、阶段计划、Crate 列表 → [`docs/RUST_PORT.md`](docs/RUST_PORT.md)。

```bash
cd rs
cargo install --path .    # 编译并把 `tt` 装到 ~/.cargo/bin
tt --version              # ticket-tracker 1.3.0
```

---

## 文档

- Python 版用法 → [`py/README.md`](py/README.md)
- Rust 重构方案 → [`docs/RUST_PORT.md`](docs/RUST_PORT.md)
- 许可证 → [`LICENSE`](LICENSE)

## 路线图

- Phase 1 — 仓库改 monorepo 布局 ✅
- Phase 2 — Rust 业务骨架（paths / config / presets / maoyan / notify / monitor） ✅
- Phase 3 — Rust CLI 1:1 对齐 Python 版 ✅
- Phase 4 — Rust TUI（ratatui + crossterm） ✅
- Phase 5 — CI（ubuntu + macos runner） ✅
- Phase 6 — Release（cross-build 四平台） ✅
