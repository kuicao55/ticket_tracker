"""配置管理：XDG 路径下的 JSON 读写、首次创建、迁移旧格式。"""

import json
import re
import time
import uuid
from datetime import datetime
from pathlib import Path

from .paths import config_file, state_dir

CONFIG_VERSION = 2
WINDOW_RE = re.compile(r"^([01]?\d|2[0-3]):([0-5]\d)-([01]?\d|2[0-3]):([0-5]\d)$")


def default_config():
    """v2 配置 schema。"""
    return {
        "version": CONFIG_VERSION,
        "discord_webhook": None,
        "quiet_window": "01:00-06:00",
        "phone_only_window": "06:00-09:00",
        "check_interval": 90,
        "alert_duration_sec": 60,
        "heartbeat_interval_sec": 3600,
        "cinemas": [],
        "watches": [],
        "_runtime": {},
    }


def _parse_window(s):
    """'01:00-06:00' → (1, 0, 6, 0)。"""
    if not isinstance(s, str):
        raise ValueError("时段必须是 'HH:MM-HH:MM' 字符串")
    m = WINDOW_RE.match(s.strip())
    if not m:
        raise ValueError("时段格式错误，应像 '01:00-06:00'")
    return tuple(int(x) for x in m.groups())


def current_mode(quiet_window, phone_only_window, now=None):
    h = (now or datetime.now()).hour
    qs, _, qe, _ = _parse_window(quiet_window)
    ps, _, pe, _ = _parse_window(phone_only_window)
    if qs <= h < qe:
        return "quiet"
    if ps <= h < pe:
        return "phone_only"
    return "normal"


def load_or_init():
    p = config_file()
    if not p.exists():
        cfg = default_config()
        save(cfg)
        return cfg

    try:
        with open(p, encoding="utf-8") as f:
            cfg = json.load(f)
    except json.JSONDecodeError:
        backup = p.with_suffix(".broken.json")
        p.rename(backup)
        cfg = default_config()
        save(cfg)
        return cfg

    # 补字段
    if cfg.get("version") != CONFIG_VERSION:
        cfg["version"] = CONFIG_VERSION
    for k, v in default_config().items():
        cfg.setdefault(k, v)
    cfg.setdefault("_runtime", {})

    _migrate_legacy_state(cfg)
    _migrate_watch_schema(cfg)
    return cfg


def save(cfg):
    p = config_file()
    p.parent.mkdir(parents=True, exist_ok=True)
    tmp = p.with_suffix(".json.tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(cfg, f, ensure_ascii=False, indent=2)
    tmp.replace(p)


def _migrate_legacy_state(cfg):
    """旧 monitor_spiderman.py 的 state.json 迁移。"""
    legacy = Path(__file__).resolve().parent.parent.parent / "state.json"
    if not legacy.exists() or cfg.get("_migrated_legacy_state"):
        return
    try:
        with open(legacy, encoding="utf-8") as f:
            old = json.load(f)
    except Exception:
        return
    for watch in cfg.get("watches", []):
        key = "movie_{}".format(watch.get("movie_id"))
        if key in old and old[key].get("presale_open"):
            watch["presale_fired"] = True
            watch.setdefault("last_alert_at", old[key].get("detected_at"))
    backup = legacy.with_suffix(".json.bak")
    if not backup.exists():
        try:
            legacy.rename(backup)
        except OSError:
            pass
    cfg["_migrated_legacy_state"] = True
    save(cfg)


def _migrate_watch_schema(cfg):
    """v1 → v2：watch.cinema_id 变成 watch.cinemas[]，并补全 cinemas。"""
    if cfg.get("_watch_schema_migrated"):
        return
    for w in cfg.get("watches", []):
        if "cinema_id" in w and "cinemas" not in w:
            w["cinemas"] = [str(w.pop("cinema_id"))]
        elif "cinemas" not in w:
            w["cinemas"] = []
        # dates 字段保证存在
        w.setdefault("dates", None)   # None = 不限
        # movie_name 自动揣测
        if not w.get("movie_name"):
            try:
                from .maoyan import fetch_movie_name
                w["movie_name"] = fetch_movie_name(w["movie_id"])
            except Exception:
                w["movie_name"] = None
    cfg["_watch_schema_migrated"] = True
    save(cfg)


# ----------------- 影院操作 -----------------

def find_cinema(cfg, cinema_id):
    return next((c for c in cfg["cinemas"] if c["id"] == str(cinema_id)), None)


def add_cinema(cfg, cinema_id, name=None):
    if find_cinema(cfg, cinema_id):
        return False
    cfg["cinemas"].append({
        "id": str(cinema_id),
        "name": name or "影城 {}".format(cinema_id),
        "builtin": False,
    })
    save(cfg)
    return True


def remove_cinema(cfg, cinema_id):
    before = len(cfg["cinemas"])
    cfg["cinemas"] = [c for c in cfg["cinemas"] if c["id"] != str(cinema_id)]
    save(cfg)
    return len(cfg["cinemas"]) < before


# ----------------- 监视项操作（v2 schema） -----------------

def list_watches(cfg):
    return list(cfg.get("watches", []))


def find_watch(cfg, watch_id):
    return next((w for w in cfg["watches"] if w["id"] == watch_id), None)


def add_watch(cfg, movie_id, cinemas, dates=None, name=None, interval=None):
    """新加一条监视。
    cinemas: 可迭代；自动注册缺失影院。
    dates: 可迭代 ["YYYY-MM-DD"] 或 None（不限）
    """
    cinemas = [str(c) for c in cinemas] if cinemas else []
    for cid in cinemas:
        if not find_cinema(cfg, cid):
            add_cinema(cfg, cid, name="影城 {}".format(cid))
    watch_id = "w_" + uuid.uuid4().hex[:6]
    cfg["watches"].append({
        "id": watch_id,
        "movie_id": int(movie_id),
        "movie_name": name,
        "cinemas": cinemas,
        "dates": sorted(set(dates)) if dates else None,
        "interval": interval,
        "enabled": True,
        "presale_fired": False,
        "created_at": datetime.now().strftime("%Y-%m-%dT%H:%M:%S"),
    })
    save(cfg)
    return watch_id


def remove_watch(cfg, watch_id):
    before = len(cfg["watches"])
    cfg["watches"] = [w for w in cfg["watches"] if w["id"] != watch_id]
    save(cfg)
    return len(cfg["watches"]) < before


def set_watch_field(cfg, watch_id, **fields):
    w = find_watch(cfg, watch_id)
    if not w:
        return None
    if "cinemas" in fields and fields["cinemas"] is not None:
        fields["cinemas"] = [str(c) for c in fields["cinemas"]]
    if "dates" in fields and fields["dates"] is not None:
        fields["dates"] = sorted(set(fields["dates"]))
    w.update(fields)
    save(cfg)
    return w


def mark_presale_fired(cfg, watch_id, cinema_id):
    w = find_watch(cfg, watch_id)
    if not w:
        return
    w["presale_fired"] = True
    fired_on = w.setdefault("fired_cinemas", [])
    if str(cinema_id) not in fired_on:
        fired_on.append(str(cinema_id))
    w["last_alert_at"] = datetime.now().strftime("%Y-%m-%dT%H:%M:%S")
    save(cfg)


# ----------------- 运行期统计 -----------------

def set_runtime(cfg, **fields):
    r = cfg.setdefault("_runtime", {})
    r.update(fields)
    r.setdefault("started_at", time.time())
    save(cfg)


def get_runtime(cfg):
    return cfg.get("_runtime", {}) or {}
