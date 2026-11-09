#!/bin/sh
# iris-install-macos.sh - shared first-install / upgrade installer for the
# official IRIS one-package install on macOS. Called by the tarball
# install.sh. Idempotent: safe to run again on upgrade.
# Reads IRIS_DOMAIN, IRIS_ADMIN_EMAIL, IRIS_ADMIN_PASSWORD from the
# environment; prompts on a TTY when they are unset.
# Mirrors the Linux iris-install.sh; the differences are all macOS:
# users/groups via dscl (no useradd/getent), services via launchd
# (no systemctl), scripts live in /usr/local/lib/iris because /usr/lib
# is SIP-protected, and Stalwart/Caddy run as root because macOS has no
# Linux-style capability for binding privileged ports.
set -e

PROG=iris-install-macos

log() { echo "$PROG: $*"; }
die() { echo "$PROG: error: $*" >&2; exit 1; }

# --- domain -------------------------------------------------------------
if [ -z "$IRIS_DOMAIN" ]; then
  if [ -t 0 ]; then
    printf 'Domain for this mail server (example: example.com): '
    read -r IRIS_DOMAIN
  fi
fi
[ -n "$IRIS_DOMAIN" ] || die "IRIS_DOMAIN is not set (set it or run interactively)"
IRIS_DOMAIN=$(printf '%s' "$IRIS_DOMAIN" | tr 'A-Z' 'a-z')
case "$IRIS_DOMAIN" in
  *://*|*/*|*_*|*..*|.*|*.)
    die "IRIS_DOMAIN '$IRIS_DOMAIN' is not a plain domain name" ;;
esac
case "$IRIS_DOMAIN" in
  *.*) ;;
  *) die "IRIS_DOMAIN '$IRIS_DOMAIN' needs at least one dot (example: example.com)" ;;
esac

MAIL_HOST="mail.$IRIS_DOMAIN"
CONTENT_HOST="mail-content.$IRIS_DOMAIN"
INBOX_DOMAIN="inbox.$IRIS_DOMAIN"

if [ -z "$IRIS_ADMIN_EMAIL" ]; then
  if [ -t 0 ]; then
    printf 'Admin email [%s]: ' "admin@$IRIS_DOMAIN"
    read -r IRIS_ADMIN_EMAIL
  fi
  IRIS_ADMIN_EMAIL=${IRIS_ADMIN_EMAIL:-admin@$IRIS_DOMAIN}
fi
IRIS_ADMIN_EMAIL=$(printf '%s' "$IRIS_ADMIN_EMAIL" | tr 'A-Z' 'a-z')
case "$IRIS_ADMIN_EMAIL" in
  *"@$IRIS_DOMAIN") ;;
  *) die "IRIS_ADMIN_EMAIL must be an address on $IRIS_DOMAIN" ;;
esac
ADMIN_LOCAL=$(printf '%s' "$IRIS_ADMIN_EMAIL" | sed 's/@.*//')

# --- users and groups (dscl; daemon accounts take the first free id >= 301) ---
free_id() { # free_id user|group
  if [ "$1" = user ]; then
    used=$(dscl . -list /Users UniqueID | awk '{print $2}' | tr '\n' ' ')
  else
    used=$(dscl . -list /Groups PrimaryGroupID | awk '{print $2}' | tr '\n' ' ')
  fi
  i=301
  while [ "$i" -le 499 ]; do
    case " $used " in
      *" $i "*) ;;
      *) echo "$i"; return 0 ;;
    esac
    i=$((i + 1))
  done
  die "no free UID/GID in 301-499"
}

ensure_user() { # ensure_user <name> <home>
  name=$1; home=$2
  if ! dscl . -read "/Groups/$name" >/dev/null 2>&1; then
    gid=$(free_id group)
    dscl . -create "/Groups/$name" PrimaryGroupID "$gid"
    dscl . -create "/Groups/$name" RealName "$name"
    log "created group $name ($gid)"
  fi
  gid=$(dscl . -read "/Groups/$name" PrimaryGroupID | awk '{print $2}')
  if ! dscl . -read "/Users/$name" >/dev/null 2>&1; then
    uid=$(free_id user)
    dscl . -create "/Users/$name" UniqueID "$uid"
    dscl . -create "/Users/$name" PrimaryGroupID "$gid"
    dscl . -create "/Users/$name" NFSHomeDirectory "$home"
    dscl . -create "/Users/$name" UserShell /usr/bin/false
    dscl . -create "/Users/$name" RealName "$name"
    log "created user $name ($uid)"
  fi
}

ensure_user iris /var/lib/iris
ensure_user stalwart /var/lib/stalwart
ensure_user caddy /var/lib/caddy

