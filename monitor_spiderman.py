#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
监测「上海前滩太古里 MOViE MOViE 影城」是否开启《蜘蛛侠：崭新之日》预售。

原理：
  猫眼移动端接口 https://m.maoyan.com/ajax/cinemaDetail?cinemaId=<影城ID>
  返回该影城当前所有【已开预售 / 已排片】的电影列表(showData.movies[])。
  只要目标电影出现在列表中且 showCount>0（有可购票场次），即代表预售已开启。

用法：
  python3 monitor_spiderman.py            # 单次检查（配合 cron/launchd）
  python3 monitor_spiderman.py --loop 60  # 常驻，每 60 秒检查一次
  python3 monitor_spiderman.py --test 1500469   # 用一部已开售的电影测试报警链路
  python3 monitor_spiderman.py --once --verbose # 单次并打印当前全部在售电影
"""

import argparse
import json
import os
import signal
import ssl
import subprocess
import sys
import time
import urllib.request
import urllib.error
from datetime import datetime

# ----------------------- 配置 -----------------------
CINEMA_ID = "37534"                       # MOViE MOViE 影城（前滩太古里店）
CINEMA_NAME = "MOViE MOViE 影城（前滩太古里店）"
TARGET_MOVIE_ID = 1490607                 # 蜘蛛侠：崭新之日 Spider-Man: Brand New Day
TARGET_KEYWORDS = ["蜘蛛侠", "崭新之日"]    # 备用关键词匹配（防 id 变更）
API_URL = "https://m.maoyan.com/ajax/cinemaDetail?cinemaId=" + CINEMA_ID
BUY_URL = "https://m.maoyan.com/shows/{}".format(CINEMA_ID)   # 手机端购票页
PC_BUY_URL = "https://www.maoyan.com/cinema/{}".format(CINEMA_ID)

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
STATE_FILE = os.path.join(SCRIPT_DIR, "state.json")
LOG_FILE = os.path.join(SCRIPT_DIR, "monitor.log")

USER_AGENT = ("Mozilla/5.0 (iPhone; CPU iPhone OS 15_0 like Mac OS X) "
              "AppleWebKit/605.1.15 (KHTML, like Gecko) "
              "Version/15.0 Mobile/15E148 Safari/604.1")

# 电脑报警最长持续时间（秒）——到点自动停止，绝不会一直响
ALERT_DURATION_SEC = 60
# 每隔多久向 Discord 发一次"运行正常"心跳（秒），默认 1 小时；可用环境变量 HEARTBEAT_SEC 覆盖
HEARTBEAT_INTERVAL_SEC = int(os.environ.get("HEARTBEAT_SEC", "3600"))

# 分时段策略（按 24 小时制的"小时"判断）：
#   [QUIET_START, QUIET_END)         01:00-06:00 完全静默：不抓取、不推送、不心跳（脚本仍在跑）
#   [PHONE_ONLY_START, PHONE_ONLY_END) 06:00-09:00 正常抓取，但命中只推 Discord、不在电脑报警
#   其余时间                          正常运行（电脑报警 + Discord 都触发）
QUIET_START, QUIET_END = 1, 6
PHONE_ONLY_START, PHONE_ONLY_END = 6, 9
# Discord Webhook（推送到手机）。留空字符串则关闭 Discord 推送。
DISCORD_WEBHOOK_URL = ("https://discord.com/api/webhooks/1527908335293173790/"
                       "MzutLSsNI_9pZdSGt8SBrIYmF-77iHQF35xtzXx3sdxgc7wOJP-c3rEdqRNlEimOlPtI")
# ---------------------------------------------------


def log(msg):
    line = "[{}] {}".format(datetime.now().strftime("%Y-%m-%d %H:%M:%S"), msg)
    print(line, flush=True)
    try:
        with open(LOG_FILE, "a", encoding="utf-8") as f:
            f.write(line + "\n")
    except OSError:
        pass


def fetch_detail(retries=3, timeout=15):
    """拉取影城详情 JSON，失败自动重试。"""
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    headers = {
        "User-Agent": USER_AGENT,
        "Referer": "https://m.maoyan.com/shows/" + CINEMA_ID,
        "Accept": "application/json, text/plain, */*",
    }
    last_err = None
    for i in range(retries):
        try:
            req = urllib.request.Request(API_URL, headers=headers)
            with urllib.request.urlopen(req, timeout=timeout, context=ctx) as resp:
                data = resp.read().decode("utf-8")
            return json.loads(data)
        except (urllib.error.URLError, json.JSONDecodeError, TimeoutError) as e:
            last_err = e
            if i < retries - 1:
                time.sleep(3)
    raise RuntimeError("拉取接口失败: {}".format(last_err))


def movie_dates(movie):
    """从 movie.shows[].plist[] 中提取所有排期日期，升序。"""
    dates = set()
    for s in (movie.get("shows") or []):
        for p in (s.get("plist") or []):
            if p.get("dt"):
                dates.add(p["dt"])
    return sorted(dates)


def find_target(movies, target_id, keywords):
    """在电影列表中找目标电影：优先按 id，其次按关键词。"""
    for m in movies:
        if m.get("id") == target_id:
            return m
    for m in movies:
        nm = m.get("nm") or ""
        if any(kw in nm for kw in keywords):
            return m
    return None


def load_state():
    try:
        with open(STATE_FILE, "r", encoding="utf-8") as f:
            return json.load(f)
    except (OSError, json.JSONDecodeError):
        return {}


def save_state(state):
    try:
        with open(STATE_FILE, "w", encoding="utf-8") as f:
            json.dump(state, f, ensure_ascii=False, indent=2)
    except OSError as e:
        log("警告：写入状态文件失败: {}".format(e))


def notify_discord(title, message, url=None):
    """通过 Discord Webhook 推送到手机。返回 True 表示成功。"""
    if not DISCORD_WEBHOOK_URL:
        return False
    content = "🕷️ **{}**\n{}".format(title, message)
    if url:
        content += "\n👉 立即购票：{}".format(url)
    payload = json.dumps({"content": content}).encode("utf-8")
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    for i in range(3):
        try:
            req = urllib.request.Request(
                DISCORD_WEBHOOK_URL, data=payload,
                headers={"Content-Type": "application/json",
                         "User-Agent": "SpidermanTicketMonitor/1.0 (+curl-compatible)"},
                method="POST")
            with urllib.request.urlopen(req, timeout=15, context=ctx) as resp:
                if 200 <= resp.status < 300:
                    log("已推送 Discord 手机提醒。")
                    return True
        except (urllib.error.URLError, TimeoutError) as e:
            if i < 2:
                time.sleep(3)
            else:
                log("警告：Discord 推送失败：{}".format(e))
    return False


def notify_macos(title, message, sound=True, open_url=None,
                 duration=ALERT_DURATION_SEC):
    """
    macOS 弹窗 + 声音 + 语音 + 打开购票页。
    声音/响铃最多持续 `duration` 秒后【自动停止】，绝不会一直响。
    """
    # 弹窗通知（系统通知本身几秒后自动消失）
    try:
        safe_msg = message.replace('"', "'")
        safe_title = title.replace('"', "'")
        script = ('display notification "{}" with title "{}" sound name "Glass"'
                  .format(safe_msg, safe_title))
        subprocess.run(["osascript", "-e", script], check=False, timeout=10)
    except (OSError, subprocess.SubprocessError):
        pass
    # 自动打开购票页面（只开一次）
    if open_url:
        try:
            subprocess.run(["open", open_url], check=False, timeout=10)
        except (OSError, subprocess.SubprocessError):
            pass
    # 在 duration 秒内周期性响铃/播报，到点即停
    deadline = time.time() + max(0, duration)
    said_once = False
    while sound and time.time() < deadline:
        sys.stdout.write("\a")           # 终端响铃
        sys.stdout.flush()
        try:
            subprocess.run(["afplay", "/System/Library/Sounds/Glass.aiff"],
                           check=False, timeout=10)
        except (OSError, subprocess.SubprocessError):
            pass
        # 语音只播报两次，避免整分钟都在说话
        if not said_once:
            try:
                subprocess.run(["say", "蜘蛛侠预售已开启，快去抢票"],
                               check=False, timeout=10)
            except (OSError, subprocess.SubprocessError):
                pass
            said_once = True
        time.sleep(3)
    log("电脑报警结束（最长 {} 秒，已自动停止）。".format(duration))


def check_once(target_id, keywords, do_alert=True, verbose=False,
               computer_alarm=True):
    """执行一次检查。返回 True 表示目标已开预售。
    computer_alarm=False 时只推 Discord、不在电脑上响铃报警。"""
    try:
        data = fetch_detail()
    except RuntimeError as e:
        log("错误：{}".format(e))
        return False

    movies = (data.get("showData") or {}).get("movies") or []
    if verbose:
        log("影城当前共 {} 部影片有排片/预售：".format(len(movies)))
        for m in movies:
            ds = movie_dates(m)
            date_str = "{}~{}".format(ds[0], ds[-1]) if ds else "无场次"
            log("   {:>8}  场次={:<3} 日期={:<22} {}".format(
                m.get("id"), m.get("showCount"), date_str, m.get("nm")))

    target = find_target(movies, target_id, keywords)

    if not target:
        log("尚未上架（列表中无目标影片）。目标 id={} 关键词={}".format(target_id, keywords))
        return False

    name = target.get("nm")
    show_count = target.get("showCount") or 0
    dates = movie_dates(target)

    if show_count <= 0 and not dates:
        log("《{}》已出现在列表但暂无可售场次(showCount=0)，预售尚未真正开启，继续监测…".format(name))
        return False

    # ======= 预售已开启 =======
    earliest = dates[0] if dates else "未知"
    latest = dates[-1] if dates else "未知"
    msg = ("《{}》预售已开启！共 {} 场，日期 {} 至 {}"
           .format(name, show_count, earliest, latest))
    log("★★★★★ " + msg + " ★★★★★")
    log("立即购票：手机 {}  | 电脑 {}".format(BUY_URL, PC_BUY_URL))

    if do_alert:
        # 状态持久化：只在【首次】从未开售→已开售的那一刻报警一次，
        # 之后哪怕场次数变化也不再重复吵你（手机 Discord 也同样只推一次）。
        state = load_state()
        key = "movie_{}".format(target_id)
        prev = state.get(key, {})
        first_time = not prev.get("presale_open")
        if first_time:
            alert_msg = "{}｜{}场｜{}起｜快抢票".format(name, show_count, earliest)
            # 先推手机（最重要，人在外面也能收到）
            notify_discord(
                title="蜘蛛侠预售开启！",
                message="{}\n场次：{} 场，{} 至 {}".format(name, show_count, earliest, latest),
                url=PC_BUY_URL,
            )
            # 再本地报警（仅在允许的时段）
            if computer_alarm:
                notify_macos(
                    title="蜘蛛侠预售开启！",
                    message=alert_msg,
                    sound=True,
                    open_url=PC_BUY_URL,
                )
            else:
                log("当前时段仅推送手机(Discord)，跳过电脑报警。")
        else:
            log("此前已报过警，本次不再重复提醒（仅更新状态）。")
        state[key] = {
            "presale_open": True,
            "show_count": show_count,
            "earliest": earliest,
            "latest": latest,
            "name": name,
            "detected_at": prev.get("detected_at") or
            datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
            "last_seen": datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
        }
        save_state(state)
    return True


def _fmt_uptime(seconds):
    seconds = int(seconds)
    h, rem = divmod(seconds, 3600)
    m, s = divmod(rem, 60)
    if h:
        return "{}小时{}分".format(h, m)
    if m:
        return "{}分{}秒".format(m, s)
    return "{}秒".format(s)


def current_mode(now=None):
    """根据当前钟点返回运行模式：quiet(静默) / phone_only(只推手机) / normal(正常)。"""
    h = (now or datetime.now()).hour
    if QUIET_START <= h < QUIET_END:
        return "quiet"
    if PHONE_ONLY_START <= h < PHONE_ONLY_END:
        return "phone_only"
    return "normal"


_MODE_DESC = {
    "quiet": "静默时段({:02d}:00-{:02d}:00)：暂停抓取与推送".format(QUIET_START, QUIET_END),
    "phone_only": "只推手机时段({:02d}:00-{:02d}:00)：命中仅推 Discord、不电脑报警".format(
        PHONE_ONLY_START, PHONE_ONLY_END),
    "normal": "正常时段：电脑报警 + Discord 均触发",
}


def run_loop(target_id, keywords, interval, do_alert, verbose, is_test):
    """常驻监测主循环：含分时段策略 + Discord 启动/停止/每小时心跳通知。"""
    start_ts = time.time()
    check_count = 0
    last_status = "尚未开售"
    last_heartbeat = time.time()   # 启动通知即视为一次心跳基点
    last_mode = None

    def send_stop_and_exit(signum, frame):
        uptime = _fmt_uptime(time.time() - start_ts)
        log("收到停止信号，正在退出…（运行 {}，检查 {} 次）".format(uptime, check_count))
        if not is_test:
            notify_discord(
                title="监测已停止 🛑",
                message="{}\n累计运行 {}，共检查 {} 次。停止前状态：{}".format(
                    CINEMA_NAME, uptime, check_count, last_status),
            )
        sys.exit(0)

    # 无论是 stop(SIGTERM) 还是 Ctrl+C(SIGINT) 都会先发结束通知再退出
    signal.signal(signal.SIGTERM, send_stop_and_exit)
    signal.signal(signal.SIGINT, send_stop_and_exit)

    log("启动常驻监测，每 {} 秒检查一次。影城={} 目标id={}".format(
        interval, CINEMA_ID, target_id))
    if not is_test:
        notify_discord(
            title="监测已启动 ✅",
            message=("{}\n目标：《蜘蛛侠：崭新之日》(id {})\n"
                     "检查频率：每 {} 秒一次｜已开启防休眠｜每小时汇报一次状态。\n"
                     "分时段：01:00-06:00 静默；06:00-09:00 仅推手机；其余正常。"
                     .format(CINEMA_NAME, TARGET_MOVIE_ID, interval)),
        )

    while True:
        # 测试/演示模式忽略时段策略，始终按正常处理，方便验证
        mode = "normal" if is_test else current_mode()
        if mode != last_mode:
            log("进入{}".format(_MODE_DESC[mode]))
            last_mode = mode

        if mode == "quiet":
            # 完全静默：不抓取、不推送、不心跳，只是继续挂着
            time.sleep(interval)
            continue

        computer_alarm = (mode == "normal")
        try:
            hit = check_once(target_id, keywords, do_alert=do_alert,
                             verbose=verbose, computer_alarm=computer_alarm)
            check_count += 1
            last_status = "已开售！" if hit else "尚未开售"
            if hit and not is_test:
                log("已检测到预售，仍将继续监测。（后续不再重复报警）")
        except Exception as e:  # noqa: BLE001  常驻进程不因偶发异常退出
            log("循环内异常（已忽略，继续）：{}".format(e))

        # 每小时心跳：告诉用户"我还活着，一切正常"（静默时段不发）
        if not is_test and (time.time() - last_heartbeat) >= HEARTBEAT_INTERVAL_SEC:
            uptime = _fmt_uptime(time.time() - start_ts)
            mode_short = {"normal": "正常", "phone_only": "只推手机"}.get(mode, mode)
            notify_discord(
                title="运行正常 ✅",
                message=("蜘蛛侠预售监测运行中\n已运行 {}｜累计检查 {} 次｜"
                         "当前状态：{}｜当前时段：{}｜最近检查 {}".format(
                             uptime, check_count, last_status, mode_short,
                             datetime.now().strftime("%H:%M:%S"))),
            )
            last_heartbeat = time.time()

        time.sleep(interval)


def main():
    parser = argparse.ArgumentParser(description="猫眼影城蜘蛛侠预售监测")
    parser.add_argument("--loop", type=int, metavar="SECONDS",
                        help="常驻循环，每 N 秒检查一次")
    parser.add_argument("--once", action="store_true", help="单次检查（默认）")
    parser.add_argument("--verbose", "-v", action="store_true",
                        help="打印当前影城全部在售影片")
    parser.add_argument("--test", type=int, metavar="MOVIE_ID",
                        help="测试模式：用指定 movieId 作为目标验证报警链路")
    parser.add_argument("--no-alert", action="store_true",
                        help="只检测不弹窗/不发声（调试用）")
    parser.add_argument("--test-discord", action="store_true",
                        help="仅发一条 Discord 测试消息到手机后退出")
    args = parser.parse_args()

    if args.test_discord:
        ok = notify_discord(
            title="测试消息",
            message="蜘蛛侠预售监测的手机推送通道正常 ✅（收到即成功）",
            url=PC_BUY_URL,
        )
        log("Discord 测试{}。".format("成功" if ok else "失败"))
        return

    target_id = args.test if args.test else TARGET_MOVIE_ID
    keywords = [] if args.test else TARGET_KEYWORDS
    do_alert = not args.no_alert

    if args.test:
        log("=== 测试模式：目标 movieId={} ===".format(target_id))

    if args.loop:
        run_loop(target_id, keywords, args.loop, do_alert,
                 args.verbose, is_test=bool(args.test))
    else:
        check_once(target_id, keywords, do_alert=do_alert, verbose=args.verbose)


if __name__ == "__main__":
    main()
