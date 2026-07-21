"""ticket-tracker 命令行入口。"""

import json
import logging
import os
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime
from pathlib import Path

import click
from rich.console import Console
from rich.table import Table

from . import __version__, config as cfg_mod
from . import notify
from .paths import config_file, log_file, pid_file
from .presets import list_presets

console = Console()


def _setup_logging(level=logging.INFO):
    lf = log_file()
    logging.basicConfig(
        level=level,
        format="%(asctime)s %(levelname)s %(name)s %(message)s",
        handlers=[
            logging.FileHandler(lf, encoding="utf-8"),
            logging.StreamHandler(),
        ],
    )


# ----------------- main / version -----------------

@click.group(invoke_without_command=True, context_settings={"help_option_names": ["-h", "--help"]})
@click.version_option(__version__, "-V", "--version")
@click.pass_context
def main(ctx):
    """ticket-tracker — 猫眼影城预售监测 CLI+TUI

常用流程：

    tt init                                      初始化（创建配置 + 内置预设）

    tt cinema add-preset 前滩太古里              添加一个内置影院

    tt watch add 1490607 --cinema 37534          监视 1490607（蜘蛛侠）于 37534 影城
    tt watch add 1490607 -c 37534 -c 2127        多影院监视
    tt watch add 1490607 -c 37534 -d 2026-07-29  限定日期

    tt films                                     猫眼在映 / 即将上映电影（最多 20 条）

    tt start                                     前台 TUI（推荐；可热键交互）
    tt start --detach                            后台守护（无人值守）
    tt stop / tt restart / tt status / tt log

    tt test                                      测试 Discord / macOS 通知
    tt doctor                                    自检

完整文档见 README 或运行 `tt help <command>`。
"""
    if ctx.invoked_subcommand is None:
        # 默认行为：打印 status
        ctx.invoke(status)


@main.command()
@click.argument("command", required=False, nargs=-1, metavar="[COMMAND]...")
def help(command):
    """显示所有命令，或指定命令的详细帮助。

示例：
        tt help                # 列出所有命令
        tt help watch          # watch 子命令的帮助
        tt help watch add      # watch add 的详细帮助
    """
    if command:
        ctx = click.get_current_context().find_root()
        # 沿着 command 一层层找
        current = ctx.command
        for token in command:
            sub = current.get_command(ctx, token)
            if not sub:
                console.print("[red]未知命令[/red]: {}".format(" ".join(command)))
                sys.exit(1)
            current = sub
        click.echo(current.get_help(ctx))
        return
    ctx = click.get_current_context().find_root()
    click.echo(ctx.command.get_help(ctx))


# ----------------- init -----------------

@main.command()
def init():
    """首次使用：创建配置 + 加入内置预设。

示例：
    tt init
    """
    cfg = cfg_mod.load_or_init()
    added = 0
    for name, p in list_presets().items():
        if cfg_mod.add_cinema(cfg, p["id"], p["name"]):
            added += 1
    console.print("[green]✓[/green] 配置文件：{}".format(config_file()))
    if added:
        console.print("[green]✓[/green] 已加入内置影院 {} 条".format(added))
    else:
        console.print("[dim]· 内置影院已在配置中[/dim]")
    if not cfg.get("discord_webhook"):
        console.print("[yellow]![/yellow] Discord webhook 未配置，请运行：")
        console.print("    tt config set discord-webhook https://discord.com/api/webhooks/...")
    console.print("\n下一步：tt cinema add-preset <名称> 或直接 tt watch add")


# ----------------- start / stop / restart -----------------

@main.command()
@click.option("--detach", is_flag=True, help="后台 daemon 运行（无 TUI）")
@click.option("--interval", "-i", type=int, default=None, help="覆盖检查间隔（秒）")
@click.option("--watch", "watch_id", default=None, help="只运行指定的 watch id（用于调试）")
def start(detach, interval, watch_id):
    """启动监测（默认前台 TUI，Ctrl+C 退出）。

示例：
    tt start                       前台 TUI（推荐）
    tt start --detach              后台守护（无 TUI，发 Discord 通知）
    tt start --detach -i 30        后台且每 30s 检查一次
    tt start --watch w_a1b2c3      只跑某一条 watch（调试用）
    """
    cfg = cfg_mod.load_or_init()
    if interval is not None:
        cfg["check_interval"] = interval
        cfg_mod.save(cfg)

    if detach:
        # daemon 模式无 TUI，没监视项就空跑；先提示用户加
        if not cfg["watches"]:
            console.print("[yellow]![/yellow] 尚无监视项，先用 tt watch add 添加：")
            console.print("    tt watch add 1490607 --cinema 37534 --name '蜘蛛侠：崭新之日'")
            sys.exit(1)
        _start_daemon_mode(watch_id)
    else:
        # 前台 TUI：允许无监视项进入（EmptyState 会引导用户按 a 添加）
        _start_foreground(cfg, watch_id)


