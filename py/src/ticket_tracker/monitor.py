"""监测循环：每条 watch = (movie_id, [cinema_ids], [dates (可选)]).
对每个 watch: 跨其所有影院抓一次，找该 movie，按 dates 过滤。"""

import logging
import signal
import sys
import threading
import time
from collections import deque
from datetime import datetime
from threading import Event

from . import config as cfg_mod
from . import maoyan, notify
from .paths import log_file

log = logging.getLogger("ticket_tracker.monitor")


# watch.status → 简短图标 + 文案（用于 Discord 心跳的单行摘要）
_STATUS_ICON = {
    "open": "🟢 已开售",
    "not_listed": "⚫ 未上架",
    "no_shows": "🟡 排片中",
    "error": "🔴 出错",
}


# ----------------- 单个 watch 检查 -----------------

def check_watch(watch, cinema_cache):
    """返回 status + info:
      status ∈ {"open", "not_listed", "no_shows", "error"}
      info: {"name", "matches": [{cinema_id, cinema_name, show_count,
                                   earliest, latest}], "raw": ...}
    """
    movie_id = watch["movie_id"]
    movie_name = watch.get("movie_name")
    cinema_ids = watch.get("cinemas") or []
    if not cinema_ids:
        return "error", {"error": "该 watch 未指定影院"}

    matches = []          # 该 watch 在各影院的开售情况
    errors = []
    any_listed = False
    cinema_names = {}
    show_dates = {}       # 每个 cinema 该 movie 实际已排到的日期（即便被 dates 过滤掉也保留）
    for cid in cinema_ids:
        try:
            if cid not in cinema_cache:
                cinema_cache[cid] = maoyan.fetch_cinema(cid)
            payload = cinema_cache[cid]
        except RuntimeError as e:
            errors.append((cid, str(e)))
            continue
        cinema_names[cid] = payload["cinema_name"]
        movie = maoyan.find_movie(payload, movie_id,
                                  [movie_name] if movie_name else [])
        if not movie:
            continue
        any_listed = True
        all_dates = maoyan.movie_dates(movie)
        show_dates[cid] = all_dates
        all_shows = sum(len(s.get("plist") or []) for s in (movie.get("shows") or []))
        show_count = movie.get("showCount") or all_shows
        # 日期过滤
        dates = all_dates
        if watch.get("dates"):
            allowed = set(watch["dates"])
            dates = [d for d in dates if d in allowed]
            # 精确数日期内的场次
            show_count = sum(
                len([p for p in (s.get("plist") or []) if p.get("dt") in allowed])
                for s in (movie.get("shows") or []))
        if not dates or show_count <= 0:
            continue
        matches.append({
            "cinema_id": cid,
            "cinema_name": payload["cinema_name"],
            "show_count": show_count,
            "earliest": dates[0],
            "latest": dates[-1],
        })

    base_info = {
        "name": movie_name or str(movie_id),
        "matches": matches,
        "cinema_names": cinema_names,
        "show_dates": show_dates,
        "errors": errors,
    }
    if not any_listed:
        return "not_listed", base_info
    if not matches:
        return "no_shows", base_info
    return "open", base_info


# ----------------- Monitor 类 -----------------

