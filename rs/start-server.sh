#!/usr/bin/env bash
# 快速启动/停止 ticket-tracker 无头主服务（tt-server）
#
# 用法:
#   ./start-server.sh                 # 后台启动（默认）
#   ./start-server.sh start --open    # 启动并打开浏览器
#   ./start-server.sh start --foreground  # 前台运行（调试）
#   ./start-server.sh status
#   ./start-server.sh logs
#   ./start-server.sh stop
#   ./start-server.sh restart
#   ./start-server.sh manifest         # 只打印 iGlance manifest，不启动服务
#   ./start-server.sh start --manifest # 启动并就绪后打印 iGlance manifest
#
# 可用环境变量:
#   TT_BIND        监听地址，默认 127.0.0.1:8787
#   TT_DB          SQLite 路径，默认 ~/.local/state/ticket-tracker/ticket-tracker.db
#   TT_SERVER_BIN  指定二进制路径
#   TT_PID_FILE    脚本 pid 文件
#   TT_LOG_FILE    日志文件
#
# 远程服务器：设置 TT_BIND=0.0.0.0:8787，然后用 SSH 隧道访问。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

BIND="${TT_BIND:-127.0.0.1:8787}"
DB="${TT_DB:-$HOME/.local/state/ticket-tracker/ticket-tracker.db}"
WEB_DIR="${TT_WEB_DIR:-$ROOT/web}"
PID_FILE="${TT_PID_FILE:-$HOME/.local/state/ticket-tracker/tt-server.pid}"
LOG_FILE="${TT_LOG_FILE:-$HOME/.local/state/ticket-tracker/tt-server.log}"

mkdir -p "$(dirname "$PID_FILE")" "$(dirname "$DB")" "$(dirname "$LOG_FILE")"

HOST="${BIND%:*}"
PORT="${BIND##*:}"
case "$HOST" in
  0.0.0.0|::|"[::]"|"") DISPLAY_HOST="127.0.0.1" ;;
  *) DISPLAY_HOST="$HOST" ;;
esac
URL="http://${DISPLAY_HOST}:${PORT}"

CMD="${1:-start}"
shift || true
OPEN=0
FOREGROUND=0
FORCE_BUILD=0
PRINT_MANIFEST=0
for arg in "$@"; do
  case "$arg" in
    --open) OPEN=1 ;;
    --foreground) FOREGROUND=1 ;;
    --build) FORCE_BUILD=1 ;;
    --manifest) PRINT_MANIFEST=1 ;;
    -h|--help|help) CMD="help" ;;
    *) echo "未知参数: $arg" >&2; CMD="help" ;;
  esac
done

usage() {
  cat <<'USAGE'
用法: ./start-server.sh [start|stop|restart|status|logs|manifest|open] [--open] [--foreground] [--build] [--manifest]

  start        后台启动主服务（默认）
  stop         停止主服务
  restart      重启
  status       查看 pid / health
  logs         实时跟踪日志
  manifest     只打印 iGlance manifest（不启动服务）
  open         在浏览器打开面板

  start --manifest  就绪后输出 IGLANCE_SERVICE_MANIFEST 行，供 iGlance 绑定

环境变量:
  TT_BIND=127.0.0.1:8787
  TT_DB=~/.local/state/ticket-tracker/ticket-tracker.db
  TT_SERVER_BIN=/path/to/tt-server
USAGE
}

resolve_bin() {
  if [ -n "${TT_SERVER_BIN:-}" ]; then
    BIN="$TT_SERVER_BIN"
  elif [ -x "$ROOT/target/release/tt-server" ]; then
    BIN="$ROOT/target/release/tt-server"
  elif command -v tt-server >/dev/null 2>&1; then
    BIN="$(command -v tt-server)"
  else
    BIN="$ROOT/target/release/tt-server"
  fi
}

fallback_manifest() {
  local version
  version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" 2>/dev/null | head -n1)"
  [ -n "$version" ] || version="0.0.0"
  printf 'IGLANCE_SERVICE_MANIFEST {"schema":"iglance.service.v1","service":{"id":"ticket_tracker","name":"Ticket Tracker","version":"%s"},"base_url":"%s","auth":{"type":"header","header":"X-Gofer-Token"},"endpoints":{"health":"/health","panel":"/","status":"/api/v1/service","memory":"/api/v1/service/memory","pause":"/api/v1/service/pause","resume":"/api/v1/service/resume","toggle":"/api/v1/service/toggle","stop":"/api/v1/service/stop"}}\n' "$version" "$URL"
}

print_manifest() {
  resolve_bin
  local payload
  if [ -x "$BIN" ] && payload="$("$BIN" manifest --base-url "$URL" 2>/dev/null)" && [ -n "$payload" ]; then
    printf '%s\n' "$payload"
  else
    # 二进制还没构建（或旧版本不支持 manifest）时也能给出 manifest，
    # 方便首次绑定；启动后仍以实际二进制支持的接口为准。
    fallback_manifest
  fi
}

build_if_needed() {
  resolve_bin
  if [ "$FORCE_BUILD" = "1" ] || [ ! -x "$BIN" ]; then
    echo "==> 构建 tt-server（release）..."
    cargo build --release --locked --bin tt-server
    BIN="$ROOT/target/release/tt-server"
  fi
}

