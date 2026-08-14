#!/usr/bin/env python3
"""xhs-notify — 通过 agent-browser 把通知推送到小红书指定群聊。

用法:
    xhs-notify --group test --title "预售开启 🎬" --body "..."
    xhs-notify --group test --title "预售开启 🎬" --body "..." --url "https://..."
    xhs-notify --title "hi" --dry-run            # 只打印不发

环境变量:
    XHS_PROFILE   agent-browser profile 路径（默认: ~/.agent-browser/profiles/xiaohongshu）
    XHS_WEBHOOK   Discord webhook URL（仅 --recover-on-block 用）

退出码:
    0  成功
    1  参数错误
    2  运行期找不到元素（侧栏 / 群聊 / 输入框）
    3  agent-browser 命令超时
    4  检测到风控拦截，需 headed 扫码登录（已触发自动恢复流程）

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
前置条件（一次性设置）
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

1. 安装 agent-browser（详见 https://github.com/anthropics/agent-browser）

2. 创建 profile 并登录小红书（只需一次，扫码后 cookie 长期保留）:

     agent-browser --headed --profile ~/.agent-browser/profiles/xiaohongshu \
       open https://www.xiaohongshu.com/explore

   弹出浏览器窗口，扫码登录，然后直接关闭浏览器即可。

3. 安装 xhs-notify 到 PATH（让 ticket_tracker 能调）:

     chmod +x xhs-notify.py
     ln -s "$(pwd)/xhs-notify.py" /usr/local/bin/xhs-notify
     # 或: cp xhs-notify.py /usr/local/bin/xhs-notify

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
运行时前置条件（每次发通知前）
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

- agent-browser daemon 在跑（首次 agent-browser 命令会自动起）
- 浏览器当前停在**小红书任意页面**即可，脚本会自己导航
- 强烈建议停在「首页 /explore」或「消息页 /chat」，不要停在外部网站

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
session 过期处理
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

- 登录态保存在 profile 里，token 一般能撑数周到数月
- 过期征兆：脚本跑到 click 群聊后找不到元素 / 页面跳到登录二维码
- 过期后重新登录（head 模式，5 秒搞定）:

     agent-browser close
     agent-browser --headed --profile ~/.agent-browser/profiles/xiaohongshu \
       open https://www.xiaohongshu.com/explore
     # 扫码，关浏览器
"""
import argparse
import json
import os
import re
import subprocess
import sys
import time
import urllib.request

# Cloudflare 会拦截默认的 Python-urllib UA（error 1010），
# 用真实浏览器 UA 通过校验。
USER_AGENT = (
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) "
    "AppleWebKit/537.36 (KHTML, like Gecko) "
    "Chrome/120.0.0.0 Safari/537.36"
)

DEFAULT_PROFILE = os.path.expanduser("~/.agent-browser/profiles/xiaohongshu")
DEFAULT_WEBHOOK = (
    "https://discord.com/api/webhooks/1537734932959334401/"
    "YN5LLtGiMS1VMkuiLgB6ZYPzljNfWJofGph8tIhPSXDvUPQTyYijtnlu99u2ZeM56XMV"
)


def ab(*args, timeout=30):
    """Run agent-browser command, return CompletedProcess."""
    return subprocess.run(
        ["agent-browser", *args],
        capture_output=True, text=True, timeout=timeout)


def snapshot():
    r = ab("snapshot", "-i")
    if r.returncode != 0:
        raise RuntimeError(f"snapshot 失败: {r.stderr.strip()}")
    return r.stdout


# 解析一行 snapshot，形如：
#   - listitem "消息" [level=1, ref=e13] clickable [cursor:pointer]
#   - generic  "test09:46" [ref=e9] clickable [cursor:pointer]
#   - generic  [ref=e4] editable [contenteditable]
# name 字段允许含转义引号 \"…\"（来自群名修改提示等场景）
_LINE_RE = re.compile(
    r'-\s+(\w+)(?:\s+"((?:[^"\\]|\\.)*)")?\s+\[(?:level=(\d+),?\s*)?ref=(e\d+)\](.*)$')


