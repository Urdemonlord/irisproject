#!/bin/bash
# build-macos.sh - build the official IRIS macOS tarballs from clean.
# Usage: ./build-macos.sh [--arch arm64|amd64|all]
# Runs ON a Mac (or a macOS CI runner): it cross-builds the IRIS binary
# for both Apple Silicon (aarch64-apple-darwin) and Intel
# (x86_64-apple-darwin) and bundles the macOS Stalwart + Caddy releases.
# Needs: curl, shasum, tar, plutil, and (unless the IRIS artefacts are
# already built) cargo + node/npm. IRIS is built from the repo checkout
# this packaging/ directory sits in, never from a fetched tag.
set -euo pipefail
cd "$(dirname "$0")"

ARCHS=all
while [ $# -gt 0 ]; do
  case "$1" in
    --arch) ARCHS="$2"; shift 2 ;;
    --arch=*) ARCHS="${1#--arch=}"; shift ;;
    *) echo "unknown argument: $1 (usage: $0 [--arch arm64|amd64|all])" >&2; exit 1 ;;
  esac
done
case "$ARCHS" in
  all) WANT="arm64 amd64" ;;
  arm64|amd64) WANT="$ARCHS" ;;
  *) echo "--arch must be arm64, amd64, or all" >&2; exit 1 ;;
esac

VERSION=$(cat ../VERSION)
PKG=iris-mail-stack
IRIS_PROJECT=$(cd ../.. && pwd)
DOWNLOADS=$PWD/build/macos/downloads
STAGING=$PWD/build/macos/staging

# shellcheck disable=SC1091
source ./versions-macos.env

command -v plutil >/dev/null 2>&1 || { echo "plutil is required (run this on a Mac)"; exit 1; }

echo "== versions: package $VERSION, stalwart $STALWART_VERSION, caddy $CADDY_VERSION, iris $(git -C "$IRIS_PROJECT" rev-parse --short HEAD)"

# --- verify downloads ----------------------------------------------------
mkdir -p "$DOWNLOADS"
for arch in $WANT; do
  case "$arch" in
    arm64) S_ASSET=$STALWART_ARM64_ASSET; S_SHA=$STALWART_ARM64_SHA256; C_ASSET=$CADDY_ARM64_ASSET; C_SHA=$CADDY_ARM64_SHA256 ;;
    amd64) S_ASSET=$STALWART_AMD64_ASSET; S_SHA=$STALWART_AMD64_SHA256; C_ASSET=$CADDY_AMD64_ASSET; C_SHA=$CADDY_AMD64_SHA256 ;;
  esac
  if [ ! -f "$DOWNLOADS/$S_ASSET" ]; then
    echo "== downloading stalwart $arch ($S_ASSET)"
    curl -fsSL "https://github.com/stalwartlabs/stalwart/releases/download/v$STALWART_VERSION/$S_ASSET" -o "$DOWNLOADS/$S_ASSET"
  fi
  if [ ! -f "$DOWNLOADS/$C_ASSET" ]; then
    echo "== downloading caddy $arch ($C_ASSET)"
    curl -fsSL "https://github.com/caddyserver/caddy/releases/download/v$CADDY_VERSION/$C_ASSET" -o "$DOWNLOADS/$C_ASSET"
  fi
  echo "$S_SHA  $DOWNLOADS/$S_ASSET" | shasum -a 256 -c -
  echo "$C_SHA  $DOWNLOADS/$C_ASSET" | shasum -a 256 -c -
done

# --- IRIS artefacts --------------------------------------------------------
# Built from this repo checkout. A release build must run from the
# release tag (packaging/VERSION must equal the tag version), so refuse
# a dirty tree: a package must never silently bundle uncommitted changes.
[ -z "$(git -C "$IRIS_PROJECT" status --porcelain)" ] || { echo "irisproject tree is dirty; commit or stash before building a package"; exit 1; }
WEB_DIST="$IRIS_PROJECT/apps/web/dist"
if [ ! -f "$WEB_DIST/index.html" ]; then
  echo "== building web dist (this takes a few minutes)"
  ( cd "$IRIS_PROJECT/apps/web" && npm run build )
fi
[ -f "$WEB_DIST/index.html" ] || { echo "web dist missing after build"; exit 1; }
grep -q "Revoke sessions" "$WEB_DIST"/assets/main-*.js || { echo "web dist is missing the PR #23 strings"; exit 1; }
for arch in $WANT; do
  case "$arch" in
    arm64) TARGET=aarch64-apple-darwin ;;
    amd64) TARGET=x86_64-apple-darwin ;;
  esac
  API_BIN="$IRIS_PROJECT/target/$TARGET/release/iris-api"
  if [ ! -x "$API_BIN" ]; then
    echo "== building iris-api for $TARGET (this takes a few minutes)"
    ( cd "$IRIS_PROJECT" && export PATH="$HOME/.cargo/bin:$PATH" && cargo build --release --target "$TARGET" --bin iris-api )
  fi
  [ -x "$API_BIN" ] || { echo "iris-api binary missing after build ($TARGET)"; exit 1; }
  echo "iris-api $arch: $(shasum -a 256 "$API_BIN" | cut -c1-16)..."
done

