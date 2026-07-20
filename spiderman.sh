#!/bin/bash
# 蜘蛛侠预售监测 —— 后台启停控制脚本
#
#   ./spiderman.sh start      后台启动监测（每 90 秒检查一次）
#   ./spiderman.sh start 30   自定义检查间隔为 30 秒
#   ./spiderman.sh stop       停止监测
#   ./spiderman.sh status     查看运行状态
#   ./spiderman.sh log        实时查看日志（Ctrl+C 退出查看）
#   ./spiderman.sh test       用一部已开售电影测试报警链路
#   ./spiderman.sh demo       实景演示（盯一部已开售电影，触发完整报警）
#
# 启动时会：①开启防休眠(caffeinate) ②向 Discord 发"已启动"通知
# 运行中：每小时向 Discord 发一次"运行正常"状态通知
# 停止时会：①解除防休眠 ②向 Discord 发"已停止"通知

set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PY="$DIR/monitor_spiderman.py"
PID_FILE="$DIR/monitor.pid"
CAF_PID_FILE="$DIR/caffeinate.pid"
LOG_FILE="$DIR/monitor.log"
INTERVAL="${2:-90}"          # 默认 90 秒

# 找一个可用的 python3
PYBIN="$(command -v python3 || true)"
if [ -z "$PYBIN" ]; then
  echo "❌ 未找到 python3，请先安装。"; exit 1
fi

is_running() {
  [ -f "$PID_FILE" ] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null
}

# 开启防休眠：让 caffeinate 绑定监测进程(-w)，监测在则电脑不睡，监测退出 caffeinate 自动结束
start_caffeinate() {
  local mon_pid="$1"
  if command -v caffeinate >/dev/null 2>&1; then
    caffeinate -i -s -w "$mon_pid" &
    echo $! > "$CAF_PID_FILE"
    echo "☕ 已开启防休眠（电脑在监测期间不会自动睡眠）"
  else
    echo "ℹ️  未找到 caffeinate，跳过防休眠。"
  fi
}

stop_caffeinate() {
  if [ -f "$CAF_PID_FILE" ]; then
    kill "$(cat "$CAF_PID_FILE")" 2>/dev/null || true
    rm -f "$CAF_PID_FILE"
    echo "☕ 已解除防休眠。"
  fi
}

cmd_start() {
  if is_running; then
    echo "⚠️  监测已在运行 (PID $(cat "$PID_FILE"))。如需重启请先 ./spiderman.sh stop"
    exit 0
  fi
  echo "🚀 启动监测：影城 37534《蜘蛛侠：崭新之日》，每 ${INTERVAL} 秒检查一次…"
  # 脚本内部已负责写 monitor.log，这里把标准输出丢弃避免日志重复；仅保留报错兜底
  nohup "$PYBIN" "$PY" --loop "$INTERVAL" >/dev/null 2>>"$LOG_FILE" &
  echo $! > "$PID_FILE"
  sleep 1
  if is_running; then
    start_caffeinate "$(cat "$PID_FILE")"
    echo "✅ 已在后台运行 (PID $(cat "$PID_FILE"))"
    echo "   已发送 Discord 启动通知，运行中每小时汇报一次状态。"
    echo "   查看日志：./spiderman.sh log    停止：./spiderman.sh stop"
  else
    echo "❌ 启动失败，请查看日志：$LOG_FILE"; exit 1
  fi
}

cmd_stop() {
  if is_running; then
    PID="$(cat "$PID_FILE")"
    # 发 SIGTERM，Python 会先推送 Discord 结束通知再退出，给它最多 6 秒
    kill "$PID" 2>/dev/null || true
    for _ in 1 2 3 4 5 6; do
      kill -0 "$PID" 2>/dev/null || break
      sleep 1
    done
    kill -9 "$PID" 2>/dev/null || true
    rm -f "$PID_FILE"
    stop_caffeinate
    echo "🛑 已停止监测 (原 PID $PID)"
  else
    echo "ℹ️  监测当前未运行。"
    rm -f "$PID_FILE"
    stop_caffeinate
  fi
}

cmd_status() {
  if is_running; then
    echo "🟢 运行中 (PID $(cat "$PID_FILE"))"
    if [ -f "$CAF_PID_FILE" ] && kill -0 "$(cat "$CAF_PID_FILE")" 2>/dev/null; then
      echo "   ☕ 防休眠：已开启"
    else
      echo "   ☕ 防休眠：未开启"
    fi
    echo "   最近日志："
    tail -n 5 "$LOG_FILE" 2>/dev/null | sed 's/^/     /'
  else
    echo "🔴 未运行"
  fi
}

cmd_log() {
  echo "📜 实时日志 (Ctrl+C 退出查看，不会停止监测)："
  touch "$LOG_FILE"
  tail -n 20 -f "$LOG_FILE"
}

cmd_test() {
  echo "🧪 测试报警链路（用已开售电影『功夫女足』模拟预售开启）…"
  "$PYBIN" "$PY" --test 1500469
  rm -f "$DIR/state.json"
  echo "🧹 测试状态已清理。若刚才弹出了通知/响了声音/打开了购票页，说明报警正常。"
}

# 实景演示：让后台循环真的去盯一部【已开售】的电影，亲眼看到完整监测→报警流程。
# 默认盯《晒后假日》(1467421)，其预售只有 07-27（约一周后），最贴近真实场景。
# 前台运行，每 20 秒查一次，按 Ctrl+C 结束演示。
cmd_demo() {
  local mid="${2:-1467421}"
  echo "🎬 演示模式：后台循环盯已开售电影 movieId=${mid}（每 20 秒一次，Ctrl+C 结束）"
  echo "   —— 首次会真实触发弹窗/声音/语音/打开购票页，之后每次只记录（防刷屏）。"
  echo "----------------------------------------------------------------------"
  trap 'echo; echo "🧹 演示结束，清理演示状态…"; rm -f "$DIR/state.json"; exit 0' INT
  "$PYBIN" "$PY" --loop 20 --test "$mid"
}

case "${1:-}" in
  start)  cmd_start ;;
  stop)   cmd_stop ;;
  restart) cmd_stop; sleep 1; cmd_start ;;
  status) cmd_status ;;
  log)    cmd_log ;;
  test)   cmd_test ;;
  demo)   cmd_demo "$@" ;;
  *)
    echo "用法: ./spiderman.sh {start [间隔秒] | stop | restart | status | log | test | demo [movieId]}"
    exit 1
    ;;
esac
