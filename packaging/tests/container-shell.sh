#!/bin/bash
# container-shell.sh - boot the test container and run commands inside it
# through the agent (same mechanism as test-container.sh), for debugging.
# Usage:
#   ./tests/container-shell.sh 'systemctl status iris-api'
#   ./tests/container-shell.sh            # reads commands from stdin, one per line
# The container is stopped when this script exits. It shares the host
# network namespace, so services are reachable on 127.0.0.1 from here.
set -uo pipefail
cd "$(dirname "$0")/.."

ROOT=${CONTAINER:-/dev/shm/iris-test}
BUILD=$PWD/build
[ -d "$ROOT" ] || { echo "container $ROOT is missing"; exit 1; }

init_pid() { pgrep -f '^/sbin/init$' | head -1 || true; }
cleanup() {
  pid=$(init_pid)
  [ -n "$pid" ] && kill -9 "$pid" 2>/dev/null || true
  sleep 1
}
trap cleanup EXIT

rm -f "$BUILD/agent-cmd.sh" "$BUILD/agent-out.txt" "$BUILD/agent-exit" "$BUILD/agent-log.txt"
cp tests/container-agent.sh "$ROOT/usr/local/bin/container-agent.sh" 2>/dev/null || true
chmod 755 "$ROOT/usr/local/bin/container-agent.sh" 2>/dev/null || true

setsid unshare --mount --pid --fork --uts /bin/sh tests/boot-container.sh \
  >/tmp/container-boot.log 2>&1 &
BOOT_PID=$!

for _ in $(seq 1 60); do
  if [ -f "$BUILD/agent-log.txt" ] && grep -q "agent started" "$BUILD/agent-log.txt"; then
    break
  fi
  kill -0 "$BOOT_PID" 2>/dev/null || { echo "boot process died"; cat /tmp/container-boot.log; exit 1; }
  sleep 1
done
grep -q "agent started" "$BUILD/agent-log.txt" 2>/dev/null || { echo "agent did not start"; cat /tmp/container-boot.log; exit 1; }

run_in() {
  local cmd="$1" i
  rm -f "$BUILD/agent-out.txt" "$BUILD/agent-exit"
  printf '%s\n' "$cmd" > "$BUILD/agent-cmd.sh"
  for i in $(seq 1 180); do
    [ -f "$BUILD/agent-exit" ] && break
    sleep 1
  done
  [ -f "$BUILD/agent-exit" ] || { echo "run_in timed out: $cmd" >&2; return 1; }
  cat "$BUILD/agent-out.txt"
  return "$(cat "$BUILD/agent-exit")"
}

# Wait for systemd (up to 2 minutes); a degraded state is fine.
for _ in $(seq 1 60); do
  state=$(run_in "systemctl is-system-running" 2>/dev/null || true)
  if printf '%s' "$state" | grep -qE "running|degraded"; then
    echo "systemd is $state" >&2
    break
  fi
  sleep 2
done

if [ $# -gt 0 ]; then
  run_in "$*"
  exit $?
fi

echo "container shell ready (one command per line, empty line quits)" >&2
while IFS= read -r line; do
  [ -z "$line" ] && break
  run_in "$line" || echo "exit=$?" >&2
done
