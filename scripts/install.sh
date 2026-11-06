#!/bin/sh
# IRIS installer — the script behind README "Install with your AI agent".
#
# What it does:
#   1. Checks for cargo, node (>= 22), npm, openssl, curl.
#   2. Creates .env from .env.example (mode 600) with a generated
#      SESSION_SECRET, unless .env already exists.
#   3. Builds the release API binary and the web bundle.
#   4. Prints the remaining manual steps (mail server, systemd,
#      reverse proxy, DNS) — those need your domain and are NOT done here.
#
# What it does NOT do: install a mail server, touch DNS, enable systemd
# units, issue TLS certificates, or send any mail. See README Deployment.
#
# Usage (from the repo root):
#   ./scripts/install.sh [--non-interactive] [--skip-build]
#
# Non-interactive values come from the environment:
#   STALWART_API_URL, TEMP_INBOX_DOMAIN,
#   TEMP_INBOX_MAIL_USER, TEMP_INBOX_MAIL_PASSWORD
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"

NON_INTERACTIVE=0
SKIP_BUILD=0
for arg in "$@"; do
  case "$arg" in
    --non-interactive) NON_INTERACTIVE=1 ;;
    --skip-build) SKIP_BUILD=1 ;;
    -h|--help)
      echo "Usage: ./scripts/install.sh [--non-interactive] [--skip-build]"
      exit 0
      ;;
    *) echo "install.sh: unknown argument: $arg" >&2; exit 2 ;;
  esac
done

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "install.sh: missing required tool: $1 ($2)" >&2
    exit 1
  fi
}

need cargo "Rust stable toolchain — https://rustup.rs"
need node "Node.js 22 — https://nodejs.org"
need npm "ships with Node.js"
need openssl "for SESSION_SECRET generation"
need curl "for the mail-server reachability check"

NODE_MAJOR=$(node -p 'process.versions.node.split(".")[0]')
if [ "$NODE_MAJOR" -lt 22 ]; then
  echo "install.sh: Node.js $NODE_MAJOR found, IRIS needs Node.js 22+" >&2
  exit 1
fi
echo "install.sh: cargo $(cargo --version | awk '{print $2}') / node $(node --version)"

prompt() {
  # prompt <VAR> <question> <default>
  _var=$1; _q=$2; _def=$3
  _val=$(eval "printf '%s' \"\${$_var:-}\"") || true
  if [ -n "$_val" ]; then
    echo "install.sh: $_var=${_val}"
    return 0
  fi
  if [ "$NON_INTERACTIVE" -eq 1 ] || [ ! -t 0 ]; then
    _val=$_def
  else
    printf '%s [%s]: ' "$_q" "$_def" >&2
    IFS= read -r _val || true
    _val=${_val:-$_def}
  fi
  _esc=$(printf '%s' "$_val" | sed -e 's/[\\"$`]/\\&/g')
  eval "$_var=\"$_esc\""
  case "$_var" in
    *PASSWORD*) echo "install.sh: $_var=(set)" ;;
    *) echo "install.sh: $_var=${_val}" ;;
  esac
}

# env_value <KEY> — read one KEY= line from .env, stripping surrounding
# whitespace and one pair of surrounding single/double quotes
# (dotenv-style quoting). Without the quote strip, a quoted "0x…"
# value would reach the build (or the secret check) with literal
# quotes attached.
env_value() {
  grep "^$1=" .env | cut -d= -f2- | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e "s/^'//" -e "s/'\$//" -e 's/^"//' -e 's/"$//' || true
}

# set_env <KEY> <VALUE> — replace or append a KEY= line in .env
set_env() {
  _key=$1; _val=$2
  _esc=$(printf '%s' "$_val" | sed -e 's/[|&\]/\\&/g')
  if grep -q "^${_key}=" .env; then
    sed -i "s|^${_key}=.*|${_key}=${_esc}|" .env
  else
    printf '%s=%s\n' "$_key" "$_val" >> .env
  fi
}

if [ -f .env ]; then
  echo "install.sh: .env already exists — leaving it untouched"
