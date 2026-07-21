"""Textual TUI：监视 dashboard + 鼠标/键盘交互。
- Header / StatsBar / WatchesTable (DataTable) / WatchDetailPanel / ActionMenu
- 8 个 BINDINGS 自动显示在 Footer
- 中部区块：未选 watch 时显示提示；选中后显示详情 + 编辑按钮（编辑按钮推 ModalScreen）
- Monitor 跑在后台线程，事件/命令走 queue.Queue
"""

import threading
import time

from rich.text import Text
from textual import on
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Container, Grid, Horizontal, VerticalScroll
from textual.reactive import reactive
from textual.screen import ModalScreen
from textual.widgets import (
    Button, DataTable, Header, Input, Label, ListItem, ListView,
    Markdown, Static,
)

from . import config as cfg_mod
from . import maoyan, notify
from .paths import log_file


# ----------------- 入口 -----------------

def run_tui(monitor):
    """前台 TUI 入口（cli.py 调用）。"""
    import os
    # 幂等启动 caffeinate：cli.py 通常已启过一次，这里检测到就不再启，
    # 避免 caffeinate 进程叠加 + 关不干净
    caffeine = None
    if not notify.is_caffeinated():
        caffeine = notify.caffeinate_start(os.getpid())
    try:
        # 注入 force_check_evt 让 App 的 'r' 热键立即触发一轮检查
        monitor.force_check_evt = threading.Event()
        # 真正启动检查循环：跑在 daemon 线程里，TUI 退出时由 _stop_evt 干净停掉
        monitor_thread = threading.Thread(
            target=monitor.run, name="tt-monitor", daemon=True)
        monitor_thread.start()
        try:
            TTApp(monitor=monitor).run()
        finally:
            monitor.stop()
            monitor_thread.join(timeout=5)
    finally:
        if caffeine is not None:
            notify.caffeinate_stop(caffeine)


# ----------------- 主 App -----------------