def _daemonize(func):
    """将 func 包装为真正的 daemon：双 fork + setsid，脱离父进程组。
    仅 Unix/macOS。"""
    if sys.platform == "win32":
        return func()

    pid = os.fork()
    if pid > 0:
        return None
    os.setsid()
    pid = os.fork()
    if pid > 0:
        os._exit(0)
    sys.stdout.flush(); sys.stderr.flush()
    return func()


def _start_daemon_mode(watch_id):
    """后台模式：纯守护，不画 TUI。"""
    _setup_logging()
    from .monitor import Monitor
    flt = [watch_id] if watch_id else None
    m = Monitor(watch_filter=flt)
    caffeine = None
    try:
        caffeine = notify.caffeinate_start(os.getpid())
    except Exception:
        pass
    try:
        m.run()
    finally:
        notify.caffeinate_stop(caffeine)


def _start_foreground(cfg, watch_id):
    """前台 TUI 模式。"""
    _setup_logging()
    _run_monitor_tui(watch_id)


def _run_monitor_tui(watch_id):
    from .monitor import Monitor
    from .tui import run_tui
    flt = [watch_id] if watch_id else None
    m = Monitor(watch_filter=flt)
    caffeine = None
    try:
        caffeine = notify.caffeinate_start(os.getpid())
    except Exception:
        pass
    try:
        run_tui(m)
    finally:
        notify.caffeinate_stop(caffeine)


@main.command()
def stop():
    """停止后台监测进程（发结束通知）。"""
    pf = pid_file()
    if not pf.exists():
        console.print("[dim]未运行[/dim]")
        return
    pid = int(pf.read_text().strip())
    try:
        os.kill(pid, signal.SIGTERM)
        for _ in range(10):
            try:
                os.kill(pid, 0)
                time.sleep(0.5)
            except ProcessLookupError:
                break
        else:
            os.kill(pid, signal.SIGKILL)
        console.print("[green]✓[/green] 已停止 (PID {})".format(pid))
    except ProcessLookupError:
        console.print("[dim]进程已不存在[/dim]")
    finally:
        pf.unlink(missing_ok=True)


@main.command()
def restart():
    """重启后台监测。"""
    ctx = click.get_current_context()
    ctx.invoke(stop)
    time.sleep(0.5)
    ctx.invoke(start, detach=True, interval=None, watch_id=None)


# ----------------- status / log -----------------

@main.command()
def status():
    """查看运行状态（一行概要）。"""
    pf = pid_file()
    if not pf.exists():
        console.print("[red]未运行[/red]  启动：tt start")
        return
    pid = int(pf.read_text().strip())
    try:
        os.kill(pid, 0)
        alive = True
    except OSError:
        alive = False
    if not alive:
        console.print("[red]进程不存在[/red] (PID {})".format(pid))
        return
    cfg = cfg_mod.load_or_init()
    n_watch = sum(1 for w in cfg["watches"] if w.get("enabled"))
    console.print("[green]运行中[/green]  PID {}  | 监视项 {} 条 | 间隔 {}s | Discord: {}".format(
        pid, n_watch, cfg.get("check_interval", 90),
        "已配置" if cfg.get("discord_webhook") else "未配置"))


@main.command()
@click.option("-n", type=int, default=None, help="最近 N 行")
@click.option("-f", "--follow", is_flag=True, help="持续 tail")
def log(n, follow):
    """查看运行日志。

示例：
    tt log              最近 30 行
    tt log -n 100       最近 100 行
    tt log -f           持续 tail（Ctrl+C 退出）
    """
    lf = log_file()
    if not lf.exists():
        console.print("[dim]尚无日志[/dim]")
        return
    if follow:
        if n is None: n = 20
        console.print("tail -f {}  (Ctrl+C 退出)".format(lf))
        subprocess.run(["tail", "-f", "-n", str(n), str(lf)])
        return
    if n is None: n = 30
    lines = lf.read_text(encoding="utf-8").splitlines()[-n:]
    for ln in lines:
        print(ln)


