#!/bin/sh
# container-agent.sh - runs INSIDE the test container as a child of PID 1
# (started by boot-container.sh just before it execs systemd). The host
# cannot nsenter into the container (setns is blocked in this sandbox),
# so this agent is the only way to run commands inside: the host writes
# /mnt/build/agent-cmd.sh (bind-mounted from the host build dir), the
# agent executes it as root in the container, and writes stdout/stderr
# to /mnt/build/agent-out.txt plus the exit code to /mnt/build/agent-exit.
# One command at a time; the host waits for agent-exit to appear.
CMD=/mnt/build/agent-cmd.sh
OUT=/mnt/build/agent-out.txt
EXITF=/mnt/build/agent-exit
LOG=/mnt/build/agent-log.txt

echo "agent started pid $$ at $(date -u +%FT%TZ)" >> "$LOG"
while true; do
  if [ -f "$CMD" ]; then
    echo "agent running $(cat "$CMD" | head -c 200)" >> "$LOG"
    rm -f "$OUT" "$EXITF"
    sh "$CMD" > "$OUT" 2>&1
    echo "$?" > "$EXITF"
    rm -f "$CMD"
    echo "agent done exit $(cat "$EXITF")" >> "$LOG"
  fi
  sleep 1
done
