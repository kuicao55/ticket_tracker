#!/usr/bin/env python3
"""wechat-notify-fast — 极速版：跳过截图/OCR/标题校验，直接把消息发给当前微信会话。

适用前提（专用单一账号，你已确认）:
    - 微信专用账号，只有一个群（如 ticket-tracker）且已置顶
    - 微信已登录，且当前正停在目标群聊页面（脚本不会再去找会话）

用法:
    wechat-notify-fast --message "正文"
    wechat-notify-fast --message "hi" --dry-run     # 只打印不发

原理:
    - 不截图、不 OCR、不校验标题、也不点窗口（比 wechat-notify 快一个数量级）
    - 微信 4.x 打开会话后输入框自动聚焦，pbcopy 写入剪贴板后，
      通过 CGEventPostToPid 直接把 Cmd+V / Enter 发到微信进程的 PID
    - 乐观执行：主路径 CGEventPostToPid 直投（最快、免前台、锁屏可用），
      一旦注入失败自动回退到 System Events key code（保守、需前台）
    - 仅系统睡眠会暂停微信进程；锁屏/熄屏不影响发送

退出码:
    0  成功
    1  参数错误
    2  运行期失败
    3  命令超时
    4  权限/环境问题
"""
import argparse
import logging
import os
import subprocess
import sys
import time
from logging.handlers import RotatingFileHandler


class NotifyError(Exception):
    def __init__(self, msg, code=2):
        super().__init__(msg)
        self.code = code


def _run(args, timeout=30, input=None):
    return subprocess.run(args, capture_output=True, text=True,
                          timeout=timeout, input=input)


def osa(script, *args, timeout=30):
    r = _run(["osascript", "-", *args], timeout=timeout, input=script)
    if r.returncode != 0:
        raise NotifyError(f"osascript 失败: {r.stderr.strip()}")
    return r.stdout.strip()


_LOG_NAME = "wechat-notify-fast"
_DEFAULT_LOG = "~/Library/Logs/wechat-notify-fast.log"


def _setup_logging(log_file):
    """配置 RotatingFileHandler（1MB×3）。多次调用会替换 handler。"""
    path = os.path.expanduser(log_file)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    log = logging.getLogger(_LOG_NAME)
    log.setLevel(logging.INFO)
    for h in list(log.handlers):
        log.removeHandler(h)
    h = RotatingFileHandler(path, maxBytes=1_000_000, backupCount=3, encoding="utf-8")
    h.setFormatter(logging.Formatter(
        "%(asctime)s [%(levelname)s] %(message)s",
        datefmt="%Y-%m-%dT%H:%M:%S"))
    log.addHandler(h)
    return log


def _msg_preview(text, limit=60):
    text = text.replace("\n", "\\n")
    return (text[:limit] + "...") if len(text) > limit else text


_POST_KEYS_JS = r'''function run(argv) {
  ObjC.import('CoreGraphics')
  ObjC.import('Foundation')
  const pid = Number(argv[0])
  const CMD = 1 << 20   // kCGEventFlagMaskCommand = 1048576
  function postKey(keyCode, flags, down) {
    const ev = $.CGEventCreateKeyboardEvent(null, keyCode, down)
    if (flags) $.CGEventSetFlags(ev, flags)
    $.CGEventPostToPid(pid, ev)
    $.NSThread.sleepForTimeInterval(0.02)
  }
  // Cmd+V 粘贴（key 9 = 'v'）
  postKey(9, CMD, true)
  postKey(9, CMD, false)
  $.NSThread.sleepForTimeInterval(0.2)
  // Enter 发送（key 36 = Return）
  postKey(36, 0, true)
  postKey(36, 0, false)
  return 'ok'
}
'''


def _wechat_pid():
    r = _run(["pgrep", "-x", "WeChat"], timeout=10)
    if r.returncode != 0 or not r.stdout.strip():
        raise NotifyError("找不到运行中的 WeChat 进程，请先启动并登录微信", code=2)
    return int(r.stdout.strip().splitlines()[0])


def _post_keys_pid(pid):
    """主路径：CGEventPostToPid 直投到 PID，无需前台，锁屏可用。"""
    r = _run(["osascript", "-l", "JavaScript", "-e", _POST_KEYS_JS, str(pid)],
             timeout=20)
    if r.returncode != 0:
        raise NotifyError(f"CGEventPostToPid 注入失败: {r.stderr.strip()}")


