#!/usr/bin/env python3
"""Phase 4 security smoke test against a running IRIS API + Stalwart (stdlib only).

Env: IRIS_URL (default http://127.0.0.1:8000), IRIS_USER, IRIS_PASS, IRIS_DB (sqlite path,
optional: enables DB-at-rest checks), SMTP_HOST/SMTP_PORT (optional: oversize check, default 127.0.0.1:25).
Exit 1 on first failed check.
"""
import http.cookiejar, json, os, re, smtplib, sqlite3, sys, urllib.error, urllib.request

URL = os.environ.get("IRIS_URL", "http://127.0.0.1:8000").rstrip("/")
USER, PASS = os.environ["IRIS_USER"], os.environ["IRIS_PASS"]
DB = os.environ.get("IRIS_DB")


def client():
    jar = http.cookiejar.CookieJar()
    return jar, urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))


def req(op, method, path, body=None, headers=None):
    r = urllib.request.Request(URL + path, json.dumps(body).encode() if body is not None else None,
                               {"content-type": "application/json", **(headers or {})}, method=method)
    try:
        with op.open(r, timeout=20) as resp:
            return resp.status, json.loads(resp.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")


def cookie(jar, name):
    return next((c.value for c in jar if c.name == name), None)


def check(name, cond):
    print(("PASS " if cond else "FAIL ") + name)
    if not cond:
        sys.exit(1)


jar, op = client()
login = {"email": USER, "password": PASS}

# 1. CSRF
check("login without CSRF token -> 403", req(op, "POST", "/auth/login", login)[0] == 403)
st, b = req(op, "GET", "/auth/csrf")
tok = cookie(jar, "iris_csrf")
check("GET /auth/csrf sets cookie", st == 200 and tok and b["csrf_token"] == tok)
check("mismatched CSRF header -> 403", req(op, "POST", "/auth/login", login, {"x-csrf-token": "0" * 64})[0] == 403)
check("login with CSRF -> 200", req(op, "POST", "/auth/login", login, {"x-csrf-token": tok})[0] == 200)
s1 = cookie(jar, "iris_session")
check("CSRF token rotated on login", cookie(jar, "iris_csrf") not in (None, tok))
check("session works", req(op, "GET", "/api/v1/session")[0] == 200)
check("logout without CSRF -> 403", req(op, "POST", "/auth/logout")[0] == 403)
check("temp create (anonymous, no CSRF) -> 403", req(op, "POST", "/api/v1/temp-inboxes")[0] == 403)

# 2. Session rotation: logging in again replaces and revokes the old session.
tok = cookie(jar, "iris_csrf")
check("re-login -> 200", req(op, "POST", "/auth/login", login, {"x-csrf-token": tok})[0] == 200)
s2 = cookie(jar, "iris_session")
check("session token rotated", s1 and s2 and s1 != s2)
jar_old, op_old = client()
op_old.addheaders = [("Cookie", f"iris_session={s1}")]
check("old session revoked -> 401", req(op_old, "GET", "/api/v1/session")[0] == 401)

# 3. Credential persistence: no plaintext password, survives API restart (sealed in DB).
if DB:
    db = sqlite3.connect(DB)
    rows = db.execute("SELECT mail_credential_sealed, mail_credential_id FROM auth_sessions WHERE revoked_at IS NULL").fetchall()
    check("live session has sealed app password", rows and all(r[0] and r[1] for r in rows))
    dump = "\n".join(db.iterdump())
    check("user password not in DB", PASS not in dump)
    check("no app password plaintext in DB", not re.search(r"app_[a-z0-9]{30,}", dump))
    check("revoked session credential wiped",
          db.execute("SELECT COUNT(*) FROM auth_sessions WHERE revoked_at IS NOT NULL AND mail_credential_sealed IS NOT NULL").fetchone()[0] == 0)

st, mbs = req(op, "GET", "/api/v1/mailboxes")
check("mailboxes via app password", st == 200 and mbs["mailboxes"])
st, msgs = req(op, "GET", f"/api/v1/mailboxes/{mbs['mailboxes'][0]['id']}/messages?limit=5")
check("inbox list via app password", st == 200)

check("logout with CSRF -> 204", req(op, "POST", "/auth/logout", None, {"x-csrf-token": cookie(jar, "iris_csrf")})[0] == 204)
op2 = urllib.request.build_opener()
op2.addheaders = [("Cookie", f"iris_session={s2}")]
check("logged-out session -> 401", req(op2, "GET", "/api/v1/session")[0] == 401)

# 4. Oversized message (> 25 MiB) rejected at SMTP.
host, port = os.environ.get("SMTP_HOST", "127.0.0.1"), int(os.environ.get("SMTP_PORT", "25"))
if os.environ.get("SKIP_SMTP") != "1":
    with smtplib.SMTP(host, port, local_hostname="smoke.example.com", timeout=60) as smtp:
        smtp.ehlo()
        limit = int(smtp.esmtp_features.get("size", "0"))
        check(f"SMTP advertises SIZE 26214400 (got {limit})", limit == 26214400)
        big = b"Subject: oversize\r\n\r\n" + (b"x" * 998 + b"\r\n") * (26 * 1024 * 1024 // 1000 + 1)
        try:
            smtp.sendmail("smoke@example.com", [USER], big)
            ok = False
        except (smtplib.SMTPSenderRefused, smtplib.SMTPDataError, smtplib.SMTPServerDisconnected) as e:
            print("   rejected:", getattr(e, "smtp_code", ""), getattr(e, "smtp_error", e))
            ok = True
        check("26 MiB message rejected (SIZE declared)", ok)
    with smtplib.SMTP(host, port, local_hostname="smoke.example.com", timeout=120) as smtp:
        smtp.ehlo()
        smtp.mail("smoke@example.com")  # no SIZE= parameter: server must enforce during DATA
        smtp.rcpt(USER)
        try:
            code, msg = smtp.data(big)
        except (smtplib.SMTPDataError, smtplib.SMTPServerDisconnected) as e:
            code, msg = getattr(e, "smtp_code", 0), getattr(e, "smtp_error", e)
        print("   DATA reply:", code, msg)
        check("26 MiB message rejected (SIZE omitted)", code >= 500 or code == 0)
print("ALL PASS")