def find_ref_by_text(snap, text, role=None, exact_name_only=False):
    """在 snapshot 中找包含 `text` 的可点击元素 ref。

    exact_name_only=True 时：精确匹配 name 字段去掉时间戳后的群名，
    避免 "test" 误中 "test-tracker"。
    exact_name_only=False 时：按整行做子串匹配（大小写不敏感），
    能容忍 name 里的转义引号，适合 sidebar 这种结构宽松的场景。
    """
    text_lower = text.lower()
    # 时间戳边界：数字（HH:MM / X月X日 等）/ 刚 / 昨 / 今 / 前天 / 周 / 星期
    # 这些都是时间相关词汇的开头，可作为群名结束边界
    _TIME_BOUNDARY = r'(?=\d|刚刚|昨天|今天|前天|周|星期|$)'
    for line in snap.splitlines():
        ref_match = re.search(r'ref=(e\d+)', line)  # 也匹配 [level=N, ref=eN] 形式
        if not ref_match:
            continue
        if "clickable" not in line and "editable" not in line:
            continue
        if role:
            if not re.match(rf'-\s+{re.escape(role)}\b', line):
                continue

        if exact_name_only:
            m = _LINE_RE.match(line)
            if not m:
                continue
            name = m.group(2) or ""
            # 群名 = name 在时间戳之前的前缀
            gm = re.match(rf'(.*?){_TIME_BOUNDARY}', name)
            group_name = (gm.group(1) if gm else name).strip()
            if group_name.lower() != text_lower:
                continue
        else:
            line_no_ref = re.sub(r'\[ref=e\d+\]', '', line).lower()
            if text_lower not in line_no_ref:
                continue

        return ref_match.group(1)
    return None


def find_editable_ref(snap):
    """找 contenteditable 输入框（聊天输入）。"""
    for line in snap.splitlines():
        if "editable" in line and "[ref=" in line:
            m = re.search(r'ref=(e\d+)', line)
            if m:
                return m.group(1)
    return None


def ensure_chat_page():
    """确保当前在消息页（/chat）。如果不在，直接 open /chat 导航过去。

    流程：
    1. 已 /chat → 直接返回
    2. 不在 /chat → open /chat（SPA 直接深链比 click 侧栏 listitem 更可靠——
       侧栏 listitem 名字会随未读 badge 变化「消息」/「1消息」，且 click 在某些
       页面状态下不触发跳转。open /chat 是 SPA 直接深链，1 次往返搞定）
    3. 等 URL 翻到 /chat 再继续（最多 3 × 0.5s），让 SPA 落地稳定
    """
    url = ab("get", "url").stdout.strip()
    if "/chat" in url:
        return  # 已经在聊天页

    ab("open", "https://www.xiaohongshu.com/chat")

    # 验证导航结果：等 URL 翻到 /chat
    for _ in range(3):
        time.sleep(0.5)
        if "/chat" in ab("get", "url").stdout:
            return
    # URL 没翻也不要紧，后面的 click_group 有自己的 retry；不误报


def click_group(group):
    """点开指定群聊。精确匹配群名前缀，找不到时重试几次。"""
    ref = None
    for attempt in range(3):
        snap = snapshot()
        ref = find_ref_by_text(snap, group, exact_name_only=True)
        if ref:
            break
        time.sleep(0.7)
    if not ref:
        raise RuntimeError(
            f"找不到群聊 {group!r}（可能列表太长被折叠、刚创建还没刷新、"
            f"或名字打错。可以手动刷新消息页或滚到列表可见区再试）")
    ab("click", f"@{ref}")
    time.sleep(0.5)


def type_and_send(text):
    """在输入框输入 text 并回车发送。"""
    snap = snapshot()
    ref = find_editable_ref(snap)
    if not ref:
        raise RuntimeError("找不到聊天输入框（contenteditable）")
    ab("click", f"@{ref}")
    time.sleep(0.1)
    ab("keyboard", "type", text)
    ab("press", "Enter")