class TTApp(App):
    """ticket-tracker Textual App。"""

    CSS = """
    Screen {
        layout: vertical;
    }
    #stats {
        height: 3;
        background: $panel;
        color: $text;
        padding: 0 2;
    }
    #main {
        height: 1fr;
        min-height: 6;
        layout: vertical;
    }
    #watches {
        height: 1fr;
        min-height: 4;
        margin: 0 1 0 1;
    }
    #empty {
        height: 1fr;
        margin: 1;
        border: round yellow;
        padding: 2 4;
        content-align: center middle;
        display: none;
    }
    App.-no-watches #empty {
        display: block;
    }
    App.-no-watches #watches {
        display: none;
    }
    #detail {
        height: auto;
        margin: 0 1 0 1;
        padding: 1 2;
        border: round $primary;
        background: $surface;
    }
    #detail-title {
        text-style: bold;
        color: $accent;
        margin-bottom: 1;
    }
    #detail-info {
        height: auto;
        margin-bottom: 1;
    }
    #detail-buttons {
        layout: grid;
        grid-size: 3;
        grid-columns: 1fr;
        grid-rows: 3;
        grid-gutter: 0 1;
        height: auto;
    }
    #detail-buttons Button {
        width: 1fr;
        height: 3;
        margin: 0;
    }
    .empty-hint {
        color: $warning;
        text-style: bold;
    }
    #actions {
        height: auto;
        layout: grid;
        grid-size: 3;
        grid-columns: 1fr;
        grid-rows: 3;     /* 9 个按钮刚好 3 行 × 3 列 */
        grid-gutter: 0 1;
        background: $panel;
        padding: 0 1;
        margin: 0 1 1 1;
    }
    #actions Button {
        width: 1fr;
        height: 3;
        margin: 0;
    }
    ModalScreen {
        align: center middle;
    }
    .modal-box {
        width: 70;
        height: auto;
        max-height: 90%;
        border: round $primary;
        background: $surface;
        padding: 1 2;
    }
    .modal-title {
        text-style: bold;
        color: $accent;
        margin-bottom: 1;
    }
    .hint {
        color: $warning;
        margin-bottom: 1;
    }
    .modal-subtitle {
        text-style: bold;
        color: $secondary;
        margin-top: 1;
        margin-bottom: 0;
    }
    Input {
        margin-bottom: 1;
    }
    .inline-form {
        layout: horizontal;
        height: 3;
        margin-bottom: 1;
    }
    .inline-form Input {
        width: 1fr;
        margin-bottom: 0;
    }
    .btn-row {
        layout: horizontal;
        height: 3;
        align: right middle;
    }
    Button {
        margin-left: 1;
    }
    .help-md Markdown {
        margin: 1 0;
    }
    """

    BINDINGS = [
        Binding("a", "add_watch", "Add", priority=True),
        Binding("d", "delete_watch", "Delete", priority=True),
        Binding("i", "edit_interval", "Interval", priority=True),
        Binding("w", "edit_webhook", "Webhook", priority=True),
        Binding("q", "edit_quiet", "Quiet", priority=True),
        Binding("p", "edit_phone", "Phone", priority=True),
        Binding("h", "edit_heartbeat", "Heartbeat", priority=True),
        Binding("r", "force_check", "Check now", priority=True),
        Binding("escape", "close_detail", "Close detail", show=False),
        Binding("?", "help", "Help", priority=True),
        Binding("ctrl+c", "quit", "Quit", show=False),
    ]

    TITLE = "ticket-tracker"

    def __init__(self, monitor):
        super().__init__()
        self.monitor = monitor
        self._cfg = monitor.cfg

    def compose(self) -> ComposeResult:
        yield Header(show_clock=True)
        yield StatsBar(monitor=self.monitor, id="stats")
        with Container(id="main"):
            yield WatchesTable(id="watches")
            yield EmptyState(id="empty")
        yield WatchDetailPanel(id="detail")
        yield ActionMenu(id="actions")

    def on_mount(self):
        self._selected_watch_id = None
        self._last_watches_fp = None
        self._refresh_watches(initial=True)
        self.set_interval(1.0, self._refresh_watches)

    def _refresh_watches(self, initial=False):
        """刷新表格。仅当 watches 内容真的变化时才 clear+re-add（避免横向滚动条跳回）。"""
        self._cfg = cfg_mod.load_or_init()
        # 把 monitor 持有的 cfg 引用也刷掉
        self.monitor.cfg = self._cfg
        # 空 / 非空切换
        if not self._cfg["watches"]:
            self.add_class("-no-watches")
        else:
            self.remove_class("-no-watches")
        # 状态栏无脑刷新（uptime / check_count 一直在变）
        try:
            self.query_one("#stats").refresh_stats(self.monitor, self._cfg)
        except Exception:
            pass
        # 表格：只有 watches 真正变了才 rebuild
        fp = self._watches_fingerprint(self._cfg)
        if initial or fp != self._last_watches_fp:
            self._last_watches_fp = fp
            try:
                self.query_one("#watches").refresh_from_cfg(self._cfg)
            except Exception:
                pass
            # 选中条目可能已不存在 / 内容已变 → 同步详情面板
            if self._selected_watch_id is not None:
                w = cfg_mod.find_watch(self._cfg, self._selected_watch_id)
                if w is None:
                    self._close_detail()
                else:
                    self.query_one("#detail").update_for_watch(w)

    @staticmethod
    def _watches_fingerprint(cfg):
        """用于判断 watches 列表是否真的变了。"""
        return tuple(
            (w["id"], w.get("enabled"), w.get("movie_name"),
             w.get("_last_status"), w.get("interval"),
             tuple(w.get("cinemas") or []),
             tuple(w.get("dates") or ()),
             tuple(w.get("fired_cinemas") or ()))
            for w in cfg["watches"]
        )

    def _show_detail(self, watch_id):
        cfg = cfg_mod.load_or_init()
        w = cfg_mod.find_watch(cfg, watch_id)
        if not w:
            return
        self._selected_watch_id = watch_id
        try:
            self.query_one("#detail").update_for_watch(w)
        except Exception:
            pass

    def _close_detail(self):
        self._selected_watch_id = None
        try:
            self.query_one("#detail").show_empty()
        except Exception:
            pass

    def action_close_detail(self):
        if isinstance(self.screen, ModalScreen):
            return
        self._close_detail()

    # ---- actions ----

    def action_add_watch(self):
        if isinstance(self.screen, ModalScreen):
            return
        self.push_screen(AddWatchModal())

    def action_delete_watch(self):
        if isinstance(self.screen, ModalScreen):
            return
        if not self._cfg["watches"]:
            self.notify("还没有 watch 可删", severity="warning")
            return
        if self._selected_watch_id:
            # 直接删选中的
            cfg = cfg_mod.load_or_init()
            if cfg_mod.find_watch(cfg, self._selected_watch_id):
                def _check(ok):
                    if not ok:
                        return
                    cfg_mod.remove_watch(cfg_mod.load_or_init(), self._selected_watch_id)
                    self.notify("✓ 已删除 {}".format(self._selected_watch_id))
                    self._close_detail()
                self.push_screen(ConfirmModal(
                    "确认删除", "确定要删除 watch {} 吗？".format(self._selected_watch_id)
                ), _check)
                return
        self.push_screen(DeleteWatchModal(list(self._cfg["watches"])))

    def action_edit_interval(self):
        if isinstance(self.screen, ModalScreen):
            return
        self.push_screen(EditValueModal(
            title="全局检查间隔（秒）",
            key="check_interval", current=str(self._cfg.get("check_interval", 90)),
            validator=lambda v: v.isdigit() and int(v) > 0,
            err_msg="必须是正整数",
        ))

    def action_edit_webhook(self):
        if isinstance(self.screen, ModalScreen):
            return
        self.push_screen(EditValueModal(
            title="Discord Webhook URL（留空=清除）",
            key="discord_webhook",
            current=self._cfg.get("discord_webhook") or "",
            allow_none=True,
        ))

    def action_edit_quiet(self):
        if isinstance(self.screen, ModalScreen):
            return
        self.push_screen(EditValueModal(
            title="静默时段 HH:MM-HH:MM（暂停抓取+推送）",
            key="quiet_window", current=self._cfg.get("quiet_window", "01:00-06:00"),
            validator=cfg_mod._parse_window,
            err_msg="格式错误，应像 01:00-06:00",
        ))

    def action_edit_phone(self):
        if isinstance(self.screen, ModalScreen):
            return
        self.push_screen(EditValueModal(
            title="Phone-only 时段 HH:MM-HH:MM（只推手机，不响铃）",
            key="phone_only_window", current=self._cfg.get("phone_only_window", "06:00-09:00"),
            validator=cfg_mod._parse_window,
            err_msg="格式错误，应像 06:00-09:00",
        ))

    def action_edit_heartbeat(self):
        if isinstance(self.screen, ModalScreen):
            return
        cur = self._cfg.get("heartbeat_interval_sec", 3600)
        cur_label = "{} 秒 ({:.1f} 小时)".format(cur, cur / 3600.0)
        self.push_screen(EditValueModal(
            title="Discord 报告间隔（秒）。例如 1800=30 分钟，3600=1 小时，86400=24 小时",
            key="heartbeat_interval_sec", current=str(cur),
            validator=lambda v: v.isdigit() and int(v) > 0,
            err_msg="必须是正整数",
        ))

    def action_force_check(self):
        if isinstance(self.screen, ModalScreen):
            return
        if self.monitor.force_check_evt:
            self.monitor.force_check_evt.set()
            self.notify("已触发立即检查")
        else:
            self.notify("Monitor 不支持 force_check", severity="error")

    def action_help(self):
        if isinstance(self.screen, ModalScreen):
            return
        self.push_screen(HelpModal())

    # Empty state 上的按钮 + 底部菜单按钮 → 统一走 action_*
    def on_button_pressed(self, event: Button.Pressed):
        # 弹窗里的按钮自己处理（避免双触发 App 层的菜单动作）
        if isinstance(self.screen, ModalScreen):
            return
        bid = event.button.id
        if bid in ("btn-add", "btn-menu-add"):
            self.action_add_watch()
        elif bid == "btn-menu-del":
            self.action_delete_watch()
        elif bid == "btn-menu-check":
            self.action_force_check()
        elif bid == "btn-menu-interval":
            self.action_edit_interval()
        elif bid == "btn-menu-webhook":
            self.action_edit_webhook()
        elif bid == "btn-menu-quiet":
            self.action_edit_quiet()
        elif bid == "btn-menu-phone":
            self.action_edit_phone()
        elif bid == "btn-menu-heartbeat":
            self.action_edit_heartbeat()
        elif bid == "btn-menu-help":
            self.action_help()


