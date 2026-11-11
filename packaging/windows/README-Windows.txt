IRIS Mail Stack - Windows (x86_64)
=================================

One zip installs the whole self-hosted mail stack on Windows 10/11 or
Windows Server: the IRIS webmail API and web app, the Stalwart mail
server (SMTP, IMAP, JMAP), and a Caddy reverse proxy with automatic
TLS. The installer asks for one domain, generates all secrets, and
provisions the mail server.

Install (run PowerShell as Administrator, from the extracted folder):

    .\scripts\install.ps1

Or non-interactively:

    .\scripts\install.ps1 -Domain example.com -NonInteractive

The installer copies everything to C:\Program Files\IRIS, registers
three Windows services (IRISStalwart, IRISApi, IRISCaddy, started via
NSSM), opens ports 80, 443, 25, 587 and 993 in Windows Firewall, and
prints the admin password once (also saved to
C:\Program Files\IRIS\etc\iris\.admin_credentials, Administrators +
SYSTEM only). It writes the DNS records to publish into
C:\Program Files\IRIS\etc\iris\dns-zone.txt: the MX, SPF, DKIM and
DMARC records for your domain. Publish those in your DNS panel.

To pick the admin password instead of a generated one:

    .\scripts\install.ps1 -Domain example.com -AdminPassword 'your-password'

To install somewhere else:

    .\scripts\install.ps1 -Domain example.com -InstallRoot 'D:\IRIS'

Upgrade: extract the new zip and run install.ps1 again. It replaces
the binaries and the web app and restarts the three services.
C:\Program Files\IRIS\.env, the mail data and the config are kept.

Uninstall (keeps mail and config until the last step):

    Stop-Service IRISStalwart, IRISApi, IRISCaddy
    C:\Program Files\IRIS\bin\nssm.exe remove IRISStalwart confirm
    C:\Program Files\IRIS\bin\nssm.exe remove IRISApi confirm
    C:\Program Files\IRIS\bin\nssm.exe remove IRISCaddy confirm
    Remove-Item 'C:\Program Files\IRIS' -Recurse

What you need that the installer cannot do for you:

- A domain you own, with DNS you can edit. A records for
  mail.<domain> and mail-content.<domain> must point at this machine
  for Caddy to issue TLS certificates.
- A PTR (reverse DNS) record for the machine IP pointing at
  mail.<domain>. Without it, other mail servers may reject your mail.
- Port 25 reachable inbound and outbound. Most home ISPs block it;
  on a home connection this install works as webmail + local mail,
  not as a public mail server. If port 25 stays blocked, set
  RESEND_API_KEY in C:\Program Files\IRIS\.env and restart the
  IRISApi service to send through Resend.
- A static public IP address. A home connection with a dynamic IP
  cannot receive mail reliably.

Honest limits (same as the Linux package, plus Windows ones):

- x86_64 only. There is no ARM64 build: Stalwart publishes no
  Windows ARM binary.
- No cert-sync yet: the self-signed certificate the installer
  generates for the mail listeners is not replaced automatically
  when Caddy issues a real one. Replace the files in
  etc\stalwart\tls and restart the IRISStalwart service.
- No cleanup timer yet: the Linux systemd timers (iris-cleanup,
  iris-cert-sync) have no Windows equivalent in this release.
- No bulk sending, no calendar, no drive. IRIS is webmail, not a suite.
- The installer creates one admin account. Add more accounts in the
  Stalwart admin UI.

Verify after install (same checks CI runs):

    .\scripts\check-windows.ps1

Webmail: https://mail.<domain>, log in as the admin email.
Temp inbox: https://mail.<domain>/#/temp (one hour, extendable to
24 hours). Health: the IRISApi service answers http://127.0.0.1:8000/health.