# ━━━━━━━━━━━━━━━ 风控自动恢复（opt-in）━━━━━━━━━━━━━━━━

def is_blocked_by_security():
    """当前是否需要登录（风控拦截 / session 过期 / 强制登录弹窗）。

    三种情况都应触发恢复流程：
    1. 风控拦截页：URL 含 /website-login/error 或 error_code=300012
    2. Session 过期：URL 是 xhs 域但页面显示登录 UI（手机/扫码登录），
       且侧栏 消息 不在（说明用户未登录）
    """
    url = ab("get", "url").stdout.strip()
    if "/website-login/error" in url or "error_code=300012" in url:
        return True
    if "xiaohongshu.com" not in url:
        return False
    snap = snapshot()
    login_keywords = ("登录", "扫码登录", "+86", "获取验证码", "手机号登录")
    has_login_ui = any(kw in snap for kw in login_keywords)
    has_sidebar = any(
        re.search(r'-\s+listitem\s+"消息"', line) for line in snap.splitlines())
    return has_login_ui and not has_sidebar


def notify_discord_webhook(webhook_url, content):
    """POST 到 Discord webhook。返回是否 2xx。失败不抛异常。"""
    if not webhook_url:
        return False
    try:
        payload = json.dumps({"content": content}).encode("utf-8")
        req = urllib.request.Request(
            webhook_url, data=payload,
            headers={"Content-Type": "application/json",
                     "User-Agent": USER_AGENT},
            method="POST")
        with urllib.request.urlopen(req, timeout=10) as resp:
            return 200 <= resp.status < 300
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", errors="replace")[:200]
        print(f"[xhs-notify] Discord HTTP {e.code}: {body}", file=sys.stderr)
        return False
    except Exception as e:
        print(f"[xhs-notify] Discord 通知异常: {type(e).__name__}: {e}",
              file=sys.stderr)
        return False


def capture_qr_png():
    """截取当前登录页（应是二维码页），返回 PNG 字节。"""
    r = subprocess.run(
        ["agent-browser", "screenshot", "--screenshot-format", "png"],
        capture_output=True, text=True, timeout=30)
    if r.returncode != 0:
        raise RuntimeError(f"screenshot 失败: {r.stderr.strip()}")
    # 输出形如：✓ Screenshot saved to /tmp/.../screenshot-1234.png
    path = None
    for line in r.stdout.splitlines():
        m = re.search(r"saved to (\S+)", line)
        if m:
            path = m.group(1)
            break
    if not path or not os.path.exists(path):
        raise RuntimeError(f"找不到截图路径: stdout={r.stdout!r}")
    with open(path, "rb") as f:
        return f.read()


def notify_discord_with_qr(webhook_url, png_bytes, caption):
    """上传 PNG 到 Discord webhook（multipart/form-data），附带文字说明。"""
    if not webhook_url:
        return False
    try:
        boundary = "----xhsNotifyBoundary7MA4YWxkTrZu0gW"
        head = (
            f"--{boundary}\r\n"
            f"Content-Disposition: form-data; name=\"payload_json\"\r\n"
            f"Content-Type: application/json\r\n\r\n"
            f"{json.dumps({'content': caption})}\r\n"
            f"--{boundary}\r\n"
            f"Content-Disposition: form-data; name=\"files[0]\"; "
            f"filename=\"qr.png\"\r\n"
            f"Content-Type: image/png\r\n\r\n"
        ).encode("utf-8")
        tail = f"\r\n--{boundary}--\r\n".encode("utf-8")
        body = head + png_bytes + tail

        req = urllib.request.Request(
            webhook_url, data=body,
            headers={"Content-Type":
                     f"multipart/form-data; boundary={boundary}",
                     "User-Agent": USER_AGENT},
            method="POST")
        with urllib.request.urlopen(req, timeout=20) as resp:
            return 200 <= resp.status < 300
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", errors="replace")[:200]
        print(f"[xhs-notify] Discord 图片 HTTP {e.code}: {body}",
              file=sys.stderr)
        return False
    except Exception as e:
        print(f"[xhs-notify] Discord 图片上传异常: {type(e).__name__}: {e}",
              file=sys.stderr)
        return False