mkdir -p /etc/iris /etc/stalwart/tls /etc/caddy
mkdir -p /var/lib/iris /var/lib/stalwart /var/log/stalwart /var/lib/caddy /var/log/caddy
mkdir -p /usr/local/share/iris/dist
chown iris:iris /var/lib/iris
chown stalwart:stalwart /var/lib/stalwart /var/log/stalwart
chown caddy:caddy /var/lib/caddy /var/log/caddy
chmod 750 /var/lib/iris /var/lib/stalwart /var/log/stalwart /var/lib/caddy /var/log/caddy
chmod 750 /etc/iris /etc/stalwart
chown root:stalwart /etc/stalwart /etc/stalwart/tls
chmod 750 /etc/stalwart/tls

# --- Stalwart store config (static file, shipped by the package) --------
# The package ships it as root:root; the installer must always fix the
# ownership, not only when it creates the file, or the stalwart user
# cannot read it and the server fails to start.
if [ ! -f /etc/stalwart/config.json ]; then
  printf '{"@type":"RocksDb","path":"/var/lib/stalwart/data"}' > /etc/stalwart/config.json
  log "wrote /etc/stalwart/config.json"
fi
chown root:stalwart /etc/stalwart/config.json
chmod 640 /etc/stalwart/config.json

# --- Caddyfile (rendered once from the shipped template) ----------------
# The template is shared with Linux; the third substitution moves the
# web root from /srv/iris/dist (Linux) to /usr/local/share/iris/dist:
# the macOS root volume is read-only and /srv cannot be created.
if [ ! -f /etc/caddy/Caddyfile ]; then
  sed -e "s|@MAIL_HOST@|$MAIL_HOST|g" \
      -e "s|@CONTENT_HOST@|$CONTENT_HOST|g" \
      -e "s|/srv/iris/dist|/usr/local/share/iris/dist|g" \
      /usr/local/share/iris/Caddyfile.template > /etc/caddy/Caddyfile
  chmod 644 /etc/caddy/Caddyfile
  log "wrote /etc/caddy/Caddyfile for $MAIL_HOST"
fi

# --- self-signed TLS for the mail listeners (replaced by cert-sync) -----
if [ ! -f /etc/stalwart/tls/fullchain.pem ] || [ ! -f /etc/stalwart/tls/privkey.pem ]; then
  openssl req -x509 -newkey rsa:2048 \
    -keyout /etc/stalwart/tls/privkey.pem \
    -out /etc/stalwart/tls/fullchain.pem \
    -days 30 -nodes -subj "/CN=$MAIL_HOST" 2>/dev/null
  chown root:stalwart /etc/stalwart/tls/fullchain.pem /etc/stalwart/tls/privkey.pem
  chmod 640 /etc/stalwart/tls/fullchain.pem
  chmod 640 /etc/stalwart/tls/privkey.pem
  log "generated a 30-day self-signed certificate for $MAIL_HOST (cert-sync replaces it with the Caddy certificate)"
fi

# --- IRIS env + generated secrets (created once, never overwritten) -----
if [ ! -f /etc/iris/iris.env ]; then
  SESSION_SECRET=$(openssl rand -hex 32)
  TEMP_PW=$(openssl rand -hex 16)
  if [ -n "$IRIS_ADMIN_PASSWORD" ]; then
    ADMIN_PW="$IRIS_ADMIN_PASSWORD"
  else
    ADMIN_PW=$(openssl rand -hex 12)
    GENERATED_ADMIN_PW=1
  fi
  umask 077
  cat > /etc/iris/iris.env <<EOF
# IRIS configuration, generated by the iris-mail-stack installer.
# Edit with care; restart iris-api after changes.
IRIS_ENV=production
IRIS_HOST=127.0.0.1
IRIS_PORT=8000
STALWART_API_URL=http://127.0.0.1:8080
DATABASE_URL=sqlite:iris.db
SESSION_SECRET=$SESSION_SECRET
SEND_LOCAL_DOMAINS=$IRIS_DOMAIN
SEND_EXTERNAL=true
TEMP_INBOX_DOMAIN=$INBOX_DOMAIN
TEMP_INBOX_DEFAULT_EXPIRY_HOURS=1
TEMP_INBOX_MAIL_USER=temp@$INBOX_DOMAIN
TEMP_INBOX_MAIL_PASSWORD=$TEMP_PW
MAIL_CONTENT_ORIGIN=https://$CONTENT_HOST
IRIS_ADMIN_EMAILS=$IRIS_ADMIN_EMAIL
TURNSTILE_SECRET_KEY=
TURNSTILE_SITE_KEY=
TURNSTILE_HOSTNAMES=
IRIS_INTEGRATION_EMAIL_ENABLED=false
IRIS_INTEGRATION_EMAIL_FROM=
RESEND_API_KEY=
EOF
  chown root:iris /etc/iris/iris.env
  chmod 640 /etc/iris/iris.env
  printf '%s\n' "$ADMIN_PW" > /etc/iris/.admin_credentials
  chmod 600 /etc/iris/.admin_credentials
  log "wrote /etc/iris/iris.env (admin password saved to /etc/iris/.admin_credentials, mode 600)"
