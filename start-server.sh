#!/usr/bin/env bash
# 仓库根目录快捷入口 → rs/start-server.sh
exec "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/rs/start-server.sh" "$@"
