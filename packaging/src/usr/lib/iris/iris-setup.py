#!/usr/bin/env python3
"""Provision a fresh Stalwart for IRIS through the JMAP management API.

Run by iris-install.sh while Stalwart runs directly (not via systemd)
with a one-time recovery admin. Standard library only.

Environment: IRIS_DOMAIN, IRIS_ADMIN_EMAIL, ADMIN_LOCAL, ADMIN_PW,
TEMP_PW, RECOVERY_PW. Exits 0 on success, 1 with a message on failure.
"""
import base64
import json
import os
import sys
import time
import urllib.error
import urllib.request

JMAP = "http://127.0.0.1:8080/jmap"
USING = ["urn:ietf:params:jmap:core", "urn:stalwart:jmap"]

DOMAIN = os.environ["IRIS_DOMAIN"]
ADMIN_EMAIL = os.environ["IRIS_ADMIN_EMAIL"]
ADMIN_LOCAL = os.environ["ADMIN_LOCAL"]
ADMIN_PW = os.environ["ADMIN_PW"]
TEMP_PW = os.environ["TEMP_PW"]
RECOVERY_PW = os.environ["RECOVERY_PW"]
MAIL_HOST = "mail." + DOMAIN
INBOX_DOMAIN = "inbox." + DOMAIN


def jmap(body, auth):
    data = json.dumps(body).encode()
    req = urllib.request.Request(
        JMAP + "/", data=data, method="POST",
        headers={"Authorization": auth, "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, json.load(resp)
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read().decode() or "{}")


def fail(msg):
    print(f"iris-setup: error: {msg}", file=sys.stderr)
    sys.exit(1)


def main():
    rec = "Basic " + base64.b64encode(f"admin:{RECOVERY_PW}".encode()).decode()

    # 1. Wait for the management API.
    for _ in range(30):
        try:
            req = urllib.request.Request(JMAP + "/session", headers={"Authorization": rec})
            with urllib.request.urlopen(req, timeout=5) as resp:
                if resp.status == 200:
                    break
        except Exception:
            pass
        time.sleep(2)
    else:
        fail("Stalwart management API did not answer on 127.0.0.1:8080")

    def call(method, args):
        status, res = jmap({"using": USING, "methodCalls": [[method, args, "0"]]}, rec)
        if status != 200:
            fail(f"{method} returned HTTP {status}: {json.dumps(res)[:300]}")
        resp = res["methodResponses"][0][1]
        for key in ("notCreated", "notUpdated", "notDestroyed"):
            if resp.get(key):
                fail(f"{method} refused: {json.dumps(resp[key])[:400]}")
        return resp

    # 2. Domains (DKIM keys are generated automatically per domain).
    res = call("x:Domain/set", {"create": {
        "d-main": {"name": DOMAIN},
        "d-inbox": {"name": INBOX_DOMAIN},
    }})
    domain_ids = {DOMAIN: res["created"]["d-main"]["id"],
                  INBOX_DOMAIN: res["created"]["d-inbox"]["id"]}
    print(f"iris-setup: domains {DOMAIN}={domain_ids[DOMAIN]}, "
          f"{INBOX_DOMAIN}={domain_ids[INBOX_DOMAIN]}")

    # 3. Accounts: admin on the main domain, temp on the inbox domain.
    def account(name, domain, pw, desc):
        res = call("x:Account/set", {"create": {"u1": {
            "@type": "User", "name": name, "domainId": domain_ids[domain],
            "description": desc,
            "credentials": {"0": {"@type": "Password", "secret": pw}},
            "roles": {"@type": "User"}, "permissions": {"@type": "Inherit"},
            "encryptionAtRest": {"@type": "Disabled"},
        }}})
        return res["created"]["u1"]["id"]

    admin_id = account(ADMIN_LOCAL, DOMAIN, ADMIN_PW, "IRIS admin account")
    temp_id = account("temp", INBOX_DOMAIN, TEMP_PW, "IRIS temp inbox catch-all account")
    print(f"iris-setup: accounts {ADMIN_EMAIL}={admin_id}, temp@{INBOX_DOMAIN}={temp_id}")

    # 4. Catch-all on the inbox domain so every temp address lands in temp.
    call("x:Domain/set", {"update": {
        domain_ids[INBOX_DOMAIN]: {"catchAllAddress": f"temp@{INBOX_DOMAIN}"}}})
    print(f"iris-setup: catch-all {INBOX_DOMAIN} -> temp@{INBOX_DOMAIN}")

    # 5. Server identity: hostname + default domain.
    call("x:SystemSettings/set", {"create": {"singleton": {
        "defaultHostname": MAIL_HOST,
        "defaultDomainId": domain_ids[DOMAIN],
    }}})
    print(f"iris-setup: default hostname {MAIL_HOST}")

    # 6. TLS: the certificate object reads the files cert-sync maintains,
    # so replacing the files and reloading is enough to rotate the cert.
    res = call("x:Certificate/set", {"create": {"le": {
        "certificate": {"@type": "File", "filePath": "/etc/stalwart/tls/fullchain.pem"},
        "privateKey": {"@type": "File", "filePath": "/etc/stalwart/tls/privkey.pem"},
    }}})
    cert_id = res["created"]["le"]["id"]
    call("x:SystemSettings/set", {"update": {"singleton": {"defaultCertificateId": cert_id}}})
    call("x:Action/set", {"create": {"a1": {"@type": "ReloadTlsCertificates"}}})
    print(f"iris-setup: TLS certificate {cert_id} (files in /etc/stalwart/tls)")

    # 7. DNS zone files for the operator to publish.
    res = call("x:Domain/get", {"properties": ["name", "dnsZoneFile"]})
    zones = []
    for d in res.get("list", []):
        zones.append(f"# DNS records for {d['name']}\n{d.get('dnsZoneFile') or ''}")
    with open("/etc/iris/dns-zone.txt", "w") as f:
        f.write("\n".join(zones))
    os.chmod("/etc/iris/dns-zone.txt", 0o640)
    print("iris-setup: DNS zone file written to /etc/iris/dns-zone.txt")

    # 8. Verify the admin account can log in over JMAP.
    basic = "Basic " + base64.b64encode(f"{ADMIN_EMAIL}:{ADMIN_PW}".encode()).decode()
    try:
        req = urllib.request.Request(JMAP + "/session", headers={"Authorization": basic})
        with urllib.request.urlopen(req, timeout=15) as resp:
            sess = json.load(resp)
        acct = sess["primaryAccounts"]["urn:ietf:params:jmap:mail"]
    except Exception as e:
        fail(f"admin JMAP login failed: {e}")
    print(f"iris-setup: admin JMAP login OK (account {acct})")


if __name__ == "__main__":
    main()