# ----------------- watch -----------------

@main.group()
def watch():
    """管理监视项（一个电影 × 多个影院 × 可选日期）。"""
    pass


def _fmt_cinema_names(cfg, cinema_ids):
    parts = []
    for cid in cinema_ids:
        cn = next((c["name"] for c in cfg["cinemas"] if c["id"] == str(cid)), None)
        if cn:
            parts.append("{} ({})".format(cn, cid))
        else:
            parts.append("(未注册 {})".format(cid))
    return " + ".join(parts)


@watch.command("list")
def watch_list():
    """列出所有监视项。

示例：tt watch list
    """
    cfg = cfg_mod.load_or_init()
    if not cfg["watches"]:
        console.print("[dim]尚无监视项。添加：tt watch add <movie_id> -c <cinema_id>[/dim]")
        return
    table = Table(title="监视项 ({} 条)".format(len(cfg["watches"])))
    table.add_column("ID", style="cyan", no_wrap=True)
    table.add_column("电影")
    table.add_column("影院")
    table.add_column("日期", justify="center")
    table.add_column("间隔", justify="center")
    table.add_column("启用", justify="center")
    table.add_column("已触发", justify="center")
    for w in cfg["watches"]:
        dates = w.get("dates")
        date_str = "不限" if not dates else "、".join(dates)
        fired = w.get("fired_cinemas") or []
        fired_str = "—"
        if fired:
            fired_str = "{}/{}".format(len(fired), len(w.get("cinemas") or []))
        table.add_row(
            w["id"],
            "{} ({})".format(w.get("movie_name") or "?", w["movie_id"]),
            _fmt_cinema_names(cfg, w.get("cinemas") or []),
            date_str,
            str(w.get("interval") or "(默认)"),
            "✓" if w.get("enabled") else "×",
            fired_str,
        )
    console.print(table)


@watch.command("add")
@click.argument("movie_id", type=int)
@click.option("--cinema", "-c", "cinemas", multiple=True,
              help="影院 ID（可多次指定）；不指定则提示先 tt cinema add")
@click.option("--date", "-d", "dates", multiple=True,
              help="限定开售日期 YYYY-MM-DD（可多次指定）；不指定=不限")
@click.option("--name", "movie_name", default=None, help="电影名（留空自动从猫眼获取）")
@click.option("--interval", type=int, default=None, help="本条独立间隔（秒）")
def watch_add(movie_id, cinemas, dates, movie_name, interval):
    """MOVIE_ID — 添加一条监视（一个电影 × 多个影院 × 可选日期）。

示例：
    tt watch add 1490607 -c 37534
    tt watch add 1490607 -c 37534 -c 2127
    tt watch add 1490607 -c 37534 -d 2026-07-29 -d 2026-07-30
    tt watch add 1490607 -c 37534 --name "蜘蛛侠：崭新之日" --interval 30
    """
    if not cinemas:
        console.print("[red]✗[/red] 至少需要一个 --cinema / -c")
        console.print("示例：tt watch add {} -c 37534".format(movie_id))
        sys.exit(1)
    cfg = cfg_mod.load_or_init()
    if movie_name is None:
        from .maoyan import fetch_movie_name
        with console.status("[cyan]正在从猫眼获取片名…[/cyan]"):
            movie_name = fetch_movie_name(movie_id)
        if movie_name:
            console.print("[dim]自动获取片名：[/dim]" + movie_name)
        else:
            movie_name = str(movie_id)
            console.print("[yellow]无法自动获取片名，将仅按 ID 匹配。[/yellow]")
    wid = cfg_mod.add_watch(cfg, movie_id, list(cinemas),
                            dates=list(dates) if dates else None,
                            name=movie_name, interval=interval)
    console.print("[green]✓[/green] 已添加监视：{}".format(wid))
    console.print("    电影 {} | 影院 {} | 日期 {}".format(
        movie_name, "、".join(cinemas),
        "不限" if not dates else "、".join(dates)))