pid_alive() {
  local pid="${1:-}"
  [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null
}

read_pid() {
  [ -f "$PID_FILE" ] && tr -d ' \n' < "$PID_FILE" || true
}

health_ok() {
  curl -fsS --max-time 1 "$URL/api/v1/health" >/dev/null 2>&1
}

print_url() {
  echo ""
  echo "  Web 面板: $URL"
  echo "  健康检查: $URL/api/v1/health"
  echo "  日志:     $LOG_FILE"
  echo "  停止:     $0 stop"
  echo ""
  if [ "$HOST" != "127.0.0.1" ] && [ "$HOST" != "localhost" ]; then
    echo "  远程访问（在本机执行）："
    echo "    ssh -L ${PORT}:127.0.0.1:${PORT} user@your-server"
    echo "    然后打开 http://127.0.0.1:${PORT}"
    echo ""
  fi
}

maybe_open() {
  [ "$OPEN" = "1" ] || return 0
  if command -v open >/dev/null 2>&1; then
    open "$URL"
  elif command -v xdg-open >/dev/null 2>&1; then
    xdg-open "$URL" >/dev/null 2>&1 &
  else
    echo "（未找到 open / xdg-open，请手动打开 ${URL}）"
  fi
}

start_server() {
  local pid
  pid="$(read_pid)"
  if pid_alive "$pid"; then
    echo "✓ tt-server 已在运行 (pid=$pid)"
    print_url
    maybe_open
    [ "$PRINT_MANIFEST" = "1" ] && print_manifest
    return 0
  fi
  if health_ok; then
    echo "✓ $URL 已经有服务在响应（可能是手动启动的 tt-server）"
    print_url
    maybe_open
    [ "$PRINT_MANIFEST" = "1" ] && print_manifest
    return 0
  fi

  build_if_needed

  local envs=(TT_BIND="$BIND" TT_DB="$DB" TT_PID_FILE="$PID_FILE" TT_LOG_FILE="$LOG_FILE")
  envs+=("TT_CALLBACK_URL=${TT_CALLBACK_URL:-http://${DISPLAY_HOST}:${PORT}/api/v1/agent/lock-result}")
  if [ -d "$WEB_DIR" ]; then
    envs+=(TT_WEB_DIR="$WEB_DIR")
  fi

  if [ "$FOREGROUND" = "1" ]; then
    if [ "$PRINT_MANIFEST" = "1" ]; then
      echo "✗ --manifest 需要后台启动（前台模式无法在 exec 后继续输出）" >&2
      return 1
    fi
    echo "==> 前台启动：$BIN"
    echo "    db=$DB  bind=$BIND"
    exec env "${envs[@]}" "$BIN"
  fi

  echo "==> 启动 tt-server ..."
  echo "    bin=$BIN"
  echo "    db=$DB"
  echo "    bind=$BIND"
  nohup env "${envs[@]}" "$BIN" </dev/null >>"$LOG_FILE" 2>&1 &
  pid=$!
  echo "$pid" > "$PID_FILE"

  # 健康检查最多等待 10 秒（iGlance 启动超时建议 >= 15 秒）。
  local deadline=$((SECONDS + 10))
  while [ "$SECONDS" -lt "$deadline" ]; do
    if health_ok; then
      echo "✓ tt-server 已启动 (pid=$pid)"
      print_url
      maybe_open
      [ "$PRINT_MANIFEST" = "1" ] && print_manifest
      return 0
    fi
    if ! pid_alive "$pid"; then
      echo "✗ tt-server 启动失败，最近日志：" >&2
      tail -n 30 "$LOG_FILE" >&2 || true
      rm -f "$PID_FILE"
      return 1
    fi
    sleep 0.25
  done

  echo "✗ 启动超时（10s 内 health 未响应），最近日志：" >&2
  tail -n 30 "$LOG_FILE" >&2 || true
  return 1
}

stop_server() {
  local pid
  pid="$(read_pid)"
  if ! pid_alive "$pid"; then
    echo "tt-server 未运行（清理 stale pid）"
    rm -f "$PID_FILE"
    return 0
  fi
  echo "==> 停止 tt-server pid=$pid ..."
  kill -TERM "$pid" 2>/dev/null || true
  local i
  for i in $(seq 1 40); do
    if ! pid_alive "$pid"; then
      rm -f "$PID_FILE"
      echo "✓ 已停止"
      return 0
    fi
    sleep 0.25
  done
  echo "==> 进程未退出，强制 kill -9"
  kill -KILL "$pid" 2>/dev/null || true
  rm -f "$PID_FILE"
}

status_server() {
  local pid
  pid="$(read_pid)"
  if pid_alive "$pid"; then
    echo "tt-server 运行中 (pid=$pid)"
  else
    echo "tt-server 未运行（pid 文件：${PID_FILE}）"
  fi
  if health_ok; then
    echo "health: $URL/api/v1/health"
    curl -s "$URL/api/v1/health"
    echo ""
  else
    echo "health: 无响应（$URL/api/v1/health）"
  fi
}

case "$CMD" in
  start) start_server ;;
  stop) stop_server ;;
  restart) stop_server; start_server ;;
  status) status_server ;;
  manifest) print_manifest ;;
  logs)
    [ -f "$LOG_FILE" ] || { echo "日志不存在: $LOG_FILE"; exit 1; }
    tail -n 100 -f "$LOG_FILE"
    ;;
  open) maybe_open ;;
  help) usage ;;
  *) usage; exit 1 ;;
esac
