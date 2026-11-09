#!/bin/sh
# install.sh - tarball installer for iris-mail-stack on macOS.
# Usage (as root): sudo ./install.sh [domain]
# Copies the payload into place, then runs the macOS installer.
# Set IRIS_DOMAIN (or pass it as the first argument),
# IRIS_ADMIN_EMAIL and IRIS_ADMIN_PASSWORD to run non-interactively.
# ditto is the macOS-native copy tool (cp -a does not exist here).
set -e
cd "$(dirname "$0")"
[ "$(id -u)" = 0 ] || { echo "run as root (sudo ./install.sh)" >&2; exit 1; }
[ -n "$1" ] && IRIS_DOMAIN="$1"
export IRIS_DOMAIN IRIS_ADMIN_EMAIL IRIS_ADMIN_PASSWORD

ditto usr /usr
ditto etc /etc
ditto var /var
ditto Library /Library
# ditto preserves the tarball's ownership (the user who built it, for
# example the CI runner). System files must be root-owned: launchd
# refuses to load a plist it does not own ("Bootstrap failed: 5:
# Input/output error"), so fix ownership before configuring.
chown root:wheel /usr/local/bin/stalwart /usr/local/bin/caddy /usr/local/bin/iris-api
chown root:wheel /usr/local/sbin/iris-health /usr/local/sbin/iris-cert-sync
chown -R root:wheel /usr/local/lib/iris /usr/local/share/iris
chown root:wheel /Library/LaunchDaemons/im.meowlabs.iris-*.plist
chmod 755 /usr/local/bin/stalwart /usr/local/bin/caddy /usr/local/bin/iris-api
chmod 755 /usr/local/lib/iris/iris-api-launch.sh /usr/local/lib/iris/iris-install-macos.sh /usr/local/lib/iris/iris-setup.py
chmod 755 /usr/local/sbin/iris-health /usr/local/sbin/iris-cert-sync
chmod 644 /Library/LaunchDaemons/im.meowlabs.iris-*.plist
chmod 644 /usr/local/share/iris/Caddyfile.template
chmod 640 /etc/stalwart/config.json 2>/dev/null || true
/usr/local/lib/iris/iris-install-macos.sh configure