@watch.command("remove")
@click.argument("watch_id")
def watch_remove(watch_id):
    """WATCH_ID — 删除一条监视。

示例：tt watch remove w_a1b2c3
    """
    cfg = cfg_mod.load_or_init()
    if cfg_mod.remove_watch(cfg, watch_id):
        console.print("[green]✓[/green] 已删除 {}".format(watch_id))
    else:
        console.print("[red]找不到[/red] {}".format(watch_id))


@watch.command("enable")
@click.argument("watch_id")
def watch_enable(watch_id):
    """启用某条监视。"""
    cfg_mod.set_watch_field(cfg_mod.load_or_init(), watch_id, enabled=True)
    console.print("[green]✓[/green] 已启用")


@watch.command("disable")
@click.argument("watch_id")
def watch_disable(watch_id):
    """停用某条监视（仍保留配置）。"""
    cfg_mod.set_watch_field(cfg_mod.load_or_init(), watch_id, enabled=False)
    console.print("[green]✓[/green] 已停用")


@watch.command("show")
@click.argument("watch_id")
def watch_show(watch_id):
    """WATCH_ID — 查看一条监视的完整详情。"""
    cfg = cfg_mod.load_or_init()
    w = cfg_mod.find_watch(cfg, watch_id)
    if not w:
        console.print("[red]找不到[/red] {}".format(watch_id))
        sys.exit(1)
    console.print("[bold]{}[/bold]".format(w["id"]))
    console.print("  电影    : {} ({})".format(w.get("movie_name") or "?", w["movie_id"]))
    console.print("  影院    : {}".format(_fmt_cinema_names(cfg, w.get("cinemas") or [])))
    console.print("  日期    : {}".format("不限" if not w.get("dates") else "、".join(w["dates"])))
    console.print("  间隔    : {}s".format(w.get("interval") or cfg.get("check_interval", 90)))
    console.print("  启用    : {}".format("是" if w.get("enabled") else "否"))
    fired = w.get("fired_cinemas") or []
    console.print("  已触发  : {}".format("、".join(fired) if fired else "—"))
    console.print("  最近状态: {}".format(w.get("_last_status", "—")))
    payload = w.get("_last_payload") or {}
    if payload.get("matches"):
        console.print("  当前匹配:")
        for m in payload["matches"]:
            console.print("    - {}: {} 场, {} 至 {}".format(
                m["cinema_name"], m["show_count"], m["earliest"], m["latest"]))


@watch.command("edit")
@click.argument("watch_id")
@click.option("--cinema", "-c", "cinemas", multiple=True, help="替换影院列表")
@click.option("--date", "-d", "dates", multiple=True, help="替换日期列表（留空=不限）")
@click.option("--interval", type=int, default=None, help="替换独立间隔（0 表示用全局）")
def watch_edit(watch_id, cinemas, dates, interval):
    """WATCH_ID — 改影院 / 日期 / 间隔（只改你指定的字段）。"""
    cfg = cfg_mod.load_or_init()
    w = cfg_mod.find_watch(cfg, watch_id)
    if not w:
        console.print("[red]找不到[/red] {}".format(watch_id))
        sys.exit(1)
    fields = {}
    if cinemas:
        fields["cinemas"] = list(cinemas)
    if dates:
        fields["dates"] = list(dates)
    if interval is not None:
        fields["interval"] = interval if interval > 0 else None
    if not fields:
        console.print("[yellow]未指定任何 --cinema / --date / --interval[/yellow]")
        sys.exit(1)
    cfg_mod.set_watch_field(cfg, watch_id, **fields)
    console.print("[green]✓[/green] 已更新 {}".format(watch_id))


# ----------------- cinema -----------------

@main.group()
def cinema():
    """管理影院。"""
    pass


@cinema.command("list")
def cinema_list():
    """列出已配置影院。

示例：tt cinema list
    """
    cfg = cfg_mod.load_or_init()
    if not cfg["cinemas"]:
        console.print("[dim]尚无影院。添加：tt cinema add 37534[/dim]")
        return
    table = Table(title="影院")
    table.add_column("ID", style="cyan")
    table.add_column("名称")
    table.add_column("城市")
    for c in cfg["cinemas"]:
        city = ""
        for p in list_presets().values():
            if p["id"] == c["id"]:
                city = p["city"]; break
        table.add_row(c["id"], c["name"], city or "—")
    console.print(table)