# ----------------- Widgets -----------------

class StatsBar(Static):
    """顶部统计条。"""

    def __init__(self, monitor, **kwargs):
        super().__init__(**kwargs)
        self.monitor = monitor

    def render(self):
        cfg = self.monitor.cfg
        mode = cfg_mod.current_mode(cfg["quiet_window"], cfg["phone_only_window"])
        checks = self.monitor.stats["check_count"]
        uptime = self._fmt(time.time() - self.monitor.stats["started_at"])
        fired = sum(len(w.get("fired_cinemas") or []) for w in cfg["watches"])

        mode_label = {"normal": "正常", "phone_only": "只推手机", "quiet": "静默"}.get(mode, mode)
        mode_style = {"normal": "green", "phone_only": "yellow", "quiet": "dim"}.get(mode, "")
        discord = "✓" if cfg.get("discord_webhook") else "✗"
        discord_style = "green" if cfg.get("discord_webhook") else "red"
        # caffeinate 状态：macOS 显示 ✓/✗；其他平台显示 n/a
        caff_state = notify.is_caffeinated()
        if caff_state is None:
            caff_label, caff_style = "n/a", "dim"
        elif caff_state:
            caff_label, caff_style = "✓", "green"
        else:
            caff_label, caff_style = "✗", "red"

        text = Text()
        text.append("⏱  ", "dim")
        text.append("已运行 ", "dim")
        text.append(uptime, "bold")
        text.append("    ")
        text.append("🔍 检查 ", "dim")
        text.append(str(checks), "bold")
        text.append(" 次    ")
        text.append("📡 模式 ", "dim")
        text.append(mode_label, mode_style)
        text.append("    ")
        text.append("📱 Discord ", "dim")
        text.append(discord, discord_style)
        text.append("    ")
        text.append("☕ 防休眠 ", "dim")
        text.append(caff_label, caff_style)
        text.append("    ")
        text.append("🔥 触发 ", "dim")
        text.append(str(fired), "bold magenta")
        return text

    def refresh_stats(self, monitor, cfg):
        # cfg 变化（modal 改了 webhook 等）后强制重画
        self.monitor.cfg = cfg
        self.refresh()

    @staticmethod
    def _fmt(sec):
        sec = int(sec)
        h, rem = divmod(sec, 3600)
        m, s = divmod(rem, 60)
        if h: return "{}h{}m".format(h, m)
        if m: return "{}m{}s".format(m, s)
        return "{}s".format(sec)


class WatchesTable(DataTable):
    """监视项表。"""

    def on_mount(self):
        self.cursor_type = "row"
        self.zebra_stripes = True
        self.add_columns("ID", "电影", "影院", "日期", "状态", "最早", "触发")

    def refresh_from_cfg(self, cfg):
        self.clear()
        for w in cfg["watches"]:
            self.add_row(
                _id_text(w),
                _movie_text(w),
                _cinema_text(cfg, w),
                _date_text(w),
                _status_text(w),
                _earliest_text(w),
                _fired_text(w),
                key=w["id"],
            )

    @on(DataTable.RowHighlighted)
    def on_row_highlighted(self, event: DataTable.RowHighlighted):
        # 高亮行时什么也不做（仅视觉）
        pass

    @on(DataTable.RowSelected)
    def on_row_selected(self, event: DataTable.RowSelected):
        # 单击 / 双击 / Enter → 中部显示该 watch 的详情
        key = event.row_key.value if event.row_key else None
        if not key:
            return
        self.app._show_detail(key)


class EmptyState(Static):
    """无 watch 时的引导。"""

    def render(self):
        return Text.assemble(
            ("🎬  ticket-tracker\n\n", "bold cyan"),
            ("你还没有添加任何监视项\n\n", "bold yellow"),
            ("按 ", "dim"),
            ("a", "bold cyan"),
            (" 添加 watch  ·  按 ", "dim"),
            ("?", "bold cyan"),
            (" 看帮助\n\n", "dim"),
        )


