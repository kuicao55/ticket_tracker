#!/usr/bin/env python3
"""xhs-notify — 通过 agent-browser 把通知推送到小红书指定群聊。

用法:
    xhs-notify --group test --title "预售开启 🎬" --body "..."
    xhs-notify --group test --title "预售开启 🎬" --body "..." --url "https://..."
    xhs-notify --title "hi" --dry-run            # 只打印不发

环境变量:
    XHS_PROFILE   agent-browser profile 路径（默认: ~/.agent-browser/profiles/xiaohongshu）

退出码:
    0  成功
    1  参数错误
    2  运行期找不到元素（侧栏 / 群聊 / 输入框）
    3  agent-browser 命令超时

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
import os
import re
import subprocess
import sys
import time

DEFAULT_PROFILE = os.path.expanduser("~/.agent-browser/profiles/xiaohongshu")


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
    # 群名时间戳的边界：HH:MM / 刚刚 / 昨天
    _TIME_BOUNDARY = r'(?=\d{1,2}:\d{2}|刚刚|昨天|$)'
    for line in snap.splitlines():
        ref_match = re.search(r'\[ref=(e\d+)\]', line)
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
    """确保当前在消息页（/chat）。如果不在，从首页导航过去。"""
    url = ab("get", "url").stdout.strip()
    if "/chat" in url:
        return  # 已经在了

    # 打开首页
    ab("open", "https://www.xiaohongshu.com/explore")
    time.sleep(0.3)

    # snapshot 找侧栏「消息」listitem
    snap = snapshot()
    ref = find_ref_by_text(snap, "消息", role="listitem")
    if not ref:
        raise RuntimeError("找不到侧栏「消息」入口（页面结构变了？）")
    ab("click", f"@{ref}")
    time.sleep(0.5)


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
        print(f"[xhs-notify] {e}", file=sys.stderr)
        sys.exit(2)

    print("[xhs-notify] 发送成功 ✓")


if __name__ == "__main__":
    main()
