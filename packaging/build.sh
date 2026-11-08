#!/bin/bash
# build.sh - build the official IRIS .deb and tarball from clean.
# Usage: ./build.sh [--skip-iris-build]
# Needs: dpkg-deb, tar, sha256sum, and (unless the IRIS artefacts are
# already built) cargo + node/npm. IRIS is built from the repo checkout
# this packaging/ directory sits in, never from a fetched tag.
set -euo pipefail
cd "$(dirname "$0")"

VERSION=$(cat VERSION)
ARCH=amd64
PKG=iris-mail-stack
IRIS_PROJECT=$(cd .. && pwd)
DOWNLOADS=$PWD/build/downloads
ROOT=$PWD/build/root
TARBALL_DIR=$PWD/build/tarball

# shellcheck disable=SC1091
source ./versions.env

echo "== versions: package $VERSION, stalwart $STALWART_VERSION, caddy $CADDY_VERSION, iris $(git -C "$IRIS_PROJECT" rev-parse --short HEAD)"

# --- verify downloads ----------------------------------------------------
[ -f "$DOWNLOADS/stalwart.tar.gz" ] || { echo "missing $DOWNLOADS/stalwart.tar.gz"; exit 1; }
[ -f "$DOWNLOADS/caddy.tar.gz" ] || { echo "missing $DOWNLOADS/caddy.tar.gz"; exit 1; }
echo "$STALWART_SHA256  $DOWNLOADS/stalwart.tar.gz" | sha256sum -c -
echo "$CADDY_SHA256  $DOWNLOADS/caddy.tar.gz" | sha256sum -c -

# --- IRIS artefacts --------------------------------------------------------
# Built from this repo checkout: the API binary and web dist of whatever
# HEAD is checked out. A release build must run from the release tag
# (packaging/VERSION must equal the tag version), so refuse a dirty
# tree: a package must never silently bundle uncommitted changes.
[ -z "$(git -C "$IRIS_PROJECT" status --porcelain)" ] || { echo "irisproject tree is dirty; commit or stash before building a package"; exit 1; }
API_BIN="$IRIS_PROJECT/target/release/iris-api"
WEB_DIST="$IRIS_PROJECT/apps/web/dist"
if [ ! -x "$API_BIN" ] || [ ! -f "$WEB_DIST/index.html" ]; then
  echo "== building IRIS (this takes a few minutes)"
  ( cd "$IRIS_PROJECT" && export PATH="$HOME/.cargo/bin:$PATH" && cargo build --release --bin iris-api )
  ( cd "$IRIS_PROJECT/apps/web" && npm run build )
fi
[ -x "$API_BIN" ] || { echo "iris-api binary missing after build"; exit 1; }
[ -f "$WEB_DIST/index.html" ] || { echo "web dist missing after build"; exit 1; }
echo "iris-api: $(sha256sum "$API_BIN" | cut -c1-16)..."
grep -q "Revoke sessions" "$WEB_DIST"/assets/main-*.js || { echo "web dist is missing the PR #23 strings"; exit 1; }

# --- assemble the staging tree ---------------------------------------------
rm -rf "$ROOT" "$TARBALL_DIR"
mkdir -p "$ROOT"
cp -r --no-preserve=ownership src/. "$ROOT/"
mkdir -p "$ROOT/usr/local/bin" "$ROOT/srv/iris/dist" "$ROOT/etc/stalwart"
mkdir -p "$ROOT/var/lib/iris" "$ROOT/var/lib/stalwart" "$ROOT/var/log/stalwart" "$ROOT/var/lib/caddy"
mkdir -p "$ROOT/etc/caddy" "$ROOT/etc/iris"

# Binaries.
tar --no-same-owner -xzf "$DOWNLOADS/stalwart.tar.gz" -C "$ROOT/usr/local/bin" stalwart
tar --no-same-owner -xzf "$DOWNLOADS/caddy.tar.gz" -C "$ROOT/usr/local/bin" caddy
cp "$API_BIN" "$ROOT/usr/local/bin/iris-api"
chmod 755 "$ROOT/usr/local/bin/stalwart" "$ROOT/usr/local/bin/caddy" "$ROOT/usr/local/bin/iris-api"

