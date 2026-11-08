#!/bin/sh
# One-line installer for iris-mail-stack (Debian 12 / Ubuntu 24.04, amd64).
# Usage: curl -fsSL https://github.com/Urdemonlord/irisproject/releases/download/v0.3.0/install.sh | sudo sh
# Or download it first, read it, then run it. It installs the .deb from
# the same release and lets its postinst do the setup.
set -e
[ "$(id -u)" = 0 ] || { echo "run as root (sudo sh install.sh)" >&2; exit 1; }
VERSION=0.3.0
URL="https://github.com/Urdemonlord/irisproject/releases/download/v${VERSION}/iris-mail-stack_${VERSION}_amd64.deb"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT INT TERM
echo "downloading iris-mail-stack $VERSION ..."
curl -fsSL "$URL" -o "$TMP/iris-mail-stack.deb"
dpkg -i "$TMP/iris-mail-stack.deb" || apt-get install -f -y
echo "installed. The postinst above asked for your domain and printed the admin password once."