class WatchDetailPanel(Container):
    """中部详情面板：
    - 未选 watch 时显示"请选择一条 watch"提示
    - 选中后显示该 watch 的全部信息 + 编辑 / 启停 / 删除 / 关闭 按钮
    """

    def __init__(self, **kwargs):
        super().__init__(**kwargs)
        self._watch_id = None

    def compose(self) -> ComposeResult:
        yield Label("watch 详情", id="detail-title")
        yield Static(id="detail-info")
        with Grid(id="detail-buttons"):
            yield Button("关闭", id="btn-detail-close")
            yield Button("编辑影院", id="btn-detail-cinemas")
            yield Button("编辑日期", id="btn-detail-dates")
            yield Button("编辑间隔", id="btn-detail-interval", variant="primary")
            yield Button("启停", id="btn-detail-toggle", variant="warning")
            yield Button("删除", id="btn-detail-delete", variant="error")

    def on_mount(self):
        self.show_empty()

    def show_empty(self):
        self._watch_id = None
        self.query_one("#detail-title", Label).update("[dim]watch 详情[/dim]")
        self.query_one("#detail-info", Static).update(
            Text.assemble(
                ("\n", ""),
                ("👆  点击列表中的 watch 查看详情和操作按钮", "dim"),
            )
        )
        # 全部按钮禁用
        for bid in ("btn-detail-cinemas", "btn-detail-dates", "btn-detail-interval",
                    "btn-detail-toggle", "btn-detail-delete", "btn-detail-close"):
            try:
                self.query_one("#{}".format(bid), Button).disabled = True
            except Exception:
                pass

    def update_for_watch(self, w):
        self._watch_id = w["id"]
        cfg = cfg_mod.load_or_init()
        # 标题
        title = "{} {}  ·  {}  ·  状态: {}".format(
            "✓" if w.get("enabled") else "×",
            w["id"],
            w.get("movie_name") or "?",
            _status_text(w).plain)
        self.query_one("#detail-title", Label).update(title)
        # 详情正文 — 紧凑两列排版
        cinemas = _cinema_text(cfg, w)
        dates = _date_text(w)
        interval = "{}s".format(w["interval"]) if w.get("interval") else "默认 ({}s)".format(
            cfg.get("check_interval", 90))
        fired = w.get("fired_cinemas") or []
        fired_str = "、".join(fired) if fired else "—"

        text = Text()
        # 第一行：影院（最占空间，单独一行）
        text.append("影院    : ", "dim")
        text.append("{}\n".format(cinemas))
        # 第二行：日期 | 间隔
        text.append("日期    : ", "dim")
        text.append("{}   ".format(dates))
        text.append("间隔    : ", "dim")
        text.append("{}\n".format(interval))
        # 第三行：启用 | 已触发
        text.append("启用    : ", "dim")
        text.append("{}   ".format("是" if w.get("enabled") else "否"))
        text.append("已触发  : ", "dim")
        text.append("{}\n".format(fired_str))
        # 匹配详情
        payload = w.get("_last_payload") or {}
        matches = payload.get("matches") if isinstance(payload, dict) else []
        if matches:
            text.append("匹配    :\n", "dim")
            for m in matches:
                text.append("  · {}  {} 场, {} ~ {}\n".format(
                    m["cinema_name"], m["show_count"], m["earliest"], m["latest"]))
        self.query_one("#detail-info", Static).update(text)
        # 启用/停用 按钮文案
        toggle_btn = self.query_one("#btn-detail-toggle", Button)
        toggle_btn.label = "停用" if w.get("enabled") else "启用"
        # 启用全部按钮
        for bid in ("btn-detail-cinemas", "btn-detail-dates", "btn-detail-interval",
                    "btn-detail-toggle", "btn-detail-delete", "btn-detail-close"):
            try:
                self.query_one("#{}".format(bid), Button).disabled = False
            except Exception:
                pass

    # ---- 按钮事件 ----

    @on(Button.Pressed, "#btn-detail-close")
    def on_close(self):
        self.app._close_detail()

    @on(Button.Pressed, "#btn-detail-cinemas")
    def on_edit_cinemas(self):
        if not self._watch_id:
            return
        cfg = cfg_mod.load_or_init()
        w = cfg_mod.find_watch(cfg, self._watch_id)
        if not w:
            return
        cur = " ".join(w.get("cinemas") or [])

        def parser(s):
            ids = [c.strip() for c in s.replace(",", " ").split() if c.strip()]
            if not ids:
                raise ValueError("至少需要一个影院 ID")
            return ids

        def _after(_):
            cfg2 = cfg_mod.load_or_init()
            w2 = cfg_mod.find_watch(cfg2, self._watch_id)
            if w2:
                self.update_for_watch(w2)
        self.app.push_screen(EditWatchFieldModal(
            watch_id=self._watch_id, field="cinemas",
            title="编辑影院 ID（空格或逗号分隔）",
            current=cur, parser=parser, allow_blank=False,
        ), _after)

    @on(Button.Pressed, "#btn-detail-dates")
    def on_edit_dates(self):
        if not self._watch_id:
            return
        cfg = cfg_mod.load_or_init()
        w = cfg_mod.find_watch(cfg, self._watch_id)
        if not w:
            return
        cur = " ".join(w.get("dates") or [])

        def parser(s):
            import re
            ds = [d.strip() for d in s.replace(",", " ").split() if d.strip()]
            for d in ds:
                if not re.match(r"^\d{4}-\d{2}-\d{2}$", d):
                    raise ValueError("日期格式错误：{}（应为 YYYY-MM-DD）".format(d))
            return ds if ds else None

        def _after(_):
            cfg2 = cfg_mod.load_or_init()
            w2 = cfg_mod.find_watch(cfg2, self._watch_id)
            if w2:
                self.update_for_watch(w2)
        self.app.push_screen(EditWatchFieldModal(
            watch_id=self._watch_id, field="dates",
            title="编辑限定日期（YYYY-MM-DD，空格分隔；留空=不限）",
            current=cur, parser=parser, allow_blank=True,
        ), _after)

    @on(Button.Pressed, "#btn-detail-interval")
    def on_edit_interval(self):
        if not self._watch_id:
            return
        cfg = cfg_mod.load_or_init()
        w = cfg_mod.find_watch(cfg, self._watch_id)
        if not w:
            return
        cur = str(w.get("interval") or "")

        def parser(s):
            if not s:
                return None
            if not s.isdigit() or int(s) <= 0:
                raise ValueError("间隔必须是正整数")
            return int(s)

        def _after(_):
            cfg2 = cfg_mod.load_or_init()
            w2 = cfg_mod.find_watch(cfg2, self._watch_id)
            if w2:
                self.update_for_watch(w2)
        self.app.push_screen(EditWatchFieldModal(
            watch_id=self._watch_id, field="interval",
            title="编辑独立间隔（秒，留空=用全局）",
            current=cur, parser=parser, allow_blank=True,
        ), _after)

    @on(Button.Pressed, "#btn-detail-toggle")
    def on_toggle(self):
        if not self._watch_id:
            return
        cfg = cfg_mod.load_or_init()
        w = cfg_mod.find_watch(cfg, self._watch_id)
        if not w:
            return
        new_state = not w.get("enabled", True)
        cfg_mod.set_watch_field(cfg, self._watch_id, enabled=new_state)
        self.app.notify("✓ {} → {}".format(self._watch_id, "启用" if new_state else "停用"))
        cfg2 = cfg_mod.load_or_init()
        w2 = cfg_mod.find_watch(cfg2, self._watch_id)
        if w2:
            self.update_for_watch(w2)

    @on(Button.Pressed, "#btn-detail-delete")
    def on_delete(self):
        if not self._watch_id:
            return
        wid = self._watch_id
        def _check(ok):
            if not ok:
                return
            cfg_mod.remove_watch(cfg_mod.load_or_init(), wid)
            self.app.notify("✓ 已删除 {}".format(wid))
            self.app._close_detail()
        self.app.push_screen(ConfirmModal(
            "确认删除", "确定要删除 watch {} 吗？".format(wid)
        ), _check)