@cinema.command("add")
@click.argument("cinema_id")
@click.option("--name", "-n", default=None, help="影院名（留空用 ID 占位）")
def cinema_add(cinema_id, name):
    """CINEMA_ID — 添加一个影院（一般用 cinema add-preset 更方便）。"""
    cfg = cfg_mod.load_or_init()
    if cfg_mod.add_cinema(cfg, cinema_id, name or "影城 {}".format(cinema_id)):
        console.print("[green]✓[/green] 已添加")
    else:
        console.print("[yellow]已存在[/yellow]")


@cinema.command("remove")
@click.argument("cinema_id")
def cinema_remove(cinema_id):
    """CINEMA_ID — 删除一个影院（不会影响已有 watch）。"""
    cfg = cfg_mod.load_or_init()
    if cfg_mod.remove_cinema(cfg, cinema_id):
        console.print("[green]✓[/green] 已删除")
    else:
        console.print("[red]找不到[/red]")


@cinema.command("presets")
def cinema_presets():
    """列出所有内置影院预设。"""
    table = Table(title="内置影院预设")
    table.add_column("名称", style="cyan")
    table.add_column("ID")
    table.add_column("城市")
    table.add_column("说明", style="dim")
    for name, p in list_presets().items():
        table.add_row(name, p["id"], p["city"], p["note"])
    console.print(table)


@cinema.command("add-preset")
@click.argument("name")
def cinema_add_preset(name):
    """NAME — 添加一个内置预设影院。

示例：
    tt cinema add-preset 前滩太古里
    """
    p = list_presets().get(name)
    if not p:
        console.print("[red]预设不存在[/red]：{}".format(name))
        console.print("可用：{}".format("、".join(list_presets().keys())))
        sys.exit(1)
    cfg = cfg_mod.load_or_init()
    if cfg_mod.add_cinema(cfg, p["id"], p["name"]):
        console.print("[green]✓[/green] 已添加 {}（{}）".format(p["name"], p["id"]))
    else:
        console.print("[yellow]已存在[/yellow]")


# ----------------- films -----------------

@main.command()
@click.argument("show_type", default=1, type=click.IntRange(1, 3))
def films(show_type):
    """SHOW_TYPE — 列出猫眼在映 / 即将上映电影（最多 ~20 条）。

    SHOW_TYPE: 1=热映 / 2=即将上映 / 3=经典

示例：
    tt films          # 热映
    tt films 2        # 即将上映
    """
    from .maoyan import fetch_films_list
    label = {1: "热映", 2: "即将上映", 3: "经典"}.get(show_type, "")
    with console.status("[cyan]正在拉取猫眼{}列表…[/cyan]".format(label)):
        rows = fetch_films_list(show_type)
    if not rows:
        console.print("[dim]无数据[/dim]")
        return
    table = Table(title="猫眼{}电影 ({} 条)".format(label, len(rows)))
    table.add_column("电影 ID", style="cyan", no_wrap=True)
    table.add_column("片名")
    for r in rows:
        table.add_row(r["id"], r["name"])
    console.print(table)
    console.print("[dim]监视：tt watch add <电影 ID> -c <影院 ID>[/dim]")
    console.print("[dim]监视某条：tt watch add <电影 ID> -c <影院 ID>[/dim]")


# ----------------- config -----------------

@main.group()
def config():
    """查看/修改配置。"""
    pass


@config.command("show")
def config_show():
    """显示当前配置（隐藏敏感项）。"""
    cfg = cfg_mod.load_or_init()
    private_keys = {"_runtime", "_migrated_legacy_state", "_watch_schema_migrated", "watches"}
    t = Table(title="配置（部分，完整在 {}）".format(config_file()))
    t.add_column("Key", style="cyan")
    t.add_column("Value")
    for k in sorted(cfg.keys()):
        if k in private_keys:
            continue
        v = cfg[k]
        if isinstance(v, (dict, list)):
            v = json.dumps(v, ensure_ascii=False)
        t.add_row(k, str(v))
    console.print(t)


