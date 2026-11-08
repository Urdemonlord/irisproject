# iris-mail-stack: the official IRIS one-package install

One `.deb` installs the whole self-hosted mail stack: the IRIS webmail
API and web app, the Stalwart mail server (SMTP, IMAP, JMAP), and a
Caddy reverse proxy with automatic TLS. The installer asks for one
domain, generates all secrets, and provisions the mail server, so a
fresh Debian 12 or Ubuntu 24.04 server goes from empty to working
webmail in one step.

This is the official package. It builds IRIS from this repo checkout
and keeps everything on: temp inbox, admin dashboard, contacts, DMARC
dashboard, real-time notifications. It is not Vahid's RustMailStack,
which builds IRIS from his fork (his branding, temp inbox off).

## What it installs

| Piece | Location | Notes |
| --- | --- | --- |
| IRIS API | `/usr/local/bin/iris-api` | 14 MB binary, systemd `iris-api`, port 8000 on loopback |
| IRIS web | `/srv/iris/dist` | Served by Caddy |
| Stalwart | `/usr/local/bin/stalwart` | systemd `iris-stalwart`, JMAP on 127.0.0.1:8080, SMTP 25, submissions 465, IMAPS 993 |
| Caddy | `/usr/local/bin/caddy` | systemd `iris-caddy`, automatic TLS, vhosts `mail.<domain>` and `mail-content.<domain>` |
| Config | `/etc/iris/iris.env`, `/etc/stalwart/config.json`, `/etc/caddy/Caddyfile` | Generated once, never overwritten on upgrade |
| Data | `/var/lib/iris/iris.db`, `/var/lib/stalwart/data` | Kept on upgrade and remove, deleted on purge |
| Helpers | `/usr/local/sbin/iris-health`, `/usr/local/sbin/iris-cert-sync` | Health check, daily TLS sync into Stalwart |

Versions are pinned in `versions.env` (Stalwart, Caddy)
with SHA-256 checks on the downloaded tarballs.

## Prerequisites (the package cannot do these for you)

- A domain you own, with DNS you can edit.
- A records pointing at the server: `mail.<domain>` and
  `mail-content.<domain>`. Caddy needs them to issue TLS certificates.
  Until they point here, the webmail is not reachable over HTTPS.
- A PTR (reverse DNS) record for the server IP pointing at
  `mail.<domain>`. Without it, other mail servers may reject your mail.
- Port 25 reachable inbound and outbound. Many VPS providers block it
  by default; ask yours to unblock it. Outbound mail goes out over
  port 25 directly. If it stays blocked, set `RESEND_API_KEY` in
  `/etc/iris/iris.env` and restart `iris-api` to send through Resend.
- Debian 12 or Ubuntu 24.04, amd64, systemd, ports 80 and 443 free.

## Install

```sh
curl -fsSL https://github.com/Urdemonlord/irisproject/releases/download/v0.3.0/install.sh | sudo sh
```

Or download the `.deb` from the release and run it:

```sh
sudo dpkg -i iris-mail-stack_0.3.0_amd64.deb
```

The installer asks for your domain (or set `IRIS_DOMAIN` first), then
prints the admin password once and saves it to
`/etc/iris/.admin_credentials` (mode 600). It also writes the DNS
records to publish into `/etc/iris/dns-zone.txt`: the MX, SPF, DKIM and
DMARC records for your domain. Publish those in your DNS panel.

Non-interactive install:

```sh
sudo IRIS_DOMAIN=example.com IRIS_ADMIN_EMAIL=admin@example.com dpkg -i iris-mail-stack_0.3.0_amd64.deb
```

To pick the admin password instead of a generated one, set
`IRIS_ADMIN_PASSWORD` as well. Never ship a default password: the
package generates one when you do not set it.

A portable tarball of the same payload is attached to each release.
On a system without dpkg, unpack it as root and run `./install.sh`:

```sh
tar xzf iris-mail-stack-0.3.0.tar.gz
cd iris-mail-stack-0.3.0
sudo ./install.sh example.com
```

## After install

- Webmail: `https://mail.<domain>`, log in as the admin email.
- Admin dashboard: sign in, then open Admin in the sidebar. It can
  disable or enable users, revoke sessions and integration keys, and
  delete temp inboxes. Set who is an admin in `/etc/iris/iris.env`
  (`IRIS_ADMIN_EMAILS`, comma-separated), then restart `iris-api`.
- More mailboxes: create them in the Stalwart admin UI, or over JMAP.
  IRIS logs in with any Stalwart account on your domain.
- Temp inbox: anyone can open `https://mail.<domain>/#/temp` and get an
  address on `inbox.<domain>` for one hour, extendable to 24 hours.
- Health: `iris-health` checks every piece and exits non-zero on failure.

## Upgrade

Install the new `.deb`. It replaces the binaries and the web app and
restarts the services. `/etc/iris`, `/var/lib/iris` and
`/var/lib/stalwart` are kept, so accounts, mail and settings survive.

```sh
sudo dpkg -i iris-mail-stack_0.3.0_amd64.deb
```

## Uninstall

Remove keeps your mail and config:

```sh
sudo dpkg -r iris-mail-stack
```

Purge deletes everything, including mail:

```sh
sudo dpkg -P iris-mail-stack
```

## Honest limits

- No bulk sending, no calendar, no drive. IRIS is webmail, not a suite.
- HTML mail renders in a same-origin `srcdoc` frame in this release,
  the same as production IRIS today. The `mail-content.<domain>`
  vhost is installed and ready; the separate origin activates once the
  web build is baked with the domain, which is a follow-up, not a
  packaging change.
- The installer creates one admin account. Add more accounts in the
  Stalwart admin UI.
- TLS for IMAP/SMTP starts as the self-signed certificate the
  installer generates; `iris-cert-sync` replaces it with the Caddy
  certificate once Caddy has issued one, and reloads Stalwart.
- The admin dashboard has no audit trail yet; admin actions are not
  logged anywhere.

## Building from source

```sh
./build.sh
```

It verifies the pinned tarball checksums, builds IRIS from this repo
checkout (building it if needed), assembles the package tree, runs
`bash -n`, `py_compile` and `systemd-analyze verify`, and writes
`build/iris-mail-stack_0.3.0_amd64.deb` and
`build/iris-mail-stack-0.3.0.tar.gz`.

## Real install test

```sh
./tests/test-container.sh
```

Builds a minimal Debian 12 container, installs the `.deb` in it with
the throwaway domain `iris-test.local`, and checks services, login,
temp inbox mail flow, and the admin dashboard. See `tests/` for what
it covers and what it cannot cover (public TLS needs real DNS).
One sandbox limit the test works around, documented in
`tests/relax-namespaces.conf`: the sandbox cannot create a mount
namespace at all, so the test relaxes exactly the mount-namespace
hardening directives (PrivateTmp, ProtectSystem, ProtectHome,
ReadWritePaths) in the container only. The shipped units keep them.
`tests/container-shell.sh` boots the same container for one-off
commands when debugging the install.

## Releasing

The package lives in `packaging/` of this repo and its version follows
the IRIS release: one tag covers both, and one release page holds the
source, the `.deb`, the tarball, and `scripts/install-one-liner.sh`
uploaded as `install.sh` (the README one-liner fetches that name).
To release: tag the IRIS version, build from that tag with
`packaging/build.sh`, then upload the three assets to the release.
Bump `packaging/VERSION` together with the tag.
