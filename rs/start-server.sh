#!/usr/bin/env bash
# 兼容入口：真正的启动逻辑在仓库根目录 start-ticket-tracker.sh。
#
# 保留本文件是为了兼容旧文档 / 脚本里的 `rs/start-server.sh` 用法；
# Release 用户只需要 tt-server + start-ticket-tracker.sh，不需要 rs/ 目录。
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$HERE/../start-ticket-tracker.sh" "$@"
