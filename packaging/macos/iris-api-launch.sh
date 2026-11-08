#!/bin/sh
# iris-api-launch.sh - env-loading wrapper for iris-api on macOS.
# launchd plists have no EnvironmentFile (the systemd unit's way of
# loading /etc/iris/iris.env), so the api and cleanup jobs run through
# this wrapper, which exports the env file and execs the real command.
# Usage: iris-api-launch.sh <command> [args...]
set -e
[ -f /etc/iris/iris.env ] || { echo "iris-api-launch: /etc/iris/iris.env missing" >&2; exit 1; }
set -a
# shellcheck disable=SC1091
. /etc/iris/iris.env
set +a
exec "$@"
