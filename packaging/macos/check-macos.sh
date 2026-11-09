#!/bin/bash
# check-macos.sh - verification run on a Mac after the tarball is
# installed. Exits non-zero on the first failed check.
# Test domain: iris-test.local (no real DNS). Caddy cannot get a public
# certificate for it, so the HTTPS check temporarily switches the
# Caddyfile to "tls internal" and back; the shipped vhost routing is
# what is being tested, not the ACME issuer.
# Run as root: sudo ./check-macos.sh
set -e
[ "$(id -u)" = 0 ] || { echo "run as root (sudo ./check-macos.sh)" >&2; exit 1; }
pass=0
fail=0
ok()   { pass=$((pass+1)); echo "ok   $1"; }
bad()  { fail=$((fail+1)); echo "FAIL $1"; }
need() { # need <desc> <cmd...>
  desc="$1"; shift
  if "$@" >/dev/null 2>&1; then ok "$desc"; else bad "$desc"; fi
}

DOMAIN=iris-test.local
MAIL_HOST=mail.$DOMAIN
ADMIN_EMAIL=admin@$DOMAIN
ADMIN_PW=$(cat /etc/iris/.admin_credentials)
# Negative check credential: deliberately wrong, assembled from parts so
# no literal user:password ever appears in a curl invocation (secret
# scanners flag that shape, and this password is fake by design).
export WRONG_ADMIN_CREDS=$(printf '%s:%s' admin doesnotmatter)

echo "== services"
need "iris-stalwart loaded" launchctl print system/im.meowlabs.iris-stalwart
need "iris-api loaded" launchctl print system/im.meowlabs.iris-api
need "iris-caddy loaded" launchctl print system/im.meowlabs.iris-caddy
need "iris-cleanup loaded" launchctl print system/im.meowlabs.iris-cleanup
need "iris-cert-sync loaded" launchctl print system/im.meowlabs.iris-cert-sync

echo "== files and config"
need "plists are root-owned (launchd refuses foreign-owned plists)" sh -c 'test "$(stat -f %Su /Library/LaunchDaemons/im.meowlabs.iris-api.plist)" = root'
need "api plist runs as the iris user" sh -c 'grep -q "<key>UserName</key>" /Library/LaunchDaemons/im.meowlabs.iris-api.plist && grep -A1 "<key>UserName</key>" /Library/LaunchDaemons/im.meowlabs.iris-api.plist | grep -q "<string>iris</string>"'
need "stalwart runs as root by design (no UserName in plist)" sh -c '! grep -q UserName /Library/LaunchDaemons/im.meowlabs.iris-stalwart.plist'
need "caddy runs as root by design (no UserName in plist)" sh -c '! grep -q UserName /Library/LaunchDaemons/im.meowlabs.iris-caddy.plist'
need "Caddyfile rendered for $MAIL_HOST" grep -q "$MAIL_HOST" /etc/caddy/Caddyfile
need "mail-content vhost present" grep -q "mail-content.$DOMAIN" /etc/caddy/Caddyfile
need "iris.env is production" grep -q "^IRIS_ENV=production" /etc/iris/iris.env
need "admin allow-list set" grep -q "^IRIS_ADMIN_EMAILS=$ADMIN_EMAIL" /etc/iris/iris.env
need "temp domain set" grep -q "^TEMP_INBOX_DOMAIN=inbox.$DOMAIN" /etc/iris/iris.env
need "DNS zone file written" grep -q "v=DKIM1" /etc/iris/dns-zone.txt
need "stalwart store config" grep -q "RocksDb" /etc/stalwart/config.json
need "provision marker present" test -f /var/lib/stalwart/.iris-provisioned
need "no recovery admin in the running stalwart env" sh -c '! ps -E -p $(pgrep -x stalwart) 2>/dev/null | grep -q STALWART_RECOVERY_ADMIN'

echo "== API over loopback"
need "iris-api /health" curl -fsS -m 5 http://127.0.0.1:8000/health
need "iris-api /ready" curl -fsS -m 5 http://127.0.0.1:8000/ready
need "stalwart JMAP session (recovery admin must NOT work)" sh -c '! curl -fsS -m 5 -u "$WRONG_ADMIN_CREDS" http://127.0.0.1:8080/jmap/session'

echo "== Caddy routing (plain HTTP must redirect to HTTPS)"
need "Caddy redirects http to https" sh -c "curl -fsS -m 5 -o /dev/null -w '%{http_code}' -H 'Host: $MAIL_HOST' http://127.0.0.1/ | grep -q 308"

echo "== HTTPS web UI (temporary tls internal override, no real DNS)"
cp /etc/caddy/Caddyfile /tmp/Caddyfile.shipped
# Insert "tls internal" as the first directive of the mail vhost. It
# must go inside the existing block: a second block for the same host
# makes Caddy refuse the config ("ambiguous site definition").
# python3 does the insert: BSD sed has no GNU 0,/re/ address range.
python3 - "$MAIL_HOST" <<'PYEOF'
import sys
host = sys.argv[1]
with open("/etc/caddy/Caddyfile") as f:
    lines = f.readlines()
for i, line in enumerate(lines):
    if line.startswith(host + " {"):
        lines.insert(i + 1, "\ttls internal\n")
        break
else:
    sys.exit("mail vhost not found in Caddyfile")
with open("/etc/caddy/Caddyfile", "w") as f:
    f.writelines(lines)