def _ensure_window_unminimized():
    """微信窗口被最小化时，CGEventPostToPid 的键盘事件会落空（无 key window）。

    分两步恢复：先 AX deminiaturize 恢复窗口显示（微信 4.x 单独 activate
    不恢复最小化窗口），再 activate 让它成为 key window 并重聚焦输入框
    （单独 deminiaturize 只恢复显示、不聚焦）。检测失败静默跳过。

    Returns: True 表示检测到最小化并执行了恢复；False 表示未最小化或检测失败。
    """
    try:
        r = _run(["osascript", "-e",
                  'tell application "System Events" to tell process "WeChat" to '
                  'get value of attribute "AXMinimized" of window 1'],
                 timeout=10)
    except subprocess.TimeoutExpired:
        return False
    if r.returncode != 0 or r.stdout.strip().lower() != "true":
        return False
    _run(["osascript", "-e",
          'tell application "System Events" to tell process "WeChat" to '
          'set value of attribute "AXMinimized" of window 1 to false'],
         timeout=10)
    time.sleep(0.2)
    _run(["osascript", "-e", 'tell application "WeChat" to activate'], timeout=10)
    time.sleep(0.4)
    return True


def _fallback_send():
    """保守路径：System Events key code（需前台 + 辅助功能权限）。"""
    osa('tell application "WeChat" to activate', timeout=10)
    time.sleep(0.3)
    try:
        osa('tell application "System Events" to tell process "WeChat" to key code 9 using {command down}',
            timeout=15)
        time.sleep(0.4)
        osa('tell application "System Events" to tell process "WeChat" to key code 36',
            timeout=15)
    except NotifyError as e:
        msg = str(e).lower()
        if "25211" in msg or "assistive" in msg or "not allowed" in msg:
            raise NotifyError(
                "缺少「辅助功能」权限：请到 系统设置 → 隐私与安全性 → 辅助功能，"
                "勾选运行本脚本的终端，然后退出重开终端。"
                f"原始错误: {e}", code=4)
        raise


def type_and_send(text):
    """剪贴板写入 → CGEventPostToPid 直投（最快）；失败回退到 System Events。

    主路径不依赖前台、不查权限、锁屏/熄屏也能发（微信需停在目标会话）。
    注意：不要用坐标点输入框，底部 0.94 高度处是工具栏（含截屏按钮），
    会误触发截图。
    """
    log = logging.getLogger(_LOG_NAME)
    log.info(f"start msg={_msg_preview(text)!r}")

    r = _run(["pbcopy"], input=text, timeout=10)
    if r.returncode != 0:
        log.error(f"pbcopy failed: {r.stderr.strip()}")
        raise NotifyError(f"写入剪贴板失败: {r.stderr.strip()}", code=2)
    log.info("pbcopy ok")

    time.sleep(0.05)
    try:
        pid = _wechat_pid()
    except NotifyError as e:
        log.error(f"wechat pid lookup failed: {e}")
        raise
    log.info(f"wechat pid={pid}")

    if _ensure_window_unminimized():
        log.warning("window was minimized; recovered via deminiaturize+activate")

    try:
        _post_keys_pid(pid)
        log.info("post keys ok (CGEventPostToPid, main path)")
    except NotifyError as e:
        log.warning(f"main path failed: {e}; falling back to System Events")
        try:
            _fallback_send()
            log.info("post keys ok (System Events, fallback)")
        except NotifyError as fe:
            log.error(f"fallback also failed: {fe}")
            raise


def main():
    p = argparse.ArgumentParser(description="极速版：直接把消息发给当前微信会话")
    p.add_argument("--message", required=True, help="要发送的正文（必填）")
    p.add_argument("--dry-run", action="store_true", help="只打印计划，不实际发送")
    p.add_argument("--log-file", default=_DEFAULT_LOG,
                   help=f"日志文件路径（默认 {_DEFAULT_LOG}，1MB×3 自动 rotate）")
    args = p.parse_args()

    log = _setup_logging(args.log_file)
    log.info(f"== invocation: msg={_msg_preview(args.message)!r} dry_run={args.dry_run}")

    print(f"[wechat-notify-fast] 文本:\n{args.message}\n")

    if args.dry_run:
        log.info("dry-run, skipping send")
        print("[wechat-notify-fast] dry-run，跳过实际发送")
        return

    try:
        type_and_send(args.message)
        log.info("== send success")
        print("[wechat-notify-fast] 发送成功 ✓")
    except subprocess.TimeoutExpired as e:
        log.error(f"== timeout: {e}")
        print(f"[wechat-notify-fast] 命令超时: {e}", file=sys.stderr)
        sys.exit(3)
    except NotifyError as e:
        log.error(f"== failed code={e.code}: {e}")
        print(f"[wechat-notify-fast] {e}", file=sys.stderr)
        sys.exit(e.code)


if __name__ == "__main__":
    main()