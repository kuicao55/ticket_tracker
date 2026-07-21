#!/usr/bin/env bash
# 开发期一键 build + 起 TUI
# 用法： ./dev.sh           # 默认 release build + 启动
#       ./dev.sh debug      # debug 构建（更快，含调试符号）
#       ./dev.sh doctor     # 不进 TUI，只跑医生

set -euo pipefail
cd "$(dirname "$0")"

PROFILE="release"
case "${1:-}" in
  debug) PROFILE="dev" ;;
  doctor) PROFILE="release"; DOCTOR=1 ;;
esac

if [ "$PROFILE" = "release" ]; then
  cargo build --release --quiet
  BIN=./target/release/tt
else
  cargo build --quiet
  BIN=./target/debug/tt
fi

if [ "${DOCTOR:-}" = "1" ]; then
  exec "$BIN" doctor
fi

echo "==> 启动 $BIN"
exec "$BIN" start