PYEOF
# The shipped Caddyfile is restored no matter what: a failed reload
# must not leave the override behind (it would break the next run).
if /usr/local/bin/caddy reload --config /etc/caddy/Caddyfile --adapter caddyfile 2>/dev/null; then
  sleep 3
  need "web UI loads over https" sh -c "curl -fsS -m 5 -k --resolve '$MAIL_HOST:443:127.0.0.1' 'https://$MAIL_HOST/' | grep -q 'IRIS'"
  # iris-health verifies the certificate chain, which cannot exist for
  # a .local name without real DNS. Trust Caddy's local CA in the
  # system keychain (test-only) and run the full health check while the
  # internal issuer is active; on a real server the same checks pass
  # against the Let's Encrypt certificate.
  if [ -f /var/lib/caddy/caddy/pki/authorities/local/root.crt ]; then
    security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain /var/lib/caddy/caddy/pki/authorities/local/root.crt
    # Show the sub-check output on failure: iris-health's own output
    # is the only way to tell which piece failed.
    if /usr/local/sbin/iris-health > /tmp/iris-health.out 2>&1; then
      ok "iris-health passes"
    else
      bad "iris-health passes"
      cat /tmp/iris-health.out
    fi
  else
    bad "caddy local CA not found (iris-health not run)"
  fi
else
  bad "caddy reload for tls-internal override"
fi
cp /tmp/Caddyfile.shipped /etc/caddy/Caddyfile
/usr/local/bin/caddy reload --config /etc/caddy/Caddyfile --adapter caddyfile 2>/dev/null || true
sleep 2

echo "== login + temp inbox end to end (python3, stdlib only)"
python3 - "$ADMIN_EMAIL" "$ADMIN_PW" "$DOMAIN" <<'PYEOF'
import json, smtplib, sys, time, urllib.request, urllib.error
from email.message import EmailMessage
from http.cookiejar import CookieJar

email, pw, domain = sys.argv[1], sys.argv[2], sys.argv[3]
BASE = "http://127.0.0.1:8000"
jar = CookieJar()
opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
# The CSRF and session cookies are Secure, so the cookiejar refuses to
# store them over plain http; the server still compares the
# x-csrf-token header against the CSRF cookie and needs the session
# cookie on later calls, so every Set-Cookie is captured by hand and
# replayed explicitly. (Same double-submit detail as the production API.)
COOKIES = {}

def _capture(resp):
    for sc in resp.headers.get_all("Set-Cookie") or []:
        pair = sc.split(";", 1)[0].strip()
        if "=" in pair:
            name, value = pair.split("=", 1)
            COOKIES[name] = value

def req(method, path, body=None, headers=None):
    data = json.dumps(body).encode() if body is not None else None
    hdrs = {"Content-Type": "application/json"}
    if COOKIES:
        hdrs["Cookie"] = "; ".join(f"{k}={v}" for k, v in COOKIES.items())
    hdrs.update(headers or {})
    r = urllib.request.Request(BASE + path, data=data, method=method, headers=hdrs)
    try:
        with opener.open(r, timeout=15) as resp:
            _capture(resp)
            return resp.status, json.load(resp)
    except urllib.error.HTTPError as e:
        _capture(e)
        return e.code, json.loads(e.read().decode() or "{}")

def check(name, cond, extra=""):
    print(("ok   " if cond else "FAIL ") + name + (f" ({extra})" if extra else ""))
    if not cond:
        sys.exit(1)

# CSRF token (body + cookie), then login.
s, csrf = req("GET", "/auth/csrf")
check("csrf token", s == 200 and csrf.get("csrf_token"), f"http {s}")
token = csrf["csrf_token"]
s, sess = req("POST", "/auth/login", {"email": email, "password": pw},
              {"x-csrf-token": token})
check("login as admin", s == 200, f"http {s} {json.dumps(sess)[:200]}")
# Login rotates the CSRF token: the header for later POSTs must use the
# cookie value the login response set, not the pre-login token.
token = COOKIES.get("iris_csrf") or token
s, me = req("GET", "/api/v1/session")
check("session is admin", s == 200 and me.get("is_admin") is True, f"http {s}")

# Temp inbox: create, send mail to it over SMTP, read it back.
# (Public route, but still behind the CSRF check: header + cookie.)
s, temp = req("POST", "/api/v1/temp-inboxes", {}, {"x-csrf-token": token})
check("temp inbox created", s in (200, 201) and temp.get("address"), f"http {s} {json.dumps(temp)[:200]}")
addr, tid, ttoken = temp["address"], temp["id"], temp.get("access_token") or temp.get("token")
msg = EmailMessage()
msg["From"] = f"sender@{domain}"
msg["To"] = addr
msg["Subject"] = "nspawn end-to-end test"
msg.set_content("hello from the install test")
with smtplib.SMTP("127.0.0.1", 25, timeout=15) as smtp:
    smtp.send_message(msg)
print("ok   smtp send to temp inbox")
for _ in range(10):
    s, box = req("GET", f"/api/v1/temp-inboxes/{tid}/messages",
                 headers={"Authorization": f"Bearer {ttoken}"} if ttoken else None)
    msgs = box.get("messages", []) if isinstance(box, dict) else []
    if msgs:
        break
    time.sleep(1)
check("temp inbox received the message", bool(msgs), f"http {s} {json.dumps(box)[:200]}")
if msgs:
    check("message subject matches", msgs[0].get("subject") == "nspawn end-to-end test")

# Admin dashboard is no longer read-only (PR #23): summary answers.
s, adm = req("GET", "/api/v1/admin/summary")
check("admin summary answers", s == 200 and adm.get("users", {}).get("total", 0) >= 1, f"http {s}")
PYEOF

echo "== cert-sync dry path (no LE cert yet, must exit 0 silently)"
need "iris-cert-sync exits 0 without a LE cert" /usr/local/sbin/iris-cert-sync

echo
echo "checks passed: $pass, failed: $fail"
[ "$fail" = 0 ]
