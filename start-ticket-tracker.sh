#!/usr/bin/env bash
# 仓库根目录入口 → rs/start-server.sh
#
# iGlance 绑定可直接使用：
#   /path/to/ticket_tracker/start-ticket-tracker.sh start --manifest
#
# 只打印 manifest（不启动服务）：
#   /path/to/ticket_tracker/start-ticket-tracker.sh manifest
exec "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/rs/start-server.sh" "$@"
