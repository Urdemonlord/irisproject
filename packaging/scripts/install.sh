#!/bin/sh
# install.sh - tarball installer for iris-mail-stack.
# Usage (as root): ./install.sh [domain]
# Copies the payload into place, then runs the same installer the .deb
# postinst runs. Set IRIS_DOMAIN (or pass it as the first argument),
# IRIS_ADMIN_EMAIL and IRIS_ADMIN_PASSWORD to run non-interactively.
set -e
cd "$(dirname "$0")"
[ "$(id -u)" = 0 ] || { echo "run as root" >&2; exit 1; }
[ -n "$1" ] && IRIS_DOMAIN="$1"
export IRIS_DOMAIN IRIS_ADMIN_EMAIL IRIS_ADMIN_PASSWORD

cp -a usr/ etc/ lib/ srv/ var/ /
chmod 755 /usr/local/bin/stalwart /usr/local/bin/caddy /usr/local/bin/iris-api
chmod 755 /usr/lib/iris/iris-install.sh /usr/lib/iris/iris-setup.py
chmod 755 /usr/local/sbin/iris-health /usr/local/sbin/iris-cert-sync
chmod 644 /lib/systemd/system/iris-*.service /lib/systemd/system/iris-*.timer
chmod 644 /usr/share/iris/Caddyfile.template
chmod 640 /etc/stalwart/config.json 2>/dev/null || true
/usr/lib/iris/iris-install.sh configure
