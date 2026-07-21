"""通知层：Discord Webhook + 电脑通知（macOS 特色 + 跨平台兜底）。"""

import json
import logging
import ssl
import subprocess
import sys
import time
import urllib.error
import urllib.request

log = logging.getLogger("ticket_tracker.notify")

IS_MAC = sys.platform == "darwin"

USER_AGENT = "ticket-tracker/1.0 (+Python urllib)"


def _ctx():
    c = ssl.create_default_context()
    c.check_hostname = False
    c.verify_mode = ssl.CERT_NONE
    return c


# ----------------- Discord -----------------

def notify_discord(webhook_url, title, message, url=None, retries=3):
    """通过 Discord Webhook 推送。返回 True/False。"""
    if not webhook_url:
        return False
    if not webhook_url.startswith("https://discord.com/api/webhooks/"):
        log.warning("Discord webhook URL 格式不符，已跳过推送")
        return False
    content = "**{}**\n{}".format(title, message)
    if url:
        content += "\n👉 {}".format(url)
    payload = json.dumps({"content": content}).encode("utf-8")
    headers = {"Content-Type": "application/json", "User-Agent": USER_AGENT}
    last = None
    for i in range(retries):
        try:
            req = urllib.request.Request(
                webhook_url, data=payload, headers=headers, method="POST")
            with urllib.request.urlopen(req, timeout=15, context=_ctx()) as resp:
                if 200 <= resp.status < 300:
                    log.info("Discord 推送成功：%s", title)
                    return True
        except (urllib.error.URLError, TimeoutError) as e:
            last = e
            if i < retries - 1:
                time.sleep(3)
    log.warning("Discord 推送失败: %s", last)
    return False


# ----------------- macOS 电脑通知（带时长上限自动停） -----------------

def notify_macos(title, message, sound=True, open_url=None, duration=60):
    """弹窗 + 周期性铃声（duration 秒内）+ 语音一次 + 自动打开购票页。
    非 macOS 平台静默返回。"""
    if not IS_MAC:
        return
    # 弹窗（系统自带几秒后自动消失）
    try:
        script = ('display notification "{}" with title "{}" sound name "Glass"'
                  .format(message.replace('"', "'"), title.replace('"', "'")))
        subprocess.run(["osascript", "-e", script], check=False, timeout=10)
    except (OSError, subprocess.SubprocessError):
        pass
    # 自动打开购票页（只开一次）
    if open_url:
        try:
            subprocess.Popen(["open", open_url])
        except (OSError, subprocess.SubprocessError):
            pass
    # 周期性响铃 + 一次性语音
    deadline = time.time() + max(0, duration)
    said = False
    while sound and time.time() < deadline:
        sys.stdout.write("\a")
        sys.stdout.flush()
        try:
            subprocess.run(["afplay", "/System/Library/Sounds/Glass.aiff"],
                           check=False, timeout=10)
        except (OSError, subprocess.SubprocessError):
            pass
        if not said:
            try:
                subprocess.run(["say", "蜘蛛侠预售已开启，快去抢票"],
                               check=False, timeout=10)
            except (OSError, subprocess.SubprocessError):
                pass
            said = True
        time.sleep(3)


# ----------------- 防休眠 -----------------

# 当前 caffeinate 子进程（模块级状态，供 TUI 状态栏展示）
_caffeinate_proc = None


def caffeinate_start(child_pid):
    """开启 caffeinate 绑定 child_pid。返回 subprocess.Popen（None 表示不可用）。"""
    global _caffeinate_proc
    if not IS_MAC:
        return None
    try:
        p = subprocess.Popen(
            ["caffeinate", "-i", "-s", "-w", str(child_pid)],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        _caffeinate_proc = p
        return p
    except OSError as e:
        log.warning("caffeinate 启动失败（已忽略）：%s", e)
        return None


def caffeinate_stop(proc):
    global _caffeinate_proc
    if proc and proc.poll() is None:
        try:
            proc.terminate()
            proc.wait(timeout=5)
        except (OSError, subprocess.TimeoutExpired):
            try:
                proc.kill()
            except OSError:
                pass
    # 清掉引用，无论是否同一个对象，让 is_caffeinated() 反映真实情况
    if proc is _caffeinate_proc or _caffeinate_proc is None:
        _caffeinate_proc = None
    elif _caffeinate_proc is not None and _caffeinate_proc.poll() is not None:
        _caffeinate_proc = None


def is_caffeinated():
    """caffeinate 子进程是否还活着：
    - macOS：返回 True / False
    - 其他平台：返回 None（不可用，UI 显示 "n/a"）
    """
    if not IS_MAC:
        return None
    p = _caffeinate_proc
    return p is not None and p.poll() is None