class ActionMenu(Grid):
    """底部按钮菜单（替代 Footer）。

    用 Grid 流式布局：CSS `grid-size: 3` 让 9 个按钮分 3 行（3+3+3），
    宽窗口按钮宽度大、窄窗口按钮按列宽自适应缩放但始终可读。
    所有按钮均带 `[X]` 前缀标出对应键盘快捷键。
    """

    def compose(self) -> ComposeResult:
        yield Button("[A] 添加", id="btn-menu-add", variant="success")
        yield Button("[D] 删除", id="btn-menu-del", variant="error")
        yield Button("[R] 立即检查", id="btn-menu-check", variant="warning")
        yield Button("[I] 检查间隔", id="btn-menu-interval")
        yield Button("[W] Discord webhook", id="btn-menu-webhook")
        yield Button("[Q] 静默时段", id="btn-menu-quiet")
        yield Button("[P] 只推手机", id="btn-menu-phone")
        yield Button("[H] 报告间隔", id="btn-menu-heartbeat")
        yield Button("[?] 帮助", id="btn-menu-help", variant="primary")


# ----------------- Modal Screens -----------------

HELP_MD = """\
# ticket-tracker 热键

| 键 | 作用 |
|---|---|
| `a` | 添加 watch（电影 ID 行有「选电影」按钮，影院 ID 行有「收藏」按钮） |
| `d` | 删除 watch（若中部已选 watch，直接删它） |
| `i` | 改全局检查间隔 |
| `w` | 改 Discord webhook |
| `q` | 改静默时段 |
| `p` | 改 phone-only 时段 |
| `h` | 改 Discord 报告间隔（默认 3600 秒 = 1 小时；可设 1800=30 分钟等） |
| `r` | 立即检查一轮 |
| `Esc` | 收起中部详情面板 |
| `?` | 显示本帮助 |
| `Ctrl+C` | 退出 |

## 鼠标

- 表格行：单击 → 中部显示详情；`Esc` 或点「关闭」收起
- 详情面板：6 个按钮（编辑影院 / 日期 / 间隔 / 启停 / 删除 / 关闭）
- 弹窗按钮：直接点
- 底部按钮菜单：9 个动作分 3 行排列，直接点（按键 `[X]` 即对应键盘快捷键）

## 小贴士

- 想编辑已添加的 watch？点列表中对应行 → 中部出现详情 → 点「编辑影院/日期/间隔」改字段
- 启停 watch？同样选中后点「启停」按钮
- 找不到电影 ID？在「添加」弹窗里点「选电影」，从猫眼热映 / 即将上映列表里挑
- 不知道影院 ID？同样在「添加」弹窗里点「收藏」，在弹出的影院收藏夹里选；没收藏的可只填 ID 拉取名称并加入
- 想从其他终端加 watch：`tt watch add <电影 ID> -c <影院 ID>`
"""


class HelpModal(ModalScreen):
    BINDINGS = [Binding("escape,q,?", "dismiss", "关闭", show=False)]

    def compose(self) -> ComposeResult:
        with VerticalScroll(classes="modal-box"):
            yield Label("ticket-tracker 帮助", classes="modal-title")
            yield Markdown(HELP_MD)
            with Horizontal(classes="btn-row"):
                yield Button("关闭 (Esc)", id="btn-close")

    @on(Button.Pressed, "#btn-close")
    def on_close(self):
        self.dismiss()


class ConfirmModal(ModalScreen):
    """通用确认。dismiss(True/False)。"""

    BINDINGS = [Binding("escape", "dismiss_false", "取消", show=False),
                Binding("enter", "dismiss_true", "确定", show=False)]

    def __init__(self, title, message):
        super().__init__()
        self.title_text = title
        self.message = message

    def compose(self) -> ComposeResult:
        with VerticalScroll(classes="modal-box"):
            yield Label(self.title_text, classes="modal-title")
            yield Label(self.message)
            with Horizontal(classes="btn-row"):
                yield Button("取消", id="btn-cancel", variant="default")
                yield Button("确定", id="btn-ok", variant="error")

    @on(Button.Pressed, "#btn-ok")
    def on_ok(self):
        self.dismiss(True)

    @on(Button.Pressed, "#btn-cancel")
    def on_cancel(self):
        self.dismiss(False)

    def action_dismiss_true(self):
        self.dismiss(True)

    def action_dismiss_false(self):
        self.dismiss(False)


