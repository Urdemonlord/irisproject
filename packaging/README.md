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

There is also a macOS tarball (Apple Silicon and Intel, no `.deb`):
see [macOS](#macos).

There is also a Windows zip (x86_64 only, no `.msi` yet):
see [Windows](#windows).

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
curl -fsSL https://github.com/Urdemonlord/irisproject/releases/download/v0.4.0/install.sh | sudo sh
```

Or download the `.deb` from the release and run it:

```sh
sudo dpkg -i iris-mail-stack_0.4.0_amd64.deb
```

The installer asks for your domain (or set `IRIS_DOMAIN` first), then
prints the admin password once and saves it to
`/etc/iris/.admin_credentials` (mode 600). It also writes the DNS
records to publish into `/etc/iris/dns-zone.txt`: the MX, SPF, DKIM and
DMARC records for your domain. Publish those in your DNS panel.

Non-interactive install:

```sh
sudo IRIS_DOMAIN=example.com IRIS_ADMIN_EMAIL=admin@example.com dpkg -i iris-mail-stack_0.4.0_amd64.deb
```

To pick the admin password instead of a generated one, set
`IRIS_ADMIN_PASSWORD` as well. Never ship a default password: the
package generates one when you do not set it.

A portable tarball of the same payload is attached to each release.
On a system without dpkg, unpack it as root and run `./install.sh`:

```sh
tar xzf iris-mail-stack-0.4.0.tar.gz
cd iris-mail-stack-0.4.0
sudo ./install.sh example.com
```

## macOS

The same stack as a tarball for macOS 14 and later, Apple Silicon
(`arm64`) and Intel (`amd64`). There is no `.deb` and no Homebrew
formula yet; the tarball is the install.

```sh
tar xzf iris-mail-stack-0.4.0-macos-arm64.tar.gz
cd iris-mail-stack-0.4.0-macos-arm64
sudo ./install.sh example.com
```

Non-interactive install works the same way as on Linux:

```sh
sudo IRIS_DOMAIN=example.com IRIS_ADMIN_EMAIL=admin@example.com ./install.sh
```

The installer asks for the domain, generates all secrets, provisions
Stalwart, and prints the admin password once (also saved to
`/etc/iris/.admin_credentials`, mode 600). It writes the DNS records
to publish into `/etc/iris/dns-zone.txt`.

Differences from the Linux package, all forced by macOS:

| Piece | Linux | macOS |
| --- | --- | --- |
| Services | systemd units | launchd plists in `/Library/LaunchDaemons` (`im.meowlabs.iris-*`) |
| Installer scripts | `/usr/lib/iris` | `/usr/local/lib/iris` (`/usr/lib` is SIP-protected) |
| Stalwart and Caddy user | dedicated `stalwart`/`caddy` users + `CAP_NET_BIND_SERVICE` | run as root: macOS has no Linux-style capability for binding ports 25/80/443 |
| IRIS API user | `iris` | `iris`, same |
| Service control | `systemctl status iris-api` | `launchctl print system/im.meowlabs.iris-api` |
| Cleanup / cert-sync | systemd timers | launchd `StartInterval` (15 min) / `StartCalendarInterval` (daily 03:17) |
| Env file loading | systemd `EnvironmentFile` | the api and cleanup jobs run through `/usr/local/lib/iris/iris-api-launch.sh`, which exports `/etc/iris/iris.env` |

Everything else is the same: paths (`/etc/iris`, `/etc/stalwart`,
`/etc/caddy`, `/var/lib/*`), the Caddyfile, the
provisioning script (`iris-setup.py`, shared verbatim), the cert sync
(`iris-cert-sync`, shared verbatim), and the health check
(`iris-health`, one line adapted: macOS LibreSSL `s_client` has no
`-brief` flag). The web dist is the one path that moves:
`/usr/local/share/iris/dist` instead of `/srv/iris/dist`, because the
macOS root volume is read-only and `/srv` cannot be created.

Upgrade: unpack the new tarball and run `./install.sh` again. It
replaces the binaries and the web app, reloads the launchd jobs, and
restarts the three services. `/etc/iris`, `/var/lib/iris` and
`/var/lib/stalwart` are kept.

Uninstall (keeps mail and config until the last step):

```sh
for job in im.meowlabs.iris-stalwart im.meowlabs.iris-api im.meowlabs.iris-caddy im.meowlabs.iris-cleanup im.meowlabs.iris-cert-sync; do
  sudo launchctl bootout "system/$job" 2>/dev/null || true
done
sudo rm /Library/LaunchDaemons/im.meowlabs.iris-*.plist
sudo rm -rf /usr/local/bin/{iris-api,stalwart,caddy} /usr/local/sbin/{iris-health,iris-cert-sync} /usr/local/lib/iris /usr/local/share/iris /etc/iris /etc/stalwart /etc/caddy
# Delete mail and data too (irreversible):
sudo rm -rf /var/lib/iris /var/lib/stalwart /var/log/stalwart /var/lib/caddy /var/log/caddy
sudo dscl . -delete /Users/iris; sudo dscl . -delete /Groups/iris
sudo dscl . -delete /Users/stalwart; sudo dscl . -delete /Groups/stalwart
sudo dscl . -delete /Users/caddy; sudo dscl . -delete /Groups/caddy
```

## Windows

The same stack as a zip for Windows 10/11 and Windows Server, x86_64
only (Stalwart publishes no Windows ARM binary). There is no `.deb`,
`.msi` or Winget package yet; the zip is the install.

```powershell
Expand-Archive iris-mail-stack-0.4.0-windows-x86_64.zip
cd iris-mail-stack-0.4.0-windows-x86_64
# Run PowerShell as Administrator:
.\scripts\install.ps1 -Domain example.com
```

Non-interactive install works the same way as on Linux:

```powershell
.\scripts\install.ps1 -Domain example.com -NonInteractive
```

The installer asks for the domain, generates all secrets, provisions
Stalwart, registers the three services with NSSM, opens ports 80, 443,
25, 587 and 993 in Windows Firewall, and prints the admin password
once (also saved to `C:\Program Files\IRIS\etc\iris\.admin_credentials`).
It writes the DNS records to publish into
`C:\Program Files\IRIS\etc\iris\dns-zone.txt`.

Differences from the Linux package, all forced by Windows:

| Piece | Linux | Windows |
| --- | --- | --- |
| Services | systemd units | NSSM-wrapped Windows services (`IRISStalwart`, `IRISApi`, `IRISCaddy`) |
| Installer scripts | `/usr/lib/iris` | `scripts\` in the zip, installed to `C:\Program Files\IRIS\scripts` |
| Paths | `/etc/iris`, `/var/lib/*`, `/srv/iris/dist` | `C:\Program Files\IRIS\etc`, `data`, `dist` |
| TLS for mail listeners | self-signed, then `iris-cert-sync` replaces it | self-signed only; no cert-sync yet, replace the files in `etc\stalwart	ls` by hand |
| Cleanup / cert-sync | systemd timers | none yet |
| Service control | `systemctl status iris-api` | `Get-Service IRISApi` |

Everything else is the same: the Caddyfile (one path moved:
`C:\Program Files\IRIS\dist` instead of `/srv/iris/dist`), the
provisioning script (`iris-setup.ps1`, a PowerShell port of
`iris-setup.py`), and the smoke test (`scripts\check-windows.ps1`).

Upgrade: extract the new zip and run `install.ps1` again. It replaces
the binaries and the web app and restarts the three services.
`C:\Program Files\IRIS\.env`, the mail data and the config are kept.

Uninstall (keeps mail and config until the last step):

```powershell
Stop-Service IRISStalwart, IRISApi, IRISCaddy
C:\Program Files\IRISin
ssm.exe remove IRISStalwart confirm
C:\Program Files\IRISin
ssm.exe remove IRISApi confirm
C:\Program Files\IRISin
ssm.exe remove IRISCaddy confirm
Remove-Item 'C:\Program Files\IRIS' -Recurse
```

Limits: a home connection usually cannot receive mail (port 25
blocked, dynamic IP, no reverse DNS); on such a machine the install
works as webmail + local mail, not as a public mail server. See
`README-Windows.txt` in the zip for the full list.

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
sudo dpkg -i iris-mail-stack_0.4.0_amd64.deb
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
`build/iris-mail-stack_0.4.0_amd64.deb` and
`build/iris-mail-stack-0.4.0.tar.gz`.

macOS tarballs are built on a Mac (or a macOS CI runner), one per arch:

```sh
cd packaging/macos
./build-macos.sh            # both archs
./build-macos.sh --arch arm64
```

It verifies the pinned macOS tarball checksums in
`versions-macos.env`, builds IRIS for `aarch64-apple-darwin` and
`x86_64-apple-darwin`, assembles the trees, runs `bash -n`,
`py_compile` and `plutil -lint`, and writes
`build/macos/iris-mail-stack-0.4.0-macos-arm64.tar.gz` and
`build/macos/iris-mail-stack-0.4.0-macos-amd64.tar.gz`.

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

The macOS install is smoke-tested the same way on a GitHub macOS
runner (`.github/workflows/package-macos.yml`): it builds both
tarballs, installs the arm64 one with the throwaway domain
`iris-test.local`, and runs `packaging/macos/check-macos.sh`, which
covers services, config, login, temp inbox mail flow, and the admin
dashboard. Run it by hand on a Mac after installing a tarball:

```sh
sudo ./packaging/macos/check-macos.sh
```

## Releasing

The package lives in `packaging/` of this repo and its version follows
the IRIS release: one tag covers both, and one release page holds the
source, the `.deb`, the tarball, and `scripts/install-one-liner.sh`
uploaded as `install.sh` (the README one-liner fetches that name).
To release: tag the IRIS version, build from that tag with
`packaging/build.sh`, then upload the three assets to the release.
Bump `packaging/VERSION` together with the tag.

The macOS tarballs ride the same release: build them from the tag with
`packaging/macos/build-macos.sh` (or take them from the `macOS
package` workflow run for the tag) and upload both to the release.

The Windows zip rides the same release too: the `Windows
package` workflow builds it on a tag push (after the smoke test)
and uploads it to the release automatically.