else
  cp .env.example .env
  chmod 600 .env
  echo "install.sh: created .env (mode 600) from .env.example"
  SECRET=$(openssl rand -hex 32)
  set_env SESSION_SECRET "$SECRET"
  echo "install.sh: generated SESSION_SECRET"

  prompt STALWART_API_URL "Stalwart JMAP API URL" "http://127.0.0.1:8092"
  prompt TEMP_INBOX_DOMAIN "Domain for temporary inboxes" "inbox.example.com"
  prompt TEMP_INBOX_MAIL_USER "Catch-all mailbox for temp inboxes (empty = temp inbox disabled)" ""
  prompt TEMP_INBOX_MAIL_PASSWORD "Catch-all mailbox password (empty = temp inbox disabled)" ""

  set_env STALWART_API_URL "$STALWART_API_URL"
  set_env TEMP_INBOX_DOMAIN "$TEMP_INBOX_DOMAIN"
  set_env TEMP_INBOX_MAIL_USER "$TEMP_INBOX_MAIL_USER"
  set_env TEMP_INBOX_MAIL_PASSWORD "$TEMP_INBOX_MAIL_PASSWORD"

  if [ "$TEMP_INBOX_DOMAIN" = "inbox.example.com" ]; then
    echo "install.sh: WARNING: TEMP_INBOX_DOMAIN is still the example value —" >&2
    echo "install.sh:   set a real domain in .env before go-live." >&2
  fi
  if [ -z "$TEMP_INBOX_MAIL_USER" ]; then
    echo "install.sh: WARNING: no catch-all mailbox configured —" >&2
    echo "install.sh:   temp inbox will not work until TEMP_INBOX_MAIL_USER/PASSWORD are set." >&2
  fi
fi

# Mail-server reachability (warning only — Stalwart may be started after install)
STALWART_URL=$(grep '^STALWART_API_URL=' .env | cut -d= -f2- | tr -d ' ')
if [ -n "$STALWART_URL" ]; then
  if curl -fsS -m 5 -o /dev/null "$STALWART_URL" 2>/dev/null; then
    echo "install.sh: mail server reachable at $STALWART_URL"
  else
    echo "install.sh: WARNING: cannot reach mail server at $STALWART_URL" >&2
    echo "install.sh:   IRIS needs Stalwart (JMAP) — install it from the official" >&2
    echo "install.sh:   image and create one permanent account plus the catch-all account." >&2
  fi
fi

if [ "$SKIP_BUILD" -eq 1 ]; then
  echo "install.sh: --skip-build, stopping before the build"
  exit 0
fi

echo "install.sh: building release API (this takes a few minutes)…"
cargo build --release -p iris-api
echo "install.sh: building web bundle…"
npm --prefix apps/web ci
# Forward the Turnstile site key (public) to the Vite build so the
# login widget is baked in; the secret stays server-side only. Values
# are read with env_value so a quoted "0x…" line in .env does not
# reach the build (or the secret check) with literal quotes attached.
SITE_KEY=$(env_value TURNSTILE_SITE_KEY)
SECRET_SET=$(env_value TURNSTILE_SECRET_KEY)
if [ -n "$SECRET_SET" ] && [ -z "$SITE_KEY" ]; then
  echo "install.sh: WARNING: TURNSTILE_SECRET_KEY is set but TURNSTILE_SITE_KEY is empty —" >&2
  echo "install.sh:   the login form will have no widget and every login will fail." >&2
  echo "install.sh:   Set TURNSTILE_SITE_KEY in .env and rebuild the web." >&2
fi
VITE_TURNSTILE_SITE_KEY="$SITE_KEY" npm --prefix apps/web run build

[ -x target/release/iris-api ] || { echo "install.sh: build failed — no target/release/iris-api" >&2; exit 1; }
[ -f apps/web/dist/index.html ] || { echo "install.sh: build failed — no apps/web/dist/index.html" >&2; exit 1; }

echo
echo "install.sh: done."
echo "  API binary : $ROOT/target/release/iris-api"
echo "  Web bundle : $ROOT/apps/web/dist"
echo
echo "Remaining manual steps (see README Deployment):"
echo "  1. Mail server: Stalwart with one permanent account + catch-all account."
echo "  2. Run the API under systemd (Restart=always) as an unprivileged user."
echo "  3. Serve apps/web/dist behind nginx/Caddy; proxy /api + /auth to the API."
echo "  4. DNS: MX, SPF, DKIM, DMARC, PTR — see docs/07-PRODUCTION-READINESS.md §4."
echo "  5. Verify: cargo test --workspace && the README §4 checklist."
echo
echo "Local dev instead: cargo run -p iris-api  +  npm --prefix apps/web run dev"
