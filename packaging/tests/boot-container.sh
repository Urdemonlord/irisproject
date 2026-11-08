#!/bin/sh
# boot-container.sh - boot the Debian 12 test container WITHOUT
# systemd-nspawn (which cannot mount sysfs in this sandbox) and WITHOUT
# a nested user namespace (a nested userns gets CapEff 0 here, so no
# mount works inside it). The shell's own userns already maps
# 0..65535 and holds CAP_SYS_ADMIN, so unsharing only mount/pid/net/uts
# is enough: proc and tmpfs mounts work, /sys is bind-mounted from the
# host's (fake) sysfs, and the container's systemd runs as PID 1.
# Usage (host side, detached):
#   setsid unshare --mount --pid --fork --uts \
#     /bin/sh tests/boot-container.sh
# No --net: this sandbox strips CAP_NET_ADMIN, so a private network
# namespace cannot bring loopback up. The container shares the host
# network namespace (loopback is already up); its services bind the
# same ports they would in isolation, which is also what lets the
# host-side test driver reach them directly.
set -e
ROOT=/dev/shm/iris-test
# The build dir is next to this script's parent (packaging/build).
BUILD=$(cd "$(dirname "$0")/.." && pwd)/build

mkdir -p "$ROOT/mnt/build"
mount -t proc proc "$ROOT/proc"
mount --bind /sys "$ROOT/sys"
# /dev cannot be bound as a whole here, so mount a tmpfs and bind the
# nodes systemd and the services need, one by one.
mount -t tmpfs tmpfs "$ROOT/dev"
for node in null zero full random urandom tty console ptmx; do
  touch "$ROOT/dev/$node"
  mount --bind "/dev/$node" "$ROOT/dev/$node"
done
mkdir -p "$ROOT/dev/pts" "$ROOT/dev/shm"
mount --bind /dev/pts "$ROOT/dev/pts"
mount --bind /dev/shm "$ROOT/dev/shm"
ln -sf /proc/self/fd "$ROOT/dev/fd"
mount -t tmpfs tmpfs "$ROOT/run"
mount --bind "$BUILD" "$ROOT/mnt/build"
mount --bind /etc/resolv.conf "$ROOT/etc/resolv.conf"

hostname iris-test

# The sandbox shell leaks its own TLS/proxy environment into the
# container (SSL_CERT_FILE points at a host path that does not exist in
# here); drop it so the test sees the same environment a real server
# has. A stray SSL_CERT_FILE would make Stalwart load no CA
# certificates and panic at startup.
unset SSL_CERT_FILE SSL_CERT_DIR CURL_CA_BUNDLE GIT_SSL_CAINFO NODE_EXTRA_CA_CERTS
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY all_proxy

# Tell systemd it runs in a container: it skips mounting API
# filesystems and hardware units, and does not try to take over the host.
export container=unshare
# Start the command agent inside the container (it is the only way to
# run commands in there: setns is blocked in this sandbox), then hand
# over PID 1 to systemd. The agent survives as a child of PID 1.
exec chroot "$ROOT" /bin/sh -c '/usr/local/bin/container-agent.sh >>/mnt/build/agent-log.txt 2>&1 & exec /sbin/init'