class Monitor:
    """常驻监测器 + TUI 数据源。"""

    def __init__(self, watch_filter=None, force_check_evt=None):
        self.cfg = cfg_mod.load_or_init()
        if watch_filter:
            ids = set(watch_filter)
            self.cfg["watches"] = [w for w in self.cfg["watches"] if w["id"] in ids]
        self.events = deque(maxlen=64)
        self.stats = {
            "started_at": time.time(),
            "check_count": 0,
            "per_cinema": {},
        }
        self._stop_evt = Event()
        # TUI 可选地注入一个 Event；set() 后本轮 _tick 立刻触发一次（不等 sleep）
        self.force_check_evt = force_check_evt

    def push_event(self, line):
        self.events.appendleft("[{}] {}".format(
            datetime.now().strftime("%H:%M:%S"), line))

    def stop(self):
        self._stop_evt.set()

    def run(self):
        # 启动通知
        n = len(self.cfg["watches"])
        if not self.cfg["watches"]:
            msg = "无监视项。请 tt watch add 或在 TUI 里按 a 添加。"
        else:
            msg = "监视项 {} 条｜间隔 {}s｜时段 quiet={} phone_only={}".format(
                n, self.cfg.get("check_interval", 90),
                self.cfg.get("quiet_window"), self.cfg.get("phone_only_window"))
        notify.notify_discord(self.cfg.get("discord_webhook"),
                              "ticket-tracker 已启动 ✅", msg)

        # 信号处理只能在主线程设置（TUI 模式下 monitor 跑在 worker 线程）
        if threading.current_thread() is threading.main_thread():
            def _graceful(signum, frame):
                self.push_event("收到停止信号，准备退出…")
                self.stop()
            signal.signal(signal.SIGTERM, _graceful)
            signal.signal(signal.SIGINT, _graceful)

        last_heartbeat = time.time()
        log.info("Monitor 主循环启动。")
        while not self._stop_evt.is_set():
            mode = cfg_mod.current_mode(
                self.cfg["quiet_window"], self.cfg["phone_only_window"])
            if mode == "quiet":
                self.push_event("进入静默时段：暂停抓取/推送")
                if self._wait_with_stop(60):
                    break
                continue

            interval = self._effective_interval()
            # 手动触发的强制检查：清旗后立即 _tick，跳过 sleep
            force = bool(self.force_check_evt and self.force_check_evt.is_set())
            if force:
                self.force_check_evt.clear()
                self.push_event("· 手动触发一轮检查…")
            if self._tick(mode, force=force):
                self.stats["check_count"] += 1
            if force:
                # 强制检查完直接进入下一轮主循环，不等 sleep
                continue

            if mode != "quiet" and \
                    (time.time() - last_heartbeat) >= self.cfg.get(
                        "heartbeat_interval_sec", 3600):
                if any(w.get("enabled") for w in self.cfg["watches"]):
                    self._send_heartbeat()
                # 无论是否发送，last_heartbeat 都推进——避免 active=0 期间累积后"补发"
                last_heartbeat = time.time()

            if self._wait_with_stop(interval):
                break

        uptime = self._fmt_uptime(time.time() - self.stats["started_at"])
        log.info("Monitor 已停止，累计检查 %s 次。", self.stats["check_count"])
        notify.notify_discord(
            self.cfg.get("discord_webhook"),
            "ticket-tracker 已停止 🛑",
            "运行时长 {}｜累计检查 {} 次".format(
                uptime, self.stats["check_count"]))

    def _effective_interval(self):
        """loop 的 tick 间隔：取所有启用 watch 的 interval 最小值，缺省用全局 check_interval。
        返回秒数。"""
        enabled = [w for w in self.cfg["watches"] if w.get("enabled")]
        if not enabled:
            return self.cfg.get("check_interval", 90)
        default = self.cfg.get("check_interval", 90)
        ints = [w.get("interval") or default for w in enabled]
        return min(ints)

    def _tick(self, mode, force=False):
        """执行一轮检查。每条 watch 按其独立 interval 节流；force=True 时跳过节流。"""
        cinema_cache = {}
        any_done = False
        now = time.time()
        for watch in self.cfg["watches"]:
            if not watch.get("enabled"):
                continue
            wid = watch["id"]
            # 独立 interval 节流：未到下次检查时间就跳过（force 模式全跑）
            w_interval = watch.get("interval")
            if not force and w_interval:
                last_map = self.stats.setdefault("per_watch_last", {})
                last = last_map.get(wid, 0)
                if (now - last) < w_interval:
                    continue
                last_map[wid] = now
            computer_alarm = (mode == "normal")
            status, info = check_watch(watch, cinema_cache)
            self.stats["per_cinema"]["|".join(watch.get("cinemas") or [])] = {
                "last_status": status,
                "last_check": datetime.now().strftime("%H:%M:%S"),
            }
            any_done = any_done or (status != "error")
            label = "{}({})".format(info.get("name") or "?", watch["movie_id"])

            if status == "open":
                lines = []
                for m in info["matches"]:
                    lines.append("{}({}场, {} 起)".format(
                        m["cinema_name"], m["show_count"], m["earliest"]))
                self.push_event("✓ {} 预售开启！{}".format(label, " / ".join(lines)))
                # 触发条件：每个 (watch, cinema) 对首次开售都报
                for m in info["matches"]:
                    cid = m["cinema_id"]
                    fired_on = watch.get("fired_cinemas", [])
                    if str(cid) in fired_on:
                        continue
                    buy_url = "https://www.maoyan.com/cinema/{}".format(cid)
                    alert = "{}｜{}：{} 场｜{} 至 {}".format(
                        info["name"], m["cinema_name"],
                        m["show_count"], m["earliest"], m["latest"])
                    notify.notify_discord(
                        self.cfg.get("discord_webhook"),
                        "预售开启 🎬", alert, url=buy_url)
                    if computer_alarm:
                        notify.notify_macos(
                            "预售开启 🎬", alert,
                            sound=True, open_url=buy_url,
                            duration=self.cfg.get("alert_duration_sec", 60))
                    cfg_mod.mark_presale_fired(self.cfg, wid, cid)
                    watch.setdefault("fired_cinemas", []).append(str(cid))
                # 该 watch 所有 cinema 都已触发 → 自动停用，不再浪费 API
                all_cinemas = {str(c) for c in watch.get("cinemas") or []}
                fired_set = {str(c) for c in watch.get("fired_cinemas", [])}
                if all_cinemas and all_cinemas <= fired_set:
                    watch["enabled"] = False
                    self.push_event(
                        "· {} 全 {} 个影院已触发，自动停用".format(
                            watch["id"], len(all_cinemas)))
                    label_disable = "{}({})".format(
                        info.get("name") or "?", watch["movie_id"])
                    self.push_event("✓ {} 已停用（任务完成）".format(label_disable))
            elif status == "not_listed":
                self.push_event("· {} 影院列表中尚未出现".format(label))
            elif status == "no_shows":
                self.push_event("· {} 列表有但未开售符合条件的场次".format(label))
            else:
                errs = "; ".join("{}: {}".format(c, e) for c, e in info.get("errors", []))
                self.push_event("✗ {} 检查出错: {}".format(label, errs or "未知"))

            watch["_last_status"] = status
            watch["_last_payload"] = info
        cfg_mod.save(self.cfg)
        return any_done

    def _send_heartbeat(self):
        """每小时向 Discord 发运行报告。

        - title 区分「例行正常」vs「检测到开售」一眼分辨
        - body 第一行基础信息（uptime / 检查次数 / 模式 / 活跃条数）
        - 之后每条 enabled watch 一行：状态 + 电影名 + ID + 影院 + 最早/最晚场次
        """
        cfg = self.cfg
        enabled = [w for w in cfg["watches"] if w.get("enabled")]
        has_open = any(w.get("_last_status") == "open" for w in enabled)
        title = "🎬 检测到开售" if has_open else "✅ 例行报告（未开售）"

        uptime = self._fmt_uptime(time.time() - self.stats["started_at"])
        mode = cfg_mod.current_mode(cfg["quiet_window"], cfg["phone_only_window"])
        mode_label = {"normal": "正常", "phone_only": "只推手机", "quiet": "静默"}.get(mode, mode)

        lines = ["⏱ {}｜🔍 {} 次｜📡 {}｜活跃 {} 条".format(
            uptime, self.stats["check_count"], mode_label, len(enabled))]
        if not enabled:
            lines.append("（无启用中的监视项）")
        for w in enabled:
            lines.append(self._watch_summary_line(w))

        notify.notify_discord(cfg.get("discord_webhook"), title, "\n".join(lines))

    def _watch_summary_line(self, w):
        """单条 watch 在 Discord 报告里的单行摘要。

        关键改进：
        - 影院用名称（cat 不到再用 ID）
        - 未开售时给真实日期信息（vs watch.dates 限制 / 已排到的最早日期）
        """
        icon = _STATUS_ICON.get(w.get("_last_status"), "⚪ 待查")
        name = w.get("movie_name") or "movie_{}".format(w["movie_id"])
        cinema_ids = [str(c) for c in (w.get("cinemas") or [])]
        payload = w.get("_last_payload") or {}
        matches = payload.get("matches") if isinstance(payload, dict) else []
        cinema_names = payload.get("cinema_names") or {}
        show_dates = payload.get("show_dates") or {}
        allowed = list(w.get("dates") or [])

        # 影院名 (ID) 列表
        cinema_label = " + ".join(
            "{} ({})".format(cinema_names.get(cid, "?"), cid)
            for cid in cinema_ids
        ) or "?"

        if matches:
            m = matches[0]
            earliest = m.get("earliest", "?")
            latest = m.get("latest", "?")
            show = m.get("show_count", 0)
            cinema = m.get("cinema_name", "?")
            if len(matches) > 1:
                detail = "{} 等 {} 家 · {} 场 · {}~{}".format(
                    cinema, len(matches), show, earliest, latest)
            else:
                detail = "{} · {} 场 · {}~{}".format(cinema, show, earliest, latest)
            return "{} {} ({}) [{}] {}".format(icon, name, w["id"], cinema_label, detail)

        # 未开售分支：列每个影院实际已排到 / 没排片的情况
        status = w.get("_last_status")
        if status == "not_listed":
            # 影院列表里就没这个电影
            detail = "影院列表中暂无此电影"
            return "{} {} ({}) [{}] {}".format(icon, name, w["id"], cinema_label, detail)

        # no_shows：影院列表有，但 dates 过滤后没匹配 / 全部未排片
        if not cinema_ids:
            detail = "尚未排片"
        else:
            allowed_set = set(allowed)
            parts = []
            single = (len(cinema_ids) == 1)
            for cid in cinema_ids:
                cn = cinema_names.get(cid, "?")
                ds = show_dates.get(cid) or []
                prefix = (cn if not single else "")
                if not ds:
                    parts.append(("{}已上架未排片" if prefix else "已上架未排片").format(prefix))
                elif not allowed:
                    parts.append(
                        ("{} 已排 {} 天，最早 {}, 但未触发开售".format(prefix, len(ds), ds[0])
                         if prefix else "已排 {} 天，最早 {}, 但未触发开售".format(len(ds), ds[0])))
                else:
                    overlap = set(ds) & allowed_set
                    if overlap:
                        oe = sorted(overlap)[0]
                        parts.append(
                            ("{} 限定内已有 {} 等 {} 天".format(prefix, oe, len(overlap))
                             if prefix else "限定内已有 {} 等 {} 天".format(oe, len(overlap))))
                    else:
                        # 最常见：限定日期没匹配；告诉用户实际最早开售日
                        parts.append(
                            ("{} 限定 {} 无场次；最早开售 {}".format(
                                prefix, "/".join(allowed), ds[0])
                             if prefix else "限定 {} 无场次；最早开售 {}".format(
                                 "/".join(allowed), ds[0])))
            detail = "；".join(parts)
        return "{} {} ({}) [{}] {}".format(icon, name, w["id"], cinema_label, detail)

    def _fmt_uptime(self, sec):
        sec = int(sec)
        h, rem = divmod(sec, 3600)
        m, s = divmod(rem, 60)
        if h: return "{}小时{}分".format(h, m)
        if m: return "{}分{}秒".format(m, s)
        return "{}秒"

    def _wait_with_stop(self, sec):
        return self._stop_evt.wait(timeout=sec)