# Web dist (normalize modes: tarballs preserve them, and the sandbox
# build leaves 750/770 on some files).
cp -r --no-preserve=ownership "$WEB_DIST/." "$ROOT/srv/iris/dist/"
chmod -R a+rX "$ROOT/srv/iris/dist"

# Stalwart store config (conffile; the installer fixes ownership to
# root:stalwart 640 on every run, since dpkg installs it as root:root).
printf '{"@type":"RocksDb","path":"/var/lib/stalwart/data"}' > "$ROOT/etc/stalwart/config.json"
chmod 640 "$ROOT/etc/stalwart/config.json"

# Scripts and units.
# Normalize directory modes first: the sandbox umask leaves 2770 on new
# dirs, and dpkg/tar would keep those on any directory they create.
find "$ROOT" -type d -exec chmod 755 {} +
chmod 755 "$ROOT/DEBIAN"
chmod 755 "$ROOT/usr/lib/iris/iris-install.sh" "$ROOT/usr/lib/iris/iris-setup.py"
chmod 755 "$ROOT/usr/local/sbin/iris-health" "$ROOT/usr/local/sbin/iris-cert-sync"
chmod 755 "$ROOT/DEBIAN/postinst" "$ROOT/DEBIAN/prerm" "$ROOT/DEBIAN/postrm"
chmod 644 "$ROOT/DEBIAN/conffiles"
sed "s/@VERSION@/$VERSION/" src/DEBIAN/control > "$ROOT/DEBIAN/control"
chmod 644 "$ROOT/DEBIAN/control"
chmod 644 "$ROOT/lib/systemd/system/"* "$ROOT/usr/share/iris/Caddyfile.template"

# --- static checks ---------------------------------------------------------
bash -n "$ROOT/usr/lib/iris/iris-install.sh"
bash -n "$ROOT/usr/local/sbin/iris-health"
bash -n "$ROOT/DEBIAN/postinst" "$ROOT/DEBIAN/prerm" "$ROOT/DEBIAN/postrm"
python3 -m py_compile "$ROOT/usr/lib/iris/iris-setup.py" "$ROOT/usr/local/sbin/iris-cert-sync"
rm -rf "$ROOT/usr/lib/iris/__pycache__" "$ROOT/usr/local/sbin/__pycache__"
if command -v systemd-analyze >/dev/null 2>&1; then
  systemd-analyze verify "$ROOT/lib/systemd/system/"*.service "$ROOT/lib/systemd/system/"*.timer || echo "NOTE: systemd-analyze verify reported issues (see above; unknown users on this host are expected)"
fi

# --- .deb ------------------------------------------------------------------
DEB="$PWD/build/${PKG}_${VERSION}_${ARCH}.deb"
dpkg-deb --build --root-owner-group "$ROOT" "$DEB"
echo "== .deb: $DEB ($(du -h "$DEB" | cut -f1))"

# --- tarball ---------------------------------------------------------------
mkdir -p "$TARBALL_DIR/$PKG-$VERSION"
cp -r --no-preserve=ownership "$ROOT/usr" "$ROOT/etc" "$ROOT/lib" "$ROOT/srv" "$ROOT/var" "$TARBALL_DIR/$PKG-$VERSION/"
cp scripts/install.sh "$TARBALL_DIR/$PKG-$VERSION/install.sh"
chmod 755 "$TARBALL_DIR/$PKG-$VERSION/install.sh"
tar --sort=name --owner=0 --group=0 -czf "$PWD/build/${PKG}-${VERSION}.tar.gz" -C "$TARBALL_DIR" "$PKG-$VERSION"
echo "== tarball: $PWD/build/${PKG}-${VERSION}.tar.gz ($(du -h "$PWD/build/${PKG}-${VERSION}.tar.gz" | cut -f1))"
echo "== done"