fi

# --- provision Stalwart (once) ------------------------------------------
if [ ! -f /var/lib/stalwart/.iris-provisioned ]; then
  ADMIN_PW=$(cat /etc/iris/.admin_credentials)
  TEMP_PW=$(sed -n 's/^TEMP_INBOX_MAIL_PASSWORD=//p' /etc/iris/iris.env)
  RECOVERY_PW=$(openssl rand -hex 16)
  # Run Stalwart directly (not via launchd) with a one-time recovery
  # admin, provision through the JMAP management API, then stop it.
  # The recovery password is never stored; the normal daemon runs
  # without it, so it stops working after this step.
  # SSL_CERT_FILE is the macOS system CA bundle.
  sudo -u stalwart env "STALWART_RECOVERY_ADMIN=admin:$RECOVERY_PW" \
    "SSL_CERT_FILE=/etc/ssl/cert.pem" \
    /usr/local/bin/stalwart --config /etc/stalwart/config.json \
    >/var/log/stalwart/setup.log 2>&1 &
  SETUP_PID=$!
  if IRIS_DOMAIN="$IRIS_DOMAIN" \
     IRIS_ADMIN_EMAIL="$IRIS_ADMIN_EMAIL" \
     ADMIN_LOCAL="$ADMIN_LOCAL" \
     ADMIN_PW="$ADMIN_PW" \
     TEMP_PW="$TEMP_PW" \
     RECOVERY_PW="$RECOVERY_PW" \
     /usr/local/lib/iris/iris-setup.py; then
    touch /var/lib/stalwart/.iris-provisioned
    chown stalwart:stalwart /var/lib/stalwart/.iris-provisioned
    log "Stalwart provisioned for $IRIS_DOMAIN"
  else
    kill "$SETUP_PID" 2>/dev/null || true
    wait "$SETUP_PID" 2>/dev/null || true
    die "Stalwart provisioning failed; see /var/log/stalwart/setup.log and re-run this script"
  fi
  kill "$SETUP_PID" 2>/dev/null || true
  wait "$SETUP_PID" 2>/dev/null || true
fi

# --- load services and timers -------------------------------------------
# bootout first: bootstrap fails if the job is already loaded, and this
# script must be safe to run again on upgrade.
for job in im.meowlabs.iris-stalwart im.meowlabs.iris-api im.meowlabs.iris-caddy im.meowlabs.iris-cleanup im.meowlabs.iris-cert-sync; do
  launchctl bootout "system/$job" 2>/dev/null || true
  launchctl bootstrap system "/Library/LaunchDaemons/$job.plist" || die "launchctl bootstrap failed for $job"
done
# Restart the three services so an upgrade picks up the new binaries.
for job in im.meowlabs.iris-stalwart im.meowlabs.iris-api im.meowlabs.iris-caddy; do
  launchctl kickstart -k "system/$job" 2>/dev/null || true
done

# --- wait for the API and print the summary ------------------------------
i=0
until curl -fsS -m 2 http://127.0.0.1:8000/health >/dev/null 2>&1; do
  i=$((i + 1))
  [ "$i" -ge 30 ] && die "iris-api did not answer /health within 60s; check launchctl print system/im.meowlabs.iris-api"
  sleep 2
done

echo
echo "IRIS is up:"
echo "  Webmail:  https://$MAIL_HOST"
echo "  Admin:    $IRIS_ADMIN_EMAIL (password in /etc/iris/.admin_credentials, printed once below)"
echo "  Temp inbox domain: $INBOX_DOMAIN"
echo
echo "Before mail flows, publish these DNS records (full zone file in /etc/iris/dns-zone.txt):"
sed -n '1,12p' /etc/iris/dns-zone.txt 2>/dev/null || true
echo
echo "Admin password (shown once, also stored in /etc/iris/.admin_credentials):"
cat /etc/iris/.admin_credentials
echo
if [ -n "$GENERATED_ADMIN_PW" ]; then
  echo "The admin password above was generated. Change it in the Stalwart admin UI or with iris-setup later."
fi
echo "Services: launchctl list | grep im.meowlabs (or launchctl print system/im.meowlabs.iris-api)"
echo "Caddy issues TLS certificates automatically once the DNS records point here."
echo "Outbound mail goes out over port 25 directly; if your provider blocks it, set RESEND_API_KEY in /etc/iris/iris.env."