class AddWatchModal(ModalScreen):
    BINDINGS = [Binding("escape", "dismiss", "取消", show=False)]

    def __init__(self, prefill=None):
        super().__init__()
        # prefill 由 SearchMovieModal 的「按 ID 加」传过来：{"movie_id": int, "movie_name": str}
        self.prefill = prefill or {}

    def compose(self) -> ComposeResult:
        with VerticalScroll(classes="modal-box"):
            yield Label("添加 watch", classes="modal-title")
            yield Label("电影 ID（整数，必填）")
            with Horizontal(classes="inline-form"):
                yield Input(id="in-movie", placeholder="例如 1490607",
                            value=str(self.prefill.get("movie_id", "")))
                yield Button("选电影", id="btn-pick-movie", variant="primary")
            yield Label("影院 ID（必填，多个用空格或逗号分隔）")
            with Horizontal(classes="inline-form"):
                yield Input(id="in-cinemas", placeholder="例如 37534  或  37534,2127")
                yield Button("收藏", id="btn-pick-cinema", variant="primary")
            yield Label("限定开售日期（选填，YYYY-MM-DD，多个用空格分隔）")
            yield Input(id="in-dates", placeholder="例如 2026-07-29  或留空=不限")
            yield Label("电影名（选填，留空自动从猫眼获取）")
            yield Input(id="in-name", placeholder="例如 蜘蛛侠：崭新之日",
                        value=self.prefill.get("movie_name", "") or "")
            yield Label("独立间隔（秒，选填，留空=全局）")
            yield Input(id="in-interval", placeholder="例如 30")
            with Horizontal(classes="btn-row"):
                yield Button("取消", id="btn-cancel")
                yield Button("添加", id="btn-add", variant="success")

    def on_mount(self):
        # 如果有预填的电影 ID，焦点跳到影院输入框
        if self.prefill.get("movie_id"):
            try:
                self.query_one("#in-cinemas", Input).focus()
            except Exception:
                pass

    @on(Button.Pressed, "#btn-cancel")
    def on_cancel(self):
        self.dismiss(None)

    @on(Button.Pressed, "#btn-pick-movie")
    def on_pick_movie(self):
        def _on_picked(result):
            if not isinstance(result, dict) or not result.get("movie_id"):
                return
            self.query_one("#in-movie", Input).value = str(result["movie_id"])
            self.query_one("#in-name", Input).value = result.get("movie_name", "") or ""
        self.app.push_screen(SearchMovieModal(), _on_picked)

    @on(Button.Pressed, "#btn-pick-cinema")
    def on_pick_cinema(self):
        def _on_picked(result):
            if not isinstance(result, dict) or not result.get("cinema_ids"):
                return
            cur = self.query_one("#in-cinemas", Input).value.strip()
            parts = ([c.strip() for c in cur.replace(",", " ").split() if c.strip()]
                     if cur else [])
            seen = set(parts)
            for cid in result["cinema_ids"]:
                if cid not in seen:
                    parts.append(str(cid))
                    seen.add(str(cid))
            self.query_one("#in-cinemas", Input).value = " ".join(parts)
        self.app.push_screen(CinemaCollectionModal(), _on_picked)

    @on(Button.Pressed, "#btn-add")
    def on_add(self):
        movie_raw = self.query_one("#in-movie", Input).value.strip()
        cinemas_raw = self.query_one("#in-cinemas", Input).value.strip()
        dates_raw = self.query_one("#in-dates", Input).value.strip()
        name = self.query_one("#in-name", Input).value.strip() or None
        interval_raw = self.query_one("#in-interval", Input).value.strip()

        if not movie_raw or not movie_raw.lstrip("-").isdigit():
            self.app.notify("电影 ID 必须是整数", severity="error")
            return
        movie_id = int(movie_raw)

        cinemas = [c.strip() for c in cinemas_raw.replace(",", " ").split() if c.strip()]
        if not cinemas:
            self.app.notify("至少需要一个影院", severity="error")
            return

        dates = None
        if dates_raw:
            dates = [d.strip() for d in dates_raw.replace(",", " ").split() if d.strip()]

        interval = None
        if interval_raw:
            if not interval_raw.isdigit() or int(interval_raw) <= 0:
                self.app.notify("间隔必须是正整数", severity="error")
                return
            interval = int(interval_raw)

        cfg = cfg_mod.load_or_init()
        if not name:
            try:
                name = maoyan.fetch_movie_name(movie_id)
            except Exception:
                name = None
        wid = cfg_mod.add_watch(cfg, movie_id, cinemas, dates=dates,
                                name=name, interval=interval)
        self.app.notify("✓ 已添加 {}".format(wid))
        self.dismiss(wid)


