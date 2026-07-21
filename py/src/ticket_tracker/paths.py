"""XDG 路径解析 + macOS 兼容兜底。"""

import os
from pathlib import Path

APP_NAME = "ticket-tracker"


def _xdg(env_var, default):
    val = os.environ.get(env_var)
    return Path(val).expanduser() if val else Path(default).expanduser()


def config_dir():
    """XDG_CONFIG_HOME 或 macOS 兜底 ~/.config/<APP_NAME>。"""
    base = _xdg("XDG_CONFIG_HOME", "~/.config")
    p = base / APP_NAME
    p.mkdir(parents=True, exist_ok=True)
    return p


def state_dir():
    """运行期状态/日志位置。XDG_STATE_HOME 或 macOS 兜底 ~/.local/state。"""
    base = _xdg("XDG_STATE_HOME", "~/.local/state")
    p = base / APP_NAME
    p.mkdir(parents=True, exist_ok=True)
    return p


def config_file():
    return config_dir() / "config.json"


def log_file():
    return state_dir() / "ticket-tracker.log"


def pid_file():
    return state_dir() / "ticket-tracker.pid"