@config.command("get")
@click.argument("key")
def config_get(key):
    """KEY — 读一个配置项。"""
    cfg = cfg_mod.load_or_init()
    aliases = {
        "discord-webhook": "discord_webhook",
        "webhook": "discord_webhook",
        "quiet": "quiet_window",
        "phone-only": "phone_only_window",
        "interval": "check_interval",
    }
    real_key = aliases.get(key, key)
    if real_key in cfg:
        print(json.dumps(cfg[real_key], ensure_ascii=False, indent=2)
              if isinstance(cfg[real_key], (dict, list)) else cfg[real_key])
    else:
        console.print("[red]无此 key[/red]"); sys.exit(1)


@config.command("set")
@click.argument("key")
@click.argument("value")
def config_set(key, value):
    """KEY VALUE — 设一个配置项。

示例：
    tt config set discord-webhook https://discord.com/api/webhooks/...
    tt config set quiet "01:00-07:00"
    tt config set interval 60
    """
    cfg = cfg_mod.load_or_init()
    aliases = {
        "discord-webhook": "discord_webhook",
        "webhook": "discord_webhook",
        "quiet": "quiet_window",
        "phone-only": "phone_only_window",
        "interval": "check_interval",
    }
    real_key = aliases.get(key, key)
    if real_key in ("check_interval", "alert_duration_sec", "heartbeat_interval_sec"):
        value = int(value)
    elif value in ("true", "false"):
        value = (value == "true")
    cfg[real_key] = value
    cfg_mod.save(cfg)
    console.print("[green]✓[/green] {} = {}".format(real_key, value))


@config.command("unset")
@click.argument("key")
def config_unset(key):
    """KEY — 移除一个配置项。"""
    cfg = cfg_mod.load_or_init()
    cfg.pop(key, None)
    cfg_mod.save(cfg)
    console.print("[green]✓[/green] 已移除")


@config.command("path")
def config_path():
    """打印配置文件路径。"""
    print(config_file())


# ----------------- test -----------------

@main.command()
@click.argument("which", default="all", type=click.Choice(["all", "discord", "macos"]))
def test(which):
    """测试通知链路（默认全部）。"""
    cfg = cfg_mod.load_or_init()
    webhook = cfg.get("discord_webhook")
    if which in ("all", "discord"):
        ok = notify.notify_discord(
            webhook, "ticket-tracker 测试",
            "如果你手机上收到这条，说明 Discord 推送正常 ✅")
        console.print(("✓" if ok else "✗") + " Discord")
    if which in ("all", "macos"):
        if sys.platform == "darwin":
            notify.notify_macos("ticket-tracker 测试",
                                "测试电脑通知（macOS）",
                                sound=False, duration=2)
            console.print("✓ macOS 弹窗已发送")
        else:
            console.print("[dim]· 非 macOS，跳过电脑通知测试[/dim]")


# ----------------- doctor -----------------

@main.command()
def doctor():
    """自检：依赖 / 网络 / 配置完整性。"""
    console.print("[bold]ticket-tracker 自检[/bold]")
    try:
        import rich
        from importlib.metadata import version
        console.print("[green]✓[/green] rich {}".format(version("rich")))
    except Exception as e:
        console.print("[red]✗[/red] rich: {}".format(e))
    try:
        import click
        from importlib.metadata import version
        console.print("[green]✓[/green] click {}".format(version("click")))
    except Exception as e:
        console.print("[red]✗[/red] click: {}".format(e))
    try:
        import textual
        from importlib.metadata import version
        console.print("[green]✓[/green] textual {}".format(version("textual")))
    except Exception as e:
        console.print("[red]✗[/red] textual: {}".format(e))
    try:
        urllib.request.urlopen("https://m.maoyan.com/ajax/cinemaDetail?cinemaId=37534",
                               timeout=10).read()
        console.print("[green]✓[/green] 网络可达 m.maoyan.com")
    except Exception as e:
        console.print("[red]✗[/red] 网络: {}".format(e))
    cfg = cfg_mod.load_or_init()
    console.print("[green]✓[/green] 配置文件：{}".format(config_file()))
    console.print("[green]✓[/green] watches: {} 条".format(len(cfg["watches"])))
    console.print("[green]✓[/green] Discord webhook: {}".format("已配置" if cfg.get("discord_webhook") else "未配置"))
    import shutil
    has_caffeinate = shutil.which("caffeinate") is not None
    console.print("[green]✓[/green] caffeinate（防休眠）: {}".format(
        "可用" if has_caffeinate else "不可用（仅 macOS）"))


if __name__ == "__main__":
    main()