class DeleteWatchModal(ModalScreen):
    BINDINGS = [Binding("escape", "dismiss", "取消", show=False)]

    def __init__(self, watches):
        super().__init__()
        self.watches = watches

    def compose(self) -> ComposeResult:
        with VerticalScroll(classes="modal-box"):
            yield Label("删除 watch", classes="modal-title")
            yield Label("选择要删除的 watch（点列表项 / 方向键 + 回车）：")
            items = [
                ListItem(Label("[{}] {} ({})".format(
                    "✓" if w.get("enabled") else "×",
                    w["id"],
                    w.get("movie_name") or w["movie_id"])),
                    id="li-{}".format(w["id"]))
                for w in self.watches
            ]
            yield ListView(*items, id="lv-watches")
            with Horizontal(classes="btn-row"):
                yield Button("取消", id="btn-cancel")
                yield Button("删除选中", id="btn-del", variant="error")

    @on(Button.Pressed, "#btn-cancel")
    def on_cancel(self):
        self.dismiss(None)

    @on(Button.Pressed, "#btn-del")
    def on_del(self):
        self._do_delete()

    @on(ListView.Selected, "#lv-watches")
    def on_selected(self, event: ListView.Selected):
        # 双击 list item 也触发删除
        self._do_delete()

    def _do_delete(self):
        lv = self.query_one("#lv-watches", ListView)
        idx = lv.index
        if idx is None or idx < 0 or idx >= len(self.watches):
            self.app.notify("请先选一个", severity="warning")
            return
        wid = self.watches[idx]["id"]
        cfg = cfg_mod.load_or_init()
        cfg_mod.remove_watch(cfg, wid)
        self.app.notify("✓ 已删除 {}".format(wid))
        self.dismiss(wid)


class EditValueModal(ModalScreen):
    """通用：改一个配置项（全局配置）。"""

    BINDINGS = [Binding("escape", "dismiss", "取消", show=False)]

    def __init__(self, title, key, current, validator=None, err_msg="", allow_none=False):
        super().__init__()
        self.title_text = title
        self.key = key
        self.current = current
        self.validator = validator
        self.err_msg = err_msg
        self.allow_none = allow_none

    def compose(self) -> ComposeResult:
        with VerticalScroll(classes="modal-box"):
            yield Label(self.title_text, classes="modal-title")
            yield Input(value=self.current, id="in-val")
            with Horizontal(classes="btn-row"):
                yield Button("取消", id="btn-cancel")
                yield Button("保存", id="btn-ok", variant="success")

    @on(Button.Pressed, "#btn-cancel")
    def on_cancel(self):
        self.dismiss(None)

    @on(Button.Pressed, "#btn-ok")
    def on_ok(self):
        v = self.query_one("#in-val", Input).value.strip()
        if self.allow_none and not v:
            v = None
        elif self.validator:
            try:
                self.validator(v)
            except Exception as e:
                self.app.notify(self.err_msg or str(e), severity="error")
                return
        cfg = cfg_mod.load_or_init()
        cfg[self.key] = int(v) if self.key in ("check_interval", "alert_duration_sec",
                                              "heartbeat_interval_sec") and v else v
        cfg_mod.save(cfg)
        self.app.notify("✓ {} 已更新".format(self.key))
        self.dismiss(v)


class EditWatchFieldModal(ModalScreen):
    """通用：编辑 watch 的某个字段（cinemas / dates / interval）。"""

    BINDINGS = [Binding("escape", "dismiss", "取消", show=False)]

    def __init__(self, watch_id, field, title, current, parser,
                 allow_blank=False, err_msg="格式错误"):
        super().__init__()
        self.watch_id = watch_id
        self.field = field
        self.title_text = title
        self.current = current
        self.parser = parser
        self.allow_blank = allow_blank
        self.err_msg = err_msg

    def compose(self) -> ComposeResult:
        with VerticalScroll(classes="modal-box"):
            yield Label(self.title_text, classes="modal-title")
            yield Input(value=self.current, id="in-val")
            with Horizontal(classes="btn-row"):
                yield Button("取消", id="btn-cancel")
                yield Button("保存", id="btn-ok", variant="success")

    @on(Button.Pressed, "#btn-cancel")
    def on_cancel(self):
        self.dismiss(None)

    @on(Button.Pressed, "#btn-ok")
    def on_ok(self):
        v = self.query_one("#in-val", Input).value.strip()
        if not v and self.allow_blank:
            parsed = None
        else:
            try:
                parsed = self.parser(v)
            except ValueError as e:
                self.app.notify(str(e) or self.err_msg, severity="error")
                return
        cfg = cfg_mod.load_or_init()
        cfg_mod.set_watch_field(cfg, self.watch_id, **{self.field: parsed})
        self.app.notify("✓ {} 已更新".format(self.field))
        self.dismiss(parsed)