def recover_from_block(webhook_url):
    """风控后的恢复流程：
       close daemon → headed 重开 → 等二维码渲染 → 截图 → Discord 上传（带图）。

    用户可在 headed 窗口扫码，或直接在 Discord 通知里扫码（手机上）。
    登录完成后关闭 headed 窗口，下次重跑 xhs-notify。
    """
    print("[xhs-notify] 检测到风控，开始自动恢复…", file=sys.stderr)
    subprocess.run(["agent-browser", "close"],
                   capture_output=True, text=True, timeout=15)
    subprocess.run(
        ["agent-browser", "--headed", "--profile", DEFAULT_PROFILE,
         "open", "https://www.xiaohongshu.com/explore"],
        capture_output=True, text=True, timeout=30)
    # 等登录页二维码渲染稳定
    time.sleep(2)

    caption = (
        "🔴 **xhs-notify 检测到小红书风控拦截**\n"
        "请用小红书 App 扫描下方二维码登录（headed 浏览器也已弹出可备用）。\n"
        "登录完成后关闭 headed 浏览器，再重新运行 xhs-notify。"
    )
    try:
        png = capture_qr_png()
        ok = notify_discord_with_qr(webhook_url, png, caption)
        if ok:
            print("[xhs-notify] Discord 通知（含二维码）已发送 ✓", file=sys.stderr)
        else:
            raise RuntimeError("图片上传失败")
    except Exception as e:
        print(f"[xhs-notify] 截图/上传失败: {type(e).__name__}，降级为纯文本",
              file=sys.stderr)
        ok = notify_discord_webhook(webhook_url, caption)
        print(f"[xhs-notify] Discord 文本通知: {'已发送' if ok else '失败'}",
              file=sys.stderr)

    print("[xhs-notify] headed 浏览器已弹出 + 二维码已推 Discord，等待扫码…",
          file=sys.stderr)


def main():
    p = argparse.ArgumentParser(
        description="把通知推送到小红书群聊（via agent-browser）",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__.split("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━")[0])
    p.add_argument("--group", required=True,
                   help="群聊名称（必填，无默认值）")
    p.add_argument("--title", required=True, help="通知标题")
    p.add_argument("--body", default="", help="通知正文（可选）")
    p.add_argument("--url", default="", help="链接（可选）")
    p.add_argument("--dry-run", action="store_true",
                   help="只打印计划，不实际发送")
    p.add_argument("--recover-on-block", action="store_true",
                   help="检测到风控时自动 headed 重开 + Discord 通知（默认关）")
    p.add_argument("--webhook",
                   default=os.environ.get("XHS_WEBHOOK", DEFAULT_WEBHOOK),
                   help="Discord webhook URL（默认已填，可被 $XHS_WEBHOOK 覆盖）")
    args = p.parse_args()

    parts = [args.title]
    if args.body:
        parts.append(args.body)
    if args.url:
        parts.append(args.url)
    text = "\n".join(parts)

    print(f"[xhs-notify] group={args.group!r}")
    print(f"[xhs-notify] 文本:\n{text}\n")

    if args.dry_run:
        print("[xhs-notify] dry-run，跳过实际发送")
        return

    try:
        ensure_chat_page()
        click_group(args.group)
        type_and_send(text)
    except subprocess.TimeoutExpired as e:
        print(f"[xhs-notify] agent-browser 命令超时: {e}", file=sys.stderr)
        sys.exit(3)
    except RuntimeError as e:
        if args.recover_on_block and is_blocked_by_security():
            recover_from_block(args.webhook)
            sys.exit(4)
        print(f"[xhs-notify] {e}", file=sys.stderr)
        sys.exit(2)

    print("[xhs-notify] 发送成功 ✓")


if __name__ == "__main__":
    main()