# --- assemble one staging tree per arch ------------------------------------
# Layout mirrors the Linux package, with macOS differences:
# /usr/lib is SIP-protected, so the installer scripts live in
# /usr/local/lib/iris; services are launchd plists in
# /Library/LaunchDaemons, not systemd units.
for arch in $WANT; do
  case "$arch" in
    arm64) TARGET=aarch64-apple-darwin; S_ASSET=$STALWART_ARM64_ASSET; C_ASSET=$CADDY_ARM64_ASSET ;;
    amd64) TARGET=x86_64-apple-darwin; S_ASSET=$STALWART_AMD64_ASSET; C_ASSET=$CADDY_AMD64_ASSET ;;
  esac
  ROOT="$STAGING/$PKG-$VERSION-macos-$arch"
  rm -rf "$ROOT"
  mkdir -p "$ROOT/usr/local/bin" "$ROOT/usr/local/sbin" "$ROOT/usr/local/lib/iris" "$ROOT/usr/local/share/iris"
  mkdir -p "$ROOT/etc/iris" "$ROOT/etc/stalwart/tls" "$ROOT/etc/caddy"
  mkdir -p "$ROOT/srv/iris/dist"
  mkdir -p "$ROOT/var/lib/iris" "$ROOT/var/lib/stalwart" "$ROOT/var/log/stalwart" "$ROOT/var/lib/caddy" "$ROOT/var/log/caddy"
  mkdir -p "$ROOT/Library/LaunchDaemons"

  # Binaries.
  tar -xzf "$DOWNLOADS/$S_ASSET" -C "$ROOT/usr/local/bin" stalwart
  tar -xzf "$DOWNLOADS/$C_ASSET" -C "$ROOT/usr/local/bin" caddy
  cp "$IRIS_PROJECT/target/$TARGET/release/iris-api" "$ROOT/usr/local/bin/iris-api"
  chmod 755 "$ROOT/usr/local/bin/stalwart" "$ROOT/usr/local/bin/caddy" "$ROOT/usr/local/bin/iris-api"

  # Web dist (normalize modes: tarballs preserve them).
  cp -R "$WEB_DIST/." "$ROOT/srv/iris/dist/"
  chmod -R a+rX "$ROOT/srv/iris/dist"

  # Stalwart store config (the installer fixes ownership to
  # root:stalwart 640 on every run).
  printf '{"@type":"RocksDb","path":"/var/lib/stalwart/data"}' > "$ROOT/etc/stalwart/config.json"
  chmod 640 "$ROOT/etc/stalwart/config.json"

  # Scripts and plists. iris-setup.py and iris-cert-sync are shared
  # verbatim with the Linux package (pure Python stdlib, same paths);
  # iris-health is the macOS adaptation (LibreSSL s_client has no
  # -brief flag), iris-install-macos.sh mirrors iris-install.sh.
  cp ../src/usr/lib/iris/iris-setup.py "$ROOT/usr/local/lib/iris/iris-setup.py"
  cp ../src/usr/local/sbin/iris-cert-sync "$ROOT/usr/local/sbin/iris-cert-sync"
  cp iris-health-macos "$ROOT/usr/local/sbin/iris-health"
  cp iris-api-launch.sh iris-install-macos.sh "$ROOT/usr/local/lib/iris/"
  cp ../src/usr/share/iris/Caddyfile.template "$ROOT/usr/local/share/iris/Caddyfile.template"
  cp launchd/*.plist "$ROOT/Library/LaunchDaemons/"
  chmod 755 "$ROOT/usr/local/lib/iris/iris-api-launch.sh" "$ROOT/usr/local/lib/iris/iris-install-macos.sh" "$ROOT/usr/local/lib/iris/iris-setup.py"
  chmod 755 "$ROOT/usr/local/sbin/iris-health" "$ROOT/usr/local/sbin/iris-cert-sync"
  chmod 644 "$ROOT/Library/LaunchDaemons/"*.plist "$ROOT/usr/local/share/iris/Caddyfile.template"
  find "$ROOT" -type d -exec chmod 755 {} +

  # --- static checks -------------------------------------------------------
  bash -n "$ROOT/usr/local/lib/iris/iris-api-launch.sh"
  bash -n "$ROOT/usr/local/lib/iris/iris-install-macos.sh"
  bash -n "$ROOT/usr/local/sbin/iris-health"
  python3 -m py_compile "$ROOT/usr/local/lib/iris/iris-setup.py" "$ROOT/usr/local/sbin/iris-cert-sync"
  rm -rf "$ROOT/usr/local/lib/iris/__pycache__" "$ROOT/usr/local/sbin/__pycache__"
  for plist in "$ROOT/Library/LaunchDaemons/"*.plist; do
    plutil -lint "$plist" >/dev/null || { echo "plist fails lint: $plist"; exit 1; }
  done

  # --- tarball -------------------------------------------------------------
  mkdir -p "$STAGING/tarball"
  cp install.sh "$ROOT/install.sh"
  chmod 755 "$ROOT/install.sh"
  TARBALL="$PWD/build/macos/${PKG}-${VERSION}-macos-${arch}.tar.gz"
  tar -czf "$TARBALL" -C "$STAGING" "$PKG-$VERSION-macos-$arch"
  echo "== tarball: $TARBALL ($(du -h "$TARBALL" | cut -f1))"
  shasum -a 256 "$TARBALL"
done
echo "== done"