class CinemaCollectionModal(ModalScreen):
    """影院收藏夹：DataTable 列已有影院，点行直接填入；底部表单只填 ID → 自动拉取名称并加入。"""

    BINDINGS = [Binding("escape", "dismiss", "关闭", show=False)]

    def compose(self) -> ComposeResult:
        with VerticalScroll(classes="modal-box"):
            yield Label("影院收藏夹", classes="modal-title")
            yield Label("点行直接填入；下面表单可添加新影院（只需输 ID，名称自动从猫眼拉取）。",
                        classes="hint")
            yield DataTable(id="tbl-cinemas", cursor_type="row", zebra_stripes=True)
            yield Label("添加新影院", classes="modal-subtitle")
            yield Label("去 maoyan.com/cinema/<ID> 看 URL 末的数字 → 填这里", classes="hint")
            with Horizontal(classes="inline-form"):
                yield Input(placeholder="例如 37534", id="in-cinema-id")
                yield Button("拉取并添加", id="btn-fetch-add", variant="primary")
            with Horizontal(classes="btn-row"):
                yield Button("关闭", id="btn-close")

    def on_mount(self):
        tbl = self.query_one("#tbl-cinemas", DataTable)
        tbl.add_columns("ID", "名称")
        self._refresh_table()

    def _refresh_table(self):
        cfg = cfg_mod.load_or_init()
        tbl = self.query_one("#tbl-cinemas", DataTable)
        tbl.clear()
        for c in cfg["cinemas"]:
            marker = "★ " if c.get("builtin") else ""
            tbl.add_row(c["id"], "{}{}".format(marker, c["name"]), key=c["id"])

    @on(Button.Pressed, "#btn-close")
    def on_close(self):
        self.dismiss(None)

    @on(DataTable.RowSelected, "#tbl-cinemas")
    def on_row_selected(self, event: DataTable.RowSelected):
        cid = str(event.row_key.value) if event.row_key else None
        if not cid:
            return
        self.dismiss({"cinema_ids": [cid]})

    @on(Button.Pressed, "#btn-fetch-add")
    @on(Input.Submitted, "#in-cinema-id")
    def on_fetch_add(self):
        raw = self.query_one("#in-cinema-id", Input).value.strip()
        if not raw or not raw.isdigit():
            self.app.notify("影院 ID 必须是整数", severity="error")
            return
        cfg = cfg_mod.load_or_init()
        # 已在收藏 → 直接 dismiss 复用
        if cfg_mod.find_cinema(cfg, raw):
            self.app.notify("该影院已在收藏夹", severity="information")
            self.dismiss({"cinema_ids": [str(raw)]})
            return
        # 拉名称
        try:
            data = maoyan.fetch_cinema(raw)
            cn = data["cinema_name"]
        except Exception as e:
            self.app.notify("拉取失败：{}".format(e), severity="error")
            return
        if cn == "影城 {}".format(raw) or not cn:
            self.app.notify("猫眼没拿到名称（ID 可能无效）", severity="warning")
            return
        cfg_mod.add_cinema(cfg, raw, name=cn)
        self._refresh_table()
        self.app.notify("✓ 已添加 {} ({})".format(cn, raw))
        self.dismiss({"cinema_ids": [str(raw)]})


class SearchMovieModal(ModalScreen):
    """猫眼在映 / 即将上映电影列表（每类最多约 20 条）。

    猫眼没开放关键词搜索 API；只能从这两个列表里挑。
    点行 → dismiss({"movie_id": ..., "movie_name": ...}) 给调用方填字段。
    """

    BINDINGS = [Binding("escape", "dismiss", "关闭", show=False)]

    def compose(self) -> ComposeResult:
        with VerticalScroll(classes="modal-box"):
            yield Label("猫眼电影列表", classes="modal-title")
            yield Label("猫眼没开放关键词搜索；只能从热映 / 即将上映里挑。",
                        classes="hint")
            with Horizontal(classes="btn-row"):
                yield Button("关闭", id="btn-close")
                yield Button("正在热映", id="btn-hot", variant="primary")
                yield Button("即将上映", id="btn-upcoming", variant="warning")
            yield DataTable(id="tbl-res", cursor_type="row", zebra_stripes=True)

    def on_mount(self):
        tbl = self.query_one("#tbl-res", DataTable)
        tbl.add_columns("电影 ID", "片名")

    @on(Button.Pressed, "#btn-close")
    def on_close(self):
        self.dismiss()

    @on(Button.Pressed, "#btn-hot")
    def on_hot(self):
        self._fetch(1)

    @on(Button.Pressed, "#btn-upcoming")
    def on_upcoming(self):
        self._fetch(2)

    def _fetch(self, show_type):
        try:
            rows = maoyan.fetch_films_list(show_type)
        except Exception as e:
            self.app.notify("拉取失败: {}".format(e), severity="error")
            return
        tbl = self.query_one("#tbl-res", DataTable)
        tbl.clear()
        for r in rows:
            tbl.add_row(r["id"], r["name"], key=r["id"])
        label = {1: "热映", 2: "即将上映"}.get(show_type, "")
        if not rows:
            self.app.notify("{} 无数据".format(label), severity="warning")
        else:
            self.app.notify("✓ {} {} 条".format(label, len(rows)))

    @on(DataTable.RowSelected, "#tbl-res")
    def on_row_selected(self, event: DataTable.RowSelected):
        tbl = self.query_one("#tbl-res", DataTable)
        try:
            cells = tbl.get_row(event.row_key)
            movie_id = int(str(cells[0]))
            name = str(cells[1])
        except Exception:
            return
        self.dismiss({"movie_id": movie_id, "movie_name": name})


# ----------------- 表格辅助函数 -----------------

def _id_text(w):
    t = Text()
    t.append(w["id"], "dim")
    t.append(" ")
    t.append(("✓" if w.get("enabled") else "×"),
             "green" if w.get("enabled") else "dim")
    return t


def _movie_text(w):
    return "{} ({})".format(w.get("movie_name") or "?", w["movie_id"])


def _cinema_text(cfg, w):
    parts = []
    for cid in w.get("cinemas") or []:
        cn = next((c["name"] for c in cfg["cinemas"] if c["id"] == str(cid)), None)
        if cn:
            parts.append("{} ({})".format(cn, cid))
        else:
            parts.append("?({})".format(cid))
    return " + ".join(parts) if parts else "[dim]无影院[/dim]"


def _date_text(w):
    d = w.get("dates")
    return "不限" if not d else "、".join(d)


def _status_text(w):
    status = w.get("_last_status", "—")
    if status == "open":
        return Text("已开售 ✓", style="bold green")
    if status == "not_listed":
        return Text("未上架", style="dim")
    if status == "no_shows":
        return Text("排片中", style="yellow")
    if status == "error":
        return Text("出错", style="red")
    return Text("待查", style="dim")


def _earliest_text(w):
    payload = w.get("_last_payload") or {}
    matches = payload.get("matches") if isinstance(payload, dict) else []
    if matches:
        return matches[0]["earliest"]
    return "—"


def _fired_text(w):
    fired = w.get("fired_cinemas") or []
    if not fired:
        return Text("—", style="dim")
    n = len(fired)
    total = len(w.get("cinemas") or [])
    return Text("{}/{}".format(n, total), style="magenta")