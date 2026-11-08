#!/bin/bash
# test-container.sh - real install test of the .deb in a Debian 12 container.
# Host side: boots the container with unshare (systemd-nspawn cannot mount
# sysfs in this sandbox, and setns is blocked, so no nsenter/machinectl),
# installs the .deb non-interactively, and runs tests/check-inside.sh.
# Commands run inside the container through the agent started by
# tests/boot-container.sh: the host writes build/agent-cmd.sh, the agent
# executes it as root in the container and writes build/agent-out.txt
# plus build/agent-exit.
# Usage: ./tests/test-container.sh
# The test domain is iris-test.local (no real DNS; Caddy cannot issue a
# public certificate for it, so the HTTPS check uses a temporary
# "tls internal" override, documented in check-inside.sh).
# The container shares the host network namespace (CAP_NET_ADMIN is
# stripped in this sandbox, so a private net ns cannot bring loopback
# up); its services bind the same ports they would in isolation.
set -euo pipefail
cd "$(dirname "$0")/.."

ROOT=${CONTAINER:-/dev/shm/iris-test}
DEB=$PWD/build/iris-mail-stack_$(cat VERSION)_amd64.deb
BUILD=$PWD/build

[ -d "$ROOT" ] || { echo "container $ROOT is missing (run debootstrap first)"; exit 1; }
[ -f "$DEB" ] || { echo ".deb $DEB is missing (run ./build.sh first)"; exit 1; }

init_pid() { pgrep -f '^/sbin/init$' | head -1 || true; }

cleanup() {
  pid=$(init_pid)
  [ -n "$pid" ] && kill -9 "$pid" 2>/dev/null || true
  sleep 2
}
trap cleanup EXIT

rm -f "$BUILD/agent-cmd.sh" "$BUILD/agent-out.txt" "$BUILD/agent-exit" "$BUILD/agent-log.txt"
cp tests/check-inside.sh "$ROOT/root/check-inside.sh"
chmod 755 "$ROOT/root/check-inside.sh"
cp tests/container-agent.sh "$ROOT/usr/local/bin/container-agent.sh"
chmod 755 "$ROOT/usr/local/bin/container-agent.sh"
# Test-only: relax the mount-namespace hardening in the container (see
# tests/relax-namespaces.conf). The sandbox cannot create a mount
# namespace at all, so the shipped units would exit 226/NAMESPACE
# here. Installed before boot so the postinst starts the services
# with the relaxation already in effect. The .deb itself is untouched.
for unit in iris-api iris-stalwart iris-caddy iris-cleanup iris-cert-sync; do
  dir="$ROOT/etc/systemd/system/$unit.service.d"
  mkdir -p "$dir"
  cp tests/relax-namespaces.conf "$dir/relax-namespaces.conf"
done

echo "== booting the container"
setsid unshare --mount --pid --fork --uts /bin/sh tests/boot-container.sh \
  >/tmp/container-boot.log 2>&1 &
BOOT_PID=$!

echo "== waiting for the in-container agent"
for _ in $(seq 1 60); do
  if [ -f "$BUILD/agent-log.txt" ] && grep -q "agent started" "$BUILD/agent-log.txt"; then
    break
  fi
  kill -0 "$BOOT_PID" 2>/dev/null || { echo "boot process died"; cat /tmp/container-boot.log; exit 1; }
  sleep 1
done
grep -q "agent started" "$BUILD/agent-log.txt" 2>/dev/null || { echo "agent did not start"; cat /tmp/container-boot.log; exit 1; }
echo "agent up"

run_in() {
  # Run a command string inside the container as root; prints its output.
  local cmd="$1" i
  rm -f "$BUILD/agent-out.txt" "$BUILD/agent-exit"
  printf '%s\n' "$cmd" > "$BUILD/agent-cmd.sh"
  for i in $(seq 1 180); do
    [ -f "$BUILD/agent-exit" ] && break
    sleep 1
  done
  [ -f "$BUILD/agent-exit" ] || { echo "run_in timed out: $cmd"; return 1; }
  cat "$BUILD/agent-out.txt"
  return "$(cat "$BUILD/agent-exit")"
}

echo "== waiting for systemd to come up"
for _ in $(seq 1 60); do
  # NB: do not pipe run_in into grep: with pipefail, is-system-running's
  # own non-zero exit (degraded) would fail the whole pipeline even
  # when grep matches. Capture the output first.
  state=$(run_in "systemctl is-system-running" 2>/dev/null || true)
  if printf '%s' "$state" | grep -qE "running|degraded"; then
    break
  fi
  sleep 2
done
run_in "systemctl is-system-running" || true
run_in "systemctl --failed --no-legend" || true

echo "== resetting any previous install attempt (a re-run must be clean)"
run_in "dpkg --purge iris-mail-stack 2>/dev/null || true"

echo "== installing the .deb (domain iris-test.local)"
run_in "DEBIAN_FRONTEND=noninteractive IRIS_DOMAIN=iris-test.local dpkg -i /mnt/build/iris-mail-stack_$(cat VERSION)_amd64.deb"

echo "== running in-container checks"
run_in "/root/check-inside.sh"
echo "== all in-container checks passed"